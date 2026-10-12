//! Explicit source includes augment the original interpreted question. The
//! server-issued option owns the output scope; callers submit references and
//! literal input values only. This module neither purchases nor authorizes.
use super::source_options::SourceOption;
use crate::{
    capability_intake::{Intake, Match},
    catalog::Catalog,
    parser::{Intent, ModelSelection},
    resolver::Plan,
};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub const MAX_SOURCES: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Include,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub version: u8,
    pub mode: Mode,
    pub sources: Vec<SelectedSource>,
}
impl Default for Selection {
    fn default() -> Self {
        Self {
            version: 1,
            mode: Mode::Include,
            sources: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SelectedSource {
    pub endpoint_id: Uuid,
    pub capability: String,
    pub subject_id: String,
    #[serde(default)]
    pub input: BTreeMap<String, Value>,
}

/// Saved only from a verified search snapshot. Repricing reads this record,
/// never client-supplied capabilities, schemas or source descriptions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Context {
    pub question: String,
    pub original_intake: Intake,
    pub search_id: String,
    pub discovery_id: Option<String>,
    pub(crate) options: Vec<SourceOption>,
    pub selection: Selection,
}
impl Context {
    pub fn view(&self) -> Value {
        json!({"question":self.question,"parsed_request":self.original_intake.parsed_request,
            "search_id":self.search_id,"discovery_id":self.discovery_id,"options":self.options,
            "selection":self.selection,"can_choose":true,"max_sources":MAX_SOURCES})
    }

    pub fn model_selection(&self) -> ModelSelection {
        ModelSelection {
            view: json!({"mode":"include",
            "rule":"Every listed source was explicitly included by the person. Preserve all original requirements and every included endpoint for its exact subject. Each included requirement is already in parsed_request and its sole matched endpoint is authoritative. Do not add unselected optional sources. Prices and approval remain backend-owned.",
            "sources":self.selection.sources}),
            literals: self
                .selection
                .sources
                .iter()
                .flat_map(|source| source.input.values().filter_map(literal))
                .collect(),
        }
    }

    pub fn expanded(&self, catalog: &Catalog) -> Result<Intake> {
        ensure!(
            self.selection.version == 1 && self.selection.sources.len() <= MAX_SOURCES,
            "invalid source selection size or version"
        );
        ensure!(
            self.original_intake.catalog_version == catalog.version,
            "Refresh the source options before changing this plan"
        );
        let mut intake = self.original_intake.clone();
        let mut selected = BTreeSet::new();
        let mut supplied = BTreeMap::new();
        let mut total = 0usize;
        let mut additions = Vec::new();
        for choice in &self.selection.sources {
            ensure!(
                !choice.endpoint_id.is_nil()
                    && selected.insert((
                        choice.endpoint_id,
                        &choice.capability,
                        &choice.subject_id
                    )),
                "a source was selected twice or has no identity"
            );
            let option = self
                .options
                .iter()
                .find(|option| {
                    option.subject_id == choice.subject_id
                        && option.source.endpoint_id.as_deref()
                            == Some(choice.endpoint_id.to_string().as_str())
                        && option.source.capability.as_deref() == Some(choice.capability.as_str())
                })
                .ok_or_else(|| {
                    anyhow::anyhow!("Selected source was not offered for this subject")
                })?;
            ensure!(
                option.source.origin == "apiosk"
                    && option.source.availability == "supported"
                    && option.requirement.subject_id.as_deref() == Some(choice.subject_id.as_str())
                    && !option.requirement.outputs.is_empty(),
                "Selected source has no executable requested scope"
            );
            let service = catalog
                .services
                .iter()
                .find(|service| {
                    service.endpoint_id == choice.endpoint_id
                        && service.contract.capability_slug == choice.capability
                        && option.source.source_slug.as_deref() == Some(service.api_slug.as_str())
                })
                .ok_or_else(|| anyhow::anyhow!("Selected source is no longer executable"))?;
            let subject = intake
                .parsed_request
                .subjects
                .iter_mut()
                .find(|subject| subject.id == choice.subject_id)
                .ok_or_else(|| anyhow::anyhow!("Selected source belongs to an unknown subject"))?;
            ensure!(choice.input.len() <= 16, "Too many values for one source");
            let mut requirement = option.requirement.clone();
            for (field, value) in &choice.input {
                ensure!(
                    service
                        .contract
                        .requires
                        .iter()
                        .any(|required| required.fact_type == *field)
                        && service.contract.input_map.contains_key(field)
                        || crate::engine::Engine::endpoint_lookup(catalog, service)
                            .is_some_and(|(seed, _)| seed == field),
                    "Selected source does not accept this input"
                );
                let text = literal(value)
                    .filter(|text| !text.trim().is_empty() && text.chars().count() <= 500)
                    .ok_or_else(|| anyhow::anyhow!("Source input must be a short literal value"))?;
                total += text.chars().count();
                ensure!(total <= 4000, "Selected source values are too long");
                let fact = catalog
                    .vocabulary
                    .get(field)
                    .ok_or_else(|| anyhow::anyhow!("Unknown source input"))?;
                catalog_core::facts::schema::validate(&fact.value_schema, value)?;
                ensure!(
                    super::input_values::documented_literal(field, &fact.value_schema, subject)
                        .is_none_or(|previous| previous == *value
                            || literal(&previous).as_deref() == Some(text.as_str())),
                    "An entered value conflicts with the original question"
                );
                let key = (&choice.subject_id, field);
                ensure!(
                    supplied
                        .insert(key, value)
                        .is_none_or(|previous| previous == value),
                    "Selected sources have conflicting values for one subject"
                );
                ensure!(
                    subject
                        .identifiers
                        .get(field)
                        .is_none_or(|previous| previous == &text),
                    "An entered value conflicts with the original question"
                );
                subject.identifiers.insert(field.clone(), text);
                if !requirement.inputs.contains(field) {
                    requirement.inputs.push(field.clone());
                }
                requirement.missing.retain(|missing| missing != field);
            }
            // A source-specific occurrence cannot be satisfied by a different
            // selected provider with similar outputs, or by an unused match.
            requirement.source_hint = Some(service.api_slug.clone());
            let matched = Match {
                slug: requirement.slug.clone(),
                query: requirement.description.clone(),
                operations: BTreeSet::from([choice.capability.clone()]),
                apiosk: vec![option.source.clone()],
                coinbase: vec![],
                coinbase_status: "complete".into(),
            };
            additions.push((requirement, matched));
            intake
                .selected_capabilities
                .insert(choice.capability.clone());
        }
        if !self.selection.sources.is_empty() {
            // The person chooses roots, not the resolver's typed dependencies.
            // A newly selected optional source may cover an unhinted original
            // requirement only with exact output-contract proof.
            let mut occurrences = BTreeMap::<String, usize>::new();
            for requirement in &intake.parsed_request.capabilities {
                let occurrence = occurrences.entry(requirement.slug.clone()).or_default();
                let matched = intake
                    .matches
                    .iter_mut()
                    .filter(|matched| matched.slug == requirement.slug)
                    .nth(*occurrence)
                    .ok_or_else(|| anyhow::anyhow!("Original source scope is inconsistent"))?;
                *occurrence += 1;
                matched.apiosk.retain(|candidate| {
                    self.selection.sources.iter().any(|choice| {
                        requirement.subject_id.as_deref() == Some(choice.subject_id.as_str())
                            && candidate.endpoint_id.as_deref()
                                == Some(choice.endpoint_id.to_string().as_str())
                            && candidate.capability.as_deref() == Some(choice.capability.as_str())
                    })
                });
                if requirement.source_hint.is_none() {
                    for (added, offered) in &additions {
                        if requirement.subject_id != added.subject_id {
                            continue;
                        }
                        let candidate = &offered.apiosk[0];
                        let service = catalog
                            .services
                            .iter()
                            .find(|service| {
                                candidate.endpoint_id.as_deref()
                                    == Some(service.endpoint_id.to_string().as_str())
                                    && candidate.capability.as_deref()
                                        == Some(service.contract.capability_slug.as_str())
                            })
                            .unwrap();
                        if crate::parser::plan_coverage::outputs_cover(
                            requirement,
                            service,
                            catalog,
                        ) && !matched.apiosk.iter().any(|existing| {
                            existing.endpoint_id == candidate.endpoint_id
                                && existing.capability == candidate.capability
                        }) {
                            matched.apiosk.push(candidate.clone());
                        }
                    }
                }
                matched.operations = matched
                    .apiosk
                    .iter()
                    .filter_map(|candidate| candidate.capability.clone())
                    .collect();
            }
        }
        for (requirement, matched) in additions {
            intake.parsed_request.capabilities.push(requirement);
            intake.matches.push(matched);
        }
        intake.selected_capabilities = intake
            .matches
            .iter()
            .flat_map(|matched| matched.operations.iter().cloned())
            .collect();
        ensure!(
            intake.parsed_request.capabilities.len() <= 32,
            "Too many combined source requirements"
        );
        Ok(intake)
    }

    /// The person has already selected exact catalog contracts. Compile those
    /// roots and their validated literals by parsed subject id; a second model
    /// cannot drop a selected source or an entered lookup seed. The normal
    /// resolver and full pre-quote input/output proof still decide readiness.
    pub fn compile(&self, catalog: &Catalog) -> Result<Option<crate::parser::ChatReply>> {
        if self.selection.sources.is_empty() {
            return Ok(None);
        }
        let intake = self.expanded(catalog)?;
        let mut intent = Intent {
            subjects: vec![],
            goals: vec![],
            answer_operations: intake.parsed_request.deliverable.operations.clone(),
            unresolved_references: vec![],
        };
        intent.answer_operations.push(format!(
            "Requested answer format: {}",
            intake.parsed_request.deliverable.format
        ));
        let mut subjects = BTreeMap::new();
        for choice in &self.selection.sources {
            let parsed = intake
                .parsed_request
                .subjects
                .iter()
                .find(|subject| subject.id == choice.subject_id)
                .unwrap();
            let service = catalog
                .services
                .iter()
                .find(|service| {
                    service.endpoint_id == choice.endpoint_id
                        && service.contract.capability_slug == choice.capability
                })
                .unwrap();
            let index = *subjects
                .entry(choice.subject_id.clone())
                .or_insert_with(|| {
                    let index = intent.subjects.len();
                    intent.subjects.push(crate::parser::Subject {
                        entity_ref: None,
                        label: parsed.label.clone(),
                        values: BTreeMap::new(),
                    });
                    index
                });
            let mut fields: BTreeSet<_> = service
                .contract
                .requires
                .iter()
                .map(|input| input.fact_type.as_str())
                .collect();
            if let Some((seed, _)) = crate::engine::Engine::endpoint_lookup(catalog, service) {
                fields.insert(seed);
            }
            for field in fields {
                let Some(fact) = catalog.vocabulary.get(field) else {
                    continue;
                };
                let Some(mut value) = choice.input.get(field).cloned().or_else(|| {
                    super::input_values::documented_literal(field, &fact.value_schema, parsed)
                }) else {
                    continue;
                };
                if catalog_core::facts::schema::validate(&fact.value_schema, &value).is_err() {
                    value = value
                        .as_str()
                        .and_then(|value| serde_json::from_str(value).ok())
                        .ok_or_else(|| {
                            anyhow::anyhow!("Selected source input has the wrong type")
                        })?;
                }
                catalog_core::facts::schema::validate(&fact.value_schema, &value)?;
                let values = &mut intent.subjects[index].values;
                ensure!(
                    values.get(field).is_none_or(|existing| existing == &value),
                    "Selected source bindings conflict"
                );
                values.insert(field.into(), value);
            }
            intent.goals.push(crate::parser::Goal {
                subject: index,
                capability: choice.capability.clone(),
                constraints: BTreeMap::from([
                    ("endpoint_id".into(), json!(choice.endpoint_id)),
                    ("source".into(), json!(service.api_slug)),
                ]),
            });
        }
        // Exact selections still need semantic matching when the user's
        // wording differs from the selected source fields. Let the bounded
        // planner map those meanings before the final coverage check.
        let mut draft=crate::state::Task {capability_intake:Some(intake),source_selection:Some(self.clone()),..Default::default()};
        draft.record_turn(&self.question,None);
        let mut checked=intent.clone();
        crate::engine::bind_subjects(&mut draft,catalog,&mut checked)?;
        if let crate::resolver::Resolution::Plan(plan)=crate::resolver::resolve(catalog,&draft,&checked) {
            if self.uncovered_inputs(&draft,catalog,&checked,&plan)?.is_empty()
                && !self.uncovered_outputs(&draft,catalog,&checked,&plan)?.is_empty() { return Ok(None); }
        }
        Ok(Some(crate::parser::ChatReply { output_bindings: vec![],
            message:
                "I will prepare the selected sources and show one total price before approval."
                    .into(),
            discover: true,
            intent: Some(intent),
            decline_reason: Some("none".into()),
        }))
    }

    /// Refresh server-owned catalog evidence without reinterpreting or changing
    /// the original question. Selections are revalidated against current offers.
    pub fn refresh(&mut self, catalog: &Catalog) {
        if self.original_intake.catalog_version == catalog.version {
            return;
        }
        self.original_intake.matches =
            super::search::search(&self.original_intake.parsed_request, catalog);
        self.original_intake.selected_capabilities = self
            .original_intake
            .matches
            .iter()
            .flat_map(|found| found.operations.iter().cloned())
            .collect();
        self.original_intake.catalog_version = catalog.version.clone();
        self.original_intake.apiosk_sources_searched =
            crate::capability_intake::source_count(catalog);
        self.options = super::source_options::options(
            &self.original_intake.parsed_request,
            &self.original_intake.matches,
            catalog,
        );
    }

    /// Required question criteria belong to the original retrieval chains.
    /// Explicit optional facets do not retroactively impose their inputs on
    /// those chains (or inherit an unrelated source-specific country filter).
    /// The selected compiler separately pins and proves every optional literal.
    pub fn uncovered_inputs(
        &self,
        task: &crate::state::Task,
        catalog: &Catalog,
        intent: &Intent,
        plan: &Plan,
    ) -> Result<Vec<String>> {
        if self.selection.sources.is_empty() {
            return Ok(crate::parser::plan_coverage::uncovered_inputs(
                task, catalog, intent, plan,
            ));
        }
        let mut scoped = task.clone();
        scoped.capability_intake = Some(self.original_proof_intake(task, catalog, intent, plan)?);
        Ok(crate::parser::plan_coverage::uncovered_inputs(
            &scoped, catalog, intent, plan,
        ))
    }

    pub fn uncovered_outputs(
        &self,
        task: &crate::state::Task,
        catalog: &Catalog,
        intent: &Intent,
        plan: &Plan,
    ) -> Result<Vec<String>> {
        if self.selection.sources.is_empty() {
            return Ok(crate::parser::plan_coverage::uncovered_outputs(
                task, catalog, intent, plan,
            ));
        }
        let original = self.original_proof_intake(task, catalog, intent, plan)?;
        let mut intake = self.expanded(catalog)?;
        // Original outputs must come from the same chains that prove the
        // original scope. Explicit optional requirements stay independently
        // pinned to their selected endpoints in the remainder of this intake.
        intake
            .matches
            .splice(..original.matches.len(), original.matches);
        let mut scoped = task.clone();
        scoped.capability_intake = Some(intake);
        Ok(crate::parser::plan_coverage::uncovered_outputs(
            &scoped, catalog, intent, plan,
        ))
    }

    fn original_proof_intake(
        &self,
        task: &crate::state::Task,
        catalog: &Catalog,
        intent: &Intent,
        plan: &Plan,
    ) -> Result<Intake> {
        let expanded = self.expanded(catalog)?;
        let mut original = self.original_intake.clone();
        original.matches = expanded
            .matches
            .into_iter()
            .take(original.matches.len())
            .collect();
        original.selected_capabilities = original
            .matches
            .iter()
            .flat_map(|matched| matched.operations.iter().cloned())
            .collect();
        // A broad requirement can match several selected roots. For a
        // jurisdiction-qualified retrieval, global optional facets are not
        // alternative proofs of that retrieval's country. Check each candidate
        // chain with the ordinary input guard before assembling the full proof.
        // A real country parameter and reviewed fixed-country route both work;
        // an unrelated optional registry can never lend its country to a root.
        for (index, cap) in original.parsed_request.capabilities.iter().enumerate() {
            let Some(subject) = original
                .parsed_request
                .subjects
                .iter()
                .find(|subject| cap.subject_id.as_deref() == Some(subject.id.as_str()))
            else {
                continue;
            };
            let mut country_subject = subject.clone();
            country_subject.identifiers.retain(|key, _| {
                matches!(
                    key.to_lowercase().replace(['.', '-'], "_").as_str(),
                    "country" | "country_code"
                )
            });
            if country_subject.identifiers.is_empty() {
                continue;
            }
            let occurrence = original.parsed_request.capabilities[..index]
                .iter()
                .filter(|previous| previous.slug == cap.slug)
                .count();
            let Some(match_index) = original
                .matches
                .iter()
                .enumerate()
                .filter(|(_, matched)| matched.slug == cap.slug)
                .nth(occurrence)
                .map(|(index, _)| index)
            else {
                continue;
            };
            let found = original.matches[match_index].clone();
            let mut country_cap = cap.clone();
            country_cap.inputs.clear();
            country_cap.missing.clear();
            let mut proving = Vec::new();
            for candidate in &found.apiosk {
                let mut probe = original.clone();
                probe.parsed_request.subjects = vec![country_subject.clone()];
                probe.parsed_request.capabilities = vec![country_cap.clone()];
                let mut candidate_match = found.clone();
                candidate_match.apiosk = vec![candidate.clone()];
                candidate_match.operations = candidate.capability.iter().cloned().collect();
                probe.matches = vec![candidate_match];
                let mut scoped = task.clone();
                scoped.capability_intake = Some(probe);
                if crate::parser::plan_coverage::uncovered_inputs(&scoped, catalog, intent, plan)
                    .is_empty()
                {
                    proving.push(candidate.clone());
                }
            }
            original.matches[match_index].operations = proving
                .iter()
                .filter_map(|candidate| candidate.capability.clone())
                .collect();
            original.matches[match_index].apiosk = proving;
        }
        for choice in &self.selection.sources {
            if original
                .parsed_request
                .capabilities
                .iter()
                .enumerate()
                .any(|(index, cap)| {
                    if cap.subject_id.as_deref() != Some(choice.subject_id.as_str()) {
                        return false;
                    }
                    let occurrence = original.parsed_request.capabilities[..index]
                        .iter()
                        .filter(|previous| previous.slug == cap.slug)
                        .count();
                    original
                        .matches
                        .iter()
                        .filter(|matched| matched.slug == cap.slug)
                        .nth(occurrence)
                        .is_some_and(|matched| {
                            matched.apiosk.iter().any(|candidate| {
                                candidate.endpoint_id.as_deref()
                                    == Some(choice.endpoint_id.to_string().as_str())
                                    && candidate.capability.as_deref()
                                        == Some(choice.capability.as_str())
                            })
                        })
                })
            {
                if let Some(subject) = original
                    .parsed_request
                    .subjects
                    .iter_mut()
                    .find(|subject| subject.id == choice.subject_id)
                {
                    for (field, value) in &choice.input {
                        if let Some(value) = literal(value) {
                            subject.identifiers.insert(field.clone(), value);
                        }
                    }
                }
            }
        }
        Ok(original)
    }

    /// A successful quote must include every chosen endpoint for the complete
    /// subject scope. Labels alone are insufficient when two subjects share a
    /// name but carry different locations or identifiers.
    pub fn verify_plan(
        &self,
        task: &crate::state::Task,
        catalog: &Catalog,
        intent: &Intent,
        plan: &Plan,
    ) -> Result<()> {
        if self.selection.sources.is_empty() {
            return Ok(());
        }
        let expected = self.compile(catalog)?.unwrap().intent.unwrap();
        let ids: Vec<_> = self
            .selection
            .sources
            .iter()
            .map(|choice| choice.subject_id.as_str())
            .fold(Vec::new(), |mut ids, id| {
                if !ids.contains(&id) {
                    ids.push(id);
                }
                ids
            });
        let identity = |id: &str, index: usize| {
            let Some(actual) = intent.subjects.get(index) else {
                return false;
            };
            let Some(expected) = ids
                .iter()
                .position(|candidate| *candidate == id)
                .and_then(|index| expected.subjects.get(index))
            else {
                return false;
            };
            actual.entity_ref.is_some()
                && actual.label == expected.label
                && expected.values.iter().all(|(field, value)| {
                    actual.values.get(field) == Some(value)
                        && crate::catalog::fact_value(
                            &task.facts,
                            actual.entity_ref.unwrap(),
                            field,
                        )
                        .is_ok_and(|bound| bound.as_ref() == Some(value))
                })
        };
        ensure!(
            intent
                .goals
                .iter()
                .all(
                    |goal| self.selection.sources.iter().any(|choice| choice.capability
                        == goal.capability
                        && identity(&choice.subject_id, goal.subject)
                        && goal.constraints.get("endpoint_id").and_then(Value::as_str)
                            == Some(choice.endpoint_id.to_string().as_str()))
                ),
            "An unselected optional source was added to the plan"
        );
        for choice in &self.selection.sources {
            ensure!(
                intent
                    .subjects
                    .iter()
                    .enumerate()
                    .any(|(index, subject)| identity(&choice.subject_id, index)
                        && intent.goals.iter().any(|goal| goal.subject == index
                            && goal.capability == choice.capability
                            && goal.constraints.get("endpoint_id").and_then(Value::as_str)
                                == Some(choice.endpoint_id.to_string().as_str()))
                        && subject
                            .entity_ref
                            .is_some_and(|entity| plan.steps.iter().any(|step| step.entity_ref
                                == entity
                                && step.service.endpoint_id == choice.endpoint_id
                                && step.service.contract.capability_slug == choice.capability))),
                "A selected source was omitted from the plan"
            );
        }
        Ok(())
    }
}

fn literal(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{catalog::Snapshot, parser::Parser, state::Task};
    fn fixture() -> (Context, Catalog) {
        let saved: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/company-reputation/combined-intake.json"
        ))
        .unwrap();
        let mut snapshot: Snapshot = serde_json::from_str(include_str!(
            "../../tests/fixtures/company-reputation/catalog.json"
        ))
        .unwrap();
        let reviewed: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/ecb-split-currency/reviewed-schemas.json"
        ))
        .unwrap();
        for fact in &mut snapshot.vocabulary {
            if let Some(row) = reviewed["after"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["slug"] == fact.slug)
            {
                fact.value_schema = row["value_schema"].clone();
            }
        }
        let catalog = Catalog::build(snapshot);
        let mut parsed: crate::capability_intake::ParsedRequest =
            serde_json::from_value(saved["parsed_request"].clone()).unwrap();
        parsed.capabilities.truncate(1);
        parsed.subjects.truncate(1);
        let question = saved["question"].as_str().unwrap().to_owned();
        let matches = crate::ask_v2::search::search(&parsed, &catalog);
        let options = super::super::source_options::options(&parsed, &matches, &catalog);
        let original_intake = Intake {
            parsed_request: parsed,
            selected_capabilities: matches
                .iter()
                .flat_map(|m| m.operations.iter().cloned())
                .collect(),
            matches,
            apiosk_sources_searched: 3,
            catalog_version: catalog.version.clone(),
        };
        (
            Context {
                question,
                original_intake,
                search_id: "server-attested".into(),
                discovery_id: None,
                options,
                selection: Selection::default(),
            },
            catalog,
        )
    }
    fn choose(context: &Context, term: &str) -> SelectedSource {
        let option = context
            .options
            .iter()
            .find(|option| {
                option
                    .source
                    .capability
                    .as_deref()
                    .is_some_and(|cap| cap.contains(term))
            })
            .unwrap();
        SelectedSource {
            endpoint_id: option.source.endpoint_id.as_ref().unwrap().parse().unwrap(),
            capability: option.source.capability.clone().unwrap(),
            subject_id: option.subject_id.clone(),
            input: BTreeMap::new(),
        }
    }
    async fn compile(context: &Context, catalog: &Catalog) -> (Task, Intent, Plan) {
        let intake = context.expanded(catalog).unwrap();
        let parser = Parser::new(
            reqwest::Client::new(),
            "offline://no-fallback-model".into(),
            String::new(),
            "fixture".into(),
        );
        let mut task = Task {
            capability_intake: Some(intake.clone()),
            source_selection: Some(context.clone()),
            ..Default::default()
        };
        task.record_turn(&context.question, None);
        let reply = if let Some(reply) = context.compile(catalog).unwrap() {
            reply
        } else {
            parser
                .chat_from_capabilities(
                    &context.question,
                    Some(&context.model_selection()),
                    &task,
                    catalog,
                    &intake,
                )
                .await
                .expect("catalog-contract selected roots compile without a model or paid call")
        };
        let mut intent = reply.intent.unwrap();
        crate::engine::bind_subjects(&mut task, catalog, &mut intent).unwrap();
        let crate::resolver::Resolution::Plan(plan) =
            crate::resolver::resolve(catalog, &task, &intent)
        else {
            panic!("selected roots need complete aggregate plan")
        };
        assert!(context
            .uncovered_inputs(&task, catalog, &intent, &plan)
            .unwrap()
            .is_empty());
        assert!(context
            .uncovered_outputs(&task, catalog, &intent, &plan)
            .unwrap()
            .is_empty());
        context.verify_plan(&task, catalog, &intent, &plan).unwrap();
        (task, intent, plan)
    }
    #[tokio::test]
    async fn explicitly_selected_optional_root_is_in_one_plan_and_cannot_disappear() {
        let (mut context, catalog) = fixture();
        context.selection.sources = vec![
            choose(&context, "crunchbase-search"),
            choose(&context, "glassdoor"),
        ];
        let (task, intent, plan) = compile(&context, &catalog).await;
        assert_eq!(intent.goals.len(), 2);
        assert_eq!(plan.steps.len(), 2);
        assert!(plan.cap_atomic > 0);
        assert_eq!(
            plan.cap_atomic,
            plan.steps
                .iter()
                .map(|step| step.service.total())
                .sum::<i64>()
        );
        let inputs: Vec<_> = plan
            .steps
            .iter()
            .map(|step| {
                step.service
                    .inputs(&task, step.entity_ref, &catalog.vocabulary)
                    .unwrap()
            })
            .collect();
        assert!(inputs
            .iter()
            .all(|input| input.to_string().contains("Starbucks")));
        assert!(task.completed.is_empty() && task.active_results.is_empty());
        let mut omitted = plan.clone();
        omitted.steps.pop();
        assert!(context
            .verify_plan(&task, &catalog, &intent, &omitted)
            .is_err());
        let mut unselected = intent.clone();
        unselected.goals[0]
            .constraints
            .insert("endpoint_id".into(), json!(Uuid::new_v4()));
        assert!(context
            .verify_plan(&task, &catalog, &unselected, &plan)
            .is_err());
        context.selection.sources.clear();
        let (_, automatic, automatic_plan) = compile(&context, &catalog).await;
        assert_eq!(automatic.goals.len(), 1);
        assert_eq!(automatic_plan.steps.len(), 1);
    }
    #[tokio::test]
    async fn selected_profile_binds_reviewed_lookup_seed_with_equal_semantic_alias() {
        let (mut context, _) = fixture();
        let catalog = Catalog::build(
            serde_json::from_str(include_str!(
                "../../tests/fixtures/integration-catalog.json"
            ))
            .unwrap(),
        );
        let parsed = &mut context.original_intake.parsed_request;
        let cap = &mut parsed.capabilities[0];
        cap.slug = "company.financial.statements".into();
        cap.name = "Company accounts".into();
        cap.description = "Retrieve company accounts".into();
        cap.source_hint = None;
        cap.outputs = vec!["accounts".into()];
        context.original_intake.matches =
            super::super::search::search(&context.original_intake.parsed_request, &catalog);
        context.original_intake.catalog_version = catalog.version.clone();
        context.options = super::super::source_options::options(
            &context.original_intake.parsed_request,
            &context.original_intake.matches,
            &catalog,
        );
        let mut selected = choose(&context, "company.accounts");
        selected
            .input
            .insert("company.name".into(), json!("Starbucks"));
        context.selection.sources = vec![selected];
        let expanded = context.expanded(&catalog).unwrap();
        assert_eq!(
            expanded.parsed_request.subjects[0].identifiers["company_name"],
            "Starbucks"
        );
        assert_eq!(
            expanded.parsed_request.subjects[0].identifiers["company.name"],
            "Starbucks"
        );
        let (task, intent, plan) = compile(&context, &catalog).await;
        assert_eq!(
            intent.subjects[0].values["company.name"],
            json!("Starbucks")
        );
        assert_eq!(
            intent.goals.len(),
            1,
            "the search is a dependency, never an unselected optional root"
        );
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.cap_atomic, 138);
        assert_eq!(
            plan.steps[0]
                .service
                .inputs(&task, plan.steps[0].entity_ref, &catalog.vocabulary)
                .unwrap()["company.name"],
            "Starbucks"
        );
    }
    #[tokio::test]
    async fn optional_global_facets_do_not_erase_or_inherit_original_registry_country_scope() {
        let (mut context, reputation) = fixture();
        let mut snapshot: Snapshot =
            serde_json::from_str(include_str!("../../tests/fixtures/workflows/catalog.json"))
                .unwrap();
        snapshot
            .services
            .extend(reputation.services.iter().cloned());
        snapshot
            .vocabulary
            .extend(reputation.vocabulary.values().cloned());
        snapshot
            .known_sources
            .extend(reputation.known_sources.iter().cloned());
        let catalog = Catalog::build(snapshot);
        let parsed = &mut context.original_intake.parsed_request;
        parsed.subjects[0].label = "Monzo".into();
        parsed.subjects[0].identifiers = BTreeMap::from([
            ("company_name".into(), "Monzo".into()),
            ("country".into(), "United Kingdom".into()),
        ]);
        let cap = &mut parsed.capabilities[0];
        cap.slug = "company.profile".into();
        cap.name = "Registered company details".into();
        cap.description = "Retrieve registered company name status and address".into();
        cap.source_hint = Some("companies-house".into());
        // This recorded fixture declares an opaque whole registry record;
        // field-level/profile aliases are tested by the separate metadata gate.
        cap.outputs = vec!["company.uk_registry_record".into()];
        cap.inputs = vec!["company_name".into(), "country".into()];
        context.original_intake.catalog_version = catalog.version.clone();
        context.original_intake.matches =
            super::super::search::search(&context.original_intake.parsed_request, &catalog);
        context.options = super::super::source_options::options(
            &context.original_intake.parsed_request,
            &context.original_intake.matches,
            &catalog,
        );
        let registry = context
            .options
            .iter()
            .find(|option| {
                option.source.source_slug.as_deref() == Some("companies-house")
                    && option.source.capability.as_deref() == Some("company.registry-profile")
            })
            .unwrap();
        let registry = SelectedSource {
            endpoint_id: registry
                .source
                .endpoint_id
                .as_ref()
                .unwrap()
                .parse()
                .unwrap(),
            capability: registry.source.capability.clone().unwrap(),
            subject_id: registry.subject_id.clone(),
            input: registry.input_values.clone(),
        };
        context.selection.sources = vec![
            registry,
            choose(&context, "crunchbase-search"),
            choose(&context, "glassdoor"),
        ];
        let (task, intent, plan) = compile(&context, &catalog).await;
        assert_eq!(intent.goals.len(), 3);
        assert_eq!(plan.steps.len(), 4, "three roots plus name lookup");
        assert!(context
            .uncovered_inputs(&task, &catalog, &intent, &plan)
            .unwrap()
            .is_empty());
        // Broad model discovery can list global profiles beside the domestic
        // registry. Their explicit inclusion must not make them alternative
        // witnesses of a country-scoped registered record.
        let mut broad = context.clone();
        broad.original_intake.parsed_request.capabilities[0].source_hint = None;
        let global_candidate = broad
            .options
            .iter()
            .find(|option| option.source.capability.as_deref() == Some("company.crunchbase-search"))
            .unwrap()
            .source
            .clone();
        broad.original_intake.matches[0]
            .operations
            .insert(global_candidate.capability.clone().unwrap());
        broad.original_intake.matches[0]
            .apiosk
            .push(global_candidate);
        let expanded = broad.expanded(&catalog).unwrap();
        let mut broad_task = task.clone();
        broad_task.capability_intake = Some(expanded);
        assert!(
            !crate::parser::plan_coverage::uncovered_inputs(&broad_task, &catalog, &intent, &plan)
                .is_empty(),
            "unscoped proof incorrectly makes optional global roots inherit country"
        );
        assert!(
            broad
                .uncovered_inputs(&broad_task, &catalog, &intent, &plan)
                .unwrap()
                .is_empty(),
            "original registry chain must prove country independently from selected global facets"
        );
        // An explicit country filter on the original global retrieval cannot
        // borrow the registry's country from an optional selected facet.
        let global = context
            .options
            .iter()
            .find(|option| option.source.capability.as_deref() == Some("company.crunchbase-search"))
            .unwrap();
        let mut narrowed = context.clone();
        narrowed.original_intake.parsed_request.capabilities = vec![global.requirement.clone()];
        narrowed.original_intake.parsed_request.capabilities[0]
            .inputs
            .push("country".into());
        narrowed.original_intake.matches =
            super::super::search::search(&narrowed.original_intake.parsed_request, &catalog);
        assert!(
            !narrowed
                .uncovered_inputs(&task, &catalog, &intent, &plan)
                .unwrap()
                .is_empty(),
            "global retrieval cannot silently lose explicit country even alongside a UK registry"
        );
        let business = context
            .options
            .iter()
            .find(|option| {
                option
                    .source
                    .capability
                    .as_deref()
                    .is_some_and(|cap| cap.contains("yelp"))
            })
            .unwrap();
        let mut yelp = context.clone();
        yelp.original_intake.parsed_request.capabilities = vec![business.requirement.clone()];
        yelp.original_intake.parsed_request.capabilities[0]
            .inputs
            .push("country".into());
        yelp.original_intake.parsed_request.subjects[0]
            .identifiers
            .insert("city".into(), "London".into());
        yelp.original_intake.matches =
            super::super::search::search(&yelp.original_intake.parsed_request, &catalog);
        yelp.options = super::super::source_options::options(
            &yelp.original_intake.parsed_request,
            &yelp.original_intake.matches,
            &catalog,
        );
        yelp.selection.sources.push(choose(&yelp, "yelp"));
        let expanded = yelp.expanded(&catalog).unwrap();
        let mut yelp_task = Task {
            capability_intake: Some(expanded),
            ..Default::default()
        };
        yelp_task.record_turn(&yelp.question, None);
        let mut yelp_intent = yelp.compile(&catalog).unwrap().unwrap().intent.unwrap();
        crate::engine::bind_subjects(&mut yelp_task, &catalog, &mut yelp_intent).unwrap();
        let crate::resolver::Resolution::Plan(yelp_plan) =
            crate::resolver::resolve(&catalog, &yelp_task, &yelp_intent)
        else {
            panic!("source plan before country proof")
        };
        assert!(
            !yelp
                .uncovered_inputs(&yelp_task, &catalog, &yelp_intent, &yelp_plan)
                .unwrap()
                .is_empty(),
            "Yelp London query must not masquerade as an enforced UK country filter"
        );
    }
    #[tokio::test]
    async fn recorded_uk_selection_keeps_optional_literals_out_of_original_registry_scope() {
        let snapshot: Snapshot = serde_json::from_str(include_str!(
            "../../tests/fixtures/selected-company-sources/catalog.json"
        ))
        .unwrap();
        let catalog = Catalog::build(snapshot);
        assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
        let row: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/selected-company-sources/uk-recorded-selection.json"
        ))
        .unwrap();
        let parsed = serde_json::from_value(row["parsed"].clone()).unwrap();
        let matches = super::super::search::search(&parsed, &catalog);
        let options = super::super::source_options::options(&parsed, &matches, &catalog);
        let context = Context {
            question: row["question"].as_str().unwrap().into(),
            search_id: "recorded-search".into(),
            discovery_id: None,
            original_intake: Intake {
                parsed_request: parsed,
                selected_capabilities: matches
                    .iter()
                    .flat_map(|found| found.operations.iter().cloned())
                    .collect(),
                matches,
                apiosk_sources_searched: 3,
                catalog_version: catalog.version.clone(),
            },
            options,
            selection: Selection {
                version: 1,
                mode: Mode::Include,
                sources: serde_json::from_value(row["selected"].clone()).unwrap(),
            },
        };
        let (task, intent, plan) = compile(&context, &catalog).await;
        assert_eq!(intent.goals.len(), 3);
        assert_eq!(plan.steps.len(), 4);
        assert!(context
            .uncovered_outputs(&task, &catalog, &intent, &plan)
            .unwrap()
            .is_empty());
        assert!(context
            .uncovered_inputs(&task, &catalog, &intent, &plan)
            .unwrap()
            .is_empty());
        // A registry's country proof cannot be combined with an output that
        // only an optional global employer source supplies.
        let mut employer = context.clone();
        employer.original_intake.parsed_request.capabilities[0].outputs =
            vec!["employer_rating".into()];
        let mut unscoped = task.clone();
        unscoped.capability_intake = Some(employer.expanded(&catalog).unwrap());
        assert!(
            crate::parser::plan_coverage::uncovered_outputs(&unscoped, &catalog, &intent, &plan)
                .is_empty(),
            "the optional source has this field, but lacks the original country scope"
        );
        assert!(
            !employer
                .uncovered_outputs(&task, &catalog, &intent, &plan)
                .unwrap()
                .is_empty(),
            "country and requested output must be proved by the same original retrieval chain"
        );
    }

    #[test]
    fn typed_numeric_selection_preserves_equal_original_scalar_and_rejects_changed_value() {
        let (mut context, mut catalog) = fixture();
        let mut choice = choose(&context, "glassdoor");
        let field = catalog
            .services
            .iter()
            .find(|service| service.endpoint_id == choice.endpoint_id)
            .unwrap()
            .contract
            .requires[0]
            .fact_type
            .clone();
        // A schema-reviewed integer input is rendered as a number by the UI;
        // parsed identifiers retain their original string representation.
        catalog.vocabulary.get_mut(&field).unwrap().value_schema =
            json!({"type":"integer","minimum":1,"maximum":10});
        context.original_intake.parsed_request.subjects[0]
            .identifiers
            .insert(field.clone(), "5".into());
        choice.input.insert(field.clone(), json!(5));
        context.selection.sources = vec![choice];
        assert!(context.expanded(&catalog).is_ok());
        context.selection.sources[0]
            .input
            .insert(field.clone(), json!(6));
        assert!(
            context.expanded(&catalog).is_err(),
            "different numeric values remain conflicts"
        );
        context.selection.sources[0].input.insert(field, json!("5"));
        assert!(
            context.expanded(&catalog).is_err(),
            "incoming values must still meet the declared integer schema"
        );
    }
    #[test]
    fn references_inputs_and_original_named_provider_are_authoritative() {
        let (mut context, catalog) = fixture();
        let choice = choose(&context, "glassdoor");
        context.selection.sources = vec![choice.clone()];
        let narrowed = context.expanded(&catalog).unwrap();
        assert!(
            narrowed.matches[0].apiosk.is_empty(),
            "explicit original Crunchbase requirement cannot be replaced by Glassdoor"
        );
        for invalid in [
            "subject",
            "endpoint",
            "capability",
            "input",
            "conflict",
            "duplicate",
            "version",
        ] {
            let mut changed = context.clone();
            match invalid {
                "subject" => changed.selection.sources[0].subject_id = "s2".into(),
                "endpoint" => changed.selection.sources[0].endpoint_id = Uuid::new_v4(),
                "capability" => changed.selection.sources[0].capability = "not.offered".into(),
                "input" => {
                    changed.selection.sources[0]
                        .input
                        .insert("not.declared".into(), json!("anything"));
                }
                "conflict" => {
                    let service = catalog
                        .services
                        .iter()
                        .find(|service| service.endpoint_id == choice.endpoint_id)
                        .unwrap();
                    changed.selection.sources[0].input.insert(
                        service.contract.requires[0].fact_type.clone(),
                        json!("Different Company"),
                    );
                }
                "duplicate" => changed.selection.sources.push(choice.clone()),
                "version" => changed.selection.version = 2,
                _ => unreachable!(),
            }
            assert!(changed.expanded(&catalog).is_err(), "{invalid}");
        }
        assert!(serde_json::from_value::<SelectedSource>(json!({"endpoint_id":choice.endpoint_id,"capability":choice.capability,"subject_id":"s1","outputs":["invented"],"price":1})).is_err());
    }
    #[test]
    fn refreshing_persisted_options_uses_current_contracts_and_preserves_question_scope() {
        let (mut context, catalog) = fixture();
        context.selection.sources = vec![choose(&context, "crunchbase-search")];
        context.original_intake.catalog_version = "old-catalog".into();
        assert!(context.expanded(&catalog).is_err());
        let original = serde_json::to_value(&context.original_intake.parsed_request).unwrap();
        context.refresh(&catalog);
        assert_eq!(
            serde_json::to_value(&context.original_intake.parsed_request).unwrap(),
            original
        );
        assert!(context.expanded(&catalog).is_ok());
        let mut removed = catalog.clone();
        removed
            .services
            .retain(|service| service.endpoint_id != context.selection.sources[0].endpoint_id);
        removed.version = "changed".into();
        context.refresh(&removed);
        assert!(context.expanded(&removed).is_err());
    }
    #[tokio::test]
    async fn equal_company_labels_do_not_merge_separately_selected_location_scopes() {
        let (mut context, catalog) = fixture();
        let template = context
            .options
            .iter()
            .find(|option| {
                option
                    .source
                    .capability
                    .as_deref()
                    .is_some_and(|cap| cap.contains("yelp"))
            })
            .unwrap()
            .clone();
        let mut parsed = context.original_intake.parsed_request.clone();
        parsed.subjects.clear();
        parsed.capabilities.clear();
        for (id, city) in [("s1", "Seattle"), ("s2", "Boston")] {
            let mut subject = context.original_intake.parsed_request.subjects[0].clone();
            subject.id = id.into();
            subject.identifiers.insert("city".into(), city.into());
            let mut cap = template.requirement.clone();
            cap.subject_id = Some(id.into());
            cap.missing.clear();
            parsed.subjects.push(subject);
            parsed.capabilities.push(cap);
        }
        context.original_intake.parsed_request = parsed;
        context.original_intake.matches =
            crate::ask_v2::search::search(&context.original_intake.parsed_request, &catalog);
        context.options = super::super::source_options::options(
            &context.original_intake.parsed_request,
            &context.original_intake.matches,
            &catalog,
        );
        context.selection.sources = context
            .options
            .iter()
            .filter(|option| option.source.endpoint_id == template.source.endpoint_id)
            .map(|option| SelectedSource {
                endpoint_id: option.source.endpoint_id.as_ref().unwrap().parse().unwrap(),
                capability: option.source.capability.clone().unwrap(),
                subject_id: option.subject_id.clone(),
                input: BTreeMap::new(),
            })
            .collect();
        assert_eq!(context.selection.sources.len(), 2);
        context.question = "Compare Starbucks locations in Seattle and Boston.".into();
        use sha2::Digest;
        context.original_intake.parsed_request.meta.text_sha256 =
            hex::encode(sha2::Sha256::digest(context.question.as_bytes()));
        let (task, intent, plan) = compile(&context, &catalog).await;
        assert_eq!(plan.steps.len(), 2);
        let mut erased = intent.clone();
        erased.subjects[1] = erased.subjects[0].clone();
        assert!(context.verify_plan(&task,&catalog,&erased,&plan).is_err(),"Seattle entity cannot stand in for Boston subject merely because both labels are Starbucks");
    }
}
