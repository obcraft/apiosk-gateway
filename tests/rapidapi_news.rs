//! Reuters and Bloomberg news through RapidAPI, checked against the complete
//! 2026-09-14 provider captures in `tests/fixtures/rapidapi-news/captures/`.
use apiosk_gateway_v2::{
    catalog::Catalog,
    parser::{Goal, Intent, Subject},
    resolver::{resolve, Resolution},
    state::{Fact, Task},
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

fn catalog() -> Catalog {
    let c = Catalog::build(
        serde_json::from_str(include_str!("fixtures/rapidapi-news/catalog.json")).unwrap(),
    );
    assert!(c.rejected.is_empty(), "{:?}", c.rejected);
    // latest, search, sections, 18 fixed sections, Bloomberg latest.
    assert_eq!(c.services.len(), 22);
    c
}
fn capture(source: &str, name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/rapidapi-news/captures/{source}/{name}.body",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}
fn task(values: &[(&str, Value)]) -> (Uuid, Task) {
    let entity = Uuid::new_v4();
    let mut t = Task::default();
    for (kind, value) in values {
        t.facts.push(Fact {
            entity_ref: entity,
            semantic_type: kind.to_string(),
            value: value.clone(),
            provenance: json!({"kind":"user_input"}),
            observed_at: chrono::Utc::now(),
            expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            verification: "user_asserted".into(),
        });
    }
    (entity, t)
}
fn intent(entity: Uuid, source: &str, capability: &str) -> Intent {
    Intent {
        subjects: vec![Subject {
            entity_ref: Some(entity),
            label: "news".into(),
            values: BTreeMap::new(),
        }],
        goals: vec![Goal {
            capability: capability.into(),
            subject: 0,
            constraints: BTreeMap::from([("source".into(), json!(source))]),
        }],
        answer_operations: vec![],
        unresolved_references: vec![],
    }
}
fn inputs_for(capability: &str) -> Vec<(&'static str, Value)> {
    match capability {
        "news.reuters-search" => vec![("news.reuters_search_query", json!("bitcoin"))],
        _ => vec![],
    }
}

#[test]
fn both_sources_are_listed_with_news_capabilities() {
    let c = catalog();
    let view = c.parser_view();
    for (slug, caps) in [
        (
            "reuters",
            vec![
                "news.reuters-latest",
                "news.reuters-search",
                "news.reuters-sections",
                "news.reuters-section-africa",
                "news.reuters-section-technology",
                "news.reuters-section-world",
            ],
        ),
        ("bloomberg", vec!["news.bloomberg-latest"]),
    ] {
        let source = view["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["slug"] == slug)
            .unwrap_or_else(|| panic!("{slug} missing from parser view"));
        assert_ne!(source["available"], false, "{source}");
        for cap in caps {
            assert!(view["capabilities"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c == cap));
        }
    }
}

#[test]
fn every_operation_resolves_binds_and_extracts_the_complete_live_response() {
    let c = catalog();
    for s in &c.services {
        let cap = s.contract.capability_slug.as_str();
        let (e, t) = task(&inputs_for(cap));
        let Resolution::Plan(plan) = resolve(&c, &t, &intent(e, &s.api_slug, cap)) else {
            panic!("{cap} did not resolve")
        };
        assert_eq!(plan.steps.len(), 1);
        // 47,259 list + 7,089 fee.
        assert_eq!(plan.cap_atomic, s.total());
        assert_eq!(s.total(), 54_348);

        let inputs = s.inputs(&t, e, &c.vocabulary).unwrap();
        match cap {
            "news.reuters-search" => assert_eq!(inputs, json!({"q":"bitcoin"})),
            "news.bloomberg-latest" => assert_eq!(inputs, json!({"id":"phx-latest"})),
            section if section.starts_with("news.reuters-section-") => {
                let slug = section.trim_start_matches("news.reuters-section-");
                let url = inputs["url"].as_str().unwrap();
                assert!(url.starts_with("https://www.reuters.com/"), "{url}");
                assert!(url.ends_with(&format!("/{slug}/")), "{section}: {url}");
                assert_eq!(inputs.as_object().unwrap().len(), 1);
            }
            _ => assert_eq!(inputs, json!({})),
        }

        let name = match cap {
            "news.reuters-latest" => "latest".to_string(),
            "news.reuters-search" => "search".to_string(),
            "news.reuters-sections" => "sections".to_string(),
            "news.bloomberg-latest" => "news-list".to_string(),
            section => format!(
                "sections/{}",
                section.trim_start_matches("news.reuters-section-")
            ),
        };
        let full = capture(&s.api_slug, &name);
        for payload in [&full, s.sample_payload.as_ref().unwrap()] {
            let result = s.contract.extract(payload, &c.vocabulary).unwrap();
            assert!(result.rejected.is_empty(), "{cap}: {:?}", result.rejected);
            assert_eq!(result.subjects.len(), 1, "{cap}");
        }
    }
}

#[test]
fn provider_errors_never_become_data_but_empty_results_do() {
    let c = catalog();
    for s in &c.services {
        let cap = &s.contract.capability_slug;
        for error in [
            json!({"message":"You are not subscribed to this API."}),
            json!({"message":"Too many requests"}),
            capture("bloomberg", "stock-keystats"),
            capture("bloomberg", "news-list-by-region"),
        ] {
            let result = s.contract.extract(&error, &c.vocabulary).unwrap();
            assert!(
                !result.rejected.is_empty(),
                "{cap} accepted an error body as data"
            );
        }
        if s.api_slug == "reuters" {
            let result = s.contract.extract(&json!([]), &c.vocabulary).unwrap();
            assert!(result.rejected.is_empty(), "{cap} rejected an empty page");
        }
    }
}

#[test]
fn malformed_or_injected_inputs_cannot_dispatch() {
    let c = catalog();
    let find = |cap: &str| {
        c.services
            .iter()
            .find(|s| s.contract.capability_slug == cap)
            .unwrap()
    };
    let search = find("news.reuters-search");
    assert!(search
        .inputs(&Task::default(), Uuid::new_v4(), &c.vocabulary)
        .is_err());
    let (e, t) = task(&[("news.reuters_search_query", json!("b"))]);
    assert!(search.inputs(&t, e, &c.vocabulary).is_err());

    // Sections are fixed by the reviewed contract; no caller value can replace the URL.
    assert!(c
        .services
        .iter()
        .all(|s| s.contract.capability_slug != "news.reuters-section"));
    assert!(!c
        .services
        .iter()
        .any(|s| s.contract.capability_slug.ends_with("-breakingviews")));
    let africa = find("news.reuters-section-africa");
    assert!(africa.contract.requires.is_empty());
    assert_eq!(
        africa
            .inputs(&Task::default(), Uuid::new_v4(), &c.vocabulary)
            .unwrap(),
        json!({"url":"https://www.reuters.com/world/africa/"})
    );

    let latest = find("news.reuters-latest");
    let (e, t) = task(&[("news.reuters_result_limit", json!(5))]);
    assert_eq!(
        latest.inputs(&t, e, &c.vocabulary).unwrap(),
        json!({"limit":5})
    );
    for bad in [json!(0), json!(13), json!("all")] {
        let (e, t) = task(&[("news.reuters_result_limit", bad.clone())]);
        assert!(latest.inputs(&t, e, &c.vocabulary).is_err(), "{bad}");
    }
}
