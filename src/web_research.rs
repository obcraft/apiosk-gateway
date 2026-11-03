//! Bounded multi-page evidence research. Search, extraction, processing and synthesis
//! are separate bounded operations; search snippets never become evidence.
use anyhow::{ensure, Result};
use futures_util::{stream, StreamExt};
use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    time::{Duration, Instant},
};
use url::Url;
mod passages;
mod exa;

pub(crate) const TARGET: usize = 10;
const READ_CONCURRENCY: usize = 5;
const MAX_CANDIDATES: usize = 25;
const TEXT_CHARS: usize = 12_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Candidate {
    pub title: String,
    pub url: String,
    pub rank: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Source {
    pub id: usize,
    pub title: String,
    pub url: String,
    pub final_url: String,
    pub search_rank: usize,
    pub retrieved_at: String,
    pub text: String,
    pub text_sha256: String,
    pub trimmed: bool,
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Block {
    pub heading: Option<String>,
    pub text: String,
    pub citations: Vec<usize>,
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Answer {
    pub blocks: Vec<Block>,
    pub limitations: Vec<String>,
}

pub(crate) fn answer_schema(ids: &[usize]) -> Value {
    json!({"type":"object","additionalProperties":false,"required":["blocks","limitations"],"properties":{
        "blocks":{"type":"array","maxItems":12,"items":{"type":"object","additionalProperties":false,
            "required":["heading","text","citations"],"properties":{
                "heading":{"type":["string","null"],"maxLength":120},
                "text":{"type":"string","minLength":1,"maxLength":1000},
                "citations":{"type":"array","minItems":1,"maxItems":5,"items":{"type":"integer","enum":ids}}}}},
        "limitations":{"type":"array","maxItems":5,"items":{"type":"string","minLength":1,"maxLength":500}}
    }})
}

/// Keep operational provenance outside the model context. Citations refer to
/// these same read-source IDs and their final public URLs.
pub(crate) fn synthesis_input(input: &Value) -> Result<(Value, Vec<usize>)> {
    let sources = input["sources"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("missing web evidence"))?;
    ensure!(
        !sources.is_empty() && sources.len() <= TARGET,
        "invalid web evidence count"
    );
    let ids = sources
        .iter()
        .map(|s| {
            s["id"]
                .as_u64()
                .map(|v| v as usize)
                .ok_or_else(|| anyhow::anyhow!("invalid evidence id"))
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        ids.iter().copied().eq(1..=sources.len()),
        "invalid evidence identity"
    );
    let evidence = sources
        .iter()
        .map(|s| {
            json!({"id":s["id"],"title":s["title"],"url":s.get("final_url").unwrap_or(&s["url"]),
        "retrieved_at":s["retrieved_at"],"text":s["text"],"trimmed":s["trimmed"]})
        })
        .collect::<Vec<_>>();
    let mut compact = input.clone();
    compact["sources"] = json!(evidence);
    Ok((compact, ids))
}

fn answer_rejection(value: &Value, count: usize, category: &'static str) -> anyhow::Error {
    use std::sync::atomic::{AtomicU64, Ordering};
    static REJECTIONS: AtomicU64 = AtomicU64::new(0);
    let blocks = value["blocks"].as_array();
    let max_text_chars = blocks
        .into_iter()
        .flatten()
        .filter_map(|b| b["text"].as_str())
        .map(|t| t.chars().count())
        .max()
        .unwrap_or(0);
    tracing::warn!(
        operation = "web-research-validation",
        category,
        rejections = REJECTIONS.fetch_add(1, Ordering::Relaxed) + 1,
        source_count = count,
        block_count = blocks.map_or(0, Vec::len),
        max_text_chars,
        "web answer rejected"
    );
    anyhow::anyhow!("web_answer_invalid_{category}")
}

pub(crate) fn parse_answer(raw: &str, count: usize) -> Result<Value> {
    let answer =
        serde_json::from_str(raw).map_err(|_| answer_rejection(&Value::Null, count, "json"))?;
    validate_answer(&answer, count)?;
    Ok(answer)
}

/// Read only complete objects from the root `blocks` array. No unfinished
/// string, draft sentence or citation is exposed. Other root properties may
/// precede blocks, so find the array with a JSON parser rather than substring
/// matching text that could itself contain `"blocks"`.
pub(crate) fn streamed_answer(raw: &str, count: usize) -> Result<Option<Value>> {
    fn whitespace(bytes: &[u8], position: &mut usize) {
        while bytes.get(*position).is_some_and(u8::is_ascii_whitespace) {
            *position += 1;
        }
    }
    fn value(raw: &str, position: &mut usize) -> Result<Option<Value>> {
        let mut reader = serde_json::Deserializer::from_str(&raw[*position..]).into_iter::<Value>();
        match reader.next() {
            Some(Ok(value)) => {
                *position += reader.byte_offset();
                Ok(Some(value))
            }
            Some(Err(error)) if error.is_eof() => Ok(None),
            None => Ok(None),
            _ => anyhow::bail!("invalid streamed web answer"),
        }
    }
    let bytes = raw.as_bytes();
    let mut position = 0;
    whitespace(bytes, &mut position);
    if bytes.get(position) != Some(&b'{') {
        return Ok(None);
    }
    position += 1;
    loop {
        whitespace(bytes, &mut position);
        if bytes.get(position).is_none_or(|c| *c == b'}') {
            return Ok(None);
        }
        let Some(key) = value(raw, &mut position)? else {
            return Ok(None);
        };
        let Some(key) = key.as_str() else {
            anyhow::bail!("invalid streamed answer property");
        };
        whitespace(bytes, &mut position);
        let Some(separator) = bytes.get(position) else {
            return Ok(None);
        };
        ensure!(*separator == b':', "invalid streamed answer property");
        position += 1;
        whitespace(bytes, &mut position);
        if key == "blocks" {
            let Some(start) = bytes.get(position) else {
                return Ok(None);
            };
            ensure!(*start == b'[', "invalid streamed answer blocks");
            position += 1;
            let mut blocks = Vec::new();
            loop {
                whitespace(bytes, &mut position);
                if bytes.get(position).is_none_or(|c| *c == b']') {
                    break;
                }
                let Some(block) = value(raw, &mut position)? else {
                    break;
                };
                blocks.push(block);
                ensure!(blocks.len() <= 12, "streamed answer too large");
                whitespace(bytes, &mut position);
                match bytes.get(position) {
                    Some(b',') => position += 1,
                    Some(b']') | None => break,
                    _ => anyhow::bail!("invalid streamed answer separator"),
                }
            }
            if blocks.is_empty() {
                return Ok(None);
            }
            let answer = json!({"blocks":blocks,"limitations":[]});
            validate_answer(&answer, count)?;
            return Ok(Some(answer));
        }
        if value(raw, &mut position)?.is_none() {
            return Ok(None);
        }
        whitespace(bytes, &mut position);
        match bytes.get(position) {
            Some(b',') => position += 1,
            Some(b'}') | None => return Ok(None),
            _ => anyhow::bail!("invalid streamed answer separator"),
        }
    }
}

pub(crate) struct SearchQuery {
    pub query: String,
    pub elapsed_ms: u64,
    pub method: &'static str,
    pub usage: Value,
}

/// Compact conversational questions for search and resolve follow-ups without
/// rewriting the question synthesis answers. Outages preserve the original.
pub(crate) async fn search_query(
    client: &crate::ask_v2::Client,
    question: &str,
    earlier: &str,
) -> SearchQuery {
    if earlier.trim().is_empty() && question.split_whitespace().count() <= 5 && !question.contains('?') {
        return SearchQuery {
            query: question.into(),
            elapsed_ms: 0,
            method: "original",
            usage: Value::Null,
        };
    }
    let started = Instant::now();
    let (query, usage, method) = match client.resolve_web_query(question, earlier).await {
        Ok((query, usage)) => (query, usage, if earlier.trim().is_empty() { "question_model" } else { "context_model" }),
        Err(_) => {
            tracing::warn!(
                operation = "web-query-resolution",
                status = "fallback",
                "research operation"
            );
            (question.into(), Value::Null, "fallback")
        }
    };
    SearchQuery {
        query,
        usage,
        method,
        elapsed_ms: started.elapsed().as_millis() as u64,
    }
}

fn normalized(raw: &str, limit: usize) -> String {
    raw.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| {
            !c.is_control() && !matches!(*c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .take(limit)
        .collect()
}
fn url_key(raw: &str) -> Option<String> {
    let mut url = crate::link_preview::public_url(raw).ok()?;
    let host = url.host_str()?.trim_start_matches("www.").to_owned();
    url.set_host(Some(&host)).ok()?;
    url.set_scheme("https").ok()?;
    let query = url
        .query_pairs()
        .filter(|(k, _)| !k.starts_with("utm_") && !["fbclid", "gclid"].contains(&k.as_ref()))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect::<Vec<_>>();
    url.set_query(None);
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query);
    }
    Some(url.as_str().trim_end_matches('/').to_owned())
}

/// Keep user-supplied links even when query rewriting omits them. The same
/// public-URL, DNS, robots and content validation applies when they are read.
fn question_links(question: &str) -> Vec<Candidate> {
    let mut seen = HashSet::new();
    question.split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '(' | ')' | '[' | ']'))
        .filter_map(|part| {
            let raw = part.trim_end_matches([',', '.', ';', '!', '?']);
            if !raw.starts_with("https://") && !raw.starts_with("http://") { return None; }
            let url = crate::link_preview::public_url(raw).ok()?;
            if !seen.insert(url_key(url.as_str())?) { return None; }
            Some(Candidate { title: url.host_str()?.to_owned(), url: url.into(), rank: 0 })
        }).take(TARGET).collect()
}

fn prioritize_links(mut links: Vec<Candidate>, candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut seen: HashSet<_> = links.iter().filter_map(|c| url_key(&c.url)).collect();
    links.extend(candidates.into_iter().filter(|c| url_key(&c.url).is_some_and(|key| seen.insert(key))));
    links.truncate(MAX_CANDIDATES);
    for (index, candidate) in links.iter_mut().enumerate() { candidate.rank = index + 1; }
    links
}

pub(crate) async fn search_for_question(query: &str, question: &str) -> Result<(Vec<Candidate>, Value, Value)> {
    let links = question_links(question);
    match search(query).await {
        Ok((candidates, providers, operation)) => Ok((prioritize_links(links, candidates), providers, operation)),
        Err(_) if !links.is_empty() => Ok((prioritize_links(links, Vec::new()), json!([]),
            json!({"status":"unavailable","cost_usd":null,"configured_api_requests":null,"fallback":"question_links"}))),
        Err(error) => Err(error),
    }
}

pub(crate) async fn search(question: &str) -> Result<(Vec<Candidate>, Value, Value)> {
    exa::supplement(question).await
}

async fn primary_search(question: &str) -> Result<(Vec<Candidate>, Value, Value)> {
    let started = Instant::now();
    let mut accounting = json!({"cost_usd":0,"configured_api_requests":0});
    let mut candidates = Vec::new();
    let mut providers: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    // Respect the Ask search provider already selected by the operator. Normal
    // questions must not silently use different discovery from source mode.
    if crate::subject_context::serpapi::enabled() {
        // Subscription credits are an operator cost, not a free request or a
        // buyer charge. No unit price is available here, so do not invent one.
        accounting = json!({"cost_usd":null,"configured_api_requests":1,"billing":"operator_subscription"});
        match crate::subject_context::serpapi::research(question).await {
            Ok(pages) if !pages.is_empty() => {
                candidates = pages.iter().enumerate().filter_map(|(rank, page)| {
                    let url = page["url"].as_str()?;
                    if !seen.insert(url_key(url)?) { return None; }
                    Some(Candidate { title: normalized(page["title"].as_str().unwrap_or(url), 300), url: url.into(), rank: rank + 1 })
                }).take(MAX_CANDIDATES).collect::<Vec<_>>();
                if !candidates.is_empty() { providers.push("SerpApi".into()); }
                if candidates.len() >= TARGET { return Ok((candidates, json!(providers), accounting)); }
            },
            Ok(_) => {},
            Err(reason) => tracing::warn!(operation="web-research-search",provider="SerpApi",reason,"configured search unavailable; trying public search"),
        }
    }
    let data = match crate::web_search::research_candidates(&json!({"query":question}), Duration::from_secs(18).saturating_sub(started.elapsed())).await {
        Ok(data) => data,
        Err(_) if !candidates.is_empty() => return Ok((candidates, json!(providers), accounting)),
        Err(error) => return Err(error),
    };
    providers.extend(data["providers"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned));
    let offset = candidates.len();
    let backup = data["results"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(rank, row)| {
            let url = row["url"].as_str()?;
            let key = url_key(url)?;
            if !seen.insert(key) {
                return None;
            }
            Some(Candidate {
                title: normalized(row["title"].as_str().unwrap_or(url), 300),
                url: url.into(),
                rank: offset + rank + 1,
            })
        })
        .take(MAX_CANDIDATES - candidates.len())
        .collect::<Vec<_>>();
    candidates.extend(backup);
    Ok((candidates, json!(providers), accounting))
}

pub(crate) fn restricted(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    ["nosnippet", "noarchive", "noai"]
        .iter()
        .any(|rule| value.contains(rule))
}
fn excluded(element: ElementRef<'_>) -> bool {
    let e = element.value();
    matches!(
        e.name(),
        "head"
            | "script"
            | "style"
            | "nav"
            | "footer"
            | "aside"
            | "form"
            | "button"
            | "noscript"
            | "svg"
            | "template"
    ) || (e.name() == "header" && !element.ancestors().filter_map(ElementRef::wrap)
            .any(|parent| matches!(parent.value().name(), "main" | "article") || parent.value().attr("role") == Some("main")))
        || e.attr("hidden").is_some()
        || e.attr("aria-hidden") == Some("true")
        || e.attr("role")
            .is_some_and(|r| matches!(r, "navigation" | "banner" | "contentinfo" | "dialog"))
        // Layout classes on the document (e.g. Drupal's layout-no-sidebars)
        // describe the page, not removable content. Explicit paywall markers
        // still exclude content, including when applied to the whole page.
        || [e.attr("id"), e.attr("class")]
            .into_iter()
            .flatten()
            .any(|v| {
                let v = v.to_ascii_lowercase();
                v.split_whitespace().any(|class| {
                    class.contains("paywall") || (!matches!(e.name(), "html" | "body")
                        && !class.contains("no-sidebar") && !class.contains("without-sidebar")
                        && ["cookie", "consent", "advert", "sidebar"]
                            .iter().any(|word| class.contains(word)))
                })
            })
}
fn extract(candidate: &Candidate, final_url: Url, bytes: &[u8], question: &str, subject: Option<&str>) -> Result<Source> {
    let html = Html::parse_document(&String::from_utf8_lossy(bytes));
    let meta = Selector::parse("meta[name='robots'],meta[name='ApioskResearchBot']").unwrap();
    ensure!(
        !html
            .select(&meta)
            .any(|e| e.value().attr("content").is_some_and(restricted)),
        "restricted page"
    );
    let title = html
        .select(&Selector::parse("title").unwrap())
        .next()
        .map(|e| normalized(&e.text().collect::<Vec<_>>().join(" "), 300))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| candidate.title.clone());
    let roots = Selector::parse("main,article,[role='main']").unwrap();
    let mut content: Vec<_> = html
        .select(&roots)
        .filter(|element| !element.ancestors().filter_map(ElementRef::wrap).any(|parent|
            matches!(parent.value().name(), "main" | "article") || parent.value().attr("role") == Some("main")))
        .flat_map(passages::readable).collect();
    if content.is_empty() { content = passages::readable(html.root_element()); }
    ensure!(passages::size(&content) >= 200, "insufficient readable text");
    let beginning = format!("{} {}", title,
        normalized(&html.root_element().text().take(20).collect::<Vec<_>>().join(" "), 200))
        .to_ascii_lowercase();
    ensure!(
        ![
            "just a moment",
            "verify you are human",
            "access denied",
            "enable javascript and cookies",
            "sign in to continue",
            "subscribe to continue"
        ]
        .iter()
        .any(|s| beginning.contains(s)),
        "blocked page"
    );
    let (text, trimmed) = passages::select(&content, question, &title, TEXT_CHARS, subject)
        .ok_or_else(|| anyhow::anyhow!("irrelevant page"))?;
    Ok(Source {
        id: 0,
        title,
        url: candidate.url.clone(),
        final_url: final_url.into(),
        search_rank: candidate.rank,
        retrieved_at: chrono::Utc::now().to_rfc3339(),
        text_sha256: hex::encode(Sha256::digest(text.as_bytes())),
        text,
        trimmed,
    })
}

/// A good search result is not necessarily readable. Once the configured
/// results are exhausted, try other public search providers within the same
/// evidence deadline. Search snippets never substitute for fetched pages.
pub(crate) async fn read_replacing(candidates: Vec<Candidate>, question: &str, query: &str) -> (Vec<Source>, Vec<Value>, Value) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(26);
    let attempted = candidates.len();
    let rank = candidates.iter().map(|c| c.rank).max().unwrap_or(0);
    let mut seen: HashSet<_> = candidates.iter().filter_map(|c| url_key(&c.url)).collect();
    let first_budget = if attempted >= MAX_CANDIDATES { 26 } else { 16 };
    let (sources, mut failures) = read_for(candidates, question, Duration::from_secs(first_budget), None).await;
    let mut operation = json!({"cost_usd":0,"elapsed_ms":0,"attempted":false,"providers":[]});
    if sources.len() >= TARGET || attempted >= MAX_CANDIDATES { return (sources, failures, operation); }
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining < Duration::from_secs(2) { return (sources, failures, operation); }
    let start = Instant::now();
    operation["attempted"] = json!(true);
    let backup = crate::web_search::research_candidates(&json!({"query":query}), remaining.min(Duration::from_secs(8))).await;
    operation["elapsed_ms"] = json!(start.elapsed().as_millis() as u64);
    let data = match backup {
        Ok(data) => data,
        Err(_) => { operation["status"] = json!("unavailable"); return (sources, failures, operation); }
    };
    operation["providers"] = data["providers"].clone();
    seen.extend(sources.iter().filter_map(|source| url_key(&source.final_url)));
    let replacements = data["results"].as_array().into_iter().flatten().filter_map(|row| {
        let url = row["url"].as_str()?;
        if !seen.insert(url_key(url)?) { return None; }
        Some((url.to_owned(), normalized(row["title"].as_str().unwrap_or(url), 300)))
    }).take(MAX_CANDIDATES - attempted).enumerate().map(|(index, (url, title))|
        Candidate { url, title, rank: rank + index + 1 }).collect::<Vec<_>>();
    operation["candidate_count"] = json!(replacements.len());
    operation["status"] = json!("complete");
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    let (sources, skipped) = read_for_from(replacements, question, remaining, None, sources).await;
    failures.extend(skipped);
    (sources, failures, operation)
}

/// Optional context is bounded separately and never blocks catalog delivery.
pub(crate) async fn read_context(candidates: Vec<Candidate>, question: &str, subject: &str) -> (Vec<Source>, Vec<Value>) {
    read_for(candidates, question, Duration::from_secs(8), Some(subject)).await
}

async fn read_for(candidates: Vec<Candidate>, question: &str, budget: Duration, subject: Option<&str>) -> (Vec<Source>, Vec<Value>) {
    read_for_from(candidates, question, budget, subject, Vec::new()).await
}

async fn read_for_from(candidates: Vec<Candidate>, question: &str, budget: Duration, subject: Option<&str>, sources: Vec<Source>) -> (Vec<Source>, Vec<Value>) {
    read_with_budget_from(candidates, question, budget, sources, if subject.is_some() { 5 } else { TARGET }, |candidate| async move {
        let (url, bytes) = crate::link_preview::Service::shared()
            .research_html(&candidate.url)
            .await?;
        extract(&candidate, url, &bytes, question, subject)
    })
    .await
}

#[cfg(test)]
async fn read_with<F, Fut>(
    candidates: Vec<Candidate>,
    question: &str,
    fetch: F,
) -> (Vec<Source>, Vec<Value>)
where
    F: Fn(Candidate) -> Fut,
    Fut: std::future::Future<Output = Result<Source>>,
{
    read_with_budget(candidates, question, Duration::from_secs(26), fetch).await
}

#[cfg(test)]
async fn read_with_budget<F, Fut>(candidates: Vec<Candidate>, question: &str, budget: Duration, fetch: F) -> (Vec<Source>, Vec<Value>)
where
    F: Fn(Candidate) -> Fut,
    Fut: std::future::Future<Output = Result<Source>>,
{
    read_with_budget_from(candidates, question, budget, Vec::new(), TARGET, fetch).await
}

async fn read_with_budget_from<F, Fut>(candidates: Vec<Candidate>, question: &str, budget: Duration, mut sources: Vec<Source>, target: usize, fetch: F) -> (Vec<Source>, Vec<Value>)
where
    F: Fn(Candidate) -> Fut,
    Fut: std::future::Future<Output = Result<Source>>,
{
    let mut failures = Vec::new();
    let deadline = tokio::time::Instant::now() + budget;
    let mut pending = candidates.into_iter();
    let mut seen: HashSet<_> = sources.iter().filter_map(|source| url_key(&source.final_url)).collect();
    let mut hashes: HashSet<_> = sources.iter().map(|source| source.text_sha256.clone()).collect();
    loop {
        if tokio::time::Instant::now() >= deadline { break; }
        let batch = pending
            .by_ref()
            .take((target - sources.len()).min(READ_CONCURRENCY))
            .collect::<Vec<_>>();
        if batch.is_empty() {
            break;
        }
        let attempted = batch.clone();
        let reads = stream::iter(batch.into_iter().map(|candidate| {
            let fetch = &fetch;
            async move {
                let started = Instant::now();
                let result = tokio::time::timeout(Duration::from_secs(8), fetch(candidate.clone()))
                    .await.unwrap_or_else(|_| Err(anyhow::anyhow!("page timeout")));
                (candidate, result, started.elapsed().as_millis() as u64)
            }
        }))
        .buffer_unordered(READ_CONCURRENCY);
        futures_util::pin_mut!(reads);
        let mut completed = Vec::new();
        while let Ok(Some(read)) = tokio::time::timeout_at(deadline, reads.next()).await {
            completed.push(read);
        }
        // Retain completed reads when one page stalls, but preserve search rank
        // and deterministic duplicate selection regardless of arrival order.
        completed.sort_by_key(|(candidate, _, _)| candidate.rank);
        for candidate in &attempted {
            if !completed.iter().any(|(read, _, _)| read.rank == candidate.rank) {
                failures.push(json!({"url":candidate.url,"rank":candidate.rank,"reason":"timeout"}));
            }
        }
        for (candidate, result, ms) in completed {
            let (accepted, reason) = match result {
                Ok(source) if !relevant(question, &source) => (None, "irrelevant"),
                Ok(source)
                    if seen.contains(&url_key(&source.final_url).unwrap_or_default())
                        || hashes.contains(&source.text_sha256) =>
                {
                    (None, "duplicate")
                }
                Ok(source) => (Some(source), "ok"),
                Err(error) => {
                    let message = error.to_string();
                    (
                        None,
                        if message.contains("robots")
                            || message.contains("restricted")
                            || message.contains("blocked")
                        {
                            "blocked"
                        } else if message.contains("timeout") {
                            "timeout"
                        } else if message.contains("irrelevant") {
                            "irrelevant"
                        } else if message.contains("insufficient") {
                            "insufficient_content"
                        } else if message.contains("too large") {
                            "oversized"
                        } else {
                            "inaccessible"
                        },
                    )
                }
            };
            tracing::info!(
                operation = "web-research-extract",
                rank = candidate.rank,
                elapsed_ms = ms,
                status = reason,
                selected_chars = accepted.as_ref().map_or(0, |source| source.text.chars().count()),
                selection = "question_passages",
                cost_usd = 0,
                "research operation"
            );
            if let Some(source) = accepted {
                seen.insert(url_key(&source.final_url).unwrap_or_default());
                hashes.insert(source.text_sha256.clone());
                sources.push(source);
            } else {
                failures.push(json!({"url":candidate.url,"rank":candidate.rank,"reason":reason}));
            }
        }
        if sources.len() == target {
            break;
        }
    }
    for (i, source) in sources.iter_mut().enumerate() {
        source.id = i + 1;
    }
    (sources, failures)
}
fn relevant(question: &str, source: &Source) -> bool {
    let text = format!("{} {}", source.title, source.text).to_lowercase();
    let terms = question
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|t| {
            t.len() >= 2
                && ![
                    "what",
                    "are",
                    "the",
                    "latest",
                    "developments",
                    "in",
                    "and",
                    "for",
                    "of",
                    "how",
                    "is",
                    "a",
                    "an",
                    "to",
                    "me",
                    "about",
                    "with",
                    "wat",
                    "zijn",
                    "het",
                    "de",
                    "een",
                    "van",
                    "en",
                    "over",
                    "laatste",
                    "ontwikkelingen",
                ]
                .contains(&t.as_str())
        })
        .collect::<Vec<_>>();
    let words = text
        .split(|c: char| !c.is_alphanumeric())
        .collect::<HashSet<_>>();
    terms.is_empty()
        || terms.iter().any(|term| {
            if term.len() <= 3 {
                words.contains(term.as_str())
            } else {
                text.contains(term)
            }
        })
}

pub(crate) fn validate_answer(value: &Value, count: usize) -> Result<Answer> {
    let answer: Answer = serde_json::from_value(value.clone())
        .map_err(|_| answer_rejection(value, count, "shape"))?;
    if answer.blocks.len() > 12 || answer.limitations.len() > 5 {
        return Err(answer_rejection(value, count, "answer_size"));
    }
    for block in &answer.blocks {
        if block.text.trim().is_empty() || block.text.chars().count() > 1000 {
            return Err(answer_rejection(value, count, "paragraph_text"));
        }
        if block
            .heading
            .as_ref()
            .is_some_and(|h| h.chars().count() > 120)
        {
            return Err(answer_rejection(value, count, "heading"));
        }
        if block.citations.is_empty()
            || block.citations.len() > 5
            || !block.citations.iter().all(|id| (1..=count).contains(id))
        {
            return Err(answer_rejection(value, count, "citations"));
        }
    }
    if !answer
        .limitations
        .iter()
        .all(|s| !s.trim().is_empty() && s.chars().count() <= 500)
    {
        return Err(answer_rejection(value, count, "limitation"));
    }
    if answer.blocks.is_empty() && answer.limitations.is_empty() {
        return Err(answer_rejection(value, count, "empty_answer"));
    }
    Ok(answer)
}

/// Most-specific robots groups, longest matching path and Allow on ties.
/// Wildcards and terminal $ are honored; ambiguous/unbounded delay skips a site.
pub(crate) fn robots_delay(raw: &str, url: &Url) -> Result<Duration> {
    let mut groups: Vec<(Vec<String>, Vec<(String, String)>)> = Vec::new();
    let mut agents = Vec::new();
    let mut rules = Vec::new();
    for line in raw.lines() {
        let line = line.split('#').next().unwrap_or("");
        let Some((key, val)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let val = val.trim().to_owned();
        if key == "user-agent" {
            if !rules.is_empty() {
                groups.push((std::mem::take(&mut agents), std::mem::take(&mut rules)));
            }
            agents.push(val.to_ascii_lowercase());
        } else if !agents.is_empty() {
            rules.push((key, val));
        }
    }
    groups.push((agents, rules));
    let specificity = |agents: &[String]| {
        agents
            .iter()
            .filter_map(|a| {
                if a == "*" {
                    Some(0)
                } else if !a.is_empty() && "apioskresearchbot".contains(a.as_str()) {
                    Some(a.len())
                } else {
                    None
                }
            })
            .max()
    };
    let best = groups.iter().filter_map(|g| specificity(&g.0)).max();
    let target = canonical(&format!(
        "{}{}",
        url.path(),
        url.query().map(|q| format!("?{q}")).unwrap_or_default()
    ));
    let mut matched: Option<(usize, bool)> = None;
    let mut delay: f64 = 0.25;
    for (_, rules) in groups
        .into_iter()
        .filter(|g| best.is_some() && specificity(&g.0) == best)
    {
        for (key, value) in rules {
            if key == "crawl-delay" {
                delay = delay.max(
                    value
                        .parse::<f64>()
                        .ok()
                        .filter(|v| v.is_finite() && *v >= 0.0)
                        .unwrap_or(60.0),
                );
            }
            if !["allow", "disallow"].contains(&key.as_str()) || value.is_empty() {
                continue;
            }
            let pattern = canonical(&value);
            if glob_match(&pattern, &target) {
                let n = pattern.replace(['*', '$'], "").len();
                let allow = key == "allow";
                if matched.is_none_or(|(prev, a)| n > prev || (n == prev && allow && !a)) {
                    matched = Some((n, allow));
                }
            }
        }
    }
    ensure!(
        matched.is_none_or(|(_, allow)| allow) && delay <= 2.0,
        "robots disallow"
    );
    Ok(Duration::from_secs_f64(delay))
}
fn canonical(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut result = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                    result.push(byte as char);
                } else {
                    result.push_str(&format!("%{byte:02X}"));
                }
                i += 3;
                continue;
            }
        }
        if bytes[i] >= 128 {
            result.push_str(&format!("%{:02X}", bytes[i]));
        } else {
            result.push(bytes[i] as char);
        }
        i += 1;
    }
    result
}
fn glob_match(pattern: &str, target: &str) -> bool {
    // Greedy wildcard matching with backtracking to the last star. Prefix
    // rules get an implicit trailing star; terminal $ requires a full match.
    let pattern = if let Some(p) = pattern.strip_suffix('$') {
        p.to_owned()
    } else {
        format!("{pattern}*")
    };
    let p = pattern.as_bytes();
    let t = target.as_bytes();
    let (mut i, mut j, mut star, mut restart) = (0, 0, None, 0);
    while j < t.len() {
        if i < p.len() && p[i] == b'*' {
            star = Some(i);
            i += 1;
            restart = j;
        } else if i < p.len() && p[i] == t[j] {
            i += 1;
            j += 1;
        } else if let Some(s) = star {
            restart += 1;
            j = restart;
            i = s + 1;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == b'*' {
        i += 1;
    }
    i == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn supplied_links_are_prioritized_deduplicated_and_public_only() {
        let links = question_links("Vergelijk [Alliance](https://example.org/about?utm_source=test) met <https://other.org/research>. Ook https://example.org/about en http://127.0.0.1/private en https://user:secret@example.org/");
        assert_eq!(links.len(), 2);
        let candidates = vec![
            Candidate { title: "duplicate".into(), url: "https://example.org/about".into(), rank: 1 },
            Candidate { title: "independent".into(), url: "https://third.org/report".into(), rank: 2 },
        ];
        let merged = prioritize_links(links, candidates);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].title, "example.org");
        assert_eq!(merged[1].url, "https://other.org/research");
        assert_eq!(merged[2].title, "independent");
        assert_eq!(merged.iter().map(|c| c.rank).collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn a_slow_page_does_not_discard_completed_page_evidence() {
        let candidates = (1..=2).map(|rank| Candidate { title: "AI compliance".into(), url: format!("https://example.org/{rank}"), rank }).collect();
        let (sources, failures) = read_with_budget(candidates, "AI compliance", Duration::from_millis(50), |candidate| async move {
            if candidate.rank == 1 { std::future::pending::<()>().await; }
            Ok(Source { id: 0, title: candidate.title, url: candidate.url.clone(), final_url: candidate.url,
                search_rank: candidate.rank, retrieved_at: "2026-11-02T00:00:00Z".into(),
                text: "AI compliance evidence from the completed page.".into(), text_sha256: "fixture".into(), trimmed: false })
        }).await;
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].search_rank, 2);
        assert_eq!(sources[0].id, 1);
        assert_eq!(failures[0]["reason"], "timeout");
    }

    #[test]
    fn relevant_article_is_found_alongside_a_larger_unrelated_article() {
        let candidate = Candidate { title: "Research archive".into(), url: "https://example.org/".into(), rank: 1 };
        let html = format!("<title>Research archive</title><article><p>{}</p></article><article><h2>AI compliance</h2><p>New compliance requirements apply to model documentation and risk assessments. The obligations have different effective dates depending on the provider and model type.</p></article>", "Sailing trips and ocean weather updates. ".repeat(100));
        let source = extract(&candidate, Url::parse(&candidate.url).unwrap(), html.as_bytes(), "AI compliance", None).unwrap();
        assert!(source.text.contains("documentation"));
        assert!(!source.text.contains("Sailing"));
    }
    #[test]
    fn page_layout_classes_do_not_erase_articles_but_sidebars_still_do() {
        let candidate = Candidate { title: "Election guide 2026".into(), url: "https://example.org/guide".into(), rank: 1 };
        for layout in ["layout-no-sidebars", "no-sidebar", "layout-without-sidebar", "has-cookie-consent"] {
            let html = format!("<body class='{layout}'><main><div class='layout-no-sidebars'><article><h1>Election guide 2026</h1><p>The 2026 midterm elections choose congressional representatives and senators. Voters can check their registration and consult the official election calendar to find the date and deadlines that apply to their district.</p></article></div><aside><p>2026: unrelated shopping offers</p></aside><div class='sidebar'><p>2026: recommended holidays</p></div><div class='cookie-consent'><p>2026 cookie preferences</p></div></main></body>");
            let source = extract(&candidate, Url::parse(&candidate.url).unwrap(), html.as_bytes(), "mid terms 2026", None).unwrap();
            assert!(source.text.contains("congressional representatives"), "{layout}");
            for unwanted in ["shopping", "holidays", "cookie preferences"] { assert!(!source.text.contains(unwanted)); }
        }
        let restricted = format!("<body class='paywall'><article><p>{}</p></article></body>", "The 2026 election article requires a subscription. ".repeat(10));
        assert!(extract(&candidate, Url::parse(&candidate.url).unwrap(), restricted.as_bytes(), "mid terms 2026", None).is_err());
    }
    #[test]
    fn article_header_is_evidence_while_site_navigation_is_not() {
        let candidate = Candidate { title: "Election guide 2026".into(), url: "https://example.org/guide".into(), rank: 1 };
        let html = "<body><header><p>2026 subscribe to our newsletter</p></header><main><article><header><h1>Election guide 2026</h1><p>The 2026 election guide explains the voting dates and offices on the ballot.</p></header><p>Voters should check their district's registration deadlines and official voting instructions. The election calendar lists the deadlines and the offices contested in each district in 2026.</p></article></main></body>";
        let source = extract(&candidate, Url::parse(&candidate.url).unwrap(), html.as_bytes(), "mid terms 2026", None).unwrap();
        assert!(source.text.contains("offices on the ballot"));
        assert!(!source.text.contains("newsletter"));
    }
    #[tokio::test]
    async fn follow_up_query_uses_context_and_resolution_failures_keep_the_original() {
        use axum::{routing::post, Json, Router};
        use std::sync::{Arc, Mutex};
        let received = Arc::new(Mutex::new(Vec::<Value>::new()));
        let capture = received.clone();
        let app=Router::new().route("/",post(move |Json(body):Json<Value>| {
            capture.lock().unwrap().push(body.clone());
            async move {
                let input:Value=serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
                if input["question"]=="Timeout" {tokio::time::sleep(Duration::from_secs(5)).await;}
                let content=if input["question"]=="Malformed" {"not JSON".into()}
                    else if input["question"]=="Too long" {json!({"query":"a".repeat(301)}).to_string()}
                    else {json!({"query":"latest AI compliance developments in the Netherlands"}).to_string()};
                Json(json!({"choices":[{"message":{"content":content}}],"usage":{"prompt_tokens":50,"completion_tokens":12,"cost":0.0001}}))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = crate::ask_v2::Client::new(
            reqwest::Client::new(),
            "fixture-key".into(),
            "unused-parser-model".into(),
        )
        .with_local_endpoint(format!("http://{address}/"))
        .unwrap();
        let question = "And in the Netherlands?";
        let earlier = "user: What are the latest developments in AI compliance?";
        let initial = search_query(&client, "Adriaan Mol", "").await;
        assert_eq!(initial.query, "Adriaan Mol");
        assert_eq!(initial.method, "original");
        assert!(received.lock().unwrap().is_empty());
        let resolved = search_query(&client, question, earlier).await;
        assert_eq!(
            resolved.query,
            "latest AI compliance developments in the Netherlands"
        );
        assert_eq!(resolved.method, "context_model");
        assert_eq!(resolved.usage["cost"], 0.0001);
        let calls = received.lock().unwrap();
        assert_eq!(calls[0]["model"], crate::ask_v2::WEB_QUERY_MODEL);
        let input: Value =
            serde_json::from_str(calls[0]["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(
            input,
            json!({"question":question,"earlier_conversation":earlier})
        );
        drop(calls);
        let standalone = search_query(&client, "What are the latest AI compliance developments in the Netherlands?", "").await;
        assert_eq!(standalone.query, "latest AI compliance developments in the Netherlands");
        assert_eq!(standalone.method, "question_model");
        assert_eq!(standalone.usage["cost"], 0.0001);
        for original in ["Malformed", "Too long", "Timeout"] {
            let started = Instant::now();
            let fallback = search_query(&client, original, earlier).await;
            assert_eq!(fallback.query, original);
            assert_eq!(fallback.method, "fallback");
            assert!(started.elapsed() < Duration::from_secs(4));
        }
        server.abort();
    }

    #[tokio::test]
    async fn reads_in_parallel_and_replaces_failed_duplicate_and_irrelevant_pages() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let active = Arc::new(AtomicUsize::new(0));
        let peak = active.clone();
        let maximum = Arc::new(AtomicUsize::new(0));
        let observed = maximum.clone();
        let attempted = Arc::new(AtomicUsize::new(0));
        let calls = attempted.clone();
        let candidates = (1..=16)
            .map(|rank| Candidate {
                title: "AI rules".into(),
                url: format!("https://example.org/{rank}"),
                rank,
            })
            .collect();
        let (sources, failures) = read_with(candidates, "AI compliance", move |candidate| {
            let active = active.clone();
            let maximum = maximum.clone();
            let attempted = attempted.clone();
            async move {
                attempted.fetch_add(1, Ordering::SeqCst);
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(10)).await;
                active.fetch_sub(1, Ordering::SeqCst);
                if candidate.rank == 2 {
                    anyhow::bail!("blocked");
                }
                let rank = if candidate.rank == 3 {
                    1
                } else {
                    candidate.rank
                };
                let text = if rank == 4 {
                    "Sailing boat news".into()
                } else {
                    format!("AI compliance evidence {rank}")
                };
                Ok(Source {
                    id: 0,
                    title: text.clone(),
                    url: candidate.url,
                    final_url: format!("https://example.org/final/{rank}"),
                    search_rank: candidate.rank,
                    retrieved_at: "2026-11-01T00:00:00Z".into(),
                    text_sha256: hex::encode(Sha256::digest(text.as_bytes())),
                    text,
                    trimmed: false,
                })
            }
        })
        .await;
        assert_eq!(
            sources.iter().map(|s| s.search_rank).collect::<Vec<_>>(),
            vec![1, 5, 6, 7, 8, 9, 10, 11, 12, 13]
        );
        assert_eq!(
            sources.iter().map(|s| s.id).collect::<Vec<_>>(),
            (1..=10).collect::<Vec<_>>()
        );
        assert_eq!(failures.len(), 3);
        assert_eq!(calls.load(Ordering::SeqCst), 13);
        assert_eq!(observed.load(Ordering::SeqCst), 5);
        assert_eq!(peak.load(Ordering::SeqCst), 0);
        let (sources, _) = read_with(
            vec![Candidate {
                title: "Blocked".into(),
                url: "https://example.org/".into(),
                rank: 1,
            }],
            "AI",
            |_| async { Err(anyhow::anyhow!("blocked")) },
        )
        .await;
        assert!(sources.is_empty());
    }

    #[tokio::test]
    async fn replacement_reads_keep_existing_citations_and_reject_redirect_and_content_duplicates() {
        let source = |rank: usize| Source { id: rank, title: "Election 2026".into(),
            url: format!("https://example.org/{rank}"), final_url: format!("https://example.org/{rank}"),
            search_rank: rank, retrieved_at: "2026-11-02T00:00:00Z".into(),
            text: "Relevant election evidence for 2026.".into(), text_sha256: format!("hash-{rank}"), trimmed: false };
        let candidates = (4..=8).map(|rank| Candidate { title: "Election 2026".into(), url: format!("https://example.org/{rank}"), rank }).collect();
        let existing = (1..=3).map(source).collect();
        let (sources, failures) = read_with_budget_from(candidates, "election 2026", Duration::from_secs(1), existing, TARGET, |candidate| async move {
            let mut result = source(candidate.rank);
            if candidate.rank == 4 { result.final_url = "https://example.org/1".into(); }
            if candidate.rank == 5 { result.text_sha256 = "hash-2".into(); }
            Ok(result)
        }).await;
        assert_eq!(sources.iter().map(|s| (s.id, s.search_rank)).collect::<Vec<_>>(), [(1,1),(2,2),(3,3),(4,6),(5,7),(6,8)]);
        assert_eq!(failures.len(), 2);
        assert!(failures.iter().all(|f| f["reason"] == "duplicate"));
    }

    #[test]
    fn streamed_blocks_wait_for_citations_and_parse_only_the_root_array() {
        let first = json!({"heading":null,"text":"Evidence with an escaped \"blocks\" label and emoji ✅.","citations":[1]});
        let prefix = format!(
            "{{\"limitations\":[\"The word blocks is not a property here\"],\"blocks\":[{first},"
        );
        let ready = streamed_answer(&prefix, 1).unwrap().unwrap();
        assert_eq!(ready["blocks"], json!([first]));
        assert!(streamed_answer(
            r#"{"blocks":[{"heading":null,"text":"Do not show an uncited draft"#,
            1
        )
        .unwrap()
        .is_none());
        assert!(streamed_answer(
            r#"{"blocks":[{"heading":null,"text":"Invalid citation","citations":[2]}"#,
            1
        )
        .is_err());
        assert!(streamed_answer(
            r#"{"blocks":[{"heading":null,"text":"Malformed escape \q"#,
            1
        )
        .is_err());
    }

    #[tokio::test]
    async fn provider_stream_publishes_a_cited_block_before_the_provider_finishes() {
        use axum::{
            body::{Body, Bytes},
            http::Response,
            routing::post,
            Json, Router,
        };
        use std::{convert::Infallible, sync::Arc};
        use tokio::sync::{mpsc, Notify};
        let gate = Arc::new(Notify::new());
        let provider_gate = gate.clone();
        let first = json!({"heading":"First finding","text":"The first supported finding arrives early.","citations":[1]});
        let second = json!({"heading":"Second finding","text":"The later supported finding arrives afterward.","citations":[1]});
        let first_content = format!("{{\"blocks\":[{first},");
        let last_content = format!("{second}],\"limitations\":[]}}");
        let app=Router::new().route("/",post(move |Json(payload):Json<Value>| {
            assert_eq!(payload["stream"],true);
            assert_eq!(payload["stream_options"]["include_usage"],true);
            assert_eq!(payload["response_format"]["type"],"json_schema");
            let gate=provider_gate.clone();let first=first_content.clone();let last=last_content.clone();
            async move {
                let frames=futures_util::stream::unfold((0,gate,first,last),|(step,gate,first,last)|async move {
                    let frame=match step {
                        0=>json!({"provider":"fixture","choices":[{"delta":{"content":first},"finish_reason":null}]}).to_string(),
                        1=>{gate.notified().await;json!({"choices":[{"delta":{"content":last},"finish_reason":"stop"}]}).to_string()},
                        2=>json!({"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":25,"cost":0.001}}).to_string(),
                        3=>"[DONE]".into(),
                        _=>return None,
                    };
                    Some((Ok::<_,Infallible>(Bytes::from(format!("data: {frame}\n\n"))),(step+1,gate,first,last)))
                });
                Response::builder().header("content-type","text/event-stream").body(Body::from_stream(frames)).unwrap()
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = crate::ask_v2::Client::new(
            reqwest::Client::new(),
            "fixture-key".into(),
            "unused".into(),
        )
        .with_local_endpoint(format!("http://{address}/"))
        .unwrap();
        let input = json!({"question":"TEST FIXTURE streaming evidence","sources":[{"id":1,"text":"Synthetic evidence"}]});
        let (sender, mut receiver) = mpsc::channel(4);
        let work = tokio::spawn(async move {
            client
                .synthesize_web_streaming(input, move |answer| {
                    let sender = sender.clone();
                    async move {
                        sender.send(answer).await?;
                        Ok(())
                    }
                })
                .await
        });
        let early = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(early["blocks"].as_array().unwrap().len(), 1);
        assert_eq!(
            early["blocks"][0]["text"],
            "The first supported finding arrives early."
        );
        assert!(
            !work.is_finished(),
            "the first paragraph must arrive while the provider is still held open"
        );
        gate.notify_one();
        let final_answer = tokio::time::timeout(Duration::from_secs(2), work)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(final_answer["blocks"].as_array().unwrap().len(), 2);
        assert_eq!(final_answer["usage"]["cost"], 0.001);
        assert_eq!(
            receiver.recv().await.unwrap()["blocks"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        server.abort();
    }

    #[tokio::test]
    async fn provider_eof_without_a_finish_reason_is_not_a_successful_answer() {
        use axum::{body::Body, http::Response, routing::post, Router};
        let answer = json!({"blocks":[{"heading":null,"text":"A complete JSON object is not a completed provider stream.","citations":[1]}],"limitations":[]});
        let frame = format!(
            "data: {}\n\n",
            json!({"choices":[{"delta":{"content":answer.to_string()}}]})
        );
        let app = Router::new().route(
            "/",
            post(move || {
                let frame = frame.clone();
                async move {
                    Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(Body::from(frame))
                        .unwrap()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = crate::ask_v2::Client::new(
            reqwest::Client::new(),
            "fixture-key".into(),
            "unused".into(),
        )
        .with_local_endpoint(format!("http://{address}/"))
        .unwrap();
        let result=client.synthesize_web_streaming(json!({"question":"TEST FIXTURE EOF","sources":[{"id":1,"text":"Synthetic evidence"}]}),|_|async {Ok(())}).await;
        assert_eq!(
            result.unwrap_err().to_string(),
            "web synthesis stream incomplete"
        );
        server.abort();
    }

    #[tokio::test]
    async fn one_synthesis_call_receives_original_question_and_all_ten_pages() {
        use axum::{routing::post, Json, Router};
        use std::sync::{Arc, Mutex};
        let received = Arc::new(Mutex::new(Vec::<Value>::new()));
        let capture = received.clone();
        let app=Router::new().route("/",post(move |Json(body):Json<Value>| {
            let input:Value=serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
            let citations=if input["question"]=="TEST FIXTURE INVALID CITATION" {json!([11])} else {json!([1,10])};
            capture.lock().unwrap().push(body);
            async move {Json(json!({"choices":[{"message":{"content":json!({"blocks":[{"heading":"Developments","text":"One consolidated finding","citations":citations}],"limitations":[]}).to_string()}}],"usage":{"prompt_tokens":100,"completion_tokens":20,"cost":0.001}}))}
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = crate::ask_v2::Client::new(
            reqwest::Client::new(),
            "fixture-key".into(),
            "unused-parser-model".into(),
        )
        .with_local_endpoint(format!("http://{address}/"))
        .unwrap();
        let input = json!({"question":"TEST FIXTURE ONLY: Combine these ten synthetic project findings, explain the common result and cite every factual paragraph.","current_date":"2026-11-01","sources":(1..=10).map(|id|json!({"id":id,"title":format!("TEST FIXTURE {id}"),"url":format!("https://example.org/original-fixture/{id}"),"final_url":format!("https://example.org/test-fixture/{id}"),"retrieved_at":"2026-11-01T00:00:00Z","text":format!("TEST FIXTURE {id}: This is synthetic evidence, not a real event. In the imaginary project, team {id} completed its review. Each team used the same checklist and recorded one improvement."),"trimmed":false,"search_rank":id,"text_sha256":"not-for-model-context"})).collect::<Vec<_>>()});
        let reply = client.synthesize_web(input.clone()).await.unwrap();
        assert_eq!(validate_answer(&reply, 10).unwrap().blocks.len(), 1);
        assert_eq!(reply["usage"]["cost"], 0.001);
        let calls = received.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["model"], crate::ask_v2::WEB_SYNTHESIS_MODEL);
        assert_eq!(calls[0]["provider"]["require_parameters"], true);
        assert_eq!(calls[0]["provider"]["sort"], "latency");
        assert_eq!(calls[0]["response_format"]["type"], "json_schema");
        assert_eq!(calls[0]["response_format"]["json_schema"]["strict"], true);
        let schema = &calls[0]["response_format"]["json_schema"]["schema"];
        assert_eq!(
            schema["properties"]["blocks"]["items"]["properties"]["citations"]["items"]["enum"],
            json!([1, 2, 3, 4, 5, 6, 7, 8, 9, 10])
        );
        let invalid = json!({"blocks":[{"heading":null,"text":"Unsupported claim","citations":[11]}],"limitations":[]});
        assert!(!jsonschema::is_valid(schema, &invalid));
        if let Ok(path) = std::env::var("WEB_RESEARCH_TEST_PAYLOAD") {
            std::fs::write(path, serde_json::to_vec_pretty(&calls[0]).unwrap()).unwrap();
        }
        let sent: Value =
            serde_json::from_str(calls[0]["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(sent["question"], input["question"]);
        assert_eq!(sent, synthesis_input(&input).unwrap().0);
        assert!(sent["sources"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s.as_object().unwrap().len() == 6
                && s.get("search_rank").is_none()
                && s.get("text_sha256").is_none()
                && s.get("final_url").is_none()));
        drop(calls);
        let mut invalid_input = input;
        invalid_input["question"] = json!("TEST FIXTURE INVALID CITATION");
        assert_eq!(
            client
                .synthesize_web(invalid_input)
                .await
                .unwrap_err()
                .to_string(),
            "web_answer_invalid_citations"
        );
        server.abort();
    }
    #[test]
    fn robots_specific_groups_wildcards_encoded_paths_and_restrictions() {
        let u = Url::parse("https://example.org/private/public?a=1").unwrap();
        assert!(robots_delay("User-agent: *\nDisallow: /private", &u).is_err());
        assert!(robots_delay(
            "User-agent: *\nDisallow: /private\nAllow: /private/public",
            &u
        )
        .is_ok());
        assert!(robots_delay(
            "User-agent: *\nDisallow: /\nUser-agent: ApioskResearchBot\nAllow: /",
            &u
        )
        .is_ok());
        assert!(robots_delay("User-agent: *\nDisallow: /*?a=1$", &u).is_err());
        assert!(robots_delay("User-agent: *\nDisallow: /%70rivate", &u).is_err());
        assert!(robots_delay("User-agent: *\nCrawl-delay: 30", &u).is_err());
        assert!(glob_match("/a*b$", "/axbxb"));
        assert!(robots_delay("User-agent: *\nDisallow: /%é", &u).is_ok());
        assert!(restricted("noai, nosnippet"));
    }
    #[test]
    fn extraction_removes_boilerplate_preserves_provenance_and_rejects_challenges() {
        let c = Candidate {
            title: "AI rules".into(),
            url: "https://example.org/original".into(),
            rank: 3,
        };
        let text = "AI compliance rules changed on 1 November. ".repeat(20);
        let html=format!("<title>AI compliance</title><nav>irrelevant</nav><article><h1>Rules</h1><p>{text}</p><script>untrusted()</script><div class='cookie'>accept cookies</div><footer>Footer</footer></article>");
        let source = extract(
            &c,
            Url::parse("https://example.org/final").unwrap(),
            html.as_bytes(),
            "AI compliance",
            None,
        )
        .unwrap();
        assert!(source.text.contains(&text.trim()));
        for unwanted in ["irrelevant", "untrusted", "accept cookies", "Footer"] {
            assert!(!source.text.contains(unwanted));
        }
        assert_eq!(source.url, c.url);
        assert_eq!(source.final_url, "https://example.org/final");
        assert_eq!(source.search_rank, 3);
        assert_eq!(
            source.text_sha256,
            hex::encode(Sha256::digest(source.text.as_bytes()))
        );
        assert!(relevant(
            "What are the latest AI compliance developments?",
            &source
        ));
        assert!(!relevant("sailing boats", &source));
        assert!(extract(
            &c,
            Url::parse(&c.url).unwrap(),
            format!("<title>Just a moment</title><p>{text}</p>").as_bytes(), "AI compliance", None
        )
        .is_err());
        assert!(extract(
            &c,
            Url::parse(&c.url).unwrap(),
            format!("<meta name='robots' content='nosnippet'><p>{text}</p>").as_bytes(), "AI compliance", None
        )
        .is_err());
    }
    #[test]
    fn citations_only_reference_read_sources_and_duplicate_urls_collapse() {
        assert!(validate_answer(
            &json!({"blocks":[{"text":"A finding","citations":[1,5]}],"limitations":[]}),
            5
        )
        .is_ok());
        for ids in [json!([0]), json!([6]), json!([])] {
            assert!(validate_answer(
                &json!({"blocks":[{"text":"Claim","citations":ids}],"limitations":[]}),
                5
            )
            .is_err());
        }
        assert_eq!(
            url_key("http://www.example.org/a/?utm_source=ad#top"),
            url_key("https://example.org/a")
        );
        assert!(url_key("https://127.0.0.1/admin").is_none());
    }
}
