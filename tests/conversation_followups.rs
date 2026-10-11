//! Same-chat routing, saved-evidence answers and fresh approval boundaries.
//! Only loopback model fixtures and the isolated support database are used.
use apiosk_gateway_v2::{auth::Principal,catalog::CatalogCache,engine::Engine,execution::Executor,
    parser::Parser,protocol::{DiscoverRequest,Status},state::{Entity,Signer,Task},store::Store};
use axum::{extract::State,routing::post,Json,Router};
use serde_json::{json,Value};
use sqlx::postgres::PgPoolOptions;
use std::sync::{Arc,Mutex};
use uuid::Uuid;

#[derive(Default)]
struct Calls { requests: Mutex<Vec<Value>> }
async fn model(State(calls):State<Arc<Calls>>,Json(body):Json<Value>)->Json<Value>{
    let input:Value=serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    calls.requests.lock().unwrap().push(input.clone());
    let content=if input.get("saved_outputs").is_some() {
        if input["question"].as_str().unwrap().starts_with("Jaarrekening") {json!({"kind":"new_request"})}
        else {json!({"kind":"saved_answer","outputs":[0]})}
    } else if input.get("sources").is_some() {
        let source=&input["sources"][0];
        json!({"observations":[{"text":"The employer rating is {{a}}, based on {{b}} reviews.","evidence":[
            {"result_ref":source["result_ref"],"pointer":"/data/rating"},
            {"result_ref":source["result_ref"],"pointer":"/data/review_count"}]}],"limitations":[],"follow_up_question":null})
    } else {
        assert!(input["context"]["conversation"].as_array().unwrap().is_empty(),"new topic inherited the old question: {input}");
        assert!(input["context"]["previous_intent"].is_null());
        json!({"message":"Ik zoek de jaarrekening op en toon eerst de prijs.","discover":true,"intent":{
            "subjects":[{"subject_ref":"s1","values":{"company.name":{"ref":"s1.label"}}}],
            "goals":[{"capability":"company.accounts","subject":0,"constraints":{}}],
            "answer_operations":[],"unresolved_references":[]}})
    };
    Json(json!({"choices":[{"finish_reason":"stop","message":{"content":content.to_string()}}]}))
}
async fn ask(Json(body):Json<Value>)->([(axum::http::header::HeaderName,&'static str);1],String){
    assert!(!body["messages"][1]["content"].as_str().unwrap().contains("Starbucks"));
    let parsed=json!({"language":"nl","subjects":[{"id":"s1","label":"Newco","type":"company","identifiers":{}}],
        "capabilities":[{"slug":"company.financial.statements","name":"Company financial statements","description":"Get company accounts","domain":"company","inputs":[],"outputs":["accounts"],"subject_id":"s1","source_hint":null,"confidence":1}],
        "deliverable":{"format":"chat","operations":[]}});
    let event=json!({"choices":[{"delta":{"content":parsed.to_string()}}]});
    ([(axum::http::header::CONTENT_TYPE,"text/event-stream")],format!("data: {event}\n\ndata: [DONE]\n\n"))
}

async fn send(engine:&Engine,p:&Principal,id:Uuid,question:&str)->apiosk_gateway_v2::protocol::Response{
    let (revision,task,_)=engine.store.load(p.owner,id).await.unwrap();
    engine.discover(p,DiscoverRequest {request_id:Uuid::new_v4(),question:question.into(),
        state:Some(engine.signer.issue(p.owner,id,revision,task.focus)),context_delta:Default::default()}).await.unwrap()
}

#[tokio::test]
#[ignore="isolated support-db.mjs required"]
async fn same_chat_answers_saved_sources_and_prices_a_new_topic_without_purchases(){
    let pool=PgPoolOptions::new().max_connections(1).after_connect(|conn,_|Box::pin(async move{
        sqlx::query("SET search_path TO public,gateway_v2,actions,billing,providers,catalog,research,identity,observability").execute(conn).await?;Ok(())
    })).connect(&std::env::var("TEST_DATABASE_URL").unwrap()).await.unwrap();
    let calls=Arc::new(Calls::default());
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base=format!("http://{}",listener.local_addr().unwrap());
    let app=Router::new().route("/model",post(model)).route("/ask",post(ask)).with_state(calls.clone());
    let server=tokio::spawn(async move{axum::serve(listener,app).await.unwrap()});
    let store=Store{pool:pool.clone()};let http=reqwest::Client::new();
    let engine=Arc::new(Engine {store:store.clone(),catalog:CatalogCache::new(pool.clone()),
        parser:Parser::new(http.clone(),format!("{base}/model"),"fixture".into(),"fixture".into())
            .with_ask_v2(apiosk_gateway_v2::ask_v2::Client::new(http.clone(),"fixture".into(),"fixture".into()).with_local_endpoint(format!("{base}/ask")).unwrap()),
        signer:Signer::new(vec![7;32]).unwrap(),executor:Executor{store:store.clone(),http,supabase_url:base,enabled:true},app_url:"http://localhost".into()});
    let p=Principal {owner:Uuid::new_v4(),connection:None,workspace:None};
    let id=store.create(p.owner,None).await.unwrap();let entity=Uuid::new_v4();let source=Uuid::new_v4();let execution=Uuid::new_v4();
    let doc=json!({"source":{"provider":"glassdoor","name":"Glassdoor"},"subject":{"label":"Starbucks","entity_ref":entity},
        "data":{"rating":3.5,"review_count":92195},"coverage":"source_response","missing_fields":[]});
    sqlx::query("INSERT INTO executions(id,owner_id,task_id,action_id,payload_hash,selection_id,reserved_atomic,actual_atomic,status) VALUES($1,$2,$3,$1,'fixture',$1,10,10,'succeeded')")
        .bind(execution).bind(p.owner).bind(id).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO results(id,owner_id,task_id,execution_id,document) VALUES($1,$2,$3,$4,$5)")
        .bind(source).bind(p.owner).bind(id).bind(execution).bind(doc).execute(&pool).await.unwrap();
    let mut task=Task::default();task.record_turn("Get the Starbucks employer rating",None);
    task.entities.push(Entity{entity_ref:entity,label:"Starbucks".into(),selected:true});
    task.result_refs=vec![source];task.active_results=vec![source];
    // Production regression: paid compute has a settled execution receipt but
    // deliberately no source result row. It must not block all later turns.
    let compute_id=Uuid::new_v4();
    let mut compute=apiosk_gateway_v2::compute::Run::new(Uuid::new_v4(),"Starbucks analysis".into(),"claude-opus-5.5".into());
    compute.action_id=compute_id;compute.status="completed".into();task.compute=Some(compute);
    sqlx::query("INSERT INTO executions(id,owner_id,task_id,action_id,payload_hash,selection_id,reserved_atomic,actual_atomic,status) VALUES($1,$2,$3,$1,'compute-fixture',$1,10,10,'succeeded')")
        .bind(compute_id).bind(p.owner).bind(id).execute(&pool).await.unwrap();
    task.analysis=Some(json!({"status":"completed","observations":[{"text":"Starbucks has an employer rating","evidence":[]}],"limitations":[]}));
    store.save(p.owner,id,0,&task).await.unwrap();
    sqlx::query("UPDATE executions SET status='unresolved' WHERE id=$1").bind(compute_id).execute(&pool).await.unwrap();
    let blocked=send(&engine,&p,id,"Explain the saved Starbucks rating").await;
    assert!(!blocked.errors.is_empty(),"an unsettled compute receipt still blocks a new turn");
    assert!(calls.requests.lock().unwrap().is_empty());
    sqlx::query("UPDATE executions SET status='succeeded' WHERE id=$1").bind(compute_id).execute(&pool).await.unwrap();
    for question in ["Use only the saved Starbucks results to show the rating and review count. No new source requests.","En hoeveel beoordelingen waren dat?"] {
        let response=send(&engine,&p,id,question).await;
        assert!(response.errors.is_empty(),"{response:?}");
        assert_eq!(response.status,Status::Running);
        assert!(response.proposal.is_none(),"saved answer must not ask for a new purchase");
        // Resume from durable state exactly like a reload after a lost response.
        engine.start_worker(p.clone(),String::new(),id).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10),async {
            while engine.worker_active(id).await.unwrap(){tokio::time::sleep(std::time::Duration::from_millis(20)).await;}
        }).await.unwrap();
        let answer=engine.view(&p,id,Uuid::new_v4(),None).await.unwrap();
        assert_eq!(answer.status,Status::Succeeded,"{answer:?}");
        assert_eq!(answer.context_view["analysis"]["observations"][0]["evidence"][1]["value"],92195,"{answer:?}");
        assert_eq!(answer.state.unwrap().state_ref,id);
    }
    let fresh=send(&engine,&p,id,"Jaarrekening van Newco").await;
    assert!(fresh.errors.is_empty(),"{fresh:?}");
    assert_eq!(fresh.status,Status::Ready,"{fresh:?}");
    assert!(fresh.proposal.as_ref().is_some_and(|p|p.max_total_atomic!="0"));
    assert_eq!(fresh.state.unwrap().state_ref,id);
    assert!(fresh.context_view["conversation"].as_array().unwrap().len()>=4);
    let turns = fresh.context_view["conversation"].as_array().unwrap();
    for turn in &turns[1..3] {
        assert_eq!(turn["output"]["analysis"]["observations"][0]["evidence"][1]["value"],92195,"a saved-only answer must remain visible after the next question: {turn}");
        assert_eq!(turn["output"]["result_refs"],json!([source]));
    }
    let (_,saved,_)=store.load(p.owner,id).await.unwrap();
    assert!(!saved.saved_answer);assert!(saved.active_results.is_empty());
    assert!(saved.intent.unwrap().to_string().contains("Newco"));
    let old=send(&engine,&p,id,"Explain the saved Starbucks review count again; no new source calls.").await;
    assert_eq!(old.status,Status::Running,"{old:?}");
    assert!(old.proposal.is_none());
    assert_eq!(store.load(p.owner,id).await.unwrap().1.active_results,vec![source]);
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM executions WHERE task_id=$1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(count,2,"follow-ups must not purchase a source");
    assert!(store.load(p.owner,id).await.unwrap().1.settled_compute.contains(&compute_id));
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM authorizations WHERE task_id=$1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(count,0,"fresh plans must wait for explicit approval");
    let missing=Uuid::new_v4();
    sqlx::query("INSERT INTO executions(id,owner_id,task_id,action_id,payload_hash,selection_id,reserved_atomic,actual_atomic,status) VALUES($1,$2,$3,$1,'missing-source',$1,10,10,'succeeded')")
        .bind(missing).bind(p.owner).bind(id).execute(&pool).await.unwrap();
    let blocked=send(&engine,&p,id,"Explain the saved Starbucks rating").await;
    assert!(!blocked.errors.is_empty(),"a source receipt without its saved result is still unresolved");
    let other=Principal{owner:Uuid::new_v4(),..p.clone()};
    let view=engine.view(&other,id,Uuid::new_v4(),None).await;
    assert!(view.is_err());
    server.abort();
}
