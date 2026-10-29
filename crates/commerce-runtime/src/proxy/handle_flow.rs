use super::*;

use crate::db::IdempotencyClaim;

pub async fn handle(
    state: Arc<AppState>,
    api_slug: &str,
    path: &str,
    request: Request<Body>,
) -> Result<Response<Body>, StatusCode> {
    if !is_valid_api_slug(api_slug) {
        return Ok(json_error_response(
            StatusCode::BAD_REQUEST,
            "invalid_slug",
            "API slug must be lowercase alphanumeric or hyphen, 1-64 chars.",
        ));
    }
    let requested_path = normalize_path(path);
    info!("PROXY HANDLE: slug={}, path={}", api_slug, requested_path);
    let method = request.method().clone();
    let headers = request.headers().clone();
    let uri = request.uri().clone();
    let agent_surface_request = requested_path == "/metadata" || requested_path == "/execute";
    let browser_mode = is_browser_html_request(&method, &headers) && !agent_surface_request;
    let request_context = request.extensions().get::<RequestContext>().cloned();
    let mut tracking_context = TrackingContext::from_request(request_context.as_ref());
    // `X-Apiosk-Buyer-Markup-Bps` is trusted here, once, and only beside a
    // valid app service key; every other caller prices at the default 15%.
    tracking_context.buyer_markup_bps =
        crate::service_auth::buyer_markup_bps(state.config.app_service_key.as_deref(), &headers);
    let mut request = Some(request);

    // Idempotency-Key: claim the key before anything runs, so that
    //   * a retry of a FINISHED call replays the stored response (no second
    //     settlement),
    //   * a duplicate of a call STILL RUNNING is answered `409 in_progress`
    //     rather than run — and paid — a second time,
    //   * a key reused for a different method or path is a 409 conflict (the
    //     fingerprint = sha256(method + path) is what catches a leaked key).
    // The claim is settled after the call: finalised into a replayable record
    // when the request was forwarded, released when it was refused before any
    // money could move, so the caller can fix the cause and retry the key.
    let idempotency_key = match extract_idempotency_key(&headers) {
        IdempotencyKey::Absent => None,
        IdempotencyKey::Valid(key) => Some(key),
        IdempotencyKey::Malformed => {
            return Ok(json_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_idempotency_key",
                "Idempotency-Key header is present but empty or malformed; omit it or send a valid 1-255 char ASCII key.",
            ));
        }
    };
    if headers.contains_key("x-apiosk-max-buyer-total-atomic") && idempotency_key.is_none() {
        return Ok(json_error_response(
            StatusCode::BAD_REQUEST,
            "idempotency_key_required",
            "Planned calls require an idempotency key.",
        ));
    }
    let mut claim: Option<(Uuid, String)> = None;
    if let Some(ref key) = idempotency_key {
        match resolve_connect_token_id(&state, &headers).await {
            Ok(Some(token_id)) => {
                let fingerprint = compute_idempotency_request_fingerprint(&method, &requested_path);
                match state
                    .db
                    .claim_idempotency_key(token_id, key, &fingerprint)
                    .await
                {
                    Ok(IdempotencyClaim::Claimed) => {
                        claim = Some((token_id, key.clone()));
                    }
                    Ok(IdempotencyClaim::Finished(record)) => {
                        info!(
                            token_id = %token_id,
                            idempotency_key = %key,
                            api_slug,
                            stored_status = record.response_status,
                            "Idempotency-Key replay; nothing re-run, nothing re-paid"
                        );
                        return Ok(rebuild_response_from_record(record));
                    }
                    Ok(IdempotencyClaim::InFlight) => {
                        info!(
                            token_id = %token_id,
                            idempotency_key = %key,
                            api_slug,
                            "Idempotency-Key duplicate while the first attempt is in flight"
                        );
                        return Ok(in_progress_response());
                    }
                    Ok(IdempotencyClaim::Conflict) => {
                        return Ok(json_error_response(
                            StatusCode::CONFLICT,
                            "idempotency_key_conflict",
                            "Idempotency-Key was reused with a different method or path. Use a unique key per logical operation.",
                        ));
                    }
                    Err(err) => {
                        warn!("Idempotency claim unavailable: {err}");
                        return Ok(json_error_response(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "idempotency_unavailable",
                            "The execution could not be recorded safely; nothing was sent.",
                        ));
                    }
                }
            }
            Ok(None) => {
                if headers.contains_key("x-apiosk-max-buyer-total-atomic") {
                    return Ok(json_error_response(
                        StatusCode::UNAUTHORIZED,
                        "idempotency_scope_required",
                        "Planned calls require a scoped execution identity.",
                    ));
                }
                // Idempotency-Key without a connect token: clients can still
                // send it but we have no per-buyer scope to cache against,
                // so it becomes a no-op. Documented.
            }
            Err(err) => {
                warn!("Idempotency identity unavailable: {err}");
                return Ok(json_error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "idempotency_unavailable",
                    "The execution identity could not be verified; nothing was sent.",
                ));
            }
        }
    }

    let handled = handle_claimed(
        &state,
        api_slug,
        path,
        &requested_path,
        &method,
        &headers,
        &uri,
        browser_mode,
        &tracking_context,
        &mut request,
    )
    .await;

    let Some((token_id, key)) = claim else {
        return Ok(match handled {
            Handled::Forwarded(response) | Handled::Refused(response) => response,
        });
    };

    match handled {
        // Refused before the paid path ran: nothing settled, so the key is
        // handed back rather than pinned to a refusal the caller can fix.
        Handled::Refused(response) => {
            if let Err(err) = state.db.release_idempotency_claim(token_id, &key).await {
                warn!(
                    "Failed to release idempotency claim (token={} key={}): {}",
                    token_id, key, err
                );
            }
            Ok(response)
        }
        // Forwarded: store the outcome under the key so a retry replays it
        // instead of triggering another settlement. Storage failures never
        // block the live response. The claim stays held: losing the replay
        // record does not prove the source was free.
        Handled::Forwarded(response) => {
            match capture_and_store_idempotency_response(&state, token_id, &key, response).await {
                Ok(replay_ready) => Ok(replay_ready),
                Err(err) => {
                    warn!(
                        "Failed to persist idempotency record (token={} key={}): {} — returning live response uncached",
                        token_id, key, err
                    );
                    Ok(err.fallback_response)
                }
            }
        }
    }
}

/// What the paid path produced: a response that came back through
/// `forward_api_request` (the call was carried, and possibly paid), or a
/// refusal made before it ran (nothing settled).
enum Handled {
    Forwarded(Response<Body>),
    Refused(Response<Body>),
}

/// `409 in_progress` — the same key is being executed right now. The caller
/// keeps the key and retries after a moment; the outcome of the first attempt
/// replays once it lands.
fn in_progress_response() -> Response<Body> {
    let mut response = json_error_response(
        StatusCode::CONFLICT,
        "in_progress",
        "A request with this Idempotency-Key is already in progress. Retry with the same key shortly to receive its outcome.",
    );
    if let Ok(value) = HeaderValue::from_str("2") {
        response.headers_mut().insert("retry-after", value);
    }
    response
}

/// Everything after the idempotency claim, up to and including the forward.
/// Split out so the caller can tell a refusal from a forwarded response and
/// settle the claim accordingly.
#[allow(clippy::too_many_arguments)]
async fn handle_claimed(
    state: &Arc<AppState>,
    api_slug: &str,
    path: &str,
    requested_path: &str,
    method: &Method,
    headers: &HeaderMap<HeaderValue>,
    uri: &Uri,
    browser_mode: bool,
    tracking_context: &TrackingContext,
    request: &mut Option<Request<Body>>,
) -> Handled {
    let mut resolved = match resolve_proxy_request_state(
        state,
        api_slug,
        path,
        requested_path,
        method,
        browser_mode,
        request,
    )
    .await
    {
        Ok(resolved) => resolved,
        Err(response) => return Handled::Refused(response),
    };

    // Resolve parameter-based pricing before dry-run, wallet selection, or the
    // x402 PaymentRequirement is created. The endpoint's stored price remains
    // the authoritative fallback when no active rule matches (or when the
    // optional rules table cannot be read during a rolling migration).
    apply_query_parameter_price(state, uri, &mut resolved).await;

    if let Some(session_id) = tracking_context.session_id {
        if let Err(err) = state
            .db
            .touch_presence_session(session_id, Some(&resolved.api.slug))
            .await
        {
            warn!("Failed to update presence session activity: {}", err);
        }
    }

    // Dry-run short-circuit. Runs BEFORE enforce_endpoint_payment so the
    // x402-with-proof settlement path (which would otherwise charge the
    // caller) never executes. Returns the same shape the dedicated
    // /v1/apis/{slug}/preflight endpoint returns.
    if dry_run_requested(headers) {
        let verdict = build_preflight_verdict(state, headers, &resolved).await;
        return Handled::Refused(preflight_response(&verdict));
    }

    let payment_decision = match enforce_endpoint_payment(
        state,
        api_slug,
        requested_path,
        headers,
        browser_mode,
        tracking_context,
        &resolved,
    )
    .await
    {
        Ok(payment) => payment,
        Err(response) => return Handled::Refused(response),
    };

    if let Some(response) = enforce_rate_limit(
        state,
        &resolved.api.slug,
        browser_mode,
        tracking_context,
        payment_decision.agent_connect_token_id,
    )
    .await
    {
        return Handled::Refused(response);
    }

    match dispatch_non_api_listing(state, path, method, browser_mode, request, &resolved).await {
        Ok(Some(response)) => return Handled::Refused(response),
        Ok(None) => {}
        Err(response) => return Handled::Refused(response),
    }

    match forward_api_request(
        state,
        api_slug,
        requested_path,
        method,
        headers,
        uri,
        browser_mode,
        tracking_context,
        request,
        &resolved,
        payment_decision,
    )
    .await
    {
        Ok(response) => Handled::Forwarded(response),
        Err(response) => classify_forward_failure(api_slug, response),
    }
}

fn classify_forward_failure(api_slug: &str, response: Response<Body>) -> Handled {
    // A saved pending collection is safe to look up again: its separate durable
    // claim prevents another paid trigger, and no buyer settlement ran. Pinning
    // the 503 in the payment replay cache would make the result unreachable.
    if api_slug == super::brightdata::SOURCE
        && response.extensions().get::<BrightDataUnsettled>().is_some()
    {
        Handled::Refused(response)
    } else {
        Handled::Forwarded(response)
    }
}

#[cfg(test)]
mod brightdata_idempotency_tests {
    use super::*;

    #[test]
    fn only_trusted_unsettled_adapter_responses_release_the_payment_claim() {
        let response = || json_error_response(StatusCode::SERVICE_UNAVAILABLE,
            "brightdata_pending", "Collection is pending");
        // A matching body/header alone must not bypass payment idempotency.
        let mut spoofed = response();
        spoofed.headers_mut().insert("x-apiosk-brightdata-unsettled", HeaderValue::from_static("true"));
        assert!(matches!(classify_forward_failure("brightdata", spoofed), Handled::Forwarded(_)));
        let mut marked = response();
        marked.extensions_mut().insert(BrightDataUnsettled);
        assert!(matches!(classify_forward_failure("brightdata", marked), Handled::Refused(_)));
        let mut other = response();
        other.extensions_mut().insert(BrightDataUnsettled);
        assert!(matches!(classify_forward_failure("another-source", other), Handled::Forwarded(_)));
        assert!(matches!(classify_forward_failure("brightdata", Response::new(Body::empty())), Handled::Forwarded(_)));
    }
}

async fn apply_query_parameter_price(
    state: &Arc<AppState>,
    uri: &Uri,
    resolved: &mut ResolvedProxyRequest,
) {
    let Some(endpoint) = resolved.endpoint.as_mut() else {
        return;
    };
    let rules = match state.db.list_endpoint_pricing_rules(endpoint.id).await {
        Ok(rules) => rules,
        Err(err) => {
            warn!(
                "Pricing-rule lookup failed for endpoint {}: {}; using default price",
                endpoint.id, err
            );
            return;
        }
    };
    if rules.is_empty() {
        return;
    }
    let params: Vec<(String, String)> = uri
        .query()
        .map(|query| {
            url::form_urlencoded::parse(query.as_bytes())
                .into_owned()
                .collect()
        })
        .unwrap_or_default();
    for rule in rules {
        let values: Vec<&str> = params
            .iter()
            .filter(|(name, _)| name == &rule.parameter_name)
            .map(|(_, value)| value.as_str())
            .collect();
        let expected = rule.comparison_value.as_deref().unwrap_or("");
        let matched = match rule.operator.as_str() {
            "exists" => !values.is_empty(),
            "not_exists" => values.is_empty(),
            "equals" => values.contains(&expected),
            "not_equals" => values.is_empty() || values.iter().all(|value| *value != expected),
            "contains" => values.iter().any(|value| value.contains(expected)),
            "not_contains" => {
                values.is_empty() || values.iter().all(|value| !value.contains(expected))
            }
            "starts_with" => values.iter().any(|value| value.starts_with(expected)),
            "ends_with" => values.iter().any(|value| value.ends_with(expected)),
            _ => false,
        };
        if matched {
            info!(
                "Pricing rule priority {} matched endpoint {} on parameter {}",
                rule.priority, endpoint.id, rule.parameter_name
            );
            endpoint.price = rule.price;
            endpoint.payment_required = endpoint.price != 0_i64;
            return;
        }
    }
}

/// Idempotency-Key parse outcome. The distinction between `Absent` and
/// `Malformed` matters: a missing key is fine for x402 (key-optional),
/// but a key that is *present and broken* should be rejected loudly
/// instead of silently treated as absent.
pub(super) enum IdempotencyKey {
    /// No Idempotency-Key header at all.
    Absent,
    /// Present and valid (1..=255 ASCII chars after trim).
    Valid(String),
    /// Present but empty / oversized / non-ASCII.
    Malformed,
}

fn extract_idempotency_key(headers: &HeaderMap<HeaderValue>) -> IdempotencyKey {
    let Some(value) = headers.get("idempotency-key") else {
        return IdempotencyKey::Absent;
    };
    let Some(raw) = value.to_str().ok().map(str::trim) else {
        return IdempotencyKey::Malformed;
    };
    // Stripe-style: keep it short and unambiguous. 1..=255 chars, ASCII.
    if raw.is_empty() || raw.len() > 255 || !raw.is_ascii() {
        return IdempotencyKey::Malformed;
    }
    IdempotencyKey::Valid(raw.to_string())
}

// `idempotency_key_present` used to live here: a bare "is the header there at
// all" check, needed only by the SEPA settlement path, which demanded a key so
// a retry could not double-debit the euro ledger. x402 has always been
// key-optional, so with the euro rail gone nothing asks the question.

async fn resolve_connect_token_id(
    state: &Arc<AppState>,
    headers: &HeaderMap<HeaderValue>,
) -> anyhow::Result<Option<Uuid>> {
    // A service key (Apiosk's own surfaces) scopes idempotency to the
    // treasury's token, exactly as if that token had been presented.
    match crate::service_auth::resolve(state, headers).await {
        crate::service_auth::ServiceAuth::NotPresented => {}
        crate::service_auth::ServiceAuth::Rejected(..) => return Ok(None),
        crate::service_auth::ServiceAuth::Resolved(lookup) => {
            return Ok(match lookup {
                crate::db::AgentConnectTokenLookup::Active(token) => Some(token.token_id),
                _ => None,
            });
        }
    }
    let Some(token) = extract_agent_connect_token(headers) else {
        return Ok(None);
    };
    let token_hash = hash_connect_token(&token);
    let lookup = state
        .db
        .lookup_agent_connect_token_by_hash(&token_hash)
        .await?;
    Ok(match lookup {
        crate::db::AgentConnectTokenLookup::Active(token) => Some(token.token_id),
        _ => None,
    })
}

fn compute_idempotency_request_fingerprint(method: &Method, requested_path: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(method.as_str().as_bytes());
    hasher.update(b"\n");
    hasher.update(requested_path.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

fn rebuild_response_from_record(record: crate::db::IdempotencyRecord) -> Response<Body> {
    let status = StatusCode::from_u16(record.response_status as u16)
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut builder = Response::builder().status(status);
    if let Some(map) = record.response_headers.as_object() {
        for (name, value) in map {
            let Some(value_str) = value.as_str() else {
                continue;
            };
            if let (Ok(hname), Ok(hvalue)) = (
                header::HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(value_str),
            ) {
                builder = builder.header(hname, hvalue);
            }
        }
    }
    builder = builder.header("X-Apiosk-Idempotent-Replay", "true");
    builder
        .body(Body::from(record.response_body))
        .unwrap_or_else(|_| Response::new(Body::from(r#"{"error":"replay_build_error"}"#)))
}

struct IdempotencyStoreError {
    fallback_response: Response<Body>,
}

impl std::fmt::Display for IdempotencyStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "idempotency persistence failed")
    }
}

/// Read the body bytes from the live response, finalise the claim under the
/// idempotency key with (status, filtered headers, body), and return a freshly
/// rebuilt response carrying the same bytes so the caller can ship it to the
/// client.
async fn capture_and_store_idempotency_response(
    state: &Arc<AppState>,
    token_id: Uuid,
    key: &str,
    response: Response<Body>,
) -> Result<Response<Body>, IdempotencyStoreError> {
    let (parts, body) = response.into_parts();
    let body_bytes = match axum::body::to_bytes(body, MAX_PROXY_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(err) => {
            error!(
                "Failed to buffer response body for idempotency cache: {}",
                err
            );
            // Can't recover the body — return a 502 so the retry will get a
            // fresh charge instead of a half-replayed response.
            return Err(IdempotencyStoreError {
                fallback_response: json_error_response(
                    StatusCode::BAD_GATEWAY,
                    "response_buffer_failed",
                    "Gateway could not buffer the upstream response for the idempotency cache.",
                ),
            });
        }
    };

    // Only keep headers an agent needs to reconstruct the receipt. Skip
    // hop-by-hop and content-encoding so the replayed bytes match what we
    // serialized to disk.
    let mut header_map = serde_json::Map::new();
    for (name, value) in parts.headers.iter() {
        let lower = name.as_str().to_ascii_lowercase();
        if lower == "content-length"
            || lower == "transfer-encoding"
            || lower == "content-encoding"
            || lower == "connection"
        {
            continue;
        }
        if let Ok(text) = value.to_str() {
            header_map.insert(lower, serde_json::Value::String(text.to_string()));
        }
    }
    let headers_json = serde_json::Value::Object(header_map);

    match state
        .db
        .finalize_idempotency_record(
            token_id,
            key,
            parts.status.as_u16(),
            headers_json,
            &body_bytes,
        )
        .await
    {
        Ok(true) => {}
        Ok(false) => warn!(
            "Idempotency claim was gone at finalise (token={} key={}); response served uncached",
            token_id, key
        ),
        Err(err) => warn!(
            "Idempotency store failed (token={} key={}): {}",
            token_id, key, err
        ),
        // Either way, fall through: the live response is still returned.
    }

    let mut rebuilt = Response::from_parts(parts, Body::from(body_bytes));
    rebuilt.headers_mut().insert(
        "X-Apiosk-Idempotent-Replay",
        HeaderValue::from_static("false"),
    );
    Ok(rebuilt)
}

async fn resolve_proxy_request_state(
    state: &Arc<AppState>,
    api_slug: &str,
    path: &str,
    requested_path: &str,
    method: &Method,
    browser_mode: bool,
    request: &mut Option<Request<Body>>,
) -> Result<ResolvedProxyRequest, Response<Body>> {
    let api = match state.cache.get_api(&state.db, api_slug).await {
        Ok(Some(api)) => api,
        Ok(None) => {
            info!("API not found: {}", api_slug);
            if browser_mode {
                return Err(branded_status_page(
                    StatusCode::NOT_FOUND,
                    "Endpoint not found",
                    "No provider matched this gateway slug.",
                    api_slug,
                    path,
                    method,
                    &[],
                    &[("Gateway", "gateway.apiosk.com".to_string())],
                    None,
                ));
            }
            return Err(json_error_response(
                StatusCode::NOT_FOUND,
                "not_found",
                &format!("API '{}' not found", api_slug),
            ));
        }
        Err(e) => {
            error!("Database error: {}", e);
            if browser_mode {
                return Err(branded_status_page(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Gateway unavailable",
                    "The gateway could not resolve this provider right now.",
                    api_slug,
                    path,
                    method,
                    &[],
                    &[("Reason", "database_error".to_string())],
                    None,
                ));
            }
            return Err(json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database_error",
                "Failed to resolve API",
            ));
        }
    };

    if api.status != "active" {
        info!("API not active: {}", api_slug);
        if browser_mode {
            return Err(branded_status_page(
                StatusCode::SERVICE_UNAVAILABLE,
                "Provider inactive",
                "This provider is currently inactive and cannot serve requests.",
                &api.slug,
                path,
                method,
                &[],
                &[("Status", api.status.clone())],
                None,
            ));
        }
        return Err(json_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "api_inactive",
            &format!("API '{}' is not active", api_slug),
        ));
    }

    // Verification gate: a provider that hasn't passed Apiosk's verification
    // step cannot route real traffic, regardless of self-reported `status`.
    // Health-check-passes-at-registration alone is no longer enough.
    // Defense-in-depth: re-validate the configured upstream URL on every
    // request. Registration already checks it, but a provider could have
    // updated origin_url to something unsafe via a DB migration or admin
    // path. We refuse to route there rather than turning the gateway into
    // an SSRF tool.
    let upstream_for_check = api
        .endpoint_url
        .as_deref()
        .unwrap_or(api.origin_url.as_str());
    if let Err(reason) = crate::api_management::helpers::validate_upstream_url(upstream_for_check) {
        warn!(
            "Refusing to route {}: configured upstream {} is not allowed ({})",
            api_slug, upstream_for_check, reason
        );
        return Err(json_error_response(
            StatusCode::BAD_GATEWAY,
            "upstream_url_not_allowed",
            "This API has an unsafe or invalid upstream URL configured.",
        ));
    }

    if !api.is_verified.unwrap_or(false) {
        info!("API not verified: {}", api_slug);
        if browser_mode {
            return Err(branded_status_page(
                StatusCode::FORBIDDEN,
                "Provider awaiting verification",
                "This provider has registered but has not yet been verified by Apiosk. Routing will start once an operator verifies the listing.",
                &api.slug,
                path,
                method,
                &[],
                &[("Reason", "api_not_verified".to_string())],
                None,
            ));
        }
        return Err(json_error_response(
            StatusCode::FORBIDDEN,
            "api_not_verified",
            &format!(
                "API '{}' has registered but is not yet verified by Apiosk; no traffic is routed yet.",
                api_slug
            ),
        ));
    }

    if *method == Method::GET && browser_mode && requested_path == "/" {
        let endpoints = match state.db.list_endpoints_for_api(api.id).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("Failed to list endpoints for {}: {}", api.slug, e);
                vec![]
            }
        };
        return Err(branded_api_manifest_page(&api.slug, &endpoints));
    }

    let listing = state.db.get_listing_by_slug(api_slug).await.ok().flatten();
    if listing_execution_disabled(listing.as_ref()) && requested_path != "/metadata" {
        info!("Execution disabled for browse-only source: {}", api_slug);
        return Err(json_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "catalog_only",
            "This source is available for catalog browsing, but execution is disabled until its commercial access and credentials are validated.",
        ));
    }
    let listing_type = listing
        .as_ref()
        .map(|l| l.listing_type.clone())
        .unwrap_or(ListingType::Api);
    let preset_auth_headers = listing
        .as_ref()
        .map(|l| extract_preset_auth_headers(&l.listing_metadata))
        .unwrap_or_default();
    let preset_auth_query = listing
        .as_ref()
        .map(|l| extract_preset_auth_query(&l.listing_metadata))
        .unwrap_or_default();

    // Federated listings used to be diverted here to a non-payable 404, because
    // the agent was expected to call the provider directly. Under the hybrid fee
    // model that is a route around the buyer's 15%, so every listing is served
    // through the gateway and this divert is gone.
    //
    // A federated call is served by the RESELL leg: the gateway forwards, gets
    // the provider's own 402, pays it from the bridge wallet, and retries once.
    // That requires `listing_metadata.resell.enabled` and a forwardable
    // `origin_url` — both set by migration 081. A federated listing missing
    // either still fails, but as an honest upstream error rather than a
    // published invitation to bypass Apiosk.

    let agent_surface_request = requested_path == "/metadata" || requested_path == "/execute";
    let agent_surface = if agent_surface_request {
        Some(load_agent_surface_context(state, &api.slug, api.id).await?)
    } else {
        None
    };

    if requested_path == "/metadata" {
        if *method != Method::GET {
            return Err(json_error_response(
                StatusCode::METHOD_NOT_ALLOWED,
                "method_not_allowed",
                "Use GET /metadata to inspect the agent-native manifest.",
            ));
        }
        let Some(agent_surface) = agent_surface else {
            return Err(json_error_response(
                StatusCode::NOT_FOUND,
                "metadata_unavailable",
                "Agent metadata is unavailable for this listing.",
            ));
        };
        return Err(agent_metadata_response(&agent_surface));
    }

    let virtual_execute_request = if requested_path == "/execute" {
        if listing_type != ListingType::Api {
            return Err(json_error_response(
                StatusCode::NOT_FOUND,
                "execute_not_supported",
                "The uniform /execute contract is only available for API listings.",
            ));
        }
        if *method != Method::POST {
            return Err(json_error_response(
                StatusCode::METHOD_NOT_ALLOWED,
                "method_not_allowed",
                "Use POST /execute with a JSON body.",
            ));
        }
        let Some(agent_surface) = agent_surface.as_ref() else {
            return Err(json_error_response(
                StatusCode::NOT_FOUND,
                "execute_unavailable",
                "Agent execution metadata is unavailable for this listing.",
            ));
        };
        Some(
            resolve_virtual_execute_request(
                take_request_or_error(request, &api.slug, requested_path, method, browser_mode)?,
                agent_surface,
                requested_path,
            )
            .await?,
        )
    } else {
        None
    };

    let resolved_method = virtual_execute_request
        .as_ref()
        .map(|execution| execution.upstream_method.clone())
        .unwrap_or_else(|| method.clone());
    let resolved_path = virtual_execute_request
        .as_ref()
        .map(|execution| execution.upstream_path.clone())
        .unwrap_or_else(|| requested_path.to_string());

    let cache_key = CacheKey {
        api_id: api.id,
        method: resolved_method.to_string(),
        path: resolved_path.clone(),
    };

    let endpoint = state
        .cache
        .get_endpoint(&state.db, &cache_key)
        .await
        .map_err(|e| {
            error!("Database error: {}", e);
            if browser_mode {
                branded_status_page(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Gateway unavailable",
                    "Could not resolve endpoint configuration.",
                    &api.slug,
                    requested_path,
                    &resolved_method,
                    &[],
                    &[("Reason", "endpoint_lookup_error".to_string())],
                    None,
                )
            } else {
                json_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "endpoint_lookup_error",
                    "Failed to resolve endpoint configuration",
                )
            }
        })?;

    if endpoint.is_none() && *method == Method::GET && requested_path == "/" {
        let endpoints = match state.db.list_endpoints_for_api(api.id).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("Failed to list endpoints for {}: {}", api.slug, e);
                vec![]
            }
        };

        if browser_mode {
            return Err(branded_api_manifest_page(&api.slug, &endpoints));
        }

        let endpoint_json: Vec<serde_json::Value> = endpoints
            .iter()
            .map(|ep| {
                serde_json::json!({
                    "method": ep.method,
                    "path": ep.path,
                    "payment_required": ep.payment_required,
                    "price_usdc": ep.price.to_string(),
                    "gateway_url": format!(
                        "https://gateway.apiosk.com/{}{}",
                        api.slug,
                        normalize_path(&ep.path)
                    ),
                })
            })
            .collect();

        let body = serde_json::json!({
            "success": true,
            "api_slug": api.slug,
            "message": "Use one of the configured endpoint paths below.",
            "endpoints": endpoint_json,
        });

        let response = Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .header("X-Apiosk-Gateway", "true")
            .body(Body::from(body.to_string()))
            .unwrap_or_else(|_| Response::new(Body::from(r#"{"error":"response_build_error"}"#)));
        return Err(response);
    }

    if endpoint.is_none() && resolved_method == Method::GET {
        let allowed_methods = match state
            .db
            .get_endpoint_methods_for_path(api.id, &resolved_path)
            .await
        {
            Ok(methods) => methods,
            Err(e) => {
                warn!(
                    "Failed to look up allowed methods for {}{}: {}",
                    api.slug, resolved_path, e
                );
                vec![]
            }
        };

        if !allowed_methods.is_empty() {
            if browser_mode {
                let endpoint_price = if let Some(first_method) = allowed_methods.first() {
                    state
                        .db
                        .get_endpoint(api.id, first_method, &resolved_path)
                        .await
                        .ok()
                        .flatten()
                        .filter(|ep| ep.payment_required)
                        .map(|ep| ep.price)
                } else {
                    None
                };
                return Err(branded_status_page(
                    StatusCode::METHOD_NOT_ALLOWED,
                    "This endpoint uses a different method",
                    "Open this URL with one of the allowed methods below to execute the provider endpoint.",
                    &api.slug,
                    requested_path,
                    method,
                    &allowed_methods,
                    &[("Gateway", "gateway.apiosk.com".to_string())],
                    endpoint_price,
                ));
            }

            let body = serde_json::json!({
                "error": "method_not_allowed",
                "message": "This endpoint is configured for different HTTP methods",
                "api_slug": api.slug,
                "path": requested_path,
                "requested_method": method.to_string(),
                "allowed_methods": allowed_methods
            });
            let response = Response::builder()
                .status(StatusCode::METHOD_NOT_ALLOWED)
                .header("Content-Type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            return Err(response);
        }
    }

    // Resell listings (listing_metadata.resell.enabled) let the gateway pay the
    // upstream's own x402 on forward. Carry the agent-facing listing price (in
    // atomic USDC) as the spread ceiling the resell leg may pay the upstream;
    // None (any non-resell listing, or a resell row missing a priced endpoint)
    // disables the resell leg entirely.
    let resell = listing
        .as_ref()
        .filter(|l| {
            l.listing_metadata
                .get("resell")
                .and_then(|r| r.get("enabled"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        })
        .and(endpoint.as_ref())
        .and_then(|ep| super::resell::usd_to_atomic_u128(&ep.price));

    let (mut preset_auth_headers, mut preset_auth_query) =
        select_preset_auth(endpoint.as_ref(), preset_auth_headers, preset_auth_query);
    if api.slug == super::apimarket::SOURCE {
        let origin = endpoint.as_ref().and_then(|e| e.origin_override.as_deref())
            .or(api.endpoint_url.as_deref()).unwrap_or(&api.origin_url);
        preset_auth_headers = if super::crunchbase::valid_route(origin, resolved_method.as_str(), &resolved_path) {
            super::crunchbase::credentials(state.db.worker_pool()).await
        } else {
            super::apimarket::credentials(state.db.worker_pool(), origin, resolved_method.as_str(), &resolved_path).await
        }
            .map_err(|code| json_error_response(StatusCode::SERVICE_UNAVAILABLE, code, "Crunchbase is temporarily unavailable or needs reconnection."))?;
        preset_auth_query.clear();
    }

    Ok(ResolvedProxyRequest {
        api,
        listing,
        listing_type,
        preset_auth_headers,
        preset_auth_query,
        endpoint,
        virtual_execute_request,
        resolved_method,
        resolved_path,
        resell,
    })
}

/// Upstream credentials to inject, as `(name, value)` pairs.
pub(super) type PresetAuth = Vec<(String, String)>;

/// Which upstream credentials a resolved endpoint gets.
///
/// An endpoint carrying its own credentials (`api_endpoints.auth_override`,
/// same `{auth_headers, auth_query}` shape as `listing_metadata`) uses ONLY
/// those. An endpoint forwarding to an override origin without its own
/// credentials gets none — a listing credential never crosses to an override
/// host (KVK: the private subscription key must not reach opendata.kvk.nl).
/// Everything else keeps listing-level auth exactly as before.
pub(super) fn select_preset_auth(
    endpoint: Option<&crate::db::Endpoint>,
    listing_headers: PresetAuth,
    listing_query: PresetAuth,
) -> (PresetAuth, PresetAuth) {
    match endpoint {
        Some(ep) => {
            if let Some(auth) = ep.auth_override.as_ref() {
                (
                    extract_preset_auth_headers(auth),
                    extract_preset_auth_query(auth),
                )
            } else if ep.origin_override.is_some() {
                (Vec::new(), Vec::new())
            } else {
                (listing_headers, listing_query)
            }
        }
        None => (listing_headers, listing_query),
    }
}

/// Apply the per-buyer / per-IP rate limit before we do any expensive work
/// upstream. `connect_token_id` (if present) is the strongest identity we
/// have; otherwise we fall back to the `client_id` hash that
/// `traffic::track_request` derives from IP + user-agent.
pub(super) async fn enforce_rate_limit(
    state: &Arc<AppState>,
    api_slug: &str,
    browser_mode: bool,
    tracking_context: &TrackingContext,
    connect_token_id: Option<Uuid>,
) -> Option<Response<Body>> {
    use crate::rate_limit::{RateLimitDecision, DEFAULT_PER_IP_POLICY, DEFAULT_PER_TOKEN_POLICY};

    // No identity at all — skip rate limiting rather than reject. Health
    // checks and warm-up requests fall through here.
    let (key, policy) = match (connect_token_id, tracking_context.client_id.as_ref()) {
        (Some(token_id), _) => (
            format!("token:{}:{}", token_id.simple(), api_slug),
            DEFAULT_PER_TOKEN_POLICY,
        ),
        (None, Some(client_id)) => (
            format!("client:{}:{}", client_id, api_slug),
            DEFAULT_PER_IP_POLICY,
        ),
        (None, None) => return None,
    };

    match state.rate_limiter.check(&key, policy).await {
        RateLimitDecision::Allowed { .. } => None,
        RateLimitDecision::Throttled { retry_after } => {
            warn!(
                "Rate limit exceeded for {} on {} — replying 429",
                key, api_slug
            );
            let retry_secs = retry_after.as_secs().max(1).to_string();
            if browser_mode {
                Some(branded_status_page(
                    StatusCode::TOO_MANY_REQUESTS,
                    "Rate limit exceeded",
                    "Too many requests for this connection. Wait a moment and try again.",
                    api_slug,
                    "",
                    &Method::GET,
                    &[],
                    &[("Retry-After", retry_secs)],
                    None,
                ))
            } else {
                let body = serde_json::json!({
                    "error": "rate_limited",
                    "message": "Too many requests for this connection on this API.",
                    "retry_after": retry_after.as_secs(),
                });
                let response = Response::builder()
                    .status(StatusCode::TOO_MANY_REQUESTS)
                    .header("Content-Type", "application/json")
                    .header("Retry-After", retry_secs)
                    .body(Body::from(body.to_string()))
                    .unwrap_or_else(|_| Response::new(Body::from(r#"{"error":"rate_limited"}"#)));
                Some(response)
            }
        }
    }
}

fn listing_execution_disabled(listing: Option<&crate::listing::Listing>) -> bool {
    let Some(metadata) = listing.map(|listing| &listing.listing_metadata) else {
        return false;
    };

    metadata
        .get("execution_enabled")
        .and_then(serde_json::Value::as_bool)
        == Some(false)
        || metadata
            .get("catalog_state")
            .and_then(serde_json::Value::as_str)
            == Some("browse_only")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The answer to a duplicate of a call still running: 409, the
    /// `in_progress` code the edge function keys on to keep its reservation
    /// and its key, and a `Retry-After` so a client that honours one waits
    /// rather than hammers. Nothing about it depends on state, which is why it
    /// can be checked without a database.
    #[tokio::test]
    async fn a_duplicate_in_flight_is_409_in_progress_with_retry_after() {
        let response = in_progress_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok()),
            Some("2")
        );
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(body["error"], "in_progress");
        assert!(body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("same key"));
    }

    #[test]
    fn browse_only_listing_cannot_execute() {
        let listing = crate::listing::Listing {
            id: uuid::Uuid::nil(),
            slug: "browse-only".to_string(),
            origin_url: "https://example.com".to_string(),
            wallet_address: "0x0000000000000000000000000000000000000000".to_string(),
            status: "active".to_string(),
            owner_id: uuid::Uuid::nil(),
            commission_rate: bigdecimal::BigDecimal::from(15),
            listing_type: crate::listing::ListingType::Api,
            is_platform_owned: true,
            listing_metadata: serde_json::json!({
                "catalog_state": "browse_only",
                "execution_enabled": false
            }),
        };

        assert!(listing_execution_disabled(Some(&listing)));
    }

    #[test]
    fn normal_listing_can_execute() {
        let listing = crate::listing::Listing {
            id: uuid::Uuid::nil(),
            slug: "ready".to_string(),
            origin_url: "https://example.com".to_string(),
            wallet_address: "0x0000000000000000000000000000000000000000".to_string(),
            status: "active".to_string(),
            owner_id: uuid::Uuid::nil(),
            commission_rate: bigdecimal::BigDecimal::from(15),
            listing_type: crate::listing::ListingType::Api,
            is_platform_owned: true,
            listing_metadata: serde_json::json!({"execution_enabled": true}),
        };

        assert!(!listing_execution_disabled(Some(&listing)));
        assert!(!listing_execution_disabled(None));
    }
}
