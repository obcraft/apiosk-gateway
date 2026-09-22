//! Bounded semantic decisions. Code owns values, plans, prices and evidence.
mod answer;
mod intent;
pub mod usage;

pub use intent::intent_request;
pub use usage::{Usage, UsageSnapshot};

use crate::{catalog::Catalog, parser::Intent, state::Task};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Shadow,
    On,
}

/// No credentials in Debug output or telemetry. Environment configuration is
/// opt-in; tests and evaluations can inject a loopback endpoint explicitly.
#[derive(Clone)]
pub struct Config {
    pub mode: Mode,
    pub url: String,
    pub api_key: String,
    pub model: String,
    pub timeout: Duration,
    pub min_probability: f64,
    pub min_confidence: f64,
    pub verification_probability: f64,
}

impl Config {
    pub fn new(api_key: String, mode: Mode) -> Self {
        Self {
            mode,
            api_key,
            url: "https://api.typesafe.ai/v1/systemone".into(),
            model: "jev-1.13.0".into(),
            timeout: Duration::from_millis(2000),
            min_probability: 0.95,
            min_confidence: 0.90,
            verification_probability: 0.99,
        }
    }

    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let mode = match std::env::var("TYPESAFE_MODE")
            .unwrap_or_else(|_| "off".into())
            .as_str()
        {
            "off" => return Ok(None),
            "shadow" => Mode::Shadow,
            "on" => Mode::On,
            _ => anyhow::bail!("TYPESAFE_MODE must be off, shadow or on"),
        };
        let mut config = Self::new(std::env::var("TYPESAFE_API_KEY").unwrap_or_default(), mode);
        if let Ok(model) = std::env::var("TYPESAFE_MODEL") {
            config.model = model;
        }
        if let Ok(timeout) = std::env::var("TYPESAFE_TIMEOUT_MS") {
            config.timeout = Duration::from_millis(timeout.parse()?);
        }
        config.validate()?;
        Ok(Some(config))
    }

    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.api_key.trim().is_empty(),
            "TYPESAFE_API_KEY is required when TypeSafe is enabled"
        );
        anyhow::ensure!(!self.model.trim().is_empty(), "empty TypeSafe model");
        anyhow::ensure!(
            (250..=5000).contains(&self.timeout.as_millis()),
            "TypeSafe deadline must be 250-5000 ms"
        );
        for threshold in [
            self.min_probability,
            self.min_confidence,
            self.verification_probability,
        ] {
            anyhow::ensure!(
                threshold.is_finite() && (0.5..=1.0).contains(&threshold),
                "invalid TypeSafe threshold"
            );
        }
        let url = url::Url::parse(&self.url)?;
        anyhow::ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "invalid TypeSafe URL"
        );
        anyhow::ensure!(
            url.scheme() == "https"
                || (url.scheme() == "http"
                    && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))),
            "TypeSafe requires HTTPS outside loopback"
        );
        Ok(())
    }
}

pub struct Client {
    http: reqwest::Client,
    config: Config,
    pub usage: Usage,
}

pub struct IntentDecision {
    pub intent: Intent,
    pub message: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Answer {
    Choice {
        choice: String,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    Noul {
        noul: f64,
    },
}

struct Answers(BTreeMap<String, Answer>);

impl Answers {
    fn choice(&self, name: &str, config: &Config) -> anyhow::Result<&str> {
        let Some(Answer::Choice {
            choice,
            probabilities,
            confidence,
        }) = self.0.get(name)
        else {
            anyhow::bail!("missing_choice");
        };
        anyhow::ensure!(
            *confidence >= config.min_confidence && probabilities[choice] >= config.min_probability,
            "uncertain_choice: {name} (probability {:.4}, confidence {:.4})",
            probabilities[choice],
            confidence
        );
        Ok(choice)
    }
    fn yes(&self, name: &str, threshold: f64) -> anyhow::Result<()> {
        let Some(Answer::Noul { noul }) = self.0.get(name) else {
            anyhow::bail!("missing_noul: {name}");
        };
        anyhow::ensure!(
            *noul >= threshold,
            "uncertain_judgment: {name} (probability {noul:.4}, requires {threshold:.4})"
        );
        Ok(())
    }
}

fn choice(instructions: &str, criteria: Value) -> Value {
    json!({"type":"choice", "instructions":instructions, "criteria":criteria})
}
fn noul(instructions: &str) -> Value {
    json!({"type":"noul", "instructions":instructions})
}

fn validate_answers(body: &Value, questions: &Value) -> anyhow::Result<Answers> {
    let answers: BTreeMap<String, Answer> = serde_json::from_value(body["answers"].clone())?;
    let questions = questions
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("invalid_questions"))?;
    anyhow::ensure!(answers.len() == questions.len(), "incomplete_answers");
    let probability = |v: f64| v.is_finite() && (0.0..=1.0).contains(&v);
    for (name, question) in questions {
        match answers.get(name) {
            Some(Answer::Choice {
                choice,
                probabilities,
                confidence,
            }) => {
                let criteria = question["criteria"]
                    .as_object()
                    .ok_or_else(|| anyhow::anyhow!("invalid_choice"))?;
                anyhow::ensure!(
                    question["type"] == "choice"
                        && probability(*confidence)
                        && probabilities.len() == criteria.len()
                        && criteria.contains_key(choice)
                        && probabilities.keys().all(|key| criteria.contains_key(key))
                        && probabilities.values().all(|value| probability(*value))
                        // Live Jev responses round probabilities (observed sum
                        // 0.99). Allow small serialization loss, never rescale
                        // a selected probability to pass acceptance thresholds.
                        && (probabilities.values().sum::<f64>() - 1.0).abs() <= 0.020_000_001
                        && probabilities
                            .values()
                            .all(|value| *value <= probabilities[choice] + 1e-9),
                    "invalid_distribution: {name} (options {}/{}, sum {:.6}, selected_known {}, confidence {:.6}, selected_probability {:?}, max_probability {:.6})",
                    probabilities.len(), criteria.len(), probabilities.values().sum::<f64>(), criteria.contains_key(choice), confidence, probabilities.get(choice), probabilities.values().copied().fold(0.0_f64, f64::max)
                );
            }
            Some(Answer::Noul { noul }) => anyhow::ensure!(
                question["type"] == "noul" && probability(*noul),
                "invalid_noul"
            ),
            None => anyhow::bail!("missing_answer"),
        }
    }
    Ok(Answers(answers))
}

#[cfg(test)]
mod response_tests {
    use super::*;

    #[test]
    fn rounded_distribution_preserves_raw_acceptance_probability() {
        let questions = json!({"pick":choice("Pick", json!({"a":"A","b":"B"}))});
        let body = json!({"answers":{"pick":{"type":"choice","choice":"a","confidence":0.97,"probabilities":{"a":0.97,"b":0.02}}}});
        let answers = validate_answers(&body, &questions).unwrap();
        assert_eq!(
            answers
                .choice("pick", &Config::new("fixture".into(), Mode::On))
                .unwrap(),
            "a"
        );
        let mut uncertain = body.clone();
        uncertain["answers"]["pick"]["probabilities"] = json!({"a":0.94,"b":0.05});
        assert!(validate_answers(&uncertain, &questions)
            .unwrap()
            .choice("pick", &Config::new("fixture".into(), Mode::On))
            .is_err());
        for probabilities in [
            json!({"a":0.7,"b":0.1}),
            json!({"a":1.01,"b":-0.02}),
            json!({"a":0.1,"b":0.9}),
            json!({"a":0.97,"unknown":0.02}),
        ] {
            let mut invalid = body.clone();
            invalid["answers"]["pick"]["probabilities"] = probabilities;
            assert!(validate_answers(&invalid, &questions).is_err());
        }
    }
}

impl Client {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        config.validate()?;
        Ok(Self {
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            config,
            usage: Usage::default(),
        })
    }
    pub fn mode(&self) -> Mode {
        self.config.mode
    }
    pub fn fingerprint(&self) -> String {
        crate::state::digest(&(
            "typesafe-v1",
            self.config.mode,
            &self.config.model,
            self.config.min_probability,
            self.config.min_confidence,
            self.config.verification_probability,
        ))
    }

    async fn evaluate(&self, surface: &'static str, request: Value) -> anyhow::Result<Answers> {
        let payload = json!({"model":self.config.model,"state":request["state"],"questions":request["questions"]});
        // Byte limits are conservative transport bounds, not tokenizer estimates.
        anyhow::ensure!(
            serde_json::to_vec(&payload)?.len() <= 96 * 1024,
            "request_too_large"
        );
        let started = std::time::Instant::now();
        self.usage.request();
        let response = self
            .http
            .post(&self.config.url)
            .bearer_auth(&self.config.api_key)
            .timeout(self.config.timeout)
            .json(&payload)
            .send()
            .await?;
        anyhow::ensure!(
            response.status().is_success(),
            "typesafe_http_status: {}",
            response.status().as_u16()
        );
        let body = crate::parser::bounded_json(response, 128 * 1024).await?;
        self.usage.typesafe(&body);
        let answers = validate_answers(&body, &payload["questions"])?;
        tracing::info!(
            surface,
            model = body["model"].as_str().unwrap_or_default(),
            model_ms = started.elapsed().as_millis() as u64,
            input_tokens = body["usage"]["input_tokens"].as_u64(),
            output_tokens = body["usage"]["output_tokens"].as_u64(),
            "typesafe response validated"
        );
        Ok(answers)
    }

    pub async fn parse(
        &self,
        question: &str,
        task: &Task,
        catalog: &Catalog,
    ) -> Option<IntentDecision> {
        self.parse_detailed(question, task, catalog).await.ok()
    }

    /// Keep the production fallback API, but expose why a standalone comparison
    /// was rejected. HTTP response bodies and credentials are never included.
    pub async fn parse_detailed(
        &self,
        question: &str,
        task: &Task,
        catalog: &Catalog,
    ) -> anyhow::Result<IntentDecision> {
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            self.config.timeout,
            self.parse_inner(question, task, catalog),
        )
        .await;
        let intent = result
            .unwrap_or_else(|_| Err(anyhow::anyhow!("typesafe_deadline")))
            .map_err(|error| {
                if error
                    .downcast_ref::<reqwest::Error>()
                    .is_some_and(reqwest::Error::is_timeout)
                {
                    anyhow::anyhow!("typesafe_deadline")
                } else {
                    error
                }
            });
        tracing::info!(
            surface = "intent",
            accepted = intent.is_ok(),
            stage_ms = started.elapsed().as_millis() as u64,
            "typesafe decision completed"
        );
        intent
    }

    pub async fn analyze(&self, operations: &Value, sources: &[Value]) -> Option<Value> {
        let started = std::time::Instant::now();
        let result =
            tokio::time::timeout(self.config.timeout, self.analyze_inner(operations, sources))
                .await;
        let answer = result.ok().and_then(Result::ok);
        tracing::info!(
            surface = "analysis",
            accepted = answer.is_some(),
            stage_ms = started.elapsed().as_millis() as u64,
            "typesafe decision completed"
        );
        answer
    }
}
