use apiosk_gateway_v2::{
    catalog::{Catalog, Snapshot},
    parser::{Goal, Intent, Subject},
    resolver::{resolve, Resolution},
    state::{Fact, Task},
    uk_supplier,
};
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;
#[allow(dead_code)]
#[path = "../legacy-executor/src/proxy/public_sources.rs"]
mod adapter;

fn catalog() -> Catalog {
    let s: Snapshot =
        serde_json::from_str(include_str!("fixtures/uk-supplier/catalog.json")).unwrap();
    let c = Catalog::build(s);
    assert!(c.rejected.is_empty(), "{:?}", c.rejected);
    c
}
fn fact(task: &mut Task, e: Uuid, kind: &str, value: Value) {
    task.facts
        .retain(|f| !(f.entity_ref == e && f.semantic_type == kind));
    task.facts.push(Fact {
        entity_ref: e,
        semantic_type: kind.into(),
        value,
        provenance: json!({"fixture":true}),
        observed_at: Utc::now(),
        expires_at: Utc::now() + chrono::Duration::hours(1),
        verification: "fixture".into(),
    });
}
#[test]
fn oversized_check_requires_narrowing_before_one_price_ceiling() {
    let c = catalog();
    let e = Uuid::new_v4();
    let mut task = Task::default();
    fact(&mut task, e, "company.name", json!("Anthropic"));
    let mut intent = Intent {
        subjects: vec![Subject {
            entity_ref: Some(e),
            label: "Anthropic".into(),
            values: BTreeMap::new(),
        }],
        goals: vec![Goal {
            capability: uk_supplier::PRESET.into(),
            subject: 0,
            constraints: BTreeMap::new(),
        }],
        answer_operations: vec![],
        unresolved_references: vec![],
    };
    uk_supplier::expand(&mut intent, &c);
    assert!(matches!(resolve(&c, &task, &intent), Resolution::Unsupported(message)
        if message == apiosk_gateway_v2::protocol::SOURCE_LIMIT_MESSAGE),
        "the original twelve-call scope must not be quoted under a ten-call limit");
    // An explicitly narrowed fixture asks for at most three director screens.
    // The resolver must price that scope, rather than silently truncate the original.
    intent.goals.retain(|goal| goal.capability != uk_supplier::screening_cap(3)
        && goal.capability != uk_supplier::screening_cap(4));
    intent.answer_operations = vec!["Produce a PDF with the requested register and VAT findings and at most three director screens. Report incomplete screening if the roster is larger.".into()];
    let Resolution::Plan(p) = resolve(&c, &task, &intent) else {
        panic!(
            "Cannot resolve exact UK request: {:?}",
            resolve(&c, &task, &intent)
        );
    };
    assert_eq!(p.steps.len(), 10);
    assert_eq!(
        p.steps
            .iter()
            .map(|s| s.service.api_slug.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "companies-house",
            "sirenic",
            "opensanctions",
            "tavily",
            "ask-merlin"
        ])
    );
    assert_eq!(
        p.cap_atomic,
        p.steps.iter().map(|s| s.service.total()).sum::<i64>()
    );
    assert!(intent.answer_operations.join(" ").contains("PDF"));
    let mut calls = Vec::new();
    for step in &p.steps {
        if uk_supplier::unused_screening_slot(&task, step) {
            continue;
        }
        let input = step.service.inputs(&task, e, &c.vocabulary).unwrap();
        if step.service.api_slug == "opensanctions" {
            assert_eq!(input["queries"]["q1"]["schema"], "Person");
            assert_ne!(input["queries"]["q1"]["properties"]["name"][0], "Anthropic");
        }
        if step.service.api_slug == "tavily" {
            let q = input["query"].as_str().unwrap().to_lowercase();
            assert!(
                q.contains("anthropic") && q.ends_with("uk company vat registration number gb")
            );
        }
        let mut body = step.service.sample_payload.clone().unwrap();
        if step.service.contract.capability_slug == "company.uk-registry-search" {
            body = json!({"items":[{"title":"ANTHROPIC LIMITED","company_number":"14604577"}]});
        }
        if step.service.contract.capability_slug == "company.registry-profile" {
            body["company_name"] = json!("ANTHROPIC LIMITED");
            body["company_number"] = json!("14604577");
        }
        let extracted = step.service.contract.extract(&body, &c.vocabulary).unwrap();
        assert!(extracted.rejected.is_empty(), "{:?}", extracted.rejected);
        for subject in extracted.subjects {
            for f in subject.facts {
                fact(&mut task, e, &f.fact_type, f.value);
            }
        }
        calls.push(step.service.contract.capability_slug.as_str());
    }
    assert_eq!(
        calls.len(),
        9,
        "only two directors; the remaining unused slot does not dispatch"
    );
}

fn merlin() -> adapter::Prepared {
    adapter::prepare(
        "ask-merlin",
        "GET",
        "https://askmerlin.com/api/lookup?q=GB220430231&max_age=3600",
    )
    .unwrap()
    .unwrap()
}
fn response() -> Value {
    json!({"type":"ukvat","data":{"vatNumber":"GB220430231","countryCode":"GB","valid":true,"status":"valid","name":"TESCO PLC","source":"live","checkedAt":Utc::now().to_rfc3339(),"stale":false}})
}
fn normalize(v: &Value) -> Result<Value, String> {
    adapter::normalize_bytes(&merlin(), &serde_json::to_vec(v).unwrap())
}
#[test]
fn uk_vat_rejects_unknown_stale_wrong_identity_and_inconsistent_payloads() {
    assert_eq!(normalize(&response()).unwrap()["data"]["valid"], true);
    let mut invalid = response();
    invalid["data"]["valid"] = json!(false);
    invalid["data"]["status"] = json!("invalid");
    invalid["data"]["name"] = Value::Null;
    assert_eq!(normalize(&invalid).unwrap()["data"]["valid"], false);
    for (key, value) in [
        ("stale", json!(true)),
        ("status", json!("unknown")),
        ("vatNumber", json!("GB471144661")),
        ("checkedAt", json!("2020-01-01T00:00:00Z")),
        ("valid", json!(false)),
        ("name", Value::Null),
        ("source", json!("archive")),
    ] {
        let mut v = response();
        v["data"][key] = value;
        assert!(normalize(&v).is_err(), "{key}");
    }
    for url in [
        "http://askmerlin.com/api/lookup?q=GB220430231",
        "https://askmerlin.com/api/lookup?q=Anthropic",
        "https://askmerlin.com/api/lookup?q=GB220430231&at=2020-01-01",
        "https://askmerlin.com/api/lookup?q=GB220430231&max_age=86400",
        "https://askmerlin.com/api/lookup?q=GB220430231&q=GB471144661",
    ] {
        assert!(adapter::prepare("ask-merlin", "GET", url).is_err());
    }
}

#[test]
fn no_number_or_ambiguous_numbers_cannot_bind_a_vat_purchase() {
    let c = catalog();
    let search = c
        .services
        .iter()
        .find(|s| s.contract.capability_slug == "company.uk-vat-candidate-search")
        .unwrap();
    for content in [
        "No published identifier found",
        "GB220430231 and GB471144661",
    ] {
        let out = search
            .contract
            .extract(
                &json!({"results":[{"url":"https://example.test","content":content}]}),
                &c.vocabulary,
            )
            .unwrap();
        assert!(!out
            .subjects
            .iter()
            .flat_map(|s| &s.facts)
            .any(|f| f.fact_type == "vat.uk.candidate_number"));
        assert!(out
            .subjects
            .iter()
            .flat_map(|s| &s.facts)
            .any(|f| f.fact_type == "vat.uk.search_evidence"));
    }
}
