//! An explicit data request is planned; it is never answered with a question
//! the price proposal already asks. When every requested capability names its
//! subject and matched an executable operation, the planning model may decline
//! only for a stated, genuine reason (conversation, an unclear request, or an
//! essential requirement no matched operation meets). A permission or
//! confirmation question ("Would you like to retrieve this now?") is not one:
//! the model is told once, and if it still declines the request is planned
//! from the source search itself. The resolver then asks the person only for
//! an input nothing can look up, as a `supply_input` action, and several
//! matches stop at `select_entity`: no dead-end reply without an action.
use super::{ChatReply, Goal, Intent, Subject};
use crate::{
    capability_intake::{Intake, Subject as ParsedSubject},
    catalog::Catalog,
};
use serde_json::json;
use std::collections::BTreeMap;

/// Why the planner returns no plan. `none` accompanies a plan.
pub(super) const DECLINE_REASONS: &[&str] = &["none", "conversation", "ambiguous_request", "unavailable_requirement"];
/// The validation error that triggers one repair attempt.
pub(super) const DECLINED: &str = "an explicit data request needs its plan, not a confirmation";
pub(super) const DECLINED_REPAIR: &str = "The request is an explicit data request: every requested capability names its subject and has a matched Apiosk operation. Do not ask for permission or confirmation; the backend's total-price proposal is the confirmation, and the person approves it there. A registry number or other identifier the user did not give is looked up by the backend from the company name: bind company.name to the subject's label reference. Return discover=true, decline_reason=\"none\" and the complete intent.";
const PLANNED_MESSAGE: &str = "I'll check the sources and show the total price before anything is bought.";

/// Every requested capability is a data retrieval with a parsed subject and
/// an executable Apiosk match. Conversation, a vague analysis and unmatched
/// requirements stay with the model's clarification.
pub(super) fn data_request(intake: &Intake) -> bool {
    let parsed = &intake.parsed_request;
    !parsed.capabilities.is_empty()
        && parsed.capabilities.iter().enumerate().all(|(index, cap)| {
            cap.domain != "general"
                && cap.subject_id.as_ref().is_some_and(|id| parsed.subjects.iter().any(|s| &s.id == id))
                && request_match(intake,index).is_some_and(|m| !m.operations.is_empty())
        })
}

/// Search returns one match per parsed request instance, not per unique slug.
pub(super) fn request_match(intake: &Intake, index: usize) -> Option<&crate::capability_intake::Match> {
    let cap=intake.parsed_request.capabilities.get(index)?;
    let count=intake.parsed_request.capabilities.iter().filter(|c|c.slug==cap.slug).count();
    if intake.matches.iter().filter(|m|m.slug==cap.slug).count()!=count { return None; }
    let occurrence=intake.parsed_request.capabilities[..index].iter().filter(|c|c.slug==cap.slug).count();
    intake.matches.iter().filter(|m|m.slug==cap.slug).nth(occurrence)
}

/// A stated reason other than `none`, whose message is not a request for
/// permission to go ahead.
pub(super) fn genuine_decline(reason: Option<&str>, message: &str) -> bool {
    matches!(reason, Some("conversation" | "ambiguous_request" | "unavailable_requirement")) && !asks_permission(message)
}

/// A yes/no question about going ahead, in the languages the Ask serves.
fn asks_permission(message: &str) -> bool {
    const PHRASES: &[&str] = &[
        "would you like me to", "do you want me to", "would you like to retrieve", "would you like to proceed",
        "would you like to see", "would you like to get", "do you want to proceed", "do you want to retrieve",
        "shall i proceed", "shall i retrieve", "should i proceed", "should i retrieve", "shall i go ahead",
        "wil je dat ik", "wilt u dat ik", "zal ik ", "wil je doorgaan", "wilt u doorgaan", "zullen we ",
        "soll ich", "möchten sie, dass ich", "sollen wir", "voulez-vous que je", "souhaitez-vous que je", "dois-je",
    ];
    let lower = message.to_lowercase();
    lower.contains('?') && PHRASES.iter().any(|phrase| lower.contains(phrase))
}

/// The plan the search found: each parsed capability's best-ranked matched
/// operation, from its source, for its parsed subject. Identifiers the user
/// typed are bound where the operation takes them; a missing one is left to
/// the backend's lookup or to the person. None when nothing can be planned.
pub(super) fn planned_reply(intake: &Intake, catalog: &Catalog) -> Option<ChatReply> {
    let parsed = &intake.parsed_request;
    let mut subjects: Vec<Subject> = parsed.subjects.iter()
        .map(|s| Subject { entity_ref: None, label: s.label.clone(), values: BTreeMap::new() })
        .collect();
    let mut goals = Vec::new();
    for (index,cap) in parsed.capabilities.iter().enumerate() {
        let subject = parsed.subjects.iter().position(|s| cap.subject_id.as_deref() == Some(s.id.as_str()))?;
        let found = request_match(intake,index)?;
        // Candidates are in the search's ranking; the operation set is not.
        let best = found.apiosk.iter().find(|c| c.capability.as_ref().is_some_and(|op| found.operations.contains(op)))?;
        let capability = best.capability.clone()?;
        bind_identifiers(&mut subjects[subject], &parsed.subjects[subject], &capability, best.source_slug.as_deref(), catalog);
        let constraints = best.source_slug.iter().map(|source| ("source".to_owned(), json!(source))).collect();
        goals.push(Goal { capability, subject, constraints });
    }
    let answer_operations = if parsed.deliverable.operations.is_empty() { vec![] }
        else { vec!["Answer the request from the retrieved source data.".to_owned()] };
    Some(ChatReply { message: PLANNED_MESSAGE.into(), discover: true, decline_reason: Some("none".into()),
        intent: Some(Intent { subjects, goals, answer_operations, unresolved_references: vec![] }) })
}

/// A typed identifier (`kvk_number: 24330087`) fills a required input of the
/// chosen operation only when the input names the same scheme and the value
/// passes its schema; anything else stays unbound rather than guessed.
fn bind_identifiers(subject: &mut Subject, parsed: &ParsedSubject, capability: &str, source: Option<&str>, catalog: &Catalog) {
    // Bind names even when the selected report consumes an identifier: its
    // dependency may resolve that identifier from the literal company name.
    for field in ["company.name", "geo.input.q"] {
        if let Some(value) = crate::ask_v2::input_values::literal(field, parsed).filter(|value|
            catalog.vocabulary.get(field).is_some_and(|fact| catalog_core::facts::schema::validate(&fact.value_schema, value).is_ok())) {
            subject.values.entry(field.into()).or_insert(value);
        }
    }
    for service in catalog.services.iter().filter(|s| s.contract.capability_slug == capability && source.is_none_or(|source| s.api_slug == source)) {
        for required in &service.contract.requires {
            let field = &required.fact_type;
            if let Some(value) = crate::ask_v2::input_values::literal(field, parsed).filter(|value|
                catalog.vocabulary.get(field).is_some_and(|fact| catalog_core::facts::schema::validate(&fact.value_schema, value).is_ok())) {
                subject.values.entry(field.clone()).or_insert(value);
            }
        }
    }
    for (key, value) in &parsed.identifiers {
        let scheme = key.split('_').next().unwrap_or(key).to_lowercase();
        if scheme.len() < 2 {
            continue;
        }
        for service in catalog.services.iter().filter(|s| s.contract.capability_slug == capability && source.is_none_or(|source| s.api_slug == source)) {
            for required in service.contract.requires.iter().filter(|r| !r.optional) {
                let names_scheme = required.fact_type.to_lowercase().replace(['_', '-'], "").contains(&scheme);
                let valid = catalog.vocabulary.get(&required.fact_type)
                    .is_some_and(|fact| catalog_core::facts::schema::validate(&fact.value_schema, &json!(value)).is_ok());
                if names_scheme && valid {
                    subject.values.entry(required.fact_type.clone()).or_insert_with(|| json!(value));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability_intake::{self, Candidate, Match};

    const QUESTION: &str = "What are the registered name, status and address of Coolblue B.V. in the Dutch KVK register?";
    const KVK: &str = "kvk-dutch-business-register";

    fn catalog() -> Catalog {
        Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/workflows/catalog.json")).unwrap())
    }

    fn candidate(capability: &str) -> Candidate {
        Candidate { origin: "apiosk".into(), name: "KVK Business Register".into(), description: String::new(),
            source_slug: Some(KVK.into()), capability: Some(capability.into()), endpoint_id: None, url: None,
            method: "GET".into(), price: None, network: None, calls_30d: None, availability: "supported".into() }
    }

    /// The live report's intake: one KVK profile capability, KVK number missing.
    fn kvk_intake(identifiers: serde_json::Value, question: &str) -> Intake {
        let parsed = capability_intake::normalize(&json!({"language":"en",
            "subjects":[{"id":"s1","label":"Coolblue B.V.","type":"company","identifiers":identifiers}],
            "capabilities":[{"slug":"company.registry.profile","name":"Company registry profile","description":"Retrieve a company's registered name, status and address from a business register.","domain":"company","inputs":["kvk_number"],"outputs":["registered_name","status","address"],"subject_id":"s1","source_hint":"kvk","confidence":0.95}],
            "deliverable":{"format":"chat","operations":[]}}).to_string(), question, "fixture").unwrap();
        let ranked = ["company.registry-profile", "company.branch-profile", "company.registered-names"];
        Intake { parsed_request: parsed,
            matches: vec![Match { slug: "company.registry.profile".into(), query: String::new(),
                operations: ranked.iter().map(|op| op.to_string()).collect(),
                apiosk: ranked.iter().map(|op| candidate(op)).collect(), coinbase: vec![], coinbase_status: "skipped".into() }],
            apiosk_sources_searched: 1, catalog_version: String::new(),
            selected_capabilities: ranked.iter().map(|op| op.to_string()).collect() }
    }

    #[test]
    fn a_confirmation_is_never_a_reason_to_stop() {
        let confirmation = "The KVK Business Register can provide the registered name, status, and address for Coolblue B.V. Would you like to retrieve this information now?";
        for reason in [None, Some("none"), Some("ambiguous_request"), Some("unavailable_requirement")] {
            assert!(!genuine_decline(reason, confirmation), "{reason:?}");
        }
        assert!(!genuine_decline(Some("ambiguous_request"), "Zal ik de jaarrekening voor je ophalen?"));
        assert!(genuine_decline(Some("ambiguous_request"), "Welke gegevens wil je controleren?"));
        assert!(genuine_decline(Some("conversation"), "Hallo! Waar kan ik je mee helpen?"));
        assert!(genuine_decline(Some("unavailable_requirement"), "KVK does not publish salaries. Which registered data do you need?"));
    }

    #[test]
    fn only_matched_data_retrievals_with_a_subject_are_explicit_requests() {
        let intake = kvk_intake(json!({}), QUESTION);
        assert!(data_request(&intake));
        let mut unmatched = intake.clone();
        unmatched.matches[0].operations.clear();
        assert!(!data_request(&unmatched));
        let mut subjectless = intake.clone();
        subjectless.parsed_request.capabilities[0].subject_id = None;
        assert!(!data_request(&subjectless));
        let mut vague = intake;
        vague.parsed_request.capabilities[0].domain = "general".into();
        assert!(!data_request(&vague));
    }

    #[test]
    fn the_search_plans_the_best_ranked_operation_from_its_source() {
        let catalog = catalog();
        let reply = planned_reply(&kvk_intake(json!({}), QUESTION), &catalog).unwrap();
        assert!(reply.discover);
        let intent = reply.intent.unwrap();
        assert_eq!(intent.subjects[0].label, "Coolblue B.V.");
        assert_eq!(intent.subjects[0].values.get("company.name"), Some(&json!("Coolblue B.V.")));
        assert!(!intent.subjects[0].values.contains_key("company_registry.kvknummer"), "the number is never guessed");
        assert_eq!(intent.goals.len(), 1);
        assert_eq!(intent.goals[0].capability, "company.registry-profile");
        assert_eq!(intent.goals[0].constraints["source"], KVK);
        intent.validate(QUESTION, &crate::state::Task::default(), &catalog).unwrap();
    }

    #[test]
    fn a_typed_registry_number_is_bound_to_the_operation_that_takes_it() {
        let question = format!("{QUESTION} KVK 24330087");
        let catalog = catalog();
        let intent = planned_reply(&kvk_intake(json!({"kvk_number":"24330087"}), &question), &catalog).unwrap().intent.unwrap();
        assert_eq!(intent.subjects[0].values["company_registry.kvknummer"], "24330087");
        let intent = planned_reply(&kvk_intake(json!({"vat_number":"24330087"}), &question), &catalog).unwrap().intent.unwrap();
        assert!(!intent.subjects[0].values.contains_key("company_registry.kvknummer"), "another scheme's number is not a KVK number");
    }

    #[test]
    fn repeated_capabilities_keep_each_subjects_source_and_missing_match() {
        let mut intake=kvk_intake(json!({}),QUESTION);
        let mut second=intake.parsed_request.subjects[0].clone();
        second.id="s2".into(); second.label="MONZO BANK LIMITED".into();
        intake.parsed_request.subjects.push(second);
        let mut cap=intake.parsed_request.capabilities[0].clone();
        cap.subject_id=Some("s2".into());cap.source_hint=Some("companies house".into());
        intake.parsed_request.capabilities.push(cap);
        let mut found=intake.matches[0].clone();
        for candidate in &mut found.apiosk {candidate.source_slug=Some("companies-house".into());}
        intake.matches.push(found);
        let planned=planned_reply(&intake,&catalog()).unwrap().intent.unwrap();
        assert_eq!(planned.goals[0].constraints["source"],KVK);
        assert_eq!(planned.goals[1].constraints["source"],"companies-house");
        assert_eq!(planned.goals[1].subject,1);
        assert_eq!(planned.subjects[1].values["company.name"],"MONZO BANK LIMITED");
        intake.matches[1].operations.clear();
        assert!(!data_request(&intake));
        assert!(planned_reply(&intake,&catalog()).is_none());
        intake.matches.pop();
        assert!(!data_request(&intake),"a collapsed match must not satisfy both subjects");
    }

    /// No network or model: intentionally separates authored extraction fixtures
    /// from the actual search, fallback planner, input binder and quote compiler.
    #[test]
    #[ignore = "requires PROFESSION_AUDIT_DIR and a read-only CATALOG_SNAPSHOT"]
    fn audit_profession_search_and_fallback_planning() {
        use crate::{catalog::Snapshot, engine::bind_subjects, resolver::{resolve, Resolution}, state::Task};
        let dir = std::path::PathBuf::from(std::env::var("PROFESSION_AUDIT_DIR").unwrap());
        let cases: Vec<serde_json::Value> = serde_json::from_slice(&std::fs::read(dir.join("cases.json")).unwrap()).unwrap();
        let snapshot: Snapshot = serde_json::from_slice(&std::fs::read(std::env::var("CATALOG_SNAPSHOT").unwrap()).unwrap()).unwrap();
        let catalog = Catalog::build(snapshot);
        std::fs::write(dir.join("rejected-contracts.json"),serde_json::to_vec_pretty(&catalog.rejected).unwrap()).unwrap();
        let mut reports = vec![];
        for case in cases {
            let question = case["question"].as_str().unwrap();
            let parsed = capability_intake::normalize(&case["authored_parse_fixture"].to_string(), question, "authored-fixture-no-model").unwrap();
            let matches = crate::ask_v2::search::search(&parsed, &catalog);
            let selected = matches.iter().flat_map(|m| m.operations.iter().cloned()).collect();
            let intake = Intake {parsed_request:parsed,matches,selected_capabilities:selected,apiosk_sources_searched:capability_intake::source_count(&catalog),catalog_version:catalog.version.clone()};
            let mut report = json!({"case":case["case"],"question":question,"intake":intake,"mode":"Authored extraction fixture; actual deterministic search, fallback planner, binder and resolver. No model, network, provider execution or purchases."});
            let mut task = Task::default();
            if let Some(reply) = planned_reply(&intake, &catalog) {
                let mut intent = reply.intent.unwrap();
                let validation = intent.validate(question, &task, &catalog).and_then(|_| bind_subjects(&mut task, &catalog, &mut intent));
                match validation {
                    Err(error) => {report["status"]=json!("invalid");report["error"]=json!(error.to_string());}
                    Ok(()) => {
                        report["intent"]=json!(intent);
                        let missing_goals:Vec<_>=case["required_capability_groups"].as_array().unwrap().iter().filter(|group|
                            !group.as_array().unwrap().iter().any(|cap|intent.goals.iter().any(|goal|Some(goal.capability.as_str())==cap.as_str()))).cloned().collect();
                        report["all_expected_retrieval_goals_planned"]=json!(missing_goals.is_empty());
                        report["missing_retrieval_goal_groups"]=json!(missing_goals);
                        match resolve(&catalog,&task,&intent) {
                            Resolution::Plan(plan) => {report["status"]=json!("quoted");report["cap_atomic"]=json!(plan.cap_atomic);
                                report["steps"]=json!(plan.steps.iter().map(|step|json!({"source":step.service.api_slug,"capability":step.service.contract.capability_slug,"inputs":step.service.inputs(&task,step.entity_ref,&catalog.vocabulary).ok()})).collect::<Vec<_>>());}
                            Resolution::Missing {kind,..} => {report["status"]=json!("needs_input");report["missing_input"]=json!(kind);}
                            Resolution::Unsupported(reason) => {report["status"]=json!("unsupported");report["reason"]=json!(reason);}
                        }
                    }
                }
            } else {report["status"]=json!("no_complete_plan");}
            let missing: Vec<_> = case["required_capability_groups"].as_array().unwrap().iter().filter(|group|
                !group.as_array().unwrap().iter().any(|cap|intake.matches.iter().any(|m|m.operations.contains(cap.as_str().unwrap())))).cloned().collect();
            report["expected_operations_discovered"]=json!(missing.is_empty());
            report["missing_expected_operation_groups"]=json!(missing);
            println!("case {} {} expected_operations={}",case["case"],report["status"],report["expected_operations_discovered"]);
            reports.push(report);
        }
        let run = std::env::var("PROFESSION_RUN").unwrap_or_else(|_|"baseline".into());
        std::fs::write(dir.join(format!("offline-{run}.json")),serde_json::to_vec_pretty(&reports).unwrap()).unwrap();
        assert_eq!(reports.len(),20,"audit fixture must cover all PDF professions");
    }
}
