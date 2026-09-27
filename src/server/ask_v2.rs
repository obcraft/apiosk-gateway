//! Standalone capability discovery: no task, quote, execution or planner state.
use super::{app_person, failure, ServerState};
use crate::capability_intake::{self, Match, ParsedRequest};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};
use uuid::Uuid;

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
