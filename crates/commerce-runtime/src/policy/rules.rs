//! The vocabulary of a verdict: which rules exist, what each one produces, and
//! what a caller has to tell us about a purchase.
//!
//! `RuleId` and its metadata mirror the portal's `src/lib/policy/types.ts`. The
//! enforcement flag in particular is a promise made to a buyer on screen: the
//! portal labels every rule with whether it actually blocks, and if the two
//! files disagree the portal is telling somebody their money is guarded when it
//! is not.

use std::fmt;

/// Every rule this gateway knows how to evaluate.
///
/// A policy may carry rules that are not here — see `PolicyRules::extra` in
/// `db::models`. Those are kept and reported, never evaluated and never
/// silently treated as satisfied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleId {
    MaxPerTransactionUsd,
    MaxPricePerCallUsd,
    MaxPerDayUsd,
    MaxPerMonthUsd,
    MaxCallsPerDay,
    ApprovalThresholdUsd,
    RequireApprovalForNewSellers,
    AllowedDomains,
    BlockedDomains,
    AllowedSellers,
    BlockedSellers,
    AllowedRegions,
    AllowedAssets,
    AllowedNetworks,
    AllowSubscriptions,
}

impl RuleId {
    /// The name the portal, the decision log and the buyer all use. Changing one
    /// of these breaks the join between a gateway verdict and the rule a buyer
    /// can see in their editor.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MaxPerTransactionUsd => "maxPerTransactionUsd",
            Self::MaxPricePerCallUsd => "maxPricePerCallUsd",
            Self::MaxPerDayUsd => "maxPerDayUsd",
            Self::MaxPerMonthUsd => "maxPerMonthUsd",
            Self::MaxCallsPerDay => "maxCallsPerDay",
            Self::ApprovalThresholdUsd => "approvalThresholdUsd",
            Self::RequireApprovalForNewSellers => "requireApprovalForNewSellers",
            Self::AllowedDomains => "allowedDomains",
            Self::BlockedDomains => "blockedDomains",
            Self::AllowedSellers => "allowedSellers",
            Self::BlockedSellers => "blockedSellers",
            Self::AllowedRegions => "allowedRegions",
            Self::AllowedAssets => "allowedAssets",
            Self::AllowedNetworks => "allowedNetworks",
            Self::AllowSubscriptions => "allowSubscriptions",
        }
    }

    /// What breaking this rule produces. Only two rules ask a human; everything
    /// else refuses, which is why an approvable purchase that also breaks a hard
    /// cap is refused rather than queued.
    pub fn verdict(self) -> PolicyDecision {
        match self {
            Self::ApprovalThresholdUsd | Self::RequireApprovalForNewSellers => {
                PolicyDecision::RequireApproval
            }
            _ => PolicyDecision::Deny,
        }
    }

    /// Whether the gateway refuses on this rule today.
    ///
    /// Every rule here is `Gateway` from `gateway/01` onward: that is what this
    /// task did. It stays as an explicit method rather than a constant `true`
    /// because the portal's matching table is what a buyer reads, and the day a
    /// rule is added on one side and not the other, this is where the two are
    /// compared.
    pub fn enforcement(self) -> RuleEnforcement {
        RuleEnforcement::Gateway
    }
}

impl fmt::Display for RuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // `Portal` is unconstructed by design — see below.
pub enum RuleEnforcement {
    /// The gateway refuses the purchase.
    Gateway,
    /// Recorded and shown, not blocking. No rule is here after gateway/01; the
    /// variant stays so a rule can be introduced in the portal ahead of the
    /// gateway again without inventing this distinction a second time.
    Portal,
}

/// One of three answers, never a boolean.
///
/// A boolean cannot express "ask a person", which is the whole reason the
/// approvals branch exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyDecision {
    Allow,
    Deny,
    RequireApproval,
}

impl PolicyDecision {
    /// The value written to `purchase_policy_decisions.decision`, which the
    /// database checks against exactly these three.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::RequireApproval => "require_approval",
        }
    }
}

/// One rule's objection, in words a buyer can act on.
#[derive(Debug, Clone)]
pub struct PolicyFinding {
    pub rule: RuleId,
    pub decision: PolicyDecision,
    pub message: String,
    /// Read by the surface that explains a block to the buyer; the enforcement
    /// decision itself is already carried by `decision`.
    #[allow(dead_code)]
    pub enforcement: RuleEnforcement,
}

/// The purchase being judged. Everything but the amount is best-effort — and a
/// rule that needs a missing field refuses rather than passes.
#[derive(Debug, Clone, Default)]
pub struct PurchaseRequest {
    /// Total the agent wants to spend, in USD.
    pub amount_usd: f64,
    /// Price of one call. Defaults to `amount_usd` for a single-call purchase.
    pub unit_price_usd: Option<f64>,
    /// Destination host as the gateway sees it: the upstream origin, not
    /// gateway.apiosk.com.
    pub destination_host: Option<String>,
    /// Apiosk API slug or seller id.
    pub seller: Option<String>,
    /// Seller jurisdiction, ISO-3166 alpha-2.
    pub seller_region: Option<String>,
    pub asset: Option<String>,
    pub network: Option<String>,
    /// Whether this charge recurs.
    pub recurring: bool,
    /// Whether this buyer has paid this seller before. Drives the new-seller
    /// hold. `None` means unknown, which does not trigger the hold — the portal
    /// behaves the same way, because holding every purchase from a buyer whose
    /// history we failed to read would be a database blip stopping their agent.
    pub seller_seen_before: Option<bool>,
}

/// What has already been spent, supplied by the caller.
///
/// The portal reads this from `gateway_usage`; the gateway reads it inside the
/// same transaction that settles. Passing it in is what keeps the evaluator
/// pure.
#[derive(Debug, Clone, Copy, Default)]
pub struct SpendSnapshot {
    pub spent_today_usd: f64,
    pub spent_this_month_usd: f64,
    pub calls_today: i64,
}

/// Headroom after this purchase lands, for a caller that wants to say what is
/// left rather than only what is allowed.
///
/// Computed on every verdict but not yet surfaced — `gateway/10` returns it on
/// the decision response.
#[derive(Debug, Clone, Copy, Default)]
#[allow(dead_code)]
pub struct RemainingHeadroom {
    pub day_usd: Option<f64>,
    pub month_usd: Option<f64>,
    pub transaction_usd: Option<f64>,
    pub calls_today: Option<i64>,
}

/// The answer, and the reasoning behind it.
#[derive(Debug, Clone)]
pub struct PolicyVerdict {
    pub decision: PolicyDecision,
    /// Every objection, denials first. Not just the first to trip: a buyer
    /// debugging a blocked agent wants the list, not a guessing game.
    #[allow(dead_code)]
    pub findings: Vec<PolicyFinding>,
    /// The one that decided it, for the log and for the message.
    pub matched: Option<PolicyFinding>,
    #[allow(dead_code)]
    pub remaining: RemainingHeadroom,
    /// Rules present in the policy that this binary could not evaluate.
    ///
    /// Reported rather than swallowed. A buyer whose newest rule is not being
    /// applied because the gateway has not shipped it yet deserves a way to
    /// find that out that is not "money left the account".
    pub unevaluated_rules: Vec<String>,
}

impl PolicyVerdict {
    /// Callers currently match on `decision` directly; kept because a bool is
    /// what the enforce layer wants once it is wired.
    #[allow(dead_code)]
    pub fn is_allowed(&self) -> bool {
        self.decision == PolicyDecision::Allow
    }

    /// One sentence for the buyer and for the decision row.
    pub fn reason(&self) -> Option<&str> {
        self.matched
            .as_ref()
            .map(|finding| finding.message.as_str())
    }

    pub fn matched_rule(&self) -> Option<&'static str> {
        self.matched.as_ref().map(|finding| finding.rule.as_str())
    }
}
