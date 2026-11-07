//! Read the fastest discovery results immediately. Source IDs are assigned
//! once, when evidence is accepted, and never change during synthesis.
use super::*;
use futures_util::Stream;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

pub(crate) struct Evidence {
    pub sources: Vec<Source>,
    pub failures: Vec<Value>,
    pub providers: Value,
    pub search: Value,
    pub elapsed_ms: u64,
}

pub(crate) async fn retrieve<P, PF>(query: &str, question: &str, subject: Option<&str>, exa_only: bool, publish: P) -> Result<Evidence>
where
    P: Fn(Vec<Source>) -> PF,
    PF: std::future::Future<Output = Result<()>>,
{
    let start = Instant::now();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(32);
    let (sender, receiver) = mpsc::channel(4);
    let discovery = async {
        let _ = sender.send(question_links(question)).await;
        let result = tokio::time::timeout(Duration::from_secs(20), async {
            if exa_only {
                let result = exa::only(query).await;
                if let Ok((rows, _, _)) = &result { let _ = sender.send(rows.clone()).await; }
                result
            } else {
                exa::supplement_streaming(query, Some(&sender)).await
            }
        }).await;
        drop(sender);
        let mut result = result.unwrap_or_else(|_| Err(anyhow::anyhow!("search timeout")));
        if let Ok((_, _, operation)) = &mut result { operation["elapsed_ms"] = json!(start.elapsed().as_millis() as u64); }
        result
    };
    let evidence_question = format!("{question}\n{query}");
    let reads = read(ReceiverStream::new(receiver).flat_map(stream::iter), &evidence_question,
        deadline, Vec::new(), &publish, |candidate| fetch(candidate, &evidence_question, subject));
    let (discovery, reads) = tokio::join!(discovery, reads);
    let (mut sources, mut failures, mut attempted) = reads?;
    let (_, mut providers, mut search) = match discovery {
        Ok(result) => result,
        Err(error) if sources.is_empty() || exa_only => return Err(error),
        Err(_) => (vec![], json!([]), json!({"status":"unavailable","cost_usd":null,"configured_api_requests":null})),
    };
    // Replacements share the original deadline and retain every published ID.
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if sources.len() < TARGET && attempted.len() < MAX_CANDIDATES && remaining >= Duration::from_secs(2) {
        if let Ok(backup) = crate::web_search::research_candidates(&json!({"query":query}), remaining.min(Duration::from_secs(8))).await {
            if let (Some(providers), Some(extra)) = (providers.as_array_mut(), backup["providers"].as_array()) {
                for provider in extra { if !providers.contains(provider) { providers.push(provider.clone()); } }
            }
            let offset = attempted.len();
            let replacements = backup["results"].as_array().into_iter().flatten().filter_map(|row| {
                let url = row["url"].as_str()?;
                if !attempted.insert(url_key(url)?) { return None; }
                Some(Candidate { url: url.into(), title: normalized(row["title"].as_str().unwrap_or(url), 300), rank: 0 })
            }).take(MAX_CANDIDATES - offset).collect::<Vec<_>>();
            let (updated, skipped, _) = read(stream::iter(replacements), &evidence_question, deadline, sources,
                &publish, |candidate| fetch(candidate, &evidence_question, subject)).await?;
            sources = updated;
            failures.extend(skipped);
            search["replacement_search"] = json!({"attempted":true,"providers":backup["providers"]});
        }
    }
    let elapsed_ms = start.elapsed().as_millis() as u64;
    search["overlapped_with_extraction"] = json!(true);
    Ok(Evidence { sources, failures, providers, search, elapsed_ms })
}

async fn fetch(candidate: Candidate, question: &str, subject: Option<&str>) -> Result<Source> {
    let (url, bytes) = crate::link_preview::Service::shared().research_html(&candidate.url).await?;
    let source = extract(&candidate, url, &bytes, question, None)?;
    ensure!(subject.is_none_or(|name| mentions_subject(&source.text, name)), "page does not identify requested company");
    Ok(source)
}

fn mentions_subject(text: &str, subject: &str) -> bool {
    let words = |text: &str| text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect::<Vec<_>>();
    let name = words(subject);
    !name.is_empty() && words(text).windows(name.len()).any(|window| window == name)
}

async fn read<S, P, PF, F, FF>(candidates: S, question: &str, deadline: tokio::time::Instant,
    mut sources: Vec<Source>, publish: &P, fetch: F) -> Result<(Vec<Source>, Vec<Value>, HashSet<String>)>
where
    S: Stream<Item = Candidate>,
    P: Fn(Vec<Source>) -> PF,
    PF: std::future::Future<Output = Result<()>>,
    F: Fn(Candidate) -> FF,
    FF: std::future::Future<Output = Result<Source>>,
{
    let mut attempted = HashSet::new();
    let mut seen: HashSet<_> = sources.iter().filter_map(|s| url_key(&s.final_url)).collect();
    let mut hashes: HashSet<_> = sources.iter().map(|s| s.text_sha256.clone()).collect();
    let offset = sources.iter().map(|s| s.search_rank).max().unwrap_or(0);
    let mut failures = Vec::new();
    {
        let reads = candidates.filter_map(|candidate| {
            let accepted = url_key(&candidate.url).is_some_and(|key| attempted.insert(key));
            std::future::ready(accepted.then_some(candidate))
        }).take(MAX_CANDIDATES).enumerate().map(|(i, mut candidate)| {
            candidate.rank = offset + i + 1;
            let fetch = &fetch;
            async move {
                let result = tokio::time::timeout(Duration::from_secs(8), fetch(candidate.clone())).await
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("page timeout")));
                (candidate, result)
            }
        }).buffer_unordered(READ_CONCURRENCY);
        futures_util::pin_mut!(reads);
        while sources.len() < TARGET {
            let Ok(Some((candidate, result))) = tokio::time::timeout_at(deadline, reads.next()).await else { break; };
            match result {
                Ok(mut source) if relevant(question, &source)
                    && !seen.contains(&url_key(&source.final_url).unwrap_or_default())
                    && !hashes.contains(&source.text_sha256) => {
                    seen.insert(url_key(&source.final_url).unwrap_or_default());
                    hashes.insert(source.text_sha256.clone());
                    source.id = sources.len() + 1;
                    sources.push(source);
                    publish(sources.clone()).await?;
                }
                Ok(_) => failures.push(json!({"url":candidate.url,"rank":candidate.rank,"reason":"duplicate_or_irrelevant"})),
                Err(_) => failures.push(json!({"url":candidate.url,"rank":candidate.rank,"reason":"unavailable"})),
            }
        }
    }
    Ok((sources, failures, attempted))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn compliance_evidence_requires_the_named_company_in_the_page() {
        assert!(!mentions_subject("Wwft guidance covers sanctions, PEPs and UBO checks.", "Mollie"));
        assert!(!mentions_subject("Mollies unrelated business", "Mollie"));
        assert!(mentions_subject("Registered company: MOLLIE B.V. in Amsterdam", "Mollie"));
        assert!(mentions_subject("Example-Pay B.V. registration", "Example Pay"));
    }

    #[tokio::test]
    async fn fast_pages_stream_while_a_provider_and_an_earlier_page_are_pending() {
        let (sender, receiver) = mpsc::channel(4);
        let candidate = |rank| Candidate { title: "AI compliance".into(), url: format!("https://example.org/{rank}"), rank };
        sender.send(vec![candidate(1), candidate(2)]).await.unwrap();
        let updates = Arc::new(Mutex::new(Vec::<Vec<Source>>::new()));
        let captured = updates.clone();
        let task = tokio::spawn(async move {
            read(ReceiverStream::new(receiver).flat_map(stream::iter), "AI compliance",
                tokio::time::Instant::now() + Duration::from_secs(3), vec![], &move |sources| {
                    captured.lock().unwrap().push(sources);
                    std::future::ready(Ok(()))
                }, |c| async move {
                    if c.url.ends_with("/1") { std::future::pending::<()>().await; }
                    Ok(Source { id: 0, title: c.title, url: c.url.clone(), final_url: c.url.clone(), search_rank: c.rank,
                        retrieved_at: "2026-11-05T00:00:00Z".into(), text: "AI compliance evidence".into(),
                        text_sha256: c.url, trimmed: false })
                }).await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while updates.lock().unwrap().is_empty() { tokio::task::yield_now().await; }
        }).await.expect("ready evidence must not wait for a stalled page or provider");
        assert_eq!(updates.lock().unwrap()[0][0].id, 1);
        assert!(updates.lock().unwrap()[0][0].url.ends_with("/2"));
        sender.send(vec![candidate(3)]).await.unwrap();
        drop(sender);
        let (sources, _, _) = task.await.unwrap().unwrap();
        let updates = updates.lock().unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(updates.len(), 2);
        assert_eq!(sources[0].url, updates[0][0].url);
        assert_eq!(sources[1].id, 2);
    }
}
