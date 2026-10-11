//! Optional, explicitly approved analysis over saved source data. No source tools.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

// Retained as the identity of version-1 quotes and already paid receipts.
pub const RESOURCE: &str = "https://x402engine.app/api/llm/claude-opus-5.5";
pub const MODEL: &str = "Claude Opus 5.5";
pub const PROVIDER_CAP: i64 = 1_000_000;
pub const BUYER_CAP: i64 = 1_150_000;
pub const DEFAULT_MODEL: &str = "deepseek-v4-flash";

pub fn legacy_model() -> String {
    "claude-opus-5.5".into()
}
fn is_legacy_model(id: &String) -> bool {
    id == "claude-opus-5.5"
}
#[derive(Debug)]
pub struct Model {
    pub id: &'static str,
    pub name: &'static str,
    pub resource: &'static str,
    pub provider_cap: i64,
}
impl Model {
    pub fn max_request_bytes(&self) -> usize {
        if self.id == "gpt-6-astra" {
            7_000
        } else {
            13_200
        }
    }
    pub fn max_tokens(&self) -> usize {
        if self.id == "gpt-6-astra" {
            4_096
        } else {
            8_192
        }
    }
    pub fn effort(&self) -> &str {
        if self.id == "gpt-6-astra" {
            "low"
        } else {
            "high"
        }
    }
    pub fn buyer_cap(&self) -> i64 {
        self.provider_cap * 115 / 100
    }
}
macro_rules! model {
    ($id:literal, $name:literal, $cap:expr) => {
        Model {
            id: $id,
            name: $name,
            resource: concat!("https://x402engine.app/api/llm/", $id),
            provider_cap: $cap,
        }
    };
}
// Server-owned allowlist. Free quotes checked 2026-10-11 with the maximum
// 13,200-byte text payload, high reasoning and 8,192 total output tokens.
// A changed provider price must still fit the displayed ceiling before payment.
pub const MODELS: &[Model] = &[
    model!("deepseek-v4-flash", "DeepSeek V4 Flash", 15_000),
    model!("glm-5.3-flash", "GLM 5.3 Flash", 25_000),
    model!("minimax-m3", "MiniMax M3", 45_000),
    model!("gpt-5.6-luna", "GPT-5.6 Luna", 75_000),
    model!("deepseek-v4-pro", "DeepSeek V4 Pro", 100_000),
    model!("gemini-3.6-flash", "Gemini 3.6 Flash", 125_000),
    model!("qwen3.7-plus", "Qwen 3.7 Plus", 170_000),
    model!("qwen3.7-max", "Qwen 3.7 Max", 230_000),
    model!("claude-sonnet-5", "Claude Sonnet 5", 500_000),
    model!("gpt-6.1-sol", "GPT-6.1 Sol", 650_000),
    model!("claude-opus-5.5", "Claude Opus 5.5", PROVIDER_CAP),
    model!("gpt-6-astra", "GPT-6 Astra", 1_730_000),
];
pub fn model(id: &str) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.id == id)
}
pub fn enabled() -> bool {
    std::env::var("ASK_PAID_ANALYSIS_ENABLED").as_deref() == Ok("1")
}

pub fn enabled_for(owner: Uuid) -> bool {
    enabled()
        && std::env::var("ASK_PAID_ANALYSIS_OWNERS")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .is_none_or(|owners| {
                owners
                    .split(',')
                    .any(|id| id.trim().parse::<Uuid>().ok() == Some(owner))
            })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Quote {
    pub version: u8,
    // Keep serialization of old Opus quotes byte-for-byte compatible with their hash.
    #[serde(default = "legacy_model", skip_serializing_if = "is_legacy_model")]
    pub model_id: String,
}
impl Quote {
    pub fn for_model(id: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(model(id).is_some(), "Unknown analysis model");
        Ok(Self {
            version: 2,
            model_id: id.into(),
        })
    }
    pub fn valid(&self) -> bool {
        (self.version == 2 || (self.version == 1 && is_legacy_model(&self.model_id)))
            && self.model().is_some()
    }
    pub fn model(&self) -> Option<&'static Model> {
        model(&self.model_id)
    }
    pub fn buyer_cap(&self) -> i64 {
        self.model().map_or(0, Model::buyer_cap)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    #[serde(default = "legacy_model")]
    pub model_id: String,
    pub quote_ref: Uuid,
    pub action_id: Uuid,
    pub question: String,
    pub status: String,
    #[serde(default)]
    pub prepared: Option<crate::parser::compute_analysis::Prepared>,
}
impl Run {
    pub fn new(quote_ref: Uuid, question: String, model_id: String) -> Self {
        Self {
            model_id,
            quote_ref,
            action_id: Uuid::new_v4(),
            question,
            status: "pending".into(),
            prepared: None,
        }
    }
}
pub fn options(selected: Option<&Quote>) -> Value {
    let current = selected
        .and_then(Quote::model)
        .unwrap_or_else(|| model(DEFAULT_MODEL).unwrap());
    json!({"model_id":current.id,"model":current.name,"max_total_atomic":current.buyer_cap().to_string(),
        "models":MODELS.iter().map(|m| json!({"id":m.id,"name":m.name,"max_total_atomic":m.buyer_cap().to_string(),"description":if m.id=="gpt-6-astra" { "Compact synthesis with a smaller evidence window to keep the extra cost down." } else { "Detailed analysis of the saved evidence." }})).collect::<Vec<_>>(),
        "provider":"x402engine",
        "description":"A more detailed analysis of your saved sources. Missing source data stays unknown."})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_quotes_keep_identity_hash_and_opus_cap() {
        let old = json!({"version":1});
        let quote: Quote = serde_json::from_value(old.clone()).unwrap();
        assert!(quote.valid());
        assert_eq!(quote.model().unwrap().resource, RESOURCE);
        assert_eq!(quote.buyer_cap(), BUYER_CAP);
        assert_eq!(serde_json::to_value(quote).unwrap(), old);
        assert!(Quote::for_model("https://untrusted.example/model").is_err());
    }
    #[test]
    fn every_choice_is_unique_bounded_and_hashes_differently() {
        let mut hashes = std::collections::BTreeSet::new();
        assert!(MODELS.len() >= 10);
        for m in MODELS {
            let quote = Quote::for_model(m.id).unwrap();
            assert!(quote.valid());
            assert_eq!(quote.buyer_cap(), m.provider_cap * 115 / 100);
            assert!(m.provider_cap <= 1_730_000);
            assert!(hashes.insert(serde_json::to_string(&quote).unwrap()));
        }
    }
}
