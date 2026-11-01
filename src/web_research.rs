//! Five-page evidence research. Search, extraction, processing and synthesis
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

pub(crate) const TARGET: usize = 5;
const MAX_CANDIDATES: usize = 15;
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

pub(crate) async fn search(question: &str) -> Result<(Vec<Candidate>, Value)> {
    let data = crate::web_search::research_candidates(&json!({"query":question})).await?;
    let mut seen = HashSet::new();
    let candidates = data["results"]
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
                rank: rank + 1,
            })
        })
        .take(MAX_CANDIDATES)
        .collect();
    Ok((candidates, data["providers"].clone()))
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
            | "header"
            | "footer"
            | "aside"
            | "form"
            | "button"
            | "noscript"
            | "svg"
            | "template"
    ) || e.attr("hidden").is_some()
        || e.attr("aria-hidden") == Some("true")
        || e.attr("role")
            .is_some_and(|r| matches!(r, "navigation" | "banner" | "contentinfo" | "dialog"))
        || [e.attr("id"), e.attr("class")]
            .into_iter()
            .flatten()
            .any(|v| {
                let v = v.to_ascii_lowercase();
                ["cookie", "consent", "advert", "sidebar", "paywall"]
                    .iter()
                    .any(|word| v.contains(word))
            })
}
fn readable(root: ElementRef<'_>) -> String {
    let parts = root
        .descendants()
        .filter_map(|node| {
            let text = node.value().as_text()?;
            if node.ancestors().filter_map(ElementRef::wrap).any(excluded) {
                return None;
            }
            Some(text.text.as_ref())
        })
        .collect::<Vec<_>>();
    normalized(&parts.join(" "), TEXT_CHARS + 1)
}
fn extract(candidate: &Candidate, final_url: Url, bytes: &[u8]) -> Result<Source> {
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
    let content = html
        .select(&roots)
        .map(readable)
        .max_by_key(String::len)
        .unwrap_or_else(|| readable(html.root_element()));
    ensure!(content.chars().count() >= 200, "insufficient readable text");
    let beginning = format!(
        "{} {}",
        title,
        content.chars().take(200).collect::<String>()
    )
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
    let trimmed = content.chars().count() > TEXT_CHARS;
    let text: String = content.chars().take(TEXT_CHARS).collect();
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

/// Each batch reads only as many pages as still needed, concurrently. Failed,
/// duplicate and irrelevant pages leave room for lower ranked replacements.
pub(crate) async fn read(candidates: Vec<Candidate>, question: &str) -> (Vec<Source>, Vec<Value>) {
    read_with(candidates, question, |candidate| async move {
        let (url, bytes) = crate::link_preview::Service::shared()
            .research_html(&candidate.url)
            .await?;
        extract(&candidate, url, &bytes)
    })
    .await
}

async fn read_with<F, Fut>(
    candidates: Vec<Candidate>,
    question: &str,
    fetch: F,
) -> (Vec<Source>, Vec<Value>)
where
    F: Fn(Candidate) -> Fut,
    Fut: std::future::Future<Output = Result<Source>>,
{
    let mut sources = Vec::new();
    let mut failures = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(26);
    let mut pending = candidates.into_iter();
    let mut seen = HashSet::new();
    let mut hashes = HashSet::new();
    loop {
        let batch = pending
            .by_ref()
            .take(TARGET - sources.len())
            .collect::<Vec<_>>();
        if batch.is_empty() {
            break;
        }
        let reads = stream::iter(batch.into_iter().map(|candidate| {
            let fetch = &fetch;
            async move {
                let started = Instant::now();
                let result = fetch(candidate.clone()).await;
                (candidate, result, started.elapsed().as_millis() as u64)
            }
        }))
        .buffered(TARGET)
        .collect::<Vec<_>>();
        let Ok(reads) = tokio::time::timeout_at(deadline, reads).await else {
            break;
        };
        for (candidate, result, ms) in reads {
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
        if sources.len() == TARGET {
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
    let answer: Answer = serde_json::from_value(value.clone())?;
    ensure!(
        answer.blocks.len() <= 12 && answer.limitations.len() <= 5,
        "answer too large"
    );
    for block in &answer.blocks {
        ensure!(
            !block.text.trim().is_empty()
                && block.text.chars().count() <= 1000
                && block
                    .heading
                    .as_ref()
                    .is_none_or(|h| h.chars().count() <= 120)
                && !block.citations.is_empty()
                && block.citations.len() <= TARGET
                && block.citations.iter().all(|id| (1..=count).contains(id)),
            "invalid cited paragraph"
        );
    }
    ensure!(
        answer
            .limitations
            .iter()
            .all(|s| !s.trim().is_empty() && s.chars().count() <= 500),
        "invalid limitation"
    );
    ensure!(
        !answer.blocks.is_empty() || !answer.limitations.is_empty(),
        "empty answer"
    );
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
        let candidates = (1..=12)
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
            vec![1, 5, 6, 7, 8]
        );
        assert_eq!(
            sources.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5]
        );
        assert_eq!(failures.len(), 3);
        assert_eq!(calls.load(Ordering::SeqCst), 8);
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
    async fn one_synthesis_call_receives_original_question_and_all_five_pages() {
        use axum::{routing::post, Json, Router};
        use std::sync::{Arc, Mutex};
        let received = Arc::new(Mutex::new(Vec::<Value>::new()));
        let capture = received.clone();
        let app=Router::new().route("/",post(move |Json(body):Json<Value>| {
            capture.lock().unwrap().push(body);
            async {Json(json!({"choices":[{"message":{"content":json!({"blocks":[{"heading":"Developments","text":"One consolidated finding","citations":[1,2,3,4,5]}],"limitations":[]}).to_string()}}],"usage":{"prompt_tokens":100,"completion_tokens":20,"cost":0.001}}))}
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
        let input = json!({"question":"What are the latest developments in AI compliance?","sources":(1..=5).map(|id|json!({"id":id,"text":format!("Evidence {id}")})).collect::<Vec<_>>()});
        let reply = client.synthesize_web(input.clone()).await.unwrap();
        assert_eq!(validate_answer(&reply, 5).unwrap().blocks.len(), 1);
        assert_eq!(reply["usage"]["cost"], 0.001);
        let calls = received.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["model"], crate::parser::MULTISOURCE_ANALYSIS_MODEL);
        let sent: Value =
            serde_json::from_str(calls[0]["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(sent, input);
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
            format!("<title>Just a moment</title><p>{text}</p>").as_bytes()
        )
        .is_err());
        assert!(extract(
            &c,
            Url::parse(&c.url).unwrap(),
            format!("<meta name='robots' content='nosnippet'><p>{text}</p>").as_bytes()
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
