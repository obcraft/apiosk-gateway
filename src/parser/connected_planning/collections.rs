//! Equivalent collection labels are grounded in the admitted source schema.
//! A temporal adjective is accepted only with documented newest-first order;
//! it never becomes an invented publication window or an open-tender claim.
use crate::{capability_intake::Intake, catalog::{Catalog, Service}};
use serde_json::Value;

fn words(value: &str) -> Vec<String> {
    value.split(|ch:char| !ch.is_alphanumeric()).filter(|word| !word.is_empty())
        .map(|word| word.to_lowercase().trim_end_matches('s').to_owned()).collect()
}

fn arrays(schema: &Value, names: &mut Vec<Vec<String>>) -> bool {
    let collection = schema["type"] == "array" && (schema["items"]["type"] == "object"
        || schema["items"]["properties"].is_object());
    if collection {
        if let Some(aliases) = schema["$comment"].as_str().and_then(|text| serde_json::from_str::<Value>(text).ok())
            .and_then(|value| value["apiosk_output_aliases_v1"].as_array().cloned()) {
            names.extend(aliases.iter().filter_map(Value::as_str).map(words));
        }
    }
    schema["properties"].as_object().into_iter().flatten()
        .fold(collection, |found, (_, child)| arrays(child, names) || found)
}

pub(super) fn newest_publications(service: &Service, catalog: &Catalog) -> bool {
    if !["size", "limit", "page_size"].iter().any(|field|
        service.fixed_params[*field].as_u64().is_some_and(|limit| (1..=1000).contains(&limit))) { return false; }
    let Some(endpoint) = catalog.known_sources.iter().find(|source| source.slug == service.api_slug)
        .and_then(|source| source.metadata.endpoints.iter().find(|endpoint| endpoint.endpoint_id == service.endpoint_id)) else { return false; };
    let Some(params) = endpoint.registration.as_ref().and_then(|registration| registration.query_params.as_ref()) else { return false; };
    let fields = params.get("properties").unwrap_or(params);
    service.fixed_params.as_object().into_iter().flatten().any(|(key, value)| {
        let schema = &fields[key];
        let text = schema["description"].as_str().unwrap_or("").to_lowercase();
        text.contains("publication date") && text.contains("newest first")
            && schema["enum"].as_array().is_some_and(|allowed| allowed.contains(value))
    })
}

fn recency_output(output: &str, descriptions: &[&str]) -> Option<String> {
    let category = words(output);
    let head = category.first()?;
    let modifier = |word: &str| matches!(word, "new" | "recent" | "latest" | "newest" | "nieuwe" | "nieuwste" | "recente");
    if modifier(head) { return Some(output.to_owned()); }
    // Keep the modifier attached to the requested collection. An unrelated
    // new company or a place name must not qualify every array in a request.
    descriptions.iter().any(|description| {
        let tokens: Vec<_> = description.split(|ch: char| !ch.is_alphanumeric()).filter(|word| !word.is_empty()).map(str::to_lowercase).collect();
        tokens.windows(2).any(|pair| modifier(&pair[0]) && words(&pair[1]).first() == Some(head))
    }).then(|| format!("new_{output}"))
}

/// Scope source alternatives before the fast compiler can accept an unsorted
/// contract alias for a request whose collection explicitly carries recency.
pub(super) fn constrain_recency(question: &str, intake: &mut Intake, catalog: &Catalog) -> Option<Vec<String>> {
    let mut constrained = false;
    for index in 0..intake.parsed_request.capabilities.len() {
        let cap = &intake.parsed_request.capabilities[index];
        if crate::capability_intake::answer_instruction(cap) { continue; }
        let found = super::super::explicit_request::request_match(intake, index)?;
        let services: Vec<_> = catalog.services.iter().filter(|service| found.apiosk.iter().any(|candidate|
            candidate.availability == "supported" && candidate.capability.as_deref() == Some(service.contract.capability_slug.as_str())
                && candidate.endpoint_id.as_deref() == Some(service.endpoint_id.to_string().as_str()))).collect();
        let temporal: Vec<_> = cap.outputs.iter().filter_map(|output| {
            let temporal = recency_output(output, &[&cap.name, &cap.description, question])?;
            // A collection needs its own declared shape/alias. This does not
            // turn a scalar or an opaque narrative into a newest-record call.
            services.iter().any(|service| !options("", output, &cap.domain, service, catalog).is_empty())
                .then_some(temporal)
        }).collect();
        if temporal.is_empty() { continue; }
        let supported: Vec<_> = services.into_iter().filter(|service| temporal.iter().all(|output|
            !options(question, output, &cap.domain, service, catalog).is_empty())).collect();
        if supported.is_empty() { return None; }
        let occurrence = intake.parsed_request.capabilities[..index].iter().filter(|prior| prior.slug == cap.slug).count();
        let target = intake.matches.iter_mut().filter(|matched| matched.slug == cap.slug).nth(occurrence)?;
        target.apiosk.retain(|candidate| supported.iter().any(|service|
            candidate.capability.as_deref() == Some(service.contract.capability_slug.as_str())
                && candidate.endpoint_id.as_deref() == Some(service.endpoint_id.to_string().as_str())));
        target.operations.retain(|operation| supported.iter().any(|service| &service.contract.capability_slug == operation));
        constrained = true;
    }
    if constrained {
        intake.selected_capabilities = intake.matches.iter().flat_map(|found| found.operations.iter().cloned()).collect();
        Some(vec!["The selected collection source returns one bounded page ordered by publication date, newest first. No publication window was supplied; inspect the returned dates. This does not establish that every notice is new or still open.".into()])
    } else { Some(vec![]) }
}

pub(super) fn options(question: &str, output: &str, domain: &str, service: &Service, catalog: &Catalog) -> Vec<(String, Option<String>)> {
    let mut category = words(output);
    let newest = category.first().is_some_and(|word| matches!(word.as_str(), "new" | "recent" | "latest" | "newest"));
    if newest { category.remove(0); }
    if category.last().is_some_and(|word| matches!(word.as_str(), "record" | "detail" | "profile")) { category.pop(); }
    if category.is_empty() { return vec![]; }
    // A narrowed result must not be widened to all source rows. The ordinary
    // compiler can still handle those requests when exact inputs/fields exist.
    let request = words(question);
    if request.iter().any(|word| matches!(word.as_str(), "open" | "active" | "awarded" | "openstaande" | "actieve" | "gegunde"
        | "yesterday" | "today" | "gisteren" | "vandaag" | "week" | "month" | "maand" | "day" | "dag" | "dagen")
        || (word.len()==4 && word.parse::<u16>().is_ok_and(|year| (1900..=2100).contains(&year)))) { return vec![]; }
    if newest && !newest_publications(service, catalog) { return vec![]; }
    let domain = words(domain);
    let contract_domain = words(service.contract.capability_slug.split('.').next().unwrap_or(""));
    service.contract.output_map.produces.keys().filter_map(|fact| {
        let schema = &catalog.vocabulary.get(fact)?.value_schema;
        let mut aliases = Vec::new();
        if !arrays(schema, &mut aliases) || !(aliases.contains(&category) || (category == domain && domain == contract_domain)) { return None; }
        let notice = newest.then(|| format!("{} returns one bounded page ordered by publication date, newest first. No publication window was supplied; inspect the returned dates. This does not establish that every notice is new or still open.", service.name));
        Some((fact.clone(), notice))
    }).collect()
}
