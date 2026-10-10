//! Explicit news searches are separate from conservative result supplements.
//! Only this dedicated read-only route may wait for a bounded feed refresh.
use super::*;
use chrono::TimeZone;
use chrono_tz::Europe::Amsterdam;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NewsQuery {
    pub query: String,
    pub today: bool,
}

/// Deliberately narrow whole-question templates. Requests with additional
/// retrievals or unsupported constraints continue through the ordinary flow.
pub(crate) fn classify_news(question: &str) -> Option<NewsQuery> {
    let question = question.trim();
    if question.chars().count() > 2000 || question.chars().any(char::is_control) { return None; }
    let question = question.trim_end_matches(['?', '.', '!']).trim_end();
    if question.chars().any(|c| matches!(c, '.' | '?' | '!' | ',' | ';' | '+' | '&' | '/' | '\\')) { return None; }
    let lower = question.to_ascii_lowercase();
    let mut capture = None;
    for separator in ["over", "about"] {
        for (index, _) in lower.match_indices(separator) {
            if index == 0 || !lower[..index].ends_with(char::is_whitespace) { continue; }
            let after = &question[index + separator.len()..];
            if !after.starts_with(|c: char| c.is_whitespace() || c == ':') { continue; }
            let Some(today) = news_header(lower[..index].trim(), separator == "over") else { continue };
            let after = after.trim_start();
            let query = after.strip_prefix(':').unwrap_or(after).trim();
            if query.contains(':') { return None; }
            capture = Some((query, today));
            break;
        }
        if capture.is_some() { break; }
    }
    let (query, today) = capture?;
    let words = normalize(query);
    if query.is_empty() || query.chars().count() > 160 || words.split_whitespace().count() > 16
        || terms(query).is_empty() { return None; }
    // These tails introduce a source, time, place, language, output, action or
    // context-dependent request that a fixed current-feed search cannot honor.
    if words.split_whitespace().any(|word| matches!(word,
        "and" | "en" | "plus" | "from" | "by" | "bij" | "near" | "using" | "via" | "according" | "volgens" | "excluding" | "except" | "zonder"
        | "in" | "on" | "since" | "until" | "vanaf" | "sinds" | "tussen" | "before" | "after"
        | "today" | "vandaag" | "yesterday" | "gisteren" | "tomorrow" | "morgen" | "last" | "past" | "this" | "morning"
        | "vorige" | "afgelopen" | "januari" | "februari" | "maart" | "april" | "mei" | "juni"
        | "juli" | "augustus" | "september" | "oktober" | "november" | "december"
        | "january" | "february" | "march" | "may" | "june" | "july" | "august" | "october"
        | "as" | "als" | "pdf" | "excel" | "xlsx" | "docx" | "json" | "csv"
        | "send" | "mail" | "email" | "stuur" | "slack" | "export" | "download"
        | "check" | "controleer" | "haal" | "retrieve" | "koop" | "buy" | "zoek" | "toon" | "geef"
        | "official" | "officieel" | "officiële" | "officiele" | "kvk" | "sec" | "jaarrekening"
        | "jaarrekeningen" | "share" | "screening" | "vergunning" | "permit"
        | "nos" | "bbc" | "npr" | "guardian" | "reuters" | "nrc" | "nytimes" | "jazeera"
        | "it" | "its" | "hetzelfde" | "dezelfde" | "dit" | "dat" | "that" | "same")
        || word.parse::<u32>().is_ok_and(|year| (1900..=2100).contains(&year))) {
        return None;
    }
    Some(NewsQuery { query: query.to_owned(), today })
}

fn news_header(header: &str, dutch: bool) -> Option<bool> {
    let words: Vec<_> = header.split_whitespace().collect();
    if dutch && words == ["wat", "wordt", "er", "vandaag", "in", "het", "nieuws", "gezegd"] { return Some(true); }
    if dutch && words == ["wat", "wordt", "er", "in", "het", "nieuws", "gezegd"] { return Some(false); }
    if dutch && (words == ["wat", "is", "er", "vandaag", "in", "het", "nieuws"]
        || words == ["welk", "nieuws", "is", "er", "vandaag"]) { return Some(true); }
    let mut words = words.as_slice();
    if dutch {
        if words.starts_with(&["wat", "is"]) { words = &words[2..]; }
        else if words.first().is_some_and(|word| matches!(*word, "toon" | "geef" | "zoek")) { words = &words[1..]; }
        if words.first() == Some(&"het") { words = &words[1..]; }
        if words.first().is_some_and(|word| matches!(*word, "laatste" | "recent" | "recente" | "actuele" | "actueel")) { words = &words[1..]; }
        match words { ["nieuws"] => Some(false), ["nieuws", "vandaag"] => Some(true), _ => None }
    } else {
        if words.starts_with(&["what", "is"]) { words = &words[2..]; }
        else if words.first().is_some_and(|word| matches!(*word, "show" | "find")) { words = &words[1..]; }
        if words.first() == Some(&"the") { words = &words[1..]; }
        if words.first().is_some_and(|word| matches!(*word, "latest" | "recent" | "current")) { words = &words[1..]; }
        match words { ["news"] => Some(false), ["news", "today"] => Some(true), _ => None }
    }
}

fn bounds(query: &NewsQuery, now: DateTime<Utc>) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    if !query.today { return Some((now - ChronoDuration::days(7), now)); }
    let date = now.with_timezone(&Amsterdam).date_naive();
    let start = Amsterdam.from_local_datetime(&date.and_hms_opt(0, 0, 0)?).single()?.with_timezone(&Utc);
    let end = Amsterdam.from_local_datetime(&date.succ_opt()?.and_hms_opt(0, 0, 0)?).single()?.with_timezone(&Utc);
    Some((start, end))
}

pub(crate) async fn search_news(query: NewsQuery) -> Value {
    let cache = CACHE.get_or_init(NewsCache::new);
    cache.refresh().await;
    response(cache, &query, Utc::now())
}

fn response(cache: &NewsCache, query: &NewsQuery, now: DateTime<Utc>) -> Value {
    let Some((start, end)) = bounds(query, now) else { return json!({"handled":false}); };
    let wanted = terms(&query.query);
    if wanted.is_empty() { return json!({"handled":false}); }
    let normalized_query = normalize(&query.query);
    let mut ranked = Vec::new();
    let mut available = 0;
    let mut updated_at = None;
    for (feed, cache) in FEEDS.into_iter().zip(&cache.feeds) {
        let Ok(snapshot) = cache.state.try_read() else { continue };
        let Some(refreshed_at) = snapshot.refreshed_at.filter(|at| *at >= now - ChronoDuration::days(7)) else { continue };
        available += 1;
        updated_at = Some(updated_at.map_or(refreshed_at, |old: DateTime<Utc>| old.max(refreshed_at)));
        for article in snapshot.articles.iter() {
            if article.published_at < start || article.published_at >= end || article.published_at > now { continue; }
            let title = normalize(&article.title);
            let text = format!("{} {}", article.title, article.summary);
            if !wanted.is_subset(&terms(&text)) { continue; }
            let score = usize::from(phrase(&title, &normalized_query)) * 2
                + wanted.intersection(&terms(&article.title)).count();
            ranked.push((article.published_at, score, feed, article.clone()));
        }
    }
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)).then_with(|| a.3.url.cmp(&b.3.url)));
    let mut seen_urls = HashSet::new();
    let mut seen_titles = HashSet::new();
    let articles: Vec<_> = ranked.into_iter().filter_map(|(_, _, feed, article)| {
        if !seen_urls.insert(article.url.clone()) || !seen_titles.insert(normalize(&article.title)) { return None; }
        Some(json!({"publisher":feed.publisher,"publisher_domain":feed.domains[0],
            "title":article.title,"summary":article.summary,"url":article.url,"feed_url":feed.url,
            "published_at":article.published_at.to_rfc3339_opts(SecondsFormat::Secs,true)}))
    }).take(20).collect();
    let status = if !articles.is_empty() { "ready" } else if available > 0 { "empty" } else { "unavailable" };
    json!({"handled":true,"kind":"rss_news","status":status,"query":query.query,
        "time_window":{"kind":if query.today {"today"} else {"recent"},"timezone":"Europe/Amsterdam",
            "start":start.to_rfc3339_opts(SecondsFormat::AutoSi,true),"end":end.to_rfc3339_opts(SecondsFormat::AutoSi,true)},
        "articles":articles,"coverage":{"configured_feeds":FEEDS.len(),"available_feeds":available,
            "updated_at":updated_at.map(|at| at.to_rfc3339_opts(SecondsFormat::Secs,true))}})
}

#[cfg(test)]
#[path = "supporting_news_direct_tests.rs"]
mod tests;
