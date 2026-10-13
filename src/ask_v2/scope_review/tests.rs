use super::*;
use axum::{body::Body, response::Response, routing::post, Json, Router};
use sha2::Digest;
use std::{collections::VecDeque, sync::{Arc, Mutex}, time::{Duration, Instant}};

const QUESTION: &str = "Find companies named Aurora and check that name against sanctions lists.";

fn capability(slug: &str, output: &str) -> Value {
    json!({"slug":slug,"name":"Requested records","description":"Retrieve the requested information.",
        "domain":"company","inputs":["company_name"],"outputs":[output],"subject_id":"s1", "source_hint":null,"confidence":0.95})
}

fn interpretation(complete: bool) -> Value {
    let mut capabilities = vec![capability("company.sanctions", "sanctions_matches")];
    if complete { capabilities.insert(0, capability("company.search", "company_records")); }
    json!({"language":"en","subjects":[{"id":"s1","label":"Aurora","type":"company","identifiers":{"company_name":"Aurora"}}],
        "capabilities":capabilities,"deliverable":{"format":"chat","operations":[]}})
}

fn requirement(quote: &str, index: usize, output: &str) -> Value {
    json!({"quote":quote,"capabilities":[{"index":index,"outputs":[output]}],"operations":[],"format":null,"issue":null})
}

fn scope_review(complete: bool) -> Value {
    if complete {
        json!({"requirements":[requirement("Find companies named Aurora",0,"company_records"),
            requirement("check that name against sanctions lists",1,"sanctions_matches")]})
    } else {
        json!({"requirements":[
            {"quote":"Find companies named Aurora","capabilities":[],"operations":[],"format":null,
                "issue":"The independently requested company search is missing; the supplied name does not retrieve matching company records."},
            requirement("check that name against sanctions lists",0,"sanctions_matches")]})
    }
}

async fn fixture(replies: Vec<Value>, delay: Duration) -> (Client, Arc<Mutex<Vec<Value>>>, tokio::task::JoinHandle<()>) {
    let replies = Arc::new(Mutex::new(VecDeque::from(replies)));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let capture = requests.clone();
    let app = Router::new().route("/", post(move |Json(body): Json<Value>| {
        let (replies, capture) = (replies.clone(), capture.clone());
        async move {
            capture.lock().unwrap().push(body);
            let reply = replies.lock().unwrap().pop_front().expect("bounded model calls");
            tokio::time::sleep(delay).await;
            Response::builder().header("content-type","text/event-stream")
                .body(Body::from(format!("data: {}\n\ndata: [DONE]\n\n",
                    json!({"choices":[{"delta":{"content":reply.to_string()}}]})))).unwrap()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = Client::new(reqwest::Client::new(),"fixture".into(),"fixture".into())
        .with_local_endpoint(format!("http://{address}/")).unwrap();
    (client, requests, server)
}

#[tokio::test]
async fn missing_clause_is_reinterpreted_and_reviewed_before_any_intake_is_returned() {
    let (client, requests, server) = fixture(vec![interpretation(false), scope_review(false), interpretation(true), scope_review(true)], Duration::ZERO).await;
    let result = client.interpret(QUESTION, &[]).await.unwrap();
    assert_eq!(result["attempts"],2);
    assert_eq!(result["scope_reviewed"],true);
    assert_eq!(result["parsed_request"]["capabilities"].as_array().unwrap().len(),2);
    let calls = requests.lock().unwrap();
    assert_eq!(calls.len(),4);
    assert_eq!(calls[0]["messages"][1]["content"], QUESTION);
    assert_eq!(calls[1]["messages"][0]["content"], PROMPT);
    assert!(calls[2]["messages"][0]["content"].as_str().unwrap().contains("independently requested company search is missing"));
    assert_eq!(calls[3]["messages"][0]["content"], PROMPT);
    for index in [1,3] {
        let input: Value = serde_json::from_str(calls[index]["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(input["question"],QUESTION);
        assert!(input.get("catalog").is_none() && input.get("source_fields").is_none());
        assert!(input["interpretation"].get("meta").is_none());
    }
    server.abort();
}

#[tokio::test]
async fn a_repeated_omission_never_returns_a_partial_interpretation() {
    let (client, requests, server) = fixture(vec![interpretation(false),scope_review(false),interpretation(false),scope_review(false)],Duration::ZERO).await;
    assert_eq!(client.interpret(QUESTION,&[]).await.unwrap_err().to_string(),UNVERIFIED);
    assert_eq!(requests.lock().unwrap().len(),4);
    server.abort();
}

#[tokio::test]
async fn independent_scope_review_uses_medium_reasoning_without_changing_initial_extraction() {
    for model in [crate::ask_v2::DEFAULT_MODEL,"openai/gpt-6-luna"] {
        let (mut client,requests,server)=fixture(vec![interpretation(true),scope_review(true)],Duration::ZERO).await;
        client.model=model.into();
        assert!(client.interpret(QUESTION,&[]).await.is_ok());
        let calls=requests.lock().unwrap();
        assert_eq!(calls.len(),2);
        assert_eq!(calls[0]["reasoning"]["effort"],if model==crate::ask_v2::DEFAULT_MODEL {"low"} else {"none"});
        assert_eq!(calls[1]["reasoning"]["effort"],"medium");
        assert_eq!(calls[1]["reasoning"]["exclude"],true);
        server.abort();
    }
}

#[tokio::test]
async fn malformed_review_gets_one_repair_without_reparsing_or_accepting_unreviewed_scope() {
    for (second_review, succeeds) in [(scope_review(true),true),(json!({"complete":true}),false)] {
        let (client,requests,server)=fixture(vec![interpretation(true),json!({"complete":true}),second_review],Duration::ZERO).await;
        let result=client.interpret(QUESTION,&[]).await;
        assert_eq!(result.is_ok(),succeeds);
        let calls=requests.lock().unwrap();
        assert_eq!(calls.len(),3);
        assert_eq!(calls[1]["messages"][1]["content"],calls[2]["messages"][1]["content"]);
        assert!(calls[2]["messages"][0]["content"].as_str().unwrap().contains("previous review failed JSON shape"));
        if let Ok(result)=result {assert_eq!(result["attempts"],1);}
        server.abort();
    }
}

#[tokio::test]
async fn review_must_accept_the_whole_request_before_the_shared_context_path_returns() {
    let prior = "Find companies named Aurora and check that name against sanctions lists.";
    let current = "Yes, both please.";
    let context = format!("Previous user request: {prior}\nCurrent request: {current}");
    let (client, requests, server) = fixture(vec![interpretation(true),scope_review(true)],Duration::ZERO).await;
    let result = client.interpret_with_context(&context,current,&context,&[]).await.unwrap();
    assert_eq!(result.parsed_request.capabilities.len(),2);
    assert_eq!(result.parsed_request.meta.text_sha256,hex::encode(sha2::Sha256::digest(current.as_bytes())));
    assert_eq!(requests.lock().unwrap().len(),2);
    server.abort();
}

#[tokio::test]
async fn total_interpretation_deadline_includes_review_and_correction() {
    let (mut client, requests, server) = fixture(vec![interpretation(false),scope_review(false),interpretation(true),scope_review(true)],Duration::from_millis(100)).await;
    client.deadline = Duration::from_millis(150);
    let start = Instant::now();
    assert_eq!(client.interpret(QUESTION,&[]).await.unwrap_err().to_string(),"ask_v2_model_timeout");
    assert!(start.elapsed()<Duration::from_millis(600));
    assert!(requests.lock().unwrap().len()<=3);
    server.abort();
}

#[test]
fn review_is_evidence_bound_and_accounts_for_every_requested_output() {
    let parsed=capability_intake::normalize(&interpretation(true).to_string(),QUESTION,"fixture").unwrap();
    assert!(validate_review(&scope_review(true).to_string(),QUESTION,&parsed).unwrap().is_none());
    for (field,value) in [("quote",json!("invented request excerpt")),("capabilities",json!([{"index":9,"outputs":[]}])),
        ("capabilities",json!([{"index":0,"outputs":["invented_output"]}]))] {
        let mut review=scope_review(true);
        review["requirements"][0][field]=value;
        assert!(validate_review(&review.to_string(),QUESTION,&parsed).is_err(),"{field}");
    }
    let mut omitted=scope_review(true);
    omitted["requirements"].as_array_mut().unwrap().remove(0);
    assert!(validate_review(&omitted.to_string(),QUESTION,&parsed).unwrap().is_some());
    assert!(validate_review("{\"requirements\":[]}",QUESTION,&parsed).is_err());
}

#[test]
fn equivalent_subjects_and_output_names_use_the_same_scope_validation() {
    for (name,output) in [("Polaris","registry_matches"),("Maple","registered_records"),("Solace","matching_entities")] {
        let question=QUESTION.replace("Aurora",name);
        let raw=interpretation(true).to_string().replace("Aurora",name).replace("company_records",output);
        let parsed=capability_intake::normalize(&raw,&question,"fixture").unwrap();
        let review=scope_review(true).to_string().replace("Aurora",name).replace("company_records",output);
        assert!(validate_review(&review,&question,&parsed).unwrap().is_none());
    }
}
