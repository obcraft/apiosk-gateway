//! The public API end to end: an organisation key discovers sources, runs one
//! directly, asks, runs a plan, and is refused by scope, expiry, spend limits
//! and idempotency — over real HTTP, real SQL and the real worker, with the
//! payment leg and the model as local fixtures.
use apiosk_gateway_v2::{
    auth::Auth,
    catalog::CatalogCache,
    engine::Engine,
    execution::Executor,
    parser::Parser,
    server::{router, ServerState},
    state::Signer,
    store::Store,
};
use axum::{extract::State, http::HeaderMap, routing::post, Json, Router};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::Semaphore;
use uuid::Uuid;

const KEY: &str = "apk_org_live_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const READ_ONLY: &str = "apk_org_live_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const EXPIRED: &str = "apk_org_live_cccccccccccccccccccccccccccccccccccccccccccccccc";
const ACCOUNT: &str = "00000000-0000-4000-8000-000000000001";
const KEY_CONNECTION: &str = "00000000-0000-4000-8000-000000000003";

struct Fixture {
    pool: PgPool,
    provider: AtomicU64,
    bearers: Mutex<Vec<String>>,
}

async fn model(Json(body): Json<Value>) -> Json<Value> {
    let input: Value = serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    if input.get("answer_operations").is_some() {
        let source = &input["sources"][0];
        return Json(json!({"choices":[{"message":{"content":json!({"observations":[{"text":"The registry lists company {{a}}.","evidence":[{"result_ref":source["result_ref"],"pointer":"/data/companies/0/id"}]}],"limitations":[]}).to_string()}}]}));
    }
    let intent = json!({"subjects":[{"entity_ref":null,"label":"Example","values":{"company.name":"Example"}}],
        "goals":[{"capability":"company.search","subject":0,"constraints":{}}],"answer_operations":["Name the company identifier"],"unresolved_references":[]});
    // Every surface now reads a question through the Ask chat pass.
    let reply = if input["surface"] == "ask-chat" {
        json!({"message":"I will look that up and show the price first.","discover":true,"intent":intent})
    } else {
        intent
    };
    Json(json!({"choices":[{"message":{"content":reply.to_string()}}]}))
}

/// The App's payment leg: settles in the ledger the way `agent-gateway/v1/run` does.
async fn provider(State(f): State<Arc<Fixture>>, headers: HeaderMap, Json(body): Json<Value>) -> Json<Value> {
    assert_eq!(body["deterministic"], true);
    f.provider.fetch_add(1, Ordering::SeqCst);
    f.bearers.lock().unwrap().push(headers["authorization"].to_str().unwrap().to_owned());
    sqlx::query("INSERT INTO public.balance_charges(id,user_id,idempotency_key,status,charged_micro_usd,actual_provider_micro_usd,provider_price_micro_usd,fee_micro_usd,metadata) VALUES($1,$2,$3,'charged',23,20,20,3,$4)")
        .bind(Uuid::new_v4())
        .bind(Uuid::parse_str(ACCOUNT).unwrap())
        .bind(body["idempotency_key"].as_str().unwrap())
        .bind(json!({"connection_id": KEY_CONNECTION}))
        .execute(&f.pool)
        .await
        .unwrap();
    Json(json!({"ok":true,"charged_micro_usd":23,"provider_micro_usd":20,"fee_micro_usd":3,"result":{"companies":[{"id":"123"}]},"result_truncated":false}))
}

struct Api {
    http: reqwest::Client,
    base: String,
}
impl Api {
    async fn call(&self, method: reqwest::Method, path: &str, key: &str, body: Option<Value>, idempotency: Option<&str>) -> (u16, Value) {
        let mut request = self.http.request(method, format!("{}{path}", self.base)).bearer_auth(key);
        if let Some(idempotency) = idempotency {
            request = request.header("Idempotency-Key", idempotency);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }
    async fn get(&self, path: &str, key: &str) -> (u16, Value) {
        self.call(reqwest::Method::GET, path, key, None, None).await
    }
    async fn post(&self, path: &str, key: &str, body: Value, idempotency: Option<&str>) -> (u16, Value) {
        self.call(reqwest::Method::POST, path, key, Some(body), idempotency).await
    }
}

fn cost(value: &Value) -> i64 {
    value["amount_micro_usd"].as_str().unwrap().parse().unwrap()
}

#[tokio::test]
#[ignore = "requires local tests/support-db.mjs; no live database"]
async fn organisation_key_public_api() {
    let pool = PgPoolOptions::new()
        .after_connect(|conn, _| Box::pin(async move {
            sqlx::query("SET search_path TO public, gateway_v2, kyb, providers, catalog, actions, billing, agents, wallets, research, integrations, observability, support, content, operations, identity, extensions").execute(conn).await?;
            Ok(())
        }))
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(10))
        .connect(&std::env::var("TEST_DATABASE_URL").unwrap_or_else(|_| "postgresql://postgres@127.0.0.1:54329/postgres?sslmode=disable".into()))
        .await
        .unwrap();
    let fixture = Arc::new(Fixture { pool: pool.clone(), provider: AtomicU64::new(0), bearers: Mutex::new(vec![]) });
    let mock = Router::new()
        .route("/chat", post(model))
        .route("/functions/v1/agent-gateway/v1/run", post(provider))
        .with_state(fixture.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let http = reqwest::Client::new();
    let store = Store { pool: pool.clone() };
    let engine = Arc::new(Engine {
        store: store.clone(),
        catalog: CatalogCache::new(pool.clone()),
        parser: Parser::new(http.clone(), format!("{base}/chat"), "fixture".into(), "fixture".into()),
        signer: Signer::new(vec![5; 32]).unwrap(),
        executor: Executor { store: store.clone(), http: http.clone(), supabase_url: base.clone(), enabled: true },
        app_url: "http://localhost:5173".into(),
    });
    let shared = engine.clone();
    let state = ServerState {
        engine,
        auth: Arc::new(Auth { pool: pool.clone(), http: http.clone(), supabase_url: base.clone(), publishable_key: "fixture".into() }),
        permits: Arc::new(Semaphore::new(8)),
        executor_compat: None,
    };
    let app = router(state, "http://localhost:5173").unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = Api { http: http.clone(), base: format!("http://{}", listener.local_addr().unwrap()) };
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // ── Authentication and scopes ────────────────────────────────────────
    let (status, body) = api.get("/v2/balance", "apk_org_live_unknown").await;
    assert_eq!((status, body["error"]["code"].as_str()), (401, Some("invalid_api_key")));
    let (status, body) = api.get("/v2/balance", EXPIRED).await;
    assert_eq!((status, body["error"]["code"].as_str()), (401, Some("api_key_expired")));
    let (status, body) = api.post("/v2/sources/fixture/run", READ_ONLY, json!({"operation":"company.search","input":{"company.name":"Example"}}), None).await;
    assert_eq!((status, body["error"]["code"].as_str(), body["error"]["required_scope"].as_str()), (403, Some("insufficient_scope"), Some("sources:execute")));
    let (status, body) = api.post("/v2/discover", KEY, json!({"request_id":Uuid::new_v4(),"question":"Search only Example"}), None).await;
    assert_eq!(status, 403, "App/MCP protocol routes refuse organisation keys: {body}");
    let workspace = api.http.get(format!("{}/v2/balance", api.base)).bearer_auth(KEY).header("x-apiosk-workspace", Uuid::new_v4().to_string()).send().await.unwrap();
    assert_eq!(workspace.status().as_u16(), 400);
    println!("PASS auth: unknown, expired, missing scope, protocol routes and workspace header refused");

    // ── Discovery ─────────────────────────────────────────────────────────
    let (status, body) = api.get("/v2/sources", READ_ONLY).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.to_string().contains("fixture"));
    let (status, source) = api.get("/v2/sources/fixture", READ_ONLY).await;
    assert_eq!(status, 200, "{source}");
    let search = source["operations"].as_array().unwrap().iter().find(|o| o["id"] == "company.search").unwrap();
    assert_eq!(search["input"][0]["field"], "company.name");
    assert_eq!(search["input"][0]["required"], true);
    assert_eq!(cost(&search["pricing"]["price"]), 23, "the same buyer price the App charges");
    let (status, body) = api.get("/v2/sources/nope", READ_ONLY).await;
    assert_eq!((status, body["error"]["code"].as_str()), (404, Some("source_not_found")));
    println!("PASS discovery: listing, operations, input schemas and buyer prices");

    // ── Direct source run: validation before anything is priced ─────────
    let run = |operation: &str, input: Value| json!({"operation":operation,"input":input});
    for (body, status, code) in [
        (run("company.missing", json!({})), 404, "operation_not_found"),
        (run("company.search", json!({"company.vat":"x"})), 422, "invalid_source_input"),
        (run("company.search", json!({})), 422, "invalid_source_input"),
        (run("company.search", json!({"company.name": 5})), 422, "invalid_source_input"),
        (json!({"operation":"company.search","input":{"company.name":"Example"},"unexpected":true}), 400, "invalid_request"),
    ] {
        let (actual, answer) = api.post("/v2/sources/fixture/run", KEY, body, None).await;
        assert_eq!((actual, answer["error"]["code"].as_str()), (status, Some(code)), "{answer}");
    }
    let (status, body) = api.post("/v2/sources/fixture/run", KEY, json!({"operation":"company.search","input":{"company.name":"Example"},"max_spend":0.00001}), None).await;
    assert_eq!((status, body["error"]["code"].as_str(), body["error"]["limit"].as_str()), (402, Some("spend_limit_exceeded"), Some("max_spend")));
    assert_eq!(fixture.provider.load(Ordering::SeqCst), 0, "no refusal reaches a provider");

    // ── Direct source run: pay once, replay without paying again ────────
    let body = json!({"operation":"company.search","input":{"company.name":"Example"}});
    let (status, first) = api.post("/v2/sources/fixture/run", KEY, body.clone(), Some("nomi-request-983472")).await;
    assert_eq!(status, 200, "{first}");
    assert_eq!(first["status"], "completed", "{first}");
    assert_eq!(first["plan"]["steps"][0]["source_id"], "fixture");
    assert_eq!(first["usage"]["sources"][0]["source_id"], "fixture");
    assert_eq!(first["usage"]["sources"][0]["calls"], 1);
    let parts: i64 = first["usage"]["sources"].as_array().unwrap().iter().map(|s| cost(&s["cost"])).sum();
    assert_eq!((parts, cost(&first["usage"]["cost"])), (23, 23), "sum(source usage) = total cost");
    assert!(first["results"][0]["data"].to_string().contains("123"), "{first}");
    assert_eq!(fixture.provider.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.bearers.lock().unwrap()[0], format!("Bearer {KEY}"), "the payment leg authenticates as the key");
    let (status, replay) = api.post("/v2/sources/fixture/run", KEY, body.clone(), Some("nomi-request-983472")).await;
    assert_eq!((status, &replay["id"]), (200, &first["id"]));
    assert_eq!(fixture.provider.load(Ordering::SeqCst), 1, "a retry is the same purchase");
    let (status, conflict) = api.post("/v2/sources/fixture/run", KEY, json!({"operation":"company.search","input":{"company.name":"Other"}}), Some("nomi-request-983472")).await;
    assert_eq!((status, conflict["error"]["code"].as_str()), (409, Some("idempotency_conflict")));
    println!("PASS direct run: validation, max_spend, one charge per Idempotency-Key, usage sums to cost");

    // ── The short call: POST /v1/execute/{source}/{operation} ───────────
    let before = fixture.provider.load(Ordering::SeqCst);
    let (status, quick) = api.post("/v1/execute/fixture/company.search", KEY, json!({"company.name": "Example"}), Some("short-call-1")).await;
    assert_eq!((status, quick["status"].as_str()), (200, Some("completed")), "{quick}");
    assert!(quick["data"].to_string().contains("123"), "one source, its document at the top level: {quick}");
    assert_eq!(cost(&quick["usage"]["cost"]), 23);
    assert_eq!(fixture.provider.load(Ordering::SeqCst), before + 1);
    let (status, replay) = api.post("/v1/execute/fixture/company.search", KEY, json!({"company.name": "Example"}), Some("short-call-1")).await;
    assert_eq!((status, &replay["id"]), (200, &quick["id"]));
    assert_eq!(fixture.provider.load(Ordering::SeqCst), before + 1, "a retry is the same purchase");
    // The source by its name, and the input by its unambiguous last word.
    let by_name = format!("/v2/execute/{}/company.search?max_spend=1.00&wait_seconds=30", source["name"].as_str().unwrap().replace(' ', "%20"));
    let (status, named) = api.post(&by_name, KEY, json!({"name": "Example"}), None).await;
    assert_eq!((status, named["status"].as_str()), (200, Some("completed")), "{named}");
    for (path, body, code, message) in [
        ("/v1/execute/nope/company.search", json!({}), 404, "source_not_found"),
        ("/v1/execute/fixture/nope", json!({}), 404, "operation_not_found"),
        ("/v1/execute/fixture/company.search", json!({}), 422, "invalid_source_input"),
        ("/v1/execute/fixture/company.search", json!({"unknown": "x"}), 422, "invalid_source_input"),
        ("/v1/execute/fixture/company.search?max_spend=0.00001", json!({"company.name": "Example"}), 402, "spend_limit_exceeded"),
    ] {
        let (actual, answer) = api.post(path, KEY, body, None).await;
        assert_eq!((actual, answer["error"]["code"].as_str()), (code, Some(message)), "{answer}");
    }
    let (status, scoped) = api.post("/v1/execute/fixture/company.search", READ_ONLY, json!({"company.name": "Example"}), None).await;
    assert_eq!((status, scoped["error"]["required_scope"].as_str()), (403, Some("sources:execute")));
    println!("PASS short call: path names the source and operation, body is the inputs, same price and idempotency");

    // ── Ask: plan, then run; auto within and above max_spend ─────────────
    let before_ask = fixture.provider.load(Ordering::SeqCst);
    let (status, plan) = api.post("/v2/ask", KEY, json!({"question":"Search only Example"}), None).await;
    assert_eq!(status, 200, "{plan}");
    assert_eq!(plan["status"], "requires_approval", "{plan}");
    assert_eq!(plan["plan"]["steps"][0]["source_id"], "fixture");
    assert_eq!(cost(&plan["plan"]["estimated_total"]), 23);
    let id = plan["id"].as_str().unwrap().to_owned();
    let (status, ran) = api.post(&format!("/v2/asks/{id}/run"), KEY, json!({"max_spend":"1.00"}), None).await;
    assert_eq!((status, ran["status"].as_str()), (200, Some("completed")), "{ran}");
    assert_eq!(cost(&ran["usage"]["cost"]), 23);
    let (status, again) = api.post(&format!("/v2/asks/{id}/run"), KEY, json!({"max_spend":"1.00"}), None).await;
    assert_eq!((status, again["status"].as_str()), (200, Some("completed")));
    assert_eq!(fixture.provider.load(Ordering::SeqCst), before_ask + 1, "running an ask twice is one run");
    let (status, read) = api.get(&format!("/v2/runs/{id}"), READ_ONLY).await;
    assert_eq!(status, 404, "another key cannot read this run: {read}");
    let (status, read) = api.get(&format!("/v2/runs/{id}"), KEY).await;
    assert_eq!((status, read["status"].as_str()), (200, Some("completed")));

    let (status, held) = api.post("/v2/ask", KEY, json!({"question":"Search only Example","execution_mode":"auto","max_spend":0.00001}), None).await;
    assert_eq!((status, held["status"].as_str(), held["approval_required_reason"]["code"].as_str()), (200, Some("requires_approval"), Some("max_spend")), "{held}");
    let (status, _) = api.post("/v2/ask", KEY, json!({"question":"Search only Example","execution_mode":"auto"}), None).await;
    assert_eq!(status, 400, "auto mode needs max_spend");
    let (status, auto) = api.post("/v2/ask", KEY, json!({"question":"Search only Example","execution_mode":"auto","max_spend":1}), None).await;
    assert_eq!((status, auto["status"].as_str()), (200, Some("completed")), "{auto}");
    assert_eq!(fixture.provider.load(Ordering::SeqCst), before_ask + 2);
    println!("PASS ask: plan then run, idempotent run, per-key isolation, auto within and above max_spend");

    // ── Parity: the Ask page, a connected chatbot and the API plan alike ─
    let plan_of = |steps: &Value| steps.as_array().unwrap().iter().map(|s| (s["source_id"].as_str().unwrap().to_owned(), s["operation"].as_str().unwrap().to_owned())).collect::<Vec<_>>();
    let question = "Search only Example";
    let (_, via_api) = api.post("/v2/ask", KEY, json!({"question": question}), None).await;
    let person = apiosk_gateway_v2::auth::Principal { owner: Uuid::new_v4(), connection: None, workspace: None };
    let chatbot = apiosk_gateway_v2::auth::Principal { owner: Uuid::parse_str(ACCOUNT).unwrap(), connection: Some(Uuid::parse_str("00000000-0000-4000-8000-000000000002").unwrap()), workspace: None };
    let request = |q: &str| apiosk_gateway_v2::protocol::DiscoverRequest { request_id: Uuid::new_v4(), question: q.into(), state: None, context_delta: Default::default() };
    let app = shared.chat(&person, request(question)).await.unwrap();
    let mcp = shared.discover(&chatbot, request(question)).await.unwrap();
    let steps = |r: &apiosk_gateway_v2::protocol::Response| r.proposal.as_ref().unwrap().step_details.iter().map(|d| (d["source"]["slug"].as_str().unwrap().to_owned(), d["capability"].as_str().unwrap().to_owned())).collect::<Vec<_>>();
    assert_eq!(steps(&app), plan_of(&via_api["plan"]["steps"]), "the Ask page and the API plan the same question the same way");
    assert_eq!(steps(&mcp), plan_of(&via_api["plan"]["steps"]), "a connected chatbot and the API plan the same question the same way");
    assert_eq!(app.proposal.as_ref().unwrap().max_total_atomic, mcp.proposal.as_ref().unwrap().max_total_atomic, "one price on every surface");
    println!("PASS parity: Ask page, chatbot connector and API produce the same plan and price");

    // ── Result metadata: source, time, identifiers, link, cost ──────────
    let (_, run) = api.post("/v1/execute/fixture/company.search", KEY, json!({"company.name": "Example"}), Some("metadata-1")).await;
    let meta = &run["results"][0]["metadata"];
    assert_eq!(meta["source_id"], "fixture", "{run}");
    assert!(meta["retrieved_at"].is_string(), "{meta}");
    assert!(meta["source_url"].is_string() || meta["source_url"].is_null(), "{meta}");
    assert_eq!(meta["operation"], "company.search");
    assert_eq!(cost(&meta["cost"]), 23);
    assert_eq!(run["metadata"], *meta, "a single-result run surfaces its metadata at the top level");
    println!("PASS metadata: every result names its source, time, operation, identifiers and cost");

    // ── Balance: the organisation's money, the key's limits ─────────────
    let (status, balance) = api.get("/v2/balance", KEY).await;
    assert_eq!(status, 200, "{balance}");
    assert_eq!(cost(&balance["limits"]["per_request"]), 1_000_000);
    let spent = 23 * (fixture.provider.load(Ordering::SeqCst) as i64);
    assert_eq!(cost(&balance["limits"]["month_spent"]), spent, "every charge this key made, from the ledger");
    assert_eq!(cost(&balance["limits"]["month_remaining"]), 2_000_000 - spent);
    let (status, body) = api.get("/v2/balance", READ_ONLY).await;
    assert_eq!((status, body["error"]["required_scope"].as_str()), (403, Some("billing:read")));
    println!("PASS balance: organisation balance, per-request and monthly limits, scope");
}
