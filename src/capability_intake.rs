//! Ask intake: validated capabilities first, catalog discovery second. Discovery
//! returns metadata; only the existing planner and approval can execute a source.
use crate::{
    catalog::{Catalog, Service},
    state::Task,
};
use anyhow::ensure;
use futures_util::{stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

mod discovery_cache;
#[cfg(test)]
pub(crate) mod named_source_fields;
pub(crate) mod broad_information;
pub(crate) mod company_lookup;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod capability_merge_tests;

const DOMAINS: &[&str] = &[
    "company",
    "location",
    "compliance",
    "vehicle",
    "finance",
    "legal",
    "procurement",
    "document",
    "research",
    "general",
];
const FORMATS: &[&str] = &["chat", "pdf", "xlsx", "docx", "json"];
const OPERATIONS: &[&str] = &["summarize", "analyze", "compare", "list", "export"];
pub const BAZAAR_SEARCH: &str = "https://api.cdp.coinbase.com/platform/v2/x402/discovery/search";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subject {
    pub id: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub identifiers: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capability {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub domain: String,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub subject_id: Option<String>,
    pub source_hint: Option<String>,
    pub confidence: f64,
    /// Computed from the parse, never trusted from model output. A dependency
    /// may supply these concepts before execution; they are not all questions.
    #[serde(default)]
    pub missing: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deliverable {
    pub format: String,
    pub operations: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub model: String,
    pub parsed_at: String,
    pub text_sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedRequest {
    pub language: String,
    pub subjects: Vec<Subject>,
    pub capabilities: Vec<Capability>,
    pub deliverable: Deliverable,
    pub meta: Meta,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub origin: String,
    pub name: String,
    pub description: String,
    pub source_slug: Option<String>,
    pub capability: Option<String>,
    pub endpoint_id: Option<String>,
    pub url: Option<String>,
    pub method: String,
    pub price: Option<String>,
    pub network: Option<String>,
    pub calls_30d: Option<u64>,
    pub availability: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Match {
    pub slug: String,
    pub query: String,
    pub operations: BTreeSet<String>,
    pub apiosk: Vec<Candidate>,
    pub coinbase: Vec<Candidate>,
    pub coinbase_status: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Intake {
    pub parsed_request: ParsedRequest,
    pub matches: Vec<Match>,
    pub apiosk_sources_searched: usize,
    pub catalog_version: String,
    /// Contract slugs used by the planner, not new model-invented execution paths.
    pub selected_capabilities: BTreeSet<String>,
}

/// Operations over sources already obtained belong to the answer, not another
/// paid retrieval. Subject context does not turn provenance formatting into a
/// company lookup; named-source research and factual analysis remain retrievals.
pub(crate) fn answer_instruction(cap: &Capability) -> bool {
    let outputs: BTreeSet<_> = cap.outputs.iter().map(String::as_str).collect();
    if cap.slug == "general.capabilities" && cap.name == "Apiosk capabilities" && cap.domain == "general"
        && cap.subject_id.is_none() && cap.source_hint.is_none() && cap.inputs.is_empty()
        && outputs == BTreeSet::from(["available_capabilities", "missing_sources"]) {
        return true;
    }
    // Coverage gaps are derived from this request's admitted sources. A
    // subject identifier is context for that explanation, not a new lookup.
    if cap.source_hint.is_none() && outputs.len() == 1 && outputs.contains("missing_sources")
        && cap.inputs.iter().all(|value| matches!(value.as_str(),
            "subject_id" | "address" | "company_name" | "license_plate" | "analysis_sources")) {
        return true;
    }
    if cap.source_hint.is_none() && matches!(cap.domain.as_str(), "general" | "research") {
        if cap.subject_id.is_none() && !outputs.is_empty()
            && outputs.iter().all(|value| matches!(*value, "citation" | "citations" | "source_citation" | "source_citations" | "bibliography"))
            && cap.inputs.iter().all(|value| matches!(value.as_str(), "data_source" | "reference_type")) {
            return true;
        }
        if (outputs.contains("official_sources") || outputs.contains("is_official"))
            && (outputs.contains("ai_analysis") || outputs.contains("ai_generated_analysis") || outputs.contains("is_ai_generated"))
            && outputs.iter().all(|value| matches!(*value,
                "official_sources" | "ai_analysis" | "ai_generated_analysis" | "differences" | "comparison"
                    | "differences_in_reliability" | "data_origin" | "source_confidence" | "discrepancies"
                    | "source_type" | "is_official" | "is_ai_generated" | "source_reference"))
            && cap.inputs.iter().all(|value| matches!(value.as_str(), "subject_id" | "analysis_sources" | "data_source")) {
            return true;
        }
    }
    cap.subject_id.is_none() && cap.inputs.is_empty()
        && matches!(cap.slug.as_str(), "general.chat" | "general.conversation")
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or("").trim().to_owned()
}
fn snake(value: &str) -> String {
    value
        .to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}
fn names(value: &Value) -> anyhow::Result<Vec<String>> {
    let values = value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("inputs and outputs must be arrays"))?;
    ensure!(values.len() <= 32, "too many input or output concepts");
    let mut result = Vec::new();
    for value in values {
        let word = value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("concept must be text"))?;
        let word = snake(word);
        ensure!(!word.is_empty() && word.len() <= 100, "invalid concept");
        if !result.contains(&word) {
            result.push(word);
        }
    }
    Ok(result)
}
fn literal(request: &str, value: &str) -> bool {
    !value.is_empty() && request.to_lowercase().contains(&value.to_lowercase())
}
pub fn normalize_slug(slug: &str, domain: &str) -> Option<(String, String)> {
    let mut parts: Vec<String> = slug
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.chars().all(|c| !c.is_ascii_lowercase()) {
                s.to_lowercase()
            } else {
                format!("{}{}", s[..1].to_ascii_lowercase(), &s[1..])
            }
        })
        .collect();
    let prefix = parts.first().map(|s| s.to_lowercase()).unwrap_or_default();
    let domain = if DOMAINS.contains(&prefix.as_str()) {
        parts.remove(0);
        prefix
    } else if DOMAINS.contains(&domain.to_lowercase().as_str()) {
        domain.to_lowercase()
    } else {
        "general".into()
    };
    parts.retain(|part| {
        part != &domain
            && ![
                "a", "an", "and", "de", "een", "en", "for", "het", "of", "or", "the", "to", "van",
                "voor",
            ]
            .contains(&part.as_str())
    });
    if parts.is_empty() {
        return None;
    }
    Some((format!("{domain}.{}", parts.join(".")), domain))
}

/// Read the first complete JSON object, allowing fenced output and trailing prose.
pub fn extract_json(raw: &str) -> anyhow::Result<Value> {
    let start = raw
        .find('{')
        .ok_or_else(|| anyhow::anyhow!("missing JSON object"))?;
    let chars: Vec<_> = raw[start..].chars().collect();
    let mut repaired = String::new();
    let (mut quoted, mut escaped) = (false, false);
    for (i, &ch) in chars.iter().enumerate() {
        if quoted {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
            }
        } else if ch == '"' {
            quoted = true;
        } else if ch == ','
            && chars[i + 1..]
                .iter()
                .find(|ch| !ch.is_whitespace())
                .is_some_and(|ch| matches!(ch, '}' | ']'))
        {
            continue;
        }
        repaired.push(ch);
    }
    let mut values = serde_json::Deserializer::from_str(&repaired).into_iter::<Value>();
    values
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing JSON object"))?
        .map_err(Into::into)
}
#[cfg(test)]
pub fn conversation_language(request: &str) -> Option<&'static str> {
    let clean = request
        .trim()
        .trim_end_matches(['!', '?', '.', ',', ' '])
        .to_lowercase();
    let mut body = clean.as_str();
    for prefix in [
        "hi",
        "hey",
        "hello",
        "hoi",
        "hallo",
        "goedemorgen",
        "goedemiddag",
        "goedenavond",
    ] {
        if let Some(tail) = body
            .strip_prefix(prefix)
            .filter(|tail| tail.starts_with([' ', ',', '!', '.', '?']))
        {
            body = tail.trim_start_matches([' ', ',', '!', '.', '?']);
            break;
        }
    }
    if [
        "hoi",
        "hallo",
        "hoe gaat het",
        "hoe gaat het met je",
        "hoe gaat het met jou",
        "hoe gaat het met u",
        "hoe is het",
        "hoe is het met je",
        "hoe is het met jou",
        "dank je",
        "dank je wel",
        "dankjewel",
        "bedankt",
    ]
    .contains(&body)
    {
        Some("nl")
    } else if [
        "hi",
        "hey",
        "hello",
        "thanks",
        "thank you",
        "how are you",
        "how are you doing",
    ]
    .contains(&body)
    {
        Some("en")
    } else {
        None
    }
}
/// Recognizers may read a polite question as the same complete request. This
/// borrowed view never rewrites stored text, literal values or the input hash.
/// An unmatched prefix or punctuation is left to the usual whole-scope parser.
pub(crate) fn literal_sentence(request: &str) -> &str {
    let text = request.trim();
    let Some(body) = text.strip_suffix('?') else { return text };
    const PREFIX: &str = "Can you ";
    if body.get(..PREFIX.len()).is_some_and(|prefix| prefix.eq_ignore_ascii_case(PREFIX)) {
        body[PREFIX.len()..].trim()
    } else {
        text
    }
}

#[cfg(test)]
pub(crate) fn literal_request(request: &str, model: &str) -> Option<ParsedRequest> {
    crate::compound_request::parse(request, model).or_else(|| atomic_literal_request(request, model))
}

#[cfg(test)]
pub(crate) fn atomic_literal_request(request: &str, model: &str) -> Option<ParsedRequest> {
    named_source_fields::parse(request, model)
        .or_else(|| broad_information::parse(request, model))
        .or_else(|| company_lookup::parse(request, model))
        .or_else(|| crate::tenderned::parse(request, model))
        .or_else(|| crate::company_request::parse(request, model))
        .or_else(|| crate::company_name_request::parse(request, model))
        .or_else(|| crate::profession_request::parse(request, model))
        .or_else(|| crate::cbs_comparison::canonical_request(request))
}

/// Historical catalog fixtures construct their expected intake directly.
/// Production intake never calls this helper or skips model interpretation.
#[cfg(test)]
pub(crate) fn offline_fixture_intake(question: &str, catalog: &Catalog) -> Intake {
    let parsed_request = literal_request(question, "offline-fixture")
        .expect("the historical fixture must have an explicit intake");
    let matches = crate::ask_v2::search::search(&parsed_request, catalog);
    Intake {
        selected_capabilities: matches.iter().flat_map(|found| found.operations.iter().cloned()).collect(),
        parsed_request,
        matches,
        apiosk_sources_searched: source_count(catalog),
        catalog_version: catalog.version.clone(),
    }
}

pub fn normalize(raw: &str, request: &str, model: &str) -> anyhow::Result<ParsedRequest> {
    normalize_with_context(raw, request, request, model)
}

pub(crate) fn normalize_with_context(raw: &str, request: &str, literals: &str, model: &str) -> anyhow::Result<ParsedRequest> {
    // Validate what the model returned. Recognizing the question text must
    // never replace a model response or turn invalid output into a valid one.
    let raw = extract_json(raw)?;
    let language = text(&raw["language"]).to_lowercase();
    ensure!(
        language.len() == 2 && language.bytes().all(|c| c.is_ascii_lowercase()),
        "language must be a two-letter code"
    );
    let subjects = raw["subjects"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("subjects must be an array"))?;
    ensure!(subjects.len() <= 8, "too many subjects");
    let mut ids = BTreeMap::new();
    let mut normalized_subjects = Vec::new();
    for subject in subjects {
        ensure!(subject.is_object(), "subject must be an object");
        let label = text(&subject["label"]);
        let kind = snake(&text(&subject["type"]));
        ensure!(
            !label.is_empty() && label.len() <= 500 && literal(literals, &label) && !kind.is_empty(),
            "subject must be literal and have a type"
        );
        let identifiers = subject["identifiers"]
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("identifiers must be an object"))?;
        let identifiers = identifiers
            .iter()
            .filter_map(|(key, value)| {
                let value = value
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| value.as_number().map(ToString::to_string))?;
                (literal(literals, &value) && !snake(key).is_empty()).then(|| (snake(key), value))
            })
            .collect();
        let id = format!("s{}", normalized_subjects.len() + 1);
        let old = text(&subject["id"]);
        ensure!(
            !old.is_empty() && !ids.contains_key(&old),
            "subject ids must be unique"
        );
        ids.insert(old, id.clone());
        normalized_subjects.push(Subject {
            id,
            label,
            kind,
            identifiers,
        });
    }
    let capabilities = raw["capabilities"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("capabilities must be an array"))?;
    ensure!(
        !capabilities.is_empty() && capabilities.len() <= 16,
        "return one to sixteen capabilities"
    );
    let mut normalized = Vec::new();
    for cap in capabilities {
        ensure!(cap.is_object(), "capability must be an object");
        let name = text(&cap["name"]);
        let description = text(&cap["description"]);
        ensure!(
            !name.is_empty()
                && name.len() <= 120
                && !description.is_empty()
                && description.len() <= 1000,
            "capability needs a generic name and description"
        );
        let (slug, domain) = normalize_slug(&text(&cap["slug"]), &text(&cap["domain"]))
            .ok_or_else(|| anyhow::anyhow!("unusable capability slug"))?;
        ensure!(slug.len() <= 200, "capability slug too long");
        let confidence = cap["confidence"]
            .as_f64()
            .or_else(|| cap["confidence"].as_str().and_then(|s| s.parse().ok()))
            .ok_or_else(|| anyhow::anyhow!("confidence must be a number"))?;
        let confidence = if confidence > 1.0 && confidence <= 100.0 {
            confidence / 100.0
        } else {
            confidence
        };
        let mut source = text(&cap["source_hint"]).to_lowercase();
        if !literal(literals, &source) {
            let phrase = source.replace('_', " ");
            if literal(literals, &phrase) { source = phrase; }
        }
        // An invalid link is a model error, not a subjectless request. Dropping
        // the link would also detach that subject's supplied selection filters.
        let subject_id = match &cap["subject_id"] {
            Value::Null => None,
            Value::String(id) if ids.contains_key(id) => ids.get(id).cloned(),
            _ => anyhow::bail!("capability subject_id must reference a supplied subject or be null"),
        };
        let mut capability = Capability {
            slug,
            name,
            description,
            domain,
            inputs: names(&cap["inputs"])?,
            outputs: names(&cap["outputs"])?,
            subject_id,
            source_hint: literal(literals, &source).then_some(source),
            confidence: confidence.clamp(0.0, 1.0),
            missing: Vec::new(),
        };
        capability.missing = missing_inputs(&capability, &normalized_subjects);
        if !answer_instruction(&capability) {
            let ungrounded: Vec<_> = capability.missing.iter().filter(|input| !identifier_input(input)).collect();
            ensure!(ungrounded.is_empty(),
                "input concepts require supplied identifiers or the literal subject: {}. Correct the input concepts and identifier keys while preserving all requested criteria and outputs; do not invent missing provider parameters",
                ungrounded.into_iter().cloned().collect::<Vec<_>>().join(", "));
        }
        if let Some(existing) = normalized.iter_mut().find(|c: &&mut Capability| {
            c.slug == capability.slug && c.subject_id == capability.subject_id
                && c.source_hint == capability.source_hint
        }) {
            // Models can split fields of one requested record across several
            // entries. Consolidate that record without losing later fields or
            // changing its subject/source scope.
            for input in capability.inputs {
                if !existing.inputs.contains(&input) { existing.inputs.push(input); }
            }
            for output in capability.outputs {
                if !existing.outputs.contains(&output) { existing.outputs.push(output); }
            }
            ensure!(existing.inputs.len() <= 32 && existing.outputs.len() <= 32,
                "too many input or output concepts for one capability");
            if existing.description != capability.description {
                existing.description = format!("{} {}", existing.description, capability.description);
                ensure!(existing.description.len() <= 1000, "combined capability description is too long");
            }
            existing.confidence = existing.confidence.min(capability.confidence);
            existing.missing = missing_inputs(existing, &normalized_subjects);
        } else {
            normalized.push(capability);
        }
    }
    let deliverable = &raw["deliverable"];
    ensure!(deliverable.is_object(), "deliverable must be an object");
    let format = text(&deliverable["format"]).to_lowercase();
    let format = match format.as_str() {
        "excel" => "xlsx",
        "word" => "docx",
        f if FORMATS.contains(&f) => f,
        _ => "chat",
    };
    // A report or dossier does not imply a downloadable file. Keep an explicit
    // format from earlier user turns, but never turn the model's preference
    // into a file-format promise the person did not make.
    let aliases: &[&str] = match format {
        "pdf" => &["pdf", "pdfbestand", "pdfdocument"],
        "xlsx" => &["xlsx", "excel", "excelbestand", "spreadsheet", "spreadsheets"],
        "docx" => &["docx", "word", "wordbestand", "worddocument"],
        "json" => &["json"],
        _ => &[],
    };
    let explicit = literals.to_lowercase().split(|ch: char| !ch.is_alphanumeric())
        .any(|word| aliases.contains(&word));
    let format = if format == "chat" || explicit { format } else { "chat" }.to_owned();
    let operations: Vec<String> = deliverable["operations"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("operations must be an array"))?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_lowercase)
        .filter(|s| OPERATIONS.contains(&s.as_str()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    // A clarified comparison is an operation on the requested source data.
    // Some models retain the previous turn's generic analysis capability as
    // well. Preserve the action in deliverable.operations, without demanding
    // a separate paid source for that same analysis. Standalone ambiguity and
    // all other (including unsupported) retrieval capabilities remain intact.
    if operations.iter().any(|op| matches!(op.as_str(), "analyze" | "compare"))
        && normalized.iter().any(|cap| cap.domain != "general")
    {
        normalized.retain(|cap| cap.slug != "general.analysis");
    }
    Ok(ParsedRequest {
        language,
        subjects: normalized_subjects,
        capabilities: normalized,
        deliverable: Deliverable { format, operations },
        meta: Meta {
            model: model.into(),
            parsed_at: chrono::Utc::now().to_rfc3339(),
            text_sha256: hex::encode(Sha256::digest(request.as_bytes())),
        },
    })
}

fn identifier_input(input: &str) -> bool {
    ["_number", "_id", "_code"].iter().any(|suffix| input.ends_with(suffix))
        || ["iban", "postcode", "zip_code", "license_plate", "bsn", "ean", "isbn"].contains(&input)
}

/// Free-text search concepts are the person's own words, which every request
/// supplies. On a capability without a subject, a model naming one ("query",
/// "topic") has not invented a missing provider parameter, so it must not fail
/// the interpretation. A subject-scoped capability keeps the stricter rule (a
/// company label does not imply a topic), and invented criteria such as a
/// record type, service or period are still corrected.
fn question_input(input: &str) -> bool {
    matches!(input, "query" | "search_query" | "search_term" | "search_terms" | "keyword" | "keywords"
        | "topic" | "question" | "prompt")
}

fn supplied_input(input: &str, capability: &Capability, subject: &Subject) -> bool {
    if subject.identifiers.get(input).is_some_and(|value| !value.is_empty()) { return true; }
    if identifier_input(input) { return false; }
    let kind = subject.kind.as_str();
    if input == kind || input == format!("{kind}_name") { return true; }
    let company = matches!(kind, "company" | "organization" | "organisation" | "business");
    if company && matches!(input, "name" | "company_name" | "organization_name" | "organisation_name" | "business_name") { return true; }
    if matches!(kind, "person" | "individual") && matches!(input, "name" | "person_name") { return true; }
    if matches!(kind, "topic" | "query" | "keyword") && matches!(input, "topic" | "query" | "keyword" | "search_term") { return true; }
    // A search term, sector or service the person typed is the free-text
    // query itself ("notices for \"schoonmaak\""). Only a named company or
    // person does not imply an unspecified topic.
    if question_input(input) && !company && !matches!(kind, "person" | "individual") { return true; }
    if matches!(kind, "address" | "location" | "place" | "property" | "building") && matches!(input, "address" | "location") { return true; }
    if input == "url" && reqwest::Url::parse(&subject.label).is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some()) { return true; }
    matches!(input, "source" | "source_hint" | "provider") && capability.source_hint.is_some()
}

/// Describe only supplied criteria, scoped to the capability's actual subject.
/// Missing identifiers remain visible for supported execution dependencies;
/// other absent concepts are invalid model extraction and get one correction.
fn missing_inputs(capability: &Capability, subjects: &[Subject]) -> Vec<String> {
    capability.inputs.iter().filter(|input| !(capability.subject_id.is_none() && question_input(input)) && !subjects.iter()
        .filter(|subject| capability.subject_id.as_ref() == Some(&subject.id))
        .any(|subject| supplied_input(input, capability, subject))).cloned().collect()
}

/// Same generic query fields as WebGPU. The external query is also scrubbed
/// against literals in the request/context so model mistakes do not copy them.
pub fn capability_query(cap: &Capability, parsed: &ParsedRequest, task: &Task) -> String {
    let mut query = [
        cap.name.clone(),
        cap.description.clone(),
        cap.inputs.join(" "),
        cap.source_hint.clone().unwrap_or_default(),
    ]
    .into_iter()
    .map(|s| s.trim_end_matches(['.', ' ']).to_owned())
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(". ")
    .replace('_', " ");
    let mut private: Vec<String> = parsed
        .subjects
        .iter()
        .flat_map(|s| std::iter::once(s.label.clone()).chain(s.identifiers.values().cloned()))
        .chain(task.entities.iter().map(|e| e.label.clone()))
        .chain(
            task.facts
                .iter()
                .filter_map(|f| f.value.as_str().map(str::to_owned)),
        )
        .collect();
    private.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for value in private {
        if value.is_empty() {
            continue;
        }
        // Use character indices from the original string, including Unicode.
        let lowered = value.to_lowercase();
        let chars: Vec<_> = query
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(query.len()))
            .collect();
        let mut ranges = Vec::new();
        for (position, &start) in chars.iter().enumerate().take(chars.len().saturating_sub(1)) {
            for &end in chars
                .iter()
                .skip(position + 1)
                .take(value.chars().count() + 2)
            {
                if query[start..end].to_lowercase() == lowered {
                    ranges.push((start, end));
                    break;
                }
            }
        }
        let mut before = query.len();
        for (start, end) in ranges.into_iter().rev() {
            if end <= before {
                query.replace_range(start..end, "");
                before = start;
            }
        }
    }
    query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(300)
        .collect()
}

pub fn queries(parsed: &ParsedRequest, task: &Task) -> BTreeMap<String, String> {
    parsed
        .capabilities
        .iter()
        .filter(|c| !answer_instruction(c))
        .map(|c| (c.slug.clone(), capability_query(c, parsed, task)))
        .collect()
}
pub fn source_count(catalog: &Catalog) -> usize {
    catalog
        .known_sources
        .iter()
        .map(|s| {
            match s.metadata.provider_slug.as_deref().and_then(crate::sources::grouped_provider) {
                Some(group) => group.handle,
                None => &s.slug,
            }
        })
        .collect::<BTreeSet<_>>()
        .len()
}
/// Whether `service` is the source the person named (a lowercase
/// `source_hint`). Discovery ranks it first and the planner holds the plan to
/// it, so the source listed first is the source that is quoted.
pub fn names_source(service: &Service, hint: &str) -> bool {
    format!("{} {}", service.api_slug, service.name)
        .to_lowercase()
        .contains(hint)
}
pub fn local_matches(
    parsed: &ParsedRequest,
    query: &BTreeMap<String, String>,
    selected: &BTreeMap<String, BTreeSet<String>>,
    catalog: &Catalog,
) -> Vec<Match> {
    query
        .iter()
        .map(|(slug, query)| {
            let selected = &selected[slug];
            let hint = parsed
                .capabilities
                .iter()
                .find(|c| &c.slug == slug)
                .and_then(|c| c.source_hint.as_deref());
            let mut services: Vec<_> = catalog
                .services
                .iter()
                .filter(|s| selected.contains(&s.contract.capability_slug))
                .collect();
            services.sort_by_key(|s| {
                let named = hint.is_some_and(|hint| names_source(s, hint));
                (!named, s.unverified, s.total(), s.api_slug.clone(), s.id)
            });
            let mut seen = BTreeSet::new();
            let apiosk = services
                .into_iter()
                .filter(|s| seen.insert(s.endpoint_id))
                .take(3)
                .map(|s| Candidate {
                    origin: "apiosk".into(),
                    name: s.name.clone(),
                    description: s
                        .description
                        .clone()
                        .unwrap_or_else(|| s.contract.capability_slug.clone()),
                    source_slug: Some(s.api_slug.clone()),
                    capability: Some(s.contract.capability_slug.clone()),
                    endpoint_id: Some(s.endpoint_id.to_string()),
                    url: s.source_url.as_deref().and_then(safe_url),
                    method: s.method.clone(),
                    price: Some(format!("{:.6} USD", s.total() as f64 / 1_000_000.0)),
                    network: None,
                    calls_30d: None,
                    availability: if s.unverified {
                        "unverified"
                    } else {
                        "supported"
                    }
                    .into(),
                })
                .collect();
            Match {
                slug: slug.clone(),
                query: query.clone(),
                operations: selected.clone(),
                apiosk,
                coinbase: vec![],
                coinbase_status: "pending".into(),
            }
        })
        .collect()
}
fn safe_url(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    (value.len() <= 2048
        && matches!(url.scheme(), "https" | "http")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none())
    .then(|| url.to_string())
}
fn coinbase_candidates(body: Value) -> anyhow::Result<Vec<Candidate>> {
    let resources = body["resources"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("missing Bazaar resources"))?;
    let mut seen = BTreeSet::new();
    Ok(resources
        .iter()
        .filter(|r| r["type"].as_str().unwrap_or("http") == "http")
        .filter_map(|r| {
            let url = safe_url(r["resource"].as_str()?)?;
            if !seen.insert(url.clone()) {
                return None;
            }
            let accepts = r["accepts"].as_array();
            let cheapest = accepts
                .into_iter()
                .flatten()
                .filter_map(|a| {
                    if a.pointer("/extra/name").and_then(Value::as_str) != Some("USD Coin") {
                        return None;
                    }
                    let amount = a.get("amount").or_else(|| a.get("maxAmountRequired"))?;
                    let amount = amount
                        .as_str()
                        .and_then(|s| s.parse::<u64>().ok())
                        .or_else(|| amount.as_u64())?;
                    Some((amount, a))
                })
                .min_by_key(|(amount, _)| *amount);
            let name = r["serviceName"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    url::Url::parse(&url)
                        .unwrap()
                        .host_str()
                        .unwrap()
                        .to_owned()
                });
            Some(Candidate {
                origin: "coinbase".into(),
                name: name.chars().take(160).collect(),
                description: r["description"]
                    .as_str()
                    .unwrap_or("")
                    .trim()
                    .chars()
                    .take(1000)
                    .collect(),
                source_slug: None,
                capability: None,
                endpoint_id: None,
                url: Some(url),
                method: r
                    .pointer("/extensions/bazaar/info/input/method")
                    .and_then(Value::as_str)
                    .unwrap_or("GET")
                    .chars()
                    .take(12)
                    .collect(),
                price: cheapest
                    .map(|(amount, _)| format!("{:.6} USD", amount as f64 / 1_000_000.0)),
                network: cheapest
                    .and_then(|(_, a)| a["network"].as_str())
                    .map(str::to_owned),
                calls_30d: r
                    .pointer("/quality/l30DaysTotalCalls")
                    .and_then(Value::as_u64),
                availability: "discovery_only".into(),
            })
        })
        .take(3)
        .collect())
}
pub async fn fetch_coinbase(
    http: &reqwest::Client,
    search_url: &str,
    query: &str,
) -> anyhow::Result<Vec<Candidate>> {
    discovery_cache::fetch(http, search_url, query).await
}

async fn fetch_coinbase_uncached(
    http: &reqwest::Client,
    search_url: &str,
    query: &str,
) -> anyhow::Result<Vec<Candidate>> {
    ensure!(!query.trim().is_empty(), "empty discovery query");
    let response = http
        .get(search_url)
        .query(&[("query", query), ("limit", "3")])
        .timeout(Duration::from_secs(4))
        .send()
        .await?
        .error_for_status()?;
    let mut chunks = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk?;
        ensure!(
            bytes.len() + chunk.len() <= 256_000,
            "Bazaar response too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    coinbase_candidates(serde_json::from_slice(&bytes)?)
}
pub async fn add_coinbase(http: &reqwest::Client, matches: &mut [Match], search_url: &str) {
    // All requests start AFTER local discovery and never target resource URLs.
    let jobs: Vec<_> = matches
        .iter()
        .enumerate()
        .map(|(index, found)| {
            let query = found.query.clone();
            let http = http.clone();
            let url = search_url.to_owned();
            async move { (index, fetch_coinbase(&http, &url, &query).await) }
        })
        .collect();
    let results: Vec<_> = stream::iter(jobs).buffer_unordered(4).collect().await;
    for (index, result) in results {
        match result {
            Ok(candidates) => {
                matches[index].coinbase = candidates;
                matches[index].coinbase_status = "complete".into();
            }
            Err(_) => {
                matches[index].coinbase_status = "unavailable".into();
            }
        }
    }
}

pub fn search_input(parsed: &ParsedRequest, task: &Task, catalog: &Catalog) -> Value {
    let queries = queries(parsed, task);
    // Score EVERY registered operation before selecting the compact semantic
    // search input. Large providers must not crowd the model context with
    // hundreds of unrelated operations. This is independent of source count.
    let words = |text: &str| -> BTreeSet<String> {
        let mut tokens: BTreeSet<_> = text
            .to_lowercase()
            .split(|ch: char| !ch.is_alphanumeric())
            .filter(|word| {
                word.len() >= 3
                    && ![
                        "the",
                        "for",
                        "and",
                        "from",
                        "with",
                        "retrieve",
                        "provides",
                        "data",
                        "information",
                        "company",
                        "general",
                        "search",
                        "find",
                        "get",
                        "list",
                        "lookup",
                        "query",
                        "api",
                    ]
                    .contains(word)
            })
            .map(|word| word.trim_end_matches('s').to_owned())
            .collect();
        // High-recall terminology only; the semantic pass decides whether a
        // financial statement, bank account, filing or summary actually fits.
        if tokens
            .iter()
            .any(|w| ["financial", "statement", "jaarrekening"].contains(&w.as_str()))
        {
            tokens.insert("account".into());
        }
        tokens
    };
    let docs: Vec<_> = catalog
        .services
        .iter()
        .map(|s| {
            words(&format!(
                "{} {} {} {} {} {}",
                s.contract.capability_slug,
                s.api_slug,
                s.name,
                s.description.as_deref().unwrap_or(""),
                s.contract
                    .input_map
                    .values()
                    .map(|b| b.name.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                s.contract
                    .output_map
                    .produces
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" ")
            ))
        })
        .collect();
    let mut frequency: BTreeMap<&str, usize> = BTreeMap::new();
    for doc in &docs {
        for word in doc {
            *frequency.entry(word).or_default() += 1;
        }
    }
    let mut chosen = BTreeSet::new();
    for (slug, query) in &queries {
        let wanted = words(query);
        let mut ranked: Vec<_> = catalog
            .services
            .iter()
            .zip(&docs)
            .map(|(service, doc)| {
                let exact = normalize_slug(&service.contract.capability_slug, "")
                    .is_some_and(|(normalized, _)| &normalized == slug);
                let score = wanted
                    .intersection(doc)
                    .map(|word| {
                        ((docs.len() + 1) as f64
                            / (*frequency.get(word.as_str()).unwrap_or(&0) + 1) as f64)
                            .ln()
                            + 1.0
                    })
                    .sum::<f64>()
                    + if exact { 100.0 } else { 0.0 };
                (score, service)
            })
            .filter(|(score, _)| *score > 0.0)
            .collect();
        ranked.sort_by(|a, b| {
            b.0.total_cmp(&a.0)
                .then(a.1.unverified.cmp(&b.1.unverified))
                .then(a.1.id.cmp(&b.1.id))
        });
        for (_, service) in ranked.into_iter().take(24) {
            chosen.insert(service.contract.capability_slug.clone());
        }
    }
    let mut operations = catalog.operation_index();
    operations
        .as_object_mut()
        .unwrap()
        .retain(|slug, _| chosen.contains(slug));
    for variants in operations
        .as_object_mut()
        .unwrap()
        .values_mut()
        .filter_map(Value::as_array_mut)
    {
        for variant in variants {
            if let Some(description) = variant["description"].as_str() {
                variant["description"] = json!(description.chars().take(320).collect::<String>());
            }
        }
    }
    json!({"queries":queries,"operations":operations,
        "workflow_presets":catalog.parser_view()["workflow_presets"],
        "sources":catalog.known_sources.iter().map(|s| json!({"slug":s.slug,"name":s.name,"description":s.metadata.description.chars().take(160).collect::<String>(),
            "provider":s.metadata.provider_slug,"category":s.metadata.category,"block_reason":s.block_reason})).collect::<Vec<_>>()})
}
/// The local scan considers the entire directory. The model only needs the
/// shortlisted contracts, each with a short id it can copy without inventing
/// a provider name or reconstructing an opaque operation slug.
pub fn search_model_input(index: &Value) -> Value {
    let mut candidates = serde_json::Map::new();
    if let Some(operations) = index["operations"].as_object() {
        for (capability, offers) in operations {
            let id = format!("c{}", candidates.len() + 1);
            candidates.insert(id, json!({"capability":capability,"offers":offers}));
        }
    }
    for preset in index["workflow_presets"].as_array().into_iter().flatten() {
        let id = format!("c{}", candidates.len() + 1);
        candidates.insert(
            id,
            json!({"capability":preset["capability"],"description":preset["description"]}),
        );
    }
    json!({"queries":index["queries"],"candidates":candidates})
}

pub fn search_reply_schema(input: &Value) -> Value {
    let slugs: Vec<_> = input["queries"]
        .as_object()
        .into_iter()
        .flat_map(|q| q.keys())
        .collect();
    let ids: Vec<_> = input["candidates"]
        .as_object()
        .into_iter()
        .flat_map(|c| c.keys())
        .collect();
    json!({"type":"object","additionalProperties":false,"required":["matches"],"properties":{
        "matches":{"type":"array","minItems":slugs.len(),"maxItems":slugs.len(),"items":{
            "type":"object","additionalProperties":false,"required":["slug","candidate_ids"],"properties":{
                "slug":{"type":"string","enum":slugs},
                "candidate_ids":{"type":"array","maxItems":32,"items":{"type":"string","enum":ids}}}}}}})
}

pub fn decode_matches(
    raw: &str,
    input: &Value,
) -> anyhow::Result<BTreeMap<String, BTreeSet<String>>> {
    let raw = extract_json(raw)?;
    let items = raw["matches"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("missing capability matches"))?;
    let mut matches = BTreeMap::new();
    let mut all = BTreeSet::new();
    for item in items {
        let slug = item["slug"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing parsed slug"))?;
        ensure!(
            input["queries"].get(slug).is_some() && !matches.contains_key(slug),
            "unknown or duplicate parsed capability"
        );
        let mut selected = BTreeSet::new();
        for value in item["candidate_ids"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("matches must be an array"))?
        {
            let id = value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("invalid candidate id"))?;
            let cap = input["candidates"]
                .get(id)
                .and_then(|c| c["capability"].as_str())
                .ok_or_else(|| anyhow::anyhow!("unknown candidate id"))?;
            selected.insert(cap.to_owned());
            all.insert(cap.to_owned());
        }
        matches.insert(slug.to_owned(), selected);
    }
    ensure!(
        all.len() <= 32 && matches.len() == input["queries"].as_object().map_or(0, |q| q.len()),
        "incomplete capability matching"
    );
    Ok(matches)
}

#[cfg(test)]
mod model_normalization_regressions {
    use super::*;

    #[test]
    fn known_questions_and_greetings_cannot_bypass_model_output_validation() {
        for request in [
            "Search Crunchbase for Starbucks and show its company description and profile link.",
            "Find public tenders for cleaning services in the Netherlands through TenderNed.",
            "Show the registered company details for Mollie B.V. (KVK 30204462).",
            "hoe gaat het met jou?",
        ] {
            assert!(normalize("invalid output", request, "fixture-model").is_err(), "{request}");
        }
    }

    #[test]
    fn greeting_normalization_preserves_model_fields_instead_of_a_canned_parse() {
        let raw = json!({"language":"nl","subjects":[],"capabilities":[{
            "slug":"general.conversation","name":"Reply to the user","description":"Respond to the greeting in the requested tone.",
            "domain":"general","inputs":[],"outputs":["reply","tone"],"subject_id":null,"source_hint":null,"confidence":0.8
        }],"deliverable":{"format":"chat","operations":[]}});
        let parsed = normalize(&raw.to_string(), "hoe gaat het met jou?", "fixture-model").unwrap();
        assert_eq!(parsed.capabilities[0].outputs, ["reply", "tone"]);
        assert_eq!(parsed.capabilities[0].name, "Reply to the user");
        assert_eq!(parsed.capabilities[0].confidence, 0.8);
        assert_eq!(parsed.meta.model, "fixture-model");
    }

    #[test]
    fn invalid_subject_links_cannot_detach_supplied_filters() {
        let question = "Show the company profile for ExampleCo in Seattle.";
        let mut raw = json!({"language":"en","subjects":[{
            "id":"company","label":"ExampleCo","type":"company",
            "identifiers":{"company_name":"ExampleCo","location":"Seattle"}
        }],"capabilities":[{
            "slug":"company.profile","name":"Company profile","description":"Retrieve a company profile.",
            "domain":"company","inputs":[],"outputs":["profile"],
            "subject_id":"company","source_hint":null,"confidence":0.95
        }],"deliverable":{"format":"chat","operations":[]}});
        let linked = normalize(&raw.to_string(), question, "fixture-model").unwrap();
        assert_eq!(linked.capabilities[0].subject_id.as_deref(), Some("s1"));
        assert_eq!(linked.subjects[0].identifiers["location"], "Seattle");
        // Even when a model omits input names, a bad subject reference must
        // trigger the existing correction attempt instead of losing Seattle.
        for invalid in [json!("missing-subject"), json!("s1"), json!(""), json!(7), json!([])] {
            raw["capabilities"][0]["subject_id"] = invalid.clone();
            let error = normalize(&raw.to_string(), question, "fixture-model").unwrap_err();
            assert_eq!(error.to_string(), "capability subject_id must reference a supplied subject or be null", "{invalid}");
        }
        raw["capabilities"][0]["subject_id"] = Value::Null;
        raw["subjects"] = json!([]);
        assert!(normalize(&raw.to_string(), question, "fixture-model").unwrap().capabilities[0].subject_id.is_none(),
            "an explicitly subjectless request remains supported");
    }
}
