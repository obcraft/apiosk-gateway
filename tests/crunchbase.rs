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
fn official_live_company_people_and_funding_are_preserved() {
    let c=Catalog::build(serde_json::from_str(include_str!("fixtures/crunchbase/official-mcp.json")).unwrap());
    let records:Vec<Value>=serde_json::from_str(include_str!("../crates/commerce-runtime/tests/fixtures/crunchbase-mcp-live.json")).unwrap();
    for record in &records {
        let envelope=json!({"source":"Crunchbase","transport":"official_mcp","tool":record["name"],"data":record["data"],"retrieved_at":record["at"]});
        for service in c.services.iter().filter(|s| s.path==format!("/mcp/{}",record["name"].as_str().unwrap())
            && (record["name"]!="cb_entity_get" || s.fixed_params["entity_def_id"]==record["arguments"]["entity_def_id"])) {
            let output=service.contract.extract(&envelope,&c.vocabulary).unwrap();
            assert!(output.rejected.is_empty());assert_eq!(output.subjects.len(),1);
            if record["name"]=="cb_entity_get" {
                // The authenticated tools/list output schema explicitly
                // permits null labels and nullable relationship entities.
                let mut nullable=envelope.clone();
                nullable["data"]["properties"][0]["label"]=Value::Null;
                nullable["data"]["cards"][0]["label"]=Value::Null;
                nullable["data"]["cards"][0]["entity_def_id"]=Value::Null;
                nullable["data"]["cards"][0]["entities"]=Value::Null;
                assert!(service.contract.extract(&nullable,&c.vocabulary).unwrap().rejected.is_empty());
            }
        }
    }
    let company=records.iter().find(|r| r["name"]=="cb_entity_get"&&r["arguments"]["entity_def_id"]=="organization").unwrap();
    let cards=company["data"]["cards"].as_array().unwrap();
    for id in ["founders_lookup","current_employees_image_list","current_advisors_image_list","funding_rounds_list","investors_list"] {
        assert!(!cards.iter().find(|c| c["card_id"]==id).unwrap()["entities"].as_array().unwrap().is_empty());
    }
}

#[test]
#[ignore = "operator-only: production catalog SQL snapshot after activation"]
fn activated_database_catalog_is_indexed_and_priced() {
    let path=std::env::var("APIOSK_CRUNCHBASE_CATALOG_SNAPSHOT").expect("catalog snapshot path required");
    let c=Catalog::build(serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap());
    assert!(c.rejected.is_empty(),"{:?}",c.rejected);
    let services:Vec<_>=c.services.iter().filter(|s| s.path.starts_with("/mcp/")).collect();
    assert_eq!(services.len(),10);
    for service in &services {
        assert_eq!(service.list_atomic,47_258);assert_eq!(service.total(),54_347);
        assert_eq!(service.source_url.as_deref(),Some("https://data.crunchbase.com/docs/tool-reference"));
        assert!(service.contract.extract(service.sample_payload.as_ref().unwrap(),&c.vocabulary).unwrap().rejected.is_empty());
    }
    for question in ["Starbucks founders and board members from Crunchbase", "Crunchbase funding rounds and investors", "Find people in Crunchbase"] {
        let index=c.operation_index_for(question).to_string();
        for capability in ["company.crunchbase-profile","person.crunchbase-search","funding.crunchbase-search"] {
            assert!(index.contains(capability),"{capability} missing from live catalog index");
        }
    }
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
        // The same tariff applies to every official MCP action. Reference
        // lookup is a paid trigger too; internal Jina enrichment adds no step.
        assert_eq!(s.list_atomic, 47_258);
        assert_eq!(s.total(), 54_347);
        assert_eq!(plan.cap_atomic, 54_347);
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
fn ask_discovers_one_company_profile_for_its_available_relationships() {
    let c = Catalog::build(serde_json::from_str(include_str!("fixtures/crunchbase/official-mcp.json")).unwrap());
    for (slug, name, outputs) in [
        ("company.profile", "Company profile", vec!["company_profile", "description", "website"]),
        ("company.profile", "Company profile", vec!["profile"]),
        ("company.founders", "Company founders", vec!["founders"]),
        ("company.employees", "Company employees", vec!["employees"]),
        ("company.board_members", "Company board members", vec!["board_members"]),
        ("company.funding_rounds", "Company funding rounds", vec!["funding_rounds"]),
        ("company.funding", "Company funding rounds", vec!["funding_rounds", "investors"]),
        ("company.investors", "Company investors", vec!["investors"]),
    ] {
        let parsed = apiosk_gateway_v2::capability_intake::normalize(&json!({
            "language":"en", "subjects":[{"id":"s1","label":"Starbucks","type":"company","identifiers":{"url":"https://www.crunchbase.com/organization/starbucks"}}],
            "capabilities":[{"slug":slug,"name":name,"description":format!("Retrieve available {name} from Crunchbase."),"domain":"company","inputs":["company_name"],"outputs":outputs,"subject_id":"s1","source_hint":"Crunchbase","confidence":1.0}],
            "deliverable":{"format":"chat","operations":[]}
        }).to_string(), "Use Crunchbase for the Starbucks company profile, founders, employees, board members, funding rounds and investors.", "fixture").unwrap();
        let found = apiosk_gateway_v2::ask_v2::search::search(&parsed, &c);
        assert!(found[0].operations.contains("company.crunchbase-profile"), "{slug}: {:?}", found[0].operations);
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
