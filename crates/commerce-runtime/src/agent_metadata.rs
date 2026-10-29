use std::collections::BTreeSet;

use serde_json::{json, Map, Value};

#[derive(Debug, Clone)]
pub struct AgentMetadataOperation {
    pub id: Option<String>,
    pub method: String,
    pub path: String,
    pub price_usd: f64,
    pub description: Option<String>,
    pub input_schema: Option<Value>,
    pub output_schema: Option<Value>,
    pub payment_required: bool,
}

pub struct AgentMetadataContext<'a> {
    pub slug: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub category: Option<&'a str>,
    pub listing_type: Option<&'a str>,
    pub origin_url: &'a str,
    pub docs_url: Option<&'a str>,
    pub raw_metadata: &'a Value,
    pub default_price_usd: Option<f64>,
    pub operations: &'a [AgentMetadataOperation],
}

/// True when a top-level `listing_metadata` key holds an injected upstream
/// credential or signing secret that must NEVER be serialized into a public
/// response (catalog list/detail, `/metadata`, `/execute`).
///
/// The forward/injection path reads these from the RAW database row (see
/// `proxy::handle_flow` -> `extract_preset_auth_headers`/`extract_preset_auth_query`),
/// so stripping them from responses does not affect upstream auth injection.
pub fn is_secret_metadata_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    matches!(
        k.as_str(),
        "auth_headers" | "auth_query" | "api_key" | "apikey"
    ) || k.starts_with("hmac")
        || k.contains("secret")
        || k.contains("password")
        || k.ends_with("_enc")
}

/// Remove injected upstream credentials / signing secrets from a
/// `listing_metadata` object in place.
pub fn redact_secret_metadata(metadata: &mut Map<String, Value>) {
    metadata.retain(|key, _| !is_secret_metadata_key(key));
}

/// Return a copy of `listing_metadata` safe to expose publicly: all injected
/// upstream credentials / signing secrets removed (see [`is_secret_metadata_key`]).
pub fn public_listing_metadata(metadata: &Value) -> Value {
    match metadata {
        Value::Object(map) => {
            let mut cleaned = map.clone();
            redact_secret_metadata(&mut cleaned);
            Value::Object(cleaned)
        }
        other => other.clone(),
    }
}

pub fn normalize_listing_metadata(context: AgentMetadataContext<'_>) -> Value {
    let mut metadata = context
        .raw_metadata
        .as_object()
        .cloned()
        .unwrap_or_default();

    let operations = normalized_operations(&metadata, context.slug, context.operations);
    let model_backed = metadata
        .get("model_backed")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| {
            derive_model_backed(
                context.slug,
                context.name,
                context.description,
                context.category,
            )
        });

    metadata.insert("agent_native".to_string(), json!(true));
    metadata.insert("mcp_native".to_string(), json!(true));
    metadata
        .entry("supported_payment_rails".to_string())
        .or_insert_with(|| json!(["x402"]));
    metadata
        .entry("metadata_path".to_string())
        .or_insert_with(|| json!("/metadata"));
    metadata
        .entry("execute_path".to_string())
        .or_insert_with(|| json!("/execute"));
    metadata.insert("model_backed".to_string(), json!(model_backed));
    metadata
        .entry("implementation_type".to_string())
        .or_insert_with(|| json!(derive_implementation_type(context.category, model_backed)));
    metadata.entry("latency_ms".to_string()).or_insert_with(|| {
        json!(derive_latency_ms(
            context.category,
            operations.as_slice(),
            model_backed,
        ))
    });

    let default_operation = resolve_default_operation(
        operations.as_slice(),
        metadata
            .get("default_operation")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty()),
    );

    let cost_per_call = metadata
        .get("cost_per_call")
        .and_then(Value::as_f64)
        .or(context.default_price_usd)
        .or_else(|| {
            default_operation
                .as_ref()
                .map(|operation| operation.price_usd)
        })
        .or_else(|| operations.first().map(|operation| operation.price_usd))
        .unwrap_or(0.0);
    metadata.insert("cost_per_call".to_string(), json!(cost_per_call));

    if let Some(default_operation) = default_operation.as_ref() {
        metadata.insert(
            "default_operation".to_string(),
            json!(default_operation
                .id
                .clone()
                .unwrap_or_else(|| normalize_path(&default_operation.path))),
        );
    }

    let input_schema = metadata
        .get("input_schema")
        .cloned()
        .unwrap_or_else(default_execute_input_schema);
    let output_schema = metadata
        .get("output_schema")
        .cloned()
        .unwrap_or_else(default_execute_output_schema);
    metadata.insert("input_schema".to_string(), input_schema.clone());
    metadata.insert("output_schema".to_string(), output_schema.clone());

    let mcp_tool_input_schema = metadata
        .get("mcp_tool")
        .and_then(Value::as_object)
        .and_then(|value| value.get("inputSchema"))
        .cloned()
        .or_else(|| {
            if input_schema == default_execute_input_schema() {
                default_operation
                    .as_ref()
                    .and_then(|operation| operation.input_schema.clone())
                    .filter(Value::is_object)
            } else {
                None
            }
        })
        .unwrap_or_else(|| input_schema.clone());

    let tags = merge_tags(
        metadata.get("tags"),
        derive_tags(
            context.category,
            context.listing_type,
            operations.as_slice(),
            model_backed,
        ),
    );
    metadata.insert("tags".to_string(), json!(tags));

    if !metadata.contains_key("operations") && !operations.is_empty() {
        metadata.insert(
            "operations".to_string(),
            Value::Array(operations.iter().map(operation_to_value).collect()),
        );
    }

    metadata.entry("provider".to_string()).or_insert_with(|| {
        json!({
            "type": "http_api",
            "origin_url": context.origin_url,
            "docs_url": context.docs_url,
            "listing_type": context.listing_type,
        })
    });

    metadata.insert(
        "mcp_tool".to_string(),
        json!({
            "name": context.slug,
            "title": context.name,
            "description": context.description,
            "inputSchema": mcp_tool_input_schema,
            "outputSchema": output_schema,
            "annotations": {
                "readOnlyHint": derive_read_only_hint(
                    context.slug,
                    context.name,
                    context.description,
                    operations.as_slice(),
                ),
                "idempotentHint": derive_idempotent_hint(operations.as_slice()),
                "destructiveHint": false,
                "openWorldHint": false,
            }
        }),
    );

    Value::Object(metadata)
}

pub fn default_root_operations(price_usd: f64) -> Vec<AgentMetadataOperation> {
    vec![
        AgentMetadataOperation {
            id: Some("/".to_string()),
            method: "GET".to_string(),
            path: "/".to_string(),
            price_usd,
            description: Some("Forward a GET request to the API root.".to_string()),
            input_schema: None,
            output_schema: None,
            payment_required: true,
        },
        AgentMetadataOperation {
            id: Some("/".to_string()),
            method: "POST".to_string(),
            path: "/".to_string(),
            price_usd,
            description: Some("Forward a POST request to the API root.".to_string()),
            input_schema: None,
            output_schema: None,
            payment_required: true,
        },
    ]
}

fn normalized_operations(
    metadata: &Map<String, Value>,
    slug: &str,
    provided: &[AgentMetadataOperation],
) -> Vec<AgentMetadataOperation> {
    if let Some(existing) = metadata.get("operations").and_then(Value::as_array) {
        let parsed: Vec<AgentMetadataOperation> =
            existing.iter().filter_map(value_to_operation).collect();
        if !parsed.is_empty() {
            return parsed;
        }
    }

    let gateway_base_url = format!("https://gateway.apiosk.com/{}", slug);
    let execute_url = format!("{}/execute", gateway_base_url);

    provided
        .iter()
        .map(|operation| AgentMetadataOperation {
            id: operation
                .id
                .clone()
                .or_else(|| Some(normalize_path(&operation.path))),
            method: operation.method.to_ascii_uppercase(),
            path: normalize_path(&operation.path),
            price_usd: operation.price_usd,
            description: operation.description.clone(),
            input_schema: operation.input_schema.clone(),
            output_schema: operation.output_schema.clone(),
            payment_required: operation.payment_required,
        })
        .map(|operation| {
            let mut value = operation_to_value(&operation);
            if let Some(object) = value.as_object_mut() {
                object.insert(
                    "gateway_url".to_string(),
                    json!(format!("{}{}", gateway_base_url, operation.path)),
                );
                object.insert("execute_url".to_string(), json!(execute_url));
            }
            value_to_operation(&value).expect("generated operation")
        })
        .collect()
}

fn value_to_operation(value: &Value) -> Option<AgentMetadataOperation> {
    let object = value.as_object()?;
    let method = object
        .get("method")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("POST")
        .to_ascii_uppercase();
    let path = object
        .get("path")
        .and_then(Value::as_str)
        .map(normalize_path)
        .unwrap_or_else(|| "/".to_string());

    Some(AgentMetadataOperation {
        id: object
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        method,
        path,
        price_usd: object
            .get("price_usd")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        description: object
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string),
        input_schema: object.get("input_schema").cloned(),
        output_schema: object.get("output_schema").cloned(),
        payment_required: object
            .get("payment_required")
            .and_then(Value::as_bool)
            .unwrap_or(true),
    })
}

fn operation_to_value(operation: &AgentMetadataOperation) -> Value {
    let mut object = Map::new();
    object.insert(
        "id".to_string(),
        json!(operation
            .id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(&operation.path)),
    );
    object.insert(
        "key".to_string(),
        json!(canonical_operation_key(&operation.method, &operation.path)),
    );
    object.insert("method".to_string(), json!(operation.method));
    object.insert("path".to_string(), json!(operation.path));
    object.insert("price_usd".to_string(), json!(operation.price_usd));
    object.insert(
        "payment_required".to_string(),
        json!(operation.payment_required),
    );
    if let Some(description) = operation.description.as_ref() {
        object.insert("description".to_string(), json!(description));
    }
    if let Some(input_schema) = operation.input_schema.as_ref() {
        object.insert("input_schema".to_string(), input_schema.clone());
    }
    if let Some(output_schema) = operation.output_schema.as_ref() {
        object.insert("output_schema".to_string(), output_schema.clone());
    }
    Value::Object(object)
}

fn derive_model_backed(slug: &str, name: &str, description: &str, category: Option<&str>) -> bool {
    if category.unwrap_or_default().eq_ignore_ascii_case("compute") {
        return true;
    }

    let haystack = format!("{slug} {name} {description}").to_ascii_lowercase();
    [
        "llm", "ai", "extract", "research", "intel", "map", "diff", "digest",
    ]
    .iter()
    .any(|token| haystack.contains(token))
}

fn derive_implementation_type(category: Option<&str>, model_backed: bool) -> &'static str {
    if model_backed {
        "hybrid"
    } else if category.unwrap_or_default().eq_ignore_ascii_case("compute") {
        "deterministic"
    } else {
        "direct"
    }
}

fn derive_latency_ms(
    category: Option<&str>,
    operations: &[AgentMetadataOperation],
    model_backed: bool,
) -> u64 {
    if operations
        .iter()
        .any(|operation| operation.path.to_ascii_lowercase().contains("batch"))
    {
        return 1_500;
    }

    let category = category.unwrap_or_default();
    if model_backed || category.eq_ignore_ascii_case("compute") {
        900
    } else if matches!(
        category.to_ascii_lowercase().as_str(),
        "weather" | "finance" | "news" | "payments"
    ) {
        250
    } else {
        400
    }
}

fn derive_default_operation(
    operations: &[AgentMetadataOperation],
) -> Option<AgentMetadataOperation> {
    let mut ranked = operations.to_vec();
    ranked.sort_by_key(default_operation_rank);
    ranked.into_iter().next()
}

fn resolve_default_operation(
    operations: &[AgentMetadataOperation],
    configured_reference: Option<&str>,
) -> Option<AgentMetadataOperation> {
    let configured =
        configured_reference.and_then(|reference| operation_by_reference(operations, reference));
    let preferred = derive_default_operation(operations);

    match (configured, preferred) {
        (Some(configured), Some(preferred))
            if is_diagnostic_operation(configured) && !is_diagnostic_operation(&preferred) =>
        {
            Some(preferred)
        }
        (Some(configured), _) => Some(configured.clone()),
        (None, Some(preferred)) => Some(preferred),
        (None, None) => None,
    }
}

fn operation_by_reference<'a>(
    operations: &'a [AgentMetadataOperation],
    reference: &str,
) -> Option<&'a AgentMetadataOperation> {
    let normalized_reference = reference.trim();
    if normalized_reference.is_empty() {
        return None;
    }

    operations.iter().find(|operation| {
        operation
            .id
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| value.eq_ignore_ascii_case(normalized_reference))
            || operation
                .path
                .eq_ignore_ascii_case(&normalize_path(normalized_reference))
            || canonical_operation_key(&operation.method, &operation.path)
                .eq_ignore_ascii_case(normalized_reference)
    })
}

fn derive_tags(
    category: Option<&str>,
    listing_type: Option<&str>,
    operations: &[AgentMetadataOperation],
    model_backed: bool,
) -> Vec<String> {
    let mut tags = BTreeSet::new();
    tags.insert("agent-ready".to_string());
    tags.insert("mcp-native".to_string());

    if let Some(category) = category.map(str::trim).filter(|value| !value.is_empty()) {
        tags.insert(category.to_ascii_lowercase());
    }

    if let Some(listing_type) = listing_type
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        tags.insert(listing_type.to_ascii_lowercase());
    }

    if model_backed {
        tags.insert("model-backed".to_string());
    }

    if operations
        .iter()
        .any(|operation| matches!(operation.method.as_str(), "GET" | "HEAD"))
    {
        tags.insert("query".to_string());
    }

    if operations
        .iter()
        .any(|operation| operation.method == "POST")
    {
        tags.insert("action".to_string());
    }

    tags.into_iter().collect()
}

fn merge_tags(existing: Option<&Value>, derived: Vec<String>) -> Vec<String> {
    let mut tags = BTreeSet::new();
    if let Some(existing) = existing.and_then(Value::as_array) {
        for tag in existing.iter().filter_map(Value::as_str) {
            let trimmed = tag.trim();
            if !trimmed.is_empty() {
                tags.insert(trimmed.to_string());
            }
        }
    }
    for tag in derived {
        tags.insert(tag);
    }
    tags.into_iter().collect()
}

fn derive_read_only_hint(
    slug: &str,
    name: &str,
    description: &str,
    operations: &[AgentMetadataOperation],
) -> bool {
    if !operations.is_empty()
        && operations
            .iter()
            .all(|operation| matches!(operation.method.as_str(), "GET" | "HEAD"))
    {
        return true;
    }

    let haystack = format!("{slug} {name} {description}").to_ascii_lowercase();
    [
        "extract", "lookup", "watch", "map", "research", "digest", "diff", "monitor", "verify",
        "metadata", "status",
    ]
    .iter()
    .any(|token| haystack.contains(token))
}

fn derive_idempotent_hint(operations: &[AgentMetadataOperation]) -> bool {
    !operations.is_empty()
        && operations
            .iter()
            .all(|operation| matches!(operation.method.as_str(), "GET" | "HEAD"))
}

fn default_operation_rank(
    operation: &AgentMetadataOperation,
) -> (u8, u8, usize, usize, u8, u8, String) {
    let normalized_path = normalize_path(&operation.path);
    let trimmed = normalized_path.trim_matches('/');
    let segment_count = if trimmed.is_empty() {
        0
    } else {
        trimmed
            .split('/')
            .filter(|segment| !segment.is_empty())
            .count()
    };
    let method_rank = match operation.method.as_str() {
        "POST" => 0,
        "PUT" => 1,
        "PATCH" => 2,
        "DELETE" => 3,
        "GET" => 4,
        "HEAD" => 5,
        _ => 6,
    };

    (
        if is_diagnostic_operation(operation) {
            1
        } else {
            0
        },
        if normalized_path.to_ascii_lowercase().contains("batch") {
            1
        } else {
            0
        },
        segment_count,
        normalized_path.len(),
        method_rank,
        if operation.payment_required { 0 } else { 1 },
        canonical_operation_key(&operation.method, &normalized_path),
    )
}

fn is_diagnostic_operation(operation: &AgentMetadataOperation) -> bool {
    is_diagnostic_operation_path(&operation.path)
}

fn is_diagnostic_operation_path(path: &str) -> bool {
    let normalized_path = normalize_path(path);
    if normalized_path == "/" {
        return true;
    }

    normalized_path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .any(|segment| {
            matches!(
                segment.to_ascii_lowercase().as_str(),
                "health"
                    | "status"
                    | "metadata"
                    | "docs"
                    | "doc"
                    | "openapi"
                    | "swagger"
                    | "ping"
                    | "ready"
                    | "readyz"
                    | "live"
                    | "livez"
            )
        })
}

fn normalize_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "/" {
        "/".to_string()
    } else if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

fn canonical_operation_key(method: &str, path: &str) -> String {
    format!("{} {}", method.to_ascii_uppercase(), normalize_path(path))
}

pub fn default_execute_input_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "operation": {
                "type": "string",
                "description": "Optional operation selector. Accepts an operation id, a path such as /extract, or a METHOD /path key."
            },
            "input": {
                "description": "JSON payload forwarded to the selected operation. For GET and HEAD operations, object keys become query parameters unless query is provided explicitly."
            },
            "query": {
                "type": "object",
                "description": "Optional query parameter override for the selected operation.",
                "additionalProperties": true
            },
            "path_params": {
                "type": "object",
                "description": "Optional path parameters used for routes that contain {param} or :param placeholders.",
                "additionalProperties": true
            }
        }
    })
}

pub fn default_execute_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "status": {
                "type": "string",
                "enum": ["success", "error"]
            },
            "result": {},
            "cost": {
                "type": "number"
            },
            "latency": {
                "type": "integer",
                "description": "Gateway-measured upstream latency in milliseconds."
            },
            "operation": {
                "type": "string"
            },
            "api": {
                "type": "string"
            },
            "upstream_status": {
                "type": "integer"
            }
        },
        "required": ["status", "result", "cost", "latency", "operation", "api", "upstream_status"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_listing_metadata_strips_injected_secrets() {
        let raw = json!({
            "cost_per_call": 0.05,
            "auth_headers": [{ "name": "X-Finnhub-Token", "value": "supersecret" }],
            "auth_query": [{ "name": "token", "value": "alsosecret" }],
            "hmac_signing_secret_enc": "enc...",
            "tags": ["finance"],
        });
        let cleaned = public_listing_metadata(&raw);
        let obj = cleaned.as_object().unwrap();
        assert!(!obj.contains_key("auth_headers"));
        assert!(!obj.contains_key("auth_query"));
        assert!(!obj.contains_key("hmac_signing_secret_enc"));
        // Non-secret fields are preserved.
        assert_eq!(obj["cost_per_call"], json!(0.05));
        assert_eq!(obj["tags"], json!(["finance"]));
    }

    #[test]
    fn normalizes_generic_listing_into_mcp_native_shape() {
        let metadata = normalize_listing_metadata(AgentMetadataContext {
            slug: "agent-scout",
            name: "Agent Scout",
            description: "Search Hacker News, GitHub repositories, and npm packages.",
            category: Some("data"),
            listing_type: Some("api"),
            origin_url: "https://example.com/api",
            docs_url: Some("https://example.com/docs"),
            raw_metadata: &json!({}),
            default_price_usd: Some(0.004),
            operations: &[AgentMetadataOperation {
                id: Some("search".to_string()),
                method: "GET".to_string(),
                path: "/search".to_string(),
                price_usd: 0.004,
                description: Some("Search public sources.".to_string()),
                input_schema: None,
                output_schema: None,
                payment_required: true,
            }],
        });

        assert_eq!(metadata["agent_native"], json!(true));
        assert_eq!(metadata["mcp_native"], json!(true));
        assert_eq!(metadata["default_operation"], json!("search"));
        assert_eq!(metadata["cost_per_call"], json!(0.004));
        assert_eq!(metadata["operations"][0]["path"], json!("/search"));
        assert_eq!(metadata["mcp_tool"]["name"], json!("agent-scout"));
        assert!(metadata["tags"]
            .as_array()
            .is_some_and(|tags| tags.iter().any(|tag| tag == "mcp-native")));
    }

    #[test]
    fn keeps_existing_operations_when_present() {
        let metadata = normalize_listing_metadata(AgentMetadataContext {
            slug: "weather",
            name: "Weather",
            description: "Current weather.",
            category: Some("weather"),
            listing_type: Some("api"),
            origin_url: "https://example.com/weather",
            docs_url: None,
            raw_metadata: &json!({
                "operations": [
                    {
                        "id": "lookup",
                        "method": "GET",
                        "path": "/current",
                        "price_usd": 0.003
                    }
                ]
            }),
            default_price_usd: Some(0.003),
            operations: &default_root_operations(0.003),
        });

        assert_eq!(metadata["operations"][0]["id"], json!("lookup"));
        assert_eq!(metadata["default_operation"], json!("lookup"));
    }

    #[test]
    fn default_operation_skips_health_and_prefers_generic_write_route() {
        let metadata = normalize_listing_metadata(AgentMetadataContext {
            slug: "bella-pizza",
            name: "La Bella Pizza API",
            description: "Order fictional pizzas.",
            category: Some("demo"),
            listing_type: Some("api"),
            origin_url: "https://order-pizza-api.fly.dev",
            docs_url: None,
            raw_metadata: &json!({}),
            default_price_usd: Some(5.0),
            operations: &[
                AgentMetadataOperation {
                    id: Some("/".to_string()),
                    method: "GET".to_string(),
                    path: "/".to_string(),
                    price_usd: 0.0,
                    description: None,
                    input_schema: None,
                    output_schema: None,
                    payment_required: false,
                },
                AgentMetadataOperation {
                    id: Some("/health".to_string()),
                    method: "GET".to_string(),
                    path: "/health".to_string(),
                    price_usd: 0.0,
                    description: None,
                    input_schema: None,
                    output_schema: None,
                    payment_required: false,
                },
                AgentMetadataOperation {
                    id: Some("/order/small".to_string()),
                    method: "POST".to_string(),
                    path: "/order/small".to_string(),
                    price_usd: 5.0,
                    description: None,
                    input_schema: Some(json!({
                        "type": "object",
                        "required": ["type"],
                        "properties": {
                            "type": { "type": "string" }
                        }
                    })),
                    output_schema: None,
                    payment_required: true,
                },
                AgentMetadataOperation {
                    id: Some("/orders".to_string()),
                    method: "POST".to_string(),
                    path: "/orders".to_string(),
                    price_usd: 5.0,
                    description: None,
                    input_schema: Some(json!({
                        "type": "object",
                        "required": ["type", "size"],
                        "properties": {
                            "type": { "type": "string" },
                            "size": { "type": "string" }
                        }
                    })),
                    output_schema: None,
                    payment_required: true,
                },
            ],
        });

        assert_eq!(metadata["default_operation"], json!("/orders"));
        assert_eq!(
            metadata["mcp_tool"]["inputSchema"],
            json!({
                "type": "object",
                "required": ["type", "size"],
                "properties": {
                    "type": { "type": "string" },
                    "size": { "type": "string" }
                }
            })
        );
    }
}
