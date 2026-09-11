use apiosk_gateway_v2::{
    catalog::{Catalog, KnownSource, Snapshot},
    parser::{Goal, Intent, Parser, Subject},
    resolver::{resolve, Resolution},
    state::{Entity, Fact, Task},
};
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

fn snapshot() -> Snapshot {
    serde_json::from_str(include_str!("fixtures/public-statistics/catalog.json")).unwrap()
}
fn catalog() -> Catalog {
    let result = Catalog::build(snapshot());
    assert!(result.rejected.is_empty(), "{:?}", result.rejected);
    assert_eq!(result.services.len(), 15);
    result
}
fn task(values: &[(&str, Value)]) -> (Uuid, Task) {
    let entity = Uuid::new_v4();
    let mut task = Task::default();
    for (kind, value) in values {
        task.facts.push(Fact {
            entity_ref: entity,
            semantic_type: kind.to_string(),
            value: value.clone(),
            provenance: json!({"kind":"test"}),
            observed_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::hours(1),
            verification: "user_asserted".into(),
        });
    }
    (entity, task)
}
fn intent(entity: Uuid, source: &str, cap: &str) -> Intent {
    Intent {
        subjects: vec![Subject {
            entity_ref: Some(entity),
            label: "dataset".into(),
            values: BTreeMap::new(),
        }],
        goals: vec![Goal {
            capability: cap.into(),
            subject: 0,
            constraints: BTreeMap::from([("source".into(), json!(source))]),
        }],
        answer_operations: vec![],
        unresolved_references: vec![],
    }
}
#[test]
fn all_three_sources_are_visible_and_every_sample_extracts_declared_facts() {
    let c = catalog();
    let view = c.parser_view();
    for slug in ["cbs-statline", "data-overheid-nl", "eurostat"] {
        assert!(view["sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["slug"] == slug && s["available"] == true));
    }
    for service in &c.services {
        let extracted = service
            .contract
            .extract(service.sample_payload.as_ref().unwrap(), &c.vocabulary)
            .unwrap();
        assert!(extracted.rejected.is_empty());
        for name in service.contract.output_map.produces.keys() {
            assert!(
                extracted.subjects.iter().any(|s| s.fact(name).is_some()),
                "{} missing {name}",
                service.path
            );
        }
    }
}
#[test]
fn documented_query_inputs_and_source_constraints_are_preserved() {
    let c = catalog();
    for (source, cap, values, expected) in [
        (
            "cbs-statline",
            "statistics.cbs-bankruptcies",
            vec![("cbs.input.odata_filter", json!("Perioden eq '2023JJ00'"))],
            json!({"$top":100,"$count":true,"$filter":"Perioden eq '2023JJ00'"}),
        ),
        (
            "data-overheid-nl",
            "data.catalog-search",
            vec![("donl.input.q", json!("laadpalen"))],
            json!({"q":"laadpalen","rows":20}),
        ),
        (
            "eurostat",
            "statistics.eurostat-data",
            vec![
                ("eurostat.input.dataset", json!("nama_10_gdp")),
                ("eurostat.input.geo", json!("NL")),
                ("eurostat.input.time", json!("2023")),
                ("eurostat.input.unit", json!("CP_MEUR")),
                ("eurostat.input.na_item", json!("B1GQ")),
            ],
            json!({"dataset":"nama_10_gdp","geo":"NL","time":"2023","unit":"CP_MEUR","na_item":"B1GQ","format":"JSON","lang":"EN"}),
        ),
    ] {
        let (entity, t) = task(&values);
        let i = intent(entity, source, cap);
        let Resolution::Plan(p) = resolve(&c, &t, &i) else {
            panic!("must plan {cap}")
        };
        assert_eq!(p.steps.len(), 1);
        assert_eq!(p.steps[0].service.api_slug, source);
        assert_eq!(
            p.steps[0]
                .service
                .inputs(&t, entity, &c.vocabulary)
                .unwrap(),
            expected
        );
        let mut unsupported = i;
        unsupported.goals[0]
            .constraints
            .insert("unmapped_period".into(), json!("latest exact year"));
        assert!(matches!(
            resolve(&c, &t, &unsupported),
            Resolution::Unsupported(_)
        ));
    }
}
#[test]
fn dataset_metadata_can_follow_search_without_inventing_identifiers() {
    let c = catalog();
    let (entity, t) = task(&[("donl.input.q", json!("laadpalen"))]);
    let Resolution::Plan(p) = resolve(
        &c,
        &t,
        &intent(entity, "data-overheid-nl", "data.dataset-metadata"),
    ) else {
        panic!("search then metadata")
    };
    assert_eq!(p.steps.len(), 2);
    assert_eq!(
        p.steps[0].service.contract.capability_slug,
        "data.catalog-search"
    );
    assert!(p.steps[1]
        .service
        .inputs(&t, entity, &c.vocabulary)
        .is_err());
}
#[test]
fn eurostat_error_envelopes_are_not_statistics() {
    let c = catalog();
    let s = c
        .services
        .iter()
        .find(|s| s.contract.capability_slug == "statistics.eurostat-data")
        .unwrap();
    let extraction = s
        .contract
        .extract(
            &json!({"error":{"status":400,"label":"Unknown dimension"}}),
            &c.vocabulary,
        )
        .unwrap();
    assert!(!extraction.rejected.is_empty());
}
#[test]
fn known_blocked_source_is_visible_but_never_executable() {
    let mut s = snapshot();
    s.known_sources.push(KnownSource {
        metadata: Default::default(),
        slug: "eodhd".into(),
        name: "EODHD Market Data".into(),
        block_reason: Some("Resale authorization is not recorded".into()),
    });
    let mut prohibited = s.services[0].clone();
    prohibited.api_slug = "eodhd".into();
    prohibited.id = Uuid::new_v4();
    s.services.push(prohibited);
    let c = Catalog::build(s);
    assert_eq!(c.services.len(), 15);
    assert_eq!(c.rejected.len(), 1);
    assert!(c.parser_view()["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["slug"] == "eodhd"
            && s["available"] == false
            && s["reason"].as_str().unwrap().contains("Resale")));
}

#[tokio::test]
#[ignore = "three free public upstream reads; no Apiosk purchase"]
async fn live_public_response_shapes() {
    let c = catalog();
    let client = reqwest::Client::new();
    for (cap,url) in [
        ("statistics.cbs-datasets","https://datasets.cbs.nl/odata/v1/CBS/Datasets?$top=2&$count=true"),
        ("data.catalog-search","https://data.overheid.nl/data/api/3/action/package_search?q=laadpalen&rows=2"),
        ("statistics.eurostat-data","https://ec.europa.eu/eurostat/api/dissemination/statistics/1.0/data/nama_10_gdp?geo=NL&time=2023&unit=CP_MEUR&na_item=B1GQ&lang=EN&format=JSON"),
    ] {
        let value:Value=client.get(url).timeout(std::time::Duration::from_secs(30)).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        let s=c.services.iter().find(|s|s.contract.capability_slug==cap).unwrap();
        let extracted=s.contract.extract(&value,&c.vocabulary).unwrap();
        assert!(extracted.rejected.is_empty(),"{cap}: {:?}",extracted.rejected);
        assert!(!extracted.subjects.is_empty(),"{cap}: no sample results");
        println!("PASS live response {cap}: {} subject(s)",extracted.subjects.len());
    }
}

#[tokio::test]
#[ignore = "reads production catalog and calls configured OpenRouter model; no paid data source"]
async fn live_public_statistics_planning() {
    dotenvy::dotenv().ok();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&std::env::var("DATABASE_URL").unwrap())
        .await
        .unwrap();
    let value: Value = sqlx::query_scalar(include_str!("../src/catalog.sql"))
        .fetch_one(&pool)
        .await
        .unwrap();
    pool.close().await;
    let mut live: Snapshot = serde_json::from_value(value).unwrap();
    let proposed = snapshot();
    if std::env::var_os("PUBLIC_STATS_DEPLOYED").is_some() {
        for service in &proposed.services {
            assert!(
                live.services.iter().any(|s| s.id == service.id),
                "Missing deployed contract: {}",
                service.id
            );
        }
        assert!(
            live.known_sources
                .iter()
                .any(|s| s.slug == "eodhd" && s.block_reason.is_some()),
            "Missing deployed EODHD block reason"
        );
    } else {
        for f in proposed.vocabulary {
            if !live.vocabulary.iter().any(|v| v.slug == f.slug) {
                live.vocabulary.push(f);
            }
        }
        for s in proposed.services {
            if !live.services.iter().any(|v| v.id == s.id) {
                live.services.push(s);
            }
        }
        if let Some(s) = live.known_sources.iter_mut().find(|s| s.slug == "eodhd") {
            s.block_reason = Some("Resale authorization is not recorded".into());
        }
    }
    let c = Catalog::build(live);
    assert!(c.rejected.is_empty(), "{:?}", c.rejected);
    let parser = Parser::new(
        reqwest::Client::new(),
        std::env::var("INTENT_API_URL").unwrap(),
        std::env::var("INTENT_API_KEY").unwrap(),
        std::env::var("INTENT_MODEL").unwrap(),
    );
    for (question,source,cap) in [
        ("Toon de CBS datasetcatalogus.","cbs-statline","statistics.cbs-datasets"),
        ("Zoek op data.overheid.nl datasets over laadpalen.","data-overheid-nl","data.catalog-search"),
        ("Zoek in Eurostat datasets over inflation.","eurostat","statistics.eurostat-dataset-search"),
        ("Haal via Eurostat dataset nama_10_gdp op voor geo NL, time 2023, unit CP_MEUR en na_item B1GQ.","eurostat","statistics.eurostat-data"),
    ] {
        let mut t=Task::default(); let mut i=parser.parse(Uuid::new_v4(),question,&t,&c).await.unwrap();
        for s in &mut i.subjects {let id=Uuid::new_v4();s.entity_ref=Some(id);t.entities.push(Entity {entity_ref:id,label:s.label.clone(),selected:true});for (kind,value) in &s.values{t.facts.push(Fact {entity_ref:id,semantic_type:kind.clone(),value:value.clone(),provenance:json!({}),observed_at:Utc::now(),expires_at:Utc::now()+chrono::Duration::hours(1),verification:"user_asserted".into()});}}
        let result=resolve(&c,&t,&i); let Resolution::Plan(p)=result else {panic!("{question}: {i:?}; {result:?}")};
        assert_eq!(p.steps.len(),1,"{i:?}");assert_eq!(p.steps[0].service.api_slug,source);assert_eq!(p.steps[0].service.contract.capability_slug,cap);
        println!("PASS {question} -> {source}/{cap}; inputs={}",p.steps[0].service.inputs(&t,p.steps[0].entity_ref,&c.vocabulary).unwrap());
    }
    let blocked = parser
        .chat(
            "Haal EODHD beurskoersen op voor AAPL.US.",
            &Task::default(),
            &c,
        )
        .await
        .unwrap();
    assert!(
        !blocked.discover,
        "blocked source cannot offer discovery: {blocked:?}"
    );
    println!("EODHD: {}", blocked.message);
}
