//! The public Gateway v2 API: discover sources, run one source directly, or
//! Ask and let Apiosk plan across sources.
//!
//! Built for organisation API keys (`apk_org_live_…`), and open to every
//! other credential alike: after `server::authenticated` a caller is an
//! `AuthContext`, and nothing below asks which credential it presented except
//! to check a key's scopes.
//!
//! Thin adapters only. A direct run is a one-goal intent pinned to its source
//! (`Engine::direct`); an Ask is `Engine::discover`. Both are priced by the
//! same planner, approved against the same connection budget, run by the same
//! server worker and paid through the same ledger reservation as the App and
//! the MCP server. There are no API-only sources and no API-only prices: the
//! per-source usage of a run sums to what the ledger charged.
// Refusals are finished HTTP responses, built only on the refusal path.
#![allow(clippy::result_large_err)]
use crate::{
    auth::AuthContext,
    catalog::{Catalog, Service},
    protocol::{DiscoverRequest, ExecuteRequest, Response, Status, MAX_QUESTION},
    resolver::Plan,
    server::{authenticated, token, Refusal, ServerState},
};
use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response as HttpResponse},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use uuid::Uuid;

const DEFAULT_WAIT_SECONDS: u64 = 25;
const MAX_WAIT_SECONDS: u64 = 55;
const MAX_SPEND_USD: f64 = 100_000.0;

type Answer = Result<HttpResponse, HttpResponse>;

// ---------------------------------------------------------------------------
// One error shape: {"error":{"code","message",...details}}
// ---------------------------------------------------------------------------

fn problem(status: StatusCode, code: &str, message: &str) -> HttpResponse {
    problem_with(status, code, message, json!({}))
}
fn problem_with(status: StatusCode, code: &str, message: &str, details: Value) -> HttpResponse {
    let mut error = json!({"code": code, "message": message});
    if let (Some(error), Some(details)) = (error.as_object_mut(), details.as_object()) {
        error.extend(details.clone());
    }
    (status, Json(json!({ "error": error }))).into_response()
}
pub(crate) fn refused(refusal: Refusal) -> HttpResponse {
    if refusal.code == "unauthorized" {
        return problem(refusal.status, "invalid_api_key", "Send a valid key as `Authorization: Bearer apk_org_live_…`.");
    }
    problem(refusal.status, refusal.code, refusal.message)
}
pub(crate) fn insufficient_scope(scope: &str) -> HttpResponse {
    problem_with(
        StatusCode::FORBIDDEN,
        "insufficient_scope",
        "This API key does not have the permission this endpoint needs. An organisation admin can create a key that has it.",
        json!({ "required_scope": scope }),
    )
}
fn busy() -> HttpResponse {
    problem(StatusCode::TOO_MANY_REQUESTS, "rate_limit_exceeded", "Too many requests are running right now. Retry shortly with the same Idempotency-Key.")
}
fn unavailable(context: &str, error: &anyhow::Error) -> HttpResponse {
    if error.to_string().contains("request rate limit") {
        return problem(StatusCode::TOO_MANY_REQUESTS, "rate_limit_exceeded", "More than 60 requests in one minute for this organisation. Wait a minute, then retry with the same Idempotency-Key.");
    }
    tracing::error!(error = %error, context, "public api dependency failed");
    problem(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", "The request could not be completed right now. Retry with the same Idempotency-Key; nothing is charged twice.")
}
fn not_found(error: &anyhow::Error) -> HttpResponse {
    if error.downcast_ref::<crate::store::StoreAccessError>().is_some() {
        return problem(StatusCode::NOT_FOUND, "run_not_found", "No ask or run with this id belongs to this caller.");
    }
    unavailable("task load", error)
}
fn no_bearer() -> HttpResponse {
    refused(Refusal { status: StatusCode::UNAUTHORIZED, code: "unauthorized", message: "" })
}

async fn context(state: &ServerState, headers: &HeaderMap, scopes: &[&str]) -> Result<AuthContext, HttpResponse> {
    let ctx = authenticated(state, headers).await.map_err(refused)?;
    if let Some(scope) = scopes.iter().find(|scope| !ctx.allows(scope)) {
        return Err(insufficient_scope(scope));
    }
    Ok(ctx)
}
fn parse<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, HttpResponse> {
    serde_json::from_slice(body).map_err(|error| {
        problem(StatusCode::BAD_REQUEST, "invalid_request", &format!("The request body is not valid: {error}."))
    })
}

// ---------------------------------------------------------------------------
// Money: the ledger counts micro-USD; the API shows both forms
// ---------------------------------------------------------------------------

pub(crate) fn money(atomic: i64) -> Value {
    json!({ "amount": atomic as f64 / 1_000_000.0, "amount_micro_usd": atomic.to_string(), "currency": "USD" })
}
fn micro(value: &Value) -> Option<i64> {
    value.as_str().and_then(|s| s.parse().ok()).or_else(|| value.as_i64())
}
/// `max_spend` in USD, as a JSON number or a decimal string.
fn spend_limit(value: Option<&Value>) -> Result<Option<i64>, HttpResponse> {
    let Some(value) = value else { return Ok(None) };
    let usd = match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    };
    match usd {
        Some(usd) if usd.is_finite() && (0.0..=MAX_SPEND_USD).contains(&usd) => Ok(Some((usd * 1_000_000.0).round() as i64)),
        _ => Err(problem(StatusCode::BAD_REQUEST, "invalid_request", "max_spend must be an amount in USD between 0 and 100000.")),
    }
}
fn wait_for(seconds: Option<u64>) -> Duration {
    Duration::from_secs(seconds.unwrap_or(DEFAULT_WAIT_SECONDS).min(MAX_WAIT_SECONDS))
}
fn limit(value: Option<usize>) -> Result<Option<usize>, HttpResponse> {
    match value {
        Some(n) if !(1..=50).contains(&n) => Err(problem(StatusCode::BAD_REQUEST, "invalid_request", "limit must be between 1 and 50.")),
        other => Ok(other),
    }
}

// ---------------------------------------------------------------------------
// Idempotency
// ---------------------------------------------------------------------------

/// The request id an `Idempotency-Key` names. Scoped to the caller's
/// connection and the route, so two keys of one organisation, or two
/// endpoints, never share a request. A retry replays the saved answer and
/// cannot create a second purchase; a different body under the same key is a
/// conflict. Without the header every call is a new request.
fn request_id(headers: &HeaderMap, ctx: &AuthContext, route: &str) -> Result<Uuid, HttpResponse> {
    let Some(value) = headers.get("idempotency-key") else {
        return Ok(Uuid::new_v4());
    };
    let key = value
        .to_str()
        .ok()
        .filter(|key| (1..=255).contains(&key.len()) && key.bytes().all(|b| b.is_ascii_graphic()))
        .ok_or_else(|| problem(StatusCode::BAD_REQUEST, "invalid_request", "Idempotency-Key must be 1 to 255 visible ASCII characters."))?;
    let scope = ctx.principal.connection.unwrap_or(ctx.principal.owner);
    let digest = Sha256::digest(format!("public-api\u{0}{scope}\u{0}{route}\u{0}{key}").as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    Ok(uuid::Builder::from_random_bytes(bytes).into_uuid())
}

/// A request the engine refused before any task existed.
fn refused_by_engine(r: &Response) -> Option<HttpResponse> {
    if r.state.is_some() {
        return None;
    }
    let code = r.errors.first().and_then(|e| e["code"].as_str()).unwrap_or("operation_failed");
    Some(match code {
        "request_conflict" => problem(StatusCode::CONFLICT, "idempotency_conflict", "This Idempotency-Key was already used with a different request."),
        "request_pending" => problem_with(StatusCode::CONFLICT, "request_in_progress", "The first request with this Idempotency-Key is still running. Retry shortly with the same key.", json!({"retry_after_ms": 2000})),
        "question_unavailable" => problem(StatusCode::SERVICE_UNAVAILABLE, "planner_unavailable", "The question could not be interpreted right now. Retry shortly; nothing was charged."),
        "unsupported_request" => problem(StatusCode::UNPROCESSABLE_ENTITY, "unsupported", "Apiosk has no sources that can answer this question. Nothing was charged."),
        "invalid_workflow_input" => problem(StatusCode::UNPROCESSABLE_ENTITY, code, r.errors.first().and_then(|e|e["message"].as_str()).unwrap_or("Check the workflow fields.")),
        "workflow_unavailable" => problem(StatusCode::UNPROCESSABLE_ENTITY, code, "The required source operations are unavailable for these workflow inputs. Nothing was charged."),
        _ => {
            tracing::warn!(errors = ?r.errors, "public api request refused by the engine");
            problem(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", "The request could not be prepared. Retry with the same Idempotency-Key; nothing was charged.")
        }
    })
}

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

fn inputs_json(service: &Service, catalog: &Catalog) -> Value {
    json!(service.contract.requires.iter().map(|r| json!({
        "field": r.fact_type,
        "required": !r.optional,
        "schema": catalog.vocabulary.get(&r.fact_type).map(|f| &f.value_schema),
    })).collect::<Vec<_>>())
}
/// One operation as a client sees it. When a source offers the same
/// operation through several endpoints — a lookup by KVK number or by branch
/// number, a registry profile by SIREN or by EU national id — each is a
/// variant with its own inputs and price, and a call runs the variant whose
/// inputs it supplies.
fn operation(variants: &[&Service], catalog: &Catalog) -> Value {
    let first = variants[0];
    let mut entry = json!({
        "id": first.contract.capability_slug,
        "source_id": first.api_slug,
        "name": first.name,
        "input": inputs_json(first, catalog),
        "output": variants.iter().flat_map(|s| s.contract.output_map.produces.keys()).collect::<std::collections::BTreeSet<_>>(),
        "pricing": { "model": "per_call", "price": money(first.total()) },
        "freshness_seconds": first.freshness_seconds,
    });
    if variants.len() > 1 {
        entry["variants"] = json!(variants.iter().map(|s| json!({
            "input": inputs_json(s, catalog),
            "pricing": { "model": "per_call", "price": money(s.total()) },
        })).collect::<Vec<_>>());
    }
    entry
}

/// The services a source id stands for. An id is a source's own id, a
/// provider that groups several sources (`pulsenetwork`), or either one's
/// name. `GET /v2/sources` lists a grouped provider once, so its id must work
/// everywhere a source id does.
fn members(catalog: &Catalog, sources: &[crate::sources::Source], id: &str) -> Option<(String, Vec<String>)> {
    let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    let executable: std::collections::BTreeSet<&str> = catalog.services.iter().map(|s| s.api_slug.as_str()).collect();
    if let Some(slug) = executable.iter().find(|slug| same(slug, id)) {
        return Some(((*slug).to_owned(), vec![(*slug).to_owned()]));
    }
    let group = |key: &str| -> Vec<String> {
        sources.iter().filter(|s| crate::sources::directory_slug(s) == key && executable.contains(s.slug.as_str())).map(|s| s.slug.clone()).collect()
    };
    for source in sources {
        let key = crate::sources::directory_slug(source);
        if same(key, id) || same(crate::sources::directory_name(source), id) || same(&source.name, id) {
            let slugs = if same(&source.name, id) && key == source.slug { vec![source.slug.clone()] } else { group(key) };
            if !slugs.is_empty() {
                return Some((if slugs.len() == 1 { slugs[0].clone() } else { key.to_owned() }, slugs));
            }
        }
    }
    None
}

/// Every operation of these sources, each with its variants, in catalogue order.
fn operations<'a>(catalog: &'a Catalog, slugs: &[String]) -> Vec<Vec<&'a Service>> {
    let mut grouped: Vec<Vec<&Service>> = Vec::new();
    for service in catalog.services.iter().filter(|s| slugs.contains(&s.api_slug)) {
        match grouped.iter_mut().find(|g| g[0].api_slug == service.api_slug && g[0].contract.capability_slug == service.contract.capability_slug) {
            Some(group) => group.push(service),
            None => grouped.push(vec![service]),
        }
    }
    grouped
}

/// The request body of a direct call, bound to the fact types one variant
/// takes. A caller may use the fact type (`company.name`), the name the
/// source's own API uses (`naam`), or the unambiguous last word (`name`).
fn bind_inputs(service: &Service, body: &serde_json::Map<String, Value>) -> Result<BTreeMap<String, Value>, HttpResponse> {
    let fields: Vec<&str> = service.contract.requires.iter().map(|r| r.fact_type.as_str()).collect();
    let mut bound = BTreeMap::new();
    for (key, value) in body {
        let wire = service.contract.input_map.iter().find(|(fact, binding)| fields.contains(&fact.as_str()) && binding.name.eq_ignore_ascii_case(key)).map(|(fact, _)| fact.as_str());
        let tail: Vec<&str> = fields.iter().copied().filter(|fact| fact.rsplit('.').next().is_some_and(|last| last.eq_ignore_ascii_case(key))).collect();
        let field = fields
            .iter()
            .copied()
            .find(|fact| fact.eq_ignore_ascii_case(key))
            .or(wire)
            .or_else(|| (tail.len() == 1).then(|| tail[0]));
        let Some(field) = field else {
            return Err(problem_with(StatusCode::UNPROCESSABLE_ENTITY, "invalid_source_input", &format!("`{key}` is not an input of this operation."),
                json!({"field": key, "expected": fields})));
        };
        if bound.insert(field.to_owned(), value.clone()).is_some() {
            return Err(problem_with(StatusCode::UNPROCESSABLE_ENTITY, "invalid_source_input", &format!("`{field}` was given twice under different names."), json!({"field": field})));
        }
    }
    let missing: Vec<&str> = service.contract.requires.iter().filter(|r| !r.optional && !bound.contains_key(&r.fact_type)).map(|r| r.fact_type.as_str()).collect();
    if !missing.is_empty() {
        return Err(problem_with(StatusCode::UNPROCESSABLE_ENTITY, "invalid_source_input", "Required inputs are missing. Nothing was charged.", json!({"missing": missing})));
    }
    Ok(bound)
}

/// The variant a call runs: the one whose inputs the body supplies. One
/// variant keeps its own precise refusal; several that fit none list what
/// each would need; the same operation offered by two sources of a group is
/// ambiguous and names them.
fn choose<'a>(candidates: &[&'a Service], body: &serde_json::Map<String, Value>) -> Result<(&'a Service, BTreeMap<String, Value>), HttpResponse> {
    let fitting: Vec<(&Service, BTreeMap<String, Value>)> = candidates.iter().filter_map(|s| bind_inputs(s, body).ok().map(|bound| (*s, bound))).collect();
    let sources: std::collections::BTreeSet<&str> = fitting.iter().map(|(s, _)| s.api_slug.as_str()).collect();
    if sources.len() > 1 {
        return Err(problem_with(StatusCode::CONFLICT, "ambiguous_operation", "Several sources in this group offer this operation. Call one by its own source id.",
            json!({"sources": sources})));
    }
    if let Some((service, bound)) = fitting.into_iter().next() {
        return Ok((service, bound));
    }
    if let [only] = candidates {
        return bind_inputs(only, body).map(|bound| (*only, bound));
    }
    Err(problem_with(StatusCode::UNPROCESSABLE_ENTITY, "invalid_source_input", "The inputs match none of this operation's variants. Nothing was charged.",
        json!({"variants": candidates.iter().map(|s| json!({
            "source_id": s.api_slug,
            "required": s.contract.requires.iter().filter(|r| !r.optional).map(|r| &r.fact_type).collect::<Vec<_>>(),
        })).collect::<Vec<_>>()})))
}

/// Resolve `{source}` and `{operation}` leniently, then pick the variant. An
/// operation is named by its id, its endpoint id or its name.
fn resolve<'a>(catalog: &'a Catalog, sources: &[crate::sources::Source], source: &str, operation: &str, body: &serde_json::Map<String, Value>) -> Result<(&'a Service, BTreeMap<String, Value>), HttpResponse> {
    let Some((_, slugs)) = members(catalog, sources, source) else {
        return Err(problem(StatusCode::NOT_FOUND, "source_not_found", "No executable source has this id or name. List sources with GET /v2/sources."));
    };
    let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    let candidates: Vec<&Service> = catalog.services.iter().filter(|s| slugs.contains(&s.api_slug) && (
        same(&s.contract.capability_slug, operation) || s.endpoint_id.to_string() == operation || same(&s.name, operation)
    )).collect();
    if candidates.is_empty() {
        return Err(problem_with(StatusCode::NOT_FOUND, "operation_not_found", &format!("This source has no operation `{operation}`."),
            json!({"operations": operations(catalog, &slugs).iter().map(|v| json!({"id": v[0].contract.capability_slug, "source_id": v[0].api_slug})).collect::<Vec<_>>()})));
    }
    choose(&candidates, body)
}

/// `GET /v2/sources/{source_id}`: operations, input schemas and prices.
pub async fn source(State(state): State<ServerState>, headers: HeaderMap, Path(source_id): Path<String>) -> HttpResponse {
    if let Err(r) = context(&state, &headers, &["sources:read"]).await {
        return r;
    }
    let Ok(_permit) = state.permits.try_acquire() else { return busy() };
    let (sources, catalog) = match tokio::try_join!(state.engine.catalog.sources(), state.engine.catalog.get()) {
        Ok(both) => both,
        Err(error) => return unavailable("catalog", &error),
    };
    let Some((id, slugs)) = members(&catalog, &sources, &source_id) else {
        return problem(StatusCode::NOT_FOUND, "source_not_found", "No executable source has this id or name. List sources with GET /v2/sources.");
    };
    let grouped = operations(&catalog, &slugs);
    let first = grouped[0][0];
    let meta = sources.iter().find(|s| s.slug == id).or_else(|| sources.iter().find(|s| crate::sources::directory_slug(s) == id));
    let group = slugs.len() > 1;
    let mut body = json!({
        "id": id,
        "name": if group { meta.map_or(id.as_str(), crate::sources::directory_name) } else { meta.map_or(first.name.as_str(), |s| s.name.as_str()) },
        "description": if group { None } else { meta.map(|s| s.description.as_str()) },
        "category": meta.map(|s| &s.category),
        "tags": meta.map(|s| &s.tags),
        "sectors": meta.map(|s| &s.sectors),
        "logo_url": meta.and_then(|s| s.logo_url.as_deref()),
        "jurisdiction": first.jurisdiction,
        "operations": grouped.iter().map(|variants| operation(variants, &catalog)).collect::<Vec<_>>(),
    });
    if group {
        // A provider of several sources: each operation names the source that runs it.
        body["sources"] = json!(slugs);
    }
    Json(body).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceRun {
    operation: String,
    #[serde(default)]
    input: BTreeMap<String, Value>,
    #[serde(default)]
    max_spend: Option<Value>,
    #[serde(default)]
    wait_seconds: Option<u64>,
}

/// `POST /v2/sources/{source_id}/run`: one named operation, no model.
pub async fn run_source(State(state): State<ServerState>, headers: HeaderMap, Path(source_id): Path<String>, body: Bytes) -> HttpResponse {
    run_source_inner(&state, &headers, &source_id, &body).await.unwrap_or_else(|r| r)
}
async fn run_source_inner(state: &ServerState, headers: &HeaderMap, source_id: &str, body: &[u8]) -> Answer {
    let ctx = context(state, headers, &["sources:execute"]).await?;
    let req: SourceRun = parse(body)?;
    let max_spend = spend_limit(req.max_spend.as_ref())?;
    let catalog = state.engine.catalog.get().await.map_err(|e| unavailable("catalog", &e))?;
    let sources = state.engine.catalog.sources().await.unwrap_or_default();
    let values: serde_json::Map<String, Value> = req.input.into_iter().collect();
    let (service, input) = resolve(&catalog, &sources, source_id, &req.operation, &values)?;
    direct(state, headers, &ctx, service, input, max_spend, wait_for(req.wait_seconds), None).await
}

/// What a direct call may set besides its inputs. Query parameters, because
/// the body of `/v1/execute` is the source's own inputs and nothing else.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ExecuteQuery {
    #[serde(default)]
    max_spend: Option<String>,
    #[serde(default)]
    wait_seconds: Option<u64>,
    #[serde(default)]
    limit: Option<usize>,
}

/// `POST /v1/execute/{source}/{operation}` (also `/v2/execute/{source}/{operation}`):
/// the shortest paid call. The key says who pays, the path says what to run,
/// and the body is the input values themselves.
///
/// ```text
/// POST /v1/execute/kvk/company_registry.search
/// {"company.name": "Mollie B.V."}
/// ```
///
/// The same registry, prices, ceilings and ledger as every other call; only
/// the envelope is gone.
pub async fn execute(State(state): State<ServerState>, headers: HeaderMap, Path((source, operation)): Path<(String, String)>, Query(query): Query<ExecuteQuery>, body: Bytes) -> HttpResponse {
    execute_inner(&state, &headers, &source, &operation, query, &body).await.unwrap_or_else(|r| r)
}
async fn execute_inner(state: &ServerState, headers: &HeaderMap, source: &str, operation: &str, query: ExecuteQuery, body: &[u8]) -> Answer {
    let ctx = context(state, headers, &["sources:execute"]).await?;
    let max_spend = spend_limit(query.max_spend.map(Value::String).as_ref())?;
    let limit = limit(query.limit)?;
    let values: Value = if body.is_empty() { json!({}) } else { parse(body)? };
    let Some(values) = values.as_object() else {
        return Err(problem(StatusCode::BAD_REQUEST, "invalid_request", "The body must be a JSON object of input values, for example {\"company.name\":\"Mollie B.V.\"}."));
    };
    let catalog = state.engine.catalog.get().await.map_err(|e| unavailable("catalog", &e))?;
    let sources = state.engine.catalog.sources().await.unwrap_or_default();
    let (service, input) = resolve(&catalog, &sources, source, operation, values)?;
    direct(state, headers, &ctx, service, input, max_spend, wait_for(query.wait_seconds), limit).await
}

/// One direct call: validate against the contract, plan it, pay for it, run it.
#[allow(clippy::too_many_arguments)]
async fn direct(state: &ServerState, headers: &HeaderMap, ctx: &AuthContext, service: &Service, input: BTreeMap<String, Value>, max_spend: Option<i64>, wait: Duration, limit: Option<usize>) -> Answer {
    let (source_id, operation) = (service.api_slug.as_str(), service.contract.capability_slug.as_str());
    let request = request_id(headers, ctx, &format!("execute/{source_id}/{operation}"))?;
    let catalog = state.engine.catalog.get().await.map_err(|e| unavailable("catalog", &e))?;
    for (field, value) in &input {
        let Some(fact) = catalog.vocabulary.get(field) else {
            return Err(problem_with(StatusCode::UNPROCESSABLE_ENTITY, "invalid_source_input", &format!("`{field}` is not a known input."), json!({"field": field})));
        };
        if let Err(error) = catalog_core::facts::schema::validate(&fact.value_schema, value) {
            return Err(problem_with(StatusCode::UNPROCESSABLE_ENTITY, "invalid_source_input", &format!("`{field}` does not match its schema: {error}."),
                json!({"field": field, "schema": fact.value_schema})));
        }
    }
    let response = {
        let Ok(_permit) = state.permits.try_acquire() else { return Err(busy()) };
        state.engine.direct(&ctx.principal, request, source_id, operation, &input).await.map_err(|e| unavailable("direct run", &e))?
    };
    if let Some(refusal) = refused_by_engine(&response) {
        return Err(refusal);
    }
    let id = response.state.as_ref().map(|s| s.state_ref).ok_or_else(|| problem(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", "The run could not be saved. Retry with the same Idempotency-Key."))?;
    let (_, task, _) = state.engine.store.load_scoped(&ctx.principal, id).await.map_err(|e| not_found(&e))?;
    let plan: Option<Plan> = task.proposal.clone().map(serde_json::from_value).transpose().map_err(|e| unavailable("plan", &e.into()))?;
    match &plan {
        // A direct run buys exactly the named source, never a dependency the
        // caller did not ask for. `POST /v2/ask` is the call that finds one.
        Some(plan) if plan.steps.iter().any(|s| s.service.api_slug != source_id) => {
            return Err(problem(StatusCode::UNPROCESSABLE_ENTITY, "invalid_source_input", "These inputs are not enough for this operation on its own. Supply its identifier inputs, or use POST /v2/ask. Nothing was charged."));
        }
        Some(_) => {}
        None => {
            let reason = response.errors.iter().find_map(|e| e["message"].as_str()).unwrap_or("This operation cannot run with these inputs.");
            return Err(problem(StatusCode::UNPROCESSABLE_ENTITY, "invalid_source_input", &format!("{reason} Nothing was charged.")));
        }
    }
    let bearer = token(headers).map_err(|_| no_bearer())?;
    authorize_and_run(state, ctx, bearer, id, max_spend).await?;
    finish(state, ctx, id, wait, limit, None).await
}

// ---------------------------------------------------------------------------
// Ask
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum Mode {
    /// Price the plan; running it is a separate `POST /v2/asks/{id}/run`.
    #[default]
    Plan,
    /// Run at once when the plan's maximum cost is within `max_spend`.
    Auto,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AskRequest {
    #[serde(default)]
    question: Option<String>,
    #[serde(default)]
    workflow: Option<WorkflowInput>,
    #[serde(default)]
    execution_mode: Mode,
    #[serde(default)]
    max_spend: Option<Value>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    wait_seconds: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowInput {
    slug: String,
    input: BTreeMap<String, Value>,
}

/// Free discovery of the same code-built recipes used by the App. Availability
/// means a plan can be built, not that a country or company has complete data.
pub async fn ask_workflows(State(state): State<ServerState>, headers: HeaderMap) -> HttpResponse {
    if let Err(response) = context(&state, &headers, &["sources:read"]).await { return response; }
    match state.engine.catalog.get().await {
        Ok(catalog) => Json(json!({"workflows":crate::workflow_catalog::definitions(&catalog),"notice":"Country availability checks planning only. Review the exact source calls and total ceiling before running; records vary by entity and period."})).into_response(),
        Err(error) => unavailable("workflow catalog", &error),
    }
}

/// `POST /v2/ask`: a question in, a priced plan (and in `auto` mode, results) out.
pub async fn ask(State(state): State<ServerState>, headers: HeaderMap, body: Bytes) -> HttpResponse {
    ask_inner(&state, &headers, &body).await.unwrap_or_else(|r| r)
}
async fn ask_inner(state: &ServerState, headers: &HeaderMap, body: &[u8]) -> Answer {
    let req: AskRequest = parse(body)?;
    let scopes: &[&str] = if req.execution_mode == Mode::Auto { &["ask:create", "ask:execute"] } else { &["ask:create"] };
    let ctx = context(state, headers, scopes).await?;
    if req.question.is_some() == req.workflow.is_some() {
        return Err(problem(StatusCode::BAD_REQUEST, "invalid_request", "Supply exactly one of question or workflow."));
    }
    let question = req.question.as_deref().unwrap_or_default().trim();
    if req.question.is_some() && (question.is_empty() || question.len() > MAX_QUESTION) {
        return Err(problem(StatusCode::BAD_REQUEST, "invalid_request", "question must be 1 to 4000 characters."));
    }
    let max_spend = spend_limit(req.max_spend.as_ref())?;
    if req.execution_mode == Mode::Auto && max_spend.is_none() {
        return Err(problem(StatusCode::BAD_REQUEST, "invalid_request", "max_spend is required with execution_mode \"auto\"."));
    }
    let limit = limit(req.limit)?;
    let request = request_id(headers, &ctx, "ask")?;
    let response = {
        let Ok(_permit) = state.permits.try_acquire() else { return Err(busy()) };
        if let Some(workflow) = req.workflow {
            state.engine.start_workflow(&ctx.principal, &workflow.slug, crate::workflow_catalog::StartRequest { request_id: request, input: workflow.input }).await.map_err(|e| unavailable("workflow ask", &e))?
        } else {
            let discover = DiscoverRequest { request_id: request, question: question.to_owned(), state: None, context_delta: Default::default() };
            state.engine.discover(&ctx.principal, discover).await.map_err(|e| unavailable("ask", &e))?
        }
    };
    if let Some(refusal) = refused_by_engine(&response) {
        return Err(refusal);
    }
    let id = response.state.as_ref().map(|s| s.state_ref).ok_or_else(|| problem(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", "The ask could not be saved. Retry with the same Idempotency-Key."))?;
    let cap = response.proposal.as_ref().map(|p| p.max_total_atomic.parse::<i64>().unwrap_or(i64::MAX));
    if req.execution_mode == Mode::Auto {
        match (cap, max_spend) {
            // Auto never spends above what the caller allowed: it answers with
            // the plan, and the caller decides.
            (Some(cap), Some(max)) if cap > max && !authorized(state, &ctx, id).await.unwrap_or(false) => {
                let note = json!({"code": "max_spend", "message": "The plan's maximum cost is above max_spend, so it was not run. Nothing was charged."});
                return finish(state, &ctx, id, Duration::ZERO, limit, Some(note)).await;
            }
            // Auto mode never buys from a source that has not answered a live
            // call yet. The caller sees the plan and may run it explicitly.
            (Some(_), _) if !authorized(state, &ctx, id).await.unwrap_or(false) && plan_unverified(state, &ctx, id).await => {
                let note = json!({"code": "unverified_source", "message": "The plan uses a source that has not been verified by a live call yet, so auto mode did not run it. Review the plan and run it with POST /v2/asks/{id}/run. Nothing was charged."});
                return finish(state, &ctx, id, Duration::ZERO, limit, Some(note)).await;
            }
            (Some(_), _) => {
                let bearer = token(headers).map_err(|_| no_bearer())?;
                authorize_and_run(state, &ctx, bearer, id, max_spend).await?;
                return finish(state, &ctx, id, wait_for(req.wait_seconds), limit, None).await;
            }
            _ => {}
        }
    }
    finish(state, &ctx, id, Duration::ZERO, limit, None).await
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RunRequest {
    #[serde(default)]
    max_spend: Option<Value>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    wait_seconds: Option<u64>,
}

/// `POST /v2/asks/{id}/run`: approve the priced plan against the caller's
/// limits and run it. Running an ask twice is the same run.
pub async fn run_ask(State(state): State<ServerState>, headers: HeaderMap, Path(id): Path<Uuid>, body: Bytes) -> HttpResponse {
    run_ask_inner(&state, &headers, id, &body).await.unwrap_or_else(|r| r)
}
async fn run_ask_inner(state: &ServerState, headers: &HeaderMap, id: Uuid, body: &[u8]) -> Answer {
    let ctx = context(state, headers, &["ask:execute"]).await?;
    let req: RunRequest = if body.is_empty() { RunRequest::default() } else { parse(body)? };
    let max_spend = spend_limit(req.max_spend.as_ref())?;
    let limit = limit(req.limit)?;
    let bearer = token(headers).map_err(|_| no_bearer())?;
    authorize_and_run(state, &ctx, bearer, id, max_spend).await?;
    finish(state, &ctx, id, wait_for(req.wait_seconds), limit, None).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContinueRequest {
    state: crate::state::Envelope,
    action_id: Uuid,
    input: Value,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    wait_seconds: Option<u64>,
}

/// Continue a saved choice or structured input. The only spending authority
/// available here is the run's existing authorization; a new quote stays unrun.
pub async fn continue_ask(State(state): State<ServerState>, headers: HeaderMap, Path(id): Path<Uuid>, body: Bytes) -> HttpResponse {
    continue_ask_inner(&state, &headers, id, &body).await.unwrap_or_else(|r| r)
}
async fn continue_ask_inner(state: &ServerState, headers: &HeaderMap, id: Uuid, body: &[u8]) -> Answer {
    let ctx = context(state, headers, &["ask:execute"]).await?;
    let req: ContinueRequest = parse(body)?;
    let limit = limit(req.limit)?;
    if req.state.state_ref != id {
        return Err(problem(StatusCode::BAD_REQUEST, "invalid_request", "The continuation state must belong to the ask in the URL."));
    }
    // Check current task access even when replaying a saved request receipt.
    let (_, _, connection) = state.engine.store.load_scoped(&ctx.principal, id).await.map_err(|e| not_found(&e))?;
    if connection != ctx.principal.connection {
        return Err(problem(StatusCode::NOT_FOUND, "run_not_found", "No ask or run with this id belongs to this caller."));
    }
    state.engine.signer.verify(ctx.principal.owner, &req.state, true)
        .map_err(|_| problem(StatusCode::BAD_REQUEST, "invalid_continuation_state", "Copy the continuation state unchanged from this run."))?;
    if !headers.contains_key("idempotency-key") {
        return Err(problem(StatusCode::BAD_REQUEST, "invalid_request", "Idempotency-Key is required when continuing a run."));
    }
    let request = request_id(headers, &ctx, &format!("asks/{id}/continue"))?;
    let bearer = token(headers).map_err(|_| no_bearer())?;
    let response = {
        let Ok(_permit) = state.permits.try_acquire() else { return Err(busy()) };
        state.engine.continue_task(&ctx.principal, bearer, ExecuteRequest {
            request_id: request, action_id: req.action_id, state: req.state,
            quote_ref: None, authorization_ref: None, idempotency_key: request, input: req.input,
        }).await.map_err(|e| unavailable("continue", &e))?
    };
    if let Some(error) = response.errors.first() {
        match error["code"].as_str() {
            Some("invalid_continuation_input") => return Err(problem(StatusCode::UNPROCESSABLE_ENTITY, "invalid_continuation_input", "Input must match the returned action schema and candidate identifiers.")),
            Some("run_cancelled") => return Err(problem(StatusCode::CONFLICT, "run_cancelled", "This run was cancelled and cannot be continued.")),
            Some("plan_expired") => return Err(problem(StatusCode::CONFLICT, "plan_expired", "The original quote expired. Saved results remain available; no source call was repeated.")),
            Some("continuation_busy" | "continuation_not_available") => return Err(problem(StatusCode::CONFLICT, "continuation_changed", "Read the run and use its current continuation state and action.")),
            _ => {}
        }
    }
    if response.status == Status::StateConflict {
        return Err(problem(StatusCode::CONFLICT, "continuation_changed", "The state expired or the run changed. Read the run and use its current continuation."));
    }
    if let Some(refusal) = refused_by_engine(&response) {
        return Err(refusal);
    }
    // This reuses the original authorization and journal, including on retry.
    // Supply may have generated a new quote: start_worker refuses it until
    // the caller explicitly approves through /run.
    if let Err(error) = state.engine.start_worker(ctx.principal.clone(), bearer.to_owned(), id).await {
        tracing::warn!(task = %id, error = %error, "continued public api run could not resume");
    }
    finish(state, &ctx, id, wait_for(req.wait_seconds), limit, None).await
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct StatusQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    wait_seconds: Option<u64>,
}

/// `GET /v2/asks/{id}` and `GET /v2/runs/{id}`: the current state. An
/// approved run whose worker stopped is resumed from its saved journal.
pub async fn run_status(State(state): State<ServerState>, headers: HeaderMap, Path(id): Path<Uuid>, Query(query): Query<StatusQuery>) -> HttpResponse {
    let ctx = match context(&state, &headers, &[]).await {
        Ok(ctx) => ctx,
        Err(r) => return r,
    };
    let limit = match limit(query.limit) {
        Ok(limit) => limit,
        Err(r) => return r,
    };
    if let Ok(bearer) = token(&headers) {
        if authorized(&state, &ctx, id).await.unwrap_or(false) && !state.engine.worker_active(id).await.unwrap_or(true) {
            if let Err(error) = state.engine.start_worker(ctx.principal.clone(), bearer.to_owned(), id).await {
                tracing::warn!(task = %id, error = %error, "public api run could not resume");
            }
        }
    }
    let wait = query.wait_seconds.map_or(Duration::ZERO, |s| wait_for(Some(s)));
    finish(&state, &ctx, id, wait, limit, None).await.unwrap_or_else(|r| r)
}

// ---------------------------------------------------------------------------
// Running: the same approval and worker as the App and MCP
// ---------------------------------------------------------------------------

/// Whether the task's CURRENT plan has an authorization.
async fn authorized(state: &ServerState, ctx: &AuthContext, id: Uuid) -> anyhow::Result<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM authorizations a JOIN tasks t ON t.id=a.task_id WHERE a.task_id=$1 AND a.owner_id=$2 AND a.revoked_at IS NULL AND a.quote_ref::text=t.document #>> '{proposal,quote_ref}')")
        .bind(id)
        .bind(ctx.principal.owner)
        .fetch_one(&state.engine.store.pool)
        .await?)
}

/// Whether the task's current plan buys from a source without a live response yet.
async fn plan_unverified(state: &ServerState, ctx: &AuthContext, id: Uuid) -> bool {
    let Ok((_, task, _)) = state.engine.store.load_scoped(&ctx.principal, id).await else { return false };
    task.proposal
        .and_then(|p| serde_json::from_value::<Plan>(p).ok())
        .is_some_and(|plan| plan.steps.iter().any(|s| s.service.unverified))
}

/// Approve the task's current plan, then hand it to the server worker.
/// A connection (every organisation key is one) approves within its own
/// budget: per request, the month, and the balance. A person approves as in
/// the App. A repeated call finds the existing authorization and approves
/// nothing new.
async fn authorize_and_run(state: &ServerState, ctx: &AuthContext, bearer: &str, id: Uuid, max_spend: Option<i64>) -> Result<(), HttpResponse> {
    let p = &ctx.principal;
    let (revision, task, _) = state.engine.store.load_scoped(p, id).await.map_err(|e| not_found(&e))?;
    if !authorized(state, ctx, id).await.map_err(|e| unavailable("authorization", &e))? {
        let Some(plan) = task.proposal.clone().map(serde_json::from_value::<Plan>).transpose().map_err(|e| unavailable("plan", &e.into()))? else {
            return Err(problem(StatusCode::CONFLICT, "plan_not_ready", "This ask has no priced plan to run yet. Read its status for the input it needs."));
        };
        if let Some(max) = max_spend.filter(|max| plan.cap_atomic > *max) {
            return Err(problem_with(StatusCode::PAYMENT_REQUIRED, "spend_limit_exceeded", "The plan's maximum cost is above max_spend. Nothing was charged.",
                json!({"limit": "max_spend", "required": money(plan.cap_atomic), "available": money(max)})));
        }
        let cap = plan.cap_atomic.to_string();
        let approved = if p.connection.is_some() {
            state.engine.executor.approve_connection(p, id, revision, plan.quote_ref, &cap).await.map(|_| ())
        } else {
            state.engine.executor.approve(p, id, plan.quote_ref, &cap).await.map(|_| ())
        };
        approved.map_err(|error| approval_refused(ctx, &error))?;
    }
    if let Err(error) = state.engine.start_worker(p.clone(), bearer.to_owned(), id).await {
        // The approval is saved; `GET /v2/runs/{id}` resumes the worker.
        tracing::warn!(task = %id, error = %error, "approved public api run could not start");
    }
    Ok(())
}

fn approval_refused(ctx: &AuthContext, error: &anyhow::Error) -> HttpResponse {
    if let Some(refusal) = error.downcast_ref::<crate::approval_feedback::BudgetRefusal>() {
        let (kind, required, available) = refusal.parts();
        let amounts = |limit: &str| json!({"limit": limit, "required": money(required), "available": money(available)});
        return match kind {
            "per_request" => problem_with(StatusCode::PAYMENT_REQUIRED, "spend_limit_exceeded", "The plan's maximum cost is above the per-request limit. Nothing was charged.", amounts("per_request")),
            // An organisation key's rolling day equals its month, so the month decides.
            "daily" if ctx.api_key.is_some() => problem_with(StatusCode::PAYMENT_REQUIRED, "spend_limit_exceeded", "The plan's maximum cost is above what this key may still spend this month. Nothing was charged.", amounts("monthly")),
            "daily" => problem_with(StatusCode::PAYMENT_REQUIRED, "spend_limit_exceeded", "The plan's maximum cost is above this connection's remaining daily budget. Nothing was charged.", amounts("daily")),
            _ => problem_with(StatusCode::PAYMENT_REQUIRED, "insufficient_balance", "The balance is too low for this plan's maximum cost. Top up in Apiosk > Balance. Nothing was charged.",
                json!({"required": money(required), "available": money(available)})),
        };
    }
    match error.to_string().as_str() {
        "quote changed or expired" => problem(StatusCode::CONFLICT, "plan_expired", "This plan's price has expired. Ask again for a new plan. Nothing was charged."),
        "state changed; check status before approving" => problem(StatusCode::CONFLICT, "plan_changed", "The plan changed while it was being approved. Read its status and retry."),
        "task cancelled" | "authorization revoked" => problem(StatusCode::CONFLICT, "run_cancelled", "This run was cancelled and is not restarted. Ask again for a new plan."),
        "purchases unavailable" => problem(StatusCode::SERVICE_UNAVAILABLE, "source_unavailable", "Purchases are temporarily unavailable. Nothing was charged."),
        "workspace spending is not permitted" => problem(StatusCode::FORBIDDEN, "spending_not_permitted", "This caller may not spend in this workspace."),
        "set a positive per-request and daily limit in Connections, then reconnect if needed" => problem(StatusCode::PAYMENT_REQUIRED, "spend_limit_exceeded", "This connection has no spending limits. Set them in Apiosk > Connections. Nothing was charged."),
        other => {
            tracing::warn!(error = %other, "public api approval unconfirmed");
            problem(StatusCode::SERVICE_UNAVAILABLE, "approval_unconfirmed", "The approval could not be confirmed. Read the run's status before retrying; do not start a new request.")
        }
    }
}

// ---------------------------------------------------------------------------
// The one response shape for asks and runs
// ---------------------------------------------------------------------------

fn public_status(r: &Response, authorized: bool) -> &'static str {
    match r.status {
        Status::Ready | Status::RequiresApproval if authorized => "running",
        Status::Ready | Status::RequiresApproval => "requires_approval",
        Status::Running => "running",
        Status::Succeeded => "completed",
        Status::Partial => "partial",
        Status::NeedsInput => "needs_input",
        Status::NeedsSelection => "needs_selection",
        Status::Unsupported => "unsupported",
        Status::Cancelled => "cancelled",
        Status::Failed | Status::StateConflict => "failed",
    }
}

/// Wait for the run to finish, then describe it once.
///
/// The wait watches the execution lease, which is one small query, rather
/// than rebuilding the whole view each time: a direct call answers as soon as
/// its source does, instead of on the next poll tick. A run still going
/// answers `202` with its id to poll; a failed one `502`.
async fn finish(state: &ServerState, ctx: &AuthContext, id: Uuid, wait: Duration, limit: Option<usize>, note: Option<Value>) -> Answer {
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline && state.engine.worker_active(id).await.unwrap_or(false) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let response = state.engine.view(&ctx.principal, id, Uuid::new_v4(), None).await.map_err(|e| not_found(&e))?;
    let authorized = authorized(state, ctx, id).await.map_err(|e| unavailable("authorization", &e))?;
    let status = public_status(&response, authorized);
    let mut body = describe(state, ctx, id, &response, status, limit).await.map_err(|e| unavailable("describe", &e))?;
    if let Some(note) = note {
        body["approval_required_reason"] = note;
    }
    let code = match status {
        "running" => StatusCode::ACCEPTED,
        "failed" => StatusCode::BAD_GATEWAY,
        _ => StatusCode::OK,
    };
    Ok((code, Json(body)).into_response())
}

async fn describe(state: &ServerState, ctx: &AuthContext, id: Uuid, r: &Response, status: &str, limit: Option<usize>) -> anyhow::Result<Value> {
    let (_, task, _) = state.engine.store.load_scoped(&ctx.principal, id).await?;
    let plan: Option<Plan> = task.proposal.clone().map(serde_json::from_value).transpose()?;
    let mut results = r.context_view["results"].as_array().cloned().unwrap_or_default();
    if let Some(limit) = limit {
        results.truncate(limit);
    }
    for result in &mut results {
        result["metadata"] = result_metadata(result, &task, plan.as_ref());
    }
    let usage = match &plan {
        Some(plan) => run_usage(state, ctx, id, plan).await?,
        None => json!({"sources": [], "cost": money(0), "held": money(0)}),
    };
    let mut body = json!({
        "id": id,
        "status": status,
        "question": task.conversation.last().map(|turn| &turn.question),
        "plan": plan.as_ref().map(plan_json),
        "answer": r.context_view["analysis"],
        "results": results,
        "usage": usage,
        "balance": { "remaining": r.billing.as_ref().and_then(|b| micro(&b["balance_available"])).map(money) },
        "errors": r.errors,
    });
    // A direct call asks one source one thing; its document is the answer.
    if let [single] = results.as_slice() {
        body["data"] = single["data"].clone();
        body["metadata"] = single["metadata"].clone();
    }
    match status {
        "needs_input" => {
            body["needs_input"] = json!(r.next_actions.iter().filter(|a| matches!(a.kind.as_str(), "supply_input" | "upload_document")).map(|a| json!({"label": a.label, "schema": a.input_schema})).collect::<Vec<_>>());
            // A clarification has no form to fill; its question is the reply.
            body["message"] = json!(task.conversation.last().and_then(|turn| turn.reply.as_deref()));
        }
        "needs_selection" => body["candidates"] = r.context_view["candidates"].clone(),
        "running" => body["retry_after_ms"] = json!(r.retry_after_ms.unwrap_or(2000)),
        "failed" => {
            let code = r.errors.iter().filter_map(|e| e["code"].as_str()).find_map(|code| {
                if code.contains("timeout") {
                    Some("source_timeout")
                } else if code.contains("unavailable") {
                    Some("source_unavailable")
                } else {
                    None
                }
            });
            body["error"] = json!({"code": code.unwrap_or("source_error"), "message": "The source could not complete this run. Anything not delivered was refunded or is held until reconciled; see usage."});
        }
        _ => {}
    }
    if matches!(status, "needs_input" | "needs_selection") {
        let actions: Vec<_> = r.next_actions.iter().filter(|a| matches!(a.kind.as_str(), "supply_input" | "select_entity")).collect();
        if !actions.is_empty() {
            body["continuation"] = json!({"state": r.state, "actions": actions});
        }
    }
    Ok(body)
}

/// Fact types that identify a subject rather than describe it.
fn is_identifier(fact_type: &str) -> bool {
    const MARKERS: [&str; 14] = ["kvk", "vestigingsnummer", "rsin", "lei", "siren", "siret", "national_id", "companies_house",
        "vat", "publicatieid", "isin", "ticker", "registration", "cvr"];
    let lower = fact_type.to_ascii_lowercase();
    MARKERS.iter().any(|marker| lower.contains(marker))
}

/// Where a result came from, when, about what, and what it cost — in one
/// fixed shape, so an agent never has to learn each source's own document.
fn result_metadata(result: &Value, task: &crate::state::Task, plan: Option<&Plan>) -> Value {
    let source = &result["source"];
    let reference = result["result_ref"].as_str().unwrap_or_default();
    let endpoint = source["endpoint_id"].as_str().and_then(|id| id.parse::<Uuid>().ok());
    let step = plan.and_then(|plan| plan.steps.iter().find(|s| Some(s.service.endpoint_id) == endpoint));
    let subject = result["subject"]["entity_ref"].as_str();
    let mut identifiers = serde_json::Map::new();
    for fact in &task.facts {
        let from_this = fact.provenance["result_ref"].as_str() == Some(reference);
        let asked_for = fact.provenance["kind"] == "user_input" && subject == Some(fact.entity_ref.to_string().as_str());
        if (from_this || asked_for) && is_identifier(&fact.semantic_type) && fact.value.is_string() {
            identifiers.entry(fact.semantic_type.clone()).or_insert_with(|| fact.value.clone());
        }
    }
    // The record's own addresses — an announcement, an article, a register
    // profile — as the source published them inside its data.
    let mut links: Vec<String> = Vec::new();
    collect_links(&result["data"], &mut links);
    json!({
        "source_id": source["provider"],
        "source_name": source["name"],
        "operation": step.map(|s| s.service.contract.capability_slug.as_str()),
        "retrieved_at": source["retrieved_at"],
        "source_url": source["url"],
        "links": links,
        "subject": result["subject"]["label"],
        "identifiers": identifiers,
        "cost": micro(&result["charged_atomic"]).map(money),
        "receipt_ref": result["receipt_ref"],
    })
}

/// Up to ten distinct http(s) links found in a source document, in order.
fn collect_links(value: &Value, links: &mut Vec<String>) {
    if links.len() >= 10 {
        return;
    }
    match value {
        Value::String(text) if (text.starts_with("https://") || text.starts_with("http://")) && text.len() <= 2048 && !text.contains(char::is_whitespace) => {
            if !links.iter().any(|link| link == text) {
                links.push(text.clone());
            }
        }
        Value::Array(items) => items.iter().for_each(|item| collect_links(item, links)),
        Value::Object(fields) => fields.values().for_each(|field| collect_links(field, links)),
        _ => {}
    }
}

fn plan_json(plan: &Plan) -> Value {
    json!({
        "quote_ref": plan.quote_ref,
        "expires_at": plan.expires_at,
        "steps": plan.steps.iter().map(|step| json!({
            "source_id": step.service.api_slug,
            "operation": step.service.contract.capability_slug,
            "name": step.service.name,
            "conditional": step.browser_fallback.is_some(),
            // false: the source has not answered a live call yet; auto mode
            // will not run it, an explicit run may.
            "verified": !step.service.unverified,
            "estimated_cost": money(plan.step_total(step)),
        })).collect::<Vec<_>>(),
        "estimated_total": money(plan.cap_atomic),
    })
}

/// What a run actually cost, per source, from the ledger. Captured charges
/// are `cost`; reservations not settled yet are `held`; refunds are neither.
async fn run_usage(state: &ServerState, ctx: &AuthContext, id: Uuid, plan: &Plan) -> anyhow::Result<Value> {
    let rows: Vec<(Uuid, Option<String>, Option<i64>, i64)> = sqlx::query_as("SELECT s.candidate_id,c.status,c.charged_micro_usd,e.reserved_atomic FROM executions e JOIN authorizations a ON a.id=e.authorization_id JOIN user_selected_apis s ON s.id=e.selection_id LEFT JOIN balance_charges c ON c.user_id=e.owner_id AND c.idempotency_key=e.id::text WHERE e.owner_id=$1 AND e.task_id=$2 AND a.quote_ref=$3 ORDER BY e.created_at,e.id")
        .bind(ctx.principal.owner)
        .bind(id)
        .bind(plan.quote_ref)
        .fetch_all(&state.engine.store.pool)
        .await?;
    let mut sources: BTreeMap<(String, String), (u32, i64)> = BTreeMap::new();
    let (mut total, mut held) = (0i64, 0i64);
    for (endpoint, ledger, charged, reserved) in rows {
        match ledger.as_deref() {
            Some("charged") => {
                let charged = charged.unwrap_or(0);
                let service = plan.steps.iter().map(|s| &s.service).find(|s| s.endpoint_id == endpoint);
                let key = service.map_or_else(|| ("unknown".to_owned(), String::new()), |s| (s.api_slug.clone(), s.contract.capability_slug.clone()));
                let entry = sources.entry(key).or_default();
                entry.0 += 1;
                entry.1 += charged;
                total += charged;
            }
            Some("refunded") | None => {}
            Some(_) => held += charged.unwrap_or(reserved),
        }
    }
    Ok(json!({
        "sources": sources.into_iter().map(|((source_id, operation), (calls, cost))| json!({"source_id": source_id, "operation": operation, "calls": calls, "cost": money(cost)})).collect::<Vec<_>>(),
        "cost": money(total),
        "held": money(held),
    }))
}

// ---------------------------------------------------------------------------
// Balance and usage
// ---------------------------------------------------------------------------

async fn key_usage(state: &ServerState, key: Uuid, limit: i32) -> anyhow::Result<Value> {
    Ok(sqlx::query_scalar("SELECT public.organisation_api_key_usage($1,$2)")
        .bind(key)
        .bind(limit)
        .fetch_one(&state.engine.store.pool)
        .await?)
}

/// `GET /v2/balance`: the organisation's balance and what this key may still spend.
pub async fn balance(State(state): State<ServerState>, headers: HeaderMap) -> HttpResponse {
    let ctx = match context(&state, &headers, &["billing:read"]).await {
        Ok(ctx) => ctx,
        Err(r) => return r,
    };
    if let Some(key) = &ctx.api_key {
        let usage = match key_usage(&state, key.id, 1).await {
            Ok(usage) => usage,
            Err(error) => return unavailable("balance", &error),
        };
        let spent = micro(&usage["month_spent_micro_usd"]).unwrap_or(0);
        return Json(json!({
            "organisation_id": key.organisation_id,
            "balance": { "available": micro(&usage["balance_micro_usd"]).map(money) },
            "key": { "id": key.id, "name": key.name, "scopes": key.scopes, "expires_at": key.expires_at },
            "limits": {
                "per_request": money(key.per_request_atomic),
                "monthly": money(key.monthly_atomic),
                "month_spent": money(spent),
                "month_remaining": money((key.monthly_atomic - spent).max(0)),
                "month_start": usage["month_start"],
            },
        }))
        .into_response();
    }
    let p = &ctx.principal;
    let available: Result<Option<i64>, sqlx::Error> = sqlx::query_scalar("SELECT balance_micro_usd FROM user_balances WHERE account_id=$1")
        .bind(p.workspace.unwrap_or(p.owner))
        .fetch_optional(&state.engine.store.pool)
        .await;
    match available {
        Ok(available) => Json(json!({ "balance": { "available": available.map(money) } })).into_response(),
        Err(error) => unavailable("balance", &error.into()),
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct UsageQuery {
    #[serde(default)]
    limit: Option<i32>,
}

/// `GET /v2/usage`: this key's spending this month and its recent charges.
pub async fn usage(State(state): State<ServerState>, headers: HeaderMap, Query(query): Query<UsageQuery>) -> HttpResponse {
    let ctx = match context(&state, &headers, &["billing:read"]).await {
        Ok(ctx) => ctx,
        Err(r) => return r,
    };
    let Some(key) = &ctx.api_key else {
        return problem(StatusCode::FORBIDDEN, "api_key_required", "Per-key usage is for organisation API keys. People see their usage in Apiosk > Usage.");
    };
    let usage = match key_usage(&state, key.id, query.limit.unwrap_or(50).clamp(1, 200)).await {
        Ok(usage) => usage,
        Err(error) => return unavailable("usage", &error),
    };
    let charges: Vec<Value> = usage["charges"].as_array().into_iter().flatten().map(|c| json!({
        "id": c["id"],
        "status": c["status"],
        "cost": money(micro(&c["charged_micro_usd"]).unwrap_or(0)),
        "created_at": c["created_at"],
        "source_id": c["source_id"],
        "operation": c["operation"],
        "name": c["name"],
        "run_id": c["task_id"],
    })).collect();
    Json(json!({
        "key_id": key.id,
        "organisation_id": key.organisation_id,
        "month_start": usage["month_start"],
        "month_spent": money(micro(&usage["month_spent_micro_usd"]).unwrap_or(0)),
        "charges": charges,
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_keeps_the_exact_ledger_amount_beside_the_decimal() {
        assert_eq!(money(23000), json!({"amount": 0.023, "amount_micro_usd": "23000", "currency": "USD"}));
        assert_eq!(money(0)["amount"], json!(0.0));
    }

    #[test]
    fn max_spend_accepts_numbers_and_decimal_strings_only() {
        assert_eq!(spend_limit(Some(&json!(1.0))).ok(), Some(Some(1_000_000)));
        assert_eq!(spend_limit(Some(&json!("0.54"))).ok(), Some(Some(540_000)));
        assert_eq!(spend_limit(None).ok(), Some(None));
        for bad in [json!(-1), json!("abc"), json!(true), json!(1e9)] {
            assert_eq!(spend_limit(Some(&bad)).unwrap_err().status(), StatusCode::BAD_REQUEST);
        }
    }

    #[test]
    fn idempotency_keys_are_scoped_to_caller_and_route() {
        let principal = crate::auth::Principal { owner: Uuid::new_v4(), connection: Some(Uuid::new_v4()), workspace: None };
        let ctx = AuthContext { principal: principal.clone(), api_key: None };
        let other = AuthContext { principal: crate::auth::Principal { connection: Some(Uuid::new_v4()), ..principal }, api_key: None };
        let mut headers = HeaderMap::new();
        headers.insert("idempotency-key", "nomi-request-983472".parse().unwrap());
        let first = request_id(&headers, &ctx, "ask").unwrap();
        assert_eq!(first, request_id(&headers, &ctx, "ask").unwrap());
        assert_ne!(first, request_id(&headers, &ctx, "sources/kvk/run").unwrap());
        assert_ne!(first, request_id(&headers, &other, "ask").unwrap());
        assert_ne!(request_id(&HeaderMap::new(), &ctx, "ask").unwrap(), request_id(&HeaderMap::new(), &ctx, "ask").unwrap());
        headers.insert("idempotency-key", "with space".parse().unwrap());
        assert_eq!(request_id(&headers, &ctx, "ask").unwrap_err().status(), StatusCode::BAD_REQUEST);
    }

    fn fixture() -> Catalog {
        Catalog::build(serde_json::from_str(include_str!("../tests/fixtures/integration-catalog.json")).unwrap())
    }
    fn source(slug: &str, provider: Option<&str>, name: &str) -> crate::sources::Source {
        serde_json::from_value(json!({"slug": slug, "provider_slug": provider, "name": name, "description": "", "category": "", "tags": [], "sectors": [], "endpoint_count": 1})).unwrap()
    }

    #[test]
    fn a_grouped_provider_is_a_source_id_like_any_other() {
        let mut catalog = fixture();
        catalog.services[0].api_slug = "signalpulse".into();
        catalog.services[1].api_slug = "macropulse".into();
        let sources = vec![source("signalpulse", Some("pulsenetwork"), "SignalPulse"), source("macropulse", Some("pulsenetwork"), "MacroPulse")];
        let (id, slugs) = members(&catalog, &sources, "pulsenetwork").unwrap();
        assert_eq!(id, "pulsenetwork");
        assert_eq!(slugs.len(), 2);
        assert_eq!(members(&catalog, &sources, "Pulse Network").unwrap().1.len(), 2);
        assert_eq!(members(&catalog, &sources, "SignalPulse").unwrap(), ("signalpulse".to_owned(), vec!["signalpulse".to_owned()]));
        assert!(members(&catalog, &sources, "nope").is_none());
        // An operation of the group runs on the member that offers it.
        let operation = catalog.services[0].contract.capability_slug.clone();
        let input = catalog.services[0].contract.requires[0].fact_type.clone();
        let body = serde_json::Map::from_iter([(input, json!("x"))]);
        let (service, _) = resolve(&catalog, &sources, "pulsenetwork", &operation, &body).unwrap();
        assert_eq!(service.api_slug, "signalpulse");
    }

    #[test]
    fn a_call_runs_the_variant_whose_inputs_it_supplies() {
        let catalog = fixture();
        let (by_name, by_id) = (&catalog.services[0], &catalog.services[1]);
        let mut id_variant = by_id.clone();
        id_variant.contract.capability_slug = by_name.contract.capability_slug.clone();
        let candidates = [by_name, &id_variant];
        let id_field = id_variant.contract.requires[0].fact_type.clone();
        let (chosen, bound) = choose(&candidates, &serde_json::Map::from_iter([(id_field.clone(), json!("123"))])).unwrap();
        assert_eq!(chosen.endpoint_id, id_variant.endpoint_id);
        assert!(bound.contains_key(&id_field));
        let refused = choose(&candidates, &serde_json::Map::new()).unwrap_err();
        assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY, "no variant fits: each one's inputs are listed");
    }

    #[test]
    fn record_links_are_collected_once_and_bounded() {
        let mut links = Vec::new();
        collect_links(&json!({"a": "https://x.test/1", "b": [{"c": "https://x.test/2"}, "https://x.test/1", "not a link", "https://has space.test"]}), &mut links);
        assert_eq!(links, vec!["https://x.test/1", "https://x.test/2"]);
        let many = json!((0..30).map(|i| format!("https://x.test/{i}")).collect::<Vec<_>>());
        let mut bounded = Vec::new();
        collect_links(&many, &mut bounded);
        assert_eq!(bounded.len(), 10);
    }

    #[test]
    fn an_approved_plan_is_running_not_awaiting_approval() {
        let ready = Response::new(Uuid::new_v4(), Status::Ready);
        assert_eq!(public_status(&ready, false), "requires_approval");
        assert_eq!(public_status(&ready, true), "running");
        assert_eq!(public_status(&Response::new(Uuid::new_v4(), Status::Succeeded), true), "completed");
        assert_eq!(public_status(&Response::new(Uuid::new_v4(), Status::StateConflict), false), "failed");
    }
}
