//! Durable settlement ledger for the direct-purchase payment pointer
//! (`crate::pay`, endpoints `/pay/charge` + `/pay/verify`).
//!
//! This table is the SOURCE OF TRUTH for the pointer flow: idempotency,
//! per-day spend caps, and one-time redemption all read and write
//! `mcp_settlements`, so every decision is correct across process restarts AND
//! across multiple gateway instances. The engine holds NO in-process state.
//! `provider_id` is a column, not a code branch — the feature is generic and
//! reusable; every provider shares this one schema.
//!
//! Lifecycle of a `payment_ref`:
//!   `challenged` (phase A issued a 402)
//!     → `paying`  (phase B atomically claimed it for verify+settle)
//!       → `paid`  (USDC settled; `tx_hash`/`settled_at` set)  [terminal]
//!     → back to `challenged` if settle fails (fail-closed, retriable)
//! Redemption (`/pay/verify`) is orthogonal: it stamps `redeemed_at` on a
//! `paid` row exactly once, via a single atomic UPDATE.
//!
//! Mirrors the atomic conditional-UPDATE / `ON CONFLICT` claim patterns in
//! [`super::idempotency`]. Schema: `migrations/030_mcp_payment_proxy.sql`.
//!
//! Runtime (non-macro) sqlx queries, matching the rest of the crate, so this
//! compiles without a live database.

use anyhow::Result;
use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use sqlx::FromRow;

use super::Database;

/// A `mcp_settlements` row, loaded for validation, idempotent replay, and
/// `/pay/verify` reason determination.
#[derive(Debug, Clone, FromRow)]
pub struct PaymentSettlement {
    pub payment_ref: String,
    pub provider_id: String,
    pub skill_id: String,
    pub buyer_ref: Option<String>,
    pub amount_usdc: BigDecimal,
    /// `challenged` | `paying` | `paid` | `failed`.
    pub status: String,
    /// Settlement transaction hash, present once `paid`.
    pub tx_hash: Option<String>,
    /// Challenge expiry; a phase-B settle past this is rejected.
    pub valid_before: DateTime<Utc>,
    /// Set exactly once by a successful `/pay/verify` redeem.
    pub redeemed_at: Option<DateTime<Utc>>,
}

impl Database {
    /// Phase A: persist a fresh `challenged` row. `payment_ref` is a freshly
    /// minted `pr_<uuid>`, so this never conflicts in practice; `ON CONFLICT DO
    /// NOTHING` keeps it idempotent if the same ref is ever re-issued.
    #[allow(clippy::too_many_arguments)]
    pub async fn pay_insert_challenge(
        &self,
        payment_ref: &str,
        provider_id: &str,
        skill_id: &str,
        buyer_ref: Option<&str>,
        amount_usdc: &BigDecimal,
        nonce: &str,
        valid_before: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO mcp_settlements
                (payment_ref, provider_id, skill_id, buyer_ref, amount_usdc,
                 status, nonce, valid_before)
            VALUES ($1, $2, $3, $4, $5, 'challenged', $6, $7)
            ON CONFLICT (payment_ref) DO NOTHING
            "#,
        )
        .bind(payment_ref)
        .bind(provider_id)
        .bind(skill_id)
        .bind(buyer_ref)
        .bind(amount_usdc)
        .bind(nonce)
        .bind(valid_before)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Load a settlement row, if any. Used by phase B for validation /
    /// idempotent replay and by `/pay/verify` to compute the `reason` when the
    /// atomic redeem matches no row.
    pub async fn pay_load(&self, payment_ref: &str) -> Result<Option<PaymentSettlement>> {
        let row: Option<PaymentSettlement> = sqlx::query_as(
            r#"SELECT payment_ref, provider_id, skill_id, buyer_ref, amount_usdc,
                      status, tx_hash, valid_before, redeemed_at
                 FROM mcp_settlements
                WHERE payment_ref = $1"#,
        )
        .bind(payment_ref)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Phase B step 1: atomically claim a `challenged` row for settlement by
    /// flipping it to `paying`. Returns `Ok(true)` when WE won the claim (this
    /// caller owns verify+settle), `Ok(false)` when the row was not in
    /// `challenged` (already paid, already in-flight, expired-and-reaped, or
    /// missing) — the caller resolves the exact case via [`Self::pay_load`].
    ///
    /// The `valid_before > now()` guard makes expiry part of the same atomic
    /// claim, so an expired challenge can never be settled.
    ///
    /// `day_bucket` is RE-STAMPED to the current UTC date here: the per-day cap
    /// attributes spend to the day the money actually moves, not the day the
    /// challenge was issued. Without this, a challenge created just before UTC
    /// midnight and settled just after would land in the previous day's bucket
    /// and escape the new day's cap query entirely.
    pub async fn pay_claim_for_settlement(&self, payment_ref: &str) -> Result<bool> {
        let claimed = sqlx::query(
            r#"UPDATE mcp_settlements
                  SET status = 'paying',
                      day_bucket = (now() AT TIME ZONE 'UTC')::date
                WHERE payment_ref = $1
                  AND status = 'challenged'
                  AND valid_before > now()"#,
        )
        .bind(payment_ref)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(claimed > 0)
    }

    /// Phase B step 3: mark a claimed (`paying`) settlement `paid` once USDC has
    /// moved to the platform wallet, and record the split the payout worker will
    /// forward. Scoped to `paying` so no concurrent path can regress the state.
    ///
    /// The settled amount is now custodial in the platform wallet; the provider's
    /// `provider_amount` is queued for on-chain forwarding by setting
    /// `payout_status='pending'` (the worker drains it). When the provider share
    /// rounds to zero there is nothing to forward, so the payout is born `paid`.
    pub async fn pay_mark_paid(
        &self,
        payment_ref: &str,
        tx_hash: Option<&str>,
        commission_usdc: &BigDecimal,
        provider_amount_usdc: &BigDecimal,
        payout_wallet: &str,
    ) -> Result<()> {
        sqlx::query(
            r#"UPDATE mcp_settlements
                  SET status = 'paid',
                      tx_hash = $2,
                      settled_at = now(),
                      commission_usdc = $3,
                      provider_amount_usdc = $4,
                      payout_wallet = $5,
                      payout_status = CASE WHEN $4 > 0 THEN 'pending' ELSE 'paid' END
                WHERE payment_ref = $1 AND status = 'paying'"#,
        )
        .bind(payment_ref)
        .bind(tx_hash)
        .bind(commission_usdc)
        .bind(provider_amount_usdc)
        .bind(payout_wallet)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Release a claim that failed to settle (verify/settle errored or was
    /// over-cap), returning the row to `challenged` so the same `payment_ref`
    /// can be retried before it expires. Fail-closed: never touches a row that
    /// already reached `paid`.
    pub async fn pay_release_claim(&self, payment_ref: &str) -> Result<()> {
        sqlx::query(
            r#"UPDATE mcp_settlements
                  SET status = 'challenged'
                WHERE payment_ref = $1 AND status = 'paying'"#,
        )
        .bind(payment_ref)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// USDC committed for `provider_id` so far this UTC day. Counts settled
    /// (`paid`) rows AND in-flight `paying` claims, so concurrent settles
    /// project each other's spend and the per-day cap holds tighter than a
    /// paid-only sum would. Used fail-closed: if this errors, the caller
    /// rejects the settle.
    pub async fn pay_day_spend(&self, provider_id: &str) -> Result<BigDecimal> {
        let total: BigDecimal = sqlx::query_scalar(
            r#"
            SELECT COALESCE(SUM(amount_usdc), 0)
              FROM mcp_settlements
             WHERE provider_id = $1
               AND status IN ('paying', 'paid')
               AND day_bucket = (now() AT TIME ZONE 'UTC')::date
            "#,
        )
        .bind(provider_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(total)
    }

    /// THE atomic redeem (`/pay/verify`). Stamps `redeemed_at` on a `paid` row
    /// that matches `(provider_id, skill_id)` and has not been redeemed, in a
    /// single statement. Returns `Ok(true)` for the exactly-one winner; every
    /// other concurrent or repeat call gets `Ok(false)`.
    ///
    /// `skill_id` binding is mandatory: without it a cheap-skill payment could
    /// redeem an expensive skill. `buyer_ref` binding is optional per provider
    /// (`verify_bind_buyer`): when enabled, the stored buyer must match the
    /// caller's `buyer_ref` (NULL-safe), otherwise `buyer_ref` is ignored.
    pub async fn pay_redeem(
        &self,
        payment_ref: &str,
        provider_id: &str,
        skill_id: &str,
        bind_buyer: bool,
        buyer_ref: Option<&str>,
    ) -> Result<bool> {
        let redeemed = sqlx::query(
            r#"UPDATE mcp_settlements
                  SET redeemed_at = now()
                WHERE payment_ref = $1
                  AND provider_id = $2
                  AND skill_id    = $3
                  AND status      = 'paid'
                  AND redeemed_at IS NULL
                  AND ($4 = FALSE OR buyer_ref IS NOT DISTINCT FROM $5)"#,
        )
        .bind(payment_ref)
        .bind(provider_id)
        .bind(skill_id)
        .bind(bind_buyer)
        .bind(buyer_ref)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(redeemed > 0)
    }
}
