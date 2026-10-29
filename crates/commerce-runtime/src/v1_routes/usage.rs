use std::{collections::HashMap, sync::Arc};

use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};

use crate::AppState;

use super::models::{ApiUsageStats, BalanceQuery, BalanceResponse, UsageQuery, UsageResponse};

pub async fn check_balance(
    State(state): State<Arc<AppState>>,
    Query(params): Query<BalanceQuery>,
) -> Result<Json<BalanceResponse>, StatusCode> {
    let today = state
        .db
        .get_wallet_usage(&params.address, "today")
        .await
        .map_err(|err| {
            tracing::error!("Failed to get today usage: {}", err);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let week = state
        .db
        .get_wallet_usage(&params.address, "week")
        .await
        .map_err(|err| {
            tracing::error!("Failed to get week usage: {}", err);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let month = state
        .db
        .get_wallet_usage(&params.address, "month")
        .await
        .map_err(|err| {
            tracing::error!("Failed to get month usage: {}", err);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(BalanceResponse {
        address: params.address,
        balance_usdc: 0.0,
        spent_today_usdc: today.total_spent_usdc.to_string().parse().unwrap_or(0.0),
        spent_week_usdc: week.total_spent_usdc.to_string().parse().unwrap_or(0.0),
        spent_month_usdc: month.total_spent_usdc.to_string().parse().unwrap_or(0.0),
    }))
}

pub async fn usage_stats(
    State(state): State<Arc<AppState>>,
    Query(params): Query<UsageQuery>,
) -> Result<Json<UsageResponse>, StatusCode> {
    let usage = state
        .db
        .get_wallet_usage(&params.address, &params.period)
        .await
        .map_err(|err| {
            tracing::error!("Failed to get usage stats: {}", err);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let by_api: HashMap<String, ApiUsageStats> = usage
        .by_api
        .into_iter()
        .map(|api| {
            (
                api.api_id.clone(),
                ApiUsageStats {
                    requests: api.requests,
                    spent_usdc: api.spent_usdc.to_string().parse().unwrap_or(0.0),
                },
            )
        })
        .collect();

    Ok(Json(UsageResponse {
        total_requests: usage.total_requests,
        total_spent_usdc: usage.total_spent_usdc.to_string().parse().unwrap_or(0.0),
        period: params.period,
        by_api,
    }))
}
