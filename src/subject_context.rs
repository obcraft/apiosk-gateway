//! Best-effort public web context, free to the buyer. An optional SerpApi key
//! pays search costs from the operator's account, never the buyer ledger.
//! No models or executor calls. Named subjects share the legacy company_context
//! wire format. Web excerpts never become registry or sanctions evidence.
use crate::capability_intake::{ParsedRequest, Subject};
use futures_util::{future::{join_all, BoxFuture}, stream::FuturesUnordered, FutureExt, StreamExt};
use scraper::{Html, Selector};
use serde_json::{json, Value};
use std::{sync::OnceLock, time::Duration};
use url::Url;
mod serpapi;

// Search, encyclopedia and supplied homepages start together. Every result is
// published on arrival; the final snapshot still supports non-streaming clients.
const SEARCH_BUDGET: Duration = Duration::from_secs(2);
const PAGE_BUDGET: Duration = Duration::from_millis(1500);

fn cache() -> &'static moka::future::Cache<String, Value> {
    static CACHE: OnceLock<moka::future::Cache<String, Value>> = OnceLock::new();
    CACHE.get_or_init(|| moka::future::Cache::builder().max_capacity(256)
        .time_to_live(Duration::from_secs(900)).build())
}

pub(crate) async fn collect(parsed: &ParsedRequest) -> Vec<Value> {
    collect_with_updates(parsed, |_, _| {}).await
}

/// The Ask page carries the original question separately from its signed
/// catalog parse. Public page excerpts inform context, never paid checks.
pub(crate) async fn collect_for_question(parsed: &ParsedRequest, question: &str, update: impl Fn(usize, Value) + Send + Sync) -> Vec<Value> {
    join_all(named_subjects(parsed).into_iter().enumerate().map(|(index, subject)| {
        let update = &update;
        async move {
            let key = question_cache_key(subject, question);
            // The existing interpretation provides the requested concepts in
            // addition to the user's wording (for example Dutch questions
            // about ownership on English registry pages). No extra model call.
            let concepts = parsed.capabilities.iter().filter(|cap| cap.subject_id.as_deref() == Some(subject.id.as_str()))
                .map(|cap| format!("{} {}", cap.name, cap.outputs.join(" "))).collect::<Vec<_>>().join("\n");
            let selection_question = format!("{question}\n{concepts}");
            let value = cached_context(key, async {
                let mut value = fetch_with_updates(subject, &|v| update(index, v)).await;
                value["content_status"] = json!("reading");
                update(index, value.clone());
                let candidates = context_candidates(&value);
                let (sources, failures) = crate::web_research::read_context(candidates, &selection_question).await;
                attach_page_context(&mut value, &sources, &failures);
                value
            }).await;
            update(index, value.clone());
            value
        }
    })).await
}

fn question_cache_key(subject: &Subject, question: &str) -> String {
    // The same company can be researched for ownership, revenue or products.
    // Reusing another question's selected passages would change the evidence.
    crate::state::digest(&(context_cache_key(subject), "question-passages-v1", question.trim()))
}

fn context_candidates(value: &Value) -> Vec<crate::web_research::Candidate> {
    let mut seen = std::collections::HashSet::new();
    value["pages"].as_array().into_iter().flatten().enumerate().filter_map(|(rank, page)| {
        let url = page["url"].as_str()?;
        if !seen.insert(url) { return None; }
        Some(crate::web_research::Candidate { title: page["title"].as_str().unwrap_or(url).into(), url: url.into(), rank: rank + 1 })
    }).collect()
}

fn attach_page_context(value: &mut Value, sources: &[crate::web_research::Source], failures: &[Value]) {
    if let Some(pages) = value["pages"].as_array_mut() {
        for page in pages {
            if let Some(source) = sources.iter().find(|source| page["url"].as_str() == Some(&source.url)) {
                page["relevant_text"] = json!(source.text);
                page["content_status"] = json!("read");
                page["final_url"] = json!(source.final_url);
                page["retrieved_at"] = json!(source.retrieved_at);
                page["text_sha256"] = json!(source.text_sha256);
                page["trimmed"] = json!(source.trimmed);
            } else {
                page["content_status"] = json!("unavailable");
                page["content_reason"] = failures.iter().find(|failure| failure["url"] == page["url"])
                    .map(|failure| failure["reason"].clone()).unwrap_or(json!("not_read_within_budget"));
            }
        }
    }
    value["content_status"] = json!("completed");
    value["content_sources_read"] = json!(sources.len());
}

pub(crate) async fn collect_with_updates(parsed: &ParsedRequest, update: impl Fn(usize, Value) + Send + Sync) -> Vec<Value> {
    let subjects = named_subjects(parsed);
    join_all(subjects.into_iter().enumerate().map(|(index, subject)| {
      let update = &update;
      async move {
        let key = context_cache_key(subject);
        let value = cached_context(key, fetch_with_updates(subject, &|value| update(index, value))).await;
        update(index, value.clone());
        value
      }
    })).await
}

fn context_cache_key(subject: &Subject) -> String {
    crate::state::digest(&(serpapi::cache_partition(), &subject.kind, subject.label.to_lowercase(), &subject.identifiers))
}

async fn cached_context(key: String, fetch: impl std::future::Future<Output = Value>) -> Value {
    // Coalesce simultaneous lookups, but let the next question retry a failure.
    cache().try_get_with(key, async {
        let value = fetch.await;
        if value["pages"].as_array().is_some_and(|pages| !pages.is_empty())
            && (value["content_status"] != "completed" || value["content_sources_read"].as_u64().unwrap_or(0) > 0) {
            Ok(value)
        } else {
            Err(value)
        }
    }).await.unwrap_or_else(|value| (*value).clone())
}

fn named_subjects(parsed: &ParsedRequest) -> Vec<&Subject> {
    let mut names = std::collections::BTreeSet::new();
    parsed.subjects.iter().filter(|s| {
        // Explicit website reading has its own quoted retrieval. Do not add
        // an unrelated search/homepage preview or duplicate its network work.
        if parsed.capabilities.iter().any(|cap| cap.subject_id.as_deref() == Some(s.id.as_str())
            && crate::website_read::requested(cap, parsed)) { return false; }
        // The parser's type vocabulary is open: a landmark, building, event,
        // artwork or other named subject must not depend on an entity allowlist.
        // Bare financial identifiers and credentials are not public-web subjects.
        !matches!(s.kind.to_ascii_lowercase().as_str(), "iban" | "bank_account" | "payment_card" | "credential")
            && !s.label.trim().is_empty() && s.label.chars().count() <= 160
            && names.insert(s.label.trim().to_lowercase())
    }).take(4).collect()
}

fn search_query(subject: &Subject) -> String {
    let qualifier = subject.identifiers.iter()
        .find(|(key, _)| matches!(key.as_str(), "country" | "country_code" | "company.country" | "city"))
        .map(|(_, value)| compact(value.clone(), 80)).unwrap_or_default();
    let suffix = if matches!(subject.kind.as_str(), "company" | "organization" | "organisation") { " official website" } else { "" };
    format!("{} {}{}", subject.label, qualifier, suffix).split_whitespace().collect::<Vec<_>>().join(" ")
}

fn search_url(query: &str) -> Url {
    let mut url = Url::parse("https://www.google.com/search").unwrap();
    url.query_pairs_mut().append_pair("q", query).append_pair("num", "5");
    url
}

fn empty(label: &str) -> Value {
    json!({"company":label,"search_url":search_url(label).as_str(),"search_status":"unavailable",
        "homepage_status":"unavailable","pages":[],"retrieved_at":chrono::Utc::now().to_rfc3339()})
}

#[cfg(test)]
async fn fetch(subject: &Subject) -> Value { fetch_with_updates(subject, &|_| {}).await }

async fn fetch_with_updates(subject: &Subject, update: &(dyn Fn(Value) + Send + Sync)) -> Value {
    let mut result = empty(&subject.label);
    result["subject_type"] = json!(subject.kind);
    let query = search_query(subject);
    result["search_url"] = json!(search_url(&query).as_str());
    result["search_status"] = json!("loading");
    result["homepage_status"] = json!("loading");
    update(result.clone());
    // A supplied homepage is independent of search availability and latency.
    let supplied = subject.identifiers.iter()
        .filter(|(key, _)| matches!(key.as_str(), "website" | "company.website" | "website_url" | "domain" | "company.domain" | "company.url"))
        .find_map(|(_, value)| homepage_url(value))
        .or_else(|| homepage_url(&subject.label));
    enum Arrival { Search(Found), Overview(Option<Found>), Homepage(Option<Value>) }
    let topic = is_topic_subject(subject);
    let mut reads: FuturesUnordered<BoxFuture<'_, Arrival>> = FuturesUnordered::new();
    reads.push(async { Arrival::Search(free_search(&subject.label, &query, topic).await) }.boxed());
    // A fast promotional snippet must not cancel the descriptive lookup.
    reads.push(async { Arrival::Overview(encyclopedia_search(&subject.label, topic).await) }.boxed());
    let mut homepage_started = supplied.is_some();
    if let Some(url) = supplied {
        reads.push(async move { Arrival::Homepage(read_homepage(&url, &subject.label).await) }.boxed());
    }
    let mut pages: Vec<Value> = vec![];
    while let Some(arrival) = reads.next().await {
        match arrival {
            Arrival::Search((found, provider, link)) | Arrival::Overview(Some((found, provider, link))) => {
                if result["search_provider"].is_null() && !found.is_empty() {
                    result["search_provider"] = json!(provider);
                    result["search_url"] = json!(link);
                }
                if !homepage_started {
                    let candidate = found.iter().filter_map(|page| homepage_url(page["url"].as_str()?))
                        .find(|url| likely_subject_host(url, &subject.label, matches!(subject.kind.as_str(), "company" | "organization" | "organisation")));
                    if let Some(url) = candidate {
                        homepage_started = true;
                        reads.push(async move { Arrival::Homepage(read_homepage(&url, &subject.label).await) }.boxed());
                    }
                }
                for mut page in found {
                    page["search_provider"] = json!(provider);
                    // Preserve arrival order and identity as a description improves.
                    if pages.iter().any(|saved| saved["url"] == page["url"]) { continue; }
                    // Five search hits, three encyclopedia hits and one homepage.
                    // Never remove an already displayed hit to make room.
                    if pages.len() >= 9 { continue; }
                    pages.push(page);
                    result["pages"] = json!(pages);
                    update(result.clone());
                }
            }
            Arrival::Homepage(page) => {
                result["homepage_status"] = json!(if page.is_some() { "completed" } else { "unavailable" });
                if let Some(page) = page {
                    if let Some(existing) = pages.iter_mut().find(|saved| saved["url"] == page["url"]) { *existing = page; }
                    else if pages.len() < 9 { pages.push(page); }
                    result["pages"] = json!(pages);
                }
                update(result.clone());
            }
            Arrival::Overview(None) => {},
        }
    }
    result["search_status"] = json!(if pages.is_empty() { "unavailable" } else { "completed" });
    if !homepage_started { result["homepage_status"] = json!("unavailable"); }
    result["pages"] = json!(pages);
    result
}

async fn read_homepage(homepage: &Url, label: &str) -> Option<Value> {
    let (url, html) = tokio::time::timeout(PAGE_BUDGET,
        crate::link_preview::Service::shared().public_html(homepage.as_str())).await.ok()?.ok()?;
    homepage_page(&url, &html, label)
}

async fn free_search(label: &str, query: &str, topic: bool) -> (Vec<Value>, &'static str, String) {
    if serpapi::enabled() {
        match serpapi::search(label, query, topic).await {
            Ok(pages) if !pages.is_empty() => return (pages, "serpapi", search_url(query).to_string()),
            Ok(_) => tracing::info!(provider = "serpapi", reason = "no_usable_results", "subject web search returned no usable pages"),
            // Never log the request URL or upstream error: api_key is a query
            // parameter. Failure falls back to the existing public searches.
            Err(reason) => tracing::info!(provider = "serpapi", reason, "subject web search unavailable; trying public search"),
        }
    }
    let service = crate::link_preview::Service::shared();
    let google = search_url(query);
    let google_read = async {
        match tokio::time::timeout(SEARCH_BUDGET, service.public_html(google.as_str())).await {
          Ok(Ok((url, html))) => {
            // A consent page, CAPTCHA or login is not a search result. Do not bypass it.
            if url.host_str() == Some("www.google.com") && url.path() == "/search" {
                let pages = search_results(&html, label, topic);
                if !pages.is_empty() { return Some((pages, "google", google.to_string())); }
            }
            tracing::info!(provider = "google", reason = "no_usable_results", "subject web search returned no usable pages");
          }
          Ok(Err(error)) => tracing::info!(provider = "google", reason = "fetch_failed", error = %error, "subject web search failed"),
          Err(_) => tracing::info!(provider = "google", reason = "timeout", "subject web search exceeded its budget"),
        }
        None
    };
    // Race public searches: a blocked Google page must not consume the whole
    // intro budget. The first usable response wins; its provider stays explicit.
    let mut bing = Url::parse("https://www.bing.com/search").unwrap();
    bing.query_pairs_mut().append_pair("q", query);
    let mut feed = bing.clone();
    feed.query_pairs_mut().append_pair("format", "rss");
    let bing_read = async {
        match tokio::time::timeout(SEARCH_BUDGET, service.public_feed(feed.as_str())).await {
          Ok(Ok((url, xml))) => {
            if url.host_str() == Some("www.bing.com") && url.path() == "/search" {
                let pages = feed_results(&xml, label, topic);
                if !pages.is_empty() { return Some((pages, "bing", bing.to_string())); }
            }
            tracing::info!(provider = "bing", reason = "no_usable_results", "subject web search returned no usable pages");
          }
          Ok(Err(error)) => tracing::info!(provider = "bing", reason = "fetch_failed", error = %error, "subject web search failed"),
          Err(_) => tracing::info!(provider = "bing", reason = "timeout", "subject web search exceeded its budget"),
        }
        None
    };
    let mut primary = vec![google_read.boxed(), bing_read.boxed()];
    // Google answers EU data-centre requests with a consent page and Bing RSS
    // with unrelated results. A topic is news-shaped: its public news feed is a
    // plain RSS read that needs neither consent nor scripts.
    if topic { primary.push(news_search(label, query).boxed()); }
    if let Some(found) = first_usable(primary).await { return found; }
    // Try independent, non-JS public sources before giving up.
    html_search(label, topic).await
        .unwrap_or_else(|| (vec![], "google", google.to_string()))
}

type Found = (Vec<Value>, &'static str, String);

/// The first usable response wins; the others are dropped and cancelled.
async fn first_usable(reads: Vec<BoxFuture<'_, Option<Found>>>) -> Option<Found> {
    let mut reads: FuturesUnordered<_> = reads.into_iter().collect();
    while let Some(found) = reads.next().await {
        if found.is_some() { return found; }
    }
    None
}

async fn news_search(label: &str, query: &str) -> Option<Found> {
    let mut feed = Url::parse("https://news.google.com/rss/search").unwrap();
    feed.query_pairs_mut().append_pair("q", query).append_pair("hl", "en-US").append_pair("gl", "US").append_pair("ceid", "US:en");
    let (final_url, bytes) = match tokio::time::timeout(SEARCH_BUDGET,
        crate::link_preview::Service::shared().public_feed(feed.as_str())).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => { tracing::info!(provider = "google_news", reason = "fetch_failed", error = %error, "subject web search failed"); return None; }
        Err(_) => { tracing::info!(provider = "google_news", reason = "timeout", "subject web search exceeded its budget"); return None; }
    };
    // A consent page or any other redirect is not a news feed. Do not bypass it.
    if final_url.host_str() != Some("news.google.com") || final_url.path() != "/rss/search" { return None; }
    let pages = news_results(&bytes, label);
    if pages.is_empty() { tracing::info!(provider = "google_news", reason = "no_usable_results", "subject web search returned no usable pages"); return None; }
    // The page accepts Google search links; this one opens the same query's news tab.
    let mut link = search_url(query);
    link.query_pairs_mut().append_pair("tbm", "nws");
    Some((pages, "google_news", link.to_string()))
}

/// Headlines with their original publisher. Google News appends " - Publisher"
/// to every title; the publisher stays visible as attribution instead.
fn news_results(bytes: &[u8], label: &str) -> Vec<Value> {
    let Ok(text) = std::str::from_utf8(bytes) else { return vec![]; };
    let Ok(document) = roxmltree::Document::parse(text) else { return vec![]; };
    let mut seen = std::collections::BTreeSet::new();
    document.descendants().filter(|node| node.has_tag_name("item")).take(40).filter_map(|item| {
        let child = |tag| item.children().find(|n| n.has_tag_name(tag));
        let url = Url::parse(child("link")?.text()?.trim()).ok()?;
        let source = child("source");
        let publisher = compact(source.and_then(|n| n.text()).unwrap_or(""), 200);
        let site = source.and_then(|n| n.attribute("url")).and_then(|raw| Url::parse(raw).ok())
            .and_then(|site| site.host_str().map(str::to_owned));
        let mut title = compact(child("title")?.text()?, 240);
        if let Some(headline) = title.strip_suffix(&format!(" - {publisher}")).filter(|_| !publisher.is_empty()) { title = headline.to_owned(); }
        let host = url.host_str()?;
        if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() || title.is_empty()
            || !matches_result(label, &title, site.as_deref().unwrap_or(host), &publisher, true)
            || !seen.insert(title.to_lowercase()) { return None; }
        Some(json!({"kind":"search_result","url":url.as_str(),"title":title,"description":title,
            "publisher":if publisher.is_empty() { site.as_deref().unwrap_or(host) } else { publisher.as_str() },
            "retrieved_at":chrono::Utc::now().to_rfc3339()}))
    }).take(5).collect()
}

async fn html_search(label: &str, topic: bool) -> Option<(Vec<Value>, &'static str, String)> {
    let mut url = Url::parse("https://html.duckduckgo.com/html/").unwrap();
    url.query_pairs_mut().append_pair("q", label);
    let (final_url, bytes) = match tokio::time::timeout(SEARCH_BUDGET,
        crate::link_preview::Service::shared().public_html(url.as_str())).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => { tracing::info!(provider = "duckduckgo", reason = "fetch_failed", error = %error, "subject web search failed"); return None; }
        Err(_) => { tracing::info!(provider = "duckduckgo", reason = "timeout", "subject web search exceeded its budget"); return None; }
    };
    if final_url.host_str() != Some("html.duckduckgo.com") || final_url.path() != "/html/" { return None; }
    let pages = html_search_results(&bytes, label, topic);
    if pages.is_empty() { tracing::info!(provider = "duckduckgo", reason = "no_usable_results", "subject web search returned no usable pages"); return None; }
    let mut link = Url::parse("https://duckduckgo.com/").unwrap();
    link.query_pairs_mut().append_pair("q", label);
    Some((pages, "duckduckgo", link.to_string()))
}

fn html_search_results(bytes: &[u8], label: &str, topic: bool) -> Vec<Value> {
    let html = Html::parse_document(&String::from_utf8_lossy(bytes));
    let rows = Selector::parse(".result").unwrap();
    let anchors = Selector::parse("a.result__a").unwrap();
    let snippets = Selector::parse(".result__snippet").unwrap();
    let mut seen = std::collections::BTreeSet::new();
    html.select(&rows).filter_map(|row| {
        let anchor = row.select(&anchors).next()?;
        let title = compact(anchor.text().collect::<Vec<_>>().join(" "), 240);
        let description = row.select(&snippets).next().map(|el| compact(el.text().collect::<Vec<_>>().join(" "), 600)).unwrap_or_default();
        let href = anchor.value().attr("href")?;
        let mut url = Url::parse("https://duckduckgo.com/").unwrap().join(href).ok()?;
        if matches!(url.host_str(), Some("duckduckgo.com" | "html.duckduckgo.com")) {
            let target = url.query_pairs().find(|(key, _)| key == "uddg")?.1.into_owned();
            url = Url::parse(&target).ok()?;
        }
        let host = url.host_str()?;
        if !matches!(url.scheme(), "https" | "http") || !url.username().is_empty() || url.password().is_some()
            || !matches_result(label, &title, host, &description, topic) || !seen.insert(url.to_string()) { return None; }
        Some(json!({"kind":"search_result","url":url.as_str(),"title":title,"description":description,"publisher":host,
            "retrieved_at":chrono::Utc::now().to_rfc3339()}))
    }).take(5).collect()
}

async fn encyclopedia_search(label: &str, topic: bool) -> Option<(Vec<Value>, &'static str, String)> {
    let mut url = Url::parse("https://en.wikipedia.org/w/api.php").unwrap();
    url.query_pairs_mut().extend_pairs([
        ("action", "query"), ("generator", "search"), ("gsrsearch", label), ("gsrlimit", "3"),
        ("prop", "extracts|info"), ("exintro", "1"), ("exlimit", "3"), ("explaintext", "1"), ("inprop", "url"), ("format", "json"),
    ]);
    let (final_url, bytes) = match tokio::time::timeout(SEARCH_BUDGET,
        crate::link_preview::Service::shared().public_json(url.as_str())).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => { tracing::info!(provider = "wikipedia", reason = "fetch_failed", error = %error, "subject web search failed"); return None; }
        Err(_) => { tracing::info!(provider = "wikipedia", reason = "timeout", "subject web search exceeded its budget"); return None; }
    };
    if final_url.host_str() != Some("en.wikipedia.org") || final_url.path() != "/w/api.php" { return None; }
    let pages = encyclopedia_results(&bytes, label, topic);
    if pages.is_empty() { tracing::info!(provider = "wikipedia", reason = "no_usable_results", "subject web search returned no usable pages"); return None; }
    let mut link = Url::parse("https://en.wikipedia.org/w/index.php").unwrap();
    link.query_pairs_mut().append_pair("search", label);
    Some((pages, "wikipedia", link.to_string()))
}

fn encyclopedia_results(bytes: &[u8], label: &str, topic: bool) -> Vec<Value> {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else { return vec![]; };
    let Some(pages) = value["query"]["pages"].as_object() else { return vec![]; };
    let mut pages: Vec<_> = pages.values().collect();
    pages.sort_by_key(|page| page["index"].as_u64().unwrap_or(u64::MAX));
    pages.into_iter().filter_map(|page| {
        let title = compact(page["title"].as_str()?, 240);
        let description = compact(page["extract"].as_str()?, 600);
        let url = Url::parse(page["fullurl"].as_str()?).ok()?;
        if url.scheme() != "https" || url.host_str() != Some("en.wikipedia.org") || !url.path().starts_with("/wiki/")
            || !url.username().is_empty() || url.password().is_some() || description.is_empty()
            || !matches_result(label, &title, "en.wikipedia.org", &description, topic) { return None; }
        Some(json!({"kind":"search_result","url":url.as_str(),"title":title,"description":description,"publisher":"en.wikipedia.org",
            "retrieved_at":chrono::Utc::now().to_rfc3339()}))
    }).take(3).collect()
}

fn feed_results(bytes: &[u8], label: &str, topic: bool) -> Vec<Value> {
    let Ok(text) = std::str::from_utf8(bytes) else { return vec![]; };
    let Ok(document) = roxmltree::Document::parse(text) else { return vec![]; };
    let name = normalized_subject_name(label);
    let mut seen = std::collections::BTreeSet::new();
    let mut pages: Vec<Value> = document.descendants().filter(|node| node.has_tag_name("item")).take(20).filter_map(|item| {
        let field = |tag| item.children().find(|n| n.has_tag_name(tag)).and_then(|n| n.text());
        let url = Url::parse(field("link")?).ok()?;
        let host = url.host_str()?;
        let title = compact(field("title")?, 240);
        let description = compact(field("description").unwrap_or(""), 600);
        if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() || url.password().is_some()
            || !matches_result(label, &title, host, &description, topic)
            || !seen.insert(url.to_string()) { return None; }
        Some(json!({"kind":"search_result","url":url.as_str(),"title":title,"description":description,"publisher":host,
            "retrieved_at":chrono::Utc::now().to_rfc3339()}))
    }).collect();
    // Prefer a descriptive subject sentence to a homepage promotion when the
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
fn normalized_subject_name(label: &str) -> String {
    subject_words(label).join("")
}
fn subject_words(label: &str) -> Vec<String> {
    label.split_whitespace().map(normalized).filter(|word| !word.is_empty()
        && !["bv", "nv", "ltd", "inc", "llc", "limited", "plc"].contains(&word.as_str())).collect()
}
fn is_topic(kind: &str) -> bool {
    matches!(kind.to_ascii_lowercase().as_str(), "topic" | "query" | "keyword" | "theme" | "news" | "headline" | "category" | "sector" | "industry")
}
/// The parser's type vocabulary is open, so "business and financial headlines"
/// may arrive as any noun. A lowercase multi-word phrase is not a proper name.
fn is_topic_subject(subject: &Subject) -> bool {
    is_topic(&subject.kind)
        || (!subject.label.chars().any(char::is_uppercase) && subject.label.split_whitespace().count() >= 2)
}

fn topic_words(text: &str) -> std::collections::BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric()).map(str::to_lowercase)
        .filter(|word| !word.is_empty() && ![
            "a", "an", "the", "and", "or", "of", "in", "on", "for", "from", "with", "to",
            "latest", "recent", "current", "news", "headline", "headlines", "updates",
        ].contains(&word.as_str()))
        .map(|word| { let singular = topic_singular(&word); if singular == "financial" { "finance".into() } else { singular.into_owned() } }).collect()
}

/// Headlines use plurals ("small businesses", "markets"); fold the common
/// English endings so a topic word matches either form.
fn topic_singular(word: &str) -> std::borrow::Cow<'_, str> {
    if word.len() <= 3 || word.ends_with("ss") || !word.ends_with('s') { return word.into(); }
    if word.ends_with("sses") { return word[..word.len() - 2].into(); }
    if word.len() > 4 && word.ends_with("ies") { return format!("{}y", &word[..word.len() - 3]).into(); }
    word[..word.len() - 1].into()
}

fn matches_result(label: &str, title: &str, host: &str, description: &str, topic: bool) -> bool {
    if !topic { return matches_subject(label, title, host, description); }
    // Topic queries are not proper names. Match meaningful terms, without
    // requiring conjunctions or requested output words in every result.
    let words = topic_words(label);
    if words.is_empty() { return matches_subject(label, title, host, description); }
    let evidence = topic_words(&format!("{title} {host} {description}"));
    let required = words.len().div_ceil(2).max(2).min(words.len());
    words.intersection(&evidence).count() >= required
}

fn matches_subject(label: &str, title: &str, host: &str, description: &str) -> bool {
    let words = subject_words(label);
    let name = words.join("");
    if name.len() < 2 { return false; }
    if normalized(title).contains(&name) || normalized(host).contains(&name) { return true; }
    // A legal name need not occur verbatim in a brand's title or domain.
    // Require every name word in the returned evidence, not just the first word.
    let evidence = format!("{title} {host} {description}").to_lowercase();
    let tokens: Vec<_> = evidence.split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty()).collect();
    words.iter().all(|word| tokens.contains(&word.as_str()))
}

fn homepage_url(raw: &str) -> Option<Url> {
    let raw = raw.trim();
    let mut url = Url::parse(if raw.contains("://") { raw } else { return homepage_url(&format!("https://{raw}")) }).ok()?;
    if !matches!(url.scheme(), "https" | "http") || !url.username().is_empty() || url.password().is_some()
        || !url.host_str()?.contains('.') || url.port().is_some() { return None; }
    url.set_path("/"); url.set_query(None); url.set_fragment(None);
    Some(url)
}
fn likely_subject_host(url: &Url, label: &str, company: bool) -> bool {
    let host = url.host_str().unwrap_or("").trim_start_matches("www.");
    let mut words = subject_words(label);
    if company {
        while words.len() > 1 && words.last().is_some_and(|word| ["bank", "group", "holdings", "company"].contains(&word.as_str())) {
            words.pop();
        }
    }
    let name = words.join("");
    // A matching domain is a candidate, not verified legal identity.
    name.len() >= 3 && host.split('.').next().is_some_and(|part| normalized(part) == name || normalized(part) == normalized_subject_name(label))
}

fn search_results(bytes: &[u8], label: &str, topic: bool) -> Vec<Value> {
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
        let description = link.ancestors().take(5).filter_map(scraper::ElementRef::wrap)
            // Do not borrow another result's snippet from a shared ancestor.
            .take_while(|parent| parent.select(&heading).take(2).count() <= 1)
            .find_map(|parent| parent.select(&snippet).next().map(|node| compact(node.text().collect::<Vec<_>>().join(" "), 600)))
            .unwrap_or_default();
        if !matches_result(label, &title, host, &description, topic) { return None; }
        Some(json!({"kind":"search_result","url":url.as_str(),"title":title,"description":description,"publisher":host}))
    }).take(5).collect()
}

fn homepage_page(url: &Url, bytes: &[u8], label: &str) -> Option<Value> {
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
    Some(json!({"kind":"homepage","company":label,"url":url.as_str(),"title":title,"publisher":url.host_str(),
        "description":description,"text":text,"retrieved_at":chrono::Utc::now().to_rfc3339()}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selected_page_context_retains_provenance_and_does_not_promote_snippets() {
        let mut context = json!({"pages":[
            {"kind":"search_result","url":"https://example.org/a","description":"Search preview only"},
            {"kind":"search_result","url":"https://example.org/b","description":"Unavailable preview"}
        ]});
        let source = crate::web_research::Source { id: 1, title: "Filing".into(),
            url: "https://example.org/a".into(), final_url: "https://example.org/filing".into(), search_rank: 1,
            retrieved_at: "2026-11-02T00:00:00Z".into(), text: "The relevant ownership passage.".into(),
            text_sha256: "hash-of-selected-text".into(), trimmed: true };
        attach_page_context(&mut context, &[source], &[json!({"url":"https://example.org/b","reason":"blocked"})]);
        assert_eq!(context["content_sources_read"], 1);
        assert_eq!(context["pages"][0]["description"], "Search preview only");
        assert_eq!(context["pages"][0]["relevant_text"], "The relevant ownership passage.");
        assert_eq!(context["pages"][0]["final_url"], "https://example.org/filing");
        assert_eq!(context["pages"][0]["trimmed"], true);
        assert_eq!(context["pages"][1]["content_reason"], "blocked");
        assert!(context["pages"][1]["relevant_text"].is_null());
        let subject = Subject { id: "1".into(), kind: "company".into(), label: "Monzo".into(), identifiers: Default::default() };
        assert_ne!(question_cache_key(&subject, "Monzo owners"), question_cache_key(&subject, "Monzo revenue"));
        assert_eq!(question_cache_key(&subject, "Monzo owners"), question_cache_key(&subject, " Monzo owners "));
    }
    #[test]
    fn topic_results_match_content_without_weakening_entity_names() {
        let label = "business and financial headlines";
        let html = br#"<div class='result'><a class='result__a' href='https://example.org/business'>Business news</a><p class='result__snippet'>Finance, markets and economy coverage.</p></div>
          <div class='result'><a class='result__a' href='https://example.org/pensions'>Pensioenfonds Vervoer</a><p class='result__snippet'>Uw pensioen.</p></div>"#;
        assert!(html_search_results(html, label, false).is_empty());
        let pages = html_search_results(html, label, true);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0]["url"], "https://example.org/business");
        let feed = br#"<rss><channel><item><title>Business news</title><link>https://example.org/business</link><description>Finance and markets coverage.</description></item></channel></rss>"#;
        assert_eq!(feed_results(feed, label, true).len(), 1);
        let google = br#"<div><a href='https://example.org/business'><h3>Business news</h3></a><div class='VwiC3b'>Finance and markets coverage.</div></div>"#;
        assert_eq!(search_results(google, label, true).len(), 1);
        assert!(!matches_result("Ada Lovelace", "Ada", "example.org", "A programming language.", false));
        assert!(!matches_result(label, "Business directory", "example.org", "Local addresses.", true));
        assert!(is_topic("topic") && is_topic("query") && is_topic("keyword"));
        assert!(!is_topic("person") && !is_topic("company"));
    }

    #[test]
    fn news_feed_keeps_publishers_and_matches_plural_topic_words() {
        let label = "business and financial headlines";
        let feed = br#"<rss><channel><title>Google News</title>
          <item><title>Small businesses face new finance rules - Example Times</title><link>https://news.google.com/rss/articles/abc?oc=5</link><pubDate>Fri, 16 Oct 2026 07:00:00 GMT</pubDate><source url="https://www.example-times.com">Example Times</source></item>
          <item><title>Small businesses face new finance rules - Example Times</title><link>https://news.google.com/rss/articles/dup?oc=5</link><source url="https://www.example-times.com">Example Times</source></item>
          <item><title>Football results - Sports Daily</title><link>https://news.google.com/rss/articles/def</link><source url="https://sports.example.org">Sports Daily</source></item>
          <item><title>Markets and finance - Business Wire</title><link>javascript:alert(1)</link><source url="https://www.businesswire.com">Business Wire</source></item>
          <item><title>Finances for business owners</title><link>https://news.google.com/rss/articles/ghi</link></item>
        </channel></rss>"#;
        let pages = news_results(feed, label);
        assert_eq!(pages.len(), 2, "{pages:?}");
        assert_eq!(pages[0]["title"], "Small businesses face new finance rules");
        assert_eq!(pages[0]["publisher"], "Example Times");
        assert_eq!(pages[1]["publisher"], "news.google.com");
        assert!(news_results(b"<html>consent</html>", label).is_empty());
        assert_eq!(topic_singular("businesses"), "business");
        assert_eq!(topic_singular("business"), "business");
        assert_eq!(topic_singular("companies"), "company");
        assert_eq!(topic_singular("markets"), "market");
        assert_eq!(topic_singular("gas"), "gas");
        let subject = |kind: &str, label: &str| Subject { id: "1".into(), kind: kind.into(), label: label.into(), identifiers: Default::default() };
        assert!(is_topic_subject(&subject("news", "Business Headlines")));
        assert!(is_topic_subject(&subject("category", label)));
        assert!(!is_topic_subject(&subject("company", "Monzo Bank")));
        assert!(!is_topic_subject(&subject("object", "Eiffel Tower")));
    }

    #[test]
    fn independent_fallbacks_keep_attribution_and_reject_unrelated_results() {
        let html = br#"<div class='result'><a class='result__a' href='//duckduckgo.com/l/?uddg=https%3A%2F%2Fmonzo.com%2F'>Monzo</a><a class='result__snippet'>Monzo Bank Limited is a UK bank.</a></div>
          <div class='result'><a class='result__a' href='https://other.org/'>Other company</a><p class='result__snippet'>Different company</p></div>
          <div class='result'><a class='result__a' href='javascript:bad()'>Monzo Bank</a></div>"#;
        let pages = html_search_results(html, "MONZO BANK LIMITED", false);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0]["url"], "https://monzo.com/");
        assert!(html_search_results(b"<form>Verify that you are human</form>", "Monzo", false).is_empty());
        let json = json!({"query":{"pages":{
            "1":{"title":"Monzo","extract":"Monzo Bank Limited is a British bank.","fullurl":"https://en.wikipedia.org/wiki/Monzo","index":1},
            "2":{"title":"Other bank","extract":"Unrelated content","fullurl":"https://en.wikipedia.org/wiki/Other","index":2},
            "3":{"title":"Monzo","extract":"Monzo Bank Limited","fullurl":"https://evil.org/","index":3}
        }}});
        let pages = encyclopedia_results(&serde_json::to_vec(&json).unwrap(), "MONZO BANK LIMITED", false);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0]["publisher"], "en.wikipedia.org");
        assert!(encyclopedia_results(b"{\"error\":{}}", "Monzo", false).is_empty());
    }

    #[test]
    #[ignore = "requires public responses saved by diagnose-subject-search.mjs"]
    fn replay_saved_public_search_responses() {
        let dir = std::path::Path::new(".local/subject-search-debug");
        let label = "MONZO BANK LIMITED";
        let google = search_results(&std::fs::read(dir.join("google.txt")).unwrap(), label, false);
        let bing = feed_results(&std::fs::read(dir.join("bing.txt")).unwrap(), label, false);
        let ddg = html_search_results(&std::fs::read(dir.join("duckduckgo.txt")).unwrap(), label, false);
        let wiki = encyclopedia_results(&std::fs::read(dir.join("wikipedia.txt")).unwrap(), label, false);
        println!("usable pages: Google={}, Bing={}, DuckDuckGo={}, Wikipedia={}", google.len(), bing.len(), ddg.len(), wiki.len());
        assert!(!ddg.is_empty() || !wiki.is_empty(), "No real fallback response produced a sourced intro");
    }
    #[test]
    fn fallback_feed_keeps_attributed_company_results_and_rejects_unsafe_links() {
        let pages = feed_results(br#"<rss><channel>
          <item><title>Coolblue - electronics</title><link>https://www.coolblue.nl/</link><description>Computers &amp; appliances</description></item>
          <item><title>Coolblue duplicate</title><link>https://www.coolblue.nl/</link></item>
          <item><title>Other company</title><link>https://unrelated.org/</link></item>
          <item><title>Coolblue</title><link>javascript:alert(1)</link></item>
          <item><title>Coolblue overview</title><link>https://encyclopedia.org/Coolblue</link><description>Coolblue is an electronics retailer.</description></item>
        </channel></rss>"#, "Coolblue B.V.", false);
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0]["description"], "Coolblue is an electronics retailer.");
        assert_eq!(pages[1]["description"], "Computers & appliances");
        assert_eq!(pages[0]["kind"], "search_result");
        assert_eq!(pages[0]["publisher"], "encyclopedia.org");
        assert_eq!(pages[1]["publisher"], "www.coolblue.nl");
        assert!(feed_results(b"<html>challenge</html>", "Coolblue", false).is_empty());
    }

    #[test]
    fn legal_names_match_evidence_across_brand_titles_and_descriptions() {
        let feed = br#"<rss><channel>
          <item><title>Monzo | Online banking made easy</title><link>https://monzo.com/</link><description>A UK bank for your everyday money.</description></item>
          <item><title>Monzo</title><link>https://example.org/Monzo</link><description>Monzo Bank Limited is a British bank.</description></item>
          <item><title>Monzo music</title><link>https://example.org/music</link><description>A music festival.</description></item>
          <item><title>Another bank</title><link>https://example.org/other</link><description>Online banking services.</description></item>
        </channel></rss>"#;
        assert_eq!(feed_results(feed, "MONZO BANK LIMITED", false).len(), 2);
        let html = br#"<div><a href='https://monzo.com/'><h3>Monzo | Online banking made easy</h3></a><div class='VwiC3b'>A UK bank for your everyday money.</div></div>"#;
        assert_eq!(search_results(html, "MONZO BANK LIMITED", false).len(), 1);
        let siblings = format!("<section>{}<div><a href='https://unrelated.org/'><h3>Other site</h3></a></div></section>", std::str::from_utf8(html).unwrap());
        assert_eq!(search_results(siblings.as_bytes(), "MONZO BANK LIMITED", false).len(), 1);
        for domain in ["https://monzo.com/", "https://monzobank.com/"] {
            assert!(likely_subject_host(&Url::parse(domain).unwrap(), "MONZO BANK LIMITED", true));
        }
        assert!(!likely_subject_host(&Url::parse("https://monzo.com/").unwrap(), "Monzo Bank", false));
        assert!(!likely_subject_host(&Url::parse("https://bank.com/").unwrap(), "MONZO BANK LIMITED", true));
        assert!(!matches_subject("Ada Lovelace", "Ada", "example.org", "Ada is a programming language."));
        assert!(matches_subject("Ada Lovelace", "Ada", "example.org", "Lovelace was a mathematician."));
    }

    #[tokio::test]
    async fn failed_lookup_can_retry_and_only_success_is_cached() {
        let key = format!("retry-test-{}", uuid::Uuid::new_v4());
        let failed = empty("Example subject");
        assert_eq!(cached_context(key.clone(), async { failed.clone() }).await, failed);
        assert!(cache().get(&key).await.is_none());
        let success = json!({"pages":[{"title":"A sourced overview"}]});
        assert_eq!(cached_context(key.clone(), async { success.clone() }).await, success);
        assert_eq!(cached_context(key, async { panic!("must reuse the successful lookup") }).await, success);
    }

    #[tokio::test]
    #[ignore = "live public internet; makes no paid calls"]
    async fn live_subject_release() {
        for (kind, label) in [("company", "MONZO BANK LIMITED"), ("person", "Ada Lovelace"), ("object", "Eiffel Tower"), ("topic", "business and financial headlines")] {
            let subject = Subject { id: "release-check".into(), kind: kind.into(), label: label.into(), identifiers: Default::default() };
            let context = fetch(&subject).await;
            let pages = context["pages"].as_array().unwrap();
            assert!(pages.iter().any(|page| page["description"].as_str().is_some_and(|s| !s.is_empty())), "No sourced introduction for {label}: {context}");
            println!("Verified {kind}: {label}; provider={}; pages={}", context["search_provider"], pages.len());
        }
        let subject = Subject { id: "homepage-release-check".into(), kind: "company".into(), label: "MONZO BANK LIMITED".into(),
            identifiers: [("website".into(), "https://monzo.com/".into())].into() };
        let context = fetch(&subject).await;
        assert_eq!(context["homepage_status"], "completed", "{context}");
        println!("Verified supplied homepage: monzo.com");
    }

    #[tokio::test]
    #[ignore = "live public internet; makes no paid calls"]
    async fn live_free_subject_context() {
        let company = Subject { id: "live".into(), kind: "company".into(), label: "Coolblue".into(), identifiers: Default::default() };
        let context = fetch(&company).await;
        assert_eq!(context["search_status"], "completed", "{context}");
        assert!(!context["pages"].as_array().unwrap().is_empty());
        println!("COOLBLUE_CONTEXT={context}");
        let company = Subject { label: "Apiosk".into(), identifiers: [("website".into(), "https://apiosk.com/".into())].into(), ..company };
        let context = fetch(&company).await;
        assert_eq!(context["homepage_status"], "completed", "{context}");
        assert!(context["pages"].as_array().unwrap().iter().any(|page| page["kind"] == "homepage"));
        println!("APIOSK_CONTEXT={context}");
    }

    #[test]
    fn search_reads_only_named_company_results_and_unwraps_google_links() {
        let pages = search_results(br#"<a href='/url?q=https%3A%2F%2Fwww.coolblue.nl%2F'><h3>Coolblue - official site</h3></a><a href='https://unrelated.org'><h3>Other business</h3></a><a href='javascript:alert(1)'><h3>Coolblue</h3></a>"#, "Coolblue BV", false);
        assert_eq!(pages.len(), 1); assert_eq!(pages[0]["url"], "https://www.coolblue.nl/");
        assert!(likely_subject_host(&Url::parse(pages[0]["url"].as_str().unwrap()).unwrap(), "Coolblue B.V.", true));
        assert!(!likely_subject_host(&Url::parse("https://companyinfo.nl").unwrap(), "Coolblue", true));
        assert!(search_results(b"<a href='https://other.nl'><h3>Other Inc.</h3></a>", "Apple Inc.", false).is_empty());
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
    fn named_people_places_and_objects_can_have_an_intro_but_identifiers_do_not() {
        let parsed: ParsedRequest = serde_json::from_value(json!({"language":"en","subjects":[{"id":"1","type":"person","label":"Private person","identifiers":{}}],"capabilities":[],"deliverable":{"format":"chat","operations":[]},"meta":{"model":"fixture","parsed_at":"now","text_sha256":"hash"}})).unwrap();
        assert_eq!(named_subjects(&parsed).len(), 1);
        assert_eq!(search_query(&parsed.subjects[0]), "Private person");
        for (kind, label) in [("person", "Ada Lovelace"), ("location", "Rotterdam"), ("object", "Eiffel Tower"), ("product", "Fairphone"), ("company", "IKEA"), ("building", "Rijksmuseum"), ("event", "Eurovision"), ("artwork", "The Night Watch")] {
            let mut parsed = parsed.clone();
            parsed.subjects[0].kind = kind.into();
            parsed.subjects[0].label = label.into();
            assert_eq!(named_subjects(&parsed).len(), 1);
            assert_eq!(named_subjects(&parsed)[0].label, label);
            let expected = if kind == "company" { format!("{label} official website") } else { label.into() };
            assert_eq!(search_query(&parsed.subjects[0]), expected);
        }
        let mut parsed = parsed;
        parsed.subjects[0].kind = "iban".into();
        assert!(named_subjects(&parsed).is_empty());
    }

    #[test]
    fn search_preserves_the_country_and_supports_named_people_and_objects() {
        let subject = Subject { id: "1".into(), kind: "company".into(), label: "Coolblue".into(),
            identifiers: [("country".into(), "Netherlands".into())].into() };
        assert_eq!(search_query(&subject), "Coolblue Netherlands official website");
        for name in ["Ada Lovelace", "Eiffel Tower"] {
            let feed = format!("<rss><channel><item><title>{name}</title><link>https://example.org/profile</link><description>Overview of {name}.</description></item></channel></rss>");
            let pages = feed_results(feed.as_bytes(), name, false);
            assert_eq!(pages.len(), 1);
            assert_eq!(pages[0]["description"], format!("Overview of {name}."));
        }
    }

    #[tokio::test]
    async fn cached_subject_context_is_reused_without_a_second_request() {
        let parsed: ParsedRequest = serde_json::from_value(json!({"language":"en","subjects":[{"id":"1","type":"company","label":"Cached company fixture","identifiers":{}}],"capabilities":[],"deliverable":{"format":"chat","operations":[]},"meta":{"model":"fixture","parsed_at":"now","text_sha256":"hash"}})).unwrap();
        let subject = &parsed.subjects[0];
        let key = context_cache_key(subject);
        let saved = json!({"company":subject.label,"search_status":"completed","pages":[{"title":"Previously saved result"}]});
        cache().insert(key, saved.clone()).await;
        assert_eq!(collect(&parsed).await, vec![saved]);
    }
}
