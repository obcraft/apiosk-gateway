use super::*;
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn keys_and_provider_schema_are_strict() {
    for bad in [
        "123",
        "123456789",
        "1234abcd",
        "12345678?year=2024",
        "12345678/2024",
    ] {
        assert!(validate_number(bad).is_err());
    }
    assert_eq!(
        number_from_path("/api/v1/hvds/jaarrekeningen/kvknummer/01234567").unwrap(),
        "01234567"
    );
    assert!(number_from_path("/api/v1/hvds/jaarrekeningen/01234567").is_err());
    assert!(financial_year(&json!({"opendataFields":[]})).is_err());
    assert!(financial_year(&json!({"opendataFields":[{"key":"FinancialYear","value":"2024"},{"key":"FinancialYear","value":"2023"}]})).is_err());
    assert_eq!(retry_after_seconds("120"), 120);
    assert_eq!(retry_after_seconds("invalid"), 65);
    assert!(retry_after_seconds("Wed, 01 Jan 2031 00:00:00 GMT") > 65);
}

async fn release_clock(pool: &PgPool) {
    sqlx::query("UPDATE kvk_cache.rate_limit SET next_allowed_at=now()-interval '1 second'")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE kvk_cache.jobs SET available_at=now()-interval '1 second' WHERE status='queued'",
    )
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "requires an isolated real PostgreSQL; scripts/test-kvk-accounts.py"]
async fn durable_single_flight_rate_limit_versions_and_recovery() {
    let database = std::env::var("KVK_TEST_DATABASE_URL").expect("isolated database required");
    assert!(database.starts_with("postgresql://postgres@127.0.0.1:"));
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(20)
        .connect(&database)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE ROLE anon; CREATE ROLE authenticated; CREATE ROLE gateway_v2_runtime; CREATE ROLE service_role;").execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../../../supabase/migrations/20260912111106_kvk_annual_accounts_queue.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    // Real database admission: distinct production routes cannot multiply quota.
    let mut first_route = Workers::new(pool.clone()).unwrap();
    first_route.egress_key = "192.0.2.10".into();
    let mut second_route = first_route.clone();
    second_route.egress_key = "192.0.2.11".into();
    first_route.check_schema().await.unwrap();
    second_route.check_schema().await.unwrap();
    lookup(&pool, "00000001").await.unwrap();
    lookup(&pool, "00000002").await.unwrap();
    let admitted = first_route.claim().await.unwrap().unwrap();
    assert!(second_route.claim().await.unwrap().is_none());
    first_route
        .finish(
            &admitted,
            Outcome::Success(json!({"opendataFields":[{"key":"FinancialYear","value":"2024"}]})),
        )
        .await
        .unwrap();
    assert!(
        second_route.claim().await.unwrap().is_none(),
        "completed fetch must retain shared pacing"
    );
    // Reset only this disposable local fixture, before the independent cases.
    sqlx::raw_sql("TRUNCATE kvk_cache.fetch_attempts,kvk_cache.jobs,kvk_cache.annual_accounts; DELETE FROM kvk_cache.rate_limit; UPDATE kvk_cache.worker_slots SET lease_token=NULL,lease_until=NULL;").execute(&pool).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let mode = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let output_mode = mode.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 4096];
            let n = socket.read(&mut buffer).await.unwrap();
            assert!(String::from_utf8_lossy(&buffer[..n])
                .contains("GET /api/v1/hvds/jaarrekeningen/kvknummer/"));
            count.fetch_add(1, Ordering::SeqCst);
            let m = output_mode.load(Ordering::SeqCst);
            let data = json!({"opendataFields":[{"key":"FinancialYear","value":if m==3 {"2025"} else {"2024"}}, {"key":"Assets","value":m.to_string()}]}).to_string();
            let (status, body, extra) = match m {
                4 => (
                    "429 Too Many Requests",
                    "{}".to_owned(),
                    "Retry-After: 130\r\n",
                ),
                5 => ("404 Not Found", "{}".to_owned(), ""),
                6 => ("200 OK", "{}".to_owned(), ""),
                _ => ("200 OK", data, ""),
            };
            let response = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let mut workers = Workers::new(pool.clone()).unwrap();
    workers.origin = origin;
    workers.check_schema().await.unwrap();
    let mut waiters = Vec::new();
    for _ in 0..10 {
        let db = pool.clone();
        waiters.push(tokio::spawn(async move {
            assert!(matches!(
                lookup(&db, "01234567").await.unwrap(),
                Lookup::Pending { .. }
            ));
        }));
    }
    for waiter in waiters {
        waiter.await.unwrap();
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM kvk_cache.jobs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    let mut runners = Vec::new();
    for _ in 0..5 {
        let w = workers.clone();
        runners.push(tokio::spawn(async move { w.run_once().await.unwrap() }));
    }
    let mut worked = 0;
    for runner in runners {
        if runner.await.unwrap() {
            worked += 1;
        }
    }
    assert_eq!(worked, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    for _ in 0..10 {
        assert!(matches!(
            lookup(&pool, "01234567").await.unwrap(),
            Lookup::Ready(_)
        ));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let version: String =
        sqlx::query_scalar("SELECT version FROM kvk_cache.jobs WHERE kvk_number='01234567'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(document(&pool, "01234567", 2024, &version)
        .await
        .unwrap()
        .is_some());
    assert!(document(&pool, "01234567", 2023, &version)
        .await
        .unwrap()
        .is_none());
    // A different company is queued while the shared rate budget is closed.
    lookup(&pool, "22222222").await.unwrap();
    assert!(!workers.run_once().await.unwrap());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    release_clock(&pool).await;
    workers.run_once().await.unwrap();
    // Corrected versions and new years coexist, including after worker restart.
    for m in [2, 3] {
        mode.store(m, Ordering::SeqCst);
        sqlx::query("UPDATE kvk_cache.jobs SET fresh_until=now()-interval '1 second' WHERE kvk_number='01234567'").execute(&pool).await.unwrap();
        assert!(matches!(
            lookup(&pool, "01234567").await.unwrap(),
            Lookup::Pending { .. }
        ));
        release_clock(&pool).await;
        let restarted = workers.clone();
        restarted.run_once().await.unwrap();
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM kvk_cache.annual_accounts WHERE kvk_number='01234567'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        3
    );
    assert!(document(&pool, "01234567", 2024, &version)
        .await
        .unwrap()
        .is_some());
    // Throttles back off globally and never poison the document cache.
    mode.store(4, Ordering::SeqCst);
    lookup(&pool, "33333333").await.unwrap();
    release_clock(&pool).await;
    workers.run_once().await.unwrap();
    assert!(matches!(
        lookup(&pool, "33333333").await.unwrap(),
        Lookup::Pending { .. }
    ));
    let delay: bool = sqlx::query_scalar(
        "SELECT next_allowed_at>now()+interval '125 seconds' FROM kvk_cache.rate_limit",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(delay);
    assert!(!workers.run_once().await.unwrap());
    mode.store(0, Ordering::SeqCst);
    release_clock(&pool).await;
    workers.run_once().await.unwrap();
    // A missing report is shared briefly, without an infinite job/retry loop.
    mode.store(5, Ordering::SeqCst);
    lookup(&pool, "44444444").await.unwrap();
    release_clock(&pool).await;
    workers.run_once().await.unwrap();
    assert!(matches!(
        lookup(&pool, "44444444").await.unwrap(),
        Lookup::Failed { .. }
    ));
    // Recovery uses leases and fences late writes from an expired worker.
    mode.store(0, Ordering::SeqCst);
    lookup(&pool, "55555555").await.unwrap();
    release_clock(&pool).await;
    let old = workers.claim().await.unwrap().unwrap();
    assert!(!workers.run_once().await.unwrap());
    sqlx::raw_sql("UPDATE kvk_cache.jobs SET lease_until=now()-interval '1 second' WHERE status='running'; UPDATE kvk_cache.worker_slots SET lease_until=now()-interval '1 second'; UPDATE kvk_cache.rate_limit SET active_until=now()-interval '1 second';").execute(&pool).await.unwrap();
    release_clock(&pool).await;
    workers.run_once().await.unwrap();
    workers
        .finish(
            &old,
            Outcome::Failure {
                code: "late".into(),
                retry: false,
                delay: 65,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        lookup(&pool, "55555555").await.unwrap(),
        Lookup::Ready(_)
    ));
    // Invalid success schemas exhaust a bounded retry budget and store no data.
    mode.store(6, Ordering::SeqCst);
    lookup(&pool, "66666666").await.unwrap();
    for _ in 0..5 {
        release_clock(&pool).await;
        workers.run_once().await.unwrap();
    }
    assert!(matches!(
        lookup(&pool, "66666666").await.unwrap(),
        Lookup::Failed { .. }
    ));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM kvk_cache.annual_accounts WHERE kvk_number='66666666'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
    // Five independent egress budgets deliver five distinct cold misses in
    // the same minute. A sixth cannot consume an already-used IP's budget.
    mode.store(0, Ordering::SeqCst);
    let mut routes = Vec::new();
    for i in 0..5 {
        let mut route = workers.clone();
        route.egress_key = format!("fixture-egress-{i}");
        route.check_schema().await.unwrap();
        routes.push(route);
        lookup(&pool, &format!("7000000{i}")).await.unwrap();
    }
    release_clock(&pool).await;
    let before = calls.load(Ordering::SeqCst);
    let started = std::time::Instant::now();
    let mut parallel = Vec::new();
    for route in routes.clone() {
        parallel.push(tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if route.run_once().await.unwrap() {
                        break true;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap()
        }));
    }
    for handle in parallel {
        assert!(handle.await.unwrap());
    }
    assert_eq!(calls.load(Ordering::SeqCst) - before, 5);
    assert!(started.elapsed() < Duration::from_secs(60));
    lookup(&pool, "70000005").await.unwrap();
    for route in routes {
        assert!(!route.run_once().await.unwrap());
    }
    assert_eq!(calls.load(Ordering::SeqCst) - before, 5);
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM kvk_cache.rate_limit WHERE egress_key LIKE 'fixture-egress-%' AND next_allowed_at>now()+interval '55 seconds'").fetch_one(&pool).await.unwrap(),5);
    // The application role can operate the queue; browser roles cannot read it.
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("SET ROLE gateway_v2_runtime")
        .execute(&mut *conn)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM kvk_cache.worker_slots")
            .fetch_one(&mut *conn)
            .await
            .unwrap(),
        5
    );
    assert!(matches!(
        lookup_on(&mut conn, "78888888").await.unwrap(),
        Lookup::Pending { .. }
    ));
    sqlx::query("UPDATE kvk_cache.jobs SET available_at=now() WHERE kvk_number='78888888'")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("RESET ROLE").execute(&mut *conn).await.unwrap();
    for role in ["anon", "authenticated"] {
        sqlx::query(&format!("SET ROLE {role}"))
            .execute(&mut *conn)
            .await
            .unwrap();
        assert!(sqlx::query("SELECT * FROM kvk_cache.annual_accounts")
            .fetch_all(&mut *conn)
            .await
            .is_err());
        sqlx::query("RESET ROLE").execute(&mut *conn).await.unwrap();
    }
    server.abort();
    println!("Verified: 10 requests / 1 upstream call; five distinct cold results in under one minute on five fixture egress budgets; sixth waits; 3 versions, Retry-After, recovery, retry exhaustion and private roles.");
}
