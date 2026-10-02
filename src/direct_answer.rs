//! Exact lookup answers from saved, reviewed source fields. Anything outside
//! these narrow question shapes keeps the normal source-analysis path.
use crate::parser::{validate_analysis, Intent};
use serde_json::{json, Value};

/// A failed synthesis must not hide a successfully saved registry dossier.
/// This conservative fallback presents exact fields, never an AI conclusion.
pub(crate) fn registry_fallback(intent: &Intent, sources: &[Value], dutch: bool) -> Option<Value> {
    if intent.subjects.len() != 1
        || intent.goals.len() != 3
        || !intent.unresolved_references.is_empty()
    {
        return None;
    }
    let entity = intent.subjects[0].entity_ref?;
    let find = |capability: &str, path: &str| -> Option<&Value> {
        let goals: Vec<_> = intent
            .goals
            .iter()
            .filter(|goal| {
                goal.capability == capability
                    && goal.subject == 0
                    && goal.constraints.get("source").and_then(Value::as_str)
                        == Some("companies-house")
            })
            .collect();
        if goals.len() != 1 {
            return None;
        }
        let endpoint = goals[0].constraints.get("endpoint_id")?.as_str()?;
        let found: Vec<_> = sources
            .iter()
            .filter(|source| {
                let doc = &source["document"];
                source["result_ref"].is_string()
                    && doc["subject"]["entity_ref"] == json!(entity)
                    && doc["source"]["provider"] == "companies-house"
                    && doc["source"]["path"] == path
                    && doc["source"]["endpoint_id"] == endpoint
                    && doc["coverage"] == "source_response"
            })
            .collect();
        (found.len() == 1).then(|| found[0])
    };
    let profile = find("company.registry-profile", "/company")?;
    let officers = find("company.officers", "/company/{company_number}/officers")?;
    let psc = find(
        "company.significant-control",
        "/company/{company_number}/persons-with-significant-control",
    )?;
    let company_number = profile["document"]["data"]["company_number"].as_str()?;
    if company_number.is_empty()
        || intent.subjects[0]
            .values
            .get("company.registration.gb.companies_house")
            .is_some_and(|expected| expected.as_str() != Some(company_number))
    {
        return None;
    }
    // The server-owned subject links the plan to each saved result. When a
    // provider also supplies its own company self-link, it must agree.
    for (source, suffix) in [
        (profile, ""),
        (officers, "/officers"),
        (psc, "/persons-with-significant-control"),
    ] {
        if source["document"]["data"]["links"]
            .get("self")
            .is_some_and(|link| {
                link.as_str() != Some(format!("/company/{company_number}{suffix}").as_str())
            })
        {
            return None;
        }
    }
    let rows = officers["document"]["data"]["items"].as_array()?;
    let psc_rows = psc["document"]["data"]["items"].as_array()?;
    let officer_count = officers["document"]["data"]["total_results"].as_u64()?;
    let psc_count = psc["document"]["data"]["total_results"].as_u64()?;
    if officer_count < rows.len() as u64 || psc_count < psc_rows.len() as u64 {
        return None;
    }
    let reference = |source: &Value, pointer: &str| json!({"result_ref":source["result_ref"],"pointer":pointer});
    let mut caveats = vec![
        if dutch {
            "De automatische samenvatting is niet beschikbaar. Hieronder staan uitsluitend gegevens uit de opgeslagen registerbronnen."
        } else {
            "Automatic synthesis is unavailable. The following information is copied only from the saved registry sources."
        },
        if dutch {
            "Een leeg PSC-registerresultaat bewijst niet dat er geen uiteindelijke eigenaren zijn. Ontbrekende velden blijven onbekend."
        } else {
            "An empty PSC register result does not prove there are no ultimate owners. Missing fields remain unknown."
        },
    ];
    if !rows.is_empty() {
        caveats.push(if dutch {"De functionarissentabel bevat de aangeleverde registraties, mogelijk ook van afgetreden functionarissen. Deze herstelweergave past geen aanvullende periode- of statusfilters toe."} else {"The officers table contains the supplied records, which may include former officers. This recovery does not apply additional period or status filters."});
    }
    if rows.len() > 200 || officer_count > rows.len() as u64 {
        caveats.push(if dutch {"De tabel toont een begrensd deel van het opgeslagen registerresultaat en is geen volledige functionarissenhistorie."} else {"The table shows a bounded portion of the saved register result and is not a complete officer history."});
    }
    if !psc_rows.is_empty() {
        caveats.push(if dutch {"De PSC-namen en aard van zeggenschap staan in het opgeslagen PSC-bronresultaat; deze terugvalweergave vat die gegevens niet samen."} else {"PSC names and the nature of control remain in the saved PSC source result; this fallback does not summarize those details."});
    }
    let presentation = if rows.is_empty() {
        json!({"kind":"text"})
    } else {
        json!({"kind":"table",
        "title":if dutch {"Functionarissen in het opgeslagen registerresultaat"} else {"Officers in the saved register result"},
        "result_ref":officers["result_ref"],"rows_pointer":"/data/items","row_indices":(0..rows.len().min(200)).collect::<Vec<_>>(),
        "columns":[{"label":if dutch {"Naam"}else{"Name"},"pointer":"/name"},{"label":if dutch {"Rol"}else{"Role"},"pointer":"/officer_role"},
            {"label":if dutch {"Benoemd op"}else{"Appointed on"},"pointer":"/appointed_on"},{"label":if dutch {"Teruggetreden op"}else{"Resigned on"},"pointer":"/resigned_on"}]})
    };
    let raw = json!({"observations":[
        {"text":if dutch {"De geregistreerde naam is {{a}}, het bedrijfsnummer {{b}}, de status {{c}} en de oprichtingsdatum {{d}}."} else {"The registered name is {{a}}, company number {{b}}, status {{c}}, and incorporation date {{d}}."},
         "evidence":[reference(profile,"/data/company_name"),reference(profile,"/data/company_number"),reference(profile,"/data/company_status"),reference(profile,"/data/date_of_creation")]},
        {"text":if dutch {"De opgeslagen functionarissenbron meldt {{a}} registraties. Benoemings- en terugtredingsdatums blijven weergegeven zoals de bron ze heeft geleverd."} else {"The saved officers source reports {{a}} records. Appointment and resignation dates are shown as supplied by the source."},"evidence":[reference(officers,"/data/total_results")]},
        {"text":if dutch {"Het opgeslagen PSC-registerresultaat meldt {{a}} registraties."} else {"The saved PSC register result reports {{a}} records."},"evidence":[reference(psc,"/data/total_results")]}
    ],"limitations":caveats,"presentation":presentation});
    let mut checked = validate_analysis(&raw.to_string(), sources, true).ok()?;
    checked["fallback"] = json!("saved_registry_fields");
    Some(checked)
}

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

    fn registry_fixture(rows: usize) -> (Intent, Vec<Value>) {
        let entity = uuid::Uuid::new_v4();
        let pairs = [
            ("company.registry-profile", "/company"),
            ("company.officers", "/company/{company_number}/officers"),
            (
                "company.significant-control",
                "/company/{company_number}/persons-with-significant-control",
            ),
        ];
        let goals:Vec<_>=pairs.iter().enumerate().map(|(i,(cap,_))|json!({"capability":cap,"subject":0,"constraints":{"source":"companies-house","endpoint_id":format!("endpoint-{i}")}})).collect();
        let intent=serde_json::from_value(json!({"subjects":[{"label":"Example PLC","entity_ref":entity,"values":{}}],"goals":goals,"answer_operations":[],"unresolved_references":[]})).unwrap();
        let data = [
            json!({"company_name":"Example PLC","company_number":"00000001","company_status":"active","date_of_creation":"1900-01-01"}),
            json!({"total_results":rows,"items":(0..rows).map(|i|json!({"name":format!("Officer {i}"),"officer_role":"director","appointed_on":"2020-01-01"})).collect::<Vec<_>>()}),
            json!({"total_results":0,"items":[]}),
        ];
        let sources=pairs.iter().enumerate().map(|(i,(_,path))|json!({"result_ref":format!("saved-{i}"),"document":{"source":{"provider":"companies-house","name":"Companies House","path":path,"endpoint_id":format!("endpoint-{i}")},"subject":{"entity_ref":entity},"coverage":"source_response","missing_fields":[],"data":data[i]}})).collect();
        (intent, sources)
    }

    #[test]
    fn failed_registry_synthesis_keeps_all_saved_officers_and_exact_evidence() {
        let (intent, sources) = registry_fixture(82);
        let before = sources.clone();
        let answer = registry_fallback(&intent, &sources, true).unwrap();
        assert_eq!(answer["status"], "partial");
        assert_eq!(answer["presentation"]["rows"].as_array().unwrap().len(), 82);
        assert_eq!(answer["presentation"]["rows"][81][0]["value"], "Officer 81");
        assert_eq!(
            answer["presentation"]["rows"][81][0]["pointer"],
            "/data/items/81/name"
        );
        assert_eq!(
            answer["presentation"]["rows"][81][0]["result_ref"],
            "saved-1"
        );
        assert_eq!(
            answer["presentation"]["rows"][0][3],
            Value::Null,
            "absent resignation remains unknown"
        );
        assert_eq!(answer["observations"][2]["evidence"][0]["value"], 0);
        assert!(answer["limitations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap().contains("bewijst niet")));
        assert_eq!(sources, before, "saved results are read-only");
        let (intent, sources) = registry_fixture(205);
        let answer = registry_fallback(&intent, &sources, false).unwrap();
        assert_eq!(
            answer["presentation"]["rows"].as_array().unwrap().len(),
            200
        );
        assert!(answer["limitations"]
            .to_string()
            .contains("bounded portion"));
    }

    #[test]
    fn registry_fallback_refuses_wrong_entity_endpoint_lookup_or_extra_goal() {
        let (intent, sources) = registry_fixture(2);
        for kind in [
            "entity",
            "endpoint",
            "lookup",
            "missing_field",
            "duplicate",
            "provider",
            "company_self_link",
        ] {
            let mut changed = sources.clone();
            match kind {
                "entity" => {
                    changed[1]["document"]["subject"]["entity_ref"] = json!(uuid::Uuid::new_v4())
                }
                "endpoint" => changed[1]["document"]["source"]["endpoint_id"] = json!("another"),
                "lookup" => changed[1]["document"]["source"]["path"] = json!("/search/companies"),
                "missing_field" => {
                    changed[0]["document"]["data"]
                        .as_object_mut()
                        .unwrap()
                        .remove("company_number");
                }
                "duplicate" => changed.push(changed[1].clone()),
                "company_self_link" => {
                    changed[1]["document"]["data"]["links"] =
                        json!({"self":"/company/99999999/officers"})
                }
                _ => changed[1]["document"]["source"]["provider"] = json!("other"),
            }
            assert!(
                registry_fallback(&intent, &changed, false).is_none(),
                "{kind}"
            );
        }
        let mut extra = intent.clone();
        extra.goals.push(extra.goals[0].clone());
        assert!(registry_fallback(&extra, &sources, false).is_none());
        let mut wrong_number = intent;
        wrong_number.subjects[0].values.insert(
            "company.registration.gb.companies_house".into(),
            json!("99999999"),
        );
        assert!(registry_fallback(&wrong_number, &sources, false).is_none());
    }

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
