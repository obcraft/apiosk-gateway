//! A public, read-only quote, followed by an authenticated adoption/approval.
use super::{failure, ServerState};
use crate::{capability_intake::{self, Intake}, engine::guest_preview::{Preview, Refusal, MAX_TOKEN_BYTES}};
use axum::{extract::State, http::{HeaderMap, StatusCode}, response::{IntoResponse, Response}, Json};
use serde::Deserialize;
use std::{sync::OnceLock, time::Duration};
use tokio::sync::Semaphore;
use uuid::Uuid;

// Anonymous traffic cannot fill the authenticated planning pool. The shared
// parser's fixed guest key also applies its existing 30/minute replica limit;
// neither caller-supplied IDs nor spoofed forwarding headers bypass the cap.
static GUEST_PERMITS: OnceLock<Semaphore> = OnceLock::new();

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PreviewRequest { request_id: Uuid, question: String }

impl PreviewRequest {
    fn valid(&self) -> bool {
        !self.request_id.is_nil() && !self.question.trim().is_empty() && self.question.encode_utf16().count() <= 4_000
    }
}

pub(super) async fn preview(State(state): State<ServerState>, Json(request): Json<PreviewRequest>) -> Response {
    if !request.valid() {
        return failure(StatusCode::BAD_REQUEST, "invalid_question", "Enter a question of up to 4,000 characters.");
    }
    let Some(client) = state.engine.parser.ask_v2() else {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "parser_unavailable", "Source discovery is temporarily unavailable. Your question is kept; try again.");
    };
    let Ok(_permit) = GUEST_PERMITS.get_or_init(|| Semaphore::new(4)).try_acquire() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "busy", "Source discovery is busy. Try again shortly.");
    };
    if !client.admit("anonymous-ask-preview") {
        return failure(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "Please wait a minute before trying another question.");
    }
    let question = request.question.trim();
    let started=std::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(90), async {
        // One model request reads the question (its fallback only on failure);
        // the catalog search and the plan compiler are deterministic.
        let catalog = state.engine.catalog.get().await.map_err(|_| unavailable())?;
        let interpretation = client.interpret_with_context(question, question, question, &[]).await.map_err(|_| unavailable())?;
        let parsed_request = interpretation.parsed_request;
        let matches = crate::ask_v2::search::search(&parsed_request, &catalog);
        let intake = Intake {
            selected_capabilities: matches.iter().flat_map(|found| found.operations.iter().cloned()).collect(), matches,
            apiosk_sources_searched: capability_intake::source_count(&catalog), catalog_version: catalog.version.clone(), parsed_request,
        };
        let mut value = state.engine.guest_plan(question, intake, &catalog).await.map_err(|_| unavailable())?;
        value["timing"]=serde_json::json!({"request_to_plan_ms":started.elapsed().as_millis() as u64,"parse_ms":interpretation.output.timing.parse_ms,
            "parse_attempts":interpretation.attempts,"model_http_requests":crate::telemetry::model_http_requests()});
        Ok(value)
    }).await;
    match result {
        Ok(Ok(value)) => ([(axum::http::header::CACHE_CONTROL, "private, no-store")], Json(value)).into_response(),
        Ok(Err(response)) => response,
        Err(_) => unavailable(),
    }
}

fn unavailable() -> Response {
    failure(StatusCode::SERVICE_UNAVAILABLE, "guest_preview_unavailable", "The sources could not be priced. Your question is kept; try again.")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ApproveRequest { request_id: Uuid, preview_token: String }

pub(super) async fn approve(State(state): State<ServerState>, headers: HeaderMap, Json(request): Json<ApproveRequest>) -> Response {
    // Starter credit and the initial guest question belong to the personal
    // account, regardless of a workspace selected in an older browser tab.
    let bearer = super::token(&headers).unwrap_or_default().to_owned();
    let person = match state.auth.verified_guest_person(&bearer).await {
        Ok(person) => person,
        Err(crate::auth::AuthError::Unauthorized | crate::auth::AuthError::Refused(_)) =>
            return failure(StatusCode::UNAUTHORIZED, "guest_login_required", "Sign in with a verified account to approve this question."),
        Err(crate::auth::AuthError::Unavailable(_)) =>
            return failure(StatusCode::SERVICE_UNAVAILABLE, "auth_unavailable", "Your sign-in could not be verified. Try again shortly."),
    };
    if request.request_id.is_nil() || request.preview_token.len() > MAX_TOKEN_BYTES {
        return failure(StatusCode::BAD_REQUEST, "invalid_guest_preview", "Search the question again before approving it.");
    }
    let preview = match Preview::open(&state.engine.signer, &request.preview_token) {
        Ok(preview) => preview,
        Err(_) => return failure(StatusCode::BAD_REQUEST, "invalid_guest_preview", "Search the question again before approving it."),
    };
    let Ok(permit) = state.permits.clone().try_acquire_owned() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "busy", "Try again shortly. Your approved question is kept.");
    };
    // Persist and approve even when the browser disconnects. Retrying the
    // same token, including with a new request ID, always recovers this task.
    let result = crate::telemetry::spawn(async move {
        let _permit = permit;
        let (id, approved) = state.engine.adopt_guest(&person, &preview).await?;
        if !approved {
            state.engine.executor.approve(&person, id, preview.quote_ref, &preview.total_atomic.to_string()).await?;
        }
        if let Err(error) = state.engine.start_worker(person.clone(), bearer, id).await {
            tracing::warn!(task=%id,error=%error,"guest approval saved; execution requires recovery");
        }
        state.engine.view(&person, id, request.request_id, None).await
    }).await;
    match result {
        Ok(Ok(response)) => ([(axum::http::header::CACHE_CONTROL, "private, no-store")], Json(response)).into_response(),
        Ok(Err(error)) => match error.downcast_ref::<Refusal>() {
            Some(Refusal::Expired) => failure(StatusCode::CONFLICT, "guest_preview_expired", "This price expired. Search again and review the new total before approving."),
            Some(Refusal::Changed) => failure(StatusCode::CONFLICT, "guest_preview_changed", "A source or its price changed. Search again and review the new total before approving."),
            Some(Refusal::Used) => failure(StatusCode::CONFLICT, "guest_preview_used", "This question was already continued by another account. Search again to prepare your own request."),
            None => {
                tracing::warn!(error=%error,"guest approval could not be confirmed");
                failure(StatusCode::SERVICE_UNAVAILABLE, "guest_approval_unavailable", "Approval could not be confirmed. Retry this saved question to recover its status.")
            },
        },
        Err(_) => failure(StatusCode::SERVICE_UNAVAILABLE, "guest_approval_unavailable", "Approval could not be confirmed. Retry this saved question to recover its status."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::{to_bytes, Body}, http::Request};
    use serde_json::{json, Value};
    use std::sync::Arc;
    use tower::ServiceExt;

    fn state() -> ServerState {
        let pool = sqlx::postgres::PgPoolOptions::new().connect_lazy("postgresql://unused@127.0.0.1:1/unused").unwrap();
        let http = reqwest::Client::new();
        let store = crate::store::Store { pool:pool.clone() };
        ServerState {
            auth:Arc::new(crate::auth::Auth { pool:pool.clone(), http:http.clone(), supabase_url:"http://127.0.0.1:1".into(), publishable_key:"unused".into() }),
            engine:Arc::new(crate::engine::Engine {
                store:store.clone(), catalog:crate::catalog::CatalogCache::new(pool),
                parser:crate::parser::Parser::new(http.clone(), "http://127.0.0.1:1".into(), "unused".into(), "unused".into()),
                signer:crate::state::Signer::new(vec![7;32]).unwrap(),
                executor:crate::execution::Executor { store,http,supabase_url:"http://127.0.0.1:1".into(),enabled:false },
                app_url:"http://localhost:5173".into(),
            }),
            permits:Arc::new(Semaphore::new(0)), executor_compat:None,
        }
    }

    #[tokio::test]
    async fn guest_routes_bound_anonymous_input_and_keep_every_execution_authenticated() {
        let app = crate::server::router(state(), "http://localhost:5173").unwrap();
        let post = |path: &str, body: Value| Request::post(path).header("content-type", "application/json").body(Body::from(body.to_string())).unwrap();
        for question in ["".to_owned(), "x".repeat(4001), "😀".repeat(2001)] {
            let response = app.clone().oneshot(post("/v2/ask-v2/guest/preview", json!({"request_id":Uuid::new_v4(),"question":question}))).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
        let response = app.clone().oneshot(post("/v2/ask-v2/guest/preview", json!({"request_id":Uuid::new_v4(),"question":"Search Example"}))).await.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "valid guest input reaches discovery without authentication");
        let value:Value = serde_json::from_slice(&to_bytes(response.into_body(), 16*1024).await.unwrap()).unwrap();
        assert_eq!(value["errors"][0]["code"], "parser_unavailable");
        for (path, body) in [
            ("/v2/ask-v2/guest/approve".to_owned(), json!({"request_id":Uuid::new_v4(),"preview_token":"x".repeat(100_000)})),
            ("/v2/ask-v2/prepare".to_owned(), json!({"request_id":Uuid::new_v4(),"endpoint_id":Uuid::new_v4(),"input":{}})),
            (format!("/v2/tasks/{}/authorize", Uuid::new_v4()), json!({"quote_ref":Uuid::new_v4(),"max_total_atomic":"1"})),
        ] {
            let response = app.clone().oneshot(post(&path, body)).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}: neither token nor payment price authorizes an anonymous execution; large signed tokens fit the route body limit");
        }
        let response = app.oneshot(post("/v2/ask-v2/guest/approve", json!({"request_id":Uuid::new_v4(),"preview_token":"x".repeat(270_000)}))).await.unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
