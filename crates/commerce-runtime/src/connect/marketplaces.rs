//! 01-06 — Owner endpoint to mint a marketplace API key.
//!
//! `POST /v1/marketplaces/{id}/api-keys` (owner-authenticated). Generates a
//! cryptographically strong key, returns the RAW key ONCE, persists only its
//! SHA-256 hash + last4, flips the marketplace to `active`, and writes an audit
//! row. The raw key is never logged and never retrievable again.
//!
//! Auth: the portal session (Supabase user) via [`owner_auth`]. Authorization:
//! the caller must own the marketplace ([`authorize_owner`] → 403/404). The
//! Idempotency-Key contract from 00-07 is enforced inline (the gateway applies
//! idempotency inline rather than as a tower layer; see `proxy::handle_flow`).

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::json;
use tracing::warn;
use uuid::Uuid;

use super::api_key_auth::{
    hash_marketplace_api_key, MARKETPLACE_API_KEY_PREFIX, MARKETPLACE_PUBLISHABLE_KEY_PREFIX,
};
use super::audit::audit_log;
use super::idempotency::{parse_idempotency_key, KeyOutcome};
use super::owner_auth::resolve_owner_user_id;
use super::{json_error, json_response};
use crate::AppState;

pub async fn generate_marketplace_api_key(
    State(state): State<Arc<AppState>>,
    Path(marketplace_id): Path<Uuid>,
    headers: HeaderMap,
) -> Response {
    // 00-07 contract: a write requires an Idempotency-Key.
    match parse_idempotency_key(&headers) {
        KeyOutcome::Valid(_) => {}
        KeyOutcome::Absent => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "idempotency_key_required",
                "Write requests require an Idempotency-Key header.",
            );
        }
        KeyOutcome::Malformed => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "invalid_idempotency_key",
                "Idempotency-Key is present but empty, oversized, or non-ASCII.",
            );
        }
    }

    let owner_user_id = match resolve_owner_user_id(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };

    let marketplace_owner = match state.db.get_connect_marketplace_owner(marketplace_id).await {
        Ok(found) => found,
        Err(err) => {
            warn!("connect marketplace owner lookup failed: {err}");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "lookup_failed",
                "Could not load the marketplace.",
            );
        }
    };
    if let Err(status) = authorize_owner(marketplace_owner, owner_user_id) {
        return match status {
            StatusCode::NOT_FOUND => json_error(
                StatusCode::NOT_FOUND,
                "marketplace_not_found",
                "Marketplace not found.",
            ),
            _ => json_error(
                StatusCode::FORBIDDEN,
                "forbidden",
                "You do not own this marketplace.",
            ),
        };
    }

    // CSPRNG key. The raw value lives only in this function + the response.
    let raw_key = match generate_raw_api_key(MARKETPLACE_API_KEY_PREFIX) {
        Ok(key) => key,
        Err(()) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "key_generation_failed",
                "Could not generate a secure key.",
            );
        }
    };
    let key_hash = hash_marketplace_api_key(&raw_key);
    let last4 = key_last4(&raw_key);

    let (key_id, created_at) = match state
        .db
        .insert_connect_api_key(marketplace_id, &key_hash, &last4, None, "secret", None)
        .await
    {
        Ok(row) => row,
        Err(err) => {
            warn!("connect api key insert failed: {err}");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "key_persist_failed",
                "Could not store the new key.",
            );
        }
    };

    // First key activates the marketplace. Best-effort: a key was minted even if
    // the status flip fails, so don't fail the response on it.
    if let Err(err) = state.db.activate_connect_marketplace(marketplace_id).await {
        warn!("connect marketplace activate failed: {err}");
    }

    // Audit (raw key never included; audit_log sanitizes metadata).
    audit_log(
        &state.db,
        &format!("user:{owner_user_id}"),
        "api_key.created",
        "marketplace",
        Some(marketplace_id),
        None,
        Some(json!({ "key_id": key_id, "last4": last4 })),
    )
    .await;

    json_response(
        StatusCode::CREATED,
        json!({
            "id": key_id,
            "api_key": raw_key,
            "last4": last4,
            "created_at": created_at.to_rfc3339(),
        }),
    )
}

/* -------------------------------------------------------------------------- */
/* Publishable embed key (045)                                                */
/* -------------------------------------------------------------------------- */

/// `POST /v1/marketplaces/{id}/publishable-keys` (owner-authenticated). Mints the
/// marketplace's PUBLISHABLE key (`apk_pub_…`) for the "Connect with Apiosk"
/// embed snippet. Unlike the secret key this value is public by design, so it is
/// returned AND persisted in full (`public_key`) so the portal can re-render the
/// snippet anytime. Rotating mints a fresh key and revokes the previous one.
pub async fn generate_marketplace_publishable_key(
    State(state): State<Arc<AppState>>,
    Path(marketplace_id): Path<Uuid>,
    headers: HeaderMap,
) -> Response {
    // 00-07 contract: a write requires an Idempotency-Key.
    match parse_idempotency_key(&headers) {
        KeyOutcome::Valid(_) => {}
        KeyOutcome::Absent => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "idempotency_key_required",
                "Write requests require an Idempotency-Key header.",
            );
        }
        KeyOutcome::Malformed => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "invalid_idempotency_key",
                "Idempotency-Key is present but empty, oversized, or non-ASCII.",
            );
        }
    }

    let owner_user_id = match resolve_owner_user_id(&state, &headers).await {
        Ok(id) => id,
        Err(response) => return response,
    };

    let marketplace_owner = match state.db.get_connect_marketplace_owner(marketplace_id).await {
        Ok(found) => found,
        Err(err) => {
            warn!("connect marketplace owner lookup failed: {err}");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "lookup_failed",
                "Could not load the marketplace.",
            );
        }
    };
    if let Err(status) = authorize_owner(marketplace_owner, owner_user_id) {
        return match status {
            StatusCode::NOT_FOUND => json_error(
                StatusCode::NOT_FOUND,
                "marketplace_not_found",
                "Marketplace not found.",
            ),
            _ => json_error(
                StatusCode::FORBIDDEN,
                "forbidden",
                "You do not own this marketplace.",
            ),
        };
    }

    let raw_key = match generate_raw_api_key(MARKETPLACE_PUBLISHABLE_KEY_PREFIX) {
        Ok(key) => key,
        Err(()) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "key_generation_failed",
                "Could not generate a secure key.",
            );
        }
    };
    let key_hash = hash_marketplace_api_key(&raw_key);
    let last4 = key_last4(&raw_key);

    // Rotate: a marketplace keeps at most one live publishable key (enforced by
    // the partial unique index), so revoke any prior one before inserting.
    if let Err(err) = state
        .db
        .revoke_connect_publishable_keys(marketplace_id)
        .await
    {
        warn!("connect publishable key revoke failed: {err}");
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "key_rotate_failed",
            "Could not rotate the publishable key.",
        );
    }

    let (key_id, created_at) = match state
        .db
        .insert_connect_api_key(
            marketplace_id,
            &key_hash,
            &last4,
            Some("Embed (publishable)"),
            "publishable",
            Some(&raw_key),
        )
        .await
    {
        Ok(row) => row,
        Err(err) => {
            warn!("connect publishable key insert failed: {err}");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "key_persist_failed",
                "Could not store the new key.",
            );
        }
    };

    audit_log(
        &state.db,
        &format!("user:{owner_user_id}"),
        "publishable_key.created",
        "marketplace",
        Some(marketplace_id),
        None,
        Some(json!({ "key_id": key_id, "last4": last4 })),
    )
    .await;

    json_response(
        StatusCode::CREATED,
        json!({
            "id": key_id,
            "publishable_key": raw_key,
            "last4": last4,
            "created_at": created_at.to_rfc3339(),
        }),
    )
}

/// Ownership check: 404 if the marketplace doesn't exist, 403 if a different user
/// owns it, Ok if the requester owns it. Pure — unit-tested.
pub fn authorize_owner(marketplace_owner: Option<Uuid>, requester: Uuid) -> Result<(), StatusCode> {
    match marketplace_owner {
        None => Err(StatusCode::NOT_FOUND),
        Some(owner) if owner == requester => Ok(()),
        Some(_) => Err(StatusCode::FORBIDDEN),
    }
}

/// `<prefix>` + 32 CSPRNG bytes (hex). Uses ring's system RNG — never a
/// non-crypto `rand`. The prefix (`apk_live_` secret / `apk_pub_` publishable)
/// classifies the key; the random body is the secret.
fn generate_raw_api_key(prefix: &str) -> Result<String, ()> {
    let rng = SystemRandom::new();
    let mut bytes = [0u8; 32];
    rng.fill(&mut bytes).map_err(|_| ())?;
    Ok(format!("{prefix}{}", hex::encode(bytes)))
}

/// Last 4 characters of the raw key, for masked display.
fn key_last4(raw_key: &str) -> String {
    let chars: Vec<char> = raw_key.chars().collect();
    let start = chars.len().saturating_sub(4);
    chars[start..].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_key_has_prefix_and_expected_length() {
        let key = generate_raw_api_key(MARKETPLACE_API_KEY_PREFIX).expect("rng");
        assert!(key.starts_with("apk_live_"));
        // apk_live_ (9) + 32 bytes hex (64) = 73 chars.
        assert_eq!(key.len(), 73);
        // Two independent calls must differ.
        assert_ne!(
            key,
            generate_raw_api_key(MARKETPLACE_API_KEY_PREFIX).expect("rng")
        );
    }

    #[test]
    fn publishable_key_has_pub_prefix() {
        let key = generate_raw_api_key(MARKETPLACE_PUBLISHABLE_KEY_PREFIX).expect("rng");
        assert!(key.starts_with("apk_pub_"));
        // apk_pub_ (8) + 32 bytes hex (64) = 72 chars.
        assert_eq!(key.len(), 72);
        assert!(super::super::api_key_auth::is_publishable_marketplace_api_key_value(&key));
    }

    #[test]
    fn hash_is_reproducible_for_a_given_key() {
        let key = generate_raw_api_key(MARKETPLACE_API_KEY_PREFIX).expect("rng");
        assert_eq!(
            hash_marketplace_api_key(&key),
            hash_marketplace_api_key(&key)
        );
        // 64 hex chars of SHA-256.
        assert_eq!(hash_marketplace_api_key(&key).len(), 64);
    }

    #[test]
    fn last4_is_the_final_four_chars() {
        assert_eq!(key_last4("apk_live_abcd1234"), "1234");
        assert_eq!(key_last4("xyz"), "xyz");
    }

    #[test]
    fn authorize_owner_matrix() {
        let owner = Uuid::from_u128(1);
        let other = Uuid::from_u128(2);
        // Unknown marketplace -> 404.
        assert_eq!(
            authorize_owner(None, owner).unwrap_err(),
            StatusCode::NOT_FOUND
        );
        // Different owner -> 403.
        assert_eq!(
            authorize_owner(Some(other), owner).unwrap_err(),
            StatusCode::FORBIDDEN
        );
        // Owner matches -> Ok.
        assert!(authorize_owner(Some(owner), owner).is_ok());
    }
}
