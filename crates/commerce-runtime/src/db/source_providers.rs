//! `source_providers` — one durable row per seller per external source.
//!
//! The derived half of a row is written here, wholesale, from the crawl's own
//! grouping ([`crate::source_index::Catalogue::provider_snapshot`]); the
//! curated half is written only by the admin portal's SECURITY DEFINER
//! functions and is deliberately untouchable from this module's upsert — the
//! column lists do not overlap, so a snapshot can never clobber a curator.
//!
//! Reads are one query per sweep, not per offer: the sweep loads the rows
//! that actually say something (non-neutral status, or curated vocabulary)
//! and matches them in memory. See `crate::sources::provider_curation`.

use super::*;

/// What the admin said about one seller, in the shape the sweep consumes.
/// `hidden` drops the seller's offers; `preferred` breaks ranking ties —
/// never more, because curation that outranks relevance is a shelf fee.
#[derive(Debug, Clone, Default)]
pub struct ProviderCuration {
    pub curated_tags: Vec<String>,
    pub curated_categories: Vec<String>,
    pub hidden: bool,
    pub preferred: bool,
}

/// Curation keyed the way the sweep looks it up: source id, then the
/// registrable domain — the same key the ranking's provider spread uses.
pub type CurationMap = HashMap<String, HashMap<String, ProviderCuration>>;

#[derive(Debug, sqlx::FromRow)]
struct CurationRow {
    source_id: String,
    provider_key: String,
    curated_tags: Vec<String>,
    curated_categories: Vec<String>,
    status: String,
}

/// Rows per statement. A big source has a few thousand sellers; chunking
/// keeps each statement well under the 5s statement timeout and the payload
/// a size the wire does not notice.
const UPSERT_CHUNK: usize = 500;

impl Database {
    /// Writes one source's seller snapshot, overwriting the derived columns
    /// and leaving the curated ones alone. Rows are never deleted: a seller
    /// that vanished from its index keeps its row with a stale
    /// `last_seen_at`, so curation survives an index outage.
    pub async fn upsert_source_providers(
        &self,
        source_id: &str,
        rows: &[crate::source_index::ProviderSnapshot],
    ) -> Result<u64> {
        let mut written = 0u64;
        for chunk in rows.chunks(UPSERT_CHUNK) {
            let payload = serde_json::to_value(chunk)?;
            let result = sqlx::query(
                r#"INSERT INTO source_providers (
                     source_id, provider_key, sample_host, name, logo_url,
                     endpoint_count, unpriced_endpoints, price_usd_min,
                     price_usd_max, networks, methods, categories, tags,
                     top_terms, sample_description, with_input_schema,
                     last_seen_at, updated_at
                   )
                   SELECT $1, r.key, r.sample_host, r.name, r.logo_url,
                          r.endpoint_count, r.unpriced_endpoints,
                          r.price_usd_min, r.price_usd_max, r.networks,
                          r.methods, r.categories, r.tags, r.top_terms,
                          r.sample_description, r.with_input_schema,
                          now(), now()
                   FROM jsonb_to_recordset($2::jsonb) AS r(
                     key text, sample_host text, name text, logo_url text,
                     endpoint_count integer, unpriced_endpoints integer,
                     price_usd_min double precision,
                     price_usd_max double precision,
                     networks text[], methods text[], categories text[],
                     tags text[], top_terms text[],
                     sample_description text, with_input_schema integer
                   )
                   ON CONFLICT (source_id, provider_key) DO UPDATE SET
                     sample_host = EXCLUDED.sample_host,
                     name = EXCLUDED.name,
                     logo_url = EXCLUDED.logo_url,
                     endpoint_count = EXCLUDED.endpoint_count,
                     unpriced_endpoints = EXCLUDED.unpriced_endpoints,
                     price_usd_min = EXCLUDED.price_usd_min,
                     price_usd_max = EXCLUDED.price_usd_max,
                     networks = EXCLUDED.networks,
                     methods = EXCLUDED.methods,
                     categories = EXCLUDED.categories,
                     tags = EXCLUDED.tags,
                     top_terms = EXCLUDED.top_terms,
                     sample_description = EXCLUDED.sample_description,
                     with_input_schema = EXCLUDED.with_input_schema,
                     last_seen_at = now(),
                     updated_at = now()"#,
            )
            .bind(source_id)
            .bind(payload)
            .execute(&self.pool)
            .await?;
            written += result.rows_affected();
        }
        Ok(written)
    }

    /// Learns from a queried source's PARTIAL answer, without ever shrinking
    /// what is already known.
    ///
    /// A queried index (the Bazaar, thirdweb) has no whole catalogue to
    /// snapshot, so its seller rows grow from the answers that flow through
    /// discovery instead — and a partial answer must never be written with
    /// `upsert_source_providers`' overwrite semantics, or one narrow question
    /// would shrink a seller's row to the three endpoints that answered it.
    /// Everything here widens or holds: counts are floors (GREATEST), the
    /// price range only extends (LEAST/GREATEST), label arrays union under
    /// the same caps the snapshot uses, and prose is kept unless the new copy
    /// says more.
    pub async fn accrete_source_providers(
        &self,
        source_id: &str,
        rows: &[crate::source_index::ProviderSnapshot],
    ) -> Result<u64> {
        let mut written = 0u64;
        for chunk in rows.chunks(UPSERT_CHUNK) {
            let payload = serde_json::to_value(chunk)?;
            let result = sqlx::query(
                r#"INSERT INTO source_providers (
                     source_id, provider_key, sample_host, name, logo_url,
                     endpoint_count, unpriced_endpoints, price_usd_min,
                     price_usd_max, networks, methods, categories, tags,
                     top_terms, sample_description, with_input_schema,
                     last_seen_at, updated_at
                   )
                   SELECT $1, r.key, r.sample_host, r.name, r.logo_url,
                          r.endpoint_count, r.unpriced_endpoints,
                          r.price_usd_min, r.price_usd_max, r.networks,
                          r.methods, r.categories, r.tags, r.top_terms,
                          r.sample_description, r.with_input_schema,
                          now(), now()
                   FROM jsonb_to_recordset($2::jsonb) AS r(
                     key text, sample_host text, name text, logo_url text,
                     endpoint_count integer, unpriced_endpoints integer,
                     price_usd_min double precision,
                     price_usd_max double precision,
                     networks text[], methods text[], categories text[],
                     tags text[], top_terms text[],
                     sample_description text, with_input_schema integer
                   )
                   ON CONFLICT (source_id, provider_key) DO UPDATE SET
                     sample_host = COALESCE(source_providers.sample_host, EXCLUDED.sample_host),
                     name = COALESCE(source_providers.name, EXCLUDED.name),
                     logo_url = COALESCE(source_providers.logo_url, EXCLUDED.logo_url),
                     endpoint_count = GREATEST(source_providers.endpoint_count, EXCLUDED.endpoint_count),
                     unpriced_endpoints = GREATEST(source_providers.unpriced_endpoints, EXCLUDED.unpriced_endpoints),
                     price_usd_min = LEAST(
                       COALESCE(source_providers.price_usd_min, EXCLUDED.price_usd_min),
                       COALESCE(EXCLUDED.price_usd_min, source_providers.price_usd_min)
                     ),
                     price_usd_max = GREATEST(
                       COALESCE(source_providers.price_usd_max, EXCLUDED.price_usd_max),
                       COALESCE(EXCLUDED.price_usd_max, source_providers.price_usd_max)
                     ),
                     networks = (
                       SELECT COALESCE(array_agg(v ORDER BY v), '{}')
                       FROM (SELECT DISTINCT v
                             FROM unnest(source_providers.networks || EXCLUDED.networks) AS v
                             ORDER BY v LIMIT 8) u
                     ),
                     methods = (
                       SELECT COALESCE(array_agg(v ORDER BY v), '{}')
                       FROM (SELECT DISTINCT v
                             FROM unnest(source_providers.methods || EXCLUDED.methods) AS v
                             ORDER BY v LIMIT 8) u
                     ),
                     categories = (
                       SELECT COALESCE(array_agg(v ORDER BY v), '{}')
                       FROM (SELECT DISTINCT v
                             FROM unnest(source_providers.categories || EXCLUDED.categories) AS v
                             ORDER BY v LIMIT 8) u
                     ),
                     tags = (
                       SELECT COALESCE(array_agg(v ORDER BY v), '{}')
                       FROM (SELECT DISTINCT v
                             FROM unnest(source_providers.tags || EXCLUDED.tags) AS v
                             ORDER BY v LIMIT 32) u
                     ),
                     top_terms = CASE
                       WHEN cardinality(source_providers.top_terms) >= cardinality(EXCLUDED.top_terms)
                       THEN source_providers.top_terms
                       ELSE EXCLUDED.top_terms
                     END,
                     sample_description = CASE
                       WHEN length(COALESCE(EXCLUDED.sample_description, ''))
                          > length(COALESCE(source_providers.sample_description, ''))
                       THEN EXCLUDED.sample_description
                       ELSE source_providers.sample_description
                     END,
                     with_input_schema = GREATEST(source_providers.with_input_schema, EXCLUDED.with_input_schema),
                     last_seen_at = now(),
                     updated_at = now()"#,
            )
            .bind(source_id)
            .bind(payload)
            .execute(&self.pool)
            .await?;
            written += result.rows_affected();
        }
        Ok(written)
    }

    /// Every row an admin has actually said something about, as the map the
    /// sweep matches against. The neutral, uncurated bulk of the table never
    /// crosses the wire.
    pub async fn source_provider_curation(&self) -> Result<CurationMap> {
        let rows = sqlx::query_as::<_, CurationRow>(
            r#"SELECT source_id, provider_key, curated_tags, curated_categories, status
               FROM source_providers
               WHERE status <> 'neutral'
                  OR cardinality(curated_tags) > 0
                  OR cardinality(curated_categories) > 0"#,
        )
        .fetch_all(&self.pool)
        .await?;

        let mut map: CurationMap = HashMap::new();
        for row in rows {
            map.entry(row.source_id).or_default().insert(
                row.provider_key,
                ProviderCuration {
                    curated_tags: row.curated_tags,
                    curated_categories: row.curated_categories,
                    hidden: row.status == "hidden",
                    preferred: row.status == "preferred",
                },
            );
        }
        Ok(map)
    }
}
