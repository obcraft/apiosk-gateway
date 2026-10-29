//! Free text to capabilities.
//!
//! `/v1/capabilities/{slug}` assumes the caller already knows the slug. An
//! agent does not. It has a need in words: "find the best financial investor
//! news". Between that sentence and `news.search` there was nothing, so the
//! only way in was to fetch all fifteen capabilities and choose, which is a
//! round trip and a judgement the catalogue should be making.
//!
//! Matching runs against what a capability *is* (name, description, category)
//! and against what its offers *say they do* (the endpoint text of the
//! listings assigned to it). The second half is what makes the example work:
//! "investor news" appears nowhere in the `news.search` description, but it is
//! all over cityfalcon's endpoint text, and cityfalcon is assigned to
//! `news.search`.
//!
//! Only capabilities the catalogue can actually SELL are returned — one with
//! no offer on a visible listing is a shelf label with nothing under it, and
//! every caller here (`/v1/discover`, `/v1/search`, `/v1/select`) can only
//! answer with offers. See the `stocked` join in [`search_sql`].
//!
//! Built on Postgres full-text search rather than trigram similarity, because
//! `pg_trgm` is not installed on this database and `similarity()` would fail at
//! runtime. `to_tsvector` is core Postgres, and it stems, so "investors" finds
//! "investor" without a wildcard.

use super::discovery::CATALOG_VISIBILITY;
use super::*;

/// A capability that matched a free-text query, and why.
#[derive(Debug, Clone, FromRow)]
pub struct CapabilityMatch {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub category: Option<String>,
    /// The capability's own input contract, carried through so a caller can be
    /// told which offers accept every input without a second lookup.
    pub input_contract: serde_json::Value,
    /// `ts_rank`. Relative and unitless: it orders results, it does not certify
    /// them.
    pub score: f64,
    /// The listing whose text carried the match, when the hit came from an
    /// offer rather than from the capability itself. Published so a caller can
    /// see why a capability surfaced for words absent from its own
    /// description.
    pub matched_via: Option<String>,
    /// How many offers this capability has that could actually serve the call:
    /// curated or declared, on an active endpoint of a visible listing. Never
    /// zero — a capability with no such offer is not returned at all.
    pub offer_count: i64,
}

/// Text search that finds nothing must return nothing.
///
/// A search that degrades to "here is the whole catalogue" teaches an agent
/// that the top result is always relevant, which is the habit that makes it
/// buy the wrong thing. With OR semantics a non-matching query scores exactly
/// zero, so this only has to be above zero to keep that property.
pub const MIN_RANK: f64 = 0.005;

/// Share of the best score a capability must reach to be returned at all.
///
/// Under OR semantics a capability matching one word of three still scores,
/// and "finance fiat market" otherwise returns five capabilities of which four
/// matched only "market". Relative rather than absolute because the scale of
/// ts_rank depends on the query: what counts as a weak match next to a strong
/// one is knowable, what counts as weak in the abstract is not.
pub const RELATIVE_FLOOR: f64 = 0.4;

/// The search SQL. Binds `$1` (query text) and `$2` (limit).
///
/// A named builder rather than an inline string so the tests can assert on the
/// SQL that actually runs. These are runtime-checked queries; without this the
/// only way to exercise them is a live pool.
fn search_sql() -> String {
    let inner = format!(
        r#"WITH q AS (
                 -- OR, not AND. plainto_tsquery ands every term, so "finance
                 -- fiat market" required all three and dropped fx.rate — the one
                 -- capability actually about fiat. Filtering on every word is
                 -- not what a search engine does; ranking on any word is, and
                 -- ts_rank below does the ordering.
                 --
                 -- Built by rewriting plainto_tsquery's OWN output rather than
                 -- assembling a tsquery from the raw input, so the text stays
                 -- sanitised and stemmed: the operator is all that changes.
                 SELECT replace(plainto_tsquery('english', $1::text)::text, '&', '|')::tsquery AS tsq
               ),
               cap AS (
                 SELECT c.slug, c.name, c.description, c.category, c.input_contract,
                        -- Normalisation flag 1 divides by the log of the
                        -- document length. Without it a capability's short
                        -- description competes against an offer's long one on
                        -- raw term frequency, and length wins: "finance fiat
                        -- market" landed on crypto.spot-price at 0.14 through
                        -- coingecko's wordy text while fx.rate, the one
                        -- capability about fiat, scored 0.04.
                        ts_rank(
                          to_tsvector('english',
                            c.name || ' ' || c.description || ' ' || COALESCE(c.category, '')),
                          q.tsq, 1
                        )::double precision AS self_rank
                 FROM capabilities c, q
                 WHERE c.status = 'active'
               ),
               -- What the offers assigned to a capability say they do. A
               -- capability inherits its suppliers' vocabulary, which is where
               -- the domain words live.
               via_offer AS (
                 SELECT co.capability_slug,
                        a.slug AS api_slug,
                        ts_rank(
                          to_tsvector('english',
                            COALESCE(e.description, '') || ' ' ||
                            COALESCE(a.description, '') || ' ' ||
                            COALESCE(a.name, '')       || ' ' ||
                            COALESCE(a.category, '')),
                          q.tsq, 1
                        )::double precision AS offer_rank
                 FROM capability_offers co
                 JOIN apis a          ON a.id = co.api_id
                 JOIN api_endpoints e ON e.id = co.endpoint_id
                 CROSS JOIN q
                 WHERE co.source IN ('curated', 'declared')
                   AND COALESCE(e.active, TRUE)
                   AND {CATALOG_VISIBILITY}
               ),
               -- What the catalogue can actually SELL under this capability:
               -- how many offers it has, and the one whose wording matched
               -- best. One aggregate rather than a DISTINCT ON plus a
               -- correlated COUNT, so both numbers describe the same set.
               stocked AS (
                 SELECT capability_slug,
                        COUNT(*)::BIGINT AS offer_count,
                        MAX(offer_rank)  AS offer_rank,
                        (ARRAY_AGG(api_slug ORDER BY offer_rank DESC))[1] AS api_slug
                 FROM via_offer
                 GROUP BY capability_slug
               )
               SELECT
                 cap.slug,
                 cap.name,
                 cap.description,
                 cap.category,
                 cap.input_contract,
                 GREATEST(cap.self_rank, st.offer_rank) AS score,
                 CASE WHEN st.offer_rank > cap.self_rank
                      THEN st.api_slug END AS matched_via,
                 st.offer_count
               FROM cap
               -- INNER, and this is the whole of the rule: a capability NOBODY
               -- STOCKS IS NOT AN ANSWER.
               --
               -- It used to be a LEFT JOIN, and an empty shelf then cost the
               -- caller the search twice over. `company.registry-profile` is
               -- written for exactly the question "how many people work at
               -- Mollie, and where?" — its description names employee counts
               -- and addresses — and it had no offers at all, because the
               -- migration that stocked it targeted a listing slug that never
               -- existed in production. So it won the search and returned
               -- nothing, twice:
               --
               --   * it took one of the four capability slots
               --     (`flow::MAX_CAPABILITIES`), which is a slot a stocked
               --     capability could have filled; and
               --   * being the top score, it set `best` for the relative floor
               --     below, so every stocked capability at less than 40% of an
               --     unsellable one was dropped as a weak match.
               --
               -- The result reads as "the catalogue has nothing for this",
               -- which is a different fact from the true one: nobody had put
               -- the shelf label on the goods. Filtering here rather than at
               -- the caller is what keeps the floor honest — it is computed
               -- over answerable capabilities only.
               JOIN stocked st ON st.capability_slug = cap.slug"#
    );

    format!(
        r#"WITH scored AS (
                 {inner}
               )
               SELECT slug, name, description, category, input_contract, score,
                      matched_via, offer_count
               FROM (
                 SELECT *, MAX(score) OVER () AS best FROM scored
               ) ranked
               WHERE score >= GREATEST({MIN_RANK}, best * {RELATIVE_FLOOR})
               ORDER BY score DESC, slug ASC
               LIMIT $2"#
    )
}

impl Database {
    /// Capabilities that could serve `query`, best first. Empty when nothing
    /// clears [`MIN_RANK`].
    pub async fn search_capabilities(
        &self,
        query: &str,
        limit: i64,
    ) -> Result<Vec<CapabilityMatch>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        Ok(sqlx::query_as::<_, CapabilityMatch>(&search_sql())
            .bind(query)
            .bind(limit.clamp(1, 25))
            .fetch_all(&self.pool)
            .await?)
    }

    /// The enabled external discovery sources, in the database spelling:
    /// `(id, fetch_kind, url, shape, note)` — `sources::SourceRow`. Ordered so
    /// the sweep and the response's stated reach are stable between calls.
    /// Errors bubble up: the caller (`sources::active_sources`) treats any
    /// failure — including the table not existing yet — as "use the built-in
    /// list", and caches a good read for thirty seconds, so an admin's
    /// enable/disable reaches the sweep within that window.
    pub async fn discovery_sources(&self) -> Result<Vec<crate::sources::SourceRow>> {
        match sqlx::query_as::<_, crate::sources::SourceRow>(DISCOVERY_SOURCES_SQL)
            .fetch_all(&self.pool)
            .await
        {
            Ok(rows) => Ok(rows),
            Err(err) => {
                // The identity columns arrive with migration 085. Until it is
                // applied the wide SELECT is a syntax error, and losing the
                // whole sweep over that would be a far worse failure than
                // sweeping without provider names. Retry on the original five
                // columns; a genuinely broken connection fails again below.
                tracing::debug!("discovery_sources: reading pre-085 columns ({})", err);
                let rows: Vec<(String, String, String, String, Option<String>)> =
                    sqlx::query_as(DISCOVERY_SOURCES_LEGACY_SQL)
                        .fetch_all(&self.pool)
                        .await?;
                Ok(rows
                    .into_iter()
                    .map(
                        |(id, fetch_kind, url, shape, note)| crate::sources::SourceRow {
                            id,
                            fetch_kind,
                            url,
                            shape,
                            note,
                            ..Default::default()
                        },
                    )
                    .collect())
            }
        }
    }

    /// Is this Supabase user an active platform admin?
    ///
    /// The admin portal's own writes are gated in Postgres by
    /// `is_active_platform_admin()`; the discovery probe runs here instead, so
    /// it asks the same table rather than trusting a claim in the caller's
    /// token. A missing row and an inactive row are the same answer.
    pub async fn is_active_platform_admin(&self, user_id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            r#"SELECT COALESCE((SELECT active FROM platform_admins WHERE user_id = $1), FALSE)"#,
        )
        .bind(user_id)
        .fetch_one(&self.pool)
        .await?)
    }

    /// Every category label the visible catalogue actually uses — capability
    /// categories plus the sector vocabulary on verified APIs. Fed to the
    /// enrichment model as vocabulary to pick from, so its categories are ones
    /// the catalogue can be searched by rather than invented synonyms.
    pub async fn capability_categories(&self) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar::<_, String>(
            r#"SELECT DISTINCT category FROM (
                     SELECT category FROM capabilities WHERE status = 'active'
                     UNION
                     -- Alleen categorieën die de catalogus ook echt kan
                     -- bedienen: deze lijst gaat als woordenschat naar het
                     -- model, en een categorie zonder listing stuurt het naar
                     -- een plan dat niemand kan uitvoeren.
                     SELECT category FROM apis
                      WHERE is_verified = TRUE AND COALESCE(discoverable, TRUE) = TRUE
                   ) labels
                   WHERE category IS NOT NULL AND category <> ''
                   ORDER BY category"#,
        )
        .fetch_all(&self.pool)
        .await?)
    }
}

/// The one read of `discovery_sources` the sweep makes. The Admin portal's
/// enable/disable switch is the `enabled` column; a source it turned off is
/// not swept, not listed by `/v1/sources`, and not browsable — all three read
/// this list.
const DISCOVERY_SOURCES_SQL: &str = r#"SELECT id, fetch_kind, url, shape, note,
                          method, request_body, name, logo_url, category, tags
                   FROM discovery_sources
                   WHERE enabled
                   ORDER BY id"#;

/// The same read against a database that has not had migration 085 applied.
const DISCOVERY_SOURCES_LEGACY_SQL: &str = r#"SELECT id, fetch_kind, url, shape, note
                   FROM discovery_sources
                   WHERE enabled
                   ORDER BY id"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_enabled_sources_reach_the_sweep() {
        // An admin's disable switch is a column, and this WHERE is the whole
        // of what honours it. Stable order, so the stated reach is stable too.
        for sql in [DISCOVERY_SOURCES_SQL, DISCOVERY_SOURCES_LEGACY_SQL] {
            assert!(sql.contains("FROM discovery_sources"));
            assert!(sql.contains("WHERE enabled"));
            assert!(sql.contains("ORDER BY id"));
        }
    }

    #[test]
    fn matching_reads_offer_text_as_well_as_capability_text() {
        // The reason "investor news" can reach news.search: those words live in
        // cityfalcon's endpoint description, not in the capability's own.
        let sql = search_sql();
        assert!(sql.contains("via_offer"));
        assert!(sql.contains("e.description"));
        assert!(sql.contains("matched_via"));
    }

    #[test]
    fn only_reviewed_assignments_can_carry_a_match() {
        let sql = search_sql();
        assert!(sql.contains("co.source IN ('curated', 'declared')"));
        assert!(!sql.contains("'inferred'"));
    }

    #[test]
    fn search_inherits_the_catalogs_visibility_rules() {
        // A capability must not surface through a listing the catalog hides.
        let sql = search_sql();
        assert!(sql.contains("a.is_verified = TRUE"));
    }

    #[test]
    fn no_trigram_dependency() {
        // pg_trgm is not installed on this database; similarity() would fail
        // at runtime rather than at build time.
        let sql = search_sql();
        assert!(!sql.contains("similarity("));
        assert!(sql.contains("to_tsvector"));
        assert!(sql.contains("plainto_tsquery"));
    }

    #[test]
    fn a_weak_match_is_dropped_rather_than_ranked_last() {
        let sql = search_sql();
        assert!(sql.contains("ORDER BY score DESC"));
        // Both floors reach the query: the absolute one rejects a query that
        // matched nothing, the relative one rejects the one-word-of-three hits
        // that OR semantics let in.
        assert!(sql.contains(&format!("GREATEST({MIN_RANK}, best * {RELATIVE_FLOOR})")));
    }

    #[test]
    fn an_unstocked_capability_is_not_an_answer() {
        // The KVK case: `company.registry-profile` describes exactly the job
        // ("employee counts ... addresses") and had no offers, so it won the
        // search, took a capability slot and raised the relative floor over
        // the capabilities that could have been sold. An INNER join to the
        // offers is what stops a shelf label with nothing under it from
        // answering.
        let sql = search_sql();
        assert!(sql.contains("JOIN stocked st ON st.capability_slug = cap.slug"));
        assert!(!sql.contains("LEFT JOIN best_offer"));
    }

    #[test]
    fn the_relative_floor_is_measured_against_sellable_capabilities() {
        // `best` has to be the best score among the rows that survive the
        // stocked join, or an unsellable capability sets the bar for everyone
        // else. Both live in the same statement, and the join is inside the
        // subquery the window function reads.
        let sql = search_sql();
        let stocked = sql.find("JOIN stocked st").expect("stocked join");
        let window = sql
            .find("MAX(score) OVER ()")
            .expect("relative floor window");
        assert!(stocked < window);
    }

    #[test]
    fn offer_count_counts_the_offers_the_search_would_serve() {
        // Published beside the match, so it has to describe the same set the
        // ranking used: curated or declared, active endpoint, visible listing.
        // A count straight off `capability_offers` includes offers on hidden
        // listings and reads as stock that cannot be bought.
        let sql = search_sql();
        assert!(sql.contains("st.offer_count"));
        assert!(!sql.contains("FROM capability_offers co2"));
    }

    #[test]
    fn terms_are_ored_so_a_partial_match_still_ranks() {
        // plainto_tsquery ands every term. "finance fiat market" then required
        // all three and dropped fx.rate, the one capability about fiat, while
        // crypto surfaced through an offer's wording.
        let sql = search_sql();
        assert!(sql.contains("replace(plainto_tsquery('english', $1::text)::text, '&', '|')"));
    }
}
