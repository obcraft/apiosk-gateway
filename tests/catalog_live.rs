use apiosk_gateway_v2::{
    catalog::{Catalog, Snapshot},
    parser::{Goal, Intent, Subject},
    resolver::{resolve, Resolution},
    state::{Fact, Task},
};
use chrono::Utc;
use serde_json::json;
use std::collections::BTreeMap;
use uuid::Uuid;

#[test]
#[ignore = "requires a read-only Supabase catalog snapshot via CATALOG_SNAPSHOT"]
fn live_snapshot_validates_when_supplied() {
    let path = std::env::var("CATALOG_SNAPSHOT").expect("CATALOG_SNAPSHOT is required");
    let snapshot: Snapshot = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let before = snapshot.services.len();
    let catalog = Catalog::build(snapshot);
    println!(
        "live catalog: {before} queried, {} accepted, {} quarantined",
        catalog.services.len(),
        catalog.rejected.len()
    );
    for (id, why) in &catalog.rejected {
        println!("quarantined {id}: {why}")
    }
    assert!(!catalog.services.is_empty());
    assert!(catalog
        .by_capability
        .contains_key("company.financial-statements"));

    let entity = Uuid::new_v4();
    let mut task = Task::default();
    task.facts.push(Fact {
        entity_ref: entity,
        semantic_type: "company.name".into(),
        value: json!("Mollie BV"),
        provenance: json!({"kind":"user_input"}),
        observed_at: Utc::now(),
        expires_at: Utc::now() + chrono::Duration::hours(1),
        verification: "user_asserted".into(),
    });
    let intent = Intent {
        subjects: vec![Subject {
            entity_ref: Some(entity),
            label: "Mollie BV".into(),
            values: BTreeMap::new(),
        }],
        goals: vec![Goal {
            capability: "company.financial-statements".into(),
            subject: 0,
            constraints: BTreeMap::new(),
        }],
        answer_operations: vec![],
        unresolved_references: vec![],
    };
    let Resolution::Plan(plan) = resolve(&catalog, &task, &intent) else {
        panic!("Mollie accounts request must resolve to a dependency plan")
    };
    assert_eq!(plan.steps.len(), 2);
    assert_eq!(
        plan.steps
            .iter()
            .map(|step| step.service.contract.capability_slug.as_str())
            .collect::<Vec<_>>(),
        ["company_registry.search", "company.financial-statements"]
    );
}
