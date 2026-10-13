//! Keep the answer fixture coupled to the actual public-source adapter.
// The shared executor adapter also exposes operations outside this fixture.
#[allow(dead_code)]
#[path = "../legacy-executor/src/proxy/public_sources.rs"]
mod adapter;
use serde_json::{json, Value};

#[test]
fn saved_location_fixture_is_the_normalized_actual_public_chain() {
    let raw: Value = serde_json::from_str(include_str!(
        "fixtures/dutch-links/damrak-public-chain.json"
    ))
    .unwrap();
    let mut normalized = json!({"address":raw["address"]});
    let vbo = raw["address"]["response"]["docs"][0]["adresseerbaarobject_id"]
        .as_str()
        .unwrap();
    let code = raw["address"]["response"]["docs"][0]["buurtcode"]
        .as_str()
        .unwrap();
    let normalize = |route: &str, key: &str, value: &str| {
        let mut url = url::Url::parse(&format!("https://api.pdok.nl/apiosk/{route}")).unwrap();
        url.query_pairs_mut().append_pair(key, value);
        let request = adapter::prepare("pdok", "GET", url.as_str())
            .unwrap()
            .unwrap();
        adapter::normalize(&request, &raw[route]).unwrap()
    };
    normalized["bag-unit"] = normalize("bag-unit", "vbo", vbo);
    normalized["bag-address"] = normalize("bag-address", "vbo", vbo);
    normalized["building"] = normalize(
        "building",
        "href",
        normalized["bag-unit"]["matched"]["single_building_href"]
            .as_str()
            .unwrap(),
    );
    normalized["neighbourhood"] = normalize("neighbourhood", "code", code);
    let fixture = "tests/fixtures/dutch-links/damrak-normalized-chain.json";
    if std::env::var("LOCATION_ANSWER_FIXTURE_EXPORT").as_deref() == Ok("1") {
        std::fs::write(fixture, serde_json::to_vec_pretty(&normalized).unwrap()).unwrap();
    }
    let saved: Value = serde_json::from_slice(&std::fs::read(fixture).unwrap()).unwrap();
    assert_eq!(
        saved, normalized,
        "answer fixtures must retain actual normalized source values"
    );
}
