use crate::{
    catalog::{fact_value, Catalog, Service},
    parser::Intent,
    protocol::{MAX_DEPTH, MAX_STEPS},
    state::{digest, Task},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub service: Service,
    pub entity_ref: Uuid,
    pub goal_index: usize,
    /// Quoted contingency. It cannot add a service or increase the approved cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_fallback: Option<crate::browser_fallback::Fallback>,
}
impl Step {
    pub fn key(&self) -> String {
        format!("{}:{}", self.service.id, self.entity_ref)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub quote_ref: Uuid,
    pub steps: Vec<Step>,
    pub cap_atomic: i64,
    pub expires_at: DateTime<Utc>,
    pub hash: String,
    /// Buyer markup for every step of this plan: 1500 standard, 2000 workflow.
    #[serde(default = "standard_markup")]
    pub fee_bps: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_slug: Option<String>,
}
pub fn standard_markup() -> i64 {
    catalog_core::fees::BUYER_MARKUP_BPS as i64
}
impl Plan {
    /// Charge this plan's steps with the workflow markup and re-quote the cap.
    pub fn apply_markup(&mut self, markup_bps: i64) {
        self.fee_bps = catalog_core::fees::allowed_markup(markup_bps.into()) as i64;
        self.cap_atomic = self.steps.iter().map(|s| s.service.total_with(self.fee_bps)).sum();
    }
    pub fn step_total(&self, step: &Step) -> i64 {
        step.service.total_with(self.fee_bps)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    Run {
        action_id: Uuid,
        step: usize,
    },
    Supply {
        action_id: Uuid,
        entity_ref: Uuid,
        semantic_type: String,
        schema: Value,
    },
    Choose {
        action_id: Uuid,
        candidates: Vec<Uuid>,
    },
    Read {
        action_id: Uuid,
        result_ref: Uuid,
    },
    Poll {
        action_id: Uuid,
        execution_id: Uuid,
    },
    Wait { action_id: Uuid, until: DateTime<Utc> },
    /// The App asks the person for a document; reading happens client-side
    /// through /v2/documents and the result is sent as the next question.
    Upload { action_id: Uuid, purpose: String },
    Analyze { action_id: Uuid },
    RetryAnalysis { action_id: Uuid },
    Cancel {
        action_id: Uuid,
    },
}
impl Action {
    pub fn id(&self) -> Uuid {
        match self {
            Self::Run { action_id, .. }
            | Self::Supply { action_id, .. }
            | Self::Choose { action_id, .. }
            | Self::Read { action_id, .. }
            | Self::Poll { action_id, .. }
            | Self::Wait { action_id, .. }
            | Self::Upload { action_id, .. }
            | Self::Analyze { action_id }
            | Self::RetryAnalysis { action_id }
            | Self::Cancel { action_id } => *action_id,
        }
    }
    pub fn public(&self) -> crate::protocol::NextAction {
        let (kind, label, paid, input) = match self {
            Self::Run { .. } => (
                "execute_quoted_step",
                "Run the next step".into(),
                true,
                json!({"type":"null"}),
            ),
            Self::Supply {
                semantic_type,
                schema,
                ..
            } => (
                "supply_input",
                format!("Provide {semantic_type}"),
                false,
                json!({"type":"object","properties":{"value":schema},"required":["value"],"additionalProperties":false}),
            ),
            Self::Choose { candidates, .. } => (
                "select_entity",
                "Choose the right match".into(),
                false,
                json!({"type":"object","properties":{"entity_ref":{"type":"string","enum":candidates}},"required":["entity_ref"],"additionalProperties":false}),
            ),
            Self::Read { .. } => (
                "read_result",
                "View saved result".into(),
                false,
                json!({"type":"object","properties":{"offset":{"type":"integer","minimum":0}},"additionalProperties":false}),
            ),
            Self::Poll { .. } | Self::Wait { .. } | Self::Analyze { .. } => (
                "poll",
                "Check status".into(),
                false,
                json!({"type":"null"}),
            ),
            Self::Upload { purpose, .. } => (
                "upload_document",
                if purpose == "invoice" { "Upload the invoice".into() } else { "Upload a document".into() },
                false,
                json!({"type":"object","x-purpose":purpose,"x-workflow":crate::workflows::SUPPLIER_CHECK,"x-accept":".pdf,application/pdf,image/png,image/jpeg","x-max-bytes":crate::documents::MAX_FILE_BYTES}),
            ),
            Self::RetryAnalysis { .. } => (
                "retry_analysis",
                "Retry analysis of saved results".into(),
                false,
                json!({"type":"null"}),
            ),
            Self::Cancel { .. } => (
                "cancel",
                "Stop remaining steps".into(),
                false,
                json!({"type":"null"}),
            ),
        };
        crate::protocol::NextAction {
            action_id: self.id(),
            kind: kind.into(),
            label,
            requires_authorization: paid,
            input_schema: input,
        }
    }
}
#[derive(Debug)]
pub enum Resolution {
    Plan(Plan),
    Missing { entity: Uuid, kind: String },
    Unsupported(String),
}
fn construction_series(service: &Service) -> bool {
    service.api_slug == "cbs-statline"
        && service.contract.capability_slug == "statistics.cbs-construction-annual"
        && service.path == "/odata/v1/CBS/85809NED/Observations"
        && service.fixed_params.get("$filter").and_then(Value::as_str) == Some("Bedrijfsgrootte eq 'WP19077' and BedrijfstakkenBranchesSBI2008 eq '350000' and contains(Perioden,'JJ')")
        && service.fixed_params.get("$top").is_none()
        && service.fixed_params.get("$skip").is_none()
}
fn annual_period(value: &Value) -> bool {
    let Some(period) = value.as_str() else { return false };
    // Only literal calendar years/ranges: month/quarter/date filters remain unsupported.
    let normalized=period.replace(" en ", "-").replace(" and ", "-").replace(" tot ", "-").replace(" to ", "-");
    let years: Vec<_> = normalized.split(|c: char| c=='-' || c=='–' || c==',' || c.is_whitespace()).filter(|s| !s.is_empty()).collect();
    !years.is_empty() && years.len() <= 2 && years.iter().all(|s| s.len()==4 && s.bytes().all(|b| b.is_ascii_digit()) && s.parse::<u16>().is_ok_and(|y| (2005..=2100).contains(&y)))
}
fn matches_source(service: &Service, source: &str, catalog: &Catalog) -> bool {
    source == service.api_slug
        || (catalog.known_sources.iter().any(|s|
            s.slug == service.api_slug && s.metadata.provider_slug.as_deref() == Some(source)))
}
fn allows(service: &Service, constraints: &BTreeMap<String, Value>, catalog: &Catalog) -> bool {
    constraints.iter().all(|(k, v)| match k.as_str() {
        "source" => v.as_str().is_some_and(|source| matches_source(service, source, catalog)),
        "jurisdiction" => v
            .as_str()
            .zip(service.jurisdiction.as_deref())
            .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b)),
        // This reviewed preset retrieves the complete annual construction series.
        // Keep the requested years/sector in the intent for source-backed analysis.
        "period" if construction_series(service) => annual_period(v),
        // Filing-history records carry dates. A requested analysis window is
        // applied to the returned records, not sent as an unsupported API
        // parameter. Preserve the period in the intent and original question;
        // exhaustive coverage and source-side filters remain separate limits.
        "period" if service.api_slug == "companies-house"
            && service.contract.capability_slug == "company.filing-history"
            && service.method == "GET"
            && service.path == "/company/{company_number}/filing-history" =>
                v.as_str().is_some_and(|period| !period.trim().is_empty() && period.len() <= 250),
        "sector" if construction_series(service) => v.as_str().is_some_and(|s|
            ["bouw", "bouwsector", "nederlandse bouwsector", "bouwnijverheid", "construction", "dutch construction"].contains(&s.to_lowercase().as_str())),
        // No timestamp/filing-selection mapping yet: refusing preserves the
        // requirement instead of claiming the first record is the latest.
        _ => false,
    })
}
pub fn resolve(catalog: &Catalog, task: &Task, intent: &Intent) -> Resolution {
    if !intent.unresolved_references.is_empty() {
        return Resolution::Unsupported(intent.unresolved_references.join("; "));
    }
    let mut steps: Vec<Step> = Vec::new();
    // Resolve the most constrained requests first. Their dependencies can
    // already satisfy a generic goal (e.g. KVK address context needs a profile).
    // Greedy generic-profile-first planning otherwise adds an unrelated cheap
    // national registry before discovering the required registry dependency.
    let mut goals: Vec<_> = intent.goals.iter().enumerate().collect();
    goals.sort_by_key(|(_, goal)| catalog.by_capability.get(&goal.capability)
        .into_iter().flatten().filter(|index| catalog.services.get(**index).is_some_and(|s| s.contract.capability_slug == goal.capability && allows(s, &goal.constraints, catalog))).count());
    for (goal_index, goal) in goals {
        // Each requested goal gets the same bounded search budget. Exploring
        // alternatives for one company must not starve the next company.
        let mut expansions = 0;
        let Some(entity) = intent.subjects.get(goal.subject).and_then(|s| s.entity_ref) else {
            return Resolution::Unsupported("unresolved subject".into());
        };
        // An already planned dependency satisfies this goal at zero extra cost.
        // Do this before bounded alternative search, which may exhaust its
        // budget before reaching the reusable service in a large catalog.
        if steps.iter().any(|step| step.entity_ref == entity
            && step.service.contract.capability_slug == goal.capability
            && allows(&step.service, &goal.constraints, catalog)) {
            continue;
        }
        let candidates = catalog
            .by_capability
            .get(&goal.capability)
            .cloned()
            .unwrap_or_default();
        let mut best: Option<Vec<Step>> = None;
        let mut missing = None;
        for index in candidates {
            let Some(service) = catalog.services.get(index).filter(|s| s.contract.capability_slug == goal.capability) else { continue; };
            if !allows(service, &goal.constraints, catalog) {
                continue;
            }
            let mut trial = steps.clone();
            let mut visiting = BTreeSet::new();
            match add(
                catalog,
                task,
                service,
                entity,
                goal_index,
                goal.constraints.get("source").and_then(Value::as_str),
                &mut trial,
                &mut visiting,
                0,
                &mut expansions,
            ) {
                Ok(()) => {
                    let total = trial.iter().map(|s| s.service.total()).sum::<i64>();
                    if best
                        .as_ref()
                        .is_none_or(|b| total < b.iter().map(|s| s.service.total()).sum())
                    {
                        best = Some(trial)
                    }
                }
                Err(e) => missing = Some(e),
            }
        }
        match best {
            Some(b) => steps = b,
            None => {
                return match missing {
                    Some(k) if catalog.vocabulary.contains_key(&k) => {
                        Resolution::Missing { entity, kind: k }
                    }
                    Some(e) => Resolution::Unsupported(e),
                    None => Resolution::Unsupported(format!(
                        "No validated service satisfies {} and all source and period requirements",
                        goal.capability
                    )),
                }
            }
        }
    }
    if steps.len() > MAX_STEPS {
        return Resolution::Unsupported("dependency step limit".into());
    }
    // Stable dependency layers: resolve every independent company first,
    // then retrieve its dependent report. Never bind across company refs.
    let mut depths = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        let depth = steps[..index].iter().enumerate().filter(|(_, previous)| {
            previous.entity_ref == step.entity_ref && step.service.contract.requires.iter()
                .any(|r| !r.optional && previous.service.contract.output_map.produces.contains_key(&r.fact_type))
        }).map(|(i, _)| depths[i] + 1).max().unwrap_or(0usize);
        depths.push(depth);
    }
    let mut layered: Vec<_> = steps.into_iter().zip(depths).collect();
    layered.sort_by_key(|(_, depth)| *depth);
    let mut steps: Vec<_> = layered.into_iter().map(|(step, _)| step).collect();
    crate::browser_fallback::reserve(catalog, task, intent, &mut steps);
    let cap_atomic = steps.iter().map(|s| s.service.total()).sum();
    let hash = digest(&steps);
    Resolution::Plan(Plan {
        quote_ref: Uuid::new_v4(),
        steps,
        cap_atomic,
        expires_at: Utc::now() + chrono::Duration::minutes(10),
        hash,
        fee_bps: standard_markup(),
        workflow_slug: None,
    })
}
#[allow(clippy::too_many_arguments)]
fn add(
    catalog: &Catalog,
    task: &Task,
    service: &Service,
    entity: Uuid,
    goal: usize,
    requested_source: Option<&str>,
    steps: &mut Vec<Step>,
    visiting: &mut BTreeSet<Uuid>,
    depth: usize,
    expansions: &mut usize,
) -> Result<(), String> {
    *expansions += 1;
    if depth > MAX_DEPTH || *expansions > 256 || steps.len() >= MAX_STEPS {
        return Err("dependency search limit".into());
    }
    if steps
        .iter()
        .any(|s| s.service.id == service.id && s.entity_ref == entity)
    {
        return Ok(());
    }
    if !visiting.insert(service.id) {
        return Err("dependency cycle".into());
    }
    for req in service.contract.requires.iter().filter(|r| !r.optional) {
        match fact_value(&task.facts, entity, &req.fact_type) {
            Ok(Some(_)) => continue,
            Err(e) => return Err(e.to_string()),
            _ => (),
        }
        let mut candidates = catalog
            .by_output
            .get(&req.fact_type)
            .cloned()
            .unwrap_or_default();
        // A named source's native lookup must not be replaced by a cheaper
        // reseller. Keep this restriction even if the native chain cannot be
        // resolved: another provider is not an automatic fallback. Distinct
        // cross-source bridges (e.g. registry address -> PDOK map) remain
        // available where the named source has no producer for that input.
        let native_source = requested_source.filter(|source| candidates.iter()
            .any(|index| matches_source(&catalog.services[*index], source, catalog)));
        candidates.retain(|index| native_source.is_none_or(|source|
            matches_source(&catalog.services[*index], source, catalog)));
        // Country-specific IDs in a pan-European response are conditional.
        // They cannot displace a contract that actually returns the needed ID.
        let guaranteed = candidates.iter().any(|index|
            !catalog.services[*index].optional_outputs.contains(&req.fact_type));
        if guaranteed {
            candidates.retain(|index| !catalog.services[*index].optional_outputs.contains(&req.fact_type));
        }
        if steps.iter().any(|s| s.entity_ref == entity
            && s.service.contract.output_map.produces.contains_key(&req.fact_type)
            && !s.service.optional_outputs.contains(&req.fact_type)
            && native_source.is_none_or(|source| matches_source(&s.service, source, catalog))) {
            continue;
        }
        // Try grounded bridges before circular name searches. Cheap but
        // unresolved alternatives must not exhaust the budget before a direct
        // lookup using an already-known identifier is considered.
        candidates.sort_by_key(|index| {
            let candidate = &catalog.services[*index];
            let missing = candidate.contract.requires.iter().filter(|r| !r.optional)
                .filter(|r| !fact_value(&task.facts, entity, &r.fact_type).is_ok_and(|v| v.is_some())
                    && !steps.iter().any(|s| s.entity_ref == entity && s.service.contract.output_map.produces.contains_key(&r.fact_type)))
                .count();
            (missing, candidate.total())
        });
        let mut found = None;
        for index in candidates {
            let mut trial = steps.clone();
            let mut seen = visiting.clone();
            if add(
                catalog,
                task,
                &catalog.services[index],
                entity,
                goal,
                requested_source,
                &mut trial,
                &mut seen,
                depth + 1,
                expansions,
            )
            .is_ok()
                && found.as_ref().is_none_or(|b: &Vec<Step>| {
                    trial.iter().map(|s| s.service.total()).sum::<i64>()
                        < b.iter().map(|s| s.service.total()).sum()
                })
            {
                found = Some(trial)
            }
        }
        if let Some(v) = found {
            *steps = v
        } else {
            return Err(req.fact_type.clone());
        }
    }
    visiting.remove(&service.id);
    steps.push(Step {
        service: service.clone(),
        entity_ref: entity,
        goal_index: goal,
        browser_fallback: None,
    });
    Ok(())
}

/// Presentation only: dependencies come from the quoted contracts, never an LLM guess.
pub fn describe_steps(plan: &Plan, sources: &[crate::sources::Source]) -> Vec<Value> {
    plan.steps.iter().enumerate().map(|(index, step)| {
        let service = &step.service;
        let source = sources.iter().find(|s| s.slug == service.api_slug);
        let capability = service.contract.capability_slug.as_str();
        let title = if step.browser_fallback.is_some() { "Search the web if a source fails or returns no usable records".to_owned() } else { match capability {
            "company.registry-search" | "company_registry.search" => "Find the company and its KVK number".to_owned(),
            "company.financial-statements" => "Retrieve financial statements".to_owned(),
            "company.registered-address-context" => "Verify the registered address and locate the property".to_owned(),
            "soil.bodemloket.feature-info" => "Check available soil information".to_owned(),
            "planning.pdok.feature-info" => "Check the published spatial plan".to_owned(),
            "company.sanctions-screen" | "company.sirenic-sanctions-check" => "Screen for sanctions matches".to_owned(),
            "news.reuters-search" => "Find relevant public news".to_owned(),
            "web.search" | "web.stableenrich-search" => "Find supporting public sources".to_owned(),
            "company.enrich-by-name" | "company.enrich" => "Retrieve additional company information".to_owned(),
            "company.lei-verification" => "Verify the Legal Entity Identifier".to_owned(),
            "domain.registration" => "Check the website domain".to_owned(),
            "company.registry-profile" => "Retrieve the company profile".to_owned(),
            "company.uk-registry-search" => "Find the UK legal entity at Companies House".to_owned(),
            "company.significant-control" => "Check registered ownership and control".to_owned(),
            "company.filing-history" => "Check the accounts filing history".to_owned(),
            "company.uk-vat-candidate-search" => "Search for published UK VAT candidates (not validation)".to_owned(),
            "vat.uk.validate" => "Validate the GB VAT candidate and registered identity".to_owned(),
            "company.uk-director-roster" => "Retrieve current directors; screen complete boards of up to five".to_owned(),
            cap if cap.starts_with("company.uk-director-screening-") => format!("Screen active director {} with OpenSanctions, if present",cap.rsplit('-').next().unwrap_or("")),
            "company.registered-names" => "Retrieve registered company names".to_owned(),
            "company.branch-profile" => "Retrieve the branch profile".to_owned(),
            "statistics.eurostat-gdp-netherlands" => "Retrieve Dutch GDP for the requested year".to_owned(),
            "statistics.eurostat-gdp-germany" => "Retrieve German GDP for the requested year".to_owned(),
            "statistics.cbs-bankruptcies" => "Retrieve the newest bankruptcy observations".to_owned(),
            "statistics.cbs-bankruptcy-measures" => "Read the CBS measures and units".to_owned(),
            "statistics.cbs-bankruptcy-sectors" => "Read the CBS sector definitions".to_owned(),
            "statistics.cbs-bankruptcy-entity-types" => "Read the CBS business-type definitions".to_owned(),
            _ => service.description.as_deref().filter(|_|service.is_document_retrieval())
                .map(|text| text.split(". ").next().unwrap_or(text).chars().take(180).collect())
                .unwrap_or_else(||capability.replace(['.', '_', '-'], " ")),
        }
        };
        let dependencies: Vec<_> = service.contract.requires.iter().filter(|r| !r.optional).filter_map(|required| {
            plan.steps[..index].iter().enumerate().rev().find(|(_, previous)|
                previous.entity_ref == step.entity_ref && previous.service.contract.output_map.produces.contains_key(&required.fact_type)
            ).map(|(previous, _)| json!({"step":previous+1,"field":required.fact_type,
                "label":match required.fact_type.as_str() { "company_registry.kvknummer" => "KVK number".to_owned(), "company.name" => "Company name".to_owned(), "company.nl.main_establishment_addresses" => "Registered visiting address".to_owned(), "source.bodemloket.b_b_o_x" | "source.pdok_ruimtelijke_plannen.b_b_o_x" => "Location from the verified address".to_owned(), _ => required.fact_type.replace(['.', '_', '-'], " ") }}))
        }).collect();
        json!({"number":index+1,"title":title,"capability":capability,
            "source":{"name":service.name,"slug":service.api_slug,
                "directory_slug":source.map(crate::sources::directory_slug).unwrap_or(&service.api_slug),
                "directory_name":source.map(crate::sources::directory_name).unwrap_or(&service.name),
                "logo_url":source.and_then(|s|s.logo_url.as_deref()),"url":service.source_url},
            "dependencies":dependencies,"conditional":step.browser_fallback.is_some(),
            "condition":step.browser_fallback.as_ref().map(|_|"Only if a quoted source fails or returns no usable records; unused fallback is not charged")})
    }).collect()
}

#[cfg(test)]
mod presentation_tests {
    use super::*;
    #[test]
    fn pulse_parent_selection_only_allows_services_owned_by_pulse() {
        let mut catalog = Catalog::build(serde_json::from_str(include_str!("../tests/fixtures/integration-catalog.json")).unwrap());
        let service = catalog.services[0].clone();
        let constraints = BTreeMap::from([("source".to_owned(), json!("pulsenetwork"))]);
        assert!(!allows(&service, &constraints, &catalog));
        catalog.known_sources.push(crate::catalog::KnownSource { slug:service.api_slug.clone(),name:"Member".into(),block_reason:None,
            metadata:crate::readiness::Metadata { provider_slug:Some("pulsenetwork".into()),..Default::default() } });
        assert!(allows(&service, &constraints, &catalog));
        let mut unrelated = service.clone(); unrelated.api_slug = "independent-pulse".into();
        assert!(!allows(&unrelated, &constraints, &catalog));
    }
    #[test]
    fn saved_analysis_retry_is_manual_and_never_authorizes_a_purchase() {
        let action=Action::RetryAnalysis{action_id:Uuid::new_v4()}.public();
        assert_eq!(action.kind,"retry_analysis");
        assert!(!action.requires_authorization);
        assert_eq!(action.input_schema,json!({"type":"null"}));
    }
    #[test]
    fn explains_only_dependencies_between_steps_for_the_same_entity() {
        let snapshot: crate::catalog::Snapshot = serde_json::from_str(include_str!("../tests/fixtures/integration-catalog.json")).unwrap();
        let entity = Uuid::new_v4();
        let mut plan = Plan { quote_ref: Uuid::new_v4(), cap_atomic: 138, expires_at: Utc::now(), hash: "unchanged".into(), fee_bps: standard_markup(), workflow_slug: None, steps: snapshot.services.into_iter().take(2).map(|service| Step { service, entity_ref: entity, goal_index: 0, browser_fallback: None }).collect() };
        let details = describe_steps(&plan, &[]);
        assert_eq!(details[1]["dependencies"][0]["step"], 1);
        assert_eq!(details[1]["dependencies"][0]["field"], "company.id");
        assert_eq!(details[0]["source"]["slug"], "fixture");
        plan.steps[1].entity_ref = Uuid::new_v4();
        assert!(describe_steps(&plan, &[])[1]["dependencies"].as_array().unwrap().is_empty());
        assert_eq!(plan.cap_atomic, 138);
    }
}
