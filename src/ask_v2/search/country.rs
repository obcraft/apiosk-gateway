//! A deterministic country signal for Ask search ranking.
//!
//! A registry answers only for its own country: KVK knows Dutch companies,
//! Companies House British ones, and SIRENE French ones. Search therefore
//! infers where the subject is registered (identifiers, legal form, a named
//! source, requested inputs, and weakly the request language) and where each
//! candidate can answer (identifier inputs, country-specific contract ids,
//! source identity, and the catalog's jurisdiction). When one country is
//! clearly named, its candidates' relevance is multiplied by 1.5 (1.25 for a
//! European register that includes it), and candidates bound to another
//! country rank after every other relevant candidate but are never removed.
//! Weak evidence (the request language, a legal form several countries use)
//! only breaks near-ties; without any evidence the order is unchanged.
use crate::{
    capability_intake::{Capability, ParsedRequest},
    catalog::{KnownSource, Service},
};
use std::collections::{BTreeMap, BTreeSet};

/// A single signal at or above this weight names the subject's country. Legal
/// forms shared by several countries and the request language stay below it.
const STRONG: f64 = 2.0;
/// Relevance factor for a candidate bound to the subject's country. It lifts
/// the subject's own register above other registers' copies of one contract,
/// but not a marginal match above a much stronger one.
const MATCH: f64 = 1.5;
/// A multi-country European register that includes the subject's country.
const EUROPE_MATCH: f64 = 1.25;
/// Weak signals only break near-ties between otherwise equal candidates.
const NUDGE: f64 = 0.05;

const IDENTIFIER: f64 = 3.0;
const NAMED_SOURCE: f64 = 3.0;
const INPUT: f64 = 2.0;
const LEGAL_FORM: f64 = 2.0;
const SHARED_FORM: f64 = 1.0;
const WEAK: f64 = 1.0;

/// EU member states and the EEA: the reach of VIES, TED, Eurostat and the
/// European national-register lookups.
const EUROPE: &[&str] = &[
    "AT", "BE", "BG", "CY", "CZ", "DE", "DK", "EE", "ES", "FI", "FR", "GR", "HR", "HU", "IE", "IT",
    "LT", "LU", "LV", "MT", "NL", "PL", "PT", "RO", "SE", "SI", "SK", "IS", "LI", "NO",
];
/// European, but outside the EU registers above; neither boosted nor demoted.
const NEAR_EUROPE: &[&str] = &["GB", "CH"];

/// Where a candidate can answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Coverage {
    /// Global or not known: never boosted or demoted.
    Unknown,
    /// Bound to one country (ISO 3166-1 alpha-2).
    Country(String),
    /// Several European registers behind one country selector.
    Europe,
}

/// How the country signal moves one candidate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Adjustment {
    /// Multiplies the candidate's relevance.
    pub factor: f64,
    /// Bound to a different country than the subject's: listed after every
    /// candidate that is not.
    pub demoted: bool,
}
impl Default for Adjustment {
    fn default() -> Self {
        Self {
            factor: 1.0,
            demoted: false,
        }
    }
}
const DEMOTED: Adjustment = Adjustment {
    factor: 1.0,
    demoted: true,
};
fn factor(factor: f64) -> Adjustment {
    Adjustment {
        factor,
        demoted: false,
    }
}

/// Evidence for the subject's country, as weights per ISO code.
#[derive(Clone, Debug, Default)]
pub(super) struct Signal {
    weights: BTreeMap<String, f64>,
}

impl Signal {
    pub fn infer(parsed: &ParsedRequest, capability: &Capability) -> Self {
        let mut signal = Self::default();
        let subject = match capability.subject_id.as_deref() {
            Some(id) => parsed.subjects.iter().find(|subject| subject.id == id),
            None if parsed.subjects.len() == 1 => parsed.subjects.first(),
            None => None,
        };
        if let Some(subject) = subject {
            for (key, value) in &subject.identifiers {
                signal.identifier(key, value);
            }
            if !matches!(subject.kind.as_str(), "person" | "individual" | "people") {
                signal.label(&subject.label);
            }
        }
        if let Some(hint) = capability.source_hint.as_deref() {
            signal.source_hint(hint);
        }
        for input in &capability.inputs {
            signal.input(input);
        }
        if let Some(country) = language_country(&parsed.language) {
            signal.add(country, WEAK);
        }
        signal
    }

    fn add(&mut self, country: &str, weight: f64) {
        *self.weights.entry(country.to_owned()).or_default() += weight;
    }

    fn identifier(&mut self, key: &str, value: &str) {
        let key = key.to_lowercase();
        let value: String = value
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .flat_map(|character| character.to_uppercase())
            .collect();
        let has = |part: &str| key.split('_').any(|word| word == part) || key == part;
        if key.contains("kvk") || has("rsin") || key.contains("vestigingsnummer") {
            self.add("NL", IDENTIFIER);
        } else if has("siren") || has("siret") || has("rna") {
            self.add("FR", IDENTIFIER);
        } else if key.contains("companies_house")
            || key == "crn"
            // A plain eight digits may equally be a KVK number; only a
            // Scottish, Northern Irish, LLP... prefix is British by itself.
            || (has("company") && has("number") && companies_house_prefix(&value))
        {
            self.add("GB", IDENTIFIER);
        } else if has("vat") || has("btw") || has("tva") || has("ust") || has("iva") {
            if let Some(country) = value.get(..2).and_then(vat_country) {
                self.add(country, IDENTIFIER);
            }
        } else if has("iban") {
            // The bank's country, which is usually but not always the holder's.
            if let Some(country) = value
                .get(..2)
                .filter(|code| code.chars().all(|c| c.is_ascii_uppercase()))
            {
                self.add(country, INPUT);
            }
        } else if has("postcode") || has("postal") || key == "zip_code" {
            if let Some(country) = postcode_country(&value) {
                self.add(country, INPUT);
            }
        } else if has("kbo") || key.contains("ondernemingsnummer") {
            self.add("BE", IDENTIFIER);
        } else if has("cvr") {
            self.add("DK", IDENTIFIER);
        } else if has("hrb") || key.contains("handelsregisternummer") {
            self.add("DE", IDENTIFIER);
        } else if has("krs") || has("regon") || has("nip") {
            self.add("PL", IDENTIFIER);
        } else if has("kenteken") {
            self.add("NL", INPUT);
        }
    }

    fn label(&mut self, label: &str) {
        let words: Vec<String> = label
            .split(|c: char| c.is_whitespace() || c == ',' || c == '(' || c == ')')
            .map(|word| {
                word.chars()
                    .filter(|c| c.is_alphanumeric() || *c == '&' || *c == '/')
                    .flat_map(char::to_lowercase)
                    .collect::<String>()
            })
            .filter(|word| !word.is_empty())
            .collect();
        // Longest legal form first: "Pty Ltd" is Australian, "GmbH & Co. KG"
        // German, "sp. z o.o." Polish, before any shorter suffix matches.
        'forms: for take in (1..=4).rev() {
            if words.len() <= take {
                continue;
            }
            let tail = words[words.len() - take..].concat();
            for (form, countries) in LEGAL_FORMS {
                if tail == *form {
                    for (country, weight) in *countries {
                        self.add(country, *weight);
                    }
                    break 'forms;
                }
            }
        }
        if words
            .iter()
            .any(|word| matches!(word.as_str(), "stichting" | "vereniging" | "coöperatie"))
        {
            self.add("NL", LEGAL_FORM);
        }
        for word in &words {
            if let Some(country) = country_word(word) {
                self.add(country, WEAK);
            }
        }
    }

    fn source_hint(&mut self, hint: &str) {
        let hint = hint.to_lowercase();
        for (needle, country, weight) in [
            ("kvk", "NL", NAMED_SOURCE),
            ("kamer van koophandel", "NL", NAMED_SOURCE),
            ("handelsregister", "NL", WEAK),
            ("tenderned", "NL", NAMED_SOURCE),
            ("cbs", "NL", NAMED_SOURCE),
            ("rdw", "NL", NAMED_SOURCE),
            ("companies house", "GB", NAMED_SOURCE),
            ("companies_house", "GB", NAMED_SOURCE),
            ("companieshouse", "GB", NAMED_SOURCE),
            ("hmrc", "GB", NAMED_SOURCE),
            ("insee", "FR", NAMED_SOURCE),
            ("sirene", "FR", NAMED_SOURCE),
            ("infogreffe", "FR", NAMED_SOURCE),
            ("inpi", "FR", NAMED_SOURCE),
            ("bodacc", "FR", NAMED_SOURCE),
            // Sirenic is French-first but covers most European registers.
            ("sirenic", "FR", WEAK),
            ("kbo", "BE", NAMED_SOURCE),
            ("brreg", "NO", NAMED_SOURCE),
            ("bolagsverket", "SE", NAMED_SOURCE),
        ] {
            if hint.contains(needle) {
                self.add(country, weight);
                return;
            }
        }
    }

    fn input(&mut self, input: &str) {
        let has = |part: &str| input.split('_').any(|word| word == part);
        if has("kvk") || has("rsin") || input.contains("kvknummer") {
            self.add("NL", INPUT);
        } else if has("siren") || has("siret") {
            self.add("FR", INPUT);
        } else if input.contains("companies_house") {
            self.add("GB", INPUT);
        }
    }

    /// The subject's country, when one signal (or several agreeing ones)
    /// names it unambiguously.
    pub fn primary(&self) -> Option<&str> {
        let mut ranked: Vec<_> = self.weights.iter().collect();
        ranked.sort_by(|a, b| b.1.total_cmp(a.1));
        match ranked.as_slice() {
            [(country, weight), rest @ ..]
                if **weight >= STRONG && rest.first().is_none_or(|(_, next)| *next < *weight) =>
            {
                Some(country.as_str())
            }
            _ => None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.weights.is_empty()
    }

    pub fn adjustment(&self, coverage: &Coverage) -> Adjustment {
        if self.is_empty() || *coverage == Coverage::Unknown {
            return Adjustment::default();
        }
        if let Some(primary) = self.primary() {
            return match coverage {
                Coverage::Country(country) if country == primary => factor(MATCH),
                // A plausible second reading (Belgian N.V., Irish Limited).
                Coverage::Country(country) if self.weights.contains_key(country) => {
                    Adjustment::default()
                }
                Coverage::Country(_) => DEMOTED,
                Coverage::Europe if EUROPE.contains(&primary) => factor(EUROPE_MATCH),
                Coverage::Europe if NEAR_EUROPE.contains(&primary) => Adjustment::default(),
                Coverage::Europe => DEMOTED,
                Coverage::Unknown => Adjustment::default(),
            };
        }
        let top = self.weights.values().copied().fold(0.0, f64::max);
        factor(match coverage {
            Coverage::Country(country) => match self.weights.get(country) {
                Some(weight) if *weight >= top => 1.0 + NUDGE,
                Some(_) => 1.0,
                None => 1.0 - NUDGE,
            },
            Coverage::Europe => {
                if self
                    .weights
                    .iter()
                    .any(|(country, weight)| *weight >= top && EUROPE.contains(&country.as_str()))
                {
                    1.0 + NUDGE / 2.0
                } else {
                    1.0
                }
            }
            Coverage::Unknown => 1.0,
        })
    }
}

/// Legal-form suffixes, compared without dots or spaces. Forms used in one
/// country carry a strong weight; forms shared by several stay weak.
const LEGAL_FORMS: &[(&str, &[(&str, f64)])] = &[
    ("ptyltd", &[("AU", LEGAL_FORM)]),
    ("pvtltd", &[("IN", LEGAL_FORM)]),
    ("privatelimited", &[("IN", LEGAL_FORM)]),
    ("gmbh&cokg", &[("DE", LEGAL_FORM)]),
    ("spzoo", &[("PL", LEGAL_FORM)]),
    ("sàrl", &[("LU", SHARED_FORM), ("CH", SHARED_FORM)]),
    ("ua", &[("NL", LEGAL_FORM)]),
    ("bv", &[("NL", LEGAL_FORM), ("BE", SHARED_FORM)]),
    ("nv", &[("NL", LEGAL_FORM), ("BE", SHARED_FORM)]),
    ("vof", &[("NL", LEGAL_FORM)]),
    ("bvba", &[("BE", LEGAL_FORM)]),
    ("sprl", &[("BE", LEGAL_FORM)]),
    ("vzw", &[("BE", LEGAL_FORM)]),
    ("commv", &[("BE", LEGAL_FORM)]),
    ("ltd", &[("GB", LEGAL_FORM), ("IE", SHARED_FORM)]),
    ("limited", &[("GB", LEGAL_FORM), ("IE", SHARED_FORM)]),
    ("plc", &[("GB", LEGAL_FORM), ("IE", SHARED_FORM)]),
    ("llp", &[("GB", LEGAL_FORM), ("US", SHARED_FORM)]),
    ("cic", &[("GB", LEGAL_FORM)]),
    ("dac", &[("IE", LEGAL_FORM)]),
    ("inc", &[("US", LEGAL_FORM)]),
    ("llc", &[("US", LEGAL_FORM)]),
    ("corp", &[("US", LEGAL_FORM)]),
    ("corporation", &[("US", LEGAL_FORM)]),
    (
        "gmbh",
        &[("DE", LEGAL_FORM), ("AT", SHARED_FORM), ("CH", SHARED_FORM)],
    ),
    ("ggmbh", &[("DE", LEGAL_FORM)]),
    ("ug", &[("DE", LEGAL_FORM)]),
    ("kgaa", &[("DE", LEGAL_FORM)]),
    ("ohg", &[("DE", LEGAL_FORM)]),
    ("ev", &[("DE", LEGAL_FORM)]),
    ("kg", &[("DE", SHARED_FORM), ("AT", SHARED_FORM)]),
    (
        "ag",
        &[
            ("DE", SHARED_FORM),
            ("CH", SHARED_FORM),
            ("AT", SHARED_FORM),
        ],
    ),
    ("sas", &[("FR", LEGAL_FORM)]),
    ("sasu", &[("FR", LEGAL_FORM)]),
    ("sarl", &[("FR", LEGAL_FORM), ("LU", SHARED_FORM)]),
    ("eurl", &[("FR", LEGAL_FORM)]),
    ("sci", &[("FR", LEGAL_FORM)]),
    ("snc", &[("FR", LEGAL_FORM)]),
    (
        "sa",
        &[
            ("FR", SHARED_FORM),
            ("ES", SHARED_FORM),
            ("BE", SHARED_FORM),
            ("LU", SHARED_FORM),
            ("CH", SHARED_FORM),
        ],
    ),
    ("sl", &[("ES", LEGAL_FORM)]),
    ("slu", &[("ES", LEGAL_FORM)]),
    ("spa", &[("IT", LEGAL_FORM)]),
    (
        "srl",
        &[
            ("IT", SHARED_FORM),
            ("RO", SHARED_FORM),
            ("BE", SHARED_FORM),
        ],
    ),
    ("aps", &[("DK", LEGAL_FORM)]),
    ("a/s", &[("DK", SHARED_FORM), ("NO", SHARED_FORM)]),
    ("ab", &[("SE", LEGAL_FORM)]),
    ("oy", &[("FI", LEGAL_FORM)]),
    ("oyj", &[("FI", LEGAL_FORM)]),
    ("asa", &[("NO", LEGAL_FORM)]),
    ("oü", &[("EE", LEGAL_FORM)]),
    ("sia", &[("LV", LEGAL_FORM)]),
    ("uab", &[("LT", LEGAL_FORM)]),
    ("sro", &[("CZ", SHARED_FORM), ("SK", SHARED_FORM)]),
    ("kft", &[("HU", LEGAL_FORM)]),
    ("zrt", &[("HU", LEGAL_FORM)]),
    ("nyrt", &[("HU", LEGAL_FORM)]),
];

fn country_word(word: &str) -> Option<&'static str> {
    Some(match word {
        "nederland" | "netherlands" | "holland" | "dutch" => "NL",
        "uk" | "britain" | "british" | "england" | "scotland" | "wales" => "GB",
        "france" | "français" | "française" | "francaise" => "FR",
        "deutschland" | "germany" => "DE",
        "belgië" | "belgie" | "belgique" | "belgium" => "BE",
        _ => return None,
    })
}

fn language_country(language: &str) -> Option<&'static str> {
    Some(match language {
        "nl" => "NL",
        "fr" => "FR",
        "de" => "DE",
        "it" => "IT",
        "es" => "ES",
        "pl" => "PL",
        "da" => "DK",
        "sv" => "SE",
        "fi" => "FI",
        "et" => "EE",
        "lv" => "LV",
        "lt" => "LT",
        "cs" => "CZ",
        "sk" => "SK",
        "hu" => "HU",
        _ => return None,
    })
}

fn vat_country(prefix: &str) -> Option<&'static str> {
    let code = match prefix {
        "EL" => "GR",
        "XI" | "GB" => "GB",
        "CH" => "CH",
        _ => EUROPE.iter().find(|code| **code == prefix)?,
    };
    Some(code)
}

fn companies_house_prefix(value: &str) -> bool {
    let (prefix, digits) = value.split_at(value.len().min(2));
    [
        "SC", "NI", "OC", "SO", "NC", "LP", "SL", "FC", "NF", "GE", "IP", "SP", "RC", "SR", "NP",
    ]
    .contains(&prefix)
        && digits.len() == 6
        && digits.chars().all(|c| c.is_ascii_digit())
}

fn postcode_country(value: &str) -> Option<&'static str> {
    let bytes = value.as_bytes();
    if bytes.len() == 6
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4..].iter().all(u8::is_ascii_uppercase)
    {
        return Some("NL");
    }
    // Outward code (A9, A99, AA9, AA99, A9A, AA9A) plus inward code (9AA).
    let uk = (5..=7).contains(&bytes.len())
        && bytes[0].is_ascii_uppercase()
        && bytes[bytes.len() - 3].is_ascii_digit()
        && bytes[bytes.len() - 2..].iter().all(u8::is_ascii_uppercase);
    uk.then_some("GB")
}

/// A two-letter country segment used in contract ids and fact types.
fn code(segment: &str) -> Option<Coverage> {
    match segment {
        "eu" | "ue" => Some(Coverage::Europe),
        "uk" | "gb" => Some(Coverage::Country("GB".into())),
        "nl" | "fr" | "de" | "be" | "lu" | "at" | "ch" | "it" | "es" | "pt" | "ie" | "dk"
        | "se" | "no" | "fi" | "is" | "ee" | "lv" | "lt" | "pl" | "cz" | "sk" | "hu" | "si"
        | "hr" | "ro" | "bg" | "gr" | "cy" | "mt" | "us" => {
            Some(Coverage::Country(segment.to_uppercase()))
        }
        _ => None,
    }
}

/// One coverage from several clues: a single country wins over a European
/// selector (a Sirenic `eu` route for GB company numbers is a GB lookup).
fn combine(found: impl IntoIterator<Item = Coverage>) -> Option<Coverage> {
    let mut countries = BTreeSet::new();
    let mut europe = false;
    for coverage in found {
        match coverage {
            Coverage::Country(country) => {
                countries.insert(country);
            }
            Coverage::Europe => europe = true,
            Coverage::Unknown => {}
        }
    }
    match countries.len() {
        1 => countries.pop_first().map(Coverage::Country),
        0 if europe => Some(Coverage::Europe),
        0 => None,
        _ => Some(Coverage::Europe),
    }
}

/// Country identifiers in an input fact type: `company_registry.kvknummer`,
/// `company.registration.gb.companies_house`, `company.registration.eu.national_id`.
fn fact_coverage(fact: &str) -> Option<Coverage> {
    let fact = fact.to_lowercase();
    let segments: Vec<&str> = fact.split('.').collect();
    if let Some(position) = segments
        .iter()
        .position(|segment| *segment == "registration")
    {
        if let Some(found) = segments.get(position + 1).and_then(|segment| code(segment)) {
            return Some(found);
        }
    }
    if fact.contains("kvk") || segments.iter().any(|s| *s == "rsin" || *s == "nl") {
        return Some(Coverage::Country("NL".into()));
    }
    if fact.contains("companies_house") {
        return Some(Coverage::Country("GB".into()));
    }
    let leaf = segments.last().copied().unwrap_or_default();
    if matches!(leaf, "siren" | "siret") {
        return Some(Coverage::Country("FR".into()));
    }
    // `vat.uk.candidate_number`, `vat.eu.number`: a country segment right
    // after the concept, never an arbitrary two-letter word further along.
    segments.get(1).and_then(|segment| code(segment))
}

/// Source identities that are themselves national or European registers.
fn source_coverage(slug: &str) -> Option<Coverage> {
    let words: Vec<&str> = slug.split(['-', '_']).collect();
    let has = |word: &str| words.contains(&word);
    if has("kvk")
        || has("cbs")
        || has("pdok")
        || has("rdw")
        || has("tenderned")
        || has("overheid")
        || has("koop")
        || has("rvo")
        || has("bodemloket")
        || has("nl")
        || has("dutch")
    {
        Some(Coverage::Country("NL".into()))
    } else if (has("companies") && has("house")) || has("hmrc") {
        Some(Coverage::Country("GB".into()))
    } else if has("insee") || has("sirene") || has("infogreffe") || has("inpi") {
        Some(Coverage::Country("FR".into()))
    } else if has("vies") || has("ted") || has("eurostat") || has("eurlex") || has("eu") {
        Some(Coverage::Europe)
    } else {
        None
    }
}

/// Provider tags that name exactly one country ("KVK", "UK", "Dutch law").
fn tag_coverage(tags: &[String]) -> Option<Coverage> {
    let mut found = BTreeSet::new();
    for tag in tags {
        for word in tag.to_lowercase().split(|c: char| !c.is_alphanumeric()) {
            let country = match word {
                "kvk" | "dutch" | "netherlands" | "nederland" => "NL",
                "uk" | "gb" | "hmrc" => "GB",
                "france" | "french" | "sirene" | "insee" => "FR",
                _ => continue,
            };
            found.insert(country);
        }
    }
    (found.len() == 1).then(|| Coverage::Country(found.pop_first().unwrap_or_default().into()))
}

/// Where one executable endpoint can answer. Its identifier inputs are the
/// hardest evidence; a Sirenic fact type minted for the provider inherits the
/// provider's home jurisdiction, so the catalog's jurisdiction is used only
/// for facts that are not namespaced by the source itself.
pub(super) fn endpoint(service: &Service, source: Option<&KnownSource>) -> Coverage {
    let facts: Vec<&str> = service
        .contract
        .requires
        .iter()
        .map(|requirement| requirement.fact_type.as_str())
        .chain(service.contract.input_map.keys().map(String::as_str))
        .collect();
    if let Some(found) = combine(facts.iter().filter_map(|fact| fact_coverage(fact))) {
        return found;
    }
    let slug = service.contract.capability_slug.to_lowercase();
    if !slug.starts_with("operation.") {
        let words = slug.split(['.', '-', '_']).skip(1);
        if let Some(found) = combine(words.filter_map(code)) {
            return found;
        }
    }
    if let Some(found) = source_coverage(&service.api_slug)
        .or_else(|| source.and_then(|source| tag_coverage(&source.metadata.tags)))
    {
        return found;
    }
    let provider = service
        .api_slug
        .split(['-', '_'])
        .next()
        .unwrap_or_default();
    let namespaced = facts
        .iter()
        .copied()
        .chain(service.contract.identifies.as_deref())
        .any(|fact| {
            fact.split('.')
                .any(|segment| segment == service.api_slug || segment == provider)
        });
    match service.jurisdiction.as_deref().map(str::to_lowercase) {
        Some(jurisdiction) if !namespaced => code(&jurisdiction).unwrap_or(Coverage::Unknown),
        _ => Coverage::Unknown,
    }
}

/// Where a discovery-only source can answer: its identity and tags.
pub(super) fn source(source: &KnownSource) -> Coverage {
    source_coverage(&source.slug)
        .or_else(|| tag_coverage(&source.metadata.tags))
        .unwrap_or(Coverage::Unknown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn signal(
        label: &str,
        identifiers: serde_json::Value,
        hint: Option<&str>,
        language: &str,
    ) -> Signal {
        let request = format!(
            "{label} {} {}",
            identifiers
                .as_object()
                .map(|object| object
                    .values()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(" "))
                .unwrap_or_default(),
            hint.unwrap_or("")
        );
        let parsed = crate::capability_intake::normalize(
            &json!({"language":language,"subjects":[{"id":"s1","label":label,"type":"company","identifiers":identifiers}],
                "capabilities":[{"slug":"company.profile","name":"Company profile","description":"Retrieve a company profile.",
                    "domain":"company","inputs":["company_name"],"outputs":["legal_name"],"subject_id":"s1","source_hint":hint,"confidence":0.9}],
                "deliverable":{"format":"chat","operations":[]}})
            .to_string(),
            &request,
            "fixture",
        )
        .unwrap();
        Signal::infer(&parsed, &parsed.capabilities[0])
    }

    #[test]
    fn legal_forms_name_the_country_only_when_they_are_unambiguous() {
        for (label, expected) in [
            ("Coolblue B.V.", Some("NL")),
            ("Coolblue BV", Some("NL")),
            ("ASML Holding N.V.", Some("NL")),
            ("Bakkerij Jansen V.O.F.", Some("NL")),
            ("Stichting Wikimedia", Some("NL")),
            ("Acme Ltd", Some("GB")),
            ("Tesco PLC", Some("GB")),
            ("Monzo Bank Limited", Some("GB")),
            ("Example SAS", Some("FR")),
            ("Exemple SARL", Some("FR")),
            ("Muster GmbH", Some("DE")),
            ("Muster GmbH & Co. KG", Some("DE")),
            ("Apple Inc.", Some("US")),
            ("Acme Pty Ltd", Some("AU")),
            ("Spotify AB", Some("SE")),
            ("Firma sp. z o.o.", Some("PL")),
            // Shared by Germany, Switzerland and Austria, or France, Spain,
            // Belgium, Luxembourg and Switzerland: too weak alone.
            ("Siemens AG", None),
            ("Example SA", None),
            ("Coolblue", None),
            ("Holland & Barrett Retail Limited", Some("GB")),
        ] {
            assert_eq!(
                signal(label, json!({}), None, "en").primary(),
                expected,
                "{label}"
            );
        }
    }

    #[test]
    fn identifiers_and_named_sources_outweigh_a_legal_form() {
        assert_eq!(
            signal("Coolblue", json!({"kvk_number":"24285251"}), None, "en").primary(),
            Some("NL")
        );
        assert_eq!(
            signal("Airbus", json!({"siren":"383474814"}), None, "en").primary(),
            Some("FR")
        );
        assert_eq!(
            signal(
                "Coolblue",
                json!({"vat_number":"NL810433941B01"}),
                None,
                "en"
            )
            .primary(),
            Some("NL")
        );
        assert_eq!(
            signal(
                "Tesco",
                json!({"iban":"GB33BUKB20201555555555"}),
                None,
                "en"
            )
            .primary(),
            Some("GB")
        );
        assert_eq!(
            signal("Acme", json!({"company_number":"SC123456"}), None, "en").primary(),
            Some("GB")
        );
        // A plain eight-digit number is a KVK or a Companies House number.
        assert_eq!(
            signal("Acme", json!({"company_number":"12345678"}), None, "en").primary(),
            None
        );
        assert_eq!(
            signal("Coolblue", json!({}), Some("kvk"), "en").primary(),
            Some("NL")
        );
        assert_eq!(
            signal("Tesco", json!({}), Some("companies house"), "en").primary(),
            Some("GB")
        );
        // An explicit identifier beats the label's legal form.
        assert_eq!(
            signal("Example Ltd", json!({"siren":"383474814"}), None, "en").primary(),
            Some("FR")
        );
    }

    #[test]
    fn language_is_a_weak_tie_breaker_that_never_names_a_country_alone() {
        let dutch = signal("Coolblue", json!({}), None, "nl");
        assert_eq!(dutch.primary(), None);
        let nl = Coverage::Country("NL".into());
        let gb = Coverage::Country("GB".into());
        assert_eq!(dutch.adjustment(&nl), factor(1.0 + NUDGE));
        assert_eq!(dutch.adjustment(&gb), factor(1.0 - NUDGE));
        // With a shared legal form it settles the reading: a German AG.
        assert_eq!(
            signal("Siemens AG", json!({}), None, "de").primary(),
            Some("DE")
        );
        assert!(signal("Coolblue", json!({}), None, "en").is_empty());
    }

    #[test]
    fn candidates_for_the_subjects_country_rise_and_others_are_demoted_not_removed() {
        let dutch = signal("Coolblue B.V.", json!({}), None, "en");
        assert_eq!(
            dutch.adjustment(&Coverage::Country("NL".into())),
            factor(MATCH)
        );
        assert_eq!(dutch.adjustment(&Coverage::Europe), factor(EUROPE_MATCH));
        assert_eq!(dutch.adjustment(&Coverage::Unknown), Adjustment::default());
        assert_eq!(dutch.adjustment(&Coverage::Country("FR".into())), DEMOTED);
        // A Belgian reading of B.V. is plausible: neither boosted nor demoted.
        assert_eq!(
            dutch.adjustment(&Coverage::Country("BE".into())),
            Adjustment::default()
        );
        let british = signal("Acme Ltd", json!({}), None, "en");
        assert_eq!(british.adjustment(&Coverage::Europe), Adjustment::default());
        let american = signal("Apple Inc.", json!({}), None, "en");
        assert!(american.adjustment(&Coverage::Europe).demoted);
    }

    fn service(
        capability: &str,
        facts: &[&str],
        api_slug: &str,
        jurisdiction: Option<&str>,
    ) -> Service {
        let snapshot: crate::catalog::Snapshot = serde_json::from_str(include_str!(
            "../../../tests/fixtures/integration-catalog.json"
        ))
        .unwrap();
        let mut service = snapshot.services[0].clone();
        service.api_slug = api_slug.into();
        service.contract.capability_slug = capability.into();
        service.jurisdiction = jurisdiction.map(str::to_owned);
        service.contract.identifies = None;
        service.contract.requires = facts
            .iter()
            .map(|fact| serde_json::from_value(json!({"fact_type":fact})).unwrap())
            .collect();
        service.contract.input_map = facts
            .iter()
            .map(|fact| {
                (
                    (*fact).to_owned(),
                    serde_json::from_value(json!({"in":"query","name":"id"})).unwrap(),
                )
            })
            .collect();
        service
    }

    #[test]
    fn candidate_coverage_comes_from_identifier_inputs_contract_ids_and_source_identity() {
        let country = |code: &str| Coverage::Country(code.into());
        for (service, expected) in [
            (
                service(
                    "company.registry-profile",
                    &["company_registry.kvknummer"],
                    "kvk-dutch-business-register",
                    Some("NL"),
                ),
                country("NL"),
            ),
            (
                service(
                    "company.registry-profile",
                    &["company.registration.fr.siren"],
                    "sirenic",
                    Some("FR"),
                ),
                country("FR"),
            ),
            (
                service(
                    "company.registry-profile",
                    &["company.registration.gb.companies_house"],
                    "companies-house",
                    Some("GB"),
                ),
                country("GB"),
            ),
            (
                service(
                    "company.registry-profile",
                    &[
                        "company.country_code",
                        "company.registration.eu.national_id",
                    ],
                    "sirenic",
                    None,
                ),
                Coverage::Europe,
            ),
            (
                service(
                    "company.sirenic-eu-entreprise-gb-company-number-dirigeants",
                    &["company.registration.gb.companies_house"],
                    "sirenic",
                    Some("GB"),
                ),
                country("GB"),
            ),
            (
                service(
                    "company.sirenic-eu-agrements",
                    &["company.name", "company.country_code"],
                    "sirenic",
                    Some("FR"),
                ),
                Coverage::Europe,
            ),
            (
                service(
                    "company.fr-registry-search",
                    &["company.name"],
                    "sirenic",
                    Some("FR"),
                ),
                country("FR"),
            ),
            (
                service(
                    "vat.uk.validate",
                    &["vat.uk.candidate_number"],
                    "ask-merlin",
                    Some("GB"),
                ),
                country("GB"),
            ),
            (
                service(
                    "vat.eu.validate",
                    &["vat.eu.country_code", "vat.eu.number"],
                    "vies",
                    Some("EU"),
                ),
                Coverage::Europe,
            ),
            (
                service(
                    "procurement.ted-kvk-notices",
                    &["company_registry.kvknummer"],
                    "ted",
                    Some("NL"),
                ),
                country("NL"),
            ),
            (
                service(
                    "statistics.cbs-bankruptcies",
                    &["cbs.input.odata_filter"],
                    "cbs-statline",
                    None,
                ),
                country("NL"),
            ),
            (
                service(
                    "operation.companies-house.get-search-93864601",
                    &["source_input.e9.q"],
                    "companies-house",
                    None,
                ),
                country("GB"),
            ),
            // Sirenic mints its own fact types with France as jurisdiction,
            // although these routes accept any EU VAT number or IBAN.
            (
                service(
                    "company.sirenic-tva-verifier-numero",
                    &["company.sirenic.input.tva_verifier_numero.numero"],
                    "sirenic",
                    Some("FR"),
                ),
                Coverage::Unknown,
            ),
            (
                service(
                    "company.sirenic-iban-verifier-iban",
                    &["company.sirenic.input.iban_verifier_iban.iban"],
                    "sirenic",
                    Some("FR"),
                ),
                Coverage::Unknown,
            ),
            (
                service(
                    "company.sanctions-screen",
                    &["company.name", "company.search.country_code"],
                    "opensanctions",
                    None,
                ),
                Coverage::Unknown,
            ),
            (
                service(
                    "company.lei-verification",
                    &["company.registration.lei"],
                    "gleif",
                    None,
                ),
                Coverage::Unknown,
            ),
            (
                service(
                    "operation.gridpulse.get-api-energy-appliance-646712a2",
                    &["source_input.e6.state"],
                    "gridpulse",
                    None,
                ),
                Coverage::Unknown,
            ),
            // A reviewed source with a fact-type jurisdiction of its own.
            (
                service(
                    "company.registry-profile",
                    &["company.registration.lei"],
                    "belgian-register",
                    Some("BE"),
                ),
                country("BE"),
            ),
        ] {
            assert_eq!(
                endpoint(&service, None),
                expected,
                "{} {}",
                service.api_slug,
                service.contract.capability_slug
            );
        }
    }
}
