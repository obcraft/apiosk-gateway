//! Retain completed evidence even if the host closes its conversation. This
//! worker only reads existing tasks/results. It cannot discover, approve, buy,
//! send notifications or create a recurring occurrence.
use crate::{auth::Principal, engine::Engine};
use sqlx::Row;
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

pub fn start(engine: Arc<Engine>) {
    tokio::spawn(async move {
        let installed: Result<bool, _> =
            sqlx::query_scalar("SELECT to_regclass('gateway_v2.company_check_runs') IS NOT NULL")
                .fetch_one(&engine.store.pool)
                .await;
        if !matches!(installed, Ok(true)) {
            tracing::warn!("company check snapshot worker unavailable: apply its database migration before starting Gateway");
            return;
        }
        let mut timer = tokio::time::interval(Duration::from_secs(30));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            if sweep(&engine).await.is_err() {
                tracing::warn!(
                    "company check snapshot sweep unavailable; saved tasks will be retried"
                );
            }
        }
    });
}

async fn sweep(engine: &Engine) -> anyhow::Result<()> {
    // Update the poll cursor atomically to distribute work between replicas.
    // This is not a source-execution lease; duplicate reads are harmless and
    // collection locks the immutable snapshot before any write.
    let rows=sqlx::query("WITH pending AS (SELECT check_id,occurrence FROM gateway_v2.company_check_runs WHERE completed_at IS NULL AND task_id IS NOT NULL AND (last_checked_at IS NULL OR last_checked_at<now()-interval '25 seconds') ORDER BY last_checked_at NULLS FIRST,created_at LIMIT 20 FOR UPDATE SKIP LOCKED), claimed AS (UPDATE gateway_v2.company_check_runs r SET last_checked_at=now() FROM pending p WHERE r.check_id=p.check_id AND r.occurrence=p.occurrence RETURNING r.check_id,r.occurrence,r.task_id) SELECT r.*,c.owner_id,c.connection_id,c.workspace_id FROM claimed r JOIN gateway_v2.company_checks c ON c.id=r.check_id")
        .fetch_all(&engine.store.pool).await?;
    for row in rows {
        let p = Principal {
            owner: row.get("owner_id"),
            connection: row.get("connection_id"),
            workspace: row.get("workspace_id"),
        };
        let id: Uuid = row.get("check_id");
        let task_id: Uuid = row.get("task_id");
        let occurrence: String = row.get("occurrence");
        if retain(engine, &p, id, &occurrence, task_id).await.is_err() {
            tracing::debug!("company check evidence not ready to retain; retrying later");
        }
    }
    Ok(())
}

async fn retain(
    engine: &Engine,
    p: &Principal,
    id: Uuid,
    occurrence: &str,
    task_id: Uuid,
) -> anyhow::Result<()> {
    let companies = super::store::companies(&engine.store.pool, p, id).await?;
    let response = engine.view(p, task_id, Uuid::new_v4(), None).await?;
    if !super::can_snapshot(&response) {
        return Ok(());
    }
    let (revision, task, _) = engine.store.load_scoped(p, task_id).await?;
    anyhow::ensure!(
        response
            .state
            .as_ref()
            .is_some_and(|s| s.revision == revision),
        "Task changed during snapshot read."
    );
    super::store::collect(engine, p, id, occurrence, &companies, &task, &response).await?;
    Ok(())
}
