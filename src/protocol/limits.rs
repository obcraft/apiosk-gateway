//! One length definition for every Ask surface, shared with the App through
//! `contracts/ask-limits.json` (the App keeps a byte copy). A length is the
//! number of Unicode scalar values of the exact string that was sent. Nothing
//! is normalized first: the parser matches literals against that same text.
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AskLimits {
    pub version: u8,
    pub unit: String,
    pub question: QuestionLimits,
    pub selection: SelectionLimits,
    pub legacy: LegacyLimits,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionLimits {
    pub max_chars: usize,
}
/// Structured context has its own budget; it never counts against the question.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionLimits {
    pub max_endpoints: usize,
    pub max_inputs_per_endpoint: usize,
    pub max_value_chars: usize,
    pub max_total_value_chars: usize,
}
/// What an App must stay within when the gateway does not advertise
/// `question_chars_v1`: the byte rule older gateways enforce.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyLimits {
    pub question_max_utf8_bytes: usize,
}

pub fn limits() -> &'static AskLimits {
    static LIMITS: OnceLock<AskLimits> = OnceLock::new();
    LIMITS.get_or_init(|| serde_json::from_str(include_str!("../../contracts/ask-limits.json")).expect("contracts/ask-limits.json matches AskLimits"))
}

/// The length of `text` in Unicode scalar values.
pub fn length(text: &str) -> usize {
    text.chars().count()
}

/// A question has at least one visible character and fits its budget.
pub fn question_ok(question: &str) -> bool {
    !question.trim().is_empty() && length(question) <= limits().question.max_chars
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn question_budget_is_the_published_discover_limit() {
        let schemas: Value = serde_json::from_str(include_str!("../../contracts/tool-schemas.json")).unwrap();
        assert_eq!(schemas["discover"]["properties"]["question"]["maxLength"], limits().question.max_chars);
        assert_eq!(limits().unit, "unicode_scalar_values");
        assert_eq!(limits().selection.max_endpoints, crate::protocol::MAX_STEPS);
    }

    #[test]
    fn accents_and_emoji_count_once_per_character() {
        let max = limits().question.max_chars;
        for character in ["a", "é", "€", "😀"] {
            let full = character.repeat(max);
            assert!(question_ok(&full), "{max} x {character} is {} bytes and still fits", full.len());
            assert!(!question_ok(&character.repeat(max + 1)));
        }
        // A combining accent is its own scalar value: no normalization.
        assert_eq!(length("e\u{301}"), 2);
    }

    #[test]
    fn a_question_needs_a_visible_character() {
        assert!(!question_ok(""));
        assert!(!question_ok(" \n\t "));
        assert!(question_ok(" ?"));
    }
}
