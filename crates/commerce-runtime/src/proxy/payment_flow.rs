use super::*;
use crate::cache::CacheKey;
use crate::payment::{is_cdp_facilitator_url, BazaarMetadata};

#[derive(Debug, Clone, Copy)]
/// Which rails a listing may be paid on.
///
/// There used to be a third member here, `fiat`: SEPA incasso through Mollie,
/// with a euro mandate the buyer signed in the portal. It is gone along with
/// the rest of the euro rail — Apiosk now debits the buyer's Apiosk balance in
/// the App's Supabase and pays the provider from treasury over x402, and that
/// is the only way money moves.
pub(super) struct AllowedApiPaymentRails {
    pub(super) credits: bool,
    pub(super) x402: bool,
}

fn check_approved_ceiling(raw: Option<&HeaderValue>, actual: i128) -> Result<(), &'static str> {
    let Some(raw) = raw else {
        return Ok(());
    };
    let ceiling = raw
        .to_str()
        .ok()
        .and_then(crate::job::attempt::settled_atomic)
        .ok_or("invalid_price_ceiling")?;
    if actual > ceiling {
        Err("approved_price_exceeded")
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod clockwork_ceiling_tests {
    use super::*;
    #[test]
    fn compares_final_buyer_total_including_markup_before_payment() {
        let quoted =
            crate::fees::quote(18_903, crate::fees::ProviderClass::Managed).buyer_total_atomic;
        assert_eq!(
            check_approved_ceiling(
                Some(&HeaderValue::from_str(&quoted.to_string()).unwrap()),
                quoted
            ),
            Ok(())
        );
        assert_eq!(
            check_approved_ceiling(Some(&HeaderValue::from_static("18903")), quoted),
            Err("approved_price_exceeded")
        );
        for bad in ["-1", "NaN", "", "0.02", "9223372036854775808"] {
            assert_eq!(
                check_approved_ceiling(Some(&HeaderValue::from_str(bad).unwrap()), quoted),
                Err("invalid_price_ceiling")
            );
        }
    }
}

impl AllowedApiPaymentRails {
    #[cfg(test)]
    pub(crate) fn new(credits: bool, x402: bool) -> Self {
        Self { credits, x402 }
    }

    pub(super) fn from_resolved(resolved: &ResolvedProxyRequest) -> Self {
        // Credits are no longer offered as a buyer payment rail, and SEPA
        // incasso no longer exists at all. x402 (USDC on the endpoint's
        // advertised payment networks) is the only rail advertised to
        // users/agents, so we collapse the *offered* rails to x402 here — the
        // single chokepoint every downstream surface (402 responses, checkout
        // pages, labels, metadata) reads from. The full per-listing rail
        // resolution is preserved in `configured_from_resolved`, and the credit
        // charge + settlement logic elsewhere is left untouched; it is simply
        // never selected.
        let _ = resolved;
        Self {
            credits: false,
            x402: true,
        }
    }

    /// Original per-listing payment-rail resolution. Retained (but no longer
    /// consulted by `from_resolved`) so the credits rail can be re-enabled
    /// without reconstructing this logic. Its `fiat`/`sepa` arms are gone with
    /// the euro rail and are not coming back.
    #[allow(dead_code)]
    fn configured_from_resolved(resolved: &ResolvedProxyRequest) -> Self {
        if resolved.listing_type != ListingType::Api {
            return Self {
                credits: false,
                x402: true,
            };
        }

        let Some(listing) = resolved.listing.as_ref() else {
            return Self {
                credits: true,
                x402: true,
            };
        };

        let Some(rails) = listing
            .listing_metadata
            .get("supported_payment_rails")
            .and_then(serde_json::Value::as_array)
        else {
            // Listings created by the provider-portal carry an empty
            // listing_metadata (apis-db.ts never writes supported_payment_rails),
            // so an absent rail set must mean "all rails". Matches the
            // no-listing branch above (credits + x402).
            return Self {
                credits: true,
                x402: true,
            };
        };

        let mut allowed = Self {
            credits: false,
            x402: false,
        };

        for rail in rails {
            match rail.as_str() {
                Some("credits") => allowed.credits = true,
                Some("x402") => allowed.x402 = true,
                // "fiat" / "sepa" / "sepa_incasso" / "mollie_sepa" used to set a
                // third flag here. A listing that still names one of them is now
                // simply a listing with no rail of its own, and falls through to
                // the x402 default below.
                _ => {}
            }
        }

        if allowed.credits || allowed.x402 {
            allowed
        } else {
            Self {
                credits: false,
                x402: true,
            }
        }
    }

    fn payment_rails_label(&self) -> String {
        match (self.credits, self.x402) {
            (true, true) => "x402 or credits".to_string(),
            (true, false) => "credits only".to_string(),
            (false, true) => "x402 only".to_string(),
            (false, false) => "not configured".to_string(),
        }
    }
}

/// Canonical mainnet USDC contract/mint per supported x402 network — the
/// single asset family the gateway accepts for paid API calls.
fn canonical_usdc_contract(network_name: &str) -> Option<&'static str> {
    match network_name {
        "base" => Some("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"),
        "polygon" => Some("0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359"),
        "arbitrum" => Some("0xaf88d065e77c8cC2239327C5EDb3A432268e5831"),
        _ => None,
    }
}

/// Whether `nt` is the canonical mainnet USDC token for a supported x402
/// network. Pins both the symbol AND the canonical contract address (compared
/// case-insensitively, since EVM rows may be stored checksummed or
/// lower-cased) so a mislabelled DB row can't slip a different asset through.
/// Network allowlist/testnet are validated separately at the call site for
/// distinct error reporting.
fn is_accepted_usdc_token(nt: &crate::db::NetworkToken) -> bool {
    nt.token_symbol == "USDC"
        && canonical_usdc_contract(&nt.network_name)
            .is_some_and(|c| nt.contract_address.eq_ignore_ascii_case(c))
}

/// One payable network for a request: the buyer-facing requirement plus the
/// provider's payout destination on that chain when one is configured
/// (consumed at settlement time).
#[derive(Debug, Clone)]
pub(super) struct X402Offer {
    pub(super) token: crate::db::NetworkToken,
    pub(super) requirement: PaymentRequirement,
    pub(super) payout_wallet: Option<String>,
}

/// Assemble the ordered offer set for an API endpoint: primary network first
/// (the endpoint's `network_token_id`), then Base (the always-available
/// fallback backed by `apis.wallet_address`), then any other network with a
/// configured `api_payout_wallets` row. A non-Base network is only offered
/// when the facilitator supports it on x402 v1 AND the API has opted into that
/// network. Guaranteed non-empty: Base always qualifies.
async fn build_x402_offers(
    state: &Arc<AppState>,
    api: &crate::db::Api,
    primary_token: &crate::db::NetworkToken,
    price: &bigdecimal::BigDecimal,
    resource_url: &str,
    description: &str,
    api_slug: &str,
) -> Result<Vec<X402Offer>, Response<Body>> {
    let caps = state
        .facilitator_caps
        .get(&state.http_client, &state.config, &state.facilitator_url)
        .await;

    // Configured per-network payout destinations (max 4 rows by schema).
    let configured: Vec<(String, String)> = state
        .db
        .get_api_payout_wallets(api.id)
        .await
        .unwrap_or_else(|e| {
            warn!("get_api_payout_wallets failed for {}: {}", api_slug, e);
            Vec::new()
        });
    let wallet_for = |network: &str| -> Option<String> {
        configured
            .iter()
            .find(|(n, _)| n == network)
            .map(|(_, w)| w.clone())
    };

    // All allowlisted USDC tokens, so alternates resolve without extra queries.
    let tokens = state
        .db
        .list_x402_usdc_network_tokens()
        .await
        .unwrap_or_else(|e| {
            warn!("list_x402_usdc_network_tokens failed: {}", e);
            Vec::new()
        });
    let token_for = |network: &str| -> Option<&crate::db::NetworkToken> {
        tokens
            .iter()
            .find(|t| t.network_name == network && is_accepted_usdc_token(t))
    };

    // Candidate order: primary, base, then remaining configured networks.
    let mut candidates: Vec<String> = vec![primary_token.network_name.clone()];
    if !candidates.contains(&"base".to_string()) {
        candidates.push("base".to_string());
    }
    for (network, _) in &configured {
        if !candidates.contains(network) {
            candidates.push(network.clone());
        }
    }

    let mut offers: Vec<X402Offer> = Vec::new();
    for network in candidates {
        let token = if network == primary_token.network_name {
            primary_token.clone()
        } else {
            match token_for(&network) {
                Some(t) => t.clone(),
                None => continue,
            }
        };

        // Payout destination on this chain. Base falls back to the API's
        // primary wallet.
        let payout_wallet = if api.apiosk_revenue {
            None
        } else {
            match wallet_for(&network) {
                Some(w) => Some(w),
                None if network == "base" => Some(api.wallet_address.trim().to_string()),
                // Primary non-base network without a wallet row: legacy 047-style
                // config (payout_network set before 048). The API wallet is the
                // provider's chosen destination on that chain too.
                None if network == primary_token.network_name => {
                    Some(api.wallet_address.trim().to_string())
                }
                None => continue,
            }
        };
        if payout_wallet
            .as_deref()
            .is_some_and(|wallet| wallet.trim().is_empty())
        {
            continue;
        }

        // Facilitator capability + platform payTo per network.
        let pay_to = if network == "base" {
            state.config.platform_wallet.clone()
        } else {
            let Some(_) = caps.support_for(&network) else {
                warn!(
                    "x402 network '{}' skipped for {}: facilitator does not support it on v1",
                    network, api_slug
                );
                continue;
            };
            state.config.platform_wallet.clone()
        };

        offers.push(X402Offer {
            requirement: PaymentRequirement::for_network(
                &pay_to,
                price,
                resource_url,
                description,
                &token,
            ),
            token,
            payout_wallet,
        });
    }

    if offers.is_empty() {
        // Base was skipped only if the API wallet itself is empty — a listing
        // that should never have been payable. Refuse rather than advertise a
        // requirement nobody can settle.
        error!(
            "No payable x402 network for {} (empty API wallet?)",
            api_slug
        );
        return Err(json_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "no_payable_network",
            "No payable x402 network is configured for this API",
        ));
    }

    Ok(offers)
}

/// Pick the offer matching the network named in the submitted payment proof.
/// Unknown/missing network → primary (offers[0]); verification then fails
/// naturally if the proof genuinely targets an unoffered network.
fn select_offer_for_proof<'a>(offers: &'a [X402Offer], proof: &str) -> &'a X402Offer {
    let network = crate::payment::decode_payment_payload(proof)
        .ok()
        .and_then(|payload| {
            // v1 payloads carry the chosen network at the top level (plain
            // name, e.g. "base"); v2 payloads carry it under `accepted.network`
            // as CAIP-2 (e.g. "eip155:8453"). Read whichever is present and
            // normalize to the v1 name the offer set is keyed on.
            payload
                .get("network")
                .or_else(|| payload.get("accepted").and_then(|a| a.get("network")))
                .and_then(|v| v.as_str())
                .map(crate::payment::v1_network_name)
        });
    match network {
        Some(name) => offers
            .iter()
            .find(|o| o.token.network_name == name)
            .unwrap_or(&offers[0]),
        None => &offers[0],
    }
}

/// Whether settlement for this listing must be DEFERRED until the upstream call
/// succeeds, rather than settled up front.
///
/// `Api` listings always defer: the buyer is charged only once the upstream
/// returned 2xx.
///
/// `Federated` listings must defer too, and the reason is load-bearing. A
/// federated call is served by the RESELL leg, which only runs when a deferred
/// payment is waiting to be settled (`forward.rs` gates on
/// `deferred_payment.is_some()`). Settling up front would:
///
///   * skip resell entirely, so the upstream 402 would pass straight back to a
///     buyer who has *already been charged*; and
///   * book the managed-style split, paying `apis.wallet_address` — which on a
///     federated listing is the PROVIDER'S OWN payTo — while the resell leg pays
///     them again inline. The provider is paid twice, out of Apiosk's margin.
///
/// Anything else (datasets, services, connectors, products) settles up front as
/// before; those are not proxied upstream calls.
fn settles_after_upstream(listing_type: &ListingType) -> bool {
    matches!(listing_type, ListingType::Api | ListingType::Federated)
}

pub(super) async fn enforce_endpoint_payment(
    state: &Arc<AppState>,
    api_slug: &str,
    requested_path: &str,
    headers: &HeaderMap<HeaderValue>,
    browser_mode: bool,
    tracking_context: &TrackingContext,
    resolved: &ResolvedProxyRequest,
) -> Result<EndpointPaymentDecision, Response<Body>> {
    let Some(ref ep) = resolved.endpoint else {
        return Ok(EndpointPaymentDecision::default());
    };

    let price_is_zero = ep.price == 0_i64;
    if !ep.payment_required || price_is_zero {
        return Ok(EndpointPaymentDecision::default());
    }

    if !settles_after_upstream(&resolved.listing_type) {
        return Err(json_error_response(
            StatusCode::BAD_REQUEST,
            "provider_x402_required",
            "This purchase needs an x402 API provider.",
        ));
    }

    // ── Hybrid fee model ────────────────────────────────────────────────────
    // `ep.price` is the PROVIDER'S LIST PRICE. Everything below this line
    // prices the BUYER, and the buyer always pays list + 15% — the one rule
    // that holds regardless of whether the listing is federated or managed.
    //
    // Every rail below (x402 wallet, prepaid managed wallet, SEPA, credits)
    // must charge the same number, so the quote is taken once here and each
    // rail is handed `buyer_total`. Passing `ep.price` to any of them would
    // charge the buyer the bare list price and leave the Apiosk fee unfunded.
    //
    // Caps and ceilings are therefore checked against the total that actually
    // leaves the wallet, not the list price: a buyer who caps a call at $0.10
    // is never debited $0.11.
    //
    // The one exception is the markup itself: a workflow run the App's service
    // key priced at list + 20% (`TrackingContext::buyer_markup_bps`) is quoted,
    // capped, policy-checked and approved at that total, so every check below
    // compares against what the App actually reserved.
    let fee_quote = crate::fees::quote_usdc_with_markup(
        &ep.price,
        crate::fees::ProviderClass::from_listing_type(resolved.api.listing_type.as_deref()),
        tracking_context.buyer_markup_bps(),
    );
    let buyer_total = fee_quote.buyer_total_usdc();
    // A plan's approved per-node ceiling is checked against the final price,
    // after parameter pricing and before any payment rail authorizes spending.
    if let Err(code) = check_approved_ceiling(
        headers.get("x-apiosk-max-buyer-total-atomic"),
        fee_quote.buyer_total_atomic,
    ) {
        return Err(json_error_response(
            if code == "invalid_price_ceiling" {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::CONFLICT
            },
            code,
            "The final buyer price must fit the valid approved atomic ceiling.",
        ));
    }

    let connect_token_ctx = resolve_connect_token(state, headers).await?;
    let connect_token_id = connect_token_ctx.as_ref().map(|c| c.token_id());
    // Buyer balances are authorized by the App; only its authenticated service
    // may request a treasury purchase. A caller-supplied wallet/proof is not a rail.
    if crate::service_auth::presented(headers).is_none()
        || connect_token_ctx.is_none()
        || extract_payment_proof(headers).is_some()
    {
        return Err(json_error_response(
            StatusCode::FORBIDDEN,
            "treasury_purchase_required",
            "Approve this purchase through your Apiosk balance.",
        ));
    }

    // Per-connection destination allowlist (migration 069). Checked here, once,
    // before any rail branches below it — so x402, SEPA, credits and the
    // checkout page are all covered by a single gate rather than three that can
    // drift apart. Only reached for paid endpoints (free ones returned above),
    // which is the intent: the list caps where money can go, not what can be
    // read.
    //
    // The host compared is the API's *upstream origin*, not gateway.apiosk.com
    // — a buyer allowlisting `api.openweathermap.org` means that provider, and
    // every Apiosk API shares the gateway hostname.
    if let Some(ref ctx) = connect_token_ctx {
        let allowed = ctx.allowed_domains();
        if !allowed.is_empty() {
            let destination = super::domain_policy::origin_host(&resolved.api.origin_url);
            let permitted = destination
                .as_deref()
                .is_some_and(|host| super::domain_policy::host_is_allowed(allowed, host));
            if !permitted {
                warn!(
                    "Connect token {} blocked paid call to {} (origin host {:?}) — not on its {}-entry allowlist",
                    ctx.token_id(),
                    api_slug,
                    destination,
                    allowed.len(),
                );
                return Err(json_error_response(
                    StatusCode::FORBIDDEN,
                    "domain_not_allowed",
                    &format!(
                        "This connection may not spend on `{}` ({}). Its allowlist is: {}. The buyer can change it in the Apiosk buyer portal under Connect → allowed domains.",
                        api_slug,
                        destination.as_deref().unwrap_or("upstream host unreadable"),
                        allowed.join(", "),
                    ),
                ));
            }
        }
    }
    // The App has already reserved the buyer's approved balance. A reviewed
    // ordinary API has no provider payment: do not build a USDC offer, select
    // a treasury wallet, or evaluate that wallet's spending policy.
    if resolved.uses_app_balance() {
        internal_revenue_receipt_id(&resolved.api, tracking_context)?;
        return Ok(EndpointPaymentDecision {
            deferred_payment: Some(DeferredApiPayment::AppBalance),
            agent_connect_token_id: connect_token_id,
            ..Default::default()
        });
    }

    if !connect_token_ctx.as_ref().is_some_and(|ctx| {
        ctx.token
            .wallets
            .iter()
            .any(|wallet| state.is_treasury_wallet(&wallet.wallet_address))
    }) {
        return Err(json_error_response(
            StatusCode::FORBIDDEN,
            "treasury_purchase_required",
            "Approve this purchase through your Apiosk balance.",
        ));
    }

    let network_token = if let Some(nt_id) = ep.network_token_id {
        match state.db.get_network_token(nt_id).await {
            Ok(Some(nt)) => nt,
            Ok(None) => {
                return Err(json_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "network_token_not_found",
                    "Endpoint network token configuration not found",
                ))
            }
            Err(e) => {
                error!("Failed to fetch network token: {}", e);
                return Err(json_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "network_token_fetch_failed",
                    "Failed to fetch endpoint network token",
                ));
            }
        }
    } else {
        match state.db.get_default_network_token().await {
            Ok(Some(nt)) => nt,
            Ok(None) => {
                return Err(json_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "default_network_token_not_found",
                    "Default network token configuration not found",
                ))
            }
            Err(e) => {
                error!("Failed to fetch default network token: {}", e);
                return Err(json_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "default_network_token_fetch_failed",
                    "Failed to fetch default network token",
                ));
            }
        }
    };

    if !crate::facilitator_caps::X402_V1_NETWORKS.contains(&network_token.network_name.as_str())
        || network_token.is_testnet
    {
        error!(
            "Rejected unsupported endpoint network token: id={}, network={}",
            network_token.id, network_token.network_name
        );
        return Err(json_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "invalid_network_token",
            "Endpoint must use a mainnet token on base, polygon, or arbitrum",
        ));
    }

    // USDC-only acceptance. The gateway settles paid API calls in USDC
    // exclusively, so reject any other token here — before a
    // PaymentRequirement is issued, verified, or settled. This is the
    // authoritative guard: the explicit `network_token_id` path
    // (`get_network_token`) is NOT symbol-filtered (only the default path is),
    // so a seeded non-USDC row (e.g. Base DAI) would otherwise pass.
    if !is_accepted_usdc_token(&network_token) {
        error!(
            "Rejected non-USDC endpoint token: id={}, symbol={}, contract={}",
            network_token.id, network_token.token_symbol, network_token.contract_address
        );
        return Err(json_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "invalid_network_token",
            "Endpoint must use the canonical USDC token for its network",
        ));
    }

    let resource_url = format!(
        "https://gateway.apiosk.com/{}/{}",
        api_slug,
        requested_path.trim_start_matches('/')
    );

    // Build the x402 offer set: one requirement per network this API can be
    // paid on (primary = the endpoint's configured network, plus Base and any
    // additional per-network payout wallets from `api_payout_wallets`, max 4
    // — one per chain). Each offer carries the provider's payout destination
    // for that chain so the settled network routes the 98% to the right
    // wallet. Networks the facilitator can't settle are silently dropped; Base
    // always survives.
    let offers = build_x402_offers(
        state,
        &resolved.api,
        &network_token,
        &buyer_total,
        &resource_url,
        &format!("API call to {}", resolved.api.slug),
        api_slug,
    )
    .await?;
    // Primary offer drives every single-network surface (checkout page,
    // hosted-wallet gating, deferred prepaid path) exactly as before.
    let network_token = offers[0].token.clone();
    let requirement = offers[0].requirement.clone();

    // The buyer's purchase policy, evaluated in the same place and for the same
    // reason as the allowlist above: once, before any rail branches, so x402,
    // SEPA, credits and the checkout page are covered by one gate rather than
    // four that drift apart.
    //
    // It can only TIGHTEN. The per-transaction and daily caps in `wallet.rs`
    // are untouched and still run underneath; a policy adds refusals and can
    // never permit a purchase those caps would have stopped. A connection with
    // no policy, or a database without migration 083, reaches settlement by
    // exactly the path it did before this existed.
    //
    // An approval the buyer already granted, if the agent is resuming one. It is
    // redeemed BEFORE evaluation, not instead of it: an approval is permission
    // for one purchase on the terms the buyer saw, so the rest of the policy is
    // still checked and a buyer who tightened a different rule in between meant
    // it. Redemption is a conditional UPDATE, so two simultaneous resumes
    // produce one purchase and one clean error.
    let mut approval_satisfied = false;
    if let Some(ref ctx) = connect_token_ctx {
        if let Some(approval_id) = super::preflight::approval_id_header(headers) {
            match state
                .db
                .redeem_purchase_approval(approval_id, Some(ctx.token_id()), &buyer_total)
                .await
            {
                Ok(Ok(approval)) => {
                    info!(
                        approval_id = %approval.id,
                        "redeemed an approved purchase for {api_slug}"
                    );
                    approval_satisfied = true;
                }
                Ok(Err(failure)) => {
                    warn!(
                        approval_id = %approval_id,
                        code = failure.code(),
                        "refused to redeem an approval for {api_slug}"
                    );
                    return Err(json_error_response(
                        StatusCode::PAYMENT_REQUIRED,
                        failure.code(),
                        &failure.message(),
                    ));
                }
                Err(err) => {
                    error!("could not redeem approval {approval_id}: {err}");
                    return Err(json_error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "approval_redeem_failed",
                        "Could not read that approval. Nothing was paid.",
                    ));
                }
            }
        }
    }

    if let Some(ref ctx) = connect_token_ctx {
        let held = crate::policy::enforce_policy(
            state,
            crate::policy::PurchaseContext {
                user_id: ctx.user_id(),
                token_id: Some(ctx.token_id()),
                wallet_id: ctx.sole_wallet().map(|wallet| wallet.wallet_id),
                wallet_address: ctx
                    .sole_wallet()
                    .map(|wallet| wallet.wallet_address.as_str()),
                amount_usd: &buyer_total,
                origin_url: &resolved.api.origin_url,
                api_slug,
                asset: Some("USDC"),
                network: Some(network_token.network_name.as_str()),
            },
        )
        .await?;

        // An already-approved purchase does not get held again for the same
        // reason the buyer just cleared. Every other verdict still stands: the
        // approval answers the hold, not the policy.
        if let Some(held) = held.filter(|_| !approval_satisfied) {
            // Park it, and hand back everything the agent needs to explain the
            // wait without a second call. Nothing is charged and nothing is
            // executed: this is the third answer between yes and no, and the
            // reason the policy engine is called an engine.
            let approval = state
                .db
                .create_purchase_approval(crate::approvals::NewApproval {
                    user_id: ctx.user_id(),
                    policy_id: Some(held.policy_id),
                    token_id: Some(ctx.token_id()),
                    wallet_id: ctx.sole_wallet().map(|wallet| wallet.wallet_id),
                    amount_usd: buyer_total.clone(),
                    destination_host: held.destination_host.as_deref(),
                    seller: Some(api_slug),
                    api_slug: Some(api_slug),
                    operation: Some(&resolved.resolved_path),
                    matched_rule: &held.matched_rule,
                    reason: &held.reason,
                    agent_label: None,
                })
                .await
                .map_err(|err| {
                    error!("could not hold {api_slug} for approval: {err}");
                    json_error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "approval_hold_failed",
                        "The buyer's rules need a human to approve this purchase, and the hold could not be created. Nothing was paid.",
                    )
                })?;

            warn!(
                policy_id = %held.policy_id,
                approval_id = %approval.id,
                rule = %held.matched_rule,
                "purchase held for approval: {api_slug}"
            );

            let body = serde_json::json!({
                "status": "approval_required",
                "error": "approval_required",
                "approval_id": approval.id,
                "expires_at": approval.expires_at,
                "approve_url": crate::approvals::approve_url(),
                "price_usdc": buyer_total.to_string(),
                "provider": api_slug,
                "capability": resolved.api.slug,
                "matched_rule": held.matched_rule,
                "message": format!(
                    "{} Nothing was paid and nothing was called. Tell the user their approval is waiting, poll GET /v1/approvals/{}, and call again with approval_id once it is approved.",
                    held.reason, approval.id
                ),
            });

            return Err(Response::builder()
                .status(StatusCode::PAYMENT_REQUIRED)
                .header("Content-Type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap_or_else(|_| {
                    json_error_response(
                        StatusCode::PAYMENT_REQUIRED,
                        "approval_required",
                        "This purchase needs the buyer's approval.",
                    )
                }));
        }
    }

    if !AllowedApiPaymentRails::from_resolved(resolved).x402 {
        return Err(json_error_response(
            StatusCode::BAD_REQUEST,
            "provider_x402_required",
            "The selected provider must accept x402.",
        ));
    }
    let ctx = connect_token_ctx
        .as_ref()
        .expect("treasury authorization checked above");
    let (session, fundability) =
        select_prepaid_wallet_with_fundability(state, ctx, &buyer_total).await?;
    if !state.is_treasury_wallet(&session.wallet_address) {
        return Err(json_error_response(
            StatusCode::FORBIDDEN,
            "treasury_purchase_required",
            "Only the Apiosk treasury pays providers.",
        ));
    }
    match fundability {
        WalletFundability::Fundable => Ok(EndpointPaymentDecision {
            deferred_payment: Some(DeferredApiPayment::AgentWalletPrepaid { session }),
            pending_credits_charge: None,
            agent_connect_token_id: connect_token_id,
        }),
        WalletFundability::NotFundable => Err(agent_wallet_insufficient_funds_response()),
        WalletFundability::Unknown => Err(agent_wallet_settlement_check_failed_response()),
    }
}

pub(super) fn insufficient_credits_response(
    browser_mode: bool,
    requirement: Option<&PaymentRequirement>,
    dashboard_url: Option<&str>,
    wallet_checkout_issue: Option<&str>,
    api_slug: &str,
    requested_path: &str,
    method: &Method,
    endpoint: &crate::db::Endpoint,
    allowed_payment_rails: AllowedApiPaymentRails,
) -> Response<Body> {
    if browser_mode {
        if let (Some(requirement), Some(dashboard_url)) = (requirement, dashboard_url) {
            let mut options = checkout_options_for_rails(
                dashboard_url,
                allowed_payment_rails,
                endpoint,
                wallet_checkout_issue,
            );
            options.banner = Some(CheckoutBanner {
                tone: CheckoutBannerTone::Warning,
                title: "Insufficient credits".to_string(),
                message: if allowed_payment_rails.x402 {
                    "Top up and retry automatically, or switch to wallet checkout for this request."
                        .to_string()
                } else {
                    "This API is credits-only. Add credits in the popup and retry automatically."
                        .to_string()
                },
            });
            payment_checkout_page(requirement, api_slug, requested_path, &options)
        } else {
            let (description, rails_label) = if allowed_payment_rails.x402 {
                (
                    "This endpoint can be paid with x402 or your credit wallet. Add credits and retry.",
                    allowed_payment_rails.payment_rails_label(),
                )
            } else {
                (
                    "This endpoint only accepts Apiosk credits. Add credits in your wallet and retry.",
                    allowed_payment_rails.payment_rails_label(),
                )
            };
            branded_status_page(
                StatusCode::PAYMENT_REQUIRED,
                "Insufficient credits",
                description,
                api_slug,
                requested_path,
                method,
                std::slice::from_ref(&endpoint.method),
                &[("Payment rails", rails_label)],
                Some(endpoint.price.clone()),
            )
        }
    } else {
        let message = if allowed_payment_rails.x402 {
            "Not enough credits for this request. Add credits or provide a valid x402 payment proof."
        } else {
            "Not enough credits for this request. Add credits and retry with your credits session."
        };
        json_error_response(
            StatusCode::PAYMENT_REQUIRED,
            "insufficient_credits",
            message,
        )
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn credits_pricing_not_supported_response(
    browser_mode: bool,
    requirement: Option<&PaymentRequirement>,
    dashboard_url: Option<&str>,
    wallet_checkout_issue: Option<&str>,
    api_slug: &str,
    requested_path: &str,
    method: &Method,
    endpoint: &crate::db::Endpoint,
    allowed_payment_rails: AllowedApiPaymentRails,
) -> Response<Body> {
    let (title, description) = if allowed_payment_rails.x402 {
        (
            "Credits unavailable for this price",
            "This endpoint price cannot be billed in whole Apiosk credits. Retry with x402 instead.",
        )
    } else {
        (
            "Credits unavailable for this price",
            "This endpoint is configured for credits, but its current price cannot be billed in whole Apiosk credits.",
        )
    };

    if browser_mode {
        if let (Some(requirement), Some(dashboard_url)) = (requirement, dashboard_url) {
            let mut options = checkout_options_for_rails(
                dashboard_url,
                allowed_payment_rails,
                endpoint,
                wallet_checkout_issue,
            );
            options.banner = Some(CheckoutBanner {
                tone: CheckoutBannerTone::Warning,
                title: title.to_string(),
                message: description.to_string(),
            });
            payment_checkout_page(requirement, api_slug, requested_path, &options)
        } else {
            branded_status_page(
                StatusCode::PAYMENT_REQUIRED,
                title,
                description,
                api_slug,
                requested_path,
                method,
                std::slice::from_ref(&endpoint.method),
                &[("Payment rails", allowed_payment_rails.payment_rails_label())],
                Some(endpoint.price.clone()),
            )
        }
    } else {
        json_error_response(
            StatusCode::PAYMENT_REQUIRED,
            "credits_price_not_supported",
            description,
        )
    }
}

// `fiat_mandate_required_response` used to live here: the 402 that told a
// buyer to verify a Dutch IBAN in the portal before Mollie SEPA incasso
// could charge them. Nothing asks for a SEPA mandate any more.

pub(crate) fn checkout_options_for_rails(
    dashboard_url: &str,
    allowed_payment_rails: AllowedApiPaymentRails,
    endpoint: &crate::db::Endpoint,
    wallet_checkout_issue: Option<&str>,
) -> PaymentCheckoutOptions {
    let mut options = PaymentCheckoutOptions::new(dashboard_url.to_string());
    let credits_supported_for_price = crate::db::endpoint_price_supports_credits(&endpoint.price);
    options.allow_credits = allowed_payment_rails.credits && credits_supported_for_price;
    options.allow_x402 = allowed_payment_rails.x402;
    options.credits_actions_enabled = options.allow_credits;
    options.wallet_actions_enabled = allowed_payment_rails.x402 && wallet_checkout_issue.is_none();

    if !allowed_payment_rails.credits {
        options.credits_hint = Some("Credits are not available for this API.".to_string());
    } else if !credits_supported_for_price {
        options.credits_hint =
            Some("This endpoint price cannot be billed in whole Apiosk credits.".to_string());
    }

    if let Some(issue) = wallet_checkout_issue {
        options.wallet_hint = Some(issue.to_string());
    } else if !allowed_payment_rails.x402 {
        options.wallet_hint =
            Some("This API does not accept wallet payments on the hosted checkout.".to_string());
    }

    options
}

#[cfg(test)]
mod usdc_acceptance_tests {
    use super::is_accepted_usdc_token;
    use crate::db::NetworkToken;
    use uuid::Uuid;

    /// A Base-mainnet token row with the given symbol + contract.
    fn base_token(symbol: &str, contract: &str) -> NetworkToken {
        NetworkToken {
            id: Uuid::nil(),
            network_name: "base".to_string(),
            network_display_name: "Base".to_string(),
            chain_id: 8453,
            explorer_url: None,
            is_testnet: false,
            token_symbol: symbol.to_string(),
            token_name: symbol.to_string(),
            token_decimals: 6,
            contract_address: contract.to_string(),
        }
    }

    const USDC: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
    const DAI: &str = "0x50c5725949A6F0c72E6C4a641F24049A917DB0Cb";

    #[test]
    fn accepts_canonical_base_usdc() {
        assert!(is_accepted_usdc_token(&base_token("USDC", USDC)));
    }

    #[test]
    fn accepts_usdc_regardless_of_address_casing() {
        // Rows may be stored checksummed or fully lower-cased.
        assert!(is_accepted_usdc_token(&base_token(
            "USDC",
            "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
        )));
    }

    #[test]
    fn rejects_base_dai() {
        // The seeded, still-active Base DAI row that the audit flagged.
        assert!(!is_accepted_usdc_token(&base_token("DAI", DAI)));
    }

    #[test]
    fn rejects_usdc_symbol_pointing_at_foreign_contract() {
        // A mislabelled row claiming "USDC" but resolving to DAI's contract.
        assert!(!is_accepted_usdc_token(&base_token("USDC", DAI)));
    }

    #[test]
    fn rejects_foreign_symbol_at_usdc_contract() {
        assert!(!is_accepted_usdc_token(&base_token("USDbC", USDC)));
    }

    #[test]
    fn accepts_canonical_usdc_on_all_supported_networks() {
        let cases = [
            ("polygon", 137, "0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359"),
            (
                "arbitrum",
                42161,
                "0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
            ),
        ];
        for (network, chain_id, contract) in cases {
            let mut token = base_token("USDC", contract);
            token.network_name = network.to_string();
            token.chain_id = chain_id;
            assert!(
                is_accepted_usdc_token(&token),
                "expected {network} USDC accepted"
            );
        }
        // Ethereum is seeded in the DB but NOT on the x402 offer allowlist.
        let mut eth = base_token("USDC", "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
        eth.network_name = "ethereum".to_string();
        assert!(!is_accepted_usdc_token(&eth));
    }
}

#[cfg(test)]
mod offer_selection_tests {
    use super::{select_offer_for_proof, settles_after_upstream, X402Offer};
    use crate::db::NetworkToken;
    use crate::listing::ListingType;
    use crate::payment::PaymentRequirement;
    use bigdecimal::BigDecimal;
    use std::str::FromStr;
    use uuid::Uuid;

    fn token(network: &str) -> NetworkToken {
        NetworkToken {
            id: Uuid::new_v4(),
            network_name: network.to_string(),
            network_display_name: network.to_string(),
            chain_id: 0,
            explorer_url: None,
            is_testnet: false,
            token_symbol: "USDC".to_string(),
            token_name: "USD Coin".to_string(),
            token_decimals: 6,
            contract_address: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".to_string(),
        }
    }

    fn offer(network: &str) -> X402Offer {
        let t = token(network);
        X402Offer {
            requirement: PaymentRequirement::new(
                "0xPLATFORM",
                &BigDecimal::from_str("0.02").unwrap(),
                "https://gateway.apiosk.com/x/y",
                "test",
                &t,
            ),
            token: t,
            payout_wallet: Some(format!("0xWALLET_{network}")),
        }
    }

    #[test]
    fn selects_the_offer_named_in_the_proof() {
        let offers = vec![offer("polygon"), offer("base")];
        let proof = r#"{"x402Version":1,"scheme":"exact","network":"base"}"#;
        assert_eq!(
            select_offer_for_proof(&offers, proof).token.network_name,
            "base"
        );
    }

    #[test]
    fn falls_back_to_primary_for_unknown_or_missing_network() {
        let offers = vec![offer("polygon"), offer("base")];
        let unknown = r#"{"x402Version":1,"scheme":"exact","network":"avalanche"}"#;
        assert_eq!(
            select_offer_for_proof(&offers, unknown).token.network_name,
            "polygon"
        );
        let missing = r#"{"x402Version":1,"scheme":"exact"}"#;
        assert_eq!(
            select_offer_for_proof(&offers, missing).token.network_name,
            "polygon"
        );
        assert_eq!(
            select_offer_for_proof(&offers, "not json at all")
                .token
                .network_name,
            "polygon"
        );
    }

    /// A federated listing MUST defer settlement until the upstream call
    /// succeeds. Settling up front skips the resell leg entirely — the leg only
    /// runs when a deferred payment is waiting — which charges the buyer and
    /// then hands them the provider's 402, and books a split that pays the
    /// provider's own payTo while the resell leg pays them again inline.
    #[test]
    fn federated_listings_settle_after_the_upstream_call() {
        assert!(
            settles_after_upstream(&ListingType::Federated),
            "a federated call is served by the resell leg, which needs a deferred payment"
        );
        assert!(settles_after_upstream(&ListingType::Api));
    }

    /// Everything that is not a proxied upstream call keeps settling up front.
    #[test]
    fn non_proxied_listing_types_still_settle_up_front() {
        for listing_type in [
            ListingType::Dataset,
            ListingType::Service,
            ListingType::Connector,
            ListingType::Skill,
            ListingType::Product,
        ] {
            assert!(
                !settles_after_upstream(&listing_type),
                "{listing_type:?} is not a proxied upstream call"
            );
        }
    }
}
