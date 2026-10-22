//! A signed free-search choice is a required root, never a price-ranking hint.
//! The ordinary App include planner is deliberately not involved here.
use super::{compile_scoped, input_context, metadata, refresh_discovery, Coverage, Gap, Prepared};
use crate::{
    ask_v2::{input_values, source_options::SourceOption, source_selection::SelectedSource},
    capability_intake::{Intake, Match, Subject}, catalog::Catalog,
    parser::{plan_coverage, source_grounding, Intent}, state::Task,
};
use anyhow::{ensure, Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

fn literal(value: &Value) -> Option<String> {
    match value { Value::String(value) => Some(value.clone()), Value::Null => None, _ => Some(value.to_string()) }
}

pub(super) struct ExplicitValues {
    pub literals: String,
    pub values: BTreeMap<(String, String), Value>,
}

// Preserve every unrelated original criterion. Replace only aliases that are
// demonstrably bound to this exact reviewed field, then prove the new binding.
fn overlay(subject: &mut Subject, field: &str, schema: &Value, value: &Value) -> Result<()> {
    let text = literal(value).context("Source input must be a declared non-null value")?;
    let keys: Vec<_> = subject.identifiers.keys().filter(|key| {
        let mut isolated = subject.clone(); isolated.label.clear();
        isolated.identifiers.retain(|candidate, _| candidate == *key);
        input_values::documented_literal(field, schema, &isolated)
            .and_then(|value| literal(&value)).is_some_and(|value| !value.trim().is_empty())
    }).cloned().collect();
    for key in keys { subject.identifiers.insert(key, text.clone()); }
    subject.identifiers.insert(field.into(), text.clone());
    let binds = |subject: &Subject| input_values::documented_literal(field, schema, subject)
        .and_then(|bound| literal(&bound)).as_deref() == Some(text.as_str());
    if !binds(subject) {
        let aliases = schema["$comment"].as_str().and_then(|comment| serde_json::from_str::<Value>(comment).ok())
            .and_then(|metadata| metadata["apiosk_input_aliases_v1"].as_array().cloned()).unwrap_or_default();
        for alias in aliases.iter().filter_map(Value::as_str).filter(|alias| input_values::reviewed_input_alias(schema, alias)) {
            let mut probe = subject.clone(); probe.identifiers.insert(alias.into(), text.clone());
            if binds(&probe) { *subject = probe; break; }
        }
    }
    ensure!(binds(subject), "The entered source value cannot be bound unambiguously");
    Ok(())
}

// An explicitly selected source can contribute its own records even
// when it cannot answer the requested profile. Only its documented result
// identity may become a verification target; unrelated filters remain binding.
fn identity_verified_contribution(
    question: &str, task: &Task, catalog: &Catalog, part: &Intake,
    service: &crate::catalog::Service, explicit: &ExplicitValues,
) -> Option<Prepared> {
    let identity = service.contract.identifies.as_ref()?;
    if !service.contract.output_map.produces.contains_key(identity) { return None; }
    let identity_schema = &catalog.vocabulary.get(identity)?.value_schema;
    let subject = part.parsed_request.subjects.first()?;
    let target = input_values::documented_literal(identity, identity_schema, subject)?;
    catalog_core::facts::schema::validate(identity_schema, &target).ok()?;
    let target_text = literal(&target).filter(|text| !text.trim().is_empty())?;
    let fields: Vec<_> = service.contract.requires.iter().filter(|input| service.contract.input_map.contains_key(&input.fact_type))
        .filter_map(|input| catalog.vocabulary.get(&input.fact_type).map(|fact| (input.fact_type.as_str(), &fact.value_schema))).collect();
    let binds = |field: &str, schema: &Value, isolated: &Subject| input_values::documented_literal(field, schema, isolated)
        .and_then(|value| literal(&value)).is_some_and(|value| !value.trim().is_empty());
    let mut projected = subject.clone();
    let mut admitted = BTreeSet::new();
    let mut identity_keys = Vec::new();
    for (key, _) in &subject.identifiers {
        let mut isolated = subject.clone(); isolated.label.clear(); isolated.identifiers.retain(|other, _| other == key);
        if fields.iter().any(|(field, schema)| binds(field, schema, &isolated)) { admitted.insert(key.clone()); }
        else if input_values::documented_literal(identity, identity_schema, &isolated).as_ref() == Some(&target) {
            identity_keys.push(key.clone()); admitted.insert(key.clone());
        } else { return None; }
    }
    if identity_keys.is_empty() { return None; }
    // Inputs mentioned independently of supplied identifiers are still scope.
    // An absent or unknown input cannot be discarded by this interpretation.
    if part.parsed_request.capabilities.iter().any(|cap| !cap.missing.is_empty() || cap.inputs.iter().any(|input|
        !admitted.contains(input) && !fields.iter().any(|(field, _)| input == field) && input != identity)) { return None; }
    for key in identity_keys { projected.identifiers.remove(&key); }
    let bound_fields: Vec<_> = fields.iter().filter(|(field, schema)| binds(field, schema, &projected)).map(|(field, _)| (*field).to_owned()).collect();
    if bound_fields.is_empty() { return None; }
    let mut native = part.clone();
    // Preserve collection ordering constraints before replacing output aliases.
    let notices = super::collections::constrain_recency(question, &mut native, catalog)?;
    native.parsed_request.subjects = vec![projected];
    let cap = native.parsed_request.capabilities.first_mut()?;
    cap.slug = service.contract.capability_slug.clone();
    cap.inputs = bound_fields;
    cap.outputs = service.contract.output_map.produces.keys().cloned().collect();
    cap.missing.clear();
    native.matches[0].slug = cap.slug.clone();
    let mut prepared = compile_scoped(question, task, catalog, &native, Some(explicit))?;
    prepared.coverage.limitations.extend(notices);
    prepared.coverage.limitations.push(format!(
        "{} returns source records. Verify the returned {} against {} before treating a record as the requested subject. This source does not by itself verify the originally requested profile or investigation.",
        service.name, identity, target_text));
    Some(prepared)
}

pub(crate) fn compile_selected(
    question: &str, task: &Task, catalog: &Catalog, original: &Intake,
    selections: &[SelectedSource], options: &[SourceOption],
) -> Result<Prepared> {
    ensure!(!selections.is_empty() && selections.len() <= crate::protocol::MAX_STEPS, "Select between 1 and 10 sources");
    ensure!(original.catalog_version == catalog.version, "Refresh the source search before reviewing these choices");
    let mut refreshed = original.clone(); refresh_discovery(&mut refreshed, catalog);
    let mut scoped = input_context(&refreshed, catalog);
    let mut seen = BTreeSet::new();
    let mut entered = BTreeMap::new();
    let mut selected_options = Vec::new();
    let mut characters = 0usize;
    for selected in selections {
        ensure!(seen.insert((selected.endpoint_id, &selected.capability, &selected.subject_id)), "A source was selected twice");
        let option = options.iter().find(|option| option.subject_id == selected.subject_id
            && option.source.endpoint_id.as_deref() == Some(selected.endpoint_id.to_string().as_str())
            && option.source.capability.as_deref() == Some(selected.capability.as_str()))
            .context("Selected source was not offered for this subject")?;
        ensure!(option.source.origin == "apiosk" && option.source.availability == "supported"
            && option.requirement.subject_id.as_deref() == Some(selected.subject_id.as_str()), "Selected source is not executable");
        let service = catalog.services.iter().find(|service| service.endpoint_id == selected.endpoint_id
            && service.contract.capability_slug == selected.capability
            && option.source.source_slug.as_deref() == Some(service.api_slug.as_str()) && !service.unverified)
            .context("Selected source is no longer executable")?;
        let subject = scoped.parsed_request.subjects.iter_mut().find(|subject| subject.id == selected.subject_id)
            .context("Selected source belongs to an unknown subject")?;
        ensure!(selected.input.len() <= 16, "Too many values for one source");
        for (field, value) in &selected.input {
            ensure!(service.contract.input_map.contains_key(field) && service.contract.requires.iter().any(|input| input.fact_type == *field), "Selected source does not accept this input");
            let text = literal(value).filter(|text| !text.trim().is_empty() && text.chars().count() <= 500)
                .context("Source input must be a short literal value")?;
            characters += text.chars().count(); ensure!(characters <= 4000, "Selected source values are too long");
            let schema = &catalog.vocabulary.get(field).context("Unknown source input")?.value_schema;
            catalog_core::facts::schema::validate(schema, value)?;
            ensure!(entered.insert((selected.subject_id.clone(), field.clone()), value.clone()).is_none_or(|previous| previous == *value),
                "Selected sources have conflicting values for one subject");
            overlay(subject, field, schema, value)?;
        }
        selected_options.push((selected, option, service));
    }
    // Pin all original matching alternatives to the person's choices. Empty
    // matches remain empty; they become disclosed gaps rather than new roots.
    for (index, cap) in scoped.parsed_request.capabilities.iter().enumerate() {
        let occurrence = scoped.parsed_request.capabilities[..index].iter().filter(|prior| prior.slug == cap.slug).count();
        let found = scoped.matches.iter_mut().filter(|found| found.slug == cap.slug).nth(occurrence)
            .context("Source search scope is inconsistent")?;
        found.apiosk.retain(|candidate| selections.iter().any(|choice| cap.subject_id.as_deref() == Some(choice.subject_id.as_str())
            && candidate.endpoint_id.as_deref() == Some(choice.endpoint_id.to_string().as_str())
            && candidate.capability.as_deref() == Some(choice.capability.as_str())));
        found.operations = found.apiosk.iter().filter_map(|candidate| candidate.capability.clone()).collect();
    }
    let explicit = ExplicitValues { literals: entered.values().filter_map(literal).collect::<Vec<_>>().join("\n"), values: entered };
    let original_proof = compile_scoped(question, task, catalog, &scoped, Some(&explicit));
    let mut coverage = original_proof.as_ref().map(|prepared| prepared.coverage.clone()).unwrap_or_else(|| Coverage {
        original_intake: original.clone(), limitations: vec![],
        gaps: scoped.parsed_request.capabilities.iter().filter(|cap| !plan_coverage::answer_instruction(cap)).flat_map(|cap| {
            let subject = scoped.parsed_request.subjects.iter().find(|subject| cap.subject_id.as_deref() == Some(subject.id.as_str())).map(|subject| subject.label.clone());
            cap.outputs.iter().filter(|output| !metadata(output)).map(move |output| Gap { subject: subject.clone(), output: output.clone() })
        }).collect(),
    });
    coverage.original_intake = original.clone();
    let mut combined = scoped.clone(); combined.parsed_request.capabilities.clear(); combined.matches.clear(); combined.parsed_request.subjects.clear();
    let mut intent = Intent { subjects: vec![], goals: vec![], answer_operations: scoped.parsed_request.deliverable.operations.clone(), unresolved_references: vec![] };
    let mut subject_indices = BTreeMap::<String, usize>::new();
    for (selected, option, service) in selected_options {
        let mut part = scoped.clone();
        let mut subject = scoped.parsed_request.subjects.iter().find(|subject| subject.id == selected.subject_id).unwrap().clone();
        let original_subject = original.parsed_request.subjects.iter().find(|subject| subject.id == selected.subject_id).unwrap();
        // An explicitly entered input for another chosen root is proved by that
        // root. Do not demand its provider identifier from this isolated call.
        // Every original criterion stays here; the final union is checked again.
        subject.identifiers.retain(|field, _| original_subject.identifiers.contains_key(field)
            || selected.input.contains_key(field) || !explicit.values.contains_key(&(selected.subject_id.clone(), field.clone())));
        part.parsed_request.subjects = vec![subject.clone()];
        let mut requirement = option.requirement.clone(); requirement.source_hint = Some(service.api_slug.clone());
        for field in selected.input.keys() { if !requirement.inputs.contains(field) { requirement.inputs.push(field.clone()); } requirement.missing.retain(|missing| missing != field); }
        part.matches = vec![Match { slug: requirement.slug.clone(), query: requirement.description.clone(), operations: BTreeSet::from([selected.capability.clone()]),
            apiosk: vec![option.source.clone()], coinbase: vec![], coinbase_status: "complete".into() }];
        part.parsed_request.capabilities = vec![requirement]; part.selected_capabilities = BTreeSet::from([selected.capability.clone()]);
        // A native output name must not bypass the selected collection's
        // publication-window proof when interpretation omitted its criteria.
        if let Some(feedback) = super::selected_collection::feedback(question, catalog, &part, service) { anyhow::bail!(feedback); }
        let direct = compile_scoped(question, task, catalog, &part, Some(&explicit));
        let directly_proven = direct.is_some();
        let prepared = direct.or_else(|| identity_verified_contribution(question, task, catalog, &part, service, &explicit))
            .or_else(|| super::selected_collection::contribution(question, task, catalog, &part, service, &explicit)).with_context(|| {
            if let Some(feedback) = super::selected_collection::feedback(question, catalog, &part, service) { return feedback; }
            let missing: Vec<_> = service.contract.requires.iter().filter(|input| !input.optional).filter(|input|
                catalog.vocabulary.get(&input.fact_type).is_none_or(|fact| input_values::documented_literal(&input.fact_type, &fact.value_schema, &subject).is_none()))
                .map(|input| input.fact_type.as_str()).collect();
            if missing.is_empty() { format!("{} cannot preserve the requested filters and output scope. Choose another source or revise the inputs.", service.name) }
            else { format!("Enter {} for {} before reviewing the selected sources.", missing.join(", "), service.name) }
        })?;
        if directly_proven && original.parsed_request.subjects.iter().filter(|candidate| candidate.label == subject.label).count() == 1 {
            // Original coverage can be proved by an individual selected root
            // even when another root adds its own provider-specific input.
            coverage.gaps.retain(|gap| !(gap.subject.as_deref() == Some(subject.label.as_str())
                && option.requirement.outputs.contains(&gap.output)
                && original.parsed_request.capabilities.iter().any(|cap| cap.slug == option.requirement.slug && cap.subject_id == option.requirement.subject_id)
                && !prepared.coverage.gaps.iter().any(|missing| missing.output == gap.output)));
        }
        let proven_subject = prepared.intake.parsed_request.subjects[0].clone();
        let part_intent = prepared.reply.intent.context("Selected source produced no executable request")?;
        ensure!(part_intent.subjects.len() == 1 && part_intent.goals.iter().any(|goal| goal.capability == selected.capability
            && goal.constraints.get("endpoint_id").and_then(Value::as_str) == Some(selected.endpoint_id.to_string().as_str())), "Selected source was replaced by another operation");
        let incoming = part_intent.subjects.into_iter().next().unwrap();
        ensure!(selected.input.iter().all(|(field, value)| incoming.values.get(field) == Some(value)),
            "The selected source did not preserve every entered value");
        let index = if let Some(index) = subject_indices.get(&selected.subject_id).copied() {
            let existing: &mut crate::parser::Subject = &mut intent.subjects[index];
            for (field, value) in incoming.values { ensure!(existing.values.get(&field).is_none_or(|previous| previous == &value), "Selected sources have conflicting input bindings"); existing.values.insert(field, value); }
            let parsed = &mut combined.parsed_request.subjects[index];
            for (field, value) in proven_subject.identifiers { ensure!(parsed.identifiers.get(&field).is_none_or(|previous| previous == &value), "Selected sources have conflicting input criteria"); parsed.identifiers.insert(field, value); }
            index
        } else {
            let index = intent.subjects.len(); subject_indices.insert(selected.subject_id.clone(), index);
            intent.subjects.push(incoming);
            combined.parsed_request.subjects.push(proven_subject); index
        };
        for mut goal in part_intent.goals { goal.subject = index; intent.goals.push(goal); }
        combined.parsed_request.capabilities.extend(prepared.intake.parsed_request.capabilities);
        combined.matches.extend(prepared.intake.matches);
        for notice in prepared.coverage.limitations { if !coverage.limitations.contains(&notice) { coverage.limitations.push(notice); } }
    }
    combined.selected_capabilities = combined.matches.iter().flat_map(|found| found.operations.iter().cloned()).collect();
    // Keep the normalized original requirements in the final proof as well.
    // The per-source occurrences above independently require every chosen root.
    if let Some(original_proof) = original_proof {
        combined.parsed_request.capabilities.extend(original_proof.intake.parsed_request.capabilities);
        combined.matches.extend(original_proof.intake.matches);
    }
    intent.answer_operations.extend(coverage.notices());
    intent.answer_operations.push("Cite the actual sources and explicitly identify missing or unverified requested information.".into());
    let services = selections.iter().filter_map(|selected| catalog.services.iter().find(|service| service.endpoint_id == selected.endpoint_id && service.contract.capability_slug == selected.capability)).collect::<Vec<_>>();
    let bindings = source_grounding::compile(&combined, catalog, &services);
    let reply = crate::parser::ChatReply { message: "Review one total price for the selected sources before approving.".into(), discover: true, intent: Some(intent), output_bindings: bindings, decline_reason: None };
    ensure!(super::super::contract_planner::ready(question, &reply, task, catalog, &combined), "The selected sources cannot preserve all supplied input criteria together");
    let mut preview = task.clone(); preview.capability_intake = Some(combined.clone());
    preview.output_grounding = source_grounding::Grounding::new(&combined, catalog, reply.output_bindings.clone());
    preview.record_turn(question, None);
    let mut bound = reply.intent.clone().unwrap(); crate::engine::bind_subjects(&mut preview, catalog, &mut bound)?;
    let crate::resolver::Resolution::Plan(plan) = crate::resolver::resolve(catalog, &preview, &bound) else { anyhow::bail!("The selected sources are not ready to price"); };
    let mut physical_calls = BTreeSet::new();
    for selected in selections {
        let entity = bound.subjects[subject_indices[&selected.subject_id]].entity_ref;
        let step = plan.steps.iter().find(|step| step.service.endpoint_id == selected.endpoint_id
            && step.service.contract.capability_slug == selected.capability && Some(step.entity_ref) == entity)
            .context("A selected source was omitted from the combined price")?;
        let input = step.service.inputs(&preview, step.entity_ref, &catalog.vocabulary)?;
        ensure!(physical_calls.insert((selected.endpoint_id, &selected.subject_id, serde_json::to_string(&input)?)),
            "Two selected source options make the same request. Select one of those options before reviewing the total price.");
    }
    Ok(Prepared { reply, intake: combined, coverage })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture(two: bool) -> (Catalog, String, Intake, Vec<SourceOption>) {
        let mut snapshot: crate::catalog::Snapshot = serde_json::from_str(include_str!("../../../tests/fixtures/connected-planning/catalog.json")).unwrap();
        if two {
            let mut alternative = snapshot.services.iter().find(|service| service.contract.capability_slug == "company.registry-profile").unwrap().clone();
            alternative.id = uuid::Uuid::new_v4(); alternative.endpoint_id = uuid::Uuid::new_v4();
            alternative.list_atomic *= 3;
            snapshot.services.push(alternative);
        }
        let catalog = Catalog::build(snapshot); assert!(catalog.rejected.is_empty());
        let saved: Value = serde_json::from_str(include_str!("../../../tests/fixtures/connected-planning/fcca.json")).unwrap();
        let question = saved["question"].as_str().unwrap().to_owned();
        let parsed_request = serde_json::from_value(saved["parsed_request"].clone()).unwrap();
        let matches = crate::ask_v2::search::search(&parsed_request, &catalog);
        let options = crate::ask_v2::source_options::options(&parsed_request, &matches, &catalog);
        let intake = Intake { parsed_request, selected_capabilities: matches.iter().flat_map(|found| found.operations.iter().cloned()).collect(), matches,
            apiosk_sources_searched: 2, catalog_version: catalog.version.clone() };
        (catalog, question, intake, options)
    }

    fn choose(options: &[SourceOption]) -> Vec<SelectedSource> {
        options.iter().filter(|option| option.source.capability.as_deref() == Some("company.registry-profile")).map(|option| SelectedSource {
            endpoint_id: option.source.endpoint_id.as_deref().unwrap().parse().unwrap(), capability: option.source.capability.clone().unwrap(),
            subject_id: option.subject_id.clone(), input: option.input_values.clone(),
        }).collect()
    }

    fn plan(prepared: Prepared, question: &str, catalog: &Catalog) -> (Task, crate::resolver::Plan) {
        let mut task = Task { capability_intake: Some(prepared.intake), ..Task::default() };
        task.record_turn(question, None);
        let mut intent = prepared.reply.intent.unwrap();
        crate::engine::bind_subjects(&mut task, catalog, &mut intent).unwrap();
        let crate::resolver::Resolution::Plan(plan) = crate::resolver::resolve(catalog, &task, &intent) else { panic!("expected priced selected plan") };
        plan.validate_independent_inputs(&task, &catalog.vocabulary).unwrap();
        (task, plan)
    }

    fn live_fixture() -> (Catalog, String, Intake, Vec<SourceOption>) {
        let catalog = Catalog::build(serde_json::from_str(include_str!("../../../tests/fixtures/connected-selection/catalog.json")).unwrap());
        assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
        let saved: Value = serde_json::from_str(include_str!("../../../tests/fixtures/connected-selection/fcca-free.json")).unwrap();
        let parsed_request = serde_json::from_value(saved["parsed_request"].clone()).unwrap();
        let matches: Vec<Match> = serde_json::from_value(saved["matches"].clone()).unwrap();
        let options = crate::ask_v2::source_options::connected_options(&parsed_request, &matches, &catalog);
        let intake = Intake { parsed_request, selected_capabilities: BTreeSet::new(), matches, apiosk_sources_searched: 2, catalog_version: catalog.version.clone() };
        (catalog, saved["question"].as_str().unwrap().to_owned(), intake, options)
    }

    fn live_choice(options: &[SourceOption], endpoint: &str) -> SelectedSource {
        let option = options.iter().find(|option| option.source.endpoint_id.as_deref() == Some(endpoint)).unwrap();
        SelectedSource { endpoint_id:endpoint.parse().unwrap(), capability:option.source.capability.clone().unwrap(), subject_id:option.subject_id.clone(), input:option.input_values.clone() }
    }

    #[test]
    fn actual_fcca_two_sources_keep_both_roots_price_and_original_gaps() {
        let (catalog, question, intake, options) = live_fixture();
        let mut choices = choose(&options);
        choices.push(live_choice(&options, "7e58d830-1e94-41ff-9809-66552e9bc431"));
        let prepared = compile_selected(&question, &Task::default(), &catalog, &intake, &choices, &options).unwrap();
        assert_eq!(prepared.coverage.gaps.iter().map(|gap| gap.output.as_str()).collect::<Vec<_>>(), ["directors", "ownership_structure"]);
        assert!(prepared.coverage.limitations.iter().any(|notice| notice.contains("Verify the returned company_registry.kvknummer against 91251982")));
        assert_eq!(serde_json::to_value(&prepared.coverage.original_intake).unwrap(), serde_json::to_value(&intake).unwrap());
        let (task, plan) = plan(prepared, &question, &catalog);
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.cap_atomic, plan.steps.iter().map(|step| step.service.total()).sum::<i64>());
        let inputs: BTreeMap<_,_> = plan.steps.iter().map(|step| (step.service.endpoint_id.to_string(), step.service.inputs(&task, step.entity_ref, &catalog.vocabulary).unwrap())).collect();
        assert_eq!(inputs["7e58d830-1e94-41ff-9809-66552e9bc431"], json!({"query":"Financial Crime Compliance Associates"}));
        assert_eq!(inputs["b050c654-284a-49e8-a7ca-0bfca565816e"], json!({"kvkNummer":"91251982"}));
        choices.reverse();
        let reversed = compile_selected(&question, &Task::default(), &catalog, &intake, &choices, &options).unwrap();
        assert_eq!(reversed.coverage.gaps.len(), 2);
        let (_, reversed_plan) = self::plan(reversed, &question, &catalog);
        assert_eq!(reversed_plan.cap_atomic, plan.cap_atomic);
        assert_eq!(reversed_plan.steps.len(), 2);
    }

    #[test]
    fn actual_fcca_search_alone_is_an_explicit_identity_check_with_original_gaps() {
        let (catalog, question, intake, options) = live_fixture();
        let choices = vec![live_choice(&options, "7e58d830-1e94-41ff-9809-66552e9bc431")];
        let prepared = compile_selected(&question, &Task::default(), &catalog, &intake, &choices, &options).unwrap();
        assert_eq!(prepared.coverage.gaps.iter().map(|gap| gap.output.as_str()).collect::<Vec<_>>(), ["company_details", "directors", "ownership_structure"]);
        assert!(prepared.coverage.limitations.iter().any(|notice| notice.contains("91251982") && notice.contains("Verify")));
        let (task, plan) = plan(prepared, &question, &catalog);
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].service.endpoint_id, choices[0].endpoint_id);
        assert_eq!(plan.steps[0].service.inputs(&task,plan.steps[0].entity_ref,&catalog.vocabulary).unwrap(), json!({"query":"Financial Crime Compliance Associates"}));
    }

    #[test]
    fn actual_fcca_lookup_requires_its_real_input_and_keeps_identity_verification() {
        let (catalog, question, intake, options) = live_fixture();
        let mut choices = choose(&options);
        let lookup = live_choice(&options, "e7b4647f-f2aa-481e-b613-9ffbf4cad6fd");
        assert!(lookup.input.is_empty());
        choices.push(lookup);
        let error = compile_selected(&question, &Task::default(), &catalog, &intake, &choices, &options).err().unwrap().to_string();
        assert!(error.contains("Enter company_registry.openkvk_slug"), "{error}");
        // An explicitly entered fixture identifier, never inferred from the name.
        choices[1].input.insert("company_registry.openkvk_slug".into(), json!("fixture-source-record-slug"));
        let lookup_id = choices[1].endpoint_id;
        for variant in [choices.clone(), choices.iter().cloned().rev().collect(), vec![choices[1].clone()]] {
            let prepared = compile_selected(&question, &Task::default(), &catalog, &intake, &variant, &options).unwrap();
            assert!(prepared.coverage.limitations.iter().any(|notice| notice.contains("Verify") && notice.contains("91251982")));
            assert_eq!(prepared.coverage.gaps.len(), if variant.len() == 2 { 2 } else { 3 });
            let (task, plan) = plan(prepared, &question, &catalog);
            assert_eq!(plan.steps.len(), variant.len());
            let lookup = plan.steps.iter().find(|step| step.service.endpoint_id == lookup_id).unwrap();
            assert_eq!(lookup.service.inputs(&task, lookup.entity_ref, &catalog.vocabulary).unwrap(), json!({"id":"fixture-source-record-slug"}));
        }
    }

    #[test]
    fn source_identity_verification_cannot_drop_other_filters_or_unknown_inputs() {
        let (catalog, question, intake, options) = live_fixture();
        for filter in ["publication_after", "country", "status"] {
            let mut filtered = intake.clone();
            filtered.parsed_request.subjects[0].identifiers.insert(filter.into(), "explicit-filter".into());
            filtered.parsed_request.capabilities[0].inputs.push(filter.into());
            let mut constrained_options = options.clone();
            for option in &mut constrained_options { if option.requirement.slug == "company.records.fetch" { option.requirement.inputs.push(filter.into()); } }
            for choices in [vec![live_choice(&options,"7e58d830-1e94-41ff-9809-66552e9bc431")], {
                let mut choices=choose(&options); choices.push(live_choice(&options,"7e58d830-1e94-41ff-9809-66552e9bc431")); choices
            }] {
                assert!(compile_selected(&question,&Task::default(),&catalog,&filtered,&choices,&constrained_options).is_err(), "discarded {filter}");
            }
        }
    }

    fn energy_selection_fixture() -> (Catalog, String, Intake, Vec<SourceOption>, SelectedSource) {
        let catalog = Catalog::build(serde_json::from_str(include_str!("../../../tests/fixtures/connected-selection/energy-catalog.json")).unwrap());
        assert!(catalog.rejected.is_empty());
        let saved: Value = serde_json::from_str(include_str!("../../../tests/fixtures/connected-selection/energy-free.json")).unwrap();
        let parsed_request = serde_json::from_value(saved["parsed_request"].clone()).unwrap();
        let matches: Vec<Match> = serde_json::from_value(saved["matches"].clone()).unwrap();
        let options = crate::ask_v2::source_options::connected_options(&parsed_request, &matches, &catalog);
        let choice = options.iter().find(|option| option.source.capability.as_deref() == Some("procurement.nl-public-contracts")).unwrap();
        let choice = SelectedSource { endpoint_id:choice.source.endpoint_id.as_deref().unwrap().parse().unwrap(), capability:choice.source.capability.clone().unwrap(), subject_id:choice.subject_id.clone(), input:choice.input_values.clone() };
        let intake = Intake { parsed_request, matches, selected_capabilities:BTreeSet::new(), catalog_version:catalog.version.clone(),apiosk_sources_searched:1 };
        (catalog, saved["question"].as_str().unwrap().to_owned(), intake, options, choice)
    }

    #[test]
    fn actual_energy_selection_requires_its_criteria_then_prices_the_documented_filtered_page() {
        let (catalog, question, intake, options, mut choice) = energy_selection_fixture();
        assert!(choice.input.is_empty());
        let error = compile_selected(&question,&Task::default(),&catalog,&intake,&[choice.clone()],&options).err().unwrap().to_string();
        assert!(error.contains("company_intel.input.search") && error.contains("AF7"), "{error}");
        let mut native_intake=intake.clone();
        native_intake.parsed_request.capabilities[0].outputs=vec!["tenderned.publication_page".into()];
        let mut native_options=options.clone();
        for option in &mut native_options { option.requirement.outputs=vec!["tenderned.publication_page".into()]; }
        assert!(compile_selected(&question,&Task::default(),&catalog,&native_intake,&[choice.clone()],&native_options).is_err(), "native output bypassed missing criteria");
        choice.input.insert("company_intel.input.search".into(),json!("energie"));
        choice.input.insert("company_intel.input.publicatiedatumpreset".into(),json!("AF7"));
        let prepared = compile_selected(&question,&Task::default(),&catalog,&intake,&[choice],&options).unwrap();
        assert_eq!(prepared.coverage.gaps.iter().map(|gap| gap.output.as_str()).collect::<Vec<_>>(),["contracts"]);
        assert!(prepared.coverage.limitations.iter().any(|notice| notice.contains("not complete coverage of all requested regions") && notice.contains("Netherlands") && notice.contains("EU")));
        assert_eq!(serde_json::to_value(&prepared.coverage.original_intake).unwrap(),serde_json::to_value(&intake).unwrap());
        let (task,plan)=plan(prepared,&question,&catalog);
        assert_eq!(plan.steps.len(),1);
        assert_eq!(plan.cap_atomic,54348);
        assert_eq!(plan.steps[0].service.inputs(&task,plan.steps[0].entity_ref,&catalog.vocabulary).unwrap(),json!({"search":"energie","publicatieDatumPreset":"AF7","page":0,"size":20,"sort":"publicatieDatum,desc"}));
    }

    #[test]
    fn selected_collection_window_does_not_accept_wrong_window_or_drop_supplied_filters() {
        let (catalog, question, intake, options, mut choice) = energy_selection_fixture();
        choice.input.insert("company_intel.input.search".into(),json!("energie"));
        choice.input.insert("company_intel.input.publicatiedatumpreset".into(),json!("AF30"));
        assert!(compile_selected(&question,&Task::default(),&catalog,&intake,&[choice.clone()],&options).is_err());
        choice.input.insert("company_intel.input.publicatiedatumpreset".into(),json!("AF7"));
        let mut filtered=intake.clone();
        filtered.parsed_request.subjects[0].identifiers.insert("status".into(),"open".into());
        filtered.parsed_request.capabilities[0].inputs.push("status".into());
        let mut options=options;
        for option in &mut options { option.requirement.inputs.push("status".into()); }
        assert!(compile_selected(&question,&Task::default(),&catalog,&filtered,&[choice],&options).is_err());
    }

    #[test]
    fn selected_publication_window_cannot_stand_in_for_an_omitted_deadline() {
        use sha2::Digest;
        let (catalog, _, original, original_options, mut choice) = energy_selection_fixture();
        choice.input.insert("company_intel.input.search".into(), json!("energie"));
        choice.input.insert("company_intel.input.publicatiedatumpreset".into(), json!("AF7"));
        for question in [
            "Find energy tenders closing in the last 7 days.",
            "Find energy publications due in the last 7 days.",
            "Find energy tenders published recently and due in the last 7 days.",
            "Vind energieaanbestedingen met een deadline in de afgelopen 7 dagen.",
            "Vind energieaanbestedingen die sluiten in de laatste 7 dagen.",
            "Find energy tenders published in the last 30 days and closing in the last 7 days.",
            "Find energy publications published in the last 14 days.",
        ] {
            for output in ["contracts", "tenderned.publication_page"] {
                let mut intake=original.clone();
                intake.parsed_request.meta.text_sha256=hex::encode(sha2::Sha256::digest(question.as_bytes()));
                // Deliberately reproduce canonical omission: no closing input or
                // description evidence is available except the original question.
                intake.parsed_request.capabilities[0].description="Find energy procurement publications".into();
                intake.parsed_request.capabilities[0].outputs=vec![output.into()];
                let mut options=original_options.clone();
                for option in &mut options { option.requirement=intake.parsed_request.capabilities[0].clone(); }
                let error=compile_selected(question,&Task::default(),&catalog,&intake,&[choice.clone()],&options).err().expect("deadline accepted as publication window").to_string();
                if question.contains("14 days") { assert!(error.contains("publication window"), "{error}"); }
                else { assert!(error.contains("closing date or deadline"), "{question}: {error}"); }
            }
        }
    }

    #[test]
    fn explicitly_all_publications_can_use_a_native_window_without_a_keyword() {
        use sha2::Digest;
        let (catalog, _, original, original_options, mut choice)=energy_selection_fixture();
        choice.input.insert("company_intel.input.publicatiedatumpreset".into(),json!("AF7"));
        for question in ["Show all publications published in the last 7 days.","Toon alle publicaties gepubliceerd in de afgelopen 7 dagen."] {
            let mut intake=original.clone();
            intake.parsed_request.meta.text_sha256=hex::encode(sha2::Sha256::digest(question.as_bytes()));
            intake.parsed_request.subjects[0].label=if question.starts_with("Toon") { "publicaties" } else { "publications" }.into();
            intake.parsed_request.capabilities[0].name="Publications".into();
            intake.parsed_request.capabilities[0].description="Retrieve all publications".into();
            intake.parsed_request.capabilities[0].outputs=vec!["tenderned.publication_page".into()];
            let mut options=original_options.clone();
            for option in &mut options { option.requirement=intake.parsed_request.capabilities[0].clone(); }
            let prepared=compile_selected(question,&Task::default(),&catalog,&intake,&[choice.clone()],&options).unwrap();
            let (task,plan)=plan(prepared,question,&catalog);
            assert_eq!(plan.steps.len(),1);
            let input=plan.steps[0].service.inputs(&task,plan.steps[0].entity_ref,&catalog.vocabulary).unwrap();
            assert_eq!(input["publicatieDatumPreset"],"AF7");
            assert!(input.get("search").is_none());
        }
        for question in ["Show all energy publications published in the last 7 days.","Show all publications about energy published in the last 7 days."] {
            let mut intake=original.clone();
            intake.parsed_request.meta.text_sha256=hex::encode(sha2::Sha256::digest(question.as_bytes()));
            let mut options=original_options.clone();
            for option in &mut options { option.requirement=intake.parsed_request.capabilities[0].clone(); }
            let error=compile_selected(question,&Task::default(),&catalog,&intake,&[choice.clone()],&options).err().unwrap().to_string();
            assert!(error.contains("company_intel.input.search"),"{error}");
        }
    }

    #[test]
    fn selected_registry_keeps_the_original_missing_directors_and_ownership() {
        let (catalog, question, intake, options) = fixture(false);
        let choices = choose(&options); assert_eq!(choices.len(), 1);
        let prepared = compile_selected(&question, &Task::default(), &catalog, &intake, &choices, &options).unwrap();
        assert_eq!(prepared.coverage.gaps.iter().map(|gap| gap.output.as_str()).collect::<Vec<_>>(), ["directors", "ownership_structure"]);
        assert_eq!(serde_json::to_value(&prepared.coverage.original_intake).unwrap(), serde_json::to_value(&intake).unwrap());
        let (_, plan) = plan(prepared, &question, &catalog);
        assert_eq!(plan.steps.len(), 1); assert_eq!(plan.steps[0].service.endpoint_id, choices[0].endpoint_id);
    }

    #[test]
    fn two_explicit_alternatives_both_survive_even_when_one_is_cheaper() {
        let (catalog, question, intake, options) = fixture(true);
        let choices = choose(&options); assert_eq!(choices.len(), 2);
        let prepared = compile_selected(&question, &Task::default(), &catalog, &intake, &choices, &options).unwrap();
        let (_, plan) = plan(prepared, &question, &catalog);
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps.iter().map(|step| step.service.endpoint_id).collect::<BTreeSet<_>>(), choices.iter().map(|choice| choice.endpoint_id).collect());
        assert_eq!(plan.cap_atomic, plan.steps.iter().map(|step| step.service.total()).sum::<i64>());
    }

    #[test]
    fn two_contract_aliases_cannot_bill_the_same_endpoint_input_twice() {
        let (mut catalog, question, mut intake, mut options) = fixture(false);
        let original = catalog.services.iter().find(|service| service.contract.capability_slug == "company.registry-profile").unwrap().clone();
        let mut alias = original.clone(); alias.id = uuid::Uuid::new_v4(); alias.contract.capability_slug = "company_registry.lookup".into();
        let alias_index = catalog.services.len();
        catalog.by_capability.entry(alias.contract.capability_slug.clone()).or_default().push(alias_index);
        for output in alias.contract.output_map.produces.keys() { catalog.by_output.entry(output.clone()).or_default().push(alias_index); }
        catalog.services.push(alias);
        let mut option = options.iter().find(|option| option.source.capability.as_deref() == Some("company.registry-profile")).unwrap().clone();
        option.source.capability = Some("company_registry.lookup".into());
        intake.matches[0].apiosk.push(option.source.clone()); intake.matches[0].operations.insert("company_registry.lookup".into());
        let mut choices = choose(&options);
        choices.push(SelectedSource { endpoint_id: original.endpoint_id, capability: "company_registry.lookup".into(), subject_id: option.subject_id.clone(), input: option.input_values.clone() });
        options.push(option);
        let error = compile_selected(&question, &Task::default(), &catalog, &intake, &choices, &options).err().unwrap().to_string();
        assert!(error.contains("same request"), "{error}");
    }

    #[test]
    fn validated_user_input_edits_are_used_without_changing_saved_original_evidence() {
        let (catalog, question, intake, options) = fixture(false);
        let mut choices = choose(&options);
        choices[0].input.insert("company_registry.kvknummer".into(), json!("00123456"));
        let prepared = compile_selected(&question, &Task::default(), &catalog, &intake, &choices, &options).unwrap();
        assert_eq!(prepared.coverage.original_intake.parsed_request.subjects[0].identifiers["kvk_number"], "91251982");
        let (task, plan) = plan(prepared, &question, &catalog);
        assert_eq!(plan.steps[0].service.inputs(&task, plan.steps[0].entity_ref, &catalog.vocabulary).unwrap()["kvkNummer"], "00123456");
    }

    #[test]
    fn distinct_subjects_with_the_same_label_keep_their_own_values_and_calls() {
        let (catalog, question, mut intake, options) = fixture(false);
        let mut second = intake.parsed_request.subjects[0].clone(); second.id = "s2".into(); second.identifiers.insert("kvk_number".into(), "00123456".into());
        intake.parsed_request.subjects.push(second);
        let mut cap = intake.parsed_request.capabilities[0].clone(); cap.subject_id = Some("s2".into()); intake.parsed_request.capabilities.push(cap);
        intake.matches.push(intake.matches[0].clone());
        let mut combined_options = options.clone();
        for mut option in options { option.subject_id = "s2".into(); option.requirement.subject_id = Some("s2".into()); option.input_values.insert("company_registry.kvknummer".into(), json!("00123456")); combined_options.push(option); }
        let prepared = compile_selected(&question, &Task::default(), &catalog, &intake, &choose(&combined_options), &combined_options).unwrap();
        let (task, plan) = plan(prepared, &question, &catalog);
        assert_eq!(plan.steps.len(), 2);
        assert_ne!(plan.steps[0].entity_ref, plan.steps[1].entity_ref);
        let values: BTreeSet<_> = plan.steps.iter().map(|step| step.service.inputs(&task,step.entity_ref,&catalog.vocabulary).unwrap()["kvkNummer"].as_str().unwrap().to_owned()).collect();
        assert_eq!(values,BTreeSet::from(["91251982".into(),"00123456".into()]));
    }

    #[test]
    fn missing_inputs_unoffered_sources_and_unedited_filters_cannot_make_a_quote() {
        let (catalog, question, intake, options) = fixture(false);
        let mut choices = choose(&options);
        let mut missing = intake.clone(); missing.parsed_request.subjects[0].identifiers.clear(); choices[0].input.clear();
        let error = compile_selected(&question, &Task::default(), &catalog, &missing, &choices, &options).err().unwrap().to_string();
        assert!(error.contains("Enter company_registry.kvknummer"), "{error}");
        choices[0].endpoint_id = uuid::Uuid::new_v4();
        assert!(compile_selected(&question, &Task::default(), &catalog, &intake, &choices, &options).is_err());
        choices = choose(&options); choices[0].input.insert("undeclared.value".into(), json!("invented"));
        assert!(compile_selected(&question, &Task::default(), &catalog, &intake, &choices, &options).is_err());
        let mut filtered = intake.clone(); filtered.parsed_request.subjects[0].identifiers.insert("publication_after".into(), "2026-10-20".into());
        filtered.parsed_request.capabilities[0].inputs.push("publication_after".into());
        let mut filtered_options = options.clone(); filtered_options[0].requirement = filtered.parsed_request.capabilities[0].clone();
        assert!(compile_selected(&question, &Task::default(), &catalog, &filtered, &choose(&options), &filtered_options).is_err());
    }

    #[test]
    fn explicit_tender_choice_retains_newest_order_and_sector() {
        use sha2::Digest;
        let question = "Via Apiosk: Zoek nieuwe aanbestedingen in de energiesector.";
        let mut snapshot: crate::catalog::Snapshot = serde_json::from_str(include_str!("../../../tests/fixtures/connected-procurement/catalog.json")).unwrap();
        let catalog = Catalog::build(snapshot.clone());
        let parsed_request = serde_json::from_value(json!({"language":"nl",
            "subjects":[{"id":"s1","label":"energiesector","type":"sector","identifiers":{"sector":"energiesector"}}],
            "capabilities":[{"slug":"procurement.tenders.search","name":"Search new tenders","description":"Search for new tenders in the energy sector", "domain":"procurement",
                "inputs":["sector"],"outputs":["tenders"],"missing":[],"subject_id":"s1","source_hint":null,"confidence":0.9}],
            "deliverable":{"format":"chat","operations":[]},
            "meta":{"model":"capture","parsed_at":"2026-10-22T18:13:19Z","text_sha256":hex::encode(sha2::Sha256::digest(question.as_bytes()))}})).unwrap();
        let matches = crate::ask_v2::search::search(&parsed_request, &catalog);
        let options = crate::ask_v2::source_options::options(&parsed_request, &matches, &catalog);
        let intake = Intake { parsed_request, selected_capabilities: matches.iter().flat_map(|found| found.operations.iter().cloned()).collect(), matches,
            apiosk_sources_searched: 1, catalog_version: catalog.version.clone() };
        let option = options.iter().find(|option| option.source.capability.as_deref() == Some("procurement.nl-public-contracts")).unwrap();
        let selection = SelectedSource { endpoint_id: option.source.endpoint_id.as_deref().unwrap().parse().unwrap(), capability: option.source.capability.clone().unwrap(), subject_id: option.subject_id.clone(), input: option.input_values.clone() };
        let prepared = compile_selected(question, &Task::default(), &catalog, &intake, &[selection.clone()], &options).unwrap();
        assert!(prepared.coverage.limitations.iter().any(|notice| notice.contains("newest first")));
        let (task, plan) = plan(prepared, question, &catalog);
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].service.inputs(&task, plan.steps[0].entity_ref, &catalog.vocabulary).unwrap(),
            json!({"page":0,"size":20,"sort":"publicatieDatum,desc","search":"energiesector"}));
        for service in &mut snapshot.services { service.fixed_params.as_object_mut().unwrap().remove("sort"); }
        let unsorted = Catalog::build(snapshot); let mut changed = intake; changed.catalog_version = unsorted.version.clone();
        assert!(compile_selected(question, &Task::default(), &unsorted, &changed, &[selection], &options).is_err());
    }

    #[test]
    fn explicit_structured_source_values_keep_the_actual_catalog_schema_and_provider_inputs() {
        use sha2::Digest;
        let catalog = Catalog::build(serde_json::from_str(include_str!("../../../tests/fixtures/connected-planning/structured-inputs-catalog.json")).unwrap());
        assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
        for (source, capability, values, provider_values) in [
            ("tavily", "web.search", json!({"web.query":"Example", "web.input.includedomains":["example.com"], "web.input.excludedomains":["excluded.example"]}),
                json!({"query":"Example", "include_domains":["example.com"], "exclude_domains":["excluded.example"]})),
            ("exa", "web.page-to-text", json!({"web.input.urls":["https://example.com/one","https://example.com/two"], "web.input.text":true}),
                json!({"urls":["https://example.com/one","https://example.com/two"], "text":true})),
            ("exa", "web.page-to-text", json!({"web.input.urls":["https://example.com/one"], "web.input.text":false}),
                json!({"urls":["https://example.com/one"], "text":false})),
            ("opensanctions", "company.uk-director-screening-1", json!({"company.uk_director_1.screening_query":{"q1":{"schema":"Person","properties":{"name":["Example"]}}}}),
                json!({"queries":{"q1":{"schema":"Person","properties":{"name":["Example"]}}}})),
        ] {
            let service = catalog.services.iter().find(|service| service.api_slug == source && service.contract.capability_slug == capability).unwrap();
            let question = "Retrieve the selected source records for Example.";
            let parsed_request = serde_json::from_value(json!({"language":"en",
                "subjects":[{"id":"s1","label":"Example","type":"topic","identifiers":{}}],
                "capabilities":[{"slug":capability,"name":"Selected records","description":"Retrieve the selected source records", "domain":"general",
                    "inputs":[],"outputs":service.contract.output_map.produces.keys().collect::<Vec<_>>(),"missing":[],"subject_id":"s1","source_hint":source,"confidence":1}],
                "deliverable":{"format":"chat","operations":[]},
                "meta":{"model":"fixture","parsed_at":"2026-10-22T18:13:19Z","text_sha256":hex::encode(sha2::Sha256::digest(question.as_bytes()))}})).unwrap();
            let candidate = crate::capability_intake::Candidate { origin:"apiosk".into(), name:service.name.clone(), description:String::new(), source_slug:Some(source.into()), capability:Some(capability.into()), endpoint_id:Some(service.endpoint_id.to_string()), url:None, method:"GET".into(), price:None, network:None, calls_30d:None, availability:"supported".into() };
            let matches = vec![Match {slug:capability.into(),query:String::new(),operations:BTreeSet::from([capability.into()]),apiosk:vec![candidate],coinbase:vec![],coinbase_status:"complete".into()}];
            let options = crate::ask_v2::source_options::options(&parsed_request, &matches, &catalog);
            let intake = Intake { parsed_request, matches, selected_capabilities:BTreeSet::from([capability.into()]), catalog_version:catalog.version.clone(),apiosk_sources_searched:3 };
            let selection = SelectedSource {endpoint_id:service.endpoint_id,capability:capability.into(),subject_id:"s1".into(),input:serde_json::from_value(values.clone()).unwrap()};
            let prepared = compile_selected(question, &Task::default(), &catalog, &intake, &[selection.clone()], &options).unwrap_or_else(|error| panic!("{source}: {error}"));
            let intent = prepared.reply.intent.as_ref().unwrap();
            for (field,value) in values.as_object().unwrap() { assert_eq!(intent.subjects[0].values.get(field),Some(value),"{source} {field}"); }
            assert!(intent.validate(question, &Task::default(), &catalog).is_err(),"ordinary model literal policy must remain strict");
            let ordinary = crate::parser::contract_planner::compile(question,question,&Task::default(),&catalog,&prepared.intake);
            assert!(!ordinary.discover,"ordinary compilation cannot authorize structured model-created values");
            let (task, plan) = plan(prepared, question, &catalog);
            let actual = plan.steps[0].service.inputs(&task, plan.steps[0].entity_ref, &catalog.vocabulary).unwrap();
            for (key,value) in provider_values.as_object().unwrap() { assert_eq!(actual.get(key),Some(value),"{source}: {actual}"); }
            let mut wrong = selection; wrong.input.insert(values.as_object().unwrap().keys().next().unwrap().clone(), json!({"not":"the declared type"}));
            assert!(compile_selected(question, &Task::default(), &catalog, &intake, &[wrong], &options).is_err());
        }
    }
}
