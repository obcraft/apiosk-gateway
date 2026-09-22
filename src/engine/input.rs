//! Shared Gateway intake rules, also used by side-effect-free parser previews.
use crate::{
    catalog::Catalog,
    parser::Intent,
    state::{Entity, Fact, Task},
};
use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

/// Bind only explicit, schema-validated values. Does not persist or execute.
/// The caller supplies a staged task when it needs a read-only preview.
pub fn bind_subjects(
    task: &mut Task,
    catalog: &Catalog,
    intent: &mut Intent,
) -> anyhow::Result<()> {
    for subject in &mut intent.subjects {
        let entity = subject.entity_ref.unwrap_or_else(Uuid::new_v4);
        if subject.entity_ref.is_none() {
            task.entities.push(Entity {
                entity_ref: entity,
                label: subject.label.clone(),
                selected: true,
            });
        }
        subject.entity_ref = Some(entity);
        for (kind, value) in &subject.values {
            user_fact(task, catalog, entity, kind, value.clone())?;
        }
    }
    Ok(())
}

pub(super) fn user_fact(
    task: &mut Task,
    catalog: &Catalog,
    entity: Uuid,
    kind: &str,
    value: Value,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        task.entities.iter().any(|e| e.entity_ref == entity),
        "unknown entity for explicit value"
    );
    let ft = catalog
        .vocabulary
        .get(kind)
        .ok_or_else(|| anyhow::anyhow!("unknown fact type"))?;
    catalog_core::facts::schema::validate(&ft.value_schema, &value)?;
    if task.facts.iter().any(|f| {
        f.entity_ref == entity
            && f.semantic_type == kind
            && f.value == value
            && f.expires_at > Utc::now()
    }) {
        return Ok(());
    }
    task.facts.push(Fact {
        entity_ref: entity,
        semantic_type: kind.into(),
        value,
        provenance: json!({"kind":"user_input"}),
        observed_at: Utc::now(),
        expires_at: Utc::now() + chrono::Duration::hours(1),
        verification: "user_asserted".into(),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Catalog, Intent) {
        (Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/integration-catalog.json")).unwrap()),
        serde_json::from_value(json!({"subjects":[{"entity_ref":null,"label":"Example","values":{"company.name":"Example"}}],"goals":[{"capability":"company.accounts","subject":0,"constraints":{}}],"answer_operations":[],"unresolved_references":[]})).unwrap())
    }
    #[test]
    fn preview_and_engine_share_input_provenance_ttl_and_deduplication() {
        let (catalog, mut intent) = fixture();
        let original = Task::default();
        let mut preview = original.clone();
        bind_subjects(&mut preview, &catalog, &mut intent).unwrap();
        bind_subjects(&mut preview, &catalog, &mut intent).unwrap();
        assert!(original.entities.is_empty() && original.facts.is_empty());
        assert_eq!(preview.entities.len(), 1);
        assert_eq!(preview.facts.len(), 1);
        assert_eq!(preview.facts[0].provenance, json!({"kind":"user_input"}));
        assert_eq!(preview.facts[0].verification, "user_asserted");
        assert!(preview.facts[0].expires_at > Utc::now() + chrono::Duration::minutes(59));
        assert!(matches!(
            crate::resolver::resolve(&catalog, &preview, &intent),
            crate::resolver::Resolution::Plan(_)
        ));
    }
    #[test]
    fn schema_and_entity_checks_remain_enforced() {
        let (catalog, mut intent) = fixture();
        intent.subjects[0]
            .values
            .insert("company.name".into(), json!({"not":"scalar"}));
        assert!(bind_subjects(&mut Task::default(), &catalog, &mut intent).is_err());
        let (_, mut intent) = fixture();
        intent.subjects[0].entity_ref = Some(Uuid::new_v4());
        assert!(bind_subjects(&mut Task::default(), &catalog, &mut intent).is_err());
    }
}
