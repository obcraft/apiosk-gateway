//! Standalone question → intent boundary for the new Ask v2 page.
//! No catalog, planner, task storage, quote or execution dependencies.
use super::Client;
use anyhow::{anyhow, ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::{BTreeMap, BTreeSet}, time::{Duration, Instant}};

pub const MODEL: &str = "qwen/qwen3-30b-a3b-instruct-2507";
const PROMPT: &str = include_str!("../../contracts/ask-intent-prompt.txt");
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    language: String,
    subjects: Vec<Subject>,
    requests: Vec<Request>,
    output: Output,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Subject { id: String, #[serde(rename="type")] kind: String, label: String, identifiers: BTreeMap<String, String> }
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: String, subject_ids: Vec<String>, kind: Kind, goal: String,
    required_fields: Vec<String>, optional_fields: Vec<String>,
    filters: BTreeMap<String, String>, sources: Vec<String>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all="snake_case")]
enum Kind { Lookup, Search, Action, Answer }
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Output { format: String, operations: Vec<String> }

fn text(value: &str, max: usize) -> bool { !value.trim().is_empty() && value.chars().count() <= max }
fn concept(value: &str) -> bool {
    text(value, 100) && value.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}
fn literal(value: &str, question: &str) -> bool { text(value, 2000) && question.contains(value) }
fn fields(values: &[String]) -> bool {
    values.len() <= 32 && values.iter().all(|v| concept(v)) && values.iter().collect::<BTreeSet<_>>().len() == values.len()
}
fn literals(values: &BTreeMap<String,String>, question: &str) -> bool {
    values.len() <= 32 && values.iter().all(|(key,value)| concept(key) && literal(value, question))
}

fn validate(raw: &str, question: &str) -> Result<Intent> {
    let intent: Intent = serde_json::from_str(raw).map_err(|_| anyhow!("intent_invalid_json"))?;
    ensure!(intent.language.len() == 2 && intent.language.bytes().all(|c| c.is_ascii_lowercase()), "intent_invalid_language");
    ensure!(intent.subjects.len() <= 16 && !intent.requests.is_empty() && intent.requests.len() <= 16, "intent_invalid_scope");
    let mut ids = BTreeSet::new();
    for subject in &intent.subjects {
        ensure!(text(&subject.id, 32) && ids.insert(subject.id.as_str()) && concept(&subject.kind)
            && literal(&subject.label, question) && literals(&subject.identifiers, question), "intent_invalid_subject");
    }
    let mut requests = BTreeSet::new();
    for request in &intent.requests {
        ensure!(text(&request.id, 32) && requests.insert(&request.id) && text(&request.goal, 1500), "intent_invalid_request");
        ensure!(request.subject_ids.len() <= 16
            && request.subject_ids.iter().all(|id| ids.contains(id.as_str()))
            && request.subject_ids.iter().collect::<BTreeSet<_>>().len() == request.subject_ids.len(), "intent_invalid_reference");
        ensure!(fields(&request.required_fields) && fields(&request.optional_fields)
            && !request.required_fields.iter().any(|f| request.optional_fields.contains(f)), "intent_invalid_fields");
        ensure!(literals(&request.filters, question) && request.sources.len() <= 16
            && request.sources.iter().all(|s| literal(s, question)), "intent_invalid_criteria");
    }
    ensure!(concept(&intent.output.format) && intent.output.operations.len() <= 16
        && intent.output.operations.iter().all(|o| text(o, 500)), "intent_invalid_output");
    Ok(intent)
}

impl Client {
    pub async fn extract_intent(&self, question: &str) -> Result<Value> {
        ensure!(!question.trim().is_empty() && question.encode_utf16().count() <= 16_000, "intent_invalid_question");
        let started = Instant::now();
        let body = json!({
            "model": MODEL, "messages": [{"role":"system","content":PROMPT},{"role":"user","content":question}],
            "temperature": 0, "stream": true, "max_tokens": 4096,
            "provider": {"require_parameters":true,"sort":"latency"},
            "response_format": {"type":"json_object"}
        });
        let raw = tokio::time::timeout(TIMEOUT, self.send_object(body, false)).await
            .map_err(|_| anyhow!("intent_timeout"))??;
        let intent = validate(&raw, question)?;
        Ok(json!({"schema_version":1,"intent":intent,"model":MODEL,"timing":{"parse_ms":started.elapsed().as_millis() as u64}}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const QUESTION: &str = "Compare Starbucks and McDonalds on Glassdoor: overall rating and available pay ratings.";
    fn fixture() -> Value { json!({
        "language":"en","subjects":[{"id":"s1","type":"company","label":"Starbucks","identifiers":{}},{"id":"s2","type":"company","label":"McDonalds","identifiers":{}}],
        "requests":[{"id":"r1","subject_ids":["s1","s2"],"kind":"lookup","goal":"Employer ratings","required_fields":["overall_rating"],"optional_fields":["pay_rating"],"filters":{},"sources":["Glassdoor"]}],
        "output":{"format":"chat","operations":["compare"]}
    }) }
    #[test]
    fn preserves_multi_subject_and_optional_fields() {
        let value = fixture();
        assert_eq!(serde_json::to_value(validate(&value.to_string(), QUESTION).unwrap()).unwrap(), value);
    }
    #[test]
    fn rejects_invented_values_and_dangling_references() {
        for (pointer,value) in [("/subjects/0/label",json!("Costa")),("/subjects/0/identifiers",json!({"country":"US"})),
            ("/requests/0/subject_ids",json!(["s3"])),("/requests/0/filters",json!({"country":"US"})),
            ("/requests/0/sources",json!(["Indeed"])),("/requests/0/optional_fields",json!(["overall_rating"]))] {
            let mut input = fixture(); *input.pointer_mut(pointer).unwrap() = value;
            assert!(validate(&input.to_string(), QUESTION).is_err(), "{pointer}");
        }
    }
    #[test]
    fn accepts_general_requests_without_catalog_categories() {
        let input = json!({"language":"nl","subjects":[],"requests":[{"id":"r1","subject_ids":[],"kind":"answer","goal":"Write a birthday poem","required_fields":[],"optional_fields":[],"filters":{},"sources":[]}],"output":{"format":"chat","operations":[]}});
        assert!(validate(&input.to_string(), "Schrijf een verjaardagsgedicht").is_ok());
    }

    #[tokio::test]
    async fn uses_one_instruct_call_and_returns_validated_intent() {
        use axum::{routing::post, Json, Router, response::Response, body::Body};
        use std::sync::{Arc, Mutex};
        let captured = Arc::new(Mutex::new(Vec::new()));
        let requests = captured.clone();
        let app = Router::new().route("/", post(move |Json(body): Json<Value>| {
            let requests = requests.clone();
            async move {
                requests.lock().unwrap().push(body);
                let event = json!({"choices":[{"delta":{"content":fixture().to_string()},"finish_reason":"stop"}]});
                Response::builder().header("content-type", "text/event-stream")
                    .body(Body::from(format!("data: {event}\n\n"))).unwrap()
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = Client::new(reqwest::Client::new(), "fixture".into(), "ignored-legacy-model".into()).with_local_endpoint(endpoint).unwrap();
        let result = client.extract_intent(QUESTION).await.unwrap();
        assert_eq!(result["intent"], fixture());
        assert_eq!(result["model"], MODEL);
        assert!(result["timing"]["parse_ms"].is_u64());
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["model"], MODEL);
        assert_eq!(requests[0]["messages"][1]["content"], QUESTION);
        assert_eq!(requests[0]["provider"], json!({"require_parameters":true,"sort":"latency"}));
        assert_eq!(requests[0]["response_format"], json!({"type":"json_object"}));
        assert!(requests[0].get("reasoning").is_none());
        server.abort();
    }

    #[tokio::test]
    async fn rate_limit_fails_without_slow_automatic_replay() {
        use axum::{routing::post, Router, http::StatusCode};
        use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let app = Router::new().route("/", post(move || { let count=count.clone(); async move { count.fetch_add(1,Ordering::SeqCst); StatusCode::TOO_MANY_REQUESTS } }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = Client::new(reqwest::Client::new(), "fixture".into(), "ignored".into()).with_local_endpoint(endpoint).unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(1), client.extract_intent(QUESTION)).await.unwrap().is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        server.abort();
    }
}
