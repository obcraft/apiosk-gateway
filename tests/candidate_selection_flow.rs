//! A name search that returns more companies than can be bound automatically
//! continues on the person's choice among the distinct ones; repeated rows of
//! one company continue by themselves. Nothing dependent is bought before.
use apiosk_gateway_v2::{
    auth::Principal,
    catalog::CatalogCache,
    engine::Engine,
    execution::Executor,
    parser::Parser,
    protocol::{ExecuteRequest, Status},
    state::Signer,
    store::Store,
};
use axum::{extract::State, routing::post, Json, Router};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use std::{
    sync::{atomic::{AtomicUsize, Ordering}, Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

struct Provider {
    pool: sqlx::PgPool,
    calls: AtomicUsize,
    accounts: Mutex<Vec<Value>>,
}

async fn provider(State(c): State<Arc<Provider>>, Json(body): Json<Value>) -> Json<Value> {
    c.calls.fetch_add(1, Ordering::SeqCst);
    let search = body["inputs"].get("company.name").is_some();
    let result = if search {
        // 150 rows: company 101 three times, then 147 other companies; or
        // four rows of one company.
        let ids: Vec<String> = if body["inputs"]["company.name"] == "Many" {
            ["100", "101", "101", "101"].iter().map(|id| id.to_string()).chain((102..248).map(|n| n.to_string())).collect()
        } else if body["inputs"]["company.name"] == "Exact Company B.V." {
            vec!["111".into(), "222".into(), "222".into()]
        } else {
            vec!["777".into(); 4]
        };
        json!({"companies":ids.iter().map(|id| json!({"id":id,"name":if id == "111" { "Exact Company B.V.".to_owned() } else { format!("Company {id}") }})).collect::<Vec<_>>()})
    } else {
        c.accounts.lock().unwrap().push(body["inputs"]["company.id"].clone());
        json!({"accounts":body["inputs"]["company.id"]})
    };
    sqlx::query("INSERT INTO public.balance_charges(id,user_id,idempotency_key,status,charged_micro_usd,provider_price_micro_usd,fee_micro_usd) SELECT $1,owner_id,id::text,'charged',$2,$3,$4 FROM gateway_v2.executions WHERE id=$5")
        .bind(Uuid::new_v4()).bind(if search { 23i64 } else { 115 }).bind(if search { 20i64 } else { 100 }).bind(if search { 3i64 } else { 15 })
        .bind(body["idempotency_key"].as_str().unwrap().parse::<Uuid>().unwrap()).execute(&c.pool).await.unwrap();
    Json(json!({"ok":true,"result":result,"charged_micro_usd":if search { 23 } else { 115 },"provider_micro_usd":if search { 20 } else { 100 },"fee_micro_usd":if search { 3 } else { 15 }}))
}

async fn settled(engine: &Engine, p: &Principal, id: Uuid) -> apiosk_gateway_v2::protocol::Response {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if !engine.worker_active(id).await.unwrap() {
                return engine.view(p, id, Uuid::new_v4(), None).await.unwrap();
            }
            tokio::time::sleep(Duration::from_millis(30)).await
        }
    }).await.expect("worker must settle")
}

/// The App's purchase of the accounts endpoint by company name: its native
/// name search is priced first, in the same quote.
async fn approved(engine: &Arc<Engine>, p: &Principal, name: &str) -> (Uuid, Uuid) {
    let accounts: Uuid = "484a43fa-3de1-4c09-8c5f-21305829b805".parse().unwrap();
    let input = std::collections::BTreeMap::from([("company.name".to_owned(), json!(name))]);
    let r = engine.prepare_endpoint(p, Uuid::new_v4(), accounts, None, &input).await.unwrap();
    assert_eq!(r.proposal.as_ref().map(|quote| quote.max_total_atomic.as_str()), Some("138"), "{r:?}");
    let quote = r.proposal.clone().unwrap_or_else(|| panic!("{r:?}"));
    let id = r.state.as_ref().unwrap().state_ref;
    engine.executor.approve(p, id, quote.quote_ref, &quote.max_total_atomic).await.unwrap();
    engine.start_worker(p.clone(), "fixture".into(), id).await.unwrap();
    (id, quote.quote_ref)
}

#[tokio::test]
#[ignore = "isolated support-db only"]
async fn many_companies_become_a_bounded_choice_and_duplicates_bind_themselves() {
    let pool = PgPoolOptions::new().max_connections(1)
        .after_connect(|conn, _| Box::pin(async move {
            sqlx::query("SET search_path TO public,gateway_v2").execute(conn).await?;
            Ok(())
        }))
        .connect(&std::env::var("TEST_DATABASE_URL").unwrap()).await.unwrap();
    // Include names as the real KVK search does, so an exact name match can
    // exercise the decision that used to skip the identifier choice.
    sqlx::query("UPDATE capability_execution_contracts SET output_map=jsonb_set(output_map,'{produces,company.name}', '{\"path\":\"$.name\"}'::jsonb) WHERE capability_slug='company.search'")
        .execute(&pool).await.unwrap();
    let state = Arc::new(Provider { pool: pool.clone(), calls: AtomicUsize::new(0), accounts: Mutex::new(Vec::new()) });
    let app = Router::new().route("/functions/v1/use-selection", post(provider)).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let store = Store { pool: pool.clone() };
    let http = reqwest::Client::new();
    let engine = Arc::new(Engine {
        store: store.clone(),
        catalog: CatalogCache::new(pool.clone()),
        parser: Parser::new(http.clone(), format!("{base}/chat"), "fixture".into(), "fixture".into()),
        signer: Signer::new(vec![3; 32]).unwrap(),
        executor: Executor { store: store.clone(), http, supabase_url: base, enabled: true },
        app_url: "http://localhost".into(),
    });
    let p = Principal { owner: Uuid::new_v4(), connection: None, workspace: None };

    // 150 rows, more than a task binds automatically: this used to stop as
    // `incomplete_source` and skip the accounts call.
    let (id, quote) = approved(&engine, &p, "Many").await;
    let choice = settled(&engine, &p, id).await;
    assert_eq!(choice.status, Status::NeedsSelection, "{choice:?}");
    assert!(choice.errors.is_empty(), "{:?}", choice.errors);
    let candidates = choice.context_view["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 20);
    let ids: Vec<_> = candidates.iter().map(|c| c["facts"][0]["value"].clone()).collect();
    assert_eq!(ids.iter().filter(|id| **id == json!("101")).count(), 1, "repeated rows are one candidate");
    assert_eq!(ids[..3], [json!("100"), json!("101"), json!("102")]);
    assert!(choice.context_view["candidates_note"].as_str().unwrap().contains("Only the first 20 distinct matches"));
    assert_eq!(state.calls.load(Ordering::SeqCst), 1, "no dependent call before the choice");
    assert_eq!(choice.billing.as_ref().unwrap()["total_charged"], "23");
    let action = choice.next_actions.iter().find(|a| a.kind == "select_entity").unwrap();
    let selected = candidates[2]["entity_ref"].clone();
    let resumed = engine.execute(&p, "fixture", ExecuteRequest {
        request_id: Uuid::new_v4(), action_id: action.action_id, idempotency_key: action.action_id,
        state: choice.state.unwrap(), quote_ref: Some(quote), authorization_ref: None, input: json!({"entity_ref":selected}),
    }).await.unwrap();
    assert_eq!(resumed.proposal.as_ref().unwrap().quote_ref, quote, "the choice continues the approved quote");
    engine.start_worker(p.clone(), "fixture".into(), id).await.unwrap();
    let done = settled(&engine, &p, id).await;
    assert_eq!(done.status, Status::Succeeded, "{done:?}");
    assert_eq!(done.result.unwrap()["data"]["accounts"], "102");
    assert!(done.context_view.get("candidates").is_none());
    assert_eq!(state.calls.load(Ordering::SeqCst), 2, "the choice does not repeat the search");
    assert_eq!(done.billing.unwrap()["total_charged"], "138");
    let approvals: i64 = sqlx::query_scalar("SELECT count(*) FROM authorizations WHERE task_id=$1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(approvals, 1);

    // Four rows of one company: no question, the accounts call follows.
    let (id, _) = approved(&engine, &p, "Repeated").await;
    let done = settled(&engine, &p, id).await;
    assert_eq!(done.status, Status::Succeeded, "{done:?}");
    assert_eq!(done.result.unwrap()["data"]["accounts"], "777");
    assert_eq!(state.calls.load(Ordering::SeqCst), 4);
    assert_eq!(*state.accounts.lock().unwrap(), [json!("102"), json!("777")]);

    // Even an exact company name waits when a different identifier is also
    // returned. The person may choose that other company under the same cap.
    let (id, quote) = approved(&engine, &p, "Exact Company B.V.").await;
    let choice = settled(&engine, &p, id).await;
    assert_eq!(choice.status, Status::NeedsSelection, "{choice:?}");
    let candidates = choice.context_view["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 2);
    assert_eq!(state.calls.load(Ordering::SeqCst), 5, "no financials before the choice");
    let selected = candidates.iter().find(|candidate| candidate["facts"].as_array().unwrap().iter().any(|fact| fact["type"] == "company.id" && fact["value"] == "222")).unwrap();
    let action = choice.next_actions.iter().find(|a| a.kind == "select_entity").unwrap();
    engine.execute(&p, "fixture", ExecuteRequest {
        request_id: Uuid::new_v4(), action_id: action.action_id, idempotency_key: action.action_id,
        state: choice.state.clone().unwrap(), quote_ref: Some(quote), authorization_ref: None,
        input: json!({"entity_ref":selected["entity_ref"]}),
    }).await.unwrap();
    engine.start_worker(p.clone(), "fixture".into(), id).await.unwrap();
    let done = settled(&engine, &p, id).await;
    assert_eq!(done.status, Status::Succeeded, "{done:?}");
    assert_eq!(done.result.unwrap()["data"]["accounts"], "222");
    assert_eq!(done.proposal.as_ref().unwrap().quote_ref, quote);
    assert_eq!(state.calls.load(Ordering::SeqCst), 6, "lookup is reused");
    let approvals: i64 = sqlx::query_scalar("SELECT count(*) FROM authorizations WHERE task_id=$1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(approvals, 1);
    server.abort();
    pool.close().await;
}
