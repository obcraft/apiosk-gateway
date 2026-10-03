//! A whole, bare KVK lookup asks for the available registered profile. It
//! cannot imply every possible company field, filings or a risk assessment.
use super::*;

pub(crate) fn parse(request: &str, model: &str) -> Option<ParsedRequest> {
    let text = request.trim().trim_end_matches(['.', '?', '!']).trim();
    let words: Vec<_> = text.split_whitespace().collect();
    let number = |word: &str| word.len() == 8 && word.bytes().all(|b| b.is_ascii_digit());
    let (identifier, phrase) = if number(words.first()?) {
        (*words.first()?, words.get(1..)?)
    } else if number(words.last()?) {
        (*words.last()?, words.get(..words.len() - 1)?)
    } else {
        return None;
    };
    // Positive whole-phrase grammar: extra fields, formats, periods, names,
    // multiple identifiers and ambiguous checks stay with the full parser.
    let phrase = phrase
        .join(" ")
        .to_lowercase()
        .replace("kvk-nummer", "kvk nummer");
    let (language, rest) = [
        ("nl", "controleer "),
        ("nl", "zoek "),
        ("nl", "bekijk "),
        ("nl", "toon "),
        ("nl", "check "),
        ("en", "look up "),
    ]
    .into_iter()
    .find_map(|(language, prefix)| phrase.strip_prefix(prefix).map(|rest| (language, rest)))?;
    let language = match rest {
        "kvk" | "kvk nummer" | "kvknummer" | "dit kvk nummer" | "dit kvknummer" | "dit kvk"
        | "het kvk nummer" | "het kvknummer" | "deze kvk" => language,
        "kvk number" | "this kvk number" | "the kvk number" => "en",
        _ => return None,
    };
    Some(ParsedRequest {
        language: language.into(),
        subjects: vec![Subject {
            id: "s1".into(), label: identifier.into(), kind: "company".into(),
            identifiers: BTreeMap::from([("kvk_number".into(), identifier.into())]),
        }],
        capabilities: vec![Capability {
            slug: "company.registry.profile".into(), name: "Company profile".into(),
            description: "Look up the available official registered company profile by registration number. Preserve the source's scope and missing fields.".into(),
            domain: "company".into(), inputs: vec!["kvk_number".into()], outputs: vec!["profile".into()],
            subject_id: Some("s1".into()), source_hint: Some("kvk".into()), confidence: 1.0, missing: vec![],
        }],
        deliverable: Deliverable { format: "chat".into(), operations: vec![] },
        meta: Meta { model: model.into(), parsed_at: chrono::Utc::now().to_rfc3339(),
            text_sha256: hex::encode(Sha256::digest(request.as_bytes())) },
    })
}

pub(crate) fn matches_intake(question: &str, intake: &Intake) -> bool {
    let Some(expected) = parse(question, "literal-company-lookup") else {
        return false;
    };
    let actual = &intake.parsed_request;
    actual.language == expected.language
        && serde_json::to_value(&actual.subjects).ok()
            == serde_json::to_value(&expected.subjects).ok()
        && serde_json::to_value(&actual.capabilities).ok()
            == serde_json::to_value(&expected.capabilities).ok()
        && serde_json::to_value(&actual.deliverable).ok()
            == serde_json::to_value(&expected.deliverable).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bare_kvk_check_interprets_only_the_registered_profile_without_a_model() {
        let client =
            crate::ask_v2::Client::new(reqwest::Client::new(), String::new(), "fixture".into());
        for (question, identifier, language) in [
            ("87560488 check dit kvk nummer", "87560488", "nl"),
            ("Controleer het KvK-nummer 00123456.", "00123456", "nl"),
            ("Check KVK 24330087", "24330087", "nl"),
            ("  Zoek   kvknummer 30204462!  ", "30204462", "nl"),
            ("Check this KVK number 87560488", "87560488", "en"),
            ("Look up KVK number 00123456.", "00123456", "en"),
        ] {
            let response = client.interpret(question, &[]).await.unwrap();
            let parsed = &response["parsed_request"];
            assert_eq!(parsed["language"], language, "{question}");
            assert_eq!(parsed["subjects"][0]["label"], identifier);
            assert_eq!(
                parsed["subjects"][0]["identifiers"],
                json!({"kvk_number": identifier})
            );
            assert_eq!(parsed["capabilities"].as_array().unwrap().len(), 1);
            assert_eq!(parsed["capabilities"][0]["outputs"], json!(["profile"]));
            assert_eq!(parsed["capabilities"][0]["source_hint"], "kvk");
        }
    }

    #[test]
    fn extra_scope_and_ambiguous_identifiers_are_never_reduced_to_a_profile() {
        for question in [
            "87560488 check dit kvk nummer en geef het e-mailadres",
            "Check KVK 87560488 met personeelsaantal en kapitaal",
            "Check KVK 87560488 inclusief jaarrekeningen",
            "Check KVK 87560488 op sancties",
            "Check KVK 87560488 in 2024",
            "Check KVK 87560488 as JSON",
            "Check KVK 87560488 en 24330087",
            "Check KVK 87560488 + adres",
            "Check KVK 87560488 via Overheid.io",
            "Check KVK 87560488. Toon de omzet.",
            "Check KVK 8756048",
            "Check KVK 875604880",
            "Check KVK 8756O488",
            "87560488",
            "Check 87560488",
            "Check dit bedrijf 87560488",
        ] {
            assert!(parse(question, "fixture").is_none(), "{question}");
        }
        let question = "Check KVK 87560488 met e-mailadres";
        let mut raw =
            serde_json::to_value(parse("Check KVK 87560488", "fixture").unwrap()).unwrap();
        raw["capabilities"][0]["outputs"] = json!(["profile", "email"]);
        let parsed = normalize(&raw.to_string(), question, "fixture").unwrap();
        assert_eq!(parsed.capabilities[0].outputs, ["profile", "email"]);
    }
}
