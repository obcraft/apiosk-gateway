use crate::store::Store;
impl Store {
    /// Bounded batches, with unresolved executions retained for reconciliation.
    pub async fn prune(&self) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM gateway_v2.requests WHERE (owner_id,request_id) IN (SELECT owner_id,request_id FROM gateway_v2.requests WHERE created_at<now()-interval '30 days' LIMIT 500)").execute(&self.pool).await?;
        sqlx::query("DELETE FROM gateway_v2.tasks WHERE id IN (SELECT t.id FROM gateway_v2.tasks t WHERE expires_at<now() AND NOT EXISTS(SELECT 1 FROM gateway_v2.executions e WHERE e.task_id=t.id AND e.status IN ('dispatching','unresolved')) ORDER BY expires_at LIMIT 50)").execute(&self.pool).await?;
        Ok(())
    }
}
