//! `GET /v1/search?q=...` — a need in words, suppliers in return.
//!
//! Every other entry point assumes the caller already knows what it is looking
//! for. An agent handed "find the best financial investor news" knows no slug,
//! and making it fetch all fifteen capabilities and choose puts the catalogue's
//! own job on the caller.
//!
//! One request now takes the sentence, finds the capabilities that could serve
//! it, and returns their candidates ranked, each with what is actually known
//! about it: the live price, the measured latency where measurement exists, the
//! success rate, and whether the offer accepts the inputs the capability
//! defines.
//!
//! ---------------------------------------------------------------------------
//! What "best" means here
//! ---------------------------------------------------------------------------
//!
//! It means what the caller says it means. `prefer=cheapest` (the default),
//! `fastest`, or `most_reliable`, and the response states which was applied.
//!
//! There is no composite quality score, and there will not be one until it can
//! be defended: blending price, latency and success rate into a single number
//! requires weights nobody has justified, and it would let a ranking that
//! favours Apiosk's own settled listings hide inside an opaque metric. The
//! agent has the preference. It should supply it.
//!
//! `fastest` and `most_reliable` rank measured offers above unmeasured ones and
//! say so. Only nine listings have a usable sample, and no federated listing
//! has any, because the gateway never proxies those and so never times them.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::db::{CapabilitySort, ProviderStats};
use crate::AppState;

const GATEWAY_BASE: &str = "https://gateway.apiosk.com";

/// Capabilities considered per search. More than a handful and the response is
/// a directory rather than an answer.
const MAX_CAPABILITIES: i64 = 4;

/// Offers returned per matched capability.
const MAX_OFFERS_PER_CAPABILITY: usize = 8;

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    #[serde(alias = "query")]
    pub q: Option<String>,
    /// `cheapest` (default) | `fastest` | `most_reliable`.
    pub prefer: Option<String>,
    /// Bind a budget before reading the list rather than after.
    pub max_price: Option<f64>,
    /// Also search the wider x402 ecosystem. See `crate::bazaar`.
    ///
    /// ON by default here, unlike on `/v1/capabilities/{slug}`. This endpoint
    /// exists to answer "who can do this", and answering it from 44 curated
    /// operations while 15,500 Bazaar resources go unmentioned is a worse
    /// answer than a mixed one that labels which half is reviewed. The
    /// capability detail page stays opt-in because crawlers read it from the
    /// sitemap and the discovery documents, and every read there would
    /// otherwise fan out to the Bazaar.
    ///
    /// Pass `include_external=false` to get the reviewed set alone.
    #[serde(default = "default_true", deserialize_with = "lenient_bool")]
    pub include_external: bool,
}

pub(super) fn default_true() -> bool {
    true
}

/// Accepts `1`, `true`, `yes` and `on` as well as the strict JSON booleans.
///
/// serde's default bool deserializer rejects `include_external=1` with a 400,
/// and `=1` is exactly what llms.txt, the OpenAPI parameter description and
/// this endpoint's own hint field tell agents to send. A flag that 400s on the
/// form its documentation prescribes is a broken contract, not a strict one.
pub(super) fn lenient_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    let raw = String::deserialize(deserializer)?;
    Ok(matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    ))
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Preference {
    Cheapest,
    Fastest,
    MostReliable,
}

impl Preference {
    fn parse(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some("fastest") => Self::Fastest,
            Some("most_reliable") | Some("most-reliable") => Self::MostReliable,
            _ => Self::Cheapest,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Cheapest => "cheapest",
            Self::Fastest => "fastest",
            Self::MostReliable => "most_reliable",
        }
    }

    fn note(&self) -> &'static str {
        match self {
            Self::Cheapest => "Ranked by listed price, cheapest first. No managed-first preference is applied.",
            Self::Fastest => "Ranked by measured median latency, fastest first. Offers with no measurement rank below every measured one rather than being guessed at: only listings Apiosk proxies are timed, so no federated offer can appear here as fast or slow.",
            Self::MostReliable => "Ranked by measured success rate. Offers with no measurement rank below every measured one. Payment challenges are excluded from the denominator, so a 402 does not count against a provider.",
        }
    }
}

/// Orders offers within one capability according to the caller's preference.
///
/// Measured offers always precede unmeasured ones under `fastest` and
/// `most_reliable`. Sorting an unmeasured offer as though it were instant would
/// put the least-known supplier at the top, which is the opposite of what the
/// caller asked for.
/// Ranks already-shaped offer JSON by a caller-supplied preference string.
/// Shared with `/v1/select`, which applies the same ordering to its survivors.
pub(super) fn rank_offers(offers: &mut [Value], prefer: Option<&str>) {
    rank(offers, Preference::parse(prefer));
}

fn rank(offers: &mut [Value], preference: Preference) {
    offers.sort_by(|a, b| {
        let key = |v: &Value| -> (u8, f64) {
            match preference {
                Preference::Cheapest => (0, v["price_usd"].as_f64().unwrap_or(f64::MAX)),
                Preference::Fastest => match v["performance"]["p50_ms"].as_f64() {
                    Some(ms) => (0, ms),
                    None => (1, 0.0),
                },
                Preference::MostReliable => match v["performance"]["success_rate"].as_f64() {
                    // Negated so a higher rate sorts first under an ascending
                    // comparison.
                    Some(rate) => (0, -rate),
                    None => (1, 0.0),
                },
            }
        };
        let (ga, va) = key(a);
        let (gb, vb) = key(b);
        ga.cmp(&gb)
            .then(va.partial_cmp(&vb).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| {
                a["api_slug"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(b["api_slug"].as_str().unwrap_or(""))
            })
    });
}

/// The performance block for one listing, or an explicit absence.
///
/// Never a zero and never a guess. An agent that reads `p50_ms: 0` concludes
/// "instant"; one that reads `measured: false` concludes "unknown", which is
/// the truth.
pub(super) fn performance_json(stats: Option<&ProviderStats>, hosted_externally: bool) -> Value {
    match stats {
        Some(s) => {
            let mut v = s.to_json();
            v["measured"] = json!(true);
            v
        }
        None => json!({
            "measured": false,
            "reason": if hosted_externally {
                "Federated listing: Apiosk indexes it but never proxies it, so no call of it is ever timed here."
            } else {
                "Not enough successful calls through Apiosk yet to report a latency or success rate."
            },
        }),
    }
}

/// `GET /v1/search`
pub async fn search(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchQuery>,
) -> Result<Response, StatusCode> {
    let query = params.q.as_deref().unwrap_or("").trim().to_string();
    if query.is_empty() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "query_required",
                "message": "Describe what you need: /v1/search?q=financial investor news. Add prefer=cheapest|fastest|most_reliable, max_price, or include_external=1.",
            })),
        )
            .into_response());
    }

    let preference = Preference::parse(params.prefer.as_deref());

    let matches = state
        .db
        .search_capabilities(&query, MAX_CAPABILITIES)
        .await
        .map_err(|err| {
            tracing::error!("capability search failed for {}: {}", query, err);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // One stats lookup for the whole response rather than one per candidate.
    // A failure costs the performance block, never the results.
    let stats = state.cache.peek_provider_stats().await.unwrap_or_default();

    let sort = CapabilitySort::Price;
    let mut results = Vec::new();
    let mut known_hosts: Vec<String> = Vec::new();

    // One round trip for every matched capability. Four matches used to mean
    // four queries, each repeating the same joins over the same tables.
    let slugs: Vec<String> = matches.iter().map(|m| m.slug.clone()).collect();
    let all_offers = state
        .db
        .list_capability_offers_many(&slugs, &sort, params.max_price)
        .await
        .unwrap_or_default();

    for m in &matches {
        let offers: Vec<&crate::db::CapabilityOffer> = all_offers
            .iter()
            .filter(|o| o.capability_slug == m.slug)
            .collect();

        let mut shaped: Vec<Value> = offers
            .iter()
            .map(|o| {
                if let Some(host) = o
                    .call_url
                    .as_deref()
                    .and_then(|u| u.split("://").nth(1))
                    .and_then(|rest| rest.split('/').next())
                {
                    known_hosts.push(host.to_ascii_lowercase());
                }
                json!({
                    "api_slug": o.api_slug,
                    "api_name": o.api_name,
                    "provider": o.provider_name,
                    "method": o.endpoint_method,
                    "path": o.endpoint_path,
                    "description": o.endpoint_description,
                    "price_usd": o.price_usd,
                    "call_url": o.call_url,
                    "settlement": "apiosk-proxied",
                    "hosted_externally": o.hosted_externally,
                    "inputs": o.input_map,
                    "accepts_all_inputs": super::capabilities::missing_inputs(&m.input_contract, &o.input_map).is_empty(),
                    "missing_inputs": super::capabilities::missing_inputs(&m.input_contract, &o.input_map),
                    "performance": performance_json(stats.get(&o.api_slug), o.hosted_externally),
                })
            })
            .collect();

        rank(&mut shaped, preference);
        shaped.truncate(MAX_OFFERS_PER_CAPABILITY);

        results.push(json!({
            "capability": m.slug,
            "name": m.name,
            "description": m.description,
            "category": m.category,
            "relevance": (m.score * 10_000.0).round() / 10_000.0,
            // Why this capability surfaced for words that are not in its own
            // description.
            "matched_via": m.matched_via,
            "offer_count": m.offer_count,
            "compare_url": format!("{GATEWAY_BASE}/v1/capabilities/{}", m.slug),
            "offers": shaped,
        }));
    }

    let swept = if params.include_external {
        // The free-text query is the only phrasing a caller gave us, so it is
        // the whole variant set here — but it now reaches every index, not
        // just the Bazaar.
        crate::sources::sweep_with_status(
            &state,
            std::slice::from_ref(&query),
            &known_hosts,
            crate::sources::MAX_RETURNED,
        )
        .await
    } else {
        crate::sources::Sweep::default()
    };
    let external = &swept.offers;

    Ok(Json(json!({
        "query": query,
        "results": results,
        "meta": {
            "matched_capabilities": results.len(),
            "prefer": preference.as_str(),
            "ranking": preference.note(),
            "max_price": params.max_price,
            "no_match_means": "No capability cleared the relevance floor. That is an honest empty, not a fallback to the whole catalogue. Try fewer or plainer words, or browse GET /v1/capabilities. The wider x402 ecosystem is searched by default; see external_candidates below.",
            "performance_coverage": {
                "measured_listings": stats.len(),
                "basis": "Median and p95 over successful calls Apiosk proxied in the last 90 days, with the sample size published per offer. Payment challenges are excluded because their timing is the gateway's, not the provider's.",
                "gap": "Federated listings are never proxied and so are never timed. Result quality is not measured anywhere.",
            },
            "external_candidates": {
                "requested": params.include_external,
                "count": external.len(),
                "offers": crate::sources::settled_json(&state, external).await,
                "why_separate": "Search hits from external x402 indexes, not reviewed offers. Their input/output descriptions are provider claims, and discovery does not establish a usable input contract or complete coverage. Some may be purchased through Apiosk after independent contract and price validation; each offer states its settlement eligibility and indicative prices. Pass include_external=false for the catalogued set alone.",
                "sources_swept": crate::sources::source_ids(&state).await,
                "source_status": swept.sources_json(),
                "sources_failed": swept.sources_failed(),
                // How much of each index this search could actually see. A
                // sweep of seven sources that reads the first page of each is
                // not a sweep of the ecosystem, and until this block existed
                // the response could not say which of the two had happened.
                "index_depth": crate::sources::coverage_json(&state).await,
            },
        },
    }))
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(slug: &str, price: f64, p50: Option<f64>, rate: Option<f64>) -> Value {
        json!({
            "api_slug": slug,
            "price_usd": price,
            "performance": match (p50, rate) {
                (None, None) => json!({"measured": false}),
                _ => json!({"measured": true, "p50_ms": p50, "success_rate": rate}),
            }
        })
    }

    #[test]
    fn cheapest_is_the_default_and_orders_by_price() {
        assert_eq!(Preference::parse(None), Preference::Cheapest);
        assert_eq!(Preference::parse(Some("nonsense")), Preference::Cheapest);
        let mut o = vec![offer("b", 0.05, None, None), offer("a", 0.001, None, None)];
        rank(&mut o, Preference::Cheapest);
        assert_eq!(o[0]["api_slug"], "a");
    }

    #[test]
    fn fastest_puts_measured_offers_above_unmeasured_ones() {
        // An unmeasured offer sorted as though it were 0ms would put the
        // least-known supplier first, which is the opposite of the request.
        let mut o = vec![
            offer("unmeasured", 0.001, None, None),
            offer("slow", 0.05, Some(1415.0), None),
            offer("fast", 0.08, Some(198.0), None),
        ];
        rank(&mut o, Preference::Fastest);
        assert_eq!(o[0]["api_slug"], "fast");
        assert_eq!(o[1]["api_slug"], "slow");
        assert_eq!(o[2]["api_slug"], "unmeasured");
    }

    #[test]
    fn most_reliable_sorts_high_success_first() {
        let mut o = vec![
            offer("flaky", 0.001, None, Some(0.62)),
            offer("solid", 0.05, None, Some(0.99)),
            offer("unknown", 0.002, None, None),
        ];
        rank(&mut o, Preference::MostReliable);
        assert_eq!(o[0]["api_slug"], "solid");
        assert_eq!(o[1]["api_slug"], "flaky");
        assert_eq!(o[2]["api_slug"], "unknown");
    }

    #[test]
    fn ties_break_on_slug_so_the_order_is_stable() {
        let mut o = vec![offer("b", 0.05, None, None), offer("a", 0.05, None, None)];
        rank(&mut o, Preference::Cheapest);
        assert_eq!(o[0]["api_slug"], "a");
    }

    #[test]
    fn an_unmeasured_listing_reports_absence_not_zero() {
        let v = performance_json(None, false);
        assert_eq!(v["measured"], false);
        assert!(v.get("p50_ms").is_none(), "a zero here reads as instant");
        let fed = performance_json(None, true);
        assert!(fed["reason"].as_str().unwrap().contains("never proxies"));
    }

    #[test]
    fn every_preference_publishes_how_it_ranked() {
        for p in [
            Preference::Cheapest,
            Preference::Fastest,
            Preference::MostReliable,
        ] {
            assert!(!p.note().is_empty());
            assert!(!p.as_str().is_empty());
        }
        // The two measured rankings must disclose the coverage gap.
        assert!(Preference::Fastest.note().contains("no measurement"));
        assert!(Preference::MostReliable.note().contains("no measurement"));
    }
}

#[cfg(test)]
mod flag_tests {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct Probe {
        #[serde(default, deserialize_with = "lenient_bool")]
        flag: bool,
    }

    fn parse(qs: &str) -> bool {
        // The value serde sees for `flag=<v>`, without pulling in a query-string
        // crate just to build it.
        let value = qs.split_once('=').map(|(_, v)| v).unwrap_or("");
        let json = serde_json::json!({ "flag": value });
        serde_json::from_value::<Probe>(json).expect("parses").flag
    }

    #[test]
    fn the_documented_form_is_accepted() {
        // llms.txt, the OpenAPI description and this endpoint's own hint all
        // tell agents to send `=1`. serde's default bool 400s on it.
        for on in ["flag=1", "flag=true", "flag=yes", "flag=on", "flag=TRUE"] {
            assert!(parse(on), "{on} should enable the flag");
        }
    }

    #[test]
    fn anything_else_is_off_rather_than_an_error() {
        for off in ["flag=0", "flag=false", "flag=no", "flag=banana", "flag="] {
            assert!(!parse(off), "{off} should leave the flag off");
        }
        assert!(!parse(""), "an empty value means off");
    }
}

#[cfg(test)]
mod default_tests {
    use super::*;

    #[test]
    fn external_sources_are_searched_unless_refused() {
        let on: SearchQuery = serde_json::from_value(serde_json::json!({"q": "x"})).unwrap();
        assert!(on.include_external, "absent means on for this endpoint");

        for refusal in ["false", "0", "no", "off"] {
            let q: SearchQuery =
                serde_json::from_value(serde_json::json!({"q": "x", "include_external": refusal}))
                    .unwrap();
            assert!(!q.include_external, "{refusal} should turn it off");
        }
    }
}
