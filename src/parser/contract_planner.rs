//! Compile an extracted request against admitted contracts. No model, network,
//! source execution, or mutation of the parsed request belongs in this stage.
use super::{
    capability_binding, explicit_request::request_match, plan_coverage, source_grounding,
    ChatReply, Goal, Intent, Subject,
};
use crate::{capability_intake::Intake, catalog::Catalog, state::Task};
use serde_json::json;
use sha2::Digest;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

const MAX_CHOICES: usize = 24;
const MAX_COMBINATIONS: usize = 256;

/// A compiled intent can be a valid missing-input draft. Only a complete,
/// executable plan may skip semantic alternative selection.
pub(crate) fn ready(question: &str, reply: &ChatReply, task: &Task, catalog: &Catalog, intake: &Intake) -> bool {
    let Some(mut intent) = reply.intent.clone().filter(|_| reply.discover) else { return false; };
    let mut preview = task.clone();
    preview.record_turn(question, None);
    preview.capability_intake = Some(intake.clone());
    preview.output_grounding = source_grounding::Grounding::new(intake, catalog, reply.output_bindings.clone());
    if crate::engine::bind_subjects(&mut preview, catalog, &mut intent).is_err() { return false; }
    let crate::resolver::Resolution::Plan(plan) = crate::resolver::resolve(catalog, &preview, &intent) else { return false; };
    plan_coverage::uncovered_outputs(&preview, catalog, &intent, &plan).is_empty()
        && plan_coverage::uncovered_inputs(&preview, catalog, &intent, &plan).is_empty()
}

#[cfg(test)]
#[path = "contract_planner/tests.rs"]
mod alternatives_tests;

pub(crate) fn compile(
    question: &str,
    literals: &str,
    task: &Task,
    catalog: &Catalog,
    intake: &Intake,
) -> ChatReply {
    let started = std::time::Instant::now();
    let reply = compile_request(question, literals, task, catalog, intake, None)
        .unwrap_or_else(|| missing_reply(intake, catalog));
    tracing::info!(
        trace = crate::telemetry::id(),
        planner = "contracts",
        planning_ms = started.elapsed().as_millis() as u64,
        ready = reply.discover,
        "compiled parsed request without a model"
    );
    reply
}

/// Only the connected source selector calls this after checking the signed
/// option and each declared input schema. Model-created values continue to use
/// the ordinary scalar-literal validation in `compile`.
pub(crate) fn compile_selected_values(
    question: &str, literals: &str, task: &Task, catalog: &Catalog, intake: &Intake,
    explicit: &BTreeMap<(String, String), serde_json::Value>,
) -> ChatReply {
    compile_request(question, literals, task, catalog, intake, Some(explicit))
        .unwrap_or_else(|| missing_reply(intake, catalog))
}

fn compile_request(
    question: &str,
    literals: &str,
    task: &Task,
    catalog: &Catalog,
    intake: &Intake,
    explicit: Option<&BTreeMap<(String, String), serde_json::Value>>,
) -> Option<ChatReply> {
    let parsed = &intake.parsed_request;
    if parsed.meta.text_sha256 != hex::encode(sha2::Sha256::digest(question.as_bytes())) {
        return None;
    }
    let mut intent = Intent {
        subjects: vec![],
        goals: vec![],
        answer_operations: parsed.deliverable.operations.clone(),
        unresolved_references: vec![],
    };
    intent.answer_operations.push(format!(
        "Requested answer format: {}",
        parsed.deliverable.format
    ));
    let mut subjects = BTreeMap::new();
    for (index, cap) in parsed
        .capabilities
        .iter()
        .enumerate()
        .filter(|(_, cap)| !plan_coverage::answer_instruction(cap))
    {
        let subject = cap
            .subject_id
            .as_ref()
            .and_then(|id| parsed.subjects.iter().find(|s| &s.id == id));
        if subject.is_none() && !parsed.subjects.is_empty() {
            return None;
        }
        let mut part = intake.clone();
        part.parsed_request.capabilities = vec![cap.clone()];
        part.parsed_request.subjects = subject.into_iter().cloned().collect();
        part.matches = vec![request_match(intake, index)?.clone()];
        part.selected_capabilities = part.matches[0].operations.clone();
        let compiled = compile_requirement(question, literals, task, catalog, &part, explicit)?;
        let incoming = compiled.subjects.into_iter().next()?;
        let id = cap.subject_id.as_deref().unwrap_or("request");
        let target = if let Some(&target) = subjects.get(id) {
            let existing: &mut Subject = &mut intent.subjects[target];
            for (field, value) in incoming.values {
                if existing
                    .values
                    .get(&field)
                    .is_some_and(|prior| prior != &value)
                {
                    return None;
                }
                existing.values.insert(field, value);
            }
            target
        } else {
            let target = intent.subjects.len();
            subjects.insert(id, target);
            intent.subjects.push(incoming);
            target
        };
        for mut goal in compiled.goals {
            goal.subject = target;
            if !intent.goals.iter().any(|other| {
                other.subject == goal.subject
                    && other.capability == goal.capability
                    && other.constraints == goal.constraints
            }) {
                intent.goals.push(goal);
            }
        }
    }
    if intent.goals.is_empty()
        || intent.goals.len() > super::MAX_GOALS
        || parsed
            .subjects
            .iter()
            .any(|subject| !subjects.contains_key(subject.id.as_str()))
    {
        return None;
    }
    // Two requirements can choose the same record or a dependency supplied by
    // another goal. Remove a redundant purchase only after full scope proof.
    for index in (0..intent.goals.len()).rev() {
        if intent.goals.len() == 1 {
            break;
        }
        let mut smaller = intent.clone();
        smaller.goals.remove(index);
        if finalize(question, literals, task, catalog, intake, smaller.clone(), true, explicit).is_some_and(|(_, _, ready)| ready) {
            intent = smaller;
        }
    }
    let (mut intent, bindings, _) = finalize(question, literals, task, catalog, intake, intent, true, explicit)?;
    intent
        .answer_operations
        .extend(source_grounding::answer_instructions(&bindings, intake));
    Some(ChatReply {output_bindings:bindings,message:if parsed.language=="nl" {
        "Ik bereid de bronaanvragen voor. Zodra alle invoer compleet is, krijg je één totaalprijs ter goedkeuring."
    } else {"I’ll prepare the source requests. Once all inputs are complete, you’ll get one total price to approve."}.into(),
        discover:true,intent:Some(intent),decline_reason:None})
}

fn compile_requirement(
    question: &str,
    literals: &str,
    task: &Task,
    catalog: &Catalog,
    intake: &Intake,
    explicit: Option<&BTreeMap<(String, String), serde_json::Value>>,
) -> Option<Intent> {
    let cap = intake.parsed_request.capabilities.first()?;
    let label = intake
        .parsed_request
        .subjects
        .first()
        .map(|subject| subject.label.clone())
        .unwrap_or_else(|| question.into());
    let found = request_match(intake, 0)?;
    let mut choices = Vec::new();
    for candidate in &found.apiosk {
        let (Some(capability), Some(source), Some(endpoint)) = (
            &candidate.capability,
            &candidate.source_slug,
            &candidate.endpoint_id,
        ) else {
            continue;
        };
        if candidate.availability != "supported" || !found.operations.contains(capability) {
            continue;
        }
        let goal = Goal {
            capability: capability.clone(),
            subject: 0,
            constraints: BTreeMap::from([
                ("source".into(), json!(source)),
                ("endpoint_id".into(), json!(endpoint)),
            ]),
        };
        if !choices.iter().any(|other: &Goal| {
            other.capability == goal.capability && other.constraints == goal.constraints
        }) {
            choices.push(goal);
        }
    }
    choices.truncate(MAX_CHOICES);
    let mut queue: VecDeque<Vec<usize>> = (0..choices.len()).map(|i| vec![i]).collect();
    let mut attempts = 0;
    let mut draft = None;
    while let Some(indices) = queue.pop_front() {
        attempts += 1;
        if attempts > MAX_COMBINATIONS {
            break;
        }
        let intent = Intent {
            subjects: vec![Subject {
                entity_ref: None,
                label: label.clone(),
                values: Default::default(),
            }],
            goals: indices.iter().map(|i| choices[*i].clone()).collect(),
            answer_operations: vec![],
            unresolved_references: vec![],
        };
        if let Some((intent, _, ready)) = finalize(question, literals, task, catalog, intake, intent, false, explicit) {
            if ready {
                return Some(intent);
            }
            // A matching endpoint with a missing identifier is only a fallback.
            // Try the other admitted alternatives before asking for that value.
            if draft.is_none() {
                draft = Some(intent);
            }
            continue;
        }
        // Combining sources must add requested fields. The final resolver also
        // validates dependencies, input scope and redundant purchases.
        if indices.len() < cap.outputs.len().clamp(1, super::MAX_GOALS) {
            for next in indices.last().copied().unwrap() + 1..choices.len() {
                let mut union = indices.clone();
                union.push(next);
                queue.push_back(union);
                if queue.len() >= MAX_COMBINATIONS {
                    break;
                }
            }
        }
    }
    draft
}

fn finalize(
    question: &str,
    literals: &str,
    task: &Task,
    catalog: &Catalog,
    intake: &Intake,
    mut intent: Intent,
    complete_scope: bool,
    explicit: Option<&BTreeMap<(String, String), serde_json::Value>>,
) -> Option<(Intent, Vec<source_grounding::Binding>, bool)> {
    bind_inputs(&mut intent, intake, catalog);
    capability_binding::bind_source_requirements(&mut intent, intake, catalog);
    if intake.parsed_request.subjects.is_empty() {
        capability_binding::bind_ecb_target_literal(&mut intent, question, catalog);
    }
    let services = selected_services(&intent, catalog);
    let bindings = source_grounding::compile(intake, catalog, &services);
    plan_coverage::prepare_grounded(&mut intent, intake, catalog, &bindings).ok()?;
    intent.bind_constraint_inputs(literals, catalog).ok()?;
    // Keyword APIs need a supplied phrase. Do this after all input-binding
    // helpers; natural-language web-search contracts may use the full request.
    let normalized = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    if intent.subjects.iter().any(|subject| subject.values.iter().any(|(field,value)|
        matches!(field.as_str(), "company_intel.input.search" | "subsidy.discovery.keyword" | "source.koop.q")
            && value.as_str().is_some_and(|query| normalized(query) == normalized(question)))) {
        return None;
    }
    validate_selected_literals(&intent, literals, task, catalog, intake, explicit).ok()?;
    let mut preview = task.clone();
    preview.capability_intake = Some(intake.clone());
    preview.output_grounding = source_grounding::Grounding::new(intake, catalog, bindings.clone());
    preview.record_turn(question, None);
    let mut bound = intent.clone();
    crate::engine::bind_subjects(&mut preview, catalog, &mut bound).ok()?;
    if preview.independent_sources()
        && !plan_coverage::uncovered_intent_outputs(intake, catalog, &bound, &bindings).is_empty()
    {
        return None;
    }
    let plan = match crate::resolver::resolve(catalog, &preview, &bound) {
        crate::resolver::Resolution::Plan(plan) => plan,
        // This is a draft, never a quote. The engine emits the schema-bound
        // input action and rechecks all calls when the value is supplied.
        crate::resolver::Resolution::Missing { .. } if preview.independent_sources() => {
            if complete_scope && !plan_coverage::supplied_inputs_preserved(intake, catalog, &preview, &bound, question) {
                return None;
            }
            return Some((intent, bindings, false));
        },
        _ => return None,
    };
    // A provisional branch cannot consume identifiers supplied for another
    // retrieval on the same subject (e.g. a profile URL beside a company name).
    // Preserve them and prove every identifier on the complete combined plan;
    // only that final scope can leave compile_request as an approval-ready plan.
    if !plan_coverage::uncovered_outputs(&preview, catalog, &bound, &plan).is_empty()
        || (complete_scope && !plan_coverage::uncovered_inputs(&preview, catalog, &bound, &plan).is_empty())
    {
        return None;
    }
    Some((intent, bindings, true))
}

fn validate_selected_literals(
    intent: &Intent, literals: &str, task: &Task, catalog: &Catalog, intake: &Intake,
    explicit: Option<&BTreeMap<(String, String), serde_json::Value>>,
) -> anyhow::Result<()> {
    let Some(explicit) = explicit else { return intent.validate(literals, task, catalog); };
    let mut literal_view = intent.clone();
    for subject in &mut literal_view.subjects {
        let structured: Vec<_> = subject.values.iter().filter(|(_, value)| !value.is_string() && !value.is_number())
            .map(|(field, value)| (field.clone(), value.clone())).collect();
        for (field, value) in structured {
            anyhow::ensure!(intake.parsed_request.subjects.iter().any(|parsed| parsed.label == subject.label
                && explicit.get(&(parsed.id.clone(), field.clone())) == Some(&value)), "non-scalar value was not explicitly entered for this subject");
            let schema = &catalog.vocabulary.get(&field).ok_or_else(|| anyhow::anyhow!("unknown input type"))?.value_schema;
            catalog_core::facts::schema::validate(schema, &value)?;
            subject.values.remove(&field);
        }
    }
    // Keep every normal label, scalar, goal and constraint check. The original
    // typed values still reach coverage validation, the resolver and execution.
    literal_view.validate(literals, task, catalog)
}

fn selected_services<'a>(
    intent: &Intent,
    catalog: &'a Catalog,
) -> Vec<&'a crate::catalog::Service> {
    catalog
        .services
        .iter()
        .filter(|service| {
            intent.goals.iter().any(|goal| {
                goal.capability == service.contract.capability_slug
                    && goal
                        .constraints
                        .get("source")
                        .and_then(serde_json::Value::as_str)
                        == Some(service.api_slug.as_str())
                    && plan_coverage::endpoint_matches(
                        goal.constraints.get("endpoint_id"),
                        Some(&service.endpoint_id.to_string()),
                    )
            })
        })
        .collect()
}

fn bind_inputs(intent: &mut Intent, intake: &Intake, catalog: &Catalog) {
    // Bind only inputs of the selected calls from the supplied subject.
    // Dependency values remain absent until an approved prerequisite completes.
    for index in 0..intent.subjects.len() {
        let Some(subject) = intake
            .parsed_request
            .subjects
            .iter()
            .find(|subject| subject.label == intent.subjects[index].label)
        else {
            continue;
        };
        let fields: BTreeSet<_> = selected_services(intent, catalog)
            .into_iter()
            .filter(|service| intent.goals.iter().any(|goal|
                goal.subject == index && goal.capability == service.contract.capability_slug))
            .flat_map(|service| service.contract.input_map.keys().cloned())
            .collect();
        for field in fields {
            let Some(fact) = catalog.vocabulary.get(&field) else {
                continue;
            };
            let Some(mut value) = crate::ask_v2::input_values::documented_literal(
                &field,
                &fact.value_schema,
                subject,
            ) else {
                continue;
            };
            if catalog_core::facts::schema::validate(&fact.value_schema, &value).is_err() {
                let Some(typed) = value
                    .as_str()
                    .and_then(|text| serde_json::from_str(text).ok())
                    .filter(|value| {
                        catalog_core::facts::schema::validate(&fact.value_schema, value).is_ok()
                    })
                else {
                    continue;
                };
                value = typed;
            }
            intent.subjects[index].values.entry(field).or_insert(value);
        }
    }
}

fn missing_reply(intake: &Intake, _catalog: &Catalog) -> ChatReply {
    plan_coverage::uncovered_reply(intake).unwrap_or_else(|| plan_coverage::rejected_reply(intake))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability_intake::{Candidate, Match};

    fn fixture() -> (Catalog, Intake, String) {
        let mut snapshot: crate::catalog::Snapshot = serde_json::from_str(include_str!(
            "../../tests/fixtures/integration-catalog.json"
        ))
        .unwrap();
        for service in &mut snapshot.services {
            service.api_slug = "newly-registered-source".into();
            if service.contract.capability_slug == "company.accounts" {
                service.contract.identifies = Some("company.id".into());
                service.contract.output_map.produces.insert("company.id".into(),
                    serde_json::from_value(json!({"path":"$.id"})).unwrap());
            }
        }
        let mut additional = snapshot
            .services
            .iter()
            .find(|service| service.contract.capability_slug == "company.accounts")
            .unwrap()
            .clone();
        additional.id = uuid::Uuid::new_v4();
        additional.endpoint_id = uuid::Uuid::new_v4();
        additional.api_slug = "another-registered-source".into();
        additional.contract.capability_slug = "company.status".into();
        additional.contract.output_map.produces =
            serde_json::from_value(json!({"company.status":{"path":"$.status"}, "company.id":{"path":"$.id"}})).unwrap();
        additional.sample_payload = Some(json!({"status":"active"}));
        snapshot.services.push(additional);
        snapshot
            .vocabulary
            .push(catalog_core::facts::contract::FactType {
                slug: "company.status".into(),
                value_schema: json!({"type":"string"}),
            });
        let catalog = Catalog::build(snapshot);
        assert!(catalog.rejected.is_empty());
        let question = "Show the accounts and status for Arbitrary Company (company ID 001234) as JSON".to_owned();
        let parsed=crate::capability_intake::normalize(&json!({"language":"en",
            "subjects":[{"id":"s1","label":"Arbitrary Company","type":"company","identifiers":{"company_name":"Arbitrary Company","company.id":"001234"}}],
            "capabilities":[{"slug":"company.records","name":"Company records","description":"Retrieve company accounts and status.","domain":"company",
                "inputs":["company_name"],"outputs":["accounts","status"],"subject_id":"s1","source_hint":null,"confidence":1}],
            "deliverable":{"format":"json","operations":["compare"]}}).to_string(),&question,"one-intake").unwrap();
        let apiosk: Vec<_> = catalog
            .services
            .iter()
            .filter(|s| s.contract.capability_slug != "company.search")
            .map(|s| Candidate {
                origin: "apiosk".into(),
                name: s.name.clone(),
                description: String::new(),
                source_slug: Some(s.api_slug.clone()),
                capability: Some(s.contract.capability_slug.clone()),
                endpoint_id: Some(s.endpoint_id.to_string()),
                url: None,
                method: "GET".into(),
                price: None,
                network: None,
                calls_30d: None,
                availability: "supported".into(),
            })
            .collect();
        let operations = apiosk.iter().filter_map(|c| c.capability.clone()).collect();
        let intake = Intake {
            parsed_request: parsed,
            matches: vec![Match {
                slug: "company.records".into(),
                query: String::new(),
                operations,
                apiosk,
                coinbase: vec![],
                coinbase_status: "skipped".into(),
            }],
            selected_capabilities: BTreeSet::from([
                "company.accounts".into(),
                "company.status".into(),
            ]),
            apiosk_sources_searched: 2,
            catalog_version: catalog.version.clone(),
        };
        (catalog, intake, question)
    }

    #[test]
    fn new_contracts_combine_without_question_templates_or_a_model() {
        let (catalog, intake, question) = fixture();
        let before = serde_json::to_value(&intake).unwrap();
        let reply = compile(&question, &question, &Task::default(), &catalog, &intake);
        assert!(reply.discover, "{}", reply.message);
        let mut intent = reply.intent.unwrap();
        assert_eq!(intent.goals.len(), 2);
        assert!(intent
            .answer_operations
            .iter()
            .any(|operation| operation.contains("json")));
        let mut task = Task {
            capability_intake: Some(intake.clone()),
            ..Default::default()
        };
        task.record_turn(&question, None);
        crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
        let crate::resolver::Resolution::Plan(plan) =
            crate::resolver::resolve(&catalog, &task, &intent)
        else {
            panic!("complete source union")
        };
        assert_eq!(
            plan.steps.len(),
            2,
            "both requested sources use the supplied identifier independently"
        );
        assert_eq!(
            plan.steps
                .iter()
                .filter(|s| s.service.contract.capability_slug == "company.search")
                .count(),
            0
        );
        assert_eq!(
            plan.cap_atomic,
            plan.steps.iter().map(|s| plan.step_total(s)).sum::<i64>()
        );
        assert_eq!(serde_json::to_value(intake).unwrap(), before);
    }

    #[test]
    fn ungrounded_scope_and_stale_intakes_never_become_partial_quotes() {
        let (catalog, intake, question) = fixture();
        for change in [
            "unavailable_output",
            "unused_filter",
            "stale_question",
            "discovery_only",
        ] {
            let mut changed = intake.clone();
            match change {
                "unavailable_output" => changed.parsed_request.capabilities[0]
                    .outputs
                    .push("private_notes".into()),
                "unused_filter" => {
                    changed.parsed_request.subjects[0]
                        .identifiers
                        .insert("period".into(), "2024".into());
                }
                "stale_question" => changed.parsed_request.meta.text_sha256 = "stale".into(),
                _ => {
                    for candidate in &mut changed.matches[0].apiosk {
                        candidate.availability = "discovery_only".into();
                    }
                }
            }
            let reply = compile(&question, &question, &Task::default(), &catalog, &changed);
            assert!(!reply.discover && reply.intent.is_none(), "{change}");
        }
    }
}
