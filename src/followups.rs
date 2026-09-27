use crate::{
    catalog::{Catalog, Service},
    state::Task,
};
use serde::Serialize;
use std::collections::BTreeSet;
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct RelatedService {
    pub service_ref: Uuid,
    pub entity_ref: Uuid,
    pub title: String,
    pub provider: String,
    pub subject: String,
    pub total_atomic: String,
}

fn title(service: &Service) -> String {
    // Legacy contracts may call several different endpoints simply "lookup".
    let label = match service.path.as_str() {
        "/api/v1/hvds/jaarrekeningen/kvknummer" => "Financial statements",
        "/api/v1/naamgevingen/kvknummer" => "Registered names",
        "/api/v1/basisprofielen" => "Company registry profile",
        "/api/v1/vestigingsprofielen" => "Branch profile",
        "/api/seo/full-audit" => "Full SEO audit",
        "/api/seo/site-audit" => "Site SEO audit",
        "/api/safety/recalls" => "Product recalls",
        "/api/safety/merchant" => "Merchant risk assessment",
        "/api/filings/search" => "Search SEC filing disclosures",
        _ => service
            .contract
            .capability_slug
            .split_once('.')
            .map_or(service.contract.capability_slug.as_str(), |(_, tail)| tail),
    };
    label.replace(['.', '_', '-'], " ")
}

/// A filing search can reuse the person's exact question as its query. The
/// provider receives no invented ticker, CIK or inferred filing period.
pub(crate) fn bind_filing_question(catalog: &Catalog, task: &mut Task, service: &Service, entity: Uuid) -> anyhow::Result<()> {
    if service.api_slug != "filingspulse" || service.path != "/api/filings/search" { return Ok(()); }
    let Some(label) = task.entities.iter().find(|item| item.entity_ref == entity && item.selected).map(|item| item.label.clone()) else { return Ok(()); };
    let Some(question) = task.conversation.last().map(|turn| turn.question.clone()) else { return Ok(()); };
    if crate::parser::broad_filing_subject(&question).is_some() { return Ok(()); }
    if !question.to_lowercase().contains("filing") || !question.to_lowercase().contains(&label.to_lowercase()) { return Ok(()); }
    for required in service.contract.requires.iter().filter(|r| !r.optional) {
        if service.contract.input_map.get(&required.fact_type).is_some_and(|binding| binding.name == "query") {
            crate::engine::input::user_fact(task, catalog, entity, &required.fact_type, serde_json::json!(question))?;
        }
    }
    Ok(())
}

/// Only suggest immediately bindable services on the last completed subject.
/// This reads catalog contracts and saved facts; it never calls a provider.
pub fn related_services(catalog: &Catalog, task: &Task) -> Vec<RelatedService> {
    if task.result_refs.is_empty()
        || task.cancelled
        || task.execution_error.is_some()
        || task.analysis.as_ref().is_some_and(|analysis| analysis["follow_up_question"].is_string()
            || analysis["observations"].as_array().is_some_and(Vec::is_empty))
        || !task.pending_candidates.is_empty()
        || !task.actions.is_empty()
    {
        return vec![];
    }
    // A form/topic-free request cannot safely seed a paid full-text search.
    if task.conversation.last().is_some_and(|turn| crate::parser::broad_filing_subject(&turn.question).is_some()) {
        return vec![];
    }
    let Some((service_id, entity_id)) = task.completed.last().and_then(|key| key.split_once(':'))
    else {
        return vec![];
    };
    let Some(last) = catalog
        .services
        .iter()
        .find(|s| s.id.to_string() == service_id)
    else {
        return vec![];
    };
    let Some(entity) = task
        .entities
        .iter()
        .find(|e| e.entity_ref.to_string() == entity_id && e.selected)
    else {
        return vec![];
    };
    let used_endpoints: BTreeSet<_> = task
        .completed
        .iter()
        .filter_map(|key| {
            let (id, subject) = key.split_once(':')?;
            (subject == entity_id)
                .then(|| {
                    catalog
                        .services
                        .iter()
                        .find(|s| s.id.to_string() == id)
                        .map(|s| s.endpoint_id)
                })
                .flatten()
        })
        .collect();
    let related_types: BTreeSet<_> = last
        .contract
        .requires
        .iter()
        .map(|r| r.fact_type.as_str())
        .chain(last.contract.output_map.produces.keys().map(String::as_str))
        .collect();
    let filing_request = task.conversation.last().is_some_and(|turn| turn.question.to_lowercase().contains("filing"));
    let uk_filing_request = task.conversation.last().is_some_and(|turn| {
        let q = turn.question.to_ascii_lowercase();
        ["companies house", "uk filing", "british filing", "united kingdom filing"].iter().any(|term| q.contains(term))
    });
    let mut candidates: Vec<_> = catalog
        .services
        .iter()
        .filter(|s| {
            !used_endpoints.contains(&s.endpoint_id)
                && (!filing_request || (s.api_slug == "filingspulse" && s.path == "/api/filings/search")
                    || (uk_filing_request && s.contract.capability_slug == "company.filing-history"))
                && (s.contract.requires.iter().any(|r| related_types.contains(r.fact_type.as_str()))
                    || (filing_request && s.api_slug == "filingspulse" && s.path == "/api/filings/search"))
                && {
                    if s.api_slug == "filingspulse" && s.path == "/api/filings/search" {
                        let mut staged = task.clone();
                        bind_filing_question(catalog, &mut staged, s, entity.entity_ref).is_ok()
                            && s.inputs(&staged, entity.entity_ref, &catalog.vocabulary).is_ok()
                    } else {
                        s.inputs(task, entity.entity_ref, &catalog.vocabulary).is_ok()
                    }
                }
                && !s.contract.output_map.produces.keys().all(|kind| {
                    crate::catalog::fact_value(&task.facts, entity.entity_ref, kind)
                        .is_ok_and(|v| v.is_some())
                })
        })
        .collect();
    candidates.sort_by_key(|s| (s.total(), s.id));
    let mut seen = BTreeSet::new();
    candidates
        .into_iter()
        .filter(|s| seen.insert(s.endpoint_id))
        .take(4)
        .map(|s| RelatedService {
            service_ref: s.id,
            entity_ref: entity.entity_ref,
            title: title(s),
            provider: match catalog.known_sources.iter().find(|source| source.slug == s.api_slug)
                .and_then(|source| source.metadata.provider_slug.as_deref())
                .and_then(crate::sources::grouped_provider) {
                Some(group) => format!("{} · {}", group.name, s.name),
                None => s.name.clone(),
            },
            subject: entity.label.clone(),
            total_atomic: s.total().to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ConversationTurn, Entity};

    #[test]
    fn topical_filing_followup_offers_filingpulse_with_exact_question() {
        let snapshot = serde_json::from_str(include_str!("../tests/fixtures/source-operations/catalog.json")).unwrap();
        let catalog = Catalog::build(snapshot);
        let previous = catalog.services.iter().find(|s| s.api_slug == "companies-house").unwrap();
        let filing = catalog.services.iter().find(|s| s.api_slug == "filingspulse" && s.path == "/api/filings/search").unwrap();
        let entity = Uuid::new_v4();
        let mut task = Task::default();
        task.entities.push(Entity { entity_ref: entity, label: "Apple Inc".into(), selected: true });
        task.result_refs.push(Uuid::new_v4());
        task.completed.push(format!("{}:{entity}", previous.id));
        task.conversation.push(ConversationTurn { question: "What do Apple Inc filings say about China supply chain risk?".into(), reply: None, output: None });
        let suggestions = related_services(&catalog, &task);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].service_ref, filing.id);
        assert_eq!(suggestions[0].provider, "Pulse Network · FilingsPulse");
        bind_filing_question(&catalog, &mut task, filing, entity).unwrap();
        assert_eq!(filing.inputs(&task, entity, &catalog.vocabulary).unwrap()["query"], "What do Apple Inc filings say about China supply chain risk?");
        task.conversation.last_mut().unwrap().question = "Filings from Apple Inc".into();
        assert!(related_services(&catalog, &task).is_empty());
    }
}
