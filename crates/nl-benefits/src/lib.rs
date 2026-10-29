//! First-party estimates and bounded municipal retrieval. These run inside
//! the executor's existing preflight/settlement path, never on a public bypass.
pub mod calculate;
pub mod municipal;

use serde_json::Value;
pub const CALCULATOR_SOURCE: &str = "nl-toeslagen-rekenservice";
pub const MUNICIPAL_SOURCE: &str = "nl-gemeentelijke-regelingen";
pub const RULESET: &str = "nl-2026.1";

#[derive(Debug)]
pub struct Error {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
}
impl Error {
    pub fn input(message: impl Into<String>) -> Self {
        Self {
            status: 400,
            code: "invalid_benefits_input",
            message: message.into(),
        }
    }
    pub fn upstream(message: impl Into<String>) -> Self {
        Self {
            status: 502,
            code: "municipal_source_unavailable",
            message: message.into(),
        }
    }
}

pub fn handles(source: &str) -> bool {
    matches!(source, CALCULATOR_SOURCE | MUNICIPAL_SOURCE)
}
pub async fn answer(source: &str, method: &str, path: &str, body: &[u8]) -> Result<Value, Error> {
    if method != "POST" || body.len() > 32_768 {
        return Err(Error::input("Use POST with a JSON body of at most 32 KiB"));
    }
    match source {
        CALCULATOR_SOURCE => calculate::answer(path, body),
        MUNICIPAL_SOURCE if path == "/regelingen/search" => municipal::answer(body).await,
        _ => Err(Error::input("Unsupported Dutch benefits route")),
    }
}

pub(crate) fn decode<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, Error> {
    // Never include the submitted value in errors: requests contain finances.
    serde_json::from_slice(body).map_err(|e| {
        let message = e.to_string();
        if message.starts_with("missing field") || message.starts_with("unknown field") {
            Error::input(message)
        } else {
            Error::input("Invalid JSON field type or value; see the endpoint input schema")
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn dispatcher_rejects_wrong_sources_methods_paths_and_oversized_bodies() {
        assert!(!handles("nibud"));
        for (source, method, path, body) in [
            (
                CALCULATOR_SOURCE,
                "GET",
                "/2026/zorgtoeslag/calculate",
                b"{}".to_vec(),
            ),
            (CALCULATOR_SOURCE, "POST", "/other", b"{}".to_vec()),
            (
                "nibud",
                "POST",
                "/2026/zorgtoeslag/calculate",
                b"{}".to_vec(),
            ),
            (
                MUNICIPAL_SOURCE,
                "POST",
                "/regelingen/search",
                vec![b' '; 32769],
            ),
        ] {
            assert_eq!(
                answer(source, method, path, &body)
                    .await
                    .unwrap_err()
                    .status,
                400
            );
        }
        let bad = br#"{"municipality":"Leiden","query":"bijstand","max_pages":6}"#;
        assert_eq!(
            answer(MUNICIPAL_SOURCE, "POST", "/regelingen/search", bad)
                .await
                .unwrap_err()
                .status,
            400
        );
    }
}
