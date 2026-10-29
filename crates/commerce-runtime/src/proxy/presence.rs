use super::*;

#[derive(Debug, Deserialize)]
pub struct PresenceConnectRequest {
    #[serde(rename = "sessionId")]
    pub session_id: Uuid,
    #[serde(rename = "clientId")]
    pub client_id: String,
    #[serde(rename = "clientKind")]
    pub client_kind: String,
    #[serde(rename = "walletAddress")]
    pub wallet_address: Option<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct PresenceHeartbeatRequest {
    #[serde(rename = "sessionId")]
    pub session_id: Uuid,
    #[serde(rename = "apiSlug")]
    pub api_slug: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PresenceDisconnectRequest {
    #[serde(rename = "sessionId")]
    pub session_id: Uuid,
}

#[derive(Debug, Serialize)]
struct PresenceConnectResponse {
    #[serde(rename = "sessionId")]
    session_id: Uuid,
    #[serde(rename = "clientId")]
    client_id: String,
    #[serde(rename = "managedWalletId")]
    managed_wallet_id: Option<Uuid>,
    #[serde(rename = "heartbeatIntervalMs")]
    heartbeat_interval_ms: u64,
    #[serde(rename = "staleAfterMs")]
    stale_after_ms: u64,
}

pub async fn presence_connect(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<PresenceConnectRequest>,
) -> Response<Body> {
    let client_id = payload.client_id.trim();
    if client_id.is_empty() {
        return json_error_response(
            StatusCode::BAD_REQUEST,
            "validation_error",
            "clientId is required.",
        );
    }

    let client_kind = payload.client_kind.trim();
    if client_kind.is_empty() {
        return json_error_response(
            StatusCode::BAD_REQUEST,
            "validation_error",
            "clientKind is required.",
        );
    }

    let connect_token_ctx = match resolve_connect_token(&state, &headers).await {
        Ok(ctx) => ctx,
        Err(response) => return response,
    };
    let agent_wallet_session = connect_token_ctx.as_ref().and_then(|ctx| {
        ctx.sole_wallet().map(|wallet| AgentWalletSession {
            token_id: ctx.token_id(),
            wallet_id: wallet.wallet_id,
            wallet_address: wallet.wallet_address.clone(),
        })
    });

    let wallet_address = if let Some(ref session) = agent_wallet_session {
        Some(session.wallet_address.clone())
    } else {
        match payload.wallet_address.as_deref() {
            Some(wallet) => match normalize_wallet_like(wallet) {
                Some(normalized) => Some(normalized),
                None => {
                    return json_error_response(
                        StatusCode::BAD_REQUEST,
                        "validation_error",
                        "walletAddress must be a valid EVM address.",
                    )
                }
            },
            None => None,
        }
    };

    match state
        .db
        .upsert_presence_session(PresenceSessionUpsert {
            session_id: payload.session_id,
            client_id: client_id.to_string(),
            client_kind: client_kind.to_string(),
            managed_wallet_id: agent_wallet_session
                .as_ref()
                .map(|session| session.wallet_id),
            wallet_address,
            metadata: payload.metadata,
        })
        .await
    {
        Ok(session) => Json(PresenceConnectResponse {
            session_id: session.session_id,
            client_id: session.client_id,
            managed_wallet_id: session.managed_wallet_id,
            heartbeat_interval_ms: 30_000,
            stale_after_ms: 90_000,
        })
        .into_response(),
        Err(err) => {
            error!("Failed to connect presence session: {}", err);
            json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "presence_connect_failed",
                "Failed to register presence session.",
            )
        }
    }
}

pub async fn presence_heartbeat(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<PresenceHeartbeatRequest>,
) -> Response<Body> {
    match state
        .db
        .heartbeat_presence_session(payload.session_id, payload.api_slug.as_deref())
        .await
    {
        Ok(Some(session)) => Json(serde_json::json!({
            "sessionId": session.session_id,
            "status": "online",
        }))
        .into_response(),
        Ok(None) => json_error_response(
            StatusCode::NOT_FOUND,
            "presence_session_not_found",
            "Presence session not found.",
        ),
        Err(err) => {
            error!("Failed to heartbeat presence session: {}", err);
            json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "presence_heartbeat_failed",
                "Failed to update presence session heartbeat.",
            )
        }
    }
}

pub async fn presence_disconnect(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<PresenceDisconnectRequest>,
) -> Response<Body> {
    match state
        .db
        .disconnect_presence_session(payload.session_id)
        .await
    {
        Ok(true) => Json(serde_json::json!({
            "sessionId": payload.session_id,
            "status": "offline",
        }))
        .into_response(),
        Ok(false) => json_error_response(
            StatusCode::NOT_FOUND,
            "presence_session_not_found",
            "Presence session not found.",
        ),
        Err(err) => {
            error!("Failed to disconnect presence session: {}", err);
            json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "presence_disconnect_failed",
                "Failed to disconnect presence session.",
            )
        }
    }
}
