//! Who ingests the discovery documents, how often, and what they saw.
//!
//! Apiosk's distribution channel is the crawlers. An agent almost never reads
//! apiosk.com — it reads `/.well-known/x402` once, caches it, and every call it
//! makes afterwards is decided from that text. That makes the discovery
//! namespace the most-read surface the gateway serves and, until this module,
//! the least measured one: `traffic::track_request` skips `/.well-known/*`
//! entirely (it is crawler-dominated and would drown the request log),
//! telemetry v2's `gateway_discovery_events` is gated behind
//! `TELEMETRY_V2_ENABLED` and off in production, and mig 064 counts discovery
//! reads for federated listings only.
//!
//! Three questions, three aggregates (migration 068):
//!
//!   * **who** — `discovery_agents`, one row per distinct user agent.
//!   * **how much** — `discovery_reads_daily`, per document and caller class.
//!   * **of what** — `discovery_impressions_daily`, per listing published.
//!
//! Aggregated in-process and flushed on a timer rather than written per
//! request: at crawler volume the per-request row costs money and carries no
//! signal the counters miss. The buffering, bounding and fail-open rules are
//! [`crate::counters`]'; this module is the keys and the SQL.

use std::collections::HashMap;

use axum::http::{HeaderMap, StatusCode};
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::PgPool;

use crate::{
    counters::{bump, report, CounterBuffer, Spec, Tracker},
    http::header_str,
    util::{sha256_hex, truncate_chars},
};

/// Buffer caps. The key spaces are all bounded by construction (documents are a
/// fixed vocabulary; slugs by the catalog), so hitting these means something
/// pathological — count it, log it, and keep serving.
const MAX_READ_KEYS: usize = 20_000;
const MAX_IMPRESSION_KEYS: usize = 50_000;
const MAX_AGENTS: usize = 5_000;

/// Longest user-agent string retained. The column is unbounded `text` and the
/// value comes straight off the wire.
const MAX_USER_AGENT_CHARS: usize = 512;

static SPEC: Spec = Spec {
    name: "discovery",
    enabled_var: "DISCOVERY_TRACKING_ENABLED",
    interval_var: "DISCOVERY_FLUSH_INTERVAL_MS",
    migration: "068_discovery_traffic",
};

/// Normalized document label for a discovery path. A bounded vocabulary keeps
/// the counter tables small no matter what a crawler probes, and keeps
/// per-listing paths (`/{slug}/openapi.json`) from exploding cardinality.
/// `None` for anything that is not a discovery document.
pub fn document_label(path: &str) -> Option<&'static str> {
    let path = path.trim_end_matches('/');
    match path {
        "" | "/.well-known" => Some("/.well-known"),
        "/.well-known/x402" | "/.well-known/x402.json" => Some("/.well-known/x402"),
        "/.well-known/agent.json" | "/.well-known/agent-card.json" => {
            Some("/.well-known/agent.json")
        }
        "/.well-known/ai-plugin.json" => Some("/.well-known/ai-plugin.json"),
        "/.well-known/mcp.json" => Some("/.well-known/mcp.json"),
        "/.well-known/security.txt" => Some("/.well-known/security.txt"),
        "/llms.txt" => Some("/llms.txt"),
        "/openapi.json" => Some("/openapi.json"),
        other if other.starts_with("/.well-known/") => Some("/.well-known/other"),
        // Per-listing documents collapse to one label each: which listing was
        // read is already answered by discovery_impressions_daily.
        other if other.ends_with("/openapi.json") => Some("api-openapi"),
        other if other.ends_with("/llms.txt") => Some("api-llms-txt"),
        _ => None,
    }
}

/// A named crawler family, or `unclassified`. Reuses the telemetry taxonomy so
/// the same user agent is labelled identically wherever it shows up.
fn crawler_family(user_agent: Option<&str>) -> String {
    crate::telemetry::classify::classify_crawler(user_agent)
        .unwrap_or_else(|| "unclassified".to_string())
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct ReadKey {
    day: NaiveDate,
    document: &'static str,
    ua_class: &'static str,
    crawler: String,
    status_code: i16,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct ImpressionKey {
    day: NaiveDate,
    api_slug: String,
    document: &'static str,
    ua_class: &'static str,
    crawler: String,
    /// Contract tier the listing was published with, as the crawler saw it
    /// (mig 073). Part of the key, not an attribute: the same listing can go out
    /// as `documented` on one read and `callable` on the next.
    contract: &'static str,
}

#[derive(Clone)]
struct AgentStat {
    user_agent: String,
    ua_class: &'static str,
    crawler: String,
    hits: i64,
    last_seen: DateTime<Utc>,
    last_document: &'static str,
    last_status: i16,
    last_ip_hash: Option<String>,
}

#[derive(Default)]
pub(crate) struct Buffer {
    reads: HashMap<ReadKey, i64>,
    impressions: HashMap<ImpressionKey, i64>,
    agents: HashMap<String, AgentStat>,
    /// Counters dropped because a buffer was full, reported once per flush.
    dropped: u64,
}

impl CounterBuffer for Buffer {
    fn is_empty(&self) -> bool {
        self.reads.is_empty() && self.impressions.is_empty() && self.agents.is_empty()
    }

    fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Each statement is independent: a failure in one aggregate is logged and
    /// does not discard the others.
    async fn flush(&self, pool: &PgPool) -> bool {
        flush_reads(self, pool).await & flush_agents(self, pool).await & flush_impressions(self, pool).await
    }
}

/// Handle stored on `AppState`. Cloning is cheap; every method is non-blocking.
pub(crate) type DiscoveryTracker = Tracker<Buffer>;

impl DiscoveryTracker {
    /// Start the tracker and its flush loop (`DISCOVERY_TRACKING_ENABLED=false`
    /// turns it off).
    pub fn start(pool: PgPool, tasks: &crate::background::BackgroundTasks) -> Self {
        Tracker::launch(pool, tasks, &SPEC)
    }

    /// Record one read of a discovery document. `path` is the raw request path;
    /// non-discovery paths are ignored, so the caller does not have to filter.
    pub fn record_read(
        &self,
        path: &str,
        status: StatusCode,
        headers: &HeaderMap,
        ip_hash: Option<&str>,
    ) {
        let Some(document) = document_label(path) else {
            return;
        };
        let user_agent = header_str(headers, "user-agent");
        let ua_class = crate::federated_traffic::ua_class(headers);
        let crawler = crawler_family(user_agent);
        let status_code = status.as_u16() as i16;

        self.record(|buffer, now| {
            let key = ReadKey {
                day: now.date_naive(),
                document,
                ua_class,
                crawler: crawler.clone(),
                status_code,
            };
            bump(&mut buffer.reads, key, 1, MAX_READ_KEYS, &mut buffer.dropped);

            // One row per distinct user agent — the "who ingests us" table. An
            // agent that sends no UA is not identifiable, so it is only counted
            // above.
            let Some(user_agent) = user_agent.map(|ua| truncate_chars(ua, MAX_USER_AGENT_CHARS)) else {
                return;
            };
            let ua_hash = sha256_hex(&user_agent);
            let known = buffer.agents.contains_key(&ua_hash);
            if !known && buffer.agents.len() >= MAX_AGENTS {
                buffer.dropped += 1;
                return;
            }
            let entry = buffer.agents.entry(ua_hash).or_insert(AgentStat {
                user_agent,
                ua_class,
                crawler,
                hits: 0,
                last_seen: now,
                last_document: document,
                last_status: status_code,
                last_ip_hash: None,
            });
            entry.hits += 1;
            entry.last_seen = now;
            entry.last_document = document;
            entry.last_status = status_code;
            if let Some(ip_hash) = ip_hash {
                entry.last_ip_hash = Some(ip_hash.to_string());
            }
        });
    }

    /// Record that `listings` were published to a caller in `document` — the
    /// impression half of the impression -> 402 -> paid funnel. Called by the
    /// handler that built the document, because only it knows what went out.
    ///
    /// Each entry is `(slug, contract_tier)`: the tier is stamped as the crawler
    /// saw it (mig 073), so "do documented listings convert better?" stays
    /// answerable later, when the same listings may have been documented since.
    /// Callers with no tier to state pass `"unknown"`.
    pub fn record_impressions(
        &self,
        listings: &[(String, &'static str)],
        document: &'static str,
        headers: &HeaderMap,
    ) {
        if listings.is_empty() {
            return;
        }
        let ua_class = crate::federated_traffic::ua_class(headers);
        let crawler = crawler_family(header_str(headers, "user-agent"));
        self.record(|buffer, now| {
            let day = now.date_naive();
            for (slug, contract) in listings {
                let key = ImpressionKey {
                    day,
                    api_slug: slug.clone(),
                    document,
                    ua_class,
                    crawler: crawler.clone(),
                    contract,
                };
                bump(&mut buffer.impressions, key, 1, MAX_IMPRESSION_KEYS, &mut buffer.dropped);
            }
        });
    }
}

/// `true` when there was nothing to write or the write succeeded.
async fn flush_reads(batch: &Buffer, pool: &PgPool) -> bool {
    if batch.reads.is_empty() {
        return true;
    }
    let mut days = Vec::new();
    let mut documents = Vec::new();
    let mut ua_classes = Vec::new();
    let mut crawlers = Vec::new();
    let mut statuses = Vec::new();
    let mut hits = Vec::new();
    for (key, count) in &batch.reads {
        days.push(key.day);
        documents.push(key.document.to_string());
        ua_classes.push(key.ua_class.to_string());
        crawlers.push(key.crawler.clone());
        statuses.push(key.status_code);
        hits.push(*count);
    }

    let result = sqlx::query(
        r#"INSERT INTO discovery_reads_daily (day, document, ua_class, crawler, status_code, hits)
           SELECT * FROM unnest($1::date[], $2::text[], $3::text[], $4::text[], $5::smallint[], $6::bigint[])
           ON CONFLICT (day, document, ua_class, crawler, status_code)
           DO UPDATE SET hits = discovery_reads_daily.hits + excluded.hits,
                         updated_at = now()"#,
    )
    .bind(&days)
    .bind(&documents)
    .bind(&ua_classes)
    .bind(&crawlers)
    .bind(&statuses)
    .bind(&hits)
    .execute(pool)
    .await;
    report("discovery reads", days.len(), result)
}

/// `true` when there was nothing to write or the write succeeded.
async fn flush_agents(batch: &Buffer, pool: &PgPool) -> bool {
    if batch.agents.is_empty() {
        return true;
    }
    let mut hashes = Vec::new();
    let mut user_agents = Vec::new();
    let mut ua_classes = Vec::new();
    let mut crawlers = Vec::new();
    let mut hits = Vec::new();
    let mut last_seen = Vec::new();
    let mut last_documents = Vec::new();
    let mut last_statuses = Vec::new();
    let mut last_ips: Vec<Option<String>> = Vec::new();
    for (hash, stat) in &batch.agents {
        hashes.push(hash.clone());
        user_agents.push(stat.user_agent.clone());
        ua_classes.push(stat.ua_class.to_string());
        crawlers.push(stat.crawler.clone());
        hits.push(stat.hits);
        last_seen.push(stat.last_seen);
        last_documents.push(stat.last_document.to_string());
        last_statuses.push(stat.last_status);
        last_ips.push(stat.last_ip_hash.clone());
    }

    // first_seen_at is only ever set on insert: the point of the column is
    // "since when has this crawler known about us".
    let result = sqlx::query(
        r#"INSERT INTO discovery_agents (
               ua_hash, user_agent, ua_class, crawler, hits,
               first_seen_at, last_seen_at, last_document, last_status, last_ip_hash)
           SELECT h, ua, cls, cr, n, seen, seen, doc, st, ip
             FROM unnest($1::text[], $2::text[], $3::text[], $4::text[], $5::bigint[],
                         $6::timestamptz[], $7::text[], $8::smallint[], $9::text[])
               AS t(h, ua, cls, cr, n, seen, doc, st, ip)
           ON CONFLICT (ua_hash) DO UPDATE SET
               hits = discovery_agents.hits + excluded.hits,
               last_seen_at = greatest(discovery_agents.last_seen_at, excluded.last_seen_at),
               last_document = excluded.last_document,
               last_status = excluded.last_status,
               last_ip_hash = coalesce(excluded.last_ip_hash, discovery_agents.last_ip_hash),
               ua_class = excluded.ua_class,
               crawler = excluded.crawler"#,
    )
    .bind(&hashes)
    .bind(&user_agents)
    .bind(&ua_classes)
    .bind(&crawlers)
    .bind(&hits)
    .bind(&last_seen)
    .bind(&last_documents)
    .bind(&last_statuses)
    .bind(&last_ips)
    .execute(pool)
    .await;
    report("discovery agents", hashes.len(), result)
}

/// `true` when there was nothing to write or the write succeeded.
async fn flush_impressions(batch: &Buffer, pool: &PgPool) -> bool {
    if batch.impressions.is_empty() {
        return true;
    }
    let mut days = Vec::new();
    let mut slugs = Vec::new();
    let mut documents = Vec::new();
    let mut ua_classes = Vec::new();
    let mut crawlers = Vec::new();
    let mut contracts = Vec::new();
    let mut impressions = Vec::new();
    for (key, count) in &batch.impressions {
        days.push(key.day);
        slugs.push(key.api_slug.clone());
        documents.push(key.document.to_string());
        ua_classes.push(key.ua_class.to_string());
        crawlers.push(key.crawler.clone());
        contracts.push(key.contract.to_string());
        impressions.push(*count);
    }

    let result = sqlx::query(
        r#"INSERT INTO discovery_impressions_daily (day, api_slug, document, ua_class, crawler, contract, impressions)
           SELECT * FROM unnest($1::date[], $2::text[], $3::text[], $4::text[], $5::text[], $6::text[], $7::bigint[])
           ON CONFLICT (day, api_slug, document, ua_class, crawler, contract)
           DO UPDATE SET impressions = discovery_impressions_daily.impressions + excluded.impressions,
                         updated_at = now()"#,
    )
    .bind(&days)
    .bind(&slugs)
    .bind(&documents)
    .bind(&ua_classes)
    .bind(&crawlers)
    .bind(&contracts)
    .bind(&impressions)
    .execute(pool)
    .await;
    report("discovery impressions", days.len(), result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_documents_collapse_to_a_bounded_vocabulary() {
        assert_eq!(
            document_label("/.well-known/x402"),
            Some("/.well-known/x402")
        );
        // Alias and trailing slash are the same document, not three rows.
        assert_eq!(
            document_label("/.well-known/x402.json"),
            Some("/.well-known/x402")
        );
        assert_eq!(
            document_label("/.well-known/x402/"),
            Some("/.well-known/x402")
        );
        assert_eq!(
            document_label("/.well-known/agent-card.json"),
            Some("/.well-known/agent.json")
        );
        // Per-listing documents collapse: which listing is answered by the
        // impressions table, not by a row per slug here.
        assert_eq!(document_label("/weather/openapi.json"), Some("api-openapi"));
        assert_eq!(document_label("/weather/llms.txt"), Some("api-llms-txt"));
        // Unknown probes inside the namespace still count as one bucket.
        assert_eq!(
            document_label("/.well-known/apple-app-site-association"),
            Some("/.well-known/other")
        );
        // Anything else is not a discovery read.
        assert_eq!(document_label("/weather/current"), None);
        assert_eq!(document_label("/v1/apis"), None);
    }

    #[test]
    fn a_disabled_tracker_records_nothing_and_never_panics() {
        let tracker = DiscoveryTracker::disabled();
        let headers = HeaderMap::new();
        tracker.record_read("/.well-known/x402", StatusCode::OK, &headers, None);
        tracker.record_impressions(
            &[("weather".to_string(), "documented")],
            "/.well-known/x402",
            &headers,
        );
    }

    #[tokio::test]
    async fn reads_count_per_document_and_agents_keep_their_last_read() {
        let tracker = DiscoveryTracker::in_memory(&SPEC);
        let mut headers = HeaderMap::new();
        headers.insert("user-agent", "GPTBot/1.0".parse().unwrap());
        tracker.record_read("/.well-known/x402", StatusCode::OK, &headers, Some("ip-1"));
        tracker.record_read("/.well-known/x402.json", StatusCode::OK, &headers, None);
        tracker.record_read("/weather/current", StatusCode::OK, &headers, None);
        tracker.record_impressions(&[("weather".to_string(), "callable")], "/.well-known/x402", &headers);

        tracker
            .peek(|buffer| {
                // Two reads of the same document under one key; the API call is
                // not a discovery read at all.
                assert_eq!(buffer.reads.len(), 1);
                assert_eq!(buffer.reads.values().next(), Some(&2));
                let agent = buffer.agents.values().next().unwrap();
                assert_eq!(agent.hits, 2);
                assert_eq!(agent.last_document, "/.well-known/x402");
                // A read with no IP hash leaves the last known one in place.
                assert_eq!(agent.last_ip_hash.as_deref(), Some("ip-1"));
                assert_eq!(buffer.impressions.len(), 1);
            })
            .unwrap();
    }
}
