//! Native commerce/provider API runtime for the standalone Apiosk v2 gateway.
//! No executable, listener, process supervisor or independently deployable service.

mod acceptance;
mod agent_metadata;
mod api_management;
mod approvals;
mod ask;
mod auth;
mod bazaar;
mod cache;
mod commerce;
mod config;
mod connect;
mod db;
mod decision_record;
mod discovery_traffic;
mod endpoint_docs;
mod enrich;
mod facilitator_caps;
mod facts;
mod federated_intake;
mod federated_traffic;
mod fees;
mod gateway_app;
mod gateway_routes;
mod handlers;
mod html;
mod intent;
mod job;
mod listing;
mod network_env;
mod param_resolver;
mod parse_budget;
mod pay;
mod payment;
mod plan;
mod policy;
mod portal_auth;
mod proxy;
mod quality;
mod query_plan;
mod rate_limit;
mod relevance;
mod request_binding;
mod request_context;
mod service_auth;
mod source_index;
mod sources;
mod structured;
mod telemetry;
mod traffic;
mod treasury_watch;
mod v1_routes;
mod webmcp_traffic;
mod well_known;

use std::sync::Arc;
use tracing::info;

use crate::auth::AuthReplayCache;
use crate::cache::EndpointCache;
use crate::config::Config;
use crate::db::Database;
use crate::rate_limit::RateLimiter;
use crate::telemetry::Telemetry;

mod background;
mod in_process;
mod in_flight;
mod observability;

pub use in_flight::InFlight;
pub use observability::{error_reporting_layer, init_error_reporting};

impl AppState {
    /// The treasury: the one wallet Apiosk's own surfaces spend from. It signs
    /// EIP-3009 authorisations for a provider's own 402 and holds USDC on
    /// Base; it holds no ETH and never calls a contract. A session running as
    /// this wallet is Apiosk paying, with the buyer's balance already debited
    /// in Supabase, so nothing about it is checked or settled on-chain here.
    pub fn treasury_address(&self) -> Option<String> {
        self.external_fetch_payer
            .as_ref()
            .map(|p| format!("{:#x}", p.address()))
            .or_else(|| {
                self.resell_payer
                    .as_ref()
                    .map(|p| format!("{:#x}", p.address()))
            })
    }

    pub fn is_treasury_wallet(&self, wallet: &str) -> bool {
        self.treasury_address()
            .is_some_and(|treasury| treasury.eq_ignore_ascii_case(wallet.trim()))
    }
}

pub(crate) struct AppState {
    router: std::sync::OnceLock<std::sync::Weak<axum::Router>>,
    in_flight: InFlight,
    pub config: Config,
    pub db: Database,
    pub cache: EndpointCache,
    pub auth_replay_cache: AuthReplayCache,
    pub http_client: reqwest::Client,
    pub facilitator_url: String,
    // `settlement_client: Option<SettlementClient>` used to sit here: the
    // ApioskSettlement contract client that moved USDC out of a BUYER's wallet
    // on Base, against an allowance they granted at connect. Apiosk keeps
    // exactly one payment rail now — the buyer's Apiosk balance is debited in
    // the App's Supabase and the treasury pays the provider's own x402 — so
    // nothing in this process calls a contract, and the only chain credential
    // it holds is the treasury signing key below.
    /// Platform payer for the resell leg — pays an upstream's own x402 on
    /// forward (if `RESELL_PAYER_PRIVATE_KEY` is set). `None` ⇒ resell disabled;
    /// resell-flagged listings then fail closed rather than forward unpaid.
    pub resell_payer: Option<crate::proxy::resell::ResellPayer>,
    /// Platform payer for the external x402 fetch proxy (`POST /v1/x402/fetch`) —
    /// pays an arbitrary external x402 endpoint from a connected buyer's managed
    /// wallet (if `EXTERNAL_FETCH_ENABLED=true`). `None` ⇒ the route 403s.
    pub external_fetch_payer: Option<crate::proxy::external_fetch::ExternalFetchPayer>,
    // There used to be a `monerium` handle here: the EUR off-ramp boundary that
    // linked a provider's wallet to an IBAN and redeemed EURe over SEPA. Apiosk
    // now keeps exactly one payment rail — the buyer's Apiosk balance is debited
    // in the App's Supabase and the treasury pays the provider over x402 — so
    // there is no fiat leg for the gateway to hold a credential for.
    /// Per-buyer / per-IP fixed-window rate limiter.
    pub rate_limiter: RateLimiter,
    /// One-time SIWE nonces for Connect merchant wallet verification (02-04/05).
    pub connect_nonce_cache: crate::connect::siwe::ConnectNonceCache,
    /// TTL-cached facilitator `/supported` capabilities — gates which non-Base
    /// x402 networks the gateway is willing to advertise in 402 responses.
    pub facilitator_caps: crate::facilitator_caps::FacilitatorCapsCache,
    /// Telemetry v2 handle (§Phase 2). A cheap no-op when
    /// `TELEMETRY_V2_ENABLED=false`; when enabled, `emit` is a non-blocking
    /// `try_send` onto a bounded queue drained by a background writer. A
    /// telemetry failure can never affect a response.
    pub telemetry: Telemetry,
    /// Crawler-ingestion counters for the `/.well-known/*` namespace (mig 068).
    /// The discovery documents are the most-read surface the gateway serves and
    /// are deliberately excluded from the request log; this is what makes them
    /// measurable. Buffered in-process and flushed on a timer — recording is
    /// non-blocking and fails open.
    pub discovery: crate::discovery_traffic::DiscoveryTracker,
    /// Every external index, crawled to the end and held searchable.
    ///
    /// The sweep in `crate::sources` reads a `Fetch::Indexed` source's FIRST
    /// PAGE — 100 entries of PayAI's 26,684, 1,000 of the Bazaar's 15,167 —
    /// because a page size was hand-written into each row's URL and no page
    /// two was ever asked for. This is where the rest of it lives: paged to
    /// the end on a timer by `crate::source_index`, filtered through an
    /// inverted index, and never on a request's critical path.
    ///
    /// Empty until the first crawl finishes, and empty is not an error — the
    /// sweep falls back to the first page exactly as before.
    pub source_index: crate::source_index::SourceIndex,
    /// WebMCP injection counters (mig 074). `/webmcp.js` is a static asset whose
    /// whole effect happens in the browser afterwards, so the request log cannot
    /// say how much agent surface providers actually inject; the shim reports
    /// each run to `POST /_apiosk/webmcp` and this buffers the counts. Same
    /// properties as `discovery`: non-blocking and fails open.
    pub webmcp: crate::webmcp_traffic::WebmcpTracker,
}

/// Owns the API router and all recurring commerce background loops.
/// Keep this value alive until the single v2 HTTP server has drained requests.
pub struct CommerceRuntime {
    router: Arc<axum::Router>,
    state: Arc<AppState>,
    _tasks: background::BackgroundTasks,
}

impl CommerceRuntime {
    pub async fn from_env() -> anyhow::Result<Self> {
        let tasks = background::BackgroundTasks::default();
        let state = initialize(&tasks, InFlight::default()).await?;
        let router = Arc::new(gateway_app::build_router(state.clone()));
        state.router.set(Arc::downgrade(&router)).expect("router initialized once");
        // The optional persisted-job loop is now owned by this runtime, not
        // a separate process role or listener. Its existing opt-in is preserved.
        if job::worker::enabled() {
            let worker_state = state.clone();
            tasks.spawn(async move {
                if let Err(error) = job::worker::run(worker_state).await {
                    tracing::error!("research worker stopped: {error}");
                }
            });
        }
        Ok(Self { router, state, _tasks: tasks })
    }

    pub fn router(&self) -> axum::Router {
        self.router.as_ref().clone()
    }

    pub async fn ready(&self) -> bool {
        !self.state.db.pool.is_closed()
    }

    pub fn in_flight(&self) -> InFlight {
        self.state.in_flight.clone()
    }

    /// The v2 listener must stop accepting requests before this is called.
    /// Stop recurring producers, then wait for admitted payment work without
    /// cancelling it just because its original HTTP caller has disappeared.
    pub async fn shutdown(&self, timeout: std::time::Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        if tokio::time::timeout_at(deadline, self._tasks.stop()).await.is_err() {
            return false;
        }
        self.state.in_flight.drain(deadline.saturating_duration_since(tokio::time::Instant::now())).await
    }
}

async fn initialize(tasks: &background::BackgroundTasks, in_flight: InFlight) -> anyhow::Result<Arc<AppState>> {
    let config = Config::from_env()?;

    // Which chain family this process may touch: mainnet in production, testnet
    // on a laptop or in CI. Decided from where the process runs, not from a flag
    // someone has to remember to set.
    //
    // Checked before the database connection on purpose: a chain misconfigured
    // to move real money from a laptop must fail on its own terms, not hide
    // behind whichever I/O happens to fail first.
    let network_env = crate::network_env::NetworkEnv::detect();
    info!(
        "Network environment: {} (on_fly={})",
        network_env.label(),
        crate::network_env::runs_on_fly()
    );

    // Which model owns discovery — said out loud, because the failure mode is
    // silence. A missing key does not error and does not warn: it returns
    // `source: "verbatim"` with every diagnostic field null, which is a
    // perfectly valid-looking response that quietly searches the user's
    // sentence as one literal keyword. Twice now that has been mistaken for a
    // broken parser rather than an absent one.
    let has_openrouter = config.openrouter_api_key.is_some();
    if has_openrouter {
        info!(
            "Engine: OPENROUTER model={} (sole vendor, sole model: parse, widening, \
             relevance, ask and request binding)",
            config.openrouter_model
        );
    } else {
        tracing::warn!(
            "Engine: OFF - OPENROUTER_API_KEY is not set. Every query is searched \
             VERBATIM as one literal keyword (source=\"verbatim\", parser_error=null). \
             There is no second vendor to fall back to."
        );
    }

    // One model, and the variables that used to be able to pick a different one
    // are now inert. Inert is worse than absent when the variable is still SET:
    // somebody wrote it expecting an effect, and silence looks like agreement.
    // Deployments are the place this rots — `OPENROUTER_MODEL` outlived the code
    // that read it by a whole refactor, and nothing said so.
    for retired in [
        "ANTHROPIC_API_KEY",
        "DISCOVER_PARSER_MODEL",
        "OPENROUTER_MODEL",
        "ASK_READ_MODEL",
        "ASK_RERANK_MODEL",
        "DISCOVER_READER",
        "GUMLOOP_API_KEY",
    ] {
        if std::env::var(retired).is_ok_and(|v| !v.trim().is_empty()) {
            tracing::warn!(
                "{retired} is set and is IGNORED. The engine model is APIOSK_ENGINE_MODEL \
                 (currently {}); nothing else selects a model or a vendor. Unset it.",
                config.openrouter_model
            );
        }
    }
    // `SettlementConfig::from_env()` was read here, and guarded against the
    // detected network so a stale `.env` could not put a laptop on Base mainnet
    // with direct settlement enabled. Both are gone with the contract client.

    let db = Database::connect(&config.database_url).await?;

    // Telemetry v2 (§Phase 2). Off by default; when enabled it uses a small,
    // SEPARATE connection pool so it can never starve the request/settlement
    // path. Any init failure leaves telemetry disabled — the gateway must start.
    let telemetry = if config.telemetry.enabled {
        match Database::connect_side_pool(&config.database_url, 2).await {
            Ok(pool) => Telemetry::start(pool, config.telemetry.clone(), tasks),
            Err(e) => {
                tracing::warn!("Telemetry pool init failed ({e}); telemetry v2 disabled");
                Telemetry::disabled()
            }
        }
    } else {
        Telemetry::disabled()
    };

    // Crawler-ingestion counters (mig 068). Shares the main pool: the writer is
    // one small upsert every 15s, never on a request path.
    let discovery = crate::discovery_traffic::DiscoveryTracker::start(db.pool.clone(), tasks);

    // WebMCP injection counters (mig 074). Same deal: one small upsert every
    // 15s, never on a request path.
    let webmcp = crate::webmcp_traffic::WebmcpTracker::start(db.pool.clone(), tasks);

    let cache = EndpointCache::new();
    let auth_replay_cache = AuthReplayCache::new();
    // S7: bound outbound proxy calls so a slow/hung/black-holed upstream cannot
    // pin a Tokio task + connection forever and exhaust the VM. connect_timeout
    // kills dead-host connects; the overall request timeout bounds the read (the
    // proxy buffers responses, so this never truncates a legitimate stream). The
    // overall timeout is env-tunable for genuinely slow compute upstreams.
    let upstream_timeout_secs = std::env::var("GATEWAY_UPSTREAM_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(60);
    let http_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(upstream_timeout_secs))
        .build()?;
    info!(
        "Upstream HTTP client: connect_timeout=10s request_timeout={}s",
        upstream_timeout_secs
    );

    let facilitator_url = config.facilitator_url.clone();
    info!("Using facilitator: {}", facilitator_url);

    // Two startup steps used to run here and are gone with the buyer's on-chain
    // rail: constructing the `SettlementClient`, and `assert_hybrid_contract`,
    // which asked the chain to quote `settleExact` and refused to serve if the
    // deployed contract disagreed with `fees.rs` (audit K1/K2 — a
    // `setPlatformFee()` to 1000 bps once went unnoticed for ~345k blocks while
    // the ledger kept booking 200 bps). The split it was guarding is still
    // computed in `fees.rs`, but no contract derives anything from it any more:
    // the treasury pays the provider's asking price over x402 and the Apiosk
    // take never leaves Apiosk.

    // Resell payer — funds paying upstream x402s so the gateway can resell an
    // already-x402-native API. Off unless RESELL_PAYER_PRIVATE_KEY is set.
    let resell_payer = crate::proxy::resell::ResellPayer::from_env();
    match resell_payer.as_ref() {
        Some(p) => info!(
            "Resell leg enabled: treasury payer={:#x} cap_per_tx_atomic={:?} cap_per_day_atomic={:?} — THIS address must hold USDC on Base",
            p.address(),
            p.cap_per_tx_atomic,
            p.cap_per_day_atomic
        ),
        None => info!(
            "Resell leg disabled - set {} to enable",
            crate::proxy::resell::TREASURY_KEY_VAR
        ),
    }

    // External x402 fetch payer — pays arbitrary external x402 endpoints from a
    // connected buyer's managed wallet (POST /v1/x402/fetch). Off unless
    // EXTERNAL_FETCH_ENABLED=true; the route 403s (feature_disabled) when None.
    let external_fetch_payer = crate::proxy::external_fetch::ExternalFetchPayer::from_env();
    match external_fetch_payer.as_ref() {
        Some(p) => info!(
            "External x402 fetch enabled: treasury payer={:#x} max_price_atomic={} fee_bps={} default_deny={} — same wallet as the resell leg",
            p.address(),
            p.max_price_atomic,
            p.fee_bps,
            p.default_deny
        ),
        None => info!(
            "External x402 fetch disabled - set EXTERNAL_FETCH_ENABLED=true and {} to enable",
            crate::proxy::resell::TREASURY_KEY_VAR
        ),
    }

    let state = Arc::new(AppState {
        router: std::sync::OnceLock::new(),
        in_flight,
        config: config.clone(),
        db,
        cache,
        auth_replay_cache,
        http_client,
        facilitator_url,
        resell_payer,
        external_fetch_payer,
        rate_limiter: RateLimiter::default(),
        connect_nonce_cache: crate::connect::siwe::ConnectNonceCache::new(),
        facilitator_caps: crate::facilitator_caps::FacilitatorCapsCache::default(),
        telemetry,
        discovery,
        source_index: crate::source_index::SourceIndex::new(),
        webmcp,
    });

    // Page every external index to the end, now and every interval after.
    // Until the first pass lands the sweep behaves exactly as it did before,
    // so this can never make a cold start worse — only a warm one deeper. See
    // `source_index` for why three percent of the ecosystem was being searched
    // and reported as a sweep of the ecosystem.
    crate::source_index::spawn(Arc::clone(&state), tasks);

    // Watch the wallet the payer legs sign from, for as long as this process
    // lives. Only when one of them is actually on: there is nothing to keep
    // funded otherwise, and a warning about an idle wallet is noise. See
    // `treasury_watch` for why an empty treasury is worth catching early —
    // it fails as an unexplained provider 402, not as an out-of-funds error.
    if let Some(address) = state
        .external_fetch_payer
        .as_ref()
        .map(|p| p.address())
        .or_else(|| state.resell_payer.as_ref().map(|p| p.address()))
    {
        treasury_watch::spawn(state.http_client.clone(), format!("{address:#x}"), tasks);
    }

    Ok(state)
}
