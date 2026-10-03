//! A bare KVK lookup is answered from its one saved official profile. Optional
//! register fields do not become prerequisites for displaying purchased data.
use crate::{parser::Intent, state::Task};
use anyhow::{ensure, Context};
use serde_json::{json, Value};

const PROVIDER: &str = "kvk-dutch-business-register";
const ENDPOINT: &str = "b050c654-284a-49e8-a7ca-0bfca565816e";
const PATH: &str = "/api/v1/basisprofielen";
const METHOD: &str = "saved_kvk_profile";
const MAX_ROWS: usize = 50;

struct Table<'a> {
    source: &'a Value,
    rows: Vec<Value>,
    shortened: bool,
}

impl Table<'_> {
    fn field(&mut self, label: &str, pointer: &str) {
        let Some(value) = self.source["document"].pointer(pointer) else { return };
        if value.is_null() || value.as_str().is_some_and(|text| text.trim().is_empty()) {
            return;
        }
        if !(value.is_string() || value.is_number() || value.is_boolean()) {
            self.shortened = true;
            return;
        }
        if self.rows.len() == MAX_ROWS {
            self.shortened = true;
            return;
        }
        let mut reference = reference(self.source, pointer);
        if let Some(text) = value.as_str().filter(|text| text.len() > 1024) {
            let end = text.char_indices().map(|(index, _)| index).take_while(|index| *index <= 1024).last().unwrap_or(0);
            reference["quote"] = json!(&text[..end]);
            self.shortened = true;
        }
        self.rows.push(json!([{"label":label}, reference]));
    }

    fn list(&mut self, pointer: &str, limit: usize, fields: &[(&str, &str)]) {
        let Some(items) = self.source["document"].pointer(pointer).and_then(Value::as_array) else { return };
        self.shortened |= items.len() > limit;
        for index in 0..items.len().min(limit) {
            for (label, suffix) in fields {
                self.field(label, &format!("{pointer}/{index}{suffix}"));
            }
        }
    }
}

fn reference(source: &Value, pointer: &str) -> Value {
    json!({"result_ref":source["result_ref"], "pointer":pointer})
}

/// Recognized requests with inconsistent evidence fail closed; other request
/// shapes retain the normal analysis route and their complete requirements.
pub(super) fn compose(question: &str, intent: &Intent, sources: &[Value]) -> anyhow::Result<Option<Value>> {
    let Some(parsed) = crate::capability_intake::company_lookup::parse(question, "saved-kvk-profile") else {
        return Ok(None);
    };
    ensure!(intent.subjects.len() == 1 && intent.goals.len() == 1 && sources.len() == 1
        && intent.unresolved_references.is_empty(), "KVK profile requires one exact saved subject and source");
    let number = &parsed.subjects[0].identifiers["kvk_number"];
    let subject = &intent.subjects[0];
    ensure!(subject.label == *number
        && subject.values.get("company_registry.kvknummer") == Some(&json!(number))
        && subject.values.iter().all(|(key, value)| matches!(key.as_str(), "company_registry.kvknummer" | "company.name") && value == number)
        && intent.answer_operations.iter().all(|operation| operation == "Requested answer format: chat"),
        "KVK profile must preserve the literal registration and answer scope");
    let entity = subject.entity_ref.context("KVK profile has no saved entity")?;
    let goal = &intent.goals[0];
    ensure!(goal.subject == 0 && goal.capability == "company.registry-profile"
        && goal.constraints.len() == 2 && goal.constraints.get("source") == Some(&json!(PROVIDER))
        && goal.constraints.get("endpoint_id") == Some(&json!(ENDPOINT)),
        "KVK profile does not match the approved register operation");
    let source = &sources[0];
    let document = &source["document"];
    ensure!(source["result_ref"].as_str().is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
        && document["subject"]["entity_ref"] == json!(entity)
        && document["source"]["provider"] == PROVIDER && document["source"]["endpoint_id"] == ENDPOINT
        && document["source"]["path"] == PATH && document["data"].is_object()
        && document["data"]["kvkNummer"] == *number,
        "saved KVK profile belongs to a different registration, entity or endpoint");
    let missing = document["missing_fields"].as_array().context("KVK profile lacks coverage metadata")?;
    ensure!(missing.iter().all(Value::is_string)
        && matches!(document["coverage"].as_str(), Some("source_response" | "partial")),
        "KVK profile has unusable source coverage");
    let dutch = parsed.language == "nl";
    let label = |nl, en| if dutch { nl } else { en };
    let mut table = Table { source, rows: vec![], shortened: false };
    for (nl, en, pointer) in [
        ("KVK-nummer", "KVK number", "/data/kvkNummer"),
        ("Geregistreerde naam", "Registered name", "/data/naam"),
        ("Statutaire naam", "Legal name", "/data/statutaireNaam"),
        ("Rechtsvorm", "Legal form", "/data/_embedded/eigenaar/rechtsvorm"),
        ("Uitgebreide rechtsvorm", "Detailed legal form", "/data/_embedded/eigenaar/uitgebreideRechtsvorm"),
        ("RSIN", "RSIN", "/data/_embedded/eigenaar/rsin"),
        ("Aanvang registratie", "Registration start", "/data/materieleRegistratie/datumAanvang"),
        ("Einde registratie", "Registration end", "/data/materieleRegistratie/datumEinde"),
        ("Geregistreerde werkzame personen", "Reported persons employed", "/data/totaalWerkzamePersonen"),
        ("Nummer hoofdvestiging", "Main establishment number", "/data/_embedded/hoofdvestiging/vestigingsnummer"),
    ] { table.field(label(nl, en), pointer); }
    table.list("/data/handelsnamen", 8, &[(label("Handelsnaam", "Trade name"), "/naam")]);
    let mut withheld = false;
    if let Some(addresses) = document.pointer("/data/_embedded/hoofdvestiging/adressen").and_then(Value::as_array) {
        table.shortened |= addresses.len() > 4;
        for (index, address) in addresses.iter().take(4).enumerate() {
            // Unknown shielding metadata is not permission to expose an address.
            if address["indAfgeschermd"] != "Nee" {
                withheld = true;
                continue;
            }
            let name = match address["type"].as_str() {
                Some("bezoekadres") => label("Bezoekadres hoofdvestiging", "Main establishment visiting address"),
                Some("postadres") => label("Postadres hoofdvestiging", "Main establishment postal address"),
                _ => label("Adres hoofdvestiging", "Main establishment address"),
            };
            table.field(name, &format!("/data/_embedded/hoofdvestiging/adressen/{index}/volledigAdres"));
        }
    }
    table.list("/data/sbiActiviteiten", 8, &[
        (label("SBI-code", "SBI code"), "/sbiCode"),
        (label("Geregistreerde activiteit", "Registered activity"), "/sbiOmschrijving"),
        (label("Hoofdactiviteit volgens de bron", "Primary activity as reported"), "/indHoofdactiviteit"),
    ]);
    table.list("/data/_embedded/hoofdvestiging/websites", 4, &[(label("Website volgens de bron", "Website as reported"), "")]);
    let partial = document["coverage"] == "partial" || !missing.is_empty()
        || document["coverage_note"].as_str().is_some_and(|note| !note.trim().is_empty());
    let mut limitations = vec![label("Niet aangeleverde of afgeschermde velden blijven onbekend. Er worden geen waarden aangevuld.",
            "Fields not supplied or withheld remain unknown. No missing values are inferred.")];
    if partial { limitations.push(label("Het opgeslagen resultaat was als onvolledig gemarkeerd. De beschikbare KVK-gegevens worden hieronder getoond; niet aangeleverde velden blijven onbekend.",
        "The saved result was marked incomplete. Available KVK fields are shown below; fields not supplied remain unknown.")); }
    if withheld { limitations.push(label("Afgeschermde adressen en adressen zonder bevestiging dat zij openbaar zijn, worden niet weergegeven.",
        "Withheld addresses and addresses without confirmation that they are public are not displayed.")); }
    if table.shortened { limitations.push(label("De tabel toont een begrensde selectie van de opgeslagen velden; lange teksten zijn ingekort. Het volledige oorspronkelijke bronresultaat blijft beschikbaar.",
        "The table shows a bounded selection of saved fields; long text is shortened. The complete original source result remains available.")); }
    let raw = json!({"observations":[{"text":label("Het opgeslagen Handelsregisterprofiel vermeldt KVK-nummer {{a}}. De beschikbare geregistreerde gegevens staan hieronder.",
        "The saved business register profile lists KVK number {{a}}. Its available registered details are shown below."),
        "evidence":[reference(source,"/data/kvkNummer")]}], "limitations":limitations,
        "presentation":{"kind":"comparison", "title":label("Geregistreerde bedrijfsgegevens", "Registered company details"),
        "columns":[{"label":label("Gegeven", "Field")},{"label":label("Waarde volgens de bron", "Reported value")}], "rows":table.rows}});
    let mut answer = crate::parser::validate_analysis(&raw.to_string(), sources, partial || withheld || table.shortened)?;
    answer["method"] = json!(METHOD);
    Ok(Some(answer))
}

/// Old contract versions required statutaireNaam even for sole proprietors
/// and failed to emit company.profile. Recover only this exact metadata gap,
/// preserving the failed-step receipt and all other failure checks.
pub(super) fn legacy_contract_gap(answer: &Value, task: &Task, sources: &[Value]) -> bool {
    if answer["method"] != METHOD || task.failed_steps.len() != 1 || sources.len() != 1 {
        return false;
    }
    let Some(plan) = task.proposal.as_ref().and_then(|plan| serde_json::from_value::<crate::resolver::Plan>(plan.clone()).ok()) else { return false };
    let [step] = plan.steps.as_slice() else { return false };
    let document = &sources[0]["document"];
    let usable_name = ["/data/naam", "/data/statutaireNaam", "/data/handelsnamen/0/naam"].into_iter()
        .any(|pointer| document.pointer(pointer).and_then(Value::as_str).is_some_and(|name| !name.trim().is_empty()));
    task.completed.contains(&step.key())
        && task.failed_steps.get(&step.key()).is_some_and(|failure| failure["code"] == "incomplete_source")
        && step.service.api_slug == PROVIDER && step.service.endpoint_id.to_string() == ENDPOINT
        && step.service.contract.capability_slug == "company.registry-profile"
        && step.service.contract.version == 7
        && document["source"]["contract_version"] == json!(step.service.contract.version)
        && document["source"]["endpoint_id"] == json!(step.service.endpoint_id)
        && document["subject"]["entity_ref"] == json!(step.entity_ref)
        && document["coverage"] == "partial" && usable_name
        && document["coverage_note"].as_str().is_none_or(|note| note.trim().is_empty())
        && document["missing_fields"].as_array().is_some_and(|fields| !fields.is_empty()
            && fields.iter().all(|field| matches!(field.as_str(), Some("company.name" | "company.profile"))))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fixture(number: &str) -> (String, Intent, Vec<Value>, Task) {
        let question = format!("{number} check dit kvk nummer");
        let catalog = crate::catalog::Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/workflows/catalog.json")).unwrap());
        let parser = crate::parser::Parser::new(reqwest::Client::new(), "http://127.0.0.1:1".into(), String::new(), "fixture".into());
        let intake = parser.ask_intake(&question, &Task::default(), &catalog, None).await.unwrap();
        let mut intent = parser.chat_from_capabilities(&question, None, &Task::default(), &catalog, &intake).await.unwrap().intent.unwrap();
        let mut task = Task { capability_intake: Some(intake), ..Default::default() };
        crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
        let crate::resolver::Resolution::Plan(plan) = crate::resolver::resolve(&catalog, &task, &intent) else { panic!("expected profile plan") };
        let result = uuid::Uuid::new_v4();
        let sources = vec![json!({"result_ref":result,"document":{
            "source":{"provider":PROVIDER,"endpoint_id":ENDPOINT,"path":PATH,"contract_version":7},
            "subject":{"entity_ref":intent.subjects[0].entity_ref,"label":number},
            "coverage":"source_response","missing_fields":[],"data":{
                "kvkNummer":number,"naam":"Example Web Development",
                "handelsnamen":[{"naam":"Example Web Development","volgorde":0},{"naam":"Other trade name","volgorde":1}],
                "totaalWerkzamePersonen":0,
                "sbiActiviteiten":[{"sbiCode":"62100","sbiOmschrijving":"Computer programming","indHoofdactiviteit":"Ja"}],
                "_embedded":{"eigenaar":{"rechtsvorm":"Eenmanszaak"},"hoofdvestiging":{
                    "vestigingsnummer":"000053469038","adressen":[{"type":"bezoekadres","indAfgeschermd":"Nee","volledigAdres":"Example street 17, Example city"}]}}
            }}})];
        task.completed.push(plan.steps[0].key());
        task.active_results.push(result);
        task.result_refs.push(result);
        task.intent = Some(json!(intent));
        task.proposal = Some(json!(plan));
        (question, intent, sources, task)
    }

    #[tokio::test]
    async fn bare_kvk_saved_profile_answers_without_legal_name_and_preserves_zero_and_identifiers() {
        let (question, intent, sources, task) = fixture("00123456").await;
        let before = sources.clone();
        let answer = super::super::reviewed_compose(&question, &intent, &sources).unwrap().unwrap();
        assert_eq!(answer["method"], METHOD);
        assert_eq!(answer["status"], "completed");
        assert!(super::super::reviewed_sources_ready(&answer, &task, &sources));
        let rows = answer["presentation"]["rows"].as_array().unwrap();
        for (pointer, value) in [
            ("/data/kvkNummer", json!("00123456")),
            ("/data/naam", json!("Example Web Development")),
            ("/data/totaalWerkzamePersonen", json!(0)),
            ("/data/handelsnamen/1/naam", json!("Other trade name")),
            ("/data/_embedded/hoofdvestiging/vestigingsnummer", json!("000053469038")),
            ("/data/_embedded/hoofdvestiging/adressen/0/volledigAdres", json!("Example street 17, Example city")),
        ] {
            let cell = rows.iter().map(|row| &row[1]).find(|cell| cell["pointer"] == pointer).unwrap();
            assert_eq!(cell["value"], value);
            assert_eq!(cell["result_ref"], sources[0]["result_ref"]);
        }
        assert_eq!(sources, before, "saved results remain unchanged");
    }

    #[tokio::test]
    async fn bare_kvk_saved_profile_fails_closed_on_scope_or_evidence_changes() {
        let (question, intent, sources, _) = fixture("87560488").await;
        for kind in ["number", "entity", "endpoint", "provider", "path", "result_ref", "coverage", "duplicate"] {
            let mut changed = sources.clone();
            match kind {
                "number" => changed[0]["document"]["data"]["kvkNummer"] = json!("24330087"),
                "entity" => changed[0]["document"]["subject"]["entity_ref"] = json!(uuid::Uuid::new_v4()),
                "endpoint" => changed[0]["document"]["source"]["endpoint_id"] = json!(uuid::Uuid::new_v4()),
                "provider" => changed[0]["document"]["source"]["provider"] = json!("another-provider"),
                "path" => changed[0]["document"]["source"]["path"] = json!("/search"),
                "result_ref" => changed[0]["result_ref"] = json!("unknown"),
                "coverage" => changed[0]["document"]["coverage"] = json!("truncated"),
                _ => changed.push(changed[0].clone()),
            }
            assert!(compose(&question, &intent, &changed).is_err(), "{kind}");
        }
        let mut changed = intent.clone();
        changed.goals[0].constraints.insert("period".into(), json!("2024"));
        assert!(compose(&question, &changed, &sources).is_err());
        changed = intent.clone(); changed.answer_operations.push("Assess credit risk".into());
        assert!(compose(&question, &changed, &sources).is_err());
        changed = intent.clone(); changed.subjects[0].values.insert("company_registry.kvknummer".into(), json!("24330087"));
        assert!(compose(&question, &changed, &sources).is_err());
        assert!(compose(&format!("{question} en geef de omzet"), &intent, &sources).unwrap().is_none());
    }

    #[tokio::test]
    async fn bare_kvk_saved_profile_handles_optional_withheld_and_large_lists() {
        let (question, intent, mut sources, _) = fixture("87560488").await;
        sources[0]["document"]["data"]["handelsnamen"] = json!((0..300).map(|index| json!({"naam":format!("Trade name {index}")})).collect::<Vec<_>>());
        sources[0]["document"]["data"]["_embedded"]["hoofdvestiging"]["adressen"][0]["indAfgeschermd"] = json!("Ja");
        let answer = compose(&question, &intent, &sources).unwrap().unwrap();
        assert_eq!(answer["status"], "partial");
        assert!(answer["presentation"]["rows"].as_array().unwrap().len() <= MAX_ROWS);
        assert!(!answer.to_string().contains("Example street"));
        assert!(answer["limitations"].to_string().contains("begrensde selectie"));
        sources[0]["document"]["data"] = json!({"kvkNummer":"87560488"});
        let answer = compose(&question, &intent, &sources).unwrap().unwrap();
        assert_eq!(answer["presentation"]["rows"].as_array().unwrap().len(), 1);
        assert_eq!(answer["status"], "completed", "optional absent profile fields do not block the saved lookup");
    }

    #[tokio::test]
    async fn bare_kvk_saved_profile_recovers_only_the_exact_legacy_failed_receipt() {
        let (question, intent, mut sources, mut task) = fixture("87560488").await;
        sources[0]["document"]["coverage"] = json!("partial");
        sources[0]["document"]["missing_fields"] = json!(["company.name", "company.profile"]);
        let key = task.completed[0].clone();
        task.failed_steps.insert(key.clone(), json!({"code":"incomplete_source"}));
        task.analysis = Some(json!({"status":"unavailable","observations":[]}));
        let before = serde_json::to_value(&task).unwrap();
        let answer = super::super::reviewed_compose(&question, &intent, &sources).unwrap().unwrap();
        assert_eq!(answer["status"], "partial");
        assert!(super::super::reviewed_sources_ready(&answer, &task, &sources));
        assert_eq!(serde_json::to_value(&task).unwrap(), before, "recovery does not rewrite execution receipts or create a purchase");
        for kind in ["extra_failure", "wrong_failure", "wrong_step", "not_completed", "extra_missing", "scope_note", "no_name", "version_mismatch", "future_version"] {
            let mut changed = task.clone(); let mut saved = sources.clone();
            match kind {
                "extra_failure" => { changed.failed_steps.insert("other".into(), json!({"code":"incomplete_source"})); },
                "wrong_failure" => { changed.failed_steps.insert(key.clone(), json!({"code":"source_unavailable"})); },
                "wrong_step" => { changed.failed_steps.clear(); changed.failed_steps.insert("other".into(), json!({"code":"incomplete_source"})); },
                "not_completed" => changed.completed.clear(),
                "extra_missing" => saved[0]["document"]["missing_fields"] = json!(["company.name", "company.profile", "company_registry.kvknummer"]),
                "scope_note" => saved[0]["document"]["coverage_note"] = json!("This response was truncated"),
                "version_mismatch" => saved[0]["document"]["source"]["contract_version"] = json!(6),
                "future_version" => {
                    saved[0]["document"]["source"]["contract_version"] = json!(8);
                    changed.proposal.as_mut().unwrap()["steps"][0]["service"]["contract"]["version"] = json!(8);
                },
                _ => saved[0]["document"]["data"] = json!({"kvkNummer":"87560488"}),
            }
            assert!(!super::super::reviewed_sources_ready(&answer, &changed, &saved), "{kind}");
        }
    }
}
