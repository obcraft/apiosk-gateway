/// Connector handler: relay calls to third-party platform integrations.
/// Similar to service but platform-aware — includes platform name in response headers
/// and can inject platform-specific auth patterns.
use axum::{body::Body, http::Request};
use tracing::{error, info};

use crate::{
    auth::should_strip_forwarded_header,
    listing::{ConnectorListing, DispatchResult},
};

pub async fn handle(
    http_client: &reqwest::Client,
    connector: &ConnectorListing,
    request: Request<Body>,
    path: &str,
) -> DispatchResult {
    info!(
        "Connector handler: platform={} relay={}{}",
        connector.platform, connector.relay_url, path
    );

    let headers = request.headers().clone();
    let uri = request.uri().clone();
    let method = request.method().clone();

    let target = format!(
        "{}{}{}",
        connector.relay_url.trim_end_matches('/'),
        path,
        uri.query().map(|q| format!("?{}", q)).unwrap_or_default()
    );

    let body_bytes = match axum::body::to_bytes(request.into_body(), usize::MAX).await {
        Ok(b) => b,
        Err(_) => return DispatchResult::error(400, "bad_request", "Could not read body"),
    };

    let reqwest_method = match method.as_str() {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        "PUT" => reqwest::Method::PUT,
        "PATCH" => reqwest::Method::PATCH,
        "DELETE" => reqwest::Method::DELETE,
        _ => reqwest::Method::POST,
    };

    let mut req_builder = http_client
        .request(reqwest_method, &target)
        .body(body_bytes.to_vec());

    // Forward original headers
    for (k, v) in headers.iter() {
        let kl = k.as_str().to_ascii_lowercase();
        if should_strip_forwarded_header(&kl, v) {
            continue;
        }
        if let (Ok(hk), Ok(hv)) = (
            reqwest::header::HeaderName::from_bytes(k.as_str().as_bytes()),
            reqwest::header::HeaderValue::from_bytes(v.as_bytes()),
        ) {
            req_builder = req_builder.header(hk, hv);
        }
    }

    // Platform-specific auth injection
    req_builder = inject_platform_auth(req_builder, connector);

    let response = match req_builder.send().await {
        Ok(r) => r,
        Err(e) => {
            error!("Connector relay failed: {}", e);
            return DispatchResult::error(
                502,
                "bad_gateway",
                &format!("{} connector unreachable", connector.platform),
            );
        }
    };

    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();

    let body = match response.bytes().await {
        Ok(b) => b.to_vec(),
        Err(e) => {
            error!("Failed to read connector response: {}", e);
            return DispatchResult::error(502, "bad_gateway", "Failed to read connector response");
        }
    };

    DispatchResult {
        status,
        content_type,
        body,
        extra_headers: vec![
            ("X-Apiosk-Type".to_string(), "connector".to_string()),
            ("X-Apiosk-Platform".to_string(), connector.platform.clone()),
        ],
    }
}

/// Inject platform-specific auth headers based on connector config
fn inject_platform_auth(
    builder: reqwest::RequestBuilder,
    connector: &ConnectorListing,
) -> reqwest::RequestBuilder {
    match connector.auth_type.as_str() {
        "apikey" => {
            if let Some((header_name, header_value)) = connector_auth_header(connector) {
                return builder.header(header_name, header_value);
            }
            builder
        }
        "oauth2" => {
            if let Some(secret) = connector
                .auth_secret
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                return builder.bearer_auth(strip_bearer_prefix(secret));
            }
            builder
        }
        "webhook" => {
            // Webhook connectors typically don't need auth injection
            builder
        }
        _ => builder,
    }
}

fn connector_auth_header(connector: &ConnectorListing) -> Option<(String, String)> {
    let secret = connector
        .auth_secret
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;

    let header_name = platform_auth_header(&connector.platform);
    let header_value = if header_name.eq_ignore_ascii_case("Authorization") {
        if secret.contains(' ') {
            secret.to_string()
        } else {
            format!("Bearer {}", secret)
        }
    } else {
        secret.to_string()
    };

    Some((header_name.to_string(), header_value))
}

fn strip_bearer_prefix(value: &str) -> &str {
    value
        .trim()
        .strip_prefix("Bearer ")
        .or_else(|| value.trim().strip_prefix("bearer "))
        .unwrap_or(value.trim())
}

/// Returns the platform-specific API key header name
fn platform_auth_header(platform: &str) -> &'static str {
    match platform {
        "slack" => "Authorization",    // Bearer xoxb-...
        "notion" => "Authorization",   // Bearer secret_...
        "airtable" => "Authorization", // Bearer key...
        "discord" => "Authorization",  // Bot TOKEN
        "github" => "Authorization",   // Bearer ghp_...
        "mollie" => "Authorization",   // Bearer live_xxx
        "stripe" => "Authorization",   // Bearer sk_...
        "shopify" => "X-Shopify-Access-Token",
        "hubspot" => "Authorization",
        _ => "Authorization",
    }
}

#[cfg(test)]
mod tests {
    use super::{connector_auth_header, platform_auth_header, strip_bearer_prefix};
    use crate::listing::ConnectorListing;
    use serde_json::json;
    use uuid::Uuid;

    fn test_connector(
        platform: &str,
        auth_type: &str,
        auth_secret: Option<&str>,
    ) -> ConnectorListing {
        ConnectorListing {
            id: Uuid::nil(),
            api_id: Uuid::nil(),
            platform: platform.to_string(),
            auth_type: auth_type.to_string(),
            relay_url: "https://example.com".to_string(),
            supported_actions: json!([]),
            auth_secret: auth_secret.map(str::to_string),
        }
    }

    #[test]
    fn adds_bearer_prefix_for_authorization_headers() {
        let connector = test_connector("mollie", "apikey", Some("live_test_123"));
        let (header_name, header_value) = connector_auth_header(&connector).expect("header");
        assert_eq!(header_name, "Authorization");
        assert_eq!(header_value, "Bearer live_test_123");
    }

    #[test]
    fn preserves_existing_authorization_scheme() {
        let connector = test_connector("discord", "apikey", Some("Bot abc123"));
        let (_, header_value) = connector_auth_header(&connector).expect("header");
        assert_eq!(header_value, "Bot abc123");
    }

    #[test]
    fn strips_bearer_prefix_for_oauth_tokens() {
        assert_eq!(strip_bearer_prefix("Bearer secret_token"), "secret_token");
        assert_eq!(strip_bearer_prefix("secret_token"), "secret_token");
    }

    #[test]
    fn mollie_uses_authorization_header() {
        assert_eq!(platform_auth_header("mollie"), "Authorization");
    }
}
