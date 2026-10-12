//! Bind the WebGPU-shaped parse to execution contracts without asking a second
//! model to recreate its subjects or identifiers. This is not an execution path.
use super::{ChatReply, Goal, Intent, ModelSelection, Subject};
use crate::{
    capability_intake::{names_source, Intake, ParsedRequest},
    catalog::{Catalog, Service},
    state::Task,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(super) struct Bindings {
    subjects: BTreeMap<String, Subject>,
    values: BTreeMap<String, Value>,
}

impl Bindings {
    pub fn new(
        parsed: &ParsedRequest,
        question: &str,
        task: &Task,
        selection: Option<&ModelSelection>,
    ) -> Self {
        let mut bindings = Self::default();
        for subject in &parsed.subjects {
            bindings.subjects.insert(
                subject.id.clone(),
                Subject {
                    entity_ref: None,
                    label: subject.label.clone(),
                    values: BTreeMap::new(),
                },
            );
            bindings
                .values
                .insert(format!("{}.label", subject.id), json!(subject.label));
            for (kind, value) in &subject.identifiers {
                bindings
                    .values
                    .insert(format!("{}.identifiers.{kind}", subject.id), json!(value));
            }
        }
        for (index, entity) in task.entities.iter().enumerate() {
            bindings.subjects.insert(
                format!("context{}", index + 1),
                Subject {
                    entity_ref: Some(entity.entity_ref),
                    label: entity.label.clone(),
                    values: BTreeMap::new(),
                },
            );
        }
        // Topic/date searches can have no named entity. The exact request is a
        // subject anchor, never an invented company name or identifier.
        if parsed.subjects.is_empty() {
            bindings.subjects.insert(
                "request".into(),
                Subject {
                    entity_ref: None,
                    label: question.to_owned(),
                    values: BTreeMap::new(),
                },
            );
        }
        bindings.values.insert("request".into(), json!(question));
        if let Some(selection) = selection {
            for (index, literal) in selection.literals.iter().enumerate() {
                bindings
                    .values
                    .insert(format!("selection{}", index + 1), json!(literal));
            }
        }
        bindings
    }

    pub fn view(&self) -> Value {
        json!({"subjects":self.subjects,"values":self.values})
    }

    pub fn subject_schema(&self, input_types: impl Iterator<Item = String>) -> Value {
        let value = json!({"anyOf":[
            {"type":"object","required":["ref"],"additionalProperties":false,
                "properties":{"ref":{"type":"string","enum":self.values.keys().collect::<Vec<_>>()}}},
            {"type":"object","required":["literal"],"additionalProperties":false,
                "properties":{"literal":{"type":["string","number"]}}}
        ]});
        let properties: serde_json::Map<String, Value> =
            input_types.map(|kind| (kind, value.clone())).collect();
        json!({"type":"object","additionalProperties":false,"required":["subject_ref","values"],
        "properties":{
            "subject_ref":{"type":"string","enum":self.subjects.keys().collect::<Vec<_>>()},
            "values":{"type":"object","properties":properties,"additionalProperties":false}
        }})
    }

    #[cfg(test)]
    pub fn decode(&self, raw: &str) -> anyhow::Result<ChatReply> {
        self.decode_reply(raw)
    }

    /// The reply, with the planner's stated reason for returning no plan.
    pub fn decode_reply(&self, raw: &str) -> anyhow::Result<ChatReply> {
        let reply: Reply = serde_json::from_str(raw)?;
        let decline_reason = reply.decline_reason;
        let intent = reply
            .intent
            .map(|intent| -> anyhow::Result<Intent> {
                let subjects = intent
                    .subjects
                    .into_iter()
                    .map(|bound| -> anyhow::Result<Subject> {
                        let mut subject = self
                            .subjects
                            .get(&bound.subject_ref)
                            .ok_or_else(|| anyhow::anyhow!("unknown parsed subject reference"))?
                            .clone();
                        for (kind, value) in bound.values {
                            let value = match value {
                                BoundValue::Reference { reference } => self
                                    .values
                                    .get(&reference)
                                    .ok_or_else(|| {
                                        anyhow::anyhow!("unknown parsed value reference")
                                    })?
                                    .clone(),
                                BoundValue::Literal { literal } => literal,
                            };
                            subject.values.insert(kind, value);
                        }
                        Ok(subject)
                    })
                    .collect::<anyhow::Result<_>>()?;
                Ok(Intent {
                    subjects,
                    goals: intent.goals,
                    answer_operations: intent.answer_operations,
                    unresolved_references: intent.unresolved_references,
                })
            })
            .transpose()?;
        Ok(ChatReply { output_bindings: reply.output_bindings,
            message: reply.message,
            discover: reply.discover,
            intent,
            decline_reason,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    #[serde(default)]
    output_bindings: Vec<super::source_grounding::Binding>,
    message: String,
    discover: bool,
    intent: Option<BoundIntent>,
    #[serde(default)]
    decline_reason: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundIntent {
    subjects: Vec<BoundSubject>,
    goals: Vec<Goal>,
    answer_operations: Vec<String>,
    unresolved_references: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundSubject {
    subject_ref: String,
    values: BTreeMap<String, BoundValue>,
}
#[derive(Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum BoundValue {
    Reference {
        #[serde(rename = "ref")]
        reference: String,
    },
    Literal {
        literal: Value,
    },
}

/// Recover one explicit target for the admitted, fixed-EUR ECB operation.
/// The caller uses this only when the displayed parse has no subjects. Never
/// guess across currencies, replace an existing value, or reverse a pair.
pub(super) fn bind_ecb_target_literal(intent: &mut Intent, question: &str, catalog: &Catalog) -> bool {
    const CAPABILITY: &str = "fx.ecb-reference-rate";
    const INPUT: &str = "fx.ecb.quote";
    let mut goals = intent.goals.iter().filter(|goal| goal.capability == CAPABILITY);
    let Some(goal) = goals.next() else { return false };
    if goals.next().is_some() { return false; }
    let subject_index = goal.subject;
    if !catalog.services.iter().any(|service| service.contract.capability_slug == CAPABILITY
        && service.contract.requires.iter().any(|required| required.fact_type == INPUT && !required.optional)) {
        return false;
    }
    let Some(allowed) = catalog.vocabulary.get(INPUT)
        .and_then(|fact| fact.value_schema.get("enum")).and_then(Value::as_array) else { return false };
    let tokens: Vec<_> = question.split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty()).collect();
    let targets: BTreeSet<_> = allowed.iter().filter_map(Value::as_str)
        .filter(|code| *code != "EUR" && code.len() == 3 && code.bytes().all(|b| b.is_ascii_uppercase()))
        .filter(|code| tokens.contains(code)).collect();
    if targets.len() != 1 { return false; }
    let target = *targets.first().unwrap();
    // Require an explicit, single EUR-first pair. A lone USD token does not
    // reveal the base: BTC to USD must never become EUR to USD. Other
    // currency-shaped uppercase codes make the empty-subject request unclear.
    if tokens.iter().filter(|token| **token == "EUR").count() != 1
        || tokens.iter().filter(|token| **token == target).count() != 1
        || tokens.iter().any(|token| token.len() == 3
            && token.bytes().all(|byte| byte.is_ascii_uppercase())
            && !matches!(*token, "EUR" | "ECB") && *token != target) {
        return false;
    }
    let pair = tokens.windows(2).any(|pair| pair == ["EUR", target])
        || tokens.windows(3).any(|pair| pair[0] == "EUR" && pair[2] == target
            && (pair[1].eq_ignore_ascii_case("to") || pair[1].eq_ignore_ascii_case("naar")));
    if !pair { return false; }
    let Some(subject) = intent.subjects.get_mut(subject_index) else { return false };
    if subject.values.contains_key(INPUT) { return false; }
    subject.values.insert(INPUT.into(), json!(target));
    true
}

/// The Dutch company register. A Dutch company's registry data comes from
/// here unless the person names another source.
const DUTCH_REGISTER: &str = "kvk-dutch-business-register";

/// Which source each goal must be bought from, decided from the parse rather
/// than left to the planning model:
///
/// 1. A source the person named ("Include the KVK source") is a requirement,
///    not a ranking hint. The model is asked to carry it as a `source`
///    constraint but can leave it out, and the resolver then quotes the
///    cheapest provider of the same operation, such as a reseller covering the
///    same register.
/// 2. Otherwise a Dutch company (a KVK number or a Dutch legal form in its
///    name) is looked up at KVK when KVK offers the operation. A reseller's
///    search can come back incomplete and stop the request after payment;
///    the register itself is the authoritative record.
///
/// The resolver never swaps a required source for another provider, so the
/// quote uses it or no plan is made. A goal that already names a source is
/// left as is; a hint that names no provider of the goal's operation, or
/// several, adds nothing.
pub(super) fn bind_source_requirements(intent: &mut Intent, intake: &Intake, catalog: &Catalog) {
    let parsed = &intake.parsed_request;
    for goal in &mut intent.goals {
        if goal.constraints.contains_key("source") {
            continue;
        }
        let label = intent
            .subjects
            .get(goal.subject)
            .map(|s| s.label.to_lowercase())
            .unwrap_or_default();
        let hints: BTreeSet<&str> = parsed
            .capabilities
            .iter()
            .filter(|cap| {
                intake
                    .matches
                    .iter()
                    .any(|m| m.slug == cap.slug && m.operations.contains(&goal.capability))
            })
            // A hint belongs to its own subject: "KVK for A, Companies House for B".
            .filter(|cap| {
                cap.subject_id
                    .as_ref()
                    .and_then(|id| parsed.subjects.iter().find(|s| &s.id == id))
                    .is_none_or(|subject| subject.label.to_lowercase() == label)
            })
            .filter_map(|cap| cap.source_hint.as_deref())
            .collect();
        let providers = |offers: &dyn Fn(&Service) -> bool| -> Vec<&str> {
            catalog
                .services
                .iter()
                .filter(|s| s.contract.capability_slug == goal.capability && offers(s))
                .map(|s| s.api_slug.as_str())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        };
        let source = if hints.is_empty() {
            let dutch = dutch_legal_form(&label)
                || parsed.subjects.iter().any(|s| {
                    s.label.to_lowercase() == label
                        && s.identifiers.keys().any(|kind| kind.contains("kvk"))
                });
            (dutch && !providers(&|s| s.api_slug == DUTCH_REGISTER).is_empty())
                .then_some(DUTCH_REGISTER)
        } else {
            match providers(&|s| hints.iter().any(|hint| names_source(s, hint)))[..] {
                [source] => Some(source),
                _ => None,
            }
        };
        if let Some(source) = source {
            goal.constraints.insert("source".into(), json!(source));
        }
    }
}

/// "Coolblue B.V.", "Philips N.V.", "Bakkerij de Vries V.O.F.".
fn dutch_legal_form(label: &str) -> bool {
    let label = label.to_lowercase().replace('.', "");
    label.contains("besloten vennootschap")
        || label.contains("naamloze vennootschap")
        || label
            .split_whitespace()
            .last()
            .is_some_and(|form| ["bv", "nv", "vof"].contains(&form))
}

/// Only user-supplied text is eligible for literal bindings. Assistant replies,
/// source descriptions and results can explain context but cannot supply inputs.
pub(super) fn literal_context(
    question: &str,
    task: &Task,
    selection: Option<&ModelSelection>,
) -> String {
    std::iter::once(question)
        .chain(task.conversation.iter().map(|turn| turn.question.as_str()))
        .chain(
            selection
                .into_iter()
                .flat_map(|s| s.literals.iter().map(String::as_str)),
        )
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{capability_intake, catalog::Catalog, state::Entity};
    use uuid::Uuid;

    fn ecb_intent() -> Intent {
        serde_json::from_value(json!({"subjects":[{"label":"request","values":{}}],
            "goals":[{"subject":0,"capability":"fx.ecb-reference-rate","constraints":{}}],
            "answer_operations":[],"unresolved_references":[]})).unwrap()
    }

    #[test]
    fn ecb_target_binds_only_one_explicit_catalog_currency_without_guessing() {
        let catalog = Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/frankfurter/catalog.json")).unwrap());
        assert!(catalog.rejected.is_empty());
        for question in [
            "Wat is de nieuwste ECB-referentiewisselkoers van EUR naar USD?",
            "Latest ECB reference rate for EUR/USD.",
            "Latest ECB reference rate EUR to USD.",
        ] {
            let mut intent = ecb_intent();
            assert!(bind_ecb_target_literal(&mut intent, question, &catalog), "{question}");
            assert_eq!(intent.subjects[0].values["fx.ecb.quote"], "USD");
            assert_eq!(intent.subjects[0].values.len(), 1);
        }
        for question in [
            "ECB EUR to USD and GBP", "ECB EUR", "ECB EUR to USDT",
            "ECB EUR to usd", "ECB EUR to USDé", "ECB EUR to dollars", "Latest ECB interest rate",
            "ECB USD/EUR", "ECB USD naar EUR",
            "Latest ECB reference rate for USD", "ECB rate BTC to USD",
            "ECB rate BTC to USD, include EUR context", "ECB rate BTC to EUR to USD",
        ] {
            let mut intent = ecb_intent();
            assert!(!bind_ecb_target_literal(&mut intent, question, &catalog), "{question}");
            assert!(intent.subjects[0].values.is_empty());
        }
    }

    #[test]
    fn ecb_target_preserves_existing_values_and_cannot_bind_other_operations() {
        let catalog = Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/frankfurter/catalog.json")).unwrap());
        let mut intent = ecb_intent();
        intent.subjects[0].values.insert("fx.ecb.quote".into(), json!("GBP"));
        assert!(!bind_ecb_target_literal(&mut intent, "ECB EUR/USD", &catalog));
        assert_eq!(intent.subjects[0].values["fx.ecb.quote"], "GBP");
        let mut other = ecb_intent();
        other.goals[0].capability = "finance.exchange-rate".into();
        assert!(!bind_ecb_target_literal(&mut other, "EUR/USD", &catalog));
        assert!(other.subjects[0].values.is_empty());
        let mut several = ecb_intent();
        several.goals.push(serde_json::from_value(json!({"subject":0,"capability":"fx.ecb-reference-rate","constraints":{}})).unwrap());
        assert!(!bind_ecb_target_literal(&mut several, "ECB EUR/USD", &catalog));
        let mut missing = ecb_intent();
        let empty = Catalog::build(serde_json::from_value(json!({"services":[],"vocabulary":[],"known_sources":[]})).unwrap());
        assert!(!bind_ecb_target_literal(&mut missing, "ECB EUR/USD", &empty));
    }

    fn parsed() -> ParsedRequest {
        capability_intake::normalize(&json!({"language":"en","subjects":[
            {"id":"s1","label":"Coolblue B.V.","type":"company","identifiers":{"kvk_number":"12345678"}},
            {"id":"s2","label":"Example Ltd","type":"company","identifiers":{}}
        ],"capabilities":[{"slug":"company.accounts","name":"Company accounts","description":"Company accounts","domain":"company","inputs":["kvk_number"],"outputs":[],"subject_id":"s1","source_hint":null,"confidence":1}],
            "deliverable":{"format":"chat","operations":["compare"]}}).to_string(),
            "Compare accounts of Coolblue B.V. KVK 12345678 with Example Ltd", "fixture").unwrap()
    }
    fn reply(subject: Value) -> Value {
        json!({"message":"I'll show the price first.","discover":true,"intent":{"subjects":[subject],
            "goals":[{"capability":"company.accounts","subject":0,"constraints":{}}],"answer_operations":[],"unresolved_references":[]}})
    }

    #[test]
    fn subjects_and_identifiers_are_copied_from_the_parse_not_regenerated() {
        let bindings = Bindings::new(&parsed(), "Compare accounts", &Task::default(), None);
        let subject = json!({"subject_ref":"s1","values":{"company.name":{"ref":"s1.label"},"company.id":{"ref":"s1.identifiers.kvk_number"}}});
        let schema =
            bindings.subject_schema(["company.name".into(), "company.id".into()].into_iter());
        assert_eq!(schema["required"], json!(["subject_ref", "values"]));
        assert_eq!(schema["additionalProperties"], false);
        assert!(schema["properties"].get("label").is_none());
        let references = &schema["properties"]["values"]["properties"]["company.name"]["anyOf"][0];
        catalog_core::facts::schema::validate(references, &json!({"ref":"s1.label"})).unwrap();
        assert!(
            catalog_core::facts::schema::validate(references, &json!({"ref":"s99.label"})).is_err()
        );
        let result = bindings
            .decode(&reply(subject.clone()).to_string())
            .unwrap()
            .intent
            .unwrap();
        assert_eq!(result.subjects[0].label, "Coolblue B.V.");
        assert_eq!(result.subjects[0].values["company.name"], "Coolblue B.V.");
        assert_eq!(result.subjects[0].values["company.id"], "12345678");
        for (key, value) in [
            ("label", json!("Coolblue BV Netherlands")),
            ("subject_ref", json!("s99")),
            ("values", json!({"company.name":{"ref":"s99.label"}})),
        ] {
            let mut bad = subject.clone();
            bad[key] = value;
            assert!(bindings.decode(&reply(bad).to_string()).is_err());
        }
        let second = bindings
            .decode(
                &reply(json!({"subject_ref":"s2","values":{"company.name":{"ref":"s2.label"}}}))
                    .to_string(),
            )
            .unwrap()
            .intent
            .unwrap();
        assert_eq!(second.subjects[0].label, "Example Ltd");
    }

    #[test]
    fn literal_escape_hatch_still_rejects_invented_values() {
        let bindings = Bindings::new(&parsed(), "Coolblue B.V.", &Task::default(), None);
        let catalog = Catalog::build(
            serde_json::from_str(include_str!(
                "../../tests/fixtures/integration-catalog.json"
            ))
            .unwrap(),
        );
        let intent = bindings.decode(&reply(json!({"subject_ref":"s1","values":{"company.name":{"literal":"Invented company"}}})).to_string()).unwrap().intent.unwrap();
        assert_eq!(
            intent
                .validate("Coolblue B.V.", &Task::default(), &catalog)
                .unwrap_err()
                .to_string(),
            "parser invented a value"
        );
        assert!(bindings.decode(&reply(json!({"subject_ref":"s1","values":{"company.name":{"ref":"s1.label","literal":"Override"}}})).to_string()).is_err());
    }

    #[test]
    fn saved_entities_and_entered_selection_keep_their_original_bindings() {
        let mut task = Task::default();
        let entity_ref = Uuid::new_v4();
        task.entities.push(Entity {
            entity_ref,
            label: "Saved company".into(),
            selected: true,
        });
        let selection = ModelSelection {
            view: json!({}),
            literals: vec!["Explicit input".into()],
        };
        let bindings = Bindings::new(&parsed(), "Check that company", &task, Some(&selection));
        let intent = bindings.decode(&reply(json!({"subject_ref":"context1","values":{"company.name":{"ref":"selection1"}}})).to_string()).unwrap().intent.unwrap();
        assert_eq!(intent.subjects[0].entity_ref, Some(entity_ref));
        assert_eq!(intent.subjects[0].label, "Saved company");
        assert_eq!(intent.subjects[0].values["company.name"], "Explicit input");
    }

    /// The fixture's accounts operation, offered by the named register and by a
    /// cheaper reseller covering the same register.
    fn catalog_with_reseller() -> Catalog {
        let mut snapshot: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/integration-catalog.json"
        ))
        .unwrap();
        let services = snapshot["services"].as_array_mut().unwrap();
        let accounts = services
            .iter()
            .find(|s| s["api_slug"] == "fixture" && s["name"] == "company.accounts")
            .unwrap()
            .clone();
        for (slug, name, price) in [
            ("kvk-dutch-business-register", "KVK Business Register", 300),
            ("sirenic", "Sirenic", 50),
        ] {
            let mut service = accounts.clone();
            service["id"] = json!(Uuid::new_v4());
            service["endpoint_id"] = json!(Uuid::new_v4());
            service["api_slug"] = json!(slug);
            service["name"] = json!(name);
            service["list_atomic"] = json!(price);
            services.push(service);
        }
        Catalog::build(serde_json::from_value(snapshot).unwrap())
    }
    fn intake(hint: Option<&str>) -> Intake {
        let mut parsed = parsed();
        parsed.capabilities[0].source_hint = hint.map(str::to_owned);
        Intake {
            matches: vec![crate::capability_intake::Match {
                slug: parsed.capabilities[0].slug.clone(),
                query: "Company accounts".into(),
                operations: ["company.accounts".to_owned()].into(),
                apiosk: vec![],
                coinbase: vec![],
                coinbase_status: "complete".into(),
            }],
            parsed_request: parsed,
            apiosk_sources_searched: 3,
            catalog_version: "fixture".into(),
            selected_capabilities: ["company.accounts".to_owned()].into(),
        }
    }
    fn planned(subject_ref: &str) -> Intent {
        let bindings = Bindings::new(&parsed(), "Compare accounts", &Task::default(), None);
        bindings
            .decode(&reply(json!({"subject_ref":subject_ref,"values":{}})).to_string())
            .unwrap()
            .intent
            .unwrap()
    }

    #[test]
    fn a_named_source_or_a_dutch_company_requires_kvk_instead_of_a_cheaper_reseller() {
        let catalog = catalog_with_reseller();
        let mut named = planned("s1");
        bind_source_requirements(&mut named, &intake(Some("kvk")), &catalog);
        assert_eq!(
            named.goals[0].constraints["source"],
            "kvk-dutch-business-register"
        );

        let mut dutch = planned("s1");
        bind_source_requirements(&mut dutch, &intake(None), &catalog);
        assert_eq!(
            dutch.goals[0].constraints["source"], "kvk-dutch-business-register",
            "a Dutch company defaults to KVK"
        );

        let mut foreign = planned("s2");
        bind_source_requirements(&mut foreign, &intake(None), &catalog);
        assert!(
            foreign.goals[0].constraints.is_empty(),
            "a non-Dutch company is priced across providers"
        );

        let mut other_subject = planned("s2");
        bind_source_requirements(&mut other_subject, &intake(Some("kvk")), &catalog);
        assert!(
            other_subject.goals[0].constraints.is_empty(),
            "a hint applies to its own subject only"
        );

        let mut unknown = planned("s1");
        bind_source_requirements(&mut unknown, &intake(Some("companies house")), &catalog);
        assert!(
            unknown.goals[0].constraints.is_empty(),
            "a hint that names no provider adds nothing, not the Dutch default"
        );

        let mut chosen = planned("s1");
        chosen.goals[0]
            .constraints
            .insert("source".into(), json!("sirenic"));
        bind_source_requirements(&mut chosen, &intake(Some("kvk")), &catalog);
        assert_eq!(
            chosen.goals[0].constraints["source"], "sirenic",
            "an explicit plan constraint is kept"
        );
    }

    #[test]
    fn dutch_legal_forms_are_recognised_without_matching_inside_words() {
        for label in [
            "Coolblue B.V.",
            "Philips N.V.",
            "Bakkerij de Vries V.O.F.",
            "Example Besloten Vennootschap",
            "ASML Holding NV",
        ] {
            assert!(dutch_legal_form(label), "{label}");
        }
        for label in ["Example Ltd", "Bvlgari", "Novartis AG", "Coolblue"] {
            assert!(!dutch_legal_form(label), "{label}");
        }
    }

    #[test]
    fn a_clarification_can_use_user_context_but_not_assistant_inventions() {
        let mut task = Task::default();
        task.record_turn(
            "Compare Coolblue B.V. and Example Ltd",
            Some("Do you mean Invented Corp?".into()),
        );
        let literals = literal_context("Their accounts", &task, None);
        assert!(literals.contains("Coolblue B.V."));
        assert!(!literals.contains("Invented Corp"));
    }
}
