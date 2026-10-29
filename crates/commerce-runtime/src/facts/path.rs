//! The bounded path dialect an extraction is written in.
//!
//! A `capability_execution_contracts.output_map` entry says where a fact lives
//! in a provider's response body:
//!
//! ```text
//!   $.resultaten[*].kvkNummer
//!   $.naam
//!   $.data[0].attributes.lei
//! ```
//!
//! DELIBERATELY NOT JSONPATH. Full JSONPath has filter expressions, script
//! expressions and recursive descent, and this path is applied to a body that
//! an upstream provider controls. A dialect with a `?(@.price < 10)` in it is a
//! small programming language pointed at untrusted input, and the reason to
//! want one — "the interesting record is the one where type == 'main'" — is a
//! selection rule that belongs in a reviewed contract as an explicit field,
//! not smuggled into a string nobody diffs.
//!
//! So four constructs, and no way to write a fifth:
//!
//! | syntax    | meaning                                        |
//! |-----------|------------------------------------------------|
//! | `$`       | the response root, required as the first token |
//! | `.name`   | that key of an object                          |
//! | `[3]`     | that index of an array                         |
//! | `[*]`     | every element of an array, in order            |
//!
//! A key that is not an identifier is written `['weird key']`. Everything else
//! is a parse error, and a parse error is a contract that cannot reach
//! `validated` — the failure lands on whoever wrote the contract, never on the
//! buyer whose job is halfway through.

use std::fmt;

use serde_json::Value;

/// One step along a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// `.name` or `['name']` — that key of an object.
    Key(String),
    /// `[3]` — that index of an array. Out of range selects nothing rather
    /// than failing: a response with fewer elements than the contract hoped
    /// for is missing data, not a broken contract.
    Index(usize),
    /// `[*]` — every element of an array, in order.
    Wildcard,
}

/// A parsed extraction path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Path {
    segments: Vec<Segment>,
    source: String,
}

impl Path {
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// The text it was parsed from, for error messages and receipts. A fact
    /// records the path it came from, so "where did this number come from"
    /// has an answer that does not require re-reading the contract.
    pub fn as_str(&self) -> &str {
        &self.source
    }

    /// Whether this path can select more than one value. A contract that
    /// declares `cardinality: one` while its path is plural is a contract
    /// error, and it is caught when the contract is validated rather than when
    /// forty companies come back.
    pub fn is_plural(&self) -> bool {
        self.segments.iter().any(|s| matches!(s, Segment::Wildcard))
    }

    /// Every value this path selects, in document order.
    ///
    /// Selecting nothing is not an error here. Whether a missing fact is fatal
    /// depends on whether the contract marked it required, and that decision
    /// belongs to the caller.
    pub fn select<'v>(&self, root: &'v Value) -> Vec<&'v Value> {
        self.select_detailed(root).values
    }

    /// The same selection, plus the one thing a plain selection hides.
    ///
    /// `$.data[0].lei` looks singular and reads a single value, and when
    /// `data` holds forty records it has QUIETLY CHOSEN one of them. That is
    /// the failure the whole cardinality rule exists to prevent, and it is
    /// invisible from the result alone — one value came back either way.
    /// [`Selection::picked_from`] carries the size of the array an index step
    /// chose out of, so a contract that promises one subject can refuse
    /// instead of guessing.
    pub fn select_detailed<'v>(&self, root: &'v Value) -> Selection<'v> {
        let mut current: Vec<&Value> = vec![root];
        let mut picked_from: Option<usize> = None;
        for segment in &self.segments {
            let mut next: Vec<&Value> = Vec::new();
            for value in current {
                match segment {
                    Segment::Key(key) => {
                        if let Some(found) = value.get(key.as_str()) {
                            next.push(found);
                        }
                    }
                    Segment::Index(index) => {
                        if let Some(items) = value.as_array() {
                            if items.len() > 1 {
                                picked_from = Some(picked_from.unwrap_or(0).max(items.len()));
                            }
                        }
                        if let Some(found) = value.get(*index) {
                            next.push(found);
                        }
                    }
                    Segment::Wildcard => {
                        if let Some(items) = value.as_array() {
                            next.extend(items.iter());
                        }
                    }
                }
            }
            current = next;
            if current.is_empty() {
                break;
            }
        }
        Selection {
            values: current,
            picked_from,
        }
    }
}

/// What a selection found, and whether finding it involved a choice.
#[derive(Debug, Clone, PartialEq)]
pub struct Selection<'v> {
    /// The selected values, in document order.
    pub values: Vec<&'v Value>,
    /// `Some(n)` when an index step read one element out of an array holding
    /// `n > 1`. A contract that claims to describe a single subject may not
    /// ship a fact whose path did this.
    pub picked_from: Option<usize>,
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.source)
    }
}

/// Why a path string is not a path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("path must start with `$`")]
    MissingRoot,
    #[error("empty key at byte {0}")]
    EmptyKey(usize),
    #[error("unterminated `[` at byte {0}")]
    UnclosedBracket(usize),
    #[error("`{found}` is not an index, `*` or a quoted key (at byte {at})")]
    BadSubscript { found: String, at: usize },
    #[error("unexpected `{found}` at byte {at}; expected `.` or `[`")]
    Unexpected { found: char, at: usize },
}

/// Parse an extraction path, or say precisely why it is not one.
pub fn parse(input: &str) -> Result<Path, PathError> {
    let bytes: Vec<char> = input.chars().collect();
    let mut i = 0usize;

    if bytes.first() != Some(&'$') {
        return Err(PathError::MissingRoot);
    }
    i += 1;

    let mut segments = Vec::new();
    while i < bytes.len() {
        match bytes[i] {
            '.' => {
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != '.' && bytes[i] != '[' {
                    i += 1;
                }
                if i == start {
                    return Err(PathError::EmptyKey(start));
                }
                segments.push(Segment::Key(bytes[start..i].iter().collect()));
            }
            '[' => {
                let open = i;
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != ']' {
                    i += 1;
                }
                if i >= bytes.len() {
                    return Err(PathError::UnclosedBracket(open));
                }
                let raw: String = bytes[start..i].iter().collect();
                i += 1; // past ']'
                segments.push(subscript(&raw, start)?);
            }
            other => {
                return Err(PathError::Unexpected {
                    found: other,
                    at: i,
                })
            }
        }
    }

    Ok(Path {
        segments,
        source: input.to_string(),
    })
}

fn subscript(raw: &str, at: usize) -> Result<Segment, PathError> {
    if raw == "*" {
        return Ok(Segment::Wildcard);
    }
    // A quoted key, for the response fields that are not identifiers.
    let quoted = (raw.starts_with('\'') && raw.ends_with('\'') && raw.len() >= 2)
        || (raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2);
    if quoted {
        let inner = &raw[1..raw.len() - 1];
        if inner.is_empty() {
            return Err(PathError::EmptyKey(at));
        }
        return Ok(Segment::Key(inner.to_string()));
    }
    if !raw.is_empty() && raw.chars().all(|c| c.is_ascii_digit()) {
        // A response deeper than this is not a response we can price.
        if let Ok(index) = raw.parse::<usize>() {
            return Ok(Segment::Index(index));
        }
    }
    Err(PathError::BadSubscript {
        found: raw.to_string(),
        at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_path_must_be_rooted() {
        assert_eq!(parse("naam"), Err(PathError::MissingRoot));
        assert_eq!(parse(""), Err(PathError::MissingRoot));
        assert!(parse("$").is_ok());
    }

    #[test]
    fn reads_a_named_key() {
        let path = parse("$.naam").unwrap();
        let body = json!({ "naam": "Mollie B.V." });
        assert_eq!(path.select(&body), vec![&json!("Mollie B.V.")]);
    }

    #[test]
    fn reads_a_nested_key() {
        let path = parse("$.data.attributes.lei").unwrap();
        let body = json!({ "data": { "attributes": { "lei": "7245..." } } });
        assert_eq!(path.select(&body), vec![&json!("7245...")]);
    }

    #[test]
    fn reads_one_array_element() {
        let path = parse("$.resultaten[0].kvkNummer").unwrap();
        let body =
            json!({ "resultaten": [{ "kvkNummer": "30204054" }, { "kvkNummer": "99999999" }] });
        assert_eq!(path.select(&body), vec![&json!("30204054")]);
        assert!(!path.is_plural());
    }

    #[test]
    fn a_wildcard_reads_every_element_in_order() {
        let path = parse("$.resultaten[*].kvkNummer").unwrap();
        let body =
            json!({ "resultaten": [{ "kvkNummer": "30204054" }, { "kvkNummer": "99999999" }] });
        assert_eq!(
            path.select(&body),
            vec![&json!("30204054"), &json!("99999999")]
        );
        assert!(path.is_plural());
    }

    #[test]
    fn a_quoted_key_carries_characters_an_identifier_cannot() {
        let path = parse("$['_embedded']['eigenaar'].naam").unwrap();
        let body = json!({ "_embedded": { "eigenaar": { "naam": "X" } } });
        assert_eq!(path.select(&body), vec![&json!("X")]);
    }

    #[test]
    fn a_missing_key_selects_nothing_rather_than_failing() {
        let path = parse("$.resultaten[*].kvkNummer").unwrap();
        assert!(path.select(&json!({ "resultaten": [] })).is_empty());
        assert!(path.select(&json!({})).is_empty());
        assert!(path.select(&json!({ "resultaten": {} })).is_empty());
    }

    #[test]
    fn an_index_past_the_end_selects_nothing() {
        let path = parse("$.a[7]").unwrap();
        assert!(path.select(&json!({ "a": [1, 2] })).is_empty());
    }

    #[test]
    fn a_wildcard_skips_elements_that_lack_the_key() {
        let path = parse("$.rows[*].kvk").unwrap();
        let body = json!({ "rows": [{ "kvk": "1" }, { "other": true }, { "kvk": "2" }] });
        assert_eq!(path.select(&body), vec![&json!("1"), &json!("2")]);
    }

    // --- what the dialect refuses ------------------------------------------
    //
    // Each of these is a construct real JSONPath has. Every one of them is a
    // parse error here, on purpose, and the test exists so that stays true.

    #[test]
    fn refuses_a_filter_expression() {
        assert!(matches!(
            parse("$.rows[?(@.type=='main')].kvk"),
            Err(PathError::BadSubscript { .. })
        ));
    }

    #[test]
    fn refuses_recursive_descent() {
        // `$..kvk` parses its first `.` and then finds an empty key.
        assert!(matches!(parse("$..kvk"), Err(PathError::EmptyKey(_))));
    }

    #[test]
    fn refuses_a_slice() {
        assert!(matches!(
            parse("$.rows[0:2]"),
            Err(PathError::BadSubscript { .. })
        ));
    }

    #[test]
    fn refuses_an_unclosed_bracket() {
        assert!(matches!(
            parse("$.rows[0"),
            Err(PathError::UnclosedBracket(_))
        ));
    }

    #[test]
    fn refuses_a_stray_character() {
        assert!(matches!(
            parse("$naam"),
            Err(PathError::Unexpected { found: 'n', .. })
        ));
    }

    #[test]
    fn indexing_into_a_single_element_array_is_not_a_choice() {
        let path = parse("$.data[0].lei").unwrap();
        let body = json!({ "data": [{ "lei": "724500..." }] });
        let found = path.select_detailed(&body);
        assert_eq!(found.values, vec![&json!("724500...")]);
        assert_eq!(found.picked_from, None);
    }

    #[test]
    fn indexing_into_a_longer_array_reports_the_choice_it_made() {
        let path = parse("$.data[0].lei").unwrap();
        let body = json!({ "data": [{ "lei": "A" }, { "lei": "B" }, { "lei": "C" }] });
        let found = path.select_detailed(&body);
        // It still returns A — but it says out loud that it chose A out of 3.
        assert_eq!(found.values, vec![&json!("A")]);
        assert_eq!(found.picked_from, Some(3));
    }

    #[test]
    fn a_wildcard_is_never_a_choice() {
        let path = parse("$.data[*].lei").unwrap();
        let body = json!({ "data": [{ "lei": "A" }, { "lei": "B" }] });
        assert_eq!(path.select_detailed(&body).picked_from, None);
    }

    #[test]
    fn keeps_the_text_it_was_parsed_from() {
        let path = parse("$.resultaten[*].kvkNummer").unwrap();
        assert_eq!(path.as_str(), "$.resultaten[*].kvkNummer");
        assert_eq!(path.to_string(), "$.resultaten[*].kvkNummer");
    }
}
