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
        serde_json::from_str(include_str!("fixtures/crunchbase/catalog.json")).unwrap(),
    );
    assert!(c.rejected.is_empty(), "{:?}", c.rejected);
    assert_eq!(c.services.len(), 2);
    c
}
#[test]
fn verified_operations_resolve_bind_and_reject_provider_errors() {
    let c = catalog();
    for s in &c.services {
        let entity = Uuid::new_v4();
        let mut task = Task::default();
        let (fact, value) = if s.path == "/autocompletes" {
            ("company.crunchbase_query", json!("OpenAI"))
        } else {
            ("company.crunchbase_limit", json!(2))
        };
        task.facts.push(Fact {
            entity_ref: entity,
            semantic_type: fact.into(),
            value,
            provenance: json!({"kind":"user_input"}),
            observed_at: chrono::Utc::now(),
            expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            verification: "user_asserted".into(),
        });
        let intent = Intent {
            subjects: vec![Subject {
                entity_ref: Some(entity),
                label: "Crunchbase".into(),
                values: BTreeMap::new(),
            }],
            goals: vec![Goal {
                capability: s.contract.capability_slug.clone(),
                subject: 0,
                constraints: BTreeMap::from([("source".into(), json!(s.api_slug))]),
            }],
            answer_operations: vec![],
            unresolved_references: vec![],
        };
        let Resolution::Plan(plan) = resolve(&c, &task, &intent) else {
            panic!("operation did not resolve")
        };
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.cap_atomic, 0);
        let inputs = s.inputs(&task, entity, &c.vocabulary).unwrap();
        if s.path == "/autocompletes" {
            assert_eq!(inputs, json!({"query":"OpenAI"}));
        } else {
            assert_eq!(inputs["limit"], 2);
            assert_eq!(inputs["query"], json!([]));
            assert_eq!(
                inputs["field_ids"],
                json!([
                    "identifier",
                    "location_identifiers",
                    "short_description",
                    "rank_org"
                ])
            );
        }
        let result = s
            .contract
            .extract(s.sample_payload.as_ref().unwrap(), &c.vocabulary)
            .unwrap();
        assert!(result.rejected.is_empty());
        assert_eq!(result.subjects.len(), 1);
        for error in [
            json!({"message":"Internal Server Error"}),
            json!({"error":"quota_exceeded"}),
            json!({"count":2,"entities":"bad"}),
        ] {
            let result = s.contract.extract(&error, &c.vocabulary).unwrap();
            assert!(!result.rejected.is_empty());
            assert!(result.subjects.is_empty());
        }
    }
}
#[test]
fn failed_profile_operation_is_not_available() {
    let c = catalog();
    assert!(c
        .services
        .iter()
        .all(|s| s.path != "/entities/organizations"));
    let view = c.parser_view();
    assert!(view["capabilities"]
        .as_array()
        .unwrap()
        .contains(&Value::String("company.crunchbase-search".into())));
}
