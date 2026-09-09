use apiosk_gateway_v2::catalog::{Catalog, Snapshot};
use serde_json::{json, Value};

fn fixture(text: &str) -> Snapshot { serde_json::from_str(text).unwrap() }

#[test]
fn original_multi_row_payloads_reproduce_five_contract_errors() {
    let s = fixture(include_str!("fixtures/direct-source-repairs.json"));
    let c = Catalog::build(s);
    assert_eq!(c.rejected.len(), 5, "Real multi-row payloads must expose the original bugs");
}

#[test]
fn repaired_contracts_preserve_all_rows_without_binding_unrelated_records_together() {
    let c = Catalog::build(fixture(include_str!("fixtures/direct-source-repaired.json")));
    assert!(c.rejected.is_empty(), "{:?}", c.rejected);
    assert_eq!(c.services.len(), 6);
    for s in &c.services {
        let e = s.contract.extract(s.sample_payload.as_ref().unwrap(), &c.vocabulary).unwrap();
        assert!(e.rejected.is_empty());
        assert_eq!(e.subjects.len(), 1);
        for name in s.contract.output_map.produces.keys() {
            assert!(e.subjects[0].fact(name).is_some(), "{} missing {name}", s.path);
        }
        if s.path.contains("basisprofielen") {
            assert_eq!(e.subjects[0].fact("company.name").unwrap().value, "Coolblue B.V.");
            assert_eq!(e.subjects[0].fact("company.profile").unwrap().value["trade_names"].as_array().unwrap().len(),9);
        }
        if s.path.contains("publicaties") {
            let page=&e.subjects[0].fact("tenderned.publication_page").unwrap().value;
            assert_eq!(page["content"].as_array().unwrap().len(),10);
            assert_eq!(page["totalElements"],221);
        }
        if s.path.contains("locatieserver") {
            let docs=&e.subjects[0].fact("pdok.search_page").unwrap().value["response"]["docs"];
            assert_eq!(docs.as_array().unwrap().len(),10);
            assert_eq!(docs[0]["type"],"weg");
            assert_eq!(docs[1]["huisnummer"],1);
            assert_eq!(docs[1]["postcode"],"1012JS");
        }
        if s.path.contains("82244NED") { assert_eq!(s.fixed_params["$orderby"], "Perioden desc"); }
    }
}

#[test]
fn valid_empty_searches_are_results_not_extraction_failures() {
    let c = Catalog::build(fixture(include_str!("fixtures/direct-source-repaired.json")));
    for s in &c.services {
        let data:Value = if s.path.contains("publicaties") { json!({"content":[],"totalElements":0}) }
            else if s.path.contains("locatieserver") { json!({"response":{"docs":[],"numFound":0}}) }
            else {continue};
        let e=s.contract.extract(&data,&c.vocabulary).unwrap();
        assert!(e.rejected.is_empty());
        assert_eq!(e.subjects.len(),1);
    }
}
