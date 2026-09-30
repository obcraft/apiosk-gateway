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
    if !recurring::enabled() {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "recurring_unavailable",
            "Recurring orders are not available yet.",
        );
    }
    let p = match person(&s, &h).await {
        Ok(p) => p,
        Err(r) => return r,
    };
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
}
pub(super) async fn approve(
    State(s): State<ServerState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<Approve>,
) -> Response {
    if !recurring::enabled() {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "recurring_unavailable",
            "Recurring orders are not available yet.",
        );
    }
    let p = match person(&s, &h).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    reply(
        async {
            recurring::read(&s.engine, &p, id).await?;
            s.engine.store.check_spending(&p).await?;
            let total = b.total.parse::<i64>()?;
            sqlx::query("select billing.approve_recurring_order($1,$2,$3)")
                .bind(p.owner)
                .bind(id)
                .bind(total)
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
    if b.state == "active" && !recurring::enabled() {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "recurring_unavailable",
            "Recurring orders are not available yet.",
        );
    }
    let p = match person(&s, &h).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    reply(
        async {
            recurring::read(&s.engine, &p, id).await?;
            s.engine.store.check_spending(&p).await?;
            sqlx::query("select billing.control_recurring_order($1,$2,$3)")
                .bind(p.owner)
                .bind(id)
                .bind(&b.state)
                .execute(&s.engine.store.pool)
                .await?;
            recurring::read(&s.engine, &p, id).await
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
