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
fn official_mcp_capabilities_bind_the_right_entity_and_keep_filters() {
    let c = Catalog::build(serde_json::from_str(include_str!("fixtures/crunchbase/official-mcp.json")).unwrap());
    assert!(c.rejected.is_empty(), "{:?}", c.rejected);
    assert_eq!(c.services.len(), 10);
    for question in ["Find founders and board members of Starbucks", "Find companies by industry and location", "Find seed funding rounds and their investors"] {
        let index = c.operation_index_for(question).to_string();
        for slug in ["company.crunchbase-profile", "person.crunchbase-search", "funding.crunchbase-search", "company.crunchbase-filter"] {
            assert!(index.contains(slug), "{slug} is missing from the planner index");
        }
        assert!(!index.contains("jina.ai"));
    }
    assert!(c.services.iter().all(|s| s.api_slug == "apimarket-magicapi-crunchbase-bbcca0a6"));
    for s in &c.services {
        let entity = Uuid::new_v4();
        let mut task = Task::default();
        for required in &s.contract.requires {
            if required.optional { continue; }
            let value = match required.fact_type.as_str() {
                "crunchbase.entity_autocomplete_query" => json!("Starbucks"),
                "crunchbase.entity_autocomplete_collection_ids" => json!("locations"),
                "crunchbase.search_query_predicates" => json!([{"field_id":"announced_on","operator_id":"gte","values":["2026-06-01"]}]),
                _ => json!("example-profile"),
            };
            task.facts.push(Fact { entity_ref:entity,semantic_type:required.fact_type.clone(),value,
                provenance:json!({"kind":"user_input"}),observed_at:chrono::Utc::now(),
                expires_at:chrono::Utc::now()+chrono::Duration::hours(1),verification:"user_asserted".into() });
        }
        let intent = Intent { subjects:vec![Subject { entity_ref:Some(entity),label:"Crunchbase".into(),values:BTreeMap::new() }],
            goals:vec![Goal { capability:s.contract.capability_slug.clone(),subject:0,constraints:BTreeMap::from([("source".into(),json!(s.api_slug))]) }],
            answer_operations:vec![],unresolved_references:vec![] };
        let Resolution::Plan(plan) = resolve(&c,&task,&intent) else { panic!("{} did not resolve",s.contract.capability_slug) };
        assert_eq!(plan.steps.len(),1);
        let inputs = s.inputs(&task,entity,&c.vocabulary).unwrap();
        match s.contract.capability_slug.as_str() {
            "person.crunchbase-search" => assert_eq!(inputs["collection_ids"],"people"),
            "company.crunchbase-official-search" => assert_eq!(inputs["collection_ids"],"organizations"),
            "person.crunchbase-profile" => assert_eq!(inputs["entity_def_id"],"person"),
            "funding.crunchbase-profile" => assert_eq!(inputs["entity_def_id"],"funding_round"),
            "funding.crunchbase-search" => { assert_eq!(inputs["collection_id"],"funding_rounds"); assert_eq!(inputs["predicates"][0]["values"][0],"2026-06-01"); },
            _ => (),
        }
        let output=s.contract.extract(s.sample_payload.as_ref().unwrap(),&c.vocabulary).unwrap();
        assert!(output.rejected.is_empty(),"{:?}",output.rejected);
        assert_eq!(output.subjects.len(),1);
        assert!(s.contract.extract(&json!({"error":"oauth_required"}),&c.vocabulary).unwrap().subjects.is_empty());
    }
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
        assert_eq!(s.list_atomic, 86_956);
        assert_eq!(s.total(), 100_000);
        assert_eq!(plan.cap_atomic, 100_000);
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
