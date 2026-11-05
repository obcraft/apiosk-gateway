//! Literal vehicle-registration answers from the purchased Overheid.io record.
//! No model or second provider call is needed to read these reported fields.
use crate::{parser::Intent, profession_request::{recognize_with_language, Request}, state::Task};
use anyhow::{ensure, Context};
use serde_json::{json, Value};

const PROVIDER: &str = "overheid-io";
const ENDPOINT: &str = "4265429e-0332-4397-a6a7-17147b6c9aa0";
const PATH: &str = "/voertuiggegevens/{kenteken}";
const METHOD: &str = "saved_vehicle_registration";

pub(super) fn compose(question: &str, intent: &Intent, sources: &[Value]) -> anyhow::Result<Option<Value>> {
    let Some((language, Request::Vehicle { plate })) = recognize_with_language(question) else { return Ok(None) };
    if !intent.goals.iter().any(|goal| goal.capability == "vehicle.registry-lookup") { return Ok(None) }
    ensure!(intent.subjects.len() == 1 && intent.goals.len() == 1 && sources.len() == 1
        && intent.unresolved_references.is_empty(), "vehicle lookup needs one resolved subject and saved source");
    let subject = &intent.subjects[0];
    let entity = subject.entity_ref.context("missing vehicle identity")?;
    ensure!(subject.values.get("vehicle.registration.nl.plate").and_then(Value::as_str)
        .is_some_and(|value| value.replace('-', "").eq_ignore_ascii_case(&plate)), "requested plate mismatch");
    let goal = &intent.goals[0];
    ensure!(goal.subject == 0 && goal.constraints.get("source") == Some(&json!(PROVIDER))
        && goal.constraints.get("endpoint_id") == Some(&json!(ENDPOINT)), "unreviewed vehicle source");
    let source = &sources[0];
    let doc = &source["document"];
    let data = &doc["data"];
    ensure!(source["result_ref"].as_str().is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
        && doc["subject"]["entity_ref"] == json!(entity)
        && doc["source"]["provider"] == PROVIDER && doc["source"]["endpoint_id"] == ENDPOINT
        && doc["source"]["path"] == PATH && data["kenteken"] == plate
        && matches!(doc["coverage"].as_str(), Some("source_response" | "partial")), "saved vehicle provenance mismatch");
    ensure!(data.pointer("/_links/self/href").is_none_or(|value| value == &format!("/voertuiggegevens/{plate}")), "vehicle self link mismatch");
    let missing = doc["missing_fields"].as_array().context("missing vehicle coverage metadata")?;
    ensure!(missing.iter().all(Value::is_string), "invalid coverage metadata");
    let local = |en, nl| if language == "nl" { nl } else { en };
    let reference = |key: &str| json!({"result_ref":source["result_ref"],"pointer":format!("/data/{key}")});
    let mut observations = vec![];
    let mut limitations = vec![];
    for (label, key) in [
        (local("Registration plate", "Kenteken"), "kenteken"),
        (local("Make", "Merk"), "merk"),
        (local("Model (trade name)", "Model (handelsbenaming)"), "handelsbenaming"),
        (local("First registration (first admission)", "Eerste registratie (eerste toelating)"), "datum_eerste_toelating"),
        (local("First Dutch registration", "Eerste registratie in Nederland"), "datum_eerste_tenaamstelling_in_nederland"),
        (local("APK expiry", "APK-vervaldatum"), "vervaldatum_apk"),
    ] {
        if data[key].as_str().is_some_and(|value| !value.trim().is_empty()) {
            observations.push(json!({"text":format!("{label}: {{{{a}}}}."),"evidence":[reference(key)]}));
        } else {
            limitations.push(format!("{label}: {}", local("not supplied by the saved source.", "niet aangeleverd door de opgeslagen bron.")));
        }
    }
    match data["openstaande_terugroepactie_indicator"].as_bool() {
        Some(value) => observations.push(json!({"text":if value {
            local("The saved RDW record indicates an outstanding recall.", "De opgeslagen RDW-registratie meldt een openstaande terugroepactie.")
        } else {
            local("The saved RDW record indicates no outstanding recall.", "De opgeslagen RDW-registratie meldt geen openstaande terugroepactie.")
        },"evidence":[reference("openstaande_terugroepactie_indicator")]})),
        None => limitations.push(local("Outstanding recall indicator: not supplied by the saved source.", "Openstaande terugroepactie: niet aangeleverd door de opgeslagen bron.").into()),
    }
    let partial = !limitations.is_empty() || missing.iter().any(|field| field != "vehicle.apk_expiry" || !valid_apk(data));
    if let Some(note) = doc["coverage_note"].as_str().filter(|note| !note.trim().is_empty()) { limitations.push(note.into()); }
    limitations.push(local("The recall indicator does not provide recall history or repair details. These fields do not establish a WOK status or the vehicle's condition.",
        "De terugroepindicator bevat geen terugroephistorie of reparatiedetails. Deze velden stellen geen WOK-status of voertuigconditie vast.").into());
    let raw = json!({"observations":observations,"limitations":limitations,"presentation":{"kind":"text"}});
    let mut answer = crate::parser::validate_analysis(&raw.to_string(), sources, partial)?;
    answer["method"] = json!(METHOD);
    Ok(Some(answer))
}

fn valid_apk(data: &Value) -> bool {
    data["vervaldatum_apk"].as_str().is_some_and(|date| ["%Y-%m-%d", "%Y%m%d"].iter()
        .any(|format| chrono::NaiveDate::parse_from_str(date, format).is_ok()))
}

/// Recover the old YYYYMMDD-only contract error, without ignoring other failures.
pub(super) fn legacy_contract_gap(answer: &Value, task: &Task, sources: &[Value]) -> bool {
    if answer["method"] != METHOD || task.failed_steps.len() != 1 || sources.len() != 1 { return false }
    let Some(plan) = task.proposal.as_ref().and_then(|plan| serde_json::from_value::<crate::resolver::Plan>(plan.clone()).ok()) else { return false };
    let [step] = plan.steps.as_slice() else { return false };
    let doc = &sources[0]["document"];
    task.completed.contains(&step.key())
        && task.failed_steps.get(&step.key()).is_some_and(|failure| failure["code"] == "incomplete_source")
        && step.service.api_slug == PROVIDER && step.service.endpoint_id.to_string() == ENDPOINT
        && step.service.path == PATH && step.service.contract.capability_slug == "vehicle.registry-lookup"
        && doc["source"]["endpoint_id"] == json!(step.service.endpoint_id)
        && doc["subject"]["entity_ref"] == json!(step.entity_ref)
        && doc["coverage"] == "partial" && doc["missing_fields"] == json!(["vehicle.apk_expiry"])
        && doc["coverage_note"].as_str().is_none_or(|note| note.trim().is_empty()) && valid_apk(&doc["data"])
}

#[cfg(test)]
mod tests {
    use super::*;
    const QUESTION: &str = "What does the RDW vehicle registration for Dutch plate RZ874R show: make and model, first registration, APK expiry and recall indicators?";
    fn fixture() -> (Intent, Vec<Value>) {
        let entity = uuid::Uuid::new_v4();
        let intent = serde_json::from_value(json!({"subjects":[{"entity_ref":entity,"label":"RZ874R","values":{"vehicle.registration.nl.plate":"RZ874R"}}],
            "goals":[{"capability":"vehicle.registry-lookup","subject":0,"constraints":{"source":PROVIDER,"endpoint_id":ENDPOINT}}],"answer_operations":[],"unresolved_references":[]})).unwrap();
        let sources = vec![json!({"result_ref":uuid::Uuid::new_v4(),"document":{"subject":{"entity_ref":entity,"label":"RZ874R"},
            "source":{"provider":PROVIDER,"path":PATH,"endpoint_id":ENDPOINT},"coverage":"partial","missing_fields":["vehicle.apk_expiry"],
            "data":{"kenteken":"RZ874R","merk":"VOLKSWAGEN","handelsbenaming":"POLO","vervaldatum_apk":"2027-10-29","vervaldatum_apk_dt":null,
                "datum_eerste_toelating":"2018-03-13","datum_eerste_toelating_dt":null,"datum_eerste_tenaamstelling_in_nederland":"2018-03-13","openstaande_terugroepactie_indicator":false}}})];
        (intent, sources)
    }
    #[test]
    fn purchased_registration_answers_all_requested_fields_with_exact_evidence() {
        let (intent, sources) = fixture();
        let answer = super::super::reviewed_compose(QUESTION, &intent, &sources).unwrap().unwrap();
        if let Ok(path) = std::env::var("RDW_ANSWER_PREVIEW") {
            std::fs::write(path, json!({"question":QUESTION,"analysis":answer,"result":{
                "result_ref":sources[0]["result_ref"],"subject":sources[0]["document"]["subject"],
                "source":sources[0]["document"]["source"],"coverage":"source_response","missing_fields":[],
                "charged_atomic":"75000","data":sources[0]["document"]["data"]}}).to_string()).unwrap();
        }
        assert_eq!(answer["status"], "completed");
        assert_eq!(answer["method"], METHOD);
        let text = answer["observations"].to_string();
        for value in ["VOLKSWAGEN", "POLO", "2018-03-13", "2027-10-29", "no outstanding recall"] { assert!(text.contains(value), "{value}: {text}"); }
        for observation in answer["observations"].as_array().unwrap() {
            for fact in observation["evidence"].as_array().unwrap() {
                assert_eq!(sources[0]["document"].pointer(fact["pointer"].as_str().unwrap()).unwrap(), &fact["value"]);
            }
        }
        assert!(!text.contains("awaiting an APK"));
    }
    #[test]
    fn absent_dates_and_recall_remain_unknown_and_identity_mismatches_are_rejected() {
        let (intent, sources) = fixture();
        let mut missing = sources.clone();
        missing[0]["document"]["data"]["vervaldatum_apk"] = Value::Null;
        missing[0]["document"]["data"]["openstaande_terugroepactie_indicator"] = Value::Null;
        let answer = compose(QUESTION, &intent, &missing).unwrap().unwrap();
        assert_eq!(answer["status"], "partial");
        assert!(!answer["observations"].to_string().contains("no outstanding recall"));
        for pointer in ["/document/data/kenteken", "/document/source/provider", "/document/source/endpoint_id", "/document/subject/entity_ref"] {
            let mut wrong = sources.clone();
            *wrong[0].pointer_mut(pointer).unwrap() = json!("other");
            assert!(compose(QUESTION, &intent, &wrong).is_err(), "{pointer}");
        }
        assert!(compose(&format!("{QUESTION} Is it safe to buy?"), &intent, &sources).unwrap().is_none());
    }

    fn catalog() -> crate::catalog::Snapshot {
        let fixture: Value = serde_json::from_str(include_str!("../../tests/fixtures/rdw-registration-planning.json")).unwrap();
        serde_json::from_value(fixture["catalog"].clone()).unwrap()
    }

    #[test]
    fn iso_apk_is_a_fact_under_the_repaired_schema_without_changing_source_values() {
        let (intent, sources) = fixture();
        let old = crate::catalog::Catalog::build(catalog());
        let service = old.services.iter().find(|service| service.endpoint_id.to_string() == ENDPOINT).unwrap();
        let data = &sources[0]["document"]["data"];
        assert!(service.contract.extract(data, &old.vocabulary).unwrap().single().unwrap().fact("vehicle.apk_expiry").is_none());
        let mut snapshot = catalog();
        let sql = include_str!("../../supabase/migrations/20261105232047_accept_rdw_apk_iso_date.sql");
        let pattern: Value = serde_json::from_str(sql.split("USING '").nth(1).unwrap().split("'::jsonb").next().unwrap()).unwrap();
        snapshot.vocabulary.iter_mut().find(|fact| fact.slug == "vehicle.apk_expiry").unwrap().value_schema["pattern"] = pattern;
        let repaired = crate::catalog::Catalog::build(snapshot);
        for date in ["2027-10-29", "20271029"] {
            let mut response = data.clone(); response["vervaldatum_apk"] = json!(date);
            let facts = service.contract.extract(&response, &repaired.vocabulary).unwrap();
            assert_eq!(facts.single().unwrap().fact("vehicle.apk_expiry").unwrap().value, date);
        }
        assert_eq!(intent.subjects.len(), 1);
    }

    #[test]
    fn recovering_the_legacy_apk_error_does_not_ignore_other_failures_or_mutate_receipts() {
        let (intent, sources) = fixture();
        let catalog = crate::catalog::Catalog::build(catalog());
        let service = catalog.services.iter().find(|service| service.endpoint_id.to_string() == ENDPOINT).unwrap().clone();
        let step = crate::resolver::Step { service, entity_ref: intent.subjects[0].entity_ref.unwrap(), goal_index: 0, browser_fallback: None };
        let key = step.key();
        let mut task = Task::default();
        task.proposal = Some(json!({"quote_ref":uuid::Uuid::new_v4(),"steps":[step],"cap_atomic":75000,"expires_at":chrono::Utc::now(),"hash":"saved"}));
        task.completed.push(key.clone());
        task.failed_steps.insert(key.clone(), json!({"code":"incomplete_source"}));
        let before = json!(task);
        let answer = compose(QUESTION, &intent, &sources).unwrap().unwrap();
        assert!(super::super::reviewed_sources_ready(&answer, &task, &sources));
        assert_eq!(json!(task), before);
        for kind in ["other_failure", "uncompleted", "missing_other_field", "truncated", "invalid_apk"] {
            let mut changed = task.clone(); let mut saved = sources.clone();
            match kind {
                "other_failure" => { changed.failed_steps.insert(key.clone(), json!({"code":"source_unavailable"})); },
                "uncompleted" => changed.completed.clear(),
                "missing_other_field" => saved[0]["document"]["missing_fields"] = json!(["vehicle.apk_expiry","vehicle.make"]),
                "truncated" => saved[0]["document"]["coverage_note"] = json!("Truncated response"),
                _ => saved[0]["document"]["data"]["vervaldatum_apk"] = json!("not a date"),
            }
            assert!(!super::super::reviewed_sources_ready(&answer, &changed, &saved), "{kind}");
        }
    }
}
