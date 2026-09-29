//! A realistic directory for calibrating search relevance and country ranking.
//! It merges the reviewed source fixtures (KVK, Companies House, Sirenic,
//! OpenSanctions, GLEIF, VIES, IBAN Check, Reuters, CBS, TED, TenderNed and the
//! Pulse Network operations), restores the live capability descriptions the
//! fixtures omit, and adds the WebberSites lookups seen in live testing.
use super::*;
use crate::catalog::{Catalog, KnownSource, Service, Snapshot};
use catalog_core::facts::contract::{InputBinding, Requirement};
use serde_json::json;
use std::sync::OnceLock;

fn snapshot(text: &str) -> Snapshot {
    serde_json::from_str(text).expect("fixture catalog")
}

/// Live `capabilities.description` values (first 400 characters, as the
/// catalog query serves them), for fixtures recorded without descriptions.
const DESCRIPTIONS: &[(&str, &str)] = &[
    ("company.registry-profile", "Retrieve the official registration record for a company from a national business register: statutory and trade names, registered activities (SBI codes), employee counts, legal form, registration date, addresses, website and branches. The authoritative record as filed with the chamber of commerce, as opposed to a scraped or inferred firmographic profile. Dutch Handelsregister (KVK) coverage: basisp"),
    ("company_registry.lookup", "Retrieve the complete official registration record for one company, including names, activities, legal form, addresses and register identifiers."),
    ("company_registry.search", "Search an official business register by company name, trade name, registration number, postcode or address and resolve the registered company identifiers."),
    ("company.financial-statements", "Retrieve structured financial data from the annual accounts a company filed with its business register: balance sheet, income statement, cash flow statement, consolidated variants, financial year and filing date. Official filed figures (jaarrekening, balans, winst- en verliesrekening, deponering), not estimates or modelled revenue. Availability depends on whether the company was required to file a"),
    ("company.officers", "Retrieve UK Companies House officers for one company; preserves pagination and total counts."),
    ("company.significant-control", "Retrieve UK Companies House persons-with-significant-control for one company; preserves pagination and total counts."),
    ("company.filing-history", "Retrieve UK Companies House filing-history for one company; preserves pagination and total counts."),
    ("company.uk-registry-search", "Find UK companies by name and resolve Companies House numbers."),
    ("company.sanctions-screen", "Screen a Company against OpenSanctions sanctions, PEP and watchlists; return candidate matches, not a definitive legal verdict."),
    ("company.watchlist-search", "Search OpenSanctions watchlists by company name; return candidates with source datasets."),
    ("company.watchlist-entity", "Read a specific OpenSanctions entity and its relationships."),
    ("company.watchlist-statements", "Read source assertions for one OpenSanctions entity."),
    ("company.legal-entity-search", "Search legal entity names in GLEIF; optional explicit country filter."),
    ("company.lei-verification", "Retrieve the legal entity and registration status for an exact LEI."),
    ("company.direct-parent", "Retrieve GLEIF direct-parent-relationship for an exact LEI. Accounting consolidation relationships, not a complete UBO register."),
    ("company.ultimate-parent", "Retrieve GLEIF ultimate-parent-relationship for an exact LEI. Accounting consolidation relationships, not a complete UBO register."),
    ("company.direct-children", "Retrieve GLEIF direct-children for an exact LEI. Accounting consolidation relationships, not a complete UBO register."),
    ("company.ultimate-children", "Retrieve GLEIF ultimate-children for an exact LEI. Accounting consolidation relationships, not a complete UBO register."),
    ("company.prospect-search", "Find companies by explicit HQ location, keywords, industry and employee range; one page, up to 10 results. Does not establish two-year growth."),
    ("company.domain-resolution", "Resolve a company name to matching domains; let the user choose when several companies match."),
    ("company.enrich", "Enrich a company from its explicit or resolved domain, preserving employee and revenue ranges."),
    ("company.enrich-by-name", "Get a best-match CompanyEnrich profile by company name; verify its returned name and domain before reusing it."),
    ("company.fr-financials", "French company financial statements from the official INPI/RNE filings by SIREN: revenue, net income, balance sheet and filed annual accounts."),
    ("company.fr-registry-search", "Search the French company register (SIRENE) by name and resolve the SIREN."),
    ("company.eu-registry-search", "Search European national business registers by company name and country."),
    ("company.sirenic-eu-entreprise-gb-company-number-dirigeants", "GB: UK company officers live from Companies House, the official UK company registry, by company number (e.g. GB/00102498): active directors and secretaries with role, appointment date, nationality, occupation, country of residence and month+year of birth only — never a correspondence address (GDPR minimisation). Resigned-officer count included. Built for KYB and due diligence on director appointme"),
    ("company.sirenic-eu-entreprise-no-id-comptes", "NO: Norwegian company annual accounts and financial statements from Norway's official Regnskapsregisteret (Brønnøysundregistrene, Brreg, NLOD 2.0). Query: /v1/eu/entreprise/NO/923609016/comptes (9-digit organisasjonsnummer). Latest filed fiscal year live from the register plus earlier years collected since 2026-07 — financials: balance sheet (assets, equity, debts), P&L (operating income and resul"),
    ("company.sirenic-eu-entreprise-lv-regnr-comptes", "LV: Latvian company annual accounts — official financial statements from VID filings via the Uzņēmumu reģistrs open data of Latvia (CC0, daily). Query: /v1/eu/entreprise/LV/40003032065/comptes (11-digit registration number). EVERY filed fiscal year in one call: balance sheet, profit and loss (revenue, net income), cash flow when filed, employees, as published (EUR, LVL before 2014; statutory and c"),
    ("company.sirenic-tva-verifier-numero", "EU VAT validation against the official VIES service: validate any intra-community VAT number (all member states) for compliance and due diligence. This tax id check is the tax half of verify supplier before payment, and a required check under the e-invoicing mandate France September 2026. Returns valid/invalid/unavailable status with the VIES consultation identifier as proof — VAT number validatio"),
    ("company.sirenic-iban-verifier-iban", "IBAN verification and bank validation for SEPA payments: a FORM check to verify a supplier before payment — ISO 13616 structure + mod-97 key, plus bank identification from FREE official sources (FR via ACPR/REGAFI incl. LEI and SIREN; BE NBB; AT OeNB incl. LEI; NL BIC list; DE Bundesbank file). French BICs via GLEIF/SWIFT BIC-to-LEI. NOT a payee verification for instant payments: neither the holde"),
    ("company.sirenic-eu-agrements", "EU financial authorisation lookup in the official ESMA Registers: search ~14,000 MiFID-regulated entities across the EU/EEA (investment firms, UCITS/AIFM managers) by name or LEI — authorisation status of the regulated entity, home and host member states, competent authority, authorisation dates. One integration covers all 27+ countries of Europe: an investment firm and regulated entity KYB check "),
    ("company.sirenic-entreprise-siren-marches-publics-ue", "European public procurement contract awards won by a French company, from TED (Tenders Electronic Daily, EU Publications Office): government contracts and tenders — buyer, country, subject, notice-level amount, CPV codes, co-winners and official notice link. Identifier-based matching only (SIREN/SIRET incl. spaced variants). Coverage: eForms award notices since 2023-10-25, above EU thresholds; ~57"),
    ("vat.uk.validate", "Validate one GB VAT number via Ask Merlin (HMRC-derived, at most one-hour cache); not company-name search"),
    ("vat.eu.validate", "Validate a VAT number using VIES; Validate EU and XI VAT numbers and inspect VIES service availability. Names and addresses depend on national availability."),
    ("vat.eu.check", "Check an EU VAT number against VIES with the requester's own VAT number for a consultation reference."),
    ("vat.eu.service-status", "Inspect VIES availability per member state."),
    ("iban.validate", "Validate an IBAN: country and length, ISO 7064 mod-97 check digits, SEPA membership and the bank and BIC behind Dutch bank codes. Does not confirm that the account exists or who holds it."),
    ("news.reuters-search", "Search Reuters news headlines by keywords (a company, person, topic or event). Returns up to 12 article links and headlines, no dates; keyword matching is loose."),
    ("news.reuters-latest", "Latest Reuters news headlines across all sections, newest wire stories with link and publication time. Optional headline count 1-12. Headlines only, not article text."),
    ("news.reuters-section-us", "Current Reuters US news headlines: up to 12 article links and headlines from the Reuters US section front page (https://www.reuters.com/world/us/), in page order. Headlines only, not article text."),
    ("news.reuters-section-uk", "Current Reuters UK news headlines: up to 12 article links and headlines from the Reuters UK section front page, in page order. Headlines only, not article text."),
    ("news.reuters-section-business", "Current Reuters business news headlines: up to 12 article links and headlines from the Reuters business section front page, in page order. Headlines only, not article text."),
    ("news.bloomberg-latest", "Latest Bloomberg news stories with headline, link and publication time."),
    ("statistics.cbs-bankruptcies", "CBS response page. Preserve counts, periods and measure codes; a bounded page is not a complete dataset. Resolve codes and units before analysis."),
    ("statistics.cbs-company-counts", "CBS response page. Preserve counts, periods and measure codes; a bounded page is not a complete dataset. Resolve codes and units before analysis."),
    ("statistics.cbs-business-closures", "CBS response page. Preserve counts, periods and measure codes; a bounded page is not a complete dataset. Resolve codes and units before analysis."),
    ("statistics.eurostat-data", "Eurostat response with dimensions, labels, units and periods; preserve all metadata."),
    ("statistics.eurostat-dataset-search", "Eurostat response with dimensions, labels, units and periods; preserve all metadata."),
    ("procurement.ted-kvk-notices", "Search TED notices by exact numeric KVK/RSIN in buyer or winner identifier fields. Identifier scheme and legal-entity identity require confirmation. Includes historical notices."),
    ("procurement.ted-rsin-notices", "Search TED notices by exact numeric KVK/RSIN in buyer or winner identifier fields. Identifier scheme and legal-entity identity require confirmation. Includes historical notices."),
    ("procurement.company-name-mentions", "Search TenderNed publication text using the KVK registered company name. Candidate mentions only: not proof of a supplier award, legal-entity match or subsidy receipt."),
    ("procurement.nl-public-contracts", "Browse Dutch public government contracts and procurement notices in the Netherlands via TenderNed. No keyword or company identifier required; optional literal search/date/sector filters. One page of 20, sorted by publication date. Preserve notice types: announcements, awards, corrections and cancellations. Not all are open tenders or awarded contracts; no full contract PDFs."),
    ("government.publication.search", "Official notice title search, authority and dates; one page"),
    ("vehicle.technical-profile", "Direct RDW technical profile by plate, excluding fuel/emissions"),
    ("building.bag-unit", "Retrieve the exact BAG verblijfsobject and its explicit building links."),
    ("building.bag-building", "Follow the single building explicitly linked by BAG; return construction year, building ID and status."),
    ("location.bag-address-context", "Resolve a BAG addressable-object ID to address, municipality, neighbourhood and linked parcels. No owner inference."),
    ("company.registered-address-context", "Match the exact KVK main visiting address to BAG IDs, municipality, neighbourhood and linked parcels. Address suffixes must match; ambiguous results require selection."),
    ("area.cbs-neighbourhood-2025", "Retrieve CBS 2025 neighbourhood statistics via PDOK using an exact neighbourhood code. Area aggregates, not company facts or property WOZ."),
    ("property.linked-parcels", "Retrieve up to 25 explicitly linked cadastral map parcels in one request. Preserve all designations; no ownership claims."),
    ("web.source-search", "Search the public web for relevant pages and supporting evidence."),
    ("web.source-fetch", "Retrieve the content of one public web page."),
];

/// The WebberSites lookups the live Ask page offered for "current local time
/// in Amsterdam": an IP geolocation and a coordinate lookup, both returning a
/// timezone. The capability description is the live keyword list.
fn webbersites(template: &Service) -> (Vec<Service>, KnownSource) {
    const DESCRIPTION: &str = "Webgereedschap: opzoeken. Zoekwoorden: blocked, catalog, citations, city, cleanup, collection, collections, content, coordinates, country, csv, datastore.";
    let lookup = |id: u128, inputs: &[(&str, &str)], outputs: &[&str]| {
        let mut service = template.clone();
        service.id = uuid::Uuid::from_u128(0x5eb0_0000 + id);
        service.endpoint_id = uuid::Uuid::from_u128(0x5eb1_0000 + id);
        service.unverified = false;
        service.api_slug = "webbersites".into();
        service.name = "WebberSites x402 Data API".into();
        service.description = Some(DESCRIPTION.into());
        service.jurisdiction = None;
        service.list_atomic = 1000;
        service.contract.capability_slug = "web_tools.lookup".into();
        service.contract.requires = inputs
            .iter()
            .map(|(fact, _)| Requirement {
                fact_type: (*fact).into(),
                subject_role: "subject".into(),
                optional: false,
            })
            .collect();
        service.contract.input_map = inputs
            .iter()
            .map(|(fact, name)| {
                let binding: InputBinding =
                    serde_json::from_value(json!({"in":"query","name":name})).unwrap();
                ((*fact).to_owned(), binding)
            })
            .collect();
        let extract = template
            .contract
            .output_map
            .produces
            .values()
            .next()
            .cloned()
            .expect("template output");
        service.contract.output_map.produces = outputs
            .iter()
            .map(|key| ((*key).to_owned(), extract.clone()))
            .collect();
        service
    };
    let services = vec![
        lookup(
            1,
            &[("web_tools.ip", "ip")],
            &[
                "web_tools.city",
                "web_tools.country",
                "web_tools.timezone",
                "web_tools.coordinates",
            ],
        ),
        lookup(
            2,
            &[("web_tools.lat", "lat"), ("web_tools.lng", "lng")],
            &["web_tools.city", "web_tools.country", "web_tools.timezone"],
        ),
    ];
    let source: KnownSource = serde_json::from_value(json!({
        "slug":"webbersites","name":"WebberSites x402 Data API","block_reason":null,
        "metadata":{"description":"Pay-per-call data and utility APIs for AI agents, including web and SEO tools, accessibility checks, design generation, crypto market data, persistent wallet-owned agent memory, and flat-priced LLM routes.",
            "category":"tools","tags":["LLM","SEO","accessibility","agent memory","ai","album metadata"],
            "capability_descriptions":{"web_tools.lookup":DESCRIPTION}}
    }))
    .unwrap();
    (services, source)
}

/// PDOK's cadastral parcel lookup as the live catalog carries it. Live search
/// offered it second for "Coolblue B.V." company profile: its land-register
/// wording ("national land register", "registered surface area", status
/// history filters) overlaps a company profile's outputs, and the Dutch
/// subject lifted it.
fn pdok_parcel_lookup(template: &Service) -> Service {
    const DESCRIPTION: &str = "Retrieve the cadastral parcel covering a point or area from a national land register: its official designation, boundary geometry and registered surface area. Answers which plot a location sits on and how big that plot is. Carries no ownership or personal data — a public cadastral map records the parcel, never who owns it. Dutch BRK coverage: kadastrale kaart, perceel, kadastrale gemeente, sectie,";
    let fields = [
        ("property.input.bbox", "bbox", false),
        ("property.sectie", "sectie", true),
        ("property.input.perceelnummer", "perceelnummer", true),
        ("property.input.statushistoriecode", "statusHistorieCode", true),
        ("property.statushistoriewaarde", "statusHistorieWaarde", true),
        ("property.kadastralegemeentecode", "kadastraleGemeenteCode", true),
        ("property.kadastralegemeentewaarde", "kadastraleGemeenteWaarde", true),
    ];
    let mut service = template.clone();
    service.id = uuid::Uuid::from_u128(0x9d0c_0001);
    service.endpoint_id = uuid::Uuid::from_u128(0x9d0c_1001);
    service.unverified = false;
    service.name = "PDOK Maps & Addresses".into();
    service.description = Some(DESCRIPTION.into());
    service.contract.capability_slug = "property.parcel-lookup".into();
    let extract = template
        .contract
        .output_map
        .produces
        .values()
        .next()
        .cloned()
        .expect("template output");
    service.contract.output_map.produces = [
        "property.perceel.designation",
        "property.perceel.registered_area",
        "property.perceel.status",
        "property.perceel.geometry",
    ]
    .iter()
    .map(|key| ((*key).to_owned(), extract.clone()))
    .collect();
    service.contract.requires = fields
        .iter()
        .map(|(fact, _, optional)| Requirement {
            fact_type: (*fact).into(),
            subject_role: "subject".into(),
            optional: *optional,
        })
        .collect();
    service.contract.input_map = fields
        .iter()
        .map(|(fact, name, _)| {
            let binding: InputBinding =
                serde_json::from_value(json!({"in":"query","name":name})).unwrap();
            ((*fact).to_owned(), binding)
        })
        .collect();
    service
}

/// Built once: the merged directory is large and immutable.
pub(super) fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let fixtures = [
            include_str!("../../../tests/fixtures/source-operations/catalog.json"),
            include_str!("../../../tests/fixtures/workflows/catalog.json"),
            include_str!("../../../tests/fixtures/company-sources/catalog.json"),
            include_str!("../../../tests/fixtures/dutch-links/catalog.json"),
            include_str!("../../../tests/fixtures/eu-public-sources/catalog.json"),
            include_str!("../../../tests/fixtures/public-statistics/catalog.json"),
            include_str!("../../../tests/fixtures/rapidapi-news/catalog.json"),
            include_str!("../../../tests/fixtures/uk-supplier/catalog.json"),
            include_str!("../../../tests/fixtures/iban-check/services.json"),
            include_str!("../../../tests/fixtures/sirenic/expanded-catalog.json"),
        ];
        let mut services: Vec<Service> = Vec::new();
        let mut known_sources: Vec<KnownSource> = Vec::new();
        let mut seen = BTreeSet::new();
        for text in fixtures {
            let snapshot = snapshot(text);
            for service in snapshot.services {
                if seen.insert((
                    service.endpoint_id,
                    service.contract.capability_slug.clone(),
                )) {
                    services.push(service);
                }
            }
            for source in snapshot.known_sources {
                if !known_sources.iter().any(|known| known.slug == source.slug) {
                    known_sources.push(source);
                }
            }
        }
        for service in &mut services {
            if service.description.as_deref().is_none_or(str::is_empty) {
                if let Some((_, text)) = DESCRIPTIONS
                    .iter()
                    .find(|(slug, _)| *slug == service.contract.capability_slug)
                {
                    service.description = Some((*text).into());
                }
            }
        }
        let template = services
            .iter()
            .find(|service| service.api_slug == "iban-check")
            .cloned()
            .expect("IBAN Check fixture");
        let (lookups, source) = webbersites(&template);
        services.extend(lookups);
        let parcels = services
            .iter()
            .find(|service| service.contract.capability_slug == "property.linked-parcels")
            .cloned()
            .expect("PDOK linked parcels fixture");
        services.push(pdok_parcel_lookup(&parcels));
        known_sources.retain(|known| known.slug != "webbersites");
        known_sources.push(source);
        let version = crate::state::digest(&(&services, &known_sources));
        Catalog {
            services,
            vocabulary: Default::default(),
            version: format!("calibration-{version}"),
            rejected: vec![],
            by_capability: Default::default(),
            by_output: Default::default(),
            known_sources,
        }
    })
}

/// One capability as the Ask parser writes it, about one subject.
struct Ask<'a> {
    slug: &'a str,
    name: &'a str,
    description: &'a str,
    inputs: &'a [&'a str],
    outputs: &'a [&'a str],
    hint: Option<&'a str>,
    subject: Option<(&'a str, &'a str)>,
    identifiers: &'a [(&'a str, &'a str)],
    language: &'a str,
}
impl Default for Ask<'_> {
    fn default() -> Self {
        Self {
            slug: "",
            name: "",
            description: "",
            inputs: &[],
            outputs: &[],
            hint: None,
            subject: None,
            identifiers: &[],
            language: "en",
        }
    }
}
impl Ask<'_> {
    fn parsed(&self) -> ParsedRequest {
        let (label, kind) = self.subject.unwrap_or(("", ""));
        let subjects = if label.is_empty() {
            json!([])
        } else {
            json!([{"id":"s1","label":label,"type":kind,
                "identifiers":self.identifiers.iter().map(|(k, v)| (k.to_string(), json!(v))).collect::<serde_json::Map<_, _>>()}])
        };
        let request = format!(
            "{label} {} {}",
            self.identifiers
                .iter()
                .map(|(_, v)| *v)
                .collect::<Vec<_>>()
                .join(" "),
            self.hint.unwrap_or("")
        );
        crate::capability_intake::normalize(&json!({"language":self.language,"subjects":subjects,
            "capabilities":[{"slug":self.slug,"name":self.name,"description":self.description,"domain":self.slug.split('.').next().unwrap(),
                "inputs":self.inputs,"outputs":self.outputs,"subject_id":if label.is_empty() {json!(null)} else {json!("s1")},
                "source_hint":self.hint,"confidence":0.9}],
            "deliverable":{"format":"chat","operations":[]}}).to_string(), &request, "fixture").unwrap()
    }
}

fn profile(subject: &'static str) -> Ask<'static> {
    Ask {
        slug: "company.profile",
        name: "Company profile",
        description: "Retrieve basic company profile information.",
        inputs: &["company_name"],
        outputs: &["legal_name", "status", "registered_address"],
        subject: Some((subject, "company")),
        ..Default::default()
    }
}

/// `source:capability` of each offered candidate, in order.
fn offered(ask: &Ask) -> Vec<String> {
    search(&ask.parsed(), catalog())[0]
        .apiosk
        .iter()
        .map(|candidate| {
            format!(
                "{}:{}",
                candidate.source_slug.as_deref().unwrap_or_default(),
                candidate.capability.as_deref().unwrap_or("-")
            )
        })
        .collect()
}

/// The registry-profile endpoint of `source` whose inputs include `input`.
fn registry_profile(source: &str, input: &str) -> String {
    catalog()
        .services
        .iter()
        .find(|service| {
            service.api_slug == source
                && service.contract.capability_slug == "company.registry-profile"
                && service.contract.input_map.contains_key(input)
        })
        .map(|service| service.endpoint_id.to_string())
        .unwrap_or_else(|| panic!("{source} registry profile with {input}"))
}

#[test]
fn current_local_time_for_a_city_offers_nothing_instead_of_city_keyed_services() {
    let catalog = catalog();
    let index = index(catalog);
    for ask in [
        Ask {
            slug: "location.time.current",
            name: "Current local time",
            description: "Retrieve the current local time and timezone for a city.",
            inputs: &["city"],
            outputs: &["local_time", "timezone"],
            subject: Some(("Amsterdam", "city")),
            ..Default::default()
        },
        Ask {
            slug: "location.timezone",
            name: "Timezone",
            description: "Retrieve the timezone of a city.",
            inputs: &["city"],
            outputs: &["timezone"],
            subject: Some(("Amsterdam", "city")),
            ..Default::default()
        },
    ] {
        let parsed = ask.parsed();
        // The live junk still shares words with the request: the WebberSites
        // IP and coordinate lookups mention `city` and return a `timezone`,
        // the Pulse guides mention local resources and real-time data.
        let capability = &parsed.capabilities[0];
        let mut generic = capability.clone();
        generic.name = format!(
            "{} {} {}",
            capability.slug,
            capability.name,
            capability.outputs.join(" ")
        );
        generic.source_hint = None;
        let wanted = document(
            Target::Source(0),
            &capability_query(&generic, &parsed, &Task::default()),
            normalized_capabilities(std::iter::once(capability.slug.as_str())),
        );
        let lexical: Vec<_> = index
            .documents
            .iter()
            .filter(|candidate| score(&wanted, candidate, &index) > 0.0)
            .collect();
        assert!(lexical.iter().any(|candidate| matches!(candidate.target,
            Target::Endpoint(position) if catalog.services[position].api_slug == "webbersites")));
        assert!(lexical.len() > 20, "{}", lexical.len());
        let found = search(&parsed, catalog);
        assert!(
            found[0].apiosk.is_empty(),
            "{}: {:?}",
            ask.slug,
            offered(&ask)
        );
        assert!(found[0].operations.is_empty());
    }
}

#[test]
fn real_capabilities_keep_their_sources_and_lose_look_alikes() {
    for (ask, first, present, absent) in [
        (
            profile("Coolblue"),
            "sirenic:company.registry-profile",
            &["companies-house:company.registry-profile"][..],
            &["compliancepulse"][..],
        ),
        (
            Ask {
                slug: "company.officers",
                name: "Company officers",
                description: "Retrieve the current directors and officers of a company.",
                inputs: &["company_name"],
                outputs: &["officers", "appointment_date"],
                subject: Some(("Tesco PLC", "company")),
                ..Default::default()
            },
            "companies-house:company.officers",
            &["sirenic:company.sirenic-eu-entreprise-gb-company-number-dirigeants"][..],
            &["citepulse"][..],
        ),
        (
            Ask {
                slug: "company.financial.statements",
                name: "Filed annual accounts",
                description: "Retrieve the annual accounts filed by a company.",
                inputs: &["company_name"],
                outputs: &["annual_accounts", "balance_sheet"],
                subject: Some(("Coolblue B.V.", "company")),
                ..Default::default()
            },
            "kvk-dutch-business-register:company.financial-statements",
            &[][..],
            &["clearcarepulse"][..],
        ),
        (
            Ask {
                slug: "compliance.sanctions.screen",
                name: "Sanctions screening",
                description: "Screen a company against international sanctions lists.",
                inputs: &["company_name"],
                outputs: &["sanctions_matches", "match_score"],
                ..Default::default()
            },
            "opensanctions:company.sanctions-screen",
            &["compliancepulse:operation.compliancepulse.get-api-screen-sanctions-96e543e5"][..],
            &[][..],
        ),
        // A Dutch VAT number: VIES covers it, Ask Merlin (GB only) does not.
        (
            Ask {
                slug: "finance.vat.validate",
                name: "VAT number validation",
                description: "Check whether a VAT number is valid and registered.",
                inputs: &["vat_number"],
                outputs: &["valid", "registered_name"],
                subject: Some(("Coolblue", "company")),
                identifiers: &[("vat_number", "NL810433941B01")],
                ..Default::default()
            },
            "sirenic:company.sirenic-tva-verifier-numero",
            &["vies:vat.eu.validate"][..],
            &[
                "iban-check",
                "company.sirenic-iban-verifier-iban",
                "ask-merlin",
            ][..],
        ),
        (
            Ask {
                slug: "finance.iban.validate",
                name: "IBAN validation",
                description: "Validate an IBAN and return the bank details.",
                inputs: &["iban"],
                outputs: &["valid", "bank_name", "bic"],
                ..Default::default()
            },
            "iban-check:iban.validate",
            &["sirenic:company.sirenic-iban-verifier-iban"][..],
            &["vies", "tva-verifier", "vat."][..],
        ),
        (
            Ask {
                slug: "research.news.company",
                name: "Latest company news",
                description: "Retrieve recent news articles about a company.",
                inputs: &["company_name"],
                outputs: &["headlines", "published_at", "article_url"],
                ..Default::default()
            },
            "reuters:news.reuters-search",
            &["reuters:news.reuters-latest"][..],
            &["pulse"][..],
        ),
        (
            Ask {
                slug: "research.news.company",
                name: "Latest company news",
                description: "Retrieve recent news articles about a company from Reuters.",
                inputs: &["company_name"],
                outputs: &["headlines"],
                hint: Some("reuters"),
                ..Default::default()
            },
            "reuters:news.reuters-search",
            &[][..],
            &["bloomberg"][..],
        ),
        (
            Ask {
                slug: "research.statistics.bankruptcies",
                name: "Bankruptcy statistics",
                description:
                    "Retrieve official statistics on the number of company bankruptcies per month.",
                inputs: &["period"],
                outputs: &["bankruptcy_count", "period"],
                ..Default::default()
            },
            "cbs-statline:statistics.cbs-bankruptcies",
            &[][..],
            &["macropulse", "proppulse"][..],
        ),
        (
            Ask {
                slug: "procurement.tenders.search",
                name: "Public tenders",
                description: "Search public procurement tender notices.",
                inputs: &["keyword"],
                outputs: &["notice_title", "buyer", "deadline"],
                ..Default::default()
            },
            "sirenic:company.sirenic-entreprise-siren-marches-publics-ue",
            &[
                "ted:procurement.ted-kvk-notices",
                "tenderned:procurement.company-name-mentions",
            ][..],
            &["policypulse", "tablepulse"][..],
        ),
        (
            Ask {
                slug: "company.registry.search",
                name: "Company registry search",
                description: "Search the business register for companies by name.",
                inputs: &["company_name"],
                outputs: &["registration_number", "legal_name"],
                ..Default::default()
            },
            "kvk-dutch-business-register:company_registry.search",
            &[][..],
            &["pulse"][..],
        ),
        (
            Ask {
                slug: "company.lei.lookup",
                name: "Legal entity identifier",
                description: "Look up the legal entity identifier record of a company.",
                inputs: &["company_name"],
                outputs: &["lei", "legal_name"],
                ..Default::default()
            },
            "gleif:company.lei-verification",
            &[][..],
            &["iban", "pulse"][..],
        ),
    ] {
        let found = offered(&ask);
        assert_eq!(
            found.first().map(String::as_str),
            Some(first),
            "{}: {found:?}",
            ask.slug
        );
        for expected in present {
            assert!(
                found.iter().any(|candidate| candidate == expected),
                "{}: {expected} in {found:?}",
                ask.slug
            );
        }
        for unwanted in absent {
            assert!(
                !found.iter().any(|candidate| candidate.contains(unwanted)),
                "{}: {unwanted} in {found:?}",
                ask.slug
            );
        }
    }
}

#[test]
fn a_company_profile_is_never_answered_by_a_land_register() {
    // Live, 2026-09-29: PDOK's parcel lookup came second for Coolblue B.V.,
    // with the parse's own wording ("…from an official business register").
    for subject in ["Coolblue B.V.", "Coolblue", "Acme Ltd"] {
        let offered = offered(&Ask {
            description: "Returns a company's registered profile, such as its legal name, status and address, from an official business register.",
            ..profile(subject)
        });
        assert!(
            !offered.iter().any(|candidate| candidate.contains("property.")),
            "{subject}: {offered:?}"
        );
    }
}

#[test]
fn the_subjects_country_puts_its_own_register_first() {
    let first = |subject: &'static str| {
        search(&profile(subject).parsed(), catalog())[0].apiosk[0]
            .endpoint_id
            .clone()
    };
    let kvk = registry_profile("kvk-dutch-business-register", "company_registry.kvknummer");
    let companies_house =
        registry_profile("companies-house", "company.registration.gb.companies_house");
    let sirenic_france = registry_profile("sirenic", "company.registration.fr.siren");
    let sirenic_europe = registry_profile("sirenic", "company.registration.eu.national_id");
    assert_eq!(first("Coolblue B.V."), Some(kvk.clone()));
    assert_eq!(first("Acme Ltd"), Some(companies_house.clone()));
    assert_eq!(first("Example SAS"), Some(sirenic_france.clone()));
    // A Dutch identifier or named source works without a legal form.
    let with_kvk = Ask {
        identifiers: &[("kvk_number", "24285251")],
        ..profile("Coolblue")
    };
    assert_eq!(
        search(&with_kvk.parsed(), catalog())[0].apiosk[0].endpoint_id,
        Some(kvk.clone())
    );
    // The European lookup covers the Netherlands and follows KVK; the French
    // and British registers fall behind every Dutch or European candidate.
    let dutch = search(&profile("Coolblue B.V.").parsed(), catalog())[0]
        .apiosk
        .clone();
    assert_eq!(dutch[0].endpoint_id, Some(kvk.clone()));
    assert_eq!(dutch[1].endpoint_id, Some(sirenic_europe.clone()));
    assert_eq!(
        dutch[2].source_slug.as_deref(),
        Some("kvk-dutch-business-register")
    );
    // Before: the cheapest copy of the contract (Sirenic France) led.
    let plain: Vec<_> = search(&profile("Coolblue").parsed(), catalog())[0]
        .apiosk
        .iter()
        .filter_map(|candidate| candidate.endpoint_id.clone())
        .collect();
    assert_eq!(plain, [sirenic_france, sirenic_europe, companies_house]);
}

#[test]
fn a_foreign_register_is_demoted_but_still_offered_when_nothing_else_fits() {
    // No Dutch source lists officers: the British registers remain.
    let ask = Ask {
        slug: "company.officers",
        name: "Company officers",
        description: "Retrieve the current directors and officers of a company.",
        inputs: &["company_name"],
        outputs: &["officers"],
        subject: Some(("Coolblue B.V.", "company")),
        ..Default::default()
    };
    let found = offered(&ask);
    assert_eq!(
        found.first().map(String::as_str),
        Some("companies-house:company.officers"),
        "{found:?}"
    );
    assert_eq!(found.len(), 3);
}

#[test]
fn without_a_country_signal_the_lexical_order_is_unchanged() {
    let catalog = catalog();
    let index = index(catalog);
    for ask in [
        profile("Coolblue"),
        Ask {
            slug: "finance.vat.validate",
            name: "VAT number validation",
            description: "Check whether a VAT number is valid and registered.",
            inputs: &["vat_number"],
            outputs: &["valid", "registered_name"],
            ..Default::default()
        },
        Ask {
            slug: "company.registry.search",
            name: "Company registry search",
            description: "Search the business register for companies by name.",
            inputs: &["company_name"],
            outputs: &["registration_number"],
            subject: Some(("Coolblue", "company")),
            ..Default::default()
        },
    ] {
        let parsed = ask.parsed();
        let capability = &parsed.capabilities[0];
        assert!(country::Signal::infer(&parsed, capability).is_empty());
        let mut generic = capability.clone();
        generic.name = format!(
            "{} {} {}",
            capability.slug,
            capability.name,
            capability.outputs.join(" ")
        );
        generic.source_hint = None;
        let wanted = document(
            Target::Source(0),
            &capability_query(&generic, &parsed, &Task::default()),
            normalized_capabilities(std::iter::once(capability.slug.as_str())),
        );
        let floor = relevance::Request::new(capability, &parsed);
        let mut lexical: Vec<_> = index
            .documents
            .iter()
            .filter_map(|candidate| match candidate.target {
                Target::Endpoint(position) => {
                    let score = score(&wanted, candidate, &index);
                    (score > 0.0 && floor.passes(&wanted, candidate, &index, false))
                        .then(|| (score, &catalog.services[position]))
                }
                Target::Source(_) => None,
            })
            .collect();
        lexical.sort_by(|a, b| {
            b.0.total_cmp(&a.0)
                .then(a.1.unverified.cmp(&b.1.unverified))
                .then(a.1.total().cmp(&b.1.total()))
                .then(a.1.api_slug.cmp(&b.1.api_slug))
                .then(a.1.endpoint_id.cmp(&b.1.endpoint_id))
        });
        let mut seen = BTreeSet::new();
        let expected: Vec<_> = lexical
            .iter()
            .filter(|(_, service)| seen.insert(service.endpoint_id))
            .take(3)
            .map(|(_, service)| Some(service.endpoint_id.to_string()))
            .collect();
        let found: Vec<_> = search(&parsed, catalog)[0]
            .apiosk
            .iter()
            .map(|candidate| candidate.endpoint_id.clone())
            .collect();
        assert_eq!(found, expected, "{}", ask.slug);
    }
}
