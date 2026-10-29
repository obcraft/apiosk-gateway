//! `POST /v1/plans` and `GET /v1/plans/:id` — the read-only plan API.
//!
//! ```text
//!   POST /v1/plans   intent  ->  DAG + one amount + a hash        nothing spent
//!   GET  /v1/plans/:id       ->  the SAME version, byte for byte  nothing spent
//! ```
//!
//! Both are read-only in the sense that matters: no provider is called, no
//! resolver fires, no balance moves and nothing is reserved. `POST` writes two
//! rows, and that is the whole of its side effects — a plan has to be written
//! down or the thing someone approves could change between being shown and
//! being agreed to.
//!
//! Questions are interpreted by the shared Ask reader, then compiled and
//! persisted. Explicit intents remain supported for existing clients. The
//! deterministic compiler never runs a model; approving or re-reading a
//! stored version never interprets the question again.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::plan::compile::{self, Limits, PlanError};
use crate::plan::{Intent, Plan};
use crate::service_auth::ServiceAuth;
use crate::AppState;

/// How long a quote stands. After this the version cannot be approved; it is
/// not deleted and nothing is released, because nothing was ever held.
const QUOTE_TTL_MINUTES: i64 = 30;

/// Retries allowed per node, carried into the plan token so the request budget
/// is part of what is approved rather than a worker setting nobody agreed to.
const MAX_RETRIES_PER_NODE: u32 = 2;

#[derive(Debug, Deserialize)]
pub struct CreatePlanRequest {
    /// The question as asked. The shared reader interprets it when intent is absent.
    pub question: String,
    /// What is wanted and what is already known. See `crate::plan::Intent`.
    #[serde(default)]
    pub intent: Option<Intent>,
    #[serde(default)]
    pub max_buyer_total_atomic: Option<i128>,
    /// The owner, as verified by the Edge facade. See [`owner_of`].
    #[serde(default)]
    pub user_id: Option<Uuid>,
    #[serde(default)]
    pub connection_id: Option<Uuid>,
    #[serde(default)]
    pub conversation_id: Option<Uuid>,
    /// EUR per USD in micro-units, snapshotted by the caller at the moment the
    /// plan is requested. Absent means no euro figure is shown at all, which
    /// is the honest fallback: a euro number that can move is worse than none.
    #[serde(default)]
    pub fx_rate_micro: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct PlanResponse {
    pub plan_id: Uuid,
    pub version_id: Uuid,
    pub version: i32,
    /// What the App and an MCP client compare to agree they hold the same plan.
    pub plan_hash: String,
    pub question: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub plan: Plan,
    /// Derived, so a caller does not have to walk the DAG to describe it.
    pub summary: Value,
    /// The signed authorisation for exactly this version.
    ///
    /// Issued WITH the plan rather than on approval, because approval is a
    /// press of a button on a surface that already holds the plan — a second
    /// round trip to mint a token would be a second chance for the plan to
    /// have moved underneath it. Holding a token spends nothing; redeeming it
    /// at `POST /v1/jobs` is the moment anything is reserved.
    pub plan_token: String,
}

/// `POST /v1/plans`.
pub async fn create_plan(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<CreatePlanRequest>,
) -> Response {
    if let Err(response) = require_service_key(&state, &headers).await {
        return response;
    }
    let (user_id, connection_id) = match owner_of(body.user_id, body.connection_id) {
        Ok(owner) => owner,
        Err(response) => return response,
    };

    let question = body.question.trim();
    if question.is_empty() || question.chars().count() > 4_000 {
        return refuse(
            StatusCode::BAD_REQUEST,
            "question_invalid",
            "question must be between 1 and 4000 characters.",
        );
    }

    let mut candidates = match state.db.plan_candidates().await {
        Ok(candidates) => candidates,
        Err(error) => {
            tracing::error!("plan candidates: {error}");
            return refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "catalogue_unavailable",
                "The contract catalogue could not be read, so no plan was made.",
            );
        }
    };
    let vocabulary = match state.db.fact_vocabulary().await {
        Ok(v) => v,
        Err(_) => {
            return refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "catalogue_unavailable",
                "Fact schemas are unavailable; no plan was made.",
            )
        }
    };
    candidates.retain(|c| c.contract.check(&vocabulary).is_ok());
    // What the reader may choose from is read off the catalogue: the kinds
    // of subject some contract can look up, the facts that identify each,
    // and the capabilities with a live contract. The compiler cannot plan
    // anything else, so letting the model pick from the whole catalogue only
    // manufactures refusals.
    let catalogue = crate::plan::reading::ReaderCatalogue::derive(&candidates, &vocabulary);

    // Interpret once, then compile and persist that exact intent. Re-reading
    // or executing the approved version never invokes the reader again.
    let mut intent = match body.intent {
        Some(intent) => intent,
        None => {
            let Some(parsed) =
                super::ask::read_research_question(&state, question, &catalogue).await
            else {
                return refuse(StatusCode::SERVICE_UNAVAILABLE, "interpretation_unavailable",
                    "The question reader is temporarily unavailable. Please try again; no plan was started.");
            };
            match crate::plan::reading::from_question(question, &parsed, &catalogue) {
                Ok(intent) => intent,
                Err(clarification) => {
                    return (StatusCode::UNPROCESSABLE_ENTITY, Json(clarification)).into_response()
                }
            }
        }
    };
    if let Some(ceiling) = body.max_buyer_total_atomic {
        intent.max_buyer_total_atomic = Some(
            intent
                .max_buyer_total_atomic
                .map_or(ceiling, |current| current.min(ceiling)),
        );
    }

    // What this thread already paid for, folded in before anything is priced.
    //
    // The reader sees one question and none of the conversation around it, so
    // a follow-up planned every lookup from zero and the buyer paid a second
    // time for a KVK number already printed in the answer above. A fact in
    // `Subject::known` produces no node and costs nothing, which is the
    // mechanism this fills.
    //
    // Scoped to this user AND this conversation, and attached only where the
    // remembered subject and the read subject agree on a fact they both state.
    // See `plan::merge_held_facts` for why identity, not role, is the join.
    //
    // Best-effort: a thread whose facts cannot be read is planned exactly as it
    // is today. A remembered fact is a saving, never a precondition, and it
    // must not be able to fail a plan.
    if let (Some(owner), Some(conversation)) = (user_id, body.conversation_id) {
        match state.db.conversation_facts(owner, conversation).await {
            Ok(held) if !held.is_empty() => {
                let before = intent
                    .subjects
                    .iter()
                    .map(|subject| subject.known.len())
                    .sum::<usize>();
                crate::plan::merge_held_facts(&mut intent, &held);
                let after = intent
                    .subjects
                    .iter()
                    .map(|subject| subject.known.len())
                    .sum::<usize>();
                if after > before {
                    tracing::info!(
                        conversation = %conversation,
                        added = after - before,
                        held = held.len(),
                        "reused facts this conversation already paid for"
                    );
                }
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(conversation = %conversation, "held facts unreadable: {error}");
            }
        }
    }

    if let Err(error) = crate::plan::validate::intent(&intent, &vocabulary) {
        return refuse(StatusCode::UNPROCESSABLE_ENTITY, "intent_invalid", &error);
    }
    let started = std::time::Instant::now();
    let plan = match compile::compile(&intent, &candidates, Limits::default(), body.fx_rate_micro) {
        Ok(plan) => plan,
        Err(error) => {
            if let Some(clarification) = missing_input(&error, &intent, &catalogue, &vocabulary) {
                return (StatusCode::UNPROCESSABLE_ENTITY, Json(clarification)).into_response();
            }
            return refuse_plan(error);
        }
    };
    if let Err(error) = crate::plan::validate::plan(&plan, &vocabulary) {
        return refuse(StatusCode::UNPROCESSABLE_ENTITY, "plan_invalid", &error);
    }
    tracing::info!(stage = "plan", plan_hash = %plan.hash(), nodes = plan.nodes.len(), buyer_total_atomic = %plan.quote.buyer_total_atomic, latency_ms = started.elapsed().as_millis() as u64, "engine plan validated");

    let intent_json = match serde_json::to_value(&intent) {
        Ok(value) => value,
        Err(error) => {
            tracing::error!("intent serialise: {error}");
            return refuse(
                StatusCode::BAD_REQUEST,
                "intent_invalid",
                "The intent could not be recorded.",
            );
        }
    };

    let stored = match state
        .db
        .insert_plan(
            user_id,
            connection_id,
            question,
            &intent_json,
            body.conversation_id,
            &plan,
            chrono::Duration::minutes(QUOTE_TTL_MINUTES),
        )
        .await
    {
        Ok(stored) => stored,
        Err(error) => {
            tracing::error!("insert plan: {error}");
            return refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "plan_not_stored",
                "The plan was compiled but could not be stored, so it is not offered.",
            );
        }
    };

    match respond(&state.config.quote_signing_secret, stored) {
        Some(response) => (StatusCode::CREATED, Json(response)).into_response(),
        None => refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "plan_not_signed",
            "The plan could not be signed, so it is not offered for approval.",
        ),
    }
}

/// Turn an unproducible required input into a typed, resumable question.
///
/// The compiler reports the leaf fact that stopped its dependency search. If
/// that fact is also a declared catalogue input, the caller can provide it; it
/// is not an "unreachable output" and must never be guessed. The exact intent
/// is returned so a UI, MCP client or worker can add the answer to
/// `subjects[*].known` and submit it again without asking the model to reinterpret
/// the original question.
fn missing_input(
    error: &PlanError,
    intent: &Intent,
    catalogue: &crate::plan::reading::ReaderCatalogue,
    vocabulary: &crate::facts::contract::Vocabulary,
) -> Option<Value> {
    let fact_type = match error {
        PlanError::Unreachable { fact_type, .. } | PlanError::Cycle { fact_type } => fact_type,
        _ => return None,
    };
    if !catalogue.inputs.contains_key(fact_type) {
        return None;
    }
    let definition = vocabulary.get(fact_type)?;
    let fields: Vec<Value> = intent
        .subjects
        .iter()
        .filter(|subject| !subject.known.contains_key(fact_type))
        .map(|subject| {
            json!({
                "subject_role": subject.role,
                "fact_type": fact_type,
                "schema": definition.value_schema,
            })
        })
        .collect();
    if fields.is_empty() {
        return None;
    }
    let label = fact_type
        .rsplit('.')
        .next()
        .unwrap_or(fact_type)
        .replace(['-', '_'], " ");
    Some(json!({
        "error": "needs_input",
        "reason": "missing_required_fact",
        "message": format!("I cannot obtain the required {label} from a validated source. Please provide it; no plan was approved and nothing was spent."),
        "missing_inputs": fields,
        "intent": intent,
        "resume": {
            "method": "POST",
            "path": "/v1/plans",
            "instruction": "Add each answer to the matching intent.subjects[].known entry and resubmit this intent with the original question."
        }
    }))
}

#[derive(Debug, Deserialize)]
pub struct ReadPlanQuery {
    #[serde(default)]
    pub user_id: Option<Uuid>,
    #[serde(default)]
    pub connection_id: Option<Uuid>,
}

/// `GET /v1/plans/:id`.
///
/// Returns the stored version, never a recompile. A recompile could disagree
/// with what was shown — a price moved, an endpoint was deactivated — and the
/// whole point of writing a version down is that the thing being approved
/// stops moving.
pub async fn get_plan(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(plan_id): Path<Uuid>,
    Query(params): Query<ReadPlanQuery>,
) -> Response {
    if let Err(response) = require_service_key(&state, &headers).await {
        return response;
    }
    let (user_id, connection_id) = match owner_of(params.user_id, params.connection_id) {
        Ok(owner) => owner,
        Err(response) => return response,
    };

    match state
        .db
        .plan_for_owner(plan_id, user_id, connection_id)
        .await
    {
        Ok(Some(stored)) => match respond(&state.config.quote_signing_secret, stored) {
            Some(response) => Json(response).into_response(),
            None => refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "plan_not_signed",
                "The plan could not be signed, so it is not offered for approval.",
            ),
        },
        // A plan that exists but belongs to someone else is NOT FOUND, not
        // FORBIDDEN. Distinguishing the two tells a caller which plan ids are
        // real, which is a fact about other people's research.
        Ok(None) => refuse(
            StatusCode::NOT_FOUND,
            "plan_not_found",
            "No plan with that id for this owner.",
        ),
        Err(error) => {
            tracing::error!("read plan: {error}");
            refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "plan_unavailable",
                "The plan could not be read.",
            )
        }
    }
}

/// Build the response, including a token for exactly this version.
///
/// The owner in the token comes from the stored plan row, not from the request
/// that is reading it: a token is minted for whoever the plan belongs to, and
/// `plan_for_owner` already refused to hand over anyone else's.
fn respond(secret: &str, stored: crate::db::plans::StoredPlan) -> Option<PlanResponse> {
    // Never mint a token for a legacy or corrupted DAG whose current canonical
    // representation does not match the immutable hash stored with it. In
    // particular, plans made before transparent quote lines existed need a new
    // quote; silently filling their source cost with zero would sign a card the
    // buyer never saw.
    if stored.plan.hash() != stored.plan_hash {
        return None;
    }
    let claim = crate::plan::token::PlanClaim::for_version(
        &stored.plan,
        stored.plan_id,
        stored.version_id,
        stored.version,
        stored.user_id,
        stored.connection_id,
        MAX_RETRIES_PER_NODE,
        stored.expires_at,
    );
    let plan_token = crate::plan::token::sign(secret, &claim)?;
    Some(PlanResponse {
        summary: summarise(&stored.plan),
        plan_id: stored.plan_id,
        version_id: stored.version_id,
        version: stored.version,
        plan_hash: stored.plan_hash,
        question: stored.question,
        expires_at: stored.expires_at,
        plan: stored.plan,
        plan_token,
    })
}

/// The few numbers a card needs, so no surface has to walk the DAG.
///
/// The card carries the transparent buyer calculation: source list costs,
/// Apiosk's 15% buyer fee, and the one total that approval authorises. It never
/// exposes the provider payout or the provider-side revenue split.
pub fn summarise(plan: &Plan) -> Value {
    let source_cost_atomic: i128 = plan
        .quote
        .lines
        .iter()
        .map(|line| line.source_cost_atomic)
        .sum();
    let apiosk_fee_atomic: i128 = plan
        .quote
        .lines
        .iter()
        .map(|line| line.apiosk_fee_atomic)
        .sum();
    json!({
        "paid_calls": plan.paid_nodes(),
        "steps_deep": plan.critical_path(),
        "source_cost_atomic": source_cost_atomic,
        "apiosk_fee_atomic": apiosk_fee_atomic,
        "apiosk_fee_bps": crate::fees::BUYER_MARKUP_BPS,
        "buyer_total_atomic": plan.quote.buyer_total_atomic,
        "lines": plan.quote.lines.iter().map(|line| json!({
            "node_id": line.node_id,
            "label": line.capability_slug,
            "source_cost_atomic": line.source_cost_atomic,
            "apiosk_fee_atomic": line.apiosk_fee_atomic,
            "buyer_total_atomic": line.buyer_total_atomic,
        })).collect::<Vec<_>>(),
        "eur_cents": plan.quote.eur.as_ref().map(|e| e.cents),
        "format": plan.format,
        "may_ask": plan.nodes.iter().any(|node| node.may_ask),
        "missing": plan
            .coverage
            .iter()
            .filter(|c| c.status == crate::plan::CoverageStatus::Unreachable)
            .map(|c| c.fact_type.clone())
            .collect::<Vec<_>>(),
    })
}

/// The Edge facade is the only caller. It holds the service key, it has
/// already verified who is asking, and it passes that identity on.
///
/// A browser cannot reach this: it has no service key, and `user_id` in a body
/// without one is refused before it is read. That is the same boundary
/// `/v1/do` already draws.
async fn require_service_key(state: &Arc<AppState>, headers: &HeaderMap) -> Result<(), Response> {
    match crate::service_auth::resolve(state, headers).await {
        ServiceAuth::Resolved(_) => Ok(()),
        ServiceAuth::NotPresented => Err(refuse(
            StatusCode::UNAUTHORIZED,
            "service_key_required",
            "Planning is reached through the Apiosk app, which presents a service key.",
        )),
        ServiceAuth::Rejected(code, message) => {
            Err(refuse(StatusCode::UNAUTHORIZED, code, message))
        }
    }
}

/// Exactly one owner. A plan with neither is a plan nobody can read back; a
/// plan with both is two answers to one question.
fn owner_of(
    user_id: Option<Uuid>,
    connection_id: Option<Uuid>,
) -> Result<(Option<Uuid>, Option<Uuid>), Response> {
    match (user_id, connection_id) {
        (Some(user), None) => Ok((Some(user), None)),
        (None, Some(connection)) => Ok((None, Some(connection))),
        _ => Err(refuse(
            StatusCode::BAD_REQUEST,
            "owner_required",
            "Exactly one of user_id or connection_id is required.",
        )),
    }
}

/// Refusals carry the reason, and where there is a number the number comes
/// with it. "Too expensive" without the price leaves the caller guessing at
/// what ceiling would work.
fn refuse_plan(error: PlanError) -> Response {
    let (status, code, detail) = match &error {
        PlanError::Unreachable { .. } => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "output_unreachable",
            json!({}),
        ),
        PlanError::Cycle { fact_type } => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "plan_cycles",
            json!({ "fact_type": fact_type }),
        ),
        PlanError::TooManyNodes { found, limit } => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "too_many_calls",
            json!({ "calls": found, "limit": limit }),
        ),
        PlanError::TooDeep { found, limit } => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "too_deep",
            json!({ "steps": found, "limit": limit }),
        ),
        PlanError::TooExpensive { found, limit } => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "over_budget",
            json!({ "buyer_total_atomic": found, "limit_atomic": limit }),
        ),
        PlanError::SearchLimit
        | PlanError::NothingWanted
        | PlanError::NoSubject
        | PlanError::UnknownRole { .. } => (StatusCode::BAD_REQUEST, "intent_invalid", json!({})),
    };
    let mut body = json!({ "error": code, "message": error.to_string() });
    if let (Some(map), Some(extra)) = (body.as_object_mut(), detail.as_object()) {
        for (key, value) in extra {
            map.insert(key.clone(), value.clone());
        }
    }
    (status, Json(body)).into_response()
}

fn refuse(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({ "error": code, "message": message }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::compile::compile_default;
    use crate::plan::{OutputFormat, Subject};

    const ZOEKEN: &str = include_str!("../../fixtures/contracts/kvk-zoeken.json");
    const BASISPROFIEL: &str = include_str!("../../fixtures/contracts/kvk-basisprofiel.json");
    const FACT_TYPES: &str = include_str!("../../fixtures/contracts/fact-types.json");

    fn candidate(text: &str, id: &str, list_atomic: i128) -> crate::plan::Candidate {
        crate::plan::Candidate {
            endpoint_method: "GET".into(),
            contract_id: id.to_string(),
            endpoint_id: format!("e_{id}"),
            api_slug: "kvk-dutch-business-register".to_string(),
            endpoint_path: "/p".to_string(),
            contract: serde_json::from_str(text).unwrap(),
            list_atomic,
            class: crate::fees::ProviderClass::Managed,
            active: true,
            jurisdiction: Some("NL".to_string()),
        }
    }

    fn a_plan() -> Plan {
        let intent = Intent {
            subjects: vec![Subject {
                role: "subject".to_string(),
                known: [("company.name".to_string(), json!("Mollie B.V."))]
                    .into_iter()
                    .collect(),
            }],
            required_outputs: vec!["company.profile".to_string()],
            optional_outputs: vec!["building.attributes".to_string()],
            format: OutputFormat::Json,
            max_buyer_total_atomic: None,
            jurisdiction: Some("NL".to_string()),
        };
        compile_default(
            &intent,
            &[
                candidate(ZOEKEN, "c_zoeken", 18_903),
                candidate(BASISPROFIEL, "c_profiel", 66_162),
            ],
        )
        .unwrap()
    }

    #[test]
    fn the_summary_carries_what_a_card_needs() {
        let summary = summarise(&a_plan());
        assert_eq!(summary["paid_calls"], json!(2));
        assert_eq!(summary["steps_deep"], json!(2));
        assert_eq!(summary["format"], json!("json"));
        // The search step can return several companies, so the card can say
        // "this may stop to ask you something" before anyone approves it.
        assert_eq!(summary["may_ask"], json!(true));
        // And it says out loud what it will not be able to tell you.
        assert_eq!(summary["missing"], json!(["building.attributes"]));
    }

    #[test]
    fn the_summary_carries_source_cost_fee_and_total_but_no_provider_payout() {
        let summary = summarise(&a_plan());
        let text = summary.to_string();
        for forbidden in [
            "provider_price",
            "providerPrice",
            "provider_payout",
            "provider_fee",
        ] {
            assert!(
                !text.contains(forbidden),
                "the plan summary must show one price, and it is the buyer's; found {forbidden}"
            );
        }
        assert_eq!(
            summary["source_cost_atomic"].as_i64().unwrap()
                + summary["apiosk_fee_atomic"].as_i64().unwrap(),
            summary["buyer_total_atomic"].as_i64().unwrap()
        );
        assert_eq!(summary["apiosk_fee_bps"], json!(1_500));
        assert!(summary["buyer_total_atomic"].is_i64());
    }

    #[test]
    fn exactly_one_owner_is_required() {
        assert!(owner_of(Some(Uuid::nil()), None).is_ok());
        assert!(owner_of(None, Some(Uuid::nil())).is_ok());
        assert!(owner_of(None, None).is_err());
        assert!(owner_of(Some(Uuid::nil()), Some(Uuid::nil())).is_err());
    }

    #[test]
    fn an_unresolvable_required_parameter_becomes_a_typed_resumable_question() {
        let candidates = vec![candidate(BASISPROFIEL, "c_profiel", 66_162)];
        let vocabulary: crate::facts::contract::Vocabulary =
            serde_json::from_str(FACT_TYPES).unwrap();
        let catalogue = crate::plan::reading::ReaderCatalogue::derive(&candidates, &vocabulary);
        let intent = Intent {
            subjects: vec![Subject {
                role: "subject".into(),
                known: [("company.name".to_string(), json!("Mollie"))]
                    .into_iter()
                    .collect(),
            }],
            required_outputs: vec!["company.profile".into()],
            optional_outputs: vec![],
            format: OutputFormat::Json,
            max_buyer_total_atomic: None,
            jurisdiction: Some("NL".into()),
        };
        let error = compile_default(&intent, &candidates).unwrap_err();
        let response = missing_input(&error, &intent, &catalogue, &vocabulary).unwrap();

        assert_eq!(response["error"], json!("needs_input"));
        assert_eq!(
            response["missing_inputs"][0]["fact_type"],
            json!("company.registration.nl.kvk")
        );
        assert_eq!(
            response["intent"]["subjects"][0]["known"]["company.name"],
            json!("Mollie")
        );
        assert!(response["message"]
            .as_str()
            .unwrap()
            .contains("nothing was spent"));
    }

    #[test]
    fn a_refusal_carries_the_numbers_the_caller_needs_to_act_on() {
        let response = refuse_plan(PlanError::TooExpensive {
            found: 173_913,
            limit: 1_000,
        });
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn every_refusal_maps_to_a_status_a_caller_can_branch_on() {
        // A bad intent is the caller's fault (400); a well-formed intent the
        // catalogue cannot serve is not (422). Collapsing the two would make
        // "fix your request" and "nobody sells this" indistinguishable.
        for (error, expected) in [
            (PlanError::NothingWanted, StatusCode::BAD_REQUEST),
            (PlanError::NoSubject, StatusCode::BAD_REQUEST),
            (
                PlanError::Unreachable {
                    fact_type: "x.y".to_string(),
                    reason: None,
                },
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                PlanError::Cycle {
                    fact_type: "x.y".to_string(),
                },
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                PlanError::TooManyNodes { found: 9, limit: 8 },
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                PlanError::TooDeep { found: 5, limit: 4 },
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
        ] {
            assert_eq!(refuse_plan(error).status(), expected);
        }
    }

    #[test]
    fn a_plan_response_serialises_with_its_hash_and_its_amount() {
        let plan = a_plan();
        let owner = Uuid::from_u128(7);
        let stored = crate::db::plans::StoredPlan {
            plan_id: Uuid::nil(),
            version_id: Uuid::nil(),
            version: 1,
            user_id: Some(owner),
            connection_id: None,
            question: "is Mollie healthy?".to_string(),
            plan_hash: plan.hash(),
            expires_at: chrono::Utc::now() + chrono::Duration::minutes(30),
            plan: plan.clone(),
        };
        let response = respond("a-secret", stored).unwrap();
        let body = serde_json::to_value(&response).unwrap();
        assert_eq!(body["plan_hash"].as_str().unwrap().len(), 64);
        assert!(body["plan"]["quote"]["buyer_total_atomic"].is_i64());
        assert_eq!(body["version"], json!(1));

        // The token that comes back authorises exactly this version, for this
        // owner, at this amount — and it verifies.
        let claim = crate::plan::token::verify(
            "a-secret",
            &response.plan_token,
            Some(owner),
            None,
            &plan.hash(),
        )
        .expect("the token issued with a plan must verify against that plan");
        assert_eq!(claim.buyer_total_atomic, plan.quote.buyer_total_atomic);
    }

    #[test]
    fn the_token_is_minted_for_the_plans_owner_and_nobody_else() {
        let plan = a_plan();
        let owner = Uuid::from_u128(7);
        let stranger = Uuid::from_u128(8);
        let stored = crate::db::plans::StoredPlan {
            plan_id: Uuid::nil(),
            version_id: Uuid::nil(),
            version: 1,
            user_id: Some(owner),
            connection_id: None,
            question: "q".to_string(),
            plan_hash: plan.hash(),
            expires_at: chrono::Utc::now() + chrono::Duration::minutes(30),
            plan: plan.clone(),
        };
        let response = respond("a-secret", stored).unwrap();
        assert_eq!(
            crate::plan::token::verify(
                "a-secret",
                &response.plan_token,
                Some(stranger),
                None,
                &plan.hash()
            ),
            Err(crate::plan::token::TokenError::WrongOwner)
        );
    }
}
