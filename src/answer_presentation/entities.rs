//! Profile / dossier (#52) and relationship diagram (#53). A finished source
//! call is not a passed check, and equal names are not a proven relation.
use super::period::event_time;
use super::period::scalar_text;
use super::resolve::{ensure, Checked, Scope};
use crate::answer_blocks::{
    BlockIssue, CheckOutcome, DatePrecision, Fact, IdentityBasis, LabeledFact, ProfileBlock, ProfileCheck, RelationEdge,
    RelationNode, RelationshipBlock, SourceFreshness, SourceState, MAX_PROFILE_ITEMS, MAX_RELATION_EDGES, MAX_RELATION_NODES,
};
use std::collections::BTreeSet;

fn labeled_list(items: &[LabeledFact], scope: &mut Scope, path: &str) -> Checked<Vec<LabeledFact>> {
    ensure(items.len() <= MAX_PROFILE_ITEMS, BlockIssue::TooManyItems, path)?;
    items.iter().enumerate().map(|(index, item)| scope.labeled(item, &format!("{path}/{index}"))).collect()
}

/// The strongest outcome the evidence supports; anything else is unknown.
fn supported_outcome(check: &ProfileCheck, evidence: &[Fact], scope: &mut Scope) -> CheckOutcome {
    match check.outcome {
        CheckOutcome::Confirmed | CheckOutcome::Contradicted if evidence.iter().any(|fact| !fact.value.is_null()) => check.outcome,
        CheckOutcome::NoMatchInScope => {
            let step = scope.ctx.source_status.iter().find(|status| Some(&status.step_key) == check.step_key.as_ref());
            let complete = step
                .filter(|status| status.state == SourceState::Completed)
                .and_then(|status| status.result_ref.as_deref())
                .and_then(|result_ref| scope.document(result_ref, "").ok())
                .is_some_and(|document| document["coverage"] == "source_response");
            if complete { CheckOutcome::NoMatchInScope } else { CheckOutcome::Unknown }
        }
        _ => CheckOutcome::Unknown,
    }
}

pub(super) fn profile(block: ProfileBlock, scope: &mut Scope) -> Checked<ProfileBlock> {
    scope.label(&block.subject_label, "subject_label")?;
    let name = scope.optional_fact(block.name.as_ref(), "name")?;
    let identifiers = labeled_list(&block.identifiers, scope, "identifiers")?;
    let status = block.status.as_ref().map(|status| scope.labeled(status, "status")).transpose()?;
    let attributes = labeled_list(&block.attributes, scope, "attributes")?;
    ensure(block.checks.len() <= MAX_PROFILE_ITEMS, BlockIssue::TooManyItems, "checks")?;
    let mut checks = Vec::with_capacity(block.checks.len());
    for (index, check) in block.checks.iter().enumerate() {
        let path = format!("checks/{index}");
        scope.item_id(&check.id, &path)?;
        scope.label(&check.label, &format!("{path}/label"))?;
        if let Some(requirement) = &check.requirement_id {
            ensure(scope.ctx.requirements.iter().any(|r| r.id == *requirement), BlockIssue::UnknownReference, format!("{path}/requirement_id"))?;
        }
        if let Some(step) = &check.step_key {
            ensure(scope.ctx.source_status.iter().any(|s| s.step_key == *step), BlockIssue::UnknownReference, format!("{path}/step_key"))?;
        }
        let evidence = check.evidence.iter().enumerate().map(|(i, fact)| scope.fact(fact, &format!("{path}/evidence/{i}"))).collect::<Checked<Vec<_>>>()?;
        let outcome = supported_outcome(check, &evidence, scope);
        checks.push(ProfileCheck { outcome, evidence, ..check.clone() });
    }
    let mut as_of = Vec::new();
    for (index, entry) in block.freshness.iter().enumerate() {
        if let Some(fact) = scope.optional_fact(entry.as_of.as_ref(), &format!("freshness/{index}/as_of"))? {
            as_of.push((entry.result_ref.clone(), fact));
        }
    }
    let used: Vec<String> = scope.used.iter().cloned().collect();
    let mut freshness = Vec::with_capacity(used.len());
    for result_ref in used {
        let document = scope.document(&result_ref, "freshness")?;
        freshness.push(SourceFreshness {
            retrieved_at: document["source"]["retrieved_at"].as_str().map(str::to_owned),
            as_of: as_of.iter().find(|(owner, _)| *owner == result_ref).map(|(_, fact)| fact.clone()),
            result_ref,
        });
    }
    Ok(ProfileBlock { name, identifiers, status, attributes, checks, freshness, ..block })
}

/// The record a pointer lies in: up to its deepest array element.
fn record_of(pointer: &str) -> &str {
    let mut end = 0;
    let mut offset = 0;
    for segment in pointer.split('/').skip(1) {
        offset += segment.len() + 1;
        if !segment.is_empty() && segment.bytes().all(|b| b.is_ascii_digit()) {
            end = offset;
        }
    }
    &pointer[..end]
}

fn normalized(fact: &Fact) -> Option<String> {
    let text: String = scalar_text(&fact.value).chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect();
    (!fact.value.is_null() && !text.is_empty()).then_some(text)
}

fn node_results(node: &RelationNode) -> BTreeSet<&str> {
    node.name.iter().chain(node.identifiers.iter().filter_map(|item| item.fact.as_ref())).filter_map(|fact| fact.result_ref.as_deref()).collect()
}

/// How an edge's endpoint is tied to the record that states the relation:
/// the node comes from that same result, or one of its identifiers appears
/// in that record. A matching name alone is never enough.
fn endpoint_basis(node: &RelationNode, result_ref: &str, evidence: &[Fact]) -> Option<IdentityBasis> {
    if node_results(node).contains(result_ref) {
        return Some(IdentityBasis::SameRecord);
    }
    let stated: BTreeSet<String> = evidence.iter().filter_map(normalized).collect();
    node.identifiers
        .iter()
        .filter_map(|item| item.fact.as_ref().and_then(normalized))
        .any(|identifier| stated.contains(&identifier))
        .then_some(IdentityBasis::IdentifierMatch)
}

fn validity(fact: Option<&Fact>, scope: &mut Scope, path: &str) -> Checked<Option<Fact>> {
    let fact = scope.optional_fact(fact, path)?;
    if let Some(value) = fact.as_ref().map(|fact| &fact.value).filter(|value| !value.is_null()) {
        ensure(event_time(value).precision != DatePrecision::Unknown, BlockIssue::UnparsedPeriod, path)?;
    }
    Ok(fact)
}

pub(super) fn relationship(block: RelationshipBlock, scope: &mut Scope) -> Checked<RelationshipBlock> {
    ensure(block.nodes.len() <= MAX_RELATION_NODES, BlockIssue::TooManyItems, "nodes")?;
    ensure(!block.edges.is_empty(), BlockIssue::Schema, "edges")?;
    ensure(block.edges.len() <= MAX_RELATION_EDGES, BlockIssue::TooManyItems, "edges")?;
    let mut nodes = Vec::with_capacity(block.nodes.len());
    for (index, node) in block.nodes.iter().enumerate() {
        let path = format!("nodes/{index}");
        scope.item_id(&node.id, &path)?;
        scope.label(&node.label, &format!("{path}/label"))?;
        let name = scope.optional_fact(node.name.as_ref(), &format!("{path}/name"))?;
        let identifiers = labeled_list(&node.identifiers, scope, &format!("{path}/identifiers"))?;
        nodes.push(RelationNode { name, identifiers, ..node.clone() });
    }
    let mut edges = Vec::with_capacity(block.edges.len());
    for (index, edge) in block.edges.iter().enumerate() {
        let path = format!("edges/{index}");
        scope.item_id(&edge.id, &path)?;
        scope.label(&edge.label, &format!("{path}/label"))?;
        let endpoint = |id: &str| nodes.iter().find(|node| node.id == id);
        let (from, to) = (endpoint(&edge.from), endpoint(&edge.to));
        ensure(from.is_some(), BlockIssue::UnknownReference, format!("{path}/from"))?;
        ensure(to.is_some() && edge.from != edge.to, BlockIssue::UnknownReference, format!("{path}/to"))?;
        let evidence = edge.evidence.iter().enumerate().map(|(i, fact)| scope.fact(fact, &format!("{path}/evidence/{i}"))).collect::<Checked<Vec<_>>>()?;
        // One stated record: a single result and one array element.
        let records: BTreeSet<(Option<&str>, Option<&str>)> = evidence.iter().map(|fact| (fact.result_ref.as_deref(), fact.pointer.as_deref().map(record_of))).collect();
        let stated = evidence.iter().any(|fact| !fact.value.is_null());
        let single = records.len() == 1 && records.iter().all(|(result, pointer)| result.is_some() && pointer.is_some());
        ensure(stated && single, BlockIssue::MissingRelationEvidence, format!("{path}/evidence"))?;
        let result_ref = evidence[0].result_ref.clone().unwrap_or_default();
        let bases = [from, to].map(|node| node.and_then(|node| endpoint_basis(node, &result_ref, &evidence)));
        let [Some(from_basis), Some(to_basis)] = bases else {
            return Err(super::resolve::Issue { code: BlockIssue::NameOnlyRelation, path });
        };
        let identity_basis = if from_basis == IdentityBasis::IdentifierMatch || to_basis == IdentityBasis::IdentifierMatch {
            IdentityBasis::IdentifierMatch
        } else {
            IdentityBasis::SameRecord
        };
        edges.push(RelationEdge {
            valid_from: validity(edge.valid_from.as_ref(), scope, &format!("{path}/valid_from"))?,
            valid_to: validity(edge.valid_to.as_ref(), scope, &format!("{path}/valid_to"))?,
            evidence,
            identity_basis: Some(identity_basis),
            ..edge.clone()
        });
    }
    Ok(RelationshipBlock { nodes, edges })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_is_the_deepest_array_element() {
        assert_eq!(record_of("/data/officers/3/name"), "/data/officers/3");
        assert_eq!(record_of("/data/groups/1/members/0/role"), "/data/groups/1/members/0");
        assert_eq!(record_of("/data/name"), "");
    }
}
