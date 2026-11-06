use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};

use crate::agent_metadata::{
    normalize_listing_metadata, public_listing_metadata, AgentMetadataContext,
    AgentMetadataOperation,
};
use crate::html::is_browser_request;
use crate::AppState;

use super::{
    models::{
        ApiDetailResponse, ApiInfoResponse, ApiListMeta, ApiListQuery, ApiListResponse,
        EndpointDocResponse, ProviderApiListQuery, ProviderApisResponse, ProviderInfoResponse,
        ProviderListQuery, ProviderListResponse, ProviderPublic, TypeInfo, TypeListResponse,
        TypesResponse,
    },
    render::{
        render_api_detail_html, render_api_list_html, render_provider_apis_html,
        render_provider_list_html, render_type_list_html, render_types_html,
    },
};

pub async fn list_apis(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Query(params): Query<ApiListQuery>,
) -> Result<Response, StatusCode> {
    let sort = match params.sort.as_str() {
        "name" | "price" | "newest" => params.sort.as_str(),
        _ => "name",
    };
    let order = match params.order.as_str() {
        "asc" | "desc" => params.order.as_str(),
        _ => "asc",
    };
    let limit = params.limit.clamp(1, 500);
    let offset = params.offset.max(0);
    let created_after = parse_filter_datetime(params.created_after.as_deref(), false)?;
    let created_before = parse_filter_datetime(params.created_before.as_deref(), true)?;
    if params
        .min_price
        .zip(params.max_price)
        .is_some_and(|(min, max)| min > max)
    {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Through the cache, not the database: `/v1/apis` is what the discovery
    // document advertises as `links.catalog`, so this is a crawler-facing path
    // running the most expensive query the gateway has.
    let (apis, total) = state
        .cache
        .get_active_apis(
            &state.db,
            clean_filter(params.search.as_deref()),
            clean_filter(params.category.as_deref()),
            clean_filter(params.tag.as_deref()),
            clean_filter(params.listing_type.as_deref()),
            params.min_price,
            params.max_price,
            created_after,
            created_before,
            clean_filter(params.slug.as_deref()),
            sort,
            order,
            limit,
            offset,
        )
        .await
        .map_err(|err| {
            tracing::error!("Failed to list APIs: {}", err);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let mut mapped: Vec<ApiInfoResponse> = apis.into_iter().map(map_api_info).collect();
    if !params.include_test {
        mapped.retain(|api| api.listing_quality != "test");
    }
    let returned = mapped.len();

    // Browsers get the catalog itself — this is the API list, so it lists
    // APIs. The provider-grouped view of the same catalog lives at
    // /v1/providers. The JSON contract below is unchanged.
    if is_browser_request(&headers) {
        let html = render_api_list_html(&mapped, total, limit, offset, params.search.as_deref());
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/html; charset=utf-8")
            .body(Body::from(html))
            .unwrap());
    }

    Ok(Json(ApiListResponse {
        apis: mapped,
        meta: ApiListMeta {
            total,
            returned,
            limit,
            offset,
            search: params.search,
            category: params.category,
            tag: params.tag,
            listing_type: params.listing_type,
            min_price: params.min_price,
            max_price: params.max_price,
            created_after: params.created_after,
            created_before: params.created_before,
            sort: sort.to_string(),
            order: order.to_string(),
            ranking: crate::db::RANKING_NOTE,
        },
    })
    .into_response())
}

/// The provider directory, render-ready and served from the short-TTL cache.
/// Shared by `GET /` (the home page), `GET /v1/providers` and the browser view
/// of `GET /v1/apis`, so all three see the same rows.
pub async fn provider_directory(
    state: &AppState,
    search: Option<&str>,
) -> Result<Vec<ProviderInfoResponse>, StatusCode> {
    let providers = state
        .cache
        .get_providers(&state.db, clean_filter(search))
        .await
        .map_err(|err| {
            tracing::error!("Failed to list providers: {}", err);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    // What has actually been measured, read from the cache the background
    // refresher fills. Never computed here: `provider_latency_stats` is a
    // 30-second percentile scan that is deliberately kept off the request path
    // (see `db::provider_stats`). A cold cache means unmeasured, and unmeasured
    // costs a card nothing.
    let stats = state.cache.peek_provider_stats().await;

    Ok(providers
        .into_iter()
        .map(|provider| {
            let quality = rate_card(&provider, stats.as_deref());
            map_provider_info(provider, quality)
        })
        .collect())
}

/// The stars one card wears: what its listings publish, plus what the gateway
/// has measured them at.
///
/// A card can stand for several listings (an account, the shared bucket), and
/// the two halves are pooled differently on purpose. Documentation is summed —
/// every priced endpoint behind the card counts, so an account cannot hide a
/// thin listing behind a well-documented one. Reliability is pooled by
/// ATTEMPTS rather than averaged across listings, because a listing called
/// twice and a listing called ten thousand times are not two equal opinions.
fn rate_card(
    provider: &crate::db::ProviderSummary,
    stats: Option<&std::collections::HashMap<String, crate::db::ProviderStats>>,
) -> Option<crate::quality::CardQuality> {
    let mut recent_attempts: i64 = 0;
    let mut recent_successes: i64 = 0;
    if let Some(stats) = stats {
        for slug in &provider.slugs {
            if let Some(row) = stats.get(slug) {
                recent_attempts += row.recent_attempts;
                recent_successes += row.recent_successes;
            }
        }
    }

    crate::quality::rate(&crate::quality::CardEvidence {
        paid_endpoints: provider.paid_endpoints,
        with_inputs: provider.endpoints_with_inputs,
        with_required: provider.endpoints_with_required,
        with_description: provider.endpoints_with_description,
        has_mark: provider
            .avatar_url
            .as_deref()
            .is_some_and(|url| !url.trim().is_empty()),
        // Absent, not zero: nobody having called this lately is an absence of
        // evidence, and `quality::rate` is built to leave it alone.
        recent_success_rate: (recent_attempts > 0)
            .then(|| recent_successes as f64 / recent_attempts as f64),
    })
}

/// `GET /v1/providers` — the provider directory. Browsers get the
/// logo/name/bio grid (see `render_provider_list_html`); JSON callers get
/// the same data as a flat list, since agents may want to enumerate
/// providers rather than individual listings.
pub async fn list_providers(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Query(params): Query<ProviderListQuery>,
) -> Result<Response, StatusCode> {
    let mut mapped = provider_directory(&state, params.search.as_deref()).await?;
    if params.sort.as_deref() == Some("name") {
        mapped.sort_by_key(|a| a.name.to_lowercase());
    }
    let total = mapped.len() as i64;
    let limit = params.limit.clamp(1, 500) as usize;
    let offset = params.offset.max(0) as usize;
    let mapped: Vec<_> = mapped.into_iter().skip(offset).take(limit).collect();

    if is_browser_request(&headers) {
        let html = render_provider_list_html(&mapped, params.search.as_deref());
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/html; charset=utf-8")
            .body(Body::from(html))
            .unwrap());
    }

    Ok(Json(ProviderListResponse {
        providers: mapped,
        total,
        search: params.search,
    })
    .into_response())
}

/// `GET /v1/providers/:username` — one provider's active, visible APIs.
/// 404s if the username has no visible listings (matching how the rest of
/// the catalog treats "exists but nothing to show" — no partial profiles
/// leak through the browse flow).
pub async fn get_provider_apis(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(username): Path<String>,
    Query(params): Query<ProviderApiListQuery>,
) -> Result<Response, StatusCode> {
    let sort = match params.sort.as_str() {
        "name" | "price" | "newest" => params.sort.as_str(),
        _ => "name",
    };
    let order = match params.order.as_str() {
        "asc" | "desc" => params.order.as_str(),
        _ => "asc",
    };
    let limit = params.limit.clamp(1, 500);
    let offset = params.offset.max(0);

    let (apis, counts) = state
        .db
        .list_active_apis_by_provider(
            &username,
            clean_filter(params.search.as_deref()),
            sort,
            order,
            limit,
            offset,
        )
        .await
        .map_err(|err| {
            tracing::error!("Failed to list provider APIs: {}", err);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let total = counts.total;

    if apis.is_empty() && total == 0 && params.search.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }

    let provider = apis
        .first()
        .and_then(|api| {
            api.provider_name.as_ref().map(|name| ProviderInfoResponse {
                name: name.clone(),
                username: api
                    .provider_username
                    .clone()
                    .unwrap_or_else(|| username.clone()),
                avatar_url: api.provider_avatar_url.clone(),
                bio: api.provider_bio.clone(),
                sector: api.provider_sector.clone(),
                api_count: total,
                service_count: counts.services,
                federated_count: counts.federated,
                managed_count: counts.managed,
                // The page the reader is already on: nothing to open.
                listing_slug: None,
                // The stars belong to the directory CARD, where a reader is
                // choosing between sources. This is the header of the page
                // they already chose, and it is built from a different query
                // that does not count endpoint documentation — rating it here
                // would mean a second, differently-derived number for the same
                // provider. Absent instead.
                quality: None,
            })
        })
        .unwrap_or_else(|| ProviderInfoResponse {
            // Listings with no provider profile are grouped under the synthetic
            // community handle, which carries a display name of its own rather
            // than echoing the raw URL segment back at the reader.
            name: if username.eq_ignore_ascii_case(crate::db::UNAFFILIATED_PROVIDER_USERNAME) {
                crate::db::UNAFFILIATED_PROVIDER_NAME.to_string()
            } else {
                username.clone()
            },
            username: username.clone(),
            avatar_url: None,
            bio: None,
            sector: None,
            api_count: total,
            service_count: counts.services,
            quality: None,
            federated_count: counts.federated,
            managed_count: counts.managed,
            listing_slug: None,
        });

    let mapped: Vec<ApiInfoResponse> = apis.into_iter().map(map_api_info).collect();
    let returned = mapped.len();

    if is_browser_request(&headers) {
        // A provider with a single API has no meaningful "pick an API" step,
        // so the provider page would just show one lonely card. Send the
        // browser straight to that API's detail page instead. Only when
        // unfiltered — a search that happens to narrow to one result should
        // still render the provider view so the user can clear/adjust it.
        if total == 1 && params.search.is_none() {
            if let Some(api) = mapped.first() {
                return Ok(Response::builder()
                    .status(StatusCode::FOUND)
                    .header("Location", format!("/v1/apis/{}", api.slug))
                    .body(Body::empty())
                    .unwrap());
            }
        }

        let html = render_provider_apis_html(
            &provider,
            &mapped,
            total,
            limit,
            offset,
            params.search.as_deref(),
        );
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/html; charset=utf-8")
            .body(Body::from(html))
            .unwrap());
    }

    Ok(Json(ProviderApisResponse {
        provider,
        apis: mapped,
        meta: ApiListMeta {
            total,
            returned,
            limit,
            offset,
            search: params.search,
            category: None,
            tag: None,
            listing_type: None,
            min_price: None,
            max_price: None,
            created_after: None,
            created_before: None,
            sort: sort.to_string(),
            order: order.to_string(),
            ranking: crate::db::RANKING_NOTE,
        },
    })
    .into_response())
}

pub async fn get_api_detail(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(slug): Path<String>,
) -> Result<Response, StatusCode> {
    let api = state
        .db
        .get_api_detail(&slug)
        .await
        .map_err(|err| {
            tracing::error!("Failed to get API detail: {}", err);
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    let endpoints = state
        .db
        .list_endpoints_with_docs(api.id)
        .await
        .map_err(|err| {
            tracing::error!("Failed to list endpoints: {}", err);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let gateway_base = format!("https://gateway.apiosk.com/{}", api.slug);
    // Federated listings are served through the gateway like every other one.
    // This used to hand out the provider's own URL for a federated listing,
    // which published a route around the buyer's 15%: call the provider
    // directly, pay list price, Apiosk never sees the transaction. The class
    // decides what Apiosk pays the provider, never what the buyer is told.
    let provider_class = crate::fees::ProviderClass::from_listing_type(api.listing_type.as_deref());
    let endpoint_docs: Vec<EndpointDocResponse> = endpoints
        .into_iter()
        .map(|endpoint| EndpointDocResponse {
            candidate_id: Some(endpoint.id),
            method: endpoint.method.clone(),
            path: endpoint.path.clone(),
            full_url: format!("{}{}", gateway_base, endpoint.path),
            description: endpoint.description,
            // The buyer price: list + 15%. `endpoint.price` is the provider's
            // list price and is what Apiosk settles against, not what the buyer
            // is quoted — publishing it here would under-quote every call.
            price_usd: crate::fees::quote_usdc(&endpoint.price, provider_class)
                .buyer_total_usdc()
                .to_string()
                .parse()
                .unwrap_or(0.0),
            payment_required: endpoint.payment_required,
            request_body: endpoint.request_body,
            response_body: endpoint.response_body,
            request_example: endpoint.request_example,
            response_example: endpoint.response_example,
            headers: endpoint.headers,
            query_params: endpoint.query_params,
            documentation_markdown: endpoint.documentation_markdown,
            external_docs_url: endpoint.external_docs_url,
            openapi_url: endpoint.openapi_url,
            authentication_notes: endpoint.authentication_notes,
            pricing_rules: endpoint.pricing_rules,
        })
        .collect();
    let metadata_operations: Vec<AgentMetadataOperation> = endpoint_docs
        .iter()
        .map(|endpoint| AgentMetadataOperation {
            id: Some(endpoint.path.clone()),
            method: endpoint.method.clone(),
            path: endpoint.path.clone(),
            price_usd: endpoint.price_usd,
            description: endpoint.description.clone(),
            input_schema: endpoint.request_body.clone(),
            output_schema: endpoint.response_body.clone(),
            payment_required: endpoint.payment_required,
        })
        .collect();
    let listing_metadata = normalize_listing_metadata(AgentMetadataContext {
        slug: &api.slug,
        name: &api.name,
        description: &api.description,
        category: api.category.as_deref(),
        listing_type: api.listing_type.as_deref(),
        origin_url: &api.origin_url,
        docs_url: api.docs_url.as_deref(),
        raw_metadata: &api.listing_metadata,
        default_price_usd: endpoint_docs.first().map(|endpoint| endpoint.price_usd),
        operations: &metadata_operations,
    });

    let operations: Vec<super::models::OperationInfo> = endpoint_docs
        .iter()
        .map(|endpoint| {
            let trimmed = endpoint.path.trim_start_matches('/');
            let id = if trimmed.is_empty() {
                // Root path "/" has no natural id; fall back to the verb so
                // the agent can disambiguate GET / vs POST / on the same API.
                endpoint.method.to_ascii_lowercase()
            } else {
                trimmed.to_string()
            };
            super::models::OperationInfo {
                id,
                method: endpoint.method.clone(),
                path: endpoint.path.clone(),
                price_usd: endpoint.price_usd,
                payment_required: endpoint.payment_required,
                description: endpoint.description.clone(),
                input_schema: endpoint.request_body.clone(),
                query_params: endpoint.query_params.clone(),
                pricing_rules: published_pricing_rules(&endpoint.pricing_rules),
                output_schema: endpoint.response_body.clone(),
            }
        })
        .collect();
    let default_price_usd = operations
        .first()
        .map(|operation| operation.price_usd)
        .unwrap_or(0.0);
    let listing_quality = if looks_like_test_listing(&api.slug, &api.name, &api.description) {
        "test".to_string()
    } else {
        "production".to_string()
    };

    // S1: never expose injected upstream credentials / signing secrets to the
    // public, unauthenticated catalog. Secrets are still injected on the forward
    // path from the raw DB row (see proxy::handle_flow).
    let listing_metadata = public_listing_metadata(&listing_metadata);

    let is_federated = provider_class == crate::fees::ProviderClass::Federated;

    // Every listing is called through the gateway now, so every listing has a
    // gateway base URL. This used to blank it for a federated listing and send
    // the caller to the provider instead — the bypass the mask closes.
    //
    // The price is still derived from the provider's own accepts[] for a
    // federated listing (the importer does not fill the endpoint price column),
    // but it is published as the BUYER price: list + 15%, the same rule a
    // managed listing follows.
    let gateway_base_url = gateway_base.clone();
    let price_usd = if is_federated {
        // A federated listing has no api_endpoints rows, so `default_price_usd`
        // is 0 and the price comes from the provider's own accepts[], which is
        // the RAW list price — apply the buyer fee exactly once here.
        let raw = federated_min_price_usd(&api.external_x402).unwrap_or(default_price_usd);
        crate::fees::quote((raw * 1_000_000.0).round() as i128, provider_class)
            .buyer_total_usdc()
            .to_string()
            .parse()
            .unwrap_or(raw)
    } else {
        // `default_price_usd` is the first operation's price, and operations are
        // already published at the buyer total (list + 15%). Re-quoting it
        // applied the markup a SECOND time (0.05 -> 0.0575 -> 0.066125); it is already
        // the buyer price, so use it as-is.
        default_price_usd
    };

    // A federated detail view is one of the few signals Apiosk has that it is
    // sending this provider demand. Detached and fail-open; see
    // `crate::federated_traffic`.
    if is_federated {
        crate::federated_traffic::bump(
            &state,
            vec![api.slug.clone()],
            crate::federated_traffic::FederatedEvent::Detail,
            crate::federated_traffic::ua_class(&headers),
        );
    }

    // The listing owner's public profile (logo/name/bio) for the detail page.
    // Best-effort: a DB error or missing profile just omits the provider block.
    let provider = state
        .db
        .get_provider_public(&slug)
        .await
        .ok()
        .flatten()
        .map(|(display_name, username, avatar_url, bio, sector)| {
            let name = display_name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .or_else(|| username.clone())
                .unwrap_or_else(|| "Provider".to_string());
            super::models::ProviderPublic {
                name,
                username,
                avatar_url,
                bio,
                sector,
            }
        });

    let response = ApiDetailResponse {
        id: api.id.to_string(),
        slug: api.slug.clone(),
        name: api.name.clone(),
        description: api.description.clone(),
        long_description: api.long_description.clone(),
        category: api.category.clone(),
        listing_type: api.listing_type.clone(),
        docs_url: api.docs_url.clone(),
        gateway_base_url,
        example_usage: api.example_usage.clone(),
        authentication: api.authentication_info.clone(),
        active: api.active.unwrap_or(true),
        verified: api.is_verified.unwrap_or(false),
        listing_quality,
        price_usd,
        operations,
        listing_metadata,
        created_at: api.created_at.to_rfc3339(),
        endpoints: endpoint_docs,
        // The gateway hosts every listing, federated included. Reporting `true`
        // here is what told agents to go around us: the field is documented as
        // "call external_resources[].resource on the provider's own host and pay
        // them directly".
        hosted_externally: false,
        // Deliberately empty. These are the provider's raw x402 resources —
        // their origin, their payTo and their list price. Publishing them is the
        // same bypass as publishing the URL, just one level down.
        external_resources: serde_json::json!([]),
        provider,
    };

    if is_browser_request(&headers) {
        let html = render_api_detail_html(&response);
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/html; charset=utf-8")
            .body(Body::from(html))
            .unwrap());
    }

    Ok(Json(response).into_response())
}

pub async fn list_types(headers: axum::http::HeaderMap) -> Response {
    let response = TypesResponse {
        gateway: "apiosk-gateway".to_string(),
        supported_types: vec![
            TypeInfo {
                key: "api".to_string(),
                description: "All verified active APIs".to_string(),
                endpoint: "/types/api/v1".to_string(),
            },
            TypeInfo {
                key: "dataset".to_string(),
                description: "Datasets and queryable data products".to_string(),
                endpoint: "/types/dataset/v1".to_string(),
            },
            TypeInfo {
                key: "service".to_string(),
                description: "Webhook-backed services and async actions".to_string(),
                endpoint: "/types/service/v1".to_string(),
            },
            TypeInfo {
                key: "connector".to_string(),
                description:
                    "Paid relays into third-party platforms like GitHub, Slack, and Notion"
                        .to_string(),
                endpoint: "/types/connector/v1".to_string(),
            },
            TypeInfo {
                key: "skill".to_string(),
                description: "Agent-native tools and MCP-style skills".to_string(),
                endpoint: "/types/skill/v1".to_string(),
            },
            TypeInfo {
                key: "product".to_string(),
                description: "Digital products and downloadable assets".to_string(),
                endpoint: "/types/product/v1".to_string(),
            },
        ],
        examples: vec![
            "GET /types".to_string(),
            "GET /types/api/v1".to_string(),
            "GET /types/dataset/v1?search=weather&limit=10".to_string(),
            "GET /types/connector/v1?search=github".to_string(),
            "GET /types/skill/v1?sort=price&order=asc".to_string(),
        ],
    };

    if is_browser_request(&headers) {
        let html = render_types_html(&response);
        return Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/html; charset=utf-8")
            .body(Body::from(html))
            .unwrap();
    }

    Json(response).into_response()
}

pub async fn list_by_type(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(listing_type): Path<String>,
    Query(params): Query<ApiListQuery>,
) -> Result<Response, StatusCode> {
    let listing_type = listing_type.to_lowercase();
    if !matches!(
        listing_type.as_str(),
        "api" | "dataset" | "service" | "connector" | "skill" | "product"
    ) {
        return Err(StatusCode::NOT_FOUND);
    }

    let sort = match params.sort.as_str() {
        "name" | "price" | "newest" => params.sort.as_str(),
        _ => "name",
    };
    let order = match params.order.as_str() {
        "asc" | "desc" => params.order.as_str(),
        _ => "asc",
    };
    let limit = params.limit.clamp(1, 500) as usize;
    let offset = params.offset.max(0) as usize;
    let created_after = parse_filter_datetime(params.created_after.as_deref(), false)?;
    let created_before = parse_filter_datetime(params.created_before.as_deref(), true)?;
    if params
        .min_price
        .zip(params.max_price)
        .is_some_and(|(min, max)| min > max)
    {
        return Err(StatusCode::BAD_REQUEST);
    }

    let (apis, _) = state
        .cache
        .get_active_apis(
            &state.db,
            clean_filter(params.search.as_deref()),
            clean_filter(params.category.as_deref()),
            clean_filter(params.tag.as_deref()),
            Some(listing_type.as_str()),
            params.min_price,
            params.max_price,
            created_after,
            created_before,
            // This route filters BY TYPE; a slug lookup belongs on /v1/apis.
            None,
            sort,
            order,
            5000,
            0,
        )
        .await
        .map_err(|err| {
            tracing::error!("Failed to list APIs by type: {}", err);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let mut filtered: Vec<ApiInfoResponse> = apis
        .into_iter()
        .filter(|api| api.listing_type == listing_type)
        .map(map_api_info)
        .collect();
    if !params.include_test {
        filtered.retain(|api| api.listing_quality != "test");
    }

    let total = filtered.len() as i64;
    let paged: Vec<ApiInfoResponse> = if offset >= filtered.len() {
        Vec::new()
    } else {
        let end = usize::min(offset + limit, filtered.len());
        filtered.drain(offset..end).collect()
    };
    let returned = paged.len();

    if is_browser_request(&headers) {
        let html = render_type_list_html(
            &listing_type,
            &paged,
            total,
            limit as i64,
            offset as i64,
            params.search.as_deref(),
        );
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/html; charset=utf-8")
            .body(Body::from(html))
            .unwrap());
    }

    Ok(Json(TypeListResponse {
        listing_type: listing_type.clone(),
        apis: paged,
        meta: ApiListMeta {
            total,
            returned,
            limit: limit as i64,
            offset: offset as i64,
            search: params.search,
            category: params.category,
            tag: params.tag,
            listing_type: Some(listing_type.clone()),
            min_price: params.min_price,
            max_price: params.max_price,
            created_after: params.created_after,
            created_before: params.created_before,
            sort: sort.to_string(),
            order: order.to_string(),
            ranking: crate::db::RANKING_NOTE,
        },
    })
    .into_response())
}

/// The parameter pricing rules, when there are any.
///
/// `api_endpoints.pricing_rules` is a JSON array that is empty for most
/// endpoints. Publishing `[]` on every operation would be noise, and
/// publishing `null` and `[]` interchangeably would make a consumer test for
/// both — so an endpoint priced by its default alone simply omits the field.
fn published_pricing_rules(rules: &serde_json::Value) -> Option<serde_json::Value> {
    match rules.as_array() {
        Some(items) if !items.is_empty() => Some(rules.clone()),
        _ => None,
    }
}

fn federated_min_price_usd(external_x402: &serde_json::Value) -> Option<f64> {
    let resources = external_x402.as_array()?;
    let mut min: Option<f64> = None;
    for res in resources {
        let Some(accepts) = res.get("accepts").and_then(|a| a.as_array()) else {
            continue;
        };
        for a in accepts {
            let raw = a.get("amount").or_else(|| a.get("maxAmountRequired"));
            // x402 `amount` is an atomic-unit integer string ("5000" = 0.005
            // USDC at 6 decimals). Some importers wrote the human decimal
            // instead ("0.005"), which must NOT be divided by 1e6 again — doing
            // so produced prices like 5e-9. A decimal point (or a non-integer
            // JSON number) marks the already-in-USDC case; a bare integer is
            // atomic and gets scaled down.
            let usd = match raw {
                Some(serde_json::Value::String(s)) => {
                    let s = s.trim();
                    s.parse::<f64>()
                        .ok()
                        .map(|v| if s.contains('.') { v } else { v / 1_000_000.0 })
                }
                Some(serde_json::Value::Number(n)) => {
                    n.as_f64()
                        .map(|v| if v.fract() != 0.0 { v } else { v / 1_000_000.0 })
                }
                _ => None,
            };
            if let Some(usd) = usd {
                min = Some(min.map_or(usd, |m| m.min(usd)));
            }
        }
    }
    min
}

fn map_api_info(api: crate::db::ApiInfo) -> ApiInfoResponse {
    let provider = api.provider_name.as_ref().map(|name| ProviderPublic {
        name: name.clone(),
        username: api.provider_username.clone(),
        avatar_url: api.provider_avatar_url.clone(),
        bio: api.provider_bio.clone(),
        sector: api.provider_sector.clone(),
    });
    let raw_category = api.category;
    let category = if raw_category == "compute" {
        "workflow".to_string()
    } else {
        raw_category.clone()
    };
    let endpoint_docs = parse_endpoint_docs(&api.slug, api.endpoint_docs);
    let metadata_operations: Vec<AgentMetadataOperation> = endpoint_docs
        .iter()
        .map(|endpoint| AgentMetadataOperation {
            id: Some(operation_id(&endpoint.method, &endpoint.path)),
            method: endpoint.method.clone(),
            path: endpoint.path.clone(),
            price_usd: endpoint.price_usd,
            description: endpoint.description.clone(),
            input_schema: endpoint.request_body.clone(),
            output_schema: endpoint.response_body.clone(),
            payment_required: endpoint.payment_required,
        })
        .collect();
    let operations: Vec<super::models::OperationInfo> = endpoint_docs
        .iter()
        .map(|endpoint| super::models::OperationInfo {
            id: operation_id(&endpoint.method, &endpoint.path),
            method: endpoint.method.clone(),
            path: endpoint.path.clone(),
            price_usd: endpoint.price_usd,
            payment_required: endpoint.payment_required,
            description: endpoint.description.clone(),
            input_schema: endpoint.request_body.clone(),
            query_params: endpoint.query_params.clone(),
            pricing_rules: published_pricing_rules(&endpoint.pricing_rules),
            output_schema: endpoint.response_body.clone(),
        })
        .collect();
    let listing_metadata = normalize_listing_metadata(AgentMetadataContext {
        slug: &api.slug,
        name: &api.name,
        description: &api.description,
        category: Some(&raw_category),
        listing_type: Some(&api.listing_type),
        origin_url: &api.origin_url,
        docs_url: Some(&api.docs_url),
        raw_metadata: &api.listing_metadata,
        default_price_usd: Some(api.price_usd.to_string().parse().unwrap_or(0.0)),
        operations: &metadata_operations,
    });

    let listing_quality = if looks_like_test_listing(&api.slug, &api.name, &api.description) {
        "test".to_string()
    } else {
        "production".to_string()
    };

    // S1: never expose injected upstream credentials / signing secrets to the
    // public, unauthenticated catalog list.
    let listing_metadata = public_listing_metadata(&listing_metadata);

    // Federated (external) listing: Apiosk indexes it but does not proxy or
    // charge for it. Blank the gateway URL so the catalog never advertises a
    // proxy route, surface the provider's resources, and derive a display price
    // from the provider's own accepts[] (the listing has no api_endpoints rows).
    // Keyed on listing_type — the authoritative federated discriminator.
    let hosted_externally = api.listing_type == "federated";
    // What the catalogue already computed: the cheapest paid endpoint on the
    // listing (MIN_PAID_PRICE in db/discovery.rs).
    let listed_price_usd: f64 = api.price_usd.to_string().parse().unwrap_or(0.0);
    let price_usd = if hosted_externally {
        // Falling back to the listing's OWN price rather than to zero.
        //
        // The comment above says a federated listing has no api_endpoints rows.
        // That held while every federated listing came from the importer, and
        // stopped holding the moment one was onboarded WITH its endpoints —
        // StableEnrich has thirty-six, priced from its own documentation, and a
        // `/.well-known/x402` that lists bare resource URLs with no `accepts[]`.
        // So `federated_min_price_usd` found nothing, and `unwrap_or(0.0)`
        // published a real, paid listing as free: "€0.00 per result, cheapest
        // service" on the provider card.
        //
        // Zero is the one value that is never a safe default for a price. It is
        // indistinguishable from a genuinely free service, it reads as a
        // promise, and it sorts to the top of `sort=price`.
        federated_min_price_usd(&api.external_x402).unwrap_or(listed_price_usd)
    } else {
        listed_price_usd
    };
    // The buyer total (list + 15%), from the single fee source, computed once
    // from the raw list price above. Additive — `price_usd` stays the raw list.
    let buyer_price_usd = crate::fees::quote(
        (price_usd * 1_000_000.0).round() as i128,
        crate::fees::ProviderClass::from_listing_type(Some(api.listing_type.as_str())),
    )
    .buyer_total_usdc()
    .to_string()
    .parse()
    .unwrap_or(price_usd);
    let gateway_url = if hosted_externally {
        String::new()
    } else {
        api.gateway_url
    };

    ApiInfoResponse {
        id: api.id,
        slug: api.slug,
        name: api.name,
        description: api.description,
        price_usd,
        buyer_price_usd,
        category,
        raw_category,
        implementation_type: api.implementation_type,
        model_backed: api.model_backed,
        listing_type: api.listing_type,
        docs_url: api.docs_url,
        origin_url: api.origin_url,
        gateway_url,
        active: api.active,
        verified: api.verified,
        endpoint_count: api.endpoint_count,
        operations,
        endpoints: endpoint_docs,
        listing_metadata,
        created_at: api.created_at.to_rfc3339(),
        listing_quality,
        hosted_externally,
        external_resources: api.external_x402,
        provider,
    }
}

fn map_provider_info(
    provider: crate::db::ProviderSummary,
    quality: Option<crate::quality::CardQuality>,
) -> ProviderInfoResponse {
    ProviderInfoResponse {
        name: provider.name,
        username: provider.username,
        avatar_url: provider.avatar_url,
        bio: provider.bio,
        sector: provider.sector,
        api_count: provider.api_count,
        service_count: provider.service_count,
        federated_count: provider.federated_count,
        managed_count: provider.managed_count,
        listing_slug: provider.listing_slug,
        quality,
    }
}

fn clean_filter(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn parse_filter_datetime(
    value: Option<&str>,
    end_of_day: bool,
) -> Result<Option<DateTime<Utc>>, StatusCode> {
    let Some(value) = clean_filter(value) else {
        return Ok(None);
    };

    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Ok(Some(parsed.with_timezone(&Utc)));
    }

    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        let time = if end_of_day {
            date.and_hms_opt(23, 59, 59)
        } else {
            date.and_hms_opt(0, 0, 0)
        };
        if let Some(naive) = time {
            return Ok(Some(Utc.from_utc_datetime(&naive)));
        }
    }

    Err(StatusCode::BAD_REQUEST)
}

fn parse_endpoint_docs(slug: &str, value: serde_json::Value) -> Vec<EndpointDocResponse> {
    serde_json::from_value::<Vec<EndpointDocResponse>>(value).unwrap_or_else(|err| {
        tracing::warn!("Failed to parse endpoint docs for {}: {}", slug, err);
        Vec::new()
    })
}

fn operation_id(method: &str, path: &str) -> String {
    let trimmed = path.trim_start_matches('/');
    if trimmed.is_empty() {
        method.to_ascii_lowercase()
    } else {
        trimmed.to_string()
    }
}

/// Heuristic — flag obvious sandbox / smoke / placeholder listings so a
/// "cheapest API" discovery query an agent makes doesn't pick them up.
/// Production listings that genuinely look like keyboard mashes can opt
/// back in via `?include_test=true`. Conservative on purpose: a listing
/// has to look obviously throwaway in TWO signals before we flag it.
///
/// Signals checked:
///   - slug is the same char repeated >= 5 times
///     ("aaaaa...", "zzzzz...", "11111...")
///   - slug, name or description starts with test/demo/sample/example/foo
///     /placeholder/lorem/sandbox (case-insensitive)
///   - description is empty or under 8 chars
pub(crate) fn looks_like_test_listing(slug: &str, name: &str, description: &str) -> bool {
    let slug_lower = slug.to_ascii_lowercase();
    let name_lower = name.to_ascii_lowercase();
    let desc_lower = description.trim().to_ascii_lowercase();

    let repeated_char =
        slug_lower.len() >= 5 && slug_lower.chars().all(|c| slug_lower.starts_with(c));

    let throwaway_prefixes = [
        "test",
        "demo",
        "sample",
        "example",
        "foo",
        "bar",
        "baz",
        "placeholder",
        "lorem",
        "sandbox",
        "tmp",
        "temp",
        "scratch",
    ];
    let any_throwaway = throwaway_prefixes.iter().any(|prefix| {
        slug_lower.starts_with(prefix)
            || name_lower.starts_with(prefix)
            || desc_lower.starts_with(prefix)
    });
    let tiny_desc = desc_lower.len() < 8;

    repeated_char || (any_throwaway && tiny_desc)
}

/// `GET /sitemap.xml` — the crawlable index of every browser-facing page, so
/// Google can discover the whole catalog rather than only the handful of pages
/// that happen to be linked. Built live from the database: the static browse
/// surfaces, one URL per active listing (`/v1/apis/{slug}`) and per provider
/// (`/v1/providers/{username}`). Absolute URLs are anchored to
/// [`crate::html::SITE_BASE_URL`] so the sitemap is host-independent.
///
/// A directory this size fits comfortably under the 50k-URL / 50MB sitemap
/// limits; listings are paged out of the DB 500 at a time and the walk stops at
/// a hard cap so a runaway catalog can never build an oversized document.
pub async fn sitemap_xml(State(state): State<Arc<AppState>>) -> Response {
    const BASE: &str = crate::html::SITE_BASE_URL;
    const PAGE: i64 = 500;
    const MAX_LISTINGS: usize = 45_000;

    let mut urls: Vec<String> = vec![
        format!("{BASE}/"),
        format!("{BASE}/v1/apis"),
        format!("{BASE}/v1/providers"),
        format!("{BASE}/types"),
        format!("{BASE}/federated/new"),
    ];
    for t in ["api", "dataset", "service", "connector", "skill", "product"] {
        urls.push(format!("{BASE}/types/{t}/v1"));
    }

    // Every capability gets its own URL. These are the comparison pages — the
    // one surface where two providers appear as alternatives — so leaving them
    // out of the sitemap meant the newest and most distinctive part of the
    // catalog was reachable only by following a link from a document a crawler
    // may never fetch. Failure costs the capability URLs, never the sitemap.
    urls.push(format!("{BASE}/v1/capabilities"));
    match state.db.list_capabilities().await {
        Ok(caps) => {
            for cap in caps {
                urls.push(format!("{BASE}/v1/capabilities/{}", xml_escape(&cap.slug)));
            }
        }
        Err(err) => tracing::error!("Sitemap capability list unavailable: {}", err),
    }

    // Every active listing, paged until the catalog is exhausted or the cap is
    // hit. A failed page is logged and ends the walk rather than 500-ing the
    // sitemap — a partial sitemap still indexes most of the catalog.
    let mut offset = 0i64;
    loop {
        match state
            .cache
            .get_active_apis(
                &state.db, None, None, None, None, None, None, None, None, None, "name", "asc",
                PAGE, offset,
            )
            .await
        {
            Ok((apis, _total)) => {
                let returned = apis.len();
                for api in apis {
                    let mapped = map_api_info(api);
                    if mapped.listing_quality == "test" {
                        continue;
                    }
                    urls.push(format!("{BASE}/v1/apis/{}", xml_escape(&mapped.slug)));
                }
                if returned < PAGE as usize || urls.len() >= MAX_LISTINGS {
                    break;
                }
                offset += PAGE;
            }
            Err(err) => {
                tracing::error!("sitemap: listing page at offset {} failed: {}", offset, err);
                break;
            }
        }
    }

    // One URL per provider. The directory is already a single cached query, so
    // this adds no per-provider round trips.
    match provider_directory(&state, None).await {
        Ok(providers) => {
            for p in providers {
                urls.push(format!("{BASE}/v1/providers/{}", xml_escape(&p.username)));
            }
        }
        Err(status) => {
            tracing::error!("sitemap: provider directory unavailable: {}", status);
        }
    }

    let mut body = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n",
    );
    for url in &urls {
        body.push_str("  <url><loc>");
        body.push_str(url);
        body.push_str("</loc></url>\n");
    }
    body.push_str("</urlset>\n");

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/xml; charset=utf-8")
        .header("Cache-Control", "public, max-age=3600")
        .body(Body::from(body))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

/// Minimal XML text escaping for values interpolated into `<loc>` — slugs and
/// usernames are normally URL-safe, but an `&` or angle bracket would otherwise
/// produce an invalid sitemap that Search Console rejects wholesale.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&apos;")
        .replace('"', "&quot;")
}

// `mod federated_call_target_tests` lived here. It tested resolving a federated
// endpoint back to the provider's own host — `federated_resource_for_path`,
// `normalized_route`, `origin_of` — so the detail page could hand a caller the
// provider's URL directly.
//
// That is the bypass the gateway mask closes: a published direct route lets a
// buyer pay the provider at list price and skip the 15% the hybrid fee model
// charges. The resolvers and their tests are gone with it. The replacement
// invariants — gateway URL, platform wallet, buyer price, no provider origin —
// are asserted in `well_known::tests` (`federated_items_are_masked_behind_the_gateway`
// and neighbours).

#[cfg(test)]
mod federated_price_tests {
    #[test]
    fn a_manifest_without_prices_yields_none_not_zero() {
        // StableEnrich publishes `{version, resources: ["https://…"]}` — bare
        // URLs, no `accepts[]`. Reading that as 0.0 published a paid listing as
        // free. None is the honest answer; the caller falls back to the price
        // the catalogue already computed.
        let bare = serde_json::json!({
            "version": 1,
            "resources": ["https://stableenrich.dev/api/exa/search"]
        });
        assert_eq!(super::federated_min_price_usd(&bare), None);
        assert_eq!(super::federated_min_price_usd(&serde_json::json!([])), None);
        assert_eq!(
            super::federated_min_price_usd(&serde_json::json!(null)),
            None
        );
    }

    #[test]
    fn a_manifest_with_prices_still_wins() {
        // The WebberSites shape must keep working unchanged: when the provider
        // states a price, that is the price.
        let priced = serde_json::json!([
            { "resource": "https://x/a", "accepts": [{ "maxAmountRequired": "1000" }] },
            { "resource": "https://x/b", "accepts": [{ "maxAmountRequired": "5000" }] }
        ]);
        assert_eq!(super::federated_min_price_usd(&priced), Some(0.001));
    }

    use super::federated_min_price_usd;
    use serde_json::json;

    fn accepts(amount: serde_json::Value) -> serde_json::Value {
        json!([{ "accepts": [{ "amount": amount }] }])
    }

    #[test]
    fn atomic_integer_string_is_scaled_down() {
        // "5000" atomic = 0.005 USDC
        assert_eq!(
            federated_min_price_usd(&accepts(json!("5000"))),
            Some(0.005)
        );
    }

    #[test]
    fn decimal_string_is_taken_as_usdc_not_divided_again() {
        // "0.005" is already USDC; dividing by 1e6 produced 5e-9 before the fix.
        assert_eq!(
            federated_min_price_usd(&accepts(json!("0.005"))),
            Some(0.005)
        );
    }

    #[test]
    fn integer_json_number_is_atomic() {
        assert_eq!(federated_min_price_usd(&accepts(json!(3000))), Some(0.003));
    }

    #[test]
    fn decimal_json_number_is_usdc() {
        assert_eq!(federated_min_price_usd(&accepts(json!(0.01))), Some(0.01));
    }

    #[test]
    fn missing_amount_yields_none() {
        assert_eq!(federated_min_price_usd(&json!([{ "accepts": [{}] }])), None);
    }
}
