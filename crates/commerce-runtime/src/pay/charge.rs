//! `POST /pay/charge` — two phases on one endpoint, distinguished by whether
//! the body carries a `payment_ref` (+ proof):
//!
//!   * Phase A (no `payment_ref`): issue an x402 challenge for `(skill_id,
//!     price)` and record a `challenged` settlement row.
//!   * Phase B (`payment_ref` + proof): atomically claim the challenge, enforce
//!     caps (fail-closed), verify + settle the payment through the gateway's
//!     pluggable x402 verifier, mark it `paid`, and emit a receipt.

use std::str::FromStr;
use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Response,
};
use bigdecimal::{BigDecimal, Zero};
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::Value;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::db::NetworkToken;
use crate::payment::{extract_payment_proof, settle_payment, verify_payment, PaymentRequirement};
use crate::AppState;

use super::{error_response, find_enabled, json_response, not_found, PaymentProvider};

#[derive(Debug, Deserialize)]
struct ChargeBody {
    provider_id: Option<String>,
    // Phase A.
    skill_id: Option<String>,
    price_usdc: Option<String>,
    buyer_ref: Option<String>,
    // Phase B.
    payment_ref: Option<String>,
    /// x402 payment proof (raw JSON or base64). May instead arrive in the
    /// `X-Payment` header, matching the rest of the gateway.
    payment: Option<String>,
}

pub async fn charge_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let body: ChargeBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "Request body must be a JSON object.",
            )
        }
    };

    let Some(provider_id) = body.provider_id.as_deref().filter(|s| !s.is_empty()) else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "provider_id is required.",
        );
    };

    // Disabled/unknown provider is inert (404) — and resolving it first means a
    // probe learns nothing about configured-but-disabled providers.
    let Some(provider) = find_enabled(&state.config.payment_providers, provider_id) else {
        return not_found();
    };
    let provider = provider.clone();

    if body.payment_ref.is_some() {
        let proof = body
            .payment
            .clone()
            .or_else(|| extract_payment_proof(&headers));
        phase_b_settle(&state, &provider, &body, proof).await
    } else {
        phase_a_challenge(&state, &provider, &body).await
    }
}

/// Phase A: bound the requested price, persist a `challenged` row, and return a
/// 402 carrying a standard x402 `accepts` requirement plus the `payment_ref`.
async fn phase_a_challenge(
    state: &AppState,
    provider: &PaymentProvider,
    body: &ChargeBody,
) -> Response {
    let Some(skill_id) = body.skill_id.as_deref().filter(|s| !s.is_empty()) else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "skill_id is required.",
        );
    };

    // The buyer pays the PLATFORM wallet (custodial settle), and the provider's
    // 98% is forwarded on-chain later by developer_payout_worker — identical to
    // the slug flow. We still require provider.settle_wallet here: it is the
    // payout DESTINATION, denormalized onto the row at settle time, so a provider
    // with no wallet can never be settled-but-unpayable.
    if provider
        .settle_wallet
        .as_deref()
        .filter(|w| !w.is_empty())
        .is_none()
    {
        // Operator error: an enabled provider with no wallet can't be paid out.
        error!(
            provider_id = %provider.id,
            "pay/charge: enabled provider has no settle_wallet configured"
        );
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "provider_unavailable",
            "This provider is not currently able to accept payments.",
        );
    }
    let platform_wallet = state.config.platform_wallet.as_str();

    // Price comes from the request (trusted_request model). The bounds below
    // are the load-bearing guardrails of that model.
    let Some(raw_price) = body.price_usdc.as_deref().and_then(parse_price) else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "price_usdc must be a positive decimal string.",
        );
    };
    // Quantize to USDC precision (6 dp) BEFORE bounding/storing so the advertised
    // challenge amount, the stored amount, and the phase-B requirement rebuilt
    // from it are byte-identical. The facilitator binds the EIP-712 signature to
    // an exact atomic amount, so a phase-A-truncates / DB-rounds-up divergence
    // would reject an otherwise-valid payment.
    // raw_price is strictly positive, so rounding can only land it at zero,
    // never negative — an is_zero() check is sufficient and precise.
    let price = raw_price.round(6);
    if price.is_zero() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "price_usdc is below the minimum supported precision (0.000001 USDC).",
        );
    }

    if let Some(max) = provider.pricing.max_per_skill() {
        if price > max {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "price_rejected",
                "price_usdc exceeds the configured maximum for a single skill.",
            );
        }
    }
    if let Some(cap_tx) = provider.cap_per_tx() {
        if price > cap_tx {
            return error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "cap_exceeded",
                "price_usdc exceeds the per-transaction cap.",
            );
        }
    }

    // ── Hybrid fee model ────────────────────────────────────────────────────
    // `price` is the provider's LIST price and the caps above bound it. What the
    // buyer is challenged for, and what is stored and later settled, is the
    // BUYER TOTAL: list + 15%. A skill sold through Apiosk is managed supply, so
    // the provider receives list − 10% at settlement.
    //
    // The challenge amount and the stored amount must stay byte-identical (the
    // facilitator binds the EIP-712 signature to an exact atomic amount), so the
    // gross-up happens once, here, and everything downstream uses the result.
    let quote = crate::fees::quote_usdc(&price, crate::fees::ProviderClass::Managed);
    let buyer_total = quote.buyer_total_usdc();

    let ttl_secs = if provider.challenge_ttl_secs > 0 {
        provider.challenge_ttl_secs
    } else {
        900
    };
    let valid_before = Utc::now() + Duration::seconds(ttl_secs);
    let payment_ref = format!("pr_{}", Uuid::new_v4());
    // Opaque challenge nonce (32 bytes). Anti-double-charge is enforced by the
    // per-payment_ref idempotency and the on-chain EIP-3009 nonce, not this.
    let nonce = format!("0x{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());

    if let Err(e) = state
        .db
        .pay_insert_challenge(
            &payment_ref,
            &provider.id,
            skill_id,
            body.buyer_ref.as_deref(),
            &buyer_total,
            &nonce,
            valid_before,
        )
        .await
    {
        error!(provider_id = %provider.id, "pay/charge: failed to persist challenge: {e}");
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "Could not create the payment challenge.",
        );
    }

    let requirement = PaymentRequirement::new(
        platform_wallet,
        &buyer_total,
        &format!("apiosk:pay:{}:{}", provider.id, skill_id),
        &format!("Apiosk direct purchase ({skill_id})"),
        &usdc_base_token(),
    );

    let requirement_json = serde_json::to_value(&requirement).unwrap_or(Value::Null);
    let amount = fmt_usdc(&buyer_total);
    let challenge = serde_json::json!({
        "x402Version": 1,
        "error": "Payment required",
        "accepts": [requirement_json],
        "payment_ref": payment_ref,
        "provider_id": provider.id,
        "skill_id": skill_id,
        "amount_usdc": amount,
        "asset": provider.asset,
        "chain": provider.chain,
        // The buyer sees all three numbers, never just the total. A charge that
        // exceeds the price the caller asked for is only defensible if the
        // tariff is stated alongside it.
        "pricing": quote.buyer_disclosure(),
        "expires_at": valid_before.to_rfc3339(),
    });

    let mut resp = json_response(StatusCode::PAYMENT_REQUIRED, challenge);
    let h = resp.headers_mut();
    h.insert("X-Payment-Protocol", header_val("x402"));
    h.insert("X-Payment-Amount", header_val(&amount));
    h.insert("X-Payment-Currency", header_val("USDC"));
    h.insert("X-Payment-Ref", header_val(&payment_ref));
    resp
}

/// Phase B: claim → cap-check → verify → settle → mark paid → receipt.
/// Every failure after the claim releases it (fail-closed, retriable).
async fn phase_b_settle(
    state: &AppState,
    provider: &PaymentProvider,
    body: &ChargeBody,
    proof: Option<String>,
) -> Response {
    let payment_ref = body.payment_ref.as_deref().unwrap_or_default();

    let Some(proof) = proof.filter(|p| !p.trim().is_empty()) else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Missing payment proof (body `payment`, or the PAYMENT-SIGNATURE / X-Payment header).",
        );
    };

    let row = match state.db.pay_load(payment_ref).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            return error_response(
                StatusCode::CONFLICT,
                "payment_invalid",
                "Unknown payment_ref.",
            )
        }
        Err(e) => {
            error!(%payment_ref, "pay/charge: load failed: {e}");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "Lookup failed.",
            );
        }
    };

    // The payment_ref must belong to the provider making the request.
    if row.provider_id != provider.id {
        return error_response(
            StatusCode::CONFLICT,
            "payment_invalid",
            "payment_ref does not belong to this provider.",
        );
    }

    // Idempotent replay / in-flight handling.
    match row.status.as_str() {
        "paid" => {
            return json_response(
                StatusCode::OK,
                paid_body(
                    &row.payment_ref,
                    &row.provider_id,
                    &row.skill_id,
                    &row.amount_usdc,
                    row.tx_hash.as_deref(),
                ),
            );
        }
        "paying" => {
            return error_response(
                StatusCode::CONFLICT,
                "settlement_in_progress",
                "This payment is already being settled; retry shortly.",
            );
        }
        "failed" => {
            return error_response(
                StatusCode::CONFLICT,
                "payment_invalid",
                "This payment_ref is no longer valid.",
            );
        }
        _ => {} // 'challenged' → proceed
    }

    if row.valid_before <= Utc::now() {
        return error_response(
            StatusCode::CONFLICT,
            "challenge_expired",
            "The payment challenge has expired; request a new one.",
        );
    }

    // Atomic claim (challenged → paying), which also re-checks expiry. Exactly
    // one caller wins; a loser either replays (now paid) or backs off.
    match state.db.pay_claim_for_settlement(payment_ref).await {
        Ok(true) => {}
        Ok(false) => {
            // Lost the race or expired-at-claim — re-read to answer precisely.
            if let Ok(Some(latest)) = state.db.pay_load(payment_ref).await {
                if latest.status == "paid" {
                    return json_response(
                        StatusCode::OK,
                        paid_body(
                            &latest.payment_ref,
                            &latest.provider_id,
                            &latest.skill_id,
                            &latest.amount_usdc,
                            latest.tx_hash.as_deref(),
                        ),
                    );
                }
                if latest.valid_before <= Utc::now() {
                    return error_response(
                        StatusCode::CONFLICT,
                        "challenge_expired",
                        "The payment challenge has expired; request a new one.",
                    );
                }
            }
            return error_response(
                StatusCode::CONFLICT,
                "settlement_in_progress",
                "This payment is already being settled; retry shortly.",
            );
        }
        Err(e) => {
            error!(%payment_ref, "pay/charge: claim failed: {e}");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "Claim failed.",
            );
        }
    }

    // From here on, every early return MUST release the claim.
    let amount = row.amount_usdc.clone();

    // Per-tx cap against the STORED amount (never the request).
    if let Some(cap_tx) = provider.cap_per_tx() {
        if amount > cap_tx {
            let _ = state.db.pay_release_claim(payment_ref).await;
            return error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "cap_exceeded",
                "Amount exceeds the per-transaction cap.",
            );
        }
    }

    // Per-day cap, fail-closed: the claim is already counted by pay_day_spend,
    // so spent_today includes this amount. A query error rejects the settle.
    if let Some(cap_day) = provider.cap_per_day() {
        match state.db.pay_day_spend(&provider.id).await {
            Ok(spent_today) => {
                if spent_today > cap_day {
                    let _ = state.db.pay_release_claim(payment_ref).await;
                    return error_response(
                        StatusCode::TOO_MANY_REQUESTS,
                        "cap_exceeded",
                        "This purchase would exceed the per-day cap.",
                    );
                }
            }
            Err(e) => {
                error!(provider_id = %provider.id, "pay/charge: day-spend query failed: {e}");
                let _ = state.db.pay_release_claim(payment_ref).await;
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "cap_check_unavailable",
                    "Could not enforce the spend cap; settle refused.",
                );
            }
        }
    }

    // The provider wallet is the PAYOUT destination (where the worker forwards
    // the 98%), resolved here so a provider that lost its wallet between phase A
    // and B fails closed rather than settling money it can't pay out.
    let payout_wallet = match provider.settle_wallet.as_deref().filter(|w| !w.is_empty()) {
        Some(w) => w.to_string(),
        None => {
            let _ = state.db.pay_release_claim(payment_ref).await;
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "provider_unavailable",
                "This provider is not currently able to accept payments.",
            );
        }
    };
    // Rebuild the requirement from the STORED amount + PLATFORM wallet, so a
    // tampered amount/payTo can never settle (the facilitator validates the
    // EIP-712 signature against exactly these fields). The buyer pays the
    // platform; the split to `payout_wallet` happens off this settle.
    let requirement = PaymentRequirement::new(
        state.config.platform_wallet.as_str(),
        &amount,
        &format!("apiosk:pay:{}:{}", provider.id, row.skill_id),
        &format!("Apiosk direct purchase ({})", row.skill_id),
        &usdc_base_token(),
    );

    // Verify, then settle, through the gateway's pluggable x402 verifier.
    if let Err(e) = verify_payment(
        &state.http_client,
        &state.config,
        &state.facilitator_url,
        &proof,
        &requirement,
    )
    .await
    {
        warn!(%payment_ref, provider_id = %provider.id, "pay/charge: verify failed: {e}");
        let _ = state.db.pay_release_claim(payment_ref).await;
        return error_response(
            StatusCode::CONFLICT,
            "payment_invalid",
            "Payment verification failed.",
        );
    }

    let settle = match settle_payment(
        &state.http_client,
        &state.config,
        &state.facilitator_url,
        &proof,
        &requirement,
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            error!(%payment_ref, provider_id = %provider.id, "pay/charge: settle failed: {e}");
            // Fail-closed: release the claim so the row is retriable, never paid.
            let _ = state.db.pay_release_claim(payment_ref).await;
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "settlement_failed",
                "Settlement failed; no charge was recorded.",
            );
        }
    };

    // Split the settled amount under the hybrid model: the provider's share is
    // forwarded on-chain by developer_payout_worker, the Apiosk fee stays in the
    // platform wallet.
    //
    // `amount` is the STORED gross — the buyer total the challenge was signed
    // for — so the list price has to be recovered from it. `from_buyer_total`
    // floors in Apiosk's direction, so the platform can never owe out more than
    // it took in; the dust lands in the fee, as it did before.
    let settled_quote = crate::fees::from_buyer_total(
        crate::fees::usdc_to_atomic(&amount),
        crate::fees::ProviderClass::Managed,
    );
    let commission_usdc = settled_quote.apiosk_take_usdc();
    let provider_amount_usdc = settled_quote.provider_payout_usdc();

    let tx_hash = settle.transaction.clone();
    if let Err(e) = state
        .db
        .pay_mark_paid(
            payment_ref,
            tx_hash.as_deref(),
            &commission_usdc,
            &provider_amount_usdc,
            &payout_wallet,
        )
        .await
    {
        // USDC moved but we failed to record it. Do NOT release (that would
        // re-open a settled charge). Surface an error; reconciliation uses the
        // tx_hash + receipt below.
        error!(%payment_ref, ?tx_hash, "pay/charge: settled but mark_paid failed: {e}");
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "Payment settled but could not be recorded; contact support with the payment_ref.",
        );
    }

    emit_receipt(
        &row.provider_id,
        &row.skill_id,
        &amount,
        &commission_usdc,
        &provider_amount_usdc,
        &payout_wallet,
        payment_ref,
        row.buyer_ref.as_deref(),
        tx_hash.as_deref(),
    );

    json_response(
        StatusCode::OK,
        paid_body(
            payment_ref,
            &provider.id,
            &row.skill_id,
            &amount,
            tx_hash.as_deref(),
        ),
    )
}

/// The `200 OK` paid body, identical for a fresh settle and an idempotent replay.
fn paid_body(
    payment_ref: &str,
    provider_id: &str,
    skill_id: &str,
    amount: &BigDecimal,
    tx_hash: Option<&str>,
) -> Value {
    serde_json::json!({
        "payment_ref": payment_ref,
        "status": "paid",
        "provider_id": provider_id,
        "skill_id": skill_id,
        "amount_usdc": fmt_usdc(amount),
        "tx_hash": tx_hash,
        "redeemable": true,
    })
}

/// Reconciliation receipt (provider pays its creators from this). Emitted at
/// settle time as a structured log; the durable record is the `mcp_settlements`
/// row. Never logs the proof, nonce, or any secret.
#[allow(clippy::too_many_arguments)]
fn emit_receipt(
    provider_id: &str,
    skill_id: &str,
    amount: &BigDecimal,
    commission_usdc: &BigDecimal,
    provider_amount_usdc: &BigDecimal,
    payout_wallet: &str,
    payment_ref: &str,
    buyer_ref: Option<&str>,
    tx_hash: Option<&str>,
) {
    let receipt = serde_json::json!({
        "type": "skill_purchase",
        "provider_id": provider_id,
        "skill_id": skill_id,
        "amount_usdc": fmt_usdc(amount),
        "commission_usdc": fmt_usdc(commission_usdc),
        "provider_amount_usdc": fmt_usdc(provider_amount_usdc),
        "payout_wallet": payout_wallet,
        "payment_ref": payment_ref,
        "buyer_ref": buyer_ref,
        "tx_hash": tx_hash,
        "ts": Utc::now().to_rfc3339(),
    });
    info!(target: "apiosk::pay::receipt", receipt = %receipt, "skill purchase settled");
}

// `commission_bps` and `split_usdc` lived here: a single rate applied to the
// gross, which is what the retired flat-rate model needed. The hybrid model is
// two-sided (buyer +10%, managed provider −10%) and cannot be expressed as one
// rate out of one gross, so the split comes from `crate::fees` — see
// `from_buyer_total`, which recovers it from the stored challenge amount.

fn fmt_usdc(d: &BigDecimal) -> String {
    d.to_string()
}

/// Parse a strictly-positive decimal price; reject zero, negatives, garbage.
fn parse_price(s: &str) -> Option<BigDecimal> {
    BigDecimal::from_str(s.trim())
        .ok()
        .filter(|d| d > &BigDecimal::from(0))
}

fn header_val(s: &str) -> axum::http::HeaderValue {
    axum::http::HeaderValue::from_str(s)
        .unwrap_or_else(|_| axum::http::HeaderValue::from_static(""))
}

/// Canonical USDC on Base, used to shape the x402 payment requirement. Matches
/// the constants the slug payment path relies on.
fn usdc_base_token() -> NetworkToken {
    NetworkToken {
        id: Uuid::nil(),
        network_name: "base".to_string(),
        network_display_name: "Base".to_string(),
        chain_id: 8453,
        explorer_url: Some("https://basescan.org".to_string()),
        is_testnet: false,
        token_symbol: "USDC".to_string(),
        token_name: "USD Coin".to_string(),
        token_decimals: 6,
        contract_address: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_price_requires_strictly_positive() {
        assert!(parse_price("12.00").is_some());
        assert!(parse_price("0").is_none(), "zero is not a chargeable price");
        assert!(parse_price("-1").is_none(), "negatives rejected");
        assert!(parse_price("abc").is_none(), "garbage rejected");
        assert_eq!(parse_price(" 12.00 "), BigDecimal::from_str("12.00").ok());
    }

    #[test]
    fn price_quantizes_to_six_decimals() {
        // The phase-A quantization the settle path relies on: round to USDC
        // precision so phase A, the stored row, and phase B all agree.
        assert_eq!(
            BigDecimal::from_str("12.9999999").unwrap().round(6),
            BigDecimal::from_str("13.000000").unwrap()
        );
        assert_eq!(
            BigDecimal::from_str("0.1234564").unwrap().round(6),
            BigDecimal::from_str("0.123456").unwrap()
        );
        // A price below USDC precision rounds to zero and must be rejected.
        assert!(BigDecimal::from_str("0.0000001")
            .unwrap()
            .round(6)
            .is_zero());
    }

    /// The worked example on the skill rail: a $0.10 skill is challenged at
    /// $0.115 and settles $0.09 to the provider, $0.025 to Apiosk.
    #[test]
    fn a_ten_cent_skill_charges_eleven_and_a_half_and_pays_nine() {
        let list = BigDecimal::from_str("0.10").unwrap();
        let quote = crate::fees::quote_usdc(&list, crate::fees::ProviderClass::Managed);
        assert_eq!(
            quote.buyer_total_usdc(),
            BigDecimal::from_str("0.115").unwrap()
        );

        // Phase B splits the STORED gross, not the list price.
        let settled = crate::fees::from_buyer_total(
            crate::fees::usdc_to_atomic(&quote.buyer_total_usdc()),
            crate::fees::ProviderClass::Managed,
        );
        assert_eq!(
            settled.provider_payout_usdc(),
            BigDecimal::from_str("0.09").unwrap()
        );
        assert_eq!(
            settled.apiosk_take_usdc(),
            BigDecimal::from_str("0.025").unwrap()
        );
    }

    /// The invariant the retired `split_usdc` existed to hold, restated against
    /// the hybrid split: the two shares reconstruct the settled gross exactly,
    /// and the provider is never owed more than actually settled.
    #[test]
    fn the_settled_split_is_exact_and_the_provider_is_never_overpaid() {
        for amount in ["12.000000", "0.011000", "0.000001", "1.000000", "0.000007"] {
            let amt = BigDecimal::from_str(amount).unwrap();
            let q = crate::fees::from_buyer_total(
                crate::fees::usdc_to_atomic(&amt),
                crate::fees::ProviderClass::Managed,
            );
            let provider = q.provider_payout_usdc();
            let commission = q.apiosk_take_usdc();
            assert_eq!(
                &commission + &provider,
                amt,
                "split must sum to the settled gross for {amount}"
            );
            assert!(provider <= amt, "provider share exceeds gross for {amount}");
            assert!(commission >= 0);
        }
    }

    #[test]
    fn paid_body_is_stable_shape() {
        let v = paid_body(
            "pr_1",
            "prov",
            "skill",
            &BigDecimal::from(12),
            Some("0xabc"),
        );
        assert_eq!(v["payment_ref"], "pr_1");
        assert_eq!(v["status"], "paid");
        assert_eq!(v["provider_id"], "prov");
        assert_eq!(v["skill_id"], "skill");
        assert_eq!(v["amount_usdc"], "12");
        assert_eq!(v["tx_hash"], "0xabc");
        assert_eq!(v["redeemable"], true);
    }
}
