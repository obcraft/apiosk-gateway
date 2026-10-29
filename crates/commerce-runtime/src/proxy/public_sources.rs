//! Narrow, read-only adapters for the three Dutch public sources. Normalization
//! happens before settlement/cache insertion: malformed source data is not sold.
#[path = "domain_intel.rs"]
mod domain_intel;
#[path = "dutch_links.rs"]
mod dutch_links;
#[path = "eu_public_sources.rs"]
mod eu_public_sources;
#[path = "eurostat_catalogue.rs"]
mod eurostat_catalogue;
#[path = "ted_links.rs"]
mod ted_links;
#[path = "uk_vat.rs"]
mod uk_vat;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use url::Url;

#[derive(Clone, Debug)]
pub struct Prepared {
    pub url: String,
    pub json_body: Option<Value>,
    source: String,
    params: BTreeMap<String, String>,
}

fn literal(value: &str) -> Result<String, String> {
    if value.is_empty() || value.chars().count() > 200 || value.chars().any(char::is_control) {
        return Err("Use a nonempty search value of at most 200 characters".into());
    }
    // CQL quoted terms: never interpolate raw operators or field expressions.
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}
fn integer(
    params: &BTreeMap<String, String>,
    key: &str,
    default: u64,
    min: u64,
    max: u64,
) -> Result<u64, String> {
    let value = params
        .get(key)
        .map(|v| v.parse::<u64>())
        .transpose()
        .map_err(|_| format!("Invalid {key}"))?
        .unwrap_or(default);
    if !(min..=max).contains(&value) {
        return Err(format!("{key} must be between {min} and {max}"));
    }
    Ok(value)
}
fn required<'a>(p: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str, String> {
    p.get(key)
        .map(String::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("Missing {key}"))
}

pub fn prepare(source: &str, method: &str, original: &str) -> Result<Option<Prepared>, String> {
    if source == "eurostat" {
        return eurostat_catalogue::prepare(method, original);
    }
    if source == "ask-merlin" {
        return uk_vat::prepare(method, original);
    }
    if domain_intel::handles(source) {
        return domain_intel::prepare(method, original);
    }
    if eu_public_sources::handles(source, original) {
        return eu_public_sources::prepare(source, method, original);
    }
    if source == "ted" {
        return ted_links::prepare(method, original);
    }
    if source == "pdok" {
        return dutch_links::prepare(method, original);
    }
    let (host, path, allowed): (&str, &str, &[&str]) = match source {
        "koop" => (
            "repository.overheid.nl",
            "/sru",
            &[
                "q",
                "authority",
                "from",
                "until",
                "start",
                "limit",
                "publication_id",
                "journal",
            ],
        ),
        "rdw" => ("opendata.rdw.nl", "/resource/m9d7-ebf2.json", &["kenteken"]),
        "rvo" => (
            "www.rvo.nl",
            "/api/v1/opendata/subsidies",
            &["q", "sector", "audience"],
        ),
        _ => return Ok(None),
    };
    let mut url = Url::parse(original).map_err(|_| "Invalid source URL")?;
    if method != "GET"
        || url.scheme() != "https"
        || url.host_str() != Some(host)
        || url.path() != path
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
    {
        return Err("Unsupported Dutch public source route".into());
    }
    let mut params = BTreeMap::new();
    for (key, value) in url.query_pairs() {
        if !allowed.contains(&key.as_ref()) || params.contains_key(key.as_ref()) {
            return Err(format!("Unsupported or repeated parameter: {key}"));
        }
        let value = value.trim().to_string();
        if value.len() > 800 {
            return Err("Input too long".into());
        }
        params.insert(key.into_owned(), value);
    }
    url.set_query(None);
    match source {
        "koop" => {
            let start = integer(&params, "start", 1, 1, 100_000)?;
            let limit = integer(&params, "limit", 10, 1, 25)?;
            let mut query = "c.product-area==officielepublicaties".to_owned();
            if let Some(journal) = params.get("journal") {
                if journal != "Staatscourant" {
                    return Err("Unsupported publication journal".into());
                }
                query.push_str(" AND w.publicatienaam==Staatscourant");
            }
            if let Some(id) = params.get("publication_id") {
                if params.keys().any(|k| k != "publication_id")
                    || id.len() > 100
                    || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                {
                    return Err("Use only a valid publication identifier for detail lookup".into());
                }
                query.push_str(&format!(" AND dt.identifier=={}", literal(id)?));
            } else {
                query.push_str(&format!(
                    " AND dt.title={}",
                    literal(required(&params, "q")?)?
                ));
                if let Some(authority) = params.get("authority") {
                    query.push_str(&format!(" AND dt.creator=={}", literal(authority)?));
                }
                for (key, op) in [("from", ">="), ("until", "<=")] {
                    if let Some(date) = params.get(key) {
                        let parsed = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
                            .map_err(|_| format!("Invalid {key} date"))?;
                        if parsed.to_string() != *date {
                            return Err(format!("Use YYYY-MM-DD for {key}"));
                        }
                        query.push_str(&format!(" AND dt.date{op}{date}"));
                    }
                }
                if let (Some(from), Some(until)) = (params.get("from"), params.get("until")) {
                    if from > until {
                        return Err("Start date must not follow end date".into());
                    }
                }
                query.push_str(" sortBy dt.modified/sort.descending");
            }
            url.query_pairs_mut().extend_pairs([
                ("operation", "searchRetrieve"),
                ("version", "2.0"),
                ("httpAccept", "application/json"),
                ("query", &query),
                ("startRecord", &start.to_string()),
                ("maximumRecords", &limit.to_string()),
            ]);
        }
        "rdw" => {
            let plate: String = required(&params, "kenteken")?
                .chars()
                .filter(|c| *c != '-' && *c != ' ')
                .collect::<String>()
                .to_ascii_uppercase();
            if plate.len() != 6 || !plate.bytes().all(|c| c.is_ascii_alphanumeric()) {
                return Err("Use a six-character Dutch registration plate".into());
            }
            params.insert("kenteken".into(), plate.clone());
            url.query_pairs_mut()
                .append_pair("kenteken", &plate)
                .append_pair("$limit", "2");
        }
        "rvo" => {
            literal(required(&params, "q")?)?;
            for v in params.values() {
                literal(v)?;
            }
            // RVO's verified public endpoint returns the first 50 descriptions.
            // Pin the explicit first page; the bare URL intermittently returns HTTP 500.
            url.query_pairs_mut().append_pair("page", "0");
        }
        _ => unreachable!(),
    }
    Ok(Some(Prepared {
        url: url.to_string(),
        source: source.into(),
        params,
        json_body: None,
    }))
}

fn text(value: &Value) -> Option<&str> {
    value
        .as_str()
        .or_else(|| value.get("$").and_then(Value::as_str))
}
fn records(value: &Value) -> Result<Vec<&Value>, String> {
    match value {
        Value::Array(rows) => Ok(rows.iter().collect()),
        Value::Object(_) => Ok(vec![value]),
        Value::Null => Ok(vec![]),
        _ => Err("Invalid source record list".into()),
    }
}
fn source_link(value: &str, host: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;
    (url.scheme() == "https"
        && url.host_str() == Some(host)
        && url.username().is_empty()
        && url.password().is_none())
    .then(|| url.to_string())
}

pub fn normalize(request: &Prepared, data: &Value) -> Result<Value, String> {
    if request.source == "ted-links" {
        return ted_links::normalize(request, data);
    }
    if request.source == "pdok-links" {
        return dutch_links::normalize(request, data);
    }
    let mut result = match request.source.as_str() {
        "koop" => {
            let response = data
                .get("searchRetrieveResponse")
                .ok_or("Missing SRU response")?;
            if response.get("diagnostics").is_some_and(|v| !v.is_null()) {
                return Err("KOOP rejected the search query".into());
            }
            let total = response["numberOfRecords"]
                .as_u64()
                .ok_or("Missing KOOP result count")?;
            let raw = records(&response["records"]["record"])?;
            let start = response["echoedSearchRetrieveRequest"]["startRecord"]
                .as_u64()
                .unwrap_or(integer(&request.params, "start", 1, 1, 100_000)?);
            let mut rows = vec![];
            for record in raw {
                let meta = &record["recordData"]["gzd"]["originalData"]["meta"];
                let kernel = &meta["owmskern"];
                let id = text(&kernel["identifier"]).ok_or("Missing publication identifier")?;
                let title = text(&kernel["title"]).ok_or("Missing publication title")?;
                if request
                    .params
                    .get("publication_id")
                    .is_some_and(|v| v != id)
                {
                    return Err("Publication identifier mismatch".into());
                }
                if request
                    .params
                    .get("journal")
                    .is_some_and(|v| text(&meta["tpmeta"]["publicatienaam"]) != Some(v.as_str()))
                {
                    return Err("Publication journal mismatch".into());
                }
                let links = records(&meta["owmsmantel"]["hasVersion"])?;
                let url = links
                    .iter()
                    .filter_map(|v| v["resourceIdentifier"].as_str())
                    .find_map(|v| source_link(v, "zoek.officielebekendmakingen.nl"));
                rows.push(json!({"id":id,"title":title,"authority":text(&kernel["creator"]),
                    "type":text(&kernel["type"]),"published":text(&meta["owmsmantel"]["date"]),
                    "modified":text(&kernel["modified"]),"summary":text(&meta["owmsmantel"]["abstract"]),
                    "url":url,"publication":text(&meta["tpmeta"]["publicatienaam"])}));
            }
            if rows.len() as u64 > total || (rows.is_empty() && total > 0 && start <= total) {
                return Err("Inconsistent KOOP page".into());
            }
            let next = start + rows.len() as u64;
            json!({"items":rows,"total":total,"start":start,"next_start":if next <= total && next > start {Some(next)}else{None},
                "coverage":{"complete":start==1 && next>total,"scope":"Official publication title search; sorted by modification date. Not consolidated current law."},
                "notice":"An application notice is not an issued permit. Company-name mentions do not establish identity or a complete permit history."})
        }
        "rdw" => {
            let rows = data.as_array().ok_or("RDW did not return an array")?;
            let plate = required(&request.params, "kenteken")?;
            if rows.len() > 1 || rows.iter().any(|v| v["kenteken"].as_str() != Some(plate)) {
                return Err("RDW exact-plate lookup mismatch".into());
            }
            json!({"items":rows,"total":rows.len(),"requested_plate":plate,"coverage":{"complete":true,"scope":"Exact plate in RDW registered-vehicle dataset"},
                "notice":"Technical register data. No owner identity or fleet ownership. Missing fields remain unknown; fuel/emissions datasets are not included."})
        }
        "rvo" => {
            let raw = data.as_array().ok_or("RVO did not return an array")?;
            if raw.len() > 50 {
                return Err("RVO page size changed; review source contract".into());
            }
            let terms: Vec<_> = required(&request.params, "q")?
                .split_whitespace()
                .map(str::to_lowercase)
                .collect();
            let mut rows = vec![];
            for row in raw {
                let id = row["id"].as_str().ok_or("Missing RVO identifier")?;
                let title = row["title"].as_str().ok_or("Missing RVO title")?;
                let haystack = [
                    "title",
                    "intro",
                    "sectors",
                    "subjects",
                    "targets",
                    "subsidies",
                    "tags",
                ]
                .iter()
                .filter_map(|k| row.get(*k).filter(|v| !v.is_null()).map(Value::to_string))
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();
                if !terms.iter().all(|term| haystack.contains(term)) {
                    continue;
                }
                if [("sector", "sectors"), ("audience", "targets")]
                    .iter()
                    .any(|(param, field)| {
                        request.params.get(*param).is_some_and(|wanted| {
                            !row[*field].as_array().is_some_and(|vs| {
                                vs.iter().any(|v| {
                                    v.as_str()
                                        .is_some_and(|s| s.to_lowercase() == wanted.to_lowercase())
                                })
                            })
                        })
                    })
                {
                    continue;
                }
                let raw_url = row["url"].as_str().ok_or("Missing RVO URL")?;
                let url = Url::parse("https://www.rvo.nl")
                    .unwrap()
                    .join(raw_url)
                    .map_err(|_| "Invalid RVO URL")?;
                let url =
                    source_link(url.as_str(), "www.rvo.nl").ok_or("Unexpected RVO link host")?;
                rows.push(json!({"id":id,"title":title,"description":row["intro"],"modified":row["changed"],
                    "sectors":row["sectors"],"subjects":row["subjects"],"audiences":row["targets"],"url":url,
                    "application_status":"unknown","eligibility":"not_assessed"}));
            }
            json!({"items":rows,"matched_in_page":rows.len(),"scanned":raw.len(),"total":null,
                "coverage":{"complete":false,"scope":"Keyword matches within the first public RVO page only (at most 50 descriptions). Later pages are not verified."},
                "notice":"Includes historical scheme descriptions. No matches in this page does not mean no subsidy exists. Current application status, deadlines and eligibility are not provided."})
        }
        _ => return Err("Unknown public source".into()),
    };
    result["source_url"] = json!(request.url);
    result["query"] = json!(request.params);
    Ok(result)
}

/// Validate raw formats before a successful call can settle.
pub fn normalize_bytes(p: &Prepared, bytes: &[u8]) -> Result<Value, String> {
    if p.source == "eurostat-catalogue" {
        return eurostat_catalogue::normalize(p, bytes);
    }
    if p.source == "ask-merlin" {
        return uk_vat::normalize(p, bytes);
    }
    if p.source.starts_with("eu:") {
        return eu_public_sources::normalize(p, bytes);
    }
    if p.source.starts_with("domain:") {
        return domain_intel::normalize(p, bytes);
    }
    let value = serde_json::from_slice(bytes)
        .map_err(|_| "Public source returned invalid JSON".to_string())?;
    normalize(p, &value)
}
pub fn bind_body(p: &mut Prepared, bytes: &[u8]) -> Result<(), String> {
    eu_public_sources::bind_body(p, bytes)
}
pub fn accept(p: &Prepared) -> Option<&'static str> {
    if p.source == "eurostat-catalogue" {
        return Some("text/plain");
    }
    eu_public_sources::accept(p)
}
pub fn multipart(p: &Prepared) -> Option<String> {
    eu_public_sources::multipart(p)
}
pub async fn follow_metadata_redirect(
    p: &Prepared,
    http: &reqwest::Client,
    response: reqwest::Response,
) -> Result<reqwest::Response, String> {
    eu_public_sources::follow_metadata_redirect(p, http, response).await
}
