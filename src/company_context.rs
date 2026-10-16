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
            tokio::time::timeout(Duration::from_secs(12), fetch(subject)).await.unwrap_or(fallback)
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
    // A supplied homepage is independent of search availability and latency.
    let supplied = subject.identifiers.iter()
        .filter(|(key, _)| matches!(key.as_str(), "website" | "company.website" | "website_url" | "domain" | "company.domain" | "company.url"))
        .find_map(|(_, value)| homepage_url(value))
        .or_else(|| homepage_url(&subject.label));
    let (search, supplied_page) = tokio::join!(free_search(&subject.label), async {
        match supplied.as_ref() {
            Some(url) => read_homepage(url, &subject.label).await,
            None => None,
        }
    });
    let (mut pages, provider, search_link) = search;
    result["search_provider"] = json!(provider);
    result["search_url"] = json!(search_link);
    result["search_status"] = json!(if pages.is_empty() { "unavailable" } else { "completed" });
    let homepage = if supplied.is_some() { supplied_page } else {
        let candidate = pages.iter().filter_map(|page| homepage_url(page["url"].as_str()?))
            .find(|url| likely_company_host(url, &subject.label));
        match candidate {
            Some(url) => read_homepage(&url, &subject.label).await,
            None => None,
        }
    };
    if let Some(page) = homepage {
        pages.retain(|p| p["url"] != page["url"]);
        pages.insert(0, page);
        result["homepage_status"] = json!("completed");
    }
    result["pages"] = json!(pages);
    result
}

async fn read_homepage(homepage: &Url, company: &str) -> Option<Value> {
    let (url, html) = tokio::time::timeout(Duration::from_secs(4),
        crate::link_preview::Service::shared().public_html(homepage.as_str())).await.ok()?.ok()?;
    homepage_page(&url, &html, company)
}

async fn free_search(company: &str) -> (Vec<Value>, &'static str, String) {
    let service = crate::link_preview::Service::shared();
    let google = search_url(company);
    if let Ok(Ok((url, html))) = tokio::time::timeout(Duration::from_secs(3), service.public_html(google.as_str())).await {
        // A consent page, CAPTCHA or login is not a search result. Do not bypass it.
        if url.host_str() == Some("www.google.com") && url.path() == "/search" {
            let pages = search_results(&html, company);
            if !pages.is_empty() { return (pages, "google", google.to_string()); }
        }
    }
    // Google's JS/consent/challenge responses are not results. A public RSS
    // fallback stays free and is explicitly attributed to Bing, never Google.
    let mut bing = Url::parse("https://www.bing.com/search").unwrap();
    bing.query_pairs_mut().append_pair("q", &format!("{company} official website"));
    let mut feed = bing.clone();
    feed.query_pairs_mut().append_pair("format", "rss");
    if let Ok(Ok((url, xml))) = tokio::time::timeout(Duration::from_secs(3), service.public_feed(feed.as_str())).await {
        if url.host_str() == Some("www.bing.com") && url.path() == "/search" {
            let pages = feed_results(&xml, company);
            if !pages.is_empty() { return (pages, "bing", bing.to_string()); }
        }
    }
    (vec![], "google", google.to_string())
}

fn feed_results(bytes: &[u8], company: &str) -> Vec<Value> {
    let Ok(text) = std::str::from_utf8(bytes) else { return vec![]; };
    let Ok(document) = roxmltree::Document::parse(text) else { return vec![]; };
    let name = company_name(company);
    let mut seen = std::collections::BTreeSet::new();
    let mut pages: Vec<Value> = document.descendants().filter(|node| node.has_tag_name("item")).take(20).filter_map(|item| {
        let field = |tag| item.children().find(|n| n.has_tag_name(tag)).and_then(|n| n.text());
        let url = Url::parse(field("link")?).ok()?;
        let host = url.host_str()?;
        let title = compact(field("title")?, 240);
        if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() || url.password().is_some()
            || name.len() < 2 || !(normalized(&title).contains(&name) || normalized(host).contains(&name))
            || !seen.insert(url.to_string()) { return None; }
        let description = compact(field("description").unwrap_or(""), 600);
        Some(json!({"kind":"search_result","url":url.as_str(),"title":title,"description":description,"publisher":host,
            "retrieved_at":chrono::Utc::now().to_rfc3339()}))
    }).collect();
    // Prefer a descriptive company sentence to a homepage promotion when the
    // homepage is blocked. Keep its original publisher and wording visible.
    pages.sort_by_key(|page| {
        let description = page["description"].as_str().unwrap_or("");
        !(normalized(description).starts_with(&name)
            && [" is ", " provides ", " offers ", " sells ", " operates "].iter().any(|verb| description.to_lowercase().contains(verb)))
    });
    pages.truncate(5);
    pages
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
    let body = ["main", "article", "body"].iter()
        .find_map(|selector| html.select(&Selector::parse(selector).unwrap()).next())?;
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
    fn fallback_feed_keeps_attributed_company_results_and_rejects_unsafe_links() {
        let pages = feed_results(br#"<rss><channel>
          <item><title>Coolblue - electronics</title><link>https://www.coolblue.nl/</link><description>Computers &amp; appliances</description></item>
          <item><title>Coolblue duplicate</title><link>https://www.coolblue.nl/</link></item>
          <item><title>Other company</title><link>https://unrelated.org/</link></item>
          <item><title>Coolblue</title><link>javascript:alert(1)</link></item>
          <item><title>Coolblue overview</title><link>https://encyclopedia.org/Coolblue</link><description>Coolblue is an electronics retailer.</description></item>
        </channel></rss>"#, "Coolblue B.V.");
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0]["description"], "Coolblue is an electronics retailer.");
        assert_eq!(pages[1]["description"], "Computers & appliances");
        assert_eq!(pages[0]["kind"], "search_result");
        assert_eq!(pages[0]["publisher"], "encyclopedia.org");
        assert_eq!(pages[1]["publisher"], "www.coolblue.nl");
        assert!(feed_results(b"<html>challenge</html>", "Coolblue").is_empty());
    }

    #[tokio::test]
    #[ignore = "live public internet; makes no paid calls"]
    async fn live_free_company_context() {
        let company = Subject { id: "live".into(), kind: "company".into(), label: "Coolblue".into(), identifiers: Default::default() };
        let context = fetch(&company).await;
        assert_eq!(context["search_status"], "completed", "{context}");
        assert!(!context["pages"].as_array().unwrap().is_empty());
        println!("COOLBLUE_CONTEXT={context}");
        let company = Subject { label: "Apiosk".into(), identifiers: [("website".into(), "https://apiosk.com/".into())].into(), ..company };
        let context = fetch(&company).await;
        assert_eq!(context["homepage_status"], "completed", "{context}");
        assert_eq!(context["pages"][0]["kind"], "homepage");
        println!("APIOSK_CONTEXT={context}");
    }

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
        let page = homepage_page(&Url::parse("https://coolblue.nl/").unwrap(), b"<title>Coolblue</title><body><div>Cookie banner</div><main>Company details</main></body>", "Coolblue").unwrap();
        assert_eq!(page["text"], "Company details");
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
