//! Capability-only directory search. This index contains catalog metadata, never
//! questions or subjects. Matches describe discovery; they do not create plans.
use crate::{
    capability_intake::{
        capability_query, normalize_slug, Candidate, Capability, Match, ParsedRequest,
    },
    catalog::{Catalog, KnownSource, Service},
    state::Task,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, OnceLock},
};

#[cfg(test)]
mod calibration;
mod country;
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
const LOCATION_FACETS: u16 = BUILDING | NEIGHBOURHOOD | BOUNDARY | VIOLATIONS | PERMITS;
const FACETS: &[(u16, &str)] = &[
    (FINANCIAL, "@financial"), (OFFICERS, "@officers"),
    (CONTROL, "@control"), (SANCTIONS, "@sanctions"),
    (BUILDING, "@building"), (NEIGHBOURHOOD, "@neighbourhood"),
    (BOUNDARY, "@boundary"), (VIOLATIONS, "@violations"),
    (PERMITS, "@permits"),
];

#[derive(Clone, Copy)]
enum Target {
    Endpoint(usize),
    Source(usize),
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
    /// Where the candidate can answer, for the country signal.
    coverage: country::Coverage,
}
struct Index {
    documents: Vec<Document>,
    frequency: BTreeMap<String, usize>,
}

/// A catalog version and the state of its English renderings.
type IndexKey = (String, u64, u64);

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
    for (position, source) in catalog.known_sources.iter().enumerate() {
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
    tokens(text)
        .iter()
        .filter_map(|word| {
            match DUTCH_STATISTICS.iter().find(|(dutch, _)| dutch == word) {
                Some((_, english)) => Some((*english).to_owned()),
                None => singular(word),
            }
        })
        .collect()
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
        .any(|word| has(word));
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
    let facets = u16::from(financial) * FINANCIAL
        | u16::from(officers) * OFFICERS
        | u16::from(control) * CONTROL
        | u16::from(sanctions) * SANCTIONS
        | location;
    tokens.retain(|token| !vague(token));
    for &(facet, marker) in FACETS {
        if facets & facet != 0 { tokens.insert(marker.into()); }
    }
    Document {
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
            let text = listing_text(candidate);
            let english = rendered(&text);
            let both = format!("{text} {}", english.as_deref().unwrap_or(""));
            if signal.adjustment(&country::listing(&both)).demoted {
                return false;
            }
            if english.is_none() && crate::english::detect(&text) == crate::english::Language::Other {
                return true;
            }
            let listing = document(Target::Source(0), &both, BTreeSet::new());
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
            let translated = translated(english, position, capability);
            let wanted = wanted(capability, parsed).union(wanted(translated, english));
            let restricted = hinted_sources(capability, catalog);
            let floor = relevance::Request::new(capability, parsed)
                .union(relevance::Request::new(translated, english));
            let signal = country::Signal::infer(parsed, capability);
            let mut endpoints = Vec::new();
            let mut sources = Vec::new();
            for candidate in &index.documents {
                let (slug, position) = match candidate.target {
                    Target::Endpoint(position) => (&catalog.services[position].api_slug, position),
                    Target::Source(position) => (&catalog.known_sources[position].slug, position),
                };
                if restricted
                    .as_ref()
                    .is_some_and(|slugs| !slugs.contains(slug))
                {
                    continue;
                }
                let score = score(&wanted, candidate, &index);
                if score <= 0.0 || !floor.passes(&wanted, candidate, &index, restricted.is_some()) {
                    continue;
                }
                let adjustment = signal.adjustment(&candidate.coverage);
                let (demoted, score) = (adjustment.demoted, score * adjustment.factor);
                match candidate.target {
                    Target::Endpoint(_) => endpoints.push(Ranked {
                        demoted,
                        score,
                        item: &catalog.services[position],
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
            let mut seen_endpoints = BTreeSet::new();
            let mut apiosk: Vec<_> = endpoints
                .iter()
                .filter(|ranked| seen_endpoints.insert(ranked.item.endpoint_id))
                .take(3)
                .map(|ranked| endpoint(ranked.item))
                .collect();
            let mut seen_sources: BTreeSet<_> = apiosk
                .iter()
                .filter_map(|candidate| candidate.source_slug.clone())
                .collect();
            for candidate in sources.iter().map(|ranked| ranked.item) {
                if apiosk.len() == 3 {
                    break;
                }
                if seen_sources.insert(candidate.slug.clone()) {
                    apiosk.push(source(candidate));
                }
            }
            let operations = apiosk
                .iter()
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
        let violations = search(&parsed(vec![json!({"slug":"location.building.violations","name":"Building violations"})]), &catalog);
        assert_eq!(violations[0].apiosk[0].capability.as_deref(), Some("building.violations"));
        let boundaries = search(&parsed(vec![json!({"slug":"location.area.boundaries","name":"Neighbourhood boundaries"})]), &catalog);
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
}
