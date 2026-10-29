use super::*;
use crate::db::CreditChargeError;

/// Default cap on a buffered upstream response body (S8). Prevents a malicious or
/// misbehaving provider from OOM-ing the gateway (256 MB VM) with a huge
/// response. Tunable via `GATEWAY_MAX_UPSTREAM_BODY_BYTES`.
const DEFAULT_MAX_UPSTREAM_BODY_BYTES: usize = 32 * 1024 * 1024;

fn max_upstream_body_bytes() -> usize {
    std::env::var("GATEWAY_MAX_UPSTREAM_BODY_BYTES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_MAX_UPSTREAM_BODY_BYTES)
}

enum UpstreamBodyError {
    /// Body exceeded the size cap (via Content-Length or while streaming chunks).
    TooLarge,
    /// Transport error while reading the body.
    Read,
}

/// Read an upstream response body into memory with a hard size cap (S8). Reads
/// incrementally so an oversized chunked / no-Content-Length response is aborted
/// before it can exhaust memory, instead of buffering the whole thing.
/// How the upstream leg can end other than with an answer.
enum ShortCircuit {
    /// A response to hand the client as the result of the call (the resell
    /// leg's own re-issue).
    Serve(Response<Body>),
    /// A gateway-side failure: unreachable upstream, unreadable body.
    Fail(Response<Body>),
}

/// The network half of a forward: unwrap the attempted response, run the
/// resell leg if the upstream answered with its own 402, and read the body
/// under the size cap.
///
/// Split out of `forward_api_request` so a cached answer can take its place
/// without duplicating the settlement, logging and response-building half
/// that follows. Everything here runs only on a cache MISS.
#[allow(clippy::too_many_arguments)]
async fn forward_upstream(
    state: &Arc<AppState>,
    requested_path: &str,
    method: &Method,
    headers: &HeaderMap<HeaderValue>,
    browser_mode: bool,
    tracking_context: &TrackingContext,
    resolved: &ResolvedProxyRequest,
    payment_decision: &EndpointPaymentDecision,
    origin_url: &str,
    body_bytes: &axum::body::Bytes,
    idempotency_key: Option<&str>,
    hmac_header: Option<&(String, String)>,
    response: Option<reqwest::Response>,
    last_error: Option<reqwest::Error>,
    resell_outcome: &mut Option<super::resell::ResellOutcome>,
    public_source: Option<&super::public_sources::Prepared>,
) -> Result<(StatusCode, HeaderMap, axum::body::Bytes), ShortCircuit> {
    let mut response = match response {
        Some(resp) => resp,
        None => {
            let failure_reason = last_error
                .map(|err| err.to_string())
                .unwrap_or_else(|| "unknown upstream error".to_string());
            if browser_mode {
                return Err(ShortCircuit::Fail(branded_status_page(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Provider temporarily unavailable",
                    "The gateway could not reach the provider after multiple attempts. Retry in a few seconds.",
                    &resolved.api.slug,
                    requested_path,
                    method,
                    &[],
                    &[
                        ("Retry-After", "5 seconds".to_string()),
                        ("Reason", failure_reason),
                    ],
                    None,
                )));
            }
            let body = serde_json::json!({
                "error": "Service temporarily unavailable",
                "message": "The gateway could not reach this API after multiple attempts. Please retry in a few seconds.",
                "retry_after": 5
            });
            return Err(ShortCircuit::Fail(
                axum::response::Response::builder()
                    .status(StatusCode::SERVICE_UNAVAILABLE)
                    .header("Content-Type", "application/json")
                    .header("Retry-After", "5")
                    .body(axum::body::Body::from(body.to_string()))
                    .unwrap(),
            ));
        }
    };

    // ── Resell leg ──────────────────────────────────────────────────────────
    // If this is a resell listing and the upstream answered with its OWN 402,
    // pay it once from the platform payer wallet and re-issue, so the caller gets
    // data. Only when the agent already holds an authorized (deferred) payment
    // to settle against on success (the existing settle at ~683 charges the
    // agent only if the paid retry returns 2xx). Fires exactly once — no retry.
    // Set when this call was served by paying the upstream's own x402. Settlement
    // reads it to reimburse the bridge wallet instead of paying the provider
    // again — see `ResellOutcome`.
    if response.status() == StatusCode::PAYMENT_REQUIRED
        && payment_decision.deferred_payment.is_some()
    {
        // A request_id is required to key the durable double-pay claim; without
        // one we cannot guarantee no double-spend, so skip resell (fail safe —
        // the upstream 402 passes through and the agent is not charged).
        if let (Some(listing_price_atomic), Some(request_id)) =
            (resolved.resell, tracking_context.request_id)
        {
            match settle_upstream_resell(
                state,
                origin_url,
                resolved,
                headers,
                body_bytes,
                idempotency_key,
                hmac_header,
                request_id,
                listing_price_atomic,
                response,
            )
            .await
            {
                Ok((paid, outcome)) => {
                    response = paid;
                    *resell_outcome = Some(outcome);
                }
                Err(client_response) => return Err(ShortCircuit::Serve(client_response)),
            }
        }
    }

    if let Some(source) = public_source {
        response =
            super::public_sources::follow_metadata_redirect(source, &state.http_client, response)
                .await
                .map_err(|message| {
                    ShortCircuit::Fail(json_error_response(
                        StatusCode::BAD_GATEWAY,
                        "source_redirect_failed",
                        &message,
                    ))
                })?;
    }
    let status = response.status();
    let mut response_headers = response.headers().clone();
    let body_cap = max_upstream_body_bytes();
    let response_body = match read_upstream_body_capped(response, body_cap).await {
        Ok(body) => body,
        Err(err) => {
            let (reason, message) = match err {
                UpstreamBodyError::TooLarge => (
                    "upstream_body_too_large",
                    "Upstream response exceeded the gateway size limit",
                ),
                UpstreamBodyError::Read => (
                    "upstream_body_read_failed",
                    "Failed to read upstream response body",
                ),
            };
            error!("{} for {} ({} byte cap)", message, &origin_url, body_cap);
            return Err(ShortCircuit::Fail(if browser_mode {
                branded_status_page(
                    StatusCode::BAD_GATEWAY,
                    "Upstream response error",
                    "The provider responded, but the gateway could not read the response body.",
                    &resolved.api.slug,
                    requested_path,
                    method,
                    &[],
                    &[("Reason", reason.to_string())],
                    None,
                )
            } else {
                json_error_response(StatusCode::BAD_GATEWAY, "bad_gateway", message)
            }));
        }
    };
    let response_body = if status.is_success() {
        if let Some(source) = public_source {
            let normalized = super::public_sources::normalize_bytes(source, &response_body)
                .map_err(|message| {
                    ShortCircuit::Fail(json_error_response(
                        StatusCode::BAD_GATEWAY,
                        "source_schema_mismatch",
                        &message,
                    ))
                })?;
            response_headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            response_headers.remove(header::CONTENT_LENGTH);
            response_headers.remove(header::CONTENT_ENCODING);
            axum::body::Bytes::from(normalized.to_string())
        } else {
            response_body
        }
    } else {
        response_body
    };
    Ok((status, response_headers, response_body))
}

async fn read_upstream_body_capped(
    mut resp: reqwest::Response,
    cap: usize,
) -> Result<axum::body::Bytes, UpstreamBodyError> {
    if let Some(len) = resp.content_length() {
        if len as usize > cap {
            return Err(UpstreamBodyError::TooLarge);
        }
    }
    let mut buf: Vec<u8> = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                if buf.len().saturating_add(chunk.len()) > cap {
                    return Err(UpstreamBodyError::TooLarge);
                }
                buf.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(_) => return Err(UpstreamBodyError::Read),
        }
    }
    Ok(axum::body::Bytes::from(buf))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn forward_api_request(
    state: &Arc<AppState>,
    api_slug: &str,
    requested_path: &str,
    method: &Method,
    headers: &HeaderMap<HeaderValue>,
    uri: &Uri,
    browser_mode: bool,
    tracking_context: &TrackingContext,
    request: &mut Option<Request<Body>>,
    resolved: &ResolvedProxyRequest,
    payment_decision: EndpointPaymentDecision,
) -> Result<Response<Body>, Response<Body>> {
    let connection_tracking_context = match payment_decision.agent_connect_token_id {
        Some(token_id) => tracking_context.with_connect_token_id(token_id),
        None => tracking_context.clone(),
    };
    let tracking_context = &connection_tracking_context;
    // A per-endpoint origin override wins over the listing's base URL: one
    // listing can span upstream hosts (KVK: api.kvk.nl + opendata.kvk.nl)
    // while staying a single source everywhere it is surfaced.
    let base_url = resolved
        .endpoint
        .as_ref()
        .and_then(|e| e.origin_override.as_deref())
        .unwrap_or_else(|| {
            resolved
                .api
                .endpoint_url
                .as_deref()
                .unwrap_or(&resolved.api.origin_url)
        });
    let origin_url = join_origin_url(base_url, &resolved.resolved_path);
    let origin_url = append_forward_query(
        &origin_url,
        uri.query(),
        resolved
            .virtual_execute_request
            .as_ref()
            .and_then(|execution| execution.upstream_query.as_deref()),
    );
    // Inject provider-configured upstream auth query params
    // (listing_metadata.auth_query), e.g. ?token=<key>. Header-based credentials
    // are applied later in build_forwarded_request via preset_auth_headers.
    let origin_url = if resolved.preset_auth_query.is_empty() {
        origin_url
    } else {
        let auth_query = resolved
            .preset_auth_query
            .iter()
            .map(|(name, value)| {
                format!(
                    "{}={}",
                    url::form_urlencoded::byte_serialize(name.as_bytes()).collect::<String>(),
                    url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>(),
                )
            })
            .collect::<Vec<_>>()
            .join("&");
        if origin_url.contains('?') {
            format!("{origin_url}&{auth_query}")
        } else {
            format!("{origin_url}?{auth_query}")
        }
    };

    let overheid_query = super::overheid::prepare(
        &resolved.api.slug,
        resolved.resolved_method.as_str(),
        &origin_url,
    )
    .map_err(|message| {
        json_error_response(StatusCode::BAD_REQUEST, "invalid_source_query", &message)
    })?;
    let origin_url = overheid_query.clone().unwrap_or(origin_url);
    let mut public_source = super::public_sources::prepare(
        &resolved.api.slug,
        resolved.resolved_method.as_str(),
        &origin_url,
    )
    .map_err(|message| {
        json_error_response(StatusCode::BAD_REQUEST, "invalid_source_query", &message)
    })?;
    let source_cache_query = origin_url.clone();
    let origin_url = public_source
        .as_ref()
        .map(|s| s.url.clone())
        .unwrap_or(origin_url);

    let body_bytes = if let Some(execution) = resolved.virtual_execute_request.as_ref() {
        execution.upstream_body_bytes.clone().into()
    } else {
        axum::body::to_bytes(
            take_request_or_error(
                request,
                &resolved.api.slug,
                requested_path,
                method,
                browser_mode,
            )?
            .into_body(),
            MAX_PROXY_BODY_BYTES,
        )
        .await
        .map_err(|e| {
            // axum::body::to_bytes wraps an oversize body in a length-limit
            // error; surface that as 413 so the caller can shrink and retry
            // instead of seeing a generic 400. The exact downstream type
            // varies between axum / http-body-util versions, so we match on
            // the rendered message rather than a concrete type.
            let oversize = e.to_string().to_ascii_lowercase().contains("length");
            error!("Failed to read request body (oversize={}): {}", oversize, e);
            let (code, label, message) = if oversize {
                (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "payload_too_large",
                    "Request body exceeds the gateway maximum (8 MiB).",
                )
            } else {
                (
                    StatusCode::BAD_REQUEST,
                    "bad_request",
                    "Failed to read request body",
                )
            };
            if browser_mode {
                branded_status_page(
                    code,
                    "Invalid request body",
                    message,
                    &resolved.api.slug,
                    requested_path,
                    method,
                    &[],
                    &[("Reason", label.to_string())],
                    None,
                )
            } else {
                json_error_response(code, label, message)
            }
        })?
    };

    let retry_delays = [
        Duration::from_millis(0),
        Duration::from_millis(350),
        Duration::from_millis(1_250),
    ];
    let request_started_at = Instant::now();
    let mut response = None;
    let mut last_error = None;

    // Stable per-call key so upstreams (Mollie, Sendcloud, etc.) can dedupe
    // any retry the gateway performs after a transport error.
    let idempotency_key = tracking_context
        .request_id
        .map(|id| id.simple().to_string());

    // Resolve the optional outbound-HMAC signature once, before the retry
    // loop, so retries reuse the same canonical signature instead of
    // re-decrypting the secret per attempt. When the API has not opted in
    // (no encrypted secret configured), this stays None.
    let mut hmac_header: Option<(String, String)> = None;
    if let (Some(algo), Some(header_name), Some(secret_enc)) = (
        resolved.api.hmac_signing_algorithm.as_deref(),
        resolved.api.hmac_signing_header.as_deref(),
        resolved.api.hmac_signing_secret_enc.as_deref(),
    ) {
        if algo == "sha256" && !header_name.is_empty() && !secret_enc.is_empty() {
            match state.db.decrypt_provider_secret(secret_enc).await {
                Ok(Some(secret)) => {
                    let canonical_path = resolved.resolved_path.as_str();
                    let sig = compute_hmac_sha256_signature(
                        &resolved.resolved_method,
                        canonical_path,
                        &body_bytes,
                        &secret,
                    );
                    hmac_header = Some((header_name.to_string(), sig));
                }
                Ok(None) => {
                    warn!(
                        "HMAC secret for {} decrypted to NULL; signing skipped",
                        resolved.api.slug
                    );
                }
                Err(e) => {
                    error!(
                        "Failed to decrypt HMAC secret for {} ({}); proxying without signature",
                        resolved.api.slug, e
                    );
                }
            }
        }
    }
    // ── Upstream response cache ─────────────────────────────────────────────
    // Opt-in per endpoint (`api_endpoints.cache_ttl_seconds`) and off
    // everywhere else. It exists for upstreams whose rate limit sits far
    // below Apiosk's traffic — KVK's open datasets allow one request per
    // minute per IP, and the gateway is one IP — where without it the second
    // buyer of the same lookup gets a 429 the first one caused.
    //
    // It changes what the gateway ASKS UPSTREAM, never what it charges: the
    // payment decision above already ran, and settlement below runs
    // identically on a hit. The buyer pays for the answer, not the round trip.
    // Safe methods only, and only answers the upstream returned as 200.
    let cache_ttl = resolved
        .endpoint
        .as_ref()
        .and_then(|ep| ep.cache_ttl_seconds)
        .filter(|seconds| *seconds > 0)
        .map(|seconds| std::time::Duration::from_secs(seconds as u64));
    let cache_key = match (&cache_ttl, resolved.endpoint.as_ref()) {
        (Some(_), Some(ep)) if resolved.resolved_method == Method::GET => {
            Some(crate::cache::upstream_cache_key(
                &ep.id,
                resolved.resolved_method.as_str(),
                &resolved.resolved_path,
                if public_source.is_some() || overheid_query.is_some() {
                    &source_cache_query
                } else {
                    uri.query().unwrap_or("")
                },
            ))
        }
        _ => None,
    };
    let annual_accounts = resolved.api.slug == kvk_accounts::PROVIDER
        && resolved
            .resolved_path
            .starts_with("/api/v1/hvds/jaarrekeningen/");
    let cached = if nl_benefits::handles(&resolved.api.slug) {
        let data = nl_benefits::answer(
            &resolved.api.slug, resolved.resolved_method.as_str(),
            &resolved.resolved_path, &body_bytes,
        ).await.map_err(|error| json_error_response(
            StatusCode::from_u16(error.status).unwrap_or(StatusCode::BAD_GATEWAY),
            error.code, &error.message,
        ))?;
        // First-party result, following the existing IBAN adapter: preflight
        // already ran; normal settlement and saved-result handling stay below.
        // POST responses never enter the shared upstream cache.
        Some(Arc::new(crate::cache::CachedUpstreamResponse::new(
            200, Some("application/json".into()), data.to_string().into_bytes(), Duration::from_secs(1),
        )))
    } else if resolved.api.slug == super::apimarket::SOURCE && resolved.resolved_path.starts_with("/mcp/") {
        let data = super::crunchbase::call(&resolved.resolved_path, &body_bytes, &resolved.preset_auth_headers)
            .await.map_err(|code| json_error_response(StatusCode::BAD_GATEWAY, code, "Crunchbase could not return this result. The source call was not retried."))?;
        Some(Arc::new(crate::cache::CachedUpstreamResponse::new(
            200, Some("application/json".into()), data.to_string().into_bytes(), Duration::from_secs(1),
        )))
    } else if annual_accounts {
        // Always consult the durable source cache, even when the old in-memory
        // cache has an entry. This route must never bypass queue/rate admission.
        let parsed = url::Url::parse(&origin_url).map_err(|_| {
            json_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_kvk_route",
                "Invalid KVK annual accounts route",
            )
        })?;
        if resolved.resolved_method != Method::GET
            || parsed.query().is_some()
            || parsed.host_str() != Some("opendata.kvk.nl")
            || parsed.scheme() != "https"
        {
            return Err(json_error_response(
                StatusCode::BAD_REQUEST,
                "unsupported_kvk_request",
                "This KVK endpoint accepts only an eight-digit KVK number",
            ));
        }
        let number = kvk_accounts::number_from_path(&resolved.resolved_path).map_err(|_| {
            json_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_kvk_number",
                "Use an eight-digit KVK number",
            )
        })?;
        match kvk_accounts::lookup(state.db.worker_pool(), number)
            .await
            .map_err(|error| {
                error!(error=%error, "KVK durable cache unavailable");
                json_error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "kvk_cache_unavailable",
                    "Annual accounts temporarily unavailable",
                )
            })? {
            kvk_accounts::Lookup::Ready(data) => {
                Some(Arc::new(crate::cache::CachedUpstreamResponse::new(
                    200,
                    Some("application/json".into()),
                    data.to_string().into_bytes(),
                    Duration::from_secs(1),
                )))
            }
            kvk_accounts::Lookup::Pending { .. } => {
                let mut response = json_error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "kvk_queued",
                    "The annual account is queued. Retry to retrieve the saved result.",
                );
                response
                    .headers_mut()
                    .insert("retry-after", HeaderValue::from_static("2"));
                return Err(response);
            }
            kvk_accounts::Lookup::Failed { code } => {
                return Err(json_error_response(
                    StatusCode::BAD_GATEWAY,
                    &code,
                    "KVK could not supply this annual account",
                ))
            }
        }
    } else if resolved.api.slug == super::brightdata::SOURCE {
        // The adapter's durable claim permits only one paid submission per
        // URL/cache period. Pending and failed responses leave here before
        // customer settlement; successful saved results use normal billing.
        let query = uri.query().or_else(|| {
            resolved.virtual_execute_request.as_ref()
                .and_then(|execution| execution.upstream_query.as_deref())
        });
        let data = super::brightdata::answer(
            state.db.worker_pool(),
            resolved.resolved_method.as_str(),
            &resolved.resolved_path,
            query,
            &body_bytes,
        ).await.map_err(|error| {
            let mut response = json_error_response(error.status, error.code, error.message);
            response.extensions_mut().insert(BrightDataUnsettled);
            if let Some(retry_after) = error.retry_after {
                response.headers_mut().insert("retry-after", HeaderValue::from_static(retry_after));
            }
            response
        })?;
        Some(Arc::new(crate::cache::CachedUpstreamResponse::new(
            200,
            Some("application/json".into()),
            data.to_string().into_bytes(),
            Duration::from_secs(1),
        )))
    } else if resolved.api.slug == super::iban_check::SOURCE {
        // First-party computed answer: no upstream call, billed and settled
        // exactly like a cached response.
        let data = super::iban_check::answer(resolved.resolved_method.as_str(), &resolved.resolved_path).map_err(|message| {
            json_error_response(StatusCode::BAD_REQUEST, "invalid_source_query", &message)
        })?;
        Some(Arc::new(crate::cache::CachedUpstreamResponse::new(
            200,
            Some("application/json".into()),
            data.to_string().into_bytes(),
            Duration::from_secs(1),
        )))
    } else {
        match &cache_key {
            Some(key) => state.cache.get_upstream_response(key).await,
            None => None,
        }
    };

    // For non-idempotent methods we only retry when the failure happened
    // before the request reached the server (`is_connect()`); otherwise a
    // retry could double-execute on the upstream side.
    if let Some(source) = public_source.as_mut() {
        super::public_sources::bind_body(source, &body_bytes).map_err(|message| {
            json_error_response(StatusCode::BAD_REQUEST, "invalid_source_body", &message)
        })?;
    }
    let method_is_idempotent = is_idempotent_method(&resolved.resolved_method);

    for (attempt_index, delay) in retry_delays.iter().enumerate() {
        if cached.is_some() {
            break;
        }
        if attempt_index > 0 {
            tokio::time::sleep(*delay).await;
        }

        let search_body = public_source
            .as_ref()
            .and_then(|s| s.json_body.as_ref())
            .map(|v| axum::body::Bytes::from(v.to_string()));
        let upstream_method = if search_body.is_some() {
            &Method::POST
        } else {
            &resolved.resolved_method
        };
        let mut request = build_forwarded_request(
            &state.http_client,
            upstream_method,
            &origin_url,
            headers,
            search_body.as_ref().unwrap_or(&body_bytes),
            &resolved.preset_auth_headers,
            idempotency_key.as_deref(),
        );
        if search_body.is_some() {
            request = request.header(header::CONTENT_TYPE, "application/json");
        }
        if let Some(source) = public_source.as_ref() {
            let mut source_headers = HeaderMap::new();
            if let Some(accept) = super::public_sources::accept(source) {
                source_headers.insert(header::ACCEPT, HeaderValue::from_static(accept));
            }
            if search_body.is_some() {
                source_headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
            }
            if let Some(body) = super::public_sources::multipart(source) {
                source_headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("multipart/form-data; boundary=apiosk-public-source"),
                );
                request = request.body(body);
            }
            request = request.headers(source_headers);
        }
        if let Some((ref name, ref value)) = hmac_header {
            request = request.header(name.as_str(), value.as_str());
        }

        match request.send().await {
            Ok(resp) => {
                response = Some(resp);
                break;
            }
            Err(err) => {
                let safe_to_retry = method_is_idempotent || err.is_connect();
                let is_last_attempt = attempt_index + 1 >= retry_delays.len();
                if !is_last_attempt && !safe_to_retry {
                    warn!(
                        "Upstream attempt {}/{} for {} {} failed and method is non-idempotent without a connect-only error; not retrying: {}",
                        attempt_index + 1,
                        retry_delays.len(),
                        resolved.resolved_method,
                        &origin_url,
                        err
                    );
                    last_error = Some(err);
                    break;
                }
                if !is_last_attempt {
                    warn!(
                        "Upstream attempt {}/{} failed for {}: {}",
                        attempt_index + 1,
                        retry_delays.len(),
                        &origin_url,
                        err
                    );
                } else {
                    error!("All upstream attempts failed for {}: {}", &origin_url, err);
                }
                last_error = Some(err);
            }
        }
    }

    // A cache hit skips the network entirely: no upstream call, no resell leg,
    // and the answer replayed byte for byte. Payment and settlement below are
    // untouched — the buyer pays for the answer, not the round trip.
    let mut resell_outcome: Option<super::resell::ResellOutcome> = None;
    let (status, mut response_headers, mut response_body) = if let Some(entry) = cached.as_ref() {
        let mut replayed_headers = HeaderMap::new();
        if let Some(content_type) = entry.content_type.as_deref() {
            if let Ok(value) = HeaderValue::from_str(content_type) {
                replayed_headers.insert(header::CONTENT_TYPE, value);
            }
        }
        replayed_headers.insert("x-apiosk-cache", HeaderValue::from_static("hit"));
        (
            StatusCode::from_u16(entry.status).unwrap_or(StatusCode::OK),
            replayed_headers,
            axum::body::Bytes::from(entry.body.clone()),
        )
    } else {
        match forward_upstream(
            state,
            requested_path,
            method,
            headers,
            browser_mode,
            tracking_context,
            resolved,
            &payment_decision,
            &origin_url,
            &body_bytes,
            idempotency_key.as_deref(),
            hmac_header.as_ref(),
            response,
            last_error,
            &mut resell_outcome,
            public_source.as_ref(),
        )
        .await
        {
            Ok(answered) => answered,
            Err(ShortCircuit::Serve(response)) => return Ok(response),
            Err(ShortCircuit::Fail(response)) => return Err(response),
        }
    };

    if nl_benefits::handles(&resolved.api.slug) {
        response_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
        response_headers.insert("x-apiosk-cache", HeaderValue::from_static("computed"));
    }
    if resolved.api.slug == super::apimarket::SOURCE && status.is_success() {
        if let Some(enriched) = super::crunchbase::enrich(&response_body).await {
            response_body = axum::body::Bytes::from(enriched);
            response_headers.remove(header::CONTENT_LENGTH);
            response_headers.remove(header::CONTENT_ENCODING);
        }
    }

    let should_log = resolved
        .endpoint
        .as_ref()
        .is_none_or(|ep| !ep.payment_required || ep.price == 0_i64);

    if should_log {
        let amount = resolved.endpoint.as_ref().map(|ep| ep.price.clone());
        let network_token_id = resolved
            .endpoint
            .as_ref()
            .and_then(|ep| ep.network_token_id);
        if let Err(e) = state
            .db
            .log_usage(
                resolved.api.id,
                resolved.endpoint.as_ref().map(|ep| ep.id),
                false,
                amount,
                network_token_id,
                None,
                tracking_context.request_id,
                tracking_context.session_id,
                tracking_context.managed_wallet_id,
                tracking_context.client_id.as_deref(),
                tracking_context.agent_connect_token_id,
            )
            .await
        {
            error!("Failed to log usage: {}", e);
        }
    }

    // Remember a fresh, successful answer for the endpoints that opted in.
    // Only 200s: an error, a 402 or a redirect must never be replayed as the
    // provider's answer to somebody else's paid call.
    if let (Some(key), Some(ttl)) = (cache_key.as_ref(), cache_ttl) {
        if cached.is_none() && status == StatusCode::OK {
            state
                .cache
                .put_upstream_response(
                    key.clone(),
                    crate::cache::CachedUpstreamResponse::new(
                        status.as_u16(),
                        response_headers
                            .get(header::CONTENT_TYPE)
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string),
                        response_body.to_vec(),
                        ttl,
                    ),
                )
                .await;
        }
    }

    let latency_ms = request_started_at.elapsed().as_millis() as u64;

    let mut credits_charge = None;
    if let Some(ref pending_credits_charge) = payment_decision.pending_credits_charge {
        if should_charge_credits_after_upstream(status) {
            let request_id = tracking_context.request_id.ok_or_else(|| {
                json_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "request_context_missing",
                    "Gateway request context was not available.",
                )
            })?;

            let endpoint = resolved.endpoint.as_ref().ok_or_else(|| {
                json_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "endpoint_missing",
                    "Endpoint metadata was not available for this paid request.",
                )
            })?;

            match state
                .db
                .charge_credits_for_api_call(
                    pending_credits_charge.user_id,
                    &resolved.api,
                    endpoint,
                    request_id,
                    tracking_context.session_id,
                    tracking_context.managed_wallet_id,
                    tracking_context.client_id.as_deref(),
                    tracking_context.agent_connect_token_id,
                )
                .await
            {
                Ok(charge) => {
                    if charge.already_processed {
                        info!(
                            "Reused existing credits charge for {}/{} request_id={}",
                            api_slug, resolved.resolved_path, request_id
                        );
                    } else {
                        info!(
                            "Credits charged for {}/{} user={} credits={} remaining={}",
                            api_slug,
                            resolved.resolved_path,
                            pending_credits_charge.user_id,
                            charge.credits_used,
                            charge.credits_remaining
                        );
                    }
                    credits_charge = Some(charge);
                }
                Err(CreditChargeError::InsufficientCredits) => {
                    return Err(insufficient_credits_response(
                        browser_mode,
                        None,
                        None,
                        None,
                        &resolved.api.slug,
                        requested_path,
                        &resolved.resolved_method,
                        endpoint,
                        AllowedApiPaymentRails::from_resolved(resolved),
                    ));
                }
                Err(CreditChargeError::UnsupportedPrice) => {
                    return Err(credits_pricing_not_supported_response(
                        browser_mode,
                        None,
                        None,
                        None,
                        &resolved.api.slug,
                        requested_path,
                        &resolved.resolved_method,
                        endpoint,
                        AllowedApiPaymentRails::from_resolved(resolved),
                    ));
                }
                Err(CreditChargeError::Database(err)) => {
                    error!("Failed to charge credits after upstream success: {}", err);
                    return Err(json_error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "credits_charge_failed",
                        "Failed to charge credits for this request.",
                    ));
                }
            }
        } else {
            info!(
                "Skipping credits charge for {}/{} because upstream returned {}",
                api_slug, resolved.resolved_path, status
            );
        }
    }

    // The euro leg used to settle here, after the upstream call returned: log
    // the paid call so the connection's `sepa_daily_limit_eur` accumulated, then
    // either append a debit to `sepa_settlement_ledger` (batched later by
    // `sepa_batch_worker` into one Mollie direct debit) or create a per-call
    // Mollie payment outright. It debited a euro amount taken straight off the
    // endpoint's USDC price and never applied the 15% buyer markup in
    // `fees.rs`, which is the bug that made removing it the fix rather than a
    // simplification. Apiosk keeps one payment rail now: the buyer's Apiosk
    // balance is debited in the App's Supabase and the treasury pays the
    // provider over x402, at the marked-up price, once.

    let credits_charge = credits_charge.as_ref();
    let mut settled_price_paid_usdc =
        credits_charge.map(|charge| charge.amount_eur.to_string().parse().unwrap_or(0.0));
    let mut settled_receipt: Option<SettledPaymentReceipt> = None;
    if let Some(ref deferred_payment) = payment_decision.deferred_payment {
        if resell_outcome.is_none() && !should_settle_deferred_api_payment(status) {
            warn!(
                "Skipping payment settlement for {}/{} because upstream returned {}",
                api_slug, resolved.resolved_path, status
            );
        } else if let Some(ref ep) = resolved.endpoint {
            let receipt = settle_deferred_api_payment(
                state,
                api_slug,
                &resolved.resolved_path,
                &resolved.api,
                ep,
                deferred_payment,
                tracking_context,
                resell_outcome.as_ref(),
            )
            .await?;
            settled_price_paid_usdc = Some(receipt.price_paid_usdc);
            settled_receipt = Some(receipt);
        }
    }

    if browser_mode && (status.is_client_error() || status.is_server_error()) {
        let mut meta = vec![("Upstream status", status.as_u16().to_string())];
        if let Some(content_type) = response_headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
        {
            meta.push(("Upstream content-type", content_type.to_string()));
        }
        return Err(branded_status_page(
            status,
            "Provider returned a non-success status",
            "The gateway reached the provider, but the provider did not return a successful response.",
            &resolved.api.slug,
            requested_path,
            method,
            &[],
            &meta,
            None,
        ));
    }

    if let Some(execution) = resolved.virtual_execute_request.as_ref() {
        return Ok(agent_execute_response(
            &resolved.api.slug,
            execution,
            resolved.endpoint.as_ref(),
            status,
            &response_headers,
            &response_body,
            latency_ms,
            settled_price_paid_usdc,
            credits_charge,
            settled_receipt.as_ref(),
        ));
    }

    let mut builder = Response::builder().status(status.as_u16());
    for (key, value) in response_headers.iter() {
        let key_lower = key.as_str().to_lowercase();
        if key_lower == "content-length"
            || key_lower == "transfer-encoding"
            || key_lower == "content-encoding"
            || key_lower == "x-apiosk-buyer-total-atomic"
            || key_lower == "x-apiosk-price-paid"
            || key_lower == "x-apiosk-price-listed"
            || key_lower == "x-apiosk-payment-rail"
            || key_lower == "x-apiosk-wallet-used"
            || key_lower == "x-apiosk-tx-hash"
        {
            continue;
        }
        if let Ok(header_name) = header::HeaderName::from_bytes(key.as_str().as_bytes()) {
            if let Ok(header_value) = HeaderValue::from_bytes(value.as_bytes()) {
                builder = builder.header(header_name, header_value);
            }
        }
    }

    if let Some(price_paid_usdc) = settled_price_paid_usdc {
        builder = builder.header("X-Apiosk-API", &resolved.api.slug);
        if let Some(receipt) = settled_receipt.as_ref() {
            builder = receipt.price_headers(builder);
        } else {
            builder = builder.header("X-Apiosk-Price-Paid", format!("{:.6}", price_paid_usdc));
            if let Some(ep) = resolved.endpoint.as_ref() {
                builder =
                    builder.header("X-Apiosk-Price-Listed", ep.price.with_scale(6).to_string());
            }
        }
        builder = builder.header(
            "X-Apiosk-Payment-Rail",
            if credits_charge.is_some() {
                "credits"
            } else {
                settled_receipt.as_ref().map_or("x402", SettledPaymentReceipt::payment_rail)
            },
        );
    }

    // Only the gateway can assert a settled buyer amount. Never copy this
    // header from an upstream body/header or infer it from a reservation.
    if settled_receipt.is_some() {
        if let Some(ep) = resolved.endpoint.as_ref() {
            let amount = crate::fees::quote_usdc_with_markup(
                &ep.price,
                crate::fees::ProviderClass::from_listing_type(resolved.api.listing_type.as_deref()),
                tracking_context.buyer_markup_bps(),
            )
            .buyer_total_atomic;
            builder = builder.header("X-Apiosk-Buyer-Total-Atomic", amount.to_string());
        }
    } else if payment_decision.deferred_payment.is_some()
        && !should_settle_deferred_api_payment(status)
        && credits_charge.is_none()
    {
        builder = builder.header("X-Apiosk-Buyer-Total-Atomic", "0");
    }

    if let Some(ref receipt) = settled_receipt {
        // Wallet selection strategy (documented once in src/proxy/wallet.rs:
        // `select_prepaid_wallet_with_fundability`): walk the connect token's
        // wallets in DB order, pick the first that passes per-tx + daily caps
        // AND on-chain `can_settle`. Multi-wallet tokens that match >1 wallet
        // will pick the same wallet across requests until caps or balance
        // change. Surface which wallet actually paid so the agent can verify.
        if let Some(ref wallet) = receipt.wallet_used {
            if let Ok(value) = HeaderValue::from_str(wallet) {
                builder = builder.header("X-Apiosk-Wallet-Used", value);
            }
        }
        if let Some(tx_hash) = receipt.onchain_tx_hash() {
            if let Ok(value) = HeaderValue::from_str(tx_hash) {
                builder = builder.header("X-Apiosk-Tx-Hash", value);
            }
        }
    }

    if let Some(charge) = credits_charge {
        builder = builder
            .header("X-Apiosk-Credits-Used", charge.credits_used.to_string())
            .header(
                "X-Apiosk-Credits-Remaining",
                charge.credits_remaining.to_string(),
            );
    }
    // Six `X-Apiosk-Fiat-*` response headers used to be set here (payment id,
    // provider payment id, status, method, sequence type, risk decision) —
    // Mollie's view of the SEPA direct debit this call had just queued. No call
    // settles in euro any more, so nothing can populate them.
    builder = builder.header("X-Apiosk-Gateway", "true");

    builder
        .body(Body::from(response_body.to_vec()))
        .map_err(|_| {
            json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "response_build_error",
                "Failed to build gateway response",
            )
        })
}

// `create_fiat_charge_for_api_call` used to live here: it POSTed to the buyer
// portal's Supabase edge function to create a Mollie SEPA direct debit for one
// API call, authenticating either as the buyer (their JWT) or as the gateway
// (service-role key plus an actor user-id header). With the euro rail gone
// there is nothing left for it to charge.

/// Pay an upstream's own x402 (from the platform payer wallet) and re-issue the
/// request once with the resulting `X-Payment` proof. Returns the re-issued
/// upstream response (any status — the caller's normal flow settles the agent
/// only on 2xx). On a PRE-payment failure (resell not configured, unparseable
/// 402, cap/spread breach, lost double-pay claim, sign error) returns a clean
/// error to the agent, who is NOT charged (no money has moved).
#[allow(clippy::too_many_arguments)]
async fn settle_upstream_resell(
    state: &Arc<AppState>,
    origin_url: &str,
    resolved: &ResolvedProxyRequest,
    headers: &HeaderMap<HeaderValue>,
    body_bytes: &axum::body::Bytes,
    idempotency_key: Option<&str>,
    hmac_header: Option<&(String, String)>,
    request_id: Uuid,
    listing_price_atomic: u128,
    resp_402: reqwest::Response,
) -> Result<(reqwest::Response, super::resell::ResellOutcome), Response<Body>> {
    fn fail(msg: &str) -> Response<Body> {
        let body = serde_json::json!({ "error": "resell_payment_failed", "message": msg });
        Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .header("Content-Type", "application/json")
            .header("X-Apiosk-Gateway", "true")
            .header("X-Apiosk-Buyer-Total-Atomic", "0")
            .body(Body::from(body.to_string()))
            .unwrap_or_else(|_| Response::new(Body::from(r#"{"error":"resell_payment_failed"}"#)))
    }

    // Payer wallet must be configured (fail closed).
    let Some(payer) = state.resell_payer.as_ref() else {
        warn!(
            "Resell listing {} reached but RESELL_PAYER_PRIVATE_KEY is unset",
            resolved.api.slug
        );
        return Err(fail("This API is temporarily unavailable."));
    };

    // Read the upstream 402 challenge — from the body AND from the header.
    //
    // The header is not optional detail. An x402 v2 provider puts its
    // `accepts[]` in the base64 `PAYMENT-REQUIRED` header and leaves the body
    // as a problem+json document with no accepts in it at all:
    //
    //   {"type":"https://paymentauth.org/problems/payment-required",
    //    "title":"Payment Required","status":402,"detail":"…"}
    //
    // Parsed body-only, that reads as "the upstream payment challenge could
    // not be understood" and the call dies before a payment is ever attempted.
    // `/v1/x402/fetch` has always read both; this path did not, so the entire
    // reviewed catalogue was unable to pay any v2 provider that challenges
    // through the header — a growing share of them.
    let payment_required_header = resp_402
        .headers()
        .get("payment-required")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = match read_upstream_body_capped(resp_402, max_upstream_body_bytes()).await {
        Ok(b) => b,
        Err(_) => return Err(fail("Could not read the upstream payment challenge.")),
    };
    let requirement = match super::resell::parse_reviewed_upstream_402(
        &body,
        payment_required_header.as_deref(),
        &resolved.api.slug,
    ) {
        Ok(r) => r,
        Err(e) => {
            warn!(
                "Resell {}: unparseable upstream 402: {}",
                resolved.api.slug, e
            );
            return Err(fail(
                "The upstream payment challenge could not be understood.",
            ));
        }
    };

    // Bazaar-imported listings: host policy and the synced payee, before any
    // claim or signature. Other listings pass without a lookup.
    if let Err(refusal) = super::bazaar_guard::guard(
        state,
        resolved.listing.as_ref().map(|l| &l.listing_metadata),
        origin_url,
        &requirement,
    )
    .await
    {
        warn!(api_slug = %resolved.api.slug, reason = refusal.code, "Resell refused by the Bazaar guard");
        return Err(fail(refusal.message));
    }

    // Caps + spread guard BEFORE signing.
    let amount = match super::resell::check_spend_allowed(
        &requirement.value_atomic,
        payer.cap_per_tx_atomic,
        listing_price_atomic,
    ) {
        Ok(a) => a,
        Err(e) => {
            warn!("Resell {}: spend refused: {}", resolved.api.slug, e);
            return Err(fail("This request exceeds the resell spending limit."));
        }
    };

    // Durable double-pay claim (fail CLOSED on any DB error — never pay real
    // USDC without a committed claim).
    match state
        .db
        .try_claim_resell_payment(
            request_id,
            resolved.api.id,
            amount,
            payer.cap_per_day_atomic,
        )
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return Err(json_error_response(
                StatusCode::GATEWAY_TIMEOUT,
                "payment_unresolved",
                "The payment claim is already in flight or unavailable. The amount remains held.",
            ))
        }
        Err(e) => {
            error!(
                "Resell {}: claim failed (failing closed): {}",
                resolved.api.slug, e
            );
            return Err(fail("This API is temporarily unavailable."));
        }
    }

    // Sign the EIP-3009 authorization and base64 the X-Payment proof.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let x_payment = match super::resell::build_x402_payment_header(
        payer.wallet(),
        &requirement,
        &request_id,
        now,
    ) {
        Ok(h) => h,
        Err(e) => {
            error!("Resell {}: signing failed: {}", resolved.api.slug, e);
            let _ = state
                .db
                .mark_resell_payment(request_id, "failed", None)
                .await;
            return Err(fail("Payment authorization could not be created."));
        }
    };

    // Re-issue the SAME request ONCE, now carrying the payment proof.
    let mut request = build_forwarded_request(
        &state.http_client,
        &resolved.resolved_method,
        origin_url,
        headers,
        body_bytes,
        &resolved.preset_auth_headers,
        idempotency_key,
    );
    // x402 v2 servers read the proof from PAYMENT-SIGNATURE; v1 from X-PAYMENT.
    // Send the version-matched proof in the right header, plus X-PAYMENT as a
    // universally-understood fallback (proven necessary by a live probe against
    // OpenWebNinja, which is v2-native and ignores an X-PAYMENT-only proof).
    if requirement.x402_version >= 2 {
        request = request.header("payment-signature", x_payment.clone());
        if resolved.api.slug != "sirenic" {
            request = request.header("x-payment", x_payment);
        }
    } else {
        request = request.header("x-payment", x_payment);
    }
    if let Some((name, value)) = hmac_header {
        request = request.header(name.as_str(), value.as_str());
    }

    match request.send().await {
        Ok(resp) => {
            let transaction = super::resell::payment_transaction(
                resp.headers()
                    .get("payment-response")
                    .or_else(|| resp.headers().get("x-payment-response"))
                    .and_then(|v| v.to_str().ok()),
            )
            .ok_or_else(|| {
                // Retain only the provider's error category, never a signature,
                // authorization or full header. This diagnoses quote dialects
                // without another potentially payable request.
                let code = resp.headers().get("payment-required")
                    .and_then(|h| h.to_str().ok())
                    .filter(|h| h.len() <= 16384)
                    .and_then(|h| BASE64_STANDARD.decode(h).ok())
                    .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                    .and_then(|d| d["error"].as_str().map(|s| s.split(':').next().unwrap_or("").to_owned()))
                    .filter(|s| s.starts_with("invalid_") && s.len() <= 100 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
                    .unwrap_or_else(|| "unconfirmed".into());
                warn!(api_slug=%resolved.api.slug, upstream_status=%resp.status(), provider_error=%code, "Upstream payment receipt unavailable; retaining claim");
                json_error_response(
                    StatusCode::GATEWAY_TIMEOUT,
                    "payment_unresolved",
                    "The provider has not confirmed its payment. The amount remains held.",
                )
            })?;
            state
                .db
                .mark_resell_payment(request_id, "paid", Some(&transaction))
                .await
                .map_err(|_| {
                    json_error_response(
                        StatusCode::GATEWAY_TIMEOUT,
                        "payment_unresolved",
                        "Payment receipt could not be recorded. The amount remains held.",
                    )
                })?;
            if !resp.status().is_success() {
                warn!(
                    "Resell {}: paid retry returned {} (upstream funded, agent not charged)",
                    resolved.api.slug,
                    resp.status()
                );
            }
            // Carry what we paid and who paid it: settlement must reimburse the
            // bridge rather than pay the provider a second time.
            Ok((
                resp,
                super::resell::ResellOutcome {
                    transaction,
                    upstream_atomic: amount,
                    payer_address: format!("{:#x}", payer.address()),
                },
            ))
        }
        Err(e) => {
            error!(
                "Resell {}: paid retry transport error: {}",
                resolved.api.slug, e
            );
            Err(json_error_response(
                StatusCode::GATEWAY_TIMEOUT,
                "payment_unresolved",
                "Provider payment confirmation is pending. The amount remains held.",
            ))
        }
    }
}
