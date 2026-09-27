//! Intake through the real router on a fresh PGlite with a fake model that
//! records every interpretation: one length definition, structured endpoint
//! selection, compiled lookups, request receipts and requotes. Run by
//! scripts/test-intake-flow.mjs (PGTEST_PORT, default 54360), one scenario at
//! a time on the one PGlite connection.

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
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::{sync::{Arc, Mutex}, time::Duration};
use uuid::Uuid;

/// The organisation key seeded by tests/support-db.mjs.
const ORGANISATION_KEY: &str = "apk_org_live_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// Every interpretation the model was asked for, as the user JSON it received.
#[derive(Default)]
struct Model {
    ask_v2_requests: Mutex<Vec<Value>>,
    chats: Mutex<Vec<Value>>,
    workflow_chats: Mutex<Vec<Value>>,
    stages: Mutex<Vec<&'static str>>,
    failures: Mutex<std::collections::BTreeMap<&'static str, usize>>,
}

fn ask_v2_capabilities() -> Value {
    json!({"language":"nl","subjects":[{"id":"s1","label":"Coolblue","type":"company","identifiers":{}}],
        "capabilities":[{"slug":"company.financial.statements","name":"Company financial statements","description":"Retrieve company annual accounts.","domain":"company","inputs":[],"outputs":["financial_statements"],"subject_id":"s1","source_hint":null,"confidence":0.95}],
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
    let continued = question == "De jaarrekening" && input["context"].to_string().contains("Coolblue");
    let content = match question.strip_prefix("Jaarrekening van ").or(continued.then_some("Coolblue")) {
        Some(label) => {
            let label = label.rsplit(' ').next().unwrap();
            json!({"message":"Ik zoek de jaarrekening op en toon eerst de prijs.","discover":true,"intent":{
                "subjects":[if input.get("bindings").is_some() {json!({"subject_ref":"s1","values":{"company.name":{"ref":"s1.label"}}})} else {json!({"entity_ref":null,"label":label,"values":{"company.name":label}})}],
                "goals":[{"capability":"company.accounts","subject":0,"constraints":{}}],"answer_operations":[],"unresolved_references":[]}})
        }
        None => json!({"message":if question.starts_with("Controleer ") {"Welke gegevens wil je controleren?"} else {"Waar kan ik je mee helpen?"},"discover":false,"intent":null}),
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
        let mock = Router::new()
            .route("/chat", post(interpret))
            .route("/ask-v2-model", post(ask_v2_model))
            .route("/bazaar", get(|State(model): State<Arc<Model>>| async move {
                model.stages.lock().unwrap().push("bazaar");
                Json(json!({"resources":[]}))
            }))
            .route("/auth/v1/user", get(user))
            .with_state(model.clone());
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
                    .with_local_endpoint(format!("{base}/ask-v2-model")).unwrap()),
            signer: Signer::new(vec![7; 32]).unwrap(),
            executor: Executor { store, http: http.clone(), supabase_url: base.clone(), enabled: true },
            app_url: "http://localhost:5173".into(),
        });
        let app = router(ServerState {
            engine,
            auth: Arc::new(Auth { pool: pool.clone(), http: http.clone(), supabase_url: base, publishable_key: "fixture".into() }),
            permits: Arc::new(tokio::sync::Semaphore::new(16)),
            executor_compat: None,
        }, "http://localhost:5173").unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gateway = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { pool, model, gateway, http }
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

    async fn chat(&self, person: &Person, body: Value) -> Value {
        let (status, response) = self.send(&person.bearer, "/v2/chat", Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{response}");
        response
    }
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
    assert_eq!(calls[0]["response_format"]["type"], "json_schema");
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
async fn ask_v2_authenticates_app_callers_before_parse_or_search() {
    let intake = Intake::start().await;
    let parse = json!({"request_id":Uuid::new_v4(),"question":"Jaarrekening van Coolblue"});
    let parsed = apiosk_gateway_v2::capability_intake::normalize(&ask_v2_capabilities().to_string(), "Jaarrekening van Coolblue", "fixture").unwrap();
    let search = json!({"parsed_request":parsed,"origin":"apiosk"});
    for (path, body) in [("/v2/ask-v2/parse", parse.clone()), ("/v2/ask-v2/search", search)] {
        let response = intake.http.post(format!("{}{path}",intake.gateway)).json(&body).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "anonymous {path}");
        let (status, response) = intake.send("invalid-fixture-bearer", path, Some(body.clone())).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{response}");
        let (status, response) = intake.send(ORGANISATION_KEY, path, Some(body)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "App-only {path}: {response}");
    }
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
    let public = ask(full.clone()).await.unwrap();
    assert_ne!(public.status(), StatusCode::BAD_REQUEST, "the public API counts the same way");
    assert_eq!(intake.model.chats().len(), 2);
    let public = ask(format!("{full}?")).await.unwrap();
    assert_eq!(public.status(), StatusCode::BAD_REQUEST);
    assert_eq!(public.json::<Value>().await.unwrap()["error"]["code"], "invalid_request");
    assert_eq!(intake.model.chats().len(), 2);
    let owned: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks WHERE owner_id=$1").bind(person.owner).fetch_one(&intake.pool).await.unwrap();
    assert_eq!(owned, 1, "only the accepted App question created a task");
    println!("intake_counts_characters_not_bytes: 4000 characters accepted on /v2/chat and /v2/ask, 4001 refused");
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

    // An old gateway rejects the unknown field before claiming the request:
    // a 422 without a JSON body, which the App answers with the legacy shape.
    let raw = intake.http.post(format!("{}/v2/chat", intake.gateway)).bearer_auth(&person.bearer)
        .json(&json!({"request_id":Uuid::new_v4(),"question":"Hallo","state":null,"context_delta":{"selection":{"version":1,"endpoints":[],"unexpected":true}}}))
        .send().await.unwrap();
    assert_eq!(raw.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(serde_json::from_str::<Value>(&raw.text().await.unwrap()).is_err());
    assert_eq!(task_count(&intake, &person).await, 0);
    println!("an_invalid_selection_is_refused_before_interpretation: no task, no model call");
}

/// A first request's receipt is recoverable by request id alone: pending
/// while claimed, completed once answered (with the task it produced),
/// unknown for an id nothing ever claimed, and unavailable to another person.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn request_receipt_recovers_a_first_request_by_id() {
    let intake = Intake::start().await;
    let person = Person::new();
    let request_id = Uuid::new_v4();
    let (status, unknown) = intake.send(&person.bearer, &format!("/v2/requests/{request_id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{unknown}");
    assert_eq!(codes(&unknown), ["request_unknown"]);

    let answered = intake.chat(&person, json!({"request_id":request_id,"question":"Hallo","state":null})).await;
    assert_eq!(answered["status"], "needs_input", "{answered}");
    let task: Uuid = answered["state"]["state_ref"].as_str().unwrap().parse().unwrap();

    let (status, receipt) = intake.send(&person.bearer, &format!("/v2/requests/{request_id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    assert_eq!(receipt["state"], "completed");
    assert_eq!(receipt["kind"], "chat");
    assert_eq!(receipt["task_ref"], json!(task));
    assert_eq!(receipt["response_status"], "needs_input");
    assert!(receipt["retry_after_ms"].is_null());

    let other = Person::new();
    let (status, denied) = intake.send(&other.bearer, &format!("/v2/requests/{request_id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "another person cannot read this receipt: {denied}");
    println!("request_receipt_recovers_a_first_request_by_id: unknown, then completed with its task, scoped to the owner");
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
