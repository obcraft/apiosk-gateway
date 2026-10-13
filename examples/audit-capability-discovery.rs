//! Real-model question -> capability -> source -> plan acceptance audit.
//! Reads catalog metadata in a read-only database session. Never creates a
//! task, approves a quote, executes a provider, or writes to the database.
//! CAPABILITY_AUDIT_DIR: output directory (default /private/tmp/apiosk-capability-discovery-audit).
//! CAPABILITY_AUDIT_CASES: optional comma-separated case ids.
//! CAPABILITY_AUDIT_RUN: output suffix (default baseline).
//! CAPABILITY_AUDIT_CATALOG: optional existing snapshot; otherwise reads live.
use apiosk_gateway_v2::{
    ask_v2,
    catalog::{Catalog, Snapshot, CATALOG_SQL},
    engine::bind_subjects,
    parser::{self, Parser},
    resolver::{resolve, Resolution},
    state::Task,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::BTreeSet, path::PathBuf, time::Instant};

#[derive(Deserialize)]
struct Case {
    id: String,
    question: String,
    expect_concepts: Vec<Vec<String>>,
    source_free: bool,
    named_source: Option<String>,
    #[serde(default)]
    expect_plan: bool,
    #[serde(default)]
    minimum_capabilities: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter("apiosk_gateway_v2::parser=info")
        .with_writer(std::io::stderr)
        .init();
    let dir = PathBuf::from(std::env::var("CAPABILITY_AUDIT_DIR")
        .unwrap_or_else(|_| "/private/tmp/apiosk-capability-discovery-audit".into()));
    std::fs::create_dir_all(&dir)?;
    let snapshot_path = std::env::var("CAPABILITY_AUDIT_CATALOG")
        .map(PathBuf::from).unwrap_or_else(|_| dir.join("catalog.json"));
    if !snapshot_path.exists() {
        let database_url = std::env::var("DATABASE_URL")?;
        let database_host = reqwest::Url::parse(&database_url)?.host_str().unwrap_or_default().to_owned();
        anyhow::ensure!(database_host.ends_with(".supabase.com") || database_host.ends_with(".supabase.co"), "audit catalog must use the configured Supabase database");
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(30))
            .after_connect(|connection, _| Box::pin(async move {
                sqlx::query("SET search_path TO public,catalog,actions,billing,providers,agents,wallets,research,integrations,observability,support,content,operations,identity,extensions").execute(&mut *connection).await?;
                sqlx::query("SET default_transaction_read_only=on").execute(&mut *connection).await?;
                sqlx::query("SET statement_timeout='25s'").execute(&mut *connection).await?;
                Ok(())
            })).connect(&database_url).await?;
        let raw: Value = sqlx::query_scalar(CATALOG_SQL).fetch_one(&pool).await?;
        pool.close().await;
        std::fs::write(&snapshot_path, serde_json::to_vec(&raw)?)?;
    }
    let snapshot: Snapshot = serde_json::from_slice(&std::fs::read(&snapshot_path)?)?;
    let catalog = Catalog::build(snapshot);
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build()?;
    let key = std::env::var("OPENROUTER_API_KEY").ok().filter(|s| !s.is_empty())
        .or_else(|| std::env::var("INTENT_API_KEY").ok())
        .ok_or_else(|| anyhow::anyhow!("model key required"))?;
    let model = std::env::var("CAPABILITY_AUDIT_MODEL").unwrap_or_else(|_| ask_v2::DEFAULT_MODEL.into());
    let parser = Parser::new(http.clone(),
        "https://openrouter.ai/api/v1/chat/completions".into(),
        key.clone(), parser::pinned_model("INTENT_MODEL"))
        .with_ask_v2(ask_v2::Client::new(http, key, model.clone()));
    let cases: Vec<Case> = serde_json::from_str(include_str!("../tests/fixtures/capability-discovery-questions.json"))?;
    let filter = std::env::var("CAPABILITY_AUDIT_CASES").ok();
    let suffix = std::env::var("CAPABILITY_AUDIT_RUN").unwrap_or_else(|_| "baseline".into());
    let mut results = vec![];
    for case in cases {
        if filter.as_ref().is_some_and(|ids| !ids.split(',').any(|id| id == case.id)) { continue; }
        let started = Instant::now();
        let mut report = json!({"case":case.id,"question":case.question,"model":model,
            "catalog_version":catalog.version,"catalog_services":catalog.services.len(),
            "boundary":"Live catalog metadata, real model parse/search/plan, in-memory bind and resolve; zero source execution, approval, payment, task persistence or database mutation.",
            "no_provider_execution":true,"assertions":{},"issues":[]});
        let mut issues = vec![];
        let mut task = Task::default();
        match parser.ask_intake(&case.question, &task, &catalog, None).await {
            Err(error) => { report["status"] = json!("intake_error"); report["error"] = json!(error.to_string()); issues.push("intake_error".to_owned()); }
            Ok(intake) => {
                report["intake"] = json!(intake);
                let hints: Vec<_> = intake.parsed_request.capabilities.iter().filter_map(|c| c.source_hint.as_deref()).collect();
                let source_ok = if case.source_free { hints.is_empty() }
                    else { case.named_source.as_ref().is_none_or(|named| !hints.is_empty() && hints.iter().all(|hint| hint.eq_ignore_ascii_case(named))) };
                report["assertions"]["source_hints_preserved"] = json!(source_ok);
                if !source_ok { issues.push("source_hint_invented_or_dropped".into()); }
                let concepts = intake.parsed_request.capabilities.iter().flat_map(|c|
                    std::iter::once(c.slug.as_str()).chain(c.outputs.iter().map(String::as_str)))
                    .collect::<Vec<_>>().join(" ").to_lowercase();
                let missing: Vec<_> = case.expect_concepts.iter().filter(|group|
                    !group.iter().any(|concept| concepts.contains(concept))).collect();
                report["assertions"]["requested_concepts_retained"] = json!(missing.is_empty());
                report["assertions"]["missing_concept_groups"] = json!(missing);
                if !missing.is_empty() { issues.push("requested_concept_lost".into()); }
                let categories_separate = intake.parsed_request.capabilities.len() >= case.minimum_capabilities;
                report["assertions"]["separate_categories_retained"] = json!(categories_separate);
                if !categories_separate { issues.push("requested_categories_collapsed".into()); }
                match parser.chat_from_capabilities(&case.question, None, &task, &catalog, &intake).await {
                    Err(error) => { report["status"] = json!("planner_error"); report["error"] = json!(error.to_string()); issues.push("planner_error".into()); }
                    Ok(reply) => {
                        report["reply"] = json!({"message":reply.message,"discover":reply.discover,
                            "decline_reason":reply.decline_reason,"intent":reply.intent,"output_bindings":reply.output_bindings});
                        if let Some(mut intent) = reply.intent {
                            // Match the connected MCP engine's mode before resolving;
                            // omitting intake here would exercise a different path.
                            task.capability_intake = Some(intake.clone());
                            task.record_turn(&case.question, None);
                            match bind_subjects(&mut task, &catalog, &mut intent) {
                                Err(error) => { report["status"] = json!("binding_error"); report["error"] = json!(error.to_string()); issues.push("binding_error".into()); }
                                Ok(()) => {
                                    report["intent"] = json!(intent);
                                    // Parser validates semantic field IDs, selected
                                    // source scope and complete output coverage.
                                    report["assertions"]["source_field_coverage_validated_by_parser"] = json!(true);
                                    match resolve(&catalog, &task, &intent) {
                                        Resolution::Plan(plan) => {
                                            let mut all_inputs_ready = true;
                                            let mut matched = true;
                                            let steps: Vec<_> = plan.steps.iter().map(|step| {
                                                let inputs = step.service.inputs(&task, step.entity_ref, &catalog.vocabulary);
                                                all_inputs_ready &= inputs.is_ok();
                                                matched &= intake.matches.iter().any(|found| found.apiosk.iter().any(|candidate|
                                                    candidate.availability == "supported" && candidate.endpoint_id.as_deref() == Some(step.service.endpoint_id.to_string().as_str())));
                                                json!({"source":step.service.api_slug,"capability":step.service.contract.capability_slug,
                                                    "endpoint_id":step.service.endpoint_id,"inputs":inputs.ok(),
                                                    "input_requirements":step.service.contract.requires,"total_atomic":step.service.total()})
                                            }).collect();
                                            let actual: BTreeSet<_> = plan.steps.iter().map(|s| &s.service.api_slug).collect();
                                            let one_cap = plan.cap_atomic == plan.steps.iter().map(|s| s.service.total()).sum::<i64>();
                                            report["status"] = json!("plan_ready");
                                            report["plan"] = json!({"cap_atomic":plan.cap_atomic,"steps":steps,"sources":actual});
                                            report["assertions"]["all_inputs_available"] = json!(all_inputs_ready);
                                            report["assertions"]["steps_from_discovered_executable_endpoints"] = json!(matched);
                                            report["assertions"]["single_cap_equals_steps"] = json!(one_cap);
                                            if !all_inputs_ready { issues.push("plan_has_missing_inputs".into()); }
                                            if !matched { issues.push("plan_uses_undiscovered_endpoint".into()); }
                                            if !one_cap { issues.push("price_cap_mismatch".into()); }
                                        }
                                        Resolution::Missing { kind, .. } => {
                                            report["status"] = json!("needs_input"); report["missing_input"] = json!(kind);
                                        }
                                        Resolution::Unsupported(reason) => {
                                            report["status"] = json!("unsupported"); report["reason"] = json!(reason);
                                        }
                                    }
                                }
                            }
                        } else {
                            report["status"] = json!(if intake.matches.iter().any(|m| m.operations.is_empty()) {"no_executable_coverage"} else {"clarification"});
                        }
                    }
                }
            }
        }
        if case.expect_plan && report["status"] != "plan_ready" { issues.push("known_search_regression_did_not_plan".into()); }
        report["expected_plan"] = json!(case.expect_plan);
        report["issues"] = json!(issues);
        report["assertions_passed"] = json!(issues.is_empty());
        report["duration_ms"] = json!(started.elapsed().as_millis());
        std::fs::write(dir.join(format!("{}-{suffix}.json", case.id)), serde_json::to_vec_pretty(&report)?)?;
        println!("{}: {} {}ms issues={}", case.id, report["status"], started.elapsed().as_millis(), report["issues"]);
        results.push(report);
    }
    std::fs::write(dir.join(format!("results-{suffix}.json")), serde_json::to_vec_pretty(&results)?)?;
    anyhow::ensure!(results.iter().all(|r| r["assertions_passed"] == true), "one or more capability-discovery assertions failed; see audit output");
    Ok(())
}
