//! Intake routes: what the App needs before and around a question.
use crate::protocol::Meta;
use axum::{
    http::header,
    response::{IntoResponse, Response as HttpResponse},
    Json,
};

/// Public and credential-free: features and limits only, no account data.
pub async fn meta() -> HttpResponse {
    ([(header::CACHE_CONTROL, "public, max-age=60")], Json(Meta::current())).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn meta_is_cacheable_and_carries_the_shared_limits() {
        let response = meta().await;
        assert_eq!(response.headers()[header::CACHE_CONTROL], "public, max-age=60");
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let meta: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let limits: serde_json::Value = serde_json::from_str(include_str!("../../contracts/ask-limits.json")).unwrap();
        assert_eq!(meta["protocol_version"], "2");
        assert_eq!(meta["limits"], limits);
        assert!(meta["features"].is_array());
        assert_eq!(meta["events"], serde_json::json!({"heartbeat_ms":10000,"stale_after_ms":25000}));
    }
}
