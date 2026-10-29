//! Public EU/NL sources: bounded requests and validation before settlement.
use super::{integer, literal, required, Prepared};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use url::Url;

pub fn handles(source: &str, url: &str) -> bool {
    matches!(
        source,
        "vies" | "eurlex-cellar" | "eu-funding-tenders" | "pdok-ruimtelijke-plannen" | "bodemloket"
    ) || (source == "koop" && url.contains("/sru/Search"))
}
pub fn prepare(source: &str, method: &str, original: &str) -> Result<Option<Prepared>, String> {
    let mut u = Url::parse(original).map_err(|_| "Invalid source URL")?;
    let host = match source {
        "vies" => "ec.europa.eu",
        "koop" => "zoekservice.overheid.nl",
        "eurlex-cellar" => "publications.europa.eu",
        "eu-funding-tenders" => "api.tech.ec.europa.eu",
        "pdok-ruimtelijke-plannen" => "service.pdok.nl",
        "bodemloket" => "gis.gdngeoservices.nl",
        _ => return Ok(None),
    };
    if u.scheme() != "https"
        || u.host_str() != Some(host)
        || u.port().is_some()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
    {
        return Err("Unsupported public source origin".into());
    }
    let mut p = BTreeMap::new();
    for (k, v) in u.query_pairs() {
        if p.insert(k.into_owned(), v.into_owned()).is_some() {
            return Err("Repeated source parameter".into());
        }
    }
    let path = u.path().to_string();
    let mut body = None;
    let allowed: &[&str] = match source {
        "vies" => &[],
        "koop" => &["x-connection", "q", "start", "limit"],
        "eurlex-cellar" if path == "/webapi/rdf/sparql" => &["query"],
        "eurlex-cellar" => &[],
        "eu-funding-tenders" => &["q", "page", "limit", "language"],
        _ => &[
            "SERVICE",
            "VERSION",
            "REQUEST",
            "CRS",
            "BBOX",
            "WIDTH",
            "HEIGHT",
            "I",
            "J",
            "LAYERS",
            "QUERY_LAYERS",
            "INFO_FORMAT",
            "FEATURE_COUNT",
            "STYLES",
            "FORMAT",
        ],
    };
    if p.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err("Unsupported source parameter".into());
    }
    u.set_query(None);
    match source {
        "vies" => {
            let base = "/taxation_customs/vies/rest-api/";
            if (method == "GET" && path == format!("{base}check-status"))
                || (method == "POST" && path == format!("{base}check-vat-number")) {
            } else if method == "GET" && path.starts_with(&format!("{base}ms/")) {
                let parts: Vec<_> = path[base.len()..].split('/').collect();
                if parts.len() != 4 || parts[2] != "vat" {
                    return Err("Invalid VAT route".into());
                }
                vat(parts[1], parts[3])?;
                p.insert("countryCode".into(), parts[1].into());
                p.insert("vatNumber".into(), parts[3].into());
            } else {
                return Err("Unsupported VIES route".into());
            }
        }
        "koop" => {
            if method != "GET" || path != "/sru/Search" {
                return Err("Unsupported KOOP route".into());
            }
            let col = required(&p, "x-connection")?;
            let field = match col {
                "cvdr" => "dcterms.title",
                "BWB" => "overheidbwb.titel",
                _ => return Err("Use cvdr or BWB".into()),
            };
            let query = format!("{field} any {}", literal(required(&p, "q")?)?);
            let start = integer(&p, "start", 1, 1, 100000)?;
            let limit = integer(&p, "limit", 10, 1, 25)?;
            u.query_pairs_mut().extend_pairs([
                ("x-connection", col),
                ("operation", "searchRetrieve"),
                ("version", "2.0"),
                ("query", &query),
                ("startRecord", &start.to_string()),
                ("maximumRecords", &limit.to_string()),
            ]);
        }
        "eurlex-cellar" => {
            if method != "GET" {
                return Err("Use GET for Cellar".into());
            }
            if path == "/webapi/rdf/sparql" {
                let q = required(&p, "query")?;
                let upper = q.to_ascii_uppercase();
                let tokens: Vec<_> = upper
                    .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .filter(|s| !s.is_empty())
                    .collect();
                if q.len() > 8000
                    || !tokens.contains(&"SELECT")
                    || tokens.iter().any(|s| {
                        [
                            "SERVICE",
                            "LOAD",
                            "INSERT",
                            "DELETE",
                            "CLEAR",
                            "DROP",
                            "CREATE",
                            "MOVE",
                            "COPY",
                            "ADD",
                            "CONSTRUCT",
                            "DESCRIBE",
                        ]
                        .contains(s)
                    })
                {
                    return Err(
                        "Use a bounded read-only SELECT query without external SERVICE".into(),
                    );
                }
                let limit = tokens
                    .windows(2)
                    .rev()
                    .find(|w| w[0] == "LIMIT")
                    .and_then(|w| w[1].parse::<u64>().ok())
                    .filter(|v| (1..=100).contains(v))
                    .ok_or("SPARQL needs LIMIT 1..100")?;
                p.insert("limit".into(), limit.to_string());
                let q = p["query"].clone();
                u.query_pairs_mut()
                    .append_pair("query", &q)
                    .append_pair("format", "application/sparql-results+json");
            } else if let Some(celex) = path.strip_prefix("/resource/celex/") {
                if celex.len() < 6
                    || celex.len() > 40
                    || !celex
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'(' || b == b')' || b == b'-')
                {
                    return Err("Invalid CELEX identifier".into());
                }
                p.insert("celex".into(), celex.into());
            } else {
                return Err("Unsupported Cellar route".into());
            }
        }
        "eu-funding-tenders" => {
            if method != "POST" || path != "/search-api/prod/rest/search" {
                return Err("Unsupported Funding route".into());
            }
            literal(required(&p, "q")?)?;
            let page = integer(&p, "page", 1, 1, 1000)?;
            let limit = integer(&p, "limit", 10, 1, 25)?;
            let language = p.get("language").map(String::as_str).unwrap_or("en");
            if language.len() != 2 || !language.bytes().all(|b| b.is_ascii_lowercase()) {
                return Err("Use a two-letter language code".into());
            }
            u.query_pairs_mut().extend_pairs([
                ("apiKey", "SEDIA"),
                ("text", required(&p, "q")?),
                ("pageNumber", &page.to_string()),
                ("pageSize", &limit.to_string()),
                ("language", language),
            ]);
            body = Some(
                json!({"bool":{"must":[{"terms":{"type":["1","2","8"]}},{"term":{"language":language}}],"must_not":[{"terms":{"DATASOURCE":["SEDIA_FAQ"]}}]}}),
            );
        }
        _ => {
            let expected = if source == "bodemloket" {
                "/standalone/services/blk_gdn/lks_blk_rd_v1/MapServer/WMSServer"
            } else {
                "/kadaster/ruimtelijke-plannen/wms/v1_0"
            };
            if method != "GET" || path != expected {
                return Err("Unsupported WMS route".into());
            }
            for (k, v) in [
                ("SERVICE", "WMS"),
                ("VERSION", "1.3.0"),
                ("REQUEST", "GetFeatureInfo"),
                ("CRS", "EPSG:28992"),
            ] {
                if p.get(k).is_some_and(|x| x != v) {
                    return Err(format!("{k} must be {v}"));
                }
                p.insert(k.into(), v.into());
            }
            let bbox: Vec<f64> = required(&p, "BBOX")?
                .split(',')
                .map(|v| v.parse().map_err(|_| "Invalid BBOX"))
                .collect::<Result<_, _>>()?;
            if bbox.len() != 4
                || bbox.iter().any(|v| !v.is_finite())
                || bbox[0] >= bbox[2]
                || bbox[1] >= bbox[3]
                || bbox[2] - bbox[0] > 10000.
                || bbox[3] - bbox[1] > 10000.
            {
                return Err("Use a finite ordered RD BBOX at most 10 km wide/high".into());
            }
            let w = integer(&p, "WIDTH", 101, 1, 2048)?;
            let h = integer(&p, "HEIGHT", 101, 1, 2048)?;
            integer(&p, "I", 50, 0, w - 1)?;
            integer(&p, "J", 50, 0, h - 1)?;
            integer(&p, "FEATURE_COUNT", 10, 1, 25)?;
            let layers = required(&p, "LAYERS")?;
            let valid = if source == "bodemloket" {
                vec!["WBB_locaties", "Beschikbaarheid_gegevens", "Bevoegd_gezag"]
            } else {
                vec![
                    "enkelbestemming",
                    "dubbelbestemming",
                    "bouwvlak",
                    "bouwaanduiding",
                    "figuur",
                    "functieaanduiding",
                    "gebiedsaanduiding",
                    "lettertekenaanduiding",
                    "maatvoering",
                    "plangebied",
                    "bestemmingsplangebied",
                ]
            };
            if !valid.contains(&layers) {
                return Err("Unsupported WMS layer".into());
            }
            if p.get("QUERY_LAYERS").is_some_and(|x| x != layers) {
                return Err("QUERY_LAYERS must match LAYERS".into());
            }
            p.insert("QUERY_LAYERS".into(), layers.into());
            let fmt = if source == "bodemloket" {
                "application/geo+json"
            } else {
                "application/json"
            };
            if p.get("INFO_FORMAT").is_some_and(|x| x != fmt) {
                return Err("Use the documented JSON info format".into());
            }
            p.insert("INFO_FORMAT".into(), fmt.into());
            for (k, v) in [
                ("WIDTH", "101"),
                ("HEIGHT", "101"),
                ("I", "50"),
                ("J", "50"),
                ("FEATURE_COUNT", "10"),
                ("STYLES", ""),
                ("FORMAT", "image/png"),
            ] {
                p.entry(k.into()).or_insert(v.into());
            }
            u.query_pairs_mut().extend_pairs(p.iter());
        }
    }
    Ok(Some(Prepared {
        url: u.to_string(),
        json_body: body,
        source: format!("eu:{source}"),
        params: p,
    }))
}
fn vat(cc: &str, nr: &str) -> Result<(), String> {
    if ![
        "AT", "BE", "BG", "CY", "CZ", "DE", "DK", "EE", "EL", "ES", "FI", "FR", "HR", "HU", "IE",
        "IT", "LT", "LU", "LV", "MT", "NL", "PL", "PT", "RO", "SE", "SI", "SK", "XI",
    ]
    .contains(&cc)
        || nr.is_empty()
        || nr.len() > 14
        || !nr
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    {
        return Err(
            "Use a VIES country code and VAT number without country prefix or punctuation".into(),
        );
    }
    Ok(())
}
pub fn bind_body(p: &mut Prepared, bytes: &[u8]) -> Result<(), String> {
    if p.source != "eu:vies" || !p.url.ends_with("/check-vat-number") {
        return Ok(());
    }
    let data: Value = serde_json::from_slice(bytes).map_err(|_| "Invalid VAT JSON body")?;
    let obj = data.as_object().ok_or("VAT body must be an object")?;
    if obj.keys().any(|k| {
        ![
            "countryCode",
            "vatNumber",
            "requesterMemberStateCode",
            "requesterNumber",
        ]
        .contains(&k.as_str())
    }) {
        return Err("Unsupported VAT body field".into());
    }
    let cc = data["countryCode"].as_str().ok_or("Missing countryCode")?;
    let nr = data["vatNumber"].as_str().ok_or("Missing vatNumber")?;
    vat(cc, nr)?;
    if obj.contains_key("requesterMemberStateCode") || obj.contains_key("requesterNumber") {
        vat(
            data["requesterMemberStateCode"]
                .as_str()
                .ok_or("Missing requester country")?,
            data["requesterNumber"]
                .as_str()
                .ok_or("Missing requester VAT")?,
        )?;
    }
    p.params.insert("countryCode".into(), cc.into());
    p.params.insert("vatNumber".into(), nr.into());
    p.json_body = Some(data);
    Ok(())
}
pub fn accept(p: &Prepared) -> Option<&'static str> {
    match p.source.as_str() {
        "eu:koop" => Some("application/xml"),
        "eu:eurlex-cellar" if p.params.contains_key("celex") => {
            Some("application/xml;notice=object")
        }
        "eu:eurlex-cellar" => Some("application/sparql-results+json"),
        s if s.starts_with("eu:") => Some("application/json"),
        _ => None,
    }
}
pub fn multipart(p: &Prepared) -> Option<String> {
    (p.source=="eu:eu-funding-tenders").then(|| {
 let fields=json!(["title","identifier","callIdentifier","callTitle","status","startDate","deadlineDate","deadlineModel","programmePeriod","frameworkProgramme","type","DATASOURCE","budgetOverview"]);
 format!("--apiosk-public-source\r\nContent-Disposition: form-data; name=\"query\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--apiosk-public-source\r\nContent-Disposition: form-data; name=\"displayFields\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--apiosk-public-source--\r\n",p.json_body.as_ref().unwrap(),fields)
 })
}

pub fn normalize(p: &Prepared, bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("Public source response exceeds 4 MiB".into());
    }
    let source = p.source.trim_start_matches("eu:");
    let (data, notice) = if source == "koop"
        || (source == "eurlex-cellar" && p.params.contains_key("celex"))
    {
        let xml = std::str::from_utf8(bytes).map_err(|_| "Invalid XML encoding")?;
        let doc = roxmltree::Document::parse(xml).map_err(|_| "Invalid source XML")?;
        if doc.descendants().any(|n| {
            n.is_element()
                && ["diagnostic", "ServiceException", "Exception"].contains(&n.tag_name().name())
        }) {
            return Err("Source returned an XML diagnostic".into());
        }
        if source == "koop" {
            if doc.root_element().tag_name().name() != "searchRetrieveResponse" {
                return Err("Missing SRU response".into());
            }
            let total = desc(doc.root_element(), "numberOfRecords")
                .and_then(|s| s.parse::<u64>().ok())
                .ok_or("Missing SRU total")?;
            let mut rows = vec![];
            for n in doc.descendants().filter(|n| n.has_tag_name("record")) {
                let id = desc(n, "identifier").ok_or("Missing regulation identifier")?;
                let title = desc(n, "title")
                    .or_else(|| desc(n, "titel"))
                    .ok_or("Missing regulation title")?;
                rows.push(json!({"id":id,"title":title,"metadata":leaf_fields(n),"url":desc(n,"preferred_url").or_else(||desc(n,"publicatieurl_html"))}));
            }
            let limit = integer(&p.params, "limit", 10, 1, 25)?;
            let start = integer(&p.params, "start", 1, 1, 100000)?;
            if rows.len() > limit as usize
                || total < rows.len() as u64
                || (rows.is_empty() && start <= total)
            {
                return Err("Inconsistent SRU page".into());
            }
            let next = start + rows.len() as u64;
            (json!({"items":rows,"total":total,"start":start,"next_start":if next<=total&&next<=100000{Some(next)}else{None},"collection":p.params["x-connection"]}),"Returned regulation metadata may include historic versions. Check effective dates and original text before drawing legal conclusions.")
        } else {
            if doc.root_element().tag_name().name() != "NOTICE" {
                return Err("Missing Cellar notice".into());
            }
            let celex = &p.params["celex"];
            let work = doc
                .descendants()
                .find(|n| n.has_tag_name("WORK"))
                .ok_or("Missing Cellar work")?;
            if !work
                .descendants()
                .filter_map(|n| n.text())
                .any(|t| t == celex || t.ends_with(&format!("/celex/{celex}")))
            {
                return Err("Cellar CELEX identity mismatch".into());
            }
            (json!({"celex":celex,"metadata":cellar_fields(work),"url":format!("https://eur-lex.europa.eu/legal-content/EN/TXT/?uri=CELEX:{celex}")}),"Cellar metadata notice, not the complete legal text or a legal opinion. Check document version and language.")
        }
    } else {
        let d: Value = serde_json::from_slice(bytes).map_err(|_| "Invalid public source JSON")?;
        match source {
            "vies" if p.url.ends_with("/check-status") => {
                if !d["vow"]["available"].is_boolean() || !d["countries"].is_array() {
                    return Err("Invalid VIES status response".into());
                }
                (d,"VIES service availability at the request time; this does not validate a VAT number.")
            }
            "vies" => {
                let valid = d
                    .get("isValid")
                    .or_else(|| d.get("valid"))
                    .and_then(Value::as_bool)
                    .ok_or("Missing VAT validity")?;
                let err = d["userError"].as_str().unwrap_or("VALID");
                if !["VALID", "INVALID"].contains(&err) || d["requestDate"].as_str().is_none() {
                    return Err("VIES unavailable or rejected the VAT request".into());
                }
                for k in ["countryCode", "vatNumber"] {
                    if d.get(k)
                        .is_some_and(|v| v.as_str() != p.params.get(k).map(String::as_str))
                    {
                        return Err("VAT response identity mismatch".into());
                    }
                }
                (json!({"country_code":p.params.get("countryCode"),"vat_number":p.params.get("vatNumber"),"valid":valid,"request_date":d["requestDate"],"name":d["name"],"address":d["address"],"upstream":d}),"VIES VAT registration validity at the request time; missing name/address is unknown. This is not a company register extract.")
            }
            "eurlex-cellar" => {
                let rows = d["results"]["bindings"]
                    .as_array()
                    .ok_or("Missing SPARQL bindings")?;
                if !d["head"]["vars"].is_array() || rows.len() > 100 {
                    return Err("Invalid or oversized SPARQL result".into());
                }
                (d,"Bounded SPARQL result only. LIMIT may truncate matching records; no complete coverage claim.")
            }
            "eu-funding-tenders" => {
                let rows = d["results"].as_array().ok_or("Missing Funding results")?;
                let total = d["totalResults"].as_u64().ok_or("Missing Funding total")?;
                let page = integer(&p.params, "page", 1, 1, 1000)?;
                let limit = integer(&p.params, "limit", 10, 1, 25)?;
                if d["pageNumber"] != page
                    || d["pageSize"] != limit
                    || rows.len() > limit as usize
                    || rows.iter().any(|r| {
                        r["metadata"]["DATASOURCE"]
                            .as_array()
                            .is_some_and(|a| a.contains(&json!("SEDIA_FAQ")))
                    })
                {
                    return Err(
                        "Funding returned an inconsistent page or ignored the requested filter"
                            .into(),
                    );
                }
                let items:Vec<Value>=rows.iter().map(|r| {
                    let mut metadata=serde_json::Map::new();
                    for key in ["identifier","callIdentifier","title","callTitle","status","startDate","deadlineDate","deadlineModel","programmePeriod","frameworkProgramme","type","DATASOURCE","budgetOverview"] {
                        if let Some(v)=r["metadata"].get(key) {metadata.insert(key.into(),v.clone());}
                    }
                    json!({"reference":r["reference"],"title":r["summary"],"url":r["url"],"language":r["language"],"metadata":metadata})
                }).collect();
                (json!({"items":items,"total":total,"page":page,"next_page":if page*limit<total&&page<1000{Some(page+1)}else{None}}),"Grant and tender discovery includes historic calls and multilingual records. No eligibility, application approval or open-status guarantee.")
            }
            "bodemloket" | "pdok-ruimtelijke-plannen" => {
                let rows = d["features"].as_array().ok_or("Missing WMS features")?;
                if d["type"] != "FeatureCollection"
                    || rows.len() > integer(&p.params, "FEATURE_COUNT", 10, 1, 25)? as usize
                {
                    return Err("Invalid WMS feature response".into());
                }
                let items: Vec<_> = rows
                    .iter()
                    .map(|r| json!({"id":r["id"],"properties":r["properties"]}))
                    .collect();
                (
                    json!({"items":items,"layer":p.params["LAYERS"],"bbox":p.params["BBOX"],"crs":"EPSG:28992"}),
                    if source == "bodemloket" {
                        "Map-layer information only; no complete soil investigation reports. No feature does not mean clean soil."
                    } else {
                        "Map-layer features at the requested pixel, not a complete or definitive statement of current planning rights. Follow original plan documents."
                    },
                )
            }
            _ => return Err("Unknown public source adapter".into()),
        }
    };
    Ok(
        json!({"data":data,"source_url":p.url,"notice":notice,"coverage":{"complete":false,"scope":"One bounded upstream response per purchased call"}}),
    )
}
fn desc<'a>(n: roxmltree::Node<'a, 'a>, name: &str) -> Option<&'a str> {
    n.descendants()
        .find(|x| x.has_tag_name(name))
        .and_then(|x| x.text())
}
fn leaf_fields(n: roxmltree::Node<'_, '_>) -> Value {
    let mut fields = serde_json::Map::new();
    for x in n
        .descendants()
        .filter(|x| x.is_element() && !x.children().any(|c| c.is_element()))
    {
        if let Some(t) = x.text().map(str::trim).filter(|s| !s.is_empty()) {
            let key = if x.tag_name().name() == "VALUE" {
                x.parent_element()
                    .map(|p| p.tag_name().name())
                    .unwrap_or("VALUE")
            } else {
                x.tag_name().name()
            };
            let a = fields
                .entry(key.to_string())
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .unwrap();
            if a.len() < 100 && !a.contains(&json!(t)) {
                a.push(json!(t));
            }
        }
    }
    Value::Object(fields)
}

fn cellar_fields(work: roxmltree::Node<'_, '_>) -> Value {
    let mut fields = serde_json::Map::new();
    for node in work.children().filter(|n| n.is_element()) {
        let key = node.tag_name().name();
        if [
            "RESOURCE_LEGAL_ID_CELEX",
            "WORK_DATE_DOCUMENT",
            "RESOURCE_LEGAL_DATE_SIGNATURE",
            "RESOURCE_LEGAL_IN-FORCE",
            "WORK_ID_DOCUMENT",
            "RESOURCE_LEGAL_YEAR",
            "URI",
            "SAMEAS",
            "WORK_HAS_EXPRESSION",
        ]
        .contains(&key)
        {
            let list = fields
                .entry(key.to_string())
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .unwrap();
            list.push(leaf_fields(node));
        }
    }
    Value::Object(fields)
}

pub async fn follow_metadata_redirect(
    p: &Prepared,
    http: &reqwest::Client,
    mut response: reqwest::Response,
) -> Result<reqwest::Response, String> {
    if p.source != "eu:eurlex-cellar" || !p.params.contains_key("celex") {
        return Ok(response);
    }
    for _ in 0..2 {
        if !response.status().is_redirection() {
            return Ok(response);
        }
        let loc = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .ok_or("Cellar redirect missing Location")?;
        let mut url = Url::parse(loc).map_err(|_| "Invalid Cellar redirect")?;
        if !["http", "https"].contains(&url.scheme())
            || url.host_str() != Some("publications.europa.eu")
            || !url.path().starts_with("/resource/cellar/")
            || url.port().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err("Unexpected Cellar metadata redirect".into());
        }
        url.set_scheme("https")
            .map_err(|_| "Invalid Cellar scheme")?;
        response = http
            .get(url)
            .header("Accept", "application/xml;notice=object")
            .send()
            .await
            .map_err(|e| e.to_string())?;
    }
    if response.status().is_redirection() {
        return Err("Too many Cellar metadata redirects".into());
    }
    Ok(response)
}
