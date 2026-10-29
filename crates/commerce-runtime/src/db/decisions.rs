//! Writes for the decision record — what was on the table, what was chosen,
//! and whether it worked. See migration 076 for why these are three tables.

use super::*;

/// The candidate set an agent was served, as served.
#[derive(Debug, Clone, Default)]
pub struct ComparisonInsert {
    pub capability_slug: String,
    pub max_price_usd: Option<f64>,
    pub sort: String,
    pub candidates: serde_json::Value,
    pub candidate_count: i32,
    pub min_price_usd: Option<f64>,
    pub max_seen_usd: Option<f64>,
    pub agent_name: Option<String>,
    pub client_kind: Option<String>,
    pub user_agent: Option<String>,
    pub wallet_address: Option<String>,
    pub request_id: Option<Uuid>,
}

/// What the agent picked, and what it passed over.
#[derive(Debug, Clone, Default)]
pub struct DecisionInsert {
    pub comparison_id: Option<Uuid>,
    pub capability_slug: String,
    pub chosen_api_slug: String,
    pub chosen_method: Option<String>,
    pub chosen_path: Option<String>,
    pub chosen_price_usd: Option<f64>,
    pub reason: Option<String>,
    pub policy: Option<String>,
    pub rejected: serde_json::Value,
    pub supersedes: Option<Uuid>,
    pub agent_name: Option<String>,
    pub wallet_address: Option<String>,
    pub request_id: Option<Uuid>,
}

/// Whether the chosen provider delivered.
#[derive(Debug, Clone, Default)]
pub struct OutcomeInsert {
    pub decision_id: Uuid,
    pub status: String,
    pub http_status: Option<i32>,
    pub latency_ms: Option<i64>,
    pub satisfied: Option<bool>,
    pub notes: Option<String>,
}

impl Database {
    /// Records a served candidate set under an id the caller already minted.
    ///
    /// The id comes from Rust rather than from `DEFAULT gen_random_uuid()` so
    /// the response can carry `comparison_id` immediately instead of waiting
    /// for this write — which runs on a detached task precisely so nobody waits
    /// for it. See `crate::decision_record`.
    ///
    /// `ON CONFLICT DO NOTHING` keeps a retry idempotent.
    pub async fn insert_comparison_with_id(&self, id: Uuid, row: &ComparisonInsert) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO comparisons
                 (id, capability_slug, max_price_usd, sort, candidates, candidate_count,
                  min_price_usd, max_seen_usd, agent_name, client_kind, user_agent,
                  wallet_address, request_id)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
               ON CONFLICT (id) DO NOTHING"#,
        )
        .bind(id)
        .bind(&row.capability_slug)
        .bind(row.max_price_usd)
        .bind(&row.sort)
        .bind(&row.candidates)
        .bind(row.candidate_count)
        .bind(row.min_price_usd)
        .bind(row.max_seen_usd)
        .bind(&row.agent_name)
        .bind(&row.client_kind)
        .bind(&row.user_agent)
        .bind(&row.wallet_address)
        .bind(row.request_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Records a decision. Unlike the comparison this is caller-facing and
    /// synchronous: the agent asked to record it and gets the id back, so a
    /// failure is an error it should see rather than one to swallow.
    pub async fn insert_decision(&self, row: &DecisionInsert) -> Result<Uuid> {
        let id: Uuid = sqlx::query_scalar(
            r#"INSERT INTO decisions
                 (comparison_id, capability_slug, chosen_api_slug, chosen_method,
                  chosen_path, chosen_price_usd, reason, policy, rejected,
                  supersedes, agent_name, wallet_address, request_id)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
               RETURNING id"#,
        )
        .bind(row.comparison_id)
        .bind(&row.capability_slug)
        .bind(&row.chosen_api_slug)
        .bind(&row.chosen_method)
        .bind(&row.chosen_path)
        .bind(row.chosen_price_usd)
        .bind(&row.reason)
        .bind(&row.policy)
        .bind(&row.rejected)
        .bind(row.supersedes)
        .bind(&row.agent_name)
        .bind(&row.wallet_address)
        .bind(row.request_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(id)
    }

    /// Records an outcome against a decision. `attempt` is derived rather than
    /// caller-supplied so a retry cannot overwrite the first attempt's verdict
    /// by re-sending `attempt: 1`.
    pub async fn insert_outcome(&self, row: &OutcomeInsert) -> Result<(Uuid, i32)> {
        let rec: (Uuid, i32) = sqlx::query_as(
            r#"INSERT INTO decision_outcomes
                 (decision_id, status, http_status, latency_ms, satisfied, notes, attempt)
               VALUES ($1, $2, $3, $4, $5, $6,
                       (SELECT COALESCE(MAX(attempt), 0) + 1
                          FROM decision_outcomes WHERE decision_id = $1))
               RETURNING id, attempt"#,
        )
        .bind(row.decision_id)
        .bind(&row.status)
        .bind(row.http_status)
        .bind(row.latency_ms)
        .bind(row.satisfied)
        .bind(&row.notes)
        .fetch_one(&self.pool)
        .await?;
        Ok(rec)
    }

    /// True when the decision exists — so a bad `decision_id` is a 404 to the
    /// caller instead of a foreign-key 500.
    pub async fn decision_exists(&self, id: Uuid) -> Result<bool> {
        let found: Option<Uuid> = sqlx::query_scalar("SELECT id FROM decisions WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(found.is_some())
    }

    /// True when the comparison exists. Lets a stale or invented
    /// `comparison_id` be dropped to NULL rather than failing the write: the
    /// decision itself is the valuable half and must still land.
    pub async fn comparison_exists(&self, id: Uuid) -> Result<bool> {
        let found: Option<Uuid> = sqlx::query_scalar("SELECT id FROM comparisons WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(found.is_some())
    }
}
