//! Optional publisher RSS context. These public reads never receive question or
//! customer data and never replace authoritative execution evidence.
use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};
use futures_util::{stream::FuturesUnordered, StreamExt};
use scraper::Html;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    sync::{Arc, OnceLock, RwLock},
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
use unicode_normalization::UnicodeNormalization;
use url::Url;

#[path = "supporting_news_direct.rs"]
mod direct;
pub(crate) use direct::{classify_news, search_news};

const CACHE_TTL: Duration = Duration::from_secs(600);
const FETCH_TIMEOUT: Duration = Duration::from_millis(1600);
const TOTAL_TIMEOUT: Duration = Duration::from_millis(1900);
const MAX_BYTES: usize = 1024 * 1024;
const MAX_ITEMS: usize = 150;
const MAX_RESULTS: usize = 4;

#[derive(Clone, Copy)]
struct Feed {
    publisher: &'static str,
    url: &'static str,
    domains: &'static [&'static str],
}

const FEEDS: [Feed; 8] = [
    Feed {
        publisher: "BBC News",
        url: "https://feeds.bbci.co.uk/news/rss.xml",
        domains: &["bbc.com", "bbc.co.uk"],
    },
    Feed {
        publisher: "The Guardian",
        url: "https://www.theguardian.com/world/rss",
        domains: &["theguardian.com"],
    },
    Feed {
        publisher: "NPR",
        url: "https://feeds.npr.org/1001/rss.xml",
        domains: &["npr.org"],
    },
    Feed {
        publisher: "Al Jazeera",
        url: "https://www.aljazeera.com/xml/rss/all.xml",
        domains: &["aljazeera.com"],
    },
    Feed {
        publisher: "The New York Times",
        url: "https://rss.nytimes.com/services/xml/rss/nyt/HomePage.xml",
        domains: &["nytimes.com"],
    },
    Feed {
        publisher: "Deutsche Welle",
        url: "https://rss.dw.com/rdf/rss-en-all",
        domains: &["dw.com"],
    },
    Feed {
        publisher: "Ars Technica",
        url: "https://feeds.arstechnica.com/arstechnica/index",
        domains: &["arstechnica.com"],
    },
    Feed {
        publisher: "NOS",
        url: "https://feeds.nos.nl/nosnieuwsalgemeen",
        domains: &["nos.nl"],
    },
];

#[derive(Clone, Debug)]
struct Article {
    title: String,
    summary: String,
    url: String,
    published_at: DateTime<Utc>,
}

#[derive(Default)]
struct CachedFeed {
    checked_at: Option<Instant>,
    refreshed_at: Option<DateTime<Utc>>,
    articles: Arc<Vec<Article>>,
}

#[derive(Default)]
struct FeedCache {
    state: RwLock<CachedFeed>,
    refresh: Mutex<()>,
}

impl FeedCache {
    // Answer composition never waits for a refresh or a cache writer.
    fn snapshot(&self) -> Arc<Vec<Article>> {
        self.state.try_read().map(|cached| cached.articles.clone()).unwrap_or_default()
    }

    // Background work and explicit news searches may load feeds. The short state lock is released
    // before I/O so readers can continue using the last successful snapshot.
    async fn refresh<F, Fut>(&self, load: F)
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Option<Vec<Article>>>,
    {
        let _refresh = self.refresh.lock().await;
        {
            let Ok(mut cached) = self.state.write() else { return };
            if cached.checked_at.is_some_and(|at| at.elapsed() < CACHE_TTL) {
                return;
            }
            // Record the attempt first so cancellation cannot cause a request storm.
            cached.checked_at = Some(Instant::now());
        }
        if let Some(articles) = load().await {
            if let Ok(mut cached) = self.state.write() {
                cached.articles = Arc::new(articles);
                cached.refreshed_at = Some(Utc::now());
            }
        }
    }
}

struct NewsCache {
    client: Option<reqwest::Client>,
    feeds: [FeedCache; 8],
}

impl NewsCache {
    fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent("Apiosk/1.0 (public RSS context; https://apiosk.com)")
                .timeout(FETCH_TIMEOUT)
                .connect_timeout(Duration::from_millis(900))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .ok(),
            feeds: std::array::from_fn(|_| FeedCache::default()),
        }
    }

    fn articles(&self) -> Vec<(Feed, Arc<Vec<Article>>)> {
        FEEDS.into_iter().zip(&self.feeds).map(|(feed, cache)| (feed, cache.snapshot())).collect()
    }

    async fn refresh(&self) {
        let Some(client) = &self.client else {
            return;
        };
        let mut pending = FuturesUnordered::new();
        for (feed, cache) in FEEDS.into_iter().zip(&self.feeds) {
            pending.push(cache.refresh(move || fetch(client, feed)));
        }
        let deadline = tokio::time::Instant::now() + TOTAL_TIMEOUT;
        while let Ok(Some(())) = tokio::time::timeout_at(deadline, pending.next()).await {}
    }
}

static CACHE: OnceLock<NewsCache> = OnceLock::new();
static BACKGROUND_STARTED: OnceLock<()> = OnceLock::new();

/// Start one process-wide refresh loop. Startup and answers never await RSS.
/// No task, question, customer identifier or source result enters this worker.
pub fn start_background_refresh() {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else { return };
    BACKGROUND_STARTED.get_or_init(|| {
        runtime.spawn(async {
            let cache = CACHE.get_or_init(NewsCache::new);
            loop {
                cache.refresh().await;
                tokio::time::sleep(CACHE_TTL).await;
            }
        });
    });
}

/// Select only already-cached articles. A cold or busy cache contributes no
/// news and never starts I/O, holds up the answer or changes its source flow.
pub fn relevant(question: &str, subjects: &[String]) -> Vec<Value> {
    let Some(cache) = CACHE.get() else { return vec![] };
    cached_relevant(cache, question, subjects, Utc::now())
}

fn cached_relevant(cache: &NewsCache, question: &str, subjects: &[String], now: DateTime<Utc>) -> Vec<Value> {
    let Some(query) = Query::new(question, subjects, now) else {
        return vec![];
    };
    select(&query, cache.articles(), now)
}

fn select(query: &Query, feeds: Vec<(Feed, Arc<Vec<Article>>)>, now: DateTime<Utc>) -> Vec<Value> {
    let mut ranked = Vec::new();
    for (feed, articles) in feeds {
        for article in articles
            .iter()
            .filter(|a| recent(a.published_at, now) && a.published_at >= query.earliest)
        {
            if let Some(score) = query.score(article) {
                ranked.push((score, feed, article.clone()));
            }
        }
    }
    ranked.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.2.published_at.cmp(&a.2.published_at))
            .then_with(|| a.2.url.cmp(&b.2.url))
    });
    let mut seen_urls = HashSet::new();
    let mut seen_titles = HashSet::new();
    let mut publisher_counts: HashMap<&str, usize> = HashMap::new();
    ranked.into_iter().filter_map(|(_, feed, a)| {
        if publisher_counts.get(feed.publisher).copied().unwrap_or(0) >= 2 { return None; }
        if !seen_urls.insert(a.url.clone()) || !seen_titles.insert(normalize(&a.title)) {
            return None;
        }
        *publisher_counts.entry(feed.publisher).or_default() += 1;
        Some(json!({
            "publisher": feed.publisher, "title": a.title, "summary": a.summary,
            "url": a.url, "published_at": a.published_at.to_rfc3339_opts(SecondsFormat::Secs, true),
            "feed_url": feed.url,
        }))
    }).take(MAX_RESULTS).collect()
}

async fn fetch(client: &reqwest::Client, feed: Feed) -> Option<Vec<Article>> {
    // Redirects are handled explicitly: even a publisher redirect may only
    // resolve to this feed's publisher domains or its original feed hostname.
    let original = Url::parse(feed.url).ok()?;
    let mut target = original.clone();
    for hop in 0..=2 {
        let response = client
            .get(target.clone())
            .header("Accept", "application/rss+xml, application/xml, text/xml")
            .send()
            .await
            .ok()?;
        if response.status().is_redirection() {
            if hop == 2 {
                return None;
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)?
                .to_str()
                .ok()?;
            let next = target.join(location).ok()?;
            if next.scheme() != "https"
                || !safe_url(&next)
                || !(next.host_str() == original.host_str() || publisher_host(&next, feed))
            {
                return None;
            }
            target = next;
            continue;
        }
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|length| length > MAX_BYTES as u64)
        {
            return None;
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.ok()?;
            if bytes.len().saturating_add(chunk.len()) > MAX_BYTES {
                return None;
            }
            bytes.extend_from_slice(&chunk);
        }
        let xml = std::str::from_utf8(&bytes).ok()?;
        return parse(xml, feed, Utc::now());
    }
    None
}

fn parse(xml: &str, feed: Feed, now: DateTime<Utc>) -> Option<Vec<Article>> {
    if xml.len() > MAX_BYTES || xml.contains("<!DOCTYPE") || xml.contains("<!ENTITY") {
        return None;
    }
    let document = roxmltree::Document::parse(xml).ok()?;
    let root = document.root_element();
    if !matches!(root.tag_name().name(), "rss" | "RDF") {
        return None;
    }
    let mut seen = HashSet::new();
    let mut articles = Vec::new();
    for item in root
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "item")
        .take(MAX_ITEMS)
    {
        let child = |names: &[&str]| {
            item.children()
                .find(|n| n.is_element() && names.contains(&n.tag_name().name()))
        };
        let Some(title) = child(&["title"]).and_then(|n| n.text()) else {
            continue;
        };
        let Some(link) = child(&["link"]).and_then(|n| n.text()) else {
            continue;
        };
        let Some(url) = article_url(link.trim(), feed) else {
            continue;
        };
        let Some(date) = child(&["pubDate", "date", "published"])
            .and_then(|n| n.text())
            .and_then(parse_date)
        else {
            continue;
        };
        if !recent(date, now) {
            continue;
        }
        let title = plain_text(title, 220);
        if title.chars().count() < 12 {
            continue;
        }
        let summary = plain_text(
            child(&["description", "summary", "encoded"])
                .and_then(|n| n.text())
                .unwrap_or(""),
            320,
        );
        if !seen.insert(url.clone()) {
            continue;
        }
        articles.push(Article {
            title,
            summary,
            url,
            published_at: date,
        });
    }
    Some(articles)
}

fn parse_date(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc2822(value.trim())
        .or_else(|_| DateTime::parse_from_rfc3339(value.trim()))
        .ok()
        .map(|date| date.with_timezone(&Utc))
}

fn recent(date: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    date <= now && date >= now - ChronoDuration::days(7)
}

fn safe_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
}

fn publisher_host(url: &Url, feed: Feed) -> bool {
    url.host_str().is_some_and(|host| {
        feed.domains
            .iter()
            .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
    })
}

fn article_url(raw: &str, feed: Feed) -> Option<String> {
    let mut url = Url::parse(raw).ok()?;
    if !safe_url(&url) || !publisher_host(&url, feed) || url.path() == "/" {
        return None;
    }
    url.set_fragment(None);
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| {
            let key = key.to_ascii_lowercase();
            !key.starts_with("utm_")
                && !matches!(
                    key.as_str(),
                    "fbclid"
                        | "gclid"
                        | "ocid"
                        | "cmpid"
                        | "at_medium"
                        | "at_campaign"
                        | "at_campaign_type"
                        | "at_link_type"
                        | "at_link_id"
                        | "at_link_origin"
                        | "at_format"
                        | "at_ptr_name"
                        | "at_bbc_team"
                        | "__source"
                        | "source"
                        | "ref"
                        | "s_cid"
                )
        })
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.set_query(None);
    if !pairs.is_empty() {
        url.query_pairs_mut().extend_pairs(pairs);
    }
    Some(url.to_string())
}

fn plain_text(raw: &str, max_chars: usize) -> String {
    let fragment = Html::parse_fragment(raw);
    let text = fragment
        .root_element()
        .descendants()
        .filter_map(|node| {
            let value = node.value().as_text()?;
            if node.ancestors().any(|ancestor| {
                ancestor.value().as_element().is_some_and(|element| {
                    matches!(
                        element.name(),
                        "script" | "style" | "noscript" | "svg" | "math"
                    )
                })
            }) {
                None
            } else {
                Some(value.as_ref())
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        normalized
    } else {
        let mut short: String = normalized
            .chars()
            .take(max_chars.saturating_sub(1))
            .collect();
        short.push('…');
        short
    }
}

fn normalize(text: &str) -> String {
    text.nfkc()
        .flat_map(char::to_lowercase)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn phrase(haystack: &str, needle: &str) -> bool {
    format!(" {haystack} ").contains(&format!(" {needle} "))
}

fn ignored_term(word: &str) -> bool {
    (word.chars().count() < 3 && word != "ai")
        || word.chars().all(char::is_numeric)
        || matches!(
            word,
            "the"
                | "and"
                | "for"
                | "that"
                | "this"
                | "with"
                | "from"
                | "what"
                | "which"
                | "who"
                | "how"
                | "are"
                | "was"
                | "were"
                | "has"
                | "have"
                | "had"
                | "about"
                | "into"
                | "over"
                | "under"
                | "after"
                | "before"
                | "can"
                | "could"
                | "would"
                | "should"
                | "will"
                | "its"
                | "their"
                | "them"
                | "our"
                | "your"
                | "you"
                | "not"
                | "all"
                | "any"
                | "more"
                | "most"
                | "than"
                | "only"
                | "also"
                | "some"
                | "these"
                | "those"
                | "does"
                | "did"
                | "been"
                | "een"
                | "het"
                | "van"
                | "voor"
                | "met"
                | "dat"
                | "die"
                | "dit"
                | "deze"
                | "zijn"
                | "ook"
                | "welke"
                | "wat"
                | "wie"
                | "hoe"
                | "kan"
                | "kun"
                | "mijn"
                | "jouw"
                | "ons"
                | "onze"
                | "hun"
                | "naar"
                | "uit"
                | "bij"
                | "tot"
                | "door"
                | "aan"
                | "als"
                | "dan"
                | "nog"
                | "niet"
                | "wel"
                | "alle"
                | "veel"
                | "meer"
                | "moet"
                | "maar"
                | "please"
                | "give"
                | "show"
                | "find"
                | "search"
                | "check"
                | "tell"
                | "explain"
                | "compare"
                | "analyse"
                | "analyze"
                | "research"
                | "report"
                | "reports"
                | "results"
                | "result"
                | "information"
                | "details"
                | "data"
                | "news"
                | "latest"
                | "recent"
                | "current"
                | "today"
                | "now"
                | "yesterday"
                | "week"
                | "weeks"
                | "days"
                | "day"
                | "month"
                | "year"
                | "years"
                | "recently"
                | "last"
                | "past"
                | "hours"
                | "hour"
                | "zoek"
                | "zoeken"
                | "geef"
                | "toon"
                | "bekijk"
                | "controleer"
                | "vergelijk"
                | "onderzoek"
                | "informatie"
                | "gegevens"
                | "nieuws"
                | "laatste"
                | "nieuwste"
                | "huidige"
                | "actuele"
                | "actueel"
                | "vandaag"
                | "gisteren"
                | "dagen"
                | "maand"
                | "jaar"
                | "jaren"
                | "afgelopen"
                | "uur"
                | "uren"
                | "weken"
                | "company"
                | "companies"
                | "business"
                | "corporation"
                | "limited"
                | "ltd"
                | "inc"
                | "plc"
                | "group"
                | "holding"
                | "holdings"
                | "bedrijf"
                | "bedrijven"
                | "organisatie"
                | "partner"
                | "customer"
                | "supplier"
                | "klant"
                | "leverancier"
                | "netherlands"
                | "nederland"
                | "dutch"
                | "nederlandse"
                | "europe"
                | "europa"
                | "european"
                | "europese"
                | "amsterdam"
                | "rotterdam"
                | "utrecht"
                | "london"
                | "londen"
                | "paris"
                | "parijs"
                | "berlin"
                | "berlijn"
                | "brussels"
                | "brussel"
                | "france"
                | "frankrijk"
                | "germany"
                | "duitsland"
                | "belgium"
                | "belgie"
                | "belgië"
                | "british"
                | "united"
                | "states"
                | "kingdom"
                | "america"
                | "american"
                | "china"
                | "chinese"
                | "world"
                | "global"
                | "international"
        )
}

// Small bilingual equivalence sets improve literal feed search without an LLM.
// Each set counts as one topic, never several independent matching terms.
fn topic(word: &str) -> &str {
    match word {
        "sanction" | "sanctions" | "sanctioned" | "sanctie" | "sancties" => "sanctions",
        "bankrupt" | "bankruptcy" | "insolvent" | "insolvency" | "failliet" | "faillissement" => {
            "bankruptcy"
        }
        "regulatory" | "regulation" | "regulations" | "regelgeving" | "regulering" => "regulation",
        "emissions" | "emission" | "uitstoot" => "emissions",
        "climate" | "klimaat" => "climate",
        "earnings" | "profits" | "profit" | "winst" | "winstgevendheid" => "profit",
        "revenue" | "revenues" | "omzet" => "revenue",
        "financial" | "financials" | "financieel" | "financiële" | "financiele" => "financial",
        "fraud" | "fraude" | "fraudulent" => "fraud",
        "investigation" | "investigations" | "investigated" => "investigation",
        "risk" | "risks" | "risico" | "risicos" => "risk",
        "privacy" | "gegevensbescherming" => "privacy",
        "artificial" | "kunstmatige" => "artificial",
        "intelligence" | "intelligentie" => "intelligence",
        "energy" | "energie" => "energy",
        "cybersecurity" | "cyberveiligheid" => "cybersecurity",
        "lawsuit" | "lawsuits" | "rechtszaak" | "rechtszaken" => "lawsuit",
        _ => word,
    }
}

fn terms(text: &str) -> HashSet<String> {
    let normalized = format!(" {} ", normalize(text))
        .replace(" artificial intelligence ", " ai ")
        .replace(" kunstmatige intelligentie ", " ai ");
    normalized
        .split_whitespace()
        .filter(|word| !ignored_term(word))
        .map(|word| topic(word).to_owned())
        .collect()
}

const GEOGRAPHIES: &[&[&str]] = &[
    &[
        "europe",
        "european",
        "european union",
        "eu",
        "europa",
        "europese",
    ],
    &["netherlands", "nederland", "dutch", "nederlandse"],
    &[
        "united states",
        "usa",
        "us",
        "america",
        "american",
        "amerika",
        "amerikaanse",
    ],
    &[
        "united kingdom",
        "uk",
        "britain",
        "british",
        "england",
        "engeland",
        "britse",
    ],
    &["germany", "german", "duitsland", "duitse"],
    &["france", "french", "frankrijk", "franse"],
    &["belgium", "belgian", "belgie", "belgië", "belgische"],
    &["china", "chinese"],
    &["india", "indian", "indiase"],
    &["japan", "japanese", "japanse"],
    &["russia", "russian", "rusland", "russische"],
    &[
        "ukraine",
        "ukrainian",
        "oekraine",
        "oekraïne",
        "oekraiense",
        "oekraïense",
    ],
    &["israel", "israeli", "israelische", "israël"],
    &["gaza"],
    &["spain", "spanish", "spanje", "spaanse"],
    &["italy", "italian", "italie", "italië", "italiaanse"],
    &["poland", "polish", "polen", "poolse"],
    &["amsterdam"],
    &["rotterdam"],
    &["utrecht"],
    &["the hague", "den haag"],
    &["london", "londen"],
    &["paris", "parijs"],
    &["berlin", "berlijn"],
    &["brussels", "brussel"],
];

fn entity_name(raw: &str) -> String {
    let mut name = normalize(raw);
    loop {
        let suffix = [
            " b v",
            " n v",
            " ltd",
            " limited",
            " plc",
            " inc",
            " corporation",
            " corp",
            " gmbh",
            " s a",
            " sa",
            " ag",
            " bv",
            " nv",
        ]
        .into_iter()
        .find(|suffix| name.ends_with(suffix));
        let Some(suffix) = suffix else {
            break;
        };
        name.truncate(name.len() - suffix.len());
    }
    name
}

fn geography_matches(raw: &str, normalized: &str, aliases: &[&str]) -> bool {
    aliases.iter().any(|place| {
        if *place == "us" {
            raw.split(|c: char| !c.is_alphanumeric())
                .any(|word| word == "US")
                || raw.contains("U.S.")
        } else {
            phrase(normalized, place)
        }
    })
}

struct Query {
    subjects: Vec<String>,
    topics: HashSet<String>,
    geographies: Vec<&'static [&'static str]>,
    earliest: DateTime<Utc>,
}

impl Query {
    fn new(question: &str, subjects: &[String], now: DateTime<Utc>) -> Option<Self> {
        use chrono::Datelike;
        let normalized = normalize(question);
        let words: Vec<_> = normalized.split_whitespace().collect();
        let historical = words.iter().any(|word| {
            word.parse::<i32>()
                .is_ok_and(|year| (1900..now.year()).contains(&year))
        });
        let asks_current = words.iter().any(|word| {
            matches!(
                *word,
                "latest"
                    | "recent"
                    | "current"
                    | "today"
                    | "now"
                    | "recently"
                    | "laatste"
                    | "nieuwste"
                    | "huidige"
                    | "actuele"
                    | "actueel"
                    | "vandaag"
                    | "nu"
            )
        });
        let unsupported_historical = [
            "yesterday",
            "gisteren",
            "last year",
            "previous year",
            "vorig jaar",
            "last month",
            "previous month",
            "vorige maand",
            "last week",
            "vorige week",
        ]
        .iter()
        .any(|period| phrase(&normalized, period));
        // Exact historical calendar dates need a historical index, not the
        // latest feed. Date-only qualifiers are deliberately not guessed.
        let calendar_date = question.split_whitespace().any(|word| {
            chrono::NaiveDate::parse_from_str(
                word.trim_matches(|c: char| !c.is_ascii_digit() && c != '-'),
                "%Y-%m-%d",
            )
            .is_ok()
        });
        if (historical || unsupported_historical || calendar_date) && !asks_current {
            return None;
        }
        let mut earliest = now - ChronoDuration::days(7);
        for window in words.windows(2) {
            if !matches!(window[0], "past" | "last" | "afgelopen" | "laatste") {
                continue;
            }
            let requested = match window[1] {
                "hour" | "uur" => Some(ChronoDuration::hours(1)),
                "day" | "dag" => Some(ChronoDuration::days(1)),
                "week" => Some(ChronoDuration::weeks(1)),
                _ => None,
            };
            if let Some(requested) = requested {
                earliest = earliest.max(now - requested);
            }
        }
        for window in words.windows(3) {
            if !matches!(window[0], "past" | "last" | "afgelopen" | "laatste") {
                continue;
            }
            let Ok(count) = window[1].parse::<i64>() else {
                continue;
            };
            if !(1..=10000).contains(&count) {
                return None;
            }
            let requested = match window[2] {
                "hour" | "hours" | "uur" | "uren" => ChronoDuration::hours(count),
                "day" | "days" | "dag" | "dagen" => ChronoDuration::days(count),
                "week" | "weeks" | "weken" => ChronoDuration::weeks(count),
                _ => continue,
            };
            earliest = earliest.max(now - requested);
        }
        // 'Today' gets a conservative subset: only articles after midnight UTC.
        // Feeds have explicit offsets; no local publication timezone is guessed.
        if words
            .iter()
            .any(|word| matches!(*word, "today" | "vandaag"))
        {
            earliest = earliest.max(now.date_naive().and_hms_opt(0, 0, 0)?.and_utc());
        }
        let geographies: Vec<_> = GEOGRAPHIES
            .iter()
            .copied()
            .filter(|aliases| geography_matches(question, &normalized, aliases))
            .collect();
        let geography_terms: HashSet<String> = geographies
            .iter()
            .flat_map(|aliases| aliases.iter())
            .flat_map(|alias| terms(alias))
            .collect();
        let subjects: Vec<String> = subjects
            .iter()
            .take(16)
            .map(|s| entity_name(s))
            .filter(|s| s.len() <= 160 && !terms(s).is_empty())
            .collect();
        let subject_terms: HashSet<String> =
            subjects.iter().flat_map(|subject| terms(subject)).collect();
        let topics: HashSet<String> = terms(question)
            .difference(&subject_terms)
            .filter(|term| !geography_terms.contains(*term))
            .cloned()
            .collect();
        if topics.is_empty() || (subjects.is_empty() && topics.len() < 2) {
            return None;
        }
        Some(Self {
            subjects,
            topics,
            geographies,
            earliest,
        })
    }

    fn score(&self, article: &Article) -> Option<usize> {
        let title = normalize(&article.title);
        let raw_text = format!("{} {}", article.title, article.summary);
        let text = normalize(&raw_text);
        if article.published_at < self.earliest {
            return None;
        }
        if !self.geographies.is_empty()
            && !self
                .geographies
                .iter()
                .any(|aliases| geography_matches(&raw_text, &text, aliases))
        {
            return None;
        }
        let article_terms = terms(&text);
        let matched = self.topics.intersection(&article_terms).count();
        if matched == 0 {
            return None;
        }
        let title_terms = terms(&title);
        let title_matches = self.topics.intersection(&title_terms).count();
        if self.subjects.is_empty() {
            return (matched >= 2 && title_matches >= 1).then_some(matched * 3 + title_matches);
        }
        let subject = self
            .subjects
            .iter()
            .find(|subject| phrase(&text, subject))?;
        // Ambiguous single-word brands need explicit corporate context and
        // a topical match in the headline, or two independent matching topics.
        if matches!(subject.as_str(), "apple" | "meta" | "shell") {
            let corporate = [
                "company",
                "shares",
                "stock",
                "earnings",
                "revenue",
                "profit",
                "ceo",
                "iphone",
                "ipad",
                "facebook",
                "instagram",
                "whatsapp",
                "oil",
                "energy",
                "bedrijf",
                "aandelen",
                "omzet",
                "winst",
            ]
            .iter()
            .any(|signal| phrase(&text, signal));
            if !phrase(&title, subject) || !corporate || (matched < 2 && title_matches == 0) {
                return None;
            }
        }
        Some(20 + matched * 3 + title_matches + usize::from(phrase(&title, subject)) * 4)
    }
}

#[cfg(test)]
#[path = "supporting_news_tests.rs"]
mod tests;
