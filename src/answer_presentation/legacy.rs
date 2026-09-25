//! Deterministic mapping from a validated version-1 presentation to blocks,
//! and the version-1 kind kept for older App bundles.
use super::period::scalar_text;
use super::resolve::{numeric, state_of};
use super::tables::row_basis;
use crate::answer_blocks::{
    AnswerBlock, Bar, BarChartBlock, BasisIssue, BlockBody, BlockKind, BlockProducer, ColumnRole, ComparedEntity, ComparisonBlock,
    ComparisonRow, Coverage, CoverageScope, DataTableBlock, Fact, RowBasis, Scale, SummaryBlock, TableColumn, TableRow,
};
use serde_json::Value;

/// A validated version-1 cell: `{value, result_ref?, pointer?, source?}` or null.
fn cell_fact(cell: &Value) -> Option<Fact> {
    if cell.is_null() {
        return None;
    }
    let text = |key: &str| cell[key].as_str().map(str::to_owned);
    let pointer = text("pointer");
    let field = pointer.as_deref().and_then(|p| p.rsplit('/').next()).map(|last| last.replace("~1", "/").replace("~0", "~"));
    let value = cell["value"].clone();
    Some(Fact { result_ref: text("result_ref"), pointer, source: text("source"), field, state: state_of(&value), value, ..Fact::default() })
}

fn heading(cell: &Value) -> String {
    scalar_text(&cell["value"])
}

/// Column role from the selected values, so sorting and filters fit the data.
fn column_role(rows: &[Vec<Value>], column: usize) -> ColumnRole {
    let values: Vec<&Value> = rows.iter().filter_map(|row| row.get(column)).map(|cell| &cell["value"]).filter(|value| !value.is_null()).collect();
    if !values.is_empty() && values.iter().all(|value| numeric(value).is_some()) {
        ColumnRole::Number
    } else if !values.is_empty() && values.iter().all(|value| value.as_str().is_some_and(|text| text.starts_with("https://") || text.starts_with("http://"))) {
        ColumnRole::Link
    } else {
        ColumnRole::Text
    }
}

fn legacy(id: usize, body: BlockBody) -> AnswerBlock {
    AnswerBlock { produced_by: BlockProducer::LegacyPresentation, ..AnswerBlock::new(format!("b{id}"), body) }
}

fn titled(block: AnswerBlock, title: &str) -> AnswerBlock {
    AnswerBlock { title: Some(title.to_owned()), ..block }
}

fn table_block(presentation: &Value, rows: &[Vec<Value>], id: usize) -> AnswerBlock {
    let headers = presentation["columns"].as_array().cloned().unwrap_or_default();
    let columns = headers
        .iter()
        .enumerate()
        .map(|(index, header)| TableColumn { id: format!("c{}", index + 1), label: heading(header), role: column_role(rows, index), ..TableColumn::default() })
        .collect();
    let table = rows.iter().map(|row| TableRow { row_index: None, cells: row.iter().map(cell_fact).collect() }).collect();
    let mut block = titled(legacy(id, BlockBody::DataTable(DataTableBlock { collection: None, columns, rows: table, sort: None })), presentation["title"].as_str().unwrap_or_default());
    block.coverage = presentation["returned_count"].as_u64().and_then(|fetched| usize::try_from(fetched).ok()).map(|fetched| Coverage {
        selected: rows.len(),
        fetched,
        total_available: None,
        paginated: false,
        scope: CoverageScope::Selection,
    });
    block
}

fn comparison_block(presentation: &Value, rows: &[Vec<Value>], id: usize) -> AnswerBlock {
    let headers = presentation["columns"].as_array().cloned().unwrap_or_default();
    let entities = headers
        .iter()
        .enumerate()
        .skip(1)
        .map(|(index, header)| ComparedEntity {
            id: format!("e{index}"),
            label: heading(header),
            name: header.get("result_ref").and_then(|_| cell_fact(header)),
            subject: None,
        })
        .collect();
    let rows = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let cells: Vec<Option<Fact>> = row.iter().skip(1).map(cell_fact).collect();
            // Version-1 cells carry no qualifiers, so comparability is unknown.
            let basis = RowBasis { comparable: false, issues: vec![BasisIssue::QualifiersUnknown], values_differ: row_basis(&cells).values_differ };
            ComparisonRow { id: format!("c{}", index + 1), label: row.first().map(heading).unwrap_or_default(), requirement_id: None, cells, basis: Some(basis) }
        })
        .collect();
    titled(legacy(id, BlockBody::Comparison(ComparisonBlock { entities, rows })), presentation["title"].as_str().unwrap_or_default())
}

fn bar_block(presentation: &Value, rows: &[Vec<Value>], id: usize) -> AnswerBlock {
    let bars: Vec<Bar> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| Bar {
            id: format!("r{}", index + 1),
            label: None,
            label_fact: row.first().and_then(cell_fact),
            fact: row.get(1).and_then(cell_fact),
            row_index: None,
            rank: None,
        })
        .collect();
    let values: Vec<f64> = bars.iter().filter_map(|bar| bar.fact.as_ref().and_then(|fact| numeric(&fact.value))).collect();
    let scale = Scale { min: values.iter().copied().fold(0.0, f64::min), max: values.iter().copied().fold(0.0, f64::max) };
    let value_label = presentation["columns"].get(1).map(heading).unwrap_or_default();
    let chart = BarChartBlock { value_label, bars, scale: Some(scale), ..BarChartBlock::default() };
    titled(legacy(id, BlockBody::BarChart(chart)), presentation["title"].as_str().unwrap_or_default())
}

/// Blocks for an analysis whose model answered in the version-1 shape.
/// `presentation` is the output of `validate`. Observations come first, as
/// the version-1 App showed them; rows are never duplicated.
pub fn blocks_from_presentation(presentation: &Value, observation_ids: &[String], limitation_count: usize) -> Vec<AnswerBlock> {
    let mut blocks = Vec::new();
    if !observation_ids.is_empty() {
        blocks.push(legacy(1, BlockBody::Narrative(crate::answer_blocks::NarrativeBlock { observation_ids: observation_ids.to_vec() })));
    }
    let id = blocks.len() + 1;
    let rows: Vec<Vec<Value>> = presentation["rows"].as_array().map(|rows| rows.iter().map(|row| row.as_array().cloned().unwrap_or_default()).collect()).unwrap_or_default();
    let columns = presentation["columns"].as_array().map_or(0, Vec::len);
    match presentation["kind"].as_str() {
        Some("document") => blocks.push(legacy(id, BlockBody::Summary(SummaryBlock {
            conclusion: None,
            finding_ids: observation_ids.to_vec(),
            open_point_ids: (1..=limitation_count).map(|n| format!("l{n}")).collect(),
            offer_report: true,
        }))),
        Some("table") => blocks.push(table_block(presentation, &rows, id)),
        // A comparison needs two entities; one column of values is a table.
        Some("comparison") if columns >= 3 => blocks.push(comparison_block(presentation, &rows, id)),
        Some("comparison") => blocks.push(table_block(presentation, &rows, id)),
        Some("chart") => blocks.push(bar_block(presentation, &rows, id)),
        _ => {}
    }
    blocks
}

/// The version-1 `presentation.kind` stub for older App bundles: the first
/// data block's kind, else "document" when a summary offers the report (the
/// older App then shows the PDF link), else "text".
pub fn legacy_kind(blocks: &[AnswerBlock]) -> &'static str {
    let data = blocks.iter().find_map(|block| match block.kind() {
        BlockKind::DataTable => Some("table"),
        BlockKind::Comparison => Some("comparison"),
        BlockKind::BarChart | BlockKind::LineChart => Some("chart"),
        _ => None,
    });
    let report = || blocks.iter().any(|block| matches!(&block.body, BlockBody::Summary(summary) if summary.offer_report));
    data.unwrap_or_else(|| if report() { "document" } else { "text" })
}
