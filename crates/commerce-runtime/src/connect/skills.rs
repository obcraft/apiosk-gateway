//! Skill onboarding endpoints (package 03) — a marketplace registers a billable
//! skill against one of its merchants (API-key auth via the 00-08 middleware).
//!
//!  * 03-01 `POST /v1/skills` — create a skill with the merchant's active payout
//!    wallet as `destination_wallet` and a SNAPSHOT of the marketplace fee config.
//!  * 03-02 `PUT  /v1/skills/{id}` — patch price / active / (deliberately) the
//!    wallet snapshot, scoped to the owning marketplace.
//!  * 03-03 `assert_merchant_ready` — the single chargeability guard, reused here
//!    and by the charge builder (04-02).
//!
//! The fee bps + wallet are copied onto the skill at creation, never joined back
//! at payment time (039): a later marketplace fee change or wallet swap must not
//! retroactively re-price an already-onboarded skill.

use std::sync::Arc;

use axum::{
    extract::{Json, Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    Extension,
};
use serde::Deserialize;
use serde_json::json;
use tracing::warn;
use uuid::Uuid;

use super::api_key_auth::MarketplaceContext;
use super::audit::audit_log;
use super::idempotency::{parse_idempotency_key, KeyOutcome};
use super::{json_error, json_response};
use crate::AppState;

/// Canonical public origin of the gateway (mirrors `gateway_routes::GATEWAY_BASE_URL`
/// and the hardcoded base already used in `payment.rs`). The charge endpoint itself
/// lands in package 04; the URL returned here is the forward-looking pointer a
/// marketplace uses to trigger the x402 paywall for a skill.
pub(super) const GATEWAY_PUBLIC_BASE: &str = "https://gateway.apiosk.com";

/* -------------------------------------------------------------------------- */
/* 03-03 chargeability guard                                                  */
/* -------------------------------------------------------------------------- */

/// Why a merchant is not yet ready to have skills charged. Typed so the rule has
/// one source of truth and a stable HTTP mapping ([`Self::as_error`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MerchantReadiness {
    #[error("merchant is not active")]
    NotActive,
    #[error("merchant has no active payout wallet")]
    NoActiveWallet,
}

impl MerchantReadiness {
    /// Stable `(error_code, message)` for the JSON body. Both map to 409 Conflict
    /// at the call site — the merchant's state, not the request, is the problem.
    pub fn as_error(self) -> (&'static str, &'static str) {
        match self {
            MerchantReadiness::NotActive => (
                "merchant_not_active",
                "This merchant hasn't finished onboarding yet.",
            ),
            MerchantReadiness::NoActiveWallet => (
                "wallet_required",
                "This merchant has no active, verified payout wallet.",
            ),
        }
    }
}

/// Central guard (03-03): a skill is only chargeable when its merchant is `active`
/// AND has an active, verified payout wallet. Pure — the DB lookups live in the
/// caller — so the three cases are unit-testable without a database. Reused by
/// 03-01, 03-02 and (later) the 04-02 charge builder.
pub fn assert_merchant_ready(
    status: &str,
    has_active_wallet: bool,
) -> Result<(), MerchantReadiness> {
    if status != "active" {
        return Err(MerchantReadiness::NotActive);
    }
    if !has_active_wallet {
        return Err(MerchantReadiness::NoActiveWallet);
    }
    Ok(())
}

/// USDC-only in v1; price is integer minor units and must be positive. The DB also
/// enforces `price_amount > 0`, but validating here returns a clean 400 instead of
/// a 500 surfaced from the CHECK constraint. Pure.
fn validate_price(amount: i64, currency: &str) -> Result<(), (&'static str, &'static str)> {
    if amount <= 0 {
        return Err((
            "invalid_price",
            "price_amount must be a positive integer in minor units.",
        ));
    }
    if !currency.eq_ignore_ascii_case("USDC") {
        return Err(("unsupported_currency", "Only USDC is supported in v1."));
    }
    Ok(())
}

/// The charge URL for a skill (`GET …/v1/skills/{id}/charge`, the 04-02 paywall).
/// Shared with the charge module so the create response and the paywall's own
/// `resource` field never drift. Pure.
pub(super) fn build_charge_url(base: &str, skill_id: Uuid) -> String {
    format!(
        "{}/v1/skills/{}/charge",
        base.trim_end_matches('/'),
        skill_id
    )
}

/// Enforce the 00-07 write contract inline (the gateway applies idempotency inline,
/// not as a tower layer). `Ok(())` when a well-formed key is present.
#[allow(clippy::result_large_err)]
fn require_idempotency_key(headers: &HeaderMap) -> Result<(), Response> {
    match parse_idempotency_key(headers) {
        KeyOutcome::Valid(_) => Ok(()),
        KeyOutcome::Absent => Err(json_error(
            StatusCode::BAD_REQUEST,
            "idempotency_key_required",
            "Write requests require an Idempotency-Key header.",
        )),
        KeyOutcome::Malformed => Err(json_error(
            StatusCode::BAD_REQUEST,
            "invalid_idempotency_key",
            "Idempotency-Key is present but empty, oversized, or non-ASCII.",
        )),
    }
}

/// True when an error is a Postgres unique-violation (SQLSTATE 23505), unwrapped
/// from the `anyhow` error the DB layer returns.
pub(super) fn is_unique_violation(err: &anyhow::Error) -> bool {
    err.downcast_ref::<sqlx::Error>()
        .and_then(|e| e.as_database_error())
        .and_then(|db| db.code())
        .map(|code| code == "23505")
        .unwrap_or(false)
}

/* -------------------------------------------------------------------------- */
/* 03-01 create skill                                                         */
/* -------------------------------------------------------------------------- */

#[derive(Deserialize)]
pub struct CreateSkillRequest {
    /// Either identifies the merchant directly …
    #[serde(default)]
    merchant_id: Option<Uuid>,
    /// … or by the marketplace-scoped external id (02-01). One is required.
    #[serde(default)]
    external_seller_id: Option<String>,
    external_skill_ref: String,
    name: String,
    /// Integer minor units (USDC = 6 decimals). Must be > 0.
    price_amount: i64,
    #[serde(default)]
    price_currency: Option<String>,
}

pub async fn create_skill(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<MarketplaceContext>,
    headers: HeaderMap,
    body: Json<CreateSkillRequest>,
) -> Response {
    if let Err(resp) = require_idempotency_key(&headers) {
        return resp;
    }
    let req = body.0;

    let external_skill_ref = req.external_skill_ref.trim();
    let name = req.name.trim();
    if external_skill_ref.is_empty() || name.is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "missing_fields",
            "external_skill_ref and name are required.",
        );
    }
    let currency = req
        .price_currency
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("USDC");
    if let Err((code, msg)) = validate_price(req.price_amount, currency) {
        return json_error(StatusCode::BAD_REQUEST, code, msg);
    }

    // Resolve the merchant, scoped to the authenticated marketplace.
    let (merchant_id, status) = match resolve_merchant(
        &state,
        &ctx,
        req.merchant_id,
        req.external_seller_id.as_deref(),
    )
    .await
    {
        Ok(Some(m)) => m,
        Ok(None) => {
            return json_error(
                StatusCode::NOT_FOUND,
                "merchant_not_found",
                "No such merchant in this marketplace.",
            )
        }
        Err(resp) => return resp,
    };

    let wallet = match state
        .db
        .get_connect_active_wallet_address(merchant_id)
        .await
    {
        Ok(w) => w,
        Err(err) => {
            warn!("connect active-wallet lookup failed: {err}");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "lookup_failed",
                "Could not load the merchant wallet.",
            );
        }
    };

    // 03-03 guard: merchant active + active wallet.
    if let Err(readiness) = assert_merchant_ready(&status, wallet.is_some()) {
        let (code, msg) = readiness.as_error();
        return json_error(StatusCode::CONFLICT, code, msg);
    }
    // Guaranteed Some after the guard; handled without unwrap to avoid a panic path.
    let Some(destination_wallet) = wallet else {
        let (code, msg) = MerchantReadiness::NoActiveWallet.as_error();
        return json_error(StatusCode::CONFLICT, code, msg);
    };

    // Fee SNAPSHOT — copied onto the skill, never re-read at payment time.
    let (marketplace_fee_bps, apiosk_fee_bps) = match state
        .db
        .get_connect_marketplace_fees(ctx.marketplace_id)
        .await
    {
        Ok(Some(fees)) => fees,
        Ok(None) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "marketplace_missing",
                "Marketplace not found.",
            )
        }
        Err(err) => {
            warn!("connect marketplace-fees lookup failed: {err}");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "lookup_failed",
                "Could not load the marketplace fees.",
            );
        }
    };

    let (skill_id, created_at) = match state
        .db
        .insert_connect_skill(
            ctx.marketplace_id,
            merchant_id,
            external_skill_ref,
            name,
            req.price_amount,
            currency,
            &destination_wallet,
            marketplace_fee_bps,
            apiosk_fee_bps,
        )
        .await
    {
        Ok(row) => row,
        Err(err) if is_unique_violation(&err) => {
            return json_error(
                StatusCode::CONFLICT,
                "skill_exists",
                "A skill with this external_skill_ref already exists for this marketplace.",
            )
        }
        Err(err) => {
            warn!("connect skill insert failed: {err}");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "skill_persist_failed",
                "Could not create the skill.",
            );
        }
    };

    audit_log(
        &state.db,
        &format!("marketplace:{}", ctx.marketplace_id),
        "skill.created",
        "skill",
        Some(skill_id),
        Some(req.price_amount),
        Some(json!({ "merchant_id": merchant_id, "external_skill_ref": external_skill_ref })),
    )
    .await;

    json_response(
        StatusCode::CREATED,
        json!({
            "id": skill_id,
            "charge_url": build_charge_url(GATEWAY_PUBLIC_BASE, skill_id),
            "destination_wallet": destination_wallet,
            "price_amount": req.price_amount,
            "price_currency": currency,
            "marketplace_fee_bps": marketplace_fee_bps,
            "apiosk_fee_bps": apiosk_fee_bps,
            "active": true,
            "created_at": created_at.to_rfc3339(),
        }),
    )
}

/// Resolve the target merchant, scoped to the authenticated marketplace.
/// `Ok(Some((id, status)))` on a hit, `Ok(None)` when nothing matches (unknown, or
/// belongs to a different marketplace — reported the same to avoid leaking cross-
/// marketplace existence). `Err(Response)` is a 400 (no selector) or 500 (lookup).
async fn resolve_merchant(
    state: &Arc<AppState>,
    ctx: &MarketplaceContext,
    merchant_id: Option<Uuid>,
    external_seller_id: Option<&str>,
) -> Result<Option<(Uuid, String)>, Response> {
    if let Some(mid) = merchant_id {
        return match state.db.get_connect_merchant(mid).await {
            Ok(Some((mkt, status))) if mkt == ctx.marketplace_id => Ok(Some((mid, status))),
            Ok(_) => Ok(None),
            Err(err) => {
                warn!("connect merchant lookup failed: {err}");
                Err(json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "lookup_failed",
                    "Could not load the merchant.",
                ))
            }
        };
    }

    let Some(ext) = external_seller_id.map(str::trim).filter(|s| !s.is_empty()) else {
        return Err(json_error(
            StatusCode::BAD_REQUEST,
            "missing_merchant",
            "Provide merchant_id or external_seller_id.",
        ));
    };

    match state
        .db
        .resolve_connect_merchant_by_external(ctx.marketplace_id, ext)
        .await
    {
        Ok(found) => Ok(found),
        Err(err) => {
            warn!("connect merchant resolve failed: {err}");
            Err(json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "lookup_failed",
                "Could not load the merchant.",
            ))
        }
    }
}

/* -------------------------------------------------------------------------- */
/* 03-02 update skill                                                         */
/* -------------------------------------------------------------------------- */

#[derive(Deserialize)]
pub struct UpdateSkillRequest {
    #[serde(default)]
    price_amount: Option<i64>,
    #[serde(default)]
    active: Option<bool>,
    /// Deliberate re-snapshot of `destination_wallet` to the merchant's CURRENT
    /// active wallet. Off by default; the fee snapshot is never touched here.
    #[serde(default)]
    resnapshot_wallet: Option<bool>,
}

pub async fn update_skill(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<MarketplaceContext>,
    Path(skill_id): Path<Uuid>,
    headers: HeaderMap,
    body: Json<UpdateSkillRequest>,
) -> Response {
    if let Err(resp) = require_idempotency_key(&headers) {
        return resp;
    }
    let req = body.0;

    // Load + authorize: the skill must belong to the authenticated marketplace.
    let (marketplace_id, merchant_id) = match state.db.get_connect_skill_owner(skill_id).await {
        Ok(Some(row)) => row,
        Ok(None) => return skill_not_found(),
        Err(err) => {
            warn!("connect skill owner lookup failed: {err}");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "lookup_failed",
                "Could not load the skill.",
            );
        }
    };
    if marketplace_id != ctx.marketplace_id {
        // Never reveal that the skill exists under a different marketplace.
        return skill_not_found();
    }

    if let Some(price) = req.price_amount {
        if let Err((code, msg)) = validate_price(price, "USDC") {
            return json_error(StatusCode::BAD_REQUEST, code, msg);
        }
    }

    // Optional deliberate wallet re-snapshot → the merchant's current active wallet.
    let mut new_wallet: Option<String> = None;
    if req.resnapshot_wallet.unwrap_or(false) {
        match state
            .db
            .get_connect_active_wallet_address(merchant_id)
            .await
        {
            Ok(Some(addr)) => new_wallet = Some(addr),
            Ok(None) => {
                let (code, msg) = MerchantReadiness::NoActiveWallet.as_error();
                return json_error(StatusCode::CONFLICT, code, msg);
            }
            Err(err) => {
                warn!("connect active-wallet lookup failed: {err}");
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "lookup_failed",
                    "Could not load the merchant wallet.",
                );
            }
        }
    }

    if let Err(err) = state
        .db
        .update_connect_skill(
            skill_id,
            req.price_amount,
            req.active,
            new_wallet.as_deref(),
        )
        .await
    {
        warn!("connect skill update failed: {err}");
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "skill_update_failed",
            "Could not update the skill.",
        );
    }

    audit_log(
        &state.db,
        &format!("marketplace:{}", ctx.marketplace_id),
        "skill.updated",
        "skill",
        Some(skill_id),
        req.price_amount,
        Some(json!({
            "price_changed": req.price_amount.is_some(),
            "active": req.active,
            "wallet_resnapshot": new_wallet.is_some(),
        })),
    )
    .await;

    json_response(StatusCode::OK, json!({ "id": skill_id, "updated": true }))
}

fn skill_not_found() -> Response {
    json_error(StatusCode::NOT_FOUND, "skill_not_found", "Skill not found.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merchant_readiness_matrix() {
        // Not active in any form → NotActive (checked before the wallet).
        assert_eq!(
            assert_merchant_ready("draft", true).unwrap_err(),
            MerchantReadiness::NotActive
        );
        assert_eq!(
            assert_merchant_ready("suspended", false).unwrap_err(),
            MerchantReadiness::NotActive
        );
        // Active but no wallet → NoActiveWallet.
        assert_eq!(
            assert_merchant_ready("active", false).unwrap_err(),
            MerchantReadiness::NoActiveWallet
        );
        // Active + active wallet → ok.
        assert!(assert_merchant_ready("active", true).is_ok());
    }

    #[test]
    fn readiness_error_codes_are_stable() {
        assert_eq!(
            MerchantReadiness::NotActive.as_error().0,
            "merchant_not_active"
        );
        assert_eq!(
            MerchantReadiness::NoActiveWallet.as_error().0,
            "wallet_required"
        );
    }

    #[test]
    fn price_validation_rules() {
        assert!(validate_price(1, "USDC").is_ok());
        assert!(validate_price(1, "usdc").is_ok()); // case-insensitive
        assert_eq!(validate_price(0, "USDC").unwrap_err().0, "invalid_price");
        assert_eq!(validate_price(-5, "USDC").unwrap_err().0, "invalid_price");
        assert_eq!(
            validate_price(100, "EUR").unwrap_err().0,
            "unsupported_currency"
        );
    }

    #[test]
    fn charge_url_points_at_the_skill_charge_endpoint() {
        let id = Uuid::from_u128(0xABC);
        assert_eq!(
            build_charge_url("https://gateway.apiosk.com", id),
            format!("https://gateway.apiosk.com/v1/skills/{id}/charge")
        );
        // Trailing slash on the base is normalized.
        assert_eq!(
            build_charge_url("https://gateway.apiosk.com/", id),
            format!("https://gateway.apiosk.com/v1/skills/{id}/charge")
        );
    }
}
