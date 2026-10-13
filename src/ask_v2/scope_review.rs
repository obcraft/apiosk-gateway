//! Compare the original request with its interpretation before source discovery.
//! A schema-valid interpretation can still omit a clause. The review inventories
//! literal request excerpts and maps them to the interpreted outputs; it never
//! receives a catalogue, source response or executable operation.
use super::{Client, Output};
use crate::capability_intake::{self, ParsedRequest};
use anyhow::{anyhow, ensure, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub(super) const PROMPT: &str = include_str!("../../contracts/intake-scope-review-prompt.txt");
const UNVERIFIED: &str = "ask_v2_interpretation_unverified";

pub(crate) struct Interpretation {
    pub parsed_request: ParsedRequest,
    pub output: Output,
    pub attempts: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Review {
    requirements: Vec<Requirement>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Requirement {
    quote: String,
    capabilities: Vec<CapabilityReference>,
    operations: Vec<String>,
    format: Option<String>,
    issue: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityReference {
    index: usize,
    outputs: Vec<String>,
}

impl Client {
    /// One canonical boundary for the App, guest preview and API. A complete
    /// interpretation is returned only after its scope review succeeds. The
    /// full sequence (including rate-limit waits) shares the former two-parse
    /// budget: at most twice the transport deadline, normally forty seconds.
    pub(crate) async fn interpret_with_context(
        &self,
        model_request: &str,
        question: &str,
        literals: &str,
        known_slugs: &[String],
    ) -> Result<Interpretation> {
        tokio::time::timeout(self.deadline.saturating_mul(2), async {
            let mut correction = None;
            let mut review_correction_available = true;
            for attempt in 1..=2 {
                let output = self.parse(model_request, known_slugs, correction.as_deref()).await?;
                let parsed = match capability_intake::normalize_with_context(
                    &output.text, question, literals, &output.model,
                ) {
                    Ok(parsed) => parsed,
                    Err(error) => {
                        #[cfg(test)]
                        crate::live_ask_planning_tests::record_validation_failure(
                            &json!({"stage":"intake_normalization","attempt":attempt,"question":question,"raw":output.text}).to_string(), &error);
                        if attempt == 1 {
                            correction = Some(error.to_string());
                            continue;
                        }
                        return Err(anyhow!(UNVERIFIED));
                    }
                };
                let feedback = self.review_scope(model_request, question, literals, &parsed, &mut review_correction_available).await?;
                match feedback {
                    None => return Ok(Interpretation { parsed_request: parsed, output, attempts: attempt }),
                    Some(feedback) if attempt == 1 => correction = Some(feedback),
                    Some(_) => return Err(anyhow!(UNVERIFIED)),
                }
            }
            Err(anyhow!(UNVERIFIED))
        }).await.map_err(|_| anyhow!("ask_v2_model_timeout"))?
    }

    async fn review_scope(
        &self,
        model_request: &str,
        question: &str,
        literals: &str,
        parsed: &ParsedRequest,
        correction_available: &mut bool,
    ) -> Result<Option<String>> {
        let mut interpretation = serde_json::to_value(parsed)?;
        interpretation.as_object_mut().expect("serialized request").remove("meta");
        let input = json!({"question":question,"request_context":model_request,"interpretation":interpretation});
        let input = input.to_string();
        let raw = self.generate_object_with_effort(PROMPT, &input, 3072, true, Some("medium")).await?;
        let reviewed = validate_review(&raw, literals, parsed);
        #[cfg(test)]
        record_review_failure("intake_scope_review", &raw, question, parsed, &reviewed);
        match reviewed {
            Ok(feedback) => Ok(feedback),
            Err(_) if *correction_available => {
                // One review-shape repair across the complete interpretation,
                // never a silent approval and never an extra source lookup.
                *correction_available = false;
                let prompt = format!("{PROMPT}\nThe previous review failed JSON shape, literal excerpt or reference validation. Rebuild the complete review with valid request excerpts and exact interpretation references. Preserve all requested requirements; do not omit or approve a requirement merely to repair the response shape.");
                let raw = self.generate_object_with_effort(&prompt, &input, 3072, true, Some("medium")).await?;
                let reviewed = validate_review(&raw, literals, parsed);
                #[cfg(test)]
                record_review_failure("intake_scope_review_repair", &raw, question, parsed, &reviewed);
                reviewed.map_err(|_| anyhow!(UNVERIFIED))
            }
            Err(_) => Err(anyhow!(UNVERIFIED)),
        }
    }
}

/// Opt-in local diagnostics for the live-model test harness. Neither prompts
/// nor model responses are written by production builds or without the
/// explicitly configured private diagnostic path.
#[cfg(test)]
fn record_review_failure(stage: &str, raw: &str, question: &str, parsed: &ParsedRequest, result: &Result<Option<String>>) {
    let error = match result {
        Err(error) => anyhow!("scope review validation: {error}"),
        Ok(Some(feedback)) => anyhow!("scope review incomplete: {feedback}"),
        Ok(None) => return,
    };
    crate::live_ask_planning_tests::record_validation_failure(
        &json!({"stage":stage,"question":question,"interpretation":parsed,"raw":raw}).to_string(), &error);
}

/// Validate the review's evidence and reference map before accepting its
/// verdict. Unknown indices, fabricated quotes or unaccounted outputs cannot
/// turn an incomplete interpretation into a ready request.
fn validate_review(raw: &str, literals: &str, parsed: &ParsedRequest) -> Result<Option<String>> {
    let review: Review = serde_json::from_value(capability_intake::extract_json(raw)?)?;
    ensure!(!review.requirements.is_empty() && review.requirements.len() <= 32, "invalid requirement inventory");
    let mut capabilities = BTreeSet::new();
    let mut outputs = BTreeSet::new();
    let mut operations = BTreeSet::new();
    let mut format = parsed.deliverable.format == "chat";
    let mut issues = Vec::new();
    for requirement in review.requirements {
        ensure!(!requirement.quote.trim().is_empty() && requirement.quote.chars().count() <= 500
            && literals.contains(&requirement.quote), "review quote must be literal user context");
        ensure!(requirement.capabilities.len() <= 16 && requirement.operations.len() <= 5, "oversized requirement map");
        for reference in &requirement.capabilities {
            let capability = parsed.capabilities.get(reference.index).ok_or_else(|| anyhow!("unknown capability reference"))?;
            ensure!(reference.outputs.len() <= 32, "oversized output map");
            capabilities.insert(reference.index);
            for output in &reference.outputs {
                ensure!(capability.outputs.contains(output), "unknown output reference");
                outputs.insert((reference.index, output.clone()));
            }
        }
        for operation in &requirement.operations {
            ensure!(parsed.deliverable.operations.contains(operation), "unknown operation reference");
            operations.insert(operation.clone());
        }
        if let Some(selected) = &requirement.format {
            ensure!(selected == &parsed.deliverable.format, "unknown format reference");
            format = true;
        }
        if let Some(issue) = requirement.issue {
            ensure!(!issue.trim().is_empty() && issue.chars().count() <= 500, "invalid review issue");
            issues.push(json!({"quote":requirement.quote,"issue":issue}));
        } else {
            ensure!(!requirement.capabilities.is_empty() || !requirement.operations.is_empty()
                || requirement.format.is_some(), "unmapped request requirement");
        }
    }
    for (index, capability) in parsed.capabilities.iter().enumerate() {
        if !capabilities.contains(&index) || capability.outputs.iter().any(|output| !outputs.contains(&(index, output.clone()))) {
            issues.push(json!({"issue":"An interpreted capability or output has no requested-result mapping.","capability":index}));
        }
    }
    if parsed.deliverable.operations.iter().any(|operation| !operations.contains(operation)) || !format {
        issues.push(json!({"issue":"An interpreted presentation operation or format has no requested-result mapping."}));
    }
    if issues.is_empty() { return Ok(None); }
    let mut feedback = format!("The independent request-scope review found missing or changed requirements: {}. Reinterpret the complete original request, fixing these issues while preserving every other requested result, subject and supplied value. Do not answer or choose sources.", Value::Array(issues));
    if feedback.len() > 14_000 {
        let mut boundary = 14_000;
        while !feedback.is_char_boundary(boundary) { boundary -= 1; }
        feedback.truncate(boundary);
    }
    Ok(Some(feedback))
}

#[cfg(test)]
mod tests;
