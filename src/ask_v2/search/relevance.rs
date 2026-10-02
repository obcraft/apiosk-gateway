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
use super::{document, vague, words, Document, Index, Target, DELIVERABLE_OPERATIONS, FACET_WORDS, SPECIFIC_OPERATIONS, STOCK_MARKET};
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
        Self {
            primary_facets,
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
        self
    }

    fn primary_topic_matches(&self, wanted: &Document, candidate: &Document) -> bool {
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
