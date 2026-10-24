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
    assert_eq!(replay, quote, "the receipt already contains the manual policy and final revision");
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn connected_endpoint_concurrent_replay_and_legacy_receipts_cannot_downgrade_policy() {
    let intake = Intake::start().await;
    let person = Person { owner:"00000000-0000-4000-8000-000000000001".parse().unwrap(), bearer:"apk_access_local_fixture".into() };
    let body = json!({"request_id":Uuid::new_v4(),"endpoint_id":endpoint("company.accounts"),
        "capability":"company.accounts","input":{"company.id":"manual-concurrency"}});
    let before = task_count(&intake, &person).await;
    let (first, second) = tokio::join!(
        intake.send(&person.bearer, "/v2/ask/source/prepare", Some(body.clone())),
        intake.send(&person.bearer, "/v2/ask/source/prepare", Some(body.clone())),
    );
    for (status, response) in [first, second] {
        assert_eq!(status, StatusCode::OK, "concurrent replay cannot fail on a policy update: {response}");
        if response["state"].is_object() { assert_eq!(response["context_view"]["manual_approval_required"], true); }
        else { assert!(codes(&response).contains(&"request_pending"), "{response}"); }
    }
    let (_, quote) = intake.send(&person.bearer, "/v2/ask/source/prepare", Some(body.clone())).await;
    let (_, replay) = intake.send(&person.bearer, "/v2/ask/source/prepare", Some(body.clone())).await;
    assert_eq!(quote, replay);
    assert_eq!(task_count(&intake, &person).await, before + 1);
    let request: Uuid = body["request_id"].as_str().unwrap().parse().unwrap();
    let receipt: Value = sqlx::query_scalar("SELECT response FROM requests WHERE owner_id=$1 AND request_id=$2")
        .bind(person.owner).bind(request).fetch_one(&intake.pool).await.unwrap();
    assert_eq!(receipt["context_view"]["manual_approval_required"], true);
    let (_, conflict) = intake.send(&person.bearer, "/v2/ask-v2/prepare", Some(body.clone())).await;
    assert!(codes(&conflict).contains(&"request_conflict"), "the App cannot replay a manual receipt under its standing-rule policy");

    let mut legacy = body; legacy["request_id"] = json!(Uuid::new_v4());
    let (_, old_quote) = intake.send(&person.bearer, "/v2/ask-v2/prepare", Some(legacy.clone())).await;
    assert_ne!(old_quote["context_view"]["manual_approval_required"], true, "existing App preparation keeps its default policy");
    let (_, conflict) = intake.send(&person.bearer, "/v2/ask/source/prepare", Some(legacy)).await;
    assert!(codes(&conflict).contains(&"request_conflict"), "manual preparation cannot adopt an old nonmanual receipt");

    let task: Uuid = quote["state"]["state_ref"].as_str().unwrap().parse().unwrap();
    sqlx::query("UPDATE tasks SET document=jsonb_set(document,'{proposal,expires_at}',$2) WHERE id=$1")
        .bind(task).bind(json!(chrono::Utc::now()-chrono::Duration::minutes(1))).execute(&intake.pool).await.unwrap();
    let (_, repriced) = intake.send(&person.bearer, "/v2/ask/requote", Some(json!({
        "request_id":Uuid::new_v4(),"state":quote["state"],"quote_ref":quote["proposal"]["quote_ref"]
    }))).await;
    assert_eq!(repriced["context_view"]["manual_approval_required"], true, "requote keeps the policy");
    let stored: Value = sqlx::query_scalar("SELECT document FROM tasks WHERE id=$1").bind(task).fetch_one(&intake.pool).await.unwrap();
    let mut intent: apiosk_gateway_v2::parser::Intent = serde_json::from_value(stored["intent"].clone()).unwrap();
    intent.subjects[0].values.insert("company.id".into(), json!("changed-manual-input"));
    let principal = Principal { owner:person.owner, connection:Some("00000000-0000-4000-8000-000000000002".parse().unwrap()), workspace:None };
    let continued = intake.engine.discover_recipe(&principal, DiscoverRequest {
        request_id:Uuid::new_v4(), question:"Continue the selected company accounts with edited inputs".into(),
        state:Some(serde_json::from_value(repriced["state"].clone()).unwrap()), context_delta:Default::default(),
    }, intent).await.unwrap();
    assert_eq!(continued.context_view["manual_approval_required"], true, "ordinary connected intake cannot drop the stored manual policy");
    assert_no_purchase(&intake, &person, task).await;
    assert!(intake.edge.calls.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn connected_endpoint_interrupted_receipt_recovers_a_durable_manual_quote() {
    let intake = Intake::start().await;
    let person = Person { owner:"00000000-0000-4000-8000-000000000001".parse().unwrap(), bearer:"apk_access_local_fixture".into() };
    let request = Uuid::new_v4();
    // Enforce the first durable write as well, before any intent/quote update.
    sqlx::query("CREATE FUNCTION public.fixture_manual_initial_policy() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.document->>'manual_approval_required' IS DISTINCT FROM 'true' THEN RAISE EXCEPTION 'fixture initial policy missing'; END IF; RETURN NEW; END $$")
        .execute(&intake.pool).await.unwrap();
    sqlx::query("CREATE TRIGGER fixture_manual_initial_policy BEFORE INSERT ON tasks FOR EACH ROW EXECUTE FUNCTION public.fixture_manual_initial_policy()")
        .execute(&intake.pool).await.unwrap();
    // Inject a failure after the quote is saved but before its response receipt
    // can be persisted. This is the process-loss boundary the adapter must cover.
    sqlx::query(&format!("CREATE FUNCTION public.fixture_manual_receipt_interruption() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.request_id='{request}'::uuid AND NEW.response IS NOT NULL THEN RAISE EXCEPTION 'fixture receipt interrupted'; END IF; RETURN NEW; END $$"))
        .execute(&intake.pool).await.unwrap();
    sqlx::query("CREATE TRIGGER fixture_manual_receipt_interruption BEFORE UPDATE ON requests FOR EACH ROW EXECUTE FUNCTION public.fixture_manual_receipt_interruption()")
        .execute(&intake.pool).await.unwrap();
    let (status, response) = intake.send(&person.bearer, "/v2/ask/source/prepare", Some(json!({
        "request_id":request,"endpoint_id":endpoint("company.accounts"),"capability":"company.accounts","input":{"company.id":"manual-interrupted"}}
    ))).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    sqlx::query("DROP TRIGGER fixture_manual_receipt_interruption ON requests").execute(&intake.pool).await.unwrap();
    sqlx::query("DROP FUNCTION public.fixture_manual_receipt_interruption()").execute(&intake.pool).await.unwrap();
    sqlx::query("DROP TRIGGER fixture_manual_initial_policy ON tasks").execute(&intake.pool).await.unwrap();
    sqlx::query("DROP FUNCTION public.fixture_manual_initial_policy()").execute(&intake.pool).await.unwrap();
    let row = sqlx::query("SELECT task_id,response FROM requests WHERE owner_id=$1 AND request_id=$2")
        .bind(person.owner).bind(request).fetch_one(&intake.pool).await.unwrap();
    assert!(row.get::<Option<Value>, _>("response").is_none(), "the receipt failure was injected");
    let task: Uuid = row.get("task_id");
    let document: Value = sqlx::query_scalar("SELECT document FROM tasks WHERE id=$1").bind(task).fetch_one(&intake.pool).await.unwrap();
    assert_eq!(document["manual_approval_required"], true);
    assert!(document["proposal"].is_object());
    let (_, mut recovered) = intake.send(&person.bearer, &format!("/v2/tasks/{task}"), None).await;
    assert_eq!(recovered["context_view"]["manual_approval_required"], true);
    recovered["context_view"]["auto_approval"] = json!({"active":true});
    let principal = Principal { owner:person.owner, connection:Some("00000000-0000-4000-8000-000000000002".parse().unwrap()), workspace:None };
    let safe = intake.engine.continue_automatically(&principal, &person.bearer, serde_json::from_value(recovered).unwrap()).await;
    assert_eq!(safe.billing.as_ref().unwrap()["authorization_active"], false);
    assert_no_purchase(&intake, &person, task).await;
    assert!(intake.edge.calls.lock().unwrap().is_empty());
}
