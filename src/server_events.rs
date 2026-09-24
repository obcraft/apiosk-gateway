use crate::{auth::Principal, protocol::Status, server::ServerState, telemetry::{self, Outcome, Phase}};
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
    #[serde(default)]
    compact: bool,
}

/// Shallow replacements keep null values and deletions distinct. The context
/// is compared separately so unchanged history/results are not sent again.
fn delta(previous: &serde_json::Value, next: &serde_json::Value) -> serde_json::Value {
    use serde_json::{json, Map, Value};
    let diff = |before: &Value, after: &Value, skip_context: bool| {
        let before = before.as_object().expect("snapshot object");
        let after = after.as_object().expect("snapshot object");
        let set: Map<String, Value> = after.iter()
            .filter(|(key, value)| !(skip_context && key.as_str() == "context_view") && before.get(*key) != Some(*value))
            .map(|(key, value)| (key.clone(), value.clone())).collect();
        let remove: Vec<_> = before.keys().filter(|key| !after.contains_key(*key)).collect();
        (set, remove.into_iter().cloned().collect::<Vec<_>>())
    };
    let (set, remove) = diff(previous, next, true);
    let (context_set, context_remove) = diff(&previous["context_view"], &next["context_view"], false);
    json!({"state_ref":previous["state"]["state_ref"],"base_revision":previous["state"]["revision"],
        "set":set,"remove":remove,"context_set":context_set,"context_remove":context_remove})
}

fn bound_cache(documents: &mut std::collections::BTreeMap<Uuid, serde_json::Value>) {
    let mut remaining = 2 * 1024 * 1024;
    documents.retain(|_, document| {
        let size = serde_json::to_vec(document).map_or(usize::MAX, |bytes| bytes.len());
        if size > remaining { return false; }
        remaining -= size;
        true
    });
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
    telemetry::set_surface(if access.connection.is_some() { telemetry::Surface::ConnectedAgent } else { telemetry::Surface::App });
    telemetry::bind(id, None);
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
    let mut documents = Default::default();
    let Ok(initial) = state.engine.view_with_results(&p, id, Uuid::new_v4(), None, &mut documents).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    bound_cache(&mut documents);
    let (sender, receiver) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(2);
    telemetry::spawn(async move {
        let _permit = permit;
        let stream = telemetry::span(Phase::SseStream).task(id);
        let (mut events, mut deltas, mut probes) = (0u64, 0u64, 0u64);
        let mut next = initial;
        let mut previous = None;
        let mut snapshot = None;
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
                    Status::NeedsInput | Status::NeedsSelection | Status::Succeeded | Status::Partial | Status::Cancelled | Status::Failed | Status::Unsupported
                ) && next.context_view["worker_active"] != true;
                let (kind, data) = if access.compact {
                    let Ok(full) = serde_json::to_value(&next) else { break; };
                    let update = snapshot.as_ref().map(|previous| delta(previous, &full));
                    let data = serde_json::to_string(update.as_ref().unwrap_or(&full));
                    snapshot = Some(full);
                    (if update.is_some() { "task_delta" } else { "task" }, data)
                } else {
                    ("task", serde_json::to_string(&next))
                };
                let Ok(data) = data else { break; };
                telemetry::sse(data.len());
                events += 1;
                deltas += u64::from(kind == "task_delta");
                let event = Event::default().event(kind).data(data);
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
            probes += 1;
            let version: Result<serde_json::Value,_> = telemetry::timed_db("sse_probe", sqlx::query_scalar("SELECT jsonb_build_array(t.revision,EXISTS(SELECT 1 FROM execution_leases l WHERE l.task_id=t.id AND l.expires_at>now()),EXISTS(SELECT 1 FROM authorizations a WHERE a.task_id=t.id AND a.owner_id=t.owner_id AND a.quote_ref::text=t.document#>>'{proposal,quote_ref}' AND a.revoked_at IS NULL AND a.expires_at>now())) FROM tasks t WHERE t.id=$1 AND t.owner_id=$2 AND t.expires_at>now()")
                .bind(id).bind(p.owner).fetch_one(&state.engine.store.pool)).await;
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
            match state.engine.view_with_results(&p, id, Uuid::new_v4(), None, &mut documents).await {
                Ok(value) => { next = value; bound_cache(&mut documents); },
                Err(_) => break,
            }
        }
        stream.measure("events", events).measure("delta_events", deltas).measure("probes", probes).finish(Outcome::Ok);
        telemetry::flush();
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
    #[test]
    fn compact_updates_preserve_nulls_removals_and_same_revision_worker_changes() {
        use serde_json::json;
        let before = json!({"state":{"state_ref":"task","revision":4},"status":"running", "retry_after_ms":750,
            "context_view":{"conversation":["large saved result".repeat(1000)],"worker_active":true,"cooldown":{"until":"later"},"analysis":null}});
        let after = json!({"state":{"state_ref":"task","revision":4},"status":"succeeded",
            "context_view":{"conversation":before["context_view"]["conversation"],"worker_active":false,"analysis":{"status":"completed"}}, "result":null});
        let patch = delta(&before, &after);
        assert!(patch["context_set"].get("conversation").is_none());
        assert_eq!(patch["context_remove"], json!(["cooldown"]));
        assert_eq!(patch["remove"], json!(["retry_after_ms"]));
        assert_eq!(patch["set"]["result"], serde_json::Value::Null);
        let mut rebuilt = before.clone();
        for (key, value) in patch["set"].as_object().unwrap() { rebuilt[key] = value.clone(); }
        for key in patch["remove"].as_array().unwrap() { rebuilt.as_object_mut().unwrap().remove(key.as_str().unwrap()); }
        for (key, value) in patch["context_set"].as_object().unwrap() { rebuilt["context_view"][key] = value.clone(); }
        for key in patch["context_remove"].as_array().unwrap() { rebuilt["context_view"].as_object_mut().unwrap().remove(key.as_str().unwrap()); }
        assert_eq!(rebuilt, after);
        assert!(patch.to_string().len() * 10 < after.to_string().len());
    }

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
