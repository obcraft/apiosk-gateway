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
    words(&capability.slug).contains("search")
        && capability.inputs.iter().any(|input| {
            let input = words(input);
            input.contains("name") && !input.is_disjoint(&domain)
        })
        && capability.outputs.iter().any(|output| {
            let output = words(output);
            !output.is_disjoint(&domain)
                && output
                    .iter()
                    .any(|word| matches!(word.as_str(), "match" | "result"))
                && output.iter().all(|word| {
                    domain.contains(word) || matches!(word.as_str(), "match" | "result" | "search")
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
