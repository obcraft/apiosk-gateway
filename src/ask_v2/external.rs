//! Admission for a selected public Bazaar offer. Only the configured executor's
//! settled source directory is read here; the selected resource is never fetched.
//! Payment remains in the existing selection/balance/treasury executor.
use bigdecimal::BigDecimal;
use catalog_core::facts::schema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::{collections::BTreeMap, str::FromStr, time::Duration};

pub const SOURCE: &str = "coinbase-x402-bazaar";

/// Opaque selection identity, not a trusted offer. Execution always resolves
/// it again against the server's directory and never accepts a client price.
pub fn selection_id(resource: &str, method: &str, query: &str) -> String {
    use base64::Engine;
    format!("bazaar:{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&(resource, method, query)).unwrap()))
}

pub fn selection(value: &str) -> Option<(String, String, String)> {
    use base64::Engine;
    if value.len() > 8192 { return None; }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(value.strip_prefix("bazaar:")?).ok()?;
    let (resource, method, query): (String, String, String) = serde_json::from_slice(&bytes).ok()?;
    (resource.len() <= 4096 && query.len() <= 500 && matches!(method.as_str(), "GET" | "POST" | "PUT" | "PATCH" | "DELETE"))
        .then_some((resource, method, query))
}
const MAX_DIRECTORY_BYTES: usize = 2 * 1024 * 1024;
const MAX_SCHEMA_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Input {
    /// Unique form key. The wire name is separate: dots in it are literal.
    pub field: String,
    pub name: String,
    pub location: String,
    pub required: bool,
    pub schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Offer {
    pub resource: String,
    pub method: String,
    pub name: String,
    pub description: String,
    pub source: String,
    pub host: String,
    pub network: Option<String>,
    pub provider_atomic: i64,
    pub total_atomic: i64,
    pub inputs: Vec<Input>,
    /// Canonical flat schema, keyed exactly as `inputs[].field`.
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundInput {
    pub inputs: Value,
    pub input_parts: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct Refusal {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, String>,
}

fn refuse(code: &str, message: &str) -> Refusal {
    Refusal {
        code: code.into(),
        message: message.into(),
        fields: BTreeMap::new(),
    }
}

fn resource(value: &str) -> Result<url::Url, Refusal> {
    let parsed = url::Url::parse(value).map_err(|_| {
        refuse(
            "external_offer_invalid",
            "This source has no valid HTTPS endpoint.",
        )
    })?;
    let host = parsed.host_str().unwrap_or("");
    if value.len() > 2048
        || parsed.scheme() != "https"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
        || parsed.port().is_some_and(|port| port != 443)
        || host.is_empty()
        || !host.contains('.')
        || host.parse::<std::net::IpAddr>().is_ok()
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host == "metadata.google.internal"
        || host.ends_with(".internal")
    {
        return Err(refuse(
            "external_offer_invalid",
            "This source has no supported public HTTPS endpoint.",
        ));
    }
    Ok(parsed)
}

fn method(value: &str) -> Result<String, Refusal> {
    let method = value.trim().to_ascii_uppercase();
    if !matches!(method.as_str(), "GET" | "POST" | "PUT" | "PATCH") {
        return Err(refuse(
            "external_method_unsupported",
            "This source's request method is not supported.",
        ));
    }
    Ok(method)
}

/// Read one fixed directory route on the configured embedded executor. The
/// caller provides a scrubbed capability query, never a question or identifier.
pub async fn resolve(
    http: &reqwest::Client,
    directory_origin: &str,
    selected_resource: &str,
    selected_method: &str,
    generic_query: &str,
) -> Result<Offer, Refusal> {
    let target = resource(selected_resource)?;
    let selected_method = method(selected_method)?;
    let mut directory = url::Url::parse(directory_origin).map_err(|_| {
        refuse(
            "external_directory_unavailable",
            "Source pricing is temporarily unavailable.",
        )
    })?;
    if directory.scheme() != "http"
        || !matches!(
            directory.host_str(),
            Some("127.0.0.1" | "localhost" | "[::1]")
        )
        || !directory.username().is_empty()
        || directory.password().is_some()
        || directory.query().is_some()
        || directory.fragment().is_some()
    {
        return Err(refuse(
            "external_directory_unavailable",
            "Source pricing is temporarily unavailable.",
        ));
    }
    if generic_query.trim().is_empty() || generic_query.chars().count() > 300 {
        return Err(refuse(
            "external_query_invalid",
            "Search for this capability again before selecting the source.",
        ));
    }
    directory.set_path("/");
    directory
        .path_segments_mut()
        .map_err(|_| {
            refuse(
                "external_directory_unavailable",
                "Source pricing is temporarily unavailable.",
            )
        })?
        .clear()
        .extend(["v1", "sources", SOURCE, "apis", target.host_str().unwrap()]);
    directory.query_pairs_mut().append_pair("q", generic_query);
    let response = http
        .get(directory)
        .timeout(Duration::from_secs(6))
        .send()
        .await
        .map_err(|_| {
            refuse(
                "external_directory_unavailable",
                "This source could not be priced. Your selection is kept; try again.",
            )
        })?;
    let body = crate::parser::bounded_json_sized(response, MAX_DIRECTORY_BYTES)
        .await
        .map_err(|_| {
            refuse(
                "external_directory_unavailable",
                "This source could not be priced. Your selection is kept; try again.",
            )
        })?
        .0;
    admit(&body, selected_resource, &selected_method)
}

/// `directory` must be the server-read settled source response, never request
/// JSON. No client price, input contract or settlement claim is accepted.
pub fn admit(
    directory: &Value,
    selected_resource: &str,
    selected_method: &str,
) -> Result<Offer, Refusal> {
    let selected_resource = resource(selected_resource)?;
    let selected_method = method(selected_method)?;
    if directory.pointer("/source/id").and_then(Value::as_str) != Some(SOURCE) {
        return Err(refuse(
            "external_source_unverified",
            "This source listing could not be confirmed. Search again.",
        ));
    }
    let rows = directory["endpoints"].as_array().ok_or_else(|| {
        refuse(
            "external_source_unverified",
            "This source listing could not be confirmed. Search again.",
        )
    })?;
    let matches: Vec<_> = rows
        .iter()
        .filter(|row| {
            row["resource"]
                .as_str()
                .and_then(|raw| resource(raw).ok())
                .as_ref()
                == Some(&selected_resource)
                && row["method"]
                    .as_str()
                    .unwrap_or("GET")
                    .trim()
                    .eq_ignore_ascii_case(&selected_method)
        })
        .collect();
    if matches.len() != 1 {
        return Err(refuse(
            "external_source_unverified",
            "The selected endpoint is no longer listed unambiguously. Search again.",
        ));
    }
    let row = matches[0];
    if row["settlement"] != "apiosk" {
        return Err(refuse("external_not_payable", "Apiosk cannot currently purchase this source from your balance. Choose another source."));
    }
    let network = row["network"].as_str().map(str::to_owned);
    if network.as_deref().is_some_and(|network| {
        !matches!(
            network.trim().to_ascii_lowercase().as_str(),
            "base" | "eip155:8453" | "8453" | ""
        )
    }) {
        return Err(refuse(
            "external_rail_unsupported",
            "This source does not use a payment network Apiosk can settle.",
        ));
    }
    let provider_atomic = atomic(&row["price_usd"])?;
    // The legacy directory's displayed buyer price can lag the current fee
    // policy. Only its provider price is authoritative here; the current
    // balance executor and this quote both apply catalog-core's buyer markup.
    let total_atomic = i64::try_from(
        catalog_core::fees::quote(
            provider_atomic.into(),
            catalog_core::fees::ProviderClass::Federated,
        )
        .buyer_total_atomic,
    )
    .map_err(|_| {
        refuse(
            "external_price_unverified",
            "This source's total price could not be confirmed.",
        )
    })?;
    // A lower legacy markup is safe: the Edge ledger charges its current
    // policy. A higher gateway total would exceed this approval's cap.
    if atomic(&row["buyer_price_usd"])? > total_atomic {
        return Err(refuse(
            "external_price_changed",
            "This source's payment quote changed. Refresh its price before approval.",
        ));
    }
    let (inputs, input_schema) =
        fields(&row["input_schema"], &selected_method, &selected_resource)?;
    Ok(Offer {
        resource: selected_resource.to_string(),
        method: selected_method,
        name: row["provider"]
            .as_str()
            .filter(|name| !name.trim().is_empty() && !name.eq_ignore_ascii_case("Coinbase Bazaar"))
            .unwrap_or(selected_resource.host_str().unwrap())
            .chars()
            .take(160)
            .collect(),
        description: row["description"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(1000)
            .collect(),
        source: SOURCE.into(),
        host: selected_resource.host_str().unwrap().into(),
        network,
        provider_atomic,
        total_atomic,
        inputs,
        input_schema,
    })
}

fn atomic(value: &Value) -> Result<i64, Refusal> {
    let text = match value {
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        _ => String::new(),
    };
    let decimal = BigDecimal::from_str(&text).map_err(|_| {
        refuse(
            "external_price_unverified",
            "This source has no confirmed price.",
        )
    })? * BigDecimal::from(1_000_000);
    let integer = decimal.with_scale(0);
    let amount = integer.to_string().parse::<i64>().ok();
    if decimal != integer || amount.is_none_or(|amount| !(1..1_000_000_000_000).contains(&amount)) {
        return Err(refuse(
            "external_price_unverified",
            "This source has no supported exact price.",
        ));
    }
    Ok(amount.unwrap())
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 120
        && !matches!(name, "__proto__" | "constructor" | "prototype")
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.:$[]-".contains(&c))
}

fn schema_error() -> Refusal {
    refuse("external_inputs_unsupported", "This source's published input contract cannot yet be checked reliably. Choose another source.")
}

// Only annotation-only keywords are removed. Unsupported constraints fail
// admission rather than being silently weakened before a paid request.
fn checked_schema(value: &Value, depth: usize) -> Result<Value, Refusal> {
    if depth > 6 {
        return Err(schema_error());
    }
    let mut value = value.as_object().cloned().ok_or_else(schema_error)?;
    for annotation in [
        "$schema",
        "$id",
        "format",
        "example",
        "deprecated",
        "readOnly",
        "writeOnly",
    ] {
        value.remove(annotation);
    }
    if let Some(constant) = value.remove("const") {
        if value.get("enum").is_some_and(|items| !items.is_array()) {
            return Err(schema_error());
        }
        if value
            .get("enum")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.contains(&constant))
        {
            return Err(schema_error());
        }
        value.insert("enum".into(), json!([constant]));
    }
    if let Some(kind) = value.get("type") {
        if !kind.as_str().is_some_and(|kind| {
            matches!(
                kind,
                "object" | "array" | "string" | "number" | "integer" | "boolean" | "null"
            )
        }) {
            return Err(schema_error());
        }
    } else if !value.contains_key("enum") {
        return Err(schema_error());
    }
    if let Some(properties) = value.get_mut("properties") {
        let properties = properties.as_object_mut().ok_or_else(schema_error)?;
        if properties.len() > 64 {
            return Err(schema_error());
        }
        for (name, child) in properties {
            if !valid_name(name) {
                return Err(schema_error());
            }
            *child = checked_schema(child, depth + 1)?;
        }
    }
    if let Some(items) = value.get_mut("items") {
        *items = checked_schema(items, depth + 1)?;
    }
    if value
        .get("additionalProperties")
        .is_some_and(|v| !v.is_boolean())
        || value
            .get("enum")
            .is_some_and(|v| !v.as_array().is_some_and(|v| !v.is_empty()))
        || ["minimum", "maximum"]
            .iter()
            .any(|key| value.get(*key).is_some_and(|v| v.as_f64().is_none()))
        || ["minLength", "maxLength"]
            .iter()
            .any(|key| value.get(*key).is_some_and(|v| v.as_u64().is_none()))
    {
        return Err(schema_error());
    }
    if let Some(required) = value.get("required") {
        let required = required.as_array().ok_or_else(schema_error)?;
        let names = value
            .get("properties")
            .and_then(Value::as_object)
            .ok_or_else(schema_error)?;
        if required
            .iter()
            .any(|name| name.as_str().is_none_or(|name| !names.contains_key(name)))
        {
            return Err(schema_error());
        }
    }
    let value = Value::Object(value);
    schema::check_schema(&value).map_err(|_| schema_error())?;
    Ok(value)
}

fn fields(
    value: &Value,
    method: &str,
    resource: &url::Url,
) -> Result<(Vec<Input>, Value), Refusal> {
    if value.is_null() {
        return Err(refuse("external_inputs_missing", "This source has not published an input schema. Choose a source with documented inputs."));
    }
    if serde_json::to_vec(value).map_or(true, |bytes| bytes.len() > MAX_SCHEMA_BYTES) {
        return Err(schema_error());
    }
    let checked = checked_schema(value, 0)?;
    let value = &checked;
    if value["type"] != "object" {
        return Err(schema_error());
    }
    let properties = value["properties"].as_object().ok_or_else(schema_error)?;
    let grouped = properties.keys().any(|key| {
        matches!(
            key.as_str(),
            "pathParams" | "queryParams" | "body" | "bodyFields"
        )
    });
    let mut groups = Vec::new();
    if grouped {
        // Transport metadata is not forwarded as a provider argument.
        for (name, value) in properties {
            let location = match name.as_str() {
                "pathParams" => "path",
                "queryParams" => "query",
                "body" | "bodyFields" => "body",
                "method" => {
                    let checked = checked_schema(value, 0)?;
                    schema::validate(&checked, &json!(method)).map_err(|_| schema_error())?;
                    continue;
                }
                "type" => {
                    schema::validate(value, &json!("http")).map_err(|_| schema_error())?;
                    continue;
                }
                _ => return Err(schema_error()),
            };
            groups.push((location, checked_schema(value, 0)?));
        }
        // Root constraints outside the known group declaration must not vanish.
        for key in value.as_object().ok_or_else(schema_error)?.keys() {
            if ![
                "type",
                "properties",
                "required",
                "additionalProperties",
                "$schema",
                "$id",
                "title",
                "description",
            ]
            .contains(&key.as_str())
            {
                return Err(schema_error());
            }
        }
    } else {
        groups.push((
            if method == "GET" { "query" } else { "body" },
            checked_schema(value, 0)?,
        ));
    }
    let mut inputs = Vec::new();
    let mut flat = Map::new();
    let mut required = Vec::new();
    for (location, group) in groups {
        if group["type"] != "object" {
            return Err(schema_error());
        }
        let names = group["properties"].as_object().ok_or_else(schema_error)?;
        let required_names = group["required"].as_array().cloned().unwrap_or_default();
        if required_names
            .iter()
            .any(|name| name.as_str().is_none_or(|name| !names.contains_key(name)))
        {
            return Err(schema_error());
        }
        for (name, definition) in names {
            if !valid_name(name) {
                return Err(schema_error());
            }
            let field = format!("{location}.{name}");
            if flat.contains_key(&field) {
                return Err(schema_error());
            }
            let is_required =
                location == "path" || required_names.iter().any(|value| value == name);
            if is_required {
                required.push(field.clone());
            }
            flat.insert(field.clone(), definition.clone());
            inputs.push(Input {
                field,
                name: name.clone(),
                location: location.into(),
                required: is_required,
                schema: definition.clone(),
            });
        }
    }
    if inputs.len() > 64 {
        return Err(schema_error());
    }
    // Every URL placeholder must have a declared path field, and every path
    // field must bind a placeholder. No leftover value can become a query.
    let path = resource.path().replace("%7B", "{").replace("%7D", "}");
    let mut tokens = Vec::new();
    for segment in path.split('/') {
        if let Some(name) = segment
            .strip_prefix('{')
            .and_then(|name| name.strip_suffix('}'))
            .or_else(|| segment.strip_prefix(':'))
        {
            if !valid_name(name) {
                return Err(schema_error());
            }
            tokens.push(name.to_owned());
        } else if segment.contains(['{', '}']) {
            return Err(schema_error());
        }
    }
    if tokens.iter().any(|token| {
        !inputs
            .iter()
            .any(|field| field.location == "path" && field.name == *token)
    }) || inputs
        .iter()
        .any(|field| field.location == "path" && !tokens.contains(&field.name))
    {
        return Err(schema_error());
    }
    let flat =
        json!({"type":"object","properties":flat,"required":required,"additionalProperties":false});
    schema::check_schema(&flat).map_err(|_| schema_error())?;
    Ok((inputs, flat))
}

/// Validate the values before a quote is persisted or any balance is reserved.
/// Callers persist both outputs and reuse them unchanged on approval/recovery.
pub fn validate_input(offer: &Offer, values: &Value) -> Result<BoundInput, Refusal> {
    let object = values
        .as_object()
        .ok_or_else(|| refuse("invalid_input", "Enter the source's requested inputs."))?;
    if serde_json::to_vec(values).map_or(true, |bytes| bytes.len() > 64 * 1024) {
        return Err(refuse("invalid_input", "These inputs are too large."));
    }
    let mut invalid = BTreeMap::new();
    for key in object.keys() {
        if !offer.inputs.iter().any(|field| field.field == *key) {
            invalid.insert(
                key.clone(),
                "This field is not declared by the source.".into(),
            );
        }
    }
    let mut input_parts = json!({"path":{},"query":{},"body":{}});
    for field in &offer.inputs {
        match object.get(&field.field) {
            None if field.required => {
                invalid.insert(field.field.clone(), "This input is required.".into());
            }
            None => {}
            Some(value) => match schema::validate(&field.schema, value) {
                Err(error) => {
                    invalid.insert(field.field.clone(), error.to_string());
                }
                Ok(()) => {
                    input_parts[&field.location][&field.name] = value.clone();
                }
            },
        }
    }
    if !invalid.is_empty() {
        return Err(Refusal {
            code: "invalid_input".into(),
            message: "Check the highlighted source inputs before approving.".into(),
            fields: invalid,
        });
    }
    schema::validate(&offer.input_schema, values).map_err(|_| {
        refuse(
            "invalid_input",
            "These inputs do not match the source's contract.",
        )
    })?;
    Ok(BoundInput {
        inputs: values.clone(),
        input_parts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn directory() -> Value {
        json!({"source":{"id":SOURCE},"endpoints":[{
            "resource":"https://registry.example.test/companies/{number}/officers","method":"GET",
            "provider":"Registry","description":"Company officers","network":"eip155:8453",
            "settlement":"apiosk","price_usd":0.1,"buyer_price_usd":0.115,
            "input_schema":{"type":"object","properties":{
                "method":{"type":"string","const":"GET"},
                "pathParams":{"type":"object","properties":{"number":{"type":"string","pattern":"^[0-9]{8}$"}},"required":["number"]},
                "queryParams":{"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":50}},"required":[]}
            }}
        }]})
    }
    fn admit_fixture(value: &Value) -> Result<Offer, Refusal> {
        admit(
            value,
            "https://registry.example.test/companies/{number}/officers",
            "GET",
        )
    }
    #[test]
    fn source_pricing_is_integer_and_separate_from_the_buyer_total() {
        let offer = admit_fixture(&directory()).unwrap();
        assert_eq!(
            (offer.provider_atomic, offer.total_atomic),
            (100_000, 115_000)
        );
        assert_eq!(offer.inputs.len(), 2);
        let bound =
            validate_input(&offer, &json!({"path.number":"09446231","query.limit":10})).unwrap();
        assert_eq!(
            bound.input_parts,
            json!({"path":{"number":"09446231"},"query":{"limit":10},"body":{}})
        );
        let restored: Offer = serde_json::from_value(serde_json::to_value(offer).unwrap()).unwrap();
        assert_eq!(restored.provider_atomic, 100_000);
    }
    #[test]
    fn exact_source_resource_method_and_payability_are_required() {
        for (pointer, value, expected) in [
            ("/source/id", json!("unknown"), "external_source_unverified"),
            (
                "/endpoints/0/resource",
                json!("https://other.example.test/companies/{number}/officers"),
                "external_source_unverified",
            ),
            (
                "/endpoints/0/method",
                json!("POST"),
                "external_source_unverified",
            ),
            (
                "/endpoints/0/settlement",
                json!("direct"),
                "external_not_payable",
            ),
            (
                "/endpoints/0/network",
                json!("solana"),
                "external_rail_unsupported",
            ),
            (
                "/endpoints/0/price_usd",
                json!(0),
                "external_price_unverified",
            ),
            (
                "/endpoints/0/buyer_price_usd",
                json!(0.12),
                "external_price_changed",
            ),
        ] {
            let mut body = directory();
            *body.pointer_mut(pointer).unwrap() = value;
            assert_eq!(admit_fixture(&body).unwrap_err().code, expected);
        }
    }
    #[test]
    fn malformed_or_missing_contract_never_becomes_a_paid_guess() {
        for contract in [
            Value::Null,
            json!({"type":"object","properties":{"q":{"$ref":"https://schema.example.test"}}}),
            json!({"type":"object","properties":{"q":{"type":["string","null"]}}}),
            json!({"type":"object","properties":{"queryParams":{"type":"object","properties":{"q":{"type":"string"}},"required":[5]}}}),
            json!({"type":"object","properties":{"queryParams":{"type":"object","properties":{"q":{"type":"string","maxLength":"10"}}}}}),
        ] {
            let mut body = directory();
            body["endpoints"][0]["input_schema"] = contract;
            assert!(admit_fixture(&body).is_err());
        }
    }
    #[test]
    fn public_sirenic_and_ukintel_contracts_admit_with_current_server_fee() {
        // Public, zero-payment directory GET snapshots from 2026-09-27. The
        // legacy display still carried 10%; balance pricing has used 15%.
        let fixtures: Vec<Value> = serde_json::from_str(include_str!(
            "../../tests/fixtures/ask-v2-external-contracts.json"
        ))
        .unwrap();
        let mut admitted = 0;
        for directory in fixtures {
            for row in directory["endpoints"].as_array().unwrap() {
                let offer = admit(&directory, row["resource"].as_str().unwrap(), "GET").unwrap();
                assert_ne!(offer.name, "Coinbase Bazaar");
                assert_eq!(offer.total_atomic, offer.provider_atomic * 115 / 100);
                assert!(offer.total_atomic > atomic(&row["buyer_price_usd"]).unwrap());
                let values = if offer.resource.ends_with("/dirigeants") {
                    json!({"path.company_number":"09446231"})
                } else if offer.resource.ends_with("/search") {
                    json!({"query.q":"Monzo","query.limit":5})
                } else {
                    json!({"path.number":"09446231","query.detail":"full"})
                };
                assert!(validate_input(&offer, &values).is_ok());
                assert!(validate_input(&offer, &json!({})).is_err());
                admitted += 1;
            }
        }
        assert_eq!(admitted, 3);
    }
    #[test]
    fn invalid_missing_and_undeclared_values_produce_field_feedback() {
        let offer = admit_fixture(&directory()).unwrap();
        for values in [
            json!({}),
            json!({"path.number":"Monzo"}),
            json!({"path.number":"09446231","query.limit":51}),
            json!({"path.number":"09446231","query.extra":true}),
        ] {
            let error = validate_input(&offer, &values).unwrap_err();
            assert_eq!(error.code, "invalid_input");
            assert!(!error.fields.is_empty());
        }
    }
    #[test]
    fn flat_post_schema_routes_only_declared_fields_to_body() {
        let mut body = directory();
        body["endpoints"][0]["resource"] = json!("https://registry.example.test/search");
        body["endpoints"][0]["method"] = json!("POST");
        body["endpoints"][0]["input_schema"] = json!({"type":"object","properties":{"name":{"type":"string","minLength":1}},"required":["name"]});
        let offer = admit(&body, "https://registry.example.test/search", "POST").unwrap();
        let bound = validate_input(&offer, &json!({"body.name":"Monzo"})).unwrap();
        assert_eq!(
            bound.input_parts,
            json!({"path":{},"query":{},"body":{"name":"Monzo"}})
        );
    }
    #[test]
    fn url_and_price_bounds_are_enforced_without_resource_requests() {
        for url in [
            "http://example.test",
            "https://127.0.0.1/a",
            "https://user:password@example.test/a",
            "https://metadata.google.internal/a",
            "https://example.test/a#fragment",
        ] {
            assert!(resource(url).is_err());
        }
        assert_eq!(atomic(&json!(0.0029)).unwrap(), 2900);
        assert_eq!(atomic(&json!("1e-6")).unwrap(), 1);
        assert!(atomic(&json!(0.0000001)).is_err());
    }
}
