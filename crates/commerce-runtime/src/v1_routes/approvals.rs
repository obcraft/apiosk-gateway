//! `GET /v1/approvals/:id` — the state of a held purchase.
//!
//! An agent that got a 402 with `status: "approval_required"` polls this until
//! the buyer answers. It is the only thing standing between "your agent stopped"
//! and "your agent is waiting for you", so it answers in words the agent can put
//! in front of a person, and it never makes the agent guess.
//!
//! Authenticated by the same connect token that created the hold, and no other.
//! An approval id is a handle, not a secret, and letting any connection read any
//! hold would leak what a buyer is buying to anyone who can guess a UUID.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use tracing::error;
use uuid::Uuid;

use crate::auth::extract_agent_connect_token;
use crate::db::AgentConnectTokenLookup;
use crate::proxy::hash_connect_token;
use crate::AppState;

pub async fn get_approval(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap<HeaderValue>,
) -> Response {
    let Some(raw_token) = extract_agent_connect_token(&headers) else {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "agent_wallet_token_missing",
            "Send the connect token via X-Apiosk-Connect-Token or Authorization: Bearer <token>.",
        );
    };

    let Ok(approval_id) = id.parse::<Uuid>() else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "approval_not_found",
            "That is not an approval id.",
        );
    };

    let token = match state
        .db
        .lookup_agent_connect_token_by_hash(&hash_connect_token(&raw_token))
        .await
    {
        Ok(AgentConnectTokenLookup::Active(token)) => token,
        Ok(_) => {
            return error_response(
                StatusCode::UNAUTHORIZED,
                "agent_wallet_token_invalid",
                "That connect token is invalid, expired, or revoked.",
            )
        }
        Err(err) => {
            error!("connect token lookup failed for /v1/approvals: {err}");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "agent_wallet_lookup_failed",
                "Could not validate the connect token.",
            );
        }
    };

    let approval = match state
        .db
        .get_purchase_approval(approval_id, Some(token.token_id))
        .await
    {
        Ok(Some(approval)) => approval,
        // "Not yours" and "does not exist" answer the same, deliberately. Telling
        // them apart would make this endpoint an oracle for which approval ids
        // are real.
        Ok(None) => {
            return error_response(
                StatusCode::NOT_FOUND,
                "approval_not_found",
                "No held purchase with that id on this connection.",
            )
        }
        Err(err) => {
            error!("could not read approval {approval_id}: {err}");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "approval_lookup_failed",
                "Could not read that held purchase.",
            );
        }
    };

    // A row the sweeper has not reached yet is still expired. Reporting it as
    // pending would have the agent poll a hold that can never be redeemed.
    let status = if approval.status == "pending" && approval.expires_at <= chrono::Utc::now() {
        "expired"
    } else {
        approval.status.as_str()
    };

    let next_step = match status {
        "approved" => "Call POST /v1/do again with this approval_id to complete the purchase.",
        "denied" => "The buyer declined. Do not retry — ask them what they would rather do.",
        "expired" => "Nobody answered in time. Get a fresh price and ask again.",
        "consumed" => "Already used. This approval authorised exactly one purchase.",
        _ => "Still waiting on the buyer. Poll again in a few seconds; do not retry the purchase.",
    };

    json_response(
        StatusCode::OK,
        json!({
            "approval_id": approval.id,
            "status": status,
            "amount_usdc": approval.amount_usd.to_string(),
            "seller": approval.seller,
            "api_slug": approval.api_slug,
            "destination_host": approval.destination_host,
            "matched_rule": approval.matched_rule,
            "reason": approval.reason,
            "requested_at": approval.created_at,
            "expires_at": approval.expires_at,
            "decided_at": approval.decided_at,
            "next_step": next_step,
        }),
    )
}

fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    let body: Value = json!({ "error": code, "message": message });
    (status, Json(body)).into_response()
}

fn json_response(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}
