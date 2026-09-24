//! Durable cost rows, one per question segment, in `gateway_v2.request_costs`.
//! Off unless `TELEMETRY_PERSIST=1`: logs stay the primary sink, rows give a
//! production baseline that outlives log retention. A row is inserted once and
//! never updated; a failed insert is logged with a fixed message and dropped.

use super::{cost::CostSnapshot, spawn, Cache, Milestone, MilestoneFields, QueryClass, Segment, SegmentOutcome, Trace};
use serde_json::{json, Map, Value};
use sqlx::PgPool;
use std::{sync::OnceLock, time::Duration};
use uuid::Uuid;

pub fn enabled() -> bool {
    std::env::var("TELEMETRY_PERSIST").as_deref() == Ok("1")
}

/// What the trace learned about its segment from milestones.
#[derive(Clone, Debug, Default)]
pub(crate) struct Summary {
    outcome: Option<SegmentOutcome>,
    fully_answered: bool,
    sources: Option<usize>,
    cap_micro_usd: Option<i64>,
}
impl Summary {
    pub(crate) fn record(&mut self, milestone: Milestone, details: &MilestoneFields) {
        self.sources = details.sources.or(self.sources);
        self.cap_micro_usd = details.cap_micro_usd.or(self.cap_micro_usd);
        let outcome = match milestone {
            Milestone::PriceReady => Some(SegmentOutcome::PriceReady),
            Milestone::NeedsInput => Some(SegmentOutcome::NeedsInput),
            Milestone::Terminal => Some(match details.status {
                Some("unsupported") => SegmentOutcome::Unsupported,
                Some("cancelled") => SegmentOutcome::Cancelled,
                Some("needs_input" | "needs_selection") => SegmentOutcome::NeedsInput,
                _ => SegmentOutcome::Failed,
            }),
            Milestone::AnswerComplete => {
                self.fully_answered = details.fully_answered == Some(true);
                Some(match details.answer_status {
                    Some("completed") => SegmentOutcome::Answered,
                    Some("unavailable") => SegmentOutcome::Unavailable,
                    _ => SegmentOutcome::Partial,
                })
            }
            _ => None,
        };
        self.outcome = outcome.or(self.outcome);
    }
}

/// Written columns; `created_at`, `expires_at` and `schema_version` keep their defaults.
const COLUMNS: [&str; 36] = [
    "id", "trace_id", "segment", "task_id", "quote_ref", "surface", "query_class", "source_count", "cache_state",
    "outcome", "fully_answered", "duration_ms", "model_calls", "model_attempts", "prompt_tokens", "completion_tokens",
    "cached_prompt_tokens", "reasoning_tokens", "model_cost_nanousd", "model_cost_complete", "source_calls",
    "source_retries", "source_refunds", "source_unresolved", "provider_micro_usd", "fee_micro_usd",
    "customer_charged_micro_usd", "customer_cap_micro_usd", "refunded_micro_usd", "db_calls", "db_ms", "bytes_in",
    "bytes_out", "model_bytes", "edge_bytes", "sse_bytes",
];

fn insert_sql() -> &'static str {
    static SQL: OnceLock<String> = OnceLock::new();
    SQL.get_or_init(|| {
        let columns = COLUMNS.join(",");
        format!("INSERT INTO gateway_v2.request_costs({columns}) SELECT {columns} FROM jsonb_populate_record(NULL::gateway_v2.request_costs,$1) ON CONFLICT DO NOTHING")
    })
}

/// The cache state a budget compares: warm only when the catalog answered
/// from memory; unknown when the segment never read it.
fn cache_state(trace: &Trace) -> &'static str {
    match trace.catalog_cache() {
        Some(Cache::Hit) => "warm",
        Some(_) => "cold",
        None => "unknown",
    }
}

/// The row for one finished segment, or none without a task or an outcome.
pub(crate) fn row(trace: &Trace, segment: Segment, snapshot: &CostSnapshot, duration: Duration, bytes: Option<(Option<u64>, Option<u64>)>) -> Option<Value> {
    let task = trace.task()?;
    let summary = trace.summary.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let outcome = summary.outcome?;
    let (model, sources) = (snapshot.model_json(), snapshot.source_json());
    let (bytes_in, bytes_out) = bytes.unwrap_or((None, None));
    let mut row = Map::new();
    let mut put = |key: &str, value: Value| {
        row.insert(key.to_owned(), value);
    };
    put("id", json!(Uuid::new_v4()));
    put("trace_id", json!(trace.id));
    put("segment", json!(segment.as_str()));
    put("task_id", json!(task));
    put("quote_ref", json!(trace.quote()));
    put("surface", json!(trace.surface().as_str()));
    put("query_class", json!(trace.query_class().unwrap_or(QueryClass::Unclassified).as_str()));
    put("source_count", json!(summary.sources));
    put("cache_state", json!(cache_state(trace)));
    put("outcome", json!(outcome.as_str()));
    put("fully_answered", json!(summary.fully_answered));
    put("duration_ms", json!(duration.as_millis() as u64));
    for key in ["model_calls", "model_attempts", "prompt_tokens", "completion_tokens", "reasoning_tokens"] {
        put(key, model[key].clone());
    }
    put("cached_prompt_tokens", model["cached_tokens"].clone());
    put("model_cost_nanousd", model["cost_nanousd"].clone());
    put("model_cost_complete", model["cost_complete"].clone());
    for key in ["source_calls", "source_retries", "source_refunds", "source_unresolved", "provider_micro_usd", "fee_micro_usd",
        "customer_charged_micro_usd", "refunded_micro_usd"] {
        put(key, sources[key].clone());
    }
    put("customer_cap_micro_usd", json!(summary.cap_micro_usd));
    put("db_calls", json!(snapshot.db_calls));
    put("db_ms", json!(snapshot.db_ms));
    put("bytes_in", json!(bytes_in));
    put("bytes_out", json!(bytes_out));
    put("model_bytes", json!(snapshot.model_input_bytes + snapshot.model_output_bytes));
    put("edge_bytes", json!(snapshot.edge_bytes));
    put("sse_bytes", json!(snapshot.sse_bytes));
    Some(Value::Object(row))
}

/// Inserts the segment's row in the background when persistence is on.
pub(crate) fn record(pool: &PgPool, trace: &Trace, segment: Segment, snapshot: &CostSnapshot, duration: Duration, bytes: Option<(Option<u64>, Option<u64>)>) {
    if !enabled() {
        return;
    }
    let Some(row) = row(trace, segment, snapshot, duration, bytes) else { return };
    let pool = pool.clone();
    spawn(async move {
        if sqlx::query(insert_sql()).bind(row).execute(&pool).await.is_err() {
            tracing::warn!("request cost row not recorded");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::{cost::CostRecord, Origin, Surface};

    #[test]
    fn a_row_has_exactly_the_table_columns_and_null_for_the_unmeasured() {
        let trace = Trace::new(Uuid::new_v4(), Origin::Client);
        let snapshot = CostRecord::default().take();
        assert!(row(&trace, Segment::Question, &snapshot, Duration::from_millis(5), None).is_none(), "no task yet");
        *trace.task.lock().unwrap() = Some(Uuid::new_v4());
        assert!(row(&trace, Segment::Question, &snapshot, Duration::from_millis(5), None).is_none(), "no outcome yet");
        trace.surface.set(Surface::ALL, Surface::App);
        trace.summary.lock().unwrap().record(Milestone::PriceReady, &MilestoneFields { sources: Some(2), cap_micro_usd: Some(138), ..Default::default() });
        let row = row(&trace, Segment::Question, &snapshot, Duration::from_millis(5), Some((Some(120), None))).unwrap();
        let keys: Vec<_> = row.as_object().unwrap().keys().cloned().collect();
        let mut columns: Vec<_> = COLUMNS.iter().map(|column| column.to_string()).collect();
        columns.sort();
        assert_eq!(keys, columns);
        assert_eq!(row["outcome"], "price_ready");
        assert_eq!(row["query_class"], "unclassified");
        assert_eq!(row["cache_state"], "unknown");
        assert_eq!(row["customer_cap_micro_usd"], 138);
        assert_eq!(row["bytes_in"], 120);
        assert_eq!(row["bytes_out"], Value::Null, "unmeasured, not zero");
        assert_eq!(row["model_calls"], 0);
    }

    #[test]
    fn an_answer_decides_the_execution_outcome_and_completeness() {
        let mut summary = Summary::default();
        summary.record(Milestone::Approved, &MilestoneFields { cap_micro_usd: Some(500), ..Default::default() });
        summary.record(Milestone::AnswerComplete, &MilestoneFields { answer_status: Some("completed"), fully_answered: Some(true), ..Default::default() });
        assert_eq!(summary.outcome, Some(SegmentOutcome::Answered));
        assert!(summary.fully_answered);
        assert_eq!(summary.cap_micro_usd, Some(500));
        summary.record(Milestone::Terminal, &MilestoneFields { status: Some("cancelled"), ..Default::default() });
        assert_eq!(summary.outcome, Some(SegmentOutcome::Cancelled));
    }
}
