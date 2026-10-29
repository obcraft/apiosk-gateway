use std::sync::Arc;

use axum::{
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use serde_json::Value;
use tracing::{error, warn};

use crate::auth::extract_agent_connect_token;
use crate::db::AgentConnectTokenLookup;
use crate::proxy::hash_connect_token;
use crate::AppState;

/// Agent self-introspection. Returns everything an agent needs to budget
/// before making paid calls: which wallets it can spend from, the caps on
/// each, and how much it has already spent today.
///
/// Two fields this response used to carry are gone, because the things they
/// described are gone. `sepa` (and the matching `"sepa"` entry in `rails`)
/// reported a euro budget on the SEPA-incasso rail; `wallets[].balance_usdc`
/// reported a live on-chain USDC balance read from the settlement contract's
/// RPC. Apiosk now keeps exactly one payment rail — the buyer's Apiosk balance
/// is debited in the App's Supabase and the treasury pays the provider over
/// x402 — so there is no euro budget and no buyer wallet whose chain balance
/// would mean anything here. `rails` is therefore always `["x402"]` or empty.
/// Both fields were already `skip_serializing_if`-optional, so a client that
/// handled their absence needs no change.
pub async fn me(State(state): State<Arc<AppState>>, headers: HeaderMap<HeaderValue>) -> Response {
    let Some(raw_token) = extract_agent_connect_token(&headers) else {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "agent_wallet_token_missing",
            "Send the connect token via X-Apiosk-Connect-Token or Authorization: Bearer <token>.",
        );
    };
    let token_hash = hash_connect_token(&raw_token);
    let lookup = match state
        .db
        .lookup_agent_connect_token_by_hash(&token_hash)
        .await
    {
        Ok(lookup) => lookup,
        Err(err) => {
            error!("Connect token lookup failed for /v1/me: {}", err);
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "agent_wallet_lookup_failed",
                "Failed to validate agent wallet token.",
            );
        }
    };
    let token = match lookup {
        AgentConnectTokenLookup::Active(token) => token,
        AgentConnectTokenLookup::Unknown => {
            return error_response(
                StatusCode::UNAUTHORIZED,
                "agent_wallet_token_unknown",
                "Agent wallet token is not recognized.",
            )
        }
        AgentConnectTokenLookup::Revoked { token_prefix } => {
            return error_response(
                StatusCode::UNAUTHORIZED,
                "agent_wallet_token_revoked",
                &format!("Agent wallet token {token_prefix}… has been revoked."),
            )
        }
        AgentConnectTokenLookup::Expired { token_prefix, .. } => {
            return error_response(
                StatusCode::UNAUTHORIZED,
                "agent_wallet_token_expired",
                &format!("Agent wallet token {token_prefix}… has expired."),
            )
        }
    };

    let policy = state
        .db
        .load_policy_for_connect_token(token.token_id)
        .await
        .map(|policy| PolicyEntry {
            id: policy.id,
            name: policy.name.clone(),
            version: policy.version,
            enabled: policy.enabled,
            unevaluated_rules: policy.rules.unknown_rule_names(),
        });

    let mut wallets: Vec<WalletEntry> = Vec::with_capacity(token.wallets.len());
    for wallet in &token.wallets {
        let spent_today = match state
            .db
            .get_wallet_paid_spend_today(&wallet.wallet_address)
            .await
        {
            Ok(value) => value.to_string().parse::<f64>().unwrap_or(0.0),
            Err(err) => {
                warn!(
                    "Failed to read wallet {} spent_today for /v1/me: {}",
                    wallet.wallet_address, err
                );
                0.0
            }
        };
        wallets.push(WalletEntry {
            address: wallet.wallet_address.clone(),
            status: wallet.wallet_status.clone(),
            cap_per_tx_usdc: wallet
                .per_tx_limit_usdc
                .to_string()
                .parse::<f64>()
                .unwrap_or(0.0),
            cap_per_day_usdc: wallet
                .daily_limit_usdc
                .to_string()
                .parse::<f64>()
                .unwrap_or(0.0),
            spent_today_usdc: spent_today,
        });
    }

    let mut rails: Vec<&'static str> = Vec::new();
    if !wallets.is_empty() {
        rails.push("x402");
    }

    let body = MeResponse {
        token_id: token.token_id,
        user_id: token.user_id,
        // Which credential this is, in the buyer's own words, and how long it
        // has left. A caller holding an opaque string has no other way to tell
        // one of its connections from another, so a "test credential" button
        // can only ever say "valid" without this.
        connection: ConnectionEntry {
            label: token.label.clone(),
            expires_at: token.expires_at.map(|at| at.to_rfc3339()),
        },
        rails,
        wallets,
        // Empty = unrestricted. Serialized either way (not skipped when empty)
        // so an agent can tell "no restriction" from "this gateway is too old
        // to report one" — the difference decides whether it should pre-filter
        // its own discovery results.
        allowed_domains: token.allowed_domains.clone(),
        // Documented in src/proxy/wallet.rs::select_prepaid_wallet_with_fundability.
        // Surfaced here so an agent knows what to expect when its connect
        // token authorizes more than one wallet. New connections in the
        // buyer portal are limited to one wallet — see buyer-portal commit
        // fead2d8 — making this strategy moot for those tokens.
        wallet_selection_strategy: "first_authorized_wallet_passing_caps_and_onchain_can_settle"
            .to_string(),
        policy,
    };

    let mut response = Json(body).into_response();
    response.headers_mut().insert(
        "Cache-Control",
        HeaderValue::from_static("no-store, max-age=0"),
    );
    response
}

// `GET /v1/me/sepa` and its `MeSepaResponse` used to sit here: the
// SEPA-rail half of this introspection surface, reporting a euro budget
// whose authoritative balance lived in the buyer portal (Mollie). Apiosk
// now keeps one payment rail — the buyer's Apiosk balance is debited in
// the App's Supabase and the treasury pays the provider over x402 — so
// there is no euro budget to report and the route is gone.

#[derive(Debug, Serialize)]
struct MeResponse {
    token_id: uuid::Uuid,
    user_id: uuid::Uuid,
    /// Who this credential is, for a caller that only holds the opaque string.
    connection: ConnectionEntry,
    /// Always `["x402"]` (or empty when the connection authorizes no wallet).
    /// The `"sepa"` value is retired along with the euro rail itself.
    rails: Vec<&'static str>,
    wallets: Vec<WalletEntry>,
    /// Destination hosts this connection may spend against. An empty array
    /// means unrestricted. A `*.` prefix also matches subdomains. Enforced on
    /// the API's upstream origin host, not on `gateway.apiosk.com`.
    allowed_domains: Vec<String>,
    wallet_selection_strategy: String,
    /// The purchase policy governing this connection.
    ///
    /// `null` means there is none — the wallet's compiled per-transaction and
    /// daily caps are the whole of the rules, which is exactly what this
    /// endpoint reported before policies existed. It is also `null` on a
    /// database that has not run migration 083, and the two are deliberately
    /// indistinguishable here: in both cases nothing beyond the caps is
    /// enforced, and reporting a policy the gateway cannot read would be worse
    /// than reporting none.
    #[serde(skip_serializing_if = "Option::is_none")]
    policy: Option<PolicyEntry>,
}

/// What an agent needs to say what it is allowed to do, without a second call.
#[derive(Debug, Serialize)]
struct PolicyEntry {
    id: uuid::Uuid,
    name: String,
    /// Bumped on every rules edit. An agent holding a cached description of its
    /// own limits can tell it has gone stale by watching this.
    version: i32,
    enabled: bool,
    /// Rules present in the policy that THIS gateway cannot evaluate, because
    /// they were shipped by the portal after this binary was built. Named
    /// rather than hidden: a buyer whose newest rule is not being applied
    /// deserves a way to find that out that is not "money left the account".
    unevaluated_rules: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ConnectionEntry {
    /// The buyer's name for it. `null` for a provider SDK key, which is named
    /// in its own table rather than here.
    label: Option<String>,
    /// RFC 3339. `null` only for credentials that don't expire on this table.
    expires_at: Option<String>,
}

#[derive(Debug, Serialize)]
struct WalletEntry {
    address: String,
    status: String,
    cap_per_tx_usdc: f64,
    cap_per_day_usdc: f64,
    spent_today_usdc: f64,
}

fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    let body: Value = serde_json::json!({
        "error": code,
        "message": message,
    });
    (status, Json(body)).into_response()
}
