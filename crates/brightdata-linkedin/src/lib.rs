//! One public LinkedIn URL per purchase. Durable claims prevent repeated paid
//! submissions when a request times out or a gateway restarts. Snapshot IDs and
//! provider credentials never cross the public boundary.
use reqwest::{header::HeaderValue, StatusCode};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use std::{sync::OnceLock, time::Duration};

pub const SOURCE: &str = "brightdata";
const ORIGIN: &str = "https://api.brightdata.com";
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_CONTROL_BYTES: usize = 64 * 1024;
const REQUEST_BUDGET: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Profile,
    Company,
    Job,
    Post,
}

impl Kind {
    fn parse(path: &str) -> Option<Self> {
        match path {
            "/linkedin/profile" => Some(Self::Profile),
            "/linkedin/company" => Some(Self::Company),
            "/linkedin/job" => Some(Self::Job),
            "/linkedin/post" => Some(Self::Post),
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

    fn dataset(self) -> &'static str {
        match self {
            Self::Profile => "gd_l1viktl72bvl7bjuj0",
            Self::Company => "gd_l1vikfnt1wgvvqz95w",
            Self::Job => "gd_lpfll7v5hcqtkxl6l",
            Self::Post => "gd_lyy3tktm25m4avu764",
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Self::Profile => "/in/",
            Self::Company => "/company/",
            Self::Job => "/jobs/view/",
            Self::Post => "/posts/",
        }
    }
}

#[derive(Debug)]
pub struct Failure {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: &'static str,
    pub retry_after: Option<&'static str>,
}

impl Failure {
    fn invalid() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_linkedin_request",
            message: "Use POST with exactly one url field containing a public HTTPS LinkedIn URL matching this route; query parameters and batch inputs are not supported.",
            retry_after: None,
        }
    }

    fn unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "brightdata_unavailable",
            message:
                "LinkedIn collection is temporarily unavailable. Retry the same request later.",
            retry_after: Some("10"),
        }
    }

    fn pending() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "brightdata_pending",
            message: "LinkedIn collection is in progress. Retry this same URL to retrieve the saved result; this response is not charged.",
            retry_after: Some("10"),
        }
    }

    fn unknown() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "brightdata_submission_unknown",
            message: "Collection submission could not be confirmed. It will not be submitted again automatically; operator reconciliation is required. This response is not charged.",
            retry_after: Some("60"),
        }
    }

    fn failed() -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            code: "brightdata_collection_failed",
            message: "The provider could not return this public LinkedIn record. This response is not charged.",
            retry_after: None,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestBody {
    url: String,
}

struct Input {
    kind: Kind,
    source_url: String,
    request_key: String,
}

fn prepare(method: &str, path: &str, query: Option<&str>, body: &[u8]) -> Result<Input, Failure> {
    if method != "POST" || query.is_some() || body.len() > 4096 {
        return Err(Failure::invalid());
    }
    let kind = Kind::parse(path).ok_or_else(Failure::invalid)?;
    let request: RequestBody = serde_json::from_slice(body).map_err(|_| Failure::invalid())?;
    let source_url = canonical_url(kind, &request.url).ok_or_else(Failure::invalid)?;
    let request_key = hex::encode(Sha256::digest(format!("{}\n{}", kind.name(), source_url)));
    Ok(Input {
        kind,
        source_url,
        request_key,
    })
}

fn canonical_url(kind: Kind, raw: &str) -> Option<String> {
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
    let slug = path.strip_prefix(kind.prefix())?;
    if slug.is_empty()
        || !slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        || (kind == Kind::Job && !slug.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    Some(format!("https://www.linkedin.com{path}"))
}

// A dedicated server secret: catalog metadata may be readable by API clients.
// Never accept buyer headers or a metadata credential as a fallback.
fn authorization(token: Option<&str>) -> Result<HeaderValue, Failure> {
    let token = token
        .filter(|token| {
            !token.is_empty() && token.len() <= 4096 && token.bytes().all(|b| b.is_ascii_graphic())
        })
        .ok_or_else(Failure::unavailable)?;
    let mut value =
        HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| Failure::unavailable())?;
    value.set_sensitive(true);
    Ok(value)
}

fn client() -> Result<&'static reqwest::Client, Failure> {
    static CLIENT: OnceLock<Result<reqwest::Client, ()>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .https_only(true)
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .timeout(Duration::from_secs(18))
                .connect_timeout(Duration::from_secs(5))
                .build()
                .map_err(|_| ())
        })
        .as_ref()
        .map_err(|_| Failure::unavailable())
}

fn trigger_url(kind: Kind) -> String {
    format!(
        "{ORIGIN}/datasets/v3/trigger?dataset_id={}&format=json&include_errors=true",
        kind.dataset()
    )
}

fn snapshot_id(value: &str) -> bool {
    (value
        .strip_prefix("s_")
        .or_else(|| value.strip_prefix("sd_"))
        .is_some_and(|suffix| !suffix.is_empty()))
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

struct Provider<'a> {
    client: &'a reqwest::Client,
    authorization: &'a HeaderValue,
    // Test-only local origin; production has no configurable origin override.
    #[cfg(test)]
    test_origin: Option<String>,
}

impl Provider<'_> {
    fn request(&self, method: reqwest::Method, url: String) -> reqwest::RequestBuilder {
        #[cfg(test)]
        let url = self
            .test_origin
            .as_ref()
            .map(|origin| url.replacen(ORIGIN, origin, 1))
            .unwrap_or(url);
        self.client
            .request(method, url)
            .header("authorization", self.authorization.clone())
    }
}

fn management_url(snapshot: &str, download: bool) -> Result<String, Failure> {
    if !snapshot_id(snapshot) {
        return Err(Failure::unavailable());
    }
    Ok(if download {
        format!("{ORIGIN}/datasets/v3/snapshot/{snapshot}?format=json")
    } else {
        format!("{ORIGIN}/datasets/v3/progress/{snapshot}")
    })
}

#[derive(FromRow)]
struct Job {
    state: String,
    snapshot_id: Option<String>,
    result: Option<Value>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Pending,
    Unknown,
    Ready,
    Failed,
    Poll,
}

fn action(job: &Job) -> Result<Action, Failure> {
    match job.state.as_str() {
        // A crashed or cancelled submit has no surviving task that could save
        // its snapshot. Surface reconciliation rather than promising progress.
        "submitting"
            if chrono::Utc::now()
                .signed_duration_since(job.updated_at)
                .num_seconds()
                > 60 =>
        {
            Ok(Action::Unknown)
        }
        "submitting" => Ok(Action::Pending),
        "unknown" => Ok(Action::Unknown),
        "ready" if job.result.is_some() => Ok(Action::Ready),
        "failed" => Ok(Action::Failed),
        "running" if job.snapshot_id.as_deref().is_some_and(snapshot_id) => Ok(Action::Poll),
        _ => Err(Failure::unavailable()),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Progress {
    Pending,
    Ready,
    Failed,
}

fn progress(value: &Value) -> Result<Progress, Failure> {
    match value.get("status").and_then(Value::as_str) {
        Some("starting" | "running") => Ok(Progress::Pending),
        Some("ready") => Ok(Progress::Ready),
        Some("failed" | "canceled" | "cancelled") => Ok(Progress::Failed),
        _ => Err(Failure::unavailable()),
    }
}

fn validate_result(value: &Value) -> Result<(), Failure> {
    let items = value
        .as_array()
        .filter(|items| items.len() == 1)
        .ok_or_else(Failure::failed)?;
    let record = items[0]
        .as_object()
        .filter(|v| !v.is_empty())
        .ok_or_else(Failure::failed)?;
    // include_errors=true returns per-input errors as records in a 200 array.
    // Such records must not be sold as a successful scrape.
    if ["error", "error_code", "error_message", "errors"]
        .iter()
        .any(|key| {
            record.get(*key).is_some_and(|v| match v {
                Value::Null | Value::Bool(false) => false,
                Value::String(v) => !v.is_empty(),
                Value::Array(v) => !v.is_empty(),
                Value::Object(v) => !v.is_empty(),
                _ => true,
            })
        })
    {
        return Err(Failure::failed());
    }
    Ok(())
}

fn strip_snapshot_metadata(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            fields.remove("snapshot_id");
            for value in fields.values_mut() {
                strip_snapshot_metadata(value);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_snapshot_metadata(item);
            }
        }
        _ => {}
    }
}

fn envelope(input: &Input, mut data: Value) -> Result<Value, Failure> {
    // Snapshot management is a private provider capability, including when a
    // future provider schema unexpectedly echoes it inside a result record.
    strip_snapshot_metadata(&mut data);
    validate_result(&data)?;
    let notice = if input.kind == Kind::Profile {
        "Public LinkedIn data supplied by Bright Data, cached for up to one hour. Profiles may be cached or incomplete; position, experience and education can be unavailable, and current_company.industry is unsupported. This is not identity verification."
    } else {
        "Public LinkedIn data supplied by Bright Data, cached for up to one hour. Fields may be missing, stale or unavailable. This is not an official LinkedIn API or identity verification."
    };
    let value = json!({
        "data": data,
        "source_url": input.source_url,
        "notice": notice,
        "coverage": {"complete": false, "scope": format!("One supplied public LinkedIn {} URL; no search, private data or completeness guarantee", input.kind.name())},
    });
    if value.to_string().len() > MAX_RESPONSE_BYTES {
        return Err(Failure::failed());
    }
    Ok(value)
}

async fn read_json(mut response: reqwest::Response, cap: usize) -> Result<Value, Failure> {
    if response
        .content_length()
        .is_some_and(|length| length > cap as u64)
    {
        return Err(Failure::failed());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| Failure::unavailable())? {
        if bytes.len().saturating_add(chunk.len()) > cap {
            return Err(Failure::failed());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| Failure::failed())
}

/// Validation and the configured-key check happen before any durable claim or
/// provider request. Called only after authorization and spending checks.
pub async fn answer(
    pool: &PgPool,
    method: &str,
    path: &str,
    query: Option<&str>,
    body: &[u8],
) -> Result<Value, Failure> {
    let input = prepare(method, path, query, body)?;
    let secret = std::env::var("BRIGHTDATA_API_KEY").ok();
    let authorization = authorization(secret.as_deref())?;
    let client = client()?;
    let provider = Provider {
        client,
        authorization: &authorization,
        #[cfg(test)]
        test_origin: None,
    };
    tokio::time::timeout(REQUEST_BUDGET, lookup(pool, &provider, &input))
        .await
        .map_err(|_| Failure::unavailable())?
}

async fn lookup(pool: &PgPool, provider: &Provider<'_>, input: &Input) -> Result<Value, Failure> {
    let inserted = sqlx::query(
        "INSERT INTO gateway_private.brightdata_linkedin_jobs (request_key,kind,source_url,state) VALUES ($1,$2,$3,'submitting') ON CONFLICT (request_key) DO NOTHING"
    ).bind(&input.request_key).bind(input.kind.name()).bind(&input.source_url)
        .execute(pool).await.map_err(|_| Failure::unavailable())?.rows_affected() == 1;
    // Only terminal jobs may expire into a fresh paid collection. Unknown or
    // submitting rows are NEVER reclaimed: their POST may already have billed.
    let refreshed = if !inserted {
        sqlx::query(
            "UPDATE gateway_private.brightdata_linkedin_jobs SET state='submitting', snapshot_id=NULL, result=NULL, error_code=NULL, created_at=now(), updated_at=now(), expires_at=now()+interval '1 hour' WHERE request_key=$1 AND kind=$2 AND source_url=$3 AND state IN ('ready','failed') AND expires_at <= now()"
        ).bind(&input.request_key).bind(input.kind.name()).bind(&input.source_url)
            .execute(pool).await.map_err(|_| Failure::unavailable())?.rows_affected() == 1
    } else {
        false
    };
    if inserted || refreshed {
        return submit(pool, provider, input).await;
    }

    // Serialize polling/downloads per public URL across gateway replicas. An
    // overlapping reader returns pending immediately rather than downloading
    // again. Network and lock lifetime are bounded by answer's 20-second budget.
    let mut transaction = pool.begin().await.map_err(|_| Failure::unavailable())?;
    let job = sqlx::query_as::<_, Job>(
        "SELECT state,snapshot_id,result,updated_at FROM gateway_private.brightdata_linkedin_jobs WHERE request_key=$1 AND kind=$2 AND source_url=$3 FOR UPDATE SKIP LOCKED"
    ).bind(&input.request_key).bind(input.kind.name()).bind(&input.source_url)
        .fetch_optional(&mut *transaction).await.map_err(|_| Failure::unavailable())?
        .ok_or_else(Failure::pending)?;
    match action(&job)? {
        Action::Pending => Err(Failure::pending()),
        Action::Unknown => {
            if job.state == "submitting" {
                sqlx::query("UPDATE gateway_private.brightdata_linkedin_jobs SET state='unknown', error_code='submission_unknown', updated_at=now() WHERE request_key=$1 AND state='submitting'")
                    .bind(&input.request_key).execute(&mut *transaction).await.map_err(|_| Failure::unavailable())?;
                transaction
                    .commit()
                    .await
                    .map_err(|_| Failure::unavailable())?;
            }
            Err(Failure::unknown())
        }
        Action::Failed => Err(Failure::failed()),
        Action::Ready => envelope(input, job.result.ok_or_else(Failure::unavailable)?),
        Action::Poll => {
            let snapshot = job
                .snapshot_id
                .as_deref()
                .ok_or_else(Failure::unavailable)?;
            let response = provider
                .request(reqwest::Method::GET, management_url(snapshot, false)?)
                .send()
                .await
                .map_err(|_| Failure::unavailable())?;
            if !response.status().is_success() {
                return Err(Failure::unavailable());
            }
            let status = read_json(response, MAX_CONTROL_BYTES).await?;
            // If the provider echoes identity, it must agree with our private job.
            if status
                .get("snapshot_id")
                .is_some_and(|id| id.as_str() != Some(snapshot))
                || status
                    .get("dataset_id")
                    .is_some_and(|id| id.as_str() != Some(input.kind.dataset()))
            {
                return Err(Failure::unavailable());
            }
            match progress(&status)? {
                Progress::Pending => Err(Failure::pending()),
                Progress::Failed => {
                    sqlx::query("UPDATE gateway_private.brightdata_linkedin_jobs SET state='failed', error_code='collection_failed', updated_at=now() WHERE request_key=$1")
                        .bind(&input.request_key).execute(&mut *transaction).await.map_err(|_| Failure::unavailable())?;
                    transaction
                        .commit()
                        .await
                        .map_err(|_| Failure::unavailable())?;
                    Err(Failure::failed())
                }
                Progress::Ready => {
                    let response = provider
                        .request(reqwest::Method::GET, management_url(snapshot, true)?)
                        .send()
                        .await
                        .map_err(|_| Failure::unavailable())?;
                    if response.status() == StatusCode::CONFLICT {
                        return Err(Failure::pending());
                    }
                    if !response.status().is_success() {
                        return Err(Failure::unavailable());
                    }
                    let downloaded = read_json(response, MAX_RESPONSE_BYTES).await;
                    let completed = downloaded.and_then(|data| {
                        envelope(input, data.clone()).map(|response| (data, response))
                    });
                    let (data, response) = match completed {
                        Ok(completed) => completed,
                        Err(error) if error.status == StatusCode::SERVICE_UNAVAILABLE => {
                            return Err(error)
                        }
                        Err(error) => {
                            sqlx::query("UPDATE gateway_private.brightdata_linkedin_jobs SET state='failed', error_code='invalid_result', updated_at=now() WHERE request_key=$1")
                                .bind(&input.request_key).execute(&mut *transaction).await.map_err(|_| Failure::unavailable())?;
                            transaction
                                .commit()
                                .await
                                .map_err(|_| Failure::unavailable())?;
                            return Err(error);
                        }
                    };
                    sqlx::query("UPDATE gateway_private.brightdata_linkedin_jobs SET state='ready', result=$2, error_code=NULL, updated_at=now(), expires_at=now()+interval '1 hour' WHERE request_key=$1")
                        .bind(&input.request_key).bind(data).execute(&mut *transaction).await.map_err(|_| Failure::unavailable())?;
                    transaction
                        .commit()
                        .await
                        .map_err(|_| Failure::unavailable())?;
                    Ok(response)
                }
            }
        }
    }
}

async fn submit(pool: &PgPool, provider: &Provider<'_>, input: &Input) -> Result<Value, Failure> {
    // Exactly one POST. A timeout, malformed success, 5xx or redirect leaves an
    // unknown durable claim; never infer that it is safe to pay for a replay.
    let response = provider
        .request(reqwest::Method::POST, trigger_url(input.kind))
        .json(&json!({"input": [{"url": input.source_url}]}))
        .send()
        .await;
    let snapshot = match response {
        Ok(response) if response.status().is_success() => read_json(response, MAX_CONTROL_BYTES)
            .await
            .ok()
            .and_then(|body| {
                body.get("snapshot_id")
                    .and_then(Value::as_str)
                    .filter(|id| snapshot_id(id))
                    .map(str::to_owned)
            }),
        Ok(response)
            if matches!(
                response.status().as_u16(),
                400 | 401 | 403 | 404 | 422 | 429
            ) =>
        {
            sqlx::query("UPDATE gateway_private.brightdata_linkedin_jobs SET state='failed', error_code='submission_rejected', updated_at=now() WHERE request_key=$1 AND state='submitting'")
                .bind(&input.request_key).execute(pool).await.map_err(|_| Failure::unavailable())?;
            return Err(Failure::failed());
        }
        _ => None,
    };
    if let Some(snapshot) = snapshot {
        let changed = sqlx::query("UPDATE gateway_private.brightdata_linkedin_jobs SET state='running', snapshot_id=$2, updated_at=now() WHERE request_key=$1 AND state='submitting'")
            .bind(&input.request_key).bind(snapshot).execute(pool).await.map_err(|_| Failure::unavailable())?.rows_affected();
        if changed != 1 {
            return Err(Failure::unknown());
        }
        Err(Failure::pending())
    } else {
        sqlx::query("UPDATE gateway_private.brightdata_linkedin_jobs SET state='unknown', error_code='submission_unknown', updated_at=now() WHERE request_key=$1 AND state='submitting'")
            .bind(&input.request_key).execute(pool).await.map_err(|_| Failure::unavailable())?;
        Err(Failure::unknown())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_and_canonicalizes_one_url_for_each_fixed_dataset() {
        for (route, url, dataset) in [
            (
                "profile",
                "https://linkedin.com/in/public-person/",
                "gd_l1viktl72bvl7bjuj0",
            ),
            (
                "company",
                "https://www.linkedin.com/company/example",
                "gd_l1vikfnt1wgvvqz95w",
            ),
            (
                "job",
                "https://www.linkedin.com/jobs/view/12345678",
                "gd_lpfll7v5hcqtkxl6l",
            ),
            (
                "post",
                "https://www.linkedin.com/posts/example_activity-12345",
                "gd_lyy3tktm25m4avu764",
            ),
        ] {
            let input = prepare(
                "POST",
                &format!("/linkedin/{route}"),
                None,
                json!({"url":url}).to_string().as_bytes(),
            )
            .unwrap();
            assert_eq!(input.kind.dataset(), dataset);
            assert!(input.source_url.starts_with("https://www.linkedin.com/"));
            assert!(!input.source_url.ends_with('/'));
            assert_eq!(trigger_url(input.kind), format!("https://api.brightdata.com/datasets/v3/trigger?dataset_id={dataset}&format=json&include_errors=true"));
            let canonical = prepare(
                "POST",
                &format!("/linkedin/{route}"),
                None,
                json!({"url":input.source_url}).to_string().as_bytes(),
            )
            .unwrap();
            assert_eq!(input.request_key, canonical.request_key);
        }
    }

    #[test]
    fn rejects_batches_overrides_duplicates_and_arbitrary_routes() {
        let valid = json!({"url":"https://www.linkedin.com/in/person"}).to_string();
        for (method, path, query) in [
            ("GET", "/linkedin/profile", None),
            ("POST", "/datasets/v3/trigger", None),
            ("POST", "/linkedin/profile", Some("dataset_id=other")),
            ("POST", "/linkedin/profile", Some("")),
        ] {
            assert!(prepare(method, path, query, valid.as_bytes()).is_err());
        }
        for body in [
            json!([{"url":"https://www.linkedin.com/in/person"}]),
            json!({"url":["https://www.linkedin.com/in/person"]}),
            json!({"url":"https://www.linkedin.com/in/person", "dataset_id":"other"}),
            json!({"url":"https://www.linkedin.com/in/person", "limit":1}),
            json!({"input":[{"url":"https://www.linkedin.com/in/person"}]}),
        ] {
            assert!(prepare(
                "POST",
                "/linkedin/profile",
                None,
                body.to_string().as_bytes()
            )
            .is_err());
        }
        assert!(prepare(
            "POST",
            "/linkedin/profile",
            None,
            br#"{"url":"https://www.linkedin.com/in/a","url":"https://www.linkedin.com/in/b"}"#
        )
        .is_err());
        for url in [
            "http://www.linkedin.com/in/person",
            "https://www.linkedin.com.evil.test/in/person",
            "https://www.linkedin.com@evil.test/in/person",
            "https://u:p@www.linkedin.com/in/person",
            "https://www.linkedin.com:8443/in/person",
            "https://www.linkedin.com/in/person?x=1",
            "https://www.linkedin.com/in/person#x",
            "https://www.linkedin.com/in/person/extra",
            "https://www.linkedin.com/company/example",
            "https://www.linkedin.com/in/a%2Fb",
            "https://www.linkedin.com/in/",
            "https://www.linkedin.com/in/person\n",
        ] {
            assert!(
                canonical_url(Kind::Profile, url).is_none(),
                "accepted {url}"
            );
        }
        assert!(canonical_url(Kind::Job, "https://www.linkedin.com/jobs/view/not-an-id").is_none());
    }

    #[test]
    fn checks_private_bearer_before_any_submission() {
        for key in [
            None,
            Some(""),
            Some("secret\n"),
            Some("Bearer secret"),
            Some(" secret"),
            Some("sëcret"),
        ] {
            assert!(authorization(key).is_err());
        }
        assert!(authorization(Some("test-key")).unwrap().is_sensitive());
    }

    #[test]
    fn unknown_or_inflight_submissions_never_become_pollable_or_replayed() {
        for (state, expected) in [
            ("submitting", Action::Pending),
            ("unknown", Action::Unknown),
            ("failed", Action::Failed),
        ] {
            let job = Job {
                state: state.into(),
                snapshot_id: None,
                result: None,
                updated_at: chrono::Utc::now(),
            };
            assert_eq!(action(&job).unwrap(), expected);
        }
        assert!(action(&Job {
            state: "running".into(),
            snapshot_id: None,
            result: None,
            updated_at: chrono::Utc::now(),
        })
        .is_err());
        assert!(action(&Job {
            state: "ready".into(),
            snapshot_id: None,
            result: None,
            updated_at: chrono::Utc::now(),
        })
        .is_err());
        assert_eq!(
            action(&Job {
                state: "running".into(),
                snapshot_id: Some("s_example123".into()),
                result: None,
                updated_at: chrono::Utc::now(),
            })
            .unwrap(),
            Action::Poll
        );
        assert_eq!(
            action(&Job {
                state: "ready".into(),
                snapshot_id: None,
                result: Some(json!([{"name":"example"}])),
                updated_at: chrono::Utc::now(),
            })
            .unwrap(),
            Action::Ready
        );
        assert_eq!(
            action(&Job {
                state: "submitting".into(),
                snapshot_id: None,
                result: None,
                updated_at: chrono::Utc::now() - chrono::Duration::seconds(61),
            })
            .unwrap(),
            Action::Unknown
        );
    }

    #[test]
    fn management_urls_cannot_escape_fixed_provider_origin() {
        assert_eq!(
            management_url("sd_m4x7enmven8djfqak", true).unwrap(),
            "https://api.brightdata.com/datasets/v3/snapshot/sd_m4x7enmven8djfqak?format=json"
        );
        assert_eq!(
            management_url("s_example123", false).unwrap(),
            "https://api.brightdata.com/datasets/v3/progress/s_example123"
        );
        assert_eq!(
            management_url("s_example123", true).unwrap(),
            "https://api.brightdata.com/datasets/v3/snapshot/s_example123?format=json"
        );
        for snapshot in [
            "",
            "s_",
            "sd_",
            "https://evil.test",
            "s_foo/bar",
            "s_foo?notify=evil",
            "s_foo%2Fbar",
            "s_foo#bar",
            "s_foo\n",
        ] {
            assert!(management_url(snapshot, true).is_err());
        }
    }

    #[test]
    fn provider_progress_and_error_records_do_not_turn_into_paid_success() {
        for status in ["starting", "running"] {
            assert_eq!(
                progress(&json!({"status":status})).unwrap(),
                Progress::Pending
            );
        }
        assert_eq!(
            progress(&json!({"status":"ready"})).unwrap(),
            Progress::Ready
        );
        for status in ["failed", "canceled", "cancelled"] {
            assert_eq!(
                progress(&json!({"status":status})).unwrap(),
                Progress::Failed
            );
        }
        assert!(progress(&json!({"status":"new_unknown_status"})).is_err());
        for result in [
            json!([]),
            json!([{}, {}]),
            json!([{}]),
            json!([null]),
            json!([{"error":"not_found"}]),
            json!([{"error_code":"404", "url":"https://www.linkedin.com/in/person"}]),
            json!([{"error_message":"private"}]),
            json!([{"errors":["blocked"]}]),
            json!({"status":"running"}),
        ] {
            assert!(validate_result(&result).is_err());
        }
        let input = prepare(
            "POST",
            "/linkedin/profile",
            None,
            br#"{"url":"https://www.linkedin.com/in/person"}"#,
        )
        .unwrap();
        let result = envelope(
            &input,
            json!([{"name":"Person", "error":null,
            "snapshot_id":"sd_private", "metadata":{"snapshot_id":"sd_private"}}]),
        )
        .unwrap();
        assert_eq!(result["coverage"]["complete"], false);
        assert_eq!(result["data"].as_array().unwrap().len(), 1);
        assert!(result.get("snapshot_id").is_none());
        assert!(!result.to_string().contains("sd_private"));
        assert!(result["notice"]
            .as_str()
            .unwrap()
            .contains("current_company.industry"));
    }

    /// Runs against a dedicated loopback PostgreSQL database, never Supabase.
    /// BRIGHTDATA_TEST_DATABASE_URL=postgres://.../apiosk_brightdata_test cargo
    /// test -p brightdata-linkedin brightdata_postgres_lifecycle -- --ignored
    #[tokio::test]
    #[ignore = "requires isolated loopback BRIGHTDATA_TEST_DATABASE_URL ending in _brightdata_test"]
    async fn brightdata_postgres_lifecycle() {
        use axum::{
            body::Body,
            extract::State,
            http::Request,
            response::{IntoResponse, Response},
            Router,
        };
        use std::sync::{
            atomic::{AtomicU8, AtomicUsize, Ordering},
            Arc, Mutex,
        };

        #[derive(Default)]
        struct Mock {
            mode: AtomicU8,
            triggers: AtomicUsize,
            polls: AtomicUsize,
            downloads: AtomicUsize,
            url: Mutex<String>,
        }
        async fn handler(State(mock): State<Arc<Mock>>, request: Request<Body>) -> Response {
            assert_eq!(request.headers()["authorization"], "Bearer test-key");
            let path = request.uri().path().to_string();
            let mode = mock.mode.load(Ordering::SeqCst);
            match path.as_str() {
                "/datasets/v3/trigger" => {
                    assert_eq!(request.method(), "POST");
                    assert_eq!(
                        request.uri().query(),
                        Some("dataset_id=gd_l1viktl72bvl7bjuj0&format=json&include_errors=true")
                    );
                    let bytes = axum::body::to_bytes(request.into_body(), 4096)
                        .await
                        .unwrap();
                    let payload: Value = serde_json::from_slice(&bytes).unwrap();
                    assert_eq!(payload.as_object().unwrap().len(), 1);
                    assert_eq!(payload["input"].as_array().unwrap().len(), 1);
                    assert_eq!(payload["input"][0].as_object().unwrap().len(), 1);
                    *mock.url.lock().unwrap() =
                        payload["input"][0]["url"].as_str().unwrap().to_owned();
                    mock.triggers.fetch_add(1, Ordering::SeqCst);
                    // Keep concurrent buyers overlapping the paid submission.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    if mode == 2 {
                        // The server received the billable POST, but its response
                        // breaks mid-stream: replay would risk a second charge.
                        let chunks: Vec<Result<bytes::Bytes, std::io::Error>> = vec![
                            Ok(bytes::Bytes::from_static(b"{\"snapshot_id\":")),
                            Err(std::io::Error::other("mock transport interruption")),
                        ];
                        return Response::new(Body::from_stream(futures_util::stream::iter(
                            chunks,
                        )));
                    }
                    axum::Json(json!({"snapshot_id":"sd_mock123"})).into_response()
                }
                "/datasets/v3/progress/sd_mock123" => {
                    assert_eq!(request.method(), "GET");
                    mock.polls.fetch_add(1, Ordering::SeqCst);
                    let status = match mode {
                        0 => "running",
                        6 => "failed",
                        _ => "ready",
                    };
                    axum::Json(json!({"snapshot_id":"sd_mock123", "dataset_id":"gd_l1viktl72bvl7bjuj0", "status":status})).into_response()
                }
                "/datasets/v3/snapshot/sd_mock123" => {
                    assert_eq!(request.method(), "GET");
                    assert_eq!(request.uri().query(), Some("format=json"));
                    mock.downloads.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    match mode {
                        3 => "{malformed".into_response(),
                        4 => axum::Json(json!([{"error":"not_found", "input":{"url":*mock.url.lock().unwrap()}}])).into_response(),
                        5 => "x".repeat(MAX_RESPONSE_BYTES + 1).into_response(),
                        7 => StatusCode::CONFLICT.into_response(),
                        _ => axum::Json(json!([{"name":"Fixture", "url":*mock.url.lock().unwrap()}])).into_response(),
                    }
                }
                _ => panic!("unexpected provider route"),
            }
        }
        fn input() -> Input {
            prepare("POST", "/linkedin/profile", None,
                json!({"url":format!("https://www.linkedin.com/in/fixture-{}", uuid::Uuid::new_v4())}).to_string().as_bytes()).unwrap()
        }
        async fn state(pool: &PgPool, input: &Input) -> String {
            sqlx::query_scalar(
                "SELECT state FROM gateway_private.brightdata_linkedin_jobs WHERE request_key=$1",
            )
            .bind(&input.request_key)
            .fetch_one(pool)
            .await
            .unwrap()
        }

        let database_url =
            std::env::var("BRIGHTDATA_TEST_DATABASE_URL").expect("isolated test database required");
        let parsed = url::Url::parse(&database_url).unwrap();
        assert!(matches!(parsed.host_str(), Some("127.0.0.1" | "localhost")));
        assert!(parsed.path().ends_with("_brightdata_test"));
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(8)
            .connect(&database_url)
            .await
            .unwrap();
        sqlx::raw_sql("CREATE SCHEMA IF NOT EXISTS gateway_private;
            CREATE TABLE IF NOT EXISTS gateway_private.brightdata_linkedin_jobs (
                request_key text PRIMARY KEY, kind text NOT NULL, source_url text NOT NULL,
                state text NOT NULL, snapshot_id text, result jsonb, error_code text,
                created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now(),
                expires_at timestamptz NOT NULL DEFAULT now()+interval '1 hour');")
            .execute(&pool).await.unwrap();
        let mock = Arc::new(Mock::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let router = Router::new().fallback(handler).with_state(mock.clone());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let auth = authorization(Some("test-key")).unwrap();
        let provider = Provider {
            client: &client,
            authorization: &auth,
            test_origin: Some(origin),
        };
        let mut keys = Vec::new();

        // Two simultaneous buyers cause exactly one paid trigger. Subsequent
        // polls keep the same private snapshot, and download happens once.
        let normal = input();
        keys.push(normal.request_key.clone());
        let (first, second) = tokio::join!(
            lookup(&pool, &provider, &normal),
            lookup(&pool, &provider, &normal)
        );
        assert_eq!(first.unwrap_err().code, "brightdata_pending");
        assert_eq!(second.unwrap_err().code, "brightdata_pending");
        assert_eq!(mock.triggers.load(Ordering::SeqCst), 1);
        assert_eq!(state(&pool, &normal).await, "running");
        assert_eq!(
            lookup(&pool, &provider, &normal).await.unwrap_err().code,
            "brightdata_pending"
        );
        mock.mode.store(1, Ordering::SeqCst);
        let (first, second) = tokio::join!(
            lookup(&pool, &provider, &normal),
            lookup(&pool, &provider, &normal)
        );
        assert!(first.is_ok() || second.is_ok());
        let saved = lookup(&pool, &provider, &normal).await.unwrap();
        assert_eq!(saved["data"][0]["name"], "Fixture");
        assert_eq!(mock.downloads.load(Ordering::SeqCst), 1);
        assert_eq!(mock.triggers.load(Ordering::SeqCst), 1);
        assert_eq!(state(&pool, &normal).await, "ready");
        assert!(!saved.to_string().contains("sd_mock123"));

        // Expired terminal rows can refresh, but simultaneous refresh buyers
        // still share exactly one paid trigger.
        sqlx::query("UPDATE gateway_private.brightdata_linkedin_jobs SET expires_at=now()-interval '1 second' WHERE request_key=$1")
            .bind(&normal.request_key).execute(&pool).await.unwrap();
        let _ = tokio::join!(
            lookup(&pool, &provider, &normal),
            lookup(&pool, &provider, &normal)
        );
        assert_eq!(mock.triggers.load(Ordering::SeqCst), 2);

        // Ambiguous transport completion is durable even after expiry. No
        // provider request is repeated when the buyer retries the same URL.
        mock.mode.store(2, Ordering::SeqCst);
        let unknown = input();
        keys.push(unknown.request_key.clone());
        assert_eq!(
            lookup(&pool, &provider, &unknown).await.unwrap_err().code,
            "brightdata_submission_unknown"
        );
        let triggers = mock.triggers.load(Ordering::SeqCst);
        sqlx::query("UPDATE gateway_private.brightdata_linkedin_jobs SET expires_at=now()-interval '2 hours' WHERE request_key=$1")
            .bind(&unknown.request_key).execute(&pool).await.unwrap();
        for _ in 0..3 {
            assert_eq!(
                lookup(&pool, &provider, &unknown).await.unwrap_err().code,
                "brightdata_submission_unknown"
            );
        }
        assert_eq!(state(&pool, &unknown).await, "unknown");
        assert_eq!(mock.triggers.load(Ordering::SeqCst), triggers);

        // Restart/cancellation between claiming and saving a snapshot becomes
        // reconciliation-required, without making another paid POST.
        let stale = input();
        keys.push(stale.request_key.clone());
        sqlx::query("INSERT INTO gateway_private.brightdata_linkedin_jobs(request_key,kind,source_url,state,updated_at,expires_at) VALUES($1,$2,$3,'submitting',now()-interval '2 minutes',now()-interval '2 hours')")
            .bind(&stale.request_key).bind(stale.kind.name()).bind(&stale.source_url).execute(&pool).await.unwrap();
        assert_eq!(
            lookup(&pool, &provider, &stale).await.unwrap_err().code,
            "brightdata_submission_unknown"
        );
        assert_eq!(state(&pool, &stale).await, "unknown");
        assert_eq!(mock.triggers.load(Ordering::SeqCst), triggers);

        // Malformed JSON, provider error records, oversize downloads and
        // failed collections become durable failures, with no retry download.
        for mode in [3, 4, 5, 6] {
            mock.mode.store(mode, Ordering::SeqCst);
            let failed = input();
            keys.push(failed.request_key.clone());
            assert_eq!(
                lookup(&pool, &provider, &failed).await.unwrap_err().code,
                "brightdata_pending"
            );
            assert_eq!(
                lookup(&pool, &provider, &failed).await.unwrap_err().code,
                "brightdata_collection_failed"
            );
            assert_eq!(state(&pool, &failed).await, "failed");
            let counts = (
                mock.triggers.load(Ordering::SeqCst),
                mock.downloads.load(Ordering::SeqCst),
            );
            assert_eq!(
                lookup(&pool, &provider, &failed).await.unwrap_err().code,
                "brightdata_collection_failed"
            );
            assert_eq!(
                (
                    mock.triggers.load(Ordering::SeqCst),
                    mock.downloads.load(Ordering::SeqCst)
                ),
                counts
            );
        }
        // Provider 409 is transient: it may download on a later read without
        // triggering another paid collection.
        mock.mode.store(7, Ordering::SeqCst);
        let delayed = input();
        keys.push(delayed.request_key.clone());
        assert_eq!(
            lookup(&pool, &provider, &delayed).await.unwrap_err().code,
            "brightdata_pending"
        );
        assert_eq!(
            lookup(&pool, &provider, &delayed).await.unwrap_err().code,
            "brightdata_pending"
        );
        let triggers = mock.triggers.load(Ordering::SeqCst);
        mock.mode.store(1, Ordering::SeqCst);
        assert!(lookup(&pool, &provider, &delayed).await.is_ok());
        assert_eq!(mock.triggers.load(Ordering::SeqCst), triggers);

        sqlx::query(
            "DELETE FROM gateway_private.brightdata_linkedin_jobs WHERE request_key=ANY($1)",
        )
        .bind(&keys)
        .execute(&pool)
        .await
        .unwrap();
        server.abort();
        pool.close().await;
    }
}
