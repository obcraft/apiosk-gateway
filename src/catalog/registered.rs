//! Direct purchases use the provider's published inputs. Planner mappings are
//! optional: an unmapped response is kept as a source document, not asserted as
//! normalized facts. Nothing is written to capability_execution_contracts.
use super::{KnownSource, ListedEndpoint, Service, Snapshot};
use anyhow::{ensure, Context};
use catalog_core::facts::{contract::{FactType, InputBinding, Requirement}, schema};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registration {
    pub list_atomic: Option<i64>,
    pub query_params: Option<Value>,
    pub request_body: Option<Value>,
    pub listing_type: String,
    pub usage_notice: Option<Value>,
}

pub(super) fn extend(snapshot: &mut Snapshot) {
    for source in &snapshot.known_sources {
        if source.block_reason.is_some() { continue; }
        for endpoint in &source.metadata.endpoints {
            if let Ok((service, vocabulary)) = operation(source, endpoint) {
                snapshot.services.push(service);
                snapshot.vocabulary.extend(vocabulary);
            }
        }
    }
}

fn operation(source: &KnownSource, endpoint: &ListedEndpoint) -> anyhow::Result<(Service, Vec<FactType>)> {
    let registration = endpoint.registration.as_ref().context("missing registered inputs")?;
    let price = registration.list_atomic.context("missing price")?;
    ensure!((0..1_000_000_000_000).contains(&price), "invalid price");
    ensure!(matches!(endpoint.method.as_str(), "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD"), "unsupported method");
    ensure!(registration.query_params.is_some() || registration.request_body.is_some(), "missing input schema");
    let mut fields = BTreeMap::new();
    for (location, shape) in [("query", &registration.query_params), ("body", &registration.request_body)] {
        if let Some(shape) = shape { collect(shape, location, &mut fields)?; }
    }
    // Path values are always required, even when omitted from the schema.
    for segment in endpoint.path.split('/') {
        let name = segment.strip_prefix('{').and_then(|s| s.strip_suffix('}'))
            .or_else(|| segment.strip_prefix(':'));
        if let Some(name) = name {
            let field = fields.entry(name.to_owned()).or_insert_with(|| ("path".into(), true, json!({"type":"string"})));
            field.0 = "path".into(); field.1 = true;
        }
    }
    ensure!(fields.len() <= 64, "too many input fields");
    let mut vocabulary = Vec::new();
    let mut requires = Vec::new();
    let mut bindings = BTreeMap::new();
    for (name, (location, required, mut value_schema)) in fields {
        ensure!(!name.is_empty() && name.len() <= 128 && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.:$[]-".contains(&b))
            && !["__proto__", "constructor", "prototype"].contains(&name.as_str()), "unsupported input name");
        // Adding another field must never reinterpret a form the user already
        // opened. Identity follows the wire name/location, not its list index.
        let field = format!("source_input.e{}.p{}", endpoint.endpoint_id.simple(), crate::state::digest(&(&location, &name)));
        value_schema.as_object_mut().context("invalid input schema")?.entry("title").or_insert_with(|| json!(name));
        schema::check_schema(&value_schema)?;
        let default = value_schema.get("default").cloned();
        if let Some(value) = &default { schema::validate(&value_schema, value)?; }
        vocabulary.push(FactType { slug: field.clone(), value_schema });
        requires.push(Requirement { fact_type: field.clone(), subject_role: "subject".into(), optional: !required });
        bindings.insert(field, InputBinding { location: Some(location), name, wrap: None, default });
    }
    let output = format!("source_document.e{}.response", endpoint.endpoint_id.simple());
    vocabulary.push(FactType { slug: output.clone(), value_schema: json!({"type":"object","required":["document"]}) });
    let service = serde_json::from_value(json!({
        "id":endpoint.endpoint_id,"endpoint_id":endpoint.endpoint_id,"unverified":true,
        "registered_input_hash":crate::state::digest(registration),
        "api_slug":source.slug,"name":source.name,"description":endpoint.description,"source_url":endpoint.source_url,
        "usage_notice":registration.usage_notice,"method":endpoint.method,"path":endpoint.path,
        "listing_type":registration.listing_type,"list_atomic":price,
        "fixed_params":{},"preconditions":{},"pagination":{},"freshness_seconds":3600,
        "sample_payload":null,"jurisdiction":null,
        "contract":{"capability_slug":format!("endpoint.{}",endpoint.endpoint_id.simple()),"version":1,
            "cardinality":"one","requires":requires,"input_map":bindings,
            "output_map":{"base":"$","produces":{output:{"adapter":"document.response","key":"document"}}},
            "identifies":null,"max_fan_out":1}
    }))?;
    Ok((service, vocabulary))
}

type Fields = BTreeMap<String, (String, bool, Value)>;

/// Both registry dialects are supported: JSON Schema and flat/grouped parameter
/// maps. Preserve nested object/array schemas instead of dropping those inputs.
fn collect(shape: &Value, location: &str, fields: &mut Fields) -> anyhow::Result<()> {
    let object = shape.as_object().context("inputs must be an object")?;
    if object.is_empty() { return Ok(()); }
    if let Some(properties) = object.get("properties") {
        schema::check_schema(&normalize(shape)?)?;
        let properties = properties.as_object().context("invalid properties")?;
        let required = object.get("required").and_then(Value::as_array);
        if let Some(required) = required {
            ensure!(required.iter().all(|name| name.as_str().is_some_and(|name| properties.contains_key(name))), "undeclared required input");
        }
        for (name, spec) in properties {
            let required = required.is_some_and(|names| names.contains(&json!(name))) || spec.get("required") == Some(&json!(true));
            field(name, spec, location, required, fields)?;
        }
    } else if object.keys().any(|key| matches!(key.as_str(), "type" | "$ref" | "oneOf" | "anyOf")) {
        anyhow::bail!("input schema does not declare fields");
    } else {
        for (name, spec) in object {
            let group = match name.as_str() {
                "query" | "queryParams" => Some("query"), "body" | "bodyFields" => Some("body"), "path" | "pathParams" => Some("path"), _ => None,
            };
            if let Some(group) = group { collect(spec, group, fields)?; }
            else { field(name, spec, location, spec.get("required") == Some(&json!(true)), fields)?; }
        }
    }
    Ok(())
}

fn field(name: &str, spec: &Value, location: &str, required: bool, fields: &mut Fields) -> anyhow::Result<()> {
    let location = spec.get("x-parameter-location").or_else(|| spec.get("in")).and_then(Value::as_str).unwrap_or(location);
    ensure!(matches!(location, "query" | "body" | "path"), "unsupported input location");
    ensure!(!fields.contains_key(name), "ambiguous input name across locations");
    fields.insert(name.to_owned(), (location.into(), required || location == "path", normalize(spec)?));
    Ok(())
}

fn normalize(spec: &Value) -> anyhow::Result<Value> {
    let mut result = Map::new();
    for (key, value) in spec.as_object().context("invalid field schema")? {
        match key.as_str() {
            "required" if value.is_boolean() => {},
            "x-parameter-location" | "in" | "example" => {},
            "properties" => {
                let properties = value.as_object().context("invalid properties")?.iter()
                    .map(|(name, spec)| Ok((name.clone(), normalize(spec)?))).collect::<anyhow::Result<Map<_, _>>>()?;
                result.insert(key.clone(), Value::Object(properties));
            },
            "items" => { result.insert(key.clone(), normalize(value)?); },
            _ => { result.insert(key.clone(), value.clone()); },
        }
    }
    Ok(Value::Object(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;

    fn snapshot() -> Snapshot {
        serde_json::from_str(include_str!("../../tests/fixtures/registered-endpoints.json")).unwrap()
    }

    #[test]
    fn registered_inputs_make_all_three_reported_endpoints_purchasable() {
        let catalog = Catalog::build(snapshot());
        assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
        assert_eq!(catalog.services.len(), 3);
        for service in &catalog.services {
            assert!(service.is_registered_operation());
            assert!(service.unverified);
            assert_eq!(service.list_atomic, 5_000);
            assert!(service.contract.requires.iter().all(|field| catalog.vocabulary[&field.fact_type].value_schema["title"].is_string()));
            let extraction = service.contract.extract(&json!({"results":[{"price":12}]}), &catalog.vocabulary).unwrap();
            assert!(extraction.rejected.is_empty());
        }
    }

    #[test]
    fn typed_inputs_defaults_and_nested_values_remain_enforced() {
        let catalog = Catalog::build(snapshot());
        let service = catalog.services.iter().find(|service| service.path.ends_with("classified-listing-search")).unwrap();
        let field = |name| service.contract.input_map.iter().find(|(_, binding)| binding.name == name).unwrap().0;
        let proxies = &catalog.vocabulary[field("proxiesSx")].value_schema;
        for value in [json!(true), json!({"country":"US","enabled":false})] { assert!(schema::validate(proxies, &value).is_ok()); }
        for value in [json!(42), json!("yes"), json!({"country":"invalid"}), json!({"proxyUrl":"https://example.com"})] {
            assert!(schema::validate(proxies, &value).is_err(), "{value}");
        }
        assert!(schema::validate(&catalog.vocabulary[field("limit")].value_schema, &json!(101)).is_err());
        assert_eq!(service.contract.input_map[field("limit")].default, Some(json!(25)));
    }

    #[test]
    fn schema_price_or_availability_changes_invalidate_the_selected_service() {
        let original = snapshot();
        let before = Catalog::build(original.clone());
        let id = original.known_sources[0].metadata.endpoints[0].endpoint_id;
        let hash = before.services.iter().find(|service| service.id == id).unwrap().version_hash();
        for change in ["price", "schema", "blocked", "missing-price"] {
            let mut snapshot = original.clone();
            let source = &mut snapshot.known_sources[0];
            let registered = source.metadata.endpoints[0].registration.as_mut().unwrap();
            match change {
                "price" => registered.list_atomic = Some(10_000),
                "schema" => registered.query_params.as_mut().unwrap()["properties"]["address"]["minLength"] = json!(42),
                "blocked" => source.block_reason = Some("Source paused".into()),
                _ => registered.list_atomic = None,
            }
            let after = Catalog::build(snapshot);
            assert!(after.services.iter().find(|service| service.id == id).is_none_or(|service| service.version_hash() != hash), "{change}");
        }
    }

    #[test]
    fn a_broken_planner_mapping_does_not_disable_valid_registered_inputs() {
        let mut snapshot = snapshot();
        let mut mapped = Catalog::build(snapshot.clone()).services[0].clone();
        mapped.id = uuid::Uuid::new_v4();
        let endpoint = mapped.endpoint_id;
        snapshot.services.push(mapped.clone());
        assert_eq!(Catalog::build(snapshot.clone()).services.iter().filter(|service| service.endpoint_id == endpoint).count(), 1);
        snapshot.services[0].list_atomic = -1;
        let catalog = Catalog::build(snapshot);
        assert!(catalog.services.iter().any(|service| service.endpoint_id == endpoint && service.is_registered_operation()));
        assert!(catalog.rejected.iter().any(|(id, _)| *id == mapped.id));
    }

    #[test]
    fn flat_grouped_optional_and_path_inputs_do_not_need_manual_review() {
        let mut snapshot = snapshot();
        let endpoint = &mut snapshot.known_sources[0].metadata.endpoints[0];
        endpoint.path = "/tokens/{address}".into();
        endpoint.registration.as_mut().unwrap().query_params = Some(json!({"queryParams":{"chain":{"type":"string","default":"base"}}}));
        let id = endpoint.endpoint_id;
        let catalog = Catalog::build(snapshot);
        let service = catalog.services.iter().find(|service| service.id == id).unwrap();
        assert!(service.contract.input_map.values().any(|field| field.name == "address" && field.location.as_deref() == Some("path")));
        assert_eq!(service.contract.requires.iter().filter(|field| !field.optional).count(), 1);
    }

    #[test]
    fn adding_a_parameter_does_not_change_existing_form_field_identities() {
        let mut snapshot = snapshot();
        let before = Catalog::build(snapshot.clone());
        let endpoint = &mut snapshot.known_sources[0].metadata.endpoints[0];
        let id = endpoint.endpoint_id;
        endpoint.registration.as_mut().unwrap().query_params.as_mut().unwrap()["properties"]["aaa_new_option"] = json!({"type":"string"});
        let after = Catalog::build(snapshot);
        let previous = before.services.iter().find(|service| service.id == id).unwrap();
        let current = after.services.iter().find(|service| service.id == id).unwrap();
        for (field, binding) in &previous.contract.input_map { assert_eq!(current.contract.input_map.get(field), Some(binding)); }
    }
}
