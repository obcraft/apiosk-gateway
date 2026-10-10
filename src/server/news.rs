//! Public, read-only access to the fixed publisher feeds. This route has no
//! task, model, catalog, database, authentication or payment dependency.
use super::failure;
use axum::{http::StatusCode, response::{IntoResponse, Response}, Json};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Semaphore;

static NEWS_PERMITS: Semaphore = Semaphore::const_new(8);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    question: String,
}

pub(super) async fn search(Json(request): Json<Request>) -> Response {
    if request.question.chars().count() > 2000 {
        return failure(StatusCode::BAD_REQUEST, "invalid_news_question", "Keep the news question under 2,000 characters.");
    }
    let Some(query) = crate::supporting_news::classify_news(&request.question) else {
        return Json(json!({"handled":false})).into_response();
    };
    let Ok(_permit) = NEWS_PERMITS.try_acquire() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "news_busy", "Try the news search again shortly.");
    };
    Json(crate::supporting_news::search_news(query).await).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::{to_bytes, Body}, extract::DefaultBodyLimit, routing::post, Router};
    use serde_json::Value;
    use tower::ServiceExt;

    fn request(body: Value) -> axum::http::Request<Body> {
        axum::http::Request::post("/v2/ask-v2/news").header("content-type", "application/json")
            .body(Body::from(body.to_string())).unwrap()
    }

    #[tokio::test]
    async fn public_news_boundary_needs_no_auth_or_app_state_and_has_its_own_limit() {
        // The handler is usable with no Engine, Auth, DB or task semaphore.
        let app = Router::new().route("/v2/ask-v2/news", post(search).layer(DefaultBodyLimit::max(8 * 1024)));
        let permits = NEWS_PERMITS.acquire_many(8).await.unwrap();
        let response = app.clone().oneshot(request(json!({"question":"Find the official company profile of Batavus"}))).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), 1024).await.unwrap()).unwrap();
        assert_eq!(body, json!({"handled":false}), "ordinary questions do not enter RSS work or wait for its permits");
        let response = app.clone().oneshot(request(json!({"question":"News about Sinterklaas"}))).await.unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        drop(permits);
        let response = app.clone().oneshot(request(json!({"question":"x".repeat(2001)}))).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let response = app.clone().oneshot(request(json!({"question":"x".repeat(9000)}))).await.unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let response = app.oneshot(request(json!({"question":"News about Sinterklaas", "url":"https://example.com/feed"}))).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY, "clients cannot supply arbitrary feed URLs");
    }
}
