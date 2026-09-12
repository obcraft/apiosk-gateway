use crate::{auth::Principal, protocol::Status, server::ServerState};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive},
        IntoResponse, Response, Sse,
    },
};
use serde::Deserialize;
use std::{convert::Infallible, time::Duration};
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;
static STREAMS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::OnceLock::new();

#[derive(Deserialize)]
pub struct EventAccess {
    owner: Uuid,
    connection: Option<Uuid>,
    expires: i64,
    signature: String,
}

/// A short-lived, task-scoped read capability. It cannot approve or execute.
pub async fn task_events(
    State(state): State<ServerState>,
    Path(id): Path<Uuid>,
    Query(access): Query<EventAccess>,
) -> Response {
    if state
        .engine
        .signer
        .verify_events(
            access.owner,
            id,
            access.connection,
            access.expires,
            &access.signature,
        )
        .is_err()
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok(permit) = STREAMS
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(128)))
        .clone()
        .try_acquire_owned()
    else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let p = Principal {
        owner: access.owner,
        connection: access.connection,
    };
    let Ok(initial) = state.engine.view(&p, id, Uuid::new_v4(), None).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let (sender, receiver) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(2);
    tokio::spawn(async move {
        let _permit = permit;
        let mut next = initial;
        let mut previous = None;
        while chrono::Utc::now().timestamp() < access.expires {
            let key = (
                next.state.as_ref().map(|s| s.revision),
                next.status.clone(),
                next.context_view["worker_active"].clone(),
                next.billing
                    .as_ref()
                    .map(|b| b["authorization_active"].clone()),
            );
            if previous.as_ref() != Some(&key) {
                let terminal = matches!(
                    next.status,
                    Status::Succeeded | Status::Partial | Status::Failed | Status::Unsupported
                ) && next.context_view["worker_active"] != true;
                let Ok(event) = Event::default().event("task").json_data(&next) else {
                    break;
                };
                if sender.send(Ok(event)).await.is_err() || terminal {
                    break;
                }
                previous = Some(key);
            }
            tokio::select! { _ = sender.closed() => break, _ = tokio::time::sleep(Duration::from_millis(750)) => {} }
            let version: Result<serde_json::Value,_> = sqlx::query_scalar("SELECT jsonb_build_array(t.revision,EXISTS(SELECT 1 FROM execution_leases l WHERE l.task_id=t.id AND l.expires_at>now()),EXISTS(SELECT 1 FROM authorizations a WHERE a.task_id=t.id AND a.owner_id=t.owner_id AND a.quote_ref::text=t.document#>>'{proposal,quote_ref}' AND a.revoked_at IS NULL AND a.expires_at>now())) FROM tasks t WHERE t.id=$1 AND t.owner_id=$2 AND t.expires_at>now()")
                .bind(id).bind(p.owner).fetch_one(&state.engine.store.pool).await;
            let current = serde_json::json!([
                next.state.as_ref().map(|s| s.revision),
                next.context_view["worker_active"],
                next.billing
                    .as_ref()
                    .map(|b| b["authorization_active"].clone())
            ]);
            match version {
                Ok(version) if version == current => continue,
                Err(_) => break,
                _ => {}
            }
            match state.engine.view(&p, id, Uuid::new_v4(), None).await {
                Ok(value) => next = value,
                Err(_) => break,
            }
        }
    });
    let mut response = Sse::new(ReceiverStream::new(receiver))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response();
    response
        .headers_mut()
        .insert("access-control-allow-origin", "*".parse().unwrap());
    response
        .headers_mut()
        .insert("cache-control", "private, no-store".parse().unwrap());
    response
        .headers_mut()
        .insert("referrer-policy", "no-referrer".parse().unwrap());
    response
}
