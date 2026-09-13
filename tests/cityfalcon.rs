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
        serde_json::from_str(include_str!("fixtures/cityfalcon/catalog.json")).unwrap(),
    );
    assert_eq!(c.services.len(), 4, "{:?}", c.rejected);
    c
}
fn task(ticker: &str) -> (Uuid, Task) {
    let id = Uuid::new_v4();
    let mut t = Task::default();
    t.facts.push(Fact {
        entity_ref: id,
        semantic_type: "finance_markets.cityfalcon_identifiers".into(),
        value: json!(ticker),
        provenance: json!({"kind":"user_input"}),
        observed_at: chrono::Utc::now(),
        expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        verification: "user_asserted".into(),
    });
    (id, t)
}
#[test]
fn every_operation_resolves_binds_and_preserves_its_actual_response() {
    let c = catalog();
    let (e, t) = task("AAPL_US");
    for s in &c.services {
        let i = Intent {
            subjects: vec![Subject {
                entity_ref: Some(e),
                label: "AAPL_US".into(),
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
        let Resolution::Plan(plan) = resolve(&c, &t, &i) else {
            panic!("{} did not resolve", s.path)
        };
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.cap_atomic, s.total());
        assert_eq!(
            s.inputs(&t, e, &c.vocabulary).unwrap()["identifiers"],
            "AAPL_US"
        );
        let sample = s.sample_payload.as_ref().unwrap();
        let result = s.contract.extract(sample, &c.vocabulary).unwrap();
        assert!(result.rejected.is_empty());
        assert_eq!(result.subjects.len(), 1);
        // A wrong envelope or upstream error cannot become a successful data fact.
        assert!(!s
            .contract
            .extract(&json!({"error":"invalid token"}), &c.vocabulary)
            .unwrap()
            .rejected
            .is_empty());
        let empty = if sample.is_array() {
            json!([])
        } else if s.path == "/stories" {
            json!({"stories":[]})
        } else {
            json!({"entities":[]})
        };
        assert!(s
            .contract
            .extract(&empty, &c.vocabulary)
            .unwrap()
            .rejected
            .is_empty());
    }
}
#[test]
fn missing_or_ambiguous_ticker_cannot_dispatch() {
    let c = catalog();
    for s in &c.services {
        assert!(s
            .inputs(&Task::default(), Uuid::new_v4(), &c.vocabulary)
            .is_err());
        for bad in ["Apple", "AAPL", "AAPL_US&access_token=bad"] {
            let (e, t) = task(bad);
            assert!(s.inputs(&t, e, &c.vocabulary).is_err());
        }
    }
}
#[test]
fn period_is_bound_and_not_silently_ignored() {
    let c = catalog();
    let (e, mut t) = task("AAPL_US");
    let s = c
        .services
        .iter()
        .find(|s| s.path == "/services/sentiment")
        .unwrap();
    let mut p = t.facts[0].clone();
    p.semantic_type = "finance_markets.cityfalcon_sentiment_period".into();
    p.value = json!("w1");
    t.facts.push(p);
    assert_eq!(
        s.inputs(&t, e, &c.vocabulary).unwrap(),
        json!({"identifiers":"AAPL_US","identifier_type":"full_tickers","period":"w1"})
    );
    t.facts.last_mut().unwrap().value = Value::String("all history".into());
    assert!(s.inputs(&t, e, &c.vocabulary).is_err());
}
