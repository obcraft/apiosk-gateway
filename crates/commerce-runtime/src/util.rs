//! Small helpers every module used to carry its own copy of.

use sha2::{Digest, Sha256};

/// Cap a string to `max_chars` characters (not bytes), preserving valid UTF-8.
pub(crate) fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

/// Lower-case hex SHA-256 of `value`.
pub(crate) fn sha256_hex(value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(value.as_bytes());
    hex::encode(digest.finalize())
}

/// An on/off switch from the environment: `1`, `true`, `yes` and `on` (any
/// case) are on, anything else set is off, and unset is `default`.
pub(crate) fn env_bool(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(raw) => matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_counts_characters_not_bytes() {
        assert_eq!(truncate_chars("héllo wörld", 5), "héllo");
        assert_eq!(truncate_chars("short", 10), "short");
    }

    #[test]
    fn env_switch_reads_the_usual_spellings() {
        std::env::set_var("UTIL_TEST_SWITCH", " Yes ");
        assert!(env_bool("UTIL_TEST_SWITCH", false));
        std::env::set_var("UTIL_TEST_SWITCH", "0");
        assert!(!env_bool("UTIL_TEST_SWITCH", true));
        std::env::remove_var("UTIL_TEST_SWITCH");
        assert!(env_bool("UTIL_TEST_SWITCH", true));
    }
}
