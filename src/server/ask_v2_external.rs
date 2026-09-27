//! App-only input review, quote, approval and read-only purchase recovery.
use super::{app_person, failure, token, ServerState};
use crate::ask_v2::{external, purchase};
use axum::{extract::{Path, State}, http::{HeaderMap, StatusCode}, response::{IntoResponse, Response}, Json};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceRequest {
    resource_url: String,
    method: String,
    query: String,
}
impl SourceRequest {
    fn valid(&self) -> bool {
        !self.resource_url.is_empty() && self.resource_url.len() <= 4096
            && matches!(self.method.as_str(), "GET" | "POST" | "PUT" | "PATCH" | "DELETE")
            && !self.query.trim().is_empty() && self.query.len() <= 500
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PrepareRequest {
    request_id: Uuid,
    resource_url: String,
    method: String,
    query: String,
    input: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Approval {
    quote_ref: Uuid,
    max_total_atomic: String,
}

async fn resolve(state: &ServerState, request: &SourceRequest) -> Result<external::Offer, Response> {
    if !request.valid() {
        return Err(failure(StatusCode::BAD_REQUEST, "invalid_source", "Choose the source again. Nothing was charged."));
    }
    let Some(directory) = state.executor_compat.as_ref() else {
        return Err(failure(StatusCode::SERVICE_UNAVAILABLE, "source_unavailable", "Source pricing is temporarily unavailable. Try again shortly."));
    };
    external::resolve(&state.engine.executor.http, directory.directory_origin(), &request.resource_url, &request.method, &request.query)
        .await.map_err(|error| failure(StatusCode::UNPROCESSABLE_ENTITY, "source_unavailable", &error.to_string()))
}

pub(super) async fn source(State(state): State<ServerState>, headers: HeaderMap, Json(request): Json<SourceRequest>) -> Response {
    let person = match app_person(&state, &headers).await { Ok(person) => person, Err(response) => return response };
    let Ok(_permit) = state.permits.try_acquire() else { return failure(StatusCode::TOO_MANY_REQUESTS, "busy", "Try again shortly."); };
    if state.engine.parser.ask_v2().is_some_and(|client| !client.admit_search(&person.owner.to_string())) {
        return failure(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "Wait a minute before checking another source.");
    }
    match resolve(&state, &request).await { Ok(offer) => Json(offer).into_response(), Err(response) => response }
}

pub(super) async fn prepare(State(state): State<ServerState>, headers: HeaderMap, Json(request): Json<PrepareRequest>) -> Response {
    let person = match app_person(&state, &headers).await { Ok(person) => person, Err(response) => return response };
    if request.request_id.is_nil() || !request.input.as_object().is_some_and(|input| input.len() <= 64) {
        return failure(StatusCode::BAD_REQUEST, "invalid_input", "Review the input fields. Nothing was charged.");
    }
    let Ok(_permit) = state.permits.try_acquire() else { return failure(StatusCode::TOO_MANY_REQUESTS, "busy", "Try again shortly."); };
    let hash = purchase::request_hash(&person, &request.resource_url, &request.method, &request.query, &request.input);
    match purchase::prepared(&state.engine, &person, request.request_id, &hash).await {
        Ok(Some(view)) => return Json(view).into_response(),
        Ok(None) => {},
        Err(error) => return purchase_failure(error),
    }
    if state.engine.parser.ask_v2().is_some_and(|client| !client.admit_search(&person.owner.to_string())) {
        return failure(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "Wait a minute before preparing another source.");
    }
    let source = SourceRequest { resource_url: request.resource_url, method: request.method, query: request.query };
    let offer = match resolve(&state, &source).await { Ok(offer) => offer, Err(response) => return response };
    let bound = match external::validate_input(&offer, &request.input) {
        Ok(bound) => bound,
        Err(error) => return failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_input", &error.to_string()),
    };
    match purchase::prepare(&state.engine, &person, request.request_id, hash, offer, bound).await {
        Ok(view) => Json(view).into_response(), Err(error) => purchase_failure(error),
    }
}

pub(super) async fn authorize(State(state): State<ServerState>, headers: HeaderMap, Path(id): Path<Uuid>, Json(request): Json<Approval>) -> Response {
    let person = match app_person(&state, &headers).await { Ok(person) => person, Err(response) => return response };
    if request.quote_ref.is_nil() || request.max_total_atomic.len() > 20 {
        return failure(StatusCode::BAD_REQUEST, "invalid_quote", "Review the current total before paying.");
    }
    let Ok(_permit) = state.permits.try_acquire() else { return failure(StatusCode::TOO_MANY_REQUESTS, "busy", "Check the saved purchase before trying again."); };
    match purchase::authorize(state.engine.clone(), person, token(&headers).unwrap().to_owned(), id, request.quote_ref, &request.max_total_atomic).await {
        Ok(view) => Json(view).into_response(), Err(error) => purchase_failure(error),
    }
}

pub(super) async fn status(State(state): State<ServerState>, headers: HeaderMap, Path(id): Path<Uuid>) -> Response {
    let person = match app_person(&state, &headers).await { Ok(person) => person, Err(response) => return response };
    match purchase::view(&state.engine, &person, id).await {
        Ok(view) => Json(view).into_response(), Err(error) => purchase_failure(error),
    }
}

fn purchase_failure(error: anyhow::Error) -> Response {
    if error.downcast_ref::<sqlx::Error>().is_some_and(crate::store::is_workspace_access_denied) {
        return failure(StatusCode::FORBIDDEN, "spending_not_permitted", "You cannot spend from this workspace.");
    }
    if error.downcast_ref::<crate::store::StoreAccessError>().is_some() || error.to_string() == "purchase_unavailable" {
        return failure(StatusCode::NOT_FOUND, "purchase_unavailable", "This purchase is not available in this workspace.");
    }
    match error.to_string().as_str() {
        "quote_expired" | "quote_changed" => failure(StatusCode::CONFLICT, "quote_changed", "The quote changed or expired. Review a new quote before paying."),
        "idempotency_conflict" => failure(StatusCode::CONFLICT, "idempotency_conflict", "This saved request has different inputs. Prepare a new quote."),
        "workspace spending is not permitted" => failure(StatusCode::FORBIDDEN, "spending_not_permitted", "You cannot spend from this workspace."),
        "purchase_cancelled" => failure(StatusCode::CONFLICT, "purchase_cancelled", "This purchase was cancelled."),
        _ => {
            tracing::warn!(error=%error, "Ask v2 purchase temporarily unavailable");
            failure(StatusCode::SERVICE_UNAVAILABLE, "purchase_unavailable", "The purchase status is temporarily unavailable. Your request is saved; check its status before trying to pay again.")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Debug, thiserror::Error)]
    #[error("{message}")]
    struct DatabaseFailure { code: &'static str, message: &'static str }
    impl sqlx::error::DatabaseError for DatabaseFailure {
        fn message(&self) -> &str { self.message }
        fn code(&self) -> Option<std::borrow::Cow<'_, str>> { Some(self.code.into()) }
        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) { self }
        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) { self }
        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> { self }
        fn kind(&self) -> sqlx::error::ErrorKind { sqlx::error::ErrorKind::Other }
    }
    #[test]
    fn permission_revocation_is_a_refusal_and_database_outage_remains_retryable() {
        let forbidden = sqlx::Error::Database(Box::new(DatabaseFailure { code:"42501", message:"forbidden" }));
        assert_eq!(purchase_failure(forbidden.into()).status(), StatusCode::FORBIDDEN);
        let unavailable = sqlx::Error::Database(Box::new(DatabaseFailure { code:"42501", message:"permission denied for table user_selected_apis" }));
        assert_eq!(purchase_failure(unavailable.into()).status(), StatusCode::SERVICE_UNAVAILABLE, "a missing runtime grant is operational, not a buyer permission refusal");
        assert_eq!(purchase_failure(sqlx::Error::PoolTimedOut.into()).status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
