//! Ask chat follow-ups after an answer: saved results answer in place across
//! the chat's purchases, new needs go back to source search, and nothing is
//! bought. Only loopback model fixtures and the isolated support database.
use apiosk_gateway_v2::{auth::Principal,catalog::CatalogCache,engine::{AskFollowUp,Engine},execution::Executor,
    parser::Parser,protocol::Status,state::{Entity,Signer,Task},store::Store};
use axum::{extract::State,routing::post,Json,Router};
use serde_json::{json,Value};
use sqlx::postgres::PgPoolOptions;
use std::sync::{Arc,Mutex};
use uuid::Uuid;

#[derive(Default)]
struct Calls { routes: Mutex<Vec<Value>> }
async fn model(State(calls):State<Arc<Calls>>,Json(body):Json<Value>)->Json<Value>{
    let input:Value=serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    let content=if input.get("saved_outputs").is_some() {
        calls.routes.lock().unwrap().push(input.clone());
        match input["question"].as_str().unwrap() {
            "Vertaal beide antwoorden naar het Nederlands" => json!({"kind":"saved_answer","outputs":[0,1]}),
            "Voeg ook de jaarrekening toe" => json!({"kind":"continue_request"}),
            _ => json!({"kind":"clarification","message":"Bedoel je Starbucks of Costa?"}),
        }
    } else {
        let source=&input["sources"][0];
        assert!(input["answer_operations"][0].as_str().unwrap().contains("in the language it asks for"),"{input}");
        json!({"observations":[{"text":"De werkgeversscore is {{a}}, op basis van {{b}} beoordelingen.","evidence":[
            {"result_ref":source["result_ref"],"pointer":"/data/rating"},
            {"result_ref":source["result_ref"],"pointer":"/data/review_count"}]}],"limitations":[],"follow_up_question":null})
    };
    Json(json!({"choices":[{"finish_reason":"stop","message":{"content":content.to_string()}}]}))
}

async fn purchase(store:&Store,pool:&sqlx::PgPool,p:&Principal,company:&str,rating:f64,reviews:u64)->(Uuid,Uuid){
    let id=store.create(p.owner,None).await.unwrap();let entity=Uuid::new_v4();let result=Uuid::new_v4();let execution=Uuid::new_v4();
    let doc=json!({"source":{"provider":"glassdoor","name":"Glassdoor"},"subject":{"label":company,"entity_ref":entity},
        "data":{"rating":rating,"review_count":reviews},"coverage":"source_response","missing_fields":[]});
    sqlx::query("INSERT INTO executions(id,owner_id,task_id,action_id,payload_hash,selection_id,reserved_atomic,actual_atomic,status) VALUES($1,$2,$3,$1,'fixture',$1,10,10,'succeeded')")
        .bind(execution).bind(p.owner).bind(id).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO results(id,owner_id,task_id,execution_id,document) VALUES($1,$2,$3,$4,$5)")
        .bind(result).bind(p.owner).bind(id).bind(execution).bind(doc).execute(pool).await.unwrap();
    let mut task=Task::default();task.record_turn(&format!("Employer rating for {company}"),None);
    task.entities.push(Entity{entity_ref:entity,label:company.into(),selected:true});
    task.result_refs=vec![result];task.active_results=vec![result];
    task.analysis=Some(json!({"status":"completed","observations":[{"text":format!("{company} has an employer rating"),"evidence":[]}],"limitations":[]}));
    store.save(p.owner,id,0,&task).await.unwrap();
    (id,result)
}

#[tokio::test]
#[ignore="isolated support-db.mjs required"]
async fn saved_results_answer_every_purchase_in_place_and_new_needs_search_without_buying(){
    let pool=PgPoolOptions::new().max_connections(1).after_connect(|conn,_|Box::pin(async move{
        sqlx::query("SET search_path TO public,gateway_v2,actions,billing,providers,catalog,research,identity,observability").execute(conn).await?;Ok(())
    })).connect(&std::env::var("TEST_DATABASE_URL").unwrap()).await.unwrap();
    let calls=Arc::new(Calls::default());
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base=format!("http://{}",listener.local_addr().unwrap());
    let app=Router::new().route("/model",post(model)).with_state(calls.clone());
    let server=tokio::spawn(async move{axum::serve(listener,app).await.unwrap()});
    let store=Store{pool:pool.clone()};let http=reqwest::Client::new();
    let engine=Arc::new(Engine {store:store.clone(),catalog:CatalogCache::new(pool.clone()),
        parser:Parser::new(http.clone(),format!("{base}/model"),"fixture".into(),"fixture".into()),
        signer:Signer::new(vec![7;32]).unwrap(),executor:Executor{store:store.clone(),http,supabase_url:base,enabled:true},app_url:"http://localhost".into()});
    let p=Principal {owner:Uuid::new_v4(),connection:None,workspace:None};
    let (costa,costa_result)=purchase(&store,&pool,&p,"Costa",3.9,1200).await;
    let (starbucks,starbucks_result)=purchase(&store,&pool,&p,"Starbucks",3.5,92195).await;
    let chat=[starbucks,costa];
    let revisions=|| async { let mut all=vec![];for id in chat {all.push(store.load(p.owner,id).await.unwrap().0);} all };
    let before=revisions().await;

    // A new need goes to source search; a question back changes nothing either.
    assert_eq!(engine.ask_follow_up(&p,Uuid::new_v4(),"Voeg ook de jaarrekening toe",&chat).await.unwrap(),AskFollowUp::Search);
    assert_eq!(engine.ask_follow_up(&p,Uuid::new_v4(),"Leg dat uit",&chat).await.unwrap(),AskFollowUp::Clarification("Bedoel je Starbucks of Costa?".into()));
    assert_eq!(revisions().await,before,"routing alone never changes a purchase");
    let listed=calls.routes.lock().unwrap()[0]["saved_outputs"].clone();
    assert_eq!((listed[0]["question"].as_str(),listed[1]["question"].as_str()),(Some("Employer rating for Starbucks"),Some("Employer rating for Costa")),"most recent purchase first: {listed}");

    // Another account's tasks are not read, and no model is asked about them.
    let other=Principal{owner:Uuid::new_v4(),..p.clone()};
    let asked=calls.routes.lock().unwrap().len();
    assert_eq!(engine.ask_follow_up(&other,Uuid::new_v4(),"Vertaal beide antwoorden naar het Nederlands",&chat).await.unwrap(),AskFollowUp::Search);
    assert_eq!(calls.routes.lock().unwrap().len(),asked);

    // Saved results answer every purchase they were selected from.
    let routed=engine.ask_follow_up(&p,Uuid::new_v4(),"Vertaal beide antwoorden naar het Nederlands",&chat).await.unwrap();
    assert_eq!(routed,AskFollowUp::SavedAnswer(vec![starbucks,costa]));
    for (id,result,reviews) in [(starbucks,starbucks_result,92195),(costa,costa_result,1200)] {
        engine.start_worker(p.clone(),String::new(),id).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10),async {
            while engine.worker_active(id).await.unwrap(){tokio::time::sleep(std::time::Duration::from_millis(20)).await;}
        }).await.unwrap();
        let answer=engine.view(&p,id,Uuid::new_v4(),None).await.unwrap();
        assert_eq!(answer.status,Status::Succeeded,"{answer:?}");
        assert!(answer.proposal.is_none(),"a saved answer is never a purchase: {answer:?}");
        assert_eq!(answer.context_view["analysis"]["observations"][0]["evidence"][1]["value"],reviews,"{answer:?}");
        let turns=answer.context_view["conversation"].as_array().unwrap();
        assert_eq!(turns.len(),2);
        assert_eq!(turns[1]["question"],"Vertaal beide antwoorden naar het Nederlands");
        assert!(turns[0]["output"]["analysis"]["observations"][0]["text"].as_str().unwrap().ends_with("has an employer rating"),"the purchase keeps its own answer: {turns:?}");
        assert_eq!(turns[0]["output"]["result_refs"],json!([result]));
    }

    // While an answer is being written its purchase is busy and left out.
    let (_,mut task,_)=store.load(p.owner,starbucks).await.unwrap();
    let revision=store.load(p.owner,starbucks).await.unwrap().0;
    task.actions=vec![json!({"kind":"analyze","action_id":Uuid::new_v4()})];
    store.save(p.owner,starbucks,revision,&task).await.unwrap();
    let asked=calls.routes.lock().unwrap().len();
    engine.ask_follow_up(&p,Uuid::new_v4(),"Leg dat uit",&chat).await.unwrap();
    assert_eq!(calls.routes.lock().unwrap()[asked]["saved_outputs"].as_array().unwrap().len(),1,"only the settled purchase is offered");

    let count:i64=sqlx::query_scalar("SELECT count(*) FROM executions WHERE task_id=ANY($1)").bind(&chat[..]).fetch_one(&pool).await.unwrap();
    assert_eq!(count,2,"follow-ups must not purchase a source");
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM authorizations WHERE task_id=ANY($1)").bind(&chat[..]).fetch_one(&pool).await.unwrap();
    assert_eq!(count,0);
    server.abort();
}
