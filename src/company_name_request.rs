//! A named-company request keeps the name literal and leaves identifiers and
//! website searches to the source dependency graph and its normal selection rules.
use crate::capability_intake::{Capability, Deliverable, Intake, Meta, ParsedRequest, Subject};
use sha2::{Digest, Sha256};

const PREFIX: &str = "Look up ";
const SUFFIX: &str = " by name and combine its registered details, available annual financial figures and website information";
pub(crate) const WEBSITE_QUERY: &str = "web.company_website_query";
pub(crate) const ANSWER_SCOPE: &str = "Combine the selected company's registered details, available annual financial figures and sourced website information into one cited answer. Preserve the resolved company identity, financial year, units, currency, consolidation scope and missing fields. Website search uses the registered name and KVK number returned for the selected company; verify the cited pages' identity before attributing statements to that company. Search results alone do not establish domain ownership or independently verify website claims. Do not infer a domain from the company name. State unavailable website information and identity uncertainty. Available figures do not establish the latest filing or a complete original annual report.";

pub(crate) fn parse(question: &str, model: &str) -> Option<ParsedRequest> {
    let text = crate::capability_intake::literal_sentence(question);
    let text = text.strip_suffix('.').unwrap_or(text);
    let end = text.len().checked_sub(SUFFIX.len())?;
    if !text.get(..PREFIX.len())?.eq_ignore_ascii_case(PREFIX)
        || !text.get(end..)?.eq_ignore_ascii_case(SUFFIX)
    {
        return None;
    }
    let label = crate::company_request::literal_name(text.get(PREFIX.len()..end)?)?;
    let capability = |slug: &str, name: &str, domain: &str, source: &str, output: &str, description: &str| Capability {
        slug: slug.into(), name: name.into(), description: description.into(), domain: domain.into(),
        inputs: vec!["company_name".into()], outputs: vec![output.into()], subject_id: Some("s1".into()),
        source_hint: Some(source.into()), confidence: 1.0, missing: vec![],
    };
    Some(ParsedRequest {
        language: "en".into(),
        subjects: vec![Subject { id: "s1".into(), label, kind: "company".into(), identifiers: Default::default() }],
        capabilities: vec![
            capability("company.registry.profile", "Company registered details", "company", "kvk", "profile",
                "Find the company by its supplied name in KVK, resolve or explicitly select its KVK number, and retrieve the available official registered profile. Preserve missing fields."),
            capability("company.financial.statements", "Available annual financial figures", "company", "kvk", "financial_statements",
                "Retrieve the available annual financial figures for that same resolved KVK number. Preserve the reported year, currency, units and consolidation scope; do not claim these are the latest filing or a complete original report."),
            Capability { outputs: vec!["answer".into(), "sources".into()], ..capability("web.company.website.search", "Company website information", "web", "linkup", "answer",
                "Search for website information through Linkup using the registered name and KVK number returned by the selected company's KVK profile. Cite the returned sources and verify the company identity before attributing their claims; a search result does not establish domain ownership.") },
        ],
        deliverable: Deliverable { format: "chat".into(), operations: vec!["summarize".into(), "analyze".into()] },
        meta: Meta { model: model.into(), parsed_at: chrono::Utc::now().to_rfc3339(),
            text_sha256: hex::encode(Sha256::digest(question.as_bytes())) },
    })
}

pub(crate) fn executable_intake(question: &str, intake: &Intake) -> Option<ParsedRequest> {
    let expected = parse(question, "literal-company-name-request")?;
    let actual = &intake.parsed_request;
    (expected.language == actual.language
        && serde_json::to_value(&expected.subjects).ok()? == serde_json::to_value(&actual.subjects).ok()?
        && serde_json::to_value(&expected.capabilities).ok()? == serde_json::to_value(&actual.capabilities).ok()?
        && serde_json::to_value(&expected.deliverable).ok()? == serde_json::to_value(&actual.deliverable).ok()?)
        .then_some(expected)
}

/// The derived query may only leave the register when the returned profile
/// belongs to the identifier that was actually selected and requested.
pub(crate) fn query_identity_gap(service: &crate::catalog::Service, inputs: &serde_json::Value, data: &serde_json::Value) -> Option<&'static str> {
    if service.api_slug != "kvk-dutch-business-register"
        || service.method != "GET" || service.path != "/api/v1/basisprofielen"
        || !service.contract.output_map.produces.contains_key(WEBSITE_QUERY)
    {
        return None;
    }
    let valid = inputs["kvkNummer"].as_str().is_some_and(|number|
        number.len() == 8 && number.bytes().all(|byte| byte.is_ascii_digit())
            && data["kvkNummer"].as_str() == Some(number));
    (!valid).then_some("company_registry.kvknummer_source_identity_mismatch")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use crate::{catalog::{Catalog, Snapshot}, parser::Parser, resolver::Resolution, state::{Fact, Task}};
    use std::{collections::BTreeSet, sync::atomic::Ordering};
    pub(super) const QUESTION: &str = "Look up Mollie B.V. by name and combine its registered details, available annual financial figures and website information.";
    const QUESTION_FORM: &str = "Can you look up Mollie B.V. by name and combine its registered details, available annual financial figures and website information?";

    fn catalog() -> Catalog {
        let original = crate::compound_examples_tests::catalog();
        let mut services = original.services.clone();
        let migration = include_str!("../supabase/migrations/20261004182009_kvk_registered_website_bridge.sql");
        let mut output: serde_json::Value = serde_json::from_str(migration.split("$expected_output$").nth(1).unwrap()).unwrap();
        output["produces"][WEBSITE_QUERY] = serde_json::from_str(migration.split("$query_output$").nth(1).unwrap()).unwrap();
        // The new reviewed contracts retain the existing source routes, prices
        // and response schemas. Only their explicit dependency fact changes.
        for service in services.iter_mut().filter(|service| service.api_slug == "kvk-dutch-business-register"
            && service.path == "/api/v1/basisprofielen") {
            service.contract.version = 9;
            service.contract.output_map = serde_json::from_value(output.clone()).unwrap();
            service.optional_outputs = service.contract.output_map.produces.keys()
                .filter(|field| field.starts_with("company.nl.") || field.as_str() == "company.profile" || field.as_str() == WEBSITE_QUERY)
                .cloned().collect();
        }
        let mut website = services.iter().find(|service| service.api_slug == "linkup"
            && service.contract.capability_slug == "web.linkup-answer").unwrap().clone();
        website.id = uuid::Uuid::from_u128(9009);
        website.contract.capability_slug = "web.company-website-search".into();
        website.description = Some("Search for website information using the selected registered company name and KVK number.".into());
        website.contract.requires[0].fact_type = WEBSITE_QUERY.into();
        let mut binding = website.contract.input_map.remove("web.source_search_query").unwrap();
        binding.location = Some("body".into());
        website.contract.input_map.insert(WEBSITE_QUERY.into(), binding);
        services.push(website);
        let mut vocabulary: Vec<_> = original.vocabulary.values().cloned().collect();
        let mut query = original.vocabulary["web.source_search_query"].clone();
        query.slug = WEBSITE_QUERY.into();
        query.value_schema = json!({"type":"string","minLength":1,"maxLength":1000});
        vocabulary.push(query);
        let catalog = Catalog::build(Snapshot { services, vocabulary, known_sources: original.known_sources.clone() });
        assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
        catalog
    }

    #[tokio::test]
    async fn named_company_plans_one_quote_with_source_bound_website_search() {
        let catalog = catalog();
        let parser = Parser::new(reqwest::Client::new(), "offline://model-calls-forbidden".into(), String::new(), "fixture".into());
        for (name, selected_name, number, form) in [("Mollie B.V.", "Mollie B.V.", "30204462"),
            ("Bas Grave Web Development", "Bas Grave Web Development", "87560488"),
            ("Atelier De Blauwe Maan", "Atelier De Blauwe Maan B.V.", "00123456")].into_iter()
            .flat_map(|(name, selected, number)| [QUESTION, QUESTION_FORM].map(|form| (name, selected, number, form))) {
            let question = form.replace("Mollie B.V.", name);
            let mut task = Task::default();
            let intake = parser.ask_intake(&question, &task, &catalog, None).await.unwrap();
            assert_eq!(intake.parsed_request.meta.text_sha256, hex::encode(Sha256::digest(question.as_bytes())));
            let mut expected = serde_json::to_value(parse(&QUESTION.replace("Mollie B.V.", name), "fixture").unwrap()).unwrap();
            let mut actual = serde_json::to_value(&intake.parsed_request).unwrap();
            expected.as_object_mut().unwrap().remove("meta"); actual.as_object_mut().unwrap().remove("meta");
            assert_eq!(actual, expected, "the question form preserves every semantic field");
            assert!(intake.parsed_request.subjects[0].identifiers.is_empty());
            let reply = parser.chat_from_capabilities(&question, None, &task, &catalog, &intake).await.unwrap();
            assert!(reply.discover, "{question}: {}", reply.message);
            let mut intent = reply.intent.unwrap();
            assert_eq!(intent.subjects.len(), 1);
            assert_eq!(intent.subjects[0].values, std::collections::BTreeMap::from([("company.name".into(), json!(name))]));
            assert_eq!(intent.answer_operations, [ANSWER_SCOPE]);
            task.capability_intake = Some(intake);
            crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
            let Resolution::Plan(plan) = crate::resolver::resolve(&catalog, &task, &intent) else { panic!("full company plan must resolve") };
            let operations: Vec<_> = plan.steps.iter().map(|step| step.service.contract.capability_slug.as_str()).collect();
            assert_eq!(operations, ["company_registry.search", "company.registry-profile", "company.financial-statements", "web.company-website-search"]);
            assert_eq!(plan.steps.iter().map(|step| step.service.api_slug.as_str()).collect::<BTreeSet<_>>(), BTreeSet::from(["kvk-dutch-business-register", "linkup"]));
            assert_eq!(plan.cap_atomic, plan.steps.iter().map(|step| plan.step_total(step)).sum::<i64>());
            let entity = intent.subjects[0].entity_ref.unwrap();
            assert!(plan.steps.iter().all(|step| step.entity_ref == entity));
            assert_eq!(plan.steps[0].service.inputs(&task, entity, &catalog.vocabulary).unwrap()["naam"], name);
            assert_eq!(plan.steps[0].service.contract.cardinality, catalog_core::facts::contract::Cardinality::Many);
            for step in &plan.steps[1..] {
                assert!(step.service.inputs(&task, entity, &catalog.vocabulary).is_err(), "no dependent call may use an invented identifier/query");
            }
            assert!(!task.facts.iter().any(|fact| fact.semantic_type == WEBSITE_QUERY || fact.semantic_type == "web.source_search_query"));
            let described = crate::resolver::describe_steps(&plan, &[]);
            assert_eq!(described[3]["dependencies"][0]["field"], WEBSITE_QUERY);
            assert_eq!(described[3]["dependencies"][0]["step"], 2);

            // Use actual contract extraction to simulate the selected profile,
            // including a returned legal name different from the search label.
            let raw = json!({"naam":selected_name,"statutaireNaam":selected_name,"kvkNummer":number,"handelsnamen":[],"sbiActiviteiten":[]});
            let profile = &plan.steps[1].service;
            assert_eq!(query_identity_gap(profile, &json!({"kvkNummer":number}), &raw), None);
            let extracted = profile.contract.extract(&raw, &catalog.vocabulary).unwrap();
            let query = extracted.subjects.iter().flat_map(|subject| &subject.facts).find(|fact| fact.fact_type == WEBSITE_QUERY).unwrap();
            assert_eq!(query.value, format!("\"{selected_name}\" KVK {number} official website products services"));
            assert!(!query.value.as_str().unwrap().contains("https://"));
            let now = chrono::Utc::now();
            let fact = |subject| Fact { entity_ref: subject, semantic_type: WEBSITE_QUERY.into(), value:query.value.clone(),
                provenance:json!({"kind":"provider_response","result_ref":uuid::Uuid::new_v4()}), observed_at:now,
                expires_at:now + chrono::Duration::hours(1), verification:"source_observed".into() };
            task.facts.push(fact(uuid::Uuid::new_v4()));
            assert!(plan.steps[3].service.inputs(&task, entity, &catalog.vocabulary).is_err(), "another company's query cannot unblock this source");
            task.facts.push(fact(entity));
            let web_inputs = plan.steps[3].service.inputs(&task, entity, &catalog.vocabulary).unwrap();
            assert_eq!(web_inputs, json!({"q":query.value,"depth":"standard","outputType":"sourcedAnswer"}));
        }
        assert_eq!(parser.calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn names_are_editable_and_never_supply_an_identifier_or_domain() {
        for name in ["Mollie B.V.", "Bas Grave Web Development", "O'Neill & Partners", "Atelier De Blauwe Maan"] {
            let question = QUESTION.replace("Mollie B.V.", name);
            let parsed = parse(&question, "fixture").unwrap();
            assert_eq!(parsed.subjects.len(), 1);
            assert_eq!(parsed.subjects[0].label, name);
            assert!(parsed.subjects[0].identifiers.is_empty());
            assert_eq!(parsed.capabilities.len(), 3);
            assert!(parsed.capabilities.iter().all(|cap| cap.subject_id.as_deref() == Some("s1")));
            assert_eq!(parsed.capabilities[2].slug, "web.company.website.search");
            assert_eq!(parsed.deliverable.operations, ["summarize", "analyze"]);
        }
    }

    #[test]
    fn extra_scope_and_ambiguous_names_are_not_silently_discarded() {
        for question in [
            format!("{QUESTION} Include salaries."),
            format!("{QUESTION_FORM} Can you include salaries?"),
            QUESTION_FORM.replace("website information?", "website information and credit rating?"),
            QUESTION.replace("Mollie B.V.", "Mollie B.V. and show ownership"),
            QUESTION.replace("Mollie B.V.", "Mollie B.V. (KVK 30204462)"),
            QUESTION.replace("Mollie B.V.", "Mollie B.V. and https://example.org"),
            QUESTION.replace("available annual financial figures", "the latest complete audited annual report"),
            QUESTION.replace("website information", "website information and credit rating"),
        ] {
            assert!(parse(&question, "fixture").is_none(), "{question}");
        }
    }

    #[test]
    fn displayed_intake_must_keep_the_whole_name_and_requested_scope() {
        let parsed_request = parse(QUESTION, "fixture").unwrap();
        let original = Intake { parsed_request, matches: vec![], selected_capabilities: Default::default(),
            apiosk_sources_searched: 0, catalog_version: "fixture".into() };
        assert!(executable_intake(QUESTION, &original).is_some());
        for mutation in ["name", "identifier", "website", "field", "operation", "source", "second_company"] {
            let mut intake = original.clone();
            let parsed = &mut intake.parsed_request;
            match mutation {
                "name" => parsed.subjects[0].label = "Another Company".into(),
                "identifier" => { parsed.subjects[0].identifiers.insert("kvk_number".into(), "30204462".into()); },
                "website" => { parsed.capabilities.pop(); },
                "field" => parsed.capabilities[0].outputs.push("email".into()),
                "operation" => parsed.deliverable.operations.push("credit_risk".into()),
                "source" => parsed.capabilities[2].source_hint = Some("different-provider".into()),
                _ => parsed.subjects.push(parsed.subjects[0].clone()),
            }
            assert!(executable_intake(QUESTION, &intake).is_none(), "{mutation}");
        }
    }

    #[test]
    fn derived_query_only_binds_for_the_selected_and_executed_registry_identity() {
        let catalog = crate::compound_examples_tests::catalog();
        let old = catalog.services.iter().find(|service| service.api_slug == "kvk-dutch-business-register"
            && service.contract.capability_slug == "company.registry-profile").unwrap();
        let mut service = old.clone();
        service.contract.output_map.produces.insert(WEBSITE_QUERY.into(),
            serde_json::from_value(json!({"path":"$.test_query"})).unwrap());
        for number in ["30204462", "00123456"] {
            let inputs = json!({"kvkNummer":number});
            assert_eq!(query_identity_gap(&service, &inputs, &json!({"kvkNummer":number})), None);
            for body in [json!({}), json!({"kvkNummer":null}), json!({"kvkNummer":123456}),
                json!({"kvkNummer":"87654321"}), json!({"kvkNummer":"000123456"})] {
                assert_eq!(query_identity_gap(&service, &inputs, &body), Some("company_registry.kvknummer_source_identity_mismatch"));
                assert_eq!(query_identity_gap(old, &inputs, &body), None, "old contracts keep their existing behavior");
            }
        }
        for inputs in [json!({}), json!({"kvkNummer":30204462}), json!({"kvkNummer":"3020446"})] {
            assert!(query_identity_gap(&service, &inputs, &json!({"kvkNummer":"30204462"})).is_some());
        }
        service.api_slug = "different-provider".into();
        assert_eq!(query_identity_gap(&service, &json!({}), &json!({})), None);
    }
}
