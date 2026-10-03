//! Each discovered retrieval must be covered by a matched, subject-specific
//! goal. Shared goals require explicit output-contract evidence; dependencies
//! belong to the resolver and answer instructions do not require a purchase.
use super::{explicit_request::request_match, Intent};
use crate::{capability_intake::Intake, catalog::Catalog};
use anyhow::ensure;
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub(super) fn prepare(
    intent: &mut Intent,
    intake: &Intake,
    catalog: &Catalog,
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
            // Sharing a purchase between separate requirements still requires
            // proof of each requested field from that one contract.
            if owners.iter().filter(|owners| owners.contains(&goal)).count() > 1 {
                ensure!(!services[goal].is_empty() && services[goal].iter().all(|service| outputs_cover(cap, service, catalog)),
                    "every parsed subject needs its requested retrieval");
            }
            useful.insert(goal);
        } else {
            let outputs: Vec<_> = cap.outputs.iter().filter(|output| !output_metadata(output)).collect();
            ensure!(!outputs.is_empty(), "every parsed subject needs its requested retrieval");
            for output in outputs {
                let proving: Vec<_> = selected.iter().copied().filter(|goal| !services[*goal].is_empty()
                    && services[*goal].iter().all(|service| output_covered(output, service, catalog, false))).collect();
                ensure!(!proving.is_empty(), "every parsed subject needs its requested retrieval");
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
        .map(|(_, cap)| cap.name.as_str())
        .collect();
    if missing.is_empty() {
        return None;
    }
    let names = missing.join("; ");
    let message = if intake.parsed_request.language == "nl" {
        format!("Voor deze onderdelen is geen passende uitvoerbare bron gevonden: {names}. De volledige aanvraag krijgt daarom nog geen prijs. Verduidelijk deze onderdelen of pas de vraag aan; de overige onderdelen blijven bewaard.")
    } else {
        format!("No matching executable source was found for these requirements: {names}. The complete request cannot be priced yet. Clarify these requirements or revise the question; the other requirements are kept.")
    };
    // This is a catalog fact, not a model draft to rewrite into broader claims
    // about a provider's existence. It never quotes a reduced request.
    Some(super::ChatReply {
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

/// A complete model reply was rejected for changing or duplicating the
/// requirements. This is neither evidence of an outage nor of absent sources.
pub(super) fn rejected_reply(intake: &Intake) -> super::ChatReply {
    let names = intake.parsed_request.capabilities.iter()
        .filter(|cap| !answer_instruction(cap)).map(|cap| cap.name.as_str())
        .collect::<Vec<_>>().join("; ");
    let message = if intake.parsed_request.language == "nl" {
        format!("Ik kon nog geen uitvoerbaar plan maken dat deze onderdelen en de opgegeven gegevens correct behoudt: {names}. Er is geen prijsvoorstel gemaakt en niets uitgevoerd. De volledige vraag blijft bewaard; verduidelijk de gewenste gegevens of probeer de planning opnieuw.")
    } else {
        format!("I could not yet build an executable plan that correctly preserves these requirements and the supplied inputs: {names}. No price proposal was made and nothing was executed. The complete question is kept; clarify the required data or try planning again.")
    };
    super::ChatReply {message, discover:false, intent:None, decline_reason:None}
}

/// Recover one concrete retrieval from the already displayed, ranked choices.
/// Model mistakes never supply values or scope to this path. Every requested
/// field, literal binding and exact resolved dependency chain must still pass.
pub(super) fn single_retrieval_reply(intake:&Intake,catalog:&Catalog,question:&str,literals:&str,task:&crate::state::Task)->Option<super::ChatReply> {
    let retrievals:Vec<_>=intake.parsed_request.capabilities.iter().enumerate().filter(|(_,cap)|!answer_instruction(cap)).collect();
    if retrievals.len()!=1 {return None;}
    let (index,cap)=retrievals[0];
    if cap.outputs.is_empty() || cap.outputs.iter().all(|output|output_metadata(output)) {return None;}
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
        let message=if intake.parsed_request.language=="nl" {"Ik controleer de gevonden bron en toon de totale prijs voordat er iets wordt gekocht."}
            else {"I will check the matched source and show the total price before anything is bought."};
        return Some(super::ChatReply {message:message.into(),discover:true,intent:Some(intent),decline_reason:Some("none".into())});
    }
    None
}

/// A complete CBS comparison needs the reviewed monthly observations and all
/// four codebooks. Keep normal discovery, binding, scope and coverage checks.
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
        Ok(intent) => super::ChatReply { message: if canonical.language == "nl" {"Ik vergelijk de absolute aantallen per sector voor de meest recente beschikbare maand, met de CBS-publicatiestatus en het landelijke totaal apart. Bekijk eerst de totale prijs."} else {"I will compare absolute counts by sector for the latest available month, preserving the CBS publication status and keeping the national total separate. Review the total price first."}.into(),
            discover:true, intent:Some(intent), decline_reason:Some("none".into()) },
        Err(_) => super::ChatReply { message: if canonical.language == "nl" {"De benodigde CBS-bronnen dekken deze vergelijking nog niet volledig. Er wordt geen onvolledig vergelijkingsplan geprijsd."} else {"The required CBS sources do not yet cover this comparison completely. An incomplete comparison will not be priced."}.into(),
            discover:false, intent:None, decline_reason:Some("unsupported".into()) },
    })
}

/// For a whole, literal broad-information request, the selected contracts and
/// their dependency graph contain everything needed to price it. Enumerate a
/// small set of already-discovered choices; every normal binding, scope and
/// output-coverage check still applies. This never purchases a source.
pub(super) fn broad_information_reply(intake: &Intake, catalog: &Catalog, question: &str, task: &crate::state::Task) -> Option<super::ChatReply> {
    let canonical = crate::company_request::executable_intake(question, intake)
        .or_else(|| crate::profession_request::executable_intake(question, intake))
        .or_else(|| crate::capability_intake::broad_information::parse(question, "literal"))?;
    let profession = crate::profession_request::recognize(question);
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
                answer_operations: if crate::profession_request::recognize(question).is_some() { vec!["Cite the retrieved sources and preserve their scope, observation dates and missing fields.".into()] } else { vec![if canonical.subjects[0].kind == "company" {
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
            let message = if crate::profession_request::recognize(question).is_some() {
                if canonical.language == "nl" { "Ik haal de gevraagde gegevens uit de gevonden bronnen en toon eerst de totale prijs." }
                else { "I will retrieve the requested data from the matched sources and show the total price first." }
            } else if canonical.subjects[0].kind == "company" { "I will retrieve the registered details and available annual financial figures, preserving their reported year and scope. Review the total price first." }
                else if canonical.language == "nl" {"Ik zoek de beschikbare gegevens bij dit adres en toon eerst de totale prijs."}
                else {"I will retrieve the available records for this address and show the total price first."};
            return Some(super::ChatReply { message: message.into(), discover: true, intent: Some(intent), decline_reason: Some("none".into()) });
        }
    }
    profession.map(|_| rejected_reply(intake))
}

fn outputs_cover(
    cap: &crate::capability_intake::Capability,
    service: &crate::catalog::Service,
    catalog: &Catalog,
) -> bool {
    !cap.outputs.is_empty() && cap.outputs.iter().all(|output|output_metadata(output) || output_covered(output,service,catalog,true))
}

fn output_metadata(output:&str)->bool {
    matches!(output,"source" | "source_reference" | "source_references" | "source_citations"
        | "missing_fields" | "search_coverage" | "coverage_description")
}

fn output_covered(output:&str,service:&crate::catalog::Service,catalog:&Catalog,required_only:bool)->bool {
    if output=="source_link" {
        let source=service.result_source();
        return [source.get("url"),source.get("usage_notice").and_then(|notice|notice.get("url"))].into_iter().flatten()
            .filter_map(Value::as_str).any(|link|reqwest::Url::parse(link).is_ok_and(|url|
                matches!(url.scheme(),"http"|"https") && url.host_str().is_some() && url.username().is_empty() && url.password().is_none()));
    }
    // Exact normalized concept suffixes and declared schema fields are
    // evidence. Descriptions and opaque records are not. Sharing a goal uses
    // the stronger required-only proof; quoting allows declared optional data.
    let parts = |value: &str| {
        value
            .to_lowercase()
            .split(|ch: char| !ch.is_alphanumeric())
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
            let requested = parts(output);
            !requested.is_empty()
                && service.contract.output_map.produces.keys().any(|fact| {
                    (!required_only || !service.optional_outputs.contains(fact)) && (parts(fact).ends_with(&requested)
                        || catalog.vocabulary.get(fact).is_some_and(|vocabulary|
                            schema_covers(&vocabulary.value_schema,output,required_only)))
                })
}

/// Schema properties support retrieving a field even if an individual row
/// lacks it. Required-only proof is used when collapsing several checks.
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
    names.iter().any(|name| *name==output || *name==singular || properties.and_then(|properties|properties.get(*name))
            .is_some_and(|child|property_alias(child,output) || property_alias(child,singular) || schema_covers(child,output,required_only)))
}

// A reviewed, property-local annotation in an existing JSON Schema keyword.
// It does not change value validation or extraction in either executor.
fn property_alias(property:&Value,output:&str)->bool {
    let Some(comment)=property.get("$comment").and_then(Value::as_str).filter(|text|text.len()<=2048) else{return false};
    let Ok(Value::Object(object))=serde_json::from_str::<Value>(comment) else{return false};
    if object.len()!=1 {return false;}
    let Some(aliases)=object.get("apiosk_output_aliases_v1").and_then(Value::as_array).filter(|values|!values.is_empty()&&values.len()<=32) else{return false};
    let mut unique=BTreeSet::new();
    if !aliases.iter().all(|alias|alias.as_str().is_some_and(|name|!name.is_empty()&&name.len()<=100
        && name.bytes().all(|b|b.is_ascii_lowercase()||b.is_ascii_digit()||b==b'_') && unique.insert(name))) {return false;}
    unique.contains(output)
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
pub(crate) fn uncovered_outputs(task:&crate::state::Task,catalog:&Catalog,intent:&Intent,plan:&crate::resolver::Plan)->Vec<String> {
    let Some(intake)=task.capability_intake.as_ref() else {return vec![]};
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
            let proven=contributing.iter().any(|index|output_covered(output,&plan.steps[*index].service,catalog,false));
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

    #[tokio::test]
    async fn bare_kvk_check_quotes_one_exact_profile_and_keeps_explicit_fields_required() {
        let question = "87560488 check dit kvk nummer";
        let catalog = Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/workflows/catalog.json")).unwrap());
        let parser = super::super::Parser::new(reqwest::Client::new(), "http://127.0.0.1:1".into(), String::new(), "fixture".into());
        let intake = parser.ask_intake(question, &Default::default(), &catalog, None).await.unwrap();
        let reply = parser.chat_from_capabilities(question, None, &Default::default(), &catalog, &intake).await.unwrap();
        assert!(reply.discover, "{}", reply.message);
        assert_eq!(parser.calls.load(std::sync::atomic::Ordering::Relaxed), 0);
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
        let parsed_request = crate::capability_intake::normalize("{}", question, "fixture").unwrap();
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
    async fn profession_questions_bind_real_source_inputs_without_a_model_or_partial_scope() {
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
            let parser = super::super::Parser::new(reqwest::Client::new(), "http://127.0.0.1:1".into(), "".into(), "fixture".into());
            let intake = parser.ask_intake(question, &Default::default(), &catalog, None).await.unwrap();
            let reply = parser.chat_from_capabilities(question, None, &Default::default(), &catalog, &intake).await.unwrap();
            assert!(reply.discover, "{question}: {}", reply.message);
            if question.starts_with("Get ") || question.starts_with("Validate ") {
                assert_eq!(intake.parsed_request.language, "en");
                assert_eq!(reply.message, "I will retrieve the requested data from the matched sources and show the total price first.");
            }
            assert_eq!(parser.calls.load(std::sync::atomic::Ordering::Relaxed), 0);
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
            let parser = super::super::Parser::new(reqwest::Client::new(), "http://127.0.0.1:1".into(), "".into(), "fixture".into());
            let intake = parser.ask_intake(question, &Default::default(), &catalog, None).await.unwrap();
            let mut intent = parser.chat_from_capabilities(question, None, &Default::default(), &catalog, &intake).await.unwrap().intent.unwrap();
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
            assert_eq!(parser.calls.load(std::sync::atomic::Ordering::Relaxed), 0);
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
            let parser = super::super::Parser::new(reqwest::Client::new(), "http://127.0.0.1:1".into(), "".into(), "fixture".into());
            let intake = parser.ask_intake(question, &Default::default(), &catalog, None).await.unwrap();
            let reply = parser.chat_from_capabilities(question, None, &Default::default(), &catalog, &intake).await.unwrap();
            assert!(!reply.discover && reply.intent.is_none(), "{changed} cannot reach a purchase");
            assert_eq!(parser.calls.load(std::sync::atomic::Ordering::Relaxed), 0, "{changed}: a model may not bypass reviewed operation scope");
        }
    }

    #[tokio::test]
    async fn broad_location_request_prices_the_real_dependency_chain_without_a_planning_model() {
        for question in ["Find building and neighbourhood information for Damrak 1, Amsterdam.",
            "Geef gebouw- en buurtinformatie voor Grote Markt 1, Groningen."] {
            let (_, catalog) = broad_location_fixture(question);
            let parser = super::super::Parser::new(reqwest::Client::new(), "http://127.0.0.1:1".into(), "".into(), "fixture".into());
            let intake = parser.ask_intake(question, &Default::default(), &catalog, None).await.unwrap();
            let reply = parser.chat_from_capabilities(question, None, &Default::default(), &catalog, &intake).await.unwrap();
            assert!(reply.discover, "{}", reply.message);
            assert_eq!(parser.calls.load(std::sync::atomic::Ordering::Relaxed), 0);
            let mut intent = reply.intent.unwrap();
            assert_eq!(intent.goals.len(), 2);
            assert_eq!(intent.subjects[0].values["geo.input.q"], intake.parsed_request.subjects[0].label);
            let mut task = crate::state::Task::default(); task.capability_intake = Some(intake);
            crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
            let crate::resolver::Resolution::Plan(plan) = crate::resolver::resolve(&catalog, &task, &intent) else {panic!("the catalog must resolve the supplied address")};
            assert_eq!(plan.steps.iter().map(|step| step.service.contract.capability_slug.as_str()).collect::<BTreeSet<_>>(),
                BTreeSet::from(["location.property-search", "building.bag-unit", "building.bag-building", "location.bag-address-context", "area.cbs-neighbourhood-2025"]));
            assert!(uncovered_outputs(&task, &catalog, &intent, &plan).is_empty());
            assert!(plan.steps.iter().all(|step| Some(step.entity_ref) == intent.subjects[0].entity_ref));
            assert!(plan.steps.iter().all(|step| step.service.api_slug == "pdok"));
        }
    }

    #[test]
    fn aggregate_retrieval_allows_only_contributing_same_subject_goals() {
        let question = "Find building and neighbourhood information for Damrak 1, Amsterdam.";
        let (mut intake, catalog) = broad_location_fixture(question);
        let original = broad_information_reply(&intake, &catalog, question, &Default::default()).unwrap().intent.unwrap();
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
    fn empty_subject_currency_recovery_requires_the_explicit_fixed_base_pair() {
        let (_,mut intake,_)=saved_vies();
        let catalog=Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/frankfurter/catalog.json")).unwrap());
        let service=&catalog.services[0];
        intake.parsed_request.subjects.clear();
        let cap=&mut intake.parsed_request.capabilities[0];
        cap.subject_id=None;cap.source_hint=Some(service.api_slug.clone());
        cap.outputs=vec!["rate".into(),"date".into(),"source_link".into()];
        intake.matches[0].operations=BTreeSet::from([service.contract.capability_slug.clone()]);
        intake.matches[0].apiosk.truncate(1);
        let candidate=&mut intake.matches[0].apiosk[0];
        candidate.capability=Some(service.contract.capability_slug.clone());
        candidate.source_slug=Some(service.api_slug.clone());candidate.endpoint_id=Some(service.endpoint_id.to_string());
        let question="Geef de laatste ECB referentiekoers van EUR naar USD met datum en bronlink.";
        let intent=single_retrieval_reply(&intake,&catalog,question,question,&Default::default()).unwrap().intent.unwrap();
        assert_eq!(intent.subjects[0].values["fx.ecb.quote"],json!("USD"));
        for question in ["ECB USD naar EUR","ECB BTC naar USD; EUR als voorbeeld","ECB EUR naar USD en GBP","ECB USD"] {
            assert!(single_retrieval_reply(&intake,&catalog,question,question,&Default::default()).is_none(),"{question}");
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
        let mut task=crate::state::Task::default();task.capability_intake=Some(intake);
        let steps=["company.search","company.accounts","company.other-data"].into_iter().enumerate().map(|(index,cap)|Step{
            service:catalog.services.iter().find(|s|s.contract.capability_slug==cap).unwrap().clone(),entity_ref:entity,
            goal_index:if index==1 {0}else{1},browser_fallback:None}).collect();
        let mut plan=Plan{quote_ref:uuid::Uuid::new_v4(),steps,cap_atomic:1,expires_at:chrono::Utc::now(),hash:"test".into(),fee_bps:1500,workflow_slug:None};
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
        assert!(!schema_covers(&officer,"officer_names",true),"optional fields still cannot justify shared-goal collapse");
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
                let mut task=crate::state::Task::default();
                task.capability_intake=serde_json::from_value(raw["intake"].clone()).unwrap();
                let mut steps=vec![];
                for entry in quote["steps"].as_array().unwrap() {
                    let service=catalog.services.iter().find(|s|entry["contract_id"].as_str()==Some(&s.id.to_string())
                        || (entry["endpoint_id"].as_str()==Some(&s.endpoint_id.to_string()) && entry["capability"]==s.contract.capability_slug)).expect("exact saved contract").clone();
                    let goal_index=entry["goal_index"].as_u64().map(|n|n as usize).unwrap_or_else(||intent.goals.iter().position(|goal|goal.capability==service.contract.capability_slug).unwrap_or(0));
                    let entity_ref=intent.subjects[intent.goals[goal_index].subject].entity_ref.unwrap();
                    steps.push(crate::resolver::Step{service,entity_ref,goal_index,browser_fallback:None});
                }
                let plan=crate::resolver::Plan{quote_ref:uuid::Uuid::new_v4(),steps,cap_atomic:0,expires_at:chrono::Utc::now(),hash:"offline audit".into(),fee_bps:1500,workflow_slug:None};
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
}
