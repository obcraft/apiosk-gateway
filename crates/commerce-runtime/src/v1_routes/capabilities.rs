//! `/v1/capabilities` — the catalog grouped by the task, not by the seller.
//!
//! `/v1/apis` answers "which listings exist". This answers "who can do this
//! job, and what does each of them charge for it" — the first surface on which
//! two providers appear as alternatives to one another.
//!
//! What it deliberately does NOT do is rank the candidates by quality. Price
//! and payment terms are known per offer; measured latency, success rate and
//! result quality are not (the raw request log has status and latency, but
//! nothing aggregates it per provider, and federated listings never touch the
//! gateway at all). An ordered list reads as a recommendation whether or not
//! one is intended, so every response states which dimensions it compared and
//! which it could not — see [`comparability`].

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::db::{CapabilityInfo, CapabilityOffer, CapabilitySort};
use crate::html::{escape_html, format_price, html_footer, html_head, is_browser_request, Nav};
use crate::AppState;

const GATEWAY_BASE: &str = "https://gateway.apiosk.com";

#[derive(Debug, Deserialize)]
pub struct CapabilityOffersQuery {
    /// `price` (default) or `managed_first`. See [`CapabilitySort`].
    pub sort: Option<String>,
    /// Drop candidates above this listed price, so an agent can bind its own
    /// budget before it reads the list rather than after.
    pub max_price: Option<f64>,
    /// Also search the wider x402 ecosystem for offers Apiosk has not
    /// catalogued, returned separately under `external_candidates`.
    ///
    /// Off by default, and deliberately so. The default response is the
    /// reviewed set: every entry hand-checked, every entry carrying an input
    /// mapping that makes it substitutable. Turning this on trades that for
    /// reach, and the response says which half is which.
    #[serde(default, deserialize_with = "super::search::lenient_bool")]
    pub include_external: bool,
}

/// What this surface did and did not compare, published on every response.
///
/// The list of gaps is as load-bearing as the list of candidates: an agent that
/// assumes an unmeasured dimension was considered will read a price ordering as
/// a reliability ordering.
fn comparability() -> Value {
    json!({
        "compared": [
            "price_usd — the listed price of the operation. For an endpoint with parameter-conditional pricing the live 402 challenge for the exact request stays authoritative.",
            "payment — settlement rail and whether the call is proxied by Apiosk or paid directly to the provider.",
            "inputs — whether the offer accepts every input the capability defines, via the per-offer input mapping."
        ],
        "not_measured_yet": [
            "latency — measured and published on GET /v1/search for listings with enough successful calls (35 today). Absent here, and absent everywhere for federated listings, which are never proxied and so never timed.",
            "reliability — measured and published on GET /v1/search as served-over-attempted, counting 2xx against 2xx plus 5xx. Payment challenges and caller errors are excluded because neither is the provider's to answer for.",
            "quality — nothing measures whether a result was actually good.",
            "terms — provider rate limits, jurisdiction, data freshness, licensing and SLA are not machine-readable."
        ],
        "assignment": "Offers are assigned to a capability by hand-verified review (source=curated) or by provider declaration (source=declared). Inferred assignments exist as a review queue and are never served: a wrong candidate costs a purchase, not just a comparison."
    })
}

fn capability_json(info: &CapabilityInfo) -> Value {
    json!({
        "slug": info.slug,
        "name": info.name,
        "description": info.description,
        "category": info.category,
        "input_contract": info.input_contract,
        "output_contract": info.output_contract,
        "offers": {
            "count": info.offer_count,
            "listings": info.listing_count,
            "price_usd": {
                "min": info.min_price_usd,
                "max": info.max_price_usd,
            },
        },
        "compare_url": format!("{GATEWAY_BASE}/v1/capabilities/{}", info.slug),
    })
}

/// Names of the capability inputs this offer cannot take.
///
/// Read off `input_contract.required` minus the keys the offer maps. A
/// non-empty list is the honest statement that the offer is only a partial
/// substitute — `twelve-data GET /exchange_rate` takes the currency pair as one
/// symbol, so it cannot accept the capability's separate `base`/`quote`.
pub(super) fn missing_inputs(contract: &Value, input_map: &Value) -> Vec<String> {
    let Some(required) = contract.get("required").and_then(Value::as_array) else {
        return Vec::new();
    };
    required
        .iter()
        .filter_map(Value::as_str)
        .filter(|name| input_map.get(*name).is_none())
        .map(str::to_string)
        .collect()
}

fn offer_json(offer: &CapabilityOffer, contract: &Value) -> Value {
    let missing = missing_inputs(contract, &offer.input_map);
    json!({
        "api_slug": offer.api_slug,
        "api_name": offer.api_name,
        "provider": {
            "name": offer.provider_name,
            "username": offer.provider_username,
        },
        "method": offer.endpoint_method,
        "path": offer.endpoint_path,
        "description": offer.endpoint_description,
        "price_usd": offer.price_usd,
        "payment_required": offer.payment_required,
        // Every listing is proxied and settled through the gateway now, so
        // `call_url` is always a gateway URL and settlement is always Apiosk's.
        // These two fields used to say "direct-to-provider" for a federated
        // listing, which told the agent to pay the provider and skip the 15%.
        "hosted_externally": false,
        "settlement": "apiosk-proxied",
        "call_url": offer.call_url,
        "callable": offer.call_url.is_some(),
        "inputs": offer.input_map,
        "fixed_params": offer.fixed_params,
        "accepts_all_inputs": missing.is_empty(),
        "missing_inputs": missing,
        "notes": offer.notes,
        "assignment": {
            "source": offer.source,
            "confidence": offer.confidence,
        },
    })
}

/// `GET /v1/capabilities` — the taxonomy, with what the catalog can serve.
pub async fn list_capabilities(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Result<Response, StatusCode> {
    let capabilities = state.db.list_capabilities().await.map_err(|err| {
        tracing::error!("Failed to list capabilities: {}", err);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if is_browser_request(&headers) {
        return Ok(html_response(render_capability_list(&capabilities)));
    }

    Ok(Json(json!({
        "capabilities": capabilities.iter().map(capability_json).collect::<Vec<_>>(),
        "meta": {
            "total": capabilities.len(),
            "comparability": comparability(),
        },
    }))
    .into_response())
}

/// `GET /v1/capabilities/{slug}` — the candidates for one task.
pub async fn get_capability(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(slug): Path<String>,
    Query(params): Query<CapabilityOffersQuery>,
) -> Result<Response, StatusCode> {
    let Some(info) = state.db.get_capability(&slug).await.map_err(|err| {
        tracing::error!("Failed to load capability {}: {}", slug, err);
        StatusCode::INTERNAL_SERVER_ERROR
    })?
    else {
        return Err(StatusCode::NOT_FOUND);
    };

    let sort = CapabilitySort::parse(params.sort.as_deref());
    let offers = state
        .db
        .list_capability_offers(&info.slug, &sort, params.max_price)
        .await
        .map_err(|err| {
            tracing::error!("Failed to load offers for {}: {}", info.slug, err);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if is_browser_request(&headers) {
        return Ok(html_response(render_capability_detail(
            &info, &offers, &sort,
        )));
    }

    let candidates: Vec<Value> = offers
        .iter()
        .map(|o| offer_json(o, &info.input_contract))
        .collect();

    // What the agent was shown, recorded before it decides. Detached and
    // crawler-skipping; see `crate::decision_record`. `None` means nothing was
    // recorded (a crawler), and the response says so by omitting the id rather
    // than inventing one.
    let comparison_id = crate::decision_record::record_comparison(
        &state,
        &headers,
        &info.slug,
        if matches!(sort, CapabilitySort::ManagedFirst) {
            "managed_first"
        } else {
            "price"
        },
        params.max_price,
        &candidates,
        headers
            .get(crate::request_context::REQUEST_ID_HEADER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| uuid::Uuid::parse_str(v.trim()).ok()),
    );

    // Unverified reach, only when asked for. Hosts already in the candidate
    // list are excluded inside the search: an external "alternative" that is
    // the same provider Apiosk already curated is not an alternative.
    let external = if params.include_external {
        let known: Vec<String> = offers
            .iter()
            .filter_map(|o| o.call_url.as_deref())
            .filter_map(|u| u.split("://").nth(1))
            .filter_map(|rest| rest.split('/').next())
            .map(|h| h.to_ascii_lowercase())
            .collect();
        let variants = crate::bazaar::query_variants(
            &info.name,
            &info.description,
            info.category.as_deref(),
            &info.slug,
        );
        crate::sources::sweep(&state, &variants, &known).await
    } else {
        Vec::new()
    };

    let prices: Vec<f64> = offers.iter().map(|o| o.price_usd).collect();
    let min = prices.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = prices.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

    Ok(Json(json!({
        "capability": capability_json(&info),
        "candidates": candidates,
        "meta": {
            "total": offers.len(),
            "sort": if matches!(sort, CapabilitySort::ManagedFirst) { "managed_first" } else { "price" },
            "max_price": params.max_price,
            "ranking": sort.note(),
            "price_usd": if prices.is_empty() {
                json!(null)
            } else {
                json!({
                    "min": min,
                    "max": max,
                    // The number that makes the case for this surface existing:
                    // how much more the dearest candidate costs than the cheapest
                    // for the same job.
                    "spread_multiple": if min > 0.0 { json!((max / min * 100.0).round() / 100.0) } else { json!(null) },
                })
            },
            "comparability": comparability(),
            "external_candidates": {
                "requested": params.include_external,
                "count": external.len(),
                "offers": crate::sources::settled_json(&state, &external).await,
                "source": crate::sources::reach_sentence(&state).await,
                "sources_swept": crate::sources::source_ids(&state).await,
                "why_separate": "These are unreviewed search hits with provider-declared contracts and no measured reliability. They are excluded from the price spread above. Each offer states its payment eligibility and indicative prices; purchasing still requires a usable input contract and a validated quote. Treat them as leads to evaluate before purchase.",
                "hint": if params.include_external { Value::Null } else { json!("Pass include_external=1 to search the wider x402 ecosystem for offers Apiosk has not catalogued yet.") },
            },
            // Quote this back on POST /v1/decisions so the choice is tied to
            // the menu it was made from. Absent when nothing was recorded.
            "comparison_id": comparison_id,
            "report_decision": {
                "url": "https://gateway.apiosk.com/v1/decisions",
                "method": "POST",
                "body": "{\"comparison_id\", \"capability\", \"chosen_api_slug\", optional \"reason\", \"policy\", \"rejected\"}",
                "why": "Nothing else records which alternatives you saw and rejected. Reporting is free, unauthenticated and never affects payment."
            },
        },
    }))
    .into_response())
}

fn html_response(body: String) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/html; charset=utf-8")
        .body(Body::from(body))
        .unwrap()
}

fn render_capability_list(capabilities: &[CapabilityInfo]) -> String {
    let mut html = html_head(
        "Capabilities — Apiosk",
        Nav::Apis,
        "Every task the Apiosk catalog can perform, with how many providers offer it and what they charge.",
        "/v1/capabilities",
    );
    html.push_str("<main class=\"container\"><h1>Capabilities</h1>");
    html.push_str("<p>The catalog grouped by the job to be done rather than by the seller. Price and payment terms are compared; latency, reliability and result quality are not measured yet.</p>");
    html.push_str("<table><thead><tr><th>Capability</th><th>Offers</th><th>Price range (USD)</th></tr></thead><tbody>");

    for cap in capabilities {
        let price = match (cap.min_price_usd, cap.max_price_usd) {
            (Some(min), Some(max)) if (max - min).abs() > f64::EPSILON => {
                format!("{} – {}", format_price(min), format_price(max))
            }
            (Some(min), _) => format_price(min),
            _ => "—".to_string(),
        };
        html.push_str(&format!(
            "<tr><td><a href=\"/v1/capabilities/{slug}\"><strong>{name}</strong></a><br><small>{desc}</small></td><td>{offers} from {listings} listing(s)</td><td>{price}</td></tr>",
            slug = escape_html(&cap.slug),
            name = escape_html(&cap.name),
            desc = escape_html(&cap.description),
            offers = cap.offer_count,
            listings = cap.listing_count,
        ));
    }

    html.push_str("</tbody></table></main>");
    html.push_str(html_footer());
    html
}

fn render_capability_detail(
    info: &CapabilityInfo,
    offers: &[CapabilityOffer],
    sort: &CapabilitySort,
) -> String {
    let mut html = html_head(
        &format!("{} — Apiosk capabilities", info.name),
        Nav::Apis,
        &info.description,
        &format!("/v1/capabilities/{}", info.slug),
    );
    html.push_str(&format!(
        "<main class=\"container\"><h1>{}</h1><p>{}</p>",
        escape_html(&info.name),
        escape_html(&info.description)
    ));
    html.push_str(&format!(
        "<p><small>{}</small></p>",
        escape_html(sort.note())
    ));
    html.push_str("<table><thead><tr><th>Provider</th><th>Operation</th><th>Price</th><th>Settlement</th><th>Notes</th></tr></thead><tbody>");

    for offer in offers {
        let missing = missing_inputs(&info.input_contract, &offer.input_map);
        let mut notes = offer.notes.clone().unwrap_or_default();
        if !missing.is_empty() {
            if !notes.is_empty() {
                notes.push(' ');
            }
            notes.push_str(&format!("Does not accept: {}.", missing.join(", ")));
        }
        html.push_str(&format!(
            "<tr><td><a href=\"/v1/apis/{slug}\">{name}</a></td><td><code>{method} {path}</code></td><td>{price}</td><td>{settlement}</td><td><small>{notes}</small></td></tr>",
            slug = escape_html(&offer.api_slug),
            name = escape_html(&offer.api_name),
            method = escape_html(&offer.endpoint_method),
            path = escape_html(&offer.endpoint_path),
            price = format_price(offer.price_usd),
            settlement = "via Apiosk",
            notes = escape_html(&notes),
        ));
    }

    html.push_str("</tbody></table>");
    html.push_str("<p><small>Compared: listed price, settlement rail and input compatibility. Not measured yet: latency, reliability, result quality, and provider terms such as rate limits, jurisdiction and licensing.</small></p>");
    html.push_str("</main>");
    html.push_str(html_footer());
    html
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract() -> Value {
        json!({"type":"object","required":["base","quote"],"properties":{}})
    }

    #[test]
    fn missing_inputs_names_what_an_offer_cannot_take() {
        let mapped = json!({"base": {"in": "query", "name": "source"}});
        assert_eq!(
            missing_inputs(&contract(), &mapped),
            vec!["quote".to_string()]
        );
    }

    #[test]
    fn a_fully_mapped_offer_reports_nothing_missing() {
        let mapped = json!({
            "base":  {"in": "query", "name": "source"},
            "quote": {"in": "query", "name": "currencies"}
        });
        assert!(missing_inputs(&contract(), &mapped).is_empty());
    }

    #[test]
    fn an_unmapped_offer_reports_every_required_input() {
        // twelve-data /exchange_rate takes the pair as one symbol, so it maps
        // nothing — the agent must be told, not left to infer it from silence.
        assert_eq!(missing_inputs(&contract(), &json!({})).len(), 2);
    }

    #[test]
    fn a_contract_without_required_inputs_is_satisfiable_by_anything() {
        assert!(missing_inputs(&json!({"type": "object"}), &json!({})).is_empty());
    }

    #[test]
    fn every_response_publishes_what_it_could_not_compare() {
        let c = comparability();
        let gaps = c["not_measured_yet"].as_array().expect("gaps present");
        for dimension in ["latency", "reliability", "quality", "terms"] {
            assert!(
                gaps.iter()
                    .any(|g| g.as_str().unwrap_or("").starts_with(dimension)),
                "{dimension} must be disclosed as unmeasured"
            );
        }
    }
}
