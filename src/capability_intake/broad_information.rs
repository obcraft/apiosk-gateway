//! Literal, constraint-free information requests. This recognizes the whole
//! sentence, not isolated keywords: extra requested facts remain on the normal
//! strict extraction path. No provider or returned value is chosen here.
use super::*;

pub(crate) fn parse(request: &str, model: &str) -> Option<ParsedRequest> {
    let text = literal_sentence(request).trim_end_matches(['.', '?', '!']).trim();
    let lower = text.to_lowercase();
    let (language, categories, address) = [
        ("en", "Find ", " information for "),
        ("en", "Show ", " information for "),
        ("en", "Give ", " information for "),
        ("nl", "Zoek ", "informatie voor "),
        ("nl", "Geef ", "informatie voor "),
        ("nl", "Toon ", "informatie voor "),
    ]
    .into_iter()
    .find_map(|(language, prefix, separator)| {
        let body = lower.strip_prefix(&prefix.to_lowercase())?;
        let (categories, _) = body.split_once(separator)?;
        let start = prefix.len() + categories.len() + separator.len();
        Some((language, categories, text.get(start..)?))
    })?;
    let categories = categories.trim();
    let concepts: &[&str] = match (language, categories) {
        ("en", "building") | ("nl", "gebouw") | ("nl", "pand") => &["building"],
        ("en", "neighbourhood" | "neighborhood") | ("nl", "buurt" | "wijk") => &["neighbourhood"],
        ("en", "building and neighbourhood" | "building and neighborhood")
        | ("nl", "gebouw- en buurt" | "gebouw en buurt" | "pand- en buurt" | "pand en buurt") => {
            &["building", "neighbourhood"]
        }
        ("en", "neighbourhood and building" | "neighborhood and building") => {
            &["neighbourhood", "building"]
        }
        _ => return None,
    };
    let address = address.trim();
    // The shared address parser consumes the complete literal with a positive
    // street/house-number/locality grammar. An extra period, output format or
    // ownership clause cannot be mistaken for part of the city and discarded.
    let parsed_address = crate::pdok_address::parse_literal(address)?;
    if parsed_address.city.is_none() && parsed_address.postcode.is_none() {
        return None;
    }
    let subject = Subject {
        id: "s1".into(),
        label: address.into(),
        kind: "address".into(),
        identifiers: BTreeMap::new(),
    };
    let capabilities = concepts.iter().map(|concept| {
        let (name, description) = if *concept == "building" {
            ("Building information", "Retrieve available official building information for an address, preserving the scope and missing fields of the source.")
        } else {
            ("Neighbourhood information", "Retrieve available official neighbourhood statistics for an address, preserving their area and reporting period.")
        };
        Capability { slug: format!("location.{concept}.information"), name: name.into(), description: description.into(),
            domain: "location".into(), inputs: vec!["address".into()], outputs: vec![format!("{concept}_info")],
            subject_id: Some(subject.id.clone()), source_hint: None, confidence: 1.0, missing: vec![] }
    }).collect();
    Some(ParsedRequest {
        language: language.into(),
        subjects: vec![subject],
        capabilities,
        deliverable: Deliverable {
            format: "chat".into(),
            operations: vec![],
        },
        meta: Meta {
            model: model.into(),
            parsed_at: chrono::Utc::now().to_rfc3339(),
            text_sha256: hex::encode(Sha256::digest(request.as_bytes())),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ask_interpretation_does_not_need_a_model_for_the_literal_information_request() {
        let client =
            crate::ask_v2::Client::new(reqwest::Client::new(), String::new(), "fixture".into());
        let response = client
            .interpret(
                "Find building and neighbourhood information for Damrak 1, Amsterdam.",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(response["model"], "literal-broad-information");
        assert_eq!(response["parsed_request"]["language"], "en");
        assert_eq!(
            response["parsed_request"]["subjects"][0]["label"],
            "Damrak 1, Amsterdam"
        );
        assert_eq!(
            response["parsed_request"]["capabilities"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn broad_information_preserves_both_requested_categories_and_full_literal_address() {
        for (question, address, language) in [
            (
                "Find building and neighbourhood information for Damrak 1, Amsterdam.",
                "Damrak 1, Amsterdam",
                "en",
            ),
            (
                "Show building and neighborhood information for Coolsingel 40, Rotterdam.",
                "Coolsingel 40, Rotterdam",
                "en",
            ),
            (
                "Geef gebouw- en buurtinformatie voor Grote Markt 1, Groningen.",
                "Grote Markt 1, Groningen",
                "nl",
            ),
        ] {
            let parsed = normalize("{}", question, "fixture").unwrap();
            assert_eq!(parsed.subjects.len(), 1);
            assert_eq!(parsed.subjects[0].label, address);
            assert!(
                parsed.subjects[0].identifiers.is_empty(),
                "country and source IDs must not be inferred"
            );
            assert_eq!(parsed.language, language);
            assert_eq!(
                parsed
                    .capabilities
                    .iter()
                    .map(|cap| cap.outputs[0].as_str())
                    .collect::<Vec<_>>(),
                ["building_info", "neighbourhood_info"]
            );
            assert!(parsed
                .capabilities
                .iter()
                .all(|cap| cap.missing.is_empty() && cap.source_hint.is_none()));
        }
    }

    #[test]
    fn explicit_details_sources_periods_and_extra_clauses_never_become_optional() {
        for question in [
            "Find building and neighbourhood information for Damrak 1, Amsterdam, including floor count.",
            "Find building information for Damrak 1, Amsterdam and energy label.",
            "Find building information for Damrak 1, Amsterdam. Include construction year.",
            "Find building information for Damrak 1, Amsterdam via PDOK.",
            "Find building information for Damrak 1, Amsterdam before 2020.",
            "Find building information for Damrak 1, Amsterdam in 2020.",
            "Find building information for Damrak 1, Amsterdam as JSON.",
            "Find building information for Damrak 1, Amsterdam owner details.",
            "Find building information for Damrak 1, Amsterdam & ownership.",
            "Find building information for Damrak 1, Amsterdam + energy label.",
            "Geef gebouwinformatie voor Damrak 1, Amsterdam met energielabel.",
            "Geef gebouw- en buurtinformatie voor Damrak 1, Amsterdam en oppervlakte.",
            "Find building name and floor count for Damrak 1, Amsterdam.",
        ] { assert!(parse(question, "fixture").is_none(), "{question}"); }
    }
}
