use super::*;

/// Directory handle for listings whose owner has no `provider_profiles` row
/// *and* no provider identity of their own (see [`SELF_SERVE_PROVIDER`]).
///
/// Self-serve federated submissions (`upsert_federated_submission`) are owned by
/// the synthetic community sentinel `00000000-…-0001`, which cannot have a
/// profile: `provider_profiles.id` is a FK to `auth.users(id)` and no auth user
/// exists for the sentinel. Because the directory queries used to INNER JOIN
/// `provider_profiles`, every such listing was live in `/v1/apis` and
/// `/.well-known/x402` yet absent from `/v1/providers` — invisible to anyone
/// browsing. Whatever is left after the self-identified listings have been
/// pulled out into providers of their own (imports such as `apiosk-mcp-tools`,
/// legacy `create_community_api` rows) is grouped under this synthetic provider.
///
/// A real provider that registers this username wins: the synthetic bucket is
/// suppressed whenever a `provider_profiles` row already claims it, so the
/// handle always resolves to exactly one provider page.
pub const UNAFFILIATED_PROVIDER_USERNAME: &str = "community";

/// Display name for [`UNAFFILIATED_PROVIDER_USERNAME`].
pub const UNAFFILIATED_PROVIDER_NAME: &str = "Community providers";

/// SQL predicate — TRUE for a listing that has no `provider_profiles` row behind
/// it yet carries its own provider identity in `listing_metadata`.
///
/// A self-serve federated submission arrives with the submitter's display name,
/// logo, website and contact email in `listing_metadata` precisely *because*
/// there is no profile row to hold them (see `upsert_federated_submission`).
/// Those are real, independent businesses, so they get a provider card and a
/// provider page of their own — keyed by the listing slug — instead of being
/// pooled behind the one [`UNAFFILIATED_PROVIDER_NAME`] card, where nobody
/// browsing the directory would ever find them by name.
///
/// The slug must still be free: a `provider_profiles` row holding that username
/// owns it outright (same rule the bucket handle follows), and such a listing
/// falls back into the bucket so `/v1/providers/:username` never resolves two
/// ways.
///
/// Expects `apis a` LEFT JOINed to `provider_profiles p`. `->>'source'` is
/// COALESCEd because a NULL here would poison the negated form used to select
/// the bucket's members.
const SELF_SERVE_PROVIDER: &str = r#"(p.id IS NULL OR COALESCE(p.username, '') = '')
                     AND COALESCE(a.listing_metadata->>'source', '') = 'self-serve-federated'
                     AND NOT EXISTS (
                       SELECT 1 FROM provider_profiles pp
                       WHERE lower(pp.username) = lower(a.slug)
                     )"#;

/// SQL predicate — TRUE for a listing Apiosk SELLS rather than resells.
///
/// `is_platform_owned` marks the listings where Apiosk is the seller and there
/// is no third-party provider taking a share: the supplier bills Apiosk on a
/// contract or a monthly invoice, and the whole ticket is Apiosk's own revenue
/// (`admin/src/lib/kpis.ts`: `own_api_revenue` against `take_rate_revenue`).
/// KVK and Sendcloud are the two.
///
/// Those get a card of their own on `/v1/providers`, keyed by slug, exactly as
/// a [`SELF_SERVE_PROVIDER`] listing does. Without this they were folded into
/// the reseller account that happens to own them — `apiosk-basics` — behind one
/// card counting dozens of unrelated third-party APIs, where a reader browsing
/// for the Dutch business register could not find it by name. A product Apiosk
/// sells under its own contract is a source in its own right; the resold shelf
/// is not, and stays pooled behind the one account card.
///
/// The slug must still be free, for the same reason the self-serve branch
/// checks: a `provider_profiles` row holding that username owns the handle
/// outright, and `/v1/providers/:username` must never resolve two ways.
///
/// Expects `apis a` (with `is_platform_owned` selected).
const FIRST_PARTY_PROVIDER: &str = r#"COALESCE(a.is_platform_owned, FALSE)
                     AND NOT EXISTS (
                       SELECT 1 FROM provider_profiles pp
                       WHERE lower(pp.username) = lower(a.slug)
                     )"#;

/// The five `provider_*` catalog columns, resolved from the listing's OWN
/// identity where it has one (joined as `sp` — see [`listing_provider_join`])
/// and from the owner's profile otherwise.
///
/// `sp` FIRST, and that order is the whole point. Two kinds of listing carry an
/// identity of their own — a [`SELF_SERVE_PROVIDER`] submission and a
/// [`FIRST_PARTY_PROVIDER`] product — and only the first of those has an empty
/// owner profile to fall back past. A first-party row is owned by the reseller
/// account (`apiosk-basics`), which has a display name, a bio and an avatar, so
/// an owner-first COALESCE never reached `sp` at all: `/v1/providers/
/// kvk-dutch-business-register` resolved to the KVK listing (see
/// [`provider_scope_predicate`]) and then titled the page "Apiosk Basics",
/// described it as "Default API's provided by Apiosk." and hung Apiosk's mark
/// beside it. The card in the directory listing already got this right — see
/// the FIRST-PARTY branch of the providers query — and the two disagreeing is
/// what sent a reader from a card named KVK to a page named something else.
///
/// Ordinary third-party listings are untouched: `sp` yields no row for them, so
/// the owner's profile is still the only identity there is.
pub(crate) const PROVIDER_IDENTITY_COLUMNS: &str = r#"COALESCE(sp.name, NULLIF(COALESCE(p.display_name, p.username), '')) as provider_name,
                 COALESCE(sp.username, NULLIF(p.username, '')) as provider_username,
                 COALESCE(sp.avatar_url, NULLIF(p.avatar_url, '')) as provider_avatar_url,
                 COALESCE(sp.bio, NULLIF(p.bio, '')) as provider_bio,
                 COALESCE(sp.sector, NULLIF(p.sector, '')) as provider_sector,"#;

/// LATERAL join yielding the listing's own provider identity — one row where the
/// listing IS a provider ([`SELF_SERVE_PROVIDER`] or [`FIRST_PARTY_PROVIDER`]),
/// none otherwise. Must follow both `apis a` and the `provider_profiles p` join
/// in the FROM clause.
///
/// Both branches are keyed by the listing slug and read the same fields, which
/// is what lets one join serve them: a submitter's identity at intake and a
/// first-party product's own name and mark land in the same columns, and
/// `listing_metadata->>'logo'` is where each keeps its avatar.
pub(crate) fn listing_provider_join() -> String {
    format!(
        r#"LEFT JOIN LATERAL (
                 SELECT
                   COALESCE(NULLIF(a.listing_metadata->>'name', ''), a.name, a.slug) as name,
                   a.slug as username,
                   NULLIF(a.listing_metadata->>'logo', '') as avatar_url,
                   NULLIF(a.description, '') as bio,
                   NULLIF(a.category, '') as sector
                 WHERE ({SELF_SERVE_PROVIDER}) OR ({FIRST_PARTY_PROVIDER})
               ) sp ON TRUE"#
    )
}

/// SQL predicate resolving the `/v1/providers/:username` handle (`$1`) to the
/// listings that belong on that page: a profiled provider's own listings, a
/// self-serve submission published under its slug, or — for the bucket handle
/// (`bucket_param`) — every unaffiliated listing without an identity of its own.
///
/// Expects `apis a` LEFT JOINed to `provider_profiles p`.
fn provider_scope_predicate(bucket_param: &str) -> String {
    format!(
        r#"(
                   -- A first-party listing answers to its OWN slug, and is
                   -- checked before the owner's handle so it does not also
                   -- appear on the reseller account's page. See
                   -- FIRST_PARTY_PROVIDER.
                   (lower(a.slug) = lower($1) AND {FIRST_PARTY_PROVIDER})
                   OR (p.username IS NOT NULL AND p.username <> '' AND lower(p.username) = lower($1)
                       AND NOT COALESCE(a.is_platform_owned, FALSE))
                   OR (
                     (p.id IS NULL OR COALESCE(p.username, '') = '')
                     AND NOT EXISTS (
                       SELECT 1 FROM provider_profiles pp
                       WHERE lower(pp.username) = lower($1)
                     )
                     AND (
                       -- The listing IS the provider: a self-serve submission is
                       -- published under its own slug.
                       (lower(a.slug) = lower($1) AND {SELF_SERVE_PROVIDER})
                       -- Everything else unaffiliated falls to the bucket.
                       OR (
                         lower($1) = lower({bucket_param})
                         AND NOT ({SELF_SERVE_PROVIDER})
                       )
                     )
                   )
                 )"#
    )
}

/// Primary ranking key for every catalog surface: managed listings first,
/// federated ones after.
///
/// A managed listing is proxied and settled by the gateway — the call is paid
/// through Apiosk. A federated listing is only indexed: the agent pays the
/// provider's own host directly and Apiosk earns nothing from it. Discovery
/// placement is the one lever that costs a federated provider something real,
/// so it is the incentive to move settlement over: publish through Apiosk and
/// your listings rank above the federated index, everywhere an agent looks
/// (`/v1/apis`, `/types/:type`, provider pages and — the surface crawlers
/// actually read — `/.well-known/x402`, where the top of the document also
/// gets the documentation budget first).
///
/// Applied ahead of the caller's `sort`, never instead of it: filters
/// (`min_price`, `category`, …) still bind exactly, and within each of the two
/// groups the requested order holds. Published as `meta.ranking` on the
/// catalog and discovery documents so the preference is disclosed rather than
/// silent.
///
/// Keyed on `listing_type` — the single authoritative federated discriminator
/// (`is_external` is only a denormalised mirror).
///
/// Expects `apis a` in scope.
pub(crate) const MANAGED_FIRST: &str =
    "(COALESCE(a.listing_type::text, 'api') <> 'federated') DESC";

/// One-line disclosure of [`MANAGED_FIRST`], published next to the results.
pub const RANKING_NOTE: &str = "managed-first: listings Apiosk publishes and operates rank above federated listings it only executes; every listing is called and settled through the gateway, and the requested sort applies within each group";

/// Where an agent sends the request for one endpoint: **always the gateway**.
///
/// This expression used to branch on `listing_type` and hand out the provider's
/// own `resource` from `apis.external_x402` for a federated listing, because the
/// gateway had no route for a federated slug and would 404.
///
/// That branch is gone. Under the hybrid fee model the gateway proxies every
/// listing, and publishing the provider's address is a published route around
/// the buyer's 15%: an agent that reads it calls the provider directly, pays
/// list price, and Apiosk sees nothing. The provider's origin is no longer
/// emitted on any surface — see `well_known::append_federated_items` and
/// `v1_routes::catalog`, which mask the same address in the discovery document
/// and on the detail page.
///
/// Expects `apis a` and `api_endpoints e` in scope.
pub(crate) const ENDPOINT_CALL_URL: &str = "'https://gateway.apiosk.com/' || a.slug || e.path";

/// The catalog's `price_usd`: the cheapest paid endpoint on the listing, 0 when
/// it has none. Also the key `sort=price` orders on, so the page query has to
/// compute it too — hence one definition rather than a copy per call site.
///
/// Expects `apis a` in scope.
const MIN_PAID_PRICE: &str = r#"COALESCE(
                   (SELECT MIN(e.price) FROM api_endpoints e WHERE e.api_id = a.id AND e.payment_required = true AND COALESCE(e.active, true)),
                   0
                 )"#;

/// Whether a listing is in the catalog AT ALL, independent of any caller
/// filter. A `WHERE` body over `apis a` that binds nothing.
///
/// Split out of [`CATALOG_FILTERS`] so every surface that publishes listings
/// answers "is this listing public?" with one predicate. `/v1/capabilities`
/// (see [`super::capabilities`]) selects endpoints rather than listings and so
/// cannot reuse the `$1..$8` filter body, but it must not develop a second
/// opinion about visibility: a capability that offered a de-listed or
/// unverified API would route an agent to something the catalog deliberately
/// hides.
///
/// The connected-payout-wallet gate that used to live here is gone. It dated
/// from the era where a provider connected their own wallet and Apiosk paid it
/// directly; payouts now run through the treasury, so a `payout_wallets` row is
/// no longer a precondition for a listing being real. Keeping the predicate
/// hid live listings for a reason that no longer exists — silently, since the
/// filter has no user-facing voice. Dropped alongside the matching publish
/// guard in provider migration
/// `20260901_drop_connected_payout_wallet_requirement.sql`.
/// `discoverable` kwam erbij toen de catalogus werd teruggebracht tot zes bronnen met
/// keurig gedocumenteerde API's. De drie regels erboven zeggen of een listing *echt* is;
/// deze zegt of we hem *aanbieden*. Dat onderscheid is opzettelijk: een listing uitzetten
/// voor discovery mag geen gevolgen hebben voor een lopende integratie die hem al bij naam
/// kent, en moet met één UPDATE terug te draaien zijn.
///
/// De kolom staat standaard op TRUE, dus voor bestaande rijen verandert er niets tot
/// migratie `095_discoverable_allowlist.sql` de selectie maakt.
pub(crate) const CATALOG_VISIBILITY: &str = r#"a.status = 'active'
                 AND COALESCE(a.active, TRUE) = TRUE
                 AND a.is_verified = TRUE
                 AND COALESCE(a.discoverable, TRUE) = TRUE"#;

/// Of dit ENDPOINT genoeg contract publiceert om eerlijk te kunnen worden
/// aangeboden. Een `WHERE`-fragment over `api_endpoints e`.
///
/// Dit is NIET dezelfde regel als "heeft een capability-koppeling", en die twee
/// verwarren is precies de fout die de herwerking rechtzet:
///
///   Een ontbrekende KOPPELING is ons gat. Het endpoint bestaat, is
///   gedocumenteerd en werkt; wij hebben het alleen nog niet aan een capability
///   gehangen. Dat mag het nooit onvindbaar maken — een gebruiker die een bron
///   bij naam noemt, hoort hem terug te krijgen.
///
///   Ontbrekende DOCUMENTATIE is het gat van de aanbieder. Zonder invoer- en
///   uitvoerschema valt er niets te binden, niets statisch te valideren en
///   niets eerlijk te vergelijken. Voor zo'n bron gokken is hoe een koper voor
///   een 400 betaalt. Die wordt daarom bij binnenkomst geweigerd in plaats van
///   meegerangschikt en achteraf uitgelegd.
///
/// De kolom staat standaard op TRUE (migratie 121), dus voor alles wat vandaag
/// in de catalogus staat verandert hier niets tot de intake hem zelf zet.
pub(crate) const ENDPOINT_ADMISSION: &str = "COALESCE(e.contract_documented, TRUE)";

/// Everything that decides whether a listing is in the catalog at all: the
/// visibility rules, then the caller's filters bound to `$1..$8`. A `WHERE`
/// body over `apis a`.
///
/// Shared verbatim by the count query and the page query because the two MUST
/// agree: a predicate that binds in one and not the other makes `meta.total`
/// disagree with the rows served, and a crawler paginating off that total walks
/// off the end or stops early — silently, since neither response looks wrong on
/// its own.
const CATALOG_USER_FILTERS: &str = r#"(
                   $1::text IS NULL
                   OR a.slug ILIKE '%' || $1 || '%'
                   OR COALESCE(a.name, a.slug) ILIKE '%' || $1 || '%'
                   OR COALESCE(a.description, '') ILIKE '%' || $1 || '%'
                   OR COALESCE(a.category, '') ILIKE '%' || $1 || '%'
                   OR COALESCE(a.listing_type::text, '') ILIKE '%' || $1 || '%'
                   OR EXISTS (
                     SELECT 1
                     FROM jsonb_array_elements_text(COALESCE(a.listing_metadata->'tags', '[]'::jsonb)) AS tags(value)
                     WHERE tags.value ILIKE '%' || $1 || '%'
                   )
                   OR EXISTS (
                     SELECT 1
                     FROM api_endpoints se
                     WHERE se.api_id = a.id
                       AND COALESCE(se.active, true)
                       AND (
                         se.path ILIKE '%' || $1 || '%'
                         OR COALESCE(se.description, '') ILIKE '%' || $1 || '%'
                       )
                   )
                 )
                 AND ($2::text IS NULL OR COALESCE(a.category, 'data') = $2)
                 AND (
                   $3::text IS NULL
                   OR lower(COALESCE(a.category, '')) = lower($3)
                   OR EXISTS (
                     SELECT 1
                     FROM jsonb_array_elements_text(COALESCE(a.listing_metadata->'tags', '[]'::jsonb)) AS tags(value)
                     WHERE lower(tags.value) = lower($3)
                        OR tags.value ILIKE '%' || $3 || '%'
                   )
                 )
                 AND ($4::text IS NULL OR COALESCE(a.listing_type::text, 'api') = $4)
                 AND (
                   $5::double precision IS NULL
                   OR COALESCE((
                     SELECT MIN(e.price::double precision)
                     FROM api_endpoints e
                     WHERE e.api_id = a.id
                       AND e.payment_required = true
                       AND COALESCE(e.active, true)
                   ), 0) >= $5
                 )
                 AND (
                   $6::double precision IS NULL
                   OR COALESCE((
                     SELECT MIN(e.price::double precision)
                     FROM api_endpoints e
                     WHERE e.api_id = a.id
                       AND e.payment_required = true
                       AND COALESCE(e.active, true)
                   ), 0) <= $6
                 )
                 AND ($7::timestamptz IS NULL OR a.created_at >= $7)
                 AND ($8::timestamptz IS NULL OR a.created_at <= $8)
                 -- An EXACT slug, for a caller that already knows which listing
                 -- it wants. `$1` cannot do this job: it is a match, not a
                 -- lookup, and it deliberately reaches into endpoint paths and
                 -- descriptions — so `search=tavily` returns the five
                 -- PulseNetwork listings whose endpoints mention Tavily, sorted
                 -- managed-first, with the Tavily listing itself on the sixth
                 -- row. A client asking for one known listing then reads a page
                 -- of near-misses and concludes the listing is gone.
                 --
                 -- Kept as the LAST filter bind on purpose: `$1..$9` then mean
                 -- the same thing in the count query and the page query, which
                 -- is the property the two must never lose.
                 AND ($9::text IS NULL OR a.slug = $9)"#;

/// [`MANAGED_FIRST`] ahead of the caller's `sort`/`order`, as an `ORDER BY`
/// body. Valid against `apis a` on its own — every key is either a column of
/// `apis` or an output alias both catalog queries define — which is what lets
/// [`catalog_page_query`] sort the page before hydrating it.
fn catalog_order_clause(sort: &str, order: &str) -> String {
    let requested_order = match (sort, order) {
        ("price", "asc") => "price_usd ASC, a.slug ASC",
        ("price", "desc") => "price_usd DESC, a.slug ASC",
        ("newest", "asc") => "a.created_at ASC, a.slug ASC",
        ("newest", "desc") => "a.created_at DESC, a.slug ASC",
        ("name", "desc") => "name DESC, a.slug ASC",
        _ => "name ASC, a.slug ASC",
    };
    format!("{MANAGED_FIRST}, {requested_order}")
}

/// How many listings match the filters, for `meta.total`. Binds `$1..$9`.
fn catalog_count_query() -> String {
    format!(
        r#"SELECT COUNT(*)
               FROM apis a
               WHERE {CATALOG_VISIBILITY} AND {CATALOG_USER_FILTERS}"#
    )
}

/// One page of the catalog, hydrated. Binds `$1..$9` for the filters, `$10`/`$11`
/// for limit/offset.
///
/// Page FIRST, hydrate SECOND. `endpoint_docs` aggregates every endpoint of a
/// listing — with its schemas, examples, markdown and pricing rules — into
/// jsonb, and it is essentially the whole cost of this query. Selected alongside
/// the ORDER BY, Postgres built that jsonb for every row matching the filters,
/// sorted rows weighing kilobytes each, then threw all but `LIMIT` of them away:
/// one page of `/v1/apis` paid for the entire catalog. Under crawler load that
/// crossed the 5s `statement_timeout` and the catalog 500'd
/// (APIOSK-GATEWAY-38, 2026-08-04).
///
/// The CTE resolves the page against `apis` alone — the ordering keys are cheap
/// and the sort runs over narrow tuples — and only those rows are hydrated.
/// MATERIALIZED so the fence holds regardless of how the planner would otherwise
/// inline a single-reference CTE.
///
/// The outer `ORDER BY` is not redundant: a join over the CTE is not obliged to
/// preserve its row order, so the page would come back scrambled without it.
fn catalog_page_query(order_clause: &str) -> String {
    format!(
        r#"WITH page AS MATERIALIZED (
                 SELECT
                   a.id,
                   COALESCE(a.name, INITCAP(REPLACE(a.slug, '-', ' '))) as name,
                   {price} as price_usd
                 FROM apis a
                 WHERE {CATALOG_VISIBILITY} AND {CATALOG_USER_FILTERS}
                 ORDER BY {order_clause}
                 LIMIT $10 OFFSET $11
               )
               SELECT
                 a.slug as id,
                 a.slug,
                 COALESCE(a.name, INITCAP(REPLACE(a.slug, '-', ' '))) as name,
                 COALESCE(a.description, 'API endpoint for ' || a.slug) as description,
                 {price} as price_usd,
                 COALESCE(a.category, 'data') as category,
                 COALESCE(
                   NULLIF(a.listing_metadata->>'implementation_type', ''),
                   CASE
                     WHEN COALESCE(a.category, 'data') = 'compute' THEN 'deterministic'
                     ELSE 'direct'
                   END
                 ) as implementation_type,
                 COALESCE((a.listing_metadata->>'model_backed')::boolean, false) as model_backed,
                 COALESCE(a.listing_type::text, 'api') as listing_type,
                 COALESCE(a.docs_url, 'https://apiosk.com/docs/' || a.slug) as docs_url,
                 COALESCE(a.endpoint_url, a.origin_url) as origin_url,
                 ('https://gateway.apiosk.com/' || a.slug) as gateway_url,
                 COALESCE(a.active, true) as active,
                 COALESCE(a.is_verified, false) as verified,
                 COALESCE((SELECT COUNT(*) FROM api_endpoints e WHERE e.api_id = a.id AND COALESCE(e.active, true)), 0)::BIGINT as endpoint_count,
                 COALESCE((
                   SELECT jsonb_agg(
                     jsonb_build_object(
                       -- `api_endpoints.id` — what POST /v1/do executes a call
                       -- by. Without it a browsed endpoint can be read and not
                       -- bought, which is what /explore was until now.
                       'candidate_id', e.id,
                       'method', e.method,
                       'path', e.path,
                       'full_url', COALESCE({ENDPOINT_CALL_URL}, ''),
                       'description', e.description,
                       'price_usd', e.price,
                       'payment_required', e.payment_required,
                       'request_body', e.request_body,
                       'response_body', e.response_body,
                       'request_example', e.request_example,
                       'response_example', e.response_example,
                       'headers', e.headers,
                       'query_params', e.query_params,
                       'documentation_markdown', e.documentation_markdown,
                       'external_docs_url', e.external_docs_url,
                       'openapi_url', e.openapi_url,
                       'authentication_notes', e.authentication_notes,
                       'pricing_rules', COALESCE((SELECT jsonb_agg(jsonb_build_object(
                         'parameter_name', r.parameter_name,
                         'operator', r.operator,
                         'value', r.comparison_value,
                         'price', r.price,
                         'priority', r.priority
                       ) ORDER BY r.priority, r.created_at) FROM endpoint_pricing_rules r WHERE r.endpoint_id = e.id AND r.active = true), '[]'::jsonb),
                       'network_token_id', e.network_token_id
                     )
                     ORDER BY e.path ASC, e.method ASC
                   )
                   FROM api_endpoints e
                   WHERE e.api_id = a.id AND COALESCE(e.active, true)
                 ), '[]'::jsonb) as endpoint_docs,
                 COALESCE(a.listing_metadata, '{{}}'::jsonb) as listing_metadata,
                 COALESCE(a.is_external, FALSE) as is_external,
                 COALESCE(a.external_x402, '[]'::jsonb) as external_x402,
                 {provider_columns}
                 a.created_at
               FROM page
               JOIN apis a ON a.id = page.id
               LEFT JOIN provider_profiles p ON p.id = a.owner_id
               {listing_join}
               ORDER BY {order_clause}"#,
        price = MIN_PAID_PRICE,
        provider_columns = PROVIDER_IDENTITY_COLUMNS,
        listing_join = listing_provider_join(),
    )
}

impl Database {
    #[allow(clippy::too_many_arguments)]
    pub async fn list_active_apis(
        &self,
        search: Option<&str>,
        category: Option<&str>,
        tag: Option<&str>,
        listing_type: Option<&str>,
        min_price: Option<f64>,
        max_price: Option<f64>,
        created_after: Option<chrono::DateTime<chrono::Utc>>,
        created_before: Option<chrono::DateTime<chrono::Utc>>,
        // One exact slug, for a caller that knows which listing it wants.
        // Applied on top of every other filter, so an invisible listing stays
        // invisible: this is a narrower question, never a way past the
        // catalogue's visibility rules.
        slug: Option<&str>,
        sort: &str,
        order: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<ApiInfo>, i64)> {
        // Managed above federated, then whatever the caller asked for.
        let order_clause = catalog_order_clause(sort, order);

        let total: i64 = sqlx::query_scalar(&catalog_count_query())
            .bind(search)
            .bind(category)
            .bind(tag)
            .bind(listing_type)
            .bind(min_price)
            .bind(max_price)
            .bind(created_after)
            .bind(created_before)
            .bind(slug)
            .fetch_one(&self.pool)
            .await?;

        let data_query = catalog_page_query(&order_clause);

        let apis = sqlx::query_as::<_, ApiInfo>(&data_query)
            .bind(search)
            .bind(category)
            .bind(tag)
            .bind(listing_type)
            .bind(min_price)
            .bind(max_price)
            .bind(created_after)
            .bind(created_before)
            .bind(slug)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?;

        Ok((apis, total))
    }

    /// Provider directory for the catalog landing page: one row per
    /// provider with a public `username` and at least one visible listing
    /// (same visibility rule as `list_active_apis`), plus how many listings
    /// they have. `search` matches the provider's name/username/bio OR any
    /// of their listings' name/description/category, so a provider surfaces
    /// even when only their API content matches.
    pub async fn list_providers(&self, search: Option<&str>) -> Result<Vec<ProviderSummary>> {
        let query = format!(
            r#"WITH visible AS (
                 SELECT a.id, a.owner_id, a.slug, a.name, a.description, a.category,
                        COALESCE(a.is_platform_owned, FALSE) as is_platform_owned,
                        COALESCE(a.listing_type::text, 'api') as listing_type,
                        COALESCE(a.listing_metadata, '{{}}'::jsonb) as listing_metadata
                   FROM apis a
                  WHERE {CATALOG_VISIBILITY}
               ),
               -- What each listing PUBLISHES about itself, counted over its
               -- priced, active endpoints: the checklist every source has to
               -- meet before an agent can call it blind (a named input schema,
               -- 100/103; a `required` list so /v1/ask knows what to ask for,
               -- 099; a sentence per endpoint). `crate::quality` turns these
               -- counts into the stars a card wears, and the counts travel
               -- with them so a reader can be told what is missing.
               --
               -- Priced and active only. A free /health endpoint is not
               -- something a buyer chooses, and an endpoint switched off is
               -- not on offer — counting either would let a card earn stars
               -- for documentation nobody can spend.
               docs AS (
                 SELECT
                   e.api_id,
                   COUNT(*)::bigint AS paid,
                   COUNT(*) FILTER (
                     WHERE COALESCE(e.request_body->'properties', '{{}}'::jsonb) <> '{{}}'::jsonb
                        OR COALESCE(e.query_params->'properties', '{{}}'::jsonb) <> '{{}}'::jsonb
                   )::bigint AS with_inputs,
                   COUNT(*) FILTER (
                     WHERE (jsonb_typeof(e.request_body->'required') = 'array'
                              AND jsonb_array_length(e.request_body->'required') > 0)
                        OR (jsonb_typeof(e.query_params->'required') = 'array'
                              AND jsonb_array_length(e.query_params->'required') > 0)
                   )::bigint AS with_required,
                   COUNT(*) FILTER (
                     WHERE COALESCE(e.description, '') <> ''
                   )::bigint AS with_description
                 FROM api_endpoints e
                 WHERE COALESCE(e.active, TRUE)
                   AND COALESCE(e.payment_required, FALSE)
                 GROUP BY e.api_id
               ),
               -- Listings whose owner has no usable profile (the self-serve
               -- federated sentinel — see UNAFFILIATED_PROVIDER_USERNAME).
               -- Without a branch of their own they are live in /v1/apis but
               -- reachable from no provider card, so browsing never surfaces
               -- them. `self_identified` splits the two kinds apart: a provider
               -- of its own below, or the shared bucket.
               unaffiliated AS (
                 SELECT a.*, ({SELF_SERVE_PROVIDER}) as self_identified
                   FROM visible a
                   LEFT JOIN provider_profiles p ON p.id = a.owner_id
                  WHERE (p.id IS NULL OR COALESCE(p.username, '') = '')
               )
               SELECT * FROM (
                 SELECT
                   COALESCE(NULLIF(p.display_name, ''), p.username) as name,
                   p.username,
                   NULLIF(p.avatar_url, '') as avatar_url,
                   NULLIF(p.bio, '') as bio,
                   NULLIF(p.sector, '') as sector,
                   COUNT(a.id) as api_count,
                   COALESCE(SUM(CASE
                     WHEN jsonb_typeof(a.listing_metadata->'service_groups') = 'array'
                       AND jsonb_array_length(a.listing_metadata->'service_groups') > 0
                     THEN jsonb_array_length(a.listing_metadata->'service_groups')
                     ELSE 1
                   END), 0)::bigint as service_count,
                   COUNT(a.id) FILTER (WHERE a.listing_type = 'federated') as federated_count,
                   COUNT(a.id) FILTER (WHERE a.listing_type <> 'federated') as managed_count,
                   -- An ACCOUNT. It opens as the list of what it publishes,
                   -- which is what the provider page is for.
                   NULL::text as listing_slug,
                   -- Every listing behind this card, so the caller can look up
                   -- what each of them has been measured at without a query
                   -- per row (`Cache::peek_provider_stats`, keyed by slug).
                   array_agg(a.slug) as slugs,
                   COALESCE(SUM(d.paid), 0)::bigint as paid_endpoints,
                   COALESCE(SUM(d.with_inputs), 0)::bigint as endpoints_with_inputs,
                   COALESCE(SUM(d.with_required), 0)::bigint as endpoints_with_required,
                   COALESCE(SUM(d.with_description), 0)::bigint as endpoints_with_description
                 FROM provider_profiles p
                 -- First-party listings are excluded here and carded on their
                 -- own below, so the reseller account's count is what it
                 -- actually offers under that name.
                 JOIN visible a ON a.owner_id = p.id AND NOT a.is_platform_owned
                 LEFT JOIN docs d ON d.api_id = a.id
                 WHERE p.username IS NOT NULL AND p.username <> ''
                   AND (
                     $1::text IS NULL
                     OR p.display_name ILIKE '%' || $1 || '%'
                     OR p.username ILIKE '%' || $1 || '%'
                     OR COALESCE(p.bio, '') ILIKE '%' || $1 || '%'
                     OR EXISTS (
                       SELECT 1 FROM apis a2
                       WHERE a2.owner_id = p.id
                         -- Same exclusion: a hit on a first-party listing must
                         -- surface that listing's card, not the account it
                         -- happens to be filed under.
                         AND NOT COALESCE(a2.is_platform_owned, FALSE)
                         AND (
                           COALESCE(a2.name, a2.slug) ILIKE '%' || $1 || '%'
                           OR COALESCE(a2.description, '') ILIKE '%' || $1 || '%'
                           OR COALESCE(a2.category, '') ILIKE '%' || $1 || '%'
                         )
                     )
                   )
                 GROUP BY p.id, p.display_name, p.username, p.avatar_url, p.bio, p.sector

                 UNION ALL

                 -- One card per FIRST-PARTY listing: what Apiosk sells under
                 -- its own contract, named for the product rather than for the
                 -- account that owns the row. See FIRST_PARTY_PROVIDER.
                 SELECT
                   COALESCE(NULLIF(a.name, ''), a.slug) as name,
                   a.slug as username,
                   NULLIF(a.listing_metadata->>'logo', '') as avatar_url,
                   NULLIF(a.description, '') as bio,
                   NULLIF(a.category, '') as sector,
                   1::bigint as api_count,
                   (CASE
                     WHEN jsonb_typeof(a.listing_metadata->'service_groups') = 'array'
                       AND jsonb_array_length(a.listing_metadata->'service_groups') > 0
                     THEN jsonb_array_length(a.listing_metadata->'service_groups')
                     ELSE 1
                   END)::bigint as service_count,
                   (CASE WHEN a.listing_type = 'federated' THEN 1 ELSE 0 END)::bigint as federated_count,
                   (CASE WHEN a.listing_type <> 'federated' THEN 1 ELSE 0 END)::bigint as managed_count,
                   -- The card IS this listing, so it opens the listing. See
                   -- ProviderSummary::listing_slug: the provider page in
                   -- between held one card repeating the name just clicked,
                   -- under the heading of the ACCOUNT that owns the row.
                   a.slug as listing_slug,
                   ARRAY[a.slug] as slugs,
                   COALESCE(d.paid, 0)::bigint as paid_endpoints,
                   COALESCE(d.with_inputs, 0)::bigint as endpoints_with_inputs,
                   COALESCE(d.with_required, 0)::bigint as endpoints_with_required,
                   COALESCE(d.with_description, 0)::bigint as endpoints_with_description
                 FROM visible a
                 LEFT JOIN docs d ON d.api_id = a.id
                 WHERE {FIRST_PARTY_PROVIDER}
                   AND (
                     $1::text IS NULL
                     OR COALESCE(a.name, a.slug) ILIKE '%' || $1 || '%'
                     OR a.slug ILIKE '%' || $1 || '%'
                     OR COALESCE(a.description, '') ILIKE '%' || $1 || '%'
                     OR COALESCE(a.category, '') ILIKE '%' || $1 || '%'
                   )

                 UNION ALL

                 -- One card per self-identified listing, built from the identity
                 -- the submitter gave at intake (display name, logo) rather than
                 -- from a profile row they cannot have. The handle is the slug,
                 -- so the card links to /v1/providers/<slug>.
                 SELECT
                   COALESCE(NULLIF(a.listing_metadata->>'name', ''), a.name, a.slug) as name,
                   a.slug as username,
                   NULLIF(a.listing_metadata->>'logo', '') as avatar_url,
                   NULLIF(a.description, '') as bio,
                   NULL::text as sector,
                   1::bigint as api_count,
                   (CASE
                     WHEN jsonb_typeof(a.listing_metadata->'service_groups') = 'array'
                       AND jsonb_array_length(a.listing_metadata->'service_groups') > 0
                     THEN jsonb_array_length(a.listing_metadata->'service_groups')
                     ELSE 1
                   END)::bigint as service_count,
                   (CASE WHEN a.listing_type = 'federated' THEN 1 ELSE 0 END)::bigint as federated_count,
                   (CASE WHEN a.listing_type <> 'federated' THEN 1 ELSE 0 END)::bigint as managed_count,
                   -- Same shape as the first-party branch: one listing,
                   -- carded under its own slug, so the card opens it.
                   a.slug as listing_slug,
                   ARRAY[a.slug] as slugs,
                   COALESCE(d.paid, 0)::bigint as paid_endpoints,
                   COALESCE(d.with_inputs, 0)::bigint as endpoints_with_inputs,
                   COALESCE(d.with_required, 0)::bigint as endpoints_with_required,
                   COALESCE(d.with_description, 0)::bigint as endpoints_with_description
                 FROM unaffiliated a
                 LEFT JOIN docs d ON d.api_id = a.id
                 WHERE a.self_identified
                   AND (
                     $1::text IS NULL
                     OR COALESCE(a.listing_metadata->>'name', '') ILIKE '%' || $1 || '%'
                     OR a.slug ILIKE '%' || $1 || '%'
                     OR COALESCE(a.name, a.slug) ILIKE '%' || $1 || '%'
                     OR COALESCE(a.description, '') ILIKE '%' || $1 || '%'
                     OR COALESCE(a.category, '') ILIKE '%' || $1 || '%'
                   )

                 UNION ALL

                 -- Everything else unaffiliated: no identity of its own, so it
                 -- shares the bucket card. HAVING keeps the empty aggregate from
                 -- emitting a 0-listing provider once the bucket runs dry.
                 SELECT
                   $3::text as name,
                   $2::text as username,
                   NULL::text as avatar_url,
                   NULL::text as bio,
                   NULL::text as sector,
                   COUNT(a.id) as api_count,
                   COALESCE(SUM(CASE
                     WHEN jsonb_typeof(a.listing_metadata->'service_groups') = 'array'
                       AND jsonb_array_length(a.listing_metadata->'service_groups') > 0
                     THEN jsonb_array_length(a.listing_metadata->'service_groups')
                     ELSE 1
                   END), 0)::bigint as service_count,
                   COUNT(a.id) FILTER (WHERE a.listing_type = 'federated') as federated_count,
                   COUNT(a.id) FILTER (WHERE a.listing_type <> 'federated') as managed_count,
                   -- The shared bucket holds many listings and no identity of
                   -- its own; it opens as a list, like an account.
                   NULL::text as listing_slug,
                   array_agg(a.slug) as slugs,
                   COALESCE(SUM(d.paid), 0)::bigint as paid_endpoints,
                   COALESCE(SUM(d.with_inputs), 0)::bigint as endpoints_with_inputs,
                   COALESCE(SUM(d.with_required), 0)::bigint as endpoints_with_required,
                   COALESCE(SUM(d.with_description), 0)::bigint as endpoints_with_description
                 FROM unaffiliated a
                 LEFT JOIN docs d ON d.api_id = a.id
                 WHERE NOT a.self_identified
                   -- A real provider holding the handle owns it outright.
                   AND NOT EXISTS (
                     SELECT 1 FROM provider_profiles pp
                     WHERE lower(pp.username) = lower($2)
                   )
                 -- The search matches the BUCKET, never individual rows: a hit
                 -- on one listing must not shrink api_count, exactly as a hit
                 -- on a profiled provider's bio does not shrink theirs.
                 HAVING COUNT(a.id) > 0
                   AND (
                     $1::text IS NULL
                     OR $3::text ILIKE '%' || $1::text || '%'
                     OR $2::text ILIKE '%' || $1::text || '%'
                     OR bool_or(
                          COALESCE(a.name, a.slug) ILIKE '%' || $1::text || '%'
                          OR COALESCE(a.description, '') ILIKE '%' || $1::text || '%'
                          OR COALESCE(a.category, '') ILIKE '%' || $1::text || '%'
                        )
                   )
               ) directory
               -- Managed providers first (see MANAGED_FIRST): a provider with
               -- at least one listing settled through Apiosk outranks one that
               -- only federates, which is exactly the trade the directory
               -- offers — move settlement over, move up the page.
               -- Within each group, biggest catalog first: the directory is a
               -- browse surface, and the provider with the most to call is the
               -- most useful card to put at the top. Name breaks ties so the
               -- order is stable.
               ORDER BY (managed_count > 0) DESC, api_count DESC, name ASC"#
        );

        let providers = sqlx::query_as::<_, ProviderSummary>(&query)
            .bind(search)
            .bind(UNAFFILIATED_PROVIDER_USERNAME)
            .bind(UNAFFILIATED_PROVIDER_NAME)
            .fetch_all(&self.pool)
            .await?;

        Ok(providers)
    }

    /// Active, visible listings owned by the provider with the given public
    /// `username` (case-insensitive). Same visibility + search rules as
    /// `list_active_apis`, scoped to one provider for the
    /// `GET /v1/providers/:username` page.
    #[allow(clippy::too_many_arguments)]
    pub async fn list_active_apis_by_provider(
        &self,
        username: &str,
        search: Option<&str>,
        sort: &str,
        order: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<ApiInfo>, ProviderListingCounts)> {
        let requested_order = match (sort, order) {
            ("price", "asc") => "price_usd ASC, a.slug ASC",
            ("price", "desc") => "price_usd DESC, a.slug ASC",
            ("newest", "asc") => "a.created_at ASC, a.slug ASC",
            ("newest", "desc") => "a.created_at DESC, a.slug ASC",
            ("name", "desc") => "name DESC, a.slug ASC",
            _ => "name ASC, a.slug ASC",
        };
        // Same managed-first rule as the catalog, inside one provider's page:
        // a provider that runs both models sees their managed listings lead.
        let order_clause = format!("{MANAGED_FIRST}, {requested_order}");

        // Counted over the provider's whole visible catalog, never the page:
        // `total` drives pagination and the managed/federated split is
        // published next to it, so the two must share one scope.
        let count_query = format!(
            r#"SELECT
                 COUNT(*) as total,
                 COALESCE(SUM(CASE
                   WHEN jsonb_typeof(a.listing_metadata->'service_groups') = 'array'
                     AND jsonb_array_length(a.listing_metadata->'service_groups') > 0
                   THEN jsonb_array_length(a.listing_metadata->'service_groups')
                   ELSE 1
                 END), 0)::bigint as services,
                 COUNT(*) FILTER (
                   WHERE COALESCE(a.listing_type::text, 'api') = 'federated'
                 ) as federated,
                 COUNT(*) FILTER (
                   WHERE COALESCE(a.listing_type::text, 'api') <> 'federated'
                 ) as managed
               FROM apis a
               LEFT JOIN provider_profiles p ON p.id = a.owner_id
               WHERE {scope}
                 AND {CATALOG_VISIBILITY}
                 AND (
                   $2::text IS NULL
                   OR a.slug ILIKE '%' || $2 || '%'
                   OR COALESCE(a.name, a.slug) ILIKE '%' || $2 || '%'
                   OR COALESCE(a.description, '') ILIKE '%' || $2 || '%'
                   OR COALESCE(a.category, '') ILIKE '%' || $2 || '%'
                 )"#,
            scope = provider_scope_predicate("$3"),
        );

        let counts = sqlx::query_as::<_, ProviderListingCounts>(&count_query)
            .bind(username)
            .bind(search)
            .bind(UNAFFILIATED_PROVIDER_USERNAME)
            .fetch_one(&self.pool)
            .await?;

        let data_query = format!(
            r#"SELECT
                 a.slug as id,
                 a.slug,
                 COALESCE(a.name, INITCAP(REPLACE(a.slug, '-', ' '))) as name,
                 COALESCE(a.description, 'API endpoint for ' || a.slug) as description,
                 COALESCE(
                   (SELECT MIN(e.price) FROM api_endpoints e WHERE e.api_id = a.id AND e.payment_required = true AND COALESCE(e.active, true)),
                   0
                 ) as price_usd,
                 COALESCE(a.category, 'data') as category,
                 COALESCE(
                   NULLIF(a.listing_metadata->>'implementation_type', ''),
                   CASE
                     WHEN COALESCE(a.category, 'data') = 'compute' THEN 'deterministic'
                     ELSE 'direct'
                   END
                 ) as implementation_type,
                 COALESCE((a.listing_metadata->>'model_backed')::boolean, false) as model_backed,
                 COALESCE(a.listing_type::text, 'api') as listing_type,
                 COALESCE(a.docs_url, 'https://apiosk.com/docs/' || a.slug) as docs_url,
                 COALESCE(a.endpoint_url, a.origin_url) as origin_url,
                 ('https://gateway.apiosk.com/' || a.slug) as gateway_url,
                 COALESCE(a.active, true) as active,
                 COALESCE(a.is_verified, false) as verified,
                 COALESCE((SELECT COUNT(*) FROM api_endpoints e WHERE e.api_id = a.id AND COALESCE(e.active, true)), 0)::BIGINT as endpoint_count,
                 COALESCE((
                   SELECT jsonb_agg(
                     jsonb_build_object(
                       -- `api_endpoints.id` — what POST /v1/do executes a call
                       -- by. Without it a browsed endpoint can be read and not
                       -- bought, which is what /explore was until now.
                       'candidate_id', e.id,
                       'method', e.method,
                       'path', e.path,
                       'full_url', COALESCE({ENDPOINT_CALL_URL}, ''),
                       'description', e.description,
                       'price_usd', e.price,
                       'payment_required', e.payment_required,
                       'request_body', e.request_body,
                       'response_body', e.response_body,
                       'request_example', e.request_example,
                       'response_example', e.response_example,
                       'headers', e.headers,
                       'query_params', e.query_params,
                       'documentation_markdown', e.documentation_markdown,
                       'external_docs_url', e.external_docs_url,
                       'openapi_url', e.openapi_url,
                       'authentication_notes', e.authentication_notes,
                       'pricing_rules', COALESCE((SELECT jsonb_agg(jsonb_build_object(
                         'parameter_name', r.parameter_name,
                         'operator', r.operator,
                         'value', r.comparison_value,
                         'price', r.price,
                         'priority', r.priority
                       ) ORDER BY r.priority, r.created_at) FROM endpoint_pricing_rules r WHERE r.endpoint_id = e.id AND r.active = true), '[]'::jsonb),
                       'network_token_id', e.network_token_id
                     )
                     ORDER BY e.path ASC, e.method ASC
                   )
                   FROM api_endpoints e
                   WHERE e.api_id = a.id AND COALESCE(e.active, true)
                 ), '[]'::jsonb) as endpoint_docs,
                 COALESCE(a.listing_metadata, '{{}}'::jsonb) as listing_metadata,
                 COALESCE(a.is_external, FALSE) as is_external,
                 COALESCE(a.external_x402, '[]'::jsonb) as external_x402,
                 {provider_columns}
                 a.created_at
               FROM apis a
               LEFT JOIN provider_profiles p ON p.id = a.owner_id
               {listing_join}
               WHERE {scope}
                 AND {CATALOG_VISIBILITY}
                 AND (
                   $2::text IS NULL
                   OR a.slug ILIKE '%' || $2 || '%'
                   OR COALESCE(a.name, a.slug) ILIKE '%' || $2 || '%'
                   OR COALESCE(a.description, '') ILIKE '%' || $2 || '%'
                   OR COALESCE(a.category, '') ILIKE '%' || $2 || '%'
                 )
               ORDER BY {order_clause}
               LIMIT $3 OFFSET $4"#,
            provider_columns = PROVIDER_IDENTITY_COLUMNS,
            listing_join = listing_provider_join(),
            scope = provider_scope_predicate("$5"),
        );

        let apis = sqlx::query_as::<_, ApiInfo>(&data_query)
            .bind(username)
            .bind(search)
            .bind(limit)
            .bind(offset)
            .bind(UNAFFILIATED_PROVIDER_USERNAME)
            .fetch_all(&self.pool)
            .await?;

        Ok((apis, counts))
    }

    pub async fn get_wallet_usage(
        &self,
        wallet_address: &str,
        period: &str,
    ) -> Result<WalletUsage> {
        let date_filter = match period {
            "today" => "occurred_at >= CURRENT_DATE",
            "week" => "occurred_at >= CURRENT_DATE - INTERVAL '7 days'",
            "month" => "occurred_at >= CURRENT_DATE - INTERVAL '30 days'",
            _ => "TRUE",
        };

        let query_str = format!(
            r#"SELECT
                 COUNT(*) as total_requests,
                 COALESCE(SUM(amount), 0) as total_spent_usdc
               FROM gateway_usage
               WHERE wallet_address = $1 AND {}"#,
            date_filter
        );

        let total: (i64, BigDecimal) = sqlx::query_as(&query_str)
            .bind(wallet_address)
            .fetch_one(&self.pool)
            .await?;

        let by_api_query = format!(
            r#"SELECT
                 a.slug as api_id,
                 COUNT(*) as requests,
                 COALESCE(SUM(g.amount), 0) as spent_usdc
               FROM gateway_usage g
               JOIN apis a ON a.id = g.api_id
               WHERE g.wallet_address = $1 AND {}
               GROUP BY a.slug
               ORDER BY requests DESC"#,
            date_filter
        );

        let by_api: Vec<ApiUsage> = sqlx::query_as(&by_api_query)
            .bind(wallet_address)
            .fetch_all(&self.pool)
            .await?;

        Ok(WalletUsage {
            total_requests: total.0,
            total_spent_usdc: total.1,
            by_api,
        })
    }
}

#[cfg(test)]
mod catalog_sql_tests {
    use super::*;

    /// The count query and the page query share `CATALOG_USER_FILTERS`, and a
    /// placeholder that means one thing in one and another thing in the other
    /// is the failure `meta.total` cannot survive: a crawler paginating off a
    /// total that counted a different set walks off the end or stops early,
    /// silently. The two are bound from the same list in `list_active_apis`,
    /// so the numbering has to match up to the filters and diverge only where
    /// the page adds LIMIT/OFFSET.
    #[test]
    fn both_catalog_queries_bind_the_same_filters() {
        let count = catalog_count_query();
        let page = catalog_page_query("name ASC");

        for n in 1..=9 {
            let placeholder = format!("${n}");
            assert!(
                count.contains(&placeholder),
                "count query is missing {placeholder}"
            );
            assert!(
                page.contains(&placeholder),
                "page query is missing {placeholder}"
            );
        }

        // Paging is the page query's alone. $10/$11 in the count query would
        // mean the filters had shifted under one of the two.
        assert!(page.contains("LIMIT $10 OFFSET $11"));
        assert!(!count.contains("$10"), "count query must not bind paging");
        assert!(!count.contains("$11"), "count query must not bind paging");
    }

    /// A slug lookup is an EQUALITY. As an ILIKE it would be `search` again —
    /// and `search` is exactly what sends a caller asking for one known listing
    /// a page of listings that merely mention it.
    #[test]
    fn the_slug_filter_is_exact() {
        assert!(CATALOG_USER_FILTERS.contains("OR a.slug = $9"));
    }

    /// The filter narrows; it never widens. A slug that names a listing which
    /// is pending, unverified or not discoverable must still return nothing,
    /// so the visibility rule stands beside it rather than behind an OR.
    #[test]
    fn a_slug_lookup_cannot_reach_an_invisible_listing() {
        let count = catalog_count_query();
        assert!(count.contains(CATALOG_VISIBILITY));
        assert!(count.contains(&format!("{CATALOG_VISIBILITY} AND")));
    }
}
