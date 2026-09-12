use apiosk_gateway_v2::{
    catalog::Catalog,
    parser::{Goal, Intent, Subject},
    resolver::{resolve, Resolution},
    state::{Fact, Task},
};
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use uuid::Uuid;
fn catalog() -> Catalog {
    let c = Catalog::build(
        serde_json::from_str(include_str!("fixtures/sirenic/catalog.json")).unwrap(),
    );
    assert_eq!(c.services.len(), 6, "{:?}", c.rejected);
    c
}
fn task(values: &[(&str, Value)]) -> (Uuid, Task) {
    let e = Uuid::new_v4();
    let mut t = Task::default();
    for (k, v) in values {
        t.facts.push(Fact {
            entity_ref: e,
            semantic_type: k.to_string(),
            value: v.clone(),
            provenance: json!({"kind":"user_input"}),
            observed_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::hours(1),
            verification: "user_asserted".into(),
        });
    }
    (e, t)
}
fn intent(e: Uuid, cap: &str) -> Intent {
    Intent {
        subjects: vec![Subject {
            entity_ref: Some(e),
            label: "DANONE".into(),
            values: BTreeMap::new(),
        }],
        goals: vec![Goal {
            capability: cap.into(),
            subject: 0,
            constraints: BTreeMap::new(),
        }],
        answer_operations: vec![],
        unresolved_references: vec![],
    }
}
#[test]
fn planner_can_select_sirenic_without_explicit_source_and_resolve_siren() {
    let c = catalog();
    let (e, t) = task(&[("company.name", json!("DANONE"))]);
    for cap in ["company.fr-financials", "company.fr-kyb"] {
        let Resolution::Plan(p) = resolve(&c, &t, &intent(e, cap)) else {
            panic!("no {cap} plan")
        };
        assert_eq!(p.steps.len(), 2);
        assert_eq!(
            p.steps[0].service.contract.capability_slug,
            "company.fr-registry-search"
        );
        assert_eq!(
            p.steps[0].service.inputs(&t, e, &c.vocabulary).unwrap(),
            json!({"q":"DANONE"})
        );
        assert!(p.steps[1].service.inputs(&t, e, &c.vocabulary).is_err());
        assert_eq!(
            p.cap_atomic,
            if cap == "company.fr-financials" {
                13800
            } else {
                174800
            }
        );
    }
}
#[test]
fn financial_quality_nulls_and_provenance_survive_extraction() {
    let c = catalog();
    let s = c
        .services
        .iter()
        .find(|s| s.path.ends_with("/finances"))
        .unwrap();
    let mut raw = s.sample_payload.clone().unwrap();
    raw["exercices"][0]["resultat_net"] = Value::Null;
    raw["provenance"] = json!([{"etat":"absence_non_conclusive","couverture":"partielle"}]);
    let ex = s.contract.extract(&raw, &c.vocabulary).unwrap();
    assert!(ex.rejected.is_empty());
    assert_eq!(
        ex.subjects[0]
            .fact("company.fr_financial_record")
            .unwrap()
            .value,
        raw
    );
    let invalid = s
        .contract
        .extract(&json!({"error":"unavailable"}), &c.vocabulary)
        .unwrap();
    assert!(!invalid.rejected.is_empty());
    let (e, t) = task(&[("company.registration.fr.siren", json!("552032534"))]);
    assert_eq!(
        s.inputs(&t, e, &c.vocabulary).unwrap(),
        json!({"siren":"552032534"})
    );
    let (e, t) = task(&[("company.registration.fr.siren", json!("552032534/other"))]);
    assert!(s.inputs(&t, e, &c.vocabulary).is_err());
}
#[test]
fn eu_search_keeps_country_and_identifier_together() {
    let c = catalog();
    let s = c
        .services
        .iter()
        .find(|s| s.path == "/v1/eu/recherche")
        .unwrap();
    let raw = json!({"resultats":[{"pays":"BE","id_national":"12345678","denomination":"Example"},{"pays":"NO","id_national":"12345678","denomination":"Example"}]});
    let ex = s.contract.extract(&raw, &c.vocabulary).unwrap();
    assert!(ex.rejected.is_empty());
    assert_eq!(ex.subjects.len(), 2);
    assert_ne!(ex.subjects[0].identity, ex.subjects[1].identity);
    let p = c
        .services
        .iter()
        .find(|s| s.path == "/v1/eu/entreprise/{pays}/{id}")
        .unwrap();
    let (e, t) = task(&[
        ("company.country_code", json!("NO")),
        ("company.registration.eu.national_id", json!("923609016")),
    ]);
    assert_eq!(
        p.inputs(&t, e, &c.vocabulary).unwrap(),
        json!({"pays":"NO","id":"923609016"})
    );
}
#[test]
fn only_reviewed_fixed_price_read_routes_are_enabled() {
    let c = catalog();
    for s in &c.services {
        assert_eq!(s.method, "GET");
        assert!(!s.path.contains("surveillance"));
        assert!(!s.path.contains("batch"));
        assert_eq!(s.total(), s.list_atomic + (s.list_atomic * 15 + 99) / 100);
    }
}

#[test]
fn partial_or_unreliable_search_cannot_authorize_dependent_purchases() {
    use apiosk_gateway_v2::source_preview::sirenic_search_incomplete;
    for raw in [
        json!({"resultats_partiels":true}),
        json!({"aucune_correspondance_fiable":true}),
        json!({"registres_muets":["NO"]}),
        json!({"etages_abandonnes":["name"]}),
    ] {
        assert!(sirenic_search_incomplete(&raw));
    }
    assert!(!sirenic_search_incomplete(
        &json!({"resultats":[],"registres_muets":[],"etages_abandonnes":[]})
    ));
}
