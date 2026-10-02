#[path = "../legacy-executor/src/proxy/public_sources.rs"]
mod adapter;
use serde_json::{json, Value};
fn raw(n: &str) -> Value {
    serde_json::from_slice(&std::fs::read(format!("tests/fixtures/dutch-links/{n}.json")).unwrap())
        .unwrap()
}
fn req(route: &str, key: &str, value: Value) -> adapter::Prepared {
    let mut url = url::Url::parse(&format!("https://api.pdok.nl/apiosk/{route}")).unwrap();
    url.query_pairs_mut().append_pair(
        key,
        &value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string()),
    );
    adapter::prepare("pdok", "GET", url.as_str())
        .unwrap()
        .unwrap()
}
fn addresses() -> Value {
    serde_json::from_str::<Value>(include_str!(
        "../crates/catalog-core/fixtures/contracts/payloads/kvk-basisprofiel-mollie.json"
    ))
    .unwrap()["_embedded"]["hoofdvestiging"]["adressen"]
        .clone()
}
#[test]
fn public_links_validate_identity_and_preserve_distinct_identifiers() {
    let cases=[("company-address","addresses",addresses(),"address"),("bag-address","vbo",json!("0363010000700614"),"address"),("bag-unit","vbo",json!("0363010000579925"),"bag"),("building","href",json!("https://api.pdok.nl/kadaster/bag/ogc/v2/collections/pand/items/52aceb68-5b01-52a0-ba95-8b0a77ab96df"),"building"),("neighbourhood","code",json!("BU0363EP02"),"neighbourhood"),("linked-parcels","designations",json!(["ASD13-Q-5400"]),"parcel")];
    let mut normalized = serde_json::Map::new();
    for (route, key, value, file) in cases {
        let p = req(route, key, value);
        let result = adapter::normalize(&p, &raw(file)).unwrap();
        assert_eq!(result["count"], 1, "{route}");
        normalized.insert(route.into(), result);
    }
    assert_ne!(
        normalized["company-address"]["matched"]["bag_object_id"],
        normalized["company-address"]["matched"]["bag_address_id"]
    );
    assert_eq!(
        normalized["company-address"]["matched"]["parcel_designations"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert!(normalized["company-address"]["matched"]
        .get("owner")
        .is_none());
    assert_eq!(normalized["building"]["matched"]["bouwjaar"], 1905);
    assert_eq!(normalized["neighbourhood"]["matched"]["jaar"], 2025);
    if std::env::var("DUTCH_LINKS_EXPORT").as_deref() == Ok("1") {
        std::fs::write(
            "tests/fixtures/dutch-links/normalized.json",
            serde_json::to_vec_pretty(&normalized).unwrap(),
        )
        .unwrap();
    }
}
#[test]
fn links_reject_wrong_ids_ignored_filters_foreign_addresses_and_untrusted_links() {
    assert!(adapter::normalize(
        &req("bag-unit", "vbo", json!("0363010000000000")),
        &raw("bag")
    )
    .is_err());
    assert!(adapter::normalize(
        &req("neighbourhood", "code", json!("BU0363AC02")),
        &raw("neighbourhood")
    )
    .is_err());
    assert!(adapter::normalize(
        &req("linked-parcels", "designations", json!(["AMR04-Q-5400"])),
        &raw("parcel")
    )
    .is_err());
    let mut d = raw("address");
    d["response"]["numFound"] = json!(101);
    assert!(adapter::normalize(&req("company-address", "addresses", addresses()), &d).is_err());
    for value in ["https://example.org/secret","https://api.pdok.nl/kadaster/bag/ogc/v2/collections/pand/items/52aceb68-5b01-52a0-ba95-8b0a77ab96df?x=1"]{let mut u=url::Url::parse("https://api.pdok.nl/apiosk/building").unwrap();u.query_pairs_mut().append_pair("href",value);assert!(adapter::prepare("pdok","GET",u.as_str()).is_err());}
    let mut a = addresses();
    a[0]["land"] = json!("België");
    let mut u = url::Url::parse("https://api.pdok.nl/apiosk/company-address").unwrap();
    u.query_pairs_mut().append_pair("addresses", &a.to_string());
    assert!(adapter::prepare("pdok", "GET", u.as_str()).is_err());
    assert!(adapter::prepare(
        "pdok",
        "GET",
        "https://api.pdok.nl/bzk/locatieserver/search/v3_1/free?q=Amsterdam"
    )
    .unwrap()
    .is_none());
}
#[test]
fn address_suffixes_and_multiple_buildings_are_never_guessed() {
    let mut a = addresses();
    a[0]["huisnummerToevoeging"] = json!("A");
    let r = adapter::normalize(&req("company-address", "addresses", a), &raw("address")).unwrap();
    assert_eq!(r["count"], 0);
    assert!(r["matched"].is_null());
    let mut d = raw("bag");
    let second = d["features"][0]["properties"]["pand.href"][0].clone();
    d["features"][0]["properties"]["pand.href"]
        .as_array_mut()
        .unwrap()
        .push(second);
    let r = adapter::normalize(&req("bag-unit", "vbo", json!("0363010000579925")), &d).unwrap();
    assert!(r["matched"].get("single_building_href").is_none());
}
#[tokio::test]
#[ignore = "Anonymous public upstream chain; KVK profile is a stored source fixture"]
async fn live_link_chain() {
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(25))
        .build()
        .unwrap();
    let mut report = vec![];
    async fn run(
        http: &reqwest::Client,
        route: &str,
        key: &str,
        v: Value,
        report: &mut Vec<Value>,
    ) -> Value {
        let p = req(route, key, v);
        let d: Value = http
            .get(&p.url)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        let r = adapter::normalize(&p, &d).unwrap();
        assert!(r["count"].as_u64().unwrap() > 0, "{route}");
        report.push(r.clone());
        r
    }
    let addr = run(
        &http,
        "company-address",
        "addresses",
        addresses(),
        &mut report,
    )
    .await;
    let row = &addr["matched"];
    run(
        &http,
        "bag-address",
        "vbo",
        row["bag_object_id"].clone(),
        &mut report,
    )
    .await;
    let bag = run(
        &http,
        "bag-unit",
        "vbo",
        row["bag_object_id"].clone(),
        &mut report,
    )
    .await;
    run(
        &http,
        "neighbourhood",
        "code",
        row["neighbourhood_code"].clone(),
        &mut report,
    )
    .await;
    run(
        &http,
        "linked-parcels",
        "designations",
        row["parcel_designations"].clone(),
        &mut report,
    )
    .await;
    if let Some(href) = bag["matched"].get("single_building_href") {
        run(&http, "building", "href", href.clone(), &mut report).await;
    }
    std::fs::write(
        "output/dutch-links-20260911/live-chain.json",
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    println!(
        "{} identity-checked live link requests passed",
        report.len()
    );
}

#[test]
fn kvk_fans_out_to_building_statistics_parcels_and_procurement_with_one_shared_lookup() {
    use apiosk_gateway_v2::{
        catalog::{Catalog, Snapshot},
        parser::{Goal, Intent, Subject},
        resolver::{resolve, Resolution},
        state::{Entity, Fact, Task},
    };
    use chrono::Utc;
    use std::collections::BTreeMap;
    use uuid::Uuid;
    let snapshot: Snapshot = serde_json::from_value(raw("catalog")).unwrap();
    let catalog = Catalog::build(snapshot);
    assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
    assert_eq!(catalog.services.len(), 12);
    let entity = Uuid::new_v4();
    let mut task = Task::default();
    task.entities.push(Entity {
        entity_ref: entity,
        label: "Mollie".into(),
        selected: true,
    });
    task.facts.push(Fact {
        entity_ref: entity,
        semantic_type: "company_registry.kvknummer".into(),
        value: json!("30204462"),
        provenance: json!({"kind":"user_input"}),
        observed_at: Utc::now(),
        expires_at: Utc::now() + chrono::Duration::hours(1),
        verification: "user_asserted".into(),
    });
    let intent = Intent {
        subjects: vec![Subject {
            entity_ref: Some(entity),
            label: "Mollie".into(),
            values: BTreeMap::new(),
        }],
        goals: [
            "building.bag-building",
            "area.cbs-neighbourhood-2025",
            "property.linked-parcels",
            "procurement.rsin-mentions",
            "procurement.company-name-mentions",
            "procurement.ted-kvk-notices",
            "procurement.ted-rsin-notices",
        ]
        .iter()
        .map(|cap| Goal {
            capability: cap.to_string(),
            subject: 0,
            constraints: BTreeMap::new(),
        })
        .collect(),
        answer_operations: vec![],
        unresolved_references: vec![],
    };
    let Resolution::Plan(plan) = resolve(&catalog, &task, &intent) else {
        panic!("Identifier graph cannot resolve")
    };
    assert_eq!(
        plan.steps
            .iter()
            .filter(|s| s.service.api_slug == "kvk-dutch-business-register")
            .count(),
        1,
        "Share the KVK profile across all branches"
    );
    assert_eq!(
        plan.steps
            .iter()
            .filter(|s| s.service.path == "/apiosk/company-address")
            .count(),
        1,
        "Share exact address match"
    );
    assert_eq!(plan.steps.len(), 10);
    assert_eq!(
        plan.cap_atomic,
        plan.steps.iter().map(|s| s.service.total()).sum::<i64>()
    );
    // Each planned step binds only actual extracted facts from the previous responses.
    for step in &plan.steps {
        let inputs = step
            .service
            .inputs(&task, entity, &catalog.vocabulary)
            .unwrap();
        let data = step.service.sample_payload.as_ref().unwrap();
        if step.service.path == "/apiosk/company-address" {
            assert_eq!(inputs["addresses"][0]["postcode"], "1015CW");
        }
        let extracted = step
            .service
            .contract
            .extract(data, &catalog.vocabulary)
            .unwrap();
        assert!(extracted.rejected.is_empty());
        for subject in extracted.subjects {
            for f in subject.facts {
                task.facts.push(Fact {
                    entity_ref: entity,
                    semantic_type: f.fact_type,
                    value: f.value,
                    provenance: json!({"kind":"source_fixture"}),
                    observed_at: Utc::now(),
                    expires_at: Utc::now() + chrono::Duration::hours(1),
                    verification: "source_observed".into(),
                });
            }
        }
    }
    let kinds: Vec<_> = task
        .facts
        .iter()
        .map(|f| f.semantic_type.as_str())
        .collect();
    for forbidden in [
        "company.subsidy_awards",
        "property.owner",
        "property.woz",
        "building.energy_label",
    ] {
        assert!(!kinds.contains(&forbidden));
    }
}

#[tokio::test]
#[ignore = "Anonymous public TED search; no Apiosk billing"]
async fn live_ted_identifier_search() {
    let p = adapter::prepare(
        "ted",
        "GET",
        "https://api.ted.europa.eu/apiosk/organisation-notices?identifier=58668320",
    )
    .unwrap()
    .unwrap();
    let response = reqwest::Client::new()
        .post(&p.url)
        .json(p.json_body.as_ref().unwrap())
        .timeout(std::time::Duration::from_secs(25))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let data: Value = response.json().await.unwrap();
    let result = adapter::normalize(&p, &data).unwrap();
    assert!(!result["items"].as_array().unwrap().is_empty());
    std::fs::write(
        "tests/fixtures/dutch-links/ted.json",
        serde_json::to_vec_pretty(&data).unwrap(),
    )
    .unwrap();
    std::fs::write(
        "tests/fixtures/dutch-links/ted-normalized.json",
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    println!(
        "TED exact identifier search passed: {} notices, total {}",
        result["items"].as_array().unwrap().len(),
        result["total"]
    );
}

#[tokio::test]
#[ignore="Configured intent model and read-only production catalog; no source purchases"]
async fn live_model_plans_identifier_chains(){
 use apiosk_gateway_v2::{catalog::{Catalog,Snapshot},parser::Parser,resolver::{resolve,Resolution},state::{Task,Entity,Fact}};
 use chrono::Utc;use uuid::Uuid;
 dotenvy::dotenv().ok();
 let pool=sqlx::postgres::PgPoolOptions::new().max_connections(1).connect(&std::env::var("DATABASE_URL").unwrap()).await.unwrap();
 let mut snapshot:Snapshot=serde_json::from_value(sqlx::query_scalar::<_,Value>(include_str!("../src/catalog.sql")).fetch_one(&pool).await.unwrap()).unwrap();pool.close().await;
 if std::env::var("DUTCH_LINKS_PRODUCTION").as_deref()!=Ok("1") {
  let additions:Snapshot=serde_json::from_value(raw("catalog")).unwrap();let caps:Vec<_>=additions.services.iter().map(|s|(s.api_slug.clone(),s.contract.capability_slug.clone())).collect();snapshot.services.retain(|s|!caps.contains(&(s.api_slug.clone(),s.contract.capability_slug.clone())));snapshot.services.extend(additions.services);
  for w in additions.vocabulary {if !snapshot.vocabulary.iter().any(|v|v.slug==w.slug){snapshot.vocabulary.push(w);}}
 }
 let catalog=Catalog::build(snapshot);assert!(catalog.rejected.is_empty(),"{:?}",catalog.rejected);
 let parser=Parser::new(reqwest::Client::new(),std::env::var("INTENT_API_URL").unwrap(),std::env::var("INTENT_API_KEY").unwrap(),std::env::var("INTENT_MODEL").unwrap());let mut report=vec![];
 for (question,expected) in [
  ("Zoek vanuit KVK-nummer 30204462 het gekoppelde BAG-pand met bouwjaar, CBS-buurtstatistieken voor 2025 en gekoppelde kadastrale percelen. Gebruik het bezoekadres van de hoofdvestiging.",vec!["building.bag-building","area.cbs-neighbourhood-2025","property.linked-parcels"]),
  ("Geef CBS-buurtstatistieken voor 2025 bij BAG adresseerbaarobject-ID 0363010000700614.",vec!["area.cbs-neighbourhood-2025"]),
  ("Zoek voor KVK-nummer 30204462 via TenderNed naar vermeldingen van de bedrijfsnaam en het RSIN. Zoek ook via TED naar notices met het KVK-nummer en RSIN in de buyer/winner identifiers. Geen conclusie over bevestigde gunning of subsidieontvangst.",vec!["procurement.company-name-mentions","procurement.rsin-mentions","procurement.ted-kvk-notices","procurement.ted-rsin-notices"]),
 ] {
  let mut task=Task::default();let mut intent=parser.parse(Uuid::new_v4(),question,&task,&catalog).await.unwrap();
  for subject in &mut intent.subjects {let id=Uuid::new_v4();subject.entity_ref=Some(id);task.entities.push(Entity{entity_ref:id,label:subject.label.clone(),selected:true});for(k,v)in &subject.values{task.facts.push(Fact{entity_ref:id,semantic_type:k.clone(),value:v.clone(),provenance:json!({}),observed_at:Utc::now(),expires_at:Utc::now()+chrono::Duration::hours(1),verification:"user_asserted".into()});}}
  let Resolution::Plan(plan)=resolve(&catalog,&task,&intent)else{panic!("Unresolved: {question} {intent:?}")};
  for cap in expected{assert!(plan.steps.iter().any(|s|s.service.contract.capability_slug==cap),"Missing {cap}: {intent:?}");}
  assert!(plan.steps.iter().filter(|s|s.service.api_slug=="kvk-dutch-business-register").count()<=1,"{intent:?} {:?}",plan.steps.iter().map(|s|(&s.service.contract.capability_slug,s.entity_ref)).collect::<Vec<_>>());
  println!("{} steps, quote {}: {question}",plan.steps.len(),plan.cap_atomic);
  report.push(json!({"question":question,"intent":intent,"plan":plan}));
 }
 std::fs::write("output/dutch-links-20260911/live-model-plans.json",serde_json::to_vec_pretty(&report).unwrap()).unwrap();
}


#[test]
fn reviewed_record_metadata_describes_real_source_fields_without_adding_validation_constraints() {
    let reviewed: Value = serde_json::from_str(include_str!("fixtures/dutch-links/reviewed-record-schemas.json")).unwrap();
    let normalized: Value = serde_json::from_str(include_str!("fixtures/dutch-links/normalized.json")).unwrap();
    for (kind, route) in [
        ("location.bag_unit_record", "bag-unit"),
        ("location.building_record", "building"),
        ("location.cbs_neighbourhood_2025", "neighbourhood"),
    ] {
        let schema = &reviewed["schemas"][kind];
        let actual = &normalized[route]["matched"];
        for (name, definition) in schema["properties"].as_object().unwrap() {
            assert!(actual.get(name).is_some(), "{route}: undeclared source property {name}");
            assert!(definition.as_object().unwrap().keys().all(|key| key == "$comment"), "annotations must not reject existing source values");
        }
        assert!(schema.get("required").is_none());
        for value in [actual.clone(), json!({}), json!({"future_source_field":null})] {
            catalog_core::facts::schema::validate(schema, &value).unwrap();
        }
        assert!(!schema.to_string().contains("floor_count"));
        assert!(!schema.to_string().contains("energy_label"));
        assert!(!schema.to_string().contains("building_name"));
    }
}

#[test]
fn damrak_public_responses_keep_the_supplied_address_linked_to_its_building_and_neighbourhood() {
    use apiosk_gateway_v2::catalog::{Catalog, Snapshot};
    let captured: Value = serde_json::from_str(include_str!("fixtures/dutch-links/damrak-public-chain.json")).unwrap();
    let catalog = Catalog::build(serde_json::from_value::<Snapshot>(raw("catalog")).unwrap());
    assert!(catalog.rejected.is_empty());
    let address = &captured["address"]["response"]["docs"][0];
    assert_eq!(captured["address"]["response"]["numFound"], 1);
    assert_eq!(address["straatnaam"], "Damrak");
    assert_eq!(address["huisnummer"], 1);
    assert_eq!(address["woonplaatsnaam"], "Amsterdam");
    let vbo = address["adresseerbaarobject_id"].clone();
    let unit = adapter::normalize(&req("bag-unit", "vbo", vbo.clone()), &captured["bag-unit"]).unwrap();
    assert_eq!(unit["matched"]["hoofdadres_identificatie"], address["nummeraanduiding_id"]);
    let href = unit["matched"]["single_building_href"].clone();
    let building = adapter::normalize(&req("building", "href", href), &captured["building"]).unwrap();
    let context = adapter::normalize(&req("bag-address", "vbo", vbo), &captured["bag-address"]).unwrap();
    assert_eq!(context["count"], 4, "one registered unit can have multiple address aliases");
    let aliases = context["items"].as_array().unwrap();
    assert!(aliases.iter().all(|row| row["bag_object_id"] == address["adresseerbaarobject_id"] && row["neighbourhood_code"] == address["buurtcode"]));
    let neighbourhood = adapter::normalize(&req("neighbourhood", "code", address["buurtcode"].clone()), &captured["neighbourhood"]).unwrap();
    for (capability, data) in [
        ("building.bag-unit", &unit),
        ("building.bag-building", &building),
        ("location.bag-address-context", &context),
        ("area.cbs-neighbourhood-2025", &neighbourhood),
    ] {
        let service = catalog.services.iter().find(|s| s.contract.capability_slug == capability).unwrap();
        let extracted = service.contract.extract(data, &catalog.vocabulary).unwrap();
        assert!(extracted.rejected.iter().all(|rejection| service.optional_outputs.contains(&rejection.fact_type)), "{capability}: required output rejected: {:?}", extracted.rejected);
        assert!(!extracted.subjects.is_empty(), "{capability}");
    }
    assert_eq!(building["matched"]["bouwjaar"], 1890);
    assert_eq!(unit["matched"]["oppervlakte"], 16100, "area belongs to the registered unit");
    assert_eq!(neighbourhood["matched"]["jaar"], 2025);
    assert_eq!(neighbourhood["matched"]["aantal_inwoners"], 485);
    assert_eq!(neighbourhood["matched"]["aantal_huishoudens"], 345);
    assert!(building["matched"].get("floor_count").is_none());
}

#[test]
fn cbs_2025_special_values_are_missing_statistics_with_source_provenance() {
    let captured: Value = serde_json::from_str(include_str!("fixtures/dutch-links/damrak-public-chain.json")).unwrap();
    let source = &captured["neighbourhood"];
    assert_eq!(source["features"][0]["properties"]["percentage_verweduwd"], -99997);
    let request = req("neighbourhood", "code", json!("BU0363AD03"));
    let normalized = adapter::normalize(&request, source).unwrap();
    for row in [&normalized["matched"], &normalized["items"][0]] {
        assert!(row["percentage_verweduwd"].is_null());
        assert_eq!(row["aantal_inwoners"], 485);
        assert_eq!(row["aantal_huishoudens"], 345);
        assert_eq!(row["jaar"], 2025);
    }
    assert_eq!(normalized["coverage"]["missing_values"], json!([{
        "field":"percentage_verweduwd", "source_value":"CBS special code -99997",
        "reason":"suppressed_or_not_present"
    }]));
    assert!(normalized["notice"].as_str().unwrap().contains("null, not numeric values"));
    assert_eq!(source["features"][0]["properties"]["percentage_verweduwd"], -99997, "original source evidence remains unchanged");

    let mut other_codes = source.clone();
    let properties = &mut other_codes["features"][0]["properties"];
    properties["pending_test"] = json!(-99995.0);
    properties["discontinued_test"] = json!(-99991);
    properties["negative_test"] = json!(-12);
    properties["other_code_test"] = json!(-99999);
    properties["zero_test"] = json!(0);
    properties["string_test"] = json!("-99997");
    let normalized = adapter::normalize(&request, &other_codes).unwrap();
    assert!(normalized["matched"]["pending_test"].is_null());
    assert!(normalized["matched"]["discontinued_test"].is_null());
    assert_eq!(normalized["matched"]["negative_test"], -12);
    assert_eq!(normalized["matched"]["other_code_test"], -99999);
    assert_eq!(normalized["matched"]["zero_test"], 0);
    assert_eq!(normalized["matched"]["string_test"], "-99997");
    let missing = normalized["coverage"]["missing_values"].as_array().unwrap();
    assert_eq!(missing.len(), 3);
    assert!(missing.iter().any(|value| value["field"] == "pending_test" && value["reason"] == "publication_pending"));
    assert!(missing.iter().any(|value| value["field"] == "discontinued_test" && value["reason"] == "no_longer_published"));

    let mut bag = captured["bag-unit"].clone();
    bag["features"][0]["properties"]["test_value"] = json!(-99997);
    let normalized = adapter::normalize(&req("bag-unit", "vbo", json!("0363010012111931")), &bag).unwrap();
    assert_eq!(normalized["matched"]["test_value"], -99997, "BAG values are not CBS statistics");
    assert!(normalized["coverage"].get("missing_values").is_none());
}
