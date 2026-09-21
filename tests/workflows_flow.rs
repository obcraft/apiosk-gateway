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
    if input.get("workflow").is_some() {
        let patch: Value = serde_json::from_str(input["question"].as_str().unwrap()).unwrap_or(json!({}));
        let mut values = input["collected"].as_object().unwrap().clone();
        for (key,value) in patch.as_object().unwrap() { values.insert(key.clone(),value.clone()); }
        let ready = input["fields"].as_array().unwrap().iter().all(|f| f["required"] != true || values.get(f["key"].as_str().unwrap()).is_some_and(|v| !v.is_null() && v.as_str() != Some("")));
        return Json(json!({"choices":[{"message":{"content":json!({"message":if ready {"I'll prepare the price for approval."} else {"Which details should I use?"},"input":patch,"ready":ready}).to_string()}}]}));
    }
    assert_eq!(
        body["response_format"]["json_schema"]["schema"]["properties"]["observations"]["maxItems"],
        24
    );
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
    let list = service["list_atomic"].as_i64().unwrap();
    assert_eq!(charged, list + (list * 2000 + 9999) / 10000);
    s.count.fetch_add(1, Ordering::SeqCst);
    if s.fail.load(Ordering::SeqCst) == 1 && service["api_slug"] == "rdw" {
        sqlx::query("INSERT INTO balance_charges(id,user_id,idempotency_key,status,charged_micro_usd) SELECT $1,user_id,$2,'refunded',0 FROM user_selected_apis WHERE id=$3")
   .bind(Uuid::new_v4()).bind(body["idempotency_key"].as_str().unwrap()).bind(id).execute(&s.pool).await.unwrap();
        return Json(json!({"ok":false,"error":"source_unavailable","charged_micro_usd":0}));
    }
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
    sqlx::raw_sql("CREATE SCHEMA catalog; ALTER TABLE public.capabilities ADD COLUMN name text NOT NULL DEFAULT 'Fixture'; ALTER TABLE public.capabilities SET SCHEMA catalog; CREATE VIEW public.capabilities AS SELECT * FROM catalog.capabilities; ALTER TABLE public.capability_execution_contracts SET SCHEMA catalog; CREATE VIEW public.capability_execution_contracts AS SELECT * FROM catalog.capability_execution_contracts; DELETE FROM catalog.capability_execution_contracts WHERE capability_slug='location.property-search';").execute(&pool).await.unwrap();
    let migration =
        include_str!("../supabase/migrations/20260920222320_eleven_workflow_catalog.sql");
    sqlx::raw_sql(migration).execute(&pool).await.unwrap();
    sqlx::raw_sql(migration).execute(&pool).await.unwrap();
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
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
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
    server.abort();
}
