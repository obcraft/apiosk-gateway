//! Fixed workflow recipes. Source calls still use the ordinary resolver, quote,
//! authorization, worker and saved-result analysis.
use crate::{
    catalog::Catalog,
    parser::{Goal, Intent, Subject},
    state::{Entity, Fact, Task},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Field {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default = "text_type")]
    pub input_type: String,
    #[serde(default)]
    pub placeholder: String,
    #[serde(default)]
    pub help: String,
    #[serde(default)]
    pub options: Vec<String>,
}
fn text_type() -> String {
    "text".into()
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Recipe {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub fields: Vec<Field>,
    pub checks: Vec<String>,
    pub limitations: Vec<String>,
    #[serde(default)]
    pub source_limitations: BTreeMap<String, Vec<String>>,
    pub output: String,
    pub answer_instructions: String,
    pub example: BTreeMap<String, Value>,
    pub steps: Vec<RecipeStep>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RecipeStep {
    pub label: String,
    /// Empty means any admitted source for the exact same capability.
    #[serde(default)]
    pub source: String,
    pub capability: String,
    pub subject: String,
    pub inputs: BTreeMap<String, Value>,
    #[serde(default)]
    pub when: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkflowContext {
    #[serde(default)]
    pub collecting: bool,
    pub slug: String,
    pub title: String,
    pub input: BTreeMap<String, Value>,
    pub limitations: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StartRequest {
    pub request_id: Uuid,
    pub input: BTreeMap<String, Value>,
}
pub fn recipes() -> Vec<Recipe> {
    serde_json::from_str(include_str!("workflow_catalog.json"))
        .expect("checked workflow definitions")
}
pub fn recipe(slug: &str) -> Option<Recipe> {
    recipes().into_iter().find(|r| r.slug == slug)
}
pub fn selected_limitations(slug: &str, sources: &[&str]) -> Option<Vec<String>> {
    let recipe = recipe(slug)?;
    let mut limitations = recipe.limitations;
    for (source, caveats) in recipe.source_limitations {
        if sources.contains(&source.as_str()) { limitations.extend(caveats); }
    }
    Some(limitations)
}
pub fn known(slug: &str) -> bool {
    slug == crate::workflows::SUPPLIER_CHECK || recipe(slug).is_some()
}

fn substitute(template: &str, values: &BTreeMap<String, Value>) -> anyhow::Result<String> {
    let mut result = template.to_owned();
    for (key, value) in values {
        let text = value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string());
        result = result.replace(&format!("{{{key}}}"), &text);
    }
    anyhow::ensure!(
        !result.contains('{') && !result.contains('}'),
        "Complete the required workflow details."
    );
    Ok(result)
}

/// Validate browser input independently of HTML validation. Derived URL hosts
/// and search queries are code-built and never become executable instructions.
pub fn prepare(
    slug: &str,
    input: &BTreeMap<String, Value>,
    catalog: &Catalog,
) -> anyhow::Result<(Intent, WorkflowContext, String)> {
    let recipe = recipe(slug).ok_or_else(|| anyhow::anyhow!("Unknown workflow."))?;
    anyhow::ensure!(
        input
            .keys()
            .all(|k| recipe.fields.iter().any(|f| f.key == *k)),
        "Unknown workflow field."
    );
    let mut values = BTreeMap::new();
    for field in &recipe.fields {
        let value = input.get(&field.key).cloned().unwrap_or(Value::Null);
        let text = match &value {
            Value::String(s) => s.trim().to_owned(),
            Value::Number(n) => n.to_string(),
            Value::Null => String::new(),
            _ => anyhow::bail!("{} must be text or a number.", field.label),
        };
        anyhow::ensure!(text.len() <= 500, "{} is too long.", field.label);
        anyhow::ensure!(
            !field.required || !text.is_empty(),
            "{} is required.",
            field.label
        );
        if text.is_empty() {
            continue;
        }
        anyhow::ensure!(
            !text.contains(['{', '}']) && !text.chars().any(|c| c.is_control()),
            "{} contains invalid characters.",
            field.label
        );
        anyhow::ensure!(
            field.options.is_empty() || field.options.contains(&text),
            "Choose a supported {}.",
            field.label
        );
        let normalized = match field.input_type.as_str() {
            "number" => {
                let n: u64 = text
                    .parse()
                    .map_err(|_| anyhow::anyhow!("{} must be a whole number.", field.label))?;
                anyhow::ensure!(n <= 1_000_000, "{} is too large.", field.label);
                json!(n)
            }
            "url" => {
                let url = url::Url::parse(&text).map_err(|_| {
                    anyhow::anyhow!("Enter a complete https:// URL for {}.", field.label)
                })?;
                anyhow::ensure!(
                    url.scheme() == "https"
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.host_str().is_some(),
                    "{} must be a public HTTPS URL.",
                    field.label
                );
                let host = url.host_str().unwrap();
                anyhow::ensure!(
                    host.contains('.')
                        && host.parse::<std::net::IpAddr>().is_err()
                        && !host.ends_with(".localhost")
                        && !host.ends_with(".local"),
                    "{} must be a public website.",
                    field.label
                );
                values.insert(
                    format!("{}_domain", field.key),
                    json!(host.trim_start_matches("www.")),
                );
                json!(url.as_str())
            }
            "domain" => {
                let host = text
                    .trim_start_matches("https://")
                    .trim_start_matches("http://")
                    .trim_end_matches('/')
                    .to_lowercase();
                anyhow::ensure!(
                    host.contains('.')
                        && host.len() <= 253
                        && host
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-'),
                    "Enter a domain such as example.com."
                );
                json!(host)
            }
            _ => json!(text),
        };
        values.insert(field.key.clone(), normalized);
    }
    if let (Some(min), Some(max)) = (
        values.get("headcount_min").and_then(Value::as_u64),
        values.get("headcount_max").and_then(Value::as_u64),
    ) {
        anyhow::ensure!(
            min <= max,
            "Minimum headcount must not exceed maximum headcount."
        );
    }
    anyhow::ensure!(
        values.contains_key("vat_country") == values.contains_key("vat_number"),
        "Provide both the VAT country code and VAT number, or leave both empty."
    );
    if let Some(plate) = values.get_mut("plate") {
        *plate = json!(plate
            .as_str()
            .unwrap()
            .replace(['-', ' '], "")
            .to_uppercase());
    }
    if let Some(number) = values.get_mut("registration") {
        *number = json!(number.as_str().unwrap().to_uppercase());
    }
    let mut subjects: Vec<Subject> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    let mut goals = Vec::new();
    for step in &recipe.steps {
        if !step.when.iter().all(|(k, w)| {
            values
                .get(k)
                .is_some_and(|v| {
                    let matches = |expected: &Value| expected.as_str().is_some_and(|expected| expected == "*" || v.as_str().is_some_and(|actual| actual.eq_ignore_ascii_case(expected)));
                    w.as_array().map_or_else(|| matches(w), |options| options.iter().any(matches))
                })
        }) {
            continue;
        }
        anyhow::ensure!(catalog.services.iter().any(|s| (step.source.is_empty()||s.api_slug == step.source) && s.contract.capability_slug == step.capability), "{} is temporarily unavailable for this workflow.", step.label);
        let index = if let Some(i) = keys.iter().position(|k| k == &step.subject) {
            i
        } else {
            let i = subjects.len();
            keys.push(step.subject.clone());
            subjects.push(Subject {
                entity_ref: None,
                label: substitute(&step.subject, &values)?,
                values: BTreeMap::new(),
            });
            i
        };
        for (kind, template) in &step.inputs {
            let value = if let Some(text) = template.as_str() {
                if let Some(key) = text
                    .strip_prefix('{')
                    .and_then(|t| t.strip_suffix('}'))
                    .filter(|k| !k.contains(['{', '}']))
                {
                    match values.get(key) {
                        Some(v) => v.clone(),
                        None => continue,
                    }
                } else {
                    json!(substitute(text, &values)?)
                }
            } else {
                template.clone()
            };
            let fact = catalog
                .vocabulary
                .get(kind)
                .ok_or_else(|| anyhow::anyhow!("Workflow input {} is unavailable.", kind))?;
            catalog_core::facts::schema::validate(&fact.value_schema, &value)
                .map_err(|_| anyhow::anyhow!("Invalid value for {}.", kind))?;
            if let Some(existing) = subjects[index].values.insert(kind.clone(), value.clone()) {
                anyhow::ensure!(existing == value, "Conflicting workflow values.");
            }
        }
        goals.push(Goal {
            capability: step.capability.clone(),
            subject: index,
            constraints: if step.source.is_empty(){BTreeMap::new()}else{BTreeMap::from([("source".into(), json!(step.source))])},
        });
    }
    anyhow::ensure!(
        !goals.is_empty() && goals.len() <= 8 && subjects.len() <= 8,
        "Invalid workflow recipe size."
    );
    let title = values
        .get("name")
        .or_else(|| values.get("address"))
        .or_else(|| values.get("topic"))
        .or_else(|| values.get("plate"))
        .or_else(|| values.get("location"))
        .or_else(|| values.get("url1"))
        .and_then(Value::as_str)
        .unwrap_or(&recipe.name)
        .to_owned();
    let selected: Vec<_> = goals.iter().filter_map(|goal| goal.constraints.get("source").and_then(Value::as_str)).collect();
    let limitations = selected_limitations(slug, &selected).unwrap_or_default();
    let context = WorkflowContext {
        collecting: false,
        slug: recipe.slug.clone(),
        title,
        input: input.clone(),
        limitations: limitations.clone(),
    };
    let question = format!(
        "{}: {}. {}",
        recipe.name,
        recipe.fields
            .iter()
            .filter_map(|field| values.get(&field.key).map(|value| (field, value)))
            .map(|(field, v)| format!(
                "{}: {}",
                field.label,
                v.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| v.to_string())
            ))
            .collect::<Vec<_>>()
            .join("; "),
        recipe.output
    );
    anyhow::ensure!(
        question.len() <= crate::protocol::MAX_QUESTION,
        "Workflow details are too long."
    );
    let intent=Intent {subjects,goals,answer_operations:vec![format!("{}. Address these checks: {}. Preserve these coverage limits: {}. Use only saved evidence; say unknown for missing checks, distinguish possible name matches from confirmed identities, and never describe a source retrieval as a passed verification. Return a concise, usable result in the user's language.",recipe.answer_instructions,recipe.checks.join("; "),limitations.join(" "))],unresolved_references:vec![]};
    Ok((intent, context, question))
}

/// Used by recipe validation and the production-catalog audit before a purchase.
pub fn plan_preview(catalog: &Catalog, intent: &Intent) -> crate::resolver::Resolution {
    let mut intent = intent.clone();
    let mut task = Task::default();
    for subject in &mut intent.subjects {
        let id = Uuid::new_v4();
        subject.entity_ref = Some(id);
        task.entities.push(Entity {
            entity_ref: id,
            label: subject.label.clone(),
            selected: true,
        });
        for (kind, value) in &subject.values {
            task.facts.push(Fact {
                entity_ref: id,
                semantic_type: kind.clone(),
                value: value.clone(),
                provenance: json!({"kind":"user_input"}),
                observed_at: chrono::Utc::now(),
                expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
                verification: "user_asserted".into(),
            });
        }
    }
    crate::resolver::resolve(catalog, &task, &intent)
}

pub fn definitions(catalog: &Catalog) -> Vec<Value> {
    recipes().into_iter().map(|r| {
        let available=prepare(&r.slug,&r.example,catalog).and_then(|(intent,_,_)| match plan_preview(catalog,&intent) {
            crate::resolver::Resolution::Plan(_)=>Ok(()), _=>anyhow::bail!("The required source operations are temporarily unavailable."),
        });
        let mut sources=BTreeMap::new();
        for step in &r.steps { for service in catalog.services.iter().filter(|s|s.contract.capability_slug==step.capability&&(step.source.is_empty()||s.api_slug==step.source)) { sources.insert(service.api_slug.clone(),service.name.clone()); } }
        let steps:Vec<_>=r.steps.iter().enumerate().map(|(i,step)| {
            let condition=step.when.iter().map(|(key,value)| {
                let label=r.fields.iter().find(|f|f.key==*key).map(|f|f.label.as_str()).unwrap_or(key);
                let value=value.as_str().or_else(||value.as_array().and_then(|options|options.first()).and_then(Value::as_str)).unwrap_or_default();
                if value=="*" {format!("When {} is provided",label.to_lowercase())} else {format!("When {} is {}",label.to_lowercase(),value)}
            }).collect::<Vec<_>>().join(" and ");
            let candidates: BTreeMap<_,_>=catalog.services.iter().filter(|s|s.contract.capability_slug==step.capability&&(step.source.is_empty()||s.api_slug==step.source)).map(|s|(&s.api_slug,&s.name)).collect();
            json!({"id":format!("step-{i}"),"label":step.label,"sources":candidates.into_iter().map(|(slug,name)|json!({"slug":slug,"name":name})).collect::<Vec<_>>(),"when":step.when,"condition":condition})
        }).collect();
        json!({"slug":r.slug,"name":r.name,"description":r.description,"category":r.category,"input":{"kind":"fields"},"fields":r.fields,"steps":steps,"checks":r.checks.iter().enumerate().map(|(i,label)|json!({"id":format!("check-{i}"),"label":label})).collect::<Vec<_>>(),"sources":sources.into_iter().map(|(slug,name)|json!({"slug":slug,"name":name})).collect::<Vec<_>>(),"price":null,"output":r.output,"limitations":r.limitations,"available":available.is_ok(),"unavailable_reason":available.err().map(|e|e.to_string())})
    }).collect()
}

#[cfg(test)]
mod scope_tests {
    #[test]
    fn limitations_follow_selected_sources() {
        let limits = super::selected_limitations("subsidy-finder", &["eu-funding-tenders", "linkup"]).unwrap();
        assert!(!limits.iter().any(|limit| limit.contains("RVO")));
        let recipe = super::recipe("subsidy-finder").unwrap();
        let rvo = recipe.source_limitations.keys().next().unwrap();
        assert!(super::selected_limitations("subsidy-finder", &[rvo]).unwrap().iter().any(|limit| limit.contains("RVO")));
        assert!(!super::selected_limitations("customer-onboarding", &["kvk", "opensanctions"]).unwrap().iter().any(|limit| limit.contains("VIES")));
    }
}
