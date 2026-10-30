//! Real HTTP + PostgreSQL integration test. Never connects to a live database.
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
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use sqlx::postgres::PgPoolOptions;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Semaphore;
use uuid::Uuid;

const OWNER: &str = "10000000-0000-4000-8000-000000000001";
const OTHER: &str = "10000000-0000-4000-8000-000000000002";
const MESSAGE: &str = "New supplier: **Acme B.V.**\nKVK 12345678 · IBAN NL91 ABNA 0417 1643 00";

async fn auth(headers: HeaderMap) -> Response {
    match headers.get("authorization").and_then(|v| v.to_str().ok()) {
        Some("Bearer owner") => Json(json!({"id":OWNER})).into_response(),
        Some("Bearer other") => Json(json!({"id":OTHER})).into_response(),
        _ => StatusCode::UNAUTHORIZED.into_response(),
    }
}
async fn account(Path(id): Path<String>) -> Json<Value> {
    Json(
        json!({"id":id,"user_id":format!("apiosk_user_{}",if id=="ca_owned"{OWNER}else{OTHER}),"status":"ACTIVE","toolkit":{"slug":"slack"}}),
    )
}
async fn tool(Path(tool): Path<String>, Json(body): Json<Value>) -> Json<Value> {
    assert_eq!(body["connected_account_id"], "ca_owned");
    let data = match tool.as_str() {
        "SLACK_LIST_CONVERSATIONS" => {
            json!({"ok":true,"channels":[{"id":"C123","name":"suppliers"}],"response_metadata":{"next_cursor":""}})
        }
        "SLACK_LIST_ALL_USERS" => {
            json!({"ok":true,"members":[{"id":"U123","real_name":"Alex"},{"id":"U456","real_name":"Sam"},{"id":"UBOT","is_bot":true},{"id":"UOLD","deleted":true}],"response_metadata":{"next_cursor":""}})
        }
        "SLACK_RETRIEVE_CONVERSATION_INFORMATION" => {
            json!({"ok":true,"channel":{"id":"C123","name":"suppliers"}})
        }
        "SLACK_CREATE_CHANNEL" => {
            assert_eq!(body["arguments"]["is_private"], true);
            json!({"ok":true,"channel":{"id":"CNEW","name":"suppliers-test"}})
        }
        "SLACK_OPEN_DM" => {
            assert_eq!(body["arguments"]["users"], "U123,U456");
            json!({"ok":true,"channel":{"id":"G123","name":"test-group"}})
        }
        _ => panic!("unexpected tool {tool}"),
    };
    Json(json!({"successful":true,"data":data}))
}
async fn subscribe(
    State(calls): State<Arc<Mutex<Vec<Value>>>>,
    Json(body): Json<Value>,
) -> Json<Value> {
    assert_eq!(body["user_id"], format!("apiosk_user_{OWNER}"));
    assert_eq!(body["connected_account_id"], "ca_owned");
    assert_eq!(body["trigger_config"]["channel_id"], "C123");
    calls.lock().unwrap().push(body);
    Json(json!({"trigger_id":"ti_fixture"}))
}
async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}
async fn value(response: reqwest::Response) -> Value {
    let status = response.status();
    let body = response.text().await.unwrap();
    assert!(status.is_success(), "{status}: {body}");
    serde_json::from_str(&body).unwrap()
}

#[tokio::test]
#[ignore = "requires an isolated local TRIGGERS_TEST_DATABASE_URL"]
async fn trigger_ingress_ownership_deduplication_and_live_delivery() {
    let database =
        std::env::var("TRIGGERS_TEST_DATABASE_URL").expect("isolated local database required");
    let url = url::Url::parse(&database).unwrap();
    assert!(
        matches!(url.host_str(), Some("127.0.0.1") | Some("localhost"))
            && url.path().starts_with("/triggers_test"),
        "test databases only"
    );
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&database)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE SCHEMA IF NOT EXISTS auth; CREATE TABLE IF NOT EXISTS auth.users(id uuid PRIMARY KEY); CREATE SCHEMA IF NOT EXISTS gateway_v2; DO $$ BEGIN IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname='gateway_v2_runtime') THEN CREATE ROLE gateway_v2_runtime; END IF; IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname='anon') THEN CREATE ROLE anon; END IF; IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname='authenticated') THEN CREATE ROLE authenticated; END IF; END $$; GRANT USAGE ON SCHEMA gateway_v2 TO gateway_v2_runtime; DROP TABLE IF EXISTS gateway_v2.inbound_trigger_events; DROP TABLE IF EXISTS gateway_v2.inbound_triggers;").execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../supabase/migrations/20261029161428_inbound_triggers.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!("../supabase/migrations/20261030094736_trigger_response_activity.sql"))
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO auth.users(id) VALUES($1),($2) ON CONFLICT DO NOTHING")
        .bind(OWNER.parse::<Uuid>().unwrap())
        .bind(OTHER.parse::<Uuid>().unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let provider=serve(Router::new().route("/auth/v1/user",get(auth))
        .route("/connected_accounts",get(||async{Json(json!({"items":[{"id":"ca_owned","user_id":format!("apiosk_user_{OWNER}"),"status":"ACTIVE","alias":"Local test Slack","toolkit":{"slug":"slack"}}]}))}))
        .route("/connected_accounts/:id",get(account))
        .route("/tools/execute/:tool",post(tool)).route("/trigger_instances/SLACK_CHANNEL_MESSAGE_RECEIVED/upsert",post(subscribe)).with_state(calls.clone())).await;
    std::env::set_var("COMPOSIO_API_KEY", "test-key");
    std::env::set_var("COMPOSIO_BASE_URL", &provider);
    std::env::set_var("COMPOSIO_WEBHOOK_SECRET", "test-secret");
    let http = reqwest::Client::new();
    // Use the production runtime role so missing grants/RLS policies fail this test.
    let runtime = PgPoolOptions::new()
        .max_connections(10)
        .after_connect(|c, _| {
            Box::pin(async move {
                sqlx::query("SET ROLE gateway_v2_runtime")
                    .execute(c)
                    .await?;
                Ok(())
            })
        })
        .connect(&database)
        .await
        .unwrap();
    let store = Store {
        pool: runtime.clone(),
    };
    let engine = Arc::new(Engine {
        store: store.clone(),
        catalog: CatalogCache::new(runtime.clone()),
        parser: Parser::new(
            http.clone(),
            format!("{provider}/unused"),
            "fixture".into(),
            "fixture".into(),
        ),
        signer: Signer::new(vec![1; 32]).unwrap(),
        executor: Executor {
            store,
            http: http.clone(),
            supabase_url: provider.clone(),
            enabled: false,
        },
        app_url: "http://localhost:5276".into(),
    });
    let base = serve(
        router(
            ServerState {
                engine,
                auth: Arc::new(Auth {
                    pool: runtime.clone(),
                    http: http.clone(),
                    supabase_url: provider.clone(),
                    publishable_key: "fixture".into(),
                }),
                permits: Arc::new(Semaphore::new(8)),
                commerce: None,
            },
            "http://localhost:5276",
        )
        .unwrap(),
    )
    .await;
    let endpoint = format!("{base}/v2/triggers");
    assert_eq!(http.get(&endpoint).send().await.unwrap().status(), 401);
    let id = Uuid::new_v4();
    let create = json!({"id":id,"source":"webhook","name":"Supplier webhook","match_prefix":"New supplier:"});
    let created = value(
        http.post(&endpoint)
            .bearer_auth("owner")
            .json(&create)
            .send()
            .await
            .unwrap(),
    )
    .await;
    let secret = created["webhook_secret"].as_str().unwrap();
    assert!(created["trigger"].get("webhook_secret_hash").is_none());
    let replay = value(
        http.post(&endpoint)
            .bearer_auth("owner")
            .json(&create)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(replay["replayed"], true);
    let incoming = format!("{endpoint}/{id}/incoming");
    let body = json!({"event_id":"external-1","text":MESSAGE});
    assert_eq!(
        http.post(&incoming)
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        http.post(&incoming)
            .bearer_auth("wrong")
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let mut sse = http
        .get(format!("{endpoint}/events?after=0"))
        .bearer_auth("owner")
        .send()
        .await
        .unwrap();
    assert_eq!(sse.status(), 200);
    let result = value(
        http.post(&incoming)
            .bearer_auth(secret)
            .json(&body)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(result["action"], "Hello world");
    let streamed = tokio::time::timeout(Duration::from_secs(4), async {
        let mut content = String::new();
        while let Some(bytes) = sse.chunk().await.unwrap() {
            content.push_str(&String::from_utf8_lossy(&bytes));
            if content.contains("Hello world") {
                return content;
            }
        }
        panic!("stream ended")
    })
    .await
    .unwrap();
    assert!(streamed.contains("event: trigger"));
    drop(sse);
    // Concurrent deliveries share the receipt, not just sequential retries.
    let (a, b) = tokio::join!(
        http.post(&incoming).bearer_auth(secret).json(&body).send(),
        http.post(&incoming).bearer_auth(secret).json(&body).send()
    );
    assert_eq!(value(a.unwrap()).await["duplicate"], true);
    assert_eq!(value(b.unwrap()).await["duplicate"], true);
    let ignored = value(
        http.post(&incoming)
            .bearer_auth(secret)
            .json(&json!({"event_id":"unmatched","text":"Good morning"}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(ignored["matched"], false);
    let own = value(
        http.get(&endpoint)
            .bearer_auth("owner")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(own["events"].as_array().unwrap().len(), 2);
    let foreign = value(
        http.get(&endpoint)
            .bearer_auth("other")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(foreign["events"], json!([]));
    assert_eq!(foreign["triggers"], json!([]));
    assert_eq!(
        http.post(format!("{endpoint}/{id}/control"))
            .bearer_auth("other")
            .json(&json!({"enabled":false}))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        http.post(format!("{endpoint}/{id}/test"))
            .bearer_auth("other")
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    value(
        http.post(format!("{endpoint}/{id}/control"))
            .bearer_auth("owner")
            .json(&json!({"enabled":false}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    let paused = value(
        http.post(&incoming)
            .bearer_auth(secret)
            .json(&json!({"event_id":"paused","text":MESSAGE}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(paused["reason"], "paused");
    value(
        http.post(format!("{endpoint}/{id}/control"))
            .bearer_auth("owner")
            .json(&json!({"enabled":true}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    let rotated = value(
        http.post(format!("{endpoint}/{id}/rotate"))
            .bearer_auth("owner")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        http.post(&incoming)
            .bearer_auth(secret)
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    value(
        http.post(&incoming)
            .bearer_auth(rotated["webhook_secret"].as_str().unwrap())
            .json(&json!({"event_id":"new-key","text":MESSAGE}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    // The exact connected account is checked before any provider mutation.
    let slack = json!({"id":Uuid::new_v4(),"source":"slack","name":"Slack suppliers","match_prefix":"New supplier:","connected_account_id":"ca_other","channel_id":"C123"});
    assert_eq!(
        http.post(&endpoint)
            .bearer_auth("owner")
            .json(&slack)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let mut slack = slack;
    slack["connected_account_id"] = json!("ca_owned");
    value(
        http.post(&endpoint)
            .bearer_auth("owner")
            .json(&slack)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(calls.lock().unwrap().len(), 1);
    value(
        http.get(format!("{endpoint}/channels?account=ca_owned"))
            .bearer_auth("owner")
            .send()
            .await
            .unwrap(),
    )
    .await;
    let members = value(
        http.get(format!("{endpoint}/members?account=ca_owned"))
            .bearer_auth("owner")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(members["members"].as_array().unwrap().len(), 2);
    assert_eq!(members["members"][0]["name"], "Alex");
    for users in [json!([]), json!(["U123", "U456"])] {
        value(
            http.post(format!("{endpoint}/channels"))
                .bearer_auth("owner")
                .json(&json!({"account":"ca_owned","name":"suppliers-test","users":users}))
                .send()
                .await
                .unwrap(),
        )
        .await;
    }
    let event = json!({"id":"evt-test","type":"composio.trigger.message","metadata":{"trigger_id":"ti_fixture","trigger_slug":"SLACK_CHANNEL_MESSAGE_RECEIVED","connected_account_id":"ca_owned","user_id":format!("apiosk_user_{OWNER}")},"data":{"text":MESSAGE,"channel":"C123","ts":"123.456","user":"U123"}});
    let send = |event: Value| {
        let raw = event.to_string();
        let ts = chrono::Utc::now().timestamp().to_string();
        let mut mac = Hmac::<Sha256>::new_from_slice(b"test-secret").unwrap();
        mac.update(format!("delivery-1.{ts}.{raw}").as_bytes());
        http.post(format!("{endpoint}/composio"))
            .header("webhook-id", "delivery-1")
            .header("webhook-timestamp", ts)
            .header(
                "webhook-signature",
                format!("v1,{}", STANDARD.encode(mac.finalize().into_bytes())),
            )
            .body(raw)
            .send()
    };
    assert_eq!(
        http.post(format!("{endpoint}/composio"))
            .json(&event)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        value(send(event.clone()).await.unwrap()).await["accepted"],
        true
    );
    value(send(event.clone()).await.unwrap()).await;
    let mut wrong = event.clone();
    wrong["data"]["channel"] = json!("CFOREIGN");
    assert_eq!(value(send(wrong).await.unwrap()).await["accepted"], false);
    let mut wrong = event.clone();
    wrong["metadata"]["user_id"] = json!(format!("apiosk_user_{OTHER}"));
    assert_eq!(value(send(wrong).await.unwrap()).await["accepted"], false);
    let mut bot = event;
    bot["data"]["bot_id"] = json!("B123");
    assert_eq!(
        value(send(bot).await.unwrap()).await["reason"],
        "bot_or_message_update"
    );
    let final_state = value(
        http.get(&endpoint)
            .bearer_auth("owner")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(final_state["events"].as_array().unwrap().len(), 4);
    // Verify public Data API roles cannot read the event text or secret hashes.
    for role in ["anon", "authenticated"] {
        let allowed:bool=sqlx::query_scalar("SELECT has_table_privilege($1,'gateway_v2.inbound_trigger_events','SELECT') OR has_table_privilege($1,'gateway_v2.inbound_triggers','SELECT')").bind(role).fetch_one(&pool).await.unwrap();
        assert!(!allowed);
    }
    if std::env::var("TRIGGERS_PREVIEW").as_deref() == Ok("true") {
        println!("TRIGGERS_PREVIEW_URL={base}");
        std::future::pending::<()>().await;
    }
}
