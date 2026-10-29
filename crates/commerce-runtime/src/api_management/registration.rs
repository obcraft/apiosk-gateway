use std::sync::Arc;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use tracing::{error, info};
use uuid::Uuid;

use crate::agent_metadata::{
    default_root_operations, normalize_listing_metadata, AgentMetadataContext,
};
use crate::{auth::verify_wallet_auth, AppState};

use super::{
    helpers::{
        perform_health_check, registration_message, registration_status, reject_register,
        to_bigdecimal, validate_price,
    },
    RegisterApiRequest, RegisterApiResponse,
};

pub async fn register_api(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<RegisterApiRequest>,
) -> Result<Json<RegisterApiResponse>, StatusCode> {
    info!(
        "Registering new API: slug={}, owner={}",
        req.slug, req.owner_wallet
    );

    if !req
        .slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Ok(reject_register(
            &req.slug,
            "Invalid slug format. Use lowercase alphanumeric and hyphens only.",
        ));
    }

    if !validate_price(req.price_usd) {
        return Ok(reject_register(
            &req.slug,
            "Price must be between 0.0001 and 10.00 USD",
        ));
    }

    if !req.owner_wallet.starts_with("0x") || req.owner_wallet.len() != 42 {
        return Ok(reject_register(&req.slug, "Invalid wallet address format"));
    }

    let action_resource = format!("register:{}", req.slug);
    let authed_wallet = match verify_wallet_auth(
        &headers,
        &state.auth_replay_cache,
        "register_api",
        &action_resource,
        Some(&req.owner_wallet),
    )
    .await
    {
        Ok(wallet) => wallet,
        Err(_) => {
            return Ok(reject_register(
                &req.slug,
                "Unauthorized: missing or invalid wallet signature",
            ));
        }
    };

    if let Err(reason) = super::helpers::validate_upstream_url(&req.endpoint_url) {
        return Ok(reject_register(&req.slug, reason));
    }

    match state.db.get_api_by_slug(&req.slug).await {
        Ok(Some(_)) => {
            return Ok(reject_register(
                &req.slug,
                format!("API slug '{}' already registered", req.slug),
            ));
        }
        Ok(None) => {}
        Err(err) => {
            error!("Database error checking slug: {}", err);
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }

    let health_check_passed =
        match perform_health_check(&state.http_client, &req.endpoint_url).await {
            Ok(true) => true,
            Ok(false) => {
                info!("Health check failed for {}", req.endpoint_url);
                false
            }
            Err(err) => {
                error!("Health check error: {}", err);
                false
            }
        };

    let api_id = Uuid::new_v4();
    let price_bd = to_bigdecimal(req.price_usd);
    // Health-check-passes alone is no longer enough to route real traffic:
    // it only tells us the URL responds at registration time. Verification
    // (which gates routing in the proxy) requires an explicit operator or
    // domain-ownership step. Newly registered APIs always start unverified;
    // `status='active'` is allowed when health passes so the provider can
    // see a green light immediately, but the proxy will return 403 until
    // is_verified flips to TRUE.
    let verified = false;
    let status = registration_status(health_check_passed);
    let default_operations = default_root_operations(req.price_usd);
    let listing_type = Some("api");
    let normalized_listing_metadata = normalize_listing_metadata(AgentMetadataContext {
        slug: &req.slug,
        name: &req.name,
        description: &req.description,
        category: Some(&req.category),
        listing_type,
        origin_url: &req.endpoint_url,
        docs_url: None,
        raw_metadata: &req.listing_metadata,
        default_price_usd: Some(req.price_usd),
        operations: &default_operations,
    });

    match state
        .db
        .create_community_api(
            api_id,
            &req.slug,
            &req.name,
            &req.description,
            &req.endpoint_url,
            &authed_wallet,
            &price_bd,
            &req.category,
            &normalized_listing_metadata,
            verified,
        )
        .await
    {
        Ok(_) => Ok(Json(RegisterApiResponse {
            success: true,
            api_id: api_id.to_string(),
            slug: req.slug.clone(),
            gateway_url: format!("https://gateway.apiosk.com/{}", req.slug),
            status: status.to_string(),
            verified,
            health_check_passed,
            message: registration_message(verified),
        })),
        Err(err) => {
            error!("Failed to create API: {}", err);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}
