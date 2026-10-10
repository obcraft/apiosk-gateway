use super::*;

fn instant(value: &str) -> DateTime<Utc> { DateTime::parse_from_rfc3339(value).unwrap().with_timezone(&Utc) }

#[test]
fn classifier_matches_shared_app_cases() {
    let cases: Value = serde_json::from_str(include_str!("../contracts/standalone-news-cases.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let result = classify_news(case["question"].as_str().unwrap());
        assert_eq!(result.as_ref().map(|q| q.query.as_str()), case["query"].as_str(), "{}", case["question"]);
        assert_eq!(result.as_ref().map(|q| if q.today { "today" } else { "recent" }), case["window"].as_str(), "{}", case["question"]);
    }
}

#[test]
fn unicode_topics_are_preserved_and_empty_or_malformed_topics_never_search_everything() {
    for topic in ["économie", "São Paulo", "Oekraïne", "Energie-transitie", "日本経済"] {
        let question = format!("Nieuws over: {topic}?");
        assert_eq!(classify_news(&question).unwrap().query, topic);
    }
    for question in ["Nieuws over: ---", "News about: …", "News about the and", "éNews about Sinterklaas",
        "Nieuws over: Sinterklaas: stuur", "Nieuws over Sinterklaas\u{0000}", "News about Sinterklaas? More?"] {
        assert!(classify_news(question).is_none(), "{question:?}");
    }
    let now = instant("2026-10-10T12:00:00Z");
    assert_eq!(response(&cache(vec![], now), &NewsQuery { query: "---".into(), today: false }, now), json!({"handled":false}));
}

#[test]
fn today_uses_amsterdam_midnight_and_dst_calendar_days() {
    let query = NewsQuery { query: "Sinterklaas".into(), today: true };
    for (now, start, end) in [
        ("2026-07-01T10:00:00Z", "2026-06-30T22:00:00Z", "2026-07-01T22:00:00Z"),
        ("2026-01-15T10:00:00Z", "2026-01-14T23:00:00Z", "2026-01-15T23:00:00Z"),
        ("2026-03-29T12:00:00Z", "2026-03-28T23:00:00Z", "2026-03-29T22:00:00Z"),
        ("2026-10-25T12:00:00Z", "2026-10-24T22:00:00Z", "2026-10-25T23:00:00Z"),
    ] {
        assert_eq!(bounds(&query, instant(now)), Some((instant(start), instant(end))));
    }
}

fn cache(items: Vec<Article>, now: DateTime<Utc>) -> NewsCache {
    let cache = NewsCache { client: None, feeds: std::array::from_fn(|_| FeedCache::default()) };
    *cache.feeds[7].state.write().unwrap() = CachedFeed { checked_at: Some(Instant::now()), refreshed_at: Some(now), articles: Arc::new(items) };
    cache
}

#[test]
fn one_explicit_topic_returns_articles_but_supplement_matching_stays_conservative() {
    let now = instant("2026-10-10T00:30:00Z");
    let query = classify_news("Wat wordt er vandaag in het nieuws gezegd over Sinterklaas?").unwrap();
    assert!(Query::new("Sinterklaas", &[], now).is_none());
    let articles = [
        ("Local midnight Sinterklaas news", "2026-10-09T22:00:00Z"),
        ("Yesterday Sinterklaas story", "2026-10-09T21:59:59Z"),
        ("Future Sinterklaas story", "2026-10-10T01:00:00Z"),
        ("Tomorrow Sinterklaas story", "2026-10-10T22:00:00Z"),
        ("Other unrelated holiday story", "2026-10-10T00:00:00Z"),
    ].into_iter().enumerate().map(|(index, (title, date))| Article { title:title.into(), summary:String::new(),
        url:format!("https://nos.nl/artikel/{index}"), published_at:instant(date) }).collect();
    let value = response(&cache(articles, now), &query, now);
    assert_eq!(value["status"], "ready");
    let result = value["articles"].as_array().unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0]["title"], "Local midnight Sinterklaas news");
    assert_eq!(result[0]["publisher_domain"], "nos.nl");
    assert_eq!(result[0]["published_at"], "2026-10-09T22:00:00Z");
}

#[test]
fn empty_success_is_distinct_from_failed_or_cold_cache() {
    let now = instant("2026-10-10T12:00:00Z");
    let query = NewsQuery { query: "Sinterklaas".into(), today: true };
    let cache = cache(vec![], now);
    let result = response(&cache, &query, now);
    assert_eq!(result["status"], "empty");
    assert_eq!(result["coverage"]["available_feeds"], 1);
    cache.feeds[7].state.write().unwrap().refreshed_at = None;
    let result = response(&cache, &query, now);
    assert_eq!(result["status"], "unavailable");
    assert_eq!(result["coverage"]["available_feeds"], 0);
    assert!(result["coverage"]["updated_at"].is_null(), "an attempted load is not a successful feed snapshot");
}

#[test]
fn explicit_results_are_capped_newest_first_and_not_limited_to_two_per_publisher() {
    let now = instant("2026-10-10T12:00:00Z");
    let query = NewsQuery { query: "Sinterklaas".into(), today: false };
    let mut articles: Vec<_> = (0..25).map(|index| Article { title:format!("Sinterklaas nieuws nummer {index}"), summary:String::new(),
        url:format!("https://nos.nl/artikel/{index}"), published_at:now - ChronoDuration::minutes(index + 1) }).collect();
    articles.push(articles[0].clone());
    articles.reverse();
    let result = response(&cache(articles, now), &query, now);
    let articles = result["articles"].as_array().unwrap();
    assert_eq!(articles.len(), 20);
    assert_eq!(articles[0]["url"], "https://nos.nl/artikel/0");
    assert_eq!(articles[19]["url"], "https://nos.nl/artikel/19");
}
