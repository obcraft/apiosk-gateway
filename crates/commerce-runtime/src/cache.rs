use anyhow::{anyhow, Result};
use moka::future::Cache;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

use crate::db::{
    Api, ApiInfo, Database, Endpoint, EndpointDocsBundle, ProviderStats, ProviderSummary,
};

/// Cache key for endpoint lookups
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub struct CacheKey {
    pub api_id: Uuid,
    pub method: String,
    pub path: String,
}

/// In-memory cache for API and endpoint configurations
///
/// Uses TTL-based expiration to balance freshness with performance.
/// Cache misses fall through to database queries.
pub struct EndpointCache {
    api_cache: Cache<String, Option<Api>>,
    endpoint_cache: Cache<CacheKey, Option<Endpoint>>,
    /// Parallel cache holding the endpoint+api documentation bundle used to
    /// enrich the outer 402 body with Bazaar discovery metadata. Kept
    /// separate from `endpoint_cache` so the hot path keeps reading the
    /// lean `Endpoint` row and the heavier docs fetch only fires when the
    /// gateway is about to emit a 402.
    endpoint_docs_cache: Cache<CacheKey, Option<EndpointDocsBundle>>,
    /// The provider directory behind `GET /` and `GET /v1/providers`, keyed by
    /// search term. The home page is the most-hit HTML route (humans and
    /// crawlers), and its query is a JOIN + GROUP BY over every active
    /// listing, so it gets a short TTL rather than running per request.
    provider_cache: Cache<String, Vec<ProviderSummary>>,
    /// Measured latency and success rate per listing, as one entry.
    ///
    /// The query percentile-scans 90 days of request logs (2.4M rows) and took
    /// 5.7s on the search path, where it runs once per request to decorate
    /// offers that are already loaded. It answers the same for every caller and
    /// moves in hours, so it is computed once a minute rather than once a
    /// request.
    provider_stats_cache: Cache<(), std::sync::Arc<HashMap<String, ProviderStats>>>,
    /// Raw Coinbase x402 Bazaar search responses, keyed by query.
    ///
    /// Held as the unfiltered payload rather than as shaped offers, so two
    /// callers that know different sets of catalogued hosts still share one
    /// round trip. Five minutes: the corpus moves in days, and this sits on a
    /// request an agent is waiting on.
    bazaar_cache: Cache<String, serde_json::Value>,
    /// The resolved external source list (`discovery_sources` rows, as the JSON
    /// `crate::sources` serialises them), as one entry.
    ///
    /// Used to live in `bazaar_cache` under a reserved key and so shared its
    /// five-minute life, which meant an admin disabling a broken index kept
    /// seeing it swept for up to five minutes. The list is one cheap indexed
    /// read against a table of a dozen rows, so it gets thirty seconds: short
    /// enough that an enable/disable is live before the admin has finished
    /// checking, long enough that a burst of searches is still one query.
    discovery_sources_cache: Cache<(), serde_json::Value>,
    /// The admin's word on external sellers (`source_providers` curation),
    /// loaded once per TTL rather than once per sweep. Same thirty-second
    /// life as the source list, for the same reason: a provider an admin just
    /// hid drops out of the next sweep before they have finished checking.
    source_provider_curation_cache: Cache<(), std::sync::Arc<crate::db::CurationMap>>,
    /// Model answers, keyed by a hash of the model plus its exact input.
    ///
    /// Shared by the query parser and request binding — the two places the v1
    /// endpoints spend real money per request. Agents retry: the same sentence
    /// arriving twice must not be two model calls. Held as raw JSON so this
    /// module stays ignorant of both callers' types, and keyed on a hash that
    /// already includes the caller's own prefix, so the two never collide. An
    /// hour, because a sentence's meaning does not change.
    query_plan_cache: Cache<String, serde_json::Value>,
    /// Discovery's answer to "who is this", keyed by a HASH of the session
    /// token — never the token itself, which is never stored and never logged.
    ///
    /// `/v1/discover` verifies a browser's session against Supabase to decide
    /// whether it may spend a model call (`crate::parse_budget`). That is one
    /// round trip on an endpoint whose cheap path — a keyword query answered
    /// from the index — is otherwise a few tens of milliseconds, so paying for
    /// it every time means paying most for the requests that cost least.
    ///
    /// SHORT, and short on purpose. Caching an identity means a session that
    /// has since been signed out keeps its answer until the entry expires, and
    /// the honest way to hold that is a window small enough to state plainly:
    /// for up to [`PARSE_IDENTITY_TTL`] after signing out, a search may still
    /// be read by a model against that person's own budget. That is the entire
    /// blast radius — it decides whether a SEARCH is parsed, and returns
    /// nothing the caller could not already see, because `/v1/discover` is
    /// public and answers anonymous callers in full.
    ///
    /// A REJECTION is cached too, and deliberately: `/v1/discover` is public
    /// and needs no credential, so without a negative entry anyone can make the
    /// gateway call the auth server once per request just by inventing a token
    /// — and that auth server is the one the credits path and the Connect
    /// endpoints depend on. See `parse_identity` for which outcomes are stored
    /// and which are not.
    ///
    /// Deliberately NOT shared with `crate::portal_auth`'s other callers. The
    /// credits path and the Connect owner endpoints authenticate the same way
    /// but move money and change configuration; they must keep asking every
    /// time. This is scoped to the one caller where bounded staleness buys
    /// something and risks nothing.
    parse_identity_cache: Cache<String, Option<String>>,
    /// The active-listing catalog behind `/.well-known/x402`, as one entry.
    ///
    /// That query aggregates every endpoint's documentation into jsonb across
    /// the whole catalog, and it is the most expensive read the gateway makes.
    /// It used to run once per discovery read; since the document was
    /// paginated it runs once per PAGE, so a crawler walking 13 pages ran it 13
    /// times. Under a crawler burst it began hitting the statement timeout and
    /// the discovery document — the one surface agents actually read —
    /// intermittently 503'd. Cached, a burst costs one query per minute.
    ///
    /// Read through `try_get_with`, not `get` + `insert`. With a plain
    /// get/insert the TTL expiry was a stampede: the query takes 1.4-2.0s in
    /// production, so every discovery read arriving inside that window missed
    /// and ran its own copy, and a crawler burst put N concurrent 2s queries on
    /// the pool once a minute. That starved the pool the request log writes on
    /// (see `traffic::REQUEST_LOG_TIMEOUT`). Coalesced, one caller refills and
    /// the rest await that same refill.
    catalog_cache: Cache<(), Arc<Vec<ApiInfo>>>,
    /// De hele servable catalogus als één blok, voor het in-geheugen zoeken van
    /// `/v1/ask`. Zes bronnen, ruwweg elfhonderd endpoints: klein genoeg om vast
    /// te houden, groot genoeg dat je hem niet per verzoek wilt ophalen.
    ask_snapshot_cache: Cache<(), crate::ask::Snapshot>,
    /// The built `items[]` for the discovery document — see `get_discovery_items`.
    discovery_items_cache: Cache<(), Arc<Vec<serde_json::Value>>>,
    /// FILTERED catalog reads, keyed by the full parameter set.
    ///
    /// `catalog_cache` above only covers the one unfiltered read behind
    /// `/.well-known/x402`. The same expensive query also backs `/v1/apis`,
    /// `/types/*` and `openapi.json` with caller-supplied filters, and those ran
    /// uncached on every request — while the discovery document advertises
    /// `/v1/apis` to crawlers as `links.catalog`. On 2026-08-03 that combination
    /// put the database into a statement-timeout spiral that took discovery down
    /// with it. Keyed so filtered reads share too, bounded so a crawler walking
    /// random `?q=` values evicts rather than grows.
    catalog_query_cache: Cache<String, Arc<(Vec<ApiInfo>, i64)>>,
    /// Last known-good catalog reads, kept far past the live TTL and read ONLY
    /// when the database refuses to serve a fresh one.
    ///
    /// The catalog query is the gateway's most expensive read and its failure
    /// mode is the statement timeout — a transient, load-shaped failure. With
    /// nothing behind it, one slow minute on the database turned every crawler
    /// hitting `/v1/apis` or `/.well-known/x402` into a 500, which is how the
    /// same incident kept reaching Sentry (2026-07-11, 2026-08-03,
    /// 2026-08-04). A catalog minutes out of date is a far better answer to an
    /// agent than no catalog at all: listings change on the order of days, and
    /// the alternative is a discovery surface that intermittently doesn't exist.
    ///
    /// Cleared by `invalidate_all` along with the live caches, so an explicit
    /// invalidation never leaves a stale answer reachable.
    catalog_query_stale: Cache<String, Arc<(Vec<ApiInfo>, i64)>>,
    /// [`Self::catalog_query_stale`] for the unfiltered discovery catalog.
    catalog_stale: Cache<(), Arc<Vec<ApiInfo>>>,
    /// Upstream responses for endpoints that declare `cache_ttl_seconds`.
    ///
    /// Opt-in per endpoint and off everywhere else. It exists for upstreams
    /// whose rate limit is far below Apiosk's traffic — KVK's open datasets
    /// allow one request per minute per IP, and the gateway is one IP — where
    /// without it the second buyer of the same lookup gets a 429 the first
    /// one caused.
    ///
    /// It changes what the gateway asks upstream, never what it charges: a
    /// cache hit is billed and settled exactly like a miss, because the buyer
    /// is paying for the answer, not for the round trip. Entries hold the
    /// upstream's own bytes, status and content type, keyed by endpoint and
    /// the exact request line; per-entry TTL comes from the endpoint row.
    upstream_response_cache: Cache<String, Arc<CachedUpstreamResponse>>,
}

/// One cached upstream answer: the bytes as they arrived, plus the two
/// headers a replay has to reproduce faithfully.
///
/// `expires_at` is carried on the entry rather than configured on the cache,
/// because the TTL is the ENDPOINT's (an annual-accounts filing is stable for
/// a day; another endpoint might want a minute) and one shared cache serves
/// them all. The cache's own TTL is the outer ceiling.
#[derive(Debug, Clone)]
pub struct CachedUpstreamResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
    expires_at: std::time::Instant,
}

impl CachedUpstreamResponse {
    pub fn new(status: u16, content_type: Option<String>, body: Vec<u8>, ttl: Duration) -> Self {
        Self {
            status,
            content_type,
            body,
            expires_at: std::time::Instant::now() + ttl.min(MAX_UPSTREAM_RESPONSE_TTL),
        }
    }

    fn is_live(&self) -> bool {
        std::time::Instant::now() < self.expires_at
    }
}

/// Ceiling on how long any upstream answer is reused, whatever an endpoint
/// row asks for. A misconfigured TTL should degrade to "stale for a day", not
/// "wrong forever".
pub const MAX_UPSTREAM_RESPONSE_TTL: Duration = Duration::from_secs(86_400);

/// The cache key for one upstream request: the endpoint it resolved to, and
/// the exact path and query that were asked for. The endpoint id is part of
/// the key so two listings can never share an entry, and the raw request line
/// is part of it so `/basisprofielen/1` and `/basisprofielen/2` cannot.
pub fn upstream_cache_key(
    endpoint_id: &uuid::Uuid,
    method: &str,
    path: &str,
    query: &str,
) -> String {
    // Joined with the ASCII unit separator, like `catalog_query_key`: a
    // printable joiner would let one field's content forge a different split.
    format!("{endpoint_id}\u{1f}{method}\u{1f}{path}\u{1f}{query}")
}

/// How long a last known-good catalog read stays usable as a fallback. Long
/// enough to cover a database slow spell end to end, short enough that a
/// genuinely broken catalog surfaces as an error rather than being papered over
/// indefinitely.
const STALE_CATALOG_TTL: Duration = Duration::from_secs(900);

/// How long the resolved external source list is trusted before the
/// `discovery_sources` table is read again. See `discovery_sources_cache`.
pub const DISCOVERY_SOURCES_TTL: Duration = Duration::from_secs(30);

/// How long a verified discovery session is trusted before Supabase is asked
/// again. One minute: long enough that a burst of searches from one tab costs
/// a single verification, short enough to state the staleness in a sentence.
/// See `parse_identity_cache`.
pub const PARSE_IDENTITY_TTL: Duration = Duration::from_secs(60);

/// Canonical cache key for a filtered catalog read.
///
/// Fields are joined with the ASCII unit separator (U+001F) rather than a
/// printable character: a listing search for `"weather|eu"` must not be able to
/// collide with a different field split, because a collision here serves one
/// caller's rows to another's query. `f64` and `DateTime` go in via `Debug`
/// since neither implements `Hash`; this only has to be canonical, not fast.
#[allow(clippy::too_many_arguments)]
fn catalog_query_key(
    search: Option<&str>,
    category: Option<&str>,
    tag: Option<&str>,
    listing_type: Option<&str>,
    min_price: Option<f64>,
    max_price: Option<f64>,
    created_after: Option<chrono::DateTime<chrono::Utc>>,
    created_before: Option<chrono::DateTime<chrono::Utc>>,
    slug: Option<&str>,
    sort: &str,
    order: &str,
    limit: i64,
    offset: i64,
) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{:?}\u{1f}{:?}\u{1f}{:?}\u{1f}{:?}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        search.unwrap_or(""),
        category.unwrap_or(""),
        tag.unwrap_or(""),
        listing_type.unwrap_or(""),
        min_price,
        max_price,
        created_after,
        created_before,
        // Part of the key, not an afterthought: without it a lookup for one
        // slug and an unfiltered page share a cache entry and one answers the
        // other.
        slug.unwrap_or(""),
        sort,
        order,
        limit,
        offset,
    )
}

impl EndpointCache {
    pub fn new() -> Self {
        // API cache: 5 minute TTL, max 1000 entries
        let api_cache = Cache::builder()
            .time_to_live(Duration::from_secs(300))
            .max_capacity(1000)
            .build();

        // Endpoint cache: 5 minute TTL, max 10000 entries
        let endpoint_cache = Cache::builder()
            .time_to_live(Duration::from_secs(300))
            .max_capacity(10000)
            .build();

        // Endpoint docs cache: same TTL/capacity as endpoint_cache; only
        // touched on 402 emission.
        let endpoint_docs_cache = Cache::builder()
            .time_to_live(Duration::from_secs(300))
            .max_capacity(10000)
            .build();

        // Provider directory: 60 second TTL — new listings should surface on
        // the landing page quickly, but a burst of traffic must not turn into
        // a burst of aggregate queries. Bounded by search term.
        let provider_stats_cache = Cache::builder()
            .time_to_live(Duration::from_secs(60))
            .max_capacity(1)
            .build();

        let bazaar_cache = Cache::builder()
            .time_to_live(Duration::from_secs(300))
            .max_capacity(500)
            .build();

        // Vijf minuten: de catalogus verandert bij een migratie of een nieuwe
        // listing, niet per minuut. Korter zetten koopt niets en betaalt een
        // volledige herlading.
        let ask_snapshot_cache = Cache::builder()
            .time_to_live(Duration::from_secs(300))
            .max_capacity(1)
            .build();

        let discovery_sources_cache = Cache::builder()
            .time_to_live(DISCOVERY_SOURCES_TTL)
            .max_capacity(1)
            .build();

        let source_provider_curation_cache = Cache::builder()
            .time_to_live(DISCOVERY_SOURCES_TTL)
            .max_capacity(1)
            .build();

        let query_plan_cache = Cache::builder()
            .time_to_live(Duration::from_secs(3600))
            .max_capacity(2_000)
            .build();

        // Upstream responses for endpoints that opt in. The per-entry TTL is
        // the endpoint's own; this outer bound is a safety net so a
        // misconfigured row cannot pin an answer forever, and the capacity
        // keeps a bursty caller from evicting everything else.
        let upstream_response_cache = Cache::builder()
            .time_to_live(MAX_UPSTREAM_RESPONSE_TTL)
            .max_capacity(5_000)
            .build();

        let parse_identity_cache = Cache::builder()
            .time_to_live(PARSE_IDENTITY_TTL)
            .max_capacity(10_000)
            .build();

        let provider_cache = Cache::builder()
            .time_to_live(Duration::from_secs(60))
            .max_capacity(200)
            .build();

        // Discovery catalog: 60 seconds, matching the provider directory. A new
        // listing appears to crawlers within a minute, and a burst of discovery
        // reads costs one aggregate query rather than one per page.
        let catalog_cache = Cache::builder()
            .time_to_live(Duration::from_secs(60))
            .max_capacity(1)
            .build();

        // Same 60s window as the catalog it is built from, so the two roll
        // together and a listing change surfaces at one predictable interval.
        let discovery_items_cache = Cache::builder()
            .time_to_live(Duration::from_secs(60))
            .max_capacity(1)
            .build();

        // Filtered catalog reads: same 60s window as the unfiltered catalog, so
        // a listing change surfaces everywhere at one interval. Capacity bounds
        // the damage from unbounded `?q=` cardinality — 500 distinct filter
        // combinations is far more than real clients use, and the LRU evicts the
        // long tail a crawler invents.
        let catalog_query_cache = Cache::builder()
            .time_to_live(Duration::from_secs(60))
            .max_capacity(500)
            .build();

        // Failure fallbacks. Same capacities as the live caches they shadow —
        // one stale entry per key that can be asked for.
        let catalog_query_stale = Cache::builder()
            .time_to_live(STALE_CATALOG_TTL)
            .max_capacity(500)
            .build();
        let catalog_stale = Cache::builder()
            .time_to_live(STALE_CATALOG_TTL)
            .max_capacity(1)
            .build();

        Self {
            api_cache,
            endpoint_cache,
            endpoint_docs_cache,
            provider_cache,
            provider_stats_cache,
            ask_snapshot_cache,
            bazaar_cache,
            discovery_sources_cache,
            source_provider_curation_cache,
            query_plan_cache,
            parse_identity_cache,
            catalog_cache,
            discovery_items_cache,
            catalog_query_cache,
            catalog_query_stale,
            catalog_stale,
            upstream_response_cache,
        }
    }

    /// A cached upstream answer for this exact request, if one is still
    /// within its endpoint's own TTL.
    pub async fn get_upstream_response(&self, key: &str) -> Option<Arc<CachedUpstreamResponse>> {
        self.upstream_response_cache
            .get(key)
            .await
            .filter(|entry| entry.is_live())
    }

    /// Remember an upstream answer. Only ever called for a 200 on a safe
    /// method — see `proxy::forward`.
    pub async fn put_upstream_response(&self, key: String, response: CachedUpstreamResponse) {
        if !response.is_live() {
            return;
        }
        self.upstream_response_cache
            .insert(key, Arc::new(response))
            .await;
    }

    /// The built discovery items for `/.well-known/x402`, shared read-only.
    ///
    /// Building them is the expensive half: every paid resource in the catalog
    /// with its schemas, examples and payment requirements. Paginating the
    /// document meant one page of 15 built all ~1.5k items and discarded the
    /// rest, once per request — concurrent crawler reads multiplied that until
    /// the process was OOM-killed on a 256 MB machine. Built once per TTL now;
    /// a request clones only the items it actually returns.
    ///
    /// Coalesced via `get_with`: on expiry, exactly one caller runs `build` and
    /// every other caller awaits that same build instead of running its own.
    pub async fn get_discovery_items<F>(&self, build: F) -> Arc<Vec<serde_json::Value>>
    where
        F: FnOnce() -> Vec<serde_json::Value>,
    {
        self.discovery_items_cache
            .get_with((), async move { Arc::new(build()) })
            .await
    }

    /// Every active listing with its endpoint documentation, for the discovery
    /// document. Shared behind an `Arc` because the value is large and every
    /// page of every crawl reads the same one.
    pub async fn get_discovery_catalog(&self, db: &Database) -> Result<Arc<Vec<ApiInfo>>> {
        let loaded = self
            .catalog_cache
            .entry(())
            .or_try_insert_with(async {
                let (apis, _) = db
                    .list_active_apis(
                        None, None, None, None, None, None, None, None, None, "name", "asc", 5000,
                        0,
                    )
                    .await?;
                Ok::<_, anyhow::Error>(Arc::new(apis))
            })
            .await;

        match loaded {
            Ok(entry) => {
                // Only a load that actually reached the database refreshes the
                // fallback; a cache hit has nothing new to record.
                let fresh = entry.is_fresh();
                let catalog = entry.into_value();
                if fresh {
                    self.catalog_stale.insert((), Arc::clone(&catalog)).await;
                }
                Ok(catalog)
            }
            Err(err) => match self.catalog_stale.get(&()).await {
                Some(stale) => {
                    tracing::warn!("discovery catalog load failed, serving stale: {err}");
                    Ok(stale)
                }
                None => Err(anyhow!("discovery catalog load failed: {err}")),
            },
        }
    }

    /// A FILTERED catalog read, cached and coalesced on the full parameter set.
    ///
    /// Every caller of `Database::list_active_apis` on a request path should go
    /// through here rather than the database directly: it is the most expensive
    /// query the gateway makes, and leaving any request path on the raw call
    /// re-opens the hole that caused the 2026-08-03 outage.
    #[allow(clippy::too_many_arguments)]
    pub async fn get_active_apis(
        &self,
        db: &Database,
        search: Option<&str>,
        category: Option<&str>,
        tag: Option<&str>,
        listing_type: Option<&str>,
        min_price: Option<f64>,
        max_price: Option<f64>,
        created_after: Option<chrono::DateTime<chrono::Utc>>,
        created_before: Option<chrono::DateTime<chrono::Utc>>,
        // One exact slug — see `Database::list_active_apis`.
        slug: Option<&str>,
        sort: &str,
        order: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<ApiInfo>, i64)> {
        let key = catalog_query_key(
            search,
            category,
            tag,
            listing_type,
            min_price,
            max_price,
            created_after,
            created_before,
            slug,
            sort,
            order,
            limit,
            offset,
        );

        let loaded = self
            .catalog_query_cache
            .entry(key.clone())
            .or_try_insert_with(async {
                db.list_active_apis(
                    search,
                    category,
                    tag,
                    listing_type,
                    min_price,
                    max_price,
                    created_after,
                    created_before,
                    slug,
                    sort,
                    order,
                    limit,
                    offset,
                )
                .await
                .map(Arc::new)
            })
            .await;

        // A statement timeout here is transient and load-shaped; the last good
        // answer for this exact query beats a 500. See `catalog_query_stale`.
        let cached = match loaded {
            Ok(entry) => {
                let fresh = entry.is_fresh();
                let page = entry.into_value();
                if fresh {
                    self.catalog_query_stale
                        .insert(key, Arc::clone(&page))
                        .await;
                }
                page
            }
            Err(err) => match self.catalog_query_stale.get(&key).await {
                Some(stale) => {
                    tracing::warn!("catalog query failed, serving stale: {err}");
                    stale
                }
                None => return Err(anyhow!("catalog query failed: {err}")),
            },
        };

        // One page's worth of rows, so the clone is cheap next to the query it
        // replaces.
        Ok((cached.0.clone(), cached.1))
    }

    /// Get API by slug, using cache
    ///
    /// The error message is deliberately CONSTANT — no slug, no path. Sentry
    /// groups issues by message, so interpolating the slug here turned one
    /// issue into one per listing and buried the signal under 159 near-identical
    /// entries. The slug is already on the `gateway_request` span (`api_slug`),
    /// which Sentry attaches as context, so putting it in the text only breaks
    /// grouping without adding information. Same rule for the two below.
    pub async fn get_api(&self, db: &Database, slug: &str) -> Result<Option<Api>> {
        self.api_cache
            .try_get_with(slug.to_string(), async { db.get_api_by_slug(slug).await })
            .await
            .map_err(|err| anyhow!("api lookup failed: {err}"))
    }

    /// Get endpoint configuration, using cache
    pub async fn get_endpoint(&self, db: &Database, key: &CacheKey) -> Result<Option<Endpoint>> {
        self.endpoint_cache
            .try_get_with(key.clone(), async {
                db.get_endpoint(key.api_id, &key.method, &key.path).await
            })
            .await
            .map_err(|err| anyhow!("endpoint lookup failed: {err}"))
    }

    /// Get the provider directory for an optional search term, using cache.
    pub async fn get_providers(
        &self,
        db: &Database,
        search: Option<&str>,
    ) -> Result<Vec<ProviderSummary>> {
        self.provider_cache
            .try_get_with(search.unwrap_or_default().to_string(), async {
                db.list_providers(search).await
            })
            .await
            .map_err(|err| anyhow!("provider directory load failed: {err}"))
    }

    /// De catalogus voor `/v1/ask`, geladen als hij er niet is.
    ///
    /// `try_get_with` laat een golf verzoeken samen één query doen in plaats van
    /// één per verzoek — dat is precies het patroon dat de provider-statistieken
    /// hierboven ook gebruiken, alleen met een query die wél binnen een seconde
    /// terug is.
    /// `engine_v2` kiest de POOL, niet de opmaak: de herwerkte keten zoekt over
    /// elk toegelaten endpoint, de oude alleen over endpoints die al een
    /// bediende capability-koppeling hebben. Zie
    /// [`crate::db::capabilities`] voor waarom dat verschil de hele herwerking
    /// is.
    ///
    /// De vlag komt uit de configuratie en verandert dus niet zonder herstart;
    /// één cachesleutel blijft daarom juist. Zou hij ooit per verzoek kunnen
    /// verschillen, dan moet hij in de sleutel.
    pub async fn get_ask_snapshot(
        &self,
        db: &Database,
        engine_v2: bool,
    ) -> Result<crate::ask::Snapshot> {
        self.ask_snapshot_cache
            .try_get_with((), async {
                if engine_v2 {
                    db.catalog_pool().await.map(std::sync::Arc::new)
                } else {
                    db.catalog_snapshot().await.map(std::sync::Arc::new)
                }
            })
            .await
            .map_err(|err| anyhow!("catalog snapshot load failed: {err}"))
    }

    /// What is cached, without ever computing it.
    ///
    /// The request path uses THIS. The stats are a decoration on offers that
    /// are already loaded — nice to rank by, never worth waiting for — and the
    /// query behind them takes ~30s against a 5s pool ceiling. Calling
    /// [`Self::get_provider_stats`] on a request therefore bought a guaranteed
    /// 5s stall for a guaranteed empty answer. Reading is free; filling is the
    /// refresher's job (`spawn_provider_stats_refresher`).
    pub async fn peek_provider_stats(
        &self,
    ) -> Option<std::sync::Arc<HashMap<String, ProviderStats>>> {
        self.provider_stats_cache.get(&()).await
    }

    /// One external-source document, fetched at most once per key at a time.
    ///
    /// `fetch` runs only on a miss, and concurrent misses on the same key share
    /// one run: moka's entry API coalesces them, so two agents asking the same
    /// index the same question inside the same instant cost that index one
    /// request, not two. A successful document is cached for the source-cache
    /// TTL; a failure is returned to every waiter and caches nothing, so the
    /// next caller retries rather than inheriting a five-minute outage.
    ///
    /// Returns the document and whether THIS call produced it (`true`) or found
    /// it already warm (`false`) — the difference between a source that
    /// answered and one that merely had answered earlier.
    pub async fn get_or_fetch_source<F, E>(
        &self,
        key: &str,
        fetch: F,
    ) -> Result<(serde_json::Value, bool), Arc<E>>
    where
        F: std::future::Future<Output = Result<serde_json::Value, E>>,
        E: Send + Sync + 'static,
    {
        let entry = self
            .bazaar_cache
            .entry(key.to_string())
            .or_try_insert_with(fetch)
            .await?;
        let fresh = entry.is_fresh();
        Ok((entry.into_value(), fresh))
    }

    /// The resolved external source list, if read within the last
    /// [`DISCOVERY_SOURCES_TTL`].
    pub async fn get_discovery_sources(&self) -> Option<serde_json::Value> {
        self.discovery_sources_cache.get(&()).await
    }

    /// Stores the resolved external source list.
    pub async fn put_discovery_sources(&self, sources: serde_json::Value) {
        self.discovery_sources_cache.insert((), sources).await;
    }

    /// The seller curation map, if read within [`DISCOVERY_SOURCES_TTL`].
    pub async fn get_source_provider_curation(
        &self,
    ) -> Option<std::sync::Arc<crate::db::CurationMap>> {
        self.source_provider_curation_cache.get(&()).await
    }

    /// Stores the seller curation map. Never called with an unreadable
    /// table's result — see `crate::sources::provider_curation` — so an
    /// outage is retried, not remembered.
    pub async fn put_source_provider_curation(&self, map: std::sync::Arc<crate::db::CurationMap>) {
        self.source_provider_curation_cache.insert((), map).await;
    }

    /// Who this session token belongs to, verifying only if we must.
    ///
    /// Read through `try_get_with`, not `get` + `insert`, for the reason
    /// `catalog_cache` records: a plain get/insert makes every concurrent miss
    /// run its own `verify`, so one tab firing several searches at once — or a
    /// caller replaying one token hard — becomes N simultaneous round trips to
    /// the auth server the credits and Connect paths also depend on. Coalesced,
    /// one caller asks and the rest await that same answer.
    ///
    /// THREE OUTCOMES, and they are cached differently, which is the whole
    /// reason this is a read-through rather than two methods:
    ///
    ///   `Ok(Some(id))`  verified. Cached for [`PARSE_IDENTITY_TTL`].
    ///   `Ok(None)`      the auth server looked and said no. ALSO cached — a
    ///                   token it has rejected does not become valid, and a new
    ///                   sign-in produces a new token with a new key, so there
    ///                   is nobody to lock out. Without this, one attacker
    ///                   replaying a made-up token costs one outbound auth call
    ///                   per request, forever.
    ///   `Err(_)`        the auth server could not be reached, or is not
    ///                   configured. NOT cached, because that is a fact about
    ///                   us and not about the caller, and a transient outage
    ///                   must not stick to a good session for a minute.
    ///                   `try_get_with` does not store errors, which is exactly
    ///                   the behaviour wanted here.
    pub async fn parse_identity<F, E>(
        &self,
        token_hash: &str,
        verify: F,
    ) -> Result<Option<String>, std::sync::Arc<E>>
    where
        F: std::future::Future<Output = Result<Option<String>, E>>,
        E: Send + Sync + 'static,
    {
        self.parse_identity_cache
            .try_get_with(token_hash.to_string(), verify)
            .await
    }

    /// A cached model answer, if the same input was sent recently.
    pub async fn get_model_output(&self, key: &str) -> Option<serde_json::Value> {
        self.query_plan_cache.get(key).await
    }

    /// Stores a model answer against its input hash.
    pub async fn put_model_output(&self, key: &str, value: serde_json::Value) {
        self.query_plan_cache.insert(key.to_string(), value).await;
    }

    /// Invalidate all cached entries
    pub async fn invalidate_all(&self) {
        self.parse_identity_cache.invalidate_all();
        self.api_cache.invalidate_all();
        self.endpoint_cache.invalidate_all();
        self.endpoint_docs_cache.invalidate_all();
        self.provider_cache.invalidate_all();
        self.discovery_sources_cache.invalidate_all();
        self.source_provider_curation_cache.invalidate_all();
        self.catalog_cache.invalidate_all();
        self.discovery_items_cache.invalidate_all();
        self.catalog_query_cache.invalidate_all();
        self.catalog_query_stale.invalidate_all();
        self.catalog_stale.invalidate_all();
    }
}

impl Default for EndpointCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The regression this cache exists to prevent. With `get` + `insert`,
    /// every reader that arrived while the build was in flight missed and ran
    /// its own — in production that meant N concurrent 2s catalog queries every
    /// time the 60s TTL lapsed. `get_with` collapses them into one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_discovery_reads_build_the_items_once() {
        let cache = Arc::new(EndpointCache::new());
        let builds = Arc::new(AtomicUsize::new(0));

        let readers: Vec<_> = (0..8)
            .map(|_| {
                let cache = Arc::clone(&cache);
                let builds = Arc::clone(&builds);
                tokio::spawn(async move {
                    cache
                        .get_discovery_items(|| {
                            builds.fetch_add(1, Ordering::SeqCst);
                            // Hold the build open so the other readers are
                            // genuinely concurrent with it, not merely after it.
                            std::thread::sleep(Duration::from_millis(50));
                            vec![serde_json::json!({ "resource": "one" })]
                        })
                        .await
                })
            })
            .collect();

        for reader in readers {
            let items = reader.await.expect("reader panicked");
            assert_eq!(items.len(), 1, "every reader sees the same built items");
        }

        assert_eq!(
            builds.load(Ordering::SeqCst),
            1,
            "eight concurrent readers must trigger exactly one build"
        );
    }

    /// Identical concurrent source fetches must reach the upstream index once.
    /// The sweep runs every phrasing of every source in parallel, and two
    /// agents asking the same question at the same moment used to be two
    /// requests to each index for the same document.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_identical_source_fetches_hit_upstream_once() {
        let cache = Arc::new(EndpointCache::new());
        let fetches = Arc::new(AtomicUsize::new(0));

        let callers: Vec<_> = (0..8)
            .map(|_| {
                let cache = Arc::clone(&cache);
                let fetches = Arc::clone(&fetches);
                tokio::spawn(async move {
                    cache
                        .get_or_fetch_source("payai::__index", async {
                            fetches.fetch_add(1, Ordering::SeqCst);
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            Ok::<_, String>(serde_json::json!({"items": [1]}))
                        })
                        .await
                })
            })
            .collect();

        let mut fresh = 0;
        for caller in callers {
            let (doc, was_fresh) = caller.await.expect("caller panicked").expect("fetch ok");
            assert_eq!(doc["items"][0], 1);
            fresh += usize::from(was_fresh);
        }
        assert_eq!(fetches.load(Ordering::SeqCst), 1, "one upstream request");
        assert_eq!(fresh, 1, "exactly one caller did the fetching");
    }

    /// A failed fetch is shared with every waiter and cached for nobody.
    #[tokio::test]
    async fn a_failed_source_fetch_is_not_cached() {
        let cache = EndpointCache::new();
        let err = cache
            .get_or_fetch_source("thirdweb::q", async {
                Err::<serde_json::Value, _>("timeout")
            })
            .await
            .expect_err("first call fails");
        assert_eq!(*err, "timeout");

        let (doc, fresh) = cache
            .get_or_fetch_source("thirdweb::q", async {
                Ok::<_, &str>(serde_json::json!({"items": []}))
            })
            .await
            .expect("second call fetches afresh");
        assert!(fresh, "the failure must not have been stored");
        assert!(doc["items"].is_array());
    }

    #[test]
    fn the_source_list_is_refreshed_well_inside_the_payload_cache_life() {
        // An admin enable/disable has to reach the sweep within about half a
        // minute; the five-minute payload cache would be too slow for that.
        assert!(DISCOVERY_SOURCES_TTL <= Duration::from_secs(30));
    }

    fn key(search: Option<&str>, category: Option<&str>, limit: i64) -> String {
        catalog_query_key(
            search, category, None, None, None, None, None, None, None, "name", "asc", limit, 0,
        )
    }

    /// A lookup for one slug and an unfiltered page are different questions,
    /// and a shared key would have one answer the other — the page of
    /// near-misses served as though it were the listing that was asked for.
    #[test]
    fn a_slug_lookup_does_not_share_a_page_key() {
        let plain = catalog_query_key(
            None, None, None, None, None, None, None, None, None, "name", "asc", 5, 0,
        );
        let by_slug = catalog_query_key(
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("tavily"),
            "name",
            "asc",
            5,
            0,
        );
        assert_ne!(plain, by_slug);
    }

    #[test]
    fn identical_filters_share_a_cache_key() {
        assert_eq!(
            key(Some("weather"), None, 20),
            key(Some("weather"), None, 20)
        );
        assert_eq!(key(None, None, 20), key(None, None, 20));
    }

    #[test]
    fn different_filters_never_share_a_cache_key() {
        let base = key(None, None, 20);
        for other in [
            key(Some("weather"), None, 20),
            key(None, Some("weather"), 20),
            key(None, None, 21),
        ] {
            assert_ne!(base, other, "distinct queries must not collide");
        }
    }

    /// A collision here would serve one caller's rows for another's filters.
    /// The separator is what prevents a value from impersonating a field break.
    #[test]
    fn a_value_containing_a_delimiter_cannot_forge_another_key() {
        assert_ne!(
            key(Some("a\u{1f}b"), None, 20),
            key(Some("a"), Some("b"), 20)
        );
        assert_ne!(key(Some("a|b"), None, 20), key(Some("a"), Some("b"), 20));
        // An empty filter and an absent one describe the same query, and the
        // handlers normalise blanks to None before they ever reach the cache.
        assert_eq!(key(Some(""), None, 20), key(None, None, 20));
    }
}

#[cfg(test)]
mod upstream_response_tests {
    use super::*;

    #[tokio::test]
    async fn a_cached_answer_is_served_until_its_endpoint_ttl_expires() {
        let cache = EndpointCache::new();
        let key = upstream_cache_key(
            &uuid::Uuid::nil(),
            "GET",
            "/api/v1/hvds/jaarrekeningen/kvknummer/68750110",
            "",
        );
        cache
            .put_upstream_response(
                key.clone(),
                CachedUpstreamResponse::new(
                    200,
                    Some("application/json".into()),
                    b"{\"opendataFields\":[]}".to_vec(),
                    Duration::from_secs(60),
                ),
            )
            .await;
        let hit = cache.get_upstream_response(&key).await.expect("hit");
        assert_eq!(hit.status, 200);
        assert_eq!(hit.content_type.as_deref(), Some("application/json"));
        assert_eq!(hit.body, b"{\"opendataFields\":[]}");

        // An entry past its endpoint's own TTL is not served, even though the
        // cache's outer TTL has not elapsed.
        let stale_key = format!("{key}-stale");
        cache
            .put_upstream_response(
                stale_key.clone(),
                CachedUpstreamResponse::new(200, None, b"old".to_vec(), Duration::ZERO),
            )
            .await;
        assert!(cache.get_upstream_response(&stale_key).await.is_none());
    }

    #[tokio::test]
    async fn one_endpoints_answer_is_never_served_for_another_request() {
        let cache = EndpointCache::new();
        let one = upstream_cache_key(&uuid::Uuid::nil(), "GET", "/basisprofielen/11111111", "");
        let two = upstream_cache_key(&uuid::Uuid::nil(), "GET", "/basisprofielen/22222222", "");
        let other_endpoint = upstream_cache_key(
            &uuid::Uuid::from_u128(9),
            "GET",
            "/basisprofielen/11111111",
            "",
        );
        let with_query = upstream_cache_key(
            &uuid::Uuid::nil(),
            "GET",
            "/basisprofielen/11111111",
            "geoData=true",
        );
        assert_ne!(one, two);
        assert_ne!(one, other_endpoint);
        assert_ne!(one, with_query);

        cache
            .put_upstream_response(
                one.clone(),
                CachedUpstreamResponse::new(200, None, b"first".to_vec(), Duration::from_secs(60)),
            )
            .await;
        assert!(cache.get_upstream_response(&two).await.is_none());
        assert!(cache.get_upstream_response(&other_endpoint).await.is_none());
        assert!(cache.get_upstream_response(&with_query).await.is_none());
    }
}
