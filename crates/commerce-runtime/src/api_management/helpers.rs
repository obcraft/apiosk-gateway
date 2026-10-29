use axum::Json;
use bigdecimal::BigDecimal;
use tracing::info;

use super::models::RegisterApiResponse;

pub(crate) fn default_category() -> String {
    "data".to_string()
}

pub(crate) fn default_listing_metadata() -> serde_json::Value {
    serde_json::json!({})
}

pub(crate) fn registration_status(verified: bool) -> &'static str {
    if verified {
        "active"
    } else {
        "inactive"
    }
}

pub(crate) fn registration_message(health_check_passed: bool) -> String {
    if health_check_passed {
        "API registered and health-checked. It is awaiting Apiosk verification before traffic is routed.".to_string()
    } else {
        "API registered but health check failed, so it was saved as inactive. Fix the endpoint and update it to go live, then request verification.".to_string()
    }
}

pub(crate) fn reject_register(slug: &str, message: impl Into<String>) -> Json<RegisterApiResponse> {
    Json(RegisterApiResponse {
        success: false,
        api_id: String::new(),
        slug: slug.to_string(),
        gateway_url: String::new(),
        status: "rejected".to_string(),
        verified: false,
        health_check_passed: false,
        message: message.into(),
    })
}

pub(crate) fn validate_price(price: f64) -> bool {
    (0.0001..=10.00).contains(&price)
}

/// Validate that an upstream endpoint URL is safe to use as a proxy target.
/// Blocks SSRF surfaces (loopback, link-local, RFC1918, Fly internal,
/// metadata services) and unsupported schemes; HTTPS only.
pub(crate) fn validate_upstream_url(url: &str) -> Result<(), &'static str> {
    let parsed = match url::Url::parse(url.trim()) {
        Ok(u) => u,
        Err(_) => return Err("Endpoint URL is not a valid URL"),
    };
    if parsed.scheme() != "https" {
        return Err("Endpoint URL must use HTTPS");
    }
    let host = match parsed.host_str() {
        Some(h) => h.to_ascii_lowercase(),
        None => return Err("Endpoint URL must include a host"),
    };
    // `Url::host_str()` returns IPv6 literals wrapped in brackets (e.g. "[::1]").
    // Strip them so a bracketed IPv6 loopback/private address can't slip past
    // both the exact-host blocklist (which lists "::1" unbracketed) and the
    // `IpAddr` parse below (which rejects the bracketed form). Hostnames are
    // unaffected since they never carry brackets.
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .map(str::to_string)
        .unwrap_or(host);
    // Block obvious SSRF targets. We do not resolve DNS here on purpose: the
    // request-time path also re-validates, so a host that resolves to a
    // private IP can be caught even if the registration check passes.
    let blocked_hosts = [
        "localhost",
        "127.0.0.1",
        "0.0.0.0",
        "::1",
        "169.254.169.254", // cloud metadata
    ];
    if blocked_hosts.contains(&host.as_str()) {
        return Err("Endpoint host is not allowed (loopback or metadata)");
    }
    if host.ends_with(".internal") || host.ends_with(".local") || host.ends_with(".localhost") {
        return Err("Endpoint host suffix is reserved for internal use");
    }
    // Numeric IP: block RFC1918 ranges.
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if is_private_ip(&ip) {
            return Err("Endpoint host points at a private IP range");
        }
    }
    if url.len() > 1024 {
        return Err("Endpoint URL is too long (max 1024 chars)");
    }
    Ok(())
}

fn is_private_ip(ip: &std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || v4.is_documentation()
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                // RFC4193 unique-local fc00::/7
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // link-local fe80::/10
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

pub(crate) fn to_bigdecimal(price: f64) -> BigDecimal {
    BigDecimal::try_from(price).unwrap_or_else(|_| BigDecimal::from(0))
}

pub(crate) async fn perform_health_check(
    client: &reqwest::Client,
    url: &str,
) -> anyhow::Result<bool> {
    let head_result = client
        .head(url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await;

    if let Ok(resp) = head_result {
        if resp.status().is_success() {
            return Ok(true);
        }
    }

    let get_result = client
        .get(url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await;

    match get_result {
        Ok(resp) if resp.status().is_success() => Ok(true),
        Ok(resp) => {
            info!(
                "Health check returned non-success status: {}",
                resp.status()
            );
            Ok(false)
        }
        Err(err) => {
            info!("Health check request failed: {}", err);
            Ok(false)
        }
    }
}
