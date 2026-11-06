mod capabilities;
mod capability_search;
mod commerce;
mod community;
pub mod connect;
pub mod connect_oauth;
mod decisions;
mod discovery;
mod endpoints;
mod external_listing;
mod idempotency;
mod intents;
pub mod jobs;
mod listings;
mod mcp;
mod models;
pub mod plans;
mod policies;
mod provider_stats;
pub(crate) mod request_log;
mod resell;
mod resolvers;
mod source_providers;
mod usage;

pub use capabilities::{CapabilityInfo, CapabilityOffer, CapabilitySort};
pub use capability_search::{MIN_RANK, RELATIVE_FLOOR};
pub use decisions::{ComparisonInsert, DecisionInsert, OutcomeInsert};
pub use discovery::{RANKING_NOTE, UNAFFILIATED_PROVIDER_NAME, UNAFFILIATED_PROVIDER_USERNAME};
pub use idempotency::{IdempotencyClaim, IdempotencyRecord, IN_FLIGHT_STATUS, RECORD_TTL_HOURS};
pub use mcp::PaymentSettlement;
pub use models::{PolicyDecisionInsert, PolicyRules, PurchaseApproval, PurchasePolicy};
pub use provider_stats::{ProviderStats, QUARANTINE_FLOOR};
pub use resolvers::{ParamResolver, PreStepRow};
pub use source_providers::{CurationMap, ProviderCuration};
pub use usage::SettlementStatus;

use anyhow::Result;
use bigdecimal::BigDecimal;
use chrono::Utc;
use sqlx::{
    postgres::{PgConnectOptions, PgPoolOptions},
    FromRow, PgPool,
};
use std::collections::HashMap;
use std::str::FromStr;
use uuid::Uuid;

use crate::listing::{
    ConnectorListing, DatasetListing, Listing, ProductListing, ProductPurchase, ServiceListing,
    SkillListing,
};
use crate::request_context::PresenceSessionContext;

pub use models::{
    AgentConnectToken, AgentConnectTokenLookup, AgentConnectTokenWallet, AgentIntentInsert, Api,
    ApiDetail, ApiInfo, ApiUsage, CreditChargeError, CreditChargeResult, Endpoint,
    EndpointDocsBundle, EndpointPricingRule, EndpointSchemas, EndpointWithDocs,
    GatewayRequestLogInsert, NetworkToken, PresenceSessionUpsert, ProviderListingCounts,
    ProviderSummary, WalletUsage,
};
pub(crate) use usage::endpoint_price_supports_credits;

#[derive(Clone)]
pub struct Database {
    pub(crate) pool: PgPool,
    /// Dedicated pool and bounded background work for best-effort request logs.
    pub(crate) log_pool: PgPool,
    pub(crate) request_log_queue: request_log::RequestLogQueue,
}

impl Database {
    /// The pool the research worker's own sweeps run on.
    ///
    /// The main pool, deliberately: the worker's statements are the same
    /// statements the request path uses and must contend with them on the same
    /// terms. The separate `log_pool` exists because the request log has a
    /// 250ms budget and was losing to catalogue reads; nothing here has a
    /// budget that tight.
    pub fn worker_pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn connect(database_url: &str) -> Result<Self> {
        let connect_options = PgConnectOptions::from_str(database_url)?
            .statement_cache_capacity(0)
            .application_name("apiosk-commerce");

        // Pool size is env-tunable so it can be raised WITHOUT a rebuild (set the
        // Fly secret GATEWAY_DB_POOL_SIZE and restart). Default raised 10 -> 20
        // after the 2026-07-11 pool-exhaustion incident (catalog reads timing
        // out on `pool timed out while waiting for an open connection`).
        let max_conns = std::env::var("GATEWAY_DB_POOL_SIZE")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(20);

        let pool = PgPoolOptions::new()
            .max_connections(max_conns)
            // Fail fast (~10s) instead of hanging on sqlx's 30s default when the
            // pool is momentarily saturated, so a slow spell can't pile up 30s
            // requests.
            .acquire_timeout(std::time::Duration::from_secs(10))
            .after_connect(|conn, _meta| {
                Box::pin(async move {
                sqlx::query("SET search_path TO public, gateway_v2, kyb, providers, catalog, actions, billing, agents, wallets, research, integrations, observability, support, content, operations, identity, extensions")
                    .execute(&mut *conn)
                    .await?;
                    sqlx::query("SET statement_timeout = '5000ms'")
                        .execute(&mut *conn)
                        .await?;
                    sqlx::query("SET lock_timeout = '3000ms'")
                        .execute(&mut *conn)
                        .await?;
                    Ok(())
                })
            })
            .connect_with(connect_options)
            .await?;

        // Small by design: the request log is one short INSERT per request, so
        // a handful of connections absorbs the concurrency. Env-tunable without
        // a rebuild, same as GATEWAY_DB_POOL_SIZE.
        let log_pool_size = std::env::var("GATEWAY_LOG_POOL_SIZE")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(4);

        let log_pool = match Self::connect_side_pool(database_url, log_pool_size).await {
            Ok(log_pool) => log_pool,
            Err(err) => {
                tracing::warn!(
                    "request-log pool init failed ({err}); falling back to the main pool"
                );
                pool.clone()
            }
        };

        Ok(Self {
            pool,
            log_pool,
            request_log_queue: request_log::RequestLogQueue::new(),
        })
    }

    /// Build a small, SEPARATE pool for a telemetry writer so telemetry inserts
    /// can never starve the request/settlement hot path of connections — nor be
    /// starved BY it, which is the failure the request log actually hit (see
    /// `Database::log_pool`). Used by the async telemetry writer and by the
    /// per-request gateway request log. Same statement/lock timeouts as the main
    /// pool. Returns a bare `PgPool` (the caller owns it); failure here leaves
    /// that writer degraded, not the gateway.
    pub async fn connect_side_pool(database_url: &str, max_connections: u32) -> Result<PgPool> {
        let connect_options = PgConnectOptions::from_str(database_url)?
            .statement_cache_capacity(0)
            .application_name("apiosk-commerce-telemetry");
        let pool = PgPoolOptions::new()
            .max_connections(max_connections.max(1))
            .acquire_timeout(std::time::Duration::from_secs(10))
            .after_connect(|conn, _meta| {
                Box::pin(async move {
                sqlx::query("SET search_path TO public, gateway_v2, kyb, providers, catalog, actions, billing, agents, wallets, research, integrations, observability, support, content, operations, identity, extensions")
                    .execute(&mut *conn)
                    .await?;
                    sqlx::query("SET statement_timeout = '5000ms'")
                        .execute(&mut *conn)
                        .await?;
                    sqlx::query("SET lock_timeout = '3000ms'")
                        .execute(&mut *conn)
                        .await?;
                    Ok(())
                })
            })
            .connect_with(connect_options)
            .await?;
        Ok(pool)
    }

}
