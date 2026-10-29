use super::agent_execute::agent_execute_response;
use super::wallet::{is_idempotent_method, ConnectTokenContext};
use super::{
    append_forward_query, build_agent_listing_metadata, build_agent_wallet_internal_tx_hash,
    build_execute_query, derive_default_operation, find_agent_operation, hash_connect_token,
    is_valid_api_slug, join_origin_url, normalize_path, normalize_wallet_like,
    payment_checkout_page, should_charge_credits_after_upstream,
    should_settle_deferred_api_payment, substitute_operation_path, uses_execute_envelope,
    wallets_match, AgentOperation, AllowedApiPaymentRails, CheckoutBanner, CheckoutBannerTone,
    PaymentCheckoutOptions,
};
use crate::api_management::helpers::validate_upstream_url;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use bigdecimal::BigDecimal;
use serde_json::json;
use uuid::Uuid;

use crate::auth::{extract_agent_connect_token, should_strip_forwarded_header};
use crate::db::{AgentConnectToken, AgentConnectTokenWallet, Endpoint};
use crate::payment::PaymentRequirement;
use crate::proxy::payment_flow::checkout_options_for_rails;

#[test]
fn extract_connect_token_from_custom_header() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-apiosk-connect-token",
        HeaderValue::from_static("aw_live_abc_123"),
    );
    let token = extract_agent_connect_token(&headers).expect("token");
    assert_eq!(token, "aw_live_abc_123");
}

#[test]
fn extract_connect_token_from_bearer() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_static("Bearer aw_live_abc123_deadbeefdeadbeef"),
    );
    let token = extract_agent_connect_token(&headers).expect("token");
    assert_eq!(token, "aw_live_abc123_deadbeefdeadbeef");
}

#[test]
fn non_connect_authorization_is_not_treated_as_wallet_token() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_static("Bearer ghp_example_token"),
    );
    assert!(extract_agent_connect_token(&headers).is_none());
}

#[test]
fn forwarded_requests_strip_connect_token_authorization_only() {
    let connect_value = HeaderValue::from_static("Bearer aw_live_abc_1234567890");
    let upstream_value = HeaderValue::from_static("Bearer ghp_example_token");

    assert!(should_strip_forwarded_header(
        header::AUTHORIZATION.as_str(),
        &connect_value
    ));
    assert!(!should_strip_forwarded_header(
        header::AUTHORIZATION.as_str(),
        &upstream_value
    ));
}

#[test]
fn hash_connect_token_is_deterministic() {
    let one = hash_connect_token("aw_live_test");
    let two = hash_connect_token("aw_live_test");
    assert_eq!(one, two);
    assert_eq!(one.len(), 64);
}

#[test]
fn internal_agent_wallet_tx_hash_is_unique() {
    let wallet_id = Uuid::parse_str("f7f23c1c-5acf-49f4-8d99-2a47032ad66a").unwrap();
    let one = build_agent_wallet_internal_tx_hash(wallet_id);
    let two = build_agent_wallet_internal_tx_hash(wallet_id);

    assert!(one.starts_with("aw_internal_"));
    assert!(one.contains(&wallet_id.simple().to_string()));
    assert_ne!(one, two);
}

fn make_token_wallet(address: &str) -> AgentConnectTokenWallet {
    AgentConnectTokenWallet {
        wallet_id: Uuid::new_v4(),
        wallet_address: address.to_string(),
        wallet_status: "active".to_string(),
        daily_limit_usdc: BigDecimal::from(25_i32),
        per_tx_limit_usdc: BigDecimal::from(5_i32),
    }
}

fn make_token(wallets: Vec<AgentConnectTokenWallet>, _legacy_sepa: bool) -> AgentConnectToken {
    AgentConnectToken {
        token_id: Uuid::new_v4(),
        user_id: Uuid::new_v4(),
        wallets,
        allowed_domains: Vec::new(),
        label: None,
        expires_at: None,
        policy_id: None,
    }
}

#[test]
fn connect_token_find_wallet_is_case_insensitive() {
    let token = make_token(
        vec![make_token_wallet(
            "0xB581c1F0d12331E3848043760B830695D3480574",
        )],
        false,
    );
    assert!(token
        .find_wallet("0xb581c1f0d12331e3848043760b830695d3480574")
        .is_some());
    assert!(token
        .find_wallet("0xB581C1F0D12331E3848043760B830695D3480574")
        .is_some());
    assert!(token.find_wallet("0xdead").is_none());
}

#[test]
fn connect_token_context_sole_wallet_requires_exactly_one() {
    let zero_wallet_ctx = ConnectTokenContext {
        token: make_token(vec![], true),
    };
    assert!(zero_wallet_ctx.sole_wallet().is_none());
    assert!(!zero_wallet_ctx.has_wallets());

    let one_wallet_ctx = ConnectTokenContext {
        token: make_token(
            vec![make_token_wallet(
                "0xB581c1F0d12331E3848043760B830695D3480574",
            )],
            false,
        ),
    };
    assert!(one_wallet_ctx.sole_wallet().is_some());
    assert!(one_wallet_ctx.has_wallets());

    let multi_wallet_ctx = ConnectTokenContext {
        token: make_token(
            vec![
                make_token_wallet("0xB581c1F0d12331E3848043760B830695D3480574"),
                make_token_wallet("0xC692d2E1e234423E48580F4760B830695D348AAA"),
            ],
            false,
        ),
    };
    assert!(multi_wallet_ctx.sole_wallet().is_none());
    assert!(multi_wallet_ctx.has_wallets());
}

#[test]
fn wallet_normalization_and_match() {
    let a = "0xB581c1F0d12331E3848043760B830695D3480574";
    let b = "0xb581c1f0d12331e3848043760b830695d3480574";
    assert_eq!(
        normalize_wallet_like(a).as_deref(),
        Some("0xb581c1f0d12331e3848043760b830695d3480574")
    );
    assert!(wallets_match(a, b));
    assert!(!wallets_match("0x1234", b));
}

#[test]
fn deferred_payment_settles_only_on_upstream_success() {
    // Settle when the provider actually served the call.
    assert!(should_settle_deferred_api_payment(StatusCode::OK));
    assert!(should_settle_deferred_api_payment(StatusCode::NO_CONTENT));
    // Never charge the buyer when the provider returned an error — client
    // errors (bad params, API-not-enabled, auth, not-found) included, not just
    // 5xx. This is the buyer-protection fix.
    assert!(!should_settle_deferred_api_payment(StatusCode::BAD_REQUEST));
    assert!(!should_settle_deferred_api_payment(
        StatusCode::UNAUTHORIZED
    ));
    assert!(!should_settle_deferred_api_payment(StatusCode::FORBIDDEN));
    assert!(!should_settle_deferred_api_payment(StatusCode::NOT_FOUND));
    assert!(!should_settle_deferred_api_payment(
        StatusCode::UNPROCESSABLE_ENTITY
    ));
    assert!(!should_settle_deferred_api_payment(
        StatusCode::INTERNAL_SERVER_ERROR
    ));
    assert!(!should_settle_deferred_api_payment(StatusCode::BAD_GATEWAY));
}

#[test]
fn credits_charge_only_on_upstream_success() {
    assert!(should_charge_credits_after_upstream(StatusCode::OK));
    assert!(should_charge_credits_after_upstream(StatusCode::NO_CONTENT));
    assert!(!should_charge_credits_after_upstream(
        StatusCode::BAD_REQUEST
    ));
    assert!(!should_charge_credits_after_upstream(StatusCode::NOT_FOUND));
    assert!(!should_charge_credits_after_upstream(
        StatusCode::INTERNAL_SERVER_ERROR
    ));
}

#[test]
fn join_origin_url_keeps_root_requests_on_base_endpoint() {
    assert_eq!(
        join_origin_url("https://zep-pay.netlify.app/api/payment-links", "/"),
        "https://zep-pay.netlify.app/api/payment-links"
    );
    assert_eq!(
        join_origin_url("https://zep-pay.netlify.app/api/payment-links/", "/"),
        "https://zep-pay.netlify.app/api/payment-links"
    );
}

#[test]
fn join_origin_url_appends_non_root_paths() {
    assert_eq!(
        join_origin_url(
            "https://zep-pay.netlify.app/api/payment-links",
            "/abc/status"
        ),
        "https://zep-pay.netlify.app/api/payment-links/abc/status"
    );
    assert_eq!(
        join_origin_url(
            "https://zep-pay.netlify.app/api/payment-links/",
            "abc/status"
        ),
        "https://zep-pay.netlify.app/api/payment-links/abc/status"
    );
}

#[test]
fn find_agent_operation_matches_path_key_and_alias() {
    let operations = vec![AgentOperation {
        id: "/extract".to_string(),
        key: "POST /extract".to_string(),
        method: "POST".to_string(),
        path: "/extract".to_string(),
        gateway_url: "https://gateway.apiosk.com/agent-docs-extract/extract".to_string(),
        execute_url: "https://gateway.apiosk.com/agent-docs-extract/execute".to_string(),
        payment_required: true,
        price_usd: 0.03,
        description: None,
        request_body: None,
        response_body: None,
        request_example: None,
        response_example: None,
        headers: None,
        query_params: None,
    }];

    assert_eq!(
        find_agent_operation(&operations, "/extract")
            .expect("path match")
            .key,
        "POST /extract"
    );
    assert_eq!(
        find_agent_operation(&operations, "POST /extract")
            .expect("key match")
            .id,
        "/extract"
    );
    assert_eq!(
        find_agent_operation(&operations, "extract")
            .expect("alias match")
            .path,
        "/extract"
    );
}

#[test]
fn substitute_operation_path_replaces_placeholders() {
    let params = json!({
        "payment_id": "pay_123",
    });

    assert_eq!(
        substitute_operation_path("/payments/{payment_id}/status", Some(&params))
            .expect("curly path"),
        "/payments/pay_123/status"
    );
    assert_eq!(
        substitute_operation_path("/payments/:payment_id/status", Some(&params))
            .expect("colon path"),
        "/payments/pay_123/status"
    );
}

#[test]
fn execute_query_uses_get_input_and_preserves_existing_query() {
    let operation = AgentOperation {
        id: "/current".to_string(),
        key: "GET /current".to_string(),
        method: "GET".to_string(),
        path: "/current".to_string(),
        gateway_url: "https://gateway.apiosk.com/weather/current".to_string(),
        execute_url: "https://gateway.apiosk.com/weather/execute".to_string(),
        payment_required: true,
        price_usd: 0.003,
        description: None,
        request_body: None,
        response_body: None,
        request_example: None,
        response_example: None,
        headers: None,
        query_params: None,
    };

    let query = build_execute_query(
        &operation,
        None,
        &json!({
            "city": "Amsterdam",
            "units": "metric"
        }),
    )
    .expect("query");

    assert_eq!(
        append_forward_query(
            "https://example.com/current",
            Some("lang=en"),
            query.as_deref()
        ),
        "https://example.com/current?lang=en&city=Amsterdam&units=metric"
    );
}

#[test]
fn raw_execute_payload_with_query_field_is_not_treated_as_envelope() {
    let payload = json!({
        "query": "What does Apiosk do?",
        "context": "Apiosk is a pay-per-call API marketplace."
    });

    assert!(!uses_execute_envelope(payload.as_object().expect("object")));
}

#[test]
fn execute_envelope_requires_real_control_keys() {
    let payload = json!({
        "operation": "query",
        "input": {
            "query": "What does Apiosk do?"
        }
    });

    assert!(uses_execute_envelope(payload.as_object().expect("object")));
}

fn sample_requirement() -> PaymentRequirement {
    PaymentRequirement {
        scheme: "exact".to_string(),
        network: "base".to_string(),
        max_amount_required: "250000".to_string(),
        resource: "https://gateway.apiosk.com/sample-api/hello".to_string(),
        description: "API call to sample-api".to_string(),
        mime_type: "application/json".to_string(),
        pay_to: "0xB581c1F0d12331E3848043760B830695D3480574".to_string(),
        max_timeout_seconds: 300,
        asset: "0x833589fCD6EDB6E08f4c7C32D4f71b54bdA02913".to_string(),
        extra: json!({ "name": "USD Coin", "version": "2" }),
        extensions: None,
        output_schema: None,
    }
}

#[tokio::test]
async fn hosted_checkout_renders_dual_rail_actions() {
    let mut options = PaymentCheckoutOptions::new("https://dashboard.apiosk.com".to_string());
    options.allow_credits = true;
    options.allow_x402 = true;
    options.banner = Some(CheckoutBanner {
        tone: CheckoutBannerTone::Warning,
        title: "Insufficient credits".to_string(),
        message: "Top up and retry automatically.".to_string(),
    });

    let response = payment_checkout_page(&sample_requirement(), "sample-api", "/hello", &options);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("checkout body");
    let html = String::from_utf8(body.to_vec()).expect("utf8 html");

    assert!(html.contains("Use credits"));
    assert!(html.contains("Top up and retry"));
    assert!(html.contains("Pay from your wallet"));
    assert!(html.contains("Connect wallet"));
    assert!(html.contains("Insufficient credits"));
    assert!(html.contains("\"allowCredits\":true"));
    assert!(html.contains("\"allowX402\":true"));
}

#[tokio::test]
async fn hosted_checkout_hides_credits_section_when_unavailable() {
    let mut options = PaymentCheckoutOptions::new("https://dashboard.apiosk.com".to_string());
    options.allow_credits = false;
    options.allow_x402 = false;
    options.credits_actions_enabled = false;
    options.wallet_actions_enabled = false;
    options.banner = Some(CheckoutBanner {
        tone: CheckoutBannerTone::Warning,
        title: "Credits unavailable for this price".to_string(),
        message: "Use wallet checkout instead.".to_string(),
    });

    let response = payment_checkout_page(&sample_requirement(), "sample-api", "/hello", &options);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("checkout body");
    let html = String::from_utf8(body.to_vec()).expect("utf8 html");

    assert!(!html.contains("Use your Apiosk balance"));
    assert!(!html.contains("Use credits</button>"));
    assert!(!html.contains("Top up and retry</button>"));
    assert!(!html.contains("Pay from your wallet"));
    assert!(html.contains("\"creditsActionsEnabled\":false"));
    assert!(html.contains("\"allowCredits\":false"));
    assert!(html.contains("\"allowX402\":false"));
}

#[test]
fn checkout_options_hide_credits_for_non_whole_credit_prices() {
    let unsupported_endpoint = Endpoint {
        id: Uuid::new_v4(),
        api_id: Uuid::new_v4(),
        method: "GET".to_string(),
        path: "/weather/current".to_string(),
        payment_required: true,
        price: "0.001".parse::<BigDecimal>().expect("price"),
        network_token_id: None,
        origin_override: None,
        auth_override: None,
        cache_ttl_seconds: None,
    };

    let options = checkout_options_for_rails(
        "https://dashboard.apiosk.com",
        AllowedApiPaymentRails::new(true, true),
        &unsupported_endpoint,
        None,
    );

    assert!(!options.allow_credits);
    assert!(!options.credits_actions_enabled);
    assert!(options.allow_x402);
}

#[test]
fn checkout_options_keep_credits_for_whole_credit_prices() {
    let supported_endpoint = Endpoint {
        id: Uuid::new_v4(),
        api_id: Uuid::new_v4(),
        method: "POST".to_string(),
        path: "/forward".to_string(),
        payment_required: true,
        price: "0.01".parse::<BigDecimal>().expect("price"),
        network_token_id: None,
        origin_override: None,
        auth_override: None,
        cache_ttl_seconds: None,
    };

    let options = checkout_options_for_rails(
        "https://dashboard.apiosk.com",
        AllowedApiPaymentRails::new(true, true),
        &supported_endpoint,
        None,
    );

    assert!(options.allow_credits);
    assert!(options.credits_actions_enabled);
    assert!(options.allow_x402);
}

#[test]
fn checkout_options_disable_wallet_actions_when_hosted_wallet_checkout_is_unavailable() {
    let endpoint = Endpoint {
        id: Uuid::new_v4(),
        api_id: Uuid::new_v4(),
        method: "GET".to_string(),
        path: "/weather/current".to_string(),
        payment_required: true,
        price: "0.01".parse::<BigDecimal>().expect("price"),
        network_token_id: None,
        origin_override: None,
        auth_override: None,
        cache_ttl_seconds: None,
    };

    let options = checkout_options_for_rails(
        "https://dashboard.apiosk.com",
        AllowedApiPaymentRails::new(true, true),
        &endpoint,
        Some("Wallet checkout is disabled because the gateway is configured with the testnet-only x402.org facilitator."),
    );

    assert!(options.allow_x402);
    assert!(!options.wallet_actions_enabled);
    assert!(options
        .wallet_hint
        .as_deref()
        .unwrap_or_default()
        .contains("testnet-only x402.org facilitator"));
}

#[test]
fn default_operation_skips_health_and_prefers_generic_write_route() {
    let operations = vec![
        AgentOperation {
            id: "/".to_string(),
            key: "GET /".to_string(),
            method: "GET".to_string(),
            path: "/".to_string(),
            gateway_url: "https://gateway.apiosk.com/bella-pizza/".to_string(),
            execute_url: "https://gateway.apiosk.com/bella-pizza/execute".to_string(),
            payment_required: false,
            price_usd: 0.0,
            description: None,
            request_body: None,
            response_body: None,
            request_example: None,
            response_example: None,
            headers: None,
            query_params: None,
        },
        AgentOperation {
            id: "/health".to_string(),
            key: "GET /health".to_string(),
            method: "GET".to_string(),
            path: "/health".to_string(),
            gateway_url: "https://gateway.apiosk.com/bella-pizza/health".to_string(),
            execute_url: "https://gateway.apiosk.com/bella-pizza/execute".to_string(),
            payment_required: false,
            price_usd: 0.0,
            description: None,
            request_body: None,
            response_body: None,
            request_example: None,
            response_example: None,
            headers: None,
            query_params: None,
        },
        AgentOperation {
            id: "/order/small".to_string(),
            key: "POST /order/small".to_string(),
            method: "POST".to_string(),
            path: "/order/small".to_string(),
            gateway_url: "https://gateway.apiosk.com/bella-pizza/order/small".to_string(),
            execute_url: "https://gateway.apiosk.com/bella-pizza/execute".to_string(),
            payment_required: true,
            price_usd: 5.0,
            description: None,
            request_body: Some(serde_json::json!({
                "type": "object",
                "required": ["type"],
                "properties": {
                    "type": { "type": "string" }
                }
            })),
            response_body: None,
            request_example: None,
            response_example: None,
            headers: None,
            query_params: None,
        },
        AgentOperation {
            id: "/orders".to_string(),
            key: "POST /orders".to_string(),
            method: "POST".to_string(),
            path: "/orders".to_string(),
            gateway_url: "https://gateway.apiosk.com/bella-pizza/orders".to_string(),
            execute_url: "https://gateway.apiosk.com/bella-pizza/execute".to_string(),
            payment_required: true,
            price_usd: 5.0,
            description: None,
            request_body: Some(serde_json::json!({
                "type": "object",
                "required": ["type", "size"],
                "properties": {
                    "type": { "type": "string" },
                    "size": { "type": "string" }
                }
            })),
            response_body: None,
            request_example: None,
            response_example: None,
            headers: None,
            query_params: None,
        },
    ];

    let default_operation = derive_default_operation(&operations).expect("default operation");
    assert_eq!(default_operation.id, "/orders");
}

#[test]
fn listing_metadata_uses_default_operation_schema_for_mcp_tool() {
    let api_detail = crate::db::ApiDetail {
        id: Uuid::new_v4(),
        slug: "bella-pizza".to_string(),
        name: "La Bella Pizza API".to_string(),
        description: "Order fictional pizzas".to_string(),
        origin_url: "https://order-pizza-api.fly.dev".to_string(),
        docs_url: None,
        category: Some("demo".to_string()),
        listing_type: Some("api".to_string()),
        listing_metadata: serde_json::json!({}),
        long_description: None,
        example_usage: None,
        authentication_info: None,
        active: Some(true),
        is_verified: Some(true),
        is_external: false,
        external_x402: serde_json::json!([]),
        created_at: chrono::Utc::now(),
    };
    let operations = vec![
        AgentOperation {
            id: "/health".to_string(),
            key: "GET /health".to_string(),
            method: "GET".to_string(),
            path: "/health".to_string(),
            gateway_url: "https://gateway.apiosk.com/bella-pizza/health".to_string(),
            execute_url: "https://gateway.apiosk.com/bella-pizza/execute".to_string(),
            payment_required: false,
            price_usd: 0.0,
            description: None,
            request_body: None,
            response_body: None,
            request_example: None,
            response_example: None,
            headers: None,
            query_params: None,
        },
        AgentOperation {
            id: "/orders".to_string(),
            key: "POST /orders".to_string(),
            method: "POST".to_string(),
            path: "/orders".to_string(),
            gateway_url: "https://gateway.apiosk.com/bella-pizza/orders".to_string(),
            execute_url: "https://gateway.apiosk.com/bella-pizza/execute".to_string(),
            payment_required: true,
            price_usd: 5.0,
            description: Some("Create a pizza order with custom size".to_string()),
            request_body: Some(serde_json::json!({
                "type": "object",
                "required": ["type", "size"],
                "properties": {
                    "type": { "type": "string" },
                    "size": { "type": "string" }
                }
            })),
            response_body: None,
            request_example: None,
            response_example: None,
            headers: None,
            query_params: None,
        },
    ];

    let metadata = build_agent_listing_metadata(&api_detail, &operations);
    assert_eq!(
        metadata
            .get("default_operation")
            .and_then(|value| value.as_str()),
        Some("/orders")
    );
    assert_eq!(
        metadata
            .get("mcp_tool")
            .and_then(|value| value.get("inputSchema"))
            .cloned(),
        operations[1].request_body
    );
}

#[test]
fn slug_validator_accepts_well_formed_slugs() {
    assert!(is_valid_api_slug("weather"));
    assert!(is_valid_api_slug("sendcloud-rates"));
    assert!(is_valid_api_slug("api-v2-2026"));
}

#[test]
fn slug_validator_rejects_bad_slugs() {
    assert!(!is_valid_api_slug(""));
    assert!(!is_valid_api_slug("-leading-hyphen"));
    assert!(!is_valid_api_slug("trailing-hyphen-"));
    assert!(!is_valid_api_slug("UPPER"));
    assert!(!is_valid_api_slug("with spaces"));
    assert!(!is_valid_api_slug("with/slash"));
    assert!(!is_valid_api_slug(&"a".repeat(65)));
}

#[test]
fn normalize_path_strips_traversal_and_control_bytes() {
    assert_eq!(normalize_path("/v1/forecast"), "/v1/forecast");
    assert_eq!(normalize_path("v1/forecast"), "/v1/forecast");
    assert_eq!(normalize_path("/v1/../../etc/passwd"), "/");
    assert_eq!(normalize_path("/v1/forecast/../v2"), "/");
    assert_eq!(normalize_path(""), "/");
    assert_eq!(normalize_path("/with\0null"), "/");
}

#[test]
fn upstream_validator_blocks_ssrf_targets() {
    assert!(validate_upstream_url("https://api.sendcloud.sc/v2/parcels").is_ok());
    assert!(validate_upstream_url("http://api.example.com").is_err()); // not https
    assert!(validate_upstream_url("https://localhost/").is_err());
    assert!(validate_upstream_url("https://127.0.0.1/").is_err());
    assert!(validate_upstream_url("https://10.0.0.1/").is_err());
    assert!(validate_upstream_url("https://192.168.1.1/").is_err());
    assert!(validate_upstream_url("https://169.254.169.254/").is_err()); // metadata
    assert!(validate_upstream_url("https://internal-svc.internal/").is_err());
    assert!(validate_upstream_url("not-a-url").is_err());
}

#[test]
fn hmac_sha256_signature_is_deterministic_and_uses_canonical_form() {
    use super::compute_hmac_sha256_signature;
    use axum::http::Method;
    let body = br#"{"hello":"world"}"#;
    let s1 =
        compute_hmac_sha256_signature(&Method::POST, "/v1/echo", body, "secret-1234567890abcdef");
    let s2 =
        compute_hmac_sha256_signature(&Method::POST, "/v1/echo", body, "secret-1234567890abcdef");
    assert_eq!(s1, s2);
    assert_eq!(s1.len(), 64);
    let s3 =
        compute_hmac_sha256_signature(&Method::GET, "/v1/echo", body, "secret-1234567890abcdef");
    assert_ne!(s1, s3, "method change must change signature");
    let s4 =
        compute_hmac_sha256_signature(&Method::POST, "/v2/echo", body, "secret-1234567890abcdef");
    assert_ne!(s1, s4, "path change must change signature");
    let s5 =
        compute_hmac_sha256_signature(&Method::POST, "/v1/echo", body, "different-secret-here");
    assert_ne!(s1, s5, "secret change must change signature");
}

// ---------------------------------------------------------------------------
// Upstream forwarding / response-code handling
// ---------------------------------------------------------------------------

#[test]
fn idempotent_methods_are_retry_safe() {
    // The retry loop in forward.rs only re-sends these after a transport error;
    // re-sending a POST/PATCH could double-execute (and double-charge) a call.
    for m in [
        Method::GET,
        Method::HEAD,
        Method::OPTIONS,
        Method::PUT,
        Method::DELETE,
        Method::TRACE,
    ] {
        assert!(is_idempotent_method(&m), "{m} should be retry-safe");
    }
    assert!(!is_idempotent_method(&Method::POST));
    assert!(!is_idempotent_method(&Method::PATCH));
}

#[test]
fn redirects_and_3xx_do_not_settle_or_charge() {
    // 3xx is not a success: a redirect/“not modified” must never bill the buyer.
    for code in [
        StatusCode::MOVED_PERMANENTLY,
        StatusCode::FOUND,
        StatusCode::NOT_MODIFIED,
    ] {
        assert!(!should_settle_deferred_api_payment(code), "{code} settled");
        assert!(
            !should_charge_credits_after_upstream(code),
            "{code} charged"
        );
    }
}

#[test]
fn transparent_proxy_strips_hop_by_hop_and_internal_headers() {
    let v = HeaderValue::from_static("x");
    // Hop-by-hop / encoding headers the gateway regenerates per upstream call.
    for h in [
        "host",
        "accept-encoding",
        "content-length",
        "transfer-encoding",
    ] {
        assert!(should_strip_forwarded_header(h, &v), "{h} must be stripped");
    }
    // Payment proof + internal gateway headers must never reach the origin.
    assert!(should_strip_forwarded_header("x-payment", &v));
    assert!(should_strip_forwarded_header("x-payment-extra", &v));
    assert!(should_strip_forwarded_header("x-apiosk-connect-token", &v));
    assert!(should_strip_forwarded_header("x-apiosk-request-id", &v));
    assert!(should_strip_forwarded_header("x-apiosk-api-key", &v));
    // Ordinary client headers are forwarded verbatim (transparent proxy).
    assert!(!should_strip_forwarded_header("accept", &v));
    assert!(!should_strip_forwarded_header("user-agent", &v));
}

#[test]
fn upstream_validator_blocks_ipv6_private_local_suffix_and_overlong_urls() {
    // IPv6 loopback / unique-local / link-local must all be blocked.
    assert!(validate_upstream_url("https://[::1]/x").is_err());
    assert!(validate_upstream_url("https://[fc00::1]/x").is_err());
    assert!(validate_upstream_url("https://[fe80::1]/x").is_err());
    // Reserved internal suffixes (not just `.internal`).
    assert!(validate_upstream_url("https://service.local/x").is_err());
    assert!(validate_upstream_url("https://service.localhost/x").is_err());
    // Over-length URL (>1024 chars) is rejected before any network use.
    let long = format!("https://api.example.com/{}", "a".repeat(1100));
    assert!(validate_upstream_url(&long).is_err());
    // A normal public HTTPS endpoint is still allowed.
    assert!(validate_upstream_url("https://api.example.com/v1/data").is_ok());
}

fn sample_operation() -> AgentOperation {
    AgentOperation {
        id: "/extract".to_string(),
        key: "POST /extract".to_string(),
        method: "POST".to_string(),
        path: "/extract".to_string(),
        gateway_url: "https://gateway.apiosk.com/x/extract".to_string(),
        execute_url: "https://gateway.apiosk.com/x/execute".to_string(),
        payment_required: true,
        price_usd: 0.03,
        description: None,
        request_body: None,
        response_body: None,
        request_example: None,
        response_example: None,
        headers: None,
        query_params: None,
    }
}

#[tokio::test]
async fn agent_execute_response_maps_2xx_to_success_with_quoted_cost() {
    let execution = super::VirtualExecuteRequest {
        operation: sample_operation(),
        upstream_method: Method::GET,
        upstream_path: "/extract".to_string(),
        upstream_query: None,
        upstream_body_bytes: vec![],
    };
    let mut resp_headers = HeaderMap::new();
    resp_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    let resp = agent_execute_response(
        "weather-api",
        &execution,
        None, // no endpoint => quoted cost falls back to operation.price_usd
        StatusCode::OK,
        &resp_headers,
        br#"{"ok":true}"#,
        12,
        None, // settled_price_paid_usdc
        None, // credits_charge
        None, // settled_receipt
    );
    // The wrapper echoes the upstream HTTP status (transparent proxy).
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["status"], "success");
    assert_eq!(json["upstream_status"], 200);
    assert_eq!(json["cost"], 0.03);
    assert!(json.get("error").is_none());
}

#[tokio::test]
async fn agent_execute_response_maps_upstream_error_to_error_and_zero_cost() {
    let execution = super::VirtualExecuteRequest {
        operation: sample_operation(),
        upstream_method: Method::POST,
        upstream_path: "/extract".to_string(),
        upstream_query: None,
        upstream_body_bytes: vec![],
    };
    let mut resp_headers = HeaderMap::new();
    resp_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    let resp = agent_execute_response(
        "weather-api",
        &execution,
        None,
        StatusCode::BAD_GATEWAY,
        &resp_headers,
        br#"{"error":"upstream boom"}"#,
        7,
        None,
        None,
        None,
    );
    // Upstream status is preserved on the wrapper response, too.
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["status"], "error");
    assert_eq!(json["upstream_status"], 502);
    // The buyer is never billed for an upstream failure.
    assert_eq!(json["cost"], 0.0);
    assert_eq!(json["error"]["code"], "upstream_error");
}

fn endpoint_row(
    origin_override: Option<&str>,
    auth_override: Option<serde_json::Value>,
) -> crate::db::Endpoint {
    crate::db::Endpoint {
        id: uuid::Uuid::nil(),
        api_id: uuid::Uuid::nil(),
        method: "GET".to_string(),
        path: "/api/v1/thing".to_string(),
        payment_required: true,
        price: bigdecimal::BigDecimal::from(0),
        network_token_id: None,
        origin_override: origin_override.map(str::to_string),
        auth_override,
        cache_ttl_seconds: None,
    }
}

#[test]
fn preset_auth_defaults_to_listing_credentials() {
    let listing = vec![("apikey".to_string(), "private".to_string())];
    let (headers, query) = super::handle_flow::select_preset_auth(
        Some(&endpoint_row(None, None)),
        listing.clone(),
        vec![],
    );
    assert_eq!(headers, listing);
    assert!(query.is_empty());
}

#[test]
fn origin_override_without_auth_strips_listing_credentials() {
    // The listing credential must never cross to an override host.
    let listing = vec![("apikey".to_string(), "private".to_string())];
    let (headers, query) = super::handle_flow::select_preset_auth(
        Some(&endpoint_row(Some("https://opendata.kvk.nl"), None)),
        listing,
        vec![("token".to_string(), "private".to_string())],
    );
    assert!(headers.is_empty());
    assert!(query.is_empty());
}

#[test]
fn auth_override_replaces_listing_credentials_entirely() {
    let listing = vec![("apikey".to_string(), "private".to_string())];
    let auth = serde_json::json!({
        "auth_headers": [{"name": "apikey", "value": "public-open-data"}]
    });
    let (headers, query) = super::handle_flow::select_preset_auth(
        Some(&endpoint_row(Some("https://opendata.kvk.nl"), Some(auth))),
        listing,
        vec![("token".to_string(), "private".to_string())],
    );
    assert_eq!(
        headers,
        vec![("apikey".to_string(), "public-open-data".to_string())]
    );
    assert!(query.is_empty());
}

#[test]
fn identifier_link_execute_queries_preserve_structured_arrays() {
    for (path, key, value) in [
        (
            "/apiosk/company-address",
            "addresses",
            serde_json::json!([{"type":"bezoekadres","land":"Nederland","postcode":"1015CW","huisnummer":126}]),
        ),
        (
            "/apiosk/linked-parcels",
            "designations",
            serde_json::json!(["ASD09-M-6676", "ASD09-M-6965"]),
        ),
    ] {
        let mut operation = sample_operation();
        operation.path = path.into();
        operation.method = "GET".into();
        let query = build_execute_query(&operation, None, &serde_json::json!({key:value}))
            .unwrap()
            .unwrap();
        let pairs: Vec<_> = url::form_urlencoded::parse(query.as_bytes()).collect();
        assert_eq!(pairs.len(), 1);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&pairs[0].1).unwrap(),
            value
        );
        assert!(super::public_sources::prepare(
            "pdok",
            "GET",
            &format!("https://api.pdok.nl{path}?{query}")
        )
        .unwrap()
        .is_some());
    }
}

#[tokio::test]
async fn settled_receipt_reports_provider_price_separately_from_buyer_total() {
    let receipt = super::SettledPaymentReceipt {
        price_listed_usdc: 0.002,
        price_paid_usdc: 0.0023,
        tx_hash: "test-receipt".into(),
        wallet_used: None,
    };
    // The normal proxy and agent envelope share this exact header builder.
    let direct = receipt
        .price_headers(axum::http::Response::builder())
        .body(())
        .unwrap();
    assert_eq!(direct.headers()["X-Apiosk-Price-Listed"], "0.002000");
    assert_eq!(direct.headers()["X-Apiosk-Price-Paid"], "0.002300");
    // /v1/do quotes from the listed header: applying the buyer fee once must
    // agree with the actual debit, including rounding for micro-USDC prices.
    let listed = direct.headers()["X-Apiosk-Price-Listed"]
        .to_str()
        .unwrap()
        .parse::<BigDecimal>()
        .unwrap();
    let quote = crate::fees::quote_usdc(&listed, crate::fees::ProviderClass::Federated);
    assert_eq!(quote.buyer_total_atomic, 2300);
    let execution = super::VirtualExecuteRequest {
        operation: sample_operation(),
        upstream_method: Method::GET,
        upstream_path: "/v1/recherche".into(),
        upstream_query: None,
        upstream_body_bytes: vec![],
    };
    let mut endpoint = endpoint_row(None, None);
    endpoint.price = "0.002".parse().unwrap();
    let response = agent_execute_response(
        "sirenic",
        &execution,
        Some(&endpoint),
        StatusCode::OK,
        &HeaderMap::new(),
        b"ok",
        1,
        Some(0.0023),
        None,
        Some(&receipt),
    );
    assert_eq!(response.headers()["X-Apiosk-Price-Listed"], "0.002000");
    assert_eq!(response.headers()["X-Apiosk-Price-Paid"], "0.002300");
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["price_listed_usdc"], 0.002);
    assert_eq!(body["cost"], 0.0023);
}

#[tokio::test]
async fn ordinary_api_envelope_reports_internal_balance_without_wallet_or_chain_hash() {
    let receipt = super::SettledPaymentReceipt {
        price_listed_usdc: 0.047259,
        price_paid_usdc: 0.054348,
        tx_hash: "apiosk_revenue_source_request".into(),
        wallet_used: None,
    };
    let execution = super::VirtualExecuteRequest {
        operation: sample_operation(), upstream_method: Method::GET,
        upstream_path: "/search".into(), upstream_query: None, upstream_body_bytes: vec![],
    };
    let response = agent_execute_response("ordinary-api", &execution, None, StatusCode::OK,
        &HeaderMap::new(), b"{\"results\":[]}", 1, Some(0.054348), None, Some(&receipt));
    assert_eq!(response.headers()["x-apiosk-payment-rail"], "apiosk_balance");
    assert!(response.headers().get("x-apiosk-tx-hash").is_none());
    assert!(response.headers().get("x-apiosk-wallet-used").is_none());
    let bytes = axum::body::to_bytes(response.into_body(), 16384).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["payment_rail"], "apiosk_balance");
    assert!(body.get("tx_hash").is_none());
    assert!(body.get("wallet_used").is_none());
    let chain_receipt = super::SettledPaymentReceipt { tx_hash: "0xactual-chain-transaction".into(), ..receipt };
    assert_eq!(chain_receipt.payment_rail(), "x402");
    assert_eq!(chain_receipt.onchain_tx_hash(), Some("0xactual-chain-transaction"));
}
