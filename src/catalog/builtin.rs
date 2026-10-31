//! Apiosk's own free sources, defined in code rather than as directory rows.
//! The free web search mirrors Tavily's `web.search` contract, so the same
//! subject values bind to it, and the executor runs it in-process without a
//! purchase (crate::web_search). Only live catalogs include it; fixtures don't.
use super::{FactType, Service, Snapshot};
use serde_json::json;
use std::sync::OnceLock;
use uuid::Uuid;

/// Fixed identity: quotes and the executor's re-check compare these.
pub const WEB_SEARCH_ID: Uuid = Uuid::from_u128(0x773fbf68_7617_49cd_b7fc_e8b9f515f673);
const WEB_SEARCH_ENDPOINT: Uuid = Uuid::from_u128(0x0bc42fcd_f0b0_4023_a597_04122838c866);

fn web_search() -> &'static (Service, Vec<FactType>) {
    static BUILT: OnceLock<(Service, Vec<FactType>)> = OnceLock::new();
    BUILT.get_or_init(|| {
        let input = |name: &str| json!({"in": "body", "name": name});
        let service = json!({
            "id": WEB_SEARCH_ID, "endpoint_id": WEB_SEARCH_ENDPOINT,
            "api_slug": crate::web_search::SLUG, "name": "Apiosk web search",
            "description": "Free web and news search through Apiosk's own SearXNG, with public news feeds, DuckDuckGo and Wikipedia as backups. Returns ranked results (title, URL, snippet, date when known), not page contents.",
            "method": "POST", "path": "/search", "listing_type": "api", "list_atomic": 0,
            "source_url": "https://docs.searxng.org/", "usage_notice": null, "jurisdiction": null,
            "fixed_params": {}, "preconditions": {}, "pagination": {}, "freshness_seconds": null,
            "optional_outputs": [], "unverified": false,
            "contract": {
                "capability_slug": "web.search", "cardinality": "one", "identifies": null, "max_fan_out": 1, "version": 2,
                "input_map": {
                    "web.query": input("query"), "web_tools.input.topic": input("topic"),
                    "web.input.timerange": input("time_range"), "web.input.startdate": input("start_date"),
                    "web.input.enddate": input("end_date"), "web.input.includedomains": input("include_domains"),
                    "web.input.excludedomains": input("exclude_domains")
                },
                "output_map": {"base": "$", "produces": {
                    "web.query": {"path": "$.query"}, "web.responsetime": {"path": "$.response_time"},
                    "web.title": {"path": "$.results[*].title"}, "web.url": {"path": "$.results[*].url"},
                    "web.content": {"path": "$.results[*].content"}, "web_tools.query": {"path": "$.query"},
                    "web_tools.title": {"path": "$.results[*].title"}, "web_tools.url": {"path": "$.results[*].url"},
                    "web_tools.content": {"path": "$.results[*].content"}
                }},
                "requires": [
                    {"fact_type": "web.query", "optional": false, "subject_role": "subject"},
                    {"fact_type": "web_tools.input.topic", "optional": true, "subject_role": "subject"},
                    {"fact_type": "web.input.timerange", "optional": true, "subject_role": "subject"},
                    {"fact_type": "web.input.startdate", "optional": true, "subject_role": "subject"},
                    {"fact_type": "web.input.enddate", "optional": true, "subject_role": "subject"},
                    {"fact_type": "web.input.includedomains", "optional": true, "subject_role": "subject"},
                    {"fact_type": "web.input.excludedomains", "optional": true, "subject_role": "subject"}
                ]
            },
            // A real response of this source, as the directory requires for every contract.
            "sample_payload": {"query": "Coolblue", "response_time": 1.4, "providers": ["SearXNG"], "results": [
                {"title": "Coolblue opens new store in Utrecht", "url": "https://nieuws.example.nl/coolblue-utrecht",
                 "content": "Coolblue opened a new store in Utrecht on Friday.", "published_date": "2026-10-28T06:21:43Z"}
            ]}
        });
        let string = json!({"type": "string"});
        let strings = json!({"type": "array", "items": {"type": "string"}});
        let vocabulary = [
            ("web.query", &string), ("web.content", &string), ("web.title", &string), ("web.url", &string),
            ("web.responsetime", &json!({"type": "number"})), ("web.input.timerange", &string),
            ("web.input.startdate", &string), ("web.input.enddate", &string),
            ("web.input.includedomains", &strings), ("web.input.excludedomains", &strings),
            ("web_tools.input.topic", &string), ("web_tools.query", &string), ("web_tools.title", &string),
            ("web_tools.url", &string), ("web_tools.content", &string),
        ]
        .into_iter()
        .map(|(slug, schema)| serde_json::from_value(json!({"slug": slug, "value_schema": schema})).expect("built-in fact type"))
        .collect();
        (serde_json::from_value(service).expect("built-in web search service"), vocabulary)
    })
}

/// Adds the free sources and any fact types they need that the snapshot lacks.
pub(super) fn extend(snapshot: &mut Snapshot) {
    let (service, vocabulary) = web_search();
    if !snapshot.services.iter().any(|s| s.id == service.id) {
        snapshot.services.push(service.clone());
    }
    for fact in vocabulary {
        if !snapshot.vocabulary.iter().any(|f| f.slug == fact.slug) {
            snapshot.vocabulary.push(fact.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_web_search_passes_the_directory_checks_and_is_cheapest() {
        let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../tests/fixtures/connected-planning/structured-inputs-catalog.json")).unwrap();
        extend(&mut snapshot);
        let catalog = super::super::Catalog::build(snapshot);
        assert!(!catalog.rejected.iter().any(|(id, _)| *id == WEB_SEARCH_ID), "{:?}", catalog.rejected);
        let first = catalog.by_capability["web.search"][0];
        assert_eq!(catalog.services[first].id, WEB_SEARCH_ID);
        assert_eq!(catalog.services[first].total(), 0);
        let tavily = catalog.services.iter().find(|s| s.api_slug == "tavily").unwrap();
        let map = |s: &Service| serde_json::to_value(&s.contract.input_map).unwrap();
        assert_eq!(map(tavily), map(&catalog.services[first]));
    }

    #[test]
    fn it_is_valid_without_any_directory_vocabulary() {
        let mut snapshot = Snapshot { services: vec![], vocabulary: vec![], known_sources: vec![] };
        extend(&mut snapshot);
        let catalog = super::super::Catalog::build(snapshot);
        assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
        assert_eq!(catalog.services.len(), 1);
    }
}
