//! 04-07 — the optional `X-Apiosk-Purpose` note ("why does the agent want this
//! skill"). One central sanitizer, reused wherever a purpose is captured.
//!
//! The value is treated as PII: it is trimmed, stripped of control characters,
//! and capped in length here, and callers MUST never log it (only persist it to
//! `connect_payments.purpose_text`, visible to the seller — 05-03). The storage
//! itself happens when the payment row is created on the settle path (04-04); this
//! module is the pure, tested sanitization the whole track shares.

/// Max stored characters. A short prompt, not a document.
pub const MAX_PURPOSE_LEN: usize = 280;

/// Sanitize an optional purpose header value. Returns `None` for missing/blank
/// input (so `purpose_text` stays NULL — never an error). Otherwise trims, drops
/// control characters (newlines/tabs/etc.), and caps at [`MAX_PURPOSE_LEN`] chars.
/// Pure.
pub fn sanitize_purpose(raw: Option<&str>) -> Option<String> {
    let trimmed = raw?.trim();
    if trimmed.is_empty() {
        return None;
    }
    let cleaned: String = trimmed
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_PURPOSE_LEN)
        .collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_blank_is_none() {
        assert_eq!(sanitize_purpose(None), None);
        assert_eq!(sanitize_purpose(Some("")), None);
        assert_eq!(sanitize_purpose(Some("   ")), None);
        // All-control input collapses to nothing.
        assert_eq!(sanitize_purpose(Some("\n\t\r")), None);
    }

    #[test]
    fn trims_and_keeps_normal_text() {
        assert_eq!(
            sanitize_purpose(Some("  book a flight to Lisbon  ")),
            Some("book a flight to Lisbon".to_string())
        );
    }

    #[test]
    fn strips_control_characters() {
        // Newlines/tabs (e.g. a header-injection attempt) are removed.
        let out = sanitize_purpose(Some("line1\nline2\tend")).unwrap();
        assert!(!out.contains('\n') && !out.contains('\t'));
        assert_eq!(out, "line1line2end");
    }

    #[test]
    fn caps_at_max_length() {
        let long = "a".repeat(MAX_PURPOSE_LEN + 100);
        let out = sanitize_purpose(Some(&long)).unwrap();
        assert_eq!(out.chars().count(), MAX_PURPOSE_LEN);
    }
}
