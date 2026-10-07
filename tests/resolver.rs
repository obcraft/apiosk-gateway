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
        known_sources: vec![],
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
fn value_selection_compares_complete_chain_cost_across_payment_sources() {
    let (catalog, task, intent) = setup();
    let mut services = catalog.services.clone();
    services[1].api_slug = "x402-accounts".into();
    // 100 + a required 20 lookup costs more than the 110 direct call, even
    // though the direct endpoint's advertised unit price is higher.
    let mut direct = service("company.accounts", json!([{"fact_type":"company.name"}]),
        json!({"company.accounts":{"path":"$.accounts"}}), 110);
    direct.api_slug = "native-accounts".into();
    let direct_id = direct.id;
    services.push(direct);
    let catalog = Catalog::build(Snapshot { services, known_sources: vec![], vocabulary: catalog.vocabulary.values().cloned().collect() });
    let Resolution::Plan(plan) = resolve(&catalog, &task, &intent) else { panic!("complete plan") };
    assert_eq!(plan.steps.len(), 1);
    assert_eq!(plan.steps[0].service.id, direct_id);
    assert_eq!(plan.cap_atomic, catalog.services.iter().find(|service| service.id == direct_id).unwrap().total());

    // Payment protocol/provider identity is not an artificial preference.
    let mut services = catalog.services.clone();
    for service in &mut services {
        if service.id == direct_id { service.api_slug = "x402-direct".into(); }
    }
    let catalog = Catalog::build(Snapshot { services, known_sources: vec![], vocabulary: catalog.vocabulary.values().cloned().collect() });
    let Resolution::Plan(plan) = resolve(&catalog, &task, &intent) else { panic!("complete plan") };
    assert_eq!(plan.steps[0].service.id, direct_id);
}

#[test]
fn equal_total_price_prefers_fewer_paid_calls() {
    let (catalog, task, intent) = setup();
    let mut services = catalog.services.clone();
    let direct = service("company.accounts", json!([{"fact_type":"company.name"}]),
        json!({"company.accounts":{"path":"$.accounts"}}), 120);
    let direct_id = direct.id;
    services.push(direct);
    let catalog = Catalog::build(Snapshot { services, known_sources: vec![], vocabulary: catalog.vocabulary.values().cloned().collect() });
    let Resolution::Plan(plan) = resolve(&catalog, &task, &intent) else { panic!("complete plan") };
    assert_eq!(plan.cap_atomic, 138);
    assert_eq!(plan.steps.len(), 1, "same buyer price should avoid an unnecessary paid dependency");
    assert_eq!(plan.steps[0].service.id, direct_id);
}

#[test]
fn two_companies_search_first_and_reuse_only_their_own_identifiers() {
    let (catalog, mut task, mut intent) = setup();
    let second = Uuid::new_v4();
    let mut name = task.facts[0].clone();
    name.entity_ref = second; name.value = json!("Other"); task.facts.push(name.clone());
    intent.subjects.push(Subject { entity_ref: Some(second), label: "Other".into(), values: BTreeMap::new() });
    intent.goals.push(Goal { subject: 1, ..intent.goals[0].clone() });
    let Resolution::Plan(plan) = resolve(&catalog, &task, &intent) else { panic!() };
    assert_eq!(plan.cap_atomic, 276);
    assert_eq!(plan.steps.iter().map(|s| s.service.contract.capability_slug.as_str()).collect::<Vec<_>>(), vec!["company.search","company.search","company.accounts","company.accounts"]);
    assert_eq!(plan.steps[0].entity_ref, plan.steps[2].entity_ref);
    assert_eq!(plan.steps[1].entity_ref, plan.steps[3].entity_ref);
    name.semantic_type = "company.id".into(); name.value = json!("second-id"); task.facts.push(name);
    let Resolution::Plan(plan) = resolve(&catalog, &task, &intent) else { panic!() };
    assert_eq!(plan.steps.len(), 3);
    assert_eq!(plan.steps.iter().filter(|s| s.service.contract.capability_slug == "company.search").count(), 1);
    assert_eq!(plan.steps.iter().find(|s| s.entity_ref == second).unwrap().service.inputs(&task, second, &catalog.vocabulary).unwrap()["company.id"], "second-id");
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
fn named_source_uses_its_own_identifier_search_before_a_cheaper_reseller() {
    let (mut catalog, task, mut intent) = setup();
    for s in &mut catalog.services { s.api_slug = "companies-house".into(); }
    let mut reseller = catalog.services[0].clone();
    reseller.id = Uuid::new_v4();
    reseller.api_slug = "sirenic".into();
    reseller.list_atomic = 1;
    // Sirenic's EU search may return a GB ID, but not for every result.
    reseller.optional_outputs = vec!["company.id".into()];
    let index = catalog.services.len();
    catalog.services.push(reseller);
    catalog.by_output.get_mut("company.id").unwrap().insert(0, index);
    intent.goals[0].constraints.insert("source".into(), json!("companies-house"));
    let Resolution::Plan(plan) = resolve(&catalog, &task, &intent) else { panic!("expected source plan") };
    assert_eq!(plan.steps.len(), 2);
    assert!(plan.steps.iter().all(|s| s.service.api_slug == "companies-house"));
}

#[test]
fn planned_optional_output_does_not_replace_the_named_sources_identifier_search() {
    let (mut catalog, task, mut intent) = setup();
    for s in &mut catalog.services { s.api_slug = "companies-house".into(); }
    let mut reseller = catalog.services[0].clone();
    reseller.id = Uuid::new_v4();
    reseller.api_slug = "sirenic".into();
    reseller.contract.capability_slug = "company.eu-search".into();
    reseller.optional_outputs = vec!["company.id".into()];
    let index = catalog.services.len();
    catalog.services.push(reseller);
    catalog.by_capability.insert("company.eu-search".into(), vec![index]);
    catalog.by_output.get_mut("company.id").unwrap().push(index);
    intent.goals.insert(0, Goal { subject: 0, capability: "company.eu-search".into(), constraints: BTreeMap::from([("source".into(), json!("sirenic"))]) });
    intent.goals[1].constraints.insert("source".into(), json!("companies-house"));
    let Resolution::Plan(plan) = resolve(&catalog, &task, &intent) else { panic!("expected source plan") };
    assert_eq!(plan.steps.len(), 3);
    assert_eq!(plan.steps.iter().filter(|s| s.service.api_slug == "companies-house").count(), 2);
}

#[test]
fn unavailable_native_dependency_does_not_fall_back_to_another_source() {
    let (mut catalog, task, mut intent) = setup();
    for s in &mut catalog.services { s.api_slug = "companies-house".into(); }
    let mut reseller = catalog.services[0].clone();
    reseller.id = Uuid::new_v4();
    reseller.api_slug = "sirenic".into();
    reseller.list_atomic = 1;
    let index = catalog.services.len();
    catalog.services.push(reseller);
    catalog.by_output.get_mut("company.id").unwrap().push(index);
    catalog.services[0].contract.requires[0].fact_type = "missing.native.input".into();
    intent.goals[0].constraints.insert("source".into(), json!("companies-house"));
    assert!(!matches!(resolve(&catalog, &task, &intent), Resolution::Plan(_)));
}

#[test]
fn companies_house_fixture_quotes_its_native_search_and_filing_history() {
    let snapshot: Snapshot = serde_json::from_str(include_str!("fixtures/uk-supplier/catalog.json")).unwrap();
    let catalog = Catalog::build(snapshot);
    let (_, mut task, mut intent) = setup();
    task.facts[0].value = json!("Revolut");
    intent.subjects[0].label = "Revolut".into();
    intent.goals[0].capability = "company.filing-history".into();
    intent.goals[0].constraints.insert("source".into(), json!("companies-house"));
    let Resolution::Plan(plan) = resolve(&catalog, &task, &intent) else { panic!("expected Companies House plan") };
    assert_eq!(plan.steps.len(), 2);
    assert!(plan.steps.iter().all(|s| s.service.api_slug == "companies-house"));
    assert_eq!(plan.steps[0].service.contract.capability_slug, "company.uk-registry-search");
    assert_eq!(plan.steps[1].service.contract.capability_slug, "company.filing-history");
}

#[test]
fn filing_history_analysis_period_does_not_block_a_native_quote() {
    let catalog = Catalog::build(serde_json::from_str(include_str!("fixtures/uk-supplier/catalog.json")).unwrap());
    let (_, mut task, mut intent) = setup();
    task.facts[0].value = json!("Revolut");
    intent.subjects[0].label = "Revolut".into();
    intent.goals[0].capability = "company.filing-history".into();
    intent.goals[0].constraints.insert("source".into(), json!("companies-house"));
    // This is the saved intent that produced the user's unnecessary question.
    intent.answer_operations = vec!["List and summarize the most important corporate events over the last 3 years from the filing history in English.".into()];
    for period in ["3 years", "last 3 years", "afgelopen drie jaar", "2023-09-22 to 2026-09-22"] {
        intent.goals[0].constraints.insert("period".into(), json!(period));
        let original = serde_json::to_value(&intent).unwrap();
        let Resolution::Plan(plan) = resolve(&catalog, &task, &intent) else { panic!("analysis period must not require clarification: {period}") };
        assert_eq!(plan.steps.len(), 2);
        assert!(plan.steps.iter().all(|step| step.service.api_slug == "companies-house"));
        assert_eq!(plan.steps[1].service.contract.capability_slug, "company.filing-history");
        assert_eq!(serde_json::to_value(&intent).unwrap(), original, "preserve the requested period for the answer");
    }
    for (key, value) in [("coverage", json!("complete")), ("source_period_filter", json!(true)), ("filing_selection", json!("latest_two"))] {
        let mut strict = intent.clone();
        strict.goals[0].constraints.insert(key.into(), value);
        assert!(matches!(resolve(&catalog, &task, &strict), Resolution::Unsupported(_)), "{key} must not become a claim about one returned page");
    }
    let (catalog, task, mut accounts) = setup();
    accounts.goals[0].constraints.insert("period".into(), json!("2024 and 2025"));
    assert!(matches!(resolve(&catalog, &task, &accounts), Resolution::Unsupported(_)), "one available account is not two requested years");
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

#[test]
fn related_services_need_fresh_unambiguous_inputs_and_exclude_repeats() {
    use apiosk_gateway_v2::{followups::related_services, state::Entity};
    let (catalog, mut task, _) = setup();
    let entity = task.facts[0].entity_ref;
    task.entities.push(Entity {
        entity_ref: entity,
        label: "Example".into(),
        selected: true,
    });
    task.result_refs.push(Uuid::new_v4());
    task.completed
        .push(format!("{}:{}", catalog.services[0].id, entity));
    assert!(
        related_services(&catalog, &task).is_empty(),
        "missing identifiers must not be guessed"
    );
    let mut id = task.facts[0].clone();
    id.semantic_type = "company.id".into();
    id.value = json!("123");
    task.facts.push(id.clone());
    assert_eq!(related_services(&catalog, &task).len(), 1);
    task.facts.last_mut().unwrap().expires_at = Utc::now() - chrono::Duration::seconds(1);
    assert!(related_services(&catalog, &task).is_empty());
    task.facts.last_mut().unwrap().expires_at = id.expires_at;
    id.value = json!("456");
    task.facts.push(id);
    assert!(
        related_services(&catalog, &task).is_empty(),
        "conflicting identifiers must not be chosen silently"
    );
    task.facts.pop();
    task.completed
        .push(format!("{}:{}", catalog.services[1].id, entity));
    assert!(
        related_services(&catalog, &task).is_empty(),
        "completed endpoints are not resold"
    );
}

#[test]
fn specific_goal_reuses_its_registry_profile_instead_of_buying_another_country() {
    let (base, task, mut intent)=setup();
    let mut nl=service("company.profile",json!([{"fact_type":"company.id"}]),json!({"company.nl.profile":{"path":"$.id"}}),50);
    nl.api_slug="nl-register".into();
    let mut fr=service("company.profile",json!([{"fact_type":"company.name"}]),json!({"company.fr.profile":{"path":"$.id"}}),1);
    fr.api_slug="fr-register".into(); fr.jurisdiction=Some("FR".into());
    let address=service("location.address",json!([{"fact_type":"company.nl.profile"}]),json!({"location.address":{"path":"$.id"}}),20);
    let mut services=base.services.clone(); services.extend([nl,fr,address]);
    let mut vocabulary:Vec<_>=base.vocabulary.values().cloned().collect();
    for kind in ["company.nl.profile","company.fr.profile","location.address"] { vocabulary.push(FactType{slug:kind.into(),value_schema:json!({"type":"string"})}); }
    let catalog=Catalog::build(Snapshot{services,vocabulary,known_sources:vec![]});
    assert!(catalog.rejected.is_empty());
    intent.goals=vec![Goal{capability:"company.profile".into(),subject:0,constraints:BTreeMap::new()},Goal{capability:"location.address".into(),subject:0,constraints:BTreeMap::new()}];
    let Resolution::Plan(plan)=resolve(&catalog,&task,&intent) else {panic!("expected plan")};
    assert_eq!(plan.steps.len(),3);
    assert!(!plan.steps.iter().any(|s|s.service.api_slug=="fr-register"));
    // A large alternative catalog must not exhaust the search budget before
    // reusing the profile already required by the address check.
    let mut crowded = catalog.clone();
    let fr_index = crowded.services.iter().position(|s| s.api_slug == "fr-register").unwrap();
    let nl_index = crowded.services.iter().position(|s| s.api_slug == "nl-register").unwrap();
    let mut alternatives = Vec::new();
    for _ in 0..300 {
        let mut alternative = crowded.services[fr_index].clone();
        alternative.id = Uuid::new_v4();
        alternative.endpoint_id = Uuid::new_v4();
        alternatives.push(crowded.services.len());
        crowded.services.push(alternative);
    }
    alternatives.push(nl_index);
    crowded.by_capability.insert("company.profile".into(), alternatives);
    let Resolution::Plan(reused) = resolve(&crowded, &task, &intent) else { panic!("reuse must not need search budget") };
    assert_eq!(reused.steps.len(), 3);
    assert!(!reused.steps.iter().any(|s| s.service.api_slug == "fr-register"));
    intent.goals[0].constraints.insert("source".into(),json!("fr-register"));
    let Resolution::Plan(plan)=resolve(&catalog,&task,&intent) else {panic!("explicit source must remain")};
    assert!(plan.steps.iter().any(|s|s.service.api_slug=="fr-register"));
}
