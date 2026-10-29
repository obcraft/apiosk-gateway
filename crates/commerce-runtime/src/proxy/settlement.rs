use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) async fn settle_deferred_api_payment(
    state: &Arc<AppState>,
    api_slug: &str,
    path: &str,
    api: &crate::db::Api,
    endpoint: &crate::db::Endpoint,
    deferred_payment: &DeferredApiPayment,
    tracking_context: &TrackingContext,
    // Set when the resell leg already paid the upstream from the bridge wallet.
    resell: Option<&crate::proxy::resell::ResellOutcome>,
) -> Result<SettledPaymentReceipt, Response<Body>> {
    // What the buyer actually paid — list + 15% under the hybrid model, not the
    // bare list price. This value is surfaced to the caller as `price_paid`, so
    // quoting the list price here would under-report every debit.
    // A workflow run priced by the App at list + 20% reports that total.
    let price_usd: f64 = crate::fees::quote_usdc_with_markup(
        &endpoint.price,
        crate::fees::ProviderClass::from_listing_type(api.listing_type.as_deref()),
        tracking_context.buyer_markup_bps(),
    )
    .buyer_total_usdc()
    .to_string()
    .parse()
    .unwrap_or(0.0);
    let (tx_hash, wallet_used) = match deferred_payment {
        DeferredApiPayment::AppBalance => {
            if !api.apiosk_revenue || resell.is_some() {
                return Err(json_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "invalid_settlement_rail",
                    "This source requires provider payment.",
                ));
            }
            let tx = internal_revenue_receipt_id(api, tracking_context)?;
            // Keep request and connection attribution, but never attribute an
            // internal balance purchase to a wallet or blockchain network.
            let mut internal_tracking = tracking_context.clone();
            internal_tracking.managed_wallet_id = None;
            record_paid_execution(
                state,
                api_slug,
                path,
                api,
                endpoint,
                &tx,
                None,
                &internal_tracking,
                None,
                None,
                None,
            )
            .await;
            (tx, None)
        }
        DeferredApiPayment::AgentWalletPrepaid { session } => {
            if !state.is_treasury_wallet(&session.wallet_address) {
                return Err(json_error_response(
                    StatusCode::FORBIDDEN,
                    "treasury_purchase_required",
                    "Only the Apiosk treasury pays providers.",
                ));
            }
            let tx = if let Some(paid) = resell {
                paid.transaction.clone()
            } else {
                pay_managed_provider(state, api, endpoint, tracking_context).await?
            };
            record_paid_execution(
                state,
                api_slug,
                path,
                api,
                endpoint,
                &tx,
                Some(session.wallet_address.clone()),
                &tracking_context.with_managed_wallet_id(session.wallet_id),
                None,
                None,
                resell,
            )
            .await;
            (tx, Some(session.wallet_address.clone()))
        }
    };

    Ok(SettledPaymentReceipt {
        price_paid_usdc: price_usd,
        price_listed_usdc: endpoint.price.to_string().parse().unwrap_or(0.0),
        tx_hash,
        wallet_used,
    })
}

/// Stable internal ledger reference, never an on-chain transaction hash.
pub(super) fn internal_revenue_receipt_id(
    api: &crate::db::Api,
    tracking: &TrackingContext,
) -> Result<String, Response<Body>> {
    let request = tracking.request_id.ok_or_else(|| {
        json_error_response(
            StatusCode::BAD_REQUEST,
            "request_id_required",
            "Request id missing.",
        )
    })?;
    Ok(format!(
        "apiosk_revenue_{}_{}",
        api.id.simple(),
        request.simple()
    ))
}

pub(super) fn build_agent_wallet_internal_tx_hash(wallet_id: Uuid) -> String {
    format!(
        "aw_internal_{}_{}",
        wallet_id.simple(),
        Uuid::new_v4().simple()
    )
}

pub(super) fn is_internal_agent_wallet_tx_hash(tx_hash: &str) -> bool {
    tx_hash.starts_with("aw_internal_")
}

pub(super) fn connector_from_listing_metadata(
    api_id: Uuid,
    metadata: &serde_json::Value,
) -> Option<crate::listing::ConnectorListing> {
    let platform = metadata.get("platform")?.as_str()?.trim();
    let relay_url = metadata.get("relay_url")?.as_str()?.trim();

    if platform.is_empty() || relay_url.is_empty() {
        return None;
    }

    let auth_type = metadata
        .get("auth_type")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("apikey");

    let supported_actions = metadata
        .get("supported_actions")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));

    let auth_secret = metadata
        .get("auth_secret")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    Some(crate::listing::ConnectorListing {
        id: Uuid::nil(),
        api_id,
        platform: platform.to_string(),
        auth_type: auth_type.to_string(),
        relay_url: relay_url.to_string(),
        supported_actions,
        auth_secret,
    })
}

/// Book the confirmed treasury payment and buyer total atomically.
/// Provider money has already moved; this never queues a second payout.
#[allow(clippy::too_many_arguments)]
pub(super) async fn record_paid_execution(
    state: &Arc<AppState>,
    api_slug: &str,
    path: &str,
    api: &crate::db::Api,
    endpoint: &crate::db::Endpoint,
    tx_hash: &str,
    wallet_address: Option<String>,
    tracking_context: &TrackingContext,
    // Which accepts[] network the buyer actually settled on (None = the
    // endpoint's own network, i.e. legacy single-network behaviour) and the
    // provider's payout wallet on that chain.
    settled_network_token_id: Option<Uuid>,
    settled_payout_wallet: Option<&str>,
    resell: Option<&crate::proxy::resell::ResellOutcome>,
) {
    // The hybrid fee model (`crate::fees`) is the single source of the split.
    // `endpoint.price` is the provider's LIST price; the buyer was charged
    // list + 15% (the buyer-side rule never varies), and the provider leg
    // depends only on how the listing is distributed:
    //
    //   federated → provider keeps its full asking price, Apiosk keeps the 15%
    //   managed   → provider pays 10%, Apiosk keeps 25% of list
    //
    // Nothing here re-derives a percentage; `fees::quote_usdc` produces every
    // leg and guarantees they re-sum to what the buyer paid.
    //
    // The buyer markup is the one per-request input: 15% by default, 20% on a
    // workflow run the App's service key priced that way. It moves the buyer
    // total and Apiosk's take only; the provider leg is identical either way.
    let provider_class = crate::fees::ProviderClass::from_listing_type(api.listing_type.as_deref());
    let list_quote = crate::fees::quote_usdc_with_markup(
        &endpoint.price,
        provider_class,
        tracking_context.buyer_markup_bps(),
    );

    // A RESOLD call has already paid the provider, inline, from the bridge
    // wallet — their own x402 pull. The "developer" leg here must therefore
    // reimburse the BRIDGE, not pay the provider a second time.
    //
    // This matters most on a federated listing, where `apis.wallet_address`
    // holds the provider's own payTo: paying it here would send them the money
    // twice, once by the resell leg and once by the payout below.
    //
    // The bridge is reimbursed exactly what it fronted, so the provider keeps
    // their full asking price and Apiosk keeps the remainder of the buyer's
    // `list + 15%`. Same shape as `proxy::external_fetch`.
    let quote = paid_execution_quote(list_quote, resell, api.apiosk_revenue);

    let SettlementAmounts {
        gross_amount,
        commission_amount,
        developer_amount,
        commission_rate,
    } = settlement_amounts(&quote);

    // `apis.commission_rate` and `commission_tiers` belong to the retired
    // flat-rate model, where a single percentage came OUT of the list price.
    // The hybrid model is two-sided and class-determined, so those columns no
    // longer drive any money movement. A row still carrying a non-canonical
    // rate means stale data that will make admin reporting disagree with the
    // ledger — surface it rather than let the two drift silently.
    let legacy_rate = match state
        .db
        .get_effective_commission_rate(api.id, api.commission_rate.clone())
        .await
    {
        Ok(rate) => rate,
        Err(e) => {
            error!("Failed to resolve commission tier: {}, using base rate", e);
            api.commission_rate.clone()
        }
    };
    let expected_legacy = crate::fees::legacy_commission_rate_for(provider_class);
    if !api.apiosk_revenue && legacy_rate != expected_legacy {
        warn!(
            "Stale apis.commission_rate for {}/{}: row says {} but this {} listing settles at {} \
             of list under the hybrid model. The split follows crate::fees, not this column — \
             migration 088 backfills it.",
            api_slug,
            path,
            legacy_rate,
            provider_class.as_str(),
            expected_legacy
        );
    }

    info!(
        "Payment settled for {}/{} ({}): tx={}, payer={:?}, list={}, buyer_paid={}, apiosk={}, developer={}",
        api_slug,
        path,
        provider_class.as_str(),
        tx_hash,
        wallet_address,
        endpoint.price,
        gross_amount,
        commission_amount,
        developer_amount
    );

    // Single transaction: settlement + developer ledger + paid usage row +
    // optional community revenue. Either everything lands or nothing does.
    let community_revenue_wallet = if api.is_community.unwrap_or(false) {
        let trimmed = api.wallet_address.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    } else {
        None
    };

    let initial_status = crate::db::SettlementStatus::Settled;
    let newly_recorded = match state
        .db
        .record_paid_settlement_atomically(
            api.id,
            endpoint.id,
            api.owner_id,
            tx_hash,
            gross_amount.clone(),
            commission_rate.clone(),
            commission_amount.clone(),
            developer_amount.clone(),
            if api.apiosk_revenue && resell.is_none() {
                None
            } else {
                settled_network_token_id.or(endpoint.network_token_id)
            },
            wallet_address.clone(),
            tracking_context.request_id,
            tracking_context.session_id,
            tracking_context.managed_wallet_id,
            tracking_context.client_id.as_deref(),
            tracking_context.agent_connect_token_id,
            community_revenue_wallet,
            initial_status,
        )
        .await
    {
        Ok(true) => true,
        Ok(false) => {
            info!(
                "Settlement {} already recorded for {}/{}; skipping duplicate ledger writes and payout",
                tx_hash, api_slug, path
            );
            false
        }
        Err(e) => {
            error!("Failed to record paid settlement atomically: {}", e);
            // Ledger write failed: do NOT enqueue a payout. Paying out without a
            // confirmed settlement row would leave the books inconsistent (and a
            // retry could double-pay). Reconciliation surfaces an on-chain
            // settlement that has no ledger row.
            false
        }
    };

    if newly_recorded {
        info!("Treasury provider payment recorded for {}", api_slug);
    }
}

/// Which split a paid execution books, from the list quote the buyer was
/// charged (at this request's markup):
///
/// * resold — the bridge already paid the provider inline and is reimbursed
///   exactly that; the rest of the buyer total is Apiosk's
///   ([`crate::fees::resell_bridge_quote`]);
/// * a reviewed platform-owned listing — no provider, all of it Apiosk's;
/// * otherwise — the hybrid split as quoted.
fn paid_execution_quote(
    list_quote: crate::fees::FeeQuote,
    resell: Option<&crate::proxy::resell::ResellOutcome>,
    apiosk_revenue: bool,
) -> crate::fees::FeeQuote {
    match resell {
        Some(outcome) => {
            let upstream = i128::try_from(outcome.upstream_atomic).unwrap_or(i128::MAX);
            crate::fees::resell_bridge_quote(list_quote, upstream)
        }
        None if apiosk_revenue => crate::fees::platform_revenue_quote(list_quote),
        None => list_quote,
    }
}

/// The settlement row's money columns, all read off one quote.
struct SettlementAmounts {
    gross_amount: BigDecimal,
    commission_amount: BigDecimal,
    developer_amount: BigDecimal,
    commission_rate: BigDecimal,
}

fn settlement_amounts(quote: &crate::fees::FeeQuote) -> SettlementAmounts {
    let gross_amount = quote.buyer_total_usdc();
    let commission_amount = quote.apiosk_take_usdc();
    let developer_amount = quote.provider_payout_usdc();
    // The ledger's rate column records the take as a fraction of what the buyer
    // actually paid — ~0.1304 federated, ~0.2174 managed at 15%; ~0.1667 and
    // 0.25 on a 20% workflow run. It is a derived reporting figure, not an
    // input: the split already happened.
    let commission_rate = if gross_amount == 0_i64 {
        BigDecimal::from(0_i64)
    } else {
        &commission_amount / &gross_amount
    };
    SettlementAmounts {
        gross_amount,
        commission_amount,
        developer_amount,
        commission_rate,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

/// Pay a hosted x402 offer from treasury, using its provider payout destination.
/// A durable claim precedes signing. Ambiguous delivery stays held, never retried.
async fn pay_managed_provider(
    state: &Arc<AppState>,
    api: &crate::db::Api,
    endpoint: &crate::db::Endpoint,
    tracking: &TrackingContext,
) -> Result<String, Response<Body>> {
    if api.apiosk_revenue {
        return Err(json_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "invalid_settlement_rail",
            "This source must use the approved App balance.",
        ));
    }
    let fail = |message: &str| {
        let mut response = json_error_response(
            StatusCode::BAD_GATEWAY,
            "treasury_payment_unavailable",
            message,
        );
        response
            .headers_mut()
            .insert("x-apiosk-buyer-total-atomic", HeaderValue::from_static("0"));
        response
    };
    let uncertain = || {
        json_error_response(
            StatusCode::GATEWAY_TIMEOUT,
            "payment_unresolved",
            "Payment confirmation is pending. The amount remains held.",
        )
    };
    let payer = state
        .resell_payer
        .as_ref()
        .ok_or_else(|| fail("Treasury is unavailable."))?;
    let request = tracking
        .request_id
        .ok_or_else(|| fail("Request id missing."))?;
    let token = state
        .db
        .get_default_network_token()
        .await
        .map_err(|_| fail("Token unavailable."))?
        .filter(|t| t.network_name == "base" && t.token_symbol == "USDC")
        .ok_or_else(|| fail("Base USDC is unavailable."))?;
    let quote = crate::fees::quote_usdc(
        &endpoint.price,
        crate::fees::ProviderClass::from_listing_type(api.listing_type.as_deref()),
    );
    let requirement = PaymentRequirement::new(
        &api.wallet_address,
        &quote.provider_payout_usdc(),
        &format!("https://gateway.apiosk.com/{}{}", api.slug, endpoint.path),
        "Provider payment",
        &token,
    );
    let raw =
        serde_json::to_vec(&serde_json::json!({"x402Version":1,"accepts":[requirement.clone()]}))
            .map_err(|_| fail("Offer unreadable."))?;
    let upstream = super::resell::parse_upstream_402(&raw, &[8453])
        .map_err(|_| fail("Provider offer invalid."))?;
    let amount = super::resell::check_spend_allowed(
        &upstream.value_atomic,
        payer.cap_per_tx_atomic,
        quote.buyer_total_atomic as u128,
    )
    .map_err(|_| fail("Treasury cap exceeded."))?;
    if !state
        .db
        .try_claim_resell_payment(request, api.id, amount, payer.cap_per_day_atomic)
        .await
        .map_err(|_| fail("Payment claim failed."))?
    {
        return Err(uncertain());
    }
    let proof = super::resell::build_x402_payment_header(
        payer.wallet(),
        &upstream,
        &request,
        chrono::Utc::now().timestamp().max(0) as u64,
    )
    .map_err(|_| fail("Payment signing failed."))?;
    let result = settle_payment(
        &state.http_client,
        &state.config,
        &state.facilitator_url,
        &proof,
        &requirement,
    )
    .await
    .map_err(|_| uncertain())?;
    if !result.success {
        return Err(uncertain());
    }
    let tx = result
        .transaction
        .filter(|tx| !tx.is_empty())
        .ok_or_else(uncertain)?;
    state
        .db
        .mark_resell_payment(request, "paid", Some(&tx))
        .await
        .map_err(|_| uncertain())?;
    Ok(tx)
}

#[cfg(test)]
mod markup_booking_tests {
    use super::*;
    use std::str::FromStr;

    fn usdc(s: &str) -> BigDecimal {
        BigDecimal::from_str(s).unwrap()
    }

    fn resold(upstream_atomic: u128) -> crate::proxy::resell::ResellOutcome {
        crate::proxy::resell::ResellOutcome {
            transaction: "0xabc".to_string(),
            upstream_atomic,
            payer_address: "0x0000000000000000000000000000000000000001".to_string(),
        }
    }

    fn list_quote(
        list: &str,
        class: crate::fees::ProviderClass,
        bps: i128,
    ) -> crate::fees::FeeQuote {
        crate::fees::quote_usdc_with_markup(&usdc(list), class, bps)
    }

    #[test]
    fn tracking_context_defaults_to_the_standard_markup() {
        let tracking = TrackingContext::default();
        assert_eq!(tracking.buyer_markup_bps(), crate::fees::BUYER_MARKUP_BPS);
        let workflow = TrackingContext {
            buyer_markup_bps: 2_000,
            ..Default::default()
        };
        assert_eq!(workflow.buyer_markup_bps(), 2_000);
        // Carried through the clones the paid path makes.
        assert_eq!(
            workflow
                .with_managed_wallet_id(Uuid::nil())
                .with_connect_token_id(Uuid::nil())
                .buyer_markup_bps(),
            2_000
        );
        let bogus = TrackingContext {
            buyer_markup_bps: 3_000,
            ..Default::default()
        };
        assert_eq!(bogus.buyer_markup_bps(), 1_500);
    }

    /// A managed $0.10 call on a 20% workflow run books gross $0.12, Apiosk
    /// $0.03, provider $0.09 — the provider leg is what a 15% call books.
    #[test]
    fn managed_workflow_run_books_twenty_percent_with_the_extra_to_apiosk() {
        let class = crate::fees::ProviderClass::Managed;
        let std = settlement_amounts(&paid_execution_quote(
            list_quote("0.10", class, 1_500),
            None,
            false,
        ));
        let wf = settlement_amounts(&paid_execution_quote(
            list_quote("0.10", class, 2_000),
            None,
            false,
        ));
        assert_eq!(std.gross_amount, usdc("0.115"));
        assert_eq!(wf.gross_amount, usdc("0.12"));
        assert_eq!(wf.commission_amount, usdc("0.03"));
        assert_eq!(wf.developer_amount, usdc("0.09"));
        assert_eq!(wf.developer_amount, std.developer_amount);
        assert_eq!(
            &wf.commission_amount + &wf.developer_amount,
            wf.gross_amount
        );
        assert_eq!(wf.commission_rate, usdc("0.25"));
    }

    #[test]
    fn federated_workflow_run_keeps_the_provider_whole() {
        let class = crate::fees::ProviderClass::Federated;
        let wf = settlement_amounts(&paid_execution_quote(
            list_quote("0.10", class, 2_000),
            None,
            false,
        ));
        assert_eq!(wf.gross_amount, usdc("0.12"));
        assert_eq!(wf.developer_amount, usdc("0.1"));
        assert_eq!(wf.commission_amount, usdc("0.02"));
    }

    #[test]
    fn resold_workflow_run_reimburses_the_bridge_and_books_twenty_percent() {
        let class = crate::fees::ProviderClass::Federated;
        let wf = settlement_amounts(&paid_execution_quote(
            list_quote("0.10", class, 2_000),
            Some(&resold(100_000)),
            false,
        ));
        assert_eq!(wf.gross_amount, usdc("0.12"));
        assert_eq!(
            wf.developer_amount,
            usdc("0.1"),
            "bridge made whole exactly"
        );
        assert_eq!(wf.commission_amount, usdc("0.02"));

        let std = settlement_amounts(&paid_execution_quote(
            list_quote("0.10", class, 1_500),
            Some(&resold(100_000)),
            false,
        ));
        assert_eq!(std.gross_amount, usdc("0.115"));
        assert_eq!(std.developer_amount, wf.developer_amount);

        // An absurd upstream amount is capped at the buyer total, never wrapped.
        let capped = paid_execution_quote(
            list_quote("0.10", class, 2_000),
            Some(&resold(u128::MAX)),
            false,
        );
        assert_eq!(capped.provider_payout_atomic, 120_000);
        assert_eq!(capped.apiosk_take_atomic, 0);
    }

    #[test]
    fn platform_owned_workflow_run_books_everything_to_apiosk() {
        let class = crate::fees::ProviderClass::Managed;
        let wf = settlement_amounts(&paid_execution_quote(
            list_quote("0.10", class, 2_000),
            None,
            true,
        ));
        assert_eq!(wf.gross_amount, usdc("0.12"));
        assert_eq!(wf.commission_amount, usdc("0.12"));
        assert_eq!(wf.developer_amount, BigDecimal::from(0));
        assert_eq!(wf.commission_rate, BigDecimal::from(1));
    }
}
