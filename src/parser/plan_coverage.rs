//! Each discovered retrieval must be covered by a matched, subject-specific
//! goal. Shared goals require explicit output-contract evidence; dependencies
//! belong to the resolver and answer instructions do not require a purchase.
use super::{explicit_request::request_match, Intent};
use crate::{capability_intake::Intake, catalog::Catalog};
use anyhow::ensure;
use serde_json::{json, Value};
#[cfg(test)]
mod yelp_location_metadata_tests;
#[cfg(test)]
mod uk_registry_profile_metadata_tests;
#[cfg(test)]
mod record_shape_tests;
use std::collections::BTreeSet;

pub(super) fn prepare(
    intent: &mut Intent,
    intake: &Intake,
    catalog: &Catalog,
) -> anyhow::Result<()> {
    prepare_grounded(intent, intake, catalog, &[])
}

pub(super) fn prepare_grounded(
    intent: &mut Intent, intake: &Intake, catalog: &Catalog, bindings: &[super::source_grounding::Binding],
) -> anyhow::Result<()> {
    let parsed = &intake.parsed_request;
    let retrievals: Vec<_> = parsed
        .capabilities
        .iter()
        .enumerate()
        .filter(|(_, cap)| !answer_instruction(cap))
        .map(|(index, _)| index)
        .collect();
    ensure!(
        !intent.goals.is_empty() && intent.goals.len() <= super::MAX_GOALS,
        "each requested retrieval needs a covered goal"
    );
    let mut unique = BTreeSet::new();
    for goal in &intent.goals {
        let subject = intent
            .subjects
            .get(goal.subject)
            .ok_or_else(|| anyhow::anyhow!("invalid subject index"))?;
        ensure!(
            unique.insert((
                subject.label.to_lowercase(),
                goal.capability.clone(),
                serde_json::to_string(&goal.constraints)?
            )),
            "duplicate retrieval goal"
        );
    }
    let edges: Vec<Vec<usize>> = intent
        .goals
        .iter()
        .map(|goal| {
            parsed
                .capabilities
                .iter()
                .enumerate()
                .filter(|(index, cap)| {
                    let subject = &intent.subjects[goal.subject];
                    let same_subject = cap
                        .subject_id
                        .as_ref()
                        .and_then(|id| parsed.subjects.iter().find(|s| &s.id == id))
                        .is_none_or(|s| s.label.eq_ignore_ascii_case(&subject.label));
                    !answer_instruction(cap)
                        && same_subject
                        && request_match(intake, *index).is_some_and(|found| {
                            found.operations.contains(&goal.capability)
                                && found
                                    .apiosk
                                    .iter()
                                    .any(|candidate| candidate_matches(goal, candidate))
                        })
                })
                .map(|(index, _)| index)
                .collect()
        })
        .collect();
    // A parsed retrieval can request more than one source-backed category.
    // Pin each goal to an actual matched endpoint before testing their union.
    for (goal_index, compatible) in edges.iter().enumerate() {
        let goal = &mut intent.goals[goal_index];
        let candidate = compatible.iter().filter_map(|index| request_match(intake, *index))
            .flat_map(|matched| &matched.apiosk)
            .find(|candidate| candidate_matches(goal, candidate))
            .ok_or_else(|| anyhow::anyhow!("every parsed subject needs its requested retrieval"))?;
        if let Some(source) = &candidate.source_slug {
            goal.constraints.insert("source".into(), json!(source));
        }
        if let Some(endpoint) = &candidate.endpoint_id {
            goal.constraints.insert("endpoint_id".into(), json!(endpoint));
        }
    }
    unique.clear();
    for goal in &intent.goals {
        ensure!(unique.insert((intent.subjects[goal.subject].label.to_lowercase(),
            goal.capability.clone(), serde_json::to_string(&goal.constraints)?)), "duplicate retrieval goal");
    }
    let owners: Vec<Vec<usize>> = parsed.capabilities.iter().enumerate().map(|(cap, _)| {
        intent.goals.iter().enumerate().filter(|(goal, selected)| edges[*goal].contains(&cap)
            && request_match(intake, cap).is_some_and(|matched| matched.apiosk.iter().any(|candidate| candidate_matches(selected, candidate))))
            .map(|(goal, _)| goal).collect()
    }).collect();
    let services: Vec<Vec<_>> = intent.goals.iter().map(|goal| catalog.services.iter().filter(|service|
        service.contract.capability_slug == goal.capability
            && Some(service.api_slug.as_str()) == goal.constraints.get("source").and_then(Value::as_str)
            && endpoint_matches(goal.constraints.get("endpoint_id"), Some(&service.endpoint_id.to_string())))
        .collect()).collect();
    let mut useful = BTreeSet::new();
    for &capability_index in &retrievals {
        let selected = &owners[capability_index];
        ensure!(!selected.is_empty(), "every parsed subject needs its requested retrieval");
        let cap = &parsed.capabilities[capability_index];
        if selected.len() == 1 {
            let goal = selected[0];
            // Sharing a purchase requires the same declared-field proof as
            // quoting those fields in one requirement. Optional row fields
            // remain supported data, even when a particular row lacks them.
            if owners.iter().filter(|owners| owners.contains(&goal)).count() > 1 {
                ensure!(!cap.outputs.is_empty() && !services[goal].is_empty() && services[goal].iter().all(|service| cap.outputs.iter().all(|output|
                    output_metadata(output) || output_covered(output,service,catalog,false)
                        || super::source_grounding::covered(bindings,capability_index,output,&[service],catalog))),
                    "every parsed subject needs its requested retrieval");
            }
            useful.insert(goal);
        } else {
            let outputs: Vec<_> = cap.outputs.iter().filter(|output| !output_metadata(output)).collect();
            ensure!(!outputs.is_empty(), "every parsed subject needs its requested retrieval");
            for output in outputs {
                let proving: Vec<_> = selected.iter().copied().filter(|goal| !services[*goal].is_empty()
                    && services[*goal].iter().all(|service| output_covered(output, service, catalog, false)
                        || super::source_grounding::covered(bindings,capability_index,output,&[service],catalog))).collect();
                if proving.is_empty() {
                    let union: Vec<_> = selected.iter().flat_map(|goal| services[*goal].iter().copied()).collect();
                    ensure!(super::source_grounding::covered(bindings,capability_index,output,&union,catalog), "every parsed subject needs its requested retrieval");
                    for goal in selected {
                        let without: Vec<_> = selected.iter().filter(|other| *other != goal).flat_map(|other|services[*other].iter().copied()).collect();
                        if !super::source_grounding::covered(bindings,capability_index,output,&without,catalog) { useful.insert(*goal); }
                    }
                }
                // An extra purchase must add requested scope, rather than
                // repeat the same data through another matched alternative.
                if proving.len() == 1 { useful.insert(proving[0]); }
            }
        }
    }
    ensure!(useful.len() == intent.goals.len(), "every goal must add requested output coverage");
    for (capability_index, goals) in owners.iter().enumerate() {
        let cap = &parsed.capabilities[capability_index];
        let Some(subject) = cap.subject_id.as_ref().and_then(|id| parsed.subjects.iter().find(|s| &s.id == id)) else {continue};
        for &goal_index in goals {
            let goal = &intent.goals[goal_index];
            let mut fields = BTreeSet::from(["company.name".to_owned(), "geo.input.q".to_owned()]);
            for service in catalog.services.iter().filter(|service| service.contract.capability_slug == goal.capability
                && endpoint_matches(goal.constraints.get("endpoint_id"), Some(&service.endpoint_id.to_string()))
                && goal.constraints.get("source").and_then(Value::as_str).is_none_or(|source| service.api_slug == source)) {
                fields.extend(service.contract.requires.iter().map(|required| required.fact_type.clone()));
            }
            let values = &mut intent.subjects[goal.subject].values;
            for field in fields {
                let Some(fact) = catalog.vocabulary.get(&field) else {continue};
                let Some(mut value) = crate::ask_v2::input_values::documented_literal(&field, &fact.value_schema, subject) else {continue};
                if catalog_core::facts::schema::validate(&fact.value_schema, &value).is_err() {
                    let Some(typed) = value.as_str().and_then(|text| serde_json::from_str::<Value>(text).ok())
                        .filter(|typed| catalog_core::facts::schema::validate(&fact.value_schema, typed).is_ok()) else {continue};
                    value = typed;
                }
                if let Some(existing) = values.get(&field) {
                    ensure!(existing == &value, "input belongs to a different parsed subject");
                } else { values.insert(field, value); }
            }
        }
    }
    for cap in parsed
        .capabilities
        .iter()
        .filter(|cap| answer_instruction(cap))
    {
        let instruction = format!("{}: {}", cap.name, cap.description);
        if !intent.answer_operations.contains(&instruction) {
            intent.answer_operations.push(instruction);
        }
        if cap.slug == "general.capabilities" {
            intent.answer_operations.push("Describe Apiosk availability only from confirmed catalog and readiness evidence. Mark unavailable evidence as unverified; never infer shipment tracking or live traffic from an unrelated successful retrieval.".into());
        }
    }
    Ok(())
}

pub(super) fn endpoint_matches(constraint: Option<&Value>, endpoint: Option<&str>) -> bool {
    constraint.is_none_or(|constraint| match constraint {
        Value::String(id) => Some(id.as_str()) == endpoint,
        Value::Array(ids) => ids
            .iter()
            .any(|id| id.as_str() == endpoint && endpoint.is_some()),
        _ => false,
    })
}
fn candidate_matches(goal: &super::Goal, candidate: &crate::capability_intake::Candidate) -> bool {
    candidate.availability == "supported"
        && candidate.endpoint_id.is_some()
        && candidate.capability.as_deref() == Some(&goal.capability)
        && goal
            .constraints
            .get("source")
            .and_then(Value::as_str)
            .is_none_or(|source| candidate.source_slug.as_deref() == Some(source))
        && endpoint_matches(
            goal.constraints.get("endpoint_id"),
            candidate.endpoint_id.as_deref(),
        )
}

/// Answer presentation is not another provider purchase. Keep this narrow:
/// subject-specific analysis and comparisons can require external data.
pub(super) fn answer_instruction(cap: &crate::capability_intake::Capability) -> bool {
    crate::capability_intake::answer_instruction(cap)
}

pub(super) fn retrievals_matched(intake: &Intake) -> bool {
    intake
        .parsed_request
        .capabilities
        .iter()
        .enumerate()
        .all(|(index, cap)| {
            answer_instruction(cap)
                || request_match(intake, index)
                    .is_some_and(executable_match)
        })
}

pub(super) fn uncovered_reply(intake: &Intake) -> Option<super::ChatReply> {
    let missing: Vec<_> = intake
        .parsed_request
        .capabilities
        .iter()
        .enumerate()
        .filter(|(index, cap)| {
            !answer_instruction(cap)
                && !cap.slug.starts_with("general.")
                && request_match(intake, *index).is_none_or(|matched| !executable_match(matched))
        })
        .map(|(index, cap)| (index, cap.name.as_str()))
        .collect();
    if missing.is_empty() {
        return None;
    }
    let names = missing.iter().map(|(_, name)| *name).collect::<Vec<_>>().join("; ");
    let discovered = missing.iter().filter_map(|(index, _)| request_match(intake, *index)).collect::<Vec<_>>();
    let message = if let Some(message) = discovery_message(&intake.parsed_request.language, &names, &discovered) {
        message
    } else if intake.parsed_request.language == "nl" {
        format!("Voor deze onderdelen is geen passende uitvoerbare bron gevonden: {names}. De volledige aanvraag krijgt daarom nog geen prijs. Verduidelijk deze onderdelen of pas de vraag aan; de overige onderdelen blijven bewaard.")
    } else {
        format!("No matching executable source was found for these requirements: {names}. The complete request cannot be priced yet. Clarify these requirements or revise the question; the other requirements are kept.")
    };
    // This is a catalog fact, not a model draft to rewrite into broader claims
    // about a provider's existence. It never quotes a reduced request.
    Some(super::ChatReply {
        output_bindings: vec![],
        message,
        discover: false,
        intent: None,
        decline_reason: None,
    })
}

fn executable_match(matched: &crate::capability_intake::Match) -> bool {
    matched.apiosk.iter().any(|candidate| candidate.availability == "supported"
        && candidate.endpoint_id.is_some()
        && candidate.capability.as_ref().is_some_and(|capability| matched.operations.contains(capability)))
}

fn discovery_message(language: &str, names: &str, matches: &[&crate::capability_intake::Match]) -> Option<String> {
    if matches.iter().any(|found| !found.coinbase.is_empty()) {
        Some(if language == "nl" {
            format!("Er zijn aanvullende diensten gevonden voor {names}. Een volledig automatisch plan is nog niet gereed. Kies een passende dienst uit de gevonden bronnen om de invoer en prijs te controleren.")
        } else {
            format!("Additional services were found for {names}. A complete automatic plan is not ready yet. Choose a matching service from the sources to review its inputs and price.")
        })
    } else if matches.iter().any(|found| found.apiosk.iter().any(|candidate| candidate.availability != "supported" && candidate.endpoint_id.is_some())) {
        Some(if language == "nl" {
            format!("Er zijn bronendpoints gevonden voor {names}. Een volledig automatisch plan is nog niet gereed; de dekking en uitvoerbaarheid moeten nog worden gecontroleerd. Bekijk de gevonden bronnen.")
        } else {
            format!("Source endpoints were found for {names}. A complete automatic plan is not ready yet; coverage and execution support still need verification. Review the matching sources below.")
        })
    } else { None }
}

/// A complete model reply was rejected for changing or duplicating the
/// requirements. This is neither evidence of an outage nor of absent sources.
pub(super) fn rejected_reply(intake: &Intake) -> super::ChatReply {
    let names = intake.parsed_request.capabilities.iter()
        .filter(|cap| !answer_instruction(cap)).map(|cap| cap.name.as_str())
        .collect::<Vec<_>>().join("; ");
    let message = if let Some(message) = discovery_message(&intake.parsed_request.language, &names, &intake.matches.iter().collect::<Vec<_>>()) {
        message
    } else if intake.parsed_request.language == "nl" {
        "Ik kon nog geen volledig plan maken. Probeer opnieuw. Er is niets uitgevoerd of afgeschreven.".into()
    } else {
        "I couldn’t build a complete plan yet. Try again. Nothing was run or charged.".into()
    };
    super::ChatReply {output_bindings: vec![], message, discover:false, intent:None, decline_reason:None}
}

/// Recover one concrete retrieval from the already displayed, ranked choices.
/// It may cover several field requirements for the same explicit subject,
/// but only when the selected contract proves every requirement in full.
/// Model mistakes never supply values or scope to this path. Every requested
/// field, literal binding and exact resolved dependency chain must still pass.
pub(super) fn single_retrieval_reply(intake:&Intake,catalog:&Catalog,question:&str,literals:&str,task:&crate::state::Task)->Option<super::ChatReply> {
    use sha2::Digest;
    if intake.parsed_request.meta.text_sha256 != hex::encode(sha2::Sha256::digest(question.as_bytes())) { return None; }
    let retrievals:Vec<_>=intake.parsed_request.capabilities.iter().enumerate().filter(|(_,cap)|!answer_instruction(cap)).collect();
    let &(index,cap)=retrievals.first()?;
    if retrievals.len()>1 && cap.subject_id.is_none() {return None;}
    if retrievals.iter().any(|(_,other)| other.subject_id!=cap.subject_id
        || other.outputs.is_empty()
        || other.outputs.iter().all(|output|output_metadata(output))) {return None;}
    if intake.parsed_request.subjects.iter().any(|subject| cap.subject_id.as_deref()!=Some(subject.id.as_str())) {return None;}
    let label=match cap.subject_id.as_ref() {
        Some(id)=>intake.parsed_request.subjects.iter().find(|subject|&subject.id==id)?.label.clone(),
        None if intake.parsed_request.subjects.is_empty()=>question.to_owned(),
        None=>return None,
    };
    for candidate in &request_match(intake,index)?.apiosk {
        let Some(capability)=candidate.capability.as_ref() else {continue};
        let mut constraints=std::collections::BTreeMap::new();
        if let Some(source)=&candidate.source_slug {constraints.insert("source".into(),json!(source));}
        if let Some(endpoint)=&candidate.endpoint_id {constraints.insert("endpoint_id".into(),json!(endpoint));}
        let goal=super::Goal {capability:capability.clone(),subject:0,constraints};
        if !candidate_matches(&goal,candidate) || !request_match(intake,index)?.operations.contains(capability) {continue;}
        let mut answer_operations=intake.parsed_request.deliverable.operations.clone();
        answer_operations.push(format!("Requested answer format: {}",intake.parsed_request.deliverable.format));
        let mut intent=Intent {subjects:vec![super::Subject {entity_ref:None,label:label.clone(),values:Default::default()}],
            goals:vec![goal],answer_operations,unresolved_references:vec![]};
        super::capability_binding::bind_source_requirements(&mut intent,intake,catalog);
        if intake.parsed_request.subjects.is_empty() {
            super::capability_binding::bind_ecb_target_literal(&mut intent,question,catalog);
        }
        if prepare(&mut intent,intake,catalog).is_err() || intent.bind_constraint_inputs(literals,catalog).is_err()
            || intent.validate(literals,task,catalog).is_err() {continue;}
        let mut preview_task=task.clone();preview_task.capability_intake=Some(intake.clone());
        let mut preview_intent=intent.clone();
        if crate::engine::bind_subjects(&mut preview_task,catalog,&mut preview_intent).is_err() {continue;}
        let crate::resolver::Resolution::Plan(plan)=crate::resolver::resolve(catalog,&preview_task,&preview_intent) else {continue};
        if !uncovered_outputs(&preview_task,catalog,&preview_intent,&plan).is_empty() {continue;}
        if !retrieval_inputs_preserved(intake,catalog,&preview_task,&preview_intent,&plan,question) {continue;}
        let message=if intake.parsed_request.language=="nl" {"Ik controleer de gevonden bron en toon de totale prijs voordat er iets wordt gekocht."}
            else {"I will check the matched source and show the total price before anything is bought."};
        return Some(super::ChatReply { output_bindings: vec![],message:message.into(),discover:true,intent:Some(intent),decline_reason:Some("none".into())});
    }
    None
}

/// Compile independent, already interpreted requirements with the same
/// contract proof as a single retrieval. Subject ids and all literal criteria
/// survive the merge; the complete union is validated again before use.
#[cfg(test)]
pub(super) fn multiple_retrieval_reply(intake:&Intake,catalog:&Catalog,question:&str,literals:&str,task:&crate::state::Task)->Option<super::ChatReply> {
    let parsed=&intake.parsed_request;
    let retrievals:Vec<_>=parsed.capabilities.iter().enumerate().filter(|(_,cap)|!answer_instruction(cap)).collect();
    if retrievals.len()<2 || retrievals.len()>super::MAX_GOALS
        || parsed.subjects.iter().map(|subject|&subject.id).collect::<BTreeSet<_>>().len()!=parsed.subjects.len() {return None;}
    let mut subjects=std::collections::BTreeMap::new();
    let mut intent=Intent {subjects:vec![],goals:vec![],answer_operations:vec![],unresolved_references:vec![]};
    for (index,cap) in retrievals {
        let id=cap.subject_id.as_ref()?;
        let subject=parsed.subjects.iter().find(|subject|&subject.id==id)?;
        let mut part=intake.clone();
        part.parsed_request.subjects=vec![subject.clone()];part.parsed_request.capabilities=vec![cap.clone()];
        part.matches=vec![request_match(intake,index)?.clone()];
        part.selected_capabilities=part.matches[0].operations.clone();
        let compiled=single_retrieval_reply(&part,catalog,question,literals,task)?.intent?;
        if compiled.subjects.len()!=1 || !compiled.unresolved_references.is_empty() {return None;}
        let incoming=&compiled.subjects[0];
        let target=if let Some(&target)=subjects.get(id) {
            let existing:&mut super::Subject=&mut intent.subjects[target];
            if existing.label!=incoming.label || existing.entity_ref!=incoming.entity_ref {return None;}
            for (field,value) in &incoming.values {
                if existing.values.get(field).is_some_and(|existing|existing!=value) {return None;}
                existing.values.insert(field.clone(),value.clone());
            }
            target
        } else {
            let target=intent.subjects.len();subjects.insert(id.clone(),target);intent.subjects.push(incoming.clone());target
        };
        for mut goal in compiled.goals {
            if goal.subject!=0 {return None;}
            goal.subject=target;
            if !intent.goals.iter().any(|existing|existing.subject==goal.subject && existing.capability==goal.capability && existing.constraints==goal.constraints) {
                intent.goals.push(goal);
            }
        }
        for operation in compiled.answer_operations {
            if !intent.answer_operations.contains(&operation) {intent.answer_operations.push(operation);}
        }
    }
    // An unassigned subject is still supplied scope, never disposable context.
    if subjects.len()!=parsed.subjects.len() {return None;}
    if prepare(&mut intent,intake,catalog).is_err() || intent.bind_constraint_inputs(literals,catalog).is_err()
        || intent.validate(literals,task,catalog).is_err() {return None;}
    let mut preview_task=task.clone();preview_task.capability_intake=Some(intake.clone());
    preview_task.record_turn(question,None);
    let mut preview_intent=intent.clone();
    if crate::engine::bind_subjects(&mut preview_task,catalog,&mut preview_intent).is_err() {return None;}
    let crate::resolver::Resolution::Plan(plan)=crate::resolver::resolve(catalog,&preview_task,&preview_intent) else {return None;};
    if !uncovered_outputs(&preview_task,catalog,&preview_intent,&plan).is_empty()
        || !uncovered_inputs(&preview_task,catalog,&preview_intent,&plan).is_empty() {return None;}
    let message=if parsed.language=="nl" {"Ik controleer de gevonden bronnen en toon de totale prijs voordat er iets wordt gekocht."}
        else {"I will check the matched sources and show the total price before anything is bought."};
    Some(super::ChatReply { output_bindings: vec![],message:message.into(),discover:true,intent:Some(intent),decline_reason:Some("none".into())})
}

/// Output coverage alone cannot prove a filtered retrieval. Every supplied
/// identifier and requested input must reach an actual selected input binding,
/// including the dependency calls which resolve an identifier from a name.
/// Uncertain mappings stay with the normal planner; this never infers a value.
fn retrieval_inputs_preserved(intake:&Intake,catalog:&Catalog,task:&crate::state::Task,intent:&Intent,plan:&crate::resolver::Plan,question:&str)->bool {
    input_bindings_preserved(intake,catalog,task,intent,&plan.steps,question,InputProof { every_identifier:true, model_bindings:false, supplied_only:false })
}

/// A reviewed output contract may document scope fixed by one exact endpoint.
/// This is catalog evidence, not an inference from a provider name, description
/// or question. A changed route or set of inputs must be reviewed again.
fn fixed_scope_covered(key:&str,value:&str,service:&crate::catalog::Service,catalog:&Catalog)->bool {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Annotation { apiosk_fixed_scope_v1:Vec<Scope> }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Scope {
        source_slug:String, endpoint_id:uuid::Uuid, method:String, path:String,
        input_facts:Vec<String>, inputs:std::collections::BTreeMap<String,Vec<String>>,
    }
    let normalized=|text:&str|text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    service.contract.output_map.produces.keys().any(|field| {
        let Some(comment)=catalog.vocabulary.get(field).and_then(|fact|fact.value_schema.get("$comment"))
            .and_then(Value::as_str).filter(|comment|comment.len()<=16_384) else {return false};
        let Ok(annotation)=serde_json::from_str::<Annotation>(comment) else {return false};
        if annotation.apiosk_fixed_scope_v1.is_empty() || annotation.apiosk_fixed_scope_v1.len()>16 {return false;}
        annotation.apiosk_fixed_scope_v1.iter().any(|scope| {
            if scope.source_slug!=service.api_slug || scope.endpoint_id!=service.endpoint_id
                || scope.method!=service.method || scope.path!=service.path || scope.input_facts.len()>32
                || scope.inputs.is_empty() || scope.inputs.len()>32 {return false;}
            let declared:BTreeSet<_>=scope.input_facts.iter().collect();
            if declared.len()!=scope.input_facts.len() || declared!=service.contract.input_map.keys().collect() {return false;}
            let Some(aliases)=scope.inputs.get(key).filter(|aliases|!aliases.is_empty() && aliases.len()<=16) else {return false};
            if aliases.iter().any(|alias|alias.trim().is_empty() || alias.len()>256) {return false;}
            let value=normalized(value);
            !value.is_empty() && aliases.iter().any(|alias|normalized(alias)==value)
        })
    })
}

#[derive(Clone, Copy)]
struct InputProof {
    every_identifier: bool,
    model_bindings: bool,
    supplied_only: bool,
}
impl InputProof {
    fn complete(every_identifier: bool) -> Self {
        Self { every_identifier, model_bindings:true, supplied_only:false }
    }
}

fn input_bindings_preserved(intake:&Intake,catalog:&Catalog,task:&crate::state::Task,intent:&Intent,steps:&[crate::resolver::Step],question:&str,proof:InputProof)->bool {
    let parsed=&intake.parsed_request;
    let Some(subject)=parsed.subjects.first() else {
        return parsed.capabilities.iter().all(|cap| answer_instruction(cap) || (cap.inputs.is_empty() && cap.missing.is_empty()));
    };
    let Some(entity)=intent.subjects.first().and_then(|subject|subject.entity_ref) else {return false};
    let mut bound=Vec::new();
    for step in steps.iter().filter(|step|step.entity_ref==entity) {
        for field in step.service.contract.input_map.keys() {
            let Some(fact)=catalog.vocabulary.get(field) else {return false};
            if let Ok(Some(value))=crate::catalog::fact_value(&task.facts,entity,field) {
                bound.push((field.as_str(),&fact.value_schema,value));
            }
        }
    }
    let mapped=|field:&str,schema:&Value,subject:&crate::capability_intake::Subject| {
        let value=crate::ask_v2::input_values::documented_literal(field,schema,subject)?;
        if catalog_core::facts::schema::validate(schema,&value).is_ok() {Some(value)}
        else {value.as_str().and_then(|text|serde_json::from_str::<Value>(text).ok())
            .filter(|typed|catalog_core::facts::schema::validate(schema,typed).is_ok())}
    };
    let normal=|name:&str|name.split(|ch:char|!ch.is_alphanumeric()).filter(|part|!part.is_empty())
        .map(str::to_lowercase).collect::<Vec<_>>().join("_");
    let name_alias=|name:&str| {
        let name=normal(name);
        name=="name" || name=="query" || name==normal(&subject.kind) || name==format!("{}_name",normal(&subject.kind))
            || (matches!(subject.kind.as_str(),"company"|"organization"|"organisation"|"business")
                && matches!(name.as_str(),"company_name"|"business_name"|"organization_name"|"organisation_name"))
    };
    let mut label_only=subject.clone();label_only.identifiers.clear();
    let mut without_label=label_only.clone();without_label.label.clear();
    let label_used=bound.iter().any(|(field,schema,value)|mapped(field,schema,&label_only).as_ref()==Some(value)
        && mapped(field,schema,&without_label).as_ref()!=Some(value));
    let identifying_input=|field:&str|steps.iter().any(|step|step.entity_ref==entity
        && step.service.contract.identifies.as_deref()==Some(field) && step.service.contract.input_map.contains_key(field));
    let identity_bound=bound.iter().any(|(field,schema,value)|identifying_input(field)
        && (mapped(field,schema,subject).as_ref()==Some(value)
            || (proof.model_bindings && intent.subjects[0].values.get(*field)==Some(value)
                && catalog_core::facts::schema::validate(schema,value).is_ok()
                && subject.identifiers.iter().any(|(key,literal)|!name_alias(key) && literal!=&subject.label
                    && (value.as_str()==Some(literal.as_str()) || serde_json::from_str::<Value>(literal).ok().as_ref()==Some(value))))));
    let source_bound=|hint:&str|steps.iter().any(|step|step.entity_ref==entity
        && intent.goals.iter().any(|goal|goal.capability==step.service.contract.capability_slug)
        && crate::capability_intake::names_source(&step.service,hint));
    let whole_query=subject.identifiers.get("query").filter(|query|query.as_str()==question && !query.is_empty())
        .filter(|query|bound.iter().any(|(field,schema,value)|matches!(field.rsplit('.').next(),Some("query"|"q"|"search"|"source_search_query"))
            && value.as_str()==Some(query.as_str()) && mapped(field,schema,subject).as_ref()==Some(value)));
    let mut identifiers=BTreeSet::new();
    for (key,value) in &subject.identifiers {
        // Some intake documents retain the original sentence as context.
        // It is not a filter unless the capability explicitly asks to use it.
        if ((key=="request" && !value.is_empty() && question.contains(value))
            || (key=="query" && value==question))
            && !parsed.capabilities.iter().filter(|cap|!answer_instruction(cap)).any(|cap|
                cap.inputs.iter().chain(&cap.missing).any(|input|normal(input)==*key || normal(input).ends_with(&format!("_{key}")))) {
            continue;
        }
        if matches!(normal(key).as_str(),"source"|"source_hint"|"provider") {
            if parsed.capabilities.iter().any(|cap|cap.source_hint.as_ref().is_some_and(|hint|hint.eq_ignore_ascii_case(value) && source_bound(hint))) {
                identifiers.insert(normal(key));continue;
            }
            if proof.every_identifier {return false;}
            continue;
        }
        if whole_query.is_some_and(|query| {
            let query=query.to_lowercase();let value=value.to_lowercase();
            !value.is_empty() && query.match_indices(&value).any(|(start,matched)|
                query[..start].chars().next_back().is_none_or(|ch|!ch.is_alphanumeric())
                && query[start+matched.len()..].chars().next().is_none_or(|ch|!ch.is_alphanumeric()))
        }) {
            identifiers.insert(normal(key));continue;
        }
        if name_alias(key) && value==&subject.label && label_used {
            identifiers.insert(normal(key));continue;
        }
        if bound.iter().any(|(field,_,bound_value)|crate::ask_v2::input_values::address_component_preserved(field,subject,key,value,bound_value)) {
            identifiers.insert(normal(key));continue;
        }
        let identity_name=name_alias(key) && !matches!(normal(key).as_str(),"query"|"topic"|"keyword");
        if identity_name && value==&subject.label && (identity_bound
            || (proof.supplied_only && intent.subjects[0].label == subject.label)) {
            identifiers.insert(normal(key));continue;
        }
        // A model can bind a literal to a documented input even when its
        // semantic alias is not one of the deterministic convenience mappings.
        // Require the exact validated value on this subject's selected input.
        if proof.model_bindings && bound.iter().any(|(field,schema,bound_value)|
            intent.subjects[0].values.get(*field)==Some(bound_value)
            && catalog_core::facts::schema::validate(schema,bound_value).is_ok()
            && (!name_alias(key) || value==&subject.label || !identifying_input(field))
            && (name_alias(key) || mapped(field,schema,&label_only).as_ref()!=Some(bound_value))
            && (bound_value.as_str()==Some(value.as_str()) || serde_json::from_str::<Value>(value).ok().as_ref()==Some(bound_value))) {
            identifiers.insert(normal(key));continue;
        }
        // A canonical field identifier can repeat the label. Its exact field
        // and value are still consumed even when label fallback is identical.
        if bound.iter().any(|(field,schema,bound_value)|(normal(field)==normal(key)
            || crate::ask_v2::input_values::reviewed_input_alias(schema,key))
            && mapped(field,schema,subject).as_ref()==Some(bound_value)
            && (bound_value.as_str()==Some(value.as_str())
                || serde_json::from_str::<Value>(value).ok().as_ref()==Some(bound_value))) {
            identifiers.insert(normal(key));continue;
        }
        if matches!(normal(key).as_str(), "country" | "country_code") {
            let name=value.trim().to_lowercase();
            let name=name.strip_prefix("the ").unwrap_or(&name).replace([' ', '-'], "");
            let countries=crate::ask_v2::search::named_countries(&name);
            let selected:Vec<_>=steps.iter().filter(|step|step.entity_ref==entity
                && intent.goals.iter().any(|goal|goal.capability==step.service.contract.capability_slug)).collect();
            if countries.len()==1 && !selected.is_empty() && selected.iter().all(|step| {
                let known=catalog.known_sources.iter().find(|source|source.slug==step.service.api_slug);
                crate::ask_v2::search::fixed_endpoint_country(&step.service,known).as_deref()==countries.first().copied()
            }) {identifiers.insert(normal(key));continue;}
        }
        if steps.iter().any(|step|step.entity_ref==entity
            && intent.goals.iter().any(|goal|goal.capability==step.service.contract.capability_slug
                && goal.constraints.get("source").and_then(Value::as_str).is_none_or(|source|source==step.service.api_slug)
                && endpoint_matches(goal.constraints.get("endpoint_id"),Some(&step.service.endpoint_id.to_string())))
            && fixed_scope_covered(key,value,&step.service,catalog)) {
            identifiers.insert(normal(key));continue;
        }
        let mut without=subject.clone();without.identifiers.remove(key);
        if bound.iter().any(|(field,schema,bound_value)|*field=="fx.ecb.quote"
            && crate::ask_v2::input_values::ecb_quote_for_pair(value).as_ref()==Some(bound_value)
            && mapped(field,schema,subject).as_ref()==Some(bound_value)
            && steps.iter().any(|step|step.entity_ref==entity && step.service.contract.capability_slug=="fx.ecb-reference-rate")) {
            identifiers.insert(normal(key));continue;
        }
        // This already-reviewed conversion validates the complete public URL,
        // entity type and exact permalink; no query or fragment is discarded.
        // Repeating the same URL in the label does not erase that proof.
        if bound.iter().any(|(field,schema,bound_value)|
            crate::ask_v2::input_values::crunchbase_profile_id(field,value).as_ref()==Some(bound_value)
                && mapped(field,schema,subject).as_ref()==Some(bound_value)) {
            identifiers.insert(normal(key));continue;
        }
        let mut parts=Vec::new();
        for (field,schema,bound_value) in &bound {
            if mapped(field,schema,subject).as_ref()!=Some(bound_value)
                || mapped(field,schema,&without).as_ref()==Some(bound_value) {continue;}
            let text=match bound_value {Value::String(text)=>text.clone(),Value::Number(number)=>number.to_string(),Value::Bool(value)=>value.to_string(),_=>continue};
            if !parts.contains(&text) {parts.push(text);}
        }
        // A documented split such as a VAT country prefix plus national
        // number is complete only when all parts reproduce the whole literal.
        // An assembled address may contain the whole city/postcode literal.
        fn reconstruct(value:&str,parts:&[String])->bool {
            if value.is_empty() {return false;}
            if parts.iter().any(|part|part==value || part.contains(value)) {return true;}
            fn consume(rest:&str,parts:&[String],used:u16)->bool {
                if rest.is_empty() {return true;}
                parts.iter().enumerate().any(|(index,part)|used&(1<<index)==0 && !part.is_empty()
                    && rest.strip_prefix(part).is_some_and(|tail|consume(tail,parts,used|(1<<index))))
            }
            parts.len()<=8 && consume(value,parts,0)
        }
        if !reconstruct(value,&parts) {
            if proof.every_identifier {return false;}
            continue;
        }
        identifiers.insert(normal(key));
    }
    if proof.supplied_only { return true; }
    parsed.capabilities.iter().filter(|cap|!answer_instruction(cap)).all(|cap|cap.inputs.iter().chain(&cap.missing).all(|input| {
        let input=normal(input);
        identifiers.contains(&input) || (name_alias(&input) && (label_used
            || (identity_bound && !matches!(input.as_str(),"query"|"topic"|"keyword"))))
            || (cap.missing.iter().any(|missing|normal(missing)==input) && {
                // An unsupplied provider identifier may be resolved by an
                // actual dependency. Supplied filters still need literal
                // preservation above; a hypothetical lookup is insufficient.
                let mut probe=subject.clone();probe.label.clear();probe.identifiers.clear();
                probe.identifiers.insert(input.clone(),"dependency-proof".into());
                steps.iter().enumerate().any(|(position,step)|step.entity_ref==entity && step.service.contract.input_map.keys().any(|field|
                    catalog.vocabulary.get(field).is_some_and(|fact|crate::ask_v2::input_values::documented_literal(field,&fact.value_schema,&probe)==Some(json!("dependency-proof")))
                    && steps[..position].iter().any(|prior|prior.entity_ref==entity && prior.service.contract.output_map.produces.contains_key(field))))
            })
            || (matches!(input.as_str(),"source"|"source_hint"|"provider") && cap.source_hint.as_ref().is_some_and(|hint|source_bound(hint)))
            || bound.iter().any(|(field,schema,value)| (normal(field)==input || field.rsplit('.').next().is_some_and(|tail|normal(tail)==input))
                && mapped(field,schema,subject).as_ref()==Some(value))
    }))
}

/// A missing API input may postpone a quote, but must never discard a supplied
/// identifier or filter while constructing that draft. No dependencies are added.
pub(super) fn supplied_inputs_preserved(intake:&Intake, catalog:&Catalog, task:&crate::state::Task, intent:&Intent, question:&str) -> bool {
    intake.parsed_request.subjects.iter().all(|subject| {
        let Some((index, bound)) = intent.subjects.iter().enumerate().find(|(_, item)| item.label == subject.label) else { return false; };
        let Some(entity_ref) = bound.entity_ref else { return false; };
        let mut scoped = intake.clone();
        scoped.parsed_request.subjects = vec![subject.clone()];
        scoped.parsed_request.capabilities.retain(|cap| cap.subject_id.as_deref() == Some(subject.id.as_str()));
        let mut selected = intent.clone();
        selected.subjects = vec![bound.clone()];
        selected.goals.retain(|goal| goal.subject == index);
        for goal in &mut selected.goals { goal.subject = 0; }
        let steps: Vec<_> = selected.goals.iter().enumerate().flat_map(|(goal_index, goal)| {
            catalog.services.iter().filter(move |service| service.contract.capability_slug == goal.capability
                && goal.constraints.get("source").and_then(Value::as_str).is_none_or(|source| service.api_slug == source)
                && endpoint_matches(goal.constraints.get("endpoint_id"), Some(&service.endpoint_id.to_string())))
                .map(move |service| crate::resolver::Step { service:service.clone(), entity_ref, goal_index, browser_fallback:None })
        }).collect();
        input_bindings_preserved(&scoped,catalog,task,&selected,&steps,question,
            InputProof { every_identifier:true, model_bindings:true, supplied_only:true })
    })
}

/// The model and deterministic planner share the same pre-quote input proof.
/// Scope can only come from the selected subject's matched goal/dependency
/// chain, never an unused service or another subject with a similar label.
pub(crate) fn uncovered_inputs(task:&crate::state::Task,catalog:&Catalog,intent:&Intent,plan:&crate::resolver::Plan)->Vec<String> {
    let Some(intake)=task.capability_intake.as_ref() else {return vec![]};
    let question=task.conversation.last().map(|turn|turn.question.as_str()).unwrap_or_default();
    let mut missing=Vec::new();
    for subject in &intake.parsed_request.subjects {
        let capabilities:Vec<_>=intake.parsed_request.capabilities.iter().enumerate()
            .filter(|(_,cap)|!answer_instruction(cap) && cap.subject_id.as_deref()==Some(subject.id.as_str())).collect();
        if capabilities.is_empty() {
            // A null or lost capability subject reference cannot erase the
            // supplied subject's identity/criteria from the purchase proof.
            let referenced=intake.parsed_request.capabilities.iter().any(|cap|cap.subject_id.as_deref()==Some(subject.id.as_str()));
            if !referenced && (!subject.label.trim().is_empty() || !subject.identifiers.is_empty())
                && intake.parsed_request.capabilities.iter().any(|cap|!answer_instruction(cap)) {
                missing.push(format!("{}: unassigned retrieval subject",subject.label));
            }
            continue;
        }
        let preserved=intent.subjects.iter().enumerate().filter(|(_,candidate)|candidate.label.eq_ignore_ascii_case(&subject.label)
            && candidate.entity_ref.is_some()).any(|(subject_index,candidate)| {
            let mut scoped=intake.clone();scoped.parsed_request.subjects=vec![subject.clone()];
            scoped.parsed_request.capabilities=capabilities.iter().map(|(_,cap)|(*cap).clone()).collect();
            let mut selected=Intent {subjects:vec![candidate.clone()],goals:vec![],answer_operations:vec![],unresolved_references:vec![]};
            let mut chains=Vec::new();let mut union:BTreeSet<usize>=BTreeSet::new();
            for (index,_) in &capabilities {
                let Some(found)=request_match(intake,*index) else {return false};
                let mut included=BTreeSet::new();let mut goals=Vec::new();
                for goal in intent.goals.iter().filter(|goal|goal.subject==subject_index
                    && found.operations.contains(&goal.capability) && found.apiosk.iter().any(|source|candidate_matches(goal,source))) {
                    let source=goal.constraints.get("source").and_then(Value::as_str);
                    for (root,_) in plan.steps.iter().enumerate().filter(|(_,step)|Some(step.entity_ref)==candidate.entity_ref
                        && step.service.contract.capability_slug==goal.capability
                        && source.is_none_or(|source|step.service.api_slug==source)
                        && endpoint_matches(goal.constraints.get("endpoint_id"),Some(&step.service.endpoint_id.to_string()))) {
                        dependency_steps(plan,task,catalog,root,source,&mut included);
                    }
                    let mut goal=goal.clone();goal.subject=0;goals.push(goal.clone());
                    if !selected.goals.iter().any(|existing|existing.capability==goal.capability && existing.constraints==goal.constraints) {selected.goals.push(goal);}
                }
                if included.is_empty() {return false;}
                union.extend(&included);chains.push((included,goals));
            }
            let mut resolved=plan.clone();resolved.steps=union.iter().map(|index|plan.steps[*index].clone()).collect();
            if !input_bindings_preserved(&scoped,catalog,task,&selected,&resolved.steps,question,InputProof::complete(true)) {return false;}
            // Inputs explicitly required by one retrieval must reach that
            // retrieval's chain, even when other retrievals share its subject.
            for ((_,cap),(included,goals)) in capabilities.iter().zip(chains) {
                let mut requirement=scoped.clone();requirement.parsed_request.capabilities=vec![(*cap).clone()];
                let mut chain=plan.clone();chain.steps=included.iter().map(|index|plan.steps[*index].clone()).collect();
                let mut selected=selected.clone();selected.goals=goals;
                if !input_bindings_preserved(&requirement,catalog,task,&selected,&chain.steps,question,InputProof::complete(false)) {return false;}
            }
            true
        });
        if !preserved {missing.push(format!("{}: supplied identifiers and filters",subject.label));}
    }
    for cap in intake.parsed_request.capabilities.iter().filter(|cap|!answer_instruction(cap) && cap.subject_id.is_none()) {
        if !cap.inputs.is_empty() || !cap.missing.is_empty() {missing.push(format!("{}: input scope",cap.name));}
    }
    missing
}

/// Historical contract fixtures exercise their explicit expected intake and
/// resolver directly. This dispatch is never available in production intake.
#[cfg(test)]
pub(crate) fn offline_fixture_reply(intake:&Intake,catalog:&Catalog,question:&str,task:&crate::state::Task)->Option<super::ChatReply> {
    compound_reply(intake,catalog,question,task)
        .or_else(||cbs_comparison_reply(intake,catalog,question,task))
        .or_else(||crate::tenderned::executable_intake(question,intake)
            .and_then(|intake|single_retrieval_reply(&intake,catalog,question,question,task)))
        .or_else(||broad_information_reply(intake,catalog,question,task))
        .or_else(||single_retrieval_reply(intake,catalog,question,question,task))
}

/// Combine complete literal clauses using the same discovery, input and output
/// checks as individual requests. A failed clause never becomes a partial quote.
#[cfg(test)]
pub(super) fn compound_reply(intake: &Intake, catalog: &Catalog, question: &str, task: &crate::state::Task) -> Option<super::ChatReply> {
    let request = crate::compound_request::recognize(question, "literal-compound")?;
    let same = |expected: &crate::capability_intake::ParsedRequest, actual: &crate::capability_intake::ParsedRequest| {
        expected.language == actual.language && serde_json::to_value(&expected.subjects).ok() == serde_json::to_value(&actual.subjects).ok()
            && serde_json::to_value(&expected.capabilities).ok() == serde_json::to_value(&actual.capabilities).ok()
            && serde_json::to_value(&expected.deliverable).ok() == serde_json::to_value(&actual.deliverable).ok()
    };
    if !same(&request.parsed, &intake.parsed_request) { return None; }
    let attempt = || -> Option<Intent> {
        let mut intent = Intent { subjects: vec![], goals: vec![], answer_operations: vec![
            "Combine all requested source findings into one cited answer. Connect supported findings, preserve each source's identity, dates, units and scope, and state unknowns. Co-occurrence is not evidence of ownership, a verified identity match or causation.".into()], unresolved_references: vec![] };
        let mut offset = 0;
        for clause in &request.clauses {
            let matches = (offset..offset + clause.parsed.capabilities.len())
                .map(|index| request_match(intake, index).cloned()).collect::<Option<Vec<_>>>()?;
            offset += clause.parsed.capabilities.len();
            let part = Intake { parsed_request: clause.parsed.clone(),
                selected_capabilities: matches.iter().flat_map(|found| found.operations.iter().cloned()).collect(),
                matches, apiosk_sources_searched: intake.apiosk_sources_searched, catalog_version: intake.catalog_version.clone() };
            let reply = cbs_comparison_reply(&part, catalog, &clause.question, task)
                .or_else(|| crate::tenderned::executable_intake(&clause.question, &part)
                    .and_then(|part| single_retrieval_reply(&part, catalog, &clause.question, &clause.question, task)))
                .or_else(|| broad_information_reply(&part, catalog, &clause.question, task))
                .or_else(|| single_retrieval_reply(&part, catalog, &clause.question, &clause.question, task))?;
            let part_intent = reply.intent.filter(|_| reply.discover)?;
            let mut subjects = Vec::new();
            for subject in part_intent.subjects {
                let index = if let Some(index) = intent.subjects.iter().position(|s| s.label == subject.label && s.entity_ref == subject.entity_ref) {
                    let values = &mut intent.subjects[index].values;
                    for (key, value) in subject.values {
                        if values.get(&key).is_some_and(|existing| existing != &value) { return None; }
                        values.insert(key, value);
                    }
                    index
                } else { let index = intent.subjects.len(); intent.subjects.push(subject); index };
                subjects.push(index);
            }
            for mut goal in part_intent.goals {
                goal.subject = *subjects.get(goal.subject)?;
                if !intent.goals.iter().any(|g| g.subject == goal.subject && g.capability == goal.capability && g.constraints == goal.constraints) {
                    intent.goals.push(goal);
                }
            }
            for operation in part_intent.answer_operations {
                if !intent.answer_operations.contains(&operation) { intent.answer_operations.push(operation); }
            }
        }
        super::capability_binding::bind_source_requirements(&mut intent, intake, catalog);
        prepare(&mut intent, intake, catalog).ok()?;
        intent.bind_constraint_inputs(question, catalog).ok()?;
        intent.validate(question, task, catalog).ok()?;
        let mut preview = task.clone(); preview.capability_intake = Some(intake.clone());
        let mut bound = intent.clone();
        crate::engine::bind_subjects(&mut preview, catalog, &mut bound).ok()?;
        let crate::resolver::Resolution::Plan(plan) = crate::resolver::resolve(catalog, &preview, &bound) else { return None; };
        if !uncovered_outputs(&preview, catalog, &bound, &plan).is_empty() { return None; }
        Some(intent)
    };
    Some({
        let intent = attempt()?;
        super::ChatReply { output_bindings: vec![], message: if intake.parsed_request.language == "nl" {
        "Ik combineer de gevraagde brongegevens in één antwoord. Bekijk eerst de totale prijs."
    } else { "I will combine the requested source data in one answer. Review the total price first." }.into(),
        discover: true, intent: Some(intent), decline_reason: Some("none".into()) }
    })
}

/// A complete CBS comparison needs the reviewed monthly observations and all
/// four codebooks. Keep normal discovery, binding, scope and coverage checks.
#[cfg(test)]
pub(super) fn cbs_comparison_reply(intake: &Intake, catalog: &Catalog, question: &str, task: &crate::state::Task) -> Option<super::ChatReply> {
    let canonical = crate::cbs_comparison::canonical_request(question)?;
    if canonical.language != intake.parsed_request.language
        || serde_json::to_value(&canonical.subjects).ok()? != serde_json::to_value(&intake.parsed_request.subjects).ok()?
        || serde_json::to_value(&canonical.capabilities).ok()? != serde_json::to_value(&intake.parsed_request.capabilities).ok()?
        || serde_json::to_value(&canonical.deliverable).ok()? != serde_json::to_value(&intake.parsed_request.deliverable).ok()? {
        return None;
    }
    let attempt = || -> anyhow::Result<Intent> {
        let services = crate::cbs_comparison::reviewed_services(catalog)?;
        let mut intent = Intent { subjects: vec![super::Subject { entity_ref: None, label: canonical.subjects[0].label.clone(), values: Default::default() }],
            goals: services.iter().map(|service| super::Goal { subject: 0, capability: service.contract.capability_slug.clone(),
                constraints: std::collections::BTreeMap::from([("source".into(),json!(service.api_slug)),("endpoint_id".into(),json!(service.endpoint_id))]) }).collect(),
            answer_operations: vec!["Compare absolute Dutch bankruptcy counts across disjoint sectors for the latest available common month, using the source's period status, count unit and population of companies, institutions and sole proprietors. Verify the complete monthly slice, keep the national total separate, and preserve source labels and missing values.".into()],
            unresolved_references: vec![] };
        super::capability_binding::bind_source_requirements(&mut intent, intake, catalog);
        prepare(&mut intent, intake, catalog)?;
        intent.bind_constraint_inputs(question, catalog)?;
        intent.validate(question, task, catalog)?;
        let mut preview = task.clone(); preview.capability_intake = Some(intake.clone());
        let mut bound = intent.clone();
        crate::engine::bind_subjects(&mut preview, catalog, &mut bound)?;
        let crate::resolver::Resolution::Plan(plan) = crate::resolver::resolve(catalog, &preview, &bound) else { anyhow::bail!("CBS comparison sources do not resolve") };
        ensure!(plan.steps.len() == services.len() && uncovered_outputs(&preview, catalog, &bound, &plan).is_empty(), "CBS comparison scope is not fully covered");
        Ok(intent)
    };
    Some(match attempt() {
        Ok(intent) => super::ChatReply { output_bindings: vec![], message: if canonical.language == "nl" {"Ik vergelijk de absolute aantallen per sector voor de meest recente beschikbare maand, met de CBS-publicatiestatus en het landelijke totaal apart. Bekijk eerst de totale prijs."} else {"I will compare absolute counts by sector for the latest available month, preserving the CBS publication status and keeping the national total separate. Review the total price first."}.into(),
            discover:true, intent:Some(intent), decline_reason:Some("none".into()) },
        Err(_) => super::ChatReply { output_bindings: vec![], message: if canonical.language == "nl" {"De benodigde CBS-bronnen dekken deze vergelijking nog niet volledig. Er wordt geen onvolledig vergelijkingsplan geprijsd."} else {"The required CBS sources do not yet cover this comparison completely. An incomplete comparison will not be priced."}.into(),
            discover:false, intent:None, decline_reason:Some("unsupported".into()) },
    })
}

/// For a whole, literal broad-information request, the selected contracts and
/// their dependency graph contain everything needed to price it. Enumerate a
/// small set of already-discovered choices; every normal binding, scope and
/// output-coverage check still applies. This never purchases a source.
#[cfg(test)]
pub(super) fn broad_information_reply(intake: &Intake, catalog: &Catalog, question: &str, task: &crate::state::Task) -> Option<super::ChatReply> {
    let canonical = crate::company_request::executable_intake(question, intake)
        .or_else(|| crate::company_name_request::executable_intake(question, intake))
        .or_else(|| crate::profession_request::executable_intake(question, intake))
        .or_else(|| crate::capability_intake::broad_information::parse(question, "literal"))?;
    let profession = crate::profession_request::recognize(question);
    let named_company = crate::company_name_request::parse(question, "literal").is_some();
    if canonical.subjects.len() != intake.parsed_request.subjects.len()
        || canonical.subjects.iter().zip(&intake.parsed_request.subjects).any(|(expected, actual)| expected.label != actual.label || expected.kind != actual.kind)
        || canonical.capabilities.len() != intake.parsed_request.capabilities.len()
        || canonical.capabilities.iter().zip(&intake.parsed_request.capabilities).any(|(expected, actual)| expected.outputs != actual.outputs || expected.subject_id != actual.subject_id || expected.source_hint != actual.source_hint) {
        return None;
    }
    let mut choices = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, cap) in intake.parsed_request.capabilities.iter().enumerate() {
        for candidate in &request_match(intake, index)?.apiosk {
            let Some(capability) = &candidate.capability else {continue};
            let mut constraints = std::collections::BTreeMap::new();
            if let Some(source) = &candidate.source_slug { constraints.insert("source".into(), json!(source)); }
            if let Some(endpoint) = &candidate.endpoint_id { constraints.insert("endpoint_id".into(), json!(endpoint)); }
            let goal = super::Goal { capability: capability.clone(), subject: 0, constraints };
            if !candidate_matches(&goal, candidate) || !request_match(intake, index)?.operations.contains(capability) {continue;}
            // A building-unit/address lookup is a dependency unless its own
            // contract supplies requested scope. It must not become an extra
            // separately requested purchase.
            if !catalog.services.iter().any(|service| service.contract.capability_slug == *capability
                && profession.as_ref().is_none_or(|request| request.allows_operation(service))
                && candidate.source_slug.as_deref() == Some(service.api_slug.as_str())
                && candidate.endpoint_id.as_deref() == Some(service.endpoint_id.to_string().as_str())
                && cap.outputs.iter().any(|output| output_covered(output, service, catalog, false))) {continue;}
            if seen.insert((goal.capability.clone(), serde_json::to_string(&goal.constraints).ok()?)) { choices.push(goal); }
        }
    }
    if choices.is_empty() || choices.len() > 8 { return profession.map(|_| rejected_reply(intake)); }
    for size in 1..=choices.len() {
        for mask in (1usize..1usize << choices.len()).filter(|mask| mask.count_ones() as usize == size) {
            let mut intent = Intent { subjects: vec![super::Subject { entity_ref: None, label: canonical.subjects[0].label.clone(), values: Default::default() }],
                goals: choices.iter().enumerate().filter(|(index, _)| mask & (1 << index) != 0).map(|(_, goal)| goal.clone()).collect(),
                answer_operations: if named_company { vec![crate::company_name_request::ANSWER_SCOPE.into()] }
                else if crate::profession_request::recognize(question).is_some() { vec!["Cite the retrieved sources and preserve their scope, observation dates and missing fields.".into()] } else { vec![if canonical.subjects[0].kind == "company" {
                    "Summarize the registered details and available annual figures; preserve the reported financial year, currency, units, consolidation scope, source and missing fields. These available figures do not establish the latest filing or a complete original annual report.".into()
                } else { "Summarize the available requested building or neighbourhood records; preserve their scope, reporting periods, sources and missing fields.".into() }] }, unresolved_references: vec![] };
            super::capability_binding::bind_source_requirements(&mut intent, intake, catalog);
            if prepare(&mut intent, intake, catalog).is_err() || intent.bind_constraint_inputs(question, catalog).is_err()
                || intent.validate(question, task, catalog).is_err() {continue;}
            let mut preview = task.clone(); preview.capability_intake = Some(intake.clone());
            let mut bound = intent.clone();
            if crate::engine::bind_subjects(&mut preview, catalog, &mut bound).is_err() {continue;}
            let crate::resolver::Resolution::Plan(plan) = crate::resolver::resolve(catalog, &preview, &bound) else {continue};
            if !uncovered_outputs(&preview, catalog, &bound, &plan).is_empty() {continue;}
            let message = if named_company {
                if canonical.language == "nl" { "Ik zoek het bedrijf op naam en combineer de geregistreerde gegevens, beschikbare financiële jaarcijfers en website-informatie. Bekijk eerst de totale prijs." }
                else { "I will look up the company by name and combine its registered details, available annual figures and website information. Review the total price first." }
            } else if crate::profession_request::recognize(question).is_some() {
                if canonical.language == "nl" { "Ik haal de gevraagde gegevens uit de gevonden bronnen en toon eerst de totale prijs." }
                else { "I will retrieve the requested data from the matched sources and show the total price first." }
            } else if canonical.subjects[0].kind == "company" { "I will retrieve the registered details and available annual financial figures, preserving their reported year and scope. Review the total price first." }
                else if canonical.language == "nl" {"Ik zoek de beschikbare gegevens bij dit adres en toon eerst de totale prijs."}
                else {"I will retrieve the available records for this address and show the total price first."};
            return Some(super::ChatReply { output_bindings: vec![], message: message.into(), discover: true, intent: Some(intent), decline_reason: Some("none".into()) });
        }
    }
    profession.map(|_| rejected_reply(intake))
}

pub(crate) fn outputs_cover(
    cap: &crate::capability_intake::Capability,
    service: &crate::catalog::Service,
    catalog: &Catalog,
) -> bool {
    !cap.outputs.is_empty() && cap.outputs.iter().all(|output|output_metadata(output) || output_covered(output,service,catalog,false))
}

fn output_metadata(output:&str)->bool {
    matches!(output,"source" | "source_reference" | "source_references" | "source_citations"
        | "missing_fields" | "search_coverage" | "coverage_description")
}

pub(super) fn output_covered(output:&str,service:&crate::catalog::Service,catalog:&Catalog,required_only:bool)->bool {
    if matches!(output,"source_link"|"source_links") {
        let source=service.result_source();
        return [source.get("url"),source.get("usage_notice").and_then(|notice|notice.get("url"))].into_iter().flatten()
            .filter_map(Value::as_str).any(|link|reqwest::Url::parse(link).is_ok_and(|url|
                matches!(url.scheme(),"http"|"https") && url.host_str().is_some() && url.username().is_empty() && url.password().is_none()));
    }
    // Exact normalized concept suffixes and declared schema fields are
    // evidence. Descriptions and opaque records are not. Quotes and shared
    // goals both allow declared optional data without promising its presence.
    let parts = |value: &str| {
        value
            .to_lowercase()
            .split(|ch: char| !ch.is_alphanumeric())
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let normalized_parts=|value:&str| {
        let mut words=parts(value);
        if words.len()>1 && words.last().is_some_and(|word|word=="link") { *words.last_mut().unwrap()="url".into(); }
        words
    };
            let requested = normalized_parts(output);
            !requested.is_empty()
                && service.contract.output_map.produces.keys().any(|fact| {
                    (!required_only || !service.optional_outputs.contains(fact)) && ((normalized_parts(fact).ends_with(&requested)
                        && (!requires_record_shape(output) || catalog.vocabulary.get(fact).is_some_and(|definition| output_shape_matches(output,&definition.value_schema))))
                        || catalog.vocabulary.get(fact).is_some_and(|vocabulary|
                            schema_covers(&vocabulary.value_schema,output,required_only)))
                })
}

/// Explicit record categories cannot be proved by a same-named scalar. A
/// bare plural is deliberately insufficient: earnings can be a scalar, while
/// status and business are not collection names. Information/overview may be
/// source narrative text. Those meanings still need semantic grounding rather
/// than a grammatical guess at execution time.
pub(super) fn requires_record_shape(output:&str)->bool {
    matches!(output.rsplit('_').next(), Some("detail" | "details" | "record" | "records"
        | "profile"))
}

fn output_shape_matches(output:&str,schema:&Value)->bool {
    if !requires_record_shape(output) {return true;}
    let kinds:Vec<_>=match &schema["type"] {
        Value::String(kind)=>vec![kind.as_str()],
        Value::Array(kinds)=>kinds.iter().filter_map(Value::as_str).filter(|kind|*kind!="null").collect(),
        _=>vec![],
    };
    if kinds.len()!=1 || !matches!(kinds[0],"object"|"array") {return false;}
    let mut concrete=schema.clone();
    concrete["type"]=json!(kinds[0]);
    crate::ask_v2::intent_fields::documented(&concrete)
}

/// Schema properties support retrieving a field even if an individual row
/// lacks it. A caller may separately ask for guaranteed required-field proof.
/// An opaque object or prose description is not field evidence.
fn schema_covers(schema:&Value,output:&str,required_only:bool)->bool {
    // A reviewed aggregate alias names this documented record, never every
    // possible detail about its subject. Opaque objects cannot prove scope.
    if schema["type"] == "object" && schema["properties"].as_object().is_some_and(|fields| !fields.is_empty())
        && property_alias(schema, output) { return true; }
    if schema.get("type").and_then(Value::as_str)==Some("array") {
        return schema.get("items").is_some_and(|items|schema_covers(items,output,required_only));
    }
    let key=output.strip_prefix("original_").unwrap_or(output);
    let singular=key.strip_suffix('s').filter(|_|!key.ends_with("ss")).unwrap_or(key);
    let required=schema.get("required").and_then(Value::as_array);
    let properties=schema.get("properties").and_then(Value::as_object);
    let names:BTreeSet<_>=required.into_iter().flatten().filter_map(Value::as_str)
        .chain(properties.into_iter().flat_map(|props|props.keys().map(String::as_str)).filter(|_|!required_only)).collect();
    names.iter().any(|name| {
        let child=properties.and_then(|properties|properties.get(*name));
        let named=*name==output || *name==singular || qualified_link_equivalent(name,output) || qualified_link_equivalent(name,singular);
        (named && (!requires_record_shape(output) || child.is_some_and(|child|output_shape_matches(output,child))))
            || child.is_some_and(|child|((property_alias(child,output) || property_alias(child,singular))
                && output_shape_matches(output,child)) || schema_covers(child,output,required_only))
    })
}

fn qualified_link_equivalent(name:&str,requested:&str)->bool {
    name.strip_suffix("_url").zip(requested.strip_suffix("_link"))
        .or_else(||name.strip_suffix("_link").zip(requested.strip_suffix("_url")))
        .is_some_and(|(left,right)|!left.is_empty() && left==right)
}

// A reviewed, property-local annotation in an existing JSON Schema keyword.
// It does not change value validation or extraction in either executor.
fn property_alias(property:&Value,output:&str)->bool {
    let aliases = output_aliases(property);
    aliases.contains(output) || aliases.iter().any(|name|qualified_link_equivalent(name,output))
}

/// Reviewed aliases shared with the standalone intent field registry.
pub(crate) fn output_aliases(property:&Value)->BTreeSet<String> {
    let Some(comment)=property.get("$comment").and_then(Value::as_str).filter(|text|text.len()<=2048) else{return BTreeSet::new()};
    let Ok(Value::Object(object))=serde_json::from_str::<Value>(comment) else{return BTreeSet::new()};
    if object.len()!=1 {return BTreeSet::new();}
    let Some(aliases)=object.get("apiosk_output_aliases_v1").and_then(Value::as_array).filter(|values|!values.is_empty()&&values.len()<=32) else{return BTreeSet::new()};
    let mut unique=BTreeSet::new();
    if !aliases.iter().all(|alias|alias.as_str().is_some_and(|name|!name.is_empty()&&name.len()<=100
        && name.bytes().all(|b|b.is_ascii_lowercase()||b.is_ascii_digit()||b==b'_') && unique.insert(name.to_owned()))) {return BTreeSet::new();}
    unique
}

fn dependency_steps(plan:&crate::resolver::Plan,task:&crate::state::Task,catalog:&Catalog,root:usize,source:Option<&str>,included:&mut BTreeSet<usize>) {
    if !included.insert(root) {return;}
    let step=&plan.steps[root];
    for required in step.service.contract.requires.iter().filter(|required|!required.optional) {
        if crate::catalog::fact_value(&task.facts,step.entity_ref,&required.fact_type).is_ok_and(|fact|fact.is_some()) {continue;}
        let native=source.filter(|source|catalog.services.iter().any(|service|service.api_slug==*source
            && service.contract.output_map.produces.contains_key(&required.fact_type)));
        let guaranteed=catalog.services.iter().any(|service|service.contract.output_map.produces.contains_key(&required.fact_type)
            && native.is_none_or(|source|service.api_slug==source) && !service.optional_outputs.contains(&required.fact_type));
        if let Some((index,_))=plan.steps[..root].iter().enumerate().find(|(_,candidate)|candidate.entity_ref==step.entity_ref
            && candidate.service.contract.output_map.produces.contains_key(&required.fact_type)
            && (!guaranteed || !candidate.service.optional_outputs.contains(&required.fact_type))
            && native.is_none_or(|source|candidate.service.api_slug==source)) {
            dependency_steps(plan,task,catalog,index,source,included);
        }
    }
}

/// Proof from the actual selected plan, including its required dependency
/// steps. A different provider, unused catalog service or another subject
/// cannot establish coverage. Presentation instructions never add facts.
/// Independent drafts must prove their requested outputs before asking for
/// inputs too. An unresolved identifier is not evidence of output coverage.
pub(super) fn uncovered_intent_outputs(
    intake: &Intake,
    catalog: &Catalog,
    intent: &Intent,
    bindings: &[super::source_grounding::Binding],
) -> Vec<String> {
    let mut missing = Vec::new();
    for (index, cap) in intake.parsed_request.capabilities.iter().enumerate()
        .filter(|(_, cap)| !answer_instruction(cap))
    {
        let found = request_match(intake, index);
        let subject = cap.subject_id.as_ref()
            .and_then(|id| intake.parsed_request.subjects.iter().find(|subject| &subject.id == id));
        let services: Vec<_> = catalog.services.iter().filter(|service| intent.goals.iter().any(|goal|
            intent.subjects.get(goal.subject).is_some_and(|selected|
                subject.is_none_or(|subject| subject.label.eq_ignore_ascii_case(&selected.label)))
                && found.is_some_and(|found| found.operations.contains(&goal.capability)
                    && found.apiosk.iter().any(|candidate| candidate_matches(goal, candidate)))
                && service.contract.capability_slug == goal.capability
                && goal.constraints.get("source").and_then(Value::as_str) == Some(service.api_slug.as_str())
                && endpoint_matches(goal.constraints.get("endpoint_id"), Some(&service.endpoint_id.to_string()))
        )).collect();
        for output in cap.outputs.iter().filter(|output| !output_metadata(output)) {
            if !services.iter().any(|service| output_covered(output, service, catalog, false))
                && !super::source_grounding::covered(bindings, index, output, &services, catalog)
            {
                missing.push(format!("{}: {}", cap.name, output));
            }
        }
    }
    missing
}

pub(crate) fn uncovered_outputs(task:&crate::state::Task,catalog:&Catalog,intent:&Intent,plan:&crate::resolver::Plan)->Vec<String> {
    let Some(intake)=task.capability_intake.as_ref() else {return vec![]};
    let bindings = task.output_grounding.as_ref().map(|grounding|grounding.bindings_for(intake,catalog)).unwrap_or(&[]);
    let mut missing=Vec::new();
    for (index,cap) in intake.parsed_request.capabilities.iter().enumerate().filter(|(_,cap)|!answer_instruction(cap)) {
        let found=request_match(intake,index);
        let literal_subject=cap.subject_id.as_ref().and_then(|id|intake.parsed_request.subjects.iter().find(|subject|&subject.id==id));
        let goals:BTreeSet<_>=intent.goals.iter().enumerate().filter(|(_,goal)|
            intent.subjects.get(goal.subject).is_some_and(|subject|
                literal_subject.is_none_or(|literal|literal.label.eq_ignore_ascii_case(&subject.label)))
                && found.is_some_and(|found|found.operations.contains(&goal.capability)
                    && found.apiosk.iter().any(|candidate|candidate_matches(goal,candidate))))
            .map(|(index,_)|index).collect();
        let mut contributing=BTreeSet::new();
        for index in goals {
            let goal=&intent.goals[index];let entity=intent.subjects[goal.subject].entity_ref;
            let source=goal.constraints.get("source").and_then(Value::as_str);
            for (root,step) in plan.steps.iter().enumerate().filter(|(_,step)|Some(step.entity_ref)==entity
                && step.service.contract.capability_slug==goal.capability
                && source.is_none_or(|source|step.service.api_slug==source)
                && endpoint_matches(goal.constraints.get("endpoint_id"),Some(&step.service.endpoint_id.to_string()))) {
                let _=step;
                dependency_steps(plan,task,catalog,root,source,&mut contributing);
            }
        }
        for output in cap.outputs.iter().filter(|output|!output_metadata(output)) {
            let proven=contributing.iter().any(|index|output_covered(output,&plan.steps[*index].service,catalog,false))
                || super::source_grounding::covered(bindings,index,output,&contributing.iter().map(|index|&plan.steps[*index].service).collect::<Vec<_>>(),catalog);
            if !proven {missing.push(format!("{}: {}",cap.name,output));}
        }
    }
    missing
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        capability_intake::{Candidate, Match, ParsedRequest},
        parser::{Goal, Subject},
    };

    #[test]
    fn official_crunchbase_profiles_declare_only_their_reviewed_public_scope() {
        let catalog = Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/crunchbase/official-mcp.json")).unwrap());
        for (slug, outputs, unavailable) in [
            ("company.crunchbase-profile", vec!["profile", "company_profile", "description", "website", "headcount", "founders", "employees", "board_members", "funding_rounds", "investors"], vec!["customer_ratings", "private_notes", "current_jobs"]),
            ("person.crunchbase-profile", vec!["profile", "person_profile", "description", "current_jobs", "board_roles", "founded_organizations", "investments"], vec!["employees", "funding_rounds", "private_notes"]),
            ("funding.crunchbase-profile", vec!["profile", "funding_profile", "money_raised", "announced_on", "investment_type", "funded_company", "investors"], vec!["employees", "board_members", "private_notes"]),
        ] {
            let service = catalog.services.iter().find(|s| s.contract.capability_slug == slug).unwrap();
            for output in outputs { assert!(output_covered(output,service,&catalog,false), "{slug}: {output}"); }
            for output in unavailable { assert!(!output_covered(output,service,&catalog,false), "{slug}: {output}"); }
        }
        let search = catalog.services.iter().find(|s| s.contract.capability_slug == "company.crunchbase-official-search").unwrap();
        assert!(!output_covered("board_members",search,&catalog,false), "identity search is not a relationship profile");
    }

    #[test]
    fn split_crunchbase_requirements_share_one_profile_in_a_large_directory() {
        assert_split_crunchbase_profile(false);
        assert_split_crunchbase_profile(true);
    }

    #[test]
    fn profile_url_repeated_as_subject_label_keeps_exact_scope() {
        let catalog = Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/crunchbase/official-mcp.json")).unwrap());
        let question = "Use Crunchbase to retrieve the full company profile for https://www.crunchbase.com/organization/starbucks, including available founders, employees, board members, funding rounds and investors.";
        let mut parsed: ParsedRequest = serde_json::from_str(include_str!("../../tests/fixtures/crunchbase/profile-intake.json")).unwrap();
        parsed.subjects[0].label = parsed.subjects[0].identifiers["url"].clone();
        parsed.capabilities.truncate(1);
        parsed.capabilities[0].outputs = ["profile", "founders", "employees", "board_members", "funding_rounds", "investors"].map(str::to_owned).to_vec();
        let matches = crate::ask_v2::search::search(&parsed, &catalog);
        let intake = Intake { selected_capabilities:matches.iter().flat_map(|m|m.operations.iter().cloned()).collect(),
            parsed_request:parsed,matches,apiosk_sources_searched:1,catalog_version:catalog.version.clone() };
        let reply = single_retrieval_reply(&intake, &catalog, question, question, &Default::default()).expect("a repeated exact URL must remain executable");
        let mut intent = reply.intent.unwrap();
        let mut task = crate::state::Task {capability_intake:Some(intake.clone()),..Default::default()};
        crate::engine::bind_subjects(&mut task,&catalog,&mut intent).unwrap();
        let crate::resolver::Resolution::Plan(plan) = crate::resolver::resolve(&catalog,&task,&intent) else {panic!("profile URL must resolve")};
        assert_eq!(plan.steps.len(),1);
        assert_eq!(intent.subjects[0].values["company.crunchbase_entity_id"],json!("starbucks"));
        assert!(uncovered_inputs(&task,&catalog,&intent,&plan).is_empty());
        assert!(uncovered_outputs(&task,&catalog,&intent,&plan).is_empty());
        for url in ["https://www.crunchbase.com/organization/starbucks?scope=private", "https://www.crunchbase.com/person/starbucks",
            "https://www.crunchbase.com/organization/starbucks/people", "https://crunchbase.com.evil.test/organization/starbucks"] {
            let mut invalid = intake.clone();
            invalid.parsed_request.subjects[0].identifiers.insert("url".into(),url.into());
            assert!(single_retrieval_reply(&invalid,&catalog,question,question,&Default::default()).is_none(),"{url}");
            task.capability_intake=Some(invalid);
            assert!(!uncovered_inputs(&task,&catalog,&intent,&plan).is_empty(),"{url}");
        }
        let mut conflicting = intake;
        conflicting.parsed_request.subjects[0].identifiers.insert("profile_url".into(),"https://www.crunchbase.com/organization/another-company".into());
        assert!(single_retrieval_reply(&conflicting,&catalog,question,question,&Default::default()).is_none());
    }

    fn assert_split_crunchbase_profile(directors_wording: bool) {
        let mut snapshot: crate::catalog::Snapshot = serde_json::from_str(include_str!("../../tests/fixtures/crunchbase/official-mcp.json")).unwrap();
        // Common directory terms must not hide the explicitly named source.
        for n in 0..50 {
            let mut other = snapshot.known_sources[0].clone();
            other.slug = format!("unrelated-{n}"); other.name = format!("Unrelated {n}");
            other.metadata.description = "Company profile, funding rounds and investors".into();
            snapshot.known_sources.push(other);
        }
        let catalog = Catalog::build(snapshot);
        let mut parsed_request: ParsedRequest = serde_json::from_str(include_str!("../../tests/fixtures/crunchbase/profile-intake.json")).unwrap();
        if directors_wording {
            // Another real extraction of the same question uses the synonym
            // in its description and separates investors from funding rounds.
            parsed_request.capabilities[3].description = "Retrieve the list of individuals serving on the company's board of directors.".into();
            parsed_request.capabilities[4].outputs = vec!["funding_rounds".into()];
            let mut investors = parsed_request.capabilities[4].clone();
            investors.slug = "company.investors".into();
            investors.name = "Company investors".into();
            investors.description = "Retrieve the list of investors involved in the company's funding rounds.".into();
            investors.outputs = vec!["investors".into()];
            parsed_request.capabilities.push(investors);
        }
        let matches = crate::ask_v2::search::search(&parsed_request,&catalog);
        for found in &matches { assert!(found.operations.contains("company.crunchbase-profile"), "{}: {:?}",found.slug,found.operations); }
        let intake = Intake { selected_capabilities:matches.iter().flat_map(|m|m.operations.iter().cloned()).collect(),
            parsed_request,matches,apiosk_sources_searched:1,catalog_version:catalog.version.clone() };
        let question = "Use Crunchbase to retrieve the full company profile for https://www.crunchbase.com/organization/starbucks, including available founders, employees, board members, funding rounds and investors.";
        let recovered = single_retrieval_reply(&intake, &catalog, question, question, &Default::default()).expect("the matched profile must recover every field requirement without model-supplied constraints");
        let mut intent = recovered.intent.unwrap();
        assert_eq!(intent.goals.len(), 1);
        assert_eq!(intent.goals[0].capability, "company.crunchbase-profile");
        let mut unavailable = intake.clone();
        unavailable.parsed_request.capabilities[3].outputs.push("private_notes".into());
        assert!(single_retrieval_reply(&unavailable, &catalog, question, question, &Default::default()).is_none(), "never quote a partial profile");
        let mut another_subject = intake.clone();
        another_subject.parsed_request.capabilities[3].subject_id = Some("another-company".into());
        assert!(single_retrieval_reply(&another_subject, &catalog, question, question, &Default::default()).is_none(), "never merge requirements belonging to different subjects");
        prepare(&mut intent,&intake,&catalog).unwrap();
        let mut task = crate::state::Task {capability_intake:Some(intake),..Default::default()};
        crate::engine::bind_subjects(&mut task,&catalog,&mut intent).unwrap();
        let crate::resolver::Resolution::Plan(plan) = crate::resolver::resolve(&catalog,&task,&intent) else {panic!("all requested profile data must resolve together")};
        assert_eq!(plan.steps.len(),1);assert_eq!(plan.cap_atomic,54_347);
        assert!(uncovered_outputs(&task,&catalog,&intent,&plan).is_empty());
    }

    #[test]
    fn repeated_country_with_distinct_years_preserves_both_literals_for_the_normal_planner() {
        let question = "Get GDP for Germany in 2023 from Eurostat. Get GDP for Germany in 2024 from Eurostat.";
        let request = crate::compound_request::recognize(question, "fixture").unwrap();
        assert_eq!(request.parsed.subjects.len(), 2);
        assert_eq!(request.parsed.subjects[0].identifiers["eurostat.input.time"], "2023");
        assert_eq!(request.parsed.subjects[1].identifiers["eurostat.input.time"], "2024");
        assert_eq!(request.parsed.capabilities[0].subject_id.as_deref(), Some("s1"));
        assert_eq!(request.parsed.capabilities[1].subject_id.as_deref(), Some("s2"));
        let catalog = Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/statistics-presets.json")).unwrap());
        let intake_for = |parsed_request: ParsedRequest| {
            let matches = crate::ask_v2::search::search(&parsed_request, &catalog);
            Intake { selected_capabilities: matches.iter().flat_map(|found| found.operations.iter().cloned()).collect(),
                parsed_request, matches, apiosk_sources_searched: 1, catalog_version: catalog.version.clone() }
        };
        // Each clause is executable; only combining different period inputs
        // under the same subject label exceeds this shortcut's scope.
        for clause in &request.clauses {
            assert!(single_retrieval_reply(&intake_for(clause.parsed.clone()), &catalog, &clause.question, &clause.question, &Default::default()).is_some());
        }
        let intake = intake_for(request.parsed);
        let before = serde_json::to_value(&intake).unwrap();
        assert!(compound_reply(&intake, &catalog, question, &Default::default()).is_none());
        assert_eq!(serde_json::to_value(&intake).unwrap(), before);
    }

    #[tokio::test]
    async fn bare_kvk_check_quotes_one_exact_profile_and_keeps_explicit_fields_required() {
        bare_kvk_check_contract(false).await;
    }

    #[tokio::test]
    async fn repaired_kvk_contract_still_quotes_the_profile_and_keeps_explicit_fields_required() {
        bare_kvk_check_contract(true).await;
    }

    async fn bare_kvk_check_contract(repaired: bool) {
        let question = "87560488 check dit kvk nummer";
        let mut snapshot: crate::catalog::Snapshot = serde_json::from_str(include_str!("../../tests/fixtures/workflows/catalog.json")).unwrap();
        if repaired {
            let migration = include_str!("../../supabase/migrations/20261003203509_support_kvk_sole_trader_profiles.sql");
            for service in snapshot.services.iter_mut().filter(|s| s.api_slug == "kvk-dutch-business-register" && s.path == "/api/v1/basisprofielen") {
                service.contract.output_map = serde_json::from_str(migration.split("$repaired_output$").nth(1).unwrap()).unwrap();
                service.contract.version = 8;
                service.optional_outputs.push("company.profile".into());
            }
        }
        let catalog = Catalog::build(snapshot);
        let intake = crate::capability_intake::offline_fixture_intake(question, &catalog);
        let reply = offline_fixture_reply(&intake, &catalog, question, &Default::default()).unwrap();
        assert!(reply.discover, "{}", reply.message);
        let mut intent = reply.intent.unwrap();
        let mut task = crate::state::Task { capability_intake: Some(intake.clone()), ..Default::default() };
        crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
        let crate::resolver::Resolution::Plan(plan) = crate::resolver::resolve(&catalog, &task, &intent) else { panic!("the supplied KVK must resolve to a profile quote") };
        assert_eq!(plan.steps.len(), 1, "a known KVK needs no company-name search or annual accounts");
        let step = &plan.steps[0];
        assert_eq!(step.service.api_slug, "kvk-dutch-business-register");
        assert_eq!(step.service.contract.capability_slug, "company.registry-profile");
        assert_eq!(step.service.inputs(&task, step.entity_ref, &catalog.vocabulary).unwrap(), json!({"kvkNummer":"87560488"}));
        assert!(plan.cap_atomic > 0);
        assert!(uncovered_outputs(&task, &catalog, &intent, &plan).is_empty());

        for field in ["email", "capital", "employees"] {
            let mut extra = intake.clone();
            extra.parsed_request.capabilities[0].outputs.push(field.into());
            assert!(!crate::capability_intake::company_lookup::matches_intake(question, &extra));
            assert!(single_retrieval_reply(&extra, &catalog, question, question, &Default::default()).is_none());
            task.capability_intake = Some(extra);
            assert_eq!(uncovered_outputs(&task, &catalog, &intent, &plan), [format!("Company profile: {field}")]);
        }
        let mut changed = intake.clone();
        changed.parsed_request.subjects[0].identifiers.insert("kvk_number".into(), "24330087".into());
        assert!(!crate::capability_intake::company_lookup::matches_intake(question, &changed));
        changed = intake.clone();
        changed.parsed_request.deliverable.format = "json".into();
        assert!(!crate::capability_intake::company_lookup::matches_intake(question, &changed));
        let mut absent = intake;
        absent.matches.iter_mut().for_each(|matched| matched.apiosk.clear());
        assert!(single_retrieval_reply(&absent, &catalog, question, question, &Default::default()).is_none());
    }

    fn broad_location_fixture(question: &str) -> (Intake, Catalog) {
        let catalog = crate::ask_v2::test_catalog::locations();
        assert!(catalog.rejected.is_empty());
        let parsed_request = crate::capability_intake::offline_fixture_intake(question, &catalog).parsed_request;
        let matches = crate::ask_v2::search::search(&parsed_request, &catalog);
        let selected_capabilities = matches.iter().flat_map(|matched| matched.operations.iter().cloned()).collect();
        (Intake { parsed_request, matches, selected_capabilities, apiosk_sources_searched: 1, catalog_version: catalog.version.clone() }, catalog)
    }

    fn profession_catalog() -> Catalog {
        let mut snapshot: crate::catalog::Snapshot = serde_json::from_str(include_str!("../../tests/fixtures/profession-output-coverage/catalog.json")).unwrap();
        let reviewed: Value = serde_json::from_str(include_str!("../../tests/fixtures/profession-output-coverage/reviewed-schemas.json")).unwrap();
        for fact in &mut snapshot.vocabulary {
            if let Some(schema) = reviewed["schemas"].get(&fact.slug) { fact.value_schema = schema.clone(); }
        }
        let catalog = Catalog::build(snapshot);
        assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
        catalog
    }

    #[tokio::test]
    async fn profession_fixture_inputs_bind_without_partial_scope() {
        let catalog = profession_catalog();
        for (question, expected) in [
            ("Get the latest official ECB reference exchange rate for EUR/USD, including the observation date, via Frankfurter.", vec![("fx.ecb-reference-rate", json!({"quote":"USD"}))]),
            ("Validate EU VAT number NL002033354B01 through VIES and show its validity and check date.", vec![("vat.eu.validate", json!({"cc":"NL","nr":"002033354B01"}))]),
            ("Get RDW vehicle data for registration plate GZ805N, including make, trade name, inspection expiry date and the outstanding recall indicator.", vec![("vehicle.technical-profile", json!({"kenteken":"GZ805N"}))]),
            ("Get public RDAP registration data and mail DNS records for example.org. Cite the sources.", vec![("domain.registration", json!({"domain":"example.org"})), ("domain.mail-dns", json!({"domain":"example.org"}))]),
            ("Geef de laatste officiële ECB-referentiewisselkoers voor EUR/USD met waarnemingsdatum via Frankfurter.", vec![("fx.ecb-reference-rate", json!({"quote":"USD"}))]),
            ("Geef de laatste officiële ECB-referentiewisselkoers voor EUR/GBP met waarnemingsdatum via Frankfurter.", vec![("fx.ecb-reference-rate", json!({"quote":"GBP"}))]),
            ("Valideer EU-btw-nummer NL002033354B01 via VIES en toon de geldigheid en controledatum.", vec![("vat.eu.validate", json!({"cc":"NL","nr":"002033354B01"}))]),
            ("Geef RDW-voertuiggegevens voor kenteken GZ-805-N, inclusief merk, handelsbenaming, APK-vervaldatum en de indicator voor openstaande terugroepacties.", vec![("vehicle.technical-profile", json!({"kenteken":"GZ-805-N"}))]),
            ("Geef openbare RDAP-registratiegegevens en mail-DNS-records voor example.org. Citeer de bronnen.", vec![("domain.registration", json!({"domain":"example.org"})), ("domain.mail-dns", json!({"domain":"example.org"}))]),
            ("Geef openbare RDAP-registratiegegevens en mail-DNS-records voor mollie.nl. Citeer de bronnen.", vec![("domain.registration", json!({"domain":"mollie.nl"})), ("domain.mail-dns", json!({"domain":"mollie.nl"}))]),
        ] {
            let intake = crate::capability_intake::offline_fixture_intake(question, &catalog);
            let reply = offline_fixture_reply(&intake, &catalog, question, &Default::default()).unwrap();
            assert!(reply.discover, "{question}: {}", reply.message);
            if question.starts_with("Get ") || question.starts_with("Validate ") {
                assert_eq!(intake.parsed_request.language, "en");
                assert_eq!(reply.message, "I will retrieve the requested data from the matched sources and show the total price first.");
            }
            let mut intent = reply.intent.unwrap();
            let mut task = crate::state::Task { capability_intake: Some(intake.clone()), ..Default::default() };
            crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
            let crate::resolver::Resolution::Plan(plan) = crate::resolver::resolve(&catalog, &task, &intent) else { panic!("{question}: expected complete plan") };
            assert_eq!(plan.steps.len(), expected.len(), "{question}");
            for (capability, inputs) in expected {
                let step = plan.steps.iter().find(|s| s.service.contract.capability_slug == capability).unwrap();
                assert_eq!(step.service.inputs(&task, step.entity_ref, &catalog.vocabulary).unwrap(), inputs, "{question}");
            }
            assert!(uncovered_outputs(&task, &catalog, &intent, &plan).is_empty());
            let mut extra = intake.clone(); extra.parsed_request.capabilities[0].outputs.push("owner_identity".into());
            assert!(broad_information_reply(&extra, &catalog, question, &Default::default()).is_none(), "extra scope never discarded");
            let mut changed = intake.clone(); changed.parsed_request.subjects[0].identifiers.insert("extra_requirement".into(), "different input".into());
            assert!(broad_information_reply(&changed, &catalog, question, &Default::default()).is_none(), "changed identifiers never discarded");
        }
    }

    #[test]
    fn profession_planner_keeps_missing_contracts_and_unsupported_targets_blocked() {
        let catalog = profession_catalog();
        for question in [
            "Geef de laatste officiële ECB-referentiewisselkoers voor EUR/AAA met waarnemingsdatum via Frankfurter.",
            "Valideer EU-btw-nummer US12345 via VIES en toon de geldigheid en controledatum.",
        ] {
            let parsed_request = crate::profession_request::parse(question, "fixture").unwrap();
            let matches = crate::ask_v2::search::search(&parsed_request, &catalog);
            let intake = Intake { parsed_request, selected_capabilities: matches.iter().flat_map(|m| m.operations.iter().cloned()).collect(), matches, apiosk_sources_searched: 1, catalog_version: catalog.version.clone() };
            assert!(!broad_information_reply(&intake, &catalog, question, &Default::default()).unwrap().discover);
        }
        let question = "Geef openbare RDAP-registratiegegevens en mail-DNS-records voor example.org. Citeer de bronnen.";
        let parsed_request = crate::profession_request::parse(question, "fixture").unwrap();
        let matches = crate::ask_v2::search::search(&parsed_request, &catalog);
        let mut intake = Intake { parsed_request, selected_capabilities: matches.iter().flat_map(|m| m.operations.iter().cloned()).collect(), matches, apiosk_sources_searched: 1, catalog_version: catalog.version.clone() };
        for matched in &mut intake.matches { matched.apiosk.retain(|candidate| candidate.capability.as_deref() != Some("domain.mail-dns")); }
        assert!(!broad_information_reply(&intake, &catalog, question, &Default::default()).unwrap().discover, "registration alone is not a complete domain+mail request");
    }

    #[tokio::test]
    async fn profession_newly_planned_get_vat_and_hyphenated_plate_reach_source_cited_answers() {
        let catalog = profession_catalog();
        for (question, capability) in [
            ("Valideer EU-btw-nummer NL004495445B01 via VIES en toon de geldigheid en controledatum.", "vat.eu.validate"),
            ("Geef RDW-voertuiggegevens voor kenteken GZ-805-N, inclusief merk, handelsbenaming, APK-vervaldatum en de indicator voor openstaande terugroepacties.", "vehicle.technical-profile"),
        ] {
            let intake = crate::capability_intake::offline_fixture_intake(question, &catalog);
            let mut intent = offline_fixture_reply(&intake, &catalog, question, &Default::default()).unwrap().intent.unwrap();
            let mut task = crate::state::Task { capability_intake: Some(intake), ..Default::default() };
            crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
            let crate::resolver::Resolution::Plan(plan) = crate::resolver::resolve(&catalog, &task, &intent) else { panic!("expected one complete lookup") };
            assert_eq!(plan.steps.len(), 1);
            let step = &plan.steps[0]; assert_eq!(step.service.contract.capability_slug, capability);
            // GET VIES uses its captured normalized GET response; the vehicle
            // payload is the separately saved paid GZ805N response. This is an
            // offline contract/composition test, not a fresh paid execution.
            let payload = if capability == "vat.eu.validate" { step.service.sample_payload.clone().unwrap() }
                else { serde_json::from_str::<Value>(include_str!("../../tests/fixtures/profession-examples/vehicle.json")).unwrap()["results"][0]["data"].clone() };
            let extracted = step.service.contract.extract(&payload, &catalog.vocabulary).unwrap();
            assert!(extracted.rejected.is_empty());
            let mut source = step.service.result_source(); source["path"] = json!(step.service.path);
            let sources = vec![json!({"result_ref":uuid::Uuid::new_v4(),"document":{
                "subject":{"entity_ref":step.entity_ref},"source":source,
                "coverage":"source_response","missing_fields":[],"data":payload}})];
            let answer = crate::profession_answer::compose(question, &intent, &sources).unwrap().unwrap();
            assert_eq!(answer["status"], "completed");
            assert!(!answer["presentation"]["rows"].as_array().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn profession_plan_cannot_purchase_a_future_unreviewed_route_with_the_same_fields() {
        let question = "Geef openbare RDAP-registratiegegevens en mail-DNS-records voor example.org. Citeer de bronnen.";
        for changed in ["provider", "capability", "method", "path"] {
            let mut snapshot: crate::catalog::Snapshot = serde_json::from_str(include_str!("../../tests/fixtures/profession-output-coverage/catalog.json")).unwrap();
            let service = snapshot.services.iter_mut().find(|s| s.contract.capability_slug == "domain.registration").unwrap();
            match changed {
                "provider" => service.api_slug = "unreviewed-domain-provider".into(),
                "capability" => service.contract.capability_slug = "domain.unreviewed-registration".into(),
                "method" => service.method = "POST".into(),
                "path" => service.path = "/unreviewed/{domain}".into(),
                _ => unreachable!(),
            }
            let catalog = Catalog::build(snapshot); assert!(catalog.rejected.is_empty());
            let intake = crate::capability_intake::offline_fixture_intake(question, &catalog);
            let reply = offline_fixture_reply(&intake, &catalog, question, &Default::default()).unwrap();
            assert!(!reply.discover && reply.intent.is_none(), "{changed} cannot reach a purchase");
        }
    }

    #[test]
    fn broad_location_request_collects_actual_api_inputs_without_a_lookup_chain() {
        for question in ["Find building and neighbourhood information for Damrak 1, Amsterdam.",
            "Geef gebouw- en buurtinformatie voor Grote Markt 1, Groningen."] {
            let (intake, catalog) = broad_location_fixture(question);
            let reply = crate::parser::contract_planner::compile(question, question, &Default::default(), &catalog, &intake);
            assert!(reply.discover, "{}", reply.message);
            let mut intent = reply.intent.unwrap();
            let mut task = crate::state::Task { capability_intake: Some(intake), ..Default::default() };
            crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
            assert!(matches!(crate::resolver::resolve(&catalog, &task, &intent), crate::resolver::Resolution::Missing { .. }));
            assert!(task.proposal.is_none() && task.pending_dispatches.is_empty());
            assert!(intent.goals.iter().all(|goal| goal.capability != "location.property-search"));
        }
    }

    #[test]
    fn aggregate_retrieval_allows_only_contributing_same_subject_goals() {
        let question = "Find building and neighbourhood information for Damrak 1, Amsterdam.";
        let (mut intake, catalog) = broad_location_fixture(question);
        let original = crate::parser::contract_planner::compile(question, question, &Default::default(), &catalog, &intake).intent.unwrap();
        intake.parsed_request.capabilities.truncate(1);
        let cap = &mut intake.parsed_request.capabilities[0];
        cap.slug = "location.building.neighbourhood.info".into();
        cap.name = "Building and neighbourhood information".into();
        cap.description = "Retrieve official building records and neighbourhood statistics for an address.".into();
        cap.outputs.push("neighbourhood_info".into());
        intake.matches = crate::ask_v2::search::search(&intake.parsed_request, &catalog);
        let mut combined = original.clone();
        prepare(&mut combined, &intake, &catalog).unwrap();
        assert_eq!(combined.goals.len(), 2, "one aggregate retrieval may require two distinct record sources");
        let unit = catalog.services.iter().find(|service| service.contract.capability_slug == "building.bag-unit").unwrap();
        let mut extra = combined.clone();
        extra.goals.push(Goal { capability: unit.contract.capability_slug.clone(), subject: 0,
            constraints: BTreeMap::from([("source".into(),json!(unit.api_slug)),("endpoint_id".into(),json!(unit.endpoint_id))]) });
        assert!(prepare(&mut extra, &intake, &catalog).is_err(), "a dependency may not become an extra requested purchase");
        let mut wrong_subject = original.clone(); wrong_subject.subjects[0].label = "Coolsingel 40, Rotterdam".into();
        assert!(prepare(&mut wrong_subject, &intake, &catalog).is_err());
        for field in ["floor_count", "energy_label", "building_name"] {
            let mut explicit = intake.clone(); explicit.parsed_request.capabilities[0].outputs.push(field.into());
            assert!(prepare(&mut original.clone(), &explicit, &catalog).is_err(), "unsupported explicit {field} remains required");
            assert!(broad_information_reply(&explicit, &catalog, question, &Default::default()).is_none());
        }
    }

    #[test]
    fn reviewed_record_aliases_never_claim_opaque_or_unrelated_fields() {
        let catalog = crate::ask_v2::test_catalog::locations();
        let building = catalog.services.iter().find(|service| service.contract.capability_slug == "building.bag-building").unwrap();
        let unit = catalog.services.iter().find(|service| service.contract.capability_slug == "building.bag-unit").unwrap();
        assert!(output_covered("building_info", building, &catalog, false));
        assert!(!output_covered("building_info", unit, &catalog, false));
        for field in ["floor_count", "energy_label", "building_name"] {assert!(!output_covered(field, building, &catalog, false));}
        let mut opaque = catalog.vocabulary["location.building_record"].value_schema.clone();
        opaque.as_object_mut().unwrap().remove("properties");
        assert!(!schema_covers(&opaque, "building_info", false));
    }

    fn saved_vies() -> (String,Intake,Catalog) {
        let fixture:Value=serde_json::from_str(include_str!("../../tests/fixtures/profession-vies-planning.json")).unwrap();
        (fixture["question"].as_str().unwrap().into(),serde_json::from_value(fixture["intake"].clone()).unwrap(),
            Catalog::build(serde_json::from_value(fixture["catalog"].clone()).unwrap()))
    }

    #[test]
    fn saved_single_vat_retrieval_recovers_only_complete_matched_operations() {
        let (question,intake,catalog)=saved_vies();
        assert!(catalog.rejected.is_empty());
        for capability in ["vat.eu.check","vat.eu.validate"] {
            let mut selected=intake.clone();
            selected.matches[0].apiosk.retain(|candidate|candidate.capability.as_deref()==Some(capability));
            let reply=single_retrieval_reply(&selected,&catalog,&question,&question,&Default::default()).expect("documented VAT validity/date and supplied VAT number resolve");
            let intent=reply.intent.unwrap();
            assert_eq!(intent.goals.len(),1);assert_eq!(intent.goals[0].capability,capability);
            assert_eq!(intent.subjects[0].values["vat.eu.country_code"],json!("NL"));
            assert_eq!(intent.subjects[0].values["vat.eu.number"],json!("002033354B01"));
            assert_eq!(intent.subjects[0].label,"NL002033354B01");
        }
        let mut status=intake.clone();
        status.matches[0].apiosk.retain(|candidate|candidate.capability.as_deref().is_some_and(|cap|cap.contains("status")));
        assert!(!status.matches[0].apiosk.is_empty());
        assert!(single_retrieval_reply(&status,&catalog,&question,&question,&Default::default()).is_none(),"service uptime does not validate the VAT number");
        let mut unsupported=intake.clone();unsupported.parsed_request.capabilities[0].outputs.push("vat_registration_date".into());
        assert!(single_retrieval_reply(&unsupported,&catalog,&question,&question,&Default::default()).is_none());
        let mut many=intake.clone();many.parsed_request.capabilities.push(many.parsed_request.capabilities[0].clone());
        assert!(single_retrieval_reply(&many,&catalog,&question,&question,&Default::default()).is_none(),"no dropped or collapsed second retrieval");
        assert!(single_retrieval_reply(&intake,&catalog,"Validate VAT number DE999999999","Validate VAT number DE999999999",&Default::default()).is_none(),"saved literals cannot be transplanted into another request");
        let mut formatted=intake.clone();
        formatted.parsed_request.deliverable.operations=vec!["summarize".into(),"compare".into()];
        formatted.parsed_request.deliverable.format="json".into();
        let intent=single_retrieval_reply(&formatted,&catalog,&question,&question,&Default::default()).unwrap().intent.unwrap();
        assert_eq!(intent.answer_operations,["summarize","compare","Requested answer format: json"]);
    }

    #[test]
    fn resolved_input_proof_keeps_every_identifier_part_and_unknown_filter() {
        let (question,intake,catalog)=saved_vies();
        let mut intent=single_retrieval_reply(&intake,&catalog,&question,&question,&Default::default()).unwrap().intent.unwrap();
        let mut task=crate::state::Task {capability_intake:Some(intake.clone()),..Default::default()};
        crate::engine::bind_subjects(&mut task,&catalog,&mut intent).unwrap();
        let crate::resolver::Resolution::Plan(plan)=crate::resolver::resolve(&catalog,&task,&intent) else {panic!("saved VAT plan")};
        assert!(retrieval_inputs_preserved(&intake,&catalog,&task,&intent,&plan,&question));
        // The full VAT number cannot be proved by just its country prefix, or
        // by just its national number, even if output coverage is unchanged.
        for omitted in ["vat.eu.number","vat.eu.country_code"] {
            let mut partial=plan.clone();
            for step in &mut partial.steps {step.service.contract.input_map.remove(omitted);}
            assert!(!retrieval_inputs_preserved(&intake,&catalog,&task,&intent,&partial,&question),"{omitted}");
        }
        for field in ["location","ticker","as_of_date"] {
            let mut filtered=intake.clone();
            filtered.parsed_request.subjects[0].identifiers.insert(field.into(),"unbound value".into());
            assert!(single_retrieval_reply(&filtered,&catalog,&question,&question,&Default::default()).is_none(),"{field}");
            let mut missing=intake.clone();missing.parsed_request.capabilities[0].missing.push(field.into());
            assert!(single_retrieval_reply(&missing,&catalog,&question,&question,&Default::default()).is_none(),"missing {field}");
        }
    }

    #[test]
    fn country_filter_requires_fixed_endpoint_scope_not_provider_headquarters() {
        let catalog=Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/tenderned-browse.json")).unwrap());
        let question="Find public tenders for cleaning services in the Netherlands through TenderNed.";
        let mut intake=crate::capability_intake::offline_fixture_intake(question,&catalog);
        let subject=&mut intake.parsed_request.subjects[0];
        subject.identifiers.retain(|key,_|key=="country");
        subject.identifiers.insert("country".into(),"the Netherlands".into());
        intake.parsed_request.capabilities[0].inputs=vec!["topic".into(),"country".into()];
        assert!(single_retrieval_reply(&intake,&catalog,question,question,&Default::default()).is_some());
        for country in ["Belgium","outside the Netherlands","Netherlands and Belgium"] {
            let mut other=intake.clone();other.parsed_request.subjects[0].identifiers.insert("country".into(),country.into());
            assert!(single_retrieval_reply(&other,&catalog,question,question,&Default::default()).is_none(),"{country}");
        }
        let (_,_,vat_catalog)=saved_vies();
        let mut global=vat_catalog.services[0].clone();
        global.api_slug="global-provider".into();global.contract.capability_slug="company.search".into();
        global.contract.requires.clear();global.contract.input_map.clear();global.contract.identifies=None;
        global.jurisdiction=Some("NL".into());
        assert_eq!(crate::ask_v2::search::fixed_endpoint_country(&global,None),None);
    }

    #[test]
    fn model_bound_literal_inputs_do_not_require_a_predefined_identifier_alias() {
        let catalog=Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/tenderned-browse.json")).unwrap());
        let question="Find public tenders for cleaning services in the Netherlands through TenderNed.";
        let mut intake=crate::capability_intake::offline_fixture_intake(question,&catalog);
        let mut intent=single_retrieval_reply(&intake,&catalog,question,question,&Default::default()).unwrap().intent.unwrap();
        intake.parsed_request.subjects[0].kind="service".into();
        intake.parsed_request.subjects[0].identifiers=BTreeMap::from([
            ("service".into(),"cleaning services".into()),("country".into(),"the Netherlands".into())]);
        intake.parsed_request.capabilities[0].inputs=vec!["service".into(),"country".into(),"source".into()];
        let mut task=crate::state::Task {capability_intake:Some(intake),..Default::default()};
        task.record_turn(question,None);
        crate::engine::bind_subjects(&mut task,&catalog,&mut intent).unwrap();
        let crate::resolver::Resolution::Plan(plan)=crate::resolver::resolve(&catalog,&task,&intent) else {panic!("typed model binding must resolve")};
        assert!(uncovered_inputs(&task,&catalog,&intent,&plan).is_empty());
        task.capability_intake.as_mut().unwrap().parsed_request.subjects[0].identifiers.insert("service".into(),"office supplies".into());
        assert!(!uncovered_inputs(&task,&catalog,&intent,&plan).is_empty(),"a different unbound criterion cannot reuse the previous literal");
    }

    #[test]
    fn model_bound_identifying_input_preserves_name_context_without_alias_guessing() {
        let catalog=Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/workflows/catalog.json")).unwrap());
        let question="Show the registered company details for Mollie B.V. (KVK 30204462).";
        let (mut intake,mut intent)=fixture("Mollie B.V.","company",1);
        intake.parsed_request.subjects[0].identifiers=BTreeMap::from([
            ("company_name".into(),"Mollie B.V.".into()),("kvk".into(),"30204462".into())]);
        let cap=&mut intake.parsed_request.capabilities[0];
        cap.slug="company.profile".into();cap.name="Company profile".into();cap.description="Registered company details".into();cap.domain="company".into();
        cap.inputs=vec!["company_name".into(),"kvk".into()];cap.outputs=vec!["profile".into()];
        intake.matches=crate::ask_v2::search::search(&intake.parsed_request,&catalog);
        let service=catalog.services.iter().find(|service|service.api_slug=="kvk-dutch-business-register"
            && service.contract.capability_slug=="company.registry-profile").unwrap();
        intent.goals[0].capability=service.contract.capability_slug.clone();
        intent.goals[0].constraints=BTreeMap::from([("source".into(),json!(service.api_slug)),("endpoint_id".into(),json!(service.endpoint_id))]);
        intent.subjects[0].values.insert("company_registry.kvknummer".into(),json!("30204462"));
        let mut task=crate::state::Task {capability_intake:Some(intake.clone()),..Default::default()};task.record_turn(question,None);
        crate::engine::bind_subjects(&mut task,&catalog,&mut intent).unwrap();
        let crate::resolver::Resolution::Plan(plan)=crate::resolver::resolve(&catalog,&task,&intent) else {panic!("explicit identifying binding resolves")};
        assert!(uncovered_inputs(&task,&catalog,&intent,&plan).is_empty(),"model-proven exact identity permits repeated company-name context");
        assert!(!retrieval_inputs_preserved(&intake,&catalog,&task,&intent,&plan,question),"the deterministic compiler must not guess unfamiliar aliases");
        for (key,value) in [("location","Seattle"),("ticker","EXAMPLE"),("company_name","Other Company B.V."),("kvk","87654321")] {
            let mut changed=task.clone();changed.capability_intake.as_mut().unwrap().parsed_request.subjects[0].identifiers.insert(key.into(),value.into());
            assert!(!uncovered_inputs(&changed,&catalog,&intent,&plan).is_empty(),"{key} is not dispensable name context");
        }
        let mut unnamed_identity=task.clone();
        let parsed=&mut unnamed_identity.capability_intake.as_mut().unwrap().parsed_request;
        parsed.subjects[0].identifiers=BTreeMap::from([("company_name".into(),"Mollie B.V.".into())]);
        parsed.capabilities[0].inputs=vec!["company_name".into()];
        assert!(!uncovered_inputs(&unnamed_identity,&catalog,&intent,&plan).is_empty(),"an invented ID cannot turn a supplied name into context");
        let mut no_identity=plan.clone();for step in &mut no_identity.steps {step.service.contract.identifies=None;}
        assert!(!uncovered_inputs(&task,&catalog,&intent,&no_identity).is_empty(),"an ordinary filter binding cannot serve as entity identity");
        let mut names_as_id=task.clone();
        let parsed=&mut names_as_id.capability_intake.as_mut().unwrap().parsed_request;
        parsed.subjects[0].identifiers.insert("company_name".into(),"30204462".into());
        parsed.subjects[0].identifiers.remove("kvk");parsed.capabilities[0].inputs=vec!["company_name".into()];
        assert!(!input_bindings_preserved(names_as_id.capability_intake.as_ref().unwrap(),&catalog,&names_as_id,&intent,&plan.steps,question,InputProof::complete(true)),
            "a contradictory company name is not the displayed subject merely because it matches the ID input");
    }

    #[test]
    fn empty_subject_currency_recovery_requires_the_explicit_fixed_base_pair() {
        let (_,mut intake,_)=saved_vies();
        let catalog=Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/frankfurter/catalog.json")).unwrap());
        let service=&catalog.services[0];
        intake.parsed_request.subjects.clear();
        let cap=&mut intake.parsed_request.capabilities[0];
        cap.subject_id=None;cap.source_hint=Some(service.api_slug.clone());
        cap.inputs.clear();cap.missing.clear();
        cap.outputs=vec!["rate".into(),"date".into(),"source_link".into()];
        intake.matches[0].operations=BTreeSet::from([service.contract.capability_slug.clone()]);
        intake.matches[0].apiosk.truncate(1);
        let candidate=&mut intake.matches[0].apiosk[0];
        candidate.capability=Some(service.contract.capability_slug.clone());
        candidate.source_slug=Some(service.api_slug.clone());candidate.endpoint_id=Some(service.endpoint_id.to_string());
        let question="Geef de laatste ECB referentiekoers van EUR naar USD met datum en bronlink.";
        use sha2::Digest;
        intake.parsed_request.meta.text_sha256=hex::encode(sha2::Sha256::digest(question.as_bytes()));
        let intent=single_retrieval_reply(&intake,&catalog,question,question,&Default::default()).unwrap().intent.unwrap();
        assert_eq!(intent.subjects[0].values["fx.ecb.quote"],json!("USD"));
        for question in ["ECB USD naar EUR","ECB BTC naar USD; EUR als voorbeeld","ECB EUR naar USD en GBP","ECB USD"] {
            intake.parsed_request.meta.text_sha256=hex::encode(sha2::Sha256::digest(question.as_bytes()));
            assert!(single_retrieval_reply(&intake,&catalog,question,question,&Default::default()).is_none(),"{question}");
        }
    }

    fn reviewed_currency_scope_fixture()->(&'static str,Intake,Catalog) {
        let mut snapshot:crate::catalog::Snapshot=serde_json::from_str(include_str!("../../tests/fixtures/frankfurter/catalog.json")).unwrap();
        let reviewed:Value=serde_json::from_str(include_str!("../../tests/fixtures/semantic-intake-aliases/reviewed-schemas.json")).unwrap();
        for fact in &mut snapshot.vocabulary {
            if let Some(schema)=reviewed["after"].as_array().unwrap().iter().find(|row|row["slug"]==fact.slug) {
                fact.value_schema=schema["value_schema"].clone();
            }
        }
        let catalog=Catalog::build(snapshot);assert!(catalog.rejected.is_empty(),"{:?}",catalog.rejected);
        let question="Get the latest official ECB reference exchange rate for EUR/USD, including the observation date, via Frankfurter.";
        let (mut intake,_)=fixture("EUR/USD","currency_pair",1);
        intake.parsed_request.subjects[0].identifiers=BTreeMap::from([
            ("currency_pair".into(),"EUR/USD".into()),("observation_date".into(),"latest".into()),
            ("rate_type".into(),"official ECB reference exchange rate".into())]);
        let cap=&mut intake.parsed_request.capabilities[0];
        cap.slug="finance.exchangeRate".into();cap.name="ECB reference exchange rate".into();
        cap.description="Retrieve the latest official ECB reference exchange rate for a currency pair.".into();
        cap.domain="finance".into();cap.source_hint=Some("frankfurter".into());
        cap.inputs=vec!["currency_pair".into(),"rate_type".into(),"observation_date".into()];
        cap.outputs=vec!["exchange_rate".into(),"observation_date".into()];
        use sha2::Digest;
        intake.parsed_request.meta.text_sha256=hex::encode(sha2::Sha256::digest(question.as_bytes()));
        intake.matches=crate::ask_v2::search::search(&intake.parsed_request,&catalog);
        (question,intake,catalog)
    }

    #[test]
    fn reviewed_fixed_scope_proves_only_the_selected_endpoint_semantics() {
        let (question,intake,catalog)=reviewed_currency_scope_fixture();
        let mut intent=single_retrieval_reply(&intake,&catalog,question,question,&Default::default())
            .expect("reviewed fixed latest ECB scope completes the dynamically parsed request").intent.unwrap();
        let mut task=crate::state::Task {capability_intake:Some(intake.clone()),..Default::default()};
        task.record_turn(question,None);
        crate::engine::bind_subjects(&mut task,&catalog,&mut intent).unwrap();
        let crate::resolver::Resolution::Plan(plan)=crate::resolver::resolve(&catalog,&task,&intent) else {panic!("reviewed currency plan")};
        assert_eq!(intent.subjects[0].values["fx.ecb.quote"],json!("USD"));
        assert!(uncovered_inputs(&task,&catalog,&intent,&plan).is_empty());
        for (key,value) in [("observation_date","2024-01-01"),("observation_date","latest before yesterday"),
            ("rate_type","intraday market exchange rate"),("rate_type","central bank rate") ] {
            let mut filtered=intake.clone();filtered.parsed_request.subjects[0].identifiers.insert(key.into(),value.into());
            assert!(single_retrieval_reply(&filtered,&catalog,question,question,&Default::default()).is_none(),"{key}={value}");
            let mut filtered_task=task.clone();filtered_task.capability_intake=Some(filtered);
            assert!(!uncovered_inputs(&filtered_task,&catalog,&intent,&plan).is_empty(),"model plan cannot drop {key}={value}");
        }
        let mut dependency_only=intent.clone();dependency_only.goals[0].capability="unrelated.retrieval".into();
        assert!(!retrieval_inputs_preserved(&intake,&catalog,&task,&dependency_only,&plan,question),"fixed scope cannot be borrowed from a non-goal step");
    }

    #[test]
    fn prequote_input_proof_rejects_orphan_subjects_even_with_empty_declared_inputs() {
        let (question,intake,catalog)=reviewed_currency_scope_fixture();
        let mut intent=single_retrieval_reply(&intake,&catalog,question,question,&Default::default()).unwrap().intent.unwrap();
        let mut task=crate::state::Task {capability_intake:Some(intake),..Default::default()};
        task.record_turn(question,None);
        crate::engine::bind_subjects(&mut task,&catalog,&mut intent).unwrap();
        let crate::resolver::Resolution::Plan(plan)=crate::resolver::resolve(&catalog,&task,&intent) else {panic!("reviewed currency plan")};
        let parsed=&mut task.capability_intake.as_mut().unwrap().parsed_request;
        parsed.capabilities[0].subject_id=None;parsed.capabilities[0].inputs.clear();
        assert!(!uncovered_inputs(&task,&catalog,&intent,&plan).is_empty(),"subjectless cap cannot discard supplied currency and time scope");
        let parsed=&mut task.capability_intake.as_mut().unwrap().parsed_request;
        parsed.capabilities[0].subject_id=Some(parsed.subjects[0].id.clone());
        let mut orphan=parsed.subjects[0].clone();orphan.id="unassigned".into();orphan.label="Other subject".into();
        orphan.identifiers=BTreeMap::from([("location".into(),"Seattle".into())]);parsed.subjects.push(orphan);
        assert!(!uncovered_inputs(&task,&catalog,&intent,&plan).is_empty(),"another matched subject cannot mask an unassigned location criterion");
    }

    #[test]
    fn fixed_scope_annotation_requires_exact_route_inputs_and_reviewed_values() {
        let (_,_,catalog)=reviewed_currency_scope_fixture();
        let service=&catalog.services[0];
        assert!(fixed_scope_covered("observation_date","latest",service,&catalog));
        assert!(fixed_scope_covered("rate_type","OFFICIAL ECB  reference exchange rate",service,&catalog));
        assert!(!fixed_scope_covered("settlement_date","latest",service,&catalog));
        for changed in ["source","endpoint","method","path","added_input","removed_input","output"] {
            let mut changed_service=service.clone();
            match changed {
                "source"=>changed_service.api_slug="other-provider".into(),
                "endpoint"=>changed_service.endpoint_id=uuid::Uuid::new_v4(),
                "method"=>changed_service.method="POST".into(),
                "path"=>changed_service.path="/historical/EUR/{quote}".into(),
                "added_input"=>{let input=changed_service.contract.input_map["fx.ecb.quote"].clone();changed_service.contract.input_map.insert("fx.date".into(),input);},
                "removed_input"=>changed_service.contract.input_map.clear(),
                "output"=>changed_service.contract.output_map.produces.clear(),
                _=>unreachable!(),
            }
            assert!(!fixed_scope_covered("observation_date","latest",&changed_service,&catalog),"{changed}");
        }
        for comment in [None,Some("not-json".to_owned()),Some("{}".into()),Some("x".repeat(16_385))] {
            let mut unreviewed=catalog.clone();
            let schema=&mut unreviewed.vocabulary.get_mut("fx.ecb.reference_rate").unwrap().value_schema;
            match comment {Some(comment)=>schema["$comment"]=json!(comment),None=>{schema.as_object_mut().unwrap().remove("$comment");}}
            assert!(!fixed_scope_covered("observation_date","latest",service,&unreviewed));
        }
    }

    #[test]
    fn full_question_query_is_only_context_when_query_input_is_not_requested() {
        let (question,mut intake,catalog)=saved_vies();
        intake.parsed_request.subjects[0].identifiers.insert("query".into(),question.clone());
        assert!(single_retrieval_reply(&intake,&catalog,&question,&question,&Default::default()).is_some());
        for query_field in ["query","web.query","web_query"] {
            let mut explicit=intake.clone();explicit.parsed_request.capabilities[0].inputs.push(query_field.into());
            assert!(single_retrieval_reply(&explicit,&catalog,&question,&question,&Default::default()).is_none(),"{query_field}");
        }
        let mut missing=intake.clone();missing.parsed_request.capabilities[0].missing.push("query".into());
        assert!(single_retrieval_reply(&missing,&catalog,&question,&question,&Default::default()).is_none());
        intake.parsed_request.subjects[0].identifiers.insert("query".into(),"different actual query".into());
        assert!(single_retrieval_reply(&intake,&catalog,&question,&question,&Default::default()).is_none());
    }

    #[test]
    fn whole_query_text_cannot_substitute_for_explicit_source_selection() {
        let question="Find a company profile via other-provider";
        let mut snapshot:crate::catalog::Snapshot=serde_json::from_str(include_str!("../../tests/fixtures/workflows/catalog.json")).unwrap();
        snapshot.services.retain(|service|service.contract.capability_slug=="web.search");snapshot.known_sources.clear();
        let catalog=Catalog::build(snapshot);
        let (mut intake,_)=fixture("company profile","topic",1);
        intake.parsed_request.subjects[0].identifiers=BTreeMap::from([("query".into(),question.into())]);
        let cap=&mut intake.parsed_request.capabilities[0];
        cap.slug="research.web.search".into();cap.name="Public web search".into();cap.description="Search public web pages".into();
        cap.inputs=vec!["query".into()];cap.outputs=vec!["url".into(),"title".into()];
        use sha2::Digest;
        intake.parsed_request.meta.text_sha256=hex::encode(sha2::Sha256::digest(question.as_bytes()));
        intake.matches=crate::ask_v2::search::search(&intake.parsed_request,&catalog);
        let mut intent=single_retrieval_reply(&intake,&catalog,question,question,&Default::default()).expect("whole query is an ordinary valid web input").intent.unwrap();
        let mut task=crate::state::Task {capability_intake:Some(intake.clone()),..Default::default()};task.record_turn(question,None);
        crate::engine::bind_subjects(&mut task,&catalog,&mut intent).unwrap();
        let crate::resolver::Resolution::Plan(plan)=crate::resolver::resolve(&catalog,&task,&intent) else {panic!("bound whole query")};
        assert!(uncovered_inputs(&task,&catalog,&intent,&plan).is_empty());
        for key in ["source","source_hint","provider"] {
            let mut filtered=intake.clone();filtered.parsed_request.subjects[0].identifiers.insert(key.into(),"other-provider".into());
            assert!(single_retrieval_reply(&filtered,&catalog,question,question,&Default::default()).is_none(),"{key}");
            let mut filtered_task=task.clone();filtered_task.capability_intake=Some(filtered);
            assert!(!uncovered_inputs(&filtered_task,&catalog,&intent,&plan).is_empty(),"{key} cannot be supplied as mere query text");
        }
    }

    fn combined_reputation_fixture(name:&str,city:&str)->(String,Intake,Catalog) {
        let saved:Value=serde_json::from_str(include_str!("../../tests/fixtures/company-reputation/combined-intake.json")).unwrap();
        let changed=serde_json::to_string(&saved).unwrap().replace("Starbucks",name).replace("Seattle",city);
        let saved:Value=serde_json::from_str(&changed).unwrap();
        let question=saved["question"].as_str().unwrap().to_owned();
        let mut snapshot:crate::catalog::Snapshot=serde_json::from_str(include_str!("../../tests/fixtures/company-reputation/catalog.json")).unwrap();
        let reviewed:Value=serde_json::from_str(include_str!("../../tests/fixtures/ecb-split-currency/reviewed-schemas.json")).unwrap();
        for fact in &mut snapshot.vocabulary {
            if let Some(row)=reviewed["after"].as_array().unwrap().iter().find(|row|row["slug"]==fact.slug) {fact.value_schema=row["value_schema"].clone();}
        }
        let catalog=Catalog::build(snapshot);assert!(catalog.rejected.is_empty());
        let mut parsed:ParsedRequest=serde_json::from_value(saved["parsed_request"].clone()).unwrap();
        use sha2::Digest;
        parsed.meta.text_sha256=hex::encode(sha2::Sha256::digest(question.as_bytes()));
        let matches=crate::ask_v2::search::search(&parsed,&catalog);
        let selected_capabilities=matches.iter().flat_map(|found|found.operations.iter().cloned()).collect();
        (question,Intake {parsed_request:parsed,matches,selected_capabilities,apiosk_sources_searched:1,catalog_version:catalog.version.clone()},catalog)
    }

    #[tokio::test]
    async fn dynamic_compiler_combines_recorded_ai_requirements_and_changed_literals() {
        for (name,city) in [("Starbucks","Seattle"),("Fictional Coffee Co","Ghent")] {
            let (question,mut intake,catalog)=combined_reputation_fixture(name,city);
            intake.parsed_request.deliverable.operations=vec!["compare".into(),"summarize".into()];
            for (index,cap) in intake.parsed_request.capabilities.iter().enumerate() {
                let mut part=intake.clone();part.parsed_request.capabilities=vec![cap.clone()];
                part.parsed_request.subjects.retain(|subject|Some(&subject.id)==cap.subject_id.as_ref());
                part.matches=vec![request_match(&intake,index).unwrap().clone()];
                assert!(single_retrieval_reply(&part,&catalog,&question,&question,&Default::default()).is_some(),"individual requirement {} must have complete bindings/outputs: {:?}",cap.name,part.matches);
            }
            let parser=crate::parser::Parser::new(reqwest::Client::new(),"offline://extra-planner-forbidden".into(),String::new(),"fixture".into());
            let mut intent=parser.chat_from_capabilities(&question,None,&Default::default(),&catalog,&intake).await
                .expect("interpreted independent requirements compile without a second model").intent.unwrap();
            assert_eq!(intent.subjects.len(),2,"same AI subject is shared; location scope keeps its own subject");
            assert_eq!(intent.goals.len(),3);
            assert!(intent.answer_operations.iter().any(|operation|operation=="compare"));
            assert!(intent.answer_operations.iter().any(|operation|operation=="summarize"));
            let mut task=crate::state::Task {capability_intake:Some(intake),..Default::default()};task.record_turn(&question,None);
            crate::engine::bind_subjects(&mut task,&catalog,&mut intent).unwrap();
            let crate::resolver::Resolution::Plan(plan)=crate::resolver::resolve(&catalog,&task,&intent) else {panic!("combined priced plan")};
            assert_eq!(plan.steps.len(),3);assert!(plan.cap_atomic>0);
            assert!(uncovered_inputs(&task,&catalog,&intent,&plan).is_empty());assert!(uncovered_outputs(&task,&catalog,&intent,&plan).is_empty());
            let yelp=plan.steps.iter().find(|step|step.service.path.contains("yelp")).unwrap();
            let inputs=yelp.service.inputs(&task,yelp.entity_ref,&catalog.vocabulary).unwrap();
            assert_eq!(inputs["query"],name);assert_eq!(inputs["location"],city);
            let crunchbase=plan.steps.iter().find(|step|step.service.contract.capability_slug=="company.crunchbase-search").unwrap();
            assert_eq!(crunchbase.service.inputs(&task,crunchbase.entity_ref,&catalog.vocabulary).unwrap()["query"],name);
            assert_ne!(crunchbase.entity_ref,yelp.entity_ref);
        }
    }

    #[test]
    fn dynamic_compiler_preserves_two_names_and_refuses_incomplete_or_conflicting_scope() {
        let (question,intake,catalog)=combined_reputation_fixture("Starbucks","Seattle");
        assert!(multiple_retrieval_reply(&intake,&catalog,&question,&question,&Default::default()).is_some());
        for fault in ["field","location","match","conflicting_subject","unassigned_subject"] {
            let mut changed=intake.clone();
            match fault {
                "field"=>changed.parsed_request.capabilities[0].outputs.push("annual_revenue".into()),
                "location"=>{changed.parsed_request.subjects[0].identifiers.insert("location".into(),"Seattle".into());},
                "match"=>changed.matches[1].apiosk.clear(),
                "conflicting_subject"=>{let mut other=changed.parsed_request.subjects[0].clone();other.identifiers.insert("company_name".into(),"Other Company".into());changed.parsed_request.subjects.push(other);},
                "unassigned_subject"=>{let mut other=changed.parsed_request.subjects[0].clone();other.id="orphan".into();changed.parsed_request.subjects.push(other);},
                _=>unreachable!(),
            }
            assert!(multiple_retrieval_reply(&changed,&catalog,&question,&question,&Default::default()).is_none(),"{fault} must remain for the model/clarification");
        }
        let question="Find company descriptions and profile links for Starbucks and Mollie on Crunchbase.";
        let mut two=intake.clone();two.parsed_request.subjects.truncate(1);two.parsed_request.capabilities.truncate(1);
        let mut second=two.parsed_request.subjects[0].clone();second.id="other".into();second.label="Mollie".into();
        second.identifiers=BTreeMap::from([("company_name".into(),"Mollie".into())]);two.parsed_request.subjects.push(second);
        let mut second=two.parsed_request.capabilities[0].clone();second.subject_id=Some("other".into());two.parsed_request.capabilities.push(second);
        use sha2::Digest;two.parsed_request.meta.text_sha256=hex::encode(sha2::Sha256::digest(question.as_bytes()));
        two.matches=crate::ask_v2::search::search(&two.parsed_request,&catalog);
        let mut intent=multiple_retrieval_reply(&two,&catalog,question,question,&Default::default()).unwrap().intent.unwrap();
        assert_eq!(intent.subjects.len(),2);assert_eq!(intent.goals.len(),2);
        let mut task=crate::state::Task {capability_intake:Some(two),..Default::default()};task.record_turn(question,None);
        crate::engine::bind_subjects(&mut task,&catalog,&mut intent).unwrap();
        let crate::resolver::Resolution::Plan(plan)=crate::resolver::resolve(&catalog,&task,&intent) else {panic!("two subject plan")};
        assert_eq!(plan.steps.len(),2);
        for (subject,name) in intent.subjects.iter().zip(["Starbucks","Mollie"]) {
            assert_eq!(crate::catalog::fact_value(&task.facts,subject.entity_ref.unwrap(),"company.crunchbase_query").unwrap(),Some(json!(name)));
        }
        assert!(uncovered_inputs(&task,&catalog,&intent,&plan).is_empty());
    }

    #[test]
    fn qualified_link_and_url_names_keep_the_same_resource_qualifier() {
        let schema=json!({"type":"object","properties":{"profile_url":{"type":"string"},"website_url":{"type":"string"},"review_url":{"type":"string"}}});
        assert!(schema_covers(&schema,"profile_link",false));
        assert!(schema_covers(&schema,"profile_links",false));
        assert!(!schema_covers(&json!({"type":"object","properties":{"website_url":{"type":"string"},"review_url":{"type":"string"}}}),"profile_link",false));
        assert!(!schema_covers(&schema,"link",false));
        assert!(!schema_covers(&schema,"funding_profile_link",false));
        assert!(!schema_covers(&schema,"profile_link",true),"optional property is not guaranteed required coverage");
    }

    #[test]
    fn combined_and_split_optional_fields_share_the_same_complete_purchase() {
        for link_output in ["url", "profile_link"] {
            for split in [false, true] {
                let (_,mut intake,catalog)=combined_reputation_fixture("Fictional Coffee Co","Ghent");
                let question="Search Crunchbase for Fictional Coffee Co and show its company description and profile link.";
                use sha2::Digest;
                intake.parsed_request.meta.text_sha256=hex::encode(sha2::Sha256::digest(question.as_bytes()));
                intake.parsed_request.subjects.truncate(1);
                intake.parsed_request.capabilities.truncate(1);
                intake.parsed_request.deliverable.operations.clear();
                let first=&mut intake.parsed_request.capabilities[0];
                first.slug="company.description".into();
                first.name="Company description".into();
                first.description="The company's description.".into();
                first.outputs=vec!["description".into()];
                if split {
                    let mut link=first.clone();
                    link.slug="company.profileLink".into();
                    link.name="Company profile link".into();
                    link.description="The company's profile link.".into();
                    link.outputs=vec![link_output.into()];
                    intake.parsed_request.capabilities.push(link);
                } else {
                    first.outputs.push(link_output.into());
                }
                intake.matches=crate::ask_v2::search::search(&intake.parsed_request,&catalog);
                intake.selected_capabilities=intake.matches.iter().flat_map(|matched|matched.operations.clone()).collect();
                let mut intent=single_retrieval_reply(&intake,&catalog,question,question,&Default::default())
                    .expect("AI field grouping cannot change support for the same declared optional fields").intent.unwrap();
                assert_eq!(intent.goals.len(),1,"all fields use one purchase");
                let mut task=crate::state::Task {capability_intake:Some(intake.clone()),..Default::default()};
                task.record_turn(question,None);
                crate::engine::bind_subjects(&mut task,&catalog,&mut intent).unwrap();
                let crate::resolver::Resolution::Plan(plan)=crate::resolver::resolve(&catalog,&task,&intent) else {panic!("complete priced plan")};
                assert_eq!(plan.steps.len(),1);
                assert_eq!(plan.steps[0].service.contract.capability_slug,"company.crunchbase-search");
                assert_eq!(plan.steps[0].service.inputs(&task,plan.steps[0].entity_ref,&catalog.vocabulary).unwrap()["query"],"Fictional Coffee Co");
                assert_eq!(plan.cap_atomic,100_000);
                assert!(uncovered_outputs(&task,&catalog,&intent,&plan).is_empty());
                assert!(uncovered_inputs(&task,&catalog,&intent,&plan).is_empty());

                let mut unsupported=intake.clone();
                unsupported.parsed_request.capabilities.last_mut().unwrap().outputs.push("annual_revenue".into());
                assert!(single_retrieval_reply(&unsupported,&catalog,question,question,&Default::default()).is_none(),
                    "a missing output cannot disappear from split or combined requirements");

                if link_output=="profile_link" {
                    let mut website_only=catalog.clone();
                    let fields=website_only.vocabulary.get_mut("company.crunchbase_search_results").unwrap().value_schema
                        ["properties"]["entities"]["items"]["properties"]["identifier"]["properties"].as_object_mut().unwrap();
                    fields.remove("url");
                    fields.insert("website_url".into(),json!({"type":"string"}));
                    assert!(single_retrieval_reply(&intake,&website_only,question,question,&Default::default()).is_none(),
                        "a website URL cannot replace the requested profile link");
                    assert!(uncovered_outputs(&task,&website_only,&intent,&plan).iter().any(|missing|missing.contains("profile_link")));
                }
            }
        }
    }

    #[test]
    fn source_link_requires_safe_saved_provenance_not_an_article_field() {
        let (_,_,catalog)=saved_vies();let mut service=catalog.services[0].clone();
        service.source_url=Some("https://example.test/official-source".into());service.usage_notice=None;
        assert!(output_covered("source_link",&service,&catalog,false));
        for requested in ["original_links","article_url","publication_dates"] {assert!(!output_covered(requested,&service,&catalog,false));}
        for invalid in [None,Some("javascript:alert(1)".into()),Some("https://user:password@example.test".into())] {
            service.source_url=invalid;assert!(!output_covered("source_link",&service,&catalog,false));
        }
        service.source_url=None;service.usage_notice=Some(json!({"text":"Primary source","url":"https://www.ecb.europa.eu/"}));
        assert!(output_covered("source_link",&service,&catalog,false));
        assert_eq!(service.result_source()["usage_notice"]["url"],"https://www.ecb.europa.eu/");
    }
    use std::collections::BTreeMap;

    fn catalog() -> Catalog {
        Catalog::build(
            serde_json::from_str(include_str!(
                "../../tests/fixtures/integration-catalog.json"
            ))
            .unwrap(),
        )
    }
    fn fixture(label: &str, kind: &str, count: usize) -> (Intake, Intent) {
        let parsed: ParsedRequest = serde_json::from_value(json!({"language":"en",
            "subjects":[{"id":"s1","label":label,"type":kind,"identifiers":{}}],
            "capabilities":(0..count).map(|n|json!({"slug":format!("research.check{n}"),"name":format!("Check {n}"),"description":"Retrieve this requested check","domain":"research","inputs":[],"outputs":[],"subject_id":"s1","source_hint":null,"confidence":1})).collect::<Vec<_>>(),
            "deliverable":{"format":"chat","operations":[]},"meta":{"model":"fixture","parsed_at":"fixture","text_sha256":"fixture"}})).unwrap();
        let candidates = vec![Candidate {
            origin: "apiosk".into(),
            name: "Fixture".into(),
            description: String::new(),
            source_slug: Some("fixture".into()),
            capability: Some("company.accounts".into()),
            endpoint_id: Some(catalog().services.iter().find(|service| service.contract.capability_slug == "company.accounts").unwrap().endpoint_id.to_string()),
            url: None,
            method: "GET".into(),
            price: None,
            network: None,
            calls_30d: None,
            availability: "supported".into(),
        }];
        let intake = Intake {
            matches: parsed
                .capabilities
                .iter()
                .map(|cap| Match {
                    slug: cap.slug.clone(),
                    query: String::new(),
                    operations: BTreeSet::from(["company.accounts".into()]),
                    apiosk: candidates.clone(),
                    coinbase: vec![],
                    coinbase_status: "skipped".into(),
                })
                .collect(),
            parsed_request: parsed,
            apiosk_sources_searched: 1,
            catalog_version: String::new(),
            selected_capabilities: BTreeSet::from(["company.accounts".into()]),
        };
        let intent = Intent {
            subjects: vec![Subject {
                entity_ref: None,
                label: label.into(),
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
        (intake, intent)
    }
    #[test]
    fn unverified_or_unidentified_candidates_cannot_authorize_a_goal() {
        let (mut intake, intent) = fixture("MONZO BANK LIMITED", "company", 1);
        let supported = intake.matches[0].apiosk[0].clone();
        let mut unverified = supported.clone();
        unverified.endpoint_id = Some(uuid::Uuid::new_v4().to_string());
        unverified.availability = "unverified".into();
        intake.matches[0].apiosk = vec![unverified.clone(), supported.clone()];
        let mut valid = intent.clone();
        prepare(&mut valid, &intake, &catalog()).unwrap();
        assert_eq!(valid.goals[0].constraints["endpoint_id"], json!(supported.endpoint_id));
        let mut invalid = intent.clone();
        invalid.goals[0].constraints.insert("endpoint_id".into(), json!(unverified.endpoint_id));
        assert!(prepare(&mut invalid, &intake, &catalog()).is_err());
        intake.matches[0].apiosk = vec![unverified];
        assert!(!retrievals_matched(&intake));
        assert!(uncovered_reply(&intake).is_some(), "stale operation sets must reveal the readiness gap before planning");
        assert!(prepare(&mut intent.clone(), &intake, &catalog()).is_err(), "stale operations must not make unverified candidates executable");
        intake.matches[0].apiosk = vec![supported];
        assert!(retrievals_matched(&intake));
        assert!(uncovered_reply(&intake).is_none());
        intake.matches[0].apiosk[0].endpoint_id = None;
        assert!(!retrievals_matched(&intake));
        assert!(prepare(&mut intent.clone(), &intake, &catalog()).is_err());
    }
    #[test]
    fn one_operation_cannot_satisfy_multiple_checks_or_duplicate_itself() {
        let (intake, mut intent) = fixture("MONZO BANK LIMITED", "company", 2);
        assert!(prepare(&mut intent, &intake, &catalog())
            .unwrap_err()
            .to_string()
            .contains("requested retrieval"));
        intent.goals.push(intent.goals[0].clone());
        assert_eq!(
            prepare(&mut intent, &intake, &catalog())
                .unwrap_err()
                .to_string(),
            "duplicate retrieval goal"
        );
    }
    #[test]
    fn saved_live_due_diligence_plan_cannot_repeat_ip_to_fill_missing_checks() {
        let rows: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/profession-live-planning.json"
        ))
        .unwrap();
        let row = rows["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["case"] == 13)
            .unwrap();
        let intake: Intake = serde_json::from_value(row["observed_intake"].clone()).unwrap();
        let mut intent: Intent = serde_json::from_value(row["observed_intent"].clone()).unwrap();
        assert_eq!(intent.goals.len(), 16);
        assert!(
            prepare(&mut intent, &intake, &catalog()).is_err(),
            "the historically accepted wrong plan must now be refused before a quote"
        );
    }
    #[test]
    fn one_verified_response_can_cover_several_requested_output_facts() {
        let (mut intake, mut intent) = fixture("MONZO BANK LIMITED", "company", 2);
        let mut catalog = catalog();
        let service = catalog
            .services
            .iter_mut()
            .find(|s| s.contract.capability_slug == "company.accounts")
            .unwrap();
        let extraction = service
            .contract
            .output_map
            .produces
            .values()
            .next()
            .unwrap()
            .clone();
        service
            .contract
            .output_map
            .produces
            .insert("company.registered_name".into(), extraction.clone());
        service
            .contract
            .output_map
            .produces
            .insert("company.registered_address".into(), extraction);
        intake.parsed_request.capabilities[0].outputs = vec!["registered_name".into()];
        intake.parsed_request.capabilities[1].outputs = vec!["registered_address".into()];
        prepare(&mut intent, &intake, &catalog).unwrap();
        assert_eq!(
            intent.goals.len(),
            1,
            "one response, one retrieval purchase"
        );
        intake.parsed_request.capabilities[1].outputs = vec!["insolvency_decision".into()];
        assert!(
            prepare(&mut intent, &intake, &catalog).is_err(),
            "matched words alone cannot prove another field is returned"
        );
    }
    #[test]
    fn unmatched_requirement_reply_is_specific_and_keeps_catalog_provider_claims_out() {
        let (mut intake, _) = fixture("MONZO BANK LIMITED", "company", 1);
        intake.parsed_request.capabilities[0].name = "CBS sector comparison".into();
        intake.matches[0].operations.clear();
        let reply = uncovered_reply(&intake).unwrap();
        assert!(!reply.discover && reply.intent.is_none());
        assert!(reply.message.contains("CBS sector comparison"));
        assert!(!reply.message.contains("CBS is unavailable"));
        assert!(
            reply.decline_reason.is_none(),
            "deterministic catalog facts are not sent for a model rewrite"
        );
    }
    #[test]
    fn platform_availability_is_metadata_but_tracking_remains_retrieval() {
        let mut cap:crate::capability_intake::Capability=serde_json::from_value(json!({
            "slug":"general.capabilities","name":"Apiosk capabilities","description":"Explain available capabilities from confirmed catalog evidence.",
            "domain":"general","inputs":[],"outputs":["available_capabilities","missing_sources"],
            "subject_id":null,"source_hint":null,"confidence":1})).unwrap();
        assert!(answer_instruction(&cap));
        cap.inputs.push("tracking_number".into());assert!(!answer_instruction(&cap));
        cap.inputs.clear();cap.outputs.push("route_delay".into());assert!(!answer_instruction(&cap));
        cap.outputs.pop();cap.name="Company capabilities".into();assert!(!answer_instruction(&cap));
        cap.name="Apiosk capabilities".into();cap.subject_id=Some("company".into());assert!(!answer_instruction(&cap));
    }

    #[test]
    fn quote_output_proof_uses_only_selected_same_subject_dependency_chain() {
        use crate::resolver::{Plan,Step};
        let mut snapshot:crate::catalog::Snapshot=serde_json::from_str(include_str!("../../tests/fixtures/integration-catalog.json")).unwrap();
        let accounts=snapshot.services.iter().find(|s|s.contract.capability_slug=="company.accounts").unwrap().clone();
        let extract=accounts.contract.output_map.produces.values().next().unwrap().clone();
        snapshot.services.iter_mut().find(|s|s.contract.capability_slug=="company.search").unwrap().contract.output_map.produces.insert("location.area".into(),extract.clone());
        let primary=snapshot.services.iter_mut().find(|s|s.contract.capability_slug=="company.accounts").unwrap();
        primary.contract.output_map.produces.clear();primary.contract.output_map.produces.insert("location.construction_year".into(),extract.clone());
        let mut unrelated=accounts.clone();unrelated.id=uuid::Uuid::new_v4();unrelated.endpoint_id=uuid::Uuid::new_v4();
        unrelated.contract.capability_slug="company.other-data".into();
        unrelated.contract.output_map.produces.clear();unrelated.contract.output_map.produces.insert("location.cadastral_parcel_ids".into(),extract.clone());
        snapshot.services.push(unrelated.clone());
        for slug in ["location.area","location.construction_year","location.cadastral_parcel_ids"] {
            snapshot.vocabulary.push(serde_json::from_value(json!({"slug":slug,"value_schema":{"type":"string"}})).unwrap());
        }
        let catalog=Catalog::build(snapshot);
        assert_eq!(catalog.services.len(),3);
        let (mut intake,mut intent)=fixture("Dam 1 Amsterdam","address",2);
        let entity=uuid::Uuid::new_v4();intent.subjects[0].entity_ref=Some(entity);
        intent.goals[0].constraints.insert("endpoint_id".into(),json!(accounts.endpoint_id));
        intent.goals.push(Goal{capability:unrelated.contract.capability_slug.clone(),subject:0,constraints:BTreeMap::from([("endpoint_id".into(),json!(unrelated.endpoint_id))])});
        intake.matches[1].operations=BTreeSet::from([unrelated.contract.capability_slug.clone()]);
        intake.matches[1].apiosk[0].capability=Some(unrelated.contract.capability_slug.clone());
        intake.matches[1].apiosk[0].endpoint_id=Some(unrelated.endpoint_id.to_string());
        intake.parsed_request.capabilities[0].outputs=vec!["area".into(),"construction_year".into(),"cadastral_parcel_ids".into(),"source_references".into(),"missing_fields".into()];
        let task = crate::state::Task { capability_intake:Some(intake), ..Default::default() };
        let steps=["company.search","company.accounts","company.other-data"].into_iter().enumerate().map(|(index,cap)|Step{
            service:catalog.services.iter().find(|s|s.contract.capability_slug==cap).unwrap().clone(),entity_ref:entity,
            goal_index:if index==1 {0}else{1},browser_fallback:None}).collect();
        let mut plan=Plan{independent_calls:false,compute:None,quote_ref:uuid::Uuid::new_v4(),steps,cap_atomic:1,expires_at:chrono::Utc::now(),hash:"test".into(),fee_bps:1500,workflow_slug:None};
        assert_eq!(uncovered_outputs(&task,&catalog,&intent,&plan),vec!["Check 0: cadastral_parcel_ids"],"the reused lookup supplies area but a valid unrelated goal cannot supply parcels");
        plan.steps[0].entity_ref=uuid::Uuid::new_v4();
        assert_eq!(uncovered_outputs(&task,&catalog,&intent,&plan),vec!["Check 0: area","Check 0: cadastral_parcel_ids"],"another subject cannot satisfy the dependency");
        plan.steps[0].entity_ref=entity;
        let mut optional_catalog=catalog;
        optional_catalog.services.iter_mut().find(|s|s.contract.capability_slug=="company.search").unwrap().optional_outputs.push("company.id".into());
        plan.steps[0].service.optional_outputs.push("company.id".into());
        assert_eq!(uncovered_outputs(&task,&optional_catalog,&intent,&plan),vec!["Check 0: cadastral_parcel_ids"],"the sole conditional lookup producer remains the selected dependency");
        plan.steps[1].service.contract.output_map.produces.insert("location.cadastral_parcel_ids".into(),extract);
        assert!(uncovered_outputs(&task,&optional_catalog,&intent,&plan).is_empty());
    }

    #[test]
    fn reviewed_optional_fields_preserve_core_quotes_without_inventing_missing_fields() {
        let mut snapshot:crate::catalog::Snapshot=serde_json::from_str(include_str!("../../tests/fixtures/profession-output-coverage/catalog.json")).unwrap();
        let annotations:Value=serde_json::from_str(include_str!("../../tests/fixtures/profession-output-coverage/reviewed-schemas.json")).unwrap();
        for fact in &mut snapshot.vocabulary {
            if let Some(schema)=annotations["schemas"].get(&fact.slug) {fact.value_schema=schema.clone();}
        }
        let catalog=Catalog::build(snapshot);assert!(catalog.rejected.is_empty(),"{:?}",catalog.rejected);
        for (capability,fields) in [
            ("vehicle.technical-profile",vec!["license_plate","brand","vehicle_type","apk_expiry","outstanding_recall_indicator","primary_color","engine_displacement"]),
            ("company.registry-profile",vec!["legal_name","registration_number","registration_status","legal_form","incorporation_date","registered_address"]),
            ("company.officers",vec!["officers","officer_name","role","appointment_date"]),
            ("company.significant-control",vec!["persons_with_significant_control","person_name","nature_of_control"]),
            ("vat.eu.validate",vec!["is_valid","check_date"]),
        ] {
            let service=catalog.services.iter().find(|service|service.contract.capability_slug==capability).unwrap();
            for field in fields {assert!(output_covered(field,service,&catalog,false),"reviewed {capability} supports {field}");}
        }
        let rdw=catalog.services.iter().find(|service|service.contract.capability_slug=="vehicle.technical-profile").unwrap();
        for absent in ["vin","fuel_type","year_of_manufacture","odometer_reading","recall_description","recall_status"] {
            assert!(!output_covered(absent,rdw,&catalog,false),"an adjacent RDW field cannot stand in for {absent}");
        }
    }

    #[test]
    fn reviewed_property_aliases_preserve_catalog_admission_and_fact_extraction() {
        let baseline:crate::catalog::Snapshot=serde_json::from_str(include_str!("../../tests/fixtures/frankfurter/catalog.json")).unwrap();
        let mut with_alias:crate::catalog::Snapshot=serde_json::from_str(include_str!("../../tests/fixtures/frankfurter/catalog.json")).unwrap();
        let schema=&mut with_alias.vocabulary.iter_mut().find(|fact|fact.slug=="fx.ecb.reference_rate").unwrap().value_schema;
        schema["properties"]["date"]["$comment"]=json!("{\"apiosk_output_aliases_v1\":[\"observation_date\"]}");
        catalog_core::facts::schema::check_schema(schema).unwrap();
        let catalog=Catalog::build(with_alias);let baseline=Catalog::build(baseline);
        assert!(catalog.rejected.is_empty());
        assert!(output_covered("observation_date",&catalog.services[0],&catalog,false));
        let raw:Value=serde_json::from_str(include_str!("../../tests/fixtures/frankfurter/eur-usd.json")).unwrap();
        let original=baseline.services[0].contract.extract(&raw,&baseline.vocabulary).unwrap();
        let annotated=catalog.services[0].contract.extract(&raw,&catalog.vocabulary).unwrap();
        assert_eq!(original,annotated,"planning annotations never change upstream data extraction");
        for comment in ["not-json".to_owned(),"{}".into(),"{\"apiosk_output_aliases_v1\":[\"is_valid\",\"is_valid\"]}".into(),
            "{\"apiosk_output_aliases_v1\":[\"not valid\"]}".into(),"{\"apiosk_output_aliases_v1\":[\"is_valid\"],\"other\":true}".into(),
            "x".repeat(2049),json!({"apiosk_output_aliases_v1":vec!["is_valid";33]}).to_string()] {
            assert!(!property_alias(&json!({"$comment":comment}),"is_valid"));
        }
    }

    #[test]
    fn saved_contracts_prove_declared_fields_but_not_absent_dates_or_opaque_properties() {
        let catalog=Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/profession-output-coverage/catalog.json")).unwrap());
        for (capability,fields) in [
            ("company.registry-profile",vec!["company_name","company_number"]),
            ("company.officers",vec!["officers"]),
            ("company.significant-control",vec!["persons_with_significant_control"]),
            ("vehicle.technical-profile",vec!["technical_page"]),
            ("fx.ecb-reference-rate",vec!["base","quote","date","rate"]),
            ("vat.eu.validate",vec!["country_code","vat_number","valid","request_date","name","address"]),
            ("domain.registration",vec!["domain","registrar","registered_at","nameservers"]),
            ("domain.mail-dns",vec!["domain","exists","accepts_mail","mx","dnssec_validated"]),
            ("news.reuters-search",vec!["titles","original_links"]),
        ] {
            let service=catalog.services.iter().find(|service|service.contract.capability_slug==capability).unwrap();
            for field in fields {assert!(output_covered(field,service,&catalog,false),"{capability} declares {field}");}
        }
        for (capability,field) in [("news.reuters-search","publication_dates"),("building.bag-building","cadastral_parcel_ids"),
            ("building.bag-unit","cadastral_parcel_ids"),("vehicle.technical-profile","brand"),("company.officers","date_of_birth")] {
            let service=catalog.services.iter().find(|service|service.contract.capability_slug==capability).unwrap();
            assert!(!output_covered(field,service,&catalog,false),"{capability} does not document {field}; opaque records require better schema metadata");
        }
        let schema=json!({"type":"object","properties":{"optional_value":{"type":"string"}}});
        assert!(schema_covers(&schema,"optional_value",false));
        assert!(!schema_covers(&schema,"optional_value",true));
        let alias=json!({"type":"object","properties":{"valid":{"type":"boolean","$comment":"{\"apiosk_output_aliases_v1\":[\"is_valid\"]}"}}});
        assert!(schema_covers(&alias,"is_valid",false));
        assert!(!schema_covers(&alias,"validity",false),"aliases match only the exact reviewed concept");
        assert!(!schema_covers(&json!({"type":"object","$comment":"{\"apiosk_output_aliases_v1\":[\"is_valid\"]}"}),"is_valid",false),"aliases must belong to declared fields");
        let officer=json!({"type":"object","properties":{"name":{"type":"string","$comment":"{\"apiosk_output_aliases_v1\":[\"officer_name\"]}"}}});
        for output in ["officer_name","officer_names","original_officer_name","original_officer_names"] {
            assert!(schema_covers(&officer,output,false),"{output}: aliases share the declared-field plural/prefix rules");
        }
        for output in ["shareholder_names","officer_residence_addresses","director_names"] {
            assert!(!schema_covers(&officer,output,false),"{output}: grammatical normalization does not infer other concepts");
        }
        assert!(!schema_covers(&officer,"officer_names",true),"optional fields do not promise a value on every row");
    }

    #[test]
    #[ignore="offline saved component audit; explicit local fixture paths only"]
    fn audit_saved_component_quote_output_coverage() {
        let base=std::env::var("APIOSK_COMPONENT_AUDIT_DIR").expect("sanitized audit directory");
        let snapshot=std::env::var("APIOSK_COMPONENT_AUDIT_CATALOG").expect("saved runtime snapshot");
        let mut snapshot:crate::catalog::Snapshot=serde_json::from_slice(&std::fs::read(snapshot).unwrap()).unwrap();
        let fx:crate::catalog::Snapshot=serde_json::from_slice(&std::fs::read(format!("{base}/frankfurter-runtime-catalog.json")).unwrap()).unwrap();
        snapshot.services.extend(fx.services);snapshot.vocabulary.extend(fx.vocabulary);snapshot.known_sources.extend(fx.known_sources);
        let overlay=std::env::var("APIOSK_COMPONENT_SCHEMA_PROPOSAL").ok();
        if let Some(path)=&overlay {
            let proposal:Value=serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            for change in proposal["changes"].as_array().unwrap() {
                let fact=snapshot.vocabulary.iter_mut().find(|fact|Some(fact.slug.as_str())==change["slug"].as_str()).unwrap();
                assert_eq!(fact.value_schema,change["before_schema"],"offline overlay must match captured schema");
                fact.value_schema=change["after_schema"].clone();
            }
        }
        let catalog=Catalog::build(snapshot);
        let mut results=vec![];
        for file in ["component-quotes-diagnostics.json","component-domain-rvo-diagnostics.json","component-title-gleif-diagnostics.json"] {
            let data:Value=serde_json::from_slice(&std::fs::read(format!("{base}/{file}")).unwrap()).unwrap();
            for raw in data["tasks"].as_array().unwrap() {
                let Some(quote)=raw["quote"].as_object() else {
                    results.push(json!({"task_id":raw["task_id"],"boundary":"no saved quote; existing refusal"}));continue;
                };
                let intent:Intent=serde_json::from_value(raw["intent"].clone()).unwrap();
                let task=crate::state::Task { capability_intake:serde_json::from_value(raw["intake"].clone()).unwrap(), ..Default::default() };
                let mut steps=vec![];
                for entry in quote["steps"].as_array().unwrap() {
                    let service=catalog.services.iter().find(|s|entry["contract_id"].as_str()==Some(&s.id.to_string())
                        || (entry["endpoint_id"].as_str()==Some(&s.endpoint_id.to_string()) && entry["capability"]==s.contract.capability_slug)).expect("exact saved contract").clone();
                    let goal_index=entry["goal_index"].as_u64().map(|n|n as usize).unwrap_or_else(||intent.goals.iter().position(|goal|goal.capability==service.contract.capability_slug).unwrap_or(0));
                    let entity_ref=intent.subjects[intent.goals[goal_index].subject].entity_ref.unwrap();
                    steps.push(crate::resolver::Step{service,entity_ref,goal_index,browser_fallback:None});
                }
                let plan=crate::resolver::Plan{independent_calls:false,compute:None,quote_ref:uuid::Uuid::new_v4(),steps,cap_atomic:0,expires_at:chrono::Utc::now(),hash:"offline audit".into(),fee_bps:1500,workflow_slug:None};
                results.push(json!({"task_id":raw["task_id"],"has_intake":task.capability_intake.is_some(),"missing_output_contracts":uncovered_outputs(&task,&catalog,&intent,&plan)}));
            }
        }
        std::fs::write(format!("{base}/component-output-coverage-impact.json"),serde_json::to_vec_pretty(&json!({"boundary":"Exact saved plans replayed without model, provider or payment calls; catalogs may be older than latest metadata updates.","reviewed_metadata_overlay":overlay.is_some(),"results":results})).unwrap()).unwrap();
    }

    #[test]
    fn missing_source_explanation_preserves_real_property_retrievals() {
        let (mut intake, mut intent) = fixture("Dam 1 Amsterdam", "address", 1);
        let mut cap: crate::capability_intake::Capability = serde_json::from_value(json!({
            "name":"Missing sources","slug":"location.property.missing.sources","domain":"location",
            "inputs":["address"],"missing":[],"outputs":["missing_sources"],"confidence":0.8,
            "subject_id":"s1","source_hint":null,
            "description":"Identifies which data sources are missing for a comprehensive property analysis."})).unwrap();
        assert!(answer_instruction(&cap));
        intake.parsed_request.capabilities.push(cap.clone());
        assert!(retrievals_matched(&intake));
        prepare(&mut intent,&intake,&catalog()).unwrap();
        assert_eq!(intent.goals.len(),1);
        assert!(intent.answer_operations.iter().any(|op|op.contains("Missing sources")));
        cap.outputs.push("soil_conditions".into());
        assert!(!answer_instruction(&cap));
        cap.outputs=vec!["missing_sources".into()];cap.source_hint=Some("public-register".into());
        assert!(!answer_instruction(&cap));
        cap.source_hint=None;cap.inputs.push("website_url".into());
        assert!(!answer_instruction(&cap));
    }

    #[test]
    fn provenance_flags_describe_existing_evidence_without_hiding_new_retrievals() {
        let (mut intake, mut intent) = fixture("ASML", "company", 1);
        let mut cap: crate::capability_intake::Capability = serde_json::from_value(json!({
            "name":"Distinguish official sources from AI analyses","slug":"research.distinguish.sources",
            "domain":"research","inputs":[],"missing":[],
            "outputs":["source_type","is_official","is_ai_generated","source_reference"],
            "confidence":0.9,"subject_id":null,"source_hint":null,
            "description":"Differentiates between information derived from official public databases and that generated by artificial intelligence."})).unwrap();
        assert!(answer_instruction(&cap));
        intake.parsed_request.capabilities.push(cap.clone());
        assert!(retrievals_matched(&intake));
        prepare(&mut intent, &intake, &catalog()).unwrap();
        assert_eq!(intent.goals.len(), 1);
        assert!(intent.answer_operations.iter().any(|op| op.contains("Distinguish official sources")));
        cap.source_hint=Some("official-registry".into());
        assert!(!answer_instruction(&cap));
        cap.source_hint=None;cap.inputs.push("website_url".into());
        assert!(!answer_instruction(&cap));
        cap.inputs.clear();cap.outputs.push("company_ownership".into());
        assert!(!answer_instruction(&cap));
    }

    #[test]
    fn citation_formatting_is_an_answer_instruction_but_citation_data_stays_a_retrieval() {
        let (mut intake, mut intent) = fixture("MONZO BANK LIMITED", "company", 1);
        let mut cap: crate::capability_intake::Capability = serde_json::from_value(json!({
            "slug":"research.source.citation","name":"Source citation",
            "description":"Provides proper citation for data sources used in research.",
            "domain":"research","inputs":["data_source","reference_type"],"missing":["data_source","reference_type"],
            "outputs":["citation"],"subject_id":null,"source_hint":null,"confidence":0.9})).unwrap();
        assert!(answer_instruction(&cap));
        intake.parsed_request.capabilities.push(cap.clone());
        assert!(retrievals_matched(&intake));
        assert!(uncovered_reply(&intake).is_none());
        prepare(&mut intent,&intake,&catalog()).unwrap();
        assert_eq!(intent.goals.len(),1);
        assert!(intent.answer_operations.iter().any(|text|text.contains("Source citation")));
        cap.outputs.push("citation_count".into());
        assert!(!answer_instruction(&cap));
        cap.outputs=vec!["citation".into()];
        cap.subject_id=Some("paper1".into());
        assert!(!answer_instruction(&cap));
        cap.subject_id=None;
        cap.source_hint=Some("crossref".into());
        assert!(!answer_instruction(&cap));
        cap.source_hint=None;
        cap.inputs.push("doi".into());
        assert!(!answer_instruction(&cap));
    }
    #[test]
    fn answer_instructions_are_preserved_without_buying_an_extra_source() {
        let (mut intake, mut intent) = fixture("MONZO BANK LIMITED", "company", 1);
        let mut cap = intake.parsed_request.capabilities[0].clone();
        cap.slug = "research.source.distinction".into();
        cap.subject_id = None;
        cap.inputs.clear();
        cap.name = "Distinguish official data and AI analysis".into();
        cap.outputs = vec![
            "official_sources".into(),
            "ai_analysis".into(),
            "differences".into(),
        ];
        intake.parsed_request.capabilities.push(cap);
        assert!(retrievals_matched(&intake));
        prepare(&mut intent, &intake, &catalog()).unwrap();
        assert_eq!(intent.goals.len(), 1);
        assert!(intent
            .answer_operations
            .iter()
            .any(|item| item.contains("official data")));
        intake.parsed_request.capabilities[1].outputs = vec!["official_sources".into(),"ai_analysis".into(),"differences_in_reliability".into(),"data_origin".into()];
        assert!(answer_instruction(&intake.parsed_request.capabilities[1]), "saved case5 distinctions are answer instructions");
        // Actual saved supplier-screening parse: provenance concerns the same
        // company, but asks for no additional external facts.
        let distinction = &mut intake.parsed_request.capabilities[1];
        distinction.slug = "research.analysis.comparison".into();
        distinction.subject_id = Some("s1".into());
        distinction.inputs = vec!["subject_id".into(), "analysis_sources".into()];
        distinction.outputs = vec!["official_sources".into(),"ai_generated_analysis".into(),"source_confidence".into(),"discrepancies".into()];
        assert!(answer_instruction(distinction));
        assert!(retrievals_matched(&intake));
        intake.parsed_request.capabilities[1]
            .outputs
            .push("external_market_statistics".into());
        assert!(
            !retrievals_matched(&intake),
            "a data requirement must remain a retrieval gap"
        );
    }
    #[test]
    fn differently_written_constraints_cannot_duplicate_the_same_pinned_purchase() {
        let (intake, mut intent) = fixture("MONZO BANK LIMITED", "company", 2);
        let mut duplicate = intent.goals[0].clone();
        duplicate
            .constraints
            .insert("source".into(), json!("fixture"));
        intent.goals.push(duplicate);
        assert_eq!(
            prepare(&mut intent, &intake, &catalog())
                .unwrap_err()
                .to_string(),
            "duplicate retrieval goal"
        );
    }
    #[test]
    fn same_provider_capability_is_pinned_to_the_matched_endpoint_variant() {
        let (mut intake, mut intent) = fixture("MONZO BANK LIMITED", "company", 1);
        let mut catalog = catalog();
        let correct = catalog
            .services
            .iter()
            .find(|service| service.contract.capability_slug == "company.accounts")
            .unwrap()
            .clone();
        let mut wrong = correct.clone();
        wrong.endpoint_id = uuid::Uuid::new_v4();
        wrong.id = uuid::Uuid::new_v4();
        wrong.list_atomic = 1;
        catalog.services.push(wrong.clone());
        intake.matches[0].apiosk[0].endpoint_id = Some(correct.endpoint_id.to_string());
        prepare(&mut intent, &intake, &catalog).unwrap();
        assert_eq!(
            intent.goals[0].constraints["endpoint_id"],
            correct.endpoint_id.to_string()
        );
        let mut task=crate::state::Task::default();
        crate::engine::bind_subjects(&mut task,&catalog,&mut intent).unwrap();
        let crate::resolver::Resolution::Plan(plan)=crate::resolver::resolve(&catalog,&task,&intent) else {panic!("the pinned variant must remain executable through its name lookup");};
        assert!(plan.steps.iter().any(|step|step.service.endpoint_id==correct.endpoint_id));
        assert!(plan.steps.iter().all(|step|step.service.endpoint_id!=wrong.endpoint_id),"the cheaper same-source variant must not substitute");
        intent.goals[0]
            .constraints
            .insert("endpoint_id".into(), json!(wrong.endpoint_id));
        assert!(
            prepare(&mut intent, &intake, &catalog).is_err(),
            "same source/capability is not permission to select an unmatched endpoint"
        );
    }
    #[test]
    fn source_binding_is_scoped_and_company_literal_is_reused() {
        let (intake, mut intent) = fixture("MONZO BANK LIMITED", "company", 1);
        prepare(&mut intent, &intake, &catalog()).unwrap();
        assert_eq!(intent.goals[0].constraints["source"], "fixture");
        assert_eq!(
            intent.subjects[0].values["company.name"],
            "MONZO BANK LIMITED"
        );
        intent.goals[0]
            .constraints
            .insert("source".into(), json!("unmatched-cheaper-source"));
        assert!(prepare(&mut intent, &intake, &catalog()).is_err());
    }
    #[test]
    fn address_and_documented_entity_name_are_bound_without_asking_again() {
        let (intake, mut intent) = fixture("Dam 1 Amsterdam", "address", 1);
        let mut catalog = catalog();
        catalog.vocabulary.insert(
            "geo.input.q".into(),
            serde_json::from_value(json!({"slug":"geo.input.q","value_schema":{"type":"string"}}))
                .unwrap(),
        );
        prepare(&mut intent, &intake, &catalog).unwrap();
        assert_eq!(intent.subjects[0].values["geo.input.q"], "Dam 1 Amsterdam");
        let field = "source_input.e96e543e56e384fe2818ce6b2ea6064ab.name";
        catalog.vocabulary.insert(field.into(),serde_json::from_value(json!({"slug":field,"value_schema":{"type":"string","description":"Person or entity name to screen"}})).unwrap());
        let service = catalog
            .services
            .iter_mut()
            .find(|s| s.contract.capability_slug == "company.accounts")
            .unwrap();
        service.contract.requires[0].fact_type = field.into();
        let (intake, mut intent) = fixture("MONZO BANK LIMITED", "company", 1);
        prepare(&mut intent, &intake, &catalog).unwrap();
        assert_eq!(intent.subjects[0].values[field], "MONZO BANK LIMITED");
        intent.subjects[0]
            .values
            .insert(field.into(), json!("Another Company"));
        assert_eq!(
            prepare(&mut intent, &intake, &catalog)
                .unwrap_err()
                .to_string(),
            "input belongs to a different parsed subject"
        );
    }
    #[test]
    fn two_same_slug_subjects_need_their_own_matched_source_and_goal() {
        let (mut intake, mut intent) = fixture("First Company", "company", 1);
        let mut second = intake.parsed_request.subjects[0].clone();
        second.id = "s2".into();
        second.label = "Second Company".into();
        intake.parsed_request.subjects.push(second);
        let mut cap = intake.parsed_request.capabilities[0].clone();
        cap.subject_id = Some("s2".into());
        intake.parsed_request.capabilities.push(cap);
        let mut matched = intake.matches[0].clone();
        matched.apiosk[0].source_slug = Some("second-provider".into());
        intake.matches.push(matched);
        intent.subjects.push(Subject {
            entity_ref: None,
            label: "Second Company".into(),
            values: BTreeMap::new(),
        });
        let mut goal = intent.goals[0].clone();
        goal.subject = 1;
        intent.goals.push(goal);
        prepare(&mut intent, &intake, &catalog()).unwrap();
        assert_eq!(intent.goals[0].constraints["source"], "fixture");
        assert_eq!(intent.goals[1].constraints["source"], "second-provider");
        intent.goals[1]
            .constraints
            .insert("source".into(), json!("fixture"));
        assert!(prepare(&mut intent, &intake, &catalog()).is_err());
    }

    #[test]
    fn company_reputation_fields_cover_ratings_but_not_reviews_or_full_profiles() {
        let migration = include_str!("../../supabase/migrations/20261011090956_declare_company_reputation_response_fields.sql");
        for (tag, present, absent) in [
            ("crunchbase_output", vec!["description", "profile_url", "crunchbase_url"], vec!["founding_date", "headquarters", "rating"]),
            ("glassdoor_output", vec!["rating", "average_rating", "review_count"], vec!["top_reviews", "customer_rating"]),
            ("yelp_output", vec!["rating", "average_rating", "review_count"], vec!["top_reviews", "employer_rating"]),
        ] {
            let schema: Value = serde_json::from_str(migration.split(&format!("${tag}$")).nth(1).unwrap()).unwrap();
            catalog_core::facts::schema::check_schema(&schema).unwrap();
            for field in present { assert!(schema_covers(&schema, field, false), "{tag}: {field}"); }
            for field in absent { assert!(!schema_covers(&schema, field, false), "{tag}: {field}"); }
            if tag != "crunchbase_output" {
                catalog_core::facts::schema::validate(&schema, &json!({"document":{"status":"OK","data":[]}})).unwrap();
                assert!(catalog_core::facts::schema::validate(&schema, &json!({"document":{"status":"ERROR","data":[]}})).is_err());
            }
        }
    }

    #[test]
    fn company_reputation_keeps_typed_coverage_after_registered_routes_are_added() {
        let catalog = Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/company-reputation/catalog.json")).unwrap());
        assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
        assert_eq!(catalog.services.len(), 3);
        assert_eq!(catalog.services.iter().map(|service| service.total()).sum::<i64>(), 106_900);
        for service in &catalog.services {
            let outputs = if service.path == "/autocompletes" { vec!["short_description", "url"] } else { vec!["rating", "review_count"] };
            for output in outputs { assert!(output_covered(output, service, &catalog, false), "{}: {output}", service.path); }
            assert!(!output_covered("top_reviews", service, &catalog, false));
            assert!(!output_covered("founding_date", service, &catalog, false));
            let extracted = service.contract.extract(service.sample_payload.as_ref().unwrap(), &catalog.vocabulary).unwrap();
            assert!(extracted.rejected.is_empty());
            assert_eq!(extracted.subjects.len(), 1);
        }
    }
}

#[cfg(test)]
mod reviewed_semantic_scope_tests {
    use super::*;

    fn reviewed() -> Value {
        serde_json::from_str(include_str!("../../tests/fixtures/semantic-intake-aliases/reviewed-schemas.json")).unwrap()
    }

    fn overlay(raw: &str, version: &str) -> Catalog {
        let mut snapshot: crate::catalog::Snapshot = serde_json::from_str(raw).unwrap();
        for fact in &mut snapshot.vocabulary {
            if let Some(row) = reviewed()[version].as_array().unwrap().iter().find(|row| row["slug"] == fact.slug) {
                fact.value_schema = row["value_schema"].clone();
            }
        }
        let catalog = Catalog::build(snapshot);
        assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
        catalog
    }

    #[test]
    fn saved_model_tender_intake_binds_keywords_and_preserves_all_filters() {
        use sha2::Digest;
        let fixture: Value = serde_json::from_str(include_str!("../../tests/fixtures/semantic-intake-aliases/luna-tender-intake.json")).unwrap();
        let question = fixture["question"].as_str().unwrap();
        let catalog = overlay(include_str!("../../tests/fixtures/tenderned-browse.json"), "after");
        let parsed_request = serde_json::from_value(fixture["parsed_request"].clone()).unwrap();
        let matches = crate::ask_v2::search::search(&parsed_request, &catalog);
        let intake = Intake {selected_capabilities:matches.iter().flat_map(|found|found.operations.iter().cloned()).collect(),
            parsed_request,matches,apiosk_sources_searched:1,catalog_version:catalog.version.clone()};
        let reply = single_retrieval_reply(&intake,&catalog,question,question,&Default::default()).expect("reviewed tender data and exact filters are executable");
        let intent = reply.intent.unwrap();
        assert_eq!(intent.goals.len(),1);
        assert_eq!(intent.goals[0].capability,"procurement.nl-public-contracts");
        assert_eq!(intent.subjects[0].values["company_intel.input.search"],json!("cleaning services"));
        assert!(!intent.subjects[0].values.values().any(|value|value.as_str()==Some(question)),"the original instruction is not a keyword");
        let mut extra = intake.clone();
        let extra_question = format!("{question} Limit the maximum budget to 50000.");
        extra.parsed_request.meta.text_sha256 = hex::encode(sha2::Sha256::digest(extra_question.as_bytes()));
        extra.parsed_request.subjects[0].identifiers.insert("query".into(),extra_question.clone());
        extra.parsed_request.subjects[0].identifiers.insert("maximum_budget".into(),"50000".into());
        assert!(single_retrieval_reply(&extra,&catalog,&extra_question,&extra_question,&Default::default()).is_none(),"unmapped filters cannot disappear");
        let mut foreign = intake;
        let foreign_question = question.replace("Netherlands","Germany");
        foreign.parsed_request.meta.text_sha256 = hex::encode(sha2::Sha256::digest(foreign_question.as_bytes()));
        foreign.parsed_request.subjects[0].identifiers.insert("query".into(),foreign_question.clone());
        foreign.parsed_request.subjects[0].identifiers.insert("country".into(),"Germany".into());
        assert!(single_retrieval_reply(&foreign,&catalog,&foreign_question,&foreign_question,&Default::default()).is_none(),"fixed Dutch coverage cannot satisfy another country");
    }

    #[test]
    fn reviewed_semantic_aliases_prove_existing_fields_without_expanding_scope() {
        for (raw, capability, alias, absent) in [
            (include_str!("../../tests/fixtures/overheid-openkvk/catalog.json"), "company_registry.lookup", "registered_details", vec!["funding_rounds", "validity", "private_notes"]),
            (include_str!("../../tests/fixtures/tenderned-browse.json"), "procurement.nl-public-contracts", "matching_tenders", vec!["open_tenders", "awarded_contracts", "contract_pdf", "deadline"]),
            (include_str!("../../tests/fixtures/profession-output-coverage/catalog.json"), "vat.eu.validate", "validity", vec!["tax_debts", "expiry_date", "credit_rating"]),
        ] {
            let before = overlay(raw, "before");
            let after = overlay(raw, "after");
            let old = before.services.iter().find(|service| service.contract.capability_slug == capability).unwrap();
            let new = after.services.iter().find(|service| service.contract.capability_slug == capability).unwrap();
            assert!(!output_covered(alias, old, &before, false), "{capability}: alias requires metadata");
            assert!(output_covered(alias, new, &after, false), "{capability}: {alias}");
            for missing in absent { assert!(!output_covered(missing, new, &after, false), "{capability}: {missing}"); }
            if let Some(payload) = old.sample_payload.as_ref() {
                assert_eq!(old.contract.extract(payload, &before.vocabulary).unwrap(), new.contract.extract(payload, &after.vocabulary).unwrap(), "aliases do not change source extraction");
            }
        }
    }

    #[test]
    fn reviewed_schema_annotations_leave_validation_unchanged() {
        fn without_comments(value: &Value) -> Value {
            match value {
                Value::Object(fields) => Value::Object(fields.iter().filter(|(key, _)| key.as_str() != "$comment")
                    .map(|(key, value)| (key.clone(), without_comments(value))).collect()),
                Value::Array(values) => Value::Array(values.iter().map(without_comments).collect()),
                _ => value.clone(),
            }
        }
        let reviewed = reviewed();
        for before in reviewed["before"].as_array().unwrap() {
            let after = reviewed["after"].as_array().unwrap().iter().find(|after| after["slug"] == before["slug"]).unwrap();
            catalog_core::facts::schema::check_schema(&after["value_schema"]).unwrap();
            assert_eq!(without_comments(&before["value_schema"]), without_comments(&after["value_schema"]), "{}", before["slug"]);
        }
    }
}
