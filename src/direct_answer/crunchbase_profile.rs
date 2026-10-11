//! Render the official saved profile contract without a generative round trip.
//! Extra analysis operations and unrelated requirements retain model synthesis.
use crate::{answer_blocks::BlockContext, capability_intake::Intake, parser::Intent};
use anyhow::{ensure, Context};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const PROVIDER: &str = "apimarket-magicapi-crunchbase-bbcca0a6";
const ENDPOINT: &str = "9651025a-3f42-4d86-aa83-b0a9c05e28c4";

fn property_path(properties: &Value, base: &str, field: &str, suffix: &str) -> Option<String> {
    let properties = properties.as_array()?;
    let found: Vec<_> = properties
        .iter()
        .enumerate()
        .filter(|(_, p)| p["field_id"] == field)
        .collect();
    let [(index, property)] = found.as_slice() else {
        return None;
    };
    let value = property["value"].pointer(suffix)?;
    (value.is_string() || value.is_number() || value.is_boolean())
        .then(|| format!("{base}/{index}/value{suffix}"))
}

pub(super) fn compose(
    question: &str,
    intent: &Intent,
    sources: &[Value],
    intake: Option<&Intake>,
) -> anyhow::Result<Option<Value>> {
    let Some(intake) = intake else {
        return Ok(None);
    };
    let parsed = &intake.parsed_request;
    let [goal] = intent.goals.as_slice() else {
        return Ok(None);
    };
    let [subject] = intent.subjects.as_slice() else {
        return Ok(None);
    };
    if parsed.meta.text_sha256 != hex::encode(Sha256::digest(question.as_bytes()))
        || goal.capability != "company.crunchbase-profile"
        || goal.subject != 0
        || goal.constraints.get("source") != Some(&json!(PROVIDER))
        || goal.constraints.get("endpoint_id") != Some(&json!(ENDPOINT))
        || goal.constraints.len() != 2
        || !intent.unresolved_references.is_empty()
        || intent
            .answer_operations
            .iter()
            .any(|op| op != "Requested answer format: chat")
        || !matches!(parsed.language.as_str(), "en" | "nl")
        || parsed.deliverable.format != "chat"
        || !parsed.deliverable.operations.is_empty()
        || parsed.subjects.len() != 1
        || parsed.capabilities.is_empty()
        || parsed.capabilities.iter().any(|cap| {
            cap.subject_id.as_ref() != Some(&parsed.subjects[0].id)
                || cap.outputs.is_empty()
                || cap.outputs.iter().any(|output| {
                    !matches!(
                        output.as_str(),
                        "profile"
                            | "company_profile"
                            | "founders"
                            | "employees"
                            | "employee_count"
                            | "board_members"
                            | "funding_rounds"
                            | "investors"
                    )
                })
        })
        || !parsed.capabilities.iter().any(|cap| {
            cap.outputs
                .iter()
                .any(|out| matches!(out.as_str(), "profile" | "company_profile"))
        })
    {
        return Ok(None);
    }
    let [source] = sources else { return Ok(None) };
    let doc = &source["document"];
    let entity = subject
        .entity_ref
        .context("Crunchbase profile missing saved entity")?;
    ensure!(
        doc["subject"]["entity_ref"] == json!(entity)
            && doc["source"]["provider"] == PROVIDER
            && doc["source"]["endpoint_id"] == ENDPOINT
            && doc["source"]["path"] == "/mcp/cb_entity_get"
            && doc["data"]["transport"] == "official_mcp"
            && doc["data"]["tool"] == "cb_entity_get"
            && source["result_ref"]
                .as_str()
                .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok()),
        "Crunchbase profile evidence does not match the approved source and subject"
    );
    let data = &doc["data"]["data"];
    ensure!(
        data["entity_def_id"] == "organization" && data["collection_id"] == "organizations",
        "Crunchbase profile is not an organization"
    );
    let properties = &data["properties"];
    let name = property_path(properties, "/data/data/properties", "identifier", "/value")
        .context("Crunchbase profile has no name")?;
    let id = properties
        .as_array()
        .context("Crunchbase profile has no properties")?
        .iter()
        .find(|p| p["field_id"] == "identifier")
        .context("Crunchbase profile has no identity")?;
    let requested = subject
        .values
        .get("company.crunchbase_entity_id")
        .and_then(Value::as_str)
        .context("Crunchbase profile has no requested identifier")?;
    ensure!(
        id["value"]["permalink"] == requested || id["value"]["uuid"] == requested,
        "Crunchbase returned a different company"
    );
    let dutch = parsed.language == "nl";
    let label = |nl, en| if dutch { nl } else { en };
    let reference =
        |path: String| -> Value { json!({"result_ref":source["result_ref"], "pointer":path}) };
    let field = |key: &str, suffix: &str| {
        property_path(properties, "/data/data/properties", key, suffix).map(&reference)
    };
    let mut observations = vec![
        json!({"text":label("Het opgeslagen Crunchbase-profiel voor {{a}} staat hieronder.", "The saved Crunchbase profile for {{a}} is shown below."), "evidence":[reference(name.clone())]}),
    ];
    if let Some(description) = field("short_description", "") {
        observations.push(json!({"text":"{{a}}", "evidence":[description]}));
    }
    let attributes: Vec<_> = [
        ("Website", "Website", "website_url", ""),
        ("Opgericht op", "Founded on", "founded_on", "/value"),
        (
            "Precisie oprichtingsdatum",
            "Founding date precision",
            "founded_on",
            "/precision",
        ),
        (
            "Medewerkerscategorie volgens de bron",
            "Employee range code reported by source",
            "num_employees_enum",
            "",
        ),
        ("Beursstatus", "IPO status", "ipo_status", ""),
        ("Beurs", "Stock exchange", "stock_exchange_symbol", ""),
        (
            "Totale financiering",
            "Total funding",
            "funding_total",
            "/value",
        ),
        (
            "Valuta financiering",
            "Funding currency",
            "funding_total",
            "/currency",
        ),
        (
            "Laatste financieringsdatum",
            "Last funding date",
            "last_funding_at",
            "",
        ),
    ]
    .into_iter()
    .filter_map(|(nl, en, key, suffix)| {
        field(key, suffix).map(|fact| json!({"label":label(nl,en),"fact":fact}))
    })
    .collect();
    let mut blocks = vec![
        json!({"id":"b1", "kind":"profile", "title":label("Bedrijfsprofiel", "Company profile"),
        "subject_label":subject.label, "subject":0, "name":reference(name), "attributes":attributes}),
    ];
    let cards = data["cards"]
        .as_array()
        .context("Crunchbase profile has no cards")?;
    let mut missing = false;
    let mut limited = false;
    type Column<'a> = (&'a str, &'a str, &'a str, &'a str);
    let groups: &[(&str, &str, &str, &[Column])] = &[
        (
            "founders_lookup",
            "Oprichters",
            "Founders",
            &[("Naam", "Name", "identifier", "/value")],
        ),
        (
            "current_employees_image_list",
            "Medewerkers volgens Crunchbase",
            "Employees reported by Crunchbase",
            &[
                ("Naam", "Name", "person_identifier", "/value"),
                ("Functie", "Title", "title", ""),
            ],
        ),
        (
            "current_advisors_image_list",
            "Bestuur en adviseurs volgens Crunchbase",
            "Board members and advisers reported by Crunchbase",
            &[
                ("Naam", "Name", "person_identifier", "/value"),
                ("Rol", "Role", "job_type", ""),
                ("Begindatum", "Started on", "started_on", "/value"),
                (
                    "Datumprecisie",
                    "Date precision",
                    "started_on",
                    "/precision",
                ),
            ],
        ),
        (
            "funding_rounds_list",
            "Financieringsrondes",
            "Funding rounds",
            &[
                ("Ronde", "Round", "identifier", "/value"),
                ("Datum", "Announced on", "announced_on", ""),
                ("Bedrag", "Amount", "money_raised", "/value"),
                ("Valuta", "Currency", "money_raised", "/currency"),
            ],
        ),
        (
            "investors_list",
            "Investeerders",
            "Investors",
            &[
                ("Investeerder", "Investor", "investor_identifier", "/value"),
                ("Ronde", "Round", "funding_round_identifier", "/value"),
            ],
        ),
    ];
    for (card_id, nl, en, columns) in groups {
        let matches: Vec<_> = cards
            .iter()
            .enumerate()
            .filter(|(_, card)| card["card_id"] == *card_id)
            .collect();
        let [(card_index, card)] = matches.as_slice() else {
            missing = true;
            continue;
        };
        let Some(entities) = card["entities"].as_array() else {
            missing = true;
            continue;
        };
        let partial = card["has_more_entities"] != false || entities.len() > 50;
        limited |= partial;
        if entities.is_empty() {
            missing = true;
            continue;
        }
        let rows: Vec<_> = entities
            .iter()
            .take(50)
            .enumerate()
            .map(|(i, entity)| {
                let base = format!("/data/data/cards/{card_index}/entities/{i}/properties");
                let cells: Vec<_> = columns
                    .iter()
                    .map(|(_, _, key, suffix)| {
                        property_path(&entity["properties"], &base, key, suffix).map(&reference)
                    })
                    .collect();
                json!({"cells":cells})
            })
            .collect();
        let mut notes = vec![];
        if partial {
            notes.push(label(
                "Crunchbase heeft meer vermeldingen; dit zijn alleen de aangeleverde records.",
                "Crunchbase has more entries; only the returned records are shown.",
            ));
        }
        blocks.push(json!({"id":format!("b{}",blocks.len()+1), "kind":"data_table", "title":label(nl,en), "notes":notes,
            "columns":columns.iter().enumerate().map(|(i,(nl,en,_,_))|json!({"id":format!("c{}",i+1),"label":label(nl,en)})).collect::<Vec<_>>(), "rows":rows}));
    }
    let mut limitations = vec![];
    if limited {
        limitations.push(label("De medewerkers- of bestuurslijsten zijn niet volledig. De tabellen tonen uitsluitend de door Crunchbase aangeleverde vermeldingen; rollen zijn niet onafhankelijk geverifieerd.", "The employee or board lists are incomplete. The tables show only the entries returned by Crunchbase; roles have not been independently verified."));
    }
    if missing {
        limitations.push(label("Sommige profielonderdelen zijn niet aangeleverd of bevatten geen vermeldingen. Dit bewijst niet dat er geen personen, financieringsrondes of investeerders zijn.", "Some profile sections were not supplied or contain no entries. This does not establish the absence of people, funding rounds or investors."));
    }
    if doc["data"]["page_enrichment"]["status"] == "unavailable" {
        limitations.push(label("De openbare profielpagina kon niet worden aangevuld. Dit antwoord gebruikt de officiële Crunchbase-gegevens.", "The public profile page could not be enriched. This answer uses the official Crunchbase data."));
    }
    let partial = limited
        || missing
        || doc["coverage"] != "source_response"
        || doc["missing_fields"]
            .as_array()
            .is_none_or(|a| !a.is_empty());
    let mut answer = crate::parser::validate_analysis(
        &json!({"observations":observations,"limitations":limitations}).to_string(),
        sources,
        partial,
    )?;
    let ids: Vec<_> = (1..=observations.len()).map(|n| format!("o{n}")).collect();
    let ctx = BlockContext {
        question,
        sources,
        derivations: &[],
        observation_ids: &ids,
        limitation_count: limitations.len(),
        missing_ids: &[],
        requirements: &[],
        source_status: &[],
        provisional: false,
    };
    let mut outcome = crate::answer_presentation::validate_blocks(&json!(blocks), &ctx);
    ensure!(
        outcome.rejected.is_empty(),
        "Crunchbase profile blocks failed validation: {:?}",
        outcome.rejected
    );
    for block in &mut outcome.blocks {
        block.produced_by = crate::answer_blocks::BlockProducer::Deterministic;
    }
    answer["answer_schema_version"] = json!(2);
    answer["blocks"] = serde_json::to_value(outcome.blocks)?;
    answer["method"] = json!("saved_crunchbase_profile");
    Ok(Some(answer))
}

#[cfg(test)]
mod tests {
    use super::*;
    const QUESTION: &str = "Use Crunchbase to retrieve the full company profile for https://www.crunchbase.com/organization/starbucks, including available founders, employees, board members, funding rounds and investors.";
    fn fixture() -> (Value, Intent, Intake) {
        let v: Value =
            serde_json::from_str(include_str!("crunchbase_profile_fixture.json")).unwrap();
        (
            v["source"].clone(),
            serde_json::from_value(v["intent"].clone()).unwrap(),
            serde_json::from_value(v["intake"].clone()).unwrap(),
        )
    }
    #[test]
    fn saved_profile_is_answered_with_all_returned_people_and_funding_without_a_model() {
        let (source, intent, intake) = fixture();
        let answer = compose(QUESTION, &intent, &[source], Some(&intake))
            .unwrap()
            .unwrap();
        assert_eq!(answer["method"], "saved_crunchbase_profile");
        assert_eq!(answer["status"], "partial");
        let counts: Vec<_> = answer["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|b| b["rows"].as_array().map(Vec::len))
            .collect();
        assert_eq!(counts, vec![3, 8, 8, 4, 3]);
        let text = answer.to_string();
        for name in [
            "Gordon Bowker",
            "Dominic Carr",
            "Cliff Burrows",
            "Pershing Square",
        ] {
            assert!(text.contains(name), "{name}");
        }
        assert!(text.contains("incomplete"));
        assert!(text.contains("1750000000"));
    }
    #[test]
    fn extra_operations_or_requirements_stay_on_the_analysis_path() {
        let (source, mut intent, mut intake) = fixture();
        intent
            .answer_operations
            .push("Compare with competitors".into());
        assert!(compose(QUESTION, &intent, &[source.clone()], Some(&intake))
            .unwrap()
            .is_none());
        intent.answer_operations.pop();
        intake.parsed_request.capabilities[0]
            .outputs
            .push("profitability".into());
        assert!(compose(QUESTION, &intent, &[source], Some(&intake))
            .unwrap()
            .is_none());
    }
    #[test]
    fn mismatched_company_and_source_fail_closed() {
        let (mut source, intent, intake) = fixture();
        source["document"]["source"]["endpoint_id"] = json!("wrong");
        assert!(compose(QUESTION, &intent, &[source], Some(&intake)).is_err());
        let (source, mut intent, intake) = fixture();
        intent.subjects[0].values.insert(
            "company.crunchbase_entity_id".into(),
            json!("other-company"),
        );
        assert!(compose(QUESTION, &intent, &[source], Some(&intake)).is_err());
    }
}
