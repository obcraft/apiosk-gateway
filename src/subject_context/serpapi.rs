//! Operator-funded Google search. One request per cached subject, no retries.
//! The fixed origin and disabled redirects keep the API key out of other hosts.
use super::{compact, matches_result};
use reqwest::Client;
use serde_json::{json, Value};
use std::{collections::BTreeSet, sync::OnceLock, time::Duration};
use url::Url;

const ENDPOINT: &str = "https://serpapi.com/search.json";
const TIMEOUT: Duration = Duration::from_secs(8);
const MAX_BYTES: usize = 2 * 1024 * 1024;

fn api_key() -> Option<String> {
    std::env::var("SERPAPI_API_KEY")
        .ok()
        .map(|key| key.trim().to_owned())
        .filter(|key| !key.is_empty())
}

fn selected(provider: &str, has_key: bool) -> bool {
    match provider.trim().to_ascii_lowercase().as_str() {
        "serpapi" => true,
        "public" => false,
        "" | "auto" => has_key,
        _ => false,
    }
}

pub(super) fn enabled() -> bool {
    selected(
        &std::env::var("ASK_WEB_SEARCH_PROVIDER").unwrap_or_default(),
        api_key().is_some(),
    )
}

pub(super) fn cache_partition() -> &'static str {
    if enabled() {
        "serpapi-v1"
    } else {
        "public-v1"
    }
}

pub(super) async fn search(
    label: &str,
    query: &str,
    topic: bool,
) -> Result<Vec<Value>, &'static str> {
    let key = api_key().ok_or("missing_api_key")?;
    static CLIENT: OnceLock<Option<Client>> = OnceLock::new();
    let client = CLIENT
        .get_or_init(|| {
            Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(TIMEOUT)
                .connect_timeout(Duration::from_secs(3))
                .build()
                .ok()
        })
        .as_ref()
        .ok_or("client_unavailable")?;
    let payload = request(client, ENDPOINT, &key, query).await?;
    pages(&payload, label, topic)
}

async fn request(
    client: &Client,
    endpoint: &str,
    key: &str,
    query: &str,
) -> Result<Value, &'static str> {
    let mut response = client
        .get(endpoint)
        .query(&[("engine", "google"), ("q", query), ("api_key", key)])
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|_| "transport")?;
    if !response.status().is_success() {
        return Err("http_status");
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BYTES as u64)
    {
        return Err("response_too_large");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "transport")? {
        if bytes.len().saturating_add(chunk.len()) > MAX_BYTES {
            return Err("response_too_large");
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "invalid_json")
}

fn pages(payload: &Value, label: &str, topic: bool) -> Result<Vec<Value>, &'static str> {
    if !payload["error"].is_null()
        || payload["search_metadata"]["status"].as_str() != Some("Success")
    {
        return Err("search_failed");
    }
    // An empty successful search is valid; any present malformed list is not.
    let rows = match payload.get("organic_results") {
        Some(Value::Array(rows)) => rows.as_slice(),
        None => &[],
        _ => return Err("invalid_results"),
    };
    let mut seen = BTreeSet::new();
    Ok(rows.iter().take(40).filter_map(|row| {
        let mut url = Url::parse(row["link"].as_str()?).ok()?;
        if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() || url.password().is_some() { return None; }
        url.set_fragment(None);
        let host = url.host_str()?;
        let title = compact(row["title"].as_str()?, 240);
        let description = compact(row["snippet"].as_str().unwrap_or(""), 600);
        if title.is_empty() || !matches_result(label, &title, host, &description, topic) || !seen.insert(url.to_string()) { return None; }
        let publisher = row["source"].as_str().filter(|source| !source.trim().is_empty()).unwrap_or(host);
        let mut page = json!({"kind":"search_result","url":url.as_str(),"title":title,"description":description,
            "publisher":compact(publisher,200),"search_provider":"serpapi","retrieved_at":chrono::Utc::now().to_rfc3339()});
        if let Some(date) = row["date"].as_str().filter(|date| !date.trim().is_empty()) {
            page["published_at"] = json!(compact(date,120));
        }
        Some(page)
    }).take(5).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "uses one configured SerpApi search credit; requires SERPAPI_API_KEY"]
    async fn live_configured_serpapi_returns_public_source_pages() {
        let rows = search("Amsterdam", "Amsterdam official website", false).await.unwrap();
        assert!(!rows.is_empty(), "SerpApi returned no relevant public pages");
        assert!(rows.iter().all(|page| page["search_provider"] == "serpapi"
            && page["url"].as_str().is_some_and(|url| url.starts_with("http"))
            && page["retrieved_at"].is_string()));
    }

    #[test]
    fn provider_selection_never_requires_a_browser_secret() {
        assert!(selected("auto", true));
        assert!(!selected("auto", false));
        assert!(selected("serpapi", false));
        assert!(!selected("public", true));
        assert!(!selected("unknown", true));
    }

    #[test]
    fn organic_results_keep_provenance_and_filter_unsafe_unrelated_and_duplicate_links() {
        let payload = json!({"search_metadata":{"status":"Success","id":"provider-id"},"organic_results":[
            {"title":"Acme company","link":"https://acme.example/about#first","snippet":"Acme provides products.","source":"Acme","date":"Oct 22, 2026"},
            {"title":"Acme duplicate","link":"https://acme.example/about#second"},
            {"title":"Unrelated shop","link":"https://other.example/"},
            {"title":"Acme unsafe","link":"javascript:alert(1)"},
            {"title":"Acme credentials","link":"https://secret@acme.example/"}
        ]});
        let rows = pages(&payload, "Acme", false).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["url"], "https://acme.example/about");
        assert_eq!(rows[0]["description"], "Acme provides products.");
        assert_eq!(rows[0]["publisher"], "Acme");
        assert_eq!(rows[0]["published_at"], "Oct 22, 2026");
        assert!(rows[0]["retrieved_at"].is_string());
        assert!(!rows[0].to_string().contains("provider-id"));
    }

    #[test]
    fn upstream_errors_are_fixed_codes_and_successful_empty_results_stay_empty() {
        for payload in [
            json!({"error":"api_key=secret"}),
            json!({"search_metadata":{"status":"Processing"}}),
        ] {
            assert_eq!(pages(&payload, "Acme", false).unwrap_err(), "search_failed");
        }
        assert!(pages(
            &json!({"search_metadata":{"status":"Success"}}),
            "Acme",
            false
        )
        .unwrap()
        .is_empty());
        assert_eq!(
            pages(
                &json!({"search_metadata":{"status":"Success"},"organic_results":{}}),
                "Acme",
                false
            )
            .unwrap_err(),
            "invalid_results"
        );
    }

    #[tokio::test]
    async fn transport_uses_one_json_request_and_never_follows_a_credential_redirect() {
        use axum::{
            extract::Query,
            http::{HeaderMap, StatusCode},
            routing::get,
            Json, Router,
        };
        use std::{
            collections::HashMap,
            sync::{
                atomic::{AtomicUsize, Ordering},
                Arc,
            },
        };
        let hits = Arc::new(AtomicUsize::new(0));
        let observed = hits.clone();
        let app = Router::new().route("/search", get(move |Query(query): Query<HashMap<String,String>>, headers: HeaderMap| {
            let observed = observed.clone();
            async move {
                observed.fetch_add(1, Ordering::SeqCst);
                assert_eq!(query.get("api_key").map(String::as_str),Some("fixture-secret"));
                assert_eq!(query.get("q").map(String::as_str),Some("Acme & partners"));
                assert_eq!(query.get("engine").map(String::as_str),Some("google"));
                assert_eq!(headers["accept"],"application/json");
                Json(json!({"search_metadata":{"status":"Success"},"organic_results":[]}))
            }
        })).route("/redirect", get(|| async { (StatusCode::FOUND, [("location","/search?api_key=fixture-secret")]) }))
          .route("/limited", get(|| async { StatusCode::TOO_MANY_REQUESTS }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(TIMEOUT)
            .build()
            .unwrap();
        assert!(request(
            &client,
            &format!("{origin}/search"),
            "fixture-secret",
            "Acme & partners"
        )
        .await
        .is_ok());
        assert_eq!(
            request(
                &client,
                &format!("{origin}/redirect"),
                "fixture-secret",
                "Acme"
            )
            .await
            .unwrap_err(),
            "http_status"
        );
        assert_eq!(
            request(
                &client,
                &format!("{origin}/limited"),
                "fixture-secret",
                "Acme"
            )
            .await
            .unwrap_err(),
            "http_status"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        server.abort();
    }
}
