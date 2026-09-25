//! Shared rules for version-2 blocks: every value, qualifier, period and
//! collection row is rebuilt from the saved source documents or a server
//! derivation. A model-supplied value is never copied.
use super::period::{parse_period, scalar_text, ParsedPeriod};
use crate::answer_blocks::{
    BlockContext, BlockIssue, Collection, Coverage, CoverageScope, Derivation, DerivationResult, Fact, FactFlag,
    LabeledFact, PeriodQualifier, Qualifier, QualifierBasis, ValueState, MAX_COLLECTION_ROWS, MAX_QUOTE_BYTES,
};
use serde_json::Value;
use std::collections::BTreeSet;

/// Evidence qualifiers (unit, currency) are short codes, never prose.
const MAX_QUALIFIER_BYTES: usize = 32;
const MAX_LABEL_BYTES: usize = 160;
const MAX_PROSE_BYTES: usize = 2000;

/// The first rule a block breaks and where, e.g. `rows/3/cells/1`.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Issue {
    pub(super) code: BlockIssue,
    pub(super) path: String,
}

pub(super) type Checked<T> = Result<T, Issue>;

pub(super) fn fail<T>(code: BlockIssue, path: impl Into<String>) -> Checked<T> {
    Err(Issue { code, path: path.into() })
}

pub(super) fn ensure(condition: bool, code: BlockIssue, path: impl Into<String>) -> Checked<()> {
    if condition { Ok(()) } else { fail(code, path) }
}

/// Model prose: no digits and no links; numbers enter text only through
/// evidence slots. The same rule applies to observations and limitations.
pub fn valid_prose(text: &str) -> bool {
    !text.trim().is_empty() && text.len() <= MAX_PROSE_BYTES && !text.chars().any(char::is_numeric) && !text.contains("://")
}

/// `^[a-z][a-z0-9_-]{0,31}$`: rows, cards, series, bars and other items.
pub(super) fn valid_item_id(id: &str) -> bool {
    let mut chars = id.chars();
    id.len() <= 32
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// `^{prefix}[0-9]{1,max_digits}$`: block, derivation and missing-information ids.
pub(super) fn valid_numbered_id(id: &str, prefix: char, max_digits: usize) -> bool {
    id.strip_prefix(prefix)
        .is_some_and(|number| (1..=max_digits).contains(&number.len()) && number.bytes().all(|b| b.is_ascii_digit()))
}

/// Numeric source value: a JSON number or a plain decimal string.
pub(super) fn numeric(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => {
            let unsigned = text.strip_prefix('-').unwrap_or(text);
            let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, "0"));
            let plain = !whole.is_empty()
                && whole.bytes().all(|b| b.is_ascii_digit())
                && (whole == "0" || !whole.starts_with('0'))
                && !fraction.is_empty()
                && fraction.bytes().all(|b| b.is_ascii_digit());
            plain.then(|| text.parse().ok()).flatten()
        }
        _ => None,
    }
}

/// Null is unknown and zero is a stated value: the two never merge.
pub(super) fn state_of(value: &Value) -> ValueState {
    if value.is_null() {
        ValueState::Missing
    } else if numeric(value) == Some(0.0) {
        ValueState::Zero
    } else {
        ValueState::Reported
    }
}

fn unescape(segment: &str) -> String {
    segment.replace("~1", "/").replace("~0", "~")
}

/// Field key for display: a key/value record's `key`, else the last segment.
fn field_key(document: &Value, pointer: &str) -> String {
    let (parent, last) = pointer.rsplit_once('/').unwrap_or(("", pointer));
    document
        .pointer(parent)
        .and_then(|record| record["key"].as_str())
        .map_or_else(|| unescape(last), str::to_owned)
}

/// A pointer relative to each record of a collection.
pub(super) fn valid_relative(pointer: &str) -> bool {
    pointer.len() > 1 && pointer.starts_with('/')
}

/// A scalar or null cell value; long text is refused without an exact quote.
fn cell_value(value: &Value, path: &str) -> Checked<Value> {
    ensure(!value.is_array() && !value.is_object(), BlockIssue::NonScalar, path)?;
    ensure(value.as_str().is_none_or(|text| text.len() <= MAX_QUOTE_BYTES), BlockIssue::QuoteRequired, path)?;
    Ok(value.clone())
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum QualifierKind {
    Unit,
    Currency,
}

impl QualifierKind {
    fn mismatch(self) -> BlockIssue {
        match self {
            Self::Unit => BlockIssue::UnitMismatch,
            Self::Currency => BlockIssue::CurrencyMismatch,
        }
    }
    fn of(self, result: &DerivationResult) -> Option<&Qualifier> {
        match self {
            Self::Unit => result.unit.as_ref(),
            Self::Currency => result.currency.as_ref(),
        }
    }
    /// A null source field leaves the qualifier unknown.
    fn text(self, value: &Value, path: &str) -> Checked<Option<String>> {
        if value.is_null() {
            return Ok(None);
        }
        ensure(!value.is_array() && !value.is_object(), BlockIssue::NonScalar, path)?;
        let text = scalar_text(value);
        ensure(!text.trim().is_empty() && text.len() <= MAX_QUALIFIER_BYTES, BlockIssue::NonScalar, path)?;
        match self {
            Self::Unit => Ok(Some(text.trim().to_owned())),
            Self::Currency => {
                let trimmed = text.trim();
                let code = trimmed
                    .get(..8)
                    .filter(|prefix| prefix.eq_ignore_ascii_case("iso4217:"))
                    .map_or(trimmed, |_| &trimmed[8..])
                    .to_ascii_uppercase();
                ensure(code.len() == 3 && code.bytes().all(|b| b.is_ascii_uppercase()), self.mismatch(), path)?;
                Ok(Some(code))
            }
        }
    }
}

/// Selected records of a saved array, with server-computed coverage.
#[derive(Debug)]
pub(super) struct Rows<'a> {
    pub(super) records: Vec<(usize, &'a Value)>,
    pub(super) coverage: Coverage,
}

impl<'a> Rows<'a> {
    /// Cell at a pointer relative to one record. An absent field is unknown,
    /// not an error: returned records can be heterogeneous.
    pub(super) fn cell(record: &Value, relative: &str, path: &str) -> Checked<Option<Fact>> {
        let Some(value) = record.pointer(relative) else { return Ok(None) };
        let value = cell_value(value, path)?;
        Ok(Some(Fact { state: state_of(&value), value, ..Fact::default() }))
    }

    /// One unit or currency per record, from a pointer relative to each record.
    pub(super) fn qualifiers(&self, kind: QualifierKind, relative: &str, path: &str) -> Checked<Vec<Option<String>>> {
        ensure(valid_relative(relative), BlockIssue::InvalidPointer, path)?;
        self.records
            .iter()
            .map(|(_, record)| record.pointer(relative).map_or(Ok(None), |value| kind.text(value, path)))
            .collect()
    }

    /// One period per record; None where the record states none.
    pub(super) fn periods(&self, period: &PeriodQualifier, path: &str) -> Checked<Vec<Option<PeriodQualifier>>> {
        let pointer = period.pointer.as_deref().filter(|p| valid_relative(p));
        let pointer = pointer.ok_or_else(|| Issue { code: BlockIssue::InvalidPointer, path: path.to_owned() })?;
        if let Some(end) = &period.end_pointer {
            ensure(valid_relative(end), BlockIssue::InvalidPointer, path)?;
        }
        self.records
            .iter()
            .map(|(_, record)| {
                let Some(value) = record.pointer(pointer).filter(|value| !value.is_null()) else { return Ok(None) };
                ensure(!value.is_array() && !value.is_object(), BlockIssue::NonScalar, path)?;
                let end = period.end_pointer.as_deref().and_then(|end| record.pointer(end));
                Ok(Some(period_qualifier(parse_period(value, end), QualifierBasis::Evidence)))
            })
            .collect()
    }

    /// The one value every record shares, or the mismatch issue.
    pub(super) fn shared(values: &[Option<String>], code: BlockIssue, path: &str) -> Checked<Option<String>> {
        let first = values.first().cloned().flatten();
        ensure(values.iter().all(|value| *value == first), code, path)?;
        Ok(first)
    }
}

fn period_qualifier(parsed: ParsedPeriod, basis: QualifierBasis) -> PeriodQualifier {
    PeriodQualifier {
        value: Some(parsed.text),
        key: parsed.key,
        granularity: Some(parsed.granularity),
        start: parsed.start,
        end: parsed.end,
        basis: Some(basis),
        ..PeriodQualifier::default()
    }
}

/// Resolution state for one block: the context plus the results it cites.
pub(super) struct Scope<'c, 'a> {
    pub(super) ctx: &'c BlockContext<'a>,
    /// Results the block's values come from, for its source status.
    pub(super) used: BTreeSet<String>,
    item_ids: BTreeSet<String>,
}

impl<'c, 'a> Scope<'c, 'a> {
    pub(super) fn new(ctx: &'c BlockContext<'a>) -> Self {
        Self { ctx, used: BTreeSet::new(), item_ids: BTreeSet::new() }
    }

    /// Rows, cards, series and other items: well-formed and unique in the block.
    pub(super) fn item_id(&mut self, id: &str, path: &str) -> Checked<()> {
        ensure(valid_item_id(id), BlockIssue::Schema, format!("{path}/id"))?;
        ensure(self.item_ids.insert(id.to_owned()), BlockIssue::DuplicateId, format!("{path}/id"))
    }

    /// Short single-line text without links. Digits belong in qualifiers,
    /// unless the label repeats part of the question (a name like "Top 10").
    pub(super) fn label(&self, text: &str, path: &str) -> Checked<()> {
        let digits_allowed = || self.ctx.question.to_lowercase().contains(&text.to_lowercase());
        ensure(
            !text.trim().is_empty()
                && text.len() <= MAX_LABEL_BYTES
                && !text.contains("://")
                && !text.contains(['\n', '\r'])
                && (!text.chars().any(char::is_numeric) || digits_allowed()),
            BlockIssue::InvalidLabel,
            path,
        )
    }

    pub(super) fn document(&mut self, result_ref: &str, path: &str) -> Checked<&'a Value> {
        let sources: &'a [Value] = self.ctx.sources;
        let source = sources.iter().find(|source| source["result_ref"].as_str() == Some(result_ref));
        let source = source.ok_or_else(|| Issue { code: BlockIssue::UnknownResultRef, path: path.to_owned() })?;
        self.used.insert(result_ref.to_owned());
        Ok(&source["document"])
    }

    /// A derivation with a server result; its inputs count as cited results.
    pub(super) fn derivation(&mut self, id: &str, path: &str) -> Checked<(&'a Derivation, &'a DerivationResult)> {
        let derivations: &'a [Derivation] = self.ctx.derivations;
        let derivation = derivations.iter().find(|derivation| derivation.id == id);
        let derivation = derivation.ok_or_else(|| Issue { code: BlockIssue::UnknownDerivation, path: path.to_owned() })?;
        let result = derivation.result.as_ref().ok_or_else(|| Issue { code: BlockIssue::DerivationFailed, path: path.to_owned() })?;
        let mut pending = vec![derivation];
        while let Some(next) = pending.pop() {
            if let Some(collection) = &next.collection {
                self.used.insert(collection.result_ref.clone());
            }
            for input in &next.inputs {
                if let Some(result_ref) = &input.result_ref {
                    self.used.insert(result_ref.clone());
                }
                if let Some(earlier) = input.derivation_id.as_deref().and_then(|id| derivations.iter().find(|d| d.id == id)) {
                    pending.push(earlier);
                }
            }
        }
        Ok((derivation, result))
    }

    /// Scalar (or null) at an absolute evidence pointer, and its field key.
    fn scalar(&mut self, result_ref: &str, pointer: &str, quote: Option<&str>, path: &str) -> Checked<(Value, String)> {
        let document = self.document(result_ref, path)?;
        ensure(crate::report_assets::evidence_pointer(pointer), BlockIssue::InvalidPointer, path)?;
        let target = document.pointer(pointer).ok_or_else(|| Issue { code: BlockIssue::MissingField, path: path.to_owned() })?;
        ensure(!target.is_array() && !target.is_object(), BlockIssue::NonScalar, path)?;
        let value = match quote {
            Some(quote) => {
                let verbatim = !quote.trim().is_empty()
                    && quote.len() <= MAX_QUOTE_BYTES
                    && target.as_str().is_some_and(|text| text.contains(quote));
                ensure(verbatim, BlockIssue::QuoteNotVerbatim, path)?;
                Value::String(quote.to_owned())
            }
            None => cell_value(target, path)?,
        };
        Ok((value, field_key(document, pointer)))
    }

    /// Resolve one fact: evidence xor derivation, qualifiers and flags.
    pub(super) fn fact(&mut self, fact: &Fact, path: &str) -> Checked<Fact> {
        let evidence = fact.result_ref.is_some() || fact.pointer.is_some();
        match (fact.derivation_id.as_deref(), evidence) {
            (Some(id), false) => {
                let (derivation, result) = self.derivation(id, path)?;
                Ok(Fact {
                    derivation_id: Some(id.to_owned()),
                    value: result.value.clone(),
                    state: result.state,
                    unit: result.unit.clone(),
                    currency: result.currency.clone(),
                    period: result.period.clone(),
                    rounded_to: derivation.decimals,
                    ..Fact::default()
                })
            }
            (None, true) => {
                let (Some(result_ref), Some(pointer)) = (fact.result_ref.as_deref(), fact.pointer.as_deref()) else {
                    return fail(BlockIssue::UnknownReference, path);
                };
                let (value, field) = self.scalar(result_ref, pointer, fact.quote.as_deref(), path)?;
                let unit = self.qualifier(fact.unit.as_ref(), QualifierKind::Unit, Some(result_ref), &format!("{path}/unit"))?;
                let currency = self.qualifier(fact.currency.as_ref(), QualifierKind::Currency, Some(result_ref), &format!("{path}/currency"))?;
                let period = self.period(fact.period.as_ref(), Some(result_ref), &format!("{path}/period"))?;
                let flags = fact
                    .flags
                    .iter()
                    .enumerate()
                    .map(|(index, flag)| self.flag(flag, &format!("{path}/flags/{index}")))
                    .collect::<Checked<Vec<_>>>()?;
                Ok(Fact {
                    result_ref: Some(result_ref.to_owned()),
                    pointer: Some(pointer.to_owned()),
                    quote: fact.quote.clone(),
                    unit,
                    currency,
                    period,
                    flags,
                    state: state_of(&value),
                    value,
                    field: Some(field),
                    ..Fact::default()
                })
            }
            _ => fail(BlockIssue::UnknownReference, path),
        }
    }

    pub(super) fn optional_fact(&mut self, fact: Option<&Fact>, path: &str) -> Checked<Option<Fact>> {
        fact.map(|fact| self.fact(fact, path)).transpose()
    }

    pub(super) fn labeled(&mut self, item: &LabeledFact, path: &str) -> Checked<LabeledFact> {
        self.label(&item.label, &format!("{path}/label"))?;
        Ok(LabeledFact { label: item.label.clone(), fact: self.optional_fact(item.fact.as_ref(), &format!("{path}/fact"))? })
    }

    /// A flag cites a source field that states it; it is never inferred.
    fn flag(&mut self, flag: &FactFlag, path: &str) -> Checked<FactFlag> {
        let document = self.document(&flag.result_ref, path)?;
        ensure(crate::report_assets::evidence_pointer(&flag.pointer), BlockIssue::InvalidPointer, path)?;
        let value = document.pointer(&flag.pointer).cloned().unwrap_or(Value::Null);
        let stated = value == Value::Bool(true) || value.as_str().is_some_and(|text| !text.trim().is_empty());
        ensure(stated, BlockIssue::FlagWithoutSource, path)?;
        Ok(FactFlag { kind: flag.kind, result_ref: flag.result_ref.clone(), pointer: flag.pointer.clone(), value })
    }

    /// Unit or currency from evidence or a derivation; never a model value.
    /// `owner` is the result a qualifier without its own `result_ref` refers to.
    pub(super) fn qualifier(&mut self, qualifier: Option<&Qualifier>, kind: QualifierKind, owner: Option<&str>, path: &str) -> Checked<Option<Qualifier>> {
        let Some(qualifier) = qualifier else { return Ok(None) };
        if let Some(id) = qualifier.derivation_id.as_deref() {
            let (_, result) = self.derivation(id, path)?;
            return Ok(kind.of(result).map(|inherited| Qualifier {
                derivation_id: Some(id.to_owned()),
                value: inherited.value.clone(),
                basis: Some(QualifierBasis::Derivation),
                ..Qualifier::default()
            }));
        }
        let result_ref = qualifier.result_ref.as_deref().or(owner);
        let (Some(result_ref), Some(pointer)) = (result_ref, qualifier.pointer.as_deref()) else {
            return fail(BlockIssue::UnknownReference, path);
        };
        let (value, _) = self.scalar(result_ref, pointer, None, path)?;
        Ok(kind.text(&value, path)?.map(|text| Qualifier {
            result_ref: Some(result_ref.to_owned()),
            pointer: Some(pointer.to_owned()),
            value: Some(text),
            basis: Some(QualifierBasis::Evidence),
            ..Qualifier::default()
        }))
    }

    /// Reporting period from evidence (optionally with an end date) or a derivation.
    pub(super) fn period(&mut self, period: Option<&PeriodQualifier>, owner: Option<&str>, path: &str) -> Checked<Option<PeriodQualifier>> {
        let Some(period) = period else { return Ok(None) };
        if let Some(id) = period.derivation_id.as_deref() {
            let (_, result) = self.derivation(id, path)?;
            return Ok(result.period.clone().map(|inherited| PeriodQualifier {
                derivation_id: Some(id.to_owned()),
                basis: Some(QualifierBasis::Derivation),
                ..inherited
            }));
        }
        let result_ref = period.result_ref.as_deref().or(owner);
        let (Some(result_ref), Some(pointer)) = (result_ref, period.pointer.as_deref()) else {
            return fail(BlockIssue::UnknownReference, path);
        };
        let (value, _) = self.scalar(result_ref, pointer, None, path)?;
        if value.is_null() {
            return Ok(None);
        }
        let end = match period.end_pointer.as_deref() {
            Some(end) => Some(self.scalar(result_ref, end, None, path)?.0),
            None => None,
        };
        Ok(Some(PeriodQualifier {
            result_ref: Some(result_ref.to_owned()),
            pointer: Some(pointer.to_owned()),
            end_pointer: period.end_pointer.clone(),
            ..period_qualifier(parse_period(&value, end.as_ref()), QualifierBasis::Evidence)
        }))
    }

    /// Explicitly selected records of a saved array with their coverage. A
    /// missing selection never means "the first rows" or "everything".
    pub(super) fn collection(&mut self, collection: &Collection, path: &str) -> Checked<Rows<'a>> {
        let document = self.document(&collection.result_ref, &format!("{path}/result_ref"))?;
        let rows_path = format!("{path}/rows_pointer");
        ensure(collection.rows_pointer.starts_with("/data/"), BlockIssue::InvalidPointer, &rows_path)?;
        let records = document
            .pointer(&collection.rows_pointer)
            .and_then(Value::as_array)
            .ok_or(Issue { code: BlockIssue::MissingField, path: rows_path })?;
        let indices_path = format!("{path}/row_indices");
        ensure(collection.row_indices.len() <= MAX_COLLECTION_ROWS, BlockIssue::TooManyItems, &indices_path)?;
        let mut seen = BTreeSet::new();
        let mut selected = Vec::with_capacity(collection.row_indices.len());
        for (position, &index) in collection.row_indices.iter().enumerate() {
            let record = records.get(index).ok_or_else(|| Issue { code: BlockIssue::RowOutOfRange, path: format!("{indices_path}/{position}") })?;
            ensure(seen.insert(index), BlockIssue::DuplicateRow, format!("{indices_path}/{position}"))?;
            selected.push((index, record));
        }
        let fetched = records.len();
        let total_available = match collection.total_pointer.as_deref() {
            Some(pointer) => {
                let total_path = format!("{path}/total_pointer");
                ensure(crate::report_assets::evidence_pointer(pointer), BlockIssue::InvalidPointer, &total_path)?;
                let total = document.pointer(pointer).and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()));
                let total = total.filter(|total| usize::try_from(*total).is_ok_and(|total| total >= fetched));
                Some(total.ok_or(Issue { code: BlockIssue::InvalidPointer, path: total_path })?)
            }
            None => None,
        };
        let next = collection.next_pointer.as_deref().map(|pointer| document.pointer(pointer));
        let paginated = total_available.is_some_and(|total| total > fetched as u64) || next.is_some_and(|next| next.is_some_and(|next| !next.is_null()));
        let states_end = total_available == Some(fetched as u64) || next.is_some_and(|next| next.is_some_and(Value::is_null));
        let scope = if selected.len() < fetched {
            CoverageScope::Selection
        } else if paginated {
            CoverageScope::FetchedPage
        } else if states_end && document["coverage"] != "partial" {
            CoverageScope::Complete
        } else {
            CoverageScope::Unknown
        };
        let coverage = Coverage { selected: selected.len(), fetched, total_available, paginated, scope };
        Ok(Rows { records: selected, coverage })
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::answer_blocks::{Granularity, SourceState, SourceStatus};
    use serde_json::json;

    pub(crate) fn sources() -> Vec<Value> {
        vec![
            json!({"result_ref":"r1","document":{"source":{"name":"Registry","retrieved_at":"2026-09-24T19:02:11Z"},"subject":{"label":"Mollie B.V.","entity_ref":"e1"},"coverage":"source_response","data":{
                "name":"Mollie B.V.","kvk":"30204462","page":{"total":3,"next":null},"text":"Price is EUR 12 monthly. ".repeat(60),
                "years":[{"year":2023,"revenue":1200000,"currency":"EUR","estimated":false},{"year":2024,"revenue":1500000,"currency":"iso4217:eur","estimated":"geschat"},{"year":"2025","revenue":null,"currency":"EUR"}],
                "zero":0,"flag":true,"nested":{"a":1}}}}),
            json!({"result_ref":"r2","document":{"source":{"name":"Registry","retrieved_at":"2026-09-24T19:02:14Z"},"subject":{"label":"Adyen N.V.","entity_ref":"e2"},"coverage":"source_response","data":{
                "name":"Adyen N.V.","years":[{"year":2024,"revenue":1900000,"currency":"EUR","unit":"EUR"}]}}}),
        ]
    }

    pub(crate) fn context<'a>(sources: &'a [Value], derivations: &'a [Derivation], status: &'a [SourceStatus]) -> BlockContext<'a> {
        BlockContext {
            question: "Vergelijk de omzet van Mollie en Adyen in de Top 10",
            sources,
            derivations,
            observation_ids: &[],
            limitation_count: 0,
            missing_ids: &[],
            requirements: &[],
            source_status: status,
            provisional: false,
        }
    }

    fn fact(value: Value) -> Fact {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn values_come_from_saved_fields_and_unknown_stays_unknown() {
        let sources = sources();
        let ctx = context(&sources, &[], &[]);
        let mut scope = Scope::new(&ctx);
        let resolved = scope.fact(&fact(json!({"result_ref":"r1","pointer":"/data/years/1/revenue","value":42,"currency":{"pointer":"/data/years/1/currency"},"period":{"pointer":"/data/years/1/year"}})), "f").unwrap();
        assert_eq!(resolved.value, json!(1500000), "the model's value is discarded");
        assert_eq!(resolved.field.as_deref(), Some("revenue"));
        assert_eq!(resolved.currency.unwrap().value.as_deref(), Some("EUR"), "normalised from iso4217:eur");
        let period = resolved.period.unwrap();
        assert_eq!((period.key.as_deref(), period.granularity), (Some("2024"), Some(Granularity::Year)));
        let missing = scope.fact(&fact(json!({"result_ref":"r1","pointer":"/data/years/2/revenue"})), "f").unwrap();
        assert_eq!((missing.value, missing.state), (Value::Null, ValueState::Missing));
        assert_eq!(scope.fact(&fact(json!({"result_ref":"r1","pointer":"/data/zero"})), "f").unwrap().state, ValueState::Zero);
        assert!(scope.used.contains("r1") && !scope.used.contains("r2"));
    }

    #[test]
    fn invalid_references_are_named_by_issue_code() {
        let sources = sources();
        let ctx = context(&sources, &[], &[]);
        let mut scope = Scope::new(&ctx);
        let code = |scope: &mut Scope, value: Value| scope.fact(&fact(value), "f").unwrap_err().code;
        assert_eq!(code(&mut scope, json!({"result_ref":"r9","pointer":"/data/name"})), BlockIssue::UnknownResultRef);
        assert_eq!(code(&mut scope, json!({"result_ref":"r1","pointer":"/source/name"})), BlockIssue::InvalidPointer);
        assert_eq!(code(&mut scope, json!({"result_ref":"r1","pointer":"/data/absent"})), BlockIssue::MissingField);
        assert_eq!(code(&mut scope, json!({"result_ref":"r1","pointer":"/data/nested"})), BlockIssue::NonScalar);
        assert_eq!(code(&mut scope, json!({"result_ref":"r1","pointer":"/data/text"})), BlockIssue::QuoteRequired);
        assert_eq!(code(&mut scope, json!({"result_ref":"r1","pointer":"/data/text","quote":"EUR 13 monthly"})), BlockIssue::QuoteNotVerbatim);
        assert_eq!(code(&mut scope, json!({"derivation_id":"d1"})), BlockIssue::UnknownDerivation);
        assert_eq!(code(&mut scope, json!({"derivation_id":"d1","result_ref":"r1","pointer":"/data/name"})), BlockIssue::UnknownReference);
        assert_eq!(code(&mut scope, json!({})), BlockIssue::UnknownReference);
        assert_eq!(code(&mut scope, json!({"result_ref":"r1","pointer":"/data/name","currency":{"value":"EUR"}})), BlockIssue::UnknownReference, "no currency without evidence");
        assert_eq!(code(&mut scope, json!({"result_ref":"r1","pointer":"/data/name","currency":{"pointer":"/data/name"}})), BlockIssue::CurrencyMismatch);
        assert_eq!(code(&mut scope, json!({"result_ref":"r1","pointer":"/data/name","flags":[{"kind":"estimated","result_ref":"r1","pointer":"/data/years/0/estimated"}]})), BlockIssue::FlagWithoutSource);
        let quoted = scope.fact(&fact(json!({"result_ref":"r1","pointer":"/data/text","quote":"EUR 12 monthly"})), "f").unwrap();
        assert_eq!(quoted.value, json!("EUR 12 monthly"));
        let flagged = scope.fact(&fact(json!({"result_ref":"r1","pointer":"/data/years/1/revenue","flags":[{"kind":"estimated","result_ref":"r1","pointer":"/data/years/1/estimated"}]})), "f").unwrap();
        assert_eq!(flagged.flags[0].value, json!("geschat"));
    }

    #[test]
    fn collections_need_an_explicit_selection_and_report_their_coverage() {
        let sources = sources();
        let ctx = context(&sources, &[], &[]);
        let mut scope = Scope::new(&ctx);
        let collection = |rows: Value, extra: Value| -> Collection {
            let mut value = json!({"result_ref":"r1","rows_pointer":"/data/years","row_indices":rows});
            value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            serde_json::from_value(value).unwrap()
        };
        let all = scope.collection(&collection(json!([2, 0, 1]), json!({"total_pointer":"/data/page/total"})), "c").unwrap();
        assert_eq!(all.records.iter().map(|(index, _)| *index).collect::<Vec<_>>(), [2, 0, 1]);
        assert_eq!(all.coverage, Coverage { selected: 3, fetched: 3, total_available: Some(3), paginated: false, scope: CoverageScope::Complete });
        let some = scope.collection(&collection(json!([1]), json!({"next_pointer":"/data/page/next"})), "c").unwrap();
        assert_eq!((some.coverage.selected, some.coverage.scope), (1, CoverageScope::Selection));
        assert_eq!(scope.collection(&collection(json!([0, 1, 2]), json!({})), "c").unwrap().coverage.scope, CoverageScope::Unknown);
        let code = |scope: &mut Scope, rows: Value, extra: Value| scope.collection(&collection(rows, extra), "c").unwrap_err().code;
        assert_eq!(code(&mut scope, json!([3]), json!({})), BlockIssue::RowOutOfRange);
        assert_eq!(code(&mut scope, json!([1, 1]), json!({})), BlockIssue::DuplicateRow);
        assert_eq!(code(&mut scope, json!((0..201).collect::<Vec<_>>()), json!({})), BlockIssue::TooManyItems);
        assert_eq!(code(&mut scope, json!([0]), json!({"total_pointer":"/data/zero"})), BlockIssue::InvalidPointer, "a total below the fetched rows is not a total");
        let absent = Rows::cell(&json!({"name":"x"}), "/revenue", "c").unwrap();
        assert!(absent.is_none(), "an absent field is an unknown cell, not an error");
    }

    #[test]
    fn labels_carry_no_numbers_links_or_lines_unless_quoted_from_the_question() {
        let sources = sources();
        let ctx = context(&sources, &[], &[]);
        let scope = Scope::new(&ctx);
        assert!(scope.label("Omzet", "l").is_ok());
        assert!(scope.label("Top 10", "l").is_ok());
        assert_eq!(scope.label("Omzet 2024", "l").unwrap_err().code, BlockIssue::InvalidLabel);
        assert!(scope.label("https://example.org", "l").is_err());
        assert!(scope.label("Two\nlines", "l").is_err());
        assert!(scope.label(" ", "l").is_err());
        assert!(valid_prose("Adyen rapporteert een hogere omzet.") && !valid_prose("Omzet van 12 miljoen."));
        assert!(valid_item_id("row-17") && !valid_item_id("Row") && !valid_item_id("1a"));
        assert!(valid_numbered_id("b12", 'b', 3) && !valid_numbered_id("b1234", 'b', 3) && !valid_numbered_id("b", 'b', 3));
        assert_eq!(numeric(&json!("-12.50")), Some(-12.5));
        assert_eq!(numeric(&json!("012")), None);
        assert_eq!(numeric(&json!("1e3")), None);
    }
}
