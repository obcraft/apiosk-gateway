use serde::Serialize;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[derive(Default)]
pub struct Usage {
    requests: AtomicU64,
    usage_responses: AtomicU64,
    priced_responses: AtomicU64,
    input_tokens: AtomicU64,
    output_tokens: AtomicU64,
    cost_nanodollars: AtomicU64,
}

#[derive(Clone, Debug, Serialize)]
pub struct UsageSnapshot {
    pub requests: u64,
    pub usage_responses: u64,
    pub priced_responses: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub known_cost_usd: f64,
    pub cost_complete: bool,
}

impl Default for UsageSnapshot {
    fn default() -> Self {
        Self {
            requests: 0,
            usage_responses: 0,
            priced_responses: 0,
            input_tokens: 0,
            output_tokens: 0,
            known_cost_usd: 0.0,
            cost_complete: true,
        }
    }
}

impl Usage {
    pub fn request(&self) {
        self.requests.fetch_add(1, Relaxed);
    }
    fn record(&self, input: Option<u64>, output: Option<u64>, cost: Option<f64>) {
        if let (Some(input), Some(output)) = (input, output) {
            self.usage_responses.fetch_add(1, Relaxed);
            self.input_tokens.fetch_add(input, Relaxed);
            self.output_tokens.fetch_add(output, Relaxed);
        }
        if let Some(cost) = cost.filter(|cost| cost.is_finite() && *cost >= 0.0) {
            self.priced_responses.fetch_add(1, Relaxed);
            self.cost_nanodollars
                .fetch_add((cost * 1e9).round() as u64, Relaxed);
        }
    }
    pub fn completion(&self, body: &Value) {
        self.record(
            body["usage"]["prompt_tokens"].as_u64(),
            body["usage"]["completion_tokens"].as_u64(),
            body["usage"]["cost"].as_f64(),
        );
    }
    pub fn typesafe(&self, body: &Value) {
        let input = body["usage"]["input_tokens"].as_u64();
        // Published rate for this pinned version; other versions remain unpriced.
        let cost = (body["model"] == "jev-1.13.0")
            .then(|| input.map(|tokens| tokens as f64 * 0.042 / 1e6))
            .flatten();
        self.record(input, body["usage"]["output_tokens"].as_u64(), cost);
    }
    pub fn snapshot(&self) -> UsageSnapshot {
        let requests = self.requests.load(Relaxed);
        let priced = self.priced_responses.load(Relaxed);
        UsageSnapshot {
            requests,
            usage_responses: self.usage_responses.load(Relaxed),
            priced_responses: priced,
            input_tokens: self.input_tokens.load(Relaxed),
            output_tokens: self.output_tokens.load(Relaxed),
            known_cost_usd: self.cost_nanodollars.load(Relaxed) as f64 / 1e9,
            cost_complete: requests == priced,
        }
    }
}
