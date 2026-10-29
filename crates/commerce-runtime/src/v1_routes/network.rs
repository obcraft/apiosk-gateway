//! `GET /v1/network` — who is on the network, as a name and a mark.
//!
//! Every other browse endpoint answers a question about what can be BOUGHT:
//! `/v1/apis` lists listings, `/v1/sources/{id}/apis` lists an index's
//! providers, `/v1/discover` ranks offers against a job. None of them answers
//! the first question a person asks in front of a homepage — "who is actually
//! in there?" — without being handed a page of prices, endpoint counts and
//! rails to read past.
//!
//! This one answers only that. A name, a mark, and where it came from. No
//! price, no endpoint count, no ranking: a strip of logos under a hero is a
//! claim about WHO, and anything else on it is either noise or a promise the
//! strip cannot keep.
//!
//! # Two corpora, one list
//!
//! * `listed` — providers in the reviewed Apiosk catalogue, from
//!   `provider_profiles`. Sold through the gateway.
//! * `indexed` — external x402 sources that ARE one seller: a storefront, a
//!   manifest, a single 402 challenge. Their `provider` and `logo_url` columns
//!   are the identity an admin recorded when the source was added.
//!
//! An aggregator — the Coinbase Bazaar, thirdweb, PayAI — has neither column
//! set, because it publishes many sellers and so is none of them. It therefore
//! falls out of this list by construction, which is the right answer twice
//! over: there is no name to show, and putting Coinbase's mark on a strip of
//! "who is on the network" would claim a relationship that reading a public
//! index is not.
//!
//! # What this deliberately does not do
//!
//! It does not sweep. Both corpora are read from tables the gateway already
//! caches, so a marquee refreshing every ten minutes costs nobody's rate limit
//! and no index is called. A list of who exists must not be paid for in calls
//! to the people on it.
//!
//! And it does not guess a mark. `logo_url` is the one the record carries or
//! it is null — a favicon guessed from a hostname 404s often enough that
//! writing it into a response would send other people's agents a broken image,
//! whereas a client that guesses can walk to the next candidate on `onerror`
//! and nobody ever sees the miss. `site_url` is published so a client CAN make
//! that guess; see the app's `markCandidates`.

use std::collections::HashSet;
use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Query, State},
    http::StatusCode,
    response::Response,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::sources::{self, Fetch};
use crate::AppState;

/// Everything, by default. The whole network is a few dozen names and a
/// marquee slices it client-side anyway; paging a list this short would buy a
/// second request and a worse first render.
const DEFAULT_LIMIT: usize = 200;
const MAX_LIMIT: usize = 500;

#[derive(Debug, Default, Deserialize)]
pub struct NetworkParams {
    limit: Option<usize>,
    /// `listed`, `indexed`, or absent for both.
    kind: Option<String>,
    /// `true` drops the entries with no mark of their own.
    with_logo: Option<bool>,
}

/// One name on the strip.
struct Brand {
    name: String,
    /// The mark the record carries, https only. Never a guess.
    logo_url: Option<String>,
    /// Where this is read from, so a client can guess a favicon off it.
    site_url: Option<String>,
    /// `listed` (reviewed Apiosk catalogue) or `indexed` (external source).
    kind: &'static str,
    /// The provider handle or the source id — whichever names it in its corpus.
    id: String,
    /// Gateway path that opens it. Relative, like `endpoints_url` elsewhere.
    href: String,
}

impl Brand {
    fn json(&self) -> Value {
        json!({
            "name": self.name,
            "logo_url": self.logo_url,
            "site_url": self.site_url,
            "kind": self.kind,
            "id": self.id,
            "href": self.href,
        })
    }
}

/// `GET /v1/network` — every name on the network, with its mark.
pub async fn list_network(
    State(state): State<Arc<AppState>>,
    Query(params): Query<NetworkParams>,
) -> Response<Body> {
    let want = params
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty());
    let want_listed = !matches!(want, Some("indexed"));
    let want_indexed = !matches!(want, Some("listed"));

    // A corpus that cannot be read costs its own half of the strip, not the
    // response: a marquee missing the catalogue still says something true, and
    // `degraded` says which half is missing rather than leaving a short list to
    // read as a small network.
    let mut degraded = false;

    let mut listed: Vec<Brand> = Vec::new();
    if want_listed {
        match super::catalog::provider_directory(&state, None).await {
            Ok(providers) => {
                for provider in providers {
                    // The pool every unaffiliated listing hangs under. A real
                    // row in the directory and not a brand: it has no mark, and
                    // "Community providers" on a strip of logos reads as a
                    // company nobody has heard of rather than as a bucket.
                    if provider.username == crate::db::UNAFFILIATED_PROVIDER_USERNAME {
                        continue;
                    }
                    listed.push(Brand {
                        name: provider.name.trim().to_string(),
                        logo_url: https_only(provider.avatar_url.as_deref()),
                        // A profile records no website, so the only address a
                        // listed provider has here is the one its mark is
                        // served from — and that is as often our storage bucket
                        // as its own domain. Left null rather than published
                        // wrong; the mark is already carried.
                        site_url: None,
                        kind: "listed",
                        href: format!("/v1/providers/{}", provider.username),
                        id: provider.username,
                    });
                }
            }
            Err(err) => {
                tracing::error!("/v1/network: provider directory unreadable: {}", err);
                degraded = true;
            }
        }
    }

    let mut indexed: Vec<Brand> = Vec::new();
    if want_indexed {
        for source in sources::active_sources(&state).await {
            // No name is an aggregator, and an aggregator is not a seller.
            let Some(name) = source
                .provider
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty())
            else {
                continue;
            };
            let url = match &source.fetch {
                Fetch::Indexed { url } => url.clone(),
                Fetch::Queried { template } => template.clone(),
            };
            indexed.push(Brand {
                name: name.to_string(),
                logo_url: https_only(source.logo_url.as_deref()),
                site_url: origin_of(&url),
                kind: "indexed",
                href: format!("/v1/sources/{}/apis", source.id),
                id: source.id,
            });
        }
    }

    // Listed first into the fold, so a company that is both a reviewed provider
    // and an external storefront keeps the entry that links to what we sell.
    let mut entries = dedupe(listed.into_iter().chain(indexed));

    if params.with_logo.unwrap_or(false) {
        entries.retain(|brand| brand.logo_url.is_some());
    }

    // Alphabetical, not ranked. The strip claims one network rather than a
    // league table, and any other order would be read as one — while a stable
    // order is what lets a cached page and a fresh one look the same.
    entries.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });

    let total = entries.len();
    let listed_count = entries.iter().filter(|b| b.kind == "listed").count();
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    entries.truncate(limit);

    json_response(json!({
        "entries": entries.iter().map(Brand::json).collect::<Vec<_>>(),
        "meta": {
            "returned": entries.len(),
            "count": total,
            "listed": listed_count,
            "indexed": total - listed_count,
            "degraded": degraded,
        },
        "note": "Who is on the network: a name and the mark the record carries, nothing else. \
                 `listed` is the reviewed Apiosk catalogue; `indexed` is an external x402 source \
                 that is itself one seller. Aggregator indexes publish many sellers and are none \
                 of them, so they are not here — see GET /v1/sources for those. `logo_url` is \
                 never guessed: it is null when the record carries no mark, and `site_url` is \
                 there so a caller can try a favicon and fall back to a letter.",
    }))
}

/// A mark the browser will actually load.
///
/// https only, exactly as the `discovery_sources` constraint requires: an http
/// image inside an https page is a request the browser refuses outright, so an
/// http logo is not a slower mark, it is no mark with a console error attached.
fn https_only(value: Option<&str>) -> Option<String> {
    let url = value?.trim();
    if url.len() > 8 && url[..8].eq_ignore_ascii_case("https://") {
        Some(url.to_string())
    } else {
        None
    }
}

/// `https://x402.forgemesh.io/.well-known/x402` → `https://x402.forgemesh.io`.
fn origin_of(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = rest.split(['/', '?', '#']).next()?.trim();
    if host.is_empty() || !host.contains('.') {
        return None;
    }
    Some(format!("https://{host}"))
}

/// `api.cdp.coinbase.com` → `coinbase.com`; `zaref.dev` → `zaref.dev`.
///
/// Last two labels, which is wrong for the handful of two-part suffixes like
/// `co.uk` — and wrong there costs one entry that was going to be deduped
/// against another `co.uk`, which is a cosmetic loss on a marquee and not a
/// wrong fact anywhere.
fn registrable(host: &str) -> String {
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() <= 2 {
        return host.to_string();
    }
    labels[labels.len() - 2..].join(".")
}

/// The name, reduced to what is left when punctuation and case stop mattering.
fn name_key(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// One entry per company.
///
/// Three collisions, because one company reaches this list three ways: under
/// its own name twice (a provider profile and a storefront source), under two
/// storefronts on the same domain, or under a longer name that opens with a
/// name already on the strip — `ForgeMesh`, `ForgeMesh Utility APIs` and
/// `ForgeMesh Disruption Intelligence` are all three at once in the live data.
///
/// The prefix test is on the display name and requires a non-alphanumeric
/// character at the seam, so `ForgeMesh Utility APIs` collapses into
/// `ForgeMesh` while `Signalis` survives a hypothetical `Signal`.
///
/// First in wins, which is why the caller feeds the reviewed catalogue first.
fn dedupe(brands: impl Iterator<Item = Brand>) -> Vec<Brand> {
    let mut keys: HashSet<String> = HashSet::new();
    let mut kept_names: Vec<String> = Vec::new();
    let mut out: Vec<Brand> = Vec::new();

    for brand in brands {
        if brand.name.is_empty() {
            continue;
        }
        let key = name_key(&brand.name);
        if key.is_empty() || keys.contains(&key) {
            continue;
        }
        let domain = brand
            .site_url
            .as_deref()
            .and_then(origin_of)
            .map(|origin| registrable(origin.trim_start_matches("https://")));
        if domain.as_ref().is_some_and(|d| keys.contains(d)) {
            continue;
        }

        let lower = brand.name.to_lowercase();
        let extends_kept = kept_names.iter().any(|kept| {
            lower.len() > kept.len()
                && lower.starts_with(kept.as_str())
                && !lower[kept.len()..].starts_with(|c: char| c.is_alphanumeric())
        });
        if extends_kept {
            continue;
        }

        keys.insert(key);
        if let Some(domain) = domain {
            keys.insert(domain);
        }
        kept_names.push(lower);
        out.push(brand);
    }

    out
}

/// Five minutes, matching the payload cache behind both corpora. A name and a
/// mark change on the order of never, and the caller of this is a landing page
/// that rebuilds on a timer.
fn json_response(body: Value) -> Response<Body> {
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header("Cache-Control", "public, max-age=300")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brand(name: &str, kind: &'static str, site: Option<&str>) -> Brand {
        Brand {
            name: name.to_string(),
            logo_url: None,
            site_url: site.map(str::to_string),
            kind,
            id: name.to_lowercase().replace(' ', "-"),
            href: String::new(),
        }
    }

    fn names(brands: Vec<Brand>) -> Vec<String> {
        brands.into_iter().map(|b| b.name).collect()
    }

    #[test]
    fn an_http_mark_is_no_mark() {
        assert_eq!(
            https_only(Some("https://zaref.dev/favicon.svg")).as_deref(),
            Some("https://zaref.dev/favicon.svg")
        );
        assert_eq!(https_only(Some("http://zaref.dev/favicon.svg")), None);
        assert_eq!(https_only(Some("  ")), None);
        assert_eq!(https_only(None), None);
    }

    #[test]
    fn a_site_is_the_origin_of_the_url_the_source_is_read_from() {
        assert_eq!(
            origin_of("https://x402.forgemesh.io/.well-known/x402").as_deref(),
            Some("https://x402.forgemesh.io")
        );
        assert_eq!(
            origin_of("https://api.cdp.coinbase.com/v2/search?limit=20&query={query}").as_deref(),
            Some("https://api.cdp.coinbase.com")
        );
        assert_eq!(origin_of("not a url"), None);
        assert_eq!(origin_of("https://localhost/x"), None);
    }

    #[test]
    fn the_same_company_under_two_names_lands_on_the_strip_once() {
        let out = dedupe(
            vec![
                brand("ForgeMesh", "listed", None),
                brand(
                    "ForgeMesh Utility APIs",
                    "indexed",
                    Some("https://x402.forgemesh.io/x"),
                ),
                brand(
                    "ForgeMesh Disruption Intelligence",
                    "indexed",
                    Some("https://disruption.forgemesh.io/x"),
                ),
            ]
            .into_iter(),
        );
        assert_eq!(names(out), vec!["ForgeMesh"]);
    }

    #[test]
    fn two_storefronts_on_one_domain_are_one_company() {
        let out = dedupe(
            vec![
                brand("Utility APIs", "indexed", Some("https://x402.acme.io/a")),
                brand("Disruption Feed", "indexed", Some("https://data.acme.io/b")),
            ]
            .into_iter(),
        );
        assert_eq!(names(out), vec!["Utility APIs"]);
    }

    #[test]
    fn a_name_that_merely_starts_with_another_is_its_own_company() {
        let out = dedupe(
            vec![
                brand("Signal", "listed", None),
                brand("Signalis", "listed", None),
            ]
            .into_iter(),
        );
        assert_eq!(names(out), vec!["Signal", "Signalis"]);
    }

    #[test]
    fn punctuation_and_case_do_not_make_a_second_company() {
        let out = dedupe(
            vec![
                brand("TensorFeed.ai", "listed", None),
                brand("tensorfeed ai", "indexed", None),
            ]
            .into_iter(),
        );
        assert_eq!(names(out), vec!["TensorFeed.ai"]);
    }

    #[test]
    fn different_companies_all_survive() {
        let out = dedupe(
            vec![
                brand("Finnhub", "listed", None),
                brand("zaref", "indexed", Some("https://zaref.dev/x")),
                brand("NetIntel", "indexed", Some("https://netintel.dev/x")),
            ]
            .into_iter(),
        );
        assert_eq!(names(out), vec!["Finnhub", "zaref", "NetIntel"]);
    }
}
