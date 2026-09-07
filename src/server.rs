use crate::{
    auth::{Auth, Principal},
    engine::Engine,
    protocol::{DiscoverRequest, ExecuteRequest, Response, Status},
};
use axum::{
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response as HttpResponse},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;
use tower_http::cors::CorsLayer;
use uuid::Uuid;

#[derive(Clone)]
pub struct ServerState {
    pub engine: Arc<Engine>,
    pub auth: Arc<Auth>,
    pub permits: Arc<Semaphore>,
}
pub fn router(state: ServerState, app_origin: &str) -> anyhow::Result<Router> {
    let origin: HeaderValue = app_origin.parse()?;
    let cors = CorsLayer::new()
        .allow_origin(origin)
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([
            axum::http::header::AUTHORIZATION,
            axum::http::header::CONTENT_TYPE,
        ])
        .max_age(Duration::from_secs(600));
    Ok(Router::new()
        .route(
            "/health",
            get(|| async { Json(json!({"service":"apiosk-gateway-v2","protocol_version":"2"})) }),
        )
        .route("/v2/discover", post(discover))
        .route("/v2/chat", post(chat))
        .route("/v2/follow-up", post(followup))
        .route("/v2/execute", post(execute))
        .route("/v2/tasks", get(tasks))
        .route("/v2/tasks/:id", get(task))
        .route("/v2/tasks/:id/authorize", post(authorize))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(cors)
        .with_state(state))
}
fn token(headers: &HeaderMap) -> anyhow::Result<&str> {
    headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or_else(|| anyhow::anyhow!("authentication required"))
}
fn failure(status: StatusCode, code: &str, message: &str) -> HttpResponse {
    (
        status,
        Json(Response::error(
            Uuid::new_v4(),
            Status::Failed,
            code,
            message,
        )),
    )
        .into_response()
}
async fn caller(state: &ServerState, headers: &HeaderMap) -> Result<Principal, HttpResponse> {
    let bearer = token(headers).map_err(|_| {
        failure(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Verbind je account om door te gaan.",
        )
    })?;
    state.auth.authenticate(bearer).await.map_err(|_| {
        failure(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "De verbinding is ongeldig of verlopen.",
        )
    })
}
async fn discover(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(req): Json<DiscoverRequest>,
) -> HttpResponse {
    let Ok(_permit) = state.permits.try_acquire() else {
        return failure(
            StatusCode::TOO_MANY_REQUESTS,
            "busy",
            "Probeer het over enkele seconden opnieuw.",
        );
    };
    let p = match caller(&state, &headers).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    match state.engine.discover(&p, req).await {
        Ok(r) => Json(r).into_response(),
        Err(e) => {
            tracing::error!(error=%e,"discover persistence failure");
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "De taak kon niet worden opgeslagen.",
            )
        }
    }
}
async fn chat(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(req): Json<DiscoverRequest>,
) -> HttpResponse {
    let Ok(_permit) = state.permits.try_acquire() else {
        return failure(
            StatusCode::TOO_MANY_REQUESTS,
            "busy",
            "Probeer het over enkele seconden opnieuw.",
        );
    };
    let p = match caller(&state, &headers).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    if !p.is_person() {
        return failure(
            StatusCode::FORBIDDEN,
            "app_only",
            "Gebruik de discover-tool voor verbonden chatbots.",
        );
    }
    match state.engine.chat(&p, req).await {
        Ok(r) => Json(r).into_response(),
        Err(e) => {
            tracing::error!(error=%e, "chat persistence failure");
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "Het gesprek kon niet worden opgeslagen.",
            )
        }
    }
}
async fn followup(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(req): Json<crate::engine::FollowupRequest>,
) -> HttpResponse {
    let Ok(_permit) = state.permits.try_acquire() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "busy", "Try again shortly.");
    };
    let p = match caller(&state, &headers).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    if !p.is_person() {
        return failure(
            StatusCode::FORBIDDEN,
            "app_only",
            "Related services are available in the App.",
        );
    }
    match state.engine.followup(&p, req).await {
        Ok(r) => Json(r).into_response(),
        Err(_) => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "The follow-up plan could not be saved. Refresh the task.",
        ),
    }
}
async fn execute(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(req): Json<ExecuteRequest>,
) -> HttpResponse {
    let Ok(permit) = state.permits.clone().try_acquire_owned() else {
        return failure(
            StatusCode::TOO_MANY_REQUESTS,
            "busy",
            "Probeer het over enkele seconden opnieuw.",
        );
    };
    let p = match caller(&state, &headers).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let bearer = token(&headers).unwrap().to_string();
    // The bounded server task survives a client disconnect. No credentials
    // are persisted; after a process crash the execution journal reconciles.
    let engine = state.engine.clone();
    match tokio::spawn(async move {
        let _permit = permit;
        engine.execute(&p, &bearer, req).await
    })
    .await
    {
        Ok(Ok(r)) => Json(r).into_response(),
        _ => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "execution_pending",
            "Controleer de taakstatus voordat je opnieuw uitvoert.",
        ),
    }
}
async fn tasks(State(state): State<ServerState>, headers: HeaderMap) -> HttpResponse {
    let Ok(_permit) = state.permits.try_acquire() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "busy", "Try again shortly.");
    };
    let p = match caller(&state, &headers).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    if !p.is_person() {
        return failure(
            StatusCode::FORBIDDEN,
            "app_only",
            "Chat history is available in the App.",
        );
    }
    match state.engine.store.list_tasks(p.owner).await {
        Ok(tasks) => Json(json!({"tasks": tasks})).into_response(),
        Err(_) => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "history_unavailable",
            "Chat history could not be loaded. Try again.",
        ),
    }
}
async fn task(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> HttpResponse {
    let p = match caller(&state, &headers).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    match state.engine.view(&p, id, Uuid::new_v4(), None).await {
        Ok(r) => Json(r).into_response(),
        Err(_) => failure(
            StatusCode::NOT_FOUND,
            "task_unavailable",
            "Deze taak is niet beschikbaar voor dit account.",
        ),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Approval {
    quote_ref: Uuid,
    max_total_atomic: String,
}
async fn authorize(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<Approval>,
) -> HttpResponse {
    let p = match caller(&state, &headers).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    match state
        .engine
        .executor
        .approve(&p, id, req.quote_ref, &req.max_total_atomic)
        .await
    {
        Ok(reference) => {
            Json(json!({"authorization_ref":reference,"task_id":id,"quote_ref":req.quote_ref}))
                .into_response()
        }
        Err(_) => failure(
            StatusCode::FORBIDDEN,
            "approval_refused",
            "Open het actuele voorstel in de app en keur het daar goed.",
        ),
    }
}
