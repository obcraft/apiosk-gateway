#[allow(dead_code)]
#[path = "../crates/commerce-runtime/src/proxy/public_sources.rs"]
mod adapter;
use serde_json::{json, Value};
fn p(source: &str, method: &str, url: &str) -> adapter::Prepared {
    adapter::prepare(source, method, url).unwrap().unwrap()
}
fn raw(name: &str) -> Vec<u8> {
    std::fs::read(format!("tests/fixtures/eu-public-sources/{name}.body")).unwrap()
}
fn wms(source: &str, layer: &str) -> adapter::Prepared {
    let base = if source == "bodemloket" {
        "https://gis.gdngeoservices.nl/standalone/services/blk_gdn/lks_blk_rd_v1/MapServer/WMSServer"
    } else {
        "https://service.pdok.nl/kadaster/ruimtelijke-plannen/wms/v1_0"
    };
    p(
        source,
        "GET",
        &format!("{base}?BBOX=120900,487200,121000,487300&LAYERS={layer}"),
    )
}
#[test]
fn real_responses_normalize_for_every_requested_route() {
    let mut samples = serde_json::Map::new();
    let mut post = p(
        "vies",
        "POST",
        "https://ec.europa.eu/taxation_customs/vies/rest-api/check-vat-number",
    );
    adapter::bind_body(
        &mut post,
        br#"{"countryCode":"NL","vatNumber":"004495445B01"}"#,
    )
    .unwrap();
    let cases=vec![
 ("vies-status",p("vies","GET","https://ec.europa.eu/taxation_customs/vies/rest-api/check-status")),
 ("vies-get",p("vies","GET","https://ec.europa.eu/taxation_customs/vies/rest-api/ms/NL/vat/004495445B01")),
 ("vies-post",post),
 ("cvdr",p("koop","GET","https://zoekservice.overheid.nl/sru/Search?x-connection=cvdr&q=water&limit=2")),
 ("bwb-ok",p("koop","GET","https://zoekservice.overheid.nl/sru/Search?x-connection=BWB&q=water&limit=2")),
 ("cellar-json",p("eurlex-cellar","GET","https://publications.europa.eu/webapi/rdf/sparql?query=SELECT+%3Fs+WHERE+%7B+%3Fs+%3Fp+%3Fo+%7D+LIMIT+2")),
 ("celex-object",p("eurlex-cellar","GET","https://publications.europa.eu/resource/celex/32016R0679")),
 ("pdok",wms("pdok-ruimtelijke-plannen","enkelbestemming")),
 ("bodem",wms("bodemloket","WBB_locaties")),
 ("bodem-hit",wms("bodemloket","Beschikbaarheid_gegevens")),
 ("funding-en",p("eu-funding-tenders","POST","https://api.tech.ec.europa.eu/search-api/prod/rest/search?q=energy&limit=2")),
 ("staatscourant",p("koop","GET","https://repository.overheid.nl/sru?q=water&journal=Staatscourant&limit=2")),
 ];
    for (name, request) in cases {
        let d = adapter::normalize_bytes(&request, &raw(name))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(d["source_url"].is_string());
        assert!(d["notice"].is_string());
        match name {
            "vies-get" | "vies-post" => {
                assert_eq!(d["data"]["valid"], true);
                assert_eq!(d["data"]["vat_number"], "004495445B01")
            }
            "bodem" => assert_eq!(d["data"]["items"], json!([])),
            "cvdr" | "bwb-ok" | "pdok" | "bodem-hit" | "funding-en" => {
                assert!(!d["data"]["items"].as_array().unwrap().is_empty())
            }
            "celex-object" => assert_eq!(d["data"]["celex"], "32016R0679"),
            "staatscourant" => assert!(d["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["publication"] == "Staatscourant")),
            _ => {}
        }
        samples.insert(name.into(), d);
    }
    if std::env::var("EU_SOURCE_EXPORT").as_deref() == Ok("1") {
        std::fs::write(
            "tests/fixtures/eu-public-sources/normalized.json",
            serde_json::to_vec_pretty(&samples).unwrap(),
        )
        .unwrap();
    }
}
#[test]
fn diagnostics_identity_failures_and_malformed_payloads_are_not_results() {
    let cvdr = p(
        "koop",
        "GET",
        "https://zoekservice.overheid.nl/sru/Search?x-connection=cvdr&q=water",
    );
    assert!(adapter::normalize_bytes(
        &cvdr,
        b"<diagnostics><diagnostic>bad index</diagnostic></diagnostics>"
    )
    .is_err());
    assert!(adapter::normalize_bytes(
        &cvdr,
        b"<!DOCTYPE test [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><test>&x;</test>"
    )
    .is_err());
    let vies = p(
        "vies",
        "GET",
        "https://ec.europa.eu/taxation_customs/vies/rest-api/ms/NL/vat/004495445B01",
    );
    let mut d: Value = serde_json::from_slice(&raw("vies-get")).unwrap();
    d["userError"] = json!("MS_UNAVAILABLE");
    assert!(adapter::normalize_bytes(&vies, &serde_json::to_vec(&d).unwrap()).is_err());
    d["userError"] = json!("VALID");
    d["vatNumber"] = json!("other");
    assert!(adapter::normalize_bytes(&vies, &serde_json::to_vec(&d).unwrap()).is_err());
    let cellar = p(
        "eurlex-cellar",
        "GET",
        "https://publications.europa.eu/resource/celex/32016R0678",
    );
    assert!(adapter::normalize_bytes(&cellar, &raw("celex-object")).is_err());
    let funding = p(
        "eu-funding-tenders",
        "POST",
        "https://api.tech.ec.europa.eu/search-api/prod/rest/search?q=energy&limit=2",
    );
    let mut f: Value = serde_json::from_slice(&raw("funding-en")).unwrap();
    f["results"][0]["metadata"]["DATASOURCE"] = json!(["SEDIA_FAQ"]);
    assert!(adapter::normalize_bytes(&funding, &serde_json::to_vec(&f).unwrap()).is_err());
    assert!(adapter::multipart(&funding)
        .unwrap()
        .contains("\"language\":\"en\""));
    assert!(adapter::normalize_bytes(
        &wms("bodemloket", "WBB_locaties"),
        b"<ServiceException>bad</ServiceException>"
    )
    .is_err());
}
#[test]
fn invalid_inputs_stop_before_upstream() {
    for u in ["https://publications.europa.eu/webapi/rdf/sparql?query=SELECT+*+WHERE+%7B%7D", "https://publications.europa.eu/webapi/rdf/sparql?query=SELECT+*+WHERE+%7BSERVICE+%3Chttps://example.org%3E%7B%7D%7D+LIMIT+1"]{assert!(adapter::prepare("eurlex-cellar","GET",u).is_err());}
    for q in [
        "BBOX=NaN,0,1,2&LAYERS=bouwvlak",
        "BBOX=0,0,1,1&LAYERS=bouwvlak&I=101",
        "BBOX=0,0,1,1&LAYERS=bouwvlak&REQUEST=GetMap",
        "BBOX=0,0,1,1&LAYERS=unknown",
    ] {
        assert!(adapter::prepare(
            "pdok-ruimtelijke-plannen",
            "GET",
            &format!("https://service.pdok.nl/kadaster/ruimtelijke-plannen/wms/v1_0?{q}")
        )
        .is_err());
    }
    let mut v = p(
        "vies",
        "POST",
        "https://ec.europa.eu/taxation_customs/vies/rest-api/check-vat-number",
    );
    assert!(adapter::bind_body(&mut v, br#"{"countryCode":"NL"}"#).is_err());
}
#[test]
fn catalog_contracts_validate_extract_and_price_every_source() {
    use apiosk_gateway_v2::catalog::{Catalog, Snapshot};
    let bytes = std::fs::read("tests/fixtures/eu-public-sources/catalog.json").unwrap();
    let snapshot: Snapshot = serde_json::from_slice(&bytes).unwrap();
    let count = snapshot.services.len();
    let catalog = Catalog::build(snapshot);
    assert_eq!(catalog.rejected, vec![]);
    assert_eq!(catalog.services.len(), count);
    assert_eq!(count, 11);
    for s in &catalog.services {
        assert_eq!(s.total(), 97826);
        assert!((0.08..=0.10).contains(&(s.total() as f64 / 1e6 * 0.92)));
    }
}
