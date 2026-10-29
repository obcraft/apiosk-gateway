//! The evaluator, ported case for case from the buyer portal's
//! `src/lib/policy/evaluate.ts`.
//!
//! Read the port alongside the original. Where a comparison looks odd here it
//! is because it is odd there, and the two agreeing matters more than either
//! being tidy.

use std::collections::HashSet;

use crate::db::PolicyRules;
use crate::proxy::domain_policy::host_is_allowed;

use super::rules::{
    PolicyDecision, PolicyFinding, PolicyVerdict, PurchaseRequest, RemainingHeadroom, RuleId,
    SpendSnapshot,
};

/// USD amounts are IEEE doubles carrying six-decimal USDC values, so
/// `0.1 + 0.2 > 0.3` is a live hazard on a daily-budget check. Compare with a
/// tolerance well below a millionth of a dollar: too small to forgive a real
/// overspend, large enough to absorb representation error.
const EPSILON: f64 = 1e-9;

fn exceeds(value: f64, limit: f64) -> bool {
    value - limit > EPSILON
}

/// The 27 EU member states, for the `EU` pseudo-region.
const EU_MEMBER_STATES: [&str; 27] = [
    "AT", "BE", "BG", "CY", "CZ", "DE", "DK", "EE", "ES", "FI", "FR", "GR", "HR", "HU", "IE", "IT",
    "LT", "LU", "LV", "MT", "NL", "PL", "PT", "RO", "SE", "SI", "SK",
];

/// Expand `EU` into member states; leave explicit ISO codes alone.
pub fn expand_regions(regions: &[String]) -> HashSet<String> {
    let mut expanded = HashSet::new();
    for region in regions {
        let code = region.trim().to_uppercase();
        if code.is_empty() {
            continue;
        }
        if code == "EU" {
            for member in EU_MEMBER_STATES {
                expanded.insert(member.to_string());
            }
        } else {
            expanded.insert(code);
        }
    }
    expanded
}

fn money(value: f64) -> String {
    if value < 0.01 && value > 0.0 {
        format!("${value:.4}")
    } else {
        format!("${value:.2}")
    }
}

fn contains_ignore_case(list: &[String], needle: &str) -> bool {
    list.iter()
        .any(|entry| entry.trim().eq_ignore_ascii_case(needle))
}

/// Judge one purchase against one rule set.
///
/// Pure: no database, no clock, no network. `spend` comes from the caller.
pub fn evaluate_policy(
    rules: &PolicyRules,
    request: &PurchaseRequest,
    spend: SpendSnapshot,
) -> PolicyVerdict {
    let mut findings: Vec<PolicyFinding> = Vec::new();

    let mut add = |rule: RuleId, message: String| {
        findings.push(PolicyFinding {
            rule,
            decision: rule.verdict(),
            message,
            enforcement: rule.enforcement(),
        });
    };

    let amount = if request.amount_usd.is_finite() {
        request.amount_usd.max(0.0)
    } else {
        0.0
    };
    let unit_price = match request.unit_price_usd {
        Some(price) if price.is_finite() => price.max(0.0),
        _ => amount,
    };

    // --- Amount ceilings ----------------------------------------------------

    if let Some(cap) = rules.max_per_transaction_usd {
        if exceeds(amount, cap) {
            add(
                RuleId::MaxPerTransactionUsd,
                format!(
                    "{} exceeds the {} per-transaction cap.",
                    money(amount),
                    money(cap)
                ),
            );
        }
    }

    if let Some(cap) = rules.max_price_per_call_usd {
        if exceeds(unit_price, cap) {
            add(
                RuleId::MaxPricePerCallUsd,
                format!(
                    "{} per call exceeds the {} unit-price cap.",
                    money(unit_price),
                    money(cap)
                ),
            );
        }
    }

    // --- Budgets ------------------------------------------------------------
    // Checked as "spent + this purchase", never "spent", so the purchase that
    // would breach the cap is the one refused — not the one after it.

    if let Some(cap) = rules.max_per_day_usd {
        if exceeds(spend.spent_today_usd + amount, cap) {
            add(
                RuleId::MaxPerDayUsd,
                format!(
                    "{} already spent today; {} more would pass the {} daily budget.",
                    money(spend.spent_today_usd),
                    money(amount),
                    money(cap)
                ),
            );
        }
    }

    if let Some(cap) = rules.max_per_month_usd {
        if exceeds(spend.spent_this_month_usd + amount, cap) {
            add(
                RuleId::MaxPerMonthUsd,
                format!(
                    "{} already spent this month; {} more would pass the {} monthly budget.",
                    money(spend.spent_this_month_usd),
                    money(amount),
                    money(cap)
                ),
            );
        }
    }

    if let Some(cap) = rules.max_calls_per_day {
        if spend.calls_today + 1 > cap {
            add(
                RuleId::MaxCallsPerDay,
                format!(
                    "{} purchases already today. The limit is {cap} per day.",
                    spend.calls_today
                ),
            );
        }
    }

    // --- Destination --------------------------------------------------------

    let host = request
        .destination_host
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_lowercase();

    if !rules.blocked_domains.is_empty() {
        // A blocklist with an unreadable host cannot match, so it cannot refuse.
        // That gap is closed by the allowlist branch below when one is set.
        if !host.is_empty() && host_is_allowed(&rules.blocked_domains, &host) {
            add(
                RuleId::BlockedDomains,
                format!("{host} is on this policy's blocked-domain list."),
            );
        }
    }

    if !rules.allowed_domains.is_empty() {
        if host.is_empty() {
            add(
                RuleId::AllowedDomains,
                "The destination host could not be read, and this policy only allows named domains."
                    .to_string(),
            );
        } else if !host_is_allowed(&rules.allowed_domains, &host) {
            add(
                RuleId::AllowedDomains,
                format!("{host} is not on this policy's allowed-domain list."),
            );
        }
    }

    // --- Seller -------------------------------------------------------------

    let seller = request
        .seller
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_lowercase();

    if !rules.blocked_sellers.is_empty()
        && !seller.is_empty()
        && contains_ignore_case(&rules.blocked_sellers, &seller)
    {
        add(
            RuleId::BlockedSellers,
            format!("{seller} is on this policy's blocked-seller list."),
        );
    }

    if !rules.allowed_sellers.is_empty() {
        if seller.is_empty() {
            add(
                RuleId::AllowedSellers,
                "The seller is unknown, and this policy only allows named sellers.".to_string(),
            );
        } else if !contains_ignore_case(&rules.allowed_sellers, &seller) {
            add(
                RuleId::AllowedSellers,
                format!("{seller} is not on this policy's allowed-seller list."),
            );
        }
    }

    // --- Jurisdiction -------------------------------------------------------

    if !rules.allowed_regions.is_empty() {
        let allowed = expand_regions(&rules.allowed_regions);
        let region = request
            .seller_region
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_uppercase();
        if region.is_empty() {
            add(
                RuleId::AllowedRegions,
                "This seller's jurisdiction is unknown, and this policy only allows named regions."
                    .to_string(),
            );
        } else if !allowed.contains(&region) {
            add(
                RuleId::AllowedRegions,
                format!("Seller is in {region}, which this policy doesn't allow."),
            );
        }
    }

    // --- Rail ---------------------------------------------------------------

    if !rules.allowed_assets.is_empty() {
        let asset = request
            .asset
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_uppercase();
        if asset.is_empty() {
            add(
                RuleId::AllowedAssets,
                "The settlement asset is unknown, and this policy names allowed assets."
                    .to_string(),
            );
        } else if !contains_ignore_case(&rules.allowed_assets, &asset) {
            add(
                RuleId::AllowedAssets,
                format!("{asset} is not an allowed settlement asset."),
            );
        }
    }

    if !rules.allowed_networks.is_empty() {
        let network = request
            .network
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        if network.is_empty() {
            add(
                RuleId::AllowedNetworks,
                "The settlement network is unknown, and this policy names allowed networks."
                    .to_string(),
            );
        } else if !contains_ignore_case(&rules.allowed_networks, &network) {
            add(
                RuleId::AllowedNetworks,
                format!("{network} is not an allowed settlement network."),
            );
        }
    }

    // --- Shape of the charge ------------------------------------------------

    if !rules.allow_subscriptions && request.recurring {
        add(
            RuleId::AllowSubscriptions,
            "This is a recurring charge, and this policy allows one-off purchases only."
                .to_string(),
        );
    }

    // --- Holds --------------------------------------------------------------
    // Evaluated last so a purchase that is both over a hard cap and over the
    // approval threshold reads, in the findings list, as denied-then-would-have-
    // asked rather than the other way round.

    if let Some(threshold) = rules.approval_threshold_usd {
        if amount >= threshold - EPSILON {
            add(
                RuleId::ApprovalThresholdUsd,
                format!(
                    "{} is at or above your {} approval threshold.",
                    money(amount),
                    money(threshold)
                ),
            );
        }
    }

    if rules.require_approval_for_new_sellers && request.seller_seen_before == Some(false) {
        let who = if seller.is_empty() {
            "this seller".to_string()
        } else {
            seller.clone()
        };
        add(
            RuleId::RequireApprovalForNewSellers,
            format!("You have never paid {who} before."),
        );
    }

    // --- Verdict ------------------------------------------------------------

    let mut ordered: Vec<PolicyFinding> = findings
        .iter()
        .filter(|f| f.decision == PolicyDecision::Deny)
        .cloned()
        .collect();
    let holds: Vec<PolicyFinding> = findings
        .iter()
        .filter(|f| f.decision == PolicyDecision::RequireApproval)
        .cloned()
        .collect();

    let decision = if !ordered.is_empty() {
        PolicyDecision::Deny
    } else if !holds.is_empty() {
        PolicyDecision::RequireApproval
    } else {
        PolicyDecision::Allow
    };
    ordered.extend(holds);

    let matched = ordered.first().cloned();

    PolicyVerdict {
        decision,
        findings: ordered,
        matched,
        remaining: RemainingHeadroom {
            // Headroom after this purchase lands. Clamped at 0: a negative
            // number here would be a budget that was already breached, which
            // cannot happen on an allowed purchase and reads as nonsense on a
            // denied one.
            day_usd: rules
                .max_per_day_usd
                .map(|cap| (cap - spend.spent_today_usd - amount).max(0.0)),
            month_usd: rules
                .max_per_month_usd
                .map(|cap| (cap - spend.spent_this_month_usd - amount).max(0.0)),
            transaction_usd: rules
                .max_per_transaction_usd
                .map(|cap| (cap - amount).max(0.0)),
            calls_today: rules
                .max_calls_per_day
                .map(|cap| (cap - spend.calls_today - 1).max(0)),
        },
        unevaluated_rules: rules.unknown_rule_names(),
    }
}

/// Evaluate against a whole policy record, honouring its `enabled` switch.
///
/// A disabled policy allows everything rather than denying everything. Turning a
/// policy off is a buyer parking a rule set they are still editing; if that
/// stopped their agent dead, nobody would ever risk the toggle. The wallet's own
/// caps still apply underneath, so there is no state in which an agent spends
/// completely uncapped.
pub fn decide_purchase(
    enabled: bool,
    rules: &PolicyRules,
    request: &PurchaseRequest,
    spend: SpendSnapshot,
) -> PolicyVerdict {
    if !enabled {
        return PolicyVerdict {
            decision: PolicyDecision::Allow,
            findings: Vec::new(),
            matched: None,
            remaining: RemainingHeadroom::default(),
            unevaluated_rules: Vec::new(),
        };
    }
    evaluate_policy(rules, request, spend)
}
