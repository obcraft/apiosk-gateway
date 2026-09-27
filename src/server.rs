use crate::{
    server_events::task_events,
    auth::{Auth, AuthContext, AuthError, KeyRefusal, Principal},
    compat::ExecutorCompat,
    engine::Engine,
    protocol::{DiscoverRequest, ExecuteRequest, Response, Status},
    telemetry::Phase,
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
use tower_http::cors::{Any, CorsLayer};
use uuid::Uuid;

mod ask_v2;
mod ask_v2_external;

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
            axum::http::HeaderName::from_static("x-apiosk-workspace"),
            axum::http::HeaderName::from_static("idempotency-key"),
            axum::http::HeaderName::from_static(crate::telemetry::TRACE_HEADER),
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
                            // The commit this image was built from, so a
                            // deploy can be matched to a branch at a glance.
                            "version":std::env::var("GATEWAY_GIT_SHA").unwrap_or_else(|_| "unknown".into()),
                            "payment_executor":executor
                        })),
                    )
                }
            }),
        )
        .route("/v2/meta", get(crate::routes::intake::meta))
        .route("/v2/requests/:request_id", get(request_receipt))
        .route("/v2/discover", post(discover))
        .route("/v2/chat", post(chat))
        .route("/v2/ask-v2/parse", post(ask_v2::parse).layer(DefaultBodyLimit::max(128 * 1024)))
        .route("/v2/ask-v2/search", post(ask_v2::search).layer(DefaultBodyLimit::max(256 * 1024)))
        .route("/v2/ask-v2/endpoints/:id", get(ask_v2::endpoint))
        .route("/v2/ask-v2/prepare", post(ask_v2::prepare))
        .route("/v2/ask-v2/external/source", post(ask_v2_external::source))
        .route("/v2/ask-v2/external/prepare", post(ask_v2_external::prepare))
        .route("/v2/ask-v2/external/:id/authorize", post(ask_v2_external::authorize))
        .route("/v2/ask-v2/external/:id", get(ask_v2_external::status))
        .route("/v2/sources", get(sources))
        .route("/v2/capabilities/resolve", post(crate::public_api::resolve_capability))
        .route("/v2/capabilities/plan", post(crate::public_api::plan_capability))
        .route("/v2/sources/:source_id/endpoints", get(source_endpoints))
        .route("/v1/source-readiness", get(source_readiness))
        .route("/v2/follow-up", post(followup))
        .route("/v2/execute", post(execute))
        .route("/v2/approve", post(approve_connection))
        .route("/v2/tasks", get(tasks))
        .route("/v2/tasks/:id", get(task))
        .route("/v2/tasks/:task/results/:result/report.pdf", get(annual_report))
        .route("/v2/tasks/:task/results/:result/assets/:index", get(result_asset))
        .route("/v2/tasks/:task/reports/:quote/report.pdf", get(research_report))
        .route("/v2/tasks/:task/reports/:quote/evidence.zip", get(research_evidence))
        .route("/v2/tasks/:id/authorize", post(authorize))
        .route("/v2/tasks/:id/requote", post(requote))
        // The public API: every credential, one execution and billing path.
        .route("/v2/sources/:source_id", get(crate::public_api::source))
        .route("/v2/sources/:source_id/run", post(crate::public_api::run_source))
        // The shortest paid call: the key pays, the path names the source and
        // operation, and the body is the input values themselves.
        .route("/v1/execute/:source/:operation", post(crate::public_api::execute))
        .route("/v2/execute/:source/:operation", post(crate::public_api::execute))
        .route("/v2/ask", post(crate::public_api::ask))
        .route("/v2/ask/workflows", get(crate::public_api::ask_workflows))
        .route("/v2/asks/:id", get(crate::public_api::run_status))
        .route("/v2/asks/:id/run", post(crate::public_api::run_ask))
        .route("/v2/asks/:id/continue", post(crate::public_api::continue_ask))
        .route("/v2/runs/:id", get(crate::public_api::run_status))
        .route("/v2/balance", get(crate::public_api::balance))
        .route("/v2/usage", get(crate::public_api::usage))
        .layer(DefaultBodyLimit::max(64 * 1024))
        // A scanned invoice page image exceeds the default JSON limit.
        .route("/v2/documents", post(read_document).layer(DefaultBodyLimit::max(16 * 1024 * 1024)))
        .route("/v2/documents/:id/read", post(read_document_again).layer(DefaultBodyLimit::max(64 * 1024)))
        .route("/v2/telemetry", post(telemetry_beacon).layer(DefaultBodyLimit::max(crate::telemetry::BEACON_MAX_BYTES)))
        .route("/v2/workflows", get(workflows))
        .route("/v2/workflows/:slug/start", post(start_workflow))
        .route("/v2/workflows/runs", get(workflow_runs))
        .route("/v2/workflows/runs/:id", get(workflow_run))
        .layer(cors)
        // EventSource runs inside the host's embedded card origin. The signed
        // task capability is read-only; keep bearer/approval routes app-only.
        .route("/v2/tasks/:id/events", get(task_events).layer(
            CorsLayer::new().allow_origin(Any).allow_methods([Method::GET])
        ));
    if compat.is_some() {
        app = app.fallback(legacy_fallback);
    }
    let telemetry = axum::middleware::from_fn_with_state(state.engine.store.pool.clone(), crate::telemetry::middleware);
    Ok(app.layer(telemetry).with_state(state))
}

#[derive(Deserialize)]
struct ReportDownload { owner: Uuid, expires: i64, signature: String }
#[derive(Deserialize)]
struct ResearchDownload { owner: Uuid, expires: i64, signature: String, snapshot: String }
async fn research_report(State(state): State<ServerState>, Path((task,quote)): Path<(Uuid,Uuid)>, Query(query): Query<ResearchDownload>) -> HttpResponse {
    let Ok(permit)=state.permits.clone().try_acquire_owned() else {return StatusCode::TOO_MANY_REQUESTS.into_response()};
    if state.engine.signer.verify_research(query.owner,task,quote,&query.snapshot,query.expires,&query.signature).is_err() {return StatusCode::FORBIDDEN.into_response()}
    let Ok((_,saved,_))=state.engine.store.load(query.owner,task).await else {return StatusCode::NOT_FOUND.into_response()};
    if state.engine.store.check_workspace(&Principal {owner:query.owner,connection:None,workspace:saved.workspace_id}).await.is_err(){return StatusCode::NOT_FOUND.into_response()};
    let Ok(document)=state.engine.research_document(query.owner,task,&saved,quote).await else {return StatusCode::NOT_FOUND.into_response()};
    if crate::state::digest(&document)!=query.snapshot {return StatusCode::CONFLICT.into_response()}
    match tokio::task::spawn_blocking(move || {let _permit=permit;crate::research_report::render(&document)}).await {
        Ok(Ok(pdf)) => ([("content-type","application/pdf".to_owned()),("content-disposition",format!("attachment; filename=\"analyse-{quote}.pdf\"")),("cache-control","private, no-store".to_owned()),("referrer-policy","no-referrer".to_owned()),("x-content-type-options","nosniff".to_owned())],pdf).into_response(),
        _ => failure(StatusCode::INTERNAL_SERVER_ERROR,"report_unavailable","PDF generation failed. Saved results and analysis remain available; no new purchase is needed."),
    }
}
async fn research_evidence(
    State(state): State<ServerState>,
    Path((task, quote)): Path<(Uuid, Uuid)>,
    Query(query): Query<ResearchDownload>,
) -> HttpResponse {
    let Ok(permit) = state.permits.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    if state
        .engine
        .signer
        .verify_research(
            query.owner,
            task,
            quote,
            &query.snapshot,
            query.expires,
            &query.signature,
        )
        .is_err()
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok((_, saved, _)) = state.engine.store.load(query.owner, task).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if state
        .engine
        .store
        .check_workspace(&Principal {
            owner: query.owner,
            connection: None,
            workspace: saved.workspace_id,
        })
        .await
        .is_err()
    {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(document) = state
        .engine
        .research_document(query.owner, task, &saved, quote)
        .await
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if crate::state::digest(&document) != query.snapshot {
        return StatusCode::CONFLICT.into_response();
    }
    match tokio::task::spawn_blocking(move || {let _permit=permit;crate::research_report::evidence_bundle(&document)}).await {
        Ok(Ok(bytes)) => ([("content-type","application/zip".to_owned()),("content-disposition",format!("attachment; filename=\"evidence-{quote}.zip\"")),("cache-control","private, no-store".to_owned()),("referrer-policy","no-referrer".to_owned()),("x-content-type-options","nosniff".to_owned())],bytes).into_response(),
        _ => failure(StatusCode::INTERNAL_SERVER_ERROR,"evidence_unavailable","Evidence export failed. Saved source results remain available; no new purchase is needed."),
    }
}
/// A map captured while the result was bought, served for an inline <img>.
/// Same scoped, expiring capability and owner/workspace checks as the PDF.
async fn result_asset(
    State(state): State<ServerState>,
    Path((task, result, index)): Path<(Uuid, Uuid, usize)>,
    Query(query): Query<ReportDownload>,
) -> HttpResponse {
    if state.engine.signer.verify_asset(query.owner, task, result, index, query.expires, &query.signature).is_err() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok((_,saved,_))=state.engine.store.load(query.owner,task).await else {return StatusCode::NOT_FOUND.into_response()};
    if state.engine.store.check_workspace(&Principal {owner:query.owner,connection:None,workspace:saved.workspace_id}).await.is_err(){return StatusCode::NOT_FOUND.into_response()};
    let Ok(document) = state.engine.store.result(query.owner, task, result).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let asset = &document[crate::report_assets::KEY]["assets"][index];
    if asset["kind"] != "map" { return StatusCode::NOT_FOUND.into_response(); }
    // bytes() re-checks the digest, PNG signature and map dimensions.
    let Ok(png) = crate::report_assets::bytes(asset) else { return StatusCode::NOT_FOUND.into_response() };
    ([("content-type", "image/png"),
      ("cache-control", "private, max-age=3600"),
      ("referrer-policy", "no-referrer"),
      ("x-content-type-options", "nosniff")], png).into_response()
}

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
    let Ok((_,saved,_))=state.engine.store.load(query.owner,task).await else {return StatusCode::NOT_FOUND.into_response()};
    if state.engine.store.check_workspace(&Principal {owner:query.owner,connection:None,workspace:saved.workspace_id}).await.is_err(){return StatusCode::NOT_FOUND.into_response()};
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
    if headers.contains_key("x-apiosk-workspace") { return failure(StatusCode::FORBIDDEN,"workspace_route_required","Use Gateway v2 for workspace requests."); }
    if token(&headers).is_ok_and(|t|t.starts_with("apk_")) {
        match caller(&state,&headers).await { Ok(p) if p.workspace.is_none()=>{},Ok(_)=>return failure(StatusCode::FORBIDDEN,"workspace_route_required","Use Gateway v2 for workspace requests."),Err(r)=>return r }
    }
    match state.executor_compat {
        Some(proxy) => crate::compat::forward_request(proxy, uri, method, headers, body).await,
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
pub(crate) fn token(headers: &HeaderMap) -> anyhow::Result<&str> {
    headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or_else(|| anyhow::anyhow!("authentication required"))
}
// Public, aggregate catalog metadata only. No credentials, samples or customer data.
async fn source_readiness(State(state): State<ServerState>) -> HttpResponse {
    let Ok(_permit) = state.permits.try_acquire() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "busy", "Try again shortly.");
    };
    match state.engine.catalog.get().await {
        Ok(catalog) => Json(crate::readiness::public_report(&catalog)).into_response(),
        Err(_) => failure(StatusCode::SERVICE_UNAVAILABLE, "catalog_unavailable", "Readiness audit is temporarily unavailable."),
    }
}
async fn sources(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Query(query): Query<crate::sources::SourceQuery>,
) -> HttpResponse {
    let Ok(_permit) = state.permits.try_acquire() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "busy", "Try again shortly.");
    };
    match authenticated(&state, &headers).await {
        Ok(ctx) if !ctx.allows("sources:read") => return crate::public_api::insufficient_scope("sources:read"),
        Ok(_) => {}
        Err(refusal) if is_api_key(&headers) => return crate::public_api::refused(refusal),
        Err(refusal) => return failure(refusal.status, refusal.code, refusal.message),
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
async fn source_endpoints(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(source_id): Path<String>,
    Query(query): Query<crate::sources::EndpointQuery>,
) -> HttpResponse {
    let Ok(_permit) = state.permits.try_acquire() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "busy", "Try again shortly.");
    };
    match authenticated(&state, &headers).await {
        Ok(ctx) if !ctx.allows("sources:read") => return crate::public_api::insufficient_scope("sources:read"),
        Ok(_) => {}
        Err(refusal) if is_api_key(&headers) => return crate::public_api::refused(refusal),
        Err(refusal) => return failure(refusal.status, refusal.code, refusal.message),
    }
    if !query.valid() || source_id.len() > 100 || !source_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return failure(StatusCode::BAD_REQUEST, "invalid_query", "Use a source slug, up to 200 search characters, and a limit from 1 to 100.");
    }
    match state.engine.catalog.get().await {
        Ok(catalog) => match crate::sources::endpoint_page(&state.engine.store.pool, &catalog, &source_id, &query).await {
            Ok(page) => Json(page).into_response(),
            Err(_) => failure(StatusCode::SERVICE_UNAVAILABLE, "catalog_unavailable", "Source endpoints are temporarily unavailable."),
        },
        Err(_) => failure(StatusCode::SERVICE_UNAVAILABLE, "catalog_unavailable", "Source endpoints are temporarily unavailable."),
    }
}
pub(crate) fn failure(status: StatusCode, code: &str, message: &str) -> HttpResponse {
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
/// Why a request was refused before it reached the engine. Each surface
/// renders it in its own error shape; the decision is made once, here.
#[derive(Debug)]
pub(crate) struct Refusal {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: &'static str,
}
const fn refusal(status: StatusCode, code: &'static str, message: &'static str) -> Refusal {
    Refusal { status, code, message }
}
pub(crate) fn is_api_key(headers: &HeaderMap) -> bool {
    token(headers).is_ok_and(|t| t.starts_with(crate::auth::ORGANISATION_KEY_PREFIX))
}
/// Every credential resolves here: extract, authenticate, then bind the
/// workspace a person or connection may act in. An organisation key always
/// spends from its organisation's main account and never names a workspace.
pub(crate) async fn authenticated(state: &ServerState, headers: &HeaderMap) -> Result<AuthContext, Refusal> {
    let bearer = token(headers).map_err(|_| {
        refusal(StatusCode::UNAUTHORIZED, "unauthorized", "Verbind je account om door te gaan.")
    })?;
    let mut ctx = crate::telemetry::timed(Phase::Auth, crate::auth::credential_kind(bearer), state.auth.resolve(bearer)).await.map_err(|error| match error {
        AuthError::Unauthorized => refusal(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "The connection is invalid or expired.",
        ),
        AuthError::Refused(KeyRefusal::Expired) => refusal(StatusCode::UNAUTHORIZED, "api_key_expired", "This API key has expired. Create a new key in Apiosk > Organisation > API keys."),
        AuthError::Refused(KeyRefusal::Revoked) => refusal(StatusCode::UNAUTHORIZED, "api_key_revoked", "This API key was revoked. Create a new key in Apiosk > Organisation > API keys."),
        AuthError::Refused(KeyRefusal::Unavailable) => refusal(StatusCode::FORBIDDEN, "organisation_unavailable", "This organisation cannot spend from its main balance right now. Ask an organisation owner to check Apiosk > Organisation."),
        AuthError::Unavailable(source) => {
            tracing::error!(error=%source, "authentication dependency unavailable");
            refusal(
                StatusCode::SERVICE_UNAVAILABLE,
                "authentication_unavailable",
                "Authentication could not be confirmed. Your session may still be valid; try again shortly.",
            )
        }
    })?;
    crate::telemetry::set_surface(crate::telemetry::Surface::of(&ctx));
    if ctx.api_key.is_some() {
        if headers.contains_key("x-apiosk-workspace") {
            return Err(refusal(StatusCode::BAD_REQUEST, "invalid_workspace", "Organisation API keys spend from the organisation's main balance; do not send a workspace."));
        }
        return Ok(ctx);
    }
    let principal = &mut ctx.principal;
    let selected = headers.get("x-apiosk-workspace").map(|h| h.to_str().ok().and_then(|s|s.parse::<Uuid>().ok()).ok_or_else(||refusal(StatusCode::BAD_REQUEST,"invalid_workspace","Invalid workspace."))).transpose()?;
    let enabled = std::env::var("WORKSPACE_CONTEXT_ENABLED").as_deref() == Ok("true");
    if selected.is_some() && !enabled { return Err(refusal(StatusCode::FORBIDDEN,"workspace_unavailable","Workspace execution is not enabled.")); }
    let unconfirmed = || refusal(StatusCode::SERVICE_UNAVAILABLE,"workspace_service_unavailable","Workspace access could not be confirmed. Try again shortly.");
    if enabled && (selected.is_some() || principal.connection.is_some()) {
        let workspace: serde_json::Value = crate::telemetry::timed(Phase::Workspace, "gateway_context", sqlx::query_scalar("SELECT public.workspace_gateway_context($1,$2,$3)")
            .bind(principal.owner).bind(principal.connection).bind(selected).fetch_one(&state.engine.store.pool)).await
            .map_err(|error| {
                if crate::store::is_workspace_access_denied(&error) {
                    refusal(StatusCode::FORBIDDEN,"workspace_access_denied","You no longer have access to this workspace.")
                } else {
                    tracing::error!(error=%error, "workspace lookup unavailable");
                    unconfirmed()
                }
            })?;
        principal.workspace = match workspace.get("workspace_id") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(value)) => Some(value.parse().map_err(|_| unconfirmed())?),
            _ => return Err(unconfirmed()),
        };
        if selected.is_some() && principal.workspace != selected {
            return Err(unconfirmed());
        }
    }
    Ok(ctx)
}
/// The App and MCP protocol routes. Organisation keys use the public API,
/// where every route checks the key's scopes.
async fn caller(state: &ServerState, headers: &HeaderMap) -> Result<Principal, HttpResponse> {
    let ctx = authenticated(state, headers).await.map_err(|r| failure(r.status, r.code, r.message))?;
    if ctx.api_key.is_some() {
        return Err(failure(StatusCode::FORBIDDEN, "public_api_required", "Organisation API keys use the public API: GET /v2/sources, POST /v2/sources/{id}/run and POST /v2/ask."));
    }
    Ok(ctx.principal)
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
/// `GET /v2/requests/:request_id` (#11, `request_receipts_v1`): what became
/// of a first request, without resending it or re-interpreting the question.
/// Read-only; claims nothing and does not count toward the claim rate limit.
async fn requote(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<crate::protocol::RequoteRequest>,
) -> HttpResponse {
    let p = match app_person(&state, &headers).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    match state.engine.requote(&p, id, req).await {
        Ok(r) => Json(r).into_response(),
        Err(e) => {
            tracing::error!(error=%e, "requote persistence failure");
            failure(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "The price could not be refreshed.")
        }
    }
}
async fn request_receipt(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(request_id): Path<Uuid>,
) -> HttpResponse {
    let p = match app_person(&state, &headers).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let row = match state.engine.store.request_receipt(p.owner, request_id, p.workspace, p.connection).await {
        Ok(row) => row,
        Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "The request receipt could not be read."),
    };
    let Some(row) = row.filter(|row| row.kind.is_some()) else {
        // Never claimed, pruned, another context, or a pre-#11 row: all read
        // the same, and the same request id is safe to resend.
        return failure(StatusCode::NOT_FOUND, "request_unknown", "This request is not known. It is safe to send again.");
    };
    let response: Option<crate::protocol::Response> = row.response.and_then(|v| serde_json::from_value(v).ok());
    let interrupted = response.is_none() && (chrono::Utc::now() - row.created_at).num_seconds() >= crate::protocol::INTERRUPTED_AFTER_SECS;
    let (state_, retry_after_ms) = match (&response, interrupted) {
        (Some(_), _) => (crate::protocol::ReceiptState::Completed, None),
        (None, true) => (crate::protocol::ReceiptState::Interrupted, None),
        (None, false) => (crate::protocol::ReceiptState::Pending, Some(2000)),
    };
    Json(crate::protocol::RequestReceipt {
        protocol_version: crate::protocol::VERSION,
        request_id,
        state: state_,
        kind: row.kind,
        task_ref: response.is_some().then(|| row.task_id).flatten(),
        response_status: response.as_ref().map(|r| r.status.clone()),
        errors: response.as_ref().map(|r| r.errors.clone()).unwrap_or_default(),
        created_at: row.created_at,
        completed_at: row.completed_at,
        retry_after_ms,
    }).into_response()
}
async fn read_document(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(req): Json<crate::documents::DocumentRequest>,
) -> HttpResponse {
    use crate::documents::{self, Content, Reading};
    let Ok(_permit) = state.permits.try_acquire() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "busy", "Try again shortly.");
    };
    let p = match app_person(&state, &headers).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let reading = match req.reading() {
        Ok(reading) => reading,
        Err(message) => return failure(StatusCode::BAD_REQUEST, "invalid_document", message),
    };
    // Shares the per-account request budget; a request ID is used once.
    match state.engine.store.claim(p.owner, req.request_id, &crate::state::digest(&("document", req.request_id, &req.file_name, &req.purpose))).await {
        Ok(crate::store::Claim::New) => {}
        Ok(_) => return failure(StatusCode::CONFLICT, "request_conflict", "Use a new request ID for each document."),
        Err(e) => return failure(StatusCode::TOO_MANY_REQUESTS, "rate_limited", &e.to_string()),
    }
    let dutch = req.dutch();
    let (raw, text, label, stored) = match reading {
        Reading::Text(text) => (state.engine.parser.extract_invoice(Content::Text(&text)).await, Some(text), "text_layer", None),
        Reading::Image(image) => (state.engine.parser.extract_invoice(Content::Image(&image)).await, None, "page_image", None),
        Reading::File { mime_type, bytes } => {
            // Reading needs the original file: keep it only for this reading window.
            let stored = match documents::store(&state.engine.store.pool, p.owner, p.workspace, &req.purpose, &req.file_name, mime_type, &bytes).await {
                Ok(stored) => stored,
                Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "document_unavailable", "The file could not be prepared for reading. Try again."),
            };
            let raw = state.engine.parser.extract_invoice(Content::File { file_name: &stored.file_name, data_url: documents::data_url(&stored.mime_type, &stored.bytes) }).await;
            (raw, None, "server_file", Some(stored))
        }
    };
    finish_document_read(&state, &p, raw, text.as_deref(), label, stored, dutch).await
}
async fn read_document_again(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<crate::documents::ReadAgain>,
) -> HttpResponse {
    use crate::documents::{self, Content};
    let Ok(_permit) = state.permits.try_acquire() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "busy", "Try again shortly.");
    };
    let p = match app_person(&state, &headers).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    match state.engine.store.claim(p.owner, req.request_id, &crate::state::digest(&("document-read", id, req.request_id))).await {
        Ok(crate::store::Claim::New) => {}
        Ok(_) => return failure(StatusCode::CONFLICT, "request_conflict", "Use a new request ID for each attempt."),
        Err(e) => return failure(StatusCode::TOO_MANY_REQUESTS, "rate_limited", &e.to_string()),
    }
    let stored = match documents::load(&state.engine.store.pool, p.owner, p.workspace, id).await {
        Ok(Some(stored)) => stored,
        Ok(None) => return failure(StatusCode::NOT_FOUND, "document_expired", "This file is no longer stored. Upload it again."),
        Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "document_unavailable", "The file could not be loaded. Try again."),
    };
    let raw = state.engine.parser.extract_invoice(Content::File { file_name: &stored.file_name, data_url: documents::data_url(&stored.mime_type, &stored.bytes) }).await;
    finish_document_read(&state, &p, raw, None, "server_file", Some(stored), req.language.as_deref() != Some("en")).await
}
async fn finish_document_read(
    state: &ServerState,
    p: &Principal,
    raw: anyhow::Result<serde_json::Value>,
    text: Option<&str>,
    reading: &str,
    stored: Option<crate::documents::Stored>,
    dutch: bool,
) -> HttpResponse {
    let kept = |deleted: bool| match &stored {
        Some(s) => json!({"id":s.id,"stored_temporarily":true,"deleted":deleted,"expires_at":if deleted { serde_json::Value::Null } else { json!(s.expires_at) }}),
        None => json!({"id":null,"stored_temporarily":false,"deleted":false,"expires_at":null}),
    };
    let raw = match raw {
        Ok(raw) => raw,
        Err(_) => {
            // Keep a stored file (for at most its hour) so reading can be retried without a new upload.
            let mut body = serde_json::to_value(Response::error(Uuid::new_v4(), Status::Failed, "extraction_unavailable", "The document could not be read just now. Try again, or enter the supplier details yourself.")).unwrap_or_default();
            body["document"] = kept(false);
            return (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response();
        }
    };
    // A successful read no longer needs the original file.
    let mut deleted = false;
    if let Some(s) = &stored {
        deleted = crate::documents::delete(&state.engine.store.pool, p.owner, s.id).await.is_ok();
    }
    match crate::documents::invoice_result(&raw, text, reading, dutch, kept(deleted)) {
        Ok(result) => Json(result).into_response(),
        Err(message) => {
            let mut body = serde_json::to_value(Response::error(Uuid::new_v4(), Status::Failed, "document_unreadable", message)).unwrap_or_default();
            body["document"] = kept(deleted);
            (StatusCode::UNPROCESSABLE_ENTITY, Json(body)).into_response()
        }
    }
}
async fn app_person(state: &ServerState, headers: &HeaderMap) -> Result<Principal, HttpResponse> {
    let p = caller(state, headers).await?;
    if !p.is_person() {
        return Err(failure(StatusCode::FORBIDDEN, "app_only", "Connected chatbots read attachments themselves and send the printed identifiers in the question."));
    }
    Ok(p)
}
#[derive(Default, Deserialize)]
struct WorkflowCatalogQuery { catalog: Option<u8> }

async fn workflows(State(state): State<ServerState>, headers: HeaderMap, Query(query): Query<WorkflowCatalogQuery>) -> HttpResponse {
    let p = match app_person(&state, &headers).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let display = state.engine.store.money_display(p.owner).await;
    match state.engine.catalog.get().await {
        Ok(catalog) => Json(crate::workflows::definitions_for_client(&catalog, &display, query.catalog == Some(2))).into_response(),
        Err(_) => failure(StatusCode::SERVICE_UNAVAILABLE, "catalog_unavailable", "Workflows are temporarily unavailable."),
    }
}

async fn start_workflow(State(state): State<ServerState>, headers: HeaderMap, Path(slug): Path<String>, Json(request): Json<crate::workflow_catalog::StartRequest>) -> HttpResponse {
    let Ok(_permit) = state.permits.try_acquire() else { return failure(StatusCode::TOO_MANY_REQUESTS, "busy", "Try again shortly."); };
    let p = match caller(&state, &headers).await { Ok(p) => p, Err(r) => return r };
    match state.engine.start_workflow(&p, &slug, request).await {
        Ok(response) => Json(response).into_response(),
        Err(error) => {
            tracing::warn!(error=%error, "workflow start failed");
            failure(StatusCode::SERVICE_UNAVAILABLE, "workflow_unavailable", "The workflow could not be prepared. Try again.")
        }
    }
}
#[derive(Deserialize)]
struct RunQuery {
    workflow: Option<String>,
}
async fn workflow_runs(State(state): State<ServerState>, headers: HeaderMap, Query(query): Query<RunQuery>) -> HttpResponse {
    let p = match app_person(&state, &headers).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    if query.workflow.as_deref().is_some_and(|w| w.len() > 64) {
        return failure(StatusCode::BAD_REQUEST, "invalid_query", "Unknown workflow.");
    }
    match state.engine.workflow_runs(&p, query.workflow.as_deref()).await {
        Ok(runs) => Json(runs).into_response(),
        Err(_) => failure(StatusCode::SERVICE_UNAVAILABLE, "history_unavailable", "Workflow history could not be loaded. Try again."),
    }
}
async fn workflow_run(State(state): State<ServerState>, headers: HeaderMap, Path(id): Path<Uuid>) -> HttpResponse {
    let p = match app_person(&state, &headers).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    match state.engine.workflow_run(&p, id).await {
        Ok(Some(run)) => Json(run).into_response(),
        Ok(None) => failure(StatusCode::NOT_FOUND, "run_unavailable", "This workflow run is not available for this account."),
        Err(_) => failure(StatusCode::SERVICE_UNAVAILABLE, "history_unavailable", "The workflow run could not be loaded. Try again."),
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
    match crate::telemetry::spawn(async move {
        let _permit = permit;
        let id = req.state.state_ref;
        let (_, saved, _) = engine.store.load_scoped(&p, id).await?;
        let continuation = saved.actions.iter().any(|a| a["action_id"] == json!(req.action_id) && matches!(a["kind"].as_str(), Some("run" | "poll" | "wait" | "analyze" | "choose" | "supply" | "retry_analysis")));
        if engine.worker_active(id).await? {
            let (_, task, _) = engine.store.load_scoped(&p, id).await?;
            if task.actions.iter().any(|a| a["action_id"] == json!(req.action_id) && matches!(a["kind"].as_str(), Some("run" | "poll" | "wait" | "analyze"))) {
                return engine.view(&p, id, req.request_id, None).await;
            }
        }
        let response = engine.execute(&p, &bearer, req).await?;
        if continuation && response.state.is_some() && response.errors.is_empty() && !matches!(response.status, Status::RequiresApproval | Status::StateConflict) {
            engine.start_worker(p.clone(), bearer, id).await?;
            engine.view(&p, id, response.request_id, None).await
        } else { Ok(response) }
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
    match state.engine.store.list_tasks_scoped(p.owner, p.workspace).await {
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
        Err(error) => task_view_failure(id, error),
    }
}
fn task_view_failure(id: Uuid, error: anyhow::Error) -> HttpResponse {
    if error.downcast_ref::<crate::store::StoreAccessError>().is_some() {
        failure(
            StatusCode::NOT_FOUND,
            "task_unavailable",
            "This task is not available for this account.",
        )
    } else {
        tracing::error!(task=%id, error=%error, "task recovery unavailable");
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "task_recovery_unavailable",
            "The saved task could not be loaded just now. Try recovery again; do not create a new purchase.",
        )
    }
}
/// The App's timings for one question: logged as one line, never stored.
async fn telemetry_beacon(State(state): State<ServerState>, headers: HeaderMap, Json(beacon): Json<crate::telemetry::Beacon>) -> HttpResponse {
    let Ok(_permit) = state.permits.try_acquire() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "busy", "Try again shortly.");
    };
    if let Err(refusal) = app_person(&state, &headers).await {
        return refusal;
    }
    match beacon.record() {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(message) => failure(StatusCode::BAD_REQUEST, "invalid_beacon", message),
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
        crate::telemetry::timed(Phase::Approve, "connection", state.engine.executor.approve_connection(&p, req.state.state_ref, req.state.revision,
            req.quote_ref, &req.max_total_atomic)).await?;
        Ok::<(), anyhow::Error>(())
    }.await;
    match result {
        Ok(()) => {
            if let Err(error) = state.engine.start_worker(p.clone(), token(&headers).unwrap().to_owned(), req.state.state_ref).await {
                tracing::warn!(task=%req.state.state_ref, error=%error, "approval saved; execution requires recovery");
            }
            match state.engine.view(&p, req.state.state_ref, req.request_id, None).await {
                Ok(response) => Json(response).into_response(),
                Err(_) => failure(StatusCode::SERVICE_UNAVAILABLE, "execution_pending", "Approval was saved. Recover the task to check its result and payment status before retrying."),
            }
        },
        Err(error) => {
            tracing::warn!(error=%error,"connection approval refused");
            let (status, body) = crate::approval_feedback::response(&error);
            (status, Json(body)).into_response()
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
    match crate::telemetry::timed(Phase::Approve, "app", state
        .engine
        .executor
        .approve(&p, id, req.quote_ref, &req.max_total_atomic))
        .await
    {
        Ok(reference) => {
            // Connected tasks must execute with their connection's bearer.
            if let Ok((_, _, None)) = state.engine.store.load_scoped(&p, id).await {
                if let Err(error) = state.engine.start_worker(p.clone(), token(&headers).unwrap().to_owned(), id).await {
                    tracing::warn!(task=%id, error=%error, "approved task could not start");
                }
            }
            match state.engine.view(&p, id, Uuid::new_v4(), None).await {
                Ok(response) => Json(response).into_response(),
                Err(_) => Json(json!({"authorization_ref":reference,"task_id":id,"quote_ref":req.quote_ref})).into_response(),
            }
        }
        Err(_) => failure(
            StatusCode::FORBIDDEN,
            "approval_refused",
            "Open the current request in Apiosk and approve it there.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_access_and_dependency_failures_have_different_http_statuses() {
        let id = Uuid::new_v4();
        let inaccessible = task_view_failure(
            id,
            crate::store::StoreAccessError::TaskUnavailable.into(),
        );
        assert_eq!(inaccessible.status(), StatusCode::NOT_FOUND);

        let transient = task_view_failure(id, anyhow::anyhow!("database unavailable"));
        assert_eq!(transient.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
