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

/// The normalized request owns the scope, regardless of sentence wording or
/// whether its literal identifier came from an explicitly continued turn.
/// Every extra field, filter or answer operation keeps the full planner. This
/// never rewrites an extraction or reduces richer requirements to a profile.
pub(crate) fn plain_profile<'a>(question: &str, intake: &'a Intake) -> Option<&'a ParsedRequest> {
    let parsed = &intake.parsed_request;
    if parsed.meta.text_sha256 != hex::encode(Sha256::digest(question.as_bytes()))
        || parsed.subjects.len() != 1 || parsed.capabilities.len() != 1
        || parsed.deliverable.format != "chat" || !parsed.deliverable.operations.is_empty() {
        return None;
    }
    let subject = &parsed.subjects[0];
    let capability = &parsed.capabilities[0];
    let number = subject.identifiers.get("kvk_number")?;
    if subject.kind != "company" || subject.label.trim().is_empty()
        || subject.identifiers.len() != 1
        || number.len() != 8 || !number.bytes().all(|byte| byte.is_ascii_digit())
        || capability.domain != "company"
        || !matches!(capability.slug.as_str(), "company.profile" | "company.registry.profile")
        || capability.subject_id.as_deref() != Some(subject.id.as_str())
        || capability.outputs != ["profile"]
        || capability.inputs.iter().any(|input| input != "kvk_number")
        || !capability.missing.is_empty()
        || capability.source_hint.as_deref().is_some_and(|hint| hint != "kvk") {
        return None;
    }
    Some(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn semantic_intake(question: &str, literals: &str, number: &str, label: &str, catalog: &Catalog) -> Intake {
        // A normalized model-shaped request, not the literal sentence parser.
        let parsed_request = normalize_with_context(&json!({"language":"nl",
            "subjects":[{"id":"company","label":label,"type":"company","identifiers":{"kvk_number":number}}],
            "capabilities":[{"slug":"company.profile","name":"Company profile","description":"Retrieve the available registered company profile.",
                "domain":"company","inputs":["kvk_number"],"outputs":["profile"],"subject_id":"company","source_hint":null,"confidence":0.95}],
            "deliverable":{"format":"chat","operations":[]}}).to_string(), question, literals, "semantic-fixture").unwrap();
        let matches = crate::ask_v2::search::search(&parsed_request, catalog);
        Intake { selected_capabilities:matches.iter().flat_map(|found| found.operations.iter().cloned()).collect(),
            parsed_request, matches, apiosk_sources_searched:source_count(catalog), catalog_version:catalog.version.clone() }
    }

    #[tokio::test]
    async fn normalized_profile_planning_uses_scope_across_wording_numbers_and_continued_context() {
        let catalog = Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/workflows/catalog.json")).unwrap());
        let parser = crate::parser::Parser::new(reqwest::Client::new(), "http://127.0.0.1:1".into(), String::new(), "fixture".into());
        for (question, number, label, earlier) in [
            ("Kun je het beschikbare bedrijfsprofiel ophalen voor KVK 87560488?", "87560488", "87560488", None),
            ("Please retrieve the registered company profile for KVK number 30204462.", "30204462", "30204462", None),
            ("Toon het bedrijfsprofiel van Example B.V. met KVK 00123456", "00123456", "Example B.V.", None),
            ("Laat dat bedrijfsprofiel zien.", "87560488", "87560488", Some("Het gaat om KVK 87560488.")),
        ] {
            assert!(parse(question, "fixture").is_none(), "exercise the semantic path: {question}");
            let mut task = Task::default();
            if let Some(previous) = earlier {
                task.conversation.push(crate::state::ConversationTurn { question:previous.into(), reply:None, output:None });
            }
            let literals = format!("{question}\n{}", earlier.unwrap_or(""));
            let intake = semantic_intake(question, &literals, number, label, &catalog);
            assert!(plain_profile(question, &intake).is_some(), "{question}");
            let reply = parser.chat_from_capabilities(question, None, &task, &catalog, &intake).await.unwrap();
            assert!(reply.discover, "{question}: {}", reply.message);
            let mut intent = reply.intent.unwrap();
            task.capability_intake = Some(intake);
            crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
            let crate::resolver::Resolution::Plan(plan) = crate::resolver::resolve(&catalog, &task, &intent) else { panic!("{question}: exact identifier must resolve") };
            assert_eq!(plan.steps.len(), 1);
            assert_eq!(plan.steps[0].service.api_slug, "kvk-dutch-business-register");
            assert_eq!(plan.steps[0].service.inputs(&task, plan.steps[0].entity_ref, &catalog.vocabulary).unwrap(), json!({"kvkNummer":number}));
            assert_eq!(intent.subjects[0].label, label);
            assert_eq!(intent.answer_operations, ["Requested answer format: chat"]);
        }
        assert_eq!(parser.calls.load(std::sync::atomic::Ordering::Relaxed), 0, "complete matched profile scope needs no second interpretation");
    }

    #[test]
    fn normalized_profile_scope_never_drops_fields_filters_operations_or_other_subjects() {
        let catalog = Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/workflows/catalog.json")).unwrap());
        let question = "Geef het bedrijfsprofiel voor KVK 87560488";
        let intake = semantic_intake(question, question, "87560488", "87560488", &catalog);
        assert!(plain_profile(question, &intake).is_some());
        for kind in ["extra_field", "year", "country", "filter", "source", "analysis", "summary", "format", "another_subject", "another_capability", "missing", "input", "stale", "invalid_number"] {
            let mut changed = intake.clone();
            let parsed = &mut changed.parsed_request;
            match kind {
                "extra_field" => parsed.capabilities[0].outputs.push("email".into()),
                "year" => { parsed.subjects[0].identifiers.insert("year".into(), "2024".into()); },
                "country" => { parsed.subjects[0].identifiers.insert("country".into(), "Belgium".into()); },
                "filter" => { parsed.subjects[0].identifiers.insert("status".into(), "active".into()); },
                "source" => parsed.capabilities[0].source_hint = Some("overheid.io".into()),
                "analysis" => parsed.deliverable.operations.push("analyze".into()),
                "summary" => parsed.deliverable.operations.push("summarize".into()),
                "format" => parsed.deliverable.format = "json".into(),
                "another_subject" => parsed.subjects.push(parsed.subjects[0].clone()),
                "another_capability" => parsed.capabilities.push(parsed.capabilities[0].clone()),
                "missing" => parsed.capabilities[0].missing.push("company_name".into()),
                "input" => parsed.capabilities[0].inputs.push("year".into()),
                "stale" => parsed.meta.text_sha256 = "different-request".into(),
                _ => { parsed.subjects[0].identifiers.insert("kvk_number".into(), "8756O488".into()); },
            }
            let before = serde_json::to_value(&changed).unwrap();
            assert!(plain_profile(question, &changed).is_none(), "{kind}");
            assert_eq!(serde_json::to_value(&changed).unwrap(), before, "eligibility never rewrites {kind}");
        }
        assert!(plain_profile("Check another company's profile", &intake).is_none());
    }

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
