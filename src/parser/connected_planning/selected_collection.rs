//! Explicit source review may retrieve a documented collection while retaining
//! the original requested output as a gap. A publication window must be a real
//! bound source parameter, never an adjective applied to an unfiltered page.
use super::{compile_scoped, collections, selected::ExplicitValues, Gap, Prepared};
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

// Relative day counts do not identify which date the user means. Keep this
// proof tied to the original wording even when canonical intake omitted it.
fn publication_window(question: &str) -> bool {
    let words = tokens(question);
    if days(question).is_none() { return false; }
    if all_publications(question) { return true; }
    let closing = words.iter().any(|word| matches!(word.as_str(), "closing" | "closes" | "closed" | "close" | "deadline" | "deadlines"
        | "sluiting" | "sluitingsdatum" | "sluitingsdata" | "sluit" | "sluiten" | "gesloten" | "inschrijfdeadline" | "inschrijfdatum"));
    if closing { return false; }
    let marker = words.windows(3).position(|part| matches!(part[0].as_str(), "last" | "past" | "afgelopen" | "laatste")
        && part[1].parse::<u32>().is_ok() && matches!(part[2].as_str(), "day" | "days" | "dag" | "dagen"));
    let Some(mut index) = marker else { return false; };
    while index > 0 {
        index -= 1;
        match words[index].as_str() {
            "published" | "gepubliceerd" | "publicatiedatum" => return true,
            "date" if index > 0 && words[index - 1] == "publication" => return true,
            "in" | "the" | "during" | "over" | "within" | "from" | "de" | "het" | "van" => {},
            _ => return false,
        }
    }
    false
}

// Only an explicit, otherwise unqualified "all publications" request proves
// that no keyword was intended. Unknown extra words still require a criterion;
// an "all energy publications" request must not become an unfiltered page.
fn all_publications(question: &str) -> bool {
    let words = tokens(question);
    let Some(window) = days(question) else { return false; };
    words.iter().any(|word| matches!(word.as_str(), "all" | "alle"))
        && words.iter().all(|word| word.parse::<u32>().ok() == Some(window) || matches!(word.as_str(),
            "show" | "list" | "find" | "retrieve" | "return" | "get" | "all" | "the" | "from" | "in" | "over" | "last" | "past" | "published"
            | "publication" | "publications" | "notice" | "notices" | "tender" | "tenders" | "new" | "recent" | "day" | "days" | "of" | "within" | "for"
            | "toon" | "geef" | "vind" | "zoek" | "haal" | "alle" | "de" | "het" | "van" | "afgelopen" | "laatste" | "gepubliceerd"
            | "publicatie" | "publicaties" | "publicatiedatum" | "dagen" | "dag" | "aankondigingen" | "aanbestedingen" | "nieuwe" | "recente"))
}

fn source_keyword(catalog: &Catalog, part: &Intake, service: &Service) -> Option<String> {
    let cap = part.parsed_request.capabilities.first()?;
    if cap.domain != service.contract.capability_slug.split('.').next()?
        || !service.contract.output_map.produces.keys().any(|field| catalog.vocabulary.get(field).is_some_and(|fact| collection(&fact.value_schema)))
        || !collections::newest_publications(service, catalog) { return None; }
    service.contract.requires.iter().find(|input| service.contract.input_map.contains_key(&input.fact_type)
        && catalog.vocabulary.get(&input.fact_type).is_some_and(|fact| ["sector", "keyword"].iter().any(|alias|
            input_values::reviewed_input_alias(&fact.value_schema, alias)))).map(|input| input.fact_type.clone())
}

// Return only declared fields. Provider names, enum codes and window lengths
// are read from the catalog; no TenderNed-specific transformation lives here.
fn reviewed_fields(question: &str, catalog: &Catalog, part: &Intake, service: &Service) -> Option<(String, String, Value)> {
    let days = days(question)?;
    let keyword = source_keyword(catalog, part, service)?;
    let endpoint = catalog.known_sources.iter().find(|source| source.slug == service.api_slug)?
        .metadata.endpoints.iter().find(|endpoint| endpoint.endpoint_id == service.endpoint_id)?;
    let params = endpoint.registration.as_ref()?.query_params.as_ref()?;
    let params = params.get("properties").unwrap_or(params);
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
    // A location label can be the collection's geographic scope rather than a
    // search term. A fixed national source proves only that exact country;
    // a city, EU, or another country must not silently become a national page.
    if source_keyword(catalog, part, service).is_some() {
        if let Some(fixed) = crate::ask_v2::search::fixed_endpoint_country(service, None) {
            for subject in &part.parsed_request.subjects {
                if matches!(subject.kind.as_str(), "location" | "place" | "region" | "country")
                    && crate::ask_v2::search::exact_country(&subject.label).as_deref() != Some(fixed.as_str()) {
                    return Some(format!("{} is limited to {fixed} and cannot establish the requested geographic scope {}. Choose a source that covers this scope; it will not be treated as {fixed}.", service.name, subject.label));
                }
            }
        }
    }
    // Mixed publication/deadline windows must be rejected before `days` narrows
    // to one window; otherwise a native output could bypass the fallback guard.
    let words = tokens(question);
    let relative_publication_source = words.iter().any(|word| matches!(word.as_str(), "day" | "days" | "dag" | "dagen"))
        && words.iter().any(|word| matches!(word.as_str(), "last" | "past" | "afgelopen" | "laatste"))
        && source_keyword(catalog, part, service).is_some()
        && catalog.known_sources.iter().find(|source| source.slug == service.api_slug)
            .and_then(|source| source.metadata.endpoints.iter().find(|endpoint| endpoint.endpoint_id == service.endpoint_id))
            .and_then(|endpoint| endpoint.registration.as_ref()).and_then(|registration| registration.query_params.as_ref())
            .is_some_and(|params| service.contract.input_map.values().any(|binding| {
                let parameter = &params.get("properties").unwrap_or(params)[&binding.name];
                parameter["enum"].is_array() && parameter["description"].as_str().is_some_and(|description| description.to_lowercase().contains("publication date"))
            }));
    if relative_publication_source && !publication_window(question) {
        return Some("A publication-date filter does not establish the requested closing date or deadline. Clarify the date criterion and use its declared source fields before reviewing these sources.".into());
    }
    let Some((keyword, window, expected)) = reviewed_fields(question, catalog, part, service) else {
        return relative_publication_source.then(|| "The requested publication window has no exact reviewed source binding. Use a documented matching date filter before reviewing these sources.".into());
    };
    if !publication_window(question) {
        return Some(format!("{} is a publication-date filter, not proof of the requested closing date or deadline. Specify the intended date criterion and use its declared source fields before reviewing these sources.", window));
    }
    let subject = part.parsed_request.subjects.first()?;
    let bound = |field: &str| catalog.vocabulary.get(field)
        .and_then(|fact| input_values::documented_literal(field, &fact.value_schema, subject));
    let mut missing = Vec::new();
    if !all_publications(question) && !bound(&keyword).is_some_and(|value| value.as_str().is_some_and(|text| !text.trim().is_empty())) { missing.push(keyword); }
    if bound(&window).as_ref() != Some(&expected) { missing.push(format!("{window}={} (the documented publication window requested)", expected)); }
    (!missing.is_empty()).then(|| format!("Enter {} for {} before reviewing the selected sources. The request will not run without its requested criteria and publication window.", missing.join(", "), service.name))
}

// A national collection can contribute only its proved country to an
// explicitly broader regional request. This is a disclosed coverage gap, never
// a claim that a national endpoint enforces the whole requested region.
fn regional_contribution(native: &mut Intake, catalog: &Catalog, service: &Service) -> Option<Vec<Gap>> {
    let mut gaps = Vec::new();
    for subject in &mut native.parsed_request.subjects {
        let regions: Vec<_> = subject.identifiers.iter().filter(|(key, _)| matches!(key.as_str(), "region" | "regions" | "country" | "countries"))
            .map(|(key,value)| (key.clone(),value.clone())).collect();
        for (key, value) in regions {
            // A provider-owned region parameter must still be enforced normally.
            if service.contract.requires.iter().any(|input| input.fact_type == key || input.fact_type.ends_with(&format!(".{key}"))
                || catalog.vocabulary.get(&input.fact_type).is_some_and(|fact| input_values::reviewed_input_alias(&fact.value_schema, &key))) { continue; }
            let fixed = crate::ask_v2::search::fixed_endpoint_country(service, None)?;
            let joined = value.to_lowercase().replace(" and ", "|").replace(" en ", "|").replace([',',';','&','+'], "|");
            let mut overlaps = false;
            let pieces: Vec<_> = joined.split('|').map(str::trim).collect();
            if pieces.is_empty() || pieces.len() > 10 { return None; }
            for piece in pieces {
                let region = piece.strip_prefix("the ").or_else(|| piece.strip_prefix("de ")).unwrap_or(piece);
                if matches!(region, "eu" | "european union" | "europese unie" | "eea" | "eer") { continue; }
                let country = crate::ask_v2::search::exact_country(piece)?;
                overlaps |= country == fixed;
            }
            // EU membership is not inferred here. An explicitly named country
            // must overlap the contract's own fixed scope; city/unknown/excluded
            // regions cannot be widened to an entire country.
            if !overlaps { return None; }
            subject.identifiers.remove(&key);
            for cap in &mut native.parsed_request.capabilities {
                if cap.subject_id.as_ref() == Some(&subject.id) { cap.inputs.retain(|input| input != &key); }
            }
            gaps.push(Gap { subject: Some(subject.label.clone()), output: format!("Full regional coverage: {value} (selected source is limited to {fixed})") });
        }
    }
    Some(gaps)
}

// Coverage is a property of the selected source, not of which output alias
// happened to compile. A direct match must disclose the same bounded national
// page as the fallback, including when interpretation omitted a region input.
pub(super) fn scope_notice(question: &str, catalog: &Catalog, part: &Intake, service: &Service) -> Option<String> {
    reviewed_fields(question, catalog, part, service)?;
    if !publication_window(question) || feedback(question, catalog, part, service).is_some() { return None; }
    let country = crate::ask_v2::search::fixed_endpoint_country(service, None)
        .map(|country| format!(" Fixed source country: {country}; coverage outside {country} is not established."))
        .unwrap_or_default();
    Some(format!("{} returns one bounded page, newest first, with the explicitly bound requested filters and publication window. This is not complete coverage of all requested regions or notice types.{country} Requested scope: {question}. Source coverage: {}", service.name, service.description.as_deref().unwrap_or("See the selected source documentation.")))
}

pub(super) fn contribution(question: &str, task: &Task, catalog: &Catalog, part: &Intake, service: &Service, explicit: &ExplicitValues) -> Option<Prepared> {
    let (keyword, window, _) = reviewed_fields(question, catalog, part, service)?;
    if !publication_window(question) || feedback(question, catalog, part, service).is_some() { return None; }
    let mut native = part.clone();
    let regional_gaps = regional_contribution(&mut native, catalog, service)?;
    let cap = native.parsed_request.capabilities.first_mut()?;
    cap.slug = service.contract.capability_slug.clone();
    let mut fields = vec![window];
    if !all_publications(question) { fields.push(keyword); }
    for field in fields { if !cap.inputs.contains(&field) { cap.inputs.push(field); } }
    cap.outputs = service.contract.output_map.produces.keys().cloned().collect();
    native.matches[0].slug = cap.slug.clone();
    let mut prepared = compile_scoped(question, task, catalog, &native, Some(explicit))?;
    prepared.coverage.gaps.extend(regional_gaps);
    Some(prepared)
}
