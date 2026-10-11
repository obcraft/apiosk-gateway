//! Offline source-contract and saved-answer regressions. Explicit intake and
//! planning fixtures do not claim to verify AI interpretation. No provider
//! request or paid replay is made.
#[allow(dead_code)]
#[path = "../legacy-executor/src/proxy/public_sources.rs"]
mod source_adapter;
use crate::{
    catalog::{Catalog, Snapshot},
    parser::Intent,
    resolver::{Plan, Resolution},
    state::Task,
};
use serde_json::{json, Value};
use uuid::Uuid;

fn catalog(text: &str) -> Catalog {
    Catalog::build(serde_json::from_str::<Snapshot>(text).unwrap())
}
async fn planned(question: &str, catalog: &Catalog, steps: usize) -> (Intent, Task, Plan) {
    let intake = crate::capability_intake::offline_fixture_intake(question, catalog);
    let reply = crate::parser::plan_coverage::offline_fixture_reply(
        &intake, catalog, question, &Task::default(),
    ).expect("the declared contract fixture must be complete");
    assert!(reply.discover, "{question}: {}", reply.message);
    let mut intent = reply.intent.unwrap();
    let mut task = Task::default();
    task.capability_intake = Some(intake);
    crate::engine::bind_subjects(&mut task, catalog, &mut intent).unwrap();
    let Resolution::Plan(plan) = crate::resolver::resolve(catalog, &task, &intent) else {
        panic!("{question}: no complete plan")
    };
    assert_eq!(plan.steps.len(), steps, "{question}");
    assert!(
        plan.cap_atomic > 0,
        "the ordinary quote and approval boundary is retained"
    );
    (intent, task, plan)
}
fn saved(step: &crate::resolver::Step, label: &str, data: Value) -> Value {
    json!({"result_ref":Uuid::new_v4(),"document":{"subject":{"entity_ref":step.entity_ref,"label":label},
        "source":{"provider":step.service.api_slug,"path":step.service.path,"endpoint_id":step.service.endpoint_id,"name":step.service.name,"contract_version":step.service.contract.version},
        "coverage":"source_response","missing_fields":[],"data":data}})
}
fn check_answer(question: &str, intent: &Intent, sources: &[Value]) -> Value {
    let before = sources.to_vec();
    let answer = crate::direct_answer::reviewed_compose(question, intent, sources)
        .unwrap()
        .expect("reviewed projection must handle the complete example");
    assert_eq!(answer["status"], "completed");
    assert!(answer["follow_up_question"].is_null());
    for row in answer["presentation"]["rows"].as_array().unwrap() {
        for cell in row.as_array().unwrap() {
            if let (Some(reference), Some(pointer)) =
                (cell["result_ref"].as_str(), cell["pointer"].as_str())
            {
                let source = sources
                    .iter()
                    .find(|s| s["result_ref"] == reference)
                    .unwrap();
                assert_eq!(
                    source["document"].pointer(pointer),
                    Some(&cell["value"]),
                    "every displayed source cell must equal its saved value"
                );
            }
        }
    }
    assert_eq!(
        sources, before,
        "answer projection cannot alter or replay the source"
    );
    answer
}
fn field<'a>(answer: &'a Value, label: &str) -> &'a Value {
    &answer["presentation"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row[0]["value"] == label)
        .unwrap()[1]["value"]
}
fn export(slug: &str, question: &str, answer: &Value, sources: &[Value]) {
    if let Ok(path) = std::env::var("ADDITIONAL_EXAMPLES_EXPORT_DIR") {
        let root = std::path::Path::new(&path);
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(
            root.join(format!("{slug}.json")),
            serde_json::to_vec_pretty(
                &json!({"question":question,"analysis":answer,"results":sources}),
            )
            .unwrap(),
        )
        .unwrap();
    }
}

#[tokio::test]
async fn extra_company_volve_uses_actual_saved_registry_and_2025_figures() {
    let question="Find the registered details and available annual financial figures for Volve Capital B.V. (KVK 86717162).";
    let fixture: Value = serde_json::from_str(include_str!(
        "../tests/fixtures/additional-examples/volve-company.json"
    ))
    .unwrap();
    let catalog = catalog(include_str!("../tests/fixtures/workflows/catalog.json"));
    let (intent, task, plan) = planned(question, &catalog, 2).await;
    let mut sources = Vec::new();
    for step in &plan.steps {
        assert_eq!(
            step.service
                .inputs(&task, step.entity_ref, &catalog.vocabulary)
                .unwrap()["kvkNummer"],
            "86717162"
        );
        let data = match step.service.contract.capability_slug.as_str() {
            "company.registry-profile" => fixture["profile"]["public_profile"].clone(),
            "company.financial-statements" => fixture["financial"]["payload"].clone(),
            other => panic!("unexpected company purchase: {other}"),
        };
        let extraction = step
            .service
            .contract
            .extract(&data, &catalog.vocabulary)
            .unwrap();
        assert!(extraction.rejected.is_empty());
        assert!(extraction.single().is_some());
        let mut source = saved(step, "Volve Capital B.V.", data);
        source["document"]["report_subject"] =
            json!({"name":"Volve Capital B.V.","identifiers":["86717162"]});
        sources.push(source);
    }
    let answer = check_answer(question, &intent, &sources);
    assert_eq!(field(&answer, "Registered name"), "Volve Capital B.V.");
    assert_eq!(field(&answer, "Financial year"), "2025");
    assert_eq!(field(&answer, "Balance sheet: Total assets"), "682987");
    assert_eq!(field(&answer, "Balance sheet: Equity"), "-373267");
    assert!(
        !answer.to_string().contains("EUR"),
        "currency is absent from the actual response"
    );
    assert!(answer["limitations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x.as_str().unwrap().contains("not a verified latest filing")));
    export("volve-company", question, &answer, &sources);
}

#[tokio::test]
async fn extra_locations_preserve_actual_five_step_address_building_neighbourhood_chains() {
    let catalog = crate::ask_v2::test_catalog::locations();
    for (slug, question, fixture, year, area, residents) in [
        (
            "rotterdam-coolsingel",
            "Find building and neighbourhood information for Coolsingel 40, Rotterdam.",
            include_str!("../tests/fixtures/additional-examples/rotterdam-coolsingel.json"),
            1920,
            29319,
            18365,
        ),
        (
            "groningen-grote-markt",
            "Find building and neighbourhood information for Grote Markt 1, Groningen.",
            include_str!("../tests/fixtures/additional-examples/groningen-grote-markt.json"),
            1813,
            3818,
            4765,
        ),
    ] {
        let raw: Value = serde_json::from_str(fixture).unwrap();
        let (intent, task, plan) = planned(question, &catalog, 5).await;
        let vbo = raw["address"]["response"]["docs"][0]["adresseerbaarobject_id"]
            .as_str()
            .unwrap();
        let code = raw["address"]["response"]["docs"][0]["buurtcode"]
            .as_str()
            .unwrap();
        let normalize = |route: &str, key: &str, value: &str| {
            let mut url = url::Url::parse(&format!("https://api.pdok.nl/apiosk/{route}")).unwrap();
            url.query_pairs_mut().append_pair(key, value);
            let prepared = source_adapter::prepare("pdok", "GET", url.as_str())
                .unwrap()
                .unwrap();
            source_adapter::normalize(&prepared, &raw[route]).unwrap()
        };
        let unit = normalize("bag-unit", "vbo", vbo);
        let building = normalize(
            "building",
            "href",
            unit["matched"]["single_building_href"].as_str().unwrap(),
        );
        let address = normalize("bag-address", "vbo", vbo);
        let neighbourhood = normalize("neighbourhood", "code", code);
        let mut sources = Vec::new();
        for step in &plan.steps {
            let data = match step.service.contract.capability_slug.as_str() {
                "location.property-search" => {
                    let inputs = step
                        .service
                        .inputs(&task, step.entity_ref, &catalog.vocabulary)
                        .unwrap();
                    assert_eq!(inputs["q"], "*:*");
                    assert_eq!(inputs["rows"], 5);
                    let observed =
                        url::Url::parse(raw["source_checks"][0]["url"].as_str().unwrap()).unwrap();
                    for (key, value) in observed.query_pairs() {
                        assert_eq!(
                            inputs[key.as_ref()]
                                .as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| inputs[key.as_ref()].to_string()),
                            value
                        );
                    }
                    assert_eq!(raw["address"]["response"]["numFound"], 1);
                    raw["address"].clone()
                }
                "building.bag-unit" => unit.clone(),
                "building.bag-building" => building.clone(),
                "location.bag-address-context" => address.clone(),
                "area.cbs-neighbourhood-2025" => neighbourhood.clone(),
                other => panic!("unexpected location purchase: {other}"),
            };
            let extraction = step
                .service
                .contract
                .extract(&data, &catalog.vocabulary)
                .unwrap();
            assert!(
                extraction
                    .rejected
                    .iter()
                    .all(|rejection| step.service.optional_outputs.contains(&rejection.fact_type)),
                "only explicitly optional omitted source fields may be rejected: {} {:?}",
                step.service.contract.capability_slug,
                extraction.rejected
            );
            for output in step
                .service
                .contract
                .output_map
                .produces
                .keys()
                .filter(|kind| !step.service.optional_outputs.contains(kind))
            {
                assert!(
                    extraction
                        .subjects
                        .iter()
                        .any(|subject| subject.facts.iter().any(|fact| fact.fact_type == *output)),
                    "missing required {output}"
                );
            }
            assert!(!extraction.subjects.is_empty());
            sources.push(saved(step, &intent.subjects[0].label, data));
        }
        let answer = check_answer(question, &intent, &sources);
        assert_eq!(field(&answer, "Registered construction year"), year);
        assert_eq!(field(&answer, "Unit area (m²)"), area);
        assert_eq!(field(&answer, "Residents in neighbourhood"), residents);
        assert_eq!(field(&answer, "CBS reporting year"), 2025);
        export(slug, question, &answer, &sources);
    }
}

#[tokio::test]
async fn extra_tenders_preserve_literal_topics_and_all_twenty_actual_notices() {
    let catalog = catalog(include_str!("../tests/fixtures/tenderned-browse.json"));
    for (slug, question, topic, fixture, total) in [
        (
            "tender-cybersecurity",
            "Find public tenders for cybersecurity in the Netherlands.",
            "cybersecurity",
            include_str!("../tests/fixtures/additional-examples/tender-cybersecurity.json"),
            182,
        ),
        (
            "tender-zonnepanelen",
            "Zoek openbare aanbestedingen voor zonnepanelen in Nederland.",
            "zonnepanelen",
            include_str!("../tests/fixtures/additional-examples/tender-zonnepanelen.json"),
            483,
        ),
        (
            "tender-schoonmaak",
            "Zoek openbare aanbestedingen voor schoonmaak in Nederland.",
            "schoonmaak",
            include_str!("../tests/fixtures/additional-examples/tender-schoonmaak.json"),
            3812,
        ),
    ] {
        let fixture: Value = serde_json::from_str(fixture).unwrap();
        let (intent, task, plan) = planned(question, &catalog, 1).await;
        let step = &plan.steps[0];
        let inputs = step
            .service
            .inputs(&task, step.entity_ref, &catalog.vocabulary)
            .unwrap();
        assert_eq!(
            inputs,
            json!({"search":topic,"page":0,"size":20,"sort":"publicatieDatum,desc"})
        );
        let original = url::Url::parse(fixture["url"].as_str().unwrap()).unwrap();
        assert_eq!(
            original
                .query_pairs()
                .find(|(k, _)| k == "search")
                .unwrap()
                .1,
            topic
        );
        let mut source = saved(step, topic, fixture["body"].clone());
        crate::tenderned::record_request(&step.service, &inputs, &mut source["document"]);
        let answer = check_answer(question, &intent, &[source.clone()]);
        assert_eq!(source["document"]["data"]["totalElements"], total);
        assert_eq!(answer["presentation"]["rows"].as_array().unwrap().len(), 20);
        assert_eq!(answer["presentation"]["returned_count"], 20);
        assert_eq!(
            answer["presentation"]["columns"].as_array().unwrap().len(),
            6
        );
        assert!(answer["limitations"].as_array().unwrap().len() >= 3);
        export(slug, question, &answer, &[source]);
    }
}
