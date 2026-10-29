use anyhow::{bail, Context, Result};

use crate::payment::{is_cdp_facilitator_url, is_testnet_only_x402_facilitator};

/// Canonical CDP mainnet facilitator. The only facilitator that settles real
/// USDC on Base mainnet AND feeds the CDP Bazaar discovery index. Used as the
/// default and as the target the startup guard requires in production.
pub const CDP_MAINNET_FACILITATOR_URL: &str = "https://api.cdp.coinbase.com/platform/v2/x402";

#[derive(Clone)]
pub struct Config {
    /// Key the gateway signs quoted offers with (`/v1/quote`).
    ///
    /// Stable across restarts on purpose: an ephemeral per-process key would be
    /// just as unforgeable and would invalidate every outstanding quote on
    /// deploy, turning a routine restart into "everyone mid-choice starts over".
    pub quote_signing_secret: String,
    pub database_url: String,
    pub dashboard_url: String,
    pub buyer_portal_url: String,
    pub mcp_proxy_upstream_urls: Vec<String>,
    /// Generic, reusable direct-purchase payment providers (see `crate::pay`).
    /// Each entry is a skill-agnostic cashier for one provider's `/pay/charge`
    /// and `/pay/verify` flow. Loaded from the `PAYMENT_PROVIDERS` env var (a
    /// JSON array) or `PAYMENT_PROVIDERS_FILE`. Empty by default, in which case
    /// both `/pay/*` endpoints return 404 and the gateway behaves exactly as
    /// before.
    pub payment_providers: Vec<crate::pay::PaymentProvider>,
    pub supabase_url: Option<String>,
    pub supabase_api_key: Option<String>,
    /// Service-role key, used to call edge functions on behalf of a connect-token
    /// holder when no buyer JWT is available (SEPA agent flow).
    pub supabase_service_role_key: Option<String>,
    /// Platform wallet that receives all x402 payments.
    /// Commission is deducted and the remainder is credited to the developer's balance.
    pub platform_wallet: String,
    /// Legacy flat commission rate, for reporting and the retired flat-rate
    /// settle path only. The live split comes from [`crate::fees`]; see the
    /// comment where this is parsed.
    pub default_commission_rate: f64,
    /// x402 facilitator URL for payment verification and settlement.
    /// Override in production if you run your own facilitator.
    pub facilitator_url: String,
    /// Optional CDP API key ID used to authenticate facilitator calls on CDP.
    pub cdp_api_key_id: Option<String>,
    /// Optional CDP API key secret used to authenticate facilitator calls on CDP.
    pub cdp_api_key_secret: Option<String>,
    /// Optional GA4 Measurement ID used for server-side request events.
    pub ga_measurement_id: Option<String>,
    /// Optional GA4 Measurement Protocol API secret.
    pub ga_api_secret: Option<String>,
    /// Require a valid Agent Wallet connect token for paid requests from managed agent wallets.
    /// Legacy non-managed wallets continue to work when this is enabled.
    pub require_agent_connect_token: bool,
    // `sepa_debit_via_ledger` (SEPA_DEBIT_VIA_LEDGER) was read here: it chose
    // between batching euro per-call debits into `sepa_settlement_ledger` and
    // creating a Mollie payment per call. Neither happens any more — Apiosk
    // keeps one payment rail, the buyer's balance in the App's Supabase paying
    // the treasury's x402 call to the provider — so the variable is ignored.
    /// Secret for HMAC-signing Apiosk Connect tokens (merchant onboarding links,
    /// merchant sessions, outbound merchant webhooks). When unset, the Connect
    /// merchant-onboarding endpoints are inert (return 503): the feature is off
    /// until configured. Never logged.
    pub connect_hmac_secret: Option<String>,
    /// The key Apiosk's own surfaces (the app's `use-selection` edge function)
    /// present on `X-Apiosk-Service-Key` to spend from the treasury. Never a
    /// buyer credential: it is a service-to-service secret, set on both sides
    /// by the operator. `None` disables service-key auth entirely.
    pub app_service_key: Option<String>,
    /// Which connect token the treasury spends through when a service key is
    /// presented: the token's id (uuid) or its visible prefix. The raw token is
    /// never needed anywhere — see `crate::service_auth`.
    pub treasury_connect_token_ref: Option<String>,
    /// Base URL of the provider portal where the merchant onboarding wizard lives;
    /// the signed onboarding link points here. `CONNECT_PORTAL_URL`, defaults to
    /// the dashboard URL.
    pub connect_portal_url: String,
    /// The settlement asset (USDC ERC-20) contract address the charge `402`
    /// advertises. `CONNECT_SETTLEMENT_ASSET`; defaults to Base mainnet USDC. The
    /// charge settles the gross to the platform wallet via the existing facilitator
    /// (no splitter contract): the seller / marketplace / Apiosk split is recorded
    /// and disbursed at payout, so `payTo` is `platform_wallet`, not a splitter.
    pub connect_settlement_asset: Option<String>,
    /// The x402 network the connect charge settles on. `CONNECT_NETWORK`, defaults
    /// to `base` (use `base-sepolia` during the testnet phase — matches the offramp
    /// chain switch).
    pub connect_network: String,
    /// Referral-marker settings for FEDERATED listings. A federated provider
    /// settles payment directly (no Apiosk proxy in the path), so Apiosk cannot
    /// see those calls — instead it offers the provider a marker to attribute
    /// Apiosk-originated traffic on their own side. One token, two vectors:
    ///   * the gateway-as-payer proxy (`POST /v1/x402/fetch`) stamps the
    ///     `X-Apiosk-Ref: <marker>` header on outbound calls, but ONLY to hosts
    ///     that back a federated listing (`proxy::external_fetch::host_is_federated`)
    ///     — providers Apiosk has a listing relationship with. Any other
    ///     external host the payer proxy reaches learns nothing. This is safe:
    ///     a constant Apiosk controls, never a caller-supplied header.
    ///   * the `/.well-known/x402` discovery document appends
    ///     `?<param>=<marker>` to each republished resource URL, but ONLY when
    ///     `inject_url` is enabled. Rewriting the provider's URL is safe only
    ///     once the provider confirms the query param is pass-through — that it
    ///     does not participate in their x402 resource matching — so it stays
    ///     off until then. Flip `FEDERATED_REF_INJECT_URL=true` to enable it.
    pub federated_ref_param: String,
    pub federated_ref_marker: String,
    pub federated_ref_inject_url: bool,
    /// Public, no-account self-serve federated intake (`GET /federated/new` +
    /// `POST /federated/inspect` + `POST /federated/submit`). Lets an external
    /// x402 provider paste their `/.well-known/x402`, preview the listing, and
    /// submit it into the existing admin review queue (`status='pending'`). Off
    /// by default; the pages 404 until `FEDERATED_SELF_SERVE_ENABLED=true`.
    pub federated_self_serve_enabled: bool,
    /// The reworked discover -> search -> compare -> autofill chain
    /// (`ASK_ENGINE_V2`). Off by default: `/v1/ask` and the flow routes keep
    /// their current pool, lanes and ordering until it is flipped on.
    ///
    /// It lives here, and NOT as a `VITE_` variable in the app, on purpose.
    /// Netlify inlines `VITE_*` at build time, so a flag the browser reads can
    /// only be changed by rebuilding and redeploying — which is not a rollback.
    /// The app learns which engine answered from `meta.engine` on the response
    /// instead, so turning this off is `fly secrets unset ASK_ENGINE_V2` and
    /// nothing else.
    pub ask_engine_v2: bool,
    /// Telemetry v2 settings (all `TELEMETRY_*` env vars). Disabled by default;
    /// see `crate::telemetry`. Purely additive/observability — never on the money
    /// path.
    pub telemetry: crate::telemetry::TelemetrySettings,
    /// Gateway-only model credential. Missing configuration disables inference.
    pub openrouter_api_key: Option<String>,
    /// Model id in OpenRouter's namespace (`vendor/model`). Defaults to
    /// [`crate::query_plan::DEFAULT_OPENROUTER_MODEL`]. Override with
    /// `APIOSK_ENGINE_MODEL`.
    pub openrouter_model: String,
    /// Sent as `HTTP-Referer` / `X-Title` so the spend is attributable in the
    /// OpenRouter dashboard. Cosmetic, but a bill you cannot split by caller is
    /// a bill you cannot act on.
    pub openrouter_referer: String,
    /// Ceiling on the reading call. Env `ASK_READ_TIMEOUT_MS`.
    pub ask_read_timeout_ms: u64,
    /// Ceiling on the judging call. Env `ASK_RERANK_TIMEOUT_MS`. A heavier
    /// model needs more than the default; the doc on the env var says how much.
    pub ask_rerank_timeout_ms: u64,
    /// How many times the judging call may be attempted. Env
    /// `ASK_RERANK_ATTEMPTS`, clamped to 1-2. See
    /// [`DEFAULT_ASK_RERANK_ATTEMPTS`] for why the default is not 1.
    pub ask_rerank_attempts: u64,
    /// Token gating the pipeline-tuning endpoints (`/v1/parse`, `/v1/match`).
    /// They exist for the discover testbench only — never for agents, the n8n
    /// node, or MCP. Unset means the endpoints answer 404 for everyone; a
    /// request must carry the token to see them at all.
    pub debug_endpoints_token: Option<String>,

    /// Key that buys the model reading on `/v1/discover` without a per-IP
    /// budget (`crate::parse_budget`).
    ///
    /// Not an access key: `/v1/discover` answers everyone, with or without it,
    /// because crawlers and unknown agents reading the catalogue is the point.
    /// What it buys is the expensive half — the Gumloop run behind the sentence
    /// — for callers we know, which in practice means Apiosk's own surfaces
    /// calling their own gateway. Unset means nobody is trusted this way and
    /// every caller is budgeted on identity alone.
    pub parse_key: Option<String>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        // Preserve the commerce pool's current permissions and quote-signing fallback.
        // Do not mutate DATABASE_URL: the v2 pool reads its own credential concurrently.
        let database_url = std::env::var("EXECUTOR_DATABASE_URL")
            .context("EXECUTOR_DATABASE_URL must be set for the native commerce runtime")?;
        Self::from_database_url(database_url)
    }

    pub(crate) fn from_database_url(database_url: String) -> Result<Self> {
        let dashboard_url = std::env::var("DASHBOARD_URL")
            .unwrap_or_else(|_| "https://dashboard.apiosk.com".to_string())
            .trim()
            .trim_end_matches('/')
            .to_string();
        let buyer_portal_url = std::env::var("BUYER_PORTAL_URL")
            .or_else(|_| std::env::var("NEXT_PUBLIC_BUYER_PORTAL_URL"))
            .unwrap_or_else(|_| "https://buyer.apiosk.com".to_string())
            .trim()
            .trim_end_matches('/')
            .to_string();
        // Falls back to the database URL, which is already a per-deployment
        // secret this process holds. Not ideal as a signing key and much better
        // than a constant: set QUOTE_SIGNING_SECRET to separate them.
        let quote_signing_secret = std::env::var("QUOTE_SIGNING_SECRET")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| {
                tracing::info!(
                    "QUOTE_SIGNING_SECRET unset — deriving the quote signing key from the commerce database credential"
                );
                format!("apiosk-quote:{database_url}")
            });

        let mcp_proxy_upstream_urls = parse_mcp_proxy_upstream_urls();
        let payment_providers = crate::pay::parse_payment_providers();

        let supabase_url = std::env::var("SUPABASE_URL")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty());
        let supabase_api_key = std::env::var("SUPABASE_AUTH_API_KEY")
            .ok()
            .or_else(|| std::env::var("SUPABASE_ANON_KEY").ok())
            .or_else(|| std::env::var("SUPABASE_SERVICE_ROLE_KEY").ok())
            .or_else(|| std::env::var("SUPABASE_SERVICE_KEY").ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let supabase_service_role_key = std::env::var("SUPABASE_SERVICE_ROLE_KEY")
            .ok()
            .or_else(|| std::env::var("SUPABASE_SERVICE_KEY").ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        let platform_wallet = std::env::var("PLATFORM_WALLET")
            .unwrap_or_else(|_| "0xB581c1F0d12331E3848043760B830695D3480574".to_string());

        // Legacy flat-rate commission. NOT the fee model any more: the hybrid
        // model (`crate::fees`) is two-sided — buyer +15%, managed provider
        // −10%, federated provider 0 — and no single rate can express it.
        //
        // Kept because the value is still reported alongside historical rows and
        // read by the retired flat-rate settle path. The default is the managed
        // class's share of the buyer total (25/115 ≈ 0.2174), matching what
        // migration 088 backfills into apis.commission_rate, so a deployment
        // that leaves this alone reports the same number the ledger books.
        //
        // Changing it does NOT change what anyone is charged.
        let default_commission_rate = std::env::var("DEFAULT_COMMISSION_RATE")
            .unwrap_or_else(|_| "0.2174".to_string())
            .parse()
            .unwrap_or(0.2174);

        // Default to the CDP mainnet facilitator. On mainnet this is the only
        // facilitator that earns real revenue AND feeds the CDP Bazaar index, so
        // a missing/empty FACILITATOR_URL must never silently fall back to the
        // testnet x402.org facilitator (where settlements earn nothing and are
        // not cataloged). The startup guard below enforces this.
        let facilitator_url = std::env::var("FACILITATOR_URL")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| CDP_MAINNET_FACILITATOR_URL.to_string());
        let cdp_api_key_id = std::env::var("CDP_API_KEY_ID").ok().and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });
        let cdp_api_key_secret = std::env::var("CDP_API_KEY_SECRET").ok().and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });

        let ga_measurement_id = std::env::var("GA_MEASUREMENT_ID").ok().and_then(|v| {
            let trimmed = v.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });

        let ga_api_secret = std::env::var("GA_API_SECRET").ok().and_then(|v| {
            let trimmed = v.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });

        let require_agent_connect_token = parse_env_bool("REQUIRE_AGENT_CONNECT_TOKEN", false);

        let connect_hmac_secret = std::env::var("CONNECT_HMAC_SECRET")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let app_service_key = std::env::var("APIOSK_APP_SERVICE_KEY")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let treasury_connect_token_ref = std::env::var("APIOSK_TREASURY_CONNECT_TOKEN_REF")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let connect_portal_url = std::env::var("CONNECT_PORTAL_URL")
            .ok()
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| dashboard_url.clone());
        // Base mainnet USDC. Overridable (e.g. Base Sepolia USDC on testnet).
        const BASE_USDC: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
        let connect_settlement_asset = std::env::var("CONNECT_SETTLEMENT_ASSET")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| Some(BASE_USDC.to_string()));
        let connect_network = std::env::var("CONNECT_NETWORK")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "base".to_string());

        // Federated referral marker. `param`/`marker` fall back to sensible
        // defaults so the payer-proxy header is meaningful out of the box; URL
        // injection stays off until a provider confirms the param is
        // pass-through (see the `federated_ref_*` field docs above).
        let federated_ref_param = std::env::var("FEDERATED_REF_PARAM")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "ref".to_string());
        let federated_ref_marker = std::env::var("FEDERATED_REF_MARKER")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "apiosk".to_string());
        let federated_ref_inject_url = parse_env_bool("FEDERATED_REF_INJECT_URL", false);
        let federated_self_serve_enabled = parse_env_bool("FEDERATED_SELF_SERVE_ENABLED", false);
        let ask_engine_v2 = parse_env_bool("ASK_ENGINE_V2", false);

        let telemetry = crate::telemetry::TelemetrySettings::from_env();

        let openrouter_api_key = std::env::var("OPENROUTER_API_KEY")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        // A fixed engine policy prevents stale deployment overrides from
        // silently reintroducing different models in individual stages:
        // Apiosk calls exactly one model, and a variable naming another is
        // ignored, loudly.
        let openrouter_model = crate::query_plan::DEFAULT_OPENROUTER_MODEL.to_string();
        if let Some(configured) = std::env::var("APIOSK_ENGINE_MODEL").ok().map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty() && *v != openrouter_model)
        {
            tracing::warn!(%configured, model = %openrouter_model, "ignoring a model other than Apiosk's OpenRouter model");
        }
        let openrouter_referer = std::env::var("OPENROUTER_REFERER")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "https://apiosk.com".to_string());
        let ask_read_timeout_ms =
            parse_env_u64("ASK_READ_TIMEOUT_MS", DEFAULT_ASK_READ_TIMEOUT_MS).clamp(250, 15_000);
        let ask_rerank_timeout_ms =
            parse_env_u64("ASK_RERANK_TIMEOUT_MS", DEFAULT_ASK_RERANK_TIMEOUT_MS)
                .clamp(250, 10_000);
        let ask_rerank_attempts =
            parse_env_u64("ASK_RERANK_ATTEMPTS", DEFAULT_ASK_RERANK_ATTEMPTS).clamp(1, 2);
        let debug_endpoints_token = std::env::var("DEBUG_ENDPOINTS_TOKEN")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        let parse_key = std::env::var("PARSE_KEY")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());

        // Escape hatch for local development against a testnet facilitator. MUST
        // be set explicitly; production never sets it, so any non-CDP or
        // incomplete facilitator config hard-fails at startup instead of quietly
        // routing real traffic to a facilitator that earns nothing.
        let allow_testnet_facilitator = parse_env_bool("ALLOW_TESTNET_FACILITATOR", false);
        validate_facilitator_config(
            &facilitator_url,
            cdp_api_key_id.as_deref(),
            cdp_api_key_secret.as_deref(),
            allow_testnet_facilitator,
        )?;

        Ok(Self {
            quote_signing_secret,
            database_url,
            dashboard_url,
            buyer_portal_url,
            mcp_proxy_upstream_urls,
            payment_providers,
            supabase_url,
            supabase_api_key,
            supabase_service_role_key,
            platform_wallet,
            default_commission_rate,
            facilitator_url,
            cdp_api_key_id,
            cdp_api_key_secret,
            ga_measurement_id,
            ga_api_secret,
            require_agent_connect_token,
            connect_hmac_secret,
            app_service_key,
            treasury_connect_token_ref,
            connect_portal_url,
            connect_settlement_asset,
            connect_network,
            federated_ref_param,
            federated_ref_marker,
            federated_ref_inject_url,
            federated_self_serve_enabled,
            ask_engine_v2,
            telemetry,
            openrouter_api_key,
            openrouter_model,
            openrouter_referer,
            ask_read_timeout_ms,
            ask_rerank_timeout_ms,
            ask_rerank_attempts,
            debug_endpoints_token,
            parse_key,
        })
    }
}

/// Refuse to start with a facilitator config that would lose money or skip
/// Bazaar indexing. Three failure modes, all fatal in production:
///
/// 1. A testnet (x402.org) facilitator without the explicit
///    `ALLOW_TESTNET_FACILITATOR=true` opt-in — settlements there earn nothing
///    and are not cataloged.
/// 2. Any non-CDP, non-testnet facilitator URL — unknown facilitators are not
///    trusted to settle mainnet revenue, so they require the same opt-in.
/// 3. The CDP facilitator selected but missing CDP API keys — calls would fail
///    auth, so verify/settle (and therefore indexing) can never succeed.
///
/// The opt-in flag only relaxes (1) and (2); it never lets the CDP facilitator
/// run without keys, because that is always broken rather than merely testnet.
fn validate_facilitator_config(
    facilitator_url: &str,
    cdp_api_key_id: Option<&str>,
    cdp_api_key_secret: Option<&str>,
    allow_testnet_facilitator: bool,
) -> Result<()> {
    // `X402_LOCAL_SETTLEMENT_ENABLED` used to exempt the whole check: with the
    // local on-chain backend on, the facilitator was never called and its
    // configuration could not lose money. That backend was the buyer's
    // pay-on-chain rail and is gone, so the facilitator is now the only thing
    // that verifies an inbound proof and these three rules always apply.

    // Must parse as a URL at all — a malformed value would otherwise surface as
    // confusing per-request failures deep in the payment hot path.
    reqwest::Url::parse(facilitator_url)
        .with_context(|| format!("FACILITATOR_URL is not a valid URL: {facilitator_url:?}"))?;

    let is_cdp = is_cdp_facilitator_url(facilitator_url);
    let is_testnet = is_testnet_only_x402_facilitator(facilitator_url);

    if is_cdp {
        // CDP requires both keys to authenticate verify/settle. Without them the
        // gateway can take payments at the protocol level but never settle them.
        if cdp_api_key_id.is_none() || cdp_api_key_secret.is_none() {
            bail!(
                "Refusing to start: FACILITATOR_URL points at the CDP facilitator ({facilitator_url}) \
                 but CDP_API_KEY_ID and/or CDP_API_KEY_SECRET are missing. Set both, or set \
                 ALLOW_TESTNET_FACILITATOR=true with a testnet facilitator for local development."
            );
        }
        return Ok(());
    }

    // Anything that is not the CDP facilitator (testnet or unknown) is only
    // permitted with the explicit local-development opt-in.
    if allow_testnet_facilitator {
        return Ok(());
    }

    if is_testnet {
        bail!(
            "Refusing to start: FACILITATOR_URL is the testnet x402.org facilitator ({facilitator_url}). \
             Mainnet settlements there earn no revenue and are not indexed in the CDP Bazaar. \
             Use {CDP_MAINNET_FACILITATOR_URL} with CDP_API_KEY_ID + CDP_API_KEY_SECRET, or set \
             ALLOW_TESTNET_FACILITATOR=true for local development."
        );
    }

    bail!(
        "Refusing to start: FACILITATOR_URL ({facilitator_url}) is not the CDP mainnet facilitator. \
         Use {CDP_MAINNET_FACILITATOR_URL} with CDP_API_KEY_ID + CDP_API_KEY_SECRET, or set \
         ALLOW_TESTNET_FACILITATOR=true if you are intentionally targeting a non-CDP facilitator \
         for local development."
    );
}

/// Harde bovengrens op het lezen in `/v1/ask`. Een agent wacht hierop; boven
/// deze grens is een lexicaal antwoord beter dan een beter antwoord dat te
/// laat komt.
pub const DEFAULT_ASK_READ_TIMEOUT_MS: u64 = 8_000;
/// Herordenen krijgt méér dan lezen, niet minder: de shortlist is honderdtwintig
/// kandidaten (~18k tokens, zie `ask::SHORTLIST`) en het antwoord tot 1.200
/// tokens gestructureerde JSON. Op 2026-09-05 stond dit op 6 s en liep ELKE
/// herordening in productie tegen die grens aan (`timing_ms.rerank` = 12.003,
/// twee pogingen van precies 6 s): geen enkel antwoord had nog een oordeel,
/// alles viel terug op woordtelling, en de App las `confident: false` als
/// "niets gevonden". Tien seconden per poging past nog binnen de 30 s die de
/// App op één `/v1/ask` wacht (lezen ≤ 8 s + 2 × 10 s). Een zwaarder model via
/// `ASK_RERANK_MODEL` haalt ook dit niet altijd; zet dan `ASK_RERANK_TIMEOUT_MS`.
pub const DEFAULT_ASK_RERANK_TIMEOUT_MS: u64 = 10_000;

/// Twee pogingen, en de tweede is geen luxe.
///
/// Valt deze call weg, dan rangschikt niets de kandidaten meer en zakt het
/// antwoord terug op woordtelling. Gemeten op "Where is Mollie B.V.
/// registered?": het KVK-register krijgt van het model `fit` 90 en lexicaal
/// relevantie 59 — hetzelfde antwoord, onherkenbaar slechter geordend. Eén
/// time-out kostte dus het hele oordeel.
///
/// De prijs staat er tegenover en is eerlijk: bij een time-out verdubbelt de
/// slechtste wachttijd van deze stap (10 s → 20 s). Alleen bij een mislukking;
/// een geslaagde eerste poging kost geen milliseconde extra. Wie die ruil niet
/// wil, zet `ASK_RERANK_ATTEMPTS=1`.
pub const DEFAULT_ASK_RERANK_ATTEMPTS: u64 = 2;

/// `NAME=1500` → 1500; leeg, ontbrekend of onleesbaar → `default`. Nul telt
/// als onleesbaar: een time-out van nul is nooit wat iemand bedoelde.
fn parse_env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

fn parse_env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| {
            let s = v.trim().to_ascii_lowercase();
            s == "1" || s == "true" || s == "yes" || s == "on"
        })
        .unwrap_or(default)
}

fn parse_mcp_proxy_upstream_urls() -> Vec<String> {
    let configured = std::env::var("MCP_PROXY_UPSTREAMS")
        .ok()
        .map(|raw| {
            raw.split([',', '\n'])
                .filter_map(normalize_mcp_proxy_upstream_url)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if configured.is_empty() {
        default_mcp_proxy_upstream_urls()
    } else {
        configured
    }
}

fn normalize_mcp_proxy_upstream_url(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.ends_with("/mcp") {
        Some(trimmed.to_string())
    } else {
        Some(format!("{trimmed}/mcp"))
    }
}

fn default_mcp_proxy_upstream_urls() -> Vec<String> {
    let mut urls = Vec::new();

    if runs_on_fly() {
        urls.push("http://apiosk-mcp.internal:3000/mcp".to_string());
    }

    urls.push("https://apiosk-mcp.fly.dev/mcp".to_string());
    urls.push("https://mcp.apiosk.com/mcp".to_string());
    urls
}

// Single definition lives in `network_env`, where it also decides mainnet vs
// testnet. Two copies could drift and disagree about what "production" means.
use crate::network_env::runs_on_fly;

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: Option<&str> = Some("test-key");

    #[test]
    fn cdp_facilitator_with_keys_is_accepted() {
        assert!(validate_facilitator_config(CDP_MAINNET_FACILITATOR_URL, KEY, KEY, false,).is_ok());
    }

    #[test]
    fn cdp_facilitator_without_keys_is_rejected_even_with_optin() {
        // Missing keys is always broken, never merely "testnet" — the opt-in
        // must NOT rescue it.
        assert!(
            validate_facilitator_config(CDP_MAINNET_FACILITATOR_URL, None, None, true).is_err()
        );
        assert!(validate_facilitator_config(CDP_MAINNET_FACILITATOR_URL, KEY, None, true).is_err());
        assert!(validate_facilitator_config(CDP_MAINNET_FACILITATOR_URL, None, KEY, true).is_err());
    }

    #[test]
    fn testnet_facilitator_is_rejected_without_optin() {
        assert!(
            validate_facilitator_config("https://www.x402.org/facilitator", None, None, false,)
                .is_err()
        );
    }

    #[test]
    fn testnet_facilitator_is_allowed_with_explicit_optin() {
        assert!(
            validate_facilitator_config("https://www.x402.org/facilitator", None, None, true,)
                .is_ok()
        );
    }

    #[test]
    fn unknown_facilitator_is_rejected_without_optin() {
        assert!(validate_facilitator_config(
            "https://evil.example.com/facilitator",
            KEY,
            KEY,
            false
        )
        .is_err());
    }

    #[test]
    fn malformed_facilitator_url_is_rejected() {
        assert!(validate_facilitator_config("not a url", KEY, KEY, true).is_err());
    }
}
