use super::*;
use crate::db::{AgentConnectToken, AgentConnectTokenLookup, AgentConnectTokenWallet};

pub(super) fn reqwest_method(method: &Method) -> reqwest::Method {
    match *method {
        Method::GET => reqwest::Method::GET,
        Method::POST => reqwest::Method::POST,
        Method::PUT => reqwest::Method::PUT,
        Method::DELETE => reqwest::Method::DELETE,
        Method::PATCH => reqwest::Method::PATCH,
        Method::HEAD => reqwest::Method::HEAD,
        Method::OPTIONS => reqwest::Method::OPTIONS,
        _ => reqwest::Method::GET,
    }
}

/// Resolved connect token + the rails it authorizes (0..N wallets and/or SEPA).
/// Carries no per-call state — wallet limit checks happen later, once the payer
/// is known (from the x402 proof) or a single wallet is being prepaid.
#[derive(Debug, Clone)]
pub(super) struct ConnectTokenContext {
    pub(super) token: AgentConnectToken,
}

impl ConnectTokenContext {
    pub(super) fn token_id(&self) -> Uuid {
        self.token.token_id
    }

    pub(super) fn user_id(&self) -> Uuid {
        self.token.user_id
    }

    pub(super) fn has_wallets(&self) -> bool {
        !self.token.wallets.is_empty()
    }

    /// Destination hosts this connection may spend against. Empty =
    /// unrestricted. See `super::domain_policy`.
    pub(super) fn allowed_domains(&self) -> &[String] {
        &self.token.allowed_domains
    }

    pub(super) fn sole_wallet(&self) -> Option<&AgentConnectTokenWallet> {
        if self.token.wallets.len() == 1 {
            self.token.wallets.first()
        } else {
            None
        }
    }
}

/// Look up the connect token from the request headers. Returns `Ok(None)` when
/// no connect-token header is present; an error response when a header is
/// present but the token is invalid / expired / revoked.
pub(super) async fn resolve_connect_token(
    state: &Arc<AppState>,
    headers: &HeaderMap<HeaderValue>,
) -> Result<Option<ConnectTokenContext>, Response<Body>> {
    // Apiosk's own surfaces spend from the treasury with a service key, which
    // the gateway resolves to the treasury's connect token itself — see
    // `crate::service_auth`. Checked first: a request carrying one is ours.
    match crate::service_auth::resolve(state, headers).await {
        crate::service_auth::ServiceAuth::NotPresented => {}
        crate::service_auth::ServiceAuth::Rejected(code, message) => {
            return Err(json_error_response(StatusCode::UNAUTHORIZED, code, message));
        }
        crate::service_auth::ServiceAuth::Resolved(lookup) => {
            return context_from_lookup(lookup);
        }
    }

    // Provider SDK key (sk_live_…) takes the same wallet-settlement path as a
    // connect token: validate it, resolve the owner's wallets, and hand back an
    // equivalent context. Checked first; it never collides with aw_ tokens.
    if let Some(api_key) = extract_provider_api_key(headers) {
        return match state.db.lookup_provider_api_key_context(&api_key).await {
            Ok(Some(token)) => Ok(Some(ConnectTokenContext { token })),
            Ok(None) => Err(json_error_response(
                StatusCode::UNAUTHORIZED,
                "provider_api_key_invalid",
                "Provider API key is not recognized or has been revoked. Create or rotate a key in the provider portal (Settings → SDK & API keys) and set APIOSK_API_KEY.",
            )),
            Err(e) => {
                error!("Failed to validate provider API key: {}", e);
                Err(json_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "provider_api_key_lookup_failed",
                    "Failed to validate the provider API key.",
                ))
            }
        };
    }

    let Some(token) = extract_agent_connect_token(headers) else {
        return Ok(None);
    };
    let token_hash = hash_connect_token(&token);

    let lookup = state
        .db
        .lookup_agent_connect_token_by_hash(&token_hash)
        .await
        .map_err(|e| {
            error!("Failed to validate agent connect token: {}", e);
            json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "agent_wallet_lookup_failed",
                "Failed to validate agent wallet token.",
            )
        })?;
    context_from_lookup(lookup)
}

/// A lookup verdict as the payment path reads it: an active token is a
/// context, anything else is the 401 that names why.
#[allow(clippy::result_large_err)]
fn context_from_lookup(
    lookup: AgentConnectTokenLookup,
) -> Result<Option<ConnectTokenContext>, Response<Body>> {
    match lookup {
        AgentConnectTokenLookup::Active(token) => Ok(Some(ConnectTokenContext { token })),
        AgentConnectTokenLookup::Unknown => Err(json_error_response(
            StatusCode::UNAUTHORIZED,
            "agent_wallet_token_unknown",
            "Agent wallet token is not recognized. The token sent on this request does not match any token issued by the dashboard. Verify APIOSK_CONNECT_TOKEN (or your saved MCP config) matches the token shown in the dashboard's agent-wallet page exactly, with no whitespace, truncation, or stale prefix.",
        )),
        AgentConnectTokenLookup::Revoked { token_prefix } => Err(json_error_response(
            StatusCode::UNAUTHORIZED,
            "agent_wallet_token_revoked",
            &format!(
                "Agent wallet token {token_prefix}… has been revoked. Issue a new connect token in the dashboard and update APIOSK_CONNECT_TOKEN (or your saved MCP config)."
            ),
        )),
        AgentConnectTokenLookup::Expired {
            token_prefix,
            expires_at,
        } => Err(json_error_response(
            StatusCode::UNAUTHORIZED,
            "agent_wallet_token_expired",
            &format!(
                "Agent wallet token {token_prefix}… expired at {expires_at}. Issue a new connect token in the dashboard and update APIOSK_CONNECT_TOKEN (or your saved MCP config)."
            ),
        )),
    }
}

/// Authorize the x402 payer against the connect token's wallet allowlist and
/// enforce that wallet's per-tx + daily USDC caps. Returns the chosen
/// `AgentWalletSession` so the caller can record settlement against it.
pub(super) async fn authorize_wallet_payment(
    state: &Arc<AppState>,
    ctx: &ConnectTokenContext,
    payer: &str,
    call_amount: &BigDecimal,
) -> Result<AgentWalletSession, Response<Body>> {
    let Some(wallet) = ctx.token.find_wallet(payer) else {
        return Err(json_error_response(
            StatusCode::FORBIDDEN,
            "agent_wallet_not_authorized",
            "Payer wallet is not authorized by this connect token.",
        ));
    };

    enforce_wallet_caps(state, wallet, call_amount).await?;

    Ok(AgentWalletSession {
        token_id: ctx.token_id(),
        wallet_id: wallet.wallet_id,
        wallet_address: wallet.wallet_address.clone(),
    })
}

/// Authorize a prepaid (no-x402-proof) call by walking the connect token's
/// authorized wallets in order and picking the first one that (a) passes the
/// per-tx + daily caps, and (b) is currently fundable on-chain.
///
/// Multi-wallet tokens are supported: if the first authorized wallet is
/// unfunded but a later one is fundable, we use the funded one. If none are
/// fundable, the most-permissive verdict (`Unknown` over `NotFundable`) is
/// returned so the caller can fall back to SEPA / credits before declaring
/// the buyer broke.
///
/// Caps errors (per-tx limit, daily limit, inactive wallet) only surface when
/// *every* authorized wallet trips a caps check — i.e. no wallet on the token
/// is eligible for the call amount.
pub(super) async fn select_prepaid_wallet_with_fundability(
    state: &Arc<AppState>,
    ctx: &ConnectTokenContext,
    call_amount: &BigDecimal,
) -> Result<(AgentWalletSession, WalletFundability), Response<Body>> {
    if !ctx.has_wallets() {
        return Err(json_error_response(
            StatusCode::FORBIDDEN,
            "agent_wallet_prepaid_requires_wallet",
            "Prepaid managed-wallet calls require a connect token authorizing at least one wallet.",
        ));
    }

    let mut last_caps_err: Option<Response<Body>> = None;
    let mut not_fundable_candidate: Option<AgentWalletSession> = None;
    let mut unknown_candidate: Option<AgentWalletSession> = None;

    for wallet in &ctx.token.wallets {
        if let Err(e) = enforce_wallet_caps(state, wallet, call_amount).await {
            last_caps_err = Some(e);
            continue;
        }

        let session = AgentWalletSession {
            token_id: ctx.token_id(),
            wallet_id: wallet.wallet_id,
            wallet_address: wallet.wallet_address.clone(),
        };

        match check_agent_wallet_prepaid_fundability(state, &session).await? {
            WalletFundability::Fundable => {
                return Ok((session, WalletFundability::Fundable));
            }
            WalletFundability::Unknown => {
                if unknown_candidate.is_none() {
                    unknown_candidate = Some(session);
                }
            }
            WalletFundability::NotFundable => {
                if not_fundable_candidate.is_none() {
                    not_fundable_candidate = Some(session);
                }
            }
        }
    }

    if let Some(session) = unknown_candidate {
        return Ok((session, WalletFundability::Unknown));
    }
    if let Some(session) = not_fundable_candidate {
        return Ok((session, WalletFundability::NotFundable));
    }

    Err(last_caps_err.unwrap_or_else(|| {
        json_error_response(
            StatusCode::FORBIDDEN,
            "agent_wallet_no_eligible_wallet",
            "No wallet on this connect token is eligible for the call.",
        )
    }))
}

pub(super) async fn enforce_wallet_caps(
    state: &Arc<AppState>,
    wallet: &AgentConnectTokenWallet,
    call_amount: &BigDecimal,
) -> Result<(), Response<Body>> {
    if wallet.wallet_status != "active" {
        return Err(json_error_response(
            StatusCode::FORBIDDEN,
            "agent_wallet_inactive",
            "Connected agent wallet is not active.",
        ));
    }

    if call_amount > &wallet.per_tx_limit_usdc {
        return Err(json_error_response(
            StatusCode::FORBIDDEN,
            "agent_wallet_per_tx_limit_exceeded",
            "Call amount exceeds wallet per-transaction limit.",
        ));
    }

    let spent_today = state
        .db
        .get_wallet_paid_spend_today(&wallet.wallet_address)
        .await
        .map_err(|e| {
            error!("Failed to read agent wallet spend for limit check: {}", e);
            json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "agent_wallet_limit_check_failed",
                "Failed to validate wallet spending limits.",
            )
        })?;
    if &spent_today + call_amount > wallet.daily_limit_usdc {
        return Err(json_error_response(
            StatusCode::FORBIDDEN,
            "agent_wallet_daily_limit_exceeded",
            "Wallet daily spending limit reached.",
        ));
    }

    Ok(())
}

// `authorize_sepa_charge` used to live here: the euro cap check that gated
// the SEPA-incasso rail (per-transaction and per-day limits on the connect
// token, against today's settled euro spend). It compared a EUR cap against
// an amount that came off the USDC price, and the rail it guarded is gone —
// the buyer's Apiosk balance is debited in the App's Supabase and the
// treasury pays the provider over x402.

/// Outcome of checking whether the managed wallet can fund the call.
///
/// `NotFundable` and `Unknown` are returned without an HTTP error so callers
/// can decide whether to fall back to another rail (credits) before
/// surfacing 402/502. `Unknown` means the on-chain settlement RPC could not
/// answer (rate-limit, network blip, etc.) after retries — the wallet may
/// actually have funds but we can't prove it right now.
pub(super) enum WalletFundability {
    Fundable,
    NotFundable,
    Unknown,
}

pub(super) fn agent_wallet_insufficient_funds_response() -> Response<Body> {
    json_error_response(
        StatusCode::PAYMENT_REQUIRED,
        "agent_wallet_insufficient_funds",
        "Managed wallet has insufficient USDC balance or allowance for this call.",
    )
}

pub(super) fn agent_wallet_settlement_check_failed_response() -> Response<Body> {
    json_error_response(
        StatusCode::BAD_GATEWAY,
        "agent_wallet_settlement_check_failed",
        "Failed to verify managed wallet balance and allowance.",
    )
}

/// Can this managed wallet carry the call?
///
/// It used to take the call amount too, because the answer came from the chain:
/// enough USDC, and a big enough allowance to the settlement contract, for THIS
/// amount. The only wallet Apiosk spends from now is the treasury, and whether
/// it can pay was settled before the request arrived — when the buyer's Apiosk
/// balance was debited in the App's Supabase — so the amount no longer changes
/// the answer and is no longer asked for. Per-transaction and daily caps are
/// still enforced by `authorize_wallet_charge`, which is a different question.
pub(super) async fn check_agent_wallet_prepaid_fundability(
    state: &Arc<AppState>,
    session: &AgentWalletSession,
) -> Result<WalletFundability, Response<Body>> {
    // The treasury is not a buyer wallet with an allowance to check: it is
    // Apiosk paying for a call whose buyer has already been debited. Asking
    // the settlement contract whether Apiosk can pay Apiosk is what turned a
    // missing settlement client into a 503 on every research step.
    if state.is_treasury_wallet(&session.wallet_address) {
        return Ok(WalletFundability::Fundable);
    }
    // Any other wallet is a BUYER wallet, and a buyer wallet used to be checked
    // against the chain here: `SettlementClient::can_settle` asked whether it
    // held enough USDC and had granted the ApioskSettlement contract a large
    // enough allowance, returning NotFundable on a clear no and Unknown when
    // the RPC was degraded. That whole rail is gone — a buyer never connects a
    // wallet and never pays a provider directly. Apiosk pays from treasury with
    // the buyer's Apiosk balance already debited in the App's Supabase, so a
    // non-treasury wallet has no way to fund a call and is refused outright.
    // This is exactly what a gateway with no DIRECT_SETTLEMENT_* configured has
    // always done; it is now the only behaviour.
    warn!(
        "Rejecting prepaid managed-wallet request: {} is not the treasury, and buyer wallets are no longer a payment rail",
        session.wallet_address
    );
    Err(json_error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "agent_wallet_prepaid_unavailable",
        "Managed wallet prepaid calls are only served from the Apiosk treasury; a buyer wallet is no longer a payment rail.",
    ))
}

pub fn hash_connect_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

pub(super) fn wallets_match(left: &str, right: &str) -> bool {
    normalize_wallet_like(left)
        .zip(normalize_wallet_like(right))
        .map(|(a, b)| a == b)
        .unwrap_or(false)
}

pub(super) fn normalize_wallet_like(wallet: &str) -> Option<String> {
    let trimmed = wallet.trim();
    if trimmed.len() != 42 || !trimmed.starts_with("0x") {
        return None;
    }
    if !trimmed.chars().skip(2).all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(trimmed.to_ascii_lowercase())
}

pub(super) fn build_forwarded_request(
    http_client: &reqwest::Client,
    method: &Method,
    origin_url: &str,
    headers: &HeaderMap,
    body_bytes: &[u8],
    preset_auth_headers: &[(String, String)],
    idempotency_key: Option<&str>,
) -> reqwest::RequestBuilder {
    let mut request = http_client
        .request(reqwest_method(method), origin_url)
        .body(body_bytes.to_vec());

    let mut caller_sent_idempotency_key = false;

    for (key, value) in headers.iter() {
        let key_lower = key.as_str().to_ascii_lowercase();
        if should_strip_forwarded_header(&key_lower, value) {
            continue;
        }
        if key_lower == "idempotency-key" {
            caller_sent_idempotency_key = true;
        }
        if let Ok(header_name) = reqwest::header::HeaderName::from_bytes(key.as_str().as_bytes()) {
            if let Ok(header_value) = reqwest::header::HeaderValue::from_bytes(value.as_bytes()) {
                request = request.header(header_name, header_value);
            }
        }
    }

    for (name, value) in preset_auth_headers {
        request = request.header(name, value);
    }

    // Inject a stable Idempotency-Key derived from this gateway request_id so
    // upstreams (Mollie, Sendcloud, Stripe-style APIs) can dedupe retries.
    // Only set it when the caller did not already supply one.
    if !caller_sent_idempotency_key {
        if let Some(key) = idempotency_key {
            if let Ok(header_value) = reqwest::header::HeaderValue::from_str(key) {
                request = request.header("Idempotency-Key", header_value);
            }
        }
    }

    request
}

/// True for HTTP methods where re-sending the same request after a transport
/// error is safe even without upstream idempotency support. Used to guard the
/// retry loop in `forward.rs` so we don't accidentally double-execute a POST.
pub(super) fn is_idempotent_method(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::PUT | Method::DELETE | Method::TRACE
    )
}

/// Compute the HMAC signature for an outbound proxy request, using the
/// canonical `method.path.sha256(body)` payload format. Returned as a
/// lowercase hex digest. `secret` is the plaintext shared secret; the
/// caller is responsible for sourcing it (decrypted from the DB) and
/// must NOT log or persist it.
pub(super) fn compute_hmac_sha256_signature(
    method: &Method,
    upstream_path: &str,
    body: &[u8],
    secret: &str,
) -> String {
    use ring::hmac;
    use sha2::{Digest, Sha256};

    let body_digest = Sha256::digest(body);
    let mut body_hex = String::with_capacity(body_digest.len() * 2);
    for byte in body_digest {
        body_hex.push_str(&format!("{:02x}", byte));
    }
    let canonical = format!("{}.{}.{}", method.as_str(), upstream_path, body_hex);

    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    let tag = hmac::sign(&key, canonical.as_bytes());
    let mut sig_hex = String::with_capacity(tag.as_ref().len() * 2);
    for byte in tag.as_ref() {
        sig_hex.push_str(&format!("{:02x}", byte));
    }
    sig_hex
}

/// Convert a DispatchResult into an Axum Response
pub(super) fn dispatch_result_to_response(result: DispatchResult) -> Response<Body> {
    let mut response = Response::new(Body::from(result.body));
    *response.status_mut() =
        StatusCode::from_u16(result.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let content_type = HeaderValue::from_str(&result.content_type)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, content_type);
    for (k, v) in result.extra_headers {
        if let (Ok(hk), Ok(hv)) = (
            header::HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(&v),
        ) {
            response.headers_mut().insert(hk, hv);
        }
    }

    response
}

/// Whether to settle a verified-but-deferred API payment, decided by the
/// upstream response. We settle ONLY on a successful (2xx) upstream response:
/// a buyer must never be charged when the provider returned an error. This
/// matches `should_charge_credits_after_upstream`, so the x402 and credits
/// rails charge on exactly the same condition.
///
/// Previously this was `!status.is_server_error()`, which charged the buyer on
/// every non-5xx response — including 400/401/403/404/422. That meant a paid
/// call to a misconfigured or unprovisioned upstream (e.g. a Google Maps API
/// not enabled on the project → 403) still took the buyer's money for an error.
///
/// NOTE: this gate is HTTP-status based. A provider that returns HTTP 200 with
/// an error in the body (e.g. Google Maps `status: REQUEST_DENIED`) will still
/// settle — the gateway is a transparent proxy and does not parse upstream
/// bodies. Such providers must be fixed upstream or delisted; the status gate
/// cannot catch body-level errors.
pub(super) fn should_settle_deferred_api_payment(status: reqwest::StatusCode) -> bool {
    status.is_success()
}

pub(super) fn should_charge_credits_after_upstream(status: reqwest::StatusCode) -> bool {
    status.is_success()
}
