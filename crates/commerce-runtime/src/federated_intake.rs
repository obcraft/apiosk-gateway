//! Public, no-account self-serve intake for FEDERATED x402 listings.
//!
//! A federated listing is an external, already-x402-native API that Apiosk only
//! INDEXES (catalog + `/.well-known/x402`); the agent pays the provider directly
//! at the provider's own resource URL + payTo. There is no Apiosk proxy, no
//! settlement, no payout. Onboarding these has historically required either the
//! `scripts/federated-import.mjs` CLI (operator) or the provider portal's import
//! flow (behind a login). This module removes that friction: any provider can
//! paste their `/.well-known/x402` URL, we read + validate it (including a live
//! 402 probe), they see a preview of exactly how the listing will render, and on
//! submit it lands as a `status='pending'` row in `apis` — the SAME review queue
//! and Resend approval-email pipeline the logged-in flow already uses.
//!
//! Nothing here touches the money path. Untrusted input never reaches the live
//! catalog: `apis` discovery requires `status='active'` (a pending row is
//! invisible) and an operator must approve before it goes live.
//!
//! Gated behind `FEDERATED_SELF_SERVE_ENABLED` (default off): the three routes
//! 404 until it is set. Abuse controls: per-IP rate limit, an HTTPS + SSRF guard
//! (`validate_upstream_url`) on both the pasted URL and the probed resource, body
//! and resource-count caps, and mandatory admin review.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Form, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::api_management::helpers::validate_upstream_url;
use crate::html::{escape_html, format_price, html_footer, html_head};
use crate::rate_limit::{RateLimitDecision, RateLimitPolicy};
use crate::v1_routes::models::{ApiDetailResponse, EndpointDocResponse, ProviderPublic};
use crate::v1_routes::render::render_api_detail_html;
use crate::AppState;

const USER_AGENT_STR: &str = "ApioskX402Importer/1.0 (+https://apiosk.com)";
const FETCH_TIMEOUT: Duration = Duration::from_secs(12);
const MAX_REDIRECTS: usize = 3;
const MAX_DOC_BYTES: usize = 1_000_000;
const MAX_402_BYTES: usize = 131_072;
/// Upper bound on how many resources we persist from one discovery document.
const MAX_RESOURCES: usize = 500;

const MAX_NAME: usize = 120;
const MAX_DESCRIPTION: usize = 400;
const MAX_SLUG: usize = 64;
const MAX_URL: usize = 1024;

/// Per-IP flood guards. Inspect is cheap-ish (fetch + probe); submit is stricter.
const INSPECT_POLICY: RateLimitPolicy = RateLimitPolicy::per_minute(20);
const SUBMIT_POLICY: RateLimitPolicy = RateLimitPolicy::per_minute(6);

// ---------------------------------------------------------------------------
// Route handlers
// ---------------------------------------------------------------------------

/// `GET /federated/new` — step 1: paste the discovery URL.
pub async fn federated_new(State(state): State<Arc<AppState>>) -> Response {
    if !state.config.federated_self_serve_enabled {
        return not_found();
    }
    page(render_step1(None, ""))
}

/// `POST /federated/inspect` — fetch + validate the discovery URL and render the
/// step-2 review page (report + prefilled form + live preview). Also serves the
/// "Refresh preview" button on step 2 (the form re-posts here with edited fields).
pub async fn federated_inspect(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<IntakeForm>,
) -> Response {
    if !state.config.federated_self_serve_enabled {
        return not_found();
    }
    if let RateLimitDecision::Throttled { .. } = state
        .rate_limiter
        .check(&format!("fed_inspect:{}", crate::http::client_ip(&headers)), INSPECT_POLICY)
        .await
    {
        return page(render_step1(
            Some("That's a lot of requests — please wait a moment and try again."),
            form.discovery_url.trim(),
        ));
    }

    let url = form.discovery_url.trim().to_string();
    match inspect(&state, &url).await {
        Ok(inspection) => page(render_step2(
            &inspection,
            &FormValues::from_form(&form, &inspection),
            &[],
        )),
        Err(err) => page(render_step1(Some(&err), &url)),
    }
}

/// `POST /federated/submit` — re-validate server-side (never trust the posted
/// resources), then insert a `status='pending'` federated row and confirm.
pub async fn federated_submit(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<IntakeForm>,
) -> Response {
    if !state.config.federated_self_serve_enabled {
        return not_found();
    }
    if let RateLimitDecision::Throttled { .. } = state
        .rate_limiter
        .check(&format!("fed_submit:{}", crate::http::client_ip(&headers)), SUBMIT_POLICY)
        .await
    {
        return page(render_step1(
            Some("That's a lot of submissions — please wait a moment and try again."),
            form.discovery_url.trim(),
        ));
    }

    // Re-fetch + re-parse authoritatively. A bad URL sends the user back to step 1.
    let url = form.discovery_url.trim().to_string();
    let inspection = match inspect(&state, &url).await {
        Ok(i) => i,
        Err(err) => return page(render_step1(Some(&err), &url)),
    };

    let values = FormValues::from_form(&form, &inspection);
    let mut errors: Vec<String> = Vec::new();

    if !valid_slug(&values.slug) {
        errors.push(
            "Choose a slug of lowercase letters, numbers and hyphens (e.g. acme-data).".into(),
        );
    }
    if values.name.trim().is_empty() {
        errors.push("Add a display name for your listing.".into());
    }
    if !valid_email(&values.contact_email) {
        errors.push("Enter a valid contact email — that's where the approval notice goes.".into());
    }
    if !values.logo_url.trim().is_empty() && !valid_https_url(&values.logo_url) {
        errors.push("The logo URL must be a public https:// image URL (or leave it blank).".into());
    }
    if values.website_url.trim().is_empty() {
        errors.push(
            "Add your provider website — it's how operators vet a listing before trusting it."
                .into(),
        );
    } else if !valid_https_url(&values.website_url) {
        errors.push("The website must be a valid https:// URL (e.g. https://acme.com).".into());
    }
    if !values.github_url.trim().is_empty() && !valid_github_url(&values.github_url) {
        errors.push(
            "The GitHub link must be an https://github.com/… URL (or leave it blank).".into(),
        );
    }
    // The live 402 probe is the "valid content behind the URL" gate.
    if !inspection.probe.ok {
        errors.push(format!(
            "The endpoint did not pass the live x402 check: {}",
            inspection.probe.message
        ));
    }

    if !errors.is_empty() {
        return page(render_step2(&inspection, &values, &errors));
    }

    // Build the persisted shape (mirrors provider/federated-import commit()).
    let logo = values.logo_url.trim();
    let github = values.github_url.trim();
    let listing_metadata = json!({
        "name": values.name,
        "logo": if logo.is_empty() { Value::Null } else { json!(logo) },
        "website": values.website_url.trim(),
        "github": if github.is_empty() { Value::Null } else { json!(github) },
        "contact_email": values.contact_email,
        "category": values.category,
        "source": "self-serve-federated",
        "tags": [values.category.clone(), "federated"],
    });
    let endpoints = derive_endpoints(&inspection.resources);
    let primary_pay_to = inspection.pay_tos.first().cloned().unwrap_or_default();
    let description = if values.description.trim().is_empty() {
        format!(
            "{} — x402 API discovered by Apiosk and executed through the Apiosk gateway.",
            values.name
        )
    } else {
        values.description.clone()
    };

    // Landing this row as `status='pending'` is the whole handoff: the review
    // gate stamps `submitted_for_review_at`, the review-email trigger notifies
    // the operator, the listing shows up in the admin portal's queue
    // (`admin_pending_apis()`), and approval there (`admin_review_api()`) flips
    // it to active and emails the address below. Nothing else runs here.
    let api_id = Uuid::new_v4();
    match state
        .db
        .upsert_federated_submission(
            api_id,
            &values.slug,
            &values.name,
            &description,
            &inspection.discovery_url,
            &primary_pay_to,
            &values.category,
            &json!(inspection.resources),
            &listing_metadata,
            values.website_url.trim(),
            Some(values.github_url.trim()).filter(|s| !s.is_empty()),
            &endpoints,
        )
        .await
    {
        // A different id back means we updated the submitter's own pending row.
        Ok(Some(row_id)) => page(render_confirmation(&values, row_id != api_id)),
        Ok(None) => {
            let mut errs = errors;
            errs.push(format!(
                "The slug \"{}\" is already taken — pick another. (Re-submitting your own listing only works while it is still in review, from the same contact email.)",
                values.slug
            ));
            page(render_step2(&inspection, &values, &errs))
        }
        Err(e) => {
            tracing::error!("federated intake insert failed: {e}");
            let mut errs = errors;
            errs.push("We could not save your submission. Please try again in a moment.".into());
            page(render_step2(&inspection, &values, &errs))
        }
    }
}

// ---------------------------------------------------------------------------
// Inspection: fetch + validate + parse + live 402 probe
// ---------------------------------------------------------------------------

struct Inspection {
    discovery_url: String,
    /// Verbatim provider resource objects (payTo-bearing accepts only).
    resources: Vec<Value>,
    pay_tos: Vec<String>,
    networks: Vec<String>,
    min_price_usd: f64,
    resource_count: usize,
    suggested_slug: String,
    suggested_name: String,
    probe: ProbeResult,
    warnings: Vec<String>,
}

struct ProbeResult {
    /// True = a real HTTP 402 x402 challenge with a payTo was observed.
    ok: bool,
    message: String,
}

async fn inspect(state: &Arc<AppState>, raw_url: &str) -> Result<Inspection, String> {
    let url = raw_url.trim();
    if url.is_empty() {
        return Err("Paste your /.well-known/x402 discovery URL to begin.".into());
    }
    if url.len() > MAX_URL {
        return Err("That URL is too long.".into());
    }
    validate_upstream_url(url).map_err(|e| e.to_string())?;

    let (final_url, body) = fetch_doc(&state.http_client, url).await?;
    let doc: Value = serde_json::from_slice(&body).map_err(|_| {
        "The URL did not return valid JSON. A discovery document must be an x402 JSON document."
            .to_string()
    })?;

    let mut resources = normalise_resources(&doc);
    if resources.is_empty() {
        return Err("No valid x402 resources found. Expected a discovery document with resources[].accepts[].payTo.".into());
    }
    let truncated = resources.len() > MAX_RESOURCES;
    if truncated {
        resources.truncate(MAX_RESOURCES);
    }

    let pay_tos = distinct_pay_tos(&resources);
    let networks = distinct_networks(&resources);
    let min_price_usd = overall_min_price(&resources);

    let host = url::Url::parse(&final_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default();
    let suggested_slug = suggest_slug(&host);
    let suggested_name = suggest_name(&host);

    let mut warnings = Vec::new();
    if pay_tos.len() > 1 {
        warnings.push(format!(
            "This document advertises {} different payout addresses across resources.",
            pay_tos.len()
        ));
    }
    if truncated {
        warnings.push(format!(
            "Only the first {MAX_RESOURCES} resources were kept; the document lists more."
        ));
    }
    if min_price_usd == 0.0 {
        warnings.push(
            "No per-call price could be read (maxAmountRequired missing); showing Free.".into(),
        );
    }

    // Live 402 probe of one advertised resource — the "valid content" gate.
    let probe = probe_402(&state.http_client, &probe_target(&resources), &pay_tos).await;

    Ok(Inspection {
        discovery_url: final_url,
        resource_count: resources.len(),
        resources,
        pay_tos,
        networks,
        min_price_usd,
        suggested_slug,
        suggested_name,
        probe,
        warnings,
    })
}

/// GET the discovery URL, following a bounded number of redirects and
/// re-running the SSRF guard on every hop (a redirect target is attacker
/// controlled). Returns the final URL and the (size-capped) body.
async fn fetch_doc(client: &reqwest::Client, url_in: &str) -> Result<(String, Vec<u8>), String> {
    let mut current = url_in.to_string();
    for _ in 0..=MAX_REDIRECTS {
        validate_upstream_url(&current).map_err(|e| e.to_string())?;
        let resp = client
            .get(current.as_str())
            .timeout(FETCH_TIMEOUT)
            .header(header::USER_AGENT, USER_AGENT_STR)
            .header(header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| format!("Could not reach the URL: {}", short_err(&e)))?;
        let status = resp.status();
        if status.is_redirection() {
            let loc = resp
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| "The URL redirected without a destination.".to_string())?;
            current = url::Url::parse(&current)
                .ok()
                .and_then(|base| base.join(loc).ok())
                .map(|u| u.to_string())
                .ok_or_else(|| "The URL redirected somewhere invalid.".to_string())?;
            continue;
        }
        if !status.is_success() {
            return Err(format!(
                "The provider returned HTTP {} for the discovery URL.",
                status.as_u16()
            ));
        }
        let body = read_bounded(resp, MAX_DOC_BYTES).await?;
        return Ok((current, body));
    }
    Err("The discovery URL redirected too many times.".into())
}

/// The x402 v2 challenge header. A v2-only provider puts the entire challenge
/// here (base64 JSON) and is free to send whatever body it likes — including a
/// hint blob with no `accepts[]` at all.
const CHALLENGE_HEADER: &str = "payment-required";

/// Which resource to probe. A discovery document may advertise path templates
/// (`/api/price/:coin`, `/v1/{id}/report`); a literal GET of one is not a real
/// call, so prefer a concrete URL and fall back to the first resource only when
/// every URL is templated.
fn probe_target(resources: &[Value]) -> String {
    let urls = resources
        .iter()
        .filter_map(|resource| resource.get("resource").and_then(Value::as_str));
    let mut first = "";
    for url in urls {
        if first.is_empty() {
            first = url;
        }
        if !is_templated_url(url) {
            return url.to_string();
        }
    }
    first.to_string()
}

fn is_templated_url(url: &str) -> bool {
    url.contains('{') || url.split('/').any(|segment| segment.starts_with(':'))
}

/// `accepts[]` out of a base64 `PAYMENT-REQUIRED` header.
///
/// The decoding is [`crate::sources::decode_challenge`] — the same reader the
/// discovery sweep uses on the same header. Two decoders for one header would
/// eventually disagree about which providers are payable.
fn accepts_from_challenge_header(value: &str) -> Vec<Value> {
    crate::sources::decode_challenge(value)
        .and_then(|json| json.get("accepts").and_then(Value::as_array).cloned())
        .unwrap_or_default()
}

/// GET one advertised resource and confirm it answers with a real x402 402
/// challenge (a payTo present). Network-agnostic: we match payTo against the
/// discovery document rather than the gateway's own payable chains.
///
/// Both dialects count. x402 v2 carries the challenge in the
/// `PAYMENT-REQUIRED` header, v1 carries it in the JSON body, and the gateway
/// itself emits both — so a probe that only read the body would reject every
/// v2-only provider even though their 402 is perfectly valid.
async fn probe_402(
    client: &reqwest::Client,
    resource_url: &str,
    doc_pay_tos: &[String],
) -> ProbeResult {
    let fail = |message: String| ProbeResult { ok: false, message };

    if resource_url.is_empty() || validate_upstream_url(resource_url).is_err() {
        return fail("the resource URL is not a public https:// URL.".into());
    }
    let resp = match client
        .get(resource_url)
        .timeout(FETCH_TIMEOUT)
        .header(header::USER_AGENT, USER_AGENT_STR)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return fail(format!("could not reach the resource ({}).", short_err(&e))),
    };
    let status = resp.status().as_u16();
    if status != 402 {
        return fail(format!(
            "expected HTTP 402 (payment required) from the resource, got {status}."
        ));
    }
    // Read the v2 header before the body: `read_bounded` consumes the response.
    let header_accepts = resp
        .headers()
        .get(CHALLENGE_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(accepts_from_challenge_header)
        .unwrap_or_default();

    let body = match read_bounded(resp, MAX_402_BYTES).await {
        Ok(b) => b,
        Err(e) => return fail(e),
    };
    let body_accepts = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|json| json.get("accepts").and_then(Value::as_array).cloned())
        .unwrap_or_default();

    let mut probe_pay_tos = pay_tos_from_accepts(&header_accepts);
    if probe_pay_tos.is_empty() {
        probe_pay_tos = pay_tos_from_accepts(&body_accepts);
    }
    if probe_pay_tos.is_empty() {
        return fail(
            "the 402 challenge did not advertise a payTo address (checked the PAYMENT-REQUIRED header and the response body)."
                .into(),
        );
    }
    let matches = probe_pay_tos
        .iter()
        .any(|p| doc_pay_tos.iter().any(|d| d.eq_ignore_ascii_case(p)));
    ProbeResult {
        ok: true,
        message: if matches {
            "live 402 challenge verified; payTo matches the discovery document.".into()
        } else {
            "live 402 challenge verified, but its payTo differs from the discovery document.".into()
        },
    }
}

async fn read_bounded(resp: reqwest::Response, cap: usize) -> Result<Vec<u8>, String> {
    if let Some(len) = resp.content_length() {
        if len as usize > cap {
            return Err("the response was too large to read.".into());
        }
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|_| "could not read the response body.".to_string())?;
    if bytes.len() > cap {
        return Err("the response was too large to read.".into());
    }
    Ok(bytes.to_vec())
}

fn short_err(e: &reqwest::Error) -> String {
    // Keep provider-facing messages terse; never leak internal detail.
    if e.is_timeout() {
        "timed out".into()
    } else if e.is_connect() {
        "connection failed".into()
    } else {
        "request failed".into()
    }
}

// ---------------------------------------------------------------------------
// Parsing (ported from scripts/federated-import.mjs / the provider edge fn)
// ---------------------------------------------------------------------------

/// Providers publish either `{resources:[...]}` (x402 discovery) or `{items:[...]}`
/// (Bazaar-style). Try each; keep the first that yields payTo-bearing resources.
fn normalise_resources(doc: &Value) -> Vec<Value> {
    for key in ["resources", "items"] {
        if let Some(arr) = doc.get(key).and_then(Value::as_array) {
            let cleaned: Vec<Value> = arr.iter().filter_map(clean_resource).collect();
            if !cleaned.is_empty() {
                return cleaned;
            }
        }
    }
    Vec::new()
}

/// Keep a resource iff it has a string `resource` URL and ≥1 accept with a
/// non-empty `payTo`. Everything else in the object (network, asset,
/// maxAmountRequired, scheme, extra) is preserved verbatim so
/// `/.well-known/x402` can republish it unchanged.
fn clean_resource(entry: &Value) -> Option<Value> {
    let obj = entry.as_object()?;
    let resource = obj
        .get("resource")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let accepts: Vec<Value> = obj
        .get("accepts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|a| {
            a.get("payTo")
                .and_then(Value::as_str)
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false)
        })
        .collect();
    if accepts.is_empty() {
        return None;
    }
    let mut out = obj.clone();
    out.insert("resource".into(), json!(resource));
    out.insert("accepts".into(), json!(accepts));
    out.entry("type").or_insert_with(|| json!("http"));
    Some(Value::Object(out))
}

fn pay_tos_from_accepts(accepts: &[Value]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for a in accepts {
        if let Some(p) = a.get("payTo").and_then(Value::as_str) {
            let p = p.trim();
            if !p.is_empty() && !out.iter().any(|e| e.eq_ignore_ascii_case(p)) {
                out.push(p.to_string());
            }
        }
    }
    out
}

fn distinct_pay_tos(resources: &[Value]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in resources {
        if let Some(accepts) = r.get("accepts").and_then(Value::as_array) {
            for p in pay_tos_from_accepts(accepts) {
                if !out.iter().any(|e| e.eq_ignore_ascii_case(&p)) {
                    out.push(p);
                }
            }
        }
    }
    out
}

fn distinct_networks(resources: &[Value]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in resources {
        if let Some(accepts) = r.get("accepts").and_then(Value::as_array) {
            for a in accepts {
                if let Some(n) = a.get("network").and_then(Value::as_str) {
                    let n = n.trim();
                    if !n.is_empty() && !out.iter().any(|e| e == n) {
                        out.push(n.to_string());
                    }
                }
            }
        }
    }
    out
}

/// Minimum per-call price (USD) across a resource's accepts[]. atomic units of a
/// 6-decimal asset (USDC/USDbC — the x402 default) / 1e6. Returns None if unknown.
///
/// Both spellings are accepted: the x402 spec's `maxAmountRequired` and the
/// `amount` many facilitators emit. Reading only one of them silently prices the
/// whole listing at 0 — the endpoint rows this feeds are what the admin review
/// sheet, the provider portal and MIN(price) discovery read, even though the
/// public detail page still renders the right price from external_x402. Keep in
/// step with catalog::federated_min_price_usd and federated-import's minPriceUsd.
pub(crate) fn min_price_of(accepts: &[Value]) -> Option<f64> {
    let mut min: Option<f64> = None;
    for a in accepts {
        // Atomic units are whole numbers. A decimal ("0.01") is a provider
        // quoting USDC directly; dividing it would price the call at 1e-8 —
        // unchargeable, and indistinguishable from free once rounded. Leave it
        // unknown so the listing is flagged rather than silently near-zero.
        let atomic = a
            .get("amount")
            .or_else(|| a.get("maxAmountRequired"))
            .and_then(|v| match v {
                Value::String(s) => s.trim().parse::<u64>().ok().map(|n| n as f64),
                Value::Number(n) => n.as_u64().map(|n| n as f64),
                _ => None,
            });
        if let Some(atomic) = atomic {
            let usd = atomic / 1_000_000.0;
            min = Some(min.map_or(usd, |m| m.min(usd)));
        }
    }
    min
}

fn overall_min_price(resources: &[Value]) -> f64 {
    let mut min: Option<f64> = None;
    for r in resources {
        if let Some(accepts) = r.get("accepts").and_then(Value::as_array) {
            if let Some(p) = min_price_of(accepts) {
                min = Some(min.map_or(p, |m| m.min(p)));
            }
        }
    }
    min.unwrap_or(0.0)
}

/// Pathname of a resource URL, defaulting to `/` (matches pathFromResource).
fn path_from_resource(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .map(|u| {
            let p = u.path();
            if p.is_empty() {
                "/".to_string()
            } else {
                p.to_string()
            }
        })
        .unwrap_or_else(|| "/".to_string())
}

/// (method, path, description, price_usd) per resource, for `api_endpoints`.
fn derive_endpoints(resources: &[Value]) -> Vec<(String, String, Option<String>, f64)> {
    resources
        .iter()
        .map(|r| {
            let resource = r.get("resource").and_then(Value::as_str).unwrap_or("/");
            let method = r
                .get("metadata")
                .and_then(|m| m.get("method"))
                .and_then(Value::as_str)
                .map(|m| m.to_uppercase())
                .unwrap_or_else(|| "POST".to_string());
            let description = r
                .get("description")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let price = r
                .get("accepts")
                .and_then(Value::as_array)
                .and_then(|a| min_price_of(a))
                .unwrap_or(0.0);
            (method, path_from_resource(resource), description, price)
        })
        .collect()
}

fn suggest_slug(host: &str) -> String {
    let host = host.to_ascii_lowercase();
    let labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    // Drop a leading generic subdomain (x402.acme.com → acme).
    let mut idx = 0;
    while idx + 1 < labels.len()
        && matches!(labels[idx], "x402" | "api" | "www" | "app" | "gateway")
    {
        idx += 1;
    }
    let base = labels.get(idx).copied().unwrap_or("provider");
    let slug: String = base
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "x402-provider".to_string()
    } else {
        slug.chars().take(MAX_SLUG).collect()
    }
}

fn suggest_name(host: &str) -> String {
    let slug = suggest_slug(host);
    let mut chars = slug.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => "x402 Provider".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Field validation
// ---------------------------------------------------------------------------

fn valid_slug(s: &str) -> bool {
    let s = s.trim();
    !s.is_empty()
        && s.len() <= MAX_SLUG
        && s.chars()
            .next()
            .map(|c| c.is_ascii_alphanumeric())
            .unwrap_or(false)
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn valid_email(s: &str) -> bool {
    let s = s.trim();
    if s.len() < 3 || s.len() > 254 || s.contains(char::is_whitespace) {
        return false;
    }
    match s.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
        }
        None => false,
    }
}

fn valid_https_url(s: &str) -> bool {
    let s = s.trim();
    s.len() <= MAX_URL
        && url::Url::parse(s)
            .map(|u| u.scheme() == "https" && u.host_str().is_some())
            .unwrap_or(false)
}

/// A GitHub link: a valid https URL whose host is `github.com` (optionally
/// `www.`). Keeps the optional repo field from collecting arbitrary URLs.
fn valid_github_url(s: &str) -> bool {
    valid_https_url(s)
        && url::Url::parse(s.trim())
            .ok()
            .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
            .is_some_and(|h| h == "github.com" || h == "www.github.com")
}

// ---------------------------------------------------------------------------
// Request/form models
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct IntakeForm {
    #[serde(default)]
    discovery_url: String,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    logo_url: String,
    #[serde(default)]
    website_url: String,
    #[serde(default)]
    github_url: String,
    #[serde(default)]
    contact_email: String,
}

/// The user-editable fields, with sensible fallbacks to the inspected suggestions.
struct FormValues {
    slug: String,
    name: String,
    category: String,
    description: String,
    logo_url: String,
    /// Provider's public website. Mandatory for a self-serve federated listing:
    /// it is the human-facing home an agent's operator can vet before trusting a
    /// URL the gateway never proxies.
    website_url: String,
    /// Optional source/repo link.
    github_url: String,
    contact_email: String,
}

impl FormValues {
    fn from_form(form: &IntakeForm, inspection: &Inspection) -> Self {
        let slug = {
            let s = form.slug.trim();
            if s.is_empty() {
                inspection.suggested_slug.clone()
            } else {
                s.to_string()
            }
        };
        let name = {
            let n = form.name.trim();
            if n.is_empty() {
                inspection.suggested_name.clone()
            } else {
                truncate(n, MAX_NAME)
            }
        };
        let category = {
            let c = form.category.trim().to_lowercase();
            if CATEGORIES.contains(&c.as_str()) {
                c
            } else {
                "data".to_string()
            }
        };
        FormValues {
            slug,
            name,
            category,
            description: truncate(form.description.trim(), MAX_DESCRIPTION),
            logo_url: truncate(form.logo_url.trim(), MAX_URL),
            website_url: truncate(form.website_url.trim(), MAX_URL),
            github_url: truncate(form.github_url.trim(), MAX_URL),
            contact_email: form.contact_email.trim().to_string(),
        }
    }
}

const CATEGORIES: &[&str] = &["data", "tools", "finance", "web", "ai", "media", "other"];

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn page(body: String) -> Response {
    Html(body).into_response()
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "Not found").into_response()
}

/// Shared wizard chrome: the base `html_head`/`html_footer` plus form styles.
fn shell(title: &str, inner: &str) -> String {
    let mut html = html_head(
        title,
        crate::html::Nav::Publish,
        "Publish your API on the Apiosk gateway and get paid per call in USDC on Base. No accounts or SDKs — list an x402-native or managed API.",
        "/federated/new",
    );
    html.push_str(INTAKE_STYLES);
    html.push_str(inner);
    html.push_str(html_footer());
    html
}

const INTAKE_STYLES: &str = r#"<style>
  .intake-lead { max-width:680px; color:#4b5563; font-size:14px; line-height:1.6; }
  .intake-field { margin-bottom:16px; }
  .intake-field label { display:block; font-size:12px; font-weight:600; color:#374151; margin-bottom:6px; }
  .intake-field input, .intake-field select, .intake-field textarea { width:100%; padding:10px 12px; border:1px solid #e5e7eb; border-radius:8px; font-size:14px; font-family:inherit; color:#111; }
  .intake-field input:focus, .intake-field select:focus, .intake-field textarea:focus { outline:none; border-color:#a1a1aa; }
  .intake-field textarea { min-height:78px; resize:vertical; }
  .intake-hint { font-size:12px; color:#9ca3af; margin-top:5px; }
  .btn { appearance:none; border:1px solid #111; background:#111; color:#fff; font-size:14px; font-weight:600; padding:10px 18px; border-radius:8px; cursor:pointer; font-family:inherit; }
  .btn:hover { background:#000; }
  .btn-secondary { background:#fff; color:#111; }
  .btn-secondary:hover { background:#f9fafb; }
  .btn-row { display:flex; gap:10px; flex-wrap:wrap; margin-top:8px; }
  .report { border:1px solid #e5e7eb; border-radius:10px; padding:14px 16px; margin-bottom:20px; }
  .report.ok { border-color:#a7f3d0; background:#ecfdf5; }
  .report.warn { border-color:#fde68a; background:#fffbeb; }
  .report.err { border-color:#fecaca; background:#fef2f2; }
  .report h3 { margin:0 0 6px; font-size:13.5px; color:#111; }
  .report p { margin:0; font-size:13px; color:#4b5563; }
  .report ul { margin:8px 0 0; padding-left:18px; font-size:13px; color:#4b5563; }
  .report code { font-family:ui-monospace, monospace; font-size:12px; }
  .cols { display:grid; grid-template-columns: minmax(0,1fr) minmax(0,1fr); gap:28px; align-items:start; }
  .preview-frame { width:100%; height:640px; border:1px solid #e5e7eb; border-radius:12px; background:#fff; }
  .preview-note { font-size:12px; color:#9ca3af; margin:0 0 8px; }
  @media (max-width:900px) { .cols { grid-template-columns:1fr; } }
</style>"#;

fn render_step1(error: Option<&str>, prefill_url: &str) -> String {
    let mut inner = String::new();
    inner.push_str(
        r#"<div class="eyebrow"><a href="/v1/apis">&larr; APIs</a> / List your x402 API</div>
        <span class="badge badge-purple">Federated &middot; self-serve</span>
        <h1 style="margin-top:10px;">List your x402 API on Apiosk</h1>
        <p class="intake-lead" style="margin-top:8px;">
          Already run an x402-native API? Get it into the Apiosk catalog and
          <code>/.well-known/x402</code> so agents can discover it. Apiosk only indexes your
          listing and sends you traffic — <strong>agents pay you directly at your own payTo</strong>.
          We never proxy your API and never touch the money.
        </p>"#,
    );
    if let Some(err) = error {
        inner.push_str(&format!(
            r#"<div class="report err"><h3>We couldn't read that</h3><p>{}</p></div>"#,
            escape_html(err)
        ));
    }
    inner.push_str(&format!(
        r#"<form method="post" action="/federated/inspect" style="max-width:560px;margin-top:22px;">
          <div class="intake-field">
            <label for="discovery_url">Your x402 discovery URL</label>
            <input type="url" id="discovery_url" name="discovery_url" required
                   placeholder="https://api.example.com/.well-known/x402" value="{}" />
            <div class="intake-hint">We fetch this, validate the resources, and run a live 402 check. HTTPS only.</div>
          </div>
          <div class="btn-row"><button class="btn" type="submit">Read my x402 endpoint &rarr;</button></div>
        </form>"#,
        escape_html(prefill_url)
    ));
    shell("List your x402 API", &inner)
}

fn render_step2(inspection: &Inspection, values: &FormValues, errors: &[String]) -> String {
    let mut inner = String::new();
    inner.push_str(
        r#"<div class="eyebrow"><a href="/federated/new">&larr; Start over</a> / Review &amp; publish</div>
        <span class="badge badge-purple">Step 2 of 2</span>
        <h1 style="margin-top:10px;">Review &amp; publish</h1>"#,
    );

    // Validation report.
    let probe_ok = inspection.probe.ok;
    let report_class = if !errors.is_empty() {
        "err"
    } else if probe_ok && inspection.warnings.is_empty() {
        "ok"
    } else {
        "warn"
    };
    inner.push_str(&format!(r#"<div class="report {report_class}">"#));
    inner.push_str(&format!(
        r#"<h3>{icon} We read {count} x402 resource{plural} from your endpoint</h3>"#,
        icon = if probe_ok { "&#10003;" } else { "&#9888;" },
        count = inspection.resource_count,
        plural = if inspection.resource_count == 1 {
            ""
        } else {
            "s"
        },
    ));
    inner.push_str("<ul>");
    inner.push_str(&format!(
        "<li>Payout address{}: <code>{}</code></li>",
        if inspection.pay_tos.len() == 1 {
            ""
        } else {
            "es"
        },
        escape_html(&inspection.pay_tos.join(", "))
    ));
    if !inspection.networks.is_empty() {
        inner.push_str(&format!(
            "<li>Networks: <code>{}</code></li>",
            escape_html(&inspection.networks.join(", "))
        ));
    }
    inner.push_str(&format!(
        "<li>Lowest per-call price: {}</li>",
        format_price(inspection.min_price_usd)
    ));
    inner.push_str(&format!(
        "<li>Live 402 check: {}</li>",
        escape_html(&inspection.probe.message)
    ));
    for w in &inspection.warnings {
        inner.push_str(&format!("<li>{}</li>", escape_html(w)));
    }
    inner.push_str("</ul></div>");

    // Field errors.
    if !errors.is_empty() {
        inner.push_str(r#"<div class="report err"><h3>Please fix the following</h3><ul>"#);
        for e in errors {
            inner.push_str(&format!("<li>{}</li>", escape_html(e)));
        }
        inner.push_str("</ul></div>");
    }

    // Two columns: form + preview.
    inner.push_str(r#"<div class="cols"><div>"#);
    inner.push_str(&render_form(inspection, values));
    inner.push_str(r#"</div><div><p class="preview-note">Live preview — this is exactly how your listing page will look once approved.</p>"#);
    let preview_doc = render_api_detail_html(&build_preview(inspection, values));
    inner.push_str(&format!(
        r#"<iframe class="preview-frame" sandbox="allow-scripts" srcdoc="{}"></iframe>"#,
        escape_srcdoc(&preview_doc)
    ));
    inner.push_str("</div></div>");

    shell("Review & publish", &inner)
}

fn render_form(inspection: &Inspection, v: &FormValues) -> String {
    let mut cats = String::new();
    for c in CATEGORIES {
        cats.push_str(&format!(
            r#"<option value="{c}"{sel}>{label}</option>"#,
            c = c,
            sel = if *c == v.category { " selected" } else { "" },
            label = capitalize(c),
        ));
    }
    format!(
        r#"<form method="post">
          <input type="hidden" name="discovery_url" value="{url}" />
          <div class="intake-field">
            <label for="name">Display name</label>
            <input type="text" id="name" name="name" required maxlength="120" value="{name}" />
          </div>
          <div class="intake-field">
            <label for="slug">Slug</label>
            <input type="text" id="slug" name="slug" required maxlength="64" value="{slug}"
                   pattern="[a-z0-9][a-z0-9-]*" />
            <div class="intake-hint">Your catalog URL: gateway.apiosk.com/{slug_disp}</div>
          </div>
          <div class="intake-field">
            <label for="category">Category</label>
            <select id="category" name="category">{cats}</select>
          </div>
          <div class="intake-field">
            <label for="logo_url">Logo URL <span style="color:#9ca3af;font-weight:400;">(optional)</span></label>
            <input type="url" id="logo_url" name="logo_url" maxlength="1024" value="{logo}"
                   placeholder="https://…/logo.png" />
          </div>
          <div class="intake-field">
            <label for="website_url">Provider website</label>
            <input type="url" id="website_url" name="website_url" required maxlength="1024" value="{website}"
                   placeholder="https://acme.com" />
            <div class="intake-hint">Your public homepage. Required — it's how operators vet a listing the gateway never proxies.</div>
          </div>
          <div class="intake-field">
            <label for="github_url">GitHub <span style="color:#9ca3af;font-weight:400;">(optional)</span></label>
            <input type="url" id="github_url" name="github_url" maxlength="1024" value="{github}"
                   placeholder="https://github.com/acme/api" />
          </div>
          <div class="intake-field">
            <label for="description">Description</label>
            <textarea id="description" name="description" maxlength="400"
                      placeholder="What does your API do?">{desc}</textarea>
          </div>
          <div class="intake-field">
            <label for="contact_email">Contact email</label>
            <input type="email" id="contact_email" name="contact_email" required maxlength="254" value="{email}" />
            <div class="intake-hint">We email this address when an operator approves your listing.</div>
          </div>
          <div class="btn-row">
            <button class="btn btn-secondary" type="submit" formaction="/federated/inspect">Refresh preview</button>
            <button class="btn" type="submit" formaction="/federated/submit">Publish for review</button>
          </div>
        </form>"#,
        url = escape_html(&inspection.discovery_url),
        name = escape_html(&v.name),
        slug = escape_html(&v.slug),
        slug_disp = escape_html(&v.slug),
        cats = cats,
        logo = escape_html(&v.logo_url),
        website = escape_html(&v.website_url),
        github = escape_html(&v.github_url),
        desc = escape_html(&v.description),
        email = escape_html(&v.contact_email),
    )
}

/// Build the `ApiDetailResponse` the real detail-page renderer consumes, so the
/// preview is byte-identical to the eventual public page.
fn build_preview(inspection: &Inspection, v: &FormValues) -> ApiDetailResponse {
    let endpoints: Vec<EndpointDocResponse> = derive_endpoints(&inspection.resources)
        .into_iter()
        .zip(inspection.resources.iter())
        .map(|((method, path, description, price_usd), res)| {
            let full_url = res
                .get("resource")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            EndpointDocResponse {
                // A preview has no `api_endpoints` row yet, so there is no id
                // to be executed by — and nothing to execute either.
                candidate_id: None,
                method,
                path,
                full_url,
                description,
                price_usd,
                payment_required: true,
                request_body: None,
                response_body: None,
                request_example: None,
                response_example: None,
                headers: None,
                query_params: None,
                documentation_markdown: None,
                external_docs_url: None,
                openapi_url: None,
                authentication_notes: None,
                pricing_rules: json!([]),
            }
        })
        .collect();

    let logo = v.logo_url.trim();
    let description = if v.description.trim().is_empty() {
        format!(
            "{} — x402 API discovered by Apiosk and executed through the Apiosk gateway.",
            v.name
        )
    } else {
        v.description.clone()
    };

    ApiDetailResponse {
        id: "preview".to_string(),
        slug: v.slug.clone(),
        name: v.name.clone(),
        description,
        long_description: None,
        category: Some(v.category.clone()),
        listing_type: Some("federated".to_string()),
        docs_url: Some(inspection.discovery_url.clone()),
        gateway_base_url: String::new(),
        example_usage: None,
        authentication: None,
        // The preview mirrors the POST-approval page: a federated listing is
        // stored is_verified=true (approval only flips status), so the live
        // detail page renders the "Verified" badge — show that here too.
        active: true,
        verified: true,
        listing_quality: "production".to_string(),
        price_usd: inspection.min_price_usd,
        operations: Vec::new(),
        listing_metadata: json!({}),
        created_at: String::new(),
        endpoints,
        hosted_externally: true,
        external_resources: json!(inspection.resources),
        provider: Some(ProviderPublic {
            name: v.name.clone(),
            username: None,
            avatar_url: if logo.is_empty() {
                None
            } else {
                Some(logo.to_string())
            },
            bio: None,
            sector: None,
        }),
    }
}

fn render_confirmation(v: &FormValues, updated: bool) -> String {
    let inner = format!(
        r#"<div class="eyebrow"><a href="/v1/apis">&larr; APIs</a></div>
        <span class="badge">{badge}</span>
        <h1 style="margin-top:10px;">Thanks — {name} is in review</h1>
        <div class="report ok" style="max-width:640px;margin-top:16px;">
          <h3>&#10003; {headline}</h3>
          <p>An Apiosk operator will review it shortly. Nothing is public until it's approved —
          then it appears in the catalog and <code>/.well-known/x402</code>, and we email
          <strong>{email}</strong>.</p>
        </div>
        <p class="intake-lead" style="margin-top:18px;">
          Need to change something? <a href="/federated/new">Submit again</a> with the same slug and
          contact email while it is still in review, and we'll replace the details in the queue.
        </p>"#,
        badge = if updated { "Updated" } else { "Submitted" },
        headline = if updated {
            "Your pending x402 listing was updated"
        } else {
            "Your x402 listing was submitted"
        },
        name = escape_html(&v.name),
        email = escape_html(&v.contact_email),
    );
    shell("Submitted for review", &inner)
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// Escape a full HTML document for embedding in an `srcdoc="…"` attribute. Only
/// `&` and `"` need escaping in a double-quoted attribute; the browser then
/// parses the decoded value as HTML (so `&rarr;` → `&amp;rarr;` → `&rarr;`).
fn escape_srcdoc(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;")
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(resources: Value) -> Value {
        json!({ "resources": resources })
    }

    #[test]
    fn challenge_header_counts_as_a_valid_402() {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine as _;

        // A v2-only provider (e.g. api.webbersites.com): the challenge is in
        // the PAYMENT-REQUIRED header and the body is an unrelated hint blob.
        let challenge = json!({
            "x402Version": 2,
            "error": "Payment required",
            "accepts": [{
                "scheme": "exact",
                "network": "eip155:8453",
                "amount": "1000",
                "payTo": "0xdd5fcEa81CA6f1EB39FEd5B973DE794293B81ba6",
            }],
        });
        let encoded = STANDARD.encode(serde_json::to_vec(&challenge).unwrap());

        let accepts = accepts_from_challenge_header(&encoded);
        assert_eq!(
            pay_tos_from_accepts(&accepts),
            vec!["0xdd5fcEa81CA6f1EB39FEd5B973DE794293B81ba6".to_string()]
        );

        // Unpadded and URL-safe encodings decode too; garbage does not panic.
        let unpadded = encoded.trim_end_matches('=');
        assert!(!accepts_from_challenge_header(unpadded).is_empty());
        assert!(accepts_from_challenge_header("not base64 at all").is_empty());
        assert!(accepts_from_challenge_header("").is_empty());
    }

    /// Regression: reading only `maxAmountRequired` priced every endpoint of an
    /// `amount`-spelling document at 0, and those rows are what the admin review
    /// sheet, the provider portal and MIN(price) discovery read.
    #[test]
    fn price_is_read_from_either_amount_spelling() {
        let amount = vec![json!({"amount": "20000"})];
        let max_amount = vec![json!({"maxAmountRequired": "10000"})];
        let numeric = vec![json!({"amount": 5000})];
        assert_eq!(min_price_of(&amount), Some(0.02));
        assert_eq!(min_price_of(&max_amount), Some(0.01));
        assert_eq!(min_price_of(&numeric), Some(0.005));
        // Cheapest wins across accepts, and an unpriced offer stays unknown.
        assert_eq!(
            min_price_of(&[json!({"amount": "20000"}), json!({"amount": "1000"})]),
            Some(0.001)
        );
        assert_eq!(min_price_of(&[json!({"scheme": "exact"})]), None);
        // A decimal quote isn't atomic units; 0.01/1e6 would read as free.
        assert_eq!(min_price_of(&[json!({"amount": "0.01"})]), None);
        assert_eq!(min_price_of(&[json!({"amount": -5})]), None);

        // ...and the endpoint rows written at import carry that price, rather
        // than the 0 that made a paid listing look free in the review sheet.
        let resources = vec![json!({
            "resource": "https://rng.example.com/dice",
            "accepts": [{"amount": "20000"}],
        })];
        assert_eq!(derive_endpoints(&resources)[0].3, 0.02);
    }

    #[test]
    fn probe_prefers_a_concrete_url_over_a_path_template() {
        let resources = vec![
            json!({"resource": "https://api.acme.com/api/price/:coin"}),
            json!({"resource": "https://api.acme.com/v1/{id}/report"}),
            json!({"resource": "https://api.acme.com/api/scrape"}),
        ];
        assert_eq!(probe_target(&resources), "https://api.acme.com/api/scrape");

        // All templated: fall back to the first rather than probing nothing.
        assert_eq!(
            probe_target(&resources[..2]),
            "https://api.acme.com/api/price/:coin"
        );
        assert_eq!(probe_target(&[]), "");
    }

    #[test]
    fn normalise_keeps_paytto_bearing_resources_verbatim() {
        let d = doc(json!([
            {
                "resource": "https://api.acme.com/whois",
                "type": "http",
                "description": "WHOIS",
                "accepts": [{
                    "scheme": "exact", "network": "eip155:8453",
                    "payTo": "0xAbC0000000000000000000000000000000000001",
                    "asset": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
                    "maxAmountRequired": "20000"
                }]
            }
        ]));
        let out = normalise_resources(&d);
        assert_eq!(out.len(), 1);
        // Verbatim accepts preserved (network + asset survive for republish).
        assert_eq!(
            out[0]["accepts"][0]["network"].as_str(),
            Some("eip155:8453")
        );
        assert_eq!(
            distinct_pay_tos(&out),
            vec!["0xAbC0000000000000000000000000000000000001"]
        );
        // 20000 / 1e6 = 0.02
        assert!((overall_min_price(&out) - 0.02).abs() < 1e-9);
    }

    #[test]
    fn normalise_drops_resources_without_paytto_and_falls_back_to_items() {
        // `resources` present but has no payTo → fall back to `items`.
        let d = json!({
            "resources": [{ "resource": "https://x/a", "accepts": [{ "scheme": "exact" }] }],
            "items": [{ "resource": "https://x/b", "accepts": [{ "payTo": "0xDEAD" }] }]
        });
        let out = normalise_resources(&d);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["resource"].as_str(), Some("https://x/b"));
    }

    #[test]
    fn normalise_rejects_empty_document() {
        assert!(normalise_resources(&json!({ "resources": [] })).is_empty());
        assert!(normalise_resources(&json!({ "foo": 1 })).is_empty());
    }

    #[test]
    fn probe_match_is_case_insensitive_on_paytto() {
        let accepts = json!([{ "payTo": "0xabc0000000000000000000000000000000000001" }]);
        let probe = pay_tos_from_accepts(accepts.as_array().unwrap());
        let doc_pay = ["0xABC0000000000000000000000000000000000001".to_string()];
        let matched = probe
            .iter()
            .any(|p| doc_pay.iter().any(|d| d.eq_ignore_ascii_case(p)));
        assert!(matched, "payTo comparison must be case-insensitive");
    }

    #[test]
    fn slug_and_name_suggestions_strip_generic_subdomains() {
        assert_eq!(suggest_slug("x402.openwebninja.com"), "openwebninja");
        assert_eq!(suggest_slug("api.webbersites.com"), "webbersites");
        assert_eq!(suggest_name("x402.openwebninja.com"), "Openwebninja");
    }

    #[test]
    fn slug_validation() {
        assert!(valid_slug("acme-data"));
        assert!(valid_slug("a1"));
        assert!(!valid_slug("-nope"));
        assert!(!valid_slug("Nope"));
        assert!(!valid_slug("has space"));
        assert!(!valid_slug(""));
    }

    #[test]
    fn email_validation() {
        assert!(valid_email("ops@acme.com"));
        assert!(!valid_email("nope"));
        assert!(!valid_email("a@b"));
        assert!(!valid_email("a b@c.com"));
    }

    #[test]
    fn derive_endpoints_uses_path_and_min_price() {
        let resources = normalise_resources(&doc(json!([
            {
                "resource": "https://api.acme.com/v1/whois?plan=pro",
                "accepts": [{ "payTo": "0x1", "maxAmountRequired": "50000" }]
            }
        ])));
        let eps = derive_endpoints(&resources);
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].1, "/v1/whois"); // path only, query dropped
        assert!((eps[0].3 - 0.05).abs() < 1e-9);
    }

    #[test]
    fn srcdoc_escaping_is_attribute_safe() {
        let doc = r#"<a href="/x?a=1&b=2">go</a>"#;
        let esc = escape_srcdoc(doc);
        assert!(
            !esc.contains('"'),
            "double quotes must be escaped for srcdoc"
        );
        assert!(
            esc.contains("&amp;"),
            "ampersands must be escaped for srcdoc"
        );
    }
}
