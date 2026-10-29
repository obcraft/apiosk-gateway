//! The wider x402 ecosystem, swept as one search engine.
//!
//! `/v1/discover` used to reach exactly one external index (the Coinbase
//! Bazaar). An agent asking what can perform a task is asking about the world,
//! so answering from one index is only marginally better than answering from
//! our own shelf. This module sweeps every free x402 index we can reach, under
//! a single wall-clock budget, and merges the result on the resource URL.
//!
//! # Two kinds of source
//!
//! The split is the whole performance story:
//!
//! * [`Fetch::Queried`] — the index only answers a question. One HTTP call per
//!   phrasing, per source, on the agent's critical path. Cached per query.
//! * [`Fetch::Indexed`] — the source can be listed rather than questioned, so
//!   its catalogue is read ONCE, in the background, and filtered locally. No
//!   network on the agent's path at all, which is what makes breadth
//!   affordable.
//!
//!   "Once" used to mean one document, and that was only ever true of the
//!   manifests. The facilitators are paginated, and each row's URL carried a
//!   hand-written page size — 1000 for the Bazaar, 200 for thirdweb, none at
//!   all for PayAI — so the sweep read the first 100 of PayAI's 26,684 and
//!   called it the whole catalogue. [`crate::source_index`] is what fixed
//!   that: a background crawler follows each index's own cursor to its own
//!   declared end and holds the result behind an inverted index, so
//!   [`read_source`] answers from ~49,000 entries instead of ~1,600. Every
//!   response now publishes how deep it actually reached — see
//!   [`coverage_json`] — so an index that could not be read to the end says
//!   so rather than being reported as complete.
//!
//! A parsed job routes its vocabulary by these economics. Each need's best
//! phrasings are rationed into the queried lane round-robin (see
//! [`allocate_queried`]); the indexed lane and the ranking get every need's
//! whole vocabulary, sectors included, because breadth there is free.
//!
//! # What a page of results is a judgement about
//!
//! Ranking answers "is this offer good for the job". A page answers "what can
//! the ecosystem do", and the two come apart the moment one seller publishes
//! forty endpoints about the topic: every one of them ranks well, the
//! truncation keeps the first twenty-five, and an agent asking about the world
//! is handed one catalogue. Both cuts are therefore dealt out across sellers
//! rather than taken off the top of a list — [`crate::source_index`] does it
//! per source, [`spread_providers`] does it on the merged page — and neither
//! ever moves an offer above a better one from its own seller.
//!
//! The queried lane still uses the five-minute cache in [`crate::cache`], and
//! so does the indexed lane until the first crawl lands — a cold start is
//! degraded to the old first-page behaviour, never broken.
//!
//! Measured on the live indexes, "read a web page" went from 8 unique
//! resources (Bazaar alone) to 54 across all seven, in 799ms of wall clock
//! against 4001ms if the sources were asked one after another. The gain is
//! query-dependent: "iban validation" only moved 19 to 23, because the Bazaar
//! already covers that job well.
//!
//! # What is deliberately not swept
//!
//! Paid indexes (x402scan at $0.01–0.02 a call, x402.direct's paid search,
//! x402list.fun's paid MCP). Sweeping those on every discover call would spend
//! real money per agent request, on results that are unverified anyway. They
//! stay reachable through the MCP's source registry, where the caller chooses
//! to pay.
//!
//! Everything this returns is unreviewed; payment eligibility is assessed
//! separately from contract validation and live provider pricing — see
//! [`crate::bazaar::ExternalOffer`] and the `external_candidates` block.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use serde_json::{json, Value};

use crate::{
    bazaar::{quality_gate, ExternalOffer},
    AppState,
};

/// Wall-clock budget for the entire sweep, however many sources it touches.
///
/// Sources run concurrently, so this is what the agent waits, not the sum. It
/// is deliberately close to the old single-source budget: breadth must come
/// from parallelism and caching, never from making the caller wait longer.
pub(crate) const SWEEP_BUDGET: Duration = Duration::from_millis(3_500);

/// Per-request timeout for one upstream call.
pub(crate) const SOURCE_TIMEOUT: Duration = Duration::from_millis(2_500);

/// How many phrasings go to each queried source for a single-need job.
///
/// Capped below the four the Bazaar uses: variants multiply by sources, and the
/// marginal phrasing buys less than the marginal source does. A multi-need
/// parse may lift the total to one slot per need, never past
/// [`MAX_QUERIED_SLOTS`] — see [`allocate_queried`].
const MAX_QUERY_VARIANTS: usize = 3;

/// Ceiling on queried phrasings however many needs a job parsed into. Matches
/// the parser's own cap of four needs per job: every need can buy one slot.
const MAX_QUERIED_SLOTS: usize = 4;

/// Bound on the indexed lane's vocabulary. A bound, not a budget: matching a
/// cached catalogue is local work, so this exists only to keep a hostile or
/// degenerate vocabulary from ballooning the filter, never to ration a real
/// one.
///
/// Forty-eight was rationing a real one. A four-need job carries, per need,
/// the parser's keywords plus the enrichment's tags, extra terms, categories
/// and sectors — comfortably past a dozen labels each — and `index_union`
/// truncates in need order, so the LAST need of a four-need job was reaching
/// the indexes on nothing at all. The ceiling could not simply be raised
/// while the filter scanned a whole dictionary per term and the ranking
/// re-derived the vocabulary per comparison; with the trigram index
/// (`crate::source_index`) and [`Scorer`] it can.
const MAX_INDEX_TERMS: usize = 96;

/// Offers one seller contributes to the merged page before the next seller
/// gets a turn. See [`spread_providers`].
const PER_PROVIDER_RUN: usize = 2;

/// How many offers survive into the response when the caller does not say.
pub const MAX_RETURNED: usize = 25;

/// The most a caller may ask for (`/v1/discover?limit=`). A sweep of seven
/// indexes rarely yields more unique matches than this, and a page of a
/// hundred unreviewed offers is already past what anyone reads.
pub const MAX_LIMIT: usize = 100;

/// USDC has six decimals; several indexes quote atomic units.
const USDC_DECIMALS: f64 = 1_000_000.0;

/// How the source hands over its data.
#[derive(Clone)]
pub enum Fetch {
    /// `{query}` is replaced with the url-encoded phrasing.
    Queried { template: String },
    /// The source can be listed rather than questioned. One document for a
    /// manifest; a first page plus a cursor for a facilitator, which
    /// [`crate::source_index`] follows to the end in the background.
    Indexed { url: String },
}

/// The response dialect a source speaks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    /// `{items|resources: [{resource, description, accepts: [...]}]}`.
    ///
    /// The x402 discovery dialect. Shared verbatim by the Coinbase Bazaar,
    /// thirdweb and the PayAI facilitator, so one parser covers three sources.
    X402Items,
    /// `{services: [{name, description, domain, endpoints: [{url, description}]}]}`.
    AgenticMarket,
    /// `{baseUrl, services: [{name, description, price, endpoint, method, category}]}`.
    X402Engine,
    /// `{base_url, routes: [{path, method, price_usd, category, description}]}`.
    AnchorManifest,
    /// `{services: [{resourceUrl, description, category, provider, network, priceUsd}]}`.
    X402Direct,
    /// `{endpoints|resources|routes: [{path|url, method, description, price}]}`.
    ///
    /// The dialect a provider writes when it publishes its OWN catalogue rather
    /// than registering with a facilitator, and by some distance the most
    /// common one in the wild: of 24 live Base providers surveyed for migration
    /// 092, seventeen speak it and none of the five older dialects could read
    /// a single offer out of any of them.
    ///
    /// It differs from [`Shape::X402Items`] in exactly two ways, both fatal to
    /// that parser: the entry list may be called `endpoints` or `routes`, and
    /// the address is under `url` or a base-relative `path` rather than
    /// `resource`. afriref publishes 1,041 endpoints this way and x402-items
    /// yielded none of them, because every entry says `url`.
    ///
    /// The base for a relative `path` is the manifest's own origin unless the
    /// document names one — which is exactly right for a well-known document,
    /// and the reason this dialect needs no per-source configuration.
    X402Manifest,
    /// A live 402 challenge, read as the offer it is:
    /// `{x402Version, resource: {url, description}, accepts: [...], extensions}`.
    ///
    /// Not an index — one endpoint describing itself. Most x402 providers
    /// publish no catalogue at all; the challenge they answer with IS their
    /// documentation, and it is richer than most index entries (it carries the
    /// bazaar input/output JSON Schema). Reading it is the only way those
    /// providers are discoverable without asking each one to publish a manifest
    /// they have no reason to write.
    ///
    /// The document arrives with HTTP 402, and sometimes only in the
    /// `payment-required` response header — see [`challenge_document`].
    X402Challenge,
}

impl Shape {
    /// Every dialect the sweep can read, in the order the probe reports them.
    pub const ALL: &'static [Shape] = &[
        Shape::X402Items,
        Shape::AgenticMarket,
        Shape::X402Engine,
        Shape::AnchorManifest,
        Shape::X402Direct,
        Shape::X402Manifest,
        Shape::X402Challenge,
    ];

    /// The database spelling, as constrained by `discovery_sources.shape`.
    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "x402-items" => Some(Self::X402Items),
            "agentic-market" => Some(Self::AgenticMarket),
            "x402engine" => Some(Self::X402Engine),
            "anchor-manifest" => Some(Self::AnchorManifest),
            "x402-direct" => Some(Self::X402Direct),
            "x402-manifest" => Some(Self::X402Manifest),
            "x402-challenge" => Some(Self::X402Challenge),
            _ => None,
        }
    }

    /// The inverse of [`Shape::from_db`], so the probe can name the dialect it
    /// found without a second table of spellings to keep in step.
    pub fn db_name(self) -> &'static str {
        match self {
            Self::X402Items => "x402-items",
            Self::AgenticMarket => "agentic-market",
            Self::X402Engine => "x402engine",
            Self::AnchorManifest => "anchor-manifest",
            Self::X402Direct => "x402-direct",
            Self::X402Manifest => "x402-manifest",
            Self::X402Challenge => "x402-challenge",
        }
    }

    /// True for a dialect whose document is itself a payment challenge, so
    /// HTTP 402 is the SUCCESSFUL answer rather than a failed read.
    ///
    /// Scoped to the shape rather than made a column: "this source speaks
    /// challenge" and "402 is not an error here" are the same fact, and two
    /// switches for one fact drift apart.
    pub fn expects_challenge(self) -> bool {
        matches!(self, Self::X402Challenge)
    }
}

#[derive(Clone)]
pub struct Source {
    pub id: String,
    pub fetch: Fetch,
    pub shape: Shape,
    /// The admin's one-line description of this index. Carried because it is
    /// the only human sentence about a source anywhere, and `/v1/sources`
    /// publishes it — an explore page that invented its own descriptions would
    /// drift from the list the moment a source changed.
    pub note: Option<String>,
    /// HTTP verb for the read. Uppercase, and `GET` unless the row says
    /// otherwise — several x402 providers only answer their challenge to a
    /// POST, and a GET to those returns 404 or 405 forever.
    pub method: String,
    /// Request body sent with a non-GET read, verbatim. Usually the smallest
    /// thing that gets past input validation to the 402, e.g. `{}`.
    pub body: Option<String>,
    /// The provider's display name, logo, category and tags. Carried onto
    /// every offer this source yields — see [`crate::bazaar::ExternalOffer`]
    /// — because a hostname is not an identity and is not a search term.
    pub provider: Option<String>,
    pub logo_url: Option<String>,
    pub category: Option<String>,
    pub tags: Vec<String>,
}

impl Source {
    /// A source with no provider identity and a plain GET read — every
    /// built-in, and the floor a database row is built up from.
    pub(crate) fn plain(id: String, fetch: Fetch, shape: Shape, note: Option<String>) -> Self {
        Self {
            id,
            fetch,
            shape,
            note,
            method: "GET".to_string(),
            body: None,
            provider: None,
            logo_url: None,
            category: None,
            tags: Vec::new(),
        }
    }

    /// What the parsers are told about this source.
    pub(crate) fn ctx<'a>(&'a self, url: &'a str) -> ParseCtx<'a> {
        ParseCtx {
            id: &self.id,
            url,
            provider: self.provider.as_deref(),
            provider_logo: self.logo_url.as_deref(),
            category: self.category.as_deref(),
            tags: &self.tags,
        }
    }
}

/// There is no built-in sweep.
///
/// There used to be: seven indexes hard-coded here, used whenever the
/// `discovery_sources` table was empty, missing or briefly unreadable, on the
/// reasoning that a database problem should narrow discovery rather than
/// switch it off.
///
/// The reasoning was wrong about what the list IS. Every source a search
/// reaches is a commercial relationship someone at Apiosk decided to have —
/// it is swept on Apiosk's behalf, its offers are settled from Apiosk's
/// treasury, and its name appears next to Apiosk's. A fallback made that set
/// unfalsifiable: an admin could remove a source, see it disappear from the
/// portal, and have the gateway keep sweeping it the moment a query timed
/// out. What discovery reaches must be exactly what someone put there —
/// an onboarded provider in the catalogue, or a row an admin added to
/// `discovery_sources` — and nothing else, ever.
///
/// Losing the table therefore loses the external sweep, and that is the
/// intended behaviour rather than a regression: the Apiosk catalogue is
/// always searched and is not a row in this table, so discovery still
/// answers. `active_sources` logs the loss at error level so an empty sweep
/// is never silent.
///
/// The original seven live in migration 078's seed, where they can be read,
/// disabled and deleted like any other row.
/// One `discovery_sources` row in the database spelling.
///
/// Everything past `note` was added by migration 085 and is optional here on
/// purpose: the gateway must keep sweeping against a database that has not had
/// that migration applied yet, so the reader falls back to the original five
/// columns and every row simply arrives without an identity.
#[derive(Debug, Clone, Default, sqlx::FromRow)]
pub struct SourceRow {
    pub id: String,
    pub fetch_kind: String,
    pub url: String,
    pub shape: String,
    pub note: Option<String>,
    pub method: Option<String>,
    pub request_body: Option<String>,
    pub name: Option<String>,
    pub logo_url: Option<String>,
    pub category: Option<String>,
    pub tags: Option<Vec<String>>,
}

/// The sweep this gateway currently runs: the enabled rows of the
/// `discovery_sources` table, and nothing else.
///
/// Cached for [`crate::cache::DISCOVERY_SOURCES_TTL`] (thirty seconds), on its
/// own rather than in the five-minute payload cache: a source an admin just
/// disabled drops out of the next sweep within that window, and one just
/// enabled is swept from then on, while the documents the sources answered
/// with keep their longer life. An unreadable table is never cached, so it is
/// asked again on the next request rather than being remembered as empty.
/// What the admin said about external sellers — the curated rows of
/// `source_providers` (migration 090), keyed source → registrable domain.
///
/// Cached for the same thirty seconds as the source list, for the same
/// reason: a seller an admin just hid drops out of the next sweep before
/// they have finished checking. An unreadable table (migration not applied,
/// database briefly away) sweeps uncurated THIS time and is asked again next
/// time — an outage is never cached as "nothing is curated".
pub async fn provider_curation(state: &Arc<AppState>) -> std::sync::Arc<crate::db::CurationMap> {
    if let Some(cached) = state.cache.get_source_provider_curation().await {
        return cached;
    }
    match state.db.source_provider_curation().await {
        Ok(map) => {
            let map = std::sync::Arc::new(map);
            state
                .cache
                .put_source_provider_curation(std::sync::Arc::clone(&map))
                .await;
            map
        }
        Err(err) => {
            tracing::debug!("source_providers unreadable; this sweep runs uncurated: {err}");
            std::sync::Arc::new(crate::db::CurationMap::new())
        }
    }
}

pub async fn active_sources(state: &Arc<AppState>) -> Vec<Source> {
    if let Some(cached) = state.cache.get_discovery_sources().await {
        if let Some(sources) = sources_from_json(&cached) {
            return sources;
        }
    }

    let sources = match state.db.discovery_sources().await {
        Ok(rows) => match sources_from_rows(&rows) {
            Some(parsed) => parsed,
            // Every row unusable, or no enabled row at all. Both are states an
            // admin can produce deliberately, and neither is an invitation to
            // sweep something they did not ask for.
            None => {
                tracing::error!(
                    "discovery_sources yielded no usable source; the external sweep is empty                      until a row is added or fixed in the admin portal"
                );
                return Vec::new();
            }
        },
        Err(err) => {
            // Table missing (migration not applied) or unreadable. The
            // catalogue is still searched; the sweep is not guessed at.
            tracing::error!(
                "discovery_sources unreadable, so nothing external is swept this request: {}",
                err
            );
            return Vec::new();
        }
    };

    state
        .cache
        .put_discovery_sources(sources_to_json(&sources))
        .await;
    sources
}

/// The enabled rows as sources, or `None` when the table yields nothing the
/// sweep can use — empty, or every row misconfigured. A row with an unknown
/// shape or fetch kind is skipped and logged rather than taking the whole list
/// down with it.
fn sources_from_rows(rows: &[SourceRow]) -> Option<Vec<Source>> {
    let parsed: Vec<Source> = rows
        .iter()
        .filter_map(|row| {
            let Some(shape) = Shape::from_db(&row.shape) else {
                tracing::warn!(
                    "discovery source {} has unknown shape {}",
                    row.id,
                    row.shape
                );
                return None;
            };
            let fetch = match row.fetch_kind.as_str() {
                "queried" => Fetch::Queried {
                    template: row.url.clone(),
                },
                "indexed" => Fetch::Indexed {
                    url: row.url.clone(),
                },
                other => {
                    tracing::warn!("discovery source {} has unknown kind {}", row.id, other);
                    return None;
                }
            };
            let mut source = Source::plain(row.id.clone(), fetch, shape, row.note.clone());
            source.method = normalise_method(row.method.as_deref());
            source.body = row.request_body.clone();
            source.provider = row.name.clone();
            source.logo_url = row.logo_url.clone();
            source.category = row.category.clone();
            source.tags = row.tags.clone().unwrap_or_default();
            Some(source)
        })
        .collect();
    if parsed.is_empty() {
        None
    } else {
        Some(parsed)
    }
}

fn sources_to_json(sources: &[Source]) -> Value {
    Value::Array(
        sources
            .iter()
            .map(|s| {
                let (kind, url) = match &s.fetch {
                    Fetch::Queried { template } => ("queried", template.clone()),
                    Fetch::Indexed { url } => ("indexed", url.clone()),
                };
                serde_json::json!({
                    "id": s.id,
                    "kind": kind,
                    "url": url,
                    "shape": s.shape.db_name(),
                    "note": s.note,
                    "method": s.method,
                    "body": s.body,
                    "provider": s.provider,
                    "logo_url": s.logo_url,
                    "category": s.category,
                    "tags": s.tags,
                })
            })
            .collect(),
    )
}

fn sources_from_json(value: &Value) -> Option<Vec<Source>> {
    let rows = value.as_array()?;
    let sources: Vec<Source> = rows
        .iter()
        .filter_map(|row| {
            let id = row.get("id")?.as_str()?.to_string();
            let url = row.get("url")?.as_str()?.to_string();
            let shape = Shape::from_db(row.get("shape")?.as_str()?)?;
            let fetch = match row.get("kind")?.as_str()? {
                "queried" => Fetch::Queried { template: url },
                "indexed" => Fetch::Indexed { url },
                _ => return None,
            };
            let note = row.get("note").and_then(Value::as_str).map(str::to_string);
            let text = |key: &str| {
                row.get(key)
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .filter(|v| !v.is_empty())
            };
            let mut source = Source::plain(id, fetch, shape, note);
            source.method = normalise_method(text("method").as_deref());
            source.body = text("body");
            source.provider = text("provider");
            source.logo_url = text("logo_url");
            source.category = text("category");
            source.tags = row
                .get("tags")
                .and_then(Value::as_array)
                .map(|t| {
                    t.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            Some(source)
        })
        .collect();
    if sources.is_empty() {
        None
    } else {
        Some(sources)
    }
}

/// How deep the sweep actually reached into each index it can list whole.
///
/// Published beside the offers because the two numbers that matter are not the
/// same number. "Seven sources were swept" was true while the sweep read the
/// first hundred entries of a twenty-six-thousand-entry index, and nothing in
/// the response could tell those apart. This says, per source, how many
/// entries are actually searchable right now, how many the index says it has,
/// and whether the crawl reached the end.
///
/// Empty before the first crawl finishes — which is itself the honest answer,
/// because at that moment the sweep really is reading first pages.
pub async fn coverage_json(state: &Arc<AppState>) -> Value {
    let sources = state.source_index.coverage_json().await;
    let indexed: u64 = sources.iter().filter_map(|s| s["indexed"].as_u64()).sum();
    let declared: u64 = sources
        .iter()
        .filter_map(|s| s["index_reports_total"].as_u64())
        .sum();
    let whole = !sources.is_empty() && sources.iter().all(|s| s["complete"] == json!(true));
    json!({
        "searchable_entries": indexed,
        "indexes_report_total": declared,
        "every_index_read_to_the_end": whole,
        "per_source": sources,
        "how": "Every index that publishes a listing endpoint is paged to the end by a background crawler and searched from memory, so a query reaches the whole catalogue rather than its first page. `complete: false` on a source means entries exist that this search did not see, and `stopped_because` says which limit stopped it.",
    })
}

/// The indexes this gateway actually sweeps, for the response to state.
///
/// Derived from the live list rather than written out in prose, because a
/// hand-maintained list in a response string is a promise that goes stale the
/// first time someone adds or drops a source — and a discovery layer whose
/// stated reach is wrong is worse than one with less reach.
pub async fn source_ids(state: &Arc<AppState>) -> Vec<String> {
    active_sources(state)
        .await
        .iter()
        .map(|s| s.id.clone())
        .collect()
}

/// How many indexes are swept, and which. Used in `external_candidates`.
pub async fn reach_sentence(state: &Arc<AppState>) -> String {
    reach_sentence_for(&source_ids(state).await)
}

/// [`reach_sentence`] for a list already read, so a response that also
/// publishes the ids themselves resolves the source list once, not twice.
pub fn reach_sentence_for(ids: &[String]) -> String {
    format!(
        "The wider x402 ecosystem: {} free indexes ({}), swept concurrently under one budget and merged on the resource URL. Paid indexes are not swept.",
        ids.len(),
        ids.join(", "),
    )
}

fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}

fn host_of(url: &str) -> Option<String> {
    let host = url.split("://").nth(1)?.split('/').next()?;
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

/// A price from any of the dialects: a number, `"$0.01"`, `"0.01"`, or an
/// atomic-unit string like `"10000"`.
///
/// Atomic detection is the only guess here: a bare integer with no decimal
/// point and a value far above any plausible per-call price is treated as
/// atomic USDC. Getting it wrong costs a mislabelled price on an unverified
/// offer, which the response already tells the agent not to trust blindly.
fn parse_price(raw: Option<&Value>) -> Option<f64> {
    let raw = raw?;
    let value = match raw {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => {
            let cleaned = s.trim().trim_start_matches('$').replace(',', "");
            if cleaned.is_empty() {
                return None;
            }
            let parsed = cleaned.parse::<f64>().ok()?;
            // No decimal point and implausibly large for one call: atomic.
            if !cleaned.contains('.') && parsed >= 1_000.0 {
                parsed / USDC_DECIMALS
            } else {
                parsed
            }
        }
        _ => return None,
    };
    if value.is_finite() && value >= 0.0 {
        Some(value)
    } else {
        None
    }
}

fn str_field(item: &Value, key: &str) -> Option<String> {
    item.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn join_url(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

/// One location group (`{type, properties, required}`) from a list of
/// `(name, field schema, required)` triples. `None` when the list is empty,
/// so an absent group never renders as an empty object.
fn group_schema(fields: Vec<(String, Value, bool)>) -> Option<Value> {
    if fields.is_empty() {
        return None;
    }
    let mut props = serde_json::Map::new();
    let mut required = Vec::new();
    for (name, schema, req) in fields {
        if req {
            required.push(Value::String(name.clone()));
        }
        props.insert(name, schema);
    }
    Some(serde_json::json!({"type": "object", "properties": props, "required": required}))
}

/// Wraps location groups into the bazaar-style input schema the rest of the
/// pipeline reads: `{properties: {pathParams|queryParams|body: {...}}}`.
///
/// Every dialect below normalises INTO this shape rather than being carried
/// verbatim, so a consumer (testbench, future binding) reads one format and
/// never learns which index an offer came from.
fn input_schema_from_groups(groups: Vec<(&str, Option<Value>)>) -> Option<Value> {
    let mut props = serde_json::Map::new();
    for (loc, group) in groups {
        if let Some(g) = group {
            props.insert(loc.to_string(), g);
        }
    }
    if props.is_empty() {
        None
    } else {
        Some(serde_json::json!({"type": "object", "properties": props}))
    }
}

/// True for values not worth copying into a field schema: nulls and empty
/// strings, which several indexes emit for every undocumented field.
fn is_blank(value: &Value) -> bool {
    value.is_null() || value.as_str().is_some_and(str::is_empty)
}

/// The parameter-list dialects: thirdweb's `outputSchema.parameters`
/// (`[{in, name, schema, required, description}]`, OpenAPI-ish) and
/// agentic.market's `endpoints[].parameters`
/// (`[{group, name, type, example, enumValues, required}]`). One converter,
/// because both are flat lists of named parameters with a location field.
fn params_list_schema(params: &Value) -> Option<Value> {
    let list = params.as_array().filter(|l| !l.is_empty())?;
    let mut by_loc: HashMap<&str, Vec<(String, Value, bool)>> = HashMap::new();
    for p in list {
        let Some(name) = p.get("name").and_then(Value::as_str) else {
            continue;
        };
        let loc = match p
            .get("in")
            .or_else(|| p.get("group"))
            .and_then(Value::as_str)
        {
            Some("path") => "pathParams",
            Some("body") => "body",
            _ => "queryParams",
        };
        let mut schema = p
            .get("schema")
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        for key in ["type", "description", "example", "default"] {
            if schema.get(key).is_none() {
                if let Some(v) = p.get(key).filter(|v| !is_blank(v)) {
                    schema[key] = v.clone();
                }
            }
        }
        if schema.get("enum").is_none() {
            if let Some(e) = p
                .get("enumValues")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty())
            {
                schema["enum"] = Value::Array(e.clone());
            }
        }
        let required = p.get("required").and_then(Value::as_bool).unwrap_or(false);
        by_loc
            .entry(loc)
            .or_default()
            .push((name.to_string(), schema, required));
    }
    input_schema_from_groups(vec![
        (
            "pathParams",
            by_loc.remove("pathParams").and_then(group_schema),
        ),
        (
            "queryParams",
            by_loc.remove("queryParams").and_then(group_schema),
        ),
        ("body", by_loc.remove("body").and_then(group_schema)),
    ])
}

/// A flat map of fields (`{name: {type, required, description, example}}`)
/// into one location group. x402engine nests these under `body`/`query`;
/// anchor's `params` is one such map for its GET query parameters.
fn flat_fields_group(fields: &Value) -> Option<Value> {
    let fields = fields.as_object().filter(|f| !f.is_empty())?;
    let list = fields
        .iter()
        .filter_map(|(name, spec)| {
            let spec = spec.as_object()?;
            let mut schema = serde_json::Map::new();
            for key in ["type", "description", "example", "default", "enum"] {
                if let Some(v) = spec.get(key).filter(|v| !is_blank(v)) {
                    schema.insert(key.to_string(), v.clone());
                }
            }
            let required = spec
                .get("required")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Some((name.clone(), Value::Object(schema), required))
        })
        .collect::<Vec<_>>();
    group_schema(list)
}

/// x402engine's `services[].parameters`: `{body|query|path: {name: {...}}}`.
fn flat_groups_schema(params: &Value) -> Option<Value> {
    let obj = params.as_object()?;
    input_schema_from_groups(vec![
        ("pathParams", obj.get("path").and_then(flat_fields_group)),
        ("queryParams", obj.get("query").and_then(flat_fields_group)),
        ("body", obj.get("body").and_then(flat_fields_group)),
    ])
}

/// The HTTP verbs a source may be read with, normalised. Anything unknown is
/// read as GET rather than refused: a typo in one row must not remove that
/// source from the sweep.
fn normalise_method(raw: Option<&str>) -> String {
    const VERBS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
    let upper = raw.unwrap_or("GET").trim().to_ascii_uppercase();
    if VERBS.contains(&upper.as_str()) {
        upper
    } else {
        "GET".to_string()
    }
}

/// What a parser knows about the source a document came from.
///
/// Two of these matter beyond bookkeeping. `url` is what relative resources
/// resolve against — several manifests list `GET /v1/wallets/{address}` and
/// nothing else, so without the source's own URL there is no endpoint to
/// return. The identity fields are stamped onto every offer, which is the only
/// reason a swept offer can say who is selling it.
pub(crate) struct ParseCtx<'a> {
    pub id: &'a str,
    pub url: &'a str,
    pub provider: Option<&'a str>,
    pub provider_logo: Option<&'a str>,
    pub category: Option<&'a str>,
    pub tags: &'a [String],
}

impl<'a> ParseCtx<'a> {
    /// A context with an id and nothing else — the shape the tests and any
    /// identity-less caller use.
    pub(crate) fn bare(id: &'a str) -> Self {
        Self {
            id,
            url: "",
            provider: None,
            provider_logo: None,
            category: None,
            tags: &[],
        }
    }
}

/// `https://api.zerion.io/.well-known/x402` -> `https://api.zerion.io`.
fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .filter(|a| !a.is_empty())?;
    Some(format!("{scheme}://{authority}"))
}

/// Turns whatever a manifest calls a resource into an absolute https URL.
///
/// Accepts an absolute URL unchanged and joins a bare path onto the source's
/// own origin. Returns `None` for anything still not absolute, because an
/// offer an agent cannot call is worse than one that was never listed.
fn absolutise(raw: &str, source_url: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if raw.starts_with("http://") || raw.starts_with("https://") {
        return Some(raw.to_string());
    }
    let origin = origin_of(source_url)?;
    if raw.starts_with('/') {
        Some(format!("{origin}{raw}"))
    } else {
        Some(format!("{origin}/{raw}"))
    }
}

/// One offer from a manifest entry that is a bare STRING rather than an object.
///
/// Five of the seven live x402 manifests we tested publish exactly this and
/// nothing else — either an absolute URL (Messari, Interzoid) or a
/// `"GET /api/v1/options/catalog"` method-and-path pair (Nansen, Zerion,
/// Laevitas). Together that is 335 real endpoints that the object-only parser
/// dropped on the floor, silently, while the source row looked healthy.
///
/// There is no description to be had, so one is derived from the verb and the
/// path. That is honestly thin as prose, which is why the source's own
/// provider name, category and tags are stamped on afterwards and fed to
/// matching — they are what makes these findable.
fn offer_from_bare_resource(raw: &str, ctx: &ParseCtx<'_>) -> Option<ExternalOffer> {
    const VERBS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

    let raw = raw.trim();
    let (method, target) = match raw.split_once(char::is_whitespace) {
        Some((head, tail)) if VERBS.contains(&head.to_ascii_uppercase().as_str()) => {
            (Some(head.to_ascii_uppercase()), tail.trim())
        }
        _ => (None, raw),
    };

    let resource = absolutise(target, ctx.url)?;
    let host = host_of(&resource)?;
    let path = resource
        .split_once("://")
        .and_then(|(_, rest)| rest.split_once('/'))
        .map(|(_, path)| format!("/{path}"))
        .unwrap_or_else(|| "/".to_string());

    Some(ExternalOffer {
        method,
        source: ctx.id.to_string(),
        host,
        description: match (ctx.provider, path.as_str()) {
            (Some(provider), "/") => format!("{provider} x402 endpoint"),
            (Some(provider), path) => format!("{provider} {path}"),
            (None, path) => path.to_string(),
        },
        resource,
        ..Default::default()
    })
}

/// The provider-declared call shape carried on a Bazaar-dialect entry:
/// `extensions.bazaar` (an example under `info`, a JSON Schema under
/// `schema.properties.input`), or the older `accepts[].outputSchema`.
///
/// Shared by the index parser and the challenge parser because it is the same
/// document fragment in both — a 402 challenge carries the identical
/// `extensions.bazaar` block that a Bazaar listing does.
fn declared_call_shape(
    container: &Value,
    accept: Option<&Value>,
) -> (Option<Value>, Option<Value>, Option<Value>) {
    let bazaar = container.get("extensions").and_then(|e| e.get("bazaar"));
    let info = bazaar.and_then(|b| b.get("info"));
    let legacy = accept.and_then(|a| a.get("outputSchema"));
    let input = info
        .and_then(|i| i.get("input"))
        .or_else(|| legacy.and_then(|s| s.get("input")))
        .filter(|v| v.is_object())
        .cloned();
    let input_schema = bazaar
        .and_then(|b| b.get("schema"))
        .and_then(|s| s.get("properties"))
        .and_then(|p| p.get("input"))
        .filter(|v| v.is_object())
        .cloned()
        .or_else(|| {
            legacy
                .and_then(|s| s.get("parameters"))
                .and_then(params_list_schema)
        });
    let output = info
        .and_then(|i| i.get("output"))
        .or_else(|| legacy.and_then(|s| s.get("output")))
        .filter(|v| v.is_object())
        .cloned();
    (input, input_schema, output)
}

/// Where a self-published manifest keeps its entry list, most specific first.
///
/// `resources` is last on purpose: a document using it with `resource` keys is
/// the x402-items dialect, and this dialect only ever sees it because the
/// entries say `url` instead.
const MANIFEST_ENTRY_KEYS: &[&str] = &["endpoints", "routes", "resources", "services", "apis"];

/// The entries of a self-published manifest, from whichever key holds them.
///
/// The FIRST non-empty list wins rather than all of them concatenated: several
/// manifests carry both `endpoints` and `free_endpoints`, and the free list is
/// not an offer anyone pays for.
fn manifest_entries(body: &Value) -> Vec<&Value> {
    for key in MANIFEST_ENTRY_KEYS {
        let entries: Vec<&Value> = body
            .get(key)
            .and_then(Value::as_array)
            .map(|a| a.iter().filter(|e| e.is_object()).collect())
            .unwrap_or_default();
        if !entries.is_empty() {
            return entries;
        }
    }
    Vec::new()
}

/// The base a relative `path` hangs off.
///
/// The document's own word first, then the origin of the manifest URL itself.
/// That fallback is what makes this dialect configuration-free: a well-known
/// document is served from the host its endpoints live on, so the address is
/// already known by the time the document is read.
fn manifest_base(body: &Value, manifest_url: &str) -> Option<String> {
    let declared = ["base_url", "baseUrl", "base", "url"]
        .iter()
        .find_map(|k| str_field(body, k))
        .or_else(|| {
            ["service", "provider", "settlement", "info"]
                .iter()
                .filter_map(|k| body.get(*k))
                .find_map(|node| {
                    ["base_url", "baseUrl", "url", "endpoint"]
                        .iter()
                        .find_map(|k| str_field(node, k))
                })
        })
        .filter(|u| u.starts_with("https://"));
    if declared.is_some() {
        return declared;
    }
    origin_of(manifest_url)
}

/// One entry's absolute resource URL: an address it states, or its path joined
/// onto the manifest's base.
///
/// `base` is already absolute (see [`manifest_base`]), so [`absolutise`] does
/// the joining — the same one every other dialect uses, rather than a second
/// implementation that could round a trailing slash differently.
fn manifest_resource(entry: &Value, base: Option<&str>) -> Option<String> {
    let address = ["url", "resource", "endpoint", "href", "path"]
        .iter()
        .find_map(|key| str_field(entry, key))?;
    absolutise(&address, base.unwrap_or_default())
}

/// The price node, in every spelling these manifests use.
fn manifest_price<'a>(entry: &'a Value, accept: Option<&'a Value>) -> Option<&'a Value> {
    for key in [
        "price_usd",
        "priceUsd",
        "priceUSD",
        "priceUSDC",
        "priceUsdc",
        "price",
        "priceHint",
        "cost",
        "amount",
    ] {
        if let Some(value) = entry.get(key) {
            if !value.is_null() {
                return Some(value);
            }
        }
    }
    accept.and_then(|a| a.get("amount").or_else(|| a.get("maxAmountRequired")))
}

/// The HTTP verb, from `method` or the first entry of a `methods` list.
fn manifest_method(entry: &Value) -> Option<String> {
    str_field(entry, "method")
        .or_else(|| {
            entry
                .get("methods")
                .and_then(Value::as_array)
                .and_then(|m| m.first())
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .map(|m| m.to_ascii_uppercase())
}

/// A settlement network declared once for the whole manifest, used when an
/// entry does not name its own.
fn manifest_network(body: &Value) -> Option<String> {
    ["network", "chain"]
        .iter()
        .find_map(|k| str_field(body, k))
        .or_else(|| {
            body.get("networks")
                .and_then(Value::as_array)
                .and_then(|n| n.first())
                .and_then(|n| {
                    n.as_str()
                        .map(str::to_string)
                        .or_else(|| str_field(n, "id"))
                })
        })
        .or_else(|| {
            ["settlement", "payment", "service"]
                .iter()
                .filter_map(|k| body.get(*k))
                .find_map(|node| ["network", "chain"].iter().find_map(|k| str_field(node, k)))
        })
}

/// Turns one source's raw document into offers, before host filtering.
///
/// The identity-less entry point. Production always has a source to hand and
/// goes through [`parse_in`]; this is what the dialect tests read against.
#[cfg(test)]
fn parse(shape: Shape, source_id: &str, body: &Value) -> Vec<ExternalOffer> {
    parse_in(shape, &ParseCtx::bare(source_id), body)
}

pub(crate) fn parse_in(shape: Shape, ctx: &ParseCtx<'_>, body: &Value) -> Vec<ExternalOffer> {
    let source_id = ctx.id;
    let mut out = Vec::new();

    match shape {
        Shape::X402Items => {
            let items = body
                .get("items")
                .or_else(|| body.get("resources"))
                .and_then(Value::as_array);
            for item in items.into_iter().flatten() {
                // Half the live manifests list resources as plain strings —
                // an absolute URL, or a "GET /path" pair. Reading only objects
                // dropped every one of them.
                if let Some(raw) = item.as_str() {
                    if let Some(offer) = offer_from_bare_resource(raw, ctx) {
                        out.push(offer);
                    }
                    continue;
                }
                let Some(resource) = str_field(item, "resource") else {
                    continue;
                };
                let accept = item
                    .get("accepts")
                    .and_then(Value::as_array)
                    .and_then(|a| a.first());
                let description = str_field(item, "description")
                    .or_else(|| accept.and_then(|a| str_field(a, "description")))
                    .or_else(|| {
                        item.get("metadata")
                            .and_then(|m| str_field(m, "description"))
                    });
                let Some(description) = description else {
                    continue;
                };
                let Some(host) = host_of(&resource) else {
                    continue;
                };
                // Provider-declared call shape — a claim, not a reviewed
                // contract, but it is what tells a callable offer from a
                // guess-and-pay one. Same fragment a 402 challenge carries,
                // so the reader is shared with the challenge parser.
                let (input, input_schema, output) = declared_call_shape(item, accept);
                // Scored HERE and nowhere later: `extensions.bazaar.schema`,
                // the usage block and the tags are all on `item`, and the
                // crawler drops the schemas from the offer the moment it has
                // them (see `crate::source_index`). After that point the
                // evidence for a score no longer exists.
                let doc_score = crate::bazaar::score_listing(item, accept).score;
                out.push(ExternalOffer {
                    doc_score,
                    price_usd: parse_price(
                        accept.and_then(|a| a.get("amount").or_else(|| a.get("maxAmountRequired"))),
                    ),
                    network: accept.and_then(|a| str_field(a, "network")),
                    pay_to: accept.and_then(|a| str_field(a, "payTo")),
                    method: input
                        .as_ref()
                        .and_then(|i| str_field(i, "method"))
                        .or_else(|| item.get("metadata").and_then(|m| str_field(m, "method")))
                        .map(|m| m.to_ascii_uppercase()),
                    input,
                    input_schema,
                    output,
                    source: source_id.to_string(),
                    host,
                    resource,
                    description,
                    ..Default::default()
                });
            }
        }

        Shape::AgenticMarket => {
            for service in body
                .get("services")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let service_desc = str_field(service, "description");
                let networks = service
                    .get("networks")
                    .and_then(Value::as_array)
                    .and_then(|n| n.first())
                    .and_then(Value::as_str)
                    .map(str::to_string);
                for endpoint in service
                    .get("endpoints")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let Some(resource) = str_field(endpoint, "url") else {
                        continue;
                    };
                    let Some(description) = str_field(endpoint, "description")
                        .or_else(|| service_desc.clone())
                        .or_else(|| str_field(service, "name"))
                    else {
                        continue;
                    };
                    let Some(host) = host_of(&resource) else {
                        continue;
                    };
                    out.push(ExternalOffer {
                        price_usd: parse_price(
                            endpoint.get("price").or_else(|| endpoint.get("priceUsd")),
                        ),
                        network: networks.clone(),
                        pay_to: None,
                        method: str_field(endpoint, "method").map(|m| m.to_ascii_uppercase()),
                        input_schema: endpoint.get("parameters").and_then(params_list_schema),
                        source: source_id.to_string(),
                        host,
                        resource,
                        description,
                        ..Default::default()
                    });
                }
            }
        }

        Shape::X402Engine => {
            let base = str_field(body, "baseUrl").unwrap_or_default();
            for service in body
                .get("services")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(endpoint) = str_field(service, "endpoint") else {
                    continue;
                };
                let resource = if endpoint.starts_with("http") {
                    endpoint
                } else if base.is_empty() {
                    continue;
                } else {
                    join_url(&base, &endpoint)
                };
                let Some(description) =
                    str_field(service, "description").or_else(|| str_field(service, "name"))
                else {
                    continue;
                };
                let Some(host) = host_of(&resource) else {
                    continue;
                };
                out.push(ExternalOffer {
                    price_usd: parse_price(service.get("price")),
                    network: None,
                    pay_to: None,
                    method: str_field(service, "method").map(|m| m.to_ascii_uppercase()),
                    input_schema: service.get("parameters").and_then(flat_groups_schema),
                    source: source_id.to_string(),
                    host,
                    resource,
                    description,
                    ..Default::default()
                });
            }
        }

        Shape::AnchorManifest => {
            let base = str_field(body, "base_url").unwrap_or_default();
            if base.is_empty() {
                return out;
            }
            let network = body
                .get("networks")
                .and_then(Value::as_array)
                .and_then(|n| n.first())
                .and_then(|n| str_field(n, "id"));
            for route in body
                .get("routes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(path) = str_field(route, "path") else {
                    continue;
                };
                let Some(description) = str_field(route, "description") else {
                    continue;
                };
                let resource = join_url(&base, &path);
                let Some(host) = host_of(&resource) else {
                    continue;
                };
                // The manifest documents a POST body as a ready JSON Schema
                // (`input_schema`) and GET parameters as a flat field map
                // (`params`); its paths carry no `:placeholders`, so the flat
                // fields are query parameters.
                let input_schema = input_schema_from_groups(vec![
                    (
                        "body",
                        route.get("input_schema").filter(|v| v.is_object()).cloned(),
                    ),
                    (
                        "queryParams",
                        route.get("params").and_then(flat_fields_group),
                    ),
                ]);
                out.push(ExternalOffer {
                    price_usd: parse_price(route.get("price_usd").or_else(|| route.get("price"))),
                    network: network.clone(),
                    pay_to: None,
                    method: str_field(route, "method").map(|m| m.to_ascii_uppercase()),
                    input_schema,
                    source: source_id.to_string(),
                    host,
                    resource,
                    description,
                    ..Default::default()
                });
            }
        }

        Shape::X402Direct => {
            for service in body
                .get("services")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(resource) = str_field(service, "resourceUrl") else {
                    continue;
                };
                let Some(description) = str_field(service, "description") else {
                    continue;
                };
                let Some(host) = host_of(&resource) else {
                    continue;
                };
                out.push(ExternalOffer {
                    price_usd: parse_price(
                        service.get("priceUsd").or_else(|| service.get("price")),
                    ),
                    network: str_field(service, "network"),
                    pay_to: None,
                    method: str_field(service, "method").map(|m| m.to_ascii_uppercase()),
                    source: source_id.to_string(),
                    host,
                    resource,
                    description,
                    ..Default::default()
                });
            }
        }

        Shape::X402Manifest => {
            let base = manifest_base(body, ctx.url);
            for entry in manifest_entries(body) {
                let Some(resource) = manifest_resource(entry, base.as_deref()) else {
                    continue;
                };
                let Some(host) = host_of(&resource) else {
                    continue;
                };
                let accept = entry
                    .get("accepts")
                    .and_then(Value::as_array)
                    .and_then(|a| a.first());
                // A manifest entry that says nothing about itself is still
                // worth publishing: its path is the only description a great
                // many of these carry, and dropping it loses the endpoint
                // entirely rather than losing a sentence.
                let description = str_field(entry, "description")
                    .or_else(|| str_field(entry, "summary"))
                    .or_else(|| str_field(entry, "name"))
                    .or_else(|| accept.and_then(|a| str_field(a, "description")))
                    .or_else(|| offer_from_bare_resource(&resource, ctx).map(|o| o.description))
                    .unwrap_or_else(|| format!("x402 endpoint on {host}"));

                let (input, input_schema, output) = declared_call_shape(entry, accept);
                out.push(ExternalOffer {
                    price_usd: parse_price(manifest_price(entry, accept)),
                    network: str_field(entry, "network")
                        .or_else(|| accept.and_then(|a| str_field(a, "network")))
                        .or_else(|| manifest_network(body)),
                    pay_to: str_field(entry, "payTo")
                        .or_else(|| accept.and_then(|a| str_field(a, "payTo"))),
                    method: manifest_method(entry),
                    input,
                    input_schema: input_schema
                        .or_else(|| entry.get("inputSchema").filter(|v| v.is_object()).cloned())
                        .or_else(|| entry.get("input_schema").filter(|v| v.is_object()).cloned()),
                    output,
                    source: source_id.to_string(),
                    host,
                    resource,
                    description,
                    ..Default::default()
                });
            }
        }

        // A live 402 challenge: one endpoint describing itself, not an index.
        //
        // `resource` is either a string or `{url, description}`. The provider's
        // own url is preferred, but only when it is a usable https address —
        // The Graph answers with an internal hostname
        // (`http://mainnet-thegraph-...eu-west3.thegraph.com/...`) that no
        // agent can reach, so anything that is not https falls back to the URL
        // we actually called, which by definition works.
        Shape::X402Challenge => {
            let resource_node = body.get("resource");
            let declared = resource_node
                .and_then(|r| {
                    r.as_str()
                        .map(str::to_string)
                        .or_else(|| str_field(r, "url"))
                })
                .filter(|u| u.starts_with("https://"));
            let Some(resource) = declared.or_else(|| {
                Some(ctx.url)
                    .filter(|u| u.starts_with("https://"))
                    .map(str::to_string)
            }) else {
                return out;
            };
            let Some(host) = host_of(&resource) else {
                return out;
            };

            // `accepts` is what makes a 402 an offer rather than a wall. Some
            // providers answer the conventional URL with a bare
            // {"error":"Payment required"} and keep the real challenge on their
            // API paths; indexing that stub would publish an endpoint that
            // cannot be quoted, cannot be settled, and does not exist.
            let Some(accept) = body
                .get("accepts")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty())
                .and_then(|a| a.first())
            else {
                return out;
            };
            let accept = Some(accept);
            let (input, input_schema, output) = declared_call_shape(body, accept);

            // The challenge's own words first, then the payment option's, then
            // the path. Never the top-level `error` — "Payment required" is
            // true of every one of these and describes none of them.
            let description = resource_node
                .and_then(|r| str_field(r, "description"))
                .or_else(|| accept.and_then(|a| str_field(a, "description")))
                .or_else(|| {
                    body.get("extensions")
                        .and_then(|e| e.get("bazaar"))
                        .and_then(|b| str_field(b, "description"))
                })
                .or_else(|| offer_from_bare_resource(&resource, ctx).map(|o| o.description))
                .unwrap_or_else(|| format!("x402 endpoint on {host}"));

            out.push(ExternalOffer {
                price_usd: parse_price(
                    accept.and_then(|a| a.get("amount").or_else(|| a.get("maxAmountRequired"))),
                ),
                network: accept.and_then(|a| str_field(a, "network")),
                pay_to: accept.and_then(|a| str_field(a, "payTo")),
                method: input
                    .as_ref()
                    .and_then(|i| str_field(i, "method"))
                    .map(|m| m.to_ascii_uppercase()),
                input,
                input_schema,
                output,
                source: source_id.to_string(),
                host,
                resource,
                description,
                ..Default::default()
            });
        }
    }

    // The source's identity, stamped on every offer it produced. Done once
    // here rather than in six parser arms: a dialect added later gets it for
    // free, and it cannot be forgotten in one branch.
    for offer in &mut out {
        offer.provider = ctx.provider.map(str::to_string);
        offer.provider_logo = ctx.provider_logo.map(str::to_string);
        offer.category = ctx.category.map(str::to_string);
        offer.tags = ctx.tags.to_vec();
    }

    out
}

/// Words worth matching on. Short and generic tokens match everything and so
/// discriminate nothing.
pub(crate) fn tokens(text: &str) -> HashSet<String> {
    const NOISE: &[&str] = &[
        "the", "and", "for", "with", "that", "this", "from", "into", "your", "api", "apis", "get",
        "one", "any", "all", "per", "via", "use", "using",
    ];
    text.to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| t.len() > 2 && !NOISE.contains(t))
        .map(str::to_string)
        .collect()
}

/// Keeps the entries of a whole-catalogue document that plausibly serve the
/// query. Permissive on purpose: a wrongly-dropped supplier is invisible, a
/// wrongly-kept one is merely ignored further down.
pub(crate) fn relevant(offers: Vec<ExternalOffer>, queries: &[String]) -> Vec<ExternalOffer> {
    let wanted: HashSet<String> = queries.iter().flat_map(|q| tokens(q)).collect();
    if wanted.is_empty() {
        return offers;
    }

    offers
        .into_iter()
        .filter(|offer| {
            // The provider's own name, category and tags are part of the
            // haystack. A 402 challenge carries one thin line of text, so
            // without them "crypto prices" would never reach CoinGecko —
            // the words that describe it live on the source row, not in the
            // document the provider serves.
            let haystack = tokens(&format!(
                "{} {} {} {} {}",
                offer.resource,
                offer.description,
                offer.provider.as_deref().unwrap_or(""),
                offer.category.as_deref().unwrap_or(""),
                offer.tags.join(" "),
            ));
            wanted
                .iter()
                .any(|w| haystack.contains(w) || haystack.iter().any(|h| h.contains(w.as_str())))
        })
        .collect()
}

/// Why one upstream read produced no document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FetchFailure {
    /// No answer inside [`SOURCE_TIMEOUT`].
    Timeout,
    /// Connection failure, a non-2xx status, or a body that is not JSON.
    Error,
}

/// One read of one URL: the document, and whether this call fetched it or
/// found it already cached.
enum Fetched {
    Fresh(Value),
    Cached(Value),
}

/// The response header an x402 provider puts its challenge in. Several put the
/// WHOLE challenge there and nothing in the body — The Graph answers a probe
/// with zero bytes — so a body-only reader sees a working provider as silence.
const CHALLENGE_HEADER: &str = "payment-required";

/// A base64 `payment-required` header decoded back into the challenge document.
///
/// Providers differ on padding and alphabet, so all four base64 flavours are
/// tried; a header that is already plain JSON is accepted too, because a few
/// send it that way.
pub(crate) fn decode_challenge(value: &str) -> Option<Value> {
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
    use base64::Engine as _;

    let encoded = value.trim();
    if let Ok(json) = serde_json::from_str::<Value>(encoded) {
        return Some(json);
    }
    let bytes = STANDARD
        .decode(encoded)
        .or_else(|_| STANDARD_NO_PAD.decode(encoded))
        .or_else(|_| URL_SAFE.decode(encoded))
        .or_else(|_| URL_SAFE_NO_PAD.decode(encoded))
        .ok()?;
    serde_json::from_slice::<Value>(&bytes).ok()
}

pub(crate) fn challenge_from_header(headers: &reqwest::header::HeaderMap) -> Option<Value> {
    headers
        .get(CHALLENGE_HEADER)
        .or_else(|| headers.get("x-payment-required"))
        .and_then(|v| v.to_str().ok())
        .and_then(decode_challenge)
}

/// Which of the two places a challenge can arrive in actually carried it.
///
/// A body that parses and names `accepts` wins; otherwise the header; otherwise
/// whatever body there was. For a non-challenge source only the body is ever
/// considered, so no ordinary index changes behaviour.
pub(crate) fn best_document(
    body: Option<Value>,
    header: Option<Value>,
    challenge: bool,
) -> Option<Value> {
    if !challenge {
        return body;
    }
    let body_has_accepts = body
        .as_ref()
        .and_then(|b| b.get("accepts"))
        .is_some_and(|a| a.as_array().is_some_and(|a| !a.is_empty()));
    if body_has_accepts {
        return body;
    }
    header.or(body)
}

fn http_method(verb: &str) -> reqwest::Method {
    reqwest::Method::from_bytes(verb.as_bytes()).unwrap_or(reqwest::Method::GET)
}

/// Fetches one URL, caching the raw document and coalescing concurrent reads.
///
/// The cache layer runs the request once per key at a time — see
/// [`crate::cache::EndpointCache::get_or_fetch_source`] — so identical
/// searches arriving together cost the index one call. A failure reaches every
/// waiter as a failure and caches nothing.
///
/// The reason for a miss is kept rather than collapsed into `None`, because
/// the sweep now reports it: "timed out" and "answered with a 500" send an
/// admin to different places.
async fn fetch(
    state: &Arc<AppState>,
    cache_key: &str,
    url: &str,
    source: &Source,
) -> Result<Fetched, FetchFailure> {
    // `reqwest::Client` is a handle; cloning it shares the pool.
    let http = state.http_client.clone();
    let key = cache_key.to_string();
    let url = url.to_string();
    let verb = source.method.clone();
    let payload = source.body.clone();
    // For a challenge source the 402 IS the document. Scoped to the shape so
    // no ordinary index can start treating a payment wall as a catalogue.
    let challenge = source.shape.expects_challenge();
    let result = state
        .cache
        .get_or_fetch_source(cache_key, async move {
            let mut request = http
                .request(http_method(&verb), &url)
                .header("Accept", "application/json")
                .timeout(SOURCE_TIMEOUT);
            if let Some(body) = payload {
                request = request
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(body);
            }
            let response = match request.send().await {
                Ok(r) if r.status().is_success() => r,
                Ok(r) if challenge && r.status() == reqwest::StatusCode::PAYMENT_REQUIRED => r,
                Ok(r) => {
                    tracing::debug!("source {} returned {}", key, r.status());
                    return Err(FetchFailure::Error);
                }
                Err(err) if err.is_timeout() => {
                    tracing::debug!("source {} timed out", key);
                    return Err(FetchFailure::Timeout);
                }
                Err(err) => {
                    tracing::debug!("source {} failed: {}", key, err);
                    return Err(FetchFailure::Error);
                }
            };
            let from_header = challenge_from_header(response.headers());
            let bytes = response.bytes().await.map_err(|err| {
                tracing::debug!("source {} unreadable: {}", key, err);
                if err.is_timeout() {
                    FetchFailure::Timeout
                } else {
                    FetchFailure::Error
                }
            })?;
            let from_body = serde_json::from_slice::<Value>(&bytes).ok();
            best_document(from_body, from_header, challenge).ok_or_else(|| {
                tracing::debug!("source {} returned no readable document", key);
                FetchFailure::Error
            })
        })
        .await;
    match result {
        Ok((body, true)) => Ok(Fetched::Fresh(body)),
        Ok((body, false)) => Ok(Fetched::Cached(body)),
        Err(failure) => Err(*failure),
    }
}

/// External offers as JSON, each carrying the gateway's indicative payment
/// eligibility and estimates — see
/// [`crate::proxy::external_fetch::quote_external_offers`].
///
/// Every surface that publishes external offers goes through here, so no screen
/// can say "pay the provider yourself" about an endpoint the gateway would
/// settle after validation. Discovery estimates are not approved prices: the
/// live challenge, input contract and buyer limits still need checking.
pub async fn settled_json(
    state: &Arc<AppState>,
    offers: &[ExternalOffer],
) -> Vec<serde_json::Value> {
    let settlements = crate::proxy::external_fetch::quote_external_offers(state, offers).await;
    offers
        .iter()
        .zip(settlements.iter())
        .map(|(offer, settlement)| offer.to_json_settled(settlement))
        .collect()
}

/// How one source fared in one sweep, for the response to state.
///
/// A sweep used to drop a failing or slow source silently, so "this index
/// answered nothing" and "this index was never heard from" looked the same to
/// the caller. Every enabled source now gets a line, including the ones that
/// contributed nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceStatus {
    pub id: String,
    pub status: SourceOutcome,
    /// Unique offers this source put into the merged list — after host policy,
    /// the toy filter and cross-source dedupe, so the numbers add up to the
    /// list. A source can be `Ok` with zero here when everything it returned
    /// another source had already listed.
    pub offers: usize,
    /// Wall clock of the slowest read for this source.
    pub ms: u64,
}

/// The one-word verdict on a source in one sweep.
///
/// A queried source is asked once per phrasing, so one source is several
/// reads. They collapse in this order: answered with offers is `Ok` (or
/// `Cached` when none of those answers cost a request); otherwise a read that
/// never came back is `Timeout`; otherwise a read that failed is `Error`;
/// otherwise every read came back with nothing, `Empty`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceOutcome {
    Ok,
    Empty,
    Timeout,
    Error,
    Cached,
    /// Answered out of the fully-crawled catalogue. `meta.source_coverage`
    /// says how deep that catalogue goes and whether it is whole.
    Indexed,
}

impl SourceOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Empty => "empty",
            Self::Timeout => "timeout",
            Self::Error => "error",
            Self::Cached => "cached",
            Self::Indexed => "indexed",
        }
    }

    /// Did the source fail to take part, as opposed to taking part and
    /// finding nothing?
    pub fn failed(self) -> bool {
        matches!(self, Self::Timeout | Self::Error)
    }
}

impl SourceStatus {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "status": self.status.as_str(),
            "offers": self.offers,
            "ms": self.ms,
        })
    }
}

/// What one read of one source came back with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadOutcome {
    /// A document was read from the network.
    Fresh,
    /// A document was read from the cache.
    Cached,
    /// The source's WHOLE catalogue was read out of `crate::source_index`,
    /// which the background crawler had already paged to the end. Distinct
    /// from `Cached` because it is a different claim: `Cached` means the first
    /// page again without paying for it, this means all of it.
    Indexed,
    Timeout,
    Error,
}

/// The result of one read task: the offers the source's document yielded for
/// the query (already relevance-filtered for an indexed source), how the
/// document was obtained, and how long that took.
///
/// This is the seam between the network half of the sweep and the pure half:
/// [`merge_sweep`] takes a list of these and needs no network at all, which
/// is what lets the merge, the ranking and the status reporting be tested
/// against canned sources.
#[derive(Debug, Clone)]
struct SourceRead {
    source_id: String,
    outcome: ReadOutcome,
    offers: Vec<ExternalOffer>,
    ms: u64,
}

/// A finished sweep: the merged, ranked offers and a status line per source.
#[derive(Debug, Clone, Default)]
pub struct Sweep {
    pub offers: Vec<ExternalOffer>,
    /// One entry per enabled source, in the order the sources were listed.
    pub sources: Vec<SourceStatus>,
    /// Which parsed needs the merged list answers, when the sweep ran on two
    /// or more named needs. Empty otherwise: coverage of one need is the
    /// offer list itself.
    pub need_coverage: Vec<NeedCoverage>,
}

impl Sweep {
    /// Sources that timed out or errored — the number a response puts next to
    /// the list so a caller can see at a glance whether the reach was whole.
    pub fn sources_failed(&self) -> usize {
        self.sources.iter().filter(|s| s.status.failed()).count()
    }

    /// The status lines as JSON, for `meta.sources` and its siblings.
    pub fn sources_json(&self) -> Vec<Value> {
        self.sources.iter().map(SourceStatus::to_json).collect()
    }

    /// Per-need offer counts as JSON, for the responses that publish them.
    pub fn need_coverage_json(&self) -> Vec<Value> {
        self.need_coverage
            .iter()
            .map(|c| json!({ "need": c.need, "offers": c.offers }))
            .collect()
    }
}

/// Sweeps every free external index for offers that could serve these queries.
///
/// The long-standing entry point: the merged offers alone, truncated to
/// [`MAX_RETURNED`]. Callers that want to say which sources answered use
/// [`sweep_with_status`]; both run the same sweep.
pub async fn sweep(
    state: &Arc<AppState>,
    queries: &[String],
    known_hosts: &[String],
) -> Vec<ExternalOffer> {
    sweep_with_status(state, queries, known_hosts, MAX_RETURNED)
        .await
        .offers
}

/// [`sweep_needs_with_status`] for a flat query list: one unnamed vocabulary,
/// so the first [`MAX_QUERY_VARIANTS`] phrasings reach the queried sources in
/// the order given — the contract every pre-parser caller already relies on.
/// The indexed lane and the ranking see the whole list; those cost nothing.
pub async fn sweep_with_status(
    state: &Arc<AppState>,
    queries: &[String],
    known_hosts: &[String],
    limit: usize,
) -> Sweep {
    sweep_needs_with_status(state, &[NeedTerms::flat(queries)], known_hosts, limit).await
}

/// [`sweep_needs_with_status`] returning the merged offers alone.
pub async fn sweep_needs(
    state: &Arc<AppState>,
    needs: &[NeedTerms],
    known_hosts: &[String],
) -> Vec<ExternalOffer> {
    sweep_needs_with_status(state, needs, known_hosts, MAX_RETURNED)
        .await
        .offers
}

/// Sweeps every enabled index on a parsed job's vocabulary and reports per
/// source how it went.
///
/// The vocabulary arrives per need and is routed by lane economics: a queried
/// source costs one HTTP call per phrasing, so slots are allocated round-robin
/// across the needs ([`allocate_queried`]); an indexed source is a cached
/// document filtered locally, so it — and the ranking — gets every need's
/// whole vocabulary ([`index_union`]). The result carries per-need coverage
/// when the job had several needs, so a half-answered job never reads as a
/// full answer.
///
/// Sources run concurrently under one budget. Whatever arrives inside it is
/// kept; a source still in flight when the clock runs out costs its own results
/// and nothing else, and is reported as a timeout rather than forgotten. A
/// source whose task fails is reported as an error, likewise without touching
/// the rest. Duplicates collapse on the resource URL, so the same endpoint
/// listed by four indexes is one offer.
///
/// `limit` caps the merged list. [`MAX_RETURNED`] is the default; the literal
/// discovery path lets a caller raise it to [`MAX_LIMIT`].
pub async fn sweep_needs_with_status(
    state: &Arc<AppState>,
    needs: &[NeedTerms],
    known_hosts: &[String],
    limit: usize,
) -> Sweep {
    let queried = allocate_queried(needs);
    let index = index_union(needs);
    if queried.is_empty() {
        return Sweep::default();
    }

    let sources = active_sources(state).await;
    // The admin's word on external sellers, loaded once per sweep (and cached
    // like the source list). It decides two things downstream: a hidden
    // seller's offers never merge, and a curated vocabulary scores and ships
    // like a published one.
    let curation = provider_curation(state).await;
    let order: Vec<String> = sources.iter().map(|s| s.id.clone()).collect();
    // The sources whose seller rows can only be learned, never crawled — see
    // the harvest below the read loop.
    let queried_ids: HashSet<String> = sources
        .iter()
        .filter(|s| !s.is_browsable())
        .map(|s| s.id.clone())
        .collect();
    let mut set = tokio::task::JoinSet::new();
    // Which source each task belongs to, so a task that never finishes — or
    // panics — can still be attributed when the budget runs out.
    let mut owner: HashMap<tokio::task::Id, String> = HashMap::new();

    for source in sources {
        match source.fetch {
            Fetch::Queried { ref template } => {
                for query in &queried {
                    let state = Arc::clone(state);
                    let query = query.clone();
                    let template = template.clone();
                    let owned = source.clone();
                    let handle = set.spawn(async move {
                        let url = template.replace("{query}", &urlencode(&query));
                        let key = format!("{}::{}", owned.id, query);
                        read_source(&state, owned, &key, &url, None).await
                    });
                    owner.insert(handle.id(), source.id.clone());
                }
            }
            Fetch::Indexed { ref url } => {
                let state = Arc::clone(state);
                let index = index.clone();
                let url = url.clone();
                let owned = source.clone();
                let handle = set.spawn(async move {
                    let key = format!("{}::__index", owned.id);
                    read_source(&state, owned, &key, &url, Some(&index)).await
                });
                owner.insert(handle.id(), source.id.clone());
            }
        }
    }

    let started = tokio::time::Instant::now();
    let deadline = started + SWEEP_BUDGET;
    let mut reads: Vec<SourceRead> = Vec::new();

    loop {
        match tokio::time::timeout_at(deadline, set.join_next_with_id()).await {
            Ok(Some(Ok((task_id, read)))) => {
                owner.remove(&task_id);
                reads.push(read);
            }
            Ok(Some(Err(err))) => {
                tracing::debug!("source task failed: {}", err);
                if let Some(source_id) = owner.remove(&err.id()) {
                    reads.push(SourceRead {
                        source_id,
                        outcome: ReadOutcome::Error,
                        offers: Vec::new(),
                        ms: started.elapsed().as_millis() as u64,
                    });
                }
            }
            Ok(None) => break,
            Err(_) => {
                tracing::debug!("sweep hit its budget with {} tasks left", set.len());
                set.abort_all();
                let ms = started.elapsed().as_millis() as u64;
                for source_id in owner.drain().map(|(_, id)| id) {
                    reads.push(SourceRead {
                        source_id,
                        outcome: ReadOutcome::Timeout,
                        offers: Vec::new(),
                        ms,
                    });
                }
                break;
            }
        }
    }

    // Learn from what the queried lanes just answered. A queried index has no
    // whole catalogue for the crawler to snapshot, so its `source_providers`
    // rows ACCRETE from live answers instead: every fresh read widens what is
    // known about the sellers it mentioned (see `accrete_source_providers` —
    // floors and unions, never overwrites). Spawned, because a bookkeeping
    // write must not add a millisecond to a search; fresh reads only, so a
    // cached answer is not re-learned every request for five minutes.
    let mut harvest: HashMap<String, Vec<ExternalOffer>> = HashMap::new();
    for read in &reads {
        if read.outcome == ReadOutcome::Fresh && queried_ids.contains(&read.source_id) {
            harvest
                .entry(read.source_id.clone())
                .or_default()
                .extend(read.offers.iter().cloned());
        }
    }
    if !harvest.is_empty() {
        let state = Arc::clone(state);
        tokio::spawn(async move {
            for (source_id, offers) in harvest {
                let rows = crate::source_index::snapshot_offers(&offers);
                if rows.is_empty() {
                    continue;
                }
                if let Err(err) = state.db.accrete_source_providers(&source_id, &rows).await {
                    tracing::debug!(source = %source_id, "provider accretion skipped: {err}");
                }
            }
        });
    }

    let mut swept = merge_sweep(reads, &order, &index, known_hosts, limit, &curation);
    swept.need_coverage = coverage_for(&swept.offers, needs);
    tracing::debug!(
        "sweep merged {} offers; {} of {} sources failed",
        swept.offers.len(),
        swept.sources_failed(),
        swept.sources.len()
    );
    swept
}

/// One parsed need's search vocabulary, routed by lane.
///
/// The two lanes have opposite economics, so they get different shares of the
/// same vocabulary: `queried` phrasings each cost one HTTP call per queried
/// source and are rationed by [`allocate_queried`]; `index` labels cost
/// nothing — a cached catalogue document is filtered locally — so every label
/// the enrichment produced is worth carrying, sectors included.
#[derive(Debug, Clone, Default)]
pub struct NeedTerms {
    /// The need these terms serve, in the parser's words. Empty for the tail
    /// vocabulary that belongs to the whole job rather than to one need (the
    /// raw sentence, capability-derived phrasings) — and for flat sweeps,
    /// which have no parse to attribute terms to.
    pub need: String,
    /// Phrasings worth an HTTP call, best first.
    pub queried: Vec<String>,
    /// Additional matching vocabulary for the indexed lane and the ranking:
    /// sectors, categories, the need clause itself.
    pub index: Vec<String>,
}

impl NeedTerms {
    /// A flat query list as one unnamed vocabulary — what every caller that
    /// predates the parser sends.
    pub fn flat(queries: &[String]) -> Self {
        Self {
            need: String::new(),
            queried: queries.to_vec(),
            index: Vec::new(),
        }
    }

    /// Everything this need matches on: both lanes' vocabulary, deduplicated.
    fn matching_terms(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for term in self.queried.iter().chain(&self.index) {
            push_unique(term, &mut out, &mut seen);
        }
        out
    }
}

/// How many merged offers plausibly serve one parsed need — the number that
/// says whether a multi-need job came back whole or half-answered. As loose
/// as [`relevant`], and for the same reason: it reports reach, it does not
/// certify fit.
#[derive(Debug, Clone)]
pub struct NeedCoverage {
    pub need: String,
    pub offers: usize,
}

/// One term into the list, unless an equivalent is already there.
fn push_unique(term: &str, out: &mut Vec<String>, seen: &mut HashSet<String>) -> bool {
    let trimmed = term.trim();
    if trimmed.is_empty() {
        return false;
    }
    if seen.insert(trimmed.to_ascii_lowercase()) {
        out.push(trimmed.to_string());
        return true;
    }
    false
}

/// The phrasings that actually cost HTTP calls, allocated round-robin across
/// the named needs.
///
/// A flat take of the first [`MAX_QUERY_VARIANTS`] let the first need's
/// keywords spend every slot: a two-need job reached the queried indexes as
/// one need and got half an answer back. Round-robin gives every need its
/// best phrasing a slot before any need gets a second — the same rule the
/// catalogue's `interleave` applies to candidates. The tail vocabulary (the
/// raw sentence, capability-derived phrasings) fills whatever is left: those
/// words match keyword indexes worst and must never starve a need.
fn allocate_queried(needs: &[NeedTerms]) -> Vec<String> {
    let named: Vec<&NeedTerms> = needs.iter().filter(|n| !n.need.trim().is_empty()).collect();
    let cap = if named.is_empty() {
        MAX_QUERY_VARIANTS
    } else {
        MAX_QUERY_VARIANTS.max(named.len().min(MAX_QUERIED_SLOTS))
    };

    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut cursors = vec![0usize; named.len()];
    'rounds: loop {
        let mut advanced = false;
        for (i, need) in named.iter().enumerate() {
            while cursors[i] < need.queried.len() {
                let term = &need.queried[cursors[i]];
                cursors[i] += 1;
                if push_unique(term, &mut out, &mut seen) {
                    advanced = true;
                    break;
                }
            }
            if out.len() >= cap {
                break 'rounds;
            }
        }
        if !advanced {
            break;
        }
    }
    for need in needs.iter().filter(|n| n.need.trim().is_empty()) {
        for term in &need.queried {
            if out.len() >= cap {
                return out;
            }
            push_unique(term, &mut out, &mut seen);
        }
    }
    // A vocabulary with nothing marked worth an HTTP call still sweeps on
    // whatever it does carry, so no caller can assemble a NeedTerms that
    // silently skips the queried half of the world.
    if out.is_empty() {
        for need in needs {
            for term in &need.index {
                if out.len() >= MAX_QUERY_VARIANTS {
                    return out;
                }
                push_unique(term, &mut out, &mut seen);
            }
        }
    }
    out
}

/// Everything the indexed lane matches on and the merged list is ranked by:
/// both lanes' vocabulary from every need, deduplicated. Breadth is free here
/// — the documents are cached and the filter is local — which is exactly why
/// the sectors and categories the queried lane rations arrive whole.
fn index_union(needs: &[NeedTerms]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for need in needs {
        for term in need.queried.iter().chain(&need.index) {
            push_unique(term, &mut out, &mut seen);
        }
    }
    out.truncate(MAX_INDEX_TERMS);
    out
}

/// Which needs the merged list actually answers. Only said when there are two
/// or more named needs: coverage of one need is the offer list itself.
fn coverage_for(offers: &[ExternalOffer], needs: &[NeedTerms]) -> Vec<NeedCoverage> {
    let named: Vec<&NeedTerms> = needs.iter().filter(|n| !n.need.trim().is_empty()).collect();
    if named.len() < 2 {
        return Vec::new();
    }
    named
        .iter()
        .map(|need| {
            let scorer = Scorer::new(&need.matching_terms());
            NeedCoverage {
                need: need.need.clone(),
                offers: offers
                    .iter()
                    .filter(|offer| scorer.score(offer) > 0)
                    .count(),
            }
        })
        .collect()
}

/// One read of one source, timed. An indexed source's whole catalogue is
/// narrowed to the query here, locally, because the document is already in
/// hand and narrowing it costs no network at all.
async fn read_source(
    state: &Arc<AppState>,
    source: Source,
    cache_key: &str,
    url: &str,
    filter_to: Option<&[String]>,
) -> SourceRead {
    let started = tokio::time::Instant::now();
    let source_id = source.id.clone();

    // The whole catalogue, if the background crawler has been round yet.
    //
    // This is the difference between searching an index and searching its
    // first page: PayAI's row yields 100 entries here and 26,684 there. No
    // network, no document cache, and the filter is a postings lookup rather
    // than 26,684 tokenisations — see `crate::source_index`.
    //
    // Only for a source whose whole document is fetchable at all. A queried
    // source has no catalogue to crawl, so it always goes to the network with
    // the caller's own phrasing, which is the thing it is good at.
    if source.is_browsable() {
        if let Some(catalogue) = state.source_index.get(&source_id).await {
            let offers = match filter_to {
                Some(variants) => catalogue.matching(variants),
                None => catalogue.matching(&[]),
            };
            return SourceRead {
                source_id,
                outcome: ReadOutcome::Indexed,
                offers,
                ms: started.elapsed().as_millis() as u64,
            };
        }
    }

    let ctx = source.ctx(url);
    let (outcome, offers) = match fetch(state, cache_key, url, &source).await {
        Ok(Fetched::Fresh(body)) => (ReadOutcome::Fresh, parse_in(source.shape, &ctx, &body)),
        Ok(Fetched::Cached(body)) => (ReadOutcome::Cached, parse_in(source.shape, &ctx, &body)),
        Err(FetchFailure::Timeout) => (ReadOutcome::Timeout, Vec::new()),
        Err(FetchFailure::Error) => (ReadOutcome::Error, Vec::new()),
    };

    // The floor, applied to everything that arrives over the network.
    //
    // The indexed lane returned above, already gated once per crawl rather
    // than once per search (`crate::source_index::crawl`). What reaches here
    // is the QUERIED lane, which has no catalogue to crawl and would otherwise
    // be held to a lower standard than a listable source purely because of how
    // it is read — plus an indexed source on a cold start, whose first page
    // must not be the one page of the corpus that skips the gate.
    let offers: Vec<ExternalOffer> = offers
        .into_iter()
        .filter(|offer| {
            quality_gate(
                &offer.resource,
                &offer.host,
                &offer.description,
                offer.price_usd,
                offer.network.as_deref(),
            )
            .is_ok()
                && offer.doc_score >= crate::bazaar::MIN_LISTING_SCORE
        })
        .collect();
    let offers = match filter_to {
        Some(variants) => relevant(offers, variants),
        None => offers,
    };
    SourceRead {
        source_id,
        outcome,
        offers,
        ms: started.elapsed().as_millis() as u64,
    }
}

/// How one source's reads collapsed into a single verdict.
///
/// A queried source is asked once per phrasing, so several reads describe one
/// source. Named fields rather than a tuple because there are now six of them
/// and `entry.4` was already a comment away from being wrong.
#[derive(Debug, Clone, Copy)]
struct Verdict {
    /// At least one read came back with offers.
    answered: bool,
    /// Every answering read was served without a request.
    all_cached: bool,
    /// At least one answering read came out of the fully-crawled catalogue.
    from_index: bool,
    timed_out: bool,
    errored: bool,
    /// Wall clock of the slowest read.
    ms: u64,
}

impl Default for Verdict {
    fn default() -> Self {
        Self {
            answered: false,
            // Vacuously true until a read proves otherwise, which is what makes
            // "every answering read was cached" the right reading of a source
            // that has not answered yet.
            all_cached: true,
            from_index: false,
            timed_out: false,
            errored: false,
            ms: 0,
        }
    }
}

/// The pure half of the sweep: per-source reads in, a ranked merged list and a
/// status line per source out.
///
/// `order` is every source that was swept, in listing order; a source with no
/// read at all (which cannot happen from [`sweep_needs_with_status`], but can from a
/// test) is reported as a timeout, the honest reading of "never heard from".
fn merge_sweep(
    reads: Vec<SourceRead>,
    order: &[String],
    variants: &[String],
    known_hosts: &[String],
    limit: usize,
    curation: &crate::db::CurationMap,
) -> Sweep {
    // Where in `merged` each resource URL already sits, so a duplicate can be
    // compared with the copy that is already held rather than merely dropped.
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut contributed: HashMap<String, usize> = HashMap::new();
    let mut merged: Vec<ExternalOffer> = Vec::new();
    let mut verdicts: HashMap<String, Verdict> = HashMap::new();

    for read in reads {
        let entry = verdicts.entry(read.source_id.clone()).or_default();
        entry.ms = entry.ms.max(read.ms);
        match read.outcome {
            ReadOutcome::Timeout => entry.timed_out = true,
            ReadOutcome::Error => entry.errored = true,
            ReadOutcome::Fresh | ReadOutcome::Cached | ReadOutcome::Indexed => {
                if !read.offers.is_empty() {
                    entry.answered = true;
                    match read.outcome {
                        ReadOutcome::Fresh => entry.all_cached = false,
                        ReadOutcome::Indexed => entry.from_index = true,
                        _ => {}
                    }
                }
            }
        }
        for mut offer in read.offers {
            if crate::bazaar::skips_host(&offer.host) || known_hosts.contains(&offer.host) {
                continue;
            }
            // The admin's word on this seller, keyed the way the spread deals:
            // source, then registrable domain. Hidden means hidden — the offer
            // never merges, so it cannot rank, ship, or occupy a slot.
            let curated = curation.get(&offer.source).and_then(|sellers| {
                sellers.get(&crate::source_index::registrable_domain(&offer.host))
            });
            if curated.is_some_and(|c| c.hidden) {
                continue;
            }
            match seen.get(&offer.resource) {
                // The two lanes do not carry the same offer. The deep index
                // drops the provider-declared schemas to fit 49,000 entries in
                // memory (`crate::source_index`), while a queried hit keeps
                // them — so which copy of a duplicate arrived first used to
                // decide, by task-completion order, whether an agent was told
                // how to call the endpoint. Take the richer one.
                Some(&at) => {
                    let held = &mut merged[at];
                    if held.input_schema.is_none() && offer.input_schema.is_some() {
                        held.input = offer.input.clone();
                        held.input_schema = offer.input_schema.clone();
                        held.output = offer.output.clone();
                        if held.method.is_none() {
                            held.method = offer.method.clone();
                        }
                    }
                }
                None => {
                    if let Some(curated) = curated {
                        adopt_curation(&mut offer, curated);
                    }
                    seen.insert(offer.resource.clone(), merged.len());
                    *contributed.entry(offer.source.clone()).or_default() += 1;
                    merged.push(offer);
                }
            }
        }
    }

    let sources = order
        .iter()
        .map(|id| {
            let status = match verdicts.get(id) {
                Some(v) if v.answered && v.from_index => SourceOutcome::Indexed,
                Some(v) if v.answered && v.all_cached => SourceOutcome::Cached,
                Some(v) if v.answered => SourceOutcome::Ok,
                Some(v) if v.timed_out => SourceOutcome::Timeout,
                Some(v) if v.errored => SourceOutcome::Error,
                Some(_) => SourceOutcome::Empty,
                None => SourceOutcome::Timeout,
            };
            SourceStatus {
                id: id.clone(),
                status,
                offers: contributed.get(id).copied().unwrap_or(0),
                ms: verdicts.get(id).map(|v| v.ms).unwrap_or(0),
            }
        })
        .collect();

    rank_offers(&mut merged, variants, curation);
    // Spread BEFORE the cut: dealing a list out and then truncating is what
    // puts more than one seller in front of the caller. Truncating first and
    // dealing afterwards only reorders a page that is already one seller's.
    spread_providers(&mut merged);
    merged.truncate(limit.clamp(1, MAX_LIMIT));
    Sweep {
        offers: merged,
        sources,
        need_coverage: Vec::new(),
    }
}

/// A search vocabulary, prepared once for scoring a whole list.
///
/// The lowercasing and tokenising used to happen inside the comparator, so a
/// 2,800-offer merge re-derived the same eighty phrasings roughly thirty
/// thousand times — the sort's own O(n log n) multiplied by the vocabulary.
/// Preparing it once and scoring each offer once (see [`rank_offers`]) is the
/// same ordering for a fraction of the work, and it is what makes a wider
/// vocabulary ([`MAX_INDEX_TERMS`]) affordable at all.
struct Scorer {
    /// Per variant: the whole phrase when it is long enough to be worth
    /// looking for verbatim, and that variant's distinct tokens.
    variants: Vec<(Option<String>, Vec<String>)>,
}

impl Scorer {
    fn new(variants: &[String]) -> Self {
        Self {
            variants: variants
                .iter()
                .map(|variant| {
                    let phrase = variant.trim().to_ascii_lowercase();
                    let words: Vec<String> = tokens(&phrase).into_iter().collect();
                    (Some(phrase).filter(|p| p.len() >= 6), words)
                })
                .collect(),
        }
    }

    /// How much of what was searched for an offer's own words actually
    /// contain.
    ///
    /// A whole phrase landing is worth more than the words of that phrase
    /// landing separately: "analyst estimates" appearing verbatim says more
    /// than an endpoint that mentions analysts somewhere and estimates
    /// somewhere else. Deliberately crude — it orders a merged list, it does
    /// not decide anything — and it never filters, so an offer scoring zero is
    /// still returned, just last.
    ///
    /// The haystack is the same text retrieval matched on, provider name,
    /// category and tags included. It used to be the resource and description
    /// alone, which meant an offer FOUND through its tags — the whole reason a
    /// one-line 402 challenge is discoverable at all — scored zero and sorted
    /// behind everything, then fell off the truncation. A search that can find
    /// something it cannot rank is a search that throws it away.
    fn score(&self, offer: &ExternalOffer) -> usize {
        let haystack = format!(
            "{} {} {} {} {}",
            offer.resource,
            offer.description,
            offer.provider.as_deref().unwrap_or(""),
            offer.category.as_deref().unwrap_or(""),
            offer.tags.join(" "),
        )
        .to_ascii_lowercase();
        let mut score = 0;
        for (phrase, words) in &self.variants {
            if phrase.as_deref().is_some_and(|p| haystack.contains(p)) {
                score += 3;
            }
            for word in words {
                if haystack.contains(word) {
                    score += 1;
                }
            }
        }
        score
    }
}

/// Price as a sort key: cheapest first, unreadable last.
///
/// An `f64` is not `Ord`, and the ordering wants to be a key rather than a
/// comparator so each offer is scored once. Six decimals of USDC survive the
/// scaling with room to spare, and a price that is not a finite number sorts
/// with the unpriced — an agent cannot budget against either.
fn price_key(price: Option<f64>) -> (u8, u64) {
    match price {
        Some(p) if p.is_finite() && p >= 0.0 => (0, (p * 1e6).min(u64::MAX as f64) as u64),
        Some(p) if p.is_finite() => (0, 0),
        _ => (1, 0),
    }
}

/// Answering the question first, price second.
///
/// This used to sort on price alone, which reads as an ordering and is not
/// one: asked for analyst revenue estimates, a hundredth-of-a-cent string
/// reverser sorted above the analyst-estimates endpoint, and the truncation
/// to MAX_RETURNED then dropped the endpoint and kept the reverser. Price
/// still decides between two offers that answer the question equally well,
/// and an offer with no readable price still sorts last within its tier —
/// an agent cannot budget against a price it cannot read.
///
/// This is the whole ordering of the external half of a discovery response;
/// there is no composite score blending it with the catalogue's own rank.
///
/// A curated `preferred` seller (see `source_providers`) breaks ties BETWEEN
/// the relevance score and the price, and nothing more. Curation that could
/// outrank the caller's own words would be a shelf placement fee wearing a
/// quality-signal hat, and that ranking mistake has been made here before.
fn rank_offers(
    offers: &mut [ExternalOffer],
    variants: &[String],
    curation: &crate::db::CurationMap,
) {
    let scorer = Scorer::new(variants);
    // Cached rather than compared: the key is derived once per offer instead
    // of twice per comparison.
    offers.sort_by_cached_key(|offer| {
        let preferred = curation
            .get(&offer.source)
            .and_then(|sellers| sellers.get(&crate::source_index::registrable_domain(&offer.host)))
            .is_some_and(|c| c.preferred);
        (
            std::cmp::Reverse(scorer.score(offer)),
            std::cmp::Reverse(preferred as u8),
            price_key(offer.price_usd),
        )
    });
}

/// The admin's vocabulary, folded into the offer's own so ONE rule scores it.
///
/// Curated tags and categories are appended to `tags` rather than scored by a
/// side formula: the [`Scorer`]'s haystack already includes tags, so a curated
/// label participates in ranking exactly as a published one does — and it
/// travels out in the response, where the app's own semantic matching reads
/// tags too. Case-insensitive dedupe, so "Crypto" curated onto an offer
/// already tagged "crypto" is one label rather than two.
fn adopt_curation(offer: &mut ExternalOffer, curated: &crate::db::ProviderCuration) {
    for label in curated
        .curated_tags
        .iter()
        .chain(curated.curated_categories.iter())
    {
        let label = label.trim();
        if label.is_empty() {
            continue;
        }
        if offer.tags.iter().any(|tag| tag.eq_ignore_ascii_case(label)) {
            continue;
        }
        offer.tags.push(label.to_string());
    }
}

/// The ranked list, dealt out so no single seller owns the front of it.
///
/// Ranking alone is a per-offer judgement, and a page of twenty-five is a
/// judgement about a set. Those come apart when one provider publishes forty
/// endpoints about the topic: every one of them scores well, the truncation
/// keeps the first twenty-five, and an agent asking what the ecosystem can do
/// is shown one seller's catalogue. Sellers are visited in the order their
/// best offer earned, and each contributes [`PER_PROVIDER_RUN`] offers — in
/// their own ranked order — before the next gets a turn, so this reorders the
/// list without ever promoting an offer above a better one from its own
/// seller.
///
/// Off by construction when there is only one seller, which is why the
/// single-host tests below see the ranking unchanged.
fn spread_providers(offers: &mut Vec<ExternalOffer>) {
    let mut lanes: Vec<std::collections::VecDeque<ExternalOffer>> = Vec::new();
    let mut lane_of: HashMap<String, usize> = HashMap::new();
    for offer in offers.drain(..) {
        let key = crate::source_index::registrable_domain(&offer.host);
        match lane_of.get(&key) {
            Some(lane) => lanes[*lane].push_back(offer),
            None => {
                lane_of.insert(key, lanes.len());
                lanes.push(std::collections::VecDeque::from([offer]));
            }
        }
    }
    if lanes.len() < 2 {
        offers.extend(lanes.into_iter().flatten());
        return;
    }
    // Offers are moved through the lanes rather than copied out of them: this
    // runs on every sweep over a list that can hold a few thousand entries,
    // and an ExternalOffer carries several owned strings.
    loop {
        let mut dealt = false;
        for lane in &mut lanes {
            for _ in 0..PER_PROVIDER_RUN {
                let Some(offer) = lane.pop_front() else { break };
                offers.push(offer);
                dealt = true;
            }
        }
        if !dealt {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// Browsing one source
// ---------------------------------------------------------------------------

/// Why a source cannot be listed. (Whether the source EXISTS is answered by
/// [`source_by_id`] returning `None`, so it is not a variant here.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowseError {
    /// A queried index only answers questions; there is no "everything" to list.
    QueryRequired,
    /// The index did not answer, or answered with something unreadable.
    SourceUnavailable,
}

impl Source {
    /// Can this index be listed whole, or does it only answer questions?
    pub fn is_browsable(&self) -> bool {
        matches!(self.fetch, Fetch::Indexed { .. })
    }
}

/// One source, if it is in the sweep right now.
///
/// Reads the same resolved list `sweep` uses, so a source that is disabled or
/// misconfigured is missing here for exactly the reason it is missing from
/// discovery — one answer to "is this live", not two.
pub async fn source_by_id(state: &Arc<AppState>, id: &str) -> Option<Source> {
    active_sources(state).await.into_iter().find(|s| s.id == id)
}

/// Everything one source publishes, read by the sweep's own parser.
///
/// This is what `sweep` does for a single index, minus the parts that only
/// make sense when several indexes are being merged: no cross-source dedupe,
/// no relevance ordering, no truncation to a page of results. Browsing wants
/// the catalogue; discovery wants the best few answers to a question.
///
/// Two filters DO stay, because they are about the offer rather than the
/// query: a host the gateway refuses to touch, and a resource that reads as a
/// demo. Showing those on an explore page would be advertising something
/// discovery will never return.
///
/// Shares the sweep's five-minute cache, keyed identically — an explore page
/// warms discovery and vice versa, and neither one can turn a browsing session
/// into a burst of traffic against somebody else's index.
pub async fn browse(
    state: &Arc<AppState>,
    source: &Source,
    query: Option<&str>,
) -> Result<Browsed, BrowseError> {
    let (cache_key, url) = match &source.fetch {
        Fetch::Indexed { url } => (format!("{}::__index", source.id), url.clone()),
        Fetch::Queried { template } => {
            // Not a shortcoming to paper over: a queried index has no endpoint
            // that returns its whole catalogue. Saying so lets the caller ask
            // for a search box instead of rendering an empty list that looks
            // like an outage.
            let query = query
                .map(str::trim)
                .filter(|q| !q.is_empty())
                .ok_or(BrowseError::QueryRequired)?;
            (
                format!("{}::{}", source.id, query),
                template.replace("{query}", &urlencode(query)),
            )
        }
    };

    // An indexed source is answered from the whole crawled catalogue —
    // SEARCHED or LISTED.
    //
    // This used to be scoped to the query case, on the reasoning that a paged
    // explore surface pages its own way and swapping 100 entries for 26,684
    // underneath it changes that surface rather than discovery's reach. That
    // was wrong in a way only the screen showed: `offset` on the listing was
    // paging the HOST GROUPS of one live first page, so PayAI listed seven
    // sellers of a hundred and eighty-three, every later offset came back
    // empty, and `/v1/sources/payai/apis/{host}` answered "publishes nothing"
    // for sellers whose crawled row says eight endpoints. A reader cannot page
    // to entries the reading never contained.
    //
    // So both paths read the crawl, which is the same corpus `/v1/discover`
    // ranks. The live first page below stays as the fallback for a queried
    // source (no catalogue exists) and for the minutes before the first crawl
    // of an indexed one lands.
    if source.is_browsable() {
        if let Some(catalogue) = state.source_index.get(&source.id).await {
            let crawled = Crawled {
                entries_read: catalogue.read,
                pages: catalogue.pages,
                complete: catalogue.stopped.complete(),
                refreshed_unix: catalogue.refreshed_unix,
            };
            let read = catalogue.len();
            let declared = catalogue.declared_total;
            // Listing lends the catalogue; searching owns its answer. The
            // whole list is nine thousand entries for one source and is not
            // copied per request to be paged over — `Catalogue::build` put it
            // in browse order so a borrow is enough.
            let offers = match query.map(str::trim).filter(|q| !q.is_empty()) {
                Some(q) => {
                    let mut hits = catalogue.matching(&[q.to_string()]);
                    sort_for_browsing(&mut hits);
                    Reading::Owned(hits)
                }
                None => Reading::Crawled(catalogue),
            };
            return Ok(Browsed {
                matched: offers.offers().len(),
                read,
                index_reports_total: declared,
                crawled: Some(crawled),
                offers,
            });
        }
    }

    let body = match fetch(state, &cache_key, &url, source).await {
        Ok(Fetched::Fresh(body)) | Ok(Fetched::Cached(body)) => body,
        Err(_) => return Err(BrowseError::SourceUnavailable),
    };

    let mut seen: HashSet<String> = HashSet::new();
    let mut offers: Vec<ExternalOffer> = parse_in(source.shape, &source.ctx(&url), &body)
        .into_iter()
        .filter(|offer| {
            quality_gate(
                &offer.resource,
                &offer.host,
                &offer.description,
                offer.price_usd,
                offer.network.as_deref(),
            )
            .is_ok()
                && offer.doc_score >= crate::bazaar::MIN_LISTING_SCORE
                && seen.insert(offer.resource.clone())
        })
        .collect();
    let read = offers.len();

    // An indexed source hands over a document; narrowing it costs no network,
    // which is what makes a per-source search box affordable at all.
    if source.is_browsable() {
        if let Some(query) = query.map(str::trim).filter(|q| !q.is_empty()) {
            offers = relevant(offers, &[query.to_string()]);
        }
    }

    sort_for_browsing(&mut offers);

    Ok(Browsed {
        read,
        matched: offers.len(),
        index_reports_total: declared_total(&body),
        crawled: None,
        offers: Reading::Owned(offers),
    })
}

/// Stable order for a browsed list.
///
/// A list that reshuffles between two reads of the same page cannot be paged
/// through, and grouping by host is what turns a flat list of resources into
/// the providers behind them. Shared by both of `browse`'s answers — the deep
/// index and the first page — so a per-source search and a per-source listing
/// never come back sorted differently.
pub(crate) fn sort_for_browsing(offers: &mut [ExternalOffer]) {
    offers.sort_by(|a, b| {
        a.host.cmp(&b.host).then_with(|| {
            match (a.price_usd, b.price_usd) {
                (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
            .then_with(|| a.resource.cmp(&b.resource))
        })
    });
}

/// Where a browsed reading came from, when it came from the crawl.
///
/// The difference between "this is one page of an index that declares 27,925"
/// and "this is that index, read to its end" is not visible in the offers
/// themselves, and it is the whole of what a coverage line has to say.
pub struct Crawled {
    /// Raw entries the crawler read across every page, before parsing and
    /// filtering. This — not the endpoint count — is what an index's declared
    /// total is comparable to: one resource listed on three networks is three
    /// entries there and one endpoint here.
    pub entries_read: usize,
    pub pages: usize,
    /// Did the crawl reach the end of what the index publishes?
    pub complete: bool,
    pub refreshed_unix: u64,
}

/// Where a reading's offers live.
///
/// A crawled listing is the whole catalogue and is BORROWED — it is up to nine
/// thousand entries for one source, and copying that per request to page over
/// it is the kind of cost this module keeps being fixed for. Everything else
/// is a freshly built answer and owns itself.
pub enum Reading {
    Owned(Vec<ExternalOffer>),
    Crawled(Arc<crate::source_index::Catalogue>),
}

impl Reading {
    pub fn offers(&self) -> &[ExternalOffer] {
        match self {
            Self::Owned(offers) => offers,
            Self::Crawled(catalogue) => catalogue.offers(),
        }
    }
}

/// One read of one index.
pub struct Browsed {
    offers: Reading,
    /// Entries the parser got out of the document, before any query filter.
    pub read: usize,
    /// Entries left after the query filter (equal to `read` without one).
    pub matched: usize,
    /// What the index says its whole catalogue holds, when it says so.
    ///
    /// This is the difference between "this index has 100 endpoints" and "this
    /// index handed us the first 100 of 26,546". PayAI declares the second and
    /// nothing in the sweep noticed until an explore page asked to list it —
    /// the module doc above still calls an indexed source "the whole catalogue
    /// as one document", which is true of the manifests and false of the
    /// paginated facilitators. Reported rather than hidden, because a reader
    /// who takes 100 for the total is wrong by two orders of magnitude.
    pub index_reports_total: Option<u64>,
    /// Set when this reading is the background crawl. `None` means one live
    /// page — a queried source, or an indexed one whose first crawl since
    /// startup has not landed yet.
    pub crawled: Option<Crawled>,
}

impl Browsed {
    /// The offers themselves, however this reading came by them.
    pub fn offers(&self) -> &[ExternalOffer] {
        self.offers.offers()
    }

    /// A reading assembled by hand, holding no offers. Tests only — every real
    /// one comes from [`browse`], which is the only place that knows which
    /// kind it is.
    #[cfg(test)]
    pub(crate) fn of(
        read: usize,
        index_reports_total: Option<u64>,
        crawled: Option<Crawled>,
    ) -> Self {
        Self {
            offers: Reading::Owned(Vec::new()),
            read,
            matched: read,
            index_reports_total,
            crawled,
        }
    }
}

/// The catalogue size an index declares, across the spellings in use.
pub(crate) fn declared_total(body: &Value) -> Option<u64> {
    body.get("pagination")
        .and_then(|p| p.get("total"))
        .or_else(|| body.get("total"))
        .or_else(|| body.get("totalCount"))
        .or_else(|| body.get("count"))
        .and_then(Value::as_u64)
}

/// How a source is reachable from the browse endpoints.
pub fn source_summary_json(source: &Source) -> Value {
    let (fetch_kind, url) = match &source.fetch {
        Fetch::Indexed { url } => ("indexed", url.clone()),
        Fetch::Queried { template } => ("queried", template.clone()),
    };
    let browsable = source.is_browsable();
    json!({
        "id": source.id,
        "fetch_kind": fetch_kind,
        "shape": source.shape.db_name(),
        "url": url,
        "note": source.note,
        // The identity columns, published rather than kept for the sweep. They
        // are already stamped onto every offer this source yields, so a surface
        // that lists the sources themselves had to invent a name out of the id
        // and had no mark at all — while the offers underneath carried both.
        //
        // Null on an aggregator, and that is the honest answer: `name` and
        // `logo_url` say who is SELLING, so an index that publishes many
        // sellers has neither. A reader-facing surface falls back to the id and
        // to the source's own site for a mark; see the app's `EntityLogo`.
        "name": source.provider,
        "logo_url": source.logo_url,
        "category": source.category,
        "tags": source.tags,
        // The distinction the explore page has to render: an indexed source can
        // be listed whole, a queried one only answers questions.
        "browsable": browsable,
        "requires_query": !browsable,
    })
}

// ---------------------------------------------------------------------------
// Probing one source
// ---------------------------------------------------------------------------

/// How long a probe waits before calling a source dead.
///
/// Deliberately longer than [`SOURCE_TIMEOUT`]: an admin asking "is this thing
/// alive" can wait, and a source that answers in four seconds is not dead, it
/// is merely useless to the sweep. Reporting it as unreachable would send
/// someone hunting for an outage that is really a latency problem, so the probe
/// waits, measures, and says which of the two it is.
const PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// Bodies past this are truncated rather than parsed. Nothing legitimate in the
/// x402 index ecosystem comes close; a source that does is a mistake worth
/// seeing as a mistake instead of as an out-of-memory.
const MAX_PROBE_BYTES: usize = 8 * 1024 * 1024;

/// How many parsed offers come back with the report.
///
/// Twenty rather than a handful because the question an admin actually asks is
/// "does this link work" — and that is answered by reading real endpoint URLs,
/// not by a count. Twenty is enough to recognise a catalogue and small enough
/// to stay in one panel.
const PROBE_SAMPLE: usize = 20;

/// What one candidate source actually returns, read by the same parser the
/// sweep uses.
///
/// The seed comment on migration 078 says every built-in shape was "probed live
/// before being added — the shape is what the source actually returns, not what
/// its documentation claims". That was done by hand and written down nowhere,
/// so the next person adding a source had no way to repeat it. This is that
/// check, performed by the code that will do the parsing for real.
///
/// Deliberately does not touch the source cache in either direction. Answering
/// a probe from a five-minute-old document would make a source that died four
/// minutes ago look healthy, and writing the probe's result into the cache
/// would let an admin's experiment decide what agents see.
///
/// `Err` is the caller's mistake (a malformed request), never the source's: a
/// timeout, a 500 or an unparseable body are findings, and findings come back
/// as `Ok` with the detail attached.
pub async fn probe(
    state: &Arc<AppState>,
    url_template: &str,
    shape_name: &str,
    fetch_kind: &str,
    query: &str,
    method: Option<&str>,
    request_body: Option<&str>,
) -> Result<Value, String> {
    let url_template = url_template.trim();
    let (shape, queried) = validate_probe(url_template, shape_name, fetch_kind)?;

    let query = match query.trim() {
        "" => "weather",
        q => q,
    };
    let url = if queried {
        url_template.replace("{query}", &urlencode(query))
    } else {
        url_template.to_string()
    };

    if let Err(reason) = crate::api_management::helpers::validate_upstream_url(&url) {
        return Err(reason.to_string());
    }

    let verb = normalise_method(method);
    let expects_challenge = shape.expects_challenge();

    let started = tokio::time::Instant::now();
    let mut request = state
        .http_client
        .request(http_method(&verb), &url)
        .header("Accept", "application/json")
        .timeout(PROBE_TIMEOUT);
    if let Some(payload) = request_body.filter(|b| !b.trim().is_empty()) {
        request = request
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(payload.to_string());
    }
    let response = request.send().await;
    let response = match response {
        Ok(r) => r,
        Err(err) => {
            return Ok(json!({
                "ok": false,
                "request": probe_request_json(&url, shape, queried, query, &verb),
                "reached": false,
                "elapsed_ms": started.elapsed().as_millis() as u64,
                "problem": if err.is_timeout() { "timeout" } else { "unreachable" },
                "detail": err.to_string(),
                "warnings": [format!(
                    "The sweep would get nothing from this source: {}",
                    if err.is_timeout() {
                        format!("no answer within the probe's {}s", PROBE_TIMEOUT.as_secs())
                    } else {
                        "the request never completed".to_string()
                    }
                )],
            }));
        }
    };

    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let header_challenge = challenge_from_header(response.headers());
    let body = response.bytes().await.unwrap_or_default();
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let bytes = body.len();

    let mut warnings: Vec<String> = Vec::new();
    // The sweep gives every source SOURCE_TIMEOUT and no more. A source that
    // answers correctly but late contributes nothing, and that is invisible
    // from the response alone — so say it here, where someone is looking.
    if elapsed_ms > SOURCE_TIMEOUT.as_millis() as u64 {
        warnings.push(format!(
            "Answered in {elapsed_ms}ms, past the sweep's {}ms per-source timeout — live discovery would drop this source even though the probe reached it.",
            SOURCE_TIMEOUT.as_millis()
        ));
    }

    // A 402 is the correct, expected answer from a challenge source — the
    // payment wall IS the document. Reporting it as an http_error is how the
    // panel used to show a healthy provider as broken.
    let payment_required = status == reqwest::StatusCode::PAYMENT_REQUIRED;
    if payment_required && !expects_challenge {
        warnings.push(
            "This source answers 402 Payment Required — that is an x402 endpoint describing itself, not an index. Set the shape to x402-challenge to read it."
                .to_string(),
        );
    }
    if !status.is_success() && !(expects_challenge && payment_required) {
        return Ok(json!({
            "ok": false,
            "request": probe_request_json(&url, shape, queried, query, &verb),
            "reached": true,
            "elapsed_ms": elapsed_ms,
            "http_status": status.as_u16(),
            "bytes": bytes,
            "content_type": content_type,
            "problem": "http_error",
            "detail": String::from_utf8_lossy(&body).chars().take(500).collect::<String>(),
            "warnings": warnings,
        }));
    }

    if bytes > MAX_PROBE_BYTES {
        warnings.push(format!(
            "Response is {bytes} bytes, past the {MAX_PROBE_BYTES}-byte probe ceiling — not parsed."
        ));
        return Ok(json!({
            "ok": false,
            "request": probe_request_json(&url, shape, queried, query, &verb),
            "reached": true,
            "elapsed_ms": elapsed_ms,
            "http_status": status.as_u16(),
            "bytes": bytes,
            "content_type": content_type,
            "problem": "too_large",
            "warnings": warnings,
        }));
    }

    let from_body: Option<Value> = serde_json::from_slice(&body).ok();
    if expects_challenge && from_body.is_none() && header_challenge.is_some() {
        warnings.push(format!(
            "The body is {bytes} bytes and carries no challenge; the whole thing arrived in the {CHALLENGE_HEADER} header, which is where it was read from."
        ));
    }
    let parsed_body: Value = match best_document(from_body, header_challenge, expects_challenge) {
        Some(v) => v,
        None => {
            let err = "no JSON in the body, and no readable payment-required header";
            return Ok(json!({
                "ok": false,
                "request": probe_request_json(&url, shape, queried, query, &verb),
                "reached": true,
                "elapsed_ms": elapsed_ms,
                "http_status": status.as_u16(),
                "bytes": bytes,
                "content_type": content_type,
                "problem": "not_json",
                "detail": err,
                "preview": String::from_utf8_lossy(&body).chars().take(500).collect::<String>(),
                "warnings": warnings,
            }));
        }
    };

    // Run every parser, not just the chosen one. "Nothing came out" and "you
    // picked the wrong dialect" look identical from a single count, and the
    // second is the far more common mistake when adding a source.
    // The context carries the probed URL: a manifest that lists
    // "GET /v1/wallets/{address}" has no absolute endpoint in it, and without
    // the URL it was read from there is nothing to show.
    let mut ctx = ParseCtx::bare("probe");
    ctx.url = &url;
    let by_shape: Vec<(Shape, Vec<ExternalOffer>)> = Shape::ALL
        .iter()
        .map(|s| (*s, parse_in(*s, &ctx, &parsed_body)))
        .collect();
    let offers: Vec<ExternalOffer> = by_shape
        .iter()
        .find(|(s, _)| *s == shape)
        .map(|(_, o)| o.clone())
        .unwrap_or_default();

    if let Some(warning) = shape_verdict(
        shape,
        &by_shape
            .iter()
            .map(|(s, o)| (*s, o.len()))
            .collect::<Vec<_>>(),
    ) {
        warnings.push(warning);
    }

    // What the sweep would throw away after parsing. An offer that parses and
    // is then dropped never reaches an agent, so a probe that stopped at the
    // parse count would overstate the source's real contribution.
    let mut skipped_host = 0usize;
    let mut toy = 0usize;
    let mut survivors: Vec<&ExternalOffer> = Vec::new();
    for offer in &offers {
        match quality_gate(
            &offer.resource,
            &offer.host,
            &offer.description,
            offer.price_usd,
            offer.network.as_deref(),
        ) {
            Err(crate::bazaar::Reject::BadHost)
            | Err(crate::bazaar::Reject::EphemeralHost)
            | Err(crate::bazaar::Reject::NotAPublicName)
            | Err(crate::bazaar::Reject::NotHttps) => skipped_host += 1,
            Err(_) => toy += 1,
            Ok(()) => survivors.push(offer),
        }
    }

    let priced = survivors.iter().filter(|o| o.price_usd.is_some()).count();
    let with_schema = survivors
        .iter()
        .filter(|o| o.input_schema.is_some())
        .count();

    if !offers.is_empty() && survivors.is_empty() {
        warnings.push(format!(
            "Every one of the {} parsed offers is filtered out before an agent sees it ({skipped_host} on host policy, {toy} as toy endpoints). This source would add nothing to the sweep.",
            offers.len()
        ));
    }
    if !survivors.is_empty() && priced == 0 {
        warnings.push(
            "No surviving offer carries a readable price. Agents cannot budget against these, and they sort last in every result."
                .to_string(),
        );
    }

    // Indexed sources are filtered locally against the query, so the parse
    // count is not what a caller would see. Show both.
    let after_relevance = if queried {
        None
    } else {
        Some(
            relevant(
                survivors.iter().map(|o| (*o).clone()).collect(),
                &[query.to_string()],
            )
            .len(),
        )
    };

    Ok(json!({
        "ok": true,
        "request": probe_request_json(&url, shape, queried, query, &verb),
        "reached": true,
        "elapsed_ms": elapsed_ms,
        "http_status": status.as_u16(),
        "bytes": bytes,
        "content_type": content_type,
        "outline": outline(&parsed_body),
        "shape_matrix": by_shape
            .iter()
            .map(|(s, o)| json!({ "shape": s.db_name(), "offers": o.len() }))
            .collect::<Vec<_>>(),
        "parsed": offers.len(),
        "dropped": { "skipped_host": skipped_host, "toy": toy },
        "usable": survivors.len(),
        "matching_query": after_relevance,
        "priced": priced,
        "with_input_schema": with_schema,
        "sample": survivors
            .iter()
            .take(PROBE_SAMPLE)
            .map(|o| json!({
                "resource": o.resource,
                "host": o.host,
                "description": o.description,
                "price_usd": o.price_usd,
                "network": o.network,
                "method": o.method,
                "has_input_schema": o.input_schema.is_some(),
            }))
            .collect::<Vec<_>>(),
        "warnings": warnings,
    }))
}

/// The parts of a probe request that can be judged without touching the
/// network, checked in the order that gives the most useful complaint first.
///
/// These are the table's own constraints (migration 078), restated here so an
/// admin hears about a mismatched `{query}` slot from the probe button rather
/// than from a Postgres check-constraint error on save.
fn validate_probe(
    url_template: &str,
    shape_name: &str,
    fetch_kind: &str,
) -> Result<(Shape, bool), String> {
    let Some(shape) = Shape::from_db(shape_name) else {
        return Err(format!(
            "unknown shape \"{shape_name}\"; the parsers are {}",
            Shape::ALL
                .iter()
                .map(|s| s.db_name())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };
    let queried = match fetch_kind {
        "queried" => true,
        "indexed" => false,
        other => {
            return Err(format!(
                "unknown fetch_kind \"{other}\"; use queried or indexed"
            ))
        }
    };
    if queried != url_template.contains("{query}") {
        return Err(if queried {
            "a queried source must carry the {query} slot in its url".to_string()
        } else {
            "an indexed source must not carry a {query} slot in its url".to_string()
        });
    }
    Ok((shape, queried))
}

/// The one thing a single offer count cannot say: whether an empty result
/// means the source is empty or the dialect is wrong.
///
/// Wrong shape is the common mistake when adding a source — the URL is right,
/// the document is full, and the chosen parser reads none of it — and from a
/// count of zero it is indistinguishable from a dead index. Comparing every
/// parser's yield on the same document separates the two, and names the parser
/// that would have worked.
fn shape_verdict(chosen: Shape, yields: &[(Shape, usize)]) -> Option<String> {
    let mine = yields
        .iter()
        .find(|(s, _)| *s == chosen)
        .map(|(_, n)| *n)
        .unwrap_or(0);
    if mine > 0 {
        return None;
    }
    match yields
        .iter()
        .filter(|(_, n)| *n > 0)
        .max_by_key(|(_, n)| *n)
    {
        Some((better, found)) => Some(format!(
            "The {} parser found nothing here, but {} reads {found} offers out of the same document — the shape is probably wrong.",
            chosen.db_name(),
            better.db_name(),
        )),
        None => Some(
            "No parser reads an offer out of this document. Either the source speaks a dialect the gateway does not know, or this URL is not the index."
                .to_string(),
        ),
    }
}

fn probe_request_json(url: &str, shape: Shape, queried: bool, query: &str, method: &str) -> Value {
    json!({
        "url": url,
        "shape": shape.db_name(),
        "fetch_kind": if queried { "queried" } else { "indexed" },
        "query": if queried { Some(query) } else { None },
        "method": method,
    })
}

/// The document's own shape, one level deep: enough to recognise "the array is
/// called `services`, not `items`" without printing a megabyte of JSON.
fn outline(body: &Value) -> Value {
    match body {
        Value::Object(map) => Value::Array(
            map.iter()
                .map(|(key, value)| {
                    json!({
                        "key": key,
                        "kind": kind_of(value),
                        "len": value.as_array().map(|a| a.len()),
                    })
                })
                .collect(),
        ),
        Value::Array(items) => json!([{ "key": "(root)", "kind": "array", "len": items.len() }]),
        other => json!([{ "key": "(root)", "kind": kind_of(other), "len": Value::Null }]),
    }
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn probe_names_the_parser_that_would_have_worked() {
        // The mistake this exists for: right URL, full document, wrong dialect.
        // A bare "0 offers" reads as a dead source and sends someone hunting an
        // outage that is really a one-word typo in the shape field.
        let verdict = shape_verdict(
            Shape::X402Items,
            &[
                (Shape::X402Items, 0),
                (Shape::AgenticMarket, 41),
                (Shape::X402Engine, 2),
            ],
        )
        .expect("a mismatch should be reported");
        assert!(verdict.contains("agentic-market"), "{verdict}");
        assert!(verdict.contains("41"), "{verdict}");
    }

    #[test]
    fn probe_says_so_when_no_parser_reads_the_document_at_all() {
        let verdict = shape_verdict(
            Shape::X402Items,
            &[(Shape::X402Items, 0), (Shape::X402Direct, 0)],
        )
        .expect("an unreadable document should be reported");
        assert!(verdict.contains("No parser"), "{verdict}");
    }

    #[test]
    fn probe_stays_quiet_when_the_chosen_shape_reads_the_document() {
        // Another parser scoring higher is not a problem on its own: several
        // dialects overlap, and a source that works is not worth second-guessing.
        assert_eq!(
            shape_verdict(
                Shape::X402Items,
                &[(Shape::X402Items, 12), (Shape::X402Direct, 30)]
            ),
            None
        );
    }

    #[test]
    fn a_paginated_index_declares_a_total_the_page_does_not_show() {
        // PayAI's real shape: 100 entries, 26,546 available. Read so a browse
        // response can say which of the two the reader is looking at.
        let body = json!({"items": [], "pagination": {"limit": 100, "offset": 0, "total": 26546}});
        assert_eq!(declared_total(&body), Some(26_546));
        // A manifest that declares nothing is not a source of a zero.
        assert_eq!(declared_total(&json!({"services": [1, 2]})), None);
    }

    #[test]
    fn probe_rejects_a_query_slot_that_contradicts_the_fetch_kind() {
        // The pair the table's check constraint enforces. Getting this wrong is
        // the most common way a hand-typed source fails to save, so the probe
        // has to catch it before it spends a request finding out.
        assert!(validate_probe(
            "https://example.test/search?q={query}",
            "x402-items",
            "indexed"
        )
        .is_err());
        assert!(validate_probe("https://example.test/all", "x402-items", "queried").is_err());
        assert_eq!(
            validate_probe("https://example.test/all", "x402-items", "indexed"),
            Ok((Shape::X402Items, false))
        );
    }

    #[test]
    fn probe_names_the_parsers_when_the_shape_is_unknown() {
        let err = validate_probe("https://example.test/all", "x402-itemz", "indexed")
            .expect_err("unknown shape");
        // The point of the message is that it lists the alternatives — an admin
        // who mistyped one has no other place to look them up.
        assert!(err.contains("x402-items"), "{err}");
        assert!(err.contains("anchor-manifest"), "{err}");
    }

    #[test]
    fn every_shape_survives_the_round_trip_through_its_db_spelling() {
        // Shape::ALL, from_db and db_name are three lists that must agree; the
        // probe reports a shape by name, so a spelling that does not round-trip
        // would tell an admin to save something the table would reject.
        for shape in Shape::ALL {
            assert_eq!(Shape::from_db(shape.db_name()), Some(*shape));
        }
    }

    #[test]
    fn outline_reports_the_array_a_document_actually_carries() {
        // The whole reason to show an outline: "services", not "items", is why
        // the x402-items parser found nothing.
        let body = json!({"baseUrl": "https://x.test", "services": [1, 2, 3]});
        let out = outline(&body);
        let services = out
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["key"] == "services")
            .expect("services entry");
        assert_eq!(services["kind"], "array");
        assert_eq!(services["len"], 3);
    }

    #[test]
    fn parses_the_shared_x402_dialect() {
        let body = json!({
            "items": [{
                "resource": "https://example.com/extract",
                "description": "Extract text from a web page",
                "accepts": [{"maxAmountRequired": "10000", "network": "eip155:8453", "payTo": "0xabc"}],
                "metadata": {"method": "post"}
            }]
        });
        let offers = parse(Shape::X402Items, "thirdweb", &body);
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].host, "example.com");
        assert_eq!(offers[0].source, "thirdweb");
        assert_eq!(offers[0].method.as_deref(), Some("POST"));
        // 10000 atomic units of USDC is one cent.
        assert_eq!(offers[0].price_usd, Some(0.01));
        // Nothing declared, so nothing invented.
        assert!(offers[0].input.is_none() && offers[0].input_schema.is_none());
    }

    #[test]
    fn carries_the_bazaars_declared_call_shape() {
        let body = json!({
            "resources": [{
                "resource": "https://example.com/news/:ticker",
                "description": "News for a ticker",
                "accepts": [{"amount": "5000", "network": "eip155:8453", "payTo": "0xabc"}],
                "extensions": {"bazaar": {
                    "info": {
                        "input": {"type": "http", "method": "GET", "pathParams": {"ticker": "AMD"}},
                        "output": {"type": "json", "example": {"articles": []}}
                    },
                    "schema": {"properties": {"input": {
                        "type": "object",
                        "properties": {"pathParams": {"properties": {"ticker": {"type": "string"}}, "required": ["ticker"]}}
                    }}}
                }}
            }]
        });
        let offers = parse(Shape::X402Items, "coinbase-x402-bazaar", &body);
        assert_eq!(offers.len(), 1);
        // Method comes from the declared input, uppercased.
        assert_eq!(offers[0].method.as_deref(), Some("GET"));
        assert_eq!(
            offers[0].input.as_ref().unwrap()["pathParams"]["ticker"],
            "AMD"
        );
        assert!(offers[0].input_schema.as_ref().unwrap()["properties"]["pathParams"].is_object());
        assert_eq!(offers[0].output.as_ref().unwrap()["type"], "json");
    }

    #[test]
    fn falls_back_to_the_legacy_output_schema_dialect() {
        let body = json!({
            "items": [{
                "resource": "https://example.com/quote",
                "description": "A quote",
                "accepts": [{"maxAmountRequired": "10000",
                             "outputSchema": {"input": {"method": "POST", "bodyFields": {"symbol": "AAPL"}},
                                              "output": {"type": "json"}}}]
            }]
        });
        let offers = parse(Shape::X402Items, "payai", &body);
        assert_eq!(offers[0].method.as_deref(), Some("POST"));
        assert_eq!(
            offers[0].input.as_ref().unwrap()["bodyFields"]["symbol"],
            "AAPL"
        );
        assert!(offers[0].input_schema.is_none());
    }

    #[test]
    fn reads_resources_as_well_as_items() {
        let body = json!({
            "resources": [{
                "resource": "https://example.com/a",
                "description": "Something useful",
                "accepts": []
            }]
        });
        assert_eq!(parse(Shape::X402Items, "payai", &body).len(), 1);
    }

    #[test]
    fn joins_manifest_paths_onto_the_base_url() {
        let body = json!({
            "base_url": "https://api.anchor-x402.com/",
            "networks": [{"id": "eip155:8453"}],
            "routes": [{"path": "/v1/anchor", "method": "POST", "price_usd": 0.005,
                        "description": "Anchor a hash to Base and Solana"}]
        });
        let offers = parse(Shape::AnchorManifest, "anchor-x402", &body);
        assert_eq!(offers[0].resource, "https://api.anchor-x402.com/v1/anchor");
        assert_eq!(offers[0].price_usd, Some(0.005));
        assert_eq!(offers[0].network.as_deref(), Some("eip155:8453"));
        assert!(offers[0].input_schema.is_none());
    }

    #[test]
    fn normalises_the_openapi_parameter_list_dialect() {
        // thirdweb's outputSchema.parameters; agentic.market's endpoint
        // parameters differ only in field spellings (group/type/enumValues).
        let body = json!({
            "items": [{
                "resource": "https://example.com/query",
                "description": "Market news",
                "accepts": [{"maxAmountRequired": "10000",
                             "outputSchema": {"parameters": [
                                 {"in": "query", "name": "tickers", "required": true,
                                  "schema": {"type": "string", "example": "COIN"},
                                  "description": "Comma-separated tickers"},
                                 {"group": "path", "name": "id", "type": "string",
                                  "required": false, "description": "", "enumValues": ["a", "b"]}
                             ]}}]
            }]
        });
        let offers = parse(Shape::X402Items, "thirdweb", &body);
        let schema = offers[0].input_schema.as_ref().unwrap();
        let query = &schema["properties"]["queryParams"];
        assert_eq!(query["properties"]["tickers"]["type"], "string");
        assert_eq!(
            query["properties"]["tickers"]["description"],
            "Comma-separated tickers"
        );
        assert_eq!(query["required"][0], "tickers");
        let path = &schema["properties"]["pathParams"];
        assert_eq!(path["properties"]["id"]["enum"][0], "a");
        // The empty description string was not copied.
        assert!(path["properties"]["id"].get("description").is_none());
    }

    #[test]
    fn normalises_x402engines_flat_body_dialect() {
        let body = json!({
            "baseUrl": "https://x402engine.app",
            "services": [{"name": "Image", "description": "Generate an image",
                          "endpoint": "/api/image", "method": "POST", "price": 0.01,
                          "parameters": {"body": {
                              "prompt": {"type": "string", "required": true, "description": "What to draw"},
                              "width": {"type": "number", "required": false}
                          }}}]
        });
        let offers = parse(Shape::X402Engine, "x402engine", &body);
        let schema = offers[0].input_schema.as_ref().unwrap();
        let body_group = &schema["properties"]["body"];
        assert_eq!(body_group["properties"]["prompt"]["type"], "string");
        assert_eq!(body_group["required"][0], "prompt");
        assert!(body_group["properties"]["width"].is_object());
    }

    #[test]
    fn anchors_body_schema_and_flat_params_both_survive() {
        let body = json!({
            "base_url": "https://api.anchor-x402.com/",
            "routes": [
                {"path": "/v1/anchor", "method": "POST", "price_usd": 0.005, "description": "Anchor a hash",
                 "input_schema": {"type": "object", "properties": {"hash": {"type": "string"}}}},
                {"path": "/v1/screen", "method": "GET", "price_usd": 0.003, "description": "Screen a wallet",
                 "params": {"wallet": {"type": "string", "required": true, "description": "EVM or Solana address"}}}
            ]
        });
        let offers = parse(Shape::AnchorManifest, "anchor-x402", &body);
        let post = offers[0].input_schema.as_ref().unwrap();
        assert_eq!(
            post["properties"]["body"]["properties"]["hash"]["type"],
            "string"
        );
        let get = offers[1].input_schema.as_ref().unwrap();
        assert_eq!(get["properties"]["queryParams"]["required"][0], "wallet");
    }

    #[test]
    fn flattens_agentic_market_services_into_endpoints() {
        let body = json!({
            "services": [{
                "name": "netintel.dev",
                "description": "",
                "networks": ["eip155:8453"],
                "endpoints": [
                    {"url": "https://netintel.dev/page-extract/read", "description": "Fetch any article"},
                    {"url": "https://netintel.dev/other", "description": "Another thing"}
                ]
            }]
        });
        let offers = parse(Shape::AgenticMarket, "agentic-market", &body);
        assert_eq!(offers.len(), 2);
        assert_eq!(offers[0].network.as_deref(), Some("eip155:8453"));
    }

    #[test]
    fn prices_survive_every_dialect() {
        assert_eq!(parse_price(Some(&json!(0.005))), Some(0.005));
        assert_eq!(parse_price(Some(&json!("$0.01"))), Some(0.01));
        assert_eq!(parse_price(Some(&json!("0.02"))), Some(0.02));
        // Atomic USDC, no decimal point and far too large to be a call price.
        assert_eq!(parse_price(Some(&json!("10000"))), Some(0.01));
        assert_eq!(parse_price(Some(&json!("not a price"))), None);
        assert_eq!(parse_price(None), None);
    }

    #[test]
    fn local_filter_keeps_matching_entries_only() {
        let offers = vec![
            ExternalOffer {
                resource: "https://a.com/page-to-text".into(),
                host: "a.com".into(),
                description: "Convert a web page into clean text".into(),
                source: "x402engine".into(),
                ..Default::default()
            },
            ExternalOffer {
                resource: "https://b.com/dex-quote".into(),
                host: "b.com".into(),
                description: "Token swap quotes".into(),
                source: "x402engine".into(),
                ..Default::default()
            },
        ];
        let kept = relevant(offers, &["read a web page".to_string()]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].host, "a.com");
    }

    // -- string manifests and live challenges -------------------------------
    //
    // Every document below is the real shape a live provider returned when
    // these dialects were added; the URLs and wording are theirs.

    #[test]
    fn reads_a_manifest_that_lists_resources_as_bare_urls() {
        // Messari and Interzoid publish `{version, resources: [url, ...]}`.
        // The object-only parser read zero offers out of 116 real endpoints.
        let body = json!({
            "version": 1,
            "resources": [
                "https://api.messari.io/ai/v2/chat/completions",
                "https://api.messari.io/news/v1/news/feed"
            ]
        });
        let ctx = ParseCtx::bare("messari");
        let offers = parse_in(Shape::X402Items, &ctx, &body);
        assert_eq!(offers.len(), 2);
        assert_eq!(offers[0].host, "api.messari.io");
        assert_eq!(
            offers[0].resource,
            "https://api.messari.io/ai/v2/chat/completions"
        );
        // No description to be had, so the path stands in for one.
        assert!(offers[0].description.contains("/ai/v2/chat/completions"));
    }

    #[test]
    fn resolves_a_method_and_path_manifest_against_the_source_url() {
        // Nansen, Zerion and Laevitas publish `"GET /v1/wallets/{address}"`:
        // a verb and a path, with the host only implied by where the manifest
        // was fetched from.
        let body = json!({
            "version": 1,
            "resources": ["GET /v1/wallets/{address}/portfolio", "POST /api/v1/agent/expert"]
        });
        let mut ctx = ParseCtx::bare("zerion");
        ctx.url = "https://api.zerion.io/.well-known/x402";
        let offers = parse_in(Shape::X402Items, &ctx, &body);
        assert_eq!(offers.len(), 2);
        assert_eq!(
            offers[0].resource,
            "https://api.zerion.io/v1/wallets/{address}/portfolio"
        );
        assert_eq!(offers[0].method.as_deref(), Some("GET"));
        assert_eq!(offers[1].method.as_deref(), Some("POST"));
    }

    #[test]
    fn a_relative_manifest_yields_nothing_without_a_source_url() {
        // Better an empty source than an offer pointing at a path no agent can
        // resolve to a host.
        let body = json!({ "resources": ["GET /v1/wallets"] });
        assert!(parse_in(Shape::X402Items, &ParseCtx::bare("x"), &body).is_empty());
    }

    #[test]
    fn reads_a_live_402_challenge_as_one_offer() {
        // CoinGecko's challenge, verbatim in structure.
        let body = json!({
            "x402Version": 2,
            "resource": {
                "url": "https://pro-api.coingecko.com/api/v3/x402/simple/price",
                "description": "Query prices & market data of coins listed on CoinGecko.",
                "mimeType": "application/json"
            },
            "accepts": [{
                "scheme": "exact",
                "network": "eip155:8453",
                "amount": "10000",
                "payTo": "0x110cdBba7FE6434Ec4CE3464CC523942ad6Fb784"
            }],
            "extensions": {"bazaar": {
                "info": {"input": {"type": "http", "method": "GET"}, "output": {"type": "json"}},
                "schema": {"properties": {"input": {"type": "object"}}}
            }}
        });
        let mut ctx = ParseCtx::bare("coingecko");
        ctx.url = "https://pro-api.coingecko.com/api/v3/x402/simple/price?vs_currencies=usd";
        let offers = parse_in(Shape::X402Challenge, &ctx, &body);
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].host, "pro-api.coingecko.com");
        assert_eq!(offers[0].price_usd, Some(0.01));
        assert_eq!(offers[0].network.as_deref(), Some("eip155:8453"));
        assert_eq!(offers[0].method.as_deref(), Some("GET"));
        assert!(offers[0].description.starts_with("Query prices"));
        // The bazaar block is the same fragment an index entry carries.
        assert!(offers[0].input_schema.is_some());
    }

    #[test]
    fn a_challenge_naming_an_unreachable_resource_falls_back_to_the_probed_url() {
        // The Graph answers with an internal hostname over plain http. Indexing
        // that would publish an endpoint no agent can reach.
        let body = json!({
            "x402Version": 2,
            "error": "Payment-Signature header is required",
            "resource": {"url": "http://mainnet-thegraph-arbitrum-02-eu-west3.thegraph.com/x"},
            "accepts": [{"network": "eip155:8453", "amount": "10000"}]
        });
        let mut ctx = ParseCtx::bare("thegraph");
        ctx.url = "https://gateway.thegraph.com/api/x402/subgraphs/id/5zvR";
        let offers = parse_in(Shape::X402Challenge, &ctx, &body);
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].host, "gateway.thegraph.com");
        // "Payment required" describes every one of these and none of them.
        assert!(!offers[0].description.to_lowercase().contains("payment"));
    }

    #[test]
    fn a_base64_challenge_header_is_the_document_when_the_body_is_empty() {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine as _;
        let challenge = json!({"x402Version": 2, "accepts": [{"amount": "10000"}]});
        let encoded = STANDARD.encode(challenge.to_string());
        let decoded = decode_challenge(&encoded).expect("header decodes");
        assert_eq!(decoded["x402Version"], 2);
        // Body wins only when it actually carries the challenge.
        let thin = json!({"error": "Payment required"});
        let picked = best_document(Some(thin), Some(decoded.clone()), true).unwrap();
        assert!(picked.get("accepts").is_some());
        // A non-challenge source never looks at the header.
        assert!(best_document(None, Some(decoded), false).is_none());
    }

    #[test]
    fn the_provider_identity_reaches_every_offer_and_the_matching() {
        let body =
            json!({ "resources": ["https://pro-api.coingecko.com/api/v3/x402/simple/price"] });
        let tags = vec!["market-data".to_string(), "crypto-prices".to_string()];
        let ctx = ParseCtx {
            id: "coingecko",
            url: "https://pro-api.coingecko.com/api/v3/x402/simple/price",
            provider: Some("CoinGecko"),
            provider_logo: Some("https://www.coingecko.com/favicon.ico"),
            category: Some("Finance"),
            tags: &tags,
        };
        let offers = parse_in(Shape::X402Items, &ctx, &body);
        assert_eq!(offers[0].provider.as_deref(), Some("CoinGecko"));
        assert_eq!(offers[0].category.as_deref(), Some("Finance"));
        assert!(offers[0].provider_logo.is_some());
        // The path says "simple/price"; only the tags say "market data", and a
        // search for that has to reach it.
        let kept = relevant(offers, &["market data".to_string()]);
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn a_source_reads_with_its_own_verb() {
        assert_eq!(normalise_method(Some("post")), "POST");
        assert_eq!(normalise_method(None), "GET");
        // A typo must not drop the source out of the sweep.
        assert_eq!(normalise_method(Some("GTE")), "GET");
    }

    #[test]
    fn a_402_with_no_payment_options_is_a_wall_not_an_offer() {
        // AgentMail's conventional URL answers exactly this.
        let body = json!({"error": "Payment required"});
        let mut ctx = ParseCtx::bare("agentmail");
        ctx.url = "https://x402.api.agentmail.to/.well-known/x402";
        assert!(parse_in(Shape::X402Challenge, &ctx, &body).is_empty());
    }

    /// Every provider from the import list, read live with the verb and dialect
    /// the importer would give it, through the same document-selection and
    /// parsing code the sweep uses.
    ///
    /// Ignored by default — it makes twenty outbound requests to other people's
    /// servers and its result depends on their uptime, so it must never run in
    /// CI. It is here because "does the sweep see these providers" is only
    /// truthfully answerable against the real endpoints:
    ///
    ///     cargo test --lib live_providers -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "hits twenty third-party endpoints; run explicitly"]
    async fn live_providers_yield_offers() {
        // name, verb, url, shape
        const PROVIDERS: &[(&str, &str, &str, Shape)] = &[
            ("CoinGecko", "GET", "https://pro-api.coingecko.com/api/v3/x402/simple/price?vs_currencies=usd&symbols=btc,eth", Shape::X402Challenge),
            ("CoinMarketCap", "GET", "https://pro-api.coinmarketcap.com/x402/v3/cryptocurrency/quotes/latest?id=1", Shape::X402Challenge),
            ("Messari", "GET", "https://api.messari.io/.well-known/x402", Shape::X402Items),
            ("Exa", "POST", "https://api.exa.ai/search", Shape::X402Challenge),
            ("Tavily", "POST", "https://x402.tavily.com/search", Shape::X402Challenge),
            ("Nansen", "GET", "https://api.nansen.ai/.well-known/x402", Shape::X402Items),
            ("Apify", "POST", "https://api.apify.com/v2/actors/apify~website-content-crawler/run-sync-get-dataset-items", Shape::X402Challenge),
            ("Glassnode", "GET", "https://x402.glassnode.com/v1/metrics/market/price_usd_close?a=BTC", Shape::X402Challenge),
            ("QuickNode", "GET", "https://x402.quicknode.com/.well-known/x402", Shape::X402Challenge),
            ("Browserbase", "POST", "https://x402.browserbase.com/browser/session/create", Shape::X402Challenge),
            ("Zerion", "GET", "https://api.zerion.io/.well-known/x402", Shape::X402Items),
            ("The Graph", "POST", "https://gateway.thegraph.com/api/x402/subgraphs/id/5zvR82QoaXYFyDEKLZ9t6v9adgnptxYpKpSbxtgVENFV", Shape::X402Challenge),
            // Answers 402 with a bare {"error":"Payment required"} and no
            // accepts — a wall, not an offer. Expected to yield nothing; the
            // real challenge is on its API paths, e.g. /v0/inboxes.
            ("AgentMail", "GET", "https://x402.api.agentmail.to/.well-known/x402", Shape::X402Challenge),
            ("OpenWeb Ninja", "GET", "https://x402.openwebninja.com/.well-known/x402", Shape::X402Items),
            ("Laevitas", "GET", "https://apiv2.laevitas.ch/.well-known/x402", Shape::X402Items),
            ("Interzoid", "GET", "https://api.interzoid.com/.well-known/x402", Shape::X402Items),
            ("Stratalize", "GET", "https://www.stratalize.com/.well-known/x402", Shape::X402Items),
        ];

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .expect("client");

        let mut working = 0usize;
        let mut total_offers = 0usize;
        let mut dead: Vec<&str> = Vec::new();

        for (name, verb, url, shape) in PROVIDERS {
            let mut request = http
                .request(http_method(verb), *url)
                .header("Accept", "application/json");
            if *verb != "GET" {
                request = request
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body("{}");
            }
            let Ok(response) = request.send().await else {
                dead.push(name);
                continue;
            };
            let status = response.status();
            let from_header = challenge_from_header(response.headers());
            let bytes = response.bytes().await.unwrap_or_default();
            let from_body = serde_json::from_slice::<Value>(&bytes).ok();
            let Some(document) = best_document(from_body, from_header, shape.expects_challenge())
            else {
                dead.push(name);
                continue;
            };

            let mut ctx = ParseCtx::bare(name);
            ctx.url = url;
            let offers = parse_in(*shape, &ctx, &document);
            let usable = offers
                .iter()
                .filter(|o| {
                    quality_gate(
                        &o.resource,
                        &o.host,
                        &o.description,
                        o.price_usd,
                        o.network.as_deref(),
                    )
                    .is_ok()
                })
                .count();

            println!(
                "{name:<16} {verb:<5} HTTP {} {:>5} bytes -> {usable} usable offers  e.g. {}",
                status.as_u16(),
                bytes.len(),
                offers.first().map(|o| o.resource.as_str()).unwrap_or("—"),
            );

            if usable > 0 {
                working += 1;
                total_offers += usable;
            } else {
                dead.push(name);
            }
        }

        println!(
            "\n{working}/{} providers yielded {total_offers} usable offers",
            PROVIDERS.len()
        );
        if !dead.is_empty() {
            println!("nothing from: {}", dead.join(", "));
        }

        // Before this work the same list yielded two working sources and 244
        // offers. These floors are deliberately well under what was measured,
        // so somebody else's outage is a smaller signal than a real regression.
        assert!(
            working >= 12,
            "only {working} of {} providers yielded offers",
            PROVIDERS.len()
        );
        assert!(
            dead.contains(&"AgentMail"),
            "a 402 with no accepts is a wall, not an offer"
        );
        assert!(total_offers >= 400, "only {total_offers} offers in total");
    }

    // -- the source list ----------------------------------------------------

    fn row(id: &str, kind: &str, url: &str, shape: &str) -> SourceRow {
        SourceRow {
            id: id.to_string(),
            fetch_kind: kind.to_string(),
            url: url.to_string(),
            shape: shape.to_string(),
            note: Some(format!("{id} note")),
            ..Default::default()
        }
    }

    #[test]
    fn the_source_list_survives_the_round_trip_through_its_cache_form() {
        // What `active_sources` writes to the cache must read back as the same
        // list, or a warm cache would sweep a different set than a cold one.
        let sources = sources_from_rows(&[
            row(
                "queried-one",
                "queried",
                "https://i.test/search?q={query}",
                "x402-items",
            ),
            row(
                "indexed-one",
                "indexed",
                "https://j.test/all",
                "anchor-manifest",
            ),
            row(
                "indexed-two",
                "indexed",
                "https://k.test/all",
                "x402-direct",
            ),
        ])
        .expect("three usable rows");
        let back = sources_from_json(&sources_to_json(&sources)).expect("readable");
        assert_eq!(back.len(), sources.len());
        for (a, b) in sources.iter().zip(back.iter()) {
            assert_eq!(a.id, b.id);
            assert_eq!(a.shape, b.shape);
            assert_eq!(a.note, b.note);
            assert_eq!(a.is_browsable(), b.is_browsable());
            let url = |s: &Source| match &s.fetch {
                Fetch::Queried { template } => template.clone(),
                Fetch::Indexed { url } => url.clone(),
            };
            assert_eq!(url(a), url(b));
        }
        // Nothing readable in the cache is a miss, not an empty sweep.
        assert!(sources_from_json(&json!([])).is_none());
        assert!(sources_from_json(&json!({"not": "a list"})).is_none());
    }

    #[test]
    fn rows_with_an_unknown_shape_or_kind_are_skipped_not_fatal() {
        let rows = vec![
            row(
                "good",
                "queried",
                "https://i.test/s?q={query}",
                "x402-items",
            ),
            row("bad-shape", "indexed", "https://i.test/all", "x402-itemz"),
            row("bad-kind", "streamed", "https://i.test/all", "x402-items"),
            row(
                "also-good",
                "indexed",
                "https://j.test/all",
                "anchor-manifest",
            ),
        ];
        let sources = sources_from_rows(&rows).expect("two usable rows");
        let ids: Vec<&str> = sources.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["good", "also-good"]);
        assert!(!sources[0].is_browsable());
        assert!(sources[1].is_browsable());
        assert_eq!(sources[0].note.as_deref(), Some("good note"));
    }

    #[test]
    fn an_empty_or_wholly_broken_table_sweeps_nothing_rather_than_something_else() {
        // `None` is what `active_sources` turns into an empty sweep. There is
        // no built-in list to fall back to on purpose: what discovery reaches
        // is exactly what an admin put in this table, and a database problem
        // must not resurrect a source somebody removed.
        assert!(sources_from_rows(&[]).is_none());
        assert!(sources_from_rows(&[row("x", "indexed", "https://i.test/all", "nope")]).is_none());
    }

    // -- the pure sweep core ------------------------------------------------

    fn ext(source: &str, resource: &str, description: &str, price: Option<f64>) -> ExternalOffer {
        ExternalOffer {
            resource: resource.to_string(),
            host: host_of(resource).unwrap(),
            description: description.to_string(),
            source: source.to_string(),
            price_usd: price,
            ..Default::default()
        }
    }

    fn read(source: &str, outcome: ReadOutcome, offers: Vec<ExternalOffer>, ms: u64) -> SourceRead {
        SourceRead {
            source_id: source.to_string(),
            outcome,
            offers,
            ms,
        }
    }

    /// One curated seller, in the map shape the sweep consumes.
    fn curated(
        source: &str,
        key: &str,
        entry: crate::db::ProviderCuration,
    ) -> crate::db::CurationMap {
        let mut map = crate::db::CurationMap::new();
        map.entry(source.to_string())
            .or_default()
            .insert(key.to_string(), entry);
        map
    }

    #[test]
    fn a_hidden_seller_never_merges() {
        let curation = curated(
            "s",
            "a.test",
            crate::db::ProviderCuration {
                hidden: true,
                ..Default::default()
            },
        );
        let swept = merge_sweep(
            vec![read(
                "s",
                ReadOutcome::Fresh,
                vec![
                    ext("s", "https://a.test/x", "weather data", Some(0.01)),
                    ext("s", "https://b.test/x", "weather data", Some(0.01)),
                ],
                1,
            )],
            &["s".to_string()],
            &["weather".to_string()],
            &[],
            MAX_RETURNED,
            &curation,
        );
        assert_eq!(swept.offers.len(), 1, "the hidden seller's offer is gone");
        assert_eq!(swept.offers[0].host, "b.test");
    }

    #[test]
    fn curated_tags_ship_on_the_offer_and_score_like_published_ones() {
        // The admin tagged a.test "weather". Its offer says nothing about
        // weather itself — the tag is the only reason it can answer this
        // query, and it must both rank on it and carry it out in the
        // response, where the app's own semantic matching reads tags too.
        let curation = curated(
            "s",
            "a.test",
            crate::db::ProviderCuration {
                curated_tags: vec!["weather".to_string()],
                ..Default::default()
            },
        );
        let swept = merge_sweep(
            vec![read(
                "s",
                ReadOutcome::Fresh,
                vec![
                    ext("s", "https://b.test/x", "generic data", Some(0.01)),
                    ext("s", "https://a.test/x", "generic data", Some(0.01)),
                ],
                1,
            )],
            &["s".to_string()],
            &["weather".to_string()],
            &[],
            MAX_RETURNED,
            &curation,
        );
        assert_eq!(swept.offers[0].host, "a.test", "the curated tag ranked it");
        assert!(
            swept.offers[0].tags.iter().any(|t| t == "weather"),
            "and the tag travels out on the offer"
        );
    }

    #[test]
    fn preferred_breaks_ties_and_never_outranks_relevance() {
        let prefer_b = curated(
            "s",
            "b.test",
            crate::db::ProviderCuration {
                preferred: true,
                ..Default::default()
            },
        );

        // Preference cannot beat the caller's own words: a.test answers the
        // query, b.test does not, and no flag changes that order.
        let mut offers = vec![
            ext("s", "https://b.test/x", "generic data", Some(0.001)),
            ext("s", "https://a.test/x", "weather forecast", Some(0.05)),
        ];
        rank_offers(&mut offers, &["weather".to_string()], &prefer_b);
        assert_eq!(offers[0].host, "a.test");

        // Between two offers the score cannot separate, preference decides —
        // above price, which is the whole point of preferring a seller.
        let mut tied = vec![
            ext("s", "https://a.test/x", "weather forecast", Some(0.001)),
            ext("s", "https://b.test/x", "weather forecast", Some(0.05)),
        ];
        rank_offers(&mut tied, &["weather".to_string()], &prefer_b);
        assert_eq!(offers.len(), 2);
        assert_eq!(tied[0].host, "b.test");
    }

    #[test]
    fn the_schema_bearing_copy_of_a_duplicate_wins_whichever_arrived_first() {
        // The deep index drops the provider-declared schemas so 49,000 entries
        // fit in memory (`crate::source_index`); a queried hit keeps them. The
        // two lanes therefore carry DIFFERENT copies of the same resource, and
        // before this the winner was whichever task finished first — so an
        // agent was told how to call an endpoint, or not, by task scheduling.
        let lean = ext("payai", "https://a.test/x", "weather forecast", Some(0.01));
        let rich = ExternalOffer {
            input_schema: Some(json!({"type": "object"})),
            method: Some("POST".to_string()),
            ..ext("bazaar", "https://a.test/x", "weather forecast", Some(0.01))
        };

        for (first, second, order_note) in [
            (lean.clone(), rich.clone(), "lean first"),
            (rich.clone(), lean.clone(), "rich first"),
        ] {
            let swept = merge_sweep(
                vec![
                    read("payai", ReadOutcome::Indexed, vec![first], 1),
                    read("bazaar", ReadOutcome::Fresh, vec![second], 90),
                ],
                &["payai".to_string(), "bazaar".to_string()],
                &["weather".to_string()],
                &[],
                MAX_RETURNED,
                &Default::default(),
            );
            assert_eq!(swept.offers.len(), 1, "{order_note}: still one offer");
            assert!(
                swept.offers[0].input_schema.is_some(),
                "{order_note}: the declared schema must survive the dedupe"
            );
            assert_eq!(
                swept.offers[0].method.as_deref(),
                Some("POST"),
                "{order_note}: and so must what it implies"
            );
        }
    }

    #[test]
    fn a_source_answered_from_the_deep_index_says_so_rather_than_cached() {
        // "cached" means the first page again without paying for it. "indexed"
        // means the whole catalogue. Reporting the second as the first is how
        // a search of 26,684 entries and a search of 100 became the same line.
        let swept = merge_sweep(
            vec![read(
                "payai",
                ReadOutcome::Indexed,
                vec![ext("payai", "https://a.test/x", "weather", Some(0.01))],
                2,
            )],
            &["payai".to_string()],
            &["weather".to_string()],
            &[],
            MAX_RETURNED,
            &Default::default(),
        );
        assert_eq!(swept.sources[0].status, SourceOutcome::Indexed);
        assert_eq!(swept.sources[0].status.as_str(), "indexed");
        assert!(!swept.sources[0].status.failed(), "it answered");
    }

    #[test]
    fn every_swept_source_gets_a_status_line_and_failures_drop_nothing_else() {
        let order = vec![
            "bazaar".to_string(),
            "thirdweb".to_string(),
            "payai".to_string(),
            "anchor".to_string(),
            "never-heard-from".to_string(),
        ];
        let reads = vec![
            read(
                "bazaar",
                ReadOutcome::Fresh,
                vec![ext(
                    "bazaar",
                    "https://a.test/x",
                    "weather forecast",
                    Some(0.01),
                )],
                120,
            ),
            // Three phrasings: one answered, one timed out. The source still
            // counts as answering.
            read("bazaar", ReadOutcome::Timeout, vec![], 2500),
            read("thirdweb", ReadOutcome::Timeout, vec![], 2500),
            read("payai", ReadOutcome::Error, vec![], 40),
            read("anchor", ReadOutcome::Cached, vec![], 1),
        ];
        let variants = vec!["weather forecast".to_string()];
        let swept = merge_sweep(
            reads,
            &order,
            &variants,
            &[],
            MAX_RETURNED,
            &Default::default(),
        );

        assert_eq!(swept.offers.len(), 1, "the good source's offer survives");
        assert_eq!(swept.sources.len(), 5, "one line per swept source");
        let by_id = |id: &str| swept.sources.iter().find(|s| s.id == id).unwrap();
        assert_eq!(by_id("bazaar").status, SourceOutcome::Ok);
        assert_eq!(by_id("bazaar").offers, 1);
        assert_eq!(
            by_id("bazaar").ms,
            2500,
            "the slowest read is the one reported"
        );
        assert_eq!(by_id("thirdweb").status, SourceOutcome::Timeout);
        assert_eq!(by_id("payai").status, SourceOutcome::Error);
        assert_eq!(by_id("anchor").status, SourceOutcome::Empty);
        assert_eq!(by_id("never-heard-from").status, SourceOutcome::Timeout);
        assert_eq!(swept.sources_failed(), 3);

        let json = swept.sources_json();
        assert_eq!(json[0]["id"], "bazaar");
        assert_eq!(json[0]["status"], "ok");
        assert_eq!(json[1]["status"], "timeout");
    }

    #[test]
    fn a_source_answered_entirely_from_cache_says_so() {
        let order = vec!["payai".to_string()];
        let offers = vec![ext("payai", "https://p.test/q", "weather", Some(0.01))];
        let swept = merge_sweep(
            vec![read("payai", ReadOutcome::Cached, offers.clone(), 0)],
            &order,
            &["weather".to_string()],
            &[],
            MAX_RETURNED,
            &Default::default(),
        );
        assert_eq!(swept.sources[0].status, SourceOutcome::Cached);

        // One fresh answer among cached ones is a live answer.
        let swept = merge_sweep(
            vec![
                read("payai", ReadOutcome::Cached, offers.clone(), 0),
                read(
                    "payai",
                    ReadOutcome::Fresh,
                    vec![ext(
                        "payai",
                        "https://p.test/r",
                        "weather radar",
                        Some(0.02),
                    )],
                    90,
                ),
            ],
            &order,
            &["weather".to_string()],
            &[],
            MAX_RETURNED,
            &Default::default(),
        );
        assert_eq!(swept.sources[0].status, SourceOutcome::Ok);
        assert_eq!(swept.sources[0].offers, 2);
    }

    #[test]
    fn the_same_resource_from_two_indexes_is_one_offer_credited_once() {
        let order = vec!["bazaar".to_string(), "thirdweb".to_string()];
        let swept = merge_sweep(
            vec![
                read(
                    "bazaar",
                    ReadOutcome::Fresh,
                    vec![ext("bazaar", "https://a.test/x", "weather", Some(0.01))],
                    10,
                ),
                read(
                    "thirdweb",
                    ReadOutcome::Fresh,
                    vec![
                        ext("thirdweb", "https://a.test/x", "weather", Some(0.01)),
                        ext("thirdweb", "https://b.test/y", "weather", Some(0.02)),
                    ],
                    10,
                ),
            ],
            &order,
            &["weather".to_string()],
            &[],
            MAX_RETURNED,
            &Default::default(),
        );
        assert_eq!(swept.offers.len(), 2);
        assert_eq!(swept.sources[0].offers, 1);
        // thirdweb's copy of a.test/x was a duplicate; only b.test/y counts.
        assert_eq!(swept.sources[1].offers, 1);
        assert_eq!(swept.sources[1].status, SourceOutcome::Ok);
    }

    #[test]
    fn a_host_already_in_the_catalogue_is_left_to_the_catalogue() {
        let order = vec!["bazaar".to_string()];
        let swept = merge_sweep(
            vec![read(
                "bazaar",
                ReadOutcome::Fresh,
                vec![ext("bazaar", "https://known.test/x", "weather", Some(0.01))],
                10,
            )],
            &order,
            &["weather".to_string()],
            &["known.test".to_string()],
            MAX_RETURNED,
            &Default::default(),
        );
        assert!(swept.offers.is_empty());
        // The source answered; its answer was simply already on the shelf.
        assert_eq!(swept.sources[0].status, SourceOutcome::Ok);
        assert_eq!(swept.sources[0].offers, 0);
    }

    /// The ordering this module used to sort with: the score derived inside
    /// the comparator, so the vocabulary was re-tokenised twice per
    /// comparison. Kept in the test module so the claim is measured against
    /// what it replaced.
    fn rank_the_old_way(offers: &mut [ExternalOffer], variants: &[String]) {
        fn score(offer: &ExternalOffer, variants: &[String]) -> usize {
            let haystack = format!("{} {}", offer.resource, offer.description).to_ascii_lowercase();
            let mut score = 0;
            for variant in variants {
                let phrase = variant.trim().to_ascii_lowercase();
                if phrase.len() >= 6 && haystack.contains(&phrase) {
                    score += 3;
                }
                for token in tokens(&phrase) {
                    if haystack.contains(&token) {
                        score += 1;
                    }
                }
            }
            score
        }
        offers.sort_by(|a, b| {
            score(b, variants)
                .cmp(&score(a, variants))
                .then(match (a.price_usd, b.price_usd) {
                    (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                })
        });
    }

    #[test]
    #[ignore = "a measurement, not an assertion; run with --ignored --nocapture"]
    fn bench_ranking() {
        use std::time::Instant;

        // What a merged sweep actually holds before the cut: up to
        // MAX_MATCHES_PER_SOURCE from each of several indexes.
        let offers: Vec<ExternalOffer> = (0..2_800)
            .map(|i| {
                ext(
                    "s",
                    &format!("https://seller{}.test/v1/quote/e{i}", i % 400),
                    "market data endpoint for analyst estimates and earnings",
                    Some((i % 97) as f64 / 1000.0),
                )
            })
            .collect();
        let variants: Vec<String> = (0..MAX_INDEX_TERMS)
            .map(|i| {
                [
                    "market data",
                    "analyst estimates",
                    "earnings",
                    "stock quote",
                    "forex",
                ][i % 5]
                    .to_string()
            })
            .collect();

        let mut old = offers.clone();
        let started = Instant::now();
        rank_the_old_way(&mut old, &variants);
        let before = started.elapsed();

        let mut new = offers.clone();
        let started = Instant::now();
        rank_offers(&mut new, &variants, &Default::default());
        let after = started.elapsed();

        let mut spread = new.clone();
        let started = Instant::now();
        spread_providers(&mut spread);
        let deal = started.elapsed();

        println!(
            "\n2,800 merged offers / {} phrasings\n  score inside the comparator  {before:?}  <- what this replaced\n  score cached per offer       {after:?}\n  dealing out across sellers   {deal:?}",
            variants.len()
        );
    }

    #[test]
    fn an_offer_found_through_its_tags_can_be_ranked_by_them_too() {
        // The retrieval haystack has always included the provider name,
        // category and tags — a one-line 402 challenge is findable only
        // through them. The ranking haystack did not, so exactly those offers
        // scored zero, sorted behind everything, and fell off the truncation.
        let mut tagged = ext("s", "https://a.test/e1", "endpoint", Some(0.02));
        tagged.category = Some("Finance".to_string());
        tagged.tags = vec!["analyst-estimates".to_string(), "earnings".to_string()];
        let mut offers = vec![
            ext("s", "https://b.test/e2", "reverse a string", Some(0.0001)),
            tagged,
        ];
        rank_offers(
            &mut offers,
            &["analyst estimates".to_string()],
            &Default::default(),
        );
        assert_eq!(
            offers[0].resource, "https://a.test/e1",
            "the tags it was found through have to count in the ordering"
        );
    }

    #[test]
    fn one_seller_does_not_own_the_whole_page() {
        // Ranking is a per-offer judgement; a page is a judgement about a set.
        // A seller with forty good endpoints used to take every slot, and an
        // agent asking what the ECOSYSTEM can do was shown one catalogue.
        let mut offers: Vec<ExternalOffer> = (0..40)
            .map(|i| {
                ext(
                    "s",
                    &format!("https://big.test/{i}"),
                    "weather forecast",
                    Some(0.01),
                )
            })
            .collect();
        offers.push(ext(
            "s",
            "https://small.test/f",
            "weather forecast",
            Some(0.01),
        ));
        offers.push(ext(
            "s",
            "https://tiny.test/f",
            "weather forecast",
            Some(0.01),
        ));

        let swept = merge_sweep(
            vec![read("s", ReadOutcome::Indexed, offers, 1)],
            &["s".to_string()],
            &["weather forecast".to_string()],
            &[],
            MAX_RETURNED,
            &Default::default(),
        );
        let sellers: HashSet<&str> = swept.offers.iter().map(|o| o.host.as_str()).collect();
        assert_eq!(
            sellers.len(),
            3,
            "every seller reached the page: {sellers:?}"
        );
        // And near the front, not at position twenty-five.
        let front: HashSet<&str> = swept
            .offers
            .iter()
            .take(6)
            .map(|o| o.host.as_str())
            .collect();
        assert!(front.contains("small.test"), "{front:?}");
        assert!(front.contains("tiny.test"), "{front:?}");
    }

    #[test]
    fn spreading_never_promotes_an_offer_above_a_better_one_from_its_own_seller() {
        // The guarantee that makes the deal safe: it reorders BETWEEN sellers
        // and never within one.
        let mut offers = vec![
            ext(
                "s",
                "https://a.test/best",
                "weather forecast radar",
                Some(0.01),
            ),
            ext("s", "https://b.test/x", "weather forecast", Some(0.01)),
            ext("s", "https://a.test/worse", "weather", Some(0.01)),
        ];
        rank_offers(
            &mut offers,
            &["weather forecast radar".to_string()],
            &Default::default(),
        );
        spread_providers(&mut offers);
        let a: Vec<&str> = offers
            .iter()
            .filter(|o| o.host == "a.test")
            .map(|o| o.resource.as_str())
            .collect();
        assert_eq!(a, ["https://a.test/best", "https://a.test/worse"]);
    }

    #[test]
    fn subdomains_of_one_seller_share_a_lane() {
        // pro-api and api are one seller, and a page that treats them as two
        // is a page that shows one seller twice while a second seller waits.
        let mut offers = vec![
            ext(
                "s",
                "https://api.big.test/a",
                "weather forecast",
                Some(0.01),
            ),
            ext(
                "s",
                "https://pro-api.big.test/b",
                "weather forecast",
                Some(0.01),
            ),
            ext(
                "s",
                "https://api.big.test/c",
                "weather forecast",
                Some(0.01),
            ),
            ext("s", "https://other.test/d", "weather forecast", Some(0.01)),
        ];
        spread_providers(&mut offers);
        assert_eq!(
            offers[2].host,
            "other.test",
            "the second seller gets its turn after one run, not after three: {:?}",
            offers
                .iter()
                .map(|o| o.resource.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn relevance_orders_phrase_over_tokens_then_price_then_unpriced_last() {
        let variants = vec!["analyst estimates".to_string()];
        let mut offers = vec![
            ext(
                "s",
                "https://a.test/unpriced",
                "analyst estimates for any ticker",
                None,
            ),
            ext(
                "s",
                "https://a.test/tokens",
                "estimates from every analyst we track",
                Some(0.001),
            ),
            ext(
                "s",
                "https://a.test/phrase-dear",
                "analyst estimates, consensus",
                Some(0.05),
            ),
            ext(
                "s",
                "https://a.test/phrase-cheap",
                "analyst estimates, consensus",
                Some(0.01),
            ),
            ext(
                "s",
                "https://a.test/reverse",
                "reverse a string",
                Some(0.0001),
            ),
        ];
        rank_offers(&mut offers, &variants, &Default::default());
        let order: Vec<&str> = offers.iter().map(|o| o.resource.as_str()).collect();
        assert_eq!(
            order,
            [
                // Phrase matches first, cheapest of them first...
                "https://a.test/phrase-cheap",
                "https://a.test/phrase-dear",
                // ...then the unpriced phrase match, after every priced one in
                // its tier...
                "https://a.test/unpriced",
                // ...then the token-only match, and the irrelevant one last,
                // however cheap.
                "https://a.test/tokens",
                "https://a.test/reverse",
            ]
        );
    }

    #[test]
    fn the_merged_list_is_cut_to_the_limit_and_the_limit_to_its_ceiling() {
        let order = vec!["s".to_string()];
        let offers: Vec<ExternalOffer> = (0..150)
            .map(|i| ext("s", &format!("https://a.test/{i}"), "weather", Some(0.01)))
            .collect();
        let swept = merge_sweep(
            vec![read("s", ReadOutcome::Fresh, offers.clone(), 1)],
            &order,
            &["weather".to_string()],
            &[],
            1_000,
            &Default::default(),
        );
        assert_eq!(swept.offers.len(), MAX_LIMIT);
        assert_eq!(swept.sources[0].offers, 150, "credit counts before the cut");

        let swept = merge_sweep(
            vec![read("s", ReadOutcome::Fresh, offers, 1)],
            &order,
            &["weather".to_string()],
            &[],
            MAX_RETURNED,
            &Default::default(),
        );
        assert_eq!(swept.offers.len(), MAX_RETURNED);
    }

    #[test]
    fn only_the_first_few_phrasings_reach_a_source() {
        let queries: Vec<String> = ["  a ", "", "b", "c", "d"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let flat = [NeedTerms::flat(&queries)];
        assert_eq!(allocate_queried(&flat), ["a", "b", "c"]);
        assert!(allocate_queried(&[NeedTerms::flat(&["  ".to_string()])]).is_empty());
    }

    // -- routing a parsed job's vocabulary -----------------------------------

    fn terms(name: &str, queried: &[&str], index: &[&str]) -> NeedTerms {
        NeedTerms {
            need: name.to_string(),
            queried: queried.iter().map(|s| s.to_string()).collect(),
            index: index.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn every_need_gets_a_queried_slot_before_any_need_gets_two() {
        let needs = vec![
            terms(
                "get the bitcoin price",
                &["bitcoin price", "btc spot", "crypto quote"],
                &[],
            ),
            terms(
                "get news about it",
                &["bitcoin news", "crypto headlines"],
                &[],
            ),
            // The tail: the raw sentence, which matches keyword indexes worst.
            terms("", &["get the bitcoin price and news about it"], &[]),
        ];
        assert_eq!(
            allocate_queried(&needs),
            ["bitcoin price", "bitcoin news", "btc spot"],
            "the second need is asked before the first need's second phrasing, and the tail never takes a named need's slot"
        );
    }

    #[test]
    fn a_four_need_job_buys_one_slot_per_need() {
        let needs = vec![
            terms("a", &["alpha", "alef"], &[]),
            terms("b", &["beta"], &[]),
            terms("c", &["gamma"], &[]),
            terms("d", &["delta"], &[]),
        ];
        assert_eq!(
            allocate_queried(&needs),
            ["alpha", "beta", "gamma", "delta"]
        );
    }

    #[test]
    fn the_indexed_lane_gets_the_whole_vocabulary_the_queried_lane_rations() {
        let needs = vec![terms(
            "dutch news",
            &["netherlands news", "dutch headlines"],
            &["media", "journalism"],
        )];
        assert_eq!(
            allocate_queried(&needs),
            ["netherlands news", "dutch headlines"],
            "index-only labels never cost an HTTP call"
        );
        assert_eq!(
            index_union(&needs),
            ["netherlands news", "dutch headlines", "media", "journalism"]
        );
    }

    #[test]
    fn a_vocabulary_with_only_index_terms_still_sweeps_the_queried_lane() {
        let needs = vec![terms("news", &[], &["dutch news", "headlines"])];
        assert_eq!(allocate_queried(&needs), ["dutch news", "headlines"]);
    }

    #[test]
    fn coverage_names_the_need_the_merged_list_left_unanswered() {
        let offers = vec![
            ext(
                "bazaar",
                "https://a.test/spot",
                "bitcoin spot price feed",
                Some(0.01),
            ),
            ext(
                "bazaar",
                "https://a.test/ohlcv",
                "bitcoin price candles",
                Some(0.02),
            ),
        ];
        let needs = vec![
            terms("get the bitcoin price", &["bitcoin price"], &[]),
            terms(
                "get the dutch weather",
                &["netherlands weather", "weather forecast"],
                &[],
            ),
        ];
        let coverage = coverage_for(&offers, &needs);
        assert_eq!(coverage.len(), 2);
        assert_eq!(coverage[0].need, "get the bitcoin price");
        assert_eq!(coverage[0].offers, 2);
        assert_eq!(coverage[1].offers, 0, "an unanswered need says so");

        // One named need publishes no coverage: the offer list is the answer.
        assert!(coverage_for(&offers, &needs[..1]).is_empty());
    }
}
