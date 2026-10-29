//! Starts the actual native runtime against the isolated PostgreSQL fixture.
//! Run via scripts/test-native-runtime.mjs, which supplies a clean environment.
use apiosk_gateway_v2::compat::CommerceService;
use axum::{
    body::{to_bytes, Body},
    http::{Method, Request, StatusCode},
};
use serde_json::{json, Value};
use std::time::Duration;

async fn post(service: &CommerceService, path: &str, body: Value) -> (StatusCode, Value) {
    let request_id = "00000000-0000-4000-8000-000000000081";
    let response = service
        .dispatch(
            Request::builder()
                .method(Method::POST)
                .uri(path)
                .header("content-type", "application/json")
                .header("x-request-id", request_id)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await;
    assert_eq!(response.headers()["x-request-id"], request_id);
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the isolated clean environment from scripts/test-native-runtime.mjs"]
async fn native_runtime_initializes_with_its_own_database_and_retains_route_gates() {
    assert_eq!(
        std::env::var("NATIVE_RUNTIME_LOCAL_FIXTURE").as_deref(),
        Ok("1")
    );
    let commerce_url = std::env::var("EXECUTOR_DATABASE_URL").unwrap();
    let parsed = url::Url::parse(&commerce_url).unwrap();
    assert_eq!(parsed.host_str(), Some("127.0.0.1"));
    assert_eq!(parsed.path(), "/postgres");
    let v2_url = std::env::var("DATABASE_URL").unwrap();
    assert_ne!(commerce_url, v2_url);
    assert_eq!(
        std::env::var("SOURCE_INDEX_ENABLED").as_deref(),
        Ok("false")
    );
    assert_eq!(
        std::env::var("EXTERNAL_FETCH_ENABLED").as_deref(),
        Ok("false")
    );

    // A v2 database credential must never become an implicit commerce role.
    // This integration binary contains only this test, so no other test can
    // observe the temporary environment change.
    std::env::remove_var("EXECUTOR_DATABASE_URL");
    let missing = commerce_runtime::CommerceRuntime::from_env().await;
    std::env::set_var("EXECUTOR_DATABASE_URL", &commerce_url);
    let error = match missing {
        Ok(_) => panic!("native commerce unexpectedly accepted only DATABASE_URL"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("EXECUTOR_DATABASE_URL"));

    // PGlite serves one connection. The existing log-pool initialization
    // falls back when its connection is rejected; this is not a concurrency test.
    let runtime = tokio::time::timeout(
        Duration::from_secs(45),
        commerce_runtime::CommerceRuntime::from_env(),
    )
    .await
    .expect("native runtime startup exceeded its local fixture budget")
    .expect("native runtime initialization");
    assert_eq!(std::env::var("DATABASE_URL").unwrap(), v2_url);
    assert!(runtime.ready().await);
    let service = CommerceService::with_in_flight(runtime.router(), runtime.in_flight());
    assert!(service.healthy().await);

    let (status, body) = post(&service, "/v1/skills", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"], "unauthorized");

    let (status, body) = post(&service, "/v1/quote", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "no_subject");

    let (status, body) = post(
        &service,
        "/v1/x402/fetch",
        json!({"url":"http://127.0.0.1:9/never-called", "confirmed_price_usdc":0}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "feature_disabled");
    assert!(runtime.shutdown(Duration::from_secs(2)).await);
}
