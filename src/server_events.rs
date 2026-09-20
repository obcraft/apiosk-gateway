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

fn changes() -> &'static tokio::sync::broadcast::Sender<Uuid> {
    static CHANGES: std::sync::OnceLock<tokio::sync::broadcast::Sender<Uuid>> = std::sync::OnceLock::new();
    CHANGES.get_or_init(|| tokio::sync::broadcast::channel(256).0)
}

/// A wakeup only: clients still receive an authorized read of committed state.
pub(crate) fn task_changed(task: Uuid) {
    let _ = changes().send(task);
}

async fn next_change(receiver: &mut tokio::sync::broadcast::Receiver<Uuid>, task: Uuid) {
    loop {
        match receiver.recv().await {
            Ok(changed) if changed != task => continue,
            _ => return,
        }
    }
}

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
    // Subscribe before the initial read so a concurrent save cannot be missed.
    let mut updates = changes().subscribe();
    let Ok((_, saved, _)) = state.engine.store.load(access.owner, id).await else { return StatusCode::NOT_FOUND.into_response() };
    let p = Principal {
        owner: access.owner,
        connection: access.connection, workspace: saved.workspace_id, };
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
                    Status::NeedsInput | Status::Succeeded | Status::Partial | Status::Cancelled | Status::Failed | Status::Unsupported
                ) && next.context_view["worker_active"] != true;
                let Ok(event) = Event::default().event("task").json_data(&next) else {
                    break;
                };
                if sender.send(Ok(event)).await.is_err() || terminal {
                    break;
                }
                previous = Some(key);
            }
            // Local saves wake immediately. Periodic reads still recover missed
            // notifications and changes made by another Gateway instance.
            tokio::select! {
                _ = sender.closed() => break,
                _ = next_change(&mut updates, id) => {},
                _ = tokio::time::sleep(Duration::from_millis(750)) => {},
            }
            if state.engine.store.check_workspace(&p).await.is_err() { break; }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wakeups_filter_other_tasks_and_recover_lag() {
        let (sender, mut receiver) = tokio::sync::broadcast::channel(2);
        let task = Uuid::new_v4();
        sender.send(Uuid::new_v4()).unwrap();
        sender.send(task).unwrap();
        tokio::time::timeout(Duration::from_millis(100), next_change(&mut receiver, task)).await.unwrap();
        assert!(receiver.try_recv().is_err(), "the matching wakeup was consumed");
        for _ in 0..3 { sender.send(Uuid::new_v4()).unwrap(); }
        tokio::time::timeout(Duration::from_millis(100), next_change(&mut receiver, task)).await.unwrap();
    }
}
