//! Runtime discovery of which x402 networks the configured facilitator can
//! verify + settle, via `GET {facilitator}/supported`.
//!
//! The gateway emits x402Version:1 requirements, so a network is only enabled
//! when the facilitator advertises a **v1** `exact` kind for it (matched by
//! v1 name or CAIP-2 alias). Anything else — unknown networks, v2-only kinds
//! (CAIP-2 era chains), fetch failures — degrades to "Base only", which is
//! today's known-good behaviour. Payment collection can therefore never break
//! because of a mis-seeded network row or a facilitator capability change.
//!

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::config::Config;
use crate::payment::{apply_facilitator_auth, facilitator_endpoint_url};

/// Networks the gateway is willing to offer on the x402 v1 rail at all.
/// `base` is always-on (never gated on /supported — it is the revenue path
/// and known to work); the rest are enabled per the facilitator's response.
pub const X402_V1_NETWORKS: &[&str] = &["base", "polygon", "arbitrum"];

/// CAIP-2 aliases the facilitator may use instead of v1 names.
fn caip2_alias(network: &str) -> Option<&'static str> {
    match network {
        "base" => Some("eip155:8453"),
        "polygon" => Some("eip155:137"),
        "arbitrum" => Some("eip155:42161"),
        _ => None,
    }
}

#[derive(Debug, Clone, Default)]
pub struct NetworkSupport;

#[derive(Debug, Clone, Default)]
pub struct SupportedNetworks {
    by_name: HashMap<String, NetworkSupport>,
}

impl SupportedNetworks {
    /// Base-only fallback used until the first successful fetch (and after
    /// persistent failures).
    pub fn base_only() -> Self {
        let mut by_name = HashMap::new();
        by_name.insert("base".to_string(), NetworkSupport);
        Self { by_name }
    }

    pub fn support_for(&self, network: &str) -> Option<&NetworkSupport> {
        self.by_name.get(network)
    }

    #[cfg(test)]
    pub(crate) fn with_network_for_test(network: &str) -> Self {
        let mut nets = Self::base_only();
        nets.by_name.insert(network.to_string(), NetworkSupport);
        nets
    }
}

#[derive(Debug, Deserialize)]
struct SupportedKind {
    #[serde(default, rename = "x402Version")]
    x402_version: u8,
    #[serde(default)]
    scheme: String,
    #[serde(default)]
    network: String,
}

#[derive(Debug, Deserialize)]
struct SupportedResponse {
    #[serde(default)]
    kinds: Vec<SupportedKind>,
}

fn parse_supported(kinds: &[SupportedKind]) -> SupportedNetworks {
    let mut nets = SupportedNetworks::base_only();
    for name in X402_V1_NETWORKS.iter().filter(|n| **n != "base") {
        let alias = caip2_alias(name);
        let kind = kinds.iter().find(|k| {
            k.scheme == "exact"
                && k.x402_version == 1
                && (k.network == *name || alias.is_some_and(|a| k.network == a))
        });
        if kind.is_some() {
            nets.by_name.insert((*name).to_string(), NetworkSupport);
        }
    }
    nets
}

/// TTL-cached `/supported` snapshot. Cheap to clone into AppState.
pub struct FacilitatorCapsCache {
    inner: RwLock<Option<(Instant, Arc<SupportedNetworks>)>>,
    ttl: Duration,
}

impl Default for FacilitatorCapsCache {
    fn default() -> Self {
        Self {
            inner: RwLock::new(None),
            ttl: Duration::from_secs(15 * 60),
        }
    }
}

impl FacilitatorCapsCache {
    /// Current capability snapshot; refreshes from the facilitator when the
    /// cached copy is older than the TTL. Never errors: on any failure the
    /// previous snapshot is kept (or Base-only if there has never been one),
    /// so callers can treat the result as authoritative.
    pub async fn get(
        &self,
        http_client: &reqwest::Client,
        config: &Config,
        facilitator_url: &str,
    ) -> Arc<SupportedNetworks> {
        if let Some((at, nets)) = self.inner.read().await.as_ref() {
            if at.elapsed() < self.ttl {
                return Arc::clone(nets);
            }
        }

        let mut guard = self.inner.write().await;
        // Double-checked: another task may have refreshed while we waited.
        if let Some((at, nets)) = guard.as_ref() {
            if at.elapsed() < self.ttl {
                return Arc::clone(nets);
            }
        }

        let fetched = fetch_supported(http_client, config, facilitator_url).await;
        match fetched {
            Ok(nets) => {
                let mut names: Vec<&str> = nets.by_name.keys().map(String::as_str).collect();
                names.sort_unstable();
                info!("facilitator /supported x402-v1 networks: {:?}", names);
                let nets = Arc::new(nets);
                *guard = Some((Instant::now(), Arc::clone(&nets)));
                nets
            }
            Err(err) => {
                warn!(
                    "facilitator /supported fetch failed ({}); keeping previous capability snapshot",
                    err
                );
                match guard.as_mut() {
                    // Keep the stale snapshot but bump its timestamp so a
                    // flapping facilitator isn't hammered on every request.
                    Some((at, nets)) => {
                        *at = Instant::now();
                        Arc::clone(nets)
                    }
                    None => {
                        let nets = Arc::new(SupportedNetworks::base_only());
                        *guard = Some((Instant::now(), Arc::clone(&nets)));
                        nets
                    }
                }
            }
        }
    }
}

async fn fetch_supported(
    http_client: &reqwest::Client,
    config: &Config,
    facilitator_url: &str,
) -> Result<SupportedNetworks, String> {
    let endpoint =
        facilitator_endpoint_url(facilitator_url, "supported").map_err(|e| e.to_string())?;
    let builder = http_client.get(endpoint.clone());
    let builder =
        apply_facilitator_auth(builder, config, &endpoint, "GET").map_err(|e| e.to_string())?;
    let response = builder
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("status {}", response.status()));
    }
    let body: SupportedResponse = response.json().await.map_err(|e| e.to_string())?;
    // TEMP diagnostic for the x402 v1→v2 migration decision (see
    // per_api_execute_paths / x402scan discussion): log which protocol
    // versions the facilitator's /supported actually advertises today, since
    // public CDP docs don't state this. Remove once answered.
    let mut versions: Vec<u8> = body.kinds.iter().map(|k| k.x402_version).collect();
    versions.sort_unstable();
    versions.dedup();
    info!("facilitator /supported x402Version values advertised: {versions:?}");
    Ok(parse_supported(&body.kinds))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(
        version: u8,
        scheme: &str,
        network: &str,
        _extra: Option<serde_json::Value>,
    ) -> SupportedKind {
        SupportedKind {
            x402_version: version,
            scheme: scheme.to_string(),
            network: network.to_string(),
        }
    }

    #[test]
    fn base_is_always_supported_even_with_empty_kinds() {
        let nets = parse_supported(&[]);
        assert!(nets.support_for("base").is_some());
        assert!(nets.support_for("polygon").is_none());
    }

    #[test]
    fn v1_names_and_caip2_aliases_both_match() {
        let nets = parse_supported(&[
            kind(1, "exact", "polygon", None),
            kind(1, "exact", "eip155:42161", None),
        ]);
        assert!(nets.support_for("polygon").is_some());
        assert!(nets.support_for("arbitrum").is_some());
        assert!(nets.support_for("solana").is_none());
    }

    #[test]
    fn v2_only_kinds_do_not_enable_a_network() {
        let nets = parse_supported(&[kind(2, "exact", "eip155:137", None)]);
        assert!(nets.support_for("polygon").is_none());
    }

    #[test]
    fn non_exact_schemes_are_ignored() {
        let nets = parse_supported(&[kind(1, "upto", "polygon", None)]);
        assert!(nets.support_for("polygon").is_none());
    }

    /// Diagnostic only: hits the real CDP facilitator's `/supported` and
    /// prints the raw JSON so we can see whether it advertises any
    /// `x402Version: 2` kinds yet. `cargo test --ignored raw_cdp_supported_probe
    /// -- --nocapture`. Not a real assertion; delete once answered.
    #[tokio::test]
    #[ignore]
    async fn raw_cdp_supported_probe() {
        dotenvy::dotenv().ok();
        let config = crate::config::Config::from_env().expect("config");
        let client = reqwest::Client::new();
        let endpoint =
            crate::payment::facilitator_endpoint_url(&config.facilitator_url, "supported")
                .expect("endpoint");
        let builder = client.get(endpoint.clone());
        let builder = crate::payment::apply_facilitator_auth(builder, &config, &endpoint, "GET")
            .expect("auth");
        let resp = builder.send().await.expect("request");
        let status = resp.status();
        let body = resp.text().await.expect("body");
        println!("STATUS: {status}\nBODY: {body}");
    }
}
