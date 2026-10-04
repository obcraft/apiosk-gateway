use apiosk_gateway_v2::{
    auth::Principal,
    catalog::{Catalog, CatalogCache},
    engine::Engine,
    execution::Executor,
    parser::Parser,
    protocol::{ExecuteRequest, Status},
    resolver::{Action, Plan, Step},
    state::{Entity, Signer, Task},
    store::Store,
};
use chrono::{Duration, Utc};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;
use std::sync::Arc;

#[tokio::test]
#[ignore = "isolated support-db only"]
async fn failed_registry_analysis_recovers_saved_results_without_model_or_purchase() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mock = axum::Router::new().fallback(axum::routing::any(move || {
        let observed = observed.clone();
        async move {
            observed.fetch_add(1, Ordering::SeqCst);
            axum::Json(json!({"choices":[{"message":{"content":"{}"}}]}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let pool = PgPoolOptions::new().max_connections(1).after_connect(|conn, _| {
        Box::pin(async move { sqlx::query("SET search_path TO public,gateway_v2").execute(conn).await?; Ok(()) })
    }).connect(&std::env::var("TEST_DATABASE_URL").unwrap()).await.unwrap();
    let store = Store { pool: pool.clone() };
    let http = reqwest::Client::new();
    let engine = Engine {
        store: store.clone(), catalog: CatalogCache::new(pool.clone()),
        parser: Parser::new(http.clone(), format!("{base}/model"), "fixture".into(), "fixture".into()),
        signer: Signer::new(vec![7; 32]).unwrap(),
        executor: Executor { store: store.clone(), http, supabase_url: base, enabled: false },
        app_url: "https://example.test".into(),
    };
    let person = Principal { owner: Uuid::new_v4(), connection: None, workspace: None };
    let task_id = store.create(person.owner, None).await.unwrap();
    let entity = Uuid::new_v4();
    let quote = Uuid::new_v4();
    let action = Uuid::new_v4();
    let catalog = Catalog::build(serde_json::from_str(include_str!("fixtures/company-sources/catalog.json")).unwrap());
    let mut task = Task::default();
    task.entities.push(Entity { entity_ref: entity, label: "Example PLC".into(), selected: true });
    task.record_turn("Geef het bedrijfsprofiel, de functionarissen en de geregistreerde zeggenschap van Example PLC.", None);
    task.capability_intake = Some(serde_json::from_value(json!({
        "parsed_request":{"language":"nl","subjects":[],"capabilities":[],"deliverable":{"format":"chat","operations":[]},
            "meta":{"model":"fixture","parsed_at":"2026-10-02T00:00:00Z","text_sha256":"fixture"}},
        "matches":[],"apiosk_sources_searched":0,"catalog_version":"fixture","selected_capabilities":[]
    })).unwrap());
    let data = [
        json!({"company_name":"Example PLC","company_number":"00000001","company_status":"active","date_of_creation":"1900-01-01","links":{"self":"/company/00000001"}}),
        json!({"total_results":82,"links":{"self":"/company/00000001/officers"},"items":(0..82).map(|i|json!({"name":format!("Officiër {i}"),"officer_role":"director","appointed_on":"2020-01-01","padding":"é".repeat(1000)})).collect::<Vec<_>>()}),
        json!({"total_results":0,"items":[],"links":{"self":"/company/00000001/persons-with-significant-control"}}),
    ];
    let mut steps = Vec::new();
    let mut goals = Vec::new();
    let mut documents = Vec::new();
    for (index, capability) in ["company.registry-profile", "company.officers", "company.significant-control"].iter().enumerate() {
        let service = catalog.services.iter().find(|service| service.api_slug == "companies-house" && service.contract.capability_slug == *capability).unwrap().clone();
        goals.push(json!({"capability":capability,"subject":0,"constraints":{"source":"companies-house","endpoint_id":service.endpoint_id}}));
        let mut document = json!({"data":data[index],"source":service.result_source(),"subject":{"entity_ref":entity,"label":"Example PLC"},
            "coverage":"source_response","missing_fields":[],"charged_atomic":"54348","currency":"USD"});
        document["source"]["path"] = json!(service.path);
        let step = Step { service, entity_ref: entity, goal_index: index, browser_fallback: None };
        task.completed.push(step.key());
        steps.push(step);
        let execution = Uuid::new_v4();
        let result = Uuid::new_v4();
        sqlx::query("INSERT INTO executions(id,owner_id,task_id,action_id,payload_hash,selection_id,reserved_atomic,actual_atomic,status) VALUES($1,$2,$3,$1,'fixture',$1,54348,54348,'succeeded')")
            .bind(execution).bind(person.owner).bind(task_id).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO results(id,owner_id,task_id,execution_id,document) VALUES($1,$2,$3,$4,$5)")
            .bind(result).bind(person.owner).bind(task_id).bind(execution).bind(&document).execute(&pool).await.unwrap();
        task.active_results.push(result);
        task.result_refs.push(result);
        documents.push(document);
    }
    task.intent = Some(json!({"subjects":[{"entity_ref":entity,"label":"Example PLC","values":{"company.registration.gb.companies_house":"00000001"}}],
        "goals":goals,"answer_operations":["Toon de opgeslagen registergegevens"],"unresolved_references":[]}));
    task.proposal = Some(serde_json::to_value(Plan { quote_ref: quote, cap_atomic: 163044, steps, expires_at: Utc::now() + Duration::hours(1),
        hash: "fixture".into(), fee_bps: 1500, workflow_slug: None }).unwrap());
    task.analysis = Some(json!({"status":"unavailable","observations":[],"limitations":["Synthesis failed"]}));
    task.actions = vec![serde_json::to_value(Action::RetryAnalysis { action_id: action }).unwrap()];
    let revision = store.save(person.owner, task_id, 0, &task).await.unwrap();
    let response = engine.execute(&person, "", ExecuteRequest {
        request_id: Uuid::new_v4(), action_id: action, state: engine.signer.issue(person.owner, task_id, revision, task.focus.clone()),
        quote_ref: Some(quote), authorization_ref: None, idempotency_key: action, input: Value::Null,
    }).await.unwrap();
    assert_eq!(response.status, Status::Partial, "{response:?}");
    let analysis = &response.context_view["analysis"];
    assert_eq!(analysis["fallback"], "saved_registry_fields", "{response:?}; outbound calls={}", calls.load(Ordering::SeqCst));
    assert!(analysis["follow_up_question"].is_null(), "a synthesis failure is not missing user context");
    assert!(analysis["observations"][0]["text"].as_str().unwrap().starts_with("De geregistreerde naam"));
    assert_eq!(analysis["presentation"]["rows"].as_array().unwrap().len(), 82);
    assert_eq!(analysis["presentation"]["rows"][81][0]["value"], "Officiër 81");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "saved recovery must not call a model or source");
    let (_, recovered, _) = store.load(person.owner, task_id).await.unwrap();
    assert_eq!(recovered.active_results, task.active_results);
    assert_eq!(recovered.completed, task.completed);
    for (result, document) in task.active_results.iter().zip(&documents) {
        assert_eq!(&store.result(person.owner, task_id, *result).await.unwrap(), document);
    }
    let (executions, charged): (i64, i64) = sqlx::query_as("SELECT count(*),sum(actual_atomic)::bigint FROM executions WHERE task_id=$1")
        .bind(task_id).fetch_one(&pool).await.unwrap();
    assert_eq!((executions, charged), (3, 163044), "recovery must not add an execution or charge");
    server.abort();
}

#[tokio::test]
#[ignore = "isolated support-db only"]
async fn saved_nonempty_analysis_retry_is_offered_and_preserves_findings_and_payments_on_model_failure() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let model_calls = Arc::new(AtomicUsize::new(0));
    let source_calls = Arc::new(AtomicUsize::new(0));
    let observed_models = model_calls.clone();
    let observed_sources = source_calls.clone();
    let mock = axum::Router::new()
        .route("/model", axum::routing::post(move || {
            let calls = observed_models.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                axum::Json(json!({"choices":[{"message":{"content":"{}"}}]}))
            }
        }))
        .fallback(axum::routing::any(move || {
            let calls = observed_sources.clone();
            async move { calls.fetch_add(1, Ordering::SeqCst); axum::http::StatusCode::BAD_GATEWAY }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let database_url = std::env::var("TEST_DATABASE_URL").unwrap();
    assert_eq!(reqwest::Url::parse(&database_url).unwrap().host_str(), Some("127.0.0.1"), "fixture must use an isolated loopback database");
    let pool = PgPoolOptions::new().max_connections(1).after_connect(|conn, _| {
        Box::pin(async move { sqlx::query("SET search_path TO public,gateway_v2").execute(conn).await?; Ok(()) })
    }).connect(&database_url).await.unwrap();
    let store = Store { pool: pool.clone() };
    let http = reqwest::Client::new();
    let engine = Engine {
        store: store.clone(), catalog: CatalogCache::new(pool.clone()),
        parser: Parser::new(http.clone(), format!("{base}/model"), "fixture".into(), "fixture".into()),
        signer: Signer::new(vec![7; 32]).unwrap(),
        executor: Executor { store: store.clone(), http, supabase_url: base, enabled: true },
        app_url: "https://example.test".into(),
    };
    let catalog = Catalog::build(serde_json::from_str(include_str!("fixtures/integration-catalog.json")).unwrap());
    for status in ["completed", "partial"] {
        let person = Principal { owner: Uuid::new_v4(), connection: None, workspace: None };
        let task_id = store.create(person.owner, None).await.unwrap();
        let entity = Uuid::new_v4();
        let quote = Uuid::new_v4();
        let authorization = Uuid::new_v4();
        sqlx::query("INSERT INTO authorizations(id,task_id,owner_id,quote_ref,plan_hash,cap_atomic,expires_at) VALUES($1,$2,$3,$4,'saved-fixture',200,now()-interval '1 hour')")
            .bind(authorization).bind(task_id).bind(person.owner).bind(quote).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO user_balances(user_id,balance_micro_usd) VALUES($1,800)")
            .bind(person.owner).execute(&pool).await.unwrap();
        let mut task = Task::default();
        task.entities.push(Entity { entity_ref: entity, label: "Saved fixture".into(), selected: true });
        task.record_turn("Combine the saved first and second source findings.", None);
        let mut steps = Vec::new();
        let mut goals = Vec::new();
        let mut evidence = Vec::new();
        for index in 0..2 {
            let mut service = catalog.services[0].clone();
            service.id = Uuid::new_v4();
            service.endpoint_id = Uuid::new_v4();
            service.api_slug = format!("saved-fixture-{index}");
            service.contract.capability_slug = format!("saved.fixture.{index}");
            service.path = format!("/saved-fixture/{index}");
            let execution = Uuid::new_v4();
            let result = Uuid::new_v4();
            let document = json!({"data":{"finding":format!("Saved finding {index}")},"source":service.result_source(),
                "subject":{"entity_ref":entity,"label":"Saved fixture"},"coverage":"source_response","missing_fields":[],"charged_atomic":"100","currency":"USD"});
            sqlx::query("INSERT INTO executions(id,owner_id,task_id,action_id,authorization_id,payload_hash,selection_id,reserved_atomic,actual_atomic,status) VALUES($1,$2,$3,$1,$4,'saved-fixture',$1,100,100,'succeeded')")
                .bind(execution).bind(person.owner).bind(task_id).bind(authorization).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO results(id,owner_id,task_id,execution_id,document) VALUES($1,$2,$3,$4,$5)")
                .bind(result).bind(person.owner).bind(task_id).bind(execution).bind(&document).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO balance_charges(id,user_id,idempotency_key,status,charged_micro_usd,actual_provider_micro_usd,fee_micro_usd) VALUES($1,$2,$3,'charged',100,87,13)")
                .bind(Uuid::new_v4()).bind(person.owner).bind(execution.to_string()).execute(&pool).await.unwrap();
            goals.push(json!({"capability":service.contract.capability_slug,"subject":0,"constraints":{}}));
            evidence.push(json!({"result_ref":result,"pointer":"/data/finding","value":document["data"]["finding"],"source":document["source"],"subject":document["subject"]}));
            let step = Step { service, entity_ref: entity, goal_index: index, browser_fallback: None };
            task.completed.push(step.key());
            steps.push(step);
            task.active_results.push(result);
            task.result_refs.push(result);
        }
        task.intent = Some(json!({"subjects":[{"entity_ref":entity,"label":"Saved fixture","values":{}}],"goals":goals,
            "answer_operations":["Combine the saved findings"],"unresolved_references":[]}));
        task.proposal = Some(serde_json::to_value(Plan { quote_ref: quote, cap_atomic: 200, steps, expires_at: Utc::now()-Duration::hours(1),
            hash: "saved-fixture".into(), fee_bps: 1500, workflow_slug: None }).unwrap());
        let previous = json!({"status":status,"observations":[{"text":"The saved findings remain available.","evidence":evidence}],
            "presentation":{"kind":"text"},"limitations":if status=="partial" {vec!["The saved sources have a bounded scope."]} else {vec![]},"follow_up_question":null});
        task.analysis = Some(previous.clone());
        assert!(task.actions.is_empty(), "the server must reconstruct the offered action");
        store.save(person.owner, task_id, 0, &task).await.unwrap();
        let snapshot_sql = "SELECT jsonb_build_object('authorizations',(SELECT jsonb_agg(to_jsonb(a) ORDER BY a.id) FROM authorizations a WHERE a.task_id=$1),'executions',(SELECT jsonb_agg(to_jsonb(e) ORDER BY e.id) FROM executions e WHERE e.task_id=$1),'results',(SELECT jsonb_agg(to_jsonb(r) ORDER BY r.id) FROM results r WHERE r.task_id=$1),'receipts',(SELECT jsonb_agg(to_jsonb(c) ORDER BY c.id) FROM balance_charges c WHERE c.user_id=$2),'balance',(SELECT balance_micro_usd FROM user_balances WHERE user_id=$2))";
        let before: Value = sqlx::query_scalar(snapshot_sql).bind(task_id).bind(person.owner).fetch_one(&pool).await.unwrap();
        let offered = engine.view(&person, task_id, Uuid::new_v4(), None).await.unwrap();
        let retries: Vec<_> = offered.next_actions.iter().filter(|action| action.kind == "retry_analysis").collect();
        assert_eq!(retries.len(), 1, "{status}: {offered:?}");
        assert_eq!(retries[0].action_id, quote);
        assert!(!retries[0].requires_authorization);
        let calls_before = model_calls.load(Ordering::SeqCst);
        let other = Principal { owner: Uuid::new_v4(), connection: None, workspace: None };
        assert!(engine.view(&other, task_id, Uuid::new_v4(), None).await.is_err(), "another owner cannot access the saved task");
        let foreign = engine.execute(&other, "", ExecuteRequest {
            request_id: Uuid::new_v4(), action_id: quote, state: offered.state.clone().unwrap(),
            quote_ref: Some(quote), authorization_ref: None, idempotency_key: Uuid::new_v4(), input: Value::Null,
        }).await.unwrap();
        assert!(!foreign.errors.is_empty(), "another owner cannot invoke the offered saved retry");
        sqlx::query("INSERT INTO execution_leases(task_id,worker_id,expires_at) VALUES($1,$2,now()+interval '1 minute')")
            .bind(task_id).bind(Uuid::new_v4()).execute(&pool).await.unwrap();
        let busy_view = engine.view(&person, task_id, Uuid::new_v4(), None).await.unwrap();
        assert!(!busy_view.next_actions.iter().any(|action| action.kind == "retry_analysis"), "active workers suppress manual recovery");
        let busy = engine.execute(&person, "", ExecuteRequest {
            request_id: Uuid::new_v4(), action_id: quote, state: offered.state.clone().unwrap(),
            quote_ref: Some(quote), authorization_ref: None, idempotency_key: Uuid::new_v4(), input: Value::Null,
        }).await.unwrap();
        assert!(!busy.errors.is_empty(), "an earlier offered action cannot bypass the active worker guard");
        assert_eq!(model_calls.load(Ordering::SeqCst), calls_before, "guard failures must not call the model");
        sqlx::query("DELETE FROM execution_leases WHERE task_id=$1").bind(task_id).execute(&pool).await.unwrap();
        let response = engine.execute(&person, "", ExecuteRequest {
            request_id: Uuid::new_v4(), action_id: retries[0].action_id, state: offered.state.unwrap(),
            quote_ref: Some(quote), authorization_ref: None, idempotency_key: Uuid::new_v4(), input: Value::Null,
        }).await.unwrap();
        assert!(model_calls.load(Ordering::SeqCst) > calls_before, "the fixture must actually exercise model failure");
        let (_, recovered, _) = store.load(person.owner, task_id).await.unwrap();
        let analysis = recovered.analysis.as_ref().unwrap();
        assert_eq!(analysis["observations"], previous["observations"], "{status}: prior verified evidence must survive");
        assert_eq!(analysis["presentation"], previous["presentation"]);
        assert_eq!(analysis["status"], previous["status"]);
        assert!(analysis["follow_up_question"].is_null(), "a failed refresh must not invent missing user context: {response:?}");
        assert!(analysis["limitations"].as_array().unwrap().iter().any(|note| note.as_str().is_some_and(|s| s.contains("previously verified findings"))));
        assert_eq!(recovered.active_results, task.active_results);
        assert_eq!(recovered.completed, task.completed);
        assert_eq!(recovered.proposal, task.proposal);
        let after: Value = sqlx::query_scalar(snapshot_sql).bind(task_id).bind(person.owner).fetch_one(&pool).await.unwrap();
        assert_eq!(after, before, "{status}: saved-only retry must preserve every execution, receipt, source document, approval and balance");
        assert_eq!(source_calls.load(Ordering::SeqCst), 0, "saved-only retry must never dispatch a provider request");
    }
    server.abort();
}

async fn event(response: &mut reqwest::Response, buffer: &mut Vec<u8>) -> (String, Value) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Some(end) = buffer.windows(2).position(|pair| pair == b"\n\n") {
                let frame = String::from_utf8(buffer[..end].to_vec()).unwrap();
                buffer.drain(..end + 2);
                let kind = frame.lines().find_map(|line| line.strip_prefix("event: "));
                let data = frame.lines().find_map(|line| line.strip_prefix("data: "));
                if let (Some(kind), Some(data)) = (kind, data) {
                    return (kind.to_owned(), serde_json::from_str(data).unwrap());
                }
            } else {
                let bytes = response.chunk().await.unwrap().expect("event stream closed early");
                buffer.extend_from_slice(&bytes);
            }
        }
    }).await.expect("saved task event timed out")
}

#[tokio::test]
#[ignore = "isolated support-db only"]
async fn saved_lookup_and_batched_history_preserve_results() {
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
        .connect(&std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let store = Store { pool: pool.clone() };
    let http = reqwest::Client::new();
    let engine = Arc::new(Engine {
        store: store.clone(),
        catalog: CatalogCache::new(pool.clone()),
        // No model key or live provider. A regression into model analysis
        // returns unavailable and fails the completed-answer assertion below.
        parser: Parser::new(
            http.clone(),
            "http://127.0.0.1:1".into(),
            "".into(),
            "fixture".into(),
        ),
        signer: Signer::new(vec![7; 32]).unwrap(),
        executor: Executor {
            store: store.clone(),
            http,
            supabase_url: "http://127.0.0.1:1".into(),
            enabled: false,
        },
        app_url: "http://localhost:5173".into(),
    });
    let person = Principal {
        owner: Uuid::new_v4(),
        connection: None,
        workspace: None,
    };
    let task_id = store.create(person.owner, None).await.unwrap();
    let catalog = Catalog::build(
        serde_json::from_str(include_str!("fixtures/supplier-check/catalog.json")).unwrap(),
    );
    let service = catalog
        .services
        .iter()
        .find(|s| s.contract.capability_slug == "company.registry-profile")
        .unwrap()
        .clone();
    let entity = Uuid::new_v4();
    let quote = Uuid::new_v4();
    let action = Uuid::new_v4();
    let mut task = Task::default();
    task.entities.push(Entity {
        entity_ref: entity,
        label: "Mollie B.V.".into(),
        selected: true,
    });
    task.record_turn(
        "Wat is het adres van Mollie B.V.?",
        Some("Ik controleer het Handelsregister.".into()),
    );
    task.intent = Some(
        json!({"subjects":[{"entity_ref":entity,"label":"Mollie B.V.","values":{}}],
        "goals":[{"capability":"company.registry-profile","subject":0,"constraints":{}}],
        "answer_operations":["Geef het adres"],"unresolved_references":[]}),
    );
    let step = Step {
        service: service.clone(),
        entity_ref: entity,
        goal_index: 0,
        browser_fallback: None,
    };
    task.completed.push(step.key());
    task.proposal = Some(
        serde_json::to_value(Plan {
            quote_ref: quote,
            steps: vec![step],
            cap_atomic: service.total(),
            expires_at: Utc::now() + Duration::hours(1),
            hash: "fixture".into(),
            fee_bps: 1500,
            workflow_slug: None,
        })
        .unwrap(),
    );
    task.actions = vec![serde_json::to_value(Action::Analyze { action_id: action }).unwrap()];
    let mut ids = Vec::new();
    for _ in 0..8 {
        let execution = Uuid::new_v4();
        let result = Uuid::new_v4();
        sqlx::query("INSERT INTO executions(id,owner_id,task_id,action_id,payload_hash,selection_id,reserved_atomic,actual_atomic,status) VALUES($1,$2,$3,$1,'fixture',$1,0,0,'succeeded')")
            .bind(execution).bind(person.owner).bind(task_id).execute(&pool).await.unwrap();
        let document = json!({"data":service.sample_payload,"source":{"provider":service.api_slug,"path":service.path,"endpoint_id":service.endpoint_id},
            "subject":{"entity_ref":entity,"label":"Mollie B.V."},"coverage":"source_response","missing_fields":[]});
        sqlx::query(
            "INSERT INTO results(id,owner_id,task_id,execution_id,document) VALUES($1,$2,$3,$4,$5)",
        )
        .bind(result)
        .bind(person.owner)
        .bind(task_id)
        .bind(execution)
        .bind(document)
        .execute(&pool)
        .await
        .unwrap();
        ids.push(result);
    }
    task.active_results = vec![ids[0]];
    task.result_refs = vec![ids[0]];
    let revision = store.save(person.owner, task_id, 0, &task).await.unwrap();
    let api = apiosk_gateway_v2::server::router(apiosk_gateway_v2::server::ServerState {
        engine: engine.clone(),
        auth: Arc::new(apiosk_gateway_v2::auth::Auth {
            pool: pool.clone(), http: reqwest::Client::new(),
            supabase_url: "http://127.0.0.1:1".into(), publishable_key: "fixture".into(),
        }),
        permits: Arc::new(tokio::sync::Semaphore::new(4)), executor_compat: None,
    }, "http://localhost:5173").unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, api).await.unwrap() });
    let expires = (Utc::now() + Duration::minutes(1)).timestamp();
    let signature = engine.signer.events_signature(person.owner, task_id, None, expires);
    let events_url = format!("{base}/v2/tasks/{task_id}/events?owner={}&expires={expires}&signature={signature}", person.owner);
    let client = reqwest::Client::new();
    let mut stream = client.get(format!("{events_url}&compact=true")).send().await.unwrap();
    let mut legacy = client.get(&events_url).send().await.unwrap();
    let mut buffer = Vec::new();
    let mut legacy_buffer = Vec::new();
    assert_eq!(event(&mut stream, &mut buffer).await.0, "stream_open", "the observable heartbeat announces itself before the first snapshot");
    let (kind, initial) = event(&mut stream, &mut buffer).await;
    assert_eq!(kind, "task", "a compact stream starts with a full recovery snapshot");
    assert_eq!(initial["state"]["revision"], revision);
    assert_eq!(event(&mut legacy, &mut legacy_buffer).await.0, "stream_open");
    assert_eq!(event(&mut legacy, &mut legacy_buffer).await.0, "task");
    let response = engine
        .execute(
            &person,
            "",
            ExecuteRequest {
                request_id: Uuid::new_v4(),
                action_id: action,
                state: engine
                    .signer
                    .issue(person.owner, task_id, revision, task.focus),
                quote_ref: Some(quote),
                authorization_ref: None,
                idempotency_key: action,
                input: Value::Null,
            },
        )
        .await
        .unwrap();
    assert_eq!(response.status, Status::Succeeded, "{response:?}");
    let (kind, update) = event(&mut stream, &mut buffer).await;
    assert_eq!(kind, "task_delta");
    assert_eq!(update["base_revision"], initial["state"]["revision"]);
    assert_eq!(update["set"]["status"], "succeeded");
    assert_eq!(update["context_set"]["analysis"], response.context_view["analysis"]);
    assert!(update["context_set"].get("results").is_none(), "unchanged source results are not retransmitted");
    let (kind, legacy_update) = event(&mut legacy, &mut legacy_buffer).await;
    assert_eq!(kind, "task", "older App and MCP clients keep full updates");
    assert_eq!(legacy_update["context_view"]["analysis"], response.context_view["analysis"]);
    let mut recovered = client.get(format!("{events_url}&compact=true")).send().await.unwrap();
    let mut recovered_buffer = Vec::new();
    assert_eq!(event(&mut recovered, &mut recovered_buffer).await.0, "stream_open");
    let (kind, recovered) = event(&mut recovered, &mut recovered_buffer).await;
    assert_eq!(kind, "task");
    assert_eq!(recovered["status"], "succeeded");
    eprintln!("LOCAL FIXTURE final event: full {} bytes, compact {} bytes", legacy_update.to_string().len(), update.to_string().len());
    assert_eq!(response.context_view["analysis"]["observations"][0]["text"],
        "Volgens het Handelsregister is het bezoekadres van de hoofdvestiging Keizersgracht 126 1015CW Amsterdam.");
    assert_eq!(
        response.context_view["analysis"]["observations"][0]["evidence"][0]["result_ref"],
        json!(ids[0])
    );
    assert!(response.context_view["report"]["download_path"].is_string());
    let (_, mut completed, _) = store.load(person.owner, task_id).await.unwrap();
    let report = engine
        .research_document(person.owner, task_id, &completed, quote)
        .await
        .unwrap();
    let path = response.context_view["report"]["download_path"]
        .as_str()
        .unwrap();
    assert!(path.contains(&format!(
        "snapshot={}",
        apiosk_gateway_v2::state::digest(&report)
    )));
    completed.archive_output(response.proposal.map(|p| serde_json::to_value(p).unwrap()));
    completed.proposal = None;
    completed.record_turn("Dank je", Some("Graag gedaan.".into()));
    completed.actions.clear();
    store
        .save(
            person.owner,
            task_id,
            response.state.unwrap().revision,
            &completed,
        )
        .await
        .unwrap();
    let history = engine
        .view(&person, task_id, Uuid::new_v4(), None)
        .await
        .unwrap();
    assert_eq!(
        history.context_view["conversation"][0]["output"]["results"][0],
        *response.result.as_ref().unwrap()
    );
    assert_eq!(
        history.context_view["conversation"][0]["output"]["analysis"],
        response.context_view["analysis"]
    );
    assert!(
        history.context_view["conversation"][0]["output"]["report"]["download_path"].is_string()
    );

    let mut repeated = ids.clone();
    repeated.extend(&ids);
    let batch = store
        .results(person.owner, task_id, &repeated)
        .await
        .unwrap();
    assert_eq!(batch.len(), ids.len());
    for id in &ids {
        assert_eq!(
            batch[id],
            store.result(person.owner, task_id, *id).await.unwrap()
        );
    }
    assert!(store.results(Uuid::new_v4(), task_id, &ids).await.is_err());
    assert!(store
        .results(person.owner, Uuid::new_v4(), &ids)
        .await
        .is_err());
    let mut missing = ids.clone();
    missing.push(Uuid::new_v4());
    assert!(store
        .results(person.owner, task_id, &missing)
        .await
        .is_err());

    let mut sequential = Vec::new();
    let mut batched = Vec::new();
    for _ in 0..20 {
        let start = std::time::Instant::now();
        for id in &ids {
            store.result(person.owner, task_id, *id).await.unwrap();
        }
        sequential.push(start.elapsed().as_micros());
        let start = std::time::Instant::now();
        store.results(person.owner, task_id, &ids).await.unwrap();
        batched.push(start.elapsed().as_micros());
    }
    sequential.sort();
    batched.sort();
    eprintln!("LOCAL FIXTURE eight result reads: sequential median {} us, batch median {} us; 8 -> 1 database round trips (not production latency)", sequential[10], batched[10]);
    sqlx::query("UPDATE tasks SET expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(task_id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(store.results(person.owner, task_id, &ids).await.is_err());
    assert_eq!(client.get(&events_url).send().await.unwrap().status(), reqwest::StatusCode::NOT_FOUND);
    server.abort();
}
