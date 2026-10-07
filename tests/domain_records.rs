//! Recorded, free Google Public DNS responses; no paid source requests.
#[allow(dead_code)]
#[path = "../legacy-executor/src/proxy/public_sources.rs"]
mod source_adapter;

use serde_json::{json, Value};
use apiosk_gateway_v2::{ask_v2::search, capability_intake::{self, Intake}, catalog::{Catalog, Snapshot}, parser::Parser, resolver::{self, Resolution}, state::Task};

fn typed_catalog() -> Catalog {
    let snapshot: Snapshot = serde_json::from_str(include_str!("fixtures/domain-intelligence/typed/catalog.json")).unwrap();
    let catalog = Catalog::build(snapshot);
    assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
    assert_eq!(catalog.services.len(), 8);
    catalog
}

#[test]
fn typed_dns_search_ignores_registry_metadata_and_incidental_aliases() {
    let mut snapshot: Snapshot = serde_json::from_str(include_str!("fixtures/domain-intelligence/typed/catalog.json")).unwrap();
    let legacy: Snapshot = serde_json::from_str(include_str!("fixtures/domain-intelligence/services.json")).unwrap();
    snapshot.services.extend(legacy.services);
    snapshot.vocabulary.extend(legacy.vocabulary);
    let catalog = Catalog::build(snapshot);
    assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
    assert_eq!(catalog.services.len(), 10);
    for kind in ["A", "AAAA", "CNAME", "MX", "NS", "TXT", "CAA", "SOA"] {
        let question = format!("Get the DNS {kind} records for www.mollie.com.");
        let request = capability_intake::normalize(&json!({"language":"en",
            "subjects":[{"id":"site","label":"www.mollie.com","type":"domain","identifiers":{"domain":"www.mollie.com"}}],
            "capabilities":[{"slug":"research.domain.dns","name":format!("DNS {kind} records"),"description":format!("Retrieve {kind} records."),
                "domain":"research","inputs":["domain"],"outputs":[format!("{}_records", kind.to_ascii_lowercase())],"subject_id":"site","source_hint":null,"confidence":1.0}],
            "deliverable":{"format":"chat","operations":["summarize"]}}).to_string(), &question, "fixture").unwrap();
        let matches = search::search(&request, &catalog);
        assert_eq!(matches[0].operations, std::collections::BTreeSet::from([format!("domain.dns-{}", kind.to_ascii_lowercase())]), "{kind}");
    }
    for name in ["www.mollie.com", "mail.example.co.uk", "_dmarc.mollie.com"] {
        let question = format!("Get the DNS MX records for {name}.");
        let request = capability_intake::normalize(&json!({"language":"en",
            "subjects":[{"id":"site","label":name,"type":"domain","identifiers":{"domain":name}}],
            "capabilities":[{"slug":"research.domain.dns","name":"DNS MX records","description":"Retrieve MX records.",
                "domain":"research","inputs":["domain"],"outputs":["mx"],"subject_id":"site","source_hint":null,"confidence":1.0}],
            "deliverable":{"format":"chat","operations":["summarize"]}}).to_string(), &question, "fixture").unwrap();
        assert_eq!(search::search(&request, &catalog)[0].operations,
            std::collections::BTreeSet::from(["domain.dns-mx".into()]), "must preserve exact owner {name}");
    }
}

#[test]
fn typed_dns_live_fixtures_preserve_record_identity_values_and_ttl() {
    let catalog = typed_catalog();
    for (kind, code, raw) in [
        ("A", 1, include_str!("fixtures/domain-intelligence/typed/mollie-com-a.json")),
        ("AAAA", 28, include_str!("fixtures/domain-intelligence/typed/mollie-com-aaaa.json")),
        ("CNAME", 5, include_str!("fixtures/domain-intelligence/typed/mollie-com-cname.json")),
        ("MX", 15, include_str!("fixtures/domain-intelligence/typed/mollie-com-mx.json")),
        ("NS", 2, include_str!("fixtures/domain-intelligence/typed/mollie-com-ns.json")),
        ("TXT", 16, include_str!("fixtures/domain-intelligence/typed/mollie-com-txt.json")),
        ("CAA", 257, include_str!("fixtures/domain-intelligence/typed/mollie-com-caa.json")),
        ("SOA", 6, include_str!("fixtures/domain-intelligence/typed/mollie-com-soa.json")),
    ] {
        let prepared = source_adapter::prepare("domain-intelligence", "GET", &format!("https://dns.google/dns/mollie.com/{kind}")).unwrap().unwrap();
        assert_eq!(prepared.url, format!("https://dns.google/resolve?name=mollie.com&type={kind}"));
        let provider: Value = serde_json::from_str(raw).unwrap();
        let result = source_adapter::normalize_bytes(&prepared, raw.as_bytes()).unwrap();
        let service = catalog.services.iter().find(|service| service.path.ends_with(&format!("/{kind}"))).unwrap();
        let extraction = service.contract.extract(&result, &catalog.vocabulary).unwrap();
        assert!(extraction.rejected.is_empty(), "{kind}: {:?}", extraction.rejected);
        assert_eq!(extraction.subjects.len(), 1);
        assert_eq!(extraction.subjects[0].fact(&format!("domain.dns-{}.result", kind.to_ascii_lowercase())).unwrap().value, result);
        assert_eq!(result["data"]["domain"], "mollie.com");
        assert_eq!(result["data"]["record_type"], kind);
        assert_eq!(result["data"]["response_code"], provider["Status"]);
        assert_eq!(result["source_url"], prepared.url);
        assert_eq!(result["coverage"]["complete"], provider["TC"] != true);
        let raw_records: Vec<_> = provider["Answer"].as_array().into_iter().flatten()
            .filter(|record| record["type"] == code || record["type"] == 5).collect();
        let records = result["data"]["records"].as_array().unwrap();
        assert_eq!(records.len(), raw_records.len());
        for (record, raw_record) in records.iter().zip(raw_records) {
            assert_eq!(record["name"], raw_record["name"].as_str().unwrap().trim_end_matches('.').to_ascii_lowercase());
            assert_eq!(record["ttl"], raw_record["TTL"]);
            assert_eq!(record["data"], raw_record["data"]);
            assert_eq!(record["type_code"], raw_record["type"]);
        }
    }
}

#[tokio::test]
async fn typed_dns_search_planning_alias_coverage_and_quote_buy_only_requested_types() {
    use axum::{routing::post, Json, Router};
    async fn model(Json(body): Json<Value>) -> Json<Value> {
        assert_eq!(body["messages"].as_array().unwrap().len(), 2, "valid typed DNS plan should need no repair: {}", body["messages"].as_array().unwrap().last().unwrap());
        let request: Value = serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
        let operations = request["source_matches"][0]["apiosk_operations"].as_array().unwrap();
        assert!((1..=3).contains(&operations.len()), "a typed DNS requirement gets its contributing operations");
        let subject = &request["source_matches"][0]["subject_ref"];
        let value_ref = format!("{}.identifiers.domain", subject.as_str().unwrap());
        let goals: Vec<_> = operations.iter().map(|operation| json!({"capability":operation,"subject":0,"constraints":{}})).collect();
        Json(json!({"choices":[{"message":{"content":json!({
            "message":"Retrieve the requested DNS records after price approval.", "discover":true, "decline_reason":"none",
            "intent":{"subjects":[{"subject_ref":subject,"values":{"domain.dns_name":{"ref":value_ref}}}],"goals":goals,
                "answer_operations":["Preserve exact DNS name, type, TTL and source; these records do not prove company ownership."],"unresolved_references":[]}
        }).to_string()}}]}))
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, Router::new().route("/model", post(model))).await.unwrap(); });
    let parser = Parser::new(reqwest::Client::new(), format!("http://{address}/model"), "fixture".into(), "fixture".into());
    let catalog = typed_catalog();
    let kinds: &[&[&str]] = &[&["A"], &["AAAA"], &["CNAME"], &["MX"], &["NS"], &["TXT"], &["CAA"], &["SOA"], &["A", "MX"], &["A", "MX", "TXT"]];
    for kinds in kinds {
        let name = if *kinds == ["TXT"] {"_dmarc.mollie.com"} else {"www.mollie.com"};
        let label = kinds.join(" and ");
        let question = format!("Get the DNS {label} records for {name} with source information.");
        let expected: std::collections::BTreeSet<_> = kinds.iter().map(|kind| format!("domain.dns-{}", kind.to_ascii_lowercase())).collect();
        let outputs: Vec<_> = kinds.iter().map(|kind| format!("{}_records", kind.to_ascii_lowercase()))
            .chain(["dns_records".into(), "source_url".into()]).collect();
        let parsed_request = capability_intake::normalize(&json!({"language":"en",
            "subjects":[{"id":"site","label":name,"type":"domain","identifiers":{"domain":name}}],
            "capabilities":[{"slug":"research.domain.dns","name":format!("DNS {label} records"),"description":format!("Retrieve the requested {label} DNS records."),
                "domain":"research","inputs":["domain"],"outputs":outputs,"subject_id":"site","source_hint":null,"confidence":1.0}],
            "deliverable":{"format":"chat","operations":["summarize"]}}).to_string(), &question, "fixture").unwrap();
        let matches = search::search(&parsed_request, &catalog);
        assert_eq!(matches[0].operations, expected, "{label}");
        let intake = Intake { parsed_request, selected_capabilities:matches.iter().flat_map(|matched| matched.operations.iter().cloned()).collect(),
            matches, apiosk_sources_searched:1, catalog_version:catalog.version.clone() };
        // A local model proposes the semantic operation and an exact parsed
        // value reference; the actual parser must prove every output alias.
        let reply = parser.chat_from_capabilities(&question, None, &Task::default(), &catalog, &intake).await.unwrap();
        assert!(reply.discover, "{label}: {}", reply.message);
        let mut intent = reply.intent.unwrap();
        let mut task = Task { capability_intake:Some(intake), ..Default::default() };
        apiosk_gateway_v2::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
        let Resolution::Plan(plan) = resolver::resolve(&catalog, &task, &intent) else {panic!("{label} did not produce a quote")};
        assert_eq!(plan.steps.len(), kinds.len(), "no automatic purchase of other DNS types");
        assert_eq!(plan.steps.iter().map(|step| step.service.contract.capability_slug.clone()).collect::<std::collections::BTreeSet<_>>(), expected);
        for step in &plan.steps {
            assert_eq!(step.service.inputs(&task, step.entity_ref, &catalog.vocabulary).unwrap()["domain"], name);
        }
        assert_eq!(plan.cap_atomic, plan.steps.iter().map(|step| step.service.total()).sum::<i64>());
    }
    server.abort();
}

#[test]
fn typed_dns_keeps_exact_subdomains_and_rejects_unsupported_routes() {
    for name in ["www.mollie.com", "_dmarc.mollie.com", "selector._domainkey.mollie.com"] {
        let prepared = source_adapter::prepare("domain-intelligence", "GET", &format!("https://dns.google/dns/{name}/TXT")).unwrap().unwrap();
        assert_eq!(prepared.url, format!("https://dns.google/resolve?name={name}&type=TXT"));
    }
    for route in ["mollie.com/ANY", "mollie.com/AXFR", "mollie.com/TXT/extra", "a..mollie.com/A", "mollie.com%2Fother/A", "127.0.0.1/A"] {
        assert!(source_adapter::prepare("domain-intelligence", "GET", &format!("https://dns.google/dns/{route}")).is_err(), "{route}");
    }
    // The existing purchase contract keeps its registrable-domain semantics.
    let legacy = source_adapter::prepare("domain-intelligence", "GET", "https://dns.google/dns/www.mollie.com").unwrap().unwrap();
    assert_eq!(legacy.url, "https://dns.google/resolve?name=mollie.com&type=MX");
}

#[test]
fn typed_dns_distinguishes_no_data_nxdomain_truncation_and_provider_errors() {
    let prepared = source_adapter::prepare("domain-intelligence", "GET", "https://dns.google/dns/_dmarc.example.com/TXT").unwrap().unwrap();
    let normalize = |data: &Value| source_adapter::normalize_bytes(&prepared, &serde_json::to_vec(data).unwrap());
    let base = json!({"Status":0,"TC":false,"AD":false,"Question":[{"name":"_dmarc.example.com.","type":16}]});
    let result = normalize(&base).unwrap();
    assert_eq!(result["data"]["exists"], true);
    assert_eq!(result["data"]["records"], json!([]));
    let mut changed = base.clone(); changed["Status"] = json!(3);
    assert_eq!(normalize(&changed).unwrap()["data"]["exists"], false);
    changed = base.clone(); changed["TC"] = json!(true);
    assert_eq!(normalize(&changed).unwrap()["coverage"]["complete"], false);
    changed = base.clone(); changed.as_object_mut().unwrap().remove("TC");
    assert_eq!(normalize(&changed).unwrap()["coverage"]["complete"], false);
    changed = base.clone(); changed["Answer"] = json!({"data":"not a record array"});
    assert!(normalize(&changed).is_err());
    for status in [1, 2, 5] {
        changed = base.clone(); changed["Status"] = json!(status);
        assert!(normalize(&changed).is_err());
    }
    changed = base.clone(); changed["Question"][0]["type"] = json!(15);
    assert!(normalize(&changed).is_err());
    changed = base.clone(); changed["Question"][0]["name"] = json!("example.com.");
    assert!(normalize(&changed).is_err());
    changed = base.clone(); changed["Answer"] = json!([{"name":"_dmarc.example.com.","type":16,"data":"\"v=DMARC1; p=reject\""}]);
    assert_eq!(normalize(&changed).unwrap()["data"]["records"][0]["ttl"], Value::Null);
    changed["Status"] = json!(3);
    assert!(normalize(&changed).is_err());
}

#[test]
fn typed_dns_retains_cname_chain_and_does_not_relabel_incidental_records() {
    let prepared = source_adapter::prepare("domain-intelligence", "GET", "https://dns.google/dns/www.example.com/A").unwrap().unwrap();
    let raw = json!({"Status":0,"TC":false,"AD":true,"Question":[{"name":"www.example.com.","type":1}],"Answer":[
        {"name":"www.example.com.","type":5,"TTL":60,"data":"cdn.example.net."},
        {"name":"cdn.example.net.","type":1,"TTL":120,"data":"192.0.2.1"},
        {"name":"cdn.example.net.","type":46,"TTL":120,"data":"opaque signature"}
    ]});
    let result = source_adapter::normalize_bytes(&prepared, &serde_json::to_vec(&raw).unwrap()).unwrap();
    assert_eq!(result["data"]["records"].as_array().unwrap().len(), 2);
    assert_eq!(result["data"]["records"][0]["type"], "CNAME");
    assert_eq!(result["data"]["records"][1]["name"], "cdn.example.net");
    assert_eq!(result["data"]["records"][1]["data"], "192.0.2.1");
    assert_eq!(result["data"]["dnssec_validated"], true);
}

#[test]
fn typed_dns_nxdomain_can_describe_a_missing_alias_target_not_the_queried_name() {
    for (kind, code) in [("A", 1), ("AAAA", 28)] {
        let prepared = source_adapter::prepare("domain-intelligence", "GET", &format!("https://dns.google/dns/www.example.com/{kind}")).unwrap().unwrap();
        let mut raw = json!({"Status":3,"TC":false,"AD":false,
            "Question":[{"name":"www.example.com.","type":code}],
            "Answer":[{"name":"www.example.com.","type":5,"TTL":60,"data":"missing.example.net."}]});
        let result = source_adapter::normalize_bytes(&prepared, &serde_json::to_vec(&raw).unwrap()).unwrap();
        assert_eq!(result["data"]["exists"], true, "the returned CNAME proves the queried alias exists");
        assert_eq!(result["data"]["response_code"], 3, "preserve the missing target response");
        assert_eq!(result["data"]["records"][0]["data"], "missing.example.net.");
        raw["Answer"][0]["name"] = json!("unrelated.example.com.");
        let result = source_adapter::normalize_bytes(&prepared, &serde_json::to_vec(&raw).unwrap()).unwrap();
        assert_eq!(result["data"]["exists"], false, "another owner's alias cannot establish this name");
    }
}
