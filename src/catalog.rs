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
use std::{collections::BTreeMap, sync::Arc};
use uuid::Uuid;

mod refresh;
mod registered;
use refresh::{Policy, Refreshing};

pub const CATALOG_SQL: &str = include_str!("catalog.sql");

/// Execution needs current contracts, prices and source blocks, without the
/// directory's endpoint statistics or recent-execution aggregation. Reuse the
/// exact service/vocabulary SQL so the approval checks cannot drift.
pub(crate) fn execution_catalog_sql() -> String {
    let (_, services) = CATALOG_SQL.split_once(" 'vocabulary',")
        .expect("catalog vocabulary boundary");
    let services = services.replace("WHERE (c.status='active' OR", "WHERE c.id=ANY($1) AND (c.status='active' OR");
    let registered = include_str!("catalog/registered.sql");
    format!("SELECT jsonb_build_object('known_sources',({registered}), 'vocabulary',{services}")
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Service {
    /// A registered operation or staged source-document mapping without live
    /// response evidence. Purchases require explicit approval; only persisted
    /// staged mappings are promoted after a usable response (engine::results).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unverified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_input_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub optional_outputs: Vec<String>,
    #[serde(default)]
    pub source_url: Option<String>,
    /// Published attribution/access terms travel with the quoted service and
    /// the saved result, so recovering data retains the same source notice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_notice: Option<Value>,
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
    /// A direct operation derived from the registered endpoint's own inputs.
    /// It does not need a separately authored planner mapping or database row.
    pub fn is_registered_operation(&self) -> bool {
        self.id == self.endpoint_id && self.contract.capability_slug == format!("endpoint.{}", self.endpoint_id.simple())
    }
    pub fn usage_notice_view(&self) -> Option<Value> {
        let notice = self.usage_notice.as_ref()?;
        let text = notice.get("text")?.as_str()?.trim();
        if text.is_empty() || text.chars().count() > 1200 || text.chars().any(|c| c.is_control() && c != '\n') {
            return None;
        }
        let mut result = json!({"text":text});
        if let Some(link) = notice.get("url").filter(|v| !v.is_null()) {
            let link = link.as_str()?;
            let parsed = url::Url::parse(link).ok()?;
            if link.len() > 2048 || parsed.scheme() != "https" || parsed.host_str().is_none()
                || !parsed.username().is_empty() || parsed.password().is_some() {
                return None;
            }
            result["url"] = json!(link);
        }
        if let Some(label) = notice.get("label").filter(|v| !v.is_null()) {
            let label = label.as_str()?.trim();
            if label.is_empty() || label.chars().count() > 120 || label.chars().any(char::is_control) {
                return None;
            }
            result["label"] = json!(label);
        }
        Some(result)
    }
    pub fn result_source(&self) -> Value {
        let mut source = json!({"provider":self.api_slug,"name":self.name,"url":self.source_url,
            "endpoint_id":self.endpoint_id,"contract_version":self.contract.version,"retrieved_at":chrono::Utc::now()});
        if let Some(notice) = self.usage_notice_view() { source["usage_notice"] = notice; }
        source
    }
    pub fn is_document_retrieval(&self) -> bool {
        self.contract.output_map.produces.values().any(|extract| matches!(extract,
            catalog_core::facts::contract::Extract::Adapter { adapter, .. } if adapter == "document.response"))
    }
    pub fn total(&self) -> i64 {
        fees::quote(
            self.list_atomic.into(),
            fees::ProviderClass::from_listing_type(Some(&self.listing_type)),
        )
        .buyer_total_atomic as i64
    }
    /// Buyer total with the plan's markup (15% standard, 20% inside workflows).
    pub fn total_with(&self, markup_bps: i64) -> i64 {
        fees::quote_with_markup(
            self.list_atomic.into(),
            fees::ProviderClass::from_listing_type(Some(&self.listing_type)),
            markup_bps.into(),
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
            let supplied = fact_value(&task.facts, entity, &r.fact_type)?;
            let fallback = self.contract.input_map.get(&r.fact_type).and_then(|b| b.default.clone()).filter(|_| r.optional);
            if let Some(value) = supplied.or(fallback) {
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
        crate::pdok_address::refine(self, &mut inputs);
        crate::tenderned::refine(self, &mut inputs);
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
    #[serde(default)]
    pub known_sources: Vec<KnownSource>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownSource {
    pub slug: String,
    pub name: String,
    pub block_reason: Option<String>,
    #[serde(default)]
    pub metadata: crate::readiness::Metadata,
}
/// Registered inputs and pricing also support direct, user-selected calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListedEndpoint {
    pub endpoint_id: Uuid,
    pub description: String,
    pub method: String,
    pub path: String,
    pub source_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration: Option<registered::Registration>,
}
#[derive(Debug, Clone)]
pub struct Catalog {
    pub services: Vec<Service>,
    pub vocabulary: Vocabulary,
    pub version: String,
    pub rejected: Vec<(Uuid, String)>,
    pub by_capability: BTreeMap<String, Vec<usize>>,
    pub by_output: BTreeMap<String, Vec<usize>>,
    pub known_sources: Vec<KnownSource>,
    /// English renderings of this catalog's texts for Ask search, shared
    /// across refreshes by `CatalogCache`; empty when translation is off.
    pub english: Arc<crate::english::Memory>,
}
impl Catalog {
    pub fn build(mut snapshot: Snapshot) -> Self {
        registered::extend(&mut snapshot);
        let version = digest(&snapshot);
        let vocabulary: Vocabulary = snapshot
            .vocabulary
            .into_iter()
            .map(|f| (f.slug.clone(), f))
            .collect();
        let mut services: Vec<Service> = vec![];
        let mut rejected = vec![];
        for service in snapshot.services {
            // Authored mappings keep priority when valid. Their absence or
            // rejection does not disable a registered endpoint's input form.
            if service.is_registered_operation() && services.iter().any(|accepted| accepted.endpoint_id == service.endpoint_id) { continue; }
            let validation = (|| -> anyhow::Result<()> {
                anyhow::ensure!(!snapshot.known_sources.iter().any(|s| s.slug == service.api_slug && s.block_reason.is_some()), "source blocked for execution");
                anyhow::ensure!(
                    service.list_atomic >= 0 && service.list_atomic < 1_000_000_000_000,
                    "invalid price"
                );
                service.contract.check(&vocabulary)?;
                anyhow::ensure!(service.optional_outputs.iter().all(|k| service.contract.output_map.produces.contains_key(k)), "optional output must be declared by the contract");
                anyhow::ensure!(service.usage_notice.is_none() || service.usage_notice_view().is_some(), "invalid source usage notice");
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
                if service.unverified {
                    // No provider evidence yet: only an opaque document
                    // contract may wait for its first live response.
                    anyhow::ensure!(service.is_document_retrieval(), "unverified contract must retrieve a source document");
                    return Ok(());
                }
                let sample = service
                    .sample_payload
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("missing validation sample"))?;
                anyhow::ensure!(
                    !sample.to_string().contains("Synthetic document-adapter fixture"),
                    "synthetic document-adapter fixture is not provider response evidence"
                );
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
            known_sources: snapshot.known_sources,
            english: Arc::default(),
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
        let mut sources: Vec<_> = sources.into_iter().map(|((slug, name), capabilities)|
            { let known = self.known_sources.iter().find(|s| s.slug == *slug);
                json!({"slug":slug,"name":name,"capabilities":capabilities,"available":true,
                    "provider_slug":known.and_then(|s|s.metadata.provider_slug.as_deref()),
                    "description":known.map(|s| &s.metadata.description),
                    "category":known.map(|s| &s.metadata.category),"tags":known.map(|s| &s.metadata.tags),
                    "capability_descriptions":known.map(|s| &s.metadata.capability_descriptions),
                    "readiness":crate::readiness::assess(self, &[slug])}) }
        ).collect();
        for source in &self.known_sources {
            if !sources.iter().any(|s| s["slug"] == source.slug) {
                sources.push(json!({"slug":source.slug,"name":source.name,"capabilities":[],"available":false,
                    "readiness":crate::readiness::assess(self, &[source.slug.as_str()]),
                    "description":source.metadata.description,"category":source.metadata.category,"tags":source.metadata.tags,
                    "reason":source.block_reason.as_deref().unwrap_or("No validated execution contract is available for this source.")}));
            }
        }
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
            "source":s.api_slug,"capability":s.contract.capability_slug,"inputs":s.contract.requires,
            "input_names":s.contract.input_map.iter().map(|(fact,binding)| (fact,&binding.name)).collect::<BTreeMap<_,_>>(),
            "output_kind":if s.is_document_retrieval() {"source_document"} else {"mapped_facts"}
        })).collect();
        let mut capabilities: Vec<&str> = self.by_capability.keys().map(String::as_str).collect();
        let mut presets = vec![];
        if crate::uk_supplier::available(self) {
            capabilities.push(crate::uk_supplier::PRESET);
            presets.push(json!({"capability":crate::uk_supplier::PRESET,"description":"UK supplier onboarding: Companies House ownership/PSC and accounts filing status, published VAT candidate search + UK validation, Sirenic active roster and OpenSanctions Person/Company director screening (complete boards up to five). One approval, saved PDF and summary; unavailable evidence stays unknown."}));
        }
        if crate::supplier_check::available(self) {
            capabilities.push(crate::supplier_check::PRESET);
            presets.push(json!({"capability":crate::supplier_check::PRESET,"description":"Supplier payment safety check for one supplier/invoice: business register, VAT, sanctions, domain age, IBAN and earlier invoices. Code adds the individual checks."}));
        }
        json!({"capabilities":capabilities,"workflow_presets":presets,"sources":sources,
            "input_types":inputs,"input_definitions":definitions,"service_inputs":bindings})
    }

    /// A compact index gives every executable operation the same opportunity
    /// to be selected before its full input schemas enter the model context.
    pub fn operation_index(&self) -> Value {
        self.index_of(self.services.iter())
    }

    /// The selection index for one question: every verified operation, plus
    /// the unverified ones whose words best match the question. Unverified
    /// operations only fill gaps, so a cheap lexical shortlist keeps the index
    /// (and every Ask) about as small as the verified catalogue alone.
    pub fn operation_index_for(&self, question: &str) -> Value {
        const UNVERIFIED: usize = 150;
        let lowered = question.to_lowercase();
        let words: Vec<&str> = lowered
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.chars().count() >= 3)
            .collect();
        let mut scored: Vec<(usize, &Service)> = self.services.iter()
            .filter(|s| s.unverified)
            .map(|s| {
                let text = format!("{} {} {} {}", s.api_slug, s.contract.capability_slug, s.name, s.description.as_deref().unwrap_or("")).to_lowercase();
                (words.iter().filter(|w| text.contains(**w)).count(), s)
            })
            .filter(|(score, _)| *score > 0)
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.total().cmp(&b.1.total())));
        self.index_of(self.services.iter().filter(|s| !s.unverified)
            .chain(scored.into_iter().take(UNVERIFIED).map(|(_, s)| s)))
    }

    fn index_of<'a>(&'a self, services: impl Iterator<Item = &'a Service>) -> Value {
        let mut operations = BTreeMap::new();
        for s in services {
            let known=self.known_sources.iter().find(|k|k.slug==s.api_slug);
            let description=s.description.as_deref().or_else(||known.and_then(|k|k.metadata.capability_descriptions.get(&s.contract.capability_slug))
                .and_then(Value::as_str)).unwrap_or(&s.contract.capability_slug);
            operations.entry(s.contract.capability_slug.clone()).or_insert_with(Vec::new).push(json!({
                "source":s.api_slug,"provider":known.and_then(|k|k.metadata.provider_slug.as_deref()),
                "description":description,"inputs":s.contract.input_map.values().map(|b|&b.name).collect::<Vec<_>>()
            }));
        }
        json!(operations)
    }

    pub fn selected_parser_view(&self, selected: &std::collections::BTreeSet<String>) -> Value {
        self.parser_view_for(selected, true)
    }

    pub fn independent_parser_view(&self, selected: &std::collections::BTreeSet<String>) -> Value {
        self.parser_view_for(selected, false)
    }

    fn parser_view_for(&self, selected: &std::collections::BTreeSet<String>, dependencies: bool) -> Value {
        let mut view=self.parser_view();
        let mut needed=selected.clone();
        // Dependencies are also executable operations. Retain every candidate
        // producer rather than introducing a vendor preference here.
        if dependencies {
          loop {
            let before=needed.len();
            for s in &self.services {
                if !needed.contains(&s.contract.capability_slug) {continue;}
                for requirement in s.contract.requires.iter().filter(|r|!r.optional) {
                    if let Some(indices)=self.by_output.get(&requirement.fact_type) {
                        for index in indices {needed.insert(self.services[*index].contract.capability_slug.clone());}
                    }
                }
            }
            if before==needed.len(){break;}
        }
        }
        // Existing composite presets need their authored dependency vocabulary.
        // They remain explicit options, not preferred providers.
        if dependencies && (selected.contains(crate::uk_supplier::PRESET)||selected.contains(crate::supplier_check::PRESET)) {
            return view;
        }
        view["capabilities"].as_array_mut().unwrap().retain(|v|v.as_str().is_some_and(|s|needed.contains(s)));
        view["workflow_presets"].as_array_mut().unwrap().retain(|v|v["capability"].as_str().is_some_and(|s|needed.contains(s)));
        view["service_inputs"].as_array_mut().unwrap().retain(|v|v["capability"].as_str().is_some_and(|s|needed.contains(s)));
        let types: std::collections::BTreeSet<_>=self.services.iter().filter(|s|needed.contains(&s.contract.capability_slug))
            .flat_map(|s|s.contract.requires.iter().map(|r|r.fact_type.as_str())).collect();
        view["input_types"].as_array_mut().unwrap().retain(|v|v.as_str().is_some_and(|s|types.contains(s)));
        view["input_definitions"].as_array_mut().unwrap().retain(|v|v["type"].as_str().is_some_and(|s|types.contains(s)));
        for source in view["sources"].as_array_mut().unwrap() {
            source["capabilities"].as_array_mut().unwrap().retain(|v|v.as_str().is_some_and(|s|needed.contains(s)));
            if let Some(descriptions)=source["capability_descriptions"].as_object_mut(){descriptions.retain(|key,_|needed.contains(key));}
            if source["capabilities"].as_array().is_some_and(|caps|caps.is_empty()) {
                // The index already supplied relevance metadata. Keep source
                // identity/restrictions so named unavailable sources are still
                // reported honestly, without repeating hundreds of tag lists.
                source.as_object_mut().unwrap().retain(|key,_|matches!(key.as_str(),"slug"|"name"|"provider_slug"|"capabilities"|"available"|"reason"|"readiness"));
                if let Some(readiness)=source["readiness"].as_object_mut(){readiness.retain(|key,_|matches!(key.as_str(),"status"|"restrictions"|"coverage_notices"));}
            }
        }
        view
    }

    /// Reviewed API contracts for semantic planning. No credentials, payloads
    /// or fixed parameters belong in the model's context.
    /// `available`: operations the Ask search already found for this question.
    /// A large catalogue is narrowed to them and the intent's own goals, as
    /// the planner is: the whole index no longer fits a model's context.
    pub fn planning_view(&self, intent: &crate::parser::Intent, available: Option<&std::collections::BTreeSet<String>>) -> Value {
        let capabilities: std::collections::BTreeSet<_> = intent
            .goals
            .iter()
            .map(|goal| goal.capability.as_str())
            .collect();
        let catalog = if self.services.len() <= 400 { self.parser_view() } else {
            let mut selected: std::collections::BTreeSet<String> = capabilities.iter().map(|c| c.to_string()).collect();
            selected.extend(available.into_iter().flatten().cloned());
            self.selected_parser_view(&selected)
        };
        let services: Vec<_> = self
            .services
            .iter()
            .filter(|s| capabilities.contains(s.contract.capability_slug.as_str()))
            .map(|s| {
                json!({"source":s.api_slug,"name":s.name,
                "capability":s.contract.capability_slug,"method":s.method,"path":s.path,
                "jurisdiction":s.jurisdiction,"requires":s.contract.requires,
                "input_map":s.contract.input_map,"outputs":s.contract.output_map,
                "pagination":s.pagination})
            })
            .collect();
        json!({"catalog":catalog,"requested_services":services})
    }
}
/// The planning catalogue and source list, refreshed in the background
/// before they expire; `refresh` holds the policy.
pub struct CatalogCache {
    pool: PgPool,
    catalog: Arc<Refreshing<Catalog>>,
    sources: Arc<Refreshing<Vec<crate::sources::Source>>>,
    /// English renderings for Ask search, kept across catalog refreshes.
    english: Arc<crate::english::English>,
}
impl CatalogCache {
    pub fn new(pool: PgPool) -> Self {
        Self { pool, catalog: Refreshing::new(Policy::CATALOG), sources: Refreshing::new(Policy::CATALOG),
            english: Arc::new(crate::english::English::disabled()) }
    }
    /// Translate the catalog's non-English texts after each load (see
    /// `english`). Without this, search reads the original texts only.
    pub fn with_english(mut self, english: crate::english::English) -> Self {
        self.english = Arc::new(english);
        self
    }
    pub fn english(&self) -> &Arc<crate::english::English> {
        &self.english
    }
    fn load(&self) -> impl std::future::Future<Output = anyhow::Result<Catalog>> + Send + 'static {
        let (pool, english) = (self.pool.clone(), self.english.clone());
        async move {
            let mut catalog = load_catalog(pool).await?;
            english.attach(&mut catalog);
            Ok(catalog)
        }
    }
    pub async fn get(&self) -> anyhow::Result<Arc<Catalog>> {
        use crate::telemetry::{ErrorClass, Outcome};
        let started = std::time::Instant::now();
        let (catalog, cache) = self.catalog.get(|| self.load()).await;
        crate::telemetry::catalog(cache, started, if catalog.is_ok() { Outcome::Ok } else { Outcome::Failed(ErrorClass::Db) });
        catalog.map_err(|error| anyhow::anyhow!("catalog unavailable: {error}"))
    }
    /// Reads the directory now instead of serving the cached catalogue, and
    /// keeps the result for later questions.
    pub async fn fresh(&self) -> anyhow::Result<Arc<Catalog>> {
        self.catalog.reload(self.load()).await
    }
    pub async fn fresh_execution(&self, service_ids: &[Uuid]) -> anyhow::Result<Arc<Catalog>> {
        let value: Value = sqlx::query_scalar(&execution_catalog_sql())
            .bind(service_ids)
            .fetch_one(&self.pool).await?;
        Ok(Arc::new(Catalog::build(serde_json::from_value(value)?)))
    }
    pub async fn sources(&self) -> anyhow::Result<Arc<Vec<crate::sources::Source>>> {
        let (sources, _) = self.sources.get(|| load_sources(self.pool.clone())).await;
        sources.map_err(|error| anyhow::anyhow!("source catalog unavailable: {error}"))
    }
}

async fn load_catalog(pool: PgPool) -> anyhow::Result<Catalog> {
    // Directory statistics can exceed the ordinary ten-second statement
    // limit while the database is busy. Bound this read separately so a
    // cold start does not crash-loop. Live execution checks keep the short
    // limit and fetch only the selected contracts.
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION READ ONLY").execute(&mut *tx).await?;
    sqlx::query("SET LOCAL statement_timeout='30s'").execute(&mut *tx).await?;
    let value: Value = sqlx::query_scalar(CATALOG_SQL)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Catalog::build(serde_json::from_value(value)?))
}

async fn load_sources(pool: PgPool) -> anyhow::Result<Vec<crate::sources::Source>> {
    let value: Value = sqlx::query_scalar(include_str!("sources.sql"))
        .fetch_one(&pool)
        .await?;
    let mut sources: Vec<crate::sources::Source> = serde_json::from_value(value)?;
    // Supabase is the v2 source of truth for provider identity and
    // logos. Sanitise those values locally; source browsing must
    // never call the public v1 provider directory as a fallback.
    crate::sources::apply_branding(&mut sources, None);
    Ok(sources)
}

#[cfg(test)]
mod parser_catalog_tests {
    use super::*;
    /// Both shapes this product actually publishes count as documented inputs.
    ///
    /// The audit used to demand JSON Schema carrying a top-level `required`
    /// key, and neither half of that held. KOOP publishes proper JSON Schema
    /// whose every parameter is optional, so it carries no `required` key at
    /// all and scored zero of one. KVK publishes a flat parameter map — the
    /// shape `readFields` builds forms from — and scored zero of five: the
    /// most-used source in the catalogue, reported as undocumented.
    ///
    /// What must NOT count is the empty publish `{"type":"object"}`, which is
    /// what several listings send when they mean "no schema": every member of
    /// it is a string, so the jsonpath test finds nothing. And `required`,
    /// where present, must still be an array — a boolean there is a defect.
    #[test]
    fn every_published_input_shape_counts_and_the_empty_publish_does_not() {
        assert!(CATALOG_SQL
            .contains("jsonb_typeof(coalesce(e.query_params->'required','[]'::jsonb))='array'"));
        assert!(CATALOG_SQL
            .contains("jsonb_typeof(coalesce(e.request_body->'required','[]'::jsonb))='array'"));
        // The old form treated "absent" and "wrong type" as the same failure.
        assert!(!CATALOG_SQL.contains("jsonb_typeof(e.query_params->'required')='array'"));
        assert!(!CATALOG_SQL.contains("jsonb_typeof(e.request_body->'required')='array'"));
        // The flat and location-grouped maps, and only where a member is an
        // object — which `{"type":"object"}` never has.
        assert!(CATALOG_SQL
            .contains(r#"jsonb_path_exists(e.query_params,'$.* ? (@.type() == "object")')"#));
        assert!(CATALOG_SQL
            .contains(r#"jsonb_path_exists(e.request_body,'$.* ? (@.type() == "object")')"#));
    }
    #[test]
    fn optional_defaults_are_validated_and_explicit_facts_win() {
        let mut snapshot: Snapshot = serde_json::from_str(include_str!("../tests/fixtures/integration-catalog.json")).unwrap();
        let service = &mut snapshot.services[0];
        service.contract.requires[0].optional = true;
        service.contract.input_map.get_mut("company.name").unwrap().default = Some(json!("reviewed fallback"));
        let catalog = Catalog::build(snapshot.clone());
        assert!(catalog.rejected.is_empty());
        let service = catalog.services.iter().find(|s| s.id == snapshot.services[0].id).unwrap();
        let entity = Uuid::new_v4();
        let mut task = Task::default();
        assert_eq!(service.inputs(&task, entity, &catalog.vocabulary).unwrap()["company.name"], "reviewed fallback");
        let mut intent: crate::parser::Intent = serde_json::from_value(json!({"subjects":[{"entity_ref":null,"label":"Explicit company","values":{"company.name":"Explicit company"}}],"goals":[],"answer_operations":[],"unresolved_references":[]})).unwrap();
        crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
        assert_eq!(service.inputs(&task, intent.subjects[0].entity_ref.unwrap(), &catalog.vocabulary).unwrap()["company.name"], "Explicit company");
        snapshot.services[0].contract.input_map.get_mut("company.name").unwrap().default = Some(json!(37));
        assert_eq!(Catalog::build(snapshot.clone()).rejected.len(), 1);
        snapshot.services[0].contract.input_map.get_mut("company.name").unwrap().default = Some(json!("valid string"));
        snapshot.services[0].contract.requires[0].optional = false;
        assert_eq!(Catalog::build(snapshot).rejected.len(), 1);
    }
    #[test]
    fn unverified_document_contract_waits_for_its_first_response() {
        let fixture: Value = serde_json::from_str(include_str!("../tests/fixtures/source-document-service.json")).unwrap();
        let mut service: Service = serde_json::from_value(fixture["service"].clone()).unwrap();
        let vocabulary: Vec<FactType> = serde_json::from_value(fixture["vocabulary"].clone()).unwrap();
        // Without evidence a verified contract is rejected.
        let verified = Catalog::build(Snapshot { services: vec![service.clone()], vocabulary: vocabulary.clone(), known_sources: vec![] });
        assert!(verified.services.is_empty());
        // Staged and unverified, the same document contract is selectable.
        service.unverified = true;
        let staged = Catalog::build(Snapshot { services: vec![service.clone()], vocabulary: vocabulary.clone(), known_sources: vec![] });
        assert_eq!(staged.services.len(), 1);
        assert!(staged.services[0].unverified);
        // An unverified contract that maps facts, not a document, never is.
        let mut snapshot: Snapshot = serde_json::from_str(include_str!("../tests/fixtures/integration-catalog.json")).unwrap();
        snapshot.services[0].unverified = true;
        snapshot.services[0].sample_payload = None;
        let id = snapshot.services[0].id;
        let mapped = Catalog::build(snapshot);
        assert!(mapped.rejected.iter().any(|(candidate, reason)| *candidate == id && reason.contains("source document")));
    }
    #[test]
    fn synthetic_document_adapter_cannot_enter_the_runtime_catalog() {
        let mut snapshot: Snapshot = serde_json::from_str(include_str!("../tests/fixtures/integration-catalog.json")).unwrap();
        snapshot.services[0].sample_payload = Some(json!({"data":"Synthetic document-adapter fixture; not a provider response or live execution."}));
        let id = snapshot.services[0].id;
        let catalog = Catalog::build(snapshot);
        assert!(catalog.services.iter().all(|s| s.id != id));
        assert!(catalog.rejected.iter().any(|(candidate, reason)| *candidate == id && reason.contains("synthetic document-adapter")));
    }
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
