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
    let (kind, initial) = event(&mut stream, &mut buffer).await;
    assert_eq!(kind, "task", "a compact stream starts with a full recovery snapshot");
    assert_eq!(initial["state"]["revision"], revision);
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
    let (kind, recovered) = event(&mut recovered, &mut Vec::new()).await;
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
