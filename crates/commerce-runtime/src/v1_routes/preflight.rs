use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

use crate::AppState;

/// `POST /v1/apis/{slug}/preflight`
///
/// Body shape (all optional):
///   { "operation": "/current", "method": "GET" }
///
/// Same verdict shape the `X-Apiosk-Dry-Run: 1` header returns when sent
/// directly against the gateway slug URL. This dedicated endpoint exists
/// so an agent can preflight before knowing the exact final URL — e.g.
/// to budget a list of candidate APIs against today's spend cap before
/// committing to one.
pub async fn preflight(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap<HeaderValue>,
    Path(slug): Path<String>,
    body: Option<Json<PreflightBody>>,
) -> Response {
    let body = body.map(|Json(b)| b).unwrap_or_default();
    match crate::proxy::preflight_for_slug(
        &state,
        &slug,
        body.operation.as_deref(),
        body.method.as_deref(),
        &headers,
    )
    .await
    {
        Ok(response) => response,
        Err(err) => err,
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct PreflightBody {
    /// Optional endpoint path (e.g. `/current` or `current`). Defaults to
    /// the API's first endpoint.
    pub operation: Option<String>,
    /// Optional HTTP verb to disambiguate when the same path has multiple
    /// methods registered. Case-insensitive. Defaults to `GET`.
    pub method: Option<String>,
}

/// `GET /v1/apis/{slug}/preflight` — same as the POST handler, for the
/// common case where the agent has no body to pass and just wants to
/// pre-check the default operation.
pub async fn preflight_get(
    state: State<Arc<AppState>>,
    headers: HeaderMap<HeaderValue>,
    path: Path<String>,
) -> Response {
    preflight(state, headers, path, None).await
}

#[allow(dead_code)]
fn _ensure_response_types_link() -> Response {
    // Touches Body and StatusCode/IntoResponse so the `use` lines don't get
    // flagged as unused if the handlers are ever inlined.
    let _: Body = Body::empty();
    let _: StatusCode = StatusCode::OK;
    Json(serde_json::json!({"ok": true})).into_response()
}
