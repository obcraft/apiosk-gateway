//! Declarative parameter resolvers (`param_resolvers`, migration 094).
//!
//! One row says: this required parameter of that target (an endpoint, or every
//! offer of a capability) can be produced by calling another endpoint first
//! and reading the value out of its response. The engine that acts on these
//! rows lives in `crate::param_resolver`; this module only loads them.

use super::discovery::CATALOG_VISIBILITY;
use super::*;

/// One resolver declaration, ready for the engine.
#[derive(Debug, Clone, FromRow)]
pub struct ParamResolver {
    pub id: Uuid,
    /// The parameter this resolver can produce, in the TARGET endpoint's own
    /// vocabulary (e.g. `kvkNummer`).
    pub target_param: String,
    /// The endpoint that produces the value. Priced, paid and settled through
    /// the normal proxy path like any other call.
    pub resolver_endpoint_id: Uuid,
    /// The resolver endpoint's own input the searchable value rides in
    /// (e.g. `naam`).
    pub resolver_input_param: String,
    pub resolver_input_hint: Option<String>,
    /// Where the value lives. Relative to one hit object when
    /// `alternatives_path` is set (`kvkNummer`); absolute into the whole
    /// response body otherwise (`resultaten[0].kvkNummer`).
    pub extraction_path: String,
    /// Path to the hit LIST, for counting alternatives and disambiguating.
    pub alternatives_path: Option<String>,
    /// Disambiguation field map — see `crate::param_resolver::choose_hit`.
    pub match_fields: Option<serde_json::Value>,
    /// The identifier's shape (`digits:8`). Read on both sides of the same
    /// question: the read-only surfaces predict whether the lookup will be
    /// needed (`crate::param_resolver::shape_is_present`), and /v1/do decides
    /// whether it runs (`resolver_applies`) — a bound value this shape rejects
    /// is a search term, not an answer.
    pub target_shape: Option<String>,
    /// Human label for the pre-step line ("KVK company lookup").
    pub label: String,
    /// Whether this row targeted the endpoint specifically (true) or the
    /// capability (false). Endpoint-level wins on the same target_param.
    pub endpoint_level: bool,
}

impl Database {
    /// Active resolver declarations for a target endpoint and/or capability.
    ///
    /// One query; when both an endpoint-level and a capability-level row
    /// declare the same `target_param`, the endpoint-level row wins
    /// (`DISTINCT ON` ordered with endpoint-level first).
    pub async fn resolvers_for(
        &self,
        endpoint_id: Option<Uuid>,
        capability: Option<&str>,
    ) -> Result<Vec<ParamResolver>> {
        if endpoint_id.is_none() && capability.is_none() {
            return Ok(Vec::new());
        }
        Ok(sqlx::query_as::<_, ParamResolver>(
            r#"SELECT DISTINCT ON (target_param)
                 id, target_param, resolver_endpoint_id, resolver_input_param,
                 resolver_input_hint, extraction_path, alternatives_path,
                 match_fields, target_shape, label,
                 (target_endpoint_id IS NOT NULL) AS endpoint_level
               FROM param_resolvers
               WHERE status = 'active'
                 AND ((target_endpoint_id IS NOT NULL AND target_endpoint_id = $1)
                   OR (target_capability IS NOT NULL AND target_capability = $2))
               ORDER BY target_param, (target_endpoint_id IS NOT NULL) DESC"#,
        )
        .bind(endpoint_id)
        .bind(capability)
        .fetch_all(&self.pool)
        .await?)
    }
}

/// A resolver declaration joined with the priced offer that would run it —
/// everything a read-only surface needs to draw the pre-step line without a
/// second query per candidate.
#[derive(Debug, Clone, FromRow)]
pub struct PreStepRow {
    pub target_endpoint_id: Option<Uuid>,
    pub target_capability: Option<String>,
    pub target_param: String,
    pub target_shape: Option<String>,
    pub label: String,
    pub resolver_api_slug: String,
    pub resolver_method: String,
    pub resolver_path: String,
    pub resolver_price_usd: f64,
    pub resolver_hosted_externally: bool,
}

impl Database {
    /// Resolver declarations for a whole page of candidates at once.
    ///
    /// One query for every endpoint and capability on the page, so
    /// `/v1/discover` can annotate ten candidates without ten round trips.
    /// The resolver endpoint is joined through the same visibility predicate
    /// the catalogue uses: a declaration pointing at an unpublishable
    /// endpoint draws no line, because it could not run either.
    pub async fn pre_step_rows(
        &self,
        endpoint_ids: &[Uuid],
        capabilities: &[String],
    ) -> Result<Vec<PreStepRow>> {
        if endpoint_ids.is_empty() && capabilities.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            r#"SELECT r.target_endpoint_id,
                      r.target_capability,
                      r.target_param,
                      r.target_shape,
                      r.label,
                      a.slug as resolver_api_slug,
                      e.method as resolver_method,
                      e.path   as resolver_path,
                      e.price::double precision as resolver_price_usd,
                      (COALESCE(a.listing_type::text, 'api') = 'federated') as resolver_hosted_externally
               FROM param_resolvers r
               JOIN api_endpoints e ON e.id = r.resolver_endpoint_id
               JOIN apis a ON a.id = e.api_id
               WHERE r.status = 'active'
                 AND COALESCE(e.active, TRUE)
                 AND {CATALOG_VISIBILITY}
                 AND (r.target_endpoint_id = ANY($1) OR r.target_capability = ANY($2))"#
        );
        Ok(sqlx::query_as::<_, PreStepRow>(&sql)
            .bind(endpoint_ids)
            .bind(capabilities)
            .fetch_all(&self.pool)
            .await?)
    }
}
