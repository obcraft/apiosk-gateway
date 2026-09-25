//! Narrative and summary blocks (#54): validated observations placed once, and
//! a conclusion whose numbers come only from facts.
use super::period::scalar_text;
use super::resolve::{ensure, fail, valid_prose, Checked, Scope};
use crate::answer_blocks::{BlockIssue, NarrativeBlock, Narrative, RequirementStatus, SummaryBlock};
use std::collections::BTreeSet;

const SLOTS: [&str; 4] = ["{{a}}", "{{b}}", "{{c}}", "{{d}}"];

/// Observations cited by id; an observation appears in one narrative only.
pub(super) fn narrative(block: NarrativeBlock, scope: &Scope, placed: &mut BTreeSet<String>) -> Checked<NarrativeBlock> {
    for (index, id) in block.observation_ids.iter().enumerate() {
        let path = format!("observation_ids/{index}");
        ensure(scope.ctx.observation_ids.contains(id), BlockIssue::UnknownReference, &path)?;
        ensure(!placed.contains(id), BlockIssue::DuplicateId, &path)?;
    }
    let ids: BTreeSet<_> = block.observation_ids.iter().collect();
    ensure(ids.len() == block.observation_ids.len(), BlockIssue::DuplicateId, "observation_ids")?;
    placed.extend(block.observation_ids.iter().cloned());
    Ok(block)
}

/// Prose rules as for observations; `{{a}}`..`{{d}}` take the facts' server values.
fn conclusion(narrative: &Narrative, scope: &mut Scope) -> Checked<Narrative> {
    ensure(valid_prose(&narrative.text), BlockIssue::InvalidLabel, "conclusion/text")?;
    ensure(narrative.facts.len() <= SLOTS.len(), BlockIssue::TooManyItems, "conclusion/facts")?;
    let facts = narrative
        .facts
        .iter()
        .enumerate()
        .map(|(index, fact)| scope.fact(fact, &format!("conclusion/facts/{index}")))
        .collect::<Checked<Vec<_>>>()?;
    let mut text = narrative.text.clone();
    for (index, slot) in SLOTS.iter().enumerate() {
        if !text.contains(slot) {
            continue;
        }
        let Some(fact) = facts.get(index) else { return fail(BlockIssue::UnknownReference, format!("conclusion/facts/{index}")) };
        // An unknown value cannot state a conclusion.
        ensure(!fact.value.is_null(), BlockIssue::MissingField, format!("conclusion/facts/{index}"))?;
        text = text.replace(slot, &scalar_text(&fact.value));
    }
    ensure(!text.contains("{{") && !text.contains("}}"), BlockIssue::InvalidLabel, "conclusion/text")?;
    Ok(Narrative { text, facts, scope: Default::default() })
}

/// Findings are observations; open points are missing information,
/// limitations or requirements still unknown. A provisional answer states no
/// conclusion. The scope stays partial until `set_conclusion_scope`.
pub(super) fn summary(block: SummaryBlock, scope: &mut Scope) -> Checked<SummaryBlock> {
    let ctx = scope.ctx;
    for (index, id) in block.finding_ids.iter().enumerate() {
        ensure(ctx.observation_ids.contains(id), BlockIssue::UnknownReference, format!("finding_ids/{index}"))?;
    }
    for (index, id) in block.open_point_ids.iter().enumerate() {
        let limitation = id.strip_prefix('l').and_then(|n| n.parse::<usize>().ok()).is_some_and(|n| (1..=ctx.limitation_count).contains(&n));
        let unknown = ctx.requirements.iter().any(|r| r.id == *id && r.status == RequirementStatus::Unknown);
        ensure(limitation || unknown || ctx.missing_ids.contains(id), BlockIssue::UnknownReference, format!("open_point_ids/{index}"))?;
    }
    let conclusion = match &block.conclusion {
        Some(_) if ctx.provisional => return fail(BlockIssue::ConclusionOnIncomplete, "conclusion"),
        Some(narrative) => Some(conclusion(narrative, scope)?),
        None => None,
    };
    Ok(SummaryBlock { conclusion, ..block })
}
