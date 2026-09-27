//! Short-lived cache of public Bazaar directory metadata, after query scrubbing.
//! It never receives questions, subjects, credentials, paid responses or facts.
//! Identical endpoint/query requests share one fetch. Only valid responses are
//! retained (including genuine zero-match responses); errors are never cached.
use super::Candidate;
use moka::future::Cache;
use sha2::{Digest, Sha256};
use std::{mem::size_of, sync::OnceLock, time::Duration};

type DiscoveryCache = Cache<[u8; 32], Vec<Candidate>>;

const TTL: Duration = Duration::from_secs(300);
// Capacity units cover at least 16 KiB each, including every String allocation.
// This bounds retained entries to 128 and their estimated data to 2 MiB. Moka
// performs expiration/eviction maintenance; no periodic cache-clearing job or
// cache of failures can discard healthy data or prolong an upstream outage.
const CAPACITY: u64 = 128;
const UNIT_BYTES: usize = 16 * 1024;

fn weight(candidates: &[Candidate]) -> u32 {
    let bytes = size_of::<[u8; 32]>()
        + size_of::<Vec<Candidate>>()
        + candidates.len() * size_of::<Candidate>()
        + candidates
            .iter()
            .map(|candidate| {
                [
                    &candidate.origin,
                    &candidate.name,
                    &candidate.description,
                    &candidate.method,
                    &candidate.availability,
                ]
                .into_iter()
                .map(String::capacity)
                .sum::<usize>()
                    + [
                        &candidate.source_slug,
                        &candidate.capability,
                        &candidate.endpoint_id,
                        &candidate.url,
                        &candidate.price,
                        &candidate.network,
                    ]
                    .into_iter()
                    .flatten()
                    .map(String::capacity)
                    .sum::<usize>()
            })
            .sum::<usize>();
    bytes.div_ceil(UNIT_BYTES).max(1) as u32
}

fn new_cache(ttl: Duration, capacity: u64) -> DiscoveryCache {
    Cache::builder()
        .max_capacity(capacity)
        .weigher(|_: &[u8; 32], candidates: &Vec<Candidate>| weight(candidates))
        .time_to_live(ttl)
        .build()
}

fn cache() -> &'static DiscoveryCache {
    static CACHE: OnceLock<DiscoveryCache> = OnceLock::new();
    CACHE.get_or_init(|| new_cache(TTL, CAPACITY))
}

fn key(search_url: &str, query: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    // Version and limit are part of the identity. Endpoint scoping keeps local
    // fixtures and any future directory configuration isolated from production.
    hash.update(b"bazaar-directory-v1:limit=3");
    hash.update(search_url.len().to_be_bytes());
    hash.update(search_url.as_bytes());
    hash.update(query.as_bytes());
    hash.finalize().into()
}

pub(super) async fn fetch(
    http: &reqwest::Client,
    search_url: &str,
    query: &str,
) -> anyhow::Result<Vec<Candidate>> {
    fetch_with(cache(), http, search_url, query).await
}

async fn fetch_with(
    cache: &DiscoveryCache,
    http: &reqwest::Client,
    search_url: &str,
    query: &str,
) -> anyhow::Result<Vec<Candidate>> {
    cache
        .try_get_with(
            key(search_url, query),
            super::fetch_coinbase_uncached(http, search_url, query),
        )
        .await
        .map_err(|error| anyhow::anyhow!("Bazaar discovery unavailable: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{http::StatusCode, response::IntoResponse, routing::get, Router};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    const RESOURCES: &str = r#"{"resources":[{"type":"http","resource":"https://example.test/source","serviceName":"Synthetic directory source"}]}"#;

    struct Directory {
        url: String,
        calls: Arc<AtomicUsize>,
        server: tokio::task::JoinHandle<()>,
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    async fn directory(
        first_failure: Option<&'static str>,
        empty: bool,
        delay: Duration,
    ) -> Directory {
        let calls = Arc::new(AtomicUsize::new(0));
        let handler_calls = calls.clone();
        let app = Router::new().route(
            "/search",
            get(move || {
                let calls = handler_calls.clone();
                async move {
                    let index = calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(delay).await;
                    if index == 0 {
                        match first_failure {
                            Some("http") => {
                                return (StatusCode::SERVICE_UNAVAILABLE, "unavailable".to_string())
                                    .into_response()
                            }
                            Some("json") => {
                                return (StatusCode::OK, "invalid json".to_string()).into_response()
                            }
                            Some("schema") => {
                                return (StatusCode::OK, "{}".to_string()).into_response()
                            }
                            Some("size") => {
                                return (StatusCode::OK, " ".repeat(256_001)).into_response()
                            }
                            _ => {}
                        }
                    }
                    (
                        StatusCode::OK,
                        if empty {
                            r#"{"resources":[]}"#
                        } else {
                            RESOURCES
                        },
                    )
                        .into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/search", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Directory { url, calls, server }
    }

    #[tokio::test]
    async fn concurrent_generic_queries_share_one_successful_fetch() {
        let server = directory(None, false, Duration::from_millis(40)).await;
        let cache = new_cache(TTL, CAPACITY);
        let http = reqwest::Client::new();
        let results = futures_util::future::join_all(
            (0..12).map(|_| fetch_with(&cache, &http, &server.url, "Company sanctions")),
        )
        .await;
        assert!(results
            .iter()
            .all(|result| result.as_ref().is_ok_and(|found| found.len() == 1)));
        assert_eq!(server.calls.load(Ordering::SeqCst), 1);
        fetch_with(&cache, &http, &server.url, "Company sanctions")
            .await
            .unwrap();
        assert_eq!(server.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn transport_parse_schema_and_size_errors_never_stick() {
        let http = reqwest::Client::new();
        for failure in ["http", "json", "schema", "size"] {
            let server = directory(Some(failure), false, Duration::ZERO).await;
            let cache = new_cache(TTL, CAPACITY);
            assert!(fetch_with(&cache, &http, &server.url, "Public tenders")
                .await
                .is_err());
            assert_eq!(
                fetch_with(&cache, &http, &server.url, "Public tenders")
                    .await
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(server.calls.load(Ordering::SeqCst), 2, "{failure}");
        }
    }

    #[tokio::test]
    async fn genuine_empty_matches_are_cached_until_expiry() {
        let server = directory(None, true, Duration::ZERO).await;
        let cache = new_cache(Duration::from_millis(60), CAPACITY);
        let http = reqwest::Client::new();
        for _ in 0..2 {
            assert!(fetch_with(&cache, &http, &server.url, "Public weather")
                .await
                .unwrap()
                .is_empty());
        }
        assert_eq!(server.calls.load(Ordering::SeqCst), 1);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(fetch_with(&cache, &http, &server.url, "Public weather")
            .await
            .unwrap()
            .is_empty());
        assert_eq!(server.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn endpoints_and_queries_have_separate_entries() {
        let a = directory(None, false, Duration::ZERO).await;
        let b = directory(None, false, Duration::ZERO).await;
        let cache = new_cache(TTL, CAPACITY);
        let http = reqwest::Client::new();
        for url in [&a.url, &b.url] {
            for query in ["Company sanctions", "Public tenders"] {
                for _ in 0..2 {
                    fetch_with(&cache, &http, url, query).await.unwrap();
                }
            }
        }
        assert_eq!(a.calls.load(Ordering::SeqCst), 2);
        assert_eq!(b.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn capacity_evicts_old_entries_and_accounts_for_large_metadata() {
        let cache = new_cache(TTL, 2);
        for query in ["Company sanctions", "Public tenders", "Public weather"] {
            cache
                .insert(key("https://example.test/search", query), vec![])
                .await;
        }
        cache.run_pending_tasks().await;
        assert!(cache.entry_count() <= 2);
        assert!(cache.weighted_size() <= 2);
        let mut candidates =
            super::super::coinbase_candidates(serde_json::from_str(RESOURCES).unwrap()).unwrap();
        candidates[0].network = Some("n".repeat(UNIT_BYTES * 3));
        assert!(weight(&candidates) > 2);
        let large_key = key("https://example.test/search", "Large metadata");
        cache.insert(large_key, candidates).await;
        cache.run_pending_tasks().await;
        assert!(cache.get(&large_key).await.is_none());
        assert!(cache.weighted_size() <= 2);
    }

    #[tokio::test]
    async fn cancelled_fetch_does_not_leave_a_stuck_coalescing_slot() {
        let server = directory(None, false, Duration::from_millis(80)).await;
        let cache = new_cache(TTL, CAPACITY);
        let http = reqwest::Client::new();
        let first = tokio::spawn({
            let cache = cache.clone();
            let http = http.clone();
            let url = server.url.clone();
            async move { fetch_with(&cache, &http, &url, "Company sanctions").await }
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while server.calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        first.abort();
        let _ = first.await;
        let found = tokio::time::timeout(
            Duration::from_secs(1),
            fetch_with(&cache, &http, &server.url, "Company sanctions"),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(server.calls.load(Ordering::SeqCst), 2);
    }
}
