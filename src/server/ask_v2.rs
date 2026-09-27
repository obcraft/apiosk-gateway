//! Capability discovery and explicit endpoint preparation. Parsing and search
//! never create tasks. Preparation saves a quote; existing task approval owns
//! every paid action.
use super::{app_person, failure, ServerState};
use crate::capability_intake::{self, Match, ParsedRequest};
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

/// Inputs and prices come from the current executable contract, never from a
/// directory card or the model's parsed metadata. This does not create a task.
pub(super) async fn endpoint(
    State(state): State<ServerState>, headers: HeaderMap, Path(id): Path<Uuid>, Query(query): Query<EndpointQuery>,
) -> Response {
    if let Err(response) = app_person(&state, &headers).await { return response; }
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

fn endpoint_descriptor(catalog: &crate::catalog::Catalog, service: &crate::catalog::Service, source: Option<&crate::sources::Source>) -> Value {
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
    let lookup_fields = crate::engine::Engine::endpoint_lookup_fields(catalog, service);
    json!({
        "endpoint_id":service.endpoint_id,"capability":service.contract.capability_slug,"name":service.name,"description":service.description,
        "source":{"slug":service.api_slug,"name":source.map(|s| s.name.as_str()).unwrap_or(&service.api_slug),
            "logo_url":source.and_then(|s| s.logo_url.as_deref().or(s.provider_logo_url.as_deref())),"url":service.source_url},
        "inputs":inputs,
        "lookup":if lookup_fields.is_empty() { Value::Null } else {
            json!({"field":"company.name","resolves":lookup_fields,
                "schema":catalog.vocabulary.get("company.name").map(|fact| &fact.value_schema)})
        },
        "price":{"currency":"USDC","provider_atomic":service.list_atomic.to_string(),"buyer_atomic":service.total().to_string()}
    })
}

pub(super) async fn prepare(
    State(state): State<ServerState>, headers: HeaderMap, Json(request): Json<PrepareRequest>,
) -> Response {
    let person = match app_person(&state, &headers).await { Ok(p) => p, Err(response) => return response };
    if !request.valid() {
        return failure(StatusCode::BAD_REQUEST, "invalid_endpoint_input", "Choose an endpoint and review its input fields. Nothing was charged.");
    }
    let Ok(permit) = state.permits.clone().try_acquire_owned() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "busy", "The service is busy. Your selected source and inputs are kept; try again shortly.");
    };
    let engine = state.engine.clone();
    // Finish saving the quote/receipt if the browser disconnects. Repeating
    // this request ID can recover it, and can never authorize a purchase.
    match crate::telemetry::spawn(async move {
        let _permit = permit;
        engine.prepare_endpoint(&person, request.request_id, request.endpoint_id, request.capability.as_deref(), &request.input).await
    }).await {
        Ok(Ok(response)) => Json(response).into_response(),
        _ => failure(StatusCode::SERVICE_UNAVAILABLE, "prepare_unavailable", "The quote could not be confirmed. Your inputs are kept; retry this request to recover it."),
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
        .parse(
            request.question.trim(),
            &request.known_slugs,
            request.correction.as_deref(),
        )
        .await
    {
        Ok(output) => {
            tracing::info!(request_id=%request.request_id, model=%output.model, parse_ms=output.timing.parse_ms, "Ask v2 capabilities parsed");
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

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Origin {
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
                && slugs.insert(c.slug.as_str())
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
    let person = match app_person(&state, &headers).await {
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
    let started = Instant::now();
    let (matches, count, version) = match request.origin {
        Origin::Apiosk => {
            let catalog = match state.engine.catalog.get().await {
                Ok(c) => c,
                Err(_) => {
                    return failure(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "catalog_unavailable",
                        "Apiosk sources are temporarily unavailable. Retry the source search.",
                    )
                }
            };
            let matches = crate::ask_v2::search::search(&request.parsed_request, &catalog);
            let mut matches = serde_json::to_value(matches).unwrap_or_else(|_| json!([]));
            // Branding is cached separately from contracts. A slow directory
            // refresh must not hold up capability matches indefinitely.
            if let Ok(Ok(sources)) =
                tokio::time::timeout(Duration::from_secs(1), state.engine.catalog.sources()).await
            {
                brand_matches(&mut matches, &sources);
            }
            (
                matches,
                capability_intake::source_count(&catalog),
                catalog.version.clone(),
            )
        }
        Origin::Coinbase => {
            let mut matches: Vec<_> =
                capability_intake::queries(&request.parsed_request, &crate::state::Task::default())
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
                capability_intake::BAZAAR_SEARCH,
            )
            .await;
            crate::ask_v2::search::filter_coinbase(&request.parsed_request, &mut matches);
            // Compare buyer totals in the table. This is still a discovery
            // estimate; the selected offer is independently resolved at quote.
            for candidate in matches.iter_mut().flat_map(|found| &mut found.coinbase) {
                candidate.price = candidate.price.as_deref().and_then(buyer_discovery_price);
            }
            (
                serde_json::to_value(matches).unwrap_or_else(|_| json!([])),
                0,
                String::new(),
            )
        }
    };
    let search_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        origin = if count > 0 { "apiosk" } else { "coinbase" },
        search_ms,
        sources_searched = count,
        "Ask v2 sources searched"
    );
    Json(json!({"matches":matches,"apiosk_sources_searched":count,"catalog_version":version,"timing":{"search_ms":search_ms}})).into_response()
}

fn buyer_discovery_price(price: &str) -> Option<String> {
    let (whole, fraction) = price.strip_suffix(" USD")?.split_once('.')?;
    if fraction.len() != 6 || !whole.bytes().chain(fraction.bytes()).all(|b| b.is_ascii_digit()) { return None; }
    let provider = whole.parse::<i64>().ok()?.checked_mul(1_000_000)?.checked_add(fraction.parse::<i64>().ok()?)?;
    if !(0..1_000_000_000_000).contains(&provider) { return None; }
    let total = catalog_core::fees::quote(provider.into(), catalog_core::fees::ProviderClass::Federated).buyer_total_atomic;
    Some(format!("{}.{:06} USD", total / 1_000_000, total % 1_000_000))
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
