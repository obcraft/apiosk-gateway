//! Turning a portal session token into a Supabase user id — in one place.
//!
//! Three surfaces authenticate a human from the portal rather than a wallet or
//! an API key: the credits payment path, the Connect owner endpoints, and the
//! admin-only discovery probe. All three do the same thing — hand the
//! `x-apiosk-user-jwt` value to Supabase `GET /auth/v1/user` and trust nothing
//! the token itself claims — and all three used to do it with their own copy of
//! the call. Three copies of "how do we know who this is" is two too many, so
//! the call lives here and each caller maps [`PortalAuthError`] onto the error
//! codes its own clients already expect.
//!
//! The token is never logged.

use std::sync::Arc;

use axum::http::header;
use serde::Deserialize;
use tracing::error;
use uuid::Uuid;

use crate::AppState;

/// Why a portal session could not be resolved to a user.
///
/// Split three ways because the three mean different things to the caller:
/// [`NotConfigured`](PortalAuthError::NotConfigured) is our mistake,
/// [`Unavailable`](PortalAuthError::Unavailable) is Supabase's, and only
/// [`Invalid`](PortalAuthError::Invalid) is the client's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalAuthError {
    /// The gateway has no Supabase auth configuration.
    NotConfigured,
    /// Supabase could not be reached at all.
    Unavailable,
    /// Missing, forged, expired, or resolving to nothing.
    Invalid,
}

#[derive(Deserialize)]
struct SupabaseAuthUser {
    id: String,
}

/// Resolve the Supabase `auth.users` id behind a portal session token.
///
/// Validated against Supabase on every call rather than by decoding the JWT
/// locally: a signature check would still accept a token the user has since
/// signed out of, and this is the only thing standing between a pasted string
/// and an authenticated identity.
pub async fn resolve_portal_user_id(
    state: &Arc<AppState>,
    jwt: &str,
    // Named only so the log line says which surface was being authenticated.
    surface: &str,
) -> Result<Uuid, PortalAuthError> {
    let supabase_url = state.config.supabase_url.as_deref().ok_or_else(|| {
        error!("SUPABASE_URL is required for {surface} auth");
        PortalAuthError::NotConfigured
    })?;
    let supabase_api_key = state.config.supabase_api_key.as_deref().ok_or_else(|| {
        error!("SUPABASE auth API key is required for {surface} auth");
        PortalAuthError::NotConfigured
    })?;

    let response = state
        .http_client
        .get(format!("{supabase_url}/auth/v1/user"))
        .header("apikey", supabase_api_key)
        .header(header::AUTHORIZATION, format!("Bearer {jwt}"))
        .header(header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|err| {
            error!("failed to validate {surface} token: {err}");
            PortalAuthError::Unavailable
        })?;

    if !response.status().is_success() {
        return Err(PortalAuthError::Invalid);
    }

    let user = response.json::<SupabaseAuthUser>().await.map_err(|err| {
        error!("failed to parse {surface} user: {err}");
        PortalAuthError::Invalid
    })?;

    Uuid::parse_str(user.id.trim()).map_err(|_| PortalAuthError::Invalid)
}
