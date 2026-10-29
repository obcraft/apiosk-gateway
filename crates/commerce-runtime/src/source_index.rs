//! Reading a paginated index to the end, off the critical path.
//!
//! ---------------------------------------------------------------------------
//! The hole this fills
//! ---------------------------------------------------------------------------
//!
//! [`crate::sources`] calls a [`Fetch::Indexed`] source "one document
//! containing the whole catalogue". That is true of the two manifests and
//! false of every facilitator, and the difference was two orders of magnitude.
//! Measured 2026-08-26:
//!
//!   PayAI            first  100 of 26,684    (the row asked for no limit)
//!   Coinbase Bazaar  first 1000 of 15,167
//!   x402.direct      first  100 of  4,019
//!   agentic.market   first  200 of  2,381
//!   thirdweb         first  200 of    708
//!
//! ~1,600 of ~49,000. The sweep searched three percent of the ecosystem and
//! reported it as a sweep of the ecosystem.
//!
//! The inconsistency was not a dialect problem: all five speak `limit` plus a
//! cursor and all five declare their total. It was that the page size was
//! HAND-WRITTEN INTO EACH ROW'S URL — 1000 for the Bazaar, 200 for thirdweb,
//! 100 for x402.direct, nothing at all for PayAI — by whoever added the row,
//! against whatever that index happened to allow that day. Five hand-picked
//! numbers is five things to keep in step with five upstreams, so they drifted
//! apart immediately and nothing noticed.
//!
//! Nothing here is hand-written per source. The crawler asks for a large page,
//! reads back what the index actually granted, follows the cursor the index
//! actually echoed, and stops when the index says it is done. A source that
//! raises its page limit is followed the next refresh; a source that lowers it
//! is followed too, without a migration.
//!
//! ---------------------------------------------------------------------------
//! Why a background task and not a deeper fetch on the request
//! ---------------------------------------------------------------------------
//!
//! The sweep has a 3.5-second budget for every source together
//! ([`crate::sources::SWEEP_BUDGET`]), and PayAI alone is 27 pages. Paging on
//! the request path would either blow the budget or, worse, be truncated by it
//! at a different depth on every request — a search whose reach depends on how
//! busy the box was.
//!
//! So the crawl happens on a timer, and the sweep reads the finished result
//! out of memory: no network at all for an indexed source, the whole catalogue
//! in scope, and a filter that is a few hash lookups rather than 49,000
//! tokenisations (see [`Catalogue::matching`]).
//!
//! ---------------------------------------------------------------------------
//! Source -> provider -> endpoint
//! ---------------------------------------------------------------------------
//!
//! An index is a flat list of endpoints, and searching it as one was costing
//! the answer twice over.
//!
//! It cost RECALL, because the unit a job is really about is the seller. Asked
//! for "NVIDIA's latest earnings and analyst estimates", a provider whose
//! `/estimates` endpoint matches one of those words and whose `/earnings`
//! endpoint matches another answers two thirds of the job — while every one of
//! its endpoints looks like a one-word match and sorts below a provider that
//! happens to say "estimates" twice on one line. Nothing in a flat search can
//! see the difference, because the fact lives above the endpoint.
//!
//! And it cost BREADTH, because a cut at [`MAX_MATCHES_PER_SOURCE`] taken off
//! the top of one flat list goes to whichever seller publishes the most
//! endpoints about the topic. The sweep then merges and truncates again, so a
//! search of 49,000 rows across seven indexes arrived as one seller's
//! catalogue with a search box in front of it.
//!
//! So the crawl now groups what it reads:
//!
//! ```text
//! SOURCE     Coinbase Bazaar / PayAI / x402.direct / a manifest
//!   |
//! PROVIDER   the registrable domain the endpoints are served from
//!   |
//! ENDPOINT   the resource itself
//! ```
//!
//! and [`Catalogue::matching`] asks the provider question before the endpoint
//! question: score the sellers, deal the allowance out across them, then take
//! each one's best endpoints. There are one to two orders of magnitude fewer
//! sellers than endpoints, which is what makes the extra layer free.
//!
//! The grouping key is the domain, not `ExternalOffer::provider` — that field
//! carries the SOURCE row's name and is identical on every entry an index
//! published, so it cannot group anything (see [`registrable_domain`]).
//!
//! What stays outside this module is unchanged and deliberate: the index is a
//! routing hint, never a source of truth. Nothing here is quoted, priced or
//! settled from. `/v1/x402/fetch` reads the live 402 for the exact request at
//! purchase time, which is why a stale price or a since-deleted endpoint in
//! the crawl costs a wasted candidate slot and never a wrong charge.
//!
//! ---------------------------------------------------------------------------
//! What the index deliberately does NOT keep
//! ---------------------------------------------------------------------------
//!
//! The provider-declared schemas — `input`, `input_schema`, `output`, which
//! come off `extensions.bazaar`. They are 60% of a Bazaar entry's bytes (2,177
//! of 3,604 measured over 200 items) and they become several times that again
//! once parsed into a [`serde_json::Value`] tree. Keeping them for 49,000
//! entries is a few hundred megabytes on a box that has 256, and this process
//! has already been OOM-killed once for holding a built discovery document
//! (see [`crate::cache::EndpointCache::get_discovery_items`]).
//!
//! Dropping them costs nothing that discovery needs, because they are not what
//! FINDS an offer — the resource, description, provider, category and tags are
//! — and they are not what SETTLES one either: `/v1/x402/fetch` quotes off the
//! price, network and host and never reads a declared schema. The queried lane
//! still carries them for its precision hits, and [`crate::sources`]'s merge
//! now prefers the schema-bearing copy of a duplicate, so an offer both lanes
//! find keeps its schema.
//!
//! ---------------------------------------------------------------------------
//! Why the lookup is a trigram index and not a postings map
//! ---------------------------------------------------------------------------
//!
//! The predicate discovery needs is a SUBSTRING match — `curren` has to find
//! `currency`, because that is what [`crate::sources::relevant`] does and the
//! two must not answer differently. A plain postings map cannot answer that
//! without scanning every key, and that scan WAS the search: ~80 wanted tokens
//! against a ~60,000-token dictionary is five million substring tests, per
//! source, per request, on the agent's critical path. [`TermIndex`] indexes
//! the dictionary's own three-byte windows, so a wanted term is narrowed to a
//! handful of candidate tokens first, and a term no index has ever heard of is
//! answered by a single failed hash lookup.
//!
//! Env:
//!   SOURCE_INDEX_ENABLED           default true; false leaves the old
//!                                  first-page behaviour exactly as it was
//!   SOURCE_INDEX_REFRESH_SECS      default 900
//!   SOURCE_INDEX_MAX_PER_SOURCE    default 40000
//!   SOURCE_INDEX_MAX_TOTAL         default 120000

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde_json::{json, Value};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::{
    bazaar::{quality_gate, ExternalOffer, MAX_PER_HOST, MIN_LISTING_SCORE},
    sources::{declared_total, tokens, Fetch, Source},
    AppState,
};

/// How long one page may take. Generous next to
/// [`crate::sources::SOURCE_TIMEOUT`] because nobody is waiting: a slow index
/// crawled slowly is still a whole index, and giving up at 2.5 seconds is how
/// you end up with three percent of it.
const PAGE_TIMEOUT: Duration = Duration::from_secs(20);

/// Whole-crawl budget for ONE source. A stop, not a schedule — a source that
/// cannot be read to the end in four minutes is kept as far as it got and
/// reported incomplete.
const SOURCE_BUDGET: Duration = Duration::from_secs(240);

/// Pause between pages of the same index. These are free public endpoints and
/// we are about to ask one of them for twenty-seven pages; doing that flat out
/// is how a free endpoint stops being free.
const PAGE_PAUSE: Duration = Duration::from_millis(120);

/// Hard ceiling on pages per source, whatever the index declares. A backstop
/// against a `total` that never gets closer, not a coverage decision: at the
/// largest page size any of these grant it is 120,000 entries.
const MAX_PAGES: usize = 120;

/// Consecutive pages the index has already handed over, before the crawl gives
/// up.
///
/// The page REPEATING, not the page yielding nothing — the difference is a
/// whole index. This counted surviving offers until 2026-09-01, when PayAI
/// stopped at page 11 holding 11,000 of a declared 27,926: it publishes
/// ~17,000 consecutive listings carrying no description, the parser drops
/// every one of them, and three such pages read as "there is nothing left"
/// while the cursor had been moving the whole time. A page that parses to
/// nothing is a page about entries not worth having, and says nothing about
/// whether there are more. See [`page_fingerprint`].
///
/// More than one, because "this page was all duplicates" does not mean "there
/// is nothing left". x402.direct sorts by a score that moves between requests,
/// so paging it returns overlapping windows: measured 2026-08-26, stopping at
/// the FIRST barren page ended the crawl at page 29 holding 2,612 of a
/// declared 4,019, while the pages after it were still carrying new entries.
/// Three in a row is the difference between 65% of that index and all of it,
/// and the page ceiling above is what stops this being unbounded.
const MAX_BARREN_PAGES: usize = 3;

/// What we ASK for. Every index clamps this to its own maximum and echoes what
/// it granted; nothing here assumes the ask was honoured.
const PAGE_ASK: u64 = 1_000;

/// Offers one source may contribute to the index.
const DEFAULT_MAX_PER_SOURCE: usize = 40_000;

/// Offers all sources together may contribute.
const DEFAULT_MAX_TOTAL: usize = 120_000;

/// How often the whole set is re-crawled.
const DEFAULT_REFRESH_SECS: u64 = 900;

/// Matches one source may hand a single search.
///
/// The sweep ranks and truncates to at most [`crate::sources::MAX_LIMIT`], so
/// this exists to bound the CLONE, not the answer. A broad word like "data"
/// matches thousands of entries in a 26,000-entry index and cloning all of
/// them per source per request is the one way a local filter can still be
/// expensive. What gets dropped is decided by [`Catalogue::matching`], which
/// deals the allowance out across providers rather than off the top of one
/// flat list.
const MAX_MATCHES_PER_SOURCE: usize = 400;

/// Endpoints one provider contributes before the next provider gets a turn.
///
/// Four rather than one, because a seller that genuinely answers the question
/// usually answers it with a small family of endpoints — a quote, a history, a
/// profile — and splitting that family across four rounds only to reassemble
/// it in the ranking buys nothing. Four rather than forty, because
/// [`MAX_MATCHES_PER_SOURCE`] divided by four still reaches a hundred distinct
/// sellers.
const PER_PROVIDER_RUN: usize = 4;

/// Sellers whose neighbouring endpoints are pulled in behind their hits.
///
/// A seller the question clearly describes usually serves the answer from
/// more than one endpoint, and the one that actually answers is often the
/// tersest: `/estimates` carries three words while `/company/profile` carries
/// thirty, so keyword retrieval ranks the wrong sibling and drops the right
/// one. Once the PROVIDER is established as a candidate, its remaining
/// endpoints are worth carrying — this is the "fetch the provider's manifest
/// and filter it" step of the pipeline, answered out of the crawl instead of
/// over the network.
///
/// They score nothing in the sweep's ranking, so they sort behind every
/// keyword match and can only ever surface in an answer that would otherwise
/// be short. That is the whole reason this is safe to do at all.
const MAX_TOPPED_UP_PROVIDERS: usize = 8;

/// Distinct wanted terms a seller has to answer before its remaining
/// endpoints are worth carrying.
///
/// Two, not one. One term in common is how nearly every seller in a large
/// index relates to nearly every question — `data`, `price`, `search` — and
/// pulling a shelf in on that much evidence is not a widening, it is noise
/// with a provider's name on it.
const MIN_TOPUP_SCORE: u32 = 2;

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

fn env_usize(name: &str, default: usize) -> usize {
    env_u64(name, default as u64) as usize
}

fn enabled() -> bool {
    !matches!(
        std::env::var("SOURCE_INDEX_ENABLED")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "0" | "false" | "no" | "off"
    )
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Where a crawl stopped, and whether that was the end
// ---------------------------------------------------------------------------

/// Why the crawler stopped asking for pages.
///
/// Published per source, because "we have all of it" and "we have as much of
/// it as fitted" are different claims and a discovery engine that cannot tell
/// them apart is back where it started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// The index's own declared total was reached, or it ran out of entries.
    /// This is the only value that means the catalogue is whole.
    Exhausted,
    /// The document carried no cursor at all — a manifest, which IS whole.
    SinglePage,
    /// A page came back with entries we already had. Defensive: an index that
    /// ignores the cursor would otherwise be paged forever.
    NoNewEntries,
    /// [`MAX_PAGES`] or [`SOURCE_BUDGET`].
    Budget,
    /// A per-source or global entry cap.
    Capped,
    /// A page failed. Whatever arrived before it is kept.
    ReadFailed,
}

impl Stop {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exhausted => "exhausted",
            Self::SinglePage => "single_page",
            Self::NoNewEntries => "no_new_entries",
            Self::Budget => "budget",
            Self::Capped => "capped",
            Self::ReadFailed => "read_failed",
        }
    }

    /// Did the crawl reach the end of what the index publishes?
    pub fn complete(self) -> bool {
        matches!(self, Self::Exhausted | Self::SinglePage)
    }
}

// ---------------------------------------------------------------------------
// A dictionary that answers a substring lookup without being scanned
// ---------------------------------------------------------------------------

/// An inverted index over a set of documents, whose lookup is the SUBSTRING
/// match [`crate::sources::relevant`] performs rather than an exact one.
///
/// The substring part is what used to make this expensive. A postings map
/// answers "which documents hold exactly this token" in one hash lookup, but
/// the predicate discovery needs is "which documents hold a token CONTAINING
/// this one" — `curren` has to find `currency` — and the only way to answer
/// that from a plain map is to scan every key. That scan was the search:
/// ~80 wanted tokens against a ~60,000-token dictionary is five million
/// substring tests, per source, per request, on the agent's critical path.
///
/// So the dictionary carries a trigram index of its own. Every token a
/// document contributes is broken into its three-byte windows, and a wanted
/// term can only be a substring of a token that holds EVERY one of the wanted
/// term's own windows. Looking up the rarest of them yields a handful of
/// candidate tokens instead of the whole dictionary, and the `contains` test
/// then runs on those alone. A term whose rarest window is unknown to the
/// dictionary is answered without a single test.
///
/// Tokens are pure ASCII alphanumerics (see [`crate::sources::tokens`]), so
/// byte windows are character windows and the index cannot split a multi-byte
/// character.
pub(crate) struct TermIndex {
    /// The dictionary, one entry per distinct token.
    terms: Vec<String>,
    /// `terms[i]` → the documents whose text contains it, ascending.
    postings: Vec<Vec<u32>>,
    /// Three-byte window → the tokens that hold it.
    trigrams: HashMap<[u8; 3], Vec<u32>>,
    /// How many documents this index covers, including any that contributed
    /// no tokens at all.
    docs: usize,
}

impl TermIndex {
    /// One document per text, in the order given; a document's id is its
    /// position.
    fn build<I: IntoIterator<Item = String>>(texts: I) -> Self {
        let mut at: HashMap<String, u32> = HashMap::new();
        let mut terms: Vec<String> = Vec::new();
        let mut postings: Vec<Vec<u32>> = Vec::new();
        let mut docs = 0usize;

        for (doc, text) in texts.into_iter().enumerate() {
            docs = doc + 1;
            for token in tokens(&text) {
                let id = match at.get(&token) {
                    Some(id) => *id,
                    None => {
                        let id = terms.len() as u32;
                        at.insert(token.clone(), id);
                        terms.push(token);
                        postings.push(Vec::new());
                        id
                    }
                };
                postings[id as usize].push(doc as u32);
            }
        }

        let mut trigrams: HashMap<[u8; 3], Vec<u32>> = HashMap::new();
        for (id, term) in terms.iter().enumerate() {
            let bytes = term.as_bytes();
            if bytes.len() < 3 {
                continue;
            }
            let mut seen: HashSet<[u8; 3]> = HashSet::new();
            for window in bytes.windows(3) {
                let key = [window[0], window[1], window[2]];
                if seen.insert(key) {
                    trigrams.entry(key).or_default().push(id as u32);
                }
            }
        }

        terms.shrink_to_fit();
        postings.shrink_to_fit();
        trigrams.shrink_to_fit();
        Self {
            terms,
            postings,
            trigrams,
            docs,
        }
    }

    /// Every dictionary token that CONTAINS `want`.
    ///
    /// The candidate set comes from the rarest of `want`'s own trigrams, which
    /// is a superset of the answer for the same reason the trigram index works
    /// at all; `contains` then decides. A wanted term shorter than a trigram
    /// cannot be narrowed this way and falls back to the scan — tokens are
    /// three characters or longer, so this is unreachable from a real
    /// vocabulary and exists so a hand-built one cannot silently match
    /// nothing.
    fn expand(&self, want: &str) -> Vec<u32> {
        let bytes = want.as_bytes();
        if bytes.len() < 3 {
            return (0..self.terms.len() as u32)
                .filter(|id| self.terms[*id as usize].contains(want))
                .collect();
        }
        let mut rarest: Option<&Vec<u32>> = None;
        for window in bytes.windows(3) {
            // Unknown to every token in the dictionary, so nothing contains
            // the whole term either. This is the common case for a wanted word
            // no index has ever heard of, and it costs one hash lookup.
            let Some(candidates) = self.trigrams.get(&[window[0], window[1], window[2]]) else {
                return Vec::new();
            };
            if rarest.is_none_or(|held| candidates.len() < held.len()) {
                rarest = Some(candidates);
            }
        }
        rarest
            .map(|candidates| {
                candidates
                    .iter()
                    .copied()
                    .filter(|id| self.terms[*id as usize].contains(want))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Documents that matched at least one wanted term, most distinct terms
    /// first, ties broken by document order so the result is stable.
    ///
    /// Counting happens in two flat arrays rather than a hash map: a broad
    /// word like `data` hits thousands of documents in a 26,000-entry index,
    /// and the per-term deduplication that stops one document being counted
    /// twice for two spellings of the same term is a stamp comparison here
    /// instead of a fresh `HashSet` per term.
    fn hits(&self, wanted: &[String]) -> Vec<(u32, u32)> {
        if self.docs == 0 || wanted.is_empty() {
            return Vec::new();
        }
        let mut counts = vec![0u32; self.docs];
        let mut stamp = vec![u32::MAX; self.docs];

        for (nth, want) in wanted.iter().enumerate() {
            let nth = nth as u32;
            for term in self.expand(want) {
                for doc in &self.postings[term as usize] {
                    let at = *doc as usize;
                    if stamp[at] != nth {
                        stamp[at] = nth;
                        counts[at] += 1;
                    }
                }
            }
        }

        let mut out: Vec<(u32, u32)> = counts
            .into_iter()
            .enumerate()
            .filter(|(_, n)| *n > 0)
            .map(|(doc, n)| (doc as u32, n))
            .collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        out
    }
}

// ---------------------------------------------------------------------------
// Providers, the layer between a source and an endpoint
// ---------------------------------------------------------------------------

/// One seller's whole presence inside one index.
///
/// An index is a flat list of endpoints, and searching it as one is what makes
/// a search of 26,000 rows answer with twenty-five endpoints from the same
/// three sellers. It also loses every provider whose endpoints are described
/// in words the question does not use: `/v1/officers` says nothing about
/// corporate directors, while the provider that serves it — its domain, its
/// category, its tags and the rest of its catalogue — says it plainly.
///
/// So the crawl groups the endpoints by who serves them, and the search asks
/// the provider question first. There are one to two orders of magnitude fewer
/// providers than endpoints, which is what makes the extra layer free.
pub struct Provider {
    /// The registrable domain its endpoints are served from — the only seller
    /// identity a facilitator listing actually carries. `ExternalOffer::provider`
    /// is the SOURCE row's name (the Bazaar, PayAI), the same on every entry
    /// that index published, so it cannot group anything.
    pub key: String,
    /// Its endpoints, as indices into [`Catalogue::offers`], in crawl order.
    offers: Vec<u32>,
}

/// One seller's matching endpoints, and what the seller is worth.
///
/// `score` is the seller's own — how many distinct wanted terms its whole
/// catalogue answers — which is what orders the lanes. `best` is its best
/// single endpoint's, kept only to break a tie between two sellers that
/// answer the same number of terms.
struct Lane {
    provider: u32,
    score: u32,
    best: u32,
    hits: Vec<u32>,
}

/// Bounds on one `source_providers` row, so a 2,000-endpoint seller writes a
/// row and not a document. Labels (networks, methods, categories) are few by
/// nature; tags and terms are the two unions that can grow without limit.
const MAX_SNAPSHOT_LABELS: usize = 8;
const MAX_SNAPSHOT_TAGS: usize = 32;
const MAX_SNAPSHOT_TERMS: usize = 24;
const MAX_SNAPSHOT_DESCRIPTION: usize = 400;

/// One seller's row for the durable `source_providers` table (migration 090):
/// the crawl's own provider grouping, aggregated. Built by
/// [`Catalogue::provider_snapshot`], written by
/// [`crate::db::Database::upsert_source_providers`].
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProviderSnapshot {
    pub key: String,
    pub sample_host: String,
    pub name: Option<String>,
    pub logo_url: Option<String>,
    pub endpoint_count: usize,
    pub unpriced_endpoints: usize,
    pub price_usd_min: Option<f64>,
    pub price_usd_max: Option<f64>,
    pub networks: Vec<String>,
    pub methods: Vec<String>,
    pub categories: Vec<String>,
    pub tags: Vec<String>,
    pub top_terms: Vec<String>,
    pub sample_description: String,
    pub with_input_schema: usize,
}

/// One seller's row, aggregated from whatever offers are in hand.
///
/// Shared by two very different callers on purpose: the crawl snapshots a
/// WHOLE catalogue through it (`Catalogue::provider_snapshot`), and the sweep
/// aggregates a queried source's PARTIAL answer through it
/// (`snapshot_offers`), so both write the same shape of row and the merge
/// semantics live in one place — the database's, not two aggregators'.
fn aggregate_provider(key: &str, offers: &[&ExternalOffer]) -> ProviderSnapshot {
    let mut hosts: HashMap<&str, u32> = HashMap::new();
    for offer in offers {
        *hosts.entry(offer.host.as_str()).or_default() += 1;
    }
    let sample_host = hosts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(a.0)))
        .map(|(host, _)| host.to_string())
        .unwrap_or_else(|| key.to_string());

    let mut prices: Vec<f64> = offers.iter().filter_map(|o| o.price_usd).collect();
    prices.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let mut networks: Vec<String> = offers.iter().filter_map(|o| o.network.clone()).collect();
    networks.sort();
    networks.dedup();
    networks.truncate(MAX_SNAPSHOT_LABELS);

    let mut methods: Vec<String> = offers.iter().filter_map(|o| o.method.clone()).collect();
    methods.sort();
    methods.dedup();
    methods.truncate(MAX_SNAPSHOT_LABELS);

    let mut categories: Vec<String> = offers.iter().filter_map(|o| o.category.clone()).collect();
    categories.sort();
    categories.dedup();
    categories.truncate(MAX_SNAPSHOT_LABELS);

    let mut tags: Vec<String> = offers.iter().flat_map(|o| o.tags.iter().cloned()).collect();
    tags.sort();
    tags.dedup();
    tags.truncate(MAX_SNAPSHOT_TAGS);

    // The seller's own vocabulary, ranked by how often it repeats it. Bare
    // numbers are dropped: "2026" is frequent and searches for nothing.
    let mut words: HashMap<String, u32> = HashMap::new();
    for offer in offers {
        for token in tokens(&offer_text(offer)) {
            if token.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            *words.entry(token).or_default() += 1;
        }
    }
    let mut ranked: Vec<(String, u32)> = words.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked.truncate(MAX_SNAPSHOT_TERMS);

    // The longest description, as the provider's own words — the same choice
    // `/v1/sources/{id}/apis` makes, for the same reason: no summary sentence
    // exists in an index.
    let mut sample_description = offers
        .iter()
        .map(|o| o.description.as_str())
        .max_by_key(|d| d.len())
        .unwrap_or("")
        .to_string();
    if sample_description.chars().count() > MAX_SNAPSHOT_DESCRIPTION {
        sample_description = sample_description
            .chars()
            .take(MAX_SNAPSHOT_DESCRIPTION)
            .collect();
    }

    ProviderSnapshot {
        key: key.to_string(),
        sample_host,
        name: offers.iter().find_map(|o| o.provider.clone()),
        logo_url: offers.iter().find_map(|o| o.provider_logo.clone()),
        endpoint_count: offers.len(),
        unpriced_endpoints: offers.len() - prices.len(),
        price_usd_min: prices.first().copied(),
        price_usd_max: prices.last().copied(),
        networks,
        methods,
        categories,
        tags,
        top_terms: ranked.into_iter().map(|(word, _)| word).collect(),
        sample_description,
        with_input_schema: offers.iter().filter(|o| o.input_schema.is_some()).count(),
    }
}

/// A flat list of offers — a queried source's answer to one live question —
/// grouped and aggregated into seller rows. Duplicate resources collapse
/// first, so the same endpoint arriving under two phrasings is one endpoint
/// and not an inflated count.
pub(crate) fn snapshot_offers(offers: &[ExternalOffer]) -> Vec<ProviderSnapshot> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut groups: HashMap<String, Vec<&ExternalOffer>> = HashMap::new();
    for offer in offers {
        if !seen.insert(offer.resource.as_str()) {
            continue;
        }
        groups.entry(provider_key(offer)).or_default().push(offer);
    }
    let mut out: Vec<ProviderSnapshot> = groups
        .into_iter()
        .map(|(key, offers)| aggregate_provider(&key, &offers))
        .collect();
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// Tokens one provider's profile may carry into the provider index.
///
/// A profile is the union of its endpoints' words, so a seller with 2,000
/// endpoints would otherwise own a dictionary entry for nearly every word in
/// the language and match every question ever asked. The cut is by frequency
/// across its own catalogue, so what survives is what that provider is
/// repeatedly about rather than an arbitrary slice.
const MAX_PROFILE_TOKENS: usize = 256;

/// `pro-api.coingecko.com` → `coingecko.com`.
///
/// Deliberately a heuristic and not a public-suffix list: the list is a
/// megabyte that changes monthly, and the failure it prevents here is two
/// subdomains of one seller being counted as two sellers — which costs a
/// little diversity, not a wrong answer. The multi-part suffixes below are
/// the ones that actually appear on API hosts.
pub(crate) fn registrable_domain(host: &str) -> String {
    const MULTI: &[&str] = &[
        "co.uk", "ac.uk", "gov.uk", "org.uk", "co.jp", "co.kr", "co.in", "co.nz", "co.za",
        "com.au", "net.au", "org.au", "com.br", "com.mx", "com.sg", "com.tr", "com.cn",
    ];
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    if labels.len() <= 2 {
        return host;
    }
    let last_two = format!("{}.{}", labels[labels.len() - 2], labels[labels.len() - 1]);
    if MULTI.contains(&last_two.as_str()) && labels.len() >= 3 {
        return format!("{}.{}", labels[labels.len() - 3], last_two);
    }
    last_two
}

/// Which seller an offer belongs to. Falls back to the resource itself when
/// the host is unreadable, so a malformed entry is its own provider rather
/// than joining every other malformed entry in one bucket that matches
/// everything.
fn provider_key(offer: &ExternalOffer) -> String {
    let key = registrable_domain(&offer.host);
    if key.is_empty() {
        offer.resource.to_ascii_lowercase()
    } else {
        key
    }
}

/// The endpoints grouped by seller, with one searchable profile text each.
fn group_providers(offers: &[ExternalOffer]) -> (Vec<Provider>, Vec<u32>, Vec<String>) {
    let mut at: HashMap<String, usize> = HashMap::new();
    let mut providers: Vec<Provider> = Vec::new();
    let mut provider_of: Vec<u32> = Vec::with_capacity(offers.len());
    // Per provider: how often each word appears across its own endpoints.
    let mut words: Vec<HashMap<String, u32>> = Vec::new();

    for (idx, offer) in offers.iter().enumerate() {
        let key = provider_key(offer);
        let slot = match at.get(&key) {
            Some(slot) => *slot,
            None => {
                let slot = providers.len();
                at.insert(key.clone(), slot);
                providers.push(Provider {
                    key,
                    offers: Vec::new(),
                });
                words.push(HashMap::new());
                slot
            }
        };
        providers[slot].offers.push(idx as u32);
        provider_of.push(slot as u32);
        for token in tokens(&offer_text(offer)) {
            *words[slot].entry(token).or_default() += 1;
        }
    }

    let profiles: Vec<String> = providers
        .iter()
        .zip(words)
        .map(|(provider, words)| {
            let mut ranked: Vec<(String, u32)> = words.into_iter().collect();
            ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            ranked.truncate(MAX_PROFILE_TOKENS);
            // The domain leads: it is the one word in a profile that the
            // seller chose to be known by, and `financialmodelingprep.com`
            // carries three searchable words a tag list often leaves out.
            let mut profile = provider.key.clone();
            for (word, _) in ranked {
                profile.push(' ');
                profile.push_str(&word);
            }
            profile
        })
        .collect();

    providers.shrink_to_fit();
    provider_of.shrink_to_fit();
    (providers, provider_of, profiles)
}

/// Everything about one offer a search is allowed to match on.
///
/// The same text [`crate::sources::relevant`] builds its haystack from, so the
/// index answers the question the linear filter answered and not a subtly
/// different one.
fn offer_text(offer: &ExternalOffer) -> String {
    format!(
        "{} {} {} {} {}",
        offer.resource,
        offer.description,
        offer.provider.as_deref().unwrap_or(""),
        offer.category.as_deref().unwrap_or(""),
        offer.tags.join(" "),
    )
}

// ---------------------------------------------------------------------------
// One source's catalogue, searchable
// ---------------------------------------------------------------------------

/// Everything one index published, indexed twice: once by endpoint and once by
/// the providers those endpoints belong to.
///
/// Tokenising happens ONCE here, at crawl time, so a search is a lookup rather
/// than the million string allocations [`crate::sources::relevant`] costs on a
/// whole catalogue. The provider layer on top is what makes the answer a
/// search of the ecosystem rather than a search of one flat list — see
/// [`Provider`] and [`Catalogue::matching`].
pub struct Catalogue {
    pub source_id: String,
    offers: Vec<ExternalOffer>,
    /// L3 — one document per endpoint.
    endpoints: TermIndex,
    /// L2 — one document per provider.
    providers: Vec<Provider>,
    provider_index: TermIndex,
    /// Offer index → its provider's index in `providers`.
    provider_of: Vec<u32>,
    /// What the index said its whole catalogue holds, when it said so.
    pub declared_total: Option<u64>,
    /// Raw entries read across all pages, before parsing and filtering.
    pub read: usize,
    pub pages: usize,
    pub stopped: Stop,
    pub refreshed_unix: u64,
    pub ms: u64,
}

impl Catalogue {
    /// The crawled offers, indexed. Both layers are built here so no caller
    /// can assemble a catalogue whose provider index disagrees with its
    /// endpoint index.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build(
        source_id: String,
        mut offers: Vec<ExternalOffer>,
        declared_total: Option<u64>,
        read: usize,
        pages: usize,
        stopped: Stop,
        refreshed_unix: u64,
        ms: u64,
    ) -> Self {
        // Canonical order, decided once per crawl rather than once per browse
        // request. Two things follow from it: `offers()` can be LENT to the
        // browse endpoints instead of cloned nine thousand entries at a time,
        // and a reader paging through a source sees a stable list — crawl
        // order is whatever the index happened to serve, which is not a thing
        // an `offset` can be trusted against.
        crate::sources::sort_for_browsing(&mut offers);
        let endpoints = TermIndex::build(offers.iter().map(offer_text));
        let (providers, provider_of, profiles) = group_providers(&offers);
        let provider_index = TermIndex::build(profiles);
        Self {
            source_id,
            offers,
            endpoints,
            providers,
            provider_index,
            provider_of,
            declared_total,
            read,
            pages,
            stopped,
            refreshed_unix,
            ms,
        }
    }

    pub fn len(&self) -> usize {
        self.offers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.offers.is_empty()
    }

    /// Every endpoint the crawl holds, in crawl order.
    ///
    /// For LISTING the index — `/v1/sources/{id}/apis` — where a query is a
    /// filter rather than the question. Browsing has to see the same corpus
    /// discovery searches: a source whose first page is 100 of 27,925 lists
    /// seven sellers out of a hundred and eighty-three and says nothing about
    /// the rest, and its per-provider page then answers 404 for sellers the
    /// crawl knows perfectly well.
    pub fn offers(&self) -> &[ExternalOffer] {
        &self.offers
    }

    /// Distinct sellers this index published.
    pub fn providers(&self) -> usize {
        self.providers.len()
    }

    /// One durable row per seller, aggregated from the crawl.
    ///
    /// This is the crawl's own grouping — the same `providers` layer the
    /// search runs on — written down: what `source_providers` (migration 090)
    /// stores and the admin curates. Everything in it is the index's claim,
    /// aggregated; nothing is invented. `top_terms` is the seller's most
    /// frequent vocabulary across its own catalogue, the searchable answer to
    /// "what is this provider repeatedly about" for sellers whose endpoint
    /// paths say nothing.
    ///
    /// `with_input_schema` counts what the crawl HOLDS, which is zero while
    /// the crawl lane strips schemas for memory (see the module doc); it
    /// starts counting the moment that changes, with no edit here.
    pub fn provider_snapshot(&self) -> Vec<ProviderSnapshot> {
        self.providers
            .iter()
            .map(|provider| {
                let offers: Vec<&ExternalOffer> = provider
                    .offers
                    .iter()
                    .map(|&idx| &self.offers[idx as usize])
                    .collect();
                aggregate_provider(&provider.key, &offers)
            })
            .collect()
    }

    /// The offers whose text plausibly serves any of these terms, best first.
    ///
    /// Three things happen here, in the order the pipeline states them:
    ///
    /// * **L3, endpoints.** Every endpoint whose own words match, and how many
    ///   distinct wanted terms each one matched. Same predicate as
    ///   [`crate::sources::relevant`], answered off the trigram index.
    /// * **L2, providers.** How many distinct wanted terms each SELLER matches
    ///   across its whole catalogue, which is the question a flat endpoint
    ///   search cannot ask. Asked for `analyst estimates for NVIDIA`, a
    ///   provider whose `/estimates` endpoint matches one term and whose
    ///   `/earnings` endpoint matches another answers two thirds of the job
    ///   while every one of its endpoints looks like a one-word match. That
    ///   provider now leads a provider whose single endpoint happens to repeat
    ///   one of the words.
    /// * **The cut.** Slots are dealt round-robin across the matching sellers
    ///   in runs of [`PER_PROVIDER_RUN`], sellers in provider-score order.
    ///   Truncating a flat list at [`MAX_MATCHES_PER_SOURCE`] handed the whole
    ///   allowance to whichever seller publishes the most endpoints about the
    ///   topic, and the sweep then merges and truncates AGAIN — so a search of
    ///   26,000 rows arrived as one seller's catalogue. Dealing them out keeps
    ///   the same ceiling and spends it on the ecosystem.
    ///
    /// Whatever room is left over is spent topping the leading sellers up with
    /// their remaining endpoints (see [`MAX_TOPPED_UP_PROVIDERS`]) — the
    /// endpoint-level half of the pipeline, run against the crawl rather than
    /// against a live manifest.
    ///
    /// The sweep re-ranks whatever survives on its own scoring; this only
    /// decides what survives to be re-ranked, and it never filters on price or
    /// on anything else a caller stated — those belong to the ranking, which
    /// can see the whole merged list.
    pub fn matching(&self, queries: &[String]) -> Vec<ExternalOffer> {
        let wanted = wanted_terms(queries);
        if wanted.is_empty() {
            // Same contract as `relevant`: no words is not a filter. Still
            // bounded, because "everything" out of a 26,000-entry index is not
            // an answer anyone asked for.
            return self
                .offers
                .iter()
                .take(MAX_MATCHES_PER_SOURCE)
                .cloned()
                .collect();
        }

        // L2 first: what each seller is worth as a whole.
        let provider_score: HashMap<u32, u32> =
            self.provider_index.hits(&wanted).into_iter().collect();

        // L3, grouped into one lane per seller. `hits` is ordered best first,
        // so each lane holds its seller's endpoints in their own rank order
        // and `best` is the head of it.
        let mut lanes: Vec<Lane> = Vec::new();
        let mut lane_of: HashMap<u32, usize> = HashMap::new();
        for (offer, score) in self.endpoints.hits(&wanted) {
            let provider = self.provider_of[offer as usize];
            match lane_of.get(&provider) {
                Some(lane) => lanes[*lane].hits.push(offer),
                None => {
                    lane_of.insert(provider, lanes.len());
                    lanes.push(Lane {
                        provider,
                        // A seller always scores at least as well as its best
                        // endpoint, except where its profile was truncated for
                        // size; take the larger so truncation can only ever
                        // cost precision, never a candidate.
                        score: provider_score
                            .get(&provider)
                            .copied()
                            .unwrap_or(0)
                            .max(score),
                        best: score,
                        hits: vec![offer],
                    });
                }
            }
        }
        lanes.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then(b.best.cmp(&a.best))
                .then(a.provider.cmp(&b.provider))
        });

        let mut out: Vec<u32> = Vec::new();
        let mut cursors = vec![0usize; lanes.len()];
        'dealing: loop {
            let mut dealt = false;
            for (i, lane) in lanes.iter().enumerate() {
                let mut run = 0usize;
                while run < PER_PROVIDER_RUN && cursors[i] < lane.hits.len() {
                    out.push(lane.hits[cursors[i]]);
                    cursors[i] += 1;
                    run += 1;
                    dealt = true;
                    if out.len() >= MAX_MATCHES_PER_SOURCE {
                        break 'dealing;
                    }
                }
            }
            if !dealt {
                break;
            }
        }

        // The leading sellers' remaining endpoints, if there is room. These
        // matched nothing themselves and are ranked last by the sweep, so they
        // can only fill out a thin answer — which is exactly the answer a
        // tersely-described endpoint used to be missing from.
        if out.len() < MAX_MATCHES_PER_SOURCE {
            let held: HashSet<u32> = out.iter().copied().collect();
            for lane in lanes
                .iter()
                .filter(|lane| lane.score >= MIN_TOPUP_SCORE)
                .take(MAX_TOPPED_UP_PROVIDERS)
            {
                let Some(seller) = self.providers.get(lane.provider as usize) else {
                    continue;
                };
                let mut run = 0usize;
                for offer in &seller.offers {
                    if run >= PER_PROVIDER_RUN || out.len() >= MAX_MATCHES_PER_SOURCE {
                        break;
                    }
                    if held.contains(offer) {
                        continue;
                    }
                    out.push(*offer);
                    run += 1;
                }
                if out.len() >= MAX_MATCHES_PER_SOURCE {
                    break;
                }
            }
        }

        out.into_iter()
            .filter_map(|idx| self.offers.get(idx as usize).cloned())
            .collect()
    }

    /// The coverage line for this source, for `meta` blocks to publish.
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.source_id,
            "indexed": self.offers.len(),
            "providers": self.providers.len(),
            "read": self.read,
            "index_reports_total": self.declared_total,
            "pages": self.pages,
            "complete": self.stopped.complete(),
            "stopped_because": self.stopped.as_str(),
            "refreshed_unix": self.refreshed_unix,
            "crawl_ms": self.ms,
        })
    }
}

/// The query as distinct tokens, in first-seen order.
///
/// Order is what [`TermIndex::hits`] stamps against, so it decides nothing
/// about the answer; deduplication does — a vocabulary that says `earnings`
/// three times must not score an offer three times for it.
fn wanted_terms(queries: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for query in queries {
        for token in tokens(query) {
            if seen.insert(token.clone()) {
                out.push(token);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

/// Every crawled catalogue, by source id.
///
/// Swapped wholesale per source on refresh: a search either sees the previous
/// crawl or the next one, never a half-rebuilt index. Empty until the first
/// refresh finishes, and an empty entry is not an error — the sweep falls back
/// to reading the source's first page exactly as it did before.
/// How much of the ecosystem the crawl currently holds.
#[derive(Debug, Clone, Default)]
pub struct Reach {
    pub endpoints: usize,
    pub providers: usize,
    pub sources: usize,
    /// Which indexes those came from, so an answer can name them.
    pub names: Vec<String>,
}

#[derive(Default)]
pub struct SourceIndex {
    inner: RwLock<HashMap<String, Arc<Catalogue>>>,
}

impl SourceIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get(&self, source_id: &str) -> Option<Arc<Catalogue>> {
        self.inner.read().await.get(source_id).cloned()
    }

    async fn put(&self, catalogue: Catalogue) {
        self.inner
            .write()
            .await
            .insert(catalogue.source_id.clone(), Arc::new(catalogue));
    }

    /// Forget catalogues whose source is no longer enabled.
    ///
    /// An admin disabling a row must not leave its 26,000 entries resident and
    /// searchable — the sweep would stop asking the source while the index
    /// kept answering for it, which is the "unfalsifiable set" that
    /// [`crate::sources::active_sources`] exists to prevent.
    async fn retain(&self, live: &HashSet<String>) {
        self.inner.write().await.retain(|id, _| live.contains(id));
    }

    /// Total offers held, across every source.
    pub async fn total(&self) -> usize {
        self.inner.read().await.values().map(|c| c.len()).sum()
    }

    /// Every crawled index at once, deduped on the resource. No network and no
    /// budget, unlike [`crate::sources`]'s sweep: this is all in memory.
    pub async fn search(&self, queries: &[String]) -> Vec<ExternalOffer> {
        let held = self.inner.read().await;
        let mut catalogues: Vec<&Arc<Catalogue>> = held.values().collect();
        catalogues.sort_by(|a, b| a.source_id.cmp(&b.source_id));

        let mut seen: HashSet<String> = HashSet::new();
        let mut out: Vec<ExternalOffer> = Vec::new();
        for catalogue in catalogues {
            for offer in catalogue.matching(queries) {
                if seen.insert(offer.resource.to_ascii_lowercase()) {
                    out.push(offer);
                }
            }
        }
        out
    }

    /// EVERY crawled offer, deduped on the resource. No terms, no caps.
    ///
    /// [`Self::search`] answers "what looks like this question", and to stay
    /// affordable it cuts each source at [`MAX_MATCHES_PER_SOURCE`] — 400 out
    /// of the 2,490 the crawl holds. That cut is invisible from the outside and
    /// it decides which endpoints a model is ever allowed to see: a listing
    /// ranked 401st by a substring sweep is gone before anything reads it.
    ///
    /// `/v1/ask` wants the opposite contract. Its own lexical pass already
    /// scores the whole pool in about a tenth of a second, and it weighs terms
    /// by how RARE they are rather than by how long they are, which the sweep
    /// does not. Handing it everything and letting it cut once is both cheaper
    /// to reason about and strictly wider than cutting twice with two different
    /// rulers.
    ///
    /// Ordered by source id and then by the crawl's own order, so the same
    /// index gives the same list to every request.
    pub async fn all(&self) -> Vec<ExternalOffer> {
        let held = self.inner.read().await;
        let mut catalogues: Vec<&Arc<Catalogue>> = held.values().collect();
        catalogues.sort_by(|a, b| a.source_id.cmp(&b.source_id));

        let mut seen: HashSet<String> = HashSet::new();
        let mut out: Vec<ExternalOffer> = Vec::new();
        for catalogue in catalogues {
            for offer in &catalogue.offers {
                if seen.insert(offer.resource.to_ascii_lowercase()) {
                    out.push(offer.clone());
                }
            }
        }
        out
    }

    /// What the crawl can reach: the number behind "we searched the indexes".
    pub async fn reach(&self) -> Reach {
        let held = self.inner.read().await;
        let mut names: Vec<String> = held.values().map(|c| c.source_id.clone()).collect();
        names.sort();
        Reach {
            endpoints: held.values().map(|c| c.len()).sum(),
            providers: held.values().map(|c| c.providers()).sum(),
            sources: held.len(),
            names,
        }
    }

    /// One coverage line per crawled source, for a response's `meta`.
    pub async fn coverage_json(&self) -> Vec<Value> {
        let held = self.inner.read().await;
        let mut out: Vec<Value> = held.values().map(|c| c.to_json()).collect();
        out.sort_by_key(|v| v["id"].as_str().unwrap_or_default().to_string());
        out
    }
}

// ---------------------------------------------------------------------------
// Pagination, read off the index rather than configured per row
// ---------------------------------------------------------------------------

/// Which cursor an index echoed back, and therefore which one it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cursor {
    /// `?offset=` — the x402 discovery dialect (Bazaar, PayAI, thirdweb) and
    /// agentic.market.
    Offset,
    /// `?page=`, 1-based — x402.direct.
    Page,
    /// Nothing echoed: a manifest. One document, and it really is whole.
    None,
}

/// A number under `pagination.<key>` or at the top level, whichever the index
/// uses. Both spellings are live: PayAI nests, agentic.market does not.
fn number(body: &Value, key: &str) -> Option<u64> {
    body.get("pagination")
        .and_then(|p| p.get(key))
        .and_then(Value::as_u64)
        .or_else(|| body.get(key).and_then(Value::as_u64))
}

fn cursor_of(body: &Value) -> Cursor {
    if number(body, "offset").is_some() {
        Cursor::Offset
    } else if number(body, "page").is_some() {
        Cursor::Page
    } else {
        Cursor::None
    }
}

/// Entries in this document, before parsing drops the unreadable ones.
///
/// The crawler advances its cursor by THIS, never by the page size it asked
/// for and never by the page size the index echoed. An index that echoes
/// `limit: 1000` while serving 200 would otherwise make the crawler skip 800
/// entries per page and call the result a whole catalogue.
fn raw_len(body: &Value) -> usize {
    raw_entries(body).map(<[Value]>::len).unwrap_or(0)
}

/// The page's entry array, whichever key this dialect puts it under.
fn raw_entries(body: &Value) -> Option<&[Value]> {
    if let Some(array) = body.as_array() {
        return Some(array);
    }
    for key in [
        "items",
        "resources",
        "services",
        "data",
        "results",
        "routes",
    ] {
        if let Some(array) = body.get(key).and_then(Value::as_array) {
            return Some(array);
        }
    }
    None
}

/// A cheap identity for one page of raw entries.
///
/// This is how "the index is ignoring the cursor" is told apart from "that
/// page parsed to nothing" — two things the crawl used to answer with the same
/// number, and getting them confused costs an index. PayAI publishes ~17,000
/// consecutive listings with no description at all, all from one host; the
/// parser drops every one of them (an offer with no words cannot be searched
/// or shown), so three pages in a row yielded nothing and the crawl called
/// itself finished at 11,000 of 27,926 — while the cursor had been moving the
/// whole time and real sellers were waiting past them.
///
/// Count, first and last entry: an index handing back the same window forever
/// repeats all three, and an index that has moved on changes at least one.
/// Hashing every entry of every page would be exact and is not worth twenty-
/// eight megabytes of formatting per crawl to catch a case this already
/// catches.
fn page_fingerprint(body: &Value) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let entries = raw_entries(body).unwrap_or(&[]);
    entries.len().hash(&mut hasher);
    for entry in entries.first().into_iter().chain(entries.last()) {
        entry.to_string().hash(&mut hasher);
    }
    hasher.finish()
}

/// `base` with these query parameters set, replacing any the row already had.
///
/// Every other parameter survives untouched, which is what keeps x402.direct's
/// `sort=score` — a row's own tuning is not the crawler's to discard.
fn with_params(base: &str, pairs: &[(&str, String)]) -> Option<String> {
    let mut url = url::Url::parse(base).ok()?;
    let existing: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    {
        let mut query = url.query_pairs_mut();
        query.clear();
        for (key, value) in &existing {
            if pairs.iter().any(|(name, _)| name == key) {
                continue;
            }
            query.append_pair(key, value);
        }
        for (key, value) in pairs {
            query.append_pair(key, value);
        }
    }
    Some(url.to_string())
}

/// The URL for one page of this source.
fn page_url(base: &str, cursor: Cursor, position: u64, limit: u64) -> Option<String> {
    match cursor {
        Cursor::Offset => with_params(
            base,
            &[
                ("limit", limit.to_string()),
                ("offset", position.to_string()),
            ],
        ),
        Cursor::Page => with_params(
            base,
            &[("limit", limit.to_string()), ("page", position.to_string())],
        ),
        Cursor::None => Some(base.to_string()),
    }
}

// ---------------------------------------------------------------------------
// The crawl
// ---------------------------------------------------------------------------

/// Reads one indexed source to the end and builds its catalogue.
///
/// Returns `None` only when the very first page could not be read at all —
/// there is nothing to publish, and overwriting a good previous crawl with an
/// empty one would turn a transient upstream blip into a search that finds
/// nothing.
async fn crawl(state: &Arc<AppState>, source: &Source, room: usize) -> Option<Catalogue> {
    let Fetch::Indexed { url: base } = &source.fetch else {
        return None;
    };

    let started = tokio::time::Instant::now();
    let deadline = started + SOURCE_BUDGET;
    let per_source = env_usize("SOURCE_INDEX_MAX_PER_SOURCE", DEFAULT_MAX_PER_SOURCE).min(room);

    let mut offers: Vec<ExternalOffer> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    // The quality gate's bookkeeping: how many one host has contributed so
    // far, and what was refused and why. Both are per crawl, so the cap is a
    // property of the published corpus rather than of whichever page an entry
    // happened to arrive on.
    let mut per_host: HashMap<String, usize> = HashMap::new();
    let mut refused: HashMap<&'static str, usize> = HashMap::new();
    let mut pages_seen: HashSet<u64> = HashSet::new();
    let mut declared: Option<u64> = None;
    let mut cursor = Cursor::Offset;
    let mut granted = PAGE_ASK;
    let mut position: u64 = 0;
    let mut read = 0usize;
    let mut pages = 0usize;
    let mut barren = 0usize;
    let mut stopped = Stop::Exhausted;

    loop {
        if pages >= MAX_PAGES || tokio::time::Instant::now() >= deadline {
            stopped = Stop::Budget;
            break;
        }

        // Page one is asked for with the cursor the dialect probably uses;
        // what comes back decides the rest. `Cursor::Offset` is the opening
        // guess only because four of the five speak it, and an index that
        // ignores an unknown `offset` on page one loses nothing by it.
        let Some(url) = page_url(base, cursor, position, granted) else {
            warn!(source = %source.id, "index url is unparseable: {base}");
            return None;
        };

        let body = match fetch_page(state, source, &url).await {
            Some(body) => body,
            None => {
                if pages == 0 {
                    warn!(source = %source.id, "first page unreadable; keeping the previous crawl");
                    return None;
                }
                stopped = Stop::ReadFailed;
                break;
            }
        };

        if pages == 0 {
            cursor = cursor_of(&body);
            declared = declared_total(&body);
            // What the index GRANTED, not what the row asked for. This is the
            // whole fix for the hand-written page sizes: thirdweb answers 200
            // to an ask of 1000 and says so, so 200 is what the next page asks
            // for.
            granted = number(&body, "limit")
                .filter(|l| *l > 0)
                .unwrap_or(PAGE_ASK);
        }

        let entries = raw_len(&body);
        read += entries;
        pages += 1;

        let ctx = source.ctx(&url);
        let mut fresh = 0usize;
        for mut offer in crate::sources::parse_in(source.shape, &ctx, &body) {
            // The filters that are about the offer rather than the query,
            // applied here so they are paid once per crawl instead of once per
            // search — and so the caps below count only entries a search could
            // ever return. See `crate::bazaar::quality_gate`: this is the floor
            // that lets the sweep be on at all.
            if let Err(reason) = quality_gate(
                &offer.resource,
                &offer.host,
                &offer.description,
                offer.price_usd,
                offer.network.as_deref(),
            ) {
                *refused.entry(reason.label()).or_default() += 1;
                continue;
            }
            // And then the bar. The gate above says a stranger could call it;
            // this says it is documented well enough to be worth handing to an
            // agent that has to fill in the parameters without asking anyone.
            // See `crate::bazaar::score_listing` — the score was computed at
            // parse time, because the schemas it reads are gone by now.
            if offer.doc_score < MIN_LISTING_SCORE {
                *refused.entry("below_score").or_default() += 1;
                continue;
            }
            if !seen.insert(offer.resource.clone()) {
                continue;
            }
            // One seller's thousand near-identical endpoints are not a
            // thousand answers. Counted on the www-stripped host, after the
            // gate, so the allowance is spent on entries that could be
            // returned rather than on ones that were going to be dropped.
            let host_key = offer.host.trim_start_matches("www.").to_ascii_lowercase();
            let slot = per_host.entry(host_key).or_default();
            if *slot >= MAX_PER_HOST {
                *refused.entry("host_cap").or_default() += 1;
                continue;
            }
            *slot += 1;
            // See the module doc: schemas are 60% of the bytes and none of the
            // findability. `method` is already derived from `input` by the
            // parser, so it survives this.
            offer.input = None;
            offer.input_schema = None;
            offer.output = None;
            offers.push(offer);
            fresh += 1;
        }

        if entries == 0 {
            stopped = Stop::Exhausted;
            break;
        }
        // Whether the PAGE moved, never whether it yielded. See
        // `page_fingerprint` and `MAX_BARREN_PAGES`.
        if pages_seen.insert(page_fingerprint(&body)) {
            barren = 0;
        } else {
            barren += 1;
            // Only after several in a row: an index whose ordering shifts
            // between requests hands back a window we have already seen and
            // then carries on with new entries. See `MAX_BARREN_PAGES`.
            if barren >= MAX_BARREN_PAGES && pages > 1 {
                stopped = Stop::NoNewEntries;
                break;
            }
        }
        // The gap between the two is this page's yield, and it is the only
        // place it is visible: a source whose pages are 1,000 entries and 0
        // offers is being read correctly and is still handing over nothing.
        debug!(
            source = %source.id,
            page = pages,
            entries,
            fresh,
            "page read"
        );
        if cursor == Cursor::None {
            stopped = Stop::SinglePage;
            break;
        }
        if offers.len() >= per_source {
            offers.truncate(per_source);
            stopped = Stop::Capped;
            break;
        }
        if declared.is_some_and(|total| read as u64 >= total) {
            stopped = Stop::Exhausted;
            break;
        }

        position = match cursor {
            // By what actually arrived. See `raw_len`.
            Cursor::Offset => position + entries as u64,
            Cursor::Page => position.max(1) + 1,
            Cursor::None => break,
        };

        tokio::time::sleep(PAGE_PAUSE).await;
    }

    // What the gate cost this source, by reason. The single most useful line
    // for tuning the four constants in `crate::bazaar`: "kept 3,900 of 15,167"
    // is a number to argue with, and a gate whose effect is invisible is one
    // that drifts until somebody turns the whole sweep off again.
    if !refused.is_empty() {
        let mut reasons: Vec<String> = refused
            .iter()
            .map(|(reason, count)| format!("{reason}={count}"))
            .collect();
        reasons.sort();
        info!(
            source = %source.id,
            read,
            kept = offers.len(),
            hosts = per_host.len(),
            refused = %reasons.join(" "),
            "quality gate"
        );
    }

    Some(Catalogue::build(
        source.id.clone(),
        offers,
        declared,
        read,
        pages,
        stopped,
        now_unix(),
        started.elapsed().as_millis() as u64,
    ))
}

/// One page, straight off the network.
///
/// Deliberately NOT through [`crate::cache`]: those entries are the sweep's
/// five-minute document cache, sized for first pages, and posting twenty-seven
/// megabyte-plus documents into it would evict everything else the gateway
/// caches to hold data only this crawl will ever read.
async fn fetch_page(state: &Arc<AppState>, source: &Source, url: &str) -> Option<Value> {
    let method =
        reqwest::Method::from_bytes(source.method.as_bytes()).unwrap_or(reqwest::Method::GET);
    let mut request = state
        .http_client
        .request(method, url)
        .header("Accept", "application/json")
        // Named, because this is the one caller that asks a free index for
        // dozens of pages in a row. An operator who wants to rate-limit or
        // talk to us can now tell which traffic is ours; the shared client
        // sends no user agent at all, and an unnamed crawler is the kind that
        // gets blocked without anybody being able to ask it to slow down.
        .header(
            reqwest::header::USER_AGENT,
            "ApioskDiscoveryCrawler/1.0 (+https://gateway.apiosk.com)",
        )
        .timeout(PAGE_TIMEOUT);
    if let Some(body) = source.body.clone() {
        request = request
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);
    }

    // For a challenge source the 402 IS the document, and it often arrives
    // only in the `payment-required` header with an empty body. Read exactly
    // the way the sweep reads it (`crate::sources::fetch`) — a crawler that
    // called 402 a failure would drop every challenge source out of the deep
    // index and re-ask it, and fail, on every refresh.
    let challenge = source.shape.expects_challenge();

    let response = match request.send().await {
        Ok(response) if response.status().is_success() => response,
        Ok(response) if challenge && response.status() == reqwest::StatusCode::PAYMENT_REQUIRED => {
            response
        }
        Ok(response) => {
            debug!(source = %source.id, "page returned {}", response.status());
            return None;
        }
        Err(err) => {
            debug!(source = %source.id, "page failed: {err}");
            return None;
        }
    };

    let from_header = crate::sources::challenge_from_header(response.headers());
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(err) => {
            debug!(source = %source.id, "page body unreadable: {err}");
            return None;
        }
    };
    let from_body = serde_json::from_slice::<Value>(&bytes).ok();
    crate::sources::best_document(from_body, from_header, challenge)
}

// ---------------------------------------------------------------------------
// The loop
// ---------------------------------------------------------------------------

/// Crawls every enabled indexed source once.
///
/// Sequential across sources on purpose. The whole point of doing this off the
/// request path is that it does not need to be fast, and five concurrent
/// crawls would put five multi-megabyte documents in memory at the same moment
/// on a box that has 256 MB of it.
async fn refresh_all(state: &Arc<AppState>) {
    let sources = crate::sources::active_sources(state).await;
    let live: HashSet<String> = sources.iter().map(|s| s.id.clone()).collect();
    state.source_index.retain(&live).await;

    let max_total = env_usize("SOURCE_INDEX_MAX_TOTAL", DEFAULT_MAX_TOTAL);

    for source in sources.iter().filter(|s| s.is_browsable()) {
        // Room left in the global budget, counting what THIS source's previous
        // crawl already holds as returnable — it is about to be replaced.
        let held = state.source_index.total().await;
        let mine = state
            .source_index
            .get(&source.id)
            .await
            .map(|c| c.len())
            .unwrap_or(0);
        let room = max_total.saturating_sub(held.saturating_sub(mine));
        if room == 0 {
            warn!(
                source = %source.id,
                "SOURCE_INDEX_MAX_TOTAL reached; this source is left at its first page"
            );
            continue;
        }

        if let Some(catalogue) = crawl(state, source, room).await {
            info!(
                source = %source.id,
                indexed = catalogue.len(),
                read = catalogue.read,
                declared = ?catalogue.declared_total,
                pages = catalogue.pages,
                complete = catalogue.stopped.complete(),
                stopped = catalogue.stopped.as_str(),
                ms = catalogue.ms,
                "indexed source crawled"
            );
            // Write the seller rows down while the crawl is in hand — the
            // durable half of what was just indexed (`source_providers`,
            // migration 090). A write failure costs the snapshot's freshness
            // and nothing else: the in-memory index still serves the sweep.
            let snapshot = catalogue.provider_snapshot();
            if let Err(err) = state
                .db
                .upsert_source_providers(&source.id, &snapshot)
                .await
            {
                warn!(
                    source = %source.id,
                    "source_providers snapshot not written (is migration 090 applied?): {err}"
                );
            }
            state.source_index.put(catalogue).await;
        }
    }
}

/// Keep every indexed source fully crawled for as long as the process lives.
///
/// The first pass runs immediately rather than after one interval: a gateway
/// that restarts must not spend fifteen minutes searching three percent of the
/// ecosystem. Until it finishes, the sweep behaves exactly as it did before —
/// first page, from the document cache — so a cold start is degraded, never
/// broken.
pub fn spawn(state: Arc<AppState>, tasks: &crate::background::BackgroundTasks) {
    if !enabled() {
        info!("SOURCE_INDEX_ENABLED is off; indexed sources stay at their first page");
        return;
    }
    let interval = env_u64("SOURCE_INDEX_REFRESH_SECS", DEFAULT_REFRESH_SECS);

    tasks.spawn(async move {
        loop {
            refresh_all(&state).await;
            // Resolved before the macro: an `.await` inside `info!`'s argument
            // list holds a non-`Send` `fmt::Arguments` across the suspension
            // point, and the whole task stops being spawnable.
            let offers = state.source_index.total().await;
            info!(offers, "external index refreshed; next pass in {interval}s");
            tokio::time::sleep(Duration::from_secs(interval)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One offer, served from a named host so the provider layer has sellers
    /// to tell apart.
    fn served(host: &str, path: &str, description: &str) -> ExternalOffer {
        ExternalOffer {
            resource: format!("https://{host}{path}"),
            host: host.to_string(),
            description: description.to_string(),
            ..Default::default()
        }
    }

    fn catalogue(offers: Vec<ExternalOffer>) -> Catalogue {
        let read = offers.len();
        Catalogue::build("t".into(), offers, None, read, 1, Stop::SinglePage, 0, 0)
    }

    fn named_catalogue(id: &str, offers: Vec<ExternalOffer>) -> Catalogue {
        let read = offers.len();
        Catalogue::build(id.into(), offers, None, read, 1, Stop::SinglePage, 0, 0)
    }

    // -- searching every index at once --------------------------------------

    #[tokio::test]
    async fn search_reads_every_index_and_lists_a_shared_endpoint_once() {
        let shared = served("api.ukintel.uk", "/credit-score", "UK company credit score");
        let index = SourceIndex::new();
        index
            .put(named_catalogue(
                "coinbase-x402-bazaar-index",
                vec![
                    shared.clone(),
                    served("api.sirenic.eu", "/recherche", "French company search"),
                ],
            ))
            .await;
        index.put(named_catalogue("payai", vec![shared])).await;

        let hits = index.search(&["credit score".to_string()]).await;

        assert_eq!(
            hits.len(),
            1,
            "the same endpoint in two indexes is one endpoint"
        );
        assert_eq!(hits[0].host, "api.ukintel.uk");

        let reach = index.reach().await;
        assert_eq!(reach.sources, 2);
        assert_eq!(
            reach.endpoints, 3,
            "reach counts what is held, not what matched"
        );
    }

    // -- the durable seller snapshot ----------------------------------------

    #[test]
    fn a_provider_snapshot_aggregates_sellers_not_endpoints() {
        let mut spot = served("api.coingecko.com", "/price", "Crypto spot price by symbol");
        spot.price_usd = Some(0.01);
        spot.tags = vec!["crypto".to_string()];
        let mut candles = served("pro-api.coingecko.com", "/ohlc", "Crypto OHLC candles");
        candles.price_usd = Some(0.05);
        let weather = served("weather.example.net", "/now", "Current weather by city");

        let snapshot = catalogue(vec![spot, candles, weather]).provider_snapshot();
        assert_eq!(snapshot.len(), 2, "three hosts, two registrable domains");

        let gecko = snapshot.iter().find(|p| p.key == "coingecko.com").unwrap();
        assert_eq!(gecko.endpoint_count, 2, "both subdomains are one seller");
        assert_eq!(gecko.price_usd_min, Some(0.01));
        assert_eq!(gecko.price_usd_max, Some(0.05));
        assert_eq!(gecko.unpriced_endpoints, 0);
        assert!(gecko.tags.contains(&"crypto".to_string()));
        // The seller's own repeated vocabulary is its functionality summary.
        assert!(gecko.top_terms.contains(&"crypto".to_string()));

        // Keyed by registrable domain — the ranking's own grouping — with the
        // full host kept alongside for display.
        let weather = snapshot.iter().find(|p| p.key == "example.net").unwrap();
        assert_eq!(weather.sample_host, "weather.example.net");
        assert_eq!(weather.unpriced_endpoints, 1);
        assert_eq!(weather.price_usd_min, None);
    }

    #[test]
    fn a_queried_answer_snapshots_without_double_counting() {
        // The same endpoint arriving under two query phrasings is one
        // endpoint. Counting it twice would inflate a floor the accretion
        // then never lets shrink.
        let offers = vec![
            served("api.coingecko.com", "/price", "Crypto spot price"),
            served("api.coingecko.com", "/price", "Crypto spot price"),
            served("api.coingecko.com", "/ohlc", "Crypto OHLC candles"),
        ];
        let snapshot = snapshot_offers(&offers);
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].key, "coingecko.com");
        assert_eq!(snapshot[0].endpoint_count, 2);
    }

    // -- pagination dialects ------------------------------------------------

    #[test]
    fn the_nested_and_flat_spellings_of_a_total_both_read() {
        // PayAI nests under `pagination`; agentic.market does not. Both are
        // live, so both have to work or one index silently stops at page one.
        let nested = json!({"pagination": {"limit": 1000, "offset": 0, "total": 26684}});
        let flat = json!({"limit": 200, "offset": 0, "total": 2381});
        assert_eq!(number(&nested, "limit"), Some(1000));
        assert_eq!(number(&flat, "limit"), Some(200));
        assert_eq!(cursor_of(&nested), Cursor::Offset);
        assert_eq!(cursor_of(&flat), Cursor::Offset);
    }

    #[test]
    fn a_page_dialect_is_recognised_as_one() {
        // x402.direct answers `{"pagination": {"page": 1, "totalPages": 41}}`
        // and ignores `offset` entirely.
        let body =
            json!({"pagination": {"page": 1, "limit": 100, "total": 4019, "totalPages": 41}});
        assert_eq!(cursor_of(&body), Cursor::Page);
    }

    #[test]
    fn a_manifest_has_no_cursor_and_is_therefore_whole() {
        let body = json!({"baseUrl": "https://x.example", "services": [{"endpoint": "/a"}]});
        assert_eq!(cursor_of(&body), Cursor::None);
        assert!(Stop::SinglePage.complete());
    }

    #[test]
    fn a_page_that_parses_to_nothing_is_not_the_end_of_the_index() {
        // PayAI's real shape: ~17,000 consecutive listings with no description
        // at all, which the parser drops entirely. Three such pages ended the
        // crawl at 11,000 of a declared 27,926. They are DIFFERENT pages, and
        // that is the whole question the barren counter is asking.
        let stub = |slug: &str| json!({"resource": format!("https://orbisapi.com/proxy/{slug}"), "description": null});
        let page = |first: &str, last: &str| {
            json!({"pagination": {"limit": 2, "offset": 0, "total": 27_926},
                   "items": [stub(first), stub(last)]})
        };
        assert_ne!(
            page_fingerprint(&page("a", "b")),
            page_fingerprint(&page("c", "d")),
        );
    }

    #[test]
    fn an_index_that_ignores_the_cursor_hands_back_the_same_page() {
        // The case the counter exists for: identical windows forever. Same
        // count, same first entry, same last entry.
        let body = json!({"pagination": {"limit": 2, "offset": 0, "total": 900},
                          "items": [{"resource": "https://a.test/1"},
                                    {"resource": "https://a.test/2"}]});
        assert_eq!(page_fingerprint(&body), page_fingerprint(&body.clone()));
    }

    #[test]
    fn a_shorter_last_page_is_its_own_page() {
        let full = json!({"items": [{"r": 1}, {"r": 2}, {"r": 3}]});
        let tail = json!({"items": [{"r": 1}, {"r": 2}]});
        assert_ne!(page_fingerprint(&full), page_fingerprint(&tail));
    }

    #[test]
    fn the_cursor_advances_by_what_arrived_not_by_what_was_promised() {
        // The failure this guards: an index echoing `limit: 1000` while
        // serving 200. Advancing by the echo skips 800 entries a page and
        // still reports a whole catalogue.
        let body = json!({"pagination": {"limit": 1000, "offset": 0, "total": 5000},
                          "items": [1, 2, 3]});
        assert_eq!(raw_len(&body), 3);
        assert_eq!(number(&body, "limit"), Some(1000));
    }

    #[test]
    fn entries_are_counted_under_every_key_the_shapes_use() {
        assert_eq!(raw_len(&json!({"items": [1, 2]})), 2);
        assert_eq!(raw_len(&json!({"services": [1, 2, 3]})), 3);
        assert_eq!(raw_len(&json!({"resources": [1]})), 1);
        assert_eq!(raw_len(&json!([1, 2, 3, 4])), 4);
        assert_eq!(raw_len(&json!({"nothing": true})), 0);
    }

    // -- url building -------------------------------------------------------

    #[test]
    fn paging_replaces_the_rows_own_limit_and_keeps_everything_else() {
        // The hand-written `limit=100` is what this whole module exists to
        // stop trusting; `sort=score` is the admin's tuning and must survive.
        let url = page_url(
            "https://x402.direct/api/services?limit=100&sort=score",
            Cursor::Page,
            2,
            100,
        )
        .expect("parses");
        assert!(url.contains("sort=score"), "{url}");
        assert!(url.contains("page=2"), "{url}");
        assert_eq!(url.matches("limit=").count(), 1, "one limit only: {url}");
        assert!(
            !url.contains("limit=100&sort"),
            "the row's limit was kept: {url}"
        );
    }

    #[test]
    fn a_source_with_no_limit_at_all_gets_one() {
        // PayAI's row asked for nothing and got the facilitator's default of
        // 100 out of 26,684.
        let url = page_url(
            "https://facilitator.payai.network/discovery/resources",
            Cursor::Offset,
            0,
            1000,
        )
        .expect("parses");
        assert!(url.contains("limit=1000"), "{url}");
        assert!(url.contains("offset=0"), "{url}");
    }

    #[test]
    fn a_cursorless_source_is_fetched_exactly_as_configured() {
        let base = "https://x402engine.app/.well-known/x402.json";
        assert_eq!(page_url(base, Cursor::None, 0, 1000).as_deref(), Some(base));
    }

    // -- the inverted index -------------------------------------------------

    #[test]
    fn matching_finds_by_substring_the_way_the_linear_filter_does() {
        // Separate hosts, so this test is about the substring predicate alone
        // and not about a seller's neighbouring endpoints coming along.
        let catalogue = catalogue(vec![
            served("a.example", "/flights", "flight status by number"),
            served("b.example", "/fx", "currency conversion rates"),
        ]);

        let hits = catalogue.matching(&["flight".to_string()]);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].resource.contains("flights"));

        // A prefix must still hit, because `relevant` keeps an offer when the
        // wanted word is a substring of one of its tokens.
        let hits = catalogue.matching(&["curren".to_string()]);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].resource.contains("/fx"));
        assert_eq!(catalogue.matching(&["helicopter".to_string()]).len(), 0);
    }

    #[test]
    fn more_matched_terms_ranks_higher() {
        let catalogue = catalogue(vec![
            served("a.example", "/one", "weather data"),
            served("b.example", "/two", "weather forecast data"),
        ]);
        let hits = catalogue.matching(&["weather".to_string(), "forecast".to_string()]);
        assert_eq!(hits.len(), 2);
        assert!(
            hits[0].resource.ends_with("/two"),
            "the offer matching both terms must lead"
        );
    }

    #[test]
    fn a_wanted_word_no_index_has_heard_of_costs_no_dictionary_scan() {
        // The trigram gate is what makes this cheap, and "cheap" is only
        // testable here as "still correct": an unknown trigram must answer
        // nothing rather than fall back to matching something.
        let catalogue = catalogue(vec![served("a.example", "/fx", "currency rates")]);
        assert!(catalogue.matching(&["zzqxvv".to_string()]).is_empty());
        // ...while a word the dictionary does hold, as a substring of a longer
        // token, still matches. This is the pair the fast path has to keep.
        assert_eq!(catalogue.matching(&["urrenc".to_string()]).len(), 1);
    }

    // -- the provider layer -------------------------------------------------

    #[test]
    fn subdomains_of_one_seller_are_one_seller() {
        assert_eq!(registrable_domain("pro-api.coingecko.com"), "coingecko.com");
        assert_eq!(registrable_domain("api.coingecko.com"), "coingecko.com");
        assert_eq!(registrable_domain("coingecko.com"), "coingecko.com");
        // A two-part suffix must not swallow the name in front of it.
        assert_eq!(registrable_domain("api.example.co.uk"), "example.co.uk");
        assert_eq!(registrable_domain("EXAMPLE.COM."), "example.com");
    }

    #[test]
    fn one_seller_cannot_take_the_whole_allowance() {
        // The failure this exists for: an index where one seller publishes
        // hundreds of endpoints about the topic and two others publish one
        // each. Cutting a flat hit list at MAX_MATCHES_PER_SOURCE returned the
        // big seller's catalogue and called it a search of the ecosystem.
        let mut offers: Vec<ExternalOffer> = (0..500)
            .map(|i| served("big.example", &format!("/quote/{i}"), "stock quote"))
            .collect();
        offers.push(served("small.example", "/q", "stock quote"));
        offers.push(served("tiny.example", "/q", "stock quote"));
        let catalogue = catalogue(offers);

        let hits = catalogue.matching(&["stock quote".to_string()]);
        assert_eq!(
            hits.len(),
            MAX_MATCHES_PER_SOURCE,
            "the ceiling still holds"
        );
        let sellers: HashSet<&str> = hits.iter().map(|o| o.host.as_str()).collect();
        assert_eq!(
            sellers.len(),
            3,
            "every seller reached the answer: {sellers:?}"
        );
        // And the small sellers are near the front, not at position 400.
        let first_four: HashSet<&str> = hits.iter().take(4 * 3).map(|o| o.host.as_str()).collect();
        assert!(first_four.contains("small.example"), "{first_four:?}");
        assert!(first_four.contains("tiny.example"), "{first_four:?}");
    }

    #[test]
    fn the_seller_that_answers_more_of_the_job_leads_the_one_that_repeats_a_word() {
        // The whole reason for a provider layer. Asked for analyst estimates
        // AND earnings, `spread.example` answers both — across two endpoints,
        // so neither endpoint is a two-term match and a flat endpoint search
        // ranks it behind `narrow.example`, which says "estimates" twice on
        // one endpoint and has nothing to say about earnings.
        let catalogue = catalogue(vec![
            served(
                "narrow.example",
                "/x",
                "analyst estimates, consensus estimates",
            ),
            served("spread.example", "/estimates", "analyst estimates"),
            served("spread.example", "/earnings", "quarterly earnings"),
        ]);

        let hits = catalogue.matching(&["analyst estimates".to_string(), "earnings".to_string()]);
        assert_eq!(
            hits[0].host,
            "spread.example",
            "the seller that answers more of the job leads: {:?}",
            hits.iter().map(|o| o.resource.as_str()).collect::<Vec<_>>()
        );
        assert!(
            hits.iter().any(|o| o.host == "narrow.example"),
            "and the other seller is still in the answer"
        );
    }

    #[test]
    fn a_leading_sellers_terse_endpoint_arrives_behind_its_hits() {
        // The endpoint that answers is often the tersest one a provider
        // publishes, so keyword retrieval drops exactly the one an agent
        // wanted. Once the SELLER is a candidate, its neighbours come along —
        // behind everything that matched, because they score nothing.
        let catalogue = catalogue(vec![
            served(
                "fmp.example",
                "/income-statement",
                "company income statement",
            ),
            served("fmp.example", "/x91", ""),
            served("other.example", "/z", "unrelated"),
        ]);

        let hits = catalogue.matching(&["income statement".to_string()]);
        let order: Vec<&str> = hits.iter().map(|o| o.resource.as_str()).collect();
        assert_eq!(order[0], "https://fmp.example/income-statement");
        assert!(
            order.contains(&"https://fmp.example/x91"),
            "the terse sibling arrives: {order:?}"
        );
        assert!(
            !order.contains(&"https://other.example/z"),
            "a seller nothing pointed at does not: {order:?}"
        );
    }

    #[test]
    fn a_wordless_search_is_still_bounded_and_still_unfiltered() {
        let offers: Vec<ExternalOffer> = (0..MAX_MATCHES_PER_SOURCE + 50)
            .map(|i| served("a.example", &format!("/{i}"), "thing"))
            .collect();
        let catalogue = catalogue(offers);
        assert_eq!(catalogue.matching(&[]).len(), MAX_MATCHES_PER_SOURCE);
        assert_eq!(catalogue.providers(), 1);
    }

    #[test]
    fn one_word_in_common_does_not_pull_a_sellers_shelf_in() {
        // Every seller in a large index shares a word with every question.
        // Topping up on that much evidence would return a shelf per weak
        // match, which is noise with a provider's name on it.
        let catalogue = catalogue(vec![
            served("shop.example", "/price", "price of a thing"),
            served("shop.example", "/unrelated", "something else entirely"),
        ]);
        let hits = catalogue.matching(&["price".to_string()]);
        assert_eq!(hits.len(), 1, "the weak match alone: {hits:?}");

        // Two terms is evidence about the seller, and the neighbour follows.
        let hits = catalogue.matching(&["price".to_string(), "thing".to_string()]);
        assert_eq!(hits.len(), 2);
        assert!(hits[1].resource.ends_with("/unrelated"));
    }

    // -- what the shape of the index is actually worth ----------------------

    /// A catalogue the size of a real one, with a real one's vocabulary shape:
    /// a few hundred common words, a long tail of rare ones, spread over many
    /// sellers.
    fn synthetic(entries: usize, sellers: usize) -> Vec<ExternalOffer> {
        const COMMON: &[&str] = &[
            "data", "price", "market", "quote", "search", "token", "chain", "score", "index",
            "report", "profile", "history", "stream", "image", "text", "model", "wallet",
        ];
        (0..entries)
            .map(|i| {
                let host = format!("seller{}.example", i % sellers);
                let a = COMMON[i % COMMON.len()];
                let b = COMMON[(i * 7 + 3) % COMMON.len()];
                ExternalOffer {
                    resource: format!("https://{host}/v1/{a}/{b}/e{i}"),
                    host,
                    description: format!("{a} {b} endpoint number {i} for agents rare{i}word"),
                    price_usd: Some((i % 97) as f64 / 1000.0),
                    ..Default::default()
                }
            })
            .collect()
    }

    fn vocabulary(terms: usize) -> Vec<String> {
        const WORDS: &[&str] = &[
            "market data",
            "analyst estimates",
            "earnings",
            "stock quote",
            "forex",
            "weather",
            "geocoding",
            "sentiment",
            "translation",
            "embeddings",
            "corporate directors",
            "company officers",
            "iban validation",
            "kvk",
            "onchain",
            "nft floor",
            "zzqxvv",
        ];
        (0..terms)
            .map(|i| WORDS[i % WORDS.len()].to_string())
            .collect()
    }

    /// The dictionary scan this module replaced: for every wanted token, walk
    /// every key of the postings map testing `contains`. Kept here, in the
    /// test module only, so the claim that the trigram index is faster is
    /// measured against the thing it replaced rather than asserted.
    fn scan_hits(index: &TermIndex, wanted: &[String]) -> usize {
        let mut hits: HashMap<u32, u32> = HashMap::new();
        for want in wanted {
            let mut seen_here: HashSet<u32> = HashSet::new();
            for (id, term) in index.terms.iter().enumerate() {
                if !term.contains(want.as_str()) {
                    continue;
                }
                for doc in &index.postings[id] {
                    seen_here.insert(*doc);
                }
            }
            for doc in seen_here {
                *hits.entry(doc).or_default() += 1;
            }
        }
        hits.len()
    }

    #[test]
    fn the_index_and_the_scan_agree_on_what_matches() {
        // The speedup is only worth anything if it is the same answer. Every
        // wanted term, against a catalogue big enough to have a real
        // dictionary, must select exactly the documents the scan selects.
        let offers = synthetic(4_000, 60);
        let index = TermIndex::build(offers.iter().map(offer_text));
        for term in vocabulary(17) {
            let wanted = wanted_terms(&[term.clone()]);
            let fast: HashSet<u32> = index
                .hits(&wanted)
                .into_iter()
                .map(|(doc, _)| doc)
                .collect();
            assert_eq!(
                fast.len(),
                scan_hits(&index, &wanted),
                "the trigram index and the dictionary scan disagree on {term:?}"
            );
        }
    }

    #[test]
    #[ignore = "a measurement, not an assertion; run with --ignored --nocapture"]
    fn bench_matching() {
        use std::time::Instant;

        let offers = synthetic(26_000, 900);
        let built = Instant::now();
        let catalogue = catalogue(offers);
        let build_ms = built.elapsed().as_millis();
        let wanted = wanted_terms(&vocabulary(96));

        let started = Instant::now();
        for _ in 0..20 {
            std::hint::black_box(scan_hits(&catalogue.endpoints, &wanted));
        }
        let scan = started.elapsed() / 20;

        let started = Instant::now();
        for _ in 0..20 {
            std::hint::black_box(catalogue.endpoints.hits(&wanted));
        }
        let indexed = started.elapsed() / 20;

        let started = Instant::now();
        for _ in 0..20 {
            std::hint::black_box(catalogue.matching(&vocabulary(96)));
        }
        let whole = started.elapsed() / 20;

        println!(
            "\n26,000 entries / {} sellers / 96 phrasings\n  crawl-time build   {build_ms} ms (once per refresh, off the request path)\n  dictionary scan    {scan:?}  <- what this replaced\n  trigram lookup     {indexed:?}\n  whole matching()   {whole:?}  (both layers, the deal, and the clone)",
            catalogue.providers(),
        );
    }

    // -- honesty about coverage --------------------------------------------

    #[test]
    fn only_reaching_the_end_counts_as_complete() {
        assert!(Stop::Exhausted.complete());
        assert!(Stop::SinglePage.complete());
        // Every one of these means entries exist that were never searched, and
        // a response that called them complete would be the bug this module
        // was written to remove.
        assert!(!Stop::Budget.complete());
        assert!(!Stop::Capped.complete());
        assert!(!Stop::ReadFailed.complete());
        assert!(!Stop::NoNewEntries.complete());
    }

    #[test]
    fn the_switch_defaults_to_on_and_takes_the_usual_spellings_of_off() {
        std::env::remove_var("SOURCE_INDEX_ENABLED");
        assert!(enabled());
        for off in ["0", "false", "no", "off", "FALSE"] {
            std::env::set_var("SOURCE_INDEX_ENABLED", off);
            assert!(!enabled(), "{off} should turn it off");
        }
        std::env::set_var("SOURCE_INDEX_ENABLED", "true");
        assert!(enabled());
        std::env::remove_var("SOURCE_INDEX_ENABLED");
    }
}
