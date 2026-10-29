//! The signed, immutable authorisation to run one exact plan version.
//!
//! A `plan_token` is what turns "here is a plan and a price" into "this buyer
//! agreed to this work at this ceiling". It is held by the caller, handed back
//! to `POST /v1/jobs`, and cannot be edited on the way — which is the whole
//! reason it is signed rather than just looked up.
//!
//! ```text
//!   plan version ──sign──▶ plan_token ──hand back──▶ POST /v1/jobs
//!                                                      │
//!                                        verify ───────┤ owner still matches?
//!                                                      │ hash still matches?
//!                                                      │ not expired?
//!                                                      ▼
//!                                                  one reservation
//! ```
//!
//! ---------------------------------------------------------------------------
//! What is inside, and why each field is
//! ---------------------------------------------------------------------------
//!
//! Everything the executing job must not be able to change:
//!
//! * **owner** — a token minted for one buyer cannot start another's job.
//! * **plan id and version, and the plan hash** — the hash is what makes the
//!   token specific to the WORK rather than to a row id. A version row cannot
//!   be edited (migration 113 has no update path), but the hash means a token
//!   would not survive it if one ever could.
//! * **buyer-total ceiling** — in atomic USDC. The reservation holds exactly
//!   this. It is not a hint.
//! * **max requests and retries** — a plan of three nodes that may retry
//!   forever has no ceiling in practice, whatever the amount says.
//! * **price and fee version** — a token signed under one fee schedule must
//!   not be redeemed under another. Fees change; approvals do not change with
//!   them.
//! * **format** — a costlier format is a different approval.
//! * **issued and expiry** — a quote goes stale, and a stale quote is not one
//!   we can honour.
//!
//! What is NOT inside: any provider price, any fee breakdown, and anything a
//! model produced. A model cannot infer consent from provider data, and a token
//! is consent.
//!
//! ---------------------------------------------------------------------------
//! Signing, and what verification deliberately does not tell you
//! ---------------------------------------------------------------------------
//!
//! Same construction as the `offer_id` in `v1_routes::quote`: base64url of the
//! JSON claim, a dot, and an HMAC-SHA256 tag over the encoded claim, verified
//! constant-time. A forged token, a token signed by another gateway and a
//! malformed string all fail identically — telling them apart would turn
//! verification into an oracle for forging one.
//!
//! Expiry is reported rather than swallowed, so an expired approval gets its
//! own refusal with "ask for a fresh quote" instead of a generic rejection.

use base64::Engine;
use ring::hmac;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{OutputFormat, Plan};

/// The fee schedule a token was signed under. A change to the buyer markup
/// changes this, and every outstanding token stops verifying — which is the
/// correct outcome: nobody agreed to the new schedule.
pub fn fee_version() -> String {
    format!(
        "buyer{}:managed{}:federated{}",
        crate::fees::BUYER_MARKUP_BPS,
        crate::fees::MANAGED_PROVIDER_FEE_BPS,
        crate::fees::FEDERATED_PROVIDER_FEE_BPS
    )
}

/// What a plan token carries, before it is signed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanClaim {
    /// Owner. Exactly one is set, mirroring `research_plans`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<Uuid>,
    pub plan_id: Uuid,
    pub version_id: Uuid,
    pub version: i32,
    /// sha256 of the work and the price. See `Plan::hash`.
    pub plan_hash: String,
    /// The ceiling the reservation holds, in atomic USDC.
    pub buyer_total_atomic: i128,
    /// Most outbound requests this job may make, retries included.
    pub max_requests: u32,
    /// Most retries per node.
    pub max_retries_per_node: u32,
    pub format: OutputFormat,
    pub fee_version: String,
    /// Unix seconds.
    pub issued_at: i64,
    pub expires_at: i64,
}

impl PlanClaim {
    /// Build a claim for one stored plan version.
    ///
    /// `max_requests` is derived from the plan rather than configured: one
    /// attempt per node plus the allowed retries. A plan cannot grant itself
    /// more calls than it has nodes for.
    #[allow(clippy::too_many_arguments)]
    pub fn for_version(
        plan: &Plan,
        plan_id: Uuid,
        version_id: Uuid,
        version: i32,
        user_id: Option<Uuid>,
        connection_id: Option<Uuid>,
        max_retries_per_node: u32,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        let nodes = plan.nodes.len() as u32;
        Self {
            user_id,
            connection_id,
            plan_id,
            version_id,
            version,
            plan_hash: plan.hash(),
            buyer_total_atomic: plan.quote.buyer_total_atomic,
            max_requests: nodes.saturating_mul(max_retries_per_node.saturating_add(1)),
            max_retries_per_node,
            format: plan.format,
            fee_version: fee_version(),
            issued_at: chrono::Utc::now().timestamp(),
            expires_at: expires_at.timestamp(),
        }
    }
}

/// Why a token may not be redeemed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    /// Forged, truncated, or signed by a different gateway. One variant on
    /// purpose: three different messages here would be three hints for whoever
    /// is trying to make a fourth.
    #[error("this is not a plan token issued by this gateway")]
    NotOurs,
    #[error("this approval has expired; ask for a fresh quote")]
    Expired,
    #[error("this approval belongs to someone else")]
    WrongOwner,
    #[error("the plan has changed since it was approved")]
    PlanChanged,
    #[error("this approval was given under a different fee schedule")]
    FeesChanged,
}

/// Sign a claim. The caller may hold this; they cannot edit it.
pub fn sign(secret: &str, claim: &PlanClaim) -> Option<String> {
    let payload = serde_json::to_vec(claim).ok()?;
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&payload);
    let tag = hmac::sign(&key(secret), encoded.as_bytes());
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(tag.as_ref());
    Some(format!("{encoded}.{signature}"))
}

/// Read a token back and check everything that could have moved under it.
///
/// `expected_hash` is the hash of the version being started, read from the
/// database at redemption time. Passing the token's own hash would make the
/// check vacuous.
pub fn verify(
    secret: &str,
    token: &str,
    user_id: Option<Uuid>,
    connection_id: Option<Uuid>,
    expected_hash: &str,
) -> Result<PlanClaim, TokenError> {
    let claim = decode(secret, token)?;

    if claim.user_id != user_id || claim.connection_id != connection_id {
        return Err(TokenError::WrongOwner);
    }
    if claim.plan_hash != expected_hash {
        return Err(TokenError::PlanChanged);
    }
    if claim.fee_version != fee_version() {
        return Err(TokenError::FeesChanged);
    }
    // Expiry last, so a token that is both expired and forged is reported as
    // forged. "Expired" would tell the holder their signature was accepted.
    if chrono::Utc::now().timestamp() > claim.expires_at {
        return Err(TokenError::Expired);
    }
    Ok(claim)
}

/// Everything [`verify`] checks except the plan hash.
///
/// Exists for exactly one caller: `POST /v1/jobs` has to know WHICH version a
/// token claims before it can read that version's stored hash to compare
/// against. The hash inside the token cannot be its own reference — comparing
/// it to itself would check nothing — so the lookup has to happen in between.
/// Everything else, including the owner and the expiry, is already enforced
/// here, and the caller must still call [`verify`] afterwards.
pub fn identify(
    secret: &str,
    token: &str,
    user_id: Option<Uuid>,
    connection_id: Option<Uuid>,
) -> Result<PlanClaim, TokenError> {
    let claim = decode(secret, token)?;
    if claim.user_id != user_id || claim.connection_id != connection_id {
        return Err(TokenError::WrongOwner);
    }
    if claim.fee_version != fee_version() {
        return Err(TokenError::FeesChanged);
    }
    if chrono::Utc::now().timestamp() > claim.expires_at {
        return Err(TokenError::Expired);
    }
    Ok(claim)
}

/// Signature check only. Split out so `verify` reads as one list of checks;
/// nothing outside this module gets a claim without the other checks running.
fn decode(secret: &str, token: &str) -> Result<PlanClaim, TokenError> {
    let (encoded, signature) = token.split_once('.').ok_or(TokenError::NotOurs)?;
    let tag = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| TokenError::NotOurs)?;
    hmac::verify(&key(secret), encoded.as_bytes(), &tag).map_err(|_| TokenError::NotOurs)?;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| TokenError::NotOurs)?;
    serde_json::from_slice(&payload).map_err(|_| TokenError::NotOurs)
}

/// The key plan tokens are signed with.
///
/// DOMAIN-SEPARATED from every other thing this gateway signs with the same
/// secret. `v1_routes::quote` signs offer tokens with the raw secret; without
/// the prefix, a valid offer token would be a structurally valid plan token,
/// and the only thing standing between the two would be that their JSON
/// payloads happen to have different fields. That is not a boundary, it is a
/// coincidence.
fn key(secret: &str) -> hmac::Key {
    let mut material = Vec::with_capacity(secret.len() + 16);
    material.extend_from_slice(b"apiosk/plan-token/v1\0");
    material.extend_from_slice(secret.as_bytes());
    hmac::Key::new(hmac::HMAC_SHA256, &material)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::compile::Limits;
    use crate::plan::quote;
    use crate::plan::{Coverage, CoverageStatus, PlanNode};
    use std::collections::BTreeMap;

    const SECRET: &str = "test-plan-signing-secret";

    fn a_plan(price: i128) -> Plan {
        let node = PlanNode {
            endpoint_method: "GET".into(),
            contract: None,
            id: "n_a".to_string(),
            contract_id: "c_a".to_string(),
            endpoint_id: "e_a".to_string(),
            capability_slug: "company_registry.search".to_string(),
            api_slug: "kvk-dutch-business-register".to_string(),
            endpoint_path: "/api/v2/zoeken".to_string(),
            subject_role: "subject".to_string(),
            produces: vec!["company.registration.nl.kvk".to_string()],
            depends_on: vec![],
            bindings: BTreeMap::new(),
            buyer_total_atomic: price,
            may_ask: true,
        };
        Plan {
            quote: quote::total(
                vec![quote::QuoteLine {
                    node_id: node.id.clone(),
                    capability_slug: node.capability_slug.clone(),
                    source_cost_atomic: price,
                    apiosk_fee_atomic: 0,
                    buyer_total_atomic: price,
                }],
                None,
            ),
            nodes: vec![node],
            coverage: vec![Coverage {
                fact_type: "company.registration.nl.kvk".to_string(),
                subject_role: "subject".to_string(),
                required: true,
                node_id: Some("n_a".to_string()),
                status: CoverageStatus::Planned,
                reason: None,
            }],
            format: OutputFormat::Json,
            limits: Limits::default(),
        }
    }

    fn claim_for(plan: &Plan, owner: Uuid, minutes: i64) -> PlanClaim {
        PlanClaim::for_version(
            plan,
            Uuid::from_u128(1),
            Uuid::from_u128(2),
            1,
            Some(owner),
            None,
            2,
            chrono::Utc::now() + chrono::Duration::minutes(minutes),
        )
    }

    #[test]
    fn a_token_this_gateway_signed_verifies() {
        let plan = a_plan(21_738);
        let owner = Uuid::from_u128(9);
        let claim = claim_for(&plan, owner, 30);
        let token = sign(SECRET, &claim).unwrap();

        let back = verify(SECRET, &token, Some(owner), None, &plan.hash()).unwrap();
        assert_eq!(back, claim);
        assert_eq!(back.buyer_total_atomic, 21_738);
    }

    #[test]
    fn the_ceiling_and_the_request_budget_come_from_the_plan() {
        let plan = a_plan(21_738);
        let claim = claim_for(&plan, Uuid::from_u128(9), 30);
        assert_eq!(claim.buyer_total_atomic, plan.quote.buyer_total_atomic);
        // One node, two retries allowed: at most three outbound requests. A
        // plan cannot grant itself more calls than it has nodes for.
        assert_eq!(claim.max_requests, 3);
        assert_eq!(claim.max_retries_per_node, 2);
    }

    #[test]
    fn an_edited_token_is_refused_and_the_edit_does_not_take() {
        let plan = a_plan(21_738);
        let owner = Uuid::from_u128(9);
        let token = sign(SECRET, &claim_for(&plan, owner, 30)).unwrap();

        // Raise the ceiling in the payload and re-encode it, keeping the old
        // signature. This is the attack the signature exists for.
        let (encoded, signature) = token.split_once('.').unwrap();
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .unwrap();
        let mut claim: PlanClaim = serde_json::from_slice(&payload).unwrap();
        claim.buyer_total_atomic = 100_000_000;
        let forged = format!(
            "{}.{signature}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&claim).unwrap())
        );
        assert_eq!(
            verify(SECRET, &forged, Some(owner), None, &plan.hash()),
            Err(TokenError::NotOurs)
        );
    }

    #[test]
    fn a_token_from_another_gateway_is_refused() {
        let plan = a_plan(21_738);
        let owner = Uuid::from_u128(9);
        let token = sign("someone-elses-secret", &claim_for(&plan, owner, 30)).unwrap();
        assert_eq!(
            verify(SECRET, &token, Some(owner), None, &plan.hash()),
            Err(TokenError::NotOurs)
        );
    }

    #[test]
    fn a_malformed_token_and_a_forged_one_answer_identically() {
        let plan = a_plan(1);
        for rubbish in ["", "no-dot", "a.b", "....", "eyJ9.zzzz"] {
            assert_eq!(
                verify(SECRET, rubbish, Some(Uuid::nil()), None, &plan.hash()),
                Err(TokenError::NotOurs),
                "{rubbish} must not be distinguishable from a forgery"
            );
        }
    }

    #[test]
    fn someone_elses_approval_cannot_start_your_job() {
        let plan = a_plan(21_738);
        let mine = Uuid::from_u128(9);
        let yours = Uuid::from_u128(10);
        let token = sign(SECRET, &claim_for(&plan, mine, 30)).unwrap();
        assert_eq!(
            verify(SECRET, &token, Some(yours), None, &plan.hash()),
            Err(TokenError::WrongOwner)
        );
        // And a connection cannot redeem a user's approval either.
        assert_eq!(
            verify(SECRET, &token, None, Some(mine), &plan.hash()),
            Err(TokenError::WrongOwner)
        );
    }

    #[test]
    fn an_expired_approval_is_refused_by_name() {
        let plan = a_plan(21_738);
        let owner = Uuid::from_u128(9);
        let token = sign(SECRET, &claim_for(&plan, owner, -1)).unwrap();
        assert_eq!(
            verify(SECRET, &token, Some(owner), None, &plan.hash()),
            Err(TokenError::Expired)
        );
    }

    #[test]
    fn a_changed_plan_is_refused_even_though_the_token_is_genuine() {
        // The token is real and unexpired. The work it was signed for is not
        // the work about to run, and that is enough.
        let approved = a_plan(21_738);
        let dearer = a_plan(80_000);
        let owner = Uuid::from_u128(9);
        let token = sign(SECRET, &claim_for(&approved, owner, 30)).unwrap();
        assert_eq!(
            verify(SECRET, &token, Some(owner), None, &dearer.hash()),
            Err(TokenError::PlanChanged)
        );
    }

    #[test]
    fn an_approval_given_under_a_different_fee_schedule_is_refused() {
        let plan = a_plan(21_738);
        let owner = Uuid::from_u128(9);
        let mut claim = claim_for(&plan, owner, 30);
        claim.fee_version = "buyer2000:managed1000:federated0".to_string();
        let token = sign(SECRET, &claim).unwrap();
        assert_eq!(
            verify(SECRET, &token, Some(owner), None, &plan.hash()),
            Err(TokenError::FeesChanged)
        );
    }

    #[test]
    fn an_offer_token_is_not_a_plan_token_even_under_the_same_secret() {
        // `v1_routes::quote` signs offer tokens with the raw secret. If plan
        // tokens used it too, an offer token would be a structurally valid
        // plan token and only its JSON fields would stand in the way.
        let raw = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, SECRET.as_bytes());
        let plan = a_plan(1);
        let claim = claim_for(&plan, Uuid::from_u128(9), 30);
        let payload = serde_json::to_vec(&claim).unwrap();
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&payload);
        let tag = ring::hmac::sign(&raw, encoded.as_bytes());
        let signed_with_the_raw_secret = format!(
            "{encoded}.{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(tag.as_ref())
        );
        assert_eq!(
            verify(
                SECRET,
                &signed_with_the_raw_secret,
                Some(Uuid::from_u128(9)),
                None,
                &plan.hash()
            ),
            Err(TokenError::NotOurs)
        );
    }

    #[test]
    fn identify_enforces_everything_except_the_hash() {
        let plan = a_plan(21_738);
        let owner = Uuid::from_u128(9);
        let token = sign(SECRET, &claim_for(&plan, owner, 30)).unwrap();

        // It yields the version id, which is the whole reason it exists.
        let claim = identify(SECRET, &token, Some(owner), None).unwrap();
        assert_eq!(claim.version_id, Uuid::from_u128(2));

        // And it still refuses everything else verify would.
        assert_eq!(
            identify(SECRET, &token, Some(Uuid::from_u128(10)), None),
            Err(TokenError::WrongOwner)
        );
        assert_eq!(
            identify("someone-elses-secret", &token, Some(owner), None),
            Err(TokenError::NotOurs)
        );
        let expired = sign(SECRET, &claim_for(&plan, owner, -1)).unwrap();
        assert_eq!(
            identify(SECRET, &expired, Some(owner), None),
            Err(TokenError::Expired)
        );
    }

    #[test]
    fn the_fee_version_names_the_schedule_in_force() {
        assert_eq!(
            fee_version(),
            format!(
                "buyer{}:managed{}:federated{}",
                crate::fees::BUYER_MARKUP_BPS,
                crate::fees::MANAGED_PROVIDER_FEE_BPS,
                crate::fees::FEDERATED_PROVIDER_FEE_BPS
            )
        );
    }

    #[test]
    fn a_token_carries_no_provider_price_and_no_fee_breakdown() {
        let plan = a_plan(21_738);
        let claim = claim_for(&plan, Uuid::from_u128(9), 30);
        let text = serde_json::to_string(&claim).unwrap();
        for forbidden in ["provider_price", "list_atomic", "markup_atomic", "payout"] {
            assert!(
                !text.contains(forbidden),
                "found {forbidden} in a plan token"
            );
        }
    }

    #[test]
    fn expiry_is_reported_only_after_the_signature_is_trusted() {
        // An expired token that was NOT signed by us must read as "not ours",
        // never as "expired" — the latter would confirm the signature checked
        // out.
        let plan = a_plan(1);
        let owner = Uuid::from_u128(9);
        let token = sign("someone-elses-secret", &claim_for(&plan, owner, -1)).unwrap();
        assert_eq!(
            verify(SECRET, &token, Some(owner), None, &plan.hash()),
            Err(TokenError::NotOurs)
        );
    }
}
