//! Validates saved payloads and records exact field provenance without model calls.
use super::run_contract::{
    EvidenceRecord, FieldOutcomeRecord, PlannerInput, SourcePlan, SourceRowStatus,
};
use crate::catalog::Catalog;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::BTreeMap;
use uuid::Uuid;

/// One saved `results` row of this run's task, mapped to its plan step by the
/// run module (endpoint_id + contract_version + subject.entity_ref).
#[derive(Debug, Clone)]
pub struct SavedResult {
    pub result_ref: Uuid,
    pub execution_id: Uuid,
    pub step_id: String,
    pub document: Value,
    pub retrieved_at: DateTime<Utc>,
}

pub struct EvidenceInput<'a> {
    pub run_id: Uuid,
    pub plan: &'a SourcePlan,
    pub input: &'a PlannerInput,
    /// The saved resolver::Plan steps, same order as plan.sources.
    pub steps: &'a [crate::resolver::Step],
    pub catalog: &'a Catalog,
    pub results: &'a [SavedResult],
    /// Task::failed_steps (step key → public error).
    pub failed_steps: &'a BTreeMap<String, Value>,
    pub now: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEvidence {
    pub accepted: u32,
    pub excluded: u32,
    /// Succeeded, NoMatchingRecords, Partial or Failed, after validation.
    pub status: SourceRowStatus,
}

#[derive(Debug, Clone, Default)]
pub struct EvidenceSet {
    pub records: Vec<EvidenceRecord>,
    pub field_outcomes: Vec<FieldOutcomeRecord>,
    /// By step_id.
    pub per_source: BTreeMap<String, SourceEvidence>,
}

fn pointers(root: &Value, path: &str) -> Vec<(String, Value)> {
    use catalog_core::facts::path::{parse, Segment};
    let Ok(path) = parse(path) else { return vec![] };
    let mut current = vec![(String::new(), root.clone())];
    for segment in path.segments() {
        let mut next = vec![];
        for (pointer, value) in current {
            match segment {
                Segment::Key(key) => {
                    if let Some(v) = value.get(key) {
                        next.push((
                            format!("{}/{}", pointer, key.replace('~', "~0").replace('/', "~1")),
                            v.clone(),
                        ));
                    }
                }
                Segment::Index(index) => {
                    if let Some(v) = value.get(*index) {
                        next.push((format!("{pointer}/{index}"), v.clone()));
                    }
                }
                Segment::Wildcard => {
                    if let Some(items) = value.as_array() {
                        for (i, v) in items.iter().enumerate() {
                            next.push((format!("{pointer}/{i}"), v.clone()));
                        }
                    }
                }
            }
        }
        current = next;
    }
    current
}
fn raw_fields(
    root: &Value,
    extract: &catalog_core::facts::contract::Extract,
    schema: &str,
) -> Vec<(String, Value)> {
    use catalog_core::facts::contract::Extract;
    match extract {
        Extract::Path {
            path,
            cast: None,
            collect: false,
        } => {
            let mut out = pointers(root, path);
            let parts: Vec<_> = schema
                .trim_start_matches('/')
                .split('/')
                .filter(|s| !s.is_empty())
                .collect();
            let mut i = 0;
            while i < parts.len() {
                let mut next = vec![];
                if parts[i] == "properties" && i + 1 < parts.len() {
                    let key = parts[i + 1].replace("~1", "/").replace("~0", "~");
                    for (p, v) in out {
                        if let Some(v) = v.get(&key) {
                            next.push((format!("{p}/{}", parts[i + 1]), v.clone()));
                        }
                    }
                    i += 2;
                } else if parts[i] == "items" {
                    for (p, v) in out {
                        if let Some(a) = v.as_array() {
                            for (j, v) in a.iter().enumerate() {
                                next.push((format!("{p}/{j}"), v.clone()));
                            }
                        }
                    }
                    i += 1;
                } else {
                    return vec![];
                }
                out = next;
            }
            out
        }
        Extract::Fields { fields } => {
            let Some(tail) = schema.strip_prefix("/properties/") else {
                return vec![];
            };
            let (key, rest) = tail
                .split_once('/')
                .map(|(k, r)| (k, format!("/{r}")))
                .unwrap_or((tail, String::new()));
            fields
                .get(&key.replace("~1", "/").replace("~0", "~"))
                .map(|e| raw_fields(root, e, &rest))
                .unwrap_or_default()
        }
        _ => vec![],
    }
}
/// Persisted derived values retain the reviewed transform and original payload.
/// This is only needed for casts, adapters and composed facts without a single
/// raw JSON Pointer; the normalized value is never supplied by a model.
pub fn normalize(document: &mut Value, step: &crate::resolver::Step, catalog: &Catalog) {
    let Ok(extraction) = step
        .service
        .contract
        .extract(&document["data"], &catalog.vocabulary)
    else {
        return;
    };
    if extraction.is_ambiguous() || !extraction.rejected.is_empty() {
        return;
    }
    let rows:Vec<_>=extraction.subjects.iter().map(|subject|{
        let facts:serde_json::Map<String,Value>=subject.facts.iter().map(|f|(f.fact_type.clone(),f.value.clone())).collect();
        serde_json::json!({"facts":facts,"source_row":subject.index,"source_base":step.service.contract.output_map.base,"transforms":step.service.contract.output_map.produces,"contract_id":step.service.id})
    }).collect();
    document["_ask_normalized"] = serde_json::json!(rows);
}
pub fn validate(input: &EvidenceInput<'_>) -> EvidenceSet {
    use super::run_contract::*;
    let mut out = EvidenceSet::default();
    for source in &input.plan.sources {
        let mut accepted = 0;
        let mut excluded = 0;
        let Some(step) = input.steps.iter().find(|s| s.key() == source.step_id) else {
            continue;
        };
        let saved = input.results.iter().find(|r| r.step_id == source.step_id);
        for assignment in &source.assignments {
            let fields = assignment
                .fields
                .required
                .iter()
                .map(|f| (f, true))
                .chain(assignment.fields.optional.iter().map(|f| (f, false)));
            for (field, required) in fields {
                let mut ids = vec![];
                let mut outcome = FieldOutcome::Missing;
                if input.failed_steps.contains_key(&source.step_id) {
                    outcome = FieldOutcome::SourceFailed
                }
                if let Some(result) = saved {
                    let data = &result.document["data"];
                    let extraction = step
                        .service
                        .contract
                        .extract(data, &input.catalog.vocabulary);
                    let valid = extraction
                        .as_ref()
                        .is_ok_and(|e| e.rejected.is_empty() && !e.is_ambiguous());
                    if !valid {
                        outcome = FieldOutcome::Ambiguous
                    } else if step
                        .service
                        .freshness_seconds
                        .is_some_and(|s| (input.now - result.retrieved_at).num_seconds() > s)
                    {
                        outcome = FieldOutcome::Stale
                    } else if let Some((fact, schema)) = field.split_once('#') {
                        if let Some(extract) = step.service.contract.output_map.produces.get(fact) {
                            let mut locations = vec![];
                            for (base, body) in
                                pointers(data, &step.service.contract.output_map.base)
                            {
                                for (pointer, value) in raw_fields(&body, extract, schema) {
                                    locations.push((format!("/data{base}{pointer}"), value));
                                }
                            }
                            if locations.is_empty() {
                                for (index, row) in result.document["_ask_normalized"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .enumerate()
                                {
                                    if let Some(value) = row["facts"].get(fact) {
                                        let root = catalog_core::facts::contract::Extract::Path {
                                            path: "$".into(),
                                            collect: false,
                                            cast: None,
                                        };
                                        for (pointer, value) in raw_fields(value, &root, schema) {
                                            locations.push((
                                                format!(
                                                    "/_ask_normalized/{index}/facts/{}{pointer}",
                                                    fact.replace('~', "~0").replace('/', "~1")
                                                ),
                                                value,
                                            ));
                                        }
                                    }
                                }
                            }
                            for (pointer, value) in locations {
                                if value.is_null() {
                                    continue;
                                }
                                let id = format!("ev-{}", out.records.len() + 1);
                                if result.document.pointer(&pointer) != Some(&value) {
                                    continue;
                                }
                                ids.push(id.clone());
                                accepted += 1;
                                outcome = FieldOutcome::Supported;
                                out.records.push(EvidenceRecord {
                                    evidence_version: 1,
                                    evidence_id: id,
                                    run_id: input.run_id,
                                    plan_revision: input.plan.plan_revision,
                                    request_id: assignment.request_id.clone(),
                                    subject_id: assignment.subject_id.clone(),
                                    field_id: field.clone(),
                                    value,
                                    unit: None,
                                    currency: None,
                                    period: None,
                                    source_id: source.source_id.clone(),
                                    endpoint_id: source.endpoint_id,
                                    contract_id: source.contract_id,
                                    step_id: source.step_id.clone(),
                                    execution_id: result.execution_id,
                                    response_ref: result.result_ref,
                                    response_pointer: pointer,
                                    retrieved_at: result.retrieved_at,
                                    source_record_date: None,
                                    source_url: None,
                                    identity: IdentityStatus::Candidate,
                                    validation_status: ValidationStatus::Accepted,
                                    outcome: FieldOutcome::Supported,
                                    exclusion_reason: None,
                                    reused_from_run: None,
                                });
                            }
                        }
                    }
                }
                if outcome != FieldOutcome::Supported {
                    excluded += 1
                }
                out.field_outcomes.push(FieldOutcomeRecord {
                    request_id: assignment.request_id.clone(),
                    subject_id: assignment.subject_id.clone(),
                    field_id: field.clone(),
                    required,
                    outcome,
                    evidence_ids: ids,
                    reason: if outcome == FieldOutcome::Supported {
                        None
                    } else {
                        Some("source_did_not_establish_field".into())
                    },
                });
            }
        }
        let prerequisite_supported = source.assignments.is_empty()
            && saved.is_some_and(|r| {
                !step
                    .service
                    .freshness_seconds
                    .is_some_and(|s| (input.now - r.retrieved_at).num_seconds() > s)
                    && step
                        .service
                        .contract
                        .extract(&r.document["data"], &input.catalog.vocabulary)
                        .is_ok_and(|e| {
                            e.rejected.is_empty()
                                && !e.is_ambiguous()
                                && e.subjects.iter().any(|s| !s.facts.is_empty())
                        })
            });
        out.per_source.insert(
            source.step_id.clone(),
            SourceEvidence {
                accepted,
                excluded,
                status: if input.failed_steps.contains_key(&source.step_id) {
                    SourceRowStatus::Failed
                } else if prerequisite_supported || accepted > 0 && excluded == 0 {
                    SourceRowStatus::Succeeded
                } else if accepted > 0 {
                    SourceRowStatus::Partial
                } else {
                    SourceRowStatus::NoMatchingRecords
                },
            },
        );
    }
    // Disagreeing values are retained but never silently selected as truth.
    let mut conflicts = std::collections::BTreeSet::new();
    for a in &out.records {
        if out.records.iter().any(|b| {
            a.request_id == b.request_id
                && a.subject_id == b.subject_id
                && a.field_id == b.field_id
                && a.source_id != b.source_id
                && a.value != b.value
        }) {
            conflicts.insert(a.evidence_id.clone());
        }
    }
    for r in &mut out.records {
        if conflicts.contains(&r.evidence_id) {
            r.outcome = FieldOutcome::Conflicting;
        }
    }
    for f in &mut out.field_outcomes {
        if f.evidence_ids.iter().any(|id| conflicts.contains(id)) {
            f.outcome = FieldOutcome::Conflicting;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ask_v2::{planner, run_contract::*};
    use serde_json::json;
    fn evaluate(data: Value, age: i64) -> (EvidenceSet, SourcePlan, PlannerInput) {
        let (c, input, ctx) = planner::tests::fixture();
        let PlanOutcome::Ready { plan, seed, .. } = planner::plan(&input, &c, &ctx) else {
            panic!()
        };
        let step = seed.plan.steps.last().unwrap();
        let result = SavedResult {
            result_ref: Uuid::new_v4(),
            execution_id: Uuid::new_v4(),
            step_id: step.key(),
            document: json!({"data":data}),
            retrieved_at: Utc::now() - chrono::Duration::seconds(age),
        };
        let evidence = validate(&EvidenceInput {
            run_id: ctx.run_id,
            plan: &plan,
            input: &input,
            steps: &seed.plan.steps,
            catalog: &c,
            results: &[result],
            failed_steps: &BTreeMap::new(),
            now: Utc::now(),
        });
        (evidence, *plan, input)
    }
    #[test]
    fn only_schema_valid_present_fresh_values_become_citations() {
        let (good, _, _) = evaluate(json!({"accounts":"2025"}), 0);
        assert_eq!(good.records.len(), 1);
        assert_eq!(good.records[0].response_pointer, "/data/accounts");
        for data in [json!({}), json!({"accounts":42}), json!({"accounts":null})] {
            let (bad, _, _) = evaluate(data, 0);
            assert!(bad.records.is_empty());
            assert_ne!(bad.field_outcomes[0].outcome, FieldOutcome::Supported);
        }
        let (old, _, _) = evaluate(json!({"accounts":"2025"}), 86400);
        assert!(old.records.is_empty());
        assert_eq!(old.field_outcomes[0].outcome, FieldOutcome::Stale);
    }
    #[test]
    fn source_failure_does_not_become_a_negative_fact() {
        let (set, _, _) = evaluate(json!({"error":"Not found"}), 0);
        assert!(set.records.is_empty());
        assert!(!set.field_outcomes.is_empty());
    }
    #[test]
    fn cited_answer_reuses_exact_values_and_rejects_invented_refs() {
        let (set, plan, input) = evaluate(json!({"accounts":"2025"}), 0);
        let context = crate::ask_v2::synthesis::SynthesisInput {
            run_id: plan.run_id,
            plan_revision: 1,
            question: "Accounts",
            input: &input,
            plan: &plan,
            evidence: &set,
            extraction_calls: 1,
        };
        let mut output = crate::ask_v2::synthesis::deterministic(&context);
        assert!(citation_issues(&output, &set.records).is_empty());
        assert_eq!(output.blocks[0].claims[0].values[0].value, "2025");
        output.blocks[0].claims[0].evidence_ids = vec!["ev-invented".into()];
        assert!(!citation_issues(&output, &set.records).is_empty());
    }
    #[test]
    fn normalized_composed_values_retain_transform_and_raw_body() {
        let (c, input, ctx) = planner::tests::fixture();
        let PlanOutcome::Ready { seed, .. } = planner::plan(&input, &c, &ctx) else {
            panic!()
        };
        let step = seed.plan.steps.last().unwrap();
        let mut document = json!({"data":{"accounts":"2025"}});
        normalize(&mut document, step, &c);
        assert_eq!(document["data"]["accounts"], "2025");
        assert_eq!(
            document["_ask_normalized"][0]["facts"]["company.accounts"],
            "2025"
        );
        assert_eq!(
            document["_ask_normalized"][0]["transforms"]["company.accounts"]["path"],
            "$.accounts"
        );
    }
}
