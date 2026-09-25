//! Request-scoped telemetry. One trace id follows a question from the Find
//! click through pricing, approval, execution and the answer. Lines go to the
//! JSON log under target `apiosk_telemetry`, one compact JSON object in the
//! `telemetry` field, so a field can be an explicit null and a line is not
//! limited by the logger's field count.
//!
//! The API accepts only closed enums, `&'static str`, ids, numbers and
//! slugs passed through [`label`]: question text, answers, credentials and
//! error messages cannot enter a telemetry line.

mod classify;
mod client;
mod cost;
mod journey;
mod names;
mod persist;

pub use classify::classify;
pub use client::{Beacon, MAX_BYTES as BEACON_MAX_BYTES};
pub use cost::{ModelAttempt, Settlement, SourceCall};
pub use journey::{answer, execution_ended, fully_answered, question, question_failed};
pub use names::{label, Cache, ErrorClass, Milestone, Phase, QueryClass, Segment, SegmentOutcome, SourceStatus, Surface};
pub use persist::enabled as persisting;

use axum::{
    extract::{MatchedPath, Request, State},
    http::{HeaderMap, HeaderValue, Method},
    middleware::Next,
    response::Response,
};
use cost::CostRecord;
use serde_json::{json, Map, Value};
use std::{
    collections::BTreeMap,
    future::Future,
    sync::{
        atomic::{AtomicU8, Ordering::Relaxed},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use uuid::Uuid;

pub const TARGET: &str = "apiosk_telemetry";
pub const TRACE_HEADER: &str = "x-apiosk-trace";
const SCHEMA: u8 = 1;

tokio::task_local! {
    static TRACE: Arc<Trace>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The App generated the id for one Find click and sent it.
    Client,
    /// No valid client id: the gateway generated one for this request.
    Generated,
}
impl Origin {
    pub const fn as_str(self) -> &'static str {
        match self {
            Origin::Client => "client",
            Origin::Generated => "generated",
        }
    }
}

/// An enum value kept in an atomic as its position in `ALL`, 0 when unset.
struct Slot(AtomicU8);
impl Slot {
    const fn new() -> Self {
        Self(AtomicU8::new(0))
    }
    fn set<T: Copy + PartialEq>(&self, all: &[T], value: T) {
        let index = all.iter().position(|item| *item == value).map_or(0, |index| index as u8 + 1);
        self.0.store(index, Relaxed);
    }
    fn get<T: Copy>(&self, all: &[T]) -> Option<T> {
        (self.0.load(Relaxed) as usize).checked_sub(1).and_then(|index| all.get(index).copied())
    }
}

/// One HTTP request's view of a trace. Work it spawns through [`spawn`]
/// shares it, so a worker started by an approval reports under that trace.
pub struct Trace {
    pub id: Uuid,
    pub origin: Origin,
    surface: Slot,
    segment: Slot,
    catalog_cache: Slot,
    parser_cache: Slot,
    query_class: Slot,
    task: Mutex<Option<Uuid>>,
    quote: Mutex<Option<Uuid>>,
    db: Mutex<BTreeMap<&'static str, (u64, u64)>>,
    summary: Mutex<persist::Summary>,
    cost: CostRecord,
}
impl Trace {
    pub fn new(id: Uuid, origin: Origin) -> Self {
        Self {
            id,
            origin,
            surface: Slot::new(),
            segment: Slot::new(),
            catalog_cache: Slot::new(),
            parser_cache: Slot::new(),
            query_class: Slot::new(),
            task: Mutex::new(None),
            quote: Mutex::new(None),
            db: Mutex::new(BTreeMap::new()),
            summary: Mutex::new(persist::Summary::default()),
            cost: CostRecord::default(),
        }
    }
    pub fn surface(&self) -> Surface {
        self.surface.get(Surface::ALL).unwrap_or(Surface::Unknown)
    }
    pub fn segment(&self) -> Option<Segment> {
        self.segment.get(Segment::ALL)
    }
    pub fn catalog_cache(&self) -> Option<Cache> {
        self.catalog_cache.get(Cache::ALL)
    }
    pub fn parser_cache(&self) -> Option<Cache> {
        self.parser_cache.get(Cache::ALL)
    }
    pub fn query_class(&self) -> Option<QueryClass> {
        self.query_class.get(QueryClass::ALL)
    }
    pub fn task(&self) -> Option<Uuid> {
        *self.task.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
    pub fn quote(&self) -> Option<Uuid> {
        *self.quote.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
    /// A new segment of the same trace, with its own counters: work that
    /// outlives the request which started it reports its own costs.
    fn child(&self, segment: Segment) -> Self {
        let child = Self::new(self.id, self.origin);
        child.surface.set(Surface::ALL, self.surface());
        child.segment.set(Segment::ALL, segment);
        if let Some(cache) = self.catalog_cache() {
            child.catalog_cache.set(Cache::ALL, cache);
        }
        if let Some(cache) = self.parser_cache() {
            child.parser_cache.set(Cache::ALL, cache);
        }
        if let Some(class) = self.query_class() {
            child.query_class.set(QueryClass::ALL, class);
        }
        *child.task.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = self.task();
        *child.quote.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = self.quote();
        *child.summary.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = self.summary.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        child
    }
    fn take_db(&self) -> Value {
        let labels = std::mem::take(&mut *self.db.lock().unwrap_or_else(|poisoned| poisoned.into_inner()));
        Value::Object(labels.into_iter().map(|(label, (calls, ms))| (label.to_owned(), json!({"calls": calls, "ms": ms}))).collect())
    }
}

/// The trace of the running task, if it was started inside one.
pub fn current() -> Option<Arc<Trace>> {
    TRACE.try_with(Arc::clone).ok()
}
/// The current trace id, for existing diagnostic lines: `trace = telemetry::id()`.
pub fn id() -> Option<tracing::field::DisplayValue<Uuid>> {
    TRACE.try_with(|trace| tracing::field::display(trace.id)).ok()
}
fn with(update: impl FnOnce(&Trace)) {
    let _ = TRACE.try_with(|trace| update(trace));
}

/// Runs `work` inside `trace`, for entry points that are not HTTP requests.
pub async fn scope<F: Future>(trace: Arc<Trace>, work: F) -> F::Output {
    TRACE.scope(trace, work).await
}

/// `tokio::spawn` that keeps the current trace. Every spawn in the engine
/// and server goes through here (checked by `tests/telemetry_propagation.rs`).
pub fn spawn<F>(work: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    match current() {
        Some(trace) => tokio::spawn(TRACE.scope(trace, work)),
        None => tokio::spawn(work),
    }
}

/// Spawns work that outlives its request, such as a server worker, as a
/// new segment of the current trace with its own cost counters.
pub fn spawn_segment<F>(segment: Segment, work: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let trace = Arc::new(match current() {
        Some(parent) => parent.child(segment),
        None => {
            let trace = Trace::new(Uuid::new_v4(), Origin::Generated);
            trace.segment.set(Segment::ALL, segment);
            trace
        }
    });
    tokio::spawn(TRACE.scope(trace, work))
}

pub fn set_surface(surface: Surface) {
    with(|trace| trace.surface.set(Surface::ALL, surface));
}
pub fn set_segment(segment: Segment) {
    with(|trace| trace.segment.set(Segment::ALL, segment));
}
/// Records how the catalog answered. A hit never hides an earlier wait in
/// the same request, so the trace keeps its first non-hit state.
pub fn set_catalog_cache(cache: Cache) {
    with(|trace| {
        let current = trace.catalog_cache();
        if matches!(current, None | Some(Cache::Hit)) || cache == Cache::StaleServed {
            trace.catalog_cache.set(Cache::ALL, cache);
        }
    });
}

/// One catalog read that began at `started`. A hit only marks the trace;
/// waiting for a load, or serving a catalog past its TTL while a refresh
/// is pending, is also a span.
pub fn catalog(state: Cache, started: Instant, outcome: Outcome) {
    set_catalog_cache(state);
    if state != Cache::Hit {
        span(Phase::Catalog).since(started).tag("catalog_cache", state.as_str()).finish(outcome);
    }
}
pub fn set_parser_cache(cache: Cache) {
    with(|trace| trace.parser_cache.set(Cache::ALL, cache));
}
pub fn set_query_class(class: QueryClass) {
    with(|trace| trace.query_class.set(QueryClass::ALL, class));
}
/// Joins the trace's later lines to a task and, once priced, its quote.
pub fn bind(task: Uuid, quote: Option<Uuid>) {
    with(|trace| {
        *trace.task.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(task);
        if quote.is_some() {
            *trace.quote.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = quote;
        }
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Failed(ErrorClass),
}
impl Outcome {
    fn error_class(self) -> Option<&'static str> {
        match self {
            Outcome::Ok => None,
            Outcome::Failed(class) => Some(class.as_str()),
        }
    }
}

/// Maps an error to its class without reading its message.
pub trait Classify {
    fn error_class(&self) -> ErrorClass;
}
impl Classify for sqlx::Error {
    fn error_class(&self) -> ErrorClass {
        match self {
            sqlx::Error::PoolTimedOut => ErrorClass::Busy,
            _ => ErrorClass::Db,
        }
    }
}
impl Classify for reqwest::Error {
    fn error_class(&self) -> ErrorClass {
        match self.status() {
            Some(status) => ErrorClass::upstream(status.as_u16()),
            None if self.is_timeout() => ErrorClass::Timeout,
            None => ErrorClass::Transport,
        }
    }
}
impl Classify for crate::auth::AuthError {
    fn error_class(&self) -> ErrorClass {
        use crate::auth::AuthError;
        match self {
            AuthError::Unauthorized => ErrorClass::Unauthorized,
            AuthError::Refused(_) => ErrorClass::Forbidden,
            AuthError::Unavailable(_) => ErrorClass::Upstream5xx,
        }
    }
}
impl Surface {
    /// The surface an authenticated caller reached the gateway through.
    pub fn of(ctx: &crate::auth::AuthContext) -> Self {
        if ctx.api_key.is_some() {
            Surface::PublicApi
        } else if ctx.principal.is_person() {
            Surface::App
        } else {
            Surface::ConnectedAgent
        }
    }
}
impl Classify for anyhow::Error {
    fn error_class(&self) -> ErrorClass {
        use crate::{parser::ModelFailure, store::StoreAccessError};
        if let Some(error) = self.downcast_ref::<sqlx::Error>() {
            error.error_class()
        } else if let Some(error) = self.downcast_ref::<reqwest::Error>() {
            error.error_class()
        } else if let Some(failure) = self.downcast_ref::<ModelFailure>() {
            match failure {
                ModelFailure::Unsupported => ErrorClass::Unsupported,
                ModelFailure::Unavailable => ErrorClass::Upstream5xx,
            }
        } else if self.downcast_ref::<StoreAccessError>().is_some() {
            ErrorClass::NotFound
        } else {
            ErrorClass::Internal
        }
    }
}

/// A timed phase. Finish it explicitly; a span dropped unfinished (an early
/// `?` return or a cancelled future) is reported as failed with `dropped`.
#[must_use = "finish the span, or it reports as dropped"]
pub struct Span {
    phase: Phase,
    label: Option<&'static str>,
    task: Option<Uuid>,
    quote: Option<Uuid>,
    fields: Map<String, Value>,
    started: Instant,
    finished: bool,
}
pub fn span(phase: Phase) -> Span {
    Span { phase, label: None, task: None, quote: None, fields: Map::new(), started: Instant::now(), finished: false }
}
impl Span {
    pub fn label(mut self, label: &'static str) -> Self {
        self.label = Some(label);
        self
    }
    pub fn task(mut self, task: Uuid) -> Self {
        self.task = Some(task);
        self
    }
    pub fn quote(mut self, quote: Uuid) -> Self {
        self.quote = Some(quote);
        self
    }
    /// A phase-specific measurement; `None` is reported as null (unmeasured).
    pub fn measure(mut self, key: &'static str, value: impl Into<Option<u64>>) -> Self {
        self.fields.insert(key.to_owned(), json!(value.into()));
        self
    }
    /// A phase-specific closed value, such as a cache state or event kind.
    pub fn tag(mut self, key: &'static str, value: &'static str) -> Self {
        self.fields.insert(key.to_owned(), json!(value));
        self
    }
    /// Measures from an earlier instant, for a phase that began before the
    /// code that can report it.
    pub fn since(mut self, started: Instant) -> Self {
        self.started = started;
        self
    }
    pub fn finish(mut self, outcome: Outcome) {
        self.emit(outcome);
    }
    fn emit(&mut self, outcome: Outcome) {
        self.finished = true;
        let mut fields = std::mem::take(&mut self.fields);
        fields.insert("phase".into(), json!(self.phase.as_str()));
        fields.insert("label".into(), json!(self.label));
        fields.insert("ms".into(), json!(self.started.elapsed().as_millis() as u64));
        fields.insert("ok".into(), json!(outcome == Outcome::Ok));
        fields.insert("error_class".into(), json!(outcome.error_class()));
        emit("span", current().as_deref(), self.task, self.quote, fields);
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        if !self.finished {
            self.emit(Outcome::Failed(ErrorClass::Dropped));
        }
    }
}

/// Times `work` as one span; an error is classified, never logged.
pub async fn timed<T, E: Classify>(phase: Phase, label: &'static str, work: impl Future<Output = Result<T, E>>) -> Result<T, E> {
    let span = span(phase).label(label);
    let result = work.await;
    span.finish(match &result {
        Ok(_) => Outcome::Ok,
        Err(error) => Outcome::Failed(error.error_class()),
    });
    result
}

fn record_db(label: &'static str, elapsed: Duration) {
    with(|trace| {
        trace.cost.db(elapsed);
        let mut labels = trace.db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = labels.entry(label).or_default();
        entry.0 += 1;
        entry.1 += elapsed.as_millis() as u64;
    });
}
/// Times one instrumented database operation until the guard drops: bind it
/// for the whole store method, `let _db = telemetry::db("tasks.load");`.
/// Reported per request (`db_calls`, `db_ms`, per label) as a lower bound of
/// all database time, never per call.
pub fn db(label: &'static str) -> DbCall {
    DbCall { label, started: Instant::now() }
}
pub struct DbCall {
    label: &'static str,
    started: Instant,
}
impl Drop for DbCall {
    fn drop(&mut self) {
        record_db(self.label, self.started.elapsed());
    }
}
/// Times one inline database query for [`db`].
pub async fn timed_db<T>(label: &'static str, work: impl Future<Output = T>) -> T {
    let _db = db(label);
    work.await
}

/// Starts one model HTTP attempt. Record the response envelope when it
/// arrives and finish with the outcome. An attempt abandoned at a stage
/// deadline reports on drop as failed with unknown usage, so the trace's
/// token totals stay marked incomplete instead of missing a billed call.
pub fn model_call(surface: &'static str, attempt: u8, model: &str, input_bytes: usize) -> ModelCall {
    ModelCall { surface, attempt, model: label(model).to_owned(), input_bytes, envelope: None, output_bytes: 0, started: Instant::now(), finished: false }
}
pub struct ModelCall {
    surface: &'static str,
    attempt: u8,
    model: String,
    input_bytes: usize,
    envelope: Option<Value>,
    output_bytes: usize,
    started: Instant,
    finished: bool,
}
impl ModelCall {
    /// Keeps only the response's usage and provider slug, never its content.
    pub fn received(&mut self, body: &Value, output_bytes: usize) {
        self.envelope = Some(json!({"usage": body["usage"], "provider": body["provider"]}));
        self.output_bytes = output_bytes;
    }
    /// Reports the attempt once; later calls are ignored.
    pub fn finish(&mut self, outcome: Outcome, reason: &'static str) {
        if std::mem::replace(&mut self.finished, true) {
            return;
        }
        model_attempt(ModelAttempt { surface: self.surface, attempt: self.attempt, model: &self.model, body: self.envelope.as_ref(),
            input_bytes: self.input_bytes, output_bytes: self.output_bytes, elapsed: self.started.elapsed(), outcome, reason });
    }
}
impl Drop for ModelCall {
    fn drop(&mut self) {
        self.finish(Outcome::Failed(ErrorClass::Dropped), "abandoned");
    }
}

/// One model HTTP attempt: a `gw.model` span plus the trace's model cost.
pub fn model_attempt(attempt: ModelAttempt) {
    let trace = current();
    if let Some(trace) = &trace {
        trace.cost.model(&attempt);
    }
    let mut fields = match cost::model_line(&attempt) {
        Value::Object(fields) => fields,
        _ => Map::new(),
    };
    fields.insert("phase".into(), json!(Phase::Model.as_str()));
    fields.insert("label".into(), json!(attempt.surface));
    fields.insert("ms".into(), json!(attempt.elapsed.as_millis() as u64));
    fields.insert("ok".into(), json!(attempt.outcome == Outcome::Ok));
    fields.insert("error_class".into(), json!(attempt.outcome.error_class()));
    emit("span", trace.as_deref(), None, None, fields);
}

/// One paid source call after the Edge hop: a `gw.edge_hop` span plus cost.
pub fn source_call(call: SourceCall) {
    let trace = current();
    if let Some(trace) = &trace {
        trace.cost.source(&call);
    }
    let mut fields = match cost::source_line(&call) {
        Value::Object(fields) => fields,
        _ => Map::new(),
    };
    fields.insert("phase".into(), json!(Phase::EdgeHop.as_str()));
    fields.insert("ms".into(), json!(call.edge_ms));
    fields.insert("ok".into(), json!(call.error_class.is_none()));
    fields.insert("error_class".into(), json!(call.error_class.map(ErrorClass::as_str)));
    emit("span", trace.as_deref(), Some(call.task), None, fields);
}
/// The ledger settled a call that was unresolved when it was dispatched.
pub fn settled(settlement: Settlement) {
    with(|trace| trace.cost.settle(&settlement));
}
/// A source call repeated after a provider refusal that refunded the first.
pub fn source_retry() {
    with(|trace| trace.cost.source_retry());
}
/// One receipt poll of the ledger for an unresolved call.
pub fn poll() {
    with(|trace| trace.cost.poll());
}
/// One server-sent event of `bytes` delivered to a viewer.
pub fn sse(bytes: usize) {
    with(|trace| trace.cost.sse(bytes));
}

/// Milestone details. Every field is optional and only set fields are sent.
#[derive(Clone, Debug, Default)]
pub struct MilestoneFields {
    pub status: Option<&'static str>,
    pub query_class: Option<QueryClass>,
    pub sources: Option<usize>,
    pub steps: Option<usize>,
    pub cap_micro_usd: Option<i64>,
    pub answer_status: Option<&'static str>,
    pub fully_answered: Option<bool>,
    pub observations: Option<usize>,
    pub evidence_refs: Option<usize>,
    pub failed_steps: Option<usize>,
    pub pending: Option<usize>,
    pub error_class: Option<ErrorClass>,
}
impl MilestoneFields {
    fn into_map(self) -> Map<String, Value> {
        let mut fields = Map::new();
        let mut put = |key: &str, value: Value| {
            if !value.is_null() {
                fields.insert(key.to_owned(), value);
            }
        };
        put("status", json!(self.status));
        put("query_class", json!(self.query_class.map(QueryClass::as_str)));
        put("sources", json!(self.sources.map(|count| if count > 1 { "multiple" } else { "one" })));
        put("source_count", json!(self.sources));
        put("steps", json!(self.steps));
        put("cap_micro_usd", json!(self.cap_micro_usd));
        put("answer_status", json!(self.answer_status));
        put("fully_answered", json!(self.fully_answered));
        put("observations", json!(self.observations));
        put("evidence_refs", json!(self.evidence_refs));
        put("failed_steps", json!(self.failed_steps));
        put("pending", json!(self.pending));
        put("error_class", json!(self.error_class.map(ErrorClass::as_str)));
        fields
    }
}

pub fn milestone(milestone: Milestone, task: Uuid, quote: Option<Uuid>, details: MilestoneFields) {
    bind(task, quote);
    emit_milestone(milestone, Some(task), quote, details);
}
pub(crate) fn emit_milestone(milestone: Milestone, task: Option<Uuid>, quote: Option<Uuid>, details: MilestoneFields) {
    let trace = current();
    if let Some(trace) = &trace {
        trace.summary.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).record(milestone, &details);
    }
    let mut fields = details.into_map();
    fields.insert("milestone".into(), json!(milestone.as_str()));
    if milestone == Milestone::PriceReady {
        let trace = trace.as_deref();
        fields.insert("catalog_cache".into(), json!(trace.and_then(Trace::catalog_cache).map(Cache::as_str)));
        fields.insert("parser_cache".into(), json!(trace.and_then(Trace::parser_cache).map(Cache::as_str)));
    }
    emit("milestone", trace.as_deref(), task, quote, fields);
}

/// Emits the trace's cost and infrastructure counters accrued after its
/// request line, as from an event stream.
pub fn flush() {
    if let Some(trace) = current() {
        flush_trace(&trace);
    }
}
/// Ends a server worker's segment: its cost line and, when persistence is
/// on, its row.
pub fn finish_worker(pool: &sqlx::PgPool, elapsed: Duration) {
    if let Some(trace) = current() {
        let snapshot = flush_trace(&trace);
        if let Some(segment @ (Segment::Execution | Segment::RetryAnalysis)) = trace.segment() {
            persist::record(pool, &trace, segment, &snapshot, elapsed, None);
        }
    }
}
fn flush_trace(trace: &Trace) -> cost::CostSnapshot {
    let snapshot = trace.cost.take();
    let mut fields = Map::new();
    fields.insert("segment".into(), json!(trace.segment().map(Segment::as_str)));
    for part in [snapshot.model_json(), snapshot.source_json(), snapshot.infra_json()] {
        if let Value::Object(part) = part {
            fields.extend(part);
        }
    }
    fields.insert("db".into(), trace.take_db());
    emit("cost", Some(trace), None, None, fields);
    snapshot
}

fn emit(kind: &'static str, trace: Option<&Trace>, task: Option<Uuid>, quote: Option<Uuid>, fields: Map<String, Value>) {
    let mut line = Map::new();
    line.insert("schema".into(), json!(SCHEMA));
    line.insert("kind".into(), json!(kind));
    line.insert("trace".into(), json!(trace.map(|trace| trace.id)));
    line.insert("origin".into(), json!(trace.map(|trace| trace.origin.as_str())));
    line.insert("surface".into(), json!(trace.map(|trace| trace.surface().as_str())));
    line.insert("task".into(), json!(task.or_else(|| trace.and_then(Trace::task))));
    line.insert("quote".into(), json!(quote.or_else(|| trace.and_then(Trace::quote))));
    line.extend(fields);
    let line = Value::Object(line);
    tracing::info!(target: TARGET, telemetry = %line, "telemetry");
}

/// A client trace id: a version 4 UUID from the header or, for EventSource,
/// which cannot send headers, from an unsigned `trace` query parameter.
fn client_trace(request: &Request) -> Option<Uuid> {
    let header = request.headers().get(TRACE_HEADER).and_then(|value| value.to_str().ok());
    let query = || {
        request.uri().query()?.split('&').find_map(|pair| pair.strip_prefix("trace="))
    };
    header
        .or_else(query)
        .and_then(|value| Uuid::parse_str(value.trim()).ok())
        .filter(|id| id.get_version_num() == 4)
}
fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers.get(axum::http::header::CONTENT_LENGTH).and_then(|value| value.to_str().ok()?.parse().ok())
}

/// Opens a trace for every request and closes it with one `request` line,
/// plus a `cost` line when the request used a model or a paid source. The
/// route is the matched template, never the concrete path.
pub async fn middleware(State(pool): State<sqlx::PgPool>, request: Request, next: Next) -> Response {
    if request.method() == Method::OPTIONS {
        return next.run(request).await;
    }
    let trace = Arc::new(match client_trace(&request) {
        Some(id) => Trace::new(id, Origin::Client),
        None => Trace::new(Uuid::new_v4(), Origin::Generated),
    });
    let route = request.extensions().get::<MatchedPath>().map_or_else(|| "fallback".to_owned(), |path| path.as_str().to_owned());
    let method = match *request.method() {
        Method::GET => "GET",
        Method::POST => "POST",
        _ => "other",
    };
    let bytes_in = content_length(request.headers());
    let (pool_size, pool_idle) = (pool.size(), pool.num_idle());
    let started = Instant::now();
    let mut response = TRACE.scope(trace.clone(), next.run(request)).await;
    let elapsed = started.elapsed();
    if let Ok(value) = HeaderValue::from_str(&trace.id.to_string()) {
        response.headers_mut().insert(TRACE_HEADER, value);
    }
    let status = response.status().as_u16();
    let class = ErrorClass::from_status(status);
    let snapshot = trace.cost.take();
    let mut fields = Map::new();
    fields.insert("phase".into(), json!(Phase::Request.as_str()));
    fields.insert("route".into(), json!(route));
    fields.insert("method".into(), json!(method));
    fields.insert("status".into(), json!(status));
    let bytes_out = content_length(response.headers());
    fields.insert("ms".into(), json!(elapsed.as_millis() as u64));
    fields.insert("ok".into(), json!(class.is_none()));
    fields.insert("error_class".into(), json!(class.map(ErrorClass::as_str)));
    fields.insert("bytes_in".into(), json!(bytes_in));
    fields.insert("bytes_out".into(), json!(bytes_out));
    fields.insert("pool_size".into(), json!(pool_size));
    fields.insert("pool_idle".into(), json!(pool_idle));
    if let Value::Object(infra) = snapshot.infra_json() {
        fields.extend(infra);
    }
    fields.insert("db".into(), trace.take_db());
    emit("request", Some(&trace), None, None, fields);
    if snapshot.has_cost() {
        let mut cost = Map::new();
        cost.insert("segment".into(), json!(trace.segment().map(Segment::as_str)));
        for part in [snapshot.model_json(), snapshot.source_json()] {
            if let Value::Object(part) = part {
                cost.extend(part);
            }
        }
        emit("cost", Some(&trace), None, None, cost);
    }
    // A question or follow-up is priced inside its request; a worker owns
    // the execution segment's row.
    if let Some(segment @ (Segment::Question | Segment::FollowUp)) = trace.segment() {
        persist::record(&pool, &trace, segment, &snapshot, elapsed, Some((bytes_in, bytes_out)));
    }
    response
}

#[cfg(test)]
pub(crate) mod capture {
    //! Collects every log line written while a test runs.
    use std::{
        io::Write,
        sync::{Arc, Mutex},
    };

    #[derive(Clone, Default)]
    pub struct Lines(Arc<Mutex<Vec<u8>>>);
    impl Write for Lines {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Lines {
        pub fn install(&self) -> tracing::subscriber::DefaultGuard {
            let writer = self.clone();
            tracing::subscriber::set_default(tracing_subscriber::fmt().json().with_writer(move || writer.clone()).finish())
        }
        pub fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
        /// The parsed payloads of every telemetry line.
        pub fn telemetry(&self) -> Vec<serde_json::Value> {
            self.text()
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .filter(|line| line["target"] == super::TARGET)
                .map(|line| serde_json::from_str(line["fields"]["telemetry"].as_str().unwrap()).unwrap())
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, routing::get, Router};
    use tower::ServiceExt;

    fn app() -> Router {
        let pool = sqlx::postgres::PgPoolOptions::new().connect_lazy("postgresql://unused@127.0.0.1:1/unused").unwrap();
        Router::new()
            .route("/probe/:id", get(|| async {
                let inner = spawn(async { id().map(|id| id.to_string()) }).await.unwrap();
                let span = span(Phase::View).label("probe").task(Uuid::nil());
                drop(db("tasks.load"));
                span.finish(Outcome::Ok);
                inner.unwrap_or_default()
            }))
            .layer(axum::middleware::from_fn_with_state(pool, middleware))
    }
    async fn call(header: Option<&str>) -> (String, String) {
        let mut request = axum::http::Request::get("/probe/Quokka-Canary-4f1c").body(Body::empty()).unwrap();
        if let Some(header) = header {
            request.headers_mut().insert(TRACE_HEADER, HeaderValue::from_str(header).unwrap());
        }
        let response = app().oneshot(request).await.unwrap();
        let echoed = response.headers()[TRACE_HEADER].to_str().unwrap().to_owned();
        let body = axum::body::to_bytes(response.into_body(), 1024).await.unwrap();
        (echoed, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn a_client_trace_reaches_spawned_work_and_every_line() {
        let lines = capture::Lines::default();
        let _guard = lines.install();
        let client = Uuid::new_v4().to_string();
        let (echoed, inner) = call(Some(&client)).await;
        assert_eq!(echoed, client);
        assert_eq!(inner, client, "spawned work keeps the request trace");
        let telemetry = lines.telemetry();
        assert_eq!(telemetry.len(), 2);
        assert!(telemetry.iter().all(|line| line["trace"] == client.as_str() && line["origin"] == "client"));
        let request = telemetry.iter().find(|line| line["kind"] == "request").unwrap();
        assert_eq!(request["route"], "/probe/:id");
        assert_eq!(request["db_calls"], 1);
        assert_eq!(request["db"]["tasks.load"]["calls"], 1);
        assert_eq!(request["bytes_in"], Value::Null, "unmeasured, not zero");
        assert!(!lines.text().contains("Quokka"), "the concrete path never reaches telemetry");
    }

    #[tokio::test]
    async fn an_event_stream_passes_its_trace_as_a_query_parameter() {
        let client = Uuid::new_v4();
        let request = axum::http::Request::get(format!("/probe/task?owner=x&trace={client}")).body(Body::empty()).unwrap();
        let response = app().oneshot(request).await.unwrap();
        assert_eq!(response.headers()[TRACE_HEADER].to_str().unwrap(), client.to_string());
    }

    #[tokio::test]
    async fn an_invalid_client_trace_is_replaced_by_a_generated_one() {
        let lines = capture::Lines::default();
        let _guard = lines.install();
        for header in [None, Some("not-a-uuid"), Some("00000000-0000-1000-8000-000000000000")] {
            let (echoed, inner) = call(header).await;
            let id = Uuid::parse_str(&echoed).unwrap();
            assert_eq!(id.get_version_num(), 4);
            assert_ne!(Some(echoed.as_str()), header);
            assert_eq!(inner, echoed);
        }
        assert!(lines.telemetry().iter().all(|line| line["origin"] == "generated"));
    }

    #[tokio::test]
    async fn an_unfinished_span_reports_dropped_and_work_outside_a_trace_still_logs() {
        let lines = capture::Lines::default();
        let _guard = lines.install();
        drop(span(Phase::Plan));
        let result: Result<(), anyhow::Error> = timed(Phase::Catalog, "fresh", async { Err(sqlx::Error::PoolTimedOut.into()) }).await;
        assert!(result.is_err());
        let telemetry = lines.telemetry();
        assert_eq!(telemetry[0]["error_class"], "dropped");
        assert_eq!(telemetry[0]["trace"], Value::Null);
        assert_eq!(telemetry[1]["error_class"], "busy");
        assert!(!lines.text().contains("pool timed out"), "error messages never reach telemetry");
    }

    #[tokio::test]
    async fn a_spawned_segment_keeps_the_trace_but_counts_its_own_costs() {
        let parent = Arc::new(Trace::new(Uuid::new_v4(), Origin::Client));
        let task = Uuid::new_v4();
        let (parent_id, child_id, child_segment, child_task) = scope(parent.clone(), async move {
            set_surface(Surface::App);
            bind(task, None);
            drop(db("tasks.load"));
            let child = spawn_segment(Segment::Execution, async {
                drop(db("tasks.save"));
                let trace = current().unwrap();
                (trace.id, trace.segment(), trace.task(), trace.surface())
            }).await.unwrap();
            (current().unwrap().id, child.0, child.1, (child.2, child.3))
        }).await;
        assert_eq!(parent_id, child_id);
        assert_eq!(child_segment, Some(Segment::Execution));
        assert_eq!(child_task, (Some(task), Surface::App));
        assert_eq!(parent.segment(), None, "the request keeps its own segment");
        assert_eq!(parent.cost.take().db_calls, 1, "the child's calls are not the request's");
    }

    #[tokio::test]
    async fn milestones_and_worker_flushes_carry_the_bound_task_and_cost() {
        let lines = capture::Lines::default();
        let _guard = lines.install();
        let trace = Arc::new(Trace::new(Uuid::new_v4(), Origin::Client));
        let (task, quote) = (Uuid::new_v4(), Uuid::new_v4());
        scope(trace.clone(), async move {
            set_catalog_cache(Cache::Hit);
            milestone(Milestone::PriceReady, task, Some(quote), MilestoneFields { query_class: Some(QueryClass::SimpleLookup), sources: Some(2), cap_micro_usd: Some(54_348), ..Default::default() });
            spawn(async {
                model_attempt(ModelAttempt { surface: "analysis", attempt: 1, model: "fixture", body: Some(&json!({"usage":{"prompt_tokens":5,"completion_tokens":1}})),
                    input_bytes: 10, output_bytes: 2, elapsed: Duration::from_millis(4), outcome: Outcome::Ok, reason: "validated" });
                set_segment(Segment::Execution);
                flush();
            }).await.unwrap();
        }).await;
        let telemetry = lines.telemetry();
        let price = &telemetry[0];
        assert_eq!(price["milestone"], "price_ready");
        assert_eq!(price["sources"], "multiple");
        assert_eq!(price["catalog_cache"], "hit");
        assert_eq!(price["parser_cache"], Value::Null);
        let cost = telemetry.iter().find(|line| line["kind"] == "cost").unwrap();
        assert_eq!(cost["task"], task.to_string());
        assert_eq!(cost["quote"], quote.to_string());
        assert_eq!(cost["segment"], "execution");
        assert_eq!(cost["prompt_tokens"], 5);
        assert_eq!(cost["source_calls"], 0);
        assert_eq!(cost["customer_charged_micro_usd"], 0);
    }
}
