//! UK onboarding uses existing quotes, authorization and per-call billing.
//! Every possible purchase is included before approval; unused director slots
//! are skipped only after a complete roster proves they do not exist.
use crate::{
    catalog::{fact_value, Catalog},
    parser::{Goal, Intent},
    resolver::Step,
    state::Task,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const PRESET: &str = "company.uk-supplier-verification";
pub const LIMIT: usize = 5;
pub const COVERAGE: &str = "company.uk_director_screening.coverage";
pub fn screening_cap(index: usize) -> String {
    format!("company.uk-director-screening-{}", index + 1)
}
pub fn available(catalog: &Catalog) -> bool {
    [
        "company.uk-registry-search",
        "company.registry-profile",
        "company.filing-history",
        "company.significant-control",
        "vat.uk.validate",
        "company.uk-director-roster",
        "company.uk-vat-candidate-search",
    ]
    .into_iter()
    .all(|c| catalog.by_capability.contains_key(c))
        && (0..LIMIT).all(|i| catalog.by_capability.contains_key(&screening_cap(i)))
}
pub fn expand(intent: &mut Intent, catalog: &Catalog) {
    if !available(catalog)
        || intent.subjects.len() != 1
        || intent.goals.len() != 1
        || intent.goals[0].capability != PRESET
        || intent.goals[0].subject != 0
        || intent.goals[0]
            .constraints
            .iter()
            .any(|(k, v)| k != "jurisdiction" || v != "GB")
    {
        return;
    }
    let subject = &mut intent.subjects[0];
    if subject.entity_ref.is_none() {
        subject
            .values
            .entry("company.name".into())
            .or_insert_with(|| json!(subject.label));
    }
    intent.goals = [
        ("company.uk-registry-search", "companies-house"),
        ("company.registry-profile", "companies-house"),
        ("company.filing-history", "companies-house"),
        ("company.significant-control", "companies-house"),
        ("vat.uk.validate", "ask-merlin"),
    ]
    .into_iter()
    .map(|(cap, source)| Goal {
        capability: cap.into(),
        subject: 0,
        constraints: BTreeMap::from([("source".into(), json!(source))]),
    })
    .collect();
    intent.goals.extend((0..LIMIT).map(|i| Goal {
        capability: screening_cap(i),
        subject: 0,
        constraints: BTreeMap::from([("source".into(), json!("opensanctions"))]),
    }));
    intent.answer_operations.push("Produce a PDF and short supplier-onboarding summary with four explicit findings: UK legal entity and ownership/PSC; filed accounts and overdue status (not a financial audit); VAT candidate discovery plus the authority-derived validation and registered-name/address match; each current director's OpenSanctions screening. Use all saved source evidence and report unknowns, possible matches and incomplete coverage. VAT search candidates are not company identifiers: an invalid candidate never proves the company has no VAT registration. No watchlist match does not mean a director is clean; this is not criminal-record, disqualification or comprehensive adverse-media clearance. Distinguish sanctions matches, PEP and other risk topics. This bounded run covers a complete board of up to five active directors; if larger/truncated, report screening incomplete. Unused screening slots incur no source charge.".into());
}

pub fn unused_screening_slot(task: &Task, step: &Step) -> bool {
    let Some(index) =
        (0..LIMIT).find(|i| step.service.contract.capability_slug == screening_cap(*i))
    else {
        return false;
    };
    let Ok(Some(coverage)) = fact_value(&task.facts, step.entity_ref, COVERAGE) else {
        return false;
    };
    step.service.api_slug == "opensanctions"
        && coverage["complete"] == true
        && coverage["active_directors"]
            .as_u64()
            .is_some_and(|n| n <= LIMIT as u64 && index as u64 >= n)
}

pub fn limitations() -> Value {
    json!([
    "Ownership and accounts are register evidence, not an audit or proof of ultimate ownership beyond disclosed records.",
    "Search for a published GB VAT candidate and validate only a unique candidate. Missing or ambiguous numbers remain unverified; a supplier invoice may be needed.",
    "OpenSanctions watchlist screening covers a complete roster of up to five active directors. It is not a clean-director certificate or criminal-record check.",
    "One maximum price includes all possible checks. Unused director slots are not called or charged."
])
}

pub fn is_check(intent: &Intent) -> bool {
    intent
        .goals
        .iter()
        .any(|g| g.capability == screening_cap(0))
}
