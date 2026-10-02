//! Repeatable, model-only profession acceptance audit. Reads catalog metadata;
//! creates no task and never executes, approves or purchases a source request.
use apiosk_gateway_v2::{
    ask_v2, catalog::{Catalog, Snapshot}, engine::bind_subjects,
    parser::{self, Parser}, resolver::{resolve, Resolution}, state::Task,
};
use serde_json::{json, Value};
use std::{path::PathBuf, time::Instant};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt().with_env_filter("apiosk_gateway_v2::parser=info").with_writer(std::io::stderr).init();
    let dir = PathBuf::from(std::env::var("PROFESSION_AUDIT_DIR")?);
    let cases: Vec<Value> = serde_json::from_slice(&std::fs::read(dir.join("cases.json"))?)?;
    let snapshot_path = std::env::var("CATALOG_SNAPSHOT")?;
    let snapshot: Snapshot = serde_json::from_slice(&std::fs::read(&snapshot_path)?)?;
    let catalog = Catalog::build(snapshot);
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build()?;
    let key = std::env::var("OPENROUTER_API_KEY").ok().filter(|s| !s.is_empty())
        .or_else(|| std::env::var("INTENT_API_KEY").ok()).ok_or_else(|| anyhow::anyhow!("model key required"))?;
    let model = parser::pinned_model("INTENT_MODEL");
    let parser = Parser::new(http.clone(), std::env::var("INTENT_API_URL").unwrap_or_else(|_| "https://openrouter.ai/api/v1/chat/completions".into()), key.clone(), model.clone())
        .with_ask_v2(ask_v2::Client::new(http, key, parser::pinned_model("ASK_V2_MODEL")));
    let full = std::env::var("PROFESSION_FULL").as_deref() == Ok("1");
    let only = std::env::var("PROFESSION_CASES").ok();
    let suffix = std::env::var("PROFESSION_RUN").unwrap_or_else(|_| "baseline".into());
    let mut results = vec![];
    for case in cases {
        let number = case["case"].as_u64().unwrap();
        if only.as_ref().is_some_and(|ids| !ids.split(',').any(|id| id.parse::<u64>() == Ok(number))) { continue; }
        let question = case[if full {"full_question"} else {"question"}].as_str().unwrap();
        let started = Instant::now();
        let mut report = json!({"case":number,"profession":case["profession"],"question":question,
            "variant":if full {"full"} else {"core"},"catalog_version":catalog.version,
            "catalog_services":catalog.services.len(),"model":model,
            "boundary":"Read-only catalog, real model, actual connector intake/search/planner and in-memory resolver; no provider calls, task persistence, approval, payment or execution.",
            "known_full_workflow_gaps":case["known_full_workflow_gaps"]});
        let mut task = Task::default();
        match parser.ask_intake(question, &task, &catalog, None).await {
            Err(error) => { report["status"] = json!("intake_error"); report["error"] = json!(error.to_string()); }
            Ok(intake) => {
                report["intake"] = json!(intake);
                match parser.chat_from_capabilities(question, None, &task, &catalog, &intake).await {
                    Err(error) => { report["status"] = json!("planner_error"); report["error"] = json!(error.to_string()); }
                    Ok(reply) => {
                        report["reply"] = json!({"message":reply.message,"discover":reply.discover,"decline_reason":reply.decline_reason,"intent":reply.intent});
                        if let Some(mut intent) = reply.intent {
                            task.record_turn(question, None);
                            match bind_subjects(&mut task, &catalog, &mut intent) {
                                Err(error) => {report["status"] = json!("binding_error"); report["error"] = json!(error.to_string());}
                                Ok(()) => {
                                    report["intent"] = json!(intent);
                                    report["facts"] = json!(task.facts);
                                    match resolve(&catalog, &task, &intent) {
                                        Resolution::Plan(plan) => {
                                            let steps: Vec<_> = plan.steps.iter().map(|step| json!({
                                                "source":step.service.api_slug,"capability":step.service.contract.capability_slug,
                                                "method":step.service.method,"path":step.service.path,"entity_ref":step.entity_ref,
                                                "goal_index":step.goal_index,"input_requirements":step.service.contract.requires,
                                                "inputs":step.service.inputs(&task, step.entity_ref, &catalog.vocabulary).ok(),
                                                "input_map":step.service.contract.input_map,
                                                "total_atomic":step.service.total()})).collect();
                                            report["status"] = json!("quoted");
                                            report["plan"] = json!({"cap_atomic":plan.cap_atomic,"steps":steps,
                                                "single_cap_equals_steps":plan.cap_atomic == plan.steps.iter().map(|s| s.service.total()).sum::<i64>()});
                                            if !full {
                                                let missing: Vec<_> = case["required_capability_groups"].as_array().unwrap().iter()
                                                    .filter(|group| !group.as_array().unwrap().iter().any(|cap|
                                                        intent.goals.iter().any(|g| Some(g.capability.as_str()) == cap.as_str())))
                                                    .cloned().collect();
                                                report["semantic_assertions"] = json!({"all_expected_goals_present":missing.is_empty(),
                                                    "missing_goal_alternatives":missing,"no_pulse_primary_substitution":plan.steps.iter().all(|s| !s.service.api_slug.to_lowercase().contains("pulse"))});
                                            }
                                        }
                                        Resolution::Missing { kind, .. } => {report["status"] = json!("needs_input"); report["missing_input"] = json!(kind);}
                                        Resolution::Unsupported(reason) => {report["status"] = json!("unsupported"); report["reason"] = json!(reason);}
                                    }
                                }
                            }
                        } else { report["status"] = json!("clarification"); }
                    }
                }
            }
        }
        report["duration_ms"] = json!(started.elapsed().as_millis());
        let filename = format!("case-{number:02}-{}-{suffix}.json", if full {"full"} else {"core"});
        std::fs::write(dir.join(filename), serde_json::to_vec_pretty(&report)?)?;
        println!("case {number:02} {} {}ms", report["status"], started.elapsed().as_millis());
        results.push(report);
    }
    std::fs::write(dir.join(format!("{}-{suffix}.json", if full {"full"} else {"core"})), serde_json::to_vec_pretty(&results)?)?;
    Ok(())
}
