use super::*;

impl Database {
    /// Claim the right to pay an upstream x402 for `request_id`, enforcing the
    /// rolling per-day spend cap atomically. Returns Ok(true) when this caller
    /// owns the payment and may proceed, Ok(false) when it is already claimed /
    /// in-flight or would breach the day cap (do NOT pay). Errors propagate so
    /// the caller FAILS CLOSED — a DB error must never let real USDC be paid
    /// without a durable claim.
    pub async fn try_claim_resell_payment(
        &self,
        request_id: Uuid,
        api_id: Uuid,
        amount_atomic: u128,
        cap_per_day_atomic: Option<u128>,
    ) -> Result<bool> {
        let amount = BigDecimal::from_str(&amount_atomic.to_string())
            .map_err(|e| anyhow::anyhow!("bad amount: {e}"))?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("select pg_advisory_xact_lock(hashtextextended('treasury-provider-spend',0))")
            .execute(&mut *tx)
            .await?;

        if let Some(cap) = cap_per_day_atomic {
            let cap_bd = BigDecimal::from_str(&cap.to_string())
                .map_err(|e| anyhow::anyhow!("bad cap: {e}"))?;
            // Sum committed spend (paying + paid) in the trailing day.
            let spent: BigDecimal = sqlx::query_scalar(
                r#"SELECT COALESCE(SUM(amount_atomic), 0)
                   FROM resell_payments
                   WHERE status IN ('paying', 'paid')
                     AND created_at >= now() - interval '1 day'"#,
            )
            .fetch_one(&mut *tx)
            .await?;
            if &spent + &amount > cap_bd {
                tx.rollback().await?;
                return Ok(false);
            }
        }

        let inserted = sqlx::query(
            r#"INSERT INTO resell_payments (request_id, api_id, amount_atomic, status)
               VALUES ($1, $2, $3, 'paying')
               ON CONFLICT (request_id) DO NOTHING"#,
        )
        .bind(request_id)
        .bind(api_id)
        .bind(&amount)
        .execute(&mut *tx)
        .await?
        .rows_affected();

        tx.commit().await?;
        Ok(inserted > 0)
    }

    /// Flip a claimed resell payment to its terminal state. Best-effort: a
    /// failure to record the outcome must not fail the request (the money has
    /// already moved), so callers log-and-continue.
    pub async fn mark_resell_payment(
        &self,
        request_id: Uuid,
        status: &str,
        upstream_tx: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            r#"UPDATE resell_payments
               SET status = $2, upstream_tx = $3, updated_at = now()
               WHERE request_id = $1"#,
        )
        .bind(request_id)
        .bind(status)
        .bind(upstream_tx)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
