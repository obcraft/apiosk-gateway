//! What one executable offer requires, produces, and how to read it back.
//!
//! A contract is a row of `capability_execution_contracts` (migration 112) in
//! memory. Deliberately a plain deserialisable struct with no database and no
//! network in it: the same contract is checked in CI against a stored
//! `sample_payload`, planned against without spending anything, and executed
//! against a live body by the worker. All three must reach the same verdict,
//! and the cheapest way to guarantee that is for all three to call this.
//!
//! ---------------------------------------------------------------------------
//! The shape, and why it is this shape
//! ---------------------------------------------------------------------------
//!
//! An output map has a BASE and a set of extracts read relative to it:
//!
//! ```json
//! { "base": "$.resultaten[*]",
//!   "produces": {
//!     "company.registration.nl.kvk": { "path": "$.kvkNummer" },
//!     "company.name":                { "path": "$.naam" } } }
//! ```
//!
//! The base names the SUBJECT. One match is one company, and every fact about
//! that company is read from inside that match. The obvious alternative — one
//! absolute path per fact, zipped by position — was tried first and is wrong:
//! when a search returns two companies and only one of them carries a name,
//! zipping pairs the second company's number with the first company's name and
//! invents a business that does not exist. Reading from inside a base cannot
//! produce that error, because there is nothing to pair.
//!
//! An extract is one of three things, and no more:
//!
//! | form                                        | reads                        |
//! |---------------------------------------------|------------------------------|
//! | `{"path": "$.naam"}`                        | one value                    |
//! | `{"path": "$.x[*].y", "collect": true}`     | all of them, as an array     |
//! | `{"fields": {...}}`                         | an object, field by field    |
//! | `{"adapter": "...", "key": "..."}`          | via a named, reviewed function |
//!
//! `cast` coerces a provider's string to the number its fact type declares —
//! KVK returns `"682987"` where `company.financial_statements` promises a
//! number. A cast that cannot be performed drops the field rather than
//! guessing; a fact that then fails its schema is not a fact.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::adapter;
use super::path::{self, Path};
use super::schema::{self, SchemaError};

/// How many subjects one run of a node describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cardinality {
    /// Exactly one. A basisprofiel lookup for one KVK number.
    One,
    /// Zero or more. A search.
    Many,
}

/// Coercion from what a provider sends to what a fact type declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cast {
    /// `"682987"` -> `682987`. A value that is not a number is dropped, not
    /// guessed at.
    Number,
    /// `682987` -> `"682987"`.
    String,
}

impl Cast {
    fn apply(self, value: Value) -> Option<Value> {
        match self {
            Cast::Number => match value {
                Value::Number(_) => Some(value),
                Value::String(text) => text
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .and_then(serde_json::Number::from_f64)
                    .map(Value::Number),
                _ => None,
            },
            Cast::String => match value {
                Value::String(_) => Some(value),
                Value::Number(number) => Some(Value::String(number.to_string())),
                Value::Bool(flag) => Some(Value::String(flag.to_string())),
                _ => None,
            },
        }
    }
}

/// How one fact is read out of a subject.
///
/// `untagged`, ordered so the discriminating key decides: `adapter`, then
/// `fields`, then `path`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Extract {
    /// A named function from [`super::adapter`], for shapes a path cannot
    /// address.
    Adapter {
        adapter: String,
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cast: Option<Cast>,
    },
    /// Compose an object, field by field. Absent fields are simply absent; the
    /// fact type's `required` decides whether that is fatal.
    Fields { fields: BTreeMap<String, Extract> },
    /// Read a path relative to the subject base.
    Path {
        path: String,
        /// Take every match as an array instead of requiring exactly one.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        collect: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cast: Option<Cast>,
    },
}

/// Where the subjects are, and what to read from each one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputMap {
    /// Path to one subject. `$` — the whole body is one subject — is the
    /// default and is what every single-result endpoint uses.
    #[serde(default = "root")]
    pub base: String,
    pub produces: BTreeMap<String, Extract>,
}

fn root() -> String {
    "$".to_string()
}

/// Where one required fact's value goes on the wire.
///
/// Same dialect as `capability_offers.input_map` (migration 075), keyed by
/// fact type instead of by capability input name. `in` is optional there and
/// optional here: the curated KVK rows omit it, and inventing a value we have
/// not verified would be a guess about how a paid call is addressed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputBinding {
    #[serde(rename = "in", default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrap: Option<String>,
    /// Reviewed fallback for an optional input, never a subject identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
}

/// A fact this contract needs before its node can be scheduled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Requirement {
    pub fact_type: String,
    /// WHICH subject the fact must be about. A contract taking two companies
    /// says `acquirer` and `target` rather than hoping position disambiguates.
    #[serde(default = "default_role")]
    pub subject_role: String,
    #[serde(default)]
    pub optional: bool,
}

fn default_role() -> String {
    "subject".to_string()
}

/// One row of `capability_execution_contracts`, as the planner and the worker
/// see it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionContract {
    pub capability_slug: String,
    pub version: i32,
    /// How many subjects ONE run describes.
    pub cardinality: Cardinality,
    #[serde(default)]
    pub requires: Vec<Requirement>,
    /// fact type -> where its value goes on the wire. Read by the planner when
    /// it binds a node; extraction never touches it.
    #[serde(default)]
    pub input_map: BTreeMap<String, InputBinding>,
    pub output_map: OutputMap,
    /// Which produced fact type IDENTIFIES the subject.
    ///
    /// Load-bearing, and not cosmetic. KVK's search returns one company twice
    /// — once as `hoofdvestiging`, once as `rechtspersoon` — with the same
    /// `kvkNummer`. Two rows, one business. Without this the run reports an
    /// ambiguity and stops to ask a question that has one answer; with it, the
    /// rows collapse on the identifier and the job continues.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifies: Option<String>,
    #[serde(default = "one")]
    pub max_fan_out: u32,
}

fn one() -> u32 {
    1
}

/// Why a contract cannot be used, or a body cannot be read with it.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ContractError {
    #[error("invalid contract inputs: {detail}")]
    InvalidInputs { detail: String },
    #[error("path `{path}` in the contract is invalid: {source}")]
    BadPath {
        path: String,
        #[source]
        source: path::PathError,
    },
    #[error("`{fact_type}` has no fact type definition")]
    UnknownFactType { fact_type: String },
    #[error("the schema for `{fact_type}` cannot be enforced: {source}")]
    UnenforceableSchema {
        fact_type: String,
        #[source]
        source: SchemaError,
    },
    #[error("`{adapter}` is not a known adapter")]
    UnknownAdapter { adapter: String },
    /// A `one` contract whose base selects many subjects. It would have to
    /// choose, and choosing is the one thing it may not do.
    #[error("contract declares cardinality `one` but its base `{base}` selects many subjects")]
    PluralBaseInSingularContract { base: String },
    /// A path inside a subject that selects several values without saying so.
    #[error(
        "`{fact_type}` reads {found} values from one subject; add `collect` if that is intended"
    )]
    UnexpectedlyPlural { fact_type: String, found: usize },
    #[error("`identifies` names `{fact_type}`, which the contract does not produce")]
    IdentityNotProduced { fact_type: String },
    #[error("required fact `{fact_type}` has no input_map entry, so the call cannot be bound")]
    UnboundRequirement { fact_type: String },
}

/// What reading a response body produced.
#[derive(Debug, Clone, PartialEq)]
pub struct Extraction {
    /// One entry per DISTINCT subject the response described, in document
    /// order. Distinct means "after collapsing on `identifies`".
    pub subjects: Vec<SubjectFacts>,
    /// Values the body did contain but which failed their type's schema. They
    /// are NOT facts and never reach `subjects`; they are carried so the
    /// failure is visible in the job record instead of looking like an empty
    /// response.
    pub rejected: Vec<Rejection>,
    /// `Some(n)` when the base reached its subject by taking one element out
    /// of an array of `n > 1`. The contract asked for one subject and the path
    /// quietly supplied one, which is not the same as the body describing one.
    pub picked_out_of: Option<usize>,
}

impl Extraction {
    /// True when the caller may not proceed on its own: several plausible
    /// subjects, or one that was silently chosen out of several. Both mean the
    /// same thing — pause, keep the lookup that was already paid for, ask.
    pub fn is_ambiguous(&self) -> bool {
        self.subjects.len() > 1 || self.picked_out_of.is_some()
    }

    /// The one subject, when there is exactly one and nothing was chosen for
    /// us. `None` is not a failure to be worked around: it means ask.
    pub fn single(&self) -> Option<&SubjectFacts> {
        if self.is_ambiguous() {
            return None;
        }
        self.subjects.first()
    }
}

/// Everything one run said about one subject.
#[derive(Debug, Clone, PartialEq)]
pub struct SubjectFacts {
    /// Position of the first row this subject was read from. Not an identity.
    pub index: usize,
    /// The value of the identifying fact, when the contract declares one.
    pub identity: Option<Value>,
    pub facts: Vec<Fact>,
}

impl SubjectFacts {
    pub fn fact(&self, fact_type: &str) -> Option<&Fact> {
        self.facts.iter().find(|f| f.fact_type == fact_type)
    }
}

/// A validated value, and where it came from.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Fact {
    pub fact_type: String,
    pub subject_role: String,
    pub value: Value,
    /// The base the subject was read from, kept verbatim so a receipt can
    /// answer "where did this come from" without re-reading the contract.
    pub extraction_base: String,
}

/// A value that was found but is not a fact, and why.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Rejection {
    pub fact_type: String,
    pub reason: String,
}

/// A fact type definition, as far as extraction cares about it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactType {
    pub slug: String,
    pub value_schema: Value,
}

/// A vocabulary keyed by fact type slug.
pub type Vocabulary = BTreeMap<String, FactType>;

impl ExecutionContract {
    /// Check the contract against itself and against the vocabulary, without a
    /// response body. This is what a contract must pass before it leaves
    /// `draft`: every path parseable, every adapter known, every schema
    /// enforceable, and no singular contract whose base selects many.
    pub fn check(&self, types: &Vocabulary) -> Result<(), ContractError> {
        let base = parse_path(&self.output_map.base)?;
        if self.cardinality == Cardinality::One && base.is_plural() {
            return Err(ContractError::PluralBaseInSingularContract {
                base: self.output_map.base.clone(),
            });
        }

        for (fact_type, extract) in &self.output_map.produces {
            let definition =
                types
                    .get(fact_type)
                    .ok_or_else(|| ContractError::UnknownFactType {
                        fact_type: fact_type.clone(),
                    })?;
            schema::check_schema(&definition.value_schema).map_err(|source| {
                ContractError::UnenforceableSchema {
                    fact_type: fact_type.clone(),
                    source,
                }
            })?;
            check_extract(extract)?;
        }

        if self.capability_slug.trim().is_empty() || self.version <= 0 || self.max_fan_out != 1 {
            return Err(ContractError::InvalidInputs {
                detail: "invalid version, capability or unsupported fan-out".into(),
            });
        }
        let mut kinds = std::collections::BTreeSet::new();
        let mut targets = std::collections::BTreeSet::new();
        // A route template can have several NAMED path slots (`{lat}`, `{lon}`,
        // `{radius}`), and each one remains unambiguous because the endpoint
        // schema names its destination. `path_append` has no slot name in the
        // route itself, so more than one append would make ordering implicit;
        // keep that legacy form limited to one.
        let mut path_appends = 0;
        for required in &self.requires {
            if required.subject_role != "subject" || !kinds.insert(&required.fact_type) {
                return Err(ContractError::InvalidInputs {
                    detail: "duplicate fact or unsupported subject role".into(),
                });
            }
            let definition =
                types
                    .get(&required.fact_type)
                    .ok_or_else(|| ContractError::UnknownFactType {
                        fact_type: required.fact_type.clone(),
                    })?;
            schema::check_schema(&definition.value_schema).map_err(|source| {
                ContractError::UnenforceableSchema {
                    fact_type: required.fact_type.clone(),
                    source,
                }
            })?;
        }
        for (kind, binding) in &self.input_map {
            let location = binding.location.as_deref().unwrap_or("query");
            let valid_location = matches!(location, "query" | "body" | "path" | "path_append");
            if location == "path_append" {
                path_appends += 1;
            }
            if let Some(value) = &binding.default {
                if !self
                    .requires
                    .iter()
                    .any(|r| r.fact_type == *kind && r.optional)
                    || types
                        .get(kind)
                        .is_none_or(|ft| schema::validate(&ft.value_schema, value).is_err())
                {
                    return Err(ContractError::InvalidInputs {
                        detail: "invalid optional input default".into(),
                    });
                }
            }
            if !kinds.contains(kind)
                || !valid_location
                || binding.name.trim().is_empty()
                || !targets.insert((location, &binding.name))
                || path_appends > 1
                || binding
                    .wrap
                    .as_deref()
                    .is_some_and(|w| !matches!(w, "array" | "uk_vat_search") || location != "body")
            {
                return Err(ContractError::InvalidInputs {
                    detail: "undeclared, duplicate or unsupported field mapping".into(),
                });
            }
        }

        // A requirement with nowhere to go cannot be bound, and a node that
        // cannot be bound must never reach a plan a buyer approves.
        for requirement in &self.requires {
            if !requirement.optional && !self.input_map.contains_key(&requirement.fact_type) {
                return Err(ContractError::UnboundRequirement {
                    fact_type: requirement.fact_type.clone(),
                });
            }
        }

        if let Some(identity) = &self.identifies {
            if !self.output_map.produces.contains_key(identity) {
                return Err(ContractError::IdentityNotProduced {
                    fact_type: identity.clone(),
                });
            }
        }
        Ok(())
    }

    /// Read a response body with this contract.
    ///
    /// Returns one [`SubjectFacts`] per distinct subject. A `one` contract that
    /// finds several returns several — deciding what to do about that is the
    /// caller's, and its only legal options are to ask or to fail. It must not
    /// take `subjects[0]`.
    pub fn extract(&self, body: &Value, types: &Vocabulary) -> Result<Extraction, ContractError> {
        let base = parse_path(&self.output_map.base)?;
        let found = base.select_detailed(body);
        let picked_out_of = found
            .picked_from
            .filter(|_| self.cardinality == Cardinality::One);

        let mut rejected: Vec<Rejection> = Vec::new();
        let mut subjects: Vec<SubjectFacts> = Vec::new();

        for (index, subject_root) in found.values.iter().enumerate() {
            let mut facts: Vec<Fact> = Vec::new();
            for (fact_type, extract) in &self.output_map.produces {
                let Some(value) = read(extract, subject_root, fact_type)? else {
                    continue;
                };
                let definition =
                    types
                        .get(fact_type)
                        .ok_or_else(|| ContractError::UnknownFactType {
                            fact_type: fact_type.clone(),
                        })?;
                match schema::validate(&definition.value_schema, &value) {
                    Ok(()) => facts.push(Fact {
                        fact_type: fact_type.clone(),
                        subject_role: "subject".to_string(),
                        value,
                        extraction_base: self.output_map.base.clone(),
                    }),
                    Err(error) => rejected.push(Rejection {
                        fact_type: fact_type.clone(),
                        reason: error.to_string(),
                    }),
                }
            }
            if facts.is_empty() {
                continue;
            }
            let identity = self
                .identifies
                .as_deref()
                .and_then(|slug| facts.iter().find(|f| f.fact_type == slug))
                .map(|f| f.value.clone());
            subjects.push(SubjectFacts {
                index,
                identity,
                facts,
            });
        }

        Ok(Extraction {
            subjects: collapse_on_identity(subjects),
            rejected,
            picked_out_of,
        })
    }
}

/// Merge rows that carry the same identifying value.
///
/// One KVK search for "Volve Capital" returns `totaal: 2` and both rows carry
/// `kvkNummer 86717162` — the company as a `hoofdvestiging` and the same
/// company as a `rechtspersoon`. Treating that as two candidates would stop a
/// job to ask a question whose two answers are the same company. Rows with no
/// identity are never merged: without an identifier there is no evidence they
/// are the same thing, and merging on a shared NAME is exactly the mistake
/// this whole module exists to prevent.
fn collapse_on_identity(subjects: Vec<SubjectFacts>) -> Vec<SubjectFacts> {
    let mut out: Vec<SubjectFacts> = Vec::new();
    for subject in subjects {
        let Some(identity) = subject.identity.clone() else {
            out.push(subject);
            continue;
        };
        match out
            .iter_mut()
            .find(|seen| seen.identity.as_ref() == Some(&identity))
        {
            // The later row may carry a fact the earlier one lacked. Keep the
            // first value for any fact both rows have: it is the one the
            // provider ranked first, and swapping it silently would make the
            // result depend on row order.
            Some(seen) => {
                for fact in subject.facts {
                    if seen.fact(&fact.fact_type).is_none() {
                        seen.facts.push(fact);
                    }
                }
            }
            None => out.push(subject),
        }
    }
    out
}

fn parse_path(text: &str) -> Result<Path, ContractError> {
    path::parse(text).map_err(|source| ContractError::BadPath {
        path: text.to_string(),
        source,
    })
}

fn check_extract(extract: &Extract) -> Result<(), ContractError> {
    match extract {
        Extract::Path { path, .. } => {
            parse_path(path)?;
            Ok(())
        }
        Extract::Adapter { adapter, .. } => {
            if adapter::KNOWN.contains(&adapter.as_str()) {
                Ok(())
            } else {
                Err(ContractError::UnknownAdapter {
                    adapter: adapter.clone(),
                })
            }
        }
        Extract::Fields { fields } => fields.values().try_for_each(check_extract),
    }
}

/// Read one extract from one subject. `None` means the subject does not carry
/// it, which is data rather than an error.
fn read(
    extract: &Extract,
    subject: &Value,
    fact_type: &str,
) -> Result<Option<Value>, ContractError> {
    match extract {
        Extract::Path {
            path,
            collect,
            cast,
        } => {
            let parsed = parse_path(path)?;
            let values = parsed.select(subject);
            if *collect {
                let items: Vec<Value> = values
                    .into_iter()
                    .cloned()
                    .filter_map(|value| match cast {
                        Some(cast) => cast.apply(value),
                        None => Some(value),
                    })
                    .collect();
                return Ok(if items.is_empty() {
                    None
                } else {
                    Some(Value::Array(items))
                });
            }
            match values.len() {
                0 => Ok(None),
                1 => Ok(apply_cast(values[0].clone(), cast)),
                found => Err(ContractError::UnexpectedlyPlural {
                    fact_type: fact_type.to_string(),
                    found,
                }),
            }
        }
        Extract::Adapter { adapter, key, cast } => {
            Ok(adapter::run(adapter, key, subject).and_then(|value| apply_cast(value, cast)))
        }
        Extract::Fields { fields } => {
            let mut object = serde_json::Map::new();
            for (name, sub) in fields {
                if let Some(value) = read(sub, subject, fact_type)? {
                    object.insert(name.clone(), value);
                }
            }
            Ok(if object.is_empty() {
                None
            } else {
                Some(Value::Object(object))
            })
        }
    }
}

fn apply_cast(value: Value, cast: &Option<Cast>) -> Option<Value> {
    match cast {
        Some(cast) => cast.apply(value),
        None => Some(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // The contracts and the bodies are the ones in gateway/fixtures/contracts:
    // the contracts migration 112 seeds, and real production responses copied
    // out of `call_runs`. Nothing here is a hand-written example of what a
    // provider might send, which is the only reason these tests are evidence.
    const FACT_TYPES: &str = include_str!("../../fixtures/contracts/fact-types.json");
    const ZOEKEN: &str = include_str!("../../fixtures/contracts/kvk-zoeken.json");
    const BASISPROFIEL: &str = include_str!("../../fixtures/contracts/kvk-basisprofiel.json");
    const JAARREKENINGEN: &str = include_str!("../../fixtures/contracts/kvk-jaarrekeningen.json");
    const VOLVE: &str =
        include_str!("../../fixtures/contracts/payloads/kvk-zoeken-volve-capital.json");
    const MOLLIE_SEARCH: &str =
        include_str!("../../fixtures/contracts/payloads/kvk-zoeken-mollie-bv.json");
    const MOLLIE_PROFILE: &str =
        include_str!("../../fixtures/contracts/payloads/kvk-basisprofiel-mollie.json");
    const FILING: &str =
        include_str!("../../fixtures/contracts/payloads/kvk-jaarrekening-86717162.json");
    const PDOK: &str = include_str!("../../fixtures/contracts/pdok-locatieserver-free.json");
    const BELLAMYPLEIN: &str =
        include_str!("../../fixtures/contracts/payloads/pdok-locatieserver-bellamyplein.json");

    fn vocabulary() -> Vocabulary {
        serde_json::from_str(FACT_TYPES).expect("fact-types.json must parse")
    }

    fn contract(text: &str) -> ExecutionContract {
        serde_json::from_str(text).expect("a contract fixture must parse")
    }

    fn body(text: &str) -> Value {
        serde_json::from_str(text).expect("a payload fixture must parse")
    }

    // --- the contracts check out before any body is read --------------------

    #[test]
    fn every_seeded_contract_passes_its_own_check() {
        let types = vocabulary();
        for text in [ZOEKEN, BASISPROFIEL, JAARREKENINGEN, PDOK] {
            contract(text)
                .check(&types)
                .expect("a seeded contract must be enforceable");
        }
    }

    // --- the search: identity, and the difference between two rows and two
    //     companies --------------------------------------------------------

    #[test]
    fn two_rows_for_one_company_are_one_company() {
        // Volve Capital comes back twice for one search: once as
        // `hoofdvestiging`, once as `rechtspersoon`, both carrying kvkNummer
        // 86717162. Reporting that as an ambiguity would stop a paid job to
        // ask a question with one answer.
        let out = contract(ZOEKEN)
            .extract(&body(VOLVE), &vocabulary())
            .unwrap();
        assert_eq!(body(VOLVE)["totaal"], json!(2));
        assert_eq!(out.subjects.len(), 1);
        assert!(!out.is_ambiguous());
        assert_eq!(out.single().unwrap().identity, Some(json!("86717162")));
    }

    #[test]
    fn ten_companies_for_one_name_stay_ten() {
        // "Mollie BV" matches 27 registrations; the page holds 10, all with
        // different KVK numbers. This is a real ambiguity and the job pauses.
        let out = contract(ZOEKEN)
            .extract(&body(MOLLIE_SEARCH), &vocabulary())
            .unwrap();
        assert_eq!(out.subjects.len(), 10);
        assert!(out.is_ambiguous());
        assert!(out.single().is_none());
        // Each candidate keeps its own name/number pairing.
        assert_eq!(out.subjects[0].identity, Some(json!("30204462")));
        assert_eq!(
            out.subjects[0].fact("company.name").unwrap().value,
            json!("Mollie B.V.")
        );
        assert_eq!(out.subjects[3].identity, Some(json!("63892405")));
        assert_eq!(
            out.subjects[3].fact("company.name").unwrap().value,
            json!("Mollie Holding B.V.")
        );
    }

    #[test]
    fn a_search_with_no_hits_yields_no_company_at_all() {
        let out = contract(ZOEKEN)
            .extract(&json!({ "totaal": 0, "resultaten": [] }), &vocabulary())
            .unwrap();
        assert!(out.subjects.is_empty());
        assert!(!out.is_ambiguous());
        assert!(out.rejected.is_empty());
    }

    // --- the profile: composing an object out of a real body ----------------

    #[test]
    fn the_profile_is_composed_from_the_real_body() {
        let out = contract(BASISPROFIEL)
            .extract(&body(MOLLIE_PROFILE), &vocabulary())
            .unwrap();
        let subject = out.single().expect("one company, unambiguously");
        assert_eq!(
            subject.fact("company.registration.nl.kvk").unwrap().value,
            json!("30204462")
        );
        let profile = &subject.fact("company.profile").unwrap().value;
        assert_eq!(profile["legal_name"], json!("Mollie B.V."));
        assert_eq!(profile["legal_form"], json!("Besloten Vennootschap"));
        assert_eq!(profile["founded_on"], json!("20050421"));
        assert_eq!(profile["employee_count"], json!(110));
        assert_eq!(profile["sbi_codes"], json!(["62900", "62100"]));
        assert_eq!(profile["trade_names"], json!(["Mollie B.V."]));
    }

    #[test]
    fn the_registered_address_comes_out_as_its_own_fact() {
        let out = contract(BASISPROFIEL)
            .extract(&body(MOLLIE_PROFILE), &vocabulary())
            .unwrap();
        let address = &out.single().unwrap().fact("address.postal").unwrap().value;
        assert_eq!(address["postcode"], json!("1015CW"));
        assert_eq!(address["city"], json!("Amsterdam"));
        assert_eq!(address["street"], json!("Keizersgracht"));
        // KVK sends the house number as a JSON number; the fact type says
        // string, and the cast is declared rather than assumed.
        assert_eq!(address["house_number"], json!("126"));
        // `land` is "Nederland", which is not the ISO code `country` promises.
        // It is deliberately not mapped: a wrong value would sink the whole
        // address fact, and the jurisdiction is already on the fact type.
        assert!(address.get("country").is_none());
    }

    // --- the filing: an adapter, and the figures that are genuinely absent --

    #[test]
    fn the_filing_yields_the_figures_it_actually_contains() {
        let out = contract(JAARREKENINGEN)
            .extract(&body(FILING), &vocabulary())
            .unwrap();
        let figures = &out
            .single()
            .unwrap()
            .fact("company.financial_statements")
            .unwrap()
            .value;
        assert_eq!(figures["financial_year"], json!(2025.0));
        assert_eq!(figures["filed_on"], json!("2026-04-26"));
        assert_eq!(figures["total_assets"], json!(682987.0));
        assert_eq!(figures["equity"], json!(-373267.0));
    }

    #[test]
    fn a_balance_sheet_only_filing_reports_no_revenue_rather_than_zero() {
        // This real filing has no income statement. "Revenue: 0" would be a
        // fabricated figure about a real company, and a downstream reader
        // cannot tell a fabricated zero from a filed one.
        let out = contract(JAARREKENINGEN)
            .extract(&body(FILING), &vocabulary())
            .unwrap();
        let figures = &out
            .single()
            .unwrap()
            .fact("company.financial_statements")
            .unwrap()
            .value;
        assert!(figures.get("revenue").is_none());
        assert!(figures.get("result_after_tax").is_none());
    }

    #[test]
    fn a_company_with_no_filing_yields_no_fact_and_no_invented_year() {
        // Two of three real production attempts returned 404. An empty body is
        // the shape a "no filing" answer takes, and it must produce nothing.
        let out = contract(JAARREKENINGEN)
            .extract(&json!({}), &vocabulary())
            .unwrap();
        assert!(out.subjects.is_empty());
        assert!(out.rejected.is_empty());
    }

    // --- what a bad body does ----------------------------------------------

    #[test]
    fn a_malformed_number_is_rejected_and_never_becomes_a_fact() {
        let mut broken = body(VOLVE);
        broken["resultaten"][0]["kvkNummer"] = json!("867171");
        broken["resultaten"][1]["kvkNummer"] = json!("867171");
        let out = contract(ZOEKEN).extract(&broken, &vocabulary()).unwrap();
        assert!(out
            .subjects
            .iter()
            .all(|s| s.fact("company.registration.nl.kvk").is_none()));
        assert_eq!(out.rejected.len(), 2);
        assert!(out.rejected[0].reason.contains("pattern"));
        // With no identity left they cannot be collapsed: without an
        // identifier there is no evidence the two rows are the same company.
        assert_eq!(out.subjects.len(), 2);
    }

    #[test]
    fn a_profile_missing_its_legal_name_is_rejected_whole() {
        let mut broken = body(MOLLIE_PROFILE);
        broken.as_object_mut().unwrap().remove("statutaireNaam");
        let out = contract(BASISPROFIEL)
            .extract(&broken, &vocabulary())
            .unwrap();
        let subject = out.single().unwrap();
        assert!(subject.fact("company.profile").is_none());
        assert!(out
            .rejected
            .iter()
            .any(|r| r.fact_type == "company.profile"));
        // The facts that ARE sound still come through. A partial answer is not
        // an empty one.
        assert!(subject.fact("company.registration.nl.kvk").is_some());
        assert!(subject.fact("address.postal").is_some());
    }

    #[test]
    fn a_null_identifier_is_not_an_identity() {
        let mut broken = body(VOLVE);
        broken["resultaten"][0]["kvkNummer"] = Value::Null;
        broken["resultaten"][1]["kvkNummer"] = Value::Null;
        let out = contract(ZOEKEN).extract(&broken, &vocabulary()).unwrap();
        assert_eq!(out.subjects.len(), 2);
        assert!(out.subjects.iter().all(|s| s.identity.is_none()));
        assert!(out.is_ambiguous());
    }

    // --- the contract checks itself ----------------------------------------

    #[test]
    fn a_singular_contract_may_not_have_a_plural_base() {
        let mut broken = contract(BASISPROFIEL);
        broken.output_map.base = "$.resultaten[*]".to_string();
        assert!(matches!(
            broken.check(&vocabulary()),
            Err(ContractError::PluralBaseInSingularContract { .. })
        ));
    }

    #[test]
    fn a_singular_contract_that_had_to_choose_reports_an_ambiguity() {
        // `$.data[0]` looks singular. Against a body holding three records it
        // has chosen one, and the result must not read as a confident answer.
        let mut contract = contract(BASISPROFIEL);
        contract.output_map.base = "$.data[0]".to_string();
        let body = json!({ "data": [
            { "kvkNummer": "30204462", "statutaireNaam": "A" },
            { "kvkNummer": "92327737", "statutaireNaam": "B" },
            { "kvkNummer": "63892405", "statutaireNaam": "C" }
        ]});
        let out = contract.extract(&body, &vocabulary()).unwrap();
        assert_eq!(out.picked_out_of, Some(3));
        assert!(out.is_ambiguous());
        assert!(out.single().is_none());
    }

    #[test]
    fn a_single_element_envelope_is_not_a_choice() {
        let mut contract = contract(BASISPROFIEL);
        contract.output_map.base = "$.data[0]".to_string();
        let body = json!({ "data": [{ "kvkNummer": "30204462", "statutaireNaam": "A" }] });
        let out = contract.extract(&body, &vocabulary()).unwrap();
        assert_eq!(out.picked_out_of, None);
        assert!(!out.is_ambiguous());
    }

    #[test]
    fn an_unknown_adapter_is_refused() {
        let mut broken = contract(JAARREKENINGEN);
        broken.output_map.produces.insert(
            "company.name".to_string(),
            Extract::Adapter {
                adapter: "kvk.made-up".to_string(),
                key: "Naam".to_string(),
                cast: None,
            },
        );
        assert!(matches!(
            broken.check(&vocabulary()),
            Err(ContractError::UnknownAdapter { .. })
        ));
    }

    #[test]
    fn an_unparseable_path_never_reaches_a_job() {
        let mut broken = contract(ZOEKEN);
        broken.output_map.produces.insert(
            "company.name".to_string(),
            Extract::Path {
                path: "$.resultaten[?(@.type=='hoofdvestiging')].naam".to_string(),
                collect: false,
                cast: None,
            },
        );
        assert!(matches!(
            broken.check(&vocabulary()),
            Err(ContractError::BadPath { .. })
        ));
    }

    #[test]
    fn a_fact_type_nobody_defined_is_refused() {
        let mut broken = contract(ZOEKEN);
        broken.output_map.produces.insert(
            "company.vibes".to_string(),
            Extract::Path {
                path: "$.vibes".to_string(),
                collect: false,
                cast: None,
            },
        );
        assert!(matches!(
            broken.check(&vocabulary()),
            Err(ContractError::UnknownFactType { .. })
        ));
    }

    #[test]
    fn an_identity_the_contract_does_not_produce_is_refused() {
        let mut broken = contract(ZOEKEN);
        broken.identifies = Some("company.profile".to_string());
        assert!(matches!(
            broken.check(&vocabulary()),
            Err(ContractError::IdentityNotProduced { .. })
        ));
    }

    #[test]
    fn a_path_that_reads_several_values_without_saying_so_is_an_error() {
        let mut broken = contract(BASISPROFIEL);
        broken.output_map.produces.insert(
            "company.name".to_string(),
            Extract::Path {
                path: "$.sbiActiviteiten[*].sbiCode".to_string(),
                collect: false,
                cast: None,
            },
        );
        assert!(matches!(
            broken.extract(&body(MOLLIE_PROFILE), &vocabulary()),
            Err(ContractError::UnexpectedlyPlural { .. })
        ));
    }

    #[test]
    fn a_required_fact_with_no_input_binding_is_refused() {
        // A node the planner cannot bind must not reach a plan a buyer
        // approves: the failure would land at execution time, after the
        // reservation, on a job that never had a chance.
        let mut broken = contract(ZOEKEN);
        broken.input_map.clear();
        assert!(matches!(
            broken.check(&vocabulary()),
            Err(ContractError::UnboundRequirement { ref fact_type }) if fact_type == "company.name"
        ));
    }

    #[test]
    fn several_named_path_slots_are_valid_but_several_appends_are_not() {
        let mut named = contract(ZOEKEN);
        named.requires.push(Requirement {
            fact_type: "company.profile".to_string(),
            subject_role: "subject".to_string(),
            optional: false,
        });
        named.input_map.insert(
            "company.name".to_string(),
            InputBinding {
                location: Some("path".to_string()),
                name: "lat".to_string(),
                wrap: None,
                default: None,
            },
        );
        named.input_map.insert(
            "company.profile".to_string(),
            InputBinding {
                location: Some("path".to_string()),
                name: "lon".to_string(),
                wrap: None,
                default: None,
            },
        );
        assert!(named.check(&vocabulary()).is_ok());

        for binding in named.input_map.values_mut() {
            binding.location = Some("path_append".to_string());
        }
        assert!(matches!(
            named.check(&vocabulary()),
            Err(ContractError::InvalidInputs { .. })
        ));
    }

    #[test]
    fn every_seeded_contract_can_bind_what_it_requires() {
        for text in [ZOEKEN, BASISPROFIEL, JAARREKENINGEN, PDOK] {
            let parsed = contract(text);
            for requirement in &parsed.requires {
                assert!(
                    parsed.input_map.contains_key(&requirement.fact_type),
                    "{} requires {} with no input binding",
                    parsed.capability_slug,
                    requirement.fact_type
                );
            }
        }
    }

    // --- PDOK: an address search over a real body ---------------------------

    #[test]
    fn the_address_search_reads_every_hit_it_was_given() {
        let out = contract(PDOK)
            .extract(&body(BELLAMYPLEIN), &vocabulary())
            .unwrap();
        assert_eq!(out.subjects.len(), 10);
        let first = &out.subjects[0].fact("address.postal").unwrap().value;
        assert_eq!(first["postcode"], json!("1053AT"));
        assert_eq!(first["street"], json!("Bellamyplein"));
        assert_eq!(first["city"], json!("Amsterdam"));
        // `huis_nlt` carries the addition ("18-1") where `huisnummer` would
        // drop it. A different flat is a different address.
        assert_eq!(first["house_number"], json!("18-1"));
    }

    #[test]
    fn an_address_search_with_several_hits_asks_rather_than_picking() {
        // This body says numFound 523624 and carries ten. An address has no
        // identifying fact in this vocabulary, so ten hits are ten candidates
        // and doc[0] is not the answer just because it scored highest.
        let out = contract(PDOK)
            .extract(&body(BELLAMYPLEIN), &vocabulary())
            .unwrap();
        assert!(out.is_ambiguous());
        assert!(out.single().is_none());
        assert_eq!(body(BELLAMYPLEIN)["response"]["numFound"], json!(523624));
    }

    #[test]
    fn a_single_hit_address_search_resolves() {
        let mut only_one = body(BELLAMYPLEIN);
        let first = only_one["response"]["docs"][0].clone();
        only_one["response"]["docs"] = json!([first]);
        let out = contract(PDOK).extract(&only_one, &vocabulary()).unwrap();
        assert_eq!(out.subjects.len(), 1);
        assert!(!out.is_ambiguous());
    }

    #[test]
    fn a_contract_round_trips_through_json() {
        let original = contract(BASISPROFIEL);
        let text = serde_json::to_string(&original).unwrap();
        let back: ExecutionContract = serde_json::from_str(&text).unwrap();
        assert_eq!(original, back);
    }
}
