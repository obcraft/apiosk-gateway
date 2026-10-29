//! Keyless, read-only Ask Merlin UK VAT lookup. Never turn outages into invalid.
use super::Prepared;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use url::Url;

pub fn prepare(method: &str, original: &str) -> Result<Option<Prepared>, String> {
    let mut url = Url::parse(original).map_err(|_| "Invalid VAT URL")?;
    if method != "GET"
        || url.scheme() != "https"
        || url.host_str() != Some("askmerlin.com")
        || url.path() != "/api/lookup"
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("Unsupported UK VAT route".into());
    }
    let mut params = BTreeMap::new();
    for (k, v) in url.query_pairs() {
        if !["q", "max_age"].contains(&k.as_ref())
            || params.insert(k.to_string(), v.to_string()).is_some()
        {
            return Err("Unsupported or repeated UK VAT parameter".into());
        }
    }
    let number = params.get("q").ok_or("Missing UK VAT number")?;
    if number.len() != 11
        || !number.starts_with("GB")
        || !number[2..].bytes().all(|b| b.is_ascii_digit())
    {
        return Err(
            "Use GB followed by nine digits; this source does not search company names".into(),
        );
    }
    if params.get("max_age").is_some_and(|v| v != "3600") {
        return Err("UK VAT checks use a maximum one-hour cache age".into());
    }
    url.set_query(None);
    url.query_pairs_mut()
        .append_pair("q", number)
        .append_pair("max_age", "3600");
    Ok(Some(Prepared {
        url: url.to_string(),
        json_body: None,
        source: "ask-merlin".into(),
        params,
    }))
}

pub fn normalize(p: &Prepared, bytes: &[u8]) -> Result<Value, String> {
    let body: Value = serde_json::from_slice(bytes).map_err(|_| "Malformed UK VAT response")?;
    let data = &body["data"];
    if body["type"] != "ukvat"
        || data["countryCode"] != "GB"
        || data["vatNumber"].as_str() != p.params.get("q").map(String::as_str)
    {
        return Err("UK VAT response identity mismatch".into());
    }
    if !matches!(data["status"].as_str(), Some("valid" | "invalid"))
        || data["stale"] != false
        || !matches!(data["source"].as_str(), Some("live" | "cache"))
    {
        return Err(
            "UK VAT authority result is unavailable or stale; not a validation result".into(),
        );
    }
    let checked = data["checkedAt"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .ok_or("Missing UK VAT check timestamp")?;
    let age = chrono::Utc::now()
        .signed_duration_since(checked)
        .num_seconds();
    if !(-60..=3660).contains(&age) {
        return Err("UK VAT check is outside the allowed freshness window".into());
    }
    if data["valid"].as_bool() != Some(data["status"] == "valid")
        || (data["valid"] == true && !data["name"].as_str().is_some_and(|v| !v.trim().is_empty()))
    {
        return Err("Inconsistent UK VAT result".into());
    }
    Ok(
        json!({"data":data,"source_url":p.url,"coverage":{"complete":true,
        "scope":"One supplied GB VAT number, HMRC-derived response via Ask Merlin, at the recorded check time."},
        "notice":"A published candidate is not an entity identity match. Compare the registered name/address. An invalid candidate does not prove the company has no other registration. Anonymous lookups have no consultation reference; this is not an HMRC certificate."}),
    )
}
