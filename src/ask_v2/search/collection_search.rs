//! Generic entity searches need contract evidence even when every requested
//! word (company, name, search, match) is otherwise a lexical stop word.
use super::words;
use crate::{
    capability_intake::Capability,
    catalog::{Catalog, Service},
};
use catalog_core::facts::contract::Cardinality;
use std::collections::BTreeSet;

pub(super) fn requested(capability: &Capability) -> bool {
    let domain = words(&capability.domain);
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
        && capability.outputs.iter().any(|output| {
            let tokens = super::tokens(output);
            let plural_entity = tokens.len() == 1
                && super::singular(&tokens[0]).is_some_and(|singular|
                    singular != tokens[0] && domain.contains(&singular));
            let output = words(output);
            !output.is_disjoint(&domain)
                && (plural_entity || output.iter().any(|word|
                    matches!(word.as_str(), "match" | "matching" | "result" | "list")))
                && output.iter().all(|word| {
                    domain.contains(word) || matches!(word.as_str(), "match" | "matching" | "result" | "search" | "list")
                })
        })
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
