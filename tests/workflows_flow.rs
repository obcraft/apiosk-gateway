//! Local durable workflow acceptance: real engine, resolver, storage and billing
//! boundary; stored provider payloads and model response are explicit fixtures.
use apiosk_gateway_v2::{
    auth::Principal,
    catalog::CatalogCache,
    engine::Engine,
    execution::Executor,
    parser::Parser,
    protocol::{ContextDelta, DiscoverRequest, ExecuteRequest, Response, Status},
    state::Signer,
    store::Store,
    workflow_catalog::{self, StartRequest},
};
use axum::{extract::State, routing::post, Json, Router};
use base64::Engine as _;
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
    count: Arc<AtomicUsize>,
    model: Arc<AtomicUsize>,
    fail: Arc<AtomicUsize>,
}
fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/workflows/catalog.json")).unwrap()
}
// Explicit loopback-only browser fixture. Never compiled into the Gateway binary.
async fn browser_stats(State(s): State<TestState>) -> Json<Value> {
    let approvals: i64 = sqlx::query_scalar("SELECT count(*) FROM authorizations").fetch_one(&s.pool).await.unwrap();
    let charges: i64 = sqlx::query_scalar("SELECT count(*) FROM balance_charges WHERE status='charged'").fetch_one(&s.pool).await.unwrap();
    Json(json!({"provider_calls":s.count.load(Ordering::SeqCst),"approvals":approvals,"settled_charges":charges,"fixture_only":true}))
}
async fn browser_fixture(request: axum::extract::Request) -> Json<Value> {
    let user = json!({"id":"00000000-0000-4000-8000-000000000001","aud":"authenticated","role":"authenticated","email":"dossier@example.test","email_confirmed_at":"2026-09-23T10:00:00Z","created_at":"2026-09-23T10:00:00Z","app_metadata":{"provider":"email"},"user_metadata":{"onboarded_at":"2026-09-23T10:00:00Z"}});
    let path = request.uri().path();
    if path == "/auth/v1/user" { return Json(user); }
    if path == "/auth/v1/token" {
        let expiry = chrono::Utc::now().timestamp() + 7200;
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let jwt = format!("{}.{}.fixture", b64.encode(r#"{"alg":"HS256","typ":"JWT"}"#), b64.encode(json!({"sub":user["id"],"role":"authenticated","exp":expiry}).to_string()));
        return Json(json!({"access_token":jwt,"refresh_token":"local-browser-fixture","expires_at":expiry,"expires_in":7200,"token_type":"bearer","user":user}));
    }
    if path.ends_with("/workspaces") {return Json(json!({"enabled":true,"spending_enabled":true,"organisations":[],"workspaces":[]}));}
    if path.ends_with("/balance_summary") {return Json(json!({"available_micro_usd":2000000,"held_micro_usd":0}));}
    Json(json!([]))
}
fn scalar(value: &Value, path: String) -> Option<String> {
    match value {
        Value::String(v) if !v.is_empty() => Some(path),
        Value::Number(_) | Value::Bool(_) => Some(path),
        Value::Object(o) => o.iter().find_map(|(k, v)| {
            scalar(
                v,
                format!("{}/{}", path, k.replace('~', "~0").replace('/', "~1")),
            )
        }),
        Value::Array(a) => a
            .iter()
            .enumerate()
            .find_map(|(i, v)| scalar(v, format!("{path}/{i}"))),
        _ => None,
    }
}
async fn model(State(s): State<TestState>, Json(body): Json<Value>) -> Json<Value> {
    s.model.fetch_add(1, Ordering::SeqCst);
    if s.fail.load(Ordering::SeqCst) == 4 {
        return Json(json!({"choices":[{"message":{"content":"{}"}}]}));
    }
    let input: Value =
        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    if input["known_slugs"].is_array() {
        // Every App chat submission, workflow input included, first crosses
        // the Ask capability parse. A workflow's field values are no data
        // request of their own; with no data capability, no source search runs.
        let parsed = json!({"language":"en","subjects":[],"capabilities":[{"slug":"general.conversation","name":"Conversation","description":"Workflow input without a data request of its own.","domain":"general","inputs":[],"outputs":[],"subject_id":null,"source_hint":null,"confidence":0.9}],"deliverable":{"format":"chat","operations":[]}});
        return Json(json!({"choices":[{"message":{"content":parsed.to_string()}}]}));
    }
    if input.get("workflow").is_some() {
        let patch: Value = serde_json::from_str(input["question"].as_str().unwrap()).unwrap_or(json!({}));
        let mut values = input["collected"].as_object().unwrap().clone();
        for (key,value) in patch.as_object().unwrap() { values.insert(key.clone(),value.clone()); }
        let ready = input["fields"].as_array().unwrap().iter().all(|f| f["required"] != true || values.get(f["key"].as_str().unwrap()).is_some_and(|v| !v.is_null() && v.as_str() != Some("")));
        return Json(json!({"choices":[{"message":{"content":json!({"message":if ready {"I'll prepare the price for approval."} else {"Which details should I use?"},"input":patch,"ready":ready}).to_string()}}]}));
    }
    assert_eq!(body["response_format"]["type"], "json_object");
    assert!(body["messages"][0]["content"].as_str().unwrap().contains("twenty-four observations"));
    assert!(
        input.get("answer_operations").is_some(),
        "Named workflows never ask the model to invent their source plan"
    );
    let observations:Vec<_>=input["sources"].as_array().unwrap().iter().take(8).filter_map(|source|{
  let pointer=scalar(&source["document"]["data"],"/data".into())?;
  Some(json!({"text":"The source reports {{a}}.","evidence":[{"result_ref":source["result_ref"],"pointer":pointer}]}))
 }).collect();
    Json(
        json!({"choices":[{"message":{"content":json!({"observations":observations,"limitations":[]}).to_string()}}]}),
    )
}
async fn provider(State(s): State<TestState>, Json(body): Json<Value>) -> Json<Value> {
    let id: Uuid = serde_json::from_value(body["selection_id"].clone()).unwrap();
    let (endpoint,cap,charged,metadata):(Uuid,String,i64,Value)=sqlx::query_as("SELECT candidate_id,capability,charged_micro_usd,metadata FROM user_selected_apis WHERE id=$1").bind(id).fetch_one(&s.pool).await.unwrap();
    let f = fixture();
    let service = f["services"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["endpoint_id"] == json!(endpoint) && v["contract"]["capability_slug"] == cap)
        .unwrap();
    assert_eq!(metadata["fee_bps"], 2000);
    assert!(workflow_catalog::known(
        metadata["workflow"].as_str().unwrap()
    ));
    if cap == "company.sanctions-screen" && matches!(metadata["workflow"].as_str(), Some("european-company-dossier" | "tender-company-dossier")) {
        let country = body["inputs"]["queries"]["q1"]["properties"]["country"][0].as_str().unwrap();
        let (source, path, name_field) = match country {
            "NL" => ("kvk-dutch-business-register", "/api/v1/basisprofielen", "statutaireNaam"),
            "GB" => ("companies-house", "/company", "company_name"),
            "FR" => ("sirenic", "/v1/entreprise/{siren}", "denomination"),
            _ => ("sirenic", "/v1/eu/entreprise/{pays}/{id}", "denomination"),
        };
        let expected = f["services"].as_array().unwrap().iter().find(|s|s["api_slug"] == source && s["path"] == path && s["contract"]["capability_slug"] == "company.registry-profile").unwrap()["sample_payload"][name_field].clone();
        assert_eq!(body["inputs"]["queries"]["q1"]["properties"]["name"][0], expected, "the actual screening request must use the registry-returned name");
    }
    let list = service["list_atomic"].as_i64().unwrap();
    assert_eq!(charged, list + (list * 2000 + 9999) / 10000);
    s.count.fetch_add(1, Ordering::SeqCst);
    if s.fail.load(Ordering::SeqCst) == 1 && service["api_slug"] == "rdw" {
        sqlx::query("INSERT INTO balance_charges(id,user_id,idempotency_key,status,charged_micro_usd) SELECT $1,user_id,$2,'refunded',0 FROM user_selected_apis WHERE id=$3")
   .bind(Uuid::new_v4()).bind(body["idempotency_key"].as_str().unwrap()).bind(id).execute(&s.pool).await.unwrap();
        return Json(json!({"ok":false,"error":"source_unavailable","charged_micro_usd":0}));
    }
    // Settle the local ledger as the production payment adapter does. A source
    // payload alone must not leave a successful browser run with held funds.
    sqlx::query("INSERT INTO balance_charges(id,user_id,idempotency_key,status,charged_micro_usd,actual_provider_micro_usd,provider_price_micro_usd,fee_micro_usd,metadata) SELECT $1,user_id,$2,'charged',$3,$4,$4,$5,metadata FROM user_selected_apis WHERE id=$6")
        .bind(Uuid::new_v4()).bind(body["idempotency_key"].as_str().unwrap()).bind(charged).bind(list).bind(charged-list).bind(id).execute(&s.pool).await.unwrap();
    let mut data = service["sample_payload"].clone();
    // Stored source examples describe unrelated real companies. Keep the fixture
    // identity consistent with this request; production rejects conflicting IDs.
    if service["api_slug"] == "companies-house"
        && cap == "company.registry-profile"
        && s.fail.load(Ordering::SeqCst) != 3
    {
        data["company_number"] = body["inputs"]["company_number"].clone();
    }
    if cap == "location.property-search" && s.fail.load(Ordering::SeqCst) != 2 {
        data["response"]["docs"] = json!([data["response"]["docs"][0].clone()]);
        data["response"]["numFound"] = json!(1);
    }
    Json(
        json!({"ok":true,"result":data,"charged_micro_usd":charged,"provider_micro_usd":list,"fee_micro_usd":charged-list,"result_truncated":false}),
    )
}
fn request(r: &Response) -> Option<ExecuteRequest> {
    let a = r.next_actions.iter().find(|a| {
        matches!(
            a.kind.as_str(),
            "execute_quoted_step" | "poll" | "select_entity"
        )
    })?;
    let input = if a.kind == "select_entity" {
        json!({"entity_ref":r.context_view["candidates"][0]["entity_ref"]})
    } else {
        Value::Null
    };
    Some(ExecuteRequest {
        request_id: Uuid::new_v4(),
        action_id: a.action_id,
        state: r.state.clone().unwrap(),
        quote_ref: r.proposal.as_ref().map(|p| p.quote_ref),
        authorization_ref: None,
        idempotency_key: a.action_id,
        input,
    })
}
#[tokio::test]
#[ignore = "requires isolated support-db; all source responses and billing are fixtures"]
async fn ten_workflows_quote_approve_complete_recover_and_keep_history() {
    let db = std::env::var("TEST_DATABASE_URL").expect("isolated database required");
    assert!(db.contains("127.0.0.1:"));
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
        sqlx::query("INSERT INTO fact_types VALUES($1,'active',$2,NULL)")
            .bind(w["slug"].as_str().unwrap())
            .bind(&w["value_schema"])
            .execute(&pool)
            .await
            .unwrap();
    }
    let mut apis = std::collections::HashMap::new();
    for service in f["services"].as_array().unwrap() {
        let c = &service["contract"];
        let endpoint: Uuid = serde_json::from_value(service["endpoint_id"].clone()).unwrap();
        let id: Uuid = serde_json::from_value(service["id"].clone()).unwrap();
        let slug = service["api_slug"].as_str().unwrap();
        let fresh = !apis.contains_key(slug);
        let api = *apis.entry(slug.to_owned()).or_insert_with(Uuid::new_v4);
        sqlx::query(
            "INSERT INTO capabilities(slug,status) VALUES($1,'active') ON CONFLICT DO NOTHING",
        )
        .bind(c["capability_slug"].as_str().unwrap())
        .execute(&pool)
        .await
        .unwrap();
        if fresh {
            sqlx::query("INSERT INTO apis(id,slug,name,listing_type,status,active,is_verified,discoverable) VALUES($1,$2,$3,$4,'active',true,true,true)").bind(api).bind(slug).bind(service["name"].as_str().unwrap()).bind(service["listing_type"].as_str().unwrap()).execute(&pool).await.unwrap();
        }
        sqlx::query("INSERT INTO api_endpoints(id,api_id,method,path,price,active) VALUES($1,$2,$3,$4,($5::bigint::numeric/1000000),true) ON CONFLICT DO NOTHING")
            .bind(endpoint).bind(api).bind(service["method"].as_str().unwrap()).bind(service["path"].as_str().unwrap()).bind(service["list_atomic"].as_i64().unwrap()).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO capability_execution_contracts(id,endpoint_id,capability_slug,version,status,requires,input_map,output_map,identifies,cardinality,max_fan_out,fixed_params,preconditions,pagination,freshness_seconds,sample_payload,validated_at,effect,price_model) VALUES($1,$2,$3,1,'active',$4,$5,$6,$10,$11,1,$7,'{}',$8,3600,$9,now(),'read','per_call')")
            .bind(id).bind(endpoint).bind(c["capability_slug"].as_str().unwrap()).bind(&c["requires"]).bind(&c["input_map"]).bind(&c["output_map"]).bind(&service["fixed_params"]).bind(&service["pagination"]).bind(&service["sample_payload"]).bind(c["identifies"].as_str()).bind(c["cardinality"].as_str().unwrap()).execute(&pool).await.unwrap();
        let produces: Vec<_> = service["optional_outputs"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|fact| json!({"fact_type":fact,"optional":true}))
            .collect();
        sqlx::query("UPDATE capability_execution_contracts SET produces=$1 WHERE id=$2")
            .bind(json!(produces))
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }

    // Exercise the whole migration twice against the fixture catalog, including
    // creating the new property contract. These public compatibility views match
    // the schema layout used by production's catalog query.
    sqlx::raw_sql("CREATE SCHEMA IF NOT EXISTS catalog; ALTER TABLE public.capabilities ADD COLUMN name text NOT NULL DEFAULT 'Fixture'; ALTER TABLE public.capabilities SET SCHEMA catalog; CREATE VIEW public.capabilities AS SELECT * FROM catalog.capabilities; ALTER TABLE public.capability_execution_contracts SET SCHEMA catalog; CREATE VIEW public.capability_execution_contracts AS SELECT * FROM catalog.capability_execution_contracts; DELETE FROM catalog.capability_execution_contracts WHERE capability_slug='location.property-search';").execute(&pool).await.unwrap();
    let migration =
        include_str!("../supabase/migrations/20260920222320_eleven_workflow_catalog.sql");
    sqlx::raw_sql(migration).execute(&pool).await.unwrap();
    sqlx::raw_sql(migration).execute(&pool).await.unwrap();
    let dossier_migration = include_str!("../supabase/migrations/20260923162344_registry_names_for_european_dossiers.sql");
    sqlx::raw_sql(dossier_migration).execute(&pool).await.unwrap();
    sqlx::raw_sql(dossier_migration).execute(&pool).await.unwrap();
    sqlx::raw_sql("CREATE ROLE service_role").execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!("../supabase/migrations/20260912111106_kvk_annual_accounts_queue.sql")).execute(&pool).await.unwrap();
    let account=f["services"].as_array().unwrap().iter().find(|s|s["contract"]["capability_slug"]=="company.financial-statements").unwrap()["sample_payload"].clone();
    sqlx::query("INSERT INTO kvk_cache.annual_accounts(kvk_number,financial_year,document_type,version,payload,source_url) VALUES('30204462',2024,$1,repeat('a',64),$2,'local fixture')")
        .bind(kvk_accounts::DOCUMENT_TYPE).bind(account).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO kvk_cache.jobs(kvk_number,status,financial_year,document_type,version,fresh_until) VALUES('30204462','succeeded',2024,$1,repeat('a',64),now()+interval '24 hours')")
        .bind(kvk_accounts::DOCUMENT_TYPE).execute(&pool).await.unwrap();
    let state = TestState {
        pool: pool.clone(),
        count: Arc::new(AtomicUsize::new(0)),
        model: Arc::new(AtomicUsize::new(0)),
        fail: Arc::new(AtomicUsize::new(0)),
    };
    let mock = Router::new()
        .route("/chat", post(model))
        .route("/functions/v1/use-selection", post(provider))
        .route("/functions/v1/agent-gateway/v1/run", post(provider))
        .route("/__fixture/stats", axum::routing::get(browser_stats))
        .fallback(browser_fixture)
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(state.clone());
    // Browser mode serves fixed loopback ports the App's dev server points at;
    // the environment may move them so parallel runs do not collide.
    let browser = std::env::var("WORKFLOWS_BROWSER").as_deref() == Ok("true");
    let browser_env = |key: &str, default: &str| std::env::var(key).unwrap_or_else(|_| default.into());
    let fixture_address = if browser {browser_env("WORKFLOWS_FIXTURE_ADDRESS", "127.0.0.1:5284")} else {"127.0.0.1:0".into()};
    let listener = tokio::net::TcpListener::bind(&fixture_address).await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let http = reqwest::Client::new();
    let store = Store { pool: pool.clone() };
    let engine = Engine {
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
    };
    assert!(
        engine.catalog.get().await.unwrap().rejected.is_empty(),
        "{:?}",
        engine.catalog.get().await.unwrap().rejected
    );
    if browser {
        let engine = Arc::new(engine);
        let api_state = apiosk_gateway_v2::server::ServerState {
            engine: engine.clone(),
            auth: Arc::new(apiosk_gateway_v2::auth::Auth {pool:pool.clone(), http:reqwest::Client::new(), supabase_url:engine.executor.supabase_url.clone(), publishable_key:"fixture".into()}),
            permits: Arc::new(tokio::sync::Semaphore::new(8)), executor_compat:None,
        };
        let gateway_address = browser_env("WORKFLOWS_GATEWAY_ADDRESS", "127.0.0.1:5283");
        let listener = tokio::net::TcpListener::bind(&gateway_address).await.unwrap();
        println!("BROWSER READY: real gateway http://{gateway_address}; fixture auth/providers http://{fixture_address}; no live charges");
        axum::serve(listener, apiosk_gateway_v2::server::router(api_state, &browser_env("WORKFLOWS_UI_URL", "http://127.0.0.1:5282")).unwrap()).await.unwrap();
        return;
    }
    let person = Principal {
        owner: Uuid::new_v4(),
        connection: None,
        workspace: None,
    };
    let stranger = Principal {
        owner: Uuid::new_v4(),
        connection: None,
        workspace: None,
    };
    let mut outcomes = vec![];
    let mut views = vec![];
    for recipe in workflow_catalog::recipes() {
        let person = Principal {
            owner: Uuid::new_v4(),
            connection: None,
            workspace: None,
        };
        let before = state.count.load(Ordering::SeqCst);
        let start_request = DiscoverRequest {
            request_id: Uuid::new_v4(), question: format!("Start {}", recipe.name), state: None,
            context_delta: ContextDelta { workflow_slug: Some(recipe.slug.clone()), ..Default::default() },
        };
        let started = engine.chat(&person, start_request.clone()).await.unwrap();
        assert_eq!(started.status, Status::NeedsInput, "{}: {started:?}",recipe.slug);
        assert!(started.errors.is_empty());
        assert!(started.proposal.is_none());
        assert_eq!(started.context_view["workflow"]["slug"], recipe.slug);
        assert_eq!(state.count.load(Ordering::SeqCst), before);
        let models = state.model.load(Ordering::SeqCst);
        let replay = engine.chat(&person, start_request).await.unwrap();
        assert_eq!(replay.state.as_ref().unwrap().state_ref, started.state.as_ref().unwrap().state_ref);
        assert_eq!(state.model.load(Ordering::SeqCst), models);
        let recovered = engine.view(&person, started.state.as_ref().unwrap().state_ref, Uuid::new_v4(), None).await.unwrap();
        let required: Vec<_> = recipe.fields.iter().filter(|f| f.required).collect();
        // Give only one required value, then recover the saved chat before completing it.
        let mut patch = json!({});
        let mut collected = recovered;
        if required.len() > 1 {
            let key = &required[0].key;
            patch[key] = recipe.example[key].clone();
            collected = engine.chat(&person, DiscoverRequest { request_id: Uuid::new_v4(), question: patch.to_string(), state: collected.state, context_delta: ContextDelta::default() }).await.unwrap();
            assert_eq!(collected.status, Status::NeedsInput, "{}: {collected:?}",recipe.slug);
            assert!(collected.proposal.is_none());
            assert!(collected.errors.is_empty());
        }
        let mut remaining = recipe.example.clone();
        if required.len() > 1 { remaining.remove(&required[0].key); }
        if recipe.slug == "customer-onboarding" {
            let mut invalid = remaining.clone();
            invalid.insert("country".into(), json!("Atlantis"));
            collected = engine.chat(&person, DiscoverRequest { request_id: Uuid::new_v4(), question: serde_json::to_string(&invalid).unwrap(), state: collected.state, context_delta: ContextDelta::default() }).await.unwrap();
            assert_eq!(collected.status, Status::NeedsInput);
            assert!(collected.errors.is_empty());
            assert!(collected.proposal.is_none());
        }
        let mut response = engine.chat(&person, DiscoverRequest { request_id: Uuid::new_v4(), question: serde_json::to_string(&remaining).unwrap(), state: collected.state, context_delta: ContextDelta::default() }).await.unwrap();
        assert_eq!(response.status, Status::Ready, "{}: {response:?}",recipe.slug);
        assert_eq!(response.billing.as_ref().unwrap()["apiosk_fee_bps"], 2000);
        if recipe.slug == "prospect-shortlist" {
            let old_quote = response.proposal.as_ref().unwrap().quote_ref;
            response = engine.chat(&person, DiscoverRequest { request_id: Uuid::new_v4(), question: serde_json::to_string(&recipe.example).unwrap(), state: response.state, context_delta: ContextDelta::default() }).await.unwrap();
            assert_eq!(response.status, Status::Ready);
            assert_ne!(response.proposal.as_ref().unwrap().quote_ref,old_quote);
            assert_eq!(response.context_view["workflow"]["slug"],recipe.slug);
            assert_eq!(response.billing.as_ref().unwrap()["apiosk_fee_bps"],2000);
        }
        let quoted = serde_json::to_value(&response).unwrap();
        let task = response.state.as_ref().unwrap().state_ref;
        let quote = response.proposal.clone().unwrap();
        assert_eq!(state.count.load(Ordering::SeqCst), before);
        let stored = engine.store.load(person.owner,task).await.unwrap().1;
        assert_eq!(stored.workflow.as_ref().unwrap().input,recipe.example);
        assert!(!stored.workflow.unwrap().collecting);
        let unapproved = engine
            .execute(&person, "fixture", request(&response).unwrap())
            .await
            .unwrap();
        assert_ne!(unapproved.status, Status::Succeeded);
        assert_eq!(state.count.load(Ordering::SeqCst), before);
        let auth = engine
            .executor
            .approve(&person, task, quote.quote_ref, &quote.max_total_atomic)
            .await
            .unwrap();
        assert_eq!(
            auth,
            engine
                .executor
                .approve(&person, task, quote.quote_ref, &quote.max_total_atomic)
                .await
                .unwrap()
        );
        for _ in 0..30 {
            if matches!(
                response.status,
                Status::Succeeded | Status::Partial | Status::Failed | Status::Cancelled
            ) {
                break;
            }
            let next = request(&response)
                .unwrap_or_else(|| panic!("{} stalled: {response:?}", recipe.slug));
            response = engine
                .execute(&person, "fixture", next.clone())
                .await
                .unwrap();
            let count = state.count.load(Ordering::SeqCst);
            let repeated = engine.execute(&person, "fixture", next).await.unwrap();
            assert_eq!(repeated.state.as_ref().unwrap().state_ref, task);
            assert_eq!(state.count.load(Ordering::SeqCst), count);
        }
        assert_eq!(
            response.status,
            Status::Succeeded,
            "{}: {response:?}",
            recipe.slug
        );
        assert_eq!(
            response.context_view["analysis"]["status"], "completed",
            "{}: {}",
            recipe.slug, response.context_view["analysis"]
        );
        assert!(!response.context_view["analysis"]["observations"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(
            response.context_view["analysis"]["limitations"]
                .as_array()
                .unwrap()
                .len()
                >= recipe.limitations.len()
        );
        let calls = state.count.load(Ordering::SeqCst) - before;
        assert_eq!(calls, quote.steps.len(), "{}", recipe.slug);
        let count = state.count.load(Ordering::SeqCst);
        let recovered = engine
            .view(&person, task, Uuid::new_v4(), None)
            .await
            .unwrap();
        assert_eq!(recovered.status, Status::Succeeded);
        assert_eq!(state.count.load(Ordering::SeqCst), count);
        if recipe.slug == "competitor-comparison" {
            state.fail.store(4, Ordering::SeqCst);
            let action = recovered.next_actions.iter().find(|action| action.kind == "retry_analysis").unwrap();
            let retried = engine.execute(&person, "fixture", ExecuteRequest {
                request_id: Uuid::new_v4(), action_id: action.action_id,
                state: recovered.state.clone().unwrap(), quote_ref: Some(quote.quote_ref),
                authorization_ref: None, idempotency_key: action.action_id, input: Value::Null,
            }).await.unwrap();
            assert_eq!(retried.context_view["analysis"]["observations"], recovered.context_view["analysis"]["observations"]);
            assert_eq!(retried.context_view["analysis"]["status"], "completed");
            assert!(retried.context_view["analysis"]["limitations"].as_array().unwrap().iter().any(|value| value.as_str().unwrap_or_default().contains("previously verified findings")));
            assert_eq!(state.count.load(Ordering::SeqCst), count);
            state.fail.store(0, Ordering::SeqCst);
        }
        let history = engine
            .workflow_runs(&person, Some(&recipe.slug))
            .await
            .unwrap();
        let run = &history["runs"][0];
        assert_eq!(run["status"], "completed");
        assert_eq!(run["surface"], "workflows");
        assert_eq!(
            run["input"]["workflow_title"],
            response.context_view["workflow"]["title"]
        );
        let id: Uuid = serde_json::from_value(run["id"].clone()).unwrap();
        assert!(engine.workflow_run(&stranger, id).await.unwrap().is_none());
        let wrong_workspace = Principal {
            owner: person.owner,
            connection: None,
            workspace: Some(Uuid::new_v4()),
        };
        assert!(engine
            .workflow_run(&wrong_workspace, id)
            .await
            .unwrap()
            .is_none());
        let authorizations: i64 =
            sqlx::query_scalar("SELECT count(*) FROM authorizations WHERE task_id=$1")
                .bind(task)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(authorizations, 1);
        outcomes.push(json!({"workflow":recipe.slug,"operations":calls,"cap_atomic":quote.max_total_atomic,"one_approval":true,"replay_safe":true,"recovery_safe":true,"history":true,"passed":true}));
        views.push(json!({"workflow":recipe.slug,"quoted":quoted,"completed":response}));
        println!(
            "PASS {}: {calls} operations, one approval, saved result",
            recipe.slug
        );
    }
    // Cancellation prevents purchase and appears in history.
    let r = workflow_catalog::recipe("company-news").unwrap();
    let models = state.model.load(Ordering::SeqCst);
    let start = engine
        .start_workflow(
            &person,
            &r.slug,
            StartRequest {
                request_id: Uuid::new_v4(),
                input: r.example,
            },
        )
        .await
        .unwrap();
    // A workflow page's recipe is its plan: its quote needs no model call,
    // not even the Ask capability parse.
    assert_eq!(start.status, Status::Ready, "{start:?}");
    assert_eq!(state.model.load(Ordering::SeqCst), models, "a workflow page start calls no model");
    let task = start.state.as_ref().unwrap().state_ref;
    let cancelled = engine
        .execute(
            &person,
            "fixture",
            ExecuteRequest {
                request_id: Uuid::new_v4(),
                action_id: task,
                state: start.state.unwrap(),
                quote_ref: start.proposal.map(|p| p.quote_ref),
                authorization_ref: None,
                idempotency_key: Uuid::new_v4(),
                input: Value::Null,
            },
        )
        .await
        .unwrap();
    assert_eq!(cancelled.status, Status::Cancelled);
    assert_eq!(
        engine
            .workflow_runs(&person, Some("company-news"))
            .await
            .unwrap()["runs"][0]["status"],
        "cancelled"
    );
    // A worker can stop after payment but before ingestion. Expired approval
    // must still allow receipt recovery, with no replay of either source.
    let r = workflow_catalog::recipe("competitor-comparison").unwrap();
    let start = engine.start_workflow(&person, &r.slug, StartRequest {
        request_id: Uuid::new_v4(), input: r.example,
    }).await.unwrap();
    let task_id = start.state.as_ref().unwrap().state_ref;
    let quote = start.proposal.as_ref().unwrap();
    let auth = engine.executor.approve(&person, task_id, quote.quote_ref, &quote.max_total_atomic).await.unwrap();
    let (revision, mut saved, _) = engine.store.load(person.owner, task_id).await.unwrap();
    let plan: apiosk_gateway_v2::resolver::Plan = serde_json::from_value(saved.proposal.clone().unwrap()).unwrap();
    let service_ids: Vec<_> = plan.steps.iter().map(|step| step.service.id).collect();
    let catalog = engine.catalog.fresh_execution(&service_ids).await.unwrap();
    assert!(!catalog.services.is_empty());
    assert!(catalog.services.iter().all(|service| service_ids.contains(&service.id)));
    for step in &plan.steps {
        assert_eq!(catalog.services.iter().find(|service| service.id == step.service.id).unwrap().version_hash(), step.service.version_hash());
    }
    for (index, step) in plan.steps.iter().enumerate() {
        let action_id = Uuid::new_v4();
        let inputs = step.service.inputs(&saved, step.entity_ref, &catalog.vocabulary).unwrap();
        engine.executor.dispatch(&person, "fixture", task_id, revision, action_id, action_id, auth, &plan, step, &inputs).await.unwrap();
        saved.pending_dispatches.push(apiosk_gateway_v2::state::PendingDispatch { step: index, action_id, inputs });
    }
    saved.actions.clear();
    engine.store.save(person.owner, task_id, revision, &saved).await.unwrap();
    sqlx::query("UPDATE authorizations SET expires_at=now()-interval '1 second' WHERE id=$1").bind(auth).execute(&pool).await.unwrap();
    let before = state.count.load(Ordering::SeqCst);
    let pending_id = saved.pending_dispatches[1].action_id;
    sqlx::query("UPDATE executions SET status='unresolved' WHERE id=$1").bind(pending_id).execute(&pool).await.unwrap();
    let mut recovered = engine.view(&person, task_id, Uuid::new_v4(), None).await.unwrap();
    assert_eq!(recovered.billing.as_ref().unwrap()["authorization_active"], false);
    // No completed call-run receipt exists yet: reading must remain pending.
    let poll_request = |view: &Response, action_id| ExecuteRequest { request_id: Uuid::new_v4(), action_id,
        state: view.state.clone().unwrap(), quote_ref: Some(quote.quote_ref), authorization_ref: None,
        idempotency_key: action_id, input: Value::Null };
    recovered = engine.execute(&person, "fixture", poll_request(&recovered, pending_id)).await.unwrap();
    assert!(recovered.context_view["results"].as_array().unwrap().is_empty());
    assert_eq!(state.count.load(Ordering::SeqCst), before);
    sqlx::query("UPDATE executions SET status='succeeded' WHERE id=$1").bind(pending_id).execute(&pool).await.unwrap();
    // Recover out of order to verify that each result keeps its own plan step.
    for pending in saved.pending_dispatches.iter().rev() {
        assert!(recovered.next_actions.iter().any(|a| a.kind == "poll" && a.action_id == pending.action_id));
        recovered = engine.execute(&person, "fixture", poll_request(&recovered, pending.action_id)).await.unwrap();
        if pending.action_id == pending_id {
            let analysis = recovered.next_actions.iter().find(|action| action.kind == "retry_analysis").unwrap();
            recovered = engine.execute(&person, "fixture", poll_request(&recovered, analysis.action_id)).await.unwrap();
            assert_eq!(recovered.context_view["analysis"]["status"], "partial");
            let (_, in_progress, _) = engine.store.load(person.owner, task_id).await.unwrap();
            assert_eq!(in_progress.pending_dispatches.len(), 1);
            assert_eq!(state.count.load(Ordering::SeqCst), before, "partial analysis must not call a source");
        }
    }
    assert_eq!(recovered.context_view["results"].as_array().unwrap().len(), 2);
    let (_, final_task, _) = engine.store.load(person.owner, task_id).await.unwrap();
    assert!(final_task.pending_dispatches.is_empty());
    assert_eq!(state.count.load(Ordering::SeqCst), before, "recovery must not call a source");
    for result in recovered.context_view["results"].as_array().unwrap() {
        assert!(plan.steps.iter().any(|step| result["subject"]["entity_ref"] == json!(step.entity_ref)));
    }

    // A source failure never turns into a complete or passed check.
    state.fail.store(1, Ordering::SeqCst);
    let r = workflow_catalog::recipe("vehicle-precheck").unwrap();
    let mut response = engine
        .start_workflow(
            &person,
            &r.slug,
            StartRequest {
                request_id: Uuid::new_v4(),
                input: r.example,
            },
        )
        .await
        .unwrap();
    let quote = response.proposal.clone().unwrap();
    let task = response.state.as_ref().unwrap().state_ref;
    engine
        .executor
        .approve(&person, task, quote.quote_ref, &quote.max_total_atomic)
        .await
        .unwrap();
    for _ in 0..20 {
        if matches!(
            response.status,
            Status::Succeeded | Status::Partial | Status::Failed
        ) {
            break;
        }
        response = engine
            .execute(&person, "fixture", request(&response).unwrap())
            .await
            .unwrap();
    }
    assert_eq!(response.status, Status::Partial, "{response:?}");
    assert_eq!(response.context_view["analysis"]["status"], "partial");
    assert_eq!(
        engine
            .workflow_runs(&person, Some("vehicle-precheck"))
            .await
            .unwrap()["runs"][0]["status"],
        "partial"
    );
    // Ambiguous addresses pause before the dependent paid requests. Wrong source
    // identifiers and failed analysis remain visible in history after recovery.
    for (mode, slug) in [
        (2, "property-precheck"),
        (3, "company-ownership"),
        (4, "company-news"),
    ] {
        state.fail.store(mode, Ordering::SeqCst);
        let person = Principal {
            owner: Uuid::new_v4(),
            connection: None,
            workspace: None,
        };
        let r = workflow_catalog::recipe(slug).unwrap();
        let mut response = engine
            .start_workflow(
                &person,
                slug,
                StartRequest {
                    request_id: Uuid::new_v4(),
                    input: r.example,
                },
            )
            .await
            .unwrap();
        let quote = response.proposal.clone().unwrap();
        let task = response.state.as_ref().unwrap().state_ref;
        engine
            .executor
            .approve(&person, task, quote.quote_ref, &quote.max_total_atomic)
            .await
            .unwrap();
        let mut chose = false;
        for _ in 0..25 {
            if matches!(
                response.status,
                Status::Succeeded | Status::Partial | Status::Failed
            ) {
                break;
            }
            if response.status == Status::NeedsSelection {
                assert_eq!(mode, 2);
                assert!(response.context_view["candidates"][0]["label"]
                    .as_str()
                    .unwrap()
                    .contains("Dam"));
                let before = state.count.load(Ordering::SeqCst);
                engine
                    .view(&person, task, Uuid::new_v4(), None)
                    .await
                    .unwrap();
                assert_eq!(state.count.load(Ordering::SeqCst), before);
                chose = true;
            }
            response = engine
                .execute(
                    &person,
                    "fixture",
                    request(&response).unwrap_or_else(|| {
                        panic!(
                            "mode {mode}: {:?}, {:?}",
                            response.status, response.next_actions
                        )
                    }),
                )
                .await
                .unwrap();
        }
        let run = engine.workflow_runs(&person, Some(slug)).await.unwrap()["runs"][0].clone();
        if mode == 2 {
            assert!(chose);
            assert_eq!(response.status, Status::Succeeded);
        }
        if mode == 3 {
            assert_eq!(response.status, Status::Partial);
            assert_eq!(run["status"], "partial");
        }
        if mode == 4 {
            assert_eq!(response.context_view["analysis"]["status"], "unavailable");
            assert_eq!(run["status"], "failed");
            let id = serde_json::from_value(run["id"].clone()).unwrap();
            assert_eq!(
                engine.workflow_run(&person, id).await.unwrap().unwrap()["run"]["status"],
                "failed"
            );
        }
    }
    std::fs::create_dir_all("output/eleven-workflows").unwrap();
    std::fs::write("output/eleven-workflows/local-durable-results.json",serde_json::to_vec_pretty(&json!({"environment":"isolated local database; fixture billing, model and source responses","results":outcomes,"source_failure":true,"cancellation":true})).unwrap()).unwrap();
    std::fs::write(
        "output/eleven-workflows/browser-views.json",
        serde_json::to_vec(&views).unwrap(),
    )
    .unwrap();
    std::fs::write(
        "output/eleven-workflows/browser-definitions.json",
        serde_json::to_vec(&apiosk_gateway_v2::workflows::definitions(
            &engine.catalog.get().await.unwrap(),
            &json!({"currency":"USD","rate":"1"}),
        ))
        .unwrap(),
    )
    .unwrap();
    // Partner API uses the same deterministic recipe and one saved quote.
    state.fail.store(0, Ordering::SeqCst);
    let engine = Arc::new(engine);
    let api_state = apiosk_gateway_v2::server::ServerState {
        engine: engine.clone(),
        auth: Arc::new(apiosk_gateway_v2::auth::Auth { pool: pool.clone(), http: reqwest::Client::new(), supabase_url: engine.executor.supabase_url.clone(), publishable_key: "fixture".into() }),
        permits: Arc::new(tokio::sync::Semaphore::new(8)), executor_compat: None,
    };
    let api_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api_url = format!("http://{}", api_listener.local_addr().unwrap());
    let app = apiosk_gateway_v2::server::router(api_state, "http://localhost:5173").unwrap();
    let api_server = tokio::spawn(async move { axum::serve(api_listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    let key = "apk_org_live_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let before = state.count.load(Ordering::SeqCst);
    let catalog_response = client.get(format!("{api_url}/v2/ask/workflows")).bearer_auth(key).send().await.unwrap();
    assert_eq!(catalog_response.status(), 200);
    let available: Value = catalog_response.json().await.unwrap();
    let dossier = available["workflows"].as_array().unwrap().iter().find(|r|r["slug"] == "european-company-dossier").unwrap();
    assert_eq!(dossier["country_coverage"].as_array().unwrap().len(), 11);
    let body = json!({"workflow":{"slug":"tender-company-dossier","input":{"name":"User supplied label","country":"NL","registration":"30204462"}}});
    let response = client.post(format!("{api_url}/v2/ask")).bearer_auth(key).header("Idempotency-Key", "partner-dossier").json(&body).send().await.unwrap();
    assert_eq!(response.status(), 200);
    let planned: Value = response.json().await.unwrap();
    assert_eq!(planned["status"], "requires_approval", "{planned}");
    assert_eq!(state.count.load(Ordering::SeqCst), before, "planning cannot buy data");
    let id = planned["id"].as_str().unwrap();
    let saved_id = Uuid::parse_str(id).unwrap();
    let (_, saved, _) = engine.store.load(Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap(), saved_id).await.unwrap();
    let plan: apiosk_gateway_v2::resolver::Plan = serde_json::from_value(saved.proposal.clone().unwrap()).unwrap();
    assert_eq!(plan.steps.len(), 5);
    assert_eq!(plan.fee_bps, 2000);
    let ceiling = plan.cap_atomic;
    assert!(!saved.facts.iter().any(|fact|fact.semantic_type == "company.name"));
    let replay: Value = client.post(format!("{api_url}/v2/ask")).bearer_auth(key).header("Idempotency-Key", "partner-dossier").json(&body).send().await.unwrap().json().await.unwrap();
    assert_eq!(replay["id"], planned["id"]);
    let conflict = client.post(format!("{api_url}/v2/ask")).bearer_auth(key).header("Idempotency-Key", "partner-dossier").json(&json!({"question":"different request"})).send().await.unwrap();
    assert_eq!(conflict.status(), 409);
    for invalid in [json!({}), json!({"question":"both","workflow":body["workflow"]})] {
        assert_eq!(client.post(format!("{api_url}/v2/ask")).bearer_auth(key).json(&invalid).send().await.unwrap().status(), 400);
    }
    let denied = client.post(format!("{api_url}/v2/ask")).bearer_auth(key).json(&json!({"workflow":{"slug":"tender-company-dossier","input":{"name":"Example","country":"GB","registration":"09446231"}}})).send().await.unwrap();
    assert_eq!(denied.status(), 422);
    let response = client.post(format!("{api_url}/v2/asks/{id}/run")).bearer_auth(key).json(&json!({"max_spend":ceiling as f64 / 1e6,"wait_seconds":20})).send().await.unwrap();
    assert!(response.status().is_success());
    let mut completed: Value = response.json().await.unwrap();
    for _ in 0..100 {
        if completed["status"] != "running" { break; }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        completed = client.get(format!("{api_url}/v2/asks/{id}")).bearer_auth(key).send().await.unwrap().json().await.unwrap();
    }
    assert_eq!(completed["status"], "completed", "{completed}");
    assert_eq!(state.count.load(Ordering::SeqCst) - before, 5);
    let (_, saved, _) = engine.store.load(Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap(), saved_id).await.unwrap();
    let registered = saved.facts.iter().find(|fact|fact.semantic_type == "company.name").unwrap();
    assert_ne!(registered.value, "User supplied label");
    let approvals: i64 = sqlx::query_scalar("SELECT count(*) FROM authorizations WHERE task_id=$1").bind(saved_id).fetch_one(&pool).await.unwrap();
    assert_eq!(approvals, 1);
    let replay: Value = client.post(format!("{api_url}/v2/asks/{id}/run")).bearer_auth(key).json(&json!({"wait_seconds":0})).send().await.unwrap().json().await.unwrap();
    assert_eq!(replay["id"], planned["id"]);
    assert_eq!(state.count.load(Ordering::SeqCst) - before, 5, "replay must not buy again");
    println!("PASS partner tender dossier: fixed recipe, registry-bound screening, one quote/approval, five purchases, replay safe");
    api_server.abort();
    server.abort();
}
