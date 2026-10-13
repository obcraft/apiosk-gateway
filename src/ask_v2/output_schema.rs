//! Shared evidence boundary for Ask discovery and output coverage.
use serde_json::Value;

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
