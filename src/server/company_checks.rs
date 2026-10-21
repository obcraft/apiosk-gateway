use super::{caller, failure, token, ServerState};
use crate::{
    company_checks::{self, store},
    protocol::DiscoverRequest,
};
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Start {
    check_id: Uuid,
    occurrence: String,
    companies: Option<Vec<company_checks::Company>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Occurrence {
    occurrence: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Acknowledge {
    delivery_id: Uuid,
    provider: String,
    provider_message_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Email {
    occurrence: String,
    delivery_id: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReadQuery {
    occurrence: Option<String>,
    result_ref: Option<Uuid>,
}

fn valid_occurrence(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':' | b'.'))
}
fn error(error: anyhow::Error) -> Response {
    if error
        .downcast_ref::<crate::store::StoreAccessError>()
        .is_some()
    {
        return failure(
            StatusCode::NOT_FOUND,
            "company_check_unavailable",
            "This check is not available for this account and connection.",
        );
    }
    if let Some(database) = error.downcast_ref::<sqlx::Error>() {
        tracing::warn!(error=%error,"company check storage unavailable");
        if database.as_database_error().is_some_and(|e| e.code().as_deref() == Some("42P01") && e.message().contains("company_check")) {
            return failure(StatusCode::SERVICE_UNAVAILABLE,"company_check_storage_not_ready","Company-monitor storage is not installed in this Gateway database. Apply the company_check_snapshots migration and restart Gateway before testing. Recover the same identifiers; do not start another purchase.");
        }
        return failure(StatusCode::SERVICE_UNAVAILABLE,"company_check_unavailable","The check could not be saved. Retry with the same check_id and occurrence; do not start another purchase.");
    }
    failure(StatusCode::CONFLICT,"company_check_conflict", "The check could not continue. Recover the current occurrence before starting another; company lists are immutable.")
}
fn validation(message: &'static str) -> Response {
    failure(StatusCode::BAD_REQUEST, "invalid_company_check", message)
}

pub(super) async fn start(
    State(s): State<ServerState>,
    h: HeaderMap,
    Json(b): Json<Start>,
) -> Response {
    if !valid_occurrence(&b.occurrence) {
        return validation("Use a stable occurrence key (letters, digits, dash, underscore, colon or dot; at most 128 characters).");
    }
    if let Some(ref companies) = b.companies {
        if company_checks::validate_companies(companies).is_err() {
            return validation(
                "Supply one to three unique companies with names and eight-digit KVK strings.",
            );
        }
    }
    let Ok(_permit) = s.permits.try_acquire() else {
        return failure(
            StatusCode::TOO_MANY_REQUESTS,
            "busy",
            "Try again with the same occurrence.",
        );
    };
    let p = match caller(&s, &h).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let (companies, run) =
        match store::begin(&s.engine, &p, b.check_id, &b.occurrence, b.companies).await {
            Ok(v) => v,
            Err(e) => return error(e),
        };
    if let Some(snapshot) = run.snapshot {
        return match store::present(&s.engine.store.pool, b.check_id, snapshot).await {
            Ok(v) => Json(v).into_response(),
            Err(e) => error(e),
        };
    }
    let result = if let Some(task) = run.task_id {
        // Retry/recovery is a read. It never re-approves or replans a task.
        s.engine.view(&p, task, run.request_id, None).await
    } else {
        s.engine
            .discover(
                &p,
                DiscoverRequest {
                    request_id: run.request_id,
                    question: company_checks::question(&companies),
                    state: None,
                    context_delta: Default::default(),
                },
            )
            .await
    };
    let mut response = match result {
        Ok(r) => r,
        Err(e) => return error(e),
    };
    if response.state.is_none() && company_checks::can_snapshot(&response) {
        return match store::finish_without_task(
            &s.engine.store.pool,
            b.check_id,
            &b.occurrence,
            &companies,
            &response,
        )
        .await
        {
            Ok(saved) => match store::present(&s.engine.store.pool, b.check_id, saved).await {
                Ok(v) => Json(v).into_response(),
                Err(e) => error(e),
            },
            Err(e) => error(e),
        };
    }
    if let Some(state) = &response.state {
        if let Err(e) = store::link(
            &s.engine.store.pool,
            b.check_id,
            &b.occurrence,
            state.state_ref,
        )
        .await
        {
            return error(e);
        }
        if run.task_id.is_none() {
            // Existing policy and exact ceiling remain authoritative. This
            // route creates neither an approval nor a recurring mandate.
            response = s
                .engine
                .continue_automatically(&p, token(&h).unwrap_or_default(), response)
                .await;
        }
    }
    response.context_view["company_check"] = json!({"check_id":b.check_id,"occurrence":b.occurrence,"company_count":companies.len(),"schedule_created":false,"collect_tool":"apiosk_company_check_result"});
    Json(response).into_response()
}

pub(super) async fn collect(
    State(s): State<ServerState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<Occurrence>,
) -> Response {
    if !valid_occurrence(&b.occurrence) {
        return validation("Invalid occurrence key.");
    }
    let Ok(_permit) = s.permits.try_acquire() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "busy", "Try again shortly.");
    };
    let p = match caller(&s, &h).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let result = async {
        s.engine.store.check_workspace(&p).await?;
        let companies = store::companies(&s.engine.store.pool,&p,id).await?;
        let run = store::load(&s.engine.store.pool,id,&b.occurrence).await?;
        if let Some(snapshot) = run.snapshot { return store::present(&s.engine.store.pool,id,snapshot).await; }
        let Some(task_id) = run.task_id else {
            return Ok(json!({"view":"company_check_pending","check_id":id,"occurrence":b.occurrence,"status":"planning","message":"Retry apiosk_company_check with these same identifiers to recover the saved planning request."}));
        };
        let response = s.engine.view(&p,task_id,Uuid::new_v4(),None).await?;
        if !company_checks::can_snapshot(&response) {
            let mut response = response;
            response.context_view["company_check"] = json!({"check_id":id,"occurrence":b.occurrence});
            return Ok(serde_json::to_value(response)?);
        }
        let (revision,task,_) = s.engine.store.load_scoped(&p,task_id).await?;
        anyhow::ensure!(response.state.as_ref().is_some_and(|s|s.revision==revision),"Task changed during snapshot read.");
        let snapshot=store::collect(&s.engine,&p,id,&b.occurrence,&companies,&task,&response).await?;
        store::present(&s.engine.store.pool,id,snapshot).await
    }.await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(e) => error(e),
    }
}

pub(super) async fn read(
    State(s): State<ServerState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(query): Query<ReadQuery>,
) -> Response {
    if query.occurrence.is_some() != query.result_ref.is_some()
        || query
            .occurrence
            .as_deref()
            .is_some_and(|o| !valid_occurrence(o))
    {
        return validation("To read saved evidence, supply both occurrence and result_ref.");
    }
    let p = match caller(&s, &h).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let result: anyhow::Result<Value> = async {
        s.engine.store.check_workspace(&p).await?;
        let companies = store::companies(&s.engine.store.pool,&p,id).await?;
        if let (Some(occurrence),Some(result_ref))=(&query.occurrence,query.result_ref) {
            let saved=store::load(&s.engine.store.pool,id,occurrence).await?.snapshot.ok_or(crate::store::StoreAccessError::TaskUnavailable)?;
            let source=saved["sources"].as_array().into_iter().flatten().find(|s|s["result_ref"]==result_ref.to_string()).cloned().ok_or(crate::store::StoreAccessError::TaskUnavailable)?;
            return Ok(json!({"view":"company_check_evidence","check_id":id,"occurrence":occurrence,"source":source}));
        }
        let runs: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('occurrence',occurrence,'task_ref',task_id,'created_at',created_at,'completed_at',completed_at) FROM gateway_v2.company_check_runs WHERE check_id=$1 ORDER BY created_at DESC LIMIT 30")
            .bind(id).fetch_all(&s.engine.store.pool).await?;
        let latest: Option<Value> = sqlx::query_scalar("SELECT snapshot FROM gateway_v2.company_check_runs WHERE check_id=$1 AND completed_at IS NOT NULL ORDER BY completed_at DESC LIMIT 1")
            .bind(id).fetch_optional(&s.engine.store.pool).await?;
        let latest=match latest {Some(v)=>Some(store::present(&s.engine.store.pool,id,v).await?),None=>None};
        Ok(json!({"view":"company_check_history","check_id":id,"companies":companies,"runs":runs,"latest":latest,"schedule_created":false}))
    }.await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(e) => error(e),
    }
}

pub(super) async fn acknowledge(
    State(s): State<ServerState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<Acknowledge>,
) -> Response {
    let p = match caller(&s, &h).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let result = async {
        s.engine.store.check_workspace(&p).await?;
        store::companies(&s.engine.store.pool, &p, id).await?;
        company_checks::delivery::confirm(
            &s.engine.store.pool,
            id,
            b.delivery_id,
            &b.provider,
            &b.provider_message_id,
        )
        .await
    }
    .await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(e) => error(e),
    }
}

/// Prepare an immutable email handoff; the host connector owns the actual send.
pub(super) async fn email(
    State(s): State<ServerState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<Email>,
) -> Response {
    if !valid_occurrence(&b.occurrence) {
        return validation("Invalid occurrence key.");
    }
    let p = match caller(&s, &h).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let result = async {
        s.engine.store.check_workspace(&p).await?;
        store::companies(&s.engine.store.pool, &p, id).await?;
        company_checks::delivery::prepare(&s.engine.store.pool, id, &b.occurrence, b.delivery_id)
            .await
    }
    .await;
    match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => error(e),
    }
}
