//! Detect a supported filter the interpreter mentioned but did not preserve.
//! This is repair feedback, never an attempt to extract or invent its value.
use crate::{capability_intake::Intake, catalog::{Catalog, Service}};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct ScopeRequirement {
    pub collection_outputs: BTreeSet<String>,
    pub input_alias: String,
    pub input_facts: BTreeSet<String>,
    collection_name: String,
    collection_description: String,
    request_hash: String,
    original_capability_index: usize,
    original_subject_id: Option<String>,
    original_collection_count: usize,
}

fn words(value: &str) -> Vec<String> {
    value.split(|ch: char| !ch.is_alphanumeric()).filter(|word| !word.is_empty()).map(str::to_lowercase).collect()
}

fn specific(alias: &str) -> bool {
    !matches!(alias, "q" | "query" | "search" | "search_query" | "search_term" | "search_phrase"
        | "keyword" | "keywords" | "name" | "label" | "term" | "text")
}

fn criterion(description: &str, alias: &[String]) -> bool {
    !alias.is_empty() && description.split(['.', ';', '!', '?']).any(|clause| {
        let tokens = words(clause);
        tokens.windows(alias.len()).enumerate().any(|(index, candidate)| candidate == alias
            && tokens[index.saturating_sub(4)..index].iter().any(|word|
                matches!(word.as_str(), "in" | "within" | "with" | "by" | "for" | "about")))
    })
}

fn context(original: &Intake, catalog: &Catalog) -> Intake {
    let mut discovered = original.clone();
    super::refresh_discovery(&mut discovered, catalog);
    super::input_context(&discovered, catalog)
}

fn services<'a>(intake: &Intake, index: usize, catalog: &'a Catalog) -> Vec<&'a Service> {
    let Some(found) = super::super::explicit_request::request_match(intake, index) else { return vec![]; };
    catalog.services.iter().filter(|service| !service.unverified
        && found.operations.contains(&service.contract.capability_slug)
        && found.apiosk.iter().any(|candidate| candidate.availability == "supported"
            && candidate.capability.as_deref() == Some(service.contract.capability_slug.as_str())
            && candidate.endpoint_id.as_deref() == Some(service.endpoint_id.to_string().as_str()))).collect()
}

/// Only reviewed aliases on a matched collection source can supply feedback.
/// Retain the original collection and source-input evidence across correction;
/// deleting a phrase in a second parse cannot erase its known requirement.
pub fn scope_requirements(original: &Intake, catalog: &Catalog) -> Vec<ScopeRequirement> {
    let intake = context(original, catalog);
    let mut missing: BTreeMap<(usize, String), ScopeRequirement> = BTreeMap::new();
    for (index, cap) in intake.parsed_request.capabilities.iter().enumerate() {
        if crate::capability_intake::answer_instruction(cap) { continue; }
        let subject = cap.subject_id.as_ref().and_then(|id| intake.parsed_request.subjects.iter().find(|subject| &subject.id == id));
        for service in services(&intake, index, catalog).into_iter().filter(|service| super::discovery::proves(service, catalog, &cap.outputs)) {
            let collection_outputs: BTreeSet<_> = cap.outputs.iter().filter(|output|
                super::discovery::proves(service, catalog, std::slice::from_ref(output))).cloned().collect();
            for required in &service.contract.requires {
                let Some(definition) = catalog.vocabulary.get(&required.fact_type) else { continue; };
                let schema = &definition.value_schema;
                if subject.and_then(|subject| crate::ask_v2::input_values::documented_literal(&required.fact_type, schema, subject)).is_some() { continue; }
                let Some(comment) = schema["$comment"].as_str().filter(|comment| comment.len() <= 2048) else { continue; };
                let Ok(metadata) = serde_json::from_str::<Value>(comment) else { continue; };
                for alias in metadata["apiosk_input_aliases_v1"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                    let normalized = words(alias);
                    if specific(alias) && crate::ask_v2::input_values::reviewed_input_alias(schema, alias)
                        && !cap.outputs.iter().any(|output| words(output).ends_with(&normalized))
                        && criterion(&cap.description, &normalized) {
                        let requirement = missing.entry((index, alias.to_owned())).or_insert_with(|| ScopeRequirement {
                            collection_outputs: BTreeSet::new(), input_alias: alias.to_owned(), input_facts: BTreeSet::new(),
                            collection_name: cap.name.clone(), collection_description: cap.description.clone(),
                            request_hash: intake.parsed_request.meta.text_sha256.clone(), original_capability_index: index,
                            original_subject_id: cap.subject_id.clone(), original_collection_count: 0,
                        });
                        requirement.collection_outputs.extend(collection_outputs.iter().cloned());
                        requirement.input_facts.insert(required.fact_type.clone());
                    }
                }
            }
        }
    }
    let mut requirements: Vec<_> = missing.into_values().collect();
    for requirement in &mut requirements {
        requirement.original_collection_count = (0..intake.parsed_request.capabilities.len())
            .filter(|index| collection_matches(requirement, &intake, *index, catalog)).count();
    }
    requirements
}

fn feedback(missing: BTreeSet<String>) -> Option<String> {
    (!missing.is_empty()).then(|| format!("Preserve these source-supported input criteria from the original question in capability.inputs and the corresponding subject.identifiers: {}. Keep the supplied literal values; do not invent values or silently broaden the request.", missing.into_iter().collect::<Vec<_>>().join(", ")))
}

/// A corrected parse may improve subject IDs, labels and domains. The original
/// documented collection and semantic input alias must still have an actual
/// bound literal on that collection, independent of the corrected description.
fn collection_service(requirement: &ScopeRequirement, intake: &Intake, index: usize, service: &Service, catalog: &Catalog) -> bool {
    let cap = &intake.parsed_request.capabilities[index];
    requirement.collection_outputs.iter().any(|output| super::discovery::proves(service, catalog, std::slice::from_ref(output)))
        && cap.outputs.iter().any(|output| super::discovery::proves(service, catalog, std::slice::from_ref(output))
            || !super::collections::options("", output, &cap.domain, service, catalog).is_empty())
}

fn collection_matches(requirement: &ScopeRequirement, intake: &Intake, index: usize, catalog: &Catalog) -> bool {
    services(intake, index, catalog).into_iter().any(|service| collection_service(requirement, intake, index, service, catalog))
}

fn corresponding(requirement: &ScopeRequirement, intake: &Intake, catalog: &Catalog) -> Option<usize> {
    if requirement.request_hash != intake.parsed_request.meta.text_sha256 { return None; }
    let candidates: Vec<_> = (0..intake.parsed_request.capabilities.len())
        .filter(|index| collection_matches(requirement, intake, *index, catalog)).collect();
    if requirement.original_collection_count == 1 && candidates.len() == 1 { return Some(candidates[0]); }
    // Repeated collections need stable per-instance correspondence. A bound
    // filter on a second subject cannot repair the first subject's omission.
    candidates.into_iter().find(|index| *index == requirement.original_capability_index
        && requirement.original_subject_id.is_some()
        && intake.parsed_request.capabilities[*index].subject_id == requirement.original_subject_id)
}

fn bound_to(requirement: &ScopeRequirement, intake: &Intake, index: usize, catalog: &Catalog) -> bool {
    let cap = &intake.parsed_request.capabilities[index];
    let Some(subject) = cap.subject_id.as_ref().and_then(|id| intake.parsed_request.subjects.iter().find(|subject| &subject.id == id)) else { return false; };
    services(intake, index, catalog).into_iter().any(|service|
        collection_service(requirement, intake, index, service, catalog)
            && service.contract.requires.iter().any(|required| catalog.vocabulary.get(&required.fact_type).is_some_and(|definition|
                crate::ask_v2::input_values::reviewed_input_alias(&definition.value_schema, &requirement.input_alias)
                    && crate::ask_v2::input_values::documented_literal(&required.fact_type, &definition.value_schema, subject).is_some())))
}

pub fn scope_feedback_for(requirements: &[ScopeRequirement], original: &Intake, catalog: &Catalog) -> Option<String> {
    let intake = context(original, catalog);
    feedback(requirements.iter().filter(|requirement| !corresponding(requirement, &intake, catalog)
        .is_some_and(|index| bound_to(requirement, &intake, index, catalog)))
        .map(|requirement| requirement.input_alias.clone()).collect())
}

/// Preserve the initial collection qualifiers after its missing input has
/// been repaired. Append evidence instead of erasing corrected meaning, and
/// refuse an oversized merge rather than truncating either requirement.
pub fn preserve_scope(requirements: &[ScopeRequirement], original: &mut Intake, catalog: &Catalog) -> bool {
    let intake = context(original, catalog);
    let mut updated = original.clone();
    for requirement in requirements {
        let Some(index) = corresponding(requirement, &intake, catalog).filter(|index| bound_to(requirement, &intake, *index, catalog)) else { return false; };
        {
            let cap = &mut updated.parsed_request.capabilities[index];
            let evidence = format!("Original collection request: {}. {}", requirement.collection_name, requirement.collection_description);
            if (cap.name == requirement.collection_name && cap.description == requirement.collection_description)
                || cap.description.contains(&evidence) { continue; }
            let combined = format!("{} {evidence}", cap.description);
            if combined.chars().count() > 1000 { return false; }
            cap.description = combined;
        }
    }
    *original = updated;
    true
}

pub fn scope_feedback(original: &Intake, catalog: &Catalog) -> Option<String> {
    feedback(scope_requirements(original, catalog).into_iter().map(|requirement| requirement.input_alias).collect())
}
