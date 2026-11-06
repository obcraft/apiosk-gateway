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
//! Same core as [`crate::discovery_traffic`] ([`crate::counters`]): counters
//! are buffered in-process and flushed on a timer, every buffer is bounded, and
//! nothing here may delay, alter or fail a response. Only browsers that expose
//! a `modelContext` report, so this counts agent-capable page loads — never
//! page views at large, and never anything about the visitor.

use std::{collections::HashMap, sync::Arc};

use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{
        header::{ORIGIN, REFERER, USER_AGENT},
        HeaderMap, StatusCode,
    },
    response::Response,
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use sqlx::PgPool;

use crate::{
    counters::{report, CounterBuffer, Spec, Tracker},
    http::{json, throttled_by_ip},
    rate_limit::RateLimitPolicy,
    util::{sha256_hex, truncate_chars},
    AppState,
};

/// Buffer caps. Both key spaces are bounded in practice (slugs by the catalog,
/// origins by the set of sites that embedded the shim), so hitting these means
/// something pathological — count it, log it, and keep serving.
const MAX_INJECTION_KEYS: usize = 20_000;
const MAX_SITE_KEYS: usize = 20_000;

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

static SPEC: Spec = Spec {
    name: "webmcp injection",
    enabled_var: "WEBMCP_TRACKING_ENABLED",
    interval_var: "WEBMCP_FLUSH_INTERVAL_MS",
    migration: "074_webmcp_injections",
};

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

/// One validated shim run, as handed to [`WebmcpTracker::record_injection`].
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
pub(crate) struct Buffer {
    injections: HashMap<InjectionKey, InjectionCount>,
    sites: HashMap<String, SiteStat>,
    /// Counters dropped because a buffer was full, reported once per flush.
    dropped: u64,
}

impl CounterBuffer for Buffer {
    fn is_empty(&self) -> bool {
        self.injections.is_empty() && self.sites.is_empty()
    }

    fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Each statement is independent: a failure in one aggregate is logged and
    /// does not discard the other.
    async fn flush(&self, pool: &PgPool) -> bool {
        flush_injections(self, pool).await & flush_sites(self, pool).await
    }
}

/// Handle stored on `AppState`. Cloning is cheap; every method is non-blocking.
pub(crate) type WebmcpTracker = Tracker<Buffer>;

impl WebmcpTracker {
    /// Start the tracker and its flush loop (`WEBMCP_TRACKING_ENABLED=false`
    /// turns it off).
    pub fn start(pool: PgPool, tasks: &crate::background::BackgroundTasks) -> Self {
        Tracker::launch(pool, tasks, &SPEC)
    }

    /// Record one shim run.
    fn record_injection(&self, run: &Injection<'_>) {
        let Injection {
            api_slug,
            origin,
            outcome,
            mode,
            ua_class,
            tools,
            user_agent,
        } = *run;

        self.record(|buffer, now| {
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
                buffer.injections.insert(key, InjectionCount { injections: 1, tools });
            } else {
                buffer.dropped += 1;
            }

            // One row per (listing, site): which provider pages carry the shim.
            let site_key = sha256_hex(&format!("{api_slug}|{origin}"));
            let known = buffer.sites.contains_key(&site_key);
            if !known && buffer.sites.len() >= MAX_SITE_KEYS {
                buffer.dropped += 1;
                return;
            }
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
                entry.last_user_agent = Some(truncate_chars(user_agent, MAX_USER_AGENT_CHARS));
            }
        });
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
            return json(
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
        return json(
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
            return json(
                StatusCode::BAD_REQUEST,
                serde_json::json!({
                    "ok": false,
                    "error": "slug_required",
                    "message": "Name the listing the shim was configured for: {\"slug\": \"...\"}.",
                }),
            );
        }
        raw => truncate_chars(&raw.to_lowercase(), MAX_SLUG_CHARS),
    };

    // Per-IP flood guard. Keyed on the IP alone: a page that reloads in a loop
    // is the case this exists for.
    if let Some(throttled) = throttled_by_ip(
        &state,
        &headers,
        "webmcp",
        REPORT_RATE_LIMIT,
        "Too many reports from this address. Try again shortly.",
    )
    .await
    {
        return throttled;
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

    state.webmcp.record_injection(&Injection {
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
    Some(truncate_chars(&raw.to_lowercase(), MAX_ORIGIN_CHARS))
}

/// `true` when there was nothing to write or the write succeeded.
async fn flush_injections(batch: &Buffer, pool: &PgPool) -> bool {
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
    .execute(pool)
    .await;
    report("webmcp injections", days.len(), result)
}

/// `true` when there was nothing to write or the write succeeded.
async fn flush_sites(batch: &Buffer, pool: &PgPool) -> bool {
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
    .execute(pool)
    .await;
    report("webmcp sites", keys.len(), result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

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
        WebmcpTracker::disabled().record_injection(&run("registered", 7));
    }

    #[tokio::test]
    async fn tools_accumulate_per_key_and_sites_keep_the_last_successful_count() {
        let tracker = WebmcpTracker::in_memory(&SPEC);
        tracker.record_injection(&run("registered", 7));
        tracker.record_injection(&run("registered", 7));
        // A later failure must not erase the tool count we last saw work.
        tracker.record_injection(&run("failed", 0));

        tracker
            .peek(|buffer| {
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
            })
            .unwrap();
    }
}
