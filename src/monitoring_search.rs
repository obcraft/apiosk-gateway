//! Sponsored company discovery for Monitoring. KVK's official Zoeken endpoint
//! is free per request under the existing API subscription. This narrowly
//! bounded endpoint never creates a buyer purchase or changes catalog pricing.
use serde_json::{json, Value};
use sqlx::PgPool;
use std::{collections::{HashMap, VecDeque}, sync::{LazyLock, Mutex}, time::{Duration, Instant}};
use uuid::Uuid;

const TTL: Duration = Duration::from_secs(30);
const WINDOW: Duration = Duration::from_secs(60);
const PAGE_SIZE: u64 = 20;
const MAX_BODY: usize = 512 * 1024;

#[derive(Debug, Clone, Copy)]
pub enum SearchError { Invalid, RateLimited, Unavailable }
impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Invalid => "Enter a company name of 2–120 characters or an eight-digit KVK number.",
            Self::RateLimited => "Too many company searches. Wait a moment and try again.",
            Self::Unavailable => "KVK search is temporarily unavailable. Your selection and saved checks are unchanged.",
        })
    }
}
impl std::error::Error for SearchError {}

#[derive(Default)]
struct SearchState {
    owners: HashMap<Uuid, VecDeque<Instant>>,
    global: VecDeque<Instant>,
    cache: HashMap<(String, u32), (Instant, Value)>,
}
static STATE: LazyLock<Mutex<SearchState>> = LazyLock::new(|| Mutex::new(SearchState::default()));
static HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| reqwest::Client::builder()
    .redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(12))
    .build().expect("monitoring KVK client"));
static PERMITS: LazyLock<tokio::sync::Semaphore> = LazyLock::new(|| tokio::sync::Semaphore::new(8));

fn validate(query: &str, page: u32) -> Result<String, SearchError> {
    let query = query.trim();
    if !(2..=120).contains(&query.chars().count()) || query.chars().any(char::is_control)
        || !(1..=1000).contains(&page) { return Err(SearchError::Invalid); }
    Ok(query.to_owned())
}

fn prune(times: &mut VecDeque<Instant>, now: Instant) {
    while times.front().is_some_and(|at| now.duration_since(*at) >= WINDOW) { times.pop_front(); }
}

fn admit(state: &mut SearchState, owner: Uuid, now: Instant) -> Result<(), SearchError> {
    state.owners.retain(|_, times| { prune(times, now); !times.is_empty() });
    prune(&mut state.global, now);
    if state.global.len() >= 1200 || state.owners.get(&owner).is_some_and(|times| times.len() >= 60) {
        return Err(SearchError::RateLimited);
    }
    state.owners.entry(owner).or_default().push_back(now);
    state.global.push_back(now);
    Ok(())
}

fn text(row: &Value, key: &str) -> Option<String> {
    row[key].as_str().map(str::trim).filter(|s| !s.is_empty())
        .map(|s| s.chars().filter(|c| !c.is_control()).take(240).collect())
}

fn normalize(document: &Value, query: &str, page: u32, at: &str) -> Result<Value, SearchError> {
    let rows = document["resultaten"].as_array().ok_or(SearchError::Unavailable)?;
    let mut candidates: Vec<Value> = Vec::new();
    let rank = |kind: &Value| match kind.as_str() { Some("rechtspersoon") => 0, Some("hoofdvestiging") => 1, _ => 2 };
    for row in rows.iter().take(PAGE_SIZE as usize) {
        let (Some(kvk), Some(name)) = (text(row, "kvkNummer"), text(row, "naam")) else { continue };
        if kvk.len() != 8 || !kvk.bytes().all(|b| b.is_ascii_digit()) { continue; }
        let address = row.pointer("/adres/binnenlandsAdres").or_else(|| row.get("adres")).unwrap_or(&Value::Null);
        let visible = row["indAfgeschermd"] != "Ja" && address["indAfgeschermd"] != "Ja";
        let establishment = text(row, "vestigingsnummer").filter(|s| s.len() == 12 && s.bytes().all(|b| b.is_ascii_digit()));
        let candidate = json!({"name":name,"kvk_number":kvk,"registration_type":text(row,"type"),
            "city":if visible {text(address,"plaats").or_else(||text(row,"plaats"))}else{None},
            "street":if visible {text(address,"straatnaam").or_else(||text(row,"straatnaam"))}else{None},
            "establishment_number":establishment});
        if let Some(existing) = candidates.iter_mut().find(|c| c["kvk_number"] == candidate["kvk_number"]) {
            // One company identity, while retaining a verified main-branch ID
            // for the optional branch profile. Never merge different KVK IDs.
            let main = if candidate["registration_type"] == "hoofdvestiging" { Some(candidate.clone()) }
                else if existing["registration_type"] == "hoofdvestiging" { Some(existing.clone()) }
                else { None };
            if rank(&candidate["registration_type"]) < rank(&existing["registration_type"]) { *existing = candidate; }
            if let Some(main) = main {
                if !main["establishment_number"].is_null() { existing["establishment_number"] = main["establishment_number"].clone(); }
                for field in ["city", "street"] { if existing[field].is_null() { existing[field] = main[field].clone(); } }
            }
        } else { candidates.push(candidate); }
    }
    let total = document["totaal"].as_u64().unwrap_or(rows.len() as u64);
    let per_page = document["resultatenPerPagina"].as_u64().filter(|n| *n > 0).unwrap_or(PAGE_SIZE);
    let has_more = document.get("volgende").is_some_and(|v| !v.is_null() && v.as_str().is_none_or(|s| !s.is_empty()))
        || (page as u64).saturating_mul(per_page) < total;
    Ok(json!({"query":query,"page":page,"total":total,"has_more":has_more,"retrieved_at":at,
        "source":{"name":"KVK Handelsregister","url":"https://developers.kvk.nl/nl/documentation/zoeken-api"},
        "results":candidates}))
}

fn response_document(status: u16, document: Value) -> Result<Value, SearchError> {
    // KVK documents IPD5200/404 as an ordinary empty search. Other 404s
    // include temporary source outages and must never look like no matches.
    if status == 404 && document["fout"].as_array().is_some_and(|errors|
        !errors.is_empty() && errors.iter().all(|error|error["code"]=="IPD5200")) {
        return Ok(json!({"totaal":0,"resultaten":[]}));
    }
    if (200..300).contains(&status) { Ok(document) } else { Err(SearchError::Unavailable) }
}

/// Credentials are read server-side from the existing verified official KVK
/// registration. A changed host, endpoint, verb or block refuses this route.
pub async fn search(pool: &PgPool, owner: Uuid, query: &str, page: u32) -> anyhow::Result<Value> {
    let query = validate(query, page)?;
    let key = (query.clone(), page);
    {
        let mut state = STATE.lock().map_err(|_| SearchError::Unavailable)?;
        let now = Instant::now();
        admit(&mut state, owner, now)?;
        state.cache.retain(|_, (at, _)| now.duration_since(*at) < TTL);
        if let Some((_, cached)) = state.cache.get(&key) { return Ok(cached.clone()); }
    }
    let _permit = PERMITS.try_acquire().map_err(|_| SearchError::RateLimited)?;
    // The private, runtime-only function checks the exact official origin and
    // endpoint before returning this one credential. No broad credential-table
    // grants or second secret copy are needed for autocomplete.
    let secret: Option<String> = sqlx::query_scalar("SELECT gateway_v2.monitoring_kvk_search_api_key()")
        .fetch_one(pool).await.map_err(|_| SearchError::Unavailable)?;
    let secret = secret.filter(|s| !s.is_empty()).ok_or(SearchError::Unavailable)?;
    let mut secret = reqwest::header::HeaderValue::from_str(&secret).map_err(|_| SearchError::Unavailable)?;
    secret.set_sensitive(true);
    let mut url = url::Url::parse("https://api.kvk.nl/api/v2/zoeken").expect("official KVK URL");
    url.query_pairs_mut().append_pair(if query.len() == 8 && query.bytes().all(|b| b.is_ascii_digit()) { "kvkNummer" } else { "naam" }, &query)
        .append_pair("pagina", &page.to_string()).append_pair("resultatenPerPagina", &PAGE_SIZE.to_string());
    let mut response = HTTP.get(url).header("apikey", secret).header("Accept", "application/json")
        .send().await.map_err(|_| SearchError::Unavailable)?;
    if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS { return Err(SearchError::RateLimited.into()); }
    let status = response.status().as_u16();
    if (!response.status().is_success() && status != 404) || response.content_length().is_some_and(|n| n > MAX_BODY as u64) {
        return Err(SearchError::Unavailable.into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| SearchError::Unavailable)? {
        if body.len() + chunk.len() > MAX_BODY { return Err(SearchError::Unavailable.into()); }
        body.extend_from_slice(&chunk);
    }
    let document = response_document(status, serde_json::from_slice(&body).map_err(|_| SearchError::Unavailable)?)?;
    let output = normalize(&document, &query, page, &chrono::Utc::now().to_rfc3339())?;
    let mut state = STATE.lock().map_err(|_| SearchError::Unavailable)?;
    if state.cache.len() >= 256 { state.cache.clear(); }
    state.cache.insert(key, (Instant::now(), output.clone()));
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn input_and_rate_are_bounded() {
        for q in ["", "a", "a\nb"] { assert!(validate(q, 1).is_err()); }
        assert!(validate("Heineken", 0).is_err());
        assert!(validate(&"a".repeat(121), 1).is_err());
        let mut state = SearchState::default(); let owner = Uuid::new_v4(); let at = Instant::now();
        for _ in 0..60 { admit(&mut state, owner, at).unwrap(); }
        assert!(matches!(admit(&mut state, owner, at), Err(SearchError::RateLimited)));
        admit(&mut state, owner, at + WINDOW).unwrap();
    }
    #[test]
    fn exact_identity_deduplication_keeps_main_branch_and_pagination() {
        let data = json!({"totaal":30,"resultatenPerPagina":20,"resultaten":[
            {"kvkNummer":"01234567","naam":"Acme","type":"hoofdvestiging","vestigingsnummer":"000012345678","adres":{"binnenlandsAdres":{"plaats":"Amsterdam"}}},
            {"kvkNummer":"01234567","naam":"Acme B.V.","type":"rechtspersoon"},
            {"kvkNummer":"12345678","naam":"Acme","type":"hoofdvestiging"},
            {"kvkNummer":"bad","naam":"Invalid"}]});
        let out = normalize(&data,"Acme",1,"now").unwrap();
        assert_eq!(out["results"].as_array().unwrap().len(),2);
        assert_eq!(out["results"][0]["name"],"Acme B.V.");
        assert_eq!(out["results"][0]["establishment_number"],"000012345678");
        assert_eq!(out["has_more"],true);
    }
    #[test]
    fn empty_results_are_valid_and_shielded_addresses_stay_hidden() {
        assert_eq!(normalize(&json!({"totaal":0,"resultaten":[]}),"Absent",1,"now").unwrap()["has_more"],false);
        let out = normalize(&json!({"resultaten":[{"kvkNummer":"12345678","naam":"Hidden","adres":{"indAfgeschermd":"Ja","plaats":"Secret","straatnaam":"Secret"}}]}),"Hidden",1,"now").unwrap();
        assert!(out["results"][0]["city"].is_null()); assert!(out["results"][0]["street"].is_null());
        assert!(normalize(&json!({"error":"not results"}),"Acme",1,"now").is_err());
    }
    #[test]
    fn only_official_no_results_error_becomes_an_empty_search() {
        let empty=response_document(404,json!({"fout":[{"code":"IPD5200"}]})).unwrap();
        assert_eq!(normalize(&empty,"Unmatched",1,"now").unwrap()["results"],json!([]));
        for error in [json!({"fout":[{"code":"IPD1002"}]}),json!({"fout":[]}),json!({"fout":[{"code":"IPD5200"},{"code":"IPD1003"}]})] {
            assert!(response_document(404,error).is_err());
        }
        assert!(response_document(500,json!({"fout":[{"code":"IPD5200"}]})).is_err());
    }
}
