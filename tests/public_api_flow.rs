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
use axum::{extract::State, http::HeaderMap, routing::{get, post}, Json, Router};
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
use sqlx::Row;

const KEY: &str = "apk_org_live_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const READ_ONLY: &str = "apk_org_live_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const EXPIRED: &str = "apk_org_live_cccccccccccccccccccccccccccccccccccccccccccccccc";
const ACCOUNT: &str = "00000000-0000-4000-8000-000000000001";
const KEY_CONNECTION: &str = "00000000-0000-4000-8000-000000000003";

struct Fixture {
    pool: PgPool,
    provider: AtomicU64,
    plans: AtomicU64,
    parses: AtomicU64,
    bearers: Mutex<Vec<String>>,
}

async fn model(State(f): State<Arc<Fixture>>, Json(body): Json<Value>) -> Json<Value> {
    f.plans.fetch_add(1, Ordering::SeqCst);
    let input: Value = serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    if input.get("answer_operations").is_some() {
        let source = &input["sources"][0];
        let pointer = if source["document"]["data"]["accounts"].is_string() { "/data/accounts" } else { "/data/companies/0/id" };
        return Json(json!({"choices":[{"message":{"content":json!({"observations":[{"text":"The registry reports {{a}}.","evidence":[{"result_ref":source["result_ref"],"pointer":pointer}]}],"limitations":[]}).to_string()}}]}));
    }
    let ambiguous = input["question"] == "Accounts for Ambiguous";
    let missing = input["question"] == "Search with missing company name";
    let name = if ambiguous { "Ambiguous" } else if missing { "company" } else { "Example" };
    let subject = if input.get("bindings").is_some() {
        json!({"subject_ref":"s1","values":if missing {json!({})} else {json!({"company.name":{"ref":"s1.label"}})}})
    } else { json!({"entity_ref":null,"label":name,"values":if missing {json!({})} else {json!({"company.name":name})}}) };
    let intent = json!({"subjects":[subject],
        "goals":[{"capability":if ambiguous {"company.accounts"} else {"company.search"},"subject":0,"constraints":{}}],"answer_operations":["Summarize the source result"],"unresolved_references":[]});
    // Every surface now reads a question through the Ask chat pass.
    let reply = if input["surface"] == "ask-chat" {
        json!({"message":"I will look that up and show the price first.","discover":true,"intent":intent})
    } else {
        intent
    };
    Json(json!({"choices":[{"message":{"content":reply.to_string()}}]}))
}

async fn question_model(State(f): State<Arc<Fixture>>, Json(body): Json<Value>) -> ([(axum::http::HeaderName, &'static str); 1], String) {
    f.parses.fetch_add(1, Ordering::SeqCst);
    let question = body["messages"][1]["content"].as_str().unwrap();
    let ambiguous = question.contains("Ambiguous");
    let name = if ambiguous {"Ambiguous"} else if question.contains("missing company name") {"company"} else {"Example"};
    let parsed = json!({"language":"en","subjects":[{"id":"s1","label":name,"type":"company","identifiers":{}}],
        "capabilities":[{"slug":if ambiguous {"company.accounts"} else {"company.search"},"name":if ambiguous {"Company accounts"} else {"Company search"},"description":if ambiguous {"Retrieve company accounts"} else {"Search companies by name"},"domain":"company","inputs":["company_name"],"outputs":["company_id"],"subject_id":"s1","source_hint":if question.contains("Fixture") {json!("fixture")} else {Value::Null},"confidence":1}],
        "deliverable":{"format":"chat","operations":[]}});
    let event = json!({"choices":[{"delta":{"content":parsed.to_string()}}]});
    ([(axum::http::header::CONTENT_TYPE,"text/event-stream")], format!("data: {event}\n\ndata: [DONE]\n\n"))
}

/// The App's payment leg: settles in the ledger the way `agent-gateway/v1/run` does.
async fn provider(State(f): State<Arc<Fixture>>, headers: HeaderMap, Json(body): Json<Value>) -> Json<Value> {
    assert_eq!(body["deterministic"], true);
    f.provider.fetch_add(1, Ordering::SeqCst);
    f.bearers.lock().unwrap().push(headers["authorization"].to_str().unwrap().to_owned());
    let selected = sqlx::query("SELECT kind,connection_id FROM public.user_selected_apis WHERE id=$1")
        .bind(body["selection_id"].as_str().unwrap().parse::<Uuid>().unwrap()).fetch_one(&f.pool).await.unwrap();
    let external = selected.get::<String,_>("kind") == "external";
    assert_eq!(selected.get::<Option<Uuid>,_>("connection_id"), Some(KEY_CONNECTION.parse().unwrap()));
    let charge_id = Uuid::new_v4();
    let accounts = body["inputs"]["company.id"].is_string();
    let (charge, provider, fee) = if accounts || external { (115i64, 100i64, 15i64) } else { (23, 20, 3) };
    sqlx::query("INSERT INTO public.balance_charges(id,user_id,idempotency_key,status,charged_micro_usd,actual_provider_micro_usd,provider_price_micro_usd,fee_micro_usd,metadata) VALUES($1,$2,$3,'charged',$4,$5,$6,$7,$8)")
        .bind(charge_id)
        .bind(Uuid::parse_str(ACCOUNT).unwrap())
        .bind(body["idempotency_key"].as_str().unwrap())
        .bind(charge).bind(provider).bind(provider).bind(fee).bind(json!({"connection_id": KEY_CONNECTION}))
        .execute(&f.pool)
        .await
        .unwrap();
    let result = if external {
        assert_eq!(body["inputs"]["query.company_name"], "Example");
        json!({"companies":[{"id":"external-123","name":"Example"}]})
    } else if accounts {
        assert_eq!(body["inputs"]["company.id"], "456", "the selected identifier reaches the dependent purchase");
        json!({"accounts":"2025", "company_id":"456"})
    } else if body["inputs"]["company.name"] == "Ambiguous" {
        json!({"companies":[{"id":"123"},{"id":"456"}]})
    } else { json!({"companies":[{"id":"123"}]}) };
    Json(json!({"ok":true,"status":"charged","execution_id":charge_id,"charged_micro_usd":charge,"provider_micro_usd":provider,"fee_micro_usd":fee,"result":result,"result_truncated":false}))
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

async fn start_api(pool: &PgPool, base: &str, shared_ask: bool) -> (Api, Arc<Engine>) {
    let http = reqwest::Client::new();
    let store = Store { pool: pool.clone() };
    let mut parser = Parser::new(http.clone(), format!("{base}/chat"), "fixture".into(), "fixture".into())
        .with_local_bazaar(format!("{base}/bazaar")).unwrap();
    if shared_ask {
        parser = parser.with_ask_v2(apiosk_gateway_v2::ask_v2::Client::new(http.clone(), "fixture".into(), "fixture".into())
            .with_local_endpoint(format!("{base}/parse")).unwrap());
    }
    let engine = Arc::new(Engine {
        store: store.clone(),
        catalog: CatalogCache::new(pool.clone()),
        parser,
        signer: Signer::new(vec![5; 32]).unwrap(),
        executor: Executor { store: store.clone(), http: http.clone(), supabase_url: base.to_owned(), enabled: true },
        app_url: "http://localhost:5173".into(),
    });
    let shared = engine.clone();
    let state = ServerState {
        engine,
        auth: Arc::new(Auth { pool: pool.clone(), http: http.clone(), supabase_url: base.to_owned(), publishable_key: "fixture".into() }),
        permits: Arc::new(Semaphore::new(8)),
        executor_compat: Some(apiosk_gateway_v2::compat::ExecutorCompat::new(http.clone(), base.to_owned()).unwrap()),
    };
    let app = router(state, "http://localhost:5173").unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = Api { http: http.clone(), base: format!("http://{}", listener.local_addr().unwrap()) };
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    (api, shared)
}

fn cost(value: &Value) -> i64 {
    value["amount_micro_usd"].as_str().unwrap().parse().unwrap()
}

#[tokio::test]
#[ignore = "requires local tests/support-db.mjs; no live database"]
async fn organisation_key_public_api() {
    let _ = tracing_subscriber::fmt().with_env_filter("warn").try_init();
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
    let fixture = Arc::new(Fixture { pool: pool.clone(), provider: AtomicU64::new(0), plans: AtomicU64::new(0), parses: AtomicU64::new(0), bearers: Mutex::new(vec![]) });
    // Published source fixtures need no execution contracts to be discoverable.
    for slug in ["kvk-dutch-business-register", "sirenic", "companies-house", "gleif", "vies", "overheid-io",
        "ted", "tenderned", "eu-funding-tenders", "cbs-statline", "eurostat", "pdok", "data-overheid-nl"] {
        sqlx::query("INSERT INTO public.apis(id,slug,name,status,active,is_verified,discoverable) VALUES($1,$2,$2,'active',true,true,true)")
            .bind(Uuid::new_v4()).bind(slug).execute(&pool).await.unwrap();
    }
    let mock = Router::new()
        .route("/chat", post(model))
        .route("/parse", post(question_model))
        .route("/bazaar", get(|| async { Json(json!({"resources":[{"resource":"https://fixture.example/data","serviceName":"Fixture Company registry search","description":"Search companies by name"}]})) }))
        .route("/v1/sources/coinbase-x402-bazaar/apis/fixture.example", get(|| async {
            Json(json!({"source":{"id":"coinbase-x402-bazaar"},"endpoints":[{"resource":"https://fixture.example/data","method":"GET","provider":"Fixture","description":"Search companies by name","settlement":"apiosk","network":"eip155:8453","price_usd":0.0001,"buyer_price_usd":0.000115,
                "input_schema":{"type":"object","properties":{"method":{"type":"string","enum":["GET"]},"queryParams":{"type":"object","properties":{"company_name":{"type":"string"}},"required":["company_name"]}}}}]}))
        }))
        .route("/auth/v1/user", get(|headers: HeaderMap| async move {
            if headers.get("authorization").is_some_and(|h| h == "Bearer fixture-person") {
                (axum::http::StatusCode::OK, Json(json!({"id":ACCOUNT})))
            } else { (axum::http::StatusCode::UNAUTHORIZED, Json(json!({}))) }
        }))
        .route("/functions/v1/agent-gateway/v1/run", post(provider))
        .with_state(fixture.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let (api, shared) = start_api(&pool, &base, false).await;

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

    // Display context uses the authenticated account's existing preference,
    // including for a source-read-only key, without planning or execution.
    let (status, body) = api.get("/v2/display-context", "apk_org_live_unknown").await;
    assert_eq!((status, body["error"]["code"].as_str()), (401, Some("invalid_api_key")));
    let expected_display = shared.store.money_display(ACCOUNT.parse().unwrap()).await;
    for key in [KEY, READ_ONLY] {
        let (status, body) = api.get("/v2/display-context", key).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body, json!({"money_display": expected_display}));
    }
    assert_eq!(fixture.provider.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.plans.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.parses.load(Ordering::SeqCst), 0);

    // ── Discovery ─────────────────────────────────────────────────────────
    let (status, body) = api.get("/v2/sources", READ_ONLY).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.to_string().contains("fixture"));
    for (capability, expected) in [
        ("eu.company.profile", vec!["kvk-dutch-business-register", "sirenic", "companies-house"]),
        ("eu.tenders.search", vec!["ted", "tenderned"]),
        ("eu.statistics.query", vec!["cbs-statline", "eurostat"]),
    ] {
        let (status, sources) = api.get(&format!("/v2/sources?capability={capability}"), READ_ONLY).await;
        assert_eq!(status, 200, "{sources}");
        let actual: Vec<_> = sources["sources"].as_array().unwrap().iter().map(|s| s["slug"].as_str().unwrap()).collect();
        assert_eq!(actual, expected);
        assert_eq!(sources["selected_capability"]["slug"], capability);
        assert!(sources["sources"].as_array().unwrap().iter().all(|s| s["available_in_v2"] == false));
    }
    let (status, resolved) = api.post("/v2/capabilities/resolve", READ_ONLY,
        json!({"capability":"eu.tenders.search","country":"NL","input":{}}), None).await;
    assert_eq!(status, 200, "{resolved}");
    assert_eq!(resolved["capability"], "eu.tenders.search");
    assert_eq!(resolved["ready_source_count"], 0, "published metadata alone must not yield a runnable source");
    let (status, refused) = api.post("/v2/capabilities/resolve", READ_ONLY,
        json!({"capability":"eu.tenders.search","country":"Netherlands"}), None).await;
    assert_eq!((status, refused["error"]["code"].as_str()), (400, Some("invalid_country")));
    let (status, refused) = api.post("/v2/capabilities/resolve", READ_ONLY,
        json!({"capability":"eu.statistics.query","country":"NL","input":{"eurostat.input.geo":"DE"}}), None).await;
    assert_eq!((status, refused["error"]["code"].as_str()), (400, Some("country_mismatch")));
    let (status, no_contract) = api.post("/v2/capabilities/plan", KEY,
        json!({"capability":"eu.tenders.search","country":"NL","input":{}}), None).await;
    assert_eq!((status, no_contract["error"]["code"].as_str()), (422, Some("capability_needs_input")), "{no_contract}");
    let (status, no_scope) = api.post("/v2/capabilities/plan", READ_ONLY,
        json!({"capability":"eu.tenders.search","country":"NL","input":{}}), None).await;
    assert_eq!((status, no_scope["error"]["required_scope"].as_str()), (403, Some("ask:create")));
    assert_eq!(fixture.provider.load(Ordering::SeqCst), 0, "capability discovery cannot purchase data");
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
    let (status, plan) = api.post("/v2/ask/plan", KEY, json!({"question":"Search only Example"}), None).await;
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

    let (status, held) = api.post("/v2/ask/plan", KEY, json!({"question":"Search only Example","execution_mode":"auto","max_spend":0.00001}), None).await;
    assert_eq!((status, held["status"].as_str(), held["approval_required_reason"]["code"].as_str()), (200, Some("requires_approval"), Some("max_spend")), "{held}");
    let (status, _) = api.post("/v2/ask/plan", KEY, json!({"question":"Search only Example","execution_mode":"auto"}), None).await;
    assert_eq!(status, 400, "auto mode needs max_spend");
    let (status, auto) = api.post("/v2/ask/plan", KEY, json!({"question":"Search only Example","execution_mode":"auto","max_spend":1}), None).await;
    assert_eq!((status, auto["status"].as_str()), (200, Some("completed")), "{auto}");
    assert_eq!(fixture.provider.load(Ordering::SeqCst), before_ask + 2);
    println!("PASS ask: plan then run, idempotent run, per-key isolation, auto within and above max_spend");

    // Credential parity within Engine::discover. This does not exercise the
    // current Ask composer, which uses parse -> search -> selection -> prepare.
    let plan_of = |steps: &Value| steps.as_array().unwrap().iter().map(|s| (s["source_id"].as_str().unwrap().to_owned(), s["operation"].as_str().unwrap().to_owned())).collect::<Vec<_>>();
    let question = "Search only Example";
    let (_, via_api) = api.post("/v2/ask/plan", KEY, json!({"question": question}), None).await;
    let person = apiosk_gateway_v2::auth::Principal { owner: Uuid::new_v4(), connection: None, workspace: None };
    let chatbot = apiosk_gateway_v2::auth::Principal { owner: Uuid::parse_str(ACCOUNT).unwrap(), connection: Some(Uuid::parse_str("00000000-0000-4000-8000-000000000002").unwrap()), workspace: None };
    let request = |q: &str| apiosk_gateway_v2::protocol::DiscoverRequest { request_id: Uuid::new_v4(), question: q.into(), state: None, context_delta: Default::default() };
    let app = shared.discover(&person, request(question)).await.unwrap();
    let mcp = shared.discover(&chatbot, request(question)).await.unwrap();
    let steps = |r: &apiosk_gateway_v2::protocol::Response| r.proposal.as_ref().unwrap().step_details.iter().map(|d| (d["source"]["slug"].as_str().unwrap().to_owned(), d["capability"].as_str().unwrap().to_owned())).collect::<Vec<_>>();
    assert_eq!(steps(&app), plan_of(&via_api["plan"]["steps"]), "person-principal discover and the public API share the planner");
    assert_eq!(steps(&mcp), plan_of(&via_api["plan"]["steps"]), "a connected chatbot and the API plan the same question the same way");
    assert_eq!(app.proposal.as_ref().unwrap().max_total_atomic, mcp.proposal.as_ref().unwrap().max_total_atomic, "one price on every surface");
    println!("PASS discover credential parity: person, connector and API share the planner and price; Ask composer parity is not established");

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

    // The usage endpoint must agree with balance on the actual fixture ledger.
    let before_reads = fixture.provider.load(Ordering::SeqCst);
    let (status, usage) = api.get("/v2/usage?limit=10", KEY).await;
    assert_eq!(status, 200, "{usage}");
    assert_eq!(cost(&usage["month_spent"]), spent);
    assert_eq!(usage["key_id"], balance["key"]["id"]);
    assert!(usage["charges"].is_array());
    let (status, usage) = api.get("/v2/usage", READ_ONLY).await;
    assert_eq!((status, usage["error"]["required_scope"].as_str()), (403, Some("billing:read")));
    let (status, workflows) = api.get("/v2/ask/workflows", READ_ONLY).await;
    assert_eq!(status, 200, "{workflows}");
    assert!(workflows["workflows"].is_array());
    // These routes intentionally require a person; public organisation keys
    // cannot stand in for consent to connected apps or recurring orders.
    for path in ["/v2/integrations", "/v2/integrations/actions", "/v2/recurring-orders"] {
        let (status, body) = api.get(path, KEY).await;
        assert_eq!(status, 403, "{path}: {body}");
    }
    assert_eq!(fixture.provider.load(Ordering::SeqCst), before_reads);
    println!("PASS remaining reads: usage agrees with balance, workflow catalog, person-only integration/recurring boundaries; no provider calls");

    // A paid search pauses for selection. A fresh API/Engine instance resumes
    // the saved task under the original approval, without repeating search.
    let before = fixture.provider.load(Ordering::SeqCst);
    let (_, planned) = api.post("/v2/ask/plan", KEY, json!({"question":"Accounts for Ambiguous"}), None).await;
    let id = planned["id"].as_str().unwrap();
    let quote = planned["plan"]["quote_ref"].clone();
    assert_eq!(cost(&planned["plan"]["estimated_total"]), 138, "{planned}");
    let (status, paused) = api.post(&format!("/v2/asks/{id}/run"), KEY, json!({"max_spend":0.000138}), None).await;
    assert_eq!((status, paused["status"].as_str()), (200, Some("needs_selection")), "{paused}");
    assert_eq!(cost(&paused["usage"]["cost"]), 23);
    assert_eq!(fixture.provider.load(Ordering::SeqCst), before + 1);
    let (api, restarted) = start_api(&pool, &base, false).await;
    let (_, paused) = api.get(&format!("/v2/runs/{id}"), KEY).await;
    let continuation = &paused["continuation"];
    assert_eq!(continuation["actions"][0]["kind"], "select_entity");
    let chosen = paused["candidates"].as_array().unwrap().iter()
        .find(|c| c["facts"].as_array().unwrap().iter().any(|f| f["value"] == "456")).unwrap()["entity_ref"].clone();
    let selection = json!({"state":continuation["state"],"action_id":continuation["actions"][0]["action_id"],"input":{"entity_ref":chosen}});
    let path = format!("/v2/asks/{id}/continue");
    for (key, expected) in [(READ_ONLY,403), ("apk_access_local_fixture",404), (EXPIRED,401)] {
        let (status, error) = api.post(&path, key, selection.clone(), Some("select-denied")).await;
        assert_eq!(status, expected, "{error}");
    }
    let (status, _) = api.post(&path, KEY, selection.clone(), None).await;
    assert_eq!(status,400,"continuation requires a stable retry identity");
    let mut bad = selection.clone(); bad["input"]["entity_ref"] = json!(Uuid::new_v4());
    let (status, error) = api.post(&path, KEY, bad, Some("select-unknown")).await;
    assert_eq!((status,error["error"]["code"].as_str()),(422,Some("invalid_continuation_input")));
    let mut bad = selection.clone(); bad["state"]["revision"] = json!(999);
    let (status, error) = api.post(&path, KEY, bad, Some("select-tampered")).await;
    assert_eq!((status,error["error"]["code"].as_str()),(400,Some("invalid_continuation_state")));
    let mut bad = selection.clone(); bad["action_id"] = json!(Uuid::new_v4());
    let (status, error) = api.post(&path, KEY, bad, Some("select-unknown-action")).await;
    assert_eq!((status,error["error"]["code"].as_str()),(409,Some("continuation_changed")));
    let mut bad = selection.clone(); bad["max_spend"] = json!(100);
    assert_eq!(api.post(&path, KEY, bad, Some("select-new-budget")).await.0,400);
    assert_eq!(fixture.provider.load(Ordering::SeqCst), before + 1,"invalid continuations buy nothing");

    let (a,b) = tokio::join!(
        api.post(&path, KEY, selection.clone(), Some("select-company")),
        api.post(&path, KEY, selection.clone(), Some("select-company")));
    assert!([200,202,409].contains(&a.0) && [200,202,409].contains(&b.0),"{a:?} {b:?}");
    assert!(a.0 != 409 || b.0 != 409,"one concurrent request must make progress");
    let (status, completed) = api.post(&path, KEY, selection.clone(), Some("select-company")).await;
    assert_eq!((status,completed["status"].as_str()),(200,Some("completed")),"{completed}");
    assert_eq!(completed["id"],json!(id));
    assert_eq!(completed["plan"]["quote_ref"],quote);
    assert_eq!(cost(&completed["usage"]["cost"]),138);
    assert_eq!(completed["results"].as_array().unwrap().len(),2);
    assert_eq!(fixture.provider.load(Ordering::SeqCst),before+2,"one search and one selected report");
    let authorizations: i64 = sqlx::query_scalar("SELECT count(*) FROM authorizations WHERE task_id=$1")
        .bind(Uuid::parse_str(id).unwrap()).fetch_one(&pool).await.unwrap();
    assert_eq!(authorizations,1,"selection preserves the single approval");
    let (status, stale) = api.post(&path, KEY, selection.clone(), Some("different-selection-request")).await;
    assert_eq!((status,stale["error"]["code"].as_str()),(409,Some("continuation_changed")));
    let mut changed = selection.clone(); changed["input"]["entity_ref"] = paused["candidates"][0]["entity_ref"].clone();
    let (status, conflict) = api.post(&path, KEY, changed, Some("select-company")).await;
    assert_eq!((status,conflict["error"]["code"].as_str()),(409,Some("idempotency_conflict")));
    assert_eq!(fixture.provider.load(Ordering::SeqCst),before+2);
    println!("PASS continuation: persisted ambiguity, fresh engine, scoped access, unchanged quote, one approval, concurrent retry and no repeated purchase");

    // Structured input creates a quote, but never silently approves it.
    let (_, missing) = api.post("/v2/ask/plan", KEY, json!({"question":"Search with missing company name"}), None).await;
    assert_eq!(missing["status"],"needs_input","{missing}");
    assert!(!missing["needs_input"].as_array().unwrap().is_empty());
    let id = missing["id"].as_str().unwrap();
    let path = format!("/v2/asks/{id}/continue");
    let input = json!({"state":missing["continuation"]["state"],"action_id":missing["continuation"]["actions"][0]["action_id"],"input":{"value":"Example"}});
    let mut invalid = input.clone(); invalid["input"]["value"] = json!(42);
    assert_eq!(api.post(&path,KEY,invalid,Some("invalid-company-name")).await.0,422);
    let (status, quoted) = api.post(&path,KEY,input.clone(),Some("supply-company-name")).await;
    assert_eq!((status,quoted["status"].as_str()),(200,Some("requires_approval")),"{quoted}");
    assert_eq!(quoted["id"],json!(id));
    assert_eq!(fixture.provider.load(Ordering::SeqCst),before+2,"input does not approve its new quote");
    let (status,replay) = api.post(&path,KEY,input,Some("supply-company-name")).await;
    assert_eq!((status,&replay["plan"]["quote_ref"]),(200,&quoted["plan"]["quote_ref"]));

    // A caller cannot turn the continuation endpoint into /run or /cancel.
    let principal = apiosk_gateway_v2::auth::Principal {owner:Uuid::parse_str(ACCOUNT).unwrap(),connection:Some(Uuid::parse_str(KEY_CONNECTION).unwrap()),workspace:None};
    let view = restarted.view(&principal,Uuid::parse_str(id).unwrap(),Uuid::new_v4(),None).await.unwrap();
    let run_action = view.next_actions.iter().find(|a|a.kind=="execute_quoted_step").unwrap();
    for action in [run_action.action_id,Uuid::parse_str(id).unwrap()] {
        let forged = json!({"state":view.state,"action_id":action,"input":null});
        let (status,error) = api.post(&path,KEY,forged,Some(&format!("not-continuation-{action}"))).await;
        assert_eq!((status,error["error"]["code"].as_str()),(409,Some("continuation_changed")));
    }
    assert_eq!(fixture.provider.load(Ordering::SeqCst),before+2);
    println!("PASS structured input: saved task, schema validation, new quote needs approval, paid/cancel actions refused");

    // Expired quotes, revoked approval and cancellation retain saved results
    // and cannot turn choosing a candidate into a fresh authorization.
    let (_, plan) = api.post("/v2/ask/plan",KEY,json!({"question":"Accounts for Ambiguous"}),None).await;
    let id = plan["id"].as_str().unwrap();
    let task_id = Uuid::parse_str(id).unwrap();
    let (_, paused) = api.post(&format!("/v2/asks/{id}/run"),KEY,json!({"max_spend":0.000138}),None).await;
    assert_eq!(paused["status"],"needs_selection","{paused}");
    let after_search = fixture.provider.load(Ordering::SeqCst);
    let (revision,mut task,_) = restarted.store.load_scoped(&principal,task_id).await.unwrap();
    let original_expiry = task.proposal.as_ref().unwrap()["expires_at"].clone();
    task.proposal.as_mut().unwrap()["expires_at"] = json!(chrono::Utc::now()-chrono::Duration::minutes(1));
    restarted.store.save(principal.owner,task_id,revision,&task).await.unwrap();
    let (_, expired) = api.get(&format!("/v2/runs/{id}"),KEY).await;
    let select = |run:&Value| json!({"state":run["continuation"]["state"],"action_id":run["continuation"]["actions"][0]["action_id"],"input":{"entity_ref":run["candidates"][1]["entity_ref"]}});
    let path = format!("/v2/asks/{id}/continue");
    let (status,error) = api.post(&path,KEY,select(&expired),Some("expired-quote")).await;
    assert_eq!((status,error["error"]["code"].as_str()),(409,Some("plan_expired")));
    let (revision,mut task,_) = restarted.store.load_scoped(&principal,task_id).await.unwrap();
    task.proposal.as_mut().unwrap()["expires_at"] = original_expiry;
    restarted.store.save(principal.owner,task_id,revision,&task).await.unwrap();
    sqlx::query("UPDATE authorizations SET revoked_at=now() WHERE task_id=$1").bind(task_id).execute(&pool).await.unwrap();
    let (_,paused) = api.get(&format!("/v2/runs/{id}"),KEY).await;
    let selection = select(&paused);
    let (status,held) = api.post(&path,KEY,selection.clone(),Some("revoked-approval")).await;
    assert_eq!((status,held["status"].as_str()),(200,Some("requires_approval")),"{held}");
    assert_eq!(cost(&held["usage"]["cost"]),23);
    assert_eq!(fixture.provider.load(Ordering::SeqCst),after_search);
    let (revision,mut task,_) = restarted.store.load_scoped(&principal,task_id).await.unwrap();
    task.cancelled = true;
    restarted.store.save(principal.owner,task_id,revision,&task).await.unwrap();
    let (status,error) = api.post(&path,KEY,selection,Some("cancelled-selection")).await;
    assert_eq!((status,error["error"]["code"].as_str()),(409,Some("run_cancelled")));
    assert_eq!(fixture.provider.load(Ordering::SeqCst),after_search);
    println!("PASS continuation boundaries: expired quote, revoked approval and cancellation never buy or approve again");

    // Add one accepted KVK operation to the isolated catalog, then prove the
    // structured capability path quotes and runs through the same worker.
    let endpoint_id = Uuid::new_v4();
    sqlx::query("INSERT INTO public.capabilities(slug,status) VALUES('company_registry.search','active') ON CONFLICT DO NOTHING")
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO public.api_endpoints(id,api_id,method,path,price,active) SELECT $1,id,'GET','/api/v2/zoeken',0.000020,true FROM public.apis WHERE slug='kvk-dutch-business-register'")
        .bind(endpoint_id).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO public.capability_execution_contracts(id,endpoint_id,capability_slug,version,status,requires,input_map,output_map,identifies,cardinality,max_fan_out,fixed_params,preconditions,pagination,freshness_seconds,sample_payload,validated_at,effect,price_model,produces,adapter_version) SELECT $1,$2,'company_registry.search',version,'active',requires,input_map,output_map,identifies,cardinality,max_fan_out,fixed_params,preconditions,pagination,freshness_seconds,sample_payload,now(),effect,price_model,produces,adapter_version FROM public.capability_execution_contracts WHERE capability_slug='company.search' LIMIT 1")
        .bind(Uuid::new_v4()).bind(endpoint_id).execute(&pool).await.unwrap();
    let sirenic_endpoint = Uuid::new_v4();
    sqlx::query("INSERT INTO public.capabilities(slug,status) VALUES('company.eu-registry-search','active') ON CONFLICT DO NOTHING")
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO public.api_endpoints(id,api_id,method,path,price,active) SELECT $1,id,'GET','/v1/eu/recherche',0.000020,true FROM public.apis WHERE slug='sirenic'")
        .bind(sirenic_endpoint).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO public.capability_execution_contracts(id,endpoint_id,capability_slug,version,status,requires,input_map,output_map,identifies,cardinality,max_fan_out,fixed_params,preconditions,pagination,freshness_seconds,sample_payload,validated_at,effect,price_model,produces,adapter_version) SELECT $1,$2,'company.eu-registry-search',version,'active',requires,input_map,output_map,identifies,cardinality,max_fan_out,fixed_params,preconditions,pagination,freshness_seconds,sample_payload,now(),effect,price_model,produces,adapter_version FROM public.capability_execution_contracts WHERE capability_slug='company.search' LIMIT 1")
        .bind(Uuid::new_v4()).bind(sirenic_endpoint).execute(&pool).await.unwrap();
    let (pilot, _) = start_api(&pool, &base, true).await;
    let before_capability = fixture.provider.load(Ordering::SeqCst);
    let input = json!({"capability":"eu.company.profile","country":"NL","input":{"company.name":"Example"}});
    let (status, resolved) = pilot.post("/v2/capabilities/resolve", READ_ONLY, input.clone(), None).await;
    assert_eq!(status, 200, "{resolved}");
    assert_eq!(resolved["ready_source_count"], 2, "{resolved}");
    let (status, planned) = pilot.post("/v2/capabilities/plan", KEY, input, Some("european-profile-1")).await;
    assert_eq!((status, planned["status"].as_str()), (200, Some("requires_approval")), "{planned}");
    assert_eq!(planned["plan"]["steps"][0]["source_id"], "kvk-dutch-business-register");
    assert_eq!(planned["plan"]["steps"][1]["source_id"], "sirenic");
    assert_eq!(fixture.provider.load(Ordering::SeqCst), before_capability, "planning is free");
    let id = planned["id"].as_str().unwrap();
    let (status, completed) = pilot.post(&format!("/v2/asks/{id}/run"), KEY, json!({"max_spend":1}), None).await;
    assert_eq!((status, completed["status"].as_str()), (200, Some("completed")), "{completed}");
    assert_eq!(completed["capability"], "eu.company.profile");
    assert_eq!(completed["results"][0]["metadata"]["source_id"], "kvk-dutch-business-register");
    assert_eq!(completed["results"][1]["metadata"]["source_id"], "sirenic");
    let facts = completed["normalized_records"][0]["facts"].as_array().expect("extracted facts");
    assert!(facts.iter().any(|fact| fact["sources"][0]["source_id"] == "kvk-dutch-business-register"
        && fact["sources"][0]["retrieved_at"].is_string() && fact["expires_at"].is_string()), "{completed}");
    assert_eq!(fixture.provider.load(Ordering::SeqCst), before_capability + 2);
    println!("PASS Alexandria capability: two endpoint fits, one priced plan and approval, two provider calls and source metadata");
    // Actual current composer parity, using the real HTTP entry points.
    let before_plans = fixture.plans.load(Ordering::SeqCst);
    let before_paid = fixture.provider.load(Ordering::SeqCst);
    let (status, app_parse) = pilot.post("/v2/ask-v2/parse", "fixture-person", json!({"request_id":Uuid::new_v4(),"question":"Search companies named Example using Fixture"}), None).await;
    assert_eq!(status, 200, "{app_parse}");
    let (status, app_search) = pilot.post("/v2/ask-v2/search", "fixture-person", json!({"parsed_request":app_parse["parsed_request"],"origin":"apiosk"}), None).await;
    assert_eq!(status, 200, "{app_search}");
    let (status, choices) = pilot.post("/v2/ask", KEY, json!({"question":"Search companies named Example using Fixture"}), None).await;
    assert_eq!(status, 200, "{choices}");
    assert_eq!(choices["searches"]["apiosk"]["matches"], app_search["matches"], "public Ask and App search return the same choices and input/output contracts");
    assert!(choices.get("plan").is_none() && choices.get("id").is_none());
    assert_eq!(fixture.provider.load(Ordering::SeqCst), before_paid);
    assert_eq!(fixture.plans.load(Ordering::SeqCst), before_plans, "Ask does not invoke the separate planner");
    let selected = choices["apis"].as_array().unwrap().iter().find(|api| api["source_slug"]=="fixture" && api["capability"]=="company.search").expect("fixture source returned");
    assert!(selected["api_id"].is_string());
    assert!(selected["inputs"].is_array() && selected["outputs"].is_array());
    assert_eq!(selected["input_values"]["company.name"], "Example");
    let selected_body = json!({"api_id":selected["api_id"],"capability":selected["capability"],"input":selected["input_values"],"max_spend":1});
    let (status, completed) = pilot.post("/v2/execute", KEY, selected_body.clone(), Some("selected-parity-1")).await;
    assert_eq!((status, completed["status"].as_str()), (200, Some("completed")), "{completed}");
    assert_eq!(completed["results"][0]["metadata"]["source_id"], "fixture");
    assert_eq!(fixture.provider.load(Ordering::SeqCst), before_paid + 1);
    assert_eq!(fixture.plans.load(Ordering::SeqCst), before_plans, "selection executes without another interpretation");
    let (_, replay) = pilot.post("/v2/execute", KEY, selected_body.clone(), Some("selected-parity-1")).await;
    assert_eq!(replay["id"], completed["id"]);
    assert_eq!(fixture.provider.load(Ordering::SeqCst), before_paid + 1, "retry does not buy twice");
    println!("PASS current App parity: identical Ask choices/input/output contracts; selected ID executes once without reinterpretation");

    let mut changed = selected_body.clone(); changed["input"]["company.name"] = json!("Other");
    assert_eq!(pilot.post("/v2/execute", KEY, changed, Some("selected-parity-1")).await.0, 409);
    assert_eq!(pilot.post("/v2/execute", KEY, selected_body.clone(), None).await.0, 400);
    assert_eq!(pilot.post("/v2/execute", READ_ONLY, selected_body.clone(), Some("no-execute-scope")).await.0, 403);
    let mut held = selected_body.clone(); held["max_spend"] = json!(0);
    assert_eq!(pilot.post("/v2/execute", KEY, held, Some("selected-cap")).await.0, 402);
    let mut invalid = selected_body.clone(); invalid["input"] = json!({"unknown":"x"});
    assert_eq!(pilot.post("/v2/execute", KEY, invalid, Some("selected-invalid")).await.0, 422);
    assert_eq!(fixture.provider.load(Ordering::SeqCst), before_paid + 1);

    // External choices use the same authoritative directory, input contract,
    // selected-source executor and saved ledger receipt as the App.
    let (_, app_external) = pilot.post("/v2/ask-v2/search", "fixture-person", json!({"parsed_request":app_parse["parsed_request"],"origin":"coinbase"}), None).await;
    assert_eq!(choices["searches"]["coinbase"]["matches"], app_external["matches"]);
    let external = choices["apis"].as_array().unwrap().iter().find(|api| api["origin"]=="coinbase" && api["executable"]==true).unwrap_or_else(|| panic!("priced executable external choice: {}", choices["searches"]["coinbase"]));
    assert!(external["api_id"].as_str().unwrap().starts_with("bazaar:"));
    assert_eq!(external["capability"], "external.request");
    assert_eq!(external["buyer_price"]["buyer_atomic"], "115");
    assert_eq!(external["missing_inputs"], json!(["query.company_name"]));
    assert!(external["outputs"].is_null(), "no fabricated output schema");
    let external_body = json!({"api_id":external["api_id"],"capability":external["capability"],"input":{"query.company_name":"Example"},"max_spend":1});
    let mut invalid = external_body.clone(); invalid["input"] = json!({});
    assert_eq!(pilot.post("/v2/execute", KEY, invalid, Some("external-invalid")).await.0, 422);
    let mut held = external_body.clone(); held["max_spend"] = json!(0);
    assert_eq!(pilot.post("/v2/execute", KEY, held, Some("external-cap")).await.0, 402);
    assert_eq!(fixture.provider.load(Ordering::SeqCst), before_paid + 1);
    let (status, running) = pilot.post("/v2/execute", KEY, external_body.clone(), Some("external-parity-1")).await;
    assert!([200,202].contains(&status), "{running}");
    let path = format!("/v2/runs/{}", running["id"].as_str().unwrap());
    let saved = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (status, saved) = pilot.get(&path, KEY).await;
            assert!([200,202].contains(&status), "{saved}");
            if saved["status"]=="succeeded" { break saved; }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }).await.expect("external settled receipt");
    assert_eq!(saved["result"]["companies"][0]["id"], "external-123");
    assert_eq!(saved["billing"]["charged_micro_usd"], 115);
    assert_eq!(pilot.get(&path, READ_ONLY).await.0, 404);
    let (_, replay) = pilot.post("/v2/execute", KEY, external_body.clone(), Some("external-parity-1")).await;
    assert_eq!(replay["id"], saved["id"]);
    assert_eq!(replay["result"], saved["result"]);
    let mut changed = external_body; changed["input"]["query.company_name"] = json!("Other");
    assert_eq!(pilot.post("/v2/execute", KEY, changed, Some("external-parity-1")).await.0, 409);
    assert_eq!(fixture.provider.load(Ordering::SeqCst), before_paid + 2);
    assert_eq!(fixture.plans.load(Ordering::SeqCst), before_plans);
    println!("PASS external parity: identical App/API offer, declared inputs, current buyer cap, one connected dispatch, scoped saved receipt, no paid replay");

}
