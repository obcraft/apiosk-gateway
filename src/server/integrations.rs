//! `/v2/integrations`: connect a person's business apps through Composio.
//! The App's Integrations page and a connected agent call the same routes; see
//! `crate::integrations` for the contract.
use super::{authenticated, failure, ServerState};
use crate::integrations::{self, ComposioError};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use uuid::Uuid;

/// The person the integration belongs to. An organisation key has no person
/// behind it to give consent, so it is refused rather than mapped to one.
async fn person(state: &ServerState, headers: &HeaderMap) -> Result<Uuid, Response> {
    let ctx = authenticated(state, headers).await.map_err(|r| failure(r.status, r.code, r.message))?;
    if ctx.api_key.is_some() {
        return Err(failure(StatusCode::FORBIDDEN, "integrations_person_only", "Integrations belong to a person. Connect them in the Apiosk app or through a connected agent, not with an organisation API key."));
    }
    Ok(ctx.principal.owner)
}

fn client() -> Result<&'static integrations::Composio, Response> {
    integrations::configured().ok_or_else(not_configured)
}

fn not_configured() -> Response {
    failure(StatusCode::SERVICE_UNAVAILABLE, "integrations_not_configured", "App integrations are not available yet.")
}

fn toolkit(slug: &str) -> Result<&'static str, Response> {
    integrations::toolkit(slug).ok_or_else(|| {
        failure(StatusCode::NOT_FOUND, "unknown_integration", "Supported integrations: hubspot, salesforce, slack, gmail, outlook, googlesheets.")
    })
}

fn provider_failed(context: &str, error: ComposioError) -> Response {
    if matches!(error, ComposioError::Unauthorized) {
        tracing::error!(context, "composio rejected the project key");
        return not_configured();
    }
    tracing::warn!(error = %error, context, "composio request failed");
    failure(StatusCode::BAD_GATEWAY, "integration_provider_unavailable", "The integration provider did not respond. Try again shortly.")
}

pub(super) async fn list(State(state): State<ServerState>, headers: HeaderMap) -> Response {
    let (owner, composio) = match (person(&state, &headers).await, client()) {
        (Ok(owner), Ok(composio)) => (owner, composio),
        (Err(r), _) | (_, Err(r)) => return r,
    };
    match composio.list(owner).await {
        Ok(rows) => Json(json!({ "integrations": rows })).into_response(),
        Err(error) => provider_failed("list", error),
    }
}

/// A hosted consent link. Open it in the person's browser; Composio sends
/// them back to the App's Integrations page afterwards.
pub(super) async fn connect(State(state): State<ServerState>, headers: HeaderMap, Path(slug): Path<String>) -> Response {
    let (owner, composio, toolkit) = match (person(&state, &headers).await, client(), toolkit(&slug)) {
        (Ok(owner), Ok(composio), Ok(toolkit)) => (owner, composio, toolkit),
        (Err(r), _, _) | (_, Err(r), _) | (_, _, Err(r)) => return r,
    };
    match composio.connect(owner, toolkit).await {
        Ok(link) => Json(link).into_response(),
        Err(error) => provider_failed("connect", error),
    }
}

pub(super) async fn disconnect(State(state): State<ServerState>, headers: HeaderMap, Path(slug): Path<String>) -> Response {
    let (owner, composio, toolkit) = match (person(&state, &headers).await, client(), toolkit(&slug)) {
        (Ok(owner), Ok(composio), Ok(toolkit)) => (owner, composio, toolkit),
        (Err(r), _, _) | (_, Err(r), _) | (_, _, Err(r)) => return r,
    };
    match composio.disconnect(owner, toolkit).await {
        Ok(removed) => Json(json!({ "toolkit": toolkit, "removed": removed })).into_response(),
        Err(error) => provider_failed("disconnect", error),
    }
}

/// What the person's connected apps can do, in the planner's capability shape.
/// The App adds these slugs to Ask's known slugs so a request like "post it in
/// Slack" plans `app.slack.sendMessage` instead of inventing a data lookup.
pub(super) async fn actions(State(state): State<ServerState>, headers: HeaderMap) -> Response {
    let (owner, composio) = match (person(&state, &headers).await, client()) {
        (Ok(owner), Ok(composio)) => (owner, composio),
        (Err(r), _) | (_, Err(r)) => return r,
    };
    match composio.actions(owner).await {
        Ok(actions) => Json(json!({ "actions": actions })).into_response(),
        Err(error) => provider_failed("actions", error),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExecuteAction {
    arguments: serde_json::Value,
}

/// Runs one action the person reviewed. App sessions only: an action writes
/// to somebody's CRM, mailbox or channel, so the arguments are confirmed by a
/// person on screen, never by an agent on their behalf.
pub(super) async fn execute(State(state): State<ServerState>, headers: HeaderMap, Path(slug): Path<String>, Json(body): Json<ExecuteAction>) -> Response {
    let (owner, composio) = match (super::app_person(&state, &headers).await, client()) {
        (Ok(person), Ok(composio)) => (person.owner, composio),
        (Err(r), _) | (_, Err(r)) => return r,
    };
    match composio.execute(owner, &slug, &body.arguments).await {
        Ok(outcome) => Json(outcome).into_response(),
        Err(integrations::ExecuteError::InvalidArguments) => failure(StatusCode::BAD_REQUEST, "invalid_arguments", "The action's arguments must be a JSON object under 64 KB."),
        Err(integrations::ExecuteError::UnknownAction) => failure(StatusCode::NOT_FOUND, "unknown_action", "This action is not available. Connect the app in Integrations first."),
        Err(integrations::ExecuteError::Provider(error)) => provider_failed("execute", error),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PrepareAction {
    question: String,
    #[serde(default)]
    results: serde_json::Value,
}

/// Proposes arguments for one action from the question and the results already
/// on screen. Nothing runs: the App shows the proposal for the person to check.
pub(super) async fn prepare(State(state): State<ServerState>, headers: HeaderMap, Path(slug): Path<String>, Json(body): Json<PrepareAction>) -> Response {
    let (owner, composio) = match (super::app_person(&state, &headers).await, client()) {
        (Ok(person), Ok(composio)) => (person.owner, composio),
        (Err(r), _) | (_, Err(r)) => return r,
    };
    let question = body.question.trim();
    if question.is_empty() || question.chars().count() > 16_000 {
        return failure(StatusCode::BAD_REQUEST, "invalid_question", "Enter a question of up to 16,000 characters.");
    }
    if serde_json::to_vec(&body.results).map_or(true, |bytes| bytes.len() > 64 * 1024) {
        return failure(StatusCode::PAYLOAD_TOO_LARGE, "results_too_large", "Send at most 64 KB of results.");
    }
    let Ok(_permit) = state.permits.try_acquire() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "busy", "The service is busy. Try again shortly.");
    };
    let actions = match composio.actions(owner).await {
        Ok(actions) => actions,
        Err(error) => return provider_failed("prepare", error),
    };
    let Some(action) = integrations::find_action(&actions, &slug) else {
        return failure(StatusCode::NOT_FOUND, "unknown_action", "This action is not available. Connect the app in Integrations first.");
    };
    match state.engine.parser.propose_action_arguments(question, action, &body.results).await {
        Ok(proposal) => Json(json!({ "action": action, "arguments": proposal["arguments"], "missing": proposal["missing"] })).into_response(),
        Err(error) => {
            tracing::warn!(error = %error, slug, "action arguments unavailable");
            // The action still runs with arguments the person enters by hand.
            Json(json!({ "action": action, "arguments": {}, "missing": action.input_schema["required"].as_array().cloned().unwrap_or_default(), "proposal_unavailable": true })).into_response()
        }
    }
}
