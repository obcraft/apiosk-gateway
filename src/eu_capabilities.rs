//! Curated discovery groups, separate from priced execution contracts.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::LazyLock;

#[derive(Debug, Deserialize, Serialize)]
pub struct Capability {
    pub slug: String,
    pub name: String,
    pub primary_sources: Vec<String>,
    pub supplementary_sources: Vec<String>,
    /// Exact runtime capabilities admitted for each source in this group.
    #[serde(default)]
    pub source_operations: BTreeMap<String, Vec<String>>,
}

pub fn definitions() -> &'static [Capability] {
    static DEFINITIONS: LazyLock<Vec<Capability>> = LazyLock::new(|| {
        serde_json::from_str(include_str!("../contracts/eu-capabilities.json"))
            .expect("valid bundled European capability definitions")
    });
    &DEFINITIONS
}

pub fn find(slug: &str) -> Option<&'static Capability> {
    definitions()
        .iter()
        .find(|c| c.slug.eq_ignore_ascii_case(slug.trim()))
}

/// Country may satisfy a source's explicit country-code fact, but must never
/// be treated as a registration number or a dataset/metric choice.
pub fn enriched_input(group: &Capability, country: Option<&str>, input: &BTreeMap<String, serde_json::Value>) -> BTreeMap<String, serde_json::Value> {
    let mut result = input.clone();
    if let Some(country) = country {
        if group.slug == "eu.company.profile" {
            result.entry("company.country_code".into()).or_insert_with(|| serde_json::json!(country));
            result.entry("company.search.country_code".into()).or_insert_with(|| serde_json::json!(country));
            if input.contains_key("vat.eu.number") {
                result.entry("vat.eu.country_code".into()).or_insert_with(|| serde_json::json!(country));
            }
        } else if group.slug == "eu.statistics.query" && input.contains_key("eurostat.input.dataset") {
            result.entry("eurostat.input.geo".into()).or_insert_with(|| serde_json::json!(eurostat_geo(country)));
        }
    }
    result
}

/// Eurostat uses EL and UK where ISO country input uses GR and GB.
pub fn eurostat_geo(country: &str) -> &str {
    match country {
        "GR" => "EL",
        "GB" => "UK",
        _ => country,
    }
}

impl Capability {
    pub fn role(&self, source: &str) -> Option<&'static str> {
        if self.primary_sources.iter().any(|slug| slug == source) {
            Some("primary")
        } else if self.supplementary_sources.iter().any(|slug| slug == source) {
            Some("supplementary")
        } else {
            None
        }
    }
}

/// The conversation sees the same groups, but only sources in its live catalog.
pub fn catalog_view(catalog: &crate::catalog::Catalog) -> serde_json::Value {
    let sources = |slugs: &[String]| {
        slugs
            .iter()
            .filter_map(|slug| {
                let name = catalog
                    .known_sources
                    .iter()
                    .find(|s| s.slug == *slug)
                    .map(|s| &s.name)
                    .or_else(|| {
                        catalog
                            .services
                            .iter()
                            .find(|s| s.api_slug == *slug)
                            .map(|s| &s.name)
                    })?;
                Some(serde_json::json!({"slug":slug,"name":name}))
            })
            .collect::<Vec<_>>()
    };
    serde_json::json!(definitions()
        .iter()
        .map(|c| serde_json::json!({
            "slug":c.slug,"name":c.name,"primary_sources":sources(&c.primary_sources),
            "supplementary_sources":sources(&c.supplementary_sources)
        }))
        .collect::<Vec<_>>())
}

/// A bare group identifier asks for its sources, not a purchase or a guessed subject.
pub fn browse_reply(
    question: &str,
    catalog: &crate::catalog::Catalog,
) -> Option<crate::parser::ChatReply> {
    let definition = find(question)?;
    let view = catalog_view(catalog);
    let group = view
        .as_array()?
        .iter()
        .find(|c| c["slug"] == definition.slug)?;
    let names = |field: &str| {
        group[field]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|source| source["name"].as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let primary = names("primary_sources");
    let extra = names("supplementary_sources");
    Some(crate::parser::ChatReply { output_bindings: vec![],
        message: format!(
            "{} — Primary sources: {}.{} This is a source overview; no data call has been run.",
            definition.slug,
            if primary.is_empty() {
                "none currently published"
            } else {
                &primary
            },
            if extra.is_empty() {
                String::new()
            } else {
                format!(" Supplementary sources: {extra}.")
            }
        ),
        discover: false,
        intent: None,
        decline_reason: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn curated_operations_belong_to_declared_sources() {
        for group in definitions() {
            let members: std::collections::BTreeSet<_> = group.primary_sources.iter().chain(&group.supplementary_sources).collect();
            let routed: std::collections::BTreeSet<_> = group.source_operations.keys().collect();
            assert_eq!(members, routed, "{} has an unrouted or undeclared source", group.slug);
            for operations in group.source_operations.values() {
                assert!(!operations.is_empty());
                assert_eq!(operations.iter().collect::<std::collections::BTreeSet<_>>().len(), operations.len());
            }
        }
    }
    #[test]
    fn country_routes_to_eurostat_geo_without_guessing_a_dataset() {
        let group = find("eu.statistics.query").unwrap();
        assert!(enriched_input(group, Some("NL"), &BTreeMap::new()).is_empty());
        let input = BTreeMap::from([("eurostat.input.dataset".into(), serde_json::json!("nama_10_gdp"))]);
        assert_eq!(enriched_input(group, Some("NL"), &input)["eurostat.input.geo"], "NL");
        assert_eq!(enriched_input(group, Some("GR"), &input)["eurostat.input.geo"], "EL");
        assert_eq!(enriched_input(group, Some("GB"), &input)["eurostat.input.geo"], "UK");
    }
    #[test]
    fn bare_capability_browsing_preserves_visibility_and_never_proposes_a_purchase() {
        let mut catalog = crate::catalog::Catalog::build(
            serde_json::from_str(include_str!("../tests/fixtures/integration-catalog.json"))
                .unwrap(),
        );
        catalog.known_sources = ["ted", "tenderned", "sirenic"]
            .iter()
            .map(|slug| crate::catalog::KnownSource {
                slug: (*slug).into(),
                name: (*slug).into(),
                block_reason: None,
                metadata: Default::default(),
            })
            .collect();
        let reply = browse_reply(" EU.TENDERS.SEARCH ", &catalog).unwrap();
        assert!(!reply.discover);
        assert!(reply.intent.is_none());
        assert!(reply.message.contains("Primary sources: ted, tenderned."));
        assert!(reply.message.contains("Supplementary sources: sirenic."));
        assert!(!reply.message.contains("eu-funding-tenders"));
        assert!(browse_reply("eu.tenders.search for Rotterdam", &catalog).is_none());
        let empty = browse_reply("eu.statistics.query", &catalog).unwrap();
        assert!(empty.message.contains("none currently published"));
    }
}
