//! Optional, catalog-owned ways to extend a question about an existing subject.
//! Discovery never changes the parsed request or authorizes a source purchase.
use crate::{
    capability_intake::{Candidate, Capability, Match, ParsedRequest, Subject},
    catalog::{Catalog, Service},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const MAX_OPTIONS: usize = super::source_selection::MAX_SOURCES;
const CONNECTED_RELATED_OPTIONS: usize = 24;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SourceOption {
    pub source: Candidate,
    pub subject_id: String,
    pub requirement: Capability,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub input_values: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_inputs: Vec<String>,
    #[serde(default)]
    pub reason: String,
}

fn family(kind: &str) -> &str {
    match kind {
        "organization" | "organisation" | "business" | "employer" => "company",
        "individual" | "people" => "person",
        "address" | "place" | "city" | "town" | "property" | "building" => "location",
        other => other,
    }
}

fn namespace(field: &str) -> &str {
    field.split(['.', '_']).next().unwrap_or(field)
}

fn literal(field: &str, subject: &Subject, catalog: &Catalog) -> Option<Value> {
    let schema = &catalog.vocabulary.get(field)?.value_schema;
    let value = super::input_values::documented_literal(field, schema, subject)?;
    if catalog_core::facts::schema::validate(schema, &value).is_ok() {
        Some(value)
    } else {
        value.as_str().and_then(|text| serde_json::from_str(text).ok())
            .filter(|value| catalog_core::facts::schema::validate(schema, value).is_ok())
    }
}

pub(crate) fn missing(service: &Service, subject: &Subject, catalog: &Catalog) -> Vec<String> {
    service.contract.requires.iter().filter(|input| !input.optional)
        .filter(|input| literal(&input.fact_type, subject, catalog).is_none())
        .map(|input| input.fact_type.clone()).collect()
}

pub(crate) fn input_values(service: &Service, subject: &Subject, catalog: &Catalog) -> BTreeMap<String, Value> {
    service.contract.requires.iter()
        .filter_map(|input| literal(&input.fact_type, subject, catalog).map(|value| (input.fact_type.clone(), value))).collect()
}

/// A typed input is stronger than a shared tag. Broad query inputs alone must
/// not turn every web, person or location search into a company source option.
fn compatible(service: &Service, subject: &Subject, catalog: &Catalog) -> bool {
    let kind = family(&subject.kind);
    if matches!(kind, "topic" | "query" | "keyword" | "general") { return false; }
    let outputs: BTreeSet<_> = service.contract.output_map.produces.keys()
        .map(|field| namespace(field)).collect();
    // An endpoint for a different entity does not become relevant because its
    // provider also lists company, people or financial products in its tags.
    if (kind == "company" && outputs.contains("person"))
        || (kind == "person" && outputs.contains("company")) { return false; }
    let exact_output = outputs.contains(kind)
        || (kind == "location" && outputs.contains("building"));
    let typed_input = service.contract.requires.iter().any(|input| {
        let Some(fact) = catalog.vocabulary.get(&input.fact_type) else { return false; };
        let binds = literal(&input.fact_type, subject, catalog).is_some();
        let typed_key = namespace(&input.fact_type) == kind
            || (kind == "location" && namespace(&input.fact_type) == "building");
        let alias = super::input_values::reviewed_input_alias(&fact.value_schema, &format!("{kind}_name"));
        let description = super::search::stems(fact.value_schema["description"].as_str().unwrap_or(""));
        let named_type = description.contains(kind)
            || (kind == "company" && ["business", "organization", "organisation", "employer"].iter().any(|word| description.contains(*word)));
        (typed_key && (binds || (exact_output && !input.optional
            && fact.value_schema["type"].as_str() == Some("string"))))
            || (binds && (alias || named_type))
    });
    if !typed_input { return false; }
    // A cross-domain relation (for example company -> business locations) must
    // be described by the endpoint, not by the provider's generic category.
    let known = catalog.known_sources.iter().find(|known| known.slug == service.api_slug);
    let endpoint_text = service.description.as_deref().or_else(|| known.and_then(|known|
        known.metadata.endpoints.iter().find(|endpoint| endpoint.endpoint_id == service.endpoint_id)
            .map(|endpoint| endpoint.description.as_str()))).unwrap_or("");
    let terms = super::search::stems(endpoint_text);
    (exact_output && namespace(&service.contract.capability_slug) == kind)
        || terms.contains(kind) || (kind == "company"
        && ["business", "organization", "organisation", "employer"].iter().any(|word| terms.contains(*word)))
}

fn requirement(service: &Service, subject: &Subject, catalog: &Catalog) -> Capability {
    let domain = service.contract.output_map.produces.keys().map(|field| namespace(field))
        .find(|domain| ["company", "location", "compliance", "vehicle", "finance", "legal", "procurement", "document", "research"].contains(domain))
        .unwrap_or("general");
    let missing = missing(service, subject, catalog);
    let mut inputs: BTreeSet<_> = input_values(service, subject, catalog).into_keys().collect();
    inputs.extend(missing.iter().cloned());
    let name = service.description.as_deref().filter(|text| !text.trim().is_empty())
        .map(|text| text.split(". ").next().unwrap_or(text).chars().take(140).collect())
        .unwrap_or_else(|| service.contract.capability_slug.replace(['.', '-', '_'], " "));
    Capability {
        slug: service.contract.capability_slug.clone(),
        name,
        description: service.description.clone().unwrap_or_else(|| service.contract.capability_slug.clone()),
        domain: domain.into(),
        inputs: inputs.into_iter().collect(),
        outputs: service.contract.output_map.produces.keys().cloned().collect(),
        subject_id: Some(subject.id.clone()),
        // Country ranking must inspect the original subject, without treating
        // this suggested provider as if the user had explicitly requested it.
        source_hint: None,
        confidence: 1.0,
        missing,
    }
}

fn key(option: &SourceOption) -> (String, Option<String>, Option<String>, Option<String>) {
    (option.subject_id.clone(), option.source.source_slug.clone(), option.source.endpoint_id.clone(), option.source.capability.clone())
}

fn matched_options(parsed: &ParsedRequest, matches: &[Match], catalog: &Catalog) -> Vec<SourceOption> {
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    let mut occurrences = BTreeMap::<&str, usize>::new();
    for cap in &parsed.capabilities {
        let occurrence = occurrences.entry(&cap.slug).or_default();
        let found = matches.iter().filter(|found| found.slug == cap.slug).nth(*occurrence);
        *occurrence += 1;
        let Some(subject) = cap.subject_id.as_ref().and_then(|id| parsed.subjects.iter().find(|subject| &subject.id == id)) else { continue; };
        let Some(found) = found else { continue; };
        for source in found.apiosk.iter().filter(|source| source.origin == "apiosk" && source.availability == "supported") {
            let service = catalog.services.iter().find(|service| Some(service.endpoint_id.to_string()) == source.endpoint_id
                && Some(service.contract.capability_slug.clone()) == source.capability
                && Some(service.api_slug.clone()) == source.source_slug);
            let Some(service) = service.filter(|service| !service.unverified) else { continue; };
            let option = SourceOption { source: source.clone(), subject_id: subject.id.clone(), requirement: cap.clone(),
                missing_inputs: missing(service, subject, catalog),
                input_values: input_values(service, subject, catalog),
                reason: "Matches your question".into() };
            if seen.insert(key(&option)) { result.push(option); }
        }
    }
    result
}

/// A connected source picker must sign every executable choice displayed by
/// discovery. The ten-source purchase limit bounds a selected basket, not the
/// discovery list. Keep the established bounded related-source suggestions too.
pub(crate) fn connected_options(parsed: &ParsedRequest, matches: &[Match], catalog: &Catalog) -> Vec<SourceOption> {
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    let matched = matched_options(parsed, matches, catalog);
    let matched_keys: BTreeSet<_> = matched.iter().map(key).collect();
    for option in matched.into_iter()
        .chain(options_with_limit(parsed, matches, catalog, CONNECTED_RELATED_OPTIONS)) {
        if let Some(subject) = parsed.subjects.iter().find(|subject| subject.id == option.subject_id
            && matches!(subject.kind.as_str(), "location" | "place" | "region" | "country")) {
            let requested: Vec<_> = parsed.capabilities.iter().filter(|cap| cap.subject_id.as_ref() == Some(&option.subject_id)
                && !crate::capability_intake::answer_instruction(cap)).collect();
            let domains: BTreeSet<_> = requested.iter().filter_map(|cap|
                crate::parser::connected_planning::qualified_collection_domain(&cap.outputs, catalog)).collect();
            // Keep an out-of-scope match visible as evidence, but do not sign a
            // purchase choice that its fixed national contract cannot honor.
            if !domains.is_empty() && catalog.services.iter().find(|service|
                option.source.endpoint_id.as_deref() == Some(service.endpoint_id.to_string().as_str())
                    && option.source.capability.as_deref() == Some(service.contract.capability_slug.as_str())
                    && option.source.source_slug.as_deref() == Some(service.api_slug.as_str()))
                .filter(|service| domains.contains(service.contract.capability_slug.split('.').next().unwrap_or("")))
                .and_then(|service| super::search::fixed_endpoint_country(service, None))
                .is_some_and(|country| super::search::exact_country(&subject.label).as_ref() != Some(&country)) { continue; }
            if !matched_keys.contains(&key(&option)) && !domains.is_empty() && !domains.contains(&option.requirement.domain)
                && !requested.iter().any(|cap| cap.domain == option.requirement.domain) { continue; }
        }
        let identity = (option.source.endpoint_id.clone(), option.source.capability.clone(), option.subject_id.clone());
        if seen.insert(identity) { result.push(option); }
    }
    result
}

/// Offer reviewed related records for the existing typed subjects. Original
/// matches keep priority; everything else remains optional until selected.
pub(crate) fn options(parsed: &ParsedRequest, matches: &[Match], catalog: &Catalog) -> Vec<SourceOption> {
    options_with_limit(parsed, matches, catalog, MAX_OPTIONS)
}

fn options_with_limit(parsed: &ParsedRequest, matches: &[Match], catalog: &Catalog, limit: usize) -> Vec<SourceOption> {
    let mut result = matched_options(parsed, matches, catalog);
    let mut seen: BTreeSet<_> = result.iter().map(key).collect();
    for subject in &parsed.subjects {
        if !parsed.capabilities.iter().any(|cap| cap.subject_id.as_ref() == Some(&subject.id)
            && !crate::capability_intake::answer_instruction(cap)) { continue; }
        let mut candidates: Vec<_> = catalog.services.iter().filter(|service|
            service.contract.capability_slug != crate::website_read::CAPABILITY
                && !service.unverified && !service.is_registered_operation() && compatible(service, subject, catalog))
            .map(|service| (service, requirement(service, subject, catalog))).collect();
        // Prefer immediately usable operations; category and tag overlap break
        // ties without replacing the typed input/output compatibility proof.
        let context: BTreeSet<_> = parsed.capabilities.iter().filter(|cap| cap.subject_id.as_ref() == Some(&subject.id))
            .flat_map(|cap| super::search::stems(&format!("{} {}", cap.domain, cap.slug))).collect();
        let affinity = |service: &Service| catalog.known_sources.iter().find(|source| source.slug == service.api_slug)
            .map(|source| super::search::stems(&format!("{} {}", source.metadata.category, source.metadata.tags.join(" ")))
                .intersection(&context).count()).unwrap_or(0);
        candidates.sort_by(|(a, ca), (b, cb)| ca.missing.len().cmp(&cb.missing.len())
            .then(affinity(b).cmp(&affinity(a))).then(a.total().cmp(&b.total()))
            .then(a.contract.capability_slug.cmp(&b.contract.capability_slug)));
        // Reuse the same catalog index and jurisdiction rules as ordinary
        // discovery. Candidate-generated metadata is not a rewritten question.
        let mut probe = parsed.clone();
        probe.capabilities = candidates.iter().take(96).map(|(_, cap)| cap.clone()).collect();
        let discovered = super::search::search(&probe, catalog);
        for ((service, mut cap), found) in candidates.into_iter().take(96).zip(discovered) {
            let Some(source) = found.apiosk.into_iter().find(|source|
                source.endpoint_id.as_deref() == Some(service.endpoint_id.to_string().as_str())
                    && source.capability.as_deref() == Some(service.contract.capability_slug.as_str())) else { continue; };
            cap.source_hint = Some(service.api_slug.clone());
            let option = SourceOption { source, subject_id: subject.id.clone(), missing_inputs: cap.missing.clone(), requirement: cap,
                input_values: input_values(service, subject, catalog),
                reason: "Related records for this subject".into() };
            if seen.insert(key(&option)) { result.push(option); }
        }
    }
    // Round-robin the source groups. Forty typed operations from one provider
    // cannot consume the signed snapshot before another source gets a slot.
    fn diverse(options: Vec<SourceOption>) -> Vec<SourceOption> {
        let mut groups = Vec::<Vec<SourceOption>>::new();
        let mut positions = BTreeMap::new();
        for option in options {
            let next = groups.len();
            let index = *positions.entry(option.source.source_slug.clone()).or_insert_with(|| {
                groups.push(Vec::new()); next
            });
            groups[index].push(option);
        }
        // Surface usable alternatives from each provider before asking for
        // identifiers needed by its other endpoints. Selection remains explicit.
        let mut rows: Vec<_> = groups.into_iter().map(|mut group| {
            group.sort_by_key(|option| !option.missing_inputs.is_empty());
            group.into_iter()
        }).collect();
        let mut result = Vec::new();
        loop {
            let before = result.len();
            for row in &mut rows { if let Some(option) = row.next() { result.push(option); } }
            if result.len() == before { break; }
        }
        result
    }
    let mut result = diverse(result);
    result.truncate(limit);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Snapshot;
    use serde_json::json;

    fn catalog() -> Catalog {
        let mut services = BTreeMap::new();
        let mut vocabulary = BTreeMap::new();
        let mut sources = BTreeMap::new();
        for fixture in [include_str!("../../tests/fixtures/workflows/catalog.json"),
            include_str!("../../tests/fixtures/company-reputation/catalog.json"),
            include_str!("../../tests/fixtures/crunchbase/official-mcp.json")] {
            let snapshot: Snapshot = serde_json::from_str(fixture).unwrap();
            for service in snapshot.services { services.insert(service.id, service); }
            for fact in snapshot.vocabulary { vocabulary.insert(fact.slug.clone(), fact); }
            for source in snapshot.known_sources {
                sources.entry(source.slug.clone()).and_modify(|old: &mut crate::catalog::KnownSource|
                    old.metadata.endpoints.extend(source.metadata.endpoints.clone())).or_insert(source);
            }
        }
        let aliases: Value = serde_json::from_str(include_str!("../../tests/fixtures/ecb-split-currency/reviewed-schemas.json")).unwrap();
        for value in aliases["after"].as_array().unwrap() {
            if let Some(fact) = vocabulary.get_mut(value["slug"].as_str().unwrap()) { fact.value_schema = value["value_schema"].clone(); }
        }
        let names: Value = serde_json::from_str(include_str!("../../tests/fixtures/crunchbase/typed-name-input.json")).unwrap();
        vocabulary.get_mut(names["slug"].as_str().unwrap()).unwrap().value_schema = names["after"].clone();
        Catalog::build(Snapshot { services: services.into_values().collect(), vocabulary: vocabulary.into_values().collect(), known_sources: sources.into_values().collect() })
    }

    fn request(name: &str, country: Option<&str>) -> ParsedRequest {
        let mut identifiers = BTreeMap::from([("company_name".to_owned(), name.to_owned())]);
        if let Some(country) = country { identifiers.insert("country".into(), country.into()); }
        serde_json::from_value(json!({"language":"en", "subjects":[{"id":"s1","type":"company","label":name,"identifiers":identifiers}],
            "capabilities":[{"slug":"company.profile","name":"Company profile","description":"Registered company details", "domain":"company",
                "inputs":["company_name"],"outputs":["profile"],"subject_id":"s1","source_hint":null,"confidence":1.0}],
            "deliverable":{"format":"chat","operations":["summarize"]},"meta":{"model":"recorded","parsed_at":"2026-10-11T00:00:00Z","text_sha256":"fixture"}})).unwrap()
    }

    #[test]
    fn connected_picker_signs_every_displayed_choice_and_deduplicates_each_subject() {
        let catalog = catalog();
        let template = catalog.services.iter().find(|service| service.contract.capability_slug == "company.registry-profile").unwrap();
        let services: Vec<_> = (0..12).map(|index| {
            let mut service = template.clone();
            service.id = uuid::Uuid::from_u128(500_000 + index);
            service.endpoint_id = service.id;
            service.contract.capability_slug = format!("company.profile-{index}");
            service
        }).collect();
        let catalog = Catalog::build(Snapshot { services, vocabulary: catalog.vocabulary.into_values().collect(), known_sources: vec![] });
        assert_eq!(catalog.services.len(),12);
        let mut parsed = request("Financial Crime Compliance Associates",None);
        parsed.subjects[0].identifiers.insert("kvk_number".into(),"91251982".into());
        let mut other = parsed.subjects[0].clone(); other.id="s2".into(); parsed.subjects.push(other);
        let cap = parsed.capabilities[0].clone();
        let mut second_subject = cap.clone(); second_subject.subject_id=Some("s2".into());
        parsed.capabilities=vec![cap.clone(),cap,second_subject];
        let candidates: Vec<_> = catalog.services.iter().map(|service| Candidate {
            origin:"apiosk".into(),name:service.name.clone(),description:"Company profile".into(),
            source_slug:Some(service.api_slug.clone()),capability:Some(service.contract.capability_slug.clone()),
            endpoint_id:Some(service.endpoint_id.to_string()),url:None,method:service.method.clone(),price:None,
            network:None,calls_30d:None,availability:"supported".into(),
        }).collect();
        let matches: Vec<_> = parsed.capabilities.iter().map(|cap| Match {slug:cap.slug.clone(),query:cap.name.clone(),
            operations:catalog.services.iter().map(|service|service.contract.capability_slug.clone()).collect(),
            apiosk:candidates.clone(),coinbase:vec![],coinbase_status:"ready".into()}).collect();
        let app = options(&parsed,&matches,&catalog);
        assert_eq!(app.len(),MAX_OPTIONS,"the existing App shortlist stays bounded");
        let connected = connected_options(&parsed,&matches,&catalog);
        assert_eq!(connected.len(),24,"all twelve displayed choices remain selectable for each subject");
        for subject in ["s1","s2"] { for service in &catalog.services {
            assert_eq!(connected.iter().filter(|option| option.subject_id==subject
                && option.source.endpoint_id.as_deref()==Some(service.endpoint_id.to_string().as_str())
                && option.source.capability.as_deref()==Some(service.contract.capability_slug.as_str())).count(),1);
        }}
        let signer=crate::state::Signer::new(vec![7;32]).unwrap();
        let principal=crate::auth::Principal{owner:uuid::Uuid::new_v4(),workspace:None,connection:Some(uuid::Uuid::new_v4())};
        let response=json!({"matches":matches,"source_options":connected,"catalog_version":catalog.version,"apiosk_sources_searched":1});
        let token=crate::search_snapshot::Snapshot::issue_connected(&signer,&principal,&parsed,&response).unwrap();
        let opened=crate::search_snapshot::Snapshot::open_connected(&signer,&principal,&parsed,&token).unwrap();
        assert_eq!(opened.source_options().len(),24,"the full choice list survives bounded signed snapshot transport");
    }

    #[test]
    fn recorded_fcca_discovery_keeps_the_displayed_overheid_lookup_selectable() {
        let capture: Value=serde_json::from_str(include_str!("../../tests/fixtures/connected-selection/fcca-free.json")).unwrap();
        let parsed: ParsedRequest=serde_json::from_value(capture["parsed_request"].clone()).unwrap();
        let matches: Vec<Match>=serde_json::from_value(capture["matches"].clone()).unwrap();
        let catalog=Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/connected-selection/catalog.json")).unwrap());
        let endpoint="e7b4647f-f2aa-481e-b613-9ffbf4cad6fd";
        assert!(!capture["source_options"].as_array().unwrap().iter().any(|option|option["source"]["endpoint_id"]==endpoint),"recorded truncated list omitted the displayed lookup");
        let choices=connected_options(&parsed,&matches,&catalog);
        let lookup: Vec<_>=choices.iter().filter(|option|option.subject_id=="s1"
            && option.source.endpoint_id.as_deref()==Some(endpoint)
            && option.source.capability.as_deref()==Some("company_registry.lookup")).collect();
        assert_eq!(lookup.len(),1);
        assert!(lookup[0].missing_inputs.iter().any(|field|field=="company_registry.openkvk_slug"),"native slug stays required rather than inventing a name lookup");
        for source in matches.iter().flat_map(|found|&found.apiosk).filter(|source|source.availability=="supported") {
            if catalog.services.iter().any(|service| Some(service.endpoint_id.to_string())==source.endpoint_id
                && Some(service.contract.capability_slug.clone())==source.capability) {
                assert!(choices.iter().any(|option|option.source.endpoint_id==source.endpoint_id && option.source.capability==source.capability));
            }
        }
        let signer=crate::state::Signer::new(vec![7;32]).unwrap();
        let principal=crate::auth::Principal{owner:uuid::Uuid::new_v4(),workspace:None,connection:Some(uuid::Uuid::new_v4())};
        let response=json!({"matches":matches,"source_options":choices,"catalog_version":catalog.version,"apiosk_sources_searched":2});
        let token=crate::search_snapshot::Snapshot::issue_connected(&signer,&principal,&parsed,&response).unwrap();
        assert!(token.len()<32*1024,"the exact recorded selection snapshot remains well within its existing bounded transport");
        let opened=crate::search_snapshot::Snapshot::open_connected(&signer,&principal,&parsed,&token).unwrap();
        assert!(opened.source_options().iter().any(|option|option.source.endpoint_id.as_deref()==Some(endpoint)));
        assert_eq!(parsed.capabilities.len(),4,"original requested coverage is retained separately");
    }

    #[test]
    fn offers_typed_related_records_and_explicit_missing_location() {
        let catalog = catalog();
        let parsed = request("Example Coffee", None);
        let found = super::super::search::search(&parsed, &catalog);
        let choices = options(&parsed, &found, &catalog);
        let slugs: Vec<_> = choices.iter().filter_map(|option| option.source.capability.as_deref()).collect();
        assert!(slugs.contains(&"company_registry.search"), "{slugs:?}");
        assert!(slugs.contains(&"company.crunchbase-official-search"), "{slugs:?}");
        assert!(slugs.iter().any(|slug| slug.contains("glassdoor")), "{slugs:?}");
        let registry = choices.iter().find(|option| option.source.capability.as_deref() == Some("company.registry-profile")
            && option.source.source_slug.as_deref() == Some("kvk-dutch-business-register")).unwrap();
        assert!(!registry.input_values.contains_key("company.name"), "an unrequested lookup seed is not a profile input");
        assert!(registry.missing_inputs.iter().any(|field| field == "company_registry.kvknummer"), "collect the profile input before approval");
        assert!(!slugs.iter().any(|slug| slug.starts_with("person.")), "{slugs:?}");
        assert!(choices.len() <= MAX_OPTIONS);
        assert_eq!(parsed.capabilities.len(), 1, "options never mutate requested scope");
        assert!(choices.iter().all(|option| option.subject_id == "s1"));
        // A smaller shortlist may omit the provider's less-ready alternatives.
        // When a location-search option is offered, its missing input remains explicit.
        let local = Catalog::build(Snapshot {
            services: catalog.services.iter().filter(|service| service.contract.output_map.produces.contains_key("location.yelp_search_results")).cloned().collect(),
            vocabulary: catalog.vocabulary.into_values().collect(), known_sources: catalog.known_sources,
        });
        let local_choices = options(&parsed, &[], &local);
        let business = local_choices.first().expect("business-location records");
        assert_eq!(business.missing_inputs.len(), 1, "{business:?}");
        assert!(business.missing_inputs[0].ends_with(".location"));
        assert!(business.input_values.values().any(|value| value == "Example Coffee"));
    }

    #[test]
    fn respects_subject_country_and_never_borrows_another_subject_location() {
        let catalog = catalog();
        let mut parsed = request("Example GmbH", Some("Germany"));
        parsed.subjects.push(Subject { id: "s2".into(), label: "Seattle".into(), kind: "city".into(), identifiers: BTreeMap::new() });
        let choices = options(&parsed, &[], &catalog);
        assert!(choices.iter().all(|option| !option.source.source_slug.as_deref().is_some_and(|slug| slug == "kvk-dutch-business-register")), "{choices:?}");
        let business = choices.iter().find(|option| option.requirement.outputs.iter().any(|output| output == "location.yelp_search_results")).unwrap();
        assert!(business.missing_inputs.iter().any(|input| input.ends_with(".location")));
        assert!(choices.iter().all(|option| option.subject_id == "s1"));
    }

    #[test]
    fn supplied_location_binds_only_its_own_subject_and_wrong_identity_type_is_excluded() {
        let catalog = catalog();
        let mut parsed = request("Different Coffee", None);
        parsed.subjects[0].identifiers.insert("city".into(), "Boston".into());
        let choices = options(&parsed, &[], &catalog);
        let business = choices.iter().find(|option| option.requirement.outputs.iter().any(|output| output == "location.yelp_search_results")).unwrap();
        assert!(business.missing_inputs.is_empty(), "{business:?}");
        parsed.subjects[0].kind = "person".into();
        parsed.subjects[0].identifiers.clear();
        let choices = options(&parsed, &[], &catalog);
        assert!(choices.iter().all(|option| !option.requirement.outputs.iter().any(|output| output.starts_with("company."))));
    }

    #[test]
    fn reviewed_employee_rating_alias_does_not_claim_customer_ratings() {
        let fixture: Value = serde_json::from_str(include_str!("../../tests/fixtures/glassdoor-employee-rating/reviewed-schema.json")).unwrap();
        let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../tests/fixtures/company-reputation/catalog.json")).unwrap();
        for version in ["before", "after"] {
            snapshot.vocabulary.iter_mut().find(|fact| fact.slug == fixture["slug"].as_str().unwrap()).unwrap().value_schema = fixture[version].clone();
            let catalog = Catalog::build(snapshot.clone());
            assert!(catalog.rejected.is_empty());
            let service = catalog.services.iter().find(|service| service.contract.output_map.produces.contains_key("company.glassdoor_search_results")).unwrap();
            let mut requirement = request("Different Employer", None).capabilities.remove(0);
            for output in ["rating", "average_rating", "employer_rating", "employee_rating", "customer_rating"] {
                requirement.outputs = vec![output.into()];
                let expected = output != "customer_rating" && (output != "employee_rating" || version == "after");
                assert_eq!(crate::parser::plan_coverage::outputs_cover(&requirement, service, &catalog), expected, "{version}: {output}");
            }
            catalog_core::facts::schema::validate(&fixture[version], &json!({"document":{"status":"OK","data":[{"rating":3.5,"review_count":123}]}})).unwrap();
            assert!(catalog_core::facts::schema::validate(&fixture[version], &json!({"document":{"status":"OK","data":[{"rating":"3.5"}]}})).is_err());
        }
    }

    #[test]
    fn official_typed_name_searches_offer_only_the_correct_entity_collection() {
        let catalog = catalog();
        for (kind, name, expected, forbidden) in [
            ("company", "Different Robotics", "company.crunchbase-official-search", "person.crunchbase-search"),
            ("person", "Ada Different", "person.crunchbase-search", "company.crunchbase-official-search"),
        ] {
            let mut parsed = request(name, None);
            parsed.subjects[0].kind = kind.into(); parsed.subjects[0].identifiers.clear();
            let choices = options(&parsed, &[], &catalog);
            let official = choices.iter().find(|option| option.source.capability.as_deref() == Some(expected)).expect(expected);
            assert_eq!(official.input_values["crunchbase.entity_autocomplete_query"], name);
            assert!(official.missing_inputs.is_empty());
            assert!(choices.iter().all(|option| option.source.capability.as_deref() != Some(forbidden)));
        }
    }

    #[test]
    fn metadata_overlap_and_many_operations_do_not_erase_source_diversity() {
        let catalog = catalog();
        let template = catalog.services.iter().find(|service| service.contract.capability_slug == "company.crunchbase-search").unwrap().clone();
        let mut services = catalog.services.clone();
        for index in 0..40 {
            let mut service = template.clone();
            service.id = uuid::Uuid::from_u128(80_000 + index);
            service.endpoint_id = service.id;
            service.api_slug = "many-company-records".into();
            service.contract.capability_slug = format!("company.record-{index}");
            service.list_atomic = 0;
            services.push(service);
        }
        let mut unrelated = template.clone();
        unrelated.id = uuid::Uuid::from_u128(90_001);
        unrelated.endpoint_id = unrelated.id;
        unrelated.api_slug = "geographic-records".into();
        unrelated.contract.capability_slug = "area.boundary".into();
        unrelated.description = Some("Return an administrative area's geometry by its name".into());
        services.push(unrelated);
        let mut browse = template;
        browse.id = uuid::Uuid::from_u128(90_002);
        browse.endpoint_id = browse.id;
        browse.contract.capability_slug = "company.browse".into();
        browse.contract.requires.iter_mut().for_each(|input| input.optional = true);
        browse.contract.requires[0].fact_type = "company.crunchbase_limit".into();
        let binding = browse.contract.input_map.values().next().unwrap().clone();
        browse.contract.input_map = BTreeMap::from([("company.crunchbase_limit".into(), binding)]);
        services.push(browse);
        let mut vocabulary: Vec<_> = catalog.vocabulary.into_values().collect();
        vocabulary.push(serde_json::from_value(json!({"slug":"company.crunchbase_limit","value_schema":{"type":"integer","minimum":1}})).unwrap());
        let catalog = Catalog::build(Snapshot { services, vocabulary, known_sources: catalog.known_sources });
        let choices = options(&request("Unseen Name", None), &[], &catalog);
        assert_eq!(choices.len(), MAX_OPTIONS);
        assert!(choices.iter().any(|option| option.requirement.outputs.contains(&"company.glassdoor_search_results".into())));
        assert!(choices.iter().filter_map(|option| option.source.source_slug.as_deref()).collect::<BTreeSet<_>>().len() >= 5);
        assert!(choices.iter().filter(|option| option.source.source_slug.as_deref() == Some("many-company-records")).count() <= 2);
        assert!(choices.iter().all(|option| !matches!(option.source.capability.as_deref(), Some("area.boundary" | "company.browse"))));
        let connected = connected_options(&request("Unseen Name", None), &[], &catalog);
        assert_eq!(connected.len(), CONNECTED_RELATED_OPTIONS, "connected callers can review more related choices without changing the App limit");
        assert!(connected.iter().filter_map(|option| option.source.source_slug.as_deref()).collect::<BTreeSet<_>>().len() >= 5);
    }

    #[tokio::test]
    #[ignore = "reads production catalog only; no model or source calls"]
    async fn live_catalog_related_options() {
        let catalog = if let Ok(path) = std::env::var("ASK_SOURCE_OPTIONS_CATALOG") {
            std::sync::Arc::new(Catalog::build(serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()))
        } else {
            dotenvy::dotenv().ok();
            let pool = sqlx::postgres::PgPoolOptions::new().max_connections(1)
                .connect(&std::env::var("DATABASE_URL").unwrap()).await.unwrap();
            let catalog = crate::catalog::CatalogCache::new(pool.clone()).get().await.unwrap();
            pool.close().await;
            catalog
        };
        std::fs::write("/tmp/apiosk-source-options-catalog.json", serde_json::to_vec(&json!({
            "services":catalog.services, "vocabulary":catalog.vocabulary.values().collect::<Vec<_>>(),
            "known_sources":catalog.known_sources})).unwrap()).unwrap();
        for (name, country) in [("Mollie B.V.", Some("Netherlands")), ("Starbucks", None), ("Monzo", Some("United Kingdom"))] {
            let parsed = request(name, country);
            let matches = super::super::search::search(&parsed, &catalog);
            let choices = options(&parsed, &matches, &catalog);
            println!("{}", json!({"subject":name,"options":choices}));
            assert!(choices.len() <= MAX_OPTIONS);
            assert!(choices.iter().all(|option| !option.input_values.is_empty() || !option.missing_inputs.is_empty()),
                "named-subject options need a literal binding or explicit missing input");
            assert!(choices.iter().all(|option| option.source.capability.as_deref() != Some("area.administrative-boundary")));
            for term in ["crunchbase", "glassdoor", "yelp"] {
                assert!(choices.iter().any(|option| format!("{} {}", option.source.name, option.source.capability.as_deref().unwrap_or("")).to_lowercase().contains(term)), "missing {term} for {name}");
            }
        }
    }
}
