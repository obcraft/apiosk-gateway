use apiosk_gateway_v2::{catalog::{Catalog,Snapshot},parser::{Intent,Subject,Goal},resolver::{resolve,Resolution},state::Task};
use serde_json::{json,Value};
use std::collections::BTreeMap;
use uuid::Uuid;
fn catalog()->Catalog { Catalog::build(serde_json::from_str::<Snapshot>(include_str!("fixtures/cbs-construction.json")).unwrap()) }
fn intent(period: &str,sector:&str)->Intent { Intent{subjects:vec![Subject{entity_ref:Some(Uuid::nil()),label:"Nederlandse bouwsector".into(),values:BTreeMap::new()}],goals:vec![Goal{subject:0,capability:"statistics.cbs-construction-annual".into(),constraints:BTreeMap::from([("source".into(),json!("cbs-statline")),("period".into(),json!(period)),("sector".into(),json!(sector))])}],answer_operations:vec!["Vergelijk de gevraagde jaren".into()],unresolved_references:vec![]} }
#[test]
fn construction_series_is_complete_and_joins_to_verified_units_periods_and_status() {
 let c=catalog();assert!(c.rejected.is_empty(),"{:?}",c.rejected);assert_eq!(c.services.len(),6);
 for s in &c.services {assert!(s.inputs(&Task::default(),Uuid::nil(),&c.vocabulary).is_ok());assert!(s.contract.extract(s.sample_payload.as_ref().unwrap(),&c.vocabulary).unwrap().rejected.is_empty());}
 let data=|suffix:&str|->&Value { c.services.iter().find(|s|s.contract.capability_slug.ends_with(suffix)).unwrap().sample_payload.as_ref().unwrap() };
 let obs=data("-annual");let rows=obs["value"].as_array().unwrap();assert_eq!(obs["@odata.count"].as_u64().unwrap() as usize,rows.len());assert!(obs.get("@odata.nextLink").is_none());
 for row in rows {assert_eq!(row["BedrijfstakkenBranchesSBI2008"],"350000");assert_eq!(row["Bedrijfsgrootte"],"WP19077");assert!(row["Perioden"].as_str().unwrap().ends_with("JJ00"));assert!(data("-measures")["value"].as_array().unwrap().iter().any(|m|m["Identifier"]==row["Measure"]));assert!(data("-periods")["value"].as_array().unwrap().iter().any(|p|p["Identifier"]==row["Perioden"]&&p["Status"].is_string()));}
 let growth=rows.iter().find(|r|r["Perioden"]=="2025JJ00"&&r["Measure"]=="M004926").unwrap();assert_eq!(growth["Value"],6.9);
 assert_eq!(data("-sectors")["value"][0]["Title"],"F Bouwnijverheid");assert_eq!(data("-metadata")["Identifier"],"85809NED");
}
#[test]
fn reviewed_annual_scope_preserves_years_and_refuses_quarters_or_other_sectors() {
 let c=catalog();let t=Task::default();let i=intent("2024-2025","bouwsector");let Resolution::Plan(p)=resolve(&c,&t,&i) else {panic!("annual comparison must plan")};assert_eq!(p.steps.len(),1);assert_eq!(i.goals[0].constraints["period"],"2024-2025");
 for (period,sector) in [("2025KW01","bouwsector"),("2024-01-01","bouwsector"),("2024-2025","bouwinstallatie"),("2024-2025","horeca")] {assert!(matches!(resolve(&c,&t,&intent(period,sector)),Resolution::Unsupported(_)),"{period} {sector}");}
 let mut snapshot:Snapshot=serde_json::from_str(include_str!("fixtures/cbs-construction.json")).unwrap();for s in &mut snapshot.services {if s.contract.capability_slug.ends_with("-annual"){s.fixed_params["$top"]=json!(2);}}
 assert!(matches!(resolve(&Catalog::build(snapshot),&t,&i),Resolution::Unsupported(_)),"a truncated preset must not satisfy a time range");
}
