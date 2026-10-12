//! Capability discovery and explicit endpoint preparation. Parsing and search
//! never create tasks. Preparation saves a quote; existing task approval owns
//! every paid action.
use super::{app_person, caller, failure, ServerState};
use crate::{capability_intake::{self, Match, ParsedRequest}, parser::conversation_reply};
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PrepareRequest {
    request_id: Uuid,
    endpoint_id: Uuid,
    capability: Option<String>,
    #[serde(default)]
    input: BTreeMap<String, Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ResearchRequest {
    request_id: Uuid,
    question: String,
    parsed_request: ParsedRequest,
    #[serde(default)]
    search_id: Option<String>,
    #[serde(default)]
    discovery_id: Option<String>,
    #[serde(default)]
    selection: Option<crate::ask_v2::source_selection::Selection>,
}

impl ResearchRequest {
    fn normalized(&self) -> anyhow::Result<ParsedRequest> {
        anyhow::ensure!(!self.request_id.is_nil() && !self.question.trim().is_empty()
            && self.question.encode_utf16().count() <= 16_000 && valid_parsed(&self.parsed_request), "invalid research request");
        let mut parsed = capability_intake::normalize(&serde_json::to_string(&self.parsed_request)?, &self.question, &self.parsed_request.meta.model)?;
        anyhow::ensure!(parsed.meta.text_sha256 == self.parsed_request.meta.text_sha256,
            "the displayed parse belongs to a different question");
        anyhow::ensure!(parsed.capabilities.len() == self.parsed_request.capabilities.len(), "research must preserve every displayed requirement");
        anyhow::ensure!(serde_json::to_value(&parsed.subjects)? == serde_json::to_value(&self.parsed_request.subjects)?,
            "research must preserve the displayed literal subjects");
        // Stable retries hash the same displayed extraction, not a fresh clock.
        parsed.meta = self.parsed_request.meta.clone();
        Ok(parsed)
    }
}

/// The App plans from the extraction it already displayed. The gateway
/// revalidates literals and the catalog version against its signed search,
/// without redefining the request or replacing displayed candidate sources.
pub(super) async fn research(
    State(state): State<ServerState>, headers: HeaderMap, Json(request): Json<ResearchRequest>,
) -> Response {
    let person = match app_person(&state, &headers).await { Ok(p) => p, Err(r) => return r };
    match request.normalized() {
        Ok(_) => {},
        Err(_) => return failure(StatusCode::BAD_REQUEST, "invalid_research_request", "The saved search does not match this question. Search the question again before planning."),
    }
    let refresh = || failure(StatusCode::CONFLICT, "search_refresh_required", "Refresh the source search before planning. The question and existing tasks are kept.");
    let Some(search_id) = request.search_id.as_deref() else { return refresh(); };
    let snapshot = match crate::search_snapshot::Snapshot::open(&state.engine.signer, &person, &request.parsed_request, search_id) {
        Ok(snapshot) => snapshot, Err(_) => return refresh(),
    };
    let snapshot = if let Some(discovery_id) = &request.discovery_id {
        let discovery = match crate::search_snapshot::Snapshot::open(&state.engine.signer, &person, &request.parsed_request, discovery_id) {
            Ok(discovery) => discovery, Err(_) => return refresh(),
        };
        match snapshot.with_discovery(discovery) { Ok(snapshot) => snapshot, Err(_) => return refresh() }
    } else { snapshot };
    // A retry with different external choices is a different discovery input.
    let combined_id = if request.selection.is_some() {
        Some(crate::state::digest(&(search_id,&request.discovery_id,&request.selection)))
    } else {request.discovery_id.as_ref().map(|discovery|crate::state::digest(&(search_id,discovery)))};
    let search_id = combined_id.as_deref().unwrap_or(search_id);
    let Ok(_permit) = state.permits.try_acquire() else { return failure(StatusCode::TOO_MANY_REQUESTS, "busy", "Try again shortly."); };
    let discover = crate::protocol::DiscoverRequest { request_id:request.request_id,question:request.question,state:None,context_delta:Default::default() };
    match state.engine.existing_research(&person, &discover, &request.parsed_request, search_id).await {
        Ok(Some(response)) => return Json(response).into_response(),
        Ok(None) => {},
        Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "research_unavailable", "The existing request could not be recovered. Try the same request again."),
    }
    let catalog = match state.engine.catalog.get().await {
        Ok(catalog) => catalog,
        Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "catalog_unavailable", "The source catalog is temporarily unavailable. Try the same request again."),
    };
    if !snapshot.current(&catalog.version) { return refresh(); }
    let options=snapshot.source_options().to_vec();
    let intake = snapshot.intake(request.parsed_request);
    let context=crate::ask_v2::source_selection::Context {question:discover.question.clone(),original_intake:intake.clone(),
        search_id:request.search_id.clone().unwrap(),discovery_id:request.discovery_id,options,selection:request.selection.unwrap_or_default()};
    let intake=match context.expanded(&catalog) {
        Ok(intake)=>intake,
        Err(error)=>return failure(StatusCode::BAD_REQUEST,"invalid_source_selection",&error.to_string()),
    };
    match state.engine.discover_searched_configured(&person, discover, intake, catalog, search_id,Some(context)).await {
        Ok(response) => Json(response).into_response(),
        Err(_) => failure(StatusCode::SERVICE_UNAVAILABLE, "research_unavailable", "The complete request could not be prepared. Your question is kept; try again."),
    }
}

impl PrepareRequest {
    fn valid(&self) -> bool {
        !self.request_id.is_nil() && !self.endpoint_id.is_nil()
            && self.capability.as_deref().is_none_or(valid_operation)
            && self.input.len() <= 64
            && self.input.keys().all(|field| !field.is_empty() && field.len() <= 200)
    }
}

fn valid_operation(value: &str) -> bool {
    !value.is_empty() && value.len() <= 200
        && value.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_'))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EndpointQuery { capability: Option<String> }

/// Inputs and prices come from the current registered endpoint or mapping, never from a
/// directory card or the model's parsed metadata. This does not create a task.
/// Connected chatbots read it too: they fill in the inputs they are shown.
pub(super) async fn endpoint(
    State(state): State<ServerState>, headers: HeaderMap, Path(id): Path<Uuid>, Query(query): Query<EndpointQuery>,
) -> Response {
    if let Err(response) = caller(&state, &headers).await { return response; }
    if query.capability.as_deref().is_some_and(|value| !valid_operation(value)) {
        return failure(StatusCode::BAD_REQUEST, "invalid_capability", "Choose the source's published capability.");
    }
    let Ok(_permit) = state.permits.try_acquire() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "busy", "Try again shortly.");
    };
    let catalog = match state.engine.catalog.get().await {
        Ok(catalog) => catalog,
        Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "catalog_unavailable", "Source inputs are temporarily unavailable. Your selected source is kept; try again."),
    };
    let service = match crate::engine::Engine::endpoint_service(&catalog, id, query.capability.as_deref()) {
        Ok(service) => service,
        Err((code, message)) => return failure(if code == "ambiguous_endpoint" { StatusCode::CONFLICT } else { StatusCode::NOT_FOUND }, code, message),
    };
    let sources = tokio::time::timeout(Duration::from_secs(1), state.engine.catalog.sources()).await.ok().and_then(Result::ok);
    let source = sources.as_ref().and_then(|sources| sources.iter().find(|source| source.slug == service.api_slug));
    Json(endpoint_descriptor(&catalog, service, source)).into_response()
}

pub(crate) fn endpoint_descriptor(catalog: &crate::catalog::Catalog, service: &crate::catalog::Service, source: Option<&crate::sources::Source>) -> Value {
    let inputs: Vec<_> = service.contract.requires.iter().map(|required| {
        let mut schema = catalog.vocabulary.get(&required.fact_type).map(|fact| fact.value_schema.clone()).unwrap_or_else(|| json!({}));
        if required.optional {
            if let Some(default) = service.contract.input_map.get(&required.fact_type).and_then(|binding| binding.default.as_ref()) {
                if let Some(schema) = schema.as_object_mut() {
                    schema.insert("default".into(), default.clone());
                }
            }
        }
        json!({"field":required.fact_type,"required":!required.optional,"schema":schema})
    }).collect();
    let lookup = crate::engine::Engine::endpoint_lookup(catalog, service);
    json!({
        "api_id":service.endpoint_id,"endpoint_id":service.endpoint_id,"capability":service.contract.capability_slug,"name":service.name,"description":service.description,
        "source":{"slug":service.api_slug,"name":source.map(|s| s.name.as_str()).unwrap_or(&service.api_slug),
            "logo_url":source.and_then(|s| s.logo_url.as_deref().or(s.provider_logo_url.as_deref())),"url":service.source_url,"usage_notice":service.usage_notice_view()},
        "inputs":inputs,
        "outputs":service.contract.output_map.produces.keys().map(|field| json!({"field":field,"schema":catalog.vocabulary.get(field).map(|fact| &fact.value_schema)})).collect::<Vec<_>>(),
        "lookup":lookup.map(|(field, resolves)| json!({"field":field,"resolves":resolves,
            "schema":catalog.vocabulary.get(field).map(|fact| &fact.value_schema)})),
        "price":{"currency":"USDC","provider_atomic":service.list_atomic.to_string(),"buyer_atomic":service.total().to_string()}
    })
}

pub(super) async fn prepare(
    State(state): State<ServerState>, headers: HeaderMap, Json(request): Json<PrepareRequest>,
) -> Response {
    // A connected chatbot gets the same quote as the App; paying still needs
    // its approval within the connection's spending limits — a click in the
    // card, or the connection's own automatic-approval rule when it covers it.
    let person = match caller(&state, &headers).await { Ok(p) => p, Err(response) => return response };
    let bearer = super::token(&headers).unwrap_or_default().to_owned();
    if !request.valid() {
        return failure(StatusCode::BAD_REQUEST, "invalid_endpoint_input", "Choose an endpoint and review its input fields. Nothing was charged.");
    }
    let Ok(permit) = state.permits.clone().try_acquire_owned() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "busy", "The service is busy. Your selected source and inputs are kept; try again shortly.");
    };
    let engine = state.engine.clone();
    // Finish saving the quote/receipt if the browser disconnects. Repeating
    // this request ID can recover it; only a connection's own standing rule
    // (never this request) can approve the purchase.
    match crate::telemetry::spawn(async move {
        let _permit = permit;
        let response = engine.prepare_endpoint(&person, request.request_id, request.endpoint_id, request.capability.as_deref(), &request.input).await?;
        Ok::<_, anyhow::Error>(engine.continue_automatically(&person, &bearer, response).await)
    }).await {
        Ok(Ok(response)) => Json(response).into_response(),
        _ => failure(StatusCode::SERVICE_UNAVAILABLE, "prepare_unavailable", "The quote could not be confirmed. Your inputs are kept; retry this request to recover it."),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FollowUpRequest {
    task_ref: Uuid,
    question: String,
}

/// Read-only interpretation of a completed endpoint request. Ownership is
/// resolved before loading context or calling the model. No quote is saved.
pub(super) async fn follow_up(
    State(state): State<ServerState>, headers: HeaderMap, Json(request): Json<FollowUpRequest>,
) -> Response {
    let person = match app_person(&state, &headers).await { Ok(person) => person, Err(response) => return response };
    if request.task_ref.is_nil() || request.question.trim().is_empty() || request.question.encode_utf16().count() > 16_000 {
        return failure(StatusCode::BAD_REQUEST, "invalid_follow_up", "Enter a follow-up of up to 16,000 characters.");
    }
    let Ok(_permit) = state.permits.try_acquire() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "busy", "Try again shortly. Your selected source is kept.");
    };
    let (revision, task, _) = match state.engine.store.load_scoped(&person, request.task_ref).await {
        Ok(saved) => saved,
        Err(error) if matches!(error.downcast_ref::<crate::store::StoreAccessError>(), Some(crate::store::StoreAccessError::TaskUnavailable)) =>
            return failure(StatusCode::NOT_FOUND, "task_unavailable", "This saved request is unavailable."),
        Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "follow_up_unavailable", "The saved request could not be read. Try again."),
    };
    match state.engine.worker_active(request.task_ref).await {
        Ok(false) => {},
        Ok(true) => return Json(crate::ask_v2::follow_up::needs_input("Wait for this request to finish before changing its inputs.")).into_response(),
        Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "follow_up_unavailable", "The saved request could not be checked. Try again."),
    }
    let catalog = match state.engine.catalog.get().await {
        Ok(catalog) => catalog,
        Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "catalog_unavailable", "The selected source's inputs could not be checked. Try again."),
    };
    let mut context = match crate::ask_v2::follow_up::saved_context(&state.engine, &person, request.task_ref, revision, &task, &catalog).await {
        Ok(Some(context)) => context,
        Ok(None) => return Json(json!({"kind":"new_request"})).into_response(),
        Err(_) => return Json(crate::ask_v2::follow_up::needs_input("This saved request cannot be changed right now. Refresh it and review the source inputs.")).into_response(),
    };
    // Use the same optional source branding as the endpoint descriptor route.
    if let Ok(Ok(sources)) = tokio::time::timeout(Duration::from_secs(1), state.engine.catalog.sources()).await {
        if let Some(source) = sources.iter().find(|source| context.descriptor["source"]["slug"] == source.slug) {
            context.descriptor["source"]["name"] = json!(source.name);
            context.descriptor["source"]["logo_url"] = json!(source.logo_url.as_deref().or(source.provider_logo_url.as_deref()));
        }
    }
    let Some(client) = state.engine.parser.ask_v2() else {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "follow_up_unavailable", "The follow-up could not be interpreted. Your selected source is kept; try again.");
    };
    if !client.admit(&person.owner.to_string()) {
        return failure(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "Please wait a minute before trying another follow-up.");
    }
    match client.endpoint_follow_up(request.question.trim(), &context).await {
        Ok(response) => Json(response).into_response(),
        Err(_) => failure(StatusCode::SERVICE_UNAVAILABLE, "follow_up_unavailable", "The follow-up could not be mapped safely. Your selected source is kept; clarify the input you want to change."),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ParseRequest {
    request_id: Uuid,
    question: String,
    #[serde(default)]
    known_slugs: Vec<String>,
    correction: Option<String>,
}

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 160
        && slug.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'.')
}

impl ParseRequest {
    fn valid(&self) -> bool {
        !self.request_id.is_nil()
            && !self.question.trim().is_empty()
            && self.question.encode_utf16().count() <= 16_000
            && self.known_slugs.len() <= 100
            && self.known_slugs.iter().all(|slug| valid_slug(slug))
            && self.correction.as_ref().is_none_or(|c| c.len() <= 4_000)
    }
}

pub(super) async fn parse(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<ParseRequest>,
) -> Response {
    let person = match app_person(&state, &headers).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    if !request.valid() {
        return failure(
            StatusCode::BAD_REQUEST,
            "invalid_question",
            "Enter a question of up to 16,000 characters.",
        );
    }
    let Some(client) = state.engine.parser.ask_v2() else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "parser_unavailable",
            "Capability parsing is temporarily unavailable. Your question is kept; try again.",
        );
    };
    let Ok(_permit) = state.permits.try_acquire() else {
        return failure(
            StatusCode::TOO_MANY_REQUESTS,
            "busy",
            "The service is busy. Your question is kept; try again shortly.",
        );
    };
    if !client.admit(&person.owner.to_string()) {
        return failure(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Please wait a minute before trying another question.",
        );
    }
    match client
        .interpret(request.question.trim(), &request.known_slugs)
        .await
    {
        Ok(output) => {
            tracing::info!(request_id=%request.request_id, model=%output["model"], parse_ms=%output["timing"]["parse_ms"], "Ask v2 capabilities parsed");
            Json(output).into_response()
        }
        Err(error) => {
            // Stable client errors contain no prompt, key or provider body.
            tracing::warn!(request_id=%request.request_id, error=%error, "Ask v2 parse unavailable");
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "parser_unavailable",
                "The model could not finish parsing. Your question is kept; try again.",
            )
        }
    }
}

/// What the Ask page sends when a question is not yet a clear data question:
/// the latest message, the conversation before it, why nothing was found, and
/// the page's own reply, which it shows whenever this one does not come.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReplyRequest {
    question: String,
    #[serde(default)]
    context: Vec<conversation_reply::Turn>,
    reason: conversation_reply::Reason,
    language: Option<String>,
    draft: Option<String>,
}

impl ReplyRequest {
    fn valid(&self) -> bool {
        let text = |value: &str, limit: usize| value.encode_utf16().count() <= limit;
        !self.question.trim().is_empty()
            && text(&self.question, 16_000)
            && self.context.len() <= 12
            && self.context.iter().all(|turn| text(&turn.text, 16_000))
            && self.language.as_deref().is_none_or(conversation_reply::valid_language)
            && self.draft.as_deref().is_none_or(|draft| text(draft, 2_000))
    }
}

/// The written reply to small talk, an unclear request or a search that found
/// nothing. It creates no task and calls no source; it only writes text.
pub(super) async fn reply(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<ReplyRequest>,
) -> Response {
    // A connected chatbot may ask for the same reply as the App.
    let person = match caller(&state, &headers).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    if !request.valid() {
        return failure(StatusCode::BAD_REQUEST, "invalid_reply_request", "Send the latest message and why nothing was found.");
    }
    let (Some(limits), Some(client)) = (state.engine.parser.ask_v2(), state.engine.parser.conversation_replies()) else {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "reply_unavailable", "A written reply is temporarily unavailable.");
    };
    let Ok(_permit) = state.permits.try_acquire() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "busy", "The service is busy. Try again shortly.");
    };
    if !limits.admit_reply(&person.owner.to_string()) {
        return failure(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "Please wait a minute before trying another question.");
    }
    // The catalog only names what the reply may mention; without it the reply
    // names no source at all.
    let catalog = match tokio::time::timeout(Duration::from_secs(1), state.engine.catalog.get()).await {
        Ok(Ok(catalog)) => conversation_reply::catalog_summary(&catalog, None),
        _ => json!({}),
    };
    let asked = conversation_reply::Request {
        question: request.question.trim(), earlier: request.context, reason: request.reason,
        language: request.language.as_deref(), draft: request.draft.as_deref(),
    };
    match client.reply(&asked, catalog).await {
        Ok(written) => Json(json!({"reply":written.text,"model":written.model})).into_response(),
        Err(_) => failure(StatusCode::SERVICE_UNAVAILABLE, "reply_unavailable", "A written reply is temporarily unavailable."),
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Origin {
    Apiosk,
    Coinbase,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SearchRequest {
    parsed_request: ParsedRequest,
    origin: Origin,
}

// This is untrusted client metadata, used only for bounded directory searches.
// It can never select executable operations, create a quote or authorize a call.
fn valid_parsed(parsed: &ParsedRequest) -> bool {
    let short = |s: &str| s.len() <= 512;
    let concepts = |v: &[String]| v.len() <= 32 && v.iter().all(|s| short(s));
    let mut slugs = BTreeSet::new();
    parsed.language.len() <= 16
        && parsed.subjects.len() <= 32
        && parsed.subjects.iter().all(|s| {
            short(&s.id)
                && short(&s.kind)
                && s.label.len() <= 4000
                && s.identifiers.len() <= 32
                && s.identifiers.iter().all(|(k, v)| short(k) && short(v))
        })
        && !parsed.capabilities.is_empty()
        && parsed.capabilities.len() <= 16
        && parsed.capabilities.iter().all(|c| {
            valid_slug(&c.slug)
                && slugs.insert((c.slug.as_str(), c.subject_id.as_deref()))
                && short(&c.name)
                && c.description.len() <= 16_000
                && short(&c.domain)
                && concepts(&c.inputs)
                && concepts(&c.outputs)
                && concepts(&c.missing)
                && c.subject_id.as_deref().is_none_or(short)
                && c.source_hint.as_deref().is_none_or(short)
                && c.confidence.is_finite()
                && (0.0..=1.0).contains(&c.confidence)
        })
        && short(&parsed.deliverable.format)
        && concepts(&parsed.deliverable.operations)
        && short(&parsed.meta.model)
        && short(&parsed.meta.parsed_at)
        && short(&parsed.meta.text_sha256)
}

pub(super) async fn search(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<SearchRequest>,
) -> Response {
    // A connected chatbot sends the capability object it filled in itself.
    let person = match caller(&state, &headers).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    if !valid_parsed(&request.parsed_request) {
        return failure(
            StatusCode::BAD_REQUEST,
            "invalid_capabilities",
            "Parse your question again before searching.",
        );
    }
    let Some(client) = state.engine.parser.ask_v2() else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "search_unavailable",
            "Source search is temporarily unavailable. Your capabilities are kept; try again.",
        );
    };
    let Ok(_permit) = state.permits.try_acquire() else {
        return failure(
            StatusCode::TOO_MANY_REQUESTS,
            "busy",
            "The service is busy. Your capabilities are kept; try again shortly.",
        );
    };
    if !client.admit_search(&person.owner.to_string()) {
        return failure(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Please wait a minute before searching again.",
        );
    }
    match search_catalog(&state, &request.parsed_request, request.origin).await {
        Ok(mut value) => {
            if person.is_person() {
                match crate::search_snapshot::Snapshot::issue(&state.engine.signer, &person, &request.parsed_request, &value) {
                    Ok(token) => value[match request.origin { Origin::Apiosk => "search_id", Origin::Coinbase => "discovery_id" }] = json!(token),
                    Err(_) => return failure(StatusCode::UNPROCESSABLE_ENTITY, "search_too_large", "This search contains too much source detail. Narrow the request and search again."),
                }
            }
            Json(value).into_response()
        },
        Err(response) => response,
    }
}

pub(crate) async fn search_catalog(state: &ServerState, parsed_request: &ParsedRequest, origin: Origin) -> Result<Value, Response> {
    let started = Instant::now();
    // The interpretation contract already supplies English capability names
    // and descriptions. Search consumes that JSON without another model pass.
    // Catalog translations prepared in the background remain available.
    let english = state.engine.catalog.english().memory();
    let (mut matches, count, version, source_options) = match origin {
        Origin::Apiosk => {
            let catalog = match state.engine.catalog.get().await {
                Ok(c) => c,
                Err(_) => {
                    return Err(failure(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "catalog_unavailable",
                        "Apiosk sources are temporarily unavailable. Retry the source search.",
                    ))
                }
            };
            let matches = crate::ask_v2::search::search(parsed_request, &catalog);
            let source_options = crate::ask_v2::source_options::options(parsed_request, &matches, &catalog);
            let mut matches = serde_json::to_value(matches).unwrap_or_else(|_| json!([]));
            // Branding is cached separately from contracts. A slow directory
            // refresh must not hold up capability matches indefinitely.
            let sources = tokio::time::timeout(Duration::from_secs(1), state.engine.catalog.sources()).await.ok().and_then(Result::ok);
            if let Some(sources) = &sources {
                brand_matches(&mut matches, sources);
            }
            describe_endpoints(&mut matches, &catalog, sources.as_ref().map(|sources| sources.as_slice()));
            bind_inputs(&mut matches, parsed_request);
            (
                matches,
                capability_intake::source_count(&catalog),
                catalog.version.clone(),
                source_options,
            )
        }
        Origin::Coinbase => {
            // Bazaar is searched in English; the query is also what a
            // purchase re-resolves the listing with.
            let mut matches: Vec<_> =
                capability_intake::queries(parsed_request, &crate::state::Task::default())
                    .into_iter()
                    .map(|(slug, query)| Match {
                        slug,
                        query,
                        operations: BTreeSet::new(),
                        apiosk: vec![],
                        coinbase: vec![],
                        coinbase_status: "pending".into(),
                    })
                    .collect();
            capability_intake::add_coinbase(
                state.engine.parser.discovery_http(),
                &mut matches,
                state.engine.parser.bazaar_search_url(),
            )
            .await;
            crate::ask_v2::search::filter_coinbase_in_english(parsed_request, parsed_request, &mut matches,
                &|text| english.english(text));
            // Compare buyer totals in the table. This is still a discovery
            // estimate; the selected offer is independently resolved at quote.
            for candidate in matches.iter_mut().flat_map(|found| &mut found.coinbase) {
                candidate.price = candidate.price.as_deref().and_then(buyer_discovery_price);
            }
            let mut matches = serde_json::to_value(matches).unwrap_or_else(|_| json!([]));
            describe_external(&mut matches, state).await;
            bind_inputs(&mut matches, parsed_request);
            (matches, 0, String::new(), Vec::new())
        }
    };
    let mut occurrences = BTreeMap::<String, usize>::new();
    for found in matches.as_array_mut().into_iter().flatten() {
        let Some(slug) = found["slug"].as_str().map(str::to_owned) else { continue };
        let occurrence = occurrences.entry(slug.clone()).or_default();
        if parsed_request.capabilities.iter().filter(|cap| cap.slug == slug).nth(*occurrence)
            .is_some_and(capability_intake::answer_instruction) {
            found["answer_only"] = json!(true);
        }
        *occurrence += 1;
    }
    let search_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        origin = if count > 0 { "apiosk" } else { "coinbase" },
        search_ms,
        sources_searched = count,
        "Ask v2 sources searched"
    );
    Ok(json!({"matches":matches,"source_options":source_options,"apiosk_sources_searched":count,"catalog_version":version,"timing":{"search_ms":search_ms}}))
}

fn buyer_discovery_price(price: &str) -> Option<String> {
    let (whole, fraction) = price.strip_suffix(" USD")?.split_once('.')?;
    if fraction.len() != 6 || !whole.bytes().chain(fraction.bytes()).all(|b| b.is_ascii_digit()) { return None; }
    let provider = whole.parse::<i64>().ok()?.checked_mul(1_000_000)?.checked_add(fraction.parse::<i64>().ok()?)?;
    if !(0..1_000_000_000_000).contains(&provider) { return None; }
    let total = catalog_core::fees::quote(provider.into(), catalog_core::fees::ProviderClass::Federated).buyer_total_atomic;
    Some(format!("{}.{:06} USD", total / 1_000_000, total % 1_000_000))
}

/// Every purchasable candidate carries its declared inputs and price, so
/// a caller can fill in the input object that prepare accepts.
fn describe_endpoints(matches: &mut Value, catalog: &crate::catalog::Catalog, sources: Option<&[crate::sources::Source]>) {
    for found in matches.as_array_mut().into_iter().flatten() {
        for candidate in found["apiosk"].as_array_mut().into_iter().flatten() {
            if !matches!(candidate["availability"].as_str(), Some("supported" | "unverified")) { continue; }
            let Some(id) = candidate["endpoint_id"].as_str().and_then(|id| Uuid::parse_str(id).ok()) else { continue };
            let Ok(service) = crate::engine::Engine::endpoint_service(catalog, id, candidate["capability"].as_str()) else { continue };
            let source = sources.and_then(|sources| sources.iter().find(|source| source.slug == service.api_slug));
            candidate["api_id"] = json!(id);
            candidate["endpoint"] = endpoint_descriptor(catalog, service, source);
        }
    }
}

/// Reuse literal values under the same rules as the App's input review. A
/// schema or requested output is never confused with a returned source value.
fn bind_inputs(matches: &mut Value, parsed: &ParsedRequest) {
    let mut match_counts = BTreeMap::<String, usize>::new();
    for found in matches.as_array().into_iter().flatten() {
        *match_counts.entry(found["slug"].as_str().unwrap_or_default().to_owned()).or_default() += 1;
    }
    let mut occurrences = BTreeMap::<String, usize>::new();
    for found in matches.as_array_mut().into_iter().flatten() {
        // Search preserves capability order, including the same capability on
        // different subjects. Never let one company's identifiers fill another.
        let slug = found["slug"].as_str().unwrap_or_default().to_owned();
        let occurrence = occurrences.entry(slug.clone()).or_default();
        let requested_count=parsed.capabilities.iter().filter(|cap|cap.slug==slug).count();
        // Older directory query maps collapse duplicate slugs. There is then
        // no unambiguous subject association; keep the input review empty.
        let capability = (match_counts.get(&slug).copied()==Some(requested_count))
            .then(||parsed.capabilities.iter().filter(|cap| cap.slug == slug).nth(*occurrence)).flatten();
        *occurrence += 1;
        let subject = match capability.and_then(|cap| cap.subject_id.as_deref()) {
            Some(id) => parsed.subjects.iter().find(|subject| subject.id == id),
            None if capability.is_none() && slug.is_empty() && parsed.subjects.len() == 1 => parsed.subjects.first(),
            None => None,
        };
        found["subject_id"] = json!(subject.map(|subject| &subject.id));
        for candidate in found.as_object_mut().into_iter().flat_map(|found| found.iter_mut())
            .filter(|(origin, _)| matches!(origin.as_str(), "apiosk" | "coinbase"))
            .flat_map(|(_, candidates)| candidates.as_array_mut().into_iter().flatten()) {
            candidate["subject_id"] = json!(subject.map(|subject| &subject.id));
            let endpoint = &mut candidate["endpoint"];
            if !endpoint.is_object() { continue; }
            let mut values = serde_json::Map::new();
            for field in endpoint["inputs"].as_array().into_iter().flatten() {
                let Some(name) = field["field"].as_str() else { continue; };
                if let Some(default) = field["schema"].get("default") { values.insert(name.into(), default.clone()); }
                if let Some(subject) = subject {
                    if let Some(value) = crate::ask_v2::input_values::documented_literal(name, &field["schema"], subject) { values.insert(name.into(), value); }
                }
                if let Some(value) = values.get(name).cloned() {
                    if catalog_core::facts::schema::validate(&field["schema"], &value).is_err() {
                        // Browser number/boolean controls coerce their text;
                        // API callers need the same schema-valid wire value.
                        let typed = value.as_str().and_then(|s| serde_json::from_str::<Value>(s).ok())
                            .filter(|typed| catalog_core::facts::schema::validate(&field["schema"], typed).is_ok());
                        values.remove(name);
                        if let Some(typed) = typed { values.insert(name.into(), typed); }
                    }
                }
            }
            let resolves: Vec<_> = endpoint["lookup"]["resolves"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            let lookup_field = endpoint["lookup"]["field"].as_str();
            let lookup_value = lookup_field.filter(|field| matches!(*field, "company.name" | "geo.input.q"))
                .and_then(|field| subject.and_then(|subject| crate::ask_v2::input_values::literal(field, subject)))
                .filter(|value| catalog_core::facts::schema::validate(&endpoint["lookup"]["schema"], value).is_ok());
            let by_lookup = !resolves.is_empty() && resolves.iter().any(|field| !values.contains_key(*field)) && lookup_value.is_some();
            if by_lookup {
                for field in &resolves { values.remove(*field); }
                values.insert(lookup_field.unwrap().into(), lookup_value.unwrap());
            }
            let missing: Vec<_> = endpoint["inputs"].as_array().into_iter().flatten()
                .filter(|field| field["required"] == true)
                .filter_map(|field| field["field"].as_str())
                .filter(|field| !values.contains_key(*field) && !(by_lookup && resolves.contains(field)))
                .map(str::to_owned).collect();
            endpoint["input_values"] = Value::Object(values);
            endpoint["missing_inputs"] = json!(missing);
        }
    }
}

async fn describe_external(matches: &mut Value, state: &ServerState) {
    use futures_util::{stream, StreamExt};
    use crate::ask_v2::external;
    let Some(directory) = state.executor_compat.as_ref() else { return; };
    let mut selections = BTreeSet::new();
    for found in matches.as_array().into_iter().flatten() {
        for candidate in found["coinbase"].as_array().into_iter().flatten() {
            if let (Some(url), Some(method), Some(query)) = (candidate["url"].as_str(), candidate["method"].as_str(), found["query"].as_str()) {
                selections.insert((url.to_owned(), method.to_owned(), query.to_owned()));
            }
        }
    }
    let offers: BTreeMap<_, _> = stream::iter(selections).map(|key| async move {
        let offer = external::resolve(&state.engine.executor.http, directory.directory_origin(), &key.0, &key.1, &key.2).await.ok();
        (key, offer)
    }).buffer_unordered(4).collect().await;
    for found in matches.as_array_mut().into_iter().flatten() {
        let query = found["query"].as_str().unwrap_or_default().to_owned();
        for candidate in found["coinbase"].as_array_mut().into_iter().flatten() {
            let key = (candidate["url"].as_str().unwrap_or_default().to_owned(), candidate["method"].as_str().unwrap_or_default().to_owned(), query.clone());
            let Some(Some(offer)) = offers.get(&key) else { continue; };
            let id = external::selection_id(&key.0, &key.1, &key.2);
            candidate["api_id"] = json!(id);
            candidate["availability"] = json!("supported");
            candidate["capability"] = json!("external.request");
            candidate["endpoint"] = json!({"api_id":id,"capability":"external.request","inputs":offer.inputs,
                "outputs":null,"output_schema":null,"lookup":null,
                "price":{"currency":"USDC","provider_atomic":offer.provider_atomic.to_string(),"buyer_atomic":offer.total_atomic.to_string()}});
        }
    }
}

fn brand_matches(matches: &mut serde_json::Value, sources: &[crate::sources::Source]) {
    for found in matches.as_array_mut().into_iter().flatten() {
        for candidate in found["apiosk"].as_array_mut().into_iter().flatten() {
            let source = candidate["source_slug"]
                .as_str()
                .and_then(|slug| sources.iter().find(|source| source.slug == slug));
            if let Some(source) = source {
                candidate["name"] = json!(source.name);
                candidate["logo_url"] = json!(source
                    .logo_url
                    .as_deref()
                    .or(source.provider_logo_url.as_deref()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_research_preserves_exact_displayed_live_checks_and_stable_metadata() {
        let fixtures:Value=serde_json::from_str(include_str!("../../tests/fixtures/profession-live-planning.json")).unwrap();
        for row in fixtures["cases"].as_array().unwrap() {
            let request=ResearchRequest {selection:None,search_id:None,discovery_id:None,request_id:Uuid::new_v4(),question:row["question"].as_str().unwrap().into(),parsed_request:serde_json::from_value(row["displayed_parse"].clone()).unwrap()};
            let normalized=request.normalized().unwrap_or_else(|error|panic!("case {}: {error}",row["case"]));
            assert_eq!(serde_json::to_value(&normalized.subjects).unwrap(),row["displayed_parse"]["subjects"]);
            assert_eq!(normalized.capabilities.len(),request.parsed_request.capabilities.len());
            assert_eq!(normalized.meta.text_sha256,request.parsed_request.meta.text_sha256);
            assert_eq!(normalized.meta.parsed_at,request.parsed_request.meta.parsed_at);
            let mut changed=request;
            changed.question.push_str(" and different requirements");
            assert!(changed.normalized().is_err(),"changed question cannot reuse another parse");
        }
    }
    #[test]
    fn complete_research_keeps_sixteen_checks_and_refuses_seventeen() {
        let fixtures:Value=serde_json::from_str(include_str!("../../tests/fixtures/profession-live-planning.json")).unwrap();
        let row=&fixtures["cases"][0];
        let mut raw=row["displayed_parse"].clone();
        let base=raw["capabilities"][0].clone();
        raw["capabilities"]=json!((0..16).map(|index|{let mut cap=base.clone();cap["slug"]=json!(format!("location.check{index}"));cap}).collect::<Vec<_>>());
        let question=row["question"].as_str().unwrap();
        let parsed=capability_intake::normalize(&raw.to_string(),question,"fixture").unwrap();
        let mut request=ResearchRequest {selection:None,search_id:None,discovery_id:None,request_id:Uuid::new_v4(),question:question.into(),parsed_request:parsed};
        assert_eq!(request.normalized().unwrap().capabilities.len(),16);
        let mut extra=request.parsed_request.capabilities[0].clone();extra.slug="location.check16".into();
        request.parsed_request.capabilities.push(extra);
        assert!(request.normalized().is_err());
    }
    #[test]
    fn complete_research_accepts_full_followup_context_and_refuses_invented_values() {
        let question="Previous request (context only): MONZO BANK LIMITED company number 09446231\n\nCurrent request: and the directors?";
        let parsed=capability_intake::normalize(&json!({"language":"en","subjects":[{"id":"s1","label":"MONZO BANK LIMITED","type":"company","identifiers":{"company_number":"09446231"}}],"capabilities":[{"slug":"company.directors","name":"Company directors","description":"Retrieve company directors","domain":"company","inputs":[],"outputs":[],"subject_id":"s1","source_hint":null,"confidence":1}],"deliverable":{"format":"chat","operations":[]}}).to_string(),question,"fixture").unwrap();
        let mut request=ResearchRequest {selection:None,search_id:None,discovery_id:None,request_id:Uuid::new_v4(),question:question.into(),parsed_request:parsed};
        assert_eq!(request.normalized().unwrap().subjects[0].identifiers["company_number"],"09446231");
        request.parsed_request.subjects[0].identifiers.insert("kvk_number".into(),"99998888".into());
        assert!(request.normalized().is_err(),"normalization cannot silently discard an invented supplied identifier");
    }

    #[test]
    #[ignore = "requires read-only saved audit App chats and CATALOG_SNAPSHOT; no network"]
    fn audit_saved_profession_discovery_and_binding() {
        let dir=std::path::PathBuf::from(std::env::var("PROFESSION_AUDIT_DIR").unwrap());
        let saved:Value=serde_json::from_slice(&std::fs::read(dir.join("../sources/live-app-test-chats.json")).unwrap()).unwrap();
        let snapshot:crate::catalog::Snapshot=serde_json::from_slice(&std::fs::read(std::env::var("CATALOG_SNAPSHOT").unwrap()).unwrap()).unwrap();
        let catalog=crate::catalog::Catalog::build(snapshot);
        let mut reports=vec![];
        for row in saved["rows"].as_array().unwrap() {
            let mut report=json!({"question":row["question"],"chat_id":row["id"],"catalog_version":catalog.version,"boundary":"Actual saved live model parse replayed through local search + endpoint descriptors + literal binder; no model calls, tasks, approvals, purchases or source execution."});
            let mut raw=row["parsed"]["parsed_request"].clone();
            if raw.is_object() {
                for cap in raw["capabilities"].as_array_mut().into_iter().flatten() {cap.as_object_mut().unwrap().remove("status");}
                let parsed:ParsedRequest=serde_json::from_value(raw).unwrap();
                let mut matches=serde_json::to_value(crate::ask_v2::search::search(&parsed,&catalog)).unwrap();
                describe_endpoints(&mut matches,&catalog,None);
                bind_inputs(&mut matches,&parsed);
                report["status"]=json!("replayed"); report["matches"]=matches;
                report["parsed_request"]=json!(parsed);
            } else {report["status"]=json!("live_parse_failed");}
            reports.push(report);
        }
        std::fs::write(dir.join("live-parse-replay.json"),serde_json::to_vec_pretty(&reports).unwrap()).unwrap();
        println!("Replayed {} saved live App records without network or purchases",reports.len());
    }
    #[test]
    fn each_discovered_operation_binds_only_its_own_subject() {
        let parsed = capability_intake::normalize(&json!({"language":"nl","subjects":[
            {"id":"s1","label":"Coolblue B.V.","type":"company","identifiers":{"kvk_number":"24330087"}},
            {"id":"s2","label":"HEMA B.V.","type":"company","identifiers":{"kvk_number":"34395564"}}],
            "capabilities":[
                {"slug":"company.profile","name":"Company profile","description":"Registry profile","domain":"company","inputs":[],"outputs":[],"subject_id":"s1","source_hint":null,"confidence":1},
                {"slug":"company.profile","name":"Company profile","description":"Registry profile","domain":"company","inputs":[],"outputs":[],"subject_id":"s2","source_hint":null,"confidence":1}],
            "deliverable":{"format":"chat","operations":["compare"]}}).to_string(), "Coolblue B.V. 24330087 HEMA B.V. 34395564", "fixture").unwrap();
        let candidate=json!({"endpoint":{"inputs":[{"field":"company_registry.kvknummer","required":true,"schema":{"type":"string","pattern":"^[0-9]{8}$"}}],"lookup":{"resolves":["company_registry.kvknummer"]}}});
        let mut matches=json!([{"slug":"company.profile","apiosk":[candidate.clone()]},{"slug":"company.profile","apiosk":[candidate]}]);
        bind_inputs(&mut matches,&parsed);
        assert_eq!(matches[0]["apiosk"][0]["endpoint"]["input_values"],json!({"company_registry.kvknummer":"24330087"}));
        assert_eq!(matches[1]["apiosk"][0]["endpoint"]["input_values"],json!({"company_registry.kvknummer":"34395564"}));
        assert_eq!(matches[0]["apiosk"][0]["subject_id"],"s1");
        assert_eq!(matches[1]["apiosk"][0]["subject_id"],"s2");
        let mut collapsed=json!([{"slug":"company.profile","coinbase":[{"endpoint":{"inputs":[{"field":"company.name","required":true,"schema":{"type":"string"}}]}}]}]);
        bind_inputs(&mut collapsed,&parsed);
        assert_eq!(collapsed[0]["coinbase"][0]["endpoint"]["input_values"],json!({}),"a collapsed directory match cannot bind the first subject to the last query");
        assert!(collapsed[0]["coinbase"][0]["subject_id"].is_null());
    }

    #[test]
    fn professional_searches_keep_literal_topic_and_typed_vehicle_and_vat_values() {
        for (slug,subject,field,expected) in [
            ("procurement.notices",json!({"id":"s1","label":"IT-diensten","type":"topic","identifiers":{}}),"company_intel.input.search",json!("IT-diensten")),
            ("finance.subsidy",json!({"id":"s1","label":"verduurzaming","type":"topic","identifiers":{}}),"subsidy.discovery.keyword",json!("verduurzaming")),
            ("vehicle.profile",json!({"id":"s1","label":"GZ934Z","type":"vehicle","identifiers":{"license_plate":"GZ934Z"}}),"vehicle.registration.nl.plate",json!("GZ934Z")),
            ("compliance.vat",json!({"id":"s1","label":"NL123456789B01","type":"vat","identifiers":{"vat_number":"NL123456789B01"}}),"vat.eu.number",json!("123456789B01")),
            ("compliance.vat",json!({"id":"s1","label":"NL123456789B01","type":"vat","identifiers":{"vat_number":"NL123456789B01"}}),"vat.eu.country_code",json!("NL")),
        ] {
            let question=subject["label"].as_str().unwrap();
            let parsed=capability_intake::normalize(&json!({"language":"nl","subjects":[subject],"capabilities":[{"slug":slug,"name":"Requested data","description":"Retrieve the requested data","domain":slug.split('.').next().unwrap(),"inputs":[],"outputs":[],"subject_id":"s1","source_hint":null,"confidence":1}],"deliverable":{"format":"chat","operations":[]}}).to_string(),question,"fixture").unwrap();
            let mut matches=json!([{"slug":slug,"apiosk":[{"endpoint":{"inputs":[{"field":field,"required":true,"schema":{"type":"string"}}]}}]}]);
            bind_inputs(&mut matches,&parsed);
            assert_eq!(matches[0]["apiosk"][0]["endpoint"]["input_values"][field],expected);
            assert_eq!(matches[0]["apiosk"][0]["endpoint"]["missing_inputs"],json!([]));
        }
    }

    #[test]
    fn a_topicless_request_does_not_invent_a_search_query_or_reuse_another_subject() {
        let parsed=capability_intake::normalize(&json!({"language":"nl","subjects":[{"id":"s1","label":"Utrecht","type":"city","identifiers":{}}],"capabilities":[{"slug":"procurement.notices","name":"Public tenders","description":"Find public tenders","domain":"procurement","inputs":[],"outputs":[],"subject_id":null,"source_hint":null,"confidence":1}],"deliverable":{"format":"chat","operations":[]}}).to_string(),"Aanbestedingen in Utrecht","fixture").unwrap();
        let mut matches=json!([{"slug":"procurement.notices","apiosk":[{"endpoint":{"inputs":[{"field":"company_intel.input.search","required":true,"schema":{"type":"string"}}]}}]}]);
        bind_inputs(&mut matches,&parsed);
        assert_eq!(matches[0]["apiosk"][0]["endpoint"]["input_values"],json!({}));
        assert_eq!(matches[0]["apiosk"][0]["endpoint"]["missing_inputs"],json!(["company_intel.input.search"]));
    }
    #[test]
    fn discovered_values_are_typed_and_invalid_literals_remain_missing() {
        let parsed = capability_intake::normalize(&json!({"language":"en","subjects":[{"id":"s1","label":"Example","type":"company","identifiers":{"number":"42","bad":"nope"}}],
            "capabilities":[{"slug":"company.profile","name":"Company profile","description":"Company registry profile","domain":"company","inputs":[],"outputs":[],"subject_id":"s1","source_hint":null,"confidence":1}],
            "deliverable":{"format":"json","operations":[]}}).to_string(), "Example 42 nope", "fixture").unwrap();
        let mut matches = json!([{"coinbase":[{"endpoint":{"inputs":[
            {"field":"query.number","required":true,"schema":{"type":"integer"}},
            {"field":"query.bad","required":true,"schema":{"type":"integer"}}
        ]}}]}]);
        bind_inputs(&mut matches, &parsed);
        assert_eq!(matches[0]["coinbase"][0]["endpoint"]["input_values"], json!({"query.number":42}));
        assert_eq!(matches[0]["coinbase"][0]["endpoint"]["missing_inputs"], json!(["query.bad"]));
    }
    #[test]
    fn coinbase_table_compares_buyer_totals_with_integer_fee_rounding() {
        assert_eq!(buyer_discovery_price("0.010000 USD").as_deref(), Some("0.011500 USD"));
        assert_eq!(buyer_discovery_price("0.000001 USD").as_deref(), Some("0.000002 USD"));
        assert_eq!(buyer_discovery_price("unknown USD"), None);
        assert_eq!(buyer_discovery_price("-1.000000 USD"), None);
    }
    #[test]
    fn endpoint_descriptor_uses_live_canonical_inputs_and_server_prices() {
        let catalog = crate::catalog::Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/integration-catalog.json")).unwrap());
        let service = catalog.services.iter().find(|s| s.contract.capability_slug == "company.accounts").unwrap();
        let descriptor = endpoint_descriptor(&catalog, service, None);
        assert_eq!(descriptor["endpoint_id"], json!(service.endpoint_id));
        assert_eq!(descriptor["inputs"], json!([{"field":"company.id","required":true,"schema":{"type":"string"}}]));
        assert_eq!(descriptor["price"]["buyer_atomic"], service.total().to_string());
        assert_eq!(descriptor["price"]["provider_atomic"], service.list_atomic.to_string());
        assert_eq!(descriptor["source"]["slug"], service.api_slug);
    }

    #[test]
    fn prepare_accepts_only_a_bounded_exact_endpoint_and_inputs() {
        let mut request: PrepareRequest = serde_json::from_value(json!({"request_id":Uuid::new_v4(),"endpoint_id":Uuid::new_v4(),"input":{"company.id":"123"}})).unwrap();
        assert!(request.valid());
        request.input.insert("x".repeat(201), json!("value"));
        assert!(!request.valid());
        request.input.clear();
        request.endpoint_id = Uuid::nil();
        assert!(!request.valid());
        assert!(serde_json::from_value::<PrepareRequest>(json!({"request_id":Uuid::new_v4(),"endpoint_id":Uuid::new_v4(),"input":{},"price":0,"approved":true})).is_err());
    }

    #[test]
    fn neighbourhood_descriptor_offers_address_lookup_from_published_contracts() {
        let catalog = crate::ask_v2::test_catalog::locations();
        let service = catalog.services.iter().find(|s| s.contract.capability_slug == "area.cbs-neighbourhood-2025").unwrap();
        let descriptor = endpoint_descriptor(&catalog, service, None);
        assert_eq!(descriptor["lookup"], json!({"field":"geo.input.q","resolves":["location.neighbourhood_code"],"schema":{"type":"string"}}));
        assert_eq!(descriptor["inputs"][0]["field"], "location.neighbourhood_code");
        assert_eq!(descriptor["price"]["buyer_atomic"], service.total().to_string());
    }

    #[test]
    fn registered_candidates_publish_their_input_forms_without_a_planner_mapping() {
        let catalog = crate::catalog::Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/registered-endpoints.json")).unwrap());
        let mut found = json!([{"apiosk":catalog.services.iter().map(|service| json!({
            "endpoint_id":service.endpoint_id,"capability":service.contract.capability_slug,"availability":"unverified"
        })).collect::<Vec<_>>()}]);
        describe_endpoints(&mut found, &catalog, None);
        for candidate in found[0]["apiosk"].as_array().unwrap() {
            assert!(candidate["endpoint"]["price"]["buyer_atomic"].is_string());
            assert!(candidate["endpoint"]["inputs"].as_array().unwrap().iter().any(|input| input["required"] == true));
            assert!(candidate["endpoint"]["inputs"].as_array().unwrap().iter().all(|input| input["schema"]["title"].is_string()));
        }
    }

    #[test]
    fn discovery_binds_the_address_seed_for_the_apps_canonical_inputs() {
        let catalog = crate::ask_v2::test_catalog::locations();
        let service = catalog.services.iter().find(|s| s.contract.capability_slug == "area.cbs-neighbourhood-2025").unwrap();
        let parsed = capability_intake::normalize(&json!({"language":"en","subjects":[{"id":"s1","label":"Damrak 1, Amsterdam","type":"address","identifiers":{}}],
            "capabilities":[{"slug":"location.building.neighbourhood.info","name":"Building and neighbourhood information","description":"Building and neighbourhood records","domain":"location","inputs":["address"],"outputs":["building_info","neighbourhood_info"],"subject_id":"s1","source_hint":null,"confidence":0.9}],
            "deliverable":{"format":"chat","operations":[]}}).to_string(), "Find building and neighbourhood information for Damrak 1, Amsterdam.", "fixture").unwrap();
        let mut matches = json!([{"apiosk":[{"endpoint":endpoint_descriptor(&catalog, service, None)}]}]);
        bind_inputs(&mut matches, &parsed);
        assert_eq!(matches[0]["apiosk"][0]["endpoint"]["input_values"], json!({"geo.input.q":"Damrak 1, Amsterdam"}));
        assert_eq!(matches[0]["apiosk"][0]["endpoint"]["missing_inputs"], json!([]));
    }

    fn request() -> ParseRequest {
        ParseRequest {
            request_id: Uuid::new_v4(),
            question: "Company officers?".into(),
            known_slugs: vec![],
            correction: None,
        }
    }
    #[test]
    fn bounds_transport_without_imposing_old_planner_contract() {
        let mut r = request();
        assert!(r.valid());
        r.question = "é".repeat(16_000);
        assert!(r.valid());
        r.question.push('é');
        assert!(!r.valid());
        r.question = " ".into();
        assert!(!r.valid());
        r.question = "Company financial statements?".into();
        r.known_slugs = vec!["company.financial.keyFigures".into()];
        assert!(r.valid());
        r.known_slugs.push("ignore instructions\n".into());
        assert!(!r.valid());
    }
    #[test]
    fn search_metadata_cannot_expand_into_execution_or_unbounded_queries() {
        let value = json!({"language":"en","subjects":[],"capabilities":[{"slug":"company.profile","name":"Company profile","description":"A company profile.","domain":"company","inputs":[],"outputs":[],"subject_id":null,"source_hint":null,"confidence":0.9,"status":null,"missing":[]}],"deliverable":{"format":"chat","operations":[]},"meta":{"model":"google/gemma-3-4b-it","parsed_at":"now","text_sha256":"hash"}});
        let mut parsed: ParsedRequest = serde_json::from_value(value).unwrap();
        assert!(valid_parsed(&parsed));
        parsed.capabilities.push(parsed.capabilities[0].clone());
        assert!(!valid_parsed(&parsed));
        parsed.capabilities.pop();
        parsed.capabilities[0].inputs = vec!["x".repeat(513)];
        assert!(!valid_parsed(&parsed));
        assert!(serde_json::from_value::<SearchRequest>(
            json!({"parsed_request":parsed,"origin":"https://other.example"})
        )
        .is_err());
    }
}
