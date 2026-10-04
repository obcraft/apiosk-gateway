//! Compose complete literal requests, preserving every clause and caller value.
use crate::capability_intake::{Deliverable, Meta, ParsedRequest};
use sha2::{Digest, Sha256};

mod atoms;
pub(crate) const COMBINE: &str = "Combine the results into one cited answer, connecting relevant findings and clearly stating unknowns.";
pub(crate) struct Clause {
    pub question: String,
    pub parsed: ParsedRequest,
}
pub(crate) struct Request {
    pub clauses: Vec<Clause>,
    pub parsed: ParsedRequest,
}

fn atomic(question: &str, model: &str) -> Option<ParsedRequest> {
    crate::capability_intake::atomic_literal_request(question, model)
        .or_else(|| atoms::parse(question, model))
}

// Whole-span recognition preserves company abbreviations, URLs and request
// sentences such as "Cite the sources". Every remaining span must also parse.
fn spans(text: &str, model: &str, depth: usize) -> Option<Vec<Clause>> {
    let text = text.trim();
    if text.is_empty() {
        return Some(vec![]);
    }
    if depth == 8 {
        return None;
    }
    if let Some(parsed) = atomic(text, model) {
        return Some(vec![Clause {
            question: text.into(),
            parsed,
        }]);
    }
    let mut quoted = false;
    let mut ends = Vec::new();
    for (index, ch) in text.char_indices() {
        if ch == '"' {
            quoted = !quoted;
        }
        if !quoted
            && (ch == '\n'
                || matches!(ch, '.' | ';')
                    && text
                        .get(index + ch.len_utf8()..)
                        .is_some_and(|tail| tail.starts_with(char::is_whitespace)))
        {
            ends.push(index + ch.len_utf8());
            if ends.len() > 64 {
                return None;
            }
        }
    }
    for end in ends.into_iter().rev() {
        let part = text[..end].trim().trim_end_matches(';').trim();
        let Some(parsed) = atomic(part, model) else {
            continue;
        };
        let Some(mut rest) = spans(&text[end..], model, depth + 1) else {
            continue;
        };
        rest.insert(
            0,
            Clause {
                question: part.into(),
                parsed,
            },
        );
        return Some(rest);
    }
    None
}

pub(crate) fn recognize(question: &str, model: &str) -> Option<Request> {
    let text = question.trim();
    if text.len() > 8_000 {
        return None;
    }
    let body = if text
        .to_ascii_lowercase()
        .ends_with(&COMBINE.to_ascii_lowercase())
    {
        text.get(..text.len() - COMBINE.len())?.trim()
    } else {
        text
    };
    let clauses = spans(body, model, 0)?;
    if clauses.len() < 2 {
        return None;
    }
    let language = clauses.first()?.parsed.language.clone();
    if clauses
        .iter()
        .any(|clause| clause.parsed.language != language)
    {
        return None;
    }
    let mut parsed = ParsedRequest {
        language,
        subjects: vec![],
        capabilities: vec![],
        deliverable: Deliverable {
            format: "chat".into(),
            operations: vec!["summarize".into(), "analyze".into()],
        },
        meta: Meta {
            model: model.into(),
            parsed_at: chrono::Utc::now().to_rfc3339(),
            text_sha256: hex::encode(Sha256::digest(question.as_bytes())),
        },
    };
    for clause in &clauses {
        if clause.parsed.deliverable.format != "chat" {
            return None;
        }
        let mut ids = std::collections::BTreeMap::new();
        for subject in &clause.parsed.subjects {
            let existing = parsed
                .subjects
                .iter()
                .find(|s| {
                    s.label == subject.label
                        && s.kind == subject.kind
                        && s.identifiers == subject.identifiers
                })
                .map(|s| s.id.clone());
            let id = existing.unwrap_or_else(|| {
                let mut subject = subject.clone();
                subject.id = format!("s{}", parsed.subjects.len() + 1);
                let id = subject.id.clone();
                parsed.subjects.push(subject);
                id
            });
            ids.insert(subject.id.clone(), id);
        }
        for cap in &clause.parsed.capabilities {
            let mut cap = cap.clone();
            cap.subject_id = match &cap.subject_id {
                Some(id) => Some(ids.get(id)?.clone()),
                None => None,
            };
            parsed.capabilities.push(cap);
        }
        for operation in &clause.parsed.deliverable.operations {
            if !parsed.deliverable.operations.contains(operation) {
                parsed.deliverable.operations.push(operation.clone());
            }
        }
    }
    if parsed.subjects.len() > 8 || parsed.capabilities.len() > 16 {
        return None;
    }
    Some(Request { clauses, parsed })
}

pub(crate) fn parse(question: &str, model: &str) -> Option<ParsedRequest> {
    recognize(question, model).map(|request| request.parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn variable_values_abbreviations_and_domains_are_preserved() {
        for (name, number, domain) in [
            ("Mollie B.V.", "30204462", "mollie.com"),
            ("Other N.V.", "12345678", "other.nl"),
        ] {
            let question = format!("Find the registered details and available annual financial figures for {name} (KVK {number}). Get public RDAP registration data and mail DNS records for {domain}. Cite the sources. {COMBINE}");
            let request = recognize(&question, "test").unwrap();
            assert_eq!(request.clauses.len(), 2);
            assert_eq!(request.parsed.subjects[0].identifiers["kvk_number"], number);
            assert_eq!(request.parsed.subjects[1].label, domain);
            assert_eq!(request.parsed.capabilities.len(), 4);
            assert_eq!(request.parsed.capabilities[0].outputs, ["profile"]);
            assert_eq!(request.parsed.subjects[0].label, name);
        }
    }
    #[test]
    fn unknown_clauses_extra_fields_and_formats_are_never_dropped() {
        let core = "Find building and neighbourhood information for Damrak 1, Amsterdam. Get the latest official ECB reference exchange rate for EUR/USD, including the observation date, via Frankfurter.";
        assert!(recognize(core, "test").is_some());
        for question in [
            format!("{core} Include private ownership records. {COMBINE}"),
            format!("{core} Return a PDF. {COMBINE}"),
            format!("{core} {COMBINE} Include salaries."),
            core.replace("Amsterdam.", "Amsterdam, including energy labels."),
        ] {
            assert!(recognize(&question, "test").is_none(), "{question}");
        }
    }
    #[test]
    fn different_checks_keep_their_own_subject_values() {
        let q = "Validate EU VAT number NL815839091B01 through VIES and show its validity and check date. Validate EU VAT number DE123456789 through VIES and show its validity and check date.";
        let parsed = recognize(q, "test").unwrap().parsed;
        assert_eq!(parsed.subjects.len(), 2);
        assert_eq!(parsed.capabilities[0].subject_id.as_deref(), Some("s1"));
        assert_eq!(parsed.capabilities[1].subject_id.as_deref(), Some("s2"));
    }
}
