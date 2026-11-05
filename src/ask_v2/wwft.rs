//! The explicitly requested Dutch Wwft package. Interpretation identifies the
//! request and its literal subjects; this product policy supplies its checks.
//! It prepares sources only. Normal coverage, quoting and approval still apply.
use crate::capability_intake::{Capability, ParsedRequest};

/// Explicitly named native checks use their published operation and output
/// vocabulary. A registry detail call by name includes its identifier lookup
/// in the displayed, priced plan; no provider call runs during planning.
pub(crate) fn named_checks(request: &mut ParsedRequest, question: &str) -> anyhow::Result<()> {
    let words: Vec<_> = question.split(|c: char| !c.is_alphanumeric()).map(str::to_lowercase).collect();
    let kvk_named = words.iter().any(|word| matches!(word.as_str(), "kvk" | "handelsregister"));
    let sanctions_named = words.iter().any(|word| word == "opensanctions");
    let mut lookups = Vec::new();
    for cap in &mut request.capabilities {
        let words = format!("{} {} {}", cap.slug, cap.name, cap.description).to_lowercase();
        let source = cap.source_hint.as_deref().unwrap_or("").to_lowercase();
        let profile = kvk_named && matches!(source.as_str(), "" | "kvk" | "handelsregister" | "kvk-dutch-business-register")
            && (source != "" || words.contains("kvk") || words.contains("handelsregister"))
            && cap.domain == "company" && ["registry", "registration", "register", "profile", "registratie", "profiel"].iter().any(|term| words.contains(term))
            && cap.outputs.iter().any(|output| matches!(output.replace(['.', '-'], "_").as_str(), "company_identifier" | "company_id" | "kvk_number" | "registration_number" | "company_registry_kvknummer" | "search_results" | "registration_details" | "registration" | "registration_status" | "company_profile" | "profile" | "legal_form"));
        let screening = sanctions_named && matches!(source.as_str(), "" | "opensanctions")
            && (source != "" || words.contains("opensanctions"))
            && ["sanction", "sanctie", "pep"].iter().any(|term| words.contains(term));
        if !profile && !screening { continue; }
        let Some(subject) = cap.subject_id.as_ref().and_then(|id| request.subjects.iter().find(|subject| &subject.id == id)) else { continue; };
        let has_identifier = subject.identifiers.iter().any(|(key, value)|
            (key.contains("kvk") || matches!(key.as_str(), "registration_number" | "company_number"))
                && value.len() == 8 && value.bytes().all(|byte| byte.is_ascii_digit()));
        if profile && has_identifier { continue; }
        // A search-only request remains a search. A registration/profile
        // request retrieves the record after identifying the company.
        let search_only = profile && cap.outputs.iter().all(|output| matches!(output.replace(['.', '-'], "_").as_str(),
            "company_identifier" | "company_id" | "company_identifiers" | "kvk_number" | "registration_number" | "search_results" | "company_registry_kvknummer"));
        let (slug, output, input, name, hint) = if screening {
            ("company.sanctions-screen", "company.screening_result", "company.name", "OpenSanctions", "opensanctions")
        } else if search_only {
            ("company_registry.search", "company_registry.kvknummer", "company.name", "KVK Zoeken", "kvk")
        } else { ("company.registry-profile", "company.profile", "company_registry.kvknummer", "KVK Basisprofiel", "kvk") };
        let known_output = |output: &str| {
            let normalized = output.replace(['.', '-'], "_").to_lowercase();
            if screening { matches!(normalized.as_str(), "sanctions_matches" | "sanctions_status" | "sanctions_result" | "sanctions" | "pep_matches" | "pep_status" | "screening_result" | "company_screening_result") }
            else { matches!(normalized.as_str(), "company_identifier" | "company_id" | "company_identifiers" | "kvk_number" | "registration_number" | "company_registry_kvknummer" | "search_results" | "registration_details" | "registration" | "registration_status" | "company_profile" | "profile" | "legal_form") }
        };
        // Broader/custom scopes keep their original semantic requirements and
        // go through normal planning; this shortcut is only for known fields.
        if !cap.outputs.iter().all(|output| known_output(output)) { continue; }
        cap.outputs.retain(|output| !known_output(output));
        if !cap.outputs.iter().any(|value| value == output) { cap.outputs.push(output.into()); }
        cap.slug = slug.into(); cap.name = name.into(); cap.source_hint = Some(hint.into());
        if !cap.inputs.iter().any(|value| value == input) { cap.inputs.push(input.into()); }
        if profile && !search_only && !has_identifier {
            cap.missing.push("company_registry.kvknummer".into());
            lookups.push(Capability { slug:"company_registry.search".into(),name:"KVK Zoeken".into(),description:"Find the KVK number needed to retrieve the requested registration.".into(),
                domain:"company".into(),inputs:vec!["company.name".into()],outputs:vec!["company_registry.kvknummer".into()],subject_id:cap.subject_id.clone(),source_hint:Some("kvk".into()),confidence:cap.confidence,missing:vec![] });
        }
    }
    for lookup in lookups {
        if !request.capabilities.iter().any(|cap| cap.slug == lookup.slug && cap.subject_id == lookup.subject_id && cap.source_hint == lookup.source_hint) {
            request.capabilities.insert(0, lookup);
        }
    }
    anyhow::ensure!(request.capabilities.len() <= 16, "Split the request into smaller groups so every required source remains in the plan");
    Ok(())
}

pub(crate) fn expand(request: &mut ParsedRequest) -> anyhow::Result<()> {
    let mut expanded = Vec::new();
    for cap in &request.capabilities {
        if cap.slug != "compliance.wwft.check" {
            expanded.push(cap.clone());
            continue;
        }
        let subject = cap.subject_id.as_ref().and_then(|id| request.subjects.iter().find(|subject| &subject.id == id))
            .ok_or_else(|| anyhow::anyhow!("A Wwft check needs a named company"))?;
        anyhow::ensure!(["country", "country_code", "jurisdiction"].iter()
            .filter_map(|key| subject.identifiers.get(*key))
            .all(|country| crate::ask_v2::search::exact_country(country).as_deref() == Some("NL")),
            "The Dutch Wwft package requires a Dutch company; preserve the supplied country and select its applicable sources");
        let checks = [
            ("company_registry.search", "KVK Zoeken", "Zoek de inschrijving op naam en vind het KvK-nummer.", "company.name", "company_registry.kvknummer", "kvk"),
            ("company.registry-profile", "KVK Basisprofiel", "Controleer de registratie en rechtsvorm.", "company_registry.kvknummer", "company.profile", "kvk"),
            ("company.branch-profile", "KVK Vestigingsprofiel", "Haal de vestigingsgegevens op met het gevonden vestigingsnummer. Bij meerdere vestigingen is een selectie nodig.", "company.vestigingsnummer", "company.vestigingsnummer", "kvk"),
            ("company.registered-names", "KVK Naamgeving", "Haal de statutaire naam en geregistreerde handelsnamen op.", "company_registry.kvknummer", "company.statutairenaam", "kvk"),
            ("company.financial-statements", "KVK Jaarrekening", "Haal de beschikbare gedeponeerde jaarrekening op. Ontbrekende open data bewijst niet dat er niet is gedeponeerd.", "company_registry.kvknummer", "company.financial_statements", "kvk"),
            ("company.sanctions-screen", "OpenSanctions", "Zoek mogelijke sanctie- en PEP-gerelateerde matches. Naamovereenkomsten zijn geen bevestigde identiteit; dit is geen volledige UBO- of persoonscontrole.", "company.name", "company.screening_result", "opensanctions"),
        ];
        for (slug, name, description, input, output, source) in checks {
            let mut outputs = vec![output.to_owned()];
            if slug == "company.registered-names" { outputs.push("company.vestigingsnummer".into()); }
            let supplied = input == "company.name" || subject.identifiers.keys().any(|key| key == input || (input == "company_registry.kvknummer" && ["kvk_number", "kvk_nummer", "registration_number"].contains(&key.as_str())));
            expanded.push(Capability { slug: slug.into(), name: name.into(), description: description.into(),
                domain: if source == "opensanctions" { "compliance" } else { "company" }.into(),
                inputs: vec![input.into()], outputs, subject_id: Some(subject.id.clone()), source_hint: Some(source.into()),
                confidence: cap.confidence, missing: if supplied { vec![] } else { vec![input.into()] } });
        }
    }
    request.capabilities = expanded;
    deduplicate(request);
    anyhow::ensure!(request.capabilities.len() <= 16, "Split this Wwft request into smaller company groups so every required source remains in the plan");
    Ok(())
}

const PACKAGE: &[&str] = &["company_registry.search", "company.registry-profile", "company.branch-profile", "company.registered-names", "company.financial-statements", "company.sanctions-screen"];

fn package_output(output: &str) -> Option<(&'static str, &'static str)> {
    Some(match output.replace(['.', '-'], "_").to_lowercase().as_str() {
        "company_identifier" | "company_id" | "registration_number" | "kvk_number" | "kvk_nummer" | "company_registry_kvknummer" => ("company_registry.search", "company_registry.kvknummer"),
        "company_profile" | "profile" | "registration" | "registration_details" | "registration_status" | "legal_form" | "company_legal_form" => ("company.registry-profile", "company.profile"),
        "statutory_name" | "registered_names" | "trade_names" | "company_statutairenaam" => ("company.registered-names", "company.statutairenaam"),
        "sanctions" | "sanctions_matches" | "sanctions_status" | "pep_matches" | "pep_status" | "screening_result" | "company_screening_result" => ("company.sanctions-screen", "company.screening_result"),
        "financial_statements" | "company_financial_statements" | "annual_accounts" => ("company.financial-statements", "company.financial_statements"),
        _ => return None,
    })
}

/// Merge redundant requested data into a complete package. Generated operation
/// names are not a vocabulary: company.nameSearch, company.find, etc. must be
/// equivalent when they request the same fields for the same subject/source.
/// Unknown fields, inputs, provider restrictions and other subjects survive.
pub(crate) fn deduplicate(request: &mut ParsedRequest) -> bool {
    let packages: Vec<_> = request.subjects.iter().filter(|subject| PACKAGE.iter().all(|slug|
        request.capabilities.iter().any(|cap| cap.slug == *slug && cap.subject_id.as_ref() == Some(&subject.id))))
        .map(|subject| subject.id.clone()).collect();
    let mut remove = std::collections::BTreeSet::new();
    for (index, cap) in request.capabilities.clone().iter().enumerate() {
        if !packages.iter().any(|id| Some(id) == cap.subject_id.as_ref()) || PACKAGE.contains(&cap.slug.as_str()) { continue; }
        let alias = match cap.slug.as_str() {
            "company.registry.search" | "company.search" | "company.searchbyname" | "company.search.by.name" => Some("company_registry.search"),
            "company.profile.retrieve" | "company.profile" | "company.registry.profile" => Some("company.registry-profile"),
            "company.branch.profile" => Some("company.branch-profile"),
            "company.registered.names" => Some("company.registered-names"),
            "company.financial.statements" => Some("company.financial-statements"),
            "company.sanctions.screen" => Some("company.sanctions-screen"),
            _ => None,
        };
        let operation = match alias.or_else(|| (!cap.outputs.is_empty() && cap.outputs.iter().all(|output| package_output(output).is_some()))
            .then(|| package_output(&cap.outputs[0]).unwrap().0)) { Some(operation) => operation, None => continue };
        let same_source = |operation: &str| cap.source_hint.as_deref().is_none_or(|source| match operation {
            "company.sanctions-screen" => source.eq_ignore_ascii_case("opensanctions"),
            _ => matches!(source.to_lowercase().as_str(), "kvk" | "handelsregister" | "kvk-dutch-business-register"),
        });
        if !same_source(operation) || cap.outputs.iter().filter_map(|output| package_output(output)).any(|(operation,_)| !same_source(operation)) { continue; }
        let target = request.capabilities.iter_mut().find(|other| other.slug == operation && other.subject_id == cap.subject_id).unwrap();
        for (values, added) in [(&mut target.inputs, &cap.inputs), (&mut target.missing, &cap.missing)] {
            for value in added { if !values.contains(value) { values.push(value.clone()); } }
        }
        for output in &cap.outputs {
            let (operation, field) = package_output(output).unwrap_or((operation, output.as_str()));
            let target = request.capabilities.iter_mut().find(|other| other.slug == operation && other.subject_id == cap.subject_id).unwrap();
            if !target.outputs.iter().any(|value| value == field) { target.outputs.push(field.into()); }
        }
        remove.insert(index);
    }
    if remove.is_empty() { return false; }
    request.capabilities = request.capabilities.drain(..).enumerate().filter_map(|(index, cap)| (!remove.contains(&index)).then_some(cap)).collect();
    true
}

/// Only unpaid intake recovery uses this. There is no quote or authorization
/// to reuse; discover the preserved requirements before creating a new quote.
pub(crate) fn repair_intake(intake: &mut crate::capability_intake::Intake, catalog: &crate::catalog::Catalog) -> bool {
    if !deduplicate(&mut intake.parsed_request) { return false; }
    intake.matches = crate::ask_v2::search::search(&intake.parsed_request, catalog);
    intake.selected_capabilities = intake.matches.iter().flat_map(|found| found.operations.clone()).collect();
    intake.catalog_version = catalog.version.clone();
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use crate::{catalog::Catalog, capability_intake::Intake, parser::contract_planner, state::Task, resolver::{self, Resolution}};

    #[test]
    fn recorded_paused_request_recovers_six_priced_steps_for_any_model_generated_name() {
        let saved: serde_json::Value = serde_json::from_str(include_str!("../../tests/fixtures/wwft-paused-intake.json")).unwrap();
        let original: Intake = serde_json::from_value(saved["intake"].clone()).unwrap();
        let question = saved["question"].as_str().unwrap();
        let catalog = Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/wwft-catalog.json")).unwrap());
        let before = json!({"matches":original.matches,"catalog_version":catalog.version});
        assert!(!crate::server::ask_v2::complete_local_plan(&catalog, question, &original.parsed_request, &before));
        for slug in ["company.nameSearch", "company.findByName", "registry.resolve", "company.zoek"] {
            let mut intake = original.clone(); intake.parsed_request.capabilities[0].slug = slug.into();
            let subjects = json!(intake.parsed_request.subjects);
            assert!(repair_intake(&mut intake, &catalog));
            assert_eq!(intake.parsed_request.capabilities.len(), 6);
            assert_eq!(json!(intake.parsed_request.subjects), subjects);
            let search = json!({"matches":intake.matches,"catalog_version":catalog.version});
            assert!(crate::server::ask_v2::complete_local_plan(&catalog, question, &intake.parsed_request, &search), "{slug}");
            let mut intent = contract_planner::compile(question, question, &Task::default(), &catalog, &intake).intent.unwrap();
            let mut task = Task { capability_intake:Some(intake.clone()), ..Default::default() };
            crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
            let Resolution::Plan(plan) = resolver::resolve(&catalog, &task, &intent) else { panic!("complete plan"); };
            assert_eq!(plan.steps.len(), 6); assert_eq!(plan.cap_atomic, 467392);
            assert!(!plan.independent_calls);
            assert!(!repair_intake(&mut intake, &catalog), "idempotent recovery");
        }
        for changed in ["provider", "outputs", "subject", "source_scope"] {
            let mut intake = original.clone();
            let cap = &mut intake.parsed_request.capabilities[0];
            match changed {
                "provider" => cap.source_hint = Some("another-provider".into()),
                "outputs" => cap.outputs.push("credit_risk_score".into()),
                "subject" => cap.subject_id = Some("another-subject".into()),
                _ => { cap.source_hint = Some("kvk".into()); cap.outputs.push("sanctions_matches".into()); },
            }
            assert!(!repair_intake(&mut intake, &catalog), "must retain distinct requirement: {changed}");
            assert_eq!(intake.parsed_request.capabilities.len(), 7);
        }
    }

    fn fixture(country: &str) -> (Catalog, Intake, String) {
        let question = format!("Doe een Wwft-check op Mollie in {country}.");
        let mut parsed_request = crate::capability_intake::normalize(&json!({"language":"nl",
            "subjects":[{"id":"s1","label":"Mollie","type":"company","identifiers":{"name":"Mollie","country":country}}],
            "capabilities":[{"slug":"compliance.wwft.check","name":"Wwft check","description":"Voer een Wwft-check uit","domain":"compliance","inputs":["name","country"],"outputs":["wwft_check"],"subject_id":"s1","source_hint":null,"confidence":1}],
            "deliverable":{"format":"chat","operations":["analyze"]}}).to_string(), &question, "fixture").unwrap();
        expand(&mut parsed_request).unwrap();
        let catalog = Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/wwft-catalog.json")).unwrap());
        assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
        let matches = crate::ask_v2::search::search(&parsed_request, &catalog);
        let intake = Intake { selected_capabilities: matches.iter().flat_map(|found| found.operations.clone()).collect(),
            parsed_request, matches, apiosk_sources_searched: catalog.known_sources.len(), catalog_version: catalog.version.clone() };
        (catalog, intake, question)
    }

    #[test]
    fn six_real_contracts_share_one_quote_and_only_explicit_dependencies() {
        let (catalog, intake, question) = fixture("Nederland");
        let reply = contract_planner::compile(&question, &question, &Task::default(), &catalog, &intake);
        assert!(contract_planner::ready(&question, &reply, &Task::default(), &catalog, &intake), "{reply:?}");
        let mut intent = reply.intent.unwrap();
        intent.validate(&question, &Task::default(), &catalog).unwrap();
        assert_eq!(intent.goals.len(), 6);
        assert_eq!(intent.subjects[0].values["company.search.country_code"], "NL");
        assert!(!intent.subjects[0].values.contains_key("company_registry.kvknummer"));
        assert!(!intent.subjects[0].values.contains_key("company.vestigingsnummer"));
        let mut task = Task { capability_intake: Some(intake), ..Default::default() };
        crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
        let Resolution::Plan(plan) = resolver::resolve(&catalog, &task, &intent) else { panic!("complete quote"); };
        assert_eq!(plan.steps.len(), 6);
        assert!(!plan.independent_calls);
        assert_eq!(plan.cap_atomic, plan.steps.iter().map(|step| step.service.total()).sum::<i64>());
        assert!(plan.steps.iter().all(|step| step.browser_fallback.is_none()));
        let position = |slug| plan.steps.iter().position(|step| step.service.contract.capability_slug == slug).unwrap();
        for consumer in ["company.registry-profile", "company.registered-names", "company.financial-statements"] {
            assert!(position("company_registry.search") < position(consumer));
        }
        assert!(position("company.registered-names") < position("company.branch-profile"));
    }

    #[test]
    fn incomplete_or_foreign_package_never_becomes_a_partial_quote() {
        let (catalog, mut intake, question) = fixture("Nederland");
        let mut foreign = intake.parsed_request.clone();
        foreign.capabilities.truncate(1);
        foreign.capabilities[0].slug = "compliance.wwft.check".into();
        foreign.subjects[0].identifiers.insert("country".into(), "Frankrijk".into());
        assert!(expand(&mut foreign).is_err());
        let names = intake.parsed_request.capabilities.iter().position(|cap| cap.slug == "company.registered-names").unwrap();
        intake.matches[names].apiosk.clear(); intake.matches[names].operations.clear();
        let reply = contract_planner::compile(&question, &question, &Task::default(), &catalog, &intake);
        assert!(!contract_planner::ready(&question, &reply, &Task::default(), &catalog, &intake));
    }

    #[test]
    fn multiple_establishments_are_not_silently_reduced_to_the_first() {
        let (catalog, _, _) = fixture("Nederland");
        let names = catalog.services.iter().find(|service| service.contract.capability_slug == "company.registered-names").unwrap();
        let mut payload = names.sample_payload.clone().unwrap();
        payload["vestigingen"].as_array_mut().unwrap().push(json!({"vestigingsnummer":"000012345678","eersteHandelsnaam":"Another branch"}));
        assert!(names.contract.extract(&payload, &catalog.vocabulary).is_err());
    }
    #[test]
    fn expands_the_complete_package_without_guessing_identifiers() {
        let mut request = crate::capability_intake::normalize(&json!({"language":"nl","subjects":[{"id":"s1","label":"Mollie","type":"company","identifiers":{"name":"Mollie","country":"Nederland"}}],
            "capabilities":[{"slug":"compliance.wwft.check","name":"Wwft check","description":"Voer een Wwft-check uit","domain":"compliance","inputs":["name","country"],"outputs":["wwft_check"],"subject_id":"s1","source_hint":null,"confidence":1}],
            "deliverable":{"format":"chat","operations":["analyze"]}}).to_string(), "Doe een Wwft-check op Mollie in Nederland", "fixture").unwrap();
        let subjects = serde_json::to_value(&request.subjects).unwrap();
        expand(&mut request).unwrap();
        assert_eq!(request.capabilities.len(), 6);
        assert_eq!(request.capabilities.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["KVK Zoeken","KVK Basisprofiel","KVK Vestigingsprofiel","KVK Naamgeving","KVK Jaarrekening","OpenSanctions"]);
        assert_eq!(serde_json::to_value(&request.subjects).unwrap(), subjects);
        assert_eq!(request.capabilities[1].missing, ["company_registry.kvknummer"]);
        let before = serde_json::to_value(&request).unwrap(); expand(&mut request).unwrap();
        assert_eq!(serde_json::to_value(&request).unwrap(), before);
    }

    #[test]
    fn repeated_registry_requirements_merge_without_losing_extra_requested_fields() {
        let (_, mut intake, question) = fixture("Nederland");
        let mut search = intake.parsed_request.capabilities[0].clone();
        search.slug = "company.registry.search".into();
        let mut profile = intake.parsed_request.capabilities[1].clone();
        profile.slug = "company.profile.retrieve".into();
        profile.outputs.push("company.nl.rsin".into());
        let mut marker = search.clone(); marker.slug = "compliance.wwft.check".into();
        intake.parsed_request.capabilities = vec![search, profile, marker];
        expand(&mut intake.parsed_request).unwrap();
        assert_eq!(intake.parsed_request.capabilities.len(), 6);
        assert!(intake.parsed_request.capabilities[1].outputs.contains(&"company.nl.rsin".into()));
        let catalog = Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/wwft-catalog.json")).unwrap());
        intake.matches = crate::ask_v2::search::search(&intake.parsed_request, &catalog);
        let reply = contract_planner::compile(&question, &question, &Task::default(), &catalog, &intake);
        assert!(contract_planner::ready(&question, &reply, &Task::default(), &catalog, &intake));
        // A separately named provider must not disappear into the KVK package.
        let mut other = intake.parsed_request.capabilities[1].clone();
        other.slug = "company.profile.retrieve".into(); other.source_hint = Some("another-provider".into());
        let mut marker = other.clone(); marker.slug = "compliance.wwft.check".into(); marker.source_hint = None;
        intake.parsed_request.capabilities = vec![marker, other];
        expand(&mut intake.parsed_request).unwrap();
        assert_eq!(intake.parsed_request.capabilities.len(), 7);
    }

    #[test]
    fn returned_registry_identifiers_bind_the_later_calls() {
        let (catalog, intake, question) = fixture("Nederland");
        let reply = contract_planner::compile(&question, &question, &Task::default(), &catalog, &intake);
        let mut intent = reply.intent.unwrap();
        let mut task = Task { capability_intake: Some(intake), ..Default::default() };
        crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
        let Resolution::Plan(plan) = resolver::resolve(&catalog, &task, &intent) else { panic!("chained plan"); };
        let profile = plan.steps.iter().find(|s| s.service.contract.capability_slug == "company.registry-profile").unwrap();
        assert!(profile.service.inputs(&task, profile.entity_ref, &catalog.vocabulary).is_err());
        for step in &plan.steps {
            let inputs = step.service.inputs(&task, step.entity_ref, &catalog.vocabulary).unwrap();
            match step.service.contract.capability_slug.as_str() {
                "company.registry-profile" | "company.registered-names" | "company.financial-statements" => assert!(inputs.to_string().contains("30204462"), "{inputs}"),
                "company.branch-profile" => assert!(inputs.to_string().contains("000018296932"), "{inputs}"),
                _ => (),
            }
            let result = step.service.contract.extract(step.service.sample_payload.as_ref().unwrap(), &catalog.vocabulary).unwrap();
            let subject = result.single().expect("unambiguous fixture");
            for field in &subject.facts {
                if field.fact_type == "company.name" { task.facts.retain(|known| known.semantic_type != "company.name"); }
                task.facts.push(crate::state::Fact { entity_ref:step.entity_ref,semantic_type:field.fact_type.clone(),value:field.value.clone(),
                    provenance:json!({"source":step.service.api_slug}),verification:"source_asserted".into(),observed_at:chrono::Utc::now(),expires_at:chrono::Utc::now()+chrono::Duration::hours(1) });
            }
        }
        let steps = resolver::describe_steps(&plan, &[]);
        assert_eq!(steps.iter().filter(|s| !s["dependencies"].as_array().unwrap().is_empty()).count(), 5);
    }

    #[test]
    fn named_registry_and_screening_quote_a_complete_chain_without_manual_source_selection() {
        let question = "Look up the KVK registration of Coolblue B.V. and check whether the company name has potential matches in OpenSanctions.";
        let mut parsed = crate::capability_intake::normalize(&json!({"language":"en","subjects":[{"id":"s1","label":"Coolblue B.V.","type":"company","identifiers":{"name":"Coolblue B.V."}}],
            "capabilities":[{"slug":"company.registry.lookup","name":"Company registry lookup","description":"Retrieve the KVK registration details for the specified company.","domain":"company","inputs":["company_name"],"outputs":["registration_details"],"subject_id":"s1","source_hint":null,"confidence":0.9},
            {"slug":"compliance.sanctions.check","name":"Sanctions screening","description":"Check the company name against OpenSanctions for potential matches.","domain":"compliance","inputs":["company_name"],"outputs":["sanctions_matches"],"subject_id":"s1","source_hint":null,"confidence":0.9}],"deliverable":{"format":"chat","operations":[]}}).to_string(), question, "fixture").unwrap();
        named_checks(&mut parsed, question).unwrap();
        assert_eq!(parsed.capabilities.len(), 3);
        let catalog = Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/wwft-catalog.json")).unwrap());
        let matches = crate::ask_v2::search::search(&parsed, &catalog);
        let search = json!({"matches":matches,"catalog_version":catalog.version});
        assert!(crate::server::ask_v2::complete_local_plan(&catalog, question, &parsed, &search));
    }
}
