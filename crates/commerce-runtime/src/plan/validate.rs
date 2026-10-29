//! Paid work is admitted only with typed inputs, explicit mappings and a bounded DAG.
use super::{Binding, CoverageStatus, Intent, Plan, PlanNode};
use crate::facts::{contract::Vocabulary, schema};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub fn fact(kind: &str, value: &Value, vocabulary: &Vocabulary) -> Result<(), String> {
    let definition = vocabulary
        .get(kind)
        .ok_or_else(|| format!("unknown fact type: {kind}"))?;
    schema::check_schema(&definition.value_schema).map_err(|e| e.to_string())?;
    if value.is_null() || value.as_str().is_some_and(|s| s.trim().is_empty()) {
        return Err(format!("empty fact: {kind}"));
    }
    schema::validate(&definition.value_schema, value).map_err(|e| e.to_string())
}

pub fn intent(intent: &Intent, vocabulary: &Vocabulary) -> Result<(), String> {
    // Several subjects are planned as several branches, never merged: a role
    // is what keeps one company's lookup from binding into another's call, so
    // a repeated or unnamed role is refused rather than deduplicated.
    //
    // There is no invented ceiling on how many companies a question may name.
    // The bound is the compiler's own: a subject cannot be researched without
    // at least one paid call, so more subjects than the paid-node budget is
    // arithmetic, not policy. Anything that fits is compiled and priced, and
    // anything that does not is refused by the compiler with the count.
    if intent.subjects.is_empty()
        || intent.subjects.len() > super::compile::Limits::default().max_paid_nodes
    {
        return Err("unsupported subject count".into());
    }
    let mut roles = BTreeSet::new();
    for subject in &intent.subjects {
        if !is_role(&subject.role) || !roles.insert(&subject.role) {
            return Err("subject roles must be distinct".into());
        }
    }
    if intent.required_outputs.is_empty()
        || intent.required_outputs.len() + intent.optional_outputs.len() > 32
    {
        return Err("invalid output count".into());
    }
    if intent
        .max_buyer_total_atomic
        .is_some_and(|n| !(0..=i64::MAX as i128).contains(&n))
    {
        return Err("invalid price ceiling".into());
    }
    for kind in intent
        .required_outputs
        .iter()
        .chain(&intent.optional_outputs)
    {
        if !vocabulary.contains_key(kind) {
            return Err(format!("unknown output: {kind}"));
        }
    }
    for subject in &intent.subjects {
        for (kind, value) in &subject.known {
            fact(kind, value, vocabulary)?;
        }
    }
    Ok(())
}

/// A subject role, as narrow as it is allowed to be. Roles are compared in SQL
/// to decide which facts may bind into which call, so they stay to a short
/// lowercase name and nothing else.
fn is_role(role: &str) -> bool {
    !role.is_empty()
        && role.len() <= 32
        && role
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

pub fn plan(plan: &Plan, vocabulary: &Vocabulary) -> Result<(), String> {
    let hard = super::compile::Limits::default();
    if plan.limits.max_paid_nodes == 0
        || plan.limits.max_paid_nodes > hard.max_paid_nodes
        || plan.limits.max_depth == 0
        || plan.limits.max_depth > hard.max_depth
        || plan.limits.max_concurrency == 0
        || plan.limits.max_concurrency > hard.max_concurrency
        || plan.nodes.len() > plan.limits.max_paid_nodes
    {
        return Err("invalid plan limits".into());
    }
    let mut seen: BTreeMap<&str, &PlanNode> = BTreeMap::new();
    let mut depths = BTreeMap::new();
    let mut sum = 0i128;
    for node in &plan.nodes {
        if node.id.is_empty() || seen.contains_key(node.id.as_str()) || !is_role(&node.subject_role)
        {
            return Err("duplicate node or unsupported subject".into());
        }
        if !matches!(
            node.endpoint_method.as_str(),
            "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD"
        ) {
            return Err("unsupported HTTP method".into());
        }
        let contract = node
            .contract
            .as_ref()
            .ok_or("plan needs a new quote with snapshotted mappings")?;
        contract.check(vocabulary).map_err(|e| e.to_string())?;
        if node.capability_slug != contract.capability_slug
            || node.produces
                != contract
                    .output_map
                    .produces
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
        {
            return Err("contract does not match plan".into());
        }
        if node.buyer_total_atomic <= 0 {
            return Err("unverifiable price".into());
        }
        sum = sum
            .checked_add(node.buyer_total_atomic)
            .filter(|n| *n <= i64::MAX as i128)
            .ok_or("price overflow")?;
        let mut dependencies = BTreeSet::new();
        for (kind, binding) in &node.bindings {
            if !contract.requires.iter().any(|r| &r.fact_type == kind)
                || !contract.input_map.contains_key(kind)
            {
                return Err("undeclared input binding".into());
            }
            match binding {
                Binding::Known { value } => fact(kind, value, vocabulary)?,
                Binding::Node { node_id } => {
                    let upstream = seen
                        .get(node_id.as_str())
                        .ok_or("dependency is missing, cyclic or out of order")?;
                    if upstream.subject_role != node.subject_role
                        || !upstream.produces.contains(kind)
                    {
                        return Err("dependency cannot provide this fact".into());
                    }
                    dependencies.insert(node_id.clone());
                }
            }
        }
        if node.depends_on != dependencies.into_iter().collect::<Vec<_>>() {
            return Err("dependency graph differs from bindings".into());
        }
        for required in contract.requires.iter().filter(|r| !r.optional) {
            if !node.bindings.contains_key(&required.fact_type) {
                return Err(format!("missing input: {}", required.fact_type));
            }
        }
        let depth = 1 + node
            .depends_on
            .iter()
            .filter_map(|id| depths.get(id))
            .max()
            .copied()
            .unwrap_or(0);
        if depth > plan.limits.max_depth {
            return Err("dependency depth exceeded".into());
        }
        depths.insert(node.id.clone(), depth);
        seen.insert(&node.id, node);
    }
    if plan.quote.buyer_total_atomic != sum || plan.quote.lines.len() != plan.nodes.len() {
        return Err("quote does not match plan".into());
    }
    for (line, node) in plan.quote.lines.iter().zip(&plan.nodes) {
        if line.node_id != node.id
            || line.capability_slug != node.capability_slug
            || line.buyer_total_atomic != node.buyer_total_atomic
            || line.source_cost_atomic <= 0
            || line.apiosk_fee_atomic
                != crate::fees::markup_atomic(
                    line.source_cost_atomic,
                    crate::fees::BUYER_MARKUP_BPS,
                )
            || line.source_cost_atomic + line.apiosk_fee_atomic != line.buyer_total_atomic
        {
            return Err("quote line mismatch".into());
        }
    }
    if !plan.coverage.iter().any(|c| c.required) {
        return Err("no required output".into());
    }
    for covered in &plan.coverage {
        if !vocabulary.contains_key(&covered.fact_type) || !is_role(&covered.subject_role) {
            return Err("unknown coverage type".into());
        }
        match covered.status {
            CoverageStatus::Planned => {
                let node = covered
                    .node_id
                    .as_deref()
                    .and_then(|id| seen.get(id))
                    .ok_or("missing output node")?;
                // The node must be the one planned for THIS subject. A
                // coverage row pointing at the other company's lookup is how a
                // two-company answer quietly becomes one company twice.
                if !node.produces.contains(&covered.fact_type)
                    || node.subject_role != covered.subject_role
                {
                    return Err("output is not produced".into());
                }
            }
            CoverageStatus::Unreachable if covered.required => {
                return Err("required output unreachable".into())
            }
            _ => {
                if covered.node_id.is_some() {
                    return Err("invalid output source".into());
                }
            }
        }
    }
    Ok(())
}

/// Select exactly the provenance the approved bindings named; never merge all
/// dependency outputs into a last-wins map. Known inputs follow the same schema.
pub fn inputs(
    node: &PlanNode,
    facts: &[(String, String, Value)],
    vocabulary: &Vocabulary,
) -> Result<BTreeMap<String, Value>, String> {
    let mut out = BTreeMap::new();
    for (kind, binding) in &node.bindings {
        let value = match binding {
            Binding::Known { value } => value.clone(),
            Binding::Node { node_id } => {
                let mut matching = facts
                    .iter()
                    .filter(|(source, fact, _)| source == node_id && fact == kind)
                    .map(|(_, _, v)| v);
                let value = matching
                    .next()
                    .ok_or_else(|| format!("missing dependency fact: {kind}"))?;
                if matching.any(|other| other != value) {
                    return Err(format!("ambiguous dependency fact: {kind}"));
                }
                value.clone()
            }
        };
        fact(kind, &value, vocabulary)?;
        out.insert(kind.clone(), value);
    }
    Ok(out)
}

/// Facts whose absence must keep this node from being reported as successful.
pub fn required_facts(plan: &Plan, node_id: &str) -> Vec<String> {
    let mut required: BTreeSet<String> = plan
        .coverage
        .iter()
        .filter(|c| c.required && c.node_id.as_deref() == Some(node_id))
        .map(|c| c.fact_type.clone())
        .collect();
    for node in &plan.nodes {
        for (kind, binding) in &node.bindings {
            if matches!(binding, Binding::Node { node_id: source } if source == node_id) {
                required.insert(kind.clone());
            }
        }
    }
    required.into_iter().collect()
}
