//! Local durable engine + real HTTP boundary + public source adapters.
//! Billing/model are explicit fixtures; optional live upstream reads cost no Apiosk balance.
#[path = "../legacy-executor/src/proxy/public_sources.rs"]
mod adapter;
use apiosk_gateway_v2::{
    auth::Principal,
    catalog::CatalogCache,
    engine::Engine,
    execution::Executor,
    parser::Parser,
    protocol::{DiscoverRequest, ExecuteRequest, Response, Status},
    state::Signer,
    store::Store,
};
use axum::{extract::State, routing::post, Json, Router};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use uuid::Uuid;

#[derive(Clone)]
struct TestState {
    pool: PgPool,
    http: reqwest::Client,
    count: Arc<AtomicUsize>,
}
fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/dutch-links/catalog.json")).unwrap()
}
async fn model(Json(_body): Json<Value>) -> Json<Value> {
    let goals = [
        "building.bag-building",
        "area.cbs-neighbourhood-2025",
        "property.linked-parcels",
        "procurement.rsin-mentions",
        "procurement.company-name-mentions",
        "procurement.ted-kvk-notices",
        "procurement.ted-rsin-notices",
    ];
    Json(
        json!({"choices":[{"message":{"content":json!({"subjects":[{"entity_ref":null,"label":"30204462","values":{"company_registry.kvknummer":"30204462"}}],"goals":goals.iter().map(|cap|json!({"capability":cap,"subject":0,"constraints":{}})).collect::<Vec<_>>(),"answer_operations":[],"unresolved_references":[]}).to_string()}}]}),
    )
}
async fn provider(State(s): State<TestState>, Json(body): Json<Value>) -> Json<Value> {
    let id: Uuid = serde_json::from_value(body["selection_id"].clone()).unwrap();
    let endpoint: Uuid =
        sqlx::query_scalar("SELECT candidate_id FROM user_selected_apis WHERE id=$1")
            .bind(id)
            .fetch_one(&s.pool)
            .await
            .unwrap();
    let f = fixture();
    let service = f["services"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["endpoint_id"] == json!(endpoint))
        .unwrap();
    let source = service["api_slug"].as_str().unwrap();
    s.count.fetch_add(1, Ordering::SeqCst);
    let mut data = if source == "pdok" || source == "ted" {
        let base = if source == "pdok" {
            "https://api.pdok.nl"
        } else {
            "https://api.ted.europa.eu"
        };
        let mut url =
            url::Url::parse(&format!("{base}{}", service["path"].as_str().unwrap())).unwrap();
        for (k, v) in body["inputs"].as_object().unwrap() {
            url.query_pairs_mut().append_pair(
                k,
                &v.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| v.to_string()),
            );
        }
        let p = adapter::prepare(source, "GET", url.as_str())
            .unwrap()
            .unwrap();
        let request = if let Some(body) = &p.json_body {
            s.http.post(&p.url).json(body)
        } else {
            s.http.get(&p.url)
        };
        let raw: Value = request
            .timeout(std::time::Duration::from_secs(25))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        adapter::normalize(&p, &raw).unwrap()
    } else {
        service["sample_payload"].clone()
    };
    if std::env::var("DUTCH_LINKS_OMIT_OPTIONAL").as_deref()==Ok("1") {
        if source=="kvk-dutch-business-register" { data["_embedded"]["eigenaar"].as_object_mut().unwrap().remove("rsin"); }
        if service["path"]=="/apiosk/company-address" { for row in data["items"].as_array_mut().unwrap(){row.as_object_mut().unwrap().remove("apartment_right_designations");} }
    }
    let provider = service["list_atomic"].as_i64().unwrap();
    let fee = (provider * 1500 + 9999) / 10000;
    Json(
        json!({"ok":true,"charged_micro_usd":provider+fee,"provider_micro_usd":provider,"fee_micro_usd":fee,"result":data,"result_truncated":false}),
    )
}
fn execute(r: &Response) -> ExecuteRequest {
    let a = r
        .next_actions
        .iter()
        .find(|a| a.kind == "execute_quoted_step")
        .unwrap();
    ExecuteRequest {
        request_id: Uuid::new_v4(),
        action_id: a.action_id,
        state: r.state.clone().unwrap(),
        quote_ref: r.proposal.as_ref().map(|q| q.quote_ref),
        authorization_ref: None,
        idempotency_key: a.action_id,
        input: Value::Null,
    }
}
#[tokio::test]
#[ignore = "requires isolated support-db; optional DUTCH_SOURCE_LIVE=1 reads public upstreams"]
async fn identifier_graph_requires_one_approval_and_reuses_each_dependency() {
    let db = std::env::var("TEST_DATABASE_URL").expect("isolated database required");
    assert!(
        db.contains("127.0.0.1:"),
        "This test must never seed a remote database"
    );
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&db)
        .await
        .unwrap();
    sqlx::raw_sql("DELETE FROM capability_execution_contracts; DELETE FROM api_endpoints; DELETE FROM apis; DELETE FROM capabilities; DELETE FROM fact_types;").execute(&pool).await.unwrap();
    let f = fixture();
    for w in f["vocabulary"].as_array().unwrap() {
        sqlx::query("INSERT INTO fact_types VALUES($1,'active',$2,'NL')")
            .bind(w["slug"].as_str().unwrap())
            .bind(&w["value_schema"])
            .execute(&pool)
            .await
            .unwrap();
    }
    let mut endpoints = HashSet::new();
    for service in f["services"].as_array().unwrap() {
        let c = &service["contract"];
        let endpoint: Uuid = serde_json::from_value(service["endpoint_id"].clone()).unwrap();
        let id: Uuid = serde_json::from_value(service["id"].clone()).unwrap();
        sqlx::query("INSERT INTO capabilities VALUES($1,'active')")
            .bind(c["capability_slug"].as_str().unwrap())
            .execute(&pool)
            .await
            .unwrap();
        if endpoints.insert(endpoint) {
            sqlx::query("INSERT INTO apis(id,slug,name,listing_type,status,active,is_verified,discoverable) VALUES($1,$2,$3,'api','active',true,true,true)").bind(endpoint).bind(service["api_slug"].as_str().unwrap()).bind(service["name"].as_str().unwrap()).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO api_endpoints VALUES($1,$1,'GET',$2,($3::bigint::numeric/1000000),true)")
                .bind(endpoint)
                .bind(service["path"].as_str().unwrap())
                .bind(service["list_atomic"].as_i64().unwrap())
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query("INSERT INTO capability_execution_contracts VALUES($1,$2,$3,1,'active',$4,$5,$6,$10,$11,1,$7,'{}',$8,3600,$9,now(),'read','per_call')")
            .bind(id).bind(endpoint).bind(c["capability_slug"].as_str().unwrap()).bind(&c["requires"]).bind(&c["input_map"]).bind(&c["output_map"]).bind(&service["fixed_params"]).bind(&service["pagination"]).bind(&service["sample_payload"]).bind(c["identifies"].as_str()).bind(c["cardinality"].as_str().unwrap()).execute(&pool).await.unwrap();
    }
    for service in f["services"].as_array().unwrap() {
        let optional=service["optional_outputs"].as_array().cloned().unwrap_or_default();
        let produces:Vec<_>=optional.into_iter().map(|fact|json!({"fact_type":fact,"optional":true})).collect();
        sqlx::query("UPDATE capability_execution_contracts SET produces=$1 WHERE id=$2").bind(json!(produces)).bind(serde_json::from_value::<Uuid>(service["id"].clone()).unwrap()).execute(&pool).await.unwrap();
    }
    let state = TestState {
        pool: pool.clone(),
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
        count: Arc::new(AtomicUsize::new(0)),
    };
    let mock = Router::new()
        .route("/chat", post(model))
        .route("/functions/v1/use-selection", post(provider))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let store = Store { pool: pool.clone() };
    let engine = Engine {
        store: store.clone(),
        catalog: CatalogCache::new(pool.clone()),
        parser: Parser::new(
            state.http.clone(),
            format!("{base}/chat"),
            "fixture".into(),
            "fixture".into(),
        ),
        signer: Signer::new(vec![3; 32]).unwrap(),
        executor: Executor {
            store,
            http: state.http.clone(),
            supabase_url: base,
            enabled: true,
        },
        app_url: "http://localhost:5173".into(),
    };
    let person = Principal {
        owner: Uuid::new_v4(),
        connection: None,
    };
    let start=engine.discover(&person,DiscoverRequest{request_id:Uuid::new_v4(),question:"For KVK 30204462 retrieve its building, CBS 2025 neighbourhood statistics, linked cadastral parcels, TenderNed mentions and TED KVK/RSIN notices.".into(),state:None,context_delta:Default::default()}).await.unwrap();
    assert_eq!(start.status, Status::Ready, "{start:?}");
    let quote = start.proposal.as_ref().unwrap();
    assert_eq!(quote.steps.len(), 10);
    let refusal = engine
        .execute(&person, "fixture", execute(&start))
        .await
        .unwrap();
    assert_ne!(refusal.status, Status::Succeeded);
    assert_eq!(state.count.load(Ordering::SeqCst), 0);
    engine
        .executor
        .approve(
            &person,
            start.state.as_ref().unwrap().state_ref,
            quote.quote_ref,
            &quote.max_total_atomic,
        )
        .await
        .unwrap();
    let missing_optional=std::env::var("DUTCH_LINKS_OMIT_OPTIONAL").as_deref()==Ok("1");
    let mut response = start.clone();
    for _ in 0..10 {
        let request = execute(&response);
        let next = engine
            .execute(&person, "fixture", request.clone())
            .await
            .unwrap();
        let calls = state.count.load(Ordering::SeqCst);
        let replay = engine.execute(&person, "fixture", request).await.unwrap();
        assert_eq!(state.count.load(Ordering::SeqCst), calls);
        assert_eq!(replay.result, next.result);
        response = next;
        if matches!(response.status,Status::Succeeded|Status::Partial) {
            break;
        }
        assert_eq!(response.status, Status::Ready, "{response:?}");
    }
    assert_eq!(response.status, if missing_optional {Status::Partial}else{Status::Succeeded}, "{response:?}");
    assert_eq!(state.count.load(Ordering::SeqCst), if missing_optional {8}else{10});
    let kvk: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM user_selected_apis WHERE api_slug='kvk-dutch-business-register'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(kvk, 1);
    let authorizations: i64 = sqlx::query_scalar("SELECT count(*) FROM gateway_v2.authorizations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(authorizations, 1);
    std::fs::write(if missing_optional {"output/dutch-links-20260911/durable-missing-rsin.json"}else{"output/dutch-links-20260911/durable-graph.json"},serde_json::to_vec_pretty(&json!({"quote":quote.max_total_atomic,"steps":10,"approvals":1,"kvk_calls":kvk,"replay_extra_calls":0,"result":response})).unwrap()).unwrap();
    println!("Ten-step graph passed: one approval, one shared KVK lookup, source-backed PDOK/TED reads, zero replay calls. KVK/TenderNed/model/billing are fixtures.");
    server.abort();
    pool.close().await;
}
