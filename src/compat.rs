//! In-process routing for the gateway's native commerce capabilities.
//! The router owns the same handlers and state as the public commerce routes;
//! no second server, loopback origin, or internal HTTP client is involved.
use axum::{
    body::{Body, Bytes},
    http::{header, HeaderMap, Method, Request, StatusCode, Uri},
    response::{IntoResponse, Response},
    Router,
};
use tower::ServiceExt;

#[derive(Clone)]
pub struct CommerceService {
    router: Router,
    in_flight: commerce_runtime::InFlight,
}

impl CommerceService {
    pub fn new(router: Router) -> Self {
        Self::with_in_flight(router, commerce_runtime::InFlight::default())
    }

    pub fn with_in_flight(router: Router, in_flight: commerce_runtime::InFlight) -> Self {
        Self { router, in_flight }
    }

    /// Dispatch through the native router so its authentication, validation,
    /// provider handlers and response headers remain the public route contract.
    pub async fn dispatch(&self, request: Request<Body>) -> Response {
        let router = self.router.clone();
        // A disconnected caller must not cancel provider/ledger completion.
        // Dropping this JoinHandle detaches the work, just as disconnecting
        // from the former internal server left its request running. The shared
        // tracker lets the sole process drain that work during shutdown.
        let work = self.in_flight.track(async move {
            match router.oneshot(request).await {
                Ok(response) => response,
                Err(never) => match never {},
            }
        });
        match crate::telemetry::spawn(work).await {
            Ok(response) => response,
            Err(error) => {
                tracing::error!(%error, "native commerce request interrupted");
                (StatusCode::SERVICE_UNAVAILABLE, "Commerce response unavailable. Check the existing request status.").into_response()
            }
        }
    }

    pub async fn healthy(&self) -> bool {
        let request = Request::get("/health").body(Body::empty()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), self.dispatch(request))
            .await
            .is_ok_and(|response| response.status().is_success())
    }
}

pub async fn forward_request(
    commerce: CommerceService,
    uri: Uri,
    method: Method,
    mut headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(path_and_query) = uri.path_and_query() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let mut request = Request::builder()
        .method(method)
        .uri(path_and_query.as_str())
        .body(Body::from(body))
        .expect("the incoming HTTP method and URI are already valid");
    // Hop-by-hop wire metadata never belongs to an in-process request. Keep
    // identity, authorization, idempotency and original Host headers intact.
    headers.remove(header::CONNECTION);
    headers.remove(header::CONTENT_LENGTH);
    *request.headers_mut() = headers;
    commerce.dispatch(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::OriginalUri, routing::{any, get}};

    #[tokio::test]
    async fn native_dispatch_preserves_path_query_payment_headers_body_and_response() {
        async fn fixture(OriginalUri(uri): OriginalUri, headers: HeaderMap, body: Bytes) -> Response {
            assert_eq!(uri, "/v1/do?deterministic=true");
            assert_eq!(headers["authorization"], "Bearer fixture-token");
            assert_eq!(headers["x-apiosk-service-key"], "fixture-secret");
            assert_eq!(headers["idempotency-key"], "stable-key");
            assert_eq!(headers["host"], "gateway.apiosk.com");
            assert_eq!(headers["fly-client-ip"], "203.0.113.7");
            assert_eq!(headers["x-forwarded-for"], "203.0.113.7, 198.51.100.2");
            assert_eq!(headers["x-real-ip"], "203.0.113.7");
            assert!(!headers.contains_key(header::CONNECTION));
            let mut response = (StatusCode::CREATED, body).into_response();
            response.headers_mut().append(header::SET_COOKIE, "one=1; Secure".parse().unwrap());
            response.headers_mut().append(header::SET_COOKIE, "two=2; Secure".parse().unwrap());
            response.headers_mut().insert("x-payment-receipt", "fixture-receipt".parse().unwrap());
            response
        }
        let commerce = CommerceService::new(Router::new()
            .route("/v1/do", any(fixture))
            .route("/health", get(|| async { StatusCode::OK })));
        assert!(commerce.healthy().await);
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("authorization", "Bearer fixture-token"),
            ("x-apiosk-service-key", "fixture-secret"),
            ("idempotency-key", "stable-key"),
            ("host", "gateway.apiosk.com"),
            ("fly-client-ip", "203.0.113.7"),
            ("x-forwarded-for", "203.0.113.7, 198.51.100.2"),
            ("x-real-ip", "203.0.113.7"),
            ("connection", "keep-alive"),
        ] {
            headers.insert(axum::http::HeaderName::from_static(name), value.parse().unwrap());
        }
        let response = forward_request(commerce, "/v1/do?deterministic=true".parse().unwrap(),
            Method::POST, headers, Bytes::from_static(br#"{"candidate_id":"abc"}"#)).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(response.headers()["x-payment-receipt"], "fixture-receipt");
        assert_eq!(response.headers().get_all(header::SET_COOKIE).iter().count(), 2);
        assert_eq!(axum::body::to_bytes(response.into_body(), 1024).await.unwrap(),
            br#"{"candidate_id":"abc"}"#[..]);
    }

    #[tokio::test]
    async fn readiness_uses_the_native_health_route() {
        let commerce = CommerceService::new(Router::new()
            .route("/health", get(|| async { StatusCode::SERVICE_UNAVAILABLE })));
        assert!(!commerce.healthy().await);
    }

    #[tokio::test]
    async fn caller_cancellation_does_not_interrupt_receipt_completion() {
        use std::sync::Arc;
        use tokio::sync::Notify;
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let receipt = Arc::new(Notify::new());
        let router = Router::new().route("/provider", any({
            let (started, release, receipt) = (started.clone(), release.clone(), receipt.clone());
            move || {
                let (started, release, receipt) = (started.clone(), release.clone(), receipt.clone());
                async move {
                    started.notify_one();
                    release.notified().await;
                    // A local stand-in for the existing durable receipt write.
                    receipt.notify_one();
                    StatusCode::OK
                }
            }
        }));
        let service = CommerceService::new(router);
        let caller = crate::telemetry::spawn(async move {
            service.dispatch(Request::post("/provider").body(Body::empty()).unwrap()).await
        });
        started.notified().await;
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(1), receipt.notified()).await.unwrap();
    }
}
