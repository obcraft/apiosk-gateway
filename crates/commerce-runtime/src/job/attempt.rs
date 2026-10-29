//! What is written down before a paid call goes out, and what an outcome means
//! afterwards.
//!
//! The discipline, in order:
//!
//! 1. Bind the node's inputs from validated facts.
//! 2. Hash the bound inputs and derive a stable idempotency key from them.
//! 3. WRITE THAT DOWN, with the reserved amount and the status, and commit.
//! 4. Only then make the request — with no database transaction open.
//! 5. Record the outcome.
//!
//! Step 3 before step 4 is the whole of crash safety. A process that dies
//! between them leaves a row saying what it was about to send; a process that
//! dies after them leaves a row saying it sent something and does not know
//! what came back, which is `unresolved` and is reconciled rather than retried.
//!
//! Step 4's parenthesis is not decoration. A network call inside an open
//! transaction holds a row lock for the length of a provider's worst day, and
//! the lease that lock was supposed to protect expires while it is held.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// sha256 over the bound inputs, canonically.
///
/// Canonical means the JSON is re-serialised from a parsed value with sorted
/// keys, so `{"a":1,"b":2}` and `{"b":2,"a":1}` hash the same. Two attempts
/// that would send the identical request must not look different to the
/// idempotency check because a map iterated in a different order.
pub fn input_hash(bound: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(canonical(bound).as_bytes());
    hex::encode(hasher.finalize())
}

fn canonical(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut parts: Vec<String> = map
                .iter()
                .map(|(key, value)| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_default(),
                        canonical(value)
                    )
                })
                .collect();
            parts.sort();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(items) => {
            // Order is meaning in an array — two names in a different order are
            // a different request — so it is preserved.
            format!(
                "[{}]",
                items.iter().map(canonical).collect::<Vec<_>>().join(",")
            )
        }
        other => other.to_string(),
    }
}

/// The key one attempt presents to the payment path.
///
/// Derived from the job, the node and the bound inputs — not from a random
/// value and not from the attempt number. A retry of the same request carries
/// the same key, which is what makes it a retry instead of a second purchase.
pub fn idempotency_key(job_id: &str, node_key: &str, input_hash: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(job_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(node_key.as_bytes());
    hasher.update(b"\0");
    hasher.update(input_hash.as_bytes());
    format!("job_{}", hex::encode(&hasher.finalize()[..16]))
}

/// Why an attempt may not be made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AttemptError {
    /// The same key was already used for DIFFERENT content. Refused rather
    /// than treated as a retry: reusing a key across different requests is how
    /// one payment silently answers for another.
    #[error("idempotency key {key} was used for different inputs")]
    KeyReused { key: String },
    /// The job has spent its request budget.
    #[error("this job has made {made} of {allowed} allowed requests")]
    OutOfRequests { made: u32, allowed: u32 },
    /// Spending this would exceed what was approved.
    #[error("this call would bring the job to {would_be}, above the approved {ceiling}")]
    OverCeiling { would_be: i128, ceiling: i128 },
    /// An earlier attempt's payment is still undecided.
    #[error("an earlier call on this job has an undecided payment; it is reconciled before more is spent")]
    UnresolvedPaymentOutstanding,
}

/// Everything the guard needs to decide, in one place so it cannot be checked
/// partially.
#[derive(Debug, Clone, PartialEq)]
pub struct Budget {
    pub requests_made: u32,
    pub max_requests: u32,
    /// Already settled plus still reserved, in atomic USDC.
    pub committed_atomic: i128,
    pub ceiling_atomic: i128,
    /// True when any node of this job is `unresolved`.
    pub has_unresolved_payment: bool,
}

/// May this node spend `cost_atomic` right now?
///
/// The order matters. An outstanding undecided payment is checked FIRST,
/// because a recovery worker reconciles before it allows new spending — a job
/// that does not know what it already spent cannot know whether the next call
/// fits under its ceiling.
pub fn may_spend(budget: &Budget, cost_atomic: i128) -> Result<(), AttemptError> {
    if budget.has_unresolved_payment {
        return Err(AttemptError::UnresolvedPaymentOutstanding);
    }
    if budget.requests_made >= budget.max_requests {
        return Err(AttemptError::OutOfRequests {
            made: budget.requests_made,
            allowed: budget.max_requests,
        });
    }
    let would_be = budget.committed_atomic.saturating_add(cost_atomic);
    if would_be > budget.ceiling_atomic {
        return Err(AttemptError::OverCeiling {
            would_be,
            ceiling: budget.ceiling_atomic,
        });
    }
    Ok(())
}

/// What came back from one attempt, before it is interpreted.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// A response arrived and was read.
    Answered {
        http_status: u16,
        charged_atomic: i128,
        /// The parsed body, for the contract to extract facts from.
        body: serde_json::Value,
    },
    /// The provider explicitly confirmed absence. A bare HTTP 404 does not
    /// establish this: it can also mean a filing product could not be delivered.
    NothingThere {
        http_status: u16,
        charged_atomic: i128,
    },
    /// The provider refused before charging.
    Refused { http_status: u16 },
    /// A non-success response with a confirmed charge. It cannot supply facts,
    /// but the charge must remain settled and must not trigger a free retry.
    FailedResponse {
        http_status: u16,
        charged_atomic: i128,
    },
    /// The request went out and we do not know what happened to it.
    Undecided,
}

/// Only an explicit, representable atomic amount from the gateway receipt.
pub fn settled_atomic(raw: &str) -> Option<i128> {
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    raw.parse::<i64>().ok().map(i128::from)
}

pub fn http_outcome(status: u16, charged: Option<i128>, body: Option<Value>) -> Outcome {
    // A status alone never proves that payment was released. Preserve evidence
    // of a charged-but-unreadable response as a settled partial result.
    //
    // The receipt header is the gateway's own word on money: `forward.rs` writes
    // `X-Apiosk-Buyer-Total-Atomic` on every response where a charge was
    // decided — the amount when settled, `0` when deferred and not taken — and
    // nothing else may write it. So a response WITHOUT it never reached the
    // point where money is decided. For a 4xx that is exactly the gateway's own
    // refusal, made before payment (`api_inactive`, `idempotency_scope_required`,
    // an unknown endpoint, a cap): nothing was sent, nothing was charged, and
    // the reservation goes back. Reading those as undecided is what used to
    // freeze a whole job on a single 404 from the proxy, since an undecided
    // payment stops every sibling node from being claimed.
    //
    // A 5xx without the header is NOT that proof: a proxy in front of the
    // gateway can answer 502 after the gateway paid and died. A 2xx without it
    // should not exist. Both stay undecided, and reconciliation reads the
    // gateway's idempotency ledger to decide them — see `worker.rs`.
    match charged {
        Some(amount) if (200..300).contains(&status) => Outcome::Answered {
            http_status: status,
            charged_atomic: amount,
            body: body.unwrap_or(Value::Null),
        },
        Some(0) => Outcome::Refused {
            http_status: status,
        },
        Some(amount) => Outcome::FailedResponse {
            http_status: status,
            charged_atomic: amount,
        },
        None if (400..500).contains(&status) => Outcome::Refused {
            http_status: status,
        },
        None => Outcome::Undecided,
    }
}

/// Where the money stands after one attempt. Mirrors the `payment_status`
/// constraint of `research_job_nodes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentStatus {
    Reserved,
    Settled,
    Released,
    Unresolved,
}

/// Read an outcome into a payment status and an amount.
///
/// The rule that is easy to get wrong: **an empty result does not make a
/// charged call free.** A failed filing request can still be billed for, and
/// releasing its reservation would mean the ledger says money
/// came back that did not. Compensating a buyer for a disappointing answer is a
/// separate, deliberate booking, not a side effect of reading a status code.
pub fn settle(outcome: &Outcome) -> (PaymentStatus, Option<i128>) {
    match outcome {
        Outcome::Answered { charged_atomic, .. }
        | Outcome::NothingThere { charged_atomic, .. }
        | Outcome::FailedResponse { charged_atomic, .. } => {
            (PaymentStatus::Settled, Some(*charged_atomic))
        }
        // Refused before anything was charged: the reservation goes back.
        Outcome::Refused { .. } => (PaymentStatus::Released, Some(0)),
        // The one case where the amount stays unknown. It is NOT zero, and it
        // is NOT the reserved amount either — nobody knows yet.
        Outcome::Undecided => (PaymentStatus::Unresolved, None),
    }
}

/// Whether an outcome may be retried.
///
/// An undecided payment is never retried by the same worker. Reconciliation
/// decides what happened first; retrying on a timeout is exactly how one
/// intent becomes two payments.
pub fn may_retry(outcome: &Outcome) -> bool {
    matches!(outcome, Outcome::Refused { http_status } if *http_status >= 500)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn budget() -> Budget {
        Budget {
            requests_made: 0,
            max_requests: 9,
            committed_atomic: 0,
            ceiling_atomic: 200_000,
            has_unresolved_payment: false,
        }
    }

    // --- idempotency --------------------------------------------------------

    #[test]
    fn the_same_request_hashes_the_same_whatever_the_key_order() {
        let a = json!({ "kvkNummer": "30204462", "geoData": false });
        let b = json!({ "geoData": false, "kvkNummer": "30204462" });
        assert_eq!(input_hash(&a), input_hash(&b));
    }

    #[test]
    fn a_different_request_hashes_differently() {
        assert_ne!(
            input_hash(&json!({ "kvkNummer": "30204462" })),
            input_hash(&json!({ "kvkNummer": "92327737" }))
        );
        // Order is meaning inside an array.
        assert_ne!(
            input_hash(&json!({ "namen": ["a", "b"] })),
            input_hash(&json!({ "namen": ["b", "a"] }))
        );
    }

    #[test]
    fn a_retry_of_the_same_request_carries_the_same_key() {
        let hash = input_hash(&json!({ "kvkNummer": "30204462" }));
        let first = idempotency_key("job-1", "profile", &hash);
        let second = idempotency_key("job-1", "profile", &hash);
        assert_eq!(
            first, second,
            "a retry must be a retry, not a second purchase"
        );
    }

    #[test]
    fn a_different_node_or_job_or_input_gets_a_different_key() {
        let hash = input_hash(&json!({ "kvkNummer": "30204462" }));
        let other = input_hash(&json!({ "kvkNummer": "92327737" }));
        let base = idempotency_key("job-1", "profile", &hash);
        assert_ne!(base, idempotency_key("job-2", "profile", &hash));
        assert_ne!(base, idempotency_key("job-1", "filing", &hash));
        assert_ne!(base, idempotency_key("job-1", "profile", &other));
    }

    // --- the spending guard -------------------------------------------------

    #[test]
    fn a_call_within_the_ceiling_is_allowed() {
        assert!(may_spend(&budget(), 76_086).is_ok());
    }

    #[test]
    fn a_call_that_would_cross_the_ceiling_is_refused_with_both_numbers() {
        let mut budget = budget();
        budget.committed_atomic = 190_000;
        let error = may_spend(&budget, 76_086).unwrap_err();
        let AttemptError::OverCeiling { would_be, ceiling } = error else {
            panic!("expected a ceiling refusal, got {error}");
        };
        assert_eq!(would_be, 266_086);
        assert_eq!(ceiling, 200_000);
    }

    #[test]
    fn spending_exactly_to_the_ceiling_is_allowed() {
        let mut budget = budget();
        budget.committed_atomic = 123_914;
        assert!(may_spend(&budget, 76_086).is_ok());
    }

    #[test]
    fn the_request_budget_is_a_ceiling_too() {
        let mut budget = budget();
        budget.requests_made = 9;
        assert!(matches!(
            may_spend(&budget, 1),
            Err(AttemptError::OutOfRequests {
                made: 9,
                allowed: 9
            })
        ));
    }

    #[test]
    fn an_undecided_payment_stops_all_new_spending_first() {
        // Checked before everything else: a job that does not know what it
        // already spent cannot know whether the next call fits.
        let mut budget = budget();
        budget.has_unresolved_payment = true;
        assert_eq!(
            may_spend(&budget, 1),
            Err(AttemptError::UnresolvedPaymentOutstanding)
        );
        // Even when there is plenty of room left.
        budget.committed_atomic = 0;
        budget.requests_made = 0;
        assert_eq!(
            may_spend(&budget, 1),
            Err(AttemptError::UnresolvedPaymentOutstanding)
        );
    }

    // --- reading an outcome -------------------------------------------------

    #[test]
    fn an_answer_settles_at_what_it_cost() {
        assert_eq!(
            settle(&Outcome::Answered {
                http_status: 200,
                charged_atomic: 76_086,
                body: json!({})
            }),
            (PaymentStatus::Settled, Some(76_086))
        );
    }

    #[test]
    fn an_empty_answer_does_not_make_a_charged_call_free() {
        // Two of three real production attempts at a filing returned 404. The
        // provider billed for them. Releasing the reservation would say money
        // came back that did not.
        assert_eq!(
            settle(&Outcome::NothingThere {
                http_status: 404,
                charged_atomic: 76_086
            }),
            (PaymentStatus::Settled, Some(76_086))
        );
    }

    #[test]
    fn a_refusal_before_charging_gives_the_reservation_back() {
        assert_eq!(
            settle(&Outcome::Refused { http_status: 422 }),
            (PaymentStatus::Released, Some(0))
        );
    }

    #[test]
    fn an_undecided_payment_has_no_amount_and_is_not_zero() {
        let (status, amount) = settle(&Outcome::Undecided);
        assert_eq!(status, PaymentStatus::Unresolved);
        assert_eq!(
            amount, None,
            "unknown is not zero and not the reserved amount"
        );
    }

    #[test]
    fn every_outcome_produces_a_pair_the_database_will_accept() {
        // `research_job_nodes` (migration 114) constrains the two together:
        // `unresolved` may carry no amount, `settled` and `released` must carry
        // one. If `settle` ever produced a pair outside that, every node it
        // wrote would fail its CHECK at run time — on a job that had already
        // spent money. This test is the two staying in step.
        for outcome in [
            Outcome::Answered {
                http_status: 200,
                charged_atomic: 76_086,
                body: json!({}),
            },
            Outcome::NothingThere {
                http_status: 404,
                charged_atomic: 76_086,
            },
            Outcome::Refused { http_status: 422 },
            Outcome::Undecided,
        ] {
            let (status, amount) = settle(&outcome);
            let accepted = match status {
                PaymentStatus::Unresolved => amount.is_none(),
                PaymentStatus::Settled | PaymentStatus::Released => amount.is_some(),
                PaymentStatus::Reserved => amount.is_none(),
            };
            assert!(
                accepted,
                "{outcome:?} yields {status:?}/{amount:?}, which migration 114 refuses"
            );
        }
    }

    #[test]
    fn a_timeout_is_never_retried() {
        // Retrying on a timeout is exactly how one intent becomes two payments.
        assert!(!may_retry(&Outcome::Undecided));
    }

    #[test]
    fn a_provider_error_may_be_retried_but_a_refusal_may_not() {
        assert!(may_retry(&Outcome::Refused { http_status: 503 }));
        assert!(!may_retry(&Outcome::Refused { http_status: 422 }));
        assert!(!may_retry(&Outcome::Answered {
            http_status: 200,
            charged_atomic: 1,
            body: json!({})
        }));
        assert!(!may_retry(&Outcome::NothingThere {
            http_status: 404,
            charged_atomic: 1
        }));
    }
}

#[cfg(test)]
mod clockwork_payment_tests {
    use super::*;
    #[test]
    fn a_filing_delivery_error_is_not_evidence_that_no_accounts_exist() {
        let body = serde_json::json!({
            "code": "IPD7002",
            "omschrijving": "Op basis van het depotId kan het deponeringsproduct niet worden geleverd."
        });
        let unpaid = http_outcome(404, Some(0), Some(body.clone()));
        assert_eq!(unpaid, Outcome::Refused { http_status: 404 });
        assert_eq!(settle(&unpaid), (PaymentStatus::Released, Some(0)));
        let paid = http_outcome(404, Some(100), Some(body));
        assert_eq!(
            paid,
            Outcome::FailedResponse {
                http_status: 404,
                charged_atomic: 100,
            }
        );
        assert_eq!(settle(&paid), (PaymentStatus::Settled, Some(100)));
        assert!(!may_retry(&paid));
    }
    #[test]
    fn payment_amount_is_exact_and_never_inferred_from_a_reservation() {
        assert_eq!(settled_atomic("1"), Some(1));
        assert_eq!(settled_atomic("76086"), Some(76086));
        for bad in ["-1", "0.0000001", "NaN", "", "9223372036854775808"] {
            assert_eq!(settled_atomic(bad), None);
        }
        assert_eq!(
            settle(&http_outcome(
                200,
                None,
                Some(serde_json::json!({"name":"Mollie"}))
            )),
            (PaymentStatus::Unresolved, None)
        );
    }
    #[test]
    fn charged_malformed_and_error_responses_are_never_free_retries() {
        for (status, body) in [(200, None), (502, None), (404, None)] {
            let outcome = http_outcome(status, Some(100), body);
            assert_eq!(settle(&outcome), (PaymentStatus::Settled, Some(100)));
            assert!(!may_retry(&outcome));
        }
        assert_eq!(
            settle(&http_outcome(502, None, None)),
            (PaymentStatus::Unresolved, None)
        );
        assert!(!may_retry(&http_outcome(502, None, None)));
    }
    #[test]
    fn the_gateways_own_refusal_before_payment_gives_the_reservation_back() {
        // No receipt header and a 4xx: the gateway said no before any money
        // was decided. Released, not undecided — an undecided payment stops
        // every other node of the job from being claimed, and one 404 from
        // the proxy used to freeze a whole research that way.
        for status in [400u16, 401, 404, 409, 422, 429] {
            let outcome = http_outcome(status, None, None);
            assert_eq!(
                outcome,
                Outcome::Refused {
                    http_status: status
                },
                "{status}"
            );
            assert_eq!(settle(&outcome), (PaymentStatus::Released, Some(0)));
            assert!(!may_retry(&outcome));
        }
        // A 5xx without the header proves nothing either way: something in
        // front of the gateway may have answered after the gateway paid.
        for status in [500u16, 502, 503, 504] {
            assert_eq!(
                settle(&http_outcome(status, None, None)),
                (PaymentStatus::Unresolved, None),
                "{status}"
            );
        }
    }
}
