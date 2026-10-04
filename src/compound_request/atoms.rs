//! Complete literal clauses for complementary, catalog-backed source reads.
//! These recognize scope and bind supplied values only; discovery, contract
//! validation, quoting and approval remain responsible for choosing execution.
use crate::capability_intake::{Capability, Deliverable, Meta, ParsedRequest, Subject};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

fn between<'a>(text: &'a str, prefix: &str, suffix: &str) -> Option<&'a str> {
    text.get(..prefix.len())?
        .eq_ignore_ascii_case(prefix)
        .then_some(())?;
    let end = text.len().checked_sub(suffix.len())?;
    text.get(end..)?
        .eq_ignore_ascii_case(suffix)
        .then_some(())?;
    text.get(prefix.len()..end)
}

fn quoted(value: &str, max: usize) -> Option<&str> {
    let value = value.strip_prefix('"')?.strip_suffix('"')?;
    (!value.trim().is_empty()
        && value == value.trim()
        && value.len() <= 500
        && value.chars().count() <= max
        && !value.chars().any(|c| c.is_control() || c == '"'))
    .then_some(value)
}

fn domain(value: &str) -> bool {
    let labels: Vec<_> = value.split('.').collect();
    let uk = labels.len() == 3
        && labels[2] == "uk"
        && ["co", "org", "me", "ltd", "plc", "net", "sch", "ac", "gov"].contains(&labels[1]);
    value.len() <= 253
        && (labels.len() == 2 || uk)
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        })
        && labels
            .last()
            .is_some_and(|last| last.len() >= 2 && last.bytes().all(|c| c.is_ascii_lowercase()))
}

#[allow(clippy::too_many_arguments)]
fn request(
    clause: &str,
    model: &str,
    label: &str,
    kind: &str,
    identifiers: BTreeMap<String, String>,
    operation: &str,
    source: &str,
    name: &str,
    description: &str,
    outputs: &[&str],
) -> ParsedRequest {
    // Discovery recognizes these exact normalized catalog operation slugs.
    let (slug, domain) = crate::capability_intake::normalize_slug(operation, "").unwrap();
    let inputs = identifiers.keys().cloned().collect();
    ParsedRequest {
        language: "en".into(),
        subjects: vec![Subject {
            id: "s1".into(),
            label: label.into(),
            kind: kind.into(),
            identifiers,
        }],
        capabilities: vec![Capability {
            slug,
            domain,
            name: name.into(),
            description: description.into(),
            inputs,
            outputs: outputs.iter().map(|v| (*v).into()).collect(),
            subject_id: Some("s1".into()),
            source_hint: Some(source.into()),
            confidence: 1.0,
            missing: vec![],
        }],
        deliverable: Deliverable {
            format: "chat".into(),
            operations: vec![],
        },
        meta: Meta {
            model: model.into(),
            parsed_at: chrono::Utc::now().to_rfc3339(),
            text_sha256: hex::encode(Sha256::digest(clause.as_bytes())),
        },
    }
}

pub(super) fn parse(clause: &str, model: &str) -> Option<ParsedRequest> {
    let text = clause.trim().strip_suffix('.').unwrap_or(clause.trim());
    let one = |label: &str,
               kind: &str,
               input: &str,
               value: &str,
               operation: &str,
               source: &str,
               name: &str,
               description: &str,
               outputs: &[&str]| {
        request(
            clause,
            model,
            label,
            kind,
            BTreeMap::from([(input.into(), value.into())]),
            operation,
            source,
            name,
            description,
            outputs,
        )
    };
    if let Some(value) = between(text, "Search the web through Linkup for ", "")
        .and_then(|value| quoted(value, 2000))
    {
        return Some(one(
            value,
            "topic",
            "web.source_search_query",
            value,
            "web.linkup-answer",
            "linkup",
            "Source-backed web context",
            "Search the supplied literal query through Linkup and retrieve its answer and source links. Preserve the returned source scope and distinguish cited source statements from synthesis.",
            &["answer", "sources"],
        ));
    }
    if let Some(value) = between(text, "Retrieve the web page ", " through Linkup") {
        let url = reqwest::Url::parse(value).ok()?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || value.chars().any(char::is_whitespace)
            || value.len() > 500
        {
            return None;
        }
        return Some(one(
            value,
            "web_page",
            "web.source_page_url",
            value,
            "web.source-fetch",
            "linkup",
            "Retrieved web page",
            "Retrieve the supplied page through Linkup. Summarize only the returned page content and cite that page; fetching it does not verify every statement it contains.",
            &["markdown"],
        ));
    }
    if let Some(value) = between(text, "Search official publications through KOOP for ", "")
        .and_then(|value| quoted(value, 200))
    {
        return Some(one(
            value,
            "topic",
            "government.publication.keyword",
            value,
            "government.publication.search",
            "koop",
            "Official publication search",
            "Search KOOP official publications using the exact supplied keyword or address. Return the available bounded source page; verify address relevance from each notice and do not treat nearby or keyword-only hits as exact property matches.",
            &["government.publication.page"],
        ));
    }
    if let Some(value) = between(text, "Validate IBAN ", " through IBAN Check") {
        if !(15..=34).contains(&value.len())
            || !value.is_ascii()
            || !value[..2].bytes().all(|c| c.is_ascii_uppercase())
            || !value[2..4].bytes().all(|c| c.is_ascii_digit())
            || !value[4..]
                .bytes()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        {
            return None;
        }
        return Some(one(
            value,
            "iban",
            "bank.iban",
            value,
            "iban.validate",
            "iban-check",
            "IBAN format and bank check",
            "Validate the supplied IBAN and retrieve the available bank details. A valid format is not proof that an account exists or who owns it; a negative validation is a valid source outcome.",
            &["iban.validate.result"],
        ));
    }
    if let Some(value) = between(text, "Get GDP for ", " from Eurostat") {
        let (country, year) = value.rsplit_once(" in ")?;
        if year.len() != 4 || !year.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let operation = if country.eq_ignore_ascii_case("the Netherlands")
            || country.eq_ignore_ascii_case("Netherlands")
        {
            "statistics.eurostat-gdp-netherlands"
        } else if country.eq_ignore_ascii_case("Germany") {
            "statistics.eurostat-gdp-germany"
        } else {
            return None;
        };
        return Some(one(
            country,
            "country",
            "eurostat.input.time",
            year,
            operation,
            "eurostat",
            "Annual GDP at current prices",
            "Retrieve GDP at market prices for the supplied country and explicit year from Eurostat, in current-price million euro. Preserve the source period, unit and observation status. This is not real GDP, GDP growth or GDP per capita.",
            &["eurostat.dataset"],
        ));
    }
    if let Some(value) = between(text, "Get public mail DNS records for ", "") {
        if !domain(value) {
            return None;
        }
        return Some(one(
            value,
            "domain",
            "domain",
            value,
            "domain.mail-dns",
            "domain-intelligence",
            "Public mail DNS records",
            "Retrieve public MX mail-routing records and the reported existence, mail acceptance and DNSSEC indicators for the supplied registrable domain. No RDAP registration, non-MX records or ownership proof are requested.",
            &["domain", "mx", "exists", "accepts_mail", "dnssec_validated"],
        ));
    }
    if let Some(value) = between(text, "Find the registered details for ", "") {
        // Reuse the existing strict company-name and explicit KVK identifier
        // grammar. Only its profile capability belongs to this actual clause.
        let template = format!(
            "Find the registered details and available annual financial figures for {value}."
        );
        let mut parsed = crate::company_request::parse(&template, model)?;
        parsed.capabilities.truncate(1);
        parsed.meta.text_sha256 = hex::encode(Sha256::digest(clause.as_bytes()));
        return Some(parsed);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compound_atoms_preserve_variable_literals_and_explicit_source_scope() {
        for (question, field, value, source, output) in [
            (
                "Search the web through Linkup for \"official IT services procurement Netherlands\".",
                "web.source_search_query",
                "official IT services procurement Netherlands",
                "linkup",
                "answer",
            ),
            (
                "Search the web through Linkup for \"solar panels procurement Rotterdam\".",
                "web.source_search_query",
                "solar panels procurement Rotterdam",
                "linkup",
                "answer",
            ),
            (
                "Retrieve the web page https://example.org through Linkup.",
                "web.source_page_url",
                "https://example.org",
                "linkup",
                "markdown",
            ),
            (
                "Retrieve the web page https://www.iana.org/help/example-domains through Linkup.",
                "web.source_page_url",
                "https://www.iana.org/help/example-domains",
                "linkup",
                "markdown",
            ),
            (
                "Search official publications through KOOP for \"Damrak 1, Amsterdam\".",
                "government.publication.keyword",
                "Damrak 1, Amsterdam",
                "koop",
                "government.publication.page",
            ),
            (
                "Search official publications through KOOP for \"Grote Markt 1, Groningen\".",
                "government.publication.keyword",
                "Grote Markt 1, Groningen",
                "koop",
                "government.publication.page",
            ),
            (
                "Validate IBAN NL91ABNA0417164300 through IBAN Check.",
                "bank.iban",
                "NL91ABNA0417164300",
                "iban-check",
                "iban.validate.result",
            ),
            (
                "Validate IBAN DE89370400440532013000 through IBAN Check.",
                "bank.iban",
                "DE89370400440532013000",
                "iban-check",
                "iban.validate.result",
            ),
            (
                "Get GDP for the Netherlands in 2024 from Eurostat.",
                "eurostat.input.time",
                "2024",
                "eurostat",
                "eurostat.dataset",
            ),
            (
                "Get GDP for Germany in 2023 from Eurostat.",
                "eurostat.input.time",
                "2023",
                "eurostat",
                "eurostat.dataset",
            ),
            (
                "Get public mail DNS records for volve.capital.",
                "domain",
                "volve.capital",
                "domain-intelligence",
                "mx",
            ),
            (
                "Get public mail DNS records for example.co.uk.",
                "domain",
                "example.co.uk",
                "domain-intelligence",
                "mx",
            ),
            (
                "Find the registered details for Mollie B.V. (KVK 30204462).",
                "kvk_number",
                "30204462",
                "kvk",
                "profile",
            ),
            (
                "Find the registered details for Louwman Dealerbedrijven B.V. (KVK 27095155).",
                "kvk_number",
                "27095155",
                "kvk",
                "profile",
            ),
            (
                "Find the registered details for \"Another company\" (KVK 00123456).",
                "kvk_number",
                "00123456",
                "kvk",
                "profile",
            ),
        ] {
            let parsed = parse(question, "fixture").unwrap_or_else(|| panic!("{question}"));
            assert_eq!(parsed.subjects.len(), 1);
            assert_eq!(parsed.subjects[0].identifiers[field], value);
            assert_eq!(parsed.capabilities.len(), 1);
            assert_eq!(parsed.capabilities[0].source_hint.as_deref(), Some(source));
            assert!(parsed.capabilities[0].outputs.iter().any(|v| v == output));
            assert_eq!(
                parsed.meta.text_sha256,
                hex::encode(Sha256::digest(question.as_bytes()))
            );
            assert!(parsed.deliverable.operations.is_empty());
            for extra in [
                " Also retrieve ownership.",
                " As JSON.",
                " Use a different source.",
            ] {
                assert!(
                    parse(&format!("{question}{extra}"), "fixture").is_none(),
                    "must preserve extra scope: {question}{extra}"
                );
            }
        }
    }

    #[test]
    fn compound_atoms_reject_ambiguous_values_or_unsupported_source_variants() {
        for question in [
            "Search the web through Linkup for official pages.",
            "Search the web through Linkup for \"\".",
            "Search the web through Linkup for \"a\" and \"b\".",
            "Retrieve the web page https://example.org and https://example.com through Linkup.",
            "Retrieve the web page https://user:secret@example.org through Linkup.",
            "Retrieve the web page file:///tmp/example through Linkup.",
            "Search official publications through KOOP for \"Damrak 1\" after 2024.",
            "Validate IBAN NL91ABNA0417164300 and DE89370400440532013000 through IBAN Check.",
            "Validate IBAN NL91 ABNA 0417 1643 00 through IBAN Check.",
            "Get GDP for Belgium in 2024 from Eurostat.",
            "Get GDP for Germany in the latest year from Eurostat.",
            "Get GDP for Germany in 2023 and 2024 from Eurostat.",
            "Get GDP growth for Germany in 2024 from Eurostat.",
            "Get public mail DNS records for https://volve.capital.",
            "Get public mail DNS records for volve.capital and example.org.",
            "Get public mail DNS records for www.example.org.",
            "Get public mail DNS records for -example.org.",
            "Get public mail DNS records for EXAMPLE.ORG.",
            "Find the registered details for Mollie B.V. (KVK 30204462) and its accounts.",
            "Find the registered details for Mollie B.V. (KVK 30204462, 00123456).",
        ] {
            assert!(parse(question, "fixture").is_none(), "{question}");
        }
    }

    #[test]
    fn compound_atoms_map_to_declared_fixture_contract_outputs() {
        use crate::catalog::{Catalog, Snapshot};
        let mut value: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/workflows/catalog.json"))
                .unwrap();
        let statistics: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/statistics-presets.json"))
                .unwrap();
        value["services"]
            .as_array_mut()
            .unwrap()
            .extend(statistics["services"].as_array().unwrap().iter().cloned());
        value["vocabulary"]
            .as_array_mut()
            .unwrap()
            .extend(statistics["vocabulary"].as_array().unwrap().iter().cloned());
        let catalog = Catalog::build(serde_json::from_value::<Snapshot>(value).unwrap());
        for (question, operation, source, input) in [
            (
                "Search the web through Linkup for \"IT services official buyers\".",
                "web.linkup-answer",
                "linkup",
                "web.source_search_query",
            ),
            (
                "Retrieve the web page https://example.org through Linkup.",
                "web.source-fetch",
                "linkup",
                "web.source_page_url",
            ),
            (
                "Search official publications through KOOP for \"Damrak 1, Amsterdam\".",
                "government.publication.search",
                "koop",
                "government.publication.keyword",
            ),
            (
                "Validate IBAN NL91ABNA0417164300 through IBAN Check.",
                "iban.validate",
                "iban-check",
                "bank.iban",
            ),
            (
                "Get GDP for the Netherlands in 2024 from Eurostat.",
                "statistics.eurostat-gdp-netherlands",
                "eurostat",
                "eurostat.input.time",
            ),
            (
                "Get GDP for Germany in 2023 from Eurostat.",
                "statistics.eurostat-gdp-germany",
                "eurostat",
                "eurostat.input.time",
            ),
            (
                "Get public mail DNS records for volve.capital.",
                "domain.mail-dns",
                "domain-intelligence",
                "domain.name",
            ),
        ] {
            let parsed = parse(question, "fixture").unwrap();
            let service = catalog
                .services
                .iter()
                .find(|s| s.api_slug == source && s.contract.capability_slug == operation)
                .unwrap_or_else(|| panic!("missing contract {source}/{operation}"));
            assert_eq!(
                parsed.capabilities[0].slug,
                crate::capability_intake::normalize_slug(operation, "")
                    .unwrap()
                    .0
            );
            let bound = crate::ask_v2::input_values::literal(input, &parsed.subjects[0])
                .expect("caller literal must bind required input");
            assert!(service
                .contract
                .requires
                .iter()
                .any(|r| r.fact_type == input));
            let expected = parsed.subjects[0].identifiers.values().next().unwrap();
            assert_eq!(bound.as_str(), Some(expected.as_str()));
            // Each aggregate output names an actual contract fact; the other
            // outputs are declared fields in the admitted fact schema.
            for output in &parsed.capabilities[0].outputs {
                let declared = service.contract.output_map.produces.keys().any(|fact| {
                    fact == output
                        || catalog.vocabulary.get(fact).is_some_and(|v| {
                            serde_json::to_string(&v.value_schema)
                                .unwrap()
                                .contains(&format!("\"{output}\""))
                        })
                });
                assert!(declared, "{operation} does not declare {output}");
            }
        }
    }
}
