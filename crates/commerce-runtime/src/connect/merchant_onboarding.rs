//! Merchant onboarding endpoints (package 02).
//!
//!  * 02-01 `POST /v1/merchants/onboarding-link` — a marketplace (API-key auth)
//!    mints a signed, expiring link for one seller.
//!  * 02-02 `POST /v1/merchants/onboarding/verify` — the portal exchanges the
//!    link token for a draft merchant + a merchant-session token.
//!  * 02-03 `POST /v1/merchants/profile` — wizard step 1 writes name/email.
//!  * 02-07 `POST /v1/merchants/activate` — finalize (requires a verified wallet),
//!    audit, and fire a signed `merchant.activated` webhook.

use std::sync::Arc;

use axum::{
    extract::{Json, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    Extension,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::time::{sleep, Duration};
use tracing::warn;
use uuid::Uuid;

use super::api_key_auth::MarketplaceContext;
use super::audit::audit_log;
use super::session::require_merchant_session;
use super::tokens::{
    decode_token, encode_token, hmac_sha256_hex, now_unix, MerchantSessionClaims, OnboardingClaims,
    TokenError,
};
use super::{json_error, json_response};
use crate::AppState;

const ONBOARDING_LINK_TTL_SECS: i64 = 30 * 60;
const MERCHANT_SESSION_TTL_SECS: i64 = 60 * 60;

#[allow(clippy::result_large_err)]
fn connect_secret_or_503(state: &Arc<AppState>) -> Result<&str, Response> {
    state.config.connect_hmac_secret.as_deref().ok_or_else(|| {
        json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "connect_not_configured",
            "Apiosk Connect is not configured on the gateway.",
        )
    })
}

/* -------------------------------------------------------------------------- */
/* 02-01 onboarding-link                                                      */
/* -------------------------------------------------------------------------- */

#[derive(Deserialize)]
pub struct OnboardingLinkRequest {
    external_seller_id: String,
}

pub async fn generate_onboarding_link(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<MarketplaceContext>,
    body: Json<OnboardingLinkRequest>,
) -> Response {
    let secret = match connect_secret_or_503(&state) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    let external_seller_id = body.0.external_seller_id.trim();
    if external_seller_id.is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "missing_external_seller_id",
            "external_seller_id is required.",
        );
    }

    let exp = now_unix() + ONBOARDING_LINK_TTL_SECS;
    let token = encode_token(
        secret,
        &OnboardingClaims {
            marketplace_id: ctx.marketplace_id,
            external_seller_id: external_seller_id.to_string(),
            exp,
        },
    );
    let url = format!(
        "{}/connect/onboard?token={}",
        state.config.connect_portal_url, token
    );
    json_response(
        StatusCode::CREATED,
        json!({ "url": url, "token": token, "expires_at": exp }),
    )
}

/* -------------------------------------------------------------------------- */
/* 02-02 onboarding verify -> merchant session                                */
/* -------------------------------------------------------------------------- */

#[derive(Deserialize)]
pub struct VerifyOnboardingRequest {
    token: String,
}

pub async fn verify_onboarding(
    State(state): State<Arc<AppState>>,
    body: Json<VerifyOnboardingRequest>,
) -> Response {
    let secret = match connect_secret_or_503(&state) {
        Ok(s) => s,
        Err(resp) => return resp,
    };

    let claims = match decode_token::<OnboardingClaims>(secret, &body.0.token, now_unix()) {
        Ok(c) => c,
        Err(TokenError::Expired) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "link_expired",
                "This onboarding link has expired. Ask the marketplace for a new one.",
            )
        }
        Err(_) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "invalid_link",
                "This onboarding link is invalid.",
            )
        }
    };

    // Idempotent: re-verifying the same link resolves to the same merchant.
    let merchant_id = match state
        .db
        .upsert_connect_merchant(claims.marketplace_id, &claims.external_seller_id)
        .await
    {
        Ok(id) => id,
        Err(err) => {
            warn!("connect onboarding upsert failed: {err}");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "merchant_upsert_failed",
                "Could not start onboarding.",
            );
        }
    };

    let session = encode_token(
        secret,
        &MerchantSessionClaims {
            merchant_id,
            marketplace_id: claims.marketplace_id,
            exp: now_unix() + MERCHANT_SESSION_TTL_SECS,
        },
    );
    json_response(
        StatusCode::OK,
        json!({
            "merchant_id": merchant_id,
            "marketplace_id": claims.marketplace_id,
            "session": session,
            "status": "draft",
        }),
    )
}

/* -------------------------------------------------------------------------- */
/* 02-03 profile                                                              */
/* -------------------------------------------------------------------------- */

#[derive(Deserialize)]
pub struct MerchantProfileRequest {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    contact_email: Option<String>,
}

pub async fn update_merchant_profile(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Json<MerchantProfileRequest>,
) -> Response {
    let session = match require_merchant_session(&state, &headers) {
        Ok(s) => s,
        Err(resp) => return resp,
    };

    let display_name = body
        .0
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let contact_email = body
        .0
        .contact_email
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    if let Some(email) = contact_email {
        if !is_plausible_email(email) {
            return json_error(
                StatusCode::BAD_REQUEST,
                "invalid_email",
                "Enter a valid contact email.",
            );
        }
    }

    if let Err(err) = state
        .db
        .update_connect_merchant_profile(session.merchant_id, display_name, contact_email)
        .await
    {
        warn!("connect profile update failed: {err}");
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "profile_save_failed",
            "Could not save the profile.",
        );
    }
    json_response(StatusCode::OK, json!({ "saved": true }))
}

/* -------------------------------------------------------------------------- */
/* 02-07 activate + webhook                                                   */
/* -------------------------------------------------------------------------- */

pub async fn activate_merchant(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let session = match require_merchant_session(&state, &headers) {
        Ok(s) => s,
        Err(resp) => return resp,
    };

    let has_wallet = state
        .db
        .merchant_has_verified_active_wallet(session.merchant_id)
        .await
        .unwrap_or(false);
    if let Err((code, msg)) = activation_gate(has_wallet) {
        return json_error(StatusCode::CONFLICT, code, msg);
    }

    if let Err(err) = state
        .db
        .activate_connect_merchant(session.merchant_id)
        .await
    {
        warn!("connect activate failed: {err}");
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "activate_failed",
            "Could not activate the merchant.",
        );
    }

    audit_log(
        &state.db,
        &format!("merchant:{}", session.merchant_id),
        "merchant.activated",
        "merchant",
        Some(session.merchant_id),
        None,
        Some(json!({ "marketplace_id": session.marketplace_id })),
    )
    .await;

    // Fire-and-forget signed webhook (never blocks the response).
    spawn_activation_webhook(state.clone(), session.marketplace_id, session.merchant_id);

    json_response(StatusCode::OK, json!({ "status": "active" }))
}

/// Pure activation gate: a merchant may only go active with a verified wallet.
pub fn activation_gate(has_verified_wallet: bool) -> Result<(), (&'static str, &'static str)> {
    if has_verified_wallet {
        Ok(())
    } else {
        Err((
            "wallet_required",
            "Connect and verify a payout wallet before activating.",
        ))
    }
}

/// The webhook body the gateway POSTs to a marketplace on activation.
pub fn build_activation_webhook_payload(marketplace_id: Uuid, merchant_id: Uuid, ts: i64) -> Value {
    json!({
        "event": "merchant.activated",
        "marketplace_id": marketplace_id,
        "merchant_id": merchant_id,
        "ts": ts,
    })
}

fn spawn_activation_webhook(state: Arc<AppState>, marketplace_id: Uuid, merchant_id: Uuid) {
    tokio::spawn(async move {
        let Some(secret) = state.config.connect_hmac_secret.clone() else {
            return;
        };
        let url = match state
            .db
            .get_connect_marketplace_webhook_url(marketplace_id)
            .await
        {
            Ok(Some(u)) if !u.trim().is_empty() => u,
            _ => return, // no webhook configured for this marketplace
        };

        let payload =
            build_activation_webhook_payload(marketplace_id, merchant_id, now_unix()).to_string();
        let signature = hmac_sha256_hex(&secret, payload.as_bytes());

        // 3 attempts with linear backoff. The signature is stable across retries.
        for attempt in 1..=3u32 {
            let result = state
                .http_client
                .post(url.trim())
                .header("content-type", "application/json")
                .header("x-apiosk-event", "merchant.activated")
                .header("x-apiosk-signature", &signature)
                .body(payload.clone())
                .send()
                .await;
            match result {
                Ok(resp) if resp.status().is_success() => return,
                Ok(resp) => warn!(
                    "connect webhook attempt {attempt} for merchant {merchant_id} -> HTTP {}",
                    resp.status()
                ),
                Err(err) => warn!(
                    "connect webhook attempt {attempt} for merchant {merchant_id} failed: {err}"
                ),
            }
            if attempt < 3 {
                sleep(Duration::from_secs(attempt as u64)).await;
            }
        }
        warn!("connect webhook delivery gave up for merchant {merchant_id}");
    });
}

/// Lightweight email sanity check (not full RFC). The real validation is the
/// downstream system's; this just blocks obvious garbage server-side.
fn is_plausible_email(value: &str) -> bool {
    let v = value.trim();
    let mut parts = v.splitn(2, '@');
    let (local, domain) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    !v.contains(char::is_whitespace)
        && !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_requires_verified_wallet() {
        assert!(activation_gate(true).is_ok());
        assert_eq!(activation_gate(false).unwrap_err().0, "wallet_required");
    }

    #[test]
    fn webhook_payload_is_signed_and_verifiable() {
        let mp = Uuid::from_u128(1);
        let m = Uuid::from_u128(2);
        let payload = build_activation_webhook_payload(mp, m, 1234).to_string();
        assert!(payload.contains("merchant.activated"));
        // Signature is reproducible + verifies under the same secret.
        let sig = hmac_sha256_hex("whsecret", payload.as_bytes());
        assert!(super::super::tokens::hmac_verify(
            "whsecret",
            payload.as_bytes(),
            &sig
        ));
        assert!(!super::super::tokens::hmac_verify(
            "other",
            payload.as_bytes(),
            &sig
        ));
    }

    #[test]
    fn email_sanity_check() {
        assert!(is_plausible_email("seller@example.com"));
        assert!(!is_plausible_email("nope"));
        assert!(!is_plausible_email("a@b"));
        assert!(!is_plausible_email("a b@c.com"));
        assert!(!is_plausible_email("@example.com"));
    }
}
