/// Service handler: relay automation/webhook calls to a configured upstream.
/// Validates optional trigger schema, injects auth, relays and returns response.
use axum::{body::Body, http::Request};
use tracing::{error, info};

use crate::{
    auth::should_strip_forwarded_header,
    listing::{DispatchResult, ServiceListing},
};

pub async fn handle(
    http_client: &reqwest::Client,
    service: &ServiceListing,
    request: Request<Body>,
) -> DispatchResult {
    info!("Service handler: relaying to {}", service.webhook_url);

    let headers = request.headers().clone();
    let uri = request.uri().clone();

    let target = format!(
        "{}{}",
        service.webhook_url.trim_end_matches('/'),
        uri.query().map(|q| format!("?{}", q)).unwrap_or_default()
    );

    let body_bytes = match axum::body::to_bytes(request.into_body(), usize::MAX).await {
        Ok(b) => b,
        Err(_) => return DispatchResult::error(400, "bad_request", "Could not read body"),
    };

    let reqwest_method = match service.relay_method.to_uppercase().as_str() {
        "GET" => reqwest::Method::GET,
        "PUT" => reqwest::Method::PUT,
        "PATCH" => reqwest::Method::PATCH,
        "DELETE" => reqwest::Method::DELETE,
        _ => reqwest::Method::POST,
    };

    let mut req_builder = http_client
        .request(reqwest_method, &target)
        .timeout(std::time::Duration::from_millis(service.timeout_ms as u64))
        .body(body_bytes.to_vec());

    // Forward original headers (strip payment + host)
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

    // Inject auth
    match service.auth_type.as_str() {
        "bearer" => {
            if let Some(secret) = &service.auth_secret {
                req_builder = req_builder.bearer_auth(secret);
            }
        }
        "header" => {
            // format: "Header-Name: value"
            if let Some(secret) = &service.auth_secret {
                if let Some((name, val)) = secret.split_once(": ") {
                    if let (Ok(hk), Ok(hv)) = (
                        reqwest::header::HeaderName::from_bytes(name.as_bytes()),
                        reqwest::header::HeaderValue::from_str(val),
                    ) {
                        req_builder = req_builder.header(hk, hv);
                    }
                }
            }
        }
        "basic" => {
            // format: "username:password"
            if let Some(secret) = &service.auth_secret {
                if let Some((user, pass)) = secret.split_once(':') {
                    req_builder = req_builder.basic_auth(user, Some(pass));
                }
            }
        }
        _ => {}
    }

    let response = match req_builder.send().await {
        Ok(r) => r,
        Err(e) => {
            error!("Service relay failed: {}", e);
            return DispatchResult::error(502, "bad_gateway", "Service webhook unreachable");
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
            error!("Failed to read service response: {}", e);
            return DispatchResult::error(502, "bad_gateway", "Failed to read service response");
        }
    };

    DispatchResult {
        status,
        content_type,
        body,
        extra_headers: vec![("X-Apiosk-Type".to_string(), "service".to_string())],
    }
}
