//! Reviewed, literal public-source lookups. Values and citations always come
//! from the purchased documents, including negative validation results.
use crate::{
    parser::Intent,
    profession_request::{recognize, Request},
};
use anyhow::{ensure, Context};
use serde_json::{json, Value};

fn saved<'a>(
    sources: &'a [Value],
    intent: &Intent,
    capability: &str,
    provider: &str,
    path: &str,
) -> anyhow::Result<&'a Value> {
    let goals: Vec<_> = intent
        .goals
        .iter()
        .filter(|g| g.capability == capability && g.subject == 0)
        .collect();
    ensure!(goals.len() == 1, "expected one reviewed source goal");
    let goal = goals[0];
    ensure!(
        goal.constraints.get("source").and_then(Value::as_str) == Some(provider),
        "unexpected source in reviewed goal"
    );
    let endpoint = goal
        .constraints
        .get("endpoint_id")
        .and_then(Value::as_str)
        .context("missing reviewed endpoint")?;
    let found: Vec<_> = sources
        .iter()
        .filter(|s| s["document"]["source"]["endpoint_id"] == endpoint)
        .collect();
    ensure!(found.len() == 1, "expected one saved source document");
    let source = found[0];
    let doc = &source["document"];
    ensure!(
        source["result_ref"]
            .as_str()
            .is_some_and(|r| uuid::Uuid::parse_str(r).is_ok())
            && doc["subject"]["entity_ref"]
                == json!(intent.subjects[0]
                    .entity_ref
                    .context("missing subject identity")?)
            && doc["source"]["provider"] == provider
            && doc["source"]["path"] == path,
        "saved source provenance does not match the requested subject"
    );
    ensure!(
        matches!(
            doc["coverage"].as_str(),
            Some("source_response" | "partial")
        ),
        "source response is unavailable"
    );
    ensure!(
        doc["missing_fields"]
            .as_array()
            .is_some_and(|x| x.iter().all(Value::is_string)),
        "missing source coverage metadata"
    );
    Ok(source)
}

fn reference(source: &Value, pointer: &str) -> Value {
    json!({"result_ref":source["result_ref"], "pointer":pointer})
}

struct Answer {
    rows: Vec<Value>,
    observations: Vec<Value>,
    limitations: Vec<String>,
}
impl Answer {
    fn new() -> Self {
        Self {
            rows: vec![],
            observations: vec![],
            limitations: vec![],
        }
    }
    fn field(&mut self, source: &Value, label: &str, pointer: &str) {
        let value = source["document"].pointer(pointer);
        let cell = value
            .filter(|v| {
                v.is_boolean()
                    || v.is_number()
                    || v.as_str()
                        .is_some_and(|s| !s.trim().is_empty() && s.trim() != "---")
            })
            .map(|_| reference(source, pointer))
            .unwrap_or(Value::Null);
        self.rows.push(json!([{"label":label},cell]));
    }
    fn strings(&mut self, source: &Value, label: &str, pointer: &str) -> anyhow::Result<()> {
        let values = source["document"]
            .pointer(pointer)
            .and_then(Value::as_array)
            .context("missing saved record list")?;
        ensure!(
            values.len() <= 100 && values.iter().all(Value::is_string),
            "unexpected saved record list"
        );
        for (i, _) in values.iter().enumerate() {
            self.field(
                source,
                &format!("{label} {}", i + 1),
                &format!("{pointer}/{i}"),
            );
        }
        Ok(())
    }
    fn observe(&mut self, source: &Value, text: &str, pointers: &[&str]) {
        self.observations.push(json!({"text":text,"evidence":pointers.iter().map(|p| reference(source,p)).collect::<Vec<_>>()}));
    }
    fn finish(mut self, sources: &[Value], title: &str) -> anyhow::Result<Value> {
        // Provider-specific coverage notices stay visible with the result.
        for source in sources {
            if let Some(note) = source["document"]["coverage_note"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
            {
                if !self.limitations.iter().any(|x| x == note) {
                    self.limitations.push(note.into());
                }
            }
        }
        let raw = json!({"observations":self.observations,"limitations":self.limitations,
            "presentation":{"kind":"comparison","title":title,"columns":[{"label":"Gegeven"},{"label":"Bronwaarde"}],"rows":self.rows}});
        let mut answer = crate::parser::validate_analysis(&raw.to_string(), sources, false)?;
        answer["projection"] = json!("saved_profession_source_fields");
        Ok(answer)
    }
}

pub(crate) fn compose(
    question: &str,
    intent: &Intent,
    sources: &[Value],
) -> anyhow::Result<Option<Value>> {
    let Some(request) = recognize(question) else {
        return Ok(None);
    };
    ensure!(
        intent.subjects.len() == 1 && intent.unresolved_references.is_empty(),
        "reviewed lookup needs a single resolved subject"
    );
    let expected_count = if matches!(request, Request::Domain { .. }) {
        2
    } else {
        1
    };
    ensure!(
        intent.goals.len() == expected_count && sources.len() == expected_count,
        "incomplete reviewed lookup"
    );
    let mut answer = Answer::new();
    let title = match request {
        Request::Vat { country, number } => {
            let (capability, path) = match intent.goals[0].capability.as_str() {
                "vat.eu.check" => (
                    "vat.eu.check",
                    "/taxation_customs/vies/rest-api/check-vat-number",
                ),
                "vat.eu.validate" => (
                    "vat.eu.validate",
                    "/taxation_customs/vies/rest-api/ms/{cc}/vat/{nr}",
                ),
                _ => anyhow::bail!("unreviewed VAT source operation"),
            };
            let source = saved(sources, intent, capability, "vies", path)?;
            let data = &source["document"]["data"]["data"];
            ensure!(
                data["country_code"] == country
                    && data["vat_number"] == number
                    && intent.subjects[0].values.get("vat.eu.country_code")
                        == Some(&json!(country))
                    && intent.subjects[0].values.get("vat.eu.number") == Some(&json!(number)),
                "VAT result identity mismatch"
            );
            let valid = data["valid"]
                .as_bool()
                .context("VIES returned no validity outcome")?;
            chrono::DateTime::parse_from_rfc3339(
                data["request_date"]
                    .as_str()
                    .context("missing VIES check date")?,
            )?;
            answer.observe(
                source,
                if valid {
                    "VIES meldt het gecontroleerde btw-nummer {{a}} als geldig op {{b}}."
                } else {
                    "VIES meldt het gecontroleerde btw-nummer {{a}} als ongeldig op {{b}}."
                },
                &[
                    "/data/data/vat_number",
                    "/data/data/request_date",
                    "/data/data/valid",
                ],
            );
            for (label, key) in [
                ("Landcode", "country_code"),
                ("Btw-nummer", "vat_number"),
                ("Geldig volgens VIES", "valid"),
                ("Controledatum", "request_date"),
                ("Naam volgens VIES", "name"),
                ("Adres volgens VIES", "address"),
            ] {
                answer.field(source, label, &format!("/data/data/{key}"));
            }
            answer.limitations.push("Dit is de VIES-registratiestatus op het controlemoment. Een ongeldig nummer is een controle-uitkomst, geen technische fout. Ontbrekende naam- en adresgegevens blijven onbekend; dit is geen Handelsregisteruittreksel.".into());
            "Btw-validatie via VIES"
        }
        Request::Vehicle { plate } => {
            let source = saved(
                sources,
                intent,
                "vehicle.technical-profile",
                "rdw",
                "/resource/m9d7-ebf2.json",
            )?;
            let data = &source["document"]["data"];
            ensure!(
                intent.subjects[0]
                    .values
                    .get("vehicle.registration.nl.plate")
                    .and_then(Value::as_str)
                    .is_some_and(|value| value.replace('-', "").eq_ignore_ascii_case(&plate))
                    && data["requested_plate"] == plate
                    && data["query"]["kenteken"] == plate
                    && data["coverage"]["complete"] == true,
                "RDW plate or coverage mismatch"
            );
            let items = data["items"].as_array().context("missing RDW rows")?;
            ensure!(
                items.len() <= 1 && data["total"].as_u64() == Some(items.len() as u64),
                "ambiguous RDW result"
            );
            if let Some(row) = items.first() {
                ensure!(row["kenteken"] == plate, "RDW returned another vehicle");
                answer.observe(
                    source,
                    "De RDW heeft een voertuigregistratie teruggegeven voor kenteken {{a}}.",
                    &["/data/items/0/kenteken"],
                );
                for (label, key) in [
                    ("Kenteken", "kenteken"),
                    ("Merk", "merk"),
                    ("Handelsbenaming", "handelsbenaming"),
                    ("Voertuigsoort", "voertuigsoort"),
                    ("APK-vervaldatum", "vervaldatum_apk_dt"),
                    ("APK-datum (broncode)", "vervaldatum_apk"),
                    (
                        "Openstaande terugroepactie",
                        "openstaande_terugroepactie_indicator",
                    ),
                    ("Eerste toelating", "datum_eerste_toelating_dt"),
                    ("Kleur", "eerste_kleur"),
                    ("Aantal zitplaatsen", "aantal_zitplaatsen"),
                ] {
                    answer.field(source, label, &format!("/data/items/0/{key}"));
                }
            } else {
                answer.observe(
                    source,
                    "De RDW heeft geen voertuigregistratie teruggegeven voor kenteken {{a}}.",
                    &["/data/requested_plate"],
                );
            }
            answer.field(source, "RDW-bron", "/data/source_url");
            answer.limitations.push("Dit zijn technische registergegevens, geen eigendomsgegevens of oordeel over de staat of veiligheid van het voertuig. De terugroepindicator bevat geen terugroephistorie of reparatiedetails. Ontbrekende velden blijven onbekend.".into());
            "RDW-voertuiggegevens"
        }
        Request::Ecb { quote } => {
            let source = saved(
                sources,
                intent,
                "fx.ecb-reference-rate",
                "frankfurter",
                "/v2/providers/ecb/rate/EUR/{quote}",
            )?;
            let data = &source["document"]["data"];
            ensure!(
                data["base"] == "EUR"
                    && data["quote"] == quote
                    && intent.subjects[0].values.get("fx.ecb.quote") == Some(&json!(quote)),
                "ECB currency pair mismatch"
            );
            ensure!(
                data["rate"]
                    .as_f64()
                    .is_some_and(|r| r.is_finite() && r > 0.0),
                "invalid ECB reference rate"
            );
            chrono::NaiveDate::parse_from_str(
                data["date"]
                    .as_str()
                    .context("missing ECB observation date")?,
                "%Y-%m-%d",
            )?;
            answer.observe(
                source,
                "De ECB-referentiekoers voor {{a}} naar {{b}} is {{c}} op waarnemingsdatum {{d}}.",
                &["/data/base", "/data/quote", "/data/rate", "/data/date"],
            );
            for (label, key) in [
                ("Basisvaluta", "base"),
                ("Doelvaluta", "quote"),
                ("Referentiekoers per euro", "rate"),
                ("Waarnemingsdatum", "date"),
            ] {
                answer.field(source, label, &format!("/data/{key}"));
            }
            answer.limitations.push("Dagelijkse ECB-referentiekoers via Frankfurter, geen live handelskoers. Op weekenden en feestdagen kan de meest recente waarneming van een eerdere werkdag zijn. ECB-data is gratis bij de ECB; Apiosk rekent voor het ophalen.".into());
            "ECB-referentiewisselkoers"
        }
        Request::Domain { domain } => {
            ensure!(
                intent.subjects[0].values.get("domain.name") == Some(&json!(domain)),
                "domain input mismatch"
            );
            let registration = saved(
                sources,
                intent,
                "domain.registration",
                "domain-intelligence",
                "/domain/{domain}",
            )?;
            let dns = saved(
                sources,
                intent,
                "domain.mail-dns",
                "domain-intelligence",
                "/dns/{domain}",
            )?;
            for source in [registration, dns] {
                ensure!(
                    source["document"]["data"]["data"]["domain"] == domain
                        && source["document"]["data"]["coverage"]["complete"] == true,
                    "domain response identity or coverage mismatch"
                );
            }
            answer.observe(
                registration,
                "De openbare RDAP-registratie betreft domein {{a}}.",
                &["/data/data/domain"],
            );
            for (label, key) in [
                ("Domein", "domain"),
                ("Geregistreerd op", "registered_at"),
                ("Laatst gewijzigd", "last_changed_at"),
                ("Verloopt op", "expires_at"),
                ("Registrar", "registrar"),
                ("RDAP-server", "rdap_server"),
            ] {
                answer.field(registration, label, &format!("/data/data/{key}"));
            }
            answer.strings(registration, "Registratiestatus", "/data/data/statuses")?;
            answer.strings(registration, "Nameserver", "/data/data/nameservers")?;
            answer.field(registration, "RDAP-bron", "/data/source_url");
            let mail = &dns["document"]["data"]["data"];
            ensure!(
                mail["exists"].is_boolean()
                    && mail["accepts_mail"].is_boolean()
                    && mail["dnssec_validated"].is_boolean(),
                "missing DNS indicators"
            );
            answer.observe(
                dns,
                "De mail-DNS-bron rapporteert voor {{a}} de indicator voor een gevonden MX-mailserver als {{b}}.",
                &["/data/data/domain", "/data/data/accepts_mail"],
            );
            for (label, key) in [
                ("DNS bestaat volgens bron", "exists"),
                ("MX-mailserver gevonden", "accepts_mail"),
                ("DNSSEC gevalideerd door resolver", "dnssec_validated"),
            ] {
                answer.field(dns, label, &format!("/data/data/{key}"));
            }
            answer.strings(dns, "MX-record", "/data/data/mx")?;
            answer.field(dns, "Mail-DNS-bron", "/data/source_url");
            answer.limitations.push("Dit zijn openbare registratie- en MX-records op het opvraagmoment. Ze bewijzen geen eigendom of identiteit van de houder en testen geen daadwerkelijke e-mailbezorging. Een expliciet null-MX-record betekent dat het domein volgens DNS geen e-mail accepteert.".into());
            "Openbare domeinregistratie en mail-DNS"
        }
    };
    Ok(Some(answer.finish(sources, title)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(name: &str) -> (String, Intent, Vec<Value>) {
        let text = match name {
            "vat" => include_str!("../tests/fixtures/profession-examples/vat.json"),
            _ => include_str!("../tests/fixtures/profession-examples/vehicle.json"),
        };
        let value: Value = serde_json::from_str(text).unwrap();
        (
            value["question"].as_str().unwrap().into(),
            serde_json::from_value(value["intent"].clone()).unwrap(),
            value["results"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| json!({"result_ref":r["result_ref"],"document":r}))
                .collect(),
        )
    }
    fn row<'a>(answer: &'a Value, label: &str) -> &'a Value {
        &answer["presentation"]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r[0]["value"] == label)
            .unwrap()[1]
    }
    fn assert_exact_citations(value: &Value, sources: &[Value]) {
        if let Some(pointer) = value["pointer"].as_str() {
            let source = sources
                .iter()
                .find(|s| s["result_ref"] == value["result_ref"])
                .unwrap();
            assert_eq!(
                &value["value"],
                source["document"].pointer(pointer).unwrap()
            );
        }
        match value {
            Value::Object(o) => {
                for child in o.values() {
                    assert_exact_citations(child, sources);
                }
            }
            Value::Array(a) => {
                for child in a {
                    assert_exact_citations(child, sources);
                }
            }
            _ => {}
        }
    }
    #[test]
    fn actual_paid_negative_vat_is_a_completed_check_with_unknown_identity() {
        let (q, intent, sources) = fixture("vat");
        let answer = compose(&q, &intent, &sources).unwrap().unwrap();
        assert_eq!(answer["status"], "completed");
        assert_eq!(row(&answer, "Geldig volgens VIES")["value"], false);
        assert!(row(&answer, "Naam volgens VIES").is_null());
        assert_eq!(row(&answer, "Btw-nummer")["value"], "002033354B01");
        assert!(answer["observations"][0]["text"]
            .as_str()
            .unwrap()
            .contains("ongeldig"));
        assert_exact_citations(&answer, &sources);
    }
    #[test]
    fn actual_paid_vehicle_keeps_identifier_apk_and_recall_separate() {
        let (q, intent, sources) = fixture("vehicle");
        let answer = compose(&q, &intent, &sources).unwrap().unwrap();
        assert_eq!(answer["status"], "completed");
        assert_eq!(row(&answer, "Kenteken")["value"], "GZ805N");
        assert_eq!(row(&answer, "Merk")["value"], "SUZUKI");
        assert_eq!(row(&answer, "Handelsbenaming")["value"], "CELERIO");
        assert_eq!(
            row(&answer, "APK-vervaldatum")["value"],
            "2027-09-19T00:00:00.000"
        );
        assert_eq!(row(&answer, "Openstaande terugroepactie")["value"], "Nee");
        assert_exact_citations(&answer, &sources);
    }
    #[test]
    fn source_identity_endpoint_and_duplicate_results_fail_closed() {
        for name in ["vat", "vehicle"] {
            let (q, intent, sources) = fixture(name);
            for pointer in [
                "/subject/entity_ref",
                "/source/provider",
                "/source/endpoint_id",
                "/source/path",
            ] {
                let mut bad = sources.clone();
                *bad[0]["document"].pointer_mut(pointer).unwrap() = json!("wrong");
                assert!(compose(&q, &intent, &bad).is_err(), "{name} {pointer}");
            }
            let mut duplicate = sources.clone();
            duplicate.push(sources[0].clone());
            assert!(compose(&q, &intent, &duplicate).is_err());
            assert!(
                compose(&format!("{q} Geef ook de kredietscore."), &intent, &sources)
                    .unwrap()
                    .is_none()
            );
        }
    }
    #[test]
    fn source_values_cannot_be_swapped_between_vats_or_plates() {
        for (name, pointer) in [
            ("vat", "/data/data/vat_number"),
            ("vat", "/data/data/country_code"),
            ("vehicle", "/data/items/0/kenteken"),
            ("vehicle", "/data/requested_plate"),
        ] {
            let (q, intent, mut sources) = fixture(name);
            *sources[0]["document"].pointer_mut(pointer).unwrap() = json!("different");
            assert!(compose(&q, &intent, &sources).is_err());
        }
    }
    #[test]
    fn missing_validation_is_not_an_invalid_vat_and_missing_apk_stays_unknown() {
        let (q, intent, mut sources) = fixture("vat");
        sources[0]["document"]["data"]["data"]["valid"] = Value::Null;
        assert!(compose(&q, &intent, &sources).is_err());
        let (q, intent, mut sources) = fixture("vehicle");
        sources[0]["document"]["data"]["items"][0]
            .as_object_mut()
            .unwrap()
            .remove("vervaldatum_apk_dt");
        let answer = compose(&q, &intent, &sources).unwrap().unwrap();
        assert!(row(&answer, "APK-vervaldatum").is_null());
    }
    #[test]
    fn empty_rdw_lookup_is_not_an_invented_vehicle() {
        let (q, intent, mut sources) = fixture("vehicle");
        sources[0]["document"]["data"]["items"] = json!([]);
        sources[0]["document"]["data"]["total"] = json!(0);
        let answer = compose(&q, &intent, &sources).unwrap().unwrap();
        assert!(answer["observations"][0]["text"]
            .as_str()
            .unwrap()
            .contains("geen voertuigregistratie"));
        assert_exact_citations(&answer, &sources);
    }

    #[test]
    fn ecb_preserves_pair_date_and_rate_and_rejects_another_currency() {
        let entity = uuid::Uuid::new_v4();
        let catalog: Value =
            serde_json::from_str(include_str!("../tests/fixtures/frankfurter/catalog.json"))
                .unwrap();
        let service = &catalog["services"][0];
        let intent:Intent=serde_json::from_value(json!({"subjects":[{"label":"EUR/USD","entity_ref":entity,"values":{"fx.ecb.quote":"USD"}}],
            "goals":[{"subject":0,"capability":"fx.ecb-reference-rate","constraints":{"source":"frankfurter","endpoint_id":service["endpoint_id"]}}],
            "answer_operations":[],"unresolved_references":[]})).unwrap();
        let mut sources = vec![
            json!({"result_ref":uuid::Uuid::new_v4(),"document":{"subject":{"entity_ref":entity},"source":{"name":"Frankfurter","provider":"frankfurter","endpoint_id":service["endpoint_id"],"path":service["path"]},"coverage":"source_response","missing_fields":[],
            "data":serde_json::from_str::<Value>(include_str!("../tests/fixtures/frankfurter/eur-usd.json")).unwrap()}}),
        ];
        let question="Geef de laatste officiële ECB-referentiewisselkoers voor EUR/USD met waarnemingsdatum via Frankfurter.";
        let answer = compose(question, &intent, &sources).unwrap().unwrap();
        assert_eq!(row(&answer, "Referentiekoers per euro")["value"], 1.1298);
        assert_eq!(row(&answer, "Waarnemingsdatum")["value"], "2026-10-01");
        assert_exact_citations(&answer, &sources);
        sources[0]["document"]["data"]["quote"] = json!("GBP");
        assert!(compose(question, &intent, &sources).is_err());
    }

    #[test]
    fn domain_keeps_exact_null_mx_and_unknown_registration_expiry() {
        let entity = uuid::Uuid::new_v4();
        let fixture: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/domain-intelligence/services.json"
        ))
        .unwrap();
        let services = fixture["services"].as_array().unwrap();
        let intent:Intent=serde_json::from_value(json!({"subjects":[{"label":"example.org","entity_ref":entity,"values":{"domain.name":"example.org"}}],
            "goals":services.iter().map(|s| json!({"subject":0,"capability":s["contract"]["capability_slug"],"constraints":{"source":s["api_slug"],"endpoint_id":s["endpoint_id"]}})).collect::<Vec<_>>(),
            "answer_operations":[],"unresolved_references":[]})).unwrap();
        // Synthetic identity substitution tests the renderer; paid production
        // evidence is collected separately before publishing the example.
        let mut sources=services.iter().map(|s| { let mut payload=s["sample_payload"].clone(); payload["data"]["domain"]=json!("example.org");
            json!({"result_ref":uuid::Uuid::new_v4(),"document":{"subject":{"entity_ref":entity},"source":{"name":s["name"],"provider":s["api_slug"],"endpoint_id":s["endpoint_id"],"path":s["path"]},"coverage":"source_response","missing_fields":[],"data":payload}})
        }).collect::<Vec<_>>();
        sources[1]["document"]["data"]["data"]["mx"] = json!(["0 ."]);
        sources[1]["document"]["data"]["data"]["accepts_mail"] = json!(false);
        let question="Geef openbare RDAP-registratiegegevens en mail-DNS-records voor example.org. Citeer de bronnen.";
        let answer = compose(question, &intent, &sources).unwrap().unwrap();
        assert_eq!(row(&answer, "MX-record 1")["value"], "0 .");
        assert_eq!(row(&answer, "MX-mailserver gevonden")["value"], false);
        assert!(row(&answer, "Verloopt op").is_null());
        assert_exact_citations(&answer, &sources);
        sources[1]["document"]["data"]["data"]["domain"] = json!("other.org");
        assert!(compose(question, &intent, &sources).is_err());
    }
}
