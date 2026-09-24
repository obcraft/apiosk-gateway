use apiosk_gateway_v2::{
    catalog::{Catalog, Snapshot},
    parser::{Goal, Intent, Subject},
    resolver::{resolve, Resolution},
    state::{Entity, Fact, Task},
};
use chrono::Utc;
use serde_json::json;
use std::collections::BTreeMap;
use uuid::Uuid;

fn catalog() -> Catalog {
    let snapshot: Snapshot =
        serde_json::from_str(include_str!("fixtures/tenderned-browse.json")).unwrap();
    let catalog = Catalog::build(snapshot);
    assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
    catalog
}

#[test]
fn country_wide_request_quotes_one_bounded_call_without_inventing_a_keyword() {
    let catalog = catalog();
    let entity = Uuid::new_v4();
    let mut task = Task::default();
    task.entities.push(Entity {
        entity_ref: entity,
        label: "netherlands".into(),
        selected: true,
    });
    let intent = Intent {
        subjects: vec![Subject {
            entity_ref: Some(entity),
            label: "netherlands".into(),
            values: BTreeMap::new(),
        }],
        goals: vec![Goal {
            capability: "procurement.nl-public-contracts".into(),
            subject: 0,
            constraints: BTreeMap::new(),
        }],
        answer_operations: vec![],
        unresolved_references: vec![],
    };
    intent
        .validate(
            "Show me public goverment contracts in netherlands",
            &task,
            &catalog,
        )
        .unwrap();
    let Resolution::Plan(plan) = resolve(&catalog, &task, &intent) else {
        panic!("Country-only browse must be executable")
    };
    assert_eq!(plan.steps.len(), 1);
    let service = &plan.steps[0].service;
    assert_eq!(service.api_slug, "tenderned");
    assert_eq!(plan.cap_atomic, service.total());
    assert!(
        plan.cap_atomic > 0,
        "Existing approval and billing remain required"
    );
    assert_eq!(
        service.inputs(&task, entity, &catalog.vocabulary).unwrap(),
        json!({"page":0,"size":20,"sort":"publicatieDatum,desc"})
    );
}

#[test]
fn explicit_keywords_and_date_filters_are_preserved() {
    let catalog = catalog();
    let entity = Uuid::new_v4();
    let mut task = Task::default();
    for (kind, value) in [
        ("company_intel.input.search", "laadpalen"),
        ("company_intel.input.publicatiedatumvanaf", "2026-01-01"),
    ] {
        task.facts.push(Fact {
            entity_ref: entity,
            semantic_type: kind.into(),
            value: json!(value),
            provenance: json!({"kind":"user_input"}),
            observed_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::hours(1),
            verification: "user_asserted".into(),
        });
    }
    let inputs = catalog.services[0]
        .inputs(&task, entity, &catalog.vocabulary)
        .unwrap();
    assert_eq!(
        inputs,
        json!({"page":0,"size":20,"sort":"publicatieDatum,desc",
                             "search":"laadpalen","publicatieDatumVanaf":"2026-01-01"})
    );
}

#[test]
fn source_rows_keep_awards_corrections_and_dates_distinct() {
    let catalog = catalog();
    let service = &catalog.services[0];
    let raw = service.sample_payload.as_ref().unwrap();
    let result = service.contract.extract(raw, &catalog.vocabulary).unwrap();
    assert!(result.rejected.is_empty());
    let page = &result.subjects[0]
        .fact("tenderned.publication_page")
        .unwrap()
        .value;
    assert_eq!(page, raw);
    let rows = page["content"].as_array().unwrap();
    assert_eq!(rows.len(), 20);
    assert!(page["totalElements"].as_u64().unwrap() > rows.len() as u64);
    for kind in ["AAO", "AGO", "REC", "VBE"] {
        assert!(
            rows.iter().any(|row| row["typePublicatie"]["code"] == kind),
            "Missing {kind} evidence"
        );
    }
    assert!(!page["last"].as_bool().unwrap());
}
