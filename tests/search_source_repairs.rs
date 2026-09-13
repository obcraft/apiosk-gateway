use apiosk_gateway_v2::{
    catalog::Catalog,
    state::{Fact, Task},
};
use serde_json::{json, Value};
use uuid::Uuid;
#[test]
fn paid_search_and_fetch_responses_have_valid_bindings_and_strict_envelopes() {
    for (data, count) in [
        (
            include_str!("fixtures/search-source-repairs/catalog.json"),
            3,
        ),
        (include_str!("fixtures/browserbase/catalog.json"), 2),
    ] {
        let f: Value = serde_json::from_str(data).unwrap();
        let c = Catalog::build(serde_json::from_value(f.clone()).unwrap());
        assert_eq!(c.services.len(), count, "{:?}", c.rejected);
        for s in &c.services {
            let e = Uuid::new_v4();
            let mut t = Task::default();
            let raw = f["services"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["id"] == s.id.to_string())
                .unwrap();
            for (k, v) in raw["test_inputs"].as_object().unwrap() {
                t.facts.push(Fact {
                    entity_ref: e,
                    semantic_type: k.clone(),
                    value: v.clone(),
                    provenance: json!({"kind":"user_input"}),
                    observed_at: chrono::Utc::now(),
                    expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
                    verification: "user_asserted".into(),
                });
            }
            let inputs = s.inputs(&t, e, &c.vocabulary).unwrap();
            assert_eq!(
                inputs.as_object().unwrap().len(),
                s.fixed_params.as_object().unwrap().len() + 1
            );
            assert!(s.inputs(&Task::default(), e, &c.vocabulary).is_err());
            assert!(s
                .contract
                .extract(s.sample_payload.as_ref().unwrap(), &c.vocabulary)
                .unwrap()
                .rejected
                .is_empty());
            assert!(!s
                .contract
                .extract(&json!({"error":"payment required"}), &c.vocabulary)
                .unwrap()
                .rejected
                .is_empty());
            let empty = if s.api_slug == "browserbase" && s.path == "/fetch" {
                json!({"content":"","contentType":"text/html","statusCode":200})
            } else if s.api_slug == "stableenrich" || s.api_slug == "browserbase" {
                json!({"results":[]})
            } else if s.path.ends_with("search") {
                json!({"answer":"No result","sources":[]})
            } else {
                json!({"markdown":""})
            };
            assert!(s
                .contract
                .extract(&empty, &c.vocabulary)
                .unwrap()
                .rejected
                .is_empty());
        }
    }
}
