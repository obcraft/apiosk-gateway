//! Connected clients can review the executable part of a request without
//! losing its unanswered requirements. No provider is called here. The App
//! continues to use its existing complete-request planner.
use super::{contract_planner, plan_coverage, ChatReply};
use crate::{capability_intake::Intake, catalog::Catalog, state::Task};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
mod collections;
mod discovery;
pub use discovery::{anchor_collection_request, input_context, refresh_discovery};
pub(crate) use discovery::qualified_collection_domain;
mod scope;
pub use scope::{preserve_scope, scope_feedback, scope_feedback_for, scope_requirements, ScopeRequirement};
mod selected;
mod selected_collection;
pub(crate) use selected::compile_selected;
pub(crate) use selected::needs_native_lookup;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gap {
    pub subject: Option<String>,
    pub output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Coverage {
    pub original_intake: Intake,
    pub gaps: Vec<Gap>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
}

impl Coverage {
    pub fn notices(&self) -> Vec<String> {
        let mut notices = self.limitations.clone();
        if self.gaps.is_empty() { return notices; }
        let fields = self.gaps.iter().map(|gap| {
            let field = gap.output.replace('_', " ");
            match &gap.subject { Some(subject) => format!("{field} ({subject})"), None => field }
        }).collect::<Vec<_>>().join(", ");
        notices.push(format!("This price covers only the listed source calls. Not retrieved or verified by this plan: {fields}. These items remain missing; do not treat the result as a complete investigation."));
        notices
    }
}

pub struct Prepared {
    pub reply: ChatReply,
    pub intake: Intake,
    pub coverage: Coverage,
}

// A structured fact such as company.profile can answer the broad category
// company_details. The namespace must be identical; a scalar, an opaque
// document or a narrower category cannot stand in for that record.
fn record_category(value: &str) -> Option<Vec<String>> {
    let mut words: Vec<_> = value.split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty()).map(str::to_lowercase).collect();
    if !matches!(words.last().map(String::as_str), Some("details" | "detail" | "profile" | "record" | "records")) { return None; }
    words.pop();
    (!words.is_empty()).then_some(words)
}

fn metadata(output: &str) -> bool {
    matches!(output, "source" | "sources" | "source_reference" | "source_references"
        | "citations" | "source_citations" | "missing_information" | "missing_fields"
        | "missing_data" | "missing_sources" | "coverage_gaps" | "verification_gaps")
}

/// Try exact contract coverage first. A reduced proposal is permitted only
/// when every remaining field and supplied filter passes the normal compiler.
/// Original requirements remain saved, returned and disclosed before approval.
pub fn compile(question: &str, task: &Task, catalog: &Catalog, original: &Intake) -> Option<Prepared> {
    compile_scoped(question, task, catalog, original, None)
}

// Explicit connected selections have already been narrowed to signed source
// options. Do not rediscover alternatives, and admit only their validated
// entered values as additional literal evidence.
fn compile_scoped(question: &str, task: &Task, catalog: &Catalog, original: &Intake, entered: Option<&selected::ExplicitValues>) -> Option<Prepared> {
    if scope_feedback(original, catalog).is_some() { return None; }
    let original_intake = original.clone();
    // An exact paused retry can reuse validated intake without parsing again.
    // Recover its empty matches too, while retaining the saved request evidence.
    let mut discovery_view = original.clone();
    if entered.is_none() { refresh_discovery(&mut discovery_view, catalog); }
    let mut input_view = input_context(&discovery_view, catalog);
    let mut limitations = collections::constrain_recency(question, &mut input_view, catalog)?;
    let recency_limited = !limitations.is_empty();
    let original = &input_view;
    let mut literals = super::capability_binding::literal_context(question, task, None);
    if let Some(entered) = entered { literals.push('\n'); literals.push_str(&entered.literals); }
    let compile_part = |intake: &Intake| match entered {
        Some(entered) => contract_planner::compile_selected_values(question, &literals, task, catalog, intake, &entered.values),
        None => contract_planner::compile(question, &literals, task, catalog, intake),
    };
    let mut full = compile_part(original);
    if contract_planner::ready(question, &full, task, catalog, original) {
        let coverage = Coverage { original_intake, gaps: vec![], limitations };
        if let Some(intent) = &mut full.intent { intent.answer_operations.extend(coverage.notices()); }
        return Some(Prepared { reply: full, intake: original.clone(), coverage });
    }
    let mut intake = original.clone();
    intake.parsed_request.capabilities.clear();
    intake.matches.clear();
    let mut gaps = Vec::new();
    let mut changed = false;
    for (index, cap) in original.parsed_request.capabilities.iter().enumerate() {
        if plan_coverage::answer_instruction(cap) { continue; }
        let found = super::explicit_request::request_match(original, index)?;
        let subject = cap.subject_id.as_ref().and_then(|id| original.parsed_request.subjects.iter().find(|subject| &subject.id == id));
        let services: Vec<_> = catalog.services.iter().filter(|service| found.apiosk.iter().any(|candidate|
            candidate.availability == "supported" && candidate.capability.as_deref() == Some(service.contract.capability_slug.as_str())
                && candidate.endpoint_id.as_deref() == Some(service.endpoint_id.to_string().as_str())
                && found.operations.contains(&service.contract.capability_slug))).collect();
        let mut kept = cap.clone();
        kept.outputs.clear();
        let mut kept_match = found.clone();
        for output in &cap.outputs {
            if metadata(output) { changed = true; continue; }
            let mut options = vec![(output.clone(), None, None)];
            if let Some(category) = record_category(output) {
                for service in &services {
                    for fact in service.contract.output_map.produces.keys() {
                        if record_category(fact).as_ref() == Some(&category)
                            && catalog.vocabulary.get(fact).is_some_and(|definition|
                                crate::ask_v2::output_schema::documented(&definition.value_schema)
                                    && matches!(definition.value_schema["type"].as_str(), Some("object" | "array"))) {
                            options.push((fact.clone(), None, None));
                        }
                    }
                }
            }
            for service in &services {
                options.extend(collections::options(question, output, &cap.domain, service, catalog).into_iter()
                    .filter(|(_, notice)| notice.is_none() || cap.outputs.iter().filter(|output| !metadata(output)).count() == 1)
                    .map(|(fact, notice)| {
                        let pin = notice.as_ref().map(|_| (service.contract.capability_slug.clone(), service.endpoint_id.to_string()));
                        (fact, notice, pin)
                    }));
            }
            options.dedup();
            let supported = options.into_iter().find(|(candidate, _, pin)| {
                let mut part = original.clone();
                let mut requirement = cap.clone();
                requirement.outputs = vec![candidate.clone()];
                part.parsed_request.capabilities = vec![requirement];
                part.parsed_request.subjects = subject.into_iter().cloned().collect();
                part.matches = vec![found.clone()];
                if let Some((capability, endpoint)) = pin {
                    part.matches[0].apiosk.retain(|candidate| candidate.capability.as_ref() == Some(capability)
                        && candidate.endpoint_id.as_ref() == Some(endpoint));
                    part.matches[0].operations.retain(|operation| operation == capability);
                }
                part.selected_capabilities = part.matches[0].operations.clone();
                let reply = compile_part(&part);
                contract_planner::ready(question, &reply, task, catalog, &part)
            });
            if let Some((supported, notice, pin)) = supported {
                if let Some(notice) = notice { if !recency_limited && !limitations.contains(&notice) { limitations.push(notice); } }
                if let Some((capability, endpoint)) = pin {
                    kept_match.apiosk.retain(|candidate| candidate.capability.as_ref() == Some(&capability)
                        && candidate.endpoint_id.as_ref() == Some(&endpoint));
                    kept_match.operations.retain(|operation| operation == &capability);
                }
                changed |= supported != *output;
                if !kept.outputs.contains(&supported) { kept.outputs.push(supported); }
            } else {
                changed = true;
                gaps.push(Gap { subject: subject.map(|subject| subject.label.clone()), output: output.clone() });
            }
        }
        if !kept.outputs.is_empty() {
            intake.parsed_request.capabilities.push(kept);
            intake.matches.push(kept_match);
        }
    }
    if !changed || intake.parsed_request.capabilities.is_empty() { return None; }
    let subjects: BTreeSet<_> = intake.parsed_request.capabilities.iter().filter_map(|cap| cap.subject_id.clone()).collect();
    intake.parsed_request.subjects.retain(|subject| subjects.contains(&subject.id));
    intake.selected_capabilities = intake.matches.iter().flat_map(|found| found.operations.iter().cloned()).collect();
    let mut reply = compile_part(&intake);
    if !contract_planner::ready(question, &reply, task, catalog, &intake) { return None; }
    let coverage = Coverage { original_intake, gaps, limitations };
    if let Some(intent) = &mut reply.intent {
        intent.answer_operations.extend(coverage.notices());
        intent.answer_operations.push("Cite the actual sources and explicitly identify missing or unverified requested information.".into());
    }
    Some(Prepared { reply, intake, coverage })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn fixture(raw: &str) -> (Catalog, String, Intake) {
        let catalog = Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/connected-planning/catalog.json")).unwrap());
        assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
        let saved: Value = serde_json::from_str(raw).unwrap();
        let question = saved["question"].as_str().unwrap().to_owned();
        let parsed_request = serde_json::from_value(saved["parsed_request"].clone()).unwrap();
        let matches = crate::ask_v2::search::search(&parsed_request, &catalog);
        let intake = Intake { selected_capabilities: matches.iter().flat_map(|found| found.operations.iter().cloned()).collect(),
            parsed_request, matches, apiosk_sources_searched: 2, catalog_version: catalog.version.clone() };
        (catalog, question, intake)
    }

    #[test]
    fn company_investigation_prices_the_registry_and_keeps_directors_and_ownership_missing() {
        let (catalog, question, intake) = fixture(include_str!("../../tests/fixtures/connected-planning/fcca.json"));
        let prepared = compile(&question, &Task::default(), &catalog, &intake).expect("available registry data must have a reviewable proposal");
        assert_eq!(prepared.coverage.gaps.iter().map(|gap|gap.output.as_str()).collect::<Vec<_>>(), ["directors", "ownership_structure"]);
        assert_eq!(serde_json::to_value(&prepared.coverage.original_intake).unwrap(), serde_json::to_value(&intake).unwrap());
        let intent = prepared.reply.intent.unwrap();
        assert_eq!(intent.goals.len(), 1);
        assert_eq!(intent.goals[0].capability, "company.registry-profile");
        assert_eq!(intent.subjects[0].values["company_registry.kvknummer"], "91251982");
        assert!(intent.answer_operations.iter().any(|text|text.contains("ownership structure")));
        assert!(prepared.coverage.notices()[0].contains("Not retrieved or verified"));
        assert!(compile("another question", &Task::default(), &catalog, &intake).is_none());
    }

    #[test]
    fn energy_tenders_use_the_existing_contract_without_dropping_sector() {
        let (catalog, question, intake) = fixture(include_str!("../../tests/fixtures/connected-planning/energy.json"));
        let prepared = compile(&question, &Task::default(), &catalog, &intake).expect("the admitted tender search must be priced");
        assert!(prepared.coverage.gaps.is_empty());
        let intent = prepared.reply.intent.unwrap();
        assert_eq!(intent.goals.len(), 1);
        assert_eq!(intent.goals[0].capability, "procurement.nl-public-contracts");
        assert_eq!(intent.subjects[0].values["company_intel.input.search"], "energiesector");
    }
}
