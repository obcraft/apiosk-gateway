//! `validate_blocks`: one version-2 block at a time. A block that breaks a
//! rule is rejected with a fixed issue code; findings and the other blocks
//! are kept, so a repair can target just the rejected block (#44).
use super::resolve::{ensure, valid_numbered_id, valid_prose, Checked, Issue, Scope};
use super::{charts, entities, map, tables, text, timeline};
use crate::answer_blocks::{
    AnswerBlock, BlockBody, BlockContext, BlockIssue, BlockKind, BlockProducer, BlockRejection, BlocksOutcome, ConclusionScope,
    Coverage, NarrativeBlock, SourceState, SourceStatus, MAX_BLOCKS, MAX_BLOCKS_BYTES,
};
use serde_json::Value;
use std::collections::BTreeSet;

const MAX_NOTES: usize = 4;
const KINDS: [&str; 11] = ["narrative", "summary", "data_table", "comparison", "kpi", "line_chart", "bar_chart", "map", "timeline", "profile", "relationship"];

fn kind_of(item: &Value) -> Option<BlockKind> {
    serde_json::from_value(item["kind"].clone()).ok()
}

/// Collections are selections; a missing `row_indices` is never "all rows".
fn missing_row_selection(item: &Value) -> Option<String> {
    let lacks = |collection: &Value| collection.is_object() && collection.get("row_indices").is_none();
    if lacks(&item["collection"]) {
        return Some("collection/row_indices".into());
    }
    let series = item["series"].as_array()?;
    series.iter().position(|series| lacks(&series["collection"])).map(|index| format!("series/{index}/collection/row_indices"))
}

/// Parse one item on its own: a schema error rejects only that block.
fn parse(item: &Value) -> Result<AnswerBlock, BlockRejection> {
    let id = item["id"].as_str();
    let kind = kind_of(item);
    let reject = |issue, path: &str| BlockRejection::new(id, kind, issue, path);
    if let Some(path) = missing_row_selection(item) {
        return Err(reject(BlockIssue::MissingRowSelection, &path));
    }
    if item["kind"].as_str().is_some_and(|kind| !KINDS.contains(&kind)) {
        return Err(reject(BlockIssue::UnsupportedKind, "kind"));
    }
    serde_json::from_value(item.clone()).map_err(|_| reject(BlockIssue::Schema, ""))
}

fn header(block: &AnswerBlock, scope: &Scope) -> Checked<()> {
    if let Some(title) = &block.title {
        scope.label(title, "title")?;
    }
    for (index, id) in block.requirement_ids.iter().enumerate() {
        ensure(scope.ctx.requirements.iter().any(|r| r.id == *id), BlockIssue::UnknownReference, format!("requirement_ids/{index}"))?;
    }
    ensure(block.notes.len() <= MAX_NOTES, BlockIssue::TooManyItems, "notes")?;
    for (index, note) in block.notes.iter().enumerate() {
        ensure(valid_prose(note), BlockIssue::InvalidLabel, format!("notes/{index}"))?;
    }
    Ok(())
}

/// Plan steps behind a block: those whose results it cites, plus pending,
/// failed or skipped steps for the same subjects (a missing part, never a
/// negative finding).
fn source_status(ctx: &BlockContext, used: &BTreeSet<String>) -> Vec<SourceStatus> {
    let entities: BTreeSet<&str> = ctx
        .sources
        .iter()
        .filter(|source| source["result_ref"].as_str().is_some_and(|r| used.contains(r)))
        .filter_map(|source| source["document"]["subject"]["entity_ref"].as_str())
        .collect();
    ctx.source_status
        .iter()
        .filter(|status| {
            status.result_ref.as_ref().is_some_and(|r| used.contains(r))
                || (matches!(status.state, SourceState::Pending | SourceState::Failed | SourceState::Skipped)
                    && status.step_key.rsplit_once(':').is_some_and(|(_, entity)| entities.contains(entity)))
        })
        .cloned()
        .collect()
}

fn validate_block(block: AnswerBlock, ctx: &BlockContext, placed: &mut BTreeSet<String>) -> Checked<AnswerBlock> {
    let mut scope = Scope::new(ctx);
    header(&block, &scope)?;
    let (body, coverage): (BlockBody, Option<Coverage>) = match block.body.clone() {
        BlockBody::Narrative(body) => (BlockBody::Narrative(text::narrative(body, &scope, placed)?), None),
        BlockBody::Summary(body) => (BlockBody::Summary(text::summary(body, &mut scope)?), None),
        BlockBody::DataTable(body) => tables::data_table(body, &mut scope).map(|(body, coverage)| (BlockBody::DataTable(body), coverage))?,
        BlockBody::Comparison(body) => (BlockBody::Comparison(tables::comparison(body, &mut scope)?), None),
        BlockBody::Kpi(body) => (BlockBody::Kpi(tables::kpi(body, &mut scope)?), None),
        BlockBody::LineChart(body) => (BlockBody::LineChart(charts::line_chart(body, &mut scope)?), None),
        BlockBody::BarChart(body) => charts::bar_chart(body, &mut scope).map(|(body, coverage)| (BlockBody::BarChart(body), coverage))?,
        BlockBody::Map(body) => map::map(body, &mut scope).map(|(body, coverage)| (BlockBody::Map(body), coverage))?,
        BlockBody::Timeline(body) => timeline::timeline(body, &mut scope).map(|(body, coverage)| (BlockBody::Timeline(body), coverage))?,
        BlockBody::Profile(body) => (BlockBody::Profile(entities::profile(body, &mut scope)?), None),
        BlockBody::Relationship(body) => (BlockBody::Relationship(entities::relationship(body, &mut scope)?), None),
    };
    Ok(AnswerBlock {
        provisional: ctx.provisional,
        produced_by: BlockProducer::Model,
        coverage,
        source_status: source_status(ctx, &scope.used),
        body,
        ..block
    })
}

fn size(value: &impl serde::Serialize) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}

/// Validate the model's `blocks` against the saved sources. Every value is
/// rebuilt by the server; the model's values are discarded. Observations no
/// narrative or summary places are added in a server narrative, so no
/// validated text is lost. Summary conclusions start with a partial scope;
/// see `set_conclusion_scope`.
pub fn validate_blocks(raw: &Value, ctx: &BlockContext<'_>) -> BlocksOutcome {
    let mut outcome = BlocksOutcome::default();
    let items = match raw.as_array() {
        Some(items) => items.as_slice(),
        None => {
            outcome.rejected.push(BlockRejection::new(None, None, BlockIssue::Schema, ""));
            &[]
        }
    };
    let mut ids = BTreeSet::new();
    let mut placed = BTreeSet::new();
    for item in items {
        let block = match parse(item) {
            Ok(block) => block,
            Err(rejection) => {
                outcome.rejected.push(rejection);
                continue;
            }
        };
        let (id, kind) = (block.id.clone(), block.kind());
        let reject = |issue: Issue| BlockRejection::new(Some(&id), Some(kind), issue.code, issue.path);
        let checked = ensure(outcome.blocks.len() < MAX_BLOCKS, BlockIssue::TooManyItems, "")
            .and_then(|()| ensure(valid_numbered_id(&id, 'b', 3), BlockIssue::Schema, "id"))
            .and_then(|()| ensure(!ids.contains(&id), BlockIssue::DuplicateId, "id"))
            .and_then(|()| validate_block(block, ctx, &mut placed));
        match checked {
            Ok(block) => {
                ids.insert(id);
                outcome.blocks.push(block);
            }
            Err(issue) => outcome.rejected.push(reject(issue)),
        }
    }
    // A block's data that does not fit the task budget is dropped from the end.
    let budget = MAX_BLOCKS_BYTES.saturating_sub(size(&ctx.derivations));
    while size(&outcome.blocks) > budget {
        let Some(index) = outcome.blocks.iter().rposition(|block| block.kind() != BlockKind::Narrative) else { break };
        let block = outcome.blocks.remove(index);
        outcome.rejected.push(BlockRejection::new(Some(&block.id), Some(block.kind()), BlockIssue::TooLarge, ""));
    }
    for block in &outcome.blocks {
        if let BlockBody::Summary(summary) = &block.body {
            placed.extend(summary.finding_ids.iter().cloned());
        }
    }
    let unplaced: Vec<String> = ctx.observation_ids.iter().filter(|id| !placed.contains(*id)).cloned().collect();
    if !unplaced.is_empty() {
        let next = outcome.blocks.iter().filter_map(|block| block.id[1..].parse::<usize>().ok()).max().unwrap_or(0) + 1;
        let narrative = AnswerBlock {
            provisional: ctx.provisional,
            produced_by: BlockProducer::Deterministic,
            ..AnswerBlock::new(format!("b{next}"), BlockBody::Narrative(NarrativeBlock { observation_ids: unplaced }))
        };
        // Text first when the model placed none, as the answer's lead.
        let first = !outcome.blocks.iter().any(|block| block.kind() == BlockKind::Narrative);
        outcome.blocks.insert(if first { 0 } else { outcome.blocks.len() }, narrative);
    }
    outcome
}

/// After completeness is reconciled: a conclusion is complete only when every
/// requirement is answered and nothing is provisional.
pub fn set_conclusion_scope(blocks: &mut [AnswerBlock], complete: bool) {
    for block in blocks {
        if let BlockBody::Summary(summary) = &mut block.body {
            if let Some(conclusion) = &mut summary.conclusion {
                conclusion.scope = if complete && !block.provisional { ConclusionScope::Complete } else { ConclusionScope::Partial };
            }
        }
    }
}
