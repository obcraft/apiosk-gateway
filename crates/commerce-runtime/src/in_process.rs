//! Dispatch internally approved calls through the same authenticated router.
//! The virtual URL is only a request-builder input: no DNS or socket is used.

use axum::{
    body::Body,
    http::{header::HOST, HeaderValue, Request, Response},
};
use futures_util::StreamExt;
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;
use tracing::Instrument;

use crate::{AppState, InFlight};

#[derive(Debug, thiserror::Error)]
pub(crate) enum DispatchError {
    #[error("commerce runtime is not available")]
    Unavailable,
    #[error("invalid internal request: {0}")]
    Request(#[from] reqwest::Error),
    #[error("internal dispatch timed out; payment outcome must be reconciled")]
    Timeout,
    #[error("invalid internal request body or URI")]
    InvalidRequest,
    #[error("internal dispatch task stopped; payment outcome must be reconciled")]
    TaskStopped,
}

impl DispatchError {
    // Only failure BEFORE dispatch is safe to retry using the existing rule.
    pub fn is_connect(&self) -> bool {
        matches!(self, Self::Unavailable)
    }
}

pub(crate) async fn send(
    state: &Arc<AppState>,
    call: reqwest::RequestBuilder,
) -> Result<reqwest::Response, DispatchError> {
    let router = state
        .router
        .get()
        .and_then(std::sync::Weak::upgrade)
        .ok_or(DispatchError::Unavailable)?;
    dispatch(
        router.as_ref().clone(),
        call.build()?,
        state.in_flight.clone(),
    )
    .await
}

async fn dispatch(
    router: axum::Router,
    request: reqwest::Request,
    in_flight: InFlight,
) -> Result<reqwest::Response, DispatchError> {
    let timeout = request
        .timeout()
        .copied()
        .unwrap_or(Duration::from_secs(60));
    let deadline = tokio::time::Instant::now() + timeout;
    let body = match request.body() {
        Some(body) => body
            .as_bytes()
            .ok_or(DispatchError::InvalidRequest)?
            .to_vec(),
        None => Vec::new(),
    };
    let mut target = Request::builder()
        .method(request.method().clone())
        .uri(&request.url()[url::Position::BeforePath..])
        .body(Body::from(body))
        .map_err(|_| DispatchError::InvalidRequest)?;
    *target.headers_mut() = request.headers().clone();
    if !target.headers().contains_key(HOST) {
        let host = &request.url()[url::Position::BeforeHost..url::Position::AfterPort];
        target.headers_mut().insert(
            HOST,
            HeaderValue::from_str(host).map_err(|_| DispatchError::InvalidRequest)?,
        );
    }
    // Preserve the former server/client cancellation boundary: once admitted,
    // provider and ledger work outlives a timed-out or disconnected caller.
    // Dropping a JoinHandle detaches it; it does not abort the paid handler.
    let task = tokio::spawn(in_flight.track(router.oneshot(target)).in_current_span());
    let response = tokio::time::timeout_at(deadline, task)
        .await
        .map_err(|_| DispatchError::Timeout)?
        .map_err(|_| DispatchError::TaskStopped)?
        .unwrap_or_else(|never| match never {});
    let (parts, body) = response.into_parts();
    // Keep streaming/error semantics, including the original total timeout.
    // A known 2xx status still reaches the existing receipt/settlement parser
    // even when reading its body subsequently fails.
    let body =
        futures_util::stream::unfold(Some(body.into_data_stream()), move |stream| async move {
            let mut stream = stream?;
            match tokio::time::timeout_at(deadline, stream.next()).await {
                Ok(Some(Ok(bytes))) => Some((Ok(bytes), Some(stream))),
                Ok(Some(Err(error))) => Some((Err(std::io::Error::other(error)), None)),
                Ok(None) => None,
                Err(_) => Some((
                    Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "internal response timed out",
                    )),
                    None,
                )),
            }
        });
    Ok(reqwest::Response::from(Response::from_parts(
        parts,
        reqwest::Body::wrap_stream(body),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{http::StatusCode, routing::post};

    #[tokio::test]
    async fn native_dispatch_preserves_auth_idempotency_query_body_and_receipt() {
        let router = axum::Router::new().route(
            "/provider/path",
            post(|request: Request<Body>| async move {
                assert_eq!(request.uri(), "/provider/path?company=Volve%20Capital");
                assert_eq!(request.headers()["x-apiosk-service-key"], "fixture-key");
                assert_eq!(request.headers()["idempotency-key"], "same-purchase");
                assert_eq!(request.headers()["x-apiosk-approval-id"], "same-approval");
                assert_eq!(request.headers()["x-apiosk-max-buyer-total-atomic"], "125");
                let body = axum::body::to_bytes(request.into_body(), 4096)
                    .await
                    .unwrap();
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                    serde_json::json!({"value": 42})
                );
                (
                    StatusCode::PAYMENT_REQUIRED,
                    [("x-apiosk-buyer-total-atomic", "0")],
                    "provider declined",
                )
            }),
        );
        let request = reqwest::Client::new()
            .post("http://commerce.internal/provider/path?company=Volve%20Capital")
            .header("x-apiosk-service-key", "fixture-key")
            .header("idempotency-key", "same-purchase")
            .header("x-apiosk-approval-id", "same-approval")
            .header("x-apiosk-max-buyer-total-atomic", "125")
            .json(&serde_json::json!({"value": 42}))
            .build()
            .unwrap();
        let response = dispatch(router, request, InFlight::default())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
        assert_eq!(response.headers()["x-apiosk-buyer-total-atomic"], "0");
        assert_eq!(response.text().await.unwrap(), "provider declined");
    }

    #[tokio::test]
    async fn timeout_after_dispatch_is_never_a_retryable_connection_failure() {
        let router = axum::Router::new().route(
            "/slow",
            post(|| async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                "too late"
            }),
        );
        let request = reqwest::Client::new()
            .post("http://commerce.internal/slow")
            .timeout(Duration::from_millis(1))
            .build()
            .unwrap();
        let error = dispatch(router, request, InFlight::default())
            .await
            .unwrap_err();
        assert!(matches!(error, DispatchError::Timeout));
        assert!(!error.is_connect());
    }

    #[tokio::test]
    async fn body_timeout_preserves_status_and_receipt_for_reconciliation() {
        let router = axum::Router::new().route(
            "/receipt",
            post(|| async {
                let body = Body::from_stream(futures_util::stream::once(async {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"late body"))
                }));
                Response::builder()
                    .status(StatusCode::OK)
                    .header("x-apiosk-buyer-total-atomic", "125")
                    .body(body)
                    .unwrap()
            }),
        );
        let request = reqwest::Client::new()
            .post("http://commerce.internal/receipt")
            .timeout(Duration::from_millis(10))
            .build()
            .unwrap();
        let response = dispatch(router, request, InFlight::default())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["x-apiosk-buyer-total-atomic"], "125");
        assert!(response.bytes().await.is_err());
    }

    async fn admitted_call_survives(caller_timeout: bool) {
        let started = Arc::new(tokio::sync::Notify::new());
        let finish = Arc::new(tokio::sync::Notify::new());
        let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let router = axum::Router::new().route(
            "/paid",
            post({
                let started = started.clone();
                let finish = finish.clone();
                let completed = completed.clone();
                move || {
                    let started = started.clone();
                    let finish = finish.clone();
                    let completed = completed.clone();
                    async move {
                        started.notify_one();
                        finish.notified().await;
                        completed.store(true, std::sync::atomic::Ordering::SeqCst);
                        "durable receipt saved"
                    }
                }
            }),
        );
        let calls = InFlight::default();
        let request = reqwest::Client::new()
            .post("http://commerce.internal/paid")
            .timeout(if caller_timeout {
                Duration::from_millis(25)
            } else {
                Duration::from_secs(5)
            })
            .build()
            .unwrap();
        let caller = tokio::spawn(dispatch(router, request, calls.clone()));
        tokio::time::timeout(Duration::from_secs(1), started.notified())
            .await
            .unwrap();
        if caller_timeout {
            assert!(matches!(
                caller.await.unwrap().unwrap_err(),
                DispatchError::Timeout
            ));
        } else {
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
        }
        assert!(!calls.drain(Duration::ZERO).await);
        assert!(!completed.load(std::sync::atomic::Ordering::SeqCst));
        finish.notify_one();
        assert!(calls.drain(Duration::from_secs(1)).await);
        assert!(completed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn timeout_does_not_cancel_admitted_payment_work() {
        admitted_call_survives(true).await;
    }

    #[tokio::test]
    async fn disconnected_caller_does_not_cancel_admitted_payment_work() {
        admitted_call_survives(false).await;
    }
}
