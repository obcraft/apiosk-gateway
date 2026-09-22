use apiosk_gateway_v2::{catalog::{Catalog, Snapshot}, parser::{Goal,Intent,Subject}, resolver::Resolution, workflow_catalog::plan_preview};
use serde_json::json;
use std::collections::{BTreeMap,BTreeSet};
fn catalog()->Catalog {
 let s:Snapshot=serde_json::from_str(include_str!("fixtures/source-operations/catalog.json")).unwrap();
 let c=Catalog::build(s);assert!(c.rejected.is_empty(),"{:?}",c.rejected);c
}
#[test]
fn every_operation_is_visible_in_the_equal_standing_index() {
 let c=catalog();let index=c.operation_index();
 for s in &c.services {assert!(index.get(&s.contract.capability_slug).is_some());}
 for source in ["filingspulse","compliancepulse","stableenrich","browserbase","companies-house"] {
  assert!(c.services.iter().any(|s|s.api_slug==source),"{source}");
 }
 let filing=c.services.iter().find(|s|s.api_slug=="filingspulse"&&s.path.ends_with("/quarterly")).unwrap();
 let selected=BTreeSet::from([filing.contract.capability_slug.clone()]);
 let view=c.selected_parser_view(&selected);
 assert_eq!(view["capabilities"],json!([filing.contract.capability_slug]));
 assert_eq!(view["service_inputs"].as_array().unwrap().len(),1);
 assert_eq!(view["input_definitions"].as_array().unwrap().len(),1);
 assert!(view.to_string().len()<c.parser_view().to_string().len()/3);
}
#[test]
fn filings_quarterly_binds_the_company_and_quotes_the_exact_operation() {
 let c=catalog();let s=c.services.iter().find(|s|s.api_slug=="filingspulse"&&s.path.ends_with("/quarterly")).unwrap();
 let fact=s.contract.requires.iter().find(|r|!r.optional).unwrap().fact_type.clone();
 let intent=Intent{subjects:vec![Subject{entity_ref:None,label:"Apple".into(),values:BTreeMap::from([(fact,json!("Apple"))])}],
 goals:vec![Goal{subject:0,capability:s.contract.capability_slug.clone(),constraints:BTreeMap::from([("source".into(),json!("pulsenetwork"))])}],
 answer_operations:vec![],unresolved_references:vec![]};
 let Resolution::Plan(plan)=plan_preview(&c,&intent)else{panic!("not quoted")};
 assert_eq!(plan.steps.len(),1);assert_eq!(plan.steps[0].service.endpoint_id,s.endpoint_id);
 assert_eq!(plan.cap_atomic,s.total());
 let readiness=apiosk_gateway_v2::readiness::assess(&c,&["filingspulse"]);
 assert_eq!(readiness["contracts"]["sample_checked_endpoints"],0);
 assert_eq!(readiness["execution_evidence"]["state"],"unmeasured");
 assert!(readiness["contracts"]["document_retrieval_endpoints"].as_u64().unwrap()>0);
}
#[test]
fn equivalent_sources_compete_by_price_and_explicit_choice_still_wins() {
 let c=catalog();let candidates:Vec<_>=c.services.iter().filter(|s|s.contract.capability_slug=="web.source-fetch").collect();
 assert_eq!(candidates.len(),2);
 let mut intent=Intent{subjects:vec![Subject{entity_ref:None,label:"Page".into(),values:BTreeMap::from([("web.source_page_url".into(),json!("https://example.com"))])}],
 goals:vec![Goal{subject:0,capability:"web.source-fetch".into(),constraints:BTreeMap::new()}],answer_operations:vec![],unresolved_references:vec![]};
 let Resolution::Plan(plan)=plan_preview(&c,&intent)else{panic!("not quoted")};
 assert_eq!(plan.cap_atomic,candidates.iter().map(|s|s.total()).min().unwrap());
 for candidate in candidates {
  intent.goals[0].constraints.insert("source".into(),json!(candidate.api_slug));
  let Resolution::Plan(plan)=plan_preview(&c,&intent)else{panic!("explicit source not quoted")};
  assert_eq!(plan.steps[0].service.api_slug,candidate.api_slug);
 }
}
