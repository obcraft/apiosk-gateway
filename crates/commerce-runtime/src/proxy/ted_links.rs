//! Public TED notice search. Identifier hits preserve source roles without
//! interpreting a procurement notice as an award to the requested legal entity.
use super::Prepared;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use url::Url;
pub fn prepare(method: &str, original: &str) -> Result<Option<Prepared>, String> {
    let u = Url::parse(original).map_err(|_| "Invalid TED route")?;
    if method != "GET"
        || u.scheme() != "https"
        || u.host_str() != Some("api.ted.europa.eu")
        || u.path() != "/apiosk/organisation-notices"
        || u.port().is_some()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
    {
        return Err("Unsupported TED route".into());
    }
    let mut p = BTreeMap::new();
    for (k, v) in u.query_pairs() {
        if !["identifier", "page"].contains(&k.as_ref()) || p.contains_key(k.as_ref()) {
            return Err("Unsupported TED search input".into());
        }
        p.insert(k.into_owned(), v.into_owned());
    }
    let id = p.get("identifier").ok_or("Missing KVK number or RSIN")?;
    if ![8, 9].contains(&id.len()) || !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Use an eight-digit KVK number or nine-digit RSIN".into());
    }
    let page = p
        .get("page")
        .map(|s| s.parse::<u64>())
        .transpose()
        .map_err(|_| "Invalid TED page")?
        .unwrap_or(1);
    if !(1..=750).contains(&page) {
        return Err("TED page must be between 1 and 750".into());
    }
    let body = json!({"query":format!("buyer-identifier = \"{id}\" OR winner-identifier = \"{id}\""),"fields":["publication-number","publication-date","buyer-name","buyer-identifier","winner-name","winner-identifier","notice-title","notice-type","links"],"limit":20,"page":page,"scope":"ALL","paginationMode":"PAGE_NUMBER","onlyLatestVersions":true});
    p.insert("page".into(), page.to_string());
    Ok(Some(Prepared {
        url: "https://api.ted.europa.eu/v3/notices/search".into(),
        source: "ted-links".into(),
        params: p,
        json_body: Some(body),
    }))
}
pub fn normalize(p: &Prepared, data: &Value) -> Result<Value, String> {
    if data["timedOut"] == true {
        return Err(
            "TED search timed out; incomplete results are not a valid identifier search".into(),
        );
    }
    let notices = data["notices"].as_array().ok_or("Missing TED notices")?;
    let total = data["totalNoticeCount"]
        .as_u64()
        .ok_or("Missing TED result total")?;
    if notices.len() > 20 || total < (notices.len() as u64) {
        return Err("Inconsistent TED page".into());
    }
    let id = p.params.get("identifier").ok_or("Missing TED identity")?;
    let mut items = vec![];
    for n in notices {
        let contains = |k: &str| {
            n[k].as_array()
                .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(id)))
        };
        if !contains("buyer-identifier") && !contains("winner-identifier") {
            return Err(
                "TED returned a notice without the requested exact buyer/winner identifier".into(),
            );
        }
        let number = n["publication-number"]
            .as_str()
            .filter(|s| {
                s.len() <= 20
                    && s.contains('-')
                    && s.bytes().all(|b| b.is_ascii_digit() || b == b'-')
            })
            .ok_or("Missing TED publication number")?;
        let mut row = n.clone();
        row.as_object_mut()
            .ok_or("Invalid TED notice")?
            .remove("links");
        row["url"] = json!(format!("https://ted.europa.eu/en/notice/-/detail/{number}"));
        row["identifier_match"] = json!({"buyer":contains("buyer-identifier"),"winner":contains("winner-identifier"),"scheme_and_legal_entity_confirmed":false});
        items.push(row);
    }
    let page = p.params["page"]
        .parse::<u64>()
        .map_err(|_| "Invalid page")?;
    let more = page * 20 < total;
    Ok(
        json!({"items":items,"total":total,"page":page,"next_page":if more&&page<750{Some(page+1)}else{None},"requested_identifier":id,"coverage":{"complete":page==1&&!more,"scope":"Exact numeric identifier in TED buyer or winner fields; returned page only. Identifier scheme, country and legal-entity identity still need confirmation."},"notice":"Includes historic notices and corrections. Matching an identifier does not by itself prove a current award, contract revenue or subsidy receipt. Pagination is capped at 15,000 notices.","source_url":p.url}),
    )
}
