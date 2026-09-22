//! Real PostgreSQL + gateway dispatch + HTTP payment-boundary fixture.
//! Source fetch/concurrency is tested separately by the kvk-accounts crate.
use apiosk_gateway_v2::{
    auth::Principal,
    catalog::Service,
    execution::{Dispatch, Executor},
    resolver::{Plan, Step},
    store::Store,
};
use axum::{extract::State, routing::post, Json, Router};
use chrono::Utc;
use serde_json::{json, Value};
use sqlx::PgPool;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uuid::Uuid;

#[derive(Clone)]
struct Payment {
    pool: PgPool,
    calls: Arc<AtomicUsize>,
    price: i64,
    total: i64,
}
async fn pay(State(s): State<Payment>, Json(request): Json<Value>) -> Json<Value> {
    let kvk_accounts::Lookup::Ready(data) =
        kvk_accounts::lookup(&s.pool, request["inputs"]["kvkNummer"].as_str().unwrap())
            .await
            .unwrap()
    else {
        panic!("payment dispatched before cache ready")
    };
    s.calls.fetch_add(1, Ordering::SeqCst);
    Json(
        json!({"ok":true,"execution_id":Uuid::new_v4(),"result":data,"charged_micro_usd":s.total,"provider_micro_usd":s.price,"fee_micro_usd":s.total-s.price}),
    )
}

#[tokio::test]
#[ignore = "requires scripts/test-kvk-accounts.py"]
async fn gateway_waits_without_reservation_then_delivers_to_ten_buyers() {
    let database = std::env::var("KVK_TEST_DATABASE_URL").unwrap();
    assert!(database.starts_with("postgresql://postgres@127.0.0.1:"));
    // Deliberately fewer connections than requests: queue admission must reuse
    // its transaction connection, rather than deadlocking on a second checkout.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .after_connect(|conn, _| Box::pin(async move {
            sqlx::query("SET search_path TO public, actions").execute(conn).await?;
            Ok(())
        }))
        .connect(&database)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!(
        "../supabase/migrations/20260907094100_gateway_v2_state.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql("CREATE TABLE public.user_selected_apis(id uuid primary key,user_id uuid,connection_id uuid,prompt text,kind text,source text,name text,capability text,api_slug text,candidate_id uuid,method text,provider_price_micro_usd bigint,charged_micro_usd bigint,metadata jsonb);").execute(&pool).await.unwrap();
    sqlx::raw_sql("CREATE SCHEMA actions; ALTER TABLE gateway_v2.tasks SET SCHEMA actions; ALTER TABLE gateway_v2.requests SET SCHEMA actions; ALTER TABLE gateway_v2.results SET SCHEMA actions; ALTER TABLE gateway_v2.executions SET SCHEMA actions; ALTER TABLE gateway_v2.authorizations SET SCHEMA actions; ALTER TABLE public.user_selected_apis SET SCHEMA actions;").execute(&pool).await.unwrap();
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/integration-catalog.json")).unwrap();
    let mut service: Service = serde_json::from_value(fixture["services"][0].clone()).unwrap();
    service.api_slug = kvk_accounts::PROVIDER.into();
    service.path = kvk_accounts::ENDPOINT.into();
    let step = Step {
        service,
        entity_ref: Uuid::new_v4(),
        goal_index: 0,
        browser_fallback: None,
    };
    let plan = Plan {
        quote_ref: Uuid::new_v4(),
        steps: vec![step.clone()],
        cap_atomic: step.service.total(),
        expires_at: Utc::now() + chrono::Duration::minutes(10),
        hash: "fixture-hash".into(),
        fee_bps: apiosk_gateway_v2::resolver::standard_markup(),
        workflow_slug: None,
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/functions/v1/use-selection", post(pay))
        .with_state(Payment {
            pool: pool.clone(),
            calls: calls.clone(),
            price: step.service.list_atomic,
            total: step.service.total(),
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let executor = Arc::new(Executor {
        store: Store { pool: pool.clone() },
        http: reqwest::Client::new(),
        supabase_url: origin,
        enabled: true,
    });
    let mut buyers = Vec::new();
    for _ in 0..10 {
        let p = Principal {
            owner: Uuid::new_v4(),
            connection: None, workspace: None, };
        let task = Uuid::new_v4();
        let authorization = Uuid::new_v4();
        sqlx::query("INSERT INTO actions.tasks(id,owner_id,document) VALUES($1,$2,'{}')")
            .bind(task)
            .bind(p.owner)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO actions.authorizations(id,task_id,owner_id,quote_ref,plan_hash,cap_atomic,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(authorization).bind(task).bind(p.owner).bind(plan.quote_ref).bind(&plan.hash).bind(plan.cap_atomic).bind(plan.expires_at).execute(&pool).await.unwrap();
        buyers.push((p, task, authorization, Uuid::new_v4(), Uuid::new_v4()));
    }
    // Unauthorized requests cannot enqueue background upstream work.
    let (p, task, _, action, idempotency) = &buyers[0];
    assert!(executor
        .dispatch(
            p,
            "fixture",
            *task,
            0,
            *action,
            *idempotency,
            Uuid::new_v4(),
            &plan,
            &step,
            &json!({"kvkNummer":"88888888"})
        )
        .await
        .is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM kvk_cache.jobs WHERE kvk_number='88888888'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
    let mut pending = Vec::new();
    for (p, task, authorization, action, idempotency) in buyers.clone() {
        let e = executor.clone();
        let plan = plan.clone();
        let step = step.clone();
        pending.push(tokio::spawn(async move {
            assert!(matches!(
                e.dispatch(
                    &p,
                    "fixture",
                    task,
                    0,
                    action,
                    idempotency,
                    authorization,
                    &plan,
                    &step,
                    &json!({"kvkNummer":"77777777"})
                )
                .await
                .unwrap(),
                Dispatch::Waiting { .. }
            ));
        }));
    }
    for waiter in pending {
        waiter.await.unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    for table in ["actions.executions", "actions.user_selected_apis"] {
        assert_eq!(
            sqlx::query_scalar::<_, i64>(&format!("SELECT count(*) FROM {table}"))
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM kvk_cache.jobs WHERE kvk_number='77777777'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    // Equivalent to the worker's atomic publication, with a documented fixture.
    let data = json!({"opendataFields":[{"key":"FinancialYear","value":"2024"}]});
    sqlx::query("INSERT INTO kvk_cache.annual_accounts(kvk_number,financial_year,document_type,version,payload,source_url) VALUES('77777777',2024,$1,repeat('a',64),$2,'fixture')")
        .bind(kvk_accounts::DOCUMENT_TYPE).bind(&data).execute(&pool).await.unwrap();
    sqlx::query("UPDATE kvk_cache.jobs SET status='succeeded',financial_year=2024,document_type=$1,version=repeat('a',64),fresh_until=now()+interval '24 hours' WHERE kvk_number='77777777'")
        .bind(kvk_accounts::DOCUMENT_TYPE).execute(&pool).await.unwrap();
    for (p, task, authorization, action, idempotency) in &buyers {
        for _ in 0..2 {
            // Second dispatch must reuse that buyer's payment identity.
            let Dispatch::Done { body, .. } = executor
                .dispatch(
                    p,
                    "fixture",
                    *task,
                    0,
                    *action,
                    *idempotency,
                    *authorization,
                    &plan,
                    &step,
                    &json!({"kvkNummer":"77777777"}),
                )
                .await
                .unwrap()
            else {
                panic!("saved result not delivered")
            };
            assert_eq!(body["result"], data);
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 10);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM actions.executions WHERE status='succeeded'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        10
    );
    // Provider failures are returned before reservation for a new buyer action.
    let (p, task, authorization, _, _) = &buyers[0];
    sqlx::query("UPDATE actions.authorizations SET cap_atomic=cap_atomic*2 WHERE id=$1")
        .bind(authorization)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO kvk_cache.jobs(kvk_number,status,error_code,fresh_until) VALUES('99999999','failed','kvk_http_404',now()+interval '5 minutes')").execute(&pool).await.unwrap();
    assert!(matches!(
        executor
            .dispatch(
                p,
                "fixture",
                *task,
                0,
                Uuid::new_v4(),
                Uuid::new_v4(),
                *authorization,
                &plan,
                &step,
                &json!({"kvkNummer":"99999999"})
            )
            .await
            .unwrap(),
        Dispatch::Failed { .. }
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 10);
    server.abort();
    println!("Gateway verified: 10 buyers share 1 job; zero waiting reservations; 10 identical saved results; payment replay deduplicated; unauthorized and missing reports never dispatched.");
}
