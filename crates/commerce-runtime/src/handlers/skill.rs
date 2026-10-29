/// Skill handler: relay AI tool calls to the skill's endpoint
/// Injects auth headers as configured, passes body through unchanged.
/// Useful for MCP endpoints, LLM tools, agent pipelines.
use axum::{body::Body, http::Request};
use tracing::{error, info};

use crate::{
    auth::should_strip_forwarded_header,
    listing::{DispatchResult, SkillListing},
};

pub async fn handle(
    http_client: &reqwest::Client,
    skill: &SkillListing,
    request: Request<Body>,
    path: &str,
) -> DispatchResult {
    info!("Skill handler: relaying to {}{}", skill.endpoint_url, path);

    let method = request.method().clone();
    let headers = request.headers().clone();
    let uri = request.uri().clone();

    // Build target URL
    let target = format!(
        "{}{}{}",
        skill.endpoint_url.trim_end_matches('/'),
        path,
        uri.query().map(|q| format!("?{}", q)).unwrap_or_default()
    );

    // Read body
    let body_bytes = match axum::body::to_bytes(request.into_body(), usize::MAX).await {
        Ok(b) => b,
        Err(e) => {
            error!("Failed to read skill request body: {}", e);
            return DispatchResult::error(400, "bad_request", "Could not read request body");
        }
    };

    let reqwest_method = match method.as_str() {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        "PUT" => reqwest::Method::PUT,
        "DELETE" => reqwest::Method::DELETE,
        "PATCH" => reqwest::Method::PATCH,
        "OPTIONS" => reqwest::Method::OPTIONS,
        _ => reqwest::Method::POST,
    };

    let mut req_builder = http_client
        .request(reqwest_method, &target)
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

    // Inject skill auth
    match skill.auth_type.as_str() {
        "bearer" => {
            if let Some(secret) = &skill.auth_secret {
                req_builder = req_builder.bearer_auth(secret);
            }
        }
        "header" => {
            // auth_secret format: "Header-Name: value"
            if let Some(secret) = &skill.auth_secret {
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
        "query" => {
            // auth_secret format: "param_name=value"
            if let Some(secret) = &skill.auth_secret {
                let sep = if target.contains('?') { '&' } else { '?' };
                let target_with_auth = format!("{}{}{}", target, sep, secret);
                req_builder = http_client
                    .request(reqwest::Method::POST, &target_with_auth)
                    .body(body_bytes.to_vec());
            }
        }
        _ => {} // "none"
    }

    // Execute
    let response = match req_builder.send().await {
        Ok(r) => r,
        Err(e) => {
            error!("Skill relay failed: {}", e);
            return DispatchResult::error(502, "bad_gateway", "Skill endpoint unreachable");
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
            error!("Failed to read skill response: {}", e);
            return DispatchResult::error(502, "bad_gateway", "Failed to read skill response");
        }
    };

    DispatchResult {
        status,
        content_type,
        body,
        extra_headers: vec![("X-Apiosk-Type".to_string(), "skill".to_string())],
    }
}
