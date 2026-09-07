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
            anyhow::ensure!(
                catalog.by_capability.contains_key(&g.capability),
                "unsupported capability: {}",
                g.capability
            );
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
        anyhow::ensure!(!self.key.is_empty(), "chat model not configured");
        let response = self.http.post(&self.url).bearer_auth(&self.key)
            .timeout(Duration::from_secs(25)).json(&json!({
                "model": self.model, "temperature": 0.2, "max_tokens": 1000,
                "response_format": {"type":"json_object"},
                "messages": [
                    {"role":"system","content":include_str!("../contracts/ask-chat-prompt.txt")},
                    {"role":"user","content":json!({"surface":"ask-chat","question":question,"context":task.context_view(),"capabilities":catalog.parser_view()["capabilities"]}).to_string()}
                ]
            })).send().await?.error_for_status()?;
        let body = bounded_json(response, 32768).await?;
        let raw = body
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing chat JSON"))?;
        let reply: ChatReply = serde_json::from_str(raw)?;
        reply.validate()?;
        Ok(reply)
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
            "parser-v2.1",
        ));
        let intent=self.cache.try_get_with(key,async {
            anyhow::ensure!(!self.key.is_empty(),"intent parser not configured");
            self.calls.fetch_add(1,Ordering::Relaxed);
            let started=std::time::Instant::now();
            let system=include_str!("../contracts/parser-prompt.txt");
            let response=self.http.post(&self.url).bearer_auth(&self.key).timeout(Duration::from_secs(25)).json(&json!({
                "model":self.model,"temperature":0,"max_tokens":2200,
                "response_format":{"type":"json_object"},
                "messages":[{"role":"system","content":system},{"role":"user","content":json!({"question":question,"context":context,"catalog":catalog.parser_view()}).to_string()}]
            })).send().await?.error_for_status()?;
            let body=bounded_json(response,65536).await?;
            let raw=body.pointer("/choices/0/message/content").and_then(Value::as_str).ok_or_else(||anyhow::anyhow!("missing intent JSON"))?;
            let intent=decode_intent(raw)?; intent.validate(question,task,catalog)?;
            tracing::info!(parser_ms=started.elapsed().as_millis() as u64,"intent parsed");
            Ok::<_,anyhow::Error>(Arc::new(intent))
        }).await.map_err(|e|anyhow::anyhow!("intent invalid or unavailable: {e}"))?;
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
