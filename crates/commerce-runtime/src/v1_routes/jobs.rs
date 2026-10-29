//! The job API: start, watch, answer, stop.
//!
//! ```text
//!   POST /v1/jobs               plan_token + idempotency key -> 202 + job_id
//!   GET  /v1/jobs/:id           status now
//!   GET  /v1/jobs/:id/events    everything after a cursor
//!   POST /v1/jobs/:id/resolve   one selection, when the job asked
//!   POST /v1/jobs/:id/cancel    stop future dispatches
//! ```
//!
//! `POST` answers 202 and returns immediately. It does not wait for the work,
//! because the work outlives the request by design: a paid job must survive a
//! closed browser, and a route that only finishes when the job does would make
//! the browser's connection load-bearing again.
//!
//! Nothing here runs a node. The worker does that, from the rows this writes.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::db::jobs::{Created, JobRow};
use crate::plan::token::{self, TokenError};
use crate::service_auth::ServiceAuth;
use crate::AppState;

/// Attempts per node. One try plus the retries the plan token authorised.
const MAX_ATTEMPTS_PER_NODE: i32 = 3;

#[derive(Debug, Deserialize)]
pub struct StartJobRequest {
    /// The signed authorisation for exactly one plan version.
    pub plan_token: String,
    /// The caller's own key. Two clicks with the same key give one job.
    pub idempotency_key: String,
    #[serde(default)]
    pub user_id: Option<Uuid>,
    #[serde(default)]
    pub connection_id: Option<Uuid>,
    #[serde(default)]
    pub conversation_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct JobResponse {
    pub job_id: Uuid,
    pub status: String,
    pub plan_id: Uuid,
    pub plan_version_id: Uuid,
    pub plan_hash: String,
    /// What was approved, in atomic USDC. Repeated on every read so a caller
    /// never has to hold it themselves to know what the ceiling is.
    pub buyer_total_ceiling_atomic: i64,
    /// What the job has actually cost so far, in atomic USDC — the sum of the
    /// nodes that are settled. This is the number a receipt is written from
    /// and the number a held budget is closed against; the ceiling above is
    /// only what the buyer agreed to risk.
    pub settled_atomic: i64,
    /// Spent but unproven, in atomic USDC. Reported apart from `settled_atomic`
    /// on purpose: it is neither a charge nor a refund yet, and a caller that
    /// added it to either would be stating an outcome nobody knows.
    pub unresolved_atomic: i64,
    pub requests_made: i32,
    pub max_requests: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_question: Option<Value>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// `POST /v1/jobs`.
pub async fn start_job(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<StartJobRequest>,
) -> Response {
    if let Err(response) = require_service_key(&state, &headers).await {
        return response;
    }
    let (user_id, connection_id) = match owner_of(body.user_id, body.connection_id) {
        Ok(owner) => owner,
        Err(response) => return response,
    };
    if body.idempotency_key.trim().is_empty() || body.idempotency_key.len() > 200 {
        return refuse(
            StatusCode::BAD_REQUEST,
            "idempotency_key_required",
            "An idempotency key of 1 to 200 characters is required, so a repeated start is one job.",
        );
    }

    // Which version is being claimed? Signature, owner, fees and expiry are all
    // checked here; only the plan hash is not, because the hash it must equal
    // is the one the DATABASE holds for that version, and the version id to
    // look it up by is inside the token. Comparing the token's hash to itself
    // would check nothing at all.
    let claimed = match token::identify(
        &state.config.quote_signing_secret,
        &body.plan_token,
        user_id,
        connection_id,
    ) {
        Ok(claim) => claim,
        Err(error) => return refuse_token(error),
    };

    let stored = match state
        .db
        .plan_for_owner(claimed.plan_id, user_id, connection_id)
        .await
    {
        Ok(Some(stored)) => stored,
        Ok(None) => {
            return refuse(
                StatusCode::NOT_FOUND,
                "plan_not_found",
                "No plan with that id for this owner.",
            )
        }
        Err(error) => {
            tracing::error!("read plan for job: {error}");
            return refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "plan_unavailable",
                "The plan could not be read, so no job was started.",
            );
        }
    };

    // The check that matters, against what the database says this version is.
    let claim = match token::verify(
        &state.config.quote_signing_secret,
        &body.plan_token,
        user_id,
        connection_id,
        &stored.plan_hash,
    ) {
        Ok(claim) => claim,
        Err(error) => return refuse_token(error),
    };

    if claim.version_id != stored.version_id
        || claim.version != stored.version
        || stored.plan.hash() != stored.plan_hash
        || claim.buyer_total_atomic != stored.plan.quote.buyer_total_atomic
        || claim.max_retries_per_node > 2
        || claim.max_requests != stored.plan.nodes.len() as u32 * (claim.max_retries_per_node + 1)
    {
        return refuse(
            StatusCode::CONFLICT,
            "plan_changed",
            "Request a fresh plan before approval.",
        );
    }
    let vocabulary = match state.db.fact_vocabulary().await {
        Ok(v) => v,
        Err(_) => {
            return refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "catalogue_unavailable",
                "No job was started.",
            )
        }
    };
    if let Err(error) = crate::plan::validate::plan(&stored.plan, &vocabulary) {
        return refuse(StatusCode::CONFLICT, "plan_invalid", &error);
    }
    let current = match state.db.plan_candidates().await {
        Ok(c) => c,
        Err(_) => {
            return refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "catalogue_unavailable",
                "No job was started.",
            )
        }
    };
    if stored.plan.nodes.iter().any(|node| {
        !current.iter().any(|c| {
            c.active
                && c.contract_id == node.contract_id
                && c.endpoint_id == node.endpoint_id
                && node.contract.as_ref() == Some(&c.contract)
                && node.api_slug == c.api_slug
                && node.endpoint_path == c.endpoint_path
                && node.endpoint_method == c.endpoint_method
                && c.list_atomic > 0
                && c.list_atomic <= i64::MAX as i128 / 2
                && crate::plan::quote::node_buyer_total(c.list_atomic, c.class)
                    == node.buyer_total_atomic
        })
    }) {
        return refuse(
            StatusCode::CONFLICT,
            "plan_changed",
            "A source or price changed; request a fresh plan.",
        );
    }
    let required = required_nodes(&stored.plan);
    match state
        .db
        .create_job(
            &stored.plan,
            &claim,
            body.conversation_id,
            body.idempotency_key.trim(),
            &required,
            (claim.max_retries_per_node + 1).min(MAX_ATTEMPTS_PER_NODE as u32) as i32,
        )
        .await
    {
        // Both answers are 202 with the same job. A repeated start is not an
        // error to report; it is the same request arriving twice, and the
        // caller wanted one job.
        Ok((row, Created::Fresh)) => (StatusCode::ACCEPTED, Json(present(row))).into_response(),
        Ok((row, Created::Existing)) => (StatusCode::ACCEPTED, Json(present(row))).into_response(),
        Err(error) => {
            tracing::error!("create job: {error}");
            refuse(
                if error.to_string() == "insufficient_funds" {
                    StatusCode::PAYMENT_REQUIRED
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                },
                if error.to_string() == "insufficient_funds" {
                    "insufficient_funds"
                } else {
                    "job_not_started"
                },
                "The job could not be started. Nothing was reserved.",
            )
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct OwnerQuery {
    #[serde(default)]
    pub user_id: Option<Uuid>,
    #[serde(default)]
    pub connection_id: Option<Uuid>,
}

/// `GET /v1/jobs/:id`.
pub async fn get_job(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(job_id): Path<Uuid>,
    Query(params): Query<OwnerQuery>,
) -> Response {
    if let Err(response) = require_service_key(&state, &headers).await {
        return response;
    }
    let (user_id, connection_id) = match owner_of(params.user_id, params.connection_id) {
        Ok(owner) => owner,
        Err(response) => return response,
    };
    match state
        .db
        .job_for_owner(Some(job_id), user_id, connection_id, None)
        .await
    {
        Ok(Some(row)) => Json(present(row)).into_response(),
        Ok(None) => not_found(),
        Err(error) => {
            tracing::error!("read job: {error}");
            refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "job_unavailable",
                "The job could not be read.",
            )
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    #[serde(default)]
    pub after: i64,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub user_id: Option<Uuid>,
    #[serde(default)]
    pub connection_id: Option<Uuid>,
}

/// `GET /v1/jobs/:id/events?after=…`.
///
/// The resume path. A browser that refreshed asks for everything after the
/// cursor it last saw and catches up; it never has to have been connected.
pub async fn job_events(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(job_id): Path<Uuid>,
    Query(params): Query<EventsQuery>,
) -> Response {
    if let Err(response) = require_service_key(&state, &headers).await {
        return response;
    }
    let (user_id, connection_id) = match owner_of(params.user_id, params.connection_id) {
        Ok(owner) => owner,
        Err(response) => return response,
    };
    // Ownership is proven on the job before any event is read, so an event
    // stream cannot be a side channel into someone else's research.
    match state
        .db
        .job_for_owner(Some(job_id), user_id, connection_id, None)
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => return not_found(),
        Err(error) => {
            tracing::error!("read job for events: {error}");
            return refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "job_unavailable",
                "The job could not be read.",
            );
        }
    }

    match state
        .db
        .job_events_after(job_id, params.after.max(0), params.limit.unwrap_or(100))
        .await
    {
        Ok(events) => {
            let cursor = events
                .last()
                .and_then(|event| event.get("seq").and_then(Value::as_i64))
                .unwrap_or(params.after.max(0));
            Json(json!({ "events": events, "cursor": cursor })).into_response()
        }
        Err(error) => {
            tracing::error!("read events: {error}");
            refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "events_unavailable",
                "The events could not be read.",
            )
        }
    }
}

/// `GET /v1/jobs/:id/facts`.
///
/// What the job actually learned, with the provenance on every row. The App
/// composes its answer from this and cites it; a finding it cannot cite is one
/// it does not make.
///
/// Rejections come back too, marked. A reader seeing only the valid rows could
/// not tell "the source answered and had nothing" from "the source sent
/// something its type does not accept", and those are different sentences.
pub async fn job_facts(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(job_id): Path<Uuid>,
    Query(params): Query<OwnerQuery>,
) -> Response {
    if let Err(response) = require_service_key(&state, &headers).await {
        return response;
    }
    let (user_id, connection_id) = match owner_of(params.user_id, params.connection_id) {
        Ok(owner) => owner,
        Err(response) => return response,
    };

    // Ownership is proven on the job first, so a fact stream cannot become a
    // side channel into someone else's research.
    match state
        .db
        .job_for_owner(Some(job_id), user_id, connection_id, None)
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => return not_found(),
        Err(error) => {
            tracing::error!("read job for facts: {error}");
            return refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "job_unavailable",
                "The job could not be read.",
            );
        }
    }

    match state.db.job_facts(job_id, user_id, connection_id).await {
        Ok(facts) => Json(json!({ "facts": facts })).into_response(),
        Err(error) => {
            tracing::error!("read facts: {error}");
            refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "facts_unavailable",
                "The facts could not be read.",
            )
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ResolveRequest {
    pub node_key: String,
    /// The identity that was chosen, exactly as it was offered.
    pub chosen: Value,
    #[serde(default)]
    pub user_id: Option<Uuid>,
    #[serde(default)]
    pub connection_id: Option<Uuid>,
}

/// `POST /v1/jobs/:id/resolve`.
///
/// Costs nothing. The lookup that produced the candidates was already made and
/// charged; this reads one of them and lets the branch continue.
pub async fn resolve_job(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(job_id): Path<Uuid>,
    Json(body): Json<ResolveRequest>,
) -> Response {
    if let Err(response) = require_service_key(&state, &headers).await {
        return response;
    }
    let (user_id, connection_id) = match owner_of(body.user_id, body.connection_id) {
        Ok(owner) => owner,
        Err(response) => return response,
    };

    let row = match state
        .db
        .job_for_owner(Some(job_id), user_id, connection_id, None)
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return not_found(),
        Err(error) => {
            tracing::error!("read job for resolve: {error}");
            return refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "job_unavailable",
                "The job could not be read.",
            );
        }
    };

    let question: Option<crate::job::ask::Question> = row
        .pending_question
        .clone()
        .and_then(|value| serde_json::from_value(value).ok());

    match crate::job::ask::resolve(question.as_ref(), &body.node_key, &body.chosen) {
        Ok(_) => {}
        Err(error) => {
            return refuse(
                StatusCode::CONFLICT,
                match error {
                    crate::job::ask::ResolveError::NotAsking => "job_not_asking",
                    crate::job::ask::ResolveError::WrongQuestion => "wrong_question",
                    crate::job::ask::ResolveError::NotACandidate => "not_a_candidate",
                },
                &error.to_string(),
            )
        }
    }

    match state
        .db
        .record_answer(job_id, &body.node_key, &body.chosen)
        .await
    {
        Ok(true) => (StatusCode::ACCEPTED, Json(json!({ "resolved": true }))).into_response(),
        Ok(false) => refuse(
            StatusCode::CONFLICT,
            "job_not_asking",
            "This job is not waiting for an answer.",
        ),
        Err(error) => {
            tracing::error!("record answer: {error}");
            refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "answer_not_recorded",
                "The answer could not be recorded, so the job was not resumed.",
            )
        }
    }
}

/// `POST /v1/jobs/:id/cancel`.
///
/// A server action. It stops future dispatches; it does not unsend a request
/// that has already left, and the worker settles those. A browser closing its
/// stream is not this, and never changes a job's status.
pub async fn cancel_job(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(job_id): Path<Uuid>,
    Json(params): Json<OwnerQuery>,
) -> Response {
    if let Err(response) = require_service_key(&state, &headers).await {
        return response;
    }
    let (user_id, connection_id) = match owner_of(params.user_id, params.connection_id) {
        Ok(owner) => owner,
        Err(response) => return response,
    };
    match state.db.cancel_job(job_id, user_id, connection_id).await {
        Ok(true) => (
            StatusCode::ACCEPTED,
            Json(json!({
                "cancelled": true,
                "note": "No further calls will be dispatched. Calls already sent are settled.",
            })),
        )
            .into_response(),
        Ok(false) => not_found(),
        Err(error) => {
            tracing::error!("cancel job: {error}");
            refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "cancel_failed",
                "The job could not be cancelled.",
            )
        }
    }
}

/// Node keys whose success a required output depends on.
///
/// This is what makes `partial` mean something: a job succeeds only when every
/// one of these succeeded, and an optional branch failing does not downgrade
/// an otherwise complete answer.
pub fn required_nodes(plan: &crate::plan::Plan) -> Vec<String> {
    plan.coverage
        .iter()
        .filter(|entry| entry.required)
        .filter_map(|entry| entry.node_id.clone())
        .collect()
}

fn present(row: JobRow) -> JobResponse {
    JobResponse {
        job_id: row.id,
        status: row.status,
        plan_id: row.plan_id,
        plan_version_id: row.plan_version_id,
        plan_hash: row.plan_hash,
        buyer_total_ceiling_atomic: row.buyer_total_ceiling_atomic,
        settled_atomic: row.settled_atomic,
        unresolved_atomic: row.unresolved_atomic,
        requests_made: row.requests_made,
        max_requests: row.max_requests,
        pending_question: row.pending_question,
        created_at: row.created_at,
        finished_at: row.finished_at,
    }
}

async fn require_service_key(state: &Arc<AppState>, headers: &HeaderMap) -> Result<(), Response> {
    match crate::service_auth::resolve(state, headers).await {
        ServiceAuth::Resolved(_) => Ok(()),
        ServiceAuth::NotPresented => Err(refuse(
            StatusCode::UNAUTHORIZED,
            "service_key_required",
            "Jobs are reached through the Apiosk app, which presents a service key.",
        )),
        ServiceAuth::Rejected(code, message) => {
            Err(refuse(StatusCode::UNAUTHORIZED, code, message))
        }
    }
}

fn owner_of(
    user_id: Option<Uuid>,
    connection_id: Option<Uuid>,
) -> Result<(Option<Uuid>, Option<Uuid>), Response> {
    match (user_id, connection_id) {
        (Some(user), None) => Ok((Some(user), None)),
        (None, Some(connection)) => Ok((None, Some(connection))),
        _ => Err(refuse(
            StatusCode::BAD_REQUEST,
            "owner_required",
            "Exactly one of user_id or connection_id is required.",
        )),
    }
}

/// A job that exists but belongs to someone else answers exactly as one that
/// does not exist. Telling them apart would leak which job ids are real.
fn not_found() -> Response {
    refuse(
        StatusCode::NOT_FOUND,
        "job_not_found",
        "No job with that id for this owner.",
    )
}

fn refuse_token(error: TokenError) -> Response {
    let (status, code) = match error {
        TokenError::NotOurs => (StatusCode::UNAUTHORIZED, "plan_token_invalid"),
        TokenError::Expired => (StatusCode::CONFLICT, "approval_expired"),
        TokenError::WrongOwner => (StatusCode::FORBIDDEN, "approval_not_yours"),
        TokenError::PlanChanged => (StatusCode::CONFLICT, "plan_changed"),
        TokenError::FeesChanged => (StatusCode::CONFLICT, "fees_changed"),
    };
    (
        status,
        Json(json!({ "error": code, "message": error.to_string() })),
    )
        .into_response()
}

fn refuse(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({ "error": code, "message": message }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{Coverage, CoverageStatus};

    fn coverage(fact: &str, required: bool, node: Option<&str>) -> Coverage {
        Coverage {
            fact_type: fact.to_string(),
            subject_role: "subject".to_string(),
            required,
            node_id: node.map(str::to_string),
            status: if node.is_some() {
                CoverageStatus::Planned
            } else {
                CoverageStatus::AlreadyKnown
            },
            reason: None,
        }
    }

    #[test]
    fn required_nodes_are_the_ones_a_required_output_depends_on() {
        let plan = crate::plan::Plan {
            nodes: vec![],
            coverage: vec![
                coverage("company.profile", true, Some("n_profile")),
                coverage("company.financial_statements", true, Some("n_filing")),
                coverage("building.attributes", false, Some("n_building")),
                // Already known: no node, so nothing to prove.
                coverage("company.name", true, None),
            ],
            quote: crate::plan::quote::total(vec![], None),
            format: crate::plan::OutputFormat::Json,
            limits: crate::plan::compile::Limits::default(),
        };
        let required = required_nodes(&plan);
        assert_eq!(required, vec!["n_profile", "n_filing"]);
        // An optional branch failing must not downgrade an otherwise complete
        // answer, so its node is not in the list.
        assert!(!required.contains(&"n_building".to_string()));
    }

    #[test]
    fn exactly_one_owner_is_required() {
        assert!(owner_of(Some(Uuid::nil()), None).is_ok());
        assert!(owner_of(None, Some(Uuid::nil())).is_ok());
        assert!(owner_of(None, None).is_err());
        assert!(owner_of(Some(Uuid::nil()), Some(Uuid::nil())).is_err());
    }

    #[test]
    fn each_token_refusal_gets_a_status_a_caller_can_act_on() {
        // A forged token is 401. An expired or changed approval is 409: the
        // caller's credentials are fine, the thing they are approving is not,
        // and the fix is a fresh quote rather than a fresh login.
        assert_eq!(
            refuse_token(TokenError::NotOurs).status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            refuse_token(TokenError::Expired).status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            refuse_token(TokenError::PlanChanged).status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            refuse_token(TokenError::FeesChanged).status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            refuse_token(TokenError::WrongOwner).status(),
            StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn a_missing_job_and_someone_elses_job_answer_identically() {
        assert_eq!(not_found().status(), StatusCode::NOT_FOUND);
    }
}
