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
        _ => service
            .contract
            .capability_slug
            .split_once('.')
            .map_or(service.contract.capability_slug.as_str(), |(_, tail)| tail),
    };
    label.replace(['.', '_', '-'], " ")
}

/// Only suggest immediately bindable services on the last completed subject.
/// This reads catalog contracts and saved facts; it never calls a provider.
pub fn related_services(catalog: &Catalog, task: &Task) -> Vec<RelatedService> {
    if task.result_refs.is_empty()
        || task.cancelled
        || task.execution_error.is_some()
        || !task.pending_candidates.is_empty()
        || !task.actions.is_empty()
    {
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
    let mut candidates: Vec<_> = catalog
        .services
        .iter()
        .filter(|s| {
            !used_endpoints.contains(&s.endpoint_id)
                && s.contract
                    .requires
                    .iter()
                    .any(|r| related_types.contains(r.fact_type.as_str()))
                && s.inputs(task, entity.entity_ref, &catalog.vocabulary)
                    .is_ok()
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
            provider: s.name.clone(),
            subject: entity.label.clone(),
            total_atomic: s.total().to_string(),
        })
        .collect()
}
