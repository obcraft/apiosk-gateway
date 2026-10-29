//! HMAC-signed Connect tokens — merchant onboarding links (02-01/02-02) and
//! merchant sessions (the seller is not a Supabase user, so the wizard carries a
//! signed, stateless session token instead of a cookie).
//!
//! Format: `hex(json_claims).hmac_sha256_hex`. The signature covers the hex
//! payload; verification is constant-time via `ring::hmac::verify`. The secret is
//! the platform `CONNECT_HMAC_SECRET` and is never logged. Same HMAC primitive as
//! the gateway's outbound request signing (`proxy::wallet::compute_hmac_…`).

use std::time::{SystemTime, UNIX_EPOCH};

use ring::hmac;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use uuid::Uuid;

/// HMAC-SHA256 of `msg` under `secret`, hex-encoded.
pub fn hmac_sha256_hex(secret: &str, msg: &[u8]) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    let tag = hmac::sign(&key, msg);
    hex::encode(tag.as_ref())
}

/// Constant-time verify of a hex signature over `msg`.
pub fn hmac_verify(secret: &str, msg: &[u8], sig_hex: &str) -> bool {
    let Ok(sig) = hex::decode(sig_hex.trim()) else {
        return false;
    };
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    hmac::verify(&key, msg, &sig).is_ok()
}

/// Claims carry an absolute expiry (unix seconds).
pub trait HasExp {
    fn exp(&self) -> i64;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OnboardingClaims {
    pub marketplace_id: Uuid,
    pub external_seller_id: String,
    pub exp: i64,
}
impl HasExp for OnboardingClaims {
    fn exp(&self) -> i64 {
        self.exp
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MerchantSessionClaims {
    pub merchant_id: Uuid,
    pub marketplace_id: Uuid,
    pub exp: i64,
}
impl HasExp for MerchantSessionClaims {
    fn exp(&self) -> i64 {
        self.exp
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum TokenError {
    Malformed,
    BadSignature,
    Expired,
}

/// Sign claims into a `hex(json).sig` token.
pub fn encode_token<T: Serialize>(secret: &str, claims: &T) -> String {
    let payload = serde_json::to_vec(claims).unwrap_or_default();
    let payload_hex = hex::encode(payload);
    let sig = hmac_sha256_hex(secret, payload_hex.as_bytes());
    format!("{payload_hex}.{sig}")
}

/// Verify signature (constant-time) + expiry, then return the claims. `now` is
/// unix seconds (injected so tests are deterministic).
pub fn decode_token<T: DeserializeOwned + HasExp>(
    secret: &str,
    token: &str,
    now: i64,
) -> Result<T, TokenError> {
    let (payload_hex, sig) = token.trim().split_once('.').ok_or(TokenError::Malformed)?;
    if !hmac_verify(secret, payload_hex.as_bytes(), sig) {
        return Err(TokenError::BadSignature);
    }
    let payload = hex::decode(payload_hex).map_err(|_| TokenError::Malformed)?;
    let claims: T = serde_json::from_slice(&payload).map_err(|_| TokenError::Malformed)?;
    if now >= claims.exp() {
        return Err(TokenError::Expired);
    }
    Ok(claims)
}

/// Current unix time in seconds.
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "test-connect-secret";

    fn onboarding(exp: i64) -> OnboardingClaims {
        OnboardingClaims {
            marketplace_id: Uuid::from_u128(1),
            external_seller_id: "seller-1".into(),
            exp,
        }
    }

    #[test]
    fn hmac_is_deterministic_and_payload_sensitive() {
        let a = hmac_sha256_hex(SECRET, b"hello");
        assert_eq!(a, hmac_sha256_hex(SECRET, b"hello"));
        assert_ne!(a, hmac_sha256_hex(SECRET, b"hell0"));
        assert_ne!(a, hmac_sha256_hex("other-secret", b"hello"));
        // 32-byte tag -> 64 hex chars.
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn round_trips_valid_token() {
        let token = encode_token(SECRET, &onboarding(1_000));
        let decoded: OnboardingClaims = decode_token(SECRET, &token, 500).expect("valid");
        assert_eq!(decoded, onboarding(1_000));
    }

    #[test]
    fn rejects_expired_token() {
        let token = encode_token(SECRET, &onboarding(1_000));
        assert_eq!(
            decode_token::<OnboardingClaims>(SECRET, &token, 1_000).unwrap_err(),
            TokenError::Expired
        );
        assert_eq!(
            decode_token::<OnboardingClaims>(SECRET, &token, 2_000).unwrap_err(),
            TokenError::Expired
        );
    }

    #[test]
    fn rejects_tampered_payload_or_signature() {
        let token = encode_token(SECRET, &onboarding(1_000));
        let (payload_hex, sig) = token.split_once('.').unwrap();

        // Flip a payload byte -> signature no longer matches.
        let mut bad_payload = payload_hex.to_string();
        bad_payload.replace_range(0..1, if &bad_payload[0..1] == "a" { "b" } else { "a" });
        let tampered = format!("{bad_payload}.{sig}");
        assert_eq!(
            decode_token::<OnboardingClaims>(SECRET, &tampered, 500).unwrap_err(),
            TokenError::BadSignature
        );

        // Wrong secret -> bad signature.
        assert_eq!(
            decode_token::<OnboardingClaims>("attacker", &token, 500).unwrap_err(),
            TokenError::BadSignature
        );
    }

    #[test]
    fn rejects_malformed_token() {
        assert_eq!(
            decode_token::<OnboardingClaims>(SECRET, "no-dot-here", 500).unwrap_err(),
            TokenError::Malformed
        );
    }
}
