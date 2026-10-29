//! How much agent surface providers actually inject, and where.
//!
//! `/webmcp.js` is the shim a provider embeds on their own site. On a
//! WebMCP-capable browser it reads the provider's listing and hands the
//! browser's `modelContext` one tool per endpoint, so an agent that lands on
//! the provider's page can call the API instead of scraping it.
//!
//! Everything that decides whether that worked happens in the browser, after
//! the script is served: the shim is a static, hour-cached asset, its request
//! carries no slug, and a page with no WebMCP host registers nothing at all. So
//! the volume question — how many WebMCPs are injected, by which providers —
//! was unanswerable from the request log. The shim now reports the outcome of
//! each run to [`report_injection`], and this module aggregates it (mig 074).
//!
//! Two aggregates:
//!
//!   * **how much** — `webmcp_injections_daily`, per listing, site and outcome.
//!   * **by whom**  — `webmcp_sites`, one row per (listing, site origin).
//!
//! Same shape and the same guarantees as [`crate::discovery_traffic`]: counters
//! are buffered in-process and flushed on a timer, every buffer is bounded, and
//! nothing here may delay, alter or fail a response. Only browsers that expose
//! a `modelContext` report, so this counts agent-capable page loads — never
//! page views at large, and never anything about the visitor.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{
        header::{CONTENT_TYPE, ORIGIN, REFERER, RETRY_AFTER, USER_AGENT},
        HeaderMap, HeaderValue, StatusCode,
    },
    response::Response,
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tracing::{info, warn};

use crate::{
    rate_limit::{RateLimitDecision, RateLimitPolicy},
    AppState,
};

/// How often buffered counters are written. A busy provider site injecting on
/// every page load is counted the same whether we flush at 10s or 60s, so this
/// is chosen for bounded memory and few round-trips, not for freshness.
const DEFAULT_FLUSH_INTERVAL: Duration = Duration::from_secs(15);

/// Buffer caps. Both key spaces are bounded in practice (slugs by the catalog,
/// origins by the set of sites that embedded the shim), so hitting these means
/// something pathological — count it, log it, and keep serving.
const MAX_INJECTION_KEYS: usize = 20_000;
const MAX_SITE_KEYS: usize = 20_000;

/// Consecutive failed flushes after which the tracker stops recording. Three
/// covers a transient database blip; a persistent failure means migration 074
/// has not been applied, and retrying every interval only spams the log.
const MAX_CONSECUTIVE_FLUSH_FAILURES: u32 = 3;

/// Open, unauthenticated endpoint called from provider pages: cap per-IP
/// reports. A real browser reports once per page load, but the browsers that
/// report at all are disproportionately agent runtimes — a fleet behind one
/// datacenter address is the normal case here, not abuse. Set high enough that
/// throttling means a flood rather than a busy customer, because a throttled
/// report is an injection we undercount.
const REPORT_RATE_LIMIT: RateLimitPolicy = RateLimitPolicy::per_minute(300);

/// Defensive column caps — the columns are unbounded `text` and every value
/// here comes off the wire.
const MAX_SLUG_CHARS: usize = 128;
const MAX_ORIGIN_CHARS: usize = 255;
const MAX_USER_AGENT_CHARS: usize = 512;
/// A listing with more tools than this is not a listing we can believe.
const MAX_TOOLS: i64 = 1_000;

/// Reserved slug for catalog mode (`data-apiosk-catalog="1"`), which registers
/// the gateway's own search/lookup tools rather than one listing's endpoints.
pub const CATALOG_SLUG: &str = "_catalog";

/// What the shim reports. Every field is optional at the serde layer and
/// validated in the handler: a malformed beacon is dropped, never fatal.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct InjectionReport {
    #[serde(alias = "apiSlug", alias = "api_slug")]
    slug: Option<String>,
    /// Number of tools handed to the browser's modelContext.
    tools: Option<i64>,
    /// 'registered' | 'empty' | 'failed'.
    outcome: Option<String>,
    /// 'listing' | 'catalog'.
    mode: Option<String>,
}

/// Outcome vocabulary, normalized to a bounded set so one caller cannot grow
/// the counter table by inventing values.
fn outcome_label(raw: Option<&str>) -> Option<&'static str> {
    match raw.map(str::trim).unwrap_or("") {
        "registered" | "ok" => Some("registered"),
        "empty" | "no_tools" => Some("empty"),
        "failed" | "error" => Some("failed"),
        _ => None,
    }
}

fn mode_label(raw: Option<&str>) -> &'static str {
    match raw.map(str::trim).unwrap_or("") {
        "catalog" => "catalog",
        _ => "listing",
    }
}

/// One validated shim run, as handed to [`WebmcpTracker::record`].
#[derive(Clone, Copy)]
struct Injection<'a> {
    api_slug: &'a str,
    /// Origin of the page that ran the shim; empty when the browser sent none.
    origin: &'a str,
    outcome: &'static str,
    mode: &'static str,
    ua_class: &'static str,
    /// Tools actually registered — zero for every outcome but `registered`.
    tools: i64,
    user_agent: Option<&'a str>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct InjectionKey {
    day: NaiveDate,
    api_slug: String,
    origin: String,
    outcome: &'static str,
    mode: &'static str,
    ua_class: &'static str,
}

#[derive(Clone)]
struct InjectionCount {
    injections: i64,
    tools: i64,
}

#[derive(Clone)]
struct SiteStat {
    api_slug: String,
    origin: String,
    mode: &'static str,
    injections: i64,
    /// Tool count of the most recent *successful* injection — the current size
    /// of this site's toolset. A failed run leaves it untouched.
    last_tools: Option<i32>,
    last_outcome: &'static str,
    last_user_agent: Option<String>,
    last_seen: DateTime<Utc>,
}

#[derive(Default)]
struct Buffer {
    injections: HashMap<InjectionKey, InjectionCount>,
    sites: HashMap<String, SiteStat>,
    /// Counters dropped because a buffer was full, reported once per flush.
    dropped: u64,
}

impl Buffer {
    fn is_empty(&self) -> bool {
        self.injections.is_empty() && self.sites.is_empty()
    }

    fn take(&mut self) -> Buffer {
        std::mem::take(self)
    }
}

/// Handle stored on `AppState`. Cloning is cheap; every method is non-blocking.
#[derive(Clone)]
pub struct WebmcpTracker {
    inner: Option<Arc<Inner>>,
}

struct Inner {
    buffer: Mutex<Buffer>,
    pool: PgPool,
    /// Cleared after [`MAX_CONSECUTIVE_FLUSH_FAILURES`] failed flushes, which in
    /// practice means migration 074 has not been applied on this database yet.
    live: AtomicBool,
    consecutive_failures: AtomicU32,
}

impl WebmcpTracker {
    /// A tracker that records nothing. Used when tracking is switched off
    /// (`WEBMCP_TRACKING_ENABLED=false`) and in tests.
    pub fn disabled() -> Self {
        Self { inner: None }
    }

    /// Start the tracker and its flush loop.
    pub fn start(pool: PgPool, tasks: &crate::background::BackgroundTasks) -> Self {
        if !env_bool("WEBMCP_TRACKING_ENABLED", true) {
            info!("webmcp injection tracking disabled (WEBMCP_TRACKING_ENABLED=false)");
            return Self::disabled();
        }

        let inner = Arc::new(Inner {
            buffer: Mutex::new(Buffer::default()),
            pool,
            live: AtomicBool::new(true),
            consecutive_failures: AtomicU32::new(0),
        });
        let interval = std::env::var("WEBMCP_FLUSH_INTERVAL_MS")
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
            "webmcp injection tracking enabled (flush every {}ms)",
            interval.as_millis()
        );
        Self { inner: Some(inner) }
    }

    /// Record one shim run.
    fn record(&self, run: &Injection<'_>) {
        let Injection {
            api_slug,
            origin,
            outcome,
            mode,
            ua_class,
            tools,
            user_agent,
        } = *run;

        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        if !inner.live.load(Ordering::Relaxed) {
            return;
        }
        let now = Utc::now();

        let Ok(mut guard) = inner.buffer.lock() else {
            return; // Poisoned by a panicking writer: drop the sample, never panic.
        };
        let buffer = &mut *guard;

        let key = InjectionKey {
            day: now.date_naive(),
            api_slug: api_slug.to_string(),
            origin: origin.to_string(),
            outcome,
            mode,
            ua_class,
        };
        if let Some(entry) = buffer.injections.get_mut(&key) {
            entry.injections += 1;
            entry.tools += tools;
        } else if buffer.injections.len() < MAX_INJECTION_KEYS {
            buffer.injections.insert(
                key,
                InjectionCount {
                    injections: 1,
                    tools,
                },
            );
        } else {
            buffer.dropped += 1;
        }

        // One row per (listing, site): which provider pages carry the shim.
        let site_key = sha256_hex(&format!("{api_slug}|{origin}"));
        let known = buffer.sites.contains_key(&site_key);
        if known || buffer.sites.len() < MAX_SITE_KEYS {
            let entry = buffer.sites.entry(site_key).or_insert(SiteStat {
                api_slug: api_slug.to_string(),
                origin: origin.to_string(),
                mode,
                injections: 0,
                last_tools: None,
                last_outcome: outcome,
                last_user_agent: None,
                last_seen: now,
            });
            entry.injections += 1;
            entry.last_outcome = outcome;
            entry.last_seen = now;
            entry.mode = mode;
            if outcome == "registered" {
                entry.last_tools = Some(tools.clamp(0, MAX_TOOLS) as i32);
            }
            if let Some(user_agent) = user_agent {
                entry.last_user_agent = Some(truncate(user_agent, MAX_USER_AGENT_CHARS));
            }
        } else {
            buffer.dropped += 1;
        }
    }
}

/// `POST /_apiosk/webmcp` — the shim reports what it injected.
///
/// Public and unauthenticated (it is called from provider pages, cross-origin),
/// per-IP rate limited, and free of anything that identifies a visitor: the
/// body carries a slug, a tool count and an outcome, and the site is taken from
/// the browser-set `Origin`/`Referer` header rather than from the body, so a
/// caller cannot attribute injections to a site it does not serve.
pub async fn report_injection(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    let report: InjectionReport = match serde_json::from_slice(&body) {
        Ok(parsed) => parsed,
        Err(_) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                serde_json::json!({
                    "ok": false,
                    "error": "invalid_json",
                    "message": "Send {\"slug\":\"...\",\"outcome\":\"registered|empty|failed\",\"tools\":N}.",
                }),
            );
        }
    };

    let Some(outcome) = outcome_label(report.outcome.as_deref()) else {
        return json_response(
            StatusCode::BAD_REQUEST,
            serde_json::json!({
                "ok": false,
                "error": "invalid_outcome",
                "message": "outcome must be one of: registered, empty, failed.",
            }),
        );
    };
    let mode = mode_label(report.mode.as_deref());

    // Catalog mode has no listing of its own; everything else must name one.
    let slug = match report.slug.as_deref().map(str::trim).unwrap_or("") {
        "" if mode == "catalog" => CATALOG_SLUG.to_string(),
        "" => {
            return json_response(
                StatusCode::BAD_REQUEST,
                serde_json::json!({
                    "ok": false,
                    "error": "slug_required",
                    "message": "Name the listing the shim was configured for: {\"slug\": \"...\"}.",
                }),
            );
        }
        raw => truncate(&raw.to_lowercase(), MAX_SLUG_CHARS),
    };

    // Per-IP flood guard. Keyed on the IP alone: a page that reloads in a loop
    // is the case this exists for.
    let ip = client_ip(&headers).unwrap_or_else(|| "unknown".to_string());
    if let RateLimitDecision::Throttled { retry_after } = state
        .rate_limiter
        .check(&format!("webmcp:{ip}"), REPORT_RATE_LIMIT)
        .await
    {
        let mut response = json_response(
            StatusCode::TOO_MANY_REQUESTS,
            serde_json::json!({ "ok": false, "error": "rate_limited" }),
        );
        if let Ok(value) = HeaderValue::from_str(&retry_after.as_secs().max(1).to_string()) {
            response.headers_mut().insert(RETRY_AFTER, value);
        }
        return response;
    }

    // Tools only mean something on a successful registration.
    let tools = if outcome == "registered" {
        report.tools.unwrap_or(0).clamp(0, MAX_TOOLS)
    } else {
        0
    };

    let origin = page_origin(&headers).unwrap_or_default();
    let ua_class = crate::federated_traffic::ua_class(&headers);
    let user_agent = headers
        .get(USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty());

    state.webmcp.record(&Injection {
        api_slug: &slug,
        origin: &origin,
        outcome,
        mode,
        ua_class,
        tools,
        user_agent,
    });

    // A beacon reads nothing back; 204 keeps it as cheap as it can be.
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .body(Body::empty())
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

/// The origin of the page that ran the shim, as `scheme://host[:port]`.
///
/// Taken from `Origin` (set by the browser on a cross-origin POST and not
/// settable by page script) and falling back to the origin part of `Referer`.
/// `None` when the browser sent neither — a same-origin run on the gateway's
/// own pages, which is the catalog case.
fn page_origin(headers: &HeaderMap) -> Option<String> {
    let raw = headers
        .get(ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty() && *v != "null")
        .map(str::to_string)
        .or_else(|| {
            let referer = headers.get(REFERER).and_then(|v| v.to_str().ok())?.trim();
            let (scheme, rest) = referer.split_once("://")?;
            let host = rest.split(['/', '?', '#']).next()?;
            if host.is_empty() {
                return None;
            }
            Some(format!("{scheme}://{host}"))
        })?;
    Some(truncate(&raw.to_lowercase(), MAX_ORIGIN_CHARS))
}

/// Client IP for the rate-limit key, same precedence as the intent survey.
fn client_ip(headers: &HeaderMap) -> Option<String> {
    headers
        .get("fly-client-ip")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            headers
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(',').next())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .map(str::to_string)
}

impl Inner {
    /// Write the buffered counters. Each statement is independent: a failure in
    /// one aggregate is logged and does not discard the other.
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
                "webmcp tracking dropped {} counter updates (buffer caps hit)",
                batch.dropped
            );
        }

        let ok = self.flush_injections(&batch).await & self.flush_sites(&batch).await;

        if ok {
            self.consecutive_failures.store(0, Ordering::Relaxed);
            return;
        }
        let failures = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
        if failures >= MAX_CONSECUTIVE_FLUSH_FAILURES {
            self.live.store(false, Ordering::Relaxed);
            warn!(
                "webmcp tracking stopped after {failures} failed flushes — most likely \
                 migration 074_webmcp_injections has not been applied. Apply it and restart \
                 the gateway to resume counting."
            );
        }
    }

    /// `true` when there was nothing to write or the write succeeded.
    async fn flush_injections(&self, batch: &Buffer) -> bool {
        if batch.injections.is_empty() {
            return true;
        }
        let mut days = Vec::new();
        let mut slugs = Vec::new();
        let mut origins = Vec::new();
        let mut outcomes = Vec::new();
        let mut modes = Vec::new();
        let mut ua_classes = Vec::new();
        let mut injections = Vec::new();
        let mut tools = Vec::new();
        for (key, count) in &batch.injections {
            days.push(key.day);
            slugs.push(key.api_slug.clone());
            origins.push(key.origin.clone());
            outcomes.push(key.outcome.to_string());
            modes.push(key.mode.to_string());
            ua_classes.push(key.ua_class.to_string());
            injections.push(count.injections);
            tools.push(count.tools);
        }

        let result = sqlx::query(
            r#"INSERT INTO webmcp_injections_daily (day, api_slug, origin, outcome, mode, ua_class, injections, tools)
               SELECT * FROM unnest($1::date[], $2::text[], $3::text[], $4::text[], $5::text[], $6::text[], $7::bigint[], $8::bigint[])
               ON CONFLICT (day, api_slug, origin, outcome, mode, ua_class)
               DO UPDATE SET injections = webmcp_injections_daily.injections + excluded.injections,
                             tools = webmcp_injections_daily.tools + excluded.tools,
                             updated_at = now()"#,
        )
        .bind(&days)
        .bind(&slugs)
        .bind(&origins)
        .bind(&outcomes)
        .bind(&modes)
        .bind(&ua_classes)
        .bind(&injections)
        .bind(&tools)
        .execute(&self.pool)
        .await;
        match result {
            Ok(_) => true,
            Err(err) => {
                warn!(
                    "webmcp injections flush failed ({} keys): {err}",
                    days.len()
                );
                false
            }
        }
    }

    /// `true` when there was nothing to write or the write succeeded.
    async fn flush_sites(&self, batch: &Buffer) -> bool {
        if batch.sites.is_empty() {
            return true;
        }
        let mut keys = Vec::new();
        let mut slugs = Vec::new();
        let mut origins = Vec::new();
        let mut modes = Vec::new();
        let mut injections = Vec::new();
        let mut last_tools: Vec<Option<i32>> = Vec::new();
        let mut last_outcomes = Vec::new();
        let mut last_user_agents: Vec<Option<String>> = Vec::new();
        let mut last_seen = Vec::new();
        for (key, stat) in &batch.sites {
            keys.push(key.clone());
            slugs.push(stat.api_slug.clone());
            origins.push(stat.origin.clone());
            modes.push(stat.mode.to_string());
            injections.push(stat.injections);
            last_tools.push(stat.last_tools);
            last_outcomes.push(stat.last_outcome.to_string());
            last_user_agents.push(stat.last_user_agent.clone());
            last_seen.push(stat.last_seen);
        }

        // first_seen_at is only ever set on insert: the point of the column is
        // "since when has this site carried the shim". last_tools keeps its
        // previous value when the newest run registered nothing.
        let result = sqlx::query(
            r#"INSERT INTO webmcp_sites (
                   site_key, api_slug, origin, mode, injections,
                   last_tools, last_outcome, last_user_agent, first_seen_at, last_seen_at)
               SELECT k, slug, orig, m, n, tools, outcome, ua, seen, seen
                 FROM unnest($1::text[], $2::text[], $3::text[], $4::text[], $5::bigint[],
                             $6::int[], $7::text[], $8::text[], $9::timestamptz[])
                   AS t(k, slug, orig, m, n, tools, outcome, ua, seen)
               ON CONFLICT (site_key) DO UPDATE SET
                   injections = webmcp_sites.injections + excluded.injections,
                   last_tools = coalesce(excluded.last_tools, webmcp_sites.last_tools),
                   last_outcome = excluded.last_outcome,
                   last_user_agent = coalesce(excluded.last_user_agent, webmcp_sites.last_user_agent),
                   last_seen_at = greatest(webmcp_sites.last_seen_at, excluded.last_seen_at),
                   mode = excluded.mode"#,
        )
        .bind(&keys)
        .bind(&slugs)
        .bind(&origins)
        .bind(&modes)
        .bind(&injections)
        .bind(&last_tools)
        .bind(&last_outcomes)
        .bind(&last_user_agents)
        .bind(&last_seen)
        .execute(&self.pool)
        .await;
        match result {
            Ok(_) => true,
            Err(err) => {
                warn!("webmcp sites flush failed ({} keys): {err}", keys.len());
                false
            }
        }
    }
}

fn json_response(status: StatusCode, body: serde_json::Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| Response::new(Body::empty()))
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
    use axum::http::{HeaderName, HeaderValue};

    #[test]
    fn outcomes_collapse_to_a_bounded_vocabulary() {
        assert_eq!(outcome_label(Some("registered")), Some("registered"));
        assert_eq!(outcome_label(Some(" ok ")), Some("registered"));
        assert_eq!(outcome_label(Some("empty")), Some("empty"));
        assert_eq!(outcome_label(Some("failed")), Some("failed"));
        // Anything invented by a caller is rejected, not stored.
        assert_eq!(outcome_label(Some("registered-ish")), None);
        assert_eq!(outcome_label(None), None);
    }

    #[test]
    fn origin_comes_from_the_browser_not_the_body() {
        let mut headers = HeaderMap::new();
        headers.insert(ORIGIN, HeaderValue::from_static("https://Provider.com"));
        assert_eq!(
            page_origin(&headers).as_deref(),
            Some("https://provider.com")
        );

        // No Origin (a same-origin POST): fall back to the Referer's origin,
        // path and query discarded.
        let mut referer_only = HeaderMap::new();
        referer_only.insert(
            REFERER,
            HeaderValue::from_static("https://provider.com/docs/pricing?x=1"),
        );
        assert_eq!(
            page_origin(&referer_only).as_deref(),
            Some("https://provider.com")
        );

        // A sandboxed iframe sends `Origin: null` — not a site.
        let mut null_origin = HeaderMap::new();
        null_origin.insert(ORIGIN, HeaderValue::from_static("null"));
        assert_eq!(page_origin(&null_origin), None);

        assert_eq!(page_origin(&HeaderMap::new()), None);
    }

    #[test]
    fn client_ip_prefers_the_fly_proxy_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("fly-client-ip"),
            HeaderValue::from_static("203.0.113.9"),
        );
        headers.insert(
            HeaderName::from_static("x-forwarded-for"),
            HeaderValue::from_static("198.51.100.4, 10.0.0.1"),
        );
        assert_eq!(client_ip(&headers), Some("203.0.113.9".to_string()));
    }

    /// A run of the shim, for the tests below.
    fn run(outcome: &'static str, tools: i64) -> Injection<'static> {
        Injection {
            api_slug: "weather",
            origin: "https://p.com",
            outcome,
            mode: "listing",
            ua_class: "browser",
            tools,
            user_agent: None,
        }
    }

    #[test]
    fn a_disabled_tracker_records_nothing_and_never_panics() {
        WebmcpTracker::disabled().record(&run("registered", 7));
    }

    // A lazy pool handle needs a Tokio context to be constructed, even though
    // nothing here ever connects: flush() is the only thing that touches it.
    #[tokio::test]
    async fn tools_accumulate_per_key_and_sites_keep_the_last_successful_count() {
        let tracker = WebmcpTracker {
            inner: Some(Arc::new(Inner {
                buffer: Mutex::new(Buffer::default()),
                // A pool handle is only touched by flush(), which this test
                // never calls.
                pool: PgPool::connect_lazy("postgres://localhost/postgres").unwrap(),
                live: AtomicBool::new(true),
                consecutive_failures: AtomicU32::new(0),
            })),
        };

        tracker.record(&run("registered", 7));
        tracker.record(&run("registered", 7));
        // A later failure must not erase the tool count we last saw work.
        tracker.record(&run("failed", 0));

        let inner = tracker.inner.as_ref().unwrap();
        let buffer = inner.buffer.lock().unwrap();
        let registered = buffer
            .injections
            .iter()
            .find(|(k, _)| k.outcome == "registered")
            .map(|(_, v)| v.clone())
            .unwrap();
        assert_eq!(registered.injections, 2);
        assert_eq!(registered.tools, 14);

        // One site row for all three runs, and it counts every run.
        assert_eq!(buffer.sites.len(), 1);
        let site = buffer.sites.values().next().unwrap();
        assert_eq!(site.injections, 3);
        assert_eq!(site.last_outcome, "failed");
        assert_eq!(site.last_tools, Some(7));
    }
}
