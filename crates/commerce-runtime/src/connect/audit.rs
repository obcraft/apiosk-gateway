//! 00-09 — Audit-log helper.
//!
//! One reusable entry point for writing a row to the append-only
//! `connect_audit_log` (00-06) on every money-related Connect action (key minted,
//! merchant activated, payment settled, …). Callers must keep the signature
//! stable — it is used by 01-06, 02-07 and 04-05.
//!
//! Two hard rules from the work package:
//!   * **Fail-soft but visible.** A failed audit write must never silently mask
//!     the main action; it is logged at `error!` and swallowed, never propagated.
//!   * **No secrets / PII in metadata.** Callers are expected not to pass
//!     credentials, and [`sanitize_audit_metadata`] strips anything that looks
//!     like one as defense-in-depth before it is persisted.

use serde_json::Value;
use uuid::Uuid;

use crate::db::Database;

/// Substrings (matched case-insensitively against each JSON object key) that mark
/// a field as a credential/PII and get dropped before persistence. The audit log
/// is operator-visible and append-only, so we never want a leaked secret frozen
/// into it.
const SENSITIVE_KEY_FRAGMENTS: &[&str] = &[
    "secret",
    "private",
    "password",
    "passwd",
    "signature",
    "api_key",
    "apikey",
    "raw_key",
    "token",
    "bearer",
    "authorization",
    "credential",
    "mnemonic",
    "seed_phrase",
];

/// Write one audit row. Fail-soft: returns `()` and logs on error so the caller's
/// main action is never blocked or hidden by an audit-write failure.
///
/// `metadata` is sanitized first — never store secrets/PII.
pub async fn audit_log(
    db: &Database,
    actor: &str,
    action: &str,
    entity: &str,
    entity_id: Option<Uuid>,
    amount: Option<i64>,
    metadata: Option<Value>,
) {
    let safe_metadata = metadata.map(sanitize_audit_metadata);
    if let Err(err) = db
        .insert_connect_audit(actor, action, entity, entity_id, amount, safe_metadata)
        .await
    {
        // Visible, not silent: an operator can see audit gaps, but the caller's
        // money action still completes.
        tracing::error!(
            actor = %actor,
            action = %action,
            entity = %entity,
            "failed to write connect audit row: {err}"
        );
    }
}

/// Recursively drop object keys that look like credentials/PII. Non-object values
/// pass through unchanged; arrays and nested objects are walked. Pure.
pub fn sanitize_audit_metadata(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(key, _)| !is_sensitive_key(key))
                .map(|(key, val)| (key, sanitize_audit_metadata(val)))
                .collect(),
        ),
        Value::Array(items) => {
            Value::Array(items.into_iter().map(sanitize_audit_metadata).collect())
        }
        other => other,
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    SENSITIVE_KEY_FRAGMENTS
        .iter()
        .any(|fragment| lower.contains(fragment))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn drops_top_level_sensitive_keys() {
        let input = json!({
            "marketplace_id": "abc",
            "amount": 1000,
            "api_key": "mk_live_should_not_persist",
            "raw_key": "leak",
            "ownership_signature": "0xdeadbeef",
        });
        let out = sanitize_audit_metadata(input);
        assert_eq!(out["marketplace_id"], json!("abc"));
        assert_eq!(out["amount"], json!(1000));
        assert!(out.get("api_key").is_none());
        assert!(out.get("raw_key").is_none());
        // "ownership_signature" contains "signature".
        assert!(out.get("ownership_signature").is_none());
    }

    #[test]
    fn drops_nested_and_arrayed_sensitive_keys() {
        let input = json!({
            "wallet": { "address": "0xAbc", "private_key": "0xsecret" },
            "events": [
                { "kind": "mint", "token": "mk_live_x" },
                { "kind": "settle", "tx_hash": "0x1" }
            ]
        });
        let out = sanitize_audit_metadata(input);
        assert_eq!(out["wallet"]["address"], json!("0xAbc"));
        assert!(out["wallet"].get("private_key").is_none());
        assert!(out["events"][0].get("token").is_none());
        assert_eq!(out["events"][0]["kind"], json!("mint"));
        assert_eq!(out["events"][1]["tx_hash"], json!("0x1"));
    }

    #[test]
    fn keeps_safe_metadata_untouched() {
        let input = json!({ "status": "settled", "currency": "USDC", "fee_due": 60 });
        assert_eq!(sanitize_audit_metadata(input.clone()), input);
    }

    #[test]
    fn key_matching_is_case_insensitive() {
        let input = json!({ "API_KEY": "x", "Authorization": "Bearer y", "ok": 1 });
        let out = sanitize_audit_metadata(input);
        assert!(out.get("API_KEY").is_none());
        assert!(out.get("Authorization").is_none());
        assert_eq!(out["ok"], json!(1));
    }
}
