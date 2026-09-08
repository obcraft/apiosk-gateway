use apiosk_gateway_v2::{
    catalog::{Catalog, Snapshot},
    parser::{Goal, Intent, Subject},
    resolver::{resolve, Resolution},
    state::{Fact, Task},
};
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/company-sources/catalog.json")).unwrap()
}
fn catalog() -> Catalog {
    let value = fixture();
    let catalog = Catalog::build(serde_json::from_value(value.clone()).unwrap());
    assert_eq!(
        catalog.services.len(),
        value["services"].as_array().unwrap().len(),
        "{:?}",
        catalog.rejected
    );
    catalog
}
fn task(values: &[(&str, Value)]) -> (Uuid, Task) {
    let entity = Uuid::new_v4();
    let mut task = Task::default();
    for (kind, value) in values {
        task.facts.push(Fact {
            entity_ref: entity,
            semantic_type: (*kind).into(),
            value: value.clone(),
            provenance: json!({"kind":"user_input"}),
            observed_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::hours(1),
            verification: "user_asserted".into(),
        });
    }
    (entity, task)
}
fn intent(entity: Uuid, capability: &str, source: &str) -> Intent {
    Intent {
        subjects: vec![Subject {
            entity_ref: Some(entity),
            label: "Example".into(),
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
#[test]
fn every_mapping_extracts_its_declared_outputs() {
    let c = catalog();
    for s in &c.services {
        let extraction = s
            .contract
            .extract(s.sample_payload.as_ref().unwrap(), &c.vocabulary)
            .unwrap();
        assert!(
            extraction.rejected.is_empty(),
            "{}: {:?}",
            s.contract.capability_slug,
            extraction.rejected
        );
        for output in s.contract.output_map.produces.keys() {
            assert!(
                extraction
                    .subjects
                    .iter()
                    .any(|subject| subject.fact(output).is_some()),
                "{} missing {output}",
                s.contract.capability_slug
            );
        }
    }
}
#[test]
fn prospect_filters_bind_to_the_documented_wire_shape() {
    let c = catalog();
    let (e, t) = task(&[
        ("company.search.headquarters_location", json!("Amsterdam")),
        ("company.search.keyword", json!("SaaS")),
        ("company.search.headcount_min", json!(50)),
        ("company.search.headcount_max", json!(200)),
    ]);
    let Resolution::Plan(p) = resolve(
        &c,
        &t,
        &intent(e, "company.prospect-search", "stableenrich"),
    ) else {
        panic!("expected search plan")
    };
    assert_eq!(p.steps.len(), 1);
    assert_eq!(
        p.steps[0].service.inputs(&t, e, &c.vocabulary).unwrap(),
        json!({"headquarters_locations":["Amsterdam"],"keywords":["SaaS"],"headcount_min":50,"headcount_max":200})
    );
    let mut constrained = intent(e, "company.prospect-search", "stableenrich");
    constrained.goals[0]
        .constraints
        .insert("period".into(), json!("two years"));
    assert!(matches!(
        resolve(&c, &t, &constrained),
        Resolution::Unsupported(_)
    ));
    let (_, empty) = task(&[]);
    assert!(matches!(
        resolve(
            &c,
            &empty,
            &intent(e, "company.prospect-search", "stableenrich")
        ),
        Resolution::Missing { .. }
    ));
}
#[test]
fn registry_and_enrichment_dependencies_resolve_without_guessed_identifiers() {
    let c = catalog();
    let (e, t) = task(&[("company.name", json!("Example"))]);
    for (source, goal, search) in [
        (
            "gleif",
            "company.lei-verification",
            "company.legal-entity-search",
        ),
        (
            "gleif",
            "company.direct-parent",
            "company.legal-entity-search",
        ),
        (
            "companies-house",
            "company.officers",
            "company.uk-registry-search",
        ),
        (
            "companies-house",
            "company.registry-profile",
            "company.uk-registry-search",
        ),
        (
            "stableenrich",
            "company.enrich",
            "company.domain-resolution",
        ),
    ] {
        let Resolution::Plan(p) = resolve(&c, &t, &intent(e, goal, source)) else {
            panic!("{source} {goal} must plan")
        };
        assert_eq!(p.steps.len(), 2, "{source} {goal}");
        assert_eq!(p.steps[0].service.contract.capability_slug, search);
        assert!(
            p.steps[1].service.inputs(&t, e, &c.vocabulary).is_err(),
            "identifier must come from first result"
        );
    }
}
#[test]
fn screening_uses_company_scope_and_preserves_empty_matches() {
    let c = catalog();
    let (e, t) = task(&[
        ("company.name", json!("Example BV")),
        ("company.search.country_code", json!("NL")),
    ]);
    let Resolution::Plan(p) = resolve(
        &c,
        &t,
        &intent(e, "company.sanctions-screen", "opensanctions"),
    ) else {
        panic!("expected screening plan")
    };
    let service = &p.steps[0].service;
    assert_eq!(
        service.inputs(&t, e, &c.vocabulary).unwrap(),
        json!({"dataset":"default","limit":10,"queries":{"q1":{"schema":"Company","properties":{"name":["Example BV"],"country":["NL"]}}}})
    );
    let empty=service.contract.extract(&json!({"responses":{"q1":{"status":200,"results":[],"total":{"value":0,"relation":"eq"}}}}),&c.vocabulary).unwrap();
    assert!(empty.rejected.is_empty());
    assert_eq!(
        empty.subjects[0]
            .fact("company.screening_result")
            .unwrap()
            .value["results"],
        json!([])
    );
    let failed = service
        .contract
        .extract(
            &json!({"responses":{"q1":{"status":500,"results":[]}}}),
            &c.vocabulary,
        )
        .unwrap();
    assert!(!failed.rejected.is_empty());
}
#[test]
fn active_companies_house_has_live_response_evidence() {
    for s in fixture()["services"].as_array().unwrap() {
        if s["api_slug"] == "companies-house" {
            assert_eq!(s["activation_status"], "active");
            assert!(s["evidence"]
                .as_str()
                .unwrap()
                .starts_with("Live Companies House capture"));
            assert!(s["sample_payload"].get("x-apiosk-unsampled").is_none());
        }
    }
}
#[test]
#[ignore = "requires the read-only production snapshot in COMPANY_SOURCES_BASELINE"]
fn proposed_catalog_preserves_existing_contracts() {
    let old: Snapshot = serde_json::from_slice(
        &std::fs::read(std::env::var("COMPANY_SOURCES_BASELINE").unwrap()).unwrap(),
    )
    .unwrap();
    let old_count = old.services.len();
    let baseline = Catalog::build(old.clone());
    let proposed: Snapshot = serde_json::from_value(fixture()).unwrap();
    let mut merged = old;
    for word in proposed.vocabulary {
        if !merged.vocabulary.iter().any(|f| f.slug == word.slug) {
            merged.vocabulary.push(word);
        }
    }
    merged.services.extend(proposed.services);
    let result = Catalog::build(merged);
    assert_eq!(result.rejected, baseline.rejected);
    assert_eq!(
        result.services.len(),
        baseline.services.len() + fixture()["services"].as_array().unwrap().len()
    );
    println!(
        "Existing catalog {old_count}; {} accepted after addition; {} rejected",
        result.services.len(),
        result.rejected.len()
    );
}

#[tokio::test]
#[ignore = "requires configured intent model; no company data is purchased"]
async fn live_parser_binds_company_requests() {
    dotenvy::dotenv().ok();
    let parser = apiosk_gateway_v2::parser::Parser::new(
        reqwest::Client::new(),
        std::env::var("INTENT_API_URL")
            .unwrap_or_else(|_| "https://openrouter.ai/api/v1/chat/completions".into()),
        std::env::var("INTENT_API_KEY").expect("configured intent model key"),
        std::env::var("INTENT_MODEL").unwrap_or_else(|_| "google/gemini-3.8-flash".into()),
    );
    let proposed: Snapshot = serde_json::from_value(fixture()).unwrap();
    let mut snapshot: Snapshot = serde_json::from_slice(
        &std::fs::read(std::env::var("COMPANY_SOURCES_BASELINE").unwrap()).unwrap(),
    )
    .unwrap();
    for word in proposed.vocabulary {
        if !snapshot.vocabulary.iter().any(|f| f.slug == word.slug) {
            snapshot.vocabulary.push(word);
        }
    }
    snapshot.services.extend(proposed.services);
    let catalog = Catalog::build(snapshot);
    for (question, capability, required) in [
        (
            "Zoek via StableEnrich SaaS-bedrijven in Amsterdam met 50 tot 200 medewerkers.",
            "company.prospect-search",
            vec![
                "company.search.headquarters_location",
                "company.search.keyword",
                "company.search.headcount_min",
                "company.search.headcount_max",
            ],
        ),
        (
            "Haal bij Companies House het bedrijfsprofiel op voor nummer 00445790.",
            "company.registry-profile",
            vec!["company.registration.gb.companies_house"],
        ),
        (
            "Haal bij GLEIF de directe moederrelatie op van LEI 5493001KJTIIGC8Y1R12.",
            "company.direct-parent",
            vec!["company.registration.lei"],
        ),
        (
            "Screen bedrijf Gazprom met OpenSanctions op sancties.",
            "company.sanctions-screen",
            vec!["company.name"],
        ),
    ] {
        let intent = parser
            .parse(Uuid::new_v4(), question, &Task::default(), &catalog)
            .await
            .unwrap();
        assert!(
            intent.goals.iter().any(|g| g.capability == capability),
            "{question}: {intent:?}"
        );
        for kind in required {
            assert!(
                intent.subjects.iter().any(|s| s.values.contains_key(kind)),
                "{question}: missing {kind}: {intent:?}"
            );
        }
        assert!(
            intent
                .goals
                .iter()
                .all(|g| g.constraints.keys().all(|k| k == "source")),
            "literal filters became unsupported constraints: {intent:?}"
        );
        println!("Live parser: {question} => {capability}");
    }
}
