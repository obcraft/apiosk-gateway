use super::*;

/// A local settled directory; no request reaches the selected HTTPS provider.
async fn external_intake() -> (Intake, Arc<std::sync::atomic::AtomicUsize>) {
    use apiosk_gateway_v2::compat::CommerceService;
    let mut intake = Intake::start().await;
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let directory = Router::new().route("/v1/sources/coinbase-x402-bazaar/apis/fixture.example", get({
        let reads = reads.clone();
        move || { let reads = reads.clone(); async move {
            reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Json(json!({"source":{"id":"coinbase-x402-bazaar"},"endpoints":[{
                "resource":"https://fixture.example/data","method":"GET","provider":"Fixture source","description":"Synthetic data",
                "network":"eip155:8453","settlement":"apiosk","price_usd":0.0001,"buyer_price_usd":0.000115,
                "input_schema":{"type":"object","properties":{"method":{"type":"string","const":"GET"},
                    "queryParams":{"type":"object","properties":{},"required":[]}}}
            }]}))
        }}
    }));
    let app = router(
        ServerState {
            engine: intake.engine.clone(),
            auth: Arc::new(Auth {
                pool: intake.pool.clone(),
                http: intake.http.clone(),
                supabase_url: intake.engine.executor.supabase_url.clone(),
                publishable_key: "fixture".into(),
            }),
            permits: Arc::new(tokio::sync::Semaphore::new(16)),
            commerce: Some(
                CommerceService::new(directory),
            ),
        },
        "http://localhost:5173",
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    intake.gateway = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (intake, reads)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn connected_external_review_prepare_and_status_never_buy_then_exact_approval_runs_once() {
    use apiosk_gateway_v2::ask_v2::{external, purchase};
    let (intake, reads) = external_intake().await;
    let person = Person {
        owner: "00000000-0000-4000-8000-000000000001".parse().unwrap(),
        bearer: "apk_access_local_fixture".into(),
    };
    let api_id = external::selection_id("https://fixture.example/data", "GET", "company data");
    let before = task_count(&intake, &person).await;
    let (status, offer) = intake
        .send(
            &person.bearer,
            "/v2/ask/external/source",
            Some(json!({"api_id":api_id})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{offer}");
    assert_eq!(offer["total_atomic"], 115);
    assert_eq!(task_count(&intake, &person).await, before);
    let id = Uuid::new_v4();
    let request = json!({"request_id":id,"api_id":api_id,"input":{}});
    let (status, quoted) = intake
        .send(
            &person.bearer,
            "/v2/ask/external/prepare",
            Some(request.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{quoted}");
    assert_eq!(quoted["status"], "requires_approval");
    assert_eq!(quoted["quote"]["max_total_atomic"], "115");
    let path = format!("/v2/ask/external/{id}");
    assert_eq!(intake.send(&person.bearer, &path, None).await.1, quoted);
    assert_eq!(
        intake
            .send(&person.bearer, "/v2/ask/external/prepare", Some(request))
            .await
            .1,
        quoted
    );
    assert_eq!(
        reads.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "inspect and first preparation only; recovery never re-fetches directory"
    );
    assert!(intake.edge.calls.lock().unwrap().is_empty());
    let saved: Value = sqlx::query_scalar("SELECT document FROM tasks WHERE id=$1")
        .bind(id)
        .fetch_one(&intake.pool)
        .await
        .unwrap();
    assert!(saved["external_purchase"]["approved_at"].is_null());
    let wrong = Principal {
        owner: Uuid::new_v4(),
        connection: Some(Uuid::new_v4()),
        workspace: None,
    };
    assert!(
        purchase::view(&intake.engine, &wrong, id).await.is_err(),
        "another owner cannot recover this purchase"
    );
    let wrong_connection = Principal {
        owner: person.owner,
        connection: Some(Uuid::new_v4()),
        workspace: None,
    };
    assert!(
        purchase::view(&intake.engine, &wrong_connection, id)
            .await
            .is_err(),
        "even the same owner cannot cross connections"
    );
    assert_eq!(
        intake.send(ORGANISATION_KEY, &path, None).await.0,
        StatusCode::NOT_FOUND
    );
    let approval = json!({"quote_ref":quoted["quote"]["quote_ref"],"max_total_atomic":"115"});
    assert_eq!(
        intake
            .send(
                ORGANISATION_KEY,
                &format!("{path}/authorize"),
                Some(approval.clone())
            )
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let mut wrong_quote = approval.clone();
    wrong_quote["max_total_atomic"] = json!("114");
    assert_eq!(
        intake
            .send(
                &person.bearer,
                &format!("{path}/authorize"),
                Some(wrong_quote)
            )
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert!(intake.edge.calls.lock().unwrap().is_empty());
    let principal = Principal {
        owner: person.owner,
        connection: Some("00000000-0000-4000-8000-000000000002".parse().unwrap()),
        workspace: None,
    };
    let quote_ref: Uuid = quoted["quote"]["quote_ref"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(purchase::authorize(
        intake.engine.clone(),
        wrong,
        person.bearer.clone(),
        id,
        quote_ref,
        "115"
    )
    .await
    .is_err());
    assert!(purchase::authorize(
        intake.engine.clone(),
        wrong_connection,
        person.bearer.clone(),
        id,
        quote_ref,
        "115"
    )
    .await
    .is_err());
    assert_eq!(
        purchase::view(&intake.engine, &principal, id)
            .await
            .unwrap()["status"],
        "requires_approval"
    );
    let balance = external_balance(&intake, &person).await;
    let approval_path = format!("{path}/authorize");
    let (first, second) = tokio::join!(
        intake.send(&person.bearer, &approval_path, Some(approval.clone())),
        intake.send(&person.bearer, &approval_path, Some(approval.clone()))
    );
    assert_eq!(first.0, StatusCode::OK, "{}", first.1);
    assert_eq!(second.0, StatusCode::OK, "{}", second.1);
    let finished = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (status, view) = intake.send(&person.bearer, &path, None).await;
            assert_eq!(status, StatusCode::OK, "{view}");
            if view["status"] == "succeeded" {
                break view;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(finished["result_present"], true);
    assert_eq!(finished["billing"]["charged_micro_usd"], 115);
    assert_eq!(external_balance(&intake, &person).await, balance - 115);
    for _ in 0..3 {
        assert_eq!(intake.send(&person.bearer, &path, None).await.1, finished);
    }
    assert_eq!(
        intake
            .send(&person.bearer, &approval_path, Some(approval))
            .await
            .1,
        finished
    );
    assert_eq!(
        intake.edge.calls.lock().unwrap().len(),
        1,
        "status and repeated approval never redispatch"
    );
    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(intake.model.stages.lock().unwrap().is_empty());
}
