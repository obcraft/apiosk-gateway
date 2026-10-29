//! Self-serve embed onboarding (045) — the public entry point behind the
//! "Connect with Apiosk" snippet.
//!
//! `POST /v1/connect/embed/start` is the publishable-key counterpart to the
//! marketplace-minted onboarding link (02-01/02-02): instead of a marketplace
//! pre-minting a per-seller token with its SECRET key, a visitor on the
//! marketplace's own site starts onboarding themselves, authenticated only by the
//! marketplace's PUBLISHABLE key (`apk_pub_…`). It resolves that key to a
//! marketplace, upserts a draft merchant for the caller's stable browser ref, and
//! returns the same merchant-session token the wizard already consumes — so the
//! rest of onboarding (profile → wallet → activate) is unchanged.
//!
//! Safe to be public: a publishable key only lets someone create a DRAFT merchant
//! for themselves; it never reads other merchants or touches the secret surface
//! (the `kind = 'publishable'` lookup is the boundary).

use std::sync::Arc;

use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::Response,
};
use serde::Deserialize;
use serde_json::json;
use tracing::warn;

use super::api_key_auth::{hash_marketplace_api_key, is_publishable_marketplace_api_key_value};
use super::tokens::{encode_token, now_unix, MerchantSessionClaims};
use super::{json_error, json_response};
use crate::AppState;

/// Merchant session lifetime — matches the onboarding-link path (02-02).
const MERCHANT_SESSION_TTL_SECS: i64 = 60 * 60;

/// Cap on the caller-supplied browser ref so a hostile embed can't write
/// unbounded `external_seller_id` values.
const MAX_EXTERNAL_REF_LEN: usize = 100;

#[derive(Deserialize)]
pub struct EmbedStartRequest {
    publishable_key: String,
    /// Stable per-browser identifier the embed script generates and persists, so
    /// re-clicking resumes the same draft merchant instead of spawning a new one.
    /// Optional — absent means a one-off connect (the gateway makes one up).
    #[serde(default)]
    external_ref: Option<String>,
}

pub async fn start_embed_onboarding(
    State(state): State<Arc<AppState>>,
    body: Json<EmbedStartRequest>,
) -> Response {
    let Some(secret) = state.config.connect_hmac_secret.as_deref() else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "connect_not_configured",
            "Apiosk Connect is not configured on the gateway.",
        );
    };

    let raw_key = body.0.publishable_key.trim();
    if !is_publishable_marketplace_api_key_value(raw_key) {
        return json_error(
            StatusCode::UNAUTHORIZED,
            "invalid_publishable_key",
            "Missing or malformed marketplace publishable key.",
        );
    }

    let key_hash = hash_marketplace_api_key(raw_key);
    let marketplace_id = match state
        .db
        .lookup_marketplace_by_publishable_key(&key_hash)
        .await
    {
        Ok(Some(id)) => id,
        Ok(None) => {
            return json_error(
                StatusCode::UNAUTHORIZED,
                "invalid_publishable_key",
                "This marketplace publishable key is unknown or has been revoked.",
            );
        }
        Err(err) => {
            // Never log the key or hash. Fail closed.
            warn!("publishable key lookup failed: {err}");
            return json_error(
                StatusCode::UNAUTHORIZED,
                "invalid_publishable_key",
                "Could not verify the publishable key.",
            );
        }
    };

    let external_seller_id = derive_external_seller_id(body.0.external_ref.as_deref());

    let merchant_id = match state
        .db
        .upsert_connect_merchant(marketplace_id, &external_seller_id)
        .await
    {
        Ok(id) => id,
        Err(err) => {
            warn!("embed onboarding upsert failed: {err}");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "merchant_upsert_failed",
                "Could not start onboarding.",
            );
        }
    };

    // Best-effort display name + logo for the connect screen branding and the
    // parent-page session container.
    let (marketplace_name, marketplace_logo) = state
        .db
        .get_connect_marketplace_brand(marketplace_id)
        .await
        .ok()
        .flatten()
        .map(|(name, logo)| (Some(name), logo))
        .unwrap_or((None, None));

    let session = encode_token(
        secret,
        &MerchantSessionClaims {
            merchant_id,
            marketplace_id,
            exp: now_unix() + MERCHANT_SESSION_TTL_SECS,
        },
    );

    json_response(
        StatusCode::OK,
        json!({
            "merchant_id": merchant_id,
            "marketplace_id": marketplace_id,
            "marketplace_name": marketplace_name,
            "marketplace_logo": marketplace_logo,
            "session": session,
            "status": "draft",
        }),
    )
}

/// Namespace + bound the seller id so an embed-originated merchant can never
/// collide with a marketplace-minted `external_seller_id` (02-01) and a hostile
/// caller can't write an unbounded value. A blank/oversized ref falls back to a
/// fresh random id (a one-off connect). Pure.
fn derive_external_seller_id(external_ref: Option<&str>) -> String {
    let cleaned = external_ref
        .map(str::trim)
        .filter(|r| !r.is_empty() && r.len() <= MAX_EXTERNAL_REF_LEN);
    match cleaned {
        Some(r) => format!("embed:{r}"),
        None => format!("embed:{}", uuid::Uuid::new_v4()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_is_namespaced_when_provided() {
        assert_eq!(
            derive_external_seller_id(Some("browser-abc")),
            "embed:browser-abc"
        );
        assert_eq!(
            derive_external_seller_id(Some("  browser-abc  ")),
            "embed:browser-abc"
        );
    }

    #[test]
    fn blank_or_oversized_ref_falls_back_to_random() {
        // Blank → random embed:<uuid>.
        let a = derive_external_seller_id(Some("   "));
        let b = derive_external_seller_id(None);
        assert!(a.starts_with("embed:") && a.len() > "embed:".len());
        assert!(b.starts_with("embed:") && b.len() > "embed:".len());
        assert_ne!(a, b); // two random ids differ

        // Oversized → random, never the raw value.
        let big = "x".repeat(MAX_EXTERNAL_REF_LEN + 1);
        let c = derive_external_seller_id(Some(&big));
        assert!(c.starts_with("embed:"));
        assert!(!c.contains(&big));
    }
}
