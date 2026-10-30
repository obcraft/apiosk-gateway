//! Durable Slack response history, using the same owner lock and SSE cursor as ingress.
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

pub(super) async fn record(
    pool: &PgPool, trigger: Uuid, owner: Uuid, key: &str, text: &str, test: bool, activity: &Value,
) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,29102026))")
        .bind(owner.to_string()).execute(&mut *tx).await?;
    // Ownership is enforced on insertion, including signed button callbacks.
    let inserted = sqlx::query("INSERT INTO gateway_v2.inbound_trigger_events(trigger_id,owner_id,event_key,text,matched,is_test,activity) SELECT id,owner_id,$3,$4,false,$5,$6 FROM gateway_v2.inbound_triggers WHERE id=$1 AND owner_id=$2 AND source='slack' ON CONFLICT(trigger_id,event_key) DO NOTHING")
        .bind(trigger).bind(owner).bind(key).bind(text).bind(test).bind(activity)
        .execute(&mut *tx).await?.rows_affected() == 1;
    tx.commit().await?;
    if inserted { let _ = super::changed().send(owner); }
    Ok(inserted)
}

/// Only display text, never callback URLs, action values, or connection identifiers.
pub(super) fn preview(blocks: &Value) -> String {
    let mut lines = Vec::new();
    for block in blocks.as_array().into_iter().flatten() {
        if let Some(text) = block["text"]["text"].as_str() { lines.push(text); }
        for field in block["fields"].as_array().into_iter().flatten() {
            if let Some(text) = field["text"].as_str() { lines.push(text); }
        }
    }
    lines.join("\n").chars().take(8000).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    #[ignore = "requires isolated local TRIGGERS_TEST_DATABASE_URL"]
    async fn durable_responses_and_concurrent_decisions_are_owner_scoped() {
        let database = std::env::var("TRIGGERS_TEST_DATABASE_URL").unwrap();
        let url = url::Url::parse(&database).unwrap();
        assert!(matches!(url.host_str(), Some("127.0.0.1") | Some("localhost")) && url.path().starts_with("/triggers_test"));
        let pool = PgPool::connect(&database).await.unwrap();
        sqlx::raw_sql("CREATE SCHEMA IF NOT EXISTS auth; CREATE TABLE IF NOT EXISTS auth.users(id uuid PRIMARY KEY); CREATE SCHEMA IF NOT EXISTS gateway_v2; DO $$ BEGIN IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname='gateway_v2_runtime') THEN CREATE ROLE gateway_v2_runtime; END IF; IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname='anon') THEN CREATE ROLE anon; END IF; IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname='authenticated') THEN CREATE ROLE authenticated; END IF; END $$; GRANT USAGE ON SCHEMA gateway_v2 TO gateway_v2_runtime;")
            .execute(&pool).await.unwrap();
        sqlx::raw_sql(include_str!("../../../supabase/migrations/20261029161428_inbound_triggers.sql")).execute(&pool).await.unwrap();
        sqlx::raw_sql(include_str!("../../../supabase/migrations/20261030094736_trigger_response_activity.sql")).execute(&pool).await.unwrap();
        let owner = Uuid::new_v4();
        let other = Uuid::new_v4();
        let trigger = Uuid::new_v4();
        sqlx::query("INSERT INTO auth.users(id) VALUES($1),($2)").bind(owner).bind(other).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO gateway_v2.inbound_triggers(id,owner_id,name,source,connected_account_id,channel_id,provider_trigger_id) VALUES($1,$2,'Suppliers','slack','ca_fixture','C1','ti_fixture')").bind(trigger).bind(owner).execute(&pool).await.unwrap();
        let runtime = sqlx::postgres::PgPoolOptions::new().after_connect(|c, _| Box::pin(async move { sqlx::query("SET ROLE gateway_v2_runtime").execute(c).await?; Ok(()) })).connect(&database).await.unwrap();
        for (i, kind) in ["chat", "screen_company", "request_approval"].iter().enumerate() {
            assert!(record(&runtime, trigger, owner, &format!("response:{i}"), "Response", true, &json!({"kind":kind,"status":"sent"})).await.unwrap());
        }
        let approved = json!({"kind":"approval_decision","status":"approved","approver_name":"Alex","request_key":"C1:1"});
        assert!(!record(&runtime, trigger, other, "decision:C1:1", "Approved", true, &approved).await.unwrap());
        let (a,b) = tokio::join!(
            record(&runtime, trigger, owner, "decision:C1:1", "Approved", true, &approved),
            record(&runtime, trigger, owner, "decision:C1:1", "Approved", true, &approved));
        assert_ne!(a.unwrap(), b.unwrap());
        let rows = super::super::events(&runtime, owner, None).await.unwrap();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0]["activity"]["status"], "approved");
        assert_eq!(rows[0]["matched"], false);
        assert!(super::super::events(&runtime, other, None).await.unwrap().is_empty());
        runtime.close().await;
        let reopened = PgPool::connect(&database).await.unwrap();
        assert!(!record(&reopened, trigger, owner, "decision:C1:1", "Rejected", true, &json!({"status":"rejected"})).await.unwrap());
        assert_eq!(super::super::events(&reopened, owner, None).await.unwrap()[0]["activity"]["status"], "approved");
        let allowed: bool = sqlx::query_scalar("SELECT has_table_privilege('authenticated','gateway_v2.inbound_trigger_events','SELECT') OR has_table_privilege('anon','gateway_v2.inbound_trigger_events','SELECT')").fetch_one(&pool).await.unwrap();
        assert!(!allowed);
    }
}
