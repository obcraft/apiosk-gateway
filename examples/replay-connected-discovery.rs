//! Replay a captured free Ask response against a catalog snapshot locally.
//! No model, database, provider, task, approval or payment calls are made.
use apiosk_gateway_v2::{
    capability_intake::Intake,
    catalog::{Catalog, Snapshot},
    engine::bind_subjects,
    parser::connected_planning,
    resolver::{resolve, Resolution},
    state::Task,
};
use serde_json::{json, Value};

fn main() -> anyhow::Result<()> {
    let question = std::env::var("CAPABILITY_REPLAY_QUESTION")?;
    let captured: Value = serde_json::from_slice(&std::fs::read(std::env::var("CAPABILITY_REPLAY_CAPTURE")?)?)?;
    let snapshot: Snapshot = serde_json::from_slice(&std::fs::read(std::env::var("CAPABILITY_REPLAY_CATALOG")?)?)?;
    let catalog = Catalog::build(snapshot);
    let mut intake: Intake = serde_json::from_value(json!({
        "parsed_request":captured["parsed_request"], "matches":captured["matches"],
        "selected_capabilities":[], "catalog_version":catalog.version,
        "apiosk_sources_searched":captured["apiosk_sources_searched"],
    }))?;
    intake.selected_capabilities = intake.matches.iter().flat_map(|found| found.operations.iter().cloned()).collect();
    let prepared = connected_planning::compile(&question, &Task::default(), &catalog, &intake)
        .ok_or_else(|| anyhow::anyhow!("captured request has no deterministic connected plan"))?;
    let mut task = Task { capability_intake: Some(prepared.intake), ..Task::default() };
    task.record_turn(&question, None);
    let mut intent = prepared.reply.intent.ok_or_else(|| anyhow::anyhow!("missing intent"))?;
    bind_subjects(&mut task, &catalog, &mut intent)?;
    let Resolution::Plan(plan) = resolve(&catalog, &task, &intent) else {
        anyhow::bail!("connected plan does not resolve");
    };
    plan.validate_independent_inputs(&task, &catalog.vocabulary)?;
    println!("{}", serde_json::to_string_pretty(&json!({
        "boundary":"Offline replay; zero network, model, provider, task, approval or payment calls",
        "question":question, "cap_atomic":plan.cap_atomic,
        "gaps":prepared.coverage.gaps,
        "steps":plan.steps.iter().map(|step| json!({
            "source":step.service.api_slug, "capability":step.service.contract.capability_slug,
            "inputs":step.service.inputs(&task, step.entity_ref, &catalog.vocabulary).ok(),
            "price_atomic":plan.step_total(step),
        })).collect::<Vec<_>>(),
    }))?);
    Ok(())
}
