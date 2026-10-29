//! EU/NL source engine integration: actual binding and adapters, fixture billing/model.
//! Billing, model and upstream responses are explicit local fixtures.
// The shared executor adapter also exposes operations outside this fixture.
#[allow(dead_code)]
#[path = "../crates/commerce-runtime/src/proxy/public_sources.rs"]
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
    serde_json::from_str(include_str!("fixtures/eu-public-sources/catalog.json")).unwrap()
}
fn case(cap: &str) -> (String, Value) {
    let f = fixture();
    let service = f["services"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["contract"]["capability_slug"] == cap)
        .unwrap();
    let e = f["endpoints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == service["endpoint_id"])
        .unwrap();
    let mut values = serde_json::Map::new();
    for (fact, binding) in service["contract"]["input_map"].as_object().unwrap() {
        if let Some(v) = e["request_example"].get(binding["name"].as_str().unwrap()) {
            values.insert(fact.clone(), v.clone());
        }
    }
    (cap.into(), Value::Object(values))
}
async fn model(Json(body): Json<Value>) -> Json<Value> {
    let input: Value =
        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    let key = input["question"]
        .as_str()
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let (cap, values) = case(key);
    let f = fixture();
    let source = f["services"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["contract"]["capability_slug"] == key)
        .unwrap()["api_slug"]
        .clone();
    let intent = json!({"subjects":[{"entity_ref":null,"label":key,"values":values}],"goals":[{"capability":cap,"subject":0,"constraints":{"source":source}}],"answer_operations":[],"unresolved_references":[]});
    // Every surface reads a question through the Ask chat pass: a concrete
    // request returns its intent together with the reply.
    let content = if input["surface"] == "ask-chat" {
        json!({"message":"I will look that up and show the price first.","discover":true,"intent":intent})
    } else {
        intent
    };
    Json(json!({"choices":[{"message":{"content":content.to_string()}}]}))
}
async fn provider(State(s): State<TestState>, Json(body): Json<Value>) -> Json<Value> {
    let id: Uuid = serde_json::from_value(body["selection_id"].clone()).unwrap();
    let cap: String = sqlx::query_scalar("SELECT capability FROM user_selected_apis WHERE id=$1")
        .bind(id)
        .fetch_one(&s.pool)
        .await
        .unwrap();
    let f = fixture();
    let service = f["services"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["contract"]["capability_slug"] == cap)
        .unwrap();
    let e = f["endpoints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == service["endpoint_id"])
        .unwrap();
    let source = service["api_slug"].as_str().unwrap();
    let info = f["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["slug"] == source)
        .unwrap();
    let mut path = e["path"].as_str().unwrap().to_string();
    let mut params = url::form_urlencoded::Serializer::new(String::new());
    let mut payload = serde_json::Map::new();
    for (k, v) in body["inputs"].as_object().unwrap() {
        let spec = e["query_params"]["properties"]
            .get(k)
            .or_else(|| e["request_body"]["properties"].get(k))
            .unwrap();
        let value = v
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| v.to_string());
        match spec["x-parameter-location"].as_str().unwrap_or("query") {
            "path" => path = path.replace(&format!("{{{k}}}"), &value),
            "body" => {
                payload.insert(k.clone(), v.clone());
            }
            _ => {
                params.append_pair(k, &value);
            }
        }
    }
    let origin = e["origin_override"]
        .as_str()
        .unwrap_or(info["origin"].as_str().unwrap());
    let mut prepared = adapter::prepare(
        source,
        service["method"].as_str().unwrap(),
        &format!("{origin}{path}?{}", params.finish()),
    )
    .unwrap()
    .unwrap();
    adapter::bind_body(&mut prepared, &serde_json::to_vec(&payload).unwrap()).unwrap();
    let name = match cap.as_str() {
        "vat.eu.validate" => "vies-get",
        "vat.eu.check" => "vies-post",
        "vat.eu.service-status" => "vies-status",
        "government.staatscourant.search" => "staatscourant",
        "government.cvdr.search" => "cvdr",
        "government.bwb.search" => "bwb-ok",
        "eu_law.sparql.select" => "cellar-json",
        "eu_law.celex.metadata" => "celex-object",
        "eu_funding.calls.search" => "funding-en",
        "planning.pdok.feature-info" => "pdok",
        _ => "bodem-hit",
    };
    let raw = std::fs::read(format!("tests/fixtures/eu-public-sources/{name}.body")).unwrap();
    let result = adapter::normalize_bytes(&prepared, &raw).unwrap();
    s.count.fetch_add(1, Ordering::SeqCst);
    Json(
        json!({"ok":true,"charged_micro_usd":97826,"provider_micro_usd":85066,"fee_micro_usd":12760,"result":result,"result_truncated":false}),
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
#[ignore = "requires isolated support-db; model, billing and source responses are fixtures"]
async fn new_sources_require_approval_and_reuse_saved_results() {
    let db = std::env::var("TEST_DATABASE_URL").expect("isolated database required");
    assert!(
        db.contains("127.0.0.1:"),
        "This test must never seed a remote database"
    );
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("SET search_path TO public,gateway_v2")
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
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
            sqlx::query("INSERT INTO api_endpoints(id,api_id,method,path,price,active) VALUES($1,$1,$3,$2,0.085066,true)")
                .bind(endpoint)
                .bind(service["path"].as_str().unwrap())
                .bind(service["method"].as_str().unwrap())
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query("INSERT INTO capability_execution_contracts VALUES($1,$2,$3,1,'active',$4,$5,$6,NULL,'one',1,$7,'{}',$8,3600,$9,now(),'read','per_call')")
            .bind(id).bind(endpoint).bind(c["capability_slug"].as_str().unwrap()).bind(&c["requires"]).bind(&c["input_map"]).bind(&c["output_map"]).bind(&service["fixed_params"]).bind(&service["pagination"]).bind(&service["sample_payload"]).execute(&pool).await.unwrap();
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
        workspace: None,
    };
    let mut evidence = vec![];
    for source in f["services"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["contract"]["capability_slug"].as_str().unwrap())
    {
        let before = state.count.load(Ordering::SeqCst);
        let (_, values) = case(source);
        let question = format!(
            "{source} {}",
            values
                .as_object()
                .unwrap()
                .values()
                .map(|v| v
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| v.to_string()))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let start = engine
            .discover(
                &person,
                DiscoverRequest {
                    request_id: Uuid::new_v4(),
                    question,
                    state: None,
                    context_delta: Default::default(),
                },
            )
            .await
            .unwrap();
        assert_eq!(start.status, Status::Ready, "{start:?}");
        let quote = start.proposal.as_ref().unwrap();
        assert_eq!(quote.max_total_atomic, "97826");
        let refusal = engine
            .execute(&person, "fixture", execute(&start))
            .await
            .unwrap();
        assert_ne!(refusal.status, Status::Succeeded);
        assert_eq!(state.count.load(Ordering::SeqCst), before);
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
        let request = execute(&start);
        let result = engine
            .execute(&person, "fixture", request.clone())
            .await
            .unwrap();
        assert_eq!(result.status, Status::Succeeded, "{result:?}");
        assert_eq!(state.count.load(Ordering::SeqCst), before + 1);
        let replay = engine.execute(&person, "fixture", request).await.unwrap();
        assert_eq!(replay.result, result.result);
        assert_eq!(state.count.load(Ordering::SeqCst), before + 1);
        let saved = engine
            .view(
                &person,
                result.state.as_ref().unwrap().state_ref,
                Uuid::new_v4(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            saved.result.as_ref().unwrap()["data"],
            result.result.as_ref().unwrap()["data"]
        );
        if source != "government.staatscourant.search" {
            assert_eq!(result.result.as_ref().unwrap()["coverage"], "partial");
        }
        evidence.push(json!({"source":source,"quote":quote.max_total_atomic,"approval_required":true,"upstream_calls":1,"replay_calls":0,"result":result.result}));
    }
    if let Ok(dir) = std::env::var("EU_SOURCE_EVIDENCE") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            format!("{dir}/durable-flow.json"),
            serde_json::to_vec_pretty(&evidence).unwrap(),
        )
        .unwrap();
    }
    println!("11 source flows: quote, approval boundary, execution, persistence and replay passed; billing/model are local fixtures.");
    server.abort();
    pool.close().await;
}
