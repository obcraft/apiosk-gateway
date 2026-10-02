//! ECB-only rate contract checked against public responses captured 2026-10-02.
//! No network, billing, model calls or publication of the hidden source.
use apiosk_gateway_v2::{catalog::{Catalog, Snapshot}, engine::bind_subjects,
    parser::{Goal, Intent, Subject}, resolver::{resolve, Resolution}, state::Task};
use serde_json::{json, Value};
use std::collections::BTreeMap;
fn catalog() -> Catalog {
    let snapshot: Snapshot = serde_json::from_str(include_str!("fixtures/frankfurter/catalog.json")).unwrap();
    let catalog = Catalog::build(snapshot);
    assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
    assert_eq!(catalog.services.len(), 1);
    catalog
}
fn intent(quote: Option<&str>) -> Intent {
    Intent { subjects: vec![Subject { entity_ref: None, label: "ECB EUR reference rate".into(),
        values: quote.map(|q| BTreeMap::from([("fx.ecb.quote".into(), json!(q))])).unwrap_or_default() }],
        goals: vec![Goal { subject: 0, capability: "fx.ecb-reference-rate".into(), constraints: BTreeMap::new() }],
        answer_operations: vec![], unresolved_references: vec![] }
}
#[test]
fn ecb_pair_binds_exactly_and_quotes_existing_frankfurter_tariff() {
    let catalog = catalog(); let mut task = Task::default(); let mut intent = intent(Some("USD"));
    bind_subjects(&mut task, &catalog, &mut intent).unwrap();
    let Resolution::Plan(plan) = resolve(&catalog, &task, &intent) else { panic!("expected rate quote") };
    assert_eq!(plan.steps.len(), 1); assert_eq!(plan.cap_atomic, 57500);
    let step = &plan.steps[0];
    assert_eq!(step.service.path, "/v2/providers/ecb/rate/EUR/{quote}");
    assert_eq!(step.service.inputs(&task, step.entity_ref, &catalog.vocabulary).unwrap(), json!({"quote":"USD"}));
    let raw: Value = serde_json::from_str(include_str!("fixtures/frankfurter/eur-usd.json")).unwrap();
    let extracted = step.service.contract.extract(&raw, &catalog.vocabulary).unwrap();
    assert!(extracted.rejected.is_empty());
    let fact = &extracted.single().unwrap().fact("fx.ecb.reference_rate").unwrap().value;
    assert_eq!(fact, &raw); assert_eq!(fact["base"], "EUR"); assert_eq!(fact["quote"], "USD");
}
#[test]
fn missing_or_unsupported_currency_never_becomes_a_valid_request() {
    let catalog = catalog(); let mut task = Task::default(); let mut missing = intent(None);
    bind_subjects(&mut task, &catalog, &mut missing).unwrap();
    assert!(!matches!(resolve(&catalog, &task, &missing), Resolution::Plan(_)));
    for currency in ["BITCOIN", "AAA", "USD,GBP", "../USD"] {
        let mut task = Task::default(); let mut invalid = intent(Some(currency));
        assert!(bind_subjects(&mut task, &catalog, &mut invalid).is_err(), "{currency}");
    }
}
#[test]
fn errors_empty_results_wrong_base_and_invalid_rates_are_not_facts() {
    let catalog = catalog(); let contract = &catalog.services[0].contract;
    for body in [json!({"status":422,"message":"invalid currency"}), json!({}), json!([]),
        json!({"date":"2026-10-01","base":"USD","quote":"EUR","rate":1.0}),
        json!({"date":"2026-10-01","base":"EUR","quote":"USD","rate":0}),
        json!({"date":"2026-10-01","base":"EUR","quote":"USD","rate":"1.0"})] {
        let extracted = contract.extract(&body, &catalog.vocabulary).unwrap();
        assert!(extracted.subjects.iter().all(|s| s.facts.is_empty()), "{body}");
    }
}
#[test]
fn every_observed_ecb_currency_response_passes_the_contract() {
    let catalog = catalog();
    let rows: Vec<Value> = serde_json::from_str(include_str!("fixtures/frankfurter/eur-currencies.json")).unwrap();
    assert_eq!(rows.len(), 30);
    for row in rows {
        let extracted = catalog.services[0].contract.extract(&row, &catalog.vocabulary).unwrap();
        assert!(extracted.rejected.is_empty(), "{row}");
        assert!(extracted.single().unwrap().fact("fx.ecb.reference_rate").is_some());
    }
}
