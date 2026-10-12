//! Deterministic planning from grounded intent. Uses the existing bounded
//! dependency resolver and prices the complete selected set without model calls.
use super::run_contract::*;
use crate::{
    catalog::{Catalog, Service},
    parser::{Goal, Intent, Subject},
    resolver::{self, Resolution},
    state::{digest, Task},
};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}")]
pub struct AdaptError {
    pub code: &'static str,
}
#[derive(Debug, Clone, Default)]
pub struct SignalSnapshot {}
pub struct PlanContext {
    pub run_id: Uuid,
    pub plan_revision: u32,
    pub policy: ScorePolicy,
    pub shadow_policies: Vec<ScorePolicy>,
    pub signals: SignalSnapshot,
    pub now: DateTime<Utc>,
}
pub fn adapt(e: &GroundedIntentEnvelope) -> Result<PlannerInput, AdaptError> {
    let fail = || AdaptError {
        code: "invalid_grounded_intent",
    };
    let i = &e.intent;
    if e.schema_version != 2
        || i.field_catalog_version.is_empty()
        || i.requests.len() > 16
        || i.subjects.len() > 16
    {
        return Err(fail());
    }
    let subjects: BTreeSet<_> = i.subjects.iter().map(|s| &s.id).collect();
    let requests: BTreeSet<_> = i.requests.iter().map(|r| &r.id).collect();
    if subjects.len() != i.subjects.len() || requests.len() != i.requests.len() {
        return Err(fail());
    }
    for r in &i.requests {
        if !i.capabilities.iter().any(|c| c.slug == r.capability_slug)
            || r.subject_ids.iter().any(|id| !subjects.contains(id))
        {
            return Err(fail());
        }
        for field in r.required_fields.iter().chain(&r.optional_fields) {
            if !i
                .field_definitions
                .iter()
                .any(|f| f.id == *field && f.id == format!("{}#{}", f.fact_type, f.schema_pointer))
            {
                return Err(fail());
            }
        }
    }
    Ok(PlannerInput {
        planner_input_version: 1,
        intent_schema_version: 2,
        field_catalog_version: i.field_catalog_version.clone(),
        question_revision: 1,
        language: i.language.clone(),
        subjects: i.subjects.clone(),
        capabilities: i.capabilities.clone(),
        requests: i.requests.clone(),
        field_definitions: i.field_definitions.clone(),
        output: i.output.clone(),
        clarifications: vec![],
        fact_schemas: BTreeMap::new(),
    })
}
pub fn apply_clarification(
    input: &PlannerInput,
    asked: &ClarificationRequest,
    answer: &ClarificationAnswer,
    catalog: &Catalog,
) -> Result<PlannerInput, AdaptError> {
    let invalid = || AdaptError {
        code: "invalid_clarification",
    };
    if answer.clarification_id != asked.clarification_id
        || answer.question_revision != input.question_revision
        || answer.option_id.is_some()
    {
        return Err(invalid());
    }
    let field = asked.value_input.as_ref().ok_or_else(invalid)?;
    let text = answer
        .value
        .as_ref()
        .filter(|v| !v.trim().is_empty() && v.len() <= field.max_length as usize)
        .ok_or_else(invalid)?;
    let schema = &catalog
        .vocabulary
        .get(&field.fact_type)
        .ok_or_else(invalid)?
        .value_schema;
    let value = if schema["type"] == "string" {
        json!(text)
    } else {
        serde_json::from_str(text).map_err(|_| invalid())?
    };
    catalog_core::facts::schema::validate(schema, &value).map_err(|_| invalid())?;
    let mut next = input.clone();
    let subject = next
        .subjects
        .iter_mut()
        .find(|s| Some(&s.id) == asked.subject_id.as_ref())
        .ok_or_else(invalid)?;
    subject
        .identifiers
        .insert(field.fact_type.clone(), text.clone());
    next.question_revision += 1;
    next.clarifications.push(answer.clone());
    Ok(next)
}
pub fn active_policy() -> ScorePolicy {
    ScorePolicy {
        score_version: "documented-contract-fit-v1".into(),
        calibration_status: CalibrationStatus::UncalibratedHeuristic,
        mode: ScoreMode::Enforced,
        threshold: ActiveThreshold::default(),
    }
}
pub async fn load_signals(_: &sqlx::PgPool, _: &Catalog) -> anyhow::Result<SignalSnapshot> {
    Ok(SignalSnapshot {})
}
fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
fn source_matches(s: &Service, r: &GroundedRequest, c: &Catalog) -> bool {
    r.sources.is_empty()
        || r.sources.iter().any(|name| {
            let n = norm(name);
            n == norm(&s.api_slug)
                || n == norm(&s.name)
                || c.known_sources
                    .iter()
                    .any(|k| k.slug == s.api_slug && n == norm(&k.name))
        })
}
// 90 is the documented conformance baseline, not estimated reliability.
// Additional points require validated response evidence and a freshness bound.
fn contract_score(s: &Service, c: &Catalog) -> f64 {
    let sample = s.sample_payload.as_ref().is_some_and(|p| {
        s.contract
            .extract(p, &c.vocabulary)
            .is_ok_and(|e| e.rejected.is_empty() && !e.subjects.is_empty())
    });
    90.0 + if sample { 7.0 } else { 0.0 }
        + if s.freshness_seconds.is_some_and(|n| n > 0) {
            3.0
        } else {
            0.0
        }
}
pub(crate) fn covers(s: &Service, id: &str, c: &Catalog) -> bool {
    let Some((fact, pointer)) = id.split_once('#') else {
        return false;
    };
    s.contract.output_map.produces.contains_key(fact)
        && !s.optional_outputs.iter().any(|f| f == fact)
        && c.vocabulary
            .get(fact)
            .and_then(|f| f.value_schema.pointer(pointer))
            .is_some_and(super::intent_fields::documented)
}
fn subset(c: &Catalog, keep: impl Fn(&Service) -> bool) -> Catalog {
    let mut c = c.clone();
    c.services.retain(keep);
    c.by_capability.clear();
    c.by_output.clear();
    for (i, s) in c.services.iter().enumerate() {
        c.by_capability
            .entry(s.contract.capability_slug.clone())
            .or_default()
            .push(i);
        for f in s.contract.output_map.produces.keys() {
            c.by_output.entry(f.clone()).or_default().push(i);
        }
    }
    c
}
fn values(s: &IntentSubject, c: &Catalog) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    for (key, value) in &s.identifiers {
        let candidates: Vec<_> = c
            .vocabulary
            .keys()
            .filter(|k| *k == key || norm(k.rsplit('.').next().unwrap_or(k)) == norm(key))
            .collect();
        if candidates.len() == 1 {
            let k = candidates[0];
            let schema = &c.vocabulary[k].value_schema;
            let v = if schema["type"] == "string" {
                json!(value)
            } else {
                serde_json::from_str(value).unwrap_or(Value::Null)
            };
            if catalog_core::facts::schema::validate(schema, &v).is_ok() {
                out.insert(k.clone(), v);
            }
        }
    }
    let name = format!("{}.name", s.kind);
    if let Some(f) = c.vocabulary.get(&name) {
        if catalog_core::facts::schema::validate(&f.value_schema, &json!(s.label)).is_ok() {
            out.entry(name).or_insert(json!(s.label));
        }
    }
    out
}
fn gap(
    r: &GroundedRequest,
    subject: Option<String>,
    field: Option<String>,
    required: bool,
    reason: GapReason,
) -> RequirementGap {
    RequirementGap {
        request_id: r.id.clone(),
        subject_id: subject,
        field,
        required,
        reason,
    }
}
pub fn plan(input: &PlannerInput, catalog: &Catalog, ctx: &PlanContext) -> PlanOutcome {
    let mut coverage = Coverage::default();
    let mut scores = vec![];
    let mut trace = PlannerTrace::default();
    let start = std::time::Instant::now();
    let c = subset(catalog, |s| {
        !s.unverified && s.list_atomic >= 0 && s.contract.max_fan_out <= 100
    });
    let mut task = Task::default();
    let mut subjects = vec![];
    let mut parsed = Intent {
        subjects: input
            .subjects
            .iter()
            .map(|s| Subject {
                entity_ref: None,
                label: s.label.clone(),
                values: values(s, &c),
            })
            .collect(),
        goals: vec![],
        answer_operations: vec![],
        unresolved_references: vec![],
    };
    if input.field_catalog_version != catalog.version
        || crate::engine::bind_subjects(&mut task, &c, &mut parsed).is_err()
    {
        return PlanOutcome::NoQualifyingSources {
            gaps: input
                .requests
                .iter()
                .map(|r| gap(r, None, None, true, GapReason::NoEligibleSource))
                .collect(),
            scores,
            trace,
        };
    }
    for (s, p) in input.subjects.iter().zip(&parsed.subjects) {
        subjects.push(SubjectBinding {
            subject_id: s.id.clone(),
            entity_ref: p.entity_ref.unwrap(),
            label: s.label.clone(),
        });
    }
    let mut steps: Vec<resolver::Step> = vec![];
    let mut assignments: BTreeMap<String, Vec<Assignment>> = BTreeMap::new();
    let mut trials = 0;
    let mut clarification = None;
    for r in &input.requests {
        for f in &r.unresolved_fields.required {
            coverage.required_gaps.push(gap(
                r,
                None,
                Some(f.field.clone()),
                true,
                if f.reason == "ambiguous" {
                    GapReason::Ambiguous
                } else {
                    GapReason::Undocumented
                },
            ));
        }
        for f in &r.unresolved_fields.optional {
            coverage.optional_gaps.push(gap(
                r,
                None,
                Some(f.field.clone()),
                false,
                GapReason::Undocumented,
            ));
        }
        if !input.capabilities.iter().any(|cap| {
            cap.slug == r.capability_slug && cap.status == "active" && cap.origin == "database"
        }) || !matches!(r.kind, RequestKind::Lookup | RequestKind::Search)
            || r.subject_ids.is_empty()
        {
            coverage
                .required_gaps
                .push(gap(r, None, None, true, GapReason::UnsupportedKind));
            continue;
        }
        for subject_id in &r.subject_ids {
            let si = input
                .subjects
                .iter()
                .position(|s| s.id == *subject_id)
                .unwrap();
            let entity = parsed.subjects[si].entity_ref.unwrap();
            if r.required_fields.is_empty() && !r.unresolved_fields.required.is_empty() {
                continue;
            }
            if r.capability_slug.starts_with("company.") && input.subjects[si].kind != "company" {
                coverage.required_gaps.push(gap(
                    r,
                    Some(subject_id.clone()),
                    None,
                    true,
                    GapReason::NoEligibleSource,
                ));
                continue;
            }
            let mut remaining = r.required_fields.clone();
            if remaining.is_empty() && r.unresolved_fields.required.is_empty() {
                let mut produced: BTreeSet<String> = BTreeSet::new();
                for s in c.services.iter().filter(|s| {
                    s.contract.capability_slug == r.capability_slug && source_matches(s, r, &c)
                }) {
                    for f in s.contract.output_map.produces.keys() {
                        let id = format!("{f}#");
                        if covers(s, &id, &c) {
                            produced.insert(id);
                        }
                    }
                }
                remaining = produced.into_iter().collect();
            }
            let mut selected_any = false;
            // A request without explicit fields still needs one exact executable capability.
            let mut first = true;
            while !remaining.is_empty() || first {
                first = false;
                let mut best: Option<(resolver::Plan, Vec<String>, &Service)> = None;
                for service in c
                    .services
                    .iter()
                    .filter(|s| s.contract.capability_slug == r.capability_slug)
                {
                    trials += 1;
                    if trials > 256 {
                        break;
                    }
                    let fields: Vec<_> = remaining
                        .iter()
                        .filter(|f| covers(service, f, &c))
                        .cloned()
                        .collect();
                    let fields_ok = remaining.is_empty() || !fields.is_empty();
                    let source_ok = source_matches(service, r, &c);
                    let mut constraints: BTreeMap<String, Value> = r
                        .filters
                        .iter()
                        .map(|(k, v)| (k.clone(), json!(v)))
                        .collect();
                    constraints.insert("endpoint_id".into(), json!(service.endpoint_id));
                    constraints.insert("source".into(), json!(service.api_slug));
                    let country = input.subjects[si]
                        .identifiers
                        .get("country")
                        .or_else(|| input.subjects[si].identifiers.get("jurisdiction"));
                    if let Some(country) = country {
                        constraints
                            .entry("jurisdiction".into())
                            .or_insert(json!(country));
                    }
                    let subject_fit = resolver::allows(service, &constraints, &c);
                    let candidate_intent = Intent {
                        subjects: parsed.subjects.clone(),
                        goals: vec![Goal {
                            capability: r.capability_slug.clone(),
                            subject: si,
                            constraints,
                        }],
                        answer_operations: vec![],
                        unresolved_references: vec![],
                    };
                    let safe = subset(&c, |s| {
                        // All dependencies inherit a known country restriction.
                        country.is_none_or(|country| {
                            s.jurisdiction
                                .as_ref()
                                .is_none_or(|j| j.eq_ignore_ascii_case(country))
                        })
                    });
                    let resolution = if source_ok && fields_ok {
                        Some(resolver::resolve(&safe, &task, &candidate_intent))
                    } else {
                        None
                    };
                    let candidate = match resolution {
                        Some(Resolution::Missing { kind, .. }) if clarification.is_none() => {
                            clarification = Some(ClarificationRequest {
                                clarification_id: format!("input:{}:{}:{}", r.id, subject_id, kind),
                                question_revision: input.question_revision,
                                reason: "missing_input".into(),
                                message_key: "run.clarification.input".into(),
                                message_args: BTreeMap::new(),
                                request_id: Some(r.id.clone()),
                                subject_id: Some(subject_id.clone()),
                                options: vec![],
                                value_input: Some(ValueInput {
                                    fact_type: kind.clone(),
                                    label: format!("Enter {kind} for {}", input.subjects[si].label),
                                    max_length: 500,
                                }),
                            });
                            None
                        }
                        Some(Resolution::Plan(p))
                            if p.steps.iter().all(|s| {
                                s.browser_fallback.is_none()
                                    && qualifies(
                                        Some(contract_score(&s.service, &c)),
                                        ctx.policy.threshold,
                                    )
                            }) =>
                        {
                            Some(p)
                        }
                        _ => None,
                    };
                    let gates = vec![
                        GateResult {
                            gate: Gate::SubjectFit,
                            passed: subject_fit,
                            reason: "contract_filters_and_jurisdiction".into(),
                        },
                        GateResult {
                            gate: Gate::Admission,
                            passed: !service.unverified,
                            reason: "admitted_verified_contract".into(),
                        },
                        GateResult {
                            gate: Gate::Price,
                            passed: service.list_atomic >= 0,
                            reason: "catalog_price".into(),
                        },
                        GateResult {
                            gate: Gate::Bounds,
                            passed: service.contract.max_fan_out <= 100,
                            reason: "bounded_single_call".into(),
                        },
                        GateResult {
                            gate: Gate::Freshness,
                            passed: subject_fit,
                            reason: "requested_constraints_preserved".into(),
                        },
                        GateResult {
                            gate: Gate::Capability,
                            passed: true,
                            reason: "exact_capability".into(),
                        },
                        GateResult {
                            gate: Gate::FieldCoverage,
                            passed: fields_ok,
                            reason: "documented_fields".into(),
                        },
                        GateResult {
                            gate: Gate::SourceRestriction,
                            passed: source_ok,
                            reason: "explicit_source".into(),
                        },
                        GateResult {
                            gate: Gate::Inputs,
                            passed: candidate.is_some(),
                            reason: "bounded_resolvable_contract".into(),
                        },
                    ];
                    // Binary contract conformance, explicitly uncalibrated. No guessed reliability credit.
                    let fit = Some(FitScore::new(contract_score(service, &c)).unwrap());
                    let decision = ctx
                        .policy
                        .threshold
                        .decide(fit, gates.iter().all(|g| g.passed));
                    scores.push(ScoreRecord {
                        score_record_version: 1,
                        candidate_id: format!("{}:{}:{}", r.id, subject_id, service.id),
                        request_id: r.id.clone(),
                        subject_ids: vec![subject_id.clone()],
                        contract_id: service.id,
                        endpoint_id: service.endpoint_id,
                        source_id: service.api_slug.clone(),
                        fit_score: fit,
                        score_version: ctx.policy.score_version.clone(),
                        calibration_status: ctx.policy.calibration_status,
                        mode: ctx.policy.mode,
                        threshold: ctx.policy.threshold,
                        decision,
                        signals: vec![
                            ScoreSignal {
                                name: "freshness_bound_seconds".into(),
                                value: service.freshness_seconds.map(|s| s as f64),
                                evidence_refs: vec![service.id.to_string()],
                                evidence_age_seconds: None,
                                sample_size: None,
                            },
                            ScoreSignal {
                                name: "measured_reliability".into(),
                                value: None,
                                evidence_refs: vec![],
                                evidence_age_seconds: None,
                                sample_size: None,
                            },
                            ScoreSignal {
                                name: "documented_contract_conformance".into(),
                                value: Some(if decision == ThresholdDecision::Qualifies {
                                    1.0
                                } else {
                                    0.0
                                }),
                                evidence_refs: vec![service.id.to_string()],
                                evidence_age_seconds: None,
                                sample_size: None,
                            },
                        ],
                        gates,
                        assigned: FieldScope {
                            required: fields.clone(),
                            optional: vec![],
                        },
                        gaps: FieldScope {
                            required: remaining
                                .iter()
                                .filter(|f| !fields.contains(f))
                                .cloned()
                                .collect(),
                            optional: vec![],
                        },
                    });
                    if decision != ThresholdDecision::Qualifies {
                        continue;
                    }
                    let Some(p) = candidate else { continue };
                    let cost = |p: &resolver::Plan| {
                        p.steps
                            .iter()
                            .filter(|s| !steps.iter().any(|old| old.key() == s.key()))
                            .map(|s| s.service.total())
                            .sum::<i64>()
                    };
                    if best.as_ref().is_none_or(|(old, covered, svc)| {
                        let ratio = (cost(&p) as i128 * covered.len().max(1) as i128)
                            .cmp(&(cost(old) as i128 * fields.len().max(1) as i128));
                        ratio.is_lt()
                            || ratio.is_eq()
                                && (
                                    cost(&p),
                                    p.steps.len(),
                                    std::cmp::Reverse((contract_score(service, &c) * 100.0) as u32),
                                    service.id,
                                ) < (
                                    cost(old),
                                    old.steps.len(),
                                    std::cmp::Reverse((contract_score(svc, &c) * 100.0) as u32),
                                    svc.id,
                                )
                    }) {
                        best = Some((p, fields, service));
                    }
                }
                let Some((p, fields, service)) = best else {
                    break;
                };
                selected_any = true;
                let key = format!("{}:{}", service.id, entity);
                let optional = r
                    .optional_fields
                    .iter()
                    .filter(|f| covers(service, f, &c))
                    .cloned()
                    .collect::<Vec<_>>();
                let a = Assignment {
                    request_id: r.id.clone(),
                    subject_id: subject_id.clone(),
                    fields: FieldScope {
                        required: fields.clone(),
                        optional,
                    },
                };
                assignments.entry(key).or_default().push(a.clone());
                coverage.covered_required.push(a);
                for step in p.steps {
                    if !steps.iter().any(|s| s.key() == step.key()) {
                        steps.push(step)
                    }
                }
                remaining.retain(|f| !fields.contains(f));
                if remaining.is_empty() {
                    break;
                }
            }
            for f in remaining {
                coverage.required_gaps.push(gap(
                    r,
                    Some(subject_id.clone()),
                    Some(f),
                    true,
                    GapReason::NoEligibleSource,
                ));
            }
            if !selected_any {
                coverage.required_gaps.push(gap(
                    r,
                    Some(subject_id.clone()),
                    None,
                    true,
                    GapReason::MissingInput,
                ));
            }
            for f in &r.optional_fields {
                if !assignments.values().flatten().any(|a| {
                    a.request_id == r.id
                        && a.subject_id == *subject_id
                        && a.fields.optional.contains(f)
                }) {
                    coverage.optional_gaps.push(gap(
                        r,
                        Some(subject_id.clone()),
                        Some(f.clone()),
                        false,
                        GapReason::NoEligibleSource,
                    ));
                }
            }
        }
    }
    let counts = Counts {
        candidates_checked: Some(scores.len() as u32),
        qualifying_candidates: Some(
            scores
                .iter()
                .filter(|s| s.decision == ThresholdDecision::Qualifies)
                .count() as u32,
        ),
        calls_total: Some(steps.len() as u32),
        selected_sources: Some(
            steps
                .iter()
                .map(|s| &s.service.api_slug)
                .collect::<BTreeSet<_>>()
                .len() as u32,
        ),
        ..Counts::default()
    };
    trace.stages.push(StageTrace {
        stage: Stage::BuildingPlan,
        elapsed_ms: start.elapsed().as_millis() as u64,
        counts: counts.clone(),
    });
    if steps.is_empty() || steps.len() > crate::protocol::MAX_STEPS {
        if let Some(clarification) = clarification {
            return PlanOutcome::NeedsClarification {
                clarification,
                scores,
                trace,
            };
        }
        return PlanOutcome::NoQualifyingSources {
            gaps: coverage.required_gaps,
            scores,
            trace,
        };
    }
    let cap = steps
        .iter()
        .try_fold(0i64, |n, s| n.checked_add(s.service.total()));
    let Some(cap) = cap else {
        return PlanOutcome::NoQualifyingSources {
            gaps: coverage.required_gaps,
            scores,
            trace,
        };
    };
    let plan = resolver::Plan {
        compute: None,
        quote_ref: Uuid::new_v4(),
        hash: digest(&steps),
        steps,
        cap_atomic: cap,
        expires_at: ctx.now + chrono::Duration::minutes(10),
        fee_bps: resolver::standard_markup(),
        workflow_slug: None,
    };
    let mut sources: Vec<PlannedSource> = vec![];
    for (index, step) in plan.steps.iter().enumerate() {
        let depends_on: Vec<_> = plan.steps[..index]
            .iter()
            .filter(|s| s.entity_ref == step.entity_ref)
            .flat_map(|s| {
                step.service
                    .contract
                    .requires
                    .iter()
                    .filter(|r| {
                        !r.optional
                            && s.service
                                .contract
                                .output_map
                                .produces
                                .contains_key(&r.fact_type)
                    })
                    .map(|r| DependencyBinding {
                        step_id: s.key(),
                        fact_type: r.fact_type.clone(),
                    })
            })
            .collect();
        let group = depends_on
            .iter()
            .filter_map(|d| {
                sources
                    .iter()
                    .find(|s| s.step_id == d.step_id)
                    .map(|s| s.execution_group + 1)
            })
            .max()
            .unwrap_or(0);
        let assigned = assignments.remove(&step.key()).unwrap_or_default();
        sources.push(PlannedSource {
            step_id: step.key(),
            step_index: index as u32,
            source_id: step.service.api_slug.clone(),
            source_name: step.service.name.clone(),
            endpoint_id: step.service.endpoint_id,
            contract_id: step.service.id,
            capability_slug: step.service.contract.capability_slug.clone(),
            role: if assigned.is_empty() {
                SourceRole::Dependency
            } else {
                SourceRole::Primary
            },
            fit_score: FitScore::new(contract_score(&step.service, &c)).unwrap(),
            selection_reasons: vec!["documented_contract_fit".into()],
            assignments: assigned,
            depends_on,
            execution_group: group,
            max_calls: 1,
            cost: CostBreakdown {
                provider_atomic: Amount(step.service.list_atomic),
                fee_atomic: Amount(step.service.total() - step.service.list_atomic),
                total_atomic: Amount(step.service.total()),
            },
        });
    }
    let provider = sources.iter().map(|s| s.cost.provider_atomic.0).sum();
    let public = SourcePlan {
        source_plan_version: 1,
        run_id: ctx.run_id,
        plan_id: Uuid::new_v4(),
        plan_revision: ctx.plan_revision,
        question_revision: input.question_revision,
        intent_schema_version: 2,
        catalog_version: catalog.version.clone(),
        field_catalog_version: input.field_catalog_version.clone(),
        score_policy: ctx.policy.clone(),
        status: if coverage.required_gaps.is_empty() {
            PlanStatus::Complete
        } else {
            PlanStatus::Partial
        },
        subjects,
        sources,
        coverage,
        quote: PlanQuote {
            quote_ref: plan.quote_ref,
            plan_hash: plan.hash.clone(),
            currency: "USD".into(),
            fee_bps: plan.fee_bps,
            pricing_rule: "catalog_provider_plus_buyer_fee".into(),
            provider_atomic: Amount(provider),
            fee_atomic: Amount(cap - provider),
            total_atomic: Amount(cap),
            variable: true,
            expires_at: plan.expires_at,
        },
        approval: Approval {
            state: ApprovalState::Awaiting,
            approved_at: None,
            authorization_ref: None,
        },
        candidates: counts,
        search: SearchReport {
            combinations_evaluated: trials,
            bound_reached: trials > 256,
        },
        expected_latency_ms: None,
        latency_basis: None,
    };
    PlanOutcome::Ready {
        plan: Box::new(public),
        seed: Box::new(ExecutionSeed {
            plan,
            entities: task.entities,
            facts: task.facts,
        }),
        scores,
        trace,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) fn fixture() -> (Catalog, PlannerInput, PlanContext) {
        let c = Catalog::build(
            serde_json::from_str(include_str!(
                "../../tests/fixtures/integration-catalog.json"
            ))
            .unwrap(),
        );
        let input:PlannerInput=serde_json::from_value(json!({"planner_input_version":1,"intent_schema_version":2,"field_catalog_version":c.version,"question_revision":1,"language":"en","subjects":[{"id":"s1","type":"company","label":"Example","identifiers":{}}],"capabilities":[{"slug":"company.accounts","name":"Accounts","description":"Accounts","category":"company","status":"active","origin":"database"}],"requests":[{"id":"r1","capability_slug":"company.accounts","subject_ids":["s1"],"kind":"lookup","goal":"Accounts","required_fields":["company.accounts#"],"optional_fields":[],"filters":{},"sources":[],"field_resolution":"resolved","unresolved_fields":{"required":[],"optional":[]}}],"field_definitions":[{"id":"company.accounts#","fact_type":"company.accounts","schema_pointer":""}],"output":{"format":"chat","operations":[]}})).unwrap();
        let ctx = PlanContext {
            run_id: Uuid::new_v4(),
            plan_revision: 1,
            policy: active_policy(),
            shadow_policies: vec![],
            signals: SignalSnapshot::default(),
            now: Utc::now(),
        };
        (c, input, ctx)
    }
    #[test]
    fn prices_dependencies_once_and_preserves_unknown_fields() {
        let (c, mut input, ctx) = fixture();
        input.requests[0]
            .unresolved_fields
            .required
            .push(UnresolvedField {
                field: "registration_details".into(),
                reason: "undocumented".into(),
            });
        let PlanOutcome::Ready { plan, seed, .. } = plan(&input, &c, &ctx) else {
            panic!("no plan")
        };
        assert_eq!(seed.plan.steps.len(), 2);
        assert_eq!(plan.status, PlanStatus::Partial);
        assert_eq!(
            plan.coverage.required_gaps[0].field.as_deref(),
            Some("registration_details")
        );
        assert_eq!(plan.sources[1].execution_group, 1);
        assert_eq!(plan.quote.total_atomic.0, seed.plan.cap_atomic);
        check_quote_totals(&plan).unwrap();
    }
    #[test]
    fn rejects_wrong_source_country_and_unknown_filters() {
        for (key, value) in [
            ("source", "wrong"),
            ("jurisdiction", "NL"),
            ("invented", "x"),
        ] {
            let (c, mut input, ctx) = fixture();
            if key == "source" {
                input.requests[0].sources = vec![value.into()]
            } else {
                input.requests[0].filters.insert(key.into(), value.into());
            }
            assert!(
                matches!(
                    plan(&input, &c, &ctx),
                    PlanOutcome::NoQualifyingSources { .. }
                ),
                "{key}"
            );
        }
    }
    #[test]
    fn repeated_requests_reuse_calls_and_bind_subjects_separately() {
        let (c, mut input, ctx) = fixture();
        let mut second = input.requests[0].clone();
        second.id = "r2".into();
        input.requests.push(second);
        let PlanOutcome::Ready { seed, plan, .. } = plan(&input, &c, &ctx) else {
            panic!()
        };
        assert_eq!(seed.plan.steps.len(), 2);
        assert_eq!(plan.sources[1].assignments.len(), 2);
    }
    #[test]
    fn unverified_and_catalog_mismatch_do_not_plan() {
        let (c, mut input, ctx) = fixture();
        input.field_catalog_version = "old".into();
        assert!(matches!(
            plan(&input, &c, &ctx),
            PlanOutcome::NoQualifyingSources { .. }
        ));
    }
}
