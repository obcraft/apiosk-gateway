//! Complete literal requests for reviewed public-source operations. Each
//! recognizer consumes the whole question, so extra facts, periods or output
//! formats continue through ordinary strict interpretation.
use crate::capability_intake::{Capability, Deliverable, Intake, Meta, ParsedRequest, Subject};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Request {
    Ecb { quote: String },
    Vat { country: String, number: String },
    Vehicle { plate: String },
    Domain { domain: String },
}

pub(crate) fn recognize(question: &str) -> Option<Request> {
    recognize_with_language(question).map(|(_, request)| request)
}

impl Request {
    /// The saved-answer projection reviews these exact source operations.
    /// Field overlap in a future generic offer cannot authorize another route.
    pub(crate) fn allows_operation(&self, service: &crate::catalog::Service) -> bool {
        let route = (
            service.contract.capability_slug.as_str(),
            service.api_slug.as_str(),
            service.method.as_str(),
            service.path.as_str(),
        );
        match self {
            Self::Ecb { .. } => {
                route
                    == (
                        "fx.ecb-reference-rate",
                        "frankfurter",
                        "GET",
                        "/v2/providers/ecb/rate/EUR/{quote}",
                    )
            }
            Self::Vat { .. } => matches!(
                route,
                (
                    "vat.eu.validate",
                    "vies",
                    "GET",
                    "/taxation_customs/vies/rest-api/ms/{cc}/vat/{nr}"
                ) | (
                    "vat.eu.check",
                    "vies",
                    "POST",
                    "/taxation_customs/vies/rest-api/check-vat-number"
                )
            ),
            Self::Vehicle { .. } => {
                route
                    == (
                        "vehicle.technical-profile",
                        "rdw",
                        "GET",
                        "/resource/m9d7-ebf2.json",
                    )
            }
            Self::Domain { .. } => matches!(
                route,
                (
                    "domain.registration",
                    "domain-intelligence",
                    "GET",
                    "/domain/{domain}"
                ) | (
                    "domain.mail-dns",
                    "domain-intelligence",
                    "GET",
                    "/dns/{domain}"
                )
            ),
        }
    }
}

fn between<'a>(text: &'a str, prefix: &str, suffix: &str) -> Option<&'a str> {
    let start = text.get(..prefix.len())?;
    let end = text.get(text.len().checked_sub(suffix.len())?..)?;
    if !start.eq_ignore_ascii_case(prefix) || !end.eq_ignore_ascii_case(suffix) {
        return None;
    }
    text.get(prefix.len()..text.len() - suffix.len())
}

fn request_text(question: &str) -> &str {
    question
        .trim()
        .strip_suffix('.')
        .unwrap_or(question.trim())
        .trim()
}

const VEHICLE_FORMS: &[(&str, &str, &str)] = &[
    ("nl", "Geef RDW-voertuiggegevens voor kenteken ", ", inclusief merk, handelsbenaming, APK-vervaldatum en de indicator voor openstaande terugroepacties"),
    ("en", "Get RDW vehicle data for registration plate ", ", including make, trade name, inspection expiry date and the outstanding recall indicator"),
];

pub(crate) fn recognize_with_language(question: &str) -> Option<(&'static str, Request)> {
    let text = request_text(question);
    for (language, prefix, suffix) in [
        (
            "nl",
            "Geef de laatste officiële ECB-referentiewisselkoers voor EUR/",
            " met waarnemingsdatum via Frankfurter",
        ),
        (
            "en",
            "Get the latest official ECB reference exchange rate for EUR/",
            ", including the observation date, via Frankfurter",
        ),
    ] {
        if let Some(quote) = between(text, prefix, suffix)
            .filter(|v| v.len() == 3 && v.bytes().all(|c| c.is_ascii_alphabetic()))
        {
            return Some((
                language,
                Request::Ecb {
                    quote: quote.to_ascii_uppercase(),
                },
            ));
        }
    }
    for (language, prefix, suffix) in [
        (
            "nl",
            "Valideer EU-btw-nummer ",
            " via VIES en toon de geldigheid en controledatum",
        ),
        (
            "en",
            "Validate EU VAT number ",
            " through VIES and show its validity and check date",
        ),
    ] {
        if let Some(vat) = between(text, prefix, suffix)
            .filter(|v| (3..=16).contains(&v.len()) && v.bytes().all(|c| c.is_ascii_alphanumeric()))
        {
            if vat[..2].bytes().all(|c| c.is_ascii_alphabetic()) {
                return Some((
                    language,
                    Request::Vat {
                        country: vat[..2].to_ascii_uppercase(),
                        number: vat[2..].to_ascii_uppercase(),
                    },
                ));
            }
        }
    }
    for &(language, prefix, suffix) in VEHICLE_FORMS {
        if let Some(plate) = between(text, prefix, suffix).filter(|v| {
            v.len() >= 6
                && v.len() <= 10
                && v.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        }) {
            let compact = plate.replace('-', "");
            if compact.len() == 6
                && compact.bytes().any(|c| c.is_ascii_digit())
                && compact.bytes().any(|c| c.is_ascii_alphabetic())
            {
                return Some((
                    language,
                    Request::Vehicle {
                        plate: compact.to_ascii_uppercase(),
                    },
                ));
            }
        }
    }
    for (language, prefix, suffix) in [
        (
            "nl",
            "Geef openbare RDAP-registratiegegevens en mail-DNS-records voor ",
            ". Citeer de bronnen",
        ),
        (
            "en",
            "Get public RDAP registration data and mail DNS records for ",
            ". Cite the sources",
        ),
    ] {
        if let Some(domain) = between(text, prefix, suffix).filter(|v| valid_domain(v)) {
            return Some((
                language,
                Request::Domain {
                    domain: domain.to_ascii_lowercase(),
                },
            ));
        }
    }
    None
}

fn valid_domain(value: &str) -> bool {
    let labels: Vec<_> = value.split('.').collect();
    let canonical_labels = if labels.last().is_some_and(|v| v.eq_ignore_ascii_case("uk"))
        && labels.get(labels.len().saturating_sub(2)).is_some_and(|v| {
            ["co", "org", "me", "ltd", "plc", "net", "sch", "ac", "gov"]
                .contains(&v.to_ascii_lowercase().as_str())
        }) {
        3
    } else {
        2
    };
    value.len() <= 253
        && labels.len() == canonical_labels
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
        && labels.last().is_some_and(|last| {
            ["com", "net", "org", "nl", "uk", "fr", "app", "dev"]
                .contains(&last.to_ascii_lowercase().as_str())
        })
}

pub(crate) fn parse(question: &str, model: &str) -> Option<ParsedRequest> {
    let (language, request) = recognize_with_language(question)?;
    let (label, kind, identifiers) = match &request {
        Request::Ecb { quote } => (
            format!("EUR/{quote}"),
            "currency_pair",
            BTreeMap::from([("fx.ecb.quote".into(), quote.clone())]),
        ),
        Request::Vat { country, number } => (
            format!("{country}{number}"),
            "vat_number",
            BTreeMap::from([("vat_number".into(), format!("{country}{number}"))]),
        ),
        Request::Vehicle { .. } => {
            // Preserve the user literal until the RDW adapter removes separators.
            let literal = VEHICLE_FORMS
                .iter()
                .find_map(|(_, prefix, suffix)| between(request_text(question), prefix, suffix))?;
            (
                literal.into(),
                "vehicle",
                BTreeMap::from([("license_plate".into(), literal.into())]),
            )
        }
        Request::Domain { domain } => (
            domain.clone(),
            "domain",
            BTreeMap::from([("domain".into(), domain.clone())]),
        ),
    };
    let capability = |slug: &str,
                      name: &str,
                      description: &str,
                      domain: &str,
                      inputs: &[&str],
                      outputs: &[&str],
                      source: Option<&str>| Capability {
        slug: slug.into(),
        name: name.into(),
        description: description.into(),
        domain: domain.into(),
        inputs: inputs.iter().map(|v| (*v).into()).collect(),
        outputs: outputs.iter().map(|v| (*v).into()).collect(),
        subject_id: Some("s1".into()),
        source_hint: source.map(str::to_owned),
        confidence: 1.0,
        missing: vec![],
    };
    let capabilities = match request {
        Request::Ecb { .. } => vec![capability("finance.ecb.reference.rate", "Latest ECB reference exchange rate",
            "Retrieve the latest published daily ECB reference exchange rate for the supplied EUR pair via Frankfurter, including the observation date. The date is returned data, not a requested historical date.",
            "finance", &["fx.ecb.quote"], &["rate", "date", "base", "quote"], Some("frankfurter"))],
        Request::Vat { .. } => vec![capability("company.vat.validation", "EU VAT validity",
            "Validate the supplied EU VAT number through VIES and return validity at its reported check date. This is not a tax-compliance conclusion.",
            "company", &["vat_number"], &["valid", "request_date"], Some("vies"))],
        Request::Vehicle { .. } => vec![capability("vehicle.technical.profile", "RDW vehicle data",
            "Retrieve the supplied Dutch registration plate's make, trade name, APK expiry and outstanding recall indicator from RDW. No owner identity or recall campaign details are requested.",
            "vehicle", &["license_plate"], &["merk", "handelsbenaming", "vervaldatum_apk", "openstaande_terugroepactie_indicator"], Some("rdw"))],
        Request::Domain { .. } => vec![
            capability("research.domain.registration", "Public RDAP registration data",
                "Retrieve the available public RDAP registration record for the supplied domain: registrar, registration events, status and nameservers. Preserve absent or redacted fields; do not infer registrant identity or control.",
                "research", &["domain"], &["domain", "registrar", "registered_at", "expires_at", "last_changed_at", "statuses", "nameservers"], None),
            capability("research.domain.mail.dns", "Domain mail DNS records",
                "Retrieve public DNS MX mail-routing records for the supplied domain, including the reported existence, mail acceptance and DNSSEC indicators. This requests MX, not TXT, A, AAAA or CNAME records, and does not prove mail delivery or ownership.",
                "research", &["domain"], &["domain", "mx", "exists", "accepts_mail", "dnssec_validated"], None),
        ],
    };
    Some(ParsedRequest {
        language: language.into(),
        subjects: vec![Subject {
            id: "s1".into(),
            label,
            kind: kind.into(),
            identifiers,
        }],
        capabilities,
        deliverable: Deliverable {
            format: "chat".into(),
            operations: vec!["list".into()],
        },
        meta: Meta {
            model: model.into(),
            parsed_at: chrono::Utc::now().to_rfc3339(),
            text_sha256: hex::encode(Sha256::digest(question.as_bytes())),
        },
    })
}

pub(crate) fn executable_intake(question: &str, intake: &Intake) -> Option<ParsedRequest> {
    let expected = parse(question, "literal-profession-request")?;
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn english_profession_questions_preserve_language_identifiers_and_complete_scope() {
        for (question, expected) in [
            ("Validate EU VAT number NL002033354B01 through VIES and show its validity and check date.", Request::Vat { country: "NL".into(), number: "002033354B01".into() }),
            ("Get the latest official ECB reference exchange rate for EUR/USD, including the observation date, via Frankfurter.", Request::Ecb { quote: "USD".into() }),
            ("Get RDW vehicle data for registration plate GZ805N, including make, trade name, inspection expiry date and the outstanding recall indicator.", Request::Vehicle { plate: "GZ805N".into() }),
            ("Get public RDAP registration data and mail DNS records for example.org. Cite the sources.", Request::Domain { domain: "example.org".into() }),
            ("Get the latest official ECB reference exchange rate for EUR/GBP, including the observation date, via Frankfurter.", Request::Ecb { quote: "GBP".into() }),
            ("Get RDW vehicle data for registration plate RZ-874-R, including make, trade name, inspection expiry date and the outstanding recall indicator.", Request::Vehicle { plate: "RZ874R".into() }),
            ("Get public RDAP registration data and mail DNS records for mollie.nl. Cite the sources.", Request::Domain { domain: "mollie.nl".into() }),
        ] {
            assert_eq!(recognize_with_language(question), Some(("en", expected)));
            let parsed = crate::capability_intake::normalize("{}", question, "fixture").unwrap();
            assert_eq!(parsed.language, "en");
            assert!(parsed.capabilities.iter().all(|cap| cap.missing.is_empty()));
            for extra in [" Also include owner identity.", " As JSON.", " Use data from 2020."] {
                assert!(recognize(&format!("{question}{extra}")).is_none(), "additional scope must remain requested");
            }
        }
        for question in [
            "Get the latest official ECB reference exchange rate for USD/EUR, including the observation date, via Frankfurter.",
            "Get the latest official ECB reference exchange rate for EUR/USD and GBP, including the observation date, via Frankfurter.",
            "Get public RDAP registration data and all DNS records for example.org. Cite the sources.",
            "Get public RDAP registration data and mail DNS records for www.example.org. Cite the sources.",
        ] { assert!(recognize(question).is_none(), "{question}"); }
    }

    #[test]
    fn literal_questions_keep_supplied_values_and_requested_fields() {
        for (question, expected) in [
            ("Geef de laatste officiële ECB-referentiewisselkoers voor EUR/USD met waarnemingsdatum via Frankfurter.", Request::Ecb { quote: "USD".into() }),
            ("Geef de laatste officiële ECB-referentiewisselkoers voor EUR/GBP met waarnemingsdatum via Frankfurter.", Request::Ecb { quote: "GBP".into() }),
            ("Valideer EU-btw-nummer NL002033354B01 via VIES en toon de geldigheid en controledatum.", Request::Vat { country: "NL".into(), number: "002033354B01".into() }),
            ("Valideer EU-btw-nummer BE0403170701 via VIES en toon de geldigheid en controledatum.", Request::Vat { country: "BE".into(), number: "0403170701".into() }),
            ("Geef RDW-voertuiggegevens voor kenteken GZ805N, inclusief merk, handelsbenaming, APK-vervaldatum en de indicator voor openstaande terugroepacties.", Request::Vehicle { plate: "GZ805N".into() }),
            ("Geef RDW-voertuiggegevens voor kenteken RZ-874-R, inclusief merk, handelsbenaming, APK-vervaldatum en de indicator voor openstaande terugroepacties.", Request::Vehicle { plate: "RZ874R".into() }),
            ("Geef openbare RDAP-registratiegegevens en mail-DNS-records voor example.org. Citeer de bronnen.", Request::Domain { domain: "example.org".into() }),
            ("Geef openbare RDAP-registratiegegevens en mail-DNS-records voor mollie.nl. Citeer de bronnen.", Request::Domain { domain: "mollie.nl".into() }),
        ] {
            assert_eq!(recognize(question), Some(expected));
            let parsed = crate::capability_intake::normalize("{}", question, "fixture").unwrap();
            assert_eq!(parsed.subjects.len(), 1);
            assert!(parsed.capabilities.iter().all(|cap| cap.missing.is_empty()));
            assert!(recognize(&format!("{question} Also include owner identity.")).is_none());
            assert!(recognize(&question.replace('.', ". As JSON.")).is_none());
        }
    }

    #[test]
    fn extra_scope_and_ambiguous_identifiers_stay_on_strict_interpretation_path() {
        for question in [
            "Geef de laatste officiële ECB-referentiewisselkoers voor USD/EUR met waarnemingsdatum via Frankfurter.",
            "Geef de laatste officiële ECB-referentiewisselkoers voor EUR/USD en GBP met waarnemingsdatum via Frankfurter.",
            "Geef de laatste officiële ECB-referentiewisselkoers voor EUR/USD met waarnemingsdatum via Frankfurter in 2020.",
            "Geef de intraday ECB-referentiewisselkoers voor EUR/USD met waarnemingsdatum via Frankfurter.",
            "Valideer EU-btw-nummer NL002033354B01 en BE0403170701 via VIES en toon de geldigheid en controledatum.",
            "Geef openbare RDAP-registratiegegevens en mail-DNS-records voor example.org en eigenaar. Citeer de bronnen.",
            "Geef openbare RDAP-registratiegegevens en mail-DNS-records voor example.org/path. Citeer de bronnen.",
            "Geef openbare RDAP-registratiegegevens en mail-DNS-records voor https://example.org. Citeer de bronnen.",
            "Geef openbare RDAP-registratiegegevens en mail-DNS-records voor -example.org. Citeer de bronnen.",
            "Geef openbare RDAP-registratiegegevens en alle DNS-records voor example.org. Citeer de bronnen.",
            "Geef openbare RDAP-registratiegegevens en mail-DNS-records voor www.example.org. Citeer de bronnen.",
            "Geef openbare RDAP-registratiegegevens en mail-DNS-records voor mail.example.co.uk. Citeer de bronnen.",
            "Geef openbare RDAP-registratiegegevens en mail-DNS-records voor example.de. Citeer de bronnen.",
            "Geef openbare RDAP-registratiegegevens en mail-DNS-records voor example.zz. Citeer de bronnen.",
        ] { assert!(recognize(question).is_none(), "{question}"); }
    }
}
