//! The Apiosk Slack app's own side: its signed interaction requests, their
//! `response_url`, and escaping. Posting goes through each workspace's Apiosk
//! bot connection in Composio.
use anyhow::{anyhow, ensure, Result};
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use std::{sync::OnceLock, time::Duration};

const DEADLINE: Duration = Duration::from_secs(10);

pub(super) fn signing_secret() -> Option<String> {
    std::env::var("SLACK_SIGNING_SECRET").ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// Answers an interaction through its `response_url`, which Slack only issues
/// on hooks.slack.com and which needs no token.
pub(super) async fn respond(url: &str, body: &Value) -> Result<()> {
    static HTTP: OnceLock<reqwest::Client> = OnceLock::new();
    ensure!(url.starts_with("https://hooks.slack.com/") && url.len() <= 500, "slack_invalid_response_url");
    let response = HTTP.get_or_init(reqwest::Client::new).post(url).timeout(DEADLINE).json(body).send().await
        .map_err(|_| anyhow!("slack_transport_failure"))?;
    ensure!(response.status().is_success(), "slack_http_status");
    Ok(())
}

/// Slack parses `<…>` in mrkdwn and fallback text as mentions and links, so
/// text it did not write itself is escaped: only these three need it.
pub(super) fn esc(s: &str) -> String {
    s.trim().replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Slack request signing: `v0=` and the hex HMAC-SHA256 of
/// `v0:{timestamp}:{body}`, accepted for five minutes.
pub(super) fn verify(secret: &str, timestamp: &str, signature: &str, body: &[u8], now: i64) -> bool {
    if timestamp.parse::<i64>().ok().is_none_or(|t| now.abs_diff(t) > 300) {
        return false;
    }
    let Some(signature) = signature.strip_prefix("v0=").and_then(|s| hex::decode(s).ok()) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(format!("v0:{timestamp}:").as_bytes());
    mac.update(body);
    mac.verify_slice(&signature).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_accepts_only_a_fresh_matching_signature() {
        let sign = |body: &[u8], ts: &str| {
            let mut mac = Hmac::<Sha256>::new_from_slice(b"secret").unwrap();
            mac.update(format!("v0:{ts}:").as_bytes());
            mac.update(body);
            format!("v0={}", hex::encode(mac.finalize().into_bytes()))
        };
        let signature = sign(b"payload=x", "1000");
        assert!(verify("secret", "1000", &signature, b"payload=x", 1100));
        assert!(!verify("secret", "1000", &signature, b"payload=y", 1100));
        assert!(!verify("other", "1000", &signature, b"payload=x", 1100));
        assert!(!verify("secret", "1000", &signature, b"payload=x", 1301));
        assert!(!verify("secret", "1000", "v1=00", b"payload=x", 1100));
    }
}
