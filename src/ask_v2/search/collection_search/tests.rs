use super::*;
use crate::capability_intake::ParsedRequest;
use serde_json::json;

fn catalog() -> Catalog {
    Catalog::build(
        serde_json::from_str(include_str!(
            "../../../../tests/fixtures/search/company-name-collections.json"
        ))
        .unwrap(),
    )
}

fn request() -> ParsedRequest {
    serde_json::from_value(json!({
        "language":"en", "subjects":[{"id":"s1","type":"company","label":"Example Company", "identifiers":{"company_name":"Example Company"}}],
        "capabilities":[{"slug":"company.search", "domain":"company", "name":"Company Search",
            "description":"Find companies matching the given name", "inputs":["company_name"],
            "outputs":["company_matches"], "subject_id":"s1", "source_hint":null,"confidence":1,"missing":[]}],
        "deliverable":{"format":"chat","operations":[]},
        "meta":{"model":"fixture","parsed_at":"2026-10-13T00:00:00Z","text_sha256":"fixture"}
    })).unwrap()
}

#[test]
fn generic_company_matches_find_typed_name_searches_without_a_provider() {
    let catalog = catalog();
    assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
    let found = super::super::search(&request(), &catalog);
    assert!(
        found[0].operations.contains("company_registry.search"),
        "{:?}",
        found[0].apiosk
    );
    assert!(
        found[0].operations.contains("company.legal-entity-search"),
        "{:?}",
        found[0].apiosk
    );
    assert!(!found[0].operations.contains("company.registry-profile"));
    assert!(!found[0].operations.contains("company.watchlist-search"));
}

#[test]
fn an_explicit_source_hint_restricts_the_same_generic_collection_search() {
    let catalog = catalog();
    let mut request = request();
    request.capabilities[0].source_hint = Some("kvk".into());
    let found = super::super::search(&request, &catalog);
    assert_eq!(
        found[0].operations,
        ["company_registry.search".into()].into()
    );
    assert!(found[0]
        .apiosk
        .iter()
        .all(|candidate| candidate.source_slug.as_deref() == Some("kvk-dutch-business-register")));
}

#[test]
fn generic_search_words_do_not_promote_single_records_opaque_outputs_or_other_domains() {
    let catalog = catalog();
    let selected = catalog
        .services
        .iter()
        .find(|service| service.contract.capability_slug == "company_registry.search")
        .unwrap();
    assert!(domains(selected, &catalog).contains("company"));
    let mut single = selected.clone();
    single.contract.cardinality = Cardinality::One;
    assert!(domains(&single, &catalog).is_empty());
    let mut unverified = selected.clone();
    unverified.unverified = true;
    assert!(domains(&unverified, &catalog).is_empty());
    let mut opaque = catalog.clone();
    opaque
        .vocabulary
        .get_mut("company.name")
        .unwrap()
        .value_schema = json!({});
    assert!(domains(selected, &opaque).is_empty());
    let mut other = request();
    other.capabilities[0].domain = "person".into();
    other.capabilities[0].slug = "person.search".into();
    other.capabilities[0].inputs = vec!["person_name".into()];
    other.capabilities[0].outputs = vec!["person_matches".into()];
    assert!(super::super::search(&other, &catalog)[0].apiosk.is_empty());
}
