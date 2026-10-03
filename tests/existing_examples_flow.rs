//! The four published starter families through the real durable engine.
//! Official recorded source payloads and a loopback settlement adapter are
//! fixtures. No provider, model, wallet or production database is contacted.
#[path = "support/gate.rs"]
mod gate;
use apiosk_gateway_v2::{
    auth::Principal, catalog::CatalogCache, engine::Engine, execution::Executor,
    parser::Parser, protocol::{DiscoverRequest, ExecuteRequest, Response, Status},
    state::Signer, store::Store,
};
use axum::{extract::State, routing::post, Json, Router};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::{collections::BTreeMap, sync::{Arc, atomic::{AtomicUsize, Ordering}}};
use uuid::Uuid;

const QUESTIONS: [(&str, &str, usize); 4] = [
    ("company", "Find the registered details and available annual financial figures for Mollie B.V. (KVK 30204462).", 2),
    ("location", "Find building and neighbourhood information for Damrak 1, Amsterdam.", 5),
    ("tender", "Find public tenders for IT services in the Netherlands.", 1),
    ("cbs", "Compare bankruptcies per sector in the Netherlands.", 5),
];

fn catalog_fixture() -> Value {
    let mut vocabulary = BTreeMap::new();
    let mut services = BTreeMap::new();
    for raw in [include_str!("fixtures/workflows/catalog.json"), include_str!("fixtures/dutch-links/catalog.json"),
        include_str!("fixtures/tenderned-browse.json"), include_str!("fixtures/cbs-bankruptcy-comparison-catalog.json")] {
        let fixture: Value = serde_json::from_str(raw).unwrap();
        for fact in fixture["vocabulary"].as_array().unwrap() {
            vocabulary.insert(fact["slug"].as_str().unwrap().to_owned(), fact.clone());
        }
        for service in fixture["services"].as_array().unwrap() {
            let capability = service["contract"]["capability_slug"].as_str().unwrap();
            let provider = service["api_slug"].as_str().unwrap();
            let wanted = match provider {
                "kvk-dutch-business-register" => matches!(capability, "company.registry-profile" | "company.financial-statements"),
                "pdok" => matches!(capability, "location.property-search" | "building.bag-unit" | "building.bag-building" | "location.bag-address-context" | "area.cbs-neighbourhood-2025"),
                "tenderned" => capability == "procurement.nl-public-contracts",
                "cbs-statline" => capability.starts_with("statistics.cbs-bankruptcy-"),
                _ => false,
            };
            if wanted { services.insert((provider.to_owned(), capability.to_owned()), service.clone()); }
        }
    }
    json!({"vocabulary":vocabulary.into_values().collect::<Vec<_>>(), "services":services.into_values().collect::<Vec<_>>()})
}

async fn seed(pool: &PgPool, fixture: &Value) {
    sqlx::raw_sql("DELETE FROM capability_execution_contracts; DELETE FROM api_endpoints; DELETE FROM apis; DELETE FROM capabilities; DELETE FROM fact_types;").execute(pool).await.unwrap();
    for fact in fixture["vocabulary"].as_array().unwrap() {
        sqlx::query("INSERT INTO fact_types VALUES($1,'active',$2,NULL)").bind(fact["slug"].as_str().unwrap()).bind(&fact["value_schema"]).execute(pool).await.unwrap();
    }
    let mut apis = BTreeMap::new();
    for service in fixture["services"].as_array().unwrap() {
        let c = &service["contract"];
        let endpoint: Uuid = serde_json::from_value(service["endpoint_id"].clone()).unwrap();
        let id: Uuid = serde_json::from_value(service["id"].clone()).unwrap();
        let slug = service["api_slug"].as_str().unwrap();
        let fresh = !apis.contains_key(slug);
        let api = *apis.entry(slug).or_insert_with(Uuid::new_v4);
        sqlx::query("INSERT INTO capabilities(slug,status) VALUES($1,'active') ON CONFLICT DO NOTHING").bind(c["capability_slug"].as_str().unwrap()).execute(pool).await.unwrap();
        if let Some(description) = service["description"].as_str() {
            sqlx::query("UPDATE capabilities SET description=$1 WHERE slug=$2").bind(description).bind(c["capability_slug"].as_str().unwrap()).execute(pool).await.unwrap();
        }
        if fresh {
            sqlx::query("INSERT INTO apis(id,slug,name,listing_type,status,active,is_verified,discoverable) VALUES($1,$2,$3,$4,'active',true,true,true)")
                .bind(api).bind(slug).bind(service["name"].as_str().unwrap()).bind(service["listing_type"].as_str().unwrap()).execute(pool).await.unwrap();
        }
        sqlx::query("INSERT INTO api_endpoints(id,api_id,method,path,price,active) VALUES($1,$2,$3,$4,($5::bigint::numeric/1000000),true) ON CONFLICT DO NOTHING")
            .bind(endpoint).bind(api).bind(service["method"].as_str().unwrap()).bind(service["path"].as_str().unwrap()).bind(service["list_atomic"].as_i64().unwrap()).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO capability_execution_contracts(id,endpoint_id,capability_slug,version,status,requires,input_map,output_map,identifies,cardinality,max_fan_out,fixed_params,preconditions,pagination,freshness_seconds,sample_payload,validated_at,effect,price_model) VALUES($1,$2,$3,1,'active',$4,$5,$6,$10,$11,1,$7,$12,$8,3600,$9,now(),'read','per_call')")
            .bind(id).bind(endpoint).bind(c["capability_slug"].as_str().unwrap()).bind(&c["requires"]).bind(&c["input_map"]).bind(&c["output_map"])
            .bind(&service["fixed_params"]).bind(&service["pagination"]).bind(&service["sample_payload"]).bind(c["identifies"].as_str()).bind(c["cardinality"].as_str().unwrap()).bind(&service["preconditions"]).execute(pool).await.unwrap();
        let optional: Vec<_> = service["optional_outputs"].as_array().into_iter().flatten().map(|fact| json!({"fact_type":fact,"optional":true})).collect();
        sqlx::query("UPDATE capability_execution_contracts SET produces=$1 WHERE id=$2").bind(json!(optional)).bind(id).execute(pool).await.unwrap();
    }
    sqlx::raw_sql("CREATE ROLE service_role").execute(pool).await.unwrap();
    sqlx::raw_sql(include_str!("../supabase/migrations/20260912111106_kvk_annual_accounts_queue.sql")).execute(pool).await.unwrap();
    let annual: Value = serde_json::from_str(include_str!("fixtures/kvk-mollie-available-figures.json")).unwrap();
    sqlx::query("INSERT INTO kvk_cache.annual_accounts(kvk_number,financial_year,document_type,version,payload,source_url) VALUES('30204462',$1,$2,repeat('a',64),$3,$4)")
        .bind(annual["financial_year"].as_i64().unwrap() as i32).bind(kvk_accounts::DOCUMENT_TYPE).bind(&annual["payload"]).bind(annual["source_url"].as_str().unwrap()).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO kvk_cache.jobs(kvk_number,status,financial_year,document_type,version,fresh_until) VALUES('30204462','succeeded',$1,$2,repeat('a',64),now()+interval '24 hours')")
        .bind(annual["financial_year"].as_i64().unwrap() as i32).bind(kvk_accounts::DOCUMENT_TYPE).execute(pool).await.unwrap();
}

#[derive(Clone)]
struct Mock {
    pool: PgPool,
    fixture: Value,
    providers: Arc<AtomicUsize>,
    models: Arc<AtomicUsize>,
}

async fn model(State(state): State<Mock>) -> Json<Value> {
    state.models.fetch_add(1, Ordering::SeqCst);
    Json(json!({"choices":[{"message":{"content":"{}"}}]}))
}

async fn provider(State(state): State<Mock>, Json(body): Json<Value>) -> Json<Value> {
    let selection: Uuid = serde_json::from_value(body["selection_id"].clone()).unwrap();
    let (endpoint, cap, charged): (Uuid, String, i64) = sqlx::query_as("SELECT candidate_id,capability,charged_micro_usd FROM user_selected_apis WHERE id=$1").bind(selection).fetch_one(&state.pool).await.unwrap();
    let service = state.fixture["services"].as_array().unwrap().iter().find(|service| service["endpoint_id"] == json!(endpoint) && service["contract"]["capability_slug"] == cap).unwrap();
    let inputs = &body["inputs"];
    let location: Value = serde_json::from_str(include_str!("fixtures/dutch-links/damrak-normalized-chain.json")).unwrap();
    let data = match cap.as_str() {
        "company.registry-profile" => { assert_eq!(inputs["kvkNummer"], "30204462"); service["sample_payload"].clone() }
        "company.financial-statements" => {
            assert_eq!(inputs["kvkNummer"], "30204462");
            serde_json::from_str::<Value>(include_str!("fixtures/kvk-mollie-available-figures.json")).unwrap()["payload"].clone()
        }
        "location.property-search" => {
            assert_eq!(inputs["q"], "*:*");
            let filter = inputs["fq"].as_str().unwrap();
            for expected in ["straatnaam:\"Damrak\"", "huisnummer:1", "woonplaatsnaam:\"Amsterdam\"", "-huisletter:", "-huisnummertoevoeging:"] { assert!(filter.contains(expected), "{filter}"); }
            location["address"].clone()
        }
        "building.bag-unit" | "location.bag-address-context" => {
            assert_eq!(inputs["vbo"], location["address"]["response"]["docs"][0]["adresseerbaarobject_id"]);
            location[if cap == "building.bag-unit" {"bag-unit"} else {"bag-address"}].clone()
        }
        "building.bag-building" => { assert_eq!(inputs["href"], location["bag-unit"]["matched"]["single_building_href"]); location["building"].clone() }
        "area.cbs-neighbourhood-2025" => { assert_eq!(inputs["code"], location["address"]["response"]["docs"][0]["buurtcode"]); location["neighbourhood"].clone() }
        "procurement.nl-public-contracts" => {
            assert_eq!(inputs, &json!({"search":"\"IT services\"","page":0,"size":20,"sort":"publicatieDatum,desc"}));
            serde_json::from_str::<Value>(include_str!("fixtures/tenderned-it-public-page.json")).unwrap()["response"].clone()
        }
        capability if capability.starts_with("statistics.cbs-bankruptcy-") => {
            assert_eq!(inputs, &service["fixed_params"], "reviewed CBS filters must reach dispatch unchanged");
            let fixture: Value = serde_json::from_str(include_str!("fixtures/cbs-bankruptcy-comparison.json")).unwrap();
            let path = service["path"].as_str().unwrap();
            fixture["sources"].as_array().unwrap().iter().find(|saved| path.ends_with(saved["resource"].as_str().unwrap())).unwrap()["data"].clone()
        }
        _ => panic!("unexpected source call: {cap}"),
    };
    let list = service["list_atomic"].as_i64().unwrap();
    assert_eq!(charged, list + (list * 1500 + 9999) / 10000);
    state.providers.fetch_add(1, Ordering::SeqCst);
    sqlx::query("INSERT INTO balance_charges(id,user_id,idempotency_key,status,charged_micro_usd,actual_provider_micro_usd,provider_price_micro_usd,fee_micro_usd,metadata) SELECT $1,user_id,$2,'charged',$3,$4,$4,$5,metadata FROM user_selected_apis WHERE id=$6")
        .bind(Uuid::new_v4()).bind(body["idempotency_key"].as_str().unwrap()).bind(charged).bind(list).bind(charged-list).bind(selection).execute(&state.pool).await.unwrap();
    Json(json!({"ok":true,"result":data,"charged_micro_usd":charged,"provider_micro_usd":list,"fee_micro_usd":charged-list,"result_truncated":false}))
}

async fn settle(engine: &Engine, person: &Principal, task: Uuid) -> Response {
    tokio::time::timeout(std::time::Duration::from_secs(45), async {
        loop {
            if !engine.worker_active(task).await.unwrap() { return engine.view(person, task, Uuid::new_v4(), None).await.unwrap() }
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
    }).await.expect("local worker did not finish")
}

#[tokio::test]
#[ignore = "isolated support-db only; recorded source data and local settlement"]
async fn four_existing_examples_complete_one_approval_and_recover_saved_evidence() {
    let database = std::env::var("TEST_DATABASE_URL").unwrap();
    assert!(database.starts_with("postgresql://postgres@127.0.0.1:"), "never run catalog fixture mutations against a remote database");
    let pool = PgPoolOptions::new().max_connections(1).after_connect(|connection, _| Box::pin(async move {
        sqlx::query("SET search_path=public,gateway_v2").execute(connection).await?; Ok(())
    })).connect(&database).await.unwrap();
    let fixture = catalog_fixture();
    seed(&pool, &fixture).await;
    let mock = Mock { pool: pool.clone(), fixture, providers: Arc::new(AtomicUsize::new(0)), models: Arc::new(AtomicUsize::new(0)) };
    let router = Router::new().route("/model", post(model)).route("/functions/v1/use-selection", post(provider)).with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let store = Store { pool: pool.clone() };
    let http = reqwest::Client::new();
    let engine = Arc::new(Engine { store: store.clone(), catalog: CatalogCache::new(pool.clone()),
        parser: Parser::new(http.clone(), format!("{base}/model"), "fixture".into(), "fixture".into())
            .with_ask_v2(apiosk_gateway_v2::ask_v2::Client::new(http.clone(), String::new(), "fixture".into())),
        signer: Signer::new(vec![6;32]).unwrap(), executor: Executor { store: store.clone(), http, supabase_url: base, enabled: true }, app_url:"http://localhost".into() });
    let person = Principal { owner: Uuid::new_v4(), connection: None, workspace: None };
    let loaded = engine.catalog.get().await.unwrap();
    assert!(loaded.rejected.is_empty(), "fixture catalog rejected: {:?}", loaded.rejected);
    assert_eq!(loaded.services.len(), 13);
    for (name, question, steps) in QUESTIONS {
        let before = mock.providers.load(Ordering::SeqCst);
        let request = DiscoverRequest { request_id: Uuid::new_v4(), question: question.into(), state: None, context_delta: Default::default() };
        let priced = engine.discover(&person, request).await.unwrap();
        assert_eq!(priced.status, Status::Ready, "{name}: {priced:?}");
        let task_id = priced.state.as_ref().unwrap().state_ref;
        let quote = priced.proposal.as_ref().unwrap();
        assert_eq!(quote.steps.len(), steps, "{name}");
        assert_eq!(mock.models.load(Ordering::SeqCst), 0, "{name}: literal question must not need model planning");
        engine.start_worker(person.clone(), "fixture".into(), task_id).await.unwrap();
        assert_eq!(mock.providers.load(Ordering::SeqCst), before, "{name}: no dispatch before approval");
        gate::assert_no_approval(&pool, task_id).await;
        let approval = engine.executor.approve(&person, task_id, quote.quote_ref, &quote.max_total_atomic).await.unwrap();
        assert_eq!(engine.executor.approve(&person, task_id, quote.quote_ref, &quote.max_total_atomic).await.unwrap(), approval, "same approval is idempotent");
        engine.start_worker(person.clone(), "fixture".into(), task_id).await.unwrap();
        engine.start_worker(person.clone(), "fixture".into(), task_id).await.unwrap();
        let done = settle(&engine, &person, task_id).await;
        assert_eq!(done.status, Status::Succeeded, "{name}: {done:?}");
        assert_eq!(done.context_view["analysis"]["status"], "completed", "{name}: {done:?}");
        assert!(done.context_view["analysis"]["follow_up_question"].is_null());
        assert!(done.context_view["analysis"]["presentation"]["rows"].as_array().is_some_and(|rows| !rows.is_empty()), "{name}: no useful rendered table");
        assert_eq!(mock.providers.load(Ordering::SeqCst), before + steps, "{name}: exactly quoted calls");
        assert_eq!(mock.models.load(Ordering::SeqCst), 0, "{name}: saved source projection must not call a model");
        let docs = gate::saved_documents(&pool, task_id).await;
        let citations = gate::assert_evidence_complete(&serde_json::to_value(&done).unwrap(), &docs);
        assert!(citations > 10, "{name}: meaningful source citations required");
        gate::assert_one_approval(&pool, task_id, &serde_json::to_value(quote).unwrap()).await;
        gate::assert_cap(&pool, task_id).await;
        gate::assert_single_settlement(&pool, task_id).await;
        let refreshed = engine.view(&person, task_id, Uuid::new_v4(), None).await.unwrap();
        assert_eq!(refreshed.context_view["analysis"], done.context_view["analysis"], "{name}: refresh preserves answer");
        let (revision, mut saved, _) = store.load(person.owner, task_id).await.unwrap();
        let original_results = saved.active_results.clone();
        // A previous synthesis failure and expired quote must recover only the
        // already-purchased evidence, without a new authorization or dispatch.
        saved.analysis = Some(json!({"status":"unavailable","observations":[],"limitations":["fixture synthesis interruption"]}));
        saved.proposal.as_mut().unwrap()["expires_at"] = json!((chrono::Utc::now()-chrono::Duration::hours(1)).to_rfc3339());
        saved.actions.clear();
        let revision = store.save(person.owner, task_id, revision, &saved).await.unwrap();
        let recovery_view = engine.view(&person, task_id, Uuid::new_v4(), None).await.unwrap();
        let action = recovery_view.next_actions.iter().find(|action| action.kind == "retry_analysis").expect("saved unavailable answer exposes a recovery action").action_id;
        let retry = ExecuteRequest { request_id: Uuid::new_v4(), action_id: action,
            state: engine.signer.issue(person.owner, task_id, revision, saved.focus.clone()), quote_ref: Some(quote.quote_ref),
            authorization_ref: None, idempotency_key: action, input: Value::Null };
        let recovered = engine.execute(&person, "fixture", retry.clone()).await.unwrap();
        assert_eq!(recovered.context_view["analysis"], done.context_view["analysis"], "{name}: saved retry must reproduce verified answer");
        let replay = engine.execute(&person, "fixture", retry).await.unwrap();
        assert_eq!(replay.context_view["analysis"], recovered.context_view["analysis"]);
        assert_eq!(store.load(person.owner, task_id).await.unwrap().1.active_results, original_results);
        engine.start_worker(person.clone(), "fixture".into(), task_id).await.unwrap();
        assert_eq!(mock.providers.load(Ordering::SeqCst), before + steps, "{name}: refresh/recovery/replay must not purchase again");
        assert_eq!(mock.models.load(Ordering::SeqCst), 0, "{name}: recovery must use saved evidence only");
        assert_eq!(gate::saved_documents(&pool, task_id).await, docs, "{name}: recovery must not change saved sources");
        gate::assert_one_approval(&pool, task_id, &serde_json::to_value(quote).unwrap()).await;
        gate::assert_single_settlement(&pool, task_id).await;
        if let Ok(directory) = std::env::var("HOMEPAGE_FIXTURE_EXPORT") {
            std::fs::create_dir_all(&directory).unwrap();
            let results: Vec<_> = docs.iter().map(|(id, doc)| { let mut result = doc.clone(); result["result_ref"] = json!(id); result }).collect();
            std::fs::write(std::path::Path::new(&directory).join(format!("{name}.json")), serde_json::to_vec_pretty(&json!({"question":question,"analysis":done.context_view["analysis"],"results":results})).unwrap()).unwrap();
        }
        println!("{name}: {steps} actual-fixture source calls, one approval, {citations} valid citations, zero replay/model calls");
    }
    assert_eq!(mock.providers.load(Ordering::SeqCst), 13);
    assert_eq!(mock.models.load(Ordering::SeqCst), 0);
    server.abort();
    pool.close().await;
}
