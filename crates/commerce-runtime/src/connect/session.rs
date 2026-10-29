//! Merchant-session auth for the onboarding wizard. The seller is not a Supabase
//! user, so wizard endpoints (profile/nonce/siwe-verify/activate) authenticate
//! with the stateless HMAC session token issued by `onboarding/verify` (02-02),
//! presented in the `x-apiosk-merchant-session` header.

use std::sync::Arc;

use axum::{
    http::{HeaderMap, StatusCode},
    response::Response,
};

use super::json_error;
use super::tokens::{decode_token, now_unix, MerchantSessionClaims, TokenError};
use crate::AppState;

pub const MERCHANT_SESSION_HEADER: &str = "x-apiosk-merchant-session";

/// Resolve + verify the merchant session, or return a ready-to-send error
/// Response (503 if Connect is unconfigured, 401 for missing/invalid/expired).
#[allow(clippy::result_large_err)]
pub fn require_merchant_session(
    state: &Arc<AppState>,
    headers: &HeaderMap,
) -> Result<MerchantSessionClaims, Response> {
    let secret = state.config.connect_hmac_secret.as_deref().ok_or_else(|| {
        json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "connect_not_configured",
            "Apiosk Connect is not configured on the gateway.",
        )
    })?;

    let token = headers
        .get(MERCHANT_SESSION_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            json_error(
                StatusCode::UNAUTHORIZED,
                "missing_session",
                "Missing merchant session token.",
            )
        })?;

    decode_token::<MerchantSessionClaims>(secret, token, now_unix()).map_err(|err| match err {
        TokenError::Expired => json_error(
            StatusCode::UNAUTHORIZED,
            "session_expired",
            "Merchant session expired — restart onboarding from your link.",
        ),
        _ => json_error(
            StatusCode::UNAUTHORIZED,
            "invalid_session",
            "Invalid merchant session token.",
        ),
    })
}
