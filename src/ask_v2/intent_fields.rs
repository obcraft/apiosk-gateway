//! Ground model-proposed field names in the admitted catalog, without another
//! model call. The public v2 field lists contain only members of this registry.
//! A documented field is not a promise that a source returns it for every row.
use crate::{
    catalog::{Catalog, Service},
    parser::plan_coverage::output_aliases,
};
use anyhow::{anyhow, ensure, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize)]
pub struct Field {
    pub id: String,
    pub fact_type: String,
    /// JSON Pointer into fact_types.value_schema, not into a provider response.
    pub schema_pointer: String,
}

#[derive(Default)]
struct Fields {
    definitions: BTreeMap<String, Field>,
    names: BTreeMap<String, BTreeSet<String>>,
}

fn key(value: &str) -> String {
    let mut out = String::new();
    let mut previous_lower = false;
    for ch in value.chars() {
        if !ch.is_ascii_alphanumeric() {
            if !out.is_empty() && !out.ends_with('_') {
                out.push('_');
            }
            previous_lower = false;
        } else {
            if ch.is_ascii_uppercase() && previous_lower && !out.ends_with('_') {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
            previous_lower = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        }
    }
    out.trim_matches('_').to_owned()
}

fn source_key(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn source_matches(service: &Service, sources: &[Value], catalog: &Catalog) -> bool {
    sources.is_empty()
        || sources.iter().filter_map(Value::as_str).any(|source| {
            let source = source_key(source);
            !source.is_empty()
                && (source == source_key(&service.api_slug)
                    || source == source_key(&service.name)
                    || catalog.known_sources.iter().any(|known| {
                        known.slug == service.api_slug && source == source_key(&known.name)
                    }))
        })
}

// Fail closed on opaque and unsupported schemas. A prose description or an
// unknown/reference/composition schema cannot manufacture a field definition.
pub(crate) fn documented(schema: &Value) -> bool {
    if ["$ref", "anyOf", "oneOf", "allOf", "not", "if"]
        .iter()
        .any(|key| schema.get(*key).is_some())
    {
        return false;
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("string" | "number" | "integer" | "boolean") => true,
        Some("object") => schema["properties"]
            .as_object()
            .is_some_and(|p| !p.is_empty()),
        Some("array") => schema.get("items").is_some_and(documented),
        _ => false,
    }
}

impl Fields {
    fn add(&mut self, fact: &str, schema: &Value, pointer: &str, name: &str, depth: usize) {
        if depth > 32 || !documented(schema) {
            return;
        }
        let id = format!("{fact}#{pointer}");
        let field = Field {
            id: id.clone(),
            fact_type: fact.into(),
            schema_pointer: pointer.into(),
        };
        self.definitions.insert(id.clone(), field);
        let mut names = output_aliases(schema);
        names.insert(key(name));
        // Full fact names can be supplied as snake_case by the extraction model.
        if pointer.is_empty() {
            names.insert(key(fact));
        }
        for name in names.into_iter().filter(|name| !name.is_empty()) {
            self.names.entry(name).or_default().insert(id.clone());
        }
        if let Some(properties) = schema["properties"].as_object() {
            for (name, child) in properties {
                let escaped = name.replace('~', "~0").replace('/', "~1");
                self.add(
                    fact,
                    child,
                    &format!("{pointer}/properties/{escaped}"),
                    name,
                    depth + 1,
                );
            }
        }
        // The array itself owns its name. Traversing its items only exposes
        // named children, avoiding an ambiguous duplicate alias for the array.
        if schema["type"] == "array" {
            self.add_items(
                fact,
                &schema["items"],
                &format!("{pointer}/items"),
                depth + 1,
            );
        }
    }

    fn add_items(&mut self, fact: &str, schema: &Value, pointer: &str, depth: usize) {
        if depth > 32 || !documented(schema) {
            return;
        }
        if let Some(properties) = schema["properties"].as_object() {
            for (name, child) in properties {
                let escaped = name.replace('~', "~0").replace('/', "~1");
                self.add(
                    fact,
                    child,
                    &format!("{pointer}/properties/{escaped}"),
                    name,
                    depth + 1,
                );
            }
        }
        if schema["type"] == "array" {
            self.add_items(
                fact,
                &schema["items"],
                &format!("{pointer}/items"),
                depth + 1,
            );
        }
    }

    fn for_request(request: &Value, catalog: &Catalog) -> Result<Self> {
        let capability = request["capability_slug"]
            .as_str()
            .ok_or_else(|| anyhow!("missing capability"))?;
        let sources = request["sources"]
            .as_array()
            .ok_or_else(|| anyhow!("missing sources"))?;
        let mut fields = Self::default();
        for service in catalog.services.iter().filter(|service| {
            !service.unverified
                && service.contract.capability_slug == capability
                && source_matches(service, sources, catalog)
        }) {
            for fact in service.contract.output_map.produces.keys() {
                if let Some(definition) = catalog.vocabulary.get(fact) {
                    fields.add(
                        fact,
                        &definition.value_schema,
                        "",
                        fact.rsplit('.').next().unwrap_or(fact),
                        0,
                    );
                }
            }
        }
        Ok(fields)
    }

    fn resolve(&self, requested: &str) -> Result<&Field, &'static str> {
        if let Some(field) = self.definitions.get(requested) {
            return Ok(field);
        }
        let Some(ids) = self.names.get(requested) else {
            return Err("undocumented");
        };
        if ids.len() != 1 {
            return Err("ambiguous");
        }
        Ok(&self.definitions[ids.first().unwrap()])
    }
}

/// Convert the internal extraction envelope to the public schema v2. Only
/// catalog-derived IDs enter required_fields/optional_fields; every unresolved
/// request is retained separately. Never replace an unknown with a broad fact.
pub fn ground_response(response: &mut Value, catalog: &Catalog) -> Result<()> {
    ensure!(
        response["schema_version"] == 1,
        "expected ungrounded intent v1"
    );
    let intent = response
        .get_mut("intent")
        .ok_or_else(|| anyhow!("missing intent"))?;
    let requests = intent["requests"]
        .as_array_mut()
        .ok_or_else(|| anyhow!("missing requests"))?;
    let mut used = BTreeMap::new();
    for request in requests {
        let registry = Fields::for_request(request, catalog)?;
        let mut unresolved = json!({"required":[],"optional":[]});
        for (list, unresolved_list) in [
            ("required_fields", "required"),
            ("optional_fields", "optional"),
        ] {
            let proposed = request[list]
                .as_array()
                .ok_or_else(|| anyhow!("missing fields"))?;
            let mut resolved = BTreeSet::new();
            for value in proposed {
                let name = value.as_str().ok_or_else(|| anyhow!("invalid field"))?;
                match registry.resolve(name) {
                    Ok(field) => {
                        resolved.insert(field.id.clone());
                        used.insert(field.id.clone(), field.clone());
                    }
                    Err(reason) => unresolved[unresolved_list]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"field":name,"reason":reason})),
                }
            }
            request[list] = json!(resolved);
        }
        // Different aliases may resolve to the same ID. Preserve the optional
        // qualifier after canonicalization, as the extraction validator does.
        let optional: BTreeSet<_> = request["optional_fields"]
            .as_array()
            .unwrap()
            .iter()
            .cloned()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect();
        request["required_fields"]
            .as_array_mut()
            .unwrap()
            .retain(|value| !optional.contains(value.as_str().unwrap()));
        request["field_resolution"] =
            json!(if unresolved["required"].as_array().unwrap().is_empty()
                && unresolved["optional"].as_array().unwrap().is_empty()
            {
                "resolved"
            } else {
                "unresolved"
            });
        request["unresolved_fields"] = unresolved;
    }
    intent["field_definitions"] = json!(used.into_values().collect::<Vec<_>>());
    intent["field_catalog_version"] = json!(catalog.version);
    response["schema_version"] = json!(2);
    Ok(())
}

#[cfg(test)]
mod tests;
