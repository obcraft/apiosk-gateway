//! The relevance floor: a candidate is offered only when it matches what the
//! capability asks for, not merely a word of its context.
//!
//! Lexical scoring ranks every document sharing one token with the request, so
//! "Current local time for a city" used to surface an IP geolocation lookup
//! (it mentions `city` and returns a `timezone`) and Pulse guides about "local
//! resources". The floor separates the request into its topic (slug and name:
//! what is asked), its outputs (what should come back) and its context
//! (description and inputs: what the person supplies). A candidate must
//!
//! 1. be anchored: share a key word of the request (the words of its
//!    capability id and the head noun of its name: "tender" in "Public
//!    tenders", "time" in "Current local time"; when the name is headed by an
//!    action, "Sector comparison", also the words of its description), a
//!    concept facet (officers, control, financial statements, sanctions) or
//!    two output words (both specific unless the candidate is a reviewed
//!    contract of the requested domain);
//! 2. carry enough evidence: one point per matched topic or output word, half
//!    for a word so common in the catalog that it proves little (`time`,
//!    `local`, `address`), one for a shared facet and half for the requested
//!    action (validate, verify, screen; not an operation Apiosk performs on
//!    the data itself, such as compare or analyze);
//! 3. reach 1 point when its reviewed capability id shares a word with the
//!    requested one (`vat.eu.validate` for `finance.vat.validate`,
//!    `company.registry-profile` for `company.profile`) or it belongs to the
//!    source the person named, and 1.5 points otherwise.
//!
//! An exact capability match always passes. When nothing passes, the
//! capability has no candidates, which the Ask page and MCP already present
//! as "no sources".
use super::{document, vague, words, Document, Index, Target, DELIVERABLE_OPERATIONS, FACET_WORDS, SENTIMENT, SPECIFIC_OPERATIONS, STOCK_MARKET, VALUATION};
use crate::{
    capability_intake::{capability_query, normalize_slug, Capability, ParsedRequest},
    state::Task,
};
use std::collections::BTreeSet;

/// Verbs that say what to do with the data, by stem. They support a match on
/// the data's own words but never anchor one: "IBAN validation" is not
/// satisfied by a VAT validation.
const ACTIONS: &[&str] = &[
    "valid", "verif", "screen", "monitor", "compar", "analy", "calcul", "convert",
];

fn action(word: &str) -> Option<&'static str> {
    ACTIONS.iter().copied().find(|stem| word.starts_with(stem))
}

pub(super) fn person_scope(text: &str) -> bool {
    // Lowercasing first preserves branded words such as LinkedIn instead
    // of treating their capitalization as a camelCase field boundary.
    let text = text.to_lowercase();
    let scope = words(&text);
    let person = ["person", "people", "personal", "professional", "individual"].iter()
        .any(|word| scope.contains(*word));
    let company = ["company", "organisation", "organization", "business"].iter()
        .any(|word| scope.contains(*word));
    let linkedin_profile = super::tokens(&text).windows(2)
        .any(|pair| pair[0] == "linkedin" && matches!(pair[1].as_str(), "profile" | "profiles"));
    person || (linkedin_profile && !company)
}

fn individual_profile_identity(scope: &BTreeSet<String>) -> bool {
    scope.contains("profile")
        && ["person", "people", "personal", "individual", "professional", "employee", "officer", "executive", "founder"]
            .iter().any(|word| scope.contains(*word))
}

/// The meaningful words of a reviewed capability id (`vat` and `eu` for
/// `vat.eu.validate`, `registry` and `profile` for `company.registry-profile`).
/// Generated `operation.*` ids name an HTTP route, not a capability.
pub(super) fn contract_words(slug: &str) -> BTreeSet<String> {
    if slug.starts_with("operation.") {
        return BTreeSet::new();
    }
    words(slug)
        .into_iter()
        .filter(|word| !vague(word) && action(word).is_none())
        .collect()
}

/// A word in at least this share of the catalog's documents (and at least
/// `GENERIC_MIN_DOCUMENTS` of them) counts half.
const GENERIC_SHARE: usize = 40;
const GENERIC_MIN_DOCUMENTS: usize = 10;
const FAMILY_EVIDENCE: f64 = 1.0;
const OTHER_EVIDENCE: f64 = 1.5;
/// Two specific output words (or three common ones) anchor a candidate from
/// outside the requested domain.
const OUTPUT_ANCHOR: f64 = 2.0;

/// The request side of the floor, scrubbed of subject names and identifiers
/// exactly as the lexical query is.
pub(super) struct Request {
    /// The words that say what is asked: the capability id's words and the
    /// head noun of its name ("tender" in "Public tenders"). Only these, a
    /// shared facet or two specific output words anchor a match.
    key: BTreeSet<String>,
    /// Key words plus the rest of the name ("public", "local", "filed").
    topic: BTreeSet<String>,
    outputs: BTreeSet<String>,
    output_concepts: BTreeSet<String>,
    name_collection_search: bool,
    actions: BTreeSet<&'static str>,
    contract: BTreeSet<String>,
    domain: String,
    /// Key words without the domain alone: `research` in
    /// `research.bankruptcies.bySector` is a bucket, not the topic, unless the
    /// name or description says it too. What a Bazaar listing must mention.
    anchors: BTreeSet<String>,
    /// Synonym facets explicitly requested by the operation identity. Context
    /// like "transactions by directors" must not make a roster a transaction.
    primary_facets: u16,
    corporate_valuation: bool,
    enterprise_multiple: bool,
    equity_quote: bool,
    crypto_sentiment: bool,
    crypto_market: bool,
    metrics: MetricScope,
    dns: bool,
    dns_record_types: u16,
    exact_dns_records: bool,
    person_profile: bool,
    company_profile: bool,
}

// The historical mail-DNS adapter drops all but the registrable-domain
// labels. It cannot answer an exact owner query such as mail.example.com.
// Keep this conservative gate aligned with that adapter's legacy contract.
fn legacy_dns_preserves_name(name: &str) -> bool {
    let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
    let labels: Vec<_> = name.split('.').collect();
    let uk = labels.len() == 3 && labels[2] == "uk"
        && ["co", "org", "me", "ltd", "plc", "net", "sch", "ac", "gov"].contains(&labels[1]);
    (labels.len() == 2 || uk) && !name.contains('_')
}

/// Do not stem DNS types: the lexical index intentionally discards one/two
/// letter words, which would make A, NS and MX indistinguishable.
pub(super) fn dns_record_types(text: &str) -> u16 {
    let tokens: Vec<_> = text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty()).collect();
    // Keep the explicit output alias; prose "a record" is an article, not
    // an IPv4 request. A record-type letter remains case-sensitive here.
    let mut types = u16::from(text.split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
        .any(|token| matches!(token.to_ascii_lowercase().as_str(), "a_record" | "a_records" | "a-record" | "a-records")));
    for (index, token) in tokens.iter().enumerate() {
        types |= match token.to_ascii_lowercase().as_str() {
            "a" if (*token == "A" && tokens.get(index + 1).is_none_or(|next|
                    matches!(next.to_ascii_lowercase().as_str(), "and" | "or" | "aaaa" | "mx" | "ns" | "txt" | "caa" | "soa" | "cname")))
                || (index.checked_sub(1).is_some_and(|i| matches!(tokens[i].to_ascii_lowercase().as_str(), "dns" | "type" | "types"))
                    && !tokens.get(index + 1).is_some_and(|next| next.eq_ignore_ascii_case("dns")))
                || (*token == "A" && tokens.get(index + 1).is_some_and(|next| matches!(next.to_ascii_lowercase().as_str(), "record" | "records"))) => 1,
            "ipv4" => 1,
            "aaaa" | "ipv6" => 2,
            "cname" => 4,
            "mx" => 8,
            "ns" | "nameserver" | "nameservers" => 16,
            // A TXT retrieval does not derive a DMARC owner, DKIM selector,
            // or evaluate an SPF policy. Only literal TXT record scope.
            "txt" => 32,
            "caa" => 64,
            "soa" => 128,
            _ => 0,
        };
    }
    if tokens.iter().any(|word| word.eq_ignore_ascii_case("dns"))
        && tokens.iter().any(|word| word.eq_ignore_ascii_case("mail")) { types |= 8; }
    if tokens.windows(2).any(|pair|pair[0].eq_ignore_ascii_case("mail")
        && matches!(pair[1].to_ascii_lowercase().as_str(),"server"|"servers")) { types |= 8; }
    types
}

fn dns_record_types_from_description(text: &str) -> u16 {
    dns_record_types(super::opening_description(text))
}

/// A benchmark/rate/market is not interchangeable with another measurement.
/// These identities come from what is requested, not missing inputs alone.
#[derive(Default)]
struct MetricScope { salary: bool, labor_scarcity: bool, unemployment: bool, sector_market: bool }
impl MetricScope {
    fn new(capability: &Capability) -> Self {
        let identity=words(&format!("{} {} {}",capability.slug,capability.name,capability.description));
        let has=|terms:&[&str]|terms.iter().any(|term|identity.contains(*term));
        let sector = has(&["sector","industry"]) || capability.inputs.iter().any(|input|
            matches!(input.as_str(),"sector" | "industry" | "sector_code" | "industry_code"));
        Self {
            salary:has(&["salary","wage","salaris","loon"]),
            labor_scarcity:has(&["labor","labour","arbeidsmarkt"]) && has(&["scarcity","shortage","tightness","krapte"]),
            unemployment:has(&["unemployment","werkloosheid"]),
            sector_market:sector && has(&["market","markt"]) && has(&["size","growth","omvang","groei"]),
        }
    }
    fn union(&mut self,other:Self) {
        self.salary|=other.salary;self.labor_scarcity|=other.labor_scarcity;
        self.unemployment|=other.unemployment;self.sector_market|=other.sector_market;
    }
    fn supported_by(&self,candidate:&Document)->bool {
        let has=|terms:&[&str]|terms.iter().any(|term|
            candidate.operation_words.contains(*term) || candidate.contract.contains(*term));
        (!self.salary || has(&["salary","wage","salaris","loon","remuneration"]))
            && (!self.labor_scarcity || (has(&["labor","labour","job","workforce","arbeidsmarkt"])
                && has(&["scarcity","shortage","tightness","krapte"])))
            && (!self.unemployment || has(&["unemployment","werkloosheid"]))
            && (!self.sector_market || (has(&["sector","industry","industrial"])
                && has(&["size","growth","revenue","turnover","sales","omvang","groei","omzet"])))
    }
}

/// `text` with the subjects' names and identifiers removed, as in the query.
fn scrubbed(text: &str, capability: &Capability, parsed: &ParsedRequest) -> String {
    let mut only = capability.clone();
    only.name = text.to_owned();
    only.description = String::new();
    only.inputs = vec![];
    only.source_hint = None;
    capability_query(&only, parsed, &Task::default())
}

impl Request {
    pub fn new(capability: &Capability, parsed: &ParsedRequest) -> Self {
        let meaningful = |word: &String| !vague(word) && action(word).is_none();
        let identity = words(&scrubbed(
            &format!("{} {}", capability.domain, capability.slug),
            capability,
            parsed,
        ));
        let name_text = scrubbed(&capability.name, capability, parsed);
        let name = words(&name_text);
        let produced = words(&scrubbed(&capability.outputs.join(" "), capability, parsed));
        let supplied = words(&capability.inputs.join(" "));
        let mut key: BTreeSet<String> = identity
            .iter()
            .filter(|word| meaningful(word))
            .cloned()
            .collect();
        // English names end in their head noun: "Current local time",
        // "Public tenders", "Bankruptcy statistics".
        if let Some(head) = name_text
            .split(|character: char| !character.is_alphanumeric())
            .rev()
            .flat_map(|raw| words(raw).into_iter().filter(|word| meaningful(word)))
            .next()
        {
            key.insert(head);
        }
        // A name headed by an action ("Comparison", "Sector comparison") or
        // one without a topic word names what to do, not which data: the
        // description then says what is compared ("Compares the number of
        // bankruptcies across sectors"). Its words, less what the person
        // supplies, are key words too.
        let action_named = name_text
            .split(|character: char| !character.is_alphanumeric())
            .rev()
            .find(|raw| !raw.is_empty())
            .is_some_and(|raw| words(raw).iter().any(|word| action(word).is_some()));
        // An analysis still has a topic. "Sentiment analysis" must not let
        // context words such as news/social media replace sentiment. Expand
        // genuinely generic actions ("Comparative analysis") from their
        // description only when the name/id do not already agree on a topic.
        let named_topic = key.iter().any(|word| name.contains(word));
        let described: BTreeSet<String> = if (action_named && !named_topic) || key.is_empty() {
            words(&scrubbed(&capability.description, capability, parsed))
                .into_iter()
                .filter(|word| meaningful(word) && !supplied.contains(word))
                .collect()
        } else {
            BTreeSet::new()
        };
        let mut contract = contract_words(&format!("{}.{}", capability.domain, capability.slug));
        // A generic id ("research.comparison") names no family of its own;
        // the description's words stand in for it.
        if contract.is_empty() {
            contract = described.clone();
        }
        key.extend(described);
        let bucket = words(&capability.domain);
        let said: BTreeSet<String> = name
            .iter()
            .cloned()
            .chain(words(&scrubbed(
                &capability.description,
                capability,
                parsed,
            )))
            .collect();
        let anchors = key
            .iter()
            .filter(|word| !bucket.contains(*word) || said.contains(*word))
            .cloned()
            .collect();
        let primary_facets = document(Target::Source(0),
            &format!("{} {}", capability.slug, name_text), BTreeSet::new()).facets;
        let valuation_text=format!("{} {} {} {}", capability.slug, capability.name,
            capability.description, capability.outputs.join(" "));
        let valuation_words = words(&valuation_text);
        // The requested fields and capability identity establish record scope.
        // Explanatory prose can explicitly exclude other types ("MX, not TXT,
        // A or AAAA") and must not turn those exclusions into required calls.
        let dns_record_types = [&capability.slug, &capability.name].into_iter()
            .chain(capability.outputs.iter())
            .fold(0, |types, field| types | dns_record_types(field));
        let dns_record_types = if dns_record_types == 0 {
            dns_record_types_from_description(&capability.description)
        } else { dns_record_types };
        let dns_identity = words(&format!("{} {}", capability.slug, capability.name));
        let dns = dns_identity.contains("dns")
            || dns_identity.contains("nameserver")
            || (dns_record_types != 0 && dns_identity.contains("record"));
        let exact_dns_records = capability.outputs.iter().any(|output|
            output.ends_with("_records") || matches!(output.as_str(), "records" | "ttl" | "record_type" | "type_code"))
            || capability.subject_id.as_ref().and_then(|id| parsed.subjects.iter().find(|subject| &subject.id == id))
                .and_then(|subject| crate::ask_v2::input_values::literal("domain.dns_name", subject))
                .and_then(|value| value.as_str().map(|name| !legacy_dns_preserves_name(name))).unwrap_or(false);
        let enterprise_multiple = enterprise_ratio(&valuation_text);
        let company_subject = capability.subject_id.as_ref().and_then(|id|
            parsed.subjects.iter().find(|subject| &subject.id == id)).is_some_and(|subject| subject.kind == "company");
        let person_subject = capability.subject_id.as_ref().and_then(|id|
            parsed.subjects.iter().find(|subject| &subject.id == id)).is_some_and(|subject| subject.kind == "person");
        // The requested operation/record establishes profile retrieval. A
        // page-search description may mention profile pages as its topic,
        // while requesting only URLs/titles; that does not require a profile
        // reader. Link-shaped outputs likewise describe discovery results.
        let profile_record = |text: &str| {
            let scope = words(text);
            scope.contains("profile") && !scope.contains("url") && !scope.contains("link")
        };
        let requested_profile = profile_record(&capability.slug)
            || capability.outputs.iter().any(|output| profile_record(output));
        let individual_record = individual_profile_identity(&words(&capability.slug))
            || (!profile_record(&capability.slug) && capability.outputs.iter()
                .any(|output| individual_profile_identity(&words(output))));
        let company_record = (company_subject || capability.domain == "company") && !individual_record;
        // The primary record and subject take precedence over related fields.
        // A company profile can include people and employee profiles without
        // becoming a person lookup. Explicit individual-profile operations and
        // person subjects still keep their stricter person scope.
        let person_profile = requested_profile && (person_subject
            || (!company_record && ["person", "people", "professional"].iter()
                .any(|word| valuation_words.contains(*word))));
        let company_profile = requested_profile && company_record && !person_profile;
        let corporate_valuation = (primary_facets & VALUATION != 0 || enterprise_multiple)
            && (company_subject || capability.domain == "company" || enterprise_multiple);
        let equity_quote = primary_facets & STOCK_MARKET != 0
            && ["price", "quote"].iter().any(|word| valuation_words.contains(*word))
            && !derivative_market(&format!("{} {}", capability.name, capability.description));
        let crypto_sentiment = primary_facets & SENTIMENT != 0
            && (valuation_words.iter().any(|word| matches!(word.as_str(), "crypto" | "cryptocurrency"))
                || capability.subject_id.as_ref().and_then(|id| parsed.subjects.iter().find(|subject| &subject.id == id))
                    .is_some_and(|subject| subject.kind.starts_with("crypto")));
        let crypto_subject = capability.subject_id.as_ref().and_then(|id|
            parsed.subjects.iter().find(|subject| &subject.id == id))
            .is_some_and(|subject| subject.kind.starts_with("crypto"));
        let crypto_market = !crypto_sentiment
            && (crypto_subject || valuation_words.iter().any(|word|
                matches!(word.as_str(), "crypto" | "cryptocurrency")))
            && ["market", "price", "liquidity", "volume", "capitalization"].iter()
                .any(|word| valuation_words.contains(*word));
        Self {
            name_collection_search: super::collection_search::requested(capability),
            person_profile,
            company_profile,
            dns,
            dns_record_types,
            exact_dns_records,
            crypto_sentiment,
            crypto_market,
            metrics: MetricScope::new(capability),
            primary_facets,
            corporate_valuation,
            enterprise_multiple,
            equity_quote,
            anchors,
            topic: key
                .iter()
                .cloned()
                .chain(name.iter().filter(|word| meaningful(word)).cloned())
                .collect(),
            key,
            // What the person supplies is context: asking the time for a city
            // does not make every city-keyed service relevant.
            outputs: produced
                .iter()
                .filter(|word| meaningful(word) && !supplied.contains(*word))
                .cloned()
                .collect(),
            output_concepts: capability.outputs.iter().map(|output|
                super::output_metadata::concept_in_domain(output, &words(&capability.domain)))
                .filter(|concept| !concept.is_empty()).collect(),
            // Only a source's own action is evidence: that a listing
            // "compares portfolios" says nothing about comparing bankruptcies,
            // which Apiosk does itself once it has the data.
            actions: identity
                .iter()
                .chain(&name)
                .chain(&produced)
                .filter_map(|word| action(word))
                .filter(|stem| !DELIVERABLE_OPERATIONS.contains(stem))
                .collect(),
            contract,
            domain: capability.domain.to_lowercase(),
        }
    }

    /// This request with another wording's words (the same capability in
    /// English): a candidate may match either.
    pub fn union(mut self, other: Request) -> Self {
        self.key.extend(other.key);
        self.topic.extend(other.topic);
        self.outputs.extend(other.outputs);
        self.output_concepts.extend(other.output_concepts);
        self.name_collection_search |= other.name_collection_search;
        self.actions.extend(other.actions);
        self.contract.extend(other.contract);
        self.anchors.extend(other.anchors);
        self.primary_facets |= other.primary_facets;
        self.corporate_valuation |= other.corporate_valuation;
        self.enterprise_multiple |= other.enterprise_multiple;
        self.equity_quote |= other.equity_quote;
        self.crypto_sentiment |= other.crypto_sentiment;
        self.crypto_market |= other.crypto_market;
        self.metrics.union(other.metrics);
        self.dns |= other.dns;
        self.dns_record_types |= other.dns_record_types;
        self.exact_dns_records |= other.exact_dns_records;
        self.person_profile |= other.person_profile;
        self.company_profile |= other.company_profile;
        self
    }

    /// A broad data category may have only one non-generic lexical word.
    /// Exact schema fields/aliases are stronger evidence than a prose mention,
    /// but do not change the operation's record, asset or measurement scope.
    pub(super) fn output_matches(&self, candidate: &Document) -> bool {
        if self.name_collection_search && candidate.namespace.contains(&self.domain)
            && candidate.name_search_domains.contains(&self.domain) {
            return true;
        }
        let offered = if self.name_collection_search { &candidate.collection_concepts }
            else { &candidate.output_concepts };
        self.primary_facets == 0 && candidate.namespace.contains(&self.domain)
            && self.metrics.supported_by(candidate) && self.sentiment_scope_matches(candidate)
            && self.output_concepts.iter().any(|concept| {
                (!words(concept).is_disjoint(&self.key)
                    || (self.key.iter().all(|word| super::output_metadata::record_word(word))
                        && concept == &format!("record:{}", self.domain)))
                    && (offered.contains(concept)
                        // A documented collection can be relevant to fields
                        // of its records. This implication is one-way: a
                        // scalar never establishes record-category coverage.
                        || offered.contains(&format!("record:{concept}")))
            })
    }

    /// A shared word such as "profile" does not make an organisation's
    /// lobbying report or a board-game guide a profile of an individual.
    /// Read the operation's own scope, not input names or provider-wide copy.
    pub fn profile_scope_matches(&self, candidate: &Document) -> bool {
        // A company namespace can also contain employee/officer profile
        // operations. Their shared "profile" key does not turn an individual
        // record into the requested organisation record.
        if self.company_profile && individual_profile_identity(&candidate.contract) {
            return false;
        }
        if !self.person_profile { return true; }
        candidate.person_scope || ["person", "people", "personal", "professional", "individual"].iter()
            .any(|word| candidate.contract.contains(*word))
    }

    fn dns_scope_matches(&self, candidate: &Document) -> bool {
        if !self.dns { return true; }
        // Compare in the same normalized namespace as the search index.
        let has_capability = |slug| normalize_slug(slug, "")
            .is_some_and(|(slug, _)| candidate.capabilities.contains(&slug.to_lowercase()));
        // RDAP may list nameservers, but it is not a DNS record lookup. A
        // shared source title must not sell registry metadata as DNS results.
        if has_capability("domain.registration") { return false; }
        if self.exact_dns_records && has_capability("domain.mail-dns") { return false; }
        if self.dns_record_types == 0 { return true; }
        let matched_types = candidate.dns_record_types & self.dns_record_types;
        // Several executable operations can jointly retrieve A + MX + TXT.
        // Keep each contributing type; the planner's output-coverage gate
        // must prove the complete union before quoting the request.
        if matches!(candidate.target, Target::Endpoint(_)) { matched_types != 0 }
        else { matched_types == self.dns_record_types }
    }

    fn primary_topic_matches(&self, wanted: &Document, candidate: &Document) -> bool {
        if candidate.target.is_endpoint() && !self.metrics.supported_by(candidate) {
            return false;
        }
        if !self.sentiment_scope_matches(candidate) || !self.crypto_market_matches(candidate) { return false; }
        if self.equity_quote {
            // A derivative mark can track a stock without being that stock's
            // exchange quote. Likewise filings and positioning are not prices.
            let scope: BTreeSet<_> = candidate.topic_scope.union(&candidate.contract).cloned().collect();
            if candidate.derivative_asset
                || !["price", "quote", "quotation", "ohlc"].iter().any(|word| scope.contains(*word)) {
                return false;
            }
        }
        // Valuation is an operation on an asset, not one universal source
        // capability. A property's AVM or a card portfolio cannot provide
        // company multiples merely because both descriptions say valuation.
        if !self.valuation_matches(candidate) { return false; }
        // The operation's own scope must name the requested topic. Broad
        // descriptions, input names and incidental output words are not
        // evidence that a different operation performs the requested check.
        // Reviewed ids contribute their semantic words; generated HTTP-route
        // ids do not. Genuine concept synonyms (accounts/statements, PSC/UBO)
        // remain valid when the requested operation itself names that facet.
        let mut specific = self.primary_facets & SPECIFIC_OPERATIONS;
        // "Stock analyst consensus" asks for analyst consensus, not merely
        // a stock price. The narrower operation dominates the asset context.
        if specific & !STOCK_MARKET != 0 { specific &= !STOCK_MARKET; }
        if specific != 0 && candidate.facets & specific != specific { return false; }
        // Measurement/presentation words do not make a second subject:
        // "unemployment rate statistics" still asks about unemployment.
        let core = |word: &String| !matches!(word.as_str(), "statistic" | "rate" | "count" | "volume" | "metric" | "data");
        let generated = candidate.target.is_endpoint() && candidate.contract.is_empty();
        let anchored = self.topic.iter().filter(|word| (!generated || core(word))
            && (candidate.topic_scope.contains(*word) || candidate.contract.contains(*word))).count();
        // Unreviewed routes cannot establish a compound operation from one
        // incidental word (job vacancies vs rental vacancies, network
        // capacity vs credit-card net value). Semantic reviewed ids and
        // explicit synonym facets retain their existing evidence path.
        let needed = if generated {
            self.anchors.iter().filter(|word| core(word)).count().clamp(1, 2)
        } else { 1 };
        anchored >= needed
            // A named measurement is also an explicit output when it follows
            // a short title such as "Crypto market context — ...".
            || (self.primary_facets & SENTIMENT != 0
                && candidate.operation_words.contains("fear") && candidate.operation_words.contains("greed"))
            || self.primary_facets & candidate.contract_facets != 0
            || (self.primary_facets != 0 && wanted.tokens.iter()
                .filter(|word| word.starts_with('@'))
                .any(|word| candidate.topic_scope.contains(word)))
            || (self.anchors.is_empty() && self.outputs.iter()
                .filter(|word| candidate.topic_scope.contains(*word)).count() >= 2)
            // A broad parsed name such as "company people" must not hide a
            // company-profile operation whose own description explicitly
            // names several requested relationship fields. Keep the domain,
            // asset and specific-operation checks above; provider-wide prose
            // and arbitrary generated routes cannot supply this evidence.
            || (!candidate.contract.is_empty() && candidate.namespace.contains(&self.domain)
                && self.outputs.iter().filter(|word|core(word) && candidate.topic_scope.contains(*word)).count() >= 2)
    }

    fn valuation_matches(&self, candidate: &Document) -> bool {
        if !self.corporate_valuation { return true; }
        let scope: BTreeSet<_> = candidate.topic_scope.union(&candidate.contract).cloned().collect();
        let corporate = ["company", "corporate", "enterprise", "equity", "stock"].iter().any(|word| scope.contains(*word));
        corporate && (!self.enterprise_multiple || candidate.enterprise_multiple)
    }

    fn sentiment_scope_matches(&self, candidate: &Document) -> bool {
        if !self.crypto_sentiment { return true; }
        let scope = if candidate.operation_words.is_empty() { &candidate.tokens } else { &candidate.operation_words };
        // Generic text/topic sentiment remains a discovery option. Explicit
        // coverage of a different market is not coverage of cryptocurrency.
        let crypto = ["crypto", "cryptocurrency", "bitcoin", "btc", "ethereum", "eth"].iter().any(|word| scope.contains(*word));
        let another_scope = ["forex", "gold", "equity", "stock", "country", "geopolitical", "fragrance", "perfume"].iter().any(|word| scope.contains(*word));
        crypto || !another_scope
    }

    fn crypto_market_matches(&self, candidate: &Document) -> bool {
        if !self.crypto_market { return true; }
        // Market area, rental price and marketplace listings share pricing
        // vocabulary, but cannot supply cryptocurrency market data. Require
        // asset evidence in the operation itself, not its provider branding.
        let scope = if candidate.operation_words.is_empty() { &candidate.tokens } else { &candidate.operation_words };
        ["crypto", "cryptocurrency", "token", "blockchain", "onchain", "dex",
            "bitcoin", "ethereum", "solana", "cardano", "coingecko", "coinpaprika"]
            .iter().any(|word| scope.contains(*word) || candidate.contract.contains(*word))
    }

    /// Whether a listing outside the catalog (a Bazaar resource) is about
    /// what is asked: it shares a concept facet, a key word of the request
    /// (not merely its domain) or two of its output words. Words alone, as
    /// the anchor of [`Request::passes`]; a listing has no reviewed contract.
    pub fn mentions(&self, wanted: &Document, listing: &Document) -> bool {
        if !self.profile_scope_matches(listing) { return false; }
        if !self.dns_scope_matches(listing) { return false; }
        if !self.sentiment_scope_matches(listing) || !self.crypto_market_matches(listing) { return false; }
        wanted.facets & listing.facets != 0
            || self
                .anchors
                .iter()
                .any(|word| listing.tokens.contains(word))
            || self
                .outputs
                .iter()
                .filter(|word| listing.tokens.contains(*word))
                .count()
                >= 2
    }

    /// Whether `candidate`, already scored above zero, is offered at all.
    /// `hinted` is true when the person named the candidate's source.
    pub fn passes(
        &self,
        wanted: &Document,
        candidate: &Document,
        index: &Index,
        hinted: bool,
    ) -> bool {
        if !self.profile_scope_matches(candidate) { return false; }
        if !self.dns_scope_matches(candidate) { return false; }
        // A generic financial label, or even an exact accounts slug, does not
        // turn filed accounts into the explicitly requested enterprise ratios.
        if !self.crypto_market_matches(candidate) { return false; }
        if self.enterprise_multiple && candidate.target.is_endpoint()
            && !self.valuation_matches(candidate) { return false; }
        if !wanted.capabilities.is_disjoint(&candidate.capabilities) {
            return true;
        }
        let reviewed_output = self.output_matches(candidate);
        // A domain word alone does not turn a profile into a collection.
        // Exact reviewed operations above remain valid with other response
        // envelopes; non-exact alternatives need declared output evidence.
        if self.name_collection_search && !reviewed_output { return false; }
        if candidate.target.is_endpoint()
            && !self.primary_topic_matches(wanted, candidate) && !reviewed_output { return false; }
        // A registered listing can establish a discovery match from its own
        // operation description, just like a Bazaar listing. Requiring a
        // second contract/output signal here hides clear matches such as
        // "annual accounts" when no execution contract exists yet.
        if matches!(candidate.target, Target::Listing(_, _)) {
            return self.mentions(wanted, candidate);
        }
        if !hinted && candidate.contract.is_empty() && !reviewed_output
            && !self.primary_topic_matches(wanted,candidate) { return false; }
        let facet = wanted.facets & candidate.facets != 0;
        // A shared facet already stands for its vocabulary ("officer",
        // "director"); counting those words again would let a program
        // officer's grant brief pass as a company officer list.
        let counted = |word: &&String| {
            candidate.tokens.contains(*word) && !(facet && FACET_WORDS.contains(&word.as_str()))
        };
        let generic = (index.documents.len() / GENERIC_SHARE).max(GENERIC_MIN_DOCUMENTS);
        let weight = |word: &String| {
            // A reviewed operation's semantic identity does not become weaker
            // as unrelated directory entries accumulate. In the same domain,
            // an exact requested key in that identity remains full evidence;
            // provider prose and incidental output words retain normal weights.
            // Explicit source hints keep their existing operation-scope rule.
            // All asset, record and primary-topic guards have already passed.
            if !candidate.contract.is_empty() && self.key.contains(word)
                && ((candidate.namespace.contains(&self.domain) && candidate.contract.contains(word))
                    || (hinted && (candidate.contract.contains(word)
                        || (candidate.namespace.contains(&self.domain) && candidate.topic_scope.contains(word))))) {
                1.0
            } else if index.frequency.get(word).copied().unwrap_or(0) >= generic {
                0.5
            } else {
                1.0
            }
        };
        let topic: BTreeSet<&String> = self.topic.iter().filter(counted).collect();
        let outputs: BTreeSet<&String> = self.outputs.iter().filter(counted).collect();
        // Outputs alone anchor a reviewed contract of the requested domain
        // ("company" for a vaguely named company profile) at the usual bar,
        // and anything else only when both words are specific.
        let output_anchor = if candidate.namespace.contains(&self.domain) {
            OTHER_EVIDENCE
        } else {
            OUTPUT_ANCHOR
        };
        // A reviewed contract of another domain says what it is: it must share
        // a key word of the request. Overlapping outputs are not enough — a
        // cadastral parcel lookup ("registered surface area", "status" filters,
        // "PDOK Maps & Addresses") is not a company profile. Unreviewed
        // services have no such id, so their outputs may still anchor them.
        let foreign = !candidate.contract.is_empty() && !candidate.namespace.contains(&self.domain);
        let anchored = reviewed_output || facet
            || self.key.iter().any(|word| counted(&word))
            || (!foreign
                && outputs.len() >= 2
                && outputs.iter().map(|word| weight(word)).sum::<f64>() >= output_anchor);
        if !anchored {
            return false;
        }
        let evidence = f64::from(u8::from(facet || reviewed_output))
            + topic.union(&outputs).map(|word| weight(word)).sum::<f64>()
            + if candidate
                .tokens
                .iter()
                .any(|word| action(word).is_some_and(|stem| self.actions.contains(stem)))
            {
                0.5
            } else {
                0.0
            };
        // Same capability family: the reviewed ids share a word, or the
        // candidate's id itself names the requested facet (annual accounts
        // for financial statements, a director summary for officers).
        let family = !candidate.contract.is_disjoint(&self.contract)
            || wanted.facets & candidate.contract_facets != 0;
        evidence
            >= if family || hinted || reviewed_output {
                FAMILY_EVIDENCE
            } else {
                OTHER_EVIDENCE
            }
    }
}

pub(super) fn enterprise_ratio(text: &str) -> bool {
    let ordered=super::tokens(text);
    let denominator=|word:&str|matches!(word,"revenue"|"ebitda"|"ebit");
    ordered.windows(2).any(|pair|pair[0]=="ev" && denominator(&pair[1]))
        || ordered.windows(3).any(|pair|pair[0]=="ev" && pair[1]=="to" && denominator(&pair[2]))
        || (ordered.windows(2).any(|pair|pair[0]=="enterprise" && pair[1]=="value")
            && ordered.iter().any(|word|matches!(word.as_str(),"ratio"|"ratios"|"multiple"|"multiples"|"ebitda")))
}

pub(super) fn derivative_market(text: &str) -> bool {
    let words: Vec<_> = text.split(|c:char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty()).map(str::to_lowercase).collect();
    words.iter().enumerate().any(|(index,word)| {
        matches!(word.as_str(), "perp" | "perps" | "perpetual" | "perpetuals" | "derivative" | "derivatives")
            && !index.checked_sub(1).is_some_and(|previous|
                matches!(words[previous].as_str(), "not" | "no" | "without" | "exclude" | "excluding"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn profile_request(kind: &str, domain: &str) -> (Request, Document) {
        profile_request_for(kind, domain, &format!("{domain}.profile"))
    }

    fn profile_request_for(kind: &str, domain: &str, slug: &str) -> (Request, Document) {
        let question = "Retrieve the registered profile for Example B.V.";
        let parsed = crate::capability_intake::normalize(&json!({
            "language":"en",
            "subjects":[{"id":"subject","label":"Example B.V.","type":kind,"identifiers":{"company_name":"Example B.V."}}],
            "capabilities":[{"slug":slug,"name":"Profile",
                "description":"Retrieve the registered profile information for a company.",
                "domain":domain,"inputs":["company_name"],"outputs":["profile"],
                "subject_id":"subject","source_hint":null,"confidence":1}],
            "deliverable":{"format":"chat","operations":[]}
        }).to_string(), question, "fixture").unwrap();
        let capability = &parsed.capabilities[0];
        let wanted = super::super::wanted(capability, &parsed);
        (Request::new(capability, &parsed), wanted)
    }

    fn operation(slug: &str, description: &str) -> Document {
        let capabilities = super::super::normalized_capabilities(std::iter::once(slug));
        let mut candidate = document(Target::Endpoint(0), &format!("{slug} {description}"), capabilities);
        candidate.contract = contract_words(slug);
        candidate.namespace = match slug.split_once('.') {
            Some(("operation", _)) | None => BTreeSet::new(),
            Some((namespace, _)) => words(namespace),
        };
        candidate.topic_scope = words(description);
        candidate.person_scope = person_scope(description);
        candidate.operation_words = words(description);
        candidate
    }

    fn crowded_index() -> Index {
        // These unrelated listings make "profile" a common directory word.
        // They must not change a reviewed registry operation's identity.
        let documents = (0..24).map(|index| document(Target::Source(index),
            "Artist profile and creative portfolio", BTreeSet::new())).collect();
        Index { documents, frequency: BTreeMap::from([("profile".into(), 24)]) }
    }

    #[test]
    fn same_domain_reviewed_profile_identity_survives_directory_growth() {
        let (request, wanted) = profile_request("company", "company");
        let candidate = operation("company.registry-profile", "Retrieve a company's official registered profile.");
        let small = Index { documents: vec![], frequency: BTreeMap::new() };
        assert!(request.passes(&wanted, &candidate, &small, false));
        assert!(request.passes(&wanted, &candidate, &crowded_index(), false),
            "a generic profile request must retain its reviewed registry match without naming a provider");
    }

    #[test]
    fn company_profile_related_people_do_not_change_the_requested_record_type() {
        // Saved model wording from the live full-profile request: its related
        // people field used to incorrectly classify this as a person profile.
        let question = "Use Crunchbase to retrieve the full company profile for https://www.crunchbase.com/organization/starbucks, including available founders, employees, board members, funding rounds and investors.";
        let parsed = crate::capability_intake::normalize(&json!({
            "language":"en",
            "subjects":[{"id":"s1","label":"starbucks","type":"company",
                "identifiers":{"url":"https://www.crunchbase.com/organization/starbucks"}}],
            "capabilities":[{"slug":"company.profile","name":"Company profile",
                "description":"Retrieve the full company profile, including requested people, funding, and investor information.",
                "domain":"company","inputs":["url"],
                "outputs":["profile","founders","employees","board_members","funding_rounds","investors"],
                "subject_id":"s1","source_hint":"crunchbase","confidence":0.99}],
            "deliverable":{"format":"chat","operations":[]}
        }).to_string(), question, "saved-model-fixture").unwrap();
        let snapshot = serde_json::from_str(include_str!("../../../tests/fixtures/crunchbase/official-mcp.json")).unwrap();
        let catalog = crate::catalog::Catalog::build(snapshot);
        let found = super::super::search(&parsed, &catalog);
        assert!(found[0].operations.contains("company.crunchbase-profile"), "{:?}", found[0].apiosk);
        assert!(!found[0].operations.contains("person.crunchbase-profile"));

        // Scope derives from the record identity, without a named company or
        // provider condition. More related-person prose and fields retain it.
        let mut generic = parsed.clone();
        generic.subjects[0].label = "Example B.V.".into();
        generic.capabilities[0].source_hint = None;
        generic.capabilities[0].outputs.push("employee_profiles".into());
        for description in [
            "A company profile including people and their professional roles.",
            "A company profile with personal biographies of founders and company officers.",
        ] {
            generic.capabilities[0].description = description.into();
            let capability = &generic.capabilities[0];
            let request = Request::new(capability, &generic);
            let wanted = super::super::wanted(capability, &generic);
            assert!(request.passes(&wanted, &operation("company.registry-profile", "Retrieve a company's official registered profile."), &crowded_index(), false));
            assert!(!request.profile_scope_matches(&operation("person.profile", "Retrieve one person's professional profile.")));
            assert!(!request.profile_scope_matches(&operation("company.employee-profile", "Retrieve one company employee profile.")));
        }
    }

    #[test]
    fn split_company_people_and_funding_requirements_find_the_shared_profile() {
        let catalog = crate::catalog::Catalog::build(serde_json::from_str(include_str!("../../../tests/fixtures/homepage-starters/catalog.json")).unwrap());
        let mut parsed: crate::capability_intake::ParsedRequest = serde_json::from_str(include_str!("../../../tests/fixtures/crunchbase/people-funding-intake.json")).unwrap();
        for name in ["Company people", "Company leadership", "Company relationships"] {
            parsed.capabilities[0].name = name.into();
            let matches = super::super::search(&parsed,&catalog);
            assert!(matches.iter().all(|found|found.operations.contains("company.crunchbase-profile")),"{name}: {matches:?}");
            assert!(!matches[0].operations.contains("person.crunchbase-profile"),"company relationships do not request individual profile retrievals");
        }
    }

    #[test]
    fn reviewed_identity_weight_does_not_bypass_domain_or_record_scope() {
        let (request, wanted) = profile_request("company", "company");
        for (slug, description) in [
            ("person.profile", "Retrieve an individual person profile."),
            ("property.profile", "Retrieve a registered property profile."),
            ("company.person-profile", "Retrieve an individual person profile."),
            ("company.employee-profile", "Retrieve a company employee profile."),
            ("company.officers", "Retrieve officers mentioned in a company profile."),
            ("operation.directory.profile", "Company profile lookup."),
        ] {
            assert!(!request.passes(&wanted, &operation(slug, description), &crowded_index(), false),
                "{slug}: shared or incidental profile wording is not a reviewed company profile identity");
        }
        let (person, wanted) = profile_request("person", "research");
        assert!(!person.passes(&wanted, &operation("company.registry-profile",
            "Retrieve a company's official registered profile."), &crowded_index(), false));
        let (employee, wanted) = profile_request_for("company", "company", "company.employee-profile");
        assert!(employee.passes(&wanted, &operation("company.employee-profile",
            "Retrieve a company employee profile."), &crowded_index(), false),
            "an explicitly requested individual record keeps its exact operation match");
    }
}
