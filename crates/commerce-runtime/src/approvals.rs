//! A purchase held for a human decision.
//!
//! # The lifecycle, and who moves each edge
//!
//! ```text
//!            gateway                buyer, in the portal            gateway
//!   (none) ──────────► pending ──────────┬──────────► approved ──────────► consumed
//!                         │              └──────────► denied
//!                         └── expires_at passes ────► expired
//! ```
//!
//! * **pending → approved / denied** — the buyer, and only the buyer. Enforced
//!   by row-level security on `purchase_approvals`, which allows an UPDATE only
//!   `where status = 'pending'` and only into those two states. A buyer cannot
//!   un-deny, re-approve, or resurrect a consumed approval, and that holds in
//!   the database rather than in the UI, because the browser is an untrusted
//!   client here.
//! * **pending → expired** — nobody; time. A held purchase is a blocked agent,
//!   so it ages out on its own or a buyer who never opens the portal silently
//!   wedges their own agent forever. `src/bin/approval_sweeper.rs` does the
//!   sweep; a redemption also refuses anything past its deadline, so an unswept
//!   row is late, not dangerous.
//! * **approved → consumed** — the gateway, exactly once. This is the edge that
//!   costs real money if it is wrong, so it is a conditional UPDATE that the
//!   database settles under concurrency, not a read-then-write in Rust. Two
//!   simultaneous redemptions produce one winner and one `invalid_grant`; a
//!   retry storm produces one purchase.
//!
//! # An approval is permission for ONE purchase
//!
//! Not a bypass of the other rules. The terms recorded at hold time — the
//! amount, the destination, the seller — are re-checked at redemption, so an
//! approval for $0.05 at api.example.com can never settle $0.50 somewhere else.
//! The policy is evaluated again too: a buyer who tightened a different rule in
//! between meant it.

use anyhow::Result;
use bigdecimal::BigDecimal;
use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use crate::db::{Database, PurchaseApproval};

/// How long a held purchase waits before it ages out.
///
/// Long enough that a buyer who is asleep still gets to decide; short enough
/// that an agent is not blocked past the point where the answer still matters,
/// and that a quoted price has not drifted into fiction.
pub const HOLD_TTL_HOURS: i64 = 24;

/// Where the buyer answers. Handed back with the hold so the agent can name the
/// place rather than telling somebody to "check the portal".
pub fn approve_url() -> String {
    std::env::var("APIOSK_BUYER_PORTAL_URL")
        .ok()
        .map(|url| url.trim_end_matches('/').to_string())
        .unwrap_or_else(|| "https://buy.apiosk.com".to_string())
}

/// What to hold, and on what terms.
pub struct NewApproval<'a> {
    pub user_id: Uuid,
    pub policy_id: Option<Uuid>,
    pub token_id: Option<Uuid>,
    pub wallet_id: Option<Uuid>,
    /// The price at hold time. Redemption refuses if the live price has risen
    /// above it — a buyer approves an amount, not an intention.
    pub amount_usd: BigDecimal,
    pub destination_host: Option<&'a str>,
    pub seller: Option<&'a str>,
    pub api_slug: Option<&'a str>,
    pub operation: Option<&'a str>,
    pub matched_rule: &'a str,
    pub reason: &'a str,
    pub agent_label: Option<&'a str>,
}

/// Why a redemption did not go through.
///
/// Each of these is a different thing to tell the user, which is why they are
/// not one error. "Still waiting" and "you said no" have opposite next steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedeemFailure {
    /// No such approval, or it belongs to another connection.
    NotFound,
    /// The buyer has not answered yet.
    StillPending,
    /// The buyer said no. Do not retry.
    Denied,
    /// Nobody answered in time.
    Expired,
    /// Already used. The purchase it authorised has happened.
    AlreadyConsumed,
    /// The live price is above the one that was approved.
    PriceMoved {
        approved_usd: BigDecimal,
        current_usd: BigDecimal,
    },
}

impl RedeemFailure {
    /// The error code an agent branches on. Closed set; see `gateway/05`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound => "approval_not_found",
            Self::StillPending => "approval_pending",
            Self::Denied => "approval_denied",
            Self::Expired => "approval_expired",
            Self::AlreadyConsumed => "approval_already_used",
            Self::PriceMoved { .. } => "price_moved",
        }
    }

    /// One sentence, and it must say what to do next rather than only what went
    /// wrong.
    pub fn message(&self) -> String {
        match self {
            Self::NotFound => {
                "No held purchase with that id on this connection.".to_string()
            }
            Self::StillPending => {
                "The buyer has not answered yet. Poll GET /v1/approvals/:id; do not retry the purchase."
                    .to_string()
            }
            Self::Denied => {
                "The buyer declined this purchase. Do not retry it — ask them what they would rather do."
                    .to_string()
            }
            Self::Expired => {
                "The hold expired before anyone answered. Get a fresh price and ask again."
                    .to_string()
            }
            Self::AlreadyConsumed => {
                "That approval has already been used. It authorises exactly one purchase."
                    .to_string()
            }
            Self::PriceMoved {
                approved_usd,
                current_usd,
            } => format!(
                "The price is now {current_usd} USDC; the buyer approved {approved_usd} USDC. \
                 Nothing was paid. Show them the new price and ask again."
            ),
        }
    }
}

impl Database {
    /// Park a purchase for a human.
    pub async fn create_purchase_approval(
        &self,
        input: NewApproval<'_>,
    ) -> Result<PurchaseApproval> {
        let expires_at = Utc::now() + Duration::hours(HOLD_TTL_HOURS);

        let row = sqlx::query_as::<_, ApprovalRow>(
            r#"INSERT INTO purchase_approvals
                 (user_id, policy_id, token_id, wallet_id, amount_usd, destination_host,
                  seller, api_slug, operation, matched_rule, reason, agent_label,
                  status, expires_at, source)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 'pending', $13, 'gateway')
               RETURNING id, user_id, policy_id, token_id, wallet_id, amount_usd,
                         destination_host, seller, api_slug, operation, matched_rule,
                         reason, status, expires_at, decided_at, consumed_at, created_at"#,
        )
        .bind(input.user_id)
        .bind(input.policy_id)
        .bind(input.token_id)
        .bind(input.wallet_id)
        .bind(&input.amount_usd)
        .bind(input.destination_host)
        .bind(input.seller)
        .bind(input.api_slug)
        .bind(input.operation)
        .bind(input.matched_rule)
        .bind(input.reason)
        .bind(input.agent_label)
        .bind(expires_at)
        .fetch_one(&self.pool)
        .await?;

        Ok(row.into_approval())
    }

    /// Read one approval, scoped to the connection that created it.
    ///
    /// Scoped, not merely filtered: an approval id is a bearer-ish handle, and
    /// letting any connection read any hold would leak what a buyer is buying to
    /// anyone who can guess a UUID.
    pub async fn get_purchase_approval(
        &self,
        id: Uuid,
        token_id: Option<Uuid>,
    ) -> Result<Option<PurchaseApproval>> {
        let row = sqlx::query_as::<_, ApprovalRow>(
            r#"SELECT id, user_id, policy_id, token_id, wallet_id, amount_usd,
                      destination_host, seller, api_slug, operation, matched_rule,
                      reason, status, expires_at, decided_at, consumed_at, created_at
                 FROM purchase_approvals
                WHERE id = $1
                  AND ($2::uuid IS NULL OR token_id = $2)
                LIMIT 1"#,
        )
        .bind(id)
        .bind(token_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(ApprovalRow::into_approval))
    }

    /// Claim an approved hold, exactly once.
    ///
    /// The single-use guarantee is the `WHERE status = 'approved'` on the UPDATE
    /// itself: two simultaneous redemptions both run the statement, one updates
    /// a row and one updates nothing. No lock held across a decision in Rust, no
    /// window between a read and a write, and no way for a retry storm to turn
    /// one approval into two purchases.
    ///
    /// The price check is in the same statement for the same reason.
    pub async fn redeem_purchase_approval(
        &self,
        id: Uuid,
        token_id: Option<Uuid>,
        current_price_usd: &BigDecimal,
    ) -> Result<Result<PurchaseApproval, RedeemFailure>> {
        let row = sqlx::query_as::<_, ApprovalRow>(
            r#"UPDATE purchase_approvals
                  SET status = 'consumed', consumed_at = now()
                WHERE id = $1
                  AND ($2::uuid IS NULL OR token_id = $2)
                  AND status = 'approved'
                  AND expires_at > now()
                  AND amount_usd >= $3
              RETURNING id, user_id, policy_id, token_id, wallet_id, amount_usd,
                        destination_host, seller, api_slug, operation, matched_rule,
                        reason, status, expires_at, decided_at, consumed_at, created_at"#,
        )
        .bind(id)
        .bind(token_id)
        .bind(current_price_usd)
        .fetch_optional(&self.pool)
        .await?;

        if let Some(row) = row {
            return Ok(Ok(row.into_approval()));
        }

        // The update matched nothing. Read the row to say WHY, because "no" and
        // "not yet" and "you already did" are three different things to tell a
        // user, and an agent that cannot tell them apart will retry the one it
        // must not.
        let Some(approval) = self.get_purchase_approval(id, token_id).await? else {
            return Ok(Err(RedeemFailure::NotFound));
        };

        let failure = match approval.status.as_str() {
            "pending" if approval.expires_at <= Utc::now() => RedeemFailure::Expired,
            "pending" => RedeemFailure::StillPending,
            "denied" => RedeemFailure::Denied,
            "expired" => RedeemFailure::Expired,
            "consumed" => RedeemFailure::AlreadyConsumed,
            // Approved, unexpired, and still refused: the only remaining reason
            // is the price.
            "approved" if approval.expires_at <= Utc::now() => RedeemFailure::Expired,
            "approved" => RedeemFailure::PriceMoved {
                approved_usd: approval.amount_usd.clone(),
                current_usd: current_price_usd.clone(),
            },
            _ => RedeemFailure::NotFound,
        };
        Ok(Err(failure))
    }

    /// Age out holds nobody answered. Returns how many were swept.
    ///
    /// Idempotent and safe to run from more than one place: the `WHERE` clause
    /// is the whole condition, so a second sweeper finds nothing to do rather
    /// than double-counting.
    pub async fn expire_stale_approvals(&self) -> Result<u64> {
        let result = sqlx::query(
            r#"UPDATE purchase_approvals
                  SET status = 'expired'
                WHERE status = 'pending'
                  AND expires_at <= now()"#,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}

#[derive(sqlx::FromRow)]
struct ApprovalRow {
    id: Uuid,
    user_id: Uuid,
    policy_id: Option<Uuid>,
    token_id: Option<Uuid>,
    wallet_id: Option<Uuid>,
    amount_usd: BigDecimal,
    destination_host: Option<String>,
    seller: Option<String>,
    api_slug: Option<String>,
    operation: Option<String>,
    matched_rule: Option<String>,
    reason: Option<String>,
    status: String,
    expires_at: DateTime<Utc>,
    decided_at: Option<DateTime<Utc>>,
    consumed_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

impl ApprovalRow {
    fn into_approval(self) -> PurchaseApproval {
        PurchaseApproval {
            id: self.id,
            user_id: self.user_id,
            policy_id: self.policy_id,
            token_id: self.token_id,
            wallet_id: self.wallet_id,
            amount_usd: self.amount_usd,
            destination_host: self.destination_host,
            seller: self.seller,
            api_slug: self.api_slug,
            operation: self.operation,
            matched_rule: self.matched_rule,
            reason: self.reason,
            status: self.status,
            expires_at: self.expires_at,
            decided_at: self.decided_at,
            consumed_at: self.consumed_at,
            created_at: self.created_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every failure has to be distinguishable by an agent, because two of them
    /// mean "retry later" and two mean "never retry this".
    #[test]
    fn each_refusal_has_its_own_code_and_its_own_next_step() {
        let cases = [
            RedeemFailure::NotFound,
            RedeemFailure::StillPending,
            RedeemFailure::Denied,
            RedeemFailure::Expired,
            RedeemFailure::AlreadyConsumed,
            RedeemFailure::PriceMoved {
                approved_usd: BigDecimal::from(1),
                current_usd: BigDecimal::from(2),
            },
        ];

        let mut codes: Vec<&str> = cases.iter().map(RedeemFailure::code).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), cases.len(), "two refusals share one code");

        for case in &cases {
            assert!(!case.message().is_empty());
        }
    }

    #[test]
    fn a_moved_price_names_both_numbers() {
        // A buyer approved an amount, not an intention. Saying only "the price
        // changed" leaves the agent unable to tell the user what to approve.
        let failure = RedeemFailure::PriceMoved {
            approved_usd: BigDecimal::from(1),
            current_usd: BigDecimal::from(2),
        };
        let message = failure.message();
        assert!(message.contains('1'), "{message}");
        assert!(message.contains('2'), "{message}");
        assert_eq!(failure.code(), "price_moved");
    }

    #[test]
    fn refusals_that_must_never_be_retried_say_so() {
        assert!(RedeemFailure::Denied.message().contains("Do not retry"));
        assert!(RedeemFailure::StillPending
            .message()
            .contains("do not retry"));
    }
}
