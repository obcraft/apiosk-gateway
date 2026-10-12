//! A company the person named needs no identifier from them when the catalog
//! can look the identifier up by that name. The planning model is asked to
//! bind the parsed name, but may leave it out when the requested operation
//! only takes an identifier (a KVK number, a SIREN). The resolver then has no
//! name to start the lookup from and would ask the person for a number they
//! do not know. Here the parsed, literal name seeds that lookup, exactly as an
//! endpoint's `lookup` field does on the Ask page: the name search is planned
//! first and the identifier comes from its result. Several matches still stop
//! at the existing choice; nothing is guessed. When no name search can supply
//! the identifier, the person is asked for it as before.
use super::input;
use crate::{
    capability_intake::ParsedRequest,
    catalog::{fact_value, Catalog},
    parser::Intent,
    resolver::{self, Resolution},
    state::Task,
};
use serde_json::json;
use std::collections::BTreeSet;
use uuid::Uuid;

const NAME: &str = "company.name";
/// Parsed subject types that name a legal entity (the parse writes a singular
/// English noun; "company" in practice).
const COMPANY_KINDS: &[&str] = &["company", "business", "organization", "organisation", "legal_entity", "supplier"];

/// Seed `company.name` from a parsed company subject when, and only when, that
/// name lets the planner obtain an identifier it would otherwise ask for. The
/// value is the user's own literal text (the parse only keeps literal labels),
/// bound like any other explicit value and kept in the intent for requotes.
pub(super) fn seed(task: &mut Task, catalog: &Catalog, intent: &mut Intent) -> anyhow::Result<()> {
    let Some(parsed) = task.capability_intake.as_ref().map(|intake| intake.parsed_request.clone()) else {
        return Ok(());
    };
    if !catalog.vocabulary.contains_key(NAME) {
        return Ok(());
    }
    let mut seeded = BTreeSet::new();
    for _ in 0..=intent.subjects.len() {
        match resolver::resolve(catalog, task, intent) {
            Resolution::Plan(_) => break,
            // The planner names one input of one subject at a time.
            Resolution::Missing { entity, kind } => {
                let Some(index) = intent.subjects.iter().position(|s| s.entity_ref == Some(entity)) else { break };
                if !seeded.insert(index) {
                    break;
                }
                let Some((trial_task, trial_intent)) = named(catalog, task, intent, &parsed, index) else { break };
                if !progressed(resolver::resolve(catalog, &trial_task, &trial_intent), entity, &kind) {
                    break;
                }
                (*task, *intent) = (trial_task, trial_intent);
            }
            // A refusal is not a missing identifier; the plan review handles it.
            Resolution::Unsupported(_) => break,
        }
    }
    Ok(())
}

/// A copy of the task and intent with the subject's parsed company name
/// bound; None when the subject is not a named company without a name.
fn named(catalog: &Catalog, task: &Task, intent: &Intent, parsed: &ParsedRequest, index: usize) -> Option<(Task, Intent)> {
    let subject = &intent.subjects[index];
    let entity = subject.entity_ref?;
    if subject.values.contains_key(NAME) || !matches!(fact_value(&task.facts, entity, NAME), Ok(None)) {
        return None;
    }
    let name = parsed_company_name(parsed, &subject.label)?;
    let mut trial_task = task.clone();
    input::user_fact(&mut trial_task, catalog, entity, NAME, json!(name)).ok()?;
    let mut trial_intent = intent.clone();
    trial_intent.subjects[index].values.insert(NAME.into(), json!(name));
    Some((trial_task, trial_intent))
}

/// The name helped when the plan is complete, or when the planner now asks
/// for something else (another subject, or an input no name can supply).
fn progressed(resolution: Resolution, entity: Uuid, kind: &str) -> bool {
    match resolution {
        Resolution::Plan(_) => true,
        Resolution::Missing { entity: next, kind: next_kind } => next != entity || next_kind != kind,
        Resolution::Unsupported(_) => false,
    }
}

/// The parsed label of a company subject, never an identifier typed as the
/// subject ("KVK 12345678" is parsed with the number as its label).
fn parsed_company_name(parsed: &ParsedRequest, label: &str) -> Option<String> {
    let label = label.trim();
    let subject = parsed.subjects.iter().find(|subject| {
        COMPANY_KINDS.contains(&subject.kind.as_str()) && subject.label.trim().to_lowercase() == label.to_lowercase()
    })?;
    let name = subject.label.trim();
    let identifier = subject.identifiers.values().any(|value| value.trim().eq_ignore_ascii_case(name));
    (!identifier && name.chars().any(char::is_alphabetic)).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability_intake::{self, Intake};

    const QUESTION: &str = "What are the registered name, status and address of Coolblue B.V. in the Dutch KVK register?";
    const KVK: &str = "kvk-dutch-business-register";

    /// Production contracts (read-only catalog snapshot): KVK's name search,
    /// profile and filings, with Sirenic's European registry alongside.
    fn catalog() -> Catalog {
        Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/workflows/catalog.json")).unwrap())
    }

    fn parsed(subjects: serde_json::Value) -> ParsedRequest {
        let question = format!("{QUESTION} Bol.com B.V. 24330087");
        let capabilities:Vec<_>=subjects.as_array().unwrap().iter().map(|subject|
            json!({"slug":"company.registry.profile","name":"Company registry profile","description":"Retrieve a company's registered name, status and address.","domain":"company","inputs":["kvk_number"],"outputs":["registered_name","status","address"],"subject_id":subject["id"],"source_hint":"kvk","confidence":0.95})).collect();
        capability_intake::normalize(&json!({"language":"en","subjects":subjects,
            "capabilities":capabilities,
            "deliverable":{"format":"chat","operations":[]}}).to_string(), &question, "fixture").unwrap()
    }

    fn company(label: &str) -> serde_json::Value {
        json!({"id":label,"label":label,"type":"company","identifiers":{}})
    }

    /// What the planning model returned in the live report: the matched KVK
    /// profile, the parsed subject, and no bound name.
    fn planned(task: &mut Task, catalog: &Catalog, parsed: ParsedRequest, subjects: &[(&str, serde_json::Value)], constraints: serde_json::Value) -> Intent {
        task.capability_intake = Some(Intake { parsed_request: parsed, matches: vec![], apiosk_sources_searched: 1,
            catalog_version: catalog.version.clone(), selected_capabilities: Default::default() });
        let mut intent: Intent = serde_json::from_value(json!({
            "subjects":subjects.iter().map(|(label, values)| json!({"entity_ref":null,"label":label,"values":values})).collect::<Vec<_>>(),
            "goals":(0..subjects.len()).map(|subject| json!({"capability":"company.registry-profile","subject":subject,"constraints":constraints})).collect::<Vec<_>>(),
            "answer_operations":[],"unresolved_references":[]})).unwrap();
        input::bind_subjects(task, catalog, &mut intent).unwrap();
        intent
    }

    fn missing(catalog: &Catalog, task: &Task, intent: &Intent) -> String {
        match resolver::resolve(catalog, task, intent) {
            Resolution::Missing { kind, .. } => kind,
            other => panic!("expected a missing input, got {other:?}"),
        }
    }

    #[test]
    fn a_named_company_is_found_by_the_sources_own_name_search_before_its_profile() {
        let catalog = catalog();
        let mut task = Task::default();
        let mut intent = planned(&mut task, &catalog, parsed(json!([company("Coolblue B.V.")])), &[("Coolblue B.V.", json!({}))], json!({"source":KVK}));
        assert_eq!(missing(&catalog, &task, &intent), "company_registry.kvknummer", "the reported failure: asked for a KVK number");

        seed(&mut task, &catalog, &mut intent).unwrap();
        assert_eq!(intent.subjects[0].values[NAME], "Coolblue B.V.", "kept in the intent, so a requote binds it again");
        let Resolution::Plan(plan) = resolver::resolve(&catalog, &task, &intent) else { panic!("expected a quote") };
        let steps: Vec<_> = plan.steps.iter().map(|s| (s.service.api_slug.as_str(), s.service.contract.capability_slug.as_str())).collect();
        assert_eq!(steps, [(KVK, "company_registry.search"), (KVK, "company.registry-profile")]);
        assert_eq!(plan.steps[0].service.contract.cardinality, catalog_core::facts::contract::Cardinality::Many, "several matches stop at the existing choice");
        let described = resolver::describe_steps(&plan, &[]);
        assert_eq!(described[1]["dependencies"], json!([{"step":1,"field":"company_registry.kvknummer","label":"KVK number"}]));
        let fact = task.facts.iter().find(|f| f.semantic_type == NAME).unwrap();
        assert_eq!(fact.provenance, json!({"kind":"user_input"}));
    }

    #[test]
    fn each_named_company_gets_its_own_lookup_in_one_quote() {
        let catalog = catalog();
        let mut task = Task::default();
        let mut intent = planned(&mut task, &catalog, parsed(json!([company("Coolblue B.V."), company("Bol.com B.V.")])),
            &[("Coolblue B.V.", json!({})), ("Bol.com B.V.", json!({}))], json!({"source":KVK}));
        seed(&mut task, &catalog, &mut intent).unwrap();
        let Resolution::Plan(plan) = resolver::resolve(&catalog, &task, &intent) else { panic!("expected one quote for both") };
        assert_eq!(plan.steps.len(), 4);
        for subject in &intent.subjects {
            assert_eq!(subject.values[NAME], json!(subject.label));
            let own: Vec<_> = plan.steps.iter().filter(|s| Some(s.entity_ref) == subject.entity_ref)
                .map(|s| s.service.contract.capability_slug.as_str()).collect();
            assert_eq!(own, ["company_registry.search", "company.registry-profile"], "{}", subject.label);
        }
    }

    /// A planning model used to ask for confirmation before the real quote.
    /// The compiler must now resolve the name-search dependency without ever
    /// contacting that model, even when it would return a confirmation prompt.
    #[tokio::test]
    async fn contract_planner_resolves_missing_registry_id_without_a_model() {
        use axum::{extract::State, routing::post, Json, Router};
        use std::sync::{Arc, Mutex};
        let confirmation = json!({"message":"The KVK Business Register can provide the registered name, status, and address for Coolblue B.V. Would you like to retrieve this information now?",
            "discover":false,"intent":null,"decline_reason":"none"});
        let calls = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let app = Router::new().route("/model", post(move |State(calls): State<Arc<Mutex<Vec<serde_json::Value>>>>, Json(body): Json<serde_json::Value>| {
            let reply = confirmation.clone();
            async move {
                calls.lock().unwrap().push(body);
                Json(json!({"choices":[{"message":{"content":reply.to_string()}}]}))
            }
        })).with_state(calls.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/model", listener.local_addr().unwrap());
        let server = crate::telemetry::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let catalog = catalog();
        let ranked = ["company.registry-profile", "company.branch-profile", "company.registered-names"];
        let candidate = |op: &str| crate::capability_intake::Candidate { origin: "apiosk".into(), name: "KVK Business Register".into(),
            description: String::new(), source_slug: Some(KVK.into()), capability: Some(op.into()),
            endpoint_id: catalog.services.iter().find(|service| service.api_slug == KVK && service.contract.capability_slug == op).map(|service| service.endpoint_id.to_string()), url: None,
            method: "GET".into(), price: None, network: None, calls_30d: None, availability: "supported".into() };
        let parsed_request = crate::capability_intake::normalize(&json!({"language":"en",
            "subjects":[{"id":"s1","label":"Coolblue B.V.","type":"company","identifiers":{}}],
            "capabilities":[{"slug":"company.registry.profile","name":"Company registry profile","description":"Retrieve a company's registered name, status and address.","domain":"company","inputs":["kvk_number"],"outputs":["registered_name","status","address"],"subject_id":"s1","source_hint":"kvk","confidence":0.95}],
            "deliverable":{"format":"chat","operations":[]}}).to_string(), QUESTION, "fixture").unwrap();
        assert_eq!(parsed_request.capabilities[0].missing, ["kvk_number"], "as in the live parse");
        let intake = Intake { parsed_request,
            matches: vec![crate::capability_intake::Match { slug: "company.registry.profile".into(), query: String::new(),
                operations: ranked.iter().map(|op| op.to_string()).collect(), apiosk: ranked.iter().map(|op| candidate(op)).collect(),
                coinbase: vec![], coinbase_status: "skipped".into() }],
            apiosk_sources_searched: 1, catalog_version: catalog.version.clone(),
            selected_capabilities: ranked.iter().map(|op| op.to_string()).collect() };
        let parser = crate::parser::Parser::new(reqwest::Client::new(), url, "fixture".into(), "fixture".into());
        let mut task = Task { capability_intake: Some(intake.clone()), ..Task::default() };
        let reply = parser.chat_from_capabilities(QUESTION, None, &task, &catalog, &intake).await.unwrap();
        server.abort();
        assert!(reply.discover, "an explicit request is planned, not confirmed");
        let calls = calls.lock().unwrap().clone();
        assert!(calls.is_empty(), "a missing identifier uses the registered dependency, not a model");

        let mut intent = reply.intent.unwrap();
        input::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
        seed(&mut task, &catalog, &mut intent).unwrap();
        let Resolution::Plan(plan) = resolver::resolve(&catalog, &task, &intent) else { panic!("expected a quote") };
        let steps: Vec<_> = plan.steps.iter().map(|s| (s.service.api_slug.as_str(), s.service.contract.capability_slug.as_str())).collect();
        assert_eq!(steps, [(KVK, "company_registry.search"), (KVK, "company.registry-profile")]);
    }

    #[test]
    fn a_supplied_identifier_is_used_directly_and_nothing_is_seeded() {
        let catalog = catalog();
        let mut task = Task::default();
        let mut intent = planned(&mut task, &catalog, parsed(json!([company("Coolblue B.V.")])),
            &[("Coolblue B.V.", json!({"company_registry.kvknummer":"24330087"}))], json!({"source":KVK}));
        seed(&mut task, &catalog, &mut intent).unwrap();
        assert!(!intent.subjects[0].values.contains_key(NAME));
        let Resolution::Plan(plan) = resolver::resolve(&catalog, &task, &intent) else { panic!("expected a quote") };
        assert_eq!(plan.steps.len(), 1);
    }

    #[test]
    fn the_number_is_asked_only_when_no_name_search_can_supply_it() {
        let mut catalog = catalog();
        let snapshot = crate::catalog::Snapshot {
            services: catalog.services.drain(..).filter(|s| s.contract.capability_slug != "company_registry.search").collect(),
            vocabulary: catalog.vocabulary.values().cloned().collect(), known_sources: catalog.known_sources.clone(),
        };
        let catalog = Catalog::build(snapshot);
        let mut task = Task::default();
        let mut intent = planned(&mut task, &catalog, parsed(json!([company("Coolblue B.V.")])), &[("Coolblue B.V.", json!({}))], json!({"source":KVK}));
        seed(&mut task, &catalog, &mut intent).unwrap();
        assert_eq!(missing(&catalog, &task, &intent), "company_registry.kvknummer");
        assert!(!intent.subjects[0].values.contains_key(NAME), "an unused name is not bound");
        assert!(task.facts.iter().all(|f| f.semantic_type != NAME));
    }

    #[test]
    fn only_a_parsed_company_name_is_used() {
        let catalog = catalog();
        for (subjects, label) in [
            // A person is not looked up in a company register.
            (json!([{"id":"s1","label":"Coolblue B.V.","type":"person","identifiers":{}}]), "Coolblue B.V."),
            // A number typed as the subject is an identifier, not a name.
            (json!([{"id":"s1","label":"24330087","type":"company","identifiers":{"kvk_number":"24330087"}}]), "24330087"),
        ] {
            let mut task = Task::default();
            let mut intent = planned(&mut task, &catalog, parsed(subjects), &[(label, json!({}))], json!({"source":KVK}));
            seed(&mut task, &catalog, &mut intent).unwrap();
            assert_eq!(missing(&catalog, &task, &intent), "company_registry.kvknummer", "{label}");
            assert!(!intent.subjects[0].values.contains_key(NAME), "{label}");
        }
        // No capability parse (a direct call or workflow intent): never seeded.
        let mut task = Task::default();
        let mut intent = planned(&mut task, &catalog, parsed(json!([company("Coolblue B.V.")])), &[("Coolblue B.V.", json!({}))], json!({"source":KVK}));
        task.capability_intake = None;
        seed(&mut task, &catalog, &mut intent).unwrap();
        assert!(!intent.subjects[0].values.contains_key(NAME));
    }
}
