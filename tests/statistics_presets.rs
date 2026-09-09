use apiosk_gateway_v2::{catalog::{Catalog,Snapshot},state::{Fact,Task}};
use chrono::Utc;
use serde_json::{json,Value};
use uuid::Uuid;
#[test]
fn source_backed_presets_need_no_invented_identifiers() {
    let c=Catalog::build(serde_json::from_str::<Snapshot>(include_str!("fixtures/statistics-presets.json")).unwrap());
    assert!(c.rejected.is_empty(),"{:?}",c.rejected);
    assert_eq!(c.services.len(),5);
    let entity=Uuid::new_v4();
    let mut task=Task::default();
    task.facts.push(Fact{entity_ref:entity,semantic_type:"eurostat.input.time".into(),value:json!("2023"),provenance:json!({"kind":"user_asserted"}),verification:"user_asserted".into(),observed_at:Utc::now(),expires_at:Utc::now()+chrono::Duration::hours(1)});
    for s in &c.services {
        let data=s.sample_payload.as_ref().unwrap();
        let e=s.contract.extract(data,&c.vocabulary).unwrap();
        assert!(e.rejected.is_empty());
        for k in s.contract.output_map.produces.keys() {assert!(e.subjects[0].fact(k).is_some());}
        let inputs=s.inputs(&task,entity,&c.vocabulary).unwrap();
        if s.api_slug=="eurostat" {
            assert_eq!(inputs["time"],"2023"); assert_eq!(inputs["dataset"],"nama_10_gdp");
            assert_eq!(inputs["unit"],"CP_MEUR"); assert_eq!(inputs["na_item"],"B1GQ");
            assert_eq!(data["dimension"]["time"]["category"]["index"]["2023"],0);
            assert!(s.inputs(&Task::default(),entity,&c.vocabulary).is_err(),"Missing year must ask, not guess");
        } else {
            assert_eq!(inputs["tableId"],"82244NED");
            let rows=data["value"].as_array().unwrap();assert!(!rows.is_empty());
            assert!(rows.iter().all(|r|r["Identifier"].is_string()&&r["Title"].is_string()));
            if s.path.ends_with("MeasureCodes") {assert_eq!(rows[0]["Unit"],"Aantal");}
            if s.path.ends_with("TypeGefailleerdeCodes") {assert_eq!(rows.len(),3);}
            assert_eq!(data.get("@odata.nextLink"),None::<&Value>,"Codebook must be complete");
        }
    }
}
