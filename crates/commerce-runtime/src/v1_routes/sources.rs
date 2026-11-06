//! `GET /v1/sources`, `/v1/sources/{id}/apis` and `/v1/sources/{id}/apis/{host}`
//! — the wider x402 ecosystem, browsable instead of only searchable.
//!
//! `/v1/discover` answers a question: describe a job, get the best few offers
//! across every index. That is the right shape for an agent and the wrong one
//! for a person who wants to look around, because the only way to see what is
//! out there is to already know what to ask for. These three endpoints walk the
//! same corpus the other way — index, then provider, then endpoint — so an
//! explore page can be built out of clicks rather than guesses.
//!
//! # The three levels
//!
//! * `/v1/sources` — the indexes the gateway sweeps, and which of them can be
//!   listed whole.
//! * `/v1/sources/{id}/apis` — the providers one index publishes. An index
//!   returns a flat list of resource URLs; the providers are recovered by
//!   grouping those on their host, which is the only provider identity an
//!   unreviewed offer actually carries.
//! * `/v1/sources/{id}/apis/{host}` — every endpoint of one provider, with the
//!   full metadata the index published: price, rail, method, declared input
//!   schema, declared output, and what Apiosk would do about settling it.
//!
//! # Two things this is careful about
//!
//! Offers go out through [`crate::sources::settled_json`], the same function
//! `/v1/discover` uses. That is not tidiness: it is what stops this surface
//! telling a reader to "pay the provider yourself" about an endpoint the
//! gateway would happily front, or quoting a price the payer would not honour.
//!
//! And a queried index has no "everything". The Bazaar, thirdweb and
//! agentic.market only answer questions, so listing them whole is not a feature
//! that was left out — there is no such request to make. They come back with
//! `requires_query: true` and refuse politely until `?q=` is supplied, rather
//! than returning an empty list that reads as an outage.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::bazaar::ExternalOffer;
use crate::http::{json, json_cached, refusal as error, throttled_by_ip};
use crate::rate_limit::RateLimitPolicy;
use crate::sources::{self, BrowseError, Browsed};
use crate::AppState;

/// Free and unauthenticated, and every miss is one call to somebody else's
/// index. The five-minute source cache absorbs a browsing session; this is
/// here for the case the cache cannot help — a queried source being typed into
/// repeatedly.
const BROWSE_RATE_LIMIT: RateLimitPolicy = RateLimitPolicy::per_minute(90);

/// Page size when the caller does not say. Generous: an explore list is read by
/// scrolling, and a page too small to fill a screen turns one look into four
/// requests.
const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;

/// The shelf's own ceiling, and why it is not [`MAX_LIMIT`].
///
/// Two hundred is the right cap on ENDPOINTS: each one carries a description,
/// a schema and a settlement note, so a bigger page is a bigger document for
/// no better answer. A provider card is a name, a host, a count and a price
/// range — an order of magnitude smaller — and the page built on it filters
/// and sorts over the whole list in the browser.
///
/// Capped at two hundred, that page would have shown "324 providers" while
/// holding 200, and every count, filter and sort under it would have been a
/// statement about a slice the reader was never told about. A shelf that
/// silently omits a third of the shop is worse than a slow one.
const SHELF_MAX_LIMIT: usize = 2_000;

#[derive(Debug, Default, Deserialize)]
pub struct BrowseParams {
    /// Required for a queried index, and an optional filter for an indexed one.
    q: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
    source: Option<String>,
    network: Option<String>,
    sort: Option<String>,
    #[serde(default)]
    count_only: bool,
}

/// `GET /v1/sources` — the indexes this gateway sweeps.
pub async fn list_sources(State(state): State<Arc<AppState>>) -> Response<Body> {
    let sources = sources::active_sources(&state).await;
    let browsable = sources.iter().filter(|s| s.is_browsable()).count();

    json_cached(
        StatusCode::OK,
        json!({
            "sources": sources.iter().map(sources::source_summary_json).collect::<Vec<_>>(),
            "meta": {
                "count": sources.len(),
                "browsable": browsable,
                "queried": sources.len() - browsable,
            },
            // Said here rather than left to be inferred from a list that does
            // not contain it. The reviewed catalogue is always searched and is
            // not one of these indexes, so a reader who took this list for the
            // whole reach would be undercounting by everything Apiosk sells.
            "note": "These are the EXTERNAL x402 indexes the gateway sweeps. The reviewed Apiosk catalogue is not one of them — it is always searched, and is browsable at GET /v1/apis.",
            "browse": {
                "apis": "/v1/sources/{id}/apis — the providers one index publishes",
                "endpoints": "/v1/sources/{id}/apis/{host} — every endpoint of one provider",
                "queried_sources": "Sources with requires_query=true only answer questions: add ?q=<what you need>.",
                "search_everything": "/v1/discover?q=<the job, in words> — all indexes at once, ranked",
            },
        }), "public, max-age=60")
}

/// `GET /v1/sources/apis` — every provider, across every index, as one shelf.
///
/// ---------------------------------------------------------------------------
/// Why this exists next to the per-source route
/// ---------------------------------------------------------------------------
///
/// `/v1/sources/{id}/apis` answers "who is in THIS index", which is the right
/// shape for drilling into one and the wrong one for a shelf. A page that
/// wants to show what Apiosk can reach had to call it once per index, merge
/// the answers in the browser, and re-merge them again whenever a source
/// finished at a different time — which meant every surface rebuilt the same
/// grouping, sorting and de-duplication, differently, in a place that cannot
/// see the whole corpus.
///
/// The gateway can see it. `crate::source_index` already holds each index's
/// catalogue grouped by provider, so answering across all of them is a merge
/// over memory rather than a fan-out over the network. One call, one ordering,
/// one definition of what a provider is.
///
/// The merge key is the HOST, not the host plus the index. Two facilitators
/// listing the same seller is the normal case — a provider registers with
/// both — and showing it twice would be describing the registry rather than
/// the ecosystem. Its endpoints are pooled, its price range spans both, and
/// `sources` on the card names every index it was found in, which is the fact
/// a reader actually wants: not "which list is this from" but "who can I buy
/// this from, and who is carrying them".
///
/// Queried indexes are skipped unless `?q=` is given: they publish no
/// catalogue, so with no question there is nothing of theirs to put on a
/// shelf. With a question they are swept like everything else.
pub async fn list_all_source_apis(
    State(state): State<Arc<AppState>>,
    Query(params): Query<BrowseParams>,
    headers: HeaderMap,
) -> Response<Body> {
    if let Some(limited) = throttled_by_ip(&state, &headers, "sources-browse", BROWSE_RATE_LIMIT, "Slow down — each miss costs an external index a request.").await {
        return limited;
    }

    let query = params.q.as_deref().map(str::trim).filter(|q| !q.is_empty());
    let sources = sources::active_sources(&state).await;

    // Everything the shelf is built from, keyed by host so a seller listed in
    // two indexes is one card.
    let mut groups: BTreeMap<String, Vec<ExternalOffer>> = BTreeMap::new();
    let mut found_in: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut per_source_hosts: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut read: Vec<Value> = Vec::new();

    for source in &sources {
        if params
            .source
            .as_deref()
            .is_some_and(|wanted| wanted != source.id)
        {
            continue;
        }
        // A queried index with no question has nothing to list. Reported as a
        // status rather than skipped silently: "this index is not on the shelf"
        // and "this index is broken" must not look the same.
        if !source.is_browsable() && query.is_none() {
            read.push(json!({
                "source": source.id,
                "status": "needs_query",
                "providers": 0,
                "note": "Only answers questions. Add ?q= to include it.",
            }));
            continue;
        }

        match sources::browse(&state, source, query).await {
            Ok(browsed) => {
                let offers = browsed.offers();
                let mut hosts: BTreeSet<String> = BTreeSet::new();
                for offer in offers {
                    if params
                        .network
                        .as_deref()
                        .is_some_and(|wanted| offer.network.as_deref() != Some(wanted))
                    {
                        continue;
                    }
                    hosts.insert(offer.host.clone());
                    found_in
                        .entry(offer.host.clone())
                        .or_default()
                        .insert(source.id.clone());
                    groups
                        .entry(offer.host.clone())
                        .or_default()
                        .push(offer.clone());
                }
                read.push(json!({
                    "source": source.id,
                    "status": "read",
                    "providers": hosts.len(),
                    "endpoints": offers.len(),
                }));
                per_source_hosts.insert(source.id.clone(), hosts);
            }
            Err(_) => {
                // One index failing must cost exactly that index. The shelf is
                // still the truth about every other one.
                read.push(json!({
                    "source": source.id,
                    "status": "unavailable",
                    "providers": 0,
                }));
            }
        }
    }

    if let Some(query) = query {
        let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        groups.retain(|host, offers| {
            let haystack = group_search_text(host, offers);
            terms.iter().all(|term| haystack.contains(term))
        });
    }

    // Which sources turned out to be wholesalers rather than shops. See the
    // note where this is used.
    let aggregators: BTreeSet<String> = per_source_hosts
        .iter()
        .filter(|(_, hosts)| hosts.len() > 1)
        .map(|(id, _)| id.clone())
        .collect();

    let total = groups.len();
    let networks: BTreeSet<String> = groups
        .values()
        .flatten()
        .filter_map(|offer| offer.network.clone())
        .collect();
    let (limit, offset) = shelf_page(&params);
    let mut grouped: Vec<_> = groups.iter().collect();
    match params.sort.as_deref() {
        Some("name") => grouped.sort_by(|(a, _), (b, _)| {
            a.to_lowercase()
                .cmp(&b.to_lowercase())
                .then_with(|| a.cmp(b))
        }),
        Some("price-asc") => grouped.sort_by(|(a_host, a), (b_host, b)| {
            compare_optional_price(min_price(a), min_price(b), false)
                .then_with(|| a_host.cmp(b_host))
        }),
        Some("price-desc") => grouped.sort_by(|(a_host, a), (b_host, b)| {
            compare_optional_price(min_price(a), min_price(b), true)
                .then_with(|| a_host.cmp(b_host))
        }),
        _ => grouped.sort_by(|(a_host, a), (b_host, b)| {
            group_match_score(b_host, b, query)
                .cmp(&group_match_score(a_host, a, query))
                .then_with(|| b.len().cmp(&a.len()))
                .then_with(|| a_host.cmp(b_host))
        }),
    }
    let apis: Vec<Value> = if params.count_only {
        Vec::new()
    } else {
        grouped
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|(host, endpoints)| {
                let refs: Vec<&ExternalOffer> = endpoints.iter().collect();
                let sources_for_host: Vec<String> = found_in
                    .get(host)
                    .map(|s| s.iter().cloned().collect())
                    .unwrap_or_default();
                // The first index that listed it owns the `source` field, so the
                // existing per-source card shape is unchanged; `sources` is the
                // addition, and it is the honest answer for a seller in two.
                let primary = sources_for_host.first().cloned().unwrap_or_default();
                let mut card = api_summary_json(&primary, host, &refs);
                if let Some(map) = card.as_object_mut() {
                    map.insert("sources".into(), json!(sources_for_host));
                    // An index that publishes many sellers is not one of them.
                    //
                    // `ExternalOffer::provider` carries the SOURCE row's name,
                    // stamped identically onto every offer that index yielded. On
                    // a storefront — one seller publishing its own catalogue —
                    // that is exactly right and is the only name the offer has. On
                    // a facilitator it put "PayAI" on the card belonging to
                    // Market2000, and then on Predge's, and then on three hundred
                    // more, so the shelf read as two companies selling everything.
                    //
                    // Which of the two an index is, is not a flag anybody has to
                    // remember to set: it is visible in what the index just
                    // handed over. One host is a storefront; many hosts is a
                    // wholesaler. Derived per crawl from the data rather than read
                    // from a column, so a row whose `name` is set by mistake — as
                    // migration 086 set both of these — cannot mislabel the shop.
                    if aggregators.contains(&primary) {
                        map.insert("provider".into(), Value::Null);
                        map.insert("provider_logo".into(), Value::Null);
                    }
                }
                card
            })
            .collect()
    };

    json_cached(
        StatusCode::OK,
        json!({
            "query": query,
            "apis": apis,
            "meta": {
                "returned": apis.len(),
                "total": total,
                "offset": offset,
                "endpoints": groups.values().map(Vec::len).sum::<usize>(),
                "sources_read": read.len(),
                "networks": networks,
            },
            "sources": sources.iter().map(sources::source_summary_json).collect::<Vec<_>>(),
            // Which index answered, and which did not. A shelf that quietly
            // drops a dead index reads as a smaller ecosystem rather than as a
            // failed read, and the reader cannot tell the difference.
            "read": read,
            "note": "Providers across every external index the gateway sweeps, merged on the host. Unreviewed: Apiosk has not checked these endpoints. The reviewed catalogue is separate, at GET /v1/apis.",
        }), "public, max-age=60")
}

/// `GET /v1/sources/{id}/apis` — the providers one index publishes.
pub async fn list_source_apis(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<BrowseParams>,
    headers: HeaderMap,
) -> Response<Body> {
    if let Some(limited) = throttled_by_ip(&state, &headers, "sources-browse", BROWSE_RATE_LIMIT, "Slow down — each miss costs an external index a request.").await {
        return limited;
    }

    let (source, read) = match load(&state, &id, params.q.as_deref()).await {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let offers = read.offers();

    // One host is one provider. It is a crude identity and the only honest one
    // available: an index entry carries a URL, not a company. Two products
    // behind the same host will read as one provider here, which is what the
    // URL says and therefore what an agent calling it would experience.
    let mut groups: BTreeMap<String, Vec<&ExternalOffer>> = BTreeMap::new();
    for offer in offers {
        groups.entry(offer.host.clone()).or_default().push(offer);
    }

    let total = groups.len();
    let (limit, offset) = page(&params);
    let apis: Vec<Value> = groups
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|(host, endpoints)| api_summary_json(&id, &host, &endpoints))
        .collect();

    json_cached(
        StatusCode::OK,
        json!({
            "source": sources::source_summary_json(&source),
            "query": params.q,
            "apis": apis,
            "meta": {
                "returned": apis.len(),
                "total": total,
                "offset": offset,
                "endpoints": read.matched,
            },
            "coverage": coverage_json(&source, &read),
        }), "public, max-age=60")
}

/// How much of the index this actually is.
///
/// An index that hands over a page and declares a much larger catalogue is
/// telling the reader something they cannot see from the list itself, and
/// "100 providers" read off a screen that is really the first 0.4% of the
/// corpus is a worse answer than no number at all.
fn coverage_json(source: &sources::Source, read: &Browsed) -> Value {
    // A queried index is never "complete": what came back is one search result,
    // usually capped (the Bazaar stops at 20), and calling that the whole index
    // would be the same overstatement as calling PayAI's first page its
    // catalogue. Four different situations, four different sentences.
    //
    // A CRAWLED reading is judged on the crawl's own raw entry count, never on
    // the endpoints held: PayAI declares 27,925 entries and yields 3,609
    // distinct endpoints once the same resource on three networks stops being
    // counted three times, so comparing 3,609 against 27,925 would report a
    // finished crawl as 13% of an index forever.
    let complete = match &read.crawled {
        Some(crawl) => {
            crawl.complete
                && read
                    .index_reports_total
                    .map(|total| total <= crawl.entries_read as u64)
                    .unwrap_or(true)
        }
        None => {
            source.is_browsable()
                && read
                    .index_reports_total
                    .map(|total| total <= read.read as u64)
                    .unwrap_or(true)
        }
    };
    let mut coverage = json!({
        "endpoints_read": read.read,
        "endpoints_matched": read.matched,
        "index_reports_total": read.index_reports_total,
        "complete": complete,
        "note": if !source.is_browsable() {
            "These are search results for this query, not the index's catalogue — most queried indexes cap what one question returns. Ask differently to see different providers."
        } else if read.crawled.is_some() && complete {
            "The gateway's background crawler read this index to its end. What is listed here is the whole catalogue, deduplicated — the same corpus /v1/discover ranks."
        } else if read.crawled.is_some() {
            "The gateway's background crawler read as far as it got before its budget ran out, and this is that. More than one page, less than the whole index; the next crawl starts again from the top."
        } else if complete {
            "This is everything the index handed over in one document."
        } else {
            "The index paginates and this is its first page. The background crawler has not landed a full read of it since this gateway started — check back in a few minutes."
        },
    });
    // Said only when there IS a crawl to describe, rather than as nulls that
    // read like a crawl that failed.
    if let Some(crawl) = &read.crawled {
        coverage["crawl"] = json!({
            "entries_read": crawl.entries_read,
            "pages": crawl.pages,
            "complete": crawl.complete,
            "refreshed_unix": crawl.refreshed_unix,
        });
    }
    coverage
}

/// `GET /v1/sources/{id}/apis/{host}` — every endpoint of one provider.
pub async fn get_source_api(
    State(state): State<Arc<AppState>>,
    Path((id, host)): Path<(String, String)>,
    Query(params): Query<BrowseParams>,
    headers: HeaderMap,
) -> Response<Body> {
    if let Some(limited) = throttled_by_ip(&state, &headers, "sources-browse", BROWSE_RATE_LIMIT, "Slow down — each miss costs an external index a request.").await {
        return limited;
    }

    let (source, read) = match load(&state, &id, params.q.as_deref()).await {
        Ok(pair) => pair,
        Err(response) => return response,
    };

    let coverage = coverage_json(&source, &read);
    let wanted = host.trim().to_ascii_lowercase();
    let mine: Vec<ExternalOffer> = offers_of(read.offers(), &wanted);

    if mine.is_empty() {
        return error(
            StatusCode::NOT_FOUND,
            "unknown_api",
            &format!(
                "{} publishes nothing for {}. List what it does publish at GET /v1/sources/{}/apis.",
                id, wanted, id
            ),
        );
    }

    // Settled together, in one call, because that is where the price comes
    // from: the platform ceiling and the host policy are facts about the
    // gateway, not about the listing, and computing them per offer separately
    // is how two screens start quoting two numbers.
    let endpoints = sources::settled_json(&state, &mine).await;
    let refs: Vec<&ExternalOffer> = mine.iter().collect();

    json_cached(
        StatusCode::OK,
        json!({
            "source": sources::source_summary_json(&source),
            "api": api_summary_json(&id, &wanted, &refs),
            "endpoints": endpoints,
            "meta": { "endpoints": mine.len() },
            "coverage": coverage,
            "unreviewed": "Everything here is the index's own claim. Apiosk has not called these endpoints, has not measured them, and does not vouch for the input or output description. Each endpoint says whether the gateway can settle it for you.",
        }), "public, max-age=60")
}

/// Resolve the source and read it, or the response explaining why not.
async fn load(
    state: &Arc<AppState>,
    id: &str,
    query: Option<&str>,
) -> Result<(sources::Source, Browsed), Response<Body>> {
    let Some(source) = sources::source_by_id(state, id).await else {
        return Err(error(
            StatusCode::NOT_FOUND,
            "unknown_source",
            "No index by that id is being swept. List them at GET /v1/sources.",
        ));
    };

    match sources::browse(state, &source, query).await {
        Ok(read) => Ok((source, read)),
        Err(BrowseError::QueryRequired) => Err(json(
            StatusCode::BAD_REQUEST,
            json!({
                "ok": false,
                "error": "query_required",
                "message": format!(
                    "{id} only answers questions — it publishes no whole-catalogue document. Add ?q=<what you need>."
                ),
                "source": sources::source_summary_json(&source),
            }),
        )),
        Err(BrowseError::SourceUnavailable) => Err(json(
            StatusCode::BAD_GATEWAY,
            json!({
                "ok": false,
                "error": "source_unavailable",
                "message": format!("{id} did not answer, or answered with something unreadable. This is the index's outage, not the catalogue's — everything else still works."),
                "source": sources::source_summary_json(&source),
            }),
        )),
    }
}

/// One provider as a card: enough to decide whether to open it, and nothing
/// that pretends to be more certain than a host name and a price range.
fn api_summary_json(source_id: &str, host: &str, endpoints: &[&ExternalOffer]) -> Value {
    let mut prices: Vec<f64> = endpoints.iter().filter_map(|o| o.price_usd).collect();
    prices.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let mut networks: Vec<String> = endpoints.iter().filter_map(|o| o.network.clone()).collect();
    networks.sort();
    networks.dedup();

    let mut methods: Vec<String> = endpoints.iter().filter_map(|o| o.method.clone()).collect();
    methods.sort();
    methods.dedup();

    // The longest description among the endpoints, as the provider's own words.
    // Not a summary of the provider — no such sentence exists in an index — so
    // it is named for what it is.
    let sample = endpoints
        .iter()
        .map(|o| o.description.as_str())
        .max_by_key(|d| d.len())
        .unwrap_or("");

    // Who is selling, when the index said so. Every offer from one host of one
    // source carries the same stamped identity (src/sources.rs::parse_in), so
    // the first one that has it speaks for the group. Absent on an aggregator,
    // where the host is the only identity there is.
    let provider = endpoints.iter().find_map(|o| o.provider.as_deref());
    let provider_logo = endpoints.iter().find_map(|o| o.provider_logo.as_deref());

    json!({
        "host": host,
        "source": source_id,
        "endpoint_count": endpoints.len(),
        "sample_description": sample,
        "provider": provider,
        "provider_logo": provider_logo,
        "price_usd_min": prices.first(),
        "price_usd_max": prices.last(),
        "unpriced_endpoints": endpoints.len() - prices.len(),
        "networks": networks,
        "methods": methods,
        // How many endpoints publish a schema an agent could call blind. The
        // single most useful number on the card: it separates a provider you
        // can integrate from one you would have to guess at.
        "with_input_schema": endpoints.iter().filter(|o| o.input_schema.is_some()).count(),
        "endpoints_url": format!("/v1/sources/{source_id}/apis/{host}"),
        "verified": false,
    })
}

/// One seller's offers, addressed by host OR by registrable domain.
///
/// A seller is a registrable domain everywhere else in the system — it is what
/// the crawl groups on and what `source_providers.provider_key` stores — while
/// this path is addressed by host. `freedomlab.space` publishes its eight
/// endpoints from `loki.freedomlab.space`, so the admin row's own key is not a
/// host any entry carries, and asking for it used to 404 against a crawl that
/// held every one of those endpoints.
///
/// Exact host first, because two products behind two hosts of one domain are
/// two pages and collapsing them would be a different answer than the list
/// gave. The domain is the fallback, and it is only reached when no host
/// matched at all.
fn offers_of(offers: &[ExternalOffer], wanted: &str) -> Vec<ExternalOffer> {
    let exact: Vec<ExternalOffer> = offers
        .iter()
        .filter(|o| o.host.eq_ignore_ascii_case(wanted))
        .cloned()
        .collect();
    if !exact.is_empty() {
        return exact;
    }
    offers
        .iter()
        .filter(|o| crate::source_index::registrable_domain(&o.host) == wanted)
        .cloned()
        .collect()
}

/// The shelf's paging. See [`SHELF_MAX_LIMIT`].
fn shelf_page(params: &BrowseParams) -> (usize, usize) {
    (
        params
            .limit
            .unwrap_or(SHELF_MAX_LIMIT)
            .clamp(1, SHELF_MAX_LIMIT),
        params.offset.unwrap_or(0),
    )
}

fn group_search_text(host: &str, offers: &[ExternalOffer]) -> String {
    let mut text = host.to_lowercase();
    for offer in offers {
        text.push(' ');
        text.push_str(&offer.description.to_lowercase());
        if let Some(provider) = offer.provider.as_deref() {
            text.push(' ');
            text.push_str(&provider.to_lowercase());
        }
    }
    text
}

fn group_match_score(host: &str, offers: &[ExternalOffer], query: Option<&str>) -> usize {
    let Some(query) = query else {
        return 0;
    };
    let name = host.to_lowercase();
    query
        .split_whitespace()
        .map(str::to_lowercase)
        .map(|term| {
            if name == term {
                100
            } else if name.starts_with(&term) {
                40
            } else if name.contains(&term) {
                20
            } else if offers.iter().any(|offer| {
                offer.description.to_lowercase().contains(&term)
                    || offer
                        .provider
                        .as_deref()
                        .is_some_and(|provider| provider.to_lowercase().contains(&term))
            }) {
                5
            } else {
                0
            }
        })
        .sum()
}

fn min_price(offers: &[ExternalOffer]) -> Option<f64> {
    offers
        .iter()
        .filter_map(|offer| offer.price_usd)
        .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
}

fn compare_optional_price(a: Option<f64>, b: Option<f64>, descending: bool) -> std::cmp::Ordering {
    match (a, b) {
        (Some(a), Some(b)) if descending => b.partial_cmp(&a).unwrap_or(std::cmp::Ordering::Equal),
        (Some(a), Some(b)) => a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn page(params: &BrowseParams) -> (usize, usize) {
    (
        params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT),
        params.offset.unwrap_or(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bazaar::ExternalOffer;

    fn offer(host: &str, resource: &str, price: Option<f64>, schema: bool) -> ExternalOffer {
        ExternalOffer {
            resource: resource.to_string(),
            host: host.to_string(),
            description: "a short one".to_string(),
            price_usd: price,
            network: Some("base".to_string()),
            method: Some("GET".to_string()),
            input_schema: schema.then(|| json!({"type": "object"})),
            source: "payai".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn an_api_card_spans_every_endpoint_of_its_host() {
        let a = offer(
            "api.example.test",
            "https://api.example.test/a",
            Some(0.01),
            true,
        );
        let b = offer(
            "api.example.test",
            "https://api.example.test/b",
            Some(0.003),
            false,
        );
        let c = ExternalOffer {
            description: "the longest description of the three, by some margin".to_string(),
            ..offer(
                "api.example.test",
                "https://api.example.test/c",
                None,
                false,
            )
        };
        let card = api_summary_json("payai", "api.example.test", &[&a, &b, &c]);

        assert_eq!(card["endpoint_count"], 3);
        // Range across the priced ones, with the unpriced counted rather than
        // silently dropped — a card that hid them would overstate coverage.
        assert_eq!(card["price_usd_min"], json!(0.003));
        assert_eq!(card["price_usd_max"], json!(0.01));
        assert_eq!(card["unpriced_endpoints"], 1);
        assert_eq!(card["with_input_schema"], 1);
        assert_eq!(card["sample_description"], json!(c.description));
        assert_eq!(
            card["endpoints_url"],
            "/v1/sources/payai/apis/api.example.test"
        );
    }

    #[test]
    fn an_api_card_never_claims_review() {
        // Everything reachable through these endpoints is an index's own claim.
        // The flag is asserted rather than assumed because a card that reads as
        // reviewed is the one mistake this surface must not make.
        let a = offer(
            "api.example.test",
            "https://api.example.test/a",
            Some(0.01),
            true,
        );
        assert_eq!(
            api_summary_json("payai", "api.example.test", &[&a])["verified"],
            json!(false)
        );
    }

    #[test]
    fn a_provider_card_carries_the_identity_the_index_published() {
        // A source that IS one seller stamps its name and logo onto every offer
        // it yields; the card is where a reader sees them instead of a
        // hostname. Taken from whichever endpoint has them — they are the
        // same on all of one host's, by construction.
        let plain = offer(
            "store.example.test",
            "https://store.example.test/a",
            Some(0.01),
            false,
        );
        let named = ExternalOffer {
            provider: Some("Agent Exchange".to_string()),
            provider_logo: Some("https://store.example.test/favicon.ico".to_string()),
            ..offer(
                "store.example.test",
                "https://store.example.test/b",
                Some(0.02),
                false,
            )
        };
        let card = api_summary_json("agent-exchange", "store.example.test", &[&plain, &named]);
        assert_eq!(card["provider"], json!("Agent Exchange"));
        assert_eq!(
            card["provider_logo"],
            json!("https://store.example.test/favicon.ico")
        );

        // An aggregator stamps nothing, and the card says so rather than
        // guessing a company out of a hostname.
        let anon = api_summary_json("payai", "api.example.test", &[&plain]);
        assert_eq!(anon["provider"], Value::Null);
        assert_eq!(anon["provider_logo"], Value::Null);
    }

    #[test]
    fn the_source_list_says_who_is_selling_when_anybody_is() {
        // Null on an aggregator: `name` and `logo_url` say who is SELLING, and
        // an index that publishes many sellers is none of them. The buyer app
        // falls back to the id and to the source's own site for a mark.
        let summary = sources::source_summary_json(&source(true));
        assert_eq!(summary["name"], Value::Null);
        assert_eq!(summary["logo_url"], Value::Null);
        assert_eq!(summary["tags"], json!([]));

        let mut storefront = source(true);
        storefront.provider = Some("Agent Exchange".to_string());
        storefront.logo_url = Some("https://store.example.test/favicon.ico".to_string());
        storefront.category = Some("Data".to_string());
        storefront.tags = vec!["data".to_string()];
        let summary = sources::source_summary_json(&storefront);
        assert_eq!(summary["name"], json!("Agent Exchange"));
        assert_eq!(
            summary["logo_url"],
            json!("https://store.example.test/favicon.ico")
        );
        assert_eq!(summary["category"], json!("Data"));
        assert_eq!(summary["tags"], json!(["data"]));
    }

    #[test]
    fn a_card_with_no_prices_at_all_states_no_range() {
        let a = offer(
            "api.example.test",
            "https://api.example.test/a",
            None,
            false,
        );
        let card = api_summary_json("payai", "api.example.test", &[&a]);
        // Null, not zero. A free endpoint and an endpoint whose price nobody
        // published are different facts, and $0.00 asserts the wrong one.
        assert_eq!(card["price_usd_min"], Value::Null);
        assert_eq!(card["price_usd_max"], Value::Null);
    }

    fn source(browsable: bool) -> sources::Source {
        sources::Source::plain(
            "payai".to_string(),
            if browsable {
                sources::Fetch::Indexed {
                    url: "https://i.test/all".to_string(),
                }
            } else {
                sources::Fetch::Queried {
                    template: "https://i.test/s?q={query}".to_string(),
                }
            },
            sources::Shape::X402Items,
            None,
        )
    }

    fn browsed(read: usize, total: Option<u64>) -> Browsed {
        Browsed::of(read, total, None)
    }

    /// The same reading, but held by the background crawler: `read` endpoints
    /// distilled from `entries` raw index entries.
    fn crawled(read: usize, entries: usize, total: Option<u64>, complete: bool) -> Browsed {
        Browsed::of(
            read,
            total,
            Some(sources::Crawled {
                entries_read: entries,
                pages: 28,
                complete,
                refreshed_unix: 1_756_000_000,
            }),
        )
    }

    #[test]
    fn a_paginated_index_is_never_reported_as_complete() {
        // PayAI's real numbers. Reporting 99 as the catalogue would be wrong by
        // more than two orders of magnitude.
        let c = coverage_json(&source(true), &browsed(99, Some(26_546)));
        assert_eq!(c["complete"], json!(false));
        assert_eq!(c["index_reports_total"], json!(26_546));
        assert!(c["note"].as_str().unwrap().contains("paginates"));
    }

    #[test]
    fn a_manifest_that_declares_nothing_is_complete() {
        let c = coverage_json(&source(true), &browsed(18, None));
        assert_eq!(c["complete"], json!(true));
    }

    #[test]
    fn a_queried_index_is_never_complete_however_much_it_returned() {
        // A search result is not a catalogue, and the Bazaar caps at 20 — so a
        // full page from a queried source is the LEAST complete answer of all,
        // not the most.
        let c = coverage_json(&source(false), &browsed(20, None));
        assert_eq!(c["complete"], json!(false));
        assert!(c["note"].as_str().unwrap().contains("search results"));
    }

    #[test]
    fn a_finished_crawl_is_complete_on_entries_not_on_endpoints() {
        // PayAI's real numbers: 27,925 entries declared and read, distilled to
        // 3,609 distinct endpoints once one resource on three networks stops
        // counting three times. Judging completeness on the endpoint count
        // would report a finished crawl as 13% of the index forever.
        let c = coverage_json(&source(true), &crawled(3_609, 27_925, Some(27_925), true));
        assert_eq!(c["complete"], json!(true));
        assert_eq!(c["crawl"]["entries_read"], json!(27_925));
        assert!(c["note"].as_str().unwrap().contains("to its end"));
    }

    #[test]
    fn a_crawl_that_ran_out_of_budget_says_so_rather_than_claiming_the_index() {
        let c = coverage_json(&source(true), &crawled(1_200, 9_000, Some(27_925), false));
        assert_eq!(c["complete"], json!(false));
        assert!(c["note"].as_str().unwrap().contains("as far as it got"));
    }

    #[test]
    fn no_crawl_yet_is_reported_as_the_first_page_it_is() {
        let c = coverage_json(&source(true), &browsed(100, Some(27_925)));
        assert_eq!(c["complete"], json!(false));
        assert!(c.get("crawl").is_none());
        assert!(c["note"].as_str().unwrap().contains("first page"));
    }

    #[test]
    fn a_seller_resolves_by_its_own_key_when_no_host_carries_it() {
        // What the admin row holds is `freedomlab.space`; what the entries
        // carry is `loki.freedomlab.space`. Eight endpoints, and the page for
        // them used to answer "publishes nothing".
        let offers = vec![
            offer(
                "loki.freedomlab.space",
                "https://loki.freedomlab.space/a",
                Some(0.01),
                false,
            ),
            offer(
                "api.freedomlab.space",
                "https://api.freedomlab.space/b",
                Some(0.02),
                false,
            ),
            offer("elsewhere.test", "https://elsewhere.test/c", None, false),
        ];
        assert_eq!(offers_of(&offers, "freedomlab.space").len(), 2);
    }

    #[test]
    fn an_exact_host_stays_its_own_page() {
        // The domain is a FALLBACK. Two products behind two hosts of one
        // domain are two pages, and the list that linked here said so.
        let offers = vec![
            offer(
                "loki.freedomlab.space",
                "https://loki.freedomlab.space/a",
                Some(0.01),
                false,
            ),
            offer(
                "api.freedomlab.space",
                "https://api.freedomlab.space/b",
                Some(0.02),
                false,
            ),
        ];
        let mine = offers_of(&offers, "loki.freedomlab.space");
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].host, "loki.freedomlab.space");
    }

    #[test]
    fn a_seller_nobody_published_is_still_nothing() {
        let offers = vec![offer(
            "elsewhere.test",
            "https://elsewhere.test/c",
            None,
            false,
        )];
        assert!(offers_of(&offers, "freedomlab.space").is_empty());
    }

    #[test]
    fn paging_is_clamped_rather_than_trusted() {
        assert_eq!(
            page(&BrowseParams {
                limit: Some(9_999),
                ..Default::default()
            })
            .0,
            MAX_LIMIT
        );
        assert_eq!(
            page(&BrowseParams {
                limit: Some(0),
                ..Default::default()
            })
            .0,
            1
        );
        assert_eq!(page(&BrowseParams::default()), (DEFAULT_LIMIT, 0));
    }

    #[test]
    fn shelf_search_matches_every_word_across_name_and_description() {
        let mut company = offer(
            "registry.example",
            "https://registry.example/company",
            Some(0.02),
            false,
        );
        company.description = "Company registry lookup".to_string();
        let offers = vec![company];
        let text = group_search_text("registry.example", &offers);
        assert!(text.contains("registry.example"));
        assert!(text.contains("company"));
        assert!(["registry", "company"]
            .iter()
            .all(|term| text.contains(term)));
    }

    #[test]
    fn unpriced_sellers_sort_last_in_both_price_directions() {
        assert_eq!(
            compare_optional_price(Some(0.02), None, false),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            compare_optional_price(Some(0.02), None, true),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn count_only_is_off_unless_the_caller_requests_it() {
        assert!(!BrowseParams::default().count_only);
    }
}
