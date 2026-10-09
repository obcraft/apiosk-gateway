//! Keep the purchased external request's field names and HTTP locations.
//! Values may change only through the recipe's approved date/input rules.
use anyhow::{ensure, Context};
use catalog_core::facts::schema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    pub fields: Vec<crate::ask_v2::external::Input>,
    pub schema: Value,
}

impl Contract {
    pub fn materialize(&self, inputs: &Value) -> anyhow::Result<Value> {
        let values = inputs
            .as_object()
            .context("Missing external input object.")?;
        ensure!(
            serde_json::to_vec(inputs)?.len() <= 64 * 1024,
            "External inputs are too large."
        );
        schema::validate(&self.schema, inputs)?;
        let mut names = BTreeSet::new();
        let mut destinations = BTreeSet::new();
        let mut parts = json!({"path":{},"query":{},"body":{}});
        for field in &self.fields {
            ensure!(
                matches!(field.location.as_str(), "path" | "query" | "body")
                    && !field.name.is_empty()
                    && names.insert(field.field.as_str())
                    && destinations.insert((&field.location, &field.name)),
                "Invalid or ambiguous frozen external input mapping."
            );
            match values.get(&field.field) {
                Some(value) => {
                    schema::validate(&field.schema, value)?;
                    parts[&field.location][&field.name] = value.clone();
                }
                None => ensure!(!field.required, "A required external input is missing."),
            }
        }
        ensure!(
            values.keys().all(|key| names.contains(key.as_str())),
            "An external input has no frozen destination."
        );
        Ok(parts)
    }
}

/// Only a server-saved purchase proves how the initial flat inputs were sent.
pub fn capture(
    selection: &mut Value,
    purchase: &crate::ask_v2::purchase::Purchase,
) -> anyhow::Result<()> {
    ensure!(
        selection["id"] == purchase.selection_id.to_string()
            && selection["kind"] == "external"
            && selection["candidate_id"].is_null()
            && selection["resource_url"] == purchase.source.resource
            && selection["method"] == purchase.source.method
            && selection["provider_price_micro_usd"].as_i64()
                == Some(purchase.source.provider_atomic)
            && selection["charged_micro_usd"].as_i64() == Some(purchase.source.total_atomic)
            && selection["metadata"]["inputs"] == purchase.input.inputs,
        "The external selection differs from the purchased request."
    );
    let contract = Contract {
        fields: purchase.source.inputs.clone(),
        schema: purchase.source.input_schema.clone(),
    };
    ensure!(
        contract.materialize(&purchase.input.inputs)? == purchase.input.input_parts,
        "The original external request split cannot be reproduced."
    );
    selection["metadata"]["recurring_input_contract"] = serde_json::to_value(contract)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract() -> Contract {
        serde_json::from_value(json!({"fields":[
            {"field":"path.record","name":"record","location":"path","required":true,"schema":{"type":"string"}},
            {"field":"query.date","name":"as_of","location":"query","required":true,"schema":{"type":"string"}},
            {"field":"body.date","name":"as_of","location":"body","required":true,"schema":{"type":"string"}},
            {"field":"payload","name":"data.items","location":"body","required":true,"schema":{"type":"array","items":{}}}
        ],"schema":{"type":"object","properties":{"path.record":{"type":"string"},"query.date":{"type":"string"},"body.date":{"type":"string"},"payload":{"type":"array","items":{}}},"required":["path.record","query.date","body.date","payload"],"additionalProperties":false}})).unwrap()
    }

    #[test]
    fn external_values_keep_wire_names_locations_and_json_types() {
        let inputs = json!({"path.record":"a/b","query.date":"2026-10-09","body.date":"2026-10-08","payload":[null,false,0,{"x":1}]});
        let parts = contract().materialize(&inputs).unwrap();
        assert_eq!(
            parts,
            json!({"path":{"record":"a/b"},"query":{"as_of":"2026-10-09"},"body":{"as_of":"2026-10-08","data.items":[null,false,0,{"x":1}]}})
        );
        let mut moved = inputs;
        moved["query.date"] = json!("2026-10-10");
        let next = contract().materialize(&moved).unwrap();
        assert_eq!(next["body"], parts["body"]);
        assert_eq!(next["path"], parts["path"]);
        assert_eq!(next["query"]["as_of"], "2026-10-10");
    }

    #[test]
    fn missing_unknown_invalid_and_ambiguous_inputs_are_refused() {
        let inputs = json!({"path.record":"a","query.date":"2026-10-09","body.date":"2026-10-08","payload":[]});
        let mut missing = inputs.clone();
        missing.as_object_mut().unwrap().remove("query.date");
        assert!(contract().materialize(&missing).is_err());
        let mut unknown = inputs.clone();
        unknown["other"] = json!("x");
        assert!(contract().materialize(&unknown).is_err());
        let mut invalid = inputs.clone();
        invalid["payload"] = json!(false);
        assert!(contract().materialize(&invalid).is_err());
        let mut duplicate = contract();
        duplicate.fields[2].location = "query".into();
        assert!(duplicate.materialize(&inputs).is_err());
        let mut location = contract();
        location.fields[0].location = "header".into();
        assert!(location.materialize(&inputs).is_err());
    }

    #[test]
    fn quote_capture_requires_the_original_url_method_price_and_split() {
        let contract = contract();
        let inputs = json!({"path.record":"a","query.date":"2026-10-09","body.date":"2026-10-08","payload":[]});
        let id = uuid::Uuid::new_v4();
        let mut purchase:crate::ask_v2::purchase::Purchase=serde_json::from_value(json!({
            "request_hash":"hash","quote_ref":uuid::Uuid::new_v4(),"execution_id":uuid::Uuid::new_v4(),"selection_id":id,"expires_at":"2026-10-09T00:00:00Z","approved_at":"2026-10-08T00:00:00Z","refusal":null,"receipt":null,
            "source":{"resource":"https://example.com/{record}","method":"POST","name":"Example","description":"","source":"bazaar","host":"example.com","network":null,"provider_atomic":100,"total_atomic":115,"inputs":contract.fields,"input_schema":contract.schema},
            "input":{"inputs":inputs,"input_parts":contract.materialize(&inputs).unwrap()}
        })).unwrap();
        let selection = json!({"id":id,"kind":"external","candidate_id":null,"resource_url":purchase.source.resource,"method":"POST","provider_price_micro_usd":100,"charged_micro_usd":115,"metadata":{"inputs":inputs}});
        let mut saved = selection.clone();
        capture(&mut saved, &purchase).unwrap();
        let captured: Contract =
            serde_json::from_value(saved["metadata"]["recurring_input_contract"].clone()).unwrap();
        assert_eq!(
            captured.materialize(&inputs).unwrap(),
            purchase.input.input_parts
        );
        for (key, value) in [
            ("resource_url", json!("https://other.example/{record}")),
            ("method", json!("GET")),
            ("provider_price_micro_usd", json!(101)),
        ] {
            let mut changed = selection.clone();
            changed[key] = value;
            assert!(capture(&mut changed, &purchase).is_err());
        }
        purchase.input.input_parts["query"]["as_of"] = json!("different");
        assert!(capture(&mut selection.clone(), &purchase).is_err());
    }
}
