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
            let intent:Intent=serde_json::from_str(raw)?; intent.validate(question,task,catalog)?;
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
