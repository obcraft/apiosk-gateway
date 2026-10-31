//! Local-only regression fixture for the ordinary-API balance path.
//! Run against an empty disposable PostgreSQL database; never production.
use super::*;
use sha2::Digest;
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};

#[tokio::test]
#[ignore = "requires APIOSK_BALANCE_TEST_DATABASE_URL on localhost"]
async fn ordinary_api_executes_without_wallet_or_network_and_records_internal_revenue() {
    let db_url = std::env::var("APIOSK_BALANCE_TEST_DATABASE_URL").expect("local fixture URL");
    let parsed = url::Url::parse(&db_url).unwrap();
    assert!(matches!(parsed.host_str(), Some("127.0.0.1" | "localhost")));
    assert!(parsed.path().starts_with("/apiosk_balance_test"));
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&db_url)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("fixtures/app-balance.sql"))
        .execute(&pool)
        .await
        .unwrap();
    let token_id = Uuid::from_u128(1);
    let api_id = Uuid::from_u128(2);
    let endpoint_id = Uuid::from_u128(3);
    let db = crate::db::Database {
        pool: pool.clone(),
        log_pool: pool.clone(),
        request_log_queue: crate::db::request_log::RequestLogQueue::new(),
    };
    // Pass the disposable database explicitly; no process-global credential
    // needs changing and neither payer nor external credentials are constructed.
    let mut config = crate::config::Config::from_database_url(db_url).unwrap();
    config.app_service_key = Some("local-balance-fixture-key".into());
    config.treasury_connect_token_ref = Some(token_id.to_string());
    let state = Arc::new(AppState {
        router: std::sync::OnceLock::new(),
        in_flight: crate::InFlight::default(),
        config,
        db,
        cache: crate::cache::EndpointCache::new(),
        auth_replay_cache: crate::auth::AuthReplayCache::new(),
        http_client: reqwest::Client::builder().no_proxy().build().unwrap(),
        facilitator_url: "http://127.0.0.1:1/never-called".into(),
        resell_payer: None,
        external_fetch_payer: None,
        rate_limiter: crate::rate_limit::RateLimiter::default(),
        connect_nonce_cache: crate::connect::siwe::ConnectNonceCache::new(),
        facilitator_caps: crate::facilitator_caps::FacilitatorCapsCache::default(),
        telemetry: crate::telemetry::Telemetry::disabled(),
        discovery: crate::discovery_traffic::DiscoveryTracker::disabled(),
        source_index: crate::source_index::SourceIndex::new(),
        webmcp: crate::webmcp_traffic::WebmcpTracker::disabled(),
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let source_status = Arc::new(AtomicU16::new(200));
    let app = axum::Router::new().route(
        "/search",
        axum::routing::get({
            let calls = calls.clone();
            let source_status = source_status.clone();
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                let status = StatusCode::from_u16(source_status.load(Ordering::SeqCst)).unwrap();
                async move {
                    (
                        status,
                        [
                            ("x-apiosk-tx-hash", "fake-upstream-hash"),
                            ("x-apiosk-wallet-used", "fake-upstream-wallet"),
                            ("x-apiosk-payment-rail", "x402"),
                        ],
                        "{\"results\":[\"charging station\"]}",
                    )
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut api = state
        .db
        .get_api_by_slug("balance-fixture")
        .await
        .unwrap()
        .unwrap();
    assert!(api.apiosk_revenue);
    api.origin_url = origin;
    let endpoint = crate::db::Endpoint {
        id: endpoint_id,
        api_id,
        method: "GET".into(),
        path: "/search".into(),
        payment_required: true,
        price: "0.047259".parse().unwrap(),
        network_token_id: Some(Uuid::from_u128(99)),
        origin_override: None,
        auth_override: None,
        cache_ttl_seconds: None,
    };
    let mut resolved = ResolvedProxyRequest {
        api,
        listing: None,
        listing_type: ListingType::Api,
        preset_auth_headers: vec![],
        preset_auth_query: vec![],
        endpoint: Some(endpoint),
        virtual_execute_request: None,
        resolved_method: Method::GET,
        resolved_path: "/search".into(),
        resell: None,
    };
    let tracking = TrackingContext {
        request_id: Some(Uuid::from_u128(4)),
        // A stale presence association must not become a treasury debit.
        managed_wallet_id: Some(Uuid::from_u128(5)),
        ..Default::default()
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-apiosk-service-key",
        HeaderValue::from_static("local-balance-fixture-key"),
    );
    headers.insert(
        "x-apiosk-max-buyer-total-atomic",
        HeaderValue::from_static("54348"),
    );
    let authorize = |headers: HeaderMap, resolved: ResolvedProxyRequest| {
        let state = state.clone();
        let tracking = tracking.clone();
        async move {
            enforce_endpoint_payment(
                &state,
                "balance-fixture",
                "/search",
                &headers,
                false,
                &tracking,
                &resolved,
            )
            .await
        }
    };
    let decision = authorize(headers.clone(), resolved.clone()).await.unwrap();
    assert!(matches!(
        decision.deferred_payment,
        Some(DeferredApiPayment::AppBalance)
    ));
    let verdict = build_preflight_verdict(&state, &headers, &resolved).await;
    assert!(verdict.would_succeed);
    assert_eq!(verdict.rail, "apiosk_balance");
    assert!(verdict.wallet_to_use.is_none());
    // Authentication and the approved total remain mandatory even without a wallet.
    let mut wrong_key = headers.clone();
    wrong_key.insert(
        "x-apiosk-service-key",
        HeaderValue::from_static("wrong-key"),
    );
    assert_eq!(
        authorize(wrong_key, resolved.clone())
            .await
            .unwrap_err()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        authorize(HeaderMap::new(), resolved.clone())
            .await
            .unwrap_err()
            .status(),
        StatusCode::FORBIDDEN
    );
    let mut under_cap = headers.clone();
    under_cap.insert(
        "x-apiosk-max-buyer-total-atomic",
        HeaderValue::from_static("54347"),
    );
    assert_eq!(
        authorize(under_cap.clone(), resolved.clone())
            .await
            .unwrap_err()
            .status(),
        StatusCode::CONFLICT
    );
    assert!(
        !build_preflight_verdict(&state, &under_cap, &resolved)
            .await
            .would_succeed
    );
    // Domain restrictions still apply to internal calls.
    sqlx::query("UPDATE agent_wallet_connect_tokens SET allowed_domains = ARRAY['other.example']")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        authorize(headers.clone(), resolved.clone())
            .await
            .unwrap_err()
            .status(),
        StatusCode::FORBIDDEN
    );
    sqlx::query("UPDATE agent_wallet_connect_tokens SET allowed_domains = ARRAY[]::text[]")
        .execute(&pool)
        .await
        .unwrap();
    // A real x402/resell source cannot enter the internal path.
    resolved.resell = Some(47259);
    assert!(!resolved.uses_app_balance());
    assert_eq!(
        authorize(headers.clone(), resolved.clone())
            .await
            .unwrap_err()
            .status(),
        StatusCode::FORBIDDEN
    );
    resolved.resell = None;
    resolved.api.apiosk_revenue = false;
    assert_eq!(
        authorize(headers.clone(), resolved.clone())
            .await
            .unwrap_err()
            .status(),
        StatusCode::FORBIDDEN
    );
    resolved.api.apiosk_revenue = true;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let replay_key = "ordinary-api-replay";
    let fingerprint = hex::encode(sha2::Sha256::digest(b"GET\n/search"));
    assert!(matches!(
        state
            .db
            .claim_idempotency_key(token_id, replay_key, &fingerprint)
            .await
            .unwrap(),
        crate::db::IdempotencyClaim::Claimed
    ));
    let uri: Uri = "/search".parse().unwrap();
    let mut request = Some(
        Request::builder()
            .uri("/search")
            .body(Body::empty())
            .unwrap(),
    );
    let response = forward_api_request(
        &state,
        "balance-fixture",
        "/search",
        &Method::GET,
        &headers,
        &uri,
        false,
        &tracking,
        &mut request,
        &resolved,
        decision,
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-apiosk-payment-rail"],
        "apiosk_balance"
    );
    assert_eq!(response.headers()["x-apiosk-buyer-total-atomic"], "54348");
    assert!(response.headers().get("x-apiosk-tx-hash").is_none());
    assert!(response.headers().get("x-apiosk-wallet-used").is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let (amount, provider, wallet, network, managed): (BigDecimal, BigDecimal, Option<String>, Option<Uuid>, Option<Uuid>) = sqlx::query_as("SELECT gross_amount, developer_amount, wallet_address, network_token_id, managed_wallet_id FROM settlements").fetch_one(&pool).await.unwrap();
    assert_eq!(amount, "0.054348".parse::<BigDecimal>().unwrap());
    assert_eq!(provider, BigDecimal::from(0));
    assert!(wallet.is_none() && network.is_none() && managed.is_none());
    let usage: i64 = sqlx::query_scalar("SELECT count(*) FROM gateway_usage WHERE paid AND wallet_address IS NULL AND managed_wallet_id IS NULL AND network_token_id IS NULL").fetch_one(&pool).await.unwrap();
    assert_eq!(usage, 1);
    // Exercise the real request handler's replay branch with the stored
    // successful source response. It must return before any upstream work.
    let saved_headers: serde_json::Map<String, serde_json::Value> = response
        .headers()
        .iter()
        .map(|(key, value)| {
            (
                key.as_str().to_string(),
                serde_json::json!(value.to_str().unwrap()),
            )
        })
        .collect();
    let saved_body = axum::body::to_bytes(response.into_body(), 16384)
        .await
        .unwrap();
    assert!(state
        .db
        .finalize_idempotency_record(
            token_id,
            replay_key,
            200,
            serde_json::Value::Object(saved_headers),
            &saved_body
        )
        .await
        .unwrap());
    let mut replay_request = Request::builder()
        .uri("/search")
        .body(Body::empty())
        .unwrap();
    *replay_request.headers_mut() = headers.clone();
    replay_request.headers_mut().insert(
        "idempotency-key",
        HeaderValue::from_static("ordinary-api-replay"),
    );
    let replay = handle_flow::handle(state.clone(), "balance-fixture", "/search", replay_request)
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.headers()["x-apiosk-idempotent-replay"], "true");
    assert_eq!(replay.headers()["x-apiosk-payment-rail"], "apiosk_balance");
    assert_eq!(
        axum::body::to_bytes(replay.into_body(), 16384)
            .await
            .unwrap(),
        saved_body
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // Recording the same completed request twice cannot duplicate its ledger.
    settle_deferred_api_payment(
        &state,
        "balance-fixture",
        "/search",
        &resolved.api,
        resolved.endpoint.as_ref().unwrap(),
        &DeferredApiPayment::AppBalance,
        &tracking,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM settlements")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    // An upstream failure does not produce a second settlement.
    source_status.store(503, Ordering::SeqCst);
    let mut failed_tracking = tracking.clone();
    failed_tracking.request_id = Some(Uuid::from_u128(6));
    let decision = authorize(headers.clone(), resolved.clone()).await.unwrap();
    let mut request = Some(
        Request::builder()
            .uri("/search")
            .body(Body::empty())
            .unwrap(),
    );
    let response = forward_api_request(
        &state,
        "balance-fixture",
        "/search",
        &Method::GET,
        &headers,
        &uri,
        false,
        &failed_tracking,
        &mut request,
        &resolved,
        decision,
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()["x-apiosk-buyer-total-atomic"], "0");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM settlements")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    // Database metadata must exclude a resell source even if its ordinary
    // revenue flags are accidentally left set.
    sqlx::query("UPDATE apis SET listing_metadata = '{\"resell\":{\"enabled\":true}}'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        !state
            .db
            .get_api_by_slug("balance-fixture")
            .await
            .unwrap()
            .unwrap()
            .apiosk_revenue
    );
    server.abort();
}
