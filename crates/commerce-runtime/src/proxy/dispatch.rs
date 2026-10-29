use super::*;

pub(super) async fn dispatch_non_api_listing(
    state: &Arc<AppState>,
    path: &str,
    method: &Method,
    browser_mode: bool,
    request: &mut Option<Request<Body>>,
    resolved: &ResolvedProxyRequest,
) -> Result<Option<Response<Body>>, Response<Body>> {
    let api = &resolved.api;

    let response = match resolved.listing_type {
        ListingType::Skill => {
            let skill = match state.db.get_skill_listing(api.id).await {
                Ok(Some(s)) => s,
                Ok(None) => {
                    if browser_mode {
                        return Ok(Some(branded_status_page(
                            StatusCode::NOT_FOUND,
                            "Skill not configured",
                            "This listing exists but has no skill configuration yet.",
                            &api.slug,
                            path,
                            method,
                            &[],
                            &[("Listing type", "skill".to_string())],
                            None,
                        )));
                    }
                    return Err(json_error_response(
                        StatusCode::NOT_FOUND,
                        "skill_not_configured",
                        "Skill listing is not configured",
                    ));
                }
                Err(e) => {
                    error!("DB error fetching skill: {}", e);
                    return Err(json_error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "skill_lookup_failed",
                        "Failed to load skill listing",
                    ));
                }
            };
            let result = handlers::skill::handle(
                &state.http_client,
                &skill,
                take_request_or_error(request, &api.slug, path, method, browser_mode)?,
                path,
            )
            .await;
            dispatch_result_to_response(result)
        }
        ListingType::Product => {
            let product = match state.db.get_product_listing(api.id).await {
                Ok(Some(p)) => p,
                Ok(None) => {
                    if browser_mode {
                        return Ok(Some(branded_status_page(
                            StatusCode::NOT_FOUND,
                            "Product not configured",
                            "This listing exists but has no product configuration yet.",
                            &api.slug,
                            path,
                            method,
                            &[],
                            &[("Listing type", "product".to_string())],
                            None,
                        )));
                    }
                    return Err(json_error_response(
                        StatusCode::NOT_FOUND,
                        "product_not_configured",
                        "Product listing is not configured",
                    ));
                }
                Err(e) => {
                    error!("DB error fetching product: {}", e);
                    return Err(json_error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "product_lookup_failed",
                        "Failed to load product listing",
                    ));
                }
            };
            let result =
                handlers::product::deliver(&state.http_client, &product, "unknown", "pending")
                    .await;
            dispatch_result_to_response(result)
        }
        ListingType::Dataset => {
            let dataset = match state.db.get_dataset_listing(api.id).await {
                Ok(Some(d)) => d,
                Ok(None) => {
                    if browser_mode {
                        return Ok(Some(branded_status_page(
                            StatusCode::NOT_FOUND,
                            "Dataset not configured",
                            "This listing exists but has no dataset configuration yet.",
                            &api.slug,
                            path,
                            method,
                            &[],
                            &[("Listing type", "dataset".to_string())],
                            None,
                        )));
                    }
                    return Err(json_error_response(
                        StatusCode::NOT_FOUND,
                        "dataset_not_configured",
                        "Dataset listing is not configured",
                    ));
                }
                Err(e) => {
                    error!("DB error fetching dataset: {}", e);
                    return Err(json_error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "dataset_lookup_failed",
                        "Failed to load dataset listing",
                    ));
                }
            };
            let result = handlers::dataset::handle(
                &state.http_client,
                &dataset,
                take_request_or_error(request, &api.slug, path, method, browser_mode)?,
                path,
            )
            .await;
            dispatch_result_to_response(result)
        }
        ListingType::Service => {
            let service = match state.db.get_service_listing(api.id).await {
                Ok(Some(s)) => s,
                Ok(None) => {
                    if browser_mode {
                        return Ok(Some(branded_status_page(
                            StatusCode::NOT_FOUND,
                            "Service not configured",
                            "This listing exists but has no service configuration yet.",
                            &api.slug,
                            path,
                            method,
                            &[],
                            &[("Listing type", "service".to_string())],
                            None,
                        )));
                    }
                    return Err(json_error_response(
                        StatusCode::NOT_FOUND,
                        "service_not_configured",
                        "Service listing is not configured",
                    ));
                }
                Err(e) => {
                    error!("DB error fetching service: {}", e);
                    return Err(json_error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "service_lookup_failed",
                        "Failed to load service listing",
                    ));
                }
            };
            let result = handlers::service::handle(
                &state.http_client,
                &service,
                take_request_or_error(request, &api.slug, path, method, browser_mode)?,
            )
            .await;
            dispatch_result_to_response(result)
        }
        ListingType::Connector => {
            let connector =
                match state.db.get_connector_listing(api.id).await {
                    Ok(Some(c)) => c,
                    Ok(None) => {
                        if let Some(connector) = resolved.listing.as_ref().and_then(|l| {
                            connector_from_listing_metadata(api.id, &l.listing_metadata)
                        }) {
                            connector
                        } else {
                            if browser_mode {
                                return Ok(Some(branded_status_page(
                                    StatusCode::NOT_FOUND,
                                    "Connector not configured",
                                    "This listing exists but has no connector configuration yet.",
                                    &api.slug,
                                    path,
                                    method,
                                    &[],
                                    &[("Listing type", "connector".to_string())],
                                    None,
                                )));
                            }
                            return Err(json_error_response(
                                StatusCode::NOT_FOUND,
                                "connector_not_configured",
                                "Connector listing is not configured",
                            ));
                        }
                    }
                    Err(e) => {
                        error!("DB error fetching connector: {}", e);
                        return Err(json_error_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "connector_lookup_failed",
                            "Failed to load connector listing",
                        ));
                    }
                };
            let result = handlers::connector::handle(
                &state.http_client,
                &connector,
                take_request_or_error(request, &api.slug, path, method, browser_mode)?,
                path,
            )
            .await;
            dispatch_result_to_response(result)
        }
        ListingType::Api => return Ok(None),
        // Federated listings are proxied like every other listing now; the
        // resell leg pays the provider's own 402 on forward. Falling through to
        // the proxy is the intended behaviour, not a gap.
        ListingType::Federated => return Ok(None),
    };

    Ok(Some(response))
}
