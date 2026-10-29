//! The capability layer: offers grouped by the task they perform.
//!
//! `/v1/apis` groups the catalog by seller. This module groups the same rows by
//! *what the buyer wants done*, which is the question an agent has before it
//! spends anything and the one the schema could not express until migration
//! 075. See that migration's header for why every seeded assignment is
//! hand-verified rather than inferred.
//!
//! Two rules hold everything here honest:
//!
//!   * **Only reviewed assignments are served.** `source = 'inferred'` rows are
//!     a review queue, never a candidate. A missed alternative costs an agent a
//!     comparison; a wrong one costs it a purchase.
//!   * **Visibility is the catalog's, not ours.** The listing predicate is
//!     [`super::discovery::CATALOG_VISIBILITY`] verbatim, so a capability can
//!     never offer a listing `/v1/apis` deliberately hides.

use super::discovery::{
    listing_provider_join, CATALOG_VISIBILITY, ENDPOINT_ADMISSION, ENDPOINT_CALL_URL,
    MANAGED_FIRST, PROVIDER_IDENTITY_COLUMNS,
};
use super::*;

/// Assignment sources an agent is allowed to see. `inferred` is excluded by
/// construction rather than by a caller-supplied flag — there is no request
/// that should surface an unreviewed guess as a purchasable candidate.
const SERVED_SOURCES: &str = "co.source IN ('curated', 'declared')";

/// How many endpoints of ONE listing a word-matched catalogue search may
/// return. Three: enough that a listing whose endpoints genuinely differ shows
/// that it does, few enough that no single seller can fill the page.
const MAX_PER_LISTING: i64 = 3;

/// How the candidate list is ordered, and the disclosure published next to it.
///
/// The catalog ranks [`MANAGED_FIRST`] everywhere (see
/// [`super::RANKING_NOTE`]): listings Apiosk settles rank above federated ones,
/// which is the incentive for a federated provider to move settlement over.
/// A *comparison* is a different surface. Reordering candidates on commercial
/// grounds while presenting them as a like-for-like comparison would make the
/// cheapest offer invisible below a managed one, which is the exact failure the
/// layer exists to remove — so the default here is price, ascending, and the
/// ranking actually used is always published.
///
/// `managed_first` remains available per request for callers that want the
/// catalog's ordering; nothing is hidden either way.
pub enum CapabilitySort {
    Price,
    ManagedFirst,
}

impl CapabilitySort {
    pub fn parse(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some("managed_first") => Self::ManagedFirst,
            _ => Self::Price,
        }
    }

    fn order_clause(&self) -> String {
        match self {
            Self::Price => "e.price ASC, a.slug ASC, e.path ASC".to_string(),
            Self::ManagedFirst => format!("{MANAGED_FIRST}, e.price ASC, a.slug ASC, e.path ASC"),
        }
    }

    /// One-line disclosure of the ordering, published as `meta.ranking`.
    pub fn note(&self) -> &'static str {
        match self {
            Self::Price => "price-ascending: candidates are ordered by the listed price of the operation, with no managed-first preference applied. Unlike /v1/apis, this surface does not rank Apiosk-settled listings above federated ones — a comparison that reorders on commercial grounds is not a comparison. Pass sort=managed_first for the catalog's ordering.",
            Self::ManagedFirst => "managed-first: listings Apiosk publishes and operates rank above federated listings it only executes, then price ascending. Every listing is called and settled through the gateway either way. This is the catalog's ordering, requested explicitly; the default on this surface is sort=price.",
        }
    }
}

/// A task a buyer can want done, plus what the catalog currently offers for it.
#[derive(Debug, Clone, FromRow)]
pub struct CapabilityInfo {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub category: Option<String>,
    pub input_contract: serde_json::Value,
    pub output_contract: serde_json::Value,
    /// Servable offers (endpoints), after visibility filtering.
    pub offer_count: i64,
    /// Distinct listings behind those offers. One listing can hold several
    /// offers for the same capability (two screenshot endpoints, say), so this
    /// is the number that answers "do I have a real alternative?".
    pub listing_count: i64,
    pub min_price_usd: Option<f64>,
    pub max_price_usd: Option<f64>,
}

/// One candidate for a capability: an endpoint, its price, and how to call it
/// with the capability's own inputs.
#[derive(Debug, Clone, FromRow)]
pub struct CapabilityOffer {
    /// Which capability this offer serves. Carried on the row because
    /// [`Database::list_capability_offers_many`] returns several capabilities'
    /// offers in one result set and the caller has to split them again.
    pub capability_slug: String,
    /// Stable identifier for this candidate across the discover -> compare ->
    /// decide pipeline.
    pub endpoint_id: Option<Uuid>,
    pub api_slug: String,
    pub api_name: String,
    pub endpoint_method: String,
    pub endpoint_path: String,
    pub endpoint_description: Option<String>,
    pub price_usd: f64,
    pub payment_required: bool,
    pub hosted_externally: bool,
    /// Where the agent actually sends the request.
    ///
    /// For a managed listing this is the gateway URL. For a federated listing
    /// it is the provider's own resource from `apis.external_x402` — never a
    /// gateway URL, because the gateway does not proxy federated listings and
    /// returns 404 for them. NULL when the resource could not be resolved, in
    /// which case the offer is published as not directly callable rather than
    /// with a target that would fail.
    pub call_url: Option<String>,
    /// capability input name -> where it goes on this endpoint. See migration
    /// 075 for the vocabulary.
    pub input_map: serde_json::Value,
    pub fixed_params: serde_json::Value,
    /// The endpoint's published input schema, both halves, exactly as the
    /// catalogue holds them.
    ///
    /// BOTH, because the catalogue is 92% GET: a GET carries its inputs in the
    /// query string, so `request_body` is empty for 1024 of 1122 endpoints and
    /// `query_params` is where their inputs actually live. Reading only the
    /// body found required fields on 54 endpoints and none at all on KVK,
    /// CityFalcon or PulseNetwork — which was a fact about the column, not
    /// about the APIs.
    ///
    /// Free-form on purpose: three shapes are in the wild (see
    /// [`crate::ask::required_inputs`], which is where they are reconciled).
    pub request_body: serde_json::Value,
    pub query_params: serde_json::Value,
    /// Why this offer is not a perfect substitute, when it isn't.
    pub notes: Option<String>,
    pub source: String,
    pub confidence: f64,
    pub provider_name: Option<String>,
    pub provider_username: Option<String>,
    /// The endpoint's own search metadata, already joined into one string:
    /// `search_terms`, `tags`, `use_cases` and `task_summary` from migration
    /// 109, backfilled for 1295 endpoints by migration 110.
    ///
    /// Written to be MATCHED rather than read. `search_terms` in particular is
    /// the buyer's vocabulary next to the provider's — "company VAT number"
    /// against "BTW-identificatie" — which is the single thing substring
    /// matching on provider wording can never bridge on its own.
    ///
    /// Until this field it was dead weight: 109 built the columns and a GIN
    /// index, 110 filled them, and no query in the gateway read either. The
    /// migration says so itself.
    ///
    /// NULL on every pre-rework query, deliberately, so the flagged path can
    /// widen recall without moving a single score on the default one.
    pub search_text: Option<String>,
}

/// Every active capability with the offer counts and price range the catalog
/// can currently serve for it. Binds nothing.
///
/// Built by a named function rather than inline, like the catalog queries next
/// door: these are runtime-checked strings, so the only way to exercise the SQL
/// without a live pool is to be able to ask for it.
fn capability_list_query() -> String {
    format!(
        r#"SELECT
                 c.slug,
                 c.name,
                 c.description,
                 c.category,
                 c.input_contract,
                 c.output_contract,
                 COALESCE(o.offer_count, 0)::BIGINT   as offer_count,
                 COALESCE(o.listing_count, 0)::BIGINT as listing_count,
                 o.min_price_usd,
                 o.max_price_usd
               FROM capabilities c
               LEFT JOIN LATERAL (
                 SELECT
                   COUNT(*)                            as offer_count,
                   COUNT(DISTINCT a.id)                as listing_count,
                   MIN(e.price::double precision)      as min_price_usd,
                   MAX(e.price::double precision)      as max_price_usd
                 FROM capability_offers co
                 JOIN apis a          ON a.id = co.api_id
                 JOIN api_endpoints e ON e.id = co.endpoint_id
                 WHERE co.capability_slug = c.slug
                   AND {SERVED_SOURCES}
                   AND COALESCE(e.active, TRUE)
                   AND {CATALOG_VISIBILITY}
               ) o ON TRUE
               WHERE c.status = 'active'
               ORDER BY offer_count DESC, c.slug ASC"#
    )
}

/// The candidates for one capability. Binds `$1` (capability slug) and `$2`
/// (optional max price).
fn capability_offers_query(sort: &CapabilitySort) -> String {
    let provider_join = listing_provider_join();
    let order_clause = sort.order_clause();

    format!(
        r#"SELECT
                 {PROVIDER_IDENTITY_COLUMNS}
                 a.slug as api_slug,
                 COALESCE(a.name, INITCAP(REPLACE(a.slug, '-', ' '))) as api_name,
                 co.capability_slug,
                 co.endpoint_id,
                 e.method as endpoint_method,
                 e.path   as endpoint_path,
                 NULLIF(e.description, '') as endpoint_description,
                 e.price::double precision as price_usd,
                 e.payment_required,
                 (COALESCE(a.listing_type::text, 'api') = 'federated') as hosted_externally,
                 {ENDPOINT_CALL_URL} as call_url,
                 co.input_map,
                 co.fixed_params,
                 -- Dubbele accolades: deze query staat in een format!-string.
                 COALESCE(e.request_body, '{{}}'::jsonb) as request_body,
                 COALESCE(e.query_params, '{{}}'::jsonb) as query_params,
                 NULL::text as search_text,
                 co.notes,
                 co.source,
                 co.confidence::double precision as confidence
               FROM capability_offers co
               JOIN apis a          ON a.id = co.api_id
               JOIN api_endpoints e ON e.id = co.endpoint_id
               LEFT JOIN provider_profiles p ON p.id = a.owner_id
               {provider_join}
               WHERE co.capability_slug = ANY($1)
                 AND {SERVED_SOURCES}
                 AND COALESCE(e.active, TRUE)
                 AND ($2::double precision IS NULL OR e.price::double precision <= $2)
                 AND {CATALOG_VISIBILITY}
               ORDER BY co.capability_slug ASC, {order_clause}"#
    )
}

/// EVERY servable offer in the catalogue, unfiltered by capability.
///
/// Same columns, same visibility rules and same call-URL resolution as
/// [`capability_offers_query`] — deliberately, so a candidate that came out of
/// the snapshot is indistinguishable from one the capability lane served. The
/// only difference is the absence of a `capability_slug = ANY($1)` filter.
///
/// This exists because the catalogue is now small enough to hold: six sources,
/// roughly eleven hundred endpoints. Scoring them in memory costs microseconds,
/// where a per-request SQL word-match costs a round trip and a scan. Below a
/// few tens of thousands of endpoints, loading the lot once and searching it in
/// process is simply the faster shape.
fn catalog_snapshot_query() -> String {
    let provider_join = listing_provider_join();

    format!(
        r#"SELECT
                 {PROVIDER_IDENTITY_COLUMNS}
                 a.slug as api_slug,
                 COALESCE(a.name, INITCAP(REPLACE(a.slug, '-', ' '))) as api_name,
                 co.capability_slug,
                 co.endpoint_id,
                 e.method as endpoint_method,
                 e.path   as endpoint_path,
                 NULLIF(e.description, '') as endpoint_description,
                 e.price::double precision as price_usd,
                 e.payment_required,
                 (COALESCE(a.listing_type::text, 'api') = 'federated') as hosted_externally,
                 {ENDPOINT_CALL_URL} as call_url,
                 co.input_map,
                 co.fixed_params,
                 -- Dubbele accolades: deze query staat in een format!-string.
                 COALESCE(e.request_body, '{{}}'::jsonb) as request_body,
                 COALESCE(e.query_params, '{{}}'::jsonb) as query_params,
                 NULL::text as search_text,
                 co.notes,
                 co.source,
                 co.confidence::double precision as confidence
               FROM capability_offers co
               JOIN apis a          ON a.id = co.api_id
               JOIN api_endpoints e ON e.id = co.endpoint_id
               LEFT JOIN provider_profiles p ON p.id = a.owner_id
               {provider_join}
               WHERE {SERVED_SOURCES}
                 AND COALESCE(e.active, TRUE)
                 AND {CATALOG_VISIBILITY}
               ORDER BY a.slug ASC, e.path ASC"#
    )
}

/// The same snapshot, rooted where the catalogue actually lives.
///
/// [`catalog_snapshot_query`] starts `FROM capability_offers` and INNER JOINs
/// its way to the endpoint. Read that direction out loud and the defect is
/// plain: an `api_endpoints` row with no `capability_offers` row cannot appear
/// in the result at all. It is not ranked low. It is not filtered late. It is
/// never loaded — so it is invisible to `/v1/ask`, which is the pool behind
/// both `apiosk` and `apiosk_discover`, before any scoring or model call runs.
///
/// A user naming a source we hold, whose capability nobody has wired yet, is
/// told it does not exist. That is the single behaviour the rework exists to
/// end, and the specs state the rule directly: a source must be FINDABLE before
/// Apiosk decides whether it is RUNNABLE. A missing mapping is our gap, and our
/// gap may not be reported as the user's mistake.
///
/// So the join flips. The pool is every admitted, visible endpoint, and the
/// capability comes along when there is one.
///
/// The row shape does not change by so much as a column. An endpoint with no
/// served offer is emitted exactly the way `search_catalog_endpoints` has
/// always emitted one — `capability_slug = ''`, `source = 'catalog'`, empty
/// maps — so every existing reader keeps working, and `is_external`, `as_offer`
/// and the shortlist deduplication need no special case.
///
/// What still holds:
///
///   - [`SERVED_SOURCES`] moves from the WHERE to the JOIN. It no longer
///     decides whether an endpoint is VISIBLE; it decides only whether a
///     capability is SERVED. An unreviewed guess is still never a purchasable
///     candidate, which is what the header of this module promises.
///   - [`CATALOG_VISIBILITY`] is applied verbatim. Nothing hidden becomes
///     visible.
///   - [`ENDPOINT_ADMISSION`] is added: a source that publishes no usable
///     contract never enters, because nothing downstream could bind or price it
///     honestly.
///
/// An endpoint serving three capabilities still yields three rows, as today;
/// `ask::shortlist_with_matches` already keeps the best one per endpoint.
fn catalog_pool_query() -> String {
    let provider_join = listing_provider_join();

    format!(
        r#"SELECT
                 {PROVIDER_IDENTITY_COLUMNS}
                 a.slug as api_slug,
                 COALESCE(a.name, INITCAP(REPLACE(a.slug, '-', ' '))) as api_name,
                 COALESCE(co.capability_slug, '') as capability_slug,
                 e.id as endpoint_id,
                 e.method as endpoint_method,
                 e.path   as endpoint_path,
                 NULLIF(e.description, '') as endpoint_description,
                 e.price::double precision as price_usd,
                 e.payment_required,
                 (COALESCE(a.listing_type::text, 'api') = 'federated') as hosted_externally,
                 {ENDPOINT_CALL_URL} as call_url,
                 -- Dubbele accolades: deze query staat in een format!-string.
                 COALESCE(co.input_map, '{{}}'::jsonb) as input_map,
                 COALESCE(co.fixed_params, '{{}}'::jsonb) as fixed_params,
                 COALESCE(e.request_body, '{{}}'::jsonb) as request_body,
                 COALESCE(e.query_params, '{{}}'::jsonb) as query_params,
                 co.notes,
                 -- De zoekwoorden van migratie 109, gevuld door 110 voor 1295
                 -- endpoints, en tot nu toe door geen enkele query gelezen.
                 -- `concat_ws` slaat NULL-kolommen over, dus een endpoint
                 -- zonder metadata levert gewoon NULL en weegt nergens mee.
                 NULLIF(concat_ws(' ',
                   array_to_string(e.search_terms, ' '),
                   array_to_string(e.tags, ' '),
                   array_to_string(e.use_cases, ' '),
                   e.task_summary
                 ), '') as search_text,
                 -- Dezelfde afspraak als `search_catalog_endpoints`: 'catalog'
                 -- is een endpoint zonder bediende koppeling, niet een fout.
                 COALESCE(co.source, 'catalog') as source,
                 COALESCE(co.confidence, 1.0)::double precision as confidence
               FROM api_endpoints e
               JOIN apis a ON a.id = e.api_id
               LEFT JOIN capability_offers co
                 ON co.endpoint_id = e.id AND {SERVED_SOURCES}
               LEFT JOIN provider_profiles p ON p.id = a.owner_id
               {provider_join}
               WHERE COALESCE(e.active, TRUE)
                 AND {CATALOG_VISIBILITY}
                 AND {ENDPOINT_ADMISSION}
               ORDER BY a.slug ASC, e.path ASC"#
    )
}

/// The SQL behind [`Database::direct_offer`]. Binds `$1` (endpoint id).
///
/// Named, like every other query in this file, for the reason the header of
/// [`capability_list_query`] gives: these are runtime-checked strings, so a
/// query nothing can ASK for is a query nothing can test. This one was inline
/// when `request_body` and `query_params` were added to [`CapabilityOffer`],
/// the two builders next door were updated and this was not — and every
/// `POST /v1/do` naming a candidate answered 500 until somebody read a log.
fn direct_offer_query() -> String {
    let provider_join = listing_provider_join();
    let sql = format!(
        r#"SELECT
                 {PROVIDER_IDENTITY_COLUMNS}
                 a.slug as api_slug,
                 COALESCE(a.name, INITCAP(REPLACE(a.slug, '-', ' '))) as api_name,
                 ''::text as capability_slug,
                 e.id as endpoint_id,
                 e.method as endpoint_method,
                 e.path   as endpoint_path,
                 NULLIF(e.description, '') as endpoint_description,
                 e.price::double precision as price_usd,
                 e.payment_required,
                 (COALESCE(a.listing_type::text, 'api') = 'federated') as hosted_externally,
                 {ENDPOINT_CALL_URL} as call_url,
                 '{{}}'::jsonb as input_map,
                 '{{}}'::jsonb as fixed_params,
                 -- Dubbele accolades: deze query staat in een format!-string.
                 COALESCE(e.request_body, '{{}}'::jsonb) as request_body,
                 COALESCE(e.query_params, '{{}}'::jsonb) as query_params,
                 NULL::text as search_text,
                 NULL::text as notes,
                 'direct'::text as source,
                 1.0::double precision as confidence
               FROM api_endpoints e
               JOIN apis a ON a.id = e.api_id
               LEFT JOIN provider_profiles p ON p.id = a.owner_id
               {provider_join}
               WHERE e.id = $1
                 AND COALESCE(e.active, TRUE)
                 AND {CATALOG_VISIBILITY}"#
    );

    sql
}

/// The SQL behind [`Database::search_catalog_endpoints`]. Binds `$1` (terms),
/// `$2` (max price) and `$3` (limit). Named for the same reason as
/// [`direct_offer_query`], and it had the same missing columns.
fn catalog_search_query() -> String {
    let provider_join = listing_provider_join();
    let sql = format!(
        r#"SELECT * FROM (
               SELECT
                 matched.*,
                 row_number() OVER (
                   PARTITION BY matched.api_slug
                   ORDER BY matched.confidence DESC, matched.price_usd ASC, matched.endpoint_path ASC
                 ) AS listing_rank
               FROM (
                 SELECT
                   {PROVIDER_IDENTITY_COLUMNS}
                   a.slug as api_slug,
                   COALESCE(a.name, INITCAP(REPLACE(a.slug, '-', ' '))) as api_name,
                   ''::text as capability_slug,
                   e.id as endpoint_id,
                   e.method as endpoint_method,
                   e.path   as endpoint_path,
                   NULLIF(e.description, '') as endpoint_description,
                   e.price::double precision as price_usd,
                   e.payment_required,
                   (COALESCE(a.listing_type::text, 'api') = 'federated') as hosted_externally,
                   {ENDPOINT_CALL_URL} as call_url,
                   '{{}}'::jsonb as input_map,
                   '{{}}'::jsonb as fixed_params,
                   -- Dubbele accolades: deze query staat in een format!-string.
                   -- See `direct_offer`: `CapabilityOffer` reads both halves
                   -- of the input schema, so a query that maps into it and
                   -- omits them fails at runtime, not at compile time.
                   COALESCE(e.request_body, '{{}}'::jsonb) as request_body,
                   COALESCE(e.query_params, '{{}}'::jsonb) as query_params,
                   NULL::text as search_text,
                   NULL::text as notes,
                   'catalog'::text as source,
                   -- Weighted, not counted. Every field used to score 1, which
                   -- ranked a listing whose PATH happens to contain "status"
                   -- level with one whose endpoint is described as returning a
                   -- flight's status. What a thing says it does is the strongest
                   -- evidence it does it; the URL it is served at is the weakest.
                   (
                     SELECT COALESCE(SUM(
                         (CASE WHEN position(term in lower(COALESCE(e.description, ''))) > 0
                               THEN 3 ELSE 0 END)
                       + (CASE WHEN position(term in lower(COALESCE(a.name, ''))) > 0
                                 OR position(term in lower(a.slug)) > 0
                               THEN 3 ELSE 0 END)
                       + (CASE WHEN position(term in lower(COALESCE(a.description, ''))) > 0
                               THEN 2 ELSE 0 END)
                       + (CASE WHEN position(term in lower(COALESCE(a.category, ''))) > 0
                                 OR EXISTS (
                                      SELECT 1
                                      FROM jsonb_array_elements_text(
                                             COALESCE(a.listing_metadata->'tags', '[]'::jsonb)
                                           ) AS tags(value)
                                      WHERE position(term in lower(tags.value)) > 0
                                    )
                               THEN 2 ELSE 0 END)
                       + (CASE WHEN position(term in lower(e.path)) > 0
                               THEN 1 ELSE 0 END)
                     ), 0)::double precision
                     FROM unnest($1::text[]) AS term
                   ) as confidence
                 FROM api_endpoints e
                 JOIN apis a ON a.id = e.api_id
                 LEFT JOIN provider_profiles p ON p.id = a.owner_id
                 {provider_join}
                 WHERE COALESCE(e.active, TRUE)
                   AND ($2::double precision IS NULL OR e.price::double precision <= $2)
                   AND {CATALOG_VISIBILITY}
               ) matched
               WHERE confidence > 0
               ORDER BY confidence DESC, price_usd ASC, api_slug ASC, endpoint_path ASC
             ) ranked
             WHERE listing_rank <= {MAX_PER_LISTING}
             ORDER BY confidence DESC, price_usd ASC, api_slug ASC, endpoint_path ASC
             LIMIT $3"#
    );

    sql
}

impl Database {
    /// Every active capability with what the catalog can currently serve for
    /// it. Capabilities with no servable offer are still returned: "nobody
    /// sells this" is a true answer, and it is the demand signal that tells a
    /// provider where the gap is.
    pub async fn list_capabilities(&self) -> Result<Vec<CapabilityInfo>> {
        Ok(
            sqlx::query_as::<_, CapabilityInfo>(&capability_list_query())
                .fetch_all(&self.pool)
                .await?,
        )
    }

    /// The whole servable catalogue in one read, for the in-memory search
    /// snapshot. Called by the refresher, never on a request.
    pub async fn catalog_snapshot(&self) -> Result<Vec<CapabilityOffer>> {
        Ok(
            sqlx::query_as::<_, CapabilityOffer>(&catalog_snapshot_query())
                .fetch_all(&self.pool)
                .await?,
        )
    }

    /// The same read, over the widened pool. See [`catalog_pool_query`].
    ///
    /// Separate method rather than a boolean on the old one, so the flag is
    /// read in exactly one place and the two paths are legible side by side in
    /// a stack trace.
    pub async fn catalog_pool(&self) -> Result<Vec<CapabilityOffer>> {
        Ok(sqlx::query_as::<_, CapabilityOffer>(&catalog_pool_query())
            .fetch_all(&self.pool)
            .await?)
    }

    /// One capability, or `None` when the slug is unknown or retired.
    pub async fn get_capability(&self, slug: &str) -> Result<Option<CapabilityInfo>> {
        Ok(self
            .list_capabilities()
            .await?
            .into_iter()
            .find(|c| c.slug == slug))
    }

    /// The candidates for one capability: every servable offer, ordered by
    /// `sort`, with the call target and input mapping resolved.
    pub async fn list_capability_offers(
        &self,
        slug: &str,
        sort: &CapabilitySort,
        max_price: Option<f64>,
    ) -> Result<Vec<CapabilityOffer>> {
        self.list_capability_offers_many(std::slice::from_ref(&slug.to_string()), sort, max_price)
            .await
    }

    /// One endpoint, as an offer, WITHOUT a capability behind it.
    ///
    /// The capability layer answers "what performs this job", and it only knows
    /// about endpoints a human has mapped: 47 of the catalogue's 1,510 today.
    /// That is the right gate for a job someone described in words — a wrong
    /// match costs a purchase — and the wrong one for an endpoint someone
    /// READ and pressed the button on. There the choice is already made, by a
    /// person, against the endpoint's own published schema and price; asking a
    /// capability matcher to re-derive it can only get it wrong.
    ///
    /// So this resolves an `api_endpoints.id` straight to the same
    /// [`CapabilityOffer`] the mapped path produces, so everything downstream
    /// of the decision — the price ceiling, the funding check, the binding, the
    /// loopback execution, the fee arithmetic — is shared code and not a second
    /// implementation. `capability_slug` is empty and `source` is `direct`,
    /// which is exactly what happened: no capability was involved.
    ///
    /// The visibility predicate is the catalogue's own, so an endpoint that
    /// `/v1/apis` hides cannot be executed by naming its id.
    pub async fn direct_offer(&self, endpoint_id: Uuid) -> Result<Option<CapabilityOffer>> {
        let provider_join = listing_provider_join();
        let sql = format!(
            r#"SELECT
                 {PROVIDER_IDENTITY_COLUMNS}
                 a.slug as api_slug,
                 COALESCE(a.name, INITCAP(REPLACE(a.slug, '-', ' '))) as api_name,
                 ''::text as capability_slug,
                 e.id as endpoint_id,
                 e.method as endpoint_method,
                 e.path   as endpoint_path,
                 NULLIF(e.description, '') as endpoint_description,
                 e.price::double precision as price_usd,
                 e.payment_required,
                 (COALESCE(a.listing_type::text, 'api') = 'federated') as hosted_externally,
                 {ENDPOINT_CALL_URL} as call_url,
                 '{{}}'::jsonb as input_map,
                 '{{}}'::jsonb as fixed_params,
                 -- Both halves of the input schema, exactly as the two
                 -- capability queries above read them. Missing here, this
                 -- query still compiled and still ran -- and then every
                 -- `FromRow` for `CapabilityOffer` failed at runtime with
                 -- "no column found for name: request_body". That is the
                 -- lookup behind `POST /v1/do` with a `candidate_id`, which
                 -- is how the app calls EVERY reviewed listing, so the whole
                 -- catalogue answered 500 while each listing looked fine.
                 COALESCE(e.request_body, '{{}}'::jsonb) as request_body,
                 COALESCE(e.query_params, '{{}}'::jsonb) as query_params,
                 NULL::text as search_text,
                 NULL::text as notes,
                 'direct'::text as source,
                 1.0::double precision as confidence
               FROM api_endpoints e
               JOIN apis a ON a.id = e.api_id
               LEFT JOIN provider_profiles p ON p.id = a.owner_id
               {provider_join}
               WHERE e.id = $1
                 AND COALESCE(e.active, TRUE)
                 AND {CATALOG_VISIBILITY}"#
        );

        Ok(sqlx::query_as::<_, CapabilityOffer>(&sql)
            .bind(endpoint_id)
            .fetch_optional(&self.pool)
            .await?)
    }

    /// Every catalogue ENDPOINT matching these words, as offers.
    ///
    /// The capability layer can only answer for the endpoints a human has
    /// mapped to one of seventeen capabilities. Measured on the live
    /// catalogue that is 47 endpoints of 1,510 — so "what can do this job"
    /// answered from capabilities alone leaves 97% of Apiosk's own shelf out
    /// of Apiosk's own search, and answers a question about flight status with
    /// an FX rate because that is the nearest capability the words matched.
    /// The external sweep, meanwhile, reads every index end to end and matches
    /// on words. The two halves of one answer were not searching the same way.
    ///
    /// This is the catalogue searched the way the sweep searches: term by
    /// term, over what the endpoint and its listing actually say. Same
    /// [`CapabilityOffer`] shape, same visibility predicate, same call target
    /// — the rows come out ready for the identical scoring, binding and
    /// settlement path, with no capability behind them (`capability_slug` is
    /// empty, `source` is `catalog`).
    ///
    /// `confidence` carries the hit count so the caller can rank without
    /// re-deriving it. Ordered by hits, then by price ascending, so the
    /// cheapest of an equally good match leads — the ordering `/v1/discover`
    /// already publishes.
    ///
    /// At most [`MAX_PER_LISTING`] endpoints of any one listing survive.
    /// Aviationstack publishes nine endpoints that all say "aviation", and
    /// without the cap a search for a flight returns Aviationstack nine times
    /// and nothing else — a page of one seller dressed as a comparison. The
    /// listing's own page is one click away and lists all nine.
    pub async fn search_catalog_endpoints(
        &self,
        terms: &[String],
        max_price: Option<f64>,
        limit: i64,
    ) -> Result<Vec<CapabilityOffer>> {
        // No words is not "everything": a term-scored search with no terms
        // scores every row zero, and returning the catalogue in price order
        // would dress a non-answer up as a match.
        let terms: Vec<String> = terms
            .iter()
            .map(|t| t.trim().to_ascii_lowercase())
            .filter(|t| t.len() >= 3)
            .take(24)
            .collect();
        if terms.is_empty() {
            return Ok(Vec::new());
        }

        let sql = catalog_search_query();

        Ok(sqlx::query_as::<_, CapabilityOffer>(&sql)
            .bind(terms)
            .bind(max_price)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?)
    }

    /// Offers named by endpoint id, in the order the ids were given.
    ///
    /// The link that makes discover -> compare -> decide a pipeline rather than
    /// three endpoints that each re-derive from scratch: a candidate carries an
    /// id, and the next step resolves it back to the offer without re-running
    /// the match. Order is preserved so a caller's ranking survives the trip.
    pub async fn capability_offers_by_endpoint(
        &self,
        endpoint_ids: &[Uuid],
    ) -> Result<Vec<CapabilityOffer>> {
        if endpoint_ids.is_empty() {
            return Ok(Vec::new());
        }
        let sql = capability_offers_query(&CapabilitySort::Price).replace(
            "co.capability_slug = ANY($1)",
            "co.endpoint_id = ANY($1::uuid[])",
        );
        let rows = sqlx::query_as::<_, CapabilityOffer>(&sql)
            .bind(endpoint_ids)
            .bind(Option::<f64>::None)
            .fetch_all(&self.pool)
            .await?;

        // Restore the caller's order; SQL returned them grouped by capability.
        let mut by_id: std::collections::HashMap<Uuid, CapabilityOffer> = rows
            .into_iter()
            .filter_map(|r| r.endpoint_id.map(|id| (id, r)))
            .collect();
        Ok(endpoint_ids
            .iter()
            .filter_map(|id| by_id.remove(id))
            .collect())
    }

    /// Offers for several capabilities in one round trip.
    ///
    /// `/v1/search` matches up to four capabilities and used to run one query
    /// per match. The rows are ordered so each capability's offers stay
    /// contiguous and in rank order, so the caller can split them without
    /// re-sorting.
    pub async fn list_capability_offers_many(
        &self,
        slugs: &[String],
        sort: &CapabilitySort,
        max_price: Option<f64>,
    ) -> Result<Vec<CapabilityOffer>> {
        if slugs.is_empty() {
            return Ok(Vec::new());
        }
        Ok(
            sqlx::query_as::<_, CapabilityOffer>(&capability_offers_query(sort))
                .bind(slugs)
                .bind(max_price)
                .fetch_all(&self.pool)
                .await?,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_defaults_to_price_and_discloses_it() {
        // An unknown, absent or empty value must not silently fall through to
        // the catalog's commercial ordering.
        for value in [None, Some(""), Some("nonsense"), Some("price")] {
            let sort = CapabilitySort::parse(value);
            assert!(
                matches!(sort, CapabilitySort::Price),
                "{value:?} should parse to price"
            );
            assert!(sort.order_clause().starts_with("e.price ASC"));
            assert!(sort.note().starts_with("price-ascending"));
        }
    }

    #[test]
    fn managed_first_is_opt_in_and_disclosed() {
        let sort = CapabilitySort::parse(Some("managed_first"));
        assert!(matches!(sort, CapabilitySort::ManagedFirst));
        assert!(sort.order_clause().starts_with(MANAGED_FIRST));
        assert!(sort.note().starts_with("managed-first"));
    }

    #[test]
    fn inferred_assignments_are_never_served() {
        // The guard is structural: no request can widen it, so assert the
        // predicate itself rather than a caller path.
        assert!(!SERVED_SOURCES.contains("inferred"));
        assert!(SERVED_SOURCES.contains("curated"));
        assert!(SERVED_SOURCES.contains("declared"));
    }

    /// Every column `CapabilityOffer` reads, by the name `FromRow` looks it up
    /// under. Adding a field to the struct without adding it here is the only
    /// way this list can go stale, and that is the point: the next person to
    /// add one has to say so in a place that fails four queries at once.
    const OFFER_COLUMNS: &[&str] = &[
        "capability_slug",
        "endpoint_id",
        "api_slug",
        "api_name",
        "endpoint_method",
        "endpoint_path",
        "endpoint_description",
        "price_usd",
        "payment_required",
        "hosted_externally",
        "call_url",
        "input_map",
        "fixed_params",
        "request_body",
        "query_params",
        "notes",
        "source",
        "confidence",
        "provider_name",
        "provider_username",
    ];

    #[test]
    fn every_offer_query_selects_every_column_the_struct_reads() {
        // sqlx resolves `CapabilityOffer`'s fields by NAME at runtime, so a
        // query that omits one does not fail to compile — it fails in
        // production, as `no column found for name: <field>`, with a 500 and
        // no body. That is what `request_body` and `query_params` did to every
        // `POST /v1/do` for a named candidate: the two builders that back
        // /v1/capabilities were updated when the fields landed, the two here
        // were not, and a buyer's reservation was HELD on each one.
        //
        // Four builders, one list. A field added to the struct and to three of
        // them now fails the fourth here rather than at a user's expense.
        let queries = [
            (
                "capability_offers",
                capability_offers_query(&CapabilitySort::Price),
            ),
            ("catalog_snapshot", catalog_snapshot_query()),
            ("catalog_pool", catalog_pool_query()),
            ("direct_offer", direct_offer_query()),
            ("catalog_search", catalog_search_query()),
        ];
        for (name, sql) in queries {
            for column in OFFER_COLUMNS {
                assert!(
                    sql.contains(&format!("{column},")) || sql.contains(&format!("as {column}")),
                    "{name} does not select `{column}`, which CapabilityOffer reads"
                );
            }
        }
    }

    #[test]
    fn the_pool_is_rooted_in_endpoints_not_in_mappings() {
        // The whole rework in one assertion.
        //
        // `catalog_snapshot_query` reads `FROM capability_offers ... JOIN
        // api_endpoints`. An endpoint with no mapping row cannot come out of
        // that query — not ranked low, not filtered late, never loaded. Since
        // that snapshot is the entire pool behind `/v1/ask`, and `/v1/ask` is
        // what `apiosk` and `apiosk_discover` call, a user naming a source we
        // actually hold gets told it does not exist.
        //
        // The pool query must therefore start at the endpoints and pick up a
        // capability only if one happens to be mapped.
        let pool = catalog_pool_query();

        let from = pool
            .split_whitespace()
            .skip_while(|word| !word.eq_ignore_ascii_case("FROM"))
            .nth(1)
            .expect("the pool query has a FROM");
        assert_eq!(
            from, "api_endpoints",
            "the pool must start at the endpoints, not at the mappings"
        );
        assert!(
            pool.contains("LEFT JOIN capability_offers"),
            "a mapping may only ever be OPTIONAL in the pool"
        );
        assert!(
            !pool.contains("JOIN api_endpoints"),
            "joining TO api_endpoints means something else is the root again"
        );

        // And the old shape is kept honest about what it is, so nobody
        // "simplifies" the two back together by accident.
        assert!(
            catalog_snapshot_query().contains("FROM capability_offers"),
            "catalog_snapshot_query is the pre-rework pool; if this changed, \
             the flagged path and the default path are no longer different"
        );
    }

    #[test]
    fn an_unmapped_endpoint_is_labelled_the_way_the_catalogue_lane_labels_one() {
        // `CapabilityOffer` is not widened for this. An endpoint with no served
        // mapping is emitted exactly as `search_catalog_endpoints` has always
        // emitted one, so every existing reader — `is_external`, `as_offer`,
        // the shortlist dedup — needs no new case.
        let pool = catalog_pool_query();
        assert!(pool.contains("COALESCE(co.capability_slug, '') as capability_slug"));
        assert!(pool.contains("COALESCE(co.source, 'catalog') as source"));
        assert!(pool.contains("COALESCE(co.input_map, '{}'::jsonb) as input_map"));
    }

    #[test]
    fn widening_the_pool_does_not_widen_what_may_be_sold() {
        // The one change that could put an unreviewed guess in front of a
        // buyer. SERVED_SOURCES moves from the WHERE to the JOIN: it stops
        // deciding whether an endpoint is VISIBLE and keeps deciding whether a
        // capability is SERVED.
        let pool = catalog_pool_query();
        let join = pool
            .split("LEFT JOIN capability_offers")
            .nth(1)
            .expect("the pool left-joins the mappings");
        let on_clause = join.split("LEFT JOIN provider_profiles").next().unwrap();
        assert!(
            on_clause.contains(SERVED_SOURCES),
            "an unreviewed mapping must not be served: {on_clause}"
        );
        // Visibility and admission are still applied, verbatim.
        assert!(pool.contains(CATALOG_VISIBILITY));
        assert!(pool.contains(ENDPOINT_ADMISSION));
    }

    #[test]
    fn no_offer_ever_resolves_to_a_provider_url() {
        // This test asserted the exact opposite until the gateway mask landed:
        // that a federated endpoint resolved to the provider's own resource,
        // never a gateway URL. That was a published route around the buyer's
        // 15% — an agent reading `call_url` would call the provider directly at
        // list price.
        //
        // The call target is now unconditional. Asserting the SQL never reaches
        // into `external_x402` is the structural guard: that column is where
        // every provider origin and payTo lives, so a query that does not read
        // it cannot leak one.
        let sql = capability_offers_query(&CapabilitySort::Price);
        assert!(
            !sql.contains("external_x402"),
            "the offers query must not read provider resources: {sql}"
        );
        assert!(sql.contains("gateway.apiosk.com"));
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;

    #[test]
    fn offers_carry_their_capability_and_stay_grouped() {
        // Several capabilities come back in one result set, so a row has to say
        // which one it belongs to and the order has to keep them contiguous.
        let sql = capability_offers_query(&CapabilitySort::Price);
        assert!(sql.contains("co.capability_slug,"));
        assert!(sql.contains("co.capability_slug = ANY($1)"));
        assert!(sql.contains("ORDER BY co.capability_slug ASC"));
    }

    #[test]
    fn the_rank_order_survives_the_grouping() {
        // Grouping must not replace the ranking, only precede it.
        let sql = capability_offers_query(&CapabilitySort::Price);
        let order = sql.split("ORDER BY").nth(1).expect("has an order clause");
        assert!(order.contains("e.price ASC"));
    }
}
