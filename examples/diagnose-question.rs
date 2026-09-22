//! Read-only live catalog + configured intent model. No provider calls or approval.
use apiosk_gateway_v2::{
    catalog::{Catalog, Snapshot, CATALOG_SQL},
    parser::Parser,
    resolver,
    state::Task,
};
use serde_json::{json, Value};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt().with_env_filter("apiosk_gateway_v2::parser=info").init();
    let pool=sqlx::postgres::PgPoolOptions::new().max_connections(1).after_connect(|c,_|Box::pin(async move{
  sqlx::query("SET search_path TO public,gateway_v2,kyb,providers,catalog,actions,billing,agents,wallets,research,integrations,observability,support,content,operations,identity,extensions").execute(&mut *c).await?;
  sqlx::query("SET default_transaction_read_only=on").execute(&mut *c).await?;Ok(())
 })).connect(&std::env::var("DATABASE_URL")?).await?;
    let raw: Value = sqlx::query_scalar(CATALOG_SQL).fetch_one(&pool).await?;
    pool.close().await;
    let snapshot: Snapshot = serde_json::from_value(raw)?;
    let catalog = Catalog::build(snapshot);
    for cap in [
        "company.registered-address-context",
        "soil.bodemloket.feature-info",
        "planning.pdok.feature-info",
    ] {
        anyhow::ensure!(
            catalog.by_capability.contains_key(cap),
            "missing {cap}; rejected {:?}",
            catalog.rejected
        );
    }
    let parser = Parser::new(
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        std::env::var("INTENT_API_URL")?,
        std::env::var("INTENT_API_KEY")?,
        std::env::var("INTENT_MODEL")?,
    );
    let default_question="Create a company and location report on ASML Holding N.V. Check its registration, sanctions and website asml.com. Find its registered address and include building and parcel maps, available soil and planning information, and the latest five relevant news articles. Find the identifiers yourself and give a short sourced conclusion.";
    let question = std::env::var("DEMO_QUESTION").unwrap_or(default_question.into());
    let question = question.as_str();
    let mut task = Task::default();
    task.record_turn(question, None);
    if std::env::var("INSPECT_INTENT").as_deref() == Ok("1") {
        let response: Value = reqwest::Client::new().post(std::env::var("INTENT_API_URL")?)
            .bearer_auth(std::env::var("INTENT_API_KEY")?)
            .json(&json!({"model":std::env::var("INTENT_MODEL")?,"temperature":0,"max_tokens":4096,
                "reasoning":{"effort":"minimal","exclude":true},"provider":{"require_parameters":true},
                "response_format":{"type":"json_object"},"messages":[
                {"role":"system","content":include_str!("../contracts/parser-prompt.txt")},
                {"role":"user","content":json!({"question":question,"context":task.parser_context(),"catalog":catalog.parser_view()}).to_string()}]}))
            .send().await?.error_for_status()?.json().await?;
        let raw=response.pointer("/choices/0/message/content").and_then(Value::as_str).unwrap_or("");
        println!("RAW INTENT: {raw}");
        let result=serde_json::from_str::<apiosk_gateway_v2::parser::Intent>(raw).map_err(anyhow::Error::from)
            .and_then(|intent| intent.validate(question,&task,&catalog));
        println!("VALIDATION: {result:?}");
        return Ok(());
    }
    let mut intent = parser
        .parse(uuid::Uuid::new_v4(), question, &task, &catalog)
        .await?;
    apiosk_gateway_v2::engine::bind_subjects(&mut task, &catalog, &mut intent)?;
    println!("{}", serde_json::to_string_pretty(&intent)?);
    match resolver::resolve(&catalog, &task, &intent) {
        resolver::Resolution::Plan(plan) => {
            for step in &plan.steps { println!("{}: {}{}", step.service.api_slug, step.service.contract.capability_slug, if step.browser_fallback.is_some() { " (conditional fallback, included in cap)" } else { "" }); }
            println!("READY: {} source calls; no purchase", plan.steps.len())
        }
        other => anyhow::bail!("not ready: {other:?}"),
    }
    Ok(())
}
