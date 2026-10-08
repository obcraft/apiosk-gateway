use apiosk_gateway_v2::{
    catalog::{Catalog, Snapshot},
    resolver::Resolution,
    workflow_catalog,
};
use serde_json::{json, Value};

fn catalog() -> Catalog {
    let mut snapshot=serde_json::from_str::<Snapshot>(include_str!("fixtures/workflows/catalog.json")).unwrap();
    let added:Snapshot=serde_json::from_str(include_str!("fixtures/source-operations/catalog.json")).unwrap();
    for service in added.services.into_iter().filter(|s|s.contract.capability_slug=="web.source-fetch") { if !snapshot.services.iter().any(|s|s.id == service.id) { snapshot.services.push(service); } }
    for word in added.vocabulary {if !snapshot.vocabulary.iter().any(|w|w.slug==word.slug){snapshot.vocabulary.push(word);}}
    Catalog::build(snapshot)
}

#[test]
fn all_workflows_are_available_and_each_recipe_prices_its_needed_sources() {
    let c = catalog();
    assert!(c.rejected.is_empty(), "{:?}", c.rejected);
    let definitions =
        apiosk_gateway_v2::workflows::definitions(&c, &json!({"currency":"EUR","rate":"0.92"}));
    assert_eq!(definitions["workflows"].as_array().unwrap().len(), 14);
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
        assert!(!sources.is_empty(), "{} needs source evidence", r.slug);
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

#[test]
fn recipes_only_quote_relevant_country_and_optional_steps() {
    let c = catalog();
    let planned = |slug: &str, edits: &[(&str, Value)]| {
        let mut input=workflow_catalog::recipe(slug).unwrap().example;
        for (key,value) in edits { if value.is_null() {input.remove(*key);} else {input.insert((*key).into(),value.clone());} }
        let (intent,_,_)=workflow_catalog::prepare(slug,&input,&c).unwrap();
        let Resolution::Plan(plan)=workflow_catalog::plan_preview(&c,&intent) else {panic!("{slug} did not plan")};
        plan.steps.iter().map(|step|(step.service.api_slug.clone(),step.service.contract.capability_slug.clone())).collect::<Vec<_>>()
    };
    let prospects=planned("prospect-shortlist",&[]);
    assert_eq!(prospects.len(),2);
    assert!(!prospects.iter().any(|(source,_)|source=="linkup"));
    let comparison=planned("competitor-comparison",&[]);
    assert_eq!(comparison.len(),2);
    assert!(comparison.iter().all(|(_,cap)|cap=="web.source-fetch"));
    assert_eq!(planned("competitor-comparison",&[("url3",json!("https://example.com/pricing"))]).len(),3);
    let onboarding=planned("customer-onboarding",&[("country",json!("GB")),("registration",json!("09446231")),("vat_number",Value::Null),("vat_country",Value::Null)]);
    assert_eq!(onboarding.len(),4);
    assert!(!onboarding.iter().any(|(source,_)|["vies","kvk-dutch-business-register","sirenic"].contains(&source.as_str())));
    let funding=planned("subsidy-finder",&[("country",json!("France"))]);
    assert_eq!(funding.len(),2);
    assert!(!funding.iter().any(|(source,_)|source=="rvo"));
    assert_eq!(planned("subsidy-finder",&[]).len(),3);
    assert_eq!(planned("subsidy-finder",&[("country",json!("nederland"))]).len(),3);
    assert_eq!(planned("subsidy-finder",&[("country",json!("NL"))]).len(),3);
    assert_eq!(planned("company-ownership",&[]).len(),4);
    assert_eq!(planned("company-ownership",&[("lei",json!("724500Y6DUVHQD6OXN27"))]).len(),7);
    let news=planned("company-news",&[("insiders",json!("No"))]);
    assert_eq!(news.len(),4);
    assert!(!news.iter().any(|(source,cap)|source=="bloomberg" || cap.contains("insider")));
    assert_eq!(planned("company-news",&[("insiders",json!("Yes"))]).len(),5);

    // Public steps carry the execution conditions, rather than an unrelated UI recipe.
    for (recipe,definition) in workflow_catalog::recipes().iter().zip(workflow_catalog::definitions(&c)) {
        let steps=definition["steps"].as_array().unwrap();
        assert_eq!(steps.len(),recipe.steps.len());
        for (public,actual) in steps.iter().zip(&recipe.steps) {
            assert_eq!(public["when"],serde_json::to_value(&actual.when).unwrap());
            if actual.source.is_empty(){assert_eq!(public["sources"].as_array().unwrap().len(),c.services.iter().filter(|s|s.contract.capability_slug==actual.capability).map(|s|&s.api_slug).collect::<std::collections::BTreeSet<_>>().len());}
            else {assert_eq!(public["sources"][0]["slug"],actual.source);}
            assert!(!public["label"].as_str().unwrap().is_empty());
        }
    }
}

#[test]
fn european_dossiers_quote_exact_country_coverage_and_registry_bound_screening() {
    let c = catalog();
    for slug in ["european-company-dossier", "tender-company-dossier"] {
        let recipe = workflow_catalog::recipe(slug).unwrap();
        assert_eq!(recipe.country_coverage.len(), if slug == "european-company-dossier" {11} else {2});
        for country in &recipe.country_coverage {
            let mut input = recipe.example.clone();
            input.extend(country.example.clone());
            input.insert("country".into(), json!(country.country));
            let (intent, _, _) = workflow_catalog::prepare(slug, &input, &c).unwrap();
            assert!(!intent.subjects[0].values.contains_key("company.name"), "screen the registry-returned name, not the supplied label");
            let Resolution::Plan(plan) = workflow_catalog::plan_preview(&c, &intent) else { panic!("{slug} {}", country.country) };
            assert_eq!(plan.steps.len(), if slug == "european-company-dossier" {3} else {5}, "{slug} {}", country.country);
            let profile = plan.steps.iter().position(|s|s.service.contract.capability_slug == "company.registry-profile").unwrap();
            let screening = plan.steps.iter().position(|s|s.service.contract.capability_slug == "company.sanctions-screen").unwrap();
            assert!(profile < screening, "registered identity must precede screening");
            assert!(!plan.steps.iter().any(|s|["linkup", "exa", "reuters"].contains(&s.service.api_slug.as_str())));
        }
        let mut unsupported = recipe.example.clone();
        unsupported.insert("country".into(), json!("DE"));
        assert!(workflow_catalog::prepare(slug, &unsupported, &c).is_err());
    }
    let mut outage = c.clone();
    outage.services.retain(|s|s.api_slug != "kvk-dutch-business-register");
    let outage = Catalog::build(Snapshot { services: outage.services, vocabulary: outage.vocabulary.into_values().collect(), known_sources: outage.known_sources });
    let definitions = workflow_catalog::definitions(&outage);
    let dossier = definitions.iter().find(|d|d["slug"] == "european-company-dossier").unwrap();
    assert_eq!(dossier["available"], true, "an NL outage must not disable FR/GB");
    for country in dossier["country_coverage"].as_array().unwrap() {
        assert_eq!(country["available"], country["country"] != "NL", "{country}");
    }
    let mut screening_outage = c;
    screening_outage.services.retain(|s|s.api_slug != "opensanctions");
    let screening_outage = Catalog::build(Snapshot { services: screening_outage.services, vocabulary: screening_outage.vocabulary.into_values().collect(), known_sources: screening_outage.known_sources });
    let definitions = workflow_catalog::definitions(&screening_outage);
    assert_eq!(definitions.iter().find(|d|d["slug"] == "european-company-dossier").unwrap()["available"], false);
}

#[test]
fn pre_kyb_standard_and_enhanced_quote_only_supported_selected_checks() {
    let c = catalog();
    for (country, registration, enhanced_count) in [("NL", "30204462", 7), ("GB", "09446231", 9), ("FR", "552032534", 8)] {
        for level in ["Standard", "Enhanced"] {
            for registration_supplied in [true, false] {
                let mut input = std::collections::BTreeMap::from([
                    ("name".into(), json!("Example company")), ("country".into(), json!(country)),
                    ("screening_level".into(), json!(level)), ("domain".into(), json!("example.com")),
                    ("vat_country".into(), json!("NL")), ("vat_number".into(), json!("815839091B01")),
                    ("iban".into(), json!("nl91 abna 0417 1643 00")),
                ]);
                if registration_supplied { input.insert("registration".into(), json!(registration)); }
                let (intent, context, _) = workflow_catalog::prepare("pre-kyb-screening", &input, &c).unwrap();
                let Resolution::Plan(mut plan) = workflow_catalog::plan_preview(&c, &intent) else {
                    panic!("pre-kyb {country} {level}, registration={registration_supplied}: {:?}", workflow_catalog::plan_preview(&c, &intent));
                };
                let caps: Vec<_> = plan.steps.iter().map(|s| s.service.contract.capability_slug.as_str()).collect();
                let search_count = caps.iter().filter(|cap| ["company_registry.search", "company.uk-registry-search", "company.fr-registry-search"].contains(cap)).count();
                assert_eq!(search_count, usize::from(!registration_supplied));
                assert_eq!(plan.steps.len(), if level == "Standard" { 5 } else { enhanced_count } + search_count, "{country} {level}: {caps:?}");
                assert_eq!(caps.contains(&"domain.mail-dns"), level == "Enhanced");
                assert_eq!(caps.contains(&"company.officers"), level == "Enhanced" && country == "GB");
                assert_eq!(caps.contains(&"company.significant-control"), level == "Enhanced" && country == "GB");
                assert!(intent.subjects.iter().any(|s| s.values.get("bank.iban") == Some(&json!("NL91ABNA0417164300"))));
                if registration_supplied {
                    assert!(!intent.subjects[0].values.contains_key("company.name"));
                    let profile = caps.iter().position(|cap| *cap == "company.registry-profile").unwrap();
                    let sanctions = caps.iter().position(|cap| *cap == "company.sanctions-screen").unwrap();
                    assert!(profile < sanctions, "screen the register-returned name when registration is supplied");
                }
                plan.apply_markup(2000);
                assert_eq!(plan.cap_atomic, plan.steps.iter().map(|s| s.service.total_with(2000)).sum::<i64>());
                assert!(context.limitations.iter().any(|s| s.contains("not individually screened")));
            }
        }
    }
}

#[test]
fn pre_kyb_name_only_remains_executable_and_optional_checks_are_not_purchased() {
    let c = catalog();
    for country in ["NL", "GB", "FR"] {
        let input = std::collections::BTreeMap::from([
            ("name".into(), json!("Example company")), ("country".into(), json!(country)),
            ("screening_level".into(), json!("Standard")),
        ]);
        let (intent, _, _) = workflow_catalog::prepare("pre-kyb-screening", &input, &c).unwrap();
        let Resolution::Plan(plan) = workflow_catalog::plan_preview(&c, &intent) else { panic!("{country}") };
        assert_eq!(plan.steps.len(), 3);
        assert!(plan.steps.iter().all(|s| !["domain-intelligence", "iban-check", "vies"].contains(&s.service.api_slug.as_str())));
    }
    for (key, value) in [("country", json!("DE")), ("screening_level", json!("Other")), ("screening_level", Value::Null), ("vat_country", json!("NL")), ("wallet", json!("0x0000000000000000000000000000000000000000")), ("iban", json!("not-an-iban"))] {
        let mut input = workflow_catalog::recipe("pre-kyb-screening").unwrap().example;
        input.insert(key.into(), value);
        assert!(workflow_catalog::prepare("pre-kyb-screening", &input, &c).is_err(), "{key}");
    }
    let mut unavailable = c.clone();
    unavailable.services.retain(|s| s.api_slug != "opensanctions");
    let r = workflow_catalog::recipe("pre-kyb-screening").unwrap();
    assert!(workflow_catalog::prepare(&r.slug, &r.example, &unavailable).is_err());
    let guidance = workflow_catalog::prepare(&r.slug, &r.example, &c).unwrap().0.answer_operations.join(" ");
    for phrase in ["Pass, Review or Reject", "requires Review", "Reject only", "matching identifiers", "unselected optional checks", "not a compliance certification", "saved evidence", "never invent links"] {
        assert!(guidance.contains(phrase), "missing decision guardrail: {phrase}");
    }
    assert!(!guidance.contains("$0.36") && !guidance.contains("100 pre-screens"));
}

#[test]
fn pre_kyb_availability_uses_minimal_per_country_inputs() {
    for excluded in ["domain-intelligence", "companies-house"] {
        let c = catalog();
        let c = Catalog::build(Snapshot {
            services: c.services.into_iter().filter(|s| s.api_slug != excluded).collect(),
            vocabulary: c.vocabulary.into_values().collect(), known_sources: c.known_sources,
        });
        let definitions = workflow_catalog::definitions(&c);
        let definition = definitions.iter().find(|r| r["slug"] == "pre-kyb-screening").unwrap();
        assert_eq!(definition["available"], true, "{excluded}");
        for coverage in definition["country_coverage"].as_array().unwrap() {
            assert_eq!(coverage["available"], !(excluded == "companies-house" && coverage["country"] == "GB"), "{excluded}: {coverage}");
        }
    }
}
