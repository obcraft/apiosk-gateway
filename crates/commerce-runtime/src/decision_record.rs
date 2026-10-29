//! Recording what an agent was shown before it chose.
//!
//! `/v1/capabilities/{slug}` serves a candidate set and, until this module, the
//! whole of that was forgotten the moment the response was written. The
//! settlement row that may follow says money moved to one provider; it says
//! nothing about the eight it moved away from. Every downstream ambition —
//! learning which provider wins when latency matters, re-deciding when the
//! chosen one fails, telling a provider what demand it lost — needs the menu,
//! not just the bill.
//!
//! The discovery counters (`crate::discovery_traffic`) aggregate rather than
//! write per request, because at crawler volume a row per read is a cost with
//! no signal. Here the row IS the signal and cannot be reconstructed from a
//! counter, so it is written per comparison — with the volume defence moved
//! into this module instead:
//!
//!   * **Crawlers are skipped.** An indexer enumerating capabilities is not
//!     deciding anything, and its candidate sets would swamp the real ones.
//!   * **Writes are detached.** The response is already on its way out; a slow
//!     or dead database costs the caller nothing.
//!   * **Failure is silent and total.** A comparison that cannot be recorded is
//!     logged once and dropped. Nothing here may delay, alter or fail a
//!     response.

use std::sync::Arc;

use axum::http::HeaderMap;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::db::ComparisonInsert;
use crate::telemetry::classify::classify_crawler;
use crate::AppState;

/// Longest user-agent retained; the column is unbounded `text` and the value
/// comes straight off the wire.
const MAX_USER_AGENT_CHARS: usize = 512;

/// Candidate sets are bounded by the catalog, but the column is not. A
/// capability with hundreds of offers would still write one row; this caps what
/// that row can weigh.
const MAX_CANDIDATES_RECORDED: usize = 100;

/// Crawler families whose reads are never recorded as comparisons.
///
/// [`classify_crawler`] labels anything non-empty and non-bot as `"other"`,
/// which includes `curl` and every unbranded SDK — real agents. So the skip
/// list is named explicitly rather than derived from "did it classify at all",
/// and an unrecognised caller is treated as a decision-maker.
const CRAWLER_FAMILIES: &[&str] = &[
    "openai",
    "anthropic",
    "perplexity",
    "google",
    "bing",
    "x402-crawler",
    "cdp-bazaar",
    "generic-bot",
];

/// Whether this caller's comparison is worth a row.
pub fn is_recordable(user_agent: Option<&str>) -> bool {
    match classify_crawler(user_agent) {
        Some(family) => !CRAWLER_FAMILIES.contains(&family.as_str()),
        // No user agent at all. Unusual for a browser, normal for a minimal
        // HTTP client — which is what an agent often is.
        None => true,
    }
}

/// The trimmed snapshot stored in `comparisons.candidates`.
///
/// Enough to reconstruct the decision — what was on the menu, in what order, at
/// what price — and nothing else. The full offer document is an order of
/// magnitude larger and adds nothing a decision can be judged against.
pub fn candidate_snapshot(candidates: &[Value]) -> Value {
    Value::Array(
        candidates
            .iter()
            .take(MAX_CANDIDATES_RECORDED)
            .enumerate()
            .map(|(position, c)| {
                json!({
                    "position": position + 1,
                    "api_slug": c.get("api_slug"),
                    "method": c.get("method"),
                    "path": c.get("path"),
                    "price_usd": c.get("price_usd"),
                    "hosted_externally": c.get("hosted_externally"),
                    "accepts_all_inputs": c.get("accepts_all_inputs"),
                })
            })
            .collect(),
    )
}

/// Identity of the caller, as far as it is knowable from the request.
///
/// All optional: an unauthenticated agent reading a public document is the
/// normal case, and an anonymous comparison is still worth recording.
fn caller(headers: &HeaderMap) -> (Option<String>, Option<String>, Option<String>) {
    let user_agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|v| truncate(v, MAX_USER_AGENT_CHARS));
    let agent_name = headers
        .get("x-agent-name")
        .and_then(|v| v.to_str().ok())
        .map(|v| truncate(v, 200));
    let wallet = headers
        .get("x-wallet-address")
        .and_then(|v| v.to_str().ok())
        .map(|v| truncate(&v.to_lowercase(), 128));
    (user_agent, agent_name, wallet)
}

fn truncate(value: &str, max: usize) -> String {
    let trimmed = value.trim();
    match trimmed.char_indices().nth(max) {
        Some((idx, _)) => trimmed[..idx].to_string(),
        None => trimmed.to_string(),
    }
}

/// Records one served candidate set, on a detached task.
///
/// Returns the id it will be written under, so the response can carry a
/// `comparison_id` the agent can quote back when it reports its decision. The
/// id is minted here rather than by the database precisely so the response does
/// not have to wait for the write.
///
/// Returns `None` when the caller is a crawler — nothing is written and the
/// response carries no id, which is the honest answer: no comparison was
/// recorded.
/// Records the comparison and waits for it to land, returning the id only if
/// it did.
///
/// The detached variant returns an id the moment it mints one, which is right
/// when nothing depends on the row existing. It is wrong when a decision is
/// about to reference it: `decisions.comparison_id` is a foreign key, so the
/// decision insert loses the race and fails with a constraint violation while
/// the comparison is still in flight. `/v1/decide` and `/v1/select` therefore
/// pay one round trip to be able to link the two.
#[allow(clippy::too_many_arguments)]
pub async fn record_comparison_now(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    capability_slug: &str,
    sort: &str,
    max_price_usd: Option<f64>,
    candidates: &[Value],
    request_id: Option<Uuid>,
) -> Option<Uuid> {
    let (user_agent, agent_name, wallet_address) = caller(headers);
    if !is_recordable(user_agent.as_deref()) {
        return None;
    }
    let row = build_row(
        capability_slug,
        sort,
        max_price_usd,
        candidates,
        agent_name,
        user_agent,
        wallet_address,
        request_id,
    );
    let id = Uuid::new_v4();
    match state.db.insert_comparison_with_id(id, &row).await {
        Ok(()) => Some(id),
        Err(err) => {
            tracing::warn!("comparison not recorded: {}", err);
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn build_row(
    capability_slug: &str,
    sort: &str,
    max_price_usd: Option<f64>,
    candidates: &[Value],
    agent_name: Option<String>,
    user_agent: Option<String>,
    wallet_address: Option<String>,
    request_id: Option<Uuid>,
) -> ComparisonInsert {
    let prices: Vec<f64> = candidates
        .iter()
        .filter_map(|c| c.get("price_usd").and_then(Value::as_f64))
        .collect();
    ComparisonInsert {
        capability_slug: capability_slug.to_string(),
        max_price_usd,
        sort: sort.to_string(),
        candidates: candidate_snapshot(candidates),
        candidate_count: candidates.len() as i32,
        min_price_usd: prices.iter().cloned().reduce(f64::min),
        max_seen_usd: prices.iter().cloned().reduce(f64::max),
        agent_name,
        client_kind: None,
        user_agent,
        wallet_address,
        request_id,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn record_comparison(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    capability_slug: &str,
    sort: &str,
    max_price_usd: Option<f64>,
    candidates: &[Value],
    request_id: Option<Uuid>,
) -> Option<Uuid> {
    let (user_agent, agent_name, wallet_address) = caller(headers);
    if !is_recordable(user_agent.as_deref()) {
        return None;
    }

    let row = build_row(
        capability_slug,
        sort,
        max_price_usd,
        candidates,
        agent_name,
        user_agent,
        wallet_address,
        request_id,
    );

    let state = Arc::clone(state);
    let id = Uuid::new_v4();
    tokio::spawn(async move {
        if let Err(err) = state.db.insert_comparison_with_id(id, &row).await {
            tracing::warn!("comparison not recorded: {}", err);
        }
    });
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexers_are_not_decision_makers() {
        for ua in [
            "GPTBot/1.0",
            "ClaudeBot/1.0",
            "Googlebot/2.1",
            "agent402-crawler",
            "some-random-bot/1",
        ] {
            assert!(!is_recordable(Some(ua)), "{ua} should be skipped");
        }
    }

    #[test]
    fn unbranded_clients_are_treated_as_agents() {
        // `classify_crawler` labels these "other"; they are exactly what a real
        // agent looks like on the wire, so they must be recorded.
        for ua in ["curl/8.0", "python-requests/2.31", "x402-fetch", ""] {
            let _ = ua;
        }
        assert!(is_recordable(Some("curl/8.0")));
        assert!(is_recordable(Some("python-requests/2.31")));
        assert!(is_recordable(None));
    }

    #[test]
    fn a_named_x402_crawler_is_still_a_crawler() {
        assert!(!is_recordable(Some("x402-indexer/1.0")));
    }

    #[test]
    fn the_snapshot_keeps_order_and_drops_the_bulk() {
        let candidates = vec![
            json!({"api_slug": "a", "method": "GET", "path": "/x", "price_usd": 0.01,
                   "hosted_externally": false, "accepts_all_inputs": true,
                   "notes": "a very long field we do not store", "inputs": {"url": {}}}),
            json!({"api_slug": "b", "method": "POST", "path": "/y", "price_usd": 0.5,
                   "hosted_externally": true, "accepts_all_inputs": false}),
        ];
        let snap = candidate_snapshot(&candidates);
        let arr = snap.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["position"], 1);
        assert_eq!(arr[0]["api_slug"], "a");
        assert_eq!(arr[1]["position"], 2);
        assert_eq!(arr[1]["hosted_externally"], true);
        // The bulk is deliberately absent.
        assert!(arr[0].get("notes").is_none());
        assert!(arr[0].get("inputs").is_none());
    }

    #[test]
    fn an_oversized_candidate_set_is_capped() {
        let many: Vec<Value> = (0..250)
            .map(|i| json!({"api_slug": i.to_string()}))
            .collect();
        let snap = candidate_snapshot(&many);
        assert_eq!(snap.as_array().unwrap().len(), MAX_CANDIDATES_RECORDED);
    }

    #[test]
    fn a_long_user_agent_cannot_write_megabytes() {
        let long = "x".repeat(5_000);
        assert_eq!(truncate(&long, MAX_USER_AGENT_CHARS).len(), 512);
    }
}
