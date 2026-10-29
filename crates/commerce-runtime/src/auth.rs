use axum::http::{header, HeaderMap, HeaderValue};
use ethers_core::types::{Address, Signature};
use ethers_core::utils::hash_message;
use moka::future::Cache;
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const HEADER_WALLET_ADDRESS: &str = "x-wallet-address";
const HEADER_WALLET_SIGNATURE: &str = "x-wallet-signature";
const HEADER_WALLET_TIMESTAMP: &str = "x-wallet-timestamp";
const HEADER_WALLET_NONCE: &str = "x-wallet-nonce";
const HEADER_USER_JWT: &str = "x-apiosk-user-jwt";
const MAX_AUTH_AGE_SECS: i64 = 300;
const INTERNAL_GATEWAY_HEADERS: &[&str] = &[
    "x-apiosk-connect-token",
    "x-apio-connect-token",
    "x-connect-token",
    HEADER_USER_JWT,
    "x-apiosk-session-id",
    "x-apiosk-request-id",
    "x-request-id",
    // Provider SDK key — internal credential, never forwarded upstream.
    PROVIDER_API_KEY_HEADER,
];

/// Header an SDK/CLI/MCP caller may use to present a provider key, as an
/// alternative to `Authorization: Bearer sk_live_…`.
pub const PROVIDER_API_KEY_HEADER: &str = "x-apiosk-api-key";

pub struct AuthReplayCache {
    used_signatures: Cache<String, bool>,
}

impl AuthReplayCache {
    pub fn new() -> Self {
        let used_signatures = Cache::builder()
            .time_to_live(Duration::from_secs(600))
            .max_capacity(100_000)
            .build();
        Self { used_signatures }
    }

    pub async fn is_replay(&self, key: &str) -> bool {
        if self.used_signatures.get(key).await.is_some() {
            return true;
        }
        self.used_signatures.insert(key.to_string(), true).await;
        false
    }
}

impl Default for AuthReplayCache {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub enum AuthError {
    MissingHeaders,
    InvalidWallet,
    InvalidSignature,
    Expired,
    Replay,
    WalletMismatch,
}

pub async fn verify_wallet_auth(
    headers: &HeaderMap,
    replay_cache: &AuthReplayCache,
    action: &str,
    resource: &str,
    expected_wallet: Option<&str>,
) -> Result<String, AuthError> {
    let wallet = header_value(headers, HEADER_WALLET_ADDRESS).ok_or(AuthError::MissingHeaders)?;
    let signature =
        header_value(headers, HEADER_WALLET_SIGNATURE).ok_or(AuthError::MissingHeaders)?;
    let timestamp =
        header_value(headers, HEADER_WALLET_TIMESTAMP).ok_or(AuthError::MissingHeaders)?;
    let nonce = header_value(headers, HEADER_WALLET_NONCE).ok_or(AuthError::MissingHeaders)?;

    if let Some(expected) = expected_wallet {
        if !eq_wallet(expected, wallet) {
            return Err(AuthError::WalletMismatch);
        }
    }

    let timestamp_i64 = timestamp
        .parse::<i64>()
        .map_err(|_| AuthError::InvalidSignature)?;
    let now = now_unix();
    if (now - timestamp_i64).abs() > MAX_AUTH_AGE_SECS {
        return Err(AuthError::Expired);
    }

    let replay_key = format!("{wallet}:{signature}:{timestamp}:{nonce}:{action}:{resource}");
    if replay_cache.is_replay(&replay_key).await {
        return Err(AuthError::Replay);
    }

    let message = canonical_message(action, wallet, resource, timestamp_i64, nonce);
    let recovered = recover_signer(&message, signature)?;
    let claimed_wallet = Address::from_str(wallet).map_err(|_| AuthError::InvalidWallet)?;

    if recovered != claimed_wallet {
        return Err(AuthError::InvalidSignature);
    }

    Ok(wallet.to_ascii_lowercase())
}

/// Recover the EIP-191 (`personal_sign`) signer of `message`, or `None` if the
/// signature is malformed. Used to verify a provider owns a wallet they want to
/// link to Monerium: recover the signer of the ownership declaration and compare
/// it to the claimed address.
pub fn recover_personal_sign(message: &str, signature_hex: &str) -> Option<Address> {
    let sig = Signature::from_str(signature_hex.trim()).ok()?;
    let digest = hash_message(message);
    sig.recover(digest).ok()
}

pub fn extract_agent_connect_token(headers: &HeaderMap) -> Option<String> {
    let header_value = INTERNAL_GATEWAY_HEADERS[0..3]
        .iter()
        .find_map(|name| headers.get(*name))
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);

    if header_value.is_some() {
        return header_value;
    }

    headers
        .get(header::AUTHORIZATION)
        .and_then(extract_bearer_token)
        .filter(|token| is_agent_connect_token_value(token))
        .map(str::to_string)
}

/// Extract a provider SDK key (`sk_live_…`) presented either via the
/// `x-apiosk-api-key` header or `Authorization: Bearer …`. Lets an SDK/CLI/MCP
/// authenticate paid `/execute` calls with the same key it manages listings
/// with — the gateway resolves it to the owner's wallet (see `resolve_connect_token`).
pub fn extract_provider_api_key(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers
        .get(PROVIDER_API_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| is_provider_api_key_value(v))
    {
        return Some(value.to_string());
    }

    headers
        .get(header::AUTHORIZATION)
        .and_then(extract_bearer_token)
        .filter(|token| is_provider_api_key_value(token))
        .map(str::to_string)
}

pub fn extract_credits_user_jwt(headers: &HeaderMap) -> Option<String> {
    headers
        .get(HEADER_USER_JWT)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub fn should_strip_forwarded_header(header_name: &str, header_value: &HeaderValue) -> bool {
    header_name == header::HOST.as_str()
        || header_name == header::ACCEPT_ENCODING.as_str()
        || header_name == header::CONTENT_LENGTH.as_str()
        || header_name == header::TRANSFER_ENCODING.as_str()
        || header_name.starts_with("x-payment")
        || INTERNAL_GATEWAY_HEADERS.contains(&header_name)
        || (header_name == header::AUTHORIZATION.as_str()
            && (is_agent_connect_authorization_value(header_value)
                || is_provider_api_key_authorization_value(header_value)))
}

fn canonical_message(
    action: &str,
    wallet: &str,
    resource: &str,
    timestamp: i64,
    nonce: &str,
) -> String {
    format!(
        "Apiosk auth\naction:{action}\nwallet:{}\nresource:{resource}\ntimestamp:{timestamp}\nnonce:{nonce}",
        wallet.to_ascii_lowercase()
    )
}

fn recover_signer(message: &str, signature_hex: &str) -> Result<Address, AuthError> {
    let sig = Signature::from_str(signature_hex).map_err(|_| AuthError::InvalidSignature)?;
    let digest = hash_message(message);
    sig.recover(digest).map_err(|_| AuthError::InvalidSignature)
}

fn header_value<'a>(headers: &'a HeaderMap, key: &str) -> Option<&'a str> {
    headers.get(key).and_then(|v| v.to_str().ok())
}

fn eq_wallet(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn extract_bearer_token(value: &HeaderValue) -> Option<&str> {
    let trimmed = value.to_str().ok()?.trim();
    trimmed
        .strip_prefix("Bearer ")
        .or_else(|| trimmed.strip_prefix("bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

fn is_agent_connect_authorization_value(value: &HeaderValue) -> bool {
    extract_bearer_token(value)
        .map(is_agent_connect_token_value)
        .unwrap_or(false)
}

fn is_agent_connect_token_value(token: &str) -> bool {
    let token = token.trim();
    token.starts_with("aw_") && token.len() >= 16 && token.split('_').count() >= 4
}

fn is_provider_api_key_authorization_value(value: &HeaderValue) -> bool {
    extract_bearer_token(value)
        .map(is_provider_api_key_value)
        .unwrap_or(false)
}

/// A provider SDK key as minted by the portal: `sk_live_` + 64 hex chars. We
/// match on the stable prefix (and a length floor) — the real validation is the
/// hash lookup in `verify_provider_api_key`, so this only needs to classify.
fn is_provider_api_key_value(token: &str) -> bool {
    let token = token.trim();
    token.starts_with("sk_live_") && token.len() >= 16
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::{
        extract_agent_connect_token, extract_credits_user_jwt, extract_provider_api_key,
        should_strip_forwarded_header, PROVIDER_API_KEY_HEADER,
    };
    use axum::http::{header, HeaderMap, HeaderValue};

    const SAMPLE_SK: &str =
        "sk_live_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn extracts_provider_api_key_from_dedicated_header() {
        let mut headers = HeaderMap::new();
        headers.insert(PROVIDER_API_KEY_HEADER, HeaderValue::from_static(SAMPLE_SK));
        assert_eq!(
            extract_provider_api_key(&headers).as_deref(),
            Some(SAMPLE_SK)
        );
    }

    #[test]
    fn extracts_provider_api_key_from_bearer() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {SAMPLE_SK}")).unwrap(),
        );
        assert_eq!(
            extract_provider_api_key(&headers).as_deref(),
            Some(SAMPLE_SK)
        );
    }

    #[test]
    fn provider_api_key_and_connect_token_do_not_cross_match() {
        // An sk_live_ bearer is not a connect token, and an aw_ bearer is not a provider key.
        let mut sk = HeaderMap::new();
        sk.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {SAMPLE_SK}")).unwrap(),
        );
        assert!(extract_agent_connect_token(&sk).is_none());

        let mut aw = HeaderMap::new();
        aw.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer aw_live_abc123_deadbeefdeadbeef"),
        );
        assert!(extract_provider_api_key(&aw).is_none());
    }

    #[test]
    fn strips_provider_api_key_credentials_from_forwarded_requests() {
        // Both the bearer form and the dedicated header must never reach upstream.
        let value = HeaderValue::from_str(&format!("Bearer {SAMPLE_SK}")).unwrap();
        assert!(should_strip_forwarded_header(
            header::AUTHORIZATION.as_str(),
            &value
        ));
        assert!(should_strip_forwarded_header(
            PROVIDER_API_KEY_HEADER,
            &HeaderValue::from_static(SAMPLE_SK)
        ));
    }

    #[test]
    fn extracts_connect_token_from_bearer_fallback_only_for_agent_tokens() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer aw_live_abc123_deadbeefdeadbeef"),
        );
        let token = extract_agent_connect_token(&headers).expect("token");
        assert_eq!(token, "aw_live_abc123_deadbeefdeadbeef");
    }

    #[test]
    fn ignores_non_agent_bearer_tokens() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer ghp_example_token"),
        );
        assert!(extract_agent_connect_token(&headers).is_none());
    }

    #[test]
    fn strips_connect_token_authorization_from_forwarded_requests() {
        let value = HeaderValue::from_static("Bearer aw_live_abc123_deadbeefdeadbeef");
        assert!(should_strip_forwarded_header(
            header::AUTHORIZATION.as_str(),
            &value
        ));
    }

    #[test]
    fn preserves_non_connect_authorization_for_upstreams() {
        let value = HeaderValue::from_static("Bearer ghp_example_token");
        assert!(!should_strip_forwarded_header(
            header::AUTHORIZATION.as_str(),
            &value
        ));
    }

    #[test]
    fn strips_content_length_from_forwarded_requests() {
        let value = HeaderValue::from_static("123");
        assert!(should_strip_forwarded_header(
            header::CONTENT_LENGTH.as_str(),
            &value
        ));
    }

    #[test]
    fn extracts_credits_user_jwt_from_internal_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-apiosk-user-jwt",
            HeaderValue::from_static("eyJhbGciOiJIUzI1NiJ9.test.token"),
        );
        let token = extract_credits_user_jwt(&headers).expect("jwt");
        assert_eq!(token, "eyJhbGciOiJIUzI1NiJ9.test.token");
    }

    #[test]
    fn strips_credits_user_jwt_from_forwarded_requests() {
        let value = HeaderValue::from_static("eyJhbGciOiJIUzI1NiJ9.test.token");
        assert!(should_strip_forwarded_header("x-apiosk-user-jwt", &value));
    }

    #[test]
    fn strips_transfer_encoding_from_forwarded_requests() {
        let value = HeaderValue::from_static("chunked");
        assert!(should_strip_forwarded_header(
            header::TRANSFER_ENCODING.as_str(),
            &value
        ));
    }
}
