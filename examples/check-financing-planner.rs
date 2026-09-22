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
    let pool=sqlx::postgres::PgPoolOptions::new().max_connections(1).after_connect(|c,_|Box::pin(async move{
  sqlx::query("SET search_path TO public,gateway_v2,kyb,providers,catalog,actions,billing,agents,wallets,research,integrations,observability,support,content,operations,identity,extensions").execute(&mut *c).await?;
  sqlx::query("SET default_transaction_read_only=on").execute(&mut *c).await?;Ok(())
 })).connect(&std::env::var("DATABASE_URL")?).await?;
    let raw: Value = sqlx::query_scalar(CATALOG_SQL).fetch_one(&pool).await?;
    pool.close().await;
    let mut raw = raw;
    // Model the accompanying migration before applying it to production.
    for s in raw["services"].as_array_mut().unwrap() {
        let cap = s["contract"]["capability_slug"]
            .as_str()
            .unwrap_or("")
            .to_owned();
        if cap == "company.registered-address-context" {
            for kind in [
                "source.bodemloket.b_b_o_x",
                "source.pdok_ruimtelijke_plannen.b_b_o_x",
            ] {
                s["contract"]["output_map"]["produces"][kind] =
                    json!({"adapter":"point.rd-extent","key":"/source_record/centroide_rd"});
                if !s["optional_outputs"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(kind))
                {
                    s["optional_outputs"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!(kind));
                }
            }
        }
        if ["soil.bodemloket.feature-info", "planning.pdok.feature-info"].contains(&cap.as_str()) {
            for (kind, binding) in s["contract"]["input_map"].as_object_mut().unwrap() {
                if kind.ends_with(".l_a_y_e_r_s") {
                    binding["default"] = json!(if cap.starts_with("soil") {
                        "Beschikbaarheid_gegevens"
                    } else {
                        "enkelbestemming"
                    });
                }
            }
            for r in s["contract"]["requires"].as_array_mut().unwrap() {
                if r["fact_type"].as_str().unwrap().ends_with(".l_a_y_e_r_s") {
                    r["optional"] = json!(true);
                }
            }
        }
    }
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
    let default_question="Prepare a financing report on ASML Holding N.V. Find and verify its registered address, assess the company’s finances and property risks, and check property ownership. Include available maps, charts and reports, with a short sourced conclusion and any information gaps.";
    let question = std::env::var("DEMO_QUESTION").unwrap_or(default_question.into());
    let question = question.as_str();
    let mut task = Task::default();
    let mut intent = parser
        .parse(uuid::Uuid::new_v4(), question, &task, &catalog)
        .await?;
    apiosk_gateway_v2::engine::bind_subjects(&mut task, &catalog, &mut intent)?;
    println!("{}", serde_json::to_string_pretty(&intent)?);
    match resolver::resolve(&catalog, &task, &intent) {
        resolver::Resolution::Plan(plan) => {
            println!("READY: {} source calls; no purchase", plan.steps.len())
        }
        other => anyhow::bail!("not ready: {other:?}"),
    }
    Ok(())
}
