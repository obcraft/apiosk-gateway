//! Live intent-model regression check; fixture catalog, no source purchases.
use apiosk_gateway_v2::{
    catalog::Catalog,
    parser::Parser,
    resolver::{resolve, Resolution},
    state::{Fact, Task},
};
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let catalog = Catalog::build(serde_json::from_str(include_str!(
        "../tests/fixtures/supplier-check/catalog.json"
    ))?);
    let parser = Parser::new(
        reqwest::Client::new(),
        "https://openrouter.ai/api/v1/chat/completions".into(),
        std::env::var("INTENT_API_KEY")?,
        std::env::var("INTENT_MODEL")?,
    );
    let question = "What should we flag before onboarding Mollie B.V. in the Netherlands? Find its KVK number through company-name search, then check its company registration, sanctions matches and domain history. Give me a short conclusion backed by the sources.";
    for (label, question, chat, restricted, domain) in [
        ("screenshot", question.to_owned(), false, false, false),
        ("with-domain", question.replace("Netherlands?", "Netherlands (mollie.com)?"), false, false, true),
        ("chat-with-domain", question.replace("Netherlands?", "Netherlands (mollie.com)?"), true, false, true),
        ("explicit-list-limit", "Screen Mollie B.V. only against Dutch sanctions lists.".into(), false, true, false),
    ] {
        let mut intent = if chat {
            parser.chat(&question, &Task::default(), &catalog).await?.intent
                .ok_or_else(|| anyhow::anyhow!("unexpected clarification"))?
        } else {
            parser.parse(Uuid::new_v4(), &question, &Task::default(), &catalog).await?
        };
        println!("{label}: {}", serde_json::to_string(&intent)?);
        let sanctions = intent.goals.iter().find(|g| g.capability == "company.sanctions-screen")
            .ok_or_else(|| anyhow::anyhow!("missing sanctions goal"))?;
        if restricted {
            anyhow::ensure!(!sanctions.constraints.is_empty() || !intent.unresolved_references.is_empty(), "explicit screening restriction was lost");
        } else {
            anyhow::ensure!(!sanctions.constraints.contains_key("jurisdiction"), "entity country leaked into screening scope");
            anyhow::ensure!(intent.unresolved_references.is_empty(), "unexpected clarification");
            anyhow::ensure!(intent.goals.iter().any(|g| g.capability == "company.registry-profile"), "profile goal lost");
            anyhow::ensure!(intent.goals.iter().any(|g| g.capability == "domain.registration" && !g.constraints.contains_key("jurisdiction")), "domain scope changed");
        }
        let mut task = Task::default();
        for subject in &mut intent.subjects {
            anyhow::ensure!(!subject.values.contains_key("company_registry.kvknummer"), "invented KVK number");
            if !domain {
                anyhow::ensure!(!subject.values.contains_key("domain.name"), "invented domain");
            }
            let entity = Uuid::new_v4();
            subject.entity_ref = Some(entity);
            for (kind, value) in &subject.values {
                task.facts.push(Fact {
                    entity_ref: entity, semantic_type: kind.clone(), value: value.clone(),
                    provenance: json!({"kind":"user_input"}), observed_at: Utc::now(),
                    expires_at: Utc::now() + chrono::Duration::hours(1), verification: "user_asserted".into(),
                });
            }
        }
        match resolve(&catalog, &task, &intent) {
            Resolution::Plan(plan) if domain => {
                let capabilities: Vec<_> = plan.steps.iter().map(|s| s.service.contract.capability_slug.as_str()).collect();
                let search = capabilities.iter().position(|c| *c == "company_registry.search").expect("missing name lookup");
                let profile = capabilities.iter().position(|c| *c == "company.registry-profile").expect("missing profile");
                anyhow::ensure!(search < profile, "profile before name lookup");
                anyhow::ensure!(plan.steps.iter().map(|s| &s.service.api_slug).collect::<std::collections::HashSet<_>>().len() == 3, "expected three sources");
            }
            Resolution::Missing { kind, .. } if !restricted && !domain => anyhow::ensure!(kind == "domain.name", "wrong missing input: {kind}"),
            Resolution::Unsupported(_) if restricted => (),
            other => anyhow::bail!("unexpected resolution: {other:?}"),
        }
    }
    println!("PASS: name lookup dependency, three sources, and explicit screening restrictions preserved. No source calls or purchases.");
    Ok(())
}
