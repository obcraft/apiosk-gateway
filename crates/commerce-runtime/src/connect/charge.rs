//! 04-02 — skill charge paywall: `GET /v1/skills/{id}/charge`.
//!
//! Public entry an agent hits to pay for a skill. With no `X-PAYMENT` proof it
//! returns an x402 `402` whose `payTo` is the PLATFORM wallet and whose body
//! carries the fee split from [`compute_split`] (04-01).
//!
//! Settlement model (least-work, no splitter contract): the agent pays the gross
//! to the platform wallet via the gateway's existing facilitator; the seller /
//! marketplace / Apiosk breakdown (2% Apiosk, x% marketplace, remainder to the
//! merchant) is recorded and disbursed at payout. The `apiosk.split` block in the
//! 402 is the informational breakdown so an agent can see where its USDC goes.
//!
//! Verifying + recording a submitted payment (04-03/04-04, facilitator-based) is
//! not wired yet, so a request that DOES carry `X-PAYMENT` gets a clear "not
//! available yet" 503 rather than a misleading second 402.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::Response,
};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde_json::{json, Value};
use tracing::{error, warn};
use uuid::Uuid;

use super::fee::{compute_split, Split};
use super::purpose::sanitize_purpose;
use super::skills::{
    assert_merchant_ready, build_charge_url, is_unique_violation, GATEWAY_PUBLIC_BASE,
};
use super::{json_error, json_response};
use crate::payment::{
    decode_payment_payload, evm_requirement_extra, settle_payment, verify_payment,
    PaymentRequirement, PAYMENT_HEADER, PAYMENT_RESPONSE_HEADER,
};
use crate::AppState;

/// Header carrying the optional purpose note (04-07).
const PURPOSE_HEADER: &str = "x-apiosk-purpose";

pub async fn charge_skill(
    State(state): State<Arc<AppState>>,
    Path(skill_id): Path<Uuid>,
    headers: HeaderMap,
) -> Response {
    // Settle model (least-work, no splitter contract): the gross is paid to the
    // PLATFORM wallet via the existing facilitator, then the seller / marketplace /
    // Apiosk split is disbursed at payout. `payTo` is therefore the platform wallet
    // (always configured); we only need the settlement asset (USDC), which defaults.
    let pay_to = state.config.platform_wallet.as_str();
    let Some(asset) = state.config.connect_settlement_asset.as_deref() else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "settlement_not_configured",
            "Skill charging is not enabled on this gateway yet.",
        );
    };

    let skill = match state.db.get_connect_skill_charge(skill_id).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return json_error(StatusCode::NOT_FOUND, "skill_not_found", "Skill not found.")
        }
        Err(err) => {
            warn!("connect skill charge lookup failed: {err}");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "lookup_failed",
                "Could not load the skill.",
            );
        }
    };

    if !skill.active {
        return json_error(
            StatusCode::CONFLICT,
            "skill_inactive",
            "This skill is not available for purchase.",
        );
    }
    // 03-03 guard, re-checked at charge time: a merchant may have been suspended or
    // lost its wallet since the skill was onboarded.
    if let Err(readiness) = assert_merchant_ready(&skill.merchant_status, skill.has_active_wallet) {
        let (code, msg) = readiness.as_error();
        return json_error(StatusCode::CONFLICT, code, msg);
    }
    // USDC-only in v1; never advertise a 402 for a token we can't settle.
    if !skill.price_currency.eq_ignore_ascii_case("USDC") {
        return json_error(
            StatusCode::CONFLICT,
            "unsupported_currency",
            "Only USDC skills can be charged.",
        );
    }

    let split = compute_split(
        skill.price_amount,
        skill.marketplace_fee_bps as i64,
        skill.apiosk_fee_bps as i64,
    );

    // No payment proof yet → the paywall: return the priced 402.
    let Some(proof) = headers.get(PAYMENT_HEADER).and_then(|v| v.to_str().ok()) else {
        let body = build_charge_402_body(&Charge402 {
            skill_id,
            marketplace_id: skill.marketplace_id,
            gross: skill.price_amount,
            network: &state.config.connect_network,
            asset,
            pay_to,
            resource: &build_charge_url(GATEWAY_PUBLIC_BASE, skill_id),
            seller_wallet: &skill.destination_wallet,
            split,
        });
        return payment_required(body, skill.price_amount);
    };

    // Payment submitted → verify + settle to the platform wallet + record + enqueue
    // the split (04-03/04-04/04-06).
    settle_connect_charge(
        &state, skill_id, &skill, split, pay_to, asset, proof, &headers,
    )
    .await
}

/// Verify a submitted payment via the existing facilitator, settle the gross to
/// the platform wallet, record the settled payment with its split, and enqueue the
/// merchant + marketplace payout legs (the worker sends them; Apiosk's 2% stays in
/// the platform wallet). Returns `200` + `PAYMENT-RESPONSE` on success.
#[allow(clippy::too_many_arguments)]
async fn settle_connect_charge(
    state: &Arc<AppState>,
    skill_id: Uuid,
    skill: &crate::db::connect::ConnectSkillCharge,
    split: Split,
    pay_to: &str,
    asset: &str,
    proof: &str,
    headers: &HeaderMap,
) -> Response {
    let requirement = build_connect_requirement(
        skill_id,
        skill.price_amount,
        &state.config.connect_network,
        asset,
        pay_to,
        &build_charge_url(GATEWAY_PUBLIC_BASE, skill_id),
    );

    // 1. Verify the EIP-3009 authorization (asset, network, amount, payTo, expiry).
    if let Err(err) = verify_payment(
        &state.http_client,
        &state.config,
        &state.config.facilitator_url,
        proof,
        &requirement,
    )
    .await
    {
        warn!("connect charge verify failed for skill {skill_id}: {err}");
        return json_error(
            StatusCode::PAYMENT_REQUIRED,
            "payment_invalid",
            "The submitted payment could not be verified.",
        );
    }

    // 2. Replay guard on the authorization nonce (the partial-unique index is the
    // hard backstop; this returns a clean 409 before we settle).
    let (nonce, payer) = decode_nonce_and_payer(proof);
    if let Some(n) = nonce.as_deref() {
        if matches!(state.db.connect_payment_nonce_seen(n).await, Ok(true)) {
            return json_error(
                StatusCode::CONFLICT,
                "already_settled",
                "This payment has already been settled.",
            );
        }
    }

    // 3. Settle: the facilitator moves the gross to the platform wallet.
    let tx = match settle_payment(
        &state.http_client,
        &state.config,
        &state.config.facilitator_url,
        proof,
        &requirement,
    )
    .await
    {
        Ok(resp) if resp.success => resp.transaction,
        Ok(resp) => {
            warn!(
                "connect charge settle rejected for skill {skill_id}: {:?}",
                resp.error_reason
            );
            return json_error(
                StatusCode::BAD_GATEWAY,
                "settle_failed",
                "Settlement failed.",
            );
        }
        Err(err) => {
            warn!("connect charge settle errored for skill {skill_id}: {err}");
            return json_error(
                StatusCode::BAD_GATEWAY,
                "settle_failed",
                "Settlement failed.",
            );
        }
    };

    // 4. Record the settled payment with its split (purpose is PII — captured, never logged).
    let purpose = sanitize_purpose(headers.get(PURPOSE_HEADER).and_then(|v| v.to_str().ok()));
    let payment_id = match state
        .db
        .record_settled_connect_payment(
            skill.marketplace_id,
            skill.merchant_id,
            skill_id,
            payer.as_deref(),
            skill.price_amount,
            split.seller_amount,
            split.marketplace_fee,
            split.apiosk_fee,
            &skill.destination_wallet,
            purpose.as_deref(),
            nonce.as_deref(),
            tx.as_deref(),
        )
        .await
    {
        Ok(id) => id,
        Err(err) if is_unique_violation(&err) => {
            return json_error(
                StatusCode::CONFLICT,
                "already_settled",
                "This payment has already been settled.",
            );
        }
        Err(err) => {
            // Settled on-chain but not recorded — needs reconciliation. Log loudly.
            error!(
                "connect charge SETTLED (tx={:?}) but record failed for skill {skill_id}: {err}",
                tx
            );
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "record_failed",
                "Payment settled but could not be recorded; contact support with the tx hash.",
            );
        }
    };

    // 5. Enqueue payout legs (the worker sends USDC from the platform wallet).
    // Merchant remainder always > 0 (price > 0, fees < 100%).
    if let Err(err) = state
        .db
        .enqueue_connect_payout(
            payment_id,
            "merchant",
            &skill.destination_wallet,
            split.seller_amount,
        )
        .await
    {
        warn!("connect payout enqueue (merchant) failed for {payment_id}: {err}");
    }
    if split.marketplace_fee > 0 {
        match state
            .db
            .get_connect_marketplace_payout_wallet(skill.marketplace_id)
            .await
        {
            Ok(Some(wallet)) => {
                if let Err(err) = state
                    .db
                    .enqueue_connect_payout(
                        payment_id,
                        "marketplace",
                        &wallet,
                        split.marketplace_fee,
                    )
                    .await
                {
                    warn!("connect payout enqueue (marketplace) failed for {payment_id}: {err}");
                }
            }
            // No payout wallet on file → the marketplace fee stays in the platform
            // wallet (recorded on the payment) until the owner links one.
            Ok(None) => warn!(
                "connect: marketplace {} has no verified payout wallet; {} minor units held",
                skill.marketplace_id, split.marketplace_fee
            ),
            Err(err) => warn!("connect marketplace wallet lookup failed for {payment_id}: {err}"),
        }
    }

    // 6. 200 + PAYMENT-RESPONSE.
    settled_response(payment_id, tx.as_deref(), &split)
}

/// The x402 PaymentRequirement the connect charge verifies + settles against —
/// pay `gross` USDC to the platform wallet. Mirrors the fields advertised in the
/// 402 body so verify/settle check exactly what the agent was shown.
fn build_connect_requirement(
    skill_id: Uuid,
    gross: i64,
    network: &str,
    asset: &str,
    pay_to: &str,
    resource: &str,
) -> PaymentRequirement {
    PaymentRequirement {
        scheme: "exact".to_string(),
        network: network.to_string(),
        max_amount_required: gross.to_string(),
        resource: resource.to_string(),
        description: format!("Apiosk Connect skill {skill_id}"),
        mime_type: "application/json".to_string(),
        pay_to: pay_to.to_string(),
        max_timeout_seconds: 300,
        asset: asset.to_string(),
        extra: evm_requirement_extra("USD Coin"),
        extensions: None,
        output_schema: None,
    }
}

/// Best-effort extraction of the EIP-3009 `nonce` + payer `from` from the x402
/// payment proof, for replay-recording. Missing fields → `None` (the on-chain
/// nonce enforcement + partial-unique index are the hard guards). Pure-ish.
fn decode_nonce_and_payer(proof: &str) -> (Option<String>, Option<String>) {
    let Ok(payload) = decode_payment_payload(proof) else {
        return (None, None);
    };
    let auth = payload.get("payload").and_then(|p| p.get("authorization"));
    let field = |key: &str| {
        auth.and_then(|a| a.get(key))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    (field("nonce"), field("from"))
}

/// `200 OK` with the x402 `PAYMENT-RESPONSE` header (base64 settlement proof) and a
/// small JSON body echoing the recorded split.
fn settled_response(payment_id: Uuid, tx: Option<&str>, split: &Split) -> Response {
    let receipt = json!({ "success": true, "transaction": tx, "payment_id": payment_id });
    let mut resp = json_response(
        StatusCode::OK,
        json!({
            "settled": true,
            "payment_id": payment_id,
            "transaction": tx,
            "split": {
                "seller_amount": split.seller_amount,
                "marketplace_fee": split.marketplace_fee,
                "apiosk_fee": split.apiosk_fee
            }
        }),
    );
    if let Ok(value) = HeaderValue::from_str(&BASE64.encode(receipt.to_string())) {
        resp.headers_mut().insert(PAYMENT_RESPONSE_HEADER, value);
    }
    resp
}

/// Inputs for the pure 402-body builder. Borrowed strings so the handler can pass
/// config/db slices without cloning.
struct Charge402<'a> {
    skill_id: Uuid,
    marketplace_id: Uuid,
    gross: i64,
    network: &'a str,
    asset: &'a str,
    pay_to: &'a str,
    resource: &'a str,
    seller_wallet: &'a str,
    split: Split,
}

/// Build the x402 `402` body: a spec-shaped `accepts` requirement (pay `gross`
/// USDC to the splitter) plus an `apiosk` block with the informational split.
/// Pure + unit-tested; the split comes solely from [`compute_split`].
fn build_charge_402_body(p: &Charge402) -> Value {
    json!({
        "x402Version": 1,
        "accepts": [{
            "scheme": "exact",
            "network": p.network,
            "maxAmountRequired": p.gross.to_string(),
            "resource": p.resource,
            "description": format!("Apiosk Connect skill {}", p.skill_id),
            "mimeType": "application/json",
            "payTo": p.pay_to,
            "maxTimeoutSeconds": 300,
            "asset": p.asset,
            // EIP-712 domain for USDC (name() = "USD Coin", version "2").
            "extra": { "name": "USD Coin", "version": "2" }
        }],
        "apiosk": {
            "skill_id": p.skill_id,
            "marketplace_id": p.marketplace_id,
            "split": {
                "seller": p.seller_wallet,
                "seller_amount": p.split.seller_amount,
                "marketplace_fee": p.split.marketplace_fee,
                "apiosk_fee": p.split.apiosk_fee
            }
        }
    })
}

/// Frame the body as a `402 Payment Required` with the gateway's x402 headers
/// (see gateway/CLAUDE.md). Never panics — falls back to a static header value.
fn payment_required(body: Value, amount: i64) -> Response {
    let mut resp = json_response(StatusCode::PAYMENT_REQUIRED, body);
    let h = resp.headers_mut();
    h.insert(
        "X-Payment-Amount",
        HeaderValue::from_str(&amount.to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("0")),
    );
    h.insert("X-Payment-Currency", HeaderValue::from_static("USDC"));
    h.insert("X-Payment-Protocol", HeaderValue::from_static("x402"));
    h.insert(
        "X-Request-Id",
        HeaderValue::from_str(&Uuid::new_v4().to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("connect-charge")),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        let split = compute_split(100_000000, 3000, 200);
        build_charge_402_body(&Charge402 {
            skill_id: Uuid::from_u128(0x5111),
            marketplace_id: Uuid::from_u128(0xA11CE),
            gross: 100_000000,
            network: "base",
            asset: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
            pay_to: "0xPLATFORM",
            resource: "https://gateway.apiosk.com/v1/skills/x/charge",
            seller_wallet: "0xSELLER",
            split,
        })
    }

    #[test]
    fn body_advertises_gross_payto_and_asset() {
        let b = sample();
        let acc = &b["accepts"][0];
        assert_eq!(acc["scheme"], "exact");
        assert_eq!(acc["network"], "base");
        assert_eq!(acc["maxAmountRequired"], "100000000"); // string, minor units
        assert_eq!(acc["payTo"], "0xPLATFORM");
        assert_eq!(acc["asset"], "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
        assert_eq!(b["x402Version"], 1);
    }

    #[test]
    fn settle_requirement_matches_the_advertised_402() {
        // verify/settle must check exactly what the 402 advertised: pay `gross`
        // USDC to the platform wallet.
        let r = build_connect_requirement(
            Uuid::from_u128(1),
            100_000000,
            "base",
            "0xUSDC",
            "0xPLATFORM",
            "https://gateway.apiosk.com/v1/skills/x/charge",
        );
        assert_eq!(r.scheme, "exact");
        assert_eq!(r.network, "base");
        assert_eq!(r.max_amount_required, "100000000");
        assert_eq!(r.pay_to, "0xPLATFORM");
        assert_eq!(r.asset, "0xUSDC");
        assert_eq!(r.extra["name"], "USD Coin");
        assert_eq!(r.extra["version"], "2");
    }

    #[test]
    fn decode_nonce_and_payer_reads_the_authorization() {
        let proof = json!({
            "x402Version": 1,
            "payload": { "authorization": { "from": "0xPAYER", "nonce": "0xNONCE", "value": "100" } }
        })
        .to_string();
        let (nonce, payer) = decode_nonce_and_payer(&proof);
        assert_eq!(nonce.as_deref(), Some("0xNONCE"));
        assert_eq!(payer.as_deref(), Some("0xPAYER"));
    }

    #[test]
    fn decode_nonce_and_payer_tolerates_missing_or_garbage() {
        assert_eq!(decode_nonce_and_payer("not json"), (None, None));
        assert_eq!(
            decode_nonce_and_payer(&json!({"payload": {}}).to_string()),
            (None, None)
        );
    }

    #[test]
    fn apiosk_split_matches_compute_split_and_sums_to_gross() {
        let b = sample();
        let s = &b["apiosk"]["split"];
        assert_eq!(s["seller"], "0xSELLER");
        let seller = s["seller_amount"].as_i64().unwrap();
        let mkt = s["marketplace_fee"].as_i64().unwrap();
        let apiosk = s["apiosk_fee"].as_i64().unwrap();
        assert_eq!(seller, 68_000000);
        assert_eq!(mkt, 30_000000);
        assert_eq!(apiosk, 2_000000);
        assert_eq!(seller + mkt + apiosk, 100_000000);
    }
}
