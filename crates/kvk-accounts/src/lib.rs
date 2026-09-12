//! Durable KVK source cache. Contains no buyer tokens, payment IDs or balances.
use anyhow::{ensure, Context};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool, Row};
use std::time::Duration;
use uuid::Uuid;

pub const PROVIDER: &str = "kvk-dutch-business-register";
pub const ENDPOINT: &str = "/api/v1/hvds/jaarrekeningen/kvknummer";
pub const DOCUMENT_TYPE: &str = "kvk-hvds-xbrl-v1";
const ORIGIN: &str = "https://opendata.kvk.nl";
const MAX_BODY: usize = 4 * 1024 * 1024;

pub enum Lookup {
    Ready(Value),
    Pending { until: DateTime<Utc> },
    Failed { code: String },
}

pub fn validate_number(number: &str) -> anyhow::Result<()> {
    ensure!(
        number.len() == 8 && number.bytes().all(|b| b.is_ascii_digit()),
        "invalid KVK number"
    );
    Ok(())
}

/// Only the reviewed public route is eligible. Unknown queries cannot silently
/// collapse into a latest-report key or be forwarded around the shared limiter.
pub fn number_from_path(path: &str) -> anyhow::Result<&str> {
    let number = path
        .strip_prefix("/api/v1/hvds/jaarrekeningen/kvknummer/")
        .context("invalid annual accounts route")?;
    validate_number(number)?;
    Ok(number)
}

pub fn financial_year(data: &Value) -> anyhow::Result<i32> {
    let fields = data["opendataFields"]
        .as_array()
        .context("missing opendataFields")?;
    let years: Vec<_> = fields
        .iter()
        .filter(|f| f["key"] == "FinancialYear")
        .collect();
    ensure!(years.len() == 1, "expected one financial year");
    let year = match &years[0]["value"] {
        Value::String(s) => s.parse::<i32>()?,
        Value::Number(n) => i32::try_from(n.as_i64().context("invalid year")?)?,
        _ => anyhow::bail!("invalid financial year"),
    };
    ensure!((1900..=2200).contains(&year), "invalid financial year");
    Ok(year)
}

/// Insert-on-conflict is the single-flight boundary across all callers.
/// A failed lookup has a short negative TTL; permanent documents never expire.
pub async fn lookup(pool: &PgPool, number: &str) -> anyhow::Result<Lookup> {
    lookup_on(&mut *pool.acquire().await?, number).await
}

/// Allows gateway admission to reuse its authorization transaction/connection.
pub async fn lookup_on(conn: &mut PgConnection, number: &str) -> anyhow::Result<Lookup> {
    validate_number(number)?;
    sqlx::query("INSERT INTO kvk_cache.jobs(kvk_number,status) VALUES($1,'queued') ON CONFLICT(kvk_number) DO UPDATE SET status='queued',attempts=0,error_code=NULL,available_at=now(),fresh_until=NULL,updated_at=now() WHERE jobs.status IN ('succeeded','failed') AND jobs.fresh_until<=now()")
        .bind(number).execute(&mut *conn).await?;
    let row = sqlx::query("SELECT j.status,j.error_code,a.payload FROM kvk_cache.jobs j LEFT JOIN kvk_cache.annual_accounts a USING(kvk_number,financial_year,document_type,version) WHERE j.kvk_number=$1")
        .bind(number).fetch_one(&mut *conn).await?;
    Ok(match row.get::<String, _>("status").as_str() {
        "succeeded" => Lookup::Ready(row.get("payload")),
        "failed" => Lookup::Failed {
            code: row
                .get::<Option<String>, _>("error_code")
                .unwrap_or_else(|| "kvk_unavailable".into()),
        },
        _ => Lookup::Pending {
            until: Utc::now() + chrono::Duration::seconds(2),
        },
    })
}

/// Historical reads are exact and never substituted with another year's data.
pub async fn document(
    pool: &PgPool,
    number: &str,
    year: i32,
    version: &str,
) -> anyhow::Result<Option<Value>> {
    validate_number(number)?;
    Ok(sqlx::query_scalar("SELECT payload FROM kvk_cache.annual_accounts WHERE kvk_number=$1 AND financial_year=$2 AND document_type=$3 AND version=$4")
        .bind(number).bind(year).bind(DOCUMENT_TYPE).bind(version).fetch_optional(pool).await?)
}

#[derive(Clone)]
pub struct Workers {
    pool: PgPool,
    http: reqwest::Client,
    origin: String,
    egress_key: String,
}

struct Claim {
    number: String,
    token: Uuid,
    attempts: i32,
}
enum Outcome {
    Success(Value),
    Failure {
        code: String,
        retry: bool,
        delay: i64,
    },
}

impl Workers {
    pub fn new(pool: PgPool) -> anyhow::Result<Self> {
        Ok(Self {
            pool,
            origin: ORIGIN.into(),
            egress_key: "shared-default".into(),
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(30))
                .build()?,
        })
    }

    /// KVK_EGRESS_ROUTES is a secret JSON array (one to five fixed routes).
    /// Each entry has expected_ipv4 and optionally proxy_url. Separate machines
    /// can each configure one direct route; one machine can configure five proxies.
    /// Production routes share one provider rate budget.
    pub async fn configured(pool: PgPool) -> anyhow::Result<Vec<Self>> {
        let Ok(raw) = std::env::var("KVK_EGRESS_ROUTES") else {
            tracing::warn!("KVK has no separate egress routes configured: capacity remains one cold lookup per minute");
            return Ok(vec![Self::new(pool)?]);
        };
        let routes: Vec<Value> = serde_json::from_str(&raw)
            .map_err(|_| anyhow::anyhow!("invalid KVK_EGRESS_ROUTES JSON"))?;
        ensure!(
            !routes.is_empty() && routes.len() <= 5,
            "configure one to five KVK egress routes"
        );
        let mut seen = std::collections::HashSet::new();
        let mut workers = Vec::new();
        for route in routes {
            ensure!(
                route.as_object().is_some_and(|m| m
                    .keys()
                    .all(|key| matches!(key.as_str(), "expected_ipv4" | "proxy_url"))),
                "invalid KVK route fields"
            );
            ensure!(
                route.get("proxy_url").is_none_or(Value::is_string),
                "KVK proxy_url must be a URL string"
            );
            let expected: std::net::Ipv4Addr = route["expected_ipv4"]
                .as_str()
                .context("KVK route requires expected_ipv4")?
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid expected KVK egress IPv4"))?;
            ensure!(
                !expected.is_private() && !expected.is_loopback() && !expected.is_unspecified(),
                "KVK egress must be public"
            );
            let mut builder = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .local_address(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(30));
            if let Some(proxy) = route["proxy_url"].as_str() {
                ensure!(
                    proxy.starts_with("https://") || proxy.starts_with("http://"),
                    "KVK requires an HTTP CONNECT proxy"
                );
                builder = builder.proxy(
                    reqwest::Proxy::all(proxy)
                        .map_err(|_| anyhow::anyhow!("invalid KVK proxy configuration"))?,
                );
            }
            let http = builder.build()?;
            // Errors deliberately exclude the proxy URL, which may carry credentials.
            let response = http
                .get("https://api.ipify.org?format=json")
                .timeout(Duration::from_secs(10))
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("KVK egress verification failed"))?
                .error_for_status()
                .map_err(|_| anyhow::anyhow!("KVK egress verification failed"))?;
            let observed: Value = response
                .json()
                .await
                .map_err(|_| anyhow::anyhow!("invalid egress verification response"))?;
            ensure!(
                observed["ip"].as_str() == Some(expected.to_string().as_str()),
                "KVK egress does not match the configured fixed IPv4"
            );
            ensure!(
                seen.insert(expected),
                "KVK routes contain a duplicate egress IP"
            );
            workers.push(Self {
                pool: pool.clone(),
                http,
                origin: ORIGIN.into(),
                egress_key: expected.to_string(),
            });
        }
        tracing::info!(routes = workers.len(), "KVK fixed egress routes verified");
        Ok(workers)
    }

    // Production workers share one provider budget regardless of their route.
    // Independent fixture origins may model distinct budgets in offline tests.
    fn rate_key(&self) -> &str {
        if self.origin == ORIGIN {
            "shared-default"
        } else {
            &self.egress_key
        }
    }

    pub fn egress_identity(&self) -> &str {
        &self.egress_key
    }

    pub async fn check_schema(&self) -> anyhow::Result<()> {
        let slots: i64 = sqlx::query_scalar("SELECT count(*) FROM kvk_cache.worker_slots")
            .fetch_one(&self.pool)
            .await?;
        ensure!(slots == 5, "KVK queue migration requires five worker slots");
        sqlx::query(
            "INSERT INTO kvk_cache.rate_limit(egress_key) VALUES($1) ON CONFLICT DO NOTHING",
        )
        .bind(self.rate_key())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Workers share five durable slots; each verified IP has its own minute
    /// budget. Multiple processes using the same IP still share that budget.
    pub fn start(&self, count: usize) -> Vec<tokio::task::JoinHandle<()>> {
        (0..count)
            .map(|worker| {
                let this = self.clone();
                tokio::spawn(async move {
                    loop {
                        if let Err(error) = this.run_once().await {
                            tracing::warn!(worker, error=%error, "KVK queue worker failed");
                        }
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                })
            })
            .collect()
    }

    async fn claim(&self) -> anyhow::Result<Option<Claim>> {
        let mut tx = self.pool.begin().await?;
        // A default/unknown route must conservatively share every IP budget.
        // Serialize only admission so mixed configurations during rollout cannot
        // give the same actual IP both a default and a named allowance.
        let admission: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(724010603)")
            .fetch_one(&mut *tx)
            .await?;
        if !admission {
            return Ok(None);
        }
        let default_busy: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM kvk_cache.rate_limit WHERE ($1='shared-default' OR egress_key='shared-default') AND (next_allowed_at>now() OR active_until>now()))")
            .bind(self.rate_key()).fetch_one(&mut *tx).await?;
        if default_busy {
            return Ok(None);
        }
        // Lock ordering is identical for every claimant. No transaction spans HTTP.
        let rate = sqlx::query("SELECT egress_key FROM kvk_cache.rate_limit WHERE egress_key=$1 AND next_allowed_at<=now() AND (active_until IS NULL OR active_until<=now()) FOR UPDATE SKIP LOCKED")
            .bind(self.rate_key()).fetch_optional(&mut *tx).await?;
        if rate.is_none() {
            return Ok(None);
        }
        let slot: Option<i32> = sqlx::query_scalar("SELECT slot FROM kvk_cache.worker_slots WHERE lease_until IS NULL OR lease_until<=now() ORDER BY slot FOR UPDATE SKIP LOCKED LIMIT 1")
            .fetch_optional(&mut *tx).await?;
        let Some(slot) = slot else { return Ok(None) };
        let job = sqlx::query("SELECT kvk_number,attempts FROM kvk_cache.jobs WHERE (status='queued' AND available_at<=now()) OR (status='running' AND lease_until<=now()) ORDER BY available_at,created_at FOR UPDATE SKIP LOCKED LIMIT 1")
            .fetch_optional(&mut *tx).await?;
        let Some(job) = job else { return Ok(None) };
        let number: String = job.get("kvk_number");
        let attempts: i32 = job.get("attempts");
        // A process crash also consumes an attempt; do not retry forever.
        if attempts >= 5 {
            sqlx::query("UPDATE kvk_cache.jobs SET status='failed',error_code='kvk_retry_exhausted',fresh_until=now()+interval '5 minutes',lease_token=NULL,lease_until=NULL,updated_at=now() WHERE kvk_number=$1")
                .bind(number).execute(&mut *tx).await?;
            tx.commit().await?;
            return Ok(None);
        }
        sqlx::query("UPDATE kvk_cache.fetch_attempts SET status='abandoned',completed_at=now(),error_code='lease_expired' WHERE kvk_number=$1 AND status='running'")
            .bind(&number).execute(&mut *tx).await?;
        let token = Uuid::new_v4();
        sqlx::query("UPDATE kvk_cache.rate_limit SET next_allowed_at=now()+interval '61 seconds',active_until=now()+interval '120 seconds',lease_token=$1 WHERE egress_key=$2")
            .bind(token).bind(self.rate_key()).execute(&mut *tx).await?;
        sqlx::query("UPDATE kvk_cache.worker_slots SET lease_token=$1,lease_until=now()+interval '120 seconds' WHERE slot=$2")
            .bind(token).bind(slot).execute(&mut *tx).await?;
        sqlx::query("UPDATE kvk_cache.jobs SET status='running',lease_token=$1,lease_until=now()+interval '120 seconds',attempts=attempts+1,updated_at=now() WHERE kvk_number=$2")
            .bind(token).bind(&number).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO kvk_cache.fetch_attempts(id,kvk_number,egress_key,attempt,status) VALUES($1,$2,$3,$4,'running')")
            .bind(token).bind(&number).bind(&self.egress_key).bind(attempts+1).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(Some(Claim {
            number,
            token,
            attempts: attempts + 1,
        }))
    }

    pub async fn run_once(&self) -> anyhow::Result<bool> {
        let Some(claim) = self.claim().await? else {
            return Ok(false);
        };
        let outcome = match self.fetch(&claim.number).await {
            Ok(outcome) => outcome,
            Err(_) => Outcome::Failure {
                code: "kvk_transport_or_schema_error".into(),
                retry: true,
                delay: 65,
            },
        };
        self.finish(&claim, outcome).await?;
        tracing::info!(kvk_number=%claim.number, egress_ip=%self.egress_key, attempt=claim.attempts, "KVK fetch attempt completed");
        Ok(true)
    }

    async fn fetch(&self, number: &str) -> anyhow::Result<Outcome> {
        let mut response = self
            .http
            .get(format!(
                "{}/api/v1/hvds/jaarrekeningen/kvknummer/{number}",
                self.origin
            ))
            .send()
            .await?;
        let status = response.status().as_u16();
        if status != 200 {
            let delay = response
                .headers()
                .get("retry-after")
                .and_then(|h| h.to_str().ok())
                .map(retry_after_seconds)
                .unwrap_or(65);
            return Ok(Outcome::Failure {
                code: format!("kvk_http_{status}"),
                retry: status == 429 || status >= 500,
                delay,
            });
        }
        ensure!(
            response
                .content_length()
                .is_none_or(|n| n <= MAX_BODY as u64),
            "KVK response too large"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                bytes.len().saturating_add(chunk.len()) <= MAX_BODY,
                "KVK response too large"
            );
            bytes.extend_from_slice(&chunk);
        }
        let data: Value = serde_json::from_slice(&bytes)?;
        financial_year(&data)?;
        Ok(Outcome::Success(data))
    }

    async fn finish(&self, claim: &Claim, outcome: Outcome) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        let active = sqlx::query("SELECT kvk_number FROM kvk_cache.jobs WHERE kvk_number=$1 AND status='running' AND lease_token=$2 AND lease_until>now() FOR UPDATE")
            .bind(&claim.number).bind(claim.token).fetch_optional(&mut *tx).await?;
        if active.is_none() {
            return Ok(());
        } // Fenced: an expired worker cannot overwrite a successor.
        let (attempt_status, error_code) = match &outcome {
            Outcome::Success(_) => ("succeeded", None),
            Outcome::Failure { code, .. } => ("failed", Some(code.as_str())),
        };
        sqlx::query("UPDATE kvk_cache.fetch_attempts SET status=$1,error_code=$2,completed_at=now() WHERE id=$3")
            .bind(attempt_status).bind(error_code).bind(claim.token).execute(&mut *tx).await?;
        let mut delay = 0;
        match outcome {
            Outcome::Success(data) => {
                let year = financial_year(&data)?;
                let version = hex::encode(Sha256::digest(serde_json::to_vec(&data)?));
                sqlx::query("INSERT INTO kvk_cache.annual_accounts(kvk_number,financial_year,document_type,version,payload,source_url) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING")
                    .bind(&claim.number).bind(year).bind(DOCUMENT_TYPE).bind(&version).bind(data)
                    .bind(format!("{ORIGIN}/api/v1/hvds/jaarrekeningen/kvknummer/{}", claim.number)).execute(&mut *tx).await?;
                sqlx::query("UPDATE kvk_cache.jobs SET status='succeeded',financial_year=$1,document_type=$2,version=$3,fresh_until=now()+interval '24 hours',error_code=NULL,lease_token=NULL,lease_until=NULL,updated_at=now() WHERE kvk_number=$4")
                    .bind(year).bind(DOCUMENT_TYPE).bind(version).bind(&claim.number).execute(&mut *tx).await?;
            }
            Outcome::Failure {
                code,
                retry,
                delay: requested,
            } => {
                delay = requested.max(65);
                let status = if retry && claim.attempts < 5 {
                    "queued"
                } else {
                    "failed"
                };
                sqlx::query("UPDATE kvk_cache.jobs SET status=$1,error_code=$2,available_at=now()+make_interval(secs=>$3::double precision),fresh_until=now()+interval '5 minutes',lease_token=NULL,lease_until=NULL,updated_at=now() WHERE kvk_number=$4")
                    .bind(status).bind(code).bind(delay as f64).bind(&claim.number).execute(&mut *tx).await?;
            }
        }
        // Success preserves shared start-to-start pacing. Retry-After extends
        // the provider budget across every production worker.
        sqlx::query("UPDATE kvk_cache.rate_limit SET next_allowed_at=greatest(next_allowed_at,now()+make_interval(secs=>$1::double precision)),active_until=NULL,lease_token=NULL WHERE egress_key=$3 AND lease_token=$2")
            .bind(delay as f64).bind(claim.token).bind(self.rate_key()).execute(&mut *tx).await?;
        sqlx::query("UPDATE kvk_cache.worker_slots SET lease_token=NULL,lease_until=NULL WHERE lease_token=$1")
            .bind(claim.token).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
}

fn retry_after_seconds(value: &str) -> i64 {
    value
        .parse::<i64>()
        .ok()
        .or_else(|| {
            DateTime::parse_from_rfc2822(value)
                .ok()
                .map(|at| (at.with_timezone(&Utc) - Utc::now()).num_seconds())
        })
        .unwrap_or(65)
        .max(65)
}

#[cfg(test)]
mod tests;
