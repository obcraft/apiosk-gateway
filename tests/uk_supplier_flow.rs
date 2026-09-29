//! Durable engine acceptance. Local-only PostgreSQL, mock model, mock paid APIs.
//! This verifies approvals/recovery/PDF plumbing, NOT live supplier clearance.
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
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uuid::Uuid;
#[derive(Clone)]
struct TestState {
    pool: PgPool,
    calls: Arc<AtomicUsize>,
    mode: Arc<AtomicUsize>,
}
fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/uk-supplier/catalog.json")).unwrap()
}
async fn model(Json(body): Json<Value>) -> Json<Value> {
    let input: Value =
        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    let reply = if input.get("answer_operations").is_some() {
        let sources = input["sources"].as_array().unwrap();
        assert!(
            sources
                .iter()
                .any(|s| s["document"]["source"]["provider"] == "sirenic"),
            "analysis needs the roster, not only goal results"
        );
        assert!(
            sources
                .iter()
                .any(|s| s["document"]["source"]["provider"] == "tavily"),
            "analysis needs VAT provenance"
        );
        let s = sources
            .iter()
            .find(|s| s["document"]["data"]["company_name"].is_string())
            .unwrap();
        json!({"observations":[{"text":"The registry identifies {{a}}.","evidence":[{"result_ref":s["result_ref"],"pointer":"/data/company_name"}]}],"limitations":["Synthetic acceptance fixture; not a live supplier verification."]})
    } else {
        let intent = json!({"subjects":[{"entity_ref":null,"label":"Anthropic","values":{}}],"goals":[{"capability":"company.uk-supplier-verification","subject":0,"constraints":{}}],"answer_operations":[],"unresolved_references":[]});
        // Every surface reads a question through the Ask chat pass: a
        // concrete request returns its intent together with the reply.
        if input["surface"] == "ask-chat" {
            json!({"message":"I will prepare the supplier checks and show the total price first.","discover":true,"intent":intent})
        } else {
            intent
        }
    };
    Json(json!({"choices":[{"message":{"content":reply.to_string()}}]}))
}
async fn provider(State(s): State<TestState>, Json(body): Json<Value>) -> Json<Value> {
    let selection: Uuid = serde_json::from_value(body["selection_id"].clone()).unwrap();
    let (cap, charged): (String, i64) =
        sqlx::query_as("SELECT capability,charged_micro_usd FROM user_selected_apis WHERE id=$1")
            .bind(selection)
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
    let mut data = service["sample_payload"].clone();
    match cap.as_str() {
        "company.uk-registry-search" => {
            data = json!({"items":[{"title":"ANTHROPIC LIMITED","company_number":"14604577"}]})
        }
        "company.registry-profile" => {
            data["company_name"] = json!("ANTHROPIC LIMITED");
            data["company_number"] = json!("14604577");
        }
        "company.uk-vat-candidate-search" if s.mode.load(Ordering::SeqCst) == 1 => {
            data = json!({"results":[]})
        }
        "company.uk-director-roster" if s.mode.load(Ordering::SeqCst) == 2 => {
            data["tronque"] = json!(true)
        }
        cap if cap.starts_with("company.uk-director-screening-") => {
            let query = &body["inputs"]["queries"]["q1"];
            assert_eq!(query["schema"], "Person");
            assert!(query["properties"]["name"][0]
                .as_str()
                .unwrap()
                .starts_with("Example Director"));
            data["responses"]["q1"]["query"] = query.clone();
        }
        _ => {}
    }
    s.calls.fetch_add(1, Ordering::SeqCst);
    let provider = service["list_atomic"].as_i64().unwrap();
    let fee = (provider * 1500 + 9999) / 10000;
    assert_eq!(charged, provider + fee);
    Json(
        json!({"ok":true,"charged_micro_usd":charged,"provider_micro_usd":provider,"fee_micro_usd":fee,"result":data,"result_truncated":false}),
    )
}
fn action(r: &Response, kind: &str) -> Option<ExecuteRequest> {
    let a = r.next_actions.iter().find(|a| a.kind == kind)?;
    Some(ExecuteRequest {
        request_id: Uuid::new_v4(),
        action_id: a.action_id,
        state: r.state.clone().unwrap(),
        quote_ref: r.proposal.as_ref().map(|q| q.quote_ref),
        authorization_ref: None,
        idempotency_key: a.action_id,
        input: Value::Null,
    })
}
async fn seed(pool: &PgPool) {
    sqlx::raw_sql("DELETE FROM capability_execution_contracts;DELETE FROM api_endpoints;DELETE FROM apis;DELETE FROM capabilities;DELETE FROM fact_types;").execute(pool).await.unwrap();
    let f = fixture();
    for w in f["vocabulary"].as_array().unwrap() {
        sqlx::query("INSERT INTO fact_types VALUES($1,'active',$2,$3)")
            .bind(w["slug"].as_str().unwrap())
            .bind(&w["value_schema"])
            .bind(w["jurisdiction"].as_str())
            .execute(pool)
            .await
            .unwrap();
    }
    let mut apis = std::collections::HashMap::new();
    for s in f["services"].as_array().unwrap() {
        let c = &s["contract"];
        let id: Uuid = serde_json::from_value(s["id"].clone()).unwrap();
        let endpoint: Uuid = serde_json::from_value(s["endpoint_id"].clone()).unwrap();
        let slug = s["api_slug"].as_str().unwrap();
        let fresh = !apis.contains_key(slug);
        let api = *apis.entry(slug.to_owned()).or_insert_with(Uuid::new_v4);
        sqlx::query(
            "INSERT INTO capabilities(slug,status) VALUES($1,'active') ON CONFLICT DO NOTHING",
        )
        .bind(c["capability_slug"].as_str().unwrap())
        .execute(pool)
        .await
        .unwrap();
        if fresh {
            sqlx::query("INSERT INTO apis(id,slug,name,listing_type,status,active,is_verified,discoverable) VALUES($1,$2,$3,$4,'active',true,true,true)").bind(api).bind(slug).bind(s["name"].as_str().unwrap()).bind(s["listing_type"].as_str().unwrap()).execute(pool).await.unwrap();
        }
        sqlx::query("INSERT INTO api_endpoints(id,api_id,method,path,price,active) VALUES($1,$2,$3,$4,($5::bigint::numeric/1000000),true) ON CONFLICT DO NOTHING").bind(endpoint).bind(api).bind(s["method"].as_str().unwrap()).bind(s["path"].as_str().unwrap()).bind(s["list_atomic"].as_i64().unwrap()).execute(pool).await.unwrap();
        let produces: Vec<_> = s["optional_outputs"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|f| json!({"fact_type":f,"optional":true}))
            .collect();
        sqlx::query("INSERT INTO capability_execution_contracts(id,endpoint_id,capability_slug,version,status,requires,input_map,output_map,identifies,cardinality,max_fan_out,fixed_params,preconditions,pagination,freshness_seconds,sample_payload,validated_at,effect,price_model,produces) VALUES($1,$2,$3,1,'active',$4,$5,$6,$10,$11,1,$7,'{}',$8,3600,$9,now(),'read','per_call',$12)")
   .bind(id).bind(endpoint).bind(c["capability_slug"].as_str().unwrap()).bind(&c["requires"]).bind(&c["input_map"]).bind(&c["output_map"]).bind(&s["fixed_params"]).bind(&s["pagination"]).bind(&s["sample_payload"]).bind(c["identifies"].as_str()).bind(c["cardinality"].as_str().unwrap()).bind(json!(produces)).execute(pool).await.unwrap();
    }
}
#[tokio::test]
#[ignore = "isolated local DB; source, model and money responses are fixtures"]
async fn one_approval_bounded_calls_saved_pdf_and_honest_missing_evidence() {
    let db = std::env::var("TEST_DATABASE_URL").unwrap();
    assert!(db.starts_with("postgresql://postgres@127.0.0.1:"));
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
    seed(&pool).await;
    let state = TestState {
        pool: pool.clone(),
        calls: Arc::new(AtomicUsize::new(0)),
        mode: Arc::new(AtomicUsize::new(0)),
    };
    let mock = Router::new()
        .route("/chat", post(model))
        .route("/functions/v1/use-selection", post(provider))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let http = reqwest::Client::new();
    let store = Store { pool: pool.clone() };
    let engine = Arc::new(Engine {
        store: store.clone(),
        catalog: CatalogCache::new(pool.clone()),
        parser: Parser::new(
            http.clone(),
            format!("{base}/chat"),
            "fixture".into(),
            "fixture".into(),
        ),
        signer: Signer::new(vec![3; 32]).unwrap(),
        executor: Executor {
            store,
            http,
            supabase_url: base,
            enabled: true,
        },
        app_url: "http://localhost:5173".into(),
    });
    let person = Principal {
        owner: Uuid::new_v4(),
        connection: None,
        workspace: None,
    };
    for (mode, expected_calls, status, background) in [
        (0, 9, Status::Succeeded, false),
        (0, 9, Status::Succeeded, true),
        (1, 8, Status::Partial, true),
        (2, 7, Status::Partial, true),
    ] {
        state.mode.store(mode, Ordering::SeqCst);
        let before = state.calls.load(Ordering::SeqCst);
        let mut r=engine.discover(&person,DiscoverRequest{request_id:Uuid::new_v4(),question:"We’re onboarding Anthropic as a supplier. Use Apiosk to verify its UK entity: ownership, filed accounts, VAT validity, and director screening.".into(),state:None,context_delta:Default::default()}).await.unwrap();
        assert_eq!(r.status, Status::Ready, "{r:?}");
        let quote = r.proposal.clone().unwrap();
        assert_eq!(quote.steps.len(), 12);
        let blocked = engine
            .execute(
                &person,
                "fixture",
                action(&r, "execute_quoted_step").unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(blocked.status, Status::RequiresApproval);
        assert_eq!(state.calls.load(Ordering::SeqCst), before);
        let task_id = r.state.as_ref().unwrap().state_ref;
        engine
            .executor
            .approve(&person, task_id, quote.quote_ref, &quote.max_total_atomic)
            .await
            .unwrap();
        if background {
            engine
                .start_worker(person.clone(), "fixture".into(), task_id)
                .await
                .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(20), async {
                while engine.worker_active(task_id).await.unwrap() {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            })
            .await
            .expect("backend must finish without browser continuation");
            r = engine
                .view(&person, task_id, Uuid::new_v4(), None)
                .await
                .unwrap();
        } else {
            let mut loops = 0;
            while let Some(next) = action(&r, "execute_quoted_step") {
                r = engine.execute(&person, "fixture", next).await.unwrap();
                loops += 1;
                assert!(loops < 20);
            }
            r = engine
                .execute(
                    &person,
                    "fixture",
                    action(&r, "poll").expect("saved analysis"),
                )
                .await
                .unwrap();
        }
        assert_eq!(r.status, status, "{r:?}");
        assert_eq!(state.calls.load(Ordering::SeqCst), before + expected_calls);
        assert_eq!(r.context_view["report"]["format"], "pdf");
        let (_, task, _) = engine.store.load(person.owner, task_id).await.unwrap();
        let doc = engine
            .research_document(person.owner, task_id, &task, quote.quote_ref)
            .await
            .unwrap();
        let pdf = apiosk_gateway_v2::research_report::render(&doc).unwrap();
        assert!(pdf.starts_with(b"%PDF-"));
        let after = state.calls.load(Ordering::SeqCst);
        engine
            .view(&person, task_id, Uuid::new_v4(), None)
            .await
            .unwrap();
        engine
            .research_document(person.owner, task_id, &task, quote.quote_ref)
            .await
            .unwrap();
        assert_eq!(
            state.calls.load(Ordering::SeqCst),
            after,
            "read/download never repurchase"
        );
        let approvals: i64 =
            sqlx::query_scalar("SELECT count(*) FROM authorizations WHERE task_id=$1")
                .bind(task_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(approvals, 1);
    }
    server.abort();
}
