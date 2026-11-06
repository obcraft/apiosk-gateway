//! `POST /v1/admin/discovery-sources/probe` — what does this source actually
//! return?
//!
//! The `discovery_sources` table (migration 078) lets an admin change what
//! `/v1/discover` sweeps without a deploy. That is the point of it, and also
//! its hazard: a source added with the wrong dialect parses silently to zero,
//! and a source that died last week goes on looking healthy in the list. The
//! only place that knows the difference is the sweep's own parser, so this
//! endpoint runs that parser on demand and reports what it saw.
//!
//! Everything the probe reports comes from [`crate::sources::probe`], which
//! shares the parsers with the live sweep. Nothing here re-implements a
//! dialect: a probe that agreed with its own copy of the rules and disagreed
//! with production would be worse than no probe at all.
//!
//! Admin-only. The admin portal's writes are gated in Postgres by
//! `is_active_platform_admin()`; this runs outside Postgres, so it authenticates
//! the portal session against Supabase ([`crate::portal_auth`]) and then asks
//! the same table. It is also the one admin surface that makes the gateway
//! fetch a URL a human just typed, so the shared SSRF guard runs on it before
//! any request leaves the box.

use std::sync::Arc;

use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde::Deserialize;

use crate::auth::extract_credits_user_jwt;
use crate::portal_auth::{resolve_portal_user_id, PortalAuthError};
use crate::http::{json as json_response, refusal as error, throttled_by_ip};
use crate::rate_limit::RateLimitPolicy;
use crate::AppState;

/// Every probe is one outbound request to somebody else's server, fired by a
/// button. Generous enough that working through a list of sources never trips
/// it, low enough that a stuck retry loop is not a small denial-of-service
/// against an index we depend on.
const PROBE_RATE_LIMIT: RateLimitPolicy = RateLimitPolicy::per_minute(20);

#[derive(Debug, Default, Deserialize)]
struct ProbeRequest {
    url: Option<String>,
    shape: Option<String>,
    fetch_kind: Option<String>,
    /// Phrasing to send to a queried source, and to filter an indexed one by.
    /// Optional: the probe falls back to a generic term, because "does this
    /// URL answer at all" is a useful answer on its own.
    query: Option<String>,
    /// HTTP verb. Absent means GET, which is what every probe sent before this
    /// field existed — several x402 providers answer their challenge only to a
    /// POST and returned 404 or 405 to every probe until now.
    method: Option<String>,
    /// Request body for a non-GET probe, verbatim. Usually the smallest thing
    /// that gets past input validation to the 402, e.g. `{}`.
    body: Option<String>,
}

pub async fn probe_source(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    if let Err(response) = require_platform_admin(&state, &headers).await {
        return response;
    }

    if let Some(limited) = throttled_by_ip(&state, &headers, "discovery-probe", PROBE_RATE_LIMIT, "That is a lot of probing. Each one calls somebody else's server — please slow down.").await {
        return limited;
    }

    let request: ProbeRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_json",
                "Send {\"url\": \"https://…\", \"shape\": \"x402-items\", \"fetch_kind\": \"indexed\"}. Optionally \"method\" and \"body\".",
            )
        }
    };

    let url = request.url.unwrap_or_default();
    if url.trim().is_empty() {
        return error(
            StatusCode::BAD_REQUEST,
            "url_required",
            "Which URL should be probed?",
        );
    }

    match crate::sources::probe(
        &state,
        &url,
        request.shape.as_deref().unwrap_or(""),
        request.fetch_kind.as_deref().unwrap_or(""),
        request.query.as_deref().unwrap_or(""),
        request.method.as_deref(),
        request.body.as_deref(),
    )
    .await
    {
        Ok(report) => json_response(StatusCode::OK, report),
        // The probe only returns Err for a request it cannot make sense of;
        // an unreachable or broken source comes back as a successful report.
        Err(message) => error(StatusCode::BAD_REQUEST, "invalid_probe", &message),
    }
}

/// 401/403 unless the portal session belongs to an active platform admin.
///
/// `pub(crate)`: every `/v1/admin/*` handler authenticates this same way, and
/// two copies of an auth gate is one more than can be reasoned about.
pub(crate) async fn require_platform_admin(
    state: &Arc<AppState>,
    headers: &HeaderMap,
) -> Result<(), Response<Body>> {
    let jwt = extract_credits_user_jwt(headers).ok_or_else(|| {
        error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Missing portal session token.",
        )
    })?;

    let user_id = resolve_portal_user_id(state, &jwt, "discovery probe")
        .await
        .map_err(|err| match err {
            PortalAuthError::NotConfigured => error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "admin_auth_not_configured",
                "Admin authentication is not configured on the gateway.",
            ),
            PortalAuthError::Unavailable => error(
                StatusCode::BAD_GATEWAY,
                "admin_auth_unavailable",
                "Could not validate the portal session.",
            ),
            PortalAuthError::Invalid => error(
                StatusCode::UNAUTHORIZED,
                "admin_auth_invalid",
                "Invalid or expired portal session token.",
            ),
        })?;

    match state.db.is_active_platform_admin(user_id).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(error(
            StatusCode::FORBIDDEN,
            "not_authorized",
            "This endpoint is for active platform admins.",
        )),
        Err(err) => {
            tracing::error!("failed to check platform admin: {err}");
            Err(error(
                StatusCode::BAD_GATEWAY,
                "admin_auth_unavailable",
                "Could not confirm admin access.",
            ))
        }
    }
}

