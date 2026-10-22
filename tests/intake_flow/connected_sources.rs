use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated support-db (scripts/test-intake-flow.mjs); no live database"]
async fn connected_selected_sources_one_combined_quote_manual_requote_and_one_approval() {
    let intake=Intake::start().await;
    let person=Person{owner:"00000000-0000-4000-8000-000000000001".parse().unwrap(),bearer:"apk_access_local_fixture".into()};
    let first:Uuid=endpoint("company.accounts").parse().unwrap();
    let second=Uuid::new_v4();let api=Uuid::new_v4();
    sqlx::query("INSERT INTO apis(id,slug,name,listing_type,status,active,is_verified,discoverable,description) VALUES($1,$2,'Second accounts provider','api','active',true,true,true,'Company annual accounts')")
        .bind(api).bind(format!("accounts-{api}")).execute(&intake.pool).await.unwrap();
    sqlx::query("INSERT INTO api_endpoints(id,api_id,method,path,price,active,description) VALUES($1,$2,'GET','/accounts',0.0002,true,'Company annual accounts')")
        .bind(second).bind(api).execute(&intake.pool).await.unwrap();
    sqlx::query("INSERT INTO capability_execution_contracts(id,endpoint_id,capability_slug,version,status,requires,input_map,output_map,identifies,cardinality,max_fan_out,fixed_params,preconditions,pagination,freshness_seconds,sample_payload,validated_at,effect,price_model) SELECT $1,$2,capability_slug,version,status,requires,input_map,output_map,identifies,cardinality,max_fan_out,fixed_params,preconditions,pagination,freshness_seconds,sample_payload,validated_at,effect,price_model FROM capability_execution_contracts WHERE endpoint_id=$3")
        .bind(Uuid::new_v4()).bind(second).bind(first).execute(&intake.pool).await.unwrap();
    let question="Jaarrekening van Coolblue";
    intake.model.ask_v2_replies.lock().unwrap().push_back(ask_v2_capabilities());
    let (status,found)=intake.send(&person.bearer,"/v2/ask",Some(json!({"question":question}))).await;
    assert_eq!(status,StatusCode::OK,"{found}");
    assert!(found["search_id"].is_string(),"{found}");
    for id in [first,second] {assert!(found["source_options"].as_array().unwrap().iter().any(|option|option["source"]["endpoint_id"]==id.to_string()),"{found}");}
    let calls=intake.model.model_http_requests.load(std::sync::atomic::Ordering::Relaxed);
    let selections=json!([
        {"endpoint_id":first,"capability":"company.accounts","subject_id":"s1","input":{"company.id":"00123456"}},
        {"endpoint_id":second,"capability":"company.accounts","subject_id":"s1","input":{"company.id":"00123456"}}
    ]);
    let request=json!({"request_id":Uuid::new_v4(),"question":question,"parsed_request":found["parsed_request"],"search_id":found["search_id"],"selections":selections});
    let before=task_count(&intake,&person).await;
    let mut tampered=request.clone();tampered["parsed_request"]["subjects"][0]["label"]=json!("Other company");
    assert_eq!(intake.send(&person.bearer,"/v2/ask/prepare",Some(tampered)).await.0,StatusCode::CONFLICT);
    assert_eq!(intake.send(ORGANISATION_KEY,"/v2/ask/prepare",Some(request.clone())).await.0,StatusCode::CONFLICT,"another connection cannot reuse the signed source choices");
    assert_eq!(task_count(&intake,&person).await,before);
    for invalid in [json!([]),json!([selections[0].clone(),selections[0].clone()]),json!([{"endpoint_id":Uuid::new_v4(),"capability":"company.accounts","input":{}}])] {
        let mut body=request.clone();body["request_id"]=json!(Uuid::new_v4());body["selections"]=invalid;
        let (_,rejected)=intake.send(&person.bearer,"/v2/ask/prepare",Some(body)).await;
        assert!(rejected["proposal"].is_null(),"{rejected}");assert!(rejected["state"].is_null(),"{rejected}");
    }
    assert_eq!(task_count(&intake,&person).await,before);
    let (status,quote)=intake.send(&person.bearer,"/v2/ask/prepare",Some(request.clone())).await;
    assert_eq!(status,StatusCode::OK,"{quote}");assert_eq!(quote["status"],"ready","{quote}");
    assert_eq!(quote["proposal"]["steps"],json!(["company.accounts","company.accounts"]));
    assert_eq!(quote["proposal"]["max_total_atomic"],"345","both explicit sources are in the one price");
    assert_eq!(quote["context_view"]["selected_sources"],selections);
    assert_eq!(quote["context_view"]["source_selection"]["can_choose"],true);
    assert_eq!(quote["context_view"]["manual_approval_required"],true);
    let mut auto_response:apiosk_gateway_v2::protocol::Response=serde_json::from_value(quote.clone()).unwrap();
    auto_response.context_view["auto_approval"]=json!({"active":true});
    let principal=Principal{owner:person.owner,connection:Some("00000000-0000-4000-8000-000000000002".parse().unwrap()),workspace:None};
    let untouched=intake.engine.continue_automatically(&principal,&person.bearer,auto_response).await;
    assert_eq!(untouched.billing.as_ref().unwrap()["authorization_active"],false,"manual basket never applies an active standing rule");
    assert_eq!(intake.model.model_http_requests.load(std::sync::atomic::Ordering::Relaxed),calls,"selection never reparses");
    assert_eq!(intake.send(&person.bearer,"/v2/ask/prepare",Some(request)).await.1,quote,"retry recovers the exact same quote");
    let task:Uuid=quote["state"]["state_ref"].as_str().unwrap().parse().unwrap();
    assert_no_purchase(&intake,&person,task).await;
    let stored:Value=sqlx::query_scalar("SELECT document FROM tasks WHERE id=$1").bind(task).fetch_one(&intake.pool).await.unwrap();
    assert_eq!(stored["intent"]["subjects"][0]["values"]["company.id"],"00123456","explicit edited literal survives normal planning");
    let endpoints:std::collections::BTreeSet<_>=stored["proposal"]["steps"].as_array().unwrap().iter().map(|step|step["service"]["endpoint_id"].as_str().unwrap().to_owned()).collect();
    assert_eq!(endpoints,std::collections::BTreeSet::from([first.to_string(),second.to_string()]));
    sqlx::query("UPDATE tasks SET document=jsonb_set(document,'{proposal,expires_at}',$2) WHERE id=$1")
        .bind(task).bind(json!(chrono::Utc::now()-chrono::Duration::minutes(1))).execute(&intake.pool).await.unwrap();
    let reprice=json!({"request_id":Uuid::new_v4(),"state":quote["state"],"quote_ref":quote["proposal"]["quote_ref"]});
    let (status,refreshed)=intake.send(&person.bearer,"/v2/ask/requote",Some(reprice.clone())).await;
    assert_eq!(status,StatusCode::OK,"{refreshed}");assert_eq!(refreshed["status"],"ready","{refreshed}");
    assert_ne!(refreshed["proposal"]["quote_ref"],quote["proposal"]["quote_ref"]);
    assert_eq!(refreshed["proposal"]["max_total_atomic"],"345");
    assert_eq!(refreshed["context_view"]["selected_sources"],selections);
    assert_eq!(intake.send(&person.bearer,"/v2/ask/requote",Some(reprice)).await.1,refreshed);
    assert_no_purchase(&intake,&person,task).await;
    assert_eq!(intake.model.model_http_requests.load(std::sync::atomic::Ordering::Relaxed),calls);
    // This route test does not fetch supplemental public websites. The
    // analysis stage reuses this saved empty context snapshot.
    sqlx::query("UPDATE tasks SET document=jsonb_set(document,'{analysis}',$2) WHERE id=$1")
        .bind(task).bind(json!({"company_context":[]})).execute(&intake.pool).await.unwrap();
    let approval=json!({"request_id":Uuid::new_v4(),"state":refreshed["state"],"quote_ref":refreshed["proposal"]["quote_ref"],"max_total_atomic":"345"});
    let (status,approved)=intake.send(&person.bearer,"/v2/approve",Some(approval)).await;
    assert_eq!(status,StatusCode::OK,"{approved}");
    assert_eq!(approved["billing"]["authorization_active"],true,"{approved}");
    assert_eq!(approved["proposal"]["max_total_atomic"],"345");
    let path=format!("/v2/tasks/{task}");
    let mut last=Value::Null;
    let saved=tokio::time::timeout(Duration::from_secs(15),async{loop {
        let (_,saved)=intake.send(&person.bearer,&path,None).await;
        last=saved.clone();
        if saved["context_view"]["worker_active"]!=true && matches!(saved["status"].as_str(),Some("succeeded"|"partial"|"failed")){break saved;}
        tokio::time::sleep(Duration::from_millis(25)).await;
    }}).await.unwrap_or_else(|_|panic!("backend worker did not complete: status={} actions={} errors={} worker={} calls={}",last["status"],last["next_actions"],last["errors"],last["context_view"]["worker_active"],intake.edge.calls.lock().unwrap().len()));
    // This fixture supplies no synthesis; assert the saved source execution
    // and its truthful partial state rather than claim a completed answer.
    assert_eq!(saved["status"],"partial","{saved}");
    assert_eq!(saved["context_view"]["result_refs"].as_array().unwrap().len(),2);
    assert_eq!(saved["billing"]["total_charged"],"345");
    assert_eq!(saved["billing"]["authorization_active"],true);
    assert_eq!(saved["proposal"]["max_total_atomic"],"345");
    assert_eq!(saved["context_view"]["source_selection"]["can_choose"],false);
    assert_eq!(intake.edge.calls.lock().unwrap().len(),2);
    let auths:i64=sqlx::query_scalar("SELECT count(*) FROM authorizations WHERE task_id=$1").bind(task).fetch_one(&intake.pool).await.unwrap();assert_eq!(auths,1);
    for _ in 0..2 {let (_,reloaded)=intake.send(&person.bearer,&path,None).await;assert_eq!(reloaded["billing"],saved["billing"]);}
    assert_eq!(intake.edge.calls.lock().unwrap().len(),2,"reading saved results never purchases again");

    // Legacy connected quotes have the same exact saved repricing contract,
    // even though they predate the source basket metadata.
    let legacy_request=json!({"request_id":Uuid::new_v4(),"question":question,"parsed_request":found["parsed_request"],"search_id":found["search_id"],"selections":[selections[0].clone()]});
    let (_,legacy)=intake.send(&person.bearer,"/v2/ask/prepare",Some(legacy_request)).await;
    assert_eq!(legacy["proposal"]["max_total_atomic"],"115","single source can be chosen independently");
    let legacy_task:Uuid=legacy["state"]["state_ref"].as_str().unwrap().parse().unwrap();
    sqlx::query("UPDATE tasks SET document=jsonb_set(document-'connected_selection','{proposal,expires_at}',$2) WHERE id=$1")
        .bind(legacy_task).bind(json!(chrono::Utc::now()-chrono::Duration::minutes(1))).execute(&intake.pool).await.unwrap();
    let (status,repriced)=intake.send(&person.bearer,"/v2/ask/requote",Some(json!({"request_id":Uuid::new_v4(),"state":legacy["state"],"quote_ref":legacy["proposal"]["quote_ref"]}))).await;
    assert_eq!(status,StatusCode::OK,"{repriced}");assert_eq!(repriced["status"],"ready","{repriced}");
    assert_eq!(repriced["context_view"]["manual_approval_required"],true);
    assert_eq!(repriced["proposal"]["max_total_atomic"],"115");
    let legacy_auths:i64=sqlx::query_scalar("SELECT count(*) FROM authorizations WHERE task_id=$1").bind(legacy_task).fetch_one(&intake.pool).await.unwrap();
    let legacy_calls:i64=sqlx::query_scalar("SELECT count(*) FROM executions WHERE task_id=$1").bind(legacy_task).fetch_one(&intake.pool).await.unwrap();
    assert_eq!((legacy_auths,legacy_calls),(0,0));
    assert_eq!(intake.edge.calls.lock().unwrap().len(),2);
    let before=task_count(&intake,&person).await;
    sqlx::query("UPDATE api_endpoints SET price=price+0.01 WHERE id=$1").bind(second).execute(&intake.pool).await.unwrap();
    intake.engine.catalog.fresh().await.unwrap();
    let (_,stale)=intake.send(&person.bearer,"/v2/ask/prepare",Some(json!({"request_id":Uuid::new_v4(),"question":question,"parsed_request":found["parsed_request"],"search_id":found["search_id"],"selections":selections}))).await;
    assert_eq!(stale["errors"][0]["code"],"source_refresh_required","{stale}");
    assert!(stale["proposal"].is_null());assert_eq!(task_count(&intake,&person).await,before);
}
