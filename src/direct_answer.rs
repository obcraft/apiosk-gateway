//! Exact lookup answers from saved, reviewed source fields. Anything outside
//! these narrow question shapes keeps the normal source-analysis path.
use crate::parser::{validate_analysis, Intent};
use serde_json::{json, Value};

pub(crate) fn compose(question: &str, intent: &Intent, sources: &[Value]) -> Option<Value> {
    if intent.subjects.len() != 1
        || intent.goals.len() != 1
        || !intent.unresolved_references.is_empty()
        || sources.len() != 1
    {
        return None;
    }
    let goal = &intent.goals[0];
    if goal.capability != "company.registry-profile"
        || goal.subject != 0
        || goal.constraints.keys().any(|key| key != "source")
    {
        return None;
    }
    let normalize = |text: &str| {
        text.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    let label = normalize(&intent.subjects[0].label);
    let question = normalize(question);
    if label.is_empty() || question.matches(&label).count() != 1 {
        return None;
    }
    let shape = question.replacen(&label, "{subject}", 1);
    let shape = shape.trim_end_matches(['?', '.', '!']).trim();
    let (kind, dutch) =
        match shape {
            "wat is het adres van {subject}" | "wat is het bezoekadres van {subject}" => {
                ("address", true)
            }
            "what is the address of {subject}" | "what is the visiting address of {subject}" => {
                ("address", false)
            }
            "wat is het kvk-nummer van {subject}"
            | "wat is het kvk nummer van {subject}"
            | "wat is het kvknummer van {subject}" => ("registration", true),
            "what is the kvk number of {subject}"
            | "what is the registration number of {subject}" => ("registration", false),
            "wat is de statutaire naam van {subject}"
            | "wat is de officiële naam van {subject}" => ("name", true),
            "what is the legal name of {subject}" | "what is the registered name of {subject}" => {
                ("name", false)
            }
            _ => return None,
        };
    let source = &sources[0];
    let document = &source["document"];
    if document["source"]["provider"] != "kvk-dutch-business-register"
        || document["source"]["path"] != "/api/v1/basisprofielen"
        || document["coverage"] != "source_response"
        || document["missing_fields"]
            .as_array()
            .is_none_or(|fields| !fields.is_empty())
    {
        return None;
    }
    let pointer = match kind {
        "address" => {
            let addresses = document
                .pointer("/data/_embedded/hoofdvestiging/adressen")?
                .as_array()?;
            let visiting: Vec<_> = addresses
                .iter()
                .enumerate()
                .filter(|(_, address)| address["type"] == "bezoekadres")
                .collect();
            if visiting.len() != 1 || visiting[0].1["indAfgeschermd"] != "Nee" {
                return None;
            }
            format!(
                "/data/_embedded/hoofdvestiging/adressen/{}/volledigAdres",
                visiting[0].0
            )
        }
        "registration" => "/data/kvkNummer".into(),
        _ => "/data/statutaireNaam".into(),
    };
    let field = document.pointer(&pointer)?.as_str()?;
    if field.trim().is_empty() || field.len() > 500 {
        return None;
    }
    if kind == "registration" && (field.len() != 8 || !field.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    let text = match (kind, dutch) {
        ("address", true) => {
            "Volgens het Handelsregister is het bezoekadres van de hoofdvestiging {{a}}."
        }
        ("address", false) => {
            "The business register lists the main establishment's visiting address as {{a}}."
        }
        ("registration", true) => "Volgens het Handelsregister is het KVK-nummer {{a}}.",
        ("registration", false) => "The business register lists KVK number {{a}}.",
        (_, true) => "Volgens het Handelsregister is de statutaire naam {{a}}.",
        (_, false) => "The business register lists the legal name as {{a}}.",
    };
    let text = if field.ends_with('.') {
        text.trim_end_matches('.')
    } else {
        text
    };
    // Use the same evidence validator and response shape as model analyses.
    validate_analysis(
        &json!({"observations":[{"text":text,"evidence":[{
        "result_ref":source["result_ref"],"pointer":pointer
    }]}],"limitations":[],"presentation":{"kind":"text"}})
        .to_string(),
        sources,
        false,
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Intent, Vec<Value>) {
        let catalog: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/supplier-check/catalog.json"
        ))
        .unwrap();
        let service = catalog["services"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["contract"]["capability_slug"] == "company.registry-profile")
            .unwrap();
        let intent = serde_json::from_value(
            json!({"subjects":[{"entity_ref":null,"label":"Mollie B.V.","values":{}}],
            "goals":[{"capability":"company.registry-profile","subject":0,"constraints":{}}],
            "answer_operations":["Answer the requested fact"],"unresolved_references":[]}),
        )
        .unwrap();
        (
            intent,
            vec![json!({"result_ref":"saved","document":{
                "source":{"provider":service["api_slug"],"path":service["path"]},
                "data":service["sample_payload"],"coverage":"source_response","missing_fields":[]
            }})],
        )
    }

    #[test]
    fn exact_lookups_keep_source_values_evidence_and_language() {
        let (intent, sources) = fixture();
        for (question, expected) in [
            ("Wat is het adres van Mollie B.V.?", "Volgens het Handelsregister is het bezoekadres van de hoofdvestiging Keizersgracht 126 1015CW Amsterdam."),
            ("Wat is het KVK-nummer van Mollie B.V.?", "Volgens het Handelsregister is het KVK-nummer 30204462."),
            ("What is the legal name of Mollie B.V.?", "The business register lists the legal name as Mollie B.V."),
        ] {
            let answer = compose(question, &intent, &sources).unwrap();
            assert_eq!(answer["observations"][0]["text"], expected);
            assert_eq!(answer["observations"][0]["evidence"][0]["result_ref"], "saved");
            assert_eq!(answer["status"], "completed");
        }
    }

    #[test]
    fn broader_questions_and_incomplete_or_ambiguous_sources_use_analysis() {
        let (intent, sources) = fixture();
        for question in [
            "Wat is het adres van Mollie B.V. en is dit veilig?",
            "Vergelijk Mollie B.V.",
            "Wat was het adres van Mollie B.V.?",
            "Wat is het adres van Mollie B.V. in 2020?",
            "Wat is het postadres van Mollie B.V.?",
            "Wat is het adres van een ander bedrijf?",
        ] {
            assert!(compose(question, &intent, &sources).is_none());
        }
        let question = "Wat is het adres van Mollie B.V.?";
        for mutation in [
            "partial",
            "missing",
            "hidden",
            "multiple",
            "no_address",
            "other_provider",
        ] {
            let mut changed = sources.clone();
            let doc = &mut changed[0]["document"];
            match mutation {
                "partial" => doc["coverage"] = json!("partial"),
                "missing" => doc["missing_fields"] = json!(["company.name"]),
                "hidden" => {
                    doc["data"]["_embedded"]["hoofdvestiging"]["adressen"][0]["indAfgeschermd"] =
                        json!("Ja")
                }
                "multiple" => {
                    let addresses = doc["data"]["_embedded"]["hoofdvestiging"]["adressen"]
                        .as_array_mut()
                        .unwrap();
                    addresses.push(addresses[0].clone());
                }
                "no_address" => doc["data"]["_embedded"]["hoofdvestiging"]["adressen"] = json!([]),
                _ => doc["source"]["provider"] = json!("other"),
            }
            assert!(compose(question, &intent, &changed).is_none(), "{mutation}");
        }
        let mut constrained = intent.clone();
        constrained.goals[0]
            .constraints
            .insert("period".into(), json!("2020"));
        assert!(compose(question, &constrained, &sources).is_none());
        assert!(compose(question, &intent, &[sources[0].clone(), sources[0].clone()]).is_none());
    }
}
