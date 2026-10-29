//! Extra pre-payment checks for listings imported from the Coinbase Bazaar.
//!
//! `scripts/bazaar-sync.mjs` (gateway repo) imports x402 resources from the
//! public Bazaar discovery index as federated resell listings and marks them
//! `listing_metadata.resell.source = "bazaar-sync"`. Nobody at Apiosk reviewed
//! those sellers one by one, so the resell leg must not simply pay whatever
//! the live 402 asks for. Before any claim or signature, three facts are
//! checked against what the sync recorded and what an operator decided:
//!
//! * the host is not on the `x402_host_policy` deny list;
//! * the demanded amount does not exceed that host's `max_price_usdc`, when
//!   one is set (the listing-price spread guard in `resell.rs` still applies
//!   on top of this);
//! * the live offer is still an exact USDC payment on Base, the only terms
//!   the import admits;
//! * the live `payTo` is one the sync recorded for this listing
//!   (`listing_metadata.bazaar.pay_to`). A changed payee means the listing no
//!   longer describes who is being paid, so the call is refused until a new
//!   sync run records it.
//!
//! Every check fails CLOSED: a missing or unreadable `pay_to`, or a policy
//! lookup that errors, refuses the payment. Listings that did not come from
//! the Bazaar sync are never affected and cost no database read.

use ethers_core::types::Address;
use serde_json::Value;
use std::str::FromStr;

/// The `listing_metadata.resell.source` the Bazaar sync writes.
pub(super) const BAZAAR_SOURCE: &str = "bazaar-sync";

/// The operator's `x402_host_policy` decision for one host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Policy {
    Deny,
    Allow { max_price_atomic: Option<u128> },
    NotListed,
}

/// Why the resell leg refused a Bazaar listing. `code` is logged; `message`
/// is safe to show a buyer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Refusal {
    pub code: &'static str,
    pub message: &'static str,
}

const HOST_DENIED: Refusal = Refusal {
    code: "bazaar_host_denied",
    message: "This provider is not available through Apiosk.",
};
const PRICE_ABOVE_HOST_CAP: Refusal = Refusal {
    code: "bazaar_price_above_host_cap",
    message: "This request exceeds the spending limit set for this provider.",
};
const PAYEE_UNKNOWN: Refusal = Refusal {
    code: "bazaar_payee_unknown",
    message: "This provider's payment details could not be confirmed. It is paused until it is re-synced.",
};
const PAYEE_CHANGED: Refusal = Refusal {
    code: "bazaar_payee_changed",
    message: "This provider's payment address changed. It is paused until it is re-synced.",
};
const OFFER_NOT_EXACT_USDC: Refusal = Refusal {
    code: "bazaar_offer_not_exact_usdc",
    message: "This provider no longer offers the payment terms Apiosk imported. It is paused until it is re-synced.",
};
/// USDC on Base: the only asset the Bazaar import admits (scripts/bazaar-sync.mjs).
const BASE_USDC: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
const UNPARSEABLE_AMOUNT: Refusal = Refusal {
    code: "bazaar_amount_unreadable",
    message: "The upstream payment challenge could not be understood.",
};
pub(super) const POLICY_UNAVAILABLE: Refusal = Refusal {
    code: "bazaar_policy_unavailable",
    message: "This API is temporarily unavailable.",
};

/// True for a listing the Bazaar sync imported.
pub(super) fn is_bazaar_listing(listing_metadata: &Value) -> bool {
    listing_metadata
        .get("resell")
        .and_then(|r| r.get("source"))
        .and_then(Value::as_str)
        == Some(BAZAAR_SOURCE)
}

/// The payees the sync recorded: a string or an array of strings. `None` when
/// absent, empty or when any entry is not an address — a partly unreadable
/// record is not evidence of who should be paid.
fn recorded_payees(listing_metadata: &Value) -> Option<Vec<Address>> {
    let raw = listing_metadata.get("bazaar")?.get("pay_to")?;
    let values: Vec<&Value> = match raw {
        Value::String(_) => vec![raw],
        Value::Array(items) => items.iter().collect(),
        _ => return None,
    };
    if values.is_empty() {
        return None;
    }
    values
        .into_iter()
        .map(|v| v.as_str().and_then(|s| Address::from_str(s.trim()).ok()))
        .collect()
}

/// Decide whether the resell leg may pay this live requirement. Pure: the
/// caller supplies the host policy (see [`policy_for`]).
pub(super) fn check(
    listing_metadata: &Value,
    policy: Policy,
    requirement: &super::resell::UpstreamRequirement,
) -> Result<(), Refusal> {
    if !is_bazaar_listing(listing_metadata) {
        return Ok(());
    }
    match policy {
        Policy::Deny => return Err(HOST_DENIED),
        Policy::Allow {
            max_price_atomic: Some(cap),
        } => {
            let amount: u128 = requirement
                .value_atomic
                .trim()
                .parse()
                .map_err(|_| UNPARSEABLE_AMOUNT)?;
            if amount > cap {
                return Err(PRICE_ABOVE_HOST_CAP);
            }
        }
        Policy::Allow {
            max_price_atomic: None,
        }
        | Policy::NotListed => {}
    }
    // `parse_upstream_402` takes the first entry on a payable chain, whatever
    // its scheme or asset. The import admitted an exact USDC-on-Base offer
    // only; anything else is not what was listed.
    if requirement.scheme != "exact"
        || requirement.chain_id != 8453
        || Some(requirement.asset) != Address::from_str(BASE_USDC).ok()
    {
        return Err(OFFER_NOT_EXACT_USDC);
    }
    let payees = recorded_payees(listing_metadata).ok_or(PAYEE_UNKNOWN)?;
    // Parsed addresses compare the twenty bytes, so checksummed and
    // lowercase spellings of the same payee are equal.
    if payees.contains(&requirement.pay_to) {
        Ok(())
    } else {
        Err(PAYEE_CHANGED)
    }
}

/// The lowercase host of an upstream URL, without port.
pub(super) fn host_of(url: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()?
        .host_str()
        .map(|h| h.trim_end_matches('.').to_ascii_lowercase())
        .filter(|h| !h.is_empty())
}

/// Read the operator's host policy, as `external_fetch::host_policy` does. A
/// database error is `Err`: the caller refuses rather than paying unchecked.
pub(super) async fn policy_for(
    state: &std::sync::Arc<crate::AppState>,
    host: &str,
) -> Result<Policy, sqlx::Error> {
    let row: Option<(String, Option<bigdecimal::BigDecimal>)> =
        sqlx::query_as("SELECT policy, max_price_usdc FROM x402_host_policy WHERE host = $1")
            .bind(host)
            .fetch_optional(&state.db.pool)
            .await?;
    Ok(match row {
        Some((policy, _)) if policy == "deny" => Policy::Deny,
        Some((_, max_price)) => Policy::Allow {
            max_price_atomic: max_price
                .as_ref()
                .and_then(super::resell::usd_to_atomic_u128),
        },
        None => Policy::NotListed,
    })
}

/// Run every Bazaar check for one resell payment. Non-Bazaar listings return
/// `Ok` without touching the database.
pub(super) async fn guard(
    state: &std::sync::Arc<crate::AppState>,
    listing_metadata: Option<&Value>,
    origin_url: &str,
    requirement: &super::resell::UpstreamRequirement,
) -> Result<(), Refusal> {
    let Some(metadata) = listing_metadata.filter(|m| is_bazaar_listing(m)) else {
        return Ok(());
    };
    let host = host_of(origin_url).ok_or(POLICY_UNAVAILABLE)?;
    let policy = policy_for(state, &host)
        .await
        .map_err(|_| POLICY_UNAVAILABLE)?;
    check(metadata, policy, requirement)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::resell::UpstreamRequirement;
    use serde_json::json;

    const PAYEE: &str = "0x3e7c9cA818f713F19e126E3B7B37B47dC1411570";
    const OTHER: &str = "0x52E29e0d2Aa49bfBfC548C0A9F2196F4aa51f3ea";

    fn requirement(amount: &str, pay_to: &str) -> UpstreamRequirement {
        UpstreamRequirement {
            scheme: "exact".to_string(),
            network: "base".to_string(),
            chain_id: 8453,
            pay_to: Address::from_str(pay_to).unwrap(),
            asset: Address::from_str("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913").unwrap(),
            pay_to_raw: pay_to.to_string(),
            asset_raw: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".to_string(),
            value_atomic: amount.to_string(),
            token_name: "USD Coin".to_string(),
            token_version: "2".to_string(),
            max_timeout_secs: 300,
            x402_version: 2,
            accepted_raw: json!({
                "scheme": "exact",
                "network": "eip155:8453",
                "amount": amount,
                "payTo": pay_to,
                "asset": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
                "extra": { "name": "USD Coin", "version": "2" }
            }),
            resource_raw: None,
        }
    }

    fn bazaar(pay_to: Value) -> Value {
        json!({
            "resell": {"enabled": true, "source": "bazaar-sync"},
            "provider_network": "Coinbase Bazaar",
            "bazaar": {"pay_to": pay_to, "run_id": "r1"}
        })
    }

    #[test]
    fn a_listing_the_sync_did_not_import_is_untouched() {
        let manual = json!({"resell": {"enabled": true, "source": "x402-manifest-import"}});
        // Even a deny and a foreign payee change nothing: not a Bazaar listing.
        assert_eq!(check(&manual, Policy::Deny, &requirement("5000", OTHER)), Ok(()));
        assert_eq!(check(&json!({}), Policy::Deny, &requirement("5000", OTHER)), Ok(()));
        assert!(!is_bazaar_listing(&Value::Null));
    }

    #[test]
    fn a_denied_host_is_refused() {
        let meta = bazaar(json!([PAYEE.to_lowercase()]));
        assert_eq!(check(&meta, Policy::Deny, &requirement("1000", PAYEE)), Err(HOST_DENIED));
    }

    #[test]
    fn the_host_price_cap_is_inclusive() {
        let meta = bazaar(json!([PAYEE.to_lowercase()]));
        let cap = Policy::Allow { max_price_atomic: Some(10_000) };
        assert_eq!(check(&meta, cap, &requirement("10000", PAYEE)), Ok(()));
        assert_eq!(check(&meta, cap, &requirement("10001", PAYEE)), Err(PRICE_ABOVE_HOST_CAP));
        assert_eq!(check(&meta, cap, &requirement("abc", PAYEE)), Err(UNPARSEABLE_AMOUNT));
        let uncapped = Policy::Allow { max_price_atomic: None };
        assert_eq!(check(&meta, uncapped, &requirement("999999", PAYEE)), Ok(()));
        assert_eq!(check(&meta, Policy::NotListed, &requirement("999999", PAYEE)), Ok(()));
    }

    #[test]
    fn a_changed_payee_is_refused() {
        let meta = bazaar(json!([PAYEE.to_lowercase()]));
        assert_eq!(
            check(&meta, Policy::NotListed, &requirement("1000", OTHER)),
            Err(PAYEE_CHANGED)
        );
    }

    #[test]
    fn the_recorded_payee_may_be_a_string_or_an_array_in_any_case() {
        for recorded in [
            json!(PAYEE),
            json!(PAYEE.to_lowercase()),
            json!(PAYEE.to_uppercase().replacen("0X", "0x", 1)),
            json!([OTHER.to_lowercase(), PAYEE.to_lowercase()]),
        ] {
            let meta = bazaar(recorded.clone());
            assert_eq!(
                check(&meta, Policy::NotListed, &requirement("1000", PAYEE)),
                Ok(()),
                "{recorded}"
            );
        }
    }

    #[test]
    fn a_missing_or_unreadable_payee_fails_closed() {
        for recorded in [json!(null), json!([]), json!(""), json!(["not-an-address"]), json!([PAYEE, 7]), json!({"a": PAYEE})] {
            let meta = bazaar(recorded.clone());
            assert_eq!(
                check(&meta, Policy::NotListed, &requirement("1000", PAYEE)),
                Err(PAYEE_UNKNOWN),
                "{recorded}"
            );
        }
        let no_bazaar_block = json!({"resell": {"enabled": true, "source": "bazaar-sync"}});
        assert_eq!(
            check(&no_bazaar_block, Policy::NotListed, &requirement("1000", PAYEE)),
            Err(PAYEE_UNKNOWN)
        );
    }

    #[test]
    fn only_the_imported_exact_usdc_on_base_offer_is_paid() {
        let meta = bazaar(json!([PAYEE.to_lowercase()]));
        let mut batch = requirement("1000", PAYEE);
        batch.scheme = "batch-settlement".to_string();
        assert_eq!(check(&meta, Policy::NotListed, &batch), Err(OFFER_NOT_EXACT_USDC));
        let mut other_asset = requirement("1000", PAYEE);
        other_asset.asset = Address::from_str(OTHER).unwrap();
        assert_eq!(check(&meta, Policy::NotListed, &other_asset), Err(OFFER_NOT_EXACT_USDC));
        let mut polygon = requirement("1000", PAYEE);
        polygon.chain_id = 137;
        assert_eq!(check(&meta, Policy::NotListed, &polygon), Err(OFFER_NOT_EXACT_USDC));
        // Not a Bazaar listing: untouched, as before.
        assert_eq!(check(&json!({}), Policy::NotListed, &batch), Ok(()));
    }

    #[test]
    fn the_host_is_the_lowercase_name_without_port() {
        assert_eq!(host_of("https://API.Example.com:8443/v1/x").as_deref(), Some("api.example.com"));
        assert_eq!(host_of("https://api.example.com").as_deref(), Some("api.example.com"));
        assert_eq!(host_of("not a url"), None);
    }
}
