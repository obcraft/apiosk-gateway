//! Repeatable multi-source example audit. Reads live catalog metadata and models;
//! creates no task and never executes, approves or purchases a source request.
use apiosk_gateway_v2::{
    ask_v2, catalog::{Catalog, Snapshot, CATALOG_SQL}, engine::bind_subjects,
    parser::{self, Parser}, resolver::{resolve, Resolution}, state::Task,
};
use serde_json::{json, Value};
use std::{path::PathBuf, time::Instant};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt().with_env_filter("apiosk_gateway_v2::parser=info").with_writer(std::io::stderr).init();
    let dir = PathBuf::from(std::env::var("MULTISOURCE_AUDIT_DIR").unwrap_or_else(|_| "/private/tmp/apiosk-multisource-audit".into()));
    std::fs::create_dir_all(&dir)?;
    let cases_path = std::env::var("MULTISOURCE_QUESTIONS").unwrap_or_else(|_| "/private/tmp/apiosk-multisource-example-draft.json".into());
    let cases: Vec<Value> = serde_json::from_slice(&std::fs::read(cases_path)?)?;
    let snapshot_path = dir.join("catalog.json");
    if std::env::var("MULTISOURCE_REFRESH_CATALOG").as_deref() == Ok("1") || !snapshot_path.exists() {
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(30))
            .after_connect(|connection, _| Box::pin(async move {
                sqlx::query("SET search_path TO public,catalog,actions,billing,providers,agents,wallets,research,integrations,observability,support,content,operations,identity,extensions").execute(&mut *connection).await?;
                sqlx::query("SET default_transaction_read_only=on").execute(&mut *connection).await?;
                sqlx::query("SET statement_timeout='25s'").execute(&mut *connection).await?;
                Ok(())
            })).connect(&std::env::var("DATABASE_URL")?).await?;
        let raw: Value = sqlx::query_scalar(CATALOG_SQL).fetch_one(&pool).await?;
        pool.close().await;
        std::fs::write(&snapshot_path, serde_json::to_vec(&raw)?)?;
    }
    if std::env::var("MULTISOURCE_SNAPSHOT_ONLY").as_deref() == Ok("1") {
        println!("Read-only catalog snapshot saved");
        return Ok(());
    }
    let snapshot: Snapshot = serde_json::from_slice(&std::fs::read(&snapshot_path)?)?;
    let catalog = Catalog::build(snapshot);
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build()?;
    let key = std::env::var("OPENROUTER_API_KEY").ok().filter(|s| !s.is_empty())
        .or_else(|| std::env::var("INTENT_API_KEY").ok()).ok_or_else(|| anyhow::anyhow!("model key required"))?;
    let model = parser::pinned_model("INTENT_MODEL");
    let parser = Parser::new(http.clone(), std::env::var("INTENT_API_URL").unwrap_or_else(|_| "https://openrouter.ai/api/v1/chat/completions".into()), key.clone(), model.clone())
        .with_ask_v2(ask_v2::Client::new(http, key, parser::pinned_model("ASK_V2_MODEL")));
    let full = false;
    let only = std::env::var("MULTISOURCE_CASES").ok();
    let suffix = std::env::var("MULTISOURCE_RUN").unwrap_or_else(|_| "baseline".into());
    let mut results = vec![];
    for (index, case) in cases.into_iter().enumerate() {
        let number = (index + 1) as u64;
        if only.as_ref().is_some_and(|ids| !ids.split(',').any(|id| id.parse::<u64>() == Ok(number))) { continue; }
        let question = case["prompt"].as_str().unwrap();
        let started = Instant::now();
        let mut report = json!({"case":number,"title":case["title"],"expected_sources":case["sources"],"question":question,
            "variant":if full {"full"} else {"core"},"catalog_version":catalog.version,
            "catalog_services":catalog.services.len(),"model":model,
            "boundary":"Read-only catalog, real model, actual connector intake/search/planner and in-memory resolver; no provider calls, task persistence, approval, payment or execution.",
            "no_provider_execution":true});
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
                                            let expected: Vec<_> = case["sources"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
                                            let actual: std::collections::BTreeSet<_> = plan.steps.iter().map(|s| s.service.api_slug.as_str()).collect();
                                            let missing: Vec<_> = expected.iter().filter(|source| !actual.contains(**source)).collect();
                                            report["semantic_assertions"] = json!({"all_expected_sources_present":missing.is_empty(),
                                                "missing_sources":missing,"actual_sources":actual,"distinct_source_count":actual.len(),
                                                "multiple_sources":actual.len() >= 2});
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
