//! A frozen, ordered recipe. No discovery or alternative-source lookup happens
//! while materializing a run: only literals, approved dates and prior receipts.
use super::{dates, Source};
use crate::{
    catalog::Service,
    resolver::Plan,
    state::{digest, Task},
};
use anyhow::{ensure, Context};
use catalog_core::facts::{
    contract::{InputBinding, Vocabulary},
    schema,
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use uuid::Uuid;

pub const VERSION: u8 = 1;

/// Supplied from owner-scoped execution/result rows, in saved plan order.
/// `plan_step` must be matched by service AND entity, never completion time.
pub struct CompletedStep {
    pub plan_step: usize,
    pub source: Source,
    pub response: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub version: u8,
    pub steps: Vec<RecipeStep>,
    /// The exact schemas used to interpret dependency outputs. Live catalog
    /// vocabulary changes cannot change a previously approved binding.
    pub vocabulary: Vocabulary,
    /// Integrity against accidental edits/reordering. The owner-scoped saved
    /// mandate remains the authorization; this digest is not a bearer token.
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipeStep {
    pub ordinal: usize,
    pub plan_step: usize,
    pub source_selection_id: Uuid,
    pub entity_ref: Uuid,
    pub service: Option<Service>,
    pub external: Option<ExternalSource>,
    pub service_hash: String,
    pub cap_micro_usd: i64,
    pub fixed_inputs: Value,
    pub date_rules: Vec<dates::DateRule>,
    pub bindings: Vec<ResultBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalSource {
    pub resource_url: String,
    pub method: String,
    pub provider_price_micro_usd: i64,
    pub input_contract: super::external_inputs::Contract,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultBinding {
    pub from_step: usize,
    pub fact_type: String,
    pub input: InputBinding,
}

/// Receipt of this occurrence, mapped to its original recipe source. Callers
/// must query by run_id; a receipt from a previous occurrence is not progress.
pub struct StepResult {
    pub ordinal: usize,
    pub source_selection_id: Uuid,
    pub body: Value,
}

impl Recipe {
    pub fn compile(
        plan: &Plan,
        task: &Task,
        completed: &[CompletedStep],
        vocabulary: &Vocabulary,
    ) -> anyhow::Result<Self> {
        let primary: Vec<_> = plan
            .steps
            .iter()
            .enumerate()
            .filter(|(_, step)| step.browser_fallback.is_none())
            .collect();
        ensure!(
            !primary.is_empty() && completed.len() == primary.len(),
            "Every primary step needs one completed receipt."
        );
        let mut steps = Vec::new();
        let mut prior = Vec::new();
        for (ordinal, ((plan_step, step), done)) in primary.iter().zip(completed).enumerate() {
            ensure!(
                *plan_step == done.plan_step,
                "Completed sources do not match the saved plan order."
            );
            let source = &done.source;
            let saved = source
                .service
                .as_ref()
                .context("A frozen recipe requires a reviewed source contract.")?;
            ensure!(
                saved.version_hash() == step.service.version_hash(),
                "The saved source contract changed."
            );
            let selection = &source.selection;
            let selection_id: Uuid = selection["id"]
                .as_str()
                .context("Missing saved source ID.")?
                .parse()?;
            ensure!(
                selection["candidate_id"] == step.service.endpoint_id.to_string()
                    && selection["capability"] == step.service.contract.capability_slug
                    && selection["api_slug"] == step.service.api_slug
                    && selection["method"] == step.service.method
                    && selection["provider_price_micro_usd"].as_i64()
                        == Some(step.service.list_atomic)
                    && selection["charged_micro_usd"].as_i64() == Some(plan.step_total(step)),
                "The completed selection does not match the exact approved source and price."
            );
            successful(&done.response)?;
            let original = selection["metadata"]["inputs"].clone();
            ensure!(
                original.is_object(),
                "The completed input object is missing."
            );
            let mut frozen = RecipeStep {
                ordinal,
                plan_step: *plan_step,
                source_selection_id: selection_id,
                entity_ref: step.entity_ref,
                service: Some(saved.clone()),
                external: None,
                service_hash: saved.version_hash(),
                cap_micro_usd: plan.step_total(step),
                fixed_inputs: original.clone(),
                date_rules: source.date_rules.clone(),
                bindings: Vec::new(),
            };
            for requirement in &saved.contract.requires {
                ensure!(
                    requirement.subject_role == "subject",
                    "Unsupported multi-role recurring binding."
                );
                let input = saved
                    .contract
                    .input_map
                    .get(&requirement.fact_type)
                    .context("The required input has no reviewed binding.")?;
                let path = input_pointer(input);
                let Some(wire_value) = original.pointer(&path) else {
                    ensure!(requirement.optional, "A required saved input is missing.");
                    continue;
                };
                let matches_wire = |value: &Value| -> bool {
                    encoded_value(saved, input, value.clone()).is_ok_and(|v| v == *wire_value)
                };
                let facts: Vec<_> = task
                    .facts
                    .iter()
                    .filter(|fact| {
                        fact.entity_ref == step.entity_ref
                            && fact.semantic_type == requirement.fact_type
                            && matches_wire(&fact.value)
                    })
                    .collect();
                // A literal the person supplied remains pinned even when a
                // preceding profile happens to return the same identifier.
                if facts.iter().any(|fact| {
                    matches!(
                        fact.provenance["kind"].as_str(),
                        Some("user" | "user_input")
                    )
                }) {
                    continue;
                }
                let producers: Vec<_> = steps
                    .iter()
                    .enumerate()
                    .filter(|(_, producer): &(usize, &RecipeStep)| {
                        producer.entity_ref == step.entity_ref
                            && producer.service.as_ref().is_some_and(|s| {
                                s.contract
                                    .output_map
                                    .produces
                                    .contains_key(&requirement.fact_type)
                            })
                    })
                    .filter_map(|(index, producer)| {
                        let value = extracted_value(
                            producer,
                            &prior[index],
                            &requirement.fact_type,
                            vocabulary,
                        )
                        .ok()?;
                        matches_wire(&value).then_some(index)
                    })
                    .collect();
                match producers.as_slice() {
                    [from_step] => {
                        remove_input(&mut frozen.fixed_inputs, &path)?;
                        frozen.bindings.push(ResultBinding {
                            from_step: *from_step, fact_type: requirement.fact_type.clone(), input: input.clone(),
                        });
                    }
                    [] => {
                        // An earlier source declares the fact but no unique
                        // valid result supports this exact input: never freeze
                        // the old selection to hide an unresolved dependency.
                        let earlier_declares = steps.iter().any(|producer: &RecipeStep|
                            producer.entity_ref == step.entity_ref
                                && producer.service.as_ref().is_some_and(|s| s.contract.output_map.produces.contains_key(&requirement.fact_type)));
                        ensure!(!earlier_declares && !facts.iter().any(|fact| fact.provenance["kind"] == "provider_response"),
                            "A dynamic input has no unique source in this recipe; review the original dependency.");
                    }
                    _ => anyhow::bail!("Several earlier sources could bind this input; an explicit source binding is required."),
                }
            }
            validate_dates(&frozen)?;
            ensure!(
                bind_inputs(&frozen, &steps, &prior, vocabulary)? == original,
                "The frozen bindings do not reproduce the purchased inputs."
            );
            steps.push(frozen);
            prior.push(StepResult {
                ordinal,
                source_selection_id: selection_id,
                body: done.response.clone(),
            });
        }
        let frozen_vocabulary = dependency_fact_types(&steps)?
            .into_iter()
            .map(|slug| {
                let definition = vocabulary
                    .get(&slug)
                    .context("Missing dependency fact schema.")?;
                Ok((slug, definition.clone()))
            })
            .collect::<anyhow::Result<Vocabulary>>()?;
        let mut recipe = Self {
            version: VERSION,
            steps,
            vocabulary: frozen_vocabulary,
            hash: String::new(),
        };
        recipe.hash = recipe.digest();
        recipe.validate()?;
        Ok(recipe)
    }

    /// One explicit standalone purchase. The caller must establish that it is
    /// a direct purchase, not one node cut out of a plan. Independent recipes
    /// may later be combined without inventing output-to-input relationships.
    pub fn fixed(sources: &[Source]) -> anyhow::Result<Self> {
        ensure!(
            sources.len() == 1,
            "Only a standalone purchase can use fixed replay."
        );
        let source = &sources[0];
        let selection = &source.selection;
        let (external, service_hash) = if let Some(service) = &source.service {
            let fee = selection["metadata"]["fee_bps"].as_i64().unwrap_or(1500);
            ensure!(
                selection["candidate_id"] == service.endpoint_id.to_string()
                    && selection["api_slug"] == service.api_slug
                    && selection["capability"] == service.contract.capability_slug
                    && selection["method"] == service.method
                    && selection["provider_price_micro_usd"].as_i64() == Some(service.list_atomic)
                    && selection["charged_micro_usd"].as_i64() == Some(service.total_with(fee)),
                "The direct selection does not match its exact source contract."
            );
            (None, service.version_hash())
        } else {
            let external = ExternalSource {
                resource_url: selection["resource_url"]
                    .as_str()
                    .context("Missing external source URL.")?
                    .into(),
                method: selection["method"]
                    .as_str()
                    .context("Missing external source method.")?
                    .into(),
                provider_price_micro_usd: selection["provider_price_micro_usd"]
                    .as_i64()
                    .context("Missing external source price.")?,
                input_contract: serde_json::from_value(selection["metadata"]["recurring_input_contract"].clone())
                    .context("The original external input destinations are unavailable. Review a new purchase.")?,
            };
            validate_external(&external)?;
            external
                .input_contract
                .materialize(&selection["metadata"]["inputs"])?;
            let hash = digest(&external);
            (Some(external), hash)
        };
        let step = RecipeStep {
            ordinal: 0,
            plan_step: 0,
            source_selection_id: selection["id"]
                .as_str()
                .context("Missing saved source ID.")?
                .parse()?,
            entity_ref: Uuid::nil(),
            service: source.service.clone(),
            service_hash,
            external,
            cap_micro_usd: selection["charged_micro_usd"]
                .as_i64()
                .context("Missing external source ceiling.")?,
            fixed_inputs: selection["metadata"]["inputs"].clone(),
            date_rules: source.date_rules.clone(),
            bindings: vec![],
        };
        let mut recipe = Self {
            version: VERSION,
            steps: vec![step],
            vocabulary: Vocabulary::new(),
            hash: String::new(),
        };
        recipe.hash = recipe.digest();
        recipe.validate()?;
        Ok(recipe)
    }

    /// Preserve the selected tasks' order without merging or substituting any
    /// of their calls. A later task keeps its own already-proven bindings.
    pub fn combine(recipes: &[Recipe]) -> anyhow::Result<Self> {
        ensure!(!recipes.is_empty(), "Choose at least one completed recipe.");
        let mut steps = Vec::new();
        let mut vocabulary = Vocabulary::new();
        let mut plan_offset = 0;
        for recipe in recipes {
            recipe.validate()?;
            for (slug, definition) in &recipe.vocabulary {
                ensure!(
                    vocabulary.get(slug).is_none_or(|saved| saved == definition),
                    "Selected recipes use different versions of the same fact schema."
                );
                vocabulary.insert(slug.clone(), definition.clone());
            }
            let offset = steps.len();
            for original in &recipe.steps {
                let mut step = original.clone();
                step.ordinal += offset;
                step.plan_step += plan_offset;
                for binding in &mut step.bindings {
                    binding.from_step += offset;
                }
                steps.push(step);
            }
            plan_offset += recipe
                .steps
                .last()
                .map(|step| step.plan_step + 1)
                .unwrap_or(0);
        }
        let mut recipe = Self {
            version: VERSION,
            steps,
            vocabulary,
            hash: String::new(),
        };
        recipe.hash = recipe.digest();
        recipe.validate()?;
        Ok(recipe)
    }

    fn digest(&self) -> String {
        digest(&(self.version, &self.steps, &self.vocabulary))
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.version == VERSION && !self.steps.is_empty() && self.steps.len() <= 100,
            "Unsupported recurring recipe version or size."
        );
        ensure!(
            self.hash == self.digest(),
            "The approved recurring recipe changed."
        );
        ensure!(
            dependency_fact_types(&self.steps)? == self.vocabulary.keys().cloned().collect(),
            "The frozen dependency schemas are incomplete."
        );
        for (slug, definition) in &self.vocabulary {
            ensure!(definition.slug == *slug, "A frozen fact identity changed.");
            schema::check_schema(&definition.value_schema)?;
        }
        let mut selections = BTreeSet::new();
        for (index, step) in self.steps.iter().enumerate() {
            ensure!(
                step.ordinal == index
                    && selections.insert(step.source_selection_id)
                    && (index == 0 || step.plan_step > self.steps[index - 1].plan_step),
                "The approved recurring source order changed."
            );
            let provider_price = match (&step.service, &step.external) {
                (Some(service), None) => {
                    ensure!(
                        step.service_hash == service.version_hash(),
                        "The frozen source changed."
                    );
                    service.list_atomic
                }
                (None, Some(external)) => {
                    validate_external(external)?;
                    ensure!(
                        step.bindings.is_empty() && step.service_hash == digest(external),
                        "An external source cannot form an untyped dependency chain."
                    );
                    external.provider_price_micro_usd
                }
                _ => anyhow::bail!("Missing or conflicting source identity."),
            };
            ensure!(
                step.cap_micro_usd >= provider_price
                    && step.cap_micro_usd > 0
                    && step.fixed_inputs.is_object(),
                "The frozen source changed."
            );
            let mut inputs = BTreeSet::new();
            for binding in &step.bindings {
                let source = self
                    .steps
                    .get(binding.from_step)
                    .context("Missing dependency source.")?;
                let service = step
                    .service
                    .as_ref()
                    .context("Missing dependency consumer contract.")?;
                ensure!(
                    binding.from_step < index
                        && source.entity_ref == step.entity_ref
                        && source.service.as_ref().is_some_and(|s| s
                            .contract
                            .output_map
                            .produces
                            .contains_key(&binding.fact_type))
                        && service.contract.input_map.get(&binding.fact_type)
                            == Some(&binding.input)
                        && service.contract.requires.iter().any(|r| r.fact_type
                            == binding.fact_type
                            && r.subject_role == "subject")
                        && inputs.insert(input_pointer(&binding.input))
                        && step
                            .fixed_inputs
                            .pointer(&input_pointer(&binding.input))
                            .is_none(),
                    "Invalid frozen input dependency."
                );
            }
            validate_dates(step)?;
        }
        Ok(())
    }

    /// Price/contract drift pauses the same recipe; it never picks a fallback.
    pub fn validate_catalog(&self, services: &[Service]) -> anyhow::Result<()> {
        self.validate()?;
        for step in &self.steps {
            if let Some(saved) = &step.service {
                ensure!(services.iter().any(|service| service.id == saved.id
                    && service.version_hash() == step.service_hash),
                    "A frozen source contract or price changed. Review the order before continuing.");
            }
        }
        Ok(())
    }

    /// Strict A then B: every preceding receipt must belong to this recipe and
    /// be successful, even if the next source has no data dependency on it.
    pub fn materialize(
        &self,
        ordinal: usize,
        prior: &[StepResult],
        scheduled: DateTime<Utc>,
        local_date: NaiveDate,
        _vocabulary: &Vocabulary,
    ) -> anyhow::Result<Value> {
        self.validate()?;
        let step = self.steps.get(ordinal).context("Unknown recurring step.")?;
        ensure!(
            prior.len() == ordinal,
            "Complete each source in the approved order."
        );
        for (index, receipt) in prior.iter().enumerate() {
            ensure!(
                receipt.ordinal == index
                    && receipt.source_selection_id == self.steps[index].source_selection_id,
                "The result belongs to another source or order."
            );
            successful(&receipt.body)?;
        }
        let inputs = bind_inputs(step, &self.steps, prior, &self.vocabulary)?;
        dates::materialize(&inputs, &step.date_rules, scheduled, local_date)
    }

    /// Rebuild the exact HTTP locations after this occurrence's approved date
    /// rules have changed the flat values. Never infer locations from method.
    pub fn input_parts(&self, ordinal: usize, inputs: &Value) -> anyhow::Result<Option<Value>> {
        self.validate()?;
        let step = self.steps.get(ordinal).context("Unknown recurring step.")?;
        step.external
            .as_ref()
            .map(|external| external.input_contract.materialize(inputs))
            .transpose()
    }
}

/// Extraction inspects every declared output, not just the bound fact. Freeze
/// those schemas and the producer's input schemas as one reviewed contract.
fn dependency_fact_types(steps: &[RecipeStep]) -> anyhow::Result<BTreeSet<String>> {
    let mut types = BTreeSet::new();
    for step in steps {
        for binding in &step.bindings {
            let contract = &steps
                .get(binding.from_step)
                .and_then(|producer| producer.service.as_ref())
                .context("Missing dependency source contract.")?
                .contract;
            types.extend(contract.output_map.produces.keys().cloned());
            types.extend(
                contract
                    .requires
                    .iter()
                    .map(|required| required.fact_type.clone()),
            );
        }
    }
    Ok(types)
}

fn successful(body: &Value) -> anyhow::Result<()> {
    ensure!(
        body["ok"] == true && body.get("result").is_some() && body["result_truncated"] != true,
        "A preceding source has no complete successful result."
    );
    Ok(())
}

fn input_pointer(input: &InputBinding) -> String {
    if input.name.starts_with('/') {
        input.name.clone()
    } else {
        format!("/{}", input.name.replace('~', "~0").replace('/', "~1"))
    }
}

fn encoded_value(service: &Service, input: &InputBinding, value: Value) -> anyhow::Result<Value> {
    let mut encoded = service
        .fixed_params
        .as_object()
        .cloned()
        .unwrap_or_default();
    crate::input_binding::insert(&mut encoded, input, value)?;
    Value::Object(encoded)
        .pointer(&input_pointer(input))
        .cloned()
        .context("Missing encoded input.")
}

fn remove_input(inputs: &mut Value, pointer: &str) -> anyhow::Result<()> {
    let (parent, leaf) = pointer.rsplit_once('/').context("Invalid input path.")?;
    let key = leaf.replace("~1", "/").replace("~0", "~");
    let object = inputs
        .pointer_mut(parent)
        .and_then(Value::as_object_mut)
        .context("Input parent is not an object.")?;
    ensure!(object.remove(&key).is_some(), "Dynamic input is missing.");
    Ok(())
}

fn extracted_value(
    step: &RecipeStep,
    result: &StepResult,
    fact_type: &str,
    vocabulary: &Vocabulary,
) -> anyhow::Result<Value> {
    ensure!(
        result.ordinal == step.ordinal && result.source_selection_id == step.source_selection_id,
        "The result does not match the approved dependency."
    );
    successful(&result.body)?;
    let extraction = step
        .service
        .as_ref()
        .context("Missing dependency contract.")?
        .contract
        .extract(&result.body["result"], vocabulary)?;
    ensure!(
        extraction.rejected.is_empty(),
        "The dependency result failed its schema."
    );
    let subject = extraction
        .single()
        .context("The dependency returned no unique subject. Review the order.")?;
    let value = subject
        .fact(fact_type)
        .context("The dependency did not return the required fact.")?
        .value
        .clone();
    schema::validate(
        &vocabulary
            .get(fact_type)
            .context("Unknown required fact.")?
            .value_schema,
        &value,
    )?;
    Ok(value)
}

fn validate_external(source: &ExternalSource) -> anyhow::Result<()> {
    let url = url::Url::parse(&source.resource_url)?;
    ensure!(
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && matches!(source.method.as_str(), "GET" | "POST")
            && source.provider_price_micro_usd >= 0,
        "Invalid frozen external source identity."
    );
    Ok(())
}

fn bind_inputs(
    step: &RecipeStep,
    steps: &[RecipeStep],
    prior: &[StepResult],
    vocabulary: &Vocabulary,
) -> anyhow::Result<Value> {
    let mut inputs = step
        .fixed_inputs
        .as_object()
        .context("Missing fixed input template.")?
        .clone();
    for binding in &step.bindings {
        let producer = steps
            .get(binding.from_step)
            .context("Missing dependency step.")?;
        let receipt = prior
            .get(binding.from_step)
            .context("The required source has not completed.")?;
        let value = extracted_value(producer, receipt, &binding.fact_type, vocabulary)?;
        crate::input_binding::insert(&mut inputs, &binding.input, value)?;
    }
    Ok(Value::Object(inputs))
}

fn validate_dates(step: &RecipeStep) -> anyhow::Result<()> {
    let mut seen = BTreeSet::new();
    for rule in &step.date_rules {
        ensure!(
            seen.insert(&rule.pointer)
                && step
                    .fixed_inputs
                    .pointer(&rule.pointer)
                    .and_then(Value::as_str)
                    == Some(rule.original.as_str())
                && (-31_622_400..=31_622_400).contains(&rule.offset_seconds)
                && rule.offset_seconds % if rule.date_only { 86400 } else { 60 } == 0,
            "An approved date rule no longer matches a fixed input."
        );
        ensure!(
            step.bindings.iter().all(|binding| {
                let path = input_pointer(&binding.input);
                rule.pointer != path
                    && !rule.pointer.starts_with(&format!("{path}/"))
                    && !path.starts_with(&format!("{}/", rule.pointer))
            }),
            "A date rule cannot overwrite a dynamic dependency."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{resolver::Step, state::Fact};
    use catalog_core::facts::contract::FactType;
    use serde_json::json;

    fn fixture() -> (Plan, Task, Vec<CompletedStep>, Vocabulary) {
        let entity = Uuid::new_v4();
        let search: Service = serde_json::from_value(json!({
            "id":Uuid::new_v4(),"endpoint_id":Uuid::new_v4(),"api_slug":"registry","name":"Registry search",
            "method":"GET","path":"/search","listing_type":"commercial","list_atomic":100,
            "fixed_params":{},"preconditions":{},"pagination":{},"freshness_seconds":3600,
            "sample_payload":null,"jurisdiction":null,"source_url":null,
            "contract":{"capability_slug":"company.search","version":1,"cardinality":"many",
                "requires":[{"fact_type":"company.name"}],"input_map":{"company.name":{"name":"name"}},
                "output_map":{"base":"$.items[*]","produces":{"company.id":{"path":"$.id"}}},"identifies":"company.id"}
        })).unwrap();
        let mut detail = search.clone();
        detail.id = Uuid::new_v4();
        detail.endpoint_id = Uuid::new_v4();
        detail.path = "/detail".into();
        detail.contract = serde_json::from_value(json!({
            "capability_slug":"company.detail","version":1,"cardinality":"one",
            "requires":[{"fact_type":"company.id"}],"input_map":{"company.id":{"name":"id"}},
            "output_map":{"base":"$","produces":{"company.detail":{"path":"$.detail"}}}
        }))
        .unwrap();
        let plan = Plan {
            quote_ref: Uuid::new_v4(),
            steps: vec![search.clone(), detail.clone()]
                .into_iter()
                .map(|service| Step {
                    service,
                    entity_ref: entity,
                    goal_index: 0,
                    browser_fallback: None,
                })
                .collect(),
            cap_atomic: search.total() + detail.total(),
            expires_at: Utc::now(),
            hash: "original-plan".into(),
            fee_bps: 1500,
            workflow_slug: None,
        };
        let fact = |kind: &str, value: Value, provenance: Value| Fact {
            entity_ref: entity,
            semantic_type: kind.into(),
            value,
            provenance,
            observed_at: Utc::now(),
            // Old facts may expire. Compilation uses historical provenance,
            // never pretends their values are current-run source evidence.
            expires_at: Utc::now() - chrono::Duration::hours(1),
            verification: "fixture".into(),
        };
        let task = Task {
            facts: vec![
                fact("company.name", json!("Acme"), json!({"kind":"user_input"})),
                fact(
                    "company.id",
                    json!("old-id"),
                    json!({"kind":"provider_response","contract_id":search.id}),
                ),
            ],
            ..Task::default()
        };
        let completed = plan.steps.iter().enumerate().map(|(index,step)| CompletedStep {
            plan_step:index,
            source:Source {selection:json!({
                "id":Uuid::new_v4(),"candidate_id":step.service.endpoint_id,"capability":step.service.contract.capability_slug,
                "api_slug":step.service.api_slug,"method":step.service.method,"provider_price_micro_usd":step.service.list_atomic,
                "charged_micro_usd":plan.step_total(step),"metadata":{"inputs":if index==0 {json!({"name":"Acme"})}else{json!({"id":"old-id"})}}
            }),service:Some(step.service.clone()),date_rules:vec![]},
            response:if index==0 {json!({"ok":true,"result":{"items":[{"id":"old-id"}]}})}else{json!({"ok":true,"result":{"detail":"original"}})},
        }).collect();
        let vocabulary = ["company.name", "company.id", "company.detail"]
            .into_iter()
            .map(|slug| {
                (
                    slug.into(),
                    FactType {
                        slug: slug.into(),
                        value_schema: json!({"type":"string","minLength":1}),
                    },
                )
            })
            .collect();
        (plan, task, completed, vocabulary)
    }

    fn receipt(recipe: &Recipe, ordinal: usize, body: Value) -> StepResult {
        StepResult {
            ordinal,
            source_selection_id: recipe.steps[ordinal].source_selection_id,
            body,
        }
    }
    fn materialize(
        recipe: &Recipe,
        ordinal: usize,
        results: &[StepResult],
        vocabulary: &Vocabulary,
    ) -> anyhow::Result<Value> {
        recipe.materialize(
            ordinal,
            results,
            "2026-10-12T08:00:00Z".parse().unwrap(),
            "2026-10-12".parse().unwrap(),
            vocabulary,
        )
    }

    #[test]
    fn repeats_exact_order_and_uses_new_upstream_identifier() {
        let (plan, task, completed, vocab) = fixture();
        let recipe = Recipe::compile(&plan, &task, &completed, &vocab).unwrap();
        assert_eq!(recipe.steps[1].fixed_inputs, json!({}));
        assert_eq!(recipe.steps[1].bindings[0].from_step, 0);
        assert_eq!(
            materialize(&recipe, 0, &[], &vocab).unwrap(),
            json!({"name":"Acme"})
        );
        assert!(materialize(&recipe, 1, &[], &vocab).is_err());
        let results = [receipt(
            &recipe,
            0,
            json!({"ok":true,"result":{"items":[{"id":"new-id"}]}}),
        )];
        assert_eq!(
            materialize(&recipe, 1, &results, &vocab).unwrap(),
            json!({"id":"new-id"})
        );
    }

    #[test]
    fn dependency_schemas_remain_frozen_when_live_vocabulary_changes() {
        let (plan, task, completed, mut vocab) = fixture();
        let recipe = Recipe::compile(&plan, &task, &completed, &vocab).unwrap();
        assert_eq!(recipe.vocabulary.len(), 2);
        assert!(!recipe.vocabulary.contains_key("company.detail"));
        vocab.get_mut("company.id").unwrap().value_schema = json!({"type":"integer"});
        assert_eq!(
            materialize(
                &recipe,
                1,
                &[receipt(
                    &recipe,
                    0,
                    json!({"ok":true,"result":{"items":[{"id":"still-a-string"}]}})
                )],
                &vocab
            )
            .unwrap(),
            json!({"id":"still-a-string"})
        );
        assert!(materialize(
            &recipe,
            1,
            &[receipt(
                &recipe,
                0,
                json!({"ok":true,"result":{"items":[{"id":123}]}})
            )],
            &vocab
        )
        .is_err());
        let mut tampered = recipe.clone();
        tampered.vocabulary = vocab;
        assert!(tampered.validate().is_err());
        let mut conflict = recipe.clone();
        conflict
            .steps
            .iter_mut()
            .for_each(|step| step.source_selection_id = Uuid::new_v4());
        conflict
            .vocabulary
            .get_mut("company.id")
            .unwrap()
            .value_schema = json!({"type":"string","minLength":3});
        conflict.hash = conflict.digest();
        assert!(Recipe::combine(&[recipe, conflict]).is_err());
    }

    #[test]
    fn rejects_ambiguous_missing_failed_truncated_and_invalid_dependency_results() {
        let (plan, task, completed, vocab) = fixture();
        let recipe = Recipe::compile(&plan, &task, &completed, &vocab).unwrap();
        for body in [
            json!({"ok":true,"result":{"items":[{"id":"a"},{"id":"b"}]}}),
            json!({"ok":true,"result":{"items":[]}}),
            json!({"ok":false,"result":{"items":[{"id":"a"}]}}),
            json!({"ok":true,"result_truncated":true,"result":{"items":[{"id":"a"}]}}),
            json!({"ok":true,"result":{"items":[{"id":123}]}}),
        ] {
            assert!(materialize(&recipe, 1, &[receipt(&recipe, 0, body)], &vocab).is_err());
        }
    }

    #[test]
    fn explicit_literal_identifier_is_pinned() {
        let (plan, mut task, completed, vocab) = fixture();
        task.facts[1].provenance = json!({"kind":"user_input"});
        let recipe = Recipe::compile(&plan, &task, &completed, &vocab).unwrap();
        assert!(recipe.steps[1].bindings.is_empty());
        let results = [receipt(
            &recipe,
            0,
            json!({"ok":true,"result":{"items":[{"id":"different"}]}}),
        )];
        assert_eq!(
            materialize(&recipe, 1, &results, &vocab).unwrap(),
            json!({"id":"old-id"})
        );
    }

    #[test]
    fn rejects_reordered_tampered_and_substituted_sources() {
        let (plan, task, mut completed, vocab) = fixture();
        let recipe = Recipe::compile(&plan, &task, &completed, &vocab).unwrap();
        let mut reordered = recipe.clone();
        reordered.steps.swap(0, 1);
        assert!(reordered.validate().is_err());
        let mut changed = recipe.clone();
        changed.steps[0].fixed_inputs = json!({"name":"Other"});
        assert!(changed.validate().is_err());
        let mut catalog: Vec<_> = plan.steps.iter().map(|step| step.service.clone()).collect();
        recipe.validate_catalog(&catalog).unwrap();
        catalog[1].list_atomic += 1;
        assert!(recipe.validate_catalog(&catalog).is_err());
        completed.swap(0, 1);
        assert!(Recipe::compile(&plan, &task, &completed, &vocab).is_err());
        completed.swap(0, 1);
        completed[1].source.selection["candidate_id"] = json!(Uuid::new_v4());
        assert!(Recipe::compile(&plan, &task, &completed, &vocab).is_err());
    }

    #[test]
    fn rejects_old_or_wrong_source_receipts_and_missing_hidden_producer() {
        let (mut plan, task, mut completed, vocab) = fixture();
        let recipe = Recipe::compile(&plan, &task, &completed, &vocab).unwrap();
        let mut wrong = receipt(&recipe, 0, completed[0].response.clone());
        wrong.source_selection_id = Uuid::new_v4();
        assert!(materialize(&recipe, 1, &[wrong], &vocab).is_err());
        plan.steps.remove(0);
        completed.remove(0);
        completed[0].plan_step = 0;
        assert!(Recipe::compile(&plan, &task, &completed, &vocab).is_err());
    }

    #[test]
    fn dates_change_only_approved_fixed_fields() {
        let (plan, task, mut completed, vocab) = fixture();
        completed[1].source.selection["metadata"]["inputs"]["date"] = json!("2026-10-09");
        completed[1].source.date_rules = vec![dates::DateRule {
            pointer: "/date".into(),
            original: "2026-10-09".into(),
            offset_seconds: -86400,
            date_only: true,
        }];
        let recipe = Recipe::compile(&plan, &task, &completed, &vocab).unwrap();
        let results = [receipt(
            &recipe,
            0,
            json!({"ok":true,"result":{"items":[{"id":"new"}]}}),
        )];
        assert_eq!(
            materialize(&recipe, 1, &results, &vocab).unwrap(),
            json!({"id":"new","date":"2026-10-11"})
        );
        completed[1].source.date_rules[0].pointer = "/id".into();
        assert!(Recipe::compile(&plan, &task, &completed, &vocab).is_err());
    }

    #[test]
    fn combined_recipes_preserve_each_binding_and_all_preceding_steps() {
        let (plan, task, completed, vocab) = fixture();
        let a = Recipe::compile(&plan, &task, &completed, &vocab).unwrap();
        let (plan, task, completed, _) = fixture();
        let b = Recipe::compile(&plan, &task, &completed, &vocab).unwrap();
        let combined = Recipe::combine(&[a, b]).unwrap();
        assert_eq!(combined.steps[3].bindings[0].from_step, 2);
        assert!(materialize(&combined, 2, &[], &vocab).is_err());
        let prior = vec![
            receipt(
                &combined,
                0,
                json!({"ok":true,"result":{"items":[{"id":"a"}]}}),
            ),
            receipt(&combined, 1, json!({"ok":true,"result":{"detail":"a"}})),
            receipt(
                &combined,
                2,
                json!({"ok":true,"result":{"items":[{"id":"b"}]}}),
            ),
        ];
        assert_eq!(
            materialize(&combined, 3, &prior, &vocab).unwrap(),
            json!({"id":"b"})
        );
    }

    #[test]
    fn external_fixed_replay_pins_url_method_and_price_and_rejects_untyped_chains() {
        let source = Source {
            selection: json!({"id":Uuid::new_v4(),"resource_url":"https://example.com/data",
            "method":"GET","provider_price_micro_usd":100,"charged_micro_usd":115,"metadata":{"inputs":{"query":"fixed"},"recurring_input_contract":external_contract("query")}}),
            service: None,
            date_rules: vec![],
        };
        let recipe = Recipe::fixed(&[source.clone()]).unwrap();
        assert_eq!(
            materialize(&recipe, 0, &[], &Vocabulary::new()).unwrap(),
            json!({"query":"fixed"})
        );
        assert!(Recipe::fixed(&[source.clone(), source]).is_err());
        let mut changed = recipe.clone();
        changed.steps[0].external.as_mut().unwrap().resource_url =
            "https://other.example/data".into();
        assert!(changed.validate().is_err());
        let mut changed = recipe.clone();
        changed.steps[0]
            .external
            .as_mut()
            .unwrap()
            .input_contract
            .fields[0]
            .location = "body".into();
        assert!(changed.validate().is_err());
        assert_eq!(
            recipe.input_parts(0, &json!({"query":"fixed"})).unwrap(),
            Some(json!({"path":{},"query":{"query":"fixed"},"body":{}}))
        );
    }

    fn external_contract(location: &str) -> Value {
        json!({"fields":[{"field":"query","name":"query","location":location,"required":true,"schema":{"type":"string"}}],"schema":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false}})
    }

    #[test]
    fn independent_direct_purchases_combine_without_inventing_bindings() {
        let (_, _, completed, vocab) = fixture();
        let reviewed = Recipe::fixed(&[completed[1].source.clone()]).unwrap();
        let external = Source {
            selection: json!({"id":Uuid::new_v4(),"resource_url":"https://example.com/data",
                "method":"POST","provider_price_micro_usd":100,"charged_micro_usd":115,
                "metadata":{"inputs":{"query":"fixed"},"recurring_input_contract":external_contract("body")}}),
            service: None,
            date_rules: vec![],
        };
        let external = Recipe::fixed(&[external]).unwrap();
        let combined = Recipe::combine(&[external, reviewed]).unwrap();
        assert!(combined.steps.iter().all(|step| step.bindings.is_empty()));
        let first = receipt(
            &combined,
            0,
            json!({"ok":true,"result":{"id":"must-not-bind"}}),
        );
        assert_eq!(
            materialize(&combined, 1, &[first], &vocab).unwrap(),
            json!({"id":"old-id"})
        );
    }

    #[test]
    fn nested_array_binding_uses_existing_reviewed_wrapper() {
        let (mut plan, task, mut completed, vocab) = fixture();
        let binding: InputBinding =
            serde_json::from_value(json!({"in":"body","name":"/query/ids","wrap":"array"}))
                .unwrap();
        plan.steps[1].service.fixed_params = json!({"query":{"status":"active"}});
        plan.steps[1]
            .service
            .contract
            .input_map
            .insert("company.id".into(), binding);
        completed[1].source.service = Some(plan.steps[1].service.clone());
        completed[1].source.selection["metadata"]["inputs"] =
            json!({"query":{"status":"active","ids":["old-id"]}});
        let recipe = Recipe::compile(&plan, &task, &completed, &vocab).unwrap();
        let results = [receipt(
            &recipe,
            0,
            json!({"ok":true,"result":{"items":[{"id":"new-id"}]}}),
        )];
        assert_eq!(
            materialize(&recipe, 1, &results, &vocab).unwrap(),
            json!({"query":{"status":"active","ids":["new-id"]}})
        );
    }
}
