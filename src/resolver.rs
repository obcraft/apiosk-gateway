use crate::{
    catalog::{fact_value, Catalog, Service},
    parser::Intent,
    protocol::{MAX_DEPTH, MAX_STEPS},
    state::{digest, Task},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub service: Service,
    pub entity_ref: Uuid,
    pub goal_index: usize,
}
impl Step {
    pub fn key(&self) -> String {
        format!("{}:{}", self.service.id, self.entity_ref)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub quote_ref: Uuid,
    pub steps: Vec<Step>,
    pub cap_atomic: i64,
    pub expires_at: DateTime<Utc>,
    pub hash: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    Run {
        action_id: Uuid,
        step: usize,
    },
    Supply {
        action_id: Uuid,
        entity_ref: Uuid,
        semantic_type: String,
        schema: Value,
    },
    Choose {
        action_id: Uuid,
        candidates: Vec<Uuid>,
    },
    Read {
        action_id: Uuid,
        result_ref: Uuid,
    },
    Poll {
        action_id: Uuid,
        execution_id: Uuid,
    },
    Cancel {
        action_id: Uuid,
    },
}
impl Action {
    pub fn id(&self) -> Uuid {
        match self {
            Self::Run { action_id, .. }
            | Self::Supply { action_id, .. }
            | Self::Choose { action_id, .. }
            | Self::Read { action_id, .. }
            | Self::Poll { action_id, .. }
            | Self::Cancel { action_id } => *action_id,
        }
    }
    pub fn public(&self) -> crate::protocol::NextAction {
        let (kind, label, paid, input) = match self {
            Self::Run { .. } => (
                "execute_quoted_step",
                "Voer de volgende stap uit".into(),
                true,
                json!({"type":"null"}),
            ),
            Self::Supply {
                semantic_type,
                schema,
                ..
            } => (
                "supply_input",
                format!("Vul {semantic_type} aan"),
                false,
                json!({"type":"object","properties":{"value":schema},"required":["value"],"additionalProperties":false}),
            ),
            Self::Choose { candidates, .. } => (
                "select_entity",
                "Selecteer de juiste entiteit".into(),
                false,
                json!({"type":"object","properties":{"entity_ref":{"type":"string","enum":candidates}},"required":["entity_ref"],"additionalProperties":false}),
            ),
            Self::Read { .. } => (
                "read_result",
                "Lees het opgeslagen resultaat".into(),
                false,
                json!({"type":"object","properties":{"offset":{"type":"integer","minimum":0}},"additionalProperties":false}),
            ),
            Self::Poll { .. } => (
                "poll",
                "Controleer de bestaande uitvoering".into(),
                false,
                json!({"type":"null"}),
            ),
            Self::Cancel { .. } => (
                "cancel",
                "Stop nieuwe uitvoeringen".into(),
                false,
                json!({"type":"null"}),
            ),
        };
        crate::protocol::NextAction {
            action_id: self.id(),
            kind: kind.into(),
            label,
            requires_authorization: paid,
            input_schema: input,
        }
    }
}
#[derive(Debug)]
pub enum Resolution {
    Plan(Plan),
    Missing { entity: Uuid, kind: String },
    Unsupported(String),
}
fn allows(service: &Service, constraints: &BTreeMap<String, Value>) -> bool {
    constraints.iter().all(|(k, v)| match k.as_str() {
        "source" => v.as_str() == Some(service.api_slug.as_str()),
        "jurisdiction" => v
            .as_str()
            .zip(service.jurisdiction.as_deref())
            .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b)),
        // No timestamp/filing-selection mapping yet: refusing preserves the
        // requirement instead of claiming the first record is the latest.
        _ => false,
    })
}
pub fn resolve(catalog: &Catalog, task: &Task, intent: &Intent) -> Resolution {
    if !intent.unresolved_references.is_empty() {
        return Resolution::Unsupported(intent.unresolved_references.join("; "));
    }
    let mut steps = Vec::new();
    let mut expansions = 0;
    for (goal_index, goal) in intent.goals.iter().enumerate() {
        let Some(entity) = intent.subjects.get(goal.subject).and_then(|s| s.entity_ref) else {
            return Resolution::Unsupported("unresolved subject".into());
        };
        let candidates = catalog
            .by_capability
            .get(&goal.capability)
            .cloned()
            .unwrap_or_default();
        let mut best: Option<Vec<Step>> = None;
        let mut missing = None;
        for index in candidates {
            let service = &catalog.services[index];
            if !allows(service, &goal.constraints) {
                continue;
            }
            let mut trial = steps.clone();
            let mut visiting = BTreeSet::new();
            match add(
                catalog,
                task,
                service,
                entity,
                goal_index,
                &mut trial,
                &mut visiting,
                0,
                &mut expansions,
            ) {
                Ok(()) => {
                    let total = trial.iter().map(|s| s.service.total()).sum::<i64>();
                    if best
                        .as_ref()
                        .is_none_or(|b| total < b.iter().map(|s| s.service.total()).sum())
                    {
                        best = Some(trial)
                    }
                }
                Err(e) => missing = Some(e),
            }
        }
        match best {
            Some(b) => steps = b,
            None => {
                return match missing {
                    Some(k) if catalog.vocabulary.contains_key(&k) => {
                        Resolution::Missing { entity, kind: k }
                    }
                    Some(e) => Resolution::Unsupported(e),
                    None => Resolution::Unsupported(format!(
                        "Geen gevalideerde dienst voldoet aan {} en alle bron-/tijdseisen",
                        goal.capability
                    )),
                }
            }
        }
    }
    if steps.len() > MAX_STEPS {
        return Resolution::Unsupported("dependency step limit".into());
    }
    let cap_atomic = steps.iter().map(|s| s.service.total()).sum();
    let hash = digest(&steps);
    Resolution::Plan(Plan {
        quote_ref: Uuid::new_v4(),
        steps,
        cap_atomic,
        expires_at: Utc::now() + chrono::Duration::minutes(10),
        hash,
    })
}
#[allow(clippy::too_many_arguments)]
fn add(
    catalog: &Catalog,
    task: &Task,
    service: &Service,
    entity: Uuid,
    goal: usize,
    steps: &mut Vec<Step>,
    visiting: &mut BTreeSet<Uuid>,
    depth: usize,
    expansions: &mut usize,
) -> Result<(), String> {
    *expansions += 1;
    if depth > MAX_DEPTH || *expansions > 256 || steps.len() >= MAX_STEPS {
        return Err("dependency search limit".into());
    }
    if steps
        .iter()
        .any(|s| s.service.id == service.id && s.entity_ref == entity)
    {
        return Ok(());
    }
    if !visiting.insert(service.id) {
        return Err("dependency cycle".into());
    }
    for req in service.contract.requires.iter().filter(|r| !r.optional) {
        match fact_value(&task.facts, entity, &req.fact_type) {
            Ok(Some(_)) => continue,
            Err(e) => return Err(e.to_string()),
            _ => (),
        }
        if steps.iter().any(|s| {
            s.entity_ref == entity
                && s.service
                    .contract
                    .output_map
                    .produces
                    .contains_key(&req.fact_type)
        }) {
            continue;
        }
        let candidates = catalog
            .by_output
            .get(&req.fact_type)
            .cloned()
            .unwrap_or_default();
        let mut found = None;
        for index in candidates {
            let mut trial = steps.clone();
            let mut seen = visiting.clone();
            if add(
                catalog,
                task,
                &catalog.services[index],
                entity,
                goal,
                &mut trial,
                &mut seen,
                depth + 1,
                expansions,
            )
            .is_ok()
                && found.as_ref().is_none_or(|b: &Vec<Step>| {
                    trial.iter().map(|s| s.service.total()).sum::<i64>()
                        < b.iter().map(|s| s.service.total()).sum()
                })
            {
                found = Some(trial)
            }
        }
        if let Some(v) = found {
            *steps = v
        } else {
            return Err(req.fact_type.clone());
        }
    }
    visiting.remove(&service.id);
    steps.push(Step {
        service: service.clone(),
        entity_ref: entity,
        goal_index: goal,
    });
    Ok(())
}

/// Presentation only: dependencies come from the quoted contracts, never an LLM guess.
pub fn describe_steps(plan: &Plan, sources: &[crate::sources::Source]) -> Vec<Value> {
    plan.steps.iter().enumerate().map(|(index, step)| {
        let service = &step.service;
        let source = sources.iter().find(|s| s.slug == service.api_slug);
        let capability = service.contract.capability_slug.as_str();
        let title = match capability {
            "company.registry-search" | "company_registry.search" => "Find the company and its KVK number".to_owned(),
            "company.financial-statements" => "Retrieve financial statements".to_owned(),
            "company.registry-profile" => "Retrieve the company profile".to_owned(),
            "company.registered-names" => "Retrieve registered company names".to_owned(),
            "company.branch-profile" => "Retrieve the branch profile".to_owned(),
            _ => capability.replace(['.', '_', '-'], " "),
        };
        let dependencies: Vec<_> = service.contract.requires.iter().filter(|r| !r.optional).filter_map(|required| {
            plan.steps[..index].iter().enumerate().rev().find(|(_, previous)|
                previous.entity_ref == step.entity_ref && previous.service.contract.output_map.produces.contains_key(&required.fact_type)
            ).map(|(previous, _)| json!({"step":previous+1,"field":required.fact_type,
                "label":if required.fact_type == "company_registry.kvknummer" {"KVK number".to_owned()} else {required.fact_type.replace(['.', '_', '-'], " ")}}))
        }).collect();
        json!({"number":index+1,"title":title,"capability":capability,
            "source":{"name":service.name,"slug":service.api_slug,"logo_url":source.and_then(|s|s.logo_url.as_deref()),"url":service.source_url},
            "dependencies":dependencies})
    }).collect()
}

#[cfg(test)]
mod presentation_tests {
    use super::*;
    #[test]
    fn explains_only_dependencies_between_steps_for_the_same_entity() {
        let snapshot: crate::catalog::Snapshot = serde_json::from_str(include_str!("../tests/fixtures/integration-catalog.json")).unwrap();
        let entity = Uuid::new_v4();
        let mut plan = Plan { quote_ref: Uuid::new_v4(), cap_atomic: 138, expires_at: Utc::now(), hash: "unchanged".into(), steps: snapshot.services.into_iter().take(2).map(|service| Step { service, entity_ref: entity, goal_index: 0 }).collect() };
        let details = describe_steps(&plan, &[]);
        assert_eq!(details[1]["dependencies"][0]["step"], 1);
        assert_eq!(details[1]["dependencies"][0]["field"], "company.id");
        assert_eq!(details[0]["source"]["slug"], "fixture");
        plan.steps[1].entity_ref = Uuid::new_v4();
        assert!(describe_steps(&plan, &[])[1]["dependencies"].as_array().unwrap().is_empty());
        assert_eq!(plan.cap_atomic, 138);
    }
}
