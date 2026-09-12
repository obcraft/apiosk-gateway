use crate::catalog::Catalog;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Source {
    pub slug: String,
    #[serde(default)]
    pub provider_slug: Option<String>,
    #[serde(default)]
    pub logo_url: Option<String>,
    #[serde(default)]
    pub provider_logo_url: Option<String>,
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
    (value.len() <= 2048
        && url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none())
    .then(|| url.to_string())
}

pub fn apply_branding(sources: &mut [Source], directory: Option<&Value>) {
    for source in sources {
        source.provider_logo_url = source.provider_logo_url.as_deref().and_then(safe_logo);
        let provider = directory
            .and_then(|d| d["providers"].as_array())
            .and_then(|rows| {
                rows.iter().find(|p| {
                    source
                        .provider_slug
                        .as_deref()
                        .is_some_and(|slug| p["username"].as_str() == Some(slug))
                })
            });
        source.logo_url = source.logo_url.as_deref().and_then(safe_logo).or_else(|| {
            provider
                .and_then(|p| p["avatar_url"].as_str())
                .and_then(safe_logo)
        });
    }
}

pub fn brand_result(result: &mut Value, sources: &[Source]) {
    let Some(slug) = result["source"]["provider"].as_str() else {
        return;
    };
    let Some(source) = sources.iter().find(|s| s.slug == slug) else {
        return;
    };
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
/// Pulse's listings are services of one registered provider, not independent
/// sources. Match the stored owner handle, never a name/suffix heuristic: KVK,
/// CBS and the other upstreams distributed by Apiosk retain their own identity.
pub fn directory_slug(source: &Source) -> &str {
    if source.provider_slug.as_deref() == Some("pulsenetwork") {
        "pulsenetwork"
    } else {
        &source.slug
    }
}
pub fn directory_name(source: &Source) -> &str {
    if source.provider_slug.as_deref() == Some("pulsenetwork") {
        "Pulse Network"
    } else {
        &source.name
    }
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
    let mut groups: BTreeMap<&str, Vec<(usize, &Source)>> = BTreeMap::new();
    for (index, source) in sources.iter().enumerate() {
        groups
            .entry(directory_slug(source))
            .or_default()
            .push((index, source));
    }
    let catalog_total = groups.len();
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
        .filter(|t| t.len() > 1 && !ignored.contains(t))
        .collect();
    let executable: BTreeSet<&str> = catalog
        .services
        .iter()
        .map(|s| s.api_slug.as_str())
        .collect();
    let restricted: BTreeSet<&str> = catalog
        .known_sources
        .iter()
        .filter(|s| s.block_reason.is_some())
        .map(|s| s.slug.as_str())
        .collect();
    let reach = |slug: &str| {
        if restricted.contains(slug) {
            2u8
        } else if executable.contains(slug) {
            0
        } else {
            1
        }
    };
    let mut matches = Vec::new();
    for (key, members) in groups {
        let matching: Vec<_> = members
            .iter()
            .filter_map(|(index, source)| {
                let haystack = format!(
                    "{} {} {} {} {} {} {} {} {}",
                    directory_name(source),
                    key,
                    source.name,
                    source.slug,
                    source.description,
                    source.category,
                    source.tags.join(" "),
                    source.sectors.join(" "),
                    capabilities_for(&source.slug)
                        .into_iter()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .replace(['.', '_'], " ")
                )
                .to_lowercase();
                let score = terms
                    .iter()
                    .filter(|term| haystack.contains(**term))
                    .count();
                ((terms.is_empty() || score > 0)
                    && selected(&query.category, [&source.category])
                    && selected(&query.tag, &source.tags)
                    && selected(&query.sector, &source.sectors)
                    && selected(&query.capability, capabilities_for(&source.slug)))
                .then_some((score, *index, *source))
            })
            .collect();
        if matching.is_empty() {
            continue;
        }
        let score = matching
            .iter()
            .map(|(score, _, _)| *score)
            .max()
            .unwrap_or(0);
        let rank = matching
            .iter()
            .map(|(_, _, s)| reach(&s.slug))
            .min()
            .unwrap_or(2);
        let index = members[0].0;
        matches.push((score, rank, index, key, members, matching));
    }
    // Group before pagination, so a provider cannot fill a page with its services.
    matches.sort_by_key(|(score, rank, index, ..)| (std::cmp::Reverse(*score), *rank, *index));
    let offset = query.offset.unwrap_or(0);
    let limit = query.limit.unwrap_or(20).clamp(1, 50);
    let page: Vec<_> = matches.iter().skip(offset).take(limit).map(|(_, _, _, key, members, matching)| {
        let first = members[0].1;
        let slugs: Vec<_> = members.iter().map(|(_, s)| s.slug.as_str()).collect();
        let services: Vec<_> = catalog.services.iter().filter(|s| slugs.contains(&s.api_slug.as_str())).collect();
        let capabilities: BTreeSet<_> = services.iter().map(|s| &s.contract.capability_slug).collect();
        let inputs: BTreeSet<_> = services.iter().flat_map(|s| s.contract.requires.iter().filter(|r| !r.optional).map(|r| &r.fact_type)).collect();
        let grouped = *key == "pulsenetwork";
        let tags: BTreeSet<_> = members.iter().flat_map(|(_,s)| &s.tags).collect();
        let sectors: BTreeSet<_> = members.iter().flat_map(|(_,s)| &s.sectors).collect();
        let categories: BTreeSet<_> = members.iter().map(|(_,s)| &s.category).filter(|s| !s.is_empty()).collect();
        let mut row = json!({"slug":key,"provider_slug":first.provider_slug,"logo_url":if grouped {first.provider_logo_url.as_ref().or(first.logo_url.as_ref())} else {first.logo_url.as_ref()},
            "name":directory_name(first),"description":if grouped {"Data services published by Pulse Network."} else {&first.description},
            "category":if grouped {"intelligence"} else {&first.category},"categories":categories,
            "tags":tags,"sectors":sectors,"endpoint_count":members.iter().map(|(_,s)|s.endpoint_count).sum::<i64>(),
            "readiness":crate::readiness::assess(catalog, &slugs),
            "available_in_v2":!services.is_empty(),"capabilities":capabilities,"input_types":inputs});
        if grouped {
            row["service_count"] = json!(members.len());
            row["matching_service_count"] = json!(matching.len());
            row["services"] = json!(matching.iter().map(|(_,_,s)|json!({"slug":s.slug,"name":s.name,"description":s.description,"category":s.category})).collect::<Vec<_>>());
        }
        row
    }).collect();
    let end = offset.saturating_add(page.len());
    json!({"protocol_version":"2","sources":page,"total":matches.len(),"catalog_total":catalog_total,
        "offset":offset,"next_offset":if end < matches.len() {Some(end)} else {None},
        "categories":categories,"tags":tags,"sectors":sectors,"capabilities":capabilities,
        "notice":"Browsing is free. Each source is counted once; services under Pulse Network are not separate sources. Apiosk checks the exact question and price before any purchase."})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pulse_services_are_one_source_before_filtering_and_pagination() {
        let mut rows = rows();
        // A platform distributor does not turn independent upstreams into one.
        for row in &mut rows {
            row.provider_slug = Some("apiosk-basics".into());
        }
        for index in 0..87 {
            rows.push(serde_json::from_value(json!({
                "slug":format!("pulse-service-{index}"),"provider_slug":"pulsenetwork",
                "name":format!("Pulse service {index}"),"description":if index==0 {"unique tax lookup"} else {"other service"},
                "category":if index==0 {"tax"} else {"intelligence"},"tags":[],"sectors":[],"endpoint_count":2,
                "provider_logo_url":"https://example.com/pulse.png"
            })).unwrap());
        }
        let mut c = catalog();
        c.services[0].api_slug = "pulse-service-0".into();
        let first = browse(
            &rows,
            &c,
            &SourceQuery {
                limit: Some(2),
                ..Default::default()
            },
        );
        assert_eq!(first["total"], 3);
        assert_eq!(first["catalog_total"], 3);
        assert_eq!(first["next_offset"], 2);
        let pulse = &first["sources"][0];
        assert_eq!(pulse["slug"], "pulsenetwork");
        assert_eq!(pulse["name"], "Pulse Network");
        assert_eq!(pulse["service_count"], 87);
        assert_eq!(pulse["endpoint_count"], 174);
        assert_eq!(pulse["logo_url"], "https://example.com/pulse.png");
        assert_eq!(pulse["services"].as_array().unwrap().len(), 87);
        assert!(pulse["available_in_v2"].as_bool().unwrap());
        let second = browse(
            &rows,
            &c,
            &SourceQuery {
                offset: Some(2),
                limit: Some(2),
                ..Default::default()
            },
        );
        assert_eq!(second["sources"].as_array().unwrap().len(), 1);
        assert!(second["next_offset"].is_null());
        assert_ne!(second["sources"][0]["slug"], "pulsenetwork");
        for query in [
            SourceQuery {
                search: Some("Pulse Network".into()),
                ..Default::default()
            },
            SourceQuery {
                category: Some("tax".into()),
                ..Default::default()
            },
            SourceQuery {
                search: Some("unique tax lookup".into()),
                ..Default::default()
            },
        ] {
            let result = browse(&rows, &c, &query);
            assert_eq!(result["total"], 1);
            assert_eq!(result["sources"][0]["slug"], "pulsenetwork");
            assert_eq!(result["sources"][0]["service_count"], 87);
        }
        rows[0].name = "Independent Pulse".into();
        assert_eq!(directory_slug(&rows[0]), "one");
    }
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
    fn an_executable_source_outranks_an_unbuyable_one_at_equal_relevance() {
        let mut catalog = catalog();
        // Everything the runtime can execute belongs to "two". "one" is listed
        // first in the catalog and is visible for reference only.
        for service in &mut catalog.services {
            service.api_slug = "two".into();
        }
        catalog.known_sources = vec![crate::catalog::KnownSource {
            slug: "one".into(),
            name: "Company data".into(),
            block_reason: Some("Resale authorization is not recorded.".into()),
            metadata: Default::default(),
        }];
        let listed = browse(&rows(), &catalog, &SourceQuery::default());
        assert_eq!(listed["sources"][0]["slug"], "two");
        assert_eq!(listed["sources"][1]["slug"], "one");
        // Relevance still outranks reach: somebody who describes the blocked
        // source still finds it rather than being quietly handed another one.
        let searched = browse(
            &rows(),
            &catalog,
            &SourceQuery {
                search: Some("annual filings".into()),
                ..Default::default()
            },
        );
        assert_eq!(searched["sources"][0]["slug"], "one");
    }
    #[test]
    fn source_query_prefers_the_listings_own_logo() {
        let sql = include_str!("sources.sql");
        assert!(sql
            .contains("coalesce(nullif(a.listing_metadata->>'logo',''),nullif(p.avatar_url,''))"));
    }
    #[test]
    fn branding_matches_provider_identity_and_enriches_old_results() {
        let mut rows = rows();
        rows[0].provider_slug = Some("registry".into());
        rows[1].logo_url = Some("javascript:alert(1)".into());
        apply_branding(
            &mut rows,
            Some(&json!({"providers":[
                    {"username":"other","avatar_url":"https://example.com/wrong.png"},
                    {"username":"registry","avatar_url":"https://example.com/registry.png"}
            ]})),
        );
        assert_eq!(
            rows[0].logo_url.as_deref(),
            Some("https://example.com/registry.png")
        );
        assert!(rows[1].logo_url.is_none());
        let mut result =
            json!({"source":{"provider":"one","endpoint_id":"original"},"data":{"value":42}});
        brand_result(&mut result, &rows);
        assert_eq!(
            result["source"]["logo_url"],
            "https://example.com/registry.png"
        );
        assert_eq!(result["source"]["name"], "Company data");
        assert_eq!(result["source"]["endpoint_id"], "original");
        assert_eq!(result["data"]["value"], 42);
        assert!(safe_logo("https://user:password@example.com/logo.png").is_none());
        rows[0].logo_url = Some("https://example.com/kvk.png".into());
        apply_branding(
            &mut rows,
            Some(
                &json!({"providers":[{"username":"registry","avatar_url":"https://example.com/apiosk.png"}]}),
            ),
        );
        assert_eq!(
            rows[0].logo_url.as_deref(),
            Some("https://example.com/kvk.png")
        );
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
