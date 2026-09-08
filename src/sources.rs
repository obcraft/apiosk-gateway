use crate::catalog::Catalog;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Source {
    pub slug: String,
    #[serde(default)]
    pub provider_slug: Option<String>,
    #[serde(default)]
    pub logo_url: Option<String>,
    pub name: String,
    pub description: String,
    pub category: String,
    #[serde(deserialize_with = "labels")]
    pub tags: Vec<String>,
    #[serde(deserialize_with = "labels")]
    pub sectors: Vec<String>,
    pub endpoint_count: i64,
}
pub fn safe_logo(value: &str) -> Option<String> {
    let url = reqwest::Url::parse(value).ok()?;
    (value.len() <= 2048 && url.scheme() == "https" && url.host_str().is_some()
        && url.username().is_empty() && url.password().is_none()).then(|| url.to_string())
}

pub fn apply_branding(sources: &mut [Source], directory: Option<&Value>) {
    for source in sources {
        let provider = directory.and_then(|d| d["providers"].as_array()).and_then(|rows|
            rows.iter().find(|p| source.provider_slug.as_deref().is_some_and(|slug| p["username"].as_str() == Some(slug))));
        source.logo_url = source.logo_url.as_deref().and_then(safe_logo)
            .or_else(|| provider.and_then(|p| p["avatar_url"].as_str()).and_then(safe_logo));
    }
}

pub fn brand_result(result: &mut Value, sources: &[Source]) {
    let Some(slug) = result["source"]["provider"].as_str() else { return; };
    let Some(source) = sources.iter().find(|s| s.slug == slug) else { return; };
    result["source"]["name"] = json!(source.name);
    result["source"]["provider_slug"] = json!(source.provider_slug);
    result["source"]["logo_url"] = json!(source.logo_url);
}

fn labels<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let values = Vec::<Value>::deserialize(d)?;
    Ok(values
        .into_iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .filter(|s| !s.is_empty() && s.chars().count() <= 80)
        .take(32)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceQuery {
    pub search: Option<String>,
    pub category: Option<String>,
    pub tag: Option<String>,
    pub sector: Option<String>,
    pub capability: Option<String>,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}
impl SourceQuery {
    pub fn valid(&self) -> bool {
        [
            &self.search,
            &self.category,
            &self.tag,
            &self.sector,
            &self.capability,
        ]
        .iter()
        .all(|v| v.as_ref().is_none_or(|s| s.chars().count() <= 200))
            && self.limit.is_none_or(|n| (1..=50).contains(&n))
    }
}
fn selected(filter: &Option<String>, values: impl IntoIterator<Item = impl AsRef<str>>) -> bool {
    filter.as_ref().is_none_or(|f| {
        f.trim().is_empty()
            || values
                .into_iter()
                .any(|v| v.as_ref().eq_ignore_ascii_case(f.trim()))
    })
}
pub fn browse(sources: &[Source], catalog: &Catalog, query: &SourceQuery) -> Value {
    let categories: BTreeSet<_> = sources
        .iter()
        .map(|s| &s.category)
        .filter(|s| !s.is_empty())
        .collect();
    let tags: BTreeSet<_> = sources.iter().flat_map(|s| &s.tags).collect();
    let sectors: BTreeSet<_> = sources.iter().flat_map(|s| &s.sectors).collect();
    let capabilities_for = |slug: &str| {
        catalog
            .services
            .iter()
            .filter(|s| s.api_slug == slug)
            .map(|s| s.contract.capability_slug.clone())
            .collect::<BTreeSet<_>>()
    };
    let capabilities: BTreeSet<_> = sources
        .iter()
        .flat_map(|s| capabilities_for(&s.slug))
        .collect();
    let search = query
        .search
        .as_deref()
        .unwrap_or_default()
        .to_lowercase()
        .replace(['.', '_'], " ");
    let ignored = [
        "a",
        "an",
        "and",
        "for",
        "in",
        "information",
        "of",
        "on",
        "or",
        "the",
        "to",
        "with",
    ];
    let terms: Vec<_> = search
        .split_whitespace()
        .filter(|term| term.len() > 1 && !ignored.contains(term))
        .collect();
    let mut matches: Vec<_> = sources
        .iter()
        .enumerate()
        .filter(|s| {
            let s = s.1;
            let haystack = format!(
                "{} {} {} {} {} {} {}",
                s.name,
                s.slug,
                s.description,
                s.category,
                s.tags.join(" "),
                s.sectors.join(" "),
                capabilities_for(&s.slug)
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .replace(['.', '_'], " ")
            )
            .to_lowercase();
            (terms.is_empty() || terms.iter().any(|term| haystack.contains(term)))
                && selected(&query.category, [&s.category])
                && selected(&query.tag, &s.tags)
                && selected(&query.sector, &s.sectors)
                && selected(&query.capability, capabilities_for(&s.slug))
        })
        .map(|(index, source)| {
            let haystack = format!(
                "{} {} {} {} {} {} {}",
                source.name, source.slug, source.description, source.category,
                source.tags.join(" "), source.sectors.join(" "),
                capabilities_for(&source.slug).into_iter().collect::<Vec<_>>().join(" ").replace(['.', '_'], " ")
            )
            .to_lowercase();
            let score = terms
                .iter()
                .filter(|term| haystack.contains(**term))
                .count();
            (score, index, source)
        })
        .collect();
    matches.sort_by_key(|(score, index, _)| (std::cmp::Reverse(*score), *index));
    let offset = query.offset.unwrap_or(0);
    let limit = query.limit.unwrap_or(20).clamp(1, 50);
    let page: Vec<_> = matches.iter().skip(offset).take(limit).map(|(_, _, s)| {
        let services: Vec<_> = catalog.services.iter().filter(|service| service.api_slug == s.slug).collect();
        let capabilities: BTreeSet<_> = services.iter().map(|s| &s.contract.capability_slug).collect();
        let inputs: BTreeSet<_> = services.iter().flat_map(|s| s.contract.requires.iter().filter(|r| !r.optional).map(|r| &r.fact_type)).collect();
        json!({"slug":s.slug,"provider_slug":s.provider_slug,"logo_url":s.logo_url,"name":s.name,"description":s.description,"category":s.category,
            "tags":s.tags,"sectors":s.sectors,"endpoint_count":s.endpoint_count,
            "available_in_v2":!services.is_empty(),"capabilities":capabilities,"input_types":inputs})
    }).collect();
    let end = offset.saturating_add(page.len());
    json!({"protocol_version":"2","sources":page,"total":matches.len(),"catalog_total":sources.len(),
        "offset":offset,"next_offset":if end < matches.len() {Some(end)} else {None},
        "categories":categories,"tags":tags,"sectors":sectors,"capabilities":capabilities,
        "notice":"Browsing is free. Catalog descriptions help choose a source; Apiosk checks the exact question and price before any purchase."})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn catalog() -> Catalog {
        Catalog::build(
            serde_json::from_str(include_str!("../tests/fixtures/integration-catalog.json"))
                .unwrap(),
        )
    }
    fn rows() -> Vec<Source> {
        serde_json::from_value(json!([
            {"slug":"one","name":"Company data","description":"Annual filings","category":"finance","tags":["registry",null,12],"sectors":["business"],"endpoint_count":2},
            {"slug":"two","name":"Weather","description":"Forecasts","category":"weather","tags":[],"sectors":[],"endpoint_count":1}
        ])).unwrap()
    }
    #[test]
    fn source_query_prefers_the_listings_own_logo() {
        let sql = include_str!("sources.sql");
        assert!(sql.contains("coalesce(nullif(a.listing_metadata->>'logo',''),nullif(p.avatar_url,''))"));
    }
    #[test]
    fn branding_matches_provider_identity_and_enriches_old_results() {
        let mut rows = rows();
        rows[0].provider_slug = Some("registry".into());
        rows[1].logo_url = Some("javascript:alert(1)".into());
        apply_branding(&mut rows, Some(&json!({"providers":[
                {"username":"other","avatar_url":"https://example.com/wrong.png"},
                {"username":"registry","avatar_url":"https://example.com/registry.png"}
        ]})));
        assert_eq!(rows[0].logo_url.as_deref(), Some("https://example.com/registry.png"));
        assert!(rows[1].logo_url.is_none());
        let mut result = json!({"source":{"provider":"one","endpoint_id":"original"},"data":{"value":42}});
        brand_result(&mut result, &rows);
        assert_eq!(result["source"]["logo_url"], "https://example.com/registry.png");
        assert_eq!(result["source"]["name"], "Company data");
        assert_eq!(result["source"]["endpoint_id"], "original");
        assert_eq!(result["data"]["value"], 42);
        assert!(safe_logo("https://user:password@example.com/logo.png").is_none());
        rows[0].logo_url = Some("https://example.com/kvk.png".into());
        apply_branding(&mut rows, Some(&json!({"providers":[{"username":"registry","avatar_url":"https://example.com/apiosk.png"}]})));
        assert_eq!(rows[0].logo_url.as_deref(), Some("https://example.com/kvk.png"));
    }
    #[test]
    fn searches_metadata_filters_and_paginates_without_claiming_executability() {
        let rows = rows();
        let c = catalog();
        let first = browse(
            &rows,
            &c,
            &SourceQuery {
                limit: Some(1),
                ..Default::default()
            },
        );
        assert_eq!(first["total"], 2);
        assert_eq!(first["next_offset"], 1);
        assert_eq!(first["sources"][0]["available_in_v2"], false);
        assert_eq!(first["sources"][0]["tags"], json!(["registry"]));
        let last = browse(
            &rows,
            &c,
            &SourceQuery {
                offset: Some(1),
                ..Default::default()
            },
        );
        assert!(last["next_offset"].is_null());
        let filtered = browse(
            &rows,
            &c,
            &SourceQuery {
                search: Some("BUSINESS filings".into()),
                tag: Some("REGISTRY".into()),
                category: Some("finance".into()),
                ..Default::default()
            },
        );
        assert_eq!(filtered["total"], 1);
        assert_eq!(filtered["catalog_total"], 2);
        assert_eq!(filtered["categories"], json!(["finance", "weather"]));
        let natural = browse(
            &rows,
            &c,
            &SourceQuery {
                search: Some("weather forecasts and annual information".into()),
                ..Default::default()
            },
        );
        assert_eq!(natural["total"], 2);
        assert_eq!(natural["sources"][0]["name"], "Weather");
        let empty = browse(
            &rows,
            &c,
            &SourceQuery {
                search: Some("invented".into()),
                ..Default::default()
            },
        );
        assert_eq!(empty["sources"], json!([]));
        assert!(empty["next_offset"].is_null());
        let overflow = browse(
            &rows,
            &c,
            &SourceQuery {
                offset: Some(usize::MAX),
                ..Default::default()
            },
        );
        assert!(overflow["next_offset"].is_null());
    }
    #[test]
    fn only_validated_contracts_make_a_source_available() {
        let c = catalog();
        let mut rows = rows();
        rows[0].slug = c.services[0].api_slug.clone();
        let r = browse(&rows, &c, &SourceQuery::default());
        assert_eq!(r["sources"][0]["available_in_v2"], true);
        assert!(!r["sources"][0]["capabilities"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(!SourceQuery {
            limit: Some(51),
            ..Default::default()
        }
        .valid());
        assert!(!SourceQuery {
            search: Some("x".repeat(201)),
            ..Default::default()
        }
        .valid());
    }
}
