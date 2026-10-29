use super::*;
use serde::Serialize;

/// "What would actually happen if I called this paid endpoint right now?"
///
/// Returned by the X-Apiosk-Dry-Run: 1 short-circuit in handle_flow. No
/// on-chain transfer, no upstream call, no settlement row. The gateway
/// applies the same wallet-selection + caps + fundability checks as a real
/// call so the answer reflects production behavior, not a parallel model.
#[derive(Debug, Serialize)]
pub(super) struct PreflightVerdict {
    pub slug: String,
    pub operation: String,
    pub method: String,
    pub price_listed_usdc: f64,
    /// "apiosk_balance" | "x402" | "credits" | "free" | "unauthorized". "sepa" was a fifth
    /// value until the euro rail was removed; it is never emitted now.
    pub rail: String,
    pub wallet_to_use: Option<String>,
    pub would_succeed: bool,
    pub reason_if_not: Option<String>,
    pub spent_after: PreflightSpentAfter,
    /// Inline reminder of how the gateway picks a wallet, mirrored from
    /// /v1/me. Surfaces selection-policy at the point an agent is most
    /// likely to be making a budget decision.
    pub wallet_selection_strategy: String,
    /// Lookups this endpoint can need before it runs: a required identifier
    /// the caller may not have (a `kvkNummer` behind a company name) that a
    /// declared resolver endpoint produces, charged as its own call.
    ///
    /// Unconditional, unlike the same field on `/v1/discover` and
    /// `/v1/quote`: preflight is asked about a URL, not about a job, so
    /// whether the identifier is already present is visible in the request
    /// itself and not something this can second-guess. Empty for the
    /// overwhelming majority of endpoints, which declare no resolver.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub possible_pre_steps: Vec<PreflightPreStep>,
    /// This call's buyer total plus every possible pre-step. Absent when
    /// there are none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub combined_buyer_total_usdc: Option<f64>,
}

/// One declared lookup, priced the way the buyer would be charged for it.
#[derive(Debug, Serialize)]
pub(super) struct PreflightPreStep {
    pub label: String,
    pub parameter: String,
    pub api_slug: String,
    pub endpoint: String,
    pub buyer_price_usdc: f64,
    /// What makes it unnecessary: supply this parameter and no lookup runs.
    pub skipped_when: String,
}

#[derive(Debug, Serialize, Default)]
pub(super) struct PreflightSpentAfter {
    pub wallet_address: Option<String>,
    pub wallet_spent_today_usdc: f64,
    pub wallet_day_cap_usdc: f64,
    // `sepa_spent_today_eur` / `sepa_day_cap_eur` were reported here while
    // SEPA incasso was a rail. Both are gone with it; the wallet pair above is
    // the whole budget picture now. Existing clients read them as optional
    // extras, so their absence is not a breaking shape change.
}

const WALLET_SELECTION_DOC: &str = "first_authorized_wallet_passing_caps_and_onchain_can_settle";

pub(super) fn dry_run_requested(headers: &HeaderMap<HeaderValue>) -> bool {
    let Some(value) = headers.get("x-apiosk-dry-run") else {
        return false;
    };
    matches!(
        value.to_str().ok().map(str::trim).map(str::to_ascii_lowercase),
        Some(ref v) if v == "1" || v == "true" || v == "yes"
    )
}

/// Honors the X-Apiosk-Rail override used for rail-level testability.
/// Returns Some("x402") for the one recognized value, None when the header is
/// absent or carries any other value. Unknown values are intentionally ignored
/// (treated as absent) so a typo never changes settlement behavior silently —
/// which is also what now happens to the retired value "sepa".
pub(super) fn forced_rail(headers: &HeaderMap<HeaderValue>) -> Option<&'static str> {
    let value = headers.get("x-apiosk-rail")?;
    match value
        .to_str()
        .ok()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("x402") => Some("x402"),
        _ => None,
    }
}

pub(super) async fn build_preflight_verdict(
    state: &Arc<AppState>,
    headers: &HeaderMap<HeaderValue>,
    resolved: &ResolvedProxyRequest,
) -> PreflightVerdict {
    let api_slug = resolved.api.slug.clone();
    let (operation, method, price_usd, payment_required) = if let Some(ref ep) = resolved.endpoint {
        (
            ep.path.clone(),
            ep.method.clone(),
            ep.price.to_string().parse::<f64>().unwrap_or(0.0),
            ep.payment_required,
        )
    } else {
        (
            resolved.resolved_path.clone(),
            resolved.resolved_method.as_str().to_string(),
            0.0_f64,
            false,
        )
    };

    let mut verdict = PreflightVerdict {
        slug: api_slug.clone(),
        operation,
        method,
        price_listed_usdc: price_usd,
        rail: "free".to_string(),
        wallet_to_use: None,
        would_succeed: true,
        reason_if_not: None,
        spent_after: PreflightSpentAfter::default(),
        wallet_selection_strategy: WALLET_SELECTION_DOC.to_string(),
        possible_pre_steps: Vec::new(),
        combined_buyer_total_usdc: None,
    };

    // Declared lookups for this endpoint. One query, skipped entirely when
    // the endpoint is unpriced; a failure here degrades the report, never the
    // verdict.
    // Buyer figures follow the request's markup: 20% only beside a valid app
    // service key that asked for it, 15% for everyone else.
    let markup_bps =
        crate::service_auth::buyer_markup_bps(state.config.app_service_key.as_deref(), headers);
    if let Some(ref ep) = resolved.endpoint {
        if let Ok(rows) = state.db.pre_step_rows(&[ep.id], &[]).await {
            verdict.possible_pre_steps = rows
                .iter()
                .map(|row| PreflightPreStep {
                    label: row.label.clone(),
                    parameter: row.target_param.clone(),
                    api_slug: row.resolver_api_slug.clone(),
                    endpoint: format!("{} {}", row.resolver_method, row.resolver_path),
                    buyer_price_usdc: crate::fees::buyer_total_usd_with_markup(
                        row.resolver_price_usd,
                        row.resolver_hosted_externally,
                        markup_bps,
                    ),
                    skipped_when: format!(
                        "the request already carries {} — no lookup runs and nothing extra is charged",
                        row.target_param
                    ),
                })
                .collect();
            if !verdict.possible_pre_steps.is_empty() {
                let own = crate::fees::buyer_total_usd_with_markup(price_usd, false, markup_bps);
                let extra: f64 = verdict
                    .possible_pre_steps
                    .iter()
                    .map(|s| s.buyer_price_usdc)
                    .sum();
                verdict.combined_buyer_total_usdc = Some(own + extra);
            }
        }
    }

    if !payment_required || price_usd <= 0.0 {
        return verdict;
    }

    let Some(ref ep) = resolved.endpoint else {
        return verdict;
    };

    if resolved.uses_app_balance() {
        // Reuse the real authorization and price-cap gates. This branch only
        // prepares an internal receipt; it never calls or charges a source.
        let tracking = TrackingContext {
            request_id: Some(Uuid::nil()),
            buyer_markup_bps: markup_bps,
            ..Default::default()
        };
        verdict.rail = "apiosk_balance".to_string();
        verdict.wallet_selection_strategy = "not_applicable".to_string();
        if let Err(response) = enforce_endpoint_payment(
            state,
            &api_slug,
            &resolved.resolved_path,
            headers,
            false,
            &tracking,
            resolved,
        )
        .await
        {
            verdict.would_succeed = false;
            let bytes = axum::body::to_bytes(response.into_body(), 16384).await.ok();
            verdict.reason_if_not = bytes
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .and_then(|body| {
                    body.get("message").and_then(|v| v.as_str()).map(str::to_owned)
                })
                .or_else(|| Some("App balance authorization was refused.".to_string()));
        }
        return verdict;
    }

    // Connect token resolution. Surface authoritative reasons rather than
    // re-encoding the gateway's own error catalog.
    let token_ctx = match resolve_connect_token(state, headers).await {
        Ok(ctx) => ctx,
        Err(_) => {
            verdict.rail = "unauthorized".to_string();
            verdict.would_succeed = false;
            verdict.reason_if_not = Some(
                "Connect token is invalid, revoked, or expired — call /v1/me with the same headers to see why."
                    .to_string(),
            );
            return verdict;
        }
    };
    let Some(ctx) = token_ctx else {
        verdict.rail = "unauthorized".to_string();
        verdict.would_succeed = false;
        verdict.reason_if_not = Some(
            "No connect token sent — attach X-Apiosk-Connect-Token or Authorization: Bearer."
                .to_string(),
        );
        return verdict;
    };

    let allowed_rails = AllowedApiPaymentRails::from_resolved(resolved);
    // NOTE: this dry-run models the x402 rail only; it does not emit a
    // 'credits' verdict even though metadata-less Api listings now allow credits
    // (AllowedApiPaymentRails::from_resolved). A real call may still settle over
    // credits when this preflight reports x402 — credits coverage in the
    // dry-run is a deliberate follow-up, not a guarantee that credits is unusable.
    let forced = forced_rail(headers);

    // `X-Apiosk-Rail: sepa` used to short-circuit here into a pure-SEPA
    // verdict, checking the connection's euro authorization and the buyer's
    // Mollie mandate. `forced_rail` no longer returns that value.

    // Wallet path: same selector as real calls. Picks the first wallet that
    // passes caps + on-chain can_settle. It used to fall back to a SEPA verdict
    // whenever no wallet could carry the call; with the euro rail gone the
    // failure is simply reported as an x402 failure.
    if ctx.has_wallets() && allowed_rails.x402 {
        match select_prepaid_wallet_with_fundability(state, &ctx, &ep.price).await {
            Ok((session, WalletFundability::Fundable)) => {
                verdict.rail = "x402".to_string();
                verdict.wallet_to_use = Some(session.wallet_address.clone());
                populate_spent_after(
                    state,
                    &ctx,
                    Some(&session.wallet_address),
                    price_usd,
                    &mut verdict,
                )
                .await;
                return verdict;
            }
            Ok((session, WalletFundability::Unknown)) => {
                // RPC degraded. There is no second rail to route to, so the
                // real flow still attempts settlement and this reports why it
                // might not land.
                verdict.rail = "x402".to_string();
                verdict.wallet_to_use = Some(session.wallet_address.clone());
                verdict.would_succeed = false;
                verdict.reason_if_not = Some(
                    "Wallet fundability could not be confirmed (settlement RPC degraded)."
                        .to_string(),
                );
                return verdict;
            }
            Ok((_, WalletFundability::NotFundable)) => {
                verdict.rail = "x402".to_string();
                verdict.would_succeed = false;
                verdict.reason_if_not = Some(
                    "No authorized wallet has enough USDC balance / allowance to settle this call."
                        .to_string(),
                );
                return verdict;
            }
            Err(_) => {
                // All wallets failed caps. There is no other rail to try.
                verdict.rail = "x402".to_string();
                verdict.would_succeed = false;
                verdict.reason_if_not = Some(
                    "Every authorized wallet failed per-tx or daily caps for this call amount."
                        .to_string(),
                );
                return verdict;
            }
        }
    }

    // X-Apiosk-Rail: x402 with no wallet path available (no authorized
    // wallet, or this API does not accept x402).
    if forced == Some("x402") {
        verdict.rail = "x402".to_string();
        verdict.would_succeed = false;
        verdict.reason_if_not = Some(
            "X-Apiosk-Rail: x402 requested but no authorized wallet can settle this call."
                .to_string(),
        );
        return verdict;
    }

    // A pure-SEPA verdict used to be returned here for a connection that
    // authorized the euro rail but no wallet, after checking the buyer's Mollie
    // mandate. Such a connection now has no rail at all and falls through to
    // "unauthorized" below.

    verdict.rail = "unauthorized".to_string();
    verdict.would_succeed = false;
    verdict.reason_if_not = Some(
        "Connect token does not authorize a rail that this API accepts (USDC x402 / credits)."
            .to_string(),
    );
    verdict
}

async fn populate_spent_after(
    state: &Arc<AppState>,
    ctx: &ConnectTokenContext,
    wallet_address: Option<&str>,
    price_usd: f64,
    verdict: &mut PreflightVerdict,
) {
    if let Some(addr) = wallet_address {
        let spent = state
            .db
            .get_wallet_paid_spend_today(addr)
            .await
            .ok()
            .and_then(|d| d.to_string().parse::<f64>().ok())
            .unwrap_or(0.0);
        let cap = ctx
            .token
            .wallets
            .iter()
            .find(|w| w.wallet_address.eq_ignore_ascii_case(addr))
            .map(|w| w.daily_limit_usdc.to_string().parse::<f64>().unwrap_or(0.0))
            .unwrap_or(0.0);
        verdict.spent_after.wallet_address = Some(addr.to_string());
        verdict.spent_after.wallet_spent_today_usdc = spent + price_usd;
        verdict.spent_after.wallet_day_cap_usdc = cap;
    }
    // Today's euro spend against the connection's SEPA day cap was filled in
    // here too. Both fields are gone with the euro rail.
}

pub(super) fn preflight_response(verdict: &PreflightVerdict) -> Response<Body> {
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header("Cache-Control", "no-store")
        .header("X-Apiosk-Gateway", "true")
        .header("X-Apiosk-Dry-Run", "true")
        .body(Body::from(
            serde_json::to_vec(verdict).unwrap_or_else(|_| b"{}".to_vec()),
        ))
        .unwrap_or_else(|_| Response::new(Body::from(r#"{"error":"preflight_build_error"}"#)))
}

/// The approval an agent is resuming, from `X-Apiosk-Approval-Id`.
///
/// A header rather than a body field because the proxy path serves every shape
/// of request — GET, POST, form, binary — and a resumed purchase has to work on
/// all of them. `/v1/do` takes it as `approval_id` and forwards it here.
pub(super) fn approval_id_header(headers: &HeaderMap<HeaderValue>) -> Option<uuid::Uuid> {
    headers
        .get("x-apiosk-approval-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse().ok())
}
