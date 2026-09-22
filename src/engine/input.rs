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
    for (index, subject) in intent.subjects.iter_mut().enumerate() {
        // A requested company web/news search can use the literal company name.
        // Do not ask the buyer to retype it as a search query, invent search
        // terms/identifiers, or overwrite a more specific supplied query.
        if let Some(name) = subject.values.get("company.name").cloned() {
            for goal in intent.goals.iter().filter(|g| g.subject == index && (g.capability.starts_with("web.") || g.capability == "news.reuters-search")) {
                for service in catalog.services.iter().filter(|s|
                    s.contract.capability_slug == goal.capability
                    && goal.constraints.get("source").is_none_or(|source| source.as_str() == Some(s.api_slug.as_str()))) {
                    for requirement in &service.contract.requires {
                        if ["web.query", "web.source_search_query", "news.reuters_search_query"].contains(&requirement.fact_type.as_str()) {
                            subject.values.entry(requirement.fact_type.clone()).or_insert_with(|| name.clone());
                        }
                    }
                }
            }
        }
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

    #[test]
    fn company_news_search_uses_name_without_requesting_it_again() {
        let catalog = Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/workflows/catalog.json")).unwrap());
        for explicit in [None, Some("ASML export controls")] {
            let mut intent: Intent = serde_json::from_value(json!({"subjects":[{"entity_ref":null,"label":"ASML Holding N.V.","values":{"company.name":"ASML Holding N.V."}}],"goals":[{"capability":"news.reuters-search","subject":0,"constraints":{}}],"answer_operations":[],"unresolved_references":[]})).unwrap();
            if let Some(query)=explicit { intent.subjects[0].values.insert("news.reuters_search_query".into(),json!(query)); }
            let mut task=Task::default();
            bind_subjects(&mut task,&catalog,&mut intent).unwrap();
            assert_eq!(intent.subjects[0].values["news.reuters_search_query"], explicit.unwrap_or("ASML Holding N.V."));
            assert!(matches!(crate::resolver::resolve(&catalog,&task,&intent),crate::resolver::Resolution::Plan(_)));
        }
    }
    #[test]
    fn company_search_reuses_each_literal_name_without_overwriting_queries_or_identifiers() {
        let catalog = Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/workflows/catalog.json")).unwrap());
        let service = catalog.services.iter().find(|s| s.api_slug == "exa" && s.contract.requires.iter().any(|r| r.fact_type == "web.query")).unwrap();
        let mut intent: Intent = serde_json::from_value(json!({
            "subjects":[
                {"entity_ref":null,"label":"ASML Holding N.V.","values":{"company.name":"ASML Holding N.V."}},
                {"entity_ref":null,"label":"Mollie B.V.","values":{"company.name":"Mollie B.V.","web.query":"Mollie B.V. news"}}
            ],
            "goals":[
                {"capability":service.contract.capability_slug,"subject":0,"constraints":{"source":"exa"}},
                {"capability":service.contract.capability_slug,"subject":1,"constraints":{"source":"exa"}}
            ],"answer_operations":[],"unresolved_references":[]
        })).unwrap();
        let mut task = Task::default();
        bind_subjects(&mut task, &catalog, &mut intent).unwrap();
        assert_eq!(intent.subjects[0].values["web.query"], "ASML Holding N.V.");
        assert_eq!(intent.subjects[1].values["web.query"], "Mollie B.V. news");
        assert!(intent.subjects.iter().all(|s| !s.values.contains_key("company_registry.kvknummer") && !s.values.contains_key("company.registration.lei")));
        let crate::resolver::Resolution::Plan(plan) = crate::resolver::resolve(&catalog, &task, &intent) else { panic!("search should not ask for an already supplied company name") };
        assert_eq!(plan.steps.len(), 2);

        // Unrelated goals and searches without a company name keep their
        // original requirements; no guessed topic is inserted.
        let (catalog, mut intent) = fixture();
        bind_subjects(&mut Task::default(), &catalog, &mut intent).unwrap();
        assert!(!intent.subjects[0].values.contains_key("web.query"));
    }
}
