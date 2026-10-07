//! Source-resolved company names, an explicit identity choice and combined web
//! research share one approval. All sources, model replies and charges are local
//! fixtures; this suite must never connect to a production database or provider.
#[path = "support/gate.rs"]
mod gate;

use apiosk_gateway_v2::{auth::Principal, catalog::CatalogCache, engine::Engine,
    execution::Executor, parser::Parser, protocol::{DiscoverRequest, ExecuteRequest, Response, Status},
    state::Signer, store::Store};
use axum::{extract::State, routing::post, Json, Router};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::{collections::BTreeMap, sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}}, time::Duration};
use uuid::Uuid;

const QUESTION: &str = "Can you look up Mollie B.V. by name and combine its registered details, available annual financial figures and website information?";
const DUTCH_QUESTION: &str = "Kun je Mollie B.V. op naam opzoeken en de geregistreerde gegevens, beschikbare financiële jaarcijfers en website-informatie combineren?";
const WEBSITE_QUERY: &str = "web.company_website_query";

fn fixture() -> Value {
    let original: Value = serde_json::from_str(include_str!("fixtures/workflows/catalog.json")).unwrap();
    let mut services: BTreeMap<String, Value> = BTreeMap::new();
    for source in original["services"].as_array().unwrap() {
        let cap = source["contract"]["capability_slug"].as_str().unwrap();
        if !(source["api_slug"]=="kvk-dutch-business-register" && matches!(cap,"company_registry.search"|"company.registry-profile"|"company.financial-statements")
            || source["api_slug"]=="linkup" && cap=="web.linkup-answer") { continue; }
        let mut source = source.clone();
        if cap == "company.registry-profile" {
            source["contract"]["version"] = json!(9);
            source["contract"]["output_map"]["produces"]["company.name"] = json!({"path":"$.naam"});
            source["contract"]["output_map"]["produces"][WEBSITE_QUERY] = json!({"adapter":"record.website-query",
                "key":json!({"name":"/naam","identifier":"/kvkNummer","registry":"KVK","pattern":"^[0-9]{8}$"}).to_string()});
            source["optional_outputs"] = json!(["company.profile","company.nl.rsin","company.nl.registered_name","company.nl.sbi_activities",
                "company.nl.main_establishment_id","company.nl.main_establishment_addresses",WEBSITE_QUERY]);
        } else if cap == "web.linkup-answer" {
            source["contract"]["capability_slug"] = json!("web.company-website-search");
            source["contract"]["requires"] = json!([{"fact_type":WEBSITE_QUERY,"subject_role":"subject","optional":false}]);
            source["contract"]["input_map"] = json!({WEBSITE_QUERY:{"name":"q","location":"body"}});
        }
        services.insert(cap.into(), source);
    }
    let mut vocabulary = original["vocabulary"].as_array().unwrap().clone();
    vocabulary.push(json!({"slug":WEBSITE_QUERY,"value_schema":{"type":"string","minLength":1}}));
    json!({"services":services.into_values().collect::<Vec<_>>(),"vocabulary":vocabulary})
}

async fn seed(pool: &PgPool, fixture: &Value) {
    sqlx::raw_sql("DELETE FROM capability_execution_contracts; DELETE FROM api_endpoints; DELETE FROM apis; DELETE FROM capabilities; DELETE FROM fact_types;").execute(pool).await.unwrap();
    for fact in fixture["vocabulary"].as_array().unwrap() {
        sqlx::query("INSERT INTO fact_types VALUES($1,'active',$2,NULL) ON CONFLICT DO NOTHING")
            .bind(fact["slug"].as_str().unwrap()).bind(&fact["value_schema"]).execute(pool).await.unwrap();
    }
    let mut apis = BTreeMap::new();
    for source in fixture["services"].as_array().unwrap() {
        let c = &source["contract"];
        let id: Uuid = serde_json::from_value(source["id"].clone()).unwrap();
        let endpoint: Uuid = serde_json::from_value(source["endpoint_id"].clone()).unwrap();
        let slug = source["api_slug"].as_str().unwrap();
        let api = *apis.entry(slug).or_insert_with(Uuid::new_v4);
        sqlx::query("INSERT INTO capabilities(slug,status) VALUES($1,'active') ON CONFLICT DO NOTHING")
            .bind(c["capability_slug"].as_str().unwrap()).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO apis(id,slug,name,listing_type,status,active,is_verified,discoverable) VALUES($1,$2,$3,'api','active',true,true,true) ON CONFLICT DO NOTHING")
            .bind(api).bind(slug).bind(source["name"].as_str().unwrap()).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO api_endpoints(id,api_id,method,path,price,active) VALUES($1,$2,$3,$4,($5::bigint::numeric/1000000),true)")
            .bind(endpoint).bind(api).bind(source["method"].as_str().unwrap()).bind(source["path"].as_str().unwrap())
            .bind(source["list_atomic"].as_i64().unwrap()).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO capability_execution_contracts(id,endpoint_id,capability_slug,version,status,requires,input_map,output_map,identifies,cardinality,max_fan_out,fixed_params,preconditions,pagination,freshness_seconds,sample_payload,validated_at,effect,price_model) VALUES($1,$2,$3,$4,'active',$5,$6,$7,$8,$9,1,$10,'{}','{}',3600,$11,now(),'read','per_call')")
            .bind(id).bind(endpoint).bind(c["capability_slug"].as_str().unwrap()).bind(c["version"].as_i64().unwrap() as i32)
            .bind(&c["requires"]).bind(&c["input_map"]).bind(&c["output_map"]).bind(c["identifies"].as_str())
            .bind(c["cardinality"].as_str().unwrap()).bind(&source["fixed_params"]).bind(&source["sample_payload"])
            .execute(pool).await.unwrap();
        let optional:Vec<_>=source["optional_outputs"].as_array().into_iter().flatten().map(|fact|json!({"fact_type":fact,"optional":true})).collect();
        sqlx::query("UPDATE capability_execution_contracts SET produces=$1 WHERE id=$2").bind(json!(optional)).bind(id).execute(pool).await.unwrap();
    }
    sqlx::raw_sql("CREATE ROLE service_role").execute(pool).await.unwrap();
    sqlx::raw_sql(include_str!("../supabase/migrations/20260912111106_kvk_annual_accounts_queue.sql")).execute(pool).await.unwrap();
    let annual: Value = serde_json::from_str(include_str!("fixtures/kvk-mollie-available-figures.json")).unwrap();
    for number in ["30204462", "00123456", "99887766"] {
        sqlx::query("INSERT INTO kvk_cache.annual_accounts(kvk_number,financial_year,document_type,version,payload,source_url) VALUES($1,$2,$3,repeat('a',64),$4,$5)")
            .bind(number).bind(annual["financial_year"].as_i64().unwrap() as i32).bind(kvk_accounts::DOCUMENT_TYPE)
            .bind(&annual["payload"]).bind(annual["source_url"].as_str().unwrap()).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO kvk_cache.jobs(kvk_number,status,financial_year,document_type,version,fresh_until) VALUES($1,'succeeded',$2,$3,repeat('a',64),now()+interval '24 hours')")
            .bind(number).bind(annual["financial_year"].as_i64().unwrap() as i32).bind(kvk_accounts::DOCUMENT_TYPE).execute(pool).await.unwrap();
    }
}

#[derive(Clone)]
struct Mock {
    pool: PgPool, fixture: Value,
    calls: Arc<Mutex<Vec<(String, Value)>>>, models: Arc<AtomicUsize>,
}

async fn provider(State(mock): State<Mock>, Json(body): Json<Value>) -> Json<Value> {
    let selection: Uuid = serde_json::from_value(body["selection_id"].clone()).unwrap();
    let (cap, charged): (String, i64) = sqlx::query_as("SELECT capability,charged_micro_usd FROM user_selected_apis WHERE id=$1")
        .bind(selection).fetch_one(&mock.pool).await.unwrap();
    let source = mock.fixture["services"].as_array().unwrap().iter().find(|s| s["contract"]["capability_slug"] == cap).unwrap();
    let inputs = &body["inputs"];
    mock.calls.lock().unwrap().push((cap.clone(), inputs.clone()));
    let data = match cap.as_str() {
        "company_registry.search" if inputs["naam"] == "Single Match Company" => json!({"pagina":1,"totaal":1,"resultaten":[
            {"naam":inputs["naam"],"kvkNummer":"30204462"}]}),
        "company_registry.search" => json!({"pagina":1,"totaal":2,"resultaten":[
            {"naam":inputs["naam"],"kvkNummer":"30204462"},
            {"naam":"Different registered company","kvkNummer":if inputs["naam"]=="Mismatched Company" {"99887766"} else {"00123456"}}]}),
        "company.registry-profile" => {
            let mut data = source["sample_payload"].clone();
            let number = inputs["kvkNummer"].as_str().unwrap();
            let name = if number=="30204462" {"Mollie Registered B.V."} else {"Selected Alternate B.V."};
            data["naam"] = json!(name); data["statutaireNaam"] = json!(name);
            data["kvkNummer"] = json!(if number=="99887766" {"88776655"} else {number});
            data["handelsnamen"] = json!([{"naam":name}]);
            // As in the actual Mollie profile, there is no website to invent.
            assert!(data.pointer("/_embedded/hoofdvestiging/websites").is_none());
            data
        }
        "company.financial-statements" => source["sample_payload"].clone(),
        "web.company-website-search" => {
            let q = inputs["q"].as_str().unwrap();
            assert!(!q.contains("https://") && !q.contains("mollie.com"), "only the selected registry identity may supply the query");
            assert_eq!(inputs["depth"], "standard"); assert_eq!(inputs["outputType"], "sourcedAnswer");
            json!({"answer":"The retrieved company website describes payment services.","sources":[{"name":"Company website","url":"https://source-selected.example.org/products"}]})
        }
        _ => panic!("unexpected source: {cap}"),
    };
    let list = source["list_atomic"].as_i64().unwrap();
    sqlx::query("INSERT INTO balance_charges(id,user_id,idempotency_key,status,charged_micro_usd,actual_provider_micro_usd,provider_price_micro_usd,fee_micro_usd,metadata) SELECT $1,user_id,$2,'charged',$3,$4,$4,$5,metadata FROM user_selected_apis WHERE id=$6")
        .bind(Uuid::new_v4()).bind(body["idempotency_key"].as_str().unwrap()).bind(charged).bind(list).bind(charged-list).bind(selection)
        .execute(&mock.pool).await.unwrap();
    Json(json!({"ok":true,"result":data,"charged_micro_usd":charged,"provider_micro_usd":list,"fee_micro_usd":charged-list,"result_truncated":false}))
}

async fn model(State(mock): State<Mock>, Json(body): Json<Value>) -> Json<Value> {
    mock.models.fetch_add(1, Ordering::SeqCst);
    let input: Value = serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    if input.get("gap").is_some() {
        return Json(json!({"choices":[{"message":{"content":json!({"message":"Which registry identity should be checked?"}).to_string()}}]}));
    }
    let mut observations = Vec::new();
    let mut combined = Vec::new();
    for source in input["sources"].as_array().expect("only saved analysis may call the model") {
        if source.get("document").is_some() {
            // A mismatched profile can leave just the saved registry lookup.
            // Keep its evidence bounded without inventing website findings.
            let pointer = if source["document"]["data"].get("resultaten").is_some() {"/data/resultaten/0/naam"} else {"/data/naam"};
            observations.push(json!({"text":"The registry returned {{a}}.","evidence":[{"result_ref":source["result_ref"],"pointer":pointer}]}));
            continue;
        }
        let records=source["records"].as_array().unwrap();
        let field=|path:&str,key:&str| records.iter().find(|record|record["path"]==path).unwrap()["fields"]
            .as_array().unwrap().iter().find(|field|field["field"]==key).unwrap();
        let financial=|key:&str| records.iter().find(|record|record["fields"].as_array().unwrap().iter()
            .any(|field|field["field"]=="key"&&field["value"]==key)).unwrap()["fields"].as_array().unwrap().iter()
            .find(|field|field["field"]=="value").unwrap();
        let (text,fields)=match source["source"]["path"].as_str().unwrap() {
            "/api/v1/basisprofielen" => ("The registry identifies {{a}} with KVK number {{b}}.",vec![field("/data","naam"),field("/data","kvkNummer")]),
            "/api/v1/hvds/jaarrekeningen/kvknummer" => ("The available filing reports assets of {{a}} for {{b}}.",vec![financial("Assets"),financial("FinancialYear")]),
            "/v1/search" => ("The web source reports {{a}} and cites {{b}}.",vec![field("/data","answer"),field("/data/sources/0","url")]),
            path=>panic!("unexpected analysis source: {path}"),
        };
        let evidence:Vec<_>=fields.iter().map(|field|json!({"fact_id":field["id"]})).collect();
        combined.push(evidence[0].clone());
        observations.push(json!({"text":text,"evidence":evidence}));
    }
    if !combined.is_empty() {observations.push(json!({"text":"The registry and website evidence support these findings within their respective scopes.","evidence":combined}));}
    let claims:Vec<_>=input["requested_parts"].as_array().into_iter().flatten().map(|part|json!({"requirement_id":part["id"],"status":"answered",
        "answered_by":[format!("o{}",observations.len())],"reason":null})).collect();
    Json(json!({"choices":[{"message":{"content":json!({"observations":observations,"limitations":["Website claims are not independent verification of the company."],
        "presentation":{"kind":"text"},"follow_up_question":null,"requirement_status":claims,"source_gaps":[]}).to_string()}}]}))
}

async fn settled(engine: &Engine, person: &Principal, task: Uuid) -> Response {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if !engine.worker_active(task).await.unwrap() {return engine.view(person,task,Uuid::new_v4(),None).await.unwrap()}
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("local worker must settle")
}

#[tokio::test]
#[ignore = "isolated support-db only; mock providers and model"]
async fn selected_registry_identity_controls_all_dependent_calls_under_one_approval() {
    let database = std::env::var("TEST_DATABASE_URL").unwrap();
    assert!(database.starts_with("postgresql://postgres@127.0.0.1:"),"never seed a remote database");
    let pool=PgPoolOptions::new().max_connections(1).after_connect(|conn,_|Box::pin(async move {
        sqlx::query("SET search_path=public,gateway_v2").execute(conn).await?;Ok(())
    })).connect(&database).await.unwrap();
    let fixture=fixture(); seed(&pool,&fixture).await;
    let mock=Mock {pool:pool.clone(),fixture,calls:Default::default(),models:Default::default()};
    let router=Router::new().route("/model",post(model)).route("/functions/v1/use-selection",post(provider)).with_state(mock.clone());
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base=format!("http://{}",listener.local_addr().unwrap());
    let server=tokio::spawn(async move {axum::serve(listener,router).await.unwrap()});
    let store=Store {pool:pool.clone()};let http=reqwest::Client::new();
    let engine=Arc::new(Engine {store:store.clone(),catalog:CatalogCache::new(pool.clone()),
        parser:Parser::new(http.clone(),format!("{base}/model"),"fixture".into(),"fixture".into())
            .with_ask_v2(apiosk_gateway_v2::ask_v2::Client::new(http.clone(),String::new(),"fixture".into())),
        signer:Signer::new(vec![8;32]).unwrap(),executor:Executor {store:store.clone(),http,supabase_url:base,enabled:true},app_url:"http://localhost".into()});
    let catalog=engine.catalog.get().await.unwrap();assert!(catalog.rejected.is_empty(),"{:?}",catalog.rejected);assert_eq!(catalog.services.len(),4);
    let person=Principal {owner:Uuid::new_v4(),connection:None,workspace:None};
    for (name,selected,form) in [("Mollie B.V.","00123456",QUESTION),("Editable Company B.V.","30204462",QUESTION),
        ("Nederlands Bedrijf B.V.","30204462",DUTCH_QUESTION),("Single Match Company","30204462",QUESTION),
        ("Mismatched Company","99887766",QUESTION)] {
        let before=mock.calls.lock().unwrap().len();let models_before=mock.models.load(Ordering::SeqCst);
        let question=form.replace("Mollie B.V.",name);
        let priced=engine.discover(&person,DiscoverRequest {request_id:Uuid::new_v4(),question,state:None,context_delta:Default::default()}).await.unwrap();
        assert_eq!(priced.status,Status::Ready,"{priced:?}");
        let task=priced.state.as_ref().unwrap().state_ref;let quote=priced.proposal.clone().unwrap();
        assert_eq!(quote.steps.len(),4);assert_eq!(mock.models.load(Ordering::SeqCst),models_before,"no model chooses company or query");
        gate::assert_no_approval(&pool,task).await;assert_eq!(mock.calls.lock().unwrap().len(),before);
        engine.executor.approve(&person,task,quote.quote_ref,&quote.max_total_atomic).await.unwrap();
        engine.start_worker(person.clone(),"fixture".into(),task).await.unwrap();
        let choice=settled(&engine,&person,task).await;
        assert_eq!(mock.calls.lock().unwrap()[before],("company_registry.search".into(),json!({"naam":name})));
        let done=if name=="Single Match Company" {
            assert_eq!(choice.status,Status::Succeeded,"an unambiguous registry result needs no extra identity question: {choice:?}");
            choice
        } else {
            assert_eq!(choice.status,Status::NeedsSelection,"{choice:?}");
            assert_eq!(mock.calls.lock().unwrap().len(),before+1,"only name search before explicit identity choice");
            let candidate=choice.context_view["candidates"].as_array().unwrap().iter().find(|candidate|
                candidate["facts"].as_array().unwrap().iter().any(|fact|fact["type"]=="company_registry.kvknummer"&&fact["value"]==selected)).unwrap();
            let action=choice.next_actions.iter().find(|a|a.kind=="select_entity").unwrap();
            let choose=ExecuteRequest {request_id:Uuid::new_v4(),action_id:action.action_id,idempotency_key:action.action_id,state:choice.state.clone().unwrap(),
                quote_ref:Some(quote.quote_ref),authorization_ref:None,input:json!({"entity_ref":candidate["entity_ref"]})};
            engine.execute(&person,"fixture",choose).await.unwrap();
            engine.start_worker(person.clone(),"fixture".into(),task).await.unwrap();
            settled(&engine,&person,task).await
        };
        let calls=mock.calls.lock().unwrap()[before..].to_vec();
        assert_eq!(calls.iter().filter(|(cap,_)|cap=="company_registry.search").count(),1,"selection reuses saved lookup");
        for (_,inputs) in calls.iter().filter(|(cap,_)|matches!(cap.as_str(),"company.registry-profile"|"company.financial-statements")) {assert_eq!(inputs["kvkNummer"],selected);}
        assert_eq!(done.proposal.as_ref().unwrap().quote_ref,quote.quote_ref);
        gate::assert_one_approval(&pool,task,&serde_json::to_value(&quote).unwrap()).await;
        gate::assert_cap(&pool,task).await;gate::assert_single_settlement(&pool,task).await;
        if name=="Mismatched Company" {
            assert!(calls.iter().all(|(cap,_)|cap!="web.company-website-search"),"mismatched KVK profile must never seed paid website research");
            assert!(!done.errors.is_empty() || done.context_view["analysis"]["status"]!="completed");
            continue;
        }
        assert_eq!(done.status,Status::Succeeded,"{done:?}");assert_eq!(calls.len(),4);
        let query=&calls.iter().find(|(cap,_)|cap=="web.company-website-search").unwrap().1["q"];
        let source_name=if selected=="30204462" {"Mollie Registered B.V."} else {"Selected Alternate B.V."};
        assert_eq!(query,&json!(format!("{} KVK {selected} official website products services",json!(source_name))));
        assert_eq!(done.context_view["analysis"]["status"],"completed","{done:?}");
        assert!(done.context_view["analysis"]["observations"].to_string().contains("https://source-selected.example.org/products"));
        assert!(gate::assert_all(&pool,task,&serde_json::to_value(&quote).unwrap(),&serde_json::to_value(&done).unwrap()).await>=4);
        let saved=gate::saved_documents(&pool,task).await;assert_eq!(saved.len(),4);
        let reload=engine.view(&person,task,Uuid::new_v4(),None).await.unwrap();
        assert_eq!(reload.context_view["analysis"],done.context_view["analysis"]);
        engine.start_worker(person.clone(),"fixture".into(),task).await.unwrap();
        assert_eq!(mock.calls.lock().unwrap().len(),before+4,"reload must never repurchase");
    }
    server.abort();pool.close().await;
}
