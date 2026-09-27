//! Supplier payment check through the durable engine: preset expansion, one
//! approval, six source calls, deterministic verdict and invoice history.
//! Model, billing and provider responses are fixtures; contracts are the real
//! production contracts exported by examples/check-supplier-payment.rs.
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
    count: Arc<AtomicUsize>,
}
fn fixture() -> Value {
    let mut catalog: Value = serde_json::from_str(include_str!("fixtures/supplier-check/catalog.json")).unwrap();
    let iban: Value = serde_json::from_str(include_str!("fixtures/iban-check/services.json")).unwrap();
    // The first-party IBAN check replaces Sirenic in the supplier check.
    catalog["services"].as_array_mut().unwrap().retain(|s| s["api_slug"] != "sirenic");
    catalog["services"].as_array_mut().unwrap().extend(iban["services"].as_array().unwrap().iter().cloned());
    catalog["vocabulary"].as_array_mut().unwrap().extend(iban["vocabulary"].as_array().unwrap().iter().cloned());
    catalog
}
async fn model(Json(body): Json<Value>) -> Json<Value> {
    let input: Value = serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    if input["question"].as_str().unwrap().contains("klopt deze factuur") {
        // The model misreads the request; code must still route the invoice check.
        return Json(json!({"choices":[{"message":{"content":"not json"}}]}));
    }
    assert!(input["question"].as_str().unwrap().contains("veilig betalen"));
    assert!(input["catalog"]["workflow_presets"][0]["capability"] == "payment.supplier-check");
    Json(json!({"choices":[{"message":{"content":json!({"subjects":[{"entity_ref":null,"label":"Mollie B.V.","values":{}}],"goals":[{"capability":"payment.supplier-check","subject":0,"constraints":{}}],"answer_operations":[],"unresolved_references":[]}).to_string()}}]}))
}
async fn provider(State(s): State<TestState>, Json(body): Json<Value>) -> Json<Value> {
    let id: Uuid = serde_json::from_value(body["selection_id"].clone()).unwrap();
    let (endpoint, charged, metadata): (Uuid, i64, Value) = sqlx::query_as("SELECT candidate_id,charged_micro_usd,metadata FROM user_selected_apis WHERE id=$1").bind(id).fetch_one(&s.pool).await.unwrap();
    let f = fixture();
    let service = f["services"].as_array().unwrap().iter().find(|v| v["endpoint_id"] == json!(endpoint)).unwrap();
    let inputs = &body["inputs"];
    let mut data = service["sample_payload"].clone();
    match service["contract"]["capability_slug"].as_str().unwrap() {
        "vat.eu.validate" => {
            data["data"]["name"] = json!("MOLLIE B.V.");
            data["data"]["vat_number"] = json!("815839091B01");
        }
        "company.sanctions-screen" => {
            data["responses"]["q1"]["results"] = json!([]);
            data["responses"]["q1"]["total"] = json!({"value":0,"relation":"eq"});
        }
        "domain.registration" => {
            let domain = inputs["domain"].as_str().unwrap();
            let registered = if domain == "mollie.com" { "2003-08-26T18:10:03Z".to_owned() } else { (chrono::Utc::now() - chrono::Duration::days(12)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true) };
            data["data"]["domain"] = json!(domain);
            data["data"]["registered_at"] = json!(registered);
        }
        "domain.mail-dns" => data["data"]["domain"] = inputs["domain"].clone(),
        "iban.validate" => data["data"]["iban"] = inputs["iban"].clone(),
        _ => {}
    }
    s.count.fetch_add(1, Ordering::SeqCst);
    let provider = service["list_atomic"].as_i64().unwrap();
    // The App's payment function prices from the selection row: 20% for workflow runs.
    let bps = if metadata["fee_bps"] == 2000 && metadata["workflow"] == "supplier-payment-check" { 2000 } else { 1500 };
    let fee = (provider * bps + 9999) / 10000;
    assert_eq!(charged, provider + fee, "selection price must match the plan markup");
    assert_eq!(bps, 2000, "every supplier-check step is a workflow run");
    Json(json!({"ok":true,"charged_micro_usd":provider+fee,"provider_micro_usd":provider,"fee_micro_usd":fee,"result":data,"result_truncated":false}))
}
fn request(r: &Response, kind: &str) -> Option<ExecuteRequest> {
    let a = r.next_actions.iter().find(|a| a.kind == kind)?;
    Some(ExecuteRequest { request_id: Uuid::new_v4(), action_id: a.action_id, state: r.state.clone().unwrap(), quote_ref: r.proposal.as_ref().map(|q| q.quote_ref), authorization_ref: None, idempotency_key: a.action_id, input: Value::Null })
}
async fn check(engine: &Engine, person: &Principal, state: &TestState, question: &str) -> Response {
    let start = engine.discover(person, DiscoverRequest { request_id: Uuid::new_v4(), question: question.into(), state: None, context_delta: Default::default() }).await.unwrap();
    assert_eq!(start.status, Status::Ready, "{start:?}");
    let quote = start.proposal.clone().unwrap();
    let capabilities: HashSet<_> = quote.steps.iter().map(String::as_str).collect();
    for want in ["company.registry-profile", "vat.eu.validate", "company.sanctions-screen", "domain.registration", "domain.mail-dns", "iban.validate"] {
        assert!(capabilities.contains(want), "{want} missing from {:?}", quote.steps);
    }
    let f = fixture();
    let expected: i64 = quote.steps.iter().map(|cap| {
        let list = f["services"].as_array().unwrap().iter().find(|s| s["contract"]["capability_slug"] == cap.as_str()).unwrap()["list_atomic"].as_i64().unwrap();
        list + (list * 2000 + 9999) / 10000
    }).sum();
    assert_eq!(quote.max_total_atomic, expected.to_string(), "the quote carries the 20% workflow markup");
    let before = state.count.load(Ordering::SeqCst);
    let refused = engine.execute(person, "fixture", request(&start, "execute_quoted_step").unwrap()).await.unwrap();
    assert_ne!(refused.status, Status::Succeeded);
    assert_eq!(state.count.load(Ordering::SeqCst), before, "no call before approval");
    engine.executor.approve(person, start.state.as_ref().unwrap().state_ref, quote.quote_ref, &quote.max_total_atomic).await.unwrap();
    let mut response = start;
    while let Some(next) = request(&response, "execute_quoted_step") {
        response = engine.execute(person, "fixture", next).await.unwrap();
    }
    assert_eq!(state.count.load(Ordering::SeqCst), before + 6, "{response:?}");
    let poll = request(&response, "poll").expect("verdict step");
    engine.execute(person, "fixture", poll).await.unwrap()
}

#[tokio::test]
#[ignore = "requires isolated support-db; model, billing and source responses are fixtures"]
async fn supplier_check_holds_payment_when_invoice_details_change() {
    let db = std::env::var("TEST_DATABASE_URL").expect("isolated database required");
    assert!(db.contains("127.0.0.1:"), "This test must never seed a remote database");
    let pool = PgPoolOptions::new().max_connections(1)
        .after_connect(|c, _| Box::pin(async move { sqlx::query("SET search_path=public,gateway_v2").execute(c).await?; Ok(()) }))
        .connect(&db).await.unwrap();
    sqlx::raw_sql("DELETE FROM capability_execution_contracts; DELETE FROM api_endpoints; DELETE FROM apis; DELETE FROM capabilities; DELETE FROM fact_types;").execute(&pool).await.unwrap();
    let f = fixture();
    for w in f["vocabulary"].as_array().unwrap() {
        sqlx::query("INSERT INTO fact_types VALUES($1,'active',$2,NULL)").bind(w["slug"].as_str().unwrap()).bind(&w["value_schema"]).execute(&pool).await.unwrap();
    }
    let mut apis = std::collections::HashMap::new();
    for service in f["services"].as_array().unwrap() {
        let c = &service["contract"];
        let endpoint: Uuid = serde_json::from_value(service["endpoint_id"].clone()).unwrap();
        let id: Uuid = serde_json::from_value(service["id"].clone()).unwrap();
        let slug = service["api_slug"].as_str().unwrap();
        let fresh = !apis.contains_key(slug);
        let api = *apis.entry(slug.to_owned()).or_insert_with(Uuid::new_v4);
        sqlx::query("INSERT INTO capabilities(slug,status) VALUES($1,'active') ON CONFLICT DO NOTHING").bind(c["capability_slug"].as_str().unwrap()).execute(&pool).await.unwrap();
        if fresh {
            sqlx::query("INSERT INTO apis(id,slug,name,listing_type,status,active,is_verified,discoverable) VALUES($1,$2,$3,$4,'active',true,true,true)").bind(api).bind(slug).bind(service["name"].as_str().unwrap()).bind(service["listing_type"].as_str().unwrap()).execute(&pool).await.unwrap();
        }
        sqlx::query("INSERT INTO api_endpoints(id,api_id,method,path,price,active) VALUES($1,$2,$3,$4,($5::bigint::numeric/1000000),true) ON CONFLICT DO NOTHING")
            .bind(endpoint).bind(api).bind(service["method"].as_str().unwrap()).bind(service["path"].as_str().unwrap()).bind(service["list_atomic"].as_i64().unwrap()).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO capability_execution_contracts(id,endpoint_id,capability_slug,version,status,requires,input_map,output_map,identifies,cardinality,max_fan_out,fixed_params,preconditions,pagination,freshness_seconds,sample_payload,validated_at,effect,price_model) VALUES($1,$2,$3,1,'active',$4,$5,$6,$10,$11,1,$7,'{}',$8,3600,$9,now(),'read','per_call')")
            .bind(id).bind(endpoint).bind(c["capability_slug"].as_str().unwrap()).bind(&c["requires"]).bind(&c["input_map"]).bind(&c["output_map"]).bind(&service["fixed_params"]).bind(&service["pagination"]).bind(&service["sample_payload"]).bind(c["identifies"].as_str()).bind(c["cardinality"].as_str().unwrap()).execute(&pool).await.unwrap();
        let produces: Vec<_> = service["optional_outputs"].as_array().cloned().unwrap_or_default().into_iter().map(|fact| json!({"fact_type":fact,"optional":true})).collect();
        sqlx::query("UPDATE capability_execution_contracts SET produces=$1 WHERE id=$2").bind(json!(produces)).bind(id).execute(&pool).await.unwrap();
    }
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let state = TestState { pool: pool.clone(), count: Arc::new(AtomicUsize::new(0)) };
    let mock = Router::new().route("/chat", post(model)).route("/functions/v1/use-selection", post(provider)).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let store = Store { pool: pool.clone() };
    let engine = Engine {
        store: store.clone(),
        catalog: CatalogCache::new(pool.clone()),
        parser: Parser::new(http.clone(), format!("{base}/chat"), "fixture".into(), "fixture".into()),
        signer: Signer::new(vec![3; 32]).unwrap(),
        executor: Executor { store, http, supabase_url: base, enabled: true },
        app_url: "http://localhost:5173".into(),
    };
    let rejected = &engine.catalog.get().await.unwrap().rejected;
    assert!(rejected.is_empty(), "contracts rejected: {rejected:?}");
    let person = Principal { owner: Uuid::new_v4(), connection: None, workspace: None };

    let first = check(&engine, &person, &state, "Kan ik deze leverancier veilig betalen? Factuur INV-2026-0041 van Mollie B.V., KVK 30204462, btw-nummer NL815839091B01, IBAN NL91 ABNA 0417 1643 00, website mollie.com, adres Keizersgracht 126, 1015 CW Amsterdam.").await;
    assert_eq!(first.status, Status::Succeeded, "{first:?}");
    let verdict = &first.context_view["analysis"]["verdict"];
    assert_eq!(first.context_view["analysis"]["kind"], "supplier_payment_check");
    assert_eq!(verdict["decision"], "ok", "{verdict:#}");
    assert_eq!(verdict["headline"], "✅ GEEN RODE VLAGGEN GEVONDEN");
    let checks = verdict["checks"].as_array().unwrap();
    for (id, status) in [("company_exists", "pass"), ("name_vat_match", "pass"), ("address_match", "pass"), ("domain_age", "pass"), ("sanctions_pep", "pass"), ("iban_valid", "pass"), ("account_holder", "unknown"), ("invoice_history", "unknown")] {
        assert!(checks.iter().any(|c| c["id"] == id && c["status"] == status), "{id} should be {status}: {verdict:#}");
    }

    let second = check(&engine, &person, &state, "Kan ik deze leverancier veilig betalen? Factuur INV-2026-0042 van Mollie B.V., KVK 30204462, btw-nummer NL815839091B01, IBAN NL02 ABNA 0123 4567 89, website mollie-betalingen.nl, adres Damrak 1, 1012 LG Amsterdam.").await;
    let verdict = &second.context_view["analysis"]["verdict"];
    assert_eq!(verdict["decision"], "hold", "{verdict:#}");
    assert_eq!(verdict["headline"], "⚠️ BETALING TEGENHOUDEN");
    let reasons: Vec<&str> = verdict["reasons"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(reasons[0].starts_with("IBAN is gewijzigd"), "{reasons:?}");
    assert!(reasons.iter().any(|r| r.contains("geregistreerd 12 dagen geleden")), "{reasons:?}");
    assert!(reasons[1].starts_with("Domein mollie-betalingen.nl geregistreerd 12 dagen geleden"), "{reasons:?}");
    assert!(reasons[2].starts_with("Factuuradres wijkt af van officiële registratie"), "{reasons:?}");
    assert!(!second.context_view["analysis"].to_string().contains("NL02ABNA0123456789"), "full IBAN must not be echoed in the verdict");
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM gateway_v2.supplier_invoice_checks WHERE owner_id=$1").bind(person.owner).fetch_one(&pool).await.unwrap();
    assert_eq!(rows, 2);
    let authorizations: i64 = sqlx::query_scalar("SELECT count(*) FROM gateway_v2.authorizations").fetch_one(&pool).await.unwrap();
    assert_eq!(authorizations, 2);

    // A model failure on explicit invoice wording still reaches the preset.
    let routed = check(&engine, &person, &state, "Hoi, klopt deze factuur van Mollie B.V.? KVK 30204462, btw NL815839091B01, IBAN NL91 ABNA 0417 1643 00, website mollie.com, 1015 CW Amsterdam, Keizersgracht 126").await;
    assert_eq!(routed.context_view["analysis"]["kind"], "supplier_payment_check", "{routed:?}");

    // Workflows open in Ask: the starter question gets an upload action without a
    // model call; the question read from the invoice runs the normal flow.
    let starter = engine.chat(&person, DiscoverRequest { request_id: Uuid::new_v4(), question: "Kan ik deze leverancier veilig betalen?".into(), state: None, context_delta: Default::default() }).await.unwrap();
    assert_eq!(starter.status, Status::NeedsInput, "{starter:?}");
    let upload = starter.next_actions.iter().find(|a| a.kind == "upload_document").expect("upload action");
    assert_eq!(upload.input_schema["x-purpose"], "invoice");
    assert!(!upload.requires_authorization);
    assert_eq!(starter.context_view["upload_request"]["workflow"], "supplier-payment-check");
    assert!(starter.context_view["conversation"][0]["reply"].as_str().unwrap().starts_with("Upload de factuur"));
    let refused = engine.execute(&person, "fixture", ExecuteRequest { request_id: Uuid::new_v4(), action_id: upload.action_id, state: starter.state.clone().unwrap(), quote_ref: None, authorization_ref: None, idempotency_key: upload.action_id, input: Value::Null }).await.unwrap();
    assert_eq!(refused.status, Status::Failed, "an upload is never executed by the gateway");
    let read_question = "Kan ik deze leverancier veilig betalen? Factuur INV-2026-0043 van Mollie B.V., KVK 30204462, btw-nummer NL815839091B01, IBAN NL91 ABNA 0417 1643 00, website mollie.com, adres Keizersgracht 126, 1015 CW Amsterdam.";
    let quoted = engine.chat(&person, DiscoverRequest { request_id: Uuid::new_v4(), question: read_question.into(), state: starter.state.clone(), context_delta: Default::default() }).await.unwrap();
    assert_eq!(quoted.status, Status::Ready, "{quoted:?}");
    assert!(quoted.next_actions.iter().all(|a| a.kind != "upload_document"), "a new question replaces the upload action");
    let quote = quoted.proposal.clone().unwrap();
    engine.executor.approve(&person, quoted.state.as_ref().unwrap().state_ref, quote.quote_ref, &quote.max_total_atomic).await.unwrap();
    let mut response = quoted;
    while let Some(next) = request(&response, "execute_quoted_step") {
        response = engine.execute(&person, "fixture", next).await.unwrap();
    }
    let done = engine.execute(&person, "fixture", request(&response, "poll").unwrap()).await.unwrap();
    assert_eq!(done.context_view["analysis"]["verdict"]["decision"], "ok", "{:#}", done.context_view["analysis"]);
    let history = engine.workflow_runs(&person, Some("supplier-payment-check")).await.unwrap();
    let runs = history["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 4, "every supplier check is saved as a workflow run: {history:#}");
    assert!(runs.iter().all(|r| r["surface"] == "ask" && r["decision"].is_string() && r["task_id"].is_string()));
    let run_id: Uuid = serde_json::from_value(runs[0]["id"].clone()).unwrap();
    let saved = engine.workflow_run(&person, run_id).await.unwrap().unwrap();
    assert_eq!(saved["task"]["context_view"]["analysis"]["kind"], "supplier_payment_check");
    let stranger = Principal { owner: Uuid::new_v4(), connection: None, workspace: None };
    assert!(engine.workflow_run(&stranger, run_id).await.unwrap().is_none());

    std::fs::create_dir_all("output/supplier-check").unwrap();
    std::fs::write("output/supplier-check/hold-verdict.json", serde_json::to_vec_pretty(&second.context_view["analysis"]).unwrap()).unwrap();
    println!("Supplier check flow: 2 invoices, 12 source calls after 2 approvals, OK then HOLD with {reasons:?}. Model/provider/billing are fixtures.");
    server.abort();
}
