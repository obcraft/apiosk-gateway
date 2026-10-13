//! Offline payment fixture: optional quote, exact approval, saved evidence,
//! fixed compute destination and receipt replay. No external provider calls.
use apiosk_gateway_v2::{
    auth::Principal,
    catalog::CatalogCache,
    compute,
    engine::{Engine, UpgradeRequest},
    execution::Executor,
    parser::Parser,
    protocol::{ExecuteRequest, Status},
    resolver::{Action, Plan, Step},
    state::{Signer, Task},
    store::Store,
};
use axum::{extract::State, routing::post, Json, Router};
use chrono::Utc;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, Row};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uuid::Uuid;
async fn provider(
    State(calls): State<Arc<AtomicUsize>>,
    Json(request): Json<Value>,
) -> Json<Value> {
    calls.fetch_add(1, Ordering::SeqCst);
    let messages = request["inputs"]["messages"].as_array().unwrap();
    let input = messages
        .iter()
        .skip(2)
        .map(|m| m["content"].as_str().unwrap())
        .collect::<String>();
    let evidence: Value = serde_json::from_str(&input).unwrap();
    let mut fields = evidence["sources"][0]["records"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|r| r["fields"].as_array().unwrap());
    let id = fields.find(|f| f[2] == 3.5).unwrap()[0].clone();
    Json(
        json!({"ok":true,"charged_micro_usd":3450,"provider_micro_usd":3000,"fee_micro_usd":450,
        "result":{"data":{"choices":[{"finish_reason":"stop","message":{"content":json!({"observations":[{"text":"The employer rating is {{a}}.","evidence":[{"fact_id":id}]}],"limitations":[]}).to_string()}}]}}}),
    )
}
fn execute(engine: &Engine, p: &Principal, id: Uuid, revision: i64, task: &Task) -> ExecuteRequest {
    let action: Action = serde_json::from_value(task.actions[0].clone()).unwrap();
    ExecuteRequest {
        request_id: Uuid::new_v4(),
        action_id: action.id(),
        state: engine
            .signer
            .issue(p.owner, id, revision, task.focus.clone()),
        quote_ref: task
            .proposal
            .as_ref()
            .and_then(|v| v["quote_ref"].as_str())
            .and_then(|s| s.parse().ok()),
        authorization_ref: None,
        idempotency_key: action.id(),
        input: Value::Null,
    }
}
#[tokio::test]
#[ignore = "isolated support-db.mjs required"]
async fn quote_approval_saved_result_and_no_paid_replay() {
    std::env::set_var("ASK_PAID_ANALYSIS_ENABLED", "1");
    let pool=PgPoolOptions::new().max_connections(1).after_connect(|conn,_|Box::pin(async move{
        sqlx::query("SET search_path TO public,gateway_v2,actions,billing,providers,catalog,research,identity,observability").execute(conn).await?;Ok(())
    })).connect(&std::env::var("TEST_DATABASE_URL").unwrap()).await.unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/functions/v1/use-selection", post(provider))
        .with_state(calls.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let http = reqwest::Client::new();
    let store = Store { pool: pool.clone() };
    let engine = Engine {
        store: store.clone(),
        catalog: CatalogCache::new(pool.clone()),
        parser: Parser::new(
            http.clone(),
            format!("{base}/never-call-model"),
            "fixture".into(),
            "fixture".into(),
        ),
        signer: Signer::new(vec![7; 32]).unwrap(),
        executor: Executor {
            store: store.clone(),
            http,
            supabase_url: base,
            enabled: true,
        },
        app_url: "http://localhost".into(),
    };
    let p = Principal {
        owner: Uuid::new_v4(),
        connection: None,
        workspace: None,
    };
    let id = store.create(p.owner, None).await.unwrap();
    let mut task = Task::default();
    task.record_turn("Explain the employer rating", None);
    let service = engine.catalog.get().await.unwrap().services[0].clone();
    let source_cost = service.total();
    let quote = Uuid::new_v4();
    let plan = Plan {
        independent_calls: false, compute: None,
        quote_ref: quote,
        steps: vec![Step {
            service,
            entity_ref: Uuid::new_v4(),
            goal_index: 0,
            browser_fallback: None,
        }],
        cap_atomic: source_cost,
        expires_at: Utc::now() + chrono::Duration::minutes(10),
        hash: "source-plan".into(),
        fee_bps: 1500,
        workflow_slug: None,
    };
    task.proposal = Some(serde_json::to_value(&plan).unwrap());
    task.actions = vec![json!({"kind":"run","action_id":Uuid::new_v4(),"step":0})];
    let revision = store.save(p.owner, id, 0, &task).await.unwrap();
    let invalid = engine
        .analysis_upgrade(
            &p,
            UpgradeRequest {
                request_id: Uuid::new_v4(),
                state: engine
                    .signer
                    .issue(p.owner, id, revision, task.focus.clone()),
                enabled: true,
                output_quote_ref: None,
                model_id: Some("unknown-model".into()),
            },
        )
        .await
        .unwrap();
    assert!(!invalid.errors.is_empty());
    // Every model has its own quote identity, shown cap and approval hash.
    let mut hashes = std::collections::BTreeSet::new();
    for model in compute::MODELS {
        let (rev, current, _) = store.load(p.owner, id).await.unwrap();
        let response = engine
            .analysis_upgrade(
                &p,
                UpgradeRequest {
                    request_id: Uuid::new_v4(),
                    state: engine.signer.issue(p.owner, id, rev, current.focus.clone()),
                    enabled: true,
                    output_quote_ref: None,
                    model_id: Some(model.id.into()),
                },
            )
            .await
            .unwrap();
        assert!(response.errors.is_empty(), "{response:?}");
        assert_eq!(
            response.proposal.as_ref().unwrap().max_total_atomic,
            (source_cost + model.buyer_cap()).to_string()
        );
        assert_eq!(
            response.context_view["analysis_upgrade"]["model"],
            model.name
        );
        let (_, current, _) = store.load(p.owner, id).await.unwrap();
        assert!(hashes.insert(current.proposal.as_ref().unwrap()["hash"].to_string()));
    }
    let (revision, task, _) = store.load(p.owner, id).await.unwrap();
    let request = UpgradeRequest {
        request_id: Uuid::new_v4(),
        state: engine
            .signer
            .issue(p.owner, id, revision, task.focus.clone()),
        enabled: true,
        output_quote_ref: None,
        model_id: Some("claude-opus-5.5".into()),
    };
    let upgraded = engine.analysis_upgrade(&p, request).await.unwrap();
    assert_eq!(upgraded.status, Status::Ready, "{upgraded:?}");
    let upgraded_plan = upgraded.proposal.unwrap();
    assert_eq!(
        upgraded_plan.max_total_atomic,
        (source_cost + compute::BUYER_CAP).to_string()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(
        engine
            .executor
            .approve(&p, id, quote, &source_cost.to_string())
            .await
            .is_err(),
        "old cheaper approval must be rejected"
    );
    engine
        .executor
        .approve(
            &p,
            id,
            upgraded_plan.quote_ref,
            &upgraded_plan.max_total_atomic,
        )
        .await
        .unwrap();
    let (revision, mut task, _) = store.load(p.owner, id).await.unwrap();
    let changed = engine
        .analysis_upgrade(
            &p,
            UpgradeRequest {
                request_id: Uuid::new_v4(),
                state: engine
                    .signer
                    .issue(p.owner, id, revision, task.focus.clone()),
                enabled: false,
                output_quote_ref: None,
                model_id: Some("claude-opus-5.5".into()),
            },
        )
        .await
        .unwrap();
    assert!(!changed.errors.is_empty(), "cannot remove approved compute");
    // A source result already exists. Upgrading it must quote only compute.
    let result = Uuid::new_v4();
    let document = json!({"source":{"provider":"glassdoor","name":"Glassdoor"},"subject":{"label":"Example Coffee"},"data":{"rating":3.5},"coverage":"source_response"});
    let original_auth: Uuid =
        sqlx::query_scalar("SELECT id FROM authorizations WHERE task_id=$1 AND quote_ref=$2")
            .bind(id)
            .bind(upgraded_plan.quote_ref)
            .fetch_one(&pool)
            .await
            .unwrap();
    let source_execution = Uuid::new_v4();
    sqlx::query("INSERT INTO executions(id,owner_id,task_id,action_id,payload_hash,selection_id,reserved_atomic,actual_atomic,status,authorization_id) VALUES($1,$2,$3,$1,'fixture',$4,$5,$5,'succeeded',$6)")
        .bind(source_execution).bind(p.owner).bind(id).bind(Uuid::new_v4()).bind(source_cost).bind(original_auth).execute(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO results(id,owner_id,task_id,execution_id,document) VALUES($1,$2,$3,$4,$5)",
    )
    .bind(result)
    .bind(p.owner)
    .bind(id)
    .bind(source_execution)
    .bind(&document)
    .execute(&pool)
    .await
    .unwrap();
    task.actions.clear();
    task.active_results = vec![result];
    task.result_refs = vec![result];
    task.analysis = Some(json!({"status":"completed","observations":[],"limitations":[]}));
    let revision = store.save(p.owner, id, revision, &task).await.unwrap();
    // The optional upfront compute uses the same authorization as the source.
    task.actions =
        vec![json!({"kind":"compute","action_id":task.compute.as_ref().unwrap().action_id})];
    store.save(p.owner, id, revision, &task).await.unwrap();
    for _ in 0..2 {
        let (revision, task, _) = store.load(p.owner, id).await.unwrap();
        let response = engine
            .execute(&p, "fixture", execute(&engine, &p, id, revision, &task))
            .await
            .unwrap();
        assert!(response.errors.is_empty(), "{response:?}");
    }
    let authorizations: i64 =
        sqlx::query_scalar("SELECT count(*) FROM authorizations WHERE task_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(authorizations, 1, "sources and compute share one approval");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let (revision, task, _) = store.load(p.owner, id).await.unwrap();
    let mut result_quote = engine
        .analysis_upgrade(
            &p,
            UpgradeRequest {
                request_id: Uuid::new_v4(),
                state: engine
                    .signer
                    .issue(p.owner, id, revision, task.focus.clone()),
                enabled: true,
                output_quote_ref: Some(upgraded_plan.quote_ref),
                model_id: Some("deepseek-v4-flash".into()),
            },
        )
        .await
        .unwrap();
    let cheap = compute::model("deepseek-v4-flash").unwrap();
    assert_eq!(
        result_quote.proposal.as_ref().unwrap().max_total_atomic,
        cheap.buyer_cap().to_string()
    );
    assert_eq!(
        result_quote.context_view["analysis_upgrade"]["can_choose"],
        true
    );
    let previous_quote = result_quote.proposal.as_ref().unwrap().quote_ref;
    let (rev, current, _) = store.load(p.owner, id).await.unwrap();
    let selected = compute::model("glm-5.3-flash").unwrap();
    result_quote = engine
        .analysis_upgrade(
            &p,
            UpgradeRequest {
                request_id: Uuid::new_v4(),
                state: engine.signer.issue(p.owner, id, rev, current.focus.clone()),
                enabled: true,
                output_quote_ref: None,
                model_id: Some(selected.id.into()),
            },
        )
        .await
        .unwrap();
    assert!(result_quote.errors.is_empty(), "{result_quote:?}");
    assert_eq!(
        result_quote.proposal.as_ref().unwrap().max_total_atomic,
        selected.buyer_cap().to_string()
    );
    assert!(engine
        .executor
        .approve(&p, id, previous_quote, &cheap.buyer_cap().to_string())
        .await
        .is_err());
    assert!(result_quote.proposal.as_ref().unwrap().steps.is_empty());
    let (revision, task, _) = store.load(p.owner, id).await.unwrap();
    let refused = engine
        .execute(&p, "fixture", execute(&engine, &p, id, revision, &task))
        .await
        .unwrap();
    assert_eq!(refused.status, Status::RequiresApproval);
    engine
        .executor
        .approve(
            &p,
            id,
            result_quote.proposal.unwrap().quote_ref,
            &selected.buyer_cap().to_string(),
        )
        .await
        .unwrap();
    // First transition persists the exact input; second dispatches once.
    for _ in 0..2 {
        let (revision, task, _) = store.load(p.owner, id).await.unwrap();
        let response = engine
            .execute(&p, "fixture", execute(&engine, &p, id, revision, &task))
            .await
            .unwrap();
        assert!(response.errors.is_empty(), "{response:?}");
    }
    let (_, task, _) = store.load(p.owner, id).await.unwrap();
    assert_eq!(task.compute.as_ref().unwrap().status, "completed");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let run = task.compute.as_ref().unwrap();
    let plan: Plan = serde_json::from_value(task.proposal.clone().unwrap()).unwrap();
    let auth: Uuid =
        sqlx::query_scalar("SELECT id FROM authorizations WHERE task_id=$1 AND quote_ref=$2")
            .bind(id)
            .bind(plan.quote_ref)
            .fetch_one(&pool)
            .await
            .unwrap();
    for _ in 0..2 {
        engine
            .executor
            .dispatch_compute(
                &p,
                "fixture",
                apiosk_gateway_v2::execution::ComputeDispatch {
                    task_id:id, revision:0, action:run.action_id, authorization:auth,
                    plan:&plan, inputs:&run.prepared.as_ref().unwrap().payload,
                },
            )
            .await
            .unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(engine
        .executor
        .recoverable_receipts(p.owner, id)
        .await
        .unwrap()
        .is_empty());
    let selection=sqlx::query("SELECT s.resource_url,s.kind,s.provider_price_micro_usd,s.charged_micro_usd FROM user_selected_apis s JOIN executions e ON e.selection_id=s.id WHERE e.id=$1").bind(run.action_id).fetch_one(&pool).await.unwrap();
    assert_eq!(
        selection.get::<i64, _>("provider_price_micro_usd"),
        selected.provider_cap
    );
    assert_eq!(
        selection.get::<i64, _>("charged_micro_usd"),
        selected.buyer_cap()
    );
    assert_eq!(
        selection.get::<String, _>("resource_url"),
        selected.resource
    );
    assert_eq!(selection.get::<String, _>("kind"), "external");
    let executions: i64 = sqlx::query_scalar("SELECT count(*) FROM executions WHERE task_id=$1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(executions, 3);
    let other = Principal {
        owner: Uuid::new_v4(),
        connection: None,
        workspace: None,
    };
    assert!(store.load_scoped(&other, id).await.is_err());
    // A browser disconnect may lose the final task save. Even cancellation
    // and quote expiry cannot cause a second purchase during receipt recovery.
    let (revision, mut saved, _) = store.load(p.owner, id).await.unwrap();
    saved.cancelled = true;
    saved.compute.as_mut().unwrap().status = "running".into();
    saved.analysis = None;
    saved.actions = vec![json!({"kind":"compute","action_id":run.action_id})];
    let revision = store.save(p.owner, id, revision, &saved).await.unwrap();
    sqlx::query("UPDATE authorizations SET expires_at=now()-interval '1 hour' WHERE task_id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let recovered = engine
        .execute(&p, "fixture", execute(&engine, &p, id, revision, &saved))
        .await
        .unwrap();
    assert_eq!(recovered.status, Status::Cancelled);
    assert_eq!(
        recovered.context_view["analysis"]["analysis_model"],
        selected.name
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    server.abort();
}
