//! Generic entity searches need contract evidence even when every requested
//! word (company, name, search, match) is otherwise a lexical stop word.
use super::words;
use crate::{
    capability_intake::{Capability, ParsedRequest},
    catalog::{Catalog, Service},
};
use catalog_core::facts::contract::Cardinality;
use std::collections::BTreeSet;

#[cfg(test)]
pub(super) fn requested(capability: &Capability) -> bool {
    requested_in_domain(capability, &capability.domain)
}

/// A generic parser bucket must not hide the entity explicitly identified by
/// both the requested collection and its linked subject. Recover only that
/// narrow name-search scope; a subject alone never changes a request's domain.
pub(super) fn request_domain(capability: &Capability, parsed: &ParsedRequest) -> Option<String> {
    let domain = if capability.domain.eq_ignore_ascii_case("general") {
        let subject = parsed.subjects.iter().find(|subject| capability.subject_id.as_ref() == Some(&subject.id))?;
        let mut kinds = words(&subject.kind).into_iter();
        let kind = kinds.next()?;
        if kinds.next().is_some() { return None; }
        kind
    } else {
        capability.domain.to_lowercase()
    };
    requested_in_domain(capability, &domain).then_some(domain)
}

fn requested_in_domain(capability: &Capability, domain: &str) -> bool {
    let domain = words(domain);
    let collection_output = |output: &String| {
        let tokens = super::tokens(output);
        let output = words(output);
        // A category may be singular in an interpreted search. The admitted
        // contract, not the spelling of the output, proves cardinality.
        let bare_entity = tokens.len() == 1 && !output.is_disjoint(&domain);
        !output.is_disjoint(&domain)
            && (bare_entity || output.iter().any(|word|
                matches!(word.as_str(), "match" | "matching" | "result" | "record" | "list" | "collection")))
            && output.iter().all(|word| {
                domain.contains(word) || matches!(word.as_str(), "match" | "matching" | "result" | "record" | "search" | "list" | "collection")
            })
    };
    let identity_output = |output: &String| {
        // General lexical matching drops two-letter tokens. Keep an explicit
        // ID token here, and reject other unknown short qualifiers rather than
        // silently erasing them from the requested identity field.
        let output: Option<BTreeSet<_>> = super::tokens(output).iter().map(|word|
            if matches!(word.as_str(), "id" | "ids") { Some("identifier".to_owned()) }
            else { super::singular(word) }).collect();
        let Some(output) = output else { return false; };
        !output.is_disjoint(&domain)
            && output.iter().any(|word| matches!(word.as_str(), "name" | "id" | "identifier"))
            && output.iter().all(|word| domain.contains(word) || matches!(word.as_str(), "name" | "id" | "identifier"))
    };
    // The parser's identifiers describe supplied values, not contract field
    // names. `name` and `<domain>_name` are the same criterion here; discovery
    // must not depend on the model choosing one spelling. The operation and
    // requested collection still establish the entity scope independently.
    let operation = words(&format!("{} {}", capability.slug, capability.name));
    operation.contains("search")
        && capability.inputs.iter().any(|input| {
            let input = words(input);
            input.contains("name")
                && input.iter().all(|word| word == "name" || domain.contains(word))
        })
        && (capability.outputs.iter().any(collection_output)
            // Names and identifiers can describe the same requested search
            // results without an extra collection label. Require every output
            // to stay within identity scope so a count, rating or profile is
            // never silently replaced by a generic name search.
            || (!capability.outputs.is_empty() && capability.outputs.iter().all(identity_output)))
}

pub(super) fn domains(service: &Service, catalog: &Catalog) -> BTreeSet<String> {
    if service.unverified
        || service.contract.capability_slug.starts_with("operation.")
        || !words(&service.contract.capability_slug).contains("search")
        || service.contract.cardinality != Cardinality::Many
    {
        return BTreeSet::new();
    }
    let name_domain = |fact: &str| -> BTreeSet<String> {
        let Some((namespace, field)) = fact.rsplit_once('.') else {
            return BTreeSet::new();
        };
        if field != "name"
            || !catalog
                .vocabulary
                .get(fact)
                .is_some_and(|definition| definition.value_schema["type"] == "string")
        {
            return BTreeSet::new();
        }
        words(namespace)
    };
    let inputs: BTreeSet<_> = service
        .contract
        .requires
        .iter()
        .flat_map(|required| name_domain(&required.fact_type))
        .collect();
    let outputs: BTreeSet<_> = service
        .contract
        .output_map
        .produces
        .keys()
        .flat_map(|fact| name_domain(fact))
        .collect();
    inputs.intersection(&outputs).cloned().collect()
}

#[cfg(test)]
mod tests;
