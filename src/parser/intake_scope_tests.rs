use super::*;
use axum::{routing::{get,post},Json,Router};
use std::sync::Mutex;

#[tokio::test]
async fn connected_agent_and_app_use_the_same_prompt_and_complete_source_search() {
    let question = "Haal de bedrijfsgegevens op van Financial Crime Compliance Associates, KVK-nummer 91251982.";
    let calls = Arc::new(Mutex::new(Vec::new()));
    let capture = calls.clone();
    let searches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count_search = searches.clone();
    let app = Router::new().route("/model",post(move |Json(body):Json<Value>| {
        let capture = capture.clone();
        async move {
            capture.lock().unwrap().push(body);
            let response = json!({"language":"nl",
                "subjects":[{"id":"s1","label":"Financial Crime Compliance Associates","type":"company","identifiers":{"kvk_number":"91251982"}}],
                "capabilities":[{"slug":"company.records.retrieve","name":"Company details","description":"Retrieve company information",
                    "domain":"company","inputs":["kvk_number"],"outputs":["company_details"],"subject_id":"s1","source_hint":null,"confidence":0.95}],
                "deliverable":{"format":"chat","operations":[]}});
            ([(axum::http::header::CONTENT_TYPE,"text/event-stream")], format!("data: {}\n\ndata: [DONE]\n\n",
                json!({"choices":[{"delta":{"content":response.to_string()}}]})))
        }
    })).route("/search",get(move || { let count_search=count_search.clone(); async move {
        count_search.fetch_add(1,std::sync::atomic::Ordering::Relaxed);
        Json(json!({"results":[]}))
    }}));
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint=format!("http://{}",listener.local_addr().unwrap());
    let server=tokio::spawn(async move{axum::serve(listener,app).await.unwrap()});
    let client=crate::ask_v2::Client::new(reqwest::Client::new(),"fixture".into(),"fixture".into())
        .with_local_endpoint(format!("{endpoint}/model")).unwrap();
    let parser=Parser::new(reqwest::Client::new(),"offline://legacy-intake-must-not-run".into(),String::new(),"fixture".into())
        .with_ask_v2(client).with_local_bazaar(format!("{endpoint}/search")).unwrap();
    let catalog=Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/workflows/catalog.json")).unwrap());
    let app=parser.prepare_ask(question,None,&Task::default(),&catalog).await.unwrap();
    let agent=parser.ask_intake(question,&Task::default(),&catalog,None).await.unwrap();
    assert_eq!(serde_json::to_value(&app.matches).unwrap(),serde_json::to_value(&agent.matches).unwrap());
    assert_eq!(app.selected_capabilities,agent.selected_capabilities);
    assert!(agent.selected_capabilities.contains("company.registry-profile"));
    assert!(agent.matches.iter().all(|found|found.coinbase_status != "pending"));
    assert_eq!(searches.load(std::sync::atomic::Ordering::Relaxed),2);
    let calls=calls.lock().unwrap();
    assert_eq!(calls.len(),2,"one interpretation per surface");
    assert_eq!(calls[0],calls[1],"the exact same prompt and model configuration");
    server.abort();
}

#[tokio::test]
async fn source_selection_preserves_user_values_with_one_interpretation_request() {
    let question="Show the registered company details.";
    let calls=Arc::new(Mutex::new(Vec::new()));
    let capture=calls.clone();
    let app=Router::new().route("/model",post(move |Json(body):Json<Value>| {
        let capture=capture.clone();
        async move {
            capture.lock().unwrap().push(body);
            let response=json!({"language":"en","subjects":[{"id":"s1","label":"Aurora","type":"company","identifiers":{"company_name":"Aurora"}}],
                "capabilities":[{"slug":"company.profile","name":"Company details","description":"Retrieve registered company details.",
                    "domain":"company","inputs":["company_name"],"outputs":["company_profile"],"subject_id":"s1","source_hint":null,"confidence":0.95}],
                "deliverable":{"format":"chat","operations":[]}});
            ([(axum::http::header::CONTENT_TYPE,"text/event-stream")],format!("data: {}\n\ndata: [DONE]\n\n",
                json!({"choices":[{"delta":{"content":response.to_string()}}]})))
        }
    })).route("/search",get(||async{Json(json!({"results":[]}))}));
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint=format!("http://{}",listener.local_addr().unwrap());
    let server=tokio::spawn(async move{axum::serve(listener,app).await.unwrap()});
    let client=crate::ask_v2::Client::new(reqwest::Client::new(),"fixture".into(),"fixture".into())
        .with_local_endpoint(format!("{endpoint}/model")).unwrap();
    let parser=Parser::new(reqwest::Client::new(),"offline://legacy-intake-must-not-run".into(),String::new(),"fixture".into())
        .with_ask_v2(client).with_local_bazaar(format!("{endpoint}/search")).unwrap();
    let selection=ModelSelection{view:json!({"mode":"restrict","source_metadata":"must not reach the interpretation model"}),literals:vec!["Aurora".into()]};
    let catalog=Catalog::build(crate::catalog::Snapshot{services:vec![],vocabulary:vec![],known_sources:vec![]});
    let result=parser.prepare_ask(question,Some(&selection),&Task::default(),&catalog).await.unwrap();
    assert_eq!(result.parsed_request.subjects[0].identifiers["company_name"],"Aurora");
    let calls=calls.lock().unwrap();
    assert_eq!(calls.len(),1,"one interpretation request; no scope review, repair or second model");
    assert!(calls[0]["messages"][1]["content"].as_str().unwrap().contains("Aurora"));
    assert!(!calls.iter().any(|call|call.to_string().contains("must not reach the interpretation model")));
    server.abort();
}
