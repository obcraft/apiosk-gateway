use super::*;

fn person() -> Person {
    Person {
        owner: "00000000-0000-4000-8000-000000000001".parse().unwrap(),
        bearer: "apk_access_local_fixture".into(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn connected_ask_lookup_is_reviewed_and_quoted_with_its_selected_source() {
    let intake = Intake::start().await;
    let person = person();
    let question = "Jaarrekening van Coolblue";
    let (_, found) = intake
        .send(
            &person.bearer,
            "/v2/ask",
            Some(json!({"question":question})),
        )
        .await;
    let option = found["source_options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["source"]["capability"] == "company.accounts")
        .unwrap();
    assert_eq!(option["endpoint"]["lookup"]["field"], "company.name");
    assert_eq!(option["endpoint"]["missing_inputs"], json!([]));
    assert_eq!(
        option["endpoint"]["input_values"],
        json!({"company.name":"Coolblue"})
    );
    let request = json!({"request_id":Uuid::new_v4(),"question":question,"parsed_request":found["parsed_request"],"search_id":found["search_id"],
        "selections":[{"endpoint_id":option["source"]["endpoint_id"],"capability":option["source"]["capability"],"subject_id":option["subject_id"],"input":option["endpoint"]["input_values"]}]});
    let (status, quote) = intake
        .send(&person.bearer, "/v2/ask/prepare", Some(request.clone()))
        .await;
    assert_eq!(status, StatusCode::OK, "{quote}");
    assert_eq!(quote["status"], "ready", "{quote}");
    assert_eq!(
        quote["proposal"]["steps"],
        json!(["company.search", "company.accounts"])
    );
    assert_eq!(
        quote["proposal"]["max_total_atomic"], "138",
        "one price includes the native lookup and selected accounts"
    );
    assert_eq!(quote["context_view"]["manual_approval_required"], true);
    let task: Uuid = quote["state"]["state_ref"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_no_purchase(&intake, &person, task).await;
    assert_eq!(
        intake
            .send(&person.bearer, "/v2/ask/prepare", Some(request))
            .await
            .1,
        quote
    );
    sqlx::query("UPDATE tasks SET document=jsonb_set(document,'{analysis}',$2) WHERE id=$1")
        .bind(task)
        .bind(json!({"company_context":[]}))
        .execute(&intake.pool)
        .await
        .unwrap();
    let (status, approved) = intake
        .send(
            &person.bearer,
            "/v2/approve",
            Some(json!({"request_id":Uuid::new_v4(),"state":quote["state"],
        "quote_ref":quote["proposal"]["quote_ref"],"max_total_atomic":"138"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{approved}");
    let path = format!("/v2/tasks/{task}");
    let saved = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let (_, saved) = intake.send(&person.bearer, &path, None).await;
            if saved["context_view"]["worker_active"] != true
                && matches!(
                    saved["status"].as_str(),
                    Some("succeeded" | "partial" | "failed")
                )
            {
                break saved;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        saved["billing"]["total_charged"], "138",
        "both approved source steps settle once"
    );
    let calls = intake.edge.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["inputs"]["company.name"], "Coolblue");
    assert_eq!(
        calls[1]["inputs"]["company.id"], "123",
        "dependent call uses returned identity"
    );
    assert_eq!(intake.model.ask_v2_requests.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn connected_ask_search_research_reuses_signed_parse_and_never_approves() {
    let intake = Intake::start().await;
    let person = person();
    let question = "Jaarrekening van Coolblue";
    let (status, found) = intake
        .send(
            &person.bearer,
            "/v2/ask",
            Some(json!({"question":question})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert_eq!(found["context_status"], "complete");
    assert!(found["company_context"].is_array());
    let parses = intake.model.ask_v2_requests.lock().unwrap().len();
    let refresh = json!({"question":question,"parsed_request":found["parsed_request"],"search_id":found["search_id"]});
    let (status, fresh) = intake
        .send(&person.bearer, "/v2/ask/search", Some(refresh.clone()))
        .await;
    assert_eq!(status, StatusCode::OK, "{fresh}");
    assert_eq!(fresh["parsed_request"], found["parsed_request"]);
    assert_eq!(fresh["attempts"], 0);
    assert_eq!(
        intake.model.ask_v2_requests.lock().unwrap().len(),
        parses,
        "retry uses the signed parse"
    );
    let request = json!({"request_id":Uuid::new_v4(),"question":question,"parsed_request":fresh["parsed_request"],"search_id":fresh["search_id"]});
    for path in ["/v2/ask/research", "/v2/ask/search"] {
        let body = if path.ends_with("research") {
            request.clone()
        } else {
            refresh.clone()
        };
        assert_eq!(
            intake
                .send(ORGANISATION_KEY, path, Some(body.clone()))
                .await
                .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            intake.send(&Person::new().bearer, path, Some(body)).await.0,
            StatusCode::FORBIDDEN
        );
    }
    let mut changed = request.clone();
    changed["parsed_request"]["subjects"][0]["label"] = json!("Another company");
    assert_eq!(
        intake
            .send(&person.bearer, "/v2/ask/research", Some(changed))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let before = task_count(&intake, &person).await;
    let (status, quoted) = intake
        .send(&person.bearer, "/v2/ask/research", Some(request.clone()))
        .await;
    assert_eq!(status, StatusCode::OK, "{quoted}");
    assert_eq!(quoted["status"], "needs_input", "{quoted}");
    assert_eq!(
        intake
            .send(&person.bearer, "/v2/ask/research", Some(request.clone()))
            .await
            .1,
        quoted,
        "unpaid research receipt replays exactly"
    );
    let action = quoted["next_actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["kind"] == "supply_input")
        .unwrap();
    let (status, quoted) = intake
        .send(
            &person.bearer,
            "/v2/execute",
            Some(
                json!({"request_id":Uuid::new_v4(),"idempotency_key":Uuid::new_v4(),
        "action_id":action["action_id"],"state":quoted["state"],"input":{"value":"00123456"}}),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{quoted}");
    assert_eq!(quoted["status"], "ready", "{quoted}");
    assert!(quoted["proposal"].is_object());
    assert_eq!(quoted["context_view"]["manual_approval_required"], true);
    assert_eq!(quoted["billing"]["authorization_active"], false);
    assert_eq!(task_count(&intake, &person).await, before + 1);
    assert_eq!(
        intake.model.ask_v2_requests.lock().unwrap().len(),
        parses,
        "research never interprets the question again"
    );
    let task: Uuid = quoted["state"]["state_ref"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_no_purchase(&intake, &person, task).await;
    let mut response: apiosk_gateway_v2::protocol::Response =
        serde_json::from_value(quoted.clone()).unwrap();
    response.context_view["auto_approval"] = json!({"active":true});
    let principal = Principal {
        owner: person.owner,
        connection: Some("00000000-0000-4000-8000-000000000002".parse().unwrap()),
        workspace: None,
    };
    let unchanged = intake
        .engine
        .continue_automatically(&principal, &person.bearer, response)
        .await;
    assert_eq!(
        unchanged.billing.unwrap()["authorization_active"],
        false,
        "a standing rule cannot buy this reviewed plan"
    );
    sqlx::query(
        "UPDATE tasks SET document=jsonb_set(document,'{proposal,expires_at}',$2) WHERE id=$1",
    )
    .bind(task)
    .bind(json!(chrono::Utc::now() - chrono::Duration::minutes(1)))
    .execute(&intake.pool)
    .await
    .unwrap();
    let (status,repriced)=intake.send(&person.bearer,"/v2/ask/requote",Some(json!({"request_id":Uuid::new_v4(),"state":quoted["state"],"quote_ref":quoted["proposal"]["quote_ref"]}))).await;
    assert_eq!(status, StatusCode::OK, "{repriced}");
    assert_ne!(
        repriced["proposal"]["quote_ref"],
        quoted["proposal"]["quote_ref"]
    );
    assert_eq!(repriced["context_view"]["manual_approval_required"], true);
    assert_no_purchase(&intake, &person, task).await;
    sqlx::query("UPDATE api_endpoints SET price=price+0.0001 WHERE id=$1")
        .bind(endpoint("company.accounts").parse::<Uuid>().unwrap())
        .execute(&intake.pool)
        .await
        .unwrap();
    intake.engine.catalog.fresh().await.unwrap();
    let mut stale = request.clone();
    stale["request_id"] = json!(Uuid::new_v4());
    assert_eq!(
        intake
            .send(&person.bearer, "/v2/ask/research", Some(stale))
            .await
            .0,
        StatusCode::CONFLICT,
        "a changed catalog cannot quote a stale search"
    );
    let (status, recovered) = intake
        .send(&person.bearer, "/v2/ask/research", Some(request))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        recovered["state"]["state_ref"],
        task.to_string(),
        "existing receipt recovers before freshness checks"
    );
    let (status, retried) = intake
        .send(&person.bearer, "/v2/ask/search", Some(refresh))
        .await;
    assert_eq!(status, StatusCode::OK, "{retried}");
    assert_ne!(
        retried["searches"]["apiosk"]["catalog_version"],
        fresh["searches"]["apiosk"]["catalog_version"]
    );
    assert_eq!(intake.model.ask_v2_requests.lock().unwrap().len(), parses);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn connected_ask_conversation_reads_saved_results_once_and_is_connection_scoped() {
    let intake = Intake::start().await;
    let person = person();
    let question = "Jaarrekening van Coolblue";
    let (_, found) = intake
        .send(
            &person.bearer,
            "/v2/ask",
            Some(json!({"question":question})),
        )
        .await;
    let (_,quoted)=intake.send(&person.bearer,"/v2/ask/prepare",Some(json!({"request_id":Uuid::new_v4(),"question":question,
        "parsed_request":found["parsed_request"],"search_id":found["search_id"],"selections":[{"endpoint_id":endpoint("company.accounts"),
        "capability":"company.accounts","subject_id":"s1","input":{"company.id":"00123456"}}]}))).await;
    let task: Uuid = quoted["state"]["state_ref"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    sqlx::query("UPDATE tasks SET document=jsonb_set(document,'{analysis}',$2) WHERE id=$1")
        .bind(task)
        .bind(json!({"company_context":[]}))
        .execute(&intake.pool)
        .await
        .unwrap();
    let (status,approved)=intake.send(&person.bearer,"/v2/approve",Some(json!({"request_id":Uuid::new_v4(),"state":quoted["state"],
        "quote_ref":quoted["proposal"]["quote_ref"],"max_total_atomic":quoted["proposal"]["max_total_atomic"]}))).await;
    assert_eq!(status, StatusCode::OK, "{approved}");
    let path = format!("/v2/tasks/{task}");
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let (_, saved) = intake.send(&person.bearer, &path, None).await;
            if saved["context_view"]["worker_active"] != true
                && matches!(
                    saved["status"].as_str(),
                    Some("succeeded" | "partial" | "failed")
                )
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let calls = intake.edge.calls.lock().unwrap().len();
    assert_eq!(calls, 1);
    let before_charges: i64 =
        sqlx::query_scalar("SELECT count(*) FROM balance_charges WHERE user_id=$1")
            .bind(person.owner)
            .fetch_one(&intake.pool)
            .await
            .unwrap();
    intake
        .model
        .conversation_routes
        .lock()
        .unwrap()
        .push_back(json!({"kind":"saved_answer","outputs":[0]}));
    let request = json!({"request_id":Uuid::new_v4(),"question":"Summarize the saved accounts","task_refs":[task]});
    assert_eq!(intake.send(ORGANISATION_KEY,"/v2/ask/conversation",Some(json!({"request_id":Uuid::new_v4(),"question":request["question"],"task_refs":[task]}))).await.1,
        json!({"kind":"search"}),"a different scoped connection cannot read these results");
    let (status, routed) = intake
        .send(
            &person.bearer,
            "/v2/ask/conversation",
            Some(request.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{routed}");
    assert_eq!(routed, json!({"kind":"saved_answer","task_refs":[task]}));
    assert_eq!(
        intake
            .send(
                &person.bearer,
                "/v2/ask/conversation",
                Some(request.clone())
            )
            .await
            .1,
        routed,
        "retry returns the saved route, even while analysis runs"
    );
    let document: Value = sqlx::query_scalar("SELECT document FROM tasks WHERE id=$1")
        .bind(task)
        .fetch_one(&intake.pool)
        .await
        .unwrap();
    assert_eq!(
        document["conversation"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|turn| turn["question"] == request["question"])
            .count(),
        1
    );
    assert_eq!(intake.edge.calls.lock().unwrap().len(), calls);
    let charges: i64 = sqlx::query_scalar("SELECT count(*) FROM balance_charges WHERE user_id=$1")
        .bind(person.owner)
        .fetch_one(&intake.pool)
        .await
        .unwrap();
    assert_eq!(
        charges, before_charges,
        "saved-only follow-up never buys data"
    );
    let mut conflict = request.clone();
    conflict["question"] = json!("Different follow-up");
    assert_eq!(
        intake
            .send(&person.bearer, "/v2/ask/conversation", Some(conflict))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let unrelated = intake
        .engine
        .store
        .create(
            person.owner,
            Some("00000000-0000-4000-8000-000000000003".parse().unwrap()),
        )
        .await
        .unwrap();
    let (_, reply) = intake
        .send(
            &person.bearer,
            "/v2/ask/conversation",
            Some(
                json!({"request_id":Uuid::new_v4(),"question":"Read this","task_refs":[unrelated]}),
            ),
        )
        .await;
    assert_eq!(
        reply,
        json!({"kind":"search"}),
        "another connection cannot expose its saved outputs"
    );
}
