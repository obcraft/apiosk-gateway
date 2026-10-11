//! Capability-only directory search. This index contains catalog metadata, never
//! questions or subjects. Matches describe discovery; they do not create plans.
use crate::{
    capability_intake::{
        capability_query, normalize_slug, Candidate, Capability, Match, ParsedRequest,
    },
    catalog::{Catalog, KnownSource, ListedEndpoint, Service},
    state::Task,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, OnceLock},
};

#[cfg(test)]
mod calibration;
mod country;
pub(crate) use country::named_countries;
mod relevance;

const FINANCIAL: u16 = 1;
const OFFICERS: u16 = 2;
const CONTROL: u16 = 4;
const SANCTIONS: u16 = 8;
const BUILDING: u16 = 16;
const NEIGHBOURHOOD: u16 = 32;
const BOUNDARY: u16 = 64;
const VIOLATIONS: u16 = 128;
const PERMITS: u16 = 256;
const STOCK_MARKET: u16 = 512;
const ANALYST_CONSENSUS: u16 = 1024;
const INSIDER_TRADES: u16 = 2048;
const VALUATION: u16 = 4096;
const LEGISLATION: u16 = 8192;
const ADVERSE_MEDIA: u16 = 16384;
const SENTIMENT: u16 = 32768;
const SPECIFIC_OPERATIONS: u16 = STOCK_MARKET | ANALYST_CONSENSUS | INSIDER_TRADES | VALUATION | ADVERSE_MEDIA;
const LOCATION_FACETS: u16 = BUILDING | NEIGHBOURHOOD | BOUNDARY | VIOLATIONS | PERMITS;
const FACETS: &[(u16, &str)] = &[
    (FINANCIAL, "@financial"), (OFFICERS, "@officers"),
    (CONTROL, "@control"), (SANCTIONS, "@sanctions"),
    (BUILDING, "@building"), (NEIGHBOURHOOD, "@neighbourhood"),
    (BOUNDARY, "@boundary"), (VIOLATIONS, "@violations"),
    (PERMITS, "@permits"), (STOCK_MARKET, "@stock-market"),
    (ANALYST_CONSENSUS, "@analyst-consensus"), (INSIDER_TRADES, "@insider-trades"),
    (VALUATION, "@valuation"), (LEGISLATION, "@legislation"), (ADVERSE_MEDIA, "@adverse-media"),
    (SENTIMENT, "@sentiment"),
];

#[derive(Clone, Copy)]
enum Target {
    Endpoint(usize),
    Listing(usize, usize),
    Source(usize),
}
impl Target {
    fn is_endpoint(self) -> bool { matches!(self, Self::Endpoint(_) | Self::Listing(_, _)) }
}

struct Document {
    target: Target,
    tokens: BTreeSet<String>,
    facets: u16,
    capabilities: BTreeSet<String>,
    /// Meaningful words of the endpoint's reviewed capability id (see
    /// `relevance::contract_words`); empty for sources.
    contract: BTreeSet<String>,
    /// Facets named by the capability id itself (`company.accounts`,
    /// `company.officers`), not merely by a description.
    contract_facets: u16,
    /// Words of the reviewed capability id's namespace (`company` and
    /// `registry` for `company_registry.lookup`); empty for sources and
    /// generated `operation.*` ids.
    namespace: BTreeSet<String>,
    /// The endpoint's own description, excluding provider branding and a
    /// semicolon-separated provider-wide appendix.
    topic_scope: BTreeSet<String>,
    /// Whether the operation's own wording describes an individual rather
    /// than an organisation. Used for person-profile requests only.
    person_scope: bool,
    /// Asset kind stated by the endpoint itself, never provider branding or
    /// caller input names. Keep the full description: a later sentence can
    /// explain that a stock mark is actually a derivative.
    derivative_asset: bool,
    /// Explicit enterprise-ratio notation in the endpoint's own scope.
    enterprise_multiple: bool,
    /// Full endpoint wording for measurement compatibility. Provider names,
    /// input names and unrelated provider metadata cannot supply a metric.
    operation_words: BTreeSet<String>,
    /// DNS types in the operation itself. A/NS/MX are too short for the
    /// general lexical index, but they name different paid capabilities.
    dns_record_types: u16,
    /// Where the candidate can answer, for the country signal.
    coverage: country::Coverage,
}
struct Index {
    documents: Vec<Document>,
    frequency: BTreeMap<String, usize>,
}

/// A catalog version and the state of its English renderings.
type IndexKey = (String, u64, u64);

/// Operation titles often precede examples or provider-wide copy. A period
/// inside ClinicalTrials.gov/Fly.io or a short label such as "FREE —" is not
/// the end of that title.
fn opening_description(text: &str) -> &str {
    // Additional analogues are not the endpoint's primary operation. A
    // US veterans' mortgage guide mentioning subsidies elsewhere must not
    // become a general business subsidy search.
    let text = text.split_once(", plus ").filter(|(_, appendix)|
        appendix.starts_with("the closest analogues") || appendix.starts_with("analogues elsewhere"))
        .map_or(text, |(operation, _)| operation);
    let mut question_intro = false;
    for (index,character) in text.char_indices() {
        let prefix=&text[..index];
        let after=&text[index+character.len_utf8()..];
        let boundary=match character {
            ';' | '!' => true,
            // A rhetorical opening question often names only a pronoun:
            // "Is it real or fake? Collectible authentication ...". Include
            // its next declarative sentence as the operation's own scope.
            '?' if !question_intro => {question_intro = true; false},
            '?' => true,
            '—' => prefix.split_whitespace().count()>1 && !prefix.trim_end().ends_with("API"),
            '.' => {
                let token=prefix.split_whitespace().last().unwrap_or_default();
                let abbreviation=token.contains('.') && token.chars().filter(|c|c.is_alphabetic()).count()<=3
                    && token.chars().filter(|c|c.is_alphabetic()).all(|c|c.is_uppercase());
                after.chars().next().is_some_and(char::is_whitespace) && !abbreviation
            }
            _ => false,
        };
        if boundary {return prefix;}
    }
    text
}

// A single catalog version is retained. Concurrent readers may finish with the
// previous Arc, but new versions replace the cache rather than accumulating.
// New English renderings (see `crate::english`) rebuild it too.
fn index(catalog: &Catalog) -> Arc<Index> {
    static LAST: OnceLock<Mutex<Option<(IndexKey, Arc<Index>)>>> = OnceLock::new();
    let cache = LAST.get_or_init(|| Mutex::new(None));
    let english = &catalog.english;
    let version = (catalog.version.clone(), english.id(), english.generation());
    if let Some((_, value)) = cache
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .filter(|(indexed, _)| indexed == &version)
    {
        return value.clone();
    }
    let mut documents = Vec::with_capacity(catalog.services.len() + catalog.known_sources.len());
    // Each document's words that only its English rendering has.
    let mut rendered: Vec<BTreeSet<String>> = Vec::with_capacity(documents.capacity());
    for (position, service) in catalog.services.iter().enumerate() {
        let known = catalog
            .known_sources
            .iter()
            .find(|source| source.slug == service.api_slug);
        let described = known
            .and_then(|source| {
                source
                    .metadata
                    .capability_descriptions
                    .get(&service.contract.capability_slug)
            })
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        // Do not attach a provider's broad description to each endpoint: that
        // would make its officers endpoint falsely match beneficial ownership.
        let mut text = format!(
            "{} {} {} {} {} {}",
            service.contract.capability_slug,
            service.name,
            service.description.as_deref().unwrap_or(""),
            described,
            service
                .contract
                .input_map
                .values()
                .map(|binding| binding.name.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            service
                .contract
                .output_map
                .produces
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        );
        // The English rendering of each text, next to the original wording.
        let rendering = english.render(&[
            &service.name,
            service.description.as_deref().unwrap_or(""),
            described,
        ]);
        // Input names describe what a caller supplies, not what the API
        // returns. "financial assistance" plus an input named
        // "first-statement-date" must not become financial statements.
        let offered_facets = document(Target::Endpoint(position), &format!("{} {} {} {} {} {}",
            service.contract.capability_slug, service.name,
            service.description.as_deref().unwrap_or(""), described,
            service.contract.output_map.produces.keys().map(String::as_str).collect::<Vec<_>>().join(" "),
            rendering), BTreeSet::new()).facets;
        let original = (!rendering.is_empty()).then(|| text.clone());
        append(&mut text, rendering);
        let capabilities =
            normalized_capabilities(std::iter::once(service.contract.capability_slug.as_str()));
        let primary = document(
            Target::Endpoint(position),
            &service.contract.capability_slug,
            capabilities.clone(),
        )
        .facets;
        let mut indexed = document(Target::Endpoint(position), &text, capabilities);
        let description=service.description.as_deref().unwrap_or(described);
        let opening = opening_description(description);
        let translated_description=english.render(&[description]);
        indexed.derivative_asset = relevance::derivative_market(description)
            || relevance::derivative_market(&translated_description);
        indexed.enterprise_multiple = relevance::enterprise_ratio(description)
            || relevance::enterprise_ratio(&translated_description)
            || relevance::enterprise_ratio(&service.contract.capability_slug);
        indexed.operation_words = words(&format!("{description} {translated_description}"));
        let translated_opening=opening_description(&translated_description);
        indexed.dns_record_types = if service.contract.capability_slug.starts_with("domain.dns-")
            || service.contract.capability_slug == "domain.mail-dns" {
            relevance::dns_record_types(&service.contract.capability_slug)
        } else {
            relevance::dns_record_types(&service.contract.capability_slug)
                | relevance::dns_record_types(opening) | relevance::dns_record_types(translated_opening)
        };
        indexed.topic_scope = document(Target::Endpoint(position), &format!("{opening} {translated_opening}"), BTreeSet::new()).tokens;
        indexed.person_scope = relevance::person_scope(&format!("{opening} {translated_opening}"));
        indexed.coverage = country::endpoint(service, known);
        indexed.contract = relevance::contract_words(&service.contract.capability_slug);
        indexed.contract_facets = primary;
        indexed.namespace = match service.contract.capability_slug.split_once('.') {
            Some(("operation", _)) | None => BTreeSet::new(),
            Some((namespace, _)) => words(namespace),
        };
        {
            // The operation's own concept takes precedence over a description
            // explaining what a caller might do afterwards. A director roster
            // "for sanctions screening" is not itself sanctions screening.
            indexed.facets = if primary != 0 { primary } else { offered_facets };
            for &(facet, marker) in FACETS {
                if indexed.facets & facet == 0 {
                    indexed.tokens.remove(marker);
                }
            }
        }
        rendered.push(rendered_only(&indexed, original.as_deref()));
        documents.push(indexed);
    }
    let accepted: BTreeSet<_> = catalog.services.iter().map(|service| service.endpoint_id).collect();
    for (position, source) in catalog.known_sources.iter().enumerate() {
        for (listing, endpoint) in source.metadata.endpoints.iter().enumerate() {
            if accepted.contains(&endpoint.endpoint_id) { continue; }
            let target = Target::Listing(position, listing);
            // Index the operation, not its provider's marketing or the names
            // of inputs. The contractless endpoint remains discovery-only.
            let description = &endpoint.description;
            let rendering = english.render(&[description]);
            let mut indexed = document(target, &format!("{description} {rendering}"), BTreeSet::new());
            indexed.topic_scope = document(target, &format!("{} {}",
                opening_description(description), opening_description(&rendering)), BTreeSet::new()).tokens;
            indexed.person_scope = relevance::person_scope(&format!("{} {}",
                opening_description(description), opening_description(&rendering)));
            indexed.operation_words = words(&format!("{description} {rendering}"));
            indexed.dns_record_types = relevance::dns_record_types(&format!("{} {}", opening_description(description), opening_description(&rendering)));
            indexed.derivative_asset = relevance::derivative_market(description) || relevance::derivative_market(&rendering);
            indexed.enterprise_multiple = relevance::enterprise_ratio(description) || relevance::enterprise_ratio(&rendering);
            indexed.coverage = country::source(source);
            rendered.push(rendered_only(&indexed, Some(description)));
            documents.push(indexed);
        }
        let descriptions = &source.metadata.capability_descriptions;
        let mut text = format!(
            "{} {} {} {} {}",
            source.name,
            source.metadata.description,
            source.metadata.category,
            source.metadata.tags.join(" "),
            descriptions
        );
        let tags = crate::english::tags_text(&source.metadata.tags);
        let mut texts = vec![
            source.name.as_str(),
            source.metadata.description.as_str(),
            source.metadata.category.as_str(),
            tags.as_str(),
        ];
        texts.extend(
            descriptions
                .as_object()
                .into_iter()
                .flat_map(|object| object.values().filter_map(serde_json::Value::as_str)),
        );
        let rendering = english.render(&texts);
        let original = (!rendering.is_empty()).then(|| text.clone());
        append(&mut text, rendering);
        let capabilities = normalized_capabilities(
            descriptions
                .as_object()
                .into_iter()
                .flat_map(|object| object.keys().map(String::as_str)),
        );
        let mut indexed = document(Target::Source(position), &text, capabilities);
        indexed.person_scope = relevance::person_scope(opening_description(&source.metadata.description));
        indexed.coverage = country::source(source);
        rendered.push(rendered_only(&indexed, original.as_deref()));
        documents.push(indexed);
    }
    // How common a word is, as the original texts use it: English renderings
    // may only add a candidate's evidence, never make a word the original
    // texts share more common (a lower idf, half weight at the relevance
    // floor) and so lose a candidate the original wording found. A word only
    // renderings use counts the renderings.
    let mut frequency: BTreeMap<String, usize> = BTreeMap::new();
    let mut translated: BTreeMap<String, usize> = BTreeMap::new();
    for (document, rendered) in documents.iter().zip(&rendered) {
        for token in &document.tokens {
            let counts = if rendered.contains(token) { &mut translated } else { &mut frequency };
            *counts.entry(token.clone()).or_default() += 1;
        }
    }
    for (token, count) in translated {
        frequency.entry(token).or_insert(count);
    }
    let value = Arc::new(Index {
        documents,
        frequency,
    });
    *cache.lock().unwrap_or_else(|p| p.into_inner()) =
        Some((version, value.clone()));
    value
}

fn normalized_capabilities<'a>(slugs: impl Iterator<Item = &'a str>) -> BTreeSet<String> {
    slugs
        .filter_map(|slug| normalize_slug(slug, "").map(|(slug, _)| slug.to_lowercase()))
        .collect()
}

/// The words `indexed` has only thanks to the English rendering appended to
/// `original`; none without a rendering.
fn rendered_only(indexed: &Document, original: Option<&str>) -> BTreeSet<String> {
    let Some(original) = original else {
        return BTreeSet::new();
    };
    let original = document(indexed.target, original, BTreeSet::new());
    indexed.tokens.difference(&original.tokens).cloned().collect()
}

fn append(text: &mut String, english: String) {
    if !english.is_empty() {
        text.push(' ');
        text.push_str(&english);
    }
}

/// The words of `text` as search compares them: camelCase split, lowercase
/// without accents (`Économie` is `economie`, `Straße` is `strasse`) and in
/// the singular. Latin words need three letters, words of other scripts two;
/// Han, kana and Hangul, written without spaces, become overlapping pairs of
/// characters. No script is dropped.
pub(crate) fn stems(text: &str) -> BTreeSet<String> {
    tokens(text).iter().filter_map(|word| singular(word)).collect()
}

/// `text` split into folded words, before the singular.
fn tokens(text: &str) -> Vec<String> {
    let chars: Vec<_> = text.chars().collect();
    let mut separated = String::with_capacity(text.len());
    for (index, &character) in chars.iter().enumerate() {
        if character.is_uppercase()
            && index > 0
            && (chars[index - 1].is_lowercase()
                || (chars[index - 1].is_uppercase()
                    && chars.get(index + 1).is_some_and(|next| next.is_lowercase())))
        {
            separated.push(' ');
        }
        crate::english::push_folded(character, &mut separated);
    }
    let mut tokens = Vec::new();
    for word in separated
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
    {
        if !word.chars().any(crate::english::unspaced) {
            tokens.push(word.to_owned());
            continue;
        }
        let characters: Vec<char> = word.chars().collect();
        let mut start = 0;
        while start < characters.len() {
            let unspaced = crate::english::unspaced(characters[start]);
            let end = (start..characters.len())
                .find(|&position| crate::english::unspaced(characters[position]) != unspaced)
                .unwrap_or(characters.len());
            let run = &characters[start..end];
            if !unspaced || run.len() == 1 {
                tokens.push(run.iter().collect());
            } else {
                tokens.extend(run.windows(2).map(|pair| pair.iter().collect::<String>()));
            }
            start = end;
        }
    }
    tokens
}

fn singular(word: &str) -> Option<String> {
    let length = word.chars().count();
    let shortest = if word.is_ascii() {
        3
    } else if word.chars().all(crate::english::unspaced) {
        1
    } else {
        2
    };
    if length > 64 || length < shortest {
        return None;
    }
    Some(match word {
        "companies" => "company".into(),
        "addresses" => "address".into(),
        "neighborhood" | "neighborhoods" | "neighbourhoods" => "neighbourhood".into(),
        "jaarrekeningen" => "jaarrekening".into(),
        "sanctioned" => "sanction".into(),
        "regulatory" | "regulation" | "regulations" | "legislation" | "wetgeving" | "law" | "laws" => "legislation".into(),
        // Not the plural of "new": Reuters headlines are not "new rules".
        "news" => "news".into(),
        // bankruptcies/bankruptcy, activities/activity, entities/entity.
        _ if word.len() > 4 && word.ends_with("ies") => {
            format!("{}y", &word[..word.len() - 3])
        }
        _ if word.len() > 3 && word.ends_with('s') && !word.ends_with("ss") && !word.ends_with("us") => {
            word[..word.len() - 1].to_owned()
        }
        _ => word.to_owned(),
    })
}

fn words(text: &str) -> BTreeSet<String> {
    let mut result: BTreeSet<String> = tokens(text)
        .iter()
        .filter_map(|word| {
            match DUTCH_STATISTICS.iter().find(|(dutch, _)| dutch == word) {
                Some((_, english)) => Some((*english).to_owned()),
                None => singular(word),
            }
        })
        .collect();
    // Fear & Greed is a named sentiment measurement, including when the
    // provider spells it without the word "sentiment".
    if result.contains("fear") && result.contains("greed") { result.insert("sentiment".into()); }
    result
}

/// Dutch statistics words and the English word the parser writes for them.
/// CBS publishes in Dutch ("Faillissementen; bedrijven en instellingen, SBI
/// 2008", tags `faillissementen`, `bedrijfstak`) while the parser writes
/// English; a Dutch-worded parse or listing meets the English one here. Exact
/// translations only: "insolventie" is not "bankruptcy", and the English
/// plural "branches" (a company's offices) is not "branche" (an industry).
///
/// English renderings (`crate::english`) now bridge every language, and the
/// calibration passes without this map when they are available. It stays for
/// when they are not (translation off, or a request's translation late or
/// failed): a parse worded in Dutch, "Faillissementen per sector", then still
/// finds CBS's bankruptcy tables, where without it only the CBS source is
/// offered.
const DUTCH_STATISTICS: &[(&str, &str)] = &[
    ("faillissement", "bankruptcy"),
    ("faillissementen", "bankruptcy"),
    ("insolventie", "insolvency"),
    ("insolventies", "insolvency"),
    ("bedrijfstak", "sector"),
    ("bedrijfstakken", "sector"),
    ("branche", "sector"),
    ("sectoren", "sector"),
    ("statistiek", "statistic"),
    ("statistieken", "statistic"),
];

/// Too general to identify what a document is about: a word of [`VAGUE`],
/// or a function word of a language other than English (see
/// `crate::english`), which a wording in that language would otherwise
/// make its rarest, strongest words.
fn vague(word: &str) -> bool {
    VAGUE.contains(&word) || crate::english::function_word(word)
}

/// Words too general to identify what a document is about.
const VAGUE: &[&str] = &[
    "the",
    "for",
    "and",
    "from",
    "with",
    "that",
    "this",
    "which",
    "about",
    "any",
    "all",
    "are",
    "its",
    "into",
    // "Bankruptcies per sector", "compared across sectors": a relation, not
    // a topic. As a token, "per" matched every price "per call".
    "per",
    "across",
    "between",
    "each",
    "retrieve",
    "provide",
    "get",
    "find",
    "search",
    "list",
    "lookup",
    "query",
    "show",
    "check",
    "fetch",
    "data",
    "information",
    "detail",
    "record",
    "result",
    "response",
    "overview",
    "current",
    "latest",
    "available",
    "company",
    "business",
    "entity",
    "general",
    "location",
    "compliance",
    "finance",
    "legal",
    "document",
    "research",
    "api",
    "endpoint",
    "service",
    "source",
    "name",
    "identifier",
    "number",
    "input",
    "output",
    "type",
    "string",
    "matching",
    "match",
    "score",
    "link",
    "based",
    "using",
    "specified",
    "supplied",
    "requested",
    "return",
];

/// The words behind the concept facets in `document`.
const FACET_WORDS: &[&str] = &[
    "jaarrekening",
    "financial",
    "statement",
    "annual",
    "account",
    "balance",
    "sheet",
    "officer",
    "director",
    "bestuurder",
    "psc",
    "ubo",
    "significant",
    "control",
    "controller",
    "beneficial",
    "ultimate",
    "owner",
    "ownership",
    "sanction",
    "sanctie",
];

impl Document {
    /// This request document with another wording's words, facets and ids.
    fn union(mut self, other: Document) -> Document {
        self.tokens.extend(other.tokens);
        self.facets |= other.facets;
        self.capabilities.extend(other.capabilities);
        self
    }
}

fn document(target: Target, text: &str, capabilities: BTreeSet<String>) -> Document {
    let mut tokens = words(text);
    let has = |word: &str| tokens.contains(word);
    let financial = has("jaarrekening")
        || (has("financial") && has("statement"))
        || (has("annual") && has("account"))
        || (has("balance") && has("sheet"))
        || capabilities.contains("company.accounts");
    let officers = ["officer", "director", "bestuurder"]
        .iter()
        .any(|word| has(word)) || (has("executive") && has("company"));
    let control = has("psc")
        || has("ubo")
        || (has("significant") && (has("control") || has("controller")))
        || ((has("beneficial") || has("ultimate")) && (has("owner") || has("ownership")));
    let sanctions = has("sanction") || has("sanctie");
    // A boundary or a violation record mentions buildings/neighbourhoods,
    // but does not answer a request for their registry data or statistics.
    let location = if has("violation") { VIOLATIONS }
        else if has("boundary") { BOUNDARY }
        else if has("permit") && (has("building") || has("construction")) { PERMITS }
        else { u16::from(has("building") || has("dwelling") || (has("bag") && has("unit"))) * BUILDING
            | u16::from(has("neighbourhood") || has("buurt")) * NEIGHBOURHOOD };
    let market = has("stock") || ((has("equity") || has("share"))
        && (has("price") || has("quote") || has("market") || has("trading")));
    let analyst = has("analyst") && (has("consensu") || has("consensus") || has("rating") || has("recommendation") || has("target"));
    let insider = has("insider") && (has("transaction") || has("trade") || has("trading") || has("dealing"));
    let valuation = has("valuation") || has("multiple") || (has("enterprise") && has("value"));
    let facets = u16::from(financial) * FINANCIAL
        | u16::from(officers) * OFFICERS
        | u16::from(control) * CONTROL
        | u16::from(sanctions) * SANCTIONS
        | u16::from(market) * STOCK_MARKET
        | u16::from(analyst) * ANALYST_CONSENSUS
        | u16::from(insider) * INSIDER_TRADES
        | u16::from(valuation) * VALUATION
        | u16::from(has("legislation")) * LEGISLATION
        | u16::from(has("adverse") && has("media")) * ADVERSE_MEDIA
        | u16::from(has("sentiment")) * SENTIMENT
        | location;
    tokens.retain(|token| !vague(token));
    for &(facet, marker) in FACETS {
        if facets & facet != 0 { tokens.insert(marker.into()); }
    }
    Document {
        derivative_asset: false,
        enterprise_multiple: false,
        operation_words: BTreeSet::new(),
        dns_record_types: relevance::dns_record_types(text),
        topic_scope: tokens.clone(),
        person_scope: relevance::person_scope(opening_description(text)),
        target,
        tokens,
        facets,
        capabilities,
        contract: BTreeSet::new(),
        contract_facets: 0,
        namespace: BTreeSet::new(),
        coverage: country::Coverage::Unknown,
    }
}

fn identity(text: &str) -> String {
    crate::english::fold(text)
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect()
}
fn identity_matches(hint: &str, value: &str) -> bool {
    let value = identity(value);
    !hint.is_empty() && (value == hint || (hint.len() >= 3 && value.contains(hint)))
}
fn hinted_sources(capability: &Capability, catalog: &Catalog) -> Option<BTreeSet<String>> {
    let hint = identity(capability.source_hint.as_deref()?);
    let mut sources: BTreeSet<_> = catalog
        .known_sources
        .iter()
        .filter(|source| {
            identity_matches(&hint, &source.slug)
                || identity_matches(&hint, &source.name)
                || source
                    .metadata
                    .provider_slug
                    .as_deref()
                    .is_some_and(|provider| identity_matches(&hint, provider))
        })
        .map(|source| source.slug.clone())
        .collect();
    sources.extend(
        catalog
            .services
            .iter()
            .filter(|service| identity_matches(&hint, &service.api_slug))
            .map(|service| service.api_slug.clone()),
    );
    (!sources.is_empty()).then_some(sources)
}

/// A named platform can be served by a third-party provider. When it is not
/// a provider identity in the catalog, retain only operations that actually
/// name it; an unknown hint must not silently become unrestricted search.
fn operation_names_source(hint: &str, candidate: &Document, catalog: &Catalog) -> bool {
    let hint = identity(hint);
    match candidate.target {
        Target::Endpoint(position) => {
            let service = &catalog.services[position];
            identity_matches(&hint, &service.name)
                || identity_matches(&hint, opening_description(service.description.as_deref().unwrap_or("")))
                || service.source_url.as_deref().is_some_and(|url| identity_matches(&hint, url))
        }
        Target::Listing(position, listing) => identity_matches(&hint,
            opening_description(&catalog.known_sources[position].metadata.endpoints[listing].description)),
        Target::Source(position) => identity_matches(&hint,
            opening_description(&catalog.known_sources[position].metadata.description)),
    }
}

/// Apply the parsed, explicit source preference to public Bazaar metadata.
/// This runs after discovery and preserves its ranking, bound and status; it
/// never fetches another page or relaxes the preference when nothing matches.
/// Without a named source, a listing must share the capability's topic (see
/// [`filter_coinbase_in_english`]).
pub fn filter_coinbase(parsed: &ParsedRequest, matches: &mut [Match]) {
    filter_coinbase_in_english(parsed, parsed, matches, &|_| None);
}

/// The text of a Bazaar listing that its topic check reads.
pub fn listing_text(candidate: &Candidate) -> String {
    format!("{}. {}", candidate.name, candidate.description)
}

/// [`filter_coinbase`] with the capabilities in English (`english`, see
/// `crate::english`) and the English rendering of a listing's text (see
/// [`listing_text`]). A listing of the source the person named is kept. Any
/// other must share the capability's topic in either wording, as an Apiosk
/// candidate must (`relevance::Request::mentions`): the Stock Trends portfolio
/// comparison is no answer to "bankruptcies per sector". A listing that is
/// not English and has no rendering is kept, as before this check. When the
/// subject's country is known, a listing about another country is removed
/// (see `country::listing`): Chicago building violations are no answer about
/// an Amsterdam address. Unlike a demoted catalog candidate it is not kept as
/// a last resort, because nothing orders it after the relevant listings.
pub fn filter_coinbase_in_english(
    parsed: &ParsedRequest,
    english: &ParsedRequest,
    matches: &mut [Match],
    rendered: &dyn Fn(&str) -> Option<Arc<str>>,
) {
    for found in matches {
        let Some((position, capability)) = parsed
            .capabilities
            .iter()
            .enumerate()
            .find(|(_, capability)| capability.slug == found.slug)
        else {
            continue;
        };
        let hint = capability
            .source_hint
            .as_deref()
            .map(identity)
            .filter(|hint| !hint.is_empty());
        let translated = translated(english, position, capability);
        let wanted = wanted(capability, parsed).union(wanted(translated, english));
        let floor = relevance::Request::new(capability, parsed)
            .union(relevance::Request::new(translated, english));
        let signal = country::Signal::infer(parsed, capability);
        found.coinbase.retain(|candidate| {
            let text = listing_text(candidate);
            let english = rendered(&text);
            let both = format!("{text} {}", english.as_deref().unwrap_or(""));
            let mut listing = document(Target::Source(0), &both, BTreeSet::new());
            listing.person_scope = relevance::person_scope(&format!("{} {}", opening_description(&candidate.description),
                opening_description(english.as_deref().unwrap_or(""))));
            if !floor.profile_scope_matches(&listing) { return false; }
            if let Some(hint) = &hint {
                return [
                    candidate.name.as_str(),
                    candidate.description.as_str(),
                    candidate.source_slug.as_deref().unwrap_or(""),
                    candidate.url.as_deref().unwrap_or(""),
                ]
                .into_iter()
                .any(|field| identity_matches(hint, field));
            }
            if signal.adjustment(&country::listing(&both)).demoted {
                return false;
            }
            if english.is_none() && crate::english::detect(&text) == crate::english::Language::Other {
                return true;
            }
            (wanted.facets & LOCATION_FACETS == 0 || wanted.facets & listing.facets & LOCATION_FACETS != 0)
                && floor.mentions(&wanted, &listing)
        });
    }
}

/// The capability at `position` of `english`, when it is `capability` in
/// English; otherwise `capability` itself.
fn translated<'a>(english: &'a ParsedRequest, position: usize, capability: &'a Capability) -> &'a Capability {
    english
        .capabilities
        .get(position)
        .filter(|translated| translated.slug == capability.slug && translated.subject_id == capability.subject_id)
        .unwrap_or(capability)
}

/// The request side of the lexical score: the capability's id, name, outputs,
/// description and inputs, without the subjects' names and identifiers.
fn wanted(capability: &Capability, parsed: &ParsedRequest) -> Document {
    let mut generic = capability.clone();
    generic.name = format!(
        "{} {} {}",
        capability.slug,
        capability.name,
        capability.outputs.join(" ")
    );
    generic.source_hint = None;
    let text = capability_query(&generic, parsed, &Task::default());
    document(
        Target::Source(0),
        &text,
        normalized_capabilities(std::iter::once(capability.slug.as_str())),
    )
}

/// Stems of the operations Apiosk itself performs on retrieved data, the
/// parser's `deliverable.operations` (compare, analyze, summarize, export;
/// "list" is already vague). Rare in the catalog, they have a high idf, yet
/// they say what will be done with the data, not which data: "Compare
/// bankruptcies per sector" made "Compares two user-supplied portfolios" look
/// as relevant as the bankruptcy table. A source's own action (validate,
/// verify, screen) does identify it and keeps its full weight.
const DELIVERABLE_OPERATIONS: &[&str] = &["compar", "analy", "summar", "export"];
/// Share of its idf that a deliverable operation's word keeps.
const OPERATION_WEIGHT: f64 = 0.25;

fn score(wanted: &Document, candidate: &Document, index: &Index) -> f64 {
    let exact = !wanted.capabilities.is_disjoint(&candidate.capabilities);
    // Explicit concepts keep otherwise overlapping descriptions apart: annual
    // accounts are not bank accounts, and officers are not PSC records.
    if wanted.facets != 0 && wanted.facets & candidate.facets == 0 {
        return 0.0;
    }
    let overlap: Vec<_> = wanted.tokens.intersection(&candidate.tokens).collect();
    if overlap.is_empty() {
        // Generic reviewed contracts such as company.search can consist
        // entirely of stop words. An exact contract is still a usable match.
        return if exact { 1000.0 } else { 0.0 };
    }
    // These accepted semantic contracts identify company-level records. Their
    // concise descriptions must not lose to a long name-search or single-PSC
    // notification description merely because that text repeats more terms.
    let direct_facets = candidate.capabilities.iter().fold(0, |facets, capability| {
        facets
            | match capability.as_str() {
                "company.officers" => OFFICERS,
                "company.significant.control" => CONTROL,
                "company.financial.statements" | "company.accounts" => FINANCIAL,
                "company.sanctions.screen" => SANCTIONS,
                "general.building.bag.unit" | "general.building.bag.building" => BUILDING,
                "general.area.cbs.neighbourhood.2025" => NEIGHBOURHOOD,
                _ => 0,
            }
    });
    overlap
        .into_iter()
        .map(|token| {
            let idf = ((index.documents.len() + 1) as f64
                / (index.frequency.get(token).copied().unwrap_or(0) + 1) as f64)
                .ln()
                + 1.0;
            idf * if token.starts_with('@') {
                8.0
            } else if DELIVERABLE_OPERATIONS
                .iter()
                .any(|stem| token.starts_with(stem))
            {
                OPERATION_WEIGHT
            } else {
                1.0
            }
        })
        .sum::<f64>()
        + if exact { 1000.0 } else { 0.0 }
        + (wanted.facets & direct_facets).count_ones() as f64 * 100.0
        // Typed legislation retrieval contracts provide legal-source metadata;
        // broad applicability advice must not displace those retrievals.
        + if (candidate.namespace.contains("government") || candidate.namespace.contains("legislation"))
            && !candidate.contract.is_empty() && wanted.facets & candidate.facets & LEGISLATION != 0 { 100.0 } else { 0.0 }
}

fn safe_url(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    (value.len() <= 2048
        && url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none())
    .then(|| url.to_string())
}
fn endpoint(service: &Service) -> Candidate {
    Candidate {
        origin: "apiosk".into(),
        name: service.name.clone(),
        description: service
            .description
            .clone()
            .unwrap_or_else(|| service.contract.capability_slug.clone()),
        source_slug: Some(service.api_slug.clone()),
        capability: Some(service.contract.capability_slug.clone()),
        endpoint_id: Some(service.endpoint_id.to_string()),
        url: service.source_url.as_deref().and_then(safe_url),
        method: service.method.clone(),
        price: Some(format!("{:.6} USD", service.total() as f64 / 1_000_000.0)),
        network: None,
        calls_30d: None,
        availability: if service.unverified {
            "unverified"
        } else {
            "supported"
        }
        .into(),
    }
}
fn source(source: &KnownSource) -> Candidate {
    Candidate {
        origin: "apiosk".into(),
        name: source.name.clone(),
        description: source.metadata.description.clone(),
        source_slug: Some(source.slug.clone()),
        capability: None,
        endpoint_id: None,
        // KnownSource has no documented URL. Do not invent an executable URL.
        url: None,
        method: String::new(),
        price: None,
        network: None,
        calls_30d: None,
        availability: "discovery_only".into(),
    }
}

fn listing(source: &KnownSource, endpoint: &ListedEndpoint) -> Candidate {
    Candidate {
        origin: "apiosk".into(), name: source.name.clone(), description: endpoint.description.clone(),
        source_slug: Some(source.slug.clone()), capability: None,
        endpoint_id: Some(endpoint.endpoint_id.to_string()),
        url: endpoint.source_url.as_deref().and_then(safe_url), method: endpoint.method.clone(),
        // Execution and its final price still need an accepted contract.
        price: None, network: None, calls_30d: None, availability: "discovery_only".into(),
    }
}

/// A relevant candidate and its place in the ranking.
struct Ranked<T> {
    /// Bound to another country than the subject's (see `country`).
    demoted: bool,
    /// Lexical relevance times the country factor.
    score: f64,
    item: T,
}

/// Rank each generic capability independently across the whole local directory.
/// Subject labels and identifiers never make a source relevant: they are
/// scrubbed from the lexical query and the relevance floor. They only order
/// the relevant candidates by the subject's country (see `country`).
pub fn search(parsed: &ParsedRequest, catalog: &Catalog) -> Vec<Match> {
    search_in_english(parsed, parsed, catalog)
}

/// [`search`] reading each capability in its own wording and in English:
/// `english` is `parsed` with its capabilities in English (see
/// `crate::english::English::request`). A candidate matching either wording
/// is found; the query, country signal and source preference are the
/// original's.
pub fn search_in_english(parsed: &ParsedRequest, english: &ParsedRequest, catalog: &Catalog) -> Vec<Match> {
    let index = index(catalog);
    let empty_task = Task::default();
    parsed
        .capabilities
        .iter()
        .enumerate()
        .filter(|(_, capability)| capability.slug != "general.conversation")
        .map(|(position, capability)| {
            let query = capability_query(capability, parsed, &empty_task);
            if crate::capability_intake::answer_instruction(capability) {
                return Match { slug: capability.slug.clone(), query, operations: BTreeSet::new(),
                    apiosk: vec![], coinbase: vec![], coinbase_status: "ready".into() };
            }
            let translated = translated(english, position, capability);
            let wanted = wanted(capability, parsed).union(wanted(translated, english));
            let restricted = hinted_sources(capability, catalog);
            let floor = relevance::Request::new(capability, parsed)
                .union(relevance::Request::new(translated, english));
            let signal = country::Signal::infer(parsed, capability);
            let mut endpoints = Vec::new();
            let mut listings = Vec::new();
            let mut sources = Vec::new();
            for candidate in &index.documents {
                let (slug, position) = match candidate.target {
                    Target::Endpoint(position) => (&catalog.services[position].api_slug, position),
                    Target::Listing(position, _) => (&catalog.known_sources[position].slug, position),
                    Target::Source(position) => (&catalog.known_sources[position].slug, position),
                };
                if restricted
                    .as_ref()
                    .is_some_and(|slugs| !slugs.contains(slug))
                {
                    continue;
                }
                if restricted.is_none() && capability.source_hint.as_deref()
                    .is_some_and(|hint| !operation_names_source(hint, candidate, catalog)) {
                    continue;
                }
                let score = score(&wanted, candidate, &index);
                if score <= 0.0 || !floor.passes(&wanted, candidate, &index, restricted.is_some()) {
                    continue;
                }
                let adjustment = signal.adjustment(&candidate.coverage);
                // A known jurisdiction conflict is not a fallback capability.
                // Keeping it at the bottom still allowed a planner to select
                // a French register for a British or Dutch company.
                if adjustment.demoted { continue; }
                let (demoted, score) = (adjustment.demoted, score * adjustment.factor);
                match candidate.target {
                    Target::Endpoint(_) => endpoints.push(Ranked {
                        demoted,
                        score,
                        item: &catalog.services[position],
                    }),
                    Target::Listing(_, listing) => listings.push(Ranked {
                        demoted, score, item: (&catalog.known_sources[position], &catalog.known_sources[position].metadata.endpoints[listing]),
                    }),
                    Target::Source(_) => sources.push(Ranked {
                        demoted,
                        score,
                        item: &catalog.known_sources[position],
                    }),
                }
            }
            endpoints.sort_by(|a, b| {
                a.demoted
                    .cmp(&b.demoted)
                    .then(b.score.total_cmp(&a.score))
                    .then(a.item.unverified.cmp(&b.item.unverified))
                    .then(a.item.total().cmp(&b.item.total()))
                    .then(a.item.api_slug.cmp(&b.item.api_slug))
                    .then(a.item.endpoint_id.cmp(&b.item.endpoint_id))
            });
            sources.sort_by(|a, b| {
                a.demoted
                    .cmp(&b.demoted)
                    .then(b.score.total_cmp(&a.score))
                    .then(a.item.slug.cmp(&b.item.slug))
            });
            listings.sort_by(|a, b| b.score.total_cmp(&a.score)
                .then(a.item.0.slug.cmp(&b.item.0.slug))
                .then(a.item.1.endpoint_id.cmp(&b.item.1.endpoint_id)));
            let mut seen_operations = BTreeSet::new();
            let mut endpoint_slots = BTreeMap::<uuid::Uuid, usize>::new();
            let mut apiosk: Vec<_> = endpoints
                .iter()
                // MCP dispatchers expose several typed operations through
                // one HTTP endpoint. Keep three endpoint alternatives and
                // up to three matching typed operations for each, so one
                // dispatcher cannot crowd other sources out of the shortlist.
                .filter(|ranked| {
                    if !seen_operations.insert((ranked.item.endpoint_id, ranked.item.contract.capability_slug.as_str())) { return false; }
                    if !endpoint_slots.contains_key(&ranked.item.endpoint_id) && endpoint_slots.len() >= 3 { return false; }
                    let count = endpoint_slots.entry(ranked.item.endpoint_id).or_default();
                    *count += 1; *count <= 3
                })
                .map(|ranked| endpoint(ranked.item))
                .collect();
            let mut seen_endpoints: BTreeSet<_> = apiosk.iter().filter_map(|candidate| candidate.endpoint_id.clone()).collect();
            // Missing automation support must not erase registered matching
            // endpoints or displace executable choices. Keep both shortlists.
            apiosk.extend(listings.iter()
                .filter(|ranked| seen_endpoints.insert(ranked.item.1.endpoint_id.to_string()))
                .take(3).map(|ranked| listing(ranked.item.0, ranked.item.1)));
            let mut seen_sources: BTreeSet<_> = apiosk
                .iter()
                .filter_map(|candidate| candidate.source_slug.clone())
                .collect();
            for candidate in sources.iter().map(|ranked| ranked.item) {
                if apiosk.len() >= 3 {
                    break;
                }
                if seen_sources.insert(candidate.slug.clone()) {
                    apiosk.push(source(candidate));
                }
            }
            let operations = apiosk
                .iter()
                .filter(|candidate| candidate.availability == "supported" && candidate.endpoint_id.is_some())
                .filter_map(|candidate| candidate.capability.clone())
                .collect();
            Match {
                slug: capability.slug.clone(),
                query,
                operations,
                apiosk,
                coinbase: vec![],
                coinbase_status: "pending".into(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{catalog::Snapshot, readiness::Metadata};
    use serde_json::json;
    use uuid::Uuid;

    fn service(id: u128, capability: &str, name: &str, description: &str, source: &str) -> Service {
        let fixture: Snapshot = serde_json::from_str(include_str!(
            "../../tests/fixtures/integration-catalog.json"
        ))
        .unwrap();
        let mut service = fixture.services[0].clone();
        service.id = Uuid::from_u128(id);
        service.endpoint_id = Uuid::from_u128(id + 1000);
        service.api_slug = source.into();
        service.contract.capability_slug = capability.into();
        service.name = name.into();
        service.description = Some(description.into());
        service
    }
    fn catalog(services: Vec<Service>, known_sources: Vec<KnownSource>) -> Catalog {
        let fixture: Snapshot = serde_json::from_str(include_str!(
            "../../tests/fixtures/integration-catalog.json"
        ))
        .unwrap();
        Catalog::build(Snapshot {
            services,
            known_sources,
            vocabulary: fixture.vocabulary,
        })
    }
    fn known(slug: &str, name: &str, description: &str) -> KnownSource {
        KnownSource {
            slug: slug.into(),
            name: name.into(),
            block_reason: None,
            metadata: Metadata {
                description: description.into(),
                ..Default::default()
            },
        }
    }
    fn parsed(capabilities: Vec<serde_json::Value>) -> ParsedRequest {
        let capabilities: Vec<_> = capabilities.into_iter().map(|value| json!({
            "slug":value["slug"],"name":value["name"],"description":value["description"].as_str().unwrap_or("Retrieve the requested information."),
            "domain":"company","inputs":[],"outputs":[],"subject_id":"s1","source_hint":value["source_hint"],"confidence":0.9
        })).collect();
        crate::capability_intake::normalize(&json!({"language":"en","subjects":[{"id":"s1","label":"Monzo Bank Limited","type":"company","identifiers":{}}],
            "capabilities":capabilities,"deliverable":{"format":"chat","operations":[]}}).to_string(),
            "Find information for Monzo Bank Limited from Companies House and preferred", "fixture").unwrap()
    }

    fn bazaar(description: &str) -> Candidate {
        Candidate {
            origin: "coinbase".into(),
            name: "Directory endpoint".into(),
            description: description.into(),
            source_slug: None,
            capability: None,
            endpoint_id: None,
            url: Some("https://example.test/public".into()),
            method: "GET".into(),
            price: None,
            network: None,
            calls_30d: None,
            availability: "discovery_only".into(),
        }
    }

    #[test]
    fn dns_record_types_do_not_treat_an_indefinite_article_as_ipv4() {
        for text in ["A DNS record lookup", "A quick DNS lookup", "a DNS lookup", "Retrieve a record for this domain", "a record"] {
            assert_eq!(relevance::dns_record_types(text), 0, "{text}");
        }
        for text in ["DNS A records", "A records", "a_records", "domain.dns-a", "DNS type A"] {
            assert_eq!(relevance::dns_record_types(text), 1, "{text}");
        }
        assert_eq!(relevance::dns_record_types("A and AAAA records"), 3);
    }

    #[test]
    fn dns_record_types_do_not_collapse_into_mail_or_registration_metadata() {
        let catalog = catalog(vec![
            service(991, "domain.registration", "RDAP and DNS", "Domain registration date (RDAP); DNS MX and nameservers are available from other operations.", "domain-intelligence"),
            service(992, "domain.mail-dns", "RDAP and DNS", "Domain mail servers (DNS MX); Registration and nameservers are separate operations.", "domain-intelligence"),
            service(993, "domain.dns-a", "DNS records", "Exact DNS A records (IPv4 addresses).", "domain-intelligence"),
            service(994, "domain.dns-aaaa", "DNS records", "Exact DNS AAAA records (IPv6 addresses).", "domain-intelligence"),
            service(995, "domain.dns-ns", "DNS records", "Exact DNS NS records (nameservers).", "domain-intelligence"),
            service(996, "domain.dns-txt", "DNS records", "Exact DNS TXT records including returned SPF and DMARC policy text.", "domain-intelligence"),
        ], vec![]);
        for (label, output, expected) in [
            ("DNS A records", "a_records", "domain.dns-a"),
            ("DNS AAAA records", "aaaa_records", "domain.dns-aaaa"),
            ("DNS NS records", "nameservers", "domain.dns-ns"),
            ("DNS TXT records", "txt_records", "domain.dns-txt"),
        ] {
            let mut request = parsed(vec![json!({"slug":"research.domain.dns", "name":label})]);
            request.capabilities[0].domain = "research".into();
            request.capabilities[0].outputs = vec![output.into()];
            let found = search(&request, &catalog);
            assert_eq!(found[0].operations, BTreeSet::from([expected.into()]), "{label}: {:?}", found[0].apiosk);
        }
        let request = parsed(vec![json!({"slug":"domain.mail-dns", "name":"DNS MX mail servers"})]);
        assert_eq!(search(&request, &catalog)[0].operations, BTreeSet::from(["domain.mail-dns".into()]));
        let mut request = parsed(vec![json!({"slug":"research.domain.mail.dns", "name":"Domain mail DNS records"})]);
        request.capabilities[0].description = "Retrieve public DNS MX mail-routing records. This requests MX, not TXT, A, AAAA or CNAME records, and does not prove mail delivery or ownership.".into();
        request.capabilities[0].outputs = vec!["mx".into(), "accepts_mail".into()];
        assert_eq!(search(&request, &catalog)[0].operations, BTreeSet::from(["domain.mail-dns".into()]));
        let mut request = parsed(vec![json!({"slug":"domain.registration", "name":"RDAP registration"})]);
        request.capabilities[0].outputs = vec!["registered_at".into(), "nameservers".into()];
        assert!(search(&request, &catalog)[0].operations.contains("domain.registration"));
    }

    #[test]
    fn dns_record_scope_is_not_overridden_by_an_exact_slug_or_source_hint() {
        let catalog = catalog(vec![service(997, "domain.mail-dns", "DNS intelligence", "DNS MX mail servers", "domain-intelligence")], vec![]);
        let mut request = parsed(vec![json!({"slug":"domain.mail-dns", "name":"DNS TXT and NS records", "source_hint":"domain-intelligence"})]);
        request.capabilities[0].outputs = vec!["txt_records".into(), "nameservers".into()];
        assert!(search(&request, &catalog)[0].operations.is_empty());
    }

    #[test]
    fn multiple_dns_types_discover_contributing_calls_but_require_complete_output_coverage() {
        use crate::{capability_intake::Intake, parser::{Goal, Intent, Subject}, resolver::{resolve, Resolution}};
        let catalog = Catalog::build(serde_json::from_str(include_str!("../../tests/fixtures/domain-intelligence/typed/catalog.json")).unwrap());
        let mut request = parsed(vec![json!({"slug":"research.domain.dns", "name":"DNS A and MX records"})]);
        request.subjects[0].label = "www.mollie.com".into();
        request.subjects[0].kind = "domain".into();
        request.capabilities[0].domain = "research".into();
        request.capabilities[0].outputs = vec!["a_records".into(), "mx_records".into()];
        let matches = search(&request, &catalog);
        assert_eq!(matches[0].operations, BTreeSet::from(["domain.dns-a".into(), "domain.dns-mx".into()]));
        let intake = Intake { parsed_request:request, selected_capabilities:matches[0].operations.clone(),
            matches, apiosk_sources_searched:1, catalog_version:catalog.version.clone() };
        let goal = |capability: &str| {
            let service = catalog.services.iter().find(|service| service.contract.capability_slug == capability).unwrap();
            Goal { capability:capability.into(), subject:0, constraints:BTreeMap::from([
                ("source".into(), json!(service.api_slug)), ("endpoint_id".into(), json!(service.endpoint_id))]) }
        };
        let mut task = Task { capability_intake:Some(intake), ..Default::default() };
        let mut intent = Intent { subjects:vec![Subject {entity_ref:None,label:"www.mollie.com".into(),
            values:BTreeMap::from([("domain.dns_name".into(),json!("www.mollie.com"))])}],
            goals:vec![goal("domain.dns-a")], answer_operations:vec![], unresolved_references:vec![] };
        crate::engine::bind_subjects(&mut task, &catalog, &mut intent).unwrap();
        let Resolution::Plan(partial) = resolve(&catalog, &task, &intent) else {panic!("A lookup")};
        assert_eq!(crate::parser::plan_coverage::uncovered_outputs(&task, &catalog, &intent, &partial), vec!["DNS A and MX records: mx_records"]);
        intent.goals.push(goal("domain.dns-mx"));
        let Resolution::Plan(complete) = resolve(&catalog, &task, &intent) else {panic!("A and MX lookups")};
        assert_eq!(complete.steps.len(), 2);
        assert!(crate::parser::plan_coverage::uncovered_outputs(&task, &catalog, &intent, &complete).is_empty());
    }

    #[test]
    fn crypto_market_data_excludes_classified_and_rental_markets() {
        let mut request = parsed(vec![json!({"slug":"research.market.analysis","name":"Market analysis",
            "description":"Retrieve price, market cap, trading volume and liquidity for a cryptocurrency token."})]);
        request.subjects[0].label = "Cardano".into();
        request.subjects[0].kind = "cryptocurrency_token".into();
        request.capabilities[0].domain = "research".into();
        request.capabilities[0].outputs = vec!["price".into(), "market_cap".into(), "volume".into()];
        let crypto = "Crypto token price and market data with market cap and 24-hour volume.";
        let classified = "Search public classified listings by market area, category, query, and common marketplace filters.";
        let rentals = "Find public rental listings by market resource id, rental category, price, bedrooms and location.";
        let catalog = catalog(vec![
            service(940, "operation.crypto.market", "Crypto prices", crypto, "crypto"),
            service(941, "operation.classified.market", "AgentBodega", classified, "agentbodega"),
            service(942, "operation.rental.market", "AgentBodega", rentals, "agentbodega"),
        ], vec![]);
        let mut found = search(&request, &catalog);
        assert_eq!(found[0].apiosk.len(), 1, "{:?}", found[0].apiosk);
        assert_eq!(found[0].apiosk[0].name, "Crypto prices");
        found[0].coinbase = vec![bazaar(crypto), bazaar(classified), bazaar(rentals)];
        filter_coinbase(&request, &mut found);
        assert_eq!(found[0].coinbase.len(), 1);
        assert_eq!(found[0].coinbase[0].description, crypto);
        // The live registered catalog must not reintroduce broad marketplaces.
        let live = search(&request, calibration::catalog());
        assert!(live[0].apiosk.iter().all(|candidate| candidate.source_slug.as_deref() != Some("agentbodega")));
    }

    #[test]
    fn sentiment_analysis_keeps_its_topic_instead_of_matching_news_context() {
        let mut request = parsed(vec![json!({"slug":"research.sentiment.analysis","name":"Sentiment analysis",
            "description":"Analyze the sentiment of public discussions, news, and social media about a specific cryptocurrency token."})]);
        request.subjects[0].label = "Cardano".into();
        request.subjects[0].kind = "cryptocurrency_token".into();
        request.capabilities[0].domain = "research".into();
        request.capabilities[0].inputs = vec!["token_name".into(), "source_type".into()];
        request.capabilities[0].outputs = ["sentiment_score", "positive_mentions", "negative_mentions", "neutral_mentions", "dominant_sentiment"].map(str::to_owned).to_vec();
        let sentiment = "Market sentiment with positive, negative and neutral shares for explicit tickers. Recent 30-day provider window only.";
        let c = catalog(vec![
            service(910,"finance_markets.cityfalcon-news","CityFALCON","Financial news for explicit full tickers. Recent 30-day provider window only.","cityfalcon"),
            service(911,"finance_markets.cityfalcon-sentiment","CityFALCON",sentiment,"cityfalcon"),
            service(912,"operation.other.sentiment","Another provider",sentiment,"another-provider"),
            service(913,"operation.pulse.sentiment","Pulse member",sentiment,"pulse-member"),
        ], vec![]);
        let found = search(&request, &c);
        assert_eq!(found[0].apiosk.len(), 3);
        assert!(found[0].apiosk.iter().all(|candidate| candidate.description == sentiment));
        let live_catalog = search(&request, calibration::catalog());
        assert!(live_catalog[0].apiosk.iter().any(|candidate|
            candidate.capability.as_deref() == Some("finance_markets.cityfalcon-sentiment")),
            "sentiment must survive the full catalog ranking: {:?}", live_catalog[0].apiosk);
        assert!(!live_catalog[0].operations.contains("finance_markets.cityfalcon-news"));
        let mut external = found;
        external[0].coinbase = vec![
            bazaar("Market & brand sentiment for any keyword, brand, ticker or product: analyses the sentiment of web & news content."),
            bazaar("Composite bull/bear sentiment for a crypto ticker or topic, fused across three sources."),
            bazaar("Composite crypto market sentiment, optionally focused on a coin or query."),
            bazaar("Real-time directional sentiment for any forex pair or gold — retail crowd positioning and COT alignment."),
        ];
        filter_coinbase(&request, &mut external);
        assert_eq!(external[0].coinbase.len(), 3);
    }

    #[test]
    fn pulse_sentiment_endpoints_are_found_without_execution_contracts() {
        // Exact registered Pulse descriptions/identities read on 2026-10-06.
        // No execution contracts are manufactured for these directory rows.
        let sources: Vec<KnownSource> = serde_json::from_str(include_str!(
            "../../tests/fixtures/pulse-sentiment-discovery.json")).unwrap();
        let mut request = parsed(vec![json!({"slug":"research.sentiment.analysis","name":"Sentiment analysis",
            "description":"Analyze the sentiment of public discussions, news, and social media about a specific cryptocurrency token."})]);
        request.subjects[0].label = "Cardano".into();
        request.subjects[0].kind = "cryptocurrency_token".into();
        request.capabilities[0].domain = "research".into();
        request.capabilities[0].outputs = vec!["sentiment_score".into(), "dominant_sentiment".into()];
        let c = catalog(vec![], sources.clone());
        let found = search(&request, &c);
        let expected = BTreeSet::from([
            "88994768-2735-4a99-9450-143a441f8ae1", // CryptoPulse snapshot
            "10113620-3e13-4378-a615-31e939fd8b97", // MacroPulse crypto
            "285e4ede-4a71-4527-b3c7-9c0de9ee1033", // SignalPulse crypto-lite
        ]);
        assert_eq!(found[0].apiosk.iter().filter_map(|candidate| candidate.endpoint_id.as_deref()).collect::<BTreeSet<_>>(), expected,
            "{:#?}", found[0].apiosk);
        assert!(found[0].apiosk.iter().all(|candidate| candidate.availability == "discovery_only" && candidate.capability.is_none()));
        assert!(found[0].operations.is_empty());
        // The match remains visible alongside an executable local result.
        let city = service(920, "finance_markets.cityfalcon-sentiment", "CityFALCON",
            "Market sentiment with positive, negative and neutral shares for explicit tickers.", "cityfalcon");
        let combined = search(&request, &catalog(vec![city], sources.clone()));
        assert_eq!(combined[0].apiosk.len(), 4);
        assert_eq!(combined[0].operations, BTreeSet::from(["finance_markets.cityfalcon-sentiment".to_owned()]));
        // A different subject must not supply false lexical evidence, and a
        // provider preference must still resolve all of Pulse's member APIs.
        request.subjects[0].label = "Solana".into();
        request.capabilities[0].source_hint = Some("Pulse Network".into());
        assert_eq!(search(&request, &c)[0].apiosk.len(), 3);

        // The real forex sentiment endpoint is discoverable for its domain,
        // but is excluded from the crypto request above.
        request.subjects.clear();
        request.capabilities[0].subject_id = None;
        request.capabilities[0].description = "Directional sentiment for a forex pair.".into();
        let forex = search(&request, &c);
        assert!(forex[0].apiosk.iter().any(|candidate| candidate.endpoint_id.as_deref() == Some("d785de6c-cdee-4fd8-8847-ed3d69fa04ce")));
    }

    #[test]
    fn registered_listing_does_not_duplicate_its_executable_endpoint() {
        let mut endpoint = service(921, "research.sentiment.analysis", "Sentiment source",
            "Sentiment analysis with positive and negative scores.", "sentiment-source");
        let mut source = known("sentiment-source", "Sentiment source", "Sentiment analysis.");
        source.metadata.endpoints.push(ListedEndpoint {
            endpoint_id: endpoint.endpoint_id, description: endpoint.description.clone().unwrap(),
            method: "GET".into(), path: "/sentiment".into(), source_url: None, registration: None,
        });
        let request = parsed(vec![json!({"slug":"research.sentiment.analysis","name":"Sentiment analysis"})]);
        let found = search(&request, &catalog(vec![endpoint.clone()], vec![source.clone()]));
        assert_eq!(found[0].apiosk.len(), 1);
        assert_eq!(found[0].apiosk[0].availability, "supported");
        // A rejected contract still leaves its registered endpoint visible,
        // but cannot become a selected executable operation.
        endpoint.list_atomic = -1;
        let found = search(&request, &catalog(vec![endpoint], vec![source]));
        assert_eq!(found[0].apiosk.len(), 1);
        assert_eq!(found[0].apiosk[0].availability, "discovery_only");
        assert!(found[0].operations.is_empty());
    }

    #[test]
    fn dutch_statistics_words_meet_the_parsers_english() {
        assert_eq!(
            words("Faillissementen; bedrijven en instellingen, bedrijfstak"),
            words("bankruptcies bedrijven instellingen sector")
        );
        assert!(words("CBS-statistieken per branche").contains("statistic"));
        assert!(words("CBS-statistieken per branche").contains("sector"));
        // A company's branches (offices) are not an industry.
        assert!(!words("addresses, website and branches").contains("sector"));
        assert!(!words("insolventie").contains("bankruptcy"));
    }

    #[test]
    fn input_names_do_not_turn_medical_bill_advice_into_company_accounts() {
        let mut medical = service(400, "operation.clearcarepulse.get-api-care-rights", "ClearCarePulse",
            "US medical-bill rights check. Nonprofit-hospital financial assistance and collection shields; ACA appeal deadlines and Medicare redetermination.", "clearcarepulse");
        medical.contract.input_map.values_mut().next().unwrap().name = "first-statement-date".into();
        let accounts = service(401, "company.financial-statements", "Company accounts",
            "Retrieve the financial statements filed by a company.", "registry");
        let mut request = parsed(vec![json!({"slug":"company.financial.statements","name":"Company financial statements",
            "description":"Retrieves the financial statements of a company as filed with the relevant authority."})]);
        request.capabilities[0].outputs = vec!["financial_statements".into(), "filing_date".into()];
        let catalog = catalog(vec![medical, accounts], vec![]);
        assert_eq!(catalog.services.len(), 2);
        let matches = search(&request, &catalog);
        assert_eq!(matches[0].apiosk.iter().map(|candidate| candidate.source_slug.as_deref().unwrap()).collect::<Vec<_>>(), vec!["registry"]);
    }

    #[test]
    fn coinbase_named_source_filters_unrelated_country_registries() {
        let mut request = parsed(vec![json!({"slug":"company.officers","name":"Officers"})]);
        request.capabilities[0].source_hint = Some("companies_house".into());
        let mut matches = search(&request, &catalog(vec![], vec![]));
        matches[0].coinbase_status = "ready".into();
        matches[0].coinbase = vec![
            bazaar("Sirenic GB company officers from Companies House"),
            bazaar("Company directors from the Cyprus registry"),
            bazaar("eu-verify UK persons with significant control (CompaniesHouse)"),
            bazaar("Latvia company officers"),
            bazaar("Japan company directors"),
        ];
        filter_coinbase(&request, &mut matches);
        assert_eq!(matches[0].coinbase.len(), 2);
        assert!(matches[0].coinbase[0].description.starts_with("Sirenic GB"));
        assert!(matches[0].coinbase[1]
            .description
            .starts_with("eu-verify UK"));
        assert_eq!(matches[0].coinbase_status, "ready");
    }

    #[test]
    fn location_example_finds_building_records_and_statistics_without_boundaries_or_violations() {
        let native = crate::ask_v2::test_catalog::locations();
        let mut services = native.services.clone();
        let mut vocabulary = native.vocabulary;
        let fixture: Snapshot = serde_json::from_str(include_str!("../../tests/fixtures/integration-catalog.json")).unwrap();
        for fact in fixture.vocabulary { vocabulary.entry(fact.slug.clone()).or_insert(fact); }
        services.extend([
            service(901, "area.boundaries", "Retrieve the official boundary and code of an administrative area", "Municipality, district and neighbourhood boundaries by address.", "pdok"),
            service(902, "building.violations", "Chicago building violations address check: open DOB code cases for a street address", "Building violation records.", "chicago-building-violations"),
        ]);
        let catalog = Catalog::build(Snapshot { services, vocabulary: vocabulary.into_values().collect(), known_sources: vec![] });
        assert!(catalog.rejected.is_empty(), "{:?}", catalog.rejected);
        let mut request = parsed(vec![
            json!({"slug":"location.building.information","name":"Building information","description":"Retrieve official building details for an address."}),
            json!({"slug":"location.neighbourhood.information","name":"Neighbourhood information","description":"Retrieve neighbourhood statistics for an address."}),
        ]);
        request.subjects[0].label = "Damrak 1, Amsterdam".into();
        request.subjects[0].kind = "address".into();
        let matches = search(&request, &catalog);
        assert!(!matches[0].apiosk.is_empty());
        assert!(!matches[1].apiosk.is_empty());
        assert!(matches[0].apiosk.iter().all(|c| c.capability.as_deref().is_some_and(|c| c.starts_with("building.bag-"))));
        assert_eq!(matches[1].apiosk[0].capability.as_deref(), Some("area.cbs-neighbourhood-2025"));
        assert!(matches.iter().flat_map(|m| &m.apiosk).all(|c| !matches!(c.capability.as_deref(), Some("area.boundaries" | "building.violations"))));

        // The actual failed chat parsed both outputs into one capability and
        // retained "Damrak 1" as the subject. Both requested kinds must fit in
        // the three-result limit, ahead of address context and boundaries.
        let mut combined = parsed(vec![json!({"slug":"location.building.neighbourhood.info","name":"Building and neighbourhood information","description":"Provides detailed information about a building and its surrounding neighbourhood."})]);
        combined.subjects[0].label = "Damrak 1".into();
        combined.subjects[0].kind = "address".into();
        combined.capabilities[0].inputs = vec!["address".into()];
        combined.capabilities[0].outputs = vec!["building_info".into(), "neighbourhood_info".into()];
        let found = search(&combined, &catalog);
        assert_eq!(found[0].operations, BTreeSet::from(["building.bag-building".into(), "building.bag-unit".into(), "area.cbs-neighbourhood-2025".into()]));

        // These are distinct supported requests, not globally hidden sources.
        let mut violation_request = parsed(vec![json!({"slug":"location.building.violations","name":"Building violations"})]);
        violation_request.subjects.clear();
        let violations = search(&violation_request, &catalog);
        assert_eq!(violations[0].apiosk[0].capability.as_deref(), Some("building.violations"));
        let mut boundary_request = parsed(vec![json!({"slug":"location.area.boundaries","name":"Neighbourhood boundaries"})]);
        boundary_request.subjects.clear();
        let boundaries = search(&boundary_request, &catalog);
        assert_eq!(boundaries[0].apiosk[0].capability.as_deref(), Some("area.boundaries"));
    }

    #[test]
    fn bazaar_location_matches_do_not_reintroduce_unrequested_violations_or_boundaries() {
        let mut request = parsed(vec![json!({"slug":"location.building.neighbourhood.info","name":"Building and neighbourhood information"})]);
        request.subjects[0].label = "Damrak 1, Amsterdam".into();
        request.subjects[0].kind = "address".into();
        let mut matches = search(&request, &catalog(vec![], vec![]));
        matches[0].coinbase = vec![
            bazaar("Official building registry records"),
            bazaar("Chicago building violations by address"),
            bazaar("Neighbourhood boundaries"),
            bazaar("Japanese forward geocoding from an address to coordinates. Japan geocoder, building-level geocode."),
            bazaar("Building permit records. Construction and building permits across Chicago, NYC, Seattle, and San Francisco."),
        ];
        filter_coinbase(&request, &mut matches);
        assert_eq!(matches[0].coinbase.len(), 1);
        assert_eq!(matches[0].coinbase[0].description, "Official building registry records");
    }

    #[test]
    fn coinbase_source_identity_can_be_in_name_slug_or_url() {
        let mut request = parsed(vec![json!({"slug":"company.officers","name":"Officers"})]);
        request.capabilities[0].source_hint = Some("Companies House".into());
        let mut matches = search(&request, &catalog(vec![], vec![]));
        let mut name = bazaar("Officers");
        name.name = "Companies-House officers".into();
        let mut slug = bazaar("Officers");
        slug.source_slug = Some("companies_house".into());
        let mut url = bazaar("Officers");
        url.url = Some("https://api.example.test/companies-house/officers".into());
        matches[0].coinbase = vec![name, slug, url];
        filter_coinbase(&request, &mut matches);
        assert_eq!(matches[0].coinbase.len(), 3);
    }

    #[test]
    fn coinbase_hint_applies_per_capability_and_does_not_widen_empty_results() {
        let mut request = parsed(vec![
            json!({"slug":"company.officers","name":"Officers"}),
            json!({"slug":"company.financial.statements","name":"Financial statements"}),
        ]);
        request.capabilities[0].source_hint = Some("companies_house".into());
        let mut matches = search(&request, &catalog(vec![], vec![]));
        matches[0].coinbase = vec![bazaar("Latvia officers")];
        matches[1].coinbase = vec![bazaar("Latvia annual accounts")];
        matches[0].apiosk = vec![bazaar("unmodified local candidate")];
        filter_coinbase(&request, &mut matches);
        assert!(matches[0].coinbase.is_empty());
        assert_eq!(matches[1].coinbase.len(), 1);
        assert_eq!(matches[0].apiosk.len(), 1);
        assert!(matches[0].operations.is_empty());
    }

    fn bankruptcies(language: &str, name: &str, description: &str, outputs: &[&str]) -> ParsedRequest {
        crate::capability_intake::normalize(
            &json!({"language":language,"subjects":[{"id":"s1","label":"Nederland","type":"country","identifiers":{}}],
                "capabilities":[{"slug":"research.bankruptcies.bySector","name":name,"description":description,"domain":"research",
                    "inputs":["country"],"outputs":outputs,"subject_id":"s1","source_hint":null,"confidence":0.9}],
                "deliverable":{"format":"chat","operations":["compare"]}})
            .to_string(),
            "Nederland",
            "fixture",
        )
        .unwrap()
    }
    fn listing(name: &str, description: &str) -> Candidate {
        let mut candidate = bazaar(description);
        candidate.name = name.into();
        candidate
    }

    #[test]
    fn bazaar_listings_must_share_the_capabilitys_topic() {
        // Live, 2026-09-29: "Compare bankruptcies per sector in the
        // Netherlands" showed Stock Trends' portfolio comparison.
        let request = bankruptcies("en", "Bankruptcies by sector", "Compares the number of bankruptcies per sector in a country.", &["sector", "bankruptcy_count"]);
        let mut matches = search(&request, &catalog(vec![], vec![]));
        let dutch = listing("CBS Open Data", "Faillissementen per bedrijfstak en maand, uitgesproken door de rechtbank.");
        let weather = listing("Weerdienst", "Het weerbericht voor de komende dagen per provincie en per stad.");
        matches[0].coinbase = vec![
            listing("Stock Trends Market Intelligence", "Compares two user-supplied portfolios using Stock Trends decision scoring, alignment, and regime context."),
            listing("scopeapi.dev", "Python Data Analysis: run pandas over a supplied dataset to compute statistics, compare groups per category and summarize the results."),
            listing("Stock Trends Market Intelligence", "Latest sector, industry-group, or industry breadth snapshot derived from Stock Trends signal distribution."),
            listing("Insolvency Watch", "Company bankruptcy filings across Europe, updated daily."),
            dutch.clone(),
            weather.clone(),
        ];
        let mut unrendered = matches.clone();
        filter_coinbase(&request, &mut unrendered);
        let kept = |matches: &[Match]| -> Vec<String> { matches[0].coinbase.iter().map(|candidate| candidate.description.chars().take(24).collect()).collect() };
        // Without English renderings a listing in another language cannot be
        // judged and is kept, as before the topic check.
        assert_eq!(
            kept(&unrendered),
            ["Latest sector, industry-", "Company bankruptcy filin", "Faillissementen per bedr", "Het weerbericht voor de "]
        );
        // With them, it is judged in English like any other.
        let renderings: std::collections::HashMap<String, Arc<str>> = [
            (listing_text(&dutch), Arc::from("Bankruptcies per branch of industry and month, declared by the court.")),
            (listing_text(&weather), Arc::from("The weather forecast for the coming days per province and per city.")),
        ]
        .into_iter()
        .collect();
        filter_coinbase_in_english(&request, &request, &mut matches, &|text| renderings.get(text).cloned());
        assert_eq!(kept(&matches), ["Latest sector, industry-", "Company bankruptcy filin", "Faillissementen per bedr"]);
        assert_eq!(matches[0].coinbase_status, "pending");
    }

    fn building(address: &str) -> ParsedRequest {
        crate::capability_intake::normalize(
            &json!({"language":"en","subjects":[{"id":"s1","label":address,"type":"address","identifiers":{}}],
                "capabilities":[{"slug":"location.building.information","name":"Building information",
                    "description":"Retrieves building information for an address.","domain":"location",
                    "inputs":["address"],"outputs":["construction_year","building_type"],"subject_id":"s1","source_hint":null,"confidence":0.9}],
                "deliverable":{"format":"chat","operations":[]}})
            .to_string(),
            &format!("Find building and neighbourhood information for {address}."),
            "fixture",
        )
        .unwrap()
    }

    #[test]
    fn bazaar_listings_about_another_country_are_no_answer_for_a_dutch_address() {
        // Live, 2026-10-01: the starter "Find building and neighbourhood
        // information for Damrak 1, Amsterdam." offered Chicago building
        // violations and a Japanese geocoder next to PDOK.
        let listings = || {
            vec![
                listing("Chicago Building Violations", "Chicago building violations address check: open DOB violations for a street address."),
                listing("Japan Address Geocode", "Japanese forward geocoding from an address to coordinates."),
                listing("Global Geocoder", "Forward geocoding of any building address worldwide."),
                listing("BAG Lookup", "Building records for addresses in Amsterdam and the rest of the Netherlands."),
            ]
        };
        let names = |matches: &[Match]| -> Vec<String> { matches[0].coinbase.iter().map(|candidate| candidate.name.clone()).collect() };
        let request = building("Damrak 1, Amsterdam");
        let mut matches = search(&request, &catalog(vec![], vec![]));
        matches[0].coinbase = listings();
        filter_coinbase(&request, &mut matches);
        assert_eq!(names(&matches), ["Global Geocoder", "BAG Lookup"]);
        // Without a known country, output relevance still excludes violations
        // from a request for building records.
        let request = building("Damrak 1");
        let mut matches = search(&request, &catalog(vec![], vec![]));
        matches[0].coinbase = listings();
        filter_coinbase(&request, &mut matches);
        assert_eq!(names(&matches), ["Global Geocoder", "BAG Lookup"]);
    }

    #[test]
    fn a_capability_read_in_english_keeps_the_english_bazaar_listings_about_it() {
        let dutch = bankruptcies("nl", "Faillissementen per sector", "Vergelijkt het aantal faillissementen per bedrijfstak.", &["aantal_faillissementen"]);
        let mut english = dutch.clone();
        english.capabilities[0].name = "Bankruptcies per sector".into();
        english.capabilities[0].description = "Compares the number of bankruptcies per sector.".into();
        english.capabilities[0].outputs = vec!["number_of_bankruptcies".into()];
        let mut matches = search_in_english(&dutch, &english, &catalog(vec![], vec![]));
        matches[0].coinbase = vec![
            listing("Insolvency Watch", "Company bankruptcy filings across Europe, updated daily."),
            listing("Stock Trends Market Intelligence", "Compares two user-supplied portfolios using Stock Trends decision scoring."),
        ];
        filter_coinbase_in_english(&dutch, &english, &mut matches, &|_| None);
        assert_eq!(matches[0].coinbase.len(), 1);
        assert_eq!(matches[0].coinbase[0].name, "Insolvency Watch");
        // The query stays the person's own; Bazaar gets the English one.
        assert!(matches[0].query.starts_with("Faillissementen"));
    }

    #[test]
    fn unicode_words_fold_accents_and_keep_other_scripts() {
        assert!(words("Économie française, Straße").is_superset(&BTreeSet::from(["economie".into(), "francaise".into(), "strasse".into()])));
        assert_eq!(words("Défaillances"), words("defaillances"));
        assert!(words("Безработица по регионам").contains("безработица"));
        // Han and kana have no spaces: pairs of characters.
        let japanese = words("都道府県別の失業率");
        assert!(japanese.contains("失業") && japanese.contains("業率"), "{japanese:?}");
        // Foreign function words are no topic.
        assert!(document(Target::Source(0), "het aantal van de faillissementen", BTreeSet::new()).tokens.iter().all(|token| !["het", "van"].contains(&token.as_str())));
        // A capability worded in another script still searches its English id.
        let request = crate::capability_intake::normalize(
            &json!({"language":"ja","subjects":[],"capabilities":[{"slug":"company.officers","name":"会社役員","description":"会社の現在の取締役を取得します。",
                "domain":"company","inputs":[],"outputs":[],"subject_id":null,"source_hint":null,"confidence":0.9}],
                "deliverable":{"format":"chat","operations":[]}}).to_string(),
            "会社役員",
            "fixture",
        )
        .unwrap();
        let catalog = catalog(vec![service(1, "company.officers", "Company officers", "Current directors and officers", "registry")], vec![]);
        assert_eq!(search(&request, &catalog)[0].apiosk.len(), 1);
    }

    #[test]
    fn monzo_officers_and_significant_control_keep_separate_endpoint_matches() {
        let catalog = catalog(
            vec![
                service(
                    1,
                    "company.officers",
                    "Company officers",
                    "List current directors and officers",
                    "companies-house",
                ),
                service(
                    2,
                    "company.persons-with-significant-control",
                    "Persons with significant control",
                    "List beneficial owners and PSC records",
                    "companies-house",
                ),
            ],
            vec![known(
                "companies-house",
                "Companies House",
                "Company officers, directors and beneficial ownership",
            )],
        );
        let results = search(
            &parsed(vec![
                json!({"slug":"company.officers","name":"Company officers"}),
                json!({"slug":"company.significantControl","name":"Persons with significant control"}),
            ]),
            &catalog,
        );
        assert_eq!(
            results[0].operations,
            BTreeSet::from(["company.officers".into()])
        );
        assert_eq!(
            results[1].operations,
            BTreeSet::from(["company.persons-with-significant-control".into()])
        );
        assert!(results.iter().all(|found| !found.query.contains("Monzo")));
    }

    #[test]
    fn unverified_listings_remain_discoverable_but_do_not_supply_planning_operations() {
        let request = parsed(vec![json!({"slug":"company.accounts","name":"Annual accounts"})]);
        let fixture: serde_json::Value = serde_json::from_str(include_str!("../../tests/fixtures/source-document-service.json")).unwrap();
        let mut unverified: Service = serde_json::from_value(fixture["service"].clone()).unwrap();
        unverified.contract.capability_slug = "company.accounts".into();
        unverified.name = "Annual accounts".into();
        unverified.description = Some("Retrieve company annual accounts".into());
        unverified.api_slug = "unverified-registry".into();
        unverified.unverified = true;
        let build = |services| {
            let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../tests/fixtures/integration-catalog.json")).unwrap();
            snapshot.services = services;
            snapshot.vocabulary.extend(serde_json::from_value::<Vec<catalog_core::facts::contract::FactType>>(fixture["vocabulary"].clone()).unwrap());
            Catalog::build(snapshot)
        };
        let found = search(&request, &build(vec![unverified.clone()]));
        assert_eq!(found[0].apiosk.len(), 1);
        assert_eq!(found[0].apiosk[0].availability, "unverified");
        assert!(found[0].operations.is_empty());

        let supported = service(2, "company.accounts", "Annual accounts", "Retrieve company annual accounts", "supported-registry");
        let found = search(&request, &build(vec![unverified, supported]));
        assert!(found[0].apiosk.iter().any(|candidate| candidate.availability == "unverified"));
        assert!(found[0].apiosk.iter().any(|candidate| candidate.availability == "supported"));
        assert_eq!(found[0].operations, BTreeSet::from(["company.accounts".into()]));
    }

    #[test]
    fn financial_statements_match_annual_accounts_but_not_bank_accounts() {
        let catalog = catalog(
            vec![
                service(
                    1,
                    "company.accounts",
                    "Annual accounts",
                    "Retrieve annual company accounts",
                    "registry",
                ),
                service(
                    2,
                    "finance.bank-accounts",
                    "Bank accounts",
                    "Retrieve bank account balances and account statements",
                    "bank",
                ),
            ],
            vec![],
        );
        let found = search(
            &parsed(vec![
                json!({"slug":"company.financialStatements","name":"Financial statements"}),
            ]),
            &catalog,
        );
        assert_eq!(
            found[0].operations,
            BTreeSet::from(["company.accounts".into()])
        );
    }

    #[test]
    fn requested_outputs_keep_both_officers_and_psc_ahead_of_related_name_searches() {
        let catalog = catalog(vec![
            service(1, "company.officers", "Companies House", "Retrieve officers for one company", "companies-house"),
            service(2, "company.significant-control", "Companies House", "Retrieve persons with significant control for one company", "companies-house"),
            service(3, "operation.companies-house.get-search-officers", "Companies House", "Search officer names and show appointment date and resignation date for each director, person with significant control and officer", "companies-house"),
            service(4, "operation.companies-house.get-psc-history", "Companies House", "The significant control and appointment notification history for one PSC id", "companies-house"),
        ], vec![]);
        let mut request = parsed(vec![
            json!({"slug":"company.profile.officers","name":"Company officers"}),
        ]);
        request.capabilities[0].outputs = vec![
            "appointment_date".into(),
            "resignation_date".into(),
            "significant_control".into(),
        ];
        let found = search(&request, &catalog);
        let top: BTreeSet<_> = found[0]
            .apiosk
            .iter()
            .take(2)
            .filter_map(|candidate| candidate.capability.as_deref())
            .collect();
        assert_eq!(
            top,
            BTreeSet::from(["company.officers", "company.significant-control"])
        );
    }

    #[test]
    fn significant_controllers_live_parse_keeps_both_company_level_endpoints() {
        let catalog = catalog(
            vec![
                service(
                    1,
                    "company.officers",
                    "Companies House",
                    "Retrieve officers for one company",
                    "companies-house",
                ),
                service(
                    2,
                    "company.significant-control",
                    "Companies House",
                    "Retrieve persons with significant control for one company",
                    "companies-house",
                ),
                service(
                    3,
                    "operation.companies-house.get-search-officers",
                    "Companies House",
                    "Search officer names, appointments and resignation status",
                    "companies-house",
                ),
                service(
                    4,
                    "operation.companies-house.get-officer-appointments",
                    "Companies House",
                    "Retrieve appointments for one officer",
                    "companies-house",
                ),
                service(
                    5,
                    "company.officers",
                    "Other registry",
                    "Retrieve company officers",
                    "other",
                ),
            ],
            vec![],
        );
        let mut request = parsed(vec![json!({
            "slug":"company.officers.controllers",
            "name":"Company officers and significant controllers",
            "description":"Retrieves information about company officers and significant controllers from a company registry"
        })]);
        request.capabilities[0].outputs = vec![
            "officers".into(),
            "significant_controllers".into(),
            "appointment_status".into(),
            "resignation_status".into(),
        ];
        request.capabilities[0].source_hint = Some("companies_house".into());
        let found = search(&request, &catalog);
        let top: BTreeSet<_> = found[0]
            .apiosk
            .iter()
            .take(2)
            .filter_map(|candidate| candidate.capability.as_deref())
            .collect();
        assert_eq!(
            top,
            BTreeSet::from(["company.officers", "company.significant-control"])
        );
        assert!(found[0]
            .apiosk
            .iter()
            .all(|candidate| candidate.source_slug.as_deref() == Some("companies-house")));
    }

    #[test]
    fn downstream_screening_description_does_not_turn_a_roster_into_sanctions_results() {
        let catalog = catalog(vec![
            service(1, "company.uk-director-roster", "Registry", "Retrieve a complete active UK director roster for bounded OpenSanctions screening", "registry"),
            service(2, "company.sanctions-screen", "OpenSanctions", "Screen a company against sanctions lists", "opensanctions"),
        ], vec![]);
        let found = search(
            &parsed(vec![
                json!({"slug":"compliance.sanctions","name":"Company sanctions screening"}),
            ]),
            &catalog,
        );
        assert_eq!(
            found[0].operations,
            BTreeSet::from(["company.sanctions-screen".into()])
        );
    }

    #[test]
    fn professional_requests_do_not_match_parenting_or_unrequested_onchain_guides() {
        let catalog = catalog(vec![
            service(1, "company.ultimate-parent", "GLEIF", "Corporate parent and subsidiary ownership structure", "gleif"),
            service(2, "operation.parentpulse.nutrition", "ParentPulse", "Pediatric nutrition and feeding guide for parenting and family agents. Parent and child nutrition, portion sizes and risks.", "parentpulse"),
            service(3, "operation.parentpulse.childcare", "ParentPulse", "Childcare comparison for parenting and family agents with local costs and subsidy programs.", "parentpulse"),
            service(4, "operation.onchainpulse.company", "OnchainPulse", "Narrative-vs-chain consistency check for token due diligence. Fetches the company website, employee metrics and project claims for blockchain wallet activity.", "onchainpulse"),
        ], vec![known("parentpulse", "ParentPulse", "Parenting, family and childcare subsidy guides")]);
        for requested in [
            json!({"slug":"company.parent.structure","name":"Company parent structure","description":"Corporate parent and subsidiary ownership structure"}),
            json!({"slug":"finance.subsidy.search","name":"Subsidy search","description":"Search business subsidy programs"}),
            json!({"slug":"company.employee.count","name":"Company employee metrics","description":"Business company size and number of employees"}),
        ] {
            let mut request=parsed(vec![requested]);
            request.capabilities[0].domain=request.capabilities[0].slug.split('.').next().unwrap().to_owned();
            let found=search(&request,&catalog);
            assert!(found[0].apiosk.iter().all(|candidate| !matches!(candidate.source_slug.as_deref(),Some("parentpulse"|"onchainpulse"))),"{:?}",found[0].apiosk);
        }
        let mut request=parsed(vec![json!({"slug":"finance.childcare.subsidy","name":"Childcare subsidy programs","description":"Childcare cost support for parents"})]);
        request.capabilities[0].domain="finance".into();
        assert!(search(&request,&catalog)[0].apiosk.iter().any(|candidate|candidate.source_slug.as_deref()==Some("parentpulse")),"Requested childcare still has its relevant source");
        let mut request=parsed(vec![json!({"slug":"company.employee.count","name":"Company employee metrics"})]);
        request.capabilities[0].source_hint=Some("onchainpulse".into());
        assert!(search(&request,&catalog)[0].operations.is_empty(),"Naming a provider cannot turn token due diligence into employee metrics");
        let mut request=parsed(vec![json!({"slug":"company.narrative.consistency","name":"Narrative consistency check"})]);
        request.subjects[0].label="Ethereum Foundation".into();
        assert!(search(&request,&catalog)[0].apiosk.iter().any(|candidate|candidate.source_slug.as_deref()==Some("onchainpulse")),"Explicit blockchain subject context is preserved");
        let equity=self::catalog(vec![service(6,"finance.equity.quote","CryptoPulse","Stock commodity FX and index prices; crypto and blockchain provider overview","cryptopulse")],vec![]);
        let request=parsed(vec![json!({"slug":"finance.equity.quote","name":"Equity quote"})]);
        assert_eq!(search(&request,&equity)[0].apiosk.len(),1,"An exact capability survives unrelated provider-wide vocabulary");
    }

    #[test]
    fn primary_topic_preserves_domains_abbreviations_and_short_intro_labels() {
        for (text,expected) in [
            ("ClinicalTrials.gov lookup, deterministic. Answers questions about trials.","ClinicalTrials.gov lookup, deterministic"),
            ("Check Fly.io's official status source. Returns status.","Check Fly.io's official status source"),
            ("U.S. sanctions screening. More details.","U.S. sanctions screening"),
            ("FREE — Base batch-anchor proof for an ISO receipt bundle hash","FREE — Base batch-anchor proof for an ISO receipt bundle hash"),
            ("RECEIPTS — the full token due-diligence report in one call","RECEIPTS — the full token due-diligence report in one call"),
            ("Google Solar API — aerial GeoTIFF data layers and shade maps","Google Solar API — aerial GeoTIFF data layers and shade maps"),
            ("Is it real or fake? Collectible authentication and counterfeit check. Provider overview.","Is it real or fake? Collectible authentication and counterfeit check"),
            ("Is it safe to exercise outside today? Real-time air quality and AQI. Provider overview.","Is it safe to exercise outside today? Real-time air quality and AQI"),
        ] {assert_eq!(opening_description(text),expected);}
        let catalog=catalog(vec![
            service(1,"operation.trial.lookup","Trial service","ClinicalTrials.gov lookup, deterministic. Answers trial recruitment questions.","trial-service"),
            service(2,"operation.fly.status","Status service","Check Fly.io's official status source. Returns uptime.","status-service"),
            service(3,"operation.receipt.proof","Receipts","FREE — Base batch-anchor proof for an ISO receipt bundle hash","receipts"),
            service(4,"operation.collectibles.authenticate","Collectibles","Is it real or fake? Collectible authentication and counterfeit check. Provider overview.","collectibles"),
            service(5,"operation.air.quality","Air","Is it safe to exercise outside today? Real-time air quality and AQI. Provider overview.","air"),
        ],vec![]);
        for (slug,name,source) in [
            ("research.clinical.trials","Clinical trials","trial-service"),
            ("research.service.status","Official service status","status-service"),
            ("research.receipt.proof","Receipt proof","receipts"),
            ("research.collectible.authentication","Collectible authentication","collectibles"),
            ("location.air.quality","Air quality","air"),
        ] {
            let found=search(&parsed(vec![json!({"slug":slug,"name":name})]),&catalog);
            assert!(found[0].apiosk.iter().any(|c|c.source_slug.as_deref()==Some(source)),"{slug}: {:?}",found[0].apiosk);
        }
    }

    #[test]
    fn irrelevant_or_vague_capabilities_do_not_match_general_company_words() {
        let catalog = catalog(
            vec![service(
                1,
                "company.officers",
                "Company officers",
                "Get current company director records",
                "registry",
            )],
            vec![],
        );
        for (slug, name) in [
            ("company.search", "Company data search"),
            ("location.weather.forecast", "Weather forecasts"),
        ] {
            let found = search(&parsed(vec![json!({"slug":slug,"name":name})]), &catalog);
            assert!(found[0].apiosk.is_empty(), "{slug}: {:?}", found[0].apiosk);
        }
    }

    #[test]
    fn recognized_source_hint_restricts_endpoints_even_when_another_is_cheaper() {
        let mut cheap = service(
            1,
            "company.officers",
            "Company officers",
            "Directors",
            "other",
        );
        cheap.list_atomic = 0;
        let catalog = catalog(
            vec![
                cheap,
                service(
                    2,
                    "company.officers",
                    "Company officers",
                    "Directors",
                    "preferred",
                ),
            ],
            vec![known("preferred", "Preferred", "Company directors")],
        );
        let found = search(
            &parsed(vec![
                json!({"slug":"company.officers","name":"Company officers","source_hint":"preferred"}),
            ]),
            &catalog,
        );
        assert_eq!(found[0].apiosk.len(), 1);
        assert_eq!(found[0].apiosk[0].source_slug.as_deref(), Some("preferred"));
    }

    fn person_profile_request() -> ParsedRequest {
        crate::capability_intake::normalize(&json!({"language":"en",
            "subjects":[{"id":"s1","label":"Olivier Brinkman","type":"person","identifiers":{}}],
            "capabilities":[{"slug":"research.person.profile","name":"Person profile",
                "description":"Retrieve a person's professional profile.","domain":"research",
                "inputs":["person_name"],"outputs":["profile"],"subject_id":"s1",
                "source_hint":"linkedin","confidence":0.95}],
            "deliverable":{"format":"chat","operations":[]}}).to_string(),
            "Check the linkedin profile from Olivier Brinkman, Founder of Apiosk", "fixture").unwrap()
    }

    #[test]
    fn person_profiles_keep_the_requested_platform_and_exclude_company_and_game_profiles() {
        // These two descriptions/IDs were returned by the live request on
        // 2026-10-07, which incorrectly asked the person for a French SIREN.
        let mut lobbying = service(1, "company.sirenic-entreprise-siren-lobbying", "Sirenic",
            "Lobbying and influence profile of a French company from the official HATVP register of interest representatives — a transparency, governance and ESG due diligence signal: registration status, category, lobbying-expense brackets per fiscal year, staff count, recent subjects with intervention domains and action types, clients (for consulting firms), affiliations, declaration defaults and deregistrat", "sirenic");
        lobbying.endpoint_id = "d9e0985f-9425-56c3-8d07-27d6e242dce0".parse().unwrap();
        let mut person = service(2, "operation.stableenrich.post-api-minerva-enrich", "StableEnrich",
            "Enrich person records with demographics, work history, education, contact info (emails + phones), address history, financial signals (income/wealth range), relatives, and social profiles.", "stableenrich");
        person.endpoint_id = "1fb6a3e3-0f69-4122-b3f7-cca5808b1dc8".parse().unwrap();
        let linkedin = service(3, "operation.profile-provider.lookup-profile", "Third-party profile API",
            "Look up one person's public LinkedIn profile per call. Returns work history and education.", "profile-provider");
        let company = service(4, "company.profile", "Company profile API",
            "Look up a LinkedIn company profile and its business information.", "company-provider");
        let catalog = catalog(vec![lobbying, person.clone(), linkedin.clone(), company],
            vec![known("tablepulse", "TablePulse",
                "Board-game and tabletop intelligence API. Synthesizes Claude's own board-game knowledge with live Tavily web grounding, the free BoardGamePrices.co.uk pricing API, and the free Gamefound crowdfunding API into structured, actionable intelligence. 11 endpoints covering recommendations, comparisons, expansions, collection valuation, crowdfunding, party/solo/family picks, complexity, pricing, and awards.")]);
        let mut request = person_profile_request();
        let found = search(&request, &catalog);
        assert_eq!(found[0].apiosk.iter().map(|candidate| candidate.source_slug.as_deref()).collect::<Vec<_>>(),
            [Some("profile-provider")], "{:?}", found[0].apiosk);

        request.capabilities[0].source_hint = None;
        let found = search(&request, &catalog);
        let ids: BTreeSet<_> = found[0].apiosk.iter().filter_map(|candidate| candidate.endpoint_id.clone()).collect();
        assert_eq!(ids, BTreeSet::from([person.endpoint_id.to_string(), linkedin.endpoint_id.to_string()]));
        assert!(found[0].apiosk.iter().all(|candidate| candidate.source_slug.as_deref() != Some("tablepulse")));
    }

    #[test]
    fn webpage_discovery_can_mention_profiles_without_requesting_profile_records() {
        let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../tests/fixtures/workflows/catalog.json")).unwrap();
        snapshot.services.retain(|service| service.contract.capability_slug == "web.search");
        snapshot.known_sources.clear();
        assert_eq!(snapshot.services.len(), 1);
        // Canonical production operation wording, distinct from its endpoint's
        // marketing copy. Its typed query and URL/title outputs are unchanged.
        snapshot.services[0].description = Some("Run a query against a web index and return ranked results (title, URL, snippet). Freshness, index coverage and whether page contents come back inline differ per offer.".into());
        let search_id = snapshot.services[0].endpoint_id.to_string();
        snapshot.services.extend([
            service(10101, "operation.profile-provider.lookup-profile", "Public profile API",
                "Look up one person's public LinkedIn profile by known profile URL.", "profile-provider"),
            service(10102, "company.registry-profile", "Company registry",
                "Retrieve a registered company profile by SIREN.", "registry"),
        ]);
        let fixture: Snapshot = serde_json::from_str(include_str!("../../tests/fixtures/integration-catalog.json")).unwrap();
        snapshot.vocabulary.extend(fixture.vocabulary);
        let catalog = Catalog::build(snapshot);
        assert_eq!(catalog.services.len(), 3, "{:?}", catalog.rejected);
        let mut request = person_profile_request();
        request.capabilities[0].slug = "research.web.search".into();
        request.capabilities[0].name = "Professional profile page search".into();
        request.capabilities[0].description = "Search the public web for professional profile pages matching the supplied criteria.".into();
        request.capabilities[0].outputs = vec!["url".into(), "title".into()];
        request.capabilities[0].source_hint = None;
        let found = search(&request, &catalog);
        assert!(found[0].apiosk.iter().any(|candidate| candidate.endpoint_id.as_deref() == Some(&search_id)), "{:?}", found[0].apiosk);
        assert!(found[0].operations.contains("web.search"));
        assert!(found[0].apiosk.iter().all(|candidate| candidate.source_slug.as_deref() != Some("registry")));

        // A request for an actual profile retains its subject gate even when
        // the display name uses the ordinary verb "search".
        let mut record = person_profile_request();
        record.capabilities[0].name = "Person profile search".into();
        record.capabilities[0].source_hint = None;
        let found = search(&record, &catalog);
        assert_eq!(found[0].apiosk.iter().map(|candidate| candidate.source_slug.as_deref()).collect::<Vec<_>>(),
            [Some("profile-provider")], "{:?}", found[0].apiosk);
    }

    #[test]
    fn an_unavailable_named_source_does_not_widen_an_exact_company_profile_match() {
        let catalog = catalog(vec![service(1, "company.profile", "Company registry",
            "Retrieve the official company registry profile.", "registry")], vec![]);
        let mut request = parsed(vec![json!({"slug":"company.profile","name":"Company profile"})]);
        assert_eq!(search(&request, &catalog)[0].apiosk.len(), 1);
        request.capabilities[0].source_hint = Some("missing-register".into());
        assert!(search(&request, &catalog)[0].apiosk.is_empty());
    }

    #[test]
    fn named_platform_cannot_bypass_person_profile_scope_in_bazaar() {
        let request = person_profile_request();
        let directory: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/ask-v2-linkedin-contract.json")).unwrap();
        let mut profile = bazaar(directory["endpoints"][0]["description"].as_str().unwrap());
        profile.url = Some(directory["endpoints"][0]["resource"].as_str().unwrap().into());
        let mut matches = search(&request, &catalog(vec![], vec![]));
        matches[0].coinbase = vec![profile,
            bazaar("Retrieve a LinkedIn company profile, employee count and revenue."),
            bazaar("Board-game profiles and intelligence, promoted on LinkedIn.")];
        filter_coinbase(&request, &mut matches);
        assert_eq!(matches[0].coinbase.len(), 1);
        assert_eq!(matches[0].coinbase[0].url.as_deref(), Some("https://agdata.shveik.dev/x402/linkedin"));
    }

    #[test]
    fn exact_generic_contract_does_not_require_lexical_overlap() {
        let catalog = catalog(vec![service(1, "company.search", "Company search", "", "registry")], vec![]);
        let found = search(&parsed(vec![json!({"slug":"company.search","name":"Company search","description":"Search companies by name"})]), &catalog);
        assert_eq!(found[0].apiosk.len(), 1);
        assert_eq!(found[0].apiosk[0].capability.as_deref(), Some("company.search"));
    }

    #[test]
    fn exact_endpoint_relevance_precedes_price_and_results_are_deduplicated() {
        let mut exact = service(
            1,
            "company.officers",
            "Company officers",
            "Company directors",
            "registry",
        );
        exact.list_atomic = 900;
        let mut broader = service(
            2,
            "company.director-summary",
            "Director summary",
            "Overview of officers and directors",
            "summary",
        );
        broader.list_atomic = 0;
        let catalog = catalog(vec![broader, exact.clone(), exact], vec![]);
        let found = search(
            &parsed(vec![
                json!({"slug":"company.officers","name":"Company officers"}),
            ]),
            &catalog,
        );
        assert_eq!(
            found[0].apiosk[0].capability.as_deref(),
            Some("company.officers")
        );
        assert_eq!(found[0].apiosk.len(), 2);
    }

    #[test]
    fn all_37_source_groups_are_searched_including_metadata_without_contracts() {
        let known_sources = (0..37)
            .map(|index| {
                known(
                    &format!("source-{index}"),
                    &format!("Source {index}"),
                    if index == 36 {
                        "Sanctions screening against current sanctions lists"
                    } else {
                        "Weather forecast"
                    },
                )
            })
            .collect();
        let catalog = catalog(vec![], known_sources);
        assert_eq!(crate::capability_intake::source_count(&catalog), 37);
        let found = search(
            &parsed(vec![
                json!({"slug":"compliance.sanctions","name":"Sanctions screening"}),
            ]),
            &catalog,
        );
        assert_eq!(found[0].apiosk.len(), 1);
        let candidate = &found[0].apiosk[0];
        assert_eq!(candidate.source_slug.as_deref(), Some("source-36"));
        assert_eq!(candidate.availability, "discovery_only");
        assert!(
            candidate.capability.is_none()
                && candidate.endpoint_id.is_none()
                && candidate.url.is_none()
                && candidate.price.is_none()
        );
        assert!(found[0].operations.is_empty());
    }

    #[test]
    fn catalog_version_replacement_invalidates_the_index_without_caching_questions() {
        let first = catalog(
            vec![service(
                1,
                "company.officers",
                "Officers",
                "Company directors",
                "registry",
            )],
            vec![],
        );
        let second = catalog(
            vec![service(
                1,
                "compliance.sanctions",
                "Sanctions",
                "Sanctions screening",
                "registry",
            )],
            vec![],
        );
        let request = parsed(vec![
            json!({"slug":"compliance.sanctions","name":"Sanctions screening"}),
        ]);
        assert!(search(&request, &first)[0].apiosk.is_empty());
        assert_eq!(search(&request, &second)[0].apiosk.len(), 1);
    }

    #[test]
    fn subject_names_cannot_make_an_unrelated_source_relevant() {
        let catalog = catalog(
            vec![],
            vec![known(
                "monzo",
                "Monzo Bank Limited",
                "Current company information",
            )],
        );
        let request = parsed(vec![
            json!({"slug":"company.search","name":"Information for Monzo Bank Limited",
            "description":"Retrieve company data for Monzo Bank Limited."}),
        ]);
        let found = search(&request, &catalog);
        assert!(!found[0].query.contains("Monzo"));
        assert!(found[0].apiosk.is_empty());
    }

    #[test]
    fn at_most_three_distinct_endpoints_are_returned_for_each_capability() {
        let catalog = catalog(
            (1..=8)
                .map(|id| {
                    service(
                        id,
                        "company.officers",
                        "Company officers",
                        "Current directors",
                        &format!("source-{id}"),
                    )
                })
                .collect(),
            vec![],
        );
        let found = search(
            &parsed(vec![
                json!({"slug":"company.officers","name":"Company officers"}),
            ]),
            &catalog,
        );
        assert_eq!(found[0].apiosk.len(), 3);
        assert_eq!(
            found[0]
                .apiosk
                .iter()
                .filter_map(|candidate| candidate.endpoint_id.as_ref())
                .collect::<BTreeSet<_>>()
                .len(),
            3
        );
    }

    #[test]
    fn unsafe_endpoint_urls_are_not_displayed_and_search_never_adds_execution_urls() {
        for value in [
            "javascript:alert(1)",
            "http://example.test/source",
            "https://user:secret@example.test/source",
        ] {
            let mut row = service(
                1,
                "company.officers",
                "Officers",
                "Company directors",
                "registry",
            );
            row.source_url = Some(value.into());
            let catalog = catalog(vec![row], vec![]);
            let found = search(
                &parsed(vec![
                    json!({"slug":"company.officers","name":"Company officers"}),
                ]),
                &catalog,
            );
            assert!(found[0].apiosk[0].url.is_none());
        }
    }

    #[test]
    #[ignore = "optional local catalog snapshot precision check; reads only a local file and makes no network/model calls"]
    fn local_catalog_precision() {
        let path = std::env::var("ASK_V2_CATALOG_SNAPSHOT").unwrap();
        let catalog =
            Catalog::build(serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap());
        for (name, slug, title, outputs, hint, expected) in [
            (
                "Monzo officers and significant controllers live parse",
                "company.officers.controllers",
                "Company officers and significant controllers",
                vec![
                    "officers",
                    "significant_controllers",
                    "appointment_status",
                    "resignation_status",
                ],
                Some("companies_house"),
                vec!["company.officers", "company.significant-control"],
            ),
            (
                "Monzo officers and PSC",
                "company.profile.officers",
                "Company officers",
                vec![
                    "appointment_date",
                    "resignation_date",
                    "significant_control",
                ],
                Some("companies house"),
                vec!["company.officers", "company.significant-control"],
            ),
            (
                "Coolblue registered address and KVK",
                "company.profile.registered_address",
                "Registered company address and KVK number",
                vec!["registered_address", "kvk_number"],
                Some("kvk"),
                vec!["company.registry-profile"],
            ),
            (
                "Financial statements",
                "company.financial.statements",
                "Company financial statements",
                vec!["financial_statements"],
                None,
                vec!["company.financial-statements"],
            ),
            (
                "Sanctions",
                "compliance.sanctions",
                "Company sanctions screening",
                vec!["matching_records", "match_scores", "source_links"],
                None,
                vec!["company.sanctions-screen"],
            ),
        ] {
            let mut request = parsed(vec![json!({"slug":slug,"name":title})]);
            request.capabilities[0].outputs = outputs.into_iter().map(str::to_owned).collect();
            request.capabilities[0].source_hint = hint.map(str::to_owned);
            let found = search(&request, &catalog);
            println!(
                "{name}: {:?}",
                found[0]
                    .apiosk
                    .iter()
                    .map(|candidate| (
                        &candidate.source_slug,
                        &candidate.capability,
                        &candidate.availability
                    ))
                    .collect::<Vec<_>>()
            );
            for capability in expected {
                assert!(
                    found[0].operations.contains(capability),
                    "{name}: expected {capability}"
                );
            }
        }
    }
    #[test]
    fn requested_operation_is_not_replaced_by_incidental_context_or_output_words() {
        let catalog = catalog(vec![
            service(810, "crypto.spot-price", "Crypto price", "Current market price of one crypto asset in one quote currency.", "crypto"),
            service(811, "company.officers", "Officers", "Retrieve company officers and directors with appointments and roles.", "registry"),
            service(812, "operation.collect.invest", "Collectible signals", "Collectible investment signal: buy, hold or sell? Returns trend analysis for cards, coins and watches.", "collectible"),
            service(813, "company.sector-benchmarks", "Sector benchmark", "Sector benchmarks: median revenue, EBITDA margin and debt ratio across peers.", "sector"),
            service(814, "operation.equity.quote", "Equity quote", "Stock quote: current share price for one listed equity.", "equity"),
            service(815, "company.insider-transactions", "Insider transactions", "Company insider transactions by officers and directors: shares bought or sold and transaction dates.", "transactions"),
        ], vec![]);
        for (slug, name, description, outputs, rejected, accepted) in [
            ("company.stock.quote", "Stock quote", "Current stock price and trading data for company shares.", vec!["current_price","quote_currency"], "crypto", Some("equity")),
            ("company.insider.transactions", "Insider transactions", "Transactions by executives and directors.", vec!["insider_name","title","transaction_date"], "registry", Some("transactions")),
            ("company.analyst.consensus", "Analyst consensus", "Analyst stock recommendations and ratings.", vec!["buy_recommendations","hold_recommendations","sell_recommendations"], "collectible", None),
            ("company.financial.valuation.ratios", "Company valuation ratios", "Company EV/EBITDA and P/E valuation ratios.", vec!["ev_ebitda","revenue","debt_ratio"], "sector", None),
        ] {
            let mut request = parsed(vec![json!({"slug":slug,"name":name,"description":description})]);
            request.subjects.clear();
            request.capabilities[0].outputs=outputs.into_iter().map(str::to_owned).collect();
            let found=search(&request,&catalog);
            assert!(found[0].apiosk.iter().all(|c|c.source_slug.as_deref()!=Some(rejected)),"{slug}: {:?}",found[0].apiosk);
            if let Some(accepted)=accepted {assert!(found[0].apiosk.iter().any(|c|c.source_slug.as_deref()==Some(accepted)),"{slug}: {:?}",found[0].apiosk);}
        }
        for (slug,name,source) in [
            ("crypto.spot.price","Crypto spot price","crypto"),
            ("company.officers","Company officers","registry"),
            ("research.collectible.investment","Collectible investment signal","collectible"),
            ("company.sector.benchmarks","Sector benchmarks","sector"),
        ] {
            let mut request=parsed(vec![json!({"slug":slug,"name":name})]); request.subjects.clear();
            assert!(search(&request,&catalog)[0].apiosk.iter().any(|c|c.source_slug.as_deref()==Some(source)),"Requested {slug} remains discoverable");
        }
    }

    #[test]
    fn country_conflicts_are_excluded_even_when_they_are_the_only_or_exact_operation() {
        let mut french=service(830,"company.registry-profile","French registry","Company registration profile from the French registry.","registry-fr"); french.jurisdiction=Some("FR".into());
        let mut british=service(831,"company.registry-profile","British registry","Company registration profile from the British registry.","registry-gb"); british.jurisdiction=Some("GB".into());
        let foreign=catalog(vec![french.clone()],vec![]);
        let request=parsed(vec![json!({"slug":"company.registry.profile","name":"Company registry profile"})]);
        assert!(search(&request,&foreign)[0].operations.is_empty());
        let mut inferred = request.clone();
        inferred.language = "nl".into();
        inferred.capabilities[0].inputs = vec!["kvk_number".into(), "rsin".into(), "kvknummer".into()];
        let mut dutch = service(832,"company.registry-profile","Dutch registry","Company registration profile from the Dutch registry.","registry-nl"); dutch.jurisdiction=Some("NL".into());
        assert!(search(&inferred, &catalog(vec![dutch], vec![]))[0].operations.is_empty(), "Requested concepts are not supplied identifiers");
        let both=catalog(vec![french,british],vec![]);
        assert_eq!(search(&request,&both)[0].apiosk.iter().filter_map(|c|c.source_slug.as_deref()).collect::<Vec<_>>(),vec!["registry-gb"]);
    }

    #[test]
    fn stock_price_is_not_a_perpetual_mark_positioning_or_filings_search() {
        let catalog=catalog(vec![
            service(890,"operation.market.equity-marks","Equity marks","Stock, commodity, FX and index prices 24/7. Live marks from Hyperliquid HIP-3 perp markets: mark price, funding and open interest.","derivatives"),
            service(891,"operation.market.equity-positioning","Equity positioning","Positioning and sentiment on stock perps — which side of TSLA, NVDA or gold is crowded right now, 24/7.","positioning"),
            service(892,"operation.market.filings-exchange","Exchange filings","International filing search by stock exchange. Regulatory filing intelligence using correct local terms for global investing.","filings"),
            service(893,"operation.market.stock-quote","Stock quote","Current stock price and currency from the stock exchange for a listed company.","exchange"),
        ],vec![]);
        let mut request=parsed(vec![json!({"slug":"company.stock.price","name":"Company stock price","description":"Returns the current stock price of a company."})]);
        request.capabilities[0].outputs=vec!["stock_price".into(),"currency".into()];
        assert_eq!(search(&request,&catalog)[0].apiosk.iter().filter_map(|c|c.source_slug.as_deref()).collect::<Vec<_>>(),vec!["exchange"]);
        request.capabilities[0].name="Stock perpetual mark price".into();
        request.capabilities[0].description="Stock perpetual mark price, funding and open interest.".into();
        request.capabilities[0].outputs=vec!["mark_price".into(),"funding".into(),"open_interest".into()];
        assert!(search(&request,&catalog)[0].apiosk.iter().any(|c|c.source_slug.as_deref()==Some("derivatives")),"explicit derivative-price requests keep derivative sources");
    }

    #[test]
    fn stock_quotes_ignore_provider_branding_inputs_and_excluded_derivatives() {
        let mut cash = service(894,"operation.market.stock-cash-quote","Stocks and Derivatives",
            "Current stock prices, excluding derivatives, from the stock exchange.","cash");
        cash.contract.input_map.values_mut().next().unwrap().name="exclude_derivatives".into();
        let catalog=catalog(vec![cash,
            service(895,"operation.market.stock-perp-quote","Markets",
                "Stock prices. These are perpetual derivative marks, with funding and open interest.","perps")],vec![]);
        let mut request=parsed(vec![json!({"slug":"company.stock.price","name":"Stock price, not derivatives",
            "description":"Current company stock quote, excluding derivatives."})]);
        request.capabilities[0].outputs=vec!["stock_price".into(),"currency".into()];
        assert_eq!(search(&request,&catalog)[0].apiosk.iter().filter_map(|c|c.source_slug.as_deref()).collect::<Vec<_>>(),vec!["cash"]);
    }

    #[test]
    fn requested_labor_metrics_do_not_match_generic_benchmarks_forecasts_or_rates() {
        let fixtures=catalog(vec![
            service(896,"company.sector-benchmarks","Corporate benchmarks","Sector benchmarks and sector statistics for a French NAF activity code: company counts, median company age, workforce-bracket distribution, revenue, EBITDA margin and debt ratio.","business"),
            service(897,"finance_markets.forecast","Market forecast","Financial markets forecast: commodity supply and demand, futures curves and market forecasts.","commodity"),
            service(898,"fx.ecb-reference-rate","ECB rate","Latest daily ECB euro foreign-exchange reference rate for one target currency.","fx"),
            service(899,"statistics.salary-benchmarks","Salary source","Salary benchmarks for software developers: average salary, median salary and salary range by location.","salary"),
            service(900,"statistics.labour-tightness","Labor source","Labor market scarcity index: labor shortages and the demand supply ratio for a job category and location.","labor"),
            service(901,"statistics.unemployment-country","Unemployment source","Official unemployment rate by country and reference period.","unemployment"),
        ],vec![]);
        for (slug,name,description,outputs,source) in [
            ("research.salary.benchmarks","Salary benchmarks for software developers","Provides salary benchmarks for software developers in a specific location.",vec!["average_salary","salary_range","median_salary"],"salary"),
            ("research.labor.market.scarcity","Labor market scarcity index","Measures the scarcity of labor in a specific job category and location.",vec!["scarcity_index","demand_supply_ratio"],"labor"),
            ("research.unemployment.rate","Official unemployment rate","Provides the official unemployment rate for a country or region.",vec!["unemployment_rate","reference_period"],"unemployment"),
        ] {
            let mut request=parsed(vec![json!({"slug":slug,"name":name,"description":description})]);
            request.subjects.clear();request.capabilities[0].subject_id=None;
            request.capabilities[0].domain="research".into();
            request.capabilities[0].outputs=outputs.into_iter().map(str::to_owned).collect();
            let found=search(&request,&fixtures);
            assert_eq!(found[0].apiosk.iter().filter_map(|c|c.source_slug.as_deref()).collect::<Vec<_>>(),vec![source],"{name}");
        }
        for (slug,name,source) in [("company.sector.benchmarks","Sector benchmarks","business"),
            ("finance.markets.forecast","Financial markets forecast","commodity"),
            ("fx.ecb.reference.rate","ECB foreign exchange reference rate","fx")] {
            let request=parsed(vec![json!({"slug":slug,"name":name})]);
            assert!(search(&request,&fixtures)[0].apiosk.iter().any(|c|c.source_slug.as_deref()==Some(source)),"{name} remains available when requested");
        }
    }

    #[test]
    fn sector_market_size_is_not_national_gdp_at_market_prices() {
        let fixtures=catalog(vec![
            service(902,"statistics.eurostat-gdp-germany","GDP source","GDP at market prices in current-price million euro for Germany. Requires the explicit year. Not real GDP, growth or GDP per capita.","gdp"),
            service(903,"statistics.industry-market-size","Industry source","Market size and growth by industry sector: turnover, revenue and growth rates by country and year.","industry"),
        ],vec![]);
        let mut request=parsed(vec![json!({"slug":"research.comparison","name":"Market size and growth comparison","description":"Compares market size and growth between two sectors based on provided data sources."})]);
        request.subjects.clear();request.capabilities[0].subject_id=None;
        request.capabilities[0].inputs=vec!["country_1".into(),"country_2".into(),"sector".into(),"data_source".into()];
        request.capabilities[0].outputs=vec!["market_size_country_1".into(),"market_size_country_2".into(),"growth_rate_country_1".into(),"growth_rate_country_2".into(),"source".into()];
        assert_eq!(search(&request,&fixtures)[0].apiosk.iter().filter_map(|c|c.source_slug.as_deref()).collect::<Vec<_>>(),vec!["industry"]);
        let requested_gdp=parsed(vec![json!({"slug":"statistics.gdp.market.prices","name":"GDP at market prices","description":"GDP at market prices in Germany."})]);
        assert!(search(&requested_gdp,&fixtures)[0].apiosk.iter().any(|c|c.source_slug.as_deref()==Some("gdp")));
    }

    #[test]
    fn provenance_comparison_keeps_subject_context_without_buying_an_analysis_api() {
        let catalog=catalog(vec![service(879,"operation.analysis.comparison","Analysis", "Compare official sources and AI generated analysis with source confidence and discrepancies.","analysis")], vec![]);
        let mut request=parsed(vec![json!({"slug":"research.analysis.comparison","name":"Analysis comparison","description":"Distinguishes between official sources and AI-generated analyses for a given subject."})]);
        let cap=&mut request.capabilities[0];
        cap.domain="research".into();
        cap.inputs=vec!["subject_id".into(),"analysis_sources".into()];
        cap.outputs=vec!["official_sources".into(),"ai_generated_analysis".into(),"source_confidence".into(),"discrepancies".into()];
        let found=search(&request,&catalog);
        assert_eq!(found.len(),1);
        assert!(found[0].operations.is_empty() && found[0].apiosk.is_empty());
        assert_eq!(request.capabilities[0].subject_id.as_deref(),Some("s1"));
        request.capabilities[0].outputs.push("new_market_statistics".into());
        assert!(!crate::capability_intake::answer_instruction(&request.capabilities[0]));
    }

    #[test]
    fn company_multiples_require_corporate_asset_scope_not_any_valuation() {
        let fixtures = catalog(vec![
            service(880,"operation.cards.portfolio","Cards", "Trading card portfolio valuation for card-collector and card-investor agents. Returns current market value across Pokemon, MTG, sports cards, and more.","cards"),
            service(881,"operation.property.valuate","Property", "Property valuation and home value estimate (AVM): estimated value with confidence range, price per square foot/meter, comparable sales, days-to-sell estimate, and negotiation intelligence for any address worldwide.","property"),
            service(882,"operation.scent.value","Fragrance", "Fragrance collection valuation — realistic resale/collector value estimate for one or more bottles.","scent"),
            service(883,"operation.company.multiples","Company multiples", "Company valuation multiples: enterprise value, EV/Revenue and EV/EBITDA ratios for a listed equity.","financial"),
        ], vec![]);
        let mut request=parsed(vec![json!({"slug":"research.valuation.metrics","name":"Valuation multiples","description":"Calculates or retrieves enterprise value to revenue and enterprise value to EBITDA ratios for a company."})]);
        request.capabilities[0].domain="research".into();
        request.capabilities[0].outputs=vec!["enterprise_value".into(),"revenue".into(),"ebitda".into(),"ev_to_revenue".into(),"ev_to_ebitda".into()];
        let found=search(&request,&fixtures);
        assert_eq!(found[0].apiosk.iter().filter_map(|c|c.source_slug.as_deref()).collect::<Vec<_>>(),vec!["financial"]);
        for (name,slug,source) in [("Property valuation","property.valuation","property"),("Trading card portfolio valuation","collectible.portfolio.valuation","cards"),("Fragrance collection valuation","fragrance.collection.valuation","scent")] {
            request.subjects.clear(); request.capabilities[0].subject_id=None;
            request.capabilities[0].name=name.into();request.capabilities[0].slug=slug.into();request.capabilities[0].domain="research".into();
            request.capabilities[0].description=name.into();request.capabilities[0].outputs=vec!["valuation".into()];
            assert!(search(&request,&fixtures)[0].apiosk.iter().any(|c|c.source_slug.as_deref()==Some(source)),"{name} remains a valid requested asset");
        }
    }

    #[test]
    fn explicit_enterprise_ratio_outputs_survive_generic_financial_labels() {
        let fixtures=catalog(vec![
            service(884,"company.financial-statements","Filed accounts","Company annual accounts: filed revenue, EBITDA, assets and debts, with reporting year.","accounts"),
            service(885,"operation.company.metrics","Corporate metrics","Company key financial figures: enterprise value, EV/Revenue and EV/EBITDA ratios for listed companies.","metrics"),
        ],vec![]);
        let mut request=parsed(vec![json!({"slug":"company.financial.keyFigures","name":"Company key financial figures","description":"Retrieve key financial metrics for a company.","outputs":["ev_revenue","ev_ebitda","currency","reporting_period"]})]);
        request.capabilities[0].domain="company".into();
        request.capabilities[0].outputs=vec!["ev_revenue".into(),"ev_ebitda".into(),"currency".into(),"reporting_period".into()];
        for slug in ["company.financial.keyFigures","company.financial-statements"] {
            request.capabilities[0].slug=slug.into();
            let found=search(&request,&fixtures);
            if slug=="company.financial.keyFigures" {
                assert!(found[0].apiosk.iter().any(|c|c.source_slug.as_deref()==Some("metrics")),"{slug}: {:?}",found[0].apiosk);
            }
            assert!(!found[0].apiosk.iter().any(|c|c.source_slug.as_deref()==Some("accounts")),"accounts do not establish enterprise ratios, even with an exact financial slug");
        }
        request.capabilities[0].name="Company annual accounts".into();
        request.capabilities[0].description="Retrieve a company's filed annual accounts.".into();
        request.capabilities[0].outputs=vec!["revenue".into(),"ebitda".into(),"reporting_year".into()];
        assert!(search(&request,&fixtures)[0].apiosk.iter().any(|c|c.source_slug.as_deref()==Some("accounts")),"ordinary annual accounts remain available");
        request.capabilities[0].description="Financial statements for EV charging revenue from electric vehicle operations.".into();
        assert!(search(&request,&fixtures)[0].apiosk.iter().any(|c|c.source_slug.as_deref()==Some("accounts")),"EV charging revenue is not enterprise-value ratio notation");
    }

    #[test]
    fn regulatory_wording_finds_official_legislation_metadata() {
        let catalog=catalog(vec![
            service(850,"government.bwb.search","KOOP","Search BWB regulation titles; national legislation metadata, not full text.","koop"),
            service(851,"eu_law.sparql.select","EUR-Lex","Query EU legal metadata with a bounded SPARQL SELECT; not the full legal text.","eurlex-cellar"),
            service(852,"operation.tax.calculate","Tax calculator","Calculate corporate income tax for a revenue estimate.","tax"),
        ], vec![]);
        let mut request=parsed(vec![json!({"slug":"research.regulatory.csrd","name":"CSRD regulations and related rulings","description":"Current regulatory information, legal rulings and parliamentary motions in the Netherlands and European Union."})]);
        request.subjects.clear();
        request.capabilities[0].outputs=vec!["regulations".into(),"legal_rulings".into(),"parliamentary_motions".into()];
        let found=search(&request,&catalog);
        assert!(found[0].apiosk.iter().any(|c|c.source_slug.as_deref()==Some("koop")),"{:?}",found[0].apiosk);
        assert!(found[0].apiosk.iter().any(|c|c.source_slug.as_deref()==Some("eurlex-cellar")),"{:?}",found[0].apiosk);
        assert!(found[0].apiosk.iter().all(|c|c.source_slug.as_deref()!=Some("tax")));
    }

    #[test]
    fn compound_stock_names_keep_the_narrower_requested_operation() {
        let catalog=catalog(vec![
            service(870,"finance.stock-quote","Stock quote","Stock quote with current trading price and volume.","prices"),
            service(871,"finance.analyst-consensus","Consensus","Analyst consensus ratings, recommendations and target prices for listed stocks.","analysts"),
            service(872,"finance.insider-transactions","Insiders","Company insider transactions: shares bought or sold by directors.","insiders"),
        ], vec![]);
        for (slug,name,source) in [
            ("company.stock.analyst.consensus","Stock analyst consensus","analysts"),
            ("company.stock.insider.transactions","Stock insider transactions","insiders"),
        ] {
            let mut request=parsed(vec![json!({"slug":slug,"name":name})]);request.subjects.clear();
            let found=search(&request,&catalog);
            assert!(found[0].apiosk.iter().all(|c|c.source_slug.as_deref()!=Some("prices")),"{name}: {:?}",found[0].apiosk);
            assert!(found[0].apiosk.iter().any(|c|c.source_slug.as_deref()==Some(source)),"{name}: {:?}",found[0].apiosk);
        }
    }

    #[test]
    fn reviewed_capability_synonyms_work_without_marketing_descriptions() {
        let mut accounts=service(880,"company.accounts","company.accounts","","registry");accounts.description=None;
        let mut officers=service(881,"company.officers","company.officers","","registry");officers.description=None;
        let catalog=catalog(vec![accounts,officers],vec![]);
        for (slug,name,expected) in [("company.financial.statements","Company financial statements","company.accounts"),("company.executives","Company executives","company.officers")] {
            let mut request=parsed(vec![json!({"slug":slug,"name":name})]);request.subjects.clear();
            assert!(search(&request,&catalog)[0].operations.contains(expected),"{slug}");
        }
    }

    #[test]
    fn foreign_media_namespace_cannot_erase_adverse_qualifier() {
        let catalog=catalog(vec![
            service(890,"media_sports.lookup","Media lookup","Find movie, music and sports media metadata.","entertainment"),
            service(891,"company.adverse-media","Adverse media","Search adverse media reports mentioning a company.","screening"),
        ],vec![]);
        let mut request=parsed(vec![json!({"slug":"company.adverse.media","name":"Adverse media","description":"Find negative news concerning a company."})]);request.subjects.clear();
        let found=search(&request,&catalog);
        assert_eq!(found[0].apiosk.iter().filter_map(|c|c.source_slug.as_deref()).collect::<Vec<_>>(),vec!["screening"]);
    }

    #[test]
    fn generated_operations_need_compound_scope_not_one_incidental_word() {
        let catalog=catalog(vec![
            service(9010,"operation.rental.market","Rental","Global rental-market intelligence: asking rent level, vacancy tightness and budget rates.","rental"),
            service(9011,"operation.emergency.fund","Emergency","Emergency-fund calculator by income, expenses and job stability.","emergency"),
            service(9012,"operation.advisor.find","Advisor","Where do I find an advisor? Advisor finder via a financial planning network, matched by location.","advisor"),
            service(9013,"operation.credit.cards","Cards","Which credit card works? Rewards card finder ranked by net annual value for travel, groceries and gas.","cards"),
            service(9014,"operation.veteran.loan","Veteran","VA home loan guide (US) for veteran agents, plus the closest analogues elsewhere: Australia's interest-rate subsidy.","veteran"),
            service(9015,"operation.jobs.vacancies","Vacancies","Job vacancy counts and volume by location and occupation.","jobs"),
            service(9016,"operation.company.network","Network","Company network reconstruction from ownership relationships.","network"),
            service(9017,"operation.grid.capacity","Grid","Electricity network capacity available at an address.","grid"),
            service(9018,"operation.business.subsidy","Subsidies","Business subsidy search with subsidy budget, eligibility and deadlines.","subsidies"),
            service(9019,"operation.unemployment","Statistics","Unemployment; key figures per month, seasonally adjusted.","statistics"),
        ],vec![]);
        assert_eq!(opening_description("Company profile, plus directors and significant controllers."), "Company profile, plus directors and significant controllers.");
        assert_eq!(opening_description("Vacancy counts, plus salary benchmarks."), "Vacancy counts, plus salary benchmarks.");
        for (slug,name,source) in [
            ("research.job.vacancy.volume","Job vacancy volume","jobs"),
            ("company.network.reconstruction","Reconstruct company network","network"),
            ("location.net.capacity","Network capacity","grid"),
            ("finance.subsidies.find","Find subsidies","subsidies"),
            ("research.statistics.unemployment","Unemployment rate","statistics"),
        ] {
            let mut request=parsed(vec![json!({"slug":slug,"name":name})]);request.subjects.clear();
            if source == "subsidies" { request.capabilities[0].outputs = vec!["budget".into(), "eligibility".into(), "deadline".into()]; }
            if source == "statistics" { request.capabilities[0].outputs = vec!["unemployment_rate".into(), "period".into()]; }
            let found=search(&request,&catalog);
            assert_eq!(found[0].apiosk.iter().filter_map(|c|c.source_slug.as_deref()).collect::<Vec<_>>(),vec![source],"{name}: {:?}",found[0].apiosk);
        }
        for (slug,name,source) in [
            ("finance.rental.market","Rental market","rental"),
            ("finance.emergency.fund","Emergency fund","emergency"),
            ("finance.advisor.find","Financial advisor","advisor"),
            ("finance.credit.card","Credit card","cards"),
            ("finance.veteran.home.loan","Veteran home loan","veteran"),
        ] {
            let mut request=parsed(vec![json!({"slug":slug,"name":name})]);request.subjects.clear();
            assert!(search(&request,&catalog)[0].apiosk.iter().any(|c|c.source_slug.as_deref()==Some(source)),"Requested {name} remains available");
        }
    }

}
