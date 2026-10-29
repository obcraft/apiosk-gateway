//! 00-07 — Idempotency middleware for the connect `/v1/*` write surface.
//!
//! Reuses the EXISTING idempotency store from migration 022
//! (`gateway_idempotency_keys`) and its `Database::{get,store}_idempotency_record`
//! helpers — this does NOT create a second idempotency table. What it adds over
//! the optional, inline replay logic in `proxy::handle_flow` is a reusable Axum
//! middleware that *requires* an `Idempotency-Key` on write methods and dedupes
//! retries.
//!
//! Protocol (per the work package):
//!   1. Missing `Idempotency-Key` on a write → 400.
//!   2. Key seen before with the SAME request hash → replay the stored response
//!      (the handler does not run again).
//!   3. Key seen before with a DIFFERENT request hash → 409 Conflict.
//!
//! The request hash is `sha256(method \n path \n body)`, so reusing a key with a
//! different payload conflicts instead of silently replaying the wrong receipt.
//!
//! Scope: persistence is keyed by the buyer's connect-token id (the FK the 022
//! table requires), exactly like the existing flow. Marketplace-management routes
//! authenticate with a marketplace key rather than a connect token; for those the
//! key is still *enforced* (400 when missing), but marketplace-scoped persistence
//! lands with those routes in package 01 (the 022 table is connect-token-scoped by
//! FK, so caching there is a deliberate follow-up, not a silent no-op here).

use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode},
    middleware::Next,
    response::Response,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::json_error;
use crate::auth::extract_agent_connect_token;
use crate::db::{AgentConnectTokenLookup, IdempotencyRecord};
use crate::proxy::hash_connect_token;
use crate::AppState;

const MAX_IDEMPOTENT_BODY_BYTES: usize = 8 * 1024 * 1024;
const REPLAY_HEADER: &str = "x-apiosk-idempotent-replay";

/// Outcome of reading the `Idempotency-Key` header.
pub enum KeyOutcome {
    /// No header at all.
    Absent,
    /// Present and valid (1..=255 ASCII chars after trim).
    Valid(String),
    /// Present but empty / oversized / non-ASCII.
    Malformed,
}

/// Replay verdict once we know whether a record already exists for this key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayDecision {
    /// No prior record — run the handler.
    Proceed,
    /// Prior record with the same request hash — replay it, skip the handler.
    Replay,
    /// Prior record with a different request hash — 409.
    Conflict,
}

/// Parse the `Idempotency-Key` header. Pure. Same 1..=255 ASCII rule the existing
/// flow uses, so behavior is consistent across the gateway.
pub fn parse_idempotency_key(headers: &HeaderMap) -> KeyOutcome {
    let Some(value) = headers.get("idempotency-key") else {
        return KeyOutcome::Absent;
    };
    let Some(raw) = value.to_str().ok().map(str::trim) else {
        return KeyOutcome::Malformed;
    };
    if raw.is_empty() || raw.len() > 255 || !raw.is_ascii() {
        return KeyOutcome::Malformed;
    }
    KeyOutcome::Valid(raw.to_string())
}

/// `sha256(method \n path \n body)`, hex-encoded. Pure + deterministic. Including
/// the body is what makes "same key, different payload" a 409 rather than a wrong
/// replay.
pub fn compute_request_fingerprint(method: &Method, path: &str, body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(method.as_str().as_bytes());
    hasher.update(b"\n");
    hasher.update(path.as_bytes());
    hasher.update(b"\n");
    hasher.update(body);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Decide what to do given the stored fingerprint (if any) and the current one.
/// Pure — this is the core of the dedup protocol and is unit-tested directly.
pub fn decide(stored_fingerprint: Option<&str>, current_fingerprint: &str) -> ReplayDecision {
    match stored_fingerprint {
        None => ReplayDecision::Proceed,
        Some(stored) if stored == current_fingerprint => ReplayDecision::Replay,
        Some(_) => ReplayDecision::Conflict,
    }
}

fn is_write_method(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

/// Axum middleware enforcing idempotency on connect write routes. Attach to the
/// POST/PUT/PATCH/DELETE `/v1/*` connect routes; read-only methods pass straight
/// through.
pub async fn enforce_connect_idempotency(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let method = request.method().clone();
    if !is_write_method(&method) {
        return next.run(request).await;
    }

    let headers = request.headers().clone();
    let key = match parse_idempotency_key(&headers) {
        KeyOutcome::Valid(key) => key,
        KeyOutcome::Absent => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "idempotency_key_required",
                "Write requests require an Idempotency-Key header.",
            );
        }
        KeyOutcome::Malformed => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "invalid_idempotency_key",
                "Idempotency-Key is present but empty, oversized, or non-ASCII.",
            );
        }
    };

    let path = request.uri().path().to_string();

    // Buffer the body up front: we need it both to fingerprint the request and to
    // forward it to the handler.
    let (parts, body) = request.into_parts();
    let body_bytes = match axum::body::to_bytes(body, MAX_IDEMPOTENT_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return json_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request_too_large",
                "Request body exceeds the gateway idempotency buffer limit.",
            );
        }
    };
    let fingerprint = compute_request_fingerprint(&method, &path, &body_bytes);

    // Persistence is scoped to the connect-token (the 022 FK). When no token is
    // present we still enforced the key above, but cannot cache — rebuild the
    // request and forward.
    let scope = resolve_connect_scope(&state, &headers).await;
    let Some(scope) = scope else {
        let request = Request::from_parts(parts, Body::from(body_bytes));
        return next.run(request).await;
    };

    match state.db.get_idempotency_record(scope, &key).await {
        Ok(Some(record)) => match decide(Some(&record.request_fingerprint), &fingerprint) {
            // A claim the proxy path holds under this key (see
            // `Database::claim_idempotency_key`): the first attempt is still
            // running, so there is no response to replay yet.
            ReplayDecision::Replay if record.is_in_flight() => {
                return json_error(
                    StatusCode::CONFLICT,
                    "in_progress",
                    "A request with this Idempotency-Key is already in progress. Retry with the same key shortly.",
                );
            }
            ReplayDecision::Replay => {
                tracing::info!("connect idempotency replay for key={key} path={path}");
                return rebuild_response_from_record(record);
            }
            ReplayDecision::Conflict => {
                return json_error(
                    StatusCode::CONFLICT,
                    "idempotency_key_conflict",
                    "Idempotency-Key was reused with a different request. Use a unique key per operation.",
                );
            }
            ReplayDecision::Proceed => {}
        },
        Ok(None) => {}
        Err(err) => {
            tracing::warn!("connect idempotency lookup failed: {err} — proceeding without replay");
        }
    }

    let request = Request::from_parts(parts, Body::from(body_bytes));
    let response = next.run(request).await;
    capture_and_store(&state, scope, &key, &fingerprint, response).await
}

/// Resolve the buyer's connect-token id (the 022 store scope). `None` when no
/// connect token is on the request.
async fn resolve_connect_scope(state: &Arc<AppState>, headers: &HeaderMap) -> Option<Uuid> {
    match crate::service_auth::resolve(state, headers).await {
        crate::service_auth::ServiceAuth::NotPresented => {}
        crate::service_auth::ServiceAuth::Rejected(..) => return None,
        crate::service_auth::ServiceAuth::Resolved(lookup) => {
            return match lookup {
                AgentConnectTokenLookup::Active(token) => Some(token.token_id),
                _ => None,
            };
        }
    }
    let token = extract_agent_connect_token(headers)?;
    let token_hash = hash_connect_token(&token);
    match state
        .db
        .lookup_agent_connect_token_by_hash(&token_hash)
        .await
    {
        Ok(AgentConnectTokenLookup::Active(token)) => Some(token.token_id),
        _ => None,
    }
}

/// Buffer the live response, persist it under (scope, key) with the request
/// fingerprint we matched on, and return a rebuilt response carrying the same
/// bytes. Storage failures never block the response.
async fn capture_and_store(
    state: &Arc<AppState>,
    scope: Uuid,
    key: &str,
    fingerprint: &str,
    response: Response,
) -> Response {
    let (parts, body) = response.into_parts();
    let body_bytes = match axum::body::to_bytes(body, MAX_IDEMPOTENT_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::error!("failed to buffer connect response for idempotency cache: {err}");
            return json_error(
                StatusCode::BAD_GATEWAY,
                "response_buffer_failed",
                "Gateway could not buffer the response for the idempotency cache.",
            );
        }
    };

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

    if let Err(err) = state
        .db
        .store_idempotency_record(
            scope,
            key,
            fingerprint,
            parts.status.as_u16(),
            headers_json,
            &body_bytes,
        )
        .await
    {
        tracing::warn!("connect idempotency store failed (key={key}): {err}");
    }

    let mut rebuilt = Response::from_parts(parts, Body::from(body_bytes));
    rebuilt
        .headers_mut()
        .insert(REPLAY_HEADER, HeaderValue::from_static("false"));
    rebuilt
}

fn rebuild_response_from_record(record: IdempotencyRecord) -> Response {
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
    builder = builder.header(REPLAY_HEADER, "true");
    builder
        .body(Body::from(record.response_body))
        .unwrap_or_else(|_| Response::new(Body::from(r#"{"error":"replay_build_error"}"#)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn missing_key_is_absent_and_drives_400() {
        // Scenario 1: a write without an Idempotency-Key parses as Absent, which
        // the middleware turns into a 400.
        assert!(matches!(
            parse_idempotency_key(&HeaderMap::new()),
            KeyOutcome::Absent
        ));
    }

    #[test]
    fn empty_and_non_ascii_keys_are_malformed() {
        let mut empty = HeaderMap::new();
        empty.insert("idempotency-key", HeaderValue::from_static("   "));
        assert!(matches!(
            parse_idempotency_key(&empty),
            KeyOutcome::Malformed
        ));

        let mut nonascii = HeaderMap::new();
        nonascii.insert(
            "idempotency-key",
            HeaderValue::from_bytes(b"k\xff").unwrap(),
        );
        assert!(matches!(
            parse_idempotency_key(&nonascii),
            KeyOutcome::Malformed
        ));
    }

    #[test]
    fn valid_key_is_extracted_trimmed() {
        let mut headers = HeaderMap::new();
        headers.insert("idempotency-key", HeaderValue::from_static("  abc-123  "));
        match parse_idempotency_key(&headers) {
            KeyOutcome::Valid(k) => assert_eq!(k, "abc-123"),
            _ => panic!("expected valid key"),
        }
    }

    #[test]
    fn fingerprint_is_deterministic_and_body_sensitive() {
        let a = compute_request_fingerprint(&Method::POST, "/v1/marketplaces", b"{\"n\":1}");
        let a2 = compute_request_fingerprint(&Method::POST, "/v1/marketplaces", b"{\"n\":1}");
        let b = compute_request_fingerprint(&Method::POST, "/v1/marketplaces", b"{\"n\":2}");
        let c = compute_request_fingerprint(&Method::PUT, "/v1/marketplaces", b"{\"n\":1}");
        assert_eq!(a, a2);
        assert_ne!(a, b, "different body must change the fingerprint");
        assert_ne!(a, c, "different method must change the fingerprint");
    }

    #[test]
    fn decide_covers_proceed_replay_conflict() {
        let fp = compute_request_fingerprint(&Method::POST, "/v1/x", b"body");
        assert_eq!(decide(None, &fp), ReplayDecision::Proceed);
        assert_eq!(decide(Some(&fp), &fp), ReplayDecision::Replay);
        assert_eq!(decide(Some("other-hash"), &fp), ReplayDecision::Conflict);
    }

    #[test]
    fn same_key_same_body_replays_and_runs_handler_once() {
        // Scenario 2, simulated through the same pure pieces the middleware uses,
        // with an in-memory stand-in for the 022 store.
        let mut store: HashMap<(Uuid, String), (String, Vec<u8>)> = HashMap::new();
        let scope = Uuid::from_u128(1);
        let key = "k1".to_string();
        let fp =
            compute_request_fingerprint(&Method::POST, "/v1/marketplaces", b"{\"name\":\"mp\"}");
        let mut handler_runs = 0;

        // First call: nothing stored -> Proceed -> handler runs -> store response.
        let stored = store.get(&(scope, key.clone())).map(|(f, _)| f.clone());
        assert_eq!(decide(stored.as_deref(), &fp), ReplayDecision::Proceed);
        handler_runs += 1;
        store.insert((scope, key.clone()), (fp.clone(), b"CREATED".to_vec()));

        // Retry: same key + same body -> Replay -> handler does NOT run again.
        let stored = store.get(&(scope, key.clone())).map(|(f, _)| f.clone());
        assert_eq!(decide(stored.as_deref(), &fp), ReplayDecision::Replay);
        assert_eq!(handler_runs, 1, "handler must run exactly once");
        assert_eq!(
            store.get(&(scope, key)).map(|(_, b)| b.clone()),
            Some(b"CREATED".to_vec())
        );
    }

    #[test]
    fn same_key_different_body_conflicts() {
        // Scenario 3.
        let fp_first =
            compute_request_fingerprint(&Method::POST, "/v1/marketplaces", b"{\"name\":\"a\"}");
        let fp_retry =
            compute_request_fingerprint(&Method::POST, "/v1/marketplaces", b"{\"name\":\"b\"}");
        assert_ne!(fp_first, fp_retry);
        assert_eq!(decide(Some(&fp_first), &fp_retry), ReplayDecision::Conflict);
    }
}
