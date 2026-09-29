//! Bind the WebGPU-shaped parse to execution contracts without asking a second
//! model to recreate its subjects or identifiers. This is not an execution path.
use super::{ChatReply, Goal, Intent, ModelSelection, Subject};
use crate::{capability_intake::ParsedRequest, state::Task};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

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
        Ok(self.decode_reply(raw)?.0)
    }

    /// The reply and the planner's stated reason for returning no plan.
    pub fn decode_reply(&self, raw: &str) -> anyhow::Result<(ChatReply, Option<String>)> {
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
        Ok((ChatReply {
            message: reply.message,
            discover: reply.discover,
            intent,
        }, decline_reason))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
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
