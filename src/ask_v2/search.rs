//! Capability-only directory search. This index contains catalog metadata, never
//! questions or subjects. Matches describe discovery; they do not create plans.
use crate::{
    capability_intake::{
        capability_query, normalize_slug, Candidate, Capability, Match, ParsedRequest,
    },
    catalog::{Catalog, KnownSource, Service},
    state::Task,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, OnceLock},
};

const FINANCIAL: u8 = 1;
const OFFICERS: u8 = 2;
const CONTROL: u8 = 4;
const SANCTIONS: u8 = 8;

#[derive(Clone, Copy)]
enum Target {
    Endpoint(usize),
    Source(usize),
}

struct Document {
    target: Target,
    tokens: BTreeSet<String>,
    facets: u8,
    capabilities: BTreeSet<String>,
}
struct Index {
    documents: Vec<Document>,
    frequency: BTreeMap<String, usize>,
}

// A single catalog version is retained. Concurrent readers may finish with the
// previous Arc, but new versions replace the cache rather than accumulating.
fn index(catalog: &Catalog) -> Arc<Index> {
    static LAST: OnceLock<Mutex<Option<(String, Arc<Index>)>>> = OnceLock::new();
    let cache = LAST.get_or_init(|| Mutex::new(None));
    if let Some((_, value)) = cache
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .filter(|(version, _)| version == &catalog.version)
    {
        return value.clone();
    }
    let mut documents = Vec::with_capacity(catalog.services.len() + catalog.known_sources.len());
    for (position, service) in catalog.services.iter().enumerate() {
        let described = catalog
            .known_sources
            .iter()
            .find(|source| source.slug == service.api_slug)
            .and_then(|source| {
                source
                    .metadata
                    .capability_descriptions
                    .get(&service.contract.capability_slug)
            })
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        // Do not attach a provider's broad description to each endpoint: that
        // would make its officers endpoint falsely match beneficial ownership.
        let text = format!(
            "{} {} {} {} {} {}",
            service.contract.capability_slug,
            service.name,
            service.description.as_deref().unwrap_or(""),
            described,
            service
                .contract
                .input_map
                .values()
                .map(|binding| binding.name.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            service
                .contract
                .output_map
                .produces
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        );
        let capabilities =
            normalized_capabilities(std::iter::once(service.contract.capability_slug.as_str()));
        documents.push(document(Target::Endpoint(position), &text, capabilities));
    }
    for (position, source) in catalog.known_sources.iter().enumerate() {
        let descriptions = &source.metadata.capability_descriptions;
        let text = format!(
            "{} {} {} {} {}",
            source.name,
            source.metadata.description,
            source.metadata.category,
            source.metadata.tags.join(" "),
            descriptions
        );
        let capabilities = normalized_capabilities(
            descriptions
                .as_object()
                .into_iter()
                .flat_map(|object| object.keys().map(String::as_str)),
        );
        documents.push(document(Target::Source(position), &text, capabilities));
    }
    let mut frequency = BTreeMap::new();
    for document in &documents {
        for token in &document.tokens {
            *frequency.entry(token.clone()).or_default() += 1;
        }
    }
    let value = Arc::new(Index {
        documents,
        frequency,
    });
    *cache.lock().unwrap_or_else(|p| p.into_inner()) =
        Some((catalog.version.clone(), value.clone()));
    value
}

fn normalized_capabilities<'a>(slugs: impl Iterator<Item = &'a str>) -> BTreeSet<String> {
    slugs
        .filter_map(|slug| normalize_slug(slug, "").map(|(slug, _)| slug.to_lowercase()))
        .collect()
}

fn words(text: &str) -> BTreeSet<String> {
    let chars: Vec<_> = text.chars().collect();
    let mut separated = String::with_capacity(text.len());
    for (index, &character) in chars.iter().enumerate() {
        if character.is_uppercase()
            && index > 0
            && (chars[index - 1].is_lowercase()
                || (chars[index - 1].is_uppercase()
                    && chars.get(index + 1).is_some_and(|next| next.is_lowercase())))
        {
            separated.push(' ');
        }
        separated.extend(character.to_lowercase());
    }
    separated
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| word.len() >= 3 && word.chars().count() <= 64)
        .map(|word| match word {
            "companies" => "company".into(),
            "addresses" => "address".into(),
            "jaarrekeningen" => "jaarrekening".into(),
            _ if word.len() > 3
                && word.ends_with('s')
                && !word.ends_with("ss")
                && !word.ends_with("us") =>
            {
                word[..word.len() - 1].to_owned()
            }
            _ => word.to_owned(),
        })
        .collect()
}

fn document(target: Target, text: &str, capabilities: BTreeSet<String>) -> Document {
    let mut tokens = words(text);
    let has = |word: &str| tokens.contains(word);
    let financial = has("jaarrekening")
        || (has("financial") && has("statement"))
        || (has("annual") && has("account"))
        || (has("balance") && has("sheet"))
        || capabilities.contains("company.accounts");
    let officers = ["officer", "director", "bestuurder"]
        .iter()
        .any(|word| has(word));
    let control = has("psc")
        || has("ubo")
        || (has("significant") && has("control"))
        || ((has("beneficial") || has("ultimate")) && (has("owner") || has("ownership")));
    let sanctions = has("sanction") || has("sanctie");
    let facets = u8::from(financial) * FINANCIAL
        | u8::from(officers) * OFFICERS
        | u8::from(control) * CONTROL
        | u8::from(sanctions) * SANCTIONS;
    const VAGUE: &[&str] = &[
        "the",
        "for",
        "and",
        "from",
        "with",
        "that",
        "this",
        "which",
        "about",
        "any",
        "all",
        "are",
        "its",
        "into",
        "retrieve",
        "provide",
        "get",
        "find",
        "search",
        "list",
        "lookup",
        "query",
        "show",
        "check",
        "fetch",
        "data",
        "information",
        "detail",
        "record",
        "result",
        "response",
        "overview",
        "current",
        "latest",
        "available",
        "company",
        "business",
        "entity",
        "general",
        "location",
        "compliance",
        "finance",
        "legal",
        "document",
        "research",
        "api",
        "endpoint",
        "service",
        "source",
        "name",
        "identifier",
        "number",
        "input",
        "output",
        "type",
        "string",
        "matching",
        "match",
        "score",
        "link",
        "based",
        "using",
        "specified",
        "supplied",
        "requested",
        "return",
    ];
    tokens.retain(|token| !VAGUE.contains(&token.as_str()));
    for (present, marker) in [
        (financial, "@financial"),
        (officers, "@officers"),
        (control, "@control"),
        (sanctions, "@sanctions"),
    ] {
        if present {
            tokens.insert(marker.into());
        }
    }
    Document {
        target,
        tokens,
        facets,
        capabilities,
    }
}

fn identity(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
fn identity_matches(hint: &str, value: &str) -> bool {
    let value = identity(value);
    !hint.is_empty() && (value == hint || (hint.len() >= 3 && value.contains(hint)))
}
fn hinted_sources(capability: &Capability, catalog: &Catalog) -> Option<BTreeSet<String>> {
    let hint = identity(capability.source_hint.as_deref()?);
    let mut sources: BTreeSet<_> = catalog
        .known_sources
        .iter()
        .filter(|source| {
            identity_matches(&hint, &source.slug)
                || identity_matches(&hint, &source.name)
                || source
                    .metadata
                    .provider_slug
                    .as_deref()
                    .is_some_and(|provider| identity_matches(&hint, provider))
        })
        .map(|source| source.slug.clone())
        .collect();
    sources.extend(
        catalog
            .services
            .iter()
            .filter(|service| identity_matches(&hint, &service.api_slug))
            .map(|service| service.api_slug.clone()),
    );
    (!sources.is_empty()).then_some(sources)
}

fn score(wanted: &Document, candidate: &Document, index: &Index) -> f64 {
    // Explicit concepts keep otherwise overlapping descriptions apart: annual
    // accounts are not bank accounts, and officers are not PSC records.
    if wanted.tokens.is_empty() || (wanted.facets != 0 && wanted.facets & candidate.facets == 0) {
        return 0.0;
    }
    let overlap: Vec<_> = wanted.tokens.intersection(&candidate.tokens).collect();
    if overlap.is_empty() {
        return 0.0;
    }
    let exact = !wanted.capabilities.is_disjoint(&candidate.capabilities);
    // These accepted semantic contracts identify company-level records. Their
    // concise descriptions must not lose to a long name-search or single-PSC
    // notification description merely because that text repeats more terms.
    let direct_facets = candidate.capabilities.iter().fold(0, |facets, capability| {
        facets
            | match capability.as_str() {
                "company.officers" => OFFICERS,
                "company.significant.control" => CONTROL,
                "company.financial.statements" | "company.accounts" => FINANCIAL,
                "company.sanctions.screen" => SANCTIONS,
                _ => 0,
            }
    });
    overlap
        .into_iter()
        .map(|token| {
            let idf = ((index.documents.len() + 1) as f64
                / (index.frequency.get(token).copied().unwrap_or(0) + 1) as f64)
                .ln()
                + 1.0;
            idf * if token.starts_with('@') { 8.0 } else { 1.0 }
        })
        .sum::<f64>()
        + if exact { 1000.0 } else { 0.0 }
        + (wanted.facets & direct_facets).count_ones() as f64 * 100.0
}

fn safe_url(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    (value.len() <= 2048
        && url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none())
    .then(|| url.to_string())
}
fn endpoint(service: &Service) -> Candidate {
    Candidate {
        origin: "apiosk".into(),
        name: service.name.clone(),
        description: service
            .description
            .clone()
            .unwrap_or_else(|| service.contract.capability_slug.clone()),
        source_slug: Some(service.api_slug.clone()),
        capability: Some(service.contract.capability_slug.clone()),
        endpoint_id: Some(service.endpoint_id.to_string()),
        url: service.source_url.as_deref().and_then(safe_url),
        method: service.method.clone(),
        price: Some(format!("{:.6} USD", service.total() as f64 / 1_000_000.0)),
        network: None,
        calls_30d: None,
        availability: if service.unverified {
            "unverified"
        } else {
            "supported"
        }
        .into(),
    }
}
fn source(source: &KnownSource) -> Candidate {
    Candidate {
        origin: "apiosk".into(),
        name: source.name.clone(),
        description: source.metadata.description.clone(),
        source_slug: Some(source.slug.clone()),
        capability: None,
        endpoint_id: None,
        // KnownSource has no documented URL. Do not invent an executable URL.
        url: None,
        method: String::new(),
        price: None,
        network: None,
        calls_30d: None,
        availability: "discovery_only".into(),
    }
}

/// Rank each generic capability independently across the whole local directory.
/// Entity labels and identifiers never participate in relevance scoring.
pub fn search(parsed: &ParsedRequest, catalog: &Catalog) -> Vec<Match> {
    let index = index(catalog);
    let empty_task = Task::default();
    parsed
        .capabilities
        .iter()
        .filter(|capability| capability.slug != "general.conversation")
        .map(|capability| {
            let query = capability_query(capability, parsed, &empty_task);
            let mut generic = capability.clone();
            generic.name = format!(
                "{} {} {}",
                capability.slug,
                capability.name,
                capability.outputs.join(" ")
            );
            generic.source_hint = None;
            let text = capability_query(&generic, parsed, &empty_task);
            let wanted = document(
                Target::Source(0),
                &text,
                normalized_capabilities(std::iter::once(capability.slug.as_str())),
            );
            let restricted = hinted_sources(capability, catalog);
            let mut endpoints = Vec::new();
            let mut sources = Vec::new();
            for candidate in &index.documents {
                let (slug, position) = match candidate.target {
                    Target::Endpoint(position) => (&catalog.services[position].api_slug, position),
                    Target::Source(position) => (&catalog.known_sources[position].slug, position),
                };
                if restricted
                    .as_ref()
                    .is_some_and(|slugs| !slugs.contains(slug))
                {
                    continue;
                }
                let score = score(&wanted, candidate, &index);
                if score <= 0.0 {
                    continue;
                }
                match candidate.target {
                    Target::Endpoint(_) => endpoints.push((score, &catalog.services[position])),
                    Target::Source(_) => sources.push((score, &catalog.known_sources[position])),
                }
            }
            endpoints.sort_by(|a, b| {
                b.0.total_cmp(&a.0)
                    .then(a.1.unverified.cmp(&b.1.unverified))
                    .then(a.1.total().cmp(&b.1.total()))
                    .then(a.1.api_slug.cmp(&b.1.api_slug))
                    .then(a.1.endpoint_id.cmp(&b.1.endpoint_id))
            });
            sources.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.slug.cmp(&b.1.slug)));
            let mut seen_endpoints = BTreeSet::new();
            let mut apiosk: Vec<_> = endpoints
                .into_iter()
                .filter(|(_, service)| seen_endpoints.insert(service.endpoint_id))
                .take(3)
                .map(|(_, service)| endpoint(service))
                .collect();
            let mut seen_sources: BTreeSet<_> = apiosk
                .iter()
                .filter_map(|candidate| candidate.source_slug.clone())
                .collect();
            for (_, candidate) in sources {
                if apiosk.len() == 3 {
                    break;
                }
                if seen_sources.insert(candidate.slug.clone()) {
                    apiosk.push(source(candidate));
                }
            }
            let operations = apiosk
                .iter()
                .filter_map(|candidate| candidate.capability.clone())
                .collect();
            Match {
                slug: capability.slug.clone(),
                query,
                operations,
                apiosk,
                coinbase: vec![],
                coinbase_status: "pending".into(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{catalog::Snapshot, readiness::Metadata};
    use serde_json::json;
    use uuid::Uuid;

    fn service(id: u128, capability: &str, name: &str, description: &str, source: &str) -> Service {
        let fixture: Snapshot = serde_json::from_str(include_str!(
            "../../tests/fixtures/integration-catalog.json"
        ))
        .unwrap();
        let mut service = fixture.services[0].clone();
        service.id = Uuid::from_u128(id);
        service.endpoint_id = Uuid::from_u128(id + 1000);
        service.api_slug = source.into();
        service.contract.capability_slug = capability.into();
        service.name = name.into();
        service.description = Some(description.into());
        service
    }
    fn catalog(services: Vec<Service>, known_sources: Vec<KnownSource>) -> Catalog {
        let fixture: Snapshot = serde_json::from_str(include_str!(
            "../../tests/fixtures/integration-catalog.json"
        ))
        .unwrap();
        Catalog::build(Snapshot {
            services,
            known_sources,
            vocabulary: fixture.vocabulary,
        })
    }
    fn known(slug: &str, name: &str, description: &str) -> KnownSource {
        KnownSource {
            slug: slug.into(),
            name: name.into(),
            block_reason: None,
            metadata: Metadata {
                description: description.into(),
                ..Default::default()
            },
        }
    }
    fn parsed(capabilities: Vec<serde_json::Value>) -> ParsedRequest {
        let capabilities: Vec<_> = capabilities.into_iter().map(|value| json!({
            "slug":value["slug"],"name":value["name"],"description":value["description"].as_str().unwrap_or("Retrieve the requested information."),
            "domain":"company","inputs":[],"outputs":[],"subject_id":"s1","source_hint":value["source_hint"],"confidence":0.9
        })).collect();
        crate::capability_intake::normalize(&json!({"language":"en","subjects":[{"id":"s1","label":"Monzo Bank Limited","type":"company","identifiers":{}}],
            "capabilities":capabilities,"deliverable":{"format":"chat","operations":[]}}).to_string(),
            "Find information for Monzo Bank Limited from Companies House and preferred", "fixture").unwrap()
    }

    #[test]
    fn monzo_officers_and_significant_control_keep_separate_endpoint_matches() {
        let catalog = catalog(
            vec![
                service(
                    1,
                    "company.officers",
                    "Company officers",
                    "List current directors and officers",
                    "companies-house",
                ),
                service(
                    2,
                    "company.persons-with-significant-control",
                    "Persons with significant control",
                    "List beneficial owners and PSC records",
                    "companies-house",
                ),
            ],
            vec![known(
                "companies-house",
                "Companies House",
                "Company officers, directors and beneficial ownership",
            )],
        );
        let results = search(
            &parsed(vec![
                json!({"slug":"company.officers","name":"Company officers"}),
                json!({"slug":"company.significantControl","name":"Persons with significant control"}),
            ]),
            &catalog,
        );
        assert_eq!(
            results[0].operations,
            BTreeSet::from(["company.officers".into()])
        );
        assert_eq!(
            results[1].operations,
            BTreeSet::from(["company.persons-with-significant-control".into()])
        );
        assert!(results.iter().all(|found| !found.query.contains("Monzo")));
    }

    #[test]
    fn financial_statements_match_annual_accounts_but_not_bank_accounts() {
        let catalog = catalog(
            vec![
                service(
                    1,
                    "company.accounts",
                    "Annual accounts",
                    "Retrieve annual company accounts",
                    "registry",
                ),
                service(
                    2,
                    "finance.bank-accounts",
                    "Bank accounts",
                    "Retrieve bank account balances and account statements",
                    "bank",
                ),
            ],
            vec![],
        );
        let found = search(
            &parsed(vec![
                json!({"slug":"company.financialStatements","name":"Financial statements"}),
            ]),
            &catalog,
        );
        assert_eq!(
            found[0].operations,
            BTreeSet::from(["company.accounts".into()])
        );
    }

    #[test]
    fn irrelevant_or_vague_capabilities_do_not_match_general_company_words() {
        let catalog = catalog(
            vec![service(
                1,
                "company.officers",
                "Company officers",
                "Get current company director records",
                "registry",
            )],
            vec![],
        );
        for (slug, name) in [
            ("company.search", "Company data search"),
            ("location.weather.forecast", "Weather forecasts"),
        ] {
            let found = search(&parsed(vec![json!({"slug":slug,"name":name})]), &catalog);
            assert!(found[0].apiosk.is_empty(), "{slug}: {:?}", found[0].apiosk);
        }
    }

    #[test]
    fn recognized_source_hint_restricts_endpoints_even_when_another_is_cheaper() {
        let mut cheap = service(
            1,
            "company.officers",
            "Company officers",
            "Directors",
            "other",
        );
        cheap.list_atomic = 0;
        let catalog = catalog(
            vec![
                cheap,
                service(
                    2,
                    "company.officers",
                    "Company officers",
                    "Directors",
                    "preferred",
                ),
            ],
            vec![known("preferred", "Preferred", "Company directors")],
        );
        let found = search(
            &parsed(vec![
                json!({"slug":"company.officers","name":"Company officers","source_hint":"preferred"}),
            ]),
            &catalog,
        );
        assert_eq!(found[0].apiosk.len(), 1);
        assert_eq!(found[0].apiosk[0].source_slug.as_deref(), Some("preferred"));
    }

    #[test]
    fn exact_endpoint_relevance_precedes_price_and_results_are_deduplicated() {
        let mut exact = service(
            1,
            "company.officers",
            "Company officers",
            "Company directors",
            "registry",
        );
        exact.list_atomic = 900;
        let mut broader = service(
            2,
            "company.director-summary",
            "Director summary",
            "Overview of officers and directors",
            "summary",
        );
        broader.list_atomic = 0;
        let catalog = catalog(vec![broader, exact.clone(), exact], vec![]);
        let found = search(
            &parsed(vec![
                json!({"slug":"company.officers","name":"Company officers"}),
            ]),
            &catalog,
        );
        assert_eq!(
            found[0].apiosk[0].capability.as_deref(),
            Some("company.officers")
        );
        assert_eq!(found[0].apiosk.len(), 2);
    }

    #[test]
    fn all_37_source_groups_are_searched_including_metadata_without_contracts() {
        let known_sources = (0..37)
            .map(|index| {
                known(
                    &format!("source-{index}"),
                    &format!("Source {index}"),
                    if index == 36 {
                        "Sanctions screening against current sanctions lists"
                    } else {
                        "Weather forecast"
                    },
                )
            })
            .collect();
        let catalog = catalog(vec![], known_sources);
        assert_eq!(crate::capability_intake::source_count(&catalog), 37);
        let found = search(
            &parsed(vec![
                json!({"slug":"compliance.sanctions","name":"Sanctions screening"}),
            ]),
            &catalog,
        );
        assert_eq!(found[0].apiosk.len(), 1);
        let candidate = &found[0].apiosk[0];
        assert_eq!(candidate.source_slug.as_deref(), Some("source-36"));
        assert_eq!(candidate.availability, "discovery_only");
        assert!(
            candidate.capability.is_none()
                && candidate.endpoint_id.is_none()
                && candidate.url.is_none()
                && candidate.price.is_none()
        );
        assert!(found[0].operations.is_empty());
    }

    #[test]
    fn catalog_version_replacement_invalidates_the_index_without_caching_questions() {
        let first = catalog(
            vec![service(
                1,
                "company.officers",
                "Officers",
                "Company directors",
                "registry",
            )],
            vec![],
        );
        let second = catalog(
            vec![service(
                1,
                "compliance.sanctions",
                "Sanctions",
                "Sanctions screening",
                "registry",
            )],
            vec![],
        );
        let request = parsed(vec![
            json!({"slug":"compliance.sanctions","name":"Sanctions screening"}),
        ]);
        assert!(search(&request, &first)[0].apiosk.is_empty());
        assert_eq!(search(&request, &second)[0].apiosk.len(), 1);
    }

    #[test]
    fn subject_names_cannot_make_an_unrelated_source_relevant() {
        let catalog = catalog(
            vec![],
            vec![known(
                "monzo",
                "Monzo Bank Limited",
                "Current company information",
            )],
        );
        let request = parsed(vec![
            json!({"slug":"company.search","name":"Information for Monzo Bank Limited",
            "description":"Retrieve company data for Monzo Bank Limited."}),
        ]);
        let found = search(&request, &catalog);
        assert!(!found[0].query.contains("Monzo"));
        assert!(found[0].apiosk.is_empty());
    }

    #[test]
    fn at_most_three_distinct_endpoints_are_returned_for_each_capability() {
        let catalog = catalog(
            (1..=8)
                .map(|id| {
                    service(
                        id,
                        "company.officers",
                        "Company officers",
                        "Current directors",
                        &format!("source-{id}"),
                    )
                })
                .collect(),
            vec![],
        );
        let found = search(
            &parsed(vec![
                json!({"slug":"company.officers","name":"Company officers"}),
            ]),
            &catalog,
        );
        assert_eq!(found[0].apiosk.len(), 3);
        assert_eq!(
            found[0]
                .apiosk
                .iter()
                .filter_map(|candidate| candidate.endpoint_id.as_ref())
                .collect::<BTreeSet<_>>()
                .len(),
            3
        );
    }

    #[test]
    fn unsafe_endpoint_urls_are_not_displayed_and_search_never_adds_execution_urls() {
        for value in [
            "javascript:alert(1)",
            "http://example.test/source",
            "https://user:secret@example.test/source",
        ] {
            let mut row = service(
                1,
                "company.officers",
                "Officers",
                "Company directors",
                "registry",
            );
            row.source_url = Some(value.into());
            let catalog = catalog(vec![row], vec![]);
            let found = search(
                &parsed(vec![
                    json!({"slug":"company.officers","name":"Company officers"}),
                ]),
                &catalog,
            );
            assert!(found[0].apiosk[0].url.is_none());
        }
    }

    #[test]
    #[ignore = "optional local catalog snapshot precision check; reads only a local file and makes no network/model calls"]
    fn local_catalog_precision() {
        let path = std::env::var("ASK_V2_CATALOG_SNAPSHOT").unwrap();
        let catalog =
            Catalog::build(serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap());
        for (name, slug, title, outputs, hint, expected) in [
            (
                "Monzo officers and PSC",
                "company.profile.officers",
                "Company officers",
                vec![
                    "appointment_date",
                    "resignation_date",
                    "significant_control",
                ],
                Some("companies house"),
                vec!["company.officers", "company.significant-control"],
            ),
            (
                "Coolblue registered address and KVK",
                "company.profile.registered_address",
                "Registered company address and KVK number",
                vec!["registered_address", "kvk_number"],
                Some("kvk"),
                vec!["company.registry-profile"],
            ),
            (
                "Financial statements",
                "company.financial.statements",
                "Company financial statements",
                vec!["financial_statements"],
                None,
                vec!["company.financial-statements"],
            ),
            (
                "Sanctions",
                "compliance.sanctions",
                "Company sanctions screening",
                vec!["matching_records", "match_scores", "source_links"],
                None,
                vec!["company.sanctions-screen"],
            ),
        ] {
            let mut request = parsed(vec![json!({"slug":slug,"name":title})]);
            request.capabilities[0].outputs = outputs.into_iter().map(str::to_owned).collect();
            request.capabilities[0].source_hint = hint.map(str::to_owned);
            let found = search(&request, &catalog);
            println!(
                "{name}: {:?}",
                found[0]
                    .apiosk
                    .iter()
                    .map(|candidate| (
                        &candidate.source_slug,
                        &candidate.capability,
                        &candidate.availability
                    ))
                    .collect::<Vec<_>>()
            );
            for capability in expected {
                assert!(
                    found[0].operations.contains(capability),
                    "{name}: expected {capability}"
                );
            }
        }
    }
}
