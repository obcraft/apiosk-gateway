//! Per-trace cost counters. Unknown stays unknown: a sum is reported only
//! with a flag saying whether every response carried the field, and a field
//! no response carried is null, never 0.

use super::{label, ErrorClass, Outcome, SourceStatus};
use serde_json::{json, Value};
use std::{
    sync::atomic::{AtomicU64, Ordering::Relaxed},
    time::Duration,
};
use uuid::Uuid;

/// One HTTP attempt at a model. Only numeric usage fields and the provider
/// slug are read from the envelope; its content never reaches telemetry.
pub struct ModelAttempt<'a> {
    pub surface: &'static str,
    pub attempt: u8,
    pub model: &'a str,
    pub body: Option<&'a Value>,
    pub input_bytes: usize,
    pub output_bytes: usize,
    pub elapsed: Duration,
    pub outcome: Outcome,
    /// A fixed diagnostic code, such as `validated` or `invalid_output`.
    pub reason: &'static str,
}

/// One paid source call after the Edge hop. Money legs stay separate:
/// provider cost of goods, the Apiosk fee and what the customer was charged.
/// A leg the response did not report is `None`; a known zero is `Some(0)`.
pub struct SourceCall<'a> {
    pub task: Uuid,
    pub api_slug: &'a str,
    pub capability: &'a str,
    pub status: SourceStatus,
    pub edge_ms: Option<u64>,
    pub provider_ms: Option<u64>,
    pub charged_micro_usd: Option<i64>,
    pub provider_micro_usd: Option<i64>,
    pub fee_micro_usd: Option<i64>,
    pub bytes: Option<usize>,
    pub replayed: bool,
    /// Why the call did not succeed; `None` for a settled success.
    pub error_class: Option<ErrorClass>,
}

macro_rules! counters {
    ($($field:ident),+ $(,)?) => {
        #[derive(Default)]
        pub(crate) struct CostRecord { $($field: AtomicU64),+ }
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub(crate) struct CostSnapshot { $(pub $field: u64),+ }
        impl CostRecord {
            /// Moves the counters out, so each increment is reported exactly
            /// once however many lines one trace flushes.
            pub(crate) fn take(&self) -> CostSnapshot {
                CostSnapshot { $($field: self.$field.swap(0, Relaxed)),+ }
            }
        }
    };
}

counters!(
    model_attempts, model_calls, model_retries, model_failures, model_responses,
    usage_responses, prompt_tokens, completion_tokens,
    cached_responses, cached_tokens, reasoning_responses, reasoning_tokens,
    priced_responses, cost_nanousd, model_input_bytes, model_output_bytes,
    source_calls, source_succeeded, source_failed, source_unresolved, source_failed_prepay,
    source_refunds, refunded_micro_usd, source_retries, polls, replayed,
    charged_reported, provider_reported, fee_reported,
    customer_charged_micro_usd, provider_micro_usd, fee_micro_usd,
    db_calls, db_ms, edge_bytes, sse_events, sse_bytes,
);

fn add(counter: &AtomicU64, value: u64) {
    counter.fetch_add(value, Relaxed);
}
fn add_money(sum: &AtomicU64, reported: &AtomicU64, value: Option<i64>) {
    if let Some(value) = value.filter(|value| *value >= 0) {
        add(sum, value as u64);
        add(reported, 1);
    }
}

impl CostRecord {
    pub(crate) fn model(&self, attempt: &ModelAttempt) {
        add(&self.model_attempts, 1);
        add(if attempt.attempt <= 1 { &self.model_calls } else { &self.model_retries }, 1);
        if attempt.outcome != Outcome::Ok {
            add(&self.model_failures, 1);
        }
        add(&self.model_input_bytes, attempt.input_bytes as u64);
        add(&self.model_output_bytes, attempt.output_bytes as u64);
        let Some(body) = attempt.body else { return };
        add(&self.model_responses, 1);
        let usage = Usage::read(body);
        if let (Some(prompt), Some(completion)) = (usage.prompt, usage.completion) {
            add(&self.usage_responses, 1);
            add(&self.prompt_tokens, prompt);
            add(&self.completion_tokens, completion);
        }
        if let Some(cached) = usage.cached {
            add(&self.cached_responses, 1);
            add(&self.cached_tokens, cached);
        }
        if let Some(reasoning) = usage.reasoning {
            add(&self.reasoning_responses, 1);
            add(&self.reasoning_tokens, reasoning);
        }
        if let Some(cost) = usage.cost_nanousd {
            add(&self.priced_responses, 1);
            add(&self.cost_nanousd, cost);
        }
    }
    pub(crate) fn source(&self, call: &SourceCall) {
        add(&self.source_calls, 1);
        add(match call.status {
            SourceStatus::Succeeded => &self.source_succeeded,
            SourceStatus::Unresolved => &self.source_unresolved,
            SourceStatus::RefusedBeforeReservation => &self.source_failed_prepay,
            SourceStatus::Refunded => &self.source_refunds,
            SourceStatus::Failed => &self.source_failed,
        }, 1);
        if call.replayed {
            add(&self.replayed, 1);
        }
        add_money(&self.customer_charged_micro_usd, &self.charged_reported, call.charged_micro_usd);
        add_money(&self.provider_micro_usd, &self.provider_reported, call.provider_micro_usd);
        add_money(&self.fee_micro_usd, &self.fee_reported, call.fee_micro_usd);
        add(&self.edge_bytes, call.bytes.unwrap_or(0) as u64);
    }
    /// A call that was unresolved at dispatch and settled later by a ledger
    /// poll: its money legs become known, or it was refunded.
    pub(crate) fn settle(&self, settlement: &Settlement) {
        match settlement {
            Settlement::Charged { charged_micro_usd, provider_micro_usd, fee_micro_usd } => {
                add_money(&self.customer_charged_micro_usd, &self.charged_reported, Some(*charged_micro_usd));
                add_money(&self.provider_micro_usd, &self.provider_reported, *provider_micro_usd);
                add_money(&self.fee_micro_usd, &self.fee_reported, *fee_micro_usd);
            }
            Settlement::Refunded { refunded_micro_usd } => {
                add(&self.source_refunds, 1);
                add(&self.refunded_micro_usd, refunded_micro_usd.unwrap_or(0).max(0) as u64);
                add(&self.charged_reported, 1);
                add(&self.provider_reported, 1);
                add(&self.fee_reported, 1);
            }
        }
    }
    pub(crate) fn source_retry(&self) {
        add(&self.source_retries, 1);
    }
    pub(crate) fn poll(&self) {
        add(&self.polls, 1);
    }
    pub(crate) fn db(&self, elapsed: Duration) {
        add(&self.db_calls, 1);
        add(&self.db_ms, elapsed.as_millis() as u64);
    }
    pub(crate) fn sse(&self, bytes: usize) {
        add(&self.sse_events, 1);
        add(&self.sse_bytes, bytes as u64);
    }
}

/// How an unresolved paid call settled when the ledger was polled.
pub enum Settlement {
    Charged { charged_micro_usd: i64, provider_micro_usd: Option<i64>, fee_micro_usd: Option<i64> },
    /// Nothing is charged after a refund; the refunded amount, when known.
    Refunded { refunded_micro_usd: Option<i64> },
}

/// The numeric usage fields of an OpenAI-compatible envelope, or of an
/// endpoint that reports `input_tokens`/`output_tokens`.
pub(crate) struct Usage {
    pub prompt: Option<u64>,
    pub completion: Option<u64>,
    pub cached: Option<u64>,
    pub reasoning: Option<u64>,
    pub cost_nanousd: Option<u64>,
}
impl Usage {
    pub(crate) fn read(body: &Value) -> Self {
        let usage = &body["usage"];
        Self {
            prompt: usage["prompt_tokens"].as_u64().or_else(|| usage["input_tokens"].as_u64()),
            completion: usage["completion_tokens"].as_u64().or_else(|| usage["output_tokens"].as_u64()),
            cached: usage["prompt_tokens_details"]["cached_tokens"].as_u64(),
            reasoning: usage["completion_tokens_details"]["reasoning_tokens"].as_u64(),
            cost_nanousd: usage["cost"].as_f64().filter(|cost| cost.is_finite() && *cost >= 0.0).map(|cost| (cost * 1e9).round() as u64),
        }
    }
    pub(crate) fn json(&self) -> Value {
        json!({"prompt_tokens":self.prompt,"completion_tokens":self.completion,"cached_tokens":self.cached,
            "reasoning_tokens":self.reasoning,"cost_nanousd":self.cost_nanousd})
    }
}

/// A sum over `total` items: zero when there were none, unknown when none
/// reported it. The matching `_complete` flag says whether all reported it.
fn known(sum: u64, reported: u64, total: u64) -> Value {
    if total == 0 { json!(0) } else if reported == 0 { Value::Null } else { json!(sum) }
}

impl CostSnapshot {
    /// Anything a cost line should report. Infrastructure counters alone are
    /// carried by the request line.
    pub(crate) fn has_cost(&self) -> bool {
        self.model_attempts + self.source_calls + self.source_retries + self.polls + self.source_refunds + self.charged_reported > 0
    }
    pub(crate) fn model_json(&self) -> Value {
        let attempts = self.model_attempts;
        json!({
            "model_calls": self.model_calls, "model_attempts": self.model_attempts,
            "model_retries": self.model_retries, "model_failures": self.model_failures,
            "model_responses": self.model_responses,
            "prompt_tokens": known(self.prompt_tokens, self.usage_responses, attempts),
            "completion_tokens": known(self.completion_tokens, self.usage_responses, attempts),
            "cached_tokens": known(self.cached_tokens, self.cached_responses, attempts),
            "reasoning_tokens": known(self.reasoning_tokens, self.reasoning_responses, attempts),
            "cost_nanousd": known(self.cost_nanousd, self.priced_responses, attempts),
            "tokens_complete": self.usage_responses == self.model_attempts,
            "cached_tokens_complete": self.cached_responses == self.model_attempts,
            "reasoning_tokens_complete": self.reasoning_responses == self.model_attempts,
            "cost_complete": self.priced_responses == self.model_attempts,
            "model_input_bytes": self.model_input_bytes, "model_output_bytes": self.model_output_bytes,
        })
    }
    pub(crate) fn source_json(&self) -> Value {
        let calls = self.source_calls;
        json!({
            "source_calls": self.source_calls, "source_succeeded": self.source_succeeded,
            "source_failed": self.source_failed, "source_unresolved": self.source_unresolved,
            "source_failed_prepay": self.source_failed_prepay, "source_refunds": self.source_refunds,
            "refunded_micro_usd": self.refunded_micro_usd, "source_retries": self.source_retries,
            "polls": self.polls, "replayed": self.replayed,
            "customer_charged_micro_usd": known(self.customer_charged_micro_usd, self.charged_reported, calls),
            "provider_micro_usd": known(self.provider_micro_usd, self.provider_reported, calls),
            "fee_micro_usd": known(self.fee_micro_usd, self.fee_reported, calls),
            "charged_complete": self.charged_reported == calls,
            "provider_complete": self.provider_reported == calls,
            "fee_complete": self.fee_reported == calls,
        })
    }
    pub(crate) fn infra_json(&self) -> Value {
        json!({"db_calls": self.db_calls, "db_ms": self.db_ms,
            "edge_bytes": self.edge_bytes, "sse_events": self.sse_events, "sse_bytes": self.sse_bytes})
    }
}

pub(crate) fn model_line(attempt: &ModelAttempt) -> Value {
    let usage = attempt.body.map(Usage::read);
    let mut line = json!({
        "attempt": attempt.attempt, "model": label(attempt.model),
        "provider": attempt.body.and_then(|body| body["provider"].as_str()).map(|provider| label(&provider.to_ascii_lowercase().replace(' ', "-")).to_owned()),
        "reason": attempt.reason, "input_bytes": attempt.input_bytes, "output_bytes": attempt.output_bytes,
    });
    let usage = usage.map_or_else(|| json!({"prompt_tokens":null,"completion_tokens":null,"cached_tokens":null,"reasoning_tokens":null,"cost_nanousd":null}), |usage| usage.json());
    if let (Some(line), Some(usage)) = (line.as_object_mut(), usage.as_object()) {
        line.extend(usage.clone());
    }
    line
}

pub(crate) fn source_line(call: &SourceCall) -> Value {
    json!({
        "api_slug": label(call.api_slug), "capability": label(call.capability), "status": call.status.as_str(),
        "edge_ms": call.edge_ms, "provider_ms": call.provider_ms,
        "overhead_ms": call.edge_ms.zip(call.provider_ms).map(|(edge, provider)| edge.saturating_sub(provider)),
        "charged_micro_usd": call.charged_micro_usd, "provider_micro_usd": call.provider_micro_usd,
        "fee_micro_usd": call.fee_micro_usd, "bytes": call.bytes, "replayed": call.replayed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attempt(body: Option<&Value>, number: u8, outcome: Outcome) -> ModelAttempt<'_> {
        ModelAttempt { surface: "chat", attempt: number, model: "fixture", body, input_bytes: 100, output_bytes: 10,
            elapsed: Duration::from_millis(5), outcome, reason: "validated" }
    }

    #[test]
    fn unknown_usage_is_null_and_marks_the_record_incomplete() {
        let record = CostRecord::default();
        let priced = json!({"usage":{"prompt_tokens":10,"completion_tokens":2,"cost":0.000001,
            "prompt_tokens_details":{"cached_tokens":8}}});
        let bare = json!({"choices":[]});
        record.model(&attempt(Some(&priced), 1, Outcome::Ok));
        record.model(&attempt(Some(&bare), 2, Outcome::Failed(ErrorClass::Validation)));
        let model = record.take().model_json();
        assert_eq!(model["model_calls"], 1);
        assert_eq!(model["model_retries"], 1);
        assert_eq!(model["model_failures"], 1);
        assert_eq!(model["prompt_tokens"], 10);
        assert_eq!(model["cached_tokens"], 8);
        assert_eq!(model["cost_nanousd"], 1000);
        assert_eq!(model["reasoning_tokens"], Value::Null, "no response reported reasoning tokens");
        assert_eq!(model["tokens_complete"], false);
        assert_eq!(model["cost_complete"], false);
        assert_eq!(record.take(), CostSnapshot::default(), "taking moves the counters out");
    }

    #[test]
    fn a_transport_failure_has_unknown_tokens_but_counts_as_an_attempt() {
        let record = CostRecord::default();
        record.model(&attempt(None, 1, Outcome::Failed(ErrorClass::Transport)));
        let snapshot = record.take();
        assert!(snapshot.has_cost());
        let model = snapshot.model_json();
        assert_eq!(model["model_attempts"], 1);
        assert_eq!(model["prompt_tokens"], Value::Null, "a lost response may still have been billed");
        assert_eq!(model["tokens_complete"], false);
    }

    #[test]
    fn money_legs_stay_separate_and_missing_legs_stay_unknown() {
        let record = CostRecord::default();
        let call = |status, charged, provider, fee| SourceCall { task: Uuid::nil(), api_slug: "kvk", capability: "company.profile",
            status, edge_ms: Some(900), provider_ms: Some(700), charged_micro_usd: charged, provider_micro_usd: provider,
            fee_micro_usd: fee, bytes: Some(2048), replayed: false, error_class: None };
        record.source(&call(SourceStatus::Succeeded, Some(54_348), Some(47_259), Some(7_089)));
        record.source(&call(SourceStatus::Unresolved, None, None, None));
        record.source(&call(SourceStatus::Unresolved, None, None, None));
        record.settle(&Settlement::Refunded { refunded_micro_usd: Some(12) });
        let sources = record.take().source_json();
        assert_eq!(sources["customer_charged_micro_usd"], 54_348);
        assert_eq!(sources["provider_micro_usd"], 47_259);
        assert_eq!(sources["fee_micro_usd"], 7_089);
        assert_eq!(sources["source_unresolved"], 2);
        assert_eq!(sources["source_refunds"], 1);
        assert_eq!(sources["refunded_micro_usd"], 12);
        assert_eq!(sources["charged_complete"], false, "one unresolved call's charge is still unknown");
        record.source(&call(SourceStatus::Unresolved, None, None, None));
        record.settle(&Settlement::Charged { charged_micro_usd: 100, provider_micro_usd: Some(87), fee_micro_usd: Some(13) });
        let settled = record.take().source_json();
        assert_eq!(settled["customer_charged_micro_usd"], 100);
        assert_eq!(settled["charged_complete"], true, "the poll made the charge known");
        assert_eq!(source_line(&call(SourceStatus::Succeeded, None, None, None))["overhead_ms"], 200);
    }

    #[test]
    fn model_lines_carry_only_numbers_and_slugs() {
        let body = json!({"provider":"Google Vertex","usage":{"prompt_tokens":3,"completion_tokens":1},
            "choices":[{"message":{"content":"Quokka Canary 4f1c B.V."}}]});
        let line = model_line(&attempt(Some(&body), 1, Outcome::Ok)).to_string();
        assert!(!line.contains("Quokka"));
        assert!(line.contains("\"provider\":\"google-vertex\""));
        assert!(line.contains("\"cached_tokens\":null"));
        let odd = json!({"provider":"Provider (EU), \"beta\""});
        assert_eq!(model_line(&attempt(Some(&odd), 1, Outcome::Ok))["provider"], "other");
    }
}
