use apiosk_gateway_v2::{
    catalog::{Catalog, Service, Snapshot},
    parser::{Goal, Intent, Subject},
    resolver::{resolve, Resolution},
    state::{Fact, Task},
};
use catalog_core::facts::contract::FactType;
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

fn service(cap: &str, requires: Value, outputs: Value, price: i64) -> Service {
    let input_map = requires
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["fact_type"].as_str().unwrap().to_owned(),
                json!({"name":r["fact_type"]}),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    serde_json::from_value(json!({"id":Uuid::new_v4(),"endpoint_id":Uuid::new_v4(),"api_slug":"fixture","name":cap,"method":"GET","path":"/data","listing_type":"api","list_atomic":price,
        "contract":{"capability_slug":cap,"version":1,"cardinality":"one","requires":requires,"input_map":input_map,"output_map":{"base":"$","produces":outputs},"max_fan_out":1},"fixed_params":{},"preconditions":{},"pagination":{},"freshness_seconds":3600,"sample_payload":{"id":"123","name":"Example","accounts":"2025"},"jurisdiction":"NL"})).unwrap()
}
fn setup() -> (Catalog, Task, Intent) {
    let search = service(
        "company.search",
        json!([{"fact_type":"company.name"}]),
        json!({"company.id":{"path":"$.id"}}),
        20,
    );
    let accounts = service(
        "company.accounts",
        json!([{"fact_type":"company.id"}]),
        json!({"company.accounts":{"path":"$.accounts"}}),
        100,
    );
    let catalog = Catalog::build(Snapshot {
        services: vec![search, accounts],
        vocabulary: ["company.name", "company.id", "company.accounts"]
            .iter()
            .map(|slug| FactType {
                slug: slug.to_string(),
                value_schema: json!({"type":"string"}),
            })
            .collect(),
    });
    let entity = Uuid::new_v4();
    let mut task = Task::default();
    task.facts.push(Fact {
        entity_ref: entity,
        semantic_type: "company.name".into(),
        value: json!("Example"),
        provenance: json!({}),
        observed_at: Utc::now(),
        expires_at: Utc::now() + chrono::Duration::hours(1),
        verification: "user_asserted".into(),
    });
    let intent = Intent {
        subjects: vec![Subject {
            entity_ref: Some(entity),
            label: "Example".into(),
            values: BTreeMap::new(),
        }],
        goals: vec![Goal {
            capability: "company.accounts".into(),
            subject: 0,
            constraints: BTreeMap::new(),
        }],
        answer_operations: vec![],
        unresolved_references: vec![],
    };
    (catalog, task, intent)
}
#[test]
fn missing_identifier_is_resolved_before_accounts_and_priced_once() {
    let (c, t, i) = setup();
    let Resolution::Plan(p) = resolve(&c, &t, &i) else {
        panic!()
    };
    assert_eq!(p.steps.len(), 2);
    assert_eq!(p.cap_atomic, 138);
}
#[test]
fn followup_uses_existing_identifier() {
    let (c, mut t, i) = setup();
    let mut id = t.facts[0].clone();
    id.semantic_type = "company.id".into();
    id.value = json!("123");
    t.facts.push(id);
    let Resolution::Plan(p) = resolve(&c, &t, &i) else {
        panic!()
    };
    assert_eq!(p.steps.len(), 1);
    assert_eq!(p.cap_atomic, 115);
}
#[test]
fn source_requirement_is_never_silently_relaxed() {
    let (c, t, mut i) = setup();
    i.goals[0]
        .constraints
        .insert("source".into(), json!("unavailable"));
    assert!(matches!(resolve(&c, &t, &i), Resolution::Unsupported(_)));
}
#[test]
fn conflicting_identifier_is_not_last_value_wins() {
    let (c, mut t, i) = setup();
    for v in ["123", "456"] {
        let mut f = t.facts[0].clone();
        f.semantic_type = "company.id".into();
        f.value = json!(v);
        t.facts.push(f)
    }
    assert!(matches!(resolve(&c, &t, &i), Resolution::Unsupported(_)));
}
#[test]
fn duplicate_goals_share_dependency_and_execution() {
    let (c, t, mut i) = setup();
    i.goals.push(i.goals[0].clone());
    let Resolution::Plan(p) = resolve(&c, &t, &i) else {
        panic!()
    };
    assert_eq!(p.steps.len(), 2);
}
#[test]
fn unknown_latest_semantics_are_reported_not_invented() {
    let (c, t, mut i) = setup();
    i.goals[0]
        .constraints
        .insert("filing_selection".into(), json!("latest_two"));
    assert!(matches!(resolve(&c, &t, &i), Resolution::Unsupported(_)));
}
