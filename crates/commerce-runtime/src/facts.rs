//! Typed facts: the vocabulary a plan is written in, and the rules that turn a
//! provider's response body into one.
//!
//! Migration 112 adds the two tables this module reads: `fact_types`, the
//! shared vocabulary, and `capability_execution_contracts`, the per-offer
//! versioned contract that says which facts a call requires, which it
//! produces, and where in the response body each one lives.
//!
//! ```text
//!   response body ──▶ path::Path      where the value is
//!                 ──▶ schema::validate whether it is that kind of value
//!                 ──▶ contract        whose subject it is about, and whether
//!                                     there is more than one of them
//! ```
//!
//! Two rules run through all three, and they are the reason this is a module
//! and not a `serde_json::pointer` call:
//!
//!   * A schema error yields no fact. Not a fact marked invalid, not a fact
//!     with a warning — nothing. Everything downstream may assume a `Fact` it
//!     holds has passed its type's schema.
//!   * A lookup with several plausible subjects is never silently narrowed to
//!     one. It comes back as an ambiguity a person resolves, with the already
//!     paid lookup kept so resolving it costs nothing extra.

pub mod adapter;
pub mod contract;
pub mod path;
pub mod schema;
