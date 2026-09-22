//! Local durable engine + real HTTP boundary + public source adapters.
//! Billing/model are explicit fixtures; optional live upstream reads cost no Apiosk balance.
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
fn fallback_case() -> bool { std::env::var("RESEARCH_FALLBACK_TEST").as_deref()==Ok("1") }
fn fixture() -> Value {
    let mut f:Value=serde_json::from_str(include_str!("fixtures/research-report-catalog.json")).unwrap();
    if fallback_case() {
        let mut browser=f["services"][0].clone();
        browser["id"]=json!("918c4f18-ce21-4dfb-bf4c-0b4d03df7771"); browser["endpoint_id"]=json!("918c4f18-ce21-4dfb-bf4c-0b4d03df7772");
        browser["api_slug"]=json!("browserbase"); browser["name"]=json!("Browserbase"); browser["path"]=json!("/search"); browser["method"]=json!("POST"); browser["list_atomic"]=json!(10000);
        browser["optional_outputs"]=json!([]); browser["fixed_params"]=json!({}); browser["pagination"]=json!({});
        browser["contract"]=json!({"capability_slug":"web.browserbase-search","version":1,"cardinality":"one","requires":[{"fact_type":"web.source_search_query","subject_role":"subject","optional":false}],"input_map":{"web.source_search_query":{"location":"body","name":"query"}},"output_map":{"base":"$","produces":{"web.browserbase_search_page":{"path":"$"}}},"max_fan_out":1});
        browser["sample_payload"]=json!({"results":[{"title":"Mollie B.V. annual report","url":"https://example.test/annual-report","description":"Synthetic public report evidence"}]});
        f["services"].as_array_mut().unwrap().push(browser);
        for (slug,schema) in [("company.name",json!({"type":"string"})),("web.source_search_query",json!({"type":"string"})),("web.browserbase_search_page",json!({"type":"object"}))] {
            if !f["vocabulary"].as_array().unwrap().iter().any(|v|v["slug"]==slug) {f["vocabulary"].as_array_mut().unwrap().push(json!({"slug":slug,"value_schema":schema}));}
        }
    }
    f
}
async fn model(Json(body): Json<Value>) -> Json<Value> {
    let input: Value =
        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    if input.get("answer_operations").is_some() {
        assert!(input["answer_operations"][0]
            .as_str()
            .unwrap()
            .contains("PDF"));
        let sources = input["sources"].as_array().unwrap();
        assert_eq!(sources.len(), if fallback_case() {4} else {3});
        if fallback_case() {assert!(sources.iter().any(|s|s["document"]["source"]["provider"]=="browserbase"));}
        let observations:Vec<_>=sources.iter().filter(|s|s["document"]["coverage"]!="empty" && s["document"]["coverage"]!="partial").map(|s| {
            let d=&s["document"]["data"];
            let pointer=if s["document"]["source"]["provider"]=="browserbase" {"/data/results/0/title"} else if d.get("opendataFields").is_some() {"/data/opendataFields/0/value"} else if d.get("_embedded").is_some() {"/data/_embedded/hoofdvestiging/adressen/0/postcode"} else {"/data/items/0/municipality"};
            json!({"text":"De bron onderbouwt dit onderdeel van het bedrijfsbeeld.","evidence":[{"result_ref":s["result_ref"],"pointer":pointer}]})
        }).collect();
        return Json(
            json!({"choices":[{"message":{"content":json!({"observations":observations,"limitations":["Locatiegegevens bewijzen geen eigendom van het bedrijf."]}).to_string()}}]}),
        );
    }
    let mut intent=json!({"subjects":[{"entity_ref":null,"label":"30204462","values":{"company_registry.kvknummer":"30204462"}}],"goals":[{"capability":"company.financial-statements","subject":0,"constraints":{"source":"kvk-dutch-business-register"}},{"capability":"company.registry-profile","subject":0,"constraints":{"source":"kvk-dutch-business-register"}},{"capability":"company.registered-address-context","subject":0,"constraints":{"source":"pdok"}}],"answer_operations":["Vat de jaarrekening, het profiel en de locaties samen en analyseer ze in een PDF."],"unresolved_references":[]});
    if fallback_case() { intent["subjects"][0]["label"]=json!("Mollie B.V."); intent["subjects"][0]["values"]["company.name"]=json!("Mollie B.V."); intent["goals"][0]["constraints"]=json!({}); }
    Json(json!({"choices":[{"message":{"content":intent.to_string()}}]}))
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
    if source == "pdok" {
        assert_eq!(
            body["inputs"]["addresses"],
            f["services"][0]["sample_payload"]["_embedded"]["hoofdvestiging"]["adressen"]
        );
    }
    let data = if fallback_case() && service["contract"]["capability_slug"]=="company.financial-statements" {json!({"filings":[]})} else { service["sample_payload"].clone() };
    if source=="browserbase" { assert!(body["inputs"]["query"].as_str().unwrap().contains("Mollie B.V. official annual report")); }
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
async fn profile_accounts_addresses_analysis_and_pdf_share_one_approval() {
    let db = std::env::var("TEST_DATABASE_URL").expect("isolated database required");
    assert!(
        db.contains("127.0.0.1:"),
        "This test must never seed a remote database"
    );
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .after_connect(|c, _| {
            Box::pin(async move {
                sqlx::query("SET search_path=public,gateway_v2")
                    .execute(c)
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
        sqlx::query("INSERT INTO capabilities(slug,status) VALUES($1,'active')")
            .bind(c["capability_slug"].as_str().unwrap())
            .execute(&pool)
            .await
            .unwrap();
        if endpoints.insert(endpoint) {
            sqlx::query("INSERT INTO apis(id,slug,name,listing_type,status,active,is_verified,discoverable) VALUES($1,$2,$3,'api','active',true,true,true)").bind(endpoint).bind(service["api_slug"].as_str().unwrap()).bind(service["name"].as_str().unwrap()).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO api_endpoints(id,api_id,method,path,price,active) VALUES($1,$1,'GET',$2,($3::bigint::numeric/1000000),true)")
                .bind(endpoint)
                .bind(service["path"].as_str().unwrap())
                .bind(service["list_atomic"].as_i64().unwrap())
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query("INSERT INTO capability_execution_contracts(id,endpoint_id,capability_slug,version,status,requires,input_map,output_map,identifies,cardinality,max_fan_out,fixed_params,preconditions,pagination,freshness_seconds,sample_payload,validated_at,effect,price_model) VALUES($1,$2,$3,1,'active',$4,$5,$6,$10,$11,1,$7,'{}',$8,3600,$9,now(),'read','per_call')")
            .bind(id).bind(endpoint).bind(c["capability_slug"].as_str().unwrap()).bind(&c["requires"]).bind(&c["input_map"]).bind(&c["output_map"]).bind(&service["fixed_params"]).bind(&service["pagination"]).bind(&service["sample_payload"]).bind(c["identifies"].as_str()).bind(c["cardinality"].as_str().unwrap()).execute(&pool).await.unwrap();
    }
    for service in f["services"].as_array().unwrap() {
        let optional = service["optional_outputs"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let produces: Vec<_> = optional
            .into_iter()
            .map(|fact| json!({"fact_type":fact,"optional":true}))
            .collect();
        sqlx::query("UPDATE capability_execution_contracts SET produces=$1 WHERE id=$2")
            .bind(json!(produces))
            .bind(serde_json::from_value::<Uuid>(service["id"].clone()).unwrap())
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::raw_sql("CREATE ROLE service_role;")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!(
        "../supabase/migrations/20260912111106_kvk_annual_accounts_queue.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let filing = &f["services"][2]["sample_payload"];
    sqlx::query("INSERT INTO kvk_cache.annual_accounts(kvk_number,financial_year,version,payload,source_url) VALUES('30204462',2024,repeat('a',64),$1,'fixture')").bind(filing).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO kvk_cache.jobs(kvk_number,status,financial_year,document_type,version,fresh_until) VALUES('30204462','succeeded',2024,'kvk-hvds-xbrl-v1',repeat('a',64),now()+interval '24 hours')").execute(&pool).await.unwrap();
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
    println!(
        "catalog rejected: {:?}",
        engine.catalog.get().await.unwrap().rejected
    );
    println!(
        "catalog services: {:?}",
        engine
            .catalog
            .get()
            .await
            .unwrap()
            .services
            .iter()
            .map(|s| &s.contract.capability_slug)
            .collect::<Vec<_>>()
    );
    let start=engine.discover(&person,DiscoverRequest{request_id:Uuid::new_v4(),question:"Voor Mollie B.V., KVK 30204462: jaarrekening, bedrijfsprofiel en PDOK voor de profieladressen. Vat samen en analyseer in een PDF.".into(),state:None,context_delta:Default::default()}).await.unwrap();
    assert_eq!(start.status, Status::Ready, "{start:?}");
    let quote = start.proposal.as_ref().unwrap();
    assert_eq!(quote.steps.len(), if fallback_case() {4} else {3});
    assert_eq!(quote.max_total_atomic, if fallback_case() {"250631"} else {"239131"});
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
    let mut response = start.clone();
    for _ in 0..if fallback_case() {4} else {3} {
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
    }
    assert_eq!(response.status, Status::Running);
    assert!(response.context_view.get("report").is_none());
    let action = response
        .next_actions
        .iter()
        .find(|a| a.kind == "poll")
        .unwrap();
    response = engine
        .execute(
            &person,
            "fixture",
            ExecuteRequest {
                request_id: Uuid::new_v4(),
                action_id: action.action_id,
                state: response.state.clone().unwrap(),
                quote_ref: Some(quote.quote_ref),
                authorization_ref: None,
                idempotency_key: action.action_id,
                input: Value::Null,
            },
        )
        .await
        .unwrap();
    assert_eq!(response.status, if fallback_case() {Status::Partial} else {Status::Succeeded}, "{response:?}");
    assert_eq!(
        response.context_view["results"].as_array().unwrap().len(),
        if fallback_case() {4} else {3}
    );
    assert!(response.context_view["report"]["download_path"].is_string());
    let (_, saved, _) = engine
        .store
        .load(person.owner, start.state.as_ref().unwrap().state_ref)
        .await
        .unwrap();
    let document = engine
        .research_document(
            person.owner,
            start.state.as_ref().unwrap().state_ref,
            &saved,
            quote.quote_ref,
        )
        .await
        .unwrap();
    let pdf = apiosk_gateway_v2::research_report::render(&document).unwrap();
    assert!(pdf.starts_with(b"%PDF-"));
    assert!(response.context_view["report"]["evidence_download_path"].is_string());
    assert!(!serde_json::to_string(&response)
        .unwrap()
        .contains("data_base64"));
    for _ in 0..2 {
        let bundle = apiosk_gateway_v2::research_report::evidence_bundle(&document).unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bundle)).unwrap();
        assert!(archive.by_name("report.pdf").is_ok());
        assert!(archive.by_name("manifest.json").is_ok());
        assert!(archive.by_name("sources/S01.json").is_ok());
    }

    assert_eq!(state.count.load(Ordering::SeqCst), if fallback_case() {4} else {3});
    let authorizations: i64 = sqlx::query_scalar("SELECT count(*) FROM gateway_v2.authorizations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(authorizations, 1);
    let fetches: i64 = sqlx::query_scalar("SELECT count(*) FROM kvk_cache.fetch_attempts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        fetches, 0,
        "saved annual account must not cause a new source fetch"
    );
    std::fs::create_dir_all("output/research-report").unwrap();
    std::fs::write("output/research-report/fixture.pdf", pdf).unwrap();
    std::fs::write(
        "output/research-report/fixture.json",
        serde_json::to_vec_pretty(&document).unwrap(),
    )
    .unwrap();
    println!("Research flow: one approval, profile-address binding, saved filing, zero duplicate calls, analysis and combined PDF passed. Model/provider/billing are fixtures.");
    server.abort();
    pool.close().await;
}
