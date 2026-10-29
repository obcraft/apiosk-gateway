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
//! signal the counters miss. Everything here is bounded and fails open — a
//! counter must never delay, alter or fail a response.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use axum::http::{HeaderMap, StatusCode};
use chrono::{DateTime, NaiveDate, Utc};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tracing::{info, warn};

/// How often buffered counters are written. A crawler reading every minute is
/// counted the same whether we flush at 10s or 60s, so this is chosen for
/// bounded memory and few round-trips, not for freshness.
const DEFAULT_FLUSH_INTERVAL: Duration = Duration::from_secs(15);

/// Buffer caps. The key spaces are all bounded by construction (documents are a
/// fixed vocabulary; slugs by the catalog), so hitting these means something
/// pathological — count it, log it, and keep serving.
const MAX_READ_KEYS: usize = 20_000;
const MAX_IMPRESSION_KEYS: usize = 50_000;
const MAX_AGENTS: usize = 5_000;

/// Longest user-agent string retained. The column is unbounded `text` and the
/// value comes straight off the wire.
const MAX_USER_AGENT_CHARS: usize = 512;

/// Consecutive failed flushes after which the tracker stops recording. Three
/// covers a transient database blip; a persistent failure is a deployment
/// problem (migration not applied), and retrying it every interval only spams
/// the log.
const MAX_CONSECUTIVE_FLUSH_FAILURES: u32 = 3;

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
struct Buffer {
    reads: HashMap<ReadKey, i64>,
    impressions: HashMap<ImpressionKey, i64>,
    agents: HashMap<String, AgentStat>,
    /// Counters dropped because a buffer was full, reported once per flush.
    dropped: u64,
}

impl Buffer {
    fn is_empty(&self) -> bool {
        self.reads.is_empty() && self.impressions.is_empty() && self.agents.is_empty()
    }

    fn take(&mut self) -> Buffer {
        std::mem::take(self)
    }
}

/// Handle stored on `AppState`. Cloning is cheap; every method is non-blocking.
#[derive(Clone)]
pub struct DiscoveryTracker {
    inner: Option<Arc<Inner>>,
}

struct Inner {
    buffer: Mutex<Buffer>,
    pool: PgPool,
    /// Cleared after [`MAX_CONSECUTIVE_FLUSH_FAILURES`] failed flushes, which in
    /// practice means migration 068 has not been applied on this database yet.
    /// Without it the writer would log the same missing-relation error every
    /// interval, forever — noise that buries the errors worth reading.
    live: AtomicBool,
    consecutive_failures: AtomicU32,
}

impl DiscoveryTracker {
    /// A tracker that records nothing. Used when tracking is switched off
    /// (`DISCOVERY_TRACKING_ENABLED=false`) and in tests.
    pub fn disabled() -> Self {
        Self { inner: None }
    }

    /// Start the tracker and its flush loop.
    pub fn start(pool: PgPool, tasks: &crate::background::BackgroundTasks) -> Self {
        if !env_bool("DISCOVERY_TRACKING_ENABLED", true) {
            info!("discovery tracking disabled (DISCOVERY_TRACKING_ENABLED=false)");
            return Self::disabled();
        }

        let inner = Arc::new(Inner {
            buffer: Mutex::new(Buffer::default()),
            pool,
            live: AtomicBool::new(true),
            consecutive_failures: AtomicU32::new(0),
        });
        let interval = std::env::var("DISCOVERY_FLUSH_INTERVAL_MS")
            .ok()
            .and_then(|raw| raw.trim().parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_FLUSH_INTERVAL);

        let flusher = inner.clone();
        tasks.spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                if !flusher.live.load(Ordering::Relaxed) {
                    break;
                }
                flusher.flush().await;
            }
        });

        info!(
            "discovery tracking enabled (flush every {}ms)",
            interval.as_millis()
        );
        Self { inner: Some(inner) }
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
        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        if !inner.live.load(Ordering::Relaxed) {
            return;
        }
        let Some(document) = document_label(path) else {
            return;
        };

        let user_agent = header_str(headers, "user-agent");
        let ua_class = crate::federated_traffic::ua_class(headers);
        let crawler = crawler_family(user_agent);
        let status_code = status.as_u16() as i16;
        let now = Utc::now();

        let Ok(mut guard) = inner.buffer.lock() else {
            return; // Poisoned by a panicking writer: drop the sample, never panic.
        };
        // Reborrow once so the field borrows below split cleanly through the guard.
        let buffer = &mut *guard;

        let key = ReadKey {
            day: now.date_naive(),
            document,
            ua_class,
            crawler: crawler.clone(),
            status_code,
        };
        bump(
            &mut buffer.reads,
            key,
            1,
            MAX_READ_KEYS,
            &mut buffer.dropped,
        );

        // One row per distinct user agent — the "who ingests us" table. An agent
        // that sends no UA is not identifiable, so it is only counted above.
        if let Some(user_agent) = user_agent.map(|ua| truncate(ua, MAX_USER_AGENT_CHARS)) {
            let ua_hash = sha256_hex(&user_agent);
            let known = buffer.agents.contains_key(&ua_hash);
            if known || buffer.agents.len() < MAX_AGENTS {
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
            } else {
                buffer.dropped += 1;
            }
        }
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
        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        if listings.is_empty() || !inner.live.load(Ordering::Relaxed) {
            return;
        }

        let ua_class = crate::federated_traffic::ua_class(headers);
        let crawler = crawler_family(header_str(headers, "user-agent"));
        let day = Utc::now().date_naive();

        let Ok(mut guard) = inner.buffer.lock() else {
            return;
        };
        let buffer = &mut *guard;
        for (slug, contract) in listings {
            let key = ImpressionKey {
                day,
                api_slug: slug.clone(),
                document,
                ua_class,
                crawler: crawler.clone(),
                contract,
            };
            bump(
                &mut buffer.impressions,
                key,
                1,
                MAX_IMPRESSION_KEYS,
                &mut buffer.dropped,
            );
        }
    }
}

fn bump<K: std::hash::Hash + Eq>(
    map: &mut HashMap<K, i64>,
    key: K,
    by: i64,
    max_keys: usize,
    dropped: &mut u64,
) {
    if let Some(count) = map.get_mut(&key) {
        *count += by;
    } else if map.len() < max_keys {
        map.insert(key, by);
    } else {
        *dropped += 1;
    }
}

impl Inner {
    /// Write the buffered counters. Each statement is independent: a failure in
    /// one aggregate is logged and does not discard the others.
    async fn flush(&self) {
        let batch = {
            let Ok(mut buffer) = self.buffer.lock() else {
                return;
            };
            if buffer.is_empty() {
                return;
            }
            buffer.take()
        };

        if batch.dropped > 0 {
            warn!(
                "discovery tracking dropped {} counter updates (buffer caps hit)",
                batch.dropped
            );
        }

        let ok = self.flush_reads(&batch).await
            & self.flush_agents(&batch).await
            & self.flush_impressions(&batch).await;

        if ok {
            self.consecutive_failures.store(0, Ordering::Relaxed);
            return;
        }
        let failures = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
        if failures >= MAX_CONSECUTIVE_FLUSH_FAILURES {
            self.live.store(false, Ordering::Relaxed);
            warn!(
                "discovery tracking stopped after {failures} failed flushes — most likely \
                 migration 068_discovery_traffic has not been applied. Apply it and restart \
                 the gateway to resume counting."
            );
        }
    }

    /// `true` when there was nothing to write or the write succeeded.
    async fn flush_reads(&self, batch: &Buffer) -> bool {
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
        .execute(&self.pool)
        .await;
        match result {
            Ok(_) => true,
            Err(err) => {
                warn!("discovery reads flush failed ({} keys): {err}", days.len());
                false
            }
        }
    }

    /// `true` when there was nothing to write or the write succeeded.
    async fn flush_agents(&self, batch: &Buffer) -> bool {
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
        .execute(&self.pool)
        .await;
        match result {
            Ok(_) => true,
            Err(err) => {
                warn!(
                    "discovery agents flush failed ({} keys): {err}",
                    hashes.len()
                );
                false
            }
        }
    }

    /// `true` when there was nothing to write or the write succeeded.
    async fn flush_impressions(&self, batch: &Buffer) -> bool {
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
        .execute(&self.pool)
        .await;
        match result {
            Ok(_) => true,
            Err(err) => {
                warn!(
                    "discovery impressions flush failed ({} keys): {err}",
                    days.len()
                );
                false
            }
        }
    }
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn sha256_hex(value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(value.as_bytes());
    hex::encode(digest.finalize())
}

fn env_bool(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(raw) => matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => default,
    }
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

    #[test]
    fn buffer_caps_drop_new_keys_instead_of_growing_without_bound() {
        let mut map: HashMap<u32, i64> = HashMap::new();
        let mut dropped = 0;
        bump(&mut map, 1, 1, 2, &mut dropped);
        bump(&mut map, 2, 1, 2, &mut dropped);
        bump(&mut map, 3, 1, 2, &mut dropped);
        assert_eq!(map.len(), 2);
        assert_eq!(dropped, 1);
        // An existing key keeps counting after the cap is hit.
        bump(&mut map, 1, 1, 2, &mut dropped);
        assert_eq!(map[&1], 2);
        assert_eq!(dropped, 1);
    }
}
