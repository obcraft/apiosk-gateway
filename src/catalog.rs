use crate::state::{digest, Fact, Task};
use catalog_core::{
    facts::{
        contract::{ExecutionContract, FactType, Vocabulary},
        schema,
    },
    fees,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use uuid::Uuid;

pub const CATALOG_SQL: &str = include_str!("catalog.sql");
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Service {
    #[serde(default)]
    pub source_url: Option<String>,
    pub id: Uuid,
    pub endpoint_id: Uuid,
    pub api_slug: String,
    pub name: String,
    pub method: String,
    pub path: String,
    pub listing_type: String,
    pub list_atomic: i64,
    pub contract: ExecutionContract,
    pub fixed_params: Value,
    pub preconditions: Value,
    pub pagination: Value,
    pub freshness_seconds: Option<i64>,
    pub sample_payload: Option<Value>,
    pub jurisdiction: Option<String>,
}
impl Service {
    pub fn total(&self) -> i64 {
        fees::quote(
            self.list_atomic.into(),
            fees::ProviderClass::from_listing_type(Some(&self.listing_type)),
        )
        .buyer_total_atomic as i64
    }
    pub fn version_hash(&self) -> String {
        digest(self)
    }
    pub fn inputs(&self, task: &Task, entity: Uuid, vocab: &Vocabulary) -> anyhow::Result<Value> {
        let mut inputs = self.fixed_params.as_object().cloned().unwrap_or_default();
        for r in &self.contract.requires {
            anyhow::ensure!(
                r.subject_role == "subject",
                "unsupported multi-role binding"
            );
            if let Some(value) = fact_value(&task.facts, entity, &r.fact_type)? {
                let ft = vocab
                    .get(&r.fact_type)
                    .ok_or_else(|| anyhow::anyhow!("unknown fact type"))?;
                schema::validate(&ft.value_schema, &value)
                    .map_err(|e| anyhow::anyhow!("invalid input: {e}"))?;
                let binding = self
                    .contract
                    .input_map
                    .get(&r.fact_type)
                    .ok_or_else(|| anyhow::anyhow!("unmapped input"))?;
                crate::input_binding::insert(&mut inputs, binding, value)?;
            } else {
                anyhow::ensure!(r.optional, "missing input: {}", r.fact_type);
            }
        }
        Ok(Value::Object(inputs))
    }
}
pub fn fact_value(facts: &[Fact], entity: Uuid, kind: &str) -> anyhow::Result<Option<Value>> {
    let mut values = Vec::new();
    for f in facts.iter().filter(|f| {
        f.entity_ref == entity && f.semantic_type == kind && f.expires_at > chrono::Utc::now()
    }) {
        if !values.contains(&f.value) {
            values.push(f.value.clone())
        }
    }
    anyhow::ensure!(values.len() <= 1, "conflicting facts for {kind}");
    Ok(values.pop())
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub services: Vec<Service>,
    pub vocabulary: Vec<FactType>,
}
#[derive(Debug, Clone)]
pub struct Catalog {
    pub services: Vec<Service>,
    pub vocabulary: Vocabulary,
    pub version: String,
    pub rejected: Vec<(Uuid, String)>,
    pub by_capability: BTreeMap<String, Vec<usize>>,
    pub by_output: BTreeMap<String, Vec<usize>>,
}
impl Catalog {
    pub fn build(snapshot: Snapshot) -> Self {
        let version = digest(&snapshot);
        let vocabulary: Vocabulary = snapshot
            .vocabulary
            .into_iter()
            .map(|f| (f.slug.clone(), f))
            .collect();
        let mut services = vec![];
        let mut rejected = vec![];
        for service in snapshot.services {
            let validation = (|| -> anyhow::Result<()> {
                anyhow::ensure!(
                    service.list_atomic >= 0 && service.list_atomic < 1_000_000_000_000,
                    "invalid price"
                );
                service.contract.check(&vocabulary)?;
                anyhow::ensure!(
                    service
                        .contract
                        .requires
                        .iter()
                        .all(|r| r.subject_role == "subject"),
                    "unsupported multi-role contract"
                );
                let mut template = service
                    .fixed_params
                    .as_object()
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("fixed parameters must be an object"))?;
                for binding in service.contract.input_map.values() {
                    crate::input_binding::insert(&mut template, binding, Value::Null)?;
                }
                anyhow::ensure!(
                    service
                        .preconditions
                        .as_object()
                        .is_some_and(|v| v.is_empty()),
                    "unimplemented preconditions"
                );
                let sample = service
                    .sample_payload
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("missing validation sample"))?;
                let extraction = service.contract.extract(sample, &vocabulary)?;
                anyhow::ensure!(
                    extraction.rejected.is_empty(),
                    "sample contains invalid facts"
                );
                Ok(())
            })();
            match validation {
                Ok(()) => services.push(service),
                Err(e) => rejected.push((service.id, e.to_string())),
            }
        }
        services.sort_by_key(|s| (s.total(), s.api_slug.clone(), s.id));
        let mut by_capability: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut by_output: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, s) in services.iter().enumerate() {
            by_capability
                .entry(s.contract.capability_slug.clone())
                .or_default()
                .push(i);
            for k in s.contract.output_map.produces.keys() {
                by_output.entry(k.clone()).or_default().push(i)
            }
        }
        Self {
            services,
            vocabulary,
            version,
            rejected,
            by_capability,
            by_output,
        }
    }
    pub fn parser_view(&self) -> Value {
        let mut sources: BTreeMap<(&str, &str), std::collections::BTreeSet<&str>> = BTreeMap::new();
        for service in &self.services {
            sources
                .entry((&service.api_slug, &service.name))
                .or_default()
                .insert(&service.contract.capability_slug);
        }
        let sources: Vec<_> = sources.into_iter().map(|((slug, name), capabilities)|
            json!({"slug":slug,"name":name,"capabilities":capabilities})
        ).collect();
        let inputs: std::collections::BTreeSet<_> = self
            .services
            .iter()
            .flat_map(|s| s.contract.requires.iter().map(|r| r.fact_type.clone()))
            .collect();
        let definitions: Vec<_> = inputs
            .iter()
            .filter_map(|slug| self.vocabulary.get(slug))
            .map(|f| json!({"type":f.slug,"schema":f.value_schema}))
            .collect();
        let bindings: Vec<_> = self.services.iter().map(|s| json!({
            "source":s.api_slug,"capability":s.contract.capability_slug,"inputs":s.contract.requires
        })).collect();
        json!({"capabilities":self.by_capability.keys().collect::<Vec<_>>(),"sources":sources,
            "input_types":inputs,"input_definitions":definitions,"service_inputs":bindings})
    }
}
pub struct CatalogCache {
    pool: PgPool,
    cache: moka::future::Cache<u8, Arc<Catalog>>,
    sources: moka::future::Cache<u8, Arc<Vec<crate::sources::Source>>>,
}
impl CatalogCache {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            sources: moka::future::Cache::builder()
                .max_capacity(1)
                .time_to_live(Duration::from_secs(60))
                .build(),
            cache: moka::future::Cache::builder()
                .max_capacity(1)
                .time_to_live(Duration::from_secs(60))
                .build(),
        }
    }
    pub async fn get(&self) -> anyhow::Result<Arc<Catalog>> {
        self.cache
            .try_get_with(0, self.fresh())
            .await
            .map_err(|e| anyhow::anyhow!("catalog unavailable: {e}"))
    }
    pub async fn fresh(&self) -> anyhow::Result<Arc<Catalog>> {
        let value: Value = sqlx::query_scalar(CATALOG_SQL)
            .fetch_one(&self.pool)
            .await?;
        Ok(Arc::new(Catalog::build(serde_json::from_value(value)?)))
    }
    pub async fn sources(&self) -> anyhow::Result<Arc<Vec<crate::sources::Source>>> {
        self.sources
            .try_get_with(0, async {
                let value: Value = sqlx::query_scalar(include_str!("sources.sql"))
                    .fetch_one(&self.pool)
                    .await?;
                let mut sources: Vec<crate::sources::Source> = serde_json::from_value(value)?;
                // Supabase is the v2 source of truth for provider identity and
                // logos. Sanitise those values locally; source browsing must
                // never call the public v1 provider directory as a fallback.
                crate::sources::apply_branding(&mut sources, None);
                Ok::<_, anyhow::Error>(Arc::new(sources))
            })
            .await
            .map_err(|_| anyhow::anyhow!("source catalog unavailable"))
    }
}

#[cfg(test)]
mod parser_catalog_tests {
    use super::*;
    #[test]
    fn parser_receives_exact_source_identity_and_supported_capabilities() {
        let snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/fixtures/integration-catalog.json"))
                .unwrap();
        let catalog = Catalog::build(snapshot);
        let view = catalog.parser_view();
        for service in &catalog.services {
            let source = view["sources"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["slug"] == service.api_slug && s["name"] == service.name)
                .unwrap();
            assert!(source["capabilities"]
                .as_array()
                .unwrap()
                .contains(&json!(service.contract.capability_slug)));
            assert!(source.get("fixed_params").is_none());
        }
    }
}
