use super::{authenticated, failure, ServerState};
use crate::recurring;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

async fn person(s: &ServerState, h: &HeaderMap) -> Result<crate::auth::Principal, Response> {
    let principal = direct_person(s, h).await?;
    if !recurring::owner_allowed(principal.owner) {
        return Err(failure(
            StatusCode::FORBIDDEN,
            "recurring_forbidden",
            "Recurring orders are not available for this account.",
        ));
    }
    Ok(principal)
}

async fn direct_person(s: &ServerState, h: &HeaderMap) -> Result<crate::auth::Principal, Response> {
    let c = authenticated(s, h)
        .await
        .map_err(|r| failure(r.status, r.code, r.message))?;
    if c.api_key.is_some() || c.principal.connection.is_some() {
        return Err(failure(
            StatusCode::FORBIDDEN,
            "app_only",
            "Manage recurring orders in the Apiosk app.",
        ));
    }
    Ok(c.principal)
}

/// Only this capability bit is public to an authenticated app session. The
/// server-side rollout list never leaves the gateway.
pub(super) async fn access(State(s): State<ServerState>, h: HeaderMap) -> Response {
    let p = match direct_person(&s, &h).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    Json(json!({"enabled": recurring::enabled_for(p.owner)})).into_response()
}

/// Stopping an owner's own mandate must still work after workspace membership
/// is removed. Authenticate the App session normally, but authorize this narrow
/// non-spending operation against the stored owner/workspace in `control`.
async fn owner_control(s: &ServerState, h: &HeaderMap) -> Result<crate::auth::Principal, Response> {
    let workspace = if let Some(value) = h.get("x-apiosk-workspace") {
        match value.to_str().ok().and_then(|value| value.parse::<Uuid>().ok()) {
            Some(id) => Some(id),
            None => return Err(failure(StatusCode::BAD_REQUEST, "invalid_workspace", "Invalid workspace.")),
        }
    } else { None };
    let mut authentication = h.clone();
    authentication.remove("x-apiosk-workspace");
    let mut principal = person(s, &authentication).await?;
    principal.workspace = workspace;
    Ok(principal)
}
fn recurring_error_message(error: &anyhow::Error) -> &'static str {
    match error.downcast_ref::<sqlx::Error>() {
        Some(sqlx::Error::Database(db)) if db.constraint() == Some("recurring_active_mandate") => return "This order is already scheduled. Open Recurring orders to manage it.",
        Some(sqlx::Error::Database(db)) if db.message() == "insufficient_balance" => return "Not enough available balance. Add funds or choose fewer runs.",
        Some(sqlx::Error::Database(db)) if db.message() == "quote_expired" => return "This quote expired. Refresh the total price.",
        Some(sqlx::Error::Database(db)) if db.message() == "recurring_delivery_busy" || db.code().as_deref() == Some("55P03") => return "A run or delivery is still in progress or needs review. Wait for it to finish before changing delivery.",
        _ => {}
    }
    match error.to_string().as_str() {
        "The order changed. Refresh it before saving delivery." => "The order changed. Refresh it before saving delivery.",
        "Delivery can only be changed on an active or paused order." => "Delivery can only be changed on an active or paused order.",
        "A dynamic input has no unique source in this recipe; review the original dependency."
        | "Several earlier sources could bind this input; an explicit source binding is required."
        | "The dependency returned no unique subject. Review the order." => "This saved answer needs a source choice before it can repeat automatically. Ask again with the exact company or record identifier.",
        "Extended analysis requires a fresh explicit approval and cannot be added to a recurring order." => "Extended analysis cannot repeat yet. Schedule the original source answer instead.",
        "A source price changed. Run and review the order again."
        | "The saved source contract changed."
        | "A source is no longer available." => "A source or its price changed. Prepare a new answer before scheduling it.",
        "Wait for the result to finish." | "Wait for all results to finish." => "Wait for the answer to finish, then try again.",
        "Verify your account email before scheduling delivery." => "Verify your Apiosk email before scheduling Gmail delivery.",
        _ => "Could not prepare this schedule. Check the delivery connection and try again.",
    }
}

fn reply(result: anyhow::Result<Value>) -> Response {
    match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => {
            tracing::warn!(error=%e,"recurring request refused");
            let message = recurring_error_message(&e);
            failure(StatusCode::CONFLICT, "recurring_refused", message)
        }
    }
}
pub(super) async fn quote(
    State(s): State<ServerState>,
    h: HeaderMap,
    Json(b): Json<recurring::QuoteRequest>,
) -> Response {
    let p = match person(&s, &h).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    if !recurring::enabled() {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "recurring_unavailable",
            "Recurring orders are not available yet.",
        );
    }
    reply(recurring::quote(&s.engine, &p, b).await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Destinations {
    connected_account_id: String,
    cursor: Option<String>,
}

pub(super) async fn destinations(
    State(s): State<ServerState>,
    h: HeaderMap,
    Json(b): Json<Destinations>,
) -> Response {
    let p = match person(&s, &h).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    if !recurring::enabled() {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "recurring_unavailable",
            "Recurring orders are not available yet.",
        );
    }
    if let Err(error) = s.engine.store.check_workspace(&p).await {
        return if matches!(
            error.downcast_ref::<crate::store::StoreAccessError>(),
            Some(crate::store::StoreAccessError::WorkspaceUnavailable)
        ) {
            failure(
                StatusCode::FORBIDDEN,
                "workspace_unavailable",
                "This workspace is unavailable.",
            )
        } else {
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "workspace_unavailable",
                "Could not check workspace access. Try again.",
            )
        };
    }
    let Some(composio) = crate::integrations::configured() else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "integrations_unavailable",
            "Delivery apps are temporarily unavailable. Try again.",
        );
    };
    match composio
        .slack_destinations(p.owner, &b.connected_account_id, b.cursor.as_deref())
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => {
            tracing::warn!(%error, "recurring destinations unavailable");
            failure(StatusCode::CONFLICT, "destinations_unavailable", "Could not load Slack conversations. Check the connection and its channel permissions, then retry.")
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateChannel {
    connected_account_id: String,
    name: String,
    is_private: bool,
}

pub(super) async fn create_channel(
    State(s): State<ServerState>, h: HeaderMap, Json(b): Json<CreateChannel>,
) -> Response {
    let p = match person(&s, &h).await { Ok(p) => p, Err(r) => return r };
    if !recurring::enabled() {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "recurring_unavailable", "Recurring orders are not available yet.");
    }
    if s.engine.store.check_workspace(&p).await.is_err() {
        return failure(StatusCode::FORBIDDEN, "workspace_unavailable", "This workspace is unavailable.");
    }
    let Some(composio) = crate::integrations::configured() else {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "integrations_unavailable", "Delivery apps are temporarily unavailable.");
    };
    match composio.create_slack_channel(p.owner, &b.connected_account_id, &b.name, b.is_private).await {
        Ok(value) => Json(value).into_response(),
        Err(message) => failure(StatusCode::CONFLICT, "channel_creation_failed", message),
    }
}

pub(super) async fn read(
    State(s): State<ServerState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let p = match person(&s, &h).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    reply(recurring::read(&s.engine, &p, id).await)
}

pub(super) async fn update_delivery(
    State(s): State<ServerState>, h: HeaderMap, Path(id): Path<Uuid>,
    Json(b): Json<recurring::delivery_edit::DeliveryEdit>,
) -> Response {
    let p = match person(&s, &h).await { Ok(p) => p, Err(r) => return r };
    if !recurring::enabled() {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "recurring_unavailable", "Recurring orders are not available yet.");
    }
    reply(recurring::delivery_edit::update(&s.engine, &p, id, b).await)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Approve {
    total: String,
    per_run_cap: String,
    workflow_hash: String,
}
pub(super) async fn approve(
    State(s): State<ServerState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<Approve>,
) -> Response {
    let p = match person(&s, &h).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    if !recurring::enabled() {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "recurring_unavailable",
            "Recurring orders are not available yet.",
        );
    }
    reply(
        async {
            recurring::verify(&s.engine, &p, id).await?;
            s.engine.store.check_spending(&p).await?;
            let total = b.total.parse::<i64>()?;
            let cap = b.per_run_cap.parse::<i64>()?;
            sqlx::query("select billing.approve_locked_recurring_order($1,$2,$3,$4,$5)")
                .bind(p.owner)
                .bind(id)
                .bind(total)
                .bind(cap)
                .bind(b.workflow_hash)
                .execute(&s.engine.store.pool)
                .await?;
            recurring::read(&s.engine, &p, id).await
        }
        .await,
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Control {
    state: String,
}
pub(super) async fn control(
    State(s): State<ServerState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<Control>,
) -> Response {
    let actor = if matches!(b.state.as_str(), "paused" | "cancelled") {
        owner_control(&s, &h).await
    } else {
        person(&s, &h).await
    };
    let p = match actor {
        Ok(p) => p,
        Err(r) => return r,
    };
    if b.state == "active" && !recurring::enabled() {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "recurring_unavailable",
            "Recurring orders are not available yet.",
        );
    }
    reply(
        async {
            if b.state == "active" {
                recurring::verify(&s.engine, &p, id).await?;
                s.engine.store.check_spending(&p).await?;
            } else {
                // Releasing the owner's future holds must remain possible
                // after workspace spending access has been revoked.
                let exists:bool=sqlx::query_scalar("select exists(select 1 from billing.recurring_orders where id=$1 and owner_id=$2 and workspace_id is not distinct from $3)")
                    .bind(id).bind(p.owner).bind(p.workspace).fetch_one(&s.engine.store.pool).await?;
                anyhow::ensure!(exists,"Order unavailable.");
            }
            sqlx::query("select billing.control_recurring_order($1,$2,$3)")
                .bind(p.owner)
                .bind(id)
                .bind(&b.state)
                .execute(&s.engine.store.pool)
                .await?;
            // A revoked workspace member cannot read its sources afterwards.
            match recurring::read(&s.engine, &p, id).await {
                Ok(order) => Ok(order),
                Err(error) if b.state == "active" => Err(error),
                Err(_) => Ok(json!({"id":id,"state":b.state,"access_revoked":true})),
            }
        }
        .await,
    )
}
pub(super) async fn list(State(s): State<ServerState>, h: HeaderMap) -> Response {
    let p = match person(&s, &h).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    reply(async{
 s.engine.store.check_workspace(&p).await?;
 let orders:Vec<Value>=sqlx::query_scalar("select jsonb_build_object('id',id,'question',question,'state',state,'reserved',reserved_micro_usd::text,'delivery',jsonb_build_object('toolkit',delivery->>'toolkit','destination',delivery->>'destination','destination_label',delivery->>'destination_label'),'schedule',configuration->'schedule','sources',coalesce((select jsonb_agg(jsonb_build_object('name',coalesce(source->'selection'->>'name',source->'selection'->>'api_slug','Saved source'),'api_slug',source->'selection'->>'api_slug') order by ordinal) from jsonb_array_elements(sources) with ordinality as saved(source,ordinal)),'[]'::jsonb)) from billing.recurring_orders where owner_id=$1 and workspace_id is not distinct from $2 and approved_at is not null order by created_at desc limit 100").bind(p.owner).bind(p.workspace).fetch_all(&s.engine.store.pool).await?;
 Ok(json!({"orders":orders}))
}.await)
}

pub(super) async fn run(
    State(s): State<ServerState>,
    h: HeaderMap,
    Path((id, run)): Path<(Uuid, Uuid)>,
) -> Response {
    let p = match person(&s, &h).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    reply(recurring::read_run(&s.engine, &p, id, run).await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ManualTrigger {
    run_id: Uuid,
}

pub(super) async fn manual_trigger(
    State(s): State<ServerState>, h: HeaderMap, Path(id): Path<Uuid>,
    Json(b): Json<ManualTrigger>,
) -> Response {
    if !recurring::manual::enabled() {
        return failure(StatusCode::NOT_FOUND, "manual_trigger_unavailable", "Manual trigger is only available on the local recurring-enabled Gateway.");
    }
    let p = match person(&s, &h).await { Ok(p) => p, Err(r) => return r };
    match recurring::manual::trigger(&s.engine, &p, id, b.run_id).await {
        Ok(()) => (StatusCode::ACCEPTED, Json(json!({"run_id":b.run_id,"state":"running"}))).into_response(),
        Err(error) => {
            tracing::warn!(%error, "manual recurring trigger refused");
            failure(StatusCode::CONFLICT, "manual_trigger_refused", "Could not start this occurrence. Check spending access and refresh the order; it must be active with its next run still queued.")
        }
    }
}
