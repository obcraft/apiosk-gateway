//! Request-local semantic links to admitted source fields. The model may match
//! wording and describe a derivation; code owns the field inventory, endpoint,
//! subject, request and catalog-version checks. Samples prove optional field
//! availability only and are never used as the answer to the current question.
use crate::{
    capability_intake::Intake,
    catalog::{Catalog, Service},
    state::digest,
};
use anyhow::ensure;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub requirement: usize,
    pub output: String,
    pub fields: Vec<String>,
    pub interpretation: String,
    pub limitation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grounding {
    request: String,
    catalog: String,
    pub bindings: Vec<Binding>,
}

impl Grounding {
    pub fn new(intake: &Intake, catalog: &Catalog, bindings: Vec<Binding>) -> Option<Self> {
        validate(&bindings, intake, catalog).ok()?;
        (!bindings.is_empty()).then(|| Self {
            request: digest(&intake.parsed_request),
            catalog: catalog.version.clone(),
            bindings,
        })
    }
    pub fn bindings_for(&self, intake: &Intake, catalog: &Catalog) -> &[Binding] {
        if self.request == digest(&intake.parsed_request) && self.catalog == catalog.version {
            &self.bindings
        } else {
            &[]
        }
    }
    pub fn notices(&self) -> Vec<String> {
        let mut notices = Vec::new();
        for notice in self
            .bindings
            .iter()
            .filter_map(|binding| binding.limitation.as_ref())
        {
            if !notices.contains(notice) {
                notices.push(notice.clone());
            }
        }
        notices
    }
}

#[derive(Clone, Serialize)]
struct Field {
    id: String,
    service: Uuid,
    fact: String,
    path: String,
    types: BTreeSet<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    aliases: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    observed: bool,
}

fn child(path: &str, name: &str) -> String {
    format!("{path}/{}", name.replace('~', "~0").replace('/', "~1"))
}

fn inventory(service: &Service, catalog: &Catalog) -> BTreeMap<String, Field> {
    if service.unverified {
        return BTreeMap::new();
    }
    let version = service.version_hash();
    let mut fields = BTreeMap::<(String, String), Field>::new();
    struct FieldMetadata {
        types: Vec<String>,
        aliases: Vec<String>,
        description: Option<String>,
        observed: bool,
    }
    fn add(
        fields: &mut BTreeMap<(String, String), Field>,
        service: Uuid,
        version: &str,
        fact: &str,
        path: &str,
        metadata: FieldMetadata,
    ) {
        let FieldMetadata { types, aliases, description, observed } = metadata;
        if fields.len() >= 256 {
            return;
        }
        let field = fields
            .entry((fact.into(), path.into()))
            .or_insert_with(|| Field {
                id: format!("f{}", &digest(&(service, version, fact, path))[..16]),
                service,
                fact: fact.into(),
                path: path.into(),
                types: BTreeSet::new(),
                aliases: vec![],
                description: None,
                observed: false,
            });
        field.types.extend(types);
        for alias in aliases {
            if !field.aliases.contains(&alias) {
                field.aliases.push(alias);
            }
        }
        if description.is_some() {
            field.description = description;
        }
        field.observed |= observed;
    }
    fn schema(
        fields: &mut BTreeMap<(String, String), Field>,
        service: Uuid,
        version: &str,
        fact: &str,
        path: &str,
        value: &Value,
        depth: usize,
    ) {
        if depth > 10 || fields.len() >= 256 {
            return;
        }
        let aliases = value["$comment"]
            .as_str()
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
            .and_then(|value| value["apiosk_output_aliases_v1"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let types = match &value["type"] {
            Value::String(kind) => vec![kind.clone()],
            Value::Array(types) => types
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            _ => vec![],
        };
        let populated = value["properties"]
            .as_object()
            .is_some_and(|properties| !properties.is_empty())
            || value.get("items").is_some();
        if !types.is_empty()
            && (populated
                || !types
                    .iter()
                    .all(|kind| matches!(kind.as_str(), "object" | "array")))
            || !aliases.is_empty()
        {
            add(
                fields,
                service,
                version,
                fact,
                path,
                FieldMetadata { types, aliases,
                    description:value["description"].as_str().map(|s| s.chars().take(240).collect()),
                    observed:false },
            );
        }
        for (name, value) in value["properties"].as_object().into_iter().flatten() {
            schema(
                fields,
                service,
                version,
                fact,
                &child(path, name),
                value,
                depth + 1,
            );
        }
        if let Some(items) = value.get("items") {
            schema(
                fields,
                service,
                version,
                fact,
                &child(path, "*"),
                items,
                depth + 1,
            );
        }
        for branch in ["anyOf", "oneOf", "allOf"] {
            for value in value[branch].as_array().into_iter().flatten() {
                schema(fields, service, version, fact, path, value, depth + 1);
            }
        }
    }
    fn sample(
        fields: &mut BTreeMap<(String, String), Field>,
        service: Uuid,
        version: &str,
        fact: &str,
        path: &str,
        value: &Value,
        depth: usize,
    ) {
        if depth > 10 || fields.len() >= 256 {
            return;
        }
        let kind = match value {
            Value::Null => return,
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        };
        if !value.as_array().is_some_and(Vec::is_empty)
            && !value.as_object().is_some_and(serde_json::Map::is_empty)
        {
            add(
                fields,
                service,
                version,
                fact,
                path,
                FieldMetadata { types:vec![kind.into()], aliases:vec![], description:None, observed:true },
            );
        }
        match value {
            Value::Object(values) => {
                for (name, value) in values {
                    sample(
                        fields,
                        service,
                        version,
                        fact,
                        &child(path, name),
                        value,
                        depth + 1,
                    );
                }
            }
            Value::Array(values) => {
                for value in values.iter().take(16) {
                    sample(
                        fields,
                        service,
                        version,
                        fact,
                        &child(path, "*"),
                        value,
                        depth + 1,
                    );
                }
            }
            _ => (),
        }
    }
    for fact in service.contract.output_map.produces.keys() {
        if let Some(definition) = catalog.vocabulary.get(fact) {
            schema(
                &mut fields,
                service.id,
                &version,
                fact,
                "",
                &definition.value_schema,
                0,
            );
        }
    }
    if let Some(extraction) = service
        .sample_payload
        .as_ref()
        .and_then(|sample| service.contract.extract(sample, &catalog.vocabulary).ok())
        .filter(|extraction| extraction.rejected.is_empty())
    {
        for subject in extraction.subjects.iter().take(16) {
            for fact in &subject.facts {
                sample(
                    &mut fields,
                    service.id,
                    &version,
                    &fact.fact_type,
                    "",
                    &fact.value,
                    0,
                );
            }
        }
    }
    fields
        .into_values()
        .map(|field| (field.id.clone(), field))
        .collect()
}



fn candidates<'a>(intake: &Intake, index: usize, catalog: &'a Catalog) -> Vec<&'a Service> {
    let Some(found) = super::explicit_request::request_match(intake, index) else {
        return vec![];
    };
    catalog
        .services
        .iter()
        .filter(|service| {
            !service.unverified
                && found.operations.contains(&service.contract.capability_slug)
                && found.apiosk.iter().any(|candidate| {
                    candidate.capability.as_deref() == Some(&service.contract.capability_slug)
                        && candidate.endpoint_id.as_deref()
                            == Some(&service.endpoint_id.to_string())
                        && candidate
                            .source_slug
                            .as_deref()
                            .is_none_or(|source| source == service.api_slug)
                })
        })
        .collect()
}

fn record_field(field:&Field)->bool {
    field.types.iter().any(|kind|matches!(kind.as_str(),"object"|"array"))
}

fn binding_shape_matches(output:&str,fields:&[&Field])->bool {
    // Semantic planning may compose a record from several concrete fields.
    // One scalar, even with a matching label, cannot stand for a record.
    !super::plan_coverage::requires_record_shape(output)
        || fields.len()>1 || fields.first().is_some_and(|field|record_field(field))
}

pub(super) fn view(intake: &Intake, catalog: &Catalog) -> Value {
    let selected: BTreeSet<_> = (0..intake.parsed_request.capabilities.len())
        .flat_map(|index| candidates(intake, index, catalog))
        .map(|service| service.id)
        .collect();
    json!({
        "requirements": intake.parsed_request.capabilities.iter().enumerate().map(|(index, cap)| json!({"requirement":index,"subject":cap.subject_id,"outputs":cap.outputs,
            "binding_required":cap.outputs.iter().filter(|output|!candidates(intake,index,catalog).iter().any(|service|super::plan_coverage::output_covered(output,service,catalog,false))).collect::<Vec<_>>(),
            "input_readiness":cap.subject_id.as_ref().and_then(|id| intake.parsed_request.subjects.iter().find(|subject| &subject.id == id))
                .map(|subject| candidates(intake,index,catalog).iter().map(|service| json!({"service":service.id,
                    "missing_inputs":crate::ask_v2::source_options::missing(service,subject,catalog)})).collect::<Vec<_>>()),
            "services":candidates(intake,index,catalog).iter().map(|service|service.id).collect::<Vec<_>>()})).collect::<Vec<_>>(),
        "sources": catalog.services.iter().filter(|service|selected.contains(&service.id)).map(|service|json!({
            "service":service.id,"source":service.name,"capability":service.contract.capability_slug,"endpoint_id":service.endpoint_id,
            "price_atomic":service.total(),"fields":inventory(service,catalog).into_values().map(|field| {
                let mut value=serde_json::to_value(field).expect("serializable field");
                value.as_object_mut().unwrap().remove("service"); value
            }).collect::<Vec<_>>()
        })).collect::<Vec<_>>()
    })
}

pub(super) fn schema() -> Value {
    json!({"type":"array","maxItems":96,"items":{"type":"object","additionalProperties":false,
    "required":["requirement","output","fields","interpretation","limitation"],"properties":{
        "requirement":{"type":"integer","minimum":0},"output":{"type":"string"},
        "fields":{"type":"array","minItems":1,"maxItems":12,"items":{"type":"string"}},
        "interpretation":{"type":"string","minLength":1,"maxLength":500},"limitation":{"type":["string","null"],"maxLength":300}
    }}})
}

/// Exact schema fields and schema-declared aliases can be compiled without a
/// model. Other wording is grounded dynamically against `view` by the planner.
pub(super) fn compile(intake:&Intake,catalog:&Catalog,selected:&[&Service])->Vec<Binding> {
    let selected:BTreeSet<_>=selected.iter().map(|service|service.id).collect();
    let mut bindings=Vec::new();
    for (requirement,cap) in intake.parsed_request.capabilities.iter().enumerate() {
        let sources:Vec<_>=candidates(intake,requirement,catalog).into_iter().filter(|service|selected.contains(&service.id))
            .map(|service|(service,inventory(service,catalog))).collect();
        for output in &cap.outputs {
            // Schema aliases and normalized field names work for any admitted
            // API, including future catalog entries. No fuzzy semantic guess.
            let normalized=|text:&str| {
                let mut name=String::new();
                for (index,ch) in text.chars().enumerate() {
                    if ch.is_uppercase() && index>0 && !name.ends_with('_') {name.push('_');}
                    if ch.is_alphanumeric() {name.extend(ch.to_lowercase());} else if !name.ends_with('_') {name.push('_');}
                }
                name.trim_matches('_').to_owned()
            };
            let mut fields:Vec<_>=sources.iter().flat_map(|(_,fields)|fields.values()).filter(|field|
                (!super::plan_coverage::requires_record_shape(output) || record_field(field))
                    && (field.aliases.contains(output) || field.path.rsplit('/').next().is_some_and(|name|!name.is_empty() && normalized(name)==normalized(output))))
                .map(|field|field.id.clone()).collect();
            fields.sort();fields.dedup();
            if !fields.is_empty() && fields.len()<=12 {bindings.push(Binding {requirement,output:output.clone(),fields,
                interpretation:format!("Report the returned {}.",output.replace('_'," ")),limitation:None});}
        }
    }
    if validate(&bindings,intake,catalog).is_ok() {bindings} else {vec![]}
}

/// The model sees each field as {id, fact, path} and sometimes cites the fact
/// or path instead of the opaque id. Resolve a citation only when it names
/// exactly one field of the requirement's admitted endpoints, preferring the
/// endpoints the reply's goals select. Unbacked citations are dropped beside
/// a backed one; a binding without any backed field still fails validation.
pub(super) fn canonicalize(
    bindings: &mut [Binding],
    intent: Option<&super::Intent>,
    intake: &Intake,
    catalog: &Catalog,
) {
    for binding in bindings.iter_mut() {
        let services = candidates(intake, binding.requirement, catalog);
        let selected = |service: &Service| intent.is_some_and(|intent| intent.goals.iter().any(|goal|
            goal.capability == service.contract.capability_slug
                && goal.constraints.get("source").and_then(Value::as_str).is_none_or(|source| source == service.api_slug)
                && super::plan_coverage::endpoint_matches(goal.constraints.get("endpoint_id"), Some(&service.endpoint_id.to_string()))));
        let fields: Vec<(bool, Field)> = services.iter()
            .flat_map(|service| inventory(service, catalog).into_values().map(|field| (selected(service), field)))
            .collect();
        // One binding describes one endpoint's response: anchor shared field
        // names to the admitted endpoint that backs most of its citations.
        let anchor = services.iter().max_by_key(|service| (
            binding.fields.iter().filter(|reference| fields.iter().any(|(_, field)|
                field.service == service.id && (&field.id == *reference || cites(field, reference)))).count(),
            selected(service),
        )).map(|service| service.id);
        let mut resolved = Vec::new();
        for reference in &binding.fields {
            let id = if fields.iter().any(|(_, field)| &field.id == reference) {
                reference.clone()
            } else {
                let named = |scope: u8| fields.iter()
                    .filter(|(chosen, field)| match scope { 0 => Some(field.service) == anchor, 1 => *chosen, _ => true }
                        && cites(field, reference))
                    .map(|(_, field)| field.id.as_str()).collect::<BTreeSet<_>>();
                [named(0), named(1), named(2)].into_iter().find(|ids| !ids.is_empty())
                    .filter(|ids| ids.len() == 1)
                    .and_then(|ids| ids.into_iter().next().map(str::to_owned))
                    .unwrap_or_else(|| reference.clone())
            };
            if !resolved.contains(&id) {
                resolved.push(id);
            }
        }
        // A citation of another requirement's endpoint proves nothing here;
        // drop it only while a backed field still grounds this binding.
        let backed: Vec<String> = resolved.iter()
            .filter(|id| fields.iter().any(|(_, field)| &field.id == *id)).cloned().collect();
        binding.fields = if backed.is_empty() { resolved } else { backed };
    }
}

fn cites(field: &Field, reference: &str) -> bool {
    let reference = reference.trim();
    if field.path.is_empty() {
        return reference == field.fact;
    }
    reference == format!("{}{}", field.fact, field.path)
        || reference == format!("{}.{}", field.fact, field.path.trim_start_matches('/'))
        || reference == field.path
}

pub(super) fn validate(
    bindings: &[Binding],
    intake: &Intake,
    catalog: &Catalog,
) -> anyhow::Result<()> {
    ensure!(bindings.len() <= 96, "too many source field bindings");
    let mut seen = BTreeSet::new();
    for binding in bindings {
        let cap = intake
            .parsed_request
            .capabilities
            .get(binding.requirement)
            .ok_or_else(|| anyhow::anyhow!("unknown source requirement"))?;
        ensure!(
            cap.outputs.contains(&binding.output)
                && seen.insert((binding.requirement, &binding.output)),
            "unknown or duplicate requested output binding"
        );
        ensure!(
            !binding.fields.is_empty()
                && binding.fields.len() <= 12
                && binding.fields.iter().collect::<BTreeSet<_>>().len() == binding.fields.len(),
            "invalid source field references"
        );
        ensure!(
            !binding.interpretation.trim().is_empty()
                && binding.interpretation.chars().count() <= 500
                && binding
                    .limitation
                    .as_ref()
                    .is_none_or(|note| !note.trim().is_empty() && note.chars().count() <= 300),
            "invalid source field explanation"
        );
        let available: BTreeMap<_,_> = candidates(intake, binding.requirement, catalog)
            .into_iter()
            .flat_map(|service| inventory(service, catalog))
            .collect();
        ensure!(
            binding.fields.iter().all(|field| available.contains_key(field)),
            "source field is not backed by this requirement's admitted endpoint"
        );
        ensure!(binding_shape_matches(&binding.output,&binding.fields.iter().filter_map(|id|available.get(id)).collect::<Vec<_>>()),
            "a record category needs a structured field or a composition of concrete fields");
    }
    Ok(())
}

pub(crate) fn covered(
    bindings: &[Binding],
    requirement: usize,
    output: &str,
    services: &[&Service],
    catalog: &Catalog,
) -> bool {
    let Some(binding) = bindings
        .iter()
        .find(|binding| binding.requirement == requirement && binding.output == output)
    else {
        return false;
    };
    if binding.fields.is_empty() {
        return false;
    }
    let fields: BTreeMap<_,_> = services
        .iter()
        .flat_map(|service| inventory(service, catalog))
        .collect();
    binding.fields.iter().all(|field| fields.contains_key(field))
        && binding_shape_matches(output,&binding.fields.iter().filter_map(|id|fields.get(id)).collect::<Vec<_>>())
}

pub(super) fn answer_instructions(bindings: &[Binding], intake: &Intake) -> Vec<String> {
    bindings.iter().map(|binding|format!("{} / {}: {} {} Use only this request's returned source evidence; sample values are not current answers and absent fields remain unavailable.",
        intake.parsed_request.capabilities[binding.requirement].name, binding.output, binding.interpretation, binding.limitation.as_deref().unwrap_or_default())).collect()
}

#[derive(Debug, thiserror::Error)]
#[error("the proposed plan is missing requested source coverage")]
pub(crate) struct CoverageError {
    pub outputs: Vec<String>,
    pub inputs: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
#[error("the proposed matched sources do not form an executable plan")]
pub(super) struct ResolutionError {
    pub reason: String,
}

/// Run the final coverage checks inside the model's bounded repair loop too.
/// A correctable draft must not become a user-facing refusal before the model
/// has seen which field or literal its proposed sources failed to preserve.
pub(super) fn verify_plan(
    intent: &super::Intent,
    task: &crate::state::Task,
    intake: &Intake,
    catalog: &Catalog,
    bindings: &[Binding],
) -> anyhow::Result<()> {
    let mut draft = task.clone();
    draft.capability_intake = Some(intake.clone());
    draft.output_grounding = Grounding::new(intake, catalog, bindings.to_vec());
    let mut intent = intent.clone();
    crate::engine::bind_subjects(&mut draft, catalog, &mut intent)?;
    if draft.independent_sources() {
        let outputs = super::plan_coverage::uncovered_intent_outputs(intake, catalog, &intent, bindings);
        if !outputs.is_empty() {
            return Err(CoverageError { outputs, inputs: vec![] }.into());
        }
    }
    match crate::resolver::resolve(catalog, &draft, &intent) {
        crate::resolver::Resolution::Plan(plan) => {
            let (outputs, inputs) = if let Some(context) = &draft.source_selection {
                (
                    context.uncovered_outputs(&draft, catalog, &intent, &plan)?,
                    context.uncovered_inputs(&draft, catalog, &intent, &plan)?,
                )
            } else {
                (
                    super::plan_coverage::uncovered_outputs(&draft, catalog, &intent, &plan),
                    super::plan_coverage::uncovered_inputs(&draft, catalog, &intent, &plan),
                )
            };
            if !outputs.is_empty() || !inputs.is_empty() {
                return Err(CoverageError { outputs, inputs }.into());
            }
        },
        crate::resolver::Resolution::Unsupported(reason)
            if super::plan_coverage::retrievals_matched(intake)
                && reason != crate::protocol::SOURCE_LIMIT_MESSAGE => {
            // Matched output fields do not prove that these exact calls can
            // satisfy the proposed constraints. Return the rejected draft to
            // the bounded model repair before presenting a terminal outcome.
            return Err(ResolutionError { reason }.into());
        },
        // Missing API inputs and the ten-call limit have dedicated actions in
        // the engine. They are requests for user context, not model outages.
        _ => {},
    }
    Ok(())
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod record_shape_tests;
