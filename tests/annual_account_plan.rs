//! Opt-in model/compiler check. Reads catalog only; never approves or buys data.
use apiosk_gateway_v2::{
    catalog::{Catalog, Snapshot},
    parser::Parser,
    resolver::{resolve, Resolution},
    state::{Entity, Fact, Task},
};
use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;
#[tokio::test]
#[ignore = "reads production catalog and calls intent model; no data purchase"]
async fn simple_annual_account_request_compiles() {
    dotenvy::dotenv().ok();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&std::env::var("DATABASE_URL").unwrap())
        .await
        .unwrap();
    sqlx::query("SET search_path TO public, gateway_v2, kyb, providers, catalog, actions, billing, agents, wallets, research, integrations, observability, support, content, operations, identity, extensions").execute(&pool).await.unwrap();
    let value: Value = sqlx::query_scalar(include_str!("../src/catalog.sql"))
        .fetch_one(&pool)
        .await
        .unwrap();
    pool.close().await;
    let catalog = Catalog::build(serde_json::from_value::<Snapshot>(value).unwrap());
    let parser = Parser::new(
        reqwest::Client::new(),
        std::env::var("INTENT_API_URL").unwrap(),
        std::env::var("INTENT_API_KEY").unwrap(),
        std::env::var("INTENT_MODEL").unwrap(),
    );
    let question = std::env::var("PLAN_CHECK_QUESTION")
        .unwrap_or_else(|_| "ik wil graag de jaarrekening van Bitvavo".into());
    let mut task = Task::default();
    let mut intent = parser
        .parse(Uuid::new_v4(), &question, &task, &catalog)
        .await
        .unwrap();
    for s in &mut intent.subjects {
        let id = Uuid::new_v4();
        s.entity_ref = Some(id);
        task.entities.push(Entity {
            entity_ref: id,
            label: s.label.clone(),
            selected: true,
        });
        for (kind, value) in &s.values {
            task.facts.push(Fact {
                entity_ref: id,
                semantic_type: kind.clone(),
                value: value.clone(),
                provenance: json!({}),
                observed_at: Utc::now(),
                expires_at: Utc::now() + chrono::Duration::hours(1),
                verification: "user_asserted".into(),
            });
        }
    }
    println!("intent={}", serde_json::to_string(&intent).unwrap());
    let latest = question.contains("meest recente");
    assert_eq!(intent.goals.len(), 1);
    assert_eq!(intent.goals[0].capability, "company.financial-statements");
    if latest {
        assert_eq!(intent.goals[0].constraints["filing_selection"], "latest");
    } else {
        assert!(intent.goals[0].constraints.is_empty());
    }
    let resolution = resolve(&catalog, &task, &intent);
    if let Resolution::Unsupported(reason) = &resolution {
        println!("compiler={reason}");

        let review = parser
            .review_plan(&question, &task, &catalog, &intent, reason)
            .await
            .unwrap();
        assert!(review.intent.is_none());
        assert!(
            question.contains("meest recente"),
            "Plain annual-account request must compile"
        );
        println!("clarification={}", review.message);
        return;
    }
    assert!(
        !latest,
        "Latest must not silently resolve to an arbitrary filing"
    );
    assert!(matches!(resolution, Resolution::Plan(_)), "{resolution:?}");
}
