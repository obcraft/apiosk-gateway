//! Turn the reader's structured reading of a question into an `Intent`.
//!
//! Nothing here knows what a company is. What a subject can be, and which
//! facts identify one, is read off the active contracts and the vocabulary:
//! a kind is the namespace of a fact some contract requires, and the facts
//! that identify a kind are exactly those required facts. Add a contract for
//! a new kind of subject and the reader offers it; retire the last one and it
//! stops. The compiler still validates coverage; this only names subjects and
//! wanted outputs.
use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::Value;

use super::compile::kind_of;
use super::{Candidate, Intent, OutputFormat, Subject};
use crate::facts::contract::Vocabulary;

/// A fact an asker can supply about a subject: which kind it belongs to and,
/// when the vocabulary constrains its shape, the pattern a value must match.
#[derive(Debug, Clone, PartialEq)]
pub struct InputFact {
    pub kind: String,
    pub pattern: Option<String>,
    pub value_type: String,
}

/// What the reader may choose from, derived from the live catalogue.
#[derive(Debug, Clone, Default)]
pub struct ReaderCatalogue {
    /// Capability → the facts of its own kind it yields.
    pub outputs_by_capability: BTreeMap<String, Vec<String>>,
    /// Fact slug → its kind and pattern, for every fact an active contract
    /// takes as input. These are the facts a subject can be known by.
    pub inputs: BTreeMap<String, InputFact>,
    pub inputs_by_capability: BTreeMap<String, Vec<Vec<String>>>,
    pub literal_url_inputs: BTreeSet<String>,
}

impl ReaderCatalogue {
    pub fn derive(candidates: &[Candidate], vocabulary: &Vocabulary) -> Self {
        let mut catalogue = ReaderCatalogue::default();
        for candidate in candidates.iter().filter(|c| c.active) {
            let kind = kind_of(&candidate.contract.capability_slug);
            let outputs = catalogue
                .outputs_by_capability
                .entry(candidate.contract.capability_slug.clone())
                .or_default();
            // Only facts of the capability's own kind: the `name` a lint tool
            // returns matched `company.name` by key and was listed as
            // something a company question wanted; the planner then bought it.
            outputs.extend(
                candidate
                    .contract
                    .output_map
                    .produces
                    .keys()
                    .filter(|fact| kind_of(fact) == kind)
                    .cloned(),
            );
            outputs.sort();
            outputs.dedup();
            // A capability whose every output belongs to another kind yields
            // NOTHING here, and offering it is how a question about a postcode
            // area came back "no validated source": the reader could only pick
            // from this list, picked one of these, and the pick produced no
            // wanted fact. PDOK's municipality boundary is the live case — its
            // `naam` matched `company.name` by key alone.
            if outputs.is_empty() {
                catalogue
                    .outputs_by_capability
                    .remove(&candidate.contract.capability_slug);
            }
            for requirement in &candidate.contract.requires {
                if requirement.optional {
                    continue;
                }
                if candidate
                    .contract
                    .input_map
                    .get(&requirement.fact_type)
                    .is_some_and(|binding| binding.name.eq_ignore_ascii_case("url"))
                    && vocabulary.get(&requirement.fact_type).is_some_and(|word| {
                        word.value_schema.get("type").and_then(Value::as_str) == Some("string")
                    })
                {
                    catalogue
                        .literal_url_inputs
                        .insert(requirement.fact_type.clone());
                }
                let pattern = vocabulary
                    .get(&requirement.fact_type)
                    .and_then(|t| t.value_schema.get("pattern"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                catalogue.inputs.insert(
                    requirement.fact_type.clone(),
                    InputFact {
                        kind: kind_of(&requirement.fact_type),
                        pattern,
                        value_type: vocabulary
                            .get(&requirement.fact_type)
                            .and_then(|t| t.value_schema.get("type"))
                            .and_then(Value::as_str)
                            .unwrap_or("string")
                            .to_owned(),
                    },
                );
            }
            let mut required: Vec<String> = candidate
                .contract
                .requires
                .iter()
                .filter(|r| !r.optional)
                .map(|r| r.fact_type.clone())
                .collect();
            required.sort();
            let alternatives = catalogue
                .inputs_by_capability
                .entry(candidate.contract.capability_slug.clone())
                .or_default();
            if !required.is_empty() && !alternatives.contains(&required) {
                alternatives.push(required);
            }
        }
        catalogue
    }

    /// The kinds of subject the catalogue can look up, sorted.
    pub fn kinds(&self) -> Vec<String> {
        let mut kinds: Vec<String> = self.inputs.values().map(|f| f.kind.clone()).collect();
        kinds.sort();
        kinds.dedup();
        kinds
    }

    /// The input facts of one kind, sorted by slug.
    pub fn inputs_of(&self, kind: &str) -> Vec<(&str, &InputFact)> {
        self.inputs
            .iter()
            .filter(|(_, f)| f.kind == kind)
            .map(|(slug, f)| (slug.as_str(), f))
            .collect()
    }

    /// Capabilities of one kind, sorted.
    fn capabilities_of(&self, kind: &str) -> Vec<&str> {
        self.outputs_by_capability
            .keys()
            .filter(|c| kind_of(c) == kind)
            .map(String::as_str)
            .collect()
    }
}

#[derive(Debug, Serialize)]
pub struct Clarification {
    error: &'static str,
    pub reason: &'static str,
    message: String,
    suggestions: Vec<String>,
}

/// `company_registry.kvknummer` → `kvknummer`, `company.financial-statements`
/// → `financial statements`. The last segment of a slug, in words.
fn words(slug: &str) -> String {
    slug.rsplit('.')
        .next()
        .unwrap_or(slug)
        .replace(['-', '_'], " ")
}

fn list(items: &[String], last: &str) -> String {
    match items.len() {
        0 => String::new(),
        1 => items[0].clone(),
        n => format!("{} {last} {}", items[..n - 1].join(", "), items[n - 1]),
    }
}

/// "a company by its name or kvknummer; a building by its postcode, …"
fn subjects_sentence(catalogue: &ReaderCatalogue, nl: bool) -> String {
    let parts: Vec<String> = catalogue
        .kinds()
        .iter()
        .map(|kind| {
            let facts: Vec<String> = catalogue
                .inputs_of(kind)
                .iter()
                .map(|(slug, _)| words(slug))
                .collect();
            let joined = list(&facts, if nl { "of" } else { "or" });
            if nl {
                format!("een {kind} met {joined}")
            } else {
                format!("a {kind} by its {joined}")
            }
        })
        .collect();
    parts.join("; ")
}

fn clarify(
    reason: &'static str,
    nl: bool,
    names: &[&str],
    kind: Option<&str>,
    chosen: &[String],
    question: Option<&str>,
    catalogue: &ReaderCatalogue,
) -> Clarification {
    let name = names.first().copied();
    let subjects = subjects_sentence(catalogue, nl);
    let subject_word = name
        .map(str::to_owned)
        .or_else(|| {
            kind.map(|k| {
                if nl {
                    format!("deze {k}")
                } else {
                    format!("this {k}")
                }
            })
        })
        .unwrap_or_else(|| {
            if nl {
                "dit onderwerp".into()
            } else {
                "this subject".into()
            }
        });
    // Capabilities to name as examples: what the reader chose, else the
    // subject's kind, else anything the catalogue offers.
    let examples: Vec<String> = if !chosen.is_empty() {
        chosen.to_vec()
    } else {
        let mut of_kind: Vec<String> = kind
            .map(|k| {
                catalogue
                    .capabilities_of(k)
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if of_kind.is_empty() {
            of_kind = catalogue.outputs_by_capability.keys().cloned().collect();
        }
        of_kind
    };
    let labels: Vec<String> = examples.iter().take(3).map(|c| words(c)).collect();
    let for_example = list(&labels, if nl { "of" } else { "or" });

    let message = question.map(str::to_owned).unwrap_or_else(|| match (reason, nl) {
        ("no_subject", true) => format!("Waar gaat het onderzoek over? Noem het onderwerp: {subjects}."),
        ("no_subject", false) => format!("What should I research? Name the subject: {subjects}."),
        ("unsupported_output", true) => format!("Voor dit onderdeel is nog geen gevalideerde bron aan de researchplanner gekoppeld. Wat wil je precies controleren over {subject_word}? Beschikbaar: {for_example}."),
        ("unsupported_output", false) => format!("This part has no validated source connected to the research planner yet. What exactly do you need to check about {subject_word}? Available: {for_example}."),
        (_, true) => format!("Wat wil je precies weten over {subject_word}? Bijvoorbeeld: {for_example}."),
        (_, false) => format!("What exactly do you need to know about {subject_word}? For example: {for_example}."),
    });

    // Suggestions are complete questions the asker can send as-is. WHERE THE
    // QUESTION NAMED SEVERAL SUBJECTS, SO DO THE SUGGESTIONS: an offer to
    // research one of them is what dropped the other from the answer. Names
    // come only from the question; nothing here invents a subject.
    let target = if names.len() > 1 {
        names.join(if nl { " en " } else { " and " })
    } else {
        name.map(str::to_owned).unwrap_or_else(|| {
            let k = kind.unwrap_or("subject");
            if nl {
                format!("<{k}>")
            } else {
                format!("<{k}>")
            }
        })
    };
    let verb = match (names.len() > 1, nl) {
        (true, true) => "Vergelijk de",
        (true, false) => "Compare the",
        (false, true) => "Toon de",
        (false, false) => "Show the",
    };
    let suggestions = examples
        .iter()
        .take(2)
        .map(|c| {
            sentence(format!(
                "{verb} {} {} {target}",
                words(c),
                if nl { "van" } else { "of" }
            ))
        })
        .collect();
    Clarification {
        error: "needs_input",
        reason,
        message,
        suggestions,
    }
}

/// Keep a value the reader attributed to a subject only if the catalogue knows
/// the fact and the question itself states the value. This applies to names as
/// well as identifiers: changing "Mollie" into "Mollie B.V." from model memory
/// can point the entire paid DAG at a different legal entity just as surely as
/// inventing a KVK number can.
fn accept(question: &str, slug: &str, value: &str, catalogue: &ReaderCatalogue) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    let Some(input) = catalogue.inputs.get(slug) else {
        return false;
    };
    let question_lower = question.to_lowercase();
    let stated = question_lower.contains(&value.to_lowercase())
        || serde_json::from_str::<Value>(value)
            .ok()
            .is_some_and(|parsed| {
                fn all_leaves_are_stated(value: &Value, question: &str) -> bool {
                    match value {
                        Value::Array(values) => {
                            !values.is_empty()
                                && values
                                    .iter()
                                    .all(|value| all_leaves_are_stated(value, question))
                        }
                        Value::String(value) => question.contains(&value.to_lowercase()),
                        Value::Number(value) => question.contains(&value.to_string()),
                        Value::Bool(value) => question.contains(&value.to_string()),
                        // An object shape is meaning, not just a list of values. It
                        // must be stated literally or supplied through a typed API.
                        Value::Object(_) | Value::Null => false,
                    }
                }
                all_leaves_are_stated(&parsed, &question_lower)
            });
    if !stated {
        return false;
    }
    match &input.pattern {
        None => true,
        Some(pattern) => {
            let matches = regex::Regex::new(pattern)
                .map(|re| re.is_match(value))
                .unwrap_or(false);
            matches
        }
    }
}

pub fn from_question(
    question: &str,
    parsed: &Value,
    catalogue: &ReaderCatalogue,
) -> Result<Intent, Clarification> {
    let nl = parsed.pointer("/question/language").and_then(Value::as_str) == Some("nl");
    let kinds = catalogue.kinds();
    let entities: Vec<&Value> = parsed
        .get("resolved_entities")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entity| {
            entity
                .get("kind")
                .and_then(Value::as_str)
                .is_some_and(|k| kinds.iter().any(|known| known == k))
        })
        .collect();
    let names: Vec<&str> = entities
        .iter()
        .filter_map(|e| e.get("name").and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let first_kind = entities
        .first()
        .and_then(|e| e.get("kind"))
        .and_then(Value::as_str);
    let chosen: Vec<String> = parsed
        .get("execution_plan")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| s.get("capability").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();

    // ONE SUBJECT PER ENTITY THE QUESTION NAMES. A question about two
    // subjects that plans for one of them is not a shorter answer, it is an
    // answer about somebody else — so the second gets its own role, its own
    // lookup, and its own line on the card.
    let mut subjects: Vec<Subject> = Vec::new();
    for entity in &entities {
        let kind = entity
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut known = BTreeMap::new();
        // Sparse entries keep the model's response proportional to the
        // question, not to every input in a catalog of thousands of endpoints.
        // Continue accepting the previous object form for saved readings.
        let entries: Vec<(&str, &Value)> = match entity.get("known") {
            Some(Value::Object(object)) => object.iter().map(|(k, v)| (k.as_str(), v)).collect(),
            Some(Value::Array(array)) => array
                .iter()
                .filter_map(|entry| {
                    let reference = entry.get("fact_type")?;
                    let slug = match reference.as_u64() {
                        Some(index) => catalogue
                            .inputs
                            .keys()
                            .nth(usize::try_from(index).ok()?)?
                            .as_str(),
                        None => reference.as_str()?,
                    };
                    Some((slug, entry.get("value")?))
                })
                .collect(),
            _ => Vec::new(),
        };
        for (slug, value) in &entries {
            if entries.iter().filter(|(key, _)| key == slug).count() != 1 {
                continue;
            }
            let Some(value) = value.as_str() else {
                continue;
            };
            // A fact of another kind attributed to this subject is the
            // reader's confusion, not a fact about it.
            if catalogue.inputs.get(*slug).is_some_and(|f| f.kind == kind)
                && accept(question, slug, value, catalogue)
            {
                let input = &catalogue.inputs[*slug];
                let typed = if input.value_type == "string" {
                    Some(Value::String(value.trim().to_owned()))
                } else {
                    serde_json::from_str::<Value>(value.trim())
                        .ok()
                        .filter(|v| match input.value_type.as_str() {
                            "integer" => v.is_i64() || v.is_u64(),
                            "number" => v.is_number(),
                            "boolean" => v.is_boolean(),
                            "array" => v.is_array(),
                            _ => false,
                        })
                };
                if let Some(value) = typed {
                    known.insert((*slug).to_owned(), value);
                }
            }
        }
        if known.is_empty() {
            continue;
        }
        subjects.push(Subject {
            role: role_for(subjects.len()),
            known,
        });
    }
    if subjects.is_empty() {
        // A literal URL is already user-supplied evidence. When the chosen
        // capability declares exactly one matching URL input, preserve it
        // even if the model omits known. Never pick between multiple URLs or
        // map a URL onto an unrelated input such as a company's name.
        let urls: BTreeSet<String> = regex::Regex::new(r#"https?://[^\s<>\"']+"#)
            .unwrap()
            .find_iter(question)
            .map(|m| {
                m.as_str()
                    .trim_end_matches(['.', ',', ';', ')', ']'])
                    .to_owned()
            })
            .collect();
        let inputs: BTreeSet<&String> = chosen
            .iter()
            .filter_map(|cap| catalogue.inputs_by_capability.get(cap))
            .flatten()
            .flatten()
            .filter(|slug| catalogue.literal_url_inputs.contains(*slug))
            .collect();
        if urls.len() == 1 && inputs.len() == 1 {
            let slug = inputs.first().unwrap();
            let value = urls.first().unwrap();
            if accept(question, slug, value, catalogue) {
                subjects.push(Subject {
                    role: role_for(0),
                    known: BTreeMap::from([((*slug).clone(), Value::String(value.clone()))]),
                });
            }
        }
    }
    if subjects.is_empty() {
        return Err(clarify(
            "no_subject",
            nl,
            &names,
            first_kind,
            &chosen,
            None,
            catalogue,
        ));
    }
    if let Some(asked) = parsed
        .pointer("/question/clarifying_question")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
    {
        return Err(clarify(
            "context",
            nl,
            &names,
            first_kind,
            &chosen,
            Some(asked),
            catalogue,
        ));
    }
    let mut outputs = Vec::new();
    for step in parsed
        .get("execution_plan")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(capability) = step.get("capability").and_then(Value::as_str) else {
            continue;
        };
        // Execution contracts own the capability-to-fact mapping. A new
        // validated source needs no branch here and no App/MCP release.
        let Some(facts) = catalogue
            .outputs_by_capability
            .get(capability)
            .filter(|facts| !facts.is_empty())
        else {
            return Err(clarify(
                "unsupported_output",
                nl,
                &names,
                first_kind,
                &[],
                None,
                catalogue,
            ));
        };
        // Broad catalog capabilities share endpoints with different outputs.
        // Buying the union would turn one question into dozens of unrelated
        // requirements. New readings select the facts actually requested.
        let requested: Vec<&String> = if let Some(selected) = step.get("outputs") {
            let selected = selected.as_array().ok_or_else(|| {
                clarify(
                    "unsupported_output",
                    nl,
                    &names,
                    first_kind,
                    &[],
                    None,
                    catalogue,
                )
            })?;
            let selected_facts: Option<Vec<&String>> = selected
                .iter()
                .map(|v| match v.as_u64() {
                    Some(index) => usize::try_from(index).ok().and_then(|i| facts.get(i)),
                    None => facts.iter().find(|f| Some(f.as_str()) == v.as_str()),
                })
                .collect();
            if selected.is_empty() || selected_facts.is_none() {
                return Err(clarify(
                    "unsupported_output",
                    nl,
                    &names,
                    first_kind,
                    &[],
                    None,
                    catalogue,
                ));
            }
            selected_facts.unwrap()
        } else {
            facts.iter().collect() // compatibility with stored object readings
        };
        for fact in requested {
            if !outputs.contains(fact) {
                outputs.push(fact.clone());
            }
        }
    }
    if outputs.is_empty() {
        return Err(clarify(
            "no_output",
            nl,
            &names,
            first_kind,
            &[],
            None,
            catalogue,
        ));
    }
    Ok(Intent {
        subjects,
        required_outputs: outputs,
        optional_outputs: vec![],
        format: OutputFormat::Json,
        max_buyer_total_atomic: None,
        jurisdiction: agreed_country(&entities),
    })
}

/// End with one full stop. `Adyen N.V.` already carries its own, and a second
/// one is what made the chips read `Adyen N.V..`.
fn sentence(text: String) -> String {
    if text.ends_with('.') {
        text
    } else {
        format!("{text}.")
    }
}

/// Role names, in the order the question named the subjects. The first stays
/// `subject`, so a one-subject plan hashes exactly as it did before.
fn role_for(index: usize) -> String {
    match index {
        0 => "subject".to_string(),
        n => format!("subject_{}", n + 1),
    }
}

/// The jurisdiction every named subject agrees on, or none.
///
/// One country over two subjects is a filter on the catalogue; two different
/// ones is not something a single filter can say, and guessing one of them
/// would plan a Dutch lookup for a Belgian entity.
fn agreed_country(entities: &[&Value]) -> Option<String> {
    let mut agreed: Option<&str> = None;
    for country in entities
        .iter()
        .filter_map(|e| e.get("country").and_then(Value::as_str))
        .filter(|c| !c.trim().is_empty())
    {
        match agreed {
            Some(seen) if seen != country => return None,
            _ => agreed = Some(country),
        }
    }
    agreed.map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn input(kind: &str, pattern: Option<&str>) -> InputFact {
        InputFact {
            kind: kind.into(),
            pattern: pattern.map(str::to_owned),
            value_type: "string".into(),
        }
    }

    /// A catalogue as `derive` would build it from live rows: two company
    /// capabilities and one area capability, with the facts their contracts
    /// require. Slugs are the shape the harvester emits, not a fixed list.
    fn catalogue() -> ReaderCatalogue {
        ReaderCatalogue {
            literal_url_inputs: BTreeSet::new(),
            inputs_by_capability: BTreeMap::new(),
            outputs_by_capability: BTreeMap::from([
                (
                    "company.financial-statements".into(),
                    vec!["company.financial_statements".into()],
                ),
                (
                    "company.registry-profile".into(),
                    vec!["company.profile".into()],
                ),
                (
                    "area.demographics".into(),
                    vec!["area.aantalinwoners".into()],
                ),
            ]),
            inputs: BTreeMap::from([
                ("company.name".into(), input("company", None)),
                (
                    "company_registry.kvknummer".into(),
                    input("company", Some("^[0-9]{8}$")),
                ),
                (
                    "building.postcode".into(),
                    input("building", Some("^[0-9]{4}(\\s?[A-Za-z]{2})?$")),
                ),
            ]),
        }
    }

    fn from_question(question: &str, parsed: &Value) -> Result<Intent, Clarification> {
        super::from_question(question, parsed, &catalogue())
    }

    fn reading(entities: Value, plan: Value) -> Value {
        json!({
            "question": { "language": "en", "clarifying_question": null },
            "resolved_entities": entities,
            "execution_plan": plan,
        })
    }

    fn company(name: &str, kvk: Option<&str>) -> Value {
        json!({ "kind": "company", "name": name, "country": "NL",
                "known": { "company.name": name, "company_registry.kvknummer": kvk } })
    }

    #[test]
    fn kinds_and_identifying_facts_come_from_the_contracts_not_from_code() {
        let c = catalogue();
        assert_eq!(c.kinds(), ["building", "company"]);
        assert_eq!(
            c.inputs_of("company")
                .iter()
                .map(|(s, _)| *s)
                .collect::<Vec<_>>(),
            ["company.name", "company_registry.kvknummer"]
        );
    }

    #[test]
    fn a_capability_that_yields_nothing_of_its_own_kind_is_not_offered() {
        use crate::facts::contract::{ExecutionContract, FactType};
        let boundary: ExecutionContract = serde_json::from_value(json!({
            "capability_slug": "area.administrative-boundary",
            "version": 1,
            "cardinality": "many",
            "requires": [{ "fact_type": "company.name", "subject_role": "subject", "optional": false }],
            "input_map": { "company.name": { "in": "query", "name": "naam" } },
            // Both outputs belong to another kind: nothing here answers an
            // `area` question.
            "output_map": { "base": "$.features[*]", "produces": {
                "company.name": { "path": "$.properties.naam" }
            }}
        }))
        .unwrap();
        let candidate = Candidate {
            endpoint_method: "GET".into(),
            contract_id: "c1".into(),
            endpoint_id: "e1".into(),
            api_slug: "pdok".into(),
            endpoint_path: "/gemeentegebied/items".into(),
            contract: boundary,
            list_atomic: 10_000,
            class: crate::fees::ProviderClass::Managed,
            active: true,
            jurisdiction: Some("NL".into()),
        };
        let vocabulary = Vocabulary::from([(
            "company.name".to_string(),
            FactType {
                slug: "company.name".into(),
                value_schema: json!({ "type": "string" }),
            },
        )]);
        let derived = ReaderCatalogue::derive(&[candidate], &vocabulary);
        assert!(
            !derived
                .outputs_by_capability
                .contains_key("area.administrative-boundary"),
            "a capability the reader picks must be able to produce something"
        );
        // Its input fact still names a subject the catalogue can look up.
        assert_eq!(derived.kinds(), ["company"]);
    }

    #[test]
    fn a_named_company_becomes_a_subject_with_the_wanted_output() {
        let parsed = reading(
            json!([company("Picnic", None)]),
            json!([{ "step": 1, "capability": "company.financial-statements", "subject": "Picnic" }]),
        );
        let intent = from_question("Show the accounts of Picnic", &parsed).unwrap();
        assert_eq!(intent.subjects.len(), 1);
        assert_eq!(intent.subjects[0].role, "subject");
        assert_eq!(intent.subjects[0].known["company.name"], json!("Picnic"));
        assert!(!intent.subjects[0]
            .known
            .contains_key("company_registry.kvknummer"));
        assert_eq!(intent.required_outputs, ["company.financial_statements"]);
        assert_eq!(intent.jurisdiction.as_deref(), Some("NL"));
    }

    #[test]
    fn a_subject_of_another_kind_works_the_same_way() {
        let parsed = reading(
            json!([{ "kind": "building", "name": "postcode area 1012", "country": "NL",
                     "known": { "building.postcode": "1012" } }]),
            json!([{ "step": 1, "capability": "area.demographics", "subject": "postcode area 1012" }]),
        );
        let intent = from_question("How many residents has postcode area 1012?", &parsed).unwrap();
        assert_eq!(intent.subjects[0].known["building.postcode"], json!("1012"));
        assert_eq!(intent.required_outputs, ["area.aantalinwoners"]);
    }

    #[test]
    fn a_patterned_fact_is_kept_only_when_the_question_states_it() {
        // Stated: kept.
        let parsed = reading(
            json!([company("Picnic", Some("12345678"))]),
            json!([{ "step": 1, "capability": "company.registry-profile", "subject": "Picnic" }]),
        );
        let intent = from_question("Profile of Picnic, kvk 12345678", &parsed).unwrap();
        assert_eq!(
            intent.subjects[0].known["company_registry.kvknummer"],
            json!("12345678")
        );

        // Recalled from memory: dropped, the name remains.
        let intent = from_question("Profile of Picnic", &parsed).unwrap();
        assert!(!intent.subjects[0]
            .known
            .contains_key("company_registry.kvknummer"));

        // Stated but the wrong shape: dropped.
        let parsed = reading(
            json!([company("Picnic", Some("1234"))]),
            json!([{ "step": 1, "capability": "company.registry-profile", "subject": "Picnic" }]),
        );
        let intent = from_question("Profile of Picnic, kvk 1234", &parsed).unwrap();
        assert!(!intent.subjects[0]
            .known
            .contains_key("company_registry.kvknummer"));
    }

    #[test]
    fn a_free_text_name_from_model_memory_is_rejected_too() {
        let parsed = reading(
            json!([company("Mollie B.V.", None)]),
            json!([{ "step": 1, "capability": "company.registry-profile", "subject": "Mollie B.V." }]),
        );

        let err = from_question("Research the company behind mollie.com", &parsed).unwrap_err();
        assert_eq!(err.reason, "no_subject");

        let intent = from_question("Research Mollie B.V.", &parsed).unwrap();
        assert_eq!(
            intent.subjects[0].known["company.name"],
            json!("Mollie B.V.")
        );
    }

    #[test]
    fn a_fact_of_another_kind_is_not_attributed_to_this_subject() {
        let parsed = reading(
            json!([{ "kind": "company", "name": "Picnic", "country": null,
                     "known": { "company.name": "Picnic", "building.postcode": "1012 AB" } }]),
            json!([{ "step": 1, "capability": "company.registry-profile", "subject": "Picnic" }]),
        );
        let intent = from_question("Picnic at 1012 AB", &parsed).unwrap();
        assert!(!intent.subjects[0].known.contains_key("building.postcode"));
    }

    #[test]
    fn a_kind_the_catalogue_cannot_look_up_is_not_a_subject() {
        let parsed = reading(
            json!([{ "kind": "person", "name": "J. Doe", "country": null, "known": { "company.name": "J. Doe" } }]),
            json!([{ "step": 1, "capability": "company.registry-profile", "subject": "J. Doe" }]),
        );
        let err = from_question("Who is J. Doe?", &parsed).unwrap_err();
        assert_eq!(err.reason, "no_subject");
        // The question back names what CAN be looked up, from the catalogue.
        assert!(
            err.message.contains("a company by its name or kvknummer"),
            "{}",
            err.message
        );
        assert!(
            err.message.contains("a building by its postcode"),
            "{}",
            err.message
        );
        assert!(!err.message.to_lowercase().contains("mollie"));
    }

    #[test]
    fn the_question_back_never_invents_a_subject() {
        let parsed = reading(json!([]), json!([]));
        let err = from_question("Which has more residents?", &parsed).unwrap_err();
        assert_eq!(err.reason, "no_subject");
        for s in &err.suggestions {
            assert!(!s.contains("Mollie"), "{s}");
            assert!(s.contains('<'), "a placeholder, not an invented name: {s}");
        }
    }

    #[test]
    fn two_subjects_are_two_roles_and_the_suggestions_keep_both() {
        let parsed = reading(
            json!([company("Adyen N.V.", None), company("Mollie B.V.", None)]),
            json!([{ "step": 1, "capability": "company.unknown", "subject": "Adyen N.V." }]),
        );
        let err = from_question("Compare Adyen N.V. and Mollie B.V.", &parsed).unwrap_err();
        assert_eq!(err.reason, "unsupported_output");
        assert!(
            err.suggestions
                .iter()
                .all(|s| s.contains("Adyen N.V. and Mollie B.V.")),
            "{:?}",
            err.suggestions
        );
        assert!(
            err.suggestions.iter().all(|s| !s.ends_with("..")),
            "{:?}",
            err.suggestions
        );

        let parsed = reading(
            json!([company("Adyen N.V.", None), company("Mollie B.V.", None)]),
            json!([{ "step": 1, "capability": "company.registry-profile", "subject": "Adyen N.V." }]),
        );
        let intent = from_question("Compare Adyen N.V. and Mollie B.V.", &parsed).unwrap();
        assert_eq!(intent.subjects.len(), 2);
        assert_eq!(intent.subjects[1].role, "subject_2");
    }

    #[test]
    fn two_countries_are_no_jurisdiction_rather_than_the_first_one() {
        let mut be = company("Odoo", None);
        be["country"] = json!("BE");
        let parsed = reading(
            json!([company("Picnic", None), be]),
            json!([{ "step": 1, "capability": "company.registry-profile", "subject": "Picnic" }]),
        );
        let intent = from_question("Compare Picnic and Odoo", &parsed).unwrap();
        assert_eq!(intent.jurisdiction, None);
    }

    #[test]
    fn a_readers_own_clarifying_question_is_asked_verbatim() {
        let mut parsed = reading(
            json!([company("Picnic", None)]),
            json!([{ "step": 1, "capability": "company.registry-profile", "subject": "Picnic" }]),
        );
        parsed["question"]["clarifying_question"] =
            json!("Which Picnic: the grocer or the shipping firm?");
        let err = from_question("Picnic accounts", &parsed).unwrap_err();
        assert_eq!(err.reason, "context");
        assert_eq!(
            err.message,
            "Which Picnic: the grocer or the shipping firm?"
        );
    }

    #[test]
    fn dutch_questions_get_a_dutch_question_back_built_from_the_same_catalogue() {
        let mut parsed = reading(json!([]), json!([]));
        parsed["question"]["language"] = json!("nl");
        let err = from_question("Hoeveel inwoners?", &parsed).unwrap_err();
        assert!(
            err.message.starts_with("Waar gaat het onderzoek over?"),
            "{}",
            err.message
        );
        assert!(
            err.message.contains("een company met name of kvknummer"),
            "{}",
            err.message
        );
    }

    #[test]
    fn sparse_readings_keep_only_supplied_facts_and_preserve_wire_types() {
        let catalogue = ReaderCatalogue {
            literal_url_inputs: BTreeSet::new(),
            inputs_by_capability: BTreeMap::new(),
            inputs: BTreeMap::from([
                (
                    "geo.latitude".into(),
                    InputFact {
                        kind: "geo".into(),
                        pattern: None,
                        value_type: "number".into(),
                    },
                ),
                (
                    "geo.tags".into(),
                    InputFact {
                        kind: "geo".into(),
                        pattern: None,
                        value_type: "array".into(),
                    },
                ),
            ]),
            outputs_by_capability: BTreeMap::from([(
                "geo.lookup".into(),
                vec!["geo.result".into()],
            )]),
        };
        let parsed = reading(
            json!([{"kind":"geo","name":"location","known":[
                {"fact_type":"geo.latitude","value":"52.37"},
                {"fact_type":"geo.tags","value":"[\"park\"]"}
            ]}]),
            json!([{"step":1,"capability":"geo.lookup","subject":"location"}]),
        );
        let intent =
            super::from_question("location at 52.37 with park", &parsed, &catalogue).unwrap();
        assert_eq!(intent.subjects[0].known["geo.latitude"], json!(52.37));
        assert_eq!(intent.subjects[0].known["geo.tags"], json!(["park"]));
    }

    #[test]
    fn duplicate_sparse_facts_do_not_choose_an_arbitrary_subject_value() {
        let parsed = reading(
            json!([{"kind":"company","name":"Picnic","known":[
                {"fact_type":"company.name","value":"Picnic"},
                {"fact_type":"company.name","value":"Mollie"}
            ]}]),
            json!([{"step":1,"capability":"company.registry-profile","subject":"Picnic"}]),
        );
        assert!(from_question("Picnic", &parsed).is_err());
    }

    #[test]
    fn a_question_selects_outputs_instead_of_buying_the_whole_capability_union() {
        let mut catalogue = catalogue();
        catalogue
            .outputs_by_capability
            .get_mut("company.registry-profile")
            .unwrap()
            .push("company.unrelated".into());
        let mut parsed = reading(
            json!([company("Picnic", None)]),
            json!([
                {"step":1,"capability":"company.registry-profile","subject":"Picnic","outputs":["company.profile"]}
            ]),
        );
        let intent = super::from_question("Picnic profile", &parsed, &catalogue).unwrap();
        assert_eq!(intent.required_outputs, vec!["company.profile"]);
        parsed["execution_plan"][0]["outputs"] = json!(["area.aantalinwoners"]);
        assert!(super::from_question("Picnic profile", &parsed, &catalogue).is_err());
    }

    #[test]
    fn indexed_fact_references_resolve_only_within_the_current_catalogue() {
        let catalogue = catalogue();
        let name = catalogue
            .inputs
            .keys()
            .position(|k| k == "company.name")
            .unwrap();
        let mut parsed = reading(
            json!([{"kind":"company","name":"Picnic","known":[
                {"fact_type":name,"value":"Picnic"}
            ]}]),
            json!([{"step":1,"capability":"company.registry-profile","subject":"Picnic","outputs":[0]}]),
        );
        let intent = super::from_question("Picnic profile", &parsed, &catalogue).unwrap();
        assert_eq!(intent.subjects[0].known["company.name"], json!("Picnic"));
        assert_eq!(intent.required_outputs, vec!["company.profile"]);
        parsed["execution_plan"][0]["outputs"] = json!([9999]);
        assert!(super::from_question("Picnic profile", &parsed, &catalogue).is_err());
    }

    #[test]
    fn an_omitted_literal_url_is_bound_only_to_an_unambiguous_declared_url_input() {
        let catalogue = ReaderCatalogue {
            inputs: BTreeMap::from([("web.url".into(), input("web", None))]),
            outputs_by_capability: BTreeMap::from([(
                "web.summarize".into(),
                vec!["web.summary".into()],
            )]),
            inputs_by_capability: BTreeMap::from([(
                "web.summarize".into(),
                vec![vec!["web.url".into()]],
            )]),
            literal_url_inputs: BTreeSet::from(["web.url".into()]),
        };
        let parsed = reading(
            json!([]),
            json!([{"step":1,"capability":"web.summarize","subject":"page","outputs":[0]}]),
        );
        let intent =
            super::from_question("Summarize https://example.com.", &parsed, &catalogue).unwrap();
        assert_eq!(
            intent.subjects[0].known["web.url"],
            json!("https://example.com")
        );
        assert!(super::from_question(
            "Compare https://example.com and https://example.org",
            &parsed,
            &catalogue
        )
        .is_err());
        assert!(super::from_question("Summarize a page", &parsed, &catalogue).is_err());
    }
}
