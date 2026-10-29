//! External x402 fetch — the gateway-as-payer proxy (`POST /v1/x402/fetch`).
//!
//! Lets a connected buyer's managed wallet pay an EXTERNAL x402 endpoint that
//! Apiosk does not host. The MCP holds no signing keys, so it delegates here.
//! Flow:
//!   1. Resolve the connect token + enforce the wallet's per-tx / daily caps
//!      (shared with the normal proxy path via `super::wallet`).
//!   2. Preflight the target URL, parse its 402 offer (`super::resell::parse_upstream_402`).
//!   3. Refuse unless: price ≤ the price the user confirmed, ≤ the global ceiling,
//!      the host is allow-listed, and the rail is Base + USDC.
//!      Then price it: `compute_fee_atomic` = max(bps markup, fee floor). The
//!      markup is the hybrid model's flat 15%; the floor is 0 by default — see
//!      `DEFAULT_FEE_MIN_ATOMIC`. A caller bounds the result with
//!      `max_total_usdc`.
//!   4. Sign an EIP-3009 payment from the platform payer wallet
//!      (`super::resell::build_x402_payment_header`) and retry the request.
//!   5. On upstream 2xx, debit the buyer's managed wallet (an `external_x402_fetches`
//!      row, which `get_wallet_paid_spend_today` sums into the daily cap) and
//!      return the provider response.
//!
//! SAFETY — moves real USDC:
//! - Ships DISABLED: `ExternalFetchPayer::from_env` returns None unless
//!   `EXTERNAL_FETCH_ENABLED=true`, so the route 403s (`feature_disabled`).
//! - Fail-closed: any missing key/cap/offer/policy → refuse, never pay.
//! - Pay-then-debit ordering: the provider is paid first (their own x402 pull on
//!   2xx); the buyer is debited only after a 2xx, so a failed upstream never
//!   charges the user. A 2xx whose debit write fails is recorded
//!   `platform_absorbed` (the platform eats it) rather than double-charging.
//! - Idempotency-Key required; a replayed key never pays twice.

use super::resell::{
    build_x402_payment_header, parse_upstream_402_with_header, usd_to_atomic_u128,
};
use super::*;

use crate::rate_limit::{RateLimitDecision, RateLimitPolicy};
use ethers_signers::LocalWallet;
use reqwest::Method as ReqMethod;
use serde_json::json;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

/// Only Base USDC is payable by the platform payer at launch.
const BASE_CHAIN_ID: u64 = 8453;
const MAX_RESPONSE_BYTES: usize = 512 * 1024;
/// How much of a refusal body is read, and how much of it is repeated back.
/// A refusal is a sentence; anything past this is a provider's error page.
const MAX_DECLINE_BYTES: usize = 4096;
const MAX_DECLINE_CHARS: usize = 300;
/// Cap on the JSON body forwarded to the provider. A paid API call carries a
/// query, not an upload; the gateway's own 8 MiB buffer is far above this.
const MAX_OUTBOUND_BODY_BYTES: usize = 256 * 1024;
const MAX_URL_BYTES: usize = 2048;
/// The verbs a paid call may use. Connection-level and cache-control verbs
/// (CONNECT, TRACE, OPTIONS, HEAD, DELETE) are refused rather than defaulted
/// to GET, so a typo is a 400 and not a silently different request.
const ALLOWED_METHODS: [&str; 4] = ["GET", "POST", "PUT", "PATCH"];
/// Paid external fetches per connect token per minute. Same figure as
/// `/v1/do`: a workflow node's pace, not a scraper's.
const FETCH_RATE_LIMIT: RateLimitPolicy = RateLimitPolicy::per_minute(30);
const PREFLIGHT_TIMEOUT_SECS: u64 = 12;
const PAID_TIMEOUT_SECS: u64 = 30;
const DEFAULT_MAX_PRICE_ATOMIC: u128 = 1_000_000; // 1.00 USDC
/// The federated buyer markup: 15%, the one buyer-facing rule of the hybrid fee
/// model. Mirrors [`crate::fees::BUYER_MARKUP_BPS`] — this route is the
/// federated leg of that model, so the two must not drift.
///
/// THIS IS NOW MARGIN, NOT REIMBURSEMENT. Under the previous flat-rate model the
/// payer wallet fronted the provider payment and was reimbursed as the 98%
/// `developer` leg of the settle split, so the old 200 bps was almost exactly
/// break-even for the bridge and left nothing over. The hybrid model separates
/// the two: the federated provider is paid their **full asking price** and
/// Apiosk keeps the buyer's 15% on top. The bridge is made whole by the provider
/// leg itself, and the markup is the take.
const DEFAULT_FEE_BPS: u32 = crate::fees::BUYER_MARKUP_BPS as u32; // 15%

/// Floor under the platform fee, in atomic USDC. **Zero by default**, tunable
/// via `EXTERNAL_FETCH_FEE_MIN_USDC`.
///
/// WHY THE FLOOR IS GONE. It used to be $0.01, and at a $0.10 provider price the
/// markup was near a cent — so the two agreed by coincidence and disagreed
/// everywhere else. Below $0.10 the floor dominated, and the buyer paid more
/// than list + 15%: on a $0.005 call a cent is a 200% markup. The hybrid model
/// makes "buyer price = API price + 15%" a promise that holds at every price,
/// and a floor silently breaks it on the majority of real traffic (a fifth of
/// observed external x402 payments price under $0.02).
///
/// The economics that motivated the floor are real — a sub-cent call can still
/// cost more in gas than its 15% earns. That is now handled where it belongs, by
/// declining uneconomic calls rather than by quietly re-pricing them: set
/// `EXTERNAL_FETCH_FEE_MIN_USDC` if a deployment needs a floor, and disclose it
/// as "list + 15%, minimum X" rather than as "list + 15%".
const DEFAULT_FEE_MIN_ATOMIC: u128 = 0;

/// Platform hot wallet + policy for the external-fetch payer. Built once at
/// startup; `None` ⇒ the feature is disabled and the route 403s. Mirrors
/// `super::resell::ResellPayer` but with its own key + ceiling so the two payers
/// stay isolated (blast radius = this wallet's balance).
pub struct ExternalFetchPayer {
    wallet: LocalWallet,
    /// Global per-call price ceiling (atomic USDC), independent of the buyer cap.
    pub max_price_atomic: u128,
    /// Platform fee added on top of the provider price, in basis points.
    /// Defaults to the hybrid model's 15% buyer markup.
    pub fee_bps: u32,
    /// Floor under that fee, in atomic USDC. Zero by default — see
    /// `DEFAULT_FEE_MIN_ATOMIC` for why the old $0.01 floor was removed.
    pub fee_min_atomic: u128,
    /// Allowlist-first: when true, a host must have an explicit `allow` row.
    pub default_deny: bool,
    /// Optional per-tx cap on the platform payer itself.
    pub cap_per_tx_atomic: Option<u128>,
}

impl ExternalFetchPayer {
    /// Load from env. Returns `None` (feature disabled) unless
    /// `EXTERNAL_FETCH_ENABLED` is truthy AND a usable payer key is present —
    /// fail closed, never panic.
    pub fn from_env() -> Option<Self> {
        if !env_bool("EXTERNAL_FETCH_ENABLED", false) {
            // Say WHICH precondition failed, at the one moment somebody can
            // act on it. Both this and a missing key produce the same opaque
            // 403 `feature_disabled` at the route, and an operator staring at
            // that message has no way to tell "the flag is off" from "the flag
            // is on and the wallet is missing" — which is exactly the guessing
            // game this line ends.
            info!(
                "External x402 fetch OFF: EXTERNAL_FETCH_ENABLED is not set to true \
                 (accepted: 1/true/yes/on). Paid external calls will 403 feature_disabled."
            );
            return None;
        }
        // The same treasury wallet the reviewed route pays from. One key, one
        // address to fund, one balance to watch — see `resell::treasury_key`.
        let Some(key) = super::resell::treasury_key() else {
            error!(
                "External x402 fetch OFF: EXTERNAL_FETCH_ENABLED is true but {} is not set. \
                 Paid external calls will 403 feature_disabled until it is.",
                super::resell::TREASURY_KEY_VAR
            );
            return None;
        };
        let wallet = match LocalWallet::from_str(&key) {
            Ok(w) => w,
            Err(_) => {
                error!(
                    "{} is set but is not a valid private key; external fetch disabled",
                    super::resell::TREASURY_KEY_VAR
                );
                return None;
            }
        };
        let max_price_atomic =
            env_usd_atomic("EXTERNAL_FETCH_MAX_PRICE_USDC").unwrap_or(DEFAULT_MAX_PRICE_ATOMIC);
        let fee_bps = std::env::var("EXTERNAL_FETCH_FEE_BPS")
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(DEFAULT_FEE_BPS);
        let fee_min_atomic =
            env_usd_atomic("EXTERNAL_FETCH_FEE_MIN_USDC").unwrap_or(DEFAULT_FEE_MIN_ATOMIC);
        // Allowlist-first unless explicitly opened to allow-by-default.
        let default_deny = std::env::var("EXTERNAL_FETCH_DEFAULT_POLICY")
            .map(|v| !v.trim().eq_ignore_ascii_case("allow"))
            .unwrap_or(true);
        Some(Self {
            wallet,
            max_price_atomic,
            fee_bps,
            fee_min_atomic,
            default_deny,
            cap_per_tx_atomic: env_usd_atomic("EXTERNAL_FETCH_CAP_PER_TX_USDC"),
        })
    }

    pub fn wallet(&self) -> &LocalWallet {
        &self.wallet
    }

    pub fn address(&self) -> ethers_core::types::Address {
        ethers_signers::Signer::address(&self.wallet)
    }
}

/// Platform fee for one external fetch: the hybrid model's 15% markup on the
/// provider price, with an optional floor under it (zero by default, so the
/// buyer pays exactly list + 15%).
///
/// The two knobs are independent — the fee is the greater of the two, so a
/// genuinely free pass-through needs BOTH `EXTERNAL_FETCH_FEE_BPS=0` and
/// `EXTERNAL_FETCH_FEE_MIN_USDC=0`. A zero `quoted_atomic` is rejected upstream
/// before this is called, so the floor never applies to a non-payment.
///
/// The bps leg is [`crate::fees::markup_atomic`] — rounded UP to the atomic
/// unit, the one rule for every markup in the gateway — so this route and the
/// hybrid quote never disagree by a unit on the same call.
fn compute_fee_atomic(quoted_atomic: u128, fee_bps: u32, fee_min_atomic: u128) -> u128 {
    // `fees` works in i128. A provider price is bounded by the payer ceiling
    // long before the product `price × bps` could leave that range; an absurd
    // value that still would is saturated rather than wrapped, and never
    // reaches a payment (the ceiling check refuses it first).
    let bps_fee = match i128::try_from(quoted_atomic)
        .ok()
        .filter(|q| q.checked_mul(fee_bps as i128).is_some())
    {
        Some(quoted) => {
            u128::try_from(crate::fees::markup_atomic(quoted, fee_bps as i128)).unwrap_or(u128::MAX)
        }
        None => (quoted_atomic / 10_000).saturating_mul(fee_bps as u128),
    };
    bps_fee.max(fee_min_atomic)
}

fn env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| {
            let s = v.trim().to_ascii_lowercase();
            s == "1" || s == "true" || s == "yes" || s == "on"
        })
        .unwrap_or(default)
}

fn env_usd_atomic(var: &str) -> Option<u128> {
    let raw = std::env::var(var).ok()?;
    let bd: BigDecimal = raw.trim().parse().ok()?;
    usd_to_atomic_u128(&bd)
}

/// USDC (f64 from the JSON body) → atomic units (6 dp). Floors sub-atomic
/// precision; returns None on negative/non-finite input.
fn usdc_f64_to_atomic(usdc: f64) -> Option<u128> {
    if !usdc.is_finite() || usdc < 0.0 {
        return None;
    }
    Some((usdc * 1_000_000.0).floor() as u128)
}

fn atomic_to_bigdecimal(atomic: u128) -> BigDecimal {
    BigDecimal::from_str(&atomic.to_string()).unwrap_or_default() / BigDecimal::from(1_000_000u64)
}

#[derive(Debug, Deserialize)]
pub struct FetchBody {
    pub url: String,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub query: Option<serde_json::Value>,
    #[serde(default)]
    pub body: Option<serde_json::Value>,
    // Note: the MCP may send a `headers` object, but the gateway intentionally
    // does NOT forward arbitrary caller headers (only accept/content-type) to
    // avoid header injection / secret leakage. Unknown JSON fields are ignored.
    pub confirmed_price_usdc: f64,
    /// Caller's ceiling on the PROVIDER price alone.
    #[serde(default)]
    pub max_price_usdc: Option<f64>,
    /// Caller's ceiling on provider price + platform fee — what actually leaves
    /// the wallet. Optional; omit to accept the published tariff.
    #[serde(default)]
    pub max_total_usdc: Option<f64>,
}

/// Indicative direct-fetch totals and directory payment eligibility, computed
/// with the same configured fee arithmetic as the payer.
///
/// The comparison screen has to answer "what does this cost me?" for an
/// endpoint outside the catalogue, and there is exactly one honest way to do
/// that: ask the code that will charge. A separate estimate in the discovery
/// layer would drift from `fetch` the first time a knob moved, and the drift
/// would show up as a price on screen that is not the price debited.
///
/// This checks the listing's price, host policy and declared network. It does
/// not validate an executable contract, authenticate a buyer or inspect a live
/// 402. A row marked eligible still needs those checks, including the real
/// price, real rail and buyer limits, before payment. The estimate is not an
/// approved purchase quote.
///
/// One query for every host in the set, rather than one per offer: a sweep
/// returns dozens of them and the comparison is on the hot path of a person
/// waiting.
pub async fn quote_external_offers(
    state: &Arc<AppState>,
    offers: &[crate::bazaar::ExternalOffer],
) -> Vec<crate::bazaar::ExternalSettlement> {
    let refuse = |reason: &str| crate::bazaar::ExternalSettlement {
        payable: false,
        buyer_price_usd: None,
        reason: Some(reason.to_string()),
    };

    let Some(payer) = state.external_fetch_payer.as_ref() else {
        return offers
            .iter()
            .map(|_| refuse("Apiosk is not settling external endpoints on this deployment."))
            .collect();
    };

    // The host policy table, read once for the whole set.
    let hosts: Vec<String> = offers
        .iter()
        .map(|o| o.host.trim().to_ascii_lowercase())
        .filter(|h| !h.is_empty())
        .collect();
    let rows: Vec<(String, String, Option<BigDecimal>)> = sqlx::query_as(
        "SELECT host, policy, max_price_usdc FROM x402_host_policy WHERE host = ANY($1)",
    )
    .bind(&hosts)
    .fetch_all(&state.db.pool)
    .await
    .unwrap_or_default();

    offers
        .iter()
        .map(|offer| {
            let host = offer.host.trim().to_ascii_lowercase();
            let policy = rows.iter().find(|(h, _, _)| *h == host);

            // A price nobody published cannot be quoted, so it cannot be pinned
            // and cannot be bought. Shown, still — an endpoint that exists is
            // worth knowing about — but not as a purchase.
            let Some(list_usd) = offer.price_usd.filter(|p| *p > 0.0) else {
                return refuse("This listing publishes no price, so there is nothing to quote or pin.");
            };
            let Some(list_atomic) = BigDecimal::try_from(list_usd)
                .ok()
                .as_ref()
                .and_then(usd_to_atomic_u128)
            else {
                return refuse("This listing's price could not be read as an amount.");
            };

            if !host_is_safe(&host) {
                return refuse("This host is not one the gateway will pay.");
            }
            match policy {
                Some((_, verdict, _)) if verdict == "deny" => {
                    return refuse("This host is on the gateway's deny list.")
                }
                Some((_, _, Some(cap))) if usd_to_atomic_u128(cap).is_some_and(|c| list_atomic > c) => {
                    return refuse("This host's price is above the cap set for it.")
                }
                None if payer.default_deny => {
                    return refuse("This host is not on the gateway's external-payment allowlist yet.")
                }
                _ => {}
            }
            if list_atomic > payer.max_price_atomic {
                return refuse("Above the ceiling the gateway will pay for a single external call.");
            }
            // The rail. Only Base USDC is payable by the platform payer, and a
            // listing that names another chain is refused here rather than at
            // the 402. A listing that names none is left in: `fetch` reads the
            // rail off the live challenge, which is the authority anyway, and
            // refuses without paying if it is not Base.
            if let Some(network) = offer.network.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                let network = network.to_ascii_lowercase();
                if !matches!(network.as_str(), "base" | "eip155:8453" | "8453") {
                    return refuse("Apiosk settles external calls on Base USDC only, and this endpoint is on another chain.");
                }
            }

            // The same arithmetic `fetch` charges: the provider's full asking
            // price, plus the platform fee, both from the payer's own config.
            let fee_atomic = compute_fee_atomic(list_atomic, payer.fee_bps, payer.fee_min_atomic);
            crate::bazaar::ExternalSettlement {
                payable: true,
                buyer_price_usd: Some((list_atomic + fee_atomic) as f64 / 1_000_000.0),
                reason: None,
            }
        })
        .collect()
}

/// Basic SSRF guard: https only, and reject localhost / private / metadata hosts
/// by literal. (DNS-rebind pinning is a later hardening; the payer wallet + caps
/// bound the blast radius in the meantime.)
fn host_is_safe(host: &str) -> bool {
    let h = host
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    if h.is_empty()
        || h == "localhost"
        || h == "metadata"
        || h == "metadata.google.internal"
        || h == "169.254.169.254"
    {
        return false;
    }
    // Literal IPv4 private / loopback / link-local ranges.
    let octets: Vec<&str> = h.split('.').collect();
    if octets.len() == 4 && octets.iter().all(|o| o.parse::<u8>().is_ok()) {
        let p: Vec<u8> = octets.iter().map(|o| o.parse::<u8>().unwrap()).collect();
        let private = p[0] == 10
            || p[0] == 127
            || p[0] == 0
            || (p[0] == 192 && p[1] == 168)
            || (p[0] == 172 && (16..=31).contains(&p[1]))
            || (p[0] == 169 && p[1] == 254);
        if private {
            return false;
        }
    }
    // IPv6 loopback / unique-local / link-local literals.
    if h.contains(':')
        && (h == "::1" || h.starts_with("fc") || h.starts_with("fd") || h.starts_with("fe80"))
    {
        return false;
    }
    true
}

/// Build an outbound request with optional query params + JSON body. The only
/// headers ever sent are `accept`, a JSON `content-type` (when there is a body),
/// and — when `ref_marker` is set — a constant `X-Apiosk-Ref` referral marker.
/// Arbitrary caller headers are still NOT forwarded (no header injection, no
/// secret leakage); `X-Apiosk-Ref` is a fixed value Apiosk controls, not a
/// caller-supplied one, so it carries none of that risk.
fn build_outbound(
    client: &reqwest::Client,
    method: &ReqMethod,
    url: &str,
    query: Option<&serde_json::Value>,
    body: Option<&serde_json::Value>,
    timeout_secs: u64,
    ref_marker: Option<&str>,
) -> reqwest::RequestBuilder {
    let mut rb = client
        .request(method.clone(), url)
        .header("accept", "application/json")
        .timeout(std::time::Duration::from_secs(timeout_secs));
    // Referral attribution, set by the caller ONLY for hosts that back a
    // federated listing (see `host_is_federated`). Apiosk settles those calls as
    // payer, so a constant, platform-controlled header lets the provider count
    // Apiosk-originated traffic on their own side. The discovery-URL vector of
    // the same marker lives in `well_known::well_known_x402`.
    if let Some(marker) = ref_marker.map(str::trim).filter(|s| !s.is_empty()) {
        rb = rb.header("X-Apiosk-Ref", marker);
    }
    if let Some(serde_json::Value::Object(map)) = query {
        let pairs: Vec<(String, String)> = map
            .iter()
            .map(|(k, v)| {
                let val = match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                (k.clone(), val)
            })
            .collect();
        rb = rb.query(&pairs);
    }
    if let Some(b) = body {
        rb = rb.json(b);
    }
    rb
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// POST /v1/x402/fetch handler.
pub async fn external_x402_fetch(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap<HeaderValue>,
    Json(body): Json<FetchBody>,
) -> Response {
    // 1. Feature gate.
    let Some(payer) = state.external_fetch_payer.as_ref() else {
        return json_error_response(
            StatusCode::FORBIDDEN,
            "feature_disabled",
            "External x402 fetch is not enabled on this gateway.",
        );
    };

    // 2. Idempotency-Key (required, mirrors the prepaid path).
    let Some(idem) = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    else {
        return json_error_response(
            StatusCode::BAD_REQUEST,
            "idempotency_key_required",
            "An Idempotency-Key header is required.",
        );
    };

    // 3. Parse + SSRF-guard the target URL.
    let target = match reqwest::Url::parse(body.url.trim()) {
        Ok(u) => u,
        Err(_) => {
            return json_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_url",
                "Target url is not a valid URL.",
            )
        }
    };
    if target.scheme() != "https" {
        return json_error_response(
            StatusCode::BAD_REQUEST,
            "insecure_url",
            "Only https:// targets are payable.",
        );
    }
    let host = target.host_str().unwrap_or("").to_string();
    if !host_is_safe(&host) {
        return json_error_response(
            StatusCode::BAD_REQUEST,
            "host_blocked",
            "Refusing to pay a local/private/metadata host.",
        );
    }

    // 4. Resolve the connect token + pick an active managed wallet.
    let ctx = match resolve_connect_token(&state, &headers).await {
        Ok(Some(ctx)) => ctx,
        Ok(None) => {
            return json_error_response(
                StatusCode::UNAUTHORIZED,
                "connect_token_required",
                "Provide a valid X-Apiosk-Connect-Token.",
            )
        }
        Err(resp) => return resp,
    };
    let Some(wallet) = ctx
        .token
        .wallets
        .iter()
        .find(|w| w.wallet_status == "active")
        .or_else(|| ctx.token.wallets.first())
    else {
        return json_error_response(
            StatusCode::FORBIDDEN,
            "no_wallet",
            "No managed wallet is linked to this connect token.",
        );
    };

    if crate::service_auth::presented(&headers).is_none()
        || !state.is_treasury_wallet(&wallet.wallet_address)
    {
        return json_error_response(
            StatusCode::FORBIDDEN,
            "treasury_purchase_required",
            "Only the Apiosk treasury pays providers.",
        );
    }

    // 4b. Per-token rate limit, before any upstream round trip or ledger write.
    //     Keyed on the token because the token is the spender; a replay counts
    //     too, so a retry storm on one key cannot outrun the limit either.
    if let RateLimitDecision::Throttled { retry_after } = state
        .rate_limiter
        .check(
            &format!("x402fetch:{}", ctx.token.token_id),
            FETCH_RATE_LIMIT,
        )
        .await
    {
        warn!(
            token_id = %ctx.token.token_id,
            idempotency_key = %idem,
            "external fetch rate limited"
        );
        let mut response = json_error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Too many /v1/x402/fetch calls on this token. Wait and retry.",
        );
        if let Ok(v) = HeaderValue::from_str(&retry_after.as_secs().max(1).to_string()) {
            response.headers_mut().insert("retry-after", v);
        }
        return response;
    }

    // 5. Idempotency replay: if this key already ran, return its outcome — never
    //    pay twice. A row still `quoted` is a first attempt in flight: 409.
    if let Some(existing) = lookup_existing_fetch(&state, wallet.wallet_id, &idem).await {
        info!(
            wallet_id = %wallet.wallet_id,
            idempotency_key = %idem,
            status = %existing.status,
            "external fetch idempotent replay"
        );
        return replay_response(existing);
    }

    // 5b. Input bounds. The method is an allow-list (a paid call is a read or a
    //     write, never a connection-level verb), the URL is bounded, and the
    //     forwarded JSON body is capped well under the gateway's own buffer so
    //     a caller cannot use the payer as a relay for bulk uploads.
    let method = match body
        .method
        .as_deref()
        .map(|m| m.trim().to_ascii_uppercase())
    {
        None => ReqMethod::GET,
        Some(m) if ALLOWED_METHODS.contains(&m.as_str()) => {
            ReqMethod::from_bytes(m.as_bytes()).unwrap_or(ReqMethod::GET)
        }
        Some(_) => {
            return json_error_response(
                StatusCode::BAD_REQUEST,
                "method_not_allowed",
                "method must be one of GET, POST, PUT, PATCH.",
            )
        }
    };
    if body.url.len() > MAX_URL_BYTES {
        return json_error_response(
            StatusCode::BAD_REQUEST,
            "url_too_long",
            "Target url exceeds the 2048-byte limit.",
        );
    }
    let outbound_body_bytes = body
        .body
        .as_ref()
        .map(|b| serde_json::to_vec(b).map(|v| v.len()).unwrap_or(usize::MAX))
        .unwrap_or(0);
    if outbound_body_bytes > MAX_OUTBOUND_BODY_BYTES {
        return json_error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "body_too_large",
            "The JSON body to forward exceeds the 256 KiB limit.",
        );
    }

    // 6. Confirmed-price gate (the in-chat checkpoint the MCP enforces).
    let Some(confirmed_atomic) = usdc_f64_to_atomic(body.confirmed_price_usdc) else {
        return json_error_response(
            StatusCode::BAD_REQUEST,
            "invalid_confirmed_price",
            "confirmed_price_usdc must be a non-negative number.",
        );
    };

    // 6b. Per-connection destination allowlist (migration 069). This is the
    //     *buyer's* list and is independent of the platform host policy in step
    //     7: the platform decides which hosts Apiosk is willing to pay at all,
    //     the buyer decides which of those their own agent may spend on. Both
    //     must pass.
    {
        let allowed = ctx.allowed_domains();
        if !allowed.is_empty() && !super::domain_policy::host_is_allowed(allowed, &host) {
            warn!(
                "Connect token {} blocked external fetch to {} — not on its {}-entry allowlist",
                ctx.token.token_id,
                host,
                allowed.len(),
            );
            return json_error_response(
                StatusCode::FORBIDDEN,
                "domain_not_allowed",
                &format!(
                    "This connection may not spend on `{host}`. Its allowlist is: {}. The buyer can change it in the Apiosk buyer portal under Connect → allowed domains.",
                    allowed.join(", "),
                ),
            );
        }
    }

    // 7. Host policy (allowlist-first).
    match host_policy(&state, &host).await {
        HostPolicy::Deny => {
            return json_error_response(
                StatusCode::FORBIDDEN,
                "host_denied",
                "This host is on the deny list.",
            )
        }
        HostPolicy::NotListed if payer.default_deny => {
            return json_error_response(
                StatusCode::FORBIDDEN,
                "host_not_allowed",
                "This host is not on the external-fetch allowlist.",
            )
        }
        HostPolicy::Allow {
            max_price_atomic: Some(cap),
        } if confirmed_atomic > cap => {
            return json_error_response(
                StatusCode::FORBIDDEN,
                "host_price_cap_exceeded",
                "Confirmed price exceeds this host's configured max price.",
            )
        }
        _ => {}
    }

    // Referral marker for this target, resolved once and reused by both the
    // preflight and the paid retry so they present identically to the provider.
    // Only federated hosts are marked; everyone else sees no Apiosk header.
    let ref_marker: Option<&str> = if host_is_federated(&state, &host).await {
        Some(state.config.federated_ref_marker.as_str())
    } else {
        None
    };

    // 8. Preflight the target to read its live 402 offer.
    let pre = build_outbound(
        &state.http_client,
        &method,
        target.as_str(),
        body.query.as_ref(),
        body.body.as_ref(),
        PREFLIGHT_TIMEOUT_SECS,
        ref_marker,
    )
    .send()
    .await;
    let pre = match pre {
        Ok(r) => r,
        Err(e) => {
            return json_error_response(
                StatusCode::BAD_GATEWAY,
                "upstream_unreachable",
                &format!("Could not reach the target: {e}"),
            )
        }
    };
    if pre.status().as_u16() != 402 {
        let status = pre.status().as_u16();
        return json_error_response(
            StatusCode::BAD_REQUEST,
            "not_x402",
            &format!("Target did not return 402 (got {status}); it may be free, need auth, or not be an x402 resource."),
        );
    }
    let payment_required_header = pre
        .headers()
        .get("payment-required")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let offer_body = match pre.bytes().await {
        Ok(b) => b,
        Err(_) => {
            return json_error_response(
                StatusCode::BAD_GATEWAY,
                "upstream_body_error",
                "Failed to read the 402 offer.",
            )
        }
    };
    let req = match parse_upstream_402_with_header(
        &offer_body,
        payment_required_header.as_deref(),
        &[BASE_CHAIN_ID],
    ) {
        Ok(r) => r,
        Err(e) => {
            return json_error_response(
                StatusCode::BAD_REQUEST,
                "unsupported_rail",
                &format!("No payable Base + USDC offer: {e}"),
            )
        }
    };

    // 9. Price ceilings: provider price ≤ confirmed ≤ every ceiling.
    let quoted_atomic: u128 = match req.value_atomic.parse() {
        Ok(v) => v,
        Err(_) => {
            return json_error_response(
                StatusCode::BAD_REQUEST,
                "bad_offer_amount",
                "Provider offer amount is unparseable.",
            )
        }
    };
    if quoted_atomic == 0 {
        return json_error_response(
            StatusCode::BAD_REQUEST,
            "zero_amount",
            "Provider offered a zero amount.",
        );
    }
    if quoted_atomic > confirmed_atomic {
        return json_error_response(
            StatusCode::PAYMENT_REQUIRED,
            "price_above_confirmed",
            "Live provider price exceeds the amount you confirmed. Re-inspect and confirm the new price.",
        );
    }
    if quoted_atomic > payer.max_price_atomic {
        return json_error_response(
            StatusCode::FORBIDDEN,
            "price_above_ceiling",
            "Provider price exceeds the platform ceiling.",
        );
    }
    if let Some(req_max) = body.max_price_usdc.and_then(usdc_f64_to_atomic) {
        if quoted_atomic > req_max {
            return json_error_response(
                StatusCode::FORBIDDEN,
                "price_above_request_max",
                "Provider price exceeds max_price_usdc.",
            );
        }
    }
    // Platform payer's own per-tx cap.
    if let Some(cap) = payer.cap_per_tx_atomic {
        if quoted_atomic > cap {
            return json_error_response(
                StatusCode::FORBIDDEN,
                "payer_cap_exceeded",
                "Provider price exceeds the payer per-tx cap.",
            );
        }
    }

    // 10. Buyer wallet caps (per-tx + daily, incl. prior external spend). fee is
    //     added on top of the provider price.
    let fee_atomic = compute_fee_atomic(quoted_atomic, payer.fee_bps, payer.fee_min_atomic);
    let total_atomic = quoted_atomic.saturating_add(fee_atomic);
    let total_usdc = atomic_to_bigdecimal(total_atomic);
    // The confirmation and the ceilings above all bound the PROVIDER price; the
    // fee floor means the total can exceed it by more than a bps markup would.
    // `max_total_usdc` is the caller's guard on what actually leaves the wallet,
    // so a floor can never surprise a buyer who asked to be protected from it.
    if let Some(req_max_total) = body.max_total_usdc.and_then(usdc_f64_to_atomic) {
        if total_atomic > req_max_total {
            return json_error_response(
                StatusCode::FORBIDDEN,
                "total_above_request_max",
                "Provider price plus platform fee exceeds max_total_usdc.",
            );
        }
    }
    if let Err(resp) = enforce_wallet_caps(&state, wallet, &total_usdc).await {
        return resp;
    }

    // 10b. Step 10b used to ask the chain whether the BUYER's wallet could
    //      cover this — USDC balance plus the allowance they granted the
    //      ApioskSettlement contract at connect — before the payer fronted the
    //      provider. There is no buyer wallet on a chain any more: the buyer's
    //      Apiosk balance was debited in the App's Supabase before this request
    //      was made, and the treasury is what pays. The caps check above is the
    //      whole pre-flight.

    // 11. Claim the idempotency key (status 'quoted'). A concurrent duplicate
    //     loses the UNIQUE race and replays.
    let quoted_usdc = atomic_to_bigdecimal(quoted_atomic);
    let fee_usdc = atomic_to_bigdecimal(fee_atomic);
    // One request_id identifies this whole external fetch — stored on the row AND
    // reused as the x402 payment nonce below, so the outbound payment joins back to
    // the gateway request/telemetry that triggered it.
    let request_id = Uuid::new_v4();
    match claim_fetch_row(
        &state,
        request_id,
        wallet.wallet_id,
        &wallet.wallet_address,
        &hash_connect_token_header(&headers),
        ctx.token.user_id,
        &idem,
        target.as_str(),
        &host,
        method.as_str(),
        &quoted_usdc,
        &fee_usdc,
        &format!("{:#x}", req.asset),
        &req.network,
        &format!("{:#x}", req.pay_to),
        ref_marker.is_some(),
    )
    .await
    {
        ClaimOutcome::Claimed => {
            info!(
                request_id = %request_id,
                idempotency_key = %idem,
                wallet_id = %wallet.wallet_id,
                host = %host,
                paid_usdc = %quoted_usdc,
                fee_usdc = %fee_usdc,
                total_usdc = %total_usdc,
                "external fetch claimed; paying provider"
            );
        }
        ClaimOutcome::Duplicate => {
            if let Some(existing) = lookup_existing_fetch(&state, wallet.wallet_id, &idem).await {
                return replay_response(existing);
            }
            return json_error_response(
                StatusCode::CONFLICT,
                "in_progress",
                "A request with this Idempotency-Key is already in progress.",
            );
        }
        ClaimOutcome::Error => {
            return json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "ledger_error",
                "Failed to record the fetch.",
            );
        }
    }

    // 12. Sign the EIP-3009 payment and retry the request WITH the proof.
    let proof = match build_x402_payment_header(payer.wallet(), &req, &request_id, now_unix()) {
        Ok(p) => p,
        Err(e) => {
            update_fetch_status(
                &state,
                wallet.wallet_id,
                &idem,
                "refused",
                Some("sign_failed"),
                None,
                None,
                None,
            )
            .await;
            error!(
                request_id = %request_id,
                idempotency_key = %idem,
                "external fetch: payment signing failed: {e}"
            );
            return json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "sign_failed",
                "Failed to sign the payment; nothing was paid.",
            );
        }
    };
    let proof_for_log = proof.clone();
    let mut paid_rb = build_outbound(
        &state.http_client,
        &method,
        target.as_str(),
        body.query.as_ref(),
        body.body.as_ref(),
        if matches!(target.as_str(), "https://x402engine.app/api/llm/deepseek-v4-flash" | "https://x402engine.app/api/llm/glm-5.3-flash" | "https://x402engine.app/api/llm/minimax-m3" | "https://x402engine.app/api/llm/gpt-5.6-luna" | "https://x402engine.app/api/llm/deepseek-v4-pro" | "https://x402engine.app/api/llm/gemini-3.6-flash" | "https://x402engine.app/api/llm/qwen3.7-plus" | "https://x402engine.app/api/llm/qwen3.7-max" | "https://x402engine.app/api/llm/claude-sonnet-5" | "https://x402engine.app/api/llm/gpt-6.1-sol" | "https://x402engine.app/api/llm/claude-opus-5.5" | "https://x402engine.app/api/llm/gpt-6-astra") { 180 } else { PAID_TIMEOUT_SECS },
        ref_marker,
    )
    .header("x-payment", proof.clone());
    if req.x402_version >= 2 {
        paid_rb = paid_rb.header("payment-signature", proof);
    }
    let paid = match paid_rb.send().await {
        Ok(r) => r,
        Err(e) => {
            update_fetch_status(
                &state,
                wallet.wallet_id,
                &idem,
                "paying",
                Some("payment_unresolved"),
                None,
                None,
                None,
            )
            .await;
            return json_error_response(
                StatusCode::GATEWAY_TIMEOUT,
                "payment_unresolved",
                &format!("Payment outcome is unconfirmed: {e}"),
            );
        }
    };

    let upstream_status = paid.status().as_u16();
    let external_tx = super::resell::payment_transaction(
        paid.headers()
            .get("payment-response")
            .or_else(|| paid.headers().get("x-payment-response"))
            .and_then(|v| v.to_str().ok()),
    );
    if external_tx.is_none() {
        return json_error_response(
            StatusCode::GATEWAY_TIMEOUT,
            "payment_unresolved",
            "Provider payment confirmation is pending. The amount remains held.",
        );
    }

    // A confirmed payment is booked even if the provider's data request failed.
    // The HTTP status travels with the body; it never proves a zero debit.
    // 13. Success: read the body (capped), debit the buyer, return the data.
    let content_type = paid
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let raw = paid.bytes().await.unwrap_or_default();
    let response_bytes = raw.len().min(MAX_RESPONSE_BYTES);
    let slice = &raw[..response_bytes];
    let data: serde_json::Value = serde_json::from_slice(slice)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(slice).to_string()));

    // The buyer is not charged here, and there is nothing to reimburse the
    // bridge for on-chain.
    //
    // This used to be the federated leg of the hybrid model: `settle_exact`
    // pulled `quoted_atomic` back to the bridge wallet and `fee_atomic` to the
    // platform wallet, out of the BUYER's wallet, against the allowance they
    // granted the ApioskSettlement contract at connect. `settle_exact` rather
    // than `settle`, because a flat `platformFeeBps` would take its cut of the
    // total and leave the bridge short on some calls and over-reimbursed on
    // others. All of that assumed a buyer who holds USDC and signs for it.
    //
    // Apiosk keeps one rail now: the buyer's Apiosk balance is debited in the
    // App's Supabase, and the treasury — the same wallet that just fronted this
    // provider's 402 — is the payer of record. So the call is simply booked as
    // debited under an internal tx hash, which is what a gateway without
    // DIRECT_SETTLEMENT_* configured has always done.
    let (charge_tx, settled_status) = (external_tx.clone(), "debited");

    update_fetch_status(
        &state,
        wallet.wallet_id,
        &idem,
        settled_status,
        if settled_status == "platform_absorbed" {
            Some("buyer_charge_failed")
        } else {
            None
        },
        Some(upstream_status as i32),
        external_tx.as_deref(),
        charge_tx.as_deref(),
    )
    .await;
    // The same books the reviewed catalogue is written into.
    //
    // Until now this call ended here, with a row in `external_x402_fetches` and
    // nothing else — and every revenue chart Apiosk has reads `settlements` and
    // `gateway_usage`. So a buyer selecting an unreviewed offer in the app, and
    // the treasury paying that provider's 402 on their behalf, produced money
    // out the door that Revenue → Growth could not see, that Traffic → Gateway
    // showed as an UNPAID request (the `gateway_usage` lateral join found
    // nothing), and that the payment funnel counted as a buyer who never even
    // attempted to pay. `/v1/do` has always written these rows; this is the
    // other half of the same buyer flow finally writing them too.
    //
    // Deliberately after `update_fetch_status` and deliberately unable to fail
    // the request: the provider has been paid and the buyer has been charged,
    // both irreversibly, several lines above. Bookkeeping that could throw away
    // a delivered answer would be worse than the gap it closes.
    if let Some(charge_tx) = charge_tx.as_deref() {
        record_external_fetch_settlement(
            &state,
            &host,
            method.as_str(),
            target.path(),
            charge_tx,
            &wallet.wallet_address,
            quoted_atomic,
            fee_atomic,
            &format!("{:#x}", req.pay_to),
            &format!("{:#x}", payer.address()),
            request_id,
        )
        .await;
    }

    info!(
        request_id = %request_id,
        idempotency_key = %idem,
        wallet_id = %wallet.wallet_id,
        upstream_status,
        paid_usdc = %quoted_usdc,
        fee_usdc = %fee_usdc,
        total_usdc = %total_usdc,
        settled_as = settled_status,
        "external fetch settled"
    );

    let remaining_daily = state
        .db
        .get_wallet_paid_spend_today(&wallet.wallet_address)
        .await
        .ok()
        .map(|spent| (wallet.daily_limit_usdc.clone() - spent).to_string());

    let payload = json!({
        "data": data,
        "upstream_status": upstream_status,
        "content_type": content_type,
        "receipt": {
            "paid_usdc": quoted_usdc.to_string(),
            "fee_usdc": fee_usdc.to_string(),
            "total_usdc": total_usdc.to_string(),
            "network": req.network,
            "pay_to": format!("{:#x}", req.pay_to),
            "charged_from": wallet.wallet_address,
            "external_tx": external_tx,
            "settlement_tx": charge_tx,
            "remaining_daily_usdc": remaining_daily,
            "settled_as": settled_status,
        },
    });

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header("X-Apiosk-Gateway", "true")
        .body(Body::from(payload.to_string()))
        .unwrap_or_else(|_| {
            json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "response_build_error",
                "Failed to build response.",
            )
        })
}

/// Write one external fetch into `settlements` and `gateway_usage`.
///
/// Everything here is best-effort and says so: each failure logs and returns.
/// The provider has been paid and the buyer charged before this is called, so
/// there is no outcome in which refusing to record is better than recording
/// nothing — but there is also nothing left to undo, and an error raised here
/// would only cost the buyer the answer they paid for.
///
/// Routed through `record_paid_execution` rather than writing the rows here.
/// That function owns the split, the developer ledger, the direct-settlement
/// finalisation and the auto-payout decision, and re-implementing any of them
/// in a second place is how two answers to "what did Apiosk earn" come to
/// exist. The `resell` argument is the important one: it tells that path the
/// upstream was already paid inline from the bridge, so the provider leg is a
/// reimbursement and is never enqueued as a payout — which is exactly the shape
/// of an external fetch.
#[allow(clippy::too_many_arguments)]
async fn record_external_fetch_settlement(
    state: &Arc<AppState>,
    host: &str,
    method: &str,
    path: &str,
    charge_tx: &str,
    buyer_wallet: &str,
    quoted_atomic: u128,
    fee_atomic: u128,
    pay_to: &str,
    payer_address: &str,
    request_id: Uuid,
) {
    let quoted_usdc = atomic_to_bigdecimal(quoted_atomic);

    // The listing exists for one reason: `settlements.api_id` is NOT NULL, and
    // a payment the books cannot attribute is a payment the books do not have.
    let listing = match state
        .db
        .ensure_external_listing(host, pay_to, method, path, &quoted_usdc)
        .await
    {
        Ok(Some(listing)) => listing,
        Ok(None) => {
            warn!(host = %host, "external fetch not recorded: host could not be resolved to a listing");
            return;
        }
        Err(e) => {
            error!(host = %host, "external fetch not recorded: {e}");
            return;
        }
    };
    let (api, endpoint) = listing;

    // `record_paid_execution` re-derives the buyer total from `endpoint.price`
    // under the hybrid model's federated leg — list + 10%. That is the same
    // number `compute_fee_atomic` produced, because `DEFAULT_FEE_BPS` IS
    // `fees::BUYER_MARKUP_BPS` and the default minimum is zero. Say so out loud
    // when an env override has pulled them apart, because the symptom otherwise
    // is a revenue chart that is quietly a few percent wrong.
    let modelled_fee =
        crate::fees::markup_atomic(quoted_atomic as i128, crate::fees::BUYER_MARKUP_BPS);
    if modelled_fee != fee_atomic as i128 {
        warn!(
            host = %host,
            charged_fee_atomic = fee_atomic,
            modelled_fee_atomic = modelled_fee,
            "EXTERNAL_FETCH_FEE_BPS/MIN diverges from the hybrid model; the settlement row \
             records the modelled fee, not the fee the buyer paid"
        );
    }

    let tracking = super::TrackingContext {
        request_id: Some(request_id),
        ..Default::default()
    };
    let resell = crate::proxy::resell::ResellOutcome {
        transaction: charge_tx.to_owned(),
        upstream_atomic: quoted_atomic,
        payer_address: payer_address.to_ascii_lowercase(),
    };

    super::settlement::record_paid_execution(
        state,
        &api.slug,
        path,
        &api,
        &endpoint,
        charge_tx,
        Some(buyer_wallet.to_string()),
        &tracking,
        None,
        None,
        Some(&resell),
    )
    .await;
}

// --- Host policy ------------------------------------------------------------

enum HostPolicy {
    Allow { max_price_atomic: Option<u128> },
    Deny,
    NotListed,
}

async fn host_policy(state: &Arc<AppState>, host: &str) -> HostPolicy {
    let row: Option<(String, Option<BigDecimal>)> =
        sqlx::query_as("SELECT policy, max_price_usdc FROM x402_host_policy WHERE host = $1")
            .bind(host)
            .fetch_optional(&state.db.pool)
            .await
            .ok()
            .flatten();
    match row {
        Some((policy, max_price)) if policy == "deny" => {
            let _ = max_price;
            HostPolicy::Deny
        }
        Some((_, max_price)) => HostPolicy::Allow {
            max_price_atomic: max_price.as_ref().and_then(usd_to_atomic_u128),
        },
        None => HostPolicy::NotListed,
    }
}

/// True when `host` backs a FEDERATED listing — an external, already-x402-native
/// API that Apiosk indexes but does not proxy. Only these hosts receive the
/// `X-Apiosk-Ref` referral marker: they are the providers Apiosk actually has a
/// listing relationship with, so attribution means something. Every other
/// external host this payer proxy can reach is a stranger and is told nothing
/// about who is paying.
///
/// Fails CLOSED (no marker) on any DB error or unparseable resource URL. The
/// marker is best-effort attribution and must never block, delay, or alter a
/// payment. Keyed on `listing_type='federated'`, the single authoritative
/// federated discriminator (`is_external` is only a denormalised mirror).
async fn host_is_federated(state: &Arc<AppState>, host: &str) -> bool {
    let host = host.trim().to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    let rows: Vec<(serde_json::Value,)> = sqlx::query_as(
        r#"SELECT external_x402
             FROM apis
            WHERE COALESCE(listing_type::text, 'api') = 'federated'
              AND status = 'active'
              AND COALESCE(active, TRUE) = TRUE"#,
    )
    .fetch_all(&state.db.pool)
    .await
    .unwrap_or_default();

    rows.iter().any(|(resources,)| {
        resources.as_array().is_some_and(|list| {
            list.iter().any(|entry| {
                entry
                    .get("resource")
                    .and_then(|v| v.as_str())
                    .and_then(|url| reqwest::Url::parse(url).ok())
                    .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
                    .is_some_and(|resource_host| resource_host == host)
            })
        })
    })
}

// --- Ledger helpers ---------------------------------------------------------

/// One row of an existing fetch, for idempotent replay.
struct ExistingFetch {
    status: String,
    quoted_price_usdc: BigDecimal,
    fee_usdc: BigDecimal,
    external_tx_hash: Option<String>,
    internal_debit_tx_hash: Option<String>,
    refusal_code: Option<String>,
}

/// The columns `lookup_existing_fetch` reads, in order.
type ExistingFetchRow = (
    String,
    BigDecimal,
    BigDecimal,
    Option<String>,
    Option<String>,
    Option<String>,
);

async fn lookup_existing_fetch(
    state: &Arc<AppState>,
    wallet_id: Uuid,
    idem: &str,
) -> Option<ExistingFetch> {
    let row: Option<ExistingFetchRow> = sqlx::query_as(
            r#"SELECT status, quoted_price_usdc, fee_usdc, external_tx_hash, internal_debit_tx_hash, refusal_code
               FROM external_x402_fetches
               WHERE wallet_id = $1 AND idempotency_key = $2"#,
        )
        .bind(wallet_id)
        .bind(idem)
        .fetch_optional(&state.db.pool)
        .await
        .ok()
        .flatten();
    row.map(
        |(
            status,
            quoted_price_usdc,
            fee_usdc,
            external_tx_hash,
            internal_debit_tx_hash,
            refusal_code,
        )| ExistingFetch {
            status,
            quoted_price_usdc,
            fee_usdc,
            external_tx_hash,
            internal_debit_tx_hash,
            refusal_code,
        },
    )
}

fn replay_response(existing: ExistingFetch) -> Response {
    match existing.status.as_str() {
        "debited" | "platform_absorbed" => {
            let payload = json!({
                "status": "success",
                "replay": true,
                "note": "Idempotent replay — the provider response was not re-fetched (no second charge).",
                "receipt": {
                    "paid_usdc": existing.quoted_price_usdc.to_string(),
                    "fee_usdc": existing.fee_usdc.to_string(),
                    "total_usdc": (&existing.quoted_price_usdc + &existing.fee_usdc).to_string(),
                    "external_tx": existing.external_tx_hash,
                    "internal_debit_tx": existing.internal_debit_tx_hash,
                    "settled_as": existing.status,
                },
            });
            Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json")
                .header("X-Apiosk-Gateway", "true")
                .body(Body::from(payload.to_string()))
                .unwrap_or_else(|_| {
                    json_error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "response_build_error",
                        "replay build failed",
                    )
                })
        }
        // The same error the first attempt answered with, under the same
        // status class: a provider that declined was a 502 then and is a 502
        // now, so a caller's retry policy reads the replay the way it read the
        // original.
        "refused" => {
            let code = existing.refusal_code.as_deref().unwrap_or("refused");
            let status = match code {
                "upstream_declined" | "upstream_error" => StatusCode::BAD_GATEWAY,
                "sign_failed" => StatusCode::INTERNAL_SERVER_ERROR,
                _ => StatusCode::BAD_REQUEST,
            };
            json_error_response(status, code, "This request was previously refused.")
        }
        _ => json_error_response(
            StatusCode::CONFLICT,
            "in_progress",
            "A request with this Idempotency-Key is already in progress.",
        ),
    }
}

enum ClaimOutcome {
    Claimed,
    Duplicate,
    Error,
}

#[allow(clippy::too_many_arguments)]
async fn claim_fetch_row(
    state: &Arc<AppState>,
    request_id: Uuid,
    wallet_id: Uuid,
    wallet_address: &str,
    connect_token_hash: &str,
    user_id: Uuid,
    idem: &str,
    target_url: &str,
    target_host: &str,
    method: &str,
    quoted_price_usdc: &BigDecimal,
    fee_usdc: &BigDecimal,
    asset: &str,
    network: &str,
    pay_to: &str,
    // Whether this call carried `X-Apiosk-Ref` — i.e. the host backs a federated
    // listing. Persisted so the admin funnel can split partner spend from
    // stranger-host spend without re-deriving `host_is_federated` after the fact.
    ref_marker_sent: bool,
) -> ClaimOutcome {
    let result = sqlx::query(
        r#"INSERT INTO external_x402_fetches
             (wallet_id, wallet_address, connect_token_hash, user_id, idempotency_key,
              target_url, target_host, method, quoted_price_usdc, fee_usdc,
              asset, network, pay_to, request_id, ref_marker_sent, status)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,'quoted')
           ON CONFLICT (wallet_id, idempotency_key) DO NOTHING"#,
    )
    .bind(wallet_id)
    .bind(wallet_address)
    .bind(connect_token_hash)
    .bind(user_id)
    .bind(idem)
    .bind(target_url)
    .bind(target_host)
    .bind(method)
    .bind(quoted_price_usdc)
    .bind(fee_usdc)
    .bind(asset)
    .bind(network)
    .bind(pay_to)
    .bind(request_id)
    .bind(ref_marker_sent)
    .execute(&state.db.pool)
    .await;
    match result {
        Ok(r) if r.rows_affected() == 1 => ClaimOutcome::Claimed,
        Ok(_) => ClaimOutcome::Duplicate,
        Err(e) => {
            error!("Failed to claim external fetch row: {e}");
            ClaimOutcome::Error
        }
    }
}

#[allow(clippy::too_many_arguments)]
/// The provider's own words for refusing a request Apiosk had already paid for.
///
/// Capped and best-effort. A provider that answers a refusal with two megabytes
/// of HTML gets the first few hundred characters of it and nothing more, and a
/// body that cannot be read at all comes back `None` — which the caller says
/// out loud rather than papering over, because "the provider refused and would
/// not say why" is itself the finding.
///
/// A JSON error body is the common case and keeps its sentence in one of a
/// small set of places; anything else is returned as the text it is.
fn decline_reason(raw: &[u8]) -> Option<String> {
    let slice = &raw[..raw.len().min(MAX_DECLINE_BYTES)];
    let text = String::from_utf8_lossy(slice);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        let mut parts: Vec<&str> = Vec::new();
        for key in [
            "error",
            "code",
            "message",
            "detail",
            "reason",
            "description",
        ] {
            if let Some(found) = value
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                if !parts.contains(&found) {
                    parts.push(found);
                }
            }
        }
        if !parts.is_empty() {
            return Some(truncate_reason(&parts.join(": ")));
        }
    }

    Some(truncate_reason(trimmed))
}

fn truncate_reason(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(MAX_DECLINE_CHARS).collect()
}

/// What the provider is asking for NOW, against what was just signed and sent.
///
/// A 402 answering a paid request is a re-quote. The provider has restated its
/// terms, and every field in that restatement is one the EIP-712 authorization
/// binds to — so a single difference between the two is not a clue about the
/// failure, it IS the failure. An amount that moved between the offer and the
/// retry, a `payTo` that rotated, an asset or chain we resolved to the wrong
/// entry of `accepts[]`, a protocol version whose dialect we answered in the
/// wrong shape: each of these produces exactly the same opaque 402, and each
/// has a different fix.
///
/// `None` means the second offer is unreadable or matches what was sent —
/// which is itself worth knowing, because it rules out this whole family and
/// points at the signature, the nonce, or the payer's balance instead.
fn describe_offer_change(
    body: &[u8],
    payment_required_header: Option<&str>,
    signed: &super::resell::UpstreamRequirement,
) -> Option<String> {
    let requoted = super::resell::parse_upstream_402_with_header(
        body,
        payment_required_header,
        &[BASE_CHAIN_ID],
    )
    .ok()?;

    let mut changes: Vec<String> = Vec::new();
    if requoted.value_atomic != signed.value_atomic {
        changes.push(format!(
            "amount {} → {}",
            signed.value_atomic, requoted.value_atomic
        ));
    }
    if requoted.pay_to != signed.pay_to {
        changes.push(format!(
            "payTo {:#x} → {:#x}",
            signed.pay_to, requoted.pay_to
        ));
    }
    if requoted.asset != signed.asset {
        changes.push(format!("asset {:#x} → {:#x}", signed.asset, requoted.asset));
    }
    if requoted.network != signed.network || requoted.chain_id != signed.chain_id {
        changes.push(format!("network {} → {}", signed.network, requoted.network));
    }
    if requoted.scheme != signed.scheme {
        changes.push(format!("scheme {} → {}", signed.scheme, requoted.scheme));
    }
    if requoted.x402_version != signed.x402_version {
        changes.push(format!(
            "x402Version {} → {}",
            signed.x402_version, requoted.x402_version
        ));
    }
    if requoted.token_name != signed.token_name || requoted.token_version != signed.token_version {
        changes.push(format!(
            "token domain {}/{} → {}/{}",
            signed.token_name, signed.token_version, requoted.token_name, requoted.token_version
        ));
    }

    (!changes.is_empty()).then(|| changes.join(", "))
}

async fn update_fetch_status(
    state: &Arc<AppState>,
    wallet_id: Uuid,
    idem: &str,
    status: &str,
    refusal_code: Option<&str>,
    upstream_status: Option<i32>,
    external_tx_hash: Option<&str>,
    internal_debit_tx_hash: Option<&str>,
) -> bool {
    let result = sqlx::query(
        r#"UPDATE external_x402_fetches
           SET status = $3,
               refusal_code = COALESCE($4, refusal_code),
               upstream_status = COALESCE($5, upstream_status),
               external_tx_hash = COALESCE($6, external_tx_hash),
               internal_debit_tx_hash = COALESCE($7, internal_debit_tx_hash),
               updated_at = now()
           WHERE wallet_id = $1 AND idempotency_key = $2"#,
    )
    .bind(wallet_id)
    .bind(idem)
    .bind(status)
    .bind(refusal_code)
    .bind(upstream_status)
    .bind(external_tx_hash)
    .bind(internal_debit_tx_hash)
    .execute(&state.db.pool)
    .await;
    matches!(result, Ok(r) if r.rows_affected() == 1)
}

/// Hash the incoming connect token (or provider key) header the same way the
/// wallet path does, so the ledger row is auditable without storing the token.
fn hash_connect_token_header(headers: &HeaderMap<HeaderValue>) -> String {
    let token = crate::service_auth::presented(headers)
        .or_else(|| extract_agent_connect_token(headers))
        .or_else(|| extract_provider_api_key(headers))
        .unwrap_or_default();
    hash_connect_token(&token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_preserves_configured_direct_fetch_fee_and_separates_balance_total() {
        let offer = crate::bazaar::ExternalOffer {
            resource: "https://registry.example.com/company".into(),
            host: "registry.example.com".into(),
            price_usd: Some(0.1),
            ..Default::default()
        };
        for (bps, floor, expected) in [(1_000, 0, 0.11), (1_500, 0, 0.115), (1_000, 20_000, 0.12)] {
            let total = (100_000 + compute_fee_atomic(100_000, bps, floor)) as f64 / 1_000_000.0;
            let value = offer.to_json_settled(&crate::bazaar::ExternalSettlement {
                payable: true,
                buyer_price_usd: Some(total),
                reason: None,
            });
            assert_eq!(value["buyer_price_usd"], json!(expected));
            assert_eq!(value["balance_buyer_price_usd"], json!(0.115));
            assert_eq!(
                value["buyer_price_basis"],
                "indicative_external_fetch_config"
            );
            assert_eq!(value["price_is_estimate"], true);
            // Eligibility never upgrades an undocumented index entry into a
            // verified offer or a promise that the purchase can run.
            assert_eq!(value["verified"], false);
            assert!(value["input_schema"].is_null());
            assert!(value["note"]
                .as_str()
                .unwrap()
                .contains("Neither is an approved quote"));
        }
    }

    /// The settlement row this path now writes gets its fee from the hybrid
    /// model, not from `compute_fee_atomic` — see
    /// `record_external_fetch_settlement`. The two agree only because the
    /// default fee IS the model's markup and the default minimum is zero.
    ///
    /// If someone retunes `DEFAULT_FEE_BPS` on its own, every external fetch
    /// starts reporting revenue that is not what the buyer paid, and the
    /// symptom is a chart that is quietly a few percent wrong — the least
    /// findable kind of accounting bug there is. Fail the build instead.
    #[test]
    fn the_default_external_fee_is_the_hybrid_models_markup() {
        assert_eq!(DEFAULT_FEE_BPS as i128, crate::fees::BUYER_MARKUP_BPS);
        assert_eq!(DEFAULT_FEE_MIN_ATOMIC, 0);

        // And therefore, at the defaults, the charged fee equals the modelled
        // fee for an ordinary price.
        let quoted: u128 = 3_000; // $0.003, the SSL check's own price
        assert_eq!(
            compute_fee_atomic(quoted, DEFAULT_FEE_BPS, DEFAULT_FEE_MIN_ATOMIC) as i128,
            crate::fees::markup_atomic(quoted as i128, crate::fees::BUYER_MARKUP_BPS),
        );
    }

    #[test]
    fn ssrf_guard_blocks_local_and_private_hosts() {
        assert!(host_is_safe("fx.example.com"));
        assert!(host_is_safe("gateway.apiosk.com"));
        assert!(!host_is_safe("localhost"));
        assert!(!host_is_safe("127.0.0.1"));
        assert!(!host_is_safe("169.254.169.254"));
        assert!(!host_is_safe("10.1.2.3"));
        assert!(!host_is_safe("192.168.0.1"));
        assert!(!host_is_safe("172.16.5.5"));
        assert!(!host_is_safe("172.31.255.255"));
        assert!(host_is_safe("172.32.0.1")); // just outside the private range
        assert!(!host_is_safe("metadata.google.internal"));
        assert!(!host_is_safe("::1"));
    }

    #[test]
    fn usdc_f64_to_atomic_floors_and_rejects_negatives() {
        assert_eq!(usdc_f64_to_atomic(0.02), Some(20_000));
        assert_eq!(usdc_f64_to_atomic(1.0), Some(1_000_000));
        assert_eq!(usdc_f64_to_atomic(0.0), Some(0));
        assert_eq!(usdc_f64_to_atomic(-1.0), None);
        assert_eq!(usdc_f64_to_atomic(f64::NAN), None);
    }

    #[test]
    fn atomic_to_bigdecimal_is_exact_for_usdc() {
        assert_eq!(
            atomic_to_bigdecimal(20_000),
            BigDecimal::from_str("0.02").unwrap()
        );
        assert_eq!(
            atomic_to_bigdecimal(1_000_000),
            BigDecimal::from_str("1").unwrap()
        );
        assert_eq!(
            atomic_to_bigdecimal(1_020_000),
            BigDecimal::from_str("1.02").unwrap()
        );
    }

    #[test]
    fn fee_is_the_hybrid_models_fifteen_percent() {
        // The buyer-facing rule: API price + 15%, at every price. On a $1.00
        // provider price that is $0.15, for a $1.15 total.
        let quoted: u128 = 1_000_000;
        let fee = compute_fee_atomic(quoted, DEFAULT_FEE_BPS, DEFAULT_FEE_MIN_ATOMIC);
        assert_eq!(fee, 150_000);
        assert_eq!(quoted.saturating_add(fee), 1_150_000);
    }

    /// The worked example from the fee model note, on the federated rail.
    #[test]
    fn a_ten_cent_federated_call_costs_the_buyer_eleven_and_a_half() {
        let quoted: u128 = 100_000; // $0.10
        let fee = compute_fee_atomic(quoted, DEFAULT_FEE_BPS, DEFAULT_FEE_MIN_ATOMIC);
        assert_eq!(fee, 15_000); // $0.015
        assert_eq!(quoted.saturating_add(fee), 115_000); // $0.115
    }

    /// The old $0.01 floor is gone. It used to dominate every price under
    /// $0.50, so a $0.005 call was charged a cent — a 200% markup dressed as
    /// "2%". `list + 15%` is only a promise if it holds at the bottom of the
    /// range too, where most real external x402 traffic sits.
    #[test]
    fn micro_priced_calls_are_charged_the_markup_not_a_flat_cent() {
        let quoted: u128 = 5_000; // $0.005
        let fee = compute_fee_atomic(quoted, DEFAULT_FEE_BPS, DEFAULT_FEE_MIN_ATOMIC);
        assert_eq!(fee, 750, "15% of $0.005, not the retired $0.01 floor");
        assert_eq!(quoted.saturating_add(fee), 5_750);
        assert_eq!(DEFAULT_FEE_MIN_ATOMIC, 0, "no floor by default");
    }

    /// The markup must stay proportional across the whole range — no crossover
    /// point where a different rule takes over.
    #[test]
    fn the_markup_is_proportional_at_every_price() {
        for quoted in [1_000u128, 5_000, 100_000, 500_000, 1_000_000, 50_000_000] {
            let fee = compute_fee_atomic(quoted, DEFAULT_FEE_BPS, DEFAULT_FEE_MIN_ATOMIC);
            assert_eq!(fee, quoted * 15 / 100, "15% of {quoted}");
        }
    }

    /// A deployment can still put a floor back, but it has to ask for one — and
    /// then the buyer-facing promise is "list + 15%, minimum X", not "list + 15%".
    #[test]
    fn a_configured_floor_still_governs_when_set() {
        assert_eq!(compute_fee_atomic(5_000, DEFAULT_FEE_BPS, 10_000), 10_000);
        assert_eq!(
            compute_fee_atomic(1_000_000, DEFAULT_FEE_BPS, 10_000),
            150_000
        );
    }

    #[test]
    fn both_knobs_zero_is_a_free_pass_through() {
        assert_eq!(compute_fee_atomic(5_000, 0, 0), 0);
        // Only one knob at zero still charges the other.
        assert_eq!(compute_fee_atomic(5_000, 0, 10_000), 10_000);
        assert_eq!(compute_fee_atomic(1_000_000, DEFAULT_FEE_BPS, 0), 150_000);
    }

    #[test]
    fn fee_math_does_not_overflow_on_absurd_inputs() {
        assert_eq!(compute_fee_atomic(u128::MAX, u32::MAX, 0), u128::MAX);
        assert_eq!(compute_fee_atomic(u128::MAX, 0, 0), 0);
        assert_eq!(compute_fee_atomic(0, DEFAULT_FEE_BPS, 0), 0);
    }

    /// Odd amounts round UP to the atomic unit, exactly as `fees::quote` does:
    /// one rule, so the receipt this route writes and the quote the catalogue
    /// shows never differ by a unit.
    #[test]
    fn the_fee_rounds_up_and_matches_the_hybrid_quote() {
        for quoted in [1u128, 5, 15, 99, 1_234_567, 100_001, 5_000, 1_000_000] {
            let fee = compute_fee_atomic(quoted, DEFAULT_FEE_BPS, DEFAULT_FEE_MIN_ATOMIC);
            let expected =
                crate::fees::quote(quoted as i128, crate::fees::ProviderClass::Federated);
            assert_eq!(fee as i128, expected.buyer_markup_atomic, "fee of {quoted}");
            assert_eq!(
                (quoted + fee) as i128,
                expected.buyer_total_atomic,
                "total of {quoted}"
            );
        }
        assert_eq!(compute_fee_atomic(5, DEFAULT_FEE_BPS, 0), 1);
        assert_eq!(compute_fee_atomic(15, DEFAULT_FEE_BPS, 0), 3);
        assert_eq!(compute_fee_atomic(1_234_567, DEFAULT_FEE_BPS, 0), 185_186);
    }

    /// The ledger's replay classification: only a finished row replays its
    /// outcome; a row still `quoted` is a request in flight and must answer 409.
    #[test]
    fn replay_distinguishes_finished_from_in_flight_rows() {
        let finished = ExistingFetch {
            status: "debited".into(),
            quoted_price_usdc: BigDecimal::from_str("0.10").unwrap(),
            fee_usdc: BigDecimal::from_str("0.01").unwrap(),
            external_tx_hash: None,
            internal_debit_tx_hash: None,
            refusal_code: None,
        };
        assert_eq!(replay_response(finished).status(), StatusCode::OK);

        let in_flight = ExistingFetch {
            status: "quoted".into(),
            quoted_price_usdc: BigDecimal::from_str("0.10").unwrap(),
            fee_usdc: BigDecimal::from_str("0.01").unwrap(),
            external_tx_hash: None,
            internal_debit_tx_hash: None,
            refusal_code: None,
        };
        assert_eq!(replay_response(in_flight).status(), StatusCode::CONFLICT);

        let refused = ExistingFetch {
            status: "refused".into(),
            quoted_price_usdc: BigDecimal::from_str("0.10").unwrap(),
            fee_usdc: BigDecimal::from_str("0.01").unwrap(),
            external_tx_hash: None,
            internal_debit_tx_hash: None,
            refusal_code: Some("upstream_declined".into()),
        };
        assert_eq!(
            replay_response(refused).status(),
            StatusCode::BAD_GATEWAY,
            "a provider refusal replays under the status it was first answered with"
        );
    }

    /* -----------------------------------------------------------------------
    A 402 answering a PAID request

    This was the failure with no evidence behind it: the gateway read the
    status, threw the body away, and reported "provider returned HTTP 402 to
    the paid request" — a sentence that fits half a dozen different bugs
    with half a dozen different fixes. These tests pin the two things that
    now come back with it.
    -------------------------------------------------------------------- */

    fn offer_json(amount: &str, pay_to: &str, version: u64) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "x402Version": version,
            "accepts": [{
                "scheme": "exact",
                "network": "base",
                "maxAmountRequired": amount,
                "payTo": pay_to,
                "asset": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
                "maxTimeoutSeconds": 300,
                "extra": { "name": "USD Coin", "version": "2" }
            }]
        }))
        .unwrap()
    }

    const PAY_TO: &str = "0x1111111111111111111111111111111111111111";

    fn signed_requirement() -> super::super::resell::UpstreamRequirement {
        super::super::resell::parse_upstream_402(&offer_json("4000", PAY_TO, 1), &[BASE_CHAIN_ID])
            .expect("the offer under test must parse")
    }

    #[test]
    fn a_refusal_carries_the_providers_own_words_out_of_a_json_body() {
        let raw = br#"{"error":"insufficient_funds","message":"payer has no USDC"}"#;
        assert_eq!(
            decline_reason(raw).as_deref(),
            Some("insufficient_funds: payer has no USDC"),
            "both halves of a provider's error object are worth repeating, in order"
        );
    }

    #[test]
    fn a_refusal_with_no_json_still_says_what_arrived() {
        assert_eq!(
            decline_reason(b"  Payment  rejected\n").as_deref(),
            Some("Payment rejected"),
        );
        assert_eq!(
            decline_reason(b"   ").as_deref(),
            None,
            "an empty body is not a reason"
        );
        assert_eq!(decline_reason(b"").as_deref(), None);
    }

    #[test]
    fn a_long_refusal_is_cut_rather_than_forwarded_whole() {
        let long = "x".repeat(5_000).into_bytes();
        let reason = decline_reason(&long).expect("a long body still yields a reason");
        assert_eq!(reason.chars().count(), MAX_DECLINE_CHARS);
    }

    #[test]
    fn a_requote_names_the_field_that_moved() {
        let signed = signed_requirement();
        // The provider now wants more than it quoted when we asked. That single
        // difference is the whole failure, and it used to be invisible.
        let change = describe_offer_change(&offer_json("5000", PAY_TO, 1), None, &signed)
            .expect("a moved amount is a change");
        assert!(
            change.contains("amount 4000 → 5000"),
            "expected the amounts side by side, got: {change}"
        );
    }

    #[test]
    fn a_requote_names_a_dialect_the_proof_answered_in_the_wrong_shape() {
        let signed = signed_requirement();
        let change = describe_offer_change(&offer_json("4000", PAY_TO, 2), None, &signed)
            .expect("a moved protocol version is a change");
        assert!(
            change.contains("x402Version 1 → 2"),
            "expected the versions side by side, got: {change}"
        );
    }

    #[test]
    fn a_requote_names_a_payto_that_rotated_under_us() {
        let signed = signed_requirement();
        let rotated = "0x2222222222222222222222222222222222222222";
        let change = describe_offer_change(&offer_json("4000", rotated, 1), None, &signed)
            .expect("a moved payTo is a change");
        assert!(
            change.contains("payTo"),
            "expected payTo named, got: {change}"
        );
    }

    /* The negative is as useful as the positives: an identical re-quote rules
    out this entire family of causes and points at the signature, the nonce
    or the payer's balance instead. Saying "nothing changed" is a finding,
    and inventing a difference would send somebody after the wrong bug. */
    #[test]
    fn an_identical_requote_reports_no_change_at_all() {
        let signed = signed_requirement();
        assert_eq!(
            describe_offer_change(&offer_json("4000", PAY_TO, 1), None, &signed),
            None,
        );
    }

    #[test]
    fn an_unreadable_second_offer_is_not_reported_as_a_difference() {
        let signed = signed_requirement();
        assert_eq!(
            describe_offer_change(b"<html>nope</html>", None, &signed),
            None
        );
    }
}
