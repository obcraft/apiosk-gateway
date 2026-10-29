//! 00-08 — Marketplace API-key auth middleware.
//!
//! Validates a marketplace's API key by hashing the presented key (SHA-256) and
//! matching it against `connect_api_keys.key_hash WHERE revoked_at IS NULL`, then
//! attaches the resolved `marketplace_id` to the request via an extension so
//! downstream connect `/v1/*` handlers can scope every read/write to that
//! marketplace.
//!
//! Mirrors the existing provider-key shape in `crate::auth` (dedicated header OR
//! `Authorization: Bearer …`) but for the Connect management surface. The raw key
//! and its hash are NEVER logged.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, HeaderValue, Request, StatusCode},
    middleware::Next,
    response::Response,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::AppState;

/// Header a marketplace may use to present its key instead of a Bearer token.
pub const MARKETPLACE_API_KEY_HEADER: &str = "x-apiosk-marketplace-key";

/// Stable prefix for a Connect marketplace key (minted in 01-06). Distinguishes
/// it from agent connect tokens (`aw_`) and provider SDK keys (`sk_live_`) so the
/// three never cross-match. The real validation is the hash lookup; this only
/// classifies which credential was presented.
pub const MARKETPLACE_API_KEY_PREFIX: &str = "apk_live_";

/// Prefix for a PUBLISHABLE marketplace key (045) — safe to embed in client-side
/// HTML (the "Connect with Apiosk" snippet). Deliberately distinct from the
/// secret `apk_live_` prefix so the two can never be confused: the secret-key
/// middleware below only ever matches `apk_live_`, and the publishable key only
/// authenticates the narrow self-serve embed-start endpoint.
pub const MARKETPLACE_PUBLISHABLE_KEY_PREFIX: &str = "apk_pub_";

/// True when `token` is shaped like a publishable embed key. Pure; the real check
/// is the `kind = 'publishable'` hash lookup in the embed-start handler.
pub fn is_publishable_marketplace_api_key_value(token: &str) -> bool {
    let token = token.trim();
    token.starts_with(MARKETPLACE_PUBLISHABLE_KEY_PREFIX) && token.len() >= 16
}

/// Resolved marketplace identity, inserted into the request extensions by
/// [`require_marketplace_api_key`]. Downstream handlers read it via
/// `req.extensions().get::<MarketplaceContext>()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketplaceContext {
    pub marketplace_id: Uuid,
}

/// SHA-256 of the raw key, hex-encoded — the exact value stored as
/// `connect_api_keys.key_hash` at mint time (01-06). Pure + deterministic.
pub fn hash_marketplace_api_key(raw_key: &str) -> String {
    let digest = Sha256::digest(raw_key.trim().as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Extract a marketplace key from either the dedicated header or an
/// `Authorization: Bearer mk_live_…` token. Returns `None` when no
/// marketplace-shaped credential is present. Pure.
pub fn extract_marketplace_api_key(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers
        .get(MARKETPLACE_API_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| is_marketplace_api_key_value(v))
    {
        return Some(value.to_string());
    }

    headers
        .get(header::AUTHORIZATION)
        .and_then(extract_bearer_token)
        .filter(|token| is_marketplace_api_key_value(token))
        .map(str::to_string)
}

/// Turn a key lookup outcome into either the request context or a 401. Pure, so
/// the 401 / success branches are unit-testable without a database. Keeping this
/// separate from the async middleware is what lets the acceptance tests cover the
/// "missing/invalid → 401" and "valid → marketplace_id propagated" paths.
pub fn authorize(lookup: Option<Uuid>) -> Result<MarketplaceContext, StatusCode> {
    match lookup {
        Some(marketplace_id) => Ok(MarketplaceContext { marketplace_id }),
        None => Err(StatusCode::UNAUTHORIZED),
    }
}

/// Axum middleware enforcing marketplace API-key auth on the connect `/v1/*`
/// surface. On success, inserts [`MarketplaceContext`] into the request
/// extensions and forwards; otherwise returns 401 with a JSON error.
pub async fn require_marketplace_api_key(
    State(state): State<Arc<AppState>>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let Some(raw_key) = extract_marketplace_api_key(request.headers()) else {
        return unauthorized();
    };

    let key_hash = hash_marketplace_api_key(&raw_key);
    let lookup = match state.db.lookup_marketplace_by_api_key(&key_hash).await {
        Ok(found) => found,
        Err(err) => {
            // Never log the key or hash. A lookup error is a 401 to the caller
            // (fail closed) but visible in logs for operators.
            tracing::warn!("marketplace api-key lookup failed: {err}");
            return unauthorized();
        }
    };

    match authorize(lookup) {
        Ok(context) => {
            request.extensions_mut().insert(context);
            next.run(request).await
        }
        Err(_) => unauthorized(),
    }
}

fn unauthorized() -> Response {
    let body = r#"{"error":"unauthorized","message":"Missing or invalid marketplace API key."}"#;
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn is_marketplace_api_key_value(token: &str) -> bool {
    let token = token.trim();
    token.starts_with(MARKETPLACE_API_KEY_PREFIX) && token.len() >= 16
}

fn extract_bearer_token(value: &HeaderValue) -> Option<&str> {
    let trimmed = value.to_str().ok()?.trim();
    trimmed
        .strip_prefix("Bearer ")
        .or_else(|| trimmed.strip_prefix("bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{header, HeaderMap, HeaderValue};

    const SAMPLE_KEY: &str = "apk_live_0123456789abcdef0123456789abcdef";

    #[test]
    fn hashes_are_deterministic_and_match_known_sha256() {
        // sha256("abc") — fixed vector proves we emit lowercase hex of the digest.
        assert_eq!(
            hash_marketplace_api_key("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hash_marketplace_api_key(SAMPLE_KEY),
            hash_marketplace_api_key(SAMPLE_KEY)
        );
    }

    #[test]
    fn hash_ignores_surrounding_whitespace() {
        assert_eq!(
            hash_marketplace_api_key(SAMPLE_KEY),
            hash_marketplace_api_key(&format!("  {SAMPLE_KEY}  "))
        );
    }

    #[test]
    fn extracts_key_from_dedicated_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            MARKETPLACE_API_KEY_HEADER,
            HeaderValue::from_static(SAMPLE_KEY),
        );
        assert_eq!(
            extract_marketplace_api_key(&headers).as_deref(),
            Some(SAMPLE_KEY)
        );
    }

    #[test]
    fn extracts_key_from_bearer() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {SAMPLE_KEY}")).unwrap(),
        );
        assert_eq!(
            extract_marketplace_api_key(&headers).as_deref(),
            Some(SAMPLE_KEY)
        );
    }

    #[test]
    fn ignores_missing_and_foreign_credentials() {
        // No credential at all.
        assert!(extract_marketplace_api_key(&HeaderMap::new()).is_none());

        // An agent connect token / provider key must not be mistaken for one.
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer sk_live_deadbeefdeadbeefdeadbeef"),
        );
        assert!(extract_marketplace_api_key(&headers).is_none());
    }

    #[test]
    fn authorize_rejects_unknown_key_with_401() {
        assert_eq!(authorize(None).unwrap_err(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn authorize_propagates_marketplace_id() {
        let id = Uuid::from_u128(0xA11CE);
        let ctx = authorize(Some(id)).expect("authorized");
        assert_eq!(ctx.marketplace_id, id);
    }
}
