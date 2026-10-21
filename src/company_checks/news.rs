//! Project only saved, dated search results. Never fetch article URLs here.
use super::{projection::evidence, Company, SavedSource};
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};

fn words(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn company_name(name: &str) -> String {
    let mut name = words(name);
    for suffix in [" b v", " n v", " bv", " nv", " holding"] {
        if let Some(base) = name.strip_suffix(suffix) {
            name = base.to_owned();
        }
    }
    name
}

pub(super) fn canonical_url(raw: &str) -> Option<String> {
    let mut url = url::Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host_str().is_none()
        || url.path() == "/"
    {
        return None;
    }
    url.set_fragment(None);
    let mut pairs: Vec<_> = url
        .query_pairs()
        .filter(|(k, _)| !k.starts_with("utm_") && !matches!(k.as_ref(), "fbclid" | "gclid"))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    pairs.sort();
    url.set_query(None);
    if !pairs.is_empty() {
        url.query_pairs_mut().extend_pairs(pairs);
    }
    Some(url.to_string())
}

pub(super) fn project(
    c: &Company,
    s: &SavedSource,
    complete: bool,
    signals: &mut Vec<Value>,
) -> bool {
    let Some(rows) = s.document["data"]["results"].as_array() else {
        return false;
    };
    let Ok(checked) = DateTime::parse_from_rfc3339(&s.retrieved_at) else {
        return false;
    };
    let checked = checked.with_timezone(&Utc);
    let name = company_name(&c.name);
    if name.chars().count() < 3 {
        return false;
    }
    let mut valid = complete;
    for (index, row) in rows.iter().enumerate() {
        let Some(title) = row["title"].as_str().filter(|s| !s.trim().is_empty()) else {
            valid = false;
            continue;
        };
        let Some(url) = row["url"].as_str().and_then(canonical_url) else {
            valid = false;
            continue;
        };
        let date = row["publishedDate"]
            .as_str()
            .or_else(|| row["published_date"].as_str());
        let Some(date) = date.and_then(|d| {
            DateTime::parse_from_rfc3339(d)
                .or_else(|_| DateTime::parse_from_rfc2822(d))
                .ok()
        }) else {
            valid = false;
            continue;
        };
        if date > checked || date < checked - Duration::days(7) {
            continue;
        }
        let excerpt = row["summary"]
            .as_str()
            .or_else(|| row["content"].as_str())
            .or_else(|| row["text"].as_str())
            .unwrap_or("");
        let text = words(&format!("{title} {excerpt}"));
        if !format!(" {text} ").contains(&format!(" {name} ")) {
            continue;
        }
        let pointer = format!("/data/results/{index}");
        signals.push(
            json!({"kvk_number":c.kvk_number,"company_name":c.name,"kind":"news_mention",
            "identity":url,"source":"news","comparison_value":url,"current":title,"previous":null,
            "message":format!("Nieuw waargenomen nieuwsbericht: {title}"),"review_required":true,
            "identity_match":"name_mention_not_verified","url":url,"published_at":date.to_rfc3339(),
            "evidence":[evidence(s,&pointer)]}),
        );
    }
    valid
}
