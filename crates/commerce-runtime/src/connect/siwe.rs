//! 02-04 / 02-05 — SIWE wallet verification for merchant onboarding.
//!
//! The seller proves ownership of a payout wallet by signing a one-time,
//! merchant-bound nonce (EIP-191 `personal_sign`). 02-04 issues the nonce
//! (CSPRNG, short TTL, single-use, bound to the merchant session); 02-05 rebuilds
//! the canonical message, recovers the signer with `ethers`, and stores the
//! address (EIP-55) as the merchant's active verified wallet. v1 is Base-only.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Response,
};
use ethers_core::{types::Address, utils::to_checksum};
use moka::future::Cache;
use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::session::require_merchant_session;
use super::{json_error, json_response};
use crate::auth::recover_personal_sign;
use crate::AppState;

/// v1 supports only Base. (Mainnet 8453 / Sepolia 84532 — both report `base`.)
const CONNECT_WALLET_CHAIN: &str = "base";

/// One-time, merchant-bound SIWE nonces. nonce -> merchant_id, 10-min TTL.
/// Per-instance like `AuthReplayCache`; fine for the single-instance gateway.
pub struct ConnectNonceCache {
    nonces: Cache<String, Uuid>,
}

impl ConnectNonceCache {
    pub fn new() -> Self {
        let nonces = Cache::builder()
            .time_to_live(Duration::from_secs(600))
            .max_capacity(100_000)
            .build();
        Self { nonces }
    }

    async fn issue(&self, nonce: &str, merchant_id: Uuid) {
        self.nonces.insert(nonce.to_string(), merchant_id).await;
    }

    /// Single-use consume: true only if the nonce exists AND is bound to this
    /// merchant; removes it either way it matches so it can't be replayed.
    async fn consume(&self, nonce: &str, merchant_id: Uuid) -> bool {
        match self.nonces.get(nonce).await {
            Some(bound) if bound == merchant_id => {
                self.nonces.invalidate(nonce).await;
                true
            }
            _ => false,
        }
    }
}

impl Default for ConnectNonceCache {
    fn default() -> Self {
        Self::new()
    }
}

/// The exact message the wallet signs. Built identically on the client and here,
/// so the recovered signer is meaningful. Address is lowercased on both sides to
/// avoid checksum-case mismatches.
pub fn build_siwe_message(merchant_id: Uuid, address: &str, nonce: &str) -> String {
    format!(
        "Apiosk Connect — verify merchant payout wallet\nmerchant: {merchant_id}\naddress: {}\nchain: {CONNECT_WALLET_CHAIN}\nnonce: {nonce}",
        address.trim().to_ascii_lowercase()
    )
}

fn random_nonce() -> Result<String, ()> {
    let rng = SystemRandom::new();
    let mut bytes = [0u8; 16];
    rng.fill(&mut bytes).map_err(|_| ())?;
    Ok(hex::encode(bytes))
}

/// POST /v1/siwe/nonce — issue a one-time nonce for the authenticated merchant.
pub async fn issue_nonce(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let session = match require_merchant_session(&state, &headers) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    let nonce = match random_nonce() {
        Ok(n) => n,
        Err(()) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "nonce_generation_failed",
                "Could not generate a nonce.",
            )
        }
    };
    state
        .connect_nonce_cache
        .issue(&nonce, session.merchant_id)
        .await;
    json_response(StatusCode::OK, json!({ "nonce": nonce }))
}

#[derive(Deserialize)]
pub struct SiweVerifyRequest {
    address: String,
    signature: String,
    nonce: String,
    #[serde(default)]
    chain: Option<String>,
}

/// POST /v1/siwe/verify — verify the signature, consume the nonce, store the
/// active verified wallet.
pub async fn verify_wallet(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: axum::extract::Json<SiweVerifyRequest>,
) -> Response {
    let session = match require_merchant_session(&state, &headers) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    let req = body.0;

    // v1: Base only.
    if let Some(chain) = req
        .chain
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        if !chain.eq_ignore_ascii_case(CONNECT_WALLET_CHAIN) {
            return json_error(
                StatusCode::BAD_REQUEST,
                "unsupported_chain",
                "Only the Base network is supported in v1.",
            );
        }
    }

    let Ok(claimed) = Address::from_str(req.address.trim()) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_address",
            "Malformed wallet address.",
        );
    };

    // Single-use, merchant-bound nonce. Consume BEFORE signature work so a
    // replay can't reuse it even on a verification failure.
    if !state
        .connect_nonce_cache
        .consume(req.nonce.trim(), session.merchant_id)
        .await
    {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_nonce",
            "Nonce is unknown, expired, already used, or not bound to this session.",
        );
    }

    let message = build_siwe_message(session.merchant_id, &req.address, req.nonce.trim());
    let Some(recovered) = recover_personal_sign(&message, &req.signature) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_signature",
            "Signature could not be recovered.",
        );
    };
    if recovered != claimed {
        return json_error(
            StatusCode::UNAUTHORIZED,
            "signature_mismatch",
            "Signature does not match the provided address.",
        );
    }

    let address = to_checksum(&claimed, None);
    if let Err(err) = state
        .db
        .store_connect_verified_wallet(session.merchant_id, CONNECT_WALLET_CHAIN, &address)
        .await
    {
        tracing::error!("connect siwe: failed to store wallet: {err}");
        return json_error(
            StatusCode::BAD_GATEWAY,
            "persist_failed",
            "Could not store the verified wallet.",
        );
    }

    json_response(
        StatusCode::OK,
        json!({ "verified": true, "address": address }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_is_stable_and_lowercases_address() {
        let m = Uuid::from_u128(7);
        let upper = build_siwe_message(m, "0xAbCdEf0000000000000000000000000000000001", "abc123");
        let lower = build_siwe_message(m, "0xabcdef0000000000000000000000000000000001", "abc123");
        assert_eq!(
            upper, lower,
            "checksum case must not change the signed message"
        );
        assert!(upper.contains("nonce: abc123"));
        assert!(upper.contains("chain: base"));
        assert!(upper.contains(&format!("merchant: {m}")));
    }

    #[test]
    fn message_changes_with_nonce_and_address() {
        let m = Uuid::from_u128(7);
        let a = build_siwe_message(m, "0x01", "nonce-a");
        let b = build_siwe_message(m, "0x01", "nonce-b");
        let c = build_siwe_message(m, "0x02", "nonce-a");
        assert_ne!(a, b);
        assert_ne!(a, c);
    }
}
