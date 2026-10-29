//! Sign-In-With-X: proving, to an upstream, that we are the wallet that paid.
//!
//! ---------------------------------------------------------------------------
//! Why a payment rail needs a signing rail
//! ---------------------------------------------------------------------------
//!
//! Some x402 providers are asynchronous. The paid call starts a job and hands
//! back a handle; the answer is collected later from a second endpoint that
//! costs nothing. That second endpoint cannot be open to the world — anyone
//! holding a job id would read a crawl somebody else paid for — so it is
//! gated on SIWX: only the wallet that paid may poll.
//!
//! That wallet is the treasury, and the treasury key lives here and nowhere a
//! buyer can reach. So the buyer physically CANNOT complete a call they have
//! already paid for. Without this module the paid leg sells a token that no
//! one on earth except this process can redeem, which is not a product.
//!
//! ---------------------------------------------------------------------------
//! Nothing here spends
//! ---------------------------------------------------------------------------
//!
//! A SIWX signature is an identity proof, not an authorization to move money.
//! It carries no amount, no recipient and no token: it says "the holder of
//! this key signed this nonce, for this domain, in this five-minute window".
//! Compare `resell::build_x402_payment_header`, which signs an EIP-3009
//! transfer and is the reason that function has caps and a durable claim in
//! front of it. This one has neither because there is nothing to cap.
//!
//! The binding that keeps it honest is `domain` + `uri` + `nonce`, all three
//! taken from the challenge the upstream just issued and never from anything
//! we choose ourselves. A signature minted for one origin is worthless at
//! another, which is exactly the property that makes it safe to sign one on
//! demand mid-request.

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ethers_core::types::H256;
use ethers_core::utils::{hash_message, to_checksum};
use ethers_signers::{LocalWallet, Signer};
use serde_json::{json, Value};

/// The header the proof travels in.
///
/// Lower-case on purpose: `reqwest` normalises header names anyway, and the
/// spec writes it `SIGN-IN-WITH-X`. Same header either way.
pub const HEADER_SIWX: &str = "sign-in-with-x";

/// The challenge an upstream published, as it published it.
///
/// Every field is carried through verbatim rather than re-derived. The server
/// verifies the signature against the message IT would have built from these
/// values, so a field we "helpfully" normalise on the way through is a field
/// whose signature will not verify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Challenge {
    pub domain: String,
    pub uri: String,
    pub version: String,
    /// CAIP-2, e.g. `eip155:8453`. Kept in that form in the PAYLOAD; the
    /// signed MESSAGE carries the bare numeric id. See `siwe_message`.
    pub chain_id: String,
    /// `eip191` is the only one this signs. Solana challenges are recognised
    /// and declined rather than mis-signed.
    pub kind: String,
    pub nonce: String,
    pub issued_at: String,
    pub statement: Option<String>,
    pub expiration_time: Option<String>,
    pub not_before: Option<String>,
    pub request_id: Option<String>,
    pub resources: Vec<String>,
    pub signature_scheme: Option<String>,
}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

impl Challenge {
    /// Read a challenge out of an upstream 402.
    ///
    /// Both places are searched, and that is not belt-and-braces: an x402 v2
    /// provider may put the whole document in the base64 `payment-required`
    /// header and leave the body as a bare problem+json with no extensions in
    /// it at all. `resell::parse_upstream_402_with_header` learned this the
    /// hard way for the payment leg; this path was written knowing it.
    pub fn find(body: &[u8], payment_required_header: Option<&str>) -> Option<Self> {
        let from_body = serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|v| Self::from_document(&v));
        if from_body.is_some() {
            return from_body;
        }
        let decoded = BASE64.decode(payment_required_header?.trim()).ok()?;
        let document = serde_json::from_slice::<Value>(&decoded).ok()?;
        Self::from_document(&document)
    }

    /// `{"extensions":{"sign-in-with-x":{"info":{…}}}}` → a challenge.
    pub fn from_document(document: &Value) -> Option<Self> {
        let info = document
            .get("extensions")?
            .get("sign-in-with-x")?
            .get("info")?;
        Self::from_info(info)
    }

    pub fn from_info(info: &Value) -> Option<Self> {
        Some(Self {
            domain: text(info, "domain")?,
            uri: text(info, "uri")?,
            // The spec's own default. A challenge that omits it is not worth
            // failing over when every implementation writes "1".
            version: text(info, "version").unwrap_or_else(|| "1".to_string()),
            chain_id: text(info, "chainId")?,
            kind: text(info, "type").unwrap_or_else(|| "eip191".to_string()),
            nonce: text(info, "nonce")?,
            issued_at: text(info, "issuedAt")?,
            statement: text(info, "statement"),
            expiration_time: text(info, "expirationTime"),
            not_before: text(info, "notBefore"),
            request_id: text(info, "requestId"),
            resources: info
                .get("resources")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            signature_scheme: text(info, "signatureScheme"),
        })
    }

    /// `eip155:8453` → `8453`. The numeric id is what goes in the message.
    pub fn evm_chain_id(&self) -> Result<u64> {
        let reference = self
            .chain_id
            .strip_prefix("eip155:")
            .ok_or_else(|| anyhow!("not an EVM chain: {}", self.chain_id))?;
        reference
            .parse::<u64>()
            .with_context(|| format!("unreadable chain reference in {}", self.chain_id))
    }

    /// True when this gateway can answer this challenge at all.
    pub fn is_signable(&self) -> bool {
        self.chain_id.starts_with("eip155:") && self.kind.eq_ignore_ascii_case("eip191")
    }
}

/// The EIP-4361 message, byte for byte as `siwe`'s `prepareMessage()` builds it.
///
/// This is transcribed from the reference implementation rather than from the
/// prose of ERC-4361, because the two differ in ways that matter to a byte
/// comparison and it is the reference implementation that the verifier on the
/// other side is running:
///
///   * the address line is EIP-55 checksummed, and a lower-case address
///     produces a signature that recovers to the right key against the wrong
///     message;
///   * `Chain ID:` carries the BARE NUMBER (`8453`), while the payload sent
///     alongside carries the CAIP-2 form (`eip155:8453`). Putting the same
///     string in both places is the obvious mistake and it fails to verify;
///   * with no statement the separator collapses to two newlines and leaves a
///     BLANK LINE where the statement would be — `join("\n\n")` over an empty
///     value, not a skipped section.
pub fn siwe_message(challenge: &Challenge, address: &str) -> Result<String> {
    let chain_id = challenge.evm_chain_id()?;

    let header = format!(
        "{} wants you to sign in with your Ethereum account:",
        challenge.domain
    );
    let mut prefix = format!("{header}\n{address}");

    // `[prefix, statement].join("\n\n")` in the reference: a missing statement
    // still contributes its separator.
    match challenge.statement.as_deref() {
        Some(statement) => prefix.push_str(&format!("\n\n{statement}\n")),
        None => prefix.push_str("\n\n"),
    }

    let mut suffix = vec![
        format!("URI: {}", challenge.uri),
        format!("Version: {}", challenge.version),
        format!("Chain ID: {chain_id}"),
        format!("Nonce: {}", challenge.nonce),
        format!("Issued At: {}", challenge.issued_at),
    ];
    if let Some(value) = &challenge.expiration_time {
        suffix.push(format!("Expiration Time: {value}"));
    }
    if let Some(value) = &challenge.not_before {
        suffix.push(format!("Not Before: {value}"));
    }
    if let Some(value) = &challenge.request_id {
        suffix.push(format!("Request ID: {value}"));
    }
    if !challenge.resources.is_empty() {
        let mut block = vec!["Resources:".to_string()];
        block.extend(challenge.resources.iter().map(|r| format!("- {r}")));
        suffix.push(block.join("\n"));
    }

    Ok(format!("{prefix}\n{}", suffix.join("\n")))
}

/// Sign the challenge and encode the header value.
///
/// Returns the base64 the `sign-in-with-x` header carries. The payload is the
/// challenge echoed back with `address` and `signature` added — flat and
/// camelCase, which is the shape providers publish in their own JSON schema
/// on the 402.
pub fn header_value(challenge: &Challenge, wallet: &LocalWallet) -> Result<String> {
    if !challenge.is_signable() {
        bail!(
            "unsupported SIWX challenge: chain={} type={}",
            challenge.chain_id,
            challenge.kind
        );
    }

    let address = to_checksum(&wallet.address(), None);
    let message = siwe_message(challenge, &address)?;
    // EIP-191 personal_sign: keccak over the "\x19Ethereum Signed Message:\n"
    // envelope. `sign_hash` and not `sign_message` because the wallet's
    // async signer wants a runtime and this runs inside one already.
    let digest = hash_message(message.as_bytes());
    let signature = wallet
        .sign_hash(H256::from(digest))
        .context("signing the SIWX challenge")?;

    let mut payload = json!({
        "domain": challenge.domain,
        "address": address,
        "uri": challenge.uri,
        "version": challenge.version,
        "chainId": challenge.chain_id,
        "type": challenge.kind,
        "nonce": challenge.nonce,
        "issuedAt": challenge.issued_at,
        "signature": format!("0x{}", signature),
    });
    let object = payload
        .as_object_mut()
        .ok_or_else(|| anyhow!("payload is not an object"))?;
    // Optional fields are included only when the challenge carried them. An
    // explicit `null` is not the same as absent to a schema validator, and
    // the verifier rebuilds the message from what it receives.
    if let Some(v) = &challenge.statement {
        object.insert("statement".into(), json!(v));
    }
    if let Some(v) = &challenge.expiration_time {
        object.insert("expirationTime".into(), json!(v));
    }
    if let Some(v) = &challenge.not_before {
        object.insert("notBefore".into(), json!(v));
    }
    if let Some(v) = &challenge.request_id {
        object.insert("requestId".into(), json!(v));
    }
    if !challenge.resources.is_empty() {
        object.insert("resources".into(), json!(challenge.resources));
    }
    if let Some(v) = &challenge.signature_scheme {
        object.insert("signatureScheme".into(), json!(v));
    }

    Ok(BASE64.encode(serde_json::to_vec(&payload)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn challenge() -> Challenge {
        Challenge {
            domain: "stableenrich.dev".into(),
            uri: "https://stableenrich.dev/api/cloudflare/jobs".into(),
            version: "1".into(),
            chain_id: "eip155:8453".into(),
            kind: "eip191".into(),
            nonce: "8dabb46397f849cb951b1cb35c30e258".into(),
            issued_at: "2026-09-02T08:14:37.975Z".into(),
            statement: Some("Sign in to verify your wallet identity".into()),
            expiration_time: Some("2026-09-02T08:19:37.975Z".into()),
            not_before: None,
            request_id: None,
            resources: vec![],
            signature_scheme: None,
        }
    }

    /// The exact bytes `siwe@2`'s `prepareMessage()` produces for this input.
    /// If this test is ever "fixed" to look tidier, the signature stops
    /// verifying at every SIWX provider and the failure is a bare 401.
    #[test]
    fn the_message_is_byte_for_byte_eip_4361() {
        let message =
            siwe_message(&challenge(), "0x1F2ddF2bAf0A56988507e08A4622920A89751665").unwrap();
        assert_eq!(
            message,
            "stableenrich.dev wants you to sign in with your Ethereum account:\n\
             0x1F2ddF2bAf0A56988507e08A4622920A89751665\n\
             \n\
             Sign in to verify your wallet identity\n\
             \n\
             URI: https://stableenrich.dev/api/cloudflare/jobs\n\
             Version: 1\n\
             Chain ID: 8453\n\
             Nonce: 8dabb46397f849cb951b1cb35c30e258\n\
             Issued At: 2026-09-02T08:14:37.975Z\n\
             Expiration Time: 2026-09-02T08:19:37.975Z"
        );
    }

    #[test]
    fn a_missing_statement_still_leaves_its_blank_line() {
        // `[prefix, undefined].join("\n\n")` in the reference implementation.
        // Skipping the section entirely shifts every following byte and the
        // signature no longer verifies.
        let mut c = challenge();
        c.statement = None;
        c.expiration_time = None;
        let message = siwe_message(&c, "0xABC").unwrap();
        assert!(
            message.starts_with(
                "stableenrich.dev wants you to sign in with your Ethereum account:\n0xABC\n\n\nURI:"
            ),
            "got: {message:?}"
        );
    }

    #[test]
    fn the_message_carries_the_number_and_the_payload_carries_caip_2() {
        // The one mismatch that looks like a typo and is load-bearing.
        let c = challenge();
        assert!(siwe_message(&c, "0xABC")
            .unwrap()
            .contains("Chain ID: 8453"));
        assert_eq!(c.chain_id, "eip155:8453");
    }

    #[test]
    fn optional_fields_appear_only_when_the_challenge_carried_them() {
        let wallet = LocalWallet::from_str(
            "0x4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318",
        )
        .unwrap();
        let mut c = challenge();
        c.request_id = None;
        let encoded = header_value(&c, &wallet).unwrap();
        let decoded: Value = serde_json::from_slice(&BASE64.decode(encoded).unwrap()).unwrap();
        let object = decoded.as_object().unwrap();
        assert!(object.contains_key("statement"));
        assert!(object.contains_key("expirationTime"));
        assert!(
            !object.contains_key("requestId"),
            "an absent field must be absent, not null"
        );
        assert!(!object.contains_key("notBefore"));
        assert_eq!(decoded["chainId"], "eip155:8453");
        assert!(decoded["signature"].as_str().unwrap().starts_with("0x"));
    }

    #[test]
    fn the_signed_address_is_checksummed_and_matches_the_payload() {
        let wallet = LocalWallet::from_str(
            "0x4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318",
        )
        .unwrap();
        let encoded = header_value(&challenge(), &wallet).unwrap();
        let decoded: Value = serde_json::from_slice(&BASE64.decode(encoded).unwrap()).unwrap();
        let address = decoded["address"].as_str().unwrap();
        assert_eq!(address, to_checksum(&wallet.address(), None));
        // Mixed case, i.e. actually checksummed rather than lower-cased.
        assert_ne!(address, address.to_lowercase());
    }

    #[test]
    fn a_solana_challenge_is_declined_rather_than_mis_signed() {
        let wallet = LocalWallet::from_str(
            "0x4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318",
        )
        .unwrap();
        let mut c = challenge();
        c.chain_id = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp".into();
        c.kind = "ed25519".into();
        assert!(!c.is_signable());
        assert!(header_value(&c, &wallet).is_err());
    }

    #[test]
    fn the_challenge_is_read_from_the_body() {
        let body = br#"{"x402Version":2,"accepts":[],"extensions":{"sign-in-with-x":{"info":{
            "domain":"stableenrich.dev","uri":"https://stableenrich.dev/api/cloudflare/jobs",
            "version":"1","chainId":"eip155:8453","type":"eip191","nonce":"abc",
            "issuedAt":"2026-09-02T08:14:37.975Z","statement":"Sign in"}}}}"#;
        let parsed = Challenge::find(body, None).expect("challenge");
        assert_eq!(parsed.nonce, "abc");
        assert_eq!(parsed.chain_id, "eip155:8453");
        assert!(parsed.is_signable());
    }

    #[test]
    fn the_challenge_is_also_read_from_the_header_when_the_body_has_none() {
        // The v2 shape that cost the payment leg a day: problem+json in the
        // body, the real document base64'd in `payment-required`.
        let document = r#"{"extensions":{"sign-in-with-x":{"info":{
            "domain":"d.example","uri":"https://d.example/jobs","version":"1",
            "chainId":"eip155:8453","type":"eip191","nonce":"n1",
            "issuedAt":"2026-09-02T08:14:37.975Z"}}}}"#;
        let header = BASE64.encode(document);
        let body = br#"{"type":"https://paymentauth.org/problems/payment-required","status":402}"#;
        let parsed = Challenge::find(body, Some(&header)).expect("challenge");
        assert_eq!(parsed.domain, "d.example");
        assert_eq!(parsed.nonce, "n1");
        assert_eq!(parsed.statement, None);
    }

    #[test]
    fn a_402_with_no_siwx_extension_is_simply_not_one() {
        let body = br#"{"x402Version":2,"accepts":[{"scheme":"exact"}]}"#;
        assert!(Challenge::find(body, None).is_none());
    }
}
