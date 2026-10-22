//! A vague model namespace must not hide an admitted, exactly named output.
//! This only supplies a search namespace; normal search still owns source,
//! country and operation relevance, and the canonical request is unchanged.
use crate::{capability_intake::Intake, catalog::{Catalog, Service}};
use serde_json::Value;
use std::collections::BTreeSet;

fn collection_alias(schema: &Value, output: &str) -> bool {
    if schema["type"] == "array" && schema["items"]["type"] == "object"
        && super::super::plan_coverage::output_aliases(schema).contains(output) {
        return true;
    }
    schema.get("properties").and_then(Value::as_object).is_some_and(|fields|
        fields.iter().any(|(name, child)| {
            (name == output && child["type"] == "array" && child["items"]["type"] == "object")
                || collection_alias(child, output)
        }))
}

fn proves(service: &Service, catalog: &Catalog, outputs: &[String]) -> bool {
    !service.unverified && service.contract.output_map.produces.keys().any(|fact|
        catalog.vocabulary.get(fact).is_some_and(|definition| outputs.iter().any(|output|
            !matches!(output.as_str(), "data" | "results" | "records" | "items" | "content" | "entries")
                && collection_alias(&definition.value_schema, output))))
}

/// Recover only an empty connected-client match with an unambiguous namespace
/// or endpoint proven by the collection's exact documented name or reviewed alias.
/// This never turns descriptive listings or an opaque source document into a call.
pub fn refresh_discovery(intake: &mut Intake, catalog: &Catalog) -> bool {
    let mut changed = false;
    for (index, cap) in intake.parsed_request.capabilities.iter().enumerate() {
        if !matches!(cap.domain.as_str(), "general" | "research") { continue; }
        let Some(found) = super::super::explicit_request::request_match(intake, index) else { continue; };
        if !found.operations.is_empty() { continue; }
        let eligible: Vec<_> = catalog.services.iter().filter(|service| proves(service, catalog, &cap.outputs)).collect();
        let domains: BTreeSet<_> = eligible.iter().filter_map(|service| service.contract.capability_slug.split_once('.').map(|(domain, _)| domain))
            .filter(|domain| !matches!(*domain, "general" | "research" | "operation" | "endpoint")).collect();
        // One admitted response may have several catalog contracts. Namespace
        // aliases for that exact endpoint are not competing source meanings.
        let endpoints: BTreeSet<_> = eligible.iter().map(|service| service.endpoint_id).collect();
        if domains.is_empty() || (domains.len() != 1 && endpoints.len() != 1) { continue; }
        let mut candidates = Vec::new();
        let mut seen = BTreeSet::new();
        for domain in domains {
            let mut search_view = intake.parsed_request.clone();
            search_view.capabilities = vec![cap.clone()];
            search_view.capabilities[0].domain = domain.to_owned();
            let Some(recovered) = crate::ask_v2::search::search(&search_view, catalog).into_iter().next() else { continue; };
            candidates.extend(recovered.apiosk.into_iter().filter(|candidate| candidate.availability == "supported" && eligible.iter().any(|service|
                candidate.capability.as_deref() == Some(service.contract.capability_slug.as_str())
                    && candidate.endpoint_id.as_deref() == Some(service.endpoint_id.to_string().as_str()))
                && seen.insert((candidate.capability.clone(), candidate.endpoint_id.clone()))));
        }
        if candidates.is_empty() { continue; }
        let Some(target) = intake.matches.iter_mut().filter(|matched| matched.slug == cap.slug).nth(
            intake.parsed_request.capabilities[..index].iter().filter(|prior| prior.slug == cap.slug).count()) else { continue; };
        target.operations.extend(candidates.iter().filter_map(|candidate| candidate.capability.clone()));
        target.apiosk.extend(candidates);
        changed = true;
    }
    if changed {
        intake.selected_capabilities = intake.matches.iter().flat_map(|found| found.operations.iter().cloned()).collect();
    }
    changed
}

/// The parser sometimes stores a typed subject's literal under `name` while
/// requesting that same type as an input. Only a reviewed source input alias
/// can bind that existing label. Keep every other identifier for validation.
pub fn input_context(original: &Intake, catalog: &Catalog) -> Intake {
    let mut intake = original.clone();
    for (index, cap) in original.parsed_request.capabilities.iter().enumerate() {
        let Some(subject) = cap.subject_id.as_ref().and_then(|id| original.parsed_request.subjects.iter().find(|subject| &subject.id == id)) else { continue; };
        if subject.label.trim().is_empty() || subject.identifiers.get("name") != Some(&subject.label)
            || subject.identifiers.contains_key(&subject.kind) || !cap.inputs.contains(&subject.kind) { continue; }
        let Some(found) = super::super::explicit_request::request_match(original, index) else { continue; };
        let declared = catalog.services.iter().any(|service| found.apiosk.iter().any(|candidate|
            candidate.availability == "supported" && candidate.capability.as_deref() == Some(service.contract.capability_slug.as_str())
                && candidate.endpoint_id.as_deref() == Some(service.endpoint_id.to_string().as_str()))
            && service.contract.requires.iter().any(|required| catalog.vocabulary.get(&required.fact_type).is_some_and(|definition|
                crate::ask_v2::input_values::reviewed_input_alias(&definition.value_schema, &subject.kind))));
        if declared {
            let target = intake.parsed_request.subjects.iter_mut().find(|target| target.id == subject.id).unwrap();
            target.identifiers.insert(subject.kind.clone(), subject.label.clone());
        }
    }
    intake
}
