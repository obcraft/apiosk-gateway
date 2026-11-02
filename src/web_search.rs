//! Free web and news search, without keys or accounts: Apiosk's own SearXNG
//! first, then public feeds and pages that a browser would read. Requests and
//! responses are Tavily-shaped, so the same `web.search` contract, company
//! checks and news projection read either source.
use chrono::{DateTime, NaiveDate, NaiveDateTime, SecondsFormat, Utc};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    sync::OnceLock,
    time::{Duration, Instant},
};
use url::Url;

/// Catalog slug of the free source.
pub const SLUG: &str = "web-search";
/// Stop asking further providers once this many results are collected.
const MIN_RESULTS: usize = 5;
/// Most results taken from any one provider.
const PER_PROVIDER: usize = 10;
const MAX_QUERY_CHARS: usize = 300;
const MAX_CONTENT_CHARS: usize = 280;
const MAX_BYTES: usize = 2 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(8);
// The RSS and HTML endpoints serve browsers, and some reject unknown clients.
const BROWSER_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36";
/// Wikimedia asks API clients to identify themselves rather than pose as a browser.
const APIOSK_UA: &str = "Apiosk/1.0 (https://apiosk.com)";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Range {
    Day,
    Week,
    Month,
    Year,
}
impl Range {
    /// Tavily's `time_range` words and their one-letter forms.
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "day" | "d" => Some(Self::Day),
            "week" | "w" => Some(Self::Week),
            "month" | "m" => Some(Self::Month),
            "year" | "y" => Some(Self::Year),
            _ => None,
        }
    }
    fn word(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Year => "year",
        }
    }
}

struct Request {
    query: String,
    news: bool,
    range: Option<Range>,
    include: Vec<String>,
    exclude: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct Item {
    title: String,
    url: String,
    content: String,
    published: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Provider {
    Searxng,
    BingNews,
    GoogleNews,
    DuckDuckGo,
    Wikipedia,
}
impl Provider {
    fn name(self) -> &'static str {
        match self {
            Self::Searxng => "SearXNG",
            Self::BingNews => "Bing News",
            Self::GoogleNews => "Google News",
            Self::DuckDuckGo => "DuckDuckGo",
            Self::Wikipedia => "Wikipedia",
        }
    }
}
/// Tried in order until `MIN_RESULTS` are collected. SearXNG comes first; the
/// rest keep search working when it is not configured or not answering.
const NEWS: [Provider; 3] = [Provider::Searxng, Provider::BingNews, Provider::GoogleNews];
const WEB: [Provider; 3] = [Provider::Searxng, Provider::DuckDuckGo, Provider::Wikipedia];

/// Runs one Tavily-shaped request body and returns a Tavily-shaped body.
pub async fn search(input: &Value) -> anyhow::Result<Value> {
    let started = Instant::now();
    let request = request_with_limit(input, MAX_QUERY_CHARS)?;
    let merge = collect(&request, MIN_RESULTS, None, |provider| run(provider, &request)).await;
    finish(&request, merge, started.elapsed())
}

/// Keep backup candidates for the five-page research reader. The ordinary
/// search contract retains its existing provider cutoff.
pub(crate) async fn research_candidates(input: &Value, budget: Duration) -> anyhow::Result<Value> {
    let started = Instant::now();
    let request = request_with_limit(input, 16_000)?;
    // A full primary response already provides five pages and five backups.
    // Finish before the adapter's 20s deadline, preserving earlier results if
    // an optional fallback is slow rather than failing the whole question.
    let deadline = tokio::time::Instant::now() + budget;
    let merge = collect(&request, PER_PROVIDER, Some(deadline), |provider| run(provider, &request)).await;
    finish(&request, merge, started.elapsed())
}

async fn collect<F, Fut>(request: &Request, minimum: usize, deadline: Option<tokio::time::Instant>, mut fetch: F) -> Merge
where
    F: FnMut(Provider) -> Fut,
    Fut: std::future::Future<Output = Option<anyhow::Result<Vec<Item>>>>,
{
    let mut merge = Merge::default();
    for provider in if request.news { NEWS } else { WEB } {
        let result = if let Some(deadline) = deadline {
            match tokio::time::timeout_at(deadline, fetch(provider)).await {
                Ok(result) => result,
                Err(_) => {
                    merge.failed = true;
                    tracing::warn!(surface = "web-search", provider = provider.name(), "research search deadline reached; keeping collected results");
                    break;
                }
            }
        } else {
            fetch(provider).await
        };
        match result {
            None => continue,
            Some(Ok(found)) => merge.add(request, provider.name(), found),
            Some(Err(error)) => {
                merge.failed = true;
                tracing::warn!(surface = "web-search", provider = provider.name(), error = %error, "web search provider failed");
            }
        }
        if merge.items.len() >= minimum {
            break;
        }
    }
    merge
}

fn finish(request: &Request, merge: Merge, elapsed: Duration) -> anyhow::Result<Value> {
    anyhow::ensure!(!(merge.items.is_empty() && merge.failed), "web search unavailable");
    Ok(response(&request.query, &merge, elapsed))
}

#[cfg(test)]
fn request(input: &Value) -> anyhow::Result<Request> {
    request_with_limit(input, MAX_QUERY_CHARS)
}

fn request_with_limit(input: &Value, query_limit: usize) -> anyhow::Result<Request> {
    let query = input["query"].as_str().map(str::trim).unwrap_or_default();
    anyhow::ensure!(
        !query.is_empty() && query.chars().count() <= query_limit,
        "Enter a search query of at most {query_limit} characters."
    );
    let domains = |key: &str| -> Vec<String> {
        input[key].as_array().into_iter().flatten().filter_map(Value::as_str)
            .map(|d| d.trim().to_ascii_lowercase())
            .map(|d| d.trim_start_matches("www.").trim_end_matches('.').to_owned())
            .filter(|d| !d.is_empty())
            .collect()
    };
    Ok(Request {
        query: query.to_owned(),
        news: input["topic"] == "news",
        range: input["time_range"].as_str().and_then(Range::parse),
        include: domains("include_domains"),
        exclude: domains("exclude_domains"),
    })
}

/// None when the provider sits this search out.
async fn run(provider: Provider, request: &Request) -> Option<anyhow::Result<Vec<Item>>> {
    let url = match provider {
        Provider::Searxng => searxng_url(searxng_base()?, request),
        Provider::BingNews => bing_url(request),
        Provider::GoogleNews => google_url(request),
        Provider::DuckDuckGo => duckduckgo_url(request),
        // Wikipedia has no date filter, so it cannot answer a timed search.
        Provider::Wikipedia if request.range.is_some() => return None,
        Provider::Wikipedia => wikipedia_url(request),
    };
    let agent = if matches!(provider, Provider::Searxng | Provider::Wikipedia) { APIOSK_UA } else { BROWSER_UA };
    Some(
        async {
            let body = fetch(url, agent).await?;
            match provider {
                Provider::Searxng => searxng_items(&body),
                Provider::BingNews => bing_items(&body),
                Provider::GoogleNews => google_items(&body),
                Provider::DuckDuckGo => Ok(duckduckgo_items(&body)),
                Provider::Wikipedia => wikipedia_items(&body),
            }
        }
        .await,
    )
}

/// Results kept so far, in provider order.
#[derive(Default)]
struct Merge {
    items: Vec<Item>,
    seen: HashSet<String>,
    providers: Vec<&'static str>,
    failed: bool,
}
impl Merge {
    fn add(&mut self, request: &Request, provider: &'static str, found: Vec<Item>) {
        let before = self.items.len();
        for item in found.into_iter().filter(|item| keep(request, item)) {
            if self.items.len() - before >= PER_PROVIDER {
                break;
            }
            if self.seen.insert(item.url.clone()) {
                self.items.push(item);
            }
        }
        if self.items.len() > before {
            self.providers.push(provider);
        }
    }
    #[cfg(test)]
    fn done(&self) -> bool {
        self.items.len() >= MIN_RESULTS
    }
}

/// News rows need a date: the company news projection rejects a whole source
/// when one of its rows has none.
fn keep(request: &Request, item: &Item) -> bool {
    let Some(host) = Url::parse(&item.url).ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
    else {
        return false;
    };
    let on = |domain: &String| host == *domain || host.ends_with(&format!(".{domain}"));
    !item.title.is_empty()
        && (!request.news || item.published.is_some())
        && (request.include.is_empty() || request.include.iter().any(on))
        && !request.exclude.iter().any(on)
}

fn response(query: &str, merge: &Merge, elapsed: Duration) -> Value {
    let results: Vec<Value> = merge.items.iter().map(|item| {
        let mut row = json!({"title": item.title, "url": item.url, "content": item.content});
        if let Some(date) = item.published {
            row["published_date"] = json!(date.to_rfc3339_opts(SecondsFormat::Secs, true));
        }
        row
    }).collect();
    json!({
        "query": query,
        "results": results,
        "response_time": (elapsed.as_secs_f64() * 100.0).round() / 100.0,
        "providers": merge.providers,
    })
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| reqwest::Client::builder().timeout(TIMEOUT).build().expect("web search client"))
}

/// The body of a 200 answer, bounded. Errors leave out the URL, which carries
/// the search query.
async fn fetch(url: Url, agent: &str) -> anyhow::Result<String> {
    let host = url.host_str().unwrap_or_default().to_owned();
    let response = client().get(url).header(reqwest::header::USER_AGENT, agent).send().await
        .map_err(reqwest::Error::without_url)?;
    // DuckDuckGo answers 202 with a challenge page when it rate-limits, so only 200 counts.
    anyhow::ensure!(response.status() == reqwest::StatusCode::OK, "{host} answered {}", response.status().as_u16());
    anyhow::ensure!(response.content_length().is_none_or(|length| length <= MAX_BYTES as u64), "{host} answered too much");
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(reqwest::Error::without_url)?;
        anyhow::ensure!(body.len() + chunk.len() <= MAX_BYTES, "{host} answered too much");
        body.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8(body)?)
}

/// `SEARXNG_URL`, read once. Anything but a plain http(s) base leaves SearXNG out.
fn searxng_base() -> Option<&'static Url> {
    static BASE: OnceLock<Option<Url>> = OnceLock::new();
    BASE.get_or_init(|| std::env::var("SEARXNG_URL").ok().and_then(|raw| checked_base(&raw))).as_ref()
}
fn checked_base(raw: &str) -> Option<Url> {
    let url = Url::parse(raw.trim()).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none())
    .then_some(url)
}

fn searxng_url(base: &Url, request: &Request) -> Url {
    let mut url = base.clone();
    if let Ok(mut path) = url.path_segments_mut() {
        path.pop_if_empty().push("search");
    }
    url.query_pairs_mut()
        .append_pair("q", &request.query)
        .append_pair("format", "json")
        .append_pair("categories", if request.news { "news" } else { "general" })
        .append_pair("language", "en-US");
    if let Some(range) = request.range {
        url.query_pairs_mut().append_pair("time_range", range.word());
    }
    url
}
fn bing_url(request: &Request) -> Url {
    let mut url = Url::parse_with_params("https://www.bing.com/news/search", [("q", request.query.as_str()), ("format", "rss"), ("mkt", "en-US")])
        .expect("Bing News URL");
    // Bing News has no past-year filter; that range searches all dates.
    let interval = match request.range {
        Some(Range::Day) => Some("7"),
        Some(Range::Week) => Some("8"),
        Some(Range::Month) => Some("9"),
        _ => None,
    };
    if let Some(interval) = interval {
        url.query_pairs_mut().append_pair("qft", &format!("interval=\"{interval}\""));
    }
    url
}
fn google_url(request: &Request) -> Url {
    let when = match request.range {
        Some(Range::Day) => " when:1d",
        Some(Range::Week) => " when:7d",
        Some(Range::Month) => " when:30d",
        Some(Range::Year) => " when:1y",
        None => "",
    };
    Url::parse_with_params("https://news.google.com/rss/search", [("q", format!("{}{when}", request.query).as_str()), ("hl", "en-US"), ("gl", "US"), ("ceid", "US:en")])
        .expect("Google News URL")
}
fn duckduckgo_url(request: &Request) -> Url {
    let mut url = Url::parse_with_params("https://html.duckduckgo.com/html/", [("q", request.query.as_str()), ("kl", "us-en")])
        .expect("DuckDuckGo URL");
    if let Some(range) = request.range {
        url.query_pairs_mut().append_pair("df", &range.word()[..1]);
    }
    url
}
fn wikipedia_url(request: &Request) -> Url {
    Url::parse_with_params("https://en.wikipedia.org/w/api.php", [("action", "query"), ("list", "search"), ("srsearch", request.query.as_str()), ("srlimit", "10"), ("format", "json")])
        .expect("Wikipedia URL")
}

fn words(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
/// Markup-free text: tags removed, entities decoded, whitespace collapsed.
fn text(html: &str) -> String {
    words(&scraper::Html::parse_fragment(html).root_element().text().collect::<String>())
}
fn clip(text: &str) -> String {
    text.chars().take(MAX_CONTENT_CHARS).collect()
}
/// RFC 3339 or RFC 2822, or SearXNG's zone-less UTC time or date.
fn parse_date(raw: &str) -> Option<DateTime<Utc>> {
    let raw = raw.trim();
    DateTime::parse_from_rfc3339(raw).or_else(|_| DateTime::parse_from_rfc2822(raw)).ok()
        .map(|date| date.with_timezone(&Utc))
        .or_else(|| ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"].iter()
            .find_map(|format| NaiveDateTime::parse_from_str(raw, format).ok())
            .map(|date| date.and_utc()))
        .or_else(|| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok()?.and_hms_opt(0, 0, 0).map(|date| date.and_utc()))
}

fn searxng_items(body: &str) -> anyhow::Result<Vec<Item>> {
    let value: Value = serde_json::from_str(body)?;
    Ok(value["results"].as_array().into_iter().flatten().filter_map(|row| {
        let url = row["url"].as_str()?.trim().to_owned();
        Some(Item {
            title: text(row["title"].as_str().filter(|t| !t.trim().is_empty()).unwrap_or(&url)),
            content: clip(&text(row["content"].as_str().unwrap_or_default())),
            published: row["publishedDate"].as_str().and_then(parse_date),
            url,
        })
    }).collect())
}

struct RssItem {
    title: String,
    link: String,
    description: String,
    date: Option<DateTime<Utc>>,
    source_name: String,
}
fn rss_items(xml: &str) -> anyhow::Result<Vec<RssItem>> {
    anyhow::ensure!(!xml.contains("<!DOCTYPE") && !xml.contains("<!ENTITY"), "unexpected feed markup");
    let document = roxmltree::Document::parse(xml)?;
    Ok(document.descendants().filter(|n| n.is_element() && n.tag_name().name() == "item").map(|item| {
        let child = |name: &str| item.children().find(|n| n.is_element() && n.tag_name().name() == name);
        // CDATA and entity-split text arrive as several text nodes.
        let inner = |name: &str| child(name)
            .map(|n| n.descendants().filter(|d| d.is_text()).filter_map(|d| d.text()).collect::<String>())
            .unwrap_or_default();
        RssItem {
            title: text(&inner("title")),
            link: inner("link").trim().to_owned(),
            description: text(&inner("description")),
            date: parse_date(&inner("pubDate")),
            source_name: text(&inner("source")),
        }
    }).collect())
}

/// Each Bing link is a redirect that carries the article in its `url` parameter.
fn bing_items(xml: &str) -> anyhow::Result<Vec<Item>> {
    Ok(rss_items(xml)?.into_iter().map(|item| {
        let url = Url::parse(&item.link).ok()
            .and_then(|link| link.query_pairs().find(|(key, _)| key == "url").map(|(_, value)| value.into_owned()))
            .unwrap_or(item.link);
        Item { title: item.title, url, content: clip(&item.description), published: item.date }
    }).collect())
}

/// Google News links stay Google redirects and have no snippet; titles come
/// as "Headline - Publisher".
fn google_items(xml: &str) -> anyhow::Result<Vec<Item>> {
    Ok(rss_items(xml)?.into_iter().map(|item| {
        let suffix = format!(" - {}", item.source_name);
        let title = match item.title.strip_suffix(&suffix) {
            Some(title) if !item.source_name.is_empty() && !title.is_empty() => title.to_owned(),
            _ => item.title.clone(),
        };
        Item { title, url: item.link, content: String::new(), published: item.date }
    }).collect())
}

/// Organic links go through /l/?uddg=<url>; ads go through /y.js and are skipped.
fn duckduckgo_items(html: &str) -> Vec<Item> {
    let selector = |css: &str| scraper::Selector::parse(css).expect("DuckDuckGo selector");
    let (result, link, snippet) = (selector(".result"), selector("a.result__a"), selector(".result__snippet"));
    let base = Url::parse("https://duckduckgo.com/").expect("DuckDuckGo base");
    let document = scraper::Html::parse_document(html);
    document.select(&result).filter_map(|row| {
        let anchor = row.select(&link).next()?;
        let href = Url::options().base_url(Some(&base)).parse(anchor.value().attr("href")?).ok()?;
        if href.path() != "/l/" {
            return None;
        }
        let url = href.query_pairs().find(|(key, _)| key == "uddg")?.1.into_owned();
        let summary = row.select(&snippet).next().map(|s| s.text().collect::<String>()).unwrap_or_default();
        Some(Item { title: words(&anchor.text().collect::<String>()), url, content: clip(&words(&summary)), published: None })
    }).collect()
}

fn wikipedia_items(body: &str) -> anyhow::Result<Vec<Item>> {
    let value: Value = serde_json::from_str(body)?;
    Ok(value["query"]["search"].as_array().into_iter().flatten().filter_map(|row| {
        let title = row["title"].as_str()?.trim();
        let mut url = Url::parse("https://en.wikipedia.org/wiki/").ok()?;
        url.path_segments_mut().ok()?.pop_if_empty().push(&title.replace(' ', "_"));
        Some(Item { title: title.to_owned(), url: url.into(), content: clip(&text(row["snippet"].as_str().unwrap_or_default())), published: None })
    }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(news: bool) -> Request {
        request(&json!({"query": " Coolblue B.V. ", "topic": if news { "news" } else { "general" }})).unwrap()
    }
    fn item(url: &str, dated: bool) -> Item {
        Item { title: "Title".into(), url: url.into(), content: String::new(), published: dated.then(Utc::now) }
    }

    #[test]
    fn requests_are_tavily_shaped_and_bounded() {
        let parsed = request(&json!({"query":"  mars rover ","topic":"news","time_range":"w","include_domains":["www.NASA.gov"]})).unwrap();
        assert_eq!(parsed.query, "mars rover");
        assert!(parsed.news);
        assert_eq!(parsed.range, Some(Range::Week));
        assert_eq!(parsed.include, ["nasa.gov"]);
        assert!(!req(false).news);
        assert!(request(&json!({"query":"   "})).is_err());
        assert!(request(&json!({})).is_err());
        assert!(request(&json!({"query": "a".repeat(MAX_QUERY_CHARS + 1)})).is_err());
        assert_eq!(request(&json!({"query":"x","time_range":"decade"})).unwrap().range, None);
    }

    #[test]
    fn searxng_base_is_a_plain_http_url() {
        assert!(checked_base("http://apiosk-searxng.internal:8080").is_some());
        assert!(checked_base("https://search.example/sub/").is_some());
        for raw in ["ftp://x", "http://user:pw@x", "http://x/?a=1", "http://x/#f", "not a url"] {
            assert!(checked_base(raw).is_none(), "{raw}");
        }
        let url = searxng_url(&checked_base("https://search.example/sub/").unwrap(), &request(&json!({"query":"a b","time_range":"day","topic":"news"})).unwrap());
        assert_eq!(url.as_str(), "https://search.example/sub/search?q=a+b&format=json&categories=news&language=en-US&time_range=day");
    }

    #[test]
    fn provider_urls_carry_their_time_filters() {
        let timed = |range: &str| request(&json!({"query":"acme","time_range":range})).unwrap();
        assert!(bing_url(&timed("day")).as_str().contains("qft=interval%3D%227%22"));
        assert!(!bing_url(&timed("year")).as_str().contains("qft"));
        assert!(google_url(&timed("month")).as_str().contains("q=acme+when%3A30d"));
        assert!(duckduckgo_url(&timed("week")).as_str().ends_with("&df=w"));
        assert!(wikipedia_url(&req(false)).as_str().contains("srsearch=Coolblue+B.V."));
    }

    #[test]
    fn searxng_results_get_utc_dates_and_plain_text() {
        let body = r#"{"results":[
            {"url":"https://a.example/1","title":"A <b>one</b>","content":"x &amp; y\n  z","publishedDate":"2026-10-28T06:21:43"},
            {"url":"https://a.example/2","title":"","content":"","publishedDate":null},
            {"title":"no url"},
            {"url":"https://a.example/3","title":"C","publishedDate":"2026-10-28T06:21:43.123456+02:00"}]}"#;
        let items = searxng_items(body).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].title, "A one");
        assert_eq!(items[0].content, "x & y z");
        assert_eq!(items[0].published.unwrap().to_rfc3339_opts(SecondsFormat::Secs, true), "2026-10-28T06:21:43Z");
        assert_eq!(items[1].title, "https://a.example/2");
        assert_eq!(items[1].published, None);
        assert_eq!(items[2].published.unwrap().to_rfc3339_opts(SecondsFormat::Secs, true), "2026-10-28T04:21:43Z");
        assert!(searxng_items("not json").is_err());
    }

    #[test]
    fn bing_links_resolve_to_the_article() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?><rss version="2.0" xmlns:News="https://www.bing.com:443/news/search?q=x&amp;format=rss"><channel><title>Bing</title>
            <item><title>Coolblue opens &amp; grows</title><link>http://www.bing.com/news/apiclick.aspx?ref=FexRss&amp;aid=&amp;url=https%3a%2f%2fnews.example%2fcoolblue%3fid%3d1&amp;c=1</link>
            <description>Rotterdam &lt;b&gt;retailer&lt;/b&gt; news</description><pubDate>Wed, 28 Oct 2026 06:21:00 GMT</pubDate><News:Source>Example</News:Source></item>
            <item><title><![CDATA[Second ]]>part</title><link>https://plain.example/a</link><description></description></item></channel></rss>"#;
        let items = bing_items(xml).unwrap();
        assert_eq!(items[0].url, "https://news.example/coolblue?id=1");
        assert_eq!(items[0].title, "Coolblue opens & grows");
        assert_eq!(items[0].content, "Rotterdam retailer news");
        assert_eq!(items[0].published.unwrap().to_rfc3339_opts(SecondsFormat::Secs, true), "2026-10-28T06:21:00Z");
        assert_eq!(items[1].url, "https://plain.example/a");
        assert_eq!(items[1].title, "Second part");
        assert_eq!(items[1].published, None);
        assert!(bing_items("<!DOCTYPE x><rss/>").is_err());
    }

    #[test]
    fn google_news_titles_lose_the_publisher() {
        let xml = r#"<rss version="2.0"><channel><item><title>Coolblue expands - Reuters</title>
            <link>https://news.google.com/rss/articles/CBMi?oc=5</link><pubDate>Thu, 29 Oct 2026 08:00:00 GMT</pubDate>
            <description>&lt;a href="x"&gt;Coolblue expands&lt;/a&gt;</description><source url="https://www.reuters.com">Reuters</source></item>
            <item><title>No publisher</title><link>https://news.google.com/rss/articles/B</link></item></channel></rss>"#;
        let items = google_items(xml).unwrap();
        assert_eq!(items[0].title, "Coolblue expands");
        assert_eq!(items[0].url, "https://news.google.com/rss/articles/CBMi?oc=5");
        assert_eq!(items[0].content, "");
        assert!(items[0].published.is_some());
        assert_eq!(items[1].title, "No publisher");
    }

    #[test]
    fn duckduckgo_keeps_organic_results_only() {
        let html = r#"<html><body>
            <div class="result results_links result--ad"><h2 class="result__title"><a class="result__a" href="https://duckduckgo.com/y.js?ad_domain=ads.example&amp;u3=x">Ad</a></h2></div>
            <div class="result results_links web-result"><h2 class="result__title"><a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.coolblue.nl%2Fover&amp;rut=abc">Over  <b>Coolblue</b></a></h2>
            <a class="result__snippet" href="x">Alles voor   een glimlach &amp; meer</a></div>
            <div class="result"><a class="result__a" href="/l/?kh=-1">No target</a></div></body></html>"#;
        let items = duckduckgo_items(html);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].url, "https://www.coolblue.nl/over");
        assert_eq!(items[0].title, "Over Coolblue");
        assert_eq!(items[0].content, "Alles voor een glimlach & meer");
    }

    #[test]
    fn wikipedia_titles_become_article_urls() {
        let body = r#"{"query":{"search":[{"title":"Mars rover","snippet":"A <span class=\"searchmatch\">rover</span> on Mars"},{"title":"AC/DC","snippet":""}]}}"#;
        let items = wikipedia_items(body).unwrap();
        assert_eq!(items[0].url, "https://en.wikipedia.org/wiki/Mars_rover");
        assert_eq!(items[0].content, "A rover on Mars");
        assert_eq!(items[1].url, "https://en.wikipedia.org/wiki/AC%2FDC");
    }

    #[test]
    fn news_keeps_only_dated_http_results() {
        let mut merge = Merge::default();
        merge.add(&req(true), "SearXNG", vec![item("https://a.example/1", false), item("https://a.example/2", true), item("ftp://a.example/3", true)]);
        assert_eq!(merge.items.len(), 1);
        assert_eq!(merge.items[0].url, "https://a.example/2");
        let mut web = Merge::default();
        web.add(&req(false), "DuckDuckGo", vec![item("https://a.example/1", false)]);
        assert_eq!(web.items.len(), 1);
    }

    #[test]
    fn domains_include_subdomains_and_exclusions_win() {
        let filtered = request(&json!({"query":"q","include_domains":["example.com"],"exclude_domains":["ads.example.com"]})).unwrap();
        let mut merge = Merge::default();
        merge.add(&filtered, "SearXNG", vec![
            item("https://example.com/a", true), item("https://www.example.com/b", true),
            item("https://ads.example.com/c", true), item("https://notexample.com/d", true)]);
        let urls: Vec<_> = merge.items.iter().map(|i| i.url.as_str()).collect();
        assert_eq!(urls, ["https://example.com/a", "https://www.example.com/b"]);
    }

    #[test]
    fn merge_dedupes_caps_each_provider_and_stops_at_enough() {
        let request = req(false);
        let mut merge = Merge::default();
        merge.add(&request, "SearXNG", vec![item("https://a.example/1", false), item("https://a.example/1", false), item("https://a.example/2", false)]);
        assert!(!merge.done());
        merge.add(&request, "Wikipedia", vec![]);
        merge.add(&request, "DuckDuckGo", (0..15).map(|i| item(&format!("https://b.example/{i}"), false)).chain([item("https://a.example/2", false)]).collect());
        assert_eq!(merge.items.len(), 2 + PER_PROVIDER);
        assert_eq!(merge.providers, ["SearXNG", "DuckDuckGo"]);
        assert!(merge.done());
        let body = response(&request.query, &merge, Duration::from_millis(1234));
        assert_eq!(body["query"], "Coolblue B.V.");
        assert_eq!(body["response_time"], 1.23);
        assert!(body["results"][0].get("published_date").is_none());
        let mut dated = Merge::default();
        dated.add(&request, "SearXNG", vec![Item { published: parse_date("2026-10-28"), ..item("https://c.example/1", false) }]);
        assert_eq!(response("q", &dated, Duration::ZERO)["results"][0]["published_date"], "2026-10-28T00:00:00Z");
    }

    #[tokio::test]
    async fn research_keeps_collected_results_when_a_later_provider_times_out() {
        let request = req(false);
        let mut attempted = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(10);
        let merge = collect(&request, PER_PROVIDER, Some(deadline), |provider| {
            attempted.push(provider);
            async move {
                if provider == Provider::Searxng {
                    Some(Ok((0..5).map(|i| item(&format!("https://a.example/{i}"), false)).collect()))
                } else {
                    std::future::pending().await
                }
            }
        }).await;
        assert_eq!(attempted, [Provider::Searxng, Provider::DuckDuckGo]);
        assert!(merge.failed);
        let body = finish(&request, merge, Duration::from_millis(10)).unwrap();
        assert_eq!(body["results"].as_array().unwrap().len(), 5);
        assert_eq!(body["providers"], json!(["SearXNG"]));
    }

    #[tokio::test]
    async fn research_stops_after_a_full_primary_response_without_waiting_for_backups() {
        let request = req(false);
        let mut attempted = Vec::new();
        let merge = collect(&request, PER_PROVIDER, Some(tokio::time::Instant::now() + Duration::from_secs(1)), |provider| {
            attempted.push(provider);
            async move { Some(Ok((0..PER_PROVIDER).map(|i| item(&format!("https://a.example/{i}"), false)).collect())) }
        }).await;
        assert_eq!(attempted, [Provider::Searxng]);
        assert_eq!(merge.items.len(), 10);
        assert!(!merge.failed);
    }

    #[tokio::test]
    async fn ordinary_search_keeps_its_existing_five_result_cutoff() {
        let request = req(false);
        let mut attempted = Vec::new();
        let merge = collect(&request, MIN_RESULTS, None, |provider| {
            attempted.push(provider);
            async move { Some(Ok((0..MIN_RESULTS).map(|i| item(&format!("https://a.example/{i}"), false)).collect())) }
        }).await;
        assert_eq!(attempted, [Provider::Searxng]);
        assert_eq!(merge.items.len(), MIN_RESULTS);
    }
}
