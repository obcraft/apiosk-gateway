use axum::{
    body::Bytes,
    extract::{OriginalUri, State},
    http::{header, HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
};
use reqwest::Client;

#[derive(Clone)]
pub struct ExecutorCompat {
    client: Client,
    origin: String,
}

impl ExecutorCompat {
    pub fn new(client: Client, origin: String) -> anyhow::Result<Self> {
        let parsed = url::Url::parse(&origin)?;
        anyhow::ensure!(
            parsed.scheme() == "http"
                && matches!(parsed.host_str(), Some("127.0.0.1" | "localhost")),
            "embedded executor must use loopback HTTP"
        );
        anyhow::ensure!(
            parsed.username().is_empty()
                && parsed.password().is_none()
                && parsed.query().is_none()
                && parsed.fragment().is_none(),
            "invalid embedded executor origin"
        );
        Ok(Self {
            client,
            origin: origin.trim_end_matches('/').to_owned(),
        })
    }

    pub async fn healthy(&self) -> bool {
        self.client
            .get(format!("{}/health", self.origin))
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
    }
}

pub async fn forward(
    State(proxy): State<ExecutorCompat>,
    OriginalUri(uri): OriginalUri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    forward_request(proxy, uri, method, headers, body).await
}

pub async fn forward_request(
    proxy: ExecutorCompat,
    uri: axum::http::Uri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(path_and_query) = uri.path_and_query() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let mut request = proxy
        .client
        .request(
            method,
            format!("{}{}", proxy.origin, path_and_query.as_str()),
        )
        .body(body);
    for (name, value) in &headers {
        if name != header::CONTENT_LENGTH && name != header::CONNECTION {
            request = request.header(name, value);
        }
    }
    let upstream = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            tracing::error!(%error, "embedded executor request failed");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "embedded payment executor unavailable",
            )
                .into_response();
        }
    };
    let status = upstream.status();
    let response_headers = upstream.headers().clone();
    let bytes = match upstream.bytes().await {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::error!(%error, "embedded executor response failed");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    let mut response = (status, bytes).into_response();
    for (name, value) in &response_headers {
        if name != header::CONTENT_LENGTH
            && name != header::TRANSFER_ENCODING
            && name != header::CONNECTION
        {
            response.headers_mut().insert(name, value.clone());
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        http::{Request, StatusCode},
        routing::{any, get},
        Router,
    };
    use tower::ServiceExt;

    async fn fixture(headers: HeaderMap, body: Bytes) -> Response {
        assert_eq!(
            headers.get("x-apiosk-service-key").unwrap(),
            "fixture-secret"
        );
        assert_eq!(headers.get("idempotency-key").unwrap(), "stable-key");
        assert_eq!(headers.get("host").unwrap(), "gateway.apiosk.com");
        (StatusCode::CREATED, body).into_response()
    }

    #[tokio::test]
    async fn forwards_payment_headers_path_query_and_body() {
        let upstream = Router::new()
            .route("/v1/do", any(fixture))
            .route("/health", get(|| async { StatusCode::OK }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let proxy = ExecutorCompat::new(Client::new(), origin).unwrap();
        assert!(proxy.healthy().await);
        let app = Router::new()
            .route("/v1/*path", any(forward))
            .with_state(proxy);
        let response = app
            .oneshot(
                Request::post("/v1/do?deterministic=true")
                    .header("x-apiosk-service-key", "fixture-secret")
                    .header("idempotency-key", "stable-key")
                    .header("host", "gateway.apiosk.com")
                    .body(axum::body::Body::from(r#"{"candidate_id":"abc"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(body, r#"{"candidate_id":"abc"}"#);
    }

    #[test]
    fn rejects_non_loopback_executor_origins() {
        assert!(ExecutorCompat::new(Client::new(), "https://gateway.apiosk.com".into()).is_err());
    }
}
