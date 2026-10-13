//! Question-directed evidence selection for the analysis model (#39).
//!
//! The model reads a projection of the saved documents; every citation is
//! validated against the originals. A projection above the input budget keeps,
//! in its largest collections, the entries most relevant to the question,
//! chosen across the whole collection and never "the first rows". A reduced
//! array becomes an object keyed by the original index, so a pointer such as
//! `/data/rows/4321/value` is the same in the projection and in the saved
//! document. A note on each reduced document states, per collection, how many
//! entries were saved, matched the question and were kept; totals, coverage
//! and every other field stay exactly as saved.
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Serialized analysis input: operations, requested parts, sources and unavailable steps.
pub(crate) const ANALYSIS_INPUT_BYTES: usize = 256 * 1024;
/// A text field above this size keeps one contiguous excerpt when reducing
/// collections alone does not fit the budget. Any quote from it is verbatim.
const EXCERPT_BYTES: usize = 4 * 1024;
/// Objects are reduced only as maps with at least this many entries, after
/// every array: dropping fields from an ordinary record loses its context.
const MAP_ENTRIES: usize = 16;
const MAX_REDUCTIONS: usize = 64;
const MAX_TERMS: usize = 32;
/// Fixed server text; never model or source text.
const NOTE: &str = "This saved source is larger than the analysis input. The entries most relevant to the question were kept from across each listed collection; the others are omitted here but remain in the saved source. A reduced array is shown as an object keyed by the original zero-based index: cite it with that index, for example /data/rows/12/name. Counts are exact: fetched entries were saved, matched entries mention a question term, kept entries are shown. Kept entries are a selection, never the complete set; use the saved totals for coverage.";

/// Words that carry no selection signal, including the fixed wording of the
/// analysis operations.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "from", "this", "that", "what", "which", "who", "how", "are", "was", "were", "has", "have",
    "show", "give", "list", "find", "all", "any", "only", "same", "using", "answer", "question", "directly", "saved",
    "source", "sources", "fields", "field", "language", "summarize", "requested", "facts", "concisely", "exact",
    "evidence", "values", "een", "het", "van", "voor", "met", "wat", "welke", "wie", "hoe", "zijn", "toon", "geef",
    "alle", "over", "naar", "bij", "uit", "die", "dat", "deze", "niet", "ook", "nog", "als", "dan", "per",
];

/// The documents the model reads for `operations`, within `budget` bytes of
/// serialized sources where possible. Documents that fit are only stripped of
/// embedded file bytes and map geometry.
pub(crate) fn project(sources: &[Value], operations: &Value, budget: usize) -> Vec<Value> {
    let mut projected: Vec<Value> = sources
        .iter()
        .map(|source| {
            let mut source = source.clone();
            source["document"] = public_view(&source["document"]);
            source
        })
        .collect();
    let mut size = serialized_len(&projected);
    if size <= budget {
        return projected;
    }
    let terms = question_terms(operations);
    let mut notes: Vec<Note> = projected.iter().map(|_| Note::default()).collect();
    // Every outermost array shrinks by the same factor, so several sources
    // share the budget instead of the largest one losing everything.
    let outermost = outermost_arrays(&projected);
    let reducible: usize = outermost.iter().map(|(_, _, length)| length).sum();
    let note_bytes = (NOTE.len() + 128) * projected.len() + 96 * outermost.len();
    let room = budget.saturating_sub(size - reducible + note_bytes);
    for (source, pointer, length) in outermost {
        let keep = (length as u128 * room as u128 / reducible.max(1) as u128) as usize;
        notes[source].shrink(&mut projected[source]["document"], pointer, &terms, Target::Keep(keep));
    }
    size = serialized_len(&projected);
    // Then the largest collection first, nested and already reduced ones included.
    for _ in 0..MAX_REDUCTIONS {
        if size <= budget {
            break;
        }
        let Some((source, pointer)) = largest_collection(&projected, &notes) else { break };
        notes[source].shrink(&mut projected[source]["document"], pointer, &terms, Target::Shrink(size - budget));
        size = serialized_len(&projected);
    }
    for (source, pointer, length) in long_texts(&projected) {
        if size <= budget {
            break;
        }
        notes[source].excerpt(&mut projected[source]["document"], pointer, length, &terms);
        size = serialized_len(&projected);
    }
    projected
}

/// Embedded file bytes are never analysis input, and map geometry dominates
/// GeoJSON size without contributing evidence. Property paths are unchanged.
fn public_view(document: &Value) -> Value {
    let mut document = crate::report_assets::public_document(document);
    if let Some(features) = document.pointer_mut("/data/features").and_then(Value::as_array_mut) {
        for feature in features {
            if let Some(object) = feature.as_object_mut() {
                object.remove("geometry");
            }
        }
    }
    document
}

#[derive(Clone, Copy)]
struct Counts {
    fetched: usize,
    matched: usize,
    kept: usize,
}

/// What was reduced in one document, by pointer.
#[derive(Default)]
struct Note {
    collections: BTreeMap<String, Counts>,
    excerpts: Vec<Value>,
}

/// The size of a reduced collection's kept entries, in their keyed form.
enum Target {
    Keep(usize),
    Shrink(usize),
}

impl Note {
    /// Reduces the collection at `pointer`. Reducing it again keeps the
    /// original counts and index spread.
    fn shrink(&mut self, document: &mut Value, pointer: String, terms: &[String], target: Target) {
        let collection = document.pointer_mut(&pointer).expect("collection found by walk");
        let previous = self.collections.get(&pointer).copied();
        let counts = reduce(collection, terms, target, previous.map(|counts| counts.fetched));
        let original = previous.unwrap_or(counts);
        self.collections.insert(pointer, Counts { kept: counts.kept, ..original });
        self.attach(document);
    }

    fn excerpt(&mut self, document: &mut Value, pointer: String, length: usize, terms: &[String]) {
        let field = document.pointer_mut(&pointer).expect("text found by walk");
        *field = json!(excerpt(field.as_str().unwrap_or_default(), terms));
        let kept = field.as_str().map_or(0, str::len);
        self.excerpts.push(json!({"pointer": pointer, "bytes": length, "kept_bytes": kept}));
        self.attach(document);
    }

    /// The note is part of the document, so it counts toward the budget.
    fn attach(&self, document: &mut Value) {
        let collections: Vec<Value> = self.collections.iter()
            .map(|(pointer, counts)| json!({"pointer": pointer, "fetched": counts.fetched, "matched": counts.matched, "kept": counts.kept}))
            .collect();
        document["evidence_selection"] = json!({"note": NOTE, "collections": collections, "excerpts": self.excerpts});
    }
}

fn serialized_len(value: &impl serde::Serialize) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}

fn escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// Every value below `root` with its pointer, depth first.
fn walk<'a>(root: &'a Value, pointer: &str, visit: &mut impl FnMut(&str, &'a Value) -> bool) {
    if !visit(pointer, root) {
        return;
    }
    match root {
        Value::Array(items) => items.iter().enumerate().for_each(|(index, item)| walk(item, &format!("{pointer}/{index}"), visit)),
        Value::Object(fields) => fields.iter().for_each(|(key, item)| walk(item, &format!("{pointer}/{}", escape(key)), visit)),
        _ => {}
    }
}

fn tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric()).filter(|token| !token.is_empty()).map(str::to_lowercase)
}

/// Distinct content words and numbers of the operations, in order.
fn question_terms(operations: &Value) -> Vec<String> {
    let mut texts = Vec::new();
    scalars(operations, &mut texts, usize::MAX);
    let mut seen = BTreeSet::new();
    texts
        .iter()
        .flat_map(|text| tokens(text).collect::<Vec<_>>())
        .filter(|token| {
            let numeric = token.chars().any(|c| c.is_ascii_digit());
            (token.chars().count() >= 3 || (numeric && token.len() >= 2)) && !STOPWORDS.contains(&token.as_str())
        })
        .filter(|token| seen.insert(token.clone()))
        .take(MAX_TERMS)
        .collect()
}

/// Up to `limit` scalar texts of a value, depth first.
fn scalars(value: &Value, out: &mut Vec<String>, limit: usize) {
    if out.len() >= limit {
        return;
    }
    match value {
        Value::String(text) => out.push(text.clone()),
        Value::Number(number) => out.push(number.to_string()),
        Value::Array(items) => items.iter().for_each(|item| scalars(item, out, limit)),
        Value::Object(fields) => fields.values().for_each(|item| scalars(item, out, limit)),
        _ => {}
    }
}

/// Exact word matches count double; a long term also matches a word with the
/// same five-letter stem ("laadpalen", "laadpaal").
fn relevance(entry: &Value, terms: &[String]) -> usize {
    if terms.is_empty() {
        return 0;
    }
    let mut texts = Vec::new();
    scalars(entry, &mut texts, 64);
    let words: BTreeSet<String> = texts.iter().flat_map(|text| tokens(text).collect::<Vec<_>>()).collect();
    let stem = |word: &str| word.chars().take(5).collect::<String>();
    let stems: BTreeSet<String> = words.iter().filter(|word| word.chars().count() >= 5).map(|word| stem(word)).collect();
    terms
        .iter()
        .map(|term| {
            if words.contains(term) {
                2
            } else if term.chars().count() >= 6 && stems.contains(&stem(term)) {
                1
            } else {
                0
            }
        })
        .sum()
}

/// Position of `index` in an order that spreads over `0..len`: first,
/// middle, quarters, eighths and so on. Equally relevant entries are kept
/// from across the collection, never from its start.
fn spread_rank(index: usize, len: usize) -> usize {
    let bits = usize::BITS - len.saturating_sub(1).leading_zeros();
    if bits == 0 {
        return 0;
    }
    index.reverse_bits() >> (usize::BITS - bits)
}

/// Arrays with at least two entries under `/data`, not nested in another.
fn outermost_arrays(projected: &[Value]) -> Vec<(usize, String, usize)> {
    let mut found = Vec::new();
    for (source, item) in projected.iter().enumerate() {
        walk(&item["document"]["data"], "/data", &mut |pointer, value| match value {
            Value::Array(items) if items.len() >= 2 => {
                found.push((source, pointer.to_owned(), serialized_len(value)));
                false
            }
            _ => true,
        });
    }
    found
}

/// The largest reducible collection under `/data`: arrays and reduced arrays
/// with two or more entries first, then maps.
fn largest_collection(projected: &[Value], notes: &[Note]) -> Option<(usize, String)> {
    let mut best: Option<((bool, usize), usize, String)> = None;
    for (source, item) in projected.iter().enumerate() {
        walk(&item["document"]["data"], "/data", &mut |pointer, value| {
            let reduced = notes[source].collections.contains_key(pointer);
            let rank = match value {
                Value::Array(items) if items.len() >= 2 => Some(true),
                Value::Object(fields) if reduced && fields.len() >= 2 => Some(true),
                Value::Object(fields) if fields.len() >= MAP_ENTRIES => Some(false),
                _ => None,
            };
            if let Some(array) = rank {
                let key = (array, serialized_len(value));
                if best.as_ref().is_none_or(|(best_key, _, _)| key > *best_key) {
                    best = Some((key, source, pointer.to_owned()));
                }
            }
            true
        });
    }
    best.map(|(_, source, pointer)| (source, pointer))
}

/// Keeps the most relevant entries of an array or map within `target` where
/// possible, and at least one entry. `original_len` is the saved length of an
/// array reduced before, whose keys are its original indices.
fn reduce(collection: &mut Value, terms: &[String], target: Target, original_len: Option<usize>) -> Counts {
    let entries: Vec<(usize, String, Value)> = match collection.take() {
        Value::Array(items) => items.into_iter().enumerate().map(|(index, item)| (index, index.to_string(), item)).collect(),
        Value::Object(fields) => fields.into_iter().enumerate().map(|(position, (key, item))| (key.parse().unwrap_or(position), key, item)).collect(),
        other => {
            *collection = other;
            return Counts { fetched: 0, matched: 0, kept: 0 };
        }
    };
    let fetched = entries.len();
    let spread = original_len.unwrap_or(fetched);
    // Entry cost in the keyed form: `"key":` plus the value and a comma.
    let costs: Vec<usize> = entries.iter().map(|(_, key, item)| key.len() + 4 + serialized_len(item)).collect();
    let total: usize = costs.iter().sum();
    let target = match target {
        Target::Keep(bytes) => bytes,
        Target::Shrink(excess) => total.saturating_sub(excess),
    };
    let scores: Vec<usize> = entries.iter().map(|(_, _, item)| relevance(item, terms)).collect();
    let mut order: Vec<usize> = (0..fetched).collect();
    order.sort_by_key(|&position| (std::cmp::Reverse(scores[position]), spread_rank(entries[position].0, spread), position));
    let mut kept = BTreeSet::new();
    let mut used = 0;
    for position in order {
        if kept.is_empty() || used + costs[position] <= target {
            used += costs[position];
            kept.insert(position);
        }
    }
    let matched = scores.iter().filter(|score| **score > 0).count();
    let selected: Map<String, Value> = entries
        .into_iter()
        .enumerate()
        .filter(|(position, _)| kept.contains(position))
        .map(|(_, (_, key, item))| (key, item))
        .collect();
    *collection = Value::Object(selected);
    Counts { fetched, matched, kept: kept.len() }
}

/// Text fields above EXCERPT_BYTES in citable places, longest first.
fn long_texts(projected: &[Value]) -> Vec<(usize, String, usize)> {
    let mut found = Vec::new();
    for (source, item) in projected.iter().enumerate() {
        walk(&item["document"], "", &mut |pointer, value| {
            if let Value::String(text) = value {
                if text.len() > EXCERPT_BYTES && crate::report_assets::evidence_pointer(pointer) {
                    found.push((source, pointer.to_owned(), text.len()));
                }
            }
            true
        });
    }
    found.sort_by_key(|(_, _, length)| std::cmp::Reverse(*length));
    found
}

/// One contiguous window of at most EXCERPT_BYTES, starting shortly before
/// the first question term, or at the start without a match.
fn excerpt(text: &str, terms: &[String]) -> String {
    let lower = text.to_lowercase();
    // Lowercasing can change byte offsets; only use a hit when it cannot.
    let hit = (lower.len() == text.len())
        .then(|| terms.iter().filter_map(|term| lower.find(term.as_str())).min())
        .flatten()
        .unwrap_or(0);
    let mut start = hit.saturating_sub(EXCERPT_BYTES / 4);
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (start + EXCERPT_BYTES).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[start..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(count: usize) -> Value {
        json!({"result_ref":"saved","document":{"source":{"name":"Ledger"},"coverage":"source_response","data":{
            "total":count,"currency":"EUR",
            "rows":(0..count).map(|row| json!({"row":row,"line":format!("Balance sheet line {row:04}"),"value_eur":row * 1000,"notes":"x".repeat(40)})).collect::<Vec<_>>()}}})
    }

    #[test]
    fn documents_within_the_budget_are_unchanged_apart_from_file_bytes_and_geometry() {
        let source = json!({"result_ref":"saved","document":{"data":{"features":[{"geometry":{"type":"Point"},"properties":{"name":"A"}}]},
            "report_assets":{"assets":[{"kind":"pdf","data_base64":"AAAA","extracted_text":"text"}]}}});
        let projected = project(&[source], &json!(["Where is A?"]), ANALYSIS_INPUT_BYTES);
        let document = &projected[0]["document"];
        assert!(document.pointer("/data/features/0/geometry").is_none());
        assert_eq!(document.pointer("/data/features/0/properties/name"), Some(&json!("A")));
        assert!(document.pointer("/report_assets/assets/0/data_base64").is_none());
        assert!(document.get("evidence_selection").is_none());
    }

    #[test]
    fn a_large_array_keeps_the_question_rows_from_anywhere_with_original_pointers_and_counts() {
        let source = rows(5000);
        assert!(serialized_len(&source) > ANALYSIS_INPUT_BYTES);
        let operations = json!(["Answer this question directly using only the saved source fields, in the same language: Balance sheet line 4321"]);
        let projected = project(std::slice::from_ref(&source), &operations, ANALYSIS_INPUT_BYTES);
        assert!(serialized_len(&projected) <= ANALYSIS_INPUT_BYTES);
        let document = &projected[0]["document"];
        // The same pointer resolves in the projection and in the original.
        let pointer = "/data/rows/4321/value_eur";
        assert_eq!(document.pointer(pointer), source["document"].pointer(pointer));
        assert_eq!(document.pointer("/data/total"), Some(&json!(5000)), "totals stay as saved");
        let note = &document["evidence_selection"]["collections"][0];
        assert_eq!((note["pointer"].as_str(), note["fetched"].as_u64()), (Some("/data/rows"), Some(5000)));
        let kept = document["data"]["rows"].as_object().unwrap();
        assert_eq!(note["kept"].as_u64(), Some(kept.len() as u64));
        assert!(kept.len() > 100 && kept.len() < 5000);
        // Equally relevant rows are spread over the whole array, not its start.
        let indices: Vec<usize> = kept.keys().map(|key| key.parse().unwrap()).collect();
        assert!(indices.iter().any(|index| *index > 4500) && indices.iter().any(|index| (2000..3000).contains(index)));
        assert!(!(0..indices.len()).all(|index| indices.contains(&index)), "never the first rows");
    }

    #[test]
    fn a_rare_match_at_the_end_of_a_long_list_is_kept() {
        let source = json!({"result_ref":"saved","document":{"data":{"items":(0..3000).map(|i| json!({"title":format!("Record {i}"),
            "topic":if i == 2999 {"laadpaal Utrecht"} else {"kantoorartikelen"},"body":"y".repeat(100)})).collect::<Vec<_>>()}}});
        let projected = project(&[source], &json!(["Which publications mention laadpalen?"]), 64 * 1024);
        let document = &projected[0]["document"];
        assert!(serialized_len(&projected) <= 64 * 1024);
        assert_eq!(document.pointer("/data/items/2999/topic"), Some(&json!("laadpaal Utrecht")));
        assert_eq!(document["evidence_selection"]["collections"][0]["matched"], 1);
    }

    #[test]
    fn a_long_text_keeps_a_verbatim_window_around_the_question() {
        let text = format!("{}The subscription costs EUR twelve per seat.{}", "intro ".repeat(20_000), " outro".repeat(20_000));
        let source = json!({"result_ref":"page","document":{"data":{"text":text}}});
        let projected = project(&[source], &json!(["What does the subscription cost?"]), 32 * 1024);
        let kept = projected[0]["document"]["data"]["text"].as_str().unwrap();
        assert!(kept.len() <= EXCERPT_BYTES && text.contains(kept) && kept.contains("subscription costs"));
        assert_eq!(projected[0]["document"]["evidence_selection"]["excerpts"][0]["pointer"], "/data/text");
    }

    #[test]
    fn several_sources_share_one_budget() {
        let sources = vec![rows(2500), rows(2500)];
        let projected = project(&sources, &json!(["Balance sheet line 0042"]), ANALYSIS_INPUT_BYTES);
        assert!(serialized_len(&projected) <= ANALYSIS_INPUT_BYTES);
        for document in projected.iter().map(|source| &source["document"]) {
            assert_eq!(document.pointer("/data/rows/42/line"), Some(&json!("Balance sheet line 0042")));
            let kept = document["evidence_selection"]["collections"][0]["kept"].as_u64().unwrap();
            assert!(kept > 500, "each source keeps a fair share: {kept}");
        }
    }

    #[test]
    fn spread_order_starts_at_both_ends_of_the_range() {
        let mut order: Vec<usize> = (0..8).collect();
        order.sort_by_key(|&index| spread_rank(index, 8));
        assert_eq!(order, [0, 4, 2, 6, 1, 5, 3, 7]);
        assert_eq!(spread_rank(0, 1), 0);
    }
}
