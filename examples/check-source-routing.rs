//! Read-only live metadata + configured intent model. No provider purchases.
use apiosk_gateway_v2::{catalog::{Catalog,Snapshot,CATALOG_SQL},parser::Parser,state::Task,engine::bind_subjects,resolver::{resolve,Resolution}};
use serde_json::Value;
#[tokio::main]
async fn main()->anyhow::Result<()> {
 dotenvy::dotenv().ok();
 tracing_subscriber::fmt().with_env_filter("apiosk_gateway_v2::parser=info").init();
 let pool=sqlx::postgres::PgPoolOptions::new().max_connections(1).after_connect(|c,_|Box::pin(async move{
  sqlx::query("SET search_path TO public,gateway_v2,kyb,providers,catalog,actions,billing,agents,wallets,research,integrations,observability,support,content,operations,identity,extensions").execute(&mut *c).await?;
  sqlx::query("SET default_transaction_read_only=on").execute(&mut *c).await?;Ok(())
 })).connect(&std::env::var("DATABASE_URL")?).await?;
 let audit_staged=std::env::args().any(|s|s=="--audit-database-staged");
 let sql=if audit_staged {CATALOG_SQL.replace("WHERE c.status='active' AND c.validated_at", "WHERE c.status='validated' AND c.adapter_version='source-document-v1' AND c.validated_at")}else{CATALOG_SQL.to_owned()};
 let raw:Value=sqlx::query_scalar(&sql).fetch_one(&pool).await?;pool.close().await;
 let mut snapshot:Snapshot=serde_json::from_value(raw)?;
 if std::env::args().any(|s|s=="--staged") {
  let added:Snapshot=serde_json::from_str(include_str!("../tests/fixtures/source-operations/catalog.json"))?;
  for service in added.services {if !snapshot.services.iter().any(|s|s.id==service.id){snapshot.services.push(service);}}
  for word in added.vocabulary {if !snapshot.vocabulary.iter().any(|w|w.slug==word.slug){snapshot.vocabulary.push(word);}}
  for source in added.known_sources {if let Some(known)=snapshot.known_sources.iter_mut().find(|s|s.slug==source.slug){
   for (cap,description) in source.metadata.capability_descriptions.as_object().into_iter().flatten(){known.metadata.capability_descriptions[cap]=description.clone();}
  }}
 }
 let c=Catalog::build(snapshot);println!("{} runtime-accepted services; {} quarantined; {} index bytes",c.services.len(),c.rejected.len(),c.operation_index().to_string().len());
 if audit_staged {
  let expected:Snapshot=serde_json::from_str(include_str!("../tests/fixtures/source-operations/catalog.json"))?;
  anyhow::ensure!(c.rejected.is_empty()&&c.services.len()==expected.services.len(),"staged admission mismatch");
  for expected in expected.services {
   let actual=c.services.iter().find(|s|s.id==expected.id).ok_or_else(||anyhow::anyhow!("missing staged service"))?;
   anyhow::ensure!(actual.contract==expected.contract&&actual.fixed_params==expected.fixed_params&&actual.preconditions==expected.preconditions&&actual.sample_payload==expected.sample_payload&&actual.list_atomic==expected.list_atomic,"staged contract differs: {}",expected.id);
  }
  println!("Database staging matches all tested contracts; no provider calls");return Ok(());
 }
 let question=std::env::var("SOURCE_QUESTION")?;
 let parser=Parser::new(reqwest::Client::new(),std::env::var("INTENT_API_URL")?,std::env::var("INTENT_API_KEY")?,std::env::var("INTENT_MODEL")?);
 let mut task=Task::default();task.record_turn(&question,None);
 let mut intent=parser.parse(uuid::Uuid::new_v4(),&question,&task,&c).await?;
 bind_subjects(&mut task,&c,&mut intent)?;
 println!("{}",serde_json::to_string(&intent)?);
 match resolve(&c,&task,&intent){
  Resolution::Plan(plan)=>{for step in &plan.steps{println!("{} {} {}",step.service.api_slug,step.service.path,step.service.contract.capability_slug);}
   println!("READY: {} calls, ${:.6} total; no approval or purchase",plan.steps.len(),plan.cap_atomic as f64/1e6);},
  other=>anyhow::bail!("not ready: {other:?}"),
 }
 Ok(())
}
