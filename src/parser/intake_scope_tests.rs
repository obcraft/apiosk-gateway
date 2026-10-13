use super::*;
use axum::{routing::{get,post},Json,Router};
use std::sync::Mutex;

#[tokio::test]
async fn source_selection_preserves_user_values_and_uses_the_same_scope_review() {
    let question="Show the registered company details.";
    let calls=Arc::new(Mutex::new(Vec::new()));
    let capture=calls.clone();
    let app=Router::new().route("/model",post(move |Json(body):Json<Value>| {
        let capture=capture.clone();
        async move {
            let review=body["messages"][0]["content"]==include_str!("../../contracts/intake-scope-review-prompt.txt");
            capture.lock().unwrap().push(body);
            let response=if review {
                json!({"requirements":[{"quote":question,"capabilities":[{"index":0,"outputs":["company_profile"]}],
                    "operations":[],"format":null,"issue":null}]})
            } else {
                json!({"language":"en","subjects":[{"id":"s1","label":"Aurora","type":"company","identifiers":{"company_name":"Aurora"}}],
                    "capabilities":[{"slug":"company.profile","name":"Company details","description":"Retrieve registered company details.",
                        "domain":"company","inputs":["company_name"],"outputs":["company_profile"],"subject_id":"s1","source_hint":null,"confidence":0.95}],
                    "deliverable":{"format":"chat","operations":[]}})
            };
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
    let selection=ModelSelection{view:json!({"mode":"restrict","source_metadata":"must not reach scope review"}),literals:vec!["Aurora".into()]};
    let catalog=Catalog::build(crate::catalog::Snapshot{services:vec![],vocabulary:vec![],known_sources:vec![]});
    let result=parser.prepare_ask(question,Some(&selection),&Task::default(),&catalog).await.unwrap();
    assert_eq!(result.parsed_request.subjects[0].identifiers["company_name"],"Aurora");
    let calls=calls.lock().unwrap();
    assert_eq!(calls.len(),2);
    assert!(calls[0]["messages"][1]["content"].as_str().unwrap().contains("Aurora"));
    assert_eq!(calls[1]["messages"][0]["content"],include_str!("../../contracts/intake-scope-review-prompt.txt"));
    assert!(!calls.iter().any(|call|call.to_string().contains("must not reach scope review")));
    server.abort();
}
