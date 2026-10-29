use super::{bounded_body, client, Value};
use serde_json::json;

fn profile_url(raw: &str) -> Option<String> {
    let u = url::Url::parse(raw).ok()?;
    if u.scheme() != "https" || !matches!(u.host_str(),Some("crunchbase.com"|"www.crunchbase.com"))
        || u.port().is_some() || !u.username().is_empty() || u.password().is_some() || u.query().is_some() { return None; }
    let parts: Vec<_> = u.path().trim_end_matches('/').split('/').collect();
    if parts.len() != 3 || !matches!(parts[1], "organization"|"person"|"funding_round") || parts[2].is_empty()
        || !parts[2].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') { return None; }
    Some(format!("https://www.crunchbase.com/{}/{}",parts[1],parts[2]))
}
fn find_profile(v: &Value, depth: usize) -> Option<String> {
    if depth > 10 { return None; }
    if let Value::Object(o) = v {
        for key in ["url","web_path","source_url"] {
            if let Some(url) = o.get(key).and_then(Value::as_str).and_then(profile_url) { return Some(url); }
        }
        if let (Some(kind),Some(permalink)) = (o.get("entity_def_id").and_then(Value::as_str),o.get("permalink").and_then(Value::as_str)) {
            if let Some(url) = profile_url(&format!("https://www.crunchbase.com/{kind}/{permalink}")) { return Some(url); }
        }
        // Prefer the entity itself over related organizations or people.
        for key in ["identifier","properties","data","entities","items"] {
            if let Some(url) = o.get(key).and_then(|v| find_profile(v,depth+1)) { return Some(url); }
        }
        for child in o.values() { if let Some(url) = find_profile(child,depth+1) { return Some(url); } }
    } else if let Value::Array(a) = v {
        for child in a.iter().take(25) { if let Some(url) = find_profile(child,depth+1) { return Some(url); } }
    }
    None
}
fn page(body: &Value, url: &str) -> Value {
    let data = &body["data"];
    let content = data["content"].as_str().unwrap_or("");
    let title = data["title"].as_str().unwrap_or("");
    let lower = format!("{title}\n{content}\n{}",data["warning"].as_str().unwrap_or("")).to_lowercase();
    let blocked = ["just a moment", "captcha", "verify your session", "verify you are human", "access denied", "checking your browser", "enable javascript and cookies", "sign in to continue", "subscribe to continue"].iter().any(|s| lower.contains(s));
    if blocked { return json!({"source_url":url,"status":"unavailable","reason":"page_requires_verification_or_login"}); }
    if body["code"] != 200 || data["httpStatus"].as_u64().is_some_and(|s| !(200..300).contains(&s))
        || data["url"].as_str().and_then(profile_url).as_deref() != Some(url) || content.trim().len() < 100 {
        return json!({"source_url":url,"status":"unavailable","reason":"no_readable_public_content"});
    }
    let text: String = content.chars().take(16000).collect();
    json!({"source_url":url,"status":"available","title":title.chars().take(250).collect::<String>(),"content":text,
        "truncated":content.chars().count()>16000,"provenance":"public_crunchbase_page","authority":"supplementary_web_content"})
}
pub(in crate::proxy) async fn enrich(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut result: Value = serde_json::from_slice(bytes).ok()?;
    if !result.is_object() || result.get("page_enrichment").is_some() || result.get("error").is_some() { return None; }
    // Reference pages describe the schema, not company data.
    if result["tool"] == "cb_reference" { return None; }
    let url = find_profile(&result,0)?;
    let enrichment = async {
        let response = client(12).ok()?.get(format!("https://r.jina.ai/{url}"))
            .header("Accept","application/json").header("X-Timeout","10").send().await.ok()?;
        if !response.status().is_success() { return None; }
        let body: Value = serde_json::from_slice(&bounded_body(response,512*1024).await.ok()?).ok()?;
        Some(page(&body,&url))
    }.await.unwrap_or_else(|| json!({"source_url":url,"status":"unavailable","reason":"reader_unavailable"}));
    // One bounded, free public-page attempt per action; no retries, expansion,
    // additional source, auth forwarding or extra buyer charge.
    result["page_enrichment"] = enrichment;
    serde_json::to_vec(&result).ok()
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn public_profiles_only() {
        for u in ["https://www.crunchbase.com/organization/starbucks?token=secret","https://crunchbase.com.evil.test/person/name","https://name:secret@crunchbase.com/person/name","https://www.crunchbase.com:8443/person/name","https://www.crunchbase.com/organization/%2e%2e","https://www.crunchbase.com/login","http://crunchbase.com/person/name"] { assert!(profile_url(u).is_none(),"{u}"); }
        assert_eq!(profile_url("https://crunchbase.com/organization/starbucks#people").unwrap(),"https://www.crunchbase.com/organization/starbucks");
    }
    #[test] fn verification_pages_are_never_company_data() {
        let url="https://www.crunchbase.com/organization/starbucks";
        let reply=json!({"code":200,"data":{"url":url,"httpStatus":200,"title":"Just a moment...","content":"We must verify your session before you can proceed","warning":"This page maybe requiring CAPTCHA"}});
        let value=page(&reply,url); assert_eq!(value["status"],"unavailable"); assert!(value.get("content").is_none());
        let reply=json!({"code":200,"data":{"url":url,"httpStatus":200,"title":"Starbucks","content":"Company and people: ".repeat(1000)}});
        let value=page(&reply,url); assert_eq!(value["status"],"available"); assert_eq!(value["truncated"],true);
    }
    #[test] fn preserve_entity_identity_when_cards_contain_other_companies() {
        let value=json!({"data":{"properties":{"identifier":{"entity_def_id":"organization","permalink":"starbucks"}},"cards":{"founders":[{"url":"https://www.crunchbase.com/person/example"}]}}});
        assert_eq!(find_profile(&value,0).unwrap(),"https://www.crunchbase.com/organization/starbucks");
    }
}
