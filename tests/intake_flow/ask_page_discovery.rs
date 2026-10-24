use super::*;

fn connected_person() -> Person {
    Person { owner:"00000000-0000-4000-8000-000000000001".parse().unwrap(), bearer:"apk_access_local_fixture".into() }
}

fn parse_for(subject: &str, registry: Option<&str>, capabilities: &[(&str, &str, &str)]) -> Value {
    let identifiers = registry.map(|id| json!({"company.id":id})).unwrap_or_else(|| json!({}));
    json!({"language":"en","subjects":[{"id":"s1","label":subject,"type":"company","identifiers":identifiers}],
        "capabilities":capabilities.iter().map(|(slug, domain, output)| json!({"slug":slug,"name":output,"description":output,
            "domain":domain,"inputs":[],"outputs":[output],"subject_id":"s1","source_hint":null,"confidence":0.95})).collect::<Vec<_>>(),
        "deliverable":{"format":"chat","operations":["list"]}})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn ask_page_connected_discovery_matches_app_model_request_and_both_origin_results() {
    let intake = Intake::start().await;
    let connected = connected_person();
    let app = Person::new();
    for (question, output) in [
        ("What does ASML do?", parse_for("ASML", None, &[("company.info","company","company_profile")])),
        ("KYB check for Mollie B.V. (KVK number 30204462): show the registered company profile and screen the company name against sanctions lists.",
            parse_for("Mollie B.V.", Some("30204462"), &[("company.profile","company","registered_company_profile"),("compliance.sanctions","compliance","sanctions_matches")])),
        ("Jaarrekening van Coolblue", ask_v2_capabilities()),
    ] {
        intake.model.ask_v2_replies.lock().unwrap().extend([output.clone(), output]);
        let before = intake.model.ask_v2_requests.lock().unwrap().len();
        let submitted = format!(" \n{question}\t ");
        let (status, parsed) = intake.send(&app.bearer, "/v2/ask-v2/parse", Some(json!({"request_id":Uuid::new_v4(),"question":submitted}))).await;
        assert_eq!(status, StatusCode::OK, "{parsed}");
        let mut originals = std::collections::BTreeMap::new();
        for origin in ["apiosk", "coinbase"] {
            let mut request = json!({"parsed_request":parsed["parsed_request"],"origin":origin});
            if origin == "apiosk" { request["question"] = json!(question); request["planning_token"] = parsed["planning_token"].clone(); }
            let (status, found) = intake.send(&app.bearer, "/v2/ask-v2/search", Some(request)).await;
            assert_eq!(status, StatusCode::OK, "{found}");
            originals.insert(origin, found);
        }
        let (status, found) = intake.send(&connected.bearer, "/v2/ask/page", Some(json!({"question":submitted}))).await;
        assert_eq!(status, StatusCode::OK, "{found}");
        assert_eq!(found["question"], question);
        assert_eq!(found["parsed_request"]["meta"]["text_sha256"], parsed["parsed_request"]["meta"]["text_sha256"]);
        let requests = intake.model.ask_v2_requests.lock().unwrap();
        assert_eq!(requests.len() - before, 2, "one identical interpretation per host");
        assert_eq!(requests[before]["messages"], requests[before + 1]["messages"]);
        assert_eq!(requests[before]["model"], requests[before + 1]["model"]);
        drop(requests);
        for field in ["language", "subjects", "capabilities", "deliverable"] {
            assert_eq!(found["parsed_request"][field], parsed["parsed_request"][field], "{question}: {field}");
        }
        for origin in ["apiosk", "coinbase"] {
            for field in ["matches", "source_options", "apiosk_sources_searched", "catalog_version", "complete_plan_available"] {
                assert_eq!(found["searches"][origin][field], originals[origin][field], "{question}: {origin}.{field}");
            }
        }
        assert!(found["company_context"].is_array());
        assert_eq!(found["context_status"], "complete");
        assert!(found["search_id"].is_string() && found["discovery_id"].is_string());
        if question == "Jaarrekening van Coolblue" {
            assert!(found["searches"]["apiosk"]["matches"].as_array().unwrap().iter().any(|row| !row["apiosk"].as_array().unwrap().is_empty()), "fixture must exercise real local source rows");
        }
    }
    assert_eq!(task_count(&intake, &connected).await, 0);
    assert!(intake.edge.calls.lock().unwrap().is_empty());
    assert!(intake.model.stages.lock().unwrap().iter().all(|stage| *stage == "bazaar"), "discovery uses only the ordinary external directory, never a second planner");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn ask_page_refresh_and_research_keep_the_same_signed_parse_without_new_interpretation() {
    let intake = Intake::start().await;
    let person = connected_person();
    let question = "Jaarrekening van Coolblue";
    let path = "/v2/ask/page";
    assert_eq!(intake.send("invalid-fixture-bearer", path, Some(json!({"question":question}))).await.0, StatusCode::UNAUTHORIZED);
    let read_only = format!("apk_org_live_{}", "b".repeat(48));
    assert_eq!(intake.send(&read_only, path, Some(json!({"question":question}))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(intake.send(&Person::new().bearer, path, Some(json!({"question":question}))).await.0, StatusCode::FORBIDDEN);
    let (status, found) = intake.send(&person.bearer, path, Some(json!({"question":question}))).await;
    assert_eq!(status, StatusCode::OK, "{found}");
    let parses = intake.model.ask_v2_requests.lock().unwrap().len();
    assert!(found["parse_id"].is_string());
    let request = json!({"question":question,"parsed_request":found["parsed_request"],"search_id":found["parse_id"]});
    let (status, fresh) = intake.send(&person.bearer, "/v2/ask/page/search", Some(request.clone())).await;
    assert_eq!(status, StatusCode::OK, "{fresh}");
    assert_eq!(fresh["parsed_request"], found["parsed_request"]);
    assert_eq!(fresh["searches"]["apiosk"]["matches"], found["searches"]["apiosk"]["matches"]);
    assert_eq!(fresh["attempts"], 0);
    assert_eq!(intake.send(ORGANISATION_KEY, "/v2/ask/page/search", Some(request.clone())).await.0, StatusCode::CONFLICT, "snapshot cannot cross connections");
    let mut changed = request; changed["parsed_request"]["subjects"][0]["label"] = json!("Other company");
    assert_eq!(intake.send(&person.bearer, "/v2/ask/page/search", Some(changed)).await.0, StatusCode::CONFLICT);
    let invalid_plan = json!({"request_id":Uuid::new_v4(),"question":question,"parsed_request":found["parsed_request"],"search_id":found["parse_id"]});
    assert_eq!(intake.send(&person.bearer, "/v2/ask/research", Some(invalid_plan)).await.0, StatusCode::CONFLICT, "a parse receipt grants no source preparation authority");
    let request = json!({"request_id":Uuid::new_v4(),"question":question,"parsed_request":fresh["parsed_request"],"search_id":fresh["search_id"],"discovery_id":fresh["discovery_id"]});
    let (status, planned) = intake.send(&person.bearer, "/v2/ask/research", Some(request.clone())).await;
    assert_eq!(status, StatusCode::OK, "{planned}");
    assert_eq!(planned["context_view"]["manual_approval_required"], true);
    let task: Uuid = planned["state"]["state_ref"].as_str().unwrap().parse().unwrap();
    assert_no_purchase(&intake, &person, task).await;
    let (status, replay) = intake.send(&person.bearer, "/v2/ask/research", Some(request)).await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay["state"]["state_ref"], planned["state"]["state_ref"]);
    assert_eq!(intake.model.ask_v2_requests.lock().unwrap().len(), parses);
    assert!(intake.edge.calls.lock().unwrap().is_empty());
}
