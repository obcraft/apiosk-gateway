use crate::{
    server_events::task_events,
    auth::{Auth, AuthContext, AuthError, KeyRefusal, Principal},
    compat::CommerceService,
    engine::Engine,
    protocol::{DiscoverRequest, ExecuteRequest, Response, Status},
    telemetry::Phase,
};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, OriginalUri, Path, Query, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response as HttpResponse},
    routing::{any, get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;
use tower_http::cors::{Any, CorsLayer};
use uuid::Uuid;

pub(crate) mod ask_v2;
mod ask_v2_external;
mod ask_v2_guest;
mod integrations;
mod link_preview;
mod news;
mod web_research;
mod recurring;
mod company_checks;
mod monitoring;
mod triggers;
pub use triggers::dossiers::start as start_dossiers;

#[derive(Clone)]
pub struct ServerState {
    pub engine: Arc<Engine>,
    pub auth: Arc<Auth>,
    pub permits: Arc<Semaphore>,
    pub commerce: Option<CommerceService>,
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
    let commerce = state.commerce.clone();
    let mut app = Router::new()
        .route("/.well-known/crunchbase-client.json", get(|| async {
            Json(json!({
                "client_id":"https://apiosk-gateway-v2.fly.dev/.well-known/crunchbase-client.json",
                "client_name":"Apiosk Crunchbase",
                "client_uri":"https://apiosk.com",
                "redirect_uris":["http://127.0.0.1:18743/callback"],
                "grant_types":["authorization_code","refresh_token"],
                "response_types":["code"],
                "token_endpoint_auth_method":"none"
            }))
        }))
        .route(
            "/health",
            get({
                let commerce = commerce.clone();
                move || async move {
                    let executor = match commerce {
                        Some(service) if service.healthy().await => "ready",
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
        // Retired App Ask routes. Saved tasks open in Ask's task view and
        // questions go through /v2/ask-v2/*; an old tab is told to reload,
        // never forwarded to the native commerce routes by the fallback.
        .route("/v2/requests/:request_id", any(ask_moved))
        .route("/v2/discover", post(discover))
        .route("/v2/link-preview", post(link_preview::preview).layer(DefaultBodyLimit::max(8 * 1024)))
        .route("/v2/chat", any(ask_moved))
        // Removed experimental page: old clients must never reach the executor.
        .route("/v2/ask-v2/intent", any(ask_moved))
        .route("/v2/ask-v2/runs", any(ask_moved))
        .route("/v2/ask-v2/runs/*rest", any(ask_moved))
        .route("/v2/ask-v2/parse", post(ask_v2::parse).layer(DefaultBodyLimit::max(128 * 1024)))
        .route("/v2/ask-v2/news", post(news::search).layer(DefaultBodyLimit::max(8 * 1024)))
        .route("/v2/ask-v2/web-research", post(web_research::research).layer(DefaultBodyLimit::max(96 * 1024)))
        .route("/v2/ask-v2/guest/preview", post(ask_v2_guest::preview).layer(DefaultBodyLimit::max(128 * 1024)))
        .route("/v2/ask-v2/guest/approve", post(ask_v2_guest::approve).layer(DefaultBodyLimit::max(256 * 1024)))
        .route("/v2/ask-v2/search", post(ask_v2::search).layer(DefaultBodyLimit::max(256 * 1024)))
        .route("/v2/ask-v2/reply", post(ask_v2::reply).layer(DefaultBodyLimit::max(256 * 1024)))
        .route("/v2/ask-v2/endpoints/:id", get(ask_v2::endpoint))
        .route("/v2/ask-v2/prepare", post(ask_v2::prepare))
        .route("/v2/ask-v2/follow-up", post(ask_v2::follow_up).layer(DefaultBodyLimit::max(128 * 1024)))
        .route("/v2/ask-v2/conversation", post(ask_v2::conversation).layer(DefaultBodyLimit::max(128 * 1024)))
        .route("/v2/ask-v2/prefill", post(ask_v2::prefill).layer(DefaultBodyLimit::max(96 * 1024)))
        .route("/v2/ask-v2/research", post(ask_v2::research).layer(DefaultBodyLimit::max(512 * 1024)))
        .route("/v2/ask-v2/external/source", post(ask_v2_external::source))
        .route("/v2/ask-v2/external/prepare", post(ask_v2_external::prepare))
        .route("/v2/ask-v2/external/:id/authorize", post(ask_v2_external::authorize))
        .route("/v2/ask-v2/external/:id", get(ask_v2_external::status))
        .route("/v2/sources", get(sources))
        .route("/v2/capabilities/resolve", post(crate::public_api::resolve_capability))
        .route("/v2/capabilities/plan", post(crate::public_api::plan_capability))
        .route("/v2/sources/:source_id/endpoints", get(source_endpoints))
        .route("/v2/sources/:source_id/services", get(source_services))
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
        .route("/v2/tasks/:id/analysis-upgrade", post(analysis_upgrade))
        .route("/v2/tasks/:id/source-selection", post(source_selection))
        // The public API: every credential, one execution and billing path.
        .route("/v2/display-context", get(crate::public_api::display_context))
        .route("/v2/sources/:source_id", get(crate::public_api::source))
        .route("/v2/sources/:source_id/run", post(crate::public_api::run_source))
        // The shortest paid call: the key pays, the path names the source and
        // operation, and the body is the input values themselves.
        .route("/v1/execute/:source/:operation", post(crate::public_api::execute))
        .route("/v2/execute/:source/:operation", post(crate::public_api::execute))
        .route("/v2/ask", post(crate::public_api::ask))
        .route("/v2/ask/page", post(crate::public_api::ask_page))
        .route("/v2/ask/page/search", post(crate::public_api::ask_page_search).layer(DefaultBodyLimit::max(512 * 1024)))
        .route("/v2/ask/prepare", post(crate::public_api::prepare_sources))
        .route("/v2/ask/source/prepare", post(crate::public_api::prepare_source))
        .route("/v2/ask/research", post(crate::public_api::research_sources).layer(DefaultBodyLimit::max(512 * 1024)))
        .route("/v2/ask/search", post(crate::public_api::search_again).layer(DefaultBodyLimit::max(512 * 1024)))
        .route("/v2/ask/conversation", post(crate::public_api::ask_conversation).layer(DefaultBodyLimit::max(128 * 1024)))
        .route("/v2/ask/external/source", post(crate::public_api::external_source))
        .route("/v2/ask/external/prepare", post(crate::public_api::external_prepare))
        .route("/v2/ask/external/:id", get(crate::public_api::external_status))
        .route("/v2/ask/external/:id/authorize", post(crate::public_api::external_authorize))
        .route("/v2/ask/requote", post(crate::public_api::requote_sources))
        .route("/v2/ask/plan", post(crate::public_api::plan_ask))
        .route("/v2/ask/workflows", get(crate::public_api::ask_workflows))
        .route("/v2/asks/:id", get(crate::public_api::run_status))
        .route("/v2/asks/:id/run", post(crate::public_api::run_ask))
        .route("/v2/asks/:id/continue", post(crate::public_api::continue_ask))
        .route("/v2/runs/:id", get(crate::public_api::run_status))
        .route("/v2/balance", get(crate::public_api::balance))
        .route("/v2/usage", get(crate::public_api::usage))
        .route("/v2/recurring-orders", get(recurring::list))
        .route("/v2/dossiers", get(triggers::dossiers::list))
        .route("/v2/dossiers/:id", get(triggers::dossiers::read))
        .route("/v2/dossiers/:id/prepare", post(triggers::dossiers::prepare))
        .route("/v2/dossiers/:id/monitor", post(triggers::dossiers::link))
        .route("/v2/monitoring", get(monitoring::list))
        .route("/v2/monitoring/prepare", post(monitoring::prepare))
        .route("/v2/monitoring/baseline", post(monitoring::baseline))
        .route("/v2/monitoring/capabilities", get(monitoring::capabilities))
        .route("/v2/monitoring/search", get(monitoring::search))
        .route("/v2/monitoring/quote", post(monitoring::quote))
        .route("/v2/monitoring/:id", get(monitoring::read))
        .route("/v2/company-checks", post(company_checks::start))
        .route("/v2/company-checks/:id", get(company_checks::read))
        .route("/v2/company-checks/:id/collect", post(company_checks::collect))
        .route("/v2/company-checks/:id/acknowledge", post(company_checks::acknowledge))
        .route("/v2/company-checks/:id/email", post(company_checks::email))
        .route("/v2/recurring-orders/access", get(recurring::access))
        .route("/v2/recurring-orders/quote", post(recurring::quote))
        .route("/v2/recurring-orders/destinations", post(recurring::destinations))
        .route("/v2/recurring-orders/channels", post(recurring::create_channel))
        .route("/v2/recurring-orders/:id", get(recurring::read))
        .route("/v2/recurring-orders/:id/approve", post(recurring::approve))
        .route("/v2/recurring-orders/:id/runs/:run", get(recurring::run))
        .route("/v2/recurring-orders/:id/control", post(recurring::control))
        .route("/v2/recurring-orders/:id/delivery", post(recurring::update_delivery))
        .route("/v2/recurring-orders/:id/manual-trigger", post(recurring::manual_trigger))
        .route("/v2/triggers", get(triggers::list).post(triggers::create))
        .route("/v2/triggers/connect/:source", post(triggers::connect))
        .route("/v2/triggers/channels", get(triggers::channels).post(triggers::create_conversation))
        .route("/v2/triggers/members", get(triggers::members))
        .route("/v2/triggers/events", get(triggers::stream))
        .route("/v2/triggers/activity/clear", post(triggers::clear_activity))
        .route("/v2/triggers/:id/remove", post(triggers::remove))
        .route("/v2/triggers/:id/control", post(triggers::control))
        .route("/v2/triggers/:id/approver", post(triggers::update_approver))
        .route("/v2/triggers/:id/rotate", post(triggers::rotate))
        .route("/v2/triggers/:id/invite", post(triggers::invite_bot))
        .route("/v2/triggers/:id/test", post(triggers::test))
        .route("/v2/triggers/:id/tool-test", post(triggers::tool_test))
        .route("/v2/triggers/slack/interactions", post(triggers::slack_interactions))
        .route("/v2/triggers/bot", get(triggers::apiosk_bot))
        .route("/v2/triggers/bot/connect", post(triggers::apiosk_bot_connect))
        .route("/v2/triggers/:id/incoming", post(triggers::webhook))
        .route("/v2/triggers/composio", post(triggers::composio_webhook))
        .route("/v2/integrations", get(integrations::list))
        .route("/v2/integrations/:toolkit/connect", post(integrations::connect))
        .route("/v2/integrations/:toolkit/disconnect", post(integrations::disconnect))
        .route("/v2/integrations/:toolkit/accounts/:id/label", post(integrations::rename_account))
        .route("/v2/integrations/:toolkit/accounts/:id/disconnect", post(integrations::disconnect_account))
        .route("/v2/integrations/:toolkit/actions", get(integrations::toolkit_actions))
        .route("/v2/integrations/actions", get(integrations::actions))
        .route("/v2/integrations/actions/:slug/prepare", post(integrations::prepare).layer(DefaultBodyLimit::max(96 * 1024)))
        .route("/v2/integrations/actions/:slug/execute", post(integrations::execute).layer(DefaultBodyLimit::max(96 * 1024)))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .route("/v2/documents", any(ask_moved))
        .route("/v2/documents/:id/read", any(ask_moved))
        .route("/v2/telemetry", post(telemetry_beacon).layer(DefaultBodyLimit::max(crate::telemetry::BEACON_MAX_BYTES)))
        .route("/v2/workflows", get(workflows))
        .route("/v2/workflows/:slug/start", post(start_workflow))
        .route("/v2/workflows/runs", get(workflow_runs))
        .layer(cors)
        // EventSource runs inside the host's embedded card origin. The signed
        // task capability is read-only; keep bearer/approval routes app-only.
        .route("/v2/tasks/:id/events", get(task_events).layer(
            CorsLayer::new().allow_origin(Any).allow_methods([Method::GET])
        ));
    if commerce.is_some() {
        app = app.fallback(commerce_fallback);
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

async fn commerce_fallback(
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
    match state.commerce {
        Some(service) => crate::compat::forward_request(service, uri, method, headers, body).await,
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
async fn source_services(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(source_id): Path<String>,
    Query(query): Query<crate::sources::ServiceQuery>,
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
        return failure(StatusCode::BAD_REQUEST, "invalid_query", "Use a grouped source slug, up to 200 characters per filter, a limit from 1 to 50, and recommended, name-asc or name-desc sorting.");
    }
    match tokio::try_join!(state.engine.catalog.sources(), state.engine.catalog.get()) {
        Ok((sources, catalog)) => match crate::sources::service_page(&sources, &catalog, &source_id, &query) {
            Some(page) => Json(page).into_response(),
            None => failure(StatusCode::NOT_FOUND, "source_not_found", "No visible grouped source has this id."),
        },
        Err(_) => failure(StatusCode::SERVICE_UNAVAILABLE, "catalog_unavailable", "Source services are temporarily unavailable."),
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
        return failure(StatusCode::BAD_REQUEST, "invalid_query", "Use a source slug, an optional service slug, up to 200 search characters, and a limit from 1 to 100.");
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
        // A connection's automatic-approval rule may continue the new plan.
        Ok(r) => {
            if r.context_view["analyzing"] == true {
                if let Some(saved) = &r.state {
                    // start_worker independently requires either the exact
                    // paid authorization or a zero-cost saved-only analysis.
                    if let Err(error) = state.engine.start_worker(p.clone(),token(&headers).unwrap_or_default().to_owned(),saved.state_ref).await {
                        tracing::warn!(error=%error,"saved conversation answer will resume on recovery");
                    }
                }
            }
            Json(state.engine.continue_automatically(&p, token(&headers).unwrap_or_default(), r).await).into_response()
        },
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
/// `/v2/chat`, `/v2/requests/:id` and `/v2/documents*` served the App's old
/// Ask conversation. Every method answers 410 here, so nothing reaches the
/// legacy fallback and the App shows the reload message as-is.
async fn ask_moved() -> HttpResponse {
    (StatusCode::GONE, Json(json!({"errors":[{"code":"ask_moved","message":"This Ask flow moved. Reload Apiosk."}]}))).into_response()
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
    if !crate::workflows::ENABLED { return Json(json!({"workflows": [], "enabled": false})).into_response(); }
    let display = state.engine.store.money_display(p.owner).await;
    match state.engine.catalog.get().await {
        Ok(catalog) => Json(crate::workflows::definitions_for_client(&catalog, &display, query.catalog == Some(2))).into_response(),
        Err(_) => failure(StatusCode::SERVICE_UNAVAILABLE, "catalog_unavailable", "Workflows are temporarily unavailable."),
    }
}

async fn start_workflow(State(state): State<ServerState>, headers: HeaderMap, Path(slug): Path<String>, Json(request): Json<crate::workflow_catalog::StartRequest>) -> HttpResponse {
    if !crate::workflows::ENABLED { return failure(StatusCode::SERVICE_UNAVAILABLE, "workflows_disabled", crate::workflows::DISABLED_MESSAGE); }
    let Ok(_permit) = state.permits.try_acquire() else { return failure(StatusCode::TOO_MANY_REQUESTS, "busy", "Try again shortly."); };
    let p = match caller(&state, &headers).await { Ok(p) => p, Err(r) => return r };
    match state.engine.start_workflow(&p, &slug, request).await {
        Ok(response) => Json(state.engine.continue_automatically(&p, token(&headers).unwrap_or_default(), response).await).into_response(),
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
    body: Bytes,
) -> HttpResponse {
    if serde_json::from_slice::<serde_json::Value>(&body).ok().is_some_and(|value| value.get("api_id").is_some()) {
        return crate::public_api::execute_selected(State(state), headers, body).await;
    }
    let req: ExecuteRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(_) => return failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_request", "Supply api_id and input, or a saved execution action."),
    };
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
        let response = if continuation && response.state.is_some() && response.errors.is_empty() && !matches!(response.status, Status::RequiresApproval | Status::StateConflict) {
            engine.start_worker(p.clone(), bearer.clone(), id).await?;
            engine.view(&p, id, response.request_id, None).await?
        } else { response };
        // A plan that still waits for approval (a new quote after a choice,
        // or a run without one) may be covered by the connection's rule.
        Ok(engine.continue_automatically(&p, &bearer, response).await)
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

async fn analysis_upgrade(State(state): State<ServerState>, headers: HeaderMap, Path(id): Path<Uuid>, Json(req): Json<crate::engine::UpgradeRequest>) -> HttpResponse {
    let p = match app_person(&state,&headers).await { Ok(p)=>p,Err(r)=>return r };
    if req.state.state_ref != id || req.request_id.is_nil() { return failure(StatusCode::BAD_REQUEST,"invalid_request","Reload this task before changing its analysis."); }
    let Ok(_permit) = state.permits.try_acquire() else { return failure(StatusCode::TOO_MANY_REQUESTS,"busy","Try again shortly."); };
    match state.engine.analysis_upgrade(&p,req).await {
        Ok(view)=>Json(view).into_response(),
        Err(_)=>failure(StatusCode::CONFLICT,"analysis_upgrade_unavailable","Reload the saved task before changing its analysis. Nothing new was purchased."),
    }
}

async fn source_selection(State(state):State<ServerState>,headers:HeaderMap,Path(id):Path<Uuid>,Json(req):Json<crate::engine::SourceSelectionRequest>)->HttpResponse {
    let p=match app_person(&state,&headers).await {Ok(p)=>p,Err(response)=>return response};
    if req.state.state_ref!=id || req.request_id.is_nil() {return failure(StatusCode::BAD_REQUEST,"invalid_request","Reload this task before changing its sources.");}
    let Ok(_permit)=state.permits.try_acquire() else {return failure(StatusCode::TOO_MANY_REQUESTS,"busy","Try again shortly.");};
    match state.engine.select_sources(&p,req).await {
        Ok(response)=>Json(response).into_response(),
        Err(_)=>failure(StatusCode::CONFLICT,"source_selection_unavailable","Reload this unapproved plan before changing its sources. Nothing was purchased."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn display_context_is_read_only_and_requires_public_api_authentication() {
        use axum::body::Body;
        use tower::ServiceExt;

        let pool = sqlx::postgres::PgPoolOptions::new().connect_lazy("postgresql://unused@127.0.0.1:1/unused").unwrap();
        let http = reqwest::Client::new();
        let store = crate::store::Store { pool: pool.clone() };
        let engine = Arc::new(Engine {
            store: store.clone(),
            catalog: crate::catalog::CatalogCache::new(pool.clone()),
            parser: crate::parser::Parser::new(http.clone(), "http://127.0.0.1:1/chat".into(), "unused".into(), "unused".into()),
            signer: crate::state::Signer::new(vec![7; 32]).unwrap(),
            executor: crate::execution::Executor { store, http: http.clone(), supabase_url: "http://127.0.0.1:1".into(), enabled: false },
            app_url: "http://localhost:5173".into(),
        });
        let app = router(ServerState {
            engine,
            auth: Arc::new(Auth { pool, http, supabase_url: "http://127.0.0.1:1".into(), publishable_key: "unused".into() }),
            permits: Arc::new(Semaphore::new(4)),
            commerce: None,
        }, "http://localhost:5173").unwrap();

        let response = app.clone().oneshot(axum::http::Request::get("/v2/display-context").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let bytes = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["code"], "invalid_api_key");
        assert!(body.get("money_display").is_none(), "unauthenticated callers cannot read account preferences");

        let response = app.oneshot(axum::http::Request::post("/v2/display-context").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

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

    /// The App's retired Ask routes answer 410 for every method, and never
    /// reach the native commerce routes that the fallback forwards other paths to.
    #[tokio::test]
    async fn retired_ask_routes_answer_gone_without_the_commerce_fallback() {
        use axum::body::Body;
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        use tower::ServiceExt;

        let forwarded = Arc::new(AtomicUsize::new(0));
        let native = Router::new().route("/health", get(|| async { StatusCode::OK })).fallback({
            let forwarded = forwarded.clone();
            move || async move { forwarded.fetch_add(1, SeqCst); (StatusCode::OK, "native commerce routes") }
        });
        // Nothing here may touch the database: the pool never connects.
        let pool = sqlx::postgres::PgPoolOptions::new().connect_lazy("postgresql://unused@127.0.0.1:1/unused").unwrap();
        let http = reqwest::Client::new();
        let store = crate::store::Store { pool: pool.clone() };
        let engine = Arc::new(Engine {
            store: store.clone(),
            catalog: crate::catalog::CatalogCache::new(pool.clone()),
            parser: crate::parser::Parser::new(http.clone(), "http://127.0.0.1:1/chat".into(), "unused".into(), "unused".into()),
            signer: crate::state::Signer::new(vec![7; 32]).unwrap(),
            executor: crate::execution::Executor { store, http: http.clone(), supabase_url: "http://127.0.0.1:1".into(), enabled: false },
            app_url: "http://localhost:5173".into(),
        });
        let app = router(ServerState {
            engine,
            auth: Arc::new(Auth { pool, http: http.clone(), supabase_url: "http://127.0.0.1:1".into(), publishable_key: "unused".into() }),
            permits: Arc::new(Semaphore::new(4)),
            commerce: Some(CommerceService::new(native)),
        }, "http://localhost:5173").unwrap();

        let request = Uuid::new_v4();
        for (method, path) in [
            (Method::POST, "/v2/chat".to_owned()),
            (Method::GET, "/v2/chat".to_owned()),
            (Method::POST, "/v2/ask-v2/intent".to_owned()),
            (Method::POST, "/v2/ask-v2/runs".to_owned()),
            (Method::GET, format!("/v2/ask-v2/runs/{request}")),
            (Method::POST, format!("/v2/ask-v2/runs/{request}/approve")),
            (Method::GET, format!("/v2/ask-v2/runs/{request}/events")),
            (Method::GET, format!("/v2/requests/{request}")),
            (Method::POST, "/v2/documents".to_owned()),
            (Method::POST, format!("/v2/documents/{request}/read")),
        ] {
            let body = if method == Method::POST { Body::from(r#"{"request_id":"00000000-0000-4000-8000-000000000001","question":"Hallo","state":null}"#) } else { Body::empty() };
            let response = app.clone().oneshot(axum::http::Request::builder().method(method.clone()).uri(&path)
                .header("authorization", "Bearer apk_would-be-a-key").header("content-type", "application/json").body(body).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::GONE, "{method} {path}");
            let bytes = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body, json!({"errors":[{"code":"ask_moved","message":"This Ask flow moved. Reload Apiosk."}]}), "{method} {path}");
        }
        assert_eq!(forwarded.load(SeqCst), 0, "no retired Ask route reaches the native commerce routes");

        let health = app.clone().oneshot(axum::http::Request::get("/health").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(health.status(), StatusCode::OK);
        let health: serde_json::Value = serde_json::from_slice(&axum::body::to_bytes(health.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(health["payment_executor"], "ready");
        assert_eq!(health["service"], "apiosk-gateway-v2");

        // The native transport keeps the v2 workspace boundary in front of
        // account-only commerce handlers; it does not create an auth shortcut.
        let response = app.clone().oneshot(axum::http::Request::get("/v1/commerce-probe")
            .header("x-apiosk-workspace", Uuid::new_v4().to_string()).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(forwarded.load(SeqCst), 0);

        // The fallback itself is live in this router: an unknown path is forwarded.
        let response = app.oneshot(axum::http::Request::get("/v1/commerce-probe").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(forwarded.load(SeqCst), 1);
    }
}
