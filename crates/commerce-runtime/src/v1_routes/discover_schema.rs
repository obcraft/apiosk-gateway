//! `GET /v1/discover/schema` — the shape a caller should turn a question into.
//!
//! # Why this exists
//!
//! `/v1/discover?q=<a sentence>` reads the sentence with a model, works out the
//! needs and the search terms, and then searches. That parse is the expensive
//! step and the fragile one: it costs a model call per question, it is rationed
//! per account (`crate::parse_budget`), and it is a second reading of something
//! the caller very often already understands perfectly well.
//!
//! When the caller is itself a language model — a chatbot holding this
//! gateway's tools over MCP or WebMCP — that reading is free at the other end
//! and better informed, because the chatbot has the conversation and this
//! endpoint has one sentence. What it lacks is the vocabulary: it cannot know
//! that this catalogue files "the annual accounts of a Dutch company" under
//! `company.financial-statements` rather than under `financial-statements`, and
//! a capability slug it invents matches nothing at all.
//!
//! So this route publishes the vocabulary, as a JSON Schema the caller can
//! register as its own tool's input schema. The agent then arrives already
//! speaking the language:
//!
//! ```text
//!   question  ->  the agent's own reading  ->  a discovery request in OUR words
//! ```
//!
//! Measured against the live catalogue, the difference is not marginal. The
//! sentence "Who owns Mollie, is it financially healthy, and who is the CTO?"
//! unparsed returns a compliance-news listing first; the same question sent as
//! `capability=company.financial-statements&parse=false` returns the Dutch
//! business register first, in less time and for no model call at all.
//!
//! # What this route is NOT
//!
//! It changes nothing about `/v1/discover`. Every parameter named in `mapping`
//! below already exists and already behaves exactly as it does today; this
//! endpoint only writes down which field of a normalised request becomes which
//! parameter, so that WebMCP, the MCP server, the app and any API client
//! translate it the same way instead of three times differently.
//!
//! It is additive in the strict sense: no existing route, response or default
//! is touched, and a caller that never fetches this keeps working.

use std::sync::Arc;

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

use crate::db::CapabilityInfo;
use crate::AppState;

/// How long a fetched schema may be considered current.
///
/// The vocabulary moves when the catalogue does, which is daily at most. An
/// agent that registers its tools once per session and caches for an hour is
/// never meaningfully out of date, and the enum below is a database read.
const CACHE_SECONDS: u32 = 3600;

/// Capabilities nothing can currently serve are left out of the enum.
///
/// A slug with no offer behind it is a promise the catalogue cannot keep: an
/// agent that picks it gets an empty answer and no way to tell "nobody sells
/// this" from "I chose the wrong word". Ten of them exist today. They stay in
/// `/v1/capabilities`, which is a description of the taxonomy; they do not
/// belong in a schema whose whole job is to be pickable.
fn is_servable(info: &CapabilityInfo) -> bool {
    info.offer_count > 0
}

/// One capability, as an agent choosing between them needs to see it.
///
/// Not `capability_json` from `/v1/capabilities`: that one carries the input
/// and output contracts and a compare URL, which is the right answer for a
/// catalogue browser and several kilobytes of noise inside a tool schema that
/// is sent on every single agent turn.
fn choice(info: &CapabilityInfo) -> Value {
    json!({
        "slug": info.slug,
        "name": info.name,
        "category": info.category,
        "offers": info.offer_count,
        "listings": info.listing_count,
        "price_usd": {
            "min": info.min_price_usd,
            "max": info.max_price_usd,
        },
    })
}

/// The JSON Schema itself, with the live vocabulary baked into it.
fn request_schema(slugs: Vec<String>) -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "ApioskDiscoveryRequest",
        "description":
            "A question, already read. Turn the person's information need into this \
             object and send it to Apiosk discovery. Apiosk searches; it does not \
             re-read the question. Nothing here spends money.",
        "type": "object",
        "additionalProperties": false,
        "required": ["query"],
        "properties": {
            "query": {
                "type": "string",
                "minLength": 3,
                "maxLength": 400,
                "description":
                    "The information need in one line, in your own words rather than \
                     the person's. Used for ranking and shown back in the answer.",
            },
            "capabilities": {
                "type": "array",
                "maxItems": 4,
                "items": { "type": "string", "enum": slugs },
                "description":
                    "What kind of work is needed, in Apiosk's own vocabulary. Pick only \
                     from this list — a slug outside it matches nothing. One question \
                     may need several (ownership, accounts and leadership are three).",
            },
            "terms": {
                "type": "array",
                "maxItems": 4,
                "items": {
                    "type": "array",
                    "maxItems": 3,
                    "items": { "type": "string", "maxLength": 60 },
                },
                "description":
                    "The words a provider would file each need under, grouped one inner \
                     array per need. These search the wider ecosystem, where Apiosk's \
                     capability vocabulary does not apply. Example: \
                     [[\"financial statements\",\"annual accounts\"],[\"directors\",\"board members\"]].",
            },
            "subjects": {
                "type": "array",
                "maxItems": 5,
                "description":
                    "What the question is ABOUT. This does not filter the search — it is \
                     carried through so the chosen service's inputs can be filled in \
                     afterwards without asking the person again.",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["type", "name"],
                    "properties": {
                        "type": {
                            "type": "string",
                            "enum": ["company", "person", "place", "domain", "asset", "document", "other"],
                        },
                        "name": { "type": "string", "maxLength": 200 },
                        "identifiers": {
                            "type": "object",
                            "description":
                                "Any identifier you already hold, by its own name: \
                                 kvk_number, vat, lei, ticker, domain, isin, email.",
                            "additionalProperties": { "type": "string" },
                        },
                    },
                },
            },
            "constraints": {
                "type": "object",
                "additionalProperties": false,
                "description": "Ceilings the person set. Every one of these filters the search.",
                "properties": {
                    "max_price_usd": {
                        "type": ["number", "null"],
                        "exclusiveMinimum": 0,
                        "description":
                            "Most the person will pay for ONE call, in US dollars. Apiosk \
                             prices in USD end to end; a euro figure must be converted \
                             before it is sent.",
                    },
                    "max_latency_ms": { "type": ["integer", "null"], "minimum": 1 },
                    "min_reliability": { "type": ["number", "null"], "minimum": 0, "maximum": 1 },
                    "optimize_for": {
                        "type": ["string", "null"],
                        "enum": ["price", "speed", "reliability", null],
                    },
                    "include_external": {
                        "type": ["boolean", "null"],
                        "description":
                            "Also sweep the wider ecosystem, not only the reviewed \
                             Apiosk catalogue. Defaults to true.",
                    },
                },
            },
        },
    })
}

/// How a normalised request becomes a `/v1/discover` call.
///
/// Written down here rather than in each client, so four transports translate
/// it identically. Anything not named here has no effect on discovery and
/// should not be asked of the model — a field the caller fills in and nobody
/// reads is a field that costs tokens and buys nothing.
fn mapping() -> Value {
    json!({
        "endpoint": "GET /v1/discover",
        "fields": {
            "query": "q",
            "capabilities": "capability — the FIRST slug narrows the reviewed half. Send one request per further slug and merge the answers; this endpoint takes one capability per call.",
            "terms": "terms — inner arrays joined by ',', the groups joined by ';'",
            "subjects": "not sent. Carried by the caller and used to fill the chosen service's inputs.",
            "constraints.max_price_usd": "max_price",
            "constraints.max_latency_ms": "max_latency_ms",
            "constraints.min_reliability": "min_reliability",
            "constraints.optimize_for": "optimize_for",
            "constraints.include_external": "include_external"
        },
        "parse": "Send parse=false whenever `capabilities` or `terms` is present: you have already read the question, and a second reading costs a model call and can only disagree with you. Send no parse parameter when `query` is all you have.",
        "enrich": "Send enrich=false alongside parse=false. Widening exists to rescue a reading; you supplied one.",
        "fallback": "If a structured request returns no candidates, retry ONCE with q alone and no parse parameter. A wrong capability guess should cost a second free request, not the answer."
    })
}

/// `GET /v1/discover/schema` — the normalised discovery request, as a schema.
pub async fn discover_schema(State(state): State<Arc<AppState>>) -> Response {
    let capabilities = match state.db.list_capabilities().await {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!("discover schema: could not list capabilities: {err}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": "capabilities_unavailable",
                    "message": "The capability vocabulary could not be read.",
                })),
            )
                .into_response();
        }
    };

    let servable: Vec<&CapabilityInfo> = capabilities.iter().filter(|c| is_servable(c)).collect();
    let slugs: Vec<String> = servable.iter().map(|c| c.slug.clone()).collect();
    let choices: Vec<Value> = servable.iter().map(|c| choice(c)).collect();

    let body = json!({
        "version": "2026-09-03",
        "request_schema": request_schema(slugs),
        "capabilities": choices,
        "mapping": mapping(),
        "meta": {
            "servable": servable.len(),
            "total": capabilities.len(),
            "note": "Capabilities with no servable offer are omitted from the enum and from `capabilities`. They still appear on GET /v1/capabilities.",
        },
    });

    (
        StatusCode::OK,
        [(
            header::CACHE_CONTROL,
            format!("public, max-age={CACHE_SECONDS}"),
        )],
        Json(body),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(slug: &str, offers: i64) -> CapabilityInfo {
        CapabilityInfo {
            slug: slug.to_string(),
            name: slug.to_string(),
            description: String::new(),
            category: None,
            input_contract: json!({}),
            output_contract: json!({}),
            offer_count: offers,
            listing_count: if offers > 0 { 1 } else { 0 },
            min_price_usd: None,
            max_price_usd: None,
        }
    }

    /// The whole point of the endpoint: an agent can only pick a slug that is
    /// in the enum, and the enum is this catalogue's own words.
    #[test]
    fn the_enum_is_the_catalogue_vocabulary() {
        let schema = request_schema(vec![
            "company.financial-statements".into(),
            "company_registry.lookup".into(),
        ]);
        let values = schema["properties"]["capabilities"]["items"]["enum"]
            .as_array()
            .expect("capabilities carries an enum");
        assert_eq!(values.len(), 2);
        assert_eq!(values[0], "company.financial-statements");
    }

    /// A capability nothing serves is a slug an agent can pick and get nothing
    /// for, with no way to tell a wrong word from an empty market.
    #[test]
    fn unservable_capabilities_stay_out_of_the_enum() {
        assert!(is_servable(&info("company.financial-statements", 1)));
        assert!(!is_servable(&info("weather.forecast", 0)));
    }

    /// `query` alone must be a valid request: an agent that has no capability
    /// to name still gets today's behaviour, which is what makes the structured
    /// form a superset rather than a replacement.
    #[test]
    fn only_the_query_is_required() {
        let schema = request_schema(vec!["people.search".into()]);
        assert_eq!(schema["required"].as_array().unwrap().len(), 1);
        assert_eq!(schema["required"][0], "query");
    }

    /// Every field the model is asked to fill in has to reach `/v1/discover`
    /// or be used afterwards. A field nobody reads costs tokens and buys
    /// nothing — see the note on `subjects`, which is the one exception and
    /// says so.
    #[test]
    fn every_field_is_accounted_for_in_the_mapping() {
        let schema = request_schema(vec!["people.search".into()]);
        let fields = mapping()["fields"].clone();
        for name in schema["properties"].as_object().unwrap().keys() {
            if name == "constraints" {
                for constraint in schema["properties"]["constraints"]["properties"]
                    .as_object()
                    .unwrap()
                    .keys()
                {
                    let key = format!("constraints.{constraint}");
                    assert!(fields.get(&key).is_some(), "{key} is not mapped");
                }
                continue;
            }
            assert!(fields.get(name).is_some(), "{name} is not mapped");
        }
    }

    /// The price ceiling is in DOLLARS. Apiosk prices in USD end to end, and a
    /// euro figure read as a dollar one is a ceiling that is silently wrong.
    #[test]
    fn the_price_ceiling_is_named_in_dollars() {
        let schema = request_schema(vec![]);
        let constraints = &schema["properties"]["constraints"]["properties"];
        assert!(constraints.get("max_price_usd").is_some());
        assert!(constraints.get("max_price_eur").is_none());
    }
}
