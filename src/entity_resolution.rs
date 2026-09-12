use crate::{catalog::fact_value, state::Task};
use catalog_core::facts::contract::SubjectFacts;
use uuid::Uuid;

fn name(value: &str) -> String {
    value
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}
fn legal_name(value: &str) -> String {
    let value = name(value);
    for suffix in [
        "beslotenvennootschap",
        "bv",
        "nv",
        "ltd",
        "limited",
        "inc",
        "gmbh",
    ] {
        if let Some(base) = value.strip_suffix(suffix).filter(|base| base.len() >= 3) {
            return base.to_owned();
        }
    }
    value
}

/// Deterministic confidence rules, not probabilistic claims. A unique exact
/// company name (ignoring legal suffix) can resolve a complete candidate set.
/// Conflicting known identifiers veto a match; near ties always ask the user.
pub fn confident_candidate(
    task: &Task,
    entity: Uuid,
    candidates: &[SubjectFacts],
) -> Option<usize> {
    let requested = fact_value(&task.facts, entity, "company.name")
        .ok()
        .flatten()
        .and_then(|v| v.as_str().map(str::to_owned))?;
    let mut ranked = Vec::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let mut score: f64 = candidate
            .fact("company.name")
            .and_then(|f| f.value.as_str())
            .map_or(0., |label| {
                if name(label) == name(&requested) {
                    0.98
                } else if legal_name(label) == legal_name(&requested) {
                    0.92
                } else {
                    0.
                }
            });
        for fact in &candidate.facts {
            if !matches!(
                fact.fact_type.as_str(),
                "company.country"
                    | "company.country_code"
                    | "company_registry.kvknummer"
                    | "company.registration.nl.kvk"
                    | "company.lei"
                    | "company.vat"
                    | "company.domain"
            ) {
                continue;
            }
            if let Ok(Some(known)) = fact_value(&task.facts, entity, &fact.fact_type) {
                if known != fact.value {
                    score = 0.;
                    break;
                }
            }
        }
        ranked.push((index, score));
    }
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    let (index, best) = *ranked.first()?;
    (best >= 0.90 && best - ranked.get(1).map_or(0., |p| p.1) >= 0.10).then_some(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use catalog_core::facts::contract::Fact;
    use chrono::Utc;
    use serde_json::json;
    fn candidate(label: &str) -> SubjectFacts {
        SubjectFacts {
            index: 0,
            identity: None,
            facts: vec![Fact {
                fact_type: "company.name".into(),
                subject_role: "subject".into(),
                value: json!(label),
                extraction_base: "/".into(),
            }],
        }
    }
    fn task() -> (Task, Uuid) {
        let id = Uuid::new_v4();
        let mut task = Task::default();
        task.facts.push(crate::state::Fact {
            entity_ref: id,
            semantic_type: "company.name".into(),
            value: json!("Bitvavo"),
            provenance: json!({"kind":"user"}),
            observed_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::hours(1),
            verification: "user".into(),
        });
        (task, id)
    }
    #[test]
    fn unique_legal_name_resolves_without_picking_a_subsidiary() {
        let (task, id) = task();
        assert_eq!(
            confident_candidate(
                &task,
                id,
                &[candidate("Bitvavo Custody B.V."), candidate("Bitvavo B.V.")]
            ),
            Some(1)
        );
    }
    #[test]
    fn ties_and_nonmatches_require_selection() {
        let (task, id) = task();
        assert_eq!(
            confident_candidate(
                &task,
                id,
                &[candidate("Bitvavo B.V."), candidate("Bitvavo N.V.")]
            ),
            None
        );
        assert_eq!(
            confident_candidate(
                &task,
                id,
                &[candidate("Bitvavo Payments"), candidate("Bitvavo Custody")]
            ),
            None
        );
    }
}
