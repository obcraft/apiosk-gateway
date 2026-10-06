//! Intake through the real router on a fresh PGlite with a fake model that
//! records every interpretation: one length definition, structured endpoint
//! selection, compiled lookups, request receipts and requotes. Run by
//! scripts/test-intake-flow.mjs (PGTEST_PORT, default 54360), one scenario at
//! a time on the one PGlite connection.

use apiosk_gateway_v2::{
    auth::{Auth, Principal},
    catalog::CatalogCache,
    engine::Engine,
    execution::Executor,
    parser::Parser,
    protocol::DiscoverRequest,
    server::{router, ServerState},
    state::Signer,
    store::Store,
};
use axum::{extract::State, http::{HeaderMap, StatusCode}, routing::{get, post}, Json, Router};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool, Row};
use std::{sync::{Arc, Mutex}, time::Duration};
use uuid::Uuid;

/// The organisation key seeded by tests/support-db.mjs.
const ORGANISATION_KEY: &str = "apk_org_live_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// A request whose planner reply binds no company name: the matched operation
/// takes only the registry id (as KVK's profile takes only a KVK number).
const NAMELESS: &str = "Wat staat er in de jaarrekening van Coolblue?";

/// Every interpretation the model was asked for, as the user JSON it received.
#[derive(Default)]
struct Model {
    bazaar_resources: Mutex<Vec<Value>>,
    ask_v2_requests: Mutex<Vec<Value>>,
    /// Requests to the conversation-reply model (OpenRouter in production).
    replies: Mutex<Vec<Value>>,
    chats: Mutex<Vec<Value>>,
    workflow_chats: Mutex<Vec<Value>>,
    stages: Mutex<Vec<&'static str>>,
    failures: Mutex<std::collections::BTreeMap<&'static str, usize>>,
}

fn ask_v2_capabilities() -> Value {
    json!({"language":"nl","subjects":[{"id":"s1","label":"Coolblue","type":"company","identifiers":{}}],
        "capabilities":[{"slug":"company.financial.statements","name":"Company financial statements","description":"Retrieve company annual accounts.","domain":"company","inputs":[],"outputs":["accounts"],"subject_id":"s1","source_hint":null,"confidence":0.95}],
        "deliverable":{"format":"chat","operations":["list"]}})
}

/// Only the new transport uses this fake SSE provider. Record its request to
/// prove the App bearer never becomes a provider credential or planner input.
async fn ask_v2_model(State(model): State<Arc<Model>>, headers: HeaderMap, Json(body): Json<Value>) -> ([(axum::http::header::HeaderName, &'static str); 1], String) {
    assert_eq!(headers.get("authorization").unwrap(), "Bearer ask-v2-fixture-key");
    model.ask_v2_requests.lock().unwrap().push(body);
    let event = json!({"choices":[{"delta":{"content":ask_v2_capabilities().to_string()}}]});
    ([(axum::http::header::CONTENT_TYPE,"text/event-stream")], format!("data: {event}\n\ndata: [DONE]\n\n"))
}
/// The conversation-reply model's written reply, or a failure when one is due.
const WRITTEN_REPLY: &str = "Goed dat je het vraagt! Welke gegevens over Coolblue wil je controleren, bijvoorbeeld de jaarrekening?";
async fn conversation_reply_model(State(model): State<Arc<Model>>, headers: HeaderMap, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
    assert_eq!(headers.get("authorization").unwrap(), "Bearer reply-fixture-key");
    model.replies.lock().unwrap().push(body);
    if model.failure("reply").is_some() {
        return (StatusCode::BAD_GATEWAY, Json(json!({"error":{"message":"private provider detail"}})));
    }
    (StatusCode::OK, Json(json!({"provider":"Alibaba","choices":[{"finish_reason":"stop","message":{"content":WRITTEN_REPLY}}]})))
}
impl Model {
    fn chats(&self) -> Vec<Value> {
        self.chats.lock().unwrap().clone()
    }
    fn fail_next(&self, stage: &'static str, attempts: usize) {
        self.failures.lock().unwrap().insert(stage, attempts);
    }
    fn failure(&self, stage: &'static str) -> Option<Json<Value>> {
        let mut failures = self.failures.lock().unwrap();
        let remaining = failures.get_mut(stage)?;
        if *remaining == 0 { return None; }
        *remaining -= 1;
        Some(Json(json!({"choices":[{"message":{"content":"deliberately invalid model output"}}]})))
    }
    fn count(&self, stage: &str) -> usize {
        self.stages.lock().unwrap().iter().filter(|&&found| found == stage).count()
    }
}

async fn user(headers: HeaderMap) -> (StatusCode, Json<Value>) {
    let owner = headers.get("authorization").and_then(|h| h.to_str().ok()).and_then(|h| h.strip_prefix("Bearer intake-")).and_then(|id| id.parse::<Uuid>().ok());
    match owner {
        Some(owner) => (StatusCode::OK, Json(json!({"id":owner}))),
        None => (StatusCode::UNAUTHORIZED, Json(json!({}))),
    }
}

/// "Jaarrekening van <Label>" asks for company accounts; anything else is
/// conversation. The label is the last word, copied literally.
async fn interpret(State(model): State<Arc<Model>>, Json(body): Json<Value>) -> Json<Value> {
    let input: Value = serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    if input["known_slugs"].is_array() {
        model.stages.lock().unwrap().push("parse");
        if let Some(failure) = model.failure("parse") { return failure; }
        let question=input["question"].as_str().unwrap();
        let continued = question == "De jaarrekening" && input["previous_context"].to_string().contains("Coolblue");
        let label=question.strip_prefix("Jaarrekening van ").or_else(|| question.strip_prefix("Controleer "))
            .map(|s| s.rsplit(' ').next().unwrap()).or(continued.then_some("Coolblue"));
        let content=json!({"language":"nl","subjects":label.map(|label| vec![json!({"id":"s1","label":label,"type":"company","identifiers":{}})]).unwrap_or_default(),
            "capabilities":[{"slug":if label.is_some(){"company.financial.statements"}else{"general.conversation"},"name":"Company financial statements","description":"Retrieve financial statements for a company.","domain":"company","inputs":[],"outputs":[],"subject_id":null,"source_hint":null,"confidence":0.9}],
            "deliverable":{"format":"chat","operations":[]}});
        return Json(json!({"choices":[{"message":{"content":content.to_string()}}]}));
    }
    if let Some(queries)=input["queries"].as_object() {
        model.stages.lock().unwrap().push("search");
        if let Some(failure) = model.failure("search") { return failure; }
        let content=json!({"matches":queries.keys().map(|slug|json!({"slug":slug,"candidate_ids":["c1"]})).collect::<Vec<_>>()});
        return Json(json!({"choices":[{"message":{"content":content.to_string()}}]}));
    }
    if input["workflow"].is_string() && input["fields"].is_array() {
        model.stages.lock().unwrap().push("workflow");
        model.workflow_chats.lock().unwrap().push(input.clone());
        if let Some(failure) = model.failure("workflow") { return failure; }
        let question = input["question"].as_str().unwrap();
        let mut values = serde_json::Map::new();
        if question.contains("Amsterdam") { values.insert("location".into(), json!("Amsterdam")); }
        if question.contains("software") { values.insert("topic".into(), json!("software")); }
        let content = json!({"message":"Welke bedrijven zoek je precies?","input":values,"ready":false});
        return Json(json!({"choices":[{"message":{"content":content.to_string()}}]}));
    }
    model.stages.lock().unwrap().push("plan");
    model.chats.lock().unwrap().push(input.clone());
    if let Some(failure) = model.failure("plan") { return failure; }
    let question = input["question"].as_str().unwrap();
    if question == NAMELESS {
        let content = json!({"message":"Ik zoek de jaarrekening op en toon eerst de prijs.","discover":true,"intent":{
            "subjects":[{"subject_ref":"s1","values":{}}],
            "goals":[{"capability":"company.accounts","subject":0,"constraints":{}}],"answer_operations":[],"unresolved_references":[]}});
        return Json(json!({"choices":[{"message":{"content":content.to_string()}}]}));
    }
    let continued = question == "De jaarrekening" && input["context"].to_string().contains("Coolblue");
    let content = match question.strip_prefix("Jaarrekening van ").or(continued.then_some("Coolblue")) {
        Some(label) => {
            let label = label.rsplit(' ').next().unwrap();
            json!({"message":"Ik zoek de jaarrekening op en toon eerst de prijs.","discover":true,"intent":{
                "subjects":[if input.get("bindings").is_some() {json!({"subject_ref":"s1","values":{"company.name":{"ref":"s1.label"}}})} else {json!({"entity_ref":null,"label":label,"values":{"company.name":label}})}],
                "goals":[{"capability":"company.accounts","subject":0,"constraints":{}}],"answer_operations":[],"unresolved_references":[]}})
        }
        None => json!({"message":if question.starts_with("Controleer ") {"Welke gegevens wil je controleren?"} else {"Waar kan ik je mee helpen?"},"discover":false,"intent":null,
            "decline_reason":if question.starts_with("Controleer ") {"ambiguous_request"} else {"conversation"}}),
    };
    Json(json!({"choices":[{"message":{"content":content.to_string()}}]}))
}

struct Person {
    owner: Uuid,
    bearer: String,
}
impl Person {
    fn new() -> Self {
        let owner = Uuid::new_v4();
        Self { owner, bearer: format!("intake-{owner}") }
    }
}

struct Intake {
    pool: PgPool,
    model: Arc<Model>,
    gateway: String,
    http: reqwest::Client,
    engine: Arc<Engine>,
    edge: Arc<FakeEdge>,
}

#[derive(Clone)]
enum EdgeOutcome {
    Success { result: Value, record_run: bool, mismatched_receipt: bool },
    Ambiguous,
    Refused,
}
struct FakeEdge {
    pool: PgPool,
    calls: Mutex<Vec<Value>>,
    outcome: Mutex<EdgeOutcome>,
}

/// The existing balance executor is replaced only inside this fixture. This
/// models its ledger/result contract, not a blockchain or production RPC.
async fn fake_edge(State(edge): State<Arc<FakeEdge>>, headers: HeaderMap, Json(request): Json<Value>) -> (StatusCode, Json<Value>) {
    let owner: Uuid = headers.get("authorization").unwrap().to_str().unwrap().strip_prefix("Bearer intake-").unwrap().parse().unwrap();
    assert_eq!(request["deterministic"], true);
    let selection: Uuid = request["selection_id"].as_str().unwrap().parse().unwrap();
    let key = request["idempotency_key"].as_str().unwrap();
    edge.calls.lock().unwrap().push(request.clone());
    let outcome = edge.outcome.lock().unwrap().clone();
    let (result, record_run, mismatched_receipt) = match outcome {
        EdgeOutcome::Ambiguous => return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"ok":false,"error":"internal_error","stage":"executing"}))),
        EdgeOutcome::Refused => return (StatusCode::PAYMENT_REQUIRED, Json(json!({"ok":false,"error":"insufficient_funds","stage":"checking_balance"}))),
        EdgeOutcome::Success { result, record_run, mismatched_receipt } => (result, record_run, mismatched_receipt),
    };
    let selected = sqlx::query("SELECT kind,resource_url,provider_price_micro_usd,charged_micro_usd FROM public.user_selected_apis WHERE id=$1 AND user_id=$2")
        .bind(selection).bind(owner).fetch_one(&edge.pool).await.unwrap();
    assert_eq!(selected.get::<String,_>("kind"), "external");
    assert_eq!(selected.get::<String,_>("resource_url"), "https://fixture.example/data");
    let provider: i64 = selected.get("provider_price_micro_usd");
    let total = provider + (provider * 1500 + 9999) / 10000;
    assert_eq!(selected.get::<i64,_>("charged_micro_usd"), total);
    let charge = Uuid::new_v4();
    let mut tx = edge.pool.begin().await.unwrap();
    assert_eq!(sqlx::query("UPDATE public.user_balances SET balance_micro_usd=balance_micro_usd-$2 WHERE user_id=$1 AND balance_micro_usd>=$2")
        .bind(owner).bind(total).execute(&mut *tx).await.unwrap().rows_affected(), 1);
    sqlx::query("INSERT INTO public.balance_charges(id,user_id,idempotency_key,status,charged_micro_usd,actual_provider_micro_usd,provider_price_micro_usd,fee_micro_usd) VALUES($1,$2,$3,'charged',$4,$5,$5,$6)")
        .bind(charge).bind(owner).bind(key).bind(total).bind(provider).bind(total-provider).execute(&mut *tx).await.unwrap();
    if record_run {
        sqlx::query("INSERT INTO public.call_runs(id,charge_id,user_id,status,output,output_truncated) VALUES($1,$2,$3,'succeeded',$4,false)")
            .bind(Uuid::new_v4()).bind(charge).bind(owner).bind(&result).execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();
    (StatusCode::OK, Json(json!({"ok":true,"status":"charged","execution_id":if mismatched_receipt {Uuid::new_v4()} else {charge},
        "charged_micro_usd":total,"provider_micro_usd":provider,"fee_micro_usd":total-provider,"result":result,"result_truncated":false})))
}

impl Intake {
    async fn start() -> Self {
        let pool = PgPoolOptions::new()
            .after_connect(|conn, _| Box::pin(async move {
                sqlx::query("SET search_path TO public, gateway_v2").execute(conn).await?;
                Ok(())
            }))
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(10))
            .connect(&std::env::var("TEST_DATABASE_URL").unwrap())
            .await
            .unwrap();
        let model = Arc::new(Model::default());
        let edge = Arc::new(FakeEdge { pool:pool.clone(), calls:Mutex::new(vec![]), outcome:Mutex::new(EdgeOutcome::Success {result:json!({"rows":[{"id":"fixture-record"}]}), record_run:true, mismatched_receipt:false}) });
        let mock = Router::new()
            .route("/chat", post(interpret))
            .route("/ask-v2-model", post(ask_v2_model))
            .route("/conversation-reply", post(conversation_reply_model))
            .route("/bazaar", get(|State(model): State<Arc<Model>>| async move {
                model.stages.lock().unwrap().push("bazaar");
                Json(json!({"resources":model.bazaar_resources.lock().unwrap().clone()}))
            }))
            .route("/auth/v1/user", get(user))
            .with_state(model.clone())
            .merge(Router::new().route("/functions/v1/use-selection", post(fake_edge)).with_state(edge.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
        let http = reqwest::Client::new();
        let store = Store { pool: pool.clone() };
        let engine = Arc::new(Engine {
            store: store.clone(),
            catalog: CatalogCache::new(pool.clone()),
            parser: Parser::new(http.clone(), format!("{base}/chat"), "fixture".into(), "fixture".into()).with_local_bazaar(format!("{base}/bazaar")).unwrap()
                .with_ask_v2(apiosk_gateway_v2::ask_v2::Client::new(http.clone(), "ask-v2-fixture-key".into(), "ask-v2-fixture-model".into())
                    .with_local_endpoint(format!("{base}/ask-v2-model")).unwrap())
                .with_conversation_replies(apiosk_gateway_v2::parser::conversation_reply::Client::new(http.clone(), "reply-fixture-key".into())
                    .with_local_endpoint(format!("{base}/conversation-reply")).unwrap()),
            signer: Signer::new(vec![7; 32]).unwrap(),
            executor: Executor { store, http: http.clone(), supabase_url: base.clone(), enabled: true },
            app_url: "http://localhost:5173".into(),
        });
        let app = router(ServerState {
            engine: engine.clone(),
            auth: Arc::new(Auth { pool: pool.clone(), http: http.clone(), supabase_url: base, publishable_key: "fixture".into() }),
            permits: Arc::new(tokio::sync::Semaphore::new(16)),
            executor_compat: None,
        }, "http://localhost:5173").unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gateway = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { pool, model, gateway, http, engine, edge }
    }

    async fn send(&self, bearer: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let url = format!("{}{path}", self.gateway);
        let request = match &body {
            Some(body) => self.http.post(url).json(body),
            None => self.http.get(url),
        }.bearer_auth(bearer);
        let response = request.send().await.unwrap();
        let status = response.status();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    /// The engine's App chat path, which still creates the tasks these flows
    /// test. The App no longer sends it over HTTP: `/v2/chat` answers 410.
    async fn chat(&self, person: &Person, body: Value) -> Value {
        let request: DiscoverRequest = serde_json::from_value(body).unwrap();
        let principal = Principal { owner: person.owner, connection: None, workspace: None };
        serde_json::to_value(self.engine.chat(&principal, request).await.unwrap()).unwrap()
    }
}

/// A declined question's reply comes from the conversation model (MCP and API
/// discover), with the planner's text kept whenever that model fails; the Ask
/// page asks the same model through its own route, which creates nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn a_declined_question_is_answered_by_the_conversation_model_or_keeps_the_planner_reply() {
    let intake = Intake::start().await;
    let discover = |person: &Person, question: &str| {
        let request: DiscoverRequest = serde_json::from_value(json!({"request_id":Uuid::new_v4(),"question":question,"state":null})).unwrap();
        let principal = Principal { owner: person.owner, connection: None, workspace: None };
        let engine = intake.engine.clone();
        async move { serde_json::to_value(engine.discover(&principal, request).await.unwrap()).unwrap() }
    };
    let person = Person::new();
    let written = discover(&person, "Controleer Coolblue").await;
    assert_eq!(written["status"], "needs_input", "{written}");
    assert_eq!(written["context_view"]["conversation"][0]["reply"], WRITTEN_REPLY, "{written}");
    let replies = intake.model.replies.lock().unwrap().clone();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0]["model"], "qwen/qwen3-235b-a22b-2507");
    let input: Value = serde_json::from_str(replies[0]["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(input["question"], "Controleer Coolblue");
    assert_eq!(input["reason"], "ambiguous_request");
    assert_eq!(input["language"], "nl");
    assert_eq!(input["draft"], "Welke gegevens wil je controleren?", "the planner's reply is the draft");
    assert!(intake.model.count("plan") >= 1, "the planner still reads the question");

    // The conversation model fails: the planner's reply, same status and actions.
    intake.model.fail_next("reply", 1);
    let other = Person::new();
    let kept = discover(&other, "Controleer Coolblue").await;
    assert_eq!(kept["context_view"]["conversation"][0]["reply"], "Welke gegevens wil je controleren?", "{kept}");
    for field in ["status", "next_actions", "proposal"] {
        assert_eq!(kept[field], written[field], "{field}");
    }

    // A plan is never rewritten by the conversation model.
    let planned = discover(&Person::new(), "Jaarrekening van Coolblue").await;
    assert_ne!(planned["status"], "needs_input", "{planned}");
    assert_eq!(intake.model.replies.lock().unwrap().len(), 2);

    // The Ask page's route: the same model, no task, the page's own text on failure.
    let tasks = task_count(&intake, &person).await;
    let body = json!({"question":"How are you?","context":[{"role":"user","text":"Hi"},{"role":"assistant","text":"Hello!"}],
        "reason":"conversation","language":"en","draft":"What data do you need, and about which company, person or place?"});
    let (status, reply) = intake.send(&person.bearer, "/v2/ask-v2/reply", Some(body.clone())).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply, json!({"reply":WRITTEN_REPLY,"model":"qwen/qwen3-235b-a22b-2507"}));
    let sent = intake.model.replies.lock().unwrap().last().cloned().unwrap();
    let input: Value = serde_json::from_str(sent["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(input["earlier_turns"], body["context"]);
    assert_eq!(input["reason"], "conversation");
    assert_eq!(task_count(&intake, &person).await, tasks, "a written reply creates no task");
    let (status, _) = intake.send("not-a-session", "/v2/ask-v2/reply", Some(body.clone())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    for invalid in [json!({"question":" ","reason":"conversation"}), json!({"question":"Hi","reason":"conversation","language":"english"})] {
        let (status, _) = intake.send(&person.bearer, "/v2/ask-v2/reply", Some(invalid)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (status, _) = intake.send(&person.bearer, "/v2/ask-v2/reply", Some(json!({"question":"Hi","reason":"none"}))).await;
    assert!(status.is_client_error(), "only the three decline reasons get a reply");
    intake.model.fail_next("reply", 1);
    let (status, failed) = intake.send(&person.bearer, "/v2/ask-v2/reply", Some(body)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(codes(&failed), ["reply_unavailable"]);
    assert!(!failed.to_string().contains("private provider detail"));
}

fn codes(response: &Value) -> Vec<&str> {
    response["errors"].as_array().unwrap().iter().filter_map(|error| error["code"].as_str()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn ask_v2_parse_then_local_search_uses_one_model_call_and_creates_no_task() {
    let intake = Intake::start().await;
    let person = Person::new();
    let question = "Jaarrekening van Coolblue";
    let (status, output) = intake.send(&person.bearer, "/v2/ask-v2/parse", Some(json!({
        "request_id":Uuid::new_v4(),"question":question,"known_slugs":[]
    }))).await;
    assert_eq!(status, StatusCode::OK, "{output}");
    assert_eq!(output["model"], "ask-v2-fixture-model");
    assert!(output["timing"]["parse_ms"].is_u64());
    let raw = output["text"].as_str().unwrap();
    assert_eq!(serde_json::from_str::<Value>(raw).unwrap(), ask_v2_capabilities());
    let parsed = apiosk_gateway_v2::capability_intake::normalize(raw, question, output["model"].as_str().unwrap()).unwrap();
    let (status, found) = intake.send(&person.bearer, "/v2/ask-v2/search", Some(json!({
        "parsed_request":parsed,"origin":"apiosk"
    }))).await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert_eq!(found["apiosk_sources_searched"], 1);
    assert!(found["catalog_version"].is_string());
    assert!(found["timing"]["search_ms"].is_u64());
    assert_eq!(found["matches"][0]["apiosk"][0]["capability"], "company.accounts", "{found}");
    let calls = intake.model.ask_v2_requests.lock().unwrap().clone();
    assert_eq!(calls.len(), 1, "local search must not call a model");
    assert_eq!(calls[0]["stream"], true);
    assert_eq!(calls[0]["model"], "ask-v2-fixture-model");
    assert_eq!(calls[0]["messages"][1]["content"], question);
    // JSON mode on Qwen's own provider since 998f35f; the gateway validates.
    assert_eq!(calls[0]["response_format"]["type"], "json_object");
    assert!(intake.model.stages.lock().unwrap().is_empty(), "the old parser, discovery model, Bazaar and planner must remain untouched");
    for response in [&output, &found] {
        for field in ["state", "proposal", "next_actions", "task_id"] {
            assert!(response.get(field).is_none(), "discovery cannot expose execution state: {response}");
        }
    }
    assert_eq!(task_count(&intake, &person).await, 0);
    let executions: i64 = sqlx::query_scalar("SELECT count(*) FROM executions e JOIN tasks t ON t.id=e.task_id WHERE t.owner_id=$1")
        .bind(person.owner).fetch_one(&intake.pool).await.unwrap();
    let approvals: i64 = sqlx::query_scalar("SELECT count(*) FROM authorizations a JOIN tasks t ON t.id=a.task_id WHERE t.owner_id=$1")
        .bind(person.owner).fetch_one(&intake.pool).await.unwrap();
    let charges: i64 = sqlx::query_scalar("SELECT count(*) FROM public.balance_charges WHERE user_id=$1")
        .bind(person.owner).fetch_one(&intake.pool).await.unwrap();
    assert_eq!((executions, approvals, charges), (0,0,0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn researched_plan_refuses_undeclared_fields_without_a_quote_or_charge_and_recovers_same_reply() {
    let intake=Intake::start().await;
    let person=Person::new();
    let question="Jaarrekening van Coolblue met publicatiedatum voor Coolblue";
    let mut raw=ask_v2_capabilities();
    raw["capabilities"][0]["outputs"]=json!(["accounts","publication_dates"]);
    let parsed=apiosk_gateway_v2::capability_intake::normalize(&raw.to_string(),question,"displayed-fixture").unwrap();
    let (status,search)=intake.send(&person.bearer,"/v2/ask-v2/search",Some(json!({"origin":"apiosk","parsed_request":parsed}))).await;
    assert_eq!(status,StatusCode::OK,"{search}");
    let body=json!({"request_id":Uuid::new_v4(),"question":question,"parsed_request":parsed,"search_id":search["search_id"]});
    let (status,refused)=intake.send(&person.bearer,"/v2/ask-v2/research",Some(body.clone())).await;
    assert_eq!(status,StatusCode::OK,"{refused}");
    assert_eq!(refused["status"],"needs_input","{refused}");
    assert!(refused["proposal"].is_null());
    let task=refused["state"]["state_ref"].as_str().unwrap().parse::<Uuid>().unwrap();
    let document:Value=sqlx::query_scalar("SELECT document FROM tasks WHERE id=$1").bind(task).fetch_one(&intake.pool).await.unwrap();
    assert_eq!(document["actions"][0]["kind"],"planning_clarification");
    assert!(document["actions"][0]["missing_output_contracts"].to_string().contains("publication_dates"));
    let reply=document["conversation"].as_array().unwrap().last().unwrap()["reply"].as_str().unwrap();
    assert!(reply.contains("publication dates") && !reply.contains("publication_dates"));
    assert!(!reply.contains("toon eerst de prijs"));
    let (_,recovered)=intake.send(&person.bearer,"/v2/ask-v2/research",Some(body)).await;
    assert_eq!(recovered,refused);
    let (_,read)=intake.send(&person.bearer,&format!("/v2/tasks/{task}"),None).await;
    assert!(read["proposal"].is_null());
    assert_eq!(intake.model.chats().len(),1,"recovery neither replans nor rewrites the factual refusal");
    assert_no_purchase(&intake,&person,task).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn ask_v2_research_keeps_signed_bazaar_matches_in_the_plan_and_saved_task() {
    let intake = Intake::start().await;
    let person = Person::new();
    intake.model.bazaar_resources.lock().unwrap().push(json!({
        "type":"http","resource":"https://example.test/annual-accounts","serviceName":"Additional annual accounts",
        "description":"Company financial statements and annual accounts by registered company identifier."
    }));
    let question = "Jaarrekening van Coolblue";
    let parsed = apiosk_gateway_v2::capability_intake::normalize(&ask_v2_capabilities().to_string(), question, "displayed-fixture").unwrap();
    let (_, local) = intake.send(&person.bearer, "/v2/ask-v2/search", Some(json!({"origin":"apiosk","parsed_request":parsed}))).await;
    let (status, external) = intake.send(&person.bearer, "/v2/ask-v2/search", Some(json!({"origin":"coinbase","parsed_request":parsed}))).await;
    assert_eq!(status, StatusCode::OK, "{external}");
    assert_eq!(external["matches"][0]["coinbase"].as_array().unwrap().len(), 1);
    assert!(external["discovery_id"].is_string());
    assert!(external["search_id"].is_null());
    let body = json!({"request_id":Uuid::new_v4(),"question":question,"parsed_request":parsed,
        "search_id":local["search_id"],"discovery_id":external["discovery_id"]});
    let mut swapped = body.clone();
    swapped["discovery_id"] = local["search_id"].clone();
    assert_eq!(intake.send(&person.bearer, "/v2/ask-v2/research", Some(swapped)).await.0, StatusCode::CONFLICT);
    let (status, response) = intake.send(&person.bearer, "/v2/ask-v2/research", Some(body.clone())).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let task: Uuid = response["state"]["state_ref"].as_str().unwrap().parse().unwrap();
    let document: Value = sqlx::query_scalar("SELECT document FROM tasks WHERE id=$1").bind(task).fetch_one(&intake.pool).await.unwrap();
    let candidates: Vec<apiosk_gateway_v2::capability_intake::Candidate> = serde_json::from_value(external["matches"][0]["coinbase"].clone()).unwrap();
    assert_eq!(document["capability_intake"]["matches"][0]["coinbase"], json!(candidates));
    assert_eq!(document["capability_intake"]["matches"][0]["coinbase_status"], "complete");
    assert!(intake.model.chats()[0]["source_matches"][0]["additional_coinbase_results"].is_array());
    assert_eq!(intake.send(&person.bearer, "/v2/ask-v2/research", Some(body)).await.1, response);
    assert!(intake.edge.calls.lock().unwrap().is_empty(), "discovery and planning never execute providers");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn ask_v2_research_preserves_displayed_parse_quotes_once_and_never_extracts_again() {
    let intake=Intake::start().await;
    let person=Person::new();
    let question="Jaarrekening van Coolblue";
    let parsed=apiosk_gateway_v2::capability_intake::normalize(&ask_v2_capabilities().to_string(),question,"displayed-fixture").unwrap();
    let mut body=json!({"request_id":Uuid::new_v4(),"question":question,"parsed_request":parsed});
    for bearer in ["invalid-fixture-bearer",ORGANISATION_KEY,"apk_access_local_fixture"] {
        let (status,_)=intake.send(bearer,"/v2/ask-v2/research",Some(body.clone())).await;
        assert!(matches!(status,StatusCode::UNAUTHORIZED|StatusCode::FORBIDDEN));
    }
    assert_eq!(task_count(&intake,&person).await,0);
    let mut changed=body.clone();changed["question"]=json!("Jaarrekening van another company");
    let (status,_)=intake.send(&person.bearer,"/v2/ask-v2/research",Some(changed)).await;
    assert_eq!(status,StatusCode::BAD_REQUEST);
    assert_eq!(task_count(&intake,&person).await,0);
    let (status,missing)=intake.send(&person.bearer,"/v2/ask-v2/research",Some(body.clone())).await;
    assert_eq!(status,StatusCode::CONFLICT);
    assert_eq!(missing["errors"][0]["code"],"search_refresh_required");
    let (status,search)=intake.send(&person.bearer,"/v2/ask-v2/search",Some(json!({"origin":"apiosk","parsed_request":parsed}))).await;
    assert_eq!(status,StatusCode::OK,"{search}");
    assert!(search["search_id"].is_string());
    body["search_id"]=search["search_id"].clone();
    let other=Person::new();
    let (status,_)=intake.send(&other.bearer,"/v2/ask-v2/research",Some(body.clone())).await;
    assert_eq!(status,StatusCode::CONFLICT,"a snapshot never changes its authenticated owner");
    let mut changed_parse=body.clone();changed_parse["parsed_request"]["capabilities"][0]["outputs"]=json!(["other_output"]);
    let (status,_)=intake.send(&person.bearer,"/v2/ask-v2/research",Some(changed_parse)).await;
    assert_eq!(status,StatusCode::CONFLICT,"a changed extraction needs a new search");
    assert_eq!(task_count(&intake,&person).await,0);
    let (status,quoted)=intake.send(&person.bearer,"/v2/ask-v2/research",Some(body.clone())).await;
    assert_eq!(status,StatusCode::OK,"{quoted}");
    assert_eq!(quoted["status"],"ready","{quoted}");
    assert!(quoted["proposal"].is_object());
    assert_eq!(task_count(&intake,&person).await,1);
    let chats=intake.model.chats();
    assert_eq!(chats.len(),1);
    assert_eq!(chats[0]["parsed_request"],body["parsed_request"]);
    assert_eq!(chats[0]["question"],question);
    let stored=&quoted["context_view"]["capability_intake"]["matches"];
    let displayed:Vec<apiosk_gateway_v2::capability_intake::Match>=serde_json::from_value(search["matches"].clone()).unwrap();
    assert_eq!(*stored,serde_json::to_value(displayed).unwrap(),"the exact displayed source set survives planning");
    assert!(intake.model.ask_v2_requests.lock().unwrap().is_empty());
    assert_eq!(intake.model.count("parse"),0);
    assert_eq!(intake.model.count("search"),0);
    let (status,retried)=intake.send(&person.bearer,"/v2/ask-v2/research",Some(body.clone())).await;
    assert_eq!(status,StatusCode::OK);
    assert_eq!(retried,quoted,"stable request id recovers the same one-price proposal");
    assert_eq!(intake.model.chats().len(),1);
    assert_eq!(task_count(&intake,&person).await,1);
    assert!(intake.edge.calls.lock().unwrap().is_empty());
    let approvals:i64=sqlx::query_scalar("SELECT count(*) FROM authorizations a JOIN tasks t ON t.id=a.task_id WHERE t.owner_id=$1").bind(person.owner).fetch_one(&intake.pool).await.unwrap();
    let charges:i64=sqlx::query_scalar("SELECT count(*) FROM public.balance_charges WHERE user_id=$1").bind(person.owner).fetch_one(&intake.pool).await.unwrap();
    assert_eq!((approvals,charges),(0,0));
    // Simulate the same issued snapshot after its TTL without sleeping or
    // calling a provider. Only the isolated fixture's saved receipt is changed.
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let token=body["search_id"].as_str().unwrap();
    let mut snapshot:Value=serde_json::from_slice(&URL_SAFE_NO_PAD.decode(token.split_once('.').unwrap().0).unwrap()).unwrap();
    snapshot["expires"]=json!(chrono::Utc::now().timestamp()-1);
    let payload=serde_json::to_vec(&snapshot).unwrap();
    let mut mac=Hmac::<Sha256>::new_from_slice(&[7;32]).unwrap();
    mac.update(b"ask-search-snapshot-v1\0");mac.update(&payload);
    let expired=format!("{}.{}",URL_SAFE_NO_PAD.encode(&payload),hex::encode(mac.finalize().into_bytes()));
    body["search_id"]=json!(expired);
    let request=DiscoverRequest { request_id:serde_json::from_value(body["request_id"].clone()).unwrap(),question:question.into(),state:None,context_delta:Default::default() };
    let hash=apiosk_gateway_v2::state::digest(&("ask-research",&request,&parsed,&expired));
    sqlx::query("UPDATE requests SET payload_hash=$3 WHERE owner_id=$1 AND request_id=$2").bind(person.owner).bind(request.request_id).bind(hash).execute(&intake.pool).await.unwrap();
    let (status,recovered)=intake.send(&person.bearer,"/v2/ask-v2/research",Some(body.clone())).await;
    assert_eq!(status,StatusCode::OK);
    assert_eq!(recovered,quoted,"an expired snapshot still recovers an existing exact receipt");
    body["request_id"]=json!(Uuid::new_v4());
    let (status,expired_reply)=intake.send(&person.bearer,"/v2/ask-v2/research",Some(body)).await;
    assert_eq!(status,StatusCode::CONFLICT);
    assert_eq!(expired_reply["errors"][0]["code"],"search_refresh_required");
    assert_eq!(task_count(&intake,&person).await,1);
    assert_eq!(intake.model.chats().len(),1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn ask_v2_authenticates_app_callers_before_parse_or_search() {
    let intake = Intake::start().await;
    let parse = json!({"request_id":Uuid::new_v4(),"question":"Jaarrekening van Coolblue"});
    let parsed = apiosk_gateway_v2::capability_intake::normalize(&ask_v2_capabilities().to_string(), "Jaarrekening van Coolblue", "fixture").unwrap();
    let search = json!({"parsed_request":parsed,"origin":"apiosk"});
    for (path, body) in [("/v2/ask-v2/parse", parse.clone()), ("/v2/ask-v2/search", search.clone())] {
        let response = intake.http.post(format!("{}{path}",intake.gateway)).json(&body).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "anonymous {path}");
        let (status, response) = intake.send("invalid-fixture-bearer", path, Some(body.clone())).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{response}");
        let (status, response) = intake.send(ORGANISATION_KEY, path, Some(body)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "App-only {path}: {response}");
    }
    // A connected chatbot fills in the capability object itself: it may
    // search, and sees each executable source's inputs, but never parses.
    let (status, response) = intake.send("apk_access_local_fixture", "/v2/ask-v2/parse", Some(parse.clone())).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{response}");
    let (status, found) = intake.send("apk_access_local_fixture", "/v2/ask-v2/search", Some(search)).await;
    assert_eq!(status, StatusCode::OK, "{found}");
    let candidate = &found["matches"][0]["apiosk"][0];
    assert_eq!(candidate["availability"], "supported", "{found}");
    assert_eq!(candidate["endpoint"]["endpoint_id"], candidate["endpoint_id"], "{found}");
    assert!(candidate["endpoint"]["inputs"].as_array().is_some_and(|inputs| !inputs.is_empty()), "{found}");
    assert!(candidate["endpoint"]["price"]["buyer_atomic"].is_string(), "{found}");
    assert!(intake.model.ask_v2_requests.lock().unwrap().is_empty());
    assert!(intake.model.stages.lock().unwrap().is_empty());
    let first = Person::new();
    let second = Person::new();
    // Exhausting one identity must not consume another person's allowance.
    let mut accepted = 0;
    loop {
        let (status, response) = intake.send(&first.bearer, "/v2/ask-v2/parse", Some(parse.clone())).await;
        if status == StatusCode::TOO_MANY_REQUESTS { break; }
        assert_eq!(status, StatusCode::OK, "{response}");
        accepted += 1;
        assert!(accepted <= 30, "per-owner parser limit must be bounded");
    }
    assert!(accepted > 0);
    let (status, response) = intake.send(&second.bearer, "/v2/ask-v2/parse", Some(parse)).await;
    assert_eq!(status, StatusCode::OK, "independent caller allowance: {response}");
    assert_eq!(intake.model.ask_v2_requests.lock().unwrap().len(), accepted + 1);
    assert_eq!(task_count(&intake, &first).await, 0);
    assert_eq!(task_count(&intake, &second).await, 0);
    assert!(intake.model.stages.lock().unwrap().is_empty());
}

/// A question is measured in Unicode scalar values on every surface: 4,000
/// accented letters and emoji are accepted where the byte rule refused them,
/// and 4,001 characters are refused before any interpretation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn intake_counts_characters_not_bytes() {
    let intake = Intake::start().await;
    let person = Person::new();
    let full = format!("{}{}", "é".repeat(2000), "😀".repeat(2000));
    assert!(full.len() > 4000, "the byte rule would have refused this question");
    let accepted = intake.chat(&person, json!({"request_id":Uuid::new_v4(),"question":full,"state":null})).await;
    assert_eq!(accepted["status"], "needs_input", "{accepted}");
    assert_eq!(intake.model.chats().len(), 1, "the question reached interpretation");

    let refused = intake.chat(&person, json!({"request_id":Uuid::new_v4(),"question":format!("{full}?"),"state":null})).await;
    assert_eq!(codes(&refused), ["operation_failed"], "{refused}");
    assert_eq!(intake.model.chats().len(), 1, "an over-long question is refused before interpretation");

    let ask = |question: String| {
        intake.http.post(format!("{}/v2/ask", intake.gateway)).bearer_auth(ORGANISATION_KEY).json(&json!({"question":question})).send()
    };
    // API and connected-agent questions are read by the Ask v2 parse first.
    let parses = || intake.model.ask_v2_requests.lock().unwrap().len();
    let public = ask(full.clone()).await.unwrap();
    assert_ne!(public.status(), StatusCode::BAD_REQUEST, "the public API counts the same way");
    let reached = parses();
    assert!(reached > 0, "the question reached interpretation");
    let public = ask("é".repeat(16_001)).await.unwrap();
    assert_eq!(public.status(), StatusCode::BAD_REQUEST);
    assert_eq!(public.json::<Value>().await.unwrap()["error"]["code"], "invalid_question");
    assert_eq!(parses(), reached, "an over-long question is refused before interpretation");
    let owned: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks WHERE owner_id=$1").bind(person.owner).fetch_one(&intake.pool).await.unwrap();
    assert_eq!(owned, 1, "only the accepted App question created a task");
    println!("intake_counts_characters_not_bytes: legacy chat limit preserved; public Ask uses the composer 16000-character limit");
}

/// The endpoint id of the fixture service with this capability.
fn endpoint(capability: &str) -> String {
    let catalog: Value = serde_json::from_str(include_str!("fixtures/integration-catalog.json")).unwrap();
    catalog["services"].as_array().unwrap().iter().find(|s| s["contract"]["capability_slug"] == capability).unwrap()["endpoint_id"].as_str().unwrap().to_owned()
}

fn selection(endpoints: Value) -> Value {
    json!({"version":1,"mode":"restrict","endpoints":endpoints})
}

async fn task_count(intake: &Intake, person: &Person) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM tasks WHERE owner_id=$1").bind(person.owner).fetch_one(&intake.pool).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn ask_v2_prepare_saves_exact_endpoint_quote_without_model_or_payment() {
    let intake = Intake::start().await;
    let person = Person::new();
    let endpoint = endpoint("company.accounts");
    let (status, descriptor) = intake.send(&person.bearer, &format!("/v2/ask-v2/endpoints/{endpoint}?capability=company.accounts"), None).await;
    assert_eq!(status, StatusCode::OK, "{descriptor}");
    assert_eq!(descriptor["endpoint_id"], endpoint);
    assert_eq!(descriptor["capability"], "company.accounts");
    assert_eq!(descriptor["inputs"], json!([{"field":"company.id","required":true,"schema":{"type":"string"}}]));
    assert_eq!(descriptor["price"]["buyer_atomic"], "115");
    assert_eq!(task_count(&intake, &person).await, 0);

    let request = json!({"request_id":Uuid::new_v4(),"endpoint_id":endpoint,"capability":"company.accounts","input":{"company.id":"12345678"}});
    let (status, quote) = intake.send(&person.bearer, "/v2/ask-v2/prepare", Some(request.clone())).await;
    assert_eq!(status, StatusCode::OK, "{quote}");
    assert_eq!(quote["status"], "ready", "{quote}");
    assert_eq!(quote["proposal"]["max_total_atomic"], "115");
    assert_eq!(quote["proposal"]["steps"], json!(["company.accounts"]));
    assert!(quote["next_actions"].as_array().unwrap().iter().any(|action| action["kind"] == "execute_quoted_step" && action["requires_authorization"] == true));
    let task: Uuid = quote["state"]["state_ref"].as_str().unwrap().parse().unwrap();
    let saved: Value = sqlx::query_scalar("SELECT document FROM tasks WHERE id=$1").bind(task).fetch_one(&intake.pool).await.unwrap();
    assert_eq!(saved["intent"]["goals"][0]["constraints"]["endpoint_id"], endpoint);
    assert_eq!(saved["intent"]["subjects"][0]["values"]["company.id"], "12345678");
    assert_eq!(saved["intent"]["answer_operations"], json!([]));
    assert_eq!(saved["proposal"]["steps"][0]["service"]["endpoint_id"], endpoint);
    assert_no_purchase(&intake, &person, task).await;
    assert!(intake.model.stages.lock().unwrap().is_empty());
    assert!(intake.model.ask_v2_requests.lock().unwrap().is_empty());

    let (_, replay) = intake.send(&person.bearer, "/v2/ask-v2/prepare", Some(request.clone())).await;
    assert_eq!(replay, quote, "the same request returns the same saved quote");
    assert_eq!(task_count(&intake, &person).await, 1);
    let mut changed = request;
    changed["input"]["company.id"] = json!("changed");
    let (_, conflict) = intake.send(&person.bearer, "/v2/ask-v2/prepare", Some(changed)).await;
    assert!(codes(&conflict).contains(&"request_conflict"));
    let other = Person::new();
    let (status, _) = intake.send(&other.bearer, &format!("/v2/tasks/{task}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "another owner cannot read the prepared task");
    assert_no_purchase(&intake, &person, task).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn ask_v2_prepare_includes_native_name_lookup_in_one_quote() {
    let intake = Intake::start().await;
    let person = Person::new();
    let endpoint = endpoint("company.accounts");
    let (_, descriptor) = intake.send(&person.bearer, &format!("/v2/ask-v2/endpoints/{endpoint}?capability=company.accounts"), None).await;
    assert_eq!(descriptor["lookup"]["field"], "company.name");
    assert_eq!(descriptor["lookup"]["resolves"], json!(["company.id"]));
    let request = json!({"request_id":Uuid::new_v4(),"endpoint_id":endpoint,"capability":"company.accounts","input":{"company.name":"Example Ltd"}});
    let (status, quote) = intake.send(&person.bearer, "/v2/ask-v2/prepare", Some(request.clone())).await;
    assert_eq!(status, StatusCode::OK, "{quote}");
    assert_eq!(quote["status"], "ready", "{quote}");
    assert_eq!(quote["proposal"]["max_total_atomic"], "138");
    assert_eq!(quote["proposal"]["steps"], json!(["company.search", "company.accounts"]));
    let task: Uuid = quote["state"]["state_ref"].as_str().unwrap().parse().unwrap();
    assert_no_purchase(&intake, &person, task).await;
    assert!(intake.model.stages.lock().unwrap().is_empty());
    assert!(intake.model.ask_v2_requests.lock().unwrap().is_empty());
    let (_, replay) = intake.send(&person.bearer, "/v2/ask-v2/prepare", Some(request)).await;
    assert_eq!(replay, quote);
    assert_eq!(task_count(&intake, &person).await, 1);
}

/// `apiosk_discover` for a named company whose matched operation takes only a
/// registry number: the planner binds no name, and the quote still starts
/// with the name search (as Ask v2's endpoint `lookup` does) instead of asking
/// the chatbot's person for the number. Nothing is bought or approved.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn discover_looks_up_a_named_company_instead_of_asking_for_its_number() {
    let intake = Intake::start().await;
    let (status, quote) = intake.send("apk_access_local_fixture", "/v2/discover", Some(json!({"request_id":Uuid::new_v4(),"question":NAMELESS}))).await;
    assert_eq!(status, StatusCode::OK, "{quote}");
    assert_eq!(quote["status"], "ready", "{quote}");
    assert!(quote["next_actions"].as_array().unwrap().iter().all(|action| action["kind"] != "supply_input"), "{quote}");
    assert_eq!(quote["proposal"]["steps"], json!(["company.search", "company.accounts"]));
    assert_eq!(quote["proposal"]["max_total_atomic"], "138");
    assert_eq!(quote["proposal"]["step_details"][1]["dependencies"][0]["field"], "company.id");
    assert_eq!(intake.model.ask_v2_requests.lock().unwrap().len(), 1, "one Ask v2 parse");
    assert_eq!(intake.model.count("plan"), 1, "one planning pass");
    let task: Uuid = quote["state"]["state_ref"].as_str().unwrap().parse().unwrap();
    let saved: Value = sqlx::query_scalar("SELECT document->'intent' FROM tasks WHERE id=$1").bind(task).fetch_one(&intake.pool).await.unwrap();
    assert_eq!(saved["subjects"][0]["values"]["company.name"], "Coolblue", "the parsed name seeds the lookup and is kept for a requote");
    let bought: i64 = sqlx::query_scalar("SELECT (SELECT count(*) FROM executions WHERE task_id=$1)+(SELECT count(*) FROM authorizations WHERE task_id=$1)")
        .bind(task).fetch_one(&intake.pool).await.unwrap();
    assert_eq!(bought, 0);
    assert!(intake.edge.calls.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn ask_v2_prepare_rejects_unknown_inputs_and_authenticates_before_task_creation() {
    let intake = Intake::start().await;
    let person = Person::new();
    let endpoint = endpoint("company.accounts");
    for input in [json!({"unrelated.name":"Example"}), json!({"company.id":123}), json!({"max_total_atomic":"0"})] {
        let (status, response) = intake.send(&person.bearer, "/v2/ask-v2/prepare", Some(json!({"request_id":Uuid::new_v4(),"endpoint_id":endpoint,"input":input}))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["status"], "needs_input", "{response}");
        assert!(codes(&response).contains(&"invalid_endpoint_input"));
        assert!(response["proposal"].is_null() && response["state"].is_null());
    }
    let (status, response) = intake.send(&person.bearer, "/v2/ask-v2/prepare", Some(json!({"request_id":Uuid::new_v4(),"endpoint_id":Uuid::new_v4(),"input":{}}))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(codes(&response).contains(&"endpoint_unavailable"));
    for bearer in ["invalid", ORGANISATION_KEY] {
        let (status, _) = intake.send(bearer, "/v2/ask-v2/prepare", Some(json!({"request_id":Uuid::new_v4(),"endpoint_id":endpoint,"input":{"company.id":"123"}}))).await;
        assert!(!status.is_success());
        let (status, _) = intake.send(bearer, &format!("/v2/ask-v2/endpoints/{endpoint}"), None).await;
        assert!(!status.is_success());
    }
    // A connected chatbot reads the same inputs and prepares the same kind of
    // quote; paying still needs its approval.
    let (status, described) = intake.send("apk_access_local_fixture", &format!("/v2/ask-v2/endpoints/{endpoint}"), None).await;
    assert_eq!(status, StatusCode::OK, "{described}");
    assert!(described["inputs"].is_array(), "{described}");
    let (status, prepared) = intake.send("apk_access_local_fixture", "/v2/ask-v2/prepare", Some(json!({"request_id":Uuid::new_v4(),"endpoint_id":endpoint,"input":{}}))).await;
    assert_eq!(status, StatusCode::OK, "{prepared}");
    assert_eq!(prepared["protocol_version"], "2", "{prepared}");
    assert!(prepared["billing"].is_null() || prepared["billing"]["total_charged"] == "0", "{prepared}");
    assert_eq!(task_count(&intake, &person).await, 0);
    assert!(intake.model.stages.lock().unwrap().is_empty());
    assert!(intake.model.ask_v2_requests.lock().unwrap().is_empty());

    let (status, missing) = intake.send(&person.bearer, "/v2/ask-v2/prepare", Some(json!({"request_id":Uuid::new_v4(),"endpoint_id":endpoint,"input":{}}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(missing["status"], "needs_input", "{missing}");
    assert!(missing["proposal"].is_null());
    assert!(missing["next_actions"].as_array().unwrap().iter().any(|action| action["kind"] == "supply_input"));
    let task = missing["state"]["state_ref"].as_str().unwrap().parse().unwrap();
    assert_no_purchase(&intake, &person, task).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn ask_v2_external_purchase_cannot_be_overwritten_by_chat() {
    use apiosk_gateway_v2::{ask_v2::{external::{BoundInput, Offer}, purchase::Purchase}, state::Task};
    let intake = Intake::start().await;
    let person = Person::new();
    let id = Uuid::new_v4();
    let saved = Task { external_purchase: Some(Purchase {
        request_hash:"fixture-request".into(),
        source: Offer { resource:"https://fixture.example/data".into(), method:"GET".into(), name:"Fixture source".into(), description:"Synthetic source".into(), source:"coinbase-x402-bazaar".into(), host:"fixture.example".into(), network:Some("eip155:8453".into()), provider_atomic:100, total_atomic:115, inputs:vec![], input_schema:json!({"type":"object","properties":{}}) },
        input:BoundInput { inputs:json!({}), input_parts:json!({"path":{},"query":{},"body":{}}) },
        quote_ref:Uuid::new_v4(), expires_at:chrono::Utc::now()+chrono::Duration::minutes(10), execution_id:Uuid::new_v4(), selection_id:Uuid::new_v4(), approved_at:None, refusal:None, receipt:None,
    }), ..Task::default() };
    let document = serde_json::to_value(&saved).unwrap();
    let revision: i64 = sqlx::query_scalar("INSERT INTO tasks(id,owner_id,document) VALUES($1,$2,$3) RETURNING revision")
        .bind(id).bind(person.owner).bind(&document).fetch_one(&intake.pool).await.unwrap();
    let state = Signer::new(vec![7;32]).unwrap().issue(person.owner, id, revision, saved.focus);
    let response = intake.chat(&person, json!({"request_id":Uuid::new_v4(),"question":"Jaarrekening van Coolblue","state":state})).await;
    assert_eq!(response["status"], "failed");
    assert!(response["errors"][0]["message"].as_str().unwrap().contains("External source purchases"));
    let unchanged: Value = sqlx::query_scalar("SELECT document FROM tasks WHERE id=$1").bind(id).fetch_one(&intake.pool).await.unwrap();
    assert_eq!(unchanged, document, "an old chat cannot replace the saved external quote");
    assert!(intake.model.stages.lock().unwrap().is_empty());
    assert_no_purchase(&intake, &person, id).await;
    let (status, purchase) = intake.send(&person.bearer, &format!("/v2/ask-v2/external/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{purchase}");
    assert_eq!(purchase["status"], "requires_approval");
    assert_eq!(purchase["quote"]["max_total_atomic"], "115");
    assert_no_purchase(&intake, &person, id).await;
}

fn external_offer() -> apiosk_gateway_v2::ask_v2::external::Offer {
    use apiosk_gateway_v2::ask_v2::external::Offer;
    Offer { resource:"https://fixture.example/data".into(), method:"GET".into(), name:"Fixture source".into(), description:"Synthetic source".into(), source:"coinbase-x402-bazaar".into(), host:"fixture.example".into(), network:Some("eip155:8453".into()), provider_atomic:100, total_atomic:115, inputs:vec![], input_schema:json!({"type":"object","properties":{},"additionalProperties":false}) }
}

async fn prepare_external(intake: &Intake, person: &Person, id: Uuid, workspace: Option<Uuid>) -> Value {
    use apiosk_gateway_v2::ask_v2::{external::BoundInput, purchase};
    let principal = Principal {owner:person.owner,connection:None,workspace};
    let offer = external_offer();
    let input = BoundInput {inputs:json!({}),input_parts:json!({"path":{},"query":{},"body":{}})};
    let hash = purchase::request_hash(&principal, &offer.resource, &offer.method, "company data", &input.inputs);
    purchase::prepare(&intake.engine, &principal, id, hash, offer, input).await.unwrap()
}

async fn external_status(intake: &Intake, person: &Person, id: Uuid) -> Value {
    let (status, view) = intake.send(&person.bearer, &format!("/v2/ask-v2/external/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    view
}

async fn wait_external(intake: &Intake, person: &Person, id: Uuid, expected: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let view = external_status(intake, person, id).await;
            if view["status"] == expected { return view; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.unwrap_or_else(|_| panic!("purchase did not reach {expected}"))
}

async fn external_balance(intake: &Intake, person: &Person) -> i64 {
    sqlx::query_scalar("SELECT balance_micro_usd FROM public.user_balances WHERE user_id=$1").bind(person.owner).fetch_one(&intake.pool).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn ask_v2_external_one_approval_charges_once_and_recovery_never_redispatches() {
    let intake = Intake::start().await;
    let person = Person::new();
    sqlx::query("INSERT INTO public.user_balances(user_id,balance_micro_usd) VALUES($1,1000)").bind(person.owner).execute(&intake.pool).await.unwrap();
    let id = Uuid::new_v4();
    let quote = prepare_external(&intake, &person, id, None).await;
    assert_eq!(quote["status"], "requires_approval");
    assert_eq!(quote["quote"]["max_total_atomic"], "115");
    assert_eq!(prepare_external(&intake, &person, id, None).await, quote);
    assert_eq!(external_status(&intake, &person, id).await, quote);
    assert_no_purchase(&intake, &person, id).await;
    assert_eq!(external_balance(&intake, &person).await, 1000);
    assert!(intake.edge.calls.lock().unwrap().is_empty());
    let path = format!("/v2/ask-v2/external/{id}/authorize");
    let approve = json!({"quote_ref":quote["quote"]["quote_ref"],"max_total_atomic":"115"});
    let (first, second) = tokio::join!(intake.send(&person.bearer, &path, Some(approve.clone())), intake.send(&person.bearer, &path, Some(approve.clone())));
    assert_eq!(first.0, StatusCode::OK, "{}", first.1);
    assert_eq!(second.0, StatusCode::OK, "{}", second.1);
    let finished = wait_external(&intake, &person, id, "succeeded").await;
    assert_eq!(finished["result_present"], true);
    assert_eq!(finished["result"], json!({"rows":[{"id":"fixture-record"}]}));
    assert_eq!(finished["billing"]["charged_micro_usd"], 115);
    assert_eq!(external_balance(&intake, &person).await, 885);
    for _ in 0..3 { assert_eq!(external_status(&intake, &person, id).await["status"], "succeeded"); }
    sqlx::query("UPDATE tasks SET document=jsonb_set(document,'{external_purchase,expires_at}',to_jsonb(now()-interval '1 minute')) WHERE id=$1").bind(id).execute(&intake.pool).await.unwrap();
    let (status, replay) = intake.send(&person.bearer, &path, Some(approve)).await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay["status"], "succeeded", "replaying approval after quote expiry only reads the saved purchase");
    assert_eq!(intake.edge.calls.lock().unwrap().len(), 1);
    let charges: i64 = sqlx::query_scalar("SELECT count(*) FROM public.balance_charges WHERE user_id=$1").bind(person.owner).fetch_one(&intake.pool).await.unwrap();
    let selections: i64 = sqlx::query_scalar("SELECT count(*) FROM public.user_selected_apis WHERE user_id=$1").bind(person.owner).fetch_one(&intake.pool).await.unwrap();
    assert_eq!((charges,selections), (1,1));
    assert!(intake.model.stages.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn ask_v2_external_refuses_wrong_quote_owner_and_workspace_before_any_charge() {
    use apiosk_gateway_v2::ask_v2::purchase;
    let intake = Intake::start().await;
    let person = Person::new();
    let id = Uuid::new_v4();
    let quote = prepare_external(&intake, &person, id, None).await;
    let path = format!("/v2/ask-v2/external/{id}/authorize");
    for approval in [json!({"quote_ref":quote["quote"]["quote_ref"],"max_total_atomic":"114"}), json!({"quote_ref":Uuid::new_v4(),"max_total_atomic":"115"})] {
        assert_eq!(intake.send(&person.bearer, &path, Some(approval)).await.0, StatusCode::CONFLICT);
    }
    let approval = json!({"quote_ref":quote["quote"]["quote_ref"],"max_total_atomic":"115"});
    let other = Person::new();
    assert_eq!(intake.send(&other.bearer, &path, Some(approval.clone())).await.0, StatusCode::NOT_FOUND);
    assert_eq!(intake.send(&other.bearer, &format!("/v2/ask-v2/external/{id}"), None).await.0, StatusCode::NOT_FOUND);
    sqlx::query("UPDATE tasks SET document=jsonb_set(document,'{external_purchase,expires_at}',to_jsonb(now()-interval '1 minute')) WHERE id=$1").bind(id).execute(&intake.pool).await.unwrap();
    assert_eq!(intake.send(&person.bearer, &path, Some(approval)).await.0, StatusCode::CONFLICT);
    let (workspace, other_workspace) = (Uuid::new_v4(), Uuid::new_v4());
    for workspace in [workspace, other_workspace] {
        sqlx::query("INSERT INTO public.fixture_workspace_access VALUES($1,$2,true)").bind(person.owner).bind(workspace).execute(&intake.pool).await.unwrap();
    }
    let scoped = Uuid::new_v4();
    let scoped_quote = prepare_external(&intake, &person, scoped, Some(workspace)).await;
    let wrong = Principal {owner:person.owner,connection:None,workspace:Some(other_workspace)};
    assert!(purchase::view(&intake.engine, &wrong, scoped).await.is_err());
    let quote_ref: Uuid = scoped_quote["quote"]["quote_ref"].as_str().unwrap().parse().unwrap();
    assert!(purchase::authorize(intake.engine.clone(), wrong, person.bearer.clone(), scoped, quote_ref, "115").await.is_err());
    let correct = Principal {owner:person.owner,connection:None,workspace:Some(workspace)};
    sqlx::query("UPDATE public.fixture_workspace_access SET can_spend=false WHERE actor_id=$1 AND workspace_id=$2").bind(person.owner).bind(workspace).execute(&intake.pool).await.unwrap();
    assert!(purchase::authorize(intake.engine.clone(), correct, person.bearer.clone(), scoped, quote_ref, "115").await.unwrap_err().to_string().contains("spending is not permitted"));
    assert!(intake.edge.calls.lock().unwrap().is_empty());
    assert_no_purchase(&intake, &person, id).await;
    assert_no_purchase(&intake, &person, scoped).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn ask_v2_external_recovers_edge_result_when_history_write_fails_including_null() {
    let intake = Intake::start().await;
    for result in [json!({"fallback":"received from the paid source"}), Value::Null] {
        let person = Person::new();
        sqlx::query("INSERT INTO public.user_balances(user_id,balance_micro_usd) VALUES($1,1000)").bind(person.owner).execute(&intake.pool).await.unwrap();
        *intake.edge.outcome.lock().unwrap() = EdgeOutcome::Success {result:result.clone(),record_run:false,mismatched_receipt:false};
        let id = Uuid::new_v4();
        let quote = prepare_external(&intake, &person, id, None).await;
        let (status, _) = intake.send(&person.bearer, &format!("/v2/ask-v2/external/{id}/authorize"), Some(json!({"quote_ref":quote["quote"]["quote_ref"],"max_total_atomic":"115"}))).await;
        assert_eq!(status, StatusCode::OK);
        let saved = wait_external(&intake, &person, id, "succeeded").await;
        assert_eq!(saved["result"], result);
        assert_eq!(saved["result_present"], true);
        assert_eq!(saved["result_truncated"], false);
        let runs: i64 = sqlx::query_scalar("SELECT count(*) FROM public.call_runs WHERE user_id=$1").bind(person.owner).fetch_one(&intake.pool).await.unwrap();
        assert_eq!(runs, 0, "result came from the validated fallback, not a history row");
        assert_eq!(external_balance(&intake, &person).await, 885);
        assert_eq!(external_status(&intake, &person, id).await, saved);
    }
    assert_eq!(intake.edge.calls.lock().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn ask_v2_external_ambiguous_and_mismatched_receipts_never_trigger_paid_replay() {
    let intake = Intake::start().await;
    for outcome in [EdgeOutcome::Ambiguous, EdgeOutcome::Success {result:json!({"wrong":"charge"}),record_run:false,mismatched_receipt:true}, EdgeOutcome::Refused] {
        let person = Person::new();
        sqlx::query("INSERT INTO public.user_balances(user_id,balance_micro_usd) VALUES($1,1000)").bind(person.owner).execute(&intake.pool).await.unwrap();
        let refused = matches!(&outcome, EdgeOutcome::Refused);
        *intake.edge.outcome.lock().unwrap() = outcome;
        let before = intake.edge.calls.lock().unwrap().len();
        let id = Uuid::new_v4();
        let quote = prepare_external(&intake, &person, id, None).await;
        let path = format!("/v2/ask-v2/external/{id}/authorize");
        let approval = json!({"quote_ref":quote["quote"]["quote_ref"],"max_total_atomic":"115"});
        assert_eq!(intake.send(&person.bearer, &path, Some(approval.clone())).await.0, StatusCode::OK);
        tokio::time::timeout(Duration::from_secs(3), async { while intake.edge.calls.lock().unwrap().len() == before {tokio::time::sleep(Duration::from_millis(10)).await;} }).await.unwrap();
        if refused {
            let saved = wait_external(&intake, &person, id, "failed").await;
            assert_eq!(saved["error"]["code"], "insufficient_balance");
            assert_eq!(saved["billing"]["status"], "not_charged");
        } else {
            tokio::time::sleep(Duration::from_millis(80)).await;
            sqlx::query("UPDATE tasks SET document=jsonb_set(document,'{external_purchase,approved_at}',to_jsonb(now()-interval '2 minutes')) WHERE id=$1").bind(id).execute(&intake.pool).await.unwrap();
            let saved = external_status(&intake, &person, id).await;
            assert_eq!(saved["status"], "running");
            assert_eq!(saved["result_present"], false);
            assert!(saved["result"].is_null());
            assert_eq!(saved["error"]["code"], "pending_reconciliation");
            let fallback: Value = sqlx::query_scalar("SELECT document->'external_purchase'->'receipt' FROM tasks WHERE id=$1").bind(id).fetch_one(&intake.pool).await.unwrap_or(Value::Null);
            assert!(fallback.is_null(), "an unrelated charge receipt cannot become saved evidence");
        }
        for _ in 0..2 {
            let _ = external_status(&intake, &person, id).await;
            assert_eq!(intake.send(&person.bearer, &path, Some(approval.clone())).await.0, StatusCode::OK);
        }
        assert_eq!(intake.edge.calls.lock().unwrap().len(), before+1);
    }
}

/// The App sends its selected endpoints and entered values as structured
/// context. The model reads them next to the unchanged question; the gateway
/// pins the plan to those endpoints and binds the entered values.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn selection_restricts_and_binds_without_rewriting_the_question() {
    let intake = Intake::start().await;
    let person = Person::new();
    let (search, accounts) = (endpoint("company.search"), endpoint("company.accounts"));
    let question = "Jaarrekening van Example";
    // Search misses its name, so this is interpreted rather than compiled.
    let chosen = selection(json!([{"endpoint_id":search,"inputs":[]},
        {"endpoint_id":accounts,"inputs":[{"fact_type":"company.id","subject_role":"subject","value":"12345678"}]}]));
    let priced = intake.chat(&person, json!({"request_id":Uuid::new_v4(),"question":question,"state":null,"context_delta":{"selection":chosen}})).await;
    assert_eq!(priced["status"], "ready", "{priced}");
    let seen = intake.model.chats();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["question"], question, "the question is sent as typed");
    assert_eq!(seen[0]["selection"]["endpoints"].as_array().unwrap().len(), 2);
    assert_eq!(seen[0]["selection"]["endpoints"][1]["inputs"][0]["value"], "12345678");
    assert_eq!(priced["context_view"]["conversation"][0]["question"], question, "the turn keeps the literal question");
    assert_eq!(priced["proposal"]["steps"], json!(["company.accounts"]), "the entered id makes the search step unnecessary");
    assert_eq!(priced["proposal"]["max_total_atomic"], "115");
    let task: Uuid = priced["state"]["state_ref"].as_str().unwrap().parse().unwrap();
    let intent: Value = sqlx::query_scalar("SELECT document->'intent' FROM tasks WHERE id=$1").bind(task).fetch_one(&intake.pool).await.unwrap();
    assert_eq!(intent["goals"][0]["constraints"]["endpoint_id"], json!(accounts));
    assert_eq!(intent["subjects"][0]["values"]["company.id"], "12345678");

    // A request outside the selection is not planned with another source.
    let outside = intake.chat(&person, json!({"request_id":Uuid::new_v4(),"question":question,"state":null,
        "context_delta":{"selection":selection(json!([{"endpoint_id":search,"inputs":[]}]))}})).await;
    assert_eq!(outside["status"], "needs_input", "{outside}");
    assert!(outside["proposal"].is_null());
    assert!(outside["context_view"]["conversation"][0]["reply"].as_str().unwrap().contains("company.accounts"), "{outside}");
    assert_eq!(intake.model.chats().len(), 2);
    println!("selection_restricts_and_binds_without_rewriting_the_question: pinned to the selected endpoint, entered id bound");
}

/// A selection that cannot be valid is refused before a task exists or the
/// model is asked, and a connected chatbot cannot send one at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn an_invalid_selection_is_refused_before_interpretation() {
    let intake = Intake::start().await;
    let person = Person::new();
    let accounts = endpoint("company.accounts");
    let wrong = selection(json!([{"endpoint_id":accounts,"inputs":[{"fact_type":"company.id","subject_role":"subject","value":12}]}]));
    let refused = intake.chat(&person, json!({"request_id":Uuid::new_v4(),"question":"Jaarrekening van Example","state":null,"context_delta":{"selection":wrong}})).await;
    assert_eq!((&refused["status"], &refused["state"]), (&json!("needs_input"), &Value::Null), "{refused}");
    assert_eq!(refused["errors"][0]["code"], "invalid_selection");
    assert_eq!((&refused["errors"][0]["endpoint_id"], &refused["errors"][0]["fact_type"]), (&json!(accounts), &json!("company.id")));
    assert!(intake.model.chats().is_empty());
    assert_eq!(task_count(&intake, &person).await, 0, "a refused selection creates no task");

    let valid = selection(json!([{"endpoint_id":accounts,"inputs":[]}]));
    let (_, discovered) = intake.send(&person.bearer, "/v2/discover", Some(json!({"request_id":Uuid::new_v4(),"question":"Jaarrekening van Example","context_delta":{"selection":valid}}))).await;
    assert_eq!(codes(&discovered), ["invalid_selection"], "selection is an App chat feature: {discovered}");
    assert!(intake.model.chats().is_empty());

    // An unknown selection field is refused before the request is claimed.
    assert!(serde_json::from_value::<DiscoverRequest>(json!({"request_id":Uuid::new_v4(),"question":"Hallo","state":null,"context_delta":{"selection":{"version":1,"endpoints":[],"unexpected":true}}})).is_err());
    assert_eq!(task_count(&intake, &person).await, 0);
    println!("an_invalid_selection_is_refused_before_interpretation: no task, no model call");
}

/// The App's retired Ask routes answer 410 for a signed-in person, before
/// any claim, model call or task: an old tab is told to reload.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn retired_ask_routes_answer_gone_and_create_nothing() {
    let intake = Intake::start().await;
    let person = Person::new();
    let request_id = Uuid::new_v4();
    for (path, body) in [
        ("/v2/chat".to_owned(), Some(json!({"request_id":request_id,"question":"Jaarrekening van Example","state":null}))),
        (format!("/v2/requests/{request_id}"), None),
        ("/v2/documents".to_owned(), Some(json!({"request_id":Uuid::new_v4(),"purpose":"invoice","file_name":"invoice.pdf"}))),
        (format!("/v2/documents/{}/read", Uuid::new_v4()), Some(json!({"request_id":Uuid::new_v4()}))),
    ] {
        let (status, gone) = intake.send(&person.bearer, &path, body).await;
        assert_eq!(status, StatusCode::GONE, "{path}: {gone}");
        assert_eq!(codes(&gone), ["ask_moved"], "{path}");
    }
    assert!(intake.model.chats().is_empty(), "no retired route reaches interpretation");
    assert_eq!(task_count(&intake, &person).await, 0);
    println!("retired_ask_routes_answer_gone_and_create_nothing: /v2/chat, /v2/requests and /v2/documents answer 410");
}

/// A quote that has not expired, or that no longer matches what is shown,
/// or a task with nothing to refresh, is refused before anything is touched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn requote_refuses_a_valid_or_mismatched_quote() {
    let intake = Intake::start().await;
    let person = Person::new();
    let priced = intake.chat(&person, json!({"request_id":Uuid::new_v4(),"question":"Jaarrekening van Example","state":null})).await;
    assert_eq!(priced["status"], "ready", "{priced}");
    let task: Uuid = priced["state"]["state_ref"].as_str().unwrap().parse().unwrap();

    let still_valid = intake.http.post(format!("{}/v2/tasks/{task}/requote", intake.gateway)).bearer_auth(&person.bearer)
        .json(&json!({"request_id":Uuid::new_v4(),"state":priced["state"],"quote_ref":priced["proposal"]["quote_ref"]}))
        .send().await.unwrap();
    assert_eq!(still_valid.status(), StatusCode::OK);
    let still_valid: Value = still_valid.json().await.unwrap();
    assert_eq!(still_valid["status"], "state_conflict", "{still_valid}");
    assert_eq!(codes(&still_valid), ["quote_still_valid"]);

    let expired = "UPDATE tasks SET document=jsonb_set(document,'{proposal,expires_at}',to_jsonb(now()-interval '1 minute')) WHERE id=$1";
    sqlx::query(expired).bind(task).execute(&intake.pool).await.unwrap();
    let mismatched = intake.http.post(format!("{}/v2/tasks/{task}/requote", intake.gateway)).bearer_auth(&person.bearer)
        .json(&json!({"request_id":Uuid::new_v4(),"state":priced["state"],"quote_ref":Uuid::new_v4()}))
        .send().await.unwrap();
    let mismatched: Value = mismatched.json().await.unwrap();
    assert_eq!(codes(&mismatched), ["quote_mismatch"], "{mismatched}");

    let fresh = intake.chat(&person, json!({"request_id":Uuid::new_v4(),"question":"Hallo","state":null})).await;
    assert_eq!(fresh["status"], "needs_input", "{fresh}");
    let empty_task: Uuid = fresh["state"]["state_ref"].as_str().unwrap().parse().unwrap();
    let nothing_to_refresh = intake.http.post(format!("{}/v2/tasks/{empty_task}/requote", intake.gateway)).bearer_auth(&person.bearer)
        .json(&json!({"request_id":Uuid::new_v4(),"state":fresh["state"],"quote_ref":Uuid::new_v4()}))
        .send().await.unwrap();
    let nothing_to_refresh: Value = nothing_to_refresh.json().await.unwrap();
    assert_eq!(codes(&nothing_to_refresh), ["requote_unavailable"], "{nothing_to_refresh}");
    println!("requote_refuses_a_valid_or_mismatched_quote: quote_still_valid, quote_mismatch, requote_unavailable");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn capability_object_precedes_discovery_and_survives_recovery_without_purchase() {
    let intake=Intake::start().await;
    let person=Person::new();
    let request=json!({"request_id":Uuid::new_v4(),"question":"Jaarrekening van Coolblue","state":null});
    let priced=intake.chat(&person,request.clone()).await;
    assert_eq!(priced["status"],"ready","{priced}");
    assert_eq!(*intake.model.stages.lock().unwrap(), vec!["parse","search","bazaar","plan"]);
    let object=&priced["context_view"]["capability_intake"];
    assert_eq!(object["parsed_request"]["subjects"][0]["label"],"Coolblue");
    assert_eq!(object["parsed_request"]["capabilities"][0]["slug"],"company.financial.statements");
    assert_eq!(object["matches"][0]["apiosk"][0]["origin"],"apiosk");
    assert!(intake.model.chats()[0]["parsed_request"].is_object());
    let replay=intake.chat(&person,request).await;
    assert_eq!(replay["state"],priced["state"]);
    assert_eq!(intake.model.stages.lock().unwrap().len(),4);
    let id=priced["state"]["state_ref"].as_str().unwrap();
    let (status,recovered)=intake.send(&person.bearer,&format!("/v2/tasks/{id}"),None).await;
    assert_eq!(status,StatusCode::OK);
    assert_eq!(recovered["context_view"]["capability_intake"],*object);
    let calls:i64=sqlx::query_scalar("SELECT count(*) FROM executions WHERE task_id=$1").bind(id.parse::<Uuid>().unwrap()).fetch_one(&intake.pool).await.unwrap();
    assert_eq!(calls,0,"discovery and recovery must never buy source data");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn clarification_continues_the_saved_question_without_losing_its_subject() {
    let intake = Intake::start().await;
    let person = Person::new();
    let initial = intake.chat(&person, json!({"request_id":Uuid::new_v4(),"question":"Controleer Coolblue","state":null})).await;
    assert_eq!(initial["status"], "needs_input", "{initial}");
    assert!(initial["errors"].as_array().unwrap().is_empty());
    assert_eq!(initial["context_view"]["capability_intake"]["parsed_request"]["subjects"][0]["label"], "Coolblue");
    let followup = intake.chat(&person, json!({"request_id":Uuid::new_v4(),"question":"De jaarrekening","state":initial["state"]})).await;
    assert_eq!(followup["status"], "ready", "{followup}");
    assert_eq!(followup["state"]["state_ref"], initial["state"]["state_ref"]);
    let object = &followup["context_view"]["capability_intake"]["parsed_request"];
    assert_eq!(object["subjects"][0]["label"], "Coolblue");
    assert_eq!(object["meta"]["text_sha256"], { use sha2::Digest; hex::encode(sha2::Sha256::digest("De jaarrekening")) });
    let id: Uuid = followup["state"]["state_ref"].as_str().unwrap().parse().unwrap();
    let calls: i64 = sqlx::query_scalar("SELECT count(*) FROM executions WHERE task_id=$1")
        .bind(id).fetch_one(&intake.pool).await.unwrap();
    assert_eq!(calls, 0, "clarification and quoting do not execute a source");
}

fn recovery_delta(known_company_id: bool) -> Value {
    json!({"explicit_values":[],"requested_focus_refs":[],"selection":selection(json!([
        {"endpoint_id":endpoint("company.search"),"inputs":[]},
        {"endpoint_id":endpoint("company.accounts"),"inputs":if known_company_id {
            json!([{"fact_type":"company.id","subject_role":"subject","value":"12345678"}])
        } else { json!([]) }}
    ]))})
}

fn paused_task(person: &Person, response: &Value, stage: &str, question: &str, delta: &Value) -> Uuid {
    assert_eq!(response["status"], "failed", "{response}");
    assert!(response["proposal"].is_null(), "paused interpretation cannot expose an old quote");
    assert_eq!(response["next_actions"], json!([]), "paused interpretation cannot expose executable actions");
    assert_eq!(response["errors"], json!([]), "the structured recovery explains the pause");
    let recovery = &response["context_view"]["intake_recovery"];
    assert_eq!(recovery["stage"], stage);
    assert_eq!(recovery["question"], question);
    assert_eq!(&recovery["context_delta"], delta);
    let signed: apiosk_gateway_v2::state::Envelope = serde_json::from_value(response["state"].clone()).unwrap();
    Signer::new(vec![7; 32]).unwrap().verify(person.owner, &signed, false).unwrap();
    signed.state_ref
}

async fn assert_no_purchase(intake: &Intake, person: &Person, task: Uuid) {
    let executions: i64 = sqlx::query_scalar("SELECT count(*) FROM executions WHERE task_id=$1")
        .bind(task).fetch_one(&intake.pool).await.unwrap();
    let approvals: i64 = sqlx::query_scalar("SELECT count(*) FROM authorizations WHERE task_id=$1")
        .bind(task).fetch_one(&intake.pool).await.unwrap();
    let charges: i64 = sqlx::query_scalar("SELECT count(*) FROM public.balance_charges WHERE user_id=$1")
        .bind(person.owner).fetch_one(&intake.pool).await.unwrap();
    assert_eq!((executions, approvals, charges), (0, 0, 0), "recovery cannot buy, approve, or charge source data");
}

/// Both interpretation stages before planning must survive malformed model
/// output without losing the submitted text or creating an executable task.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn intake_failures_save_scoped_recovery_and_replay_without_model_calls() {
    let intake = Intake::start().await;
    for stage in ["parse", "search"] {
        let person = Person::new();
        let question = "Jaarrekening van Coolblue";
        let delta = recovery_delta(false);
        intake.model.fail_next(stage, 2);
        let request = json!({"request_id":Uuid::new_v4(),"question":question,"state":null,"context_delta":delta});
        let failed = intake.chat(&person, request.clone()).await;
        let task = paused_task(&person, &failed, "parsing", question, &delta);
        assert!(failed["context_view"]["capability_intake"].is_null());
        assert_eq!(failed["context_view"]["conversation"].as_array().unwrap().len(), 1);
        let calls = intake.model.stages.lock().unwrap().clone();

        assert_eq!(intake.chat(&person, request).await, failed, "same request id replays the persisted failure");
        assert_eq!(*intake.model.stages.lock().unwrap(), calls, "replay must not interpret again");
        let (status, recovered) = intake.send(&person.bearer, &format!("/v2/tasks/{task}"), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(recovered["context_view"]["intake_recovery"], failed["context_view"]["intake_recovery"]);
        assert_eq!(paused_task(&person, &recovered, "parsing", question, &delta), task);
        let stored: Value = sqlx::query_scalar("SELECT document->'intake_recovery' FROM tasks WHERE id=$1")
            .bind(task).fetch_one(&intake.pool).await.unwrap();
        assert_eq!(stored["question"], question);
        assert_eq!(stored["context_delta"], delta);
        assert!(stored["created_at"].is_string());

        let other = Person::new();
        let (status, denied) = intake.send(&other.bearer, &format!("/v2/tasks/{task}"), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "another account cannot read a saved prompt: {denied}");
        assert!(!denied.to_string().contains("Coolblue"));
        assert_no_purchase(&intake, &person, task).await;

        let parse_calls = intake.model.count("parse");
        let resumed = intake.chat(&person, json!({"request_id":Uuid::new_v4(),"question":question,
            "state":recovered["state"],"context_delta":recovered["context_view"]["intake_recovery"]["context_delta"]})).await;
        assert_eq!(resumed["status"], "ready", "{resumed}");
        assert_eq!(resumed["state"]["state_ref"], json!(task));
        assert!(resumed["context_view"]["intake_recovery"].is_null());
        assert_eq!(intake.model.count("parse"), parse_calls + 1, "an incomplete parse/search restarts extraction");
        assert_eq!(resumed["context_view"]["conversation"].as_array().unwrap().len(), 1, "retry replaces the paused turn");
        assert_no_purchase(&intake, &person, task).await;
    }
}

/// A recent validated object survives a planner outage. Resume skips repeated
/// extraction/discovery, creates a price, and still needs explicit approval.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn planning_recovery_reuses_discovery_and_requires_a_fresh_explicit_approval() {
    let intake = Intake::start().await;
    let person = Person::new();
    let question = "Jaarrekening van Coolblue";
    let delta = recovery_delta(true);
    intake.model.fail_next("plan", 2);
    let failed = intake.chat(&person, json!({"request_id":Uuid::new_v4(),"question":question,"state":null,"context_delta":delta})).await;
    let task = paused_task(&person, &failed, "planning", question, &delta);
    let parsed = failed["context_view"]["capability_intake"].clone();
    assert_eq!(parsed["parsed_request"]["subjects"][0]["label"], "Coolblue");
    assert!(!parsed["matches"][0]["apiosk"].as_array().unwrap().is_empty());
    assert_eq!((intake.model.count("parse"), intake.model.count("search"), intake.model.count("plan")), (1, 1, 2));
    let (status, recovered) = intake.send(&person.bearer, &format!("/v2/tasks/{task}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(recovered["context_view"]["capability_intake"], parsed);
    assert_eq!(recovered["context_view"]["intake_recovery"]["context_delta"], delta);
    assert_no_purchase(&intake, &person, task).await;

    let request = json!({"request_id":Uuid::new_v4(),"question":question,"state":recovered["state"],"context_delta":delta});
    let resumed = intake.chat(&person, request.clone()).await;
    assert_eq!(resumed["status"], "ready", "{resumed}");
    assert_eq!(resumed["state"]["state_ref"], json!(task));
    assert_eq!(resumed["context_view"]["capability_intake"], parsed, "the exact validated object is reused");
    assert!(resumed["context_view"]["intake_recovery"].is_null());
    assert_eq!(resumed["context_view"]["conversation"].as_array().unwrap().len(), 1);
    assert_eq!(resumed["context_view"]["conversation"][0]["question"], question);
    assert_eq!((intake.model.count("parse"), intake.model.count("search"), intake.model.count("plan")), (1, 1, 3));
    assert_eq!(resumed["proposal"]["max_total_atomic"], "115");
    assert_eq!(resumed["proposal"]["steps"], json!(["company.accounts"]));
    assert_eq!(intake.chat(&person, request).await, resumed, "successful resume also has an idempotent receipt");
    assert_eq!(intake.model.count("plan"), 3);

    let action = resumed["next_actions"].as_array().unwrap().iter()
        .find(|action| action["kind"] == "execute_quoted_step").unwrap();
    assert_eq!(action["requires_authorization"], true);
    let (status, blocked) = intake.send(&person.bearer, "/v2/execute", Some(json!({"request_id":Uuid::new_v4(),
        "action_id":action["action_id"],"state":resumed["state"],"quote_ref":resumed["proposal"]["quote_ref"],
        "authorization_ref":null,"idempotency_key":action["action_id"],"input":null}))).await;
    assert_eq!(status, StatusCode::OK, "{blocked}");
    assert_eq!(blocked["status"], "requires_approval", "retry must not inherit or create approval: {blocked}");
    assert_no_purchase(&intake, &person, task).await;
}

/// Editing either the request or source/input selection invalidates discovery
/// reuse, even if the task's previous validated object is only seconds old.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn recovery_reparses_changed_questions_and_selections() {
    let intake = Intake::start().await;
    for changed in ["question", "selection"] {
        let person = Person::new();
        let question = "Jaarrekening van Coolblue";
        let delta = recovery_delta(false);
        intake.model.fail_next("plan", 2);
        let failed = intake.chat(&person, json!({"request_id":Uuid::new_v4(),"question":question,"state":null,"context_delta":delta})).await;
        let task = paused_task(&person, &failed, "planning", question, &delta);
        let before = (intake.model.count("parse"), intake.model.count("search"));
        let new_question = if changed == "question" { "Jaarrekening van Example" } else { question };
        let new_delta = if changed == "selection" { recovery_delta(true) } else { delta };
        let resumed = intake.chat(&person, json!({"request_id":Uuid::new_v4(),"question":new_question,
            "state":failed["state"],"context_delta":new_delta})).await;
        assert_eq!(resumed["status"], "ready", "{changed}: {resumed}");
        assert_eq!(resumed["state"]["state_ref"], json!(task));
        assert_eq!((intake.model.count("parse"), intake.model.count("search")), (before.0 + 1, before.1 + 1), "changed {changed} must rerun extraction and matching");
        assert_eq!(resumed["context_view"]["capability_intake"]["parsed_request"]["subjects"][0]["label"],
            if changed == "question" { "Example" } else { "Coolblue" });
        assert_no_purchase(&intake, &person, task).await;
    }
}

/// A workflow's collected fields belong to its saved task, including while
/// extraction or the dedicated workflow conversation model is unavailable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn workflow_recovery_preserves_collected_input_across_model_outages() {
    let intake = Intake::start().await;
    for failure_stage in ["parse", "workflow"] {
        let person = Person::new();
        let initial = intake.chat(&person, json!({"request_id":Uuid::new_v4(),"question":"Zoek klanten in Amsterdam",
            "state":null,"context_delta":{"workflow_slug":"prospect-shortlist"}})).await;
        assert_eq!(initial["status"], "needs_input", "{initial}");
        let task: Uuid = initial["state"]["state_ref"].as_str().unwrap().parse().unwrap();
        let prior: Value = sqlx::query_scalar("SELECT document->'workflow' FROM tasks WHERE id=$1")
            .bind(task).fetch_one(&intake.pool).await.unwrap();
        assert_eq!(prior["input"]["location"], "Amsterdam");
        let first_turn = initial["context_view"]["conversation"][0].clone();

        intake.model.fail_next(failure_stage, 2);
        let delta = json!({"explicit_values":[],"requested_focus_refs":[]});
        let failed = intake.chat(&person, json!({"request_id":Uuid::new_v4(),"question":"software",
            "state":initial["state"],"context_delta":delta})).await;
        assert_eq!(paused_task(&person, &failed, if failure_stage == "parse" { "parsing" } else { "planning" }, "software", &delta), task);
        let saved: Value = sqlx::query_scalar("SELECT document->'workflow' FROM tasks WHERE id=$1")
            .bind(task).fetch_one(&intake.pool).await.unwrap();
        assert_eq!(saved["input"], prior["input"], "{failure_stage} outage cannot erase collected inputs");
        assert_eq!(saved["collecting"], true);
        assert_eq!(failed["context_view"]["conversation"][0], first_turn);
        assert_eq!(failed["context_view"]["conversation"].as_array().unwrap().len(), 2);
        let parses = intake.model.count("parse");
        let resumed = intake.chat(&person, json!({"request_id":Uuid::new_v4(),"question":"software",
            "state":failed["state"],"context_delta":delta})).await;
        assert_eq!(resumed["status"], "needs_input", "{failure_stage}: {resumed}");
        assert!(resumed["context_view"]["intake_recovery"].is_null());
        assert_eq!(resumed["context_view"]["conversation"][0], first_turn);
        assert_eq!(resumed["context_view"]["conversation"].as_array().unwrap().len(), 2);
        assert_eq!(intake.model.count("parse"), parses + usize::from(failure_stage == "parse"));
        let continued: Value = sqlx::query_scalar("SELECT document->'workflow' FROM tasks WHERE id=$1")
            .bind(task).fetch_one(&intake.pool).await.unwrap();
        assert_eq!(continued["input"], json!({"location":"Amsterdam","topic":"software"}));
        let workflow_calls = intake.model.workflow_chats.lock().unwrap();
        assert_eq!(workflow_calls.last().unwrap()["collected"]["location"], "Amsterdam", "resume passes prior inputs to the workflow model");
        drop(workflow_calls);
        assert_no_purchase(&intake, &person, task).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn retrying_an_identical_question_preserves_the_prior_clarification_turn() {
    let intake = Intake::start().await;
    let person = Person::new();
    let question = "Controleer Coolblue";
    let initial = intake.chat(&person, json!({"request_id":Uuid::new_v4(),"question":question,"state":null})).await;
    assert_eq!(initial["status"], "needs_input", "{initial}");
    let first_turn = initial["context_view"]["conversation"][0].clone();
    assert!(first_turn["reply"].is_string());
    intake.model.fail_next("plan", 2);
    let delta = json!({"explicit_values":[],"requested_focus_refs":[]});
    let failed = intake.chat(&person, json!({"request_id":Uuid::new_v4(),"question":question,
        "state":initial["state"],"context_delta":delta})).await;
    let task = paused_task(&person, &failed, "planning", question, &delta);
    assert_eq!(failed["context_view"]["conversation"].as_array().unwrap().len(), 2);
    assert_eq!(failed["context_view"]["conversation"][0], first_turn);
    assert!(failed["context_view"]["conversation"][1]["reply"].is_null());
    let resumed = intake.chat(&person, json!({"request_id":Uuid::new_v4(),"question":question,
        "state":failed["state"],"context_delta":delta})).await;
    assert_eq!(resumed["status"], "needs_input", "{resumed}");
    assert_eq!(resumed["context_view"]["conversation"].as_array().unwrap().len(), 2);
    assert_eq!(resumed["context_view"]["conversation"][0], first_turn, "resume removes only its own pending turn");
    assert_eq!(resumed["context_view"]["conversation"][1]["question"], question);
    assert!(resumed["context_view"]["conversation"][1]["reply"].is_string());
    assert_no_purchase(&intake, &person, task).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn planner_outage_clears_old_price_and_resume_creates_a_new_quote() {
    let intake = Intake::start().await;
    let person = Person::new();
    let prior = intake.chat(&person, json!({"request_id":Uuid::new_v4(),"question":"Jaarrekening van Example","state":null})).await;
    assert_eq!(prior["status"], "ready", "{prior}");
    let question = "Jaarrekening van Coolblue";
    let delta = recovery_delta(true);
    intake.model.fail_next("plan", 2);
    let failed = intake.chat(&person, json!({"request_id":Uuid::new_v4(),"question":question,
        "state":prior["state"],"context_delta":delta})).await;
    let task = paused_task(&person, &failed, "planning", question, &delta);
    assert_eq!(failed["context_view"]["conversation"][0]["output"]["proposal"]["quote_ref"], prior["proposal"]["quote_ref"], "old price remains read-only conversation history");
    let resumed = intake.chat(&person, json!({"request_id":Uuid::new_v4(),"question":question,
        "state":failed["state"],"context_delta":delta})).await;
    assert_eq!(resumed["status"], "ready", "{resumed}");
    assert_ne!(resumed["proposal"]["quote_ref"], prior["proposal"]["quote_ref"], "resume never revives an old quote");
    assert_eq!(resumed["proposal"]["max_total_atomic"], "115");
    assert_eq!(resumed["context_view"]["conversation"].as_array().unwrap().len(), 2);
    assert_no_purchase(&intake, &person, task).await;
}

/// Editing a paused workflow starter keeps its workflow selection on the same
/// task; recovery cannot be used to switch that task to a different workflow.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn edited_workflow_starter_recovers_only_its_original_workflow() {
    let intake = Intake::start().await;
    let other_workflow = apiosk_gateway_v2::workflow_catalog::recipes().into_iter()
        .find(|recipe| recipe.slug != "prospect-shortlist").unwrap().slug;
    for failure_stage in ["parse", "workflow"] {
        let person = Person::new();
        let question = "Zoek klanten in Amsterdam";
        let delta = json!({"workflow_slug":"prospect-shortlist","explicit_values":[],"requested_focus_refs":[]});
        intake.model.fail_next(failure_stage, 2);
        let failed = intake.chat(&person, json!({"request_id":Uuid::new_v4(),"question":question,
            "state":null,"context_delta":delta})).await;
        let task = paused_task(&person, &failed, if failure_stage == "parse" { "parsing" } else { "planning" }, question, &delta);
        let edited_question = "Zoek klanten in Amsterdam software";

        let wrong_workflow = intake.chat(&person, json!({"request_id":Uuid::new_v4(),"question":edited_question,
            "state":failed["state"],"context_delta":{"workflow_slug":other_workflow}})).await;
        assert_eq!(wrong_workflow["status"], "failed", "{wrong_workflow}");
        assert_eq!(codes(&wrong_workflow), ["operation_failed"]);
        assert!(wrong_workflow["errors"][0]["message"].as_str().unwrap().contains("Start a workflow in a new App chat"), "a different workflow remains disallowed: {wrong_workflow}");
        let (status, still_paused) = intake.send(&person.bearer, &format!("/v2/tasks/{task}"), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(still_paused["context_view"]["intake_recovery"], failed["context_view"]["intake_recovery"], "rejected switching cannot replace the saved starter");

        let parses = intake.model.count("parse");
        let edited = intake.chat(&person, json!({"request_id":Uuid::new_v4(),"question":edited_question,
            "state":still_paused["state"],"context_delta":delta})).await;
        assert_eq!(edited["status"], "needs_input", "an edited starter continues its own workflow: {failure_stage}: {edited}");
        assert_eq!(edited["errors"], json!([]));
        assert_eq!(edited["state"]["state_ref"], json!(task));
        assert!(edited["context_view"]["intake_recovery"].is_null());
        assert_eq!(edited["context_view"]["workflow"]["slug"], "prospect-shortlist");
        assert_eq!(intake.model.count("parse"), parses + 1, "edited text must be parsed again");
        let stored: Value = sqlx::query_scalar("SELECT document->'workflow' FROM tasks WHERE id=$1")
            .bind(task).fetch_one(&intake.pool).await.unwrap();
        assert_eq!(stored["input"], json!({"location":"Amsterdam","topic":"software"}));
        assert_no_purchase(&intake, &person, task).await;
    }
}
