use super::*;
use axum::extract::Path;
use serde_json::Value;

#[derive(Debug, Clone, Copy)]
enum ResourceKind {
    Dataset,
    Document,
    Media,
}

impl ResourceKind {
    fn route_prefix(self) -> &'static str {
        match self {
            ResourceKind::Dataset => "datasets",
            ResourceKind::Document => "documents",
            ResourceKind::Media => "media",
        }
    }
}

#[derive(Debug, Clone)]
struct PaidResourceConfig {
    slug: String,
    url: String,
    content_type: Option<String>,
    file_name: Option<String>,
}

pub async fn serve_dataset_file(
    State(state): State<Arc<AppState>>,
    Path((collection_slug, file_slug)): Path<(String, String)>,
    request: Request<Body>,
) -> Response<Body> {
    serve_paid_resource(
        state,
        ResourceKind::Dataset,
        collection_slug,
        None,
        file_slug,
        request,
    )
    .await
}

pub async fn serve_document_file(
    State(state): State<Arc<AppState>>,
    Path((collection_slug, file_slug)): Path<(String, String)>,
    request: Request<Body>,
) -> Response<Body> {
    serve_paid_resource(
        state,
        ResourceKind::Document,
        collection_slug,
        None,
        file_slug,
        request,
    )
    .await
}

pub async fn serve_media_file(
    State(state): State<Arc<AppState>>,
    Path((collection_slug, route_group, file_slug)): Path<(String, String, String)>,
    request: Request<Body>,
) -> Response<Body> {
    serve_paid_resource(
        state,
        ResourceKind::Media,
        collection_slug,
        Some(route_group),
        file_slug,
        request,
    )
    .await
}

async fn serve_paid_resource(
    state: Arc<AppState>,
    kind: ResourceKind,
    collection_slug: String,
    route_group: Option<String>,
    file_slug: String,
    request: Request<Body>,
) -> Response<Body> {
    if !is_valid_api_slug(&collection_slug) || !is_valid_resource_slug(&file_slug) {
        return json_error_response(
            StatusCode::BAD_REQUEST,
            "invalid_resource_slug",
            "Collection and file slugs must be lowercase URL-safe identifiers.",
        );
    }
    if let Some(group) = route_group.as_deref() {
        if !is_valid_resource_slug(group) {
            return json_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_route_group",
                "Media route group must be a lowercase URL-safe identifier.",
            );
        }
    }

    let method = request.method().clone();
    if method != Method::GET && method != Method::HEAD {
        return json_error_response(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "Paid resources support GET and HEAD only.",
        );
    }

    let headers = request.headers().clone();
    let request_context = request.extensions().get::<RequestContext>().cloned();
    let tracking_context = TrackingContext::from_request(request_context.as_ref());
    let browser_mode = is_browser_html_request(&method, &headers);
    let endpoint_path = match route_group.as_deref() {
        Some(group) => format!("/{}/{}", group, file_slug),
        None => format!("/files/{}", file_slug),
    };
    let gateway_path = match route_group.as_deref() {
        Some(group) => format!("/{}/{}/{}", collection_slug, group, file_slug),
        None => format!("/{}/files/{}", collection_slug, file_slug),
    };

    let api = match state.cache.get_api(&state.db, &collection_slug).await {
        Ok(Some(api)) => api,
        Ok(None) => {
            return json_error_response(
                StatusCode::NOT_FOUND,
                "collection_not_found",
                "No paid resource collection matched this slug.",
            )
        }
        Err(err) => {
            error!(
                "Paid resource API lookup failed for {}: {}",
                collection_slug, err
            );
            return json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database_error",
                "Failed to resolve paid resource collection.",
            );
        }
    };

    if api.status != "active" || !api.is_verified.unwrap_or(false) {
        return json_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "collection_inactive",
            "This paid resource collection is not active.",
        );
    }

    let listing = match state.db.get_listing_by_slug(&collection_slug).await {
        Ok(listing) => listing,
        Err(err) => {
            error!(
                "Paid resource listing lookup failed for {}: {}",
                collection_slug, err
            );
            return json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "listing_lookup_failed",
                "Failed to load paid resource listing.",
            );
        }
    };
    let listing_type = listing
        .as_ref()
        .map(|listing| listing.listing_type.clone())
        .unwrap_or(ListingType::Api);

    let resource = match resolve_paid_resource(
        &state,
        listing.as_ref(),
        api.id,
        kind,
        route_group.as_deref(),
        &file_slug,
    )
    .await
    {
        Ok(Some(resource)) => resource,
        Ok(None) => {
            return json_error_response(
                StatusCode::NOT_FOUND,
                "resource_not_found",
                "No paid file resource matched this slug.",
            )
        }
        Err(response) => return response,
    };

    if let Err(reason) = crate::api_management::helpers::validate_upstream_url(&resource.url) {
        warn!(
            "Refusing paid resource {} from unsafe URL {} ({})",
            resource.slug, resource.url, reason
        );
        return json_error_response(
            StatusCode::BAD_GATEWAY,
            "resource_url_not_allowed",
            "This paid resource has an unsafe or invalid storage URL configured.",
        );
    }

    let endpoint = match state
        .db
        .get_endpoint(api.id, method.as_str(), &endpoint_path)
        .await
    {
        Ok(endpoint) => endpoint,
        Err(err) => {
            error!(
                "Paid resource endpoint lookup failed for {}{}: {}",
                collection_slug, endpoint_path, err
            );
            return json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "endpoint_lookup_failed",
                "Failed to resolve paid resource endpoint.",
            );
        }
    };

    let resolved = ResolvedProxyRequest {
        api,
        listing,
        listing_type,
        preset_auth_headers: Vec::new(),
        preset_auth_query: Vec::new(),
        endpoint,
        virtual_execute_request: None,
        resolved_method: method.clone(),
        resolved_path: endpoint_path.clone(),
        resell: None, // paid-resource collections are first-party, never resold
    };

    let payment_decision = match enforce_endpoint_payment(
        &state,
        kind.route_prefix(),
        &gateway_path,
        &headers,
        browser_mode,
        &tracking_context,
        &resolved,
    )
    .await
    {
        Ok(decision) => decision,
        Err(response) => return response,
    };

    if let Some(response) = super::handle_flow::enforce_rate_limit(
        &state,
        &resolved.api.slug,
        browser_mode,
        &tracking_context,
        payment_decision.agent_connect_token_id,
    )
    .await
    {
        return response;
    }

    match proxy_resource_url(&state, &resource, &method, &headers).await {
        Ok(response) => {
            log_free_resource_usage(&state, &resolved, &tracking_context).await;
            response
        }
        Err(response) => response,
    }
}

async fn resolve_paid_resource(
    state: &Arc<AppState>,
    listing: Option<&Listing>,
    api_id: Uuid,
    kind: ResourceKind,
    route_group: Option<&str>,
    file_slug: &str,
) -> Result<Option<PaidResourceConfig>, Response<Body>> {
    if let Some(metadata) = listing.map(|listing| &listing.listing_metadata) {
        if let Some(resource) = resource_from_metadata(metadata, kind, route_group, file_slug) {
            return Ok(Some(resource));
        }
    }

    match kind {
        ResourceKind::Dataset => match state.db.get_dataset_listing(api_id).await {
            Ok(Some(dataset)) => {
                if let Some(url) = dataset.bulk_url {
                    let default_slug = dataset.data_format.as_str();
                    if file_slug == "download" || file_slug == "bulk" || file_slug == default_slug {
                        return Ok(Some(PaidResourceConfig {
                            slug: file_slug.to_string(),
                            url,
                            content_type: content_type_for_format(&dataset.data_format)
                                .map(str::to_string),
                            file_name: Some(format!("dataset.{}", dataset.data_format)),
                        }));
                    }
                }
                Ok(None)
            }
            Ok(None) => Ok(None),
            Err(err) => {
                error!("Dataset fallback lookup failed: {}", err);
                Err(json_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "dataset_lookup_failed",
                    "Failed to load dataset resource configuration.",
                ))
            }
        },
        ResourceKind::Document | ResourceKind::Media => {
            match state.db.get_product_listing(api_id).await {
                Ok(Some(product)) => {
                    if file_slug == slug_without_extension(&product.file_name)
                        || file_slug == product.file_name
                    {
                        let url = match product.storage_type.as_str() {
                            "url" => product.storage_path,
                            "supabase" | "s3" => product.storage_path,
                            _ => return Ok(None),
                        };
                        return Ok(Some(PaidResourceConfig {
                            slug: file_slug.to_string(),
                            url,
                            content_type: Some(product.mime_type),
                            file_name: Some(product.file_name),
                        }));
                    }
                    Ok(None)
                }
                Ok(None) => Ok(None),
                Err(err) => {
                    error!("Product fallback lookup failed: {}", err);
                    Err(json_error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "product_lookup_failed",
                        "Failed to load file resource configuration.",
                    ))
                }
            }
        }
    }
}

fn resource_from_metadata(
    metadata: &Value,
    kind: ResourceKind,
    route_group: Option<&str>,
    file_slug: &str,
) -> Option<PaidResourceConfig> {
    let containers = [
        "paid_resources",
        "resources",
        "files",
        kind.route_prefix(),
        "media",
        "documents",
        "datasets",
    ];

    for key in containers {
        if let Some(container) = metadata.get(key) {
            if let Some(resource) = resource_from_value(container, route_group, file_slug) {
                return Some(resource);
            }
        }
    }

    resource_from_value(metadata, route_group, file_slug)
}

fn resource_from_value(
    value: &Value,
    route_group: Option<&str>,
    file_slug: &str,
) -> Option<PaidResourceConfig> {
    match value {
        Value::Array(items) => items
            .iter()
            .find_map(|item| resource_from_object(item, route_group, file_slug)),
        Value::Object(map) => {
            if let Some(item) = map.get(file_slug) {
                return resource_from_object(item, route_group, file_slug).or_else(|| {
                    item.as_str().map(|url| PaidResourceConfig {
                        slug: file_slug.to_string(),
                        url: url.to_string(),
                        content_type: None,
                        file_name: None,
                    })
                });
            }
            map.values()
                .find_map(|item| resource_from_object(item, route_group, file_slug))
        }
        _ => None,
    }
}

fn resource_from_object(
    value: &Value,
    route_group: Option<&str>,
    file_slug: &str,
) -> Option<PaidResourceConfig> {
    let object = value.as_object()?;
    let slug = string_field(value, &["slug", "file_slug", "fileSlug", "id", "name"])?;
    if slug != file_slug {
        return None;
    }
    if let Some(expected_group) = route_group {
        if let Some(group) = string_field(value, &["route_group", "routeGroup", "group", "type"]) {
            if group != expected_group {
                return None;
            }
        }
    }
    let url = string_field(
        value,
        &[
            "url",
            "storage_url",
            "storageUrl",
            "external_url",
            "externalUrl",
            "href",
        ],
    )?;
    if object.is_empty() {
        return None;
    }
    Some(PaidResourceConfig {
        slug,
        url,
        content_type: string_field(
            value,
            &["content_type", "contentType", "mime_type", "mimeType"],
        ),
        file_name: string_field(
            value,
            &["file_name", "fileName", "filename", "download_name"],
        ),
    })
}

async fn proxy_resource_url(
    state: &Arc<AppState>,
    resource: &PaidResourceConfig,
    method: &Method,
    headers: &HeaderMap<HeaderValue>,
) -> Result<Response<Body>, Response<Body>> {
    let reqwest_method = if *method == Method::HEAD {
        reqwest::Method::HEAD
    } else {
        reqwest::Method::GET
    };
    let mut request = state.http_client.request(reqwest_method, &resource.url);
    for name in [
        header::RANGE,
        header::IF_RANGE,
        header::IF_NONE_MATCH,
        header::IF_MODIFIED_SINCE,
    ] {
        if let Some(value) = headers.get(&name) {
            request = request.header(name, value.clone());
        }
    }

    let upstream = request.send().await.map_err(|err| {
        error!("Paid resource fetch failed for {}: {}", resource.url, err);
        json_error_response(
            StatusCode::BAD_GATEWAY,
            "resource_fetch_failed",
            "Could not retrieve paid resource from storage.",
        )
    })?;

    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut builder = Response::builder().status(status);
    for name in [
        header::CONTENT_TYPE,
        header::CONTENT_LENGTH,
        header::ACCEPT_RANGES,
        header::CONTENT_RANGE,
        header::CACHE_CONTROL,
        header::ETAG,
        header::LAST_MODIFIED,
    ] {
        if let Some(value) = upstream.headers().get(&name) {
            builder = builder.header(name, value);
        }
    }

    if upstream.headers().get(header::CONTENT_TYPE).is_none() {
        if let Some(content_type) = resource.content_type.as_deref() {
            builder = builder.header(header::CONTENT_TYPE, content_type);
        }
    }
    if upstream
        .headers()
        .get(header::CONTENT_DISPOSITION)
        .is_none()
    {
        if let Some(file_name) = resource.file_name.as_deref() {
            builder = builder.header(
                header::CONTENT_DISPOSITION,
                format!(
                    "attachment; filename=\"{}\"",
                    sanitize_header_filename(file_name)
                ),
            );
        }
    } else if let Some(value) = upstream.headers().get(header::CONTENT_DISPOSITION) {
        builder = builder.header(header::CONTENT_DISPOSITION, value);
    }
    builder = builder.header("X-Apiosk-Resource", &resource.slug);

    if *method == Method::HEAD {
        return builder.body(Body::empty()).map_err(|_| {
            json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "response_build_failed",
                "Failed to build paid resource response.",
            )
        });
    }

    let body = upstream.bytes().await.map_err(|err| {
        error!("Paid resource read failed for {}: {}", resource.url, err);
        json_error_response(
            StatusCode::BAD_GATEWAY,
            "resource_read_failed",
            "Failed to read paid resource from storage.",
        )
    })?;

    builder.body(Body::from(body)).map_err(|_| {
        json_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "response_build_failed",
            "Failed to build paid resource response.",
        )
    })
}

async fn log_free_resource_usage(
    state: &Arc<AppState>,
    resolved: &ResolvedProxyRequest,
    tracking_context: &TrackingContext,
) {
    let should_log = resolved
        .endpoint
        .as_ref()
        .is_none_or(|ep| !ep.payment_required || ep.price == 0_i64);
    if !should_log {
        return;
    }
    let amount = resolved.endpoint.as_ref().map(|ep| ep.price.clone());
    let network_token_id = resolved
        .endpoint
        .as_ref()
        .and_then(|ep| ep.network_token_id);
    let endpoint_id = resolved.endpoint.as_ref().map(|ep| ep.id);
    if let Err(err) = state
        .db
        .log_usage(
            resolved.api.id,
            endpoint_id,
            false,
            amount,
            network_token_id,
            None,
            tracking_context.request_id,
            tracking_context.session_id,
            tracking_context.managed_wallet_id,
            tracking_context.client_id.as_deref(),
            tracking_context.agent_connect_token_id,
        )
        .await
    {
        warn!("Failed to log free paid-resource usage: {}", err);
    }
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn content_type_for_format(format: &str) -> Option<&'static str> {
    match format {
        "csv" => Some("text/csv"),
        "json" => Some("application/json"),
        "jsonl" => Some("application/x-ndjson"),
        "parquet" => Some("application/octet-stream"),
        _ => None,
    }
}

fn slug_without_extension(file_name: &str) -> &str {
    file_name
        .rsplit_once('.')
        .map_or(file_name, |(stem, _)| stem)
}

fn sanitize_header_filename(file_name: &str) -> String {
    file_name
        .chars()
        .filter(|ch| ch.is_ascii_graphic() && *ch != '"' && *ch != '\\')
        .collect()
}

fn is_valid_resource_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 128
        && slug.chars().all(|ch| {
            ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_' | '.')
        })
}
