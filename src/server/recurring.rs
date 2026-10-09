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
    let workspace = h
        .get("x-apiosk-workspace")
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(|value| value.parse::<Uuid>().ok())
                .ok_or_else(|| {
                    failure(
                        StatusCode::BAD_REQUEST,
                        "invalid_workspace",
                        "Invalid workspace.",
                    )
                })
        })
        .transpose()?;
    let mut authentication = h.clone();
    authentication.remove("x-apiosk-workspace");
    let mut principal = person(s, &authentication).await?;
    principal.workspace = workspace;
    Ok(principal)
}
fn reply(result: anyhow::Result<Value>) -> Response {
    match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => {
            tracing::warn!(error=%e,"recurring request refused");
            let message = match e.downcast_ref::<sqlx::Error>() {
                Some(sqlx::Error::Database(db)) if db.constraint()==Some("recurring_active_mandate") => "This order is already scheduled. Open Recurring orders to manage it.",
                Some(sqlx::Error::Database(db)) if db.message()=="insufficient_balance" => "Your available balance is too low for this reservation. Add funds or shorten the period.",
                Some(sqlx::Error::Database(db)) if db.message()=="quote_expired" => "This quote expired. Refresh the total price before approving.",
                _ => "The recurring order could not be saved. Check the schedule, connected delivery app, source prices and available balance.",
            };
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
 let orders:Vec<Value>=sqlx::query_scalar("select jsonb_build_object('id',id,'question',question,'state',state,'reserved',reserved_micro_usd::text) from billing.recurring_orders where owner_id=$1 and workspace_id is not distinct from $2 and approved_at is not null order by created_at desc limit 100").bind(p.owner).bind(p.workspace).fetch_all(&s.engine.store.pool).await?;
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
