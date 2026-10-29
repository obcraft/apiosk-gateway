//! Loading executable contracts, and writing down the plan that was compiled
//! from them (migrations 112 and 113).
//!
//! The compiler in `crate::plan` has no database and no HTTP client. This
//! module is the only thing that gives it input, and it hands over the whole
//! candidate set at once: contract, endpoint, price and status together. That
//! is not an optimisation. A compiler that could ask the database a follow-up
//! question mid-search could also ask a provider one, and "the planner spends
//! nothing" would stop being a property of the code and start being a habit.

use super::discovery::CATALOG_VISIBILITY;
use super::*;

use crate::facts::contract::ExecutionContract;
use crate::fees::ProviderClass;
use crate::plan::{Candidate, Plan};

/// One row of the join the compiler plans over.
#[derive(Debug, Clone, FromRow)]
struct ContractRow {
    contract_id: Uuid,
    endpoint_id: Uuid,
    api_slug: String,
    endpoint_path: String,
    endpoint_method: String,
    listing_type: Option<String>,
    active: bool,
    price: Option<BigDecimal>,
    jurisdiction: Option<String>,
    capability_slug: String,
    version: i32,
    cardinality: String,
    requires: serde_json::Value,
    input_map: serde_json::Value,
    output_map: serde_json::Value,
    identifies: Option<String>,
    max_fan_out: i32,
}

impl Database {
    /// Every active execution contract, as candidates the compiler can rank.
    ///
    /// `active` and `price` come from `api_endpoints` rather than from the
    /// contract row, deliberately. A contract records what an endpoint CAN do;
    /// whether it may be called today and what it costs are properties of the
    /// endpoint, and reading them from the contract would let a stale copy
    /// authorise a call. Live today that difference is not hypothetical: two
    /// `capability_offers` and one active `param_resolvers` row still point at
    /// a KVK endpoint whose `active` is false.
    pub async fn plan_candidates(&self) -> Result<Vec<Candidate>> {
        let rows = sqlx::query_as::<_, ContractRow>(&format!(
            r#"
            select c.id                as contract_id,
                   c.endpoint_id       as endpoint_id,
                   a.slug              as api_slug,
                   e.path              as endpoint_path,
                   e.method            as endpoint_method,
                   -- `listing_type` is a Postgres ENUM, and sqlx will not decode
                   -- an enum into a String on its own. Live this failed every
                   -- plan with `catalogue_unavailable`; the tests never saw it
                   -- because they build candidates from JSON, not from a row.
                   a.listing_type::text as listing_type,
                   e.active            as active,
                   e.price             as price,
                   ft.jurisdiction     as jurisdiction,
                   c.capability_slug   as capability_slug,
                   c.version           as version,
                   c.cardinality       as cardinality,
                   c.requires          as requires,
                   c.input_map         as input_map,
                   c.output_map        as output_map,
                   c.identifies        as identifies,
                   c.max_fan_out       as max_fan_out
              from capability_execution_contracts c
              join api_endpoints e on e.id = c.endpoint_id
              join apis a          on a.id = e.api_id
              -- The jurisdiction of the identity fact a contract produces, when
              -- it declares one. A contract with no declared identity is not
              -- jurisdiction-bound and matches any intent.
              left join fact_types ft on ft.slug = c.identifies
             where c.status = 'active'
               and {CATALOG_VISIBILITY}
            "#
        ))
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().filter_map(into_candidate).collect())
    }
}

/// Turn a row into a candidate, or drop it.
///
/// A contract whose stored JSON no longer deserialises is dropped rather than
/// failing the whole compile. The alternative — one malformed row taking down
/// planning for every buyer — is worse, and a dropped candidate surfaces the
/// same way any missing source does: the plan says the output is unreachable
/// and names it.
fn into_candidate(row: ContractRow) -> Option<Candidate> {
    let contract = ExecutionContract {
        capability_slug: row.capability_slug,
        version: row.version,
        cardinality: serde_json::from_value(serde_json::Value::String(row.cardinality)).ok()?,
        requires: serde_json::from_value(row.requires).ok()?,
        input_map: serde_json::from_value(row.input_map).ok()?,
        output_map: serde_json::from_value(row.output_map).ok()?,
        identifies: row.identifies,
        max_fan_out: u32::try_from(row.max_fan_out).unwrap_or(1),
    };
    Some(Candidate {
        endpoint_method: row.endpoint_method,
        contract_id: row.contract_id.to_string(),
        endpoint_id: row.endpoint_id.to_string(),
        api_slug: row.api_slug,
        endpoint_path: row.endpoint_path,
        contract,
        list_atomic: row
            .price
            .as_ref()
            .map(crate::fees::usdc_to_atomic)
            .unwrap_or(0),
        class: ProviderClass::from_listing_type(row.listing_type.as_deref()),
        active: row.active,
        jurisdiction: row.jurisdiction,
    })
}

/// A stored plan and the version most recently compiled for it.
#[derive(Debug, Clone)]
pub struct StoredPlan {
    pub plan_id: Uuid,
    pub version_id: Uuid,
    pub version: i32,
    /// Whose plan it is, read back from the row rather than echoed from the
    /// request. The token is minted for this owner, so a token can never be
    /// minted for a caller the plan does not belong to.
    pub user_id: Option<Uuid>,
    pub connection_id: Option<Uuid>,
    pub question: String,
    pub plan_hash: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub plan: Plan,
}

#[derive(Debug, Clone, FromRow)]
struct PlanVersionRow {
    plan_id: Uuid,
    version_id: Uuid,
    version: i32,
    user_id: Option<Uuid>,
    connection_id: Option<Uuid>,
    question: String,
    plan_hash: String,
    expires_at: chrono::DateTime<chrono::Utc>,
    dag: serde_json::Value,
}

impl Database {
    /// Write a goal and its first compiled version.
    ///
    /// One transaction: a plan row with no version is a goal nobody can be
    /// shown, and a version with no plan cannot exist at all.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_plan(
        &self,
        user_id: Option<Uuid>,
        connection_id: Option<Uuid>,
        question: &str,
        intent: &serde_json::Value,
        conversation_id: Option<Uuid>,
        plan: &Plan,
        ttl: chrono::Duration,
    ) -> Result<StoredPlan> {
        let mut tx = self.pool.begin().await?;

        let plan_id: Uuid = sqlx::query_scalar(
            r#"insert into research_plans
                 (user_id, connection_id, question, intent, conversation_id)
               values ($1, $2, $3, $4, $5)
               returning id"#,
        )
        .bind(user_id)
        .bind(connection_id)
        .bind(question)
        .bind(intent)
        .bind(conversation_id)
        .fetch_one(&mut *tx)
        .await?;

        let hash = plan.hash();
        let expires_at = chrono::Utc::now() + ttl;
        let dag = serde_json::to_value(plan)?;
        let coverage = serde_json::to_value(&plan.coverage)?;
        let limits = serde_json::to_value(plan.limits)?;

        let version_id: Uuid = sqlx::query_scalar(
            r#"insert into research_plan_versions
                 (plan_id, version, dag, coverage, buyer_total_atomic,
                  eur_cents, fx_rate_micro, fx_taken_at,
                  output_format, limits, plan_hash, expires_at)
               values ($1, 1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
               returning id"#,
        )
        .bind(plan_id)
        .bind(&dag)
        .bind(&coverage)
        .bind(atomic_i64(plan.quote.buyer_total_atomic)?)
        .bind(plan.quote.eur.as_ref().map(|e| e.cents))
        .bind(plan.quote.eur.as_ref().map(|e| e.rate_micro))
        .bind(plan.quote.eur.as_ref().map(|e| e.taken_at))
        .bind(match plan.format {
            crate::plan::OutputFormat::Json => "json",
            crate::plan::OutputFormat::Pdf => "pdf",
        })
        .bind(&limits)
        .bind(&hash)
        .bind(expires_at)
        .fetch_one(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok(StoredPlan {
            plan_id,
            version_id,
            version: 1,
            user_id,
            connection_id,
            question: question.to_string(),
            plan_hash: hash,
            expires_at,
            plan: plan.clone(),
        })
    }

    /// Read back the latest version of a plan, for its owner only.
    ///
    /// Ownership is part of the WHERE clause rather than a check on the
    /// result. A query that can return another buyer's plan before deciding
    /// not to show it is one refactor away from showing it.
    pub async fn plan_for_owner(
        &self,
        plan_id: Uuid,
        user_id: Option<Uuid>,
        connection_id: Option<Uuid>,
    ) -> Result<Option<StoredPlan>> {
        let row = sqlx::query_as::<_, PlanVersionRow>(
            r#"select p.id as plan_id, v.id as version_id, v.version,
                      p.user_id, p.connection_id,
                      p.question, v.plan_hash, v.expires_at, v.dag
                 from research_plan_versions v
                 join research_plans p on p.id = v.plan_id
                where v.plan_id = $1
                  and ( ($2::uuid is not null and p.user_id = $2)
                     or ($3::uuid is not null and p.connection_id = $3) )
                order by v.version desc
                limit 1"#,
        )
        .bind(plan_id)
        .bind(user_id)
        .bind(connection_id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else { return Ok(None) };
        let plan: Plan = serde_json::from_value(row.dag)?;
        Ok(Some(StoredPlan {
            plan_id: row.plan_id,
            version_id: row.version_id,
            version: row.version,
            user_id: row.user_id,
            connection_id: row.connection_id,
            question: row.question,
            plan_hash: row.plan_hash,
            expires_at: row.expires_at,
            plan,
        }))
    }
}

/// Narrow an atomic-USDC amount to the `bigint` the money columns are.
///
/// Refuses rather than clamps. `unwrap_or(i64::MAX)` on a ceiling would turn an
/// impossible number into an almost unlimited authorisation — the one direction
/// a failure here must never round. In practice `i64` holds nine trillion
/// dollars in atomic USDC, so reaching this is a bug somewhere upstream, and a
/// bug in a price is exactly what should stop rather than degrade.
pub(crate) fn atomic_i64(value: i128) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| anyhow::anyhow!("amount {value} does not fit the ledger's bigint"))
}
