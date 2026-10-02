//! Real stored PDOK contracts: the address search and its downstream records
//! were captured in separate fixture catalogs. No live result is implied.
use crate::catalog::{Catalog, Snapshot};
use std::collections::BTreeMap;

pub fn locations() -> Catalog {
    let mut services = BTreeMap::new();
    let mut vocabulary = BTreeMap::new();
    for text in [include_str!("../../tests/fixtures/workflows/catalog.json"), include_str!("../../tests/fixtures/dutch-links/catalog.json")] {
        let snapshot: Snapshot = serde_json::from_str(text).unwrap();
        for service in snapshot.services.into_iter().filter(|s| s.api_slug == "pdok") {
            services.insert(service.contract.capability_slug.clone(), service);
        }
        for fact in snapshot.vocabulary { vocabulary.insert(fact.slug.clone(), fact); }
    }
    Catalog::build(Snapshot { services: services.into_values().collect(), vocabulary: vocabulary.into_values().collect(), known_sources: vec![] })
}
