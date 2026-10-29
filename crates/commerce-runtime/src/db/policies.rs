//! Reads and writes for the purchase policy engine (migration 083).
//!
//! Queries only. No rule is interpreted here — that lives in `crate::policy`,
//! ported from the buyer portal's `src/lib/policy/evaluate.ts` so the portal's
//! simulator and this gateway answer the same question the same way. Putting
//! even one threshold comparison in this file would be the start of the second
//! implementation this whole plan exists to prevent.
//!
//! Everything is best-effort against a database that has not run the migration.
//! Same reasoning as `usage::get_connect_token_allowed_domains`: this binary
//! can ship ahead of the schema, and a gateway that refuses every paid call
//! because a policy table is missing has turned an optional feature into an
//! outage. No policy means today's behaviour — the compiled per-transaction and
//! daily caps in `proxy::wallet`, which are enforced either way.

use super::*;
use crate::db::{PolicyDecisionInsert, PolicyRules, PurchasePolicy};

/// Whether the policy tables exist, probed once and then remembered.
///
/// A missing table is a fact about the deployment and will not change while the
/// process runs, so it is latched. Any other error is transient — a connection
/// blip must not disable every buyer's policy for the lifetime of a process.
mod availability {
    use std::sync::atomic::{AtomicU8, Ordering};

    pub const UNKNOWN: u8 = 0;
    pub const PRESENT: u8 = 1;
    pub const ABSENT: u8 = 2;

    static STATE: AtomicU8 = AtomicU8::new(UNKNOWN);

    pub fn is_absent() -> bool {
        STATE.load(Ordering::Relaxed) == ABSENT
    }

    pub fn mark_present() {
        STATE.store(PRESENT, Ordering::Relaxed);
    }

    /// Returns true the first time it latches, so the log line is written once.
    pub fn mark_absent() -> bool {
        STATE.swap(ABSENT, Ordering::Relaxed) != ABSENT
    }
}

/// Postgres `undefined_table` (42P01), i.e. migration 083 has not been applied.
fn is_undefined_table(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(|db_err| db_err.code())
        .map(|code| code == "42P01" || code == "42703")
        .unwrap_or(false)
}

#[derive(sqlx::FromRow)]
struct PolicyRow {
    id: Uuid,
    user_id: Uuid,
    name: String,
    rules: serde_json::Value,
    version: i32,
    enabled: bool,
}

impl PolicyRow {
    /// A policy whose `rules` will not deserialise is a policy this gateway
    /// cannot reason about. It becomes an empty rule set rather than an error:
    /// the compiled caps still apply, and an unparseable blob must not take out
    /// the payment path.
    fn into_policy(self) -> PurchasePolicy {
        let rules = serde_json::from_value::<PolicyRules>(self.rules).unwrap_or_else(|err| {
            tracing::warn!(policy_id = %self.id, "purchase policy rules did not parse: {err}");
            PolicyRules::default()
        });
        PurchasePolicy {
            id: self.id,
            user_id: self.user_id,
            name: self.name,
            rules,
            version: self.version,
            enabled: self.enabled,
        }
    }
}

const SELECT_POLICY: &str = "SELECT p.id, p.user_id, p.name, p.rules, p.version, p.enabled \
                             FROM purchase_policies p ";

impl Database {
    /// The policy governing a connection, or `None` when it has no policy, the
    /// policy is disabled, or the engine is not on this database.
    ///
    /// A disabled policy returns `None` on purpose rather than an empty rule
    /// set: `enabled = false` means "go back to what the columns say", which is
    /// exactly what no policy means, and collapsing the two here keeps every
    /// caller from having to know the difference.
    pub async fn load_policy_for_connect_token(&self, token_id: Uuid) -> Option<PurchasePolicy> {
        self.load_policy(
            &format!(
                "{SELECT_POLICY} JOIN agent_wallet_connect_tokens t ON t.policy_id = p.id \
                 WHERE t.id = $1 AND p.enabled LIMIT 1"
            ),
            token_id,
            "connect token",
        )
        .await
    }

    /// The policy attached to a wallet. Read when a connection has none of its
    /// own — the wallet's policy is the fallback, not an addition.
    pub async fn load_policy_for_wallet(&self, wallet_id: Uuid) -> Option<PurchasePolicy> {
        self.load_policy(
            &format!(
                "{SELECT_POLICY} JOIN agent_wallets w ON w.policy_id = p.id \
                 WHERE w.id = $1 AND p.enabled LIMIT 1"
            ),
            wallet_id,
            "wallet",
        )
        .await
    }

    async fn load_policy(&self, sql: &str, id: Uuid, what: &str) -> Option<PurchasePolicy> {
        if availability::is_absent() {
            return None;
        }

        match sqlx::query_as::<_, PolicyRow>(sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
        {
            Ok(row) => {
                availability::mark_present();
                row.map(PolicyRow::into_policy)
            }
            Err(err) if is_undefined_table(&err) => {
                if availability::mark_absent() {
                    tracing::info!(
                        "purchase policy tables are absent; policies are not enforced until \
                         migration 083 is applied. Compiled per-transaction and daily caps still apply."
                    );
                }
                None
            }
            Err(err) => {
                // Transient. Do NOT latch: latching here would silently disable
                // every buyer's policy for the life of the process because of
                // one bad round-trip.
                tracing::warn!("could not load purchase policy for {what} {id}: {err}");
                None
            }
        }
    }

    /// Append one verdict to the log.
    ///
    /// Never fails a request. A purchase that was correctly allowed must not be
    /// refused because the audit write did not land — but the failure is logged
    /// loudly, because a decision log with holes in it is worth less than one
    /// that is known to be complete.
    pub async fn insert_policy_decision(&self, input: PolicyDecisionInsert<'_>) {
        if availability::is_absent() {
            return;
        }

        let result = sqlx::query(
            r#"INSERT INTO purchase_policy_decisions
                 (user_id, policy_id, policy_version, token_id, wallet_id, decision,
                  matched_rule, reason, amount_usd, destination_host, seller, source)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'gateway')"#,
        )
        .bind(input.user_id)
        .bind(input.policy_id)
        .bind(input.policy_version)
        .bind(input.token_id)
        .bind(input.wallet_id)
        .bind(input.decision)
        .bind(input.matched_rule)
        .bind(input.reason)
        .bind(input.amount_usd)
        .bind(input.destination_host)
        .bind(input.seller)
        .execute(&self.pool)
        .await;

        match result {
            Ok(_) => availability::mark_present(),
            Err(err) if is_undefined_table(&err) => {
                if availability::mark_absent() {
                    tracing::info!(
                        "purchase_policy_decisions is absent; verdicts are not being logged \
                         until migration 083 is applied"
                    );
                }
            }
            Err(err) => {
                tracing::error!(
                    decision = input.decision,
                    "could not write purchase policy decision: {err}"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property the whole `jsonb` decision rests on.
    ///
    /// A policy carrying a rule this binary has never heard of must parse, keep
    /// the rule, and be reportable — not error, and not silently drop it. The
    /// portal ships a new rule type without a migration, so an older gateway
    /// reading a newer policy is the ordinary case, not the exotic one.
    #[test]
    fn an_unknown_rule_survives_the_round_trip() {
        let raw = serde_json::json!({
            "maxPerTransactionUsd": 1.5,
            "maxPerDayUsd": 20.0,
            "allowedAssets": ["USDC"],
            // Invented here, and deliberately never added to PolicyRules.
            "maxPurchasesPerFortnight": 3,
            "requireSignedProvenance": true,
        });

        let rules: PolicyRules = serde_json::from_value(raw.clone()).expect("must parse");

        assert_eq!(rules.max_per_transaction_usd, Some(1.5));
        assert_eq!(rules.max_per_day_usd, Some(20.0));
        assert_eq!(rules.allowed_assets, vec!["USDC".to_string()]);

        // Kept, not dropped.
        assert_eq!(
            rules.unknown_rule_names(),
            vec![
                "maxPurchasesPerFortnight".to_string(),
                "requireSignedProvenance".to_string()
            ]
        );

        // And still there on the way back out, so a decision row can show what
        // this gateway did not evaluate.
        let back = serde_json::to_value(&rules).expect("must serialise");
        assert_eq!(back["maxPurchasesPerFortnight"], serde_json::json!(3));
        assert_eq!(back["requireSignedProvenance"], serde_json::json!(true));
    }

    #[test]
    fn an_empty_policy_is_a_policy_with_no_rules() {
        // `rules: {}` is the column default. It must mean "nothing set", not a
        // parse failure that takes out the payment path.
        let rules: PolicyRules = serde_json::from_value(serde_json::json!({})).expect("must parse");
        assert_eq!(rules.max_per_transaction_usd, None);
        assert!(rules.allowed_domains.is_empty());
        assert!(rules.unknown_rule_names().is_empty());
        // The portal's own default: subscriptions off unless asked for.
        assert!(!rules.allow_subscriptions);
    }

    #[test]
    fn a_null_rule_reads_as_unset_rather_than_as_an_error() {
        // TypeScript writes `null` for an unset number. serde would reject that
        // for a bare Option without `default`, which is why the struct carries
        // one.
        let rules: PolicyRules = serde_json::from_value(serde_json::json!({
            "maxPerTransactionUsd": null,
            "approvalThresholdUsd": null,
        }))
        .expect("must parse");
        assert_eq!(rules.max_per_transaction_usd, None);
        assert_eq!(rules.approval_threshold_usd, None);
    }
}
