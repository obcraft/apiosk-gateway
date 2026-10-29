//! What to do when a lookup came back with more than one plausible company.
//!
//! ```text
//!   lookup succeeds ──▶ one subject   ──▶ facts, dependants run
//!                   ──▶ several       ──▶ QUESTION: candidates kept, the
//!                                         lookup stays paid for, dependants
//!                                         wait, one selection is asked for
//!                   ──▶ none          ──▶ an explicitly empty result
//! ```
//!
//! The rule the whole module exists for: **resuming costs nothing.** The
//! lookup was made and charged; its candidates are already in hand. Choosing
//! one of them is reading a value out of a row, not calling anyone. A resume
//! path that re-runs the lookup turns every ambiguous company into two
//! purchases, and ambiguity is the normal case — "Mollie BV" matches 27
//! registrations.
//!
//! And what is NOT here: a confidence number. A model's opinion that candidate
//! two is 0.94 likely is not an authoritative identity check, and using one to
//! skip the question would make the job silently research a company nobody
//! chose.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::facts::contract::{Extraction, SubjectFacts};

/// What a node's response means for the job.
#[derive(Debug, Clone, PartialEq)]
pub enum Reading {
    /// One subject. Its facts are ready and its dependants may run.
    Resolved(Box<SubjectFacts>),
    /// Several plausible subjects. Nothing downstream may run until one is
    /// chosen, and the choice is a person's.
    Ambiguous(Question),
    /// The source answered, and the answer is that there is nothing. A real
    /// result: a company with no filed accounts, a name with no registration.
    /// Explicitly NOT an error, and explicitly not a reason to invent one.
    Empty,
}

/// One selection to put to whoever owns the job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Question {
    /// The node that is asking.
    pub node_key: String,
    /// What it needs, in the vocabulary the answer will be validated against.
    pub fact_type: String,
    pub candidates: Vec<Candidate>,
    /// True when the candidates are a page of a longer list. Shown, because
    /// "these ten of twenty-seven" is a different question from "these ten".
    pub truncated: bool,
}

/// One thing that could be meant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    /// The identifying value. This is what a resume binds.
    pub identity: Value,
    /// Everything else the lookup said about it, for telling two Mollie B.V.s
    /// apart. Whoever chooses needs the address, not just the number.
    pub facts: Vec<CandidateFact>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateFact {
    pub fact_type: String,
    pub value: Value,
}

/// Why an answer to a question cannot be accepted.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ResolveError {
    #[error("this job is not waiting for an answer")]
    NotAsking,
    #[error("that answer is for a different question")]
    WrongQuestion,
    /// The load-bearing one. An answer must be one of the candidates the
    /// already-paid lookup produced. Accepting a KVK number typed in from
    /// somewhere else would let a caller point a paid job at a company the
    /// lookup never found.
    #[error("that is not one of the candidates this lookup returned")]
    NotACandidate,
}

/// Read an extraction into what the job should do next.
pub fn read(node_key: &str, identity_fact_type: &str, extraction: &Extraction) -> Reading {
    if extraction.subjects.is_empty() {
        return Reading::Empty;
    }
    if !extraction.is_ambiguous() {
        // `single()` returns None for anything ambiguous, including the case
        // where one subject was silently picked out of many. That is why it is
        // used rather than `subjects[0]`.
        if let Some(subject) = extraction.single() {
            return Reading::Resolved(Box::new(subject.clone()));
        }
    }
    Reading::Ambiguous(Question {
        node_key: node_key.to_string(),
        fact_type: identity_fact_type.to_string(),
        candidates: extraction
            .subjects
            .iter()
            .filter_map(|subject| {
                Some(Candidate {
                    identity: subject.identity.clone()?,
                    facts: subject
                        .facts
                        .iter()
                        .map(|fact| CandidateFact {
                            fact_type: fact.fact_type.clone(),
                            value: fact.value.clone(),
                        })
                        .collect(),
                })
            })
            .collect(),
        // A subject with no validated identifier cannot be offered as a choice
        // — there would be nothing to bind if it were picked — so the list can
        // be shorter than the number of subjects found.
        truncated: extraction.picked_out_of.is_some()
            || extraction
                .subjects
                .iter()
                .any(|subject| subject.identity.is_none()),
    })
}

/// Accept an answer, or say why not.
///
/// Returns the candidate that was chosen. Nothing is called: the facts came
/// back with the lookup that was already paid for, and this reads one of them.
pub fn resolve<'q>(
    question: Option<&'q Question>,
    node_key: &str,
    chosen: &Value,
) -> Result<&'q Candidate, ResolveError> {
    let question = question.ok_or(ResolveError::NotAsking)?;
    if question.node_key != node_key {
        return Err(ResolveError::WrongQuestion);
    }
    question
        .candidates
        .iter()
        .find(|candidate| &candidate.identity == chosen)
        .ok_or(ResolveError::NotACandidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::contract::{ExecutionContract, FactType, Vocabulary};
    use serde_json::json;

    const ZOEKEN: &str = include_str!("../../fixtures/contracts/kvk-zoeken.json");
    const FACT_TYPES: &str = include_str!("../../fixtures/contracts/fact-types.json");
    const VOLVE: &str =
        include_str!("../../fixtures/contracts/payloads/kvk-zoeken-volve-capital.json");
    const MOLLIE: &str =
        include_str!("../../fixtures/contracts/payloads/kvk-zoeken-mollie-bv.json");

    fn vocabulary() -> Vocabulary {
        serde_json::from_str::<std::collections::BTreeMap<String, FactType>>(FACT_TYPES).unwrap()
    }

    fn extract(payload: &str) -> Extraction {
        let contract: ExecutionContract = serde_json::from_str(ZOEKEN).unwrap();
        let body: serde_json::Value = serde_json::from_str(payload).unwrap();
        contract.extract(&body, &vocabulary()).unwrap()
    }

    fn read_search(payload: &str) -> Reading {
        read("search", "company.registration.nl.kvk", &extract(payload))
    }

    #[test]
    fn one_company_resolves_and_asks_nothing() {
        // Volve Capital: two rows, one kvkNummer. Asking here would put a
        // question with one answer in front of a person.
        let Reading::Resolved(subject) = read_search(VOLVE) else {
            panic!("one company must resolve");
        };
        assert_eq!(subject.identity, Some(json!("86717162")));
    }

    #[test]
    fn ten_companies_ask_once_and_carry_what_is_needed_to_choose() {
        let Reading::Ambiguous(question) = read_search(MOLLIE) else {
            panic!("ten companies must ask");
        };
        assert_eq!(question.node_key, "search");
        assert_eq!(question.fact_type, "company.registration.nl.kvk");
        assert_eq!(question.candidates.len(), 10);
        // Whoever chooses needs more than ten numbers: several are literally
        // called "Mollie B.V.".
        assert!(question.candidates[0]
            .facts
            .iter()
            .any(|f| f.fact_type == "company.name"));
        assert_eq!(question.candidates[0].identity, json!("30204462"));
        assert_eq!(question.candidates[1].identity, json!("92327737"));
    }

    #[test]
    fn nothing_found_is_a_result_and_not_a_question() {
        let contract: ExecutionContract = serde_json::from_str(ZOEKEN).unwrap();
        let empty = contract
            .extract(&json!({ "totaal": 0, "resultaten": [] }), &vocabulary())
            .unwrap();
        assert_eq!(
            read("search", "company.registration.nl.kvk", &empty),
            Reading::Empty
        );
    }

    #[test]
    fn choosing_a_candidate_costs_nothing_and_yields_the_identity() {
        let Reading::Ambiguous(question) = read_search(MOLLIE) else {
            panic!()
        };
        let chosen = resolve(Some(&question), "search", &json!("63892405")).unwrap();
        assert_eq!(chosen.identity, json!("63892405"));
        assert!(chosen
            .facts
            .iter()
            .any(|f| f.value == json!("Mollie Holding B.V.")));
    }

    #[test]
    fn an_answer_that_was_never_offered_is_refused() {
        // Otherwise a caller could point a paid job at a company the lookup
        // never found, by typing a number in.
        let Reading::Ambiguous(question) = read_search(MOLLIE) else {
            panic!()
        };
        assert_eq!(
            resolve(Some(&question), "search", &json!("99999999")),
            Err(ResolveError::NotACandidate)
        );
    }

    #[test]
    fn an_answer_to_a_different_node_is_refused() {
        let Reading::Ambiguous(question) = read_search(MOLLIE) else {
            panic!()
        };
        assert_eq!(
            resolve(Some(&question), "profile", &json!("30204462")),
            Err(ResolveError::WrongQuestion)
        );
    }

    #[test]
    fn answering_a_job_that_is_not_asking_is_refused() {
        assert_eq!(
            resolve(None, "search", &json!("30204462")),
            Err(ResolveError::NotAsking)
        );
    }

    #[test]
    fn a_silently_picked_subject_still_asks() {
        // `$.data[0]` against a body of three has chosen one. It looks
        // resolved and is not.
        let mut contract: ExecutionContract = serde_json::from_str(ZOEKEN).unwrap();
        contract.cardinality = crate::facts::contract::Cardinality::One;
        contract.output_map.base = "$.resultaten[0]".to_string();
        let body: serde_json::Value = serde_json::from_str(MOLLIE).unwrap();
        let extraction = contract.extract(&body, &vocabulary()).unwrap();
        assert!(matches!(
            read("search", "company.registration.nl.kvk", &extraction),
            Reading::Ambiguous(_)
        ));
    }

    #[test]
    fn a_question_says_when_its_list_is_incomplete() {
        // "These ten of twenty-seven" is a different question from "these ten".
        let Reading::Ambiguous(question) = read_search(MOLLIE) else {
            panic!()
        };
        // Every candidate on this page carries an identifier, so nothing was
        // dropped from the list itself.
        assert!(!question.truncated);
        assert_eq!(question.candidates.len(), 10);
    }
}
