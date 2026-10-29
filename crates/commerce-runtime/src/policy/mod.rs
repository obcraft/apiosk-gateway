//! The purchase policy engine: the decision, before money moves.
//!
//! ```text
//!     Agent wants to spend $0.08
//!             ↓
//!        evaluate_policy()
//!             ↓
//!     ALLOW / DENY / REQUIRE_APPROVAL
//! ```
//!
//! This is a PORT, not a reinvention. The reference implementation is the buyer
//! portal's `src/lib/policy/evaluate.ts`, which runs in the policy simulator and
//! in the preview a buyer sees when they attach a policy to a connection. The
//! two must agree, because the portal shows a buyer what will happen and this
//! decides what does happen — and the only way to guarantee agreement is one
//! behaviour described by one test suite run against both. `evaluate_test.rs`
//! is that suite, ported case for case from `evaluate.test.ts`.
//!
//! Where they disagree, this file wins and the portal is the bug. Same contract
//! `proxy::domain_policy` already has with the portal's `allowed-domains.ts`.
//!
//! Three properties are load-bearing:
//!
//! 1. **Every rule is evaluated, not just the first to trip.** A buyer debugging
//!    a blocked agent wants the whole list of reasons, not a guessing game. The
//!    verdict picks a winner; the findings keep the rest.
//! 2. **DENY beats REQUIRE_APPROVAL beats ALLOW.** An approvable purchase that
//!    also breaks a hard cap is refused outright — asking a human to approve
//!    something the policy already forbids trains them to rubber-stamp.
//! 3. **Missing context never silently passes.** If a rule needs a fact the
//!    caller did not supply (no seller region while regions are restricted), the
//!    purchase is refused. An allowlist a missing field can bypass is not an
//!    allowlist.
//!
//! Pure functions only: no database, no network, no clock. The caller supplies
//! the spend snapshot, which keeps "who counts the money" at the edge where it
//! belongs and keeps this testable without a fixture server.

pub mod enforce;
pub mod evaluate;
pub mod rules;

#[cfg(test)]
mod evaluate_test;

// The payment path needs exactly two of these. Everything else stays reachable
// by path (`policy::evaluate::…`), which keeps a re-export from quietly becoming
// an invitation to call the evaluator from somewhere that has no business
// deciding whether money moves.
pub use enforce::{enforce_policy, PurchaseContext};
