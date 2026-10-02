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
use super::{document, vague, words, Document, Index, Target, DELIVERABLE_OPERATIONS, FACET_WORDS, SPECIFIC_OPERATIONS, STOCK_MARKET, VALUATION};
use crate::{
    capability_intake::{capability_query, Capability, ParsedRequest},
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
    metrics: MetricScope,
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
        let described: BTreeSet<String> = if action_named || key.is_empty() {
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
        let enterprise_multiple = enterprise_ratio(&valuation_text);
        let company_subject = capability.subject_id.as_ref().and_then(|id|
            parsed.subjects.iter().find(|subject| &subject.id == id)).is_some_and(|subject| subject.kind == "company");
        let corporate_valuation = (primary_facets & VALUATION != 0 || enterprise_multiple)
            && (company_subject || capability.domain == "company" || enterprise_multiple);
        let equity_quote = primary_facets & STOCK_MARKET != 0
            && ["price", "quote"].iter().any(|word| valuation_words.contains(*word))
            && !derivative_market(&format!("{} {}", capability.name, capability.description));
        Self {
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
        self.actions.extend(other.actions);
        self.contract.extend(other.contract);
        self.anchors.extend(other.anchors);
        self.primary_facets |= other.primary_facets;
        self.corporate_valuation |= other.corporate_valuation;
        self.enterprise_multiple |= other.enterprise_multiple;
        self.equity_quote |= other.equity_quote;
        self.metrics.union(other.metrics);
        self
    }

    fn primary_topic_matches(&self, wanted: &Document, candidate: &Document) -> bool {
        if matches!(candidate.target, Target::Endpoint(_)) && !self.metrics.supported_by(candidate) {
            return false;
        }
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
        let generated = matches!(candidate.target, Target::Endpoint(_)) && candidate.contract.is_empty();
        let anchored = self.topic.iter().filter(|word| (!generated || core(word))
            && (candidate.topic_scope.contains(*word) || candidate.contract.contains(*word))).count();
        // Unreviewed routes cannot establish a compound operation from one
        // incidental word (job vacancies vs rental vacancies, network
        // capacity vs credit-card net value). Semantic reviewed ids and
        // explicit synonym facets retain their existing evidence path.
        let needed = if generated {
            self.anchors.iter().filter(|word| core(word)).count().min(2).max(1)
        } else { 1 };
        anchored >= needed
            || self.primary_facets & candidate.contract_facets != 0
            || (self.primary_facets != 0 && wanted.tokens.iter()
                .filter(|word| word.starts_with('@'))
                .any(|word| candidate.topic_scope.contains(word)))
            || (self.anchors.is_empty() && self.outputs.iter()
                .filter(|word| candidate.topic_scope.contains(*word)).count() >= 2)
    }

    fn valuation_matches(&self, candidate: &Document) -> bool {
        if !self.corporate_valuation { return true; }
        let scope: BTreeSet<_> = candidate.topic_scope.union(&candidate.contract).cloned().collect();
        let corporate = ["company", "corporate", "enterprise", "equity", "stock"].iter().any(|word| scope.contains(*word));
        corporate && (!self.enterprise_multiple || candidate.enterprise_multiple)
    }

    /// Whether a listing outside the catalog (a Bazaar resource) is about
    /// what is asked: it shares a concept facet, a key word of the request
    /// (not merely its domain) or two of its output words. Words alone, as
    /// the anchor of [`Request::passes`]; a listing has no reviewed contract.
    pub fn mentions(&self, wanted: &Document, listing: &Document) -> bool {
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
        // A generic financial label, or even an exact accounts slug, does not
        // turn filed accounts into the explicitly requested enterprise ratios.
        if self.enterprise_multiple && matches!(candidate.target, Target::Endpoint(_))
            && !self.valuation_matches(candidate) { return false; }
        if !wanted.capabilities.is_disjoint(&candidate.capabilities) {
            return true;
        }
        if matches!(candidate.target, Target::Endpoint(_))
            && !self.primary_topic_matches(wanted, candidate) { return false; }
        if !hinted && candidate.contract.is_empty() && !self.primary_topic_matches(wanted,candidate) { return false; }
        let facet = wanted.facets & candidate.facets != 0;
        // A shared facet already stands for its vocabulary ("officer",
        // "director"); counting those words again would let a program
        // officer's grant brief pass as a company officer list.
        let counted = |word: &&String| {
            candidate.tokens.contains(*word) && !(facet && FACET_WORDS.contains(&word.as_str()))
        };
        let generic = (index.documents.len() / GENERIC_SHARE).max(GENERIC_MIN_DOCUMENTS);
        let weight = |word: &String| {
            if index.frequency.get(word).copied().unwrap_or(0) >= generic {
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
        let anchored = facet
            || self.key.iter().any(|word| counted(&word))
            || (!foreign
                && outputs.len() >= 2
                && outputs.iter().map(|word| weight(word)).sum::<f64>() >= output_anchor);
        if !anchored {
            return false;
        }
        let evidence = f64::from(u8::from(facet))
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
            >= if family || hinted {
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
