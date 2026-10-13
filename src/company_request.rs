//! Whole, literal requests for the registered profile and available annual
//! figures of an explicitly identified Dutch company. This does not select a
//! filing or claim that the available figures are the latest filed accounts.
use crate::capability_intake::{Capability, Deliverable, Meta, ParsedRequest, Subject};
use anyhow::{ensure, Context};
#[cfg(test)]
use crate::capability_intake::Intake;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const PREFIX: &str = "Find the registered details and available annual financial figures for ";

/// Recognize the complete supported sentence, including its explicit KVK
/// number. Extra requirements fall through to ordinary strict interpretation.
pub(crate) fn parse(question: &str, model: &str) -> Option<ParsedRequest> {
    let text = crate::capability_intake::literal_sentence(question);
    let text = text
        .strip_suffix('.')
        .unwrap_or(text)
        .trim();
    let prefix = text.get(..PREFIX.len())?;
    if !prefix.eq_ignore_ascii_case(PREFIX) {
        return None;
    }
    let body = text.get(PREFIX.len()..)?.strip_suffix(')')?;
    let (label, identifier) = body.rsplit_once(" (")?;
    let (kind, number) = identifier.split_once(' ')?;
    if !kind.eq_ignore_ascii_case("KVK")
        || number.len() != 8
        || !number.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let label = literal_name(label)?;
    let subject = Subject {
        id: "s1".into(),
        label,
        kind: "company".into(),
        identifiers: BTreeMap::from([("kvk_number".into(), number.into())]),
    };
    let capability = |slug: &str, name: &str, description: &str, output: &str| Capability {
        slug: slug.into(),
        name: name.into(),
        description: description.into(),
        domain: "company".into(),
        inputs: vec!["kvk_number".into()],
        outputs: vec![output.into()],
        subject_id: Some(subject.id.clone()),
        source_hint: Some("kvk".into()),
        confidence: 1.0,
        missing: vec![],
    };
    Some(ParsedRequest {
        language: "en".into(),
        capabilities: vec![
            capability("company.registry.profile", "Company registered details",
                "Retrieve the available official registered company profile for the supplied KVK number. Preserve the source's registered name and available details; do not invent missing fields.", "profile"),
            capability("company.financial.statements", "Available annual financial figures",
                "Retrieve available annual financial figures for the supplied KVK number. Preserve the returned financial year, units, currency, source, consolidation scope and missing fields. Availability does not establish the latest filing or a complete original annual report.", "financial_statements"),
        ],
        subjects: vec![subject],
        deliverable: Deliverable { format: "chat".into(), operations: vec![] },
        meta: Meta { model: model.into(), parsed_at: chrono::Utc::now().to_rfc3339(),
            text_sha256: hex::encode(Sha256::digest(question.as_bytes())) },
    })
}

/// Return the canonical request only when the displayed intake retains its
/// complete subject, identifier and scope. This is permission to prepare a
/// normal checked plan, never permission to execute a provider call.
#[cfg(test)]
pub(crate) fn executable_intake(question: &str, intake: &Intake) -> Option<ParsedRequest> {
    let expected = parse(question, "literal-company-request")?;
    let actual = &intake.parsed_request;
    if actual.language != expected.language
        || serde_json::to_value(&actual.subjects).ok()?
            != serde_json::to_value(&expected.subjects).ok()?
        || serde_json::to_value(&actual.capabilities).ok()?
            != serde_json::to_value(&expected.capabilities).ok()?
        || serde_json::to_value(&actual.deliverable).ok()?
            != serde_json::to_value(&expected.deliverable).ok()?
    {
        return None;
    }
    Some(expected)
}

pub(crate) fn literal_name(value: &str) -> Option<String> {
    let value = value.trim();
    let quoted = value
        .strip_prefix('"')
        .and_then(|name| name.strip_suffix('"'));
    let name = quoted.unwrap_or(value);
    if name.is_empty()
        || name.chars().count() > 250
        || !name.chars().any(char::is_alphabetic)
        || !name.chars().all(|ch| {
            ch.is_alphanumeric() || matches!(ch, ' ' | '.' | ',' | '&' | '\'' | '’' | '-' | '/')
        })
    {
        return None;
    }
    if quoted.is_none() {
        // The explicit KVK identifier delimits the supplied company name.
        // Trade names need no legal-form suffix. Additional instructions still
        // belong to the full parser instead of becoming part of that name.
        let lower = name.to_lowercase();
        // A terminal dot (including B.V.) is part of the literal name. An
        // internal sentence boundary is ambiguous; quoted names can retain it.
        let words: Vec<_> = name.split_whitespace().collect();
        if words.windows(2).any(|pair| pair[0].ends_with('.')) {
            return None;
        }
        // Ambiguous clauses belong to the full parser even when someone
        // appends another legal-form suffix after the requested detail.
        if lower.split(|ch: char| !ch.is_alphanumeric()).any(|word| {
            matches!(
                word,
                "including"
                    | "include"
                    | "with"
                    | "without"
                    | "via"
                    | "using"
                    | "before"
                    | "after"
                    | "since"
                    | "during"
                    | "between"
                    | "latest"
                    | "newest"
                    | "recent"
                    | "auditor"
                    | "ownership"
                    | "owners"
                    | "pdf"
                    | "json"
                    | "xlsx"
                    | "csv"
                    | "show"
                    | "return"
                    | "compare"
                    | "summarize"
                    | "summarise"
                    | "for"
            )
        }) {
            return None;
        }
    }
    Some(name.into())
}

const PROVIDER: &str = "kvk-dutch-business-register";
const PROFILE_ENDPOINT: &str = "b050c654-284a-49e8-a7ca-0bfca565816e";
const FIGURES_ENDPOINT: &str = "a4d6eb45-a960-4f50-9298-a91b8063e374";

fn saved<'a>(
    sources: &'a [Value],
    entity: uuid::Uuid,
    endpoint: &str,
    path: &str,
) -> anyhow::Result<&'a Value> {
    let found: Vec<_> = sources
        .iter()
        .filter(|source| source["document"]["source"]["endpoint_id"] == endpoint)
        .collect();
    ensure!(
        found.len() == 1,
        "company answer needs one saved result for each reviewed KVK endpoint"
    );
    let source = found[0];
    let document = &source["document"];
    ensure!(
        source["result_ref"]
            .as_str()
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
            && document["subject"]["entity_ref"] == json!(entity)
            && document["source"]["provider"] == PROVIDER
            && document["source"]["path"] == path,
        "company result does not belong to the supplied subject and KVK endpoint"
    );
    ensure!(
        matches!(
            document["coverage"].as_str(),
            Some("source_response" | "partial")
        ) && document["missing_fields"]
            .as_array()
            .is_some_and(|fields| fields.iter().all(Value::is_string)),
        "company result has no usable saved coverage metadata"
    );
    ensure!(
        document.get("coverage_note").is_none_or(
            |note| note.is_null() || note.as_str().is_some_and(|text| text.trim().is_empty())
        ),
        "company result has an additional scope limitation that must not be hidden"
    );
    Ok(source)
}

fn reference(source: &Value, pointer: &str) -> Value {
    json!({"result_ref":source["result_ref"],"pointer":pointer})
}

fn row(rows: &mut Vec<Value>, source: &Value, label: &str, pointer: &str) -> anyhow::Result<()> {
    let cell = match source["document"].pointer(pointer) {
        Some(value) if !value.is_null() => {
            ensure!(
                value.is_string() || value.is_number() || value.is_boolean(),
                "company display field is not scalar"
            );
            if value.as_str().is_some_and(|text| text.trim().is_empty()) {
                Value::Null
            } else {
                reference(source, pointer)
            }
        }
        _ => Value::Null,
    };
    rows.push(json!([{"label":label}, cell]));
    Ok(())
}

struct FinancialField {
    key: String,
    pointer: String,
    section: Option<&'static str>,
    section_pointer: Option<String>,
}

fn financial_fields(
    value: &Value,
    pointer: &str,
    section: Option<&'static str>,
    section_pointer: Option<&str>,
    depth: usize,
    fields: &mut Vec<FinancialField>,
) -> anyhow::Result<()> {
    ensure!(
        depth <= 16 && fields.len() <= 20_000,
        "annual figures exceed reviewed display capacity"
    );
    let children = value
        .as_array()
        .context("annual figures need an opendataFields array")?;
    for (index, child) in children.iter().enumerate() {
        let at = format!("{pointer}/{index}");
        let object = child
            .as_object()
            .context("annual figure field is not an object")?;
        ensure!(
            object
                .keys()
                .all(|key| matches!(key.as_str(), "key" | "value" | "opendataFields")),
            "annual figure has unreviewed value context"
        );
        let key = child["key"].as_str().context("annual figure has no key")?;
        let (section, own_pointer) = match key {
            "BalanceSheet" => (Some("Balance sheet"), Some(format!("{at}/key"))),
            "ConsolidatedBalanceSheet" => (
                Some("Consolidated balance sheet"),
                Some(format!("{at}/key")),
            ),
            "IncomeStatement" => (Some("Income statement"), Some(format!("{at}/key"))),
            "ConsolidatedIncomeStatement" => (
                Some("Consolidated income statement"),
                Some(format!("{at}/key")),
            ),
            "CashFlowStatement" => (Some("Cash flow statement"), Some(format!("{at}/key"))),
            "ConsolidatedCashFlowStatement" => (
                Some("Consolidated cash flow statement"),
                Some(format!("{at}/key")),
            ),
            _ => (section, section_pointer.map(str::to_owned)),
        };
        if let Some(value) = child.get("value") {
            ensure!(
                value.is_null() || value.is_string() || value.is_number() || value.is_boolean(),
                "annual figure has nonscalar value context"
            );
            fields.push(FinancialField {
                key: key.into(),
                pointer: format!("{at}/value"),
                section,
                section_pointer: own_pointer.clone(),
            });
        }
        if let Some(children) = child.get("opendataFields") {
            financial_fields(
                children,
                &format!("{at}/opendataFields"),
                section,
                own_pointer.as_deref(),
                depth + 1,
                fields,
            )?;
        }
    }
    Ok(())
}

fn figure_label(key: &str) -> Option<&'static str> {
    Some(match key {
        "Assets" => "Total assets",
        "Equity" => "Equity",
        "Revenue" => "Revenue",
        "ResultAfterTax" => "Result after tax",
        "ResultForTheYear" => "Result for the year",
        "AssetsCurrent" => "Current assets",
        "AssetsNoncurrent" => "Non-current assets",
        "Liabilities" => "Liabilities",
        "LiabilitiesCurrent" => "Current liabilities",
        "LiabilitiesNoncurrent" => "Non-current liabilities",
        "EquityAndLiabilities" => "Equity and liabilities",
        "CashFlowOperatingActivities" => "Cash flow from operating activities",
        "CashFlowInvestingActivities" => "Cash flow from investing activities",
        "CashFlowFinancingActivities" => "Cash flow from financing activities",
        _ => return None,
    })
}

fn metadata_label(key: &str) -> Option<&'static str> {
    Some(match key.to_ascii_lowercase().as_str() {
        "currency" => "Currency",
        "unit" => "Unit",
        "scale" => "Scale",
        "decimals" => "Decimals",
        "consolidationstatus" => "Consolidation status",
        "consolidated" => "Consolidated",
        _ => return None,
    })
}

fn decimal(value: &Value) -> bool {
    if value.is_number() {
        return true;
    }
    let Some(text) = value.as_str() else {
        return false;
    };
    let unsigned = text.strip_prefix(['-', '+']).unwrap_or(text);
    let (integer, fraction) = unsigned
        .split_once('.')
        .map_or((unsigned, None), |(integer, fraction)| {
            (integer, Some(fraction))
        });
    !integer.is_empty()
        && integer.bytes().all(|byte| byte.is_ascii_digit())
        && fraction.is_none_or(|fraction| {
            !fraction.is_empty() && fraction.bytes().all(|byte| byte.is_ascii_digit())
        })
}

/// Only saved native KVK responses can supply the answer. `None` means another
/// question shape; a recognized request with mismatched or incomplete evidence
/// returns an error so the caller cannot replace it with model guesses.
pub(crate) fn compose(
    question: &str,
    intent: &crate::parser::Intent,
    sources: &[Value],
) -> anyhow::Result<Option<Value>> {
    let Some(parsed) = parse(question, "literal-company-answer") else {
        return Ok(None);
    };
    ensure!(
        intent.subjects.len() == 1
            && intent.goals.len() == 2
            && sources.len() == 2
            && intent.unresolved_references.is_empty(),
        "company answer requires both complete saved company branches"
    );
    let subject = &intent.subjects[0];
    let expected = &parsed.subjects[0];
    let number = &expected.identifiers["kvk_number"];
    ensure!(
        subject.label == expected.label
            && subject.values.get("company_registry.kvknummer") == Some(&json!(number))
            && subject
                .values
                .iter()
                .all(|(key, value)| match key.as_str() {
                    "company_registry.kvknummer" => value == number,
                    "company.name" => value == &expected.label,
                    _ => false,
                }),
        "company answer must retain the literal name and KVK number"
    );
    for (capability, endpoint) in [
        ("company.registry-profile", PROFILE_ENDPOINT),
        ("company.financial-statements", FIGURES_ENDPOINT),
    ] {
        ensure!(
            intent
                .goals
                .iter()
                .filter(|goal| goal.subject == 0
                    && goal.capability == capability
                    && goal.constraints.get("source") == Some(&json!(PROVIDER))
                    && goal.constraints.get("endpoint_id") == Some(&json!(endpoint))
                    && goal.constraints.len() == 2)
                .count()
                == 1,
            "company answer does not match the two approved KVK operations"
        );
    }
    let entity = subject
        .entity_ref
        .context("company answer has no saved subject")?;
    let profile = saved(sources, entity, PROFILE_ENDPOINT, "/api/v1/basisprofielen")?;
    let figures = saved(
        sources,
        entity,
        FIGURES_ENDPOINT,
        crate::annual_report::ENDPOINT,
    )?;
    ensure!(
        profile["result_ref"] != figures["result_ref"],
        "company sources share a result reference"
    );
    let name_pointer = ["/data/statutaireNaam", "/data/naam"]
        .into_iter()
        .find(|pointer| profile["document"].pointer(pointer)
            .and_then(Value::as_str).is_some_and(|name| !name.trim().is_empty()))
        .context("company profile has no registered name")?;
    ensure!(
        profile["document"]["data"]["kvkNummer"] == *number,
        "company profile does not identify the supplied KVK number"
    );
    let identifiers = figures["document"]["report_subject"]["identifiers"]
        .as_array()
        .context("annual figures lack the saved KVK binding")?;
    ensure!(
        !identifiers.is_empty() && identifiers.iter().all(|value| value == number),
        "annual figures belong to a different KVK number"
    );
    let mut fields = Vec::new();
    financial_fields(
        &figures["document"]["data"]["opendataFields"],
        "/data/opendataFields",
        None,
        None,
        0,
        &mut fields,
    )?;
    let years: Vec<_> = fields
        .iter()
        .filter(|field| field.key == "FinancialYear")
        .collect();
    ensure!(
        years.len() <= 1 && years.first().is_none_or(|year| year.section.is_none()),
        "annual figures need one unambiguous reported financial year"
    );
    let year = years.first().filter(|field| {
        figures["document"]
            .pointer(&field.pointer)
            .is_some_and(|value| {
                !value.is_null() && !value.as_str().is_some_and(|text| text.trim().is_empty())
            })
    });
    if let Some(field) = year {
        let value = figures["document"].pointer(&field.pointer).unwrap();
        let valid = value
            .as_str()
            .and_then(|year| year.parse::<u16>().ok())
            .or_else(|| value.as_u64().and_then(|year| u16::try_from(year).ok()));
        ensure!(
            valid.is_some_and(|year| (1900..=2200).contains(&year)),
            "financial year is not a reported calendar year"
        );
    }

    let mut rows = Vec::new();
    for (label, pointer) in [
        ("Registered name", name_pointer),
        ("KVK number", "/data/kvkNummer"),
        ("Legal form", "/data/_embedded/eigenaar/rechtsvorm"),
        ("RSIN", "/data/_embedded/eigenaar/rsin"),
        (
            "Registration start",
            "/data/materieleRegistratie/datumAanvang",
        ),
        ("Reported persons employed", "/data/totaalWerkzamePersonen"),
    ] {
        row(&mut rows, profile, label, pointer)?;
    }
    if let Some(addresses) =
        profile["document"]["data"]["_embedded"]["hoofdvestiging"]["adressen"].as_array()
    {
        ensure!(
            addresses.len() <= 4,
            "main-establishment address scope exceeds compact company display"
        );
        for (index, address) in addresses.iter().enumerate() {
            if address["indAfgeschermd"] == "Nee" {
                let label = match address["type"].as_str() {
                    Some("bezoekadres") => "Main establishment visiting address",
                    Some("postadres") => "Main establishment postal address",
                    _ => "Main establishment address",
                };
                row(
                    &mut rows,
                    profile,
                    label,
                    &format!("/data/_embedded/hoofdvestiging/adressen/{index}/volledigAdres"),
                )?;
            }
        }
    }
    if let Some(activities) = profile["document"]["data"]["sbiActiviteiten"].as_array() {
        ensure!(
            activities.len() <= 10,
            "registered activity scope exceeds compact company display"
        );
        for (index, _) in activities.iter().enumerate() {
            row(
                &mut rows,
                profile,
                "Registered SBI code",
                &format!("/data/sbiActiviteiten/{index}/sbiCode"),
            )?;
            row(
                &mut rows,
                profile,
                "Registered activity",
                &format!("/data/sbiActiviteiten/{index}/sbiOmschrijving"),
            )?;
        }
    }
    if let Some(year) = year {
        row(&mut rows, figures, "Financial year", &year.pointer)?;
    } else {
        rows.push(json!([{"label":"Financial year"},null]));
    }
    let mut scopes = BTreeMap::new();
    let mut labels = std::collections::BTreeSet::new();
    let mut figure_count = 0;
    for field in &fields {
        let Some(label) = figure_label(&field.key).or_else(|| metadata_label(&field.key)) else {
            continue;
        };
        let label = field
            .section
            .map_or_else(|| label.to_owned(), |section| format!("{section}: {label}"));
        ensure!(
            labels.insert(label.clone()),
            "duplicate annual figure or unit in the same statement scope"
        );
        if figure_label(&field.key).is_some() {
            ensure!(
                field.section.is_some(),
                "annual amount has no reviewed statement scope"
            );
            let value = figures["document"]
                .pointer(&field.pointer)
                .unwrap_or(&Value::Null);
            if !value.is_null() && !value.as_str().is_some_and(|value| value.trim().is_empty()) {
                ensure!(
                    decimal(value),
                    "annual financial amount is not a source decimal"
                );
                figure_count += 1;
            }
        }
        if let (Some(section), Some(pointer)) = (field.section, field.section_pointer.as_ref()) {
            scopes.insert(section, pointer.clone());
        }
        row(&mut rows, figures, &label, &field.pointer)?;
    }
    ensure!(
        rows.len() <= 50,
        "company source scope exceeds compact answer capacity; full saved results remain available"
    );
    let mut observations = vec![
        json!({"text":"The register identifies {{a}} with KVK number {{b}}.",
        "evidence":[reference(profile,name_pointer), reference(profile,"/data/kvkNummer")]}),
    ];
    if let Some(year) = year {
        observations.push(json!({"text":"The available annual figures report financial year {{a}}. The table keeps the source's statement scopes separate.",
        "evidence":[reference(figures,&year.pointer)]}));
    }
    for pointer in scopes.values() {
        observations.push(json!({"text":"The supplied statement section is {{a}}.", "evidence":[reference(figures,pointer)]}));
    }
    let mut limitations = vec![
        "These are available KVK source data, not a verified latest filing or a complete original annual report. The generated PDF is a rendering of the saved figures.",
        "Values are copied without calculations or conversion. Currency, unit, scale and consolidation status apply only where the source supplies them; no currency or unit is inferred.",
        "The table is a compact selection of registered details and available financial fields. Missing or withheld fields remain unknown, never zero. All original fields remain in the saved source results.",
    ];
    if figure_count == 0 {
        limitations.push("The saved annual source provides no recognized nonempty financial figures for this summary. This does not establish that no annual accounts were filed.");
    }
    if year.is_none() {
        limitations.push("The saved annual source does not supply a financial year. No year is inferred from the current date or retrieval date.");
    }
    let source_partial = sources.iter().any(|source| {
        source["document"]["coverage"] == "partial"
            || source["document"]["missing_fields"]
                .as_array()
                .is_some_and(|fields| !fields.is_empty())
    });
    if source_partial {
        limitations.push("A saved source reports incomplete coverage. Its missing-field details remain in the saved source result; unavailable information remains unknown.");
    }
    let raw = json!({"observations":observations,"limitations":limitations,
        "presentation":{"kind":"comparison","title":"Registered details and available annual figures",
            "columns":[{"label":"Field"},{"label":"Reported value"}],"rows":rows}});
    let mut answer = crate::parser::validate_analysis(
        &raw.to_string(),
        sources,
        source_partial || year.is_none() || figure_count == 0,
    )?;
    answer["method"] = json!("saved_kvk_company_figures");
    Ok(Some(answer))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const QUESTION: &str = "Find the registered details and available annual financial figures for Coolblue B.V. (KVK 24330087).";

    fn intake(question: &str) -> Intake {
        Intake {
            parsed_request: parse(question, "fixture").unwrap(),
            matches: vec![],
            apiosk_sources_searched: 0,
            catalog_version: "fixture".into(),
            selected_capabilities: Default::default(),
        }
    }

    fn saved_fixture() -> (String, crate::parser::Intent, Vec<Value>) {
        // The registry payload is the recorded Mollie-shaped catalog fixture;
        // annual figures are the catalog's synthetic contract sample. This is
        // projection evidence only, never a claim about Mollie's real figures.
        let catalog = crate::catalog::Catalog::build(
            serde_json::from_str(include_str!("../tests/fixtures/workflows/catalog.json")).unwrap(),
        );
        let question = format!("{PREFIX}Mollie B.V. (KVK 30204462).");
        let entity = uuid::Uuid::new_v4();
        let mut goals = Vec::new();
        let mut sources = Vec::new();
        for capability in ["company.registry-profile", "company.financial-statements"] {
            let service = catalog
                .services
                .iter()
                .find(|service| {
                    service.api_slug == PROVIDER && service.contract.capability_slug == capability
                })
                .unwrap();
            goals.push(crate::parser::Goal {
                capability: capability.into(),
                subject: 0,
                constraints: BTreeMap::from([
                    ("source".into(), json!(PROVIDER)),
                    ("endpoint_id".into(), json!(service.endpoint_id)),
                ]),
            });
            sources.push(json!({"result_ref":uuid::Uuid::new_v4(),"document":{
                "subject":{"entity_ref":entity,"label":"Mollie B.V."}, "source":{"provider":PROVIDER,"name":"KVK Business Register","path":service.path,"endpoint_id":service.endpoint_id},
                "report_subject":{"name":"Mollie B.V.","identifiers":["30204462"]},
                "coverage":"source_response","missing_fields":[],"data":service.sample_payload}}));
        }
        let intent = crate::parser::Intent {
            subjects: vec![crate::parser::Subject {
                entity_ref: Some(entity),
                label: "Mollie B.V.".into(),
                values: BTreeMap::from([
                    ("company_registry.kvknummer".into(), json!("30204462")),
                    ("company.name".into(), json!("Mollie B.V.")),
                ]),
            }],
            goals,
            answer_operations: vec![],
            unresolved_references: vec![],
        };
        (question, intent, sources)
    }

    fn shown<'a>(answer: &'a Value, label: &str) -> &'a Value {
        &answer["presentation"]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row[0]["value"] == label)
            .unwrap()[1]
    }

    #[test]
    fn saved_company_answer_cites_registry_and_exact_year_figures_without_new_facts() {
        let (question, intent, sources) = saved_fixture();
        let before = sources.clone();
        let answer = compose(&question, &intent, &sources).unwrap().unwrap();
        assert_eq!(answer["status"], "completed");
        assert_eq!(shown(&answer, "Registered name")["value"], "Mollie B.V.");
        assert_eq!(shown(&answer, "KVK number")["value"], "30204462");
        assert_eq!(shown(&answer, "Financial year")["value"], "2024");
        assert_eq!(
            shown(&answer, "Balance sheet: Total assets")["value"],
            "123456"
        );
        assert_eq!(shown(&answer, "Balance sheet: Equity")["value"], "65432");
        for row in answer["presentation"]["rows"].as_array().unwrap() {
            if row[1].is_null() {
                continue;
            }
            let source = sources
                .iter()
                .find(|source| source["result_ref"] == row[1]["result_ref"])
                .unwrap();
            assert_eq!(
                Some(&row[1]["value"]),
                source["document"].pointer(row[1]["pointer"].as_str().unwrap())
            );
        }
        let labels = answer["presentation"]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row[0]["value"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(!labels.iter().any(|label| label.contains("filing")
            || label.contains("adopt")
            || label.contains("Currency")));
        assert!(!answer.to_string().contains("EUR"));
        assert!(answer["limitations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value
                .as_str()
                .unwrap()
                .contains("not a verified latest filing")));
        assert_eq!(
            sources, before,
            "composition cannot alter or replay a purchased source"
        );
    }

    #[tokio::test]
    async fn published_mollie_request_quotes_both_branches_and_composes_actual_saved_figures() {
        let (question, _, mut sources) = saved_fixture();
        assert_eq!(question,"Find the registered details and available annual financial figures for Mollie B.V. (KVK 30204462).");
        let cached: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/kvk-mollie-available-figures.json"
        ))
        .unwrap();
        let profile: Value = serde_json::from_str(include_str!(
            "../crates/catalog-core/fixtures/contracts/payloads/kvk-basisprofiel-mollie.json"
        ))
        .unwrap();
        assert_eq!(cached["kvk_number"], profile["kvkNummer"]);
        let catalog = crate::catalog::Catalog::build(
            serde_json::from_str(include_str!("../tests/fixtures/workflows/catalog.json")).unwrap(),
        );
        let intake = crate::capability_intake::offline_fixture_intake(&question, &catalog);
        let mut intent = crate::parser::plan_coverage::offline_fixture_reply(&intake, &catalog, &question, &Default::default()).unwrap()
            .intent
            .unwrap();
        let mut task = crate::state::Task { capability_intake:Some(intake), ..Default::default() };
        crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
        let crate::resolver::Resolution::Plan(plan) =
            crate::resolver::resolve(&catalog, &task, &intent)
        else {
            panic!("supplied Mollie KVK must receive one complete quote")
        };
        assert_eq!(plan.steps.len(), 2);
        assert!(plan.cap_atomic > 0);
        for source in &mut sources {
            source["document"]["subject"]["entity_ref"] =
                json!(intent.subjects[0].entity_ref.unwrap());
        }
        sources[0]["document"]["data"] = profile;
        sources[1]["document"]["data"] = cached["payload"].clone();
        sources[1]["document"]["source"]["url"] = cached["source_url"].clone();
        sources[1]["document"]["source"]["retrieved_at"] = cached["cached_at"].clone();
        for step in &plan.steps {
            let source = sources
                .iter()
                .find(|source| {
                    source["document"]["source"]["endpoint_id"] == json!(step.service.endpoint_id)
                })
                .unwrap();
            let extraction = step
                .service
                .contract
                .extract(&source["document"]["data"], &catalog.vocabulary)
                .unwrap();
            assert!(extraction.rejected.is_empty());
            assert!(extraction.single().is_some());
        }
        let answer = compose(&question, &intent, &sources).unwrap().unwrap();
        assert_eq!(answer["status"], "completed");
        assert_eq!(shown(&answer, "Financial year")["value"], "2020");
        assert_eq!(
            shown(&answer, "Balance sheet: Total assets")["value"],
            "194495000"
        );
        assert_eq!(shown(&answer, "Balance sheet: Equity")["value"], "7663000");
        assert_eq!(
            shown(&answer, "Income statement: Result after tax")["value"],
            "-5166000"
        );
        assert_eq!(
            shown(&answer, "Main establishment visiting address")["value"],
            "Keizersgracht 126 1015CW Amsterdam"
        );
        assert!(
            answer["presentation"]["rows"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| {
                    let label = row[0]["value"].as_str().unwrap();
                    !label.contains("Consolidated")
                        && !label.contains("Currency")
                        && !label.contains("filing date")
                }),
            "consolidation, currency and filing date are not supplied by this response"
        );
    }

    #[test]
    fn annual_scopes_units_nulls_and_exact_decimal_strings_are_preserved() {
        let (question, intent, mut sources) = saved_fixture();
        let fields = sources[1]["document"]["data"]["opendataFields"]
            .as_array_mut()
            .unwrap();
        fields[1]["opendataFields"][0]["value"] = json!("9007199254740993.01");
        fields.push(json!({"key":"Currency","value":"EUR"}));
        fields.push(json!({"key":"Unit","value":"thousands"}));
        fields.push(json!({"key":"DocumentAdoptionDate","value":"2026-04-26"}));
        fields.push(json!({"key":"ConsolidatedBalanceSheet","opendataFields":[{"key":"Assets","value":"0"},{"key":"Equity","value":"-373267"},{"key":"Liabilities","value":null}]}));
        let answer = compose(&question, &intent, &sources).unwrap().unwrap();
        assert_eq!(
            shown(&answer, "Balance sheet: Total assets")["value"],
            "9007199254740993.01"
        );
        assert_eq!(
            shown(&answer, "Consolidated balance sheet: Total assets")["value"],
            "0"
        );
        assert_eq!(
            shown(&answer, "Consolidated balance sheet: Equity")["value"],
            "-373267"
        );
        assert!(shown(&answer, "Consolidated balance sheet: Liabilities").is_null());
        assert_eq!(shown(&answer, "Currency")["value"], "EUR");
        assert_eq!(shown(&answer, "Unit")["value"], "thousands");
        assert!(
            !answer.to_string().contains("2026-04-26"),
            "an adoption-date field is not silently presented as a filing date"
        );
        assert!(answer["observations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|observation| observation["evidence"][0]["value"] == "ConsolidatedBalanceSheet"));
    }

    #[test]
    fn empty_or_yearless_saved_annual_data_is_explicitly_partial_never_zero() {
        let (question, intent, original) = saved_fixture();
        for fields in [
            json!([]),
            json!([{"key":"FinancialYear","value":null}]),
            json!([{"key":"FinancialYear","value":"2024"}]),
        ] {
            let mut sources = original.clone();
            sources[1]["document"]["data"]["opendataFields"] = fields;
            sources[1]["document"]["coverage"] = json!("partial");
            sources[1]["document"]["missing_fields"] = json!(["company.financial_statements"]);
            let answer = compose(&question, &intent, &sources).unwrap().unwrap();
            assert_eq!(answer["status"], "partial");
            assert_eq!(shown(&answer, "Registered name")["value"], "Mollie B.V.");
            assert!(answer["limitations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value
                    .as_str()
                    .unwrap()
                    .contains("does not establish that no annual accounts were filed")));
            assert!(!answer["presentation"]["rows"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row[0]["value"].as_str().unwrap().contains("Total assets")));
        }
    }

    #[test]
    fn trade_name_profile_and_empty_annuals_keep_the_available_registered_evidence() {
        let (_, mut intent, mut sources) = saved_fixture();
        let name = "Bas Grave Web Development.";
        let number = "87560488";
        let question = format!("{PREFIX}{name} (KVK {number}).");
        intent.subjects[0].label = name.into();
        intent.subjects[0].values = BTreeMap::from([
            ("company.name".into(), json!(name)),
            ("company_registry.kvknummer".into(), json!(number)),
        ]);
        for source in &mut sources {
            source["document"]["subject"]["label"] = json!(name);
            source["document"]["report_subject"] = json!({"name":name,"identifiers":[number]});
        }
        sources[0]["document"]["data"] = json!({"kvkNummer":number,"naam":"Bas Grave Web Development"});
        sources[1]["document"]["data"] = json!({"opendataFields":[]});
        let original = sources.clone();
        let answer = compose(&question, &intent, &sources).unwrap().unwrap();
        assert_eq!(answer["status"], "partial");
        assert_eq!(shown(&answer, "Registered name")["value"], "Bas Grave Web Development");
        assert_eq!(shown(&answer, "Registered name")["pointer"], "/data/naam");
        assert_eq!(answer["observations"][0]["evidence"][0]["pointer"], "/data/naam");
        assert_eq!(shown(&answer, "KVK number")["value"], number);
        assert_eq!(sources, original);
        assert!(answer["limitations"].as_array().unwrap().iter().any(|value|
            value.as_str().unwrap().contains("no recognized nonempty financial figures")));
        sources[0]["document"]["data"]["kvkNummer"] = json!("30204462");
        assert!(compose(&question, &intent, &sources).is_err());
    }

    #[test]
    fn recognized_company_mismatches_fail_closed_and_other_questions_remain_unhandled() {
        let (question, intent, sources) = saved_fixture();
        for mutation in [
            "entity",
            "number",
            "financial_binding",
            "provider",
            "endpoint",
            "path",
            "missing_source",
            "scope_note",
            "duplicate_year",
            "duplicate_figure",
            "nonnumeric_figure",
        ] {
            let mut changed = sources.clone();
            match mutation {
                "entity" => {
                    changed[1]["document"]["subject"]["entity_ref"] = json!(uuid::Uuid::new_v4())
                }
                "number" => changed[0]["document"]["data"]["kvkNummer"] = json!("24330087"),
                "financial_binding" => {
                    changed[1]["document"]["report_subject"]["identifiers"] = json!(["24330087"])
                }
                "provider" => {
                    changed[1]["document"]["source"]["provider"] = json!("another-source")
                }
                "endpoint" => {
                    changed[1]["document"]["source"]["endpoint_id"] = json!(uuid::Uuid::new_v4())
                }
                "path" => changed[1]["document"]["source"]["path"] = json!("/other"),
                "missing_source" => {
                    changed.pop();
                }
                "nonnumeric_figure" => {
                    changed[1]["document"]["data"]["opendataFields"][1]["opendataFields"][0]
                        ["value"] = json!(true)
                }
                "scope_note" => {
                    changed[1]["document"]["coverage_note"] =
                        json!("Important additional source limitation")
                }
                "duplicate_year" => changed[1]["document"]["data"]["opendataFields"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"key":"FinancialYear","value":"2025"})),
                _ => changed[1]["document"]["data"]["opendataFields"][1]["opendataFields"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"key":"Equity","value":"123"})),
            }
            assert!(compose(&question, &intent, &changed).is_err(), "{mutation}");
        }
        let mut changed = intent.clone();
        changed.goals[1]
            .constraints
            .insert("filing_selection".into(), json!("latest"));
        assert!(compose(&question, &changed, &sources).is_err());
        assert!(compose(
            "Find the registered details and latest filed accounts for Coolblue B.V.",
            &intent,
            &sources
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn exact_company_and_explicit_identifier_are_preserved_without_extra_fields() {
        for (name, number, expected) in [
            ("Bas Grave Web Development", "87560488", "Bas Grave Web Development"),
            ("Bas Grave Web Development.", "87560488", "Bas Grave Web Development."),
            ("Stichting Voor Elkaar", "12345678", "Stichting Voor Elkaar"),
            ("Check Point", "01234567", "Check Point"),
            ("Coolblue B.V.", "24330087", "Coolblue B.V."),
            ("Mollie B.V.", "30204462", "Mollie B.V."),
            (
                "O’Neill & Partners N.V.",
                "01234567",
                "O’Neill & Partners N.V.",
            ),
            (
                "\"Stichting Voor Elkaar\"",
                "12345678",
                "Stichting Voor Elkaar",
            ),
        ] {
            let question = format!("{PREFIX}{name} (KVK {number}).");
            let parsed = parse(&question, "fixture").unwrap();
            assert_eq!(parsed.subjects.len(), 1);
            assert_eq!(parsed.subjects[0].label, expected);
            assert_eq!(
                parsed.subjects[0].identifiers,
                BTreeMap::from([("kvk_number".into(), number.into())])
            );
            assert_eq!(
                parsed
                    .capabilities
                    .iter()
                    .map(|cap| cap.outputs.clone())
                    .collect::<Vec<_>>(),
                vec![vec!["profile"], vec!["financial_statements"]]
            );
            assert!(parsed
                .capabilities
                .iter()
                .all(|cap| cap.inputs == ["kvk_number"] && cap.missing.is_empty()));
            assert_eq!(
                parsed.meta.text_sha256,
                hex::encode(Sha256::digest(question.as_bytes()))
            );
        }
    }

    #[test]
    fn latest_specific_periods_formats_and_extra_requirements_stay_strict() {
        for question in [
            "Find the registered details and latest filed accounts for Coolblue B.V.",
            "Find the registered details and latest filed accounts for Coolblue B.V. (KVK 24330087).",
            "Find the registered details and available annual financial figures for Coolblue B.V. (KVK 24330087) for 2024.",
            "Find the registered details and available annual financial figures for Coolblue B.V. for 2024 (KVK 24330087).",
            "Find the registered details and available annual financial figures for Coolblue B.V. including ownership (KVK 24330087).",
            "Find the registered details and available annual financial figures for Coolblue B.V. including ownership B.V. (KVK 24330087).",
            "Find the registered details and available annual financial figures for Coolblue B.V. (KVK 24330087), including auditor details.",
            "Find the registered details and available annual financial figures for Coolblue B.V. (KVK 24330087). Return JSON.",
            "Find the registered details and available annual financial figures for Coolblue B.V. (KVK 24330087) as PDF.",
            "Find the registered details and available annual financial figures for Coolblue B.V. (KVK 24330087) and Mollie B.V. (KVK 30204462).",
            "Find the registered details and available annual financial figures for Coolblue B.V. (KVK 24330087, 30204462).",
            "Find the registered details and available annual financial figures for Coolblue B.V. (KVK 2433008).",
            "Find the registered details and available annual financial figures for Coolblue B.V. (KVK 243300870).",
            "Find the registered details and available annual financial figures for Coolblue B.V. (KVK 2433O087).",
        ] { assert!(parse(question, "fixture").is_none(), "{question}"); }
        for action in ["Get", "Find", "Retrieve", "Fetch", "Check", "Validate", "Add", "List", "Also get", "Please retrieve", "Can you show"] {
            for name in ["Example Company.", "Example B.V.", "A."] {
                let question = format!("{PREFIX}{name} {action} salaries (KVK 12345678).");
                assert!(parse(&question, "fixture").is_none(), "{question}");
            }
        }
    }

    #[test]
    fn preparing_requires_the_same_displayed_scope_and_identifier() {
        assert!(executable_intake(QUESTION, &intake(QUESTION)).is_some());
        let mut changed = intake(QUESTION);
        changed.parsed_request.subjects[0]
            .identifiers
            .insert("kvk_number".into(), "30204462".into());
        assert!(executable_intake(QUESTION, &changed).is_none());
        let mut changed = intake(QUESTION);
        changed.parsed_request.capabilities[1]
            .outputs
            .push("filing_date".into());
        assert!(executable_intake(QUESTION, &changed).is_none());
        let mut changed = intake(QUESTION);
        changed.parsed_request.deliverable.format = "pdf".into();
        assert!(executable_intake(QUESTION, &changed).is_none());
    }

    #[tokio::test]
    async fn offline_company_fixture_uses_the_normal_two_call_quote_without_name_lookup() {
        let catalog = crate::catalog::Catalog::build(
            serde_json::from_str(include_str!("../tests/fixtures/workflows/catalog.json")).unwrap(),
        );
        let intake = crate::capability_intake::offline_fixture_intake(QUESTION, &catalog);
        assert!(executable_intake(QUESTION, &intake).is_some());
        let reply = crate::parser::plan_coverage::offline_fixture_reply(&intake, &catalog, QUESTION, &Default::default()).unwrap();
        assert!(reply.discover, "{}", reply.message);
        let mut intent = reply.intent.unwrap();
        assert_eq!(intent.goals.len(), 2);
        assert_eq!(
            intent.subjects[0].values["company_registry.kvknummer"],
            "24330087"
        );
        let mut task = crate::state::Task { capability_intake:Some(intake), ..Default::default() };
        crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
        let crate::resolver::Resolution::Plan(plan) =
            crate::resolver::resolve(&catalog, &task, &intent)
        else {
            panic!("both available company records should receive one quote")
        };
        assert_eq!(plan.steps.len(), 2);
        assert!(
            plan.cap_atomic > 0,
            "ordinary buyer pricing and approval are still required"
        );
        assert!(plan.steps.iter().all(|step| step.service.api_slug
            == "kvk-dutch-business-register"
            && ["company.registry-profile", "company.financial-statements"]
                .contains(&step.service.contract.capability_slug.as_str())));
    }

    #[test]
    fn aggregates_are_actual_contract_outputs_and_identifier_binds_without_name_search() {
        let catalog = crate::catalog::Catalog::build(
            serde_json::from_str(include_str!("../tests/fixtures/workflows/catalog.json")).unwrap(),
        );
        let parsed = parse(QUESTION, "fixture").unwrap();
        let mut goals = vec![];
        for (operation, output) in [
            ("company.registry-profile", "profile"),
            ("company.financial-statements", "financial_statements"),
        ] {
            let service = catalog
                .services
                .iter()
                .find(|service| {
                    service.api_slug == "kvk-dutch-business-register"
                        && service.contract.capability_slug == operation
                })
                .unwrap();
            assert!(service
                .contract
                .output_map
                .produces
                .contains_key(&format!("company.{output}")));
            goals.push(crate::parser::Goal {
                capability: operation.into(),
                subject: 0,
                constraints: BTreeMap::from([
                    ("source".into(), json!(service.api_slug)),
                    ("endpoint_id".into(), json!(service.endpoint_id)),
                ]),
            });
        }
        let value =
            crate::ask_v2::input_values::literal("company_registry.kvknummer", &parsed.subjects[0])
                .unwrap();
        assert_eq!(value, "24330087");
        let mut intent = crate::parser::Intent {
            subjects: vec![crate::parser::Subject {
                entity_ref: None,
                label: parsed.subjects[0].label.clone(),
                values: BTreeMap::from([("company_registry.kvknummer".into(), value)]),
            }],
            goals,
            answer_operations: vec![],
            unresolved_references: vec![],
        };
        let mut task = crate::state::Task::default();
        crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
        let crate::resolver::Resolution::Plan(plan) =
            crate::resolver::resolve(&catalog, &task, &intent)
        else {
            panic!("explicit KVK must resolve both source calls")
        };
        assert_eq!(
            plan.steps.len(),
            2,
            "the supplied identifier needs no paid name lookup or candidate choice"
        );
        assert!(plan.steps.iter().all(|step| step.service.api_slug
            == "kvk-dutch-business-register"
            && Some(step.entity_ref) == intent.subjects[0].entity_ref));
        let financial = plan
            .steps
            .iter()
            .find(|step| step.service.contract.capability_slug == "company.financial-statements")
            .unwrap();
        let source = financial.service.sample_payload.as_ref().unwrap();
        let extracted = financial
            .service
            .contract
            .extract(source, &catalog.vocabulary)
            .unwrap();
        let values = &extracted
            .single()
            .unwrap()
            .fact("company.financial_statements")
            .unwrap()
            .value;
        assert_eq!(
            values,
            &json!({"financial_year":2024.0,"total_assets":123456.0,"equity":65432.0})
        );
        for missing in [
            "currency",
            "filed_on",
            "adopted_on",
            "latest",
            "document_url",
        ] {
            assert!(
                values.get(missing).is_none(),
                "{missing} must never be fabricated from the available figures"
            );
        }
        assert_eq!(
            financial.service.result_source()["provider"],
            "kvk-dutch-business-register"
        );
        intent
            .goals
            .iter_mut()
            .find(|goal| goal.capability == "company.financial-statements")
            .unwrap()
            .constraints
            .insert("filing_selection".into(), json!("latest"));
        assert!(
            matches!(
                crate::resolver::resolve(&catalog, &task, &intent),
                crate::resolver::Resolution::Unsupported(_)
            ),
            "the same source plan cannot satisfy an explicit latest requirement"
        );
    }
}
