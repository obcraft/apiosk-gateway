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
        && parsed.capabilities.iter().all(|cap| {
            cap.domain != "general"
                && cap.subject_id.as_ref().is_some_and(|id| parsed.subjects.iter().any(|s| &s.id == id))
                && intake.matches.iter().any(|m| m.slug == cap.slug && !m.operations.is_empty())
        })
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
    for cap in &parsed.capabilities {
        let subject = parsed.subjects.iter().position(|s| cap.subject_id.as_deref() == Some(s.id.as_str()))?;
        let found = intake.matches.iter().find(|m| m.slug == cap.slug)?;
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
        assert!(intent.subjects[0].values.is_empty(), "the name lookup is the engine's, the number is never guessed");
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
        assert!(intent.subjects[0].values.is_empty(), "another scheme's number is not a KVK number");
    }
}
