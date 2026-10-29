use super::*;

pub(super) async fn resolve_virtual_execute_request(
    request: Request<Body>,
    agent_surface: &AgentSurfaceContext,
    requested_path: &str,
) -> Result<VirtualExecuteRequest, Response<Body>> {
    let body_bytes = axum::body::to_bytes(request.into_body(), usize::MAX)
        .await
        .map_err(|err| {
            error!("Failed to read /execute body: {}", err);
            json_error_response(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "Failed to read /execute request body.",
            )
        })?;

    let payload = serde_json::from_slice::<serde_json::Value>(&body_bytes).map_err(|_| {
        json_error_response(
            StatusCode::BAD_REQUEST,
            "invalid_json",
            "POST /execute expects a JSON object body.",
        )
    })?;

    let payload_object = payload.as_object().ok_or_else(|| {
        json_error_response(
            StatusCode::BAD_REQUEST,
            "validation_error",
            "POST /execute expects a JSON object body.",
        )
    })?;

    let uses_envelope = uses_execute_envelope(payload_object);

    let input = if uses_envelope {
        payload_object
            .get("input")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}))
    } else {
        payload.clone()
    };

    let operation_reference = payload_object
        .get("operation")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let operation = operation_reference
        .and_then(|reference| find_agent_operation(&agent_surface.operations, reference))
        .or_else(|| agent_surface.default_operation.clone())
        .ok_or_else(|| {
            json_error_response(
                StatusCode::NOT_FOUND,
                "operation_not_found",
                "No callable operation is configured for this API.",
            )
        })?;

    let upstream_method = method_from_string(&operation.method).map_err(|_| {
        json_error_response(
            StatusCode::BAD_GATEWAY,
            "invalid_operation_method",
            "The selected operation uses an unsupported HTTP method.",
        )
    })?;

    let path_params = payload_object
        .get("path_params")
        .or_else(|| payload_object.get("pathParams"))
        .filter(|value| value.is_object())
        .cloned()
        .or_else(|| input.as_object().map(|_| input.clone()));
    let upstream_path =
        substitute_operation_path(&operation.path, path_params.as_ref()).map_err(|message| {
            json_error_response(StatusCode::BAD_REQUEST, "validation_error", &message)
        })?;
    let explicit_query = if uses_envelope {
        payload_object.get("query")
    } else {
        None
    };
    let upstream_query =
        build_execute_query(&operation, explicit_query, &input).map_err(|message| {
            json_error_response(StatusCode::BAD_REQUEST, "validation_error", &message)
        })?;

    let upstream_body_bytes = if matches!(upstream_method, Method::GET | Method::HEAD) {
        vec![]
    } else {
        serde_json::to_vec(&input).map_err(|err| {
            error!(
                "Failed to serialize /execute input for {}: {}",
                requested_path, err
            );
            json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "serialization_failed",
                "Failed to serialize the forwarded execute payload.",
            )
        })?
    };

    Ok(VirtualExecuteRequest {
        operation,
        upstream_method,
        upstream_path,
        upstream_query,
        upstream_body_bytes,
    })
}

pub(super) fn build_execute_query(
    operation: &AgentOperation,
    explicit_query: Option<&serde_json::Value>,
    input: &serde_json::Value,
) -> Result<Option<String>, String> {
    let query_source = if let Some(query) = explicit_query {
        Some(query)
    } else if matches!(operation.method.as_str(), "GET" | "HEAD") {
        Some(input)
    } else {
        None
    };

    let Some(query_source) = query_source else {
        return Ok(None);
    };

    let query_object = query_source.as_object().ok_or_else(|| {
        "query must be an object, and GET/HEAD execute inputs must be JSON objects.".to_string()
    })?;
    if query_object.is_empty() {
        return Ok(None);
    }

    let mut url = reqwest::Url::parse("https://gateway.apiosk.invalid").expect("static URL");
    {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query_object {
            // These virtual read adapters accept one structured JSON argument.
            // Repeating array elements would lose address/parcel relationships.
            if value.is_array()
                && matches!(
                    (operation.path.as_str(), key.as_str()),
                    ("/apiosk/company-address", "addresses")
                        | ("/apiosk/linked-parcels", "designations")
                )
            {
                pairs.append_pair(key, &value.to_string());
                continue;
            }
            for encoded_value in json_query_value_strings(value) {
                pairs.append_pair(key, &encoded_value);
            }
        }
    }

    Ok(url.query().map(str::to_string))
}

pub(super) fn json_query_value_strings(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Null => vec!["null".to_string()],
        serde_json::Value::Bool(value) => vec![value.to_string()],
        serde_json::Value::Number(value) => vec![value.to_string()],
        serde_json::Value::String(value) => vec![value.clone()],
        serde_json::Value::Array(values) => values
            .iter()
            .flat_map(json_query_value_strings)
            .collect::<Vec<_>>(),
        serde_json::Value::Object(_) => vec![value.to_string()],
    }
}

pub(super) fn substitute_operation_path(
    operation_path: &str,
    path_params: Option<&serde_json::Value>,
) -> Result<String, String> {
    let mut resolved = normalize_path(operation_path);
    let Some(path_params) = path_params.and_then(|value| value.as_object()) else {
        if has_unresolved_path_params(&resolved) {
            return Err("path_params are required for this operation.".to_string());
        }
        return Ok(resolved);
    };

    for (key, value) in path_params {
        let replacement = match value {
            serde_json::Value::Null => "null".to_string(),
            serde_json::Value::Bool(value) => value.to_string(),
            serde_json::Value::Number(value) => value.to_string(),
            serde_json::Value::String(value) => value.clone(),
            _ => value.to_string(),
        };
        resolved = resolved.replace(&format!("{{{}}}", key), &replacement);
        resolved = resolved.replace(&format!(":{}", key), &replacement);
    }

    if has_unresolved_path_params(&resolved) {
        return Err("One or more required path_params are missing.".to_string());
    }

    Ok(resolved)
}

pub(super) fn has_unresolved_path_params(path: &str) -> bool {
    path.contains('{') || path.split('/').any(|segment| segment.starts_with(':'))
}

pub(super) fn find_matching_operation_metadata<'a>(
    raw_metadata: &'a serde_json::Value,
    method: &str,
    path: &str,
) -> Option<&'a serde_json::Value> {
    let target_key = canonical_operation_key(method, path);
    raw_metadata
        .get("operations")
        .and_then(|value| value.as_array())
        .and_then(|items| {
            items.iter().find(|item| {
                let item_method = item
                    .get("method")
                    .and_then(|value| value.as_str())
                    .unwrap_or(method);
                let item_path = item
                    .get("path")
                    .and_then(|value| value.as_str())
                    .map(normalize_path)
                    .unwrap_or_else(|| normalize_path(path));

                item.get("key")
                    .and_then(|value| value.as_str())
                    .map(|value| value.eq_ignore_ascii_case(&target_key))
                    .unwrap_or(false)
                    || canonical_operation_key(item_method, &item_path) == target_key
                    || item
                        .get("id")
                        .and_then(|value| value.as_str())
                        .map(|value| value.trim() == path)
                        .unwrap_or(false)
            })
        })
}

pub(super) fn derive_default_operation(operations: &[AgentOperation]) -> Option<AgentOperation> {
    let mut ranked = operations.to_vec();
    ranked.sort_by_key(default_operation_rank);
    ranked.into_iter().next()
}

pub(super) fn resolve_default_operation(
    operations: &[AgentOperation],
    configured_reference: Option<&str>,
) -> Option<AgentOperation> {
    let configured =
        configured_reference.and_then(|reference| find_agent_operation(operations, reference));
    let preferred = derive_default_operation(operations);

    match (configured, preferred) {
        (Some(configured), Some(preferred))
            if is_diagnostic_operation(&configured) && !is_diagnostic_operation(&preferred) =>
        {
            Some(preferred)
        }
        (Some(configured), _) => Some(configured),
        (None, Some(preferred)) => Some(preferred),
        (None, None) => None,
    }
}

pub(super) fn find_agent_operation(
    operations: &[AgentOperation],
    reference: &str,
) -> Option<AgentOperation> {
    let trimmed = reference.trim();
    if trimmed.is_empty() {
        return None;
    }

    let normalized_path = normalize_path(trimmed);
    let normalized_key = if trimmed.contains(' ') {
        trimmed.to_ascii_uppercase()
    } else {
        String::new()
    };
    let alias = operation_alias(&normalized_path);

    operations.iter().find_map(|operation| {
        if operation.id == trimmed
            || operation.id == normalized_path
            || operation.key.eq_ignore_ascii_case(trimmed)
            || (!normalized_key.is_empty() && operation.key == normalized_key)
            || operation.path == normalized_path
            || operation_alias(&operation.path) == alias
        {
            Some(operation.clone())
        } else {
            None
        }
    })
}

pub(super) fn operation_alias(path: &str) -> String {
    let normalized = normalize_path(path);
    let trimmed = normalized.trim_matches('/');
    if trimmed.is_empty() {
        return "root".to_string();
    }
    trimmed
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

pub(super) fn canonical_operation_key(method: &str, path: &str) -> String {
    format!("{} {}", method.to_ascii_uppercase(), normalize_path(path))
}

fn default_operation_rank(operation: &AgentOperation) -> (u8, u8, usize, usize, u8, u8, String) {
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

fn is_diagnostic_operation(operation: &AgentOperation) -> bool {
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

pub(super) fn derive_read_only_hint(
    api_detail: &crate::db::ApiDetail,
    operations: &[AgentOperation],
) -> bool {
    if operations
        .iter()
        .all(|operation| matches!(operation.method.as_str(), "GET" | "HEAD"))
    {
        return true;
    }

    let haystack = format!(
        "{} {} {}",
        api_detail.slug, api_detail.name, api_detail.description
    )
    .to_ascii_lowercase();
    [
        "extract", "lookup", "watch", "map", "research", "digest", "diff", "monitor", "verify",
        "metadata", "status",
    ]
    .iter()
    .any(|token| haystack.contains(token))
}

pub(super) fn derive_idempotent_hint(operations: &[AgentOperation]) -> bool {
    operations
        .iter()
        .all(|operation| matches!(operation.method.as_str(), "GET" | "HEAD"))
}

pub(super) fn method_from_string(method: &str) -> Result<Method, ()> {
    Method::from_bytes(method.as_bytes()).map_err(|_| ())
}

pub(super) fn append_forward_query(
    base_url: &str,
    original_query: Option<&str>,
    extra_query: Option<&str>,
) -> String {
    let mut queries = Vec::new();
    if let Some(query) = original_query.filter(|query| !query.is_empty()) {
        queries.push(query);
    }
    if let Some(query) = extra_query.filter(|query| !query.is_empty()) {
        queries.push(query);
    }

    if queries.is_empty() {
        base_url.to_string()
    } else {
        format!("{}?{}", base_url, queries.join("&"))
    }
}

pub(super) fn uses_execute_envelope(
    payload_object: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    payload_object.contains_key("input")
        || payload_object.contains_key("operation")
        || payload_object.contains_key("path_params")
        || payload_object.contains_key("pathParams")
}

pub(super) fn agent_metadata_response(agent_surface: &AgentSurfaceContext) -> Response<Body> {
    let body = serde_json::json!({
        "success": true,
        "status": "success",
        "api": {
            "id": agent_surface.api_detail.id,
            "slug": agent_surface.api_detail.slug,
            "name": agent_surface.api_detail.name,
            "description": agent_surface.api_detail.description,
            "category": agent_surface.api_detail.category,
            "listing_type": agent_surface.api_detail.listing_type,
            "gateway_base_url": format!("https://gateway.apiosk.com/{}", agent_surface.api_detail.slug),
            "origin_url": agent_surface.api_detail.origin_url,
            "docs_url": agent_surface.api_detail.docs_url,
        },
        "execute": {
            "method": "POST",
            "path": "/execute",
            "default_operation": agent_surface
                .default_operation
                .as_ref()
                .map(|operation| operation.id.clone()),
            "request_formats": [
                {
                    "type": "raw_default_input",
                    "description": "POST the default operation's JSON body directly to /execute."
                },
                {
                    "type": "envelope",
                    "description": "Use { operation, input, query, path_params } when selecting a non-default operation or overriding path/query forwarding."
                }
            ],
        },
        "metadata_path": "/metadata",
        "operations": agent_surface.operations,
        "metadata": agent_surface.listing_metadata,
        "mcp_tool": agent_surface.listing_metadata.get("mcp_tool").cloned().unwrap_or_else(|| serde_json::json!({})),
        "input_schema": agent_surface.listing_metadata.get("input_schema").cloned().unwrap_or_else(default_execute_input_schema),
        "output_schema": agent_surface.listing_metadata.get("output_schema").cloned().unwrap_or_else(default_execute_output_schema),
        "cost_per_call": agent_surface.listing_metadata.get("cost_per_call").cloned().unwrap_or_else(|| serde_json::json!(0.0)),
        "latency_ms": agent_surface.listing_metadata.get("latency_ms").cloned().unwrap_or_else(|| serde_json::json!(0)),
        "tags": agent_surface.listing_metadata.get("tags").cloned().unwrap_or_else(|| serde_json::json!([])),
        "agent_native": true,
        "mcp_native": true,
    });

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header("Cache-Control", "no-store")
        .header("X-Apiosk-Gateway", "true")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| Response::new(Body::from(r#"{"error":"response_build_error"}"#)))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn agent_execute_response(
    api_slug: &str,
    execution: &VirtualExecuteRequest,
    endpoint: Option<&crate::db::Endpoint>,
    status: reqwest::StatusCode,
    response_headers: &reqwest::header::HeaderMap,
    response_body: &[u8],
    latency_ms: u64,
    settled_price_paid_usdc: Option<f64>,
    credits_charge: Option<&crate::db::CreditChargeResult>,
    settled_receipt: Option<&SettledPaymentReceipt>,
) -> Response<Body> {
    let content_type = response_headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream");
    let quoted_cost = endpoint
        .map(|value| value.price.to_string().parse().unwrap_or(0.0))
        .unwrap_or(execution.operation.price_usd);
    let cost = credits_charge
        .map(|charge| charge.amount_eur.to_string().parse().unwrap_or(0.0))
        .or(settled_price_paid_usdc)
        .unwrap_or_else(|| {
            if status.is_success() {
                quoted_cost
            } else {
                0.0
            }
        });
    let result = response_result_value(content_type, response_body);

    let mut body = serde_json::json!({
        "status": if status.is_success() { "success" } else { "error" },
        "result": result,
        "cost": cost,
        "latency": latency_ms,
        "operation": execution.operation.id,
        "api": api_slug,
        "upstream_status": status.as_u16(),
    });

    if let Some(charge) = credits_charge {
        body["payment_rail"] = serde_json::json!("credits");
        body["credits_used"] = serde_json::json!(charge.credits_used);
        body["credits_remaining"] = serde_json::json!(charge.credits_remaining);
    } else if settled_price_paid_usdc.is_some() {
        body["payment_rail"] = serde_json::json!(settled_receipt.map_or("x402", SettledPaymentReceipt::payment_rail));
    }

    if let Some(receipt) = settled_receipt {
        if let Some(ref wallet) = receipt.wallet_used {
            body["wallet_used"] = serde_json::json!(wallet);
        }
        if let Some(tx_hash) = receipt.onchain_tx_hash() {
            body["tx_hash"] = serde_json::json!(tx_hash);
        }
        body["price_listed_usdc"] = serde_json::json!(receipt.price_listed_usdc);
    }

    if !status.is_success() {
        body["error"] = serde_json::json!({
            "code": "upstream_error",
            "message": format!("Upstream returned {} {}", status.as_u16(), status.canonical_reason().unwrap_or("Unknown")),
        });
    }

    let mut builder = Response::builder()
        .status(status.as_u16())
        .header("Content-Type", "application/json")
        .header("Cache-Control", "no-store")
        .header("X-Apiosk-Gateway", "true")
        .header("X-Apiosk-API", api_slug)
        .header("X-Apiosk-Operation", execution.operation.key.clone());

    if credits_charge.is_some() || settled_price_paid_usdc.is_some() {
        builder = builder.header(
            "X-Apiosk-Payment-Rail",
            if credits_charge.is_some() {
                "credits"
            } else {
                settled_receipt.map_or("x402", SettledPaymentReceipt::payment_rail)
            },
        );
    }

    if let Some(receipt) = settled_receipt {
        if let Some(ref wallet) = receipt.wallet_used {
            if let Ok(value) = HeaderValue::from_str(wallet) {
                builder = builder.header("X-Apiosk-Wallet-Used", value);
            }
        }
        if let Some(tx_hash) = receipt.onchain_tx_hash() {
            if let Ok(value) = HeaderValue::from_str(tx_hash) {
                builder = builder.header("X-Apiosk-Tx-Hash", value);
            }
        }
        builder = receipt.price_headers(builder);
    }

    builder
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| Response::new(Body::from(r#"{"error":"response_build_error"}"#)))
}

pub(super) fn response_result_value(content_type: &str, response_body: &[u8]) -> serde_json::Value {
    if content_type.contains("application/json") || content_type.contains("+json") {
        if let Ok(json_body) = serde_json::from_slice::<serde_json::Value>(response_body) {
            return json_body;
        }
    }

    if content_type.starts_with("text/")
        || content_type.contains("xml")
        || content_type.contains("html")
        || content_type.contains("javascript")
        || content_type.contains("svg")
    {
        if let Ok(text_body) = std::str::from_utf8(response_body) {
            return serde_json::json!(text_body);
        }
    }

    if let Ok(text_body) = std::str::from_utf8(response_body) {
        return serde_json::json!(text_body);
    }

    serde_json::json!({
        "encoding": "base64",
        "content_type": content_type,
        "byte_length": response_body.len(),
        "data": BASE64_STANDARD.encode(response_body),
    })
}
