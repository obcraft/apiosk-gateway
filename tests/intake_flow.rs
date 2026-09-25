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
    chats: Mutex<Vec<Value>>,
}
impl Model {
    fn chats(&self) -> Vec<Value> {
        self.chats.lock().unwrap().clone()
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
    model.chats.lock().unwrap().push(input.clone());
    let question = input["question"].as_str().unwrap();
    let content = match question.strip_prefix("Jaarrekening van ") {
        Some(label) => {
            let label = label.rsplit(' ').next().unwrap();
            json!({"message":"Ik zoek de jaarrekening op en toon eerst de prijs.","discover":true,"intent":{
                "subjects":[{"entity_ref":null,"label":label,"values":{"company.name":label}}],
                "goals":[{"capability":"company.accounts","subject":0,"constraints":{}}],"answer_operations":[],"unresolved_references":[]}})
        }
        None => json!({"message":"Waar kan ik je mee helpen?","discover":false,"intent":null}),
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
            parser: Parser::new(http.clone(), format!("{base}/chat"), "fixture".into(), "fixture".into()),
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
