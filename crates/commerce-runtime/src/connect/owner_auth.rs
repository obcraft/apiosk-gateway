//! Resolve the Apiosk portal user (Supabase auth) behind a Connect owner request.
//!
//! The marketplace owner is a Supabase `auth.users` row, not a wallet, so owner
//! endpoints authenticate with the portal's session JWT (sent in the
//! `x-apiosk-user-jwt` header) rather than the wallet-signature scheme used by
//! `payout.rs`/`api_management`. Validation itself lives in
//! [`crate::portal_auth`], shared with the credits path and the admin discovery
//! probe, so a forged/expired token can never resolve to a user on any of them.
//! All this module adds is the Connect-specific error vocabulary.

use std::sync::Arc;

use axum::{
    http::{HeaderMap, StatusCode},
    response::Response,
};
use uuid::Uuid;

use super::json_error;
use crate::auth::extract_credits_user_jwt;
use crate::portal_auth::{resolve_portal_user_id, PortalAuthError};
use crate::AppState;

/// Resolve the authenticated owner's `user_id`, or a ready-to-send error
/// `Response` (401 missing/invalid token, 5xx when auth is unconfigured/down).
pub async fn resolve_owner_user_id(
    state: &Arc<AppState>,
    headers: &HeaderMap,
) -> Result<Uuid, Response> {
    let jwt = extract_credits_user_jwt(headers).ok_or_else(|| {
        json_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Missing portal session token.",
        )
    })?;

    resolve_portal_user_id(state, &jwt, "connect owner")
        .await
        .map_err(|err| match err {
            PortalAuthError::NotConfigured => json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "owner_auth_not_configured",
                "Owner authentication is not configured on the gateway.",
            ),
            PortalAuthError::Unavailable => json_error(
                StatusCode::BAD_GATEWAY,
                "owner_auth_unavailable",
                "Could not validate the portal session.",
            ),
            PortalAuthError::Invalid => json_error(
                StatusCode::UNAUTHORIZED,
                "owner_auth_invalid",
                "Invalid or expired portal session token.",
            ),
        })
}
