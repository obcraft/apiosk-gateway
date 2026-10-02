use apiosk_gateway_v2::{catalog::{Catalog, Snapshot}, engine::bind_subjects,
    parser::{Goal, Intent, Subject}, resolver::{describe_steps, resolve, Resolution}, state::Task};
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn snapshot(notice: Option<Value>) -> Snapshot {
    let mut value: Value = serde_json::from_str(include_str!("fixtures/frankfurter/catalog.json")).unwrap();
    if let Some(notice) = notice { value["services"][0]["usage_notice"] = notice; }
    serde_json::from_value(value).unwrap()
}
fn notice() -> Value {
    json!({"text":"Source: European Central Bank. Original data is available free of charge.",
        "url":"https://www.ecb.europa.eu/stats/policy_and_exchange_rates/euro_reference_exchange_rates/html/index.en.html",
        "label":"Free ECB reference rates"})
}

#[test]
fn notice_survives_quoted_service_and_saved_result_without_affecting_price() {
    let catalog = Catalog::build(snapshot(Some(notice())));
    assert!(catalog.rejected.is_empty());
    let mut task = Task::default();
    let mut intent = Intent { subjects: vec![Subject { entity_ref: None, label: "EUR/USD".into(),
        values: BTreeMap::from([("fx.ecb.quote".into(), json!("USD"))]) }],
        goals: vec![Goal { subject: 0, capability: "fx.ecb-reference-rate".into(), constraints: BTreeMap::new() }],
        answer_operations: vec![], unresolved_references: vec![] };
    bind_subjects(&mut task, &catalog, &mut intent).unwrap();
    let Resolution::Plan(plan) = resolve(&catalog, &task, &intent) else { panic!("expected quote") };
    assert_eq!(plan.cap_atomic, 57500);
    assert_eq!(describe_steps(&plan, &[])[0]["source"]["usage_notice"], notice());
    let saved: apiosk_gateway_v2::catalog::Service = serde_json::from_value(serde_json::to_value(&plan.steps[0].service).unwrap()).unwrap();
    let document = json!({"data":{"rate":1.1225},"source":saved.result_source()});
    let recovered: Value = serde_json::from_str(&document.to_string()).unwrap();
    assert_eq!(recovered["source"]["usage_notice"], notice());
}

#[test]
fn malformed_required_source_notices_withdraw_the_service_instead_of_disappearing() {
    for bad in [json!({"text":""}),json!({"text":"x".repeat(1201)}),json!({"text":"Required terms","url":"javascript:alert(1)"}),json!({"text":"Required terms","url":"https://user:secret@example.com"}),json!({"text":"Required terms","label":[]})] {
        let catalog = Catalog::build(snapshot(Some(bad)));
        assert!(catalog.services.is_empty());
        assert_eq!(catalog.rejected.len(), 1);
        assert!(catalog.rejected[0].1.contains("invalid source usage notice"));
    }
}

#[test]
fn old_service_snapshots_need_no_notice_and_changed_terms_change_the_quote_fingerprint() {
    let old = Catalog::build(snapshot(None));
    let new = Catalog::build(snapshot(Some(notice())));
    assert!(old.services[0].usage_notice_view().is_none());
    assert!(serde_json::to_value(&old.services[0]).unwrap().get("usage_notice").is_none());
    assert_ne!(old.services[0].version_hash(), new.services[0].version_hash());
    assert_eq!(old.services[0].total(), new.services[0].total());
}
