//! Local planning regressions for the published multi-source examples.
//! No model key, database, provider, wallet or network connection is used.
use crate::{
    catalog::{Catalog, Snapshot},
    parser::{Intent, Parser},
    resolver::{Plan, Resolution},
    state::Task,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::Ordering,
};

struct Case {
    title: &'static str,
    question: &'static str,
    providers: &'static [&'static str],
    operations: &'static [&'static str],
}

fn catalog() -> Catalog {
    let mut services = BTreeMap::new();
    let mut vocabulary = BTreeMap::new();
    let mut known_sources = BTreeMap::new();
    for text in [
        include_str!("../tests/fixtures/workflows/catalog.json"),
        include_str!("../tests/fixtures/dutch-links/catalog.json"),
        include_str!("../tests/fixtures/profession-output-coverage/catalog.json"),
        include_str!("../tests/fixtures/statistics-presets.json"),
        include_str!("../tests/fixtures/tenderned-browse.json"),
        include_str!("../tests/fixtures/cbs-bankruptcy-comparison-catalog.json"),
        include_str!("../tests/fixtures/frankfurter/catalog.json"),
    ] {
        let snapshot: Snapshot = serde_json::from_str(text).unwrap();
        for service in snapshot.services {
            services.insert(
                (
                    service.api_slug.clone(),
                    service.contract.capability_slug.clone(),
                ),
                service,
            );
        }
        for fact in snapshot.vocabulary {
            vocabulary.insert(fact.slug.clone(), fact);
        }
        for source in snapshot.known_sources {
            known_sources.insert(source.slug.clone(), source);
        }
    }
    // The same reviewed schemas are already used by profession planning tests.
    let reviewed: Value = serde_json::from_str(include_str!(
        "../tests/fixtures/profession-output-coverage/reviewed-schemas.json"
    ))
    .unwrap();
    for (slug, fact) in &mut vocabulary {
        if let Some(schema) = reviewed["schemas"].get(slug) {
            fact.value_schema = schema.clone();
        }
    }
    // Refresh the three historical opaque PDOK records from the applied,
    // reviewed metadata migration; this is the production schema, not a test
    // relaxation or an invented field declaration.
    let pdok =
        include_str!("../supabase/migrations/20261002204215_describe_pdok_record_coverage.sql");
    for line in pdok
        .lines()
        .filter(|line| line.starts_with("UPDATE catalog.fact_types SET value_schema=$schema$"))
    {
        let slug = line
            .split("WHERE slug='")
            .nth(1)
            .unwrap()
            .split('\'')
            .next()
            .unwrap();
        let schema = serde_json::from_str(line.split("$schema$").nth(1).unwrap()).unwrap();
        vocabulary.get_mut(slug).unwrap().value_schema = schema;
    }
    let catalog = Catalog::build(Snapshot {
        services: services.into_values().collect(),
        vocabulary: vocabulary.into_values().collect(),
        known_sources: known_sources.into_values().collect(),
    });
    assert!(
        catalog.rejected.is_empty(),
        "fixture contracts rejected: {:?}",
        catalog.rejected
    );
    catalog
}

fn parser() -> Parser {
    // reqwest cannot dispatch this scheme, even if a regression calls a model.
    Parser::new(
        reqwest::Client::new(),
        "offline://model-calls-forbidden".into(),
        String::new(),
        "fixture".into(),
    )
}

async fn plan(question: &str, catalog: &Catalog) -> (Task, Intent, Plan) {
    let parser = parser();
    let task = Task::default();
    let intake = parser
        .ask_intake(question, &task, catalog, None)
        .await
        .unwrap();
    assert_eq!(intake.parsed_request.language, "en");
    let reply = parser
        .chat_from_capabilities(question, None, &task, catalog, &intake)
        .await
        .unwrap();
    assert_eq!(
        parser.calls.load(Ordering::Relaxed),
        0,
        "literal compounds must not call a model"
    );
    assert!(reply.discover, "{question}\n{}", reply.message);
    let mut intent = reply
        .intent
        .expect("complete discover reply must include intent");
    assert!(intent.unresolved_references.is_empty());
    assert!(
        intent
            .answer_operations
            .iter()
            .any(|operation| operation.to_lowercase().contains("combin")),
        "joint synthesis must remain requested"
    );
    let mut task = Task {
        capability_intake: Some(intake),
        ..task
    };
    crate::engine::bind_subjects(&mut task, catalog, &mut intent).unwrap();
    let resolution = crate::resolver::resolve(catalog, &task, &intent);
    let Resolution::Plan(plan) = resolution else {
        panic!("{question}\nExpected complete plan, got {resolution:?}")
    };
    assert!(!plan.quote_ref.is_nil());
    assert!(plan.cap_atomic > 0);
    assert_eq!(
        plan.cap_atomic,
        plan.steps
            .iter()
            .map(|step| plan.step_total(step))
            .sum::<i64>(),
        "one total cap must cover every quoted source step"
    );
    assert!(
        plan.steps
            .iter()
            .map(|step| step.service.api_slug.as_str())
            .collect::<BTreeSet<_>>()
            .len()
            >= 2,
        "{question}: distinct providers required"
    );
    assert_eq!(
        plan.steps
            .iter()
            .map(|step| step.key())
            .collect::<BTreeSet<_>>()
            .len(),
        plan.steps.len(),
        "no repeated purchase of the same operation/subject"
    );
    (task, intent, plan)
}

fn inputs(plan: &Plan, task: &Task, catalog: &Catalog, operation: &str) -> Value {
    let selected: Vec<_> = plan
        .steps
        .iter()
        .filter(|step| step.service.contract.capability_slug == operation)
        .collect();
    assert_eq!(
        selected.len(),
        1,
        "expected one exact operation {operation}"
    );
    selected[0]
        .service
        .inputs(task, selected[0].entity_ref, &catalog.vocabulary)
        .unwrap()
}

fn assert_scope(case: &Case, plan: &Plan) {
    assert_eq!(
        plan.steps
            .iter()
            .map(|step| step.service.api_slug.as_str())
            .collect::<BTreeSet<_>>(),
        case.providers.iter().copied().collect(),
        "{} provider scope",
        case.title
    );
    assert_eq!(
        plan.steps
            .iter()
            .map(|step| step.service.contract.capability_slug.as_str())
            .collect::<BTreeSet<_>>(),
        case.operations.iter().copied().collect(),
        "{} operation scope",
        case.title
    );
    assert_eq!(
        plan.steps.len(),
        case.operations.len(),
        "{} unexpected duplicate/extra call",
        case.title
    );
}

fn assert_starting_inputs(case: &Case, plan: &Plan, task: &Task, catalog: &Catalog) {
    for (operation, expected) in [
        (
            "company.registry-profile",
            match case.title {
                "Volve Capital" => Some("86717162"),
                "Fleet manager" => Some("27095155"),
                "Check a company" | "Accountant" => Some("30204462"),
                _ => None,
            },
        ),
        (
            "company.financial-statements",
            match case.title {
                "Volve Capital" => Some("86717162"),
                "Check a company" => Some("30204462"),
                _ => None,
            },
        ),
    ] {
        if let Some(number) = expected {
            assert_eq!(
                inputs(plan, task, catalog, operation),
                json!({"kvkNummer":number})
            );
        }
    }
    for (title, domain) in [
        ("Check a company", "mollie.com"),
        ("Volve Capital", "volve.capital"),
        ("Research journalist", "example.org"),
    ] {
        if case.title == title {
            assert_eq!(
                inputs(plan, task, catalog, "domain.mail-dns"),
                json!({"domain":domain})
            );
            if title != "Volve Capital" {
                assert_eq!(
                    inputs(plan, task, catalog, "domain.registration"),
                    json!({"domain":domain})
                );
            }
        }
    }
    for (title, topic) in [
        ("Find opportunities", "IT services"),
        ("Cybersecurity", "cybersecurity"),
        ("Solar panels", "solar panels"),
        ("Cleaning", "cleaning services"),
    ] {
        if case.title == title {
            assert_eq!(
                inputs(plan, task, catalog, "procurement.nl-public-contracts"),
                json!({"search":match topic { "IT services" => "\"IT services\"", "solar panels" => "zonnepanelen", "cleaning services" => "schoonmaak", _ => topic },"page":0,"size":20,"sort":"publicatieDatum,desc"})
            );
            assert_eq!(
                inputs(plan, task, catalog, "web.linkup-answer"),
                json!({"q":format!("{topic} public tenders Netherlands official buyer procurement"),"depth":"standard","outputType":"sourcedAnswer"})
            );
        }
    }
    for (title, address, street, number, city) in [
        (
            "Check locations",
            "Damrak 1, Amsterdam",
            "Damrak",
            1,
            "Amsterdam",
        ),
        (
            "Coolsingel, Rotterdam",
            "Coolsingel 40, Rotterdam",
            "Coolsingel",
            40,
            "Rotterdam",
        ),
        (
            "Grote Markt, Groningen",
            "Grote Markt 1, Groningen",
            "Grote Markt",
            1,
            "Groningen",
        ),
    ] {
        if case.title == title {
            assert_eq!(
                inputs(plan, task, catalog, "government.publication.search"),
                json!({"q":address,"limit":10})
            );
            let values = inputs(plan, task, catalog, "location.property-search");
            assert_eq!(values["q"], "*:*");
            let filter = values["fq"].as_str().unwrap();
            for exact in [
                format!("straatnaam:\"{street}\""),
                format!("huisnummer:{number}"),
                format!("woonplaatsnaam:\"{city}\""),
                "-huisletter:".into(),
                "-huisnummertoevoeging:".into(),
            ] {
                assert!(filter.contains(&exact), "{filter} missing {exact}");
            }
        }
    }
    match case.title {
        "Research a market" => {
            assert_eq!(
                inputs(plan, task, catalog, "statistics.eurostat-gdp-netherlands"),
                json!({"time":"2024","format":"JSON","lang":"EN","dataset":"nama_10_gdp","geo":"NL","unit":"CP_MEUR","na_item":"B1GQ"})
            );
            for step in plan
                .steps
                .iter()
                .filter(|s| s.service.api_slug == "cbs-statline")
            {
                assert_eq!(
                    step.service
                        .inputs(task, step.entity_ref, &catalog.vocabulary)
                        .unwrap(),
                    step.service.fixed_params,
                    "reviewed CBS filters must remain exact"
                );
            }
        }
        "Accountant" => assert_eq!(
            inputs(plan, task, catalog, "vat.eu.validate"),
            json!({"cc":"NL","nr":"815839091B01"})
        ),
        "CFO / treasury" => {
            assert_eq!(
                inputs(plan, task, catalog, "iban.validate"),
                json!({"iban":"NL91ABNA0417164300"})
            );
            assert_eq!(
                inputs(plan, task, catalog, "fx.ecb-reference-rate"),
                json!({"quote":"USD"})
            );
        }
        "Fleet manager" => assert_eq!(
            inputs(plan, task, catalog, "vehicle.technical-profile"),
            json!({"kenteken":"GZ805N"})
        ),
        "Research journalist" => assert_eq!(
            inputs(plan, task, catalog, "web.source-fetch"),
            json!({"url":"https://example.org","mode":"standard"})
        ),
        _ => (),
    }
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            title: "Check a company",
            question: "Find the registered details and available annual financial figures for Mollie B.V. (KVK 30204462). Get public RDAP registration data and mail DNS records for mollie.com. Cite the sources. Combine the results into one cited answer, connecting relevant findings and clearly stating unknowns.",
            providers: &["kvk-dutch-business-register", "domain-intelligence"],
            operations: &[
                "company.registry-profile",
                "company.financial-statements",
                "domain.registration",
                "domain.mail-dns",
            ],
        },
        Case {
            title: "Find opportunities",
            question: "Find public tenders for IT services in the Netherlands. Search the web through Linkup for \"IT services public tenders Netherlands official buyer procurement\". Combine the results into one cited answer, connecting relevant findings and clearly stating unknowns.",
            providers: &["tenderned", "linkup"],
            operations: &["procurement.nl-public-contracts", "web.linkup-answer"],
        },
        Case {
            title: "Research a market",
            question: "Compare bankruptcies per sector in the Netherlands. Get GDP for the Netherlands in 2024 from Eurostat. Combine the results into one cited answer, connecting relevant findings and clearly stating unknowns.",
            providers: &["cbs-statline", "eurostat"],
            operations: &[
                "statistics.cbs-bankruptcy-sector-comparison",
                "statistics.cbs-bankruptcy-sectors",
                "statistics.cbs-bankruptcy-periods",
                "statistics.cbs-bankruptcy-measures",
                "statistics.cbs-bankruptcy-entity-types",
                "statistics.eurostat-gdp-netherlands",
            ],
        },
        Case {
            title: "Check locations",
            question: "Find building and neighbourhood information for Damrak 1, Amsterdam. Search official publications through KOOP for \"Damrak 1, Amsterdam\". Combine the results into one cited answer, connecting relevant findings and clearly stating unknowns.",
            providers: &["pdok", "koop"],
            operations: &[
                "location.property-search",
                "building.bag-unit",
                "building.bag-building",
                "location.bag-address-context",
                "area.cbs-neighbourhood-2025",
                "government.publication.search",
            ],
        },
        Case {
            title: "Volve Capital",
            question: "Find the registered details and available annual financial figures for Volve Capital B.V. (KVK 86717162). Get public mail DNS records for volve.capital. Combine the results into one cited answer, connecting relevant findings and clearly stating unknowns.",
            providers: &["kvk-dutch-business-register", "domain-intelligence"],
            operations: &[
                "company.registry-profile",
                "company.financial-statements",
                "domain.mail-dns",
            ],
        },
        Case {
            title: "Cybersecurity",
            question: "Find public tenders for cybersecurity in the Netherlands. Search the web through Linkup for \"cybersecurity public tenders Netherlands official buyer procurement\". Combine the results into one cited answer, connecting relevant findings and clearly stating unknowns.",
            providers: &["tenderned", "linkup"],
            operations: &["procurement.nl-public-contracts", "web.linkup-answer"],
        },
        Case {
            title: "Solar panels",
            question: "Find public tenders for solar panels in the Netherlands. Search the web through Linkup for \"solar panels public tenders Netherlands official buyer procurement\". Combine the results into one cited answer, connecting relevant findings and clearly stating unknowns.",
            providers: &["tenderned", "linkup"],
            operations: &["procurement.nl-public-contracts", "web.linkup-answer"],
        },
        Case {
            title: "Cleaning",
            question: "Find public tenders for cleaning services in the Netherlands. Search the web through Linkup for \"cleaning services public tenders Netherlands official buyer procurement\". Combine the results into one cited answer, connecting relevant findings and clearly stating unknowns.",
            providers: &["tenderned", "linkup"],
            operations: &["procurement.nl-public-contracts", "web.linkup-answer"],
        },
        Case {
            title: "Coolsingel, Rotterdam",
            question: "Find building and neighbourhood information for Coolsingel 40, Rotterdam. Search official publications through KOOP for \"Coolsingel 40, Rotterdam\". Combine the results into one cited answer, connecting relevant findings and clearly stating unknowns.",
            providers: &["pdok", "koop"],
            operations: &[
                "location.property-search",
                "building.bag-unit",
                "building.bag-building",
                "location.bag-address-context",
                "area.cbs-neighbourhood-2025",
                "government.publication.search",
            ],
        },
        Case {
            title: "Grote Markt, Groningen",
            question: "Find building and neighbourhood information for Grote Markt 1, Groningen. Search official publications through KOOP for \"Grote Markt 1, Groningen\". Combine the results into one cited answer, connecting relevant findings and clearly stating unknowns.",
            providers: &["pdok", "koop"],
            operations: &[
                "location.property-search",
                "building.bag-unit",
                "building.bag-building",
                "location.bag-address-context",
                "area.cbs-neighbourhood-2025",
                "government.publication.search",
            ],
        },
        Case {
            title: "Accountant",
            question: "Find the registered details for Mollie B.V. (KVK 30204462). Validate EU VAT number NL815839091B01 through VIES and show its validity and check date. Combine the results into one cited answer, connecting relevant findings and clearly stating unknowns.",
            providers: &["kvk-dutch-business-register", "vies"],
            operations: &["company.registry-profile", "vat.eu.validate"],
        },
        Case {
            title: "CFO / treasury",
            question: "Validate IBAN NL91ABNA0417164300 through IBAN Check. Get the latest official ECB reference exchange rate for EUR/USD, including the observation date, via Frankfurter. Combine the results into one cited answer, connecting relevant findings and clearly stating unknowns.",
            providers: &["iban-check", "frankfurter"],
            operations: &["iban.validate", "fx.ecb-reference-rate"],
        },
        Case {
            title: "Fleet manager",
            question: "Get RDW vehicle data for registration plate GZ805N, including make, trade name, inspection expiry date and the outstanding recall indicator. Find the registered details for Louwman Dealerbedrijven B.V. (KVK 27095155). Combine the results into one cited answer, connecting relevant findings and clearly stating unknowns.",
            providers: &["rdw", "kvk-dutch-business-register"],
            operations: &["vehicle.technical-profile", "company.registry-profile"],
        },
        Case {
            title: "Research journalist",
            question: "Get public RDAP registration data and mail DNS records for example.org. Cite the sources. Retrieve the web page https://example.org through Linkup. Combine the results into one cited answer, connecting relevant findings and clearly stating unknowns.",
            providers: &["domain-intelligence", "linkup"],
            operations: &["domain.registration", "domain.mail-dns", "web.source-fetch"],
        },
    ]
}

macro_rules! example_test {
    ($name:ident, $index:expr) => {
        #[tokio::test]
        async fn $name() {
            let catalog = catalog();
            let cases = cases();
            let case = &cases[$index];
            let (task, _, plan) = plan(case.question, &catalog).await;
            assert_scope(case, &plan);
            assert_starting_inputs(case, &plan, &task, &catalog);
        }
    };
}
example_test!(compound_example_company, 0);
example_test!(compound_example_tenders, 1);
example_test!(compound_example_market, 2);
example_test!(compound_example_location, 3);
example_test!(compound_example_volve, 4);
example_test!(compound_example_cybersecurity, 5);
example_test!(compound_example_solar, 6);
example_test!(compound_example_cleaning, 7);
example_test!(compound_example_rotterdam, 8);
example_test!(compound_example_groningen, 9);
example_test!(compound_example_accountant, 10);
example_test!(compound_example_treasury, 11);
example_test!(compound_example_fleet, 12);
example_test!(compound_example_journalist, 13);

/// The simplified App cards are actual submitted questions, without an
/// expanded hidden prompt or the former synthesis-instruction suffix.
#[tokio::test]
async fn compound_examples_six_short_published_questions_keep_sources_inputs_and_synthesis() {
    let catalog = catalog();
    let historical = cases();
    let simple = [
        Case {
            title: "A company and its website",
            question: "Find the registered details for Mollie B.V. (KVK 30204462). Retrieve the web page https://mollie.com through Linkup.",
            providers: &["kvk-dutch-business-register", "linkup"],
            operations: &["company.registry-profile", "web.source-fetch"],
        },
        Case {
            title: "Find cleaning contracts",
            question: "Find public tenders for cleaning services in the Netherlands. Search the web through Linkup for \"cleaning tenders Netherlands\".",
            providers: historical[7].providers,
            operations: historical[7].operations,
        },
        Case {
            title: "Learn about an address",
            question: "Find building and neighbourhood information for Damrak 1, Amsterdam. Search official publications through KOOP for \"Damrak 1, Amsterdam\".",
            providers: historical[3].providers,
            operations: historical[3].operations,
        },
        Case {
            title: "Check a VAT number",
            question: "Find the registered details for Mollie B.V. (KVK 30204462). Validate EU VAT number NL815839091B01 through VIES and show its validity and check date.",
            providers: historical[10].providers,
            operations: historical[10].operations,
        },
        Case {
            title: "Find solar panel contracts",
            question: "Find public tenders for solar panels in the Netherlands. Search the web through Linkup for \"solar panel tenders Netherlands\".",
            providers: historical[6].providers,
            operations: historical[6].operations,
        },
        Case {
            title: "Explore the Dutch economy",
            question: "Compare bankruptcies per sector in the Netherlands. Get GDP for the Netherlands in 2024 from Eurostat.",
            providers: historical[2].providers,
            operations: historical[2].operations,
        },
    ];
    for (index, case) in simple.iter().enumerate() {
        assert!(!case.question.contains(crate::compound_request::COMBINE));
        let (task, _, plan) = plan(case.question, &catalog).await;
        assert_scope(case, &plan);
        let requested = &task.capability_intake.as_ref().unwrap().parsed_request;
        for operation in ["summarize", "analyze"] {
            assert!(requested.deliverable.operations.iter().any(|value| value == operation));
        }
        if matches!(index, 0 | 3) {
            assert_eq!(inputs(&plan, &task, &catalog, "company.registry-profile"), json!({"kvkNummer":"30204462"}));
            assert!(requested.subjects.iter().any(|subject| subject.label == "Mollie B.V."));
        }
        match index {
            0 => assert_eq!(
                inputs(&plan, &task, &catalog, "web.source-fetch"),
                json!({"url":"https://mollie.com","mode":"standard"}),
            ),
            1 | 4 => {
                let (search, query) = if index == 1 {
                    ("schoonmaak", "cleaning tenders Netherlands")
                } else {
                    ("zonnepanelen", "solar panel tenders Netherlands")
                };
                assert_eq!(
                    inputs(&plan, &task, &catalog, "procurement.nl-public-contracts"),
                    json!({"search":search,"page":0,"size":20,"sort":"publicatieDatum,desc"}),
                );
                assert_eq!(
                    inputs(&plan, &task, &catalog, "web.linkup-answer"),
                    json!({"q":query,"depth":"standard","outputType":"sourcedAnswer"}),
                );
            }
            2 => {
                assert_address_input(&inputs(&plan, &task, &catalog, "location.property-search"), "Damrak", 1, "Amsterdam");
                assert_eq!(
                    inputs(&plan, &task, &catalog, "government.publication.search"),
                    json!({"q":"Damrak 1, Amsterdam","limit":10}),
                );
            }
            3 => assert_eq!(
                inputs(&plan, &task, &catalog, "vat.eu.validate"),
                json!({"cc":"NL","nr":"815839091B01"}),
            ),
            5 => assert_starting_inputs(&historical[2], &plan, &task, &catalog),
            _ => unreachable!(),
        }
    }
}

#[tokio::test]
async fn compound_examples_changed_values_reach_their_own_sources_without_original_identity() {
    let catalog = catalog();
    let cases = cases();
    for (index, replacements, expected, forbidden) in [
        (
            0,
            vec![
                ("Mollie B.V.", "Another N.V."),
                ("30204462", "00123456"),
                ("mollie.com", "other.nl"),
            ],
            vec![
                ("company.registry-profile", "/kvkNummer", "00123456"),
                ("company.financial-statements", "/kvkNummer", "00123456"),
                ("domain.registration", "/domain", "other.nl"),
                ("domain.mail-dns", "/domain", "other.nl"),
            ],
            vec!["Mollie", "30204462", "mollie.com"],
        ),
        (
            3,
            vec![("Damrak 1, Amsterdam", "Coolsingel 41, Rotterdam")],
            vec![(
                "government.publication.search",
                "/q",
                "Coolsingel 41, Rotterdam",
            )],
            vec!["Damrak", "Amsterdam"],
        ),
        (
            10,
            vec![
                ("Mollie B.V.", "Another B.V."),
                ("30204462", "00123456"),
                ("NL815839091B01", "BE0403170701"),
            ],
            vec![
                ("company.registry-profile", "/kvkNummer", "00123456"),
                ("vat.eu.validate", "/cc", "BE"),
                ("vat.eu.validate", "/nr", "0403170701"),
            ],
            vec!["Mollie", "30204462", "815839091B01"],
        ),
        (
            11,
            vec![
                ("NL91ABNA0417164300", "DE89370400440532013000"),
                ("EUR/USD", "EUR/GBP"),
            ],
            vec![
                ("iban.validate", "/iban", "DE89370400440532013000"),
                ("fx.ecb-reference-rate", "/quote", "GBP"),
            ],
            vec!["NL91ABNA0417164300", "USD"],
        ),
        (
            12,
            vec![
                ("GZ805N", "RZ-874-R"),
                ("Louwman Dealerbedrijven B.V.", "Another B.V."),
                ("27095155", "00123456"),
            ],
            vec![
                ("vehicle.technical-profile", "/kenteken", "RZ-874-R"),
                ("company.registry-profile", "/kvkNummer", "00123456"),
            ],
            vec!["GZ805N", "Louwman", "27095155"],
        ),
        (
            13,
            vec![("example.org", "example.com")],
            vec![
                ("domain.registration", "/domain", "example.com"),
                ("domain.mail-dns", "/domain", "example.com"),
                ("web.source-fetch", "/url", "https://example.com"),
            ],
            vec!["example.org"],
        ),
    ] {
        let mut question = cases[index].question.to_owned();
        for (from, to) in replacements {
            question = question.replace(from, to);
        }
        let (task, intent, plan) = plan(&question, &catalog).await;
        assert_scope(&cases[index], &plan);
        for (operation, pointer, value) in expected {
            assert_eq!(
                inputs(&plan, &task, &catalog, operation).pointer(pointer),
                Some(&json!(value)),
                "changed {operation} input"
            );
        }
        if index == 3 {
            let values = inputs(&plan, &task, &catalog, "location.property-search");
            let filter = values["fq"].as_str().unwrap();
            for part in [
                "straatnaam:\"Coolsingel\"",
                "huisnummer:41",
                "woonplaatsnaam:\"Rotterdam\"",
            ] {
                assert!(filter.contains(part), "{filter}");
            }
        }
        let bound =
            serde_json::to_string(&json!({"subjects":intent.subjects,"facts":task.facts})).unwrap();
        for old in forbidden {
            assert!(
                !bound.contains(old),
                "original identity leaked after edit: {old}"
            );
        }
    }
}

#[tokio::test]
async fn compound_examples_unknown_extra_scope_never_becomes_a_partial_quote() {
    let catalog = catalog();
    for case in cases() {
        for suffix in [
            " Also retrieve private owner identities.",
            " Return the results as a PDF.",
            " Include data from 2020 only.",
        ] {
            let question = format!("{}{suffix}", case.question);
            assert!(
                crate::compound_request::parse(&question, "fixture").is_none(),
                "{} silently dropped additional scope",
                case.title
            );
            let parser = parser();
            // Unknown scope needs ordinary interpretation. With no model
            // configured, the actual Ask intake fails before it can quote.
            assert!(
                parser
                    .ask_intake(&question, &Task::default(), &catalog, None)
                    .await
                    .is_err(),
                "{} quoted only the supported prefix",
                case.title
            );
            assert_eq!(parser.calls.load(Ordering::Relaxed), 0);
        }
    }
}

fn assert_address_input(values: &Value, street: &str, number: u32, city: &str) {
    assert_eq!(values["q"], "*:*");
    let filter = values["fq"].as_str().unwrap();
    for exact in [
        format!("straatnaam:\"{street}\""),
        format!("huisnummer:{number}"),
        format!("woonplaatsnaam:\"{city}\""),
        "-huisletter:".into(),
        "-huisnummertoevoeging:".into(),
    ] {
        assert!(filter.contains(&exact), "{filter} missing {exact}");
    }
}

#[tokio::test]
async fn compound_dynamic_clause_reorder_preserves_scope_and_source_bindings() {
    let catalog = catalog();
    let cases = cases();
    // Cover abbreviations, the two-sentence DNS request, dependent location
    // lookups, and independent numeric subjects; neither source gets priority.
    for index in [0, 3, 11, 13] {
        let case = &cases[index];
        let request = crate::compound_request::recognize(case.question, "fixture").unwrap();
        assert_eq!(request.clauses.len(), 2);
        let reordered = format!(
            "{} {} {}",
            request.clauses[1].question,
            request.clauses[0].question,
            crate::compound_request::COMBINE
        );
        let (task, _, plan) = plan(&reordered, &catalog).await;
        assert_scope(case, &plan);
        assert_starting_inputs(case, &plan, &task, &catalog);
    }
}

#[tokio::test]
async fn compound_dynamic_unpublished_topic_reaches_both_sources() {
    let catalog = catalog();
    let cases = cases();
    let question = cases[1]
        .question
        .replace("IT services", "cloud migration services");
    let (task, intent, plan) = plan(&question, &catalog).await;
    assert_scope(&cases[1], &plan);
    assert_eq!(
        inputs(&plan, &task, &catalog, "procurement.nl-public-contracts"),
        json!({"search":"\"cloud migration services\"","page":0,"size":20,"sort":"publicatieDatum,desc"})
    );
    assert_eq!(
        inputs(&plan, &task, &catalog, "web.linkup-answer"),
        json!({"q":"cloud migration services public tenders Netherlands official buyer procurement","depth":"standard","outputType":"sourcedAnswer"})
    );
    let bound = json!({"subjects":intent.subjects,"facts":task.facts}).to_string();
    assert!(!bound.contains("IT services"));
}

#[tokio::test]
async fn compound_dynamic_unpublished_address_reaches_location_and_notice_search() {
    let catalog = catalog();
    let cases = cases();
    let question = cases[3]
        .question
        .replace("Damrak 1, Amsterdam", "Kerkstraat 17, Utrecht");
    let (task, intent, plan) = plan(&question, &catalog).await;
    assert_scope(&cases[3], &plan);
    assert_address_input(
        &inputs(&plan, &task, &catalog, "location.property-search"),
        "Kerkstraat",
        17,
        "Utrecht",
    );
    assert_eq!(
        inputs(&plan, &task, &catalog, "government.publication.search"),
        json!({"q":"Kerkstraat 17, Utrecht","limit":10})
    );
    let bound = json!({"subjects":intent.subjects,"facts":task.facts}).to_string();
    for original in ["Damrak", "Amsterdam"] {
        assert!(!bound.contains(original), "old address leaked: {original}");
    }
}

#[tokio::test]
async fn compound_dynamic_gdp_year_and_country_keep_the_other_clause_scope() {
    let catalog = catalog();
    let cases = cases();
    let case = &cases[2];
    for (country, year, operation, geo) in [
        (
            "the Netherlands",
            "2022",
            "statistics.eurostat-gdp-netherlands",
            "NL",
        ),
        ("Germany", "2023", "statistics.eurostat-gdp-germany", "DE"),
    ] {
        // Only GDP is changed: the bankruptcy comparison remains Dutch.
        let question = case.question.replace(
            "Get GDP for the Netherlands in 2024",
            &format!("Get GDP for {country} in {year}"),
        );
        let (task, _, plan) = plan(&question, &catalog).await;
        let expected_operations: BTreeSet<_> = case
            .operations
            .iter()
            .map(|&value| {
                if value == "statistics.eurostat-gdp-netherlands" {
                    operation
                } else {
                    value
                }
            })
            .collect();
        assert_eq!(
            plan.steps
                .iter()
                .map(|step| step.service.contract.capability_slug.as_str())
                .collect::<BTreeSet<_>>(),
            expected_operations
        );
        assert_eq!(plan.steps.len(), case.operations.len());
        assert_eq!(
            plan.steps
                .iter()
                .map(|step| step.service.api_slug.as_str())
                .collect::<BTreeSet<_>>(),
            case.providers.iter().copied().collect()
        );
        assert_eq!(
            inputs(&plan, &task, &catalog, operation),
            json!({"time":year,"format":"JSON","lang":"EN","dataset":"nama_10_gdp","geo":geo,"unit":"CP_MEUR","na_item":"B1GQ"})
        );
        for step in plan
            .steps
            .iter()
            .filter(|step| step.service.api_slug == "cbs-statline")
        {
            assert_eq!(
                step.service
                    .inputs(&task, step.entity_ref, &catalog.vocabulary)
                    .unwrap(),
                step.service.fixed_params,
                "changing Eurostat country/year must not alter CBS filters"
            );
        }
    }
}

#[tokio::test]
async fn compound_dynamic_editing_one_duplicated_address_does_not_change_the_other() {
    let catalog = catalog();
    let cases = cases();
    let question = cases[3]
        .question
        .replacen("Damrak 1, Amsterdam", "Kerkstraat 17, Utrecht", 1);
    let (task, _, plan) = plan(&question, &catalog).await;
    assert_scope(&cases[3], &plan);
    assert_address_input(
        &inputs(&plan, &task, &catalog, "location.property-search"),
        "Kerkstraat",
        17,
        "Utrecht",
    );
    assert_eq!(
        inputs(&plan, &task, &catalog, "government.publication.search"),
        json!({"q":"Damrak 1, Amsterdam","limit":10}),
        "the separate publication query must keep its explicitly supplied address"
    );
    let subject_for = |operation| {
        plan.steps
            .iter()
            .find(|step| step.service.contract.capability_slug == operation)
            .unwrap()
            .entity_ref
    };
    assert_ne!(
        subject_for("location.property-search"),
        subject_for("government.publication.search"),
        "different explicit address subjects must not merge"
    );
}
