//! Explicit operator source test. Uses live workers; no buyer/billing mutations.
//! cargo run --example kvk_queue_live -- companies.json output/report.json
use anyhow::{ensure, Context};
use chrono::Utc;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{postgres::PgPoolOptions, PgPool, Row};
use std::{
    collections::HashSet,
    path::PathBuf,
    time::{Duration, Instant},
};

async fn snapshot(pool: &PgPool, numbers: &[String]) -> anyhow::Result<Value> {
    let jobs: Value = sqlx::query_scalar("SELECT coalesce(jsonb_agg(to_jsonb(j) ORDER BY kvk_number),'[]'::jsonb) FROM kvk_cache.jobs j WHERE kvk_number=ANY($1)")
        .bind(numbers).fetch_one(pool).await?;
    let attempts: Value = sqlx::query_scalar("SELECT coalesce(jsonb_agg(to_jsonb(a) ORDER BY started_at),'[]'::jsonb) FROM kvk_cache.fetch_attempts a WHERE kvk_number=ANY($1)")
        .bind(numbers).fetch_one(pool).await?;
    Ok(json!({"jobs":jobs,"attempts":attempts}))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let mut args = std::env::args().skip(1);
    let company_file = args.next().context("provide companies JSON file")?;
    let output = PathBuf::from(args.next().context("provide output JSON path")?);
    let companies: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(company_file)?)?;
    ensure!(
        !companies.is_empty() && companies.len() <= 5,
        "live batch must contain one to five companies"
    );
    let numbers: Vec<String> = companies
        .iter()
        .map(|c| {
            c["kvk_number"]
                .as_str()
                .context("missing KVK number")
                .map(str::to_owned)
        })
        .collect::<Result<_, _>>()?;
    for n in &numbers {
        kvk_accounts::validate_number(n)?;
    }
    ensure!(
        numbers.iter().collect::<HashSet<_>>().len() == numbers.len(),
        "companies must be distinct"
    );
    let database = std::env::var("DATABASE_URL")?;
    let u = url::Url::parse(&database)?;
    ensure!(
        u.host_str() == Some("aws-1-eu-west-1.pooler.supabase.com")
            && u.username() == "gateway_v2_server.jgjoiyqdyypouskftzeq",
        "unexpected database identity"
    );
    let pool = PgPoolOptions::new()
        .max_connections(6)
        .connect(&database)
        .await?;
    let existing: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM kvk_cache.jobs WHERE kvk_number=ANY($1) AND status='succeeded'",
    )
    .bind(&numbers)
    .fetch_one(&pool)
    .await?;
    ensure!(existing==0,"cold-test companies already have saved results; choose new companies, never delete cached evidence");
    let baseline = snapshot(&pool, &numbers).await?;
    let mut tx = pool.begin().await?;
    for number in &numbers {
        ensure!(
            matches!(
                kvk_accounts::lookup_on(&mut tx, number).await?,
                kvk_accounts::Lookup::Pending { .. }
            ),
            "expected cold miss"
        );
    }
    // A brief deliberate schedule verifies durable queue waiting before claims.
    sqlx::query("UPDATE kvk_cache.jobs SET available_at=now()+interval '5 seconds' WHERE kvk_number=ANY($1)").bind(&numbers).execute(&mut *tx).await?;
    let started_at = Utc::now();
    let clock = Instant::now();
    tx.commit().await?;
    let mut duplicates = Vec::new();
    for _ in 0..10 {
        let p = pool.clone();
        let n = numbers[0].clone();
        duplicates.push(tokio::spawn(
            async move { kvk_accounts::lookup(&p, &n).await },
        ));
    }
    for duplicate in duplicates {
        duplicate.await??;
    }
    let queued = snapshot(&pool, &numbers).await?;
    ensure!(
        queued["jobs"].as_array().unwrap().len() == numbers.len(),
        "single-flight created duplicate jobs"
    );
    println!(
        "Queued {} distinct companies plus 10 duplicate requests",
        numbers.len()
    );
    let deadline = Instant::now() + Duration::from_secs(420);
    let mut final_state;
    let mut previous = String::new();
    loop {
        final_state = snapshot(&pool, &numbers).await?;
        let summary = final_state["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|j| {
                format!(
                    "{}:{}",
                    j["kvk_number"].as_str().unwrap(),
                    j["status"].as_str().unwrap()
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        if summary != previous {
            println!("{:.1}s {summary}", clock.elapsed().as_secs_f64());
            previous = summary;
        }
        if final_state["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|j| matches!(j["status"].as_str(), Some("succeeded" | "failed")))
            || Instant::now() >= deadline
        {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let completion_seconds = clock.elapsed().as_secs_f64();
    let mut documents = Vec::new();
    for company in &companies {
        let number = company["kvk_number"].as_str().unwrap();
        if let Some(row)=sqlx::query("SELECT a.financial_year,a.version,a.payload,a.source_url,a.fetched_at FROM kvk_cache.jobs j JOIN kvk_cache.annual_accounts a USING(kvk_number,financial_year,document_type,version) WHERE j.kvk_number=$1 AND j.status='succeeded'").bind(number).fetch_optional(&pool).await? {
            let data: Value=row.get("payload");let year:i32=row.get("financial_year");let version:String=row.get("version");
            ensure!(kvk_accounts::financial_year(&data)?==year,"stored year differs from source");
            ensure!(hex::encode(Sha256::digest(serde_json::to_vec(&data)?))==version,"stored document hash differs from content");
            documents.push(json!({"company":company,"financial_year":year,"version":version,"source_url":row.get::<String,_>("source_url"),"fetched_at":row.get::<chrono::DateTime<Utc>,_>("fetched_at"),"payload":data}));
        }
    }
    let before_replay = final_state["attempts"].as_array().unwrap().len();
    for _ in 0..3 {
        for number in &numbers {
            let _ = kvk_accounts::lookup(&pool, number).await?;
        }
    }
    tokio::time::sleep(Duration::from_secs(3)).await;
    let after = snapshot(&pool, &numbers).await?;
    let no_extra_calls = before_replay == after["attempts"].as_array().unwrap().len();
    let old_ids: HashSet<_> = baseline["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["id"].as_str())
        .collect();
    let run_attempts: Vec<_> = after["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| !old_ids.contains(a["id"].as_str().unwrap()))
        .cloned()
        .collect();
    let egress_ips: HashSet<_> = run_attempts
        .iter()
        .filter_map(|a| a["egress_key"].as_str())
        .collect();
    let report = json!({"started_at":started_at,"completed_at":Utc::now(),"companies":companies,"run_attempts":run_attempts,"baseline_snapshot":baseline,"queued_snapshot":queued,"final_snapshot":after,"documents":documents,"completion_seconds":completion_seconds,"distinct_egress_ips":egress_ips,"cache_replay_added_no_upstream_calls":no_extra_calls,"initial_duplicate_requests":10,"replay_requests":numbers.len()*3,"billing":"no buyer, reservation or payment mutations"});
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!("Stored {}/{} documents; {} outbound IPs; repeat requests added no upstream calls: {}; evidence: {}",documents.len(),numbers.len(),egress_ips.len(),no_extra_calls,output.display());
    ensure!(
        documents.len() == numbers.len(),
        "some KVK reports were unavailable; see saved evidence"
    );
    ensure!(
        no_extra_calls,
        "cache replay caused additional upstream work"
    );
    ensure!(
        run_attempts.len() == numbers.len(),
        "cold batch did not use exactly one upstream fetch per company"
    );
    // Throughput is reported, never inferred from worker or address count.
    Ok(())
}
