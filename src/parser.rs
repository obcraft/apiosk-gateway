use crate::{
    catalog::Catalog,
    state::{digest, Task},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subject {
    pub entity_ref: Option<Uuid>,
    pub label: String,
    pub values: BTreeMap<String, Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Goal {
    pub capability: String,
    pub subject: usize,
    pub constraints: BTreeMap<String, Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    pub subjects: Vec<Subject>,
    pub goals: Vec<Goal>,
    pub answer_operations: Vec<String>,
    pub unresolved_references: Vec<String>,
}
impl Intent {
    pub fn validate(&self, question: &str, task: &Task, catalog: &Catalog) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.subjects.len() <= 8 && !self.goals.is_empty() && self.goals.len() <= 8,
            "invalid intent size"
        );
        for s in &self.subjects {
            if let Some(id) = s.entity_ref {
                anyhow::ensure!(
                    task.entities.iter().any(|e| e.entity_ref == id),
                    "unknown entity reference"
                );
            } else {
                anyhow::ensure!(
                    !s.label.is_empty()
                        && question.to_lowercase().contains(&s.label.to_lowercase()),
                    "subject label must be explicit in question"
                );
            }
            for (kind, value) in &s.values {
                let literal = match value {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    _ => anyhow::bail!("explicit values must be scalar literals"),
                };
                anyhow::ensure!(
                    !literal.is_empty()
                        && question.to_lowercase().contains(&literal.to_lowercase()),
                    "parser invented a value"
                );
                let ft = catalog
                    .vocabulary
                    .get(kind)
                    .ok_or_else(|| anyhow::anyhow!("unknown input type"))?;
                catalog_core::facts::schema::validate(&ft.value_schema, value)?;
            }
        }
        for g in &self.goals {
            anyhow::ensure!(g.subject < self.subjects.len(), "invalid subject index");
            if !catalog.by_capability.contains_key(&g.capability) {
                return Err(ModelFailure::Unsupported.into());
            }
        }
        Ok(())
    }
}

fn decode_intent(raw: &str) -> anyhow::Result<Intent> {
    let mut value: Value = serde_json::from_str(raw)?;
    let one_subject = value
        .get("subjects")
        .and_then(Value::as_array)
        .is_some_and(|subjects| subjects.len() == 1);
    if one_subject {
        if let Some(goals) = value.get_mut("goals").and_then(Value::as_array_mut) {
            for goal in goals {
                if goal.get("subject").is_some_and(Value::is_null) {
                    goal["subject"] = json!(0);
                }
            }
        }
    }
    Ok(serde_json::from_value(value)?)
}
/// App-only conversational routing; never an executable action or approval.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatReply {
    pub message: String,
    pub discover: bool,
}
impl ChatReply {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.message.trim().is_empty() && self.message.len() <= 3000,
            "invalid chat reply"
        );
        Ok(())
    }
}
/// Only these public classifications leave the model boundary. Provider
/// envelopes, JSON errors and model internals stay out of client responses.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ModelFailure {
    #[error("The question could not be processed just now. Please try again.")]
    Unavailable,
    #[error("Apiosk does not currently have a supported data source for this question. Try a different question or explore the available sources.")]
    Unsupported,
}

pub struct Parser {
    http: reqwest::Client,
    url: String,
    key: String,
    model: String,
    cache: moka::future::Cache<String, Arc<Intent>>,
    pub calls: AtomicU64,
}
impl Parser {
    pub fn new(http: reqwest::Client, url: String, key: String, model: String) -> Self {
        Self {
            http,
            url,
            key,
            model,
            cache: moka::future::Cache::builder()
                .max_capacity(1024)
                .time_to_live(Duration::from_secs(600))
                .build(),
            calls: AtomicU64::new(0),
        }
    }
    pub async fn chat(
        &self,
        question: &str,
        task: &Task,
        catalog: &Catalog,
    ) -> anyhow::Result<ChatReply> {
        let payload = json!({
            "model": self.model, "temperature": 0, "max_tokens": 4096,
            "reasoning": {"effort":"minimal", "exclude":true},
            "provider": {"require_parameters":true},
            "response_format": {"type":"json_schema", "json_schema": {
                "name":"ask_chat_reply", "strict":true, "schema": {
                    "type":"object", "additionalProperties":false,
                    "required":["message","discover"],
                    "properties":{"message":{"type":"string"},"discover":{"type":"boolean"}}
                }
            }},
            "messages": [
                {"role":"system","content":include_str!("../contracts/ask-chat-prompt.txt")},
                {"role":"user","content":json!({"surface":"ask-chat","question":question,"context":task.context_view(),"capabilities":catalog.parser_view()["capabilities"]}).to_string()}
            ]
        });
        self.completion("chat", &payload, |raw| {
            let reply: ChatReply = serde_json::from_str(raw)?;
            reply.validate()?;
            Ok(reply)
        }).await
    }

    /// Retrying interpretation is safe: this boundary cannot call a data
    /// provider, authorize a quote or execute a purchase. Both attempts share
    /// the original 25-second stage deadline.
    async fn completion<T>(
        &self,
        surface: &'static str,
        payload: &Value,
        decode: impl Fn(&str) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        if self.key.is_empty() { return Err(ModelFailure::Unavailable.into()); }
        let work = async {
            for attempt in 1..=2 {
                let response = self.http.post(&self.url).bearer_auth(&self.key)
                    .timeout(Duration::from_secs(12)).json(payload).send().await;
                let outcome: Result<(), &str> = match response {
                    Ok(response) if response.status().is_success() => {
                        match bounded_json(response, 65536).await {
                            Ok(body) => {
                                let choice = body.pointer("/choices/0");
                                let finish = choice.and_then(|c| c["finish_reason"].as_str());
                                let refused = choice.is_some_and(|c| c["message"]["refusal"].as_str().is_some_and(|v| !v.is_empty()))
                                    || finish == Some("content_filter");
                                if refused { return Err(ModelFailure::Unavailable.into()); }
                                let raw = body.pointer("/choices/0/message/content").and_then(Value::as_str)
                                    .filter(|text| !text.trim().is_empty());
                                if !body["error"].is_null() || finish == Some("length") {
                                    Err("incomplete_response")
                                } else if let Some(raw) = raw {
                                    match decode(raw) {
                                        Ok(value) => return Ok(value),
                                        Err(error) if matches!(error.downcast_ref::<ModelFailure>(), Some(ModelFailure::Unsupported)) => {
                                            return Err(ModelFailure::Unsupported.into());
                                        }
                                        Err(_) => Err("invalid_output"),
                                    }
                                } else { Err("empty_output") }
                            }
                            Err(_) => Err("invalid_envelope"),
                        }
                    }
                    Ok(response) => {
                        let status = response.status().as_u16();
                        tracing::warn!(surface, attempt, status, "model HTTP failure");
                        if status != 408 && status != 429 && status < 500 {
                            return Err(ModelFailure::Unavailable.into());
                        }
                        Err("upstream_unavailable")
                    }
                    Err(_) => Err("transport_failure"),
                };
                if let Err(reason) = outcome {
                    // No prompt, answer, reasoning or credentials in diagnostics.
                    tracing::warn!(surface, attempt, reason, "model response unavailable");
                }
            }
            Err(ModelFailure::Unavailable.into())
        };
        tokio::time::timeout(Duration::from_secs(25), work).await
            .unwrap_or_else(|_| Err(ModelFailure::Unavailable.into()))
    }
    pub async fn parse(
        &self,
        owner: Uuid,
        question: &str,
        task: &Task,
        catalog: &Catalog,
    ) -> anyhow::Result<Intent> {
        let context = task.context_view();
        let key = digest(&(
            owner,
            question.trim(),
            &context,
            &catalog.version,
            &self.model,
            "parser-v2.2",
        ));
        let intent=self.cache.try_get_with(key,async {
            self.calls.fetch_add(1,Ordering::Relaxed);
            let started=std::time::Instant::now();
            let system=include_str!("../contracts/parser-prompt.txt");
            let payload=json!({
                "model":self.model,"temperature":0,"max_tokens":4096,
                "reasoning":{"effort":"minimal","exclude":true},
                "provider":{"require_parameters":true},
                "response_format":{"type":"json_object"},
                "messages":[{"role":"system","content":system},{"role":"user","content":json!({"question":question,"context":context,"catalog":catalog.parser_view()}).to_string()}]
            });
            let intent=self.completion("intent", &payload, |raw| {
                let intent=decode_intent(raw)?;
                intent.validate(question,task,catalog)?;
                Ok(intent)
            }).await?;
            tracing::info!(parser_ms=started.elapsed().as_millis() as u64,"intent parsed");
            Ok::<_,anyhow::Error>(Arc::new(intent))
        }).await.map_err(|e| anyhow::Error::new(e.downcast_ref::<ModelFailure>().cloned().unwrap_or(ModelFailure::Unavailable)))?;
        Ok((*intent).clone())
    }
}
pub async fn bounded_json(mut response: reqwest::Response, limit: usize) -> anyhow::Result<Value> {
    if let Some(n) = response.content_length() {
        anyhow::ensure!(n <= limit as u64, "response exceeds size limit");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        anyhow::ensure!(
            bytes.len() + chunk.len() <= limit,
            "response exceeds size limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn model_fixture(responses: Vec<(u16, Value)>) -> (Parser, Arc<AtomicU64>, tokio::task::JoinHandle<()>) {
        use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
        use std::{collections::VecDeque, sync::Mutex};
        let calls = Arc::new(AtomicU64::new(0));
        let state = (Arc::new(Mutex::new(VecDeque::from(responses))), calls.clone());
        let app = Router::new().route("/", post(|State((queue, calls)): State<(Arc<Mutex<VecDeque<(u16, Value)>>>, Arc<AtomicU64>)>| async move {
            calls.fetch_add(1, Ordering::Relaxed);
            let (status, body) = queue.lock().unwrap().pop_front().expect("unexpected extra model request");
            (StatusCode::from_u16(status).unwrap(), Json(body))
        })).with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (Parser::new(reqwest::Client::new(), url, "fixture".into(), "fixture".into()), calls, server)
    }

    fn decode_chat(raw: &str) -> anyhow::Result<ChatReply> {
        let reply: ChatReply = serde_json::from_str(raw)?;
        reply.validate()?;
        Ok(reply)
    }

    #[tokio::test]
    async fn empty_truncated_and_invalid_model_output_recovers_once() {
        let valid = json!({"error":null,"choices":[{"finish_reason":"stop","message":{"content":r#"{"message":"Which period do you mean?","discover":false}"#}}]});
        for failure in [
            json!({"choices":[{"message":{"content":null,"reasoning":r#"{"message":"not the answer","discover":true}"#}}]}),
            json!({"choices":[{"finish_reason":"length","message":{"content":r#"{"message":"truncated","discover":true}"#}}]}),
            json!({"choices":[{"message":{"content":"not JSON"}}]}),
            json!({"choices":[{"message":{"content":r#"{"message":"bad types","discover":"yes"}"#}}]}),
            json!({"error":{"code":503,"message":"private upstream detail"}}),
        ] {
            let (parser, calls, server) = model_fixture(vec![(200, failure), (200, valid.clone())]).await;
            let reply = parser.completion("chat", &json!({}), decode_chat).await.unwrap();
            assert_eq!(reply.message, "Which period do you mean?");
            assert!(!reply.discover);
            assert_eq!(calls.load(Ordering::Relaxed), 2);
            server.abort();
        }
    }

    #[tokio::test]
    async fn repeated_empty_output_is_bounded_and_has_a_safe_error() {
        let empty = json!({"choices":[{"message":{"content":null}}]});
        let (parser, calls, server) = model_fixture(vec![(200, empty.clone()), (200, empty)]).await;
        let error = parser.completion("chat", &json!({}), decode_chat).await.unwrap_err();
        assert!(matches!(error.downcast_ref::<ModelFailure>(), Some(ModelFailure::Unavailable)));
        assert!(!error.to_string().contains("JSON"));
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        server.abort();
    }

    #[tokio::test]
    async fn refusals_and_permanent_http_errors_are_not_retried() {
        for response in [
            (200, json!({"choices":[{"finish_reason":"content_filter","message":{"content":null}}]})),
            (401, json!({"error":{"message":"secret upstream configuration"}})),
        ] {
            let (parser, calls, server) = model_fixture(vec![response]).await;
            assert!(parser.completion("chat", &json!({}), decode_chat).await.is_err());
            assert_eq!(calls.load(Ordering::Relaxed), 1);
            server.abort();
        }
    }

    #[test]
    fn chat_reply_is_bounded_and_cannot_authorize_or_fill_inputs() {
        let reply: ChatReply = serde_json::from_value(
            json!({"message":"Hallo! Hoe kan ik je helpen?","discover":false}),
        )
        .unwrap();
        assert!(reply.validate().is_ok());
        assert!(serde_json::from_value::<ChatReply>(
            json!({"message":"Yes","discover":true,"approved":true})
        )
        .is_err());
        assert!(ChatReply {
            message: " ".into(),
            discover: false
        }
        .validate()
        .is_err());
        assert!(ChatReply {
            message: "x".repeat(3001),
            discover: false
        }
        .validate()
        .is_err());
    }

    #[test]
    fn a_null_subject_is_repaired_only_when_there_is_one_unambiguous_subject() {
        let repaired = decode_intent(
            r#"{
            "subjects":[{"entity_ref":null,"label":"Acme","values":{}}],
            "goals":[{"capability":"company.search","subject":null,"constraints":{}}],
            "answer_operations":[],"unresolved_references":[]
        }"#,
        )
        .unwrap();
        assert_eq!(repaired.goals[0].subject, 0);
        assert!(decode_intent(r#"{
            "subjects":[{"entity_ref":null,"label":"Acme","values":{}},{"entity_ref":null,"label":"Beta","values":{}}],
            "goals":[{"capability":"company.search","subject":null,"constraints":{}}],
            "answer_operations":[],"unresolved_references":[]
        }"#).is_err());
    }
}
