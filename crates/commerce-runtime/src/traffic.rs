use std::{
    net::IpAddr,
    sync::{atomic::AtomicI64, Arc},
    time::Instant,
};

use axum::{
    body::Body,
    extract::State,
    http::{HeaderName, HeaderValue, Request},
    middleware::Next,
    response::Response,
};
use sha2::{Digest, Sha256};
use tokio::time::{timeout, Duration};
use tracing::{warn, Instrument};
use uuid::Uuid;

use crate::{
    db::GatewayRequestLogInsert,
    request_context::{RequestContext, REQUEST_ID_HEADER, SESSION_ID_HEADER},
    telemetry::{
        body::CountingBody,
        classify,
        event::{DiscoveryEvent, Outcome, RequestKind, RequestRecord, TelemetryEvent},
        geo, sanitize, TelemetryOutcome,
    },
    AppState,
};

const PRESENCE_LOOKUP_TIMEOUT: Duration = Duration::from_millis(250);

pub async fn track_request(
    State(state): State<Arc<AppState>>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let started = Instant::now();
    let method = request.method().to_string();
    let path = request.uri().path().to_string();
    let query_string = sanitize_query(request.uri().query());
    // The whole /.well-known namespace is discovery-crawler traffic
    // (Bazaar, Agent402, agent registries poll it) — keep it out of the
    // request log so it doesn't drown real API activity.
    let should_skip_db_tracking = path == "/.well-known"
        || path.starts_with("/.well-known/")
        || matches!(
            path.as_str(),
            "/" | "/health"
                | "/favicon.ico"
                | "/favicon.svg"
                | "/favicon-dark.ico"
                | "/favicon-dark-96x96.png"
                | "/icon-maskable-512.png"
                | "/favicon-96x96.png"
                | "/apple-touch-icon.png"
                | "/site.webmanifest"
                | "/web-app-manifest-192x192.png"
                | "/web-app-manifest-512x512.png"
                | "/logo.svg"
                | "/logo.png"
                | "/llms.txt"
                | "/openapi.json"
        );
    let headers = request.headers().clone();

    // Telemetry v2 (§Phase 2) — disabled by default. When off, `telemetry_on`
    // stays false and none of the capture/wrap/emit code below runs, so
    // behaviour is byte-identical. Capture the URI-derived fields now, before
    // `request` is consumed by `next.run`.
    let telemetry_on = state.telemetry.is_enabled();
    let (tel_raw_query, tel_http_version, tel_uri_scheme, tel_occurred_at) = if telemetry_on {
        (
            request.uri().query().map(|q| q.to_string()),
            Some(format!("{:?}", request.version())),
            request.uri().scheme_str().map(|s| s.to_string()),
            Some(chrono::Utc::now()),
        )
    } else {
        (None, None, None, None)
    };

    let request_id = headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| Uuid::parse_str(value.trim()).ok())
        .unwrap_or_else(Uuid::new_v4);
    let presence = if should_skip_db_tracking {
        None
    } else {
        match headers
            .get(SESSION_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| Uuid::parse_str(value.trim()).ok())
        {
            Some(session_id) => match timeout(
                PRESENCE_LOOKUP_TIMEOUT,
                state.db.get_presence_session_context(session_id),
            )
            .await
            {
                Ok(Ok(session)) => session,
                Ok(Err(err)) => {
                    warn!("Failed to resolve session context for request log: {}", err);
                    None
                }
                Err(_) => {
                    warn!(
                        "Timed out resolving session context for request log on {} {}",
                        method, path
                    );
                    None
                }
            },
            None => None,
        }
    };

    request.extensions_mut().insert(RequestContext {
        request_id,
        presence: presence.clone(),
    });

    // Telemetry v2: classify the request (path-only, so it's stable before the
    // response) and, for spine-eligible traffic, wrap the REQUEST body with a
    // pass-through byte counter. Discovery / health / assets / the root page are
    // not spine rows — discovery goes to its own cheaper table below.
    let is_api_route_pre =
        path.starts_with("/v1/") || path.starts_with("/types") || path.starts_with("/health");
    let tel_kind = if telemetry_on {
        Some(classify::classify_request_kind(&path, is_api_route_pre))
    } else {
        None
    };
    let tel_spine = telemetry_on
        && path != "/"
        && matches!(
            tel_kind,
            Some(RequestKind::Api | RequestKind::GatewayApi | RequestKind::Checkout)
        );
    let tel_request_bytes = Arc::new(AtomicI64::new(0));
    if tel_spine {
        let (parts, body) = request.into_parts();
        let counted = CountingBody::new(body, tel_request_bytes.clone());
        request = Request::from_parts(parts, Body::new(counted));
    }

    // Wrap the rest of the request in a tracing span so every info!/warn!/
    // error! emitted by deeper handlers carries request_id, method, path
    // and (when known) api_slug. JSON output ships these as top-level keys
    // alongside the message, which makes log filters in Fly / Sentry an
    // exact-match search.
    let provisional_api_slug = path
        .trim_start_matches('/')
        .split('/')
        .next()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let span = tracing::info_span!(
        "gateway_request",
        request_id = %request_id,
        method = %method,
        path = %path,
        api_slug = provisional_api_slug.as_deref().unwrap_or("")
    );
    let mut response = next.run(request).instrument(span).await;
    if let Ok(header_value) = HeaderValue::from_str(&request_id.to_string()) {
        response
            .headers_mut()
            .insert(HeaderName::from_static(REQUEST_ID_HEADER), header_value);
    }

    let latency_ms = started.elapsed().as_millis() as i64;
    let status_code = response.status().as_u16() as i32;
    let response_bytes = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok());
    let is_api_route =
        path.starts_with("/v1/") || path.starts_with("/types") || path.starts_with("/health");
    let api_slug = if is_api_route {
        None
    } else {
        path.trim_start_matches('/')
            .split('/')
            .next()
            .filter(|s| !s.is_empty())
    };
    let ip_address = client_ip_from_headers(&headers);
    let user_agent = headers.get("user-agent").and_then(|v| v.to_str().ok());
    let origin = headers.get("origin").and_then(|v| v.to_str().ok());
    let referrer = headers.get("referer").and_then(|v| v.to_str().ok());
    let ip_hash = ip_address.as_deref().map(hash_ip);
    let request_headers = redacted_request_headers(&headers);
    let error_code = error_code_for_status(status_code);

    // Discovery documents are excluded from the request log above (crawler
    // volume would drown real API activity) but they are the surface agents
    // actually read, so they get their own aggregated counters instead of
    // nothing at all. Buffered in-process; this call never touches the DB.
    state
        .discovery
        .record_read(&path, response.status(), &headers, ip_hash.as_deref());

    if !should_skip_db_tracking {
        state.db.log_request_event(GatewayRequestLogInsert {
            request_id,
            session_id: presence.as_ref().map(|ctx| ctx.session_id),
            managed_wallet_id: presence.as_ref().and_then(|ctx| ctx.managed_wallet_id),
            client_id: presence.as_ref().map(|ctx| ctx.client_id.as_str()),
            method: &method,
            path: &path,
            query_string: query_string.as_deref(),
            status_code,
            latency_ms,
            api_slug,
            is_api_route,
            user_agent,
            ip_hash: ip_hash.as_deref(),
            origin,
            referrer,
            request_headers: &request_headers,
            response_bytes,
            error_code,
        });
    }

    if let (Some(measurement_id), Some(api_secret)) = (
        state.config.ga_measurement_id.clone(),
        state.config.ga_api_secret.clone(),
    ) {
        if path != "/health" && path != "/favicon.ico" && path != "/logo.png" {
            let client_id = build_client_id(ip_address.as_deref(), user_agent);
            let ga_url = format!(
                "https://www.google-analytics.com/mp/collect?measurement_id={}&api_secret={}",
                measurement_id, api_secret
            );
            let method_for_ga = method.clone();
            let path_for_ga = path.clone();
            let api_slug_for_ga = api_slug.map(|s| s.to_string());
            let user_agent_for_ga = user_agent.map(|s| s.to_string());
            let referrer_for_ga = referrer.map(|s| s.to_string());
            let http_client = state.http_client.clone();

            tokio::spawn(async move {
                let payload = serde_json::json!({
                    "client_id": client_id,
                    "non_personalized_ads": true,
                    "events": [
                        {
                            "name": "gateway_request",
                            "params": {
                                "engagement_time_msec": latency_ms.max(1),
                                "method": method_for_ga,
                                "path": path_for_ga,
                                "status_code": status_code,
                                "latency_ms": latency_ms,
                                "is_api_route": is_api_route,
                                "api_slug": api_slug_for_ga,
                                "user_agent": user_agent_for_ga,
                                "referrer": referrer_for_ga,
                            }
                        }
                    ]
                });

                if let Err(err) = http_client.post(ga_url).json(&payload).send().await {
                    warn!("Failed to send GA Measurement Protocol event: {}", err);
                }
            });
        }
    }

    // -----------------------------------------------------------------------
    // Telemetry v2 emission (§Phase 2). Entirely additive: the old
    // gateway_request_logs write + GA send above are untouched. Spine rows wrap
    // the response body so the completion callback records exact response_bytes
    // + client-disconnect and enqueues ONE append-only row; discovery traffic
    // (previously discarded) goes to its own cheaper table inline.
    // -----------------------------------------------------------------------
    if tel_spine {
        let status_u16 = response.status().as_u16();
        let kind = tel_kind.unwrap_or(RequestKind::Api);
        let hget = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        };

        // Fly-Client-IP is injected by Fly Proxy. Fall back to the configured
        // trusted-proxy resolver for deployments behind another proxy.
        let (tel_ip, tel_ip_trusted) = match state.telemetry.trusted_proxies() {
            Some(tp) => {
                if let Some(ip) = header_ip(&headers, "fly-client-ip") {
                    (Some(ip), true)
                } else {
                    let xff = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok());
                    let xreal = headers.get("x-real-ip").and_then(|v| v.to_str().ok());
                    let r = sanitize::resolve_client_ip(xff, xreal, None, tp);
                    (r.ip.map(|ip| ip.to_string()), r.trusted)
                }
            }
            None => (None, false),
        };

        let agent = sanitize::parse_agent(user_agent);
        let sq = sanitize::sanitize_query(tel_raw_query.as_deref());

        // Coarse geography from the Fly edge PoP (or a CDN country header, if
        // one ever fronts us). Gated on TELEMETRY_GEO_ENABLED; see geo.rs for
        // why this is edge-derived and not IP geolocation.
        let tel_geo = match state.telemetry.settings() {
            Some(s) if s.geo_enabled => geo::enrich(&headers),
            _ => geo::GeoInfo::default(),
        };

        // Optional handler enrichment via a response extension (non-invasive
        // seam — handlers MAY set a precise stage/code, else infer from status).
        let enrich = response.extensions().get::<TelemetryOutcome>().cloned();
        let mut outcome = classify::classify_outcome(status_u16, kind, false);
        let mut failure_stage = classify::failure_stage_for_status(status_u16);
        let mut error_code = None;
        let mut retryable = None;
        if let Some(e) = enrich {
            if let Some(o) = e.outcome {
                outcome = o;
            }
            if e.failure_stage.is_some() {
                failure_stage = e.failure_stage;
            }
            error_code = e.error_code;
            retryable = e.retryable;
        }

        let response_content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let record = RequestRecord {
            request_id,
            trace_id: hget("x-trace-id").and_then(|s| Uuid::parse_str(s.trim()).ok()),
            parent_request_id: hget("x-parent-request-id")
                .and_then(|s| Uuid::parse_str(s.trim()).ok()),
            occurred_at: tel_occurred_at,
            completed_at: None,
            method: method.clone(),
            host: hget("host"),
            path: path.clone(),
            route_template: None,
            query_keys: sq.keys,
            http_version: tel_http_version.clone(),
            scheme: tel_uri_scheme.clone().or_else(|| hget("x-forwarded-proto")),
            api_id: None,
            endpoint_id: None,
            api_slug: api_slug.map(|s| s.to_string()),
            request_kind: Some(kind),
            status_code: Some(status_u16 as i16),
            outcome: Some(outcome),
            failure_stage,
            error_code,
            retryable,
            latency_ms: Some(latency_ms),
            request_bytes: Some(tel_request_bytes.load(std::sync::atomic::Ordering::Relaxed)),
            response_bytes: None,
            request_content_type: hget("content-type"),
            response_content_type,
            time_to_first_byte_ms: Some(latency_ms),
            client_disconnected: None,
            ip_address: tel_ip,
            country_code: tel_geo.country_code,
            region: tel_geo.region,
            // ASN needs an IP→network dataset we deliberately don't ship yet
            // (see geo.rs "upgrade path"); the edge PoP carries the geography.
            asn: None,
            network_name: tel_geo.network_name,
            network_type: tel_geo.network_type,
            user_agent: user_agent.map(|s| s.to_string()),
            agent_name: agent.agent_name,
            agent_version: agent.agent_version,
            agent_framework: agent.agent_framework,
            sdk_name: agent.sdk_name,
            sdk_version: agent.sdk_version,
            client_kind: agent.client_kind,
            user_id: None,
            session_id: presence.as_ref().map(|p| p.session_id),
            client_id: presence.as_ref().map(|p| p.client_id.clone()),
            managed_wallet_id: presence.as_ref().and_then(|p| p.managed_wallet_id),
            agent_connect_token_id: None,
            wallet_address: None,
            referrer: referrer.map(|s| s.to_string()),
            origin: hget("origin"),
            metadata: serde_json::json!({
                "query_redacted_count": sq.redacted_count,
                "ip_trusted": tel_ip_trusted,
                "is_api_route": is_api_route,
            }),
        };

        let (parts, body) = response.into_parts();
        let telem = state.telemetry.clone();
        let resp_counter = Arc::new(AtomicI64::new(0));
        let counted = CountingBody::new(body, resp_counter).on_done(move |bytes, ended_ok| {
            let mut rec = record;
            rec.response_bytes = Some(bytes);
            rec.client_disconnected = Some(!ended_ok);
            rec.completed_at = Some(chrono::Utc::now());
            if !ended_ok {
                rec.outcome = Some(Outcome::ClientDisconnected);
            }
            telem.emit(TelemetryEvent::Request(Box::new(rec)));
        });
        response = Response::from_parts(parts, Body::new(counted));
    } else if telemetry_on && matches!(tel_kind, Some(RequestKind::Discovery)) {
        // Discovery / agent-manifest traffic — sampled, lightweight, own table.
        if state.telemetry.should_sample_discovery(request_id) {
            let resp_bytes = response
                .headers()
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<i64>().ok());
            let client_fp = state.telemetry.fingerprint(
                format!(
                    "{}|{}",
                    ip_address.as_deref().unwrap_or(""),
                    user_agent.unwrap_or("")
                )
                .as_bytes(),
            );
            state
                .telemetry
                .emit(TelemetryEvent::Discovery(Box::new(DiscoveryEvent {
                    request_id,
                    occurred_at: tel_occurred_at,
                    resource: Some(path.clone()),
                    method: Some(method.clone()),
                    status_code: Some(status_code as i16),
                    ip_address: ip_address.clone(),
                    client_fingerprint: client_fp,
                    user_agent: user_agent.map(|s| s.to_string()),
                    crawler_class: classify::classify_crawler(user_agent),
                    cache_result: None,
                    latency_ms: Some(latency_ms),
                    response_bytes: resp_bytes,
                    metadata: serde_json::json!({}),
                })));
        }
    }

    response
}

pub(crate) fn hash_ip(ip: &str) -> String {
    // A deployment-specific salt prevents hashes from being comparable across
    // installations. Rotate it when old caller correlation is no longer needed.
    let salt = std::env::var("IP_HASH_SALT").unwrap_or_else(|_| "apiosk-gateway".into());
    let mut digest = Sha256::new();
    digest.update(salt.as_bytes());
    digest.update(b"|");
    digest.update(ip.as_bytes());
    hex::encode(digest.finalize())
}

fn sanitize_query(raw: Option<&str>) -> Option<String> {
    let raw = raw.filter(|q| !q.is_empty())?;
    let sensitive = [
        "key",
        "token",
        "secret",
        "password",
        "signature",
        "authorization",
        "api_key",
        "apikey",
    ];
    let encoded = url::form_urlencoded::parse(raw.as_bytes())
        .map(|(key, value)| {
            let lower = key.to_ascii_lowercase();
            let value = if sensitive.iter().any(|needle| lower.contains(needle)) {
                std::borrow::Cow::Borrowed("[REDACTED]")
            } else {
                value
            };
            (key, value)
        })
        .collect::<Vec<_>>();
    let result = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(encoded)
        .finish();
    Some(result.chars().take(2048).collect())
}

fn redacted_request_headers(headers: &axum::http::HeaderMap) -> serde_json::Value {
    const ALLOWED: &[&str] = &[
        "accept",
        "content-type",
        "origin",
        "referer",
        "user-agent",
        "x-request-id",
        "x-apiosk-agent-id",
    ];
    let mut out = serde_json::Map::new();
    for name in ALLOWED {
        if let Some(value) = headers.get(*name).and_then(|v| v.to_str().ok()) {
            out.insert(
                (*name).into(),
                serde_json::Value::String(value.chars().take(512).collect()),
            );
        }
    }
    // Make redaction explicit in the trace without retaining credential values.
    for name in [
        "authorization",
        "cookie",
        "x-api-key",
        "payment-signature",
        "x-payment",
    ] {
        if headers.contains_key(name) {
            out.insert(name.into(), serde_json::Value::String("[REDACTED]".into()));
        }
    }
    serde_json::Value::Object(out)
}

fn error_code_for_status(status: i32) -> Option<&'static str> {
    match status {
        400 => Some("BAD_REQUEST"),
        401 => Some("UNAUTHORIZED"),
        402 => Some("PAYMENT_REQUIRED"),
        403 => Some("FORBIDDEN"),
        404 => Some("NOT_FOUND"),
        429 => Some("RATE_LIMITED"),
        500..=599 => Some("UPSTREAM_OR_GATEWAY_ERROR"),
        _ => None,
    }
}

fn build_client_id(ip_address: Option<&str>, user_agent: Option<&str>) -> String {
    let fingerprint = format!(
        "{}|{}",
        ip_address.unwrap_or("unknown-ip"),
        user_agent.unwrap_or("unknown-ua")
    );
    let h1 = stable_hash(&fingerprint);
    let h2 = stable_hash(&(fingerprint + "|apiosk-gateway"));
    format!("{:016x}.{:016x}", h1, h2)
}

fn stable_hash(input: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in input.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn parse_ip_from_header(raw: &str) -> Option<String> {
    let candidate = raw.trim().trim_matches('"');
    if candidate.is_empty() {
        return None;
    }

    if let Ok(ip) = candidate.parse::<IpAddr>() {
        return Some(ip.to_string());
    }

    // Handle bracketed IPv6 with optional port: [::1]:1234
    if let Some(stripped) = candidate
        .strip_prefix('[')
        .and_then(|s| s.split(']').next())
    {
        if let Ok(ip) = stripped.parse::<IpAddr>() {
            return Some(ip.to_string());
        }
    }

    // Handle IPv4 with port: 203.0.113.42:1234
    if candidate.contains('.') && candidate.matches(':').count() == 1 {
        if let Some((host, _port)) = candidate.rsplit_once(':') {
            if let Ok(ip) = host.parse::<IpAddr>() {
                return Some(ip.to_string());
            }
        }
    }

    None
}

fn header_ip(headers: &axum::http::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .and_then(parse_ip_from_header)
}

pub(crate) fn client_ip_from_headers(headers: &axum::http::HeaderMap) -> Option<String> {
    header_ip(headers, "fly-client-ip")
        .or_else(|| header_ip(headers, "x-forwarded-for"))
        .or_else(|| header_ip(headers, "x-real-ip"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    #[test]
    fn client_ip_prefers_fly_proxy_header() {
        let mut headers = HeaderMap::new();
        headers.insert("fly-client-ip", HeaderValue::from_static("203.0.113.9"));
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.4, 10.0.0.1"),
        );

        assert_eq!(
            client_ip_from_headers(&headers),
            Some("203.0.113.9".to_string())
        );
    }

    #[test]
    fn client_ip_falls_back_to_forwarded_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.4, 10.0.0.1"),
        );

        assert_eq!(
            client_ip_from_headers(&headers),
            Some("198.51.100.4".to_string())
        );
    }
}
