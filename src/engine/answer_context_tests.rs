use super::*;
use crate::{catalog::Snapshot, parser::{Goal, Subject}, resolver::Step};
use std::collections::BTreeMap;

fn fixture(address: &str, code: &str) -> (Plan, Intent, Task, BTreeMap<Uuid, Value>) {
    let snapshot: Snapshot = serde_json::from_str(include_str!("../../tests/fixtures/dutch-links/catalog.json")).unwrap();
    let entity = Uuid::new_v4();
    let mut steps: Vec<Step> = ["location.bag-address-context", "area.cbs-neighbourhood-2025"]
        .into_iter().enumerate().map(|(goal_index, capability)| Step {
            service: snapshot.services.iter().find(|s|s.contract.capability_slug == capability).unwrap().clone(),
            entity_ref: entity, goal_index, browser_fallback: None,
        }).collect();
    // The third independently requested source is irrelevant to the join.
    // Its fixture service only needs the same planning and saved-source shape.
    let mut other = steps[1].clone();
    other.service.api_slug = "koop".into();
    other.service.endpoint_id = Uuid::new_v4();
    other.service.path = "/sru".into();
    other.service.contract.capability_slug = "government.publication.search".into();
    other.goal_index = 2;
    steps.push(other);
    let intent = Intent {
        subjects: vec![Subject { entity_ref: Some(entity), label: address.into(), values: BTreeMap::from([("geo.input.q".into(), json!(address))]) }],
        goals: steps[1..].iter().map(|s|Goal {
            capability:s.service.contract.capability_slug.clone(), subject:0,
            constraints:BTreeMap::from([("source".into(),json!(s.service.api_slug)),("endpoint_id".into(),json!(s.service.endpoint_id))]),
        }).collect(),
        answer_operations:vec!["Combine the sources.".into()], unresolved_references:vec![],
    };
    let mut task = Task::default();
    task.record_turn(&format!("Find neighbourhood information for {address}. Search official publications for that address."),None);
    let mut documents = BTreeMap::new();
    for (i,step) in steps.iter().enumerate() {
        let id = Uuid::new_v4();
        task.active_results.push(id);
        task.completed.push(step.key());
        documents.insert(id,json!({
            "source":{"provider":step.service.api_slug,"endpoint_id":step.service.endpoint_id,"path":step.service.path,"contract_version":step.service.contract.version},
            "subject":{"label":address,"entity_ref":entity},"coverage":"source_response","missing_fields":[],
            "data":match i {
                0 => json!({"items":[{"address":address,"neighbourhood_code":code}]}),
                1 => json!({"items":[{"buurtcode":code,"jaar":2025,"aantal_inwoners":0}]}),
                _ => json!({"query":{"q":address},"total":0}),
            }
        }));
    }
    let plan = Plan { quote_ref:Uuid::new_v4(), steps, cap_atomic:1, expires_at:Utc::now(), hash:"fixture".into(), fee_bps:1500, workflow_slug:None };
    (plan,intent,task,documents)
}

#[test]
fn compound_location_identity_keeps_exact_saved_address_join_for_variable_values() {
    for (address,code) in [("Damrak 1, Amsterdam","BU0363AD03"),("Kerkstraat 17, Utrecht","BU03440101")] {
        let (plan,intent,mut task,documents) = fixture(address,code);
        for wording in ["Combine these records.","Geef de buurt met bijbehorende publicaties."] {
            task.record_turn(wording,None);
            let before = documents.clone();
            let sources = answer_sources(&plan,&intent,&task,&documents);
            assert_eq!(sources.len(),3);
            let source = &sources[0];
            assert_eq!(source["result_ref"],json!(task.active_results[0]));
            assert_eq!(source["document"].pointer("/data/items/0/address"),Some(&json!(address)));
            assert_eq!(source["document"].pointer("/data/items/0/neighbourhood_code"),Some(&json!(code)));
            assert_eq!(source["document"],documents[&task.active_results[0]]);
            assert_eq!(documents,before,"source data is not rewritten");
            assert_eq!(sources[1]["document"]["data"]["items"][0]["aantal_inwoners"],0);
        }
    }
}

#[test]
fn compound_location_identity_needs_requested_target_and_real_contract_dependency() {
    let (plan,intent,task,documents) = fixture("Example street 8, Delft","BU05030101");
    for mutation in ["no_goal","no_target","other_entity","other_target_source","other_dependency_source","wrong_route","wrong_output","wrong_input","source_constraint","endpoint_constraint","single_provider"] {
        let (mut plan,mut intent) = (plan.clone(),intent.clone());
        match mutation {
            "no_goal" => {intent.goals.remove(0);},
            "no_target" => {plan.steps.remove(1);},
            "other_entity" => plan.steps[1].entity_ref=Uuid::new_v4(),
            "other_target_source" => plan.steps[1].service.api_slug="other".into(),
            "other_dependency_source" => plan.steps[0].service.api_slug="other".into(),
            "wrong_route" => plan.steps[0].service.path="/unrelated".into(),
            "wrong_output" => {plan.steps[0].service.contract.output_map.produces.remove("location.neighbourhood_code");},
            "wrong_input" => plan.steps[1].service.contract.requires.clear(),
            "source_constraint" => {intent.goals[0].constraints.insert("source".into(),json!("other"));},
            "endpoint_constraint" => {intent.goals[0].constraints.insert("endpoint_id".into(),json!(Uuid::new_v4()));},
            "single_provider" => {intent.goals.pop();plan.steps.pop();},
            _ => unreachable!(),
        }
        let selected=answer_sources(&plan,&intent,&task,&documents);
        assert!(!selected.iter().any(|s|s["result_ref"]==json!(task.active_results[0])),"{mutation}");
    }
}

#[test]
fn compound_location_identity_requires_completed_exact_saved_source_binding() {
    let (plan,intent,task,documents)=fixture("Another road 9, Leiden","BU05460000");
    for key in ["provider","endpoint_id","path","contract_version","entity_ref","not_completed","failed","skipped"] {
        let (mut task,mut documents)=(task.clone(),documents.clone());
        match key {
            "entity_ref" => documents.get_mut(&task.active_results[0]).unwrap()["subject"][key]=json!(Uuid::new_v4()),
            "not_completed" => task.completed.retain(|s|s!=&plan.steps[0].key()),
            "failed" => {task.failed_steps.insert(plan.steps[0].key(),json!({"code":"incomplete_source"}));},
            "skipped" => {task.skipped_steps.insert(plan.steps[0].key());},
            _ => documents.get_mut(&task.active_results[0]).unwrap()["source"][key]=json!("mismatch"),
        }
        assert!(!answer_sources(&plan,&intent,&task,&documents).iter().any(|s|s["result_ref"]==json!(task.active_results[0])),"{key}");
    }
}

#[test]
fn compound_location_context_does_not_infer_or_repair_mismatched_raw_identity() {
    let (plan,intent,task,mut documents)=fixture("Another road 9, Leiden","BU05460000");
    documents.get_mut(&task.active_results[0]).unwrap()["data"]["items"][0]["neighbourhood_code"]=json!("DIFFERENT");
    let selected=answer_sources(&plan,&intent,&task,&documents);
    assert_eq!(selected[0]["document"]["data"]["items"][0]["neighbourhood_code"],"DIFFERENT");
    assert_eq!(selected[1]["document"]["data"]["items"][0]["buurtcode"],"BU05460000");
    assert!(selected.iter().all(|s|s.get("analysis").is_none() && s.get("completeness").is_none()));
}

#[test]
#[ignore = "offline saved-source audit; explicit local input and output paths required"]
fn audit_saved_location_identity_sources() {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let input_path = std::env::var("LOCATION_CONTEXT_AUDIT_IN").expect("saved task export path");
    let output_path = std::env::var("LOCATION_CONTEXT_AUDIT_OUT").expect("private output path");
    let input: Value = serde_json::from_slice(&std::fs::read(input_path).unwrap()).unwrap();
    let task: Task = serde_json::from_value(input["task"].clone()).unwrap();
    let intent: Intent = serde_json::from_value(task.intent.clone().unwrap()).unwrap();
    let plan: Plan = serde_json::from_value(task.proposal.clone().unwrap()).unwrap();
    let documents: BTreeMap<Uuid,Value> = input["sources"].as_array().unwrap().iter().map(|source| {
        (Uuid::parse_str(source["result_ref"].as_str().unwrap()).unwrap(),source["document"].clone())
    }).collect();
    let sources = answer_sources(&plan,&intent,&task,&documents);
    assert!(sources.iter().any(|s|s["document"]["source"]["path"]=="/apiosk/bag-address"));
    for source in &sources {
        let id=Uuid::parse_str(source["result_ref"].as_str().unwrap()).unwrap();
        assert_eq!(source["document"],documents[&id]);
    }
    let audit=json!({"question":task.conversation.last().unwrap().question,"intent":intent,
        "sources":sources,"failed_steps":task.failed_steps,"original_source_count":documents.len(),
        "boundary":"Exact production answer_sources over the saved approved task and original documents. No source or model calls."});
    let mut file=std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(output_path).unwrap();
    file.write_all(&serde_json::to_vec_pretty(&audit).unwrap()).unwrap();
    eprintln!("saved location source selection: {} of {} original documents",sources.len(),documents.len());
}
