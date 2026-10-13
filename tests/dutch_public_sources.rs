// The shared executor adapter also exposes operations outside this fixture.
#[allow(dead_code)]
#[path = "../legacy-executor/src/proxy/public_sources.rs"]
mod adapter;
use serde_json::{json, Value};

fn raw(name: &str) -> Value {
    serde_json::from_slice(
        &std::fs::read(format!("tests/fixtures/dutch-public-sources/{name}.json")).unwrap(),
    )
    .unwrap()
}
fn request(source: &str, query: &str) -> adapter::Prepared {
    let base = match source {
        "koop" => "https://repository.overheid.nl/sru",
        "rdw" => "https://opendata.rdw.nl/resource/m9d7-ebf2.json",
        _ => "https://www.rvo.nl/api/v1/opendata/subsidies",
    };
    adapter::prepare(source, "GET", &format!("{base}?{query}"))
        .unwrap()
        .unwrap()
}

#[test]
fn real_source_samples_preserve_identity_rows_and_coverage() {
    let cases = [
        (
            "koop",
            "q=omgevingsvergunning&authority=Amsterdam&limit=2",
            "koop-many",
        ),
        ("rdw", "kenteken=rz-874-r", "rdw"),
        ("rvo", "q=energie", "rvo"),
    ];
    let mut samples = serde_json::Map::new();
    for (source, query, fixture) in cases {
        let p = request(source, query);
        let result = adapter::normalize(&p, &raw(fixture)).unwrap();
        assert!(!result["items"].as_array().unwrap().is_empty(), "{source}");
        if source == "koop" {
            assert_eq!(result["items"].as_array().unwrap().len(), 2);
            assert_eq!(result["next_start"], 3);
            assert_eq!(result["items"][0]["id"], "gmb-2026-427235");
            assert_eq!(result["items"][0]["authority"], "Amsterdam");
        } else if source == "rdw" {
            assert_eq!(result["requested_plate"], "RZ874R");
            assert_eq!(result["items"][0]["kenteken"], "RZ874R");
            assert!(p.url.contains("kenteken=RZ874R"));
        } else {
            assert_eq!(result["coverage"]["complete"], false);
            assert!(result["total"].is_null());
            assert_eq!(result["scanned"], 50);
            assert_eq!(result["items"][0]["application_status"], "unknown");
            assert_eq!(
                p.url, "https://www.rvo.nl/api/v1/opendata/subsidies?page=0",
                "Only the verified first page is fetched; keywords filter locally"
            );
        }
        samples.insert(source.into(), result);
    }
    samples.insert(
        "koop-detail".into(),
        adapter::normalize(
            &request("koop", "publication_id=gmb-2026-427235"),
            &raw("koop-detail"),
        )
        .unwrap(),
    );
    if std::env::var("PUBLIC_SOURCE_EXPORT").as_deref() == Ok("1") {
        std::fs::write(
            "tests/fixtures/dutch-public-sources/normalized.json",
            serde_json::to_vec_pretty(&samples).unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn empty_pages_are_valid_and_rvo_no_match_is_not_a_complete_search() {
    let empty = adapter::normalize(
        &request("koop", "q=apioskzeroresultxzy92745"),
        &raw("koop-empty"),
    )
    .unwrap();
    assert_eq!(empty["total"], 0);
    assert_eq!(empty["items"], json!([]));
    assert!(empty["next_start"].is_null());
    let rdw = adapter::normalize(&request("rdw", "kenteken=000000"), &json!([])).unwrap();
    assert_eq!(rdw["total"], 0);
    let rvo = adapter::normalize(&request("rvo", "q=doesnotexistzz91"), &raw("rvo")).unwrap();
    assert_eq!(rvo["matched_in_page"], 0);
    assert_eq!(rvo["coverage"]["complete"], false);
    assert!(rvo["total"].is_null());
}

#[test]
fn wrong_identity_diagnostics_and_shape_changes_fail_before_settlement() {
    assert!(adapter::normalize(&request("rdw", "kenteken=ABC123"), &raw("rdw")).is_err());
    assert!(adapter::normalize(&request("koop","q=water"),&json!({"searchRetrieveResponse":{"diagnostics":{"diagnostic":{"message":"Unsupported index"}}}})).is_err());
    assert!(adapter::normalize(
        &request("koop", "q=water"),
        &json!({"searchRetrieveResponse":{"numberOfRecords":1}})
    )
    .is_err());
    assert!(
        adapter::normalize(&request("rvo", "q=water"), &json!({"error":"unavailable"})).is_err()
    );
    assert!(adapter::normalize(
        &request("rvo", "q=water"),
        &json!([{"id":"x","title":"water","url":"https://example.com"}])
    )
    .is_err());
}

#[test]
fn query_inputs_are_bounded_and_literal() {
    assert!(adapter::prepare("other", "GET", "https://example.com")
        .unwrap()
        .is_none());
    for query in [
        "q=x&limit=1000",
        "q=x&start=0",
        "q=x&q=y",
        "query=c.product-area=all",
        "q=x&from=2026-02-30",
        "q=x&from=2026-09-11&until=2026-09-01",
    ] {
        assert!(
            adapter::prepare(
                "koop",
                "GET",
                &format!("https://repository.overheid.nl/sru?{query}")
            )
            .is_err(),
            "{query}"
        );
    }
    assert!(adapter::prepare("koop", "POST", "https://repository.overheid.nl/sru?q=x").is_err());
    assert!(adapter::prepare(
        "rvo",
        "GET",
        "https://www.rvo.nl/api/v1/opendata/subsidies?q=x&page=1"
    )
    .is_err());
    let p = request("koop", "q=water%22%20OR%20dt.title%3Dx&authority=Amsterdam");
    let query = url::Url::parse(&p.url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "query")
        .unwrap()
        .1
        .into_owned();
    assert!(query.contains("dt.title=\"water\\\" OR dt.title=x\""));
}

#[test]
fn source_dates_and_single_record_detail_use_verified_wire_shapes() {
    let detail = adapter::normalize(
        &request("koop", "publication_id=gmb-2026-427235"),
        &raw("koop-detail"),
    )
    .unwrap();
    assert_eq!(detail["items"].as_array().unwrap().len(), 1);
    assert_eq!(detail["items"][0]["id"], "gmb-2026-427235");
    let p = request(
        "koop",
        "q=omgevingsvergunning&authority=Amsterdam&from=2026-09-01&until=2026-09-11&limit=2",
    );
    let page = adapter::normalize(&p, &raw("koop-dates")).unwrap();
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    for row in page["items"].as_array().unwrap() {
        let date = row["published"].as_str().unwrap();
        assert!(("2026-09-01"..="2026-09-11").contains(&date));
    }
}

#[test]
fn catalog_admits_every_contract_and_quotes_the_existing_metadata_tier() {
    use apiosk_gateway_v2::{
        catalog::{Catalog, Snapshot},
        parser::{Goal, Intent, Subject},
        resolver::{resolve, Resolution},
        state::{Fact, Task},
    };
    use chrono::Utc;
    use std::collections::BTreeMap;
    use uuid::Uuid;
    let snapshot: Snapshot = serde_json::from_value(raw("catalog")).unwrap();
    let catalog = Catalog::build(snapshot);
    assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
    assert_eq!(catalog.services.len(), 4);
    for service in &catalog.services {
        let extracted = service
            .contract
            .extract(
                service.sample_payload.as_ref().unwrap(),
                &catalog.vocabulary,
            )
            .unwrap();
        assert!(extracted.rejected.is_empty());
        assert_eq!(extracted.subjects.len(), 1);
        assert_eq!(service.total(), 54348);
        let e = Uuid::new_v4();
        let mut task = Task::default();
        for required in service.contract.requires.iter().filter(|r| !r.optional) {
            let value = match required.fact_type.as_str() {
                "vehicle.registration.nl.plate" => json!("rz-874-r"),
                "government.publication.identifier" => json!("gmb-2026-427235"),
                "subsidy.discovery.keyword" => json!("energie"),
                _ => json!("omgevingsvergunning"),
            };
            task.facts.push(Fact {
                entity_ref: e,
                semantic_type: required.fact_type.clone(),
                value,
                provenance: json!({"kind":"user_input"}),
                observed_at: Utc::now(),
                expires_at: Utc::now() + chrono::Duration::hours(1),
                verification: "user_asserted".into(),
            });
        }
        let intent = Intent {
            subjects: vec![Subject {
                entity_ref: Some(e),
                label: service.name.clone(),
                values: BTreeMap::new(),
            }],
            goals: vec![Goal {
                capability: service.contract.capability_slug.clone(),
                subject: 0,
                constraints: BTreeMap::from([("source".into(), json!(service.api_slug))]),
            }],
            answer_operations: vec![],
            unresolved_references: vec![],
        };
        let Resolution::Plan(plan) = resolve(&catalog, &task, &intent) else {
            panic!("{} did not resolve", service.name)
        };
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.cap_atomic, 54348);
        let input = service.inputs(&task, e, &catalog.vocabulary).unwrap();
        let source = raw("catalog")["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["slug"] == service.api_slug)
            .unwrap()
            .clone();
        let mut url = url::Url::parse(&format!(
            "{}{}",
            source["origin"].as_str().unwrap(),
            service.path
        ))
        .unwrap();
        for (key, value) in input.as_object().unwrap() {
            url.query_pairs_mut().append_pair(
                key,
                &value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string()),
            );
        }
        assert!(adapter::prepare(&service.api_slug, "GET", url.as_str())
            .unwrap()
            .is_some());
    }
}

#[tokio::test]
#[ignore = "configured intent model and read-only production catalog; no source purchases"]
async fn live_natural_language_routes_to_each_new_capability() {
    use apiosk_gateway_v2::{
        catalog::{Catalog, Snapshot},
        parser::Parser,
        resolver::{resolve, Resolution},
        state::{Entity, Fact, Task},
    };
    use chrono::Utc;
    use uuid::Uuid;
    dotenvy::dotenv().ok();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&std::env::var("DATABASE_URL").unwrap())
        .await
        .unwrap();
    let live: Value = sqlx::query_scalar(include_str!("../src/catalog.sql"))
        .fetch_one(&pool)
        .await
        .unwrap();
    pool.close().await;
    let mut snapshot: Snapshot = serde_json::from_value(live).unwrap();
    if std::env::var("DUTCH_CATALOG_PRODUCTION").as_deref() != Ok("1") {
        let new: Snapshot = serde_json::from_value(raw("catalog")).unwrap();
        snapshot
            .services
            .retain(|s| !matches!(s.api_slug.as_str(), "koop" | "rdw" | "rvo"));
        for w in new.vocabulary {
            if !snapshot.vocabulary.iter().any(|v| v.slug == w.slug) {
                snapshot.vocabulary.push(w);
            }
        }
        snapshot.services.extend(new.services);
    }
    let catalog = Catalog::build(snapshot);
    assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
    let parser = Parser::new(
        reqwest::Client::new(),
        std::env::var("INTENT_API_URL").unwrap(),
        std::env::var("INTENT_API_KEY").unwrap(),
        std::env::var("INTENT_MODEL").unwrap(),
    );
    let questions=[
        ("koop","government.publication.search","Zoek via KOOP officiële publicaties met omgevingsvergunning in de titel, gepubliceerd door Amsterdam van 2026-09-01 tot 2026-09-11."),
        ("koop","government.publication.detail","Toon via KOOP publicatie gmb-2026-427235."),
        ("rdw","vehicle.technical-profile","Toon via RDW de technische voertuiggegevens en APK-datum voor kenteken rz-874-r."),
        ("rvo","subsidy.scheme.first-page-search","Zoek in de eerste pagina RVO-subsidiebeschrijvingen naar energie. Geef de bronlinks, geen oordeel over openstelling of geschiktheid."),
    ];
    let mut report = vec![];
    for (source, capability, question) in questions {
        let mut task = Task::default();
        let mut intent = parser
            .parse(Uuid::new_v4(), question, &task, &catalog)
            .await
            .unwrap();
        for subject in &mut intent.subjects {
            let entity = Uuid::new_v4();
            subject.entity_ref = Some(entity);
            task.entities.push(Entity {
                entity_ref: entity,
                label: subject.label.clone(),
                selected: true,
            });
            for (kind, value) in &subject.values {
                task.facts.push(Fact {
                    entity_ref: entity,
                    semantic_type: kind.clone(),
                    value: value.clone(),
                    provenance: json!({}),
                    observed_at: Utc::now(),
                    expires_at: Utc::now() + chrono::Duration::hours(1),
                    verification: "user_asserted".into(),
                });
            }
        }
        let Resolution::Plan(plan) = resolve(&catalog, &task, &intent) else {
            panic!("{question}: {intent:?}")
        };
        assert_eq!(plan.steps.len(), 1, "{question}");
        assert_eq!(plan.steps[0].service.api_slug, source);
        assert_eq!(plan.steps[0].service.contract.capability_slug, capability);
        let inputs = plan.steps[0]
            .service
            .inputs(&task, plan.steps[0].entity_ref, &catalog.vocabulary)
            .unwrap();
        if capability == "government.publication.search" {
            assert_eq!(inputs["from"], "2026-09-01");
            assert_eq!(inputs["until"], "2026-09-11");
            assert_eq!(inputs["authority"], "Amsterdam");
        }
        println!("{source}: {capability} {inputs}");
        report.push(json!({"question":question,"source":source,"capability":capability,"inputs":inputs,"quote":plan.cap_atomic,"intent":intent}));
    }
    std::fs::write(
        "output/source-expansion-20260911/live-planning.json",
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
}
