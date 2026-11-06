//! Federated traffic monitoring — proving Apiosk is a distribution channel.
//!
//! A first-party listing is proxied, so every call is already visible: a
//! `gateway_usage` row, a settlement, a payout. A FEDERATED listing is not.
//! Apiosk indexes it and the agent pays the provider directly, which leaves us
//! with nothing to count — the whole reason the `X-Apiosk-Ref` marker exists
//! (see `config::Config::federated_ref_*`).
//!
//! (This used to cite `gateway_requests_v2`. That table is telemetry v2, which
//! is gated behind `TELEMETRY_V2_ENABLED` and is off in production, so it is
//! empty — audit B1. The billing tables above are what actually carries
//! first-party visibility today.)
//!
//! This module fills the gap on the paths Apiosk *does* own, all of them ahead
//! of the payment:
//!
//!   * **discovery** — a read of `/.well-known/x402`, counted per federated
//!     listing it advertises.
//!   * **detail** — a view of a federated listing's catalog detail page.
//!   * **click** — an outbound hop through [`federated_redirect`]
//!     (`GET /f/:slug`), the tracked link Apiosk hands out wherever it points a
//!     human or an agent at a federated provider.
//!
//! What this deliberately does NOT do is sit on the x402 resource URL itself.
//! Redirecting the paid call through the gateway would make the provider's 402
//! challenge advertise a `resource` that disagrees with the URL the client
//! called, would risk the `PAYMENT-SIGNATURE` header being dropped across a
//! cross-origin redirect, and would put Apiosk back into a money path it
//! deliberately stays out of. Federated means federated.
//!
//! Every write here is fire-and-forget and fails open: a dead counter must
//! never delay, alter or fail a response.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{
        header::{CACHE_CONTROL, LOCATION, REFERER, USER_AGENT},
        HeaderMap, StatusCode,
    },
    response::Response,
};
use serde::Deserialize;
use tracing::warn;

use crate::AppState;

/// Which step of the federated funnel a hit belongs to. Mirrors the `event`
/// check constraint on `federated_traffic_daily` (migration 064).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FederatedEvent {
    /// `/.well-known/x402` was read, and this listing was in the document.
    Discovery,
    /// The listing's catalog detail page (`/v1/apis/:slug`) was viewed.
    Detail,
    /// A caller followed the tracked `/f/:slug` hop out to the provider.
    Click,
}

impl FederatedEvent {
    fn as_str(self) -> &'static str {
        match self {
            FederatedEvent::Discovery => "discovery",
            FederatedEvent::Detail => "detail",
            FederatedEvent::Click => "click",
        }
    }
}

/// Defensive caps — these columns are unbounded `text`, and the values come
/// straight off the wire.
const MAX_USER_AGENT_CHARS: usize = 512;
const MAX_REFERER_CHARS: usize = 1_024;
const MAX_URL_CHARS: usize = 2_048;

/// Coarse caller class, so a discovery crawler polling us every minute is never
/// mistaken for real demand. Order matters: a crawler that advertises itself
/// with a browser-shaped UA (GPTBot, most indexers) must classify as a crawler,
/// so that check runs first.
pub fn ua_class(headers: &HeaderMap) -> &'static str {
    let Some(ua) = headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return "unknown";
    };
    let ua = ua.to_ascii_lowercase();

    const CRAWLER: [&str; 8] = [
        "bot", "crawler", "spider", "slurp", "bazaar", "monitor", "uptime", "scraper",
    ];
    if CRAWLER.iter().any(|needle| ua.contains(needle)) {
        return "crawler";
    }

    // Machine clients: x402 tooling, agent frameworks and the plain HTTP
    // libraries agents are built on.
    const AGENT: [&str; 12] = [
        "x402",
        "apiosk",
        "openai",
        "gpt",
        "claude",
        "anthropic",
        "langchain",
        "python-requests",
        "httpx",
        "axios",
        "node-fetch",
        "go-http-client",
    ];
    if AGENT.iter().any(|needle| ua.contains(needle)) {
        return "agent";
    }

    if ua.starts_with("mozilla/") {
        return "browser";
    }

    // curl/undici/wget and anything else scripted.
    if ua.starts_with("curl")
        || ua.starts_with("wget")
        || ua.contains("undici")
        || ua.contains("okhttp")
        || ua.contains("java/")
    {
        return "agent";
    }

    "unknown"
}

/// Increment the day counter for `slugs`. Non-blocking: the caller gets its
/// response back immediately and the upsert lands on a detached task. A DB
/// error is logged once and dropped — monitoring never blocks traffic.
pub fn bump(state: &Arc<AppState>, slugs: Vec<String>, event: FederatedEvent, ua_class: &str) {
    if slugs.is_empty() {
        return;
    }
    let pool = state.db.pool.clone();
    let ua_class = ua_class.to_string();
    tokio::spawn(async move {
        let result = sqlx::query(
            r#"INSERT INTO federated_traffic_daily (day, api_slug, event, ua_class, hits)
               SELECT (now() AT TIME ZONE 'utc')::date, s, $2, $3, 1
                 FROM unnest($1::text[]) AS s
               ON CONFLICT (day, api_slug, event, ua_class)
               DO UPDATE SET hits = federated_traffic_daily.hits + 1,
                             updated_at = now()"#,
        )
        .bind(&slugs)
        .bind(event.as_str())
        .bind(&ua_class)
        .execute(&pool)
        .await;
        if let Err(e) = result {
            warn!(
                "federated traffic counter failed ({} slugs): {e}",
                slugs.len()
            );
        }
    });
}

#[derive(Debug, Deserialize)]
pub struct RedirectParams {
    /// Optional explicit target. Only honoured when it exactly matches a URL
    /// the listing itself registered — see [`resolve_target`]. Anything else is
    /// a 404, so this endpoint can never be used as an open redirect.
    #[serde(default)]
    pub r: Option<String>,
}

/// `GET /f/:slug` — the tracked outbound hop to a federated provider.
///
/// Records the click, then 302s to the provider. Everything about the redirect
/// is derived from the listing's own registered URLs; nothing a caller supplies
/// can steer it somewhere else.
pub async fn federated_redirect(
    State(state): State<Arc<AppState>>,
    Path(slug): Path<String>,
    Query(params): Query<RedirectParams>,
    headers: HeaderMap,
) -> Response {
    let slug = slug.trim().to_string();
    let Some(target) = resolve_target(&state, &slug, params.r.as_deref()).await else {
        return not_found();
    };
    let host = host_of(&target).unwrap_or_default();
    let class = ua_class(&headers);

    record_click(&state, &slug, &target, &host, class, &headers);
    bump(&state, vec![slug], FederatedEvent::Click, class);

    Response::builder()
        .status(StatusCode::FOUND)
        .header(LOCATION, target)
        // A cached redirect is an uncounted redirect.
        .header(CACHE_CONTROL, "no-store")
        .body(Body::empty())
        .unwrap_or_else(|_| not_found())
}

fn not_found() -> Response {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"error":"unknown_federated_listing","message":"No active federated listing with that slug, or the requested target is not one of its registered URLs."}"#,
        ))
        .unwrap()
}

/// The URLs a federated listing registered: its published x402 resources, plus
/// the entry points (docs, discovery document, origin). Together they are the
/// redirect allowlist.
type FederatedListingUrls = (
    serde_json::Value,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// Resolve where `/f/:slug` should send the caller, from the listing's own
/// registered URLs. See [`pick_target`] for the allowlist rule.
async fn resolve_target(
    state: &Arc<AppState>,
    slug: &str,
    requested: Option<&str>,
) -> Option<String> {
    if slug.is_empty() || slug.len() > 128 {
        return None;
    }
    let row: Option<FederatedListingUrls> = sqlx::query_as(
        r#"SELECT COALESCE(external_x402, '[]'::jsonb),
                      docs_url,
                      external_discovery_url,
                      origin_url
                 FROM apis
                WHERE slug = $1
                  AND COALESCE(listing_type::text, 'api') = 'federated'
                  AND status = 'active'
                  AND COALESCE(active, TRUE) = TRUE
                LIMIT 1"#,
    )
    .bind(slug)
    .fetch_optional(&state.db.pool)
    .await
    .map_err(|e| warn!("federated redirect lookup failed for {slug}: {e}"))
    .ok()
    .flatten();

    let (resources, docs_url, discovery_url, origin_url) = row?;
    pick_target(
        &resources,
        &[docs_url, discovery_url, origin_url],
        requested,
    )
}

/// The redirect rule, kept pure so it can be tested without a database.
///
/// With no `?r=`, the listing's first usable entry point (docs, then discovery
/// document, then origin). With `?r=<url>`, that URL must be byte-identical to
/// one the listing registered — a resource in `external_x402[]` or an entry
/// point — otherwise `None`, and the caller gets a 404.
///
/// Exact match is the whole open-redirect defence. Anything looser (prefix,
/// same-host, "starts with the provider domain") is how these endpoints get
/// turned into phishing relays, so it stays exact.
fn pick_target(
    resources: &serde_json::Value,
    entry_points: &[Option<String>],
    requested: Option<&str>,
) -> Option<String> {
    let entry_points: Vec<&str> = entry_points
        .iter()
        .filter_map(|url| url.as_deref())
        .map(str::trim)
        .filter(|url| is_http_url(url))
        .collect();

    let requested = requested.map(str::trim).filter(|url| !url.is_empty());
    let Some(requested) = requested else {
        return entry_points.first().map(|url| url.to_string());
    };
    if requested.len() > MAX_URL_CHARS {
        return None;
    }

    let registered = resources
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|res| res.get("resource").and_then(serde_json::Value::as_str))
                .any(|url| url == requested)
        })
        .unwrap_or(false);

    if registered || entry_points.contains(&requested) {
        Some(requested.to_string())
    } else {
        None
    }
}

/// Write the raw click row. Detached, same fail-open contract as [`bump`].
fn record_click(
    state: &Arc<AppState>,
    slug: &str,
    target_url: &str,
    target_host: &str,
    ua_class: &str,
    headers: &HeaderMap,
) {
    let pool = state.db.pool.clone();
    let slug = slug.to_string();
    let target_url = crate::util::truncate_chars(target_url, MAX_URL_CHARS);
    let target_host = target_host.to_string();
    let ua_class = ua_class.to_string();
    let user_agent = headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(|value| crate::util::truncate_chars(value, MAX_USER_AGENT_CHARS));
    let referer = headers
        .get(REFERER)
        .and_then(|value| value.to_str().ok())
        .map(|value| crate::util::truncate_chars(value, MAX_REFERER_CHARS));
    let ip_hash =
        crate::traffic::client_ip_from_headers(headers).map(|ip| crate::traffic::hash_ip(&ip));

    tokio::spawn(async move {
        let result = sqlx::query(
            r#"INSERT INTO federated_referral_clicks
                 (api_slug, target_url, target_host, ua_class, user_agent, ip_hash, referer)
               VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
        )
        .bind(&slug)
        .bind(&target_url)
        .bind(&target_host)
        .bind(&ua_class)
        .bind(&user_agent)
        .bind(&ip_hash)
        .bind(&referer)
        .execute(&pool)
        .await;
        if let Err(e) = result {
            warn!("federated click log failed for {slug}: {e}");
        }
    });
}

/// Host of an absolute http(s) URL, lowercased and without port. Deliberately
/// hand-rolled (no `url` crate dependency) and matches the host extraction the
/// external-fetch payer proxy already uses.
pub fn host_of(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = rest
        .split('/')
        .next()?
        .split('?')
        .next()?
        .split('#')
        .next()?
        .rsplit('@')
        .next()?;
    let host = host.split(':').next()?.trim().to_ascii_lowercase();
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

fn is_http_url(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://")) && host_of(url).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers_with_ua(ua: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_str(ua).unwrap());
        headers
    }

    #[test]
    fn crawlers_beat_browser_shaped_user_agents() {
        // GPTBot advertises a Mozilla prefix; it is still a crawler, and
        // counting it as a browser would invent human demand that isn't there.
        let headers = headers_with_ua("Mozilla/5.0 AppleWebKit (compatible; GPTBot/1.2)");
        assert_eq!(ua_class(&headers), "crawler");
    }

    #[test]
    fn classifies_agents_browsers_and_unknown() {
        assert_eq!(ua_class(&headers_with_ua("x402-fetch/0.4.2")), "agent");
        assert_eq!(
            ua_class(&headers_with_ua("python-requests/2.31.0")),
            "agent"
        );
        assert_eq!(ua_class(&headers_with_ua("curl/8.4.0")), "agent");
        assert_eq!(
            ua_class(&headers_with_ua(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Safari/605.1.15"
            )),
            "browser"
        );
        assert_eq!(ua_class(&HeaderMap::new()), "unknown");
    }

    #[test]
    fn extracts_host_without_port_or_credentials() {
        assert_eq!(
            host_of("https://api.example.com/x/y?a=1"),
            Some("api.example.com".into())
        );
        assert_eq!(
            host_of("http://API.Example.com:8080/x"),
            Some("api.example.com".into())
        );
        assert_eq!(host_of("ftp://api.example.com"), None);
        assert_eq!(host_of("not a url"), None);
    }

    #[test]
    fn rejects_non_http_entry_points() {
        assert!(is_http_url("https://api.example.com/.well-known/x402"));
        assert!(!is_http_url("javascript:alert(1)"));
        assert!(!is_http_url(""));
    }

    fn listing() -> (serde_json::Value, Vec<Option<String>>) {
        let resources = serde_json::json!([
            { "resource": "https://api.example.com/v1/whois" },
            { "resource": "https://api.example.com/v1/dns" },
        ]);
        let entry_points = vec![
            None,
            Some("https://api.example.com/.well-known/x402".to_string()),
            Some("https://api.example.com".to_string()),
        ];
        (resources, entry_points)
    }

    #[test]
    fn bare_hop_uses_the_first_usable_entry_point() {
        let (resources, entry_points) = listing();
        assert_eq!(
            pick_target(&resources, &entry_points, None),
            Some("https://api.example.com/.well-known/x402".to_string())
        );
    }

    #[test]
    fn honours_a_registered_resource_url() {
        let (resources, entry_points) = listing();
        assert_eq!(
            pick_target(
                &resources,
                &entry_points,
                Some("https://api.example.com/v1/dns")
            ),
            Some("https://api.example.com/v1/dns".to_string())
        );
    }

    #[test]
    fn refuses_anything_the_listing_did_not_register() {
        let (resources, entry_points) = listing();
        // An unregistered path on the provider's OWN host is still refused:
        // same-host matching is exactly the loophole that turns a tracking hop
        // into an open redirect.
        for hostile in [
            "https://evil.example.net/phish",
            "https://api.example.com/v1/whois/../../evil",
            "https://api.example.com.evil.net/v1/dns",
            "//evil.example.net",
            "javascript:alert(1)",
        ] {
            assert_eq!(
                pick_target(&resources, &entry_points, Some(hostile)),
                None,
                "should have refused {hostile}"
            );
        }
    }

    #[test]
    fn falls_back_to_nothing_when_the_listing_has_no_usable_urls() {
        assert_eq!(
            pick_target(
                &serde_json::json!([]),
                &[None, Some("not a url".into())],
                None
            ),
            None
        );
    }
}
