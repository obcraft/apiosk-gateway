use apiosk_gateway_v2::{
    catalog::{Catalog, Snapshot},
    resolver::Resolution,
    workflow_catalog,
};
use serde_json::{json, Value};

fn catalog() -> Catalog {
    Catalog::build(
        serde_json::from_str::<Snapshot>(include_str!("fixtures/workflows/catalog.json")).unwrap(),
    )
}

#[test]
fn all_eleven_workflows_are_available_and_each_recipe_prices_multiple_sources() {
    let c = catalog();
    assert!(c.rejected.is_empty(), "{:?}", c.rejected);
    let definitions =
        apiosk_gateway_v2::workflows::definitions(&c, &json!({"currency":"EUR","rate":"0.92"}));
    assert_eq!(definitions["workflows"].as_array().unwrap().len(), 11);
    let legacy = apiosk_gateway_v2::workflows::definitions_for_client(&c, &json!({}), false);
    assert_eq!(legacy["workflows"].as_array().unwrap().len(), 1);
    assert_eq!(legacy["workflows"][0]["slug"], "supplier-payment-check");
    for r in workflow_catalog::recipes() {
        let (intent, _, _) = workflow_catalog::prepare(&r.slug, &r.example, &c)
            .unwrap_or_else(|e| panic!("{}: {e}", r.slug));
        let Resolution::Plan(mut p) = workflow_catalog::plan_preview(&c, &intent) else {
            panic!(
                "{} did not resolve: {:?}",
                r.slug,
                workflow_catalog::plan_preview(&c, &intent)
            )
        };
        p.apply_markup(2000);
        let sources: std::collections::BTreeSet<_> =
            p.steps.iter().map(|s| &s.service.api_slug).collect();
        assert!(sources.len() >= 2, "{} must combine sources", r.slug);
        assert!(p.steps.len() <= 12);
        assert_eq!(
            p.cap_atomic,
            p.steps
                .iter()
                .map(|s| s.service.total_with(2000))
                .sum::<i64>()
        );
        println!(
            "{}: {} paid operations, {} sources, cap ${:.6}",
            r.slug,
            p.steps.len(),
            sources.len(),
            p.cap_atomic as f64 / 1e6
        );
    }
    assert!(
        definitions["workflows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["available"] != false),
        "{definitions}"
    );
}

#[test]
fn country_optional_and_comparison_variants_resolve() {
    let c = catalog();
    for slug in ["customer-onboarding", "company-health"] {
        for (country, number) in [("NL", "30204462"), ("GB", "09446231"), ("FR", "552032534")] {
            let mut input = workflow_catalog::recipe(slug).unwrap().example;
            input.insert("country".into(), json!(country));
            input.insert("registration".into(), json!(number));
            let (intent, _, _) = workflow_catalog::prepare(slug, &input, &c).unwrap();
            assert!(
                matches!(
                    workflow_catalog::plan_preview(&c, &intent),
                    Resolution::Plan(_)
                ),
                "{slug} {country}"
            );
        }
    }
    for slug in ["competitor-comparison", "company-ownership", "company-news"] {
        let mut input = workflow_catalog::recipe(slug).unwrap().example;
        match slug {
            "competitor-comparison" => {
                input.insert("url3".into(), json!("https://example.com/pricing"));
            }
            "company-ownership" => {
                input.insert("lei".into(), json!("724500Y6DUVHQD6OXN27"));
            }
            _ => {
                input.insert("insiders".into(), json!("No"));
            }
        }
        let (intent, _, _) = workflow_catalog::prepare(slug, &input, &c).unwrap();
        assert!(
            matches!(
                workflow_catalog::plan_preview(&c, &intent),
                Resolution::Plan(_)
            ),
            "{slug}"
        );
    }
}

#[test]
fn invalid_inputs_and_missing_sources_cannot_be_quoted() {
    let mut c = catalog();
    for (slug, key, value) in [
        ("company-news", "ticker", json!("AAPL")),
        ("company-ownership", "registration", json!("123")),
        ("competitor-comparison", "url1", json!("http://127.0.0.1/")),
        ("vehicle-precheck", "plate", json!("no?plate")),
        ("prospect-shortlist", "headcount_min", json!(-1)),
        ("customer-onboarding", "vat_country", Value::Null),
    ] {
        let mut input = workflow_catalog::recipe(slug).unwrap().example;
        input.insert(key.into(), value);
        assert!(
            workflow_catalog::prepare(slug, &input, &c).is_err(),
            "{slug} {key}"
        );
    }
    c.services.retain(|s| s.api_slug != "stableenrich");
    let r = workflow_catalog::recipe("prospect-shortlist").unwrap();
    assert!(workflow_catalog::prepare(&r.slug, &r.example, &c).is_err());
    let definitions = workflow_catalog::definitions(&c);
    assert_eq!(
        definitions
            .iter()
            .find(|r| r["slug"] == "prospect-shortlist")
            .unwrap()["available"],
        false
    );
}

#[test]
fn property_address_outputs_preserve_subject_identity_and_coordinates() {
    let c = catalog();
    let s = c
        .services
        .iter()
        .find(|s| s.contract.capability_slug == "location.property-search")
        .unwrap();
    let result = s
        .contract
        .extract(s.sample_payload.as_ref().unwrap(), &c.vocabulary)
        .unwrap();
    assert!(result.rejected.is_empty());
    assert!(!result.subjects.is_empty());
    for subject in result.subjects {
        assert!(subject
            .facts
            .iter()
            .any(|f| f.fact_type == "location.bag_object_id"));
        let maps: Vec<_> = subject
            .facts
            .iter()
            .filter(|f| f.fact_type.ends_with("b_b_o_x"))
            .collect();
        assert_eq!(maps.len(), 2);
        assert_eq!(maps[0].value, maps[1].value);
    }
}
