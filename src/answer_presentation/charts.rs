//! Line chart (#48) and bar chart / ranking (#49). Every plotted value is a
//! fact; gaps stay gaps and the bar scale covers every bar, never a page.
use super::resolve::{ensure, fail, numeric, valid_relative, Checked, Issue, QualifierKind, Rows, Scope};
use super::tables::relative;
use crate::answer_blocks::{
    Bar, BarChartBlock, BlockIssue, Collection, Coverage, CoverageScope, Fact, GapReason, Granularity, LineChartBlock,
    PeriodQualifier, Qualifier, Ranking, RankingScope, Scale, Series, SeriesPoint, SortDirection, ValueState, MAX_BARS,
    MAX_SERIES, MAX_SERIES_POINTS,
};
use std::collections::BTreeSet;

fn missing() -> Fact {
    Fact { value: serde_json::Value::Null, state: ValueState::Missing, ..Fact::default() }
}

/// Chart values are numbers; null is an explicit unknown, never zero.
fn plotted(fact: &Fact, path: &str) -> Checked<()> {
    ensure(fact.value.is_null() || numeric(&fact.value).is_some(), BlockIssue::IncompatibleSeries, path)
}

/// The one unit or currency a set of values shares. A declared qualifier
/// covers values without their own; any other disagreement is a mismatch.
fn shared_qualifier<'q>(declared: Option<Qualifier>, values: impl IntoIterator<Item = Option<&'q Qualifier>>, code: BlockIssue, path: &str) -> Checked<Option<Qualifier>> {
    let text = |q: Option<&Qualifier>| q.and_then(|q| q.value.clone());
    let mut values = values.into_iter();
    match declared {
        Some(declared) => {
            let expected = declared.value.clone();
            ensure(values.all(|value| value.is_none() || text(value) == expected), code, path)?;
            Ok(Some(declared))
        }
        None => {
            let values: Vec<Option<&Qualifier>> = values.collect();
            // A value without its own qualifier is unknown, not a mismatch.
            let expected = values.iter().find_map(|value| text(*value));
            ensure(values.iter().all(|value| text(*value).is_none() || text(*value) == expected), code, path)?;
            let reference = values.into_iter().find(|value| text(*value).is_some()).flatten();
            Ok(reference.map(|q| Qualifier { value: q.value.clone(), basis: q.basis, ..Qualifier::default() }))
        }
    }
}

/// A qualifier over a collection: relative to each record (all must agree)
/// or an absolute reference.
fn collection_qualifier(scope: &mut Scope, rows: &Rows, qualifier: Option<&Qualifier>, kind: QualifierKind, collection: &Collection, code: BlockIssue, path: &str) -> Checked<Option<Qualifier>> {
    match qualifier {
        Some(q) if relative(q) => {
            let values = rows.qualifiers(kind, q.pointer.as_deref().unwrap_or_default(), path)?;
            let shared = Rows::shared(&values, code, path)?;
            Ok(shared.map(|value| Qualifier { pointer: q.pointer.clone(), value: Some(value), basis: Some(crate::answer_blocks::QualifierBasis::Evidence), ..Qualifier::default() }))
        }
        other => scope.qualifier(other, kind, Some(&collection.result_ref), path),
    }
}

fn relative_pointer(pointer: Option<&str>, path: String) -> Checked<&str> {
    pointer.filter(|p| valid_relative(p)).ok_or(Issue { code: BlockIssue::InvalidPointer, path })
}

/// One series with its own unit and currency values (before the chart check).
struct BuiltSeries {
    series: Series,
    unit: Option<Qualifier>,
    currency: Option<Qualifier>,
}

fn explicit_series(series: &Series, scope: &mut Scope, path: &str) -> Checked<BuiltSeries> {
    ensure(series.points.len() <= MAX_SERIES_POINTS, BlockIssue::TooManyItems, format!("{path}/points"))?;
    let mut points = Vec::with_capacity(series.points.len());
    for (index, point) in series.points.iter().enumerate() {
        let point_path = format!("{path}/points/{index}");
        // The x position is a stated period, never a derived or invented one.
        ensure(point.period.derivation_id.is_none(), BlockIssue::UnparsedPeriod, format!("{point_path}/period"))?;
        let period = scope.period(Some(&point.period), None, &format!("{point_path}/period"))?;
        let period = period.ok_or_else(|| Issue { code: BlockIssue::UnparsedPeriod, path: format!("{point_path}/period") })?;
        // A period that failed to parse is reported before any qualifier check.
        ensure(period.granularity.is_some_and(|g| g != Granularity::Unparsed), BlockIssue::UnparsedPeriod, format!("{point_path}/period"))?;
        let fact = point.fact.as_ref().ok_or_else(|| Issue { code: BlockIssue::Schema, path: format!("{point_path}/fact") })?;
        let fact = scope.fact(fact, &format!("{point_path}/fact"))?;
        plotted(&fact, &format!("{point_path}/fact"))?;
        points.push(SeriesPoint { row_index: None, period, fact: Some(fact), gap: None });
    }
    let declared_unit = scope.qualifier(series.unit.as_ref(), QualifierKind::Unit, None, &format!("{path}/unit"))?;
    let declared_currency = scope.qualifier(series.currency.as_ref(), QualifierKind::Currency, None, &format!("{path}/currency"))?;
    let facts = || points.iter().filter_map(|point| point.fact.as_ref());
    let unit = shared_qualifier(declared_unit.clone(), facts().map(|f| f.unit.as_ref()), BlockIssue::IncompatibleSeries, path)?;
    let currency = shared_qualifier(declared_currency.clone(), facts().map(|f| f.currency.as_ref()), BlockIssue::IncompatibleSeries, path)?;
    let series = Series { collection: None, period_pointer: None, value_pointer: None, unit: declared_unit, currency: declared_currency, points, ..series.clone() };
    Ok(BuiltSeries { series, unit, currency })
}

fn collection_series(series: &Series, collection: &Collection, scope: &mut Scope, path: &str) -> Checked<BuiltSeries> {
    let rows = scope.collection(collection, &format!("{path}/collection"))?;
    let period_pointer = relative_pointer(series.period_pointer.as_deref(), format!("{path}/period_pointer"))?;
    let value_pointer = relative_pointer(series.value_pointer.as_deref(), format!("{path}/value_pointer"))?;
    let periods = rows.periods(&PeriodQualifier { pointer: Some(period_pointer.to_owned()), ..PeriodQualifier::default() }, &format!("{path}/period_pointer"))?;
    let mut points = Vec::with_capacity(rows.records.len());
    for (position, ((index, record), period)) in rows.records.iter().zip(periods).enumerate() {
        let point_path = format!("{path}/points/{position}");
        let period = period.ok_or_else(|| Issue { code: BlockIssue::UnparsedPeriod, path: format!("{point_path}/period") })?;
        ensure(period.granularity.is_some_and(|g| g != Granularity::Unparsed), BlockIssue::UnparsedPeriod, format!("{point_path}/period"))?;
        let fact = Rows::cell(record, value_pointer, &format!("{point_path}/fact"))?.unwrap_or_else(missing);
        plotted(&fact, &format!("{point_path}/fact"))?;
        points.push(SeriesPoint { row_index: Some(*index), period, fact: Some(fact), gap: None });
    }
    let unit = collection_qualifier(scope, &rows, series.unit.as_ref(), QualifierKind::Unit, collection, BlockIssue::IncompatibleSeries, &format!("{path}/unit"))?;
    let currency = collection_qualifier(scope, &rows, series.currency.as_ref(), QualifierKind::Currency, collection, BlockIssue::IncompatibleSeries, &format!("{path}/currency"))?;
    let series = Series { unit: unit.clone(), currency: currency.clone(), points, ..series.clone() };
    Ok(BuiltSeries { series, unit, currency })
}

/// Chronological series on one granularity; every key a series lacks
/// becomes an explicit gap and nothing is interpolated.
pub(super) fn line_chart(block: LineChartBlock, scope: &mut Scope) -> Checked<LineChartBlock> {
    scope.label(&block.value_label, "value_label")?;
    ensure(!block.series.is_empty(), BlockIssue::Schema, "series")?;
    ensure(block.series.len() <= MAX_SERIES, BlockIssue::TooManyItems, "series")?;
    let mut built = Vec::with_capacity(block.series.len());
    for (index, series) in block.series.iter().enumerate() {
        let path = format!("series/{index}");
        scope.item_id(&series.id, &path)?;
        scope.label(&series.label, &format!("{path}/label"))?;
        built.push(match &series.collection {
            Some(collection) => collection_series(series, collection, scope, &path)?,
            None => explicit_series(series, scope, &path)?,
        });
    }
    let mut granularity: Option<Granularity> = None;
    let mut domain = BTreeSet::new();
    for (s, entry) in built.iter().enumerate() {
        let mut keys = BTreeSet::new();
        for (p, point) in entry.series.points.iter().enumerate() {
            let path = format!("series/{s}/points/{p}/period");
            let parsed = point.period.granularity.filter(|g| *g != Granularity::Unparsed);
            let (Some(key), Some(parsed)) = (point.period.key.clone(), parsed) else { return fail(BlockIssue::UnparsedPeriod, path) };
            ensure(*granularity.get_or_insert(parsed) == parsed, BlockIssue::PeriodMismatch, &path)?;
            ensure(keys.insert(key.clone()), BlockIssue::DuplicatePeriod, &path)?;
            domain.insert(key);
        }
        let valued = entry.series.points.iter().filter(|point| point.fact.as_ref().is_some_and(|f| !f.value.is_null())).count();
        ensure(valued >= 2, BlockIssue::Schema, format!("series/{s}/points"))?;
    }
    let unit = shared_qualifier(None, built.iter().map(|entry| entry.unit.as_ref()), BlockIssue::IncompatibleSeries, "series")?;
    let currency = shared_qualifier(None, built.iter().map(|entry| entry.currency.as_ref()), BlockIssue::IncompatibleSeries, "series")?;
    let series = built
        .into_iter()
        .map(|entry| {
            let mut series = entry.series;
            let present: BTreeSet<String> = series.points.iter().filter_map(|point| point.period.key.clone()).collect();
            for point in &mut series.points {
                if point.fact.as_ref().is_some_and(|fact| fact.value.is_null()) {
                    point.gap = Some(GapReason::NullInSource);
                }
            }
            series.points.extend(domain.iter().filter(|key| !present.contains(*key)).map(|key| SeriesPoint {
                period: PeriodQualifier { key: Some(key.clone()), granularity, ..PeriodQualifier::default() },
                gap: Some(GapReason::NotInSource),
                ..SeriesPoint::default()
            }));
            series.points.sort_by(|a, b| a.period.key.cmp(&b.period.key));
            series
        })
        .collect();
    Ok(LineChartBlock { series, granularity, unit, currency, ..block })
}

/// Ranking scope may never claim more than the coverage shows.
fn ranking_scope_fits(ranking: &Ranking, coverage: Option<&Coverage>) -> bool {
    match ranking.scope {
        RankingScope::Selection => true,
        RankingScope::Fetched => coverage.is_some_and(|c| c.selected == c.fetched),
        RankingScope::TotalAvailable => coverage.is_some_and(|c| c.scope == CoverageScope::Complete),
    }
}

/// Order valued bars by the ranking direction, unknowns last; ties share a rank.
fn rank(bars: &mut Vec<Bar>, direction: SortDirection) {
    let value = |bar: &Bar| bar.fact.as_ref().and_then(|fact| numeric(&fact.value));
    let (mut valued, unknown): (Vec<Bar>, Vec<Bar>) = std::mem::take(bars).into_iter().partition(|bar| value(bar).is_some());
    valued.sort_by(|a, b| {
        let order = value(a).partial_cmp(&value(b)).unwrap_or(std::cmp::Ordering::Equal);
        if direction == SortDirection::Desc { order.reverse() } else { order }
    });
    let mut previous: Option<(f64, u32)> = None;
    for (position, bar) in valued.iter_mut().enumerate() {
        let current = value(bar).unwrap_or_default();
        let rank = match previous {
            Some((before, rank)) if before == current => rank,
            _ => u32::try_from(position + 1).unwrap_or(u32::MAX),
        };
        bar.rank = Some(rank);
        previous = Some((current, rank));
    }
    bars.extend(valued);
    bars.extend(unknown);
}

pub(super) fn bar_chart(block: BarChartBlock, scope: &mut Scope) -> Checked<(BarChartBlock, Option<Coverage>)> {
    scope.label(&block.value_label, "value_label")?;
    let (mut bars, coverage, unit, currency) = match &block.collection {
        Some(collection) => {
            let rows = scope.collection(collection, "collection")?;
            let label_pointer = relative_pointer(block.label_pointer.as_deref(), "label_pointer".into())?;
            let value_pointer = relative_pointer(block.value_pointer.as_deref(), "value_pointer".into())?;
            let mut bars = Vec::with_capacity(rows.records.len());
            for (position, (index, record)) in rows.records.iter().enumerate() {
                let path = format!("bars/{position}");
                bars.push(Bar {
                    id: format!("row-{index}"),
                    label: None,
                    label_fact: Rows::cell(record, label_pointer, &format!("{path}/label_fact"))?,
                    fact: Some(Rows::cell(record, value_pointer, &format!("{path}/fact"))?.unwrap_or_else(missing)),
                    row_index: Some(*index),
                    rank: None,
                });
            }
            let unit = collection_qualifier(scope, &rows, block.unit.as_ref(), QualifierKind::Unit, collection, BlockIssue::UnitMismatch, "unit")?;
            let currency = collection_qualifier(scope, &rows, block.currency.as_ref(), QualifierKind::Currency, collection, BlockIssue::CurrencyMismatch, "currency")?;
            (bars, Some(rows.coverage), unit, currency)
        }
        None => {
            ensure(!block.bars.is_empty(), BlockIssue::Schema, "bars")?;
            ensure(block.bars.len() <= MAX_BARS, BlockIssue::TooManyItems, "bars")?;
            let mut bars = Vec::with_capacity(block.bars.len());
            for (index, bar) in block.bars.iter().enumerate() {
                let path = format!("bars/{index}");
                scope.item_id(&bar.id, &path)?;
                ensure(bar.label.is_some() || bar.label_fact.is_some(), BlockIssue::Schema, format!("{path}/label"))?;
                if let Some(label) = &bar.label {
                    scope.label(label, &format!("{path}/label"))?;
                }
                let label_fact = scope.optional_fact(bar.label_fact.as_ref(), &format!("{path}/label_fact"))?;
                let fact = scope.optional_fact(bar.fact.as_ref(), &format!("{path}/fact"))?;
                bars.push(Bar { label_fact, fact, row_index: None, rank: None, ..bar.clone() });
            }
            let declared_unit = scope.qualifier(block.unit.as_ref(), QualifierKind::Unit, None, "unit")?;
            let declared_currency = scope.qualifier(block.currency.as_ref(), QualifierKind::Currency, None, "currency")?;
            let facts = || bars.iter().filter_map(|bar| bar.fact.as_ref()).filter(|fact| !fact.value.is_null());
            let unit = shared_qualifier(declared_unit, facts().map(|f| f.unit.as_ref()), BlockIssue::UnitMismatch, "bars")?;
            let currency = shared_qualifier(declared_currency, facts().map(|f| f.currency.as_ref()), BlockIssue::CurrencyMismatch, "bars")?;
            (bars, None, unit, currency)
        }
    };
    for (index, bar) in bars.iter().enumerate() {
        if let Some(fact) = &bar.fact {
            plotted(fact, &format!("bars/{index}/fact"))?;
        }
    }
    let values: Vec<f64> = bars.iter().filter_map(|bar| bar.fact.as_ref().and_then(|fact| numeric(&fact.value))).collect();
    let scale = Scale { min: values.iter().copied().fold(0.0, f64::min), max: values.iter().copied().fold(0.0, f64::max) };
    if let Some(ranking) = &block.ranking {
        if let Some(id) = &ranking.derivation_id {
            scope.derivation(id, "ranking/derivation_id")?;
        }
        if !ranking_scope_fits(ranking, coverage.as_ref()) {
            return fail(BlockIssue::RankingScopeExceedsCoverage, "ranking/scope");
        }
        rank(&mut bars, ranking.direction);
    }
    Ok((BarChartBlock { bars, unit, currency, scale: Some(scale), ..block }, coverage))
}
