use crate::store::Store;
impl Store {
    /// Bounded batches, with unresolved executions retained for reconciliation.
    pub async fn prune(&self) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM requests WHERE (owner_id,request_id) IN (SELECT owner_id,request_id FROM requests WHERE created_at<now()-interval '30 days' LIMIT 500)").execute(&self.pool).await?;
        sqlx::query("DELETE FROM tasks WHERE id IN (SELECT t.id FROM tasks t WHERE expires_at<now() AND NOT EXISTS(SELECT 1 FROM executions e WHERE e.task_id=t.id AND e.status IN ('dispatching','unresolved')) ORDER BY expires_at LIMIT 50)").execute(&self.pool).await?;
        Ok(())
    }

    /// Close out dispatches that nothing is ever going to finish.
    ///
    /// `prune` keeps `dispatching` and `unresolved` rows on purpose, for a
    /// reconciliation that was never written — `executions_reconcile` has been
    /// indexing for it since the schema was created. A row that never leaves
    /// `dispatching` holds three things open for as long as it stands:
    ///
    ///   * its task survives every prune, because `prune` skips a task with an
    ///     execution still in flight;
    ///   * `execution.rs` keeps answering "retry in 2 seconds" for that whole
    ///     source, because the source looks busy;
    ///   * the readiness audit counts it against the source.
    ///
    /// One KVK dispatch from 2026-09-07 did all three for five days, and the
    /// third is why the most-used source in the catalogue could not reach
    /// planner-ready.
    ///
    /// An hour is far longer than any dispatch can legitimately take, so a row
    /// older than that was abandoned rather than slow. `failed` with
    /// `actual_atomic = 0` releases the authorization budget the row was
    /// holding — see the allocation sum in `execution.rs`, which excludes
    /// failed rows. It says nothing about the ledger, which keeps its own
    /// record in `balance_charges`.
    ///
    /// A row with ANY charge against it is left exactly where it is. Money moved
    /// for that call, and where it should end up is a question about a person's
    /// balance, not a row to tidy away on a timer. Those are counted and warned
    /// about instead, so they stop being invisible.
    ///
    /// Returns (closed, awaiting a decision).
    pub async fn reconcile(&self) -> anyhow::Result<(u64, i64)> {
        let closed = sqlx::query(
            "UPDATE executions SET status='failed',actual_atomic=0,updated_at=now(), \
             response=jsonb_build_object('error','dispatch_abandoned','detail','No outcome was recorded within an hour of dispatch; the reservation was released.') \
             WHERE id IN (SELECT e.id FROM executions e \
             WHERE e.status IN ('dispatching','unresolved') AND e.updated_at<now()-interval '1 hour' \
             AND NOT EXISTS(SELECT 1 FROM balance_charges c WHERE c.idempotency_key=e.id::text) \
             ORDER BY e.updated_at LIMIT 50)",
        )
        .execute(&self.pool)
        .await?
        .rows_affected();
        let held: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM executions e \
             WHERE e.status IN ('dispatching','unresolved') AND e.updated_at<now()-interval '1 hour' \
             AND EXISTS(SELECT 1 FROM balance_charges c WHERE c.idempotency_key=e.id::text)",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok((closed, held))
    }
}
