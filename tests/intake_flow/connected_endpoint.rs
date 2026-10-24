use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn connected_endpoint_quote_matches_app_and_stays_manual_on_retry_and_status() {
    let intake = Intake::start().await;
    let person = Person { owner:"00000000-0000-4000-8000-000000000001".parse().unwrap(), bearer:"apk_access_local_fixture".into() };
    let endpoint = endpoint("company.accounts");
    let body = json!({"request_id":Uuid::new_v4(),"endpoint_id":endpoint,"capability":"company.accounts",
        "input":{"company.id":"00123456"},"question":"Show the company accounts for company 00123456."});
    let path = "/v2/ask/source/prepare";
    assert_eq!(intake.send("invalid-fixture-bearer", path, Some(body.clone())).await.0, StatusCode::UNAUTHORIZED);
    let read_only = format!("apk_org_live_{}", "b".repeat(48));
    let (status, refused) = intake.send(&read_only, path, Some(body.clone())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(refused["error"]["required_scope"], "ask:create");
    let app_person = Person::new();
    let (status, refused) = intake.send(&app_person.bearer, path, Some(body.clone())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(refused["error"]["code"], "connection_required");

    let (status, quote) = intake.send(&person.bearer, path, Some(body.clone())).await;
    assert_eq!(status, StatusCode::OK, "{quote}");
    assert_eq!(quote["status"], "ready", "{quote}");
    assert_eq!(quote["context_view"]["manual_approval_required"], true);
    assert_eq!(quote["proposal"]["max_total_atomic"], "115");
    assert_eq!(quote["proposal"]["steps"], json!(["company.accounts"]));
    let task: Uuid = quote["state"]["state_ref"].as_str().unwrap().parse().unwrap();
    let saved: Value = sqlx::query_scalar("SELECT document FROM tasks WHERE id=$1").bind(task).fetch_one(&intake.pool).await.unwrap();
    assert_eq!(saved["manual_approval_required"], true);
    assert_eq!(saved["intent"]["subjects"][0]["values"]["company.id"], "00123456");
    assert_eq!(saved["proposal"]["steps"][0]["service"]["endpoint_id"], endpoint);

    let mut app_body = body.clone(); app_body["request_id"] = json!(Uuid::new_v4());
    let (_, app_quote) = intake.send(&app_person.bearer, "/v2/ask-v2/prepare", Some(app_body)).await;
    assert_eq!(quote["proposal"]["max_total_atomic"], app_quote["proposal"]["max_total_atomic"]);
    assert_eq!(quote["proposal"]["steps"], app_quote["proposal"]["steps"]);
    let before = task_count(&intake, &person).await;
    let (_, replay) = intake.send(&person.bearer, path, Some(body.clone())).await;
    assert_eq!(replay["state"]["state_ref"], quote["state"]["state_ref"]);
    assert_eq!(replay["state"]["revision"], quote["state"]["revision"]);
    assert_eq!(replay["proposal"], quote["proposal"]);
    assert_eq!(replay["context_view"]["manual_approval_required"], true);
    assert_eq!(task_count(&intake, &person).await, before);

    // Even an active standing policy cannot auto-run this persisted quote.
    let principal = Principal { owner:person.owner, connection:Some("00000000-0000-4000-8000-000000000002".parse().unwrap()), workspace:None };
    for mut view in [quote.clone(), replay] {
        view["context_view"]["auto_approval"] = json!({"active":true});
        let response = intake.engine.continue_automatically(&principal, &person.bearer, serde_json::from_value(view).unwrap()).await;
        assert_eq!(response.billing.as_ref().unwrap()["authorization_active"], false);
    }
    for _ in 0..2 {
        let (status, saved) = intake.send(&person.bearer, &format!("/v2/tasks/{task}"), None).await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        assert_eq!(saved["context_view"]["manual_approval_required"], true);
        let (status, _) = intake.send(&person.bearer, &format!("/v2/asks/{task}"), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_no_purchase(&intake, &person, task).await;
    }
    let mut changed = body.clone(); changed["input"]["company.id"] = json!("another-company");
    let (_, conflict) = intake.send(&person.bearer, path, Some(changed)).await;
    assert!(codes(&conflict).contains(&"request_conflict"), "{conflict}");
    // Existing public prepare accepts organisation connections with ask:create.
    let mut org_body = body; org_body["request_id"] = json!(Uuid::new_v4());
    let (status, org_quote) = intake.send(ORGANISATION_KEY, path, Some(org_body)).await;
    assert_eq!(status, StatusCode::OK, "{org_quote}");
    assert_eq!(org_quote["context_view"]["manual_approval_required"], true);
    let org_task: Uuid = org_quote["state"]["state_ref"].as_str().unwrap().parse().unwrap();
    assert_no_purchase(&intake, &person, org_task).await;
    assert!(intake.model.stages.lock().unwrap().is_empty(), "endpoint preparation never invokes a planner model");
    assert!(intake.model.ask_v2_requests.lock().unwrap().is_empty());
    assert!(intake.edge.calls.lock().unwrap().is_empty());
}
