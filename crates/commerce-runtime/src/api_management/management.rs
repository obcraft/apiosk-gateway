use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use tracing::{error, info};

use crate::{auth::verify_wallet_auth, AppState};

use super::{
    helpers::{to_bigdecimal, validate_price},
    DeleteApiQuery, DeleteApiResponse, MyApisQuery, MyApisResponse, UpdateApiRequest,
    UpdateApiResponse, UpdateEndpointDocsRequest, UpdateEndpointDocsResponse,
};

pub async fn update_api(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Json(req): Json<UpdateApiRequest>,
) -> Result<Json<UpdateApiResponse>, StatusCode> {
    info!("Updating API: slug={}, owner={}", slug, req.owner_wallet);

    let api = match state.db.get_api_by_slug(&slug).await {
        Ok(Some(api)) => api,
        Ok(None) => {
            return Ok(Json(UpdateApiResponse {
                success: false,
                message: format!("API '{}' not found", slug),
            }));
        }
        Err(err) => {
            error!("Database error: {}", err);
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    if let Some(ref owner) = api.owner_wallet {
        if owner.to_lowercase() != req.owner_wallet.to_lowercase() {
            return Ok(Json(UpdateApiResponse {
                success: false,
                message: "Unauthorized: wallet address does not match API owner".to_string(),
            }));
        }
        let action_resource = format!("update:{}", slug);
        if verify_wallet_auth(
            &headers,
            &state.auth_replay_cache,
            "update_api",
            &action_resource,
            Some(owner),
        )
        .await
        .is_err()
        {
            return Ok(Json(UpdateApiResponse {
                success: false,
                message: "Unauthorized: missing or invalid wallet signature".to_string(),
            }));
        }
    } else {
        return Ok(Json(UpdateApiResponse {
            success: false,
            message: "API is not a community-registered API".to_string(),
        }));
    }

    if let Some(ref url) = req.endpoint_url {
        if !url.starts_with("https://") {
            return Ok(Json(UpdateApiResponse {
                success: false,
                message: "Endpoint URL must use HTTPS".to_string(),
            }));
        }
    }

    if let Some(price) = req.price_usd {
        if !validate_price(price) {
            return Ok(Json(UpdateApiResponse {
                success: false,
                message: "Price must be between 0.0001 and 10.00 USD".to_string(),
            }));
        }
    }

    match state
        .db
        .update_community_api(
            api.id,
            req.endpoint_url.as_deref(),
            req.price_usd.map(to_bigdecimal).as_ref(),
            req.description.as_deref(),
            req.active,
            req.listing_metadata.as_ref(),
        )
        .await
    {
        Ok(_) => {
            state.cache.invalidate_all().await;
            Ok(Json(UpdateApiResponse {
                success: true,
                message: "API updated successfully".to_string(),
            }))
        }
        Err(err) => {
            error!("Failed to update API: {}", err);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

pub async fn my_apis(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<MyApisQuery>,
) -> Result<Json<MyApisResponse>, StatusCode> {
    info!("Fetching APIs for wallet: {}", params.wallet);

    let action_resource = format!("mine:{}", params.wallet);
    if verify_wallet_auth(
        &headers,
        &state.auth_replay_cache,
        "my_apis",
        &action_resource,
        Some(&params.wallet),
    )
    .await
    .is_err()
    {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let apis = match state.db.get_apis_by_wallet(&params.wallet).await {
        Ok(apis) => apis,
        Err(err) => {
            error!("Database error: {}", err);
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    let total_earnings = match state.db.get_total_earnings(&params.wallet).await {
        Ok(earnings) => earnings.to_string().parse().unwrap_or(0.0),
        Err(err) => {
            error!("Failed to get total earnings: {}", err);
            0.0
        }
    };

    Ok(Json(MyApisResponse {
        apis,
        total_earnings_usd: total_earnings,
    }))
}

pub async fn delete_api(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Query(params): Query<DeleteApiQuery>,
) -> Result<Json<DeleteApiResponse>, StatusCode> {
    info!("Deactivating API: slug={}, wallet={}", slug, params.wallet);

    let api = match state.db.get_api_by_slug(&slug).await {
        Ok(Some(api)) => api,
        Ok(None) => {
            return Ok(Json(DeleteApiResponse {
                success: false,
                message: format!("API '{}' not found", slug),
            }));
        }
        Err(err) => {
            error!("Database error: {}", err);
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    if let Some(ref owner) = api.owner_wallet {
        if owner.to_lowercase() != params.wallet.to_lowercase() {
            return Ok(Json(DeleteApiResponse {
                success: false,
                message: "Unauthorized: wallet address does not match API owner".to_string(),
            }));
        }
        let action_resource = format!("delete:{}", slug);
        if verify_wallet_auth(
            &headers,
            &state.auth_replay_cache,
            "delete_api",
            &action_resource,
            Some(owner),
        )
        .await
        .is_err()
        {
            return Ok(Json(DeleteApiResponse {
                success: false,
                message: "Unauthorized: missing or invalid wallet signature".to_string(),
            }));
        }
    } else {
        return Ok(Json(DeleteApiResponse {
            success: false,
            message: "API is not a community-registered API".to_string(),
        }));
    }

    match state.db.deactivate_api(api.id).await {
        Ok(_) => {
            state.cache.invalidate_all().await;
            Ok(Json(DeleteApiResponse {
                success: true,
                message: format!("API '{}' deactivated successfully", slug),
            }))
        }
        Err(err) => {
            error!("Failed to deactivate API: {}", err);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

pub async fn update_endpoint_docs(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Json(req): Json<UpdateEndpointDocsRequest>,
) -> Result<Json<UpdateEndpointDocsResponse>, StatusCode> {
    info!(
        "Updating endpoint docs for {}: {} {}",
        slug, req.method, req.path
    );

    let api = match state.db.get_api_by_slug(&slug).await {
        Ok(Some(api)) => api,
        Ok(None) => {
            return Ok(Json(UpdateEndpointDocsResponse {
                success: false,
                message: "API not found".to_string(),
            }));
        }
        Err(err) => {
            error!("Database error: {}", err);
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    let owner = api.owner_wallet.as_deref().unwrap_or(&api.wallet_address);
    if owner.to_lowercase() != req.owner_wallet.to_lowercase() {
        return Ok(Json(UpdateEndpointDocsResponse {
            success: false,
            message: "Not authorized to update this API".to_string(),
        }));
    }

    let action_resource = format!("docs:{}:{}:{}", slug, req.method, req.path);
    if verify_wallet_auth(
        &headers,
        &state.auth_replay_cache,
        "update_docs",
        &action_resource,
        Some(owner),
    )
    .await
    .is_err()
    {
        return Ok(Json(UpdateEndpointDocsResponse {
            success: false,
            message: "Unauthorized: missing or invalid wallet signature".to_string(),
        }));
    }

    let endpoint = match state.db.get_endpoint(api.id, &req.method, &req.path).await {
        Ok(Some(endpoint)) => endpoint,
        Ok(None) => {
            return Ok(Json(UpdateEndpointDocsResponse {
                success: false,
                message: format!("Endpoint {} {} not found", req.method, req.path),
            }));
        }
        Err(err) => {
            error!("Failed to get endpoint: {}", err);
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    match state
        .db
        .update_endpoint_docs(
            endpoint.id,
            req.description.as_deref(),
            req.request_body.as_ref(),
            req.response_body.as_ref(),
            req.request_example.as_ref(),
            req.response_example.as_ref(),
        )
        .await
    {
        Ok(_) => Ok(Json(UpdateEndpointDocsResponse {
            success: true,
            message: "Endpoint documentation updated".to_string(),
        })),
        Err(err) => {
            error!("Failed to update endpoint docs: {}", err);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}
