//! Each discovered retrieval must be covered by a matched, subject-specific
//! goal. Shared goals require explicit output-contract evidence; dependencies
//! belong to the resolver and answer instructions do not require a purchase.
use super::{explicit_request::request_match, Intent};
use crate::{capability_intake::Intake, catalog::Catalog};
use anyhow::ensure;
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub(super) fn prepare(
    intent: &mut Intent,
    intake: &Intake,
    catalog: &Catalog,
) -> anyhow::Result<()> {
    let parsed = &intake.parsed_request;
    let retrievals: Vec<_> = parsed
        .capabilities
        .iter()
        .enumerate()
        .filter(|(_, cap)| !answer_instruction(cap))
        .map(|(index, _)| index)
        .collect();
    ensure!(
        !intent.goals.is_empty() && intent.goals.len() <= retrievals.len(),
        "each requested retrieval needs a covered goal"
    );
    let mut unique = BTreeSet::new();
    for goal in &intent.goals {
        let subject = intent
            .subjects
            .get(goal.subject)
            .ok_or_else(|| anyhow::anyhow!("invalid subject index"))?;
        ensure!(
            unique.insert((
                subject.label.to_lowercase(),
                goal.capability.clone(),
                serde_json::to_string(&goal.constraints)?
            )),
            "duplicate retrieval goal"
        );
    }
    let edges: Vec<Vec<usize>> = intent
        .goals
        .iter()
        .map(|goal| {
            parsed
                .capabilities
                .iter()
                .enumerate()
                .filter(|(index, cap)| {
                    let subject = &intent.subjects[goal.subject];
                    let same_subject = cap
                        .subject_id
                        .as_ref()
                        .and_then(|id| parsed.subjects.iter().find(|s| &s.id == id))
                        .is_none_or(|s| s.label.eq_ignore_ascii_case(&subject.label));
                    !answer_instruction(cap)
                        && same_subject
                        && request_match(intake, *index).is_some_and(|found| {
                            found.operations.contains(&goal.capability)
                                && found
                                    .apiosk
                                    .iter()
                                    .any(|candidate| candidate_matches(goal, candidate))
                        })
                })
                .map(|(index, _)| index)
                .collect()
        })
        .collect();
    fn assign(
        goal: usize,
        edges: &[Vec<usize>],
        owners: &mut [Option<usize>],
        seen: &mut BTreeSet<usize>,
    ) -> bool {
        for &capability in &edges[goal] {
            if !seen.insert(capability) {
                continue;
            }
            if owners[capability].is_none_or(|previous| assign(previous, edges, owners, seen)) {
                owners[capability] = Some(goal);
                return true;
            }
        }
        false
    }
    let mut owners = vec![None; parsed.capabilities.len()];
    for goal in 0..intent.goals.len() {
        ensure!(
            assign(goal, &edges, &mut owners, &mut BTreeSet::new()),
            "every parsed subject needs its requested retrieval"
        );
    }
    // Pin the actual matched variant, not only its provider: one provider may
    // publish the same capability for several incompatible jurisdictions.
    for (capability_index, goal_index) in owners
        .iter()
        .enumerate()
        .filter_map(|(cap, goal)| goal.map(|goal| (cap, goal)))
    {
        let goal = &mut intent.goals[goal_index];
        let found = request_match(intake, capability_index).unwrap();
        let candidate = found
            .apiosk
            .iter()
            .find(|candidate| candidate_matches(goal, candidate))
            .ok_or_else(|| anyhow::anyhow!("every parsed subject needs its requested retrieval"))?;
        if let Some(source) = &candidate.source_slug {
            goal.constraints.insert("source".into(), json!(source));
        }
        if let Some(endpoint) = &candidate.endpoint_id {
            goal.constraints
                .insert("endpoint_id".into(), json!(endpoint));
        }
    }
    unique.clear();
    for goal in &intent.goals {
        ensure!(
            unique.insert((
                intent.subjects[goal.subject].label.to_lowercase(),
                goal.capability.clone(),
                serde_json::to_string(&goal.constraints)?
            )),
            "duplicate retrieval goal"
        );
    }
    for &capability_index in &retrievals {
        if owners[capability_index].is_some() {
            continue;
        }
        let found = request_match(intake, capability_index)
            .ok_or_else(|| anyhow::anyhow!("every parsed subject needs its requested retrieval"))?;
        let shared = intent
            .goals
            .iter()
            .enumerate()
            .find(|(goal_index, goal)| {
                edges[*goal_index].contains(&capability_index)
                    && found
                        .apiosk
                        .iter()
                        .any(|candidate| candidate_matches(goal, candidate))
                    && {
                        let services: Vec<_> = catalog
                            .services
                            .iter()
                            .filter(|service| {
                                service.contract.capability_slug == goal.capability
                                    && Some(service.api_slug.as_str())
                                        == goal.constraints.get("source").and_then(Value::as_str)
                                    && endpoint_matches(
                                        goal.constraints.get("endpoint_id"),
                                        Some(&service.endpoint_id.to_string()),
                                    )
                            })
                            .collect();
                        !services.is_empty()
                            && services.iter().all(|service| {
                                std::iter::once(capability_index)
                                    .chain(owners.iter().enumerate().filter_map(|(cap, owner)| {
                                        (*owner == Some(*goal_index)).then_some(cap)
                                    }))
                                    .all(|cap| outputs_cover(&parsed.capabilities[cap], service))
                            })
                    }
            })
            .map(|(index, _)| index);
        ensure!(
            shared.is_some(),
            "every parsed subject needs its requested retrieval"
        );
        owners[capability_index] = shared;
    }
    for (capability_index, goal_index) in owners
        .into_iter()
        .enumerate()
        .filter_map(|(cap, goal)| goal.map(|goal| (cap, goal)))
    {
        let goal = &mut intent.goals[goal_index];
        let cap = &parsed.capabilities[capability_index];
        let Some(subject) = cap
            .subject_id
            .as_ref()
            .and_then(|id| parsed.subjects.iter().find(|s| &s.id == id))
        else {
            continue;
        };
        let mut fields = BTreeSet::from(["company.name".to_owned(), "geo.input.q".to_owned()]);
        for service in catalog.services.iter().filter(|service| {
            service.contract.capability_slug == goal.capability
                && endpoint_matches(
                    goal.constraints.get("endpoint_id"),
                    Some(&service.endpoint_id.to_string()),
                )
                && goal
                    .constraints
                    .get("source")
                    .and_then(Value::as_str)
                    .is_none_or(|source| service.api_slug == source)
        }) {
            fields.extend(
                service
                    .contract
                    .requires
                    .iter()
                    .map(|required| required.fact_type.clone()),
            );
        }
        let values = &mut intent.subjects[goal.subject].values;
        for field in fields {
            let Some(fact) = catalog.vocabulary.get(&field) else {
                continue;
            };
            let Some(mut value) = crate::ask_v2::input_values::documented_literal(
                &field,
                &fact.value_schema,
                subject,
            ) else {
                continue;
            };
            if catalog_core::facts::schema::validate(&fact.value_schema, &value).is_err() {
                let Some(typed) = value
                    .as_str()
                    .and_then(|text| serde_json::from_str::<Value>(text).ok())
                    .filter(|typed| {
                        catalog_core::facts::schema::validate(&fact.value_schema, typed).is_ok()
                    })
                else {
                    continue;
                };
                value = typed;
            }
            if let Some(existing) = values.get(&field) {
                ensure!(
                    existing == &value,
                    "input belongs to a different parsed subject"
                );
            } else {
                values.insert(field, value);
            }
        }
    }
    for cap in parsed
        .capabilities
        .iter()
        .filter(|cap| answer_instruction(cap))
    {
        let instruction = format!("{}: {}", cap.name, cap.description);
        if !intent.answer_operations.contains(&instruction) {
            intent.answer_operations.push(instruction);
        }
    }
    Ok(())
}

pub(super) fn endpoint_matches(constraint: Option<&Value>, endpoint: Option<&str>) -> bool {
    constraint.is_none_or(|constraint| match constraint {
        Value::String(id) => Some(id.as_str()) == endpoint,
        Value::Array(ids) => ids
            .iter()
            .any(|id| id.as_str() == endpoint && endpoint.is_some()),
        _ => false,
    })
}
fn candidate_matches(goal: &super::Goal, candidate: &crate::capability_intake::Candidate) -> bool {
    candidate.availability == "supported"
        && candidate.endpoint_id.is_some()
        && candidate.capability.as_deref() == Some(&goal.capability)
        && goal
            .constraints
            .get("source")
            .and_then(Value::as_str)
            .is_none_or(|source| candidate.source_slug.as_deref() == Some(source))
        && endpoint_matches(
            goal.constraints.get("endpoint_id"),
            candidate.endpoint_id.as_deref(),
        )
}

/// Answer presentation is not another provider purchase. Keep this narrow:
/// subject-specific analysis and comparisons can require external data.
pub(super) fn answer_instruction(cap: &crate::capability_intake::Capability) -> bool {
    crate::capability_intake::answer_instruction(cap)
}

pub(super) fn retrievals_matched(intake: &Intake) -> bool {
    intake
        .parsed_request
        .capabilities
        .iter()
        .enumerate()
        .all(|(index, cap)| {
            answer_instruction(cap)
                || request_match(intake, index)
                    .is_some_and(executable_match)
        })
}

pub(super) fn uncovered_reply(intake: &Intake) -> Option<super::ChatReply> {
    let missing: Vec<_> = intake
        .parsed_request
        .capabilities
        .iter()
        .enumerate()
        .filter(|(index, cap)| {
            !answer_instruction(cap)
                && !cap.slug.starts_with("general.")
                && request_match(intake, *index).is_none_or(|matched| !executable_match(matched))
        })
        .map(|(_, cap)| cap.name.as_str())
        .collect();
    if missing.is_empty() {
        return None;
    }
    let names = missing.join("; ");
    let message = if intake.parsed_request.language == "nl" {
        format!("Voor deze onderdelen is geen passende uitvoerbare bron gevonden: {names}. De volledige aanvraag krijgt daarom nog geen prijs. Verduidelijk deze onderdelen of pas de vraag aan; de overige onderdelen blijven bewaard.")
    } else {
        format!("No matching executable source was found for these requirements: {names}. The complete request cannot be priced yet. Clarify these requirements or revise the question; the other requirements are kept.")
    };
    // This is a catalog fact, not a model draft to rewrite into broader claims
    // about a provider's existence. It never quotes a reduced request.
    Some(super::ChatReply {
        message,
        discover: false,
        intent: None,
        decline_reason: None,
    })
}

fn executable_match(matched: &crate::capability_intake::Match) -> bool {
    matched.apiosk.iter().any(|candidate| candidate.availability == "supported"
        && candidate.endpoint_id.is_some()
        && candidate.capability.as_ref().is_some_and(|capability| matched.operations.contains(capability)))
}

/// A complete model reply was rejected for changing or duplicating the
/// requirements. This is neither evidence of an outage nor of absent sources.
pub(super) fn rejected_reply(intake: &Intake) -> super::ChatReply {
    let names = intake.parsed_request.capabilities.iter()
        .filter(|cap| !answer_instruction(cap)).map(|cap| cap.name.as_str())
        .collect::<Vec<_>>().join("; ");
    let message = if intake.parsed_request.language == "nl" {
        format!("Ik kon nog geen uitvoerbaar plan maken dat deze onderdelen en de opgegeven gegevens correct behoudt: {names}. Er is geen prijsvoorstel gemaakt en niets uitgevoerd. De volledige vraag blijft bewaard; verduidelijk de gewenste gegevens of probeer de planning opnieuw.")
    } else {
        format!("I could not yet build an executable plan that correctly preserves these requirements and the supplied inputs: {names}. No price proposal was made and nothing was executed. The complete question is kept; clarify the required data or try planning again.")
    };
    super::ChatReply {message, discover:false, intent:None, decline_reason:None}
}

fn outputs_cover(
    cap: &crate::capability_intake::Capability,
    service: &crate::catalog::Service,
) -> bool {
    // Exact normalized concept suffixes are contract evidence; descriptions,
    // optional response fields and generic semantic similarity are not.
    let parts = |value: &str| {
        value
            .to_lowercase()
            .split(|ch: char| !ch.is_alphanumeric())
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    !cap.outputs.is_empty()
        && cap.outputs.iter().all(|output| {
            let requested = parts(output);
            !requested.is_empty()
                && service.contract.output_map.produces.keys().any(|fact| {
                    !service.optional_outputs.contains(fact) && parts(fact).ends_with(&requested)
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        capability_intake::{Candidate, Match, ParsedRequest},
        parser::{Goal, Subject},
    };
    use std::collections::BTreeMap;

    fn catalog() -> Catalog {
        Catalog::build(
            serde_json::from_str(include_str!(
                "../../tests/fixtures/integration-catalog.json"
            ))
            .unwrap(),
        )
    }
    fn fixture(label: &str, kind: &str, count: usize) -> (Intake, Intent) {
        let parsed: ParsedRequest = serde_json::from_value(json!({"language":"en",
            "subjects":[{"id":"s1","label":label,"type":kind,"identifiers":{}}],
            "capabilities":(0..count).map(|n|json!({"slug":format!("research.check{n}"),"name":format!("Check {n}"),"description":"Retrieve this requested check","domain":"research","inputs":[],"outputs":[],"subject_id":"s1","source_hint":null,"confidence":1})).collect::<Vec<_>>(),
            "deliverable":{"format":"chat","operations":[]},"meta":{"model":"fixture","parsed_at":"fixture","text_sha256":"fixture"}})).unwrap();
        let candidates = vec![Candidate {
            origin: "apiosk".into(),
            name: "Fixture".into(),
            description: String::new(),
            source_slug: Some("fixture".into()),
            capability: Some("company.accounts".into()),
            endpoint_id: Some(catalog().services.iter().find(|service| service.contract.capability_slug == "company.accounts").unwrap().endpoint_id.to_string()),
            url: None,
            method: "GET".into(),
            price: None,
            network: None,
            calls_30d: None,
            availability: "supported".into(),
        }];
        let intake = Intake {
            matches: parsed
                .capabilities
                .iter()
                .map(|cap| Match {
                    slug: cap.slug.clone(),
                    query: String::new(),
                    operations: BTreeSet::from(["company.accounts".into()]),
                    apiosk: candidates.clone(),
                    coinbase: vec![],
                    coinbase_status: "skipped".into(),
                })
                .collect(),
            parsed_request: parsed,
            apiosk_sources_searched: 1,
            catalog_version: String::new(),
            selected_capabilities: BTreeSet::from(["company.accounts".into()]),
        };
        let intent = Intent {
            subjects: vec![Subject {
                entity_ref: None,
                label: label.into(),
                values: BTreeMap::new(),
            }],
            goals: vec![Goal {
                capability: "company.accounts".into(),
                subject: 0,
                constraints: BTreeMap::new(),
            }],
            answer_operations: vec![],
            unresolved_references: vec![],
        };
        (intake, intent)
    }
    #[test]
    fn unverified_or_unidentified_candidates_cannot_authorize_a_goal() {
        let (mut intake, intent) = fixture("MONZO BANK LIMITED", "company", 1);
        let supported = intake.matches[0].apiosk[0].clone();
        let mut unverified = supported.clone();
        unverified.endpoint_id = Some(uuid::Uuid::new_v4().to_string());
        unverified.availability = "unverified".into();
        intake.matches[0].apiosk = vec![unverified.clone(), supported.clone()];
        let mut valid = intent.clone();
        prepare(&mut valid, &intake, &catalog()).unwrap();
        assert_eq!(valid.goals[0].constraints["endpoint_id"], json!(supported.endpoint_id));
        let mut invalid = intent.clone();
        invalid.goals[0].constraints.insert("endpoint_id".into(), json!(unverified.endpoint_id));
        assert!(prepare(&mut invalid, &intake, &catalog()).is_err());
        intake.matches[0].apiosk = vec![unverified];
        assert!(!retrievals_matched(&intake));
        assert!(uncovered_reply(&intake).is_some(), "stale operation sets must reveal the readiness gap before planning");
        assert!(prepare(&mut intent.clone(), &intake, &catalog()).is_err(), "stale operations must not make unverified candidates executable");
        intake.matches[0].apiosk = vec![supported];
        assert!(retrievals_matched(&intake));
        assert!(uncovered_reply(&intake).is_none());
        intake.matches[0].apiosk[0].endpoint_id = None;
        assert!(!retrievals_matched(&intake));
        assert!(prepare(&mut intent.clone(), &intake, &catalog()).is_err());
    }
    #[test]
    fn one_operation_cannot_satisfy_multiple_checks_or_duplicate_itself() {
        let (intake, mut intent) = fixture("MONZO BANK LIMITED", "company", 2);
        assert!(prepare(&mut intent, &intake, &catalog())
            .unwrap_err()
            .to_string()
            .contains("requested retrieval"));
        intent.goals.push(intent.goals[0].clone());
        assert_eq!(
            prepare(&mut intent, &intake, &catalog())
                .unwrap_err()
                .to_string(),
            "duplicate retrieval goal"
        );
    }
    #[test]
    fn saved_live_due_diligence_plan_cannot_repeat_ip_to_fill_missing_checks() {
        let rows: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/profession-live-planning.json"
        ))
        .unwrap();
        let row = rows["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["case"] == 13)
            .unwrap();
        let intake: Intake = serde_json::from_value(row["observed_intake"].clone()).unwrap();
        let mut intent: Intent = serde_json::from_value(row["observed_intent"].clone()).unwrap();
        assert_eq!(intent.goals.len(), 16);
        assert!(
            prepare(&mut intent, &intake, &catalog()).is_err(),
            "the historically accepted wrong plan must now be refused before a quote"
        );
    }
    #[test]
    fn one_verified_response_can_cover_several_requested_output_facts() {
        let (mut intake, mut intent) = fixture("MONZO BANK LIMITED", "company", 2);
        let mut catalog = catalog();
        let service = catalog
            .services
            .iter_mut()
            .find(|s| s.contract.capability_slug == "company.accounts")
            .unwrap();
        let extraction = service
            .contract
            .output_map
            .produces
            .values()
            .next()
            .unwrap()
            .clone();
        service
            .contract
            .output_map
            .produces
            .insert("company.registered_name".into(), extraction.clone());
        service
            .contract
            .output_map
            .produces
            .insert("company.registered_address".into(), extraction);
        intake.parsed_request.capabilities[0].outputs = vec!["registered_name".into()];
        intake.parsed_request.capabilities[1].outputs = vec!["registered_address".into()];
        prepare(&mut intent, &intake, &catalog).unwrap();
        assert_eq!(
            intent.goals.len(),
            1,
            "one response, one retrieval purchase"
        );
        intake.parsed_request.capabilities[1].outputs = vec!["insolvency_decision".into()];
        assert!(
            prepare(&mut intent, &intake, &catalog).is_err(),
            "matched words alone cannot prove another field is returned"
        );
    }
    #[test]
    fn unmatched_requirement_reply_is_specific_and_keeps_catalog_provider_claims_out() {
        let (mut intake, _) = fixture("MONZO BANK LIMITED", "company", 1);
        intake.parsed_request.capabilities[0].name = "CBS sector comparison".into();
        intake.matches[0].operations.clear();
        let reply = uncovered_reply(&intake).unwrap();
        assert!(!reply.discover && reply.intent.is_none());
        assert!(reply.message.contains("CBS sector comparison"));
        assert!(!reply.message.contains("CBS is unavailable"));
        assert!(
            reply.decline_reason.is_none(),
            "deterministic catalog facts are not sent for a model rewrite"
        );
    }
    #[test]
    fn citation_formatting_is_an_answer_instruction_but_citation_data_stays_a_retrieval() {
        let (mut intake, mut intent) = fixture("MONZO BANK LIMITED", "company", 1);
        let mut cap: crate::capability_intake::Capability = serde_json::from_value(json!({
            "slug":"research.source.citation","name":"Source citation",
            "description":"Provides proper citation for data sources used in research.",
            "domain":"research","inputs":["data_source","reference_type"],"missing":["data_source","reference_type"],
            "outputs":["citation"],"subject_id":null,"source_hint":null,"confidence":0.9})).unwrap();
        assert!(answer_instruction(&cap));
        intake.parsed_request.capabilities.push(cap.clone());
        assert!(retrievals_matched(&intake));
        assert!(uncovered_reply(&intake).is_none());
        prepare(&mut intent,&intake,&catalog()).unwrap();
        assert_eq!(intent.goals.len(),1);
        assert!(intent.answer_operations.iter().any(|text|text.contains("Source citation")));
        cap.outputs.push("citation_count".into());
        assert!(!answer_instruction(&cap));
        cap.outputs=vec!["citation".into()];
        cap.subject_id=Some("paper1".into());
        assert!(!answer_instruction(&cap));
        cap.subject_id=None;
        cap.source_hint=Some("crossref".into());
        assert!(!answer_instruction(&cap));
        cap.source_hint=None;
        cap.inputs.push("doi".into());
        assert!(!answer_instruction(&cap));
    }
    #[test]
    fn answer_instructions_are_preserved_without_buying_an_extra_source() {
        let (mut intake, mut intent) = fixture("MONZO BANK LIMITED", "company", 1);
        let mut cap = intake.parsed_request.capabilities[0].clone();
        cap.slug = "research.source.distinction".into();
        cap.subject_id = None;
        cap.inputs.clear();
        cap.name = "Distinguish official data and AI analysis".into();
        cap.outputs = vec![
            "official_sources".into(),
            "ai_analysis".into(),
            "differences".into(),
        ];
        intake.parsed_request.capabilities.push(cap);
        assert!(retrievals_matched(&intake));
        prepare(&mut intent, &intake, &catalog()).unwrap();
        assert_eq!(intent.goals.len(), 1);
        assert!(intent
            .answer_operations
            .iter()
            .any(|item| item.contains("official data")));
        intake.parsed_request.capabilities[1].outputs = vec!["official_sources".into(),"ai_analysis".into(),"differences_in_reliability".into(),"data_origin".into()];
        assert!(answer_instruction(&intake.parsed_request.capabilities[1]), "saved case5 distinctions are answer instructions");
        // Actual saved supplier-screening parse: provenance concerns the same
        // company, but asks for no additional external facts.
        let distinction = &mut intake.parsed_request.capabilities[1];
        distinction.slug = "research.analysis.comparison".into();
        distinction.subject_id = Some("s1".into());
        distinction.inputs = vec!["subject_id".into(), "analysis_sources".into()];
        distinction.outputs = vec!["official_sources".into(),"ai_generated_analysis".into(),"source_confidence".into(),"discrepancies".into()];
        assert!(answer_instruction(distinction));
        assert!(retrievals_matched(&intake));
        intake.parsed_request.capabilities[1]
            .outputs
            .push("external_market_statistics".into());
        assert!(
            !retrievals_matched(&intake),
            "a data requirement must remain a retrieval gap"
        );
    }
    #[test]
    fn differently_written_constraints_cannot_duplicate_the_same_pinned_purchase() {
        let (intake, mut intent) = fixture("MONZO BANK LIMITED", "company", 2);
        let mut duplicate = intent.goals[0].clone();
        duplicate
            .constraints
            .insert("source".into(), json!("fixture"));
        intent.goals.push(duplicate);
        assert_eq!(
            prepare(&mut intent, &intake, &catalog())
                .unwrap_err()
                .to_string(),
            "duplicate retrieval goal"
        );
    }
    #[test]
    fn same_provider_capability_is_pinned_to_the_matched_endpoint_variant() {
        let (mut intake, mut intent) = fixture("MONZO BANK LIMITED", "company", 1);
        let mut catalog = catalog();
        let correct = catalog
            .services
            .iter()
            .find(|service| service.contract.capability_slug == "company.accounts")
            .unwrap()
            .clone();
        let mut wrong = correct.clone();
        wrong.endpoint_id = uuid::Uuid::new_v4();
        wrong.id = uuid::Uuid::new_v4();
        wrong.list_atomic = 1;
        catalog.services.push(wrong.clone());
        intake.matches[0].apiosk[0].endpoint_id = Some(correct.endpoint_id.to_string());
        prepare(&mut intent, &intake, &catalog).unwrap();
        assert_eq!(
            intent.goals[0].constraints["endpoint_id"],
            correct.endpoint_id.to_string()
        );
        let mut task=crate::state::Task::default();
        crate::engine::bind_subjects(&mut task,&catalog,&mut intent).unwrap();
        let crate::resolver::Resolution::Plan(plan)=crate::resolver::resolve(&catalog,&task,&intent) else {panic!("the pinned variant must remain executable through its name lookup");};
        assert!(plan.steps.iter().any(|step|step.service.endpoint_id==correct.endpoint_id));
        assert!(plan.steps.iter().all(|step|step.service.endpoint_id!=wrong.endpoint_id),"the cheaper same-source variant must not substitute");
        intent.goals[0]
            .constraints
            .insert("endpoint_id".into(), json!(wrong.endpoint_id));
        assert!(
            prepare(&mut intent, &intake, &catalog).is_err(),
            "same source/capability is not permission to select an unmatched endpoint"
        );
    }
    #[test]
    fn source_binding_is_scoped_and_company_literal_is_reused() {
        let (intake, mut intent) = fixture("MONZO BANK LIMITED", "company", 1);
        prepare(&mut intent, &intake, &catalog()).unwrap();
        assert_eq!(intent.goals[0].constraints["source"], "fixture");
        assert_eq!(
            intent.subjects[0].values["company.name"],
            "MONZO BANK LIMITED"
        );
        intent.goals[0]
            .constraints
            .insert("source".into(), json!("unmatched-cheaper-source"));
        assert!(prepare(&mut intent, &intake, &catalog()).is_err());
    }
    #[test]
    fn address_and_documented_entity_name_are_bound_without_asking_again() {
        let (intake, mut intent) = fixture("Dam 1 Amsterdam", "address", 1);
        let mut catalog = catalog();
        catalog.vocabulary.insert(
            "geo.input.q".into(),
            serde_json::from_value(json!({"slug":"geo.input.q","value_schema":{"type":"string"}}))
                .unwrap(),
        );
        prepare(&mut intent, &intake, &catalog).unwrap();
        assert_eq!(intent.subjects[0].values["geo.input.q"], "Dam 1 Amsterdam");
        let field = "source_input.e96e543e56e384fe2818ce6b2ea6064ab.name";
        catalog.vocabulary.insert(field.into(),serde_json::from_value(json!({"slug":field,"value_schema":{"type":"string","description":"Person or entity name to screen"}})).unwrap());
        let service = catalog
            .services
            .iter_mut()
            .find(|s| s.contract.capability_slug == "company.accounts")
            .unwrap();
        service.contract.requires[0].fact_type = field.into();
        let (intake, mut intent) = fixture("MONZO BANK LIMITED", "company", 1);
        prepare(&mut intent, &intake, &catalog).unwrap();
        assert_eq!(intent.subjects[0].values[field], "MONZO BANK LIMITED");
        intent.subjects[0]
            .values
            .insert(field.into(), json!("Another Company"));
        assert_eq!(
            prepare(&mut intent, &intake, &catalog)
                .unwrap_err()
                .to_string(),
            "input belongs to a different parsed subject"
        );
    }
    #[test]
    fn two_same_slug_subjects_need_their_own_matched_source_and_goal() {
        let (mut intake, mut intent) = fixture("First Company", "company", 1);
        let mut second = intake.parsed_request.subjects[0].clone();
        second.id = "s2".into();
        second.label = "Second Company".into();
        intake.parsed_request.subjects.push(second);
        let mut cap = intake.parsed_request.capabilities[0].clone();
        cap.subject_id = Some("s2".into());
        intake.parsed_request.capabilities.push(cap);
        let mut matched = intake.matches[0].clone();
        matched.apiosk[0].source_slug = Some("second-provider".into());
        intake.matches.push(matched);
        intent.subjects.push(Subject {
            entity_ref: None,
            label: "Second Company".into(),
            values: BTreeMap::new(),
        });
        let mut goal = intent.goals[0].clone();
        goal.subject = 1;
        intent.goals.push(goal);
        prepare(&mut intent, &intake, &catalog()).unwrap();
        assert_eq!(intent.goals[0].constraints["source"], "fixture");
        assert_eq!(intent.goals[1].constraints["source"], "second-provider");
        intent.goals[1]
            .constraints
            .insert("source".into(), json!("fixture"));
        assert!(prepare(&mut intent, &intake, &catalog()).is_err());
    }
}
