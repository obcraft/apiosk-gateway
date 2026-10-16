//! Free, best-effort public web context. No paid APIs, models, executor,
//! credentials or billing calls. Web excerpts never become registry evidence.
use crate::capability_intake::{ParsedRequest, Subject};
use futures_util::future::join_all;
use scraper::{Html, Selector};
use serde_json::{json, Value};
use std::{sync::OnceLock, time::Duration};
use url::Url;

fn cache() -> &'static moka::future::Cache<String, Value> {
    static CACHE: OnceLock<moka::future::Cache<String, Value>> = OnceLock::new();
    CACHE.get_or_init(|| moka::future::Cache::builder().max_capacity(256)
        .time_to_live(Duration::from_secs(900)).build())
}

pub(crate) async fn collect(parsed: &ParsedRequest) -> Vec<Value> {
    let subjects = companies(parsed);
    join_all(subjects.into_iter().map(|subject| async move {
        let key = crate::state::digest(&(subject.label.to_lowercase(), &subject.identifiers));
        cache().get_with(key, async {
            let fallback = empty(&subject.label);
            tokio::time::timeout(Duration::from_secs(9), fetch(subject)).await.unwrap_or(fallback)
        }).await
    })).await
}

fn companies(parsed: &ParsedRequest) -> Vec<&Subject> {
    let mut names = std::collections::BTreeSet::new();
    parsed.subjects.iter().filter(|s| {
        matches!(s.kind.as_str(), "company" | "organization" | "organisation")
            && !s.label.trim().is_empty() && s.label.chars().count() <= 160
            && names.insert(s.label.trim().to_lowercase())
    }).take(4).collect()
}

fn search_url(company: &str) -> Url {
    let mut url = Url::parse("https://www.google.com/search").unwrap();
    url.query_pairs_mut().append_pair("q", &format!("{company} official website")).append_pair("num", "5");
    url
}

fn empty(company: &str) -> Value {
    json!({"company":company,"search_url":search_url(company).as_str(),"search_status":"unavailable",
        "homepage_status":"unavailable","pages":[],"retrieved_at":chrono::Utc::now().to_rfc3339()})
}

async fn fetch(subject: &Subject) -> Value {
    let mut result = empty(&subject.label);
    let service = crate::link_preview::Service::shared();
    let mut pages = Vec::new();
    if let Ok(Ok((url, html))) = tokio::time::timeout(Duration::from_secs(4), service.public_html(search_url(&subject.label).as_str())).await {
        // A consent page, CAPTCHA or login is not a search result. Do not bypass it.
        if url.host_str() == Some("www.google.com") && url.path() == "/search" {
            pages = search_results(&html, &subject.label);
            result["search_status"] = json!(if pages.is_empty() { "unavailable" } else { "completed" });
        }
    }
    let homepage = subject.identifiers.iter()
        .filter(|(key, _)| matches!(key.as_str(), "website" | "company.website" | "website_url" | "domain" | "company.domain"))
        .find_map(|(_, value)| homepage_url(value))
        .or_else(|| pages.iter().filter_map(|page| homepage_url(page["url"].as_str()?))
            .find(|url| likely_company_host(url, &subject.label)));
    if let Some(homepage) = homepage {
        // Give a known homepage its own bounded attempt even if Google is blocked.
        if let Ok(Ok((url, html))) = tokio::time::timeout(Duration::from_secs(4), service.public_html(homepage.as_str())).await {
            if let Some(page) = homepage_page(&url, &html, &subject.label) {
                pages.retain(|p| p["url"].as_str() != Some(url.as_str()));
                pages.insert(0, page);
                result["homepage_status"] = json!("completed");
            }
        }
    }
    result["pages"] = json!(pages);
    result
}

fn compact(text: impl Into<String>, max: usize) -> String {
    text.into().split_whitespace().collect::<Vec<_>>().join(" ").chars().take(max).collect()
}
fn normalized(text: &str) -> String { text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect() }
fn company_name(company: &str) -> String {
    normalized(&company.split_whitespace().filter(|word| !["bv", "nv", "ltd", "inc", "llc", "limited", "plc"].contains(&normalized(word).as_str())).collect::<Vec<_>>().join(""))
}

fn homepage_url(raw: &str) -> Option<Url> {
    let raw = raw.trim();
    let mut url = Url::parse(if raw.contains("://") { raw } else { return homepage_url(&format!("https://{raw}")) }).ok()?;
    if !matches!(url.scheme(), "https" | "http") || !url.username().is_empty() || url.password().is_some()
        || !url.host_str()?.contains('.') || url.port().is_some() { return None; }
    url.set_path("/"); url.set_query(None); url.set_fragment(None);
    Some(url)
}
fn likely_company_host(url: &Url, company: &str) -> bool {
    let host = url.host_str().unwrap_or("").trim_start_matches("www.");
    let name = company_name(company);
    // A matching domain is a candidate, not verified legal identity.
    name.len() >= 3 && host.split('.').next().is_some_and(|part| normalized(part) == name)
}

fn search_results(bytes: &[u8], company: &str) -> Vec<Value> {
    let html = Html::parse_document(&String::from_utf8_lossy(bytes));
    let links = Selector::parse("a[href]").unwrap();
    let heading = Selector::parse("h3").unwrap();
    let snippet = Selector::parse(".VwiC3b, .BNeawe.s3v9rd, .aCOpRe, [data-sncf]").unwrap();
    let mut seen = std::collections::BTreeSet::new();
    html.select(&links).filter_map(|link| {
        let title = compact(link.select(&heading).next()?.text().collect::<Vec<_>>().join(" "), 240);
        let href = link.value().attr("href")?;
        let mut url = Url::parse(href).or_else(|_| Url::parse("https://www.google.com")?.join(href)).ok()?;
        if url.host_str() == Some("www.google.com") && url.path() == "/url" {
            let target = url.query_pairs().find(|(key, _)| key == "q" || key == "url")?.1.into_owned();
            url = Url::parse(&target).ok()?;
        }
        if !matches!(url.scheme(), "https" | "http") || !url.username().is_empty() || url.password().is_some() { return None; }
        let host = url.host_str()?;
        if host == "google.com" || host.ends_with(".google.com") || !seen.insert(url.to_string()) { return None; }
        let name = company_name(company);
        if name.len() < 2 || !(normalized(&title).contains(&name) || normalized(host).contains(&name)) { return None; }
        let description = link.ancestors().skip(1).take(4).filter_map(scraper::ElementRef::wrap)
            .find_map(|parent| parent.select(&snippet).next().map(|node| compact(node.text().collect::<Vec<_>>().join(" "), 600)))
            .unwrap_or_else(|| "Google search result; open the source to verify its contents.".into());
        Some(json!({"kind":"search_result","url":url.as_str(),"title":title,"description":description,"publisher":host}))
    }).take(5).collect()
}

fn homepage_page(url: &Url, bytes: &[u8], company: &str) -> Option<Value> {
    let html = Html::parse_document(&String::from_utf8_lossy(bytes));
    let title = compact(html.select(&Selector::parse("title").unwrap()).next()?.text().collect::<Vec<_>>().join(" "), 240);
    let lower = title.to_lowercase();
    if ["just a moment", "access denied", "captcha", "verify you are human", "attention required"].iter().any(|s| lower.contains(s)) { return None; }
    let description = html.select(&Selector::parse("meta[name='description'], meta[property='og:description']").unwrap())
        .find_map(|node| node.value().attr("content")).map(|text| compact(text, 600)).unwrap_or_default();
    let body = html.select(&Selector::parse("main, article, body").unwrap()).next()?;
    let text = compact(body.descendants().filter_map(|node| {
        let value = node.value().as_text()?;
        if node.ancestors().filter_map(scraper::ElementRef::wrap).any(|parent| {
            matches!(parent.value().name(), "script" | "style" | "nav" | "footer" | "header" | "noscript" | "template")
                || parent.value().attr("aria-hidden") == Some("true") || parent.value().attr("hidden").is_some()
        }) { return None; }
        Some(value.text.to_string())
    }).collect::<Vec<_>>().join(" "), 2400);
    if description.is_empty() && text.is_empty() { return None; }
    Some(json!({"kind":"homepage","company":company,"url":url.as_str(),"title":title,"publisher":url.host_str(),
        "description":description,"text":text,"retrieved_at":chrono::Utc::now().to_rfc3339()}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn search_reads_only_named_company_results_and_unwraps_google_links() {
        let pages = search_results(br#"<a href='/url?q=https%3A%2F%2Fwww.coolblue.nl%2F'><h3>Coolblue - official site</h3></a><a href='https://unrelated.org'><h3>Other business</h3></a><a href='javascript:alert(1)'><h3>Coolblue</h3></a>"#, "Coolblue BV");
        assert_eq!(pages.len(), 1); assert_eq!(pages[0]["url"], "https://www.coolblue.nl/");
        assert!(likely_company_host(&Url::parse(pages[0]["url"].as_str().unwrap()).unwrap(), "Coolblue B.V."));
        assert!(!likely_company_host(&Url::parse("https://companyinfo.nl").unwrap(), "Coolblue"));
        assert!(search_results(b"<a href='https://other.nl'><h3>Other Inc.</h3></a>", "Apple Inc.").is_empty());
    }
    #[test]
    fn homepage_is_bounded_plain_text_without_scripts_or_hidden_content() {
        let page = homepage_page(&Url::parse("https://www.coolblue.nl/").unwrap(), br#"<title>Coolblue</title><meta name='description' content='Electronics'><body><nav>Skip navigation</nav><main>Company context <script>steal()</script><span hidden>secret</span></main></body>"#, "Coolblue").unwrap();
        assert_eq!(page["description"], "Electronics"); assert_eq!(page["text"], "Company context");
        assert!(homepage_page(&Url::parse("https://coolblue.nl/").unwrap(), b"<title>Just a moment</title><body>Verify you are human</body>", "Coolblue").is_none());
    }
    #[test]
    fn non_company_requests_never_search() {
        let parsed: ParsedRequest = serde_json::from_value(json!({"language":"en","subjects":[{"id":"1","type":"person","label":"Private person","identifiers":{}}],"capabilities":[],"deliverable":{"format":"chat","operations":[]},"meta":{"model":"fixture","parsed_at":"now","text_sha256":"hash"}})).unwrap();
        assert!(companies(&parsed).is_empty());
    }

    #[tokio::test]
    async fn cached_company_context_is_reused_without_a_second_request() {
        let parsed: ParsedRequest = serde_json::from_value(json!({"language":"en","subjects":[{"id":"1","type":"company","label":"Cached company fixture","identifiers":{}}],"capabilities":[],"deliverable":{"format":"chat","operations":[]},"meta":{"model":"fixture","parsed_at":"now","text_sha256":"hash"}})).unwrap();
        let subject = &parsed.subjects[0];
        let key = crate::state::digest(&(subject.label.to_lowercase(), &subject.identifiers));
        let saved = json!({"company":subject.label,"search_status":"completed","pages":[{"title":"Previously saved result"}]});
        cache().insert(key, saved.clone()).await;
        assert_eq!(collect(&parsed).await, vec![saved]);
    }
}
