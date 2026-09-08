use crate::{
    auth::{Auth, Principal},
    compat::ExecutorCompat,
    engine::Engine,
    protocol::{DiscoverRequest, ExecuteRequest, Response, Status},
};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, OriginalUri, Path, Query, State},
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
    pub executor_compat: Option<ExecutorCompat>,
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
    let compat = state.executor_compat.clone();
    let mut app = Router::new()
        .route(
            "/health",
            get({
                let compat = compat.clone();
                move || async move {
                    let executor = match compat {
                        Some(proxy) if proxy.healthy().await => "ready",
                        Some(_) => "unavailable",
                        None => "external",
                    };
                    let status = if executor == "unavailable" {
                        StatusCode::SERVICE_UNAVAILABLE
                    } else {
                        StatusCode::OK
                    };
                    (
                        status,
                        Json(json!({
                            "service":"apiosk-gateway-v2",
                            "protocol_version":"2",
                            "payment_executor":executor
                        })),
                    )
                }
            }),
        )
        .route("/v2/discover", post(discover))
        .route("/v2/chat", post(chat))
        .route("/v2/sources", get(sources))
        .route("/v2/follow-up", post(followup))
        .route("/v2/execute", post(execute))
        .route("/v2/approve", post(approve_connection))
        .route("/v2/tasks", get(tasks))
        .route("/v2/tasks/:id", get(task))
        .route("/v2/tasks/:task/results/:result/report.pdf", get(annual_report))
        .route("/v2/tasks/:id/authorize", post(authorize))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(cors);
    if compat.is_some() {
        app = app.fallback(legacy_fallback);
    }
    Ok(app.with_state(state))
}

#[derive(Deserialize)]
struct ReportDownload { owner: Uuid, expires: i64, signature: String }
async fn annual_report(
    State(state): State<ServerState>,
    Path((task, result)): Path<(Uuid, Uuid)>,
    Query(query): Query<ReportDownload>,
) -> HttpResponse {
    let Ok(_permit) = state.permits.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    if state.engine.signer.verify_report(query.owner, task, result, query.expires, &query.signature).is_err() {
        return StatusCode::FORBIDDEN.into_response();
    }
    // A scoped, expiring capability permits browser downloads without exposing
    // account credentials. Store lookup still enforces owner and task retention.
    let Ok(document) = state.engine.store.result(query.owner, task, result).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !crate::annual_report::eligible(&document) { return StatusCode::NOT_FOUND.into_response(); }
    match tokio::task::spawn_blocking(move || {
        let _permit = _permit;
        crate::annual_report::render(&document)
    }).await {
        Ok(Ok(pdf)) => (
            [("content-type", "application/pdf".to_owned()),
             ("content-disposition", format!("attachment; filename=\"jaarrekening-{result}.pdf\"")),
             ("cache-control", "private, no-store".to_owned()),
             ("referrer-policy", "no-referrer".to_owned()),
             ("x-content-type-options", "nosniff".to_owned())], pdf
        ).into_response(),
        _ => failure(StatusCode::INTERNAL_SERVER_ERROR, "report_unavailable", "PDF generation failed. Your saved source data is still available; no new purchase is needed."),
    }
}

async fn legacy_fallback(
    State(state): State<ServerState>,
    OriginalUri(uri): OriginalUri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> HttpResponse {
    match state.executor_compat {
        Some(proxy) => crate::compat::forward_request(proxy, uri, method, headers, body).await,
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
fn token(headers: &HeaderMap) -> anyhow::Result<&str> {
    headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or_else(|| anyhow::anyhow!("authentication required"))
}
async fn sources(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Query(query): Query<crate::sources::SourceQuery>,
) -> HttpResponse {
    let Ok(_permit) = state.permits.try_acquire() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "busy", "Try again shortly.");
    };
    if let Err(r) = caller(&state, &headers).await {
        return r;
    }
    if !query.valid() {
        return failure(
            StatusCode::BAD_REQUEST,
            "invalid_query",
            "Use up to 200 characters per filter and a limit from 1 to 50.",
        );
    }
    match tokio::try_join!(state.engine.catalog.sources(), state.engine.catalog.get()) {
        Ok((sources, catalog)) => {
            Json(crate::sources::browse(&sources, &catalog, &query)).into_response()
        }
        Err(_) => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "catalog_unavailable",
            "Sources are temporarily unavailable. Try again shortly.",
        ),
    }
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
            "The connection is invalid or expired.",
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
                "The task could not be saved.",
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
                "The conversation could not be saved.",
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
            "Check the task status before retrying execution.",
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
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectionApproval {
    request_id: Uuid,
    state: crate::state::Envelope,
    quote_ref: Uuid,
    max_total_atomic: String,
}
async fn approve_connection(
    State(state): State<ServerState>, headers: HeaderMap, Json(req): Json<ConnectionApproval>,
) -> HttpResponse {
    let p = match caller(&state, &headers).await { Ok(p) => p, Err(r) => return r };
    let result = async {
        state.engine.signer.verify(p.owner, &req.state, false)?;
        state.engine.executor.approve_connection(&p, req.state.state_ref, req.state.revision,
            req.quote_ref, &req.max_total_atomic).await?;
        state.engine.view(&p, req.state.state_ref, req.request_id, None).await
    }.await;
    match result {
        Ok(response) => Json(response).into_response(),
        Err(error) => {
            tracing::warn!(error=%error,"connection approval refused");
            failure(StatusCode::FORBIDDEN, "approval_refused",
                "Approval could not be saved. Check the current plan, balance and connection spending limits, then try again. No new purchase was made by this approval request.")
        }
    }
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
            "Open the current request in Apiosk and approve it there.",
        ),
    }
}
