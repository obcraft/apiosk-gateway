//! Coarse request geography, from the edge that accepted the connection.
//!
//! WHAT THIS IS NOT: a client-IP geolocation. We do not ship a GeoIP database,
//! and `gateway.apiosk.com` sits directly on Fly with no CDN in front of it
//! (`server: Fly`, `fly-request-id: …-ams`, no `cf-ray`), so the
//! `cf-ipcountry` / `x-vercel-ip-country` headers other stacks rely on never
//! arrive. Before this module every geo column in `gateway_requests_v2` was
//! hardcoded `None` — 398,751 rows with nothing in `country_code`, `region`,
//! `asn` or `network_name`.
//!
//! WHAT THIS IS: Fly Proxy anycasts to the PoP nearest the caller and stamps it
//! on `Fly-Region` (`ams`, `iad`, `nrt`, …). That is a real, free geography
//! signal — an agent fleet in AWS us-east-1 lands on `iad`, one in Frankfurt on
//! `fra`. It answers "which part of the world is the demand coming from" at
//! continent/country-of-PoP resolution, which is the question the portal
//! actually asks.
//!
//! It does NOT answer "which country is this specific caller in": a caller can
//! be routed to a neighbouring PoP, and one PoP serves many countries. So the
//! value is written as the country OF THE EDGE, `network_name` records which
//! PoP it came from, and `network_type` is stamped `fly-edge` so no reader can
//! mistake this for IP-level geolocation. Migration 071 puts the same warning
//! on the column comments.
//!
//! Upgrade path, when PoP resolution stops being enough: keep this as the
//! fallback and populate `country_code`/`asn` from a MaxMind GeoLite2 lookup on
//! `ip_address` in the writer. The columns and the flag do not change.
//!
//! Gated by `TELEMETRY_GEO_ENABLED` (default false), like the rest of v2.

use axum::http::HeaderMap;

/// Coarse geography for one request. All fields optional — enrichment is
/// best-effort and never fails a request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GeoInfo {
    /// ISO-3166-1 alpha-2 of the EDGE PoP, not of the client IP.
    pub country_code: Option<String>,
    /// Fly PoP code, e.g. `ams`.
    pub region: Option<String>,
    /// `fly-edge:<pop>` — makes the provenance self-describing in the row.
    pub network_name: Option<String>,
    /// Constant marker so queries can exclude edge-derived geo in one predicate.
    pub network_type: Option<String>,
}

/// Country of each Fly PoP. Fly region codes are IATA-derived and stable; an
/// unknown code yields `None` for the country but still records the PoP, so a
/// newly-opened region degrades to "we know where, not which country" rather
/// than losing the row.
fn fly_pop_country(pop: &str) -> Option<&'static str> {
    Some(match pop {
        "ams" => "NL",
        "arn" => "SE",
        "atl" | "bos" | "den" | "dfw" | "ewr" | "iad" | "lax" | "mia" | "ord" | "phx" | "sea"
        | "sjc" => "US",
        "bog" => "CO",
        "bom" => "IN",
        "cdg" => "FR",
        "eze" => "AR",
        "fra" => "DE",
        "gdl" | "qro" => "MX",
        "gig" | "gru" => "BR",
        "hkg" => "HK",
        "jnb" => "ZA",
        "lhr" => "GB",
        "mad" => "ES",
        "nrt" => "JP",
        "otp" => "RO",
        "scl" => "CL",
        "sin" => "SG",
        "syd" => "AU",
        "waw" => "PL",
        "yul" | "yyz" => "CA",
        _ => return None,
    })
}

/// Read a header as a trimmed, lowercased, bounded string. Values come straight
/// off the wire, so cap the length and reject anything that is not a plausible
/// region code before it reaches a `text` column.
fn header_pop(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers
        .get(name)?
        .to_str()
        .ok()?
        .trim()
        .to_ascii_lowercase();
    if raw.is_empty() || raw.len() > 8 || !raw.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(raw)
}

/// Best-effort geography for a request.
///
/// `Fly-Region` is injected by Fly Proxy and cannot be set by a caller: the
/// proxy overwrites any client-supplied copy. We also accept the two common CDN
/// country headers, so putting Cloudflare (or Vercel) in front later starts
/// producing TRUE client-IP geography with no code change — a real country
/// always wins over the PoP approximation.
pub fn enrich(headers: &HeaderMap) -> GeoInfo {
    let pop = header_pop(headers, "fly-region");

    // A CDN that actually geolocates the client beats the edge approximation.
    let cdn_country = headers
        .get("cf-ipcountry")
        .or_else(|| headers.get("x-vercel-ip-country"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_ascii_uppercase())
        .filter(|s| s.len() == 2 && s.chars().all(|c| c.is_ascii_alphabetic()));

    let from_cdn = cdn_country.is_some();
    let country_code =
        cdn_country.or_else(|| pop.as_deref().and_then(fly_pop_country).map(str::to_string));

    GeoInfo {
        country_code,
        region: pop.clone(),
        network_name: pop.as_ref().map(|p| format!("fly-edge:{p}")),
        network_type: pop.as_ref().map(|_| {
            if from_cdn {
                // Country is client-accurate; the PoP is still recorded.
                "cdn-geo".to_string()
            } else {
                "fly-edge".to_string()
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn hm(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn fly_region_yields_pop_and_country() {
        let g = enrich(&hm(&[("fly-region", "ams")]));
        assert_eq!(g.region.as_deref(), Some("ams"));
        assert_eq!(g.country_code.as_deref(), Some("NL"));
        assert_eq!(g.network_name.as_deref(), Some("fly-edge:ams"));
        assert_eq!(g.network_type.as_deref(), Some("fly-edge"));
    }

    #[test]
    fn unknown_pop_keeps_region_drops_country() {
        // A newly-opened Fly region must not lose the PoP just because the
        // country table hasn't caught up.
        let g = enrich(&hm(&[("fly-region", "xyz")]));
        assert_eq!(g.region.as_deref(), Some("xyz"));
        assert_eq!(g.country_code, None);
        assert_eq!(g.network_name.as_deref(), Some("fly-edge:xyz"));
    }

    #[test]
    fn cdn_country_beats_pop_approximation() {
        // Caller is in Germany but routed to the Amsterdam PoP: trust the CDN.
        let g = enrich(&hm(&[("fly-region", "ams"), ("cf-ipcountry", "de")]));
        assert_eq!(g.country_code.as_deref(), Some("DE"));
        assert_eq!(g.region.as_deref(), Some("ams"));
        assert_eq!(g.network_type.as_deref(), Some("cdn-geo"));
    }

    #[test]
    fn no_headers_is_empty_not_garbage() {
        assert_eq!(enrich(&HeaderMap::new()), GeoInfo::default());
    }

    #[test]
    fn rejects_implausible_region_values() {
        // Bounded + charset-checked before it reaches a text column.
        assert_eq!(enrich(&hm(&[("fly-region", "../../etc")])).region, None);
        assert_eq!(
            enrich(&hm(&[("fly-region", "waaaaaaaytoolong")])).region,
            None
        );
        assert_eq!(enrich(&hm(&[("fly-region", "")])).region, None);
    }

    #[test]
    fn rejects_malformed_cdn_country() {
        // Not two alphabetic characters ⇒ ignored, fall back to the PoP.
        let g = enrich(&hm(&[("fly-region", "ams"), ("cf-ipcountry", "XX1")]));
        assert_eq!(g.country_code.as_deref(), Some("NL"));
    }
}
