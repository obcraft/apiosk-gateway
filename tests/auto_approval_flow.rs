//! Automatic approval against the local fixture database: a connection's
//! standing rule approves a priced plan within every limit, is recorded as
//! `connection_auto`, and leaves everything else waiting for Approve.
use apiosk_gateway_v2::{
    auth::Principal,
    auto_approval::Denial,
    execution::{AutoApproval, Executor},
    resolver::Plan,
    state::Task,
    store::Store,
};
use serde_json::json;
use sqlx::{postgres::PgPoolOptions, Row};
use std::time::Duration;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires local tests/support-db.mjs on 54329; no live database"]
async fn a_standing_rule_approves_within_its_limits_and_nothing_else() {
    let pool = PgPoolOptions::new()
        .after_connect(|conn, _| Box::pin(async move {
            sqlx::query("SET search_path TO public, gateway_v2, kyb, providers, catalog, actions, billing, agents, wallets, research, integrations, observability, support, content, operations, identity, extensions").execute(conn).await?;
            Ok(())
        }))
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&std::env::var("TEST_DATABASE_URL").unwrap_or_else(|_| {
            "postgresql://postgres@127.0.0.1:54329/postgres?sslmode=disable".into()
        }))
        .await
        .unwrap();

    // The App migration (20260929120000) in fixture form: its read function
    // over a rule table, and the audit value on authorizations. Idempotent,
    // because the fixture database outlives one test run.
    for statement in [
        "CREATE TABLE IF NOT EXISTS public.fixture_auto_approval(connection_id uuid PRIMARY KEY, limit_atomic bigint, expires_at timestamptz, monthly_remaining_atomic bigint)",
        "CREATE OR REPLACE FUNCTION gateway_v2.connection_auto_approval(p_owner uuid, p_connection uuid) RETURNS TABLE(limit_atomic bigint, expires_at timestamptz, monthly_remaining_atomic bigint) LANGUAGE sql STABLE SECURITY DEFINER SET search_path='' AS $$ SELECT f.limit_atomic,f.expires_at,f.monthly_remaining_atomic FROM public.fixture_auto_approval f JOIN public.agent_connections c ON c.id=f.connection_id WHERE c.id=p_connection AND c.user_id=p_owner AND c.revoked_at IS NULL $$",
        "GRANT EXECUTE ON FUNCTION gateway_v2.connection_auto_approval(uuid,uuid) TO gateway_v2_runtime",
        r#"DO $$ DECLARE c record; BEGIN
            FOR c IN SELECT conname FROM pg_constraint WHERE conrelid='gateway_v2.authorizations'::regclass AND contype='c' AND pg_get_constraintdef(oid) ILIKE '%approved_via%' LOOP
              EXECUTE format('ALTER TABLE gateway_v2.authorizations DROP CONSTRAINT %I', c.conname);
            END LOOP;
            ALTER TABLE gateway_v2.authorizations ADD CONSTRAINT authorizations_approved_via_check CHECK (approved_via IN ('person_app','connection_budget','connection_auto'));
            ALTER TABLE gateway_v2.authorizations ADD CONSTRAINT authorization_origin CHECK ((approved_via='person_app' AND approved_connection_id IS NULL) OR (approved_via IN ('connection_budget','connection_auto') AND approved_connection_id IS NOT NULL));
           END $$"#,
    ] {
        sqlx::query(statement).execute(&pool).await.unwrap();
    }

    let owner = Uuid::new_v4();
    let connection = Uuid::new_v4();
    sqlx::query("INSERT INTO public.agent_connections VALUES($1,$2,NULL,1000000,2000000)")
        .bind(connection).bind(owner).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO public.user_balances(user_id,balance_micro_usd) VALUES($1,5000000)")
        .bind(owner).execute(&pool).await.unwrap();
    let rule = |limit: i64, days: i64, month: Option<i64>| {
        let pool = pool.clone();
        async move {
            sqlx::query("INSERT INTO public.fixture_auto_approval VALUES($1,$2,now()+make_interval(days=>$3::int),$4) ON CONFLICT(connection_id) DO UPDATE SET limit_atomic=EXCLUDED.limit_atomic,expires_at=EXCLUDED.expires_at,monthly_remaining_atomic=EXCLUDED.monthly_remaining_atomic")
                .bind(connection).bind(limit).bind(days).bind(month).execute(&pool).await.unwrap();
        }
    };
    // A priced, runnable plan saved for this connection: the moment the card
    // would offer Approve.
    let priced = |cap: i64, choice_pending: bool| {
        let pool = pool.clone();
        async move {
            let id = Uuid::new_v4();
            let plan = Plan { compute: None,
                quote_ref: Uuid::new_v4(), steps: vec![], cap_atomic: cap,
                expires_at: chrono::Utc::now() + chrono::Duration::minutes(10),
                hash: format!("plan-{id}"), fee_bps: 1500, workflow_slug: None,
            };
            let mut task = Task::default();
            task.proposal = Some(serde_json::to_value(&plan).unwrap());
            task.actions = vec![json!({"kind":"run","action_id":Uuid::new_v4(),"step":0})];
            if choice_pending {
                task.pending_candidates = vec![json!({"entity_ref":Uuid::new_v4()})];
            }
            sqlx::query("INSERT INTO gateway_v2.tasks(id,owner_id,connection_id,revision,document) VALUES($1,$2,$3,1,$4)")
                .bind(id).bind(owner).bind(connection).bind(serde_json::to_value(&task).unwrap()).execute(&pool).await.unwrap();
            (id, plan)
        }
    };

    // As the gateway runs in production: the narrow runtime role.
    sqlx::query("SET ROLE gateway_v2_runtime").execute(&pool).await.unwrap();
    let store = Store { pool: pool.clone() };
    let executor = Executor { store: store.clone(), http: reqwest::Client::new(), supabase_url: "http://127.0.0.1:9".into(), enabled: true };
    let chatbot = Principal { owner, connection: Some(connection), workspace: None };
    let person = Principal { owner, connection: None, workspace: None };

    let (task, plan) = priced(138, false).await;
    assert_eq!(executor.auto_approve_connection(&chatbot, task).await.unwrap(), AutoApproval::Declined(Denial::Off), "no rule: ask each time");
    assert!(executor.auto_policy(&chatbot).await.unwrap().is_none());

    sqlx::query("RESET ROLE").execute(&pool).await.unwrap();
    rule(100, 30, None).await;
    sqlx::query("SET ROLE gateway_v2_runtime").execute(&pool).await.unwrap();
    assert_eq!(executor.auto_approve_connection(&chatbot, task).await.unwrap(), AutoApproval::Declined(Denial::AboveAutoLimit));

    sqlx::query("RESET ROLE").execute(&pool).await.unwrap();
    rule(500, -1, None).await;
    sqlx::query("SET ROLE gateway_v2_runtime").execute(&pool).await.unwrap();
    assert_eq!(executor.auto_approve_connection(&chatbot, task).await.unwrap(), AutoApproval::Declined(Denial::Expired));

    sqlx::query("RESET ROLE").execute(&pool).await.unwrap();
    rule(500, 30, Some(100)).await;
    sqlx::query("SET ROLE gateway_v2_runtime").execute(&pool).await.unwrap();
    assert_eq!(executor.auto_approve_connection(&chatbot, task).await.unwrap(), AutoApproval::Declined(Denial::Monthly));

    sqlx::query("RESET ROLE").execute(&pool).await.unwrap();
    rule(500, 30, None).await;
    sqlx::query("SET ROLE gateway_v2_runtime").execute(&pool).await.unwrap();
    let policy = executor.auto_policy(&chatbot).await.unwrap().unwrap();
    assert_eq!((policy.limit_atomic, policy.monthly_remaining_atomic), (500, None));
    assert_eq!(executor.auto_approve_connection(&person, task).await.unwrap(), AutoApproval::Declined(Denial::Off), "never for a person");
    let (waiting, _) = priced(138, true).await;
    assert_eq!(executor.auto_approve_connection(&chatbot, waiting).await.unwrap(), AutoApproval::Declined(Denial::NotReady), "a choice is outstanding");

    let AutoApproval::Approved(id) = executor.auto_approve_connection(&chatbot, task).await.unwrap() else {
        panic!("a plan inside every limit is approved");
    };
    let audit = sqlx::query("SELECT approved_via,approved_connection_id,cap_atomic,quote_ref FROM gateway_v2.authorizations WHERE id=$1")
        .bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(audit.get::<String, _>("approved_via"), "connection_auto");
    assert_eq!(audit.get::<Uuid, _>("approved_connection_id"), connection);
    assert_eq!(audit.get::<i64, _>("cap_atomic"), 138);
    assert_eq!(audit.get::<Uuid, _>("quote_ref"), plan.quote_ref);
    assert_eq!(executor.auto_approve_connection(&chatbot, task).await.unwrap(), AutoApproval::AlreadyAuthorized(id), "one approval per quote");
    let billing = store.billing(owner, task, Some(plan.quote_ref), plan.cap_atomic).await.unwrap();
    assert_eq!(billing["authorization_active"], true);
    assert_eq!(billing["approved_via"], "connection_auto");

    // A withdrawn approval is never renewed by the rule.
    sqlx::query("UPDATE gateway_v2.authorizations SET revoked_at=now() WHERE id=$1").bind(id).execute(&pool).await.unwrap();
    assert_eq!(executor.auto_approve_connection(&chatbot, task).await.unwrap(), AutoApproval::Declined(Denial::NotReady));

    // The balance still decides.
    sqlx::query("RESET ROLE").execute(&pool).await.unwrap();
    sqlx::query("UPDATE public.user_balances SET balance_micro_usd=100 WHERE user_id=$1").bind(owner).execute(&pool).await.unwrap();
    sqlx::query("SET ROLE gateway_v2_runtime").execute(&pool).await.unwrap();
    let (poor, _) = priced(138, false).await;
    assert_eq!(executor.auto_approve_connection(&chatbot, poor).await.unwrap(), AutoApproval::Declined(Denial::Balance));

    // Purchases switched off approve nothing.
    let disabled = Executor { enabled: false, ..executor };
    assert_eq!(disabled.auto_approve_connection(&chatbot, poor).await.unwrap(), AutoApproval::Declined(Denial::PurchasesUnavailable));
    sqlx::query("RESET ROLE").execute(&pool).await.unwrap();
}
