//! Authenticated Ask runs; all mutations are scoped to a person and workspace.
use super::{app_person, failure, token, ServerState};
use crate::ask_v2::{run::Run, run_contract::*};
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use uuid::Uuid;
// Verified immutable identity; neither email metadata nor browser flags grant access.
const ASK_RUN_OWNER: Uuid = Uuid::from_u128(0xbf085fe841c44699978fe75e89518bf7);
pub(super) async fn private_person(state: &ServerState, headers: &HeaderMap) -> Result<crate::auth::Principal, Response> {
    let person = app_person(state, headers).await?;
    if person.owner != ASK_RUN_OWNER {
        return Err(failure(StatusCode::NOT_FOUND, "not_found", "Not found."));
    }
    Ok(person)
}
fn error(e: anyhow::Error) -> Response {
    let code = e.to_string();
    let (status, message) = match code.as_str() {
        "run_unavailable" => (
            StatusCode::NOT_FOUND,
            "This request is unavailable in this workspace.",
        ),
        "invalid_question" => (
            StatusCode::BAD_REQUEST,
            "Enter a question of up to 16,000 characters.",
        ),
        "quote_expired" => (
            StatusCode::CONFLICT,
            "This quote expired. Submit the question again to get current prices.",
        ),
        "quote_mismatch"
        | "idempotency_conflict"
        | "run_not_awaiting_approval"
        | "invalid_clarification"
        | "clarification_stale"
        | "replan_blocked" => (
            StatusCode::CONFLICT,
            "Refresh this request before continuing.",
        ),
        _ => {
            tracing::warn!(error=%e,"Ask run unavailable");
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "run_unavailable",
                "Could not continue this request. Your saved progress is kept; try again.",
            );
        }
    };
    failure(status, &code, message)
}
async fn scoped(state: &ServerState, headers: &HeaderMap, id: Uuid) -> Result<Run, Response> {
    let person = private_person(state, headers).await?;
    Ok(Run {
        engine: state.engine.clone(),
        person,
        id,
    })
}
pub(super) async fn start(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<StartRunRequest>,
) -> Response {
    let person = match private_person(&state, &headers).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let result = async {
        let run = Run::start(state.engine.clone(), person, request).await?;
        run.resume(token(&headers).unwrap_or_default().into())
            .await?;
        run.snapshot().await
    }
    .await;
    match result {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(e) => error(e),
    }
}
pub(super) async fn get(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let run = match scoped(&state, &headers, id).await {
        Ok(r) => r,
        Err(r) => return r,
    };
    let result = async {
        run.resume(token(&headers).unwrap_or_default().into())
            .await?;
        run.snapshot().await
    }
    .await;
    match result {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(e) => error(e),
    }
}
pub(super) async fn approve(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<ApproveRunRequest>,
) -> Response {
    let run = match scoped(&state, &headers, id).await {
        Ok(r) => r,
        Err(r) => return r,
    };
    let result = async {
        run.approve(request, token(&headers).unwrap_or_default().into())
            .await?;
        run.snapshot().await
    }
    .await;
    match result {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(e) => error(e),
    }
}
pub(super) async fn cancel(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let run = match scoped(&state, &headers, id).await {
        Ok(r) => r,
        Err(r) => return r,
    };
    let result = async {
        run.cancel().await?;
        run.snapshot().await
    }
    .await;
    match result {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(e) => error(e),
    }
}
#[derive(Deserialize)]
pub(super) struct Cursor {
    #[serde(default)]
    after: u64,
}
pub(super) async fn events(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(cursor): Query<Cursor>,
) -> Response {
    let run = match scoped(&state, &headers, id).await {
        Ok(r) => r,
        Err(r) => return r,
    };
    match run.events(cursor.after).await {
        Ok(page) => Json(page).into_response(),
        Err(e) => error(e),
    }
}

pub(super) async fn clarify(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<ClarifyRunRequest>,
) -> Response {
    let run = match scoped(&state, &headers, id).await {
        Ok(r) => r,
        Err(r) => return r,
    };
    let result = async {
        run.clarify(request, token(&headers).unwrap_or_default().into())
            .await?;
        run.snapshot().await
    }
    .await;
    match result {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(e) => error(e),
    }
}
pub(super) async fn replan(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<ReplanRunRequest>,
) -> Response {
    let run = match scoped(&state, &headers, id).await {
        Ok(r) => r,
        Err(r) => return r,
    };
    let result = async {
        run.replan(request, token(&headers).unwrap_or_default().into())
            .await?;
        run.snapshot().await
    }
    .await;
    match result {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(e) => error(e),
    }
}
