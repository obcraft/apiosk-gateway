//! Which chain family this process is allowed to touch.
//!
//! Three rails used to move money, each picking a chain from its own env var
//! (`SETTLEMENT_CHAIN_ID`, `DEVELOPER_PAYOUT_CHAIN_ID`, `OFFRAMP_CHAIN_ID`),
//! and each defaulting to Base **mainnet** independently. A developer running
//! the gateway on their laptop with a populated `.env` therefore settled real
//! USDC against the real contract — nothing in the process distinguished a
//! laptop from production.
//!
//! All three of those rails are gone. Apiosk keeps one: the buyer's Apiosk
//! balance is debited in the App's Supabase and the treasury pays the
//! provider's own x402, signing an authorisation rather than sending a
//! transaction. What survives here is the environment signal itself, which the
//! gateway still logs at startup and the MCP proxy still reads.
//!
//! This module is the one place that decides, and it decides from where the
//! process is running rather than from a flag someone has to remember:
//!
//!   * on Fly (`FLY_APP_NAME` and friends are injected by the platform)
//!     → [`NetworkEnv::Mainnet`]
//!   * anywhere else, including localhost and CI → [`NetworkEnv::Testnet`]
//!
//! `APIOSK_NETWORK_ENV=mainnet|testnet` overrides both directions, which is how
//! a staging Fly app runs on testnet or a local integration test deliberately
//! targets mainnet. It is the only escape hatch; there is no per-rail bypass.
//!
//! The defaults alone are not enough — a stale `.env` overrides a default. So
//! [`guard_chain_id`] additionally *rejects* a chain that contradicts the
//! environment, and the callers refuse to start on that error.

// This module used to be shared verbatim by two binaries (the gateway and the
// payout worker) via `#[path]`, each using a different subset, which is why the
// allow is here. The payout worker is gone, and with it the callers of
// `guard_chain_id` / `guard_rpc_url` / the chain defaults: the surviving
// payment path signs an authorisation and never picks an RPC. They are kept —
// tested, cheap, and the right guard for anything that touches a chain again —
// rather than deleted along with the rails that happened to be their only
// callers. `NetworkEnv::detect`, `label` and `runs_on_fly` are live.
#![allow(dead_code)]

use anyhow::{bail, Result};

pub const BASE_MAINNET_CHAIN_ID: u64 = 8453;
pub const BASE_SEPOLIA_CHAIN_ID: u64 = 84532;

const BASE_MAINNET_RPC_URL: &str = "https://mainnet.base.org";
const BASE_SEPOLIA_RPC_URL: &str = "https://sepolia.base.org";

/// Mainnet chain ids across every rail the gateway settles or pays out on.
/// Base, Polygon, Arbitrum, Ethereum, Optimism.
const MAINNET_CHAIN_IDS: &[u64] = &[1, 10, 137, 8453, 42161];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkEnv {
    /// Real money. Only ever selected in production (or by explicit override).
    Mainnet,
    /// Play money. The default everywhere that is not production.
    Testnet,
}

impl NetworkEnv {
    /// Resolve the environment. Explicit override first, then the platform.
    pub fn detect() -> Self {
        match std::env::var("APIOSK_NETWORK_ENV")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "mainnet" | "production" | "prod" | "live" => Self::Mainnet,
            "testnet" | "local" | "development" | "dev" | "sepolia" => Self::Testnet,
            // Unset or unrecognised: infer from the platform. Defaulting to
            // Testnet here is deliberate — an unrecognised value must never
            // silently unlock real funds.
            _ => {
                if runs_on_fly() {
                    Self::Mainnet
                } else {
                    Self::Testnet
                }
            }
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Mainnet => "mainnet",
            Self::Testnet => "testnet",
        }
    }

    /// Default chain when the rail's own env var is unset.
    pub fn default_chain_id(self) -> u64 {
        match self {
            Self::Mainnet => BASE_MAINNET_CHAIN_ID,
            Self::Testnet => BASE_SEPOLIA_CHAIN_ID,
        }
    }

    /// Default RPC endpoint when the rail's own env var is unset.
    pub fn default_rpc_url(self) -> &'static str {
        match self {
            Self::Mainnet => BASE_MAINNET_RPC_URL,
            Self::Testnet => BASE_SEPOLIA_RPC_URL,
        }
    }
}

pub fn is_mainnet_chain_id(chain_id: u64) -> bool {
    MAINNET_CHAIN_IDS.contains(&chain_id)
}

/// Refuse a chain id that contradicts the environment.
///
/// `component` names the env var at fault so the error tells the operator
/// exactly what to change.
pub fn guard_chain_id(component: &str, chain_id: u64) -> Result<()> {
    let env = NetworkEnv::detect();
    let chain_is_mainnet = is_mainnet_chain_id(chain_id);

    match (env, chain_is_mainnet) {
        (NetworkEnv::Testnet, true) => bail!(
            "Refusing to start: {component}={chain_id} is a MAINNET chain, but this process is \
             running outside production so it is restricted to testnet. Moving real funds from a \
             local or CI run is almost never intended. Use {} (Base Sepolia), or set \
             APIOSK_NETWORK_ENV=mainnet if you genuinely mean to touch real money from here.",
            BASE_SEPOLIA_CHAIN_ID
        ),
        (NetworkEnv::Mainnet, false) => bail!(
            "Refusing to start: {component}={chain_id} is a TESTNET chain, but this process is \
             running in production. Payments would settle in play money while the catalog \
             advertises real prices. Use {} (Base mainnet), or set APIOSK_NETWORK_ENV=testnet if \
             this deployment is deliberately a staging environment.",
            BASE_MAINNET_CHAIN_ID
        ),
        _ => Ok(()),
    }
}

/// Catch the copy-paste error of a chain id and an RPC URL that disagree.
/// Only fires on the two hosts we can recognise with certainty.
pub fn guard_rpc_url(component: &str, chain_id: u64, rpc_url: &str) -> Result<()> {
    let url = rpc_url.to_ascii_lowercase();
    let looks_testnet =
        url.contains("sepolia") || url.contains("testnet") || url.contains("goerli");
    let looks_mainnet = url.contains("mainnet.base.org") || url.contains("base-mainnet");

    if is_mainnet_chain_id(chain_id) && looks_testnet {
        bail!(
            "Refusing to start: {component} chain {chain_id} is mainnet but its RPC URL \
             ({rpc_url}) points at a testnet. One of the two is wrong."
        );
    }
    if !is_mainnet_chain_id(chain_id) && looks_mainnet {
        bail!(
            "Refusing to start: {component} chain {chain_id} is a testnet but its RPC URL \
             ({rpc_url}) points at mainnet. One of the two is wrong."
        );
    }
    Ok(())
}

/// Fly injects these into every machine; their absence means we are not in
/// production. Used as the environment signal, and by the MCP proxy to decide
/// whether the `.internal` address is reachable.
pub fn runs_on_fly() -> bool {
    [
        "FLY_APP_NAME",
        "FLY_MACHINE_ID",
        "FLY_ALLOC_ID",
        "FLY_REGION",
    ]
    .iter()
    .any(|name| std::env::var_os(name).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_ids_classify_correctly() {
        assert!(is_mainnet_chain_id(BASE_MAINNET_CHAIN_ID));
        assert!(!is_mainnet_chain_id(BASE_SEPOLIA_CHAIN_ID));
        // The other rails the gateway settles on are mainnet too.
        assert!(is_mainnet_chain_id(137));
        assert!(is_mainnet_chain_id(42161));
        // An unknown chain is treated as non-mainnet: it cannot be a rail we
        // move real money on, since every such rail is enumerated above.
        assert!(!is_mainnet_chain_id(31337));
    }

    #[test]
    fn defaults_follow_the_environment() {
        assert_eq!(
            NetworkEnv::Testnet.default_chain_id(),
            BASE_SEPOLIA_CHAIN_ID
        );
        assert_eq!(
            NetworkEnv::Mainnet.default_chain_id(),
            BASE_MAINNET_CHAIN_ID
        );
        assert!(NetworkEnv::Testnet.default_rpc_url().contains("sepolia"));
        assert!(NetworkEnv::Mainnet.default_rpc_url().contains("mainnet"));
    }

    #[test]
    fn rpc_url_must_agree_with_its_chain() {
        // The realistic mistake: chain flipped to Sepolia, RPC left on mainnet.
        assert!(guard_rpc_url("SETTLEMENT", BASE_SEPOLIA_CHAIN_ID, BASE_MAINNET_RPC_URL).is_err());
        assert!(guard_rpc_url("SETTLEMENT", BASE_MAINNET_CHAIN_ID, BASE_SEPOLIA_RPC_URL).is_err());
        // Matching pairs pass.
        assert!(guard_rpc_url("SETTLEMENT", BASE_MAINNET_CHAIN_ID, BASE_MAINNET_RPC_URL).is_ok());
        assert!(guard_rpc_url("SETTLEMENT", BASE_SEPOLIA_CHAIN_ID, BASE_SEPOLIA_RPC_URL).is_ok());
        // An unrecognised host is not evidence of a mismatch either way.
        assert!(guard_rpc_url("SETTLEMENT", BASE_MAINNET_CHAIN_ID, "https://rpc.example").is_ok());
    }
}
