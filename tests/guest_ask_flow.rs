//! Isolated guest discovery -> login -> exact-quote approval -> paid result.
//! Model, identity and payment endpoints are local fixtures; no live services.
use apiosk_gateway_v2::{auth::Auth, catalog::CatalogCache, engine::Engine, execution::Executor,
    parser::Parser, server::{router, ServerState}, state::Signer, store::Store};
use axum::{extract::State, http::{HeaderMap, StatusCode}, routing::{get, post}, Json, Router};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::{sync::{Arc, atomic::{AtomicUsize, Ordering}}, time::Duration};
use tokio::sync::Semaphore;
use uuid::Uuid;

const OWNER: &str = "00000000-0000-4000-8000-000000000001";
struct Fixture { pool: PgPool, calls: AtomicUsize }

async fn parse() -> ([(axum::http::HeaderName, &'static str);1], String) {
    let parsed = json!({"language":"en","subjects":[{"id":"s1","type":"company","label":"Example","identifiers":{}}],
        "capabilities":[{"slug":"company.search","name":"Company search","description":"Search companies by name","domain":"company",
        "inputs":["company_name"],"outputs":["company_id"],"subject_id":"s1","source_hint":"fixture","confidence":1}],
        "deliverable":{"format":"chat","operations":[]}});
    let event = json!({"choices":[{"delta":{"content":parsed.to_string()}}]});
    ([(axum::http::header::CONTENT_TYPE,"text/event-stream")],format!("data: {event}\n\ndata: [DONE]\n\n"))
}

async fn model(Json(body): Json<Value>) -> Json<Value> {
    let input: Value = serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    let reply = if input.get("answer_operations").is_some() {
        let source = &input["sources"][0];
        json!({"observations":[{"text":"The source reports {{a}}.","evidence":[{"result_ref":source["result_ref"],"pointer":"/data/companies/0/id"}]}],"limitations":[]})
    } else {
        json!({"message":"I will search Example and show the total price.","discover":true,"intent":{
            "subjects":[{"subject_ref":"s1","values":{"company.name":{"ref":"s1.label"}}}],
            "goals":[{"capability":"company.search","subject":0,"constraints":{"source":"fixture"}}],
            "answer_operations":[],"unresolved_references":[]}})
    };
    Json(json!({"choices":[{"message":{"content":reply.to_string()}}]}))
}

async fn provider(State(fixture): State<Arc<Fixture>>, headers: HeaderMap, Json(body): Json<Value>) -> Json<Value> {
    fixture.calls.fetch_add(1, Ordering::SeqCst);
    assert_eq!(headers["authorization"], "Bearer guest-person");
    assert_eq!(body["inputs"]["company.name"], "Example", "execute the original literal input");
    let selection = body["selection_id"].as_str().unwrap().parse::<Uuid>().unwrap();
    let connection:Option<Uuid> = sqlx::query_scalar("SELECT connection_id FROM public.user_selected_apis WHERE id=$1")
        .bind(selection).fetch_one(&fixture.pool).await.unwrap();
    assert!(connection.is_none());
    let charge = Uuid::new_v4();
    sqlx::query("INSERT INTO public.balance_charges(id,user_id,idempotency_key,status,charged_micro_usd,actual_provider_micro_usd,provider_price_micro_usd,fee_micro_usd,metadata) VALUES($1,$2,$3,'charged',23,20,20,3,'{}')")
        .bind(charge).bind(OWNER.parse::<Uuid>().unwrap()).bind(body["idempotency_key"].as_str().unwrap()).execute(&fixture.pool).await.unwrap();
    Json(json!({"ok":true,"status":"charged","execution_id":charge,"charged_micro_usd":23,"provider_micro_usd":20,"fee_micro_usd":3,
        "result":{"companies":[{"id":"123","name":"Example"}]},"result_truncated":false}))
}

#[tokio::test]
#[ignore = "requires isolated support-db on TEST_DATABASE_URL; never uses a live database"]
async fn guest_preview_login_approval_and_retry_execute_the_original_question_once() {
    let url = std::env::var("TEST_DATABASE_URL").expect("isolated database required");
    assert!(url.starts_with("postgresql://postgres@127.0.0.1:"));
    let pool = PgPoolOptions::new().max_connections(1)
        .after_connect(|conn,_| Box::pin(async move {
            sqlx::query("SET search_path TO public,gateway_v2,catalog,providers,identity,billing,agents,actions,kyb,integrations,observability,research,extensions").execute(conn).await?;
            Ok(())
        })).connect(&url).await.unwrap();
    let fixture = Arc::new(Fixture { pool:pool.clone(), calls:AtomicUsize::new(0) });
    let mock = Router::new().route("/parse",post(parse)).route("/chat",post(model))
        .route("/auth/v1/user",get(|headers:HeaderMap| async move {
            match headers.get("authorization").and_then(|value| value.to_str().ok()) {
                Some("Bearer guest-person") => (StatusCode::OK,Json(json!({"id":OWNER,"email":"guest@example.test","is_anonymous":false,"email_confirmed_at":"2026-01-01T00:00:00Z"}))),
                Some("Bearer another-person") => (StatusCode::OK,Json(json!({"id":"00000000-0000-4000-8000-000000000099","email":"other@example.test","is_anonymous":false,"email_confirmed_at":"2026-01-01T00:00:00Z"}))),
                Some("Bearer anonymous-person") => (StatusCode::OK,Json(json!({"id":OWNER,"email":"guest@example.test","is_anonymous":true,"email_confirmed_at":"2026-01-01T00:00:00Z"}))),
                Some("Bearer unverified-person") => (StatusCode::OK,Json(json!({"id":OWNER,"email":"guest@example.test","is_anonymous":false,"email_confirmed_at":null}))),
                _ => (StatusCode::UNAUTHORIZED,Json(json!({}))),
            }
        })).route("/functions/v1/use-selection",post(provider)).with_state(fixture.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = format!("http://{}",listener.local_addr().unwrap());
    let mock_handle = tokio::spawn(async move { axum::serve(listener,mock).await.unwrap() });
    let http = reqwest::Client::new();
    let store = Store { pool:pool.clone() };
    let engine = Arc::new(Engine {
        store:store.clone(), catalog:CatalogCache::new(pool.clone()),
        parser:Parser::new(http.clone(),format!("{upstream}/chat"),"fixture".into(),"fixture".into())
            .with_ask_v2(apiosk_gateway_v2::ask_v2::Client::new(http.clone(),"fixture".into(),"fixture".into())
                .with_local_endpoint(format!("{upstream}/parse")).unwrap()),
        signer:Signer::new(vec![8;32]).unwrap(), app_url:"http://localhost:5173".into(),
        executor:Executor { store,http:http.clone(),supabase_url:upstream.clone(),enabled:true },
    });
    let app = router(ServerState { engine,auth:Arc::new(Auth { pool:pool.clone(),http:http.clone(),supabase_url:upstream,publishable_key:"fixture".into() }),
        permits:Arc::new(Semaphore::new(16)),executor_compat:None },"http://localhost:5173").unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}",listener.local_addr().unwrap());
    let app_handle = tokio::spawn(async move { axum::serve(listener,app).await.unwrap() });
    let count:i64 = sqlx::query_scalar("SELECT count(*) FROM gateway_v2.tasks").fetch_one(&pool).await.unwrap();
    let response = http.post(format!("{base}/v2/ask-v2/guest/preview"))
        .json(&json!({"request_id":Uuid::new_v4(),"question":"Search Example using Fixture"})).send().await.unwrap();
    let status = response.status();
    let preview:Value = response.json().await.unwrap();
    assert_eq!(status,StatusCode::OK,"{preview}");
    assert_eq!(preview["status"],"ready","{preview}");
    assert_eq!(preview["total_atomic"],"23","buyer total includes the fee");
    assert!(!preview["sources"].as_array().unwrap().is_empty());
    assert_eq!(fixture.calls.load(Ordering::SeqCst),0);
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM gateway_v2.tasks").fetch_one(&pool).await.unwrap(),count,"guests create no tasks");
    let approval = || json!({"request_id":Uuid::new_v4(),"preview_token":preview["preview_token"]});
    let response = http.post(format!("{base}/v2/ask-v2/guest/approve")).json(&approval()).send().await.unwrap();
    assert_eq!(response.status(),StatusCode::UNAUTHORIZED);
    assert_eq!(fixture.calls.load(Ordering::SeqCst),0);
    let approve = |who:&str| http.post(format!("{base}/v2/ask-v2/guest/approve")).bearer_auth(who)
        .header("x-apiosk-workspace",Uuid::new_v4().to_string()).json(&approval());
    for who in ["anonymous-person","unverified-person","apk_access_local_fixture"] {
        let response = approve(who).send().await.unwrap();
        assert_eq!(response.status(),StatusCode::UNAUTHORIZED,"{who}");
    }
    let (first,retry) = tokio::join!(approve("guest-person").send(),approve("guest-person").send());
    let first = first.unwrap(); let retry = retry.unwrap();
    let (status,retry_status) = (first.status(),retry.status());
    let first:Value = first.json().await.unwrap(); let retry:Value = retry.json().await.unwrap();
    assert_eq!(status,StatusCode::OK,"{first}");
    assert_eq!(retry_status,StatusCode::OK,"{retry}");
    let task = first["state"]["state_ref"].as_str().unwrap().to_owned();
    assert_eq!(retry["state"]["state_ref"],task,"new request IDs cannot duplicate approval");
    let mut completed = first;
    for _ in 0..80 {
        if completed["status"] == "succeeded" { break; }
        tokio::time::sleep(Duration::from_millis(100)).await;
        completed = http.get(format!("{base}/v2/tasks/{task}")).bearer_auth("guest-person").send().await.unwrap().json().await.unwrap();
    }
    assert_eq!(completed["status"],"succeeded","{completed}");
    assert_eq!(fixture.calls.load(Ordering::SeqCst),1);
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM authorizations WHERE task_id=$1").bind(task.parse::<Uuid>().unwrap()).fetch_one(&pool).await.unwrap(),1);
    let response = approve("guest-person").send().await.unwrap();
    assert_eq!(response.status(),StatusCode::OK);
    let resumed:Value = response.json().await.unwrap();
    assert_eq!(resumed["state"]["state_ref"],task);
    assert_eq!(fixture.calls.load(Ordering::SeqCst),1);
    let response = approve("another-person").send().await.unwrap();
    assert_eq!(response.status(),StatusCode::CONFLICT);
    let refused:Value = response.json().await.unwrap();
    assert_eq!(refused["errors"][0]["code"],"guest_preview_used");
    app_handle.abort(); mock_handle.abort();
}
