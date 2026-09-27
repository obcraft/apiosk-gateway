use crate::catalog::Catalog;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
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
    #[serde(default, deserialize_with = "labels")]
    pub categories: Vec<String>,
    #[serde(default, deserialize_with = "labels")]
    pub offered_capabilities: Vec<String>,
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
        .take(256)
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
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointQuery {
    pub search: Option<String>,
    pub capabilities: Option<String>,
    /// One service of a grouped source (a Pulse Network listing), by its
    /// listing slug. Absent, the page spans every service of the source.
    pub service: Option<String>,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}
impl EndpointQuery {
    pub fn capabilities(&self) -> Vec<&str> {
        self.capabilities.as_deref().map(|s| s.split(',').collect()).unwrap_or_default()
    }
    /// The listing slug to narrow to, or `""` for every service. The SQL
    /// treats `""` as no filter, so an absent parameter changes nothing.
    pub fn service(&self) -> &str {
        self.service.as_deref().unwrap_or("")
    }
    pub fn valid(&self) -> bool {
        self.search.as_ref().is_none_or(|s| s.chars().count() <= 200)
            && self.service.as_ref().is_none_or(|s| !s.is_empty() && s.len() <= 100 && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')))
            && self.capabilities.as_ref().is_none_or(|s| !s.is_empty() && s.len() <= 16_000)
            && self.capabilities().len() <= 128
            && self.capabilities().iter().all(|s| !s.is_empty() && s.len() <= 200 && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_')))
            && self.limit.is_none_or(|n| (1..=100).contains(&n))
            && self.offset.is_none_or(|n| n <= 10_000)
    }
}

/// Compact, paged endpoint index for the Ask picker. The v1 provider route
/// embeds full request/response schemas and examples in every listing; Pulse
/// alone has hundreds of endpoints, so fetching it all delays the picker.
pub async fn endpoint_page(pool: &PgPool, catalog: &Catalog, source_id: &str, query: &EndpointQuery) -> anyhow::Result<Value> {
    let offset = query.offset.unwrap_or(0);
    let limit = query.limit.unwrap_or(40);
    let search = query.search.as_deref().unwrap_or("").trim();
    let mut result: Value = sqlx::query_scalar(include_str!("source_endpoints.sql"))
        .bind(source_id)
        .bind(search)
        .bind(limit as i64)
        .bind(offset as i64)
        .bind(query.capabilities())
        .bind(query.service())
        .fetch_one(pool).await?;
    mark_available(&mut result, catalog);
    let count = result["items"].as_array().map(Vec::len).unwrap_or(0);
    let total = result["total"].as_u64().unwrap_or(0) as usize;
    result["next_offset"] = json!((offset + count < total).then_some(offset + count));
    Ok(result)
}

fn mark_available(result: &mut Value, catalog: &Catalog) {
    let available: BTreeSet<_> = catalog.services.iter().map(|s| s.endpoint_id.to_string()).collect();
    if let Some(items) = result["items"].as_array_mut() {
        for item in items {
            let id = item["endpoint"]["candidate_id"].as_str().unwrap_or_default().to_owned();
            item["endpoint"]["planner_available"] = json!(available.contains(&id));
            item["endpoint"]["inputs"] = json!(endpoint_inputs(catalog, &id));
        }
    }
}

/// What a runnable endpoint needs, so the buyer can supply known values up
/// front. Every accepted contract on the endpoint contributes; a fact any
/// contract requires is required. Unrunnable endpoints have no contract to ask from.
fn endpoint_inputs(catalog: &Catalog, endpoint_id: &str) -> Vec<Value> {
    let mut inputs: BTreeMap<(String, String), bool> = BTreeMap::new();
    for service in catalog.services.iter().filter(|s| s.endpoint_id.to_string() == endpoint_id) {
        for r in &service.contract.requires {
            let optional = inputs.entry((r.fact_type.clone(), r.subject_role.clone())).or_insert(true);
            *optional &= r.optional;
        }
    }
    let mut rows: Vec<_> = inputs.into_iter().map(|((fact_type, role), optional)| {
        let schema = catalog.vocabulary.get(&fact_type).map(|f| &f.value_schema);
        json!({"fact_type": fact_type, "subject_role": role, "optional": optional,
            "type": schema.and_then(|s| s["type"].as_str()),
            "enum": schema.and_then(|s| s["enum"].as_array()),
            "description": schema.and_then(|s| s["description"].as_str())})
    }).collect();
    rows.sort_by_key(|r| r["optional"].as_bool());
    rows
}
fn selected(filter: &Option<String>, values: impl IntoIterator<Item = impl AsRef<str>>) -> bool {
    filter.as_ref().is_none_or(|f| {
        f.trim().is_empty()
            || values
                .into_iter()
                .any(|v| v.as_ref().eq_ignore_ascii_case(f.trim()))
    })
}
/// Capability slugs are per-contract, so one question ("tell me about this
/// company") is spread over several: StableEnrich enriches by domain, KVK and
/// Companies House serve a registry profile, Pulse services a company lookup.
/// Filtering on any member lists every source in its family. Browse only: the
/// planner still quotes the exact contracted slug. Mirrors the app's
/// `capabilityMatches` in source-directory.ts.
const CAPABILITY_FAMILIES: &[&[&str]] = &[&[
    "company.enrich",
    "company.enrich-by-name",
    "company.registry-profile",
    "company_registry.lookup",
    "company_intel.lookup",
    "company.fr-kyb",
]];
fn capability_selected(filter: &Option<String>, values: impl IntoIterator<Item = impl AsRef<str>>) -> bool {
    let Some(wanted) = filter.as_deref().map(str::trim).filter(|f| !f.is_empty()) else {
        return true;
    };
    let family = CAPABILITY_FAMILIES
        .iter()
        .find(|members| members.iter().any(|m| m.eq_ignore_ascii_case(wanted)))
        .copied()
        .unwrap_or(&[]);
    values.into_iter().any(|v| {
        let v = v.as_ref();
        v.eq_ignore_ascii_case(wanted) || family.iter().any(|m| m.eq_ignore_ascii_case(v))
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
    // Keep every verified listing in the directory. Readiness and executable
    // capabilities remain separate: a discovery-only source is visible, but
    // cannot be quoted by the planner.
    let executable: BTreeSet<&str> = catalog.services.iter().map(|s| s.api_slug.as_str()).collect();
    let restricted: BTreeSet<&str> = catalog.known_sources.iter()
        .filter(|s| s.block_reason.is_some()).map(|s| s.slug.as_str()).collect();
    let sources: Vec<&Source> = sources.iter().collect();
    let categories: BTreeSet<_> = sources.iter()
        .flat_map(|s| s.categories.iter().chain(std::iter::once(&s.category)))
        .filter(|s| !s.is_empty()).collect();
    let tags: BTreeSet<_> = sources.iter().flat_map(|s| &s.tags).collect();
    let sectors: BTreeSet<_> = sources.iter().flat_map(|s| &s.sectors).collect();
    let executable_capabilities_for = |slug: &str| {
        catalog
            .services
            .iter()
            .filter(|s| s.api_slug == slug)
            .map(|s| s.contract.capability_slug.clone())
            .collect::<BTreeSet<_>>()
    };
    let capabilities_for = |source: &Source| {
        source.offered_capabilities.iter().cloned()
            .chain(executable_capabilities_for(&source.slug))
            // These names group sources for discovery; they do not create a
            // runnable contract or grant an unreviewed source membership.
            .filter(|slug| crate::eu_capabilities::find(slug).is_none())
            .chain(crate::eu_capabilities::definitions().iter()
                .filter(|c| c.role(&source.slug) == Some("primary"))
                .map(|c| c.slug.clone()))
            .collect::<BTreeSet<_>>()
    };
    let capability_filter = query.capability.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let selected_group = capability_filter.and_then(crate::eu_capabilities::find)
        .or_else(|| if capability_filter.is_none() {
            query.search.as_deref().and_then(crate::eu_capabilities::find)
        } else { None });
    let capabilities: BTreeSet<_> = sources
        .iter()
        .flat_map(|s| capabilities_for(s))
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
                    "{} {} {} {} {} {} {} {} {} {}",
                    directory_name(source),
                    key,
                    source.name,
                    source.slug,
                    source.description,
                    source.category,
                    source.categories.join(" "),
                    source.tags.join(" "),
                    source.sectors.join(" "),
                    capabilities_for(source)
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
                    && selected(&query.category, source.categories.iter().chain(std::iter::once(&source.category)))
                    && selected(&query.tag, &source.tags)
                    && selected(&query.sector, &source.sectors)
                    && selected_group.is_none_or(|group| group.role(&source.slug) == Some("primary"))
                    && capability_selected(&query.capability, capabilities_for(source)))
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
    if let Some(group) = selected_group {
        matches.sort_by_key(|(_, _, _, key, ..)| group.primary_sources.iter().position(|slug| slug == key).unwrap_or(usize::MAX));
    }
    let offset = query.offset.unwrap_or(0);
    let limit = query.limit.unwrap_or(20).clamp(1, 50);
    let page: Vec<_> = matches.iter().skip(offset).take(limit).map(|(_, _, _, key, members, matching)| {
        let first = members[0].1;
        let slugs: Vec<_> = members.iter().map(|(_, s)| s.slug.as_str()).collect();
        let services: Vec<_> = catalog.services.iter().filter(|s| slugs.contains(&s.api_slug.as_str())).collect();
        let executable_capabilities: BTreeSet<_> = services.iter().map(|s| &s.contract.capability_slug).collect();
        let capabilities: BTreeSet<_> = members.iter().flat_map(|(_,s)| capabilities_for(s)).collect();
        let inputs: BTreeSet<_> = services.iter().flat_map(|s| s.contract.requires.iter().filter(|r| !r.optional).map(|r| &r.fact_type)).collect();
        let grouped = *key == "pulsenetwork";
        let tags: BTreeSet<_> = members.iter().flat_map(|(_,s)| &s.tags).collect();
        let sectors: BTreeSet<_> = members.iter().flat_map(|(_,s)| &s.sectors).collect();
        let categories: BTreeSet<_> = members.iter()
            .flat_map(|(_,s)| s.categories.iter().chain(std::iter::once(&s.category)))
            .filter(|s| !s.is_empty()).collect();
        let mut row = json!({"slug":key,"provider_slug":first.provider_slug,"logo_url":if grouped {first.provider_logo_url.as_ref().or(first.logo_url.as_ref())} else {first.logo_url.as_ref()},
            "name":directory_name(first),"description":if grouped {"Data services published by Pulse Network."} else {&first.description},
            "category":if grouped {"intelligence"} else {&first.category},"categories":categories,
            "tags":tags,"sectors":sectors,"endpoint_count":members.iter().map(|(_,s)|s.endpoint_count).sum::<i64>(),
            "readiness":crate::readiness::assess(catalog, &slugs),
            "available_in_v2":members.iter().any(|(_,s)| !restricted.contains(s.slug.as_str()) && executable.contains(s.slug.as_str())),
            "capabilities":capabilities,"executable_capabilities":executable_capabilities,"input_types":inputs});
        row["capability_roles"] = json!(crate::eu_capabilities::definitions().iter()
            .filter_map(|c| c.role(key).map(|role| (&c.slug, role))).collect::<BTreeMap<_, _>>());
        if grouped {
            row["service_count"] = json!(members.len());
            row["matching_service_count"] = json!(matching.len());
            row["services"] = json!(matching.iter().map(|(_,_,s)|json!({"slug":s.slug,"name":s.name,"description":s.description,"category":s.category})).collect::<Vec<_>>());
        }
        row
    }).collect();
    let end = offset.saturating_add(page.len());
    let group_view = |group: &crate::eu_capabilities::Capability| {
        let visible = |slugs: &[String]| slugs.iter().filter_map(|slug| sources.iter()
            .find(|source| source.slug == *slug)
            .map(|source| json!({"slug":source.slug,"name":source.name}))).collect::<Vec<_>>();
        json!({"slug":group.slug,"name":group.name,
            "primary_sources":visible(&group.primary_sources),
            "supplementary_sources":visible(&group.supplementary_sources)})
    };
    json!({"protocol_version":"2","sources":page,"total":matches.len(),"catalog_total":catalog_total,
        "offset":offset,"next_offset":if end < matches.len() {Some(end)} else {None},
        "categories":categories,"tags":tags,"sectors":sectors,"capabilities":capabilities,
        "capability_groups":crate::eu_capabilities::definitions().iter().map(group_view).collect::<Vec<_>>(),
        "selected_capability":selected_group.map(group_view),
        "notice":"Browsing is free. Every verified source is listed, including discovery-only and restricted sources. Each source is counted once; services under Pulse Network are not separate sources. Only runtime-accepted capabilities can be quoted, and Apiosk checks the exact price before any purchase."})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn eu_rows() -> Vec<Source> {
        let slugs = ["kvk-dutch-business-register", "sirenic", "companies-house", "gleif", "vies", "overheid-io",
            "ted", "tenderned", "eu-funding-tenders", "cbs-statline", "eurostat", "pdok", "data-overheid-nl", "unrelated"];
        slugs.iter().rev().map(|slug| serde_json::from_value(json!({
            "slug":slug,"provider_slug":"apiosk-basics","name":slug,"description":"EU company tender statistics data",
            "category":"data","tags":[],"sectors":[],"endpoint_count":1,
            // Provider metadata must not be able to self-enrol in curated groups.
            "offered_capabilities":["eu.company.profile","eu.tenders.search","eu.statistics.query"]
        })).unwrap()).collect()
    }

    #[test]
    fn european_capabilities_return_only_their_primary_sources_in_order() {
        let c = catalog();
        let rows = eu_rows();
        for (slug, primary, supplementary) in [
            ("eu.company.profile", vec!["kvk-dutch-business-register", "sirenic", "companies-house"], vec!["gleif", "vies", "overheid-io"]),
            ("eu.tenders.search", vec!["ted", "tenderned"], vec!["sirenic"]),
            ("eu.statistics.query", vec!["cbs-statline", "eurostat"], vec!["data-overheid-nl"]),
        ] {
            for query in [SourceQuery { capability: Some(format!(" {} ", slug.to_uppercase())), ..Default::default() },
                SourceQuery { search: Some(slug.into()), ..Default::default() }] {
                let result = browse(&rows, &c, &query);
                let found: Vec<_> = result["sources"].as_array().unwrap().iter().map(|s| s["slug"].as_str().unwrap()).collect();
                assert_eq!(found, primary);
                assert_eq!(result["total"], primary.len());
                let extras: Vec<_> = result["selected_capability"]["supplementary_sources"].as_array().unwrap().iter().map(|s| s["slug"].as_str().unwrap()).collect();
                assert_eq!(extras, supplementary);
                for source in result["sources"].as_array().unwrap() {
                    assert_eq!(source["capability_roles"][slug], "primary");
                    assert_eq!(source["available_in_v2"], false);
                    assert_eq!(source["executable_capabilities"], json!([]));
                }
            }
        }
    }

    #[test]
    fn european_groups_respect_visibility_filters_and_pagination() {
        let c = catalog();
        let mut rows = eu_rows();
        rows.retain(|s| s.slug != "companies-house" && s.slug != "vies");
        let query = SourceQuery { capability: Some("eu.company.profile".into()), limit: Some(1), ..Default::default() };
        let first = browse(&rows, &c, &query);
        assert_eq!(first["total"], 2);
        assert_eq!(first["next_offset"], 1);
        assert_eq!(first["sources"][0]["slug"], "kvk-dutch-business-register");
        assert_eq!(first["selected_capability"]["primary_sources"].as_array().unwrap().len(), 2);
        assert_eq!(first["selected_capability"]["supplementary_sources"].as_array().unwrap().len(), 2);
        let second = browse(&rows, &c, &SourceQuery { offset: Some(1), ..query });
        assert_eq!(second["sources"][0]["slug"], "sirenic");
        assert!(second["next_offset"].is_null());
        let excluded = browse(&rows, &c, &SourceQuery { capability: Some("eu.company.profile".into()), category: Some("weather".into()), ..Default::default() });
        assert_eq!(excluded["total"], 0);
        let directory = browse(&rows, &c, &SourceQuery::default());
        let sirenic = directory["sources"].as_array().unwrap().iter().find(|s| s["slug"] == "sirenic").unwrap();
        assert_eq!(sirenic["capability_roles"]["eu.tenders.search"], "supplementary");
        assert!(!sirenic["capabilities"].as_array().unwrap().contains(&json!("eu.tenders.search")));
    }
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
        let template = c.services[0].clone();
        c.services = rows.iter().map(|s| { let mut service = template.clone(); service.api_slug = s.slug.clone(); service }).collect();
        // Keep the parent first while its 87 services still occupy one row.
        rows.rotate_left(2);
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
        let independent = rows.iter_mut().find(|s| s.slug == "one").unwrap();
        independent.name = "Independent Pulse".into();
        assert_eq!(directory_slug(independent), "one");
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
    fn all_verified_sources_are_visible_without_making_uncontracted_calls_purchasable() {
        let mut catalog = catalog();
        // Everything the runtime can execute belongs to "two". "one" is
        // still discoverable, with its purchase restriction intact.
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
        assert_eq!(listed["sources"].as_array().unwrap().len(), 2);
        assert_eq!(listed["sources"][1]["slug"], "one");
        assert_eq!(listed["sources"][1]["available_in_v2"], false);
        assert_eq!(listed["total"], 2);
        assert_eq!(listed["catalog_total"], 2);
        assert_eq!(listed["categories"], json!(["finance", "weather"]));
        assert_eq!(listed["tags"], json!(["registry"]));
        assert!(listed["next_offset"].is_null());
        let searched = browse(
            &rows(),
            &catalog,
            &SourceQuery {
                search: Some("annual filings".into()),
                ..Default::default()
            },
        );
        assert_eq!(searched["sources"][0]["slug"], "one");
        assert_eq!(searched["total"], 1);
        // An accepted service cannot override an explicit purchase restriction.
        let mut blocked_service = catalog.services[0].clone();
        blocked_service.api_slug = "one".into();
        catalog.services.push(blocked_service);
        let restricted = browse(&rows(), &catalog, &SourceQuery::default());
        assert_eq!(restricted["total"], 2);
        assert_eq!(restricted["sources"][1]["available_in_v2"], false);
        // Removing the restriction alone does not make an unimplemented source usable.
        catalog.known_sources.clear();
        catalog.services.retain(|s| s.api_slug == "two");
        let discovery = browse(&rows(), &catalog, &SourceQuery::default());
        assert_eq!(discovery["total"], 2);
        assert_eq!(discovery["sources"][1]["available_in_v2"], false);
    }
    #[test]
    fn offered_capability_is_browsable_without_becoming_executable() {
        let mut sources = rows();
        sources[0].offered_capabilities = vec!["filings.sec-search".into()];
        let catalog = catalog();
        let result = browse(&sources, &catalog, &SourceQuery {
            capability: Some("filings.sec-search".into()), ..Default::default()
        });
        assert_eq!(result["total"], 1);
        assert_eq!(result["sources"][0]["slug"], sources[0].slug);
        assert_eq!(result["sources"][0]["capabilities"], json!(["filings.sec-search"]));
        assert_eq!(result["sources"][0]["executable_capabilities"], json!([]));
    }
    #[test]
    fn runnable_endpoints_list_their_contract_inputs_required_first() {
        let catalog = catalog();
        let service = catalog.services.iter().find(|s| !s.contract.requires.is_empty()).unwrap();
        let mut page = json!({"items":[
            {"endpoint":{"candidate_id":service.endpoint_id.to_string()}},
            {"endpoint":{"candidate_id":"00000000-0000-0000-0000-000000000000"}}
        ]});
        mark_available(&mut page, &catalog);
        let inputs = page["items"][0]["endpoint"]["inputs"].as_array().unwrap();
        let required = service.contract.requires.iter().find(|r| !r.optional).unwrap();
        assert!(inputs.iter().any(|i| i["fact_type"] == required.fact_type.as_str() && i["optional"] == false));
        let flags: Vec<_> = inputs.iter().map(|i| i["optional"].as_bool().unwrap()).collect();
        assert!(flags.windows(2).all(|w| w[0] <= w[1]), "required inputs come first");
        assert_eq!(page["items"][1]["endpoint"]["inputs"], json!([]));
    }
    #[test]
    fn capability_filter_matches_the_whole_company_profile_family() {
        let mut sources = rows();
        sources[0].offered_capabilities = vec!["company.registry-profile".into()];
        sources[1].offered_capabilities = vec!["company.sanctions-screen".into()];
        let catalog = catalog();
        let result = browse(&sources, &catalog, &SourceQuery {
            capability: Some("company.enrich".into()), ..Default::default()
        });
        assert_eq!(result["total"], 1);
        assert_eq!(result["sources"][0]["slug"], sources[0].slug);
        // Outside a family the filter stays exact.
        let exact = browse(&sources, &catalog, &SourceQuery {
            capability: Some("company.sanctions-screen".into()), ..Default::default()
        });
        assert_eq!(exact["total"], 1);
        assert_eq!(exact["sources"][0]["slug"], sources[1].slug);
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
    fn searches_metadata_filters_and_paginates_executable_sources() {
        let rows = rows();
        let mut c = catalog();
        let template = c.services[0].clone();
        c.services = rows.iter().map(|s| { let mut service = template.clone(); service.api_slug = s.slug.clone(); service }).collect();
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
        assert_eq!(first["sources"][0]["available_in_v2"], true);
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
    #[test]
    fn endpoint_index_marks_only_runtime_accepted_contracts_as_selectable() {
        let catalog = catalog();
        let accepted = catalog.services[0].endpoint_id;
        let mut page = json!({"items":[
            {"endpoint":{"candidate_id":accepted}},
            {"endpoint":{"candidate_id":uuid::Uuid::new_v4()}}
        ]});
        mark_available(&mut page, &catalog);
        assert_eq!(page["items"][0]["endpoint"]["planner_available"], true);
        assert_eq!(page["items"][1]["endpoint"]["planner_available"], false);
        assert!(EndpointQuery {limit:Some(100),..Default::default()}.valid());
        assert!(!EndpointQuery {limit:Some(101),..Default::default()}.valid());
    }
    #[test]
    fn endpoint_capability_filters_are_bounded_exact_contract_identifiers() {
        let query = EndpointQuery {capabilities:Some("operation.onchainpulse.get-api-rh-launches-19e826f6,eu.statistics.query".into()), ..Default::default()};
        assert!(query.valid());
        assert_eq!(query.capabilities(), vec!["operation.onchainpulse.get-api-rh-launches-19e826f6", "eu.statistics.query"]);
        for invalid in ["", "foo,,bar", "a b", "x%", "x'", "a,", &"x".repeat(201), &vec!["x";129].join(",")] {
            assert!(!EndpointQuery {capabilities:Some(invalid.into()),..Default::default()}.valid(), "{invalid}");
        }
        assert!(EndpointQuery::default().capabilities().is_empty());
    }
    #[test]
    fn endpoint_service_filter_is_an_optional_bounded_listing_slug() {
        // Absent, the query binds "" and the SQL keeps every service of the
        // source, so pages without the parameter are unchanged.
        assert_eq!(EndpointQuery::default().service(), "");
        assert!(EndpointQuery::default().valid());
        for slug in ["pulse-company-lookup", "kvk", "cbs_statline", "api.v2"] {
            let query = EndpointQuery {service:Some(slug.into()), ..Default::default()};
            assert!(query.valid(), "{slug}");
            assert_eq!(query.service(), slug);
        }
        for invalid in ["", "a b", "x%", "x'", "a,b", "x/y", &"x".repeat(101)] {
            assert!(!EndpointQuery {service:Some(invalid.into()),..Default::default()}.valid(), "{invalid}");
        }
        let query: EndpointQuery = serde_json::from_value(json!({"service":"pulse-company-lookup","limit":40})).unwrap();
        assert_eq!(query.service(), "pulse-company-lookup");
    }
    #[test]
    fn endpoint_sql_narrows_to_one_listing_only_when_a_service_is_given() {
        let sql = include_str!("source_endpoints.sql");
        // $6 is the service slug bound by `endpoint_page`; "" disables it.
        assert!(sql.contains("AND ($6='' OR a.slug=$6)"));
        // It narrows the matched set, before totals and paging are computed.
        let matched_end = sql.find("), filtered AS").unwrap();
        assert!(sql.find("$6").unwrap() < matched_end);
        assert_eq!(sql.matches("$6").count(), 2);
    }

}
