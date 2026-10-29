//! Running an approved plan, durably.
//!
//! A paid job must survive a closed browser, a dropped stream and a crashed
//! process, and must never pay twice for the same intent. That is not a
//! property of any one function; it is the shape of this module:
//!
//! ```text
//!   POST /v1/jobs      202 + job_id      the browser can leave now
//!        │
//!        ▼
//!   research_job_nodes  the work list, in Postgres
//!        │
//!        │ claim with a short lease      two workers cannot take one node
//!        ▼
//!   worker process      writes what it is about to do BEFORE doing it
//!        │
//!        │ recovery loop                 an expired lease is retaken, and
//!        ▼                               undecided payments are reconciled
//!                                        BEFORE any new spending
//! ```
//!
//! The scheduling rules themselves are pure and live in [`schedule`], so the
//! things the release gates ask about — that a failed essential node blocks
//! its branch, that independent branches still finish, that partial success
//! marks only what was proven — are tested as functions rather than inferred
//! from a running system.

pub mod ask;
pub mod attempt;
pub mod bind;
pub mod schedule;
pub mod worker;
