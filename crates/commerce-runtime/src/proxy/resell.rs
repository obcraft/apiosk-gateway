//! Resell leg — pay an upstream's own x402 (HTTP 402) on forward.
//!
//! Some listings resell an API that is ITSELF x402-native (e.g. OpenWebNinja).
//! When the gateway forwards such a request the upstream answers 402. To serve
//! the data, the gateway must PAY that upstream: it signs an EIP-3009
//! `TransferWithAuthorization` from the platform payer wallet to the upstream's
//! `payTo` for the EXACT amount the upstream demands, base64-encodes the x402
//! proof into an `X-Payment` header, and retries the request once. The provider
//! is paid inline (their own x402 pull); the agent is charged the listing price
//! (provider price + fee) by the existing seller-side settle path; Apiosk keeps
//! the spread.
//!
//! SAFETY — this moves real USDC on every call:
//! - Gated on `listing_metadata.resell.enabled`; fails CLOSED when the payer key
//!   or caps are unset (the caller must 402/502 the agent, never forward unpaid).
//! - Enforces a per-tx cap AND a spread guard (upstream amount ≤ listing price)
//!   BEFORE signing, so a malicious upstream can't drain the payer wallet.
//! - The digest is produced by ethers' tested EIP-712 encoder (`TypedData`), fed
//!   the exact typed-data JSON the browser signer uses — no hand-rolled keccak.
//! - The nonce is deterministic per `request_id` so a retry re-signs the SAME
//!   authorization (the token contract rejects the replay), and the caller MUST
//!   still hold a durable per-`request_id` double-pay claim before calling here.

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use bigdecimal::BigDecimal;
use ethers_core::types::transaction::eip712::{Eip712, TypedData};
use ethers_core::types::{Address, Signature, H256};
use ethers_core::utils::keccak256;
use ethers_signers::LocalWallet;
use serde_json::{json, Value};
use std::str::FromStr;
use uuid::Uuid;

/// What a resold call actually cost, and who fronted it.
///
/// This travels from the resell leg to settlement because the two must agree on
/// one fact: **the provider has already been paid**, inline, by their own x402
/// pull from the payer wallet. The settlement step must therefore reimburse the
/// PAYER, not pay the provider a second time.
///
/// Without this the provider is paid twice — once by the resell leg and again by
/// `queue_auto_payout_request`, which pays `apis.wallet_address`, and on a
/// federated listing that column holds the provider's own payTo.
#[derive(Debug, Clone)]
pub struct ResellOutcome {
    /// Confirmed on-chain transfer returned by the selected x402 provider.
    pub transaction: String,
    /// Atomic USDC actually paid to the upstream. This is the provider's own
    /// asking price, and exactly what the bridge is owed back.
    pub upstream_atomic: u128,
    /// The payer (bridge) wallet that fronted it, `0x…` lowercase hex.
    pub payer_address: String,
}

/// Decode the v1/v2 settlement receipt; HTTP success alone is not payment evidence.
pub fn payment_transaction(header: Option<&str>) -> Option<String> {
    let raw = header.filter(|h| h.len() <= 16384)?;
    let bytes = BASE64.decode(raw).ok()?;
    let receipt: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    if receipt.get("success").and_then(|v| v.as_bool()) != Some(true) {
        return None;
    }
    let tx = receipt
        .get("transaction")
        .or_else(|| receipt.get("transactionHash"))?
        .as_str()?;
    (tx.len() == 66 && tx.starts_with("0x") && tx[2..].bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| tx.to_owned())
}

/// The platform payer that funds resell (paying upstream x402s). Built once at
/// startup from `RESELL_PAYER_PRIVATE_KEY`; absent ⇒ resell is DISABLED and any
/// resell-enabled listing must fail closed (never forward unpaid). Separate from
/// the settlement operator wallet so resell does not require the settle contract.
pub struct ResellPayer {
    wallet: LocalWallet,
    /// Max USDC (atomic, 6-dp) the gateway will pay a single upstream call.
    pub cap_per_tx_atomic: Option<u128>,
    /// Max USDC (atomic) the gateway will pay upstreams in a rolling day.
    pub cap_per_day_atomic: Option<u128>,
}

impl ResellPayer {
    /// Load from env. Returns None (resell disabled) when the key is unset or
    /// unparseable — fail closed, never panic the process.
    pub fn from_env() -> Option<Self> {
        // Prefer a dedicated resell key; otherwise reuse the existing (funded)
        // settlement operator wallet — the same hot wallet the gateway already
        // signs with — so resell needs no new secret.
        let Some(key) = treasury_key() else {
            tracing::info!(
                "Resell leg OFF: {TREASURY_KEY_VAR} is not set. Paid calls to reviewed \
                 catalogue APIs cannot pay their provider."
            );
            return None;
        };
        // The chain id only matters for EIP-155 tx signing; resell signs off-
        // chain via sign_hash, so no with_chain_id is needed.
        let wallet = match LocalWallet::from_str(&key) {
            Ok(w) => w,
            Err(_) => {
                tracing::error!(
                    "{TREASURY_KEY_VAR} is set but is not a valid private key; resell disabled"
                );
                return None;
            }
        };
        Some(Self {
            wallet,
            cap_per_tx_atomic: env_usd_atomic("RESELL_CAP_PER_TX_USDC"),
            cap_per_day_atomic: env_usd_atomic("RESELL_CAP_PER_DAY_USDC"),
        })
    }

    pub fn wallet(&self) -> &LocalWallet {
        &self.wallet
    }

    pub fn address(&self) -> Address {
        ethers_signers::Signer::address(&self.wallet)
    }
}

/// The env var holding the ONE key that pays providers.
pub const TREASURY_KEY_VAR: &str = "TREASURY_PRIVATE_KEY";

/// The names this used to be spelled, kept only to be complained about.
const RETIRED_KEY_VARS: [&str; 4] = [
    "RESELL_PAYER_PRIVATE_KEY",
    "EXTERNAL_PAYER_PRIVATE_KEY",
    "DEVELOPER_PAYOUT_OPERATOR_PRIVATE_KEY",
    "SETTLEMENT_OPERATOR_PRIVATE_KEY",
];

/// The treasury key, or nothing.
///
/// ONE WALLET PAYS PROVIDERS, AND IT IS NAMED HERE.
///
/// This used to be two independent fallback chains — two deep on the reviewed
/// route, four deep on the external one — resolved silently at startup. Which
/// name answered decided which address had to hold USDC on Base, the two
/// routes could and did land on different wallets, and nothing anywhere said
/// so. "Which wallet is used for paying?" had no answer short of reading the
/// deploy's environment and re-deriving an address by hand, and the wallet
/// somebody was watching was not necessarily the wallet that signed.
///
/// So: one name, no fallback. A deploy that still sets only an old name is
/// not quietly downgraded to a disabled feature — it is told, by name, at
/// startup, which is the one moment somebody can act on it.
///
/// The retired names are still read by the subsystems they were actually
/// named for — `settlement.rs` and the developer payout worker sign their own
/// on-chain transactions with their own operator keys. Those are different
/// jobs on different contracts and are deliberately NOT consolidated here;
/// only the x402 provider-payment leg is.
pub fn treasury_key() -> Option<String> {
    if let Some(key) = std::env::var(TREASURY_KEY_VAR)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return Some(key);
    }

    for retired in RETIRED_KEY_VARS {
        if std::env::var(retired).is_ok_and(|v| !v.trim().is_empty()) {
            tracing::error!(
                "{retired} is set but is no longer read for paying providers. \
                 One wallet pays now: set {TREASURY_KEY_VAR} to the treasury key \
                 and fund that address with USDC on Base."
            );
            return None;
        }
    }
    None
}

fn env_usd_atomic(var: &str) -> Option<u128> {
    let raw = std::env::var(var).ok()?;
    let bd: BigDecimal = raw.trim().parse().ok()?;
    usd_to_atomic_u128(&bd)
}

/// USD (BigDecimal) → atomic USDC (u128, 6 decimals), truncating sub-atomic
/// precision. None on negative input.
pub(crate) fn usd_to_atomic_u128(usd: &BigDecimal) -> Option<u128> {
    if usd < &BigDecimal::from(0) {
        return None;
    }
    (usd * BigDecimal::from(1_000_000u64))
        .with_scale(0)
        .to_string()
        .parse::<u128>()
        .ok()
}

/// The upstream's x402 requirement, parsed from its 402 response. These are the
/// fields the EIP-712 authorization binds to (from the v1 `accepts[]` in the
/// body — the dialect every released x402 SDK signs against).
#[derive(Debug, Clone, PartialEq)]
pub struct UpstreamRequirement {
    pub scheme: String,
    /// Bare network name echoed into the proof, e.g. "base".
    pub network: String,
    pub chain_id: u64,
    pub pay_to: Address,
    /// The USDC contract = EIP-712 `verifyingContract`.
    pub asset: Address,
    /// `payTo` and `asset` EXACTLY AS THE PROVIDER WROTE THEM.
    ///
    /// Parsing an address and printing it back is lossy in the one way that
    /// matters here: `{:#x}` lowercases, and EIP-55 checksummed addresses are
    /// mixed case. That is invisible on-chain — both forms decode to the same
    /// twenty bytes — and fatal off-chain, because a server matching our
    /// payment against its own `accepts[]` with `===` sees two different
    /// strings and finds nothing.
    ///
    /// One did, and its crash is the reason these fields exist:
    ///
    ///   "Cannot destructure property 'extra' of 'accepted' as it is undefined."
    ///
    /// So the envelope echoes the provider's own spelling and the signature is
    /// still computed over the parsed address. Nothing about the payment
    /// changes; only the string we hand back.
    pub pay_to_raw: String,
    pub asset_raw: String,
    /// Amount in atomic USDC units (6 decimals) as an integer string — signed
    /// and paid VERBATIM (never re-scaled).
    pub value_atomic: String,
    /// EIP-712 domain `name` (the token's on-chain `name()`), e.g. "USD Coin".
    pub token_name: String,
    /// EIP-712 domain `version`, e.g. "2".
    pub token_version: String,
    pub max_timeout_secs: u64,
    /// The upstream's advertised x402 protocol version (1 or 2). The proof we
    /// return must match this dialect: v1 ⇒ bare network name; v2 ⇒ CAIP-2.
    pub x402_version: u8,
    /// The `accepts[]` entry this requirement was parsed from, byte-for-byte
    /// as the provider wrote it. A v2 proof must carry it under `accepted`:
    /// the server matches the payment against its own offer through this
    /// object, and one that finds it missing treats the whole request as
    /// unpaid — a second 402 with an empty body and the identical offer,
    /// which is the "Cannot destructure property 'extra' of 'accepted'"
    /// crash above wearing a politer face.
    pub accepted_raw: Value,
    /// The offer's top-level `resource` object (v2 offers carry one), echoed
    /// into the proof beside `accepted` for the same reason.
    pub resource_raw: Option<Value>,
}

/// Map an x402 network field (bare "base" or CAIP-2 "eip155:8453") to
/// (chain_id, bare_name). Only chains the payer wallet can be funded on are
/// resolvable; anything else is rejected upstream (fail-closed).
fn resolve_network(network: &str) -> Option<(u64, &'static str)> {
    let n = network.trim().to_lowercase();
    match n.as_str() {
        "base" | "eip155:8453" => Some((8453, "base")),
        "polygon" | "eip155:137" => Some((137, "polygon")),
        "arbitrum" | "arbitrum-one" | "eip155:42161" => Some((42161, "arbitrum")),
        _ => None,
    }
}

fn parse_address(s: &str) -> Result<Address> {
    Address::from_str(s.trim()).with_context(|| format!("invalid address: {s}"))
}

/// The default USDC EIP-712 domain name/version, used only when the upstream 402
/// omits `extra` (well-formed x402 upstreams always include it).
const DEFAULT_USDC_NAME: &str = "USD Coin";
const DEFAULT_USDC_VERSION: &str = "2";

/// Parse an upstream 402 into a payable requirement, choosing the first
/// `accepts[]` entry on a network the payer can fund (`allowed_chain_ids`).
///
/// Supports both the x402 v1 JSON body and the x402 v2 base64-encoded
/// `PAYMENT-REQUIRED` header. The body wins when both are present.
pub fn parse_upstream_402(body: &[u8], allowed_chain_ids: &[u64]) -> Result<UpstreamRequirement> {
    parse_upstream_402_with_header(body, None, allowed_chain_ids)
}

/// Sirenic is priced in USD internally. Select only its reviewed USDC/Base
/// quote, regardless of the order of USDC/EURC offers, before any claim/signing.
pub fn parse_reviewed_upstream_402(
    body: &[u8],
    header: Option<&str>,
    api_slug: &str,
) -> Result<UpstreamRequirement> {
    if api_slug != "sirenic" {
        return parse_upstream_402_with_header(body, header, &[8453]);
    }
    // The JSON body is Sirenic's human-facing quote, with legacy fields and
    // no v2 resource descriptor. PAYMENT-REQUIRED is the wire contract the
    // server verifies; prefer it even when the body also has accepts[].
    let mut doc: Value = if let Some(header) = header.filter(|v| !v.trim().is_empty()) {
        serde_json::from_slice(&BASE64.decode(header.trim())?)?
    } else {
        serde_json::from_slice(body)?
    };
    anyhow::ensure!(
        doc["x402Version"] == 2 && doc["resource"].is_object(),
        "Sirenic requires the complete x402 v2 payment requirements"
    );
    let accepts = doc["accepts"]
        .as_array_mut()
        .context("missing Sirenic quote")?;
    accepts.retain(|r| {
        r["scheme"] == "exact"
            && r["network"] == "eip155:8453"
            && r["asset"].as_str().is_some_and(|v| {
                v.eq_ignore_ascii_case("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")
            })
            && r["payTo"].as_str().is_some_and(|v| {
                v.eq_ignore_ascii_case("0x76A672EEe56D29D475b0715cc03B8C99D70EC8A2")
            })
            && r["extra"]["name"] == "USD Coin"
            && r["extra"]["version"] == "2"
            && r["maxTimeoutSeconds"]
                .as_u64()
                .is_some_and(|v| (45..=200).contains(&v))
            && (r.get("maxAmountRequired").is_none() || r["amount"] == r["maxAmountRequired"])
    });
    anyhow::ensure!(
        !accepts.is_empty(),
        "Sirenic quote differs from reviewed payment terms"
    );
    parse_upstream_402(&serde_json::to_vec(&doc)?, &[8453])
}

pub fn parse_upstream_402_with_header(
    body: &[u8],
    payment_required_header: Option<&str>,
    allowed_chain_ids: &[u64],
) -> Result<UpstreamRequirement> {
    let body_doc = serde_json::from_slice::<Value>(body).ok();
    let body_has_accepts = body_doc
        .as_ref()
        .and_then(|doc| doc.get("accepts"))
        .and_then(Value::as_array)
        .is_some_and(|accepts| !accepts.is_empty());

    let doc = if body_has_accepts {
        body_doc.expect("checked above")
    } else if let Some(encoded) = payment_required_header.filter(|value| !value.trim().is_empty()) {
        let decoded = BASE64
            .decode(encoded.trim())
            .context("PAYMENT-REQUIRED header is not valid base64")?;
        serde_json::from_slice(&decoded).context("PAYMENT-REQUIRED header is not valid JSON")?
    } else {
        return Err(anyhow!(
            "upstream 402 has no accepts[] in body or PAYMENT-REQUIRED header"
        ));
    };
    let x402_version = doc
        .get("x402Version")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .clamp(1, 2) as u8;
    let accepts = doc
        .get("accepts")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("upstream 402 has no accepts[]"))?;

    // Prefer an accepts entry on a chain we can pay; fail closed otherwise.
    for entry in accepts {
        let network = entry.get("network").and_then(Value::as_str).unwrap_or("");
        let Some((chain_id, bare)) = resolve_network(network) else {
            continue;
        };
        if !allowed_chain_ids.contains(&chain_id) {
            continue;
        }
        // Amount: v1 `maxAmountRequired` or v2 `amount`, atomic integer string.
        let value_atomic = entry
            .get("maxAmountRequired")
            .or_else(|| entry.get("amount"))
            .and_then(value_as_atomic_string)
            .ok_or_else(|| anyhow!("accepts entry missing amount"))?;
        let pay_to_raw = entry
            .get("payTo")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("accepts entry missing payTo"))?
            .trim()
            .to_string();
        let pay_to = parse_address(&pay_to_raw)?;
        let asset_raw = entry
            .get("asset")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("accepts entry missing asset"))?
            .trim()
            .to_string();
        let asset = parse_address(&asset_raw)?;
        let extra = entry.get("extra");
        let token_name = extra
            .and_then(|e| e.get("name"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_USDC_NAME)
            .to_string();
        let token_version = extra
            .and_then(|e| e.get("version"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_USDC_VERSION)
            .to_string();
        let max_timeout_secs = entry
            .get("maxTimeoutSeconds")
            .and_then(Value::as_u64)
            .unwrap_or(300);
        let scheme = entry
            .get("scheme")
            .and_then(Value::as_str)
            .unwrap_or("exact")
            .to_string();

        return Ok(UpstreamRequirement {
            scheme,
            network: bare.to_string(),
            chain_id,
            pay_to,
            asset,
            pay_to_raw,
            asset_raw,
            value_atomic,
            token_name,
            token_version,
            max_timeout_secs,
            x402_version,
            accepted_raw: entry.clone(),
            resource_raw: doc.get("resource").filter(|r| r.is_object()).cloned(),
        });
    }

    bail!("upstream 402 offered no accepts[] on a payable network")
}

/// Read an amount as an atomic integer string, accepting a JSON string or number.
fn value_as_atomic_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => {
            let t = s.trim();
            (!t.is_empty() && t.chars().all(|c| c.is_ascii_digit())).then(|| t.to_string())
        }
        Value::Number(n) if n.is_u64() => Some(n.to_string()),
        _ => None,
    }
}

/// Deterministic 32-byte nonce per request so a retry re-signs the SAME
/// authorization (the token contract rejects the on-chain replay).
fn nonce_for_request(request_id: &Uuid) -> [u8; 32] {
    keccak256(request_id.as_bytes())
}

/// Build the base64 `X-Payment` header value: sign an EIP-3009
/// `TransferWithAuthorization` from `wallet` to the upstream's payTo for the
/// exact demanded amount, and wrap it in the x402 v1 proof shape.
///
/// `now_unix` is injected so the result is testable and the caller controls the
/// clock (the runtime forbids ambient time in some contexts).
pub fn build_x402_payment_header(
    wallet: &LocalWallet,
    req: &UpstreamRequirement,
    request_id: &Uuid,
    now_unix: u64,
) -> Result<String> {
    let from = format!("{:#x}", ethers_signers::Signer::address(wallet));
    // The provider's own spelling, not ours. See `pay_to_raw`.
    let to = req.pay_to_raw.clone();
    let asset = req.asset_raw.clone();
    let nonce_hex = format!("0x{}", hex::encode(nonce_for_request(request_id)));
    // validBefore is bounded by the upstream's maxTimeoutSeconds (never longer).
    let ttl = req.max_timeout_secs.min(600);
    anyhow::ensure!(ttl > 0, "upstream payment timeout must be positive");
    let valid_before = (now_unix + ttl).to_string();

    // The typed data — identical structure to what the browser signer builds
    // (payment_page.rs) — handed to ethers' tested EIP-712 encoder.
    let typed = json!({
        "types": {
            "EIP712Domain": [
                {"name": "name", "type": "string"},
                {"name": "version", "type": "string"},
                {"name": "chainId", "type": "uint256"},
                {"name": "verifyingContract", "type": "address"}
            ],
            "TransferWithAuthorization": [
                {"name": "from", "type": "address"},
                {"name": "to", "type": "address"},
                {"name": "value", "type": "uint256"},
                {"name": "validAfter", "type": "uint256"},
                {"name": "validBefore", "type": "uint256"},
                {"name": "nonce", "type": "bytes32"}
            ]
        },
        "primaryType": "TransferWithAuthorization",
        "domain": {
            "name": req.token_name,
            "version": req.token_version,
            "chainId": req.chain_id,
            "verifyingContract": asset
        },
        "message": {
            "from": from,
            "to": to,
            "value": req.value_atomic,
            "validAfter": "0",
            "validBefore": valid_before,
            "nonce": nonce_hex
        }
    });

    let td: TypedData =
        serde_json::from_value(typed).context("failed to build EIP-712 typed data")?;
    let digest = td
        .encode_eip712()
        .map_err(|e| anyhow!("EIP-712 encoding failed: {e}"))?;
    let signature: Signature = wallet
        .sign_hash(H256::from(digest))
        .context("failed to sign EIP-712 digest")?;
    let sig_hex = format!("0x{}", hex::encode(signature.to_vec()));

    let payload = json!({
        "signature": sig_hex,
        "authorization": {
            "from": from,
            "to": to,
            "value": req.value_atomic,
            "validAfter": "0",
            "validBefore": valid_before,
            "nonce": nonce_hex
        }
    });

    // Match the upstream's dialect. The EIP-712 signature is identical either
    // way — only the JSON envelope differs, and the difference is structural,
    // not cosmetic. A v2 PaymentPayload is `{x402Version, resource, accepted,
    // payload}` (specs/x402-specification-v2.md): the server locates the offer
    // we are paying by the echoed `accepted` entry and `resource`. Sending it
    // the v1 `{scheme, network, payload}` shape instead reads as no payment at
    // all — the provider answers a signed, funded proof with a second 402, an
    // empty body and the identical offer re-quoted, which is precisely what
    // api.agentstools.dev did on every paid retry until this was fixed.
    let proof = if req.x402_version >= 2 {
        let mut envelope = serde_json::Map::new();
        envelope.insert("x402Version".to_string(), json!(2));
        if let Some(resource) = &req.resource_raw {
            envelope.insert("resource".to_string(), resource.clone());
        }
        envelope.insert("accepted".to_string(), req.accepted_raw.clone());
        envelope.insert("payload".to_string(), payload);
        Value::Object(envelope)
    } else {
        json!({
            "x402Version": 1,
            "scheme": req.scheme,
            "network": req.network,
            "payload": payload
        })
    };

    Ok(BASE64.encode(serde_json::to_string(&proof).context("failed to serialize proof")?))
}

/// Fail-closed pre-sign guard: refuse to pay if the upstream's demanded amount
/// exceeds the per-tx cap OR the listing price the agent is charged (the spread
/// must be non-negative, else Apiosk loses money on the call). Amounts are
/// atomic USDC (6 decimals).
pub fn check_spend_allowed(
    upstream_atomic: &str,
    cap_per_tx_atomic: Option<u128>,
    listing_price_atomic: u128,
) -> Result<u128> {
    let amount: u128 = upstream_atomic
        .parse()
        .with_context(|| format!("unparseable upstream amount: {upstream_atomic}"))?;
    if amount == 0 {
        bail!("upstream demanded a zero amount");
    }
    if let Some(cap) = cap_per_tx_atomic {
        if amount > cap {
            bail!("upstream amount {amount} exceeds per-tx cap {cap}");
        }
    }
    if amount > listing_price_atomic {
        bail!(
            "upstream amount {amount} exceeds listing price {listing_price_atomic} (negative spread)"
        );
    }
    Ok(amount)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Deterministic throwaway test key (NOT a real wallet).
    const TEST_KEY: &str = "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d";

    #[test]
    fn receipt_requires_confirmed_transaction() {
        let tx = format!("0x{}", "a".repeat(64));
        let header = |value: serde_json::Value| BASE64.encode(value.to_string());
        assert_eq!(
            payment_transaction(Some(&header(
                serde_json::json!({"success":true,"transaction":tx})
            ))),
            Some(tx.clone())
        );
        for value in [
            serde_json::json!({"success":false,"transaction":tx}),
            serde_json::json!({"success":true}),
            serde_json::json!({"success":true,"transaction":"unknown"}),
        ] {
            assert_eq!(payment_transaction(Some(&header(value))), None);
        }
        assert_eq!(payment_transaction(None), None);
        assert_eq!(payment_transaction(Some("invalid")), None);
    }

    fn test_wallet() -> LocalWallet {
        LocalWallet::from_str(TEST_KEY).unwrap()
    }

    fn base_usdc_requirement(amount: &str) -> UpstreamRequirement {
        UpstreamRequirement {
            scheme: "exact".to_string(),
            network: "base".to_string(),
            chain_id: 8453,
            pay_to: parse_address("0x3e7c9cA818f713F19e126E3B7B37B47dC1411570").unwrap(),
            asset: parse_address("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913").unwrap(),
            // Checksummed, as a real offer writes them — which is the whole
            // point of keeping them separately.
            pay_to_raw: "0x3e7c9cA818f713F19e126E3B7B37B47dC1411570".to_string(),
            asset_raw: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".to_string(),
            value_atomic: amount.to_string(),
            token_name: "USD Coin".to_string(),
            token_version: "2".to_string(),
            max_timeout_secs: 300,
            x402_version: 1,
            accepted_raw: json!({
                "scheme": "exact",
                "network": "base",
                "maxAmountRequired": amount,
                "payTo": "0x3e7c9cA818f713F19e126E3B7B37B47dC1411570",
                "asset": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
                "extra": { "name": "USD Coin", "version": "2" }
            }),
            resource_raw: None,
        }
    }

    /* -------------------------------------------------------------------
    "Cannot destructure property 'extra' of 'accepted' as it is undefined."

    A real provider's crash, answering a request we had just signed and
    paid for. Its server matched our payment against its own `accepts[]`,
    found nothing, and fell over destructuring the nothing. The reason it
    found nothing: it offered a checksummed `payTo` and we echoed a
    lowercased one, because the address had made a round trip through
    `Address` and back out through `{:#x}`.

    On-chain the two are the same twenty bytes. Through a `===` in
    somebody else's server they are two different strings, and the whole
    payment is refused with a 402 whose body is `{}`.
    ------------------------------------------------------------------ */
    /* A real provider's v2 challenge: a problem+json body carrying no accepts
    at all, and the offer itself base64'd into the PAYMENT-REQUIRED header.
    Read body-only this is "the upstream payment challenge could not be
    understood", and no payment is ever attempted. */
    #[test]
    fn a_v2_challenge_that_lives_only_in_the_header_is_still_payable() {
        let problem_json = serde_json::to_vec(&json!({
            "type": "https://paymentauth.org/problems/payment-required",
            "title": "Payment Required",
            "status": 402,
            "detail": "Payment is required."
        }))
        .unwrap();

        let offer = BASE64.encode(
            serde_json::to_vec(&json!({
                "x402Version": 2,
                "accepts": [{
                    "scheme": "exact",
                    "network": "eip155:8453",
                    "amount": "4000",
                    "payTo": "0xCe339395d7f2ea5a5009022EF1C392Fb208a989E",
                    "asset": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
                    "maxTimeoutSeconds": 300,
                    "extra": { "name": "USD Coin", "version": "2" }
                }]
            }))
            .unwrap(),
        );

        assert!(
            parse_upstream_402(&problem_json, &[8453]).is_err(),
            "the body alone genuinely carries nothing payable — that is the trap"
        );

        let req = parse_upstream_402_with_header(&problem_json, Some(&offer), &[8453])
            .expect("the header carries the offer and must be read");
        assert_eq!(req.value_atomic, "4000");
        assert_eq!(req.x402_version, 2);
        assert_eq!(req.network, "base");
    }

    #[test]
    fn the_proof_echoes_the_providers_own_spelling_of_payto_and_asset() {
        let checksummed_pay_to = "0xCe339395d7f2ea5a5009022EF1C392Fb208a989E";
        let checksummed_asset = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
        let body = serde_json::to_vec(&json!({
            "x402Version": 2,
            "accepts": [{
                "scheme": "exact",
                "network": "eip155:8453",
                "amount": "4000",
                "payTo": checksummed_pay_to,
                "asset": checksummed_asset,
                "maxTimeoutSeconds": 300,
                "extra": { "name": "USD Coin", "version": "2" }
            }]
        }))
        .unwrap();

        let req = parse_upstream_402(&body, &[8453]).expect("offer must parse");
        assert_eq!(req.pay_to_raw, checksummed_pay_to);
        assert_eq!(req.asset_raw, checksummed_asset);

        let header =
            build_x402_payment_header(&test_wallet(), &req, &Uuid::nil(), 1_700_000_000).unwrap();
        let decoded: Value = serde_json::from_slice(&BASE64.decode(header).unwrap()).unwrap();
        let to = decoded["payload"]["authorization"]["to"].as_str().unwrap();

        assert_eq!(
            to, checksummed_pay_to,
            "the envelope must hand back the string the provider sent, character for character"
        );
        assert_ne!(
            to,
            checksummed_pay_to.to_lowercase(),
            "not silently lowercased"
        );
    }

    /* The signature is over the ADDRESS, not over its spelling — so echoing a
    checksummed string must not change the digest. If it ever does, a
    provider that does normalise addresses would start rejecting us for the
    opposite reason. */
    #[test]
    fn echoing_the_checksummed_form_does_not_change_what_was_signed() {
        let mut lower = base_usdc_requirement("4000");
        lower.pay_to_raw = lower.pay_to_raw.to_lowercase();
        lower.asset_raw = lower.asset_raw.to_lowercase();
        let checksummed = base_usdc_requirement("4000");

        let a =
            build_x402_payment_header(&test_wallet(), &lower, &Uuid::nil(), 1_700_000_000).unwrap();
        let b =
            build_x402_payment_header(&test_wallet(), &checksummed, &Uuid::nil(), 1_700_000_000)
                .unwrap();

        let sig = |header: String| -> String {
            let v: Value = serde_json::from_slice(&BASE64.decode(header).unwrap()).unwrap();
            v["payload"]["signature"].as_str().unwrap().to_string()
        };
        assert_eq!(sig(a), sig(b));
    }

    #[test]
    fn parses_caip2_and_bare_networks_and_picks_funded_chain() {
        // Two accepts: arbitrum (not funded) then base (funded) — must pick base.
        let body = serde_json::to_vec(&json!({
            "accepts": [
                {"scheme":"exact","network":"eip155:42161","maxAmountRequired":"5000",
                 "payTo":"0x3e7c9cA818f713F19e126E3B7B37B47dC1411570",
                 "asset":"0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
                 "extra":{"name":"USD Coin","version":"2"}},
                {"scheme":"exact","network":"eip155:8453","maxAmountRequired":"5000",
                 "payTo":"0x3e7c9cA818f713F19e126E3B7B37B47dC1411570",
                 "asset":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
                 "extra":{"name":"USD Coin","version":"2"}}
            ]
        }))
        .unwrap();
        let req = parse_upstream_402(&body, &[8453]).unwrap();
        assert_eq!(req.chain_id, 8453);
        assert_eq!(req.network, "base");
        assert_eq!(req.value_atomic, "5000");
        assert_eq!(req.token_name, "USD Coin");
    }

    #[test]
    fn parse_fails_closed_when_no_funded_network() {
        let body = serde_json::to_vec(&json!({
            "accepts": [
                {"scheme":"exact","network":"eip155:42161","maxAmountRequired":"5000",
                 "payTo":"0x3e7c9cA818f713F19e126E3B7B37B47dC1411570",
                 "asset":"0xaf88d065e77c8cC2239327C5EDb3A432268e5831"}
            ]
        }))
        .unwrap();
        // Only Base funded, but upstream only offers Arbitrum → must error.
        assert!(parse_upstream_402(&body, &[8453]).is_err());
    }

    #[test]
    fn parses_v2_payment_required_header_when_body_is_empty() {
        let challenge = json!({
            "x402Version": 2,
            "resource": {"url": "https://example.com/search?q=weather", "method": "GET"},
            "accepts": [{
                "scheme": "exact",
                "network": "eip155:8453",
                "amount": "20000",
                "payTo": "0x3e7c9cA818f713F19e126E3B7B37B47dC1411570",
                "asset": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
                "maxTimeoutSeconds": 300,
                "extra": {"name": "USD Coin", "version": "2"}
            }]
        });
        let encoded = BASE64.encode(serde_json::to_vec(&challenge).unwrap());
        let req = parse_upstream_402_with_header(&[], Some(&encoded), &[8453]).unwrap();
        assert_eq!(req.x402_version, 2);
        assert_eq!(req.chain_id, 8453);
        assert_eq!(req.value_atomic, "20000");
    }

    #[test]
    fn header_is_standard_base64_v1_shape() {
        let req = base_usdc_requirement("5100");
        let rid = Uuid::from_u128(0x1234);
        let header = build_x402_payment_header(&test_wallet(), &req, &rid, 1_700_000_000).unwrap();
        // Standard base64 (padding, no url-safe chars).
        assert!(!header.contains('-') && !header.contains('_'));
        let decoded = BASE64.decode(header).unwrap();
        let proof: Value = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(proof["x402Version"], 1);
        assert_eq!(proof["scheme"], "exact");
        assert_eq!(proof["network"], "base");
        let auth = &proof["payload"]["authorization"];
        assert_eq!(auth["value"], "5100"); // atomic string, verbatim
        assert_eq!(auth["validAfter"], "0");
        assert!(auth["nonce"].as_str().unwrap().starts_with("0x"));
        let sig = proof["payload"]["signature"].as_str().unwrap();
        assert_eq!(sig.len(), 2 + 130); // 0x + 65 bytes hex
    }

    /* The bug this shape exists to kill: api.agentstools.dev (x402 v2,
    Python) answered every signed, funded, correctly-addressed proof with a
    second 402, an empty `{}` body and the identical offer re-quoted —
    because the proof arrived in the v1 `{scheme, network, payload}` shape
    and a v2 server locates the offer being paid through the echoed
    `accepted` entry. No `accepted`, no payment, as far as it can see. */
    #[test]
    fn a_v2_proof_echoes_the_offers_accepted_entry_and_resource() {
        let accepts_entry = json!({
            "scheme": "exact",
            "network": "eip155:8453",
            "amount": "40000",
            "payTo": "0xCe339395d7f2ea5a5009022EF1C392Fb208a989E",
            "asset": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
            "maxTimeoutSeconds": 300,
            "extra": { "name": "USD Coin", "version": "2" }
        });
        let resource = json!({
            "url": "https://api.example.dev/news/brief",
            "description": "Situation brief on a topic",
            "mimeType": "application/json"
        });
        let body = serde_json::to_vec(&json!({
            "x402Version": 2,
            "resource": resource,
            "accepts": [accepts_entry]
        }))
        .unwrap();

        let req = parse_upstream_402(&body, &[8453]).expect("offer must parse");
        let header =
            build_x402_payment_header(&test_wallet(), &req, &Uuid::nil(), 1_700_000_000).unwrap();
        let proof: Value = serde_json::from_slice(&BASE64.decode(header).unwrap()).unwrap();

        assert_eq!(proof["x402Version"], 2);
        assert_eq!(
            proof["accepted"], accepts_entry,
            "the accepted entry must go back byte-for-byte — it is how the server finds the offer"
        );
        assert_eq!(proof["resource"], resource);
        assert!(proof["payload"]["signature"].is_string());
        // Spec-exact envelope: the v1 top-level fields must NOT ride along. A
        // strict v2 parser (additionalProperties: false) rejects the whole
        // proof over them, which is indistinguishable from no payment at all.
        assert!(proof.get("scheme").is_none());
        assert!(proof.get("network").is_none());
    }

    /* A v1 offer keeps producing the v1 shape, exactly as before. */
    #[test]
    fn a_v1_offer_still_gets_the_v1_proof_shape() {
        let req = base_usdc_requirement("4000");
        let header =
            build_x402_payment_header(&test_wallet(), &req, &Uuid::nil(), 1_700_000_000).unwrap();
        let proof: Value = serde_json::from_slice(&BASE64.decode(header).unwrap()).unwrap();
        assert_eq!(proof["x402Version"], 1);
        assert_eq!(proof["scheme"], "exact");
        assert_eq!(proof["network"], "base");
        assert!(proof.get("accepted").is_none());
        assert!(proof.get("resource").is_none());
    }

    #[test]
    fn signature_recovers_to_the_payer_wallet() {
        // Proves the digest + signature are internally correct: re-derive the
        // EIP-712 digest from the emitted authorization and recover the signer.
        let wallet = test_wallet();
        let req = base_usdc_requirement("5100");
        let rid = Uuid::from_u128(0xabcd);
        let now = 1_700_000_000u64;
        let header = build_x402_payment_header(&wallet, &req, &rid, now).unwrap();
        let proof: Value = serde_json::from_slice(&BASE64.decode(header).unwrap()).unwrap();
        let auth = &proof["payload"]["authorization"];

        let typed = json!({
            "types": {
                "EIP712Domain": [
                    {"name":"name","type":"string"},{"name":"version","type":"string"},
                    {"name":"chainId","type":"uint256"},{"name":"verifyingContract","type":"address"}
                ],
                "TransferWithAuthorization": [
                    {"name":"from","type":"address"},{"name":"to","type":"address"},
                    {"name":"value","type":"uint256"},{"name":"validAfter","type":"uint256"},
                    {"name":"validBefore","type":"uint256"},{"name":"nonce","type":"bytes32"}
                ]
            },
            "primaryType": "TransferWithAuthorization",
            "domain": {"name": req.token_name, "version": req.token_version,
                       "chainId": req.chain_id, "verifyingContract": format!("{:#x}", req.asset)},
            "message": auth.clone()
        });
        let td: TypedData = serde_json::from_value(typed).unwrap();
        let digest = td.encode_eip712().unwrap();
        let sig = Signature::from_str(proof["payload"]["signature"].as_str().unwrap()).unwrap();
        let recovered = sig.recover(H256::from(digest)).unwrap();
        assert_eq!(recovered, ethers_signers::Signer::address(&wallet));
    }

    #[test]
    fn caps_and_spread_are_enforced() {
        // within cap and spread
        assert_eq!(
            check_spend_allowed("5000", Some(10_000), 5100).unwrap(),
            5000
        );
        // exceeds per-tx cap
        assert!(check_spend_allowed("20000", Some(10_000), 30000).is_err());
        // negative spread (upstream wants more than the agent is charged)
        assert!(check_spend_allowed("6000", Some(10_000), 5100).is_err());
        // zero amount
        assert!(check_spend_allowed("0", None, 5100).is_err());
    }

    #[test]
    fn short_payment_quotes_are_never_extended() {
        let mut req = base_usdc_requirement("20000");
        req.max_timeout_secs = 45;
        let header =
            build_x402_payment_header(&test_wallet(), &req, &Uuid::nil(), 1_700_000_000).unwrap();
        let proof: Value = serde_json::from_slice(&BASE64.decode(header).unwrap()).unwrap();
        assert_eq!(
            proof["payload"]["authorization"]["validBefore"],
            "1700000045"
        );
        req.max_timeout_secs = 0;
        assert!(
            build_x402_payment_header(&test_wallet(), &req, &Uuid::nil(), 1_700_000_000).is_err()
        );
    }

    #[test]
    fn sirenic_selects_only_reviewed_usdc_terms() {
        let mut doc: Value =
            serde_json::from_str(include_str!("fixtures/sirenic-payment-required.json")).unwrap();
        doc["accepts"].as_array_mut().unwrap().reverse();
        let parse = |d: &Value| {
            parse_reviewed_upstream_402(&serde_json::to_vec(d).unwrap(), None, "sirenic")
        };
        let req = parse(&doc).unwrap();
        assert_eq!(req.value_atomic, "2000");
        assert_eq!(req.token_name, "USD Coin");
        assert!(req.resource_raw.is_some());
        let encoded = BASE64.encode(serde_json::to_vec(&doc).unwrap());
        assert_eq!(
            parse_reviewed_upstream_402(
                include_bytes!("fixtures/sirenic-search-quote.json"),
                Some(&encoded),
                "sirenic"
            )
            .unwrap(),
            req
        );
        assert!(parse_reviewed_upstream_402(
            include_bytes!("fixtures/sirenic-search-quote.json"),
            None,
            "sirenic"
        )
        .is_err());
        assert!(req.accepted_raw.get("maxAmountRequired").is_none());
        let invoice = parse_reviewed_upstream_402(
            include_bytes!("fixtures/sirenic-invoice-payment-required.json"),
            None,
            "sirenic",
        )
        .unwrap();
        assert_eq!(invoice.max_timeout_secs, 45);
        assert_eq!(invoice.value_atomic, "20000");

        let health = parse_reviewed_upstream_402(
            include_bytes!("fixtures/sirenic-health-payment-required.json"),
            None,
            "sirenic",
        )
        .unwrap();
        assert_eq!(health.value_atomic, "150000");
        assert_eq!(health.max_timeout_secs, 200);
        // The health check and intelligence report publish 200-second quotes.
        // They retain the same fixed amount, Base USDC asset and recipient.
        let mut report = doc.clone();
        for accept in report["accepts"].as_array_mut().unwrap() {
            accept["maxTimeoutSeconds"] = json!(200);
        }
        assert_eq!(parse(&report).unwrap().max_timeout_secs, 200);
        for accept in report["accepts"].as_array_mut().unwrap() {
            accept["maxTimeoutSeconds"] = json!(201);
        }
        assert!(parse(&report).is_err());

        // Removing USDC must not silently opt the treasury into EURC.
        let mut eurc = doc.clone();
        eurc["accepts"]
            .as_array_mut()
            .unwrap()
            .retain(|a| a["extra"]["name"] != "USD Coin");
        assert!(parse(&eurc).is_err());
        for (field, value) in [
            ("payTo", json!("0x0000000000000000000000000000000000000001")),
            ("network", json!("eip155:1")),
            ("scheme", json!("upto")),
            ("maxTimeoutSeconds", json!(3600)),
            ("maxAmountRequired", json!("9999")),
        ] {
            let mut changed = doc.clone();
            for accept in changed["accepts"].as_array_mut().unwrap() {
                accept[field] = value.clone();
            }
            assert!(parse(&changed).is_err(), "{field}");
        }
        assert!(check_spend_allowed(&req.value_atomic, Some(2000), 2000).is_ok());
        assert!(check_spend_allowed(&req.value_atomic, Some(1999), 2000).is_err());
        assert!(check_spend_allowed(&req.value_atomic, Some(2000), 1999).is_err());
    }

    /// LIVE end-to-end probe — IGNORED by default because it spends real USDC.
    /// Drives the actual resell path (parse → sign EIP-3009 → pay) against
    /// OpenWebNinja. Run:
    ///   set -a; . .env; set +a
    ///   cargo test --bin apiosk-gateway resell::tests::live_resell_probe \
    ///     -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_resell_probe() {
        // Use the real operator key if it's present + valid; otherwise fall back
        // to an UNFUNDED throwaway so we can still probe format/signature
        // acceptance (a funding error from the facilitator = our proof is
        // correct; a malformed/invalid-signature error = a real bug).
        let env_key = std::env::var(TREASURY_KEY_VAR).unwrap_or_default();
        let (wallet, funded) = match LocalWallet::from_str(env_key.trim()) {
            Ok(w) if env_key.trim().trim_start_matches("0x").len() == 64 => (w, true),
            _ => {
                eprintln!("[resell-probe] no real key — UNFUNDED throwaway (format/acceptance probe only)");
                (LocalWallet::from_str(TEST_KEY).unwrap(), false)
            }
        };
        let payer = ethers_signers::Signer::address(&wallet);
        eprintln!("[resell-probe] payer = {payer:#x} (funded={funded})");

        let client = reqwest::Client::new();
        let url = "https://x402.openwebninja.com/ai-answers?query=capital%20of%20France";

        // 1) Trigger the upstream 402.
        let r1 = client
            .get(url)
            .header("accept", "application/json")
            .send()
            .await
            .expect("get 402");
        let s1 = r1.status();
        let body = r1.bytes().await.expect("402 body");
        eprintln!("[resell-probe] step1 upstream status = {s1}");
        assert_eq!(s1.as_u16(), 402, "expected an initial 402");

        // 2) Parse the requirement with our real parser (Base only).
        let req = parse_upstream_402(&body, &[8453]).expect("parse 402");
        eprintln!(
            "[resell-probe] step2 parsed: payTo={:#x} value={} asset={:#x} chain={} name='{}' ver='{}'",
            req.pay_to, req.value_atomic, req.asset, req.chain_id, req.token_name, req.token_version
        );

        // 3) Sign the EIP-3009 authorization + build the X-Payment proof.
        let rid = Uuid::new_v4();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let header = build_x402_payment_header(&wallet, &req, &rid, now).expect("sign");
        eprintln!(
            "[resell-probe] step3 X-Payment built ({} b64 chars)",
            header.len()
        );

        // 4) Re-issue WITH the payment proof — this pays the provider. v2 servers
        //    read PAYMENT-SIGNATURE; send both for compatibility.
        let mut rb = client.get(url).header("accept", "application/json");
        if req.x402_version >= 2 {
            rb = rb.header("payment-signature", header.clone());
        }
        let r2 = rb
            .header("x-payment", header)
            .send()
            .await
            .expect("paid retry");
        let s2 = r2.status();
        eprintln!("[resell-probe] step4 paid status = {s2}");
        for (k, v) in r2.headers().iter() {
            let kl = k.as_str().to_lowercase();
            if kl.contains("payment")
                || kl.contains("x402")
                || kl.contains("error")
                || kl == "www-authenticate"
            {
                eprintln!(
                    "[resell-probe] step4 header {kl}: {}",
                    v.to_str().unwrap_or("?")
                );
            }
        }
        let paid_body = r2.text().await.unwrap_or_default();
        eprintln!(
            "[resell-probe] step4 body[..600] = {}",
            &paid_body[..paid_body.len().min(600)]
        );
        // Interpret, don't hard-assert: with an unfunded key we expect the
        // facilitator to accept the FORMAT and fail on funds/settlement — that
        // still proves the proof + signature are correct. Only a funded key can
        // return 2xx with data.
        if funded {
            assert_ne!(
                s2.as_u16(),
                402,
                "funded paid retry still 402 — upstream rejected"
            );
        }
    }
}
