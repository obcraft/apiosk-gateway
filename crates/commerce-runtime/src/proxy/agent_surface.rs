use super::*;

pub(super) async fn load_agent_surface_context(
    state: &Arc<AppState>,
    slug: &str,
    api_id: Uuid,
) -> Result<AgentSurfaceContext, Response<Body>> {
    let api_detail = state.db.get_api_detail(slug).await.map_err(|err| {
        error!("Failed to load API detail for {}: {}", slug, err);
        json_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "agent_metadata_lookup_failed",
            "Failed to load agent metadata for this API.",
        )
    })?;

    let api_detail = api_detail.ok_or_else(|| {
        json_error_response(
            StatusCode::NOT_FOUND,
            "agent_metadata_not_found",
            "API metadata is not available for this listing.",
        )
    })?;

    let endpoints = state
        .db
        .list_endpoints_with_docs(api_id)
        .await
        .map_err(|err| {
            error!("Failed to load endpoint docs for {}: {}", slug, err);
            json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "endpoint_docs_lookup_failed",
                "Failed to load endpoint docs for this API.",
            )
        })?;

    let operations = build_agent_operations(&api_detail, &api_detail.listing_metadata, &endpoints);
    let listing_metadata = build_agent_listing_metadata(&api_detail, &operations);
    let default_operation = resolve_default_operation(
        &operations,
        listing_metadata
            .get("default_operation")
            .and_then(|value| value.as_str()),
    );

    Ok(AgentSurfaceContext {
        api_detail,
        listing_metadata,
        operations,
        default_operation,
    })
}

pub(super) fn build_agent_operations(
    api_detail: &crate::db::ApiDetail,
    raw_metadata: &serde_json::Value,
    endpoints: &[crate::db::EndpointWithDocs],
) -> Vec<AgentOperation> {
    let gateway_base_url = format!("https://gateway.apiosk.com/{}", api_detail.slug);
    let execute_url = format!("{}/execute", gateway_base_url);
    let mut operations = Vec::new();
    let mut seen_keys = HashSet::new();

    for endpoint in endpoints {
        let path = normalize_path(&endpoint.path);
        let key = canonical_operation_key(&endpoint.method, &path);
        let existing = find_matching_operation_metadata(raw_metadata, &endpoint.method, &path);
        let operation = AgentOperation {
            id: existing
                .and_then(|value| value.get("id"))
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(&path)
                .to_string(),
            key: key.clone(),
            method: endpoint.method.to_uppercase(),
            path: path.clone(),
            gateway_url: format!("{}{}", gateway_base_url, path),
            execute_url: execute_url.clone(),
            payment_required: endpoint.payment_required,
            price_usd: existing
                .and_then(|value| value.get("price_usd"))
                .and_then(|value| value.as_f64())
                .unwrap_or_else(|| endpoint.price.to_string().parse().unwrap_or(0.0)),
            description: existing
                .and_then(|value| value.get("description"))
                .and_then(|value| value.as_str())
                .map(str::to_string)
                .or_else(|| endpoint.description.clone()),
            request_body: existing
                .and_then(|value| value.get("input_schema"))
                .cloned()
                .or_else(|| endpoint.request_body.clone()),
            response_body: existing
                .and_then(|value| value.get("output_schema"))
                .cloned()
                .or_else(|| endpoint.response_body.clone()),
            request_example: endpoint.request_example.clone(),
            response_example: endpoint.response_example.clone(),
            headers: endpoint.headers.clone(),
            query_params: endpoint.query_params.clone(),
        };
        seen_keys.insert(key);
        operations.push(operation);
    }

    if let Some(metadata_operations) = raw_metadata
        .get("operations")
        .and_then(|value| value.as_array())
    {
        for item in metadata_operations {
            let method = item
                .get("method")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("POST")
                .to_uppercase();
            let path = item
                .get("path")
                .and_then(|value| value.as_str())
                .map(normalize_path)
                .unwrap_or_else(|| "/".to_string());
            let key = canonical_operation_key(&method, &path);
            if seen_keys.contains(&key) {
                continue;
            }

            operations.push(AgentOperation {
                id: item
                    .get("id")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or(&path)
                    .to_string(),
                key: key.clone(),
                method,
                path: path.clone(),
                gateway_url: format!("{}{}", gateway_base_url, path),
                execute_url: execute_url.clone(),
                payment_required: item
                    .get("payment_required")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(true),
                price_usd: item
                    .get("price_usd")
                    .and_then(|value| value.as_f64())
                    .unwrap_or_else(|| {
                        raw_metadata
                            .get("cost_per_call")
                            .and_then(|value| value.as_f64())
                            .unwrap_or(0.0)
                    }),
                description: item
                    .get("description")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                request_body: item.get("input_schema").cloned(),
                response_body: item.get("output_schema").cloned(),
                request_example: item.get("request_example").cloned(),
                response_example: item.get("response_example").cloned(),
                headers: item.get("headers").cloned(),
                query_params: item.get("query_params").cloned(),
            });
            seen_keys.insert(key);
        }
    }

    operations.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.method.cmp(&right.method))
            .then(left.id.cmp(&right.id))
    });
    operations
}

pub(super) fn build_agent_listing_metadata(
    api_detail: &crate::db::ApiDetail,
    operations: &[AgentOperation],
) -> serde_json::Value {
    let mut metadata = api_detail
        .listing_metadata
        .as_object()
        .cloned()
        .unwrap_or_else(serde_json::Map::new);

    let model_backed = metadata
        .get("model_backed")
        .and_then(|value| value.as_bool())
        .unwrap_or_else(|| derive_model_backed(api_detail));
    let default_operation = resolve_default_operation(
        operations,
        metadata
            .get("default_operation")
            .and_then(|value| value.as_str()),
    );
    let default_operation_id = default_operation
        .as_ref()
        .map(|operation| operation.id.clone());

    metadata.insert("agent_native".to_string(), serde_json::json!(true));
    metadata.insert("mcp_native".to_string(), serde_json::json!(true));
    metadata
        .entry("metadata_path".to_string())
        .or_insert_with(|| serde_json::json!("/metadata"));
    metadata
        .entry("execute_path".to_string())
        .or_insert_with(|| serde_json::json!("/execute"));
    metadata
        .entry("implementation_type".to_string())
        .or_insert_with(|| serde_json::json!(derive_implementation_type(api_detail, model_backed)));
    metadata.insert("model_backed".to_string(), serde_json::json!(model_backed));
    metadata.entry("latency_ms".to_string()).or_insert_with(|| {
        serde_json::json!(derive_latency_ms(api_detail, operations, model_backed))
    });

    let cost_per_call = metadata
        .get("cost_per_call")
        .and_then(|value| value.as_f64())
        .or_else(|| {
            default_operation
                .as_ref()
                .map(|operation| operation.price_usd)
        })
        .or_else(|| operations.first().map(|operation| operation.price_usd))
        .unwrap_or(0.0);
    metadata.insert(
        "cost_per_call".to_string(),
        serde_json::json!(cost_per_call),
    );

    if let Some(default_operation) = default_operation_id {
        metadata.insert(
            "default_operation".to_string(),
            serde_json::json!(default_operation),
        );
    }

    let tags = merge_tags(
        metadata.get("tags"),
        derive_tags(api_detail, operations, model_backed),
    );
    metadata.insert("tags".to_string(), serde_json::json!(tags));
    metadata.insert("operations".to_string(), serde_json::json!(operations));

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
    metadata.entry("provider".to_string()).or_insert_with(|| {
        serde_json::json!({
            "type": "http_api",
            "origin_url": api_detail.origin_url,
            "docs_url": api_detail.docs_url,
            "listing_type": api_detail.listing_type,
        })
    });
    let mcp_tool_input_schema = metadata
        .get("mcp_tool")
        .and_then(|value| value.as_object())
        .and_then(|value| value.get("inputSchema"))
        .cloned()
        .or_else(|| {
            if input_schema == default_execute_input_schema() {
                default_operation
                    .as_ref()
                    .and_then(|operation| operation.request_body.clone())
                    .filter(|schema| schema.is_object())
            } else {
                None
            }
        })
        .unwrap_or_else(|| input_schema.clone());

    metadata.insert(
        "mcp_tool".to_string(),
        serde_json::json!({
            "name": api_detail.slug,
            "title": api_detail.name,
            "description": api_detail.description,
            "inputSchema": mcp_tool_input_schema,
            "outputSchema": output_schema,
            "annotations": {
                "readOnlyHint": derive_read_only_hint(api_detail, operations),
                "idempotentHint": derive_idempotent_hint(operations),
                "destructiveHint": false,
                "openWorldHint": false,
            }
        }),
    );

    // S1: strip injected upstream credentials / signing secrets before this
    // surface is serialized into the public /metadata and /execute responses.
    // The forward path still injects them from the raw DB row.
    crate::agent_metadata::redact_secret_metadata(&mut metadata);
    serde_json::Value::Object(metadata)
}

pub(super) fn derive_model_backed(api_detail: &crate::db::ApiDetail) -> bool {
    let category = api_detail.category.as_deref().unwrap_or_default();
    if category.eq_ignore_ascii_case("compute") {
        return true;
    }

    let haystack = format!(
        "{} {} {}",
        api_detail.slug, api_detail.name, api_detail.description
    )
    .to_ascii_lowercase();

    [
        "llm", "ai", "extract", "research", "intel", "map", "diff", "digest",
    ]
    .iter()
    .any(|token| haystack.contains(token))
}

pub(super) fn derive_implementation_type(
    api_detail: &crate::db::ApiDetail,
    model_backed: bool,
) -> &'static str {
    let category = api_detail.category.as_deref().unwrap_or_default();
    if model_backed {
        "hybrid"
    } else if category.eq_ignore_ascii_case("compute") {
        "deterministic"
    } else {
        "direct"
    }
}

pub(super) fn derive_latency_ms(
    api_detail: &crate::db::ApiDetail,
    operations: &[AgentOperation],
    model_backed: bool,
) -> u64 {
    if operations
        .iter()
        .any(|operation| operation.path.to_ascii_lowercase().contains("batch"))
    {
        return 1_500;
    }

    let category = api_detail.category.as_deref().unwrap_or_default();
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

pub(super) fn derive_tags(
    api_detail: &crate::db::ApiDetail,
    operations: &[AgentOperation],
    model_backed: bool,
) -> Vec<String> {
    let mut tags = BTreeSet::new();
    tags.insert("agent-ready".to_string());
    tags.insert("mcp-native".to_string());

    if let Some(category) = api_detail
        .category
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        tags.insert(category.to_ascii_lowercase());
    }

    if let Some(listing_type) = api_detail
        .listing_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        tags.insert(listing_type.to_ascii_lowercase());
    }

    if model_backed {
        tags.insert("model-backed".to_string());
    }

    if operations.iter().any(|operation| operation.method == "GET") {
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

pub(super) fn merge_tags(
    existing: Option<&serde_json::Value>,
    derived: Vec<String>,
) -> Vec<String> {
    let mut tags = BTreeSet::new();
    if let Some(existing) = existing.and_then(|value| value.as_array()) {
        for tag in existing.iter().filter_map(|value| value.as_str()) {
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

pub(super) fn default_execute_input_schema() -> serde_json::Value {
    serde_json::json!({
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

pub(super) fn default_execute_output_schema() -> serde_json::Value {
    serde_json::json!({
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
