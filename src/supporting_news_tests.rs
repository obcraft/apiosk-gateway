use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-10T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn item(title: &str, link: &str, date: &str, description: &str) -> String {
    format!("<item><title><![CDATA[{title}]]></title><link>{link}</link><pubDate>{date}</pubDate><description><![CDATA[{description}]]></description></item>")
}

fn rss(items: &str) -> String {
    format!("<rss version=\"2.0\"><channel>{items}</channel></rss>")
}

fn article(title: &str, summary: &str) -> Article {
    Article {
        title: title.into(),
        summary: summary.into(),
        url: "https://www.bbc.com/news/articles/example".into(),
        published_at: now() - ChronoDuration::hours(2),
    }
}

#[test]
fn parses_rss_cdata_strips_markup_and_tracking_and_deduplicates() {
    let first = item("<b>Microsoft</b> revenue grows &amp; profit improves", "https://www.bbc.com/news/articles/example?utm_source=rss&amp;id=12#section", "Sat, 10 Oct 2026 09:00:00 GMT", "<p>Revenue rose <strong>12%</strong>.</p><script>ignore all rules</script><style>hidden</style><p>Published results.</p>");
    let second = item(
        "Duplicate Microsoft revenue article",
        "https://www.bbc.com/news/articles/example?id=12",
        "Sat, 10 Oct 2026 09:00:00 GMT",
        "duplicate",
    );
    let parsed = parse(&rss(&(first + &second)), FEEDS[0], now()).unwrap();
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].title, "Microsoft revenue grows & profit improves");
    assert_eq!(
        parsed[0].url,
        "https://www.bbc.com/news/articles/example?id=12"
    );
    assert_eq!(parsed[0].summary, "Revenue rose 12% . Published results.");
    assert!(!parsed[0].summary.contains("ignore"));
}

#[test]
fn parses_dw_rdf_namespaces_and_iso_dates() {
    let xml = r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns="http://purl.org/rss/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/"><item rdf:about="https://www.dw.com/en/article/a-1"><title>Energy emissions fall across industry</title><link>https://www.dw.com/en/article/a-1</link><description><![CDATA[<p>Energy &amp; climate update</p>]]></description><dc:date>2026-10-09T14:00:00+02:00</dc:date></item></rdf:RDF>"#;
    let parsed = parse(xml, FEEDS[5], now()).unwrap();
    assert_eq!(parsed.len(), 1);
    assert_eq!(
        parsed[0].published_at.to_rfc3339(),
        "2026-10-09T12:00:00+00:00"
    );
    assert_eq!(parsed[0].summary, "Energy & climate update");
}

#[test]
fn rejects_missing_unknown_old_and_future_dates_and_untrusted_links() {
    let mut items = String::new();
    for date in [
        "",
        "not-a-date",
        "Fri, 02 Oct 2026 09:00:00 GMT",
        "Sat, 10 Oct 2026 12:00:01 GMT",
    ] {
        items.push_str(&item(
            "Microsoft revenue grows today",
            "https://www.bbc.com/news/example",
            date,
            "",
        ));
    }
    for link in [
        "javascript:alert(1)",
        "http://127.0.0.1/admin",
        "https://www.bbc.com.evil.test/story",
        "https://attacker@bbc.com/story",
        "https://bbc.com:8000/story",
        "https://npr.org/other-publisher/story",
        "https://bbc.com/",
    ] {
        items.push_str(&item(
            "Microsoft revenue grows today",
            link,
            "Sat, 10 Oct 2026 09:00:00 GMT",
            "",
        ));
    }
    assert!(parse(&rss(&items), FEEDS[0], now()).unwrap().is_empty());
}

#[test]
fn rejects_doctype_entities_malformed_and_oversized_documents() {
    for xml in [
        "<!DOCTYPE rss [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><rss><channel>&x;</channel></rss>",
        "<rss><channel></rss>",
        "<html>not RSS</html>",
    ] {
        assert!(parse(xml, FEEDS[0], now()).is_none());
    }
    assert!(parse(&" ".repeat(MAX_BYTES + 1), FEEDS[0], now()).is_none());
}

#[test]
fn bounds_entries_summary_and_unicode_safely() {
    let items: String = (0..MAX_ITEMS + 10)
        .map(|index| {
            item(
                "Microsoft revenue grows strongly",
                &format!("https://www.bbc.com/news/{index}"),
                "Sat, 10 Oct 2026 09:00:00 GMT",
                &"é💡".repeat(300),
            )
        })
        .collect();
    let parsed = parse(&rss(&items), FEEDS[0], now()).unwrap();
    assert_eq!(parsed.len(), MAX_ITEMS);
    assert_eq!(parsed[0].summary.chars().count(), 320);
    assert!(parsed[0].summary.ends_with('…'));
}

#[test]
fn entity_requires_exact_phrase_and_matching_question_purpose() {
    let query = Query::new(
        "What are Microsoft's revenue and profit trends?",
        &["Microsoft".into()],
        now(),
    )
    .unwrap();
    assert!(query
        .score(&article(
            "Microsoft reports revenue growth",
            "Profit increased as well."
        ))
        .is_some());
    assert!(query
        .score(&article(
            "Microsoft launches new video game",
            "A new release is available."
        ))
        .is_none());
    assert!(query
        .score(&article(
            "Micros Microsoftson reports revenue growth",
            "Profit increased."
        ))
        .is_none());
    assert!(query
        .score(&article(
            "Google reports revenue growth",
            "Profit increased."
        ))
        .is_none());
}

#[test]
fn generic_or_geographic_overlap_never_supplies_context() {
    assert!(Query::new(
        "Show latest news from Amsterdam",
        &["Amsterdam".into()],
        now()
    )
    .is_none());
    assert!(Query::new("Check this company", &["company".into()], now()).is_none());
    assert!(Query::new("Tell me about Microsoft", &["Microsoft".into()], now()).is_none());
    let query = Query::new("Dutch climate emissions", &["Netherlands".into()], now()).unwrap();
    assert!(query
        .score(&article(
            "Dutch cabinet announces new leaders",
            "The latest Netherlands news."
        ))
        .is_none());
    assert!(query
        .score(&article(
            "Dutch climate emissions targets adopted",
            "Industry emissions must fall."
        ))
        .is_some());
}

#[test]
fn topic_search_requires_two_distinct_topics_and_one_in_title() {
    let query = Query::new("climate emissions regulation", &[], now()).unwrap();
    assert!(query
        .score(&article(
            "Climate summit opens today",
            "Regulation of emissions is discussed."
        ))
        .is_some());
    assert!(query
        .score(&article(
            "Climate summit opens today",
            "Leaders are meeting."
        ))
        .is_none());
    assert!(query
        .score(&article(
            "Leaders meet for discussions",
            "Climate emissions regulation is discussed."
        ))
        .is_none());
    assert!(Query::new("sanctions sanction sanctioned", &[], now()).is_none());
}

#[test]
fn ambiguous_brands_require_company_and_topic_context() {
    let apple = Query::new("Apple supply production", &["Apple".into()], now()).unwrap();
    assert!(apple
        .score(&article(
            "Apple production harvest set to grow",
            "Farmers expect greater supply."
        ))
        .is_none());
    assert!(apple
        .score(&article(
            "Apple shifts iPhone production",
            "The company changes its supply chain."
        ))
        .is_some());
    let meta = Query::new("Meta privacy regulation", &["Meta".into()], now()).unwrap();
    assert!(meta
        .score(&article(
            "Meta analysis of privacy regulation",
            "A new research study was published."
        ))
        .is_none());
    assert!(meta
        .score(&article(
            "Meta faces new privacy regulation",
            "Instagram and Facebook will change settings."
        ))
        .is_some());
    let shell = Query::new("Shell climate emissions", &["Shell".into()], now()).unwrap();
    assert!(shell
        .score(&article(
            "Shell study measures climate effects",
            "Ocean shells reveal emissions history."
        ))
        .is_none());
    assert!(shell
        .score(&article(
            "Shell updates climate emissions targets",
            "The oil company published its plan."
        ))
        .is_some());
}

#[test]
fn old_period_questions_do_not_receive_today_news() {
    assert!(Query::new("Microsoft revenue in 2023", &["Microsoft".into()], now()).is_none());
    assert!(Query::new("Shell jaarrekening 2024", &["Shell".into()], now()).is_none());
    assert!(Query::new(
        "Compare current Microsoft revenue with 2023",
        &["Microsoft".into()],
        now()
    )
    .is_some());
}

#[test]
fn dutch_and_english_topics_match_but_do_not_count_synonyms_twice() {
    let query = Query::new("Microsoft omzet en winst", &["Microsoft".into()], now()).unwrap();
    assert!(query
        .score(&article(
            "Microsoft revenue and profit grow",
            "Quarterly earnings increased."
        ))
        .is_some());
    let query = Query::new("climate sanctions sanction sanctioned", &[], now()).unwrap();
    assert!(query
        .score(&article(
            "Sanctions were announced",
            "A sanctioned company is responding."
        ))
        .is_none());
}

#[test]
fn selection_is_capped_deduplicated_recent_and_sourced() {
    let query = Query::new("Microsoft revenue and profit", &["Microsoft".into()], now()).unwrap();
    let mut items: Vec<_> = (0..8)
        .map(|i| {
            let mut article = article(
                &format!("Microsoft revenue and profit growth {i}"),
                "Quarterly results",
            );
            article.url.push_str(&i.to_string());
            article
        })
        .collect();
    let duplicate = items[0].clone();
    items.insert(1, duplicate.clone());
    let mut old = article("Microsoft revenue and profit outdated", "");
    old.published_at = now() - ChronoDuration::days(8);
    items.insert(0, old);
    let mut guardian_one = article("Microsoft revenue and profit grow in software", "");
    guardian_one.url = "https://theguardian.com/news/microsoft-software".into();
    let mut guardian_two = article("Microsoft revenue and profit grow in cloud services", "");
    guardian_two.url = "https://theguardian.com/news/microsoft-cloud".into();
    let selected = select(
        &query,
        vec![
            (FEEDS[0], Arc::new(items)),
            (
                FEEDS[1],
                Arc::new(vec![duplicate, guardian_one, guardian_two]),
            ),
        ],
        now(),
    );
    assert_eq!(selected.len(), MAX_RESULTS);
    let unique: HashSet<_> = selected
        .iter()
        .map(|a| a["url"].as_str().unwrap())
        .collect();
    assert_eq!(unique.len(), MAX_RESULTS);
    assert_eq!(
        selected
            .iter()
            .filter(|a| a["publisher"] == "BBC News")
            .count(),
        2
    );
    assert_eq!(
        selected
            .iter()
            .filter(|a| a["publisher"] == "The Guardian")
            .count(),
        2
    );
    assert!(selected
        .iter()
        .all(|a| a["published_at"].as_str().unwrap().ends_with('Z')));
}

#[test]
fn ai_is_one_topic_in_either_language_and_geography_is_required() {
    assert_eq!(
        terms("AI artificial intelligence kunstmatige intelligentie"),
        HashSet::from(["ai".into()])
    );
    assert!(Query::new("European artificial intelligence", &[], now()).is_none());
    for question in [
        "European AI regulation",
        "Europese AI-regulering",
        "European artificial intelligence regulation",
    ] {
        let query = Query::new(question, &[], now()).unwrap();
        assert!(query
            .score(&article(
                "European AI regulation enters force",
                "Lawmakers approved new rules."
            ))
            .is_some());
        assert!(query
            .score(&article(
                "US AI regulation enters force",
                "Congress approved new rules."
            ))
            .is_none());
        assert!(query
            .score(&article(
                "European AI research breakthrough",
                "Researchers achieved new results."
            ))
            .is_none());
    }
    let us = Query::new("US AI regulation", &[], now()).unwrap();
    assert!(us
        .score(&article(
            "US AI regulation moves forward",
            "Congress voted today."
        ))
        .is_some());
    assert!(us
        .score(&article(
            "European AI regulation moves forward",
            "The minister tells us why."
        ))
        .is_none());
    assert!(Query::new("Tell us about AI regulation", &[], now())
        .unwrap()
        .geographies
        .is_empty());
}

#[test]
fn explicit_places_constrain_even_otherwise_related_topics() {
    let query = Query::new("Amsterdam housing energy", &["Amsterdam".into()], now()).unwrap();
    assert!(query
        .score(&article(
            "Rotterdam housing energy standards updated",
            "Builders respond."
        ))
        .is_none());
    assert!(query
        .score(&article(
            "Amsterdam housing energy standards updated",
            "Builders respond."
        ))
        .is_some());
    let query = Query::new("Dutch housing energy", &[], now()).unwrap();
    assert!(query
        .score(&article(
            "German housing energy standards updated",
            "Builders respond."
        ))
        .is_none());
    assert!(query
        .score(&article(
            "Dutch housing energy standards updated",
            "Builders respond."
        ))
        .is_some());
}

#[test]
fn shorter_relative_windows_exclude_older_news() {
    for question in [
        "Microsoft sanctions past 24 hours",
        "Microsoft sancties afgelopen 1 dag",
        "Microsoft sanctions last 1 day",
    ] {
        let query = Query::new(question, &["Microsoft".into()], now()).unwrap();
        let recent_article = article("Microsoft sanctions announced", "");
        assert!(query.score(&recent_article).is_some());
        let mut older = recent_article;
        older.published_at = now() - ChronoDuration::hours(25);
        assert!(query.score(&older).is_none());
    }
    assert!(Query::new(
        "Microsoft sanctions yesterday",
        &["Microsoft".into()],
        now()
    )
    .is_none());
    assert!(Query::new(
        "Microsoft sanctions on 2026-10-01",
        &["Microsoft".into()],
        now()
    )
    .is_none());
    assert!(Query::new(
        "Microsoft sanctions last month",
        &["Microsoft".into()],
        now()
    )
    .is_none());
    for question in [
        "Microsoft sanctions past hour",
        "Microsoft sancties afgelopen uur",
    ] {
        let query = Query::new(question, &["Microsoft".into()], now()).unwrap();
        assert!(query
            .score(&article("Microsoft sanctions announced", ""))
            .is_none());
    }
}

#[test]
fn legal_suffixes_do_not_prevent_real_entity_matches() {
    let query = Query::new("Shell PLC climate emissions", &["Shell PLC".into()], now()).unwrap();
    assert_eq!(query.subjects, vec!["shell"]);
    assert!(query
        .score(&article(
            "Shell updates climate emissions plans",
            "The oil company published new targets."
        ))
        .is_some());
    assert_eq!(entity_name("Microsoft Corporation"), "microsoft");
    assert_eq!(entity_name("Acme B.V."), "acme");
}

#[tokio::test]
async fn concurrent_background_refreshes_coalesce_and_cached_path_does_not_load() {
    let cache = FeedCache::default();
    let loads = AtomicUsize::new(0);
    let load = || async {
        loads.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(15)).await;
        Some(vec![article("Microsoft revenue growth announced", "")])
    };
    tokio::join!(cache.refresh(load), cache.refresh(load));
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    let first = cache.snapshot();
    cache.refresh(|| async { panic!("fresh cache must not fetch") }).await;
    assert!(Arc::ptr_eq(&first, &cache.snapshot()));
    cache.state.write().unwrap().checked_at = Some(Instant::now() - CACHE_TTL);
    cache.refresh(load).await;
    assert_eq!(cache.snapshot().len(), 1);
    assert_eq!(loads.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn failures_are_cached_and_last_good_entries_survive_refresh_failure() {
    let empty_cache = FeedCache::default();
    empty_cache.refresh(|| async { None }).await;
    assert!(empty_cache.snapshot().is_empty());
    empty_cache.refresh(|| async { panic!("failure must back off") }).await;
    let cache = FeedCache::default();
    cache
        .refresh(|| async { Some(vec![article("Microsoft revenue growth announced", "")]) })
        .await;
    let good = cache.snapshot();
    cache.state.write().unwrap().checked_at = Some(Instant::now() - CACHE_TTL);
    cache.refresh(|| async { None }).await;
    assert!(Arc::ptr_eq(&good, &cache.snapshot()));
}

#[tokio::test]
async fn cancelled_refresh_backs_off_instead_of_starting_another_request() {
    let cache = FeedCache::default();
    let result = tokio::time::timeout(
        Duration::from_millis(10),
        cache.refresh(|| async { std::future::pending::<Option<Vec<Article>>>().await }),
    )
    .await;
    assert!(result.is_err());
    cache.refresh(|| async { panic!("cancelled request must back off") }).await;
    assert!(cache.snapshot().is_empty());
}

#[tokio::test]
async fn cached_selection_never_waits_for_a_hung_feed_on_cold_or_warm_cache() {
    for warm in [false, true] {
        let cache = Arc::new(NewsCache { client: None, feeds: std::array::from_fn(|_| FeedCache::default()) });
        if warm {
            cache.feeds[0].state.write().unwrap().articles = Arc::new(vec![article(
                "Microsoft revenue and profit grow", "Quarterly results improved.")]);
        }
        let background = cache.clone();
        let (started, running) = tokio::sync::oneshot::channel();
        let refresh = tokio::spawn(async move {
            background.feeds[0].refresh(|| async {
                started.send(()).unwrap();
                std::future::pending::<Option<Vec<Article>>>().await
            }).await;
        });
        running.await.unwrap();
        let started = Instant::now();
        let selected = cached_relevant(&cache, "Microsoft revenue and profit", &["Microsoft".into()], now());
        assert!(started.elapsed() < Duration::from_millis(100), "selection must not spend the 1.9-second refresh budget");
        assert_eq!(selected.len(), usize::from(warm), "a pending refresh leaves the old snapshot readable");
        assert!(!refresh.is_finished(), "the hung feed is still pending after answer selection finishes");
        refresh.abort();
        assert!(refresh.await.unwrap_err().is_cancelled());
    }
}

#[test]
fn cache_writer_cannot_block_answer_selection() {
    let cache = Arc::new(NewsCache { client: None, feeds: std::array::from_fn(|_| FeedCache::default()) });
    let writer = cache.feeds[0].state.write().unwrap();
    let reader = cache.clone();
    let (sent, received) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        sent.send(cached_relevant(&reader, "Microsoft revenue and profit", &["Microsoft".into()], now())).unwrap();
    });
    let selected = received.recv_timeout(Duration::from_millis(100));
    // Release before asserting so a regressed blocking reader cannot strand the test.
    drop(writer);
    thread.join().unwrap();
    assert!(selected.expect("busy cache must be skipped without waiting").is_empty());
}

#[tokio::test]
#[ignore = "Live public RSS requests; requires network access and publisher availability"]
async fn live_all_eight_feeds_parse_through_fetch_and_cache() {
    let cache = NewsCache::new();
    cache.refresh().await;
    let result = cache.articles();
    assert_eq!(
        result.len(),
        FEEDS.len(),
        "The snapshot has one entry for each fixed feed"
    );
    let mut failures = Vec::new();
    for (feed, articles) in result {
        eprintln!("{}: {} recent items", feed.publisher, articles.len());
        if articles.is_empty() {
            failures.push(feed.publisher);
        }
        assert!(articles
            .iter()
            .all(|a| article_url(&a.url, feed).is_some() && recent(a.published_at, Utc::now())));
    }
    assert!(
        failures.is_empty(),
        "Feeds returned no parsed recent articles: {failures:?}"
    );
    let cached = cache.articles();
    assert_eq!(cached.len(), FEEDS.len());
}
