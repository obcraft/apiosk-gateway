//! Service-only bridge: App inference uses the Gateway's one model policy.
use crate::{service_auth::ServiceAuth, AppState};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructuredRequest {
    system: String,
    user: String,
    schema: Value,
    max_tokens: u32,
    label: String,
}

pub async fn structured_inference(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<StructuredRequest>,
) -> Response {
    if !matches!(
        crate::service_auth::resolve(&state, &headers).await,
        ServiceAuth::Resolved(_)
    ) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"service_key_required"})),
        )
            .into_response();
    }
    if request.system.len() > 16_000
        || request.user.len() > 32_000
        || request.schema.to_string().len() > 32_000
        || request.label.len() > 64
        || !(1..=2048).contains(&request.max_tokens)
        || crate::structured::check_schema(&request.schema).is_err()
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"invalid_inference_request"})),
        )
            .into_response();
    }
    let Some(vendor) = crate::query_plan::parser_vendor(&state.config) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"engine_unavailable"})),
        )
            .into_response();
    };
    match crate::query_plan::call_structured(
        &state,
        &vendor,
        &request.system,
        request.user,
        request.schema,
        request.max_tokens,
        &request.label,
        Duration::from_secs(10),
    )
    .await
    {
        Ok(output) => {
            Json(json!({"output":output,"model":state.config.openrouter_model})).into_response()
        }
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"engine_unavailable"})),
        )
            .into_response(),
    }
}
