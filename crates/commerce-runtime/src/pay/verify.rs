//! `POST /pay/verify` — the provider calls this BEFORE delivering a skill.
//!
//! It is the redeem + the enforcement: a single atomic UPDATE stamps
//! `redeemed_at` on a `paid` settlement that matches `(provider_id, skill_id)`
//! and has not been redeemed. Exactly one caller can win, so one payment
//! delivers exactly one skill — even under concurrent or post-restart retries.
//! This guarantee cannot live in process memory; it lives in the row.
//!
//! Always responds `200` with `{valid, reason, skill_id}` — "not paid" is a
//! valid answer, not an error. Only a malformed body (400) or a disabled
//! provider (404) is an HTTP error.

use std::sync::Arc;

use axum::{body::Bytes, extract::State, http::StatusCode, response::Response};
use serde::Deserialize;
use tracing::error;

use crate::AppState;

use super::{error_response, find_enabled, json_response, not_found};

#[derive(Debug, Deserialize)]
struct VerifyBody {
    provider_id: Option<String>,
    payment_ref: Option<String>,
    skill_id: Option<String>,
    buyer_ref: Option<String>,
}

pub async fn verify_handler(State(state): State<Arc<AppState>>, body: Bytes) -> Response {
    let body: VerifyBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "Request body must be a JSON object.",
            )
        }
    };

    let (Some(provider_id), Some(payment_ref), Some(skill_id)) = (
        body.provider_id.as_deref().filter(|s| !s.is_empty()),
        body.payment_ref.as_deref().filter(|s| !s.is_empty()),
        body.skill_id.as_deref().filter(|s| !s.is_empty()),
    ) else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "provider_id, payment_ref, and skill_id are required.",
        );
    };

    let Some(provider) = find_enabled(&state.config.payment_providers, provider_id) else {
        return not_found();
    };
    let bind_buyer = provider.verify_bind_buyer;

    // THE atomic redeem. A returned row means we are the single winner.
    match state
        .db
        .pay_redeem(
            payment_ref,
            provider_id,
            skill_id,
            bind_buyer,
            body.buyer_ref.as_deref(),
        )
        .await
    {
        Ok(true) => json_response(
            StatusCode::OK,
            serde_json::json!({ "valid": true, "reason": null, "skill_id": skill_id }),
        ),
        Ok(false) => {
            // No row matched/won. A read-only lookup explains why.
            let reason = match state.db.pay_load(payment_ref).await {
                Ok(maybe) => redeem_failure_reason(
                    maybe,
                    provider_id,
                    skill_id,
                    bind_buyer,
                    body.buyer_ref.as_deref(),
                ),
                Err(e) => {
                    error!(%payment_ref, "pay/verify: reason lookup failed: {e}");
                    // Fail-closed: refuse delivery rather than guess `valid:true`.
                    "lookup_failed"
                }
            };
            json_response(
                StatusCode::OK,
                serde_json::json!({ "valid": false, "reason": reason, "skill_id": skill_id }),
            )
        }
        Err(e) => {
            error!(%payment_ref, "pay/verify: redeem failed: {e}");
            // Fail-closed: an infra error must never read as a valid redeem.
            json_response(
                StatusCode::OK,
                serde_json::json!({ "valid": false, "reason": "lookup_failed", "skill_id": skill_id }),
            )
        }
    }
}

/// Map a non-redeeming row to the precise `reason`. Order matters: the most
/// specific mismatch wins, and the `already_redeemed` check is placed so that a
/// concurrent verify that won the redeem between our UPDATE and this read is
/// reported as `already_redeemed`, not a false negative.
fn redeem_failure_reason(
    row: Option<crate::db::PaymentSettlement>,
    provider_id: &str,
    skill_id: &str,
    bind_buyer: bool,
    buyer_ref: Option<&str>,
) -> &'static str {
    let Some(row) = row else {
        return "not_found";
    };
    if row.provider_id != provider_id {
        return "wrong_provider";
    }
    if row.skill_id != skill_id {
        return "wrong_skill";
    }
    if row.redeemed_at.is_some() {
        return "already_redeemed";
    }
    if row.status != "paid" {
        // 'challenged'/'paying' (incl. expired-but-unpaid) and 'failed'.
        return "not_paid";
    }
    if bind_buyer && row.buyer_ref.as_deref() != buyer_ref {
        return "wrong_buyer";
    }
    // Paid, unredeemed, all bindings match: a concurrent caller just redeemed it.
    "already_redeemed"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::PaymentSettlement;
    use bigdecimal::BigDecimal;
    use chrono::{Duration, Utc};

    fn row(
        status: &str,
        provider: &str,
        skill: &str,
        redeemed: bool,
        buyer: Option<&str>,
    ) -> PaymentSettlement {
        PaymentSettlement {
            payment_ref: "pr_1".to_string(),
            provider_id: provider.to_string(),
            skill_id: skill.to_string(),
            buyer_ref: buyer.map(|s| s.to_string()),
            amount_usdc: BigDecimal::from(12),
            status: status.to_string(),
            tx_hash: Some("0xtx".to_string()),
            valid_before: Utc::now() + Duration::seconds(60),
            redeemed_at: if redeemed { Some(Utc::now()) } else { None },
        }
    }

    #[test]
    fn not_found_when_no_row() {
        assert_eq!(
            redeem_failure_reason(None, "p", "s", false, None),
            "not_found"
        );
    }

    #[test]
    fn wrong_provider_beats_other_mismatches() {
        let r = row("paid", "other", "s", false, None);
        assert_eq!(
            redeem_failure_reason(Some(r), "p", "s", false, None),
            "wrong_provider"
        );
    }

    #[test]
    fn wrong_skill_is_detected() {
        let r = row("paid", "p", "other", false, None);
        assert_eq!(
            redeem_failure_reason(Some(r), "p", "s", false, None),
            "wrong_skill"
        );
    }

    #[test]
    fn unpaid_challenge_is_not_paid() {
        let r = row("challenged", "p", "s", false, None);
        assert_eq!(
            redeem_failure_reason(Some(r), "p", "s", false, None),
            "not_paid"
        );
    }

    #[test]
    fn already_redeemed_row() {
        let r = row("paid", "p", "s", true, None);
        assert_eq!(
            redeem_failure_reason(Some(r), "p", "s", false, None),
            "already_redeemed"
        );
    }

    #[test]
    fn buyer_mismatch_only_when_bound() {
        let bound = row("paid", "p", "s", false, Some("alice"));
        assert_eq!(
            redeem_failure_reason(Some(bound), "p", "s", true, Some("bob")),
            "wrong_buyer"
        );
        // Same data, binding off: buyer is ignored, so the only remaining
        // explanation is a concurrent redeem.
        let unbound = row("paid", "p", "s", false, Some("alice"));
        assert_eq!(
            redeem_failure_reason(Some(unbound), "p", "s", false, Some("bob")),
            "already_redeemed"
        );
    }
}
