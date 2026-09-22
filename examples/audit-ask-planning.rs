//! Uses the configured model and a read-only catalog snapshot. No source calls,
//! task writes, approvals or billing. Reports both App and connector planning.
use apiosk_gateway_v2::{catalog::{Catalog, Snapshot}, engine::bind_subjects, parser::Parser, resolver::{resolve, Resolution}, state::Task};
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt().with_env_filter("apiosk_gateway_v2::parser=info").init();
    let snapshot: Snapshot = serde_json::from_slice(&std::fs::read(std::env::var("CATALOG_SNAPSHOT")?)?)?;
    let catalog = Catalog::build(snapshot);
    let parser = Parser::new(reqwest::Client::new(), std::env::var("INTENT_API_URL")?, std::env::var("INTENT_API_KEY")?, std::env::var("INTENT_MODEL")?);
    let questions: Vec<String> = serde_json::from_str(&std::env::var("AUDIT_QUESTIONS")?)?;
    for question in questions {
        let mut task = Task::default();
        let result = if std::env::var("AUDIT_SURFACE").as_deref() == Ok("connector") {
            parser.parse(uuid::Uuid::new_v4(), &question, &task, &catalog).await
        } else {
            parser.chat(&question, &task, &catalog).await.and_then(|reply| reply.intent.ok_or_else(|| anyhow::anyhow!("Clarification: {}", reply.message)))
        };
        match result {
            Ok(mut intent) => {
                task.record_turn(&question, None);
                bind_subjects(&mut task, &catalog, &mut intent)?;
                let result = match resolve(&catalog, &task, &intent) {
                    Resolution::Plan(plan) => json!({"status":"quoted","cap_atomic":plan.cap_atomic,"steps":plan.steps.iter().map(|s|json!({"source":s.service.api_slug,"capability":s.service.contract.capability_slug})).collect::<Vec<_>>()}),
                    other => json!({"status":"needs_input","reason":format!("{other:?}")}),
                };
                println!("{}", json!({"question":question,"intent":intent,"result":result}));
            }
            Err(error) => println!("{}", json!({"question":question,"error":error.to_string()})),
        }
    }
    Ok(())
}
