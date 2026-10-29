//! Is there money in the wallet that pays providers?
//!
//! ---------------------------------------------------------------------------
//! Why this is a background task and not a comment in a runbook
//! ---------------------------------------------------------------------------
//!
//! The treasury signs an EIP-3009 authorization; the provider's facilitator
//! redeems it. Facilitators verify the payer can cover the amount BEFORE they
//! serve, so a treasury that has run dry does not fail as "out of funds" — it
//! fails as the provider answering 402 to a request we just paid for, with no
//! body, no code, and no mention of a balance anywhere in it.
//!
//! That is indistinguishable, from the outside, from a price that moved, a
//! signature the facilitator would not verify, or a dialect mismatch. It cost
//! a day to tell apart once. It should never cost that again, and the cheapest
//! way to make sure is to notice the balance falling before a user does.
//!
//! Read-only. This watches; it never moves money. Topping the treasury up is a
//! decision with a human on the end of it, and an auto-funder that can drain a
//! bank account because a balance read flickered is a worse bug than the one
//! it prevents.

use std::time::Duration;

use tracing::{error, info, warn};

/// USDC on Base mainnet. The only asset the payer legs can settle in today —
/// `external_fetch.rs` hardcodes chain 8453 as the sole payable rail.
const USDC_BASE_MAINNET: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
const BASE_MAINNET_RPC: &str = "https://mainnet.base.org";

/// `balanceOf(address)`.
const BALANCE_OF_SELECTOR: &str = "70a08231";

/// Atomic USDC (6 dp) below which the balance is called low.
///
/// Default 1.00 USDC. Not a number with a theory behind it — it is "more than
/// a handful of calls, less than a rounding error" — and it is an env var
/// because the right figure is a function of traffic, which changes.
const DEFAULT_LOW_WATER_ATOMIC: u128 = 1_000_000;

/// How often to look. Slow on purpose: this is a slope, not an event, and a
/// public RPC endpoint asked every few seconds is a public RPC endpoint that
/// starts refusing.
const DEFAULT_INTERVAL_SECS: u64 = 900;

fn env_u128(name: &str, default: u128) -> u128 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| *v >= 0.0)
        .map(|usd| (usd * 1_000_000.0) as u128)
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

/// `4000000` → `"4.000000"`. Whole-unit USDC, for a human reading a log line.
fn format_usdc(atomic: u128) -> String {
    format!("{}.{:06}", atomic / 1_000_000, atomic % 1_000_000)
}

/// One `eth_call` to `balanceOf`. `None` when the RPC could not be reached or
/// answered something unreadable — which is NOT a zero balance, and must never
/// be reported as one. A watch that cries "empty treasury" every time a public
/// RPC hiccups is a watch that gets muted, and then it is not a watch.
async fn read_balance_atomic(client: &reqwest::Client, address: &str) -> Option<u128> {
    let bare = address.trim_start_matches("0x").to_ascii_lowercase();
    let data = format!("0x{BALANCE_OF_SELECTOR}{bare:0>64}");

    let response = client
        .post(BASE_MAINNET_RPC)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_call",
            "params": [{ "to": USDC_BASE_MAINNET, "data": data }, "latest"],
        }))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .ok()?;

    let body: serde_json::Value = response.json().await.ok()?;
    let hex = body.get("result")?.as_str()?.trim_start_matches("0x");
    u128::from_str_radix(hex, 16).ok()
}

/// Watch the treasury for as long as the process lives.
///
/// Spawned once, by `CommerceRuntime`, and only when a payer leg is actually enabled —
/// there is nothing to keep funded otherwise. Its task is aborted on runtime drop.
pub fn spawn(client: reqwest::Client, address: String, tasks: &crate::background::BackgroundTasks) {
    let low_water = env_u128("TREASURY_LOW_WATER_USDC", DEFAULT_LOW_WATER_ATOMIC);
    let interval = env_u64("TREASURY_WATCH_INTERVAL_SECS", DEFAULT_INTERVAL_SECS);

    tasks.spawn(async move {
        loop {
            match read_balance_atomic(&client, &address).await {
                Some(0) => error!(
                    treasury = %address,
                    "TREASURY IS EMPTY: 0 USDC on Base. Every paid call will now fail as a \
                     provider 402 with no explanation — the facilitator refuses before serving. \
                     Fund {address} with USDC on Base mainnet."
                ),
                Some(balance) if balance < low_water => warn!(
                    treasury = %address,
                    balance_usdc = %format_usdc(balance),
                    low_water_usdc = %format_usdc(low_water),
                    "Treasury is low. When it empties, paid calls fail as unexplained provider \
                     402s rather than as an out-of-funds error."
                ),
                Some(balance) => info!(
                    treasury = %address,
                    balance_usdc = %format_usdc(balance),
                    "Treasury balance OK"
                ),
                // Reported, and deliberately not escalated. See the comment on
                // `read_balance_atomic`.
                None => warn!(
                    treasury = %address,
                    "Could not read the treasury balance from {BASE_MAINNET_RPC}; will retry. \
                     This is a failed read, not a zero balance."
                ),
            }
            tokio::time::sleep(Duration::from_secs(interval)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_units_and_six_decimals_both_survive_the_formatting() {
        assert_eq!(format_usdc(5_000_000), "5.000000");
        assert_eq!(format_usdc(0), "0.000000");
        // A tenth of a cent is a real price in this product; it must not print
        // as 0.00 in the line that decides whether somebody tops the wallet up.
        assert_eq!(format_usdc(1_000), "0.001000");
        assert_eq!(format_usdc(4_930_000), "4.930000");
    }

    #[test]
    fn the_low_water_mark_reads_usd_and_stores_atomic() {
        std::env::set_var("TREASURY_TEST_LOW_WATER", "2.5");
        assert_eq!(env_u128("TREASURY_TEST_LOW_WATER", 0), 2_500_000);
        std::env::remove_var("TREASURY_TEST_LOW_WATER");
        // Unset, unparseable and negative all fall back rather than disabling
        // the alarm by accident.
        assert_eq!(env_u128("TREASURY_TEST_ABSENT", 1_000_000), 1_000_000);
        std::env::set_var("TREASURY_TEST_JUNK", "not a number");
        assert_eq!(env_u128("TREASURY_TEST_JUNK", 1_000_000), 1_000_000);
        std::env::set_var("TREASURY_TEST_NEG", "-5");
        assert_eq!(env_u128("TREASURY_TEST_NEG", 1_000_000), 1_000_000);
        std::env::remove_var("TREASURY_TEST_JUNK");
        std::env::remove_var("TREASURY_TEST_NEG");
    }

    #[test]
    fn a_zero_interval_falls_back_rather_than_spinning_the_loop() {
        std::env::set_var("TREASURY_TEST_INTERVAL", "0");
        assert_eq!(env_u64("TREASURY_TEST_INTERVAL", 900), 900);
        std::env::remove_var("TREASURY_TEST_INTERVAL");
    }
}
