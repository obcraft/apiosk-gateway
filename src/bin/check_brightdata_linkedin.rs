//! Operator-only bounded checks. Never prints credentials, URLs, records,
//! snapshot identifiers, upstream bodies or database diagnostics.
use reqwest::{header::HeaderValue, StatusCode};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{
    postgres::{PgConnectOptions, PgPoolOptions, PgSslMode},
    ConnectOptions, PgPool,
};
use std::{
    env,
    process::ExitCode,
    str::FromStr,
    time::{Duration, Instant},
};
use uuid::Uuid;

const PURCHASE_URL: &str = "http://127.0.0.1:8082/v1/do";
// EUR 0.05 at the billing rate EUR 0.92/USD configured on 2026-08-21.
// The underlying purchase and its hard ceiling remain denominated in USD.
const PURCHASE_CEILING_USDC: f64 = 0.054347;
const MAX_PURCHASE_RESPONSE_BYTES: usize = 5 * 1024 * 1024;

#[derive(Clone, Copy)]
enum Kind {
    Profile,
    Company,
    Job,
    Post,
}

impl Kind {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "profile" => Some(Self::Profile),
            "company" => Some(Self::Company),
            "job" => Some(Self::Job),
            "post" => Some(Self::Post),
            _ => None,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Profile => "profile",
            Self::Company => "company",
            Self::Job => "job",
            Self::Post => "post",
        }
    }
    fn path(self) -> &'static str {
        match self {
            Self::Profile => "/linkedin/profile",
            Self::Company => "/linkedin/company",
            Self::Job => "/linkedin/job",
            Self::Post => "/linkedin/post",
        }
    }
    fn url_prefix(self) -> &'static str {
        match self {
            Self::Profile => "/in/",
            Self::Company => "/company/",
            Self::Job => "/jobs/view/",
            Self::Post => "/posts/",
        }
    }
}

struct Arguments {
    kind: Kind,
    url: String,
    purchase: bool,
}

fn parse_arguments(arguments: &[String]) -> Option<Arguments> {
    if arguments.len() != 2 && !(arguments.len() == 3 && arguments[2] == "--purchase") {
        return None;
    }
    let kind = Kind::parse(&arguments[0])?;
    let raw = &arguments[1];
    if raw.len() > 2048 || raw.bytes().any(|b| b.is_ascii_whitespace() || b == b'\\') {
        return None;
    }
    let url = url::Url::parse(raw).ok()?;
    if url.scheme() != "https"
        || !matches!(url.host_str(), Some("linkedin.com" | "www.linkedin.com"))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let path = url.path().strip_suffix('/').unwrap_or(url.path());
    let slug = path.strip_prefix(kind.url_prefix())?;
    if slug.is_empty()
        || !slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        || (matches!(kind, Kind::Job) && !slug.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    Some(Arguments {
        kind,
        url: format!("https://www.linkedin.com{path}"),
        purchase: arguments.len() == 3,
    })
}

#[derive(Serialize)]
struct Summary {
    kind: Option<&'static str>,
    mode: &'static str,
    status: &'static str,
    code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    http_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    execution_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    record_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    coverage_complete: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    did: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    paid: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    charged_usdc: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tx_present: Option<bool>,
    elapsed_ms: u128,
}

impl Summary {
    fn failed(args: Option<&Arguments>, code: &'static str) -> Self {
        Self {
            kind: args.map(|a| a.kind.name()),
            mode: if args.is_some_and(|a| a.purchase) {
                "purchase"
            } else {
                "collect"
            },
            status: "failed",
            code,
            http_status: None,
            execution_status: None,
            record_count: None,
            coverage_complete: None,
            did: None,
            paid: None,
            charged_usdc: None,
            request_id: None,
            tx_present: None,
            elapsed_ms: 0,
        }
    }
    fn exit_code(&self) -> ExitCode {
        ExitCode::from(match self.status {
            "ready" => 0,
            "pending" => 2,
            _ => 1,
        })
    }
}

fn database_options(raw: &str) -> Option<PgConnectOptions> {
    let database_url = url::Url::parse(raw).ok()?;
    let mut options = PgConnectOptions::from_str(raw).ok()?;
    // Identical remote TLS verification to the gateway. Parsing the original
    // URL preserves its configured sslrootcert (including the deployed CA).
    if !matches!(
        database_url.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]")
    ) {
        options = options.ssl_mode(PgSslMode::VerifyFull);
    }
    Some(options.disable_statement_logging())
}

fn collection_summary(
    args: &Arguments,
    result: Result<Value, brightdata_linkedin::Failure>,
) -> Summary {
    match result {
        Ok(result) => {
            let mut summary = Summary::failed(Some(args), "brightdata_ready");
            summary.status = "ready";
            summary.http_status = Some(200);
            summary.record_count = result.get("data").and_then(Value::as_array).map(Vec::len);
            summary.coverage_complete = result
                .get("coverage")
                .and_then(|v| v.get("complete"))
                .and_then(Value::as_bool);
            summary
        }
        Err(error) => {
            let code = match error.code {
                "invalid_linkedin_request" => "invalid_linkedin_request",
                "brightdata_pending" => "brightdata_pending",
                "brightdata_submission_unknown" => "brightdata_submission_unknown",
                "brightdata_collection_failed" => "brightdata_collection_failed",
                _ => "brightdata_unavailable",
            };
            let mut summary = Summary::failed(Some(args), code);
            summary.http_status = Some(error.status.as_u16());
            if code == "brightdata_pending" {
                summary.status = "pending";
            }
            summary
        }
    }
}

fn request_key(args: &Arguments) -> String {
    hex::encode(Sha256::digest(format!(
        "{}\n{}",
        args.kind.name(),
        args.url
    )))
}

fn purchase_summary(args: &Arguments, status: StatusCode, result: &Value) -> Summary {
    let execution = &result["execution"];
    let mut summary = Summary::failed(Some(args), "purchase_unconfirmed");
    summary.http_status = Some(status.as_u16());
    summary.execution_status = execution
        .get("status")
        .and_then(Value::as_u64)
        .and_then(|status| u16::try_from(status).ok());
    summary.did = result.get("did").and_then(Value::as_bool);
    summary.paid = execution.get("paid").and_then(Value::as_bool);
    summary.charged_usdc = execution
        .get("charged_usdc")
        .and_then(|value| {
            value
                .as_f64()
                .or_else(|| value.as_str().and_then(|v| v.parse::<f64>().ok()))
        })
        .filter(|value| value.is_finite() && *value >= 0.0);
    summary.request_id = execution
        .get("request_id")
        .and_then(Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok())
        .filter(|id| !id.is_nil());
    summary.tx_present = Some(
        execution
            .get("tx_hash")
            .and_then(Value::as_str)
            .is_some_and(|hash| {
                hash.len() == 66
                    && hash.starts_with("0x")
                    && hash[2..].bytes().all(|b| b.is_ascii_hexdigit())
            }),
    );
    if status == StatusCode::OK
        && summary.did == Some(true)
        && summary.paid == Some(true)
        && summary
            .execution_status
            .is_some_and(|status| (200..300).contains(&status))
        && summary.request_id.is_some()
        && execution.get("error").is_none_or(Value::is_null)
        && summary
            .charged_usdc
            .is_some_and(|price| price > 0.0 && price <= PURCHASE_CEILING_USDC + 1e-12)
    {
        summary.status = "ready";
        summary.code = "purchase_verified";
    }
    summary
}

async fn purchase(pool: &PgPool, args: &Arguments) -> Summary {
    // This mode NEVER starts/resumes provider collection. Operator must first
    // obtain a ready record using the default mode, then activate the source.
    let ready: Result<bool, _> = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM gateway_private.brightdata_linkedin_jobs WHERE request_key=$1 AND kind=$2 AND source_url=$3 AND state='ready' AND result IS NOT NULL AND expires_at>now())")
        .bind(request_key(args)).bind(args.kind.name()).bind(&args.url).fetch_one(pool).await;
    match ready {
        Ok(true) => {}
        Ok(false) => return Summary::failed(Some(args), "cache_not_ready"),
        Err(_) => return Summary::failed(Some(args), "database_unavailable"),
    }
    let endpoints: Result<Vec<Uuid>, _> = sqlx::query_scalar("SELECT e.id FROM providers.api_endpoints e JOIN providers.apis a ON a.id=e.api_id WHERE a.slug='brightdata' AND a.status='active' AND a.active=true AND e.active=true AND e.method='POST' AND e.path=$1 AND e.price=0.047258 LIMIT 2")
        .bind(args.kind.path()).fetch_all(pool).await;
    let endpoint = match endpoints {
        Ok(ids) if ids.len() == 1 => ids[0],
        Ok(_) => return Summary::failed(Some(args), "active_endpoint_unavailable"),
        Err(_) => return Summary::failed(Some(args), "database_unavailable"),
    };
    let key = match env::var("APIOSK_APP_SERVICE_KEY") {
        Ok(key)
            if !key.is_empty()
                && key.len() <= 4096
                && key.bytes().all(|b| b.is_ascii_graphic()) =>
        {
            key
        }
        _ => return Summary::failed(Some(args), "service_key_unavailable"),
    };
    let mut header = match HeaderValue::from_str(&key) {
        Ok(header) => header,
        Err(_) => return Summary::failed(Some(args), "service_key_unavailable"),
    };
    header.set_sensitive(true);
    let client = match reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(100))
        .build()
    {
        Ok(client) => client,
        Err(_) => return Summary::failed(Some(args), "purchase_unavailable"),
    };
    let response = client.post(PURCHASE_URL).header("X-Apiosk-Service-Key", header)
        .header("Idempotency-Key", format!("brightdata-probe-20261008-{}", request_key(args)))
        .json(&json!({"candidate_id":endpoint,"job":"Read-only source integration verification", "inputs":{"url":args.url},
            "deterministic":true,"parse":false,"max_price":PURCHASE_CEILING_USDC,"max_price_usdc":PURCHASE_CEILING_USDC,"dry_run":false,"timeout_ms":90000}))
        .send().await;
    let mut response = match response {
        Ok(response) => response,
        Err(_) => return Summary::failed(Some(args), "purchase_unconfirmed"),
    };
    let status = response.status();
    if response
        .content_length()
        .is_some_and(|size| size > MAX_PURCHASE_RESPONSE_BYTES as u64)
    {
        return Summary::failed(Some(args), "purchase_response_too_large");
    }
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk))
                if body.len().saturating_add(chunk.len()) <= MAX_PURCHASE_RESPONSE_BYTES =>
            {
                body.extend_from_slice(&chunk)
            }
            Ok(Some(_)) => return Summary::failed(Some(args), "purchase_response_too_large"),
            Ok(None) => break,
            Err(_) => return Summary::failed(Some(args), "purchase_unconfirmed"),
        }
    }
    match serde_json::from_slice(&body) {
        Ok(result) => purchase_summary(args, status, &result),
        Err(_) => Summary::failed(Some(args), "purchase_response_invalid"),
    }
}

async fn run(args: &Arguments) -> Summary {
    let database = match env::var("DATABASE_URL") {
        Ok(database) => database,
        Err(_) => return Summary::failed(Some(args), "database_not_configured"),
    };
    let options = match database_options(&database) {
        Some(options) => options,
        None => return Summary::failed(Some(args), "database_configuration_invalid"),
    };
    let pool = match PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .after_connect(|connection, _| {
            Box::pin(async move {
                sqlx::query("SET statement_timeout='5s'")
                    .execute(&mut *connection)
                    .await?;
                sqlx::query("SET lock_timeout='3s'")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect_with(options)
        .await
    {
        Ok(pool) => pool,
        Err(_) => return Summary::failed(Some(args), "database_unavailable"),
    };
    let summary = if args.purchase {
        purchase(&pool, args).await
    } else {
        let body = json!({"url":args.url}).to_string();
        collection_summary(
            args,
            brightdata_linkedin::answer(&pool, "POST", args.kind.path(), None, body.as_bytes())
                .await,
        )
    };
    let _ = tokio::time::timeout(Duration::from_secs(2), pool.close()).await;
    summary
}

#[tokio::main]
async fn main() -> ExitCode {
    // No tracing subscriber and no dotenv loading: the operator process uses
    // only its inherited server secrets and emits one allowlisted JSON summary.
    let started = Instant::now();
    let Some(args) = parse_arguments(&env::args().skip(1).collect::<Vec<_>>()) else {
        let mut summary = Summary::failed(None, "invalid_arguments");
        summary.elapsed_ms = started.elapsed().as_millis();
        println!(
            "{}",
            serde_json::to_string(&summary).expect("serializable summary")
        );
        return ExitCode::from(64);
    };
    let budget = Duration::from_secs(if args.purchase { 110 } else { 28 });
    let mut summary = match tokio::time::timeout(budget, run(&args)).await {
        Ok(summary) => summary,
        Err(_) => Summary::failed(Some(&args), "probe_timeout"),
    };
    summary.elapsed_ms = started.elapsed().as_millis();
    println!(
        "{}",
        serde_json::to_string(&summary).expect("serializable summary")
    );
    summary.exit_code()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(kind: &str, url: &str, purchase: bool) -> Option<Arguments> {
        let mut arguments = vec![kind.into(), url.into()];
        if purchase {
            arguments.push("--purchase".into());
        }
        parse_arguments(&arguments)
    }
    #[test]
    fn accepts_only_single_matching_public_urls_and_stable_purchase_identity() {
        let first = args("company", "https://linkedin.com/company/example/", true).unwrap();
        let second = args("company", "https://www.linkedin.com/company/example", true).unwrap();
        assert_eq!(request_key(&first), request_key(&second));
        for (kind, url) in [
            ("profile", "https://www.linkedin.com/in/example"),
            ("job", "https://www.linkedin.com/jobs/view/12345"),
            ("post", "https://www.linkedin.com/posts/example-123"),
        ] {
            assert!(args(kind, url, false).is_some());
        }
        for url in [
            "http://www.linkedin.com/company/example",
            "https://evil.test/company/example",
            "https://www.linkedin.com/company/example?x=1",
            "https://key@www.linkedin.com/company/example",
            "https://www.linkedin.com/in/example",
            "https://www.linkedin.com/company/example/path",
        ] {
            assert!(args("company", url, false).is_none());
        }
        assert!(parse_arguments(&[
            "company".into(),
            "https://www.linkedin.com/company/example".into(),
            "--purchase".into(),
            "--purchase".into()
        ])
        .is_none());
    }
    #[test]
    fn enforces_remote_verify_full_and_preserves_ca_from_database_url() {
        let options = database_options("postgres://fixture:secret@db.example.test/db?sslmode=disable&sslrootcert=%2Fetc%2Fssl%2Fcerts%2Fsupabase-prod-ca-2021.crt").unwrap();
        assert!(matches!(options.get_ssl_mode(), PgSslMode::VerifyFull));
        assert!(options
            .to_url_lossy()
            .query_pairs()
            .any(|(key, value)| key == "sslrootcert"
                && value == "file: /etc/ssl/certs/supabase-prod-ca-2021.crt"));
        assert!(matches!(
            database_options("postgres://fixture@127.0.0.1/db?sslmode=disable")
                .unwrap()
                .get_ssl_mode(),
            PgSslMode::Disable
        ));
        assert!(database_options("invalid database secret").is_none());
    }
    #[test]
    fn summaries_never_include_record_contents_secrets_urls_or_snapshot_ids() {
        let args = args("company", "https://www.linkedin.com/company/example", false).unwrap();
        let summary = collection_summary(
            &args,
            Ok(
                json!({"data":[{"name":"PRIVATE_RECORD", "snapshot_id":"sd_PRIVATE", "secret":"PRIVATE_KEY"}],"source_url":"PRIVATE_URL","coverage":{"complete":false,"scope":"PRIVATE_SCOPE"}}),
            ),
        );
        let output = serde_json::to_string(&summary).unwrap();
        assert!(!output.contains("PRIVATE"));
        assert_eq!(summary.record_count, Some(1));
        let pending = collection_summary(
            &args,
            Err(brightdata_linkedin::Failure {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "brightdata_pending",
                message: "PRIVATE_MESSAGE",
                retry_after: Some("10"),
            }),
        );
        assert_eq!(pending.status, "pending");
        let unknown = collection_summary(
            &args,
            Err(brightdata_linkedin::Failure {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "brightdata_submission_unknown",
                message: "PRIVATE_MESSAGE",
                retry_after: Some("60"),
            }),
        );
        assert_eq!(unknown.status, "failed");
    }
    #[test]
    fn purchase_summary_accepts_internal_settlement_without_onchain_transaction() {
        let args = args("company", "https://www.linkedin.com/company/example", true).unwrap();
        let mut body = json!({"did":true,"secret":"PRIVATE_KEY","source_url":"PRIVATE_URL","execution":{"paid":true,"status":200,"payment_rail":"apiosk_balance","charged_usdc":"0.054347","request_id":"00000000-0000-4000-8000-000000000001","tx_hash":null,"error":null,"data":"PRIVATE_RECORD"}});
        let summary = purchase_summary(&args, StatusCode::OK, &body);
        assert_eq!(summary.status, "ready");
        assert_eq!(summary.tx_present, Some(false));
        assert_eq!(summary.execution_status, Some(200));
        assert!(!serde_json::to_string(&summary).unwrap().contains("PRIVATE"));
        for invalid_price in [
            json!(0),
            json!(-0.01),
            json!(0.054348),
            json!("NaN"),
            json!("Infinity"),
        ] {
            body["execution"]["charged_usdc"] = invalid_price;
            assert_eq!(
                purchase_summary(&args, StatusCode::OK, &body).status,
                "failed"
            );
        }
        body["execution"]["charged_usdc"] = json!(0.054347);
        for status in [json!(503), json!("200"), Value::Null] {
            body["execution"]["status"] = status;
            assert_eq!(
                purchase_summary(&args, StatusCode::OK, &body).status,
                "failed"
            );
        }
        body["execution"]["status"] = json!(200);
        body["execution"]["error"] = json!("payment_unresolved");
        assert_eq!(
            purchase_summary(&args, StatusCode::OK, &body).status,
            "failed"
        );
        body["execution"]["error"] = Value::Null;
        body["execution"]["charged_usdc"] = json!("PRIVATE_KEY");
        body["execution"]["request_id"] = json!("PRIVATE_KEY");
        let summary = purchase_summary(&args, StatusCode::OK, &body);
        assert!(summary.charged_usdc.is_none());
        assert!(summary.request_id.is_none());
        assert!(!serde_json::to_string(&summary).unwrap().contains("PRIVATE"));
        body["execution"]["charged_usdc"] = json!(0.054347);
        assert_eq!(
            purchase_summary(&args, StatusCode::OK, &body).status,
            "failed"
        );
    }
}
