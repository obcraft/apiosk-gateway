/// Dataset handler: deliver structured data after x402 payment.
/// Supports bulk download (redirect to signed URL) and query relay (proxy to upstream).
use axum::{body::Body, http::Request};
use tracing::{error, info};

use crate::{
    auth::should_strip_forwarded_header,
    listing::{DatasetListing, DispatchResult},
};

pub async fn handle(
    http_client: &reqwest::Client,
    dataset: &DatasetListing,
    request: Request<Body>,
    path: &str,
) -> DispatchResult {
    info!(
        "Dataset handler: type={} api={}",
        dataset.delivery_type, dataset.api_id
    );

    match dataset.delivery_type.as_str() {
        "bulk" => handle_bulk(http_client, dataset).await,
        "stream" | "query" => handle_query(http_client, dataset, request, path).await,
        _ => DispatchResult::error(500, "config_error", "Unknown dataset delivery type"),
    }
}

/// Bulk: return a signed download URL (redirect or JSON with url)
async fn handle_bulk(http_client: &reqwest::Client, dataset: &DatasetListing) -> DispatchResult {
    let url = match &dataset.bulk_url {
        Some(u) => u.clone(),
        None => return DispatchResult::error(503, "unavailable", "Bulk URL not configured"),
    };

    // Proxy the actual file (or redirect — here we proxy so caller doesn't need to re-auth)
    let response = match http_client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            error!("Dataset bulk fetch failed: {}", e);
            return DispatchResult::error(502, "bad_gateway", "Could not retrieve dataset");
        }
    };

    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    let body = match response.bytes().await {
        Ok(b) => b.to_vec(),
        Err(e) => {
            error!("Failed to read dataset response: {}", e);
            return DispatchResult::error(502, "stream_error", "Failed to read dataset");
        }
    };

    let format = dataset.data_format.as_str();
    let disposition = match format {
        "csv" => "attachment; filename=\"dataset.csv\"",
        "jsonl" => "attachment; filename=\"dataset.jsonl\"",
        "parquet" => "attachment; filename=\"dataset.parquet\"",
        _ => "attachment; filename=\"dataset\"",
    };

    DispatchResult {
        status,
        content_type,
        body,
        extra_headers: vec![
            ("Content-Disposition".to_string(), disposition.to_string()),
            ("X-Apiosk-Type".to_string(), "dataset".to_string()),
            ("X-Dataset-Format".to_string(), format.to_string()),
            (
                "X-Record-Count".to_string(),
                dataset.record_count.unwrap_or(0).to_string(),
            ),
        ],
    }
}

/// Query/stream: proxy the request to the upstream query endpoint
async fn handle_query(
    http_client: &reqwest::Client,
    dataset: &DatasetListing,
    request: Request<Body>,
    path: &str,
) -> DispatchResult {
    let base = match &dataset.query_endpoint {
        Some(u) => u.clone(),
        None => return DispatchResult::error(503, "unavailable", "Query endpoint not configured"),
    };

    let method = request.method().clone();
    let headers = request.headers().clone();
    let uri = request.uri().clone();

    let target = format!(
        "{}{}{}",
        base.trim_end_matches('/'),
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
        _ => reqwest::Method::GET,
    };

    let mut req_builder = http_client
        .request(reqwest_method, &target)
        .body(body_bytes.to_vec());

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

    let response = match req_builder.send().await {
        Ok(r) => r,
        Err(e) => {
            error!("Dataset query relay failed: {}", e);
            return DispatchResult::error(502, "bad_gateway", "Dataset endpoint unreachable");
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
            error!("Failed to read dataset query response: {}", e);
            return DispatchResult::error(502, "bad_gateway", "Failed to read response");
        }
    };

    DispatchResult {
        status,
        content_type,
        body,
        extra_headers: vec![("X-Apiosk-Type".to_string(), "dataset".to_string())],
    }
}
