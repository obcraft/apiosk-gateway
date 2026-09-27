//! Versioned answer contract: `answer_schema_version` 2 (backlog #55).
//!
//! Types only. The analysis model proposes blocks, derivations, requirement
//! statuses and missing information that point at saved evidence
//! (`result_ref` + JSON pointer, optional exact `quote`) or at a derivation.
//! Fields documented as "Server" are rebuilt by gateway code from the saved
//! source documents; a model-supplied value there is always discarded.
//!
//! Validation lives elsewhere: blocks in `answer_presentation.rs` (lane
//! OUTPUT), derivations in `derivations.rs` and requirements in
//! `completeness.rs` (lane ANALYSIS). Change this module only through the
//! integrator, and only additively within one schema version.
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Analyses without `answer_schema_version` are version 1 (one `presentation`).
pub const ANSWER_SCHEMA_VERSION: u8 = 2;
pub const MAX_BLOCKS: usize = 12;
/// Serialized `blocks` + `derivations` of one analysis. The task document,
/// including six archived turns, must stay within `Task::bounded` (1 MiB).
pub const MAX_BLOCKS_BYTES: usize = 96 * 1024;
pub const MAX_QUOTE_BYTES: usize = 1024;
pub const MAX_COLLECTION_ROWS: usize = 200;
pub const MAX_EXPLICIT_ROWS: usize = 50;
pub const MAX_COLUMNS: usize = 8;
pub const MAX_ENTITIES: usize = 8;
pub const MAX_KPI_CARDS: usize = 8;
pub const MAX_SERIES: usize = 6;
pub const MAX_SERIES_POINTS: usize = 400;
pub const MAX_BARS: usize = 200;
pub const MAX_MAP_FEATURES: usize = 500;
pub const MAX_MAP_COORDINATES: usize = 5_000;
pub const MAX_TIMELINE_EVENTS: usize = 200;
pub const MAX_PROFILE_ITEMS: usize = 24;
pub const MAX_RELATION_NODES: usize = 60;
pub const MAX_RELATION_EDGES: usize = 120;

fn is_false(value: &bool) -> bool {
    !*value
}

/// `answer_schema_version` of a stored analysis; 1 when absent.
pub fn schema_version(analysis: &Value) -> u64 {
    analysis["answer_schema_version"].as_u64().unwrap_or(1)
}

/// Version-1 `presentation` kept on version-2 analyses for older App bundles.
/// Kind only: never a second copy of the rows.
pub fn legacy_presentation_stub(kind: &str) -> Value {
    json!({"kind": kind, "superseded_by": "blocks"})
}

// ---------------------------------------------------------------------------
// References, facts and qualifiers
// ---------------------------------------------------------------------------

/// Evidence (`result_ref` + `pointer`, optional `quote`) xor `derivation_id`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ValueRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<String>,
    /// Exact contiguous excerpt of a long text field, at most MAX_QUOTE_BYTES.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derivation_id: Option<String>,
}

impl ValueRef {
    pub fn evidence(result_ref: impl Into<String>, pointer: impl Into<String>) -> Self {
        Self { result_ref: Some(result_ref.into()), pointer: Some(pointer.into()), ..Self::default() }
    }
    pub fn derivation(id: impl Into<String>) -> Self {
        Self { derivation_id: Some(id.into()), ..Self::default() }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueState {
    /// Source value copied unchanged.
    #[default]
    Reported,
    /// The source states numeric zero.
    Zero,
    /// Null in the source or absent from the record: unknown, never zero.
    Missing,
    /// Server-computed; see the derivation formula.
    Derived,
}

impl ValueState {
    pub fn is_reported(&self) -> bool {
        *self == Self::Reported
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlagKind {
    Estimated,
    Provisional,
    Revised,
    Rounded,
    Confidential,
}

/// A property the source states about a value. Never inferred by the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactFlag {
    pub kind: FlagKind,
    pub result_ref: String,
    pub pointer: String,
    /// Server: the flag field's source value.
    #[serde(default)]
    pub value: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualifierBasis {
    /// Resolved from a saved source field.
    Evidence,
    /// Inherited from derivation inputs that all agree.
    Derivation,
    /// Defined by the operation: percent, count or ratio.
    Operation,
}

/// Unit or currency. Model: a reference. Server: `value` and `basis`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Qualifier {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_ref: Option<String>,
    /// Absolute, or relative to each record inside a collection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derivation_id: Option<String>,
    /// Server: "EUR", "residents", "percent".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<QualifierBasis>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Granularity {
    Day,
    Month,
    Quarter,
    HalfYear,
    Year,
    Range,
    /// Displayable, never an axis or comparison basis.
    Unparsed,
}

/// Reporting period. Model: a reference. Server: every other field.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PeriodQualifier {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_ref: Option<String>,
    /// Period label or start date; absolute, or relative inside a collection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<String>,
    /// Optional explicit end date field in the same record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_pointer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derivation_id: Option<String>,
    /// Server: source text, e.g. "2024", "2024MM03", "2023-07-01".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Server: comparison and ordering key: "2024", "2024-03", "2024-Q1",
    /// "2024-H1", "2024-03-31" or "2023-07-01/2024-06-30".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub granularity: Option<Granularity>,
    /// Server: only when the source states exact dates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<QualifierBasis>,
}

/// One displayed value. Model: origin plus qualifier references. Server: the
/// rest. Inside a collection-backed block the origin and `field` are omitted
/// and implied: `collection.result_ref` + `{rows_pointer}/{row_index}{pointer}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Fact {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derivation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<Qualifier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<Qualifier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<PeriodQualifier>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<FactFlag>,
    /// Server: source scalar (or the verified quote), derivation result, or null.
    #[serde(default)]
    pub value: Value,
    /// Server.
    #[serde(default, skip_serializing_if = "ValueState::is_reported")]
    pub state: ValueState,
    /// Server: display rounding for derived values; `value` stays exact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rounded_to: Option<u8>,
    /// Server: source field key, for display formatting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Version-1 cells only: source display name. Version 2 uses `source_index`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl Fact {
    pub fn evidence(result_ref: impl Into<String>, pointer: impl Into<String>) -> Self {
        Self { result_ref: Some(result_ref.into()), pointer: Some(pointer.into()), ..Self::default() }
    }
    pub fn derived(derivation_id: impl Into<String>) -> Self {
        Self { derivation_id: Some(derivation_id.into()), state: ValueState::Derived, ..Self::default() }
    }
    pub fn origin(&self) -> ValueRef {
        ValueRef {
            result_ref: self.result_ref.clone(),
            pointer: self.pointer.clone(),
            quote: self.quote.clone(),
            derivation_id: self.derivation_id.clone(),
        }
    }
}

/// A qualitative label with an optional fact; `fact: None` is an explicit unknown.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LabeledFact {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fact: Option<Fact>,
}

// ---------------------------------------------------------------------------
// Collections, coverage and source status
// ---------------------------------------------------------------------------

/// A saved source array. `row_indices` is an explicit relevance selection over
/// the whole array, never "the first N rows".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Collection {
    pub result_ref: String,
    /// Absolute, under /data/.
    pub rows_pointer: String,
    pub row_indices: Vec<usize>,
    /// Absolute pointer to the source-reported total, e.g. "/data/totalElements".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_pointer: Option<String>,
    /// Absolute pointer to a next-page marker; non-null means paginated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_pointer: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageScope {
    /// Every record the source has for this query is shown.
    Complete,
    /// Every fetched record is shown; the source reports or signals more.
    FetchedPage,
    /// A relevance selection from the fetched records.
    Selection,
    /// The source states no total.
    Unknown,
}

/// Server-computed counts; never a model claim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Coverage {
    pub selected: usize,
    pub fetched: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_available: Option<u64>,
    pub paginated: bool,
    pub scope: CoverageScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceState {
    Completed,
    Partial,
    Empty,
    Pending,
    Failed,
    Skipped,
}

/// Server: the plan step behind a block (#30). Pending or failed marks a
/// missing part; it is never a negative finding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceStatus {
    /// `Step::key()`: "<service_id>:<entity_ref>".
    pub step_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_ref: Option<String>,
    pub source_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_label: Option<String>,
    pub state: SourceState,
}

/// Server: one entry per saved document the analysis may cite.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SourceIndexEntry {
    pub result_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieved_at: Option<String>,
    /// Document coverage: "source_response", "partial" or "empty".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_ref: Option<String>,
}

// ---------------------------------------------------------------------------
// Blocks
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockProducer {
    #[default]
    Model,
    /// Gateway or App code without a model (direct answers, provisional views).
    Deterministic,
    /// Mapped from a validated version-1 presentation.
    LegacyPresentation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    Narrative,
    Summary,
    DataTable,
    Comparison,
    Kpi,
    LineChart,
    BarChart,
    Map,
    Timeline,
    Profile,
    Relationship,
}

/// One answer component. Serialized flat: `{"id":..,"kind":..,<kind fields>}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnswerBlock {
    /// ^b[0-9]{1,3}$, unique within the answer.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requirement_ids: Vec<String>,
    /// Qualitative caveats for this block; same prose rules as limitations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// Server.
    #[serde(default)]
    pub provisional: bool,
    /// Server.
    #[serde(default)]
    pub produced_by: BlockProducer,
    /// Server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<Coverage>,
    /// Server.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_status: Vec<SourceStatus>,
    #[serde(flatten)]
    pub body: BlockBody,
}

impl AnswerBlock {
    pub fn new(id: impl Into<String>, body: BlockBody) -> Self {
        Self {
            id: id.into(),
            title: None,
            requirement_ids: Vec::new(),
            notes: Vec::new(),
            provisional: false,
            produced_by: BlockProducer::Model,
            coverage: None,
            source_status: Vec::new(),
            body,
        }
    }
    pub fn narrative(id: impl Into<String>, observation_ids: Vec<String>) -> Self {
        Self::new(id, BlockBody::Narrative(NarrativeBlock { observation_ids }))
    }
    pub fn kind(&self) -> BlockKind {
        match &self.body {
            BlockBody::Narrative(_) => BlockKind::Narrative,
            BlockBody::Summary(_) => BlockKind::Summary,
            BlockBody::DataTable(_) => BlockKind::DataTable,
            BlockBody::Comparison(_) => BlockKind::Comparison,
            BlockBody::Kpi(_) => BlockKind::Kpi,
            BlockBody::LineChart(_) => BlockKind::LineChart,
            BlockBody::BarChart(_) => BlockKind::BarChart,
            BlockBody::Map(_) => BlockKind::Map,
            BlockBody::Timeline(_) => BlockKind::Timeline,
            BlockBody::Profile(_) => BlockKind::Profile,
            BlockBody::Relationship(_) => BlockKind::Relationship,
        }
    }
}

// Blocks are built once per analysis and serialized; variant size does not
// matter, and boxing would complicate every pattern match in the validators.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BlockBody {
    Narrative(NarrativeBlock),
    Summary(SummaryBlock),
    DataTable(DataTableBlock),
    Comparison(ComparisonBlock),
    Kpi(KpiBlock),
    LineChart(LineChartBlock),
    BarChart(BarChartBlock),
    Map(MapBlock),
    Timeline(TimelineBlock),
    Profile(ProfileBlock),
    Relationship(RelationshipBlock),
}

// ----- narrative and summary (#54) -----

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NarrativeBlock {
    /// "o1".. in observation order.
    pub observation_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConclusionScope {
    #[default]
    Partial,
    Complete,
}

/// Prose without digits or links; `{{a}}`..`{{d}}` are replaced by `facts[0..4]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Narrative {
    pub text: String,
    #[serde(default)]
    pub facts: Vec<Fact>,
    /// Server: complete only when completeness is complete and nothing is provisional.
    #[serde(default)]
    pub scope: ConclusionScope,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SummaryBlock {
    /// Never present on a provisional answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conclusion: Option<Narrative>,
    /// "o1"..
    #[serde(default)]
    pub finding_ids: Vec<String>,
    /// "m1".., "l1".. or unknown "r1"..
    #[serde(default)]
    pub open_point_ids: Vec<String>,
    /// The PDF renders this same validated analysis; no second analysis.
    #[serde(default)]
    pub offer_report: bool,
}

// ----- data table (#45) -----

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnRole {
    #[default]
    Text,
    Label,
    Number,
    Amount,
    Date,
    Link,
    Identifier,
    Status,
    Category,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TableColumn {
    pub id: String,
    pub label: String,
    /// Collection mode: pointer relative to each record, e.g. "/name".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<String>,
    /// Drives App filters: category/status facets, number/amount/date ranges.
    #[serde(default)]
    pub role: ColumnRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<Qualifier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<Qualifier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<PeriodQualifier>,
    /// Column choice: available but not shown initially.
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TableRow {
    /// Server, collection mode: index in the source array.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_index: Option<usize>,
    /// One per column; None is unknown, never zero.
    pub cells: Vec<Option<Fact>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDirection {
    #[default]
    Desc,
    Asc,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SortSpec {
    pub column_id: String,
    #[serde(default)]
    pub direction: SortDirection,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DataTableBlock {
    /// Collection mode. Absent: explicit mode with model-written facts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<Collection>,
    pub columns: Vec<TableColumn>,
    /// Collection mode: server. Explicit mode: model facts, server-resolved.
    #[serde(default)]
    pub rows: Vec<TableRow>,
    /// Initial order; the App sorts the complete selected set before paging.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<SortSpec>,
}

// ----- comparison (#46) -----

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ComparedEntity {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<Fact>,
    /// Intent subject index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BasisIssue {
    Unit,
    Currency,
    Period,
    Definition,
    QualifiersUnknown,
}

/// Server. Differences are named, not hidden; there is no winner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowBasis {
    pub comparable: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<BasisIssue>,
    pub values_differ: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ComparisonRow {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requirement_id: Option<String>,
    /// One per entity; None is unknown.
    pub cells: Vec<Option<Fact>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<RowBasis>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ComparisonBlock {
    pub entities: Vec<ComparedEntity>,
    pub rows: Vec<ComparisonRow>,
}

// ----- KPI cards (#47) -----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeBasis {
    PriorPeriod,
    OtherEntity,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KpiChange {
    /// A difference, or a percentage of a difference, over (fact, baseline).
    pub derivation_id: String,
    pub baseline: Fact,
    /// Server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Fact>,
    /// Server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<ChangeBasis>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KpiCard {
    pub id: String,
    pub label: String,
    pub fact: Fact,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<KpiChange>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KpiBlock {
    pub cards: Vec<KpiCard>,
}

// ----- line chart (#48) -----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapReason {
    NotInSource,
    NullInSource,
    SourceFailed,
    SourcePending,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SeriesPoint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_index: Option<usize>,
    /// X position. Server-inserted gap points carry only `key` and `granularity`.
    pub period: PeriodQualifier,
    /// None: explicit gap. Never interpolated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fact: Option<Fact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<GapReason>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Series {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<Collection>,
    /// Collection mode, relative to each record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_pointer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_pointer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<Qualifier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<Qualifier>,
    /// Server-ordered chronologically, including explicit gaps.
    #[serde(default)]
    pub points: Vec<SeriesPoint>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LineChartBlock {
    pub value_label: String,
    pub series: Vec<Series>,
    /// Server: one granularity for every point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub granularity: Option<Granularity>,
    /// Server: shared by every series.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<Qualifier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<Qualifier>,
}

// ----- bar chart / ranking (#49) -----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RankingScope {
    Selection,
    Fetched,
    TotalAvailable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ranking {
    #[serde(default)]
    pub direction: SortDirection,
    /// Model claim; the server rejects a scope wider than the coverage.
    pub scope: RankingScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derivation_id: Option<String>,
}

/// Server: fixed over every bar in the block, zero included.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Scale {
    pub min: f64,
    pub max: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Bar {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_fact: Option<Fact>,
    /// None or a null value: unknown, no bar, not ranked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fact: Option<Fact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_index: Option<usize>,
    /// Server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BarChartBlock {
    pub value_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<Collection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_pointer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_pointer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<Qualifier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<Qualifier>,
    #[serde(default)]
    pub bars: Vec<Bar>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ranking: Option<Ranking>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<Scale>,
}

// ----- map (#50) -----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeometryFormat {
    Geojson,
    WktPoint,
    LatLon,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Crs {
    #[serde(rename = "EPSG:4326")]
    Epsg4326,
    #[serde(rename = "EPSG:28992")]
    Epsg28992,
    #[serde(rename = "EPSG:3857")]
    Epsg3857,
}

/// Source coordinates only; no geocoding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeometryRef {
    /// Omitted inside a collection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_ref: Option<String>,
    /// GeoJSON geometry object, WKT POINT string, or latitude (with lon_pointer).
    pub pointer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lon_pointer: Option<String>,
    pub format: GeometryFormat,
    /// Source field that states the CRS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crs_pointer: Option<String>,
    /// Server: GeoJSON geometry in the source CRS, coordinates unchanged.
    #[serde(default)]
    pub geometry: Value,
    /// Server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crs: Option<Crs>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MapFeature {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_fact: Option<Fact>,
    /// None: location unknown; the feature stays in the list view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<GeometryRef>,
    #[serde(default)]
    pub facts: Vec<LabeledFact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_index: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapAsset {
    pub result_ref: String,
    pub asset_index: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MapBlock {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<Collection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_pointer: Option<String>,
    /// Collection mode: template with pointers relative to each record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<GeometryRef>,
    #[serde(default)]
    pub features: Vec<MapFeature>,
    /// An existing captured map image of the same result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset: Option<MapAsset>,
    /// Server: one CRS for every feature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crs: Option<Crs>,
}

// ----- timeline (#51) -----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DateKind {
    Published,
    Effective,
    Deadline,
    Filed,
    Registered,
    Decision,
    Start,
    End,
    PeriodEnd,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatePrecision {
    Year,
    Month,
    Day,
    DateTime,
    Unknown,
}

/// Server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventTime {
    pub precision: DatePrecision,
    /// ISO date or RFC 3339 instant from the source; None when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_key: Option<String>,
    /// Only when the source states an offset; None renders a floating local date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utc_offset: Option<String>,
    /// IANA zone from a source field only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineEvent {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_fact: Option<Fact>,
    pub date_kind: DateKind,
    /// None or a null value: unknown date. Never estimated or filled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<Fact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<Fact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<Fact>,
    #[serde(default)]
    pub facts: Vec<LabeledFact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<EventTime>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TimelineBlock {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<Collection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_pointer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_pointer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_pointer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_kind: Option<DateKind>,
    #[serde(default)]
    pub events: Vec<TimelineEvent>,
}

// ----- profile / dossier (#52) -----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckOutcome {
    /// Evidence states the checked property.
    Confirmed,
    /// Evidence states the opposite.
    Contradicted,
    /// Complete source response without a match: bounded to that source's scope.
    NoMatchInScope,
    /// Pending, failed, partial or empty coverage. A finished call is not a pass.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileCheck {
    pub id: String,
    pub label: String,
    /// Model claim; the server downgrades unsupported outcomes to unknown.
    pub outcome: CheckOutcome,
    #[serde(default)]
    pub evidence: Vec<Fact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requirement_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_key: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SourceFreshness {
    pub result_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieved_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of: Option<Fact>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProfileBlock {
    pub subject_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<Fact>,
    #[serde(default)]
    pub identifiers: Vec<LabeledFact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<LabeledFact>,
    #[serde(default)]
    pub attributes: Vec<LabeledFact>,
    #[serde(default)]
    pub checks: Vec<ProfileCheck>,
    /// Server.
    #[serde(default)]
    pub freshness: Vec<SourceFreshness>,
}

// ----- relationship diagram (#53) -----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Organisation,
    Person,
    Address,
    Asset,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    Ownership,
    Control,
    Officer,
    Parent,
    Subsidiary,
    RegisteredAddress,
    Contract,
    Other,
}

/// Server. Equal names alone are never a basis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityBasis {
    SameRecord,
    IdentifierMatch,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelationNode {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<Fact>,
    pub node_kind: NodeKind,
    #[serde(default)]
    pub identifiers: Vec<LabeledFact>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelationEdge {
    pub id: String,
    pub from: String,
    pub to: String,
    pub relation: RelationKind,
    pub label: String,
    pub directed: bool,
    pub evidence: Vec<Fact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<Fact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_to: Option<Fact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_basis: Option<IdentityBasis>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RelationshipBlock {
    #[serde(default)]
    pub nodes: Vec<RelationNode>,
    #[serde(default)]
    pub edges: Vec<RelationEdge>,
}

// ---------------------------------------------------------------------------
// Derivations (#41)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivationOp {
    Sum,
    Difference,
    Percentage,
    Ratio,
    Rank,
    Count,
    Min,
    Max,
    Average,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingPolicy {
    #[default]
    Fail,
    /// Skip nulls, list them in `excluded`, mark the result incomplete.
    Exclude,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZeroDivisorPolicy {
    #[default]
    Fail,
    NullResult,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeriodRule {
    /// Every input has the same period key.
    #[default]
    Same,
    /// Distinct, non-overlapping keys of one granularity (flows only).
    Span,
    /// Exactly two distinct keys of one granularity.
    Compare,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DerivationInputValue {
    pub reference: ValueRef,
    #[serde(default)]
    pub value: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RankEntry {
    pub rank: u32,
    /// Position in `inputs` (or in the resolved collection rows).
    pub input: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_index: Option<usize>,
    #[serde(default)]
    pub value: Value,
}

/// Server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DerivationResult {
    /// JSON number when exact within 15 significant digits, else a decimal string; null if missing.
    #[serde(default)]
    pub value: Value,
    pub state: ValueState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<Qualifier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<Qualifier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<PeriodQualifier>,
    #[serde(default)]
    pub inputs: Vec<DerivationInputValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<usize>,
    /// False with exclusions or incomplete collection coverage.
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<Coverage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ranking: Vec<RankEntry>,
    /// Rendered from field keys, source names and period keys; no model text.
    pub formula: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Derivation {
    /// ^d[0-9]{1,3}$. Inputs may reference earlier derivations only.
    pub id: String,
    pub op: DerivationOp,
    #[serde(default)]
    pub inputs: Vec<ValueRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<Collection>,
    /// With `collection`: numeric field relative to each record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_pointer: Option<String>,
    #[serde(default)]
    pub missing: MissingPolicy,
    #[serde(default)]
    pub zero_divisor: ZeroDivisorPolicy,
    #[serde(default)]
    pub periods: PeriodRule,
    /// Rank order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<SortDirection>,
    /// Display rounding only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decimals: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<DerivationResult>,
}

// ---------------------------------------------------------------------------
// Completeness (#42), missing information and clarification (#43)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriterionKind {
    Capability,
    Operation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementStatus {
    Answered,
    Unknown,
    NotApplicable,
    Pending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownReason {
    NotInSource,
    SourceFailed,
    SourcePending,
    NotFetched,
    AmbiguousIdentity,
    EntityMismatch,
    PeriodUnavailable,
    PeriodMismatch,
    OutsideCoverage,
    NotAddressed,
}

/// Generated by the server from the intent; the model only claims a status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Requirement {
    /// ^r[0-9]{1,3}$.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_ref: Option<String>,
    pub criterion: String,
    pub criterion_kind: CriterionKind,
    /// Requested period key, e.g. "2024".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<String>,
    pub status: RequirementStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<UnknownReason>,
    /// "o1", block ids, derivation ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answered_by: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_ids: Vec<String>,
    /// Required for not_applicable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Fact>,
}

/// Model output item under `requirement_status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequirementClaim {
    pub requirement_id: String,
    pub status: RequirementStatus,
    #[serde(default)]
    pub answered_by: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<UnknownReason>,
    #[serde(default)]
    pub missing_ids: Vec<String>,
    #[serde(default)]
    pub evidence: Vec<Fact>,
}

/// Server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Completeness {
    pub requirements: Vec<Requirement>,
    /// Every requirement answered or not_applicable.
    pub complete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingKind {
    EntityIdentity,
    Period,
    Criterion,
    SourceScope,
    Document,
    Value,
    SourceOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    AskUser,
    /// Display only: any purchase is a separate, visible, priced action.
    NewSourceCall,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissingInformation {
    /// ^m[0-9]{1,2}$.
    pub id: String,
    pub kind: MissingKind,
    #[serde(default)]
    pub requirement_ids: Vec<String>,
    /// Prose rules: no digits, no links.
    pub description: String,
    pub blocking: bool,
    pub resolution: Resolution,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClarificationTemplate {
    WhichEntity,
    WhichPeriod,
    WhichCriterion,
    WhichSource,
    NarrowSearch,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClarificationOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fact: Option<Fact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_ref: Option<String>,
}

/// One focused question in the same analysis output; `follow_up_question`
/// mirrors `question` for version-1 readers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clarification {
    pub question: String,
    pub template: ClarificationTemplate,
    #[serde(default)]
    pub missing_ids: Vec<String>,
    #[serde(default)]
    pub options: Vec<ClarificationOption>,
}

// ---------------------------------------------------------------------------
// Validation outcome (#44)
// ---------------------------------------------------------------------------

/// Fixed diagnostic codes: safe for logs and targeted repair prompts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockIssue {
    Schema,
    UnsupportedKind,
    DuplicateId,
    InvalidLabel,
    UnknownReference,
    UnknownResultRef,
    InvalidPointer,
    MissingField,
    NonScalar,
    QuoteRequired,
    QuoteNotVerbatim,
    UnknownDerivation,
    DerivationFailed,
    UnitMismatch,
    CurrencyMismatch,
    PeriodMismatch,
    UnparsedPeriod,
    DuplicatePeriod,
    IncompatibleSeries,
    MissingRowSelection,
    RowOutOfRange,
    DuplicateRow,
    WidthMismatch,
    TooManyItems,
    TooLarge,
    RankingScopeExceedsCoverage,
    ChangeWithoutBasis,
    FlagWithoutSource,
    UnsupportedCrs,
    InvalidGeometry,
    MissingRelationEvidence,
    NameOnlyRelation,
    ConclusionOnIncomplete,
}

impl BlockIssue {
    pub fn code(self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockRejection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<BlockKind>,
    pub issue: BlockIssue,
    /// Location inside the block, e.g. "rows/3/cells/1".
    #[serde(default)]
    pub path: String,
}

impl BlockRejection {
    pub fn new(block_id: Option<&str>, kind: Option<BlockKind>, issue: BlockIssue, path: impl Into<String>) -> Self {
        Self { block_id: block_id.map(str::to_owned), kind, issue, path: path.into() }
    }
}

/// Read-only inputs for `answer_presentation::validate_blocks`, assembled by
/// the parser after derivations, observations and missing information.
#[derive(Debug, Clone, Copy)]
pub struct BlockContext<'a> {
    /// Labels with digits must be contiguous substrings of the question.
    pub question: &'a str,
    /// Original saved documents `[{result_ref, document}]`, never projections.
    pub sources: &'a [Value],
    pub derivations: &'a [Derivation],
    /// "o1".. in observation order.
    pub observation_ids: &'a [String],
    /// Limitations are addressed as "l1".. in order.
    pub limitation_count: usize,
    pub missing_ids: &'a [String],
    pub requirements: &'a [Requirement],
    /// Every plan step: completed (with result_ref), pending, failed or skipped.
    pub source_status: &'a [SourceStatus],
    /// A step is still pending: blocks are provisional, conclusions withheld.
    pub provisional: bool,
}

#[derive(Debug, Clone, Default)]
pub struct BlocksOutcome {
    pub blocks: Vec<AnswerBlock>,
    pub rejected: Vec<BlockRejection>,
}

// ---------------------------------------------------------------------------
// Keys added to the analysis object
// ---------------------------------------------------------------------------

/// Version-2 keys merged into the analysis JSON next to the version-1 keys
/// (`status`, `observations`, `limitations`, `follow_up_question`, `presentation`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AnswerExtension {
    pub answer_schema_version: u8,
    #[serde(default)]
    pub blocks: Vec<AnswerBlock>,
    #[serde(default)]
    pub derivations: Vec<Derivation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completeness: Option<Completeness>,
    #[serde(default)]
    pub missing_information: Vec<MissingInformation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clarification: Option<Clarification>,
    #[serde(default)]
    pub provisional: bool,
    #[serde(default)]
    pub source_index: Vec<SourceIndexEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dropped_blocks: Vec<BlockRejection>,
}

impl AnswerExtension {
    pub fn new() -> Self {
        Self { answer_schema_version: ANSWER_SCHEMA_VERSION, ..Self::default() }
    }
    /// Insert or replace the version-2 keys on an analysis object.
    pub fn attach(&self, analysis: &mut Value) -> serde_json::Result<()> {
        if let Value::Object(fields) = serde_json::to_value(self)? {
            if let Some(target) = analysis.as_object_mut() {
                target.extend(fields);
            }
        }
        Ok(())
    }
    /// The version-2 keys of a stored analysis; None for version 1.
    pub fn read(analysis: &Value) -> Option<Self> {
        if schema_version(analysis) < u64::from(ANSWER_SCHEMA_VERSION) {
            return None;
        }
        serde_json::from_value(analysis.clone()).ok()
    }
}
