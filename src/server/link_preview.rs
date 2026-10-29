use super::{app_person, failure, ServerState};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    url: String,
}

pub(super) async fn preview(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<Request>,
) -> Response {
    let person = match app_person(&state, &headers).await {
        Ok(person) => person,
        Err(response) => return response,
    };
    if request.url.is_empty() || request.url.len() > 4096 {
        return failure(
            StatusCode::BAD_REQUEST,
            "invalid_url",
            "This link has no preview.",
        );
    }
    let service = crate::link_preview::Service::shared();
    if !service.admit(person.owner) {
        return failure(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Link previews are temporarily unavailable.",
        );
    }
    let result = service.preview(&request.url).await;
    (
        [(axum::http::header::CACHE_CONTROL, "private, no-store")],
        Json(result),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::Request as HttpRequest,
        routing::get,
        Router,
    };
    use serde_json::{json, Value};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn route_requires_a_session_bounds_input_and_keeps_downloads_without_tasks() {
        let owner = uuid::Uuid::new_v4();
        let auth_calls = Arc::new(AtomicUsize::new(0));
        let auth = Router::new().route(
            "/auth/v1/user",
            get({
                let calls = auth_calls.clone();
                move |headers: HeaderMap| {
                    let calls = calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(
                            headers.get("authorization").unwrap(),
                            "Bearer preview-fixture"
                        );
                        Json(json!({"id":owner}))
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, auth).await.unwrap() });
        // A lazy, unreachable database proves the preview does not create a
        // task, quote or execution. The execution semaphore is exhausted too.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused@127.0.0.1:1/unused")
            .unwrap();
        let http = reqwest::Client::new();
        let store = crate::store::Store { pool: pool.clone() };
        let app = crate::server::router(
            ServerState {
                auth: Arc::new(crate::auth::Auth {
                    pool: pool.clone(),
                    http: http.clone(),
                    supabase_url: origin,
                    publishable_key: "fixture".into(),
                }),
                engine: Arc::new(crate::engine::Engine {
                    store: store.clone(),
                    catalog: crate::catalog::CatalogCache::new(pool),
                    parser: crate::parser::Parser::new(
                        http.clone(),
                        "http://127.0.0.1:1/chat".into(),
                        "unused".into(),
                        "unused".into(),
                    ),
                    signer: crate::state::Signer::new(vec![7; 32]).unwrap(),
                    executor: crate::execution::Executor {
                        store,
                        http,
                        supabase_url: "http://127.0.0.1:1".into(),
                        enabled: false,
                    },
                    app_url: "http://localhost:5173".into(),
                }),
                permits: Arc::new(tokio::sync::Semaphore::new(0)),
                commerce: None,
            },
            "http://localhost:5173",
        )
        .unwrap();
        let request = |body: Value, authenticated: bool| {
            let mut builder =
                HttpRequest::post("/v2/link-preview").header("content-type", "application/json");
            if authenticated {
                builder = builder.header("authorization", "Bearer preview-fixture");
            }
            builder.body(Body::from(body.to_string())).unwrap()
        };
        let url = "https://apiosk.com/report.pdf?signature=fixture";
        let response = app
            .clone()
            .oneshot(request(json!({"url":url}), false))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(auth_calls.load(Ordering::SeqCst), 0);
        let response = app
            .clone()
            .oneshot(request(json!({"url":"x".repeat(9000)}), true))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let response = app
            .clone()
            .oneshot(request(
                json!({"url":url,"headers":{"authorization":"not forwarded"}}),
                true,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        for _ in 0..60 {
            let response = app
                .clone()
                .oneshot(request(json!({"url":url}), true))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers().get("cache-control").unwrap(),
                "private, no-store"
            );
            let body: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap())
                    .unwrap();
            assert_eq!(
                body,
                json!({"url":url,"title":null,"description":null,"site_name":null})
            );
        }
        let response = app
            .oneshot(request(json!({"url":url}), true))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        server.abort();
    }
}
