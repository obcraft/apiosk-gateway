//! Offline runtime admission and exact input-binding audit. Never calls an API.
use apiosk_gateway_v2::{catalog::{Catalog, Snapshot}, parser::{Goal, Intent, Subject}, resolver::Resolution, workflow_catalog::plan_preview};
use serde_json::{json, Value};
use std::collections::BTreeMap;
fn value(schema: &Value)->Value {
    if let Some(value)=schema["enum"].as_array().and_then(|a|a.first()){return value.clone();}
    match schema["type"].as_str() {
        Some("integer")=>json!(schema["minimum"].as_i64().unwrap_or(1).max(1).min(schema["maximum"].as_i64().unwrap_or(i64::MAX))),
        Some("number")=>json!(schema["minimum"].as_f64().unwrap_or(1.0).max(1.0).min(schema["maximum"].as_f64().unwrap_or(f64::MAX))),
        _=>json!("Example"),
    }
}
fn main()->anyhow::Result<()> {
    let path=std::env::args().nth(1).ok_or_else(||anyhow::anyhow!("snapshot path required"))?;
    let snapshot:Snapshot=serde_json::from_slice(&std::fs::read(path)?)?;
    let count=snapshot.services.len();let catalog=Catalog::build(snapshot);
    anyhow::ensure!(catalog.rejected.is_empty(),"Rejected: {:?}",catalog.rejected);
    let mut planned=0;let mut pattern_checks=0;
    for s in &catalog.services {
        if !s.is_document_retrieval(){continue;}
        for error in [json!({"error":"upstream failed"}),json!({"status":"pending"})] {
            let extraction=s.contract.extract(&error,&catalog.vocabulary)?;
            anyhow::ensure!(extraction.subjects.iter().all(|subject|subject.facts.is_empty()),"error became a fact");
        }
        let mut values=BTreeMap::new();
        for r in s.contract.requires.iter().filter(|r|!r.optional) {
            let schema=&catalog.vocabulary[&r.fact_type].value_schema;
            let v=value(schema);
            if catalog_core::facts::schema::validate(schema,&v).is_err(){pattern_checks+=1;continue;}
            values.insert(r.fact_type.clone(),v);
        }
        if values.len()!=s.contract.requires.iter().filter(|r|!r.optional).count(){continue;}
        let intent=Intent{subjects:vec![Subject{entity_ref:None,label:"Example".into(),values}],
            goals:vec![Goal{subject:0,capability:s.contract.capability_slug.clone(),constraints:BTreeMap::new()}],answer_operations:vec![],unresolved_references:vec![]};
        let Resolution::Plan(plan)=plan_preview(&catalog,&intent) else {anyhow::bail!("cannot plan {}",s.contract.capability_slug)};
        anyhow::ensure!(plan.steps.len()==1&&plan.steps[0].service.endpoint_id==s.endpoint_id,"wrong endpoint");
        anyhow::ensure!(plan.cap_atomic==s.total(),"wrong quote");planned+=1;
    }
    println!("{} runtime-accepted document contracts; {} exact one-call quotes; {} constrained inputs checked by runtime schemas; zero provider calls",count,planned,pattern_checks);
    Ok(())
}
