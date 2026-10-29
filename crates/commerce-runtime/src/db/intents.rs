use super::*;

impl Database {
    /// Record one agent intent-survey answer (`agent_intents`, migration 049).
    ///
    /// Best-effort telemetry: the caller treats a failure as non-fatal — the
    /// 402 survey is optional and must never affect the payment path.
    pub async fn insert_agent_intent(&self, row: AgentIntentInsert<'_>) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO agent_intents
               (request_id, api_slug, path, method, intent, use_case, contact,
                agent_name, user_agent, ip_address, source)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::inet, $11)"#,
        )
        .bind(row.request_id)
        .bind(row.api_slug)
        .bind(row.path)
        .bind(row.method)
        .bind(row.intent)
        .bind(row.use_case)
        .bind(row.contact)
        .bind(row.agent_name)
        .bind(row.user_agent)
        .bind(row.ip_address.as_deref())
        .bind(row.source)
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}
