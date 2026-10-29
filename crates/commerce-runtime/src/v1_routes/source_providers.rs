//! `POST /v1/admin/sources/{id}/providers/refresh` — write one source's
//! sellers down, now.
//!
//! The crawler snapshots every indexed source's providers into
//! `source_providers` after each pass (migration 090, `src/source_index.rs`),
//! so this endpoint is not the only writer — it exists for the moment an
//! admin is LOOKING: a source just added, a crawl just finished, a curation
//! pass about to start. Waiting up to fifteen minutes for the next timer tick
//! to see rows appear reads as "the feature is broken".
//!
//! It snapshots from the in-memory catalogue rather than re-crawling: the
//! crawl already ran, its result is in RAM, and re-fetching 26,000 entries
//! because a button was pressed would be a small denial-of-service against an
//! index we depend on. A source that has no catalogue yet — queried-only, or
//! the first crawl still pending — says so instead of pretending.
//!
//! Admin-only, same gate as the probe (`require_platform_admin`), and the
//! report carries the first rows THEMSELVES, not a count: the question an
//! admin is asking is "did real sellers land", and that is answered by
//! reading real domains with real prices next to them.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header::RETRY_AFTER, HeaderMap, HeaderValue, StatusCode},
    response::Response,
};
use serde_json::{json, Value};

use crate::rate_limit::{RateLimitDecision, RateLimitPolicy};
use crate::AppState;

use super::discovery_sources::require_platform_admin;

/// Refreshing is a database write from memory — cheap — but it is fired by a
/// button, and buttons get leaned on.
const REFRESH_RATE_LIMIT: RateLimitPolicy = RateLimitPolicy::per_minute(30);

/// Rows shown in the report. The convention every admin test action follows:
/// twenty real records, because "did it work" is answered by reading actual
/// rows, and twenty is enough to recognise a catalogue and small enough to
/// stay in one panel.
const REPORT_SAMPLE: usize = 20;

pub async fn refresh_source_providers(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response<Body> {
    if let Err(response) = require_platform_admin(&state, &headers).await {
        return response;
    }
    if let Some(limited) = rate_limited(&state, &headers).await {
        return limited;
    }

    let Some(source) = crate::sources::source_by_id(&state, &id).await else {
        return error(
            StatusCode::NOT_FOUND,
            "unknown_source",
            "No enabled source by that id. Check the Sources list — a disabled source is not swept and has no catalogue to snapshot.",
        );
    };

    if !source.is_browsable() {
        return error(
            StatusCode::BAD_REQUEST,
            "queried_source",
            "This index only answers questions — it publishes no whole catalogue, so there is nothing to snapshot providers from. Provider rows exist for indexed sources only.",
        );
    }

    let Some(catalogue) = state.source_index.get(&id).await else {
        return error(
            StatusCode::CONFLICT,
            "not_crawled_yet",
            "This source has no crawl in memory yet. Either the first crawl since startup is still running (give it a few minutes), or SOURCE_INDEX_ENABLED is off on this gateway.",
        );
    };

    let snapshot = catalogue.provider_snapshot();
    let written = match state.db.upsert_source_providers(&id, &snapshot).await {
        Ok(written) => written,
        Err(err) => {
            tracing::error!(source = %id, "source_providers refresh failed: {err}");
            return error(
                StatusCode::BAD_GATEWAY,
                "write_failed",
                "The snapshot could not be written. If this persists, check that migration 090_source_providers.sql has been applied.",
            );
        }
    };

    // Real rows, not a count — and biggest sellers first, because those are
    // the ones worth curating.
    let mut ranked: Vec<&crate::source_index::ProviderSnapshot> = snapshot.iter().collect();
    ranked.sort_by(|a, b| {
        b.endpoint_count
            .cmp(&a.endpoint_count)
            .then(a.key.cmp(&b.key))
    });
    let sample: Vec<Value> = ranked
        .iter()
        .take(REPORT_SAMPLE)
        .map(|row| {
            json!({
                "provider_key": row.key,
                "sample_host": row.sample_host,
                "endpoint_count": row.endpoint_count,
                "price_usd_min": row.price_usd_min,
                "price_usd_max": row.price_usd_max,
                "unpriced_endpoints": row.unpriced_endpoints,
                "tags": row.tags,
                "categories": row.categories,
                "top_terms": row.top_terms.iter().take(8).collect::<Vec<_>>(),
                "sample_description": row.sample_description,
            })
        })
        .collect();

    json_response(
        StatusCode::OK,
        json!({
            "ok": true,
            "source_id": id,
            "providers": snapshot.len(),
            "rows_written": written,
            "sample": sample,
            "crawl": {
                "indexed": catalogue.len(),
                "read": catalogue.read,
                "index_reports_total": catalogue.declared_total,
                "pages": catalogue.pages,
                "complete": catalogue.stopped.complete(),
                "stopped_because": catalogue.stopped.as_str(),
                "refreshed_unix": catalogue.refreshed_unix,
            },
            "note": "Derived columns overwritten from the crawl in memory; curated columns untouched. Rows for sellers no longer in the index are kept with their old last_seen_at.",
        }),
    )
}

async fn rate_limited(state: &Arc<AppState>, headers: &HeaderMap) -> Option<Response<Body>> {
    let ip = headers
        .get("fly-client-ip")
        .or_else(|| headers.get("x-forwarded-for"))
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(',').next().unwrap_or(v).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    match state
        .rate_limiter
        .check(
            &format!("source-providers-refresh:{ip}"),
            REFRESH_RATE_LIMIT,
        )
        .await
    {
        RateLimitDecision::Allowed { .. } => None,
        RateLimitDecision::Throttled { retry_after } => {
            let mut response = error(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "Refreshed very recently. The snapshot only changes when a crawl does — give it a moment.",
            );
            if let Ok(value) = HeaderValue::from_str(&retry_after.as_secs().max(1).to_string()) {
                response.headers_mut().insert(RETRY_AFTER, value);
            }
            Some(response)
        }
    }
}

fn json_response(status: StatusCode, body: Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn error(status: StatusCode, code: &str, message: &str) -> Response<Body> {
    json_response(
        status,
        json!({"ok": false, "error": code, "message": message}),
    )
}
