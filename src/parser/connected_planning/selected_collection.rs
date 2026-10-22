//! Explicit source review may retrieve a documented collection while retaining
//! the original requested output as a gap. A publication window must be a real
//! bound source parameter, never an adjective applied to an unfiltered page.
use super::{compile_scoped, collections, selected::ExplicitValues, Prepared};
use crate::{ask_v2::input_values, capability_intake::Intake, catalog::{Catalog, Service}, state::Task};
use serde_json::Value;

fn tokens(text: &str) -> Vec<String> {
    text.split(|ch: char| !ch.is_alphanumeric()).filter(|word| !word.is_empty()).map(str::to_lowercase).collect()
}

fn collection(schema: &Value) -> bool {
    (schema["type"] == "array" && (schema["items"]["type"] == "object" || schema["items"]["properties"].is_object()))
        || schema["properties"].as_object().into_iter().flatten().any(|(_, schema)| collection(schema))
}

fn days(question: &str) -> Option<u32> {
    let words = tokens(question);
    if words.iter().any(|word| matches!(word.as_str(), "open" | "active" | "awarded" | "openstaande" | "actieve" | "gegunde"
        | "today" | "yesterday" | "vandaag" | "gisteren" | "week" | "month" | "maand")
        || word.parse::<u32>().is_ok_and(|year| (1900..=2100).contains(&year))) { return None; }
    let windows: Vec<_> = words.windows(3).filter_map(|part| {
        (matches!(part[0].as_str(), "last" | "past" | "afgelopen" | "laatste")
            && matches!(part[2].as_str(), "day" | "days" | "dag" | "dagen"))
            .then(|| part[1].parse::<u32>().ok()).flatten().filter(|days| (1..=366).contains(days))
    }).collect();
    (windows.len() == 1).then(|| windows[0])
}

// Return only declared fields. Provider names, enum codes and window lengths
// are read from the catalog; no TenderNed-specific transformation lives here.
fn reviewed_fields(question: &str, catalog: &Catalog, part: &Intake, service: &Service) -> Option<(String, String, Value)> {
    let days = days(question)?;
    let cap = part.parsed_request.capabilities.first()?;
    if cap.domain != service.contract.capability_slug.split('.').next()?
        || !service.contract.output_map.produces.keys().any(|field| catalog.vocabulary.get(field).is_some_and(|fact| collection(&fact.value_schema)))
        || !collections::newest_publications(service, catalog) { return None; }
    let endpoint = catalog.known_sources.iter().find(|source| source.slug == service.api_slug)?
        .metadata.endpoints.iter().find(|endpoint| endpoint.endpoint_id == service.endpoint_id)?;
    let params = endpoint.registration.as_ref()?.query_params.as_ref()?;
    let params = params.get("properties").unwrap_or(params);
    let keyword = service.contract.requires.iter().find(|input| service.contract.input_map.contains_key(&input.fact_type)
        && catalog.vocabulary.get(&input.fact_type).is_some_and(|fact| ["sector", "keyword"].iter().any(|alias|
            input_values::reviewed_input_alias(&fact.value_schema, alias))))?.fact_type.clone();
    for input in &service.contract.requires {
        let binding = service.contract.input_map.get(&input.fact_type)?;
        let parameter = &params[&binding.name];
        let description = parameter["description"].as_str().unwrap_or("");
        if !description.to_lowercase().contains("publication date") { continue; }
        let words = tokens(description);
        for value in parameter["enum"].as_array().into_iter().flatten() {
            let code = value.as_str()?.to_lowercase();
            let Some(index) = words.iter().position(|word| word == &code) else { continue; };
            let section = &words[index + 1..words.len().min(index + 8)];
            let matches = section.windows(3).any(|part| part[0] == "last" && part[1].parse::<u32>().ok() == Some(days)
                && matches!(part[2].as_str(), "day" | "days"));
            if matches && catalog.vocabulary.get(&input.fact_type).is_some_and(|fact|
                catalog_core::facts::schema::validate(&fact.value_schema, value).is_ok()) {
                return Some((keyword, input.fact_type.clone(), value.clone()));
            }
        }
    }
    None
}

pub(super) fn feedback(question: &str, catalog: &Catalog, part: &Intake, service: &Service) -> Option<String> {
    let (keyword, window, expected) = reviewed_fields(question, catalog, part, service)?;
    let subject = part.parsed_request.subjects.first()?;
    let bound = |field: &str| catalog.vocabulary.get(field)
        .and_then(|fact| input_values::documented_literal(field, &fact.value_schema, subject));
    let mut missing = Vec::new();
    if !bound(&keyword).is_some_and(|value| value.as_str().is_some_and(|text| !text.trim().is_empty())) { missing.push(keyword); }
    if bound(&window).as_ref() != Some(&expected) { missing.push(format!("{window}={} (the documented publication window requested)", expected)); }
    (!missing.is_empty()).then(|| format!("Enter {} for {} before reviewing the selected sources. The request will not run without its keyword and publication window.", missing.join(", "), service.name))
}

pub(super) fn contribution(question: &str, task: &Task, catalog: &Catalog, part: &Intake, service: &Service, explicit: &ExplicitValues) -> Option<Prepared> {
    let (keyword, window, _) = reviewed_fields(question, catalog, part, service)?;
    if feedback(question, catalog, part, service).is_some() { return None; }
    let mut native = part.clone();
    let cap = native.parsed_request.capabilities.first_mut()?;
    cap.slug = service.contract.capability_slug.clone();
    for field in [keyword, window] { if !cap.inputs.contains(&field) { cap.inputs.push(field); } }
    cap.outputs = service.contract.output_map.produces.keys().cloned().collect();
    native.matches[0].slug = cap.slug.clone();
    let mut prepared = compile_scoped(question, task, catalog, &native, Some(explicit))?;
    prepared.coverage.limitations.push(format!("{} returns one bounded page, newest first, with the explicitly bound keyword and publication window. This is not complete coverage of all requested regions or notice types. Requested scope: {}. Source coverage: {}", service.name, part.parsed_request.capabilities[0].description, service.description.as_deref().unwrap_or("See the selected source documentation.")));
    Some(prepared)
}
