//! Curated discovery groups, separate from priced execution contracts.
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

#[derive(Debug, Deserialize, Serialize)]
pub struct Capability {
    pub slug: String,
    pub name: String,
    pub primary_sources: Vec<String>,
    pub supplementary_sources: Vec<String>,
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
    Some(crate::parser::ChatReply {
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
