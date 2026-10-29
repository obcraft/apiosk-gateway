//! One question, end to end through the real router, with every log line
//! captured: a client trace follows the question from Find to the answer,
//! including the server worker, and no line carries the question, the answer
//! or a credential. With TELEMETRY_PERSIST=1 each segment leaves one cost
//! row that the runtime role may insert but never rewrite. Run by
//! scripts/test-telemetry.mjs on a fresh PGlite.

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
use axum::{extract::State, http::{HeaderMap, StatusCode}, routing::{get, post}, Json, Router};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, Row};
use std::{
    io::Write,
    sync::{atomic::{AtomicU64, Ordering}, Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

const CANARY: &str = "Quokka Canary 4f1c B.V.";
const BEARER: &str = "eyJhbGciOiJIUzI1NiJ9.quokka-canary-4f1c.c2lnbmF0dXJl";

#[derive(Clone, Default)]
struct Lines(Arc<Mutex<Vec<u8>>>);
impl Write for Lines {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Lines {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
    fn telemetry(&self) -> Vec<Value> {
        self.text()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|line| line["target"] == "apiosk_telemetry")
            .map(|line| serde_json::from_str(line["fields"]["telemetry"].as_str().unwrap()).unwrap())
            .collect()
    }
}

async fn model(Json(body): Json<Value>) -> Json<Value> {
    let input: Value = serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    if input.get("answer_operations").is_some() {
        let source = &input["sources"][0];
        let answer = json!({"observations":[{"text":"The registry reports accounts for {{a}}.","evidence":[{"result_ref":source["result_ref"],"pointer":"/data/accounts"}]}],"limitations":[]});
        return Json(json!({"provider":"Fixture Provider","choices":[{"message":{"content":answer.to_string()}}],
            "usage":{"prompt_tokens":900,"completion_tokens":40,"prompt_tokens_details":{"cached_tokens":512},"cost":0.00021}}));
    }
    // The question asks for a written summary: discover (unlike the retired
    // App chat) answers only the operations the intent names, so the answer
    // operation is what takes the question through analysis.
    let intent = json!({"subjects":[{"entity_ref":null,"label":CANARY,"values":{"company.name":CANARY}}],
        "goals":[{"capability":"company.accounts","subject":0,"constraints":{}}],"answer_operations":["Summarize the annual accounts"],"unresolved_references":[]});
    // No `cost` here: the trace must report the model cost as incomplete, not 0.
    Json(json!({"provider":"Fixture Provider","choices":[{"message":{"content":json!({"message":"Ik zoek de jaarrekening op.","discover":true,"intent":intent}).to_string()}}],
        "usage":{"prompt_tokens":1200,"completion_tokens":80}}))
}
async fn provider(State(calls): State<Arc<AtomicU64>>, Json(body): Json<Value>) -> Json<Value> {
    calls.fetch_add(1, Ordering::Relaxed);
    let (result, charged, cost, fee) = if body["inputs"].get("company.name").is_some() {
        (json!({"companies":[{"id":"123"}]}), 23, 20, 3)
    } else {
        (json!({"accounts":"2025"}), 115, 100, 15)
    };
    Json(json!({"ok":true,"charged_micro_usd":charged,"provider_micro_usd":cost,"fee_micro_usd":fee,"result":result,"result_truncated":false,"duration_ms":40,"replayed":false}))
}
async fn user(headers: HeaderMap) -> (StatusCode, Json<Value>) {
    if headers.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {BEARER}")) {
        (StatusCode::OK, Json(json!({"id":"00000000-0000-4000-8000-000000000001"})))
    } else {
        (StatusCode::UNAUTHORIZED, Json(json!({})))
    }
}

#[tokio::test]
#[ignore = "requires an isolated support-db (scripts/test-telemetry.mjs); no live database"]
async fn one_trace_follows_a_question_without_content_or_credentials() {
    std::env::set_var("TELEMETRY_PERSIST", "1");
    let lines = Lines::default();
    let writer = lines.clone();
    let _logging = tracing::subscriber::set_default(
        tracing_subscriber::fmt().json().with_max_level(tracing::Level::INFO).with_writer(move || writer.clone()).finish(),
    );
    let pool = PgPoolOptions::new()
        .after_connect(|conn, _| Box::pin(async move {
            sqlx::query("SET search_path TO public, gateway_v2").execute(conn).await?;
            Ok(())
        }))
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let calls = Arc::new(AtomicU64::new(0));
    let mock = Router::new()
        .route("/chat", post(model))
        .route("/functions/v1/use-selection", post(provider))
        .route("/auth/v1/user", get(user))
        .with_state(calls.clone());
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
        executor: Executor { store, http: http.clone(), supabase_url: base.clone(), enabled: true },
        app_url: "http://localhost:5173".into(),
    });
    let app = router(ServerState {
        engine,
        auth: Arc::new(Auth { pool: pool.clone(), http: http.clone(), supabase_url: base.clone(), publishable_key: "fixture".into() }),
        permits: Arc::new(tokio::sync::Semaphore::new(8)),
        commerce: None,
    }, "http://localhost:5173").unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let trace = Uuid::new_v4().to_string();
    let client = reqwest::Client::new();
    let send = |request: reqwest::RequestBuilder| request.bearer_auth(BEARER).header("x-apiosk-trace", &trace).send();
    // A question over HTTP: /v2/discover shares the App chat's intake (the
    // App's own /v2/chat route is retired and answers 410). Discover writes
    // an answer only when asked for one, so this question asks for a summary.
    let asked = send(client.post(format!("{gateway}/v2/discover")).json(&json!({"request_id":Uuid::new_v4(),"question":format!("Vat de jaarrekening van {CANARY} samen"),"state":null})))
        .await.unwrap();
    assert_eq!(asked.headers()["x-apiosk-trace"], trace.as_str());
    let priced: Value = asked.json().await.unwrap();
    assert_eq!(priced["status"], "ready", "{priced}");
    let task = priced["state"]["state_ref"].as_str().unwrap().to_owned();
    let proposal = &priced["proposal"];

    // EventSource cannot send headers: the stream names its trace in the query.
    let events = priced["context_view"]["events_path"].as_str().unwrap();
    let mut stream = client.get(format!("{gateway}{events}&compact=true&trace={trace}")).send().await.unwrap();
    assert!(stream.chunk().await.unwrap().is_some(), "the stream sends its first snapshot");
    drop(stream);

    let approved = send(client.post(format!("{gateway}/v2/tasks/{task}/authorize"))
        .json(&json!({"quote_ref":proposal["quote_ref"],"max_total_atomic":proposal["max_total_atomic"]}))).await.unwrap();
    assert_eq!(approved.status(), StatusCode::OK);
    let mut answered = Value::Null;
    for _ in 0..100 {
        answered = send(client.get(format!("{gateway}/v2/tasks/{task}"))).await.unwrap().json().await.unwrap();
        if answered["status"] == "succeeded" && answered["context_view"]["worker_active"] != true { break; }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(answered["status"], "succeeded", "{answered}");
    assert_eq!(calls.load(Ordering::Relaxed), 2, "search then accounts, once each");

    let beacon = json!({"v":1,"trace":trace,"task":task,"device":"desktop","viewport_bucket":1024,"net":"wifi",
        "phases":{"find_to_price":900,"approve_to_answer":2100},"counts":{"sse_open":1},"outcome":"answered"});
    let accepted = send(client.post(format!("{gateway}/v2/telemetry")).json(&beacon)).await.unwrap();
    assert_eq!(accepted.status(), StatusCode::NO_CONTENT);
    let mut leaking = beacon.clone();
    leaking["question"] = json!(CANARY);
    let refused = send(client.post(format!("{gateway}/v2/telemetry")).json(&leaking)).await.unwrap();
    assert!(refused.status().is_client_error(), "a beacon with free text is refused");
    let oversized = send(client.post(format!("{gateway}/v2/telemetry")).header("content-type", "application/json").body(" ".repeat(5000))).await.unwrap();
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);

    // The worker reports its execution segment when it finishes.
    for _ in 0..50 {
        if lines.telemetry().iter().any(|line| line["kind"] == "cost" && line["segment"] == "execution") { break; }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let text = lines.text();
    for secret in ["Quokka", "Canary", "4f1c", "Bearer ", "eyJ", "apk_"] {
        assert!(!text.contains(secret), "a log line contains {secret:?}");
    }
    let telemetry = lines.telemetry();
    let ours: Vec<_> = telemetry.iter().filter(|line| line["trace"] == trace.as_str()).collect();
    let foreign: Vec<_> = telemetry.iter().filter(|line| line["trace"] != trace.as_str() && line["kind"] != "request").collect();
    assert!(foreign.is_empty(), "every span, milestone and cost line of the question carries its trace: {foreign:?}");
    for milestone in ["price_ready", "approved", "worker_started", "first_data", "sources_ready", "answer_complete"] {
        assert!(ours.iter().any(|line| line["milestone"] == milestone), "missing milestone {milestone}");
    }
    for phase in ["gw.request", "gw.auth", "gw.claim", "gw.model", "gw.plan", "gw.view", "gw.approve", "gw.worker_queue",
        "gw.worker", "gw.dispatch_reserve", "gw.edge_hop", "gw.analysis", "gw.sse_stream"] {
        assert!(ours.iter().any(|line| line["phase"] == phase), "missing phase {phase}");
    }
    let price = ours.iter().find(|line| line["milestone"] == "price_ready").unwrap();
    assert_eq!(price["task"], task.as_str());
    assert_eq!(price["steps"], 2);
    assert_eq!(price["parser_cache"], "miss");
    assert_eq!(price["query_class"], "simple_lookup", "the identifier search only serves the asked lookup");
    let unknown: Vec<_> = ours.iter().filter(|line| line["kind"] != "request" && line["phase"] != "gw.auth" && line["surface"] != "app").collect();
    assert!(unknown.is_empty(), "the App surface is known after auth: {unknown:?}");
    assert!(ours.iter().any(|line| line["kind"] == "client" && line["phases"]["find_to_price"] == 900));

    // Cost lines add up per trace; unknown stays null, never 0.
    let costs: Vec<_> = ours.iter().filter(|line| line["kind"] == "cost").collect();
    let sum = |key: &str| costs.iter().filter_map(|line| line[key].as_u64()).sum::<u64>();
    assert_eq!(sum("model_calls"), 2);
    assert_eq!(sum("prompt_tokens"), 2100);
    assert_eq!(sum("cached_tokens"), 512);
    assert_eq!(sum("source_calls"), 2);
    assert_eq!(sum("customer_charged_micro_usd"), 138);
    assert_eq!(sum("provider_micro_usd"), 120);
    assert_eq!(sum("fee_micro_usd"), 18);
    let chat_cost = costs.iter().find(|line| line["segment"] == "question").unwrap();
    assert_eq!(chat_cost["cost_nanousd"], Value::Null, "the model response carried no cost");
    assert_eq!(chat_cost["cost_complete"], false);
    let worker = costs.iter().find(|line| line["segment"] == "execution" && line.get("db_calls").is_some()).expect("the worker flushes its own counters");
    assert!(worker["db_calls"].as_u64().unwrap() > 0);
    assert!(worker["db"]["tasks.load"]["calls"].as_u64().unwrap() > 0);
    let edge = ours.iter().find(|line| line["phase"] == "gw.edge_hop").unwrap();
    assert_eq!(edge["provider_ms"], 40);
    assert_eq!(edge["api_slug"], "fixture");
    let requests: Vec<_> = ours.iter().filter(|line| line["kind"] == "request").collect();
    assert!(requests.iter().any(|line| line["route"] == "/v2/tasks/:id/authorize" && line["status"] == 200));
    assert!(requests.iter().all(|line| !line["route"].as_str().unwrap().contains(&task)), "routes are templates");

    // One durable row per segment, written in the background.
    let trace_id: Uuid = trace.parse().unwrap();
    let mut rows = Vec::new();
    for _ in 0..50 {
        rows = sqlx::query("SELECT segment,outcome,fully_answered,surface,query_class,source_count,cache_state,model_calls,model_cost_nanousd,model_cost_complete,cached_prompt_tokens,source_calls,customer_charged_micro_usd,provider_micro_usd,fee_micro_usd,customer_cap_micro_usd,bytes_in,db_calls FROM gateway_v2.request_costs WHERE trace_id=$1 ORDER BY segment DESC")
            .bind(trace_id).fetch_all(&pool).await.unwrap();
        if rows.len() == 2 { break; }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(rows.len(), 2, "a question row and an execution row");
    let cap: i64 = proposal["max_total_atomic"].as_str().unwrap().parse().unwrap();
    let question = &rows[0];
    assert_eq!(question.get::<String, _>("segment"), "question");
    assert_eq!(question.get::<String, _>("outcome"), "price_ready");
    assert_eq!(question.get::<String, _>("surface"), "app");
    assert_eq!(question.get::<String, _>("query_class"), "simple_lookup");
    assert_eq!(question.get::<Option<i16>, _>("source_count"), Some(1));
    assert_eq!(question.get::<Option<i32>, _>("model_calls"), Some(1));
    assert_eq!(question.get::<Option<i64>, _>("model_cost_nanousd"), None, "unpriced, not free");
    assert_eq!(question.get::<Option<bool>, _>("model_cost_complete"), Some(false));
    assert_eq!(question.get::<Option<i64>, _>("customer_cap_micro_usd"), Some(cap));
    assert!(question.get::<Option<i64>, _>("bytes_in").is_some());
    let execution = &rows[1];
    assert_eq!(execution.get::<String, _>("segment"), "execution");
    assert_eq!(execution.get::<String, _>("outcome"), "answered");
    assert!(execution.get::<bool, _>("fully_answered"));
    assert_eq!(execution.get::<Option<i64>, _>("model_cost_nanousd"), Some(210_000));
    assert_eq!(execution.get::<Option<i64>, _>("cached_prompt_tokens"), Some(512));
    assert_eq!(execution.get::<Option<i32>, _>("source_calls"), Some(2));
    assert_eq!(execution.get::<Option<i64>, _>("customer_charged_micro_usd"), Some(138));
    assert_eq!(execution.get::<Option<i64>, _>("provider_micro_usd"), Some(120));
    assert_eq!(execution.get::<Option<i64>, _>("fee_micro_usd"), Some(18));
    assert_eq!(execution.get::<Option<i64>, _>("customer_cap_micro_usd"), Some(cap));
    assert!(execution.get::<Option<i32>, _>("db_calls").unwrap() > 0);
    assert_eq!(execution.get::<Option<i64>, _>("bytes_in"), None, "a worker has no request body");

    // The runtime role may insert once and prune, never rewrite a row.
    let mut runtime = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL ROLE gateway_v2_runtime").execute(&mut *runtime).await.unwrap();
    sqlx::query("INSERT INTO gateway_v2.request_costs(id,trace_id,segment,task_id,surface,query_class,cache_state,outcome) VALUES($1,$2,'follow_up',$3,'app','unclassified','unknown','failed')")
        .bind(Uuid::new_v4()).bind(Uuid::new_v4()).bind(Uuid::new_v4()).execute(&mut *runtime).await.unwrap();
    let rewrite = sqlx::query("UPDATE gateway_v2.request_costs SET outcome='answered' WHERE trace_id=$1").bind(trace_id).execute(&mut *runtime).await;
    assert!(rewrite.is_err(), "UPDATE is not granted");
    runtime.rollback().await.unwrap();
    let mut runtime = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL ROLE gateway_v2_runtime").execute(&mut *runtime).await.unwrap();
    sqlx::query("DELETE FROM gateway_v2.request_costs WHERE expires_at<now()").execute(&mut *runtime).await.unwrap();
    runtime.rollback().await.unwrap();
    for role in ["anon", "authenticated"] {
        let mut client = pool.begin().await.unwrap();
        sqlx::query(&format!("SET LOCAL ROLE {role}")).execute(&mut *client).await.unwrap();
        assert!(sqlx::query("SELECT 1 FROM gateway_v2.request_costs LIMIT 1").execute(&mut *client).await.is_err(), "{role} cannot read cost rows");
        client.rollback().await.unwrap();
    }
}
