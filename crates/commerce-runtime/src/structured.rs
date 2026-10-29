//! Local enforcement of the structured-output subset used by the engine.
//! Unsupported schema keywords fail before a model request is sent.
use serde_json::Value;

pub fn check_schema(schema: &Value) -> Result<(), String> {
    fn walk(s: &Value, depth: usize) -> Result<(), String> {
        if depth > 32 {
            return Err("schema too deep".into());
        }
        let o = s.as_object().ok_or("schema must be an object")?;
        for (k, v) in o {
            match k.as_str() {
                "title" | "description" | "default" | "examples" => {}
                "type" => {
                    let valid = |t: &Value| {
                        t.as_str().is_some_and(|t| {
                            matches!(
                                t,
                                "object"
                                    | "array"
                                    | "string"
                                    | "integer"
                                    | "number"
                                    | "boolean"
                                    | "null"
                            )
                        })
                    };
                    if !valid(v)
                        && !v
                            .as_array()
                            .is_some_and(|a| !a.is_empty() && a.iter().all(valid))
                    {
                        return Err("invalid type".into());
                    }
                }
                "properties" => {
                    for child in v.as_object().ok_or("invalid properties")?.values() {
                        walk(child, depth + 1)?;
                    }
                }
                "items" => walk(v, depth + 1)?,
                "anyOf" => {
                    let a = v
                        .as_array()
                        .filter(|a| !a.is_empty())
                        .ok_or("invalid anyOf")?;
                    for child in a {
                        walk(child, depth + 1)?;
                    }
                }
                "required" => {
                    if !v.as_array().is_some_and(|a| a.iter().all(Value::is_string)) {
                        return Err("invalid required".into());
                    }
                }
                "enum" => {
                    if !v.as_array().is_some_and(|a| !a.is_empty()) {
                        return Err("invalid enum".into());
                    }
                }
                "additionalProperties" => {
                    if v != &Value::Bool(false) {
                        return Err("objects must forbid extra properties".into());
                    }
                }
                "minItems" | "maxItems" | "minLength" | "maxLength" => {
                    if v.as_u64().is_none() {
                        return Err(format!("invalid {k}"));
                    }
                }
                "minimum" | "maximum" => {
                    if v.as_f64().is_none() {
                        return Err(format!("invalid {k}"));
                    }
                }
                "pattern" => {
                    regex::Regex::new(v.as_str().ok_or("invalid pattern")?)
                        .map_err(|_| "invalid pattern")?;
                }
                "format" => {
                    if !v.as_str().is_some_and(|s| {
                        matches!(
                            s,
                            "date"
                                | "date-time"
                                | "uri"
                                | "url"
                                | "email"
                                | "uuid"
                                | "ipv4"
                                | "ipv6"
                        )
                    }) {
                        return Err("unsupported format".into());
                    }
                }
                _ => return Err(format!("unsupported schema keyword: {k}")),
            }
        }
        if s.get("type").is_none() && s.get("anyOf").is_none() && s.get("enum").is_none() {
            return Err("unconstrained schema".into());
        }
        if s["type"] == "object"
            || s["type"]
                .as_array()
                .is_some_and(|a| a.contains(&Value::String("object".into())))
        {
            let props = s["properties"]
                .as_object()
                .ok_or("object needs properties")?;
            let required = s["required"].as_array().ok_or("object needs required")?;
            if s["additionalProperties"] != false
                || required.len() != props.len()
                || props
                    .keys()
                    .any(|k| !required.contains(&Value::String(k.clone())))
            {
                return Err("strict objects require every property and forbid extras".into());
            }
        }
        Ok(())
    }
    walk(schema, 0)
}

pub fn validate(schema: &Value, value: &Value) -> Result<(), String> {
    check_schema(schema)?;
    fn check(s: &Value, v: &Value) -> Result<(), String> {
        if let Some(branches) = s["anyOf"].as_array() {
            if !branches.iter().any(|b| check(b, v).is_ok()) {
                return Err("no matching schema branch".into());
            }
        }
        let matches = |t: &str| match t {
            "object" => v.is_object(),
            "array" => v.is_array(),
            "string" => v.is_string(),
            "integer" => v.as_i64().is_some() || v.as_u64().is_some(),
            "number" => v.is_number(),
            "boolean" => v.is_boolean(),
            "null" => v.is_null(),
            _ => false,
        };
        if let Some(t) = s.get("type") {
            if !t.as_str().is_some_and(matches)
                && !t
                    .as_array()
                    .is_some_and(|a| a.iter().any(|t| t.as_str().is_some_and(matches)))
            {
                return Err("wrong value type".into());
            }
        }
        if s["enum"].as_array().is_some_and(|a| !a.contains(v)) {
            return Err("value outside enum".into());
        }
        if let Some(o) = v.as_object() {
            if let Some(required) = s["required"].as_array() {
                if required
                    .iter()
                    .any(|k| !o.contains_key(k.as_str().unwrap()))
                {
                    return Err("missing property".into());
                }
            }
            for (k, child) in o {
                if let Some(sub) = s["properties"].get(k) {
                    check(sub, child)?;
                } else if s["additionalProperties"] == false {
                    return Err("unexpected property".into());
                }
            }
        }
        let within = |n: u64, min: &str, max: &str| {
            !s[min].as_u64().is_some_and(|m| n < m) && !s[max].as_u64().is_some_and(|m| n > m)
        };
        if let Some(a) = v.as_array() {
            if !within(a.len() as u64, "minItems", "maxItems") {
                return Err("array length outside bounds".into());
            }
            if let Some(items) = s.get("items") {
                for item in a {
                    check(items, item)?;
                }
            }
        }
        if let Some(n) = v.as_f64() {
            if s["minimum"].as_f64().is_some_and(|m| n < m)
                || s["maximum"].as_f64().is_some_and(|m| n > m)
            {
                return Err("number outside bounds".into());
            }
        }
        if let Some(t) = v.as_str() {
            if !within(t.chars().count() as u64, "minLength", "maxLength") {
                return Err("string length outside bounds".into());
            }
            if let Some(p) = s["pattern"].as_str() {
                if !regex::Regex::new(p)
                    .map_err(|_| "invalid pattern")?
                    .is_match(t)
                {
                    return Err("pattern mismatch".into());
                }
            }
            if let Some(f) = s["format"].as_str() {
                let valid = match f {
                    "date" => chrono::NaiveDate::parse_from_str(t, "%Y-%m-%d").is_ok(),
                    "date-time" => chrono::DateTime::parse_from_rfc3339(t).is_ok(),
                    "uri" | "url" => url::Url::parse(t).is_ok(),
                    "uuid" => uuid::Uuid::parse_str(t).is_ok(),
                    "ipv4" => t.parse::<std::net::Ipv4Addr>().is_ok(),
                    "ipv6" => t.parse::<std::net::Ipv6Addr>().is_ok(),
                    "email" => t.split_once('@').is_some_and(|(a, b)| {
                        !a.is_empty()
                            && b.contains('.')
                            && !t.chars().any(char::is_whitespace)
                            && !b.contains('@')
                    }),
                    _ => false,
                };
                if !valid {
                    return Err("format mismatch".into());
                }
            }
        }
        Ok(())
    }
    check(schema, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn nullable_is_present_and_extra_keys_are_rejected() {
        let s = json!({"type":"object","required":["id"],"additionalProperties":false,"properties":{"id":{"anyOf":[{"type":"string","pattern":"^[0-9]{8}$"},{"type":"null"}]}}});
        for v in [json!({"id":null}), json!({"id":"30204462"})] {
            assert!(validate(&s, &v).is_ok());
        }
        for v in [
            json!({}),
            json!({"id":12}),
            json!({"id":"Mollie"}),
            json!({"id":null,"pay":true}),
        ] {
            assert!(validate(&s, &v).is_err());
        }
    }
    #[test]
    fn bounds_and_union_types_are_enforced() {
        let s =
            json!({"type":"array","maxItems":2,"items":{"type":["integer","null"],"minimum":1}});
        assert!(validate(&s, &json!([1, null])).is_ok());
        for v in [json!([1, 2, 3]), json!([0]), json!([1.5]), json!(["1"])] {
            assert!(validate(&s, &v).is_err());
        }
    }
    #[test]
    fn unsupported_and_malformed_constraints_fail_before_use() {
        for s in [
            json!({"$ref":"https://example.com"}),
            json!({"type":"banana"}),
            json!({"maximum":"10"}),
            json!({"type":"object","properties":{}}),
        ] {
            assert!(check_schema(&s).is_err());
        }
    }
}
