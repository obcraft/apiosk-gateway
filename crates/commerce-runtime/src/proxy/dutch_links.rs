//! Identifier-preserving, single-upstream-call links between public Dutch datasets.
use super::Prepared;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use url::Url;
const BASE: &str = "https://api.pdok.nl";
const SEARCH: &str = "/bzk/locatieserver/search/v3_1/free";
fn digits(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| b.is_ascii_digit())
}
fn text<'a>(p: &'a BTreeMap<String, String>, k: &str) -> Result<&'a str, String> {
    p.get(k)
        .map(String::as_str)
        .ok_or_else(|| format!("Missing {k}"))
}
fn building_url(s: &str) -> Result<Url, String> {
    let u = Url::parse(s).map_err(|_| "Invalid building link")?;
    let prefix = "/kadaster/bag/ogc/v2/collections/pand/items/";
    let tail = u
        .path()
        .strip_prefix(prefix)
        .ok_or("Invalid building path")?;
    if u.scheme() != "https"
        || u.host_str() != Some("api.pdok.nl")
        || u.port().is_some()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
        || uuid::Uuid::parse_str(tail).is_err()
    {
        return Err("Use a canonical PDOK building link returned by the BAG record".into());
    }
    Ok(u)
}
pub fn prepare(method: &str, original: &str) -> Result<Option<Prepared>, String> {
    let input = Url::parse(original).map_err(|_| "Invalid PDOK URL")?;
    if !input.path().starts_with("/apiosk/") {
        return Ok(None);
    }
    if method != "GET"
        || input.scheme() != "https"
        || input.host_str() != Some("api.pdok.nl")
        || input.port().is_some()
        || !input.username().is_empty()
        || input.password().is_some()
        || input.fragment().is_some()
    {
        return Err("Unsupported PDOK link route".into());
    }
    let route = input.path().trim_start_matches("/apiosk/");
    let key = match route {
        "company-address" => "addresses",
        "bag-address" | "bag-unit" => "vbo",
        "building" => "href",
        "neighbourhood" => "code",
        "linked-parcels" => "designations",
        _ => return Err("Unknown PDOK link route".into()),
    };
    let mut p = BTreeMap::new();
    for (k, v) in input.query_pairs() {
        if k != key || p.contains_key(k.as_ref()) || v.len() > 16000 {
            return Err("Unsupported or repeated PDOK link input".into());
        }
        p.insert(k.into_owned(), v.into_owned());
    }
    let value = text(&p, key)?.to_owned();
    let mut url = Url::parse(BASE).unwrap();
    match route {
        "company-address" => {
            let rows: Value =
                serde_json::from_str(&value).map_err(|_| "Expected KVK address array")?;
            let addresses = rows.as_array().ok_or("Expected KVK address array")?;
            let visiting: Vec<_> = addresses
                .iter()
                .filter(|a| a["type"] == "bezoekadres" && a["indAfgeschermd"] != "Ja")
                .collect();
            if visiting.len() != 1 {
                return Err("A single disclosed visiting address is required; choose the company establishment first".into());
            }
            let a = visiting[0];
            if a["land"] != "Nederland" {
                return Err("BAG linking requires a Dutch visiting address".into());
            }
            let postcode = a["postcode"]
                .as_str()
                .ok_or("Missing postcode")?
                .replace(' ', "")
                .to_ascii_uppercase();
            if !postcode.is_ascii()
                || postcode.len() != 6
                || !digits(&postcode[..4], 4)
                || !postcode[4..].bytes().all(|b| b.is_ascii_uppercase())
            {
                return Err("Invalid postcode".into());
            }
            let number = a["huisnummer"]
                .as_u64()
                .filter(|v| *v > 0 && *v < 100000)
                .ok_or("Missing numeric house number")?;
            p.insert("postcode".into(), postcode.clone());
            p.insert("number".into(), number.to_string());
            for (k, field) in [
                ("letter", "huisletter"),
                ("addition", "huisnummerToevoeging"),
            ] {
                p.insert(
                    k.into(),
                    a[field].as_str().unwrap_or("").trim().to_ascii_uppercase(),
                );
            }
            url.set_path(SEARCH);
            url.query_pairs_mut()
                .append_pair("q", "*:*")
                .append_pair(
                    "fq",
                    &format!("type:adres AND postcode:{postcode} AND huisnummer:{number}"),
                )
                .append_pair("rows", "100");
        }
        "bag-address" => {
            if !digits(&value, 16) {
                return Err("Use a 16-digit BAG addressable-object identifier".into());
            }
            url.set_path(SEARCH);
            url.query_pairs_mut()
                .append_pair("q", "*:*")
                .append_pair(
                    "fq",
                    &format!("type:adres AND adresseerbaarobject_id:{value}"),
                )
                .append_pair("rows", "100");
        }
        "bag-unit" => {
            if !digits(&value, 16) {
                return Err("Use a 16-digit BAG verblijfsobject identifier".into());
            }
            url.set_path("/kadaster/bag/ogc/v2/collections/verblijfsobject/items");
            url.query_pairs_mut()
                .append_pair("identificatie", &value)
                .append_pair("limit", "2")
                .append_pair("f", "json");
        }
        "building" => {
            url = building_url(&value)?;
            url.query_pairs_mut().append_pair("f", "json");
        }
        "neighbourhood" => {
            if value.len() != 10
                || !value.starts_with("BU")
                || !value[2..].bytes().all(|b| b.is_ascii_alphanumeric())
            {
                return Err("Use a CBS neighbourhood code, including BU prefix".into());
            }
            url.set_path("/cbs/wijken-en-buurten-2025/ogc/v1/collections/buurten/items");
            url.query_pairs_mut()
                .append_pair("buurtcode", &value)
                .append_pair("limit", "2")
                .append_pair("f", "json");
        }
        "linked-parcels" => {
            let codes: Vec<String> =
                serde_json::from_str(&value).map_err(|_| "Expected parcel designation array")?;
            if codes.is_empty()
                || codes.len() > 25
                || codes.iter().any(|c| {
                    let v: Vec<_> = c.split('-').collect();
                    v.len() != 3
                        || v[0].len() != 5
                        || !v[0].bytes().all(|b| b.is_ascii_alphanumeric())
                        || !(1..=2).contains(&v[1].len())
                        || !v[1].bytes().all(|b| b.is_ascii_uppercase())
                        || v[2].is_empty()
                        || !v[2].bytes().all(|b| b.is_ascii_digit())
                })
            {
                return Err("Use 1–25 cadastral parcel designations, e.g. ASD13-Q-5400; apartment-right IDs are distinct".into());
            }
            url.set_path(SEARCH);
            url.query_pairs_mut()
                .append_pair("q", "*:*")
                .append_pair(
                    "fq",
                    &format!(
                        "type:perceel AND identificatie:({})",
                        codes
                            .iter()
                            .map(|v| format!("\"{v}\""))
                            .collect::<Vec<_>>()
                            .join(" OR ")
                    ),
                )
                .append_pair("rows", "25");
        }
        _ => unreachable!(),
    }
    p.insert("route".into(), route.into());
    Ok(Some(Prepared {
        url: url.to_string(),
        source: "pdok-links".into(),
        params: p,
        json_body: None,
    }))
}
pub fn normalize(p: &Prepared, data: &Value) -> Result<Value, String> {
    let route = text(&p.params, "route")?;
    let mut items = vec![];
    let mut missing_values = vec![];
    let mut complete = true;
    let mut notice="Source-linked location context. This does not establish property ownership, current zoning or company-level statistics.";
    match route {
        "company-address" | "bag-address" | "linked-parcels" => {
            let response = &data["response"];
            let docs = response["docs"]
                .as_array()
                .ok_or("Missing PDOK documents")?;
            let count = response["numFound"].as_u64().ok_or("Missing PDOK total")?;
            if count != docs.len() as u64 {
                return Err(
                    "PDOK link response is incomplete; narrow the address or parcel selection"
                        .into(),
                );
            }
            let codes: Vec<String> = if route == "linked-parcels" {
                serde_json::from_str(text(&p.params, "designations")?)
                    .map_err(|_| "Invalid parcel selection")?
            } else {
                vec![]
            };
            for d in docs {
                if route == "linked-parcels" {
                    let code = d["identificatie"]
                        .as_str()
                        .ok_or("Missing parcel identity")?;
                    if d["type"] != "perceel" || !codes.iter().any(|v| v == code) {
                        return Err("Parcel identity mismatch".into());
                    }
                    items.push(d.clone());
                    continue;
                }
                if d["type"] != "adres" {
                    return Err("Expected BAG address".into());
                }
                if route == "company-address" {
                    if d["postcode"].as_str() != Some(text(&p.params, "postcode")?)
                        || d["huisnummer"].as_u64().map(|n| n.to_string()).as_deref()
                            != Some(text(&p.params, "number")?)
                    {
                        return Err("PDOK address identity mismatch".into());
                    }
                    let letter = d["huisletter"]
                        .as_str()
                        .unwrap_or("")
                        .trim()
                        .to_ascii_uppercase();
                    let addition = d["huisnummertoevoeging"]
                        .as_str()
                        .unwrap_or("")
                        .trim()
                        .to_ascii_uppercase();
                    if letter != text(&p.params, "letter")?
                        || addition != text(&p.params, "addition")?
                    {
                        continue;
                    }
                } else if d["adresseerbaarobject_id"].as_str() != Some(text(&p.params, "vbo")?) {
                    return Err("BAG identity mismatch".into());
                }
                let vbo = d["adresseerbaarobject_id"]
                    .as_str()
                    .filter(|s| digits(s, 16))
                    .ok_or("Missing BAG addressable-object ID")?;
                items.push(json!({"address":d["weergavenaam"],"bag_object_id":vbo,"bag_address_id":d["nummeraanduiding_id"],"municipality_code":d["gemeentecode"],"municipality":d["gemeentenaam"],"neighbourhood_code":d["buurtcode"],"neighbourhood":d["buurtnaam"],"district_code":d["wijkcode"],"district":d["wijknaam"],"parcel_designations":d["gekoppeld_perceel"],"apartment_right_designations":d["gekoppeld_appartement"],"centroid_wgs84":d["centroide_ll"],"source_record":d}));
            }
            if route == "linked-parcels" {
                complete = items.len()
                    == codes
                        .iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        .len();
            }
        }
        "bag-unit" | "neighbourhood" => {
            let features = data["features"]
                .as_array()
                .ok_or("Missing source features")?;
            if features.len() > 1
                || data["links"]
                    .as_array()
                    .is_some_and(|l| l.iter().any(|v| v["rel"] == "next"))
            {
                return Err("Exact identifier returned multiple or paginated records".into());
            }
            for f in features {
                let v = &f["properties"];
                let (field, key) = if route == "bag-unit" {
                    ("identificatie", "vbo")
                } else {
                    ("buurtcode", "code")
                };
                if v[field].as_str() != Some(text(&p.params, key)?) {
                    return Err("Source feature identity mismatch".into());
                }
                let mut row = v.clone();
                if route == "bag-unit" {
                    if let Some(hrefs) = v["pand.href"].as_array() {
                        for href in hrefs {
                            building_url(href.as_str().ok_or("Invalid building link")?)?;
                        }
                        if hrefs.len() == 1 {
                            row["single_building_href"] = hrefs[0].clone();
                        }
                    }
                } else if v["jaar"] != 2025 {
                    return Err("CBS period mismatch".into());
                } else if let Some(properties) = row.as_object_mut() {
                    // CBS 2025 special values are absence, not statistics:
                    // https://www.cbs.nl/nl-nl/longread/diversen/2025/toelichting-wijk-en-buurtkaart-2023-2024-en-2025?onepage=true
                    for (field, value) in properties {
                        let reason = match value.as_f64() {
                            Some(-99997.0) => "suppressed_or_not_present",
                            Some(-99995.0) => "publication_pending",
                            Some(-99991.0) => "no_longer_published",
                            _ => continue,
                        };
                        // Retain the original code as labelled provenance,
                        // outside the records, never as another numeric fact.
                        missing_values.push(json!({"field":field,
                            "source_value":format!("CBS special code {value}"),"reason":reason}));
                        *value = Value::Null;
                    }
                }
                items.push(row);
            }
            if route == "neighbourhood" {
                notice="CBS neighbourhood statistics for 2025, delivered by PDOK. Suppressed, unavailable, pending or discontinued statistics are null, not numeric values; original special codes are recorded in coverage.missing_values. Area aggregates are not company facts or an individual property WOZ value.";
            }
        }
        "building" => {
            let expected = building_url(text(&p.params, "href")?)?;
            if data["id"].as_str() != expected.path().rsplit('/').next() {
                return Err("Building feature identity mismatch".into());
            }
            let v = &data["properties"];
            if !v["identificatie"].as_str().is_some_and(|s| digits(s, 16)) {
                return Err("Missing BAG building ID".into());
            }
            items.push(v.clone());
        }
        _ => return Err("Unknown link adapter".into()),
    }
    let matched = if items.len() == 1 {
        items[0].clone()
    } else {
        Value::Null
    };
    let mut result = json!({"items":items,"matched":matched,"count":items.len(),"coverage":{"complete":complete,"scope":notice},"notice":notice,"source_url":p.url,"route":route});
    if route == "neighbourhood" {
        result["coverage"]["missing_values"] = json!(missing_values);
    }
    Ok(result)
}
