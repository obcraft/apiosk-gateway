use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use serde_json::{json, Map, Value};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

#[derive(Default)]
pub struct Fixture {
    pub typesafe_calls: AtomicU64,
    pub baseline_calls: AtomicU64,
    pub provider_calls: AtomicU64,
    pub reject_guard: Option<String>,
    pub malformed: bool,
    pub http_failure: bool,
    pub slow: bool,
}

pub fn decision_response(body: &Value) -> Value {
    let numeric = body["state"]["question"]
        .as_str()
        .is_some_and(|q| q.contains("123"));
    let mut answers = Map::new();
    for (name, question) in body["questions"].as_object().unwrap() {
        let answer = if question["type"] == "noul" {
            json!({"type":"noul","noul":1.0})
        } else {
            let criteria = question["criteria"].as_object().unwrap();
            let selected = match name.as_str() {
                "capability" => "company.accounts".to_owned(),
                "input_type" => if numeric {
                    "company.id"
                } else {
                    "company.name"
                }
                .to_owned(),
                "input_span" => criteria
                    .iter()
                    .find(|(_, value)| *value == if numeric { "123" } else { "Example" })
                    .unwrap()
                    .0
                    .clone(),
                "source" => "__none__".into(),
                "language" => "en".into(),
                "field" => criteria
                    .iter()
                    .find(|(_, value)| value["pointer"] == "/data/accounts")
                    .unwrap()
                    .0
                    .clone(),
                _ => panic!("unexpected question {name}"),
            };
            assert!(criteria.contains_key(&selected));
            let probabilities: Map<String, Value> = criteria
                .keys()
                .map(|key| (key.clone(), json!(if key == &selected { 1.0 } else { 0.0 })))
                .collect();
            json!({"type":"choice","choice":selected,"confidence":1.0,"probabilities":probabilities})
        };
        answers.insert(name.clone(), answer);
    }
    json!({"model":"jev-1.13.0","answers":answers,"usage":{"input_tokens":100,"output_tokens":20}})
}

async fn typesafe(
    State(f): State<Arc<Fixture>>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    f.typesafe_calls.fetch_add(1, Ordering::Relaxed);
    if f.slow {
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    }
    if f.http_failure {
        return (StatusCode::TOO_MANY_REQUESTS, Json(json!({})));
    }
    let mut response = decision_response(&body);
    if let Some(guard) = &f.reject_guard {
        if response["answers"].get(guard).is_some() {
            response["answers"][guard]["noul"] = json!(0.4);
        }
    }
    if f.malformed && response["answers"].get("capability").is_some() {
        response["answers"]["capability"]["choice"] = json!("invented.capability");
    }
    (StatusCode::OK, Json(response))
}

async fn baseline(State(f): State<Arc<Fixture>>, Json(body): Json<Value>) -> Json<Value> {
    f.baseline_calls.fetch_add(1, Ordering::Relaxed);
    let input: Value =
        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    let answer = if input.get("answer_operations").is_some() {
        json!({"observations":[{"text":"The source reports {{a}}.","evidence":[{"result_ref":input["sources"][0]["result_ref"],"pointer":"/data/accounts"}]}],"limitations":[]})
    } else {
        assert!(
            input["catalog"]["capabilities"]
                .as_array()
                .is_some_and(|caps| caps.len() == 2),
            "fallback must receive the entire catalog"
        );
        let constraints = if input["question"].as_str().unwrap().contains("latest") {
            json!({"filing_selection":"latest"})
        } else {
            json!({})
        };
        let intent = json!({"subjects":[{"entity_ref":null,"label":"Example","values":{"company.name":"Example"}}],"goals":[{"capability":"company.accounts","subject":0,"constraints":constraints}],"answer_operations":["Explain the filing"],"unresolved_references":[]});
        if input["surface"] == "ask-chat" {
            json!({"message":"Baseline reply","discover":true,"intent":intent})
        } else {
            intent
        }
    };
    Json(
        json!({"choices":[{"message":{"content":answer.to_string()}}],"usage":{"prompt_tokens":200,"completion_tokens":50,"cost":0.001}}),
    )
}

async fn provider(State(f): State<Arc<Fixture>>, Json(body): Json<Value>) -> Json<Value> {
    assert_eq!(body["deterministic"], true);
    assert_eq!(body["inputs"]["company.id"], "123");
    f.provider_calls.fetch_add(1, Ordering::Relaxed);
    Json(
        json!({"ok":true,"charged_micro_usd":115,"provider_micro_usd":100,"fee_micro_usd":15,"result":{"accounts":"2025"},"result_truncated":false}),
    )
}

pub async fn server(f: Arc<Fixture>) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route("/typesafe", post(typesafe))
        .route("/chat", post(baseline))
        .route("/functions/v1/use-selection", post(provider))
        .route("/functions/v1/agent-gateway/v1/run", post(provider))
        .with_state(f);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, task)
}
