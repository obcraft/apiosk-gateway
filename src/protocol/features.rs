//! Feature negotiation for `GET /v2/meta`. A feature is advertised only when
//! this build implements it and the operator lists it in `APIOSK_V2_FEATURES`
//! (a comma list; `*` lists every implemented feature). A rolling deploy mixes
//! old and new instances behind one address, so a feature is listed only after
//! every instance runs the new code, and unlisted five minutes before a code
//! rollback. The App treats an unlisted feature exactly like an old gateway.

/// Every feature this build implements, in the order they are advertised.
pub const IMPLEMENTED: &[&str] = &[
    "trace_header_v1",
    "question_chars_v1",
    "chat_selection_v1",
    "request_receipts_v1",
    "requote_v1",
];

/// The implemented features the environment allows.
pub fn advertised() -> Vec<&'static str> {
    allowed(std::env::var("APIOSK_V2_FEATURES").ok().as_deref())
}

fn allowed(list: Option<&str>) -> Vec<&'static str> {
    let listed: Vec<&str> = list.unwrap_or_default().split(',').map(str::trim).filter(|name| !name.is_empty()).collect();
    IMPLEMENTED.iter().copied().filter(|feature| listed.contains(&"*") || listed.contains(feature)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_advertised_until_the_operator_lists_it() {
        assert!(allowed(None).is_empty());
        assert!(allowed(Some("")).is_empty());
        assert!(allowed(Some(" , ")).is_empty());
    }

    #[test]
    fn only_implemented_features_are_advertised() {
        assert_eq!(allowed(Some("*")), IMPLEMENTED);
        assert_eq!(allowed(Some(" trace_header_v1 ,future_feature_v9")), ["trace_header_v1"]);
    }
}
