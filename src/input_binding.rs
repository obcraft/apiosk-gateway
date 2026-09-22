//! Bind catalog-authored body templates without asking the intent model to
//! manufacture nested provider JSON. Only explicit facts fill the leaves.
use anyhow::{ensure, Context};
use catalog_core::facts::contract::InputBinding;
use serde_json::{Map, Value};

pub fn insert(
    inputs: &mut Map<String, Value>,
    binding: &InputBinding,
    value: Value,
) -> anyhow::Result<()> {
    let value = match binding.wrap.as_deref() {
        None => value,
        Some("array") if binding.location.as_deref() == Some("body") => Value::Array(vec![value]),
        Some("uk_vat_search") if binding.location.as_deref() == Some("body") => {
            if value.is_null() { value } else {
                let name = value.as_str().context("VAT search needs a company name")?;
                ensure!(!name.trim().is_empty() && name.len() <= 200, "Invalid VAT search company");
                Value::String(format!("{name} UK company VAT registration number GB"))
            }
        },
        _ => anyhow::bail!("unsupported input wrapping"),
    };
    if !binding.name.starts_with('/') {
        ensure!(
            !inputs.contains_key(&binding.name),
            "input overwrites a fixed parameter"
        );
        inputs.insert(binding.name.clone(), value);
        return Ok(());
    }
    ensure!(
        binding.location.as_deref() == Some("body"),
        "nested binding must target the body"
    );
    // Parents must already exist in the reviewed fixed_params template. Never
    // interpret a caller value as a field name, array index or URL segment.
    let parts: Vec<_> = binding.name[1..].split('/').collect();
    ensure!(parts.len() >= 2 && parts.len() <= 8 && parts.iter().all(|p|
        !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')),
        "invalid nested body binding");
    let mut parent = inputs.get_mut(parts[0]).context("missing body template")?;
    for part in &parts[1..parts.len() - 1] {
        parent = parent
            .as_object_mut()
            .context("body template parent must be an object")?
            .get_mut(*part)
            .context("missing body template parent")?;
    }
    let object = parent
        .as_object_mut()
        .context("body template parent must be an object")?;
    let leaf = parts.last().unwrap();
    ensure!(
        !object.contains_key(*leaf),
        "input overwrites a fixed body value"
    );
    object.insert((*leaf).into(), value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn binding(name: &str, wrap: Option<&str>) -> InputBinding {
        InputBinding {
            location: Some("body".into()),
            name: name.into(),
            wrap: wrap.map(str::to_owned),
            default: None,
        }
    }
    #[test]
    fn binds_literal_search_filters_and_company_screening() {
        let mut search = Map::new();
        insert(
            &mut search,
            &binding("headquarters_locations", Some("array")),
            json!("Amsterdam"),
        )
        .unwrap();
        insert(&mut search, &binding("headcount_min", None), json!(50)).unwrap();
        assert_eq!(
            Value::Object(search),
            json!({"headquarters_locations":["Amsterdam"],"headcount_min":50})
        );
        let mut body =
            json!({"dataset":"default","queries":{"q1":{"schema":"Company","properties":{}}}})
                .as_object()
                .unwrap()
                .clone();
        insert(
            &mut body,
            &binding("/queries/q1/properties/name", Some("array")),
            json!("Example BV"),
        )
        .unwrap();
        assert_eq!(
            Value::Object(body),
            json!({"dataset":"default","queries":{"q1":{"schema":"Company","properties":{"name":["Example BV"]}}}})
        );
    }
    #[test]
    fn cannot_overwrite_constants_or_create_unreviewed_template_paths() {
        let base =
            json!({"dataset":"default","queries":{"q1":{"schema":"Company","properties":{}}}})
                .as_object()
                .unwrap()
                .clone();
        for name in [
            "dataset",
            "/queries/q1/schema",
            "/queries/q2/name",
            "/queries/../name",
            "/queries/q1/properties/~1name",
        ] {
            assert!(
                insert(
                    &mut base.clone(),
                    &binding(name, None),
                    json!("replacement")
                )
                .is_err(),
                "{name}"
            );
        }
        let mut query = binding("/queries/q1/properties/name", None);
        query.location = Some("query".into());
        assert!(insert(&mut base.clone(), &query, json!("Example")).is_err());
    }
}
