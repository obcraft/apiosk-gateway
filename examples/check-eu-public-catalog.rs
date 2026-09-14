//! Verify a saved production catalog with the real intent model; no source purchases.
use apiosk_gateway_v2::{
    catalog::{Catalog, Snapshot},
    parser::Parser,
    resolver::{resolve, Resolution},
    state::{Fact, Task},
};
use serde_json::json;
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter("apiosk_gateway_v2=debug")
        .init();
    let file = std::env::var("EU_CATALOG_SNAPSHOT")?;
    let snap: Snapshot = serde_json::from_slice(&std::fs::read(file)?)?;
    let c = Catalog::build(snap);
    let expected = [
        "vat.eu.validate",
        "vat.eu.check",
        "vat.eu.service-status",
        "government.staatscourant.search",
        "government.cvdr.search",
        "government.bwb.search",
        "eu_law.sparql.select",
        "eu_law.celex.metadata",
        "eu_funding.calls.search",
        "planning.pdok.feature-info",
        "soil.bodemloket.feature-info",
    ];
    for cap in expected {
        anyhow::ensure!(
            c.by_capability.contains_key(cap),
            "Missing {cap}; rejected={:?}",
            c.rejected
        );
    }
    let parser = Parser::new(
        reqwest::Client::new(),
        std::env::var("INTENT_API_URL")?,
        std::env::var("INTENT_API_KEY")?,
        std::env::var("INTENT_MODEL")?,
    );
    for (question,cap) in [
 ("Controleer via VIES het btw-nummer 004495445B01 voor landcode NL.","vat.eu.validate"),
 ("Zoek in de Staatscourant via KOOP naar water, maximaal 2 resultaten.","government.staatscourant.search"),
 ("Zoek via KOOP CVDR naar lokale regelingen met water in de titel, maximaal 2 resultaten.","government.cvdr.search"),
 ("Haal bij EUR-Lex Cellar de metadata op voor CELEX 32016R0679.","eu_law.celex.metadata"),
 ("Zoek via EU Funding & Tenders naar energy, maximaal 2 Engelstalige resultaten.","eu_funding.calls.search"),
 ("Gebruik PDOK Ruimtelijke Plannen voor de laag enkelbestemming met RD BBOX 120900,487200,121000,487300 en pixel I=50 J=50 in een beeld van 101 bij 101 pixels.","planning.pdok.feature-info"),
 ("Gebruik Bodemloket voor de laag Beschikbaarheid_gegevens met RD BBOX 120900,487200,121000,487300 en pixel I=50 J=50 in een beeld van 101 bij 101 pixels.","soil.bodemloket.feature-info")]{
  let mut task=Task::default();let mut intent=parser.parse(uuid::Uuid::new_v4(),question,&task,&c).await?;
  // VIES offers the same validation by GET or POST; either is acceptable.
  anyhow::ensure!(intent.goals.iter().any(|g|g.capability==cap||(cap=="vat.eu.validate"&&g.capability=="vat.eu.check")),"Unexpected goal for {question}: {intent:?}");
  for subject in &mut intent.subjects{let id=uuid::Uuid::new_v4();subject.entity_ref=Some(id);for(k,v)in &subject.values{task.facts.push(Fact{entity_ref:id,semantic_type:k.clone(),value:v.clone(),provenance:json!({"kind":"user_input"}),observed_at:chrono::Utc::now(),expires_at:chrono::Utc::now()+chrono::Duration::hours(1),verification:"user_asserted".into()});}}
  let Resolution::Plan(plan)=resolve(&c,&task,&intent)else{anyhow::bail!("No plan for {question}: {intent:?}")};
  anyhow::ensure!(plan.cap_atomic==97826,"Wrong quote for {question}: {}",plan.cap_atomic);
  for step in &plan.steps{let inputs=step.service.inputs(&task,step.entity_ref,&c.vocabulary)?;println!("{}",json!({"question":question,"capability":step.service.contract.capability_slug,"inputs":inputs,"total_micro_usd":plan.cap_atomic,"passed":true}));}
 }
    Ok(())
}
