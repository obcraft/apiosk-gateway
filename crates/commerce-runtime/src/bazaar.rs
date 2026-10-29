//! Shared vocabulary for offers Apiosk has not catalogued.
//!
//! `/v1/capabilities/{slug}` compares what Apiosk has curated. That is 44
//! operations out of a catalogue of 1513, against an external ecosystem holding
//! tens of thousands of resources — so for most jobs the honest answer to "who
//! can do this" is bigger than the answer Apiosk can give from its own rows.
//!
//! The fetching lives in [`crate::sources`], which sweeps every free x402 index
//! concurrently. What stays here is what all of those share: the
//! [`ExternalOffer`] shape they are normalised into, the phrasings a capability
//! is searched under ([`query_variants`]), and the two quality heuristics every
//! index is filtered through ([`looks_like_toy`], [`skips_host`]).
//!
//! ---------------------------------------------------------------------------
//! Why these results are NOT candidates
//! ---------------------------------------------------------------------------
//!
//! Everything here is unverified, and the response says so. Three reasons it
//! cannot be merged into `candidates`:
//!
//!   * **No reviewed input contract.** Many Bazaar resources now publish a
//!     provider-declared input schema (`extensions.bazaar`), which the sweep
//!     carries through on the offer — but it is the provider's own claim,
//!     never checked against the endpoint, so there is still nothing to build
//!     a trusted `input_map` from. They are comparable on price; they are not
//!     substitutable.
//!   * **No review.** A curated assignment was checked against the endpoint's
//!     real contract by a human. A search hit was checked by a search engine.
//!     Mixing them would make the whole list only as trustworthy as the weaker
//!     half.
//!   * **No quality floor.** The corpus contains dice rollers and timezone
//!     toys next to real infrastructure. [`looks_like_toy`] drops the obvious
//!     ones, but it is a heuristic and cannot be anything else.
//!
//! So they are returned under `external_candidates`, behind an opt-in flag,
//! labelled unverified, and never counted in the comparison's price spread.

use std::borrow::Cow;

use serde_json::{json, Value};

/// Hosts whose resources are never offered. Apiosk's own surface is already in
/// the catalogue, and re-importing it would have the comparison list itself.
const SKIP_HOST_SUFFIXES: &[&str] = &["apiosk.com"];

/// Substrings that mark a resource as a demo rather than a supplier.
///
/// The corpus is open, so it contains toys: dice rollers, coin flips, "hello
/// world" endpoints. A comparison that offers those next to a real provider
/// reads as a catalogue of junk, which costs more trust than the extra
/// coverage buys. Deliberately conservative: it drops the obvious and keeps
/// anything ambiguous, because a wrongly-dropped supplier is invisible and a
/// wrongly-kept toy is merely ignored.
const TOY_MARKERS: &[&str] = &[
    "dice",
    "coinflip",
    "coin-flip",
    "hello world",
    "helloworld",
    "test endpoint",
    "lorem ipsum",
    "random number",
    "guess the",
    "8ball",
    "8-ball",
    "fortune cookie",
];

/// One unverified offer from the wider ecosystem.
#[derive(Debug, Clone, Default)]
pub struct ExternalOffer {
    pub resource: String,
    pub host: String,
    pub description: String,
    pub price_usd: Option<f64>,
    pub network: Option<String>,
    pub pay_to: Option<String>,
    pub method: Option<String>,
    /// Provider-declared example call shape (method, pathParams, queryParams,
    /// body), straight from the index listing. Declared, not verified: nobody
    /// at Apiosk has called the endpoint to check it.
    pub input: Option<Value>,
    /// Provider-declared JSON Schema for the input, when the listing carries
    /// one. This is what separates a callable offer from a guess-and-pay one.
    pub input_schema: Option<Value>,
    /// Provider-declared output description (type, example), when present.
    pub output: Option<Value>,
    /// Which index this came from, e.g. `coinbase-x402-bazaar` or `payai`.
    ///
    /// Carried per offer rather than stated once for the whole block: the sweep
    /// merges several indexes, so "where did this come from" is now a per-offer
    /// fact, and an agent that keeps one entry needs it to survive the split.
    pub source: String,
    /// Who is selling this, in their own words — the `name` on the
    /// `discovery_sources` row the offer was swept from.
    ///
    /// Before this existed the only provider identity an offer carried was its
    /// hostname, so `pro-api.coingecko.com` had to stand in for "CoinGecko".
    /// That is not a name an agent can match a request against, and it is not
    /// a name a person recognises in a list.
    pub provider: Option<String>,
    /// The provider's logo, carried straight from the source row so every
    /// surface renders the same mark. Never derived here: a guessed favicon
    /// that 404s is worse than no logo at all, so the guessing (and the
    /// checking) happens once, at import.
    pub provider_logo: Option<String>,
    /// The provider's own category, e.g. `Finance`. Matched on, not just shown.
    pub category: Option<String>,
    /// The provider's own tags, e.g. `market-data`, `crypto-prices`. These are
    /// the words a resource URL and a terse description usually leave out, and
    /// they are why a 402-challenge offer with one line of text is findable.
    pub tags: Vec<String>,
    /// How completely this listing documents itself, out of ten — see
    /// [`score_listing`]. Scored at parse time, while the provider-declared
    /// schemas are still in hand, because the crawler drops them immediately
    /// afterwards to fit the corpus in memory (`crate::source_index`). The
    /// number survives; the several kilobytes it was read from do not.
    pub doc_score: u8,
}

/// What Apiosk would do with one external offer if the buyer picked it.
///
/// An x402 endpoint outside the catalogue is not automatically unbuyable: the
/// gateway can front the provider's own 402 from the platform payer wallet and
/// charge its configured fee (`POST /v1/x402/fetch`). Whether it can do that for
/// this endpoint depends on
/// the feature being enabled, the rail being Base + USDC, the price sitting
/// under the platform ceiling and the host's policy — four facts only the
/// gateway holds. It is computed once, by the code that also charges, and
/// carried on the offer so that discovery and the comparison quote the same
/// direct-fetch estimate. Balance purchases use their own validated quote.
#[derive(Debug, Clone, Default)]
pub struct ExternalSettlement {
    /// The listed host, network and price pass discovery's settlement checks.
    /// The input contract and live payment challenge still need validation.
    pub payable: bool,
    /// Indicative USD total for `/v1/x402/fetch`, using the deployment's
    /// configured fee and minimum. This may differ from a balance purchase's
    /// standard fee. The live provider challenge still needs validation.
    /// `None` when no total can be estimated.
    pub buyer_price_usd: Option<f64>,
    /// Why not, in the caller's language. `None` when it is payable.
    pub reason: Option<String>,
}

impl ExternalOffer {
    /// The offer as JSON, carrying what the gateway would actually do with it.
    ///
    /// The only way an offer becomes JSON: every surface goes through
    /// [`crate::sources::settled_json`], so no screen can say "pay the
    /// provider yourself" about an endpoint the gateway would settle.
    pub fn to_json_settled(&self, settlement: &ExternalSettlement) -> Value {
        let mut value = self.base_json();
        if let Some(map) = value.as_object_mut() {
            map.insert(
                "settlement".into(),
                json!(if settlement.payable {
                    "apiosk"
                } else {
                    "direct"
                }),
            );
            map.insert("buyer_price_usd".into(), json!(settlement.buyer_price_usd));
            // Preserve the configured direct-fetch estimate: changing a
            // discovery response must not change that route's tariff. The
            // balance workflow independently quotes at the standard fee,
            // which can differ from an older deployment's fetch settings.
            map.insert(
                "buyer_price_basis".into(),
                json!("indicative_external_fetch_config"),
            );
            let balance_price = self
                .price_usd
                .filter(|price| settlement.payable && price.is_finite() && *price > 0.0)
                .map(|price| crate::fees::buyer_total_usd(price, true));
            map.insert("balance_buyer_price_usd".into(), json!(balance_price));
            map.insert(
                "balance_buyer_markup_bps".into(),
                json!(crate::fees::BUYER_MARKUP_BPS),
            );
            map.insert("price_is_estimate".into(), json!(true));
            map.insert("settlement_reason".into(), json!(settlement.reason));
            map.insert(
                "note".into(),
                json!(if settlement.payable {
                    "Unreviewed: input/output descriptions are the provider's claims. The listed payment network and host policy permit Apiosk settlement, subject to a valid input contract and live provider price. buyer_price_usd estimates /v1/x402/fetch using its configured fee; balance_buyer_price_usd estimates a standard Apiosk balance purchase. These may differ. Neither is an approved quote: review the validated total before purchase."
                } else if self.input.is_some() || self.input_schema.is_some() {
                    "Unverified. The input/output description is the provider's own claim from the index listing; Apiosk has not reviewed this endpoint and cannot settle it. Pay the provider directly."
                } else {
                    "Unverified. Apiosk has not reviewed this endpoint, has no input mapping for it, and cannot settle it. Pay the provider directly and read its own description before calling."
                }),
            );
        }
        value
    }

    fn base_json(&self) -> Value {
        json!({
            "resource": self.resource,
            "host": self.host,
            "description": self.description,
            "price_usd": self.price_usd,
            "network": self.network,
            "pay_to": self.pay_to,
            "method": self.method,
            "input": self.input,
            "input_schema": self.input_schema,
            "output": self.output,
            "source": self.source,
            "provider": self.provider,
            "provider_logo": self.provider_logo,
            "category": self.category,
            "tags": self.tags,
            "verified": false,
            // `note` is filled in by `to_json_settled`. Said per offer, not only
            // once in the meta block: an agent that reads a single entry out of
            // context still has to be told that nobody checked this one.
        })
    }
}

/// True for a resource that reads as a demo rather than a supplier.
///
/// A single-word marker matches only as a WHOLE word. `dice` used to be a
/// substring test, so every market-data endpoint whose description mentioned
/// "indices" was silently dropped as a dice roller — the exact opposite of the
/// "wrongly-dropped supplier is invisible" rule this list is written to.
/// Multi-word markers stay substring tests: they are specific enough already.
pub fn looks_like_toy(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    TOY_MARKERS.iter().any(|marker| {
        if marker.contains(' ') || marker.contains('-') {
            lower.contains(marker)
        } else {
            lower
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|word| word == *marker)
        }
    })
}

fn skip_host(host: &str) -> bool {
    SKIP_HOST_SUFFIXES
        .iter()
        .any(|suffix| is_under(host, suffix))
}

/// Whether a host is never offered as external supply. Shared with the
/// multi-source sweep so one list governs every index, not just this one.
pub fn skips_host(host: &str) -> bool {
    skip_host(host)
}

// ---------------------------------------------------------------------------
// The quality gate
// ---------------------------------------------------------------------------
//
// [`looks_like_toy`] answers "is this a joke endpoint" and nothing else, which
// was the whole floor under the sweep until now. It is a list of eleven
// substrings against a corpus of roughly forty thousand, so what it actually
// caught was dice rollers, and what it let through was every endpoint whose
// description is the word "test", every localhost URL somebody registered from
// a laptop, and every ngrok tunnel that stopped existing the day it was
// published. Measured on the live corpus that is most of it, and it is why
// migration 095 turned the whole sweep off rather than tune it: nine of ten
// listings did not answer, and a catalogue that is nine-tenths broken costs
// more trust than the coverage buys.
//
// So the floor is now about the LISTING rather than about jokes: can this
// entry be called at all, does it say what it does, and does it name a price
// anybody could pay. Three questions, each of which is a fact on the record
// rather than a judgement about the seller, and each of which is checked once
// per crawl instead of once per search.
//
// The gate deliberately does NOT probe the endpoint. Reading forty thousand
// 402 challenges would be free in money and enormous in time and outbound
// traffic, and it would make our crawl the single largest client of every
// provider in the ecosystem. What survives here is "plausibly callable", not
// "verified"; verification remains what the catalogue means and what these
// offers are explicitly labelled as lacking.

/// Hosts that cannot serve a paying stranger, whatever they claim.
///
/// Two kinds, both fatal and for the same reason: the address either resolves
/// to the publisher's own machine (localhost, a bare IP, `.local`) or it is a
/// tunnel that exists for as long as one terminal window stays open (ngrok,
/// trycloudflare, serveo). Either way the listing describes something nobody
/// else can reach, so it is not supply.
///
/// Matched as DOMAIN SUFFIXES, never as substrings. `.local` as a substring
/// drops `api.locale.com`, and `localhost` as a substring drops
/// `api.mylocalhost-data.io` — the same bug `looks_like_toy` was fixed for
/// when "indices" was being read as a dice roller. A suffix match means
/// `example.com` and `api.example.com` are refused and `counterexample.com`
/// is not.
const BAD_HOST_SUFFIXES: &[&str] = &[
    "localhost",
    "local",
    "test",
    "invalid",
    "internal",
    "onion",
    "example.com",
    "example.org",
    "example.net",
    "ngrok.io",
    "ngrok-free.app",
    "ngrok-free.dev",
    "ngrok.app",
    "ngrok.dev",
    "trycloudflare.com",
    "loca.lt",
    "serveo.net",
    "nip.io",
    "sslip.io",
    "xip.io",
];

/// Free hosting tiers. Not broken, and not banned on principle — but on this
/// corpus they mark hobby demos with overwhelming reliability, because a
/// provider charging money for an endpoint moves it to a domain it owns.
///
/// This is the one rule in the gate that is a judgement rather than a fact,
/// and it is the first thing to relax if a real supplier is ever found behind
/// one: the cost of a wrongly-dropped supplier is that it is invisible, which
/// is worse than the cost of a wrongly-kept demo.
const EPHEMERAL_HOST_SUFFIXES: &[&str] = &[
    "onrender.com",
    "zeabur.app",
    "run.app",
    "herokuapp.com",
    "railway.app",
    "up.railway.app",
    "fly.dev",
    "replit.dev",
    "repl.co",
    "glitch.me",
    "vercel.app",
    "netlify.app",
    "pages.dev",
    "workers.dev",
];

/// Descriptions that describe nothing. Whole-word matched against a short
/// description, so a real sentence that happens to contain "test" survives and
/// a description that IS the word "test" does not.
const JUNK_DESCRIPTIONS: &[&str] = &[
    "test", "testing", "demo", "example", "sample", "foo", "bar", "baz", "ping", "pong", "asdf",
    "todo", "tbd", "n/a", "none", "untitled",
];

/// Phrases that mark a description as never having been written.
const JUNK_PHRASES: &[&str] = &[
    "lorem ipsum",
    "your description here",
    "description here",
    "add a description",
    "no description",
    "coming soon",
    "placeholder",
];

/// Shortest description that can distinguish one endpoint from another.
///
/// Eighty characters is about one full sentence. Below that the description is
/// a label ("Weather API", "Get price") and the ranking has nothing to match a
/// job against, so the entry can only ever be found by someone who already
/// knows its URL — which is precisely the person who does not need discovery.
const MIN_DESC_CHARS: usize = 80;

/// And it has to be a sentence rather than a long name: eighty characters of
/// hyphenated URL slug passes the character floor and says nothing.
const MIN_DESC_WORDS: usize = 12;

/// Letters, so a description made of punctuation or digits is not a sentence.
const MIN_DESC_LETTERS: usize = 3;

/// Why one offer was refused. Returned rather than a bare `false` so the crawl
/// can report what it dropped and why — a gate whose effect nobody can see is
/// a gate nobody can tune, and the four constants above are meant to be tuned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    /// Not an https URL, or not a URL at all.
    NotHttps,
    /// Unreachable by anyone but the publisher, or a disposable tunnel.
    BadHost,
    /// Free hosting tier; on this corpus, a hobby demo.
    EphemeralHost,
    /// No dot, or a one-character TLD: not a public name.
    NotAPublicName,
    /// Shorter than a sentence, so nothing can be matched against it.
    ThinDescription,
    /// A placeholder somebody never replaced.
    JunkDescription,
    /// Reads as a toy rather than a supplier.
    Toy,
    /// No price, or a price of zero: nothing anybody could quote.
    Unpriced,
    /// No network, or a network the listing itself calls unknown.
    NoNetwork,
}

impl Reject {
    /// The one word this reason is counted under in the crawl's log.
    pub fn label(self) -> &'static str {
        match self {
            Reject::NotHttps => "not_https",
            Reject::BadHost => "bad_host",
            Reject::EphemeralHost => "ephemeral_host",
            Reject::NotAPublicName => "not_a_public_name",
            Reject::ThinDescription => "thin_description",
            Reject::JunkDescription => "junk_description",
            Reject::Toy => "toy",
            Reject::Unpriced => "unpriced",
            Reject::NoNetwork => "no_network",
        }
    }
}

/// True when `host` is `suffix` or sits under it as a label boundary.
///
/// `is_under("api.example.com", "example.com")` is true;
/// `is_under("counterexample.com", "example.com")` is false. Allocation-free:
/// this runs once per suffix per offer, forty-five thousand offers per crawl,
/// and the obvious `format!(".{suffix}")` spelling is half a million throwaway
/// strings for a comparison that never needed one.
fn is_under(host: &str, suffix: &str) -> bool {
    if host == suffix {
        return true;
    }
    match host.len().checked_sub(suffix.len()) {
        // The byte before the suffix must be the label separator, or
        // "notexample.com" reads as being under "example.com".
        Some(cut) if cut > 0 => host.as_bytes()[cut - 1] == b'.' && &host[cut..] == suffix,
        _ => false,
    }
}

/// True when a host is one nobody outside the publisher can reach.
fn bad_host(host: &str) -> bool {
    if BAD_HOST_SUFFIXES
        .iter()
        .any(|suffix| is_under(host, suffix))
    {
        return true;
    }
    // A bare IPv4 address: four dot-separated numbers and nothing else. An
    // address is not a name, and nothing that charges money publishes one.
    let mut labels = 0usize;
    for label in host.split('.') {
        if label.is_empty() || !label.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        labels += 1;
    }
    labels == 4
}

/// True for a free hosting tier.
fn ephemeral_host(host: &str) -> bool {
    EPHEMERAL_HOST_SUFFIXES
        .iter()
        .any(|suffix| is_under(host, suffix))
}

/// True when the host looks like a public name: has a dot and a real TLD.
fn public_name(host: &str) -> bool {
    match host.rsplit_once('.') {
        Some((left, tld)) => {
            !left.is_empty() && tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic())
        }
        None => false,
    }
}

/// True when a description is a placeholder rather than a description.
fn junk_description(description: &str) -> bool {
    let lower = description.to_ascii_lowercase();
    if JUNK_PHRASES.iter().any(|phrase| lower.contains(phrase)) {
        return true;
    }
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '/')
        .filter(|w| !w.is_empty())
        .collect();
    // Only when the WHOLE description is junk words. One "test" inside a real
    // sentence is a real sentence.
    !words.is_empty() && words.iter().all(|w| JUNK_DESCRIPTIONS.contains(w))
}

/// Whether an external listing is plausibly callable supply.
///
/// `Ok(())` to keep it, `Err(reason)` to drop it. Every check is on the
/// listing's own record: no network call, no model, no lookup.
pub fn quality_gate(
    resource: &str,
    host: &str,
    description: &str,
    price_usd: Option<f64>,
    network: Option<&str>,
) -> Result<(), Reject> {
    if !resource.starts_with("https://") {
        return Err(Reject::NotHttps);
    }
    // Borrowed when the host is already lowercase, which it is for all but a
    // handful of listings. See `is_under` on why this path avoids allocating.
    let trimmed = host.trim();
    let lowered: Cow<'_, str> = if trimmed.bytes().any(|b| b.is_ascii_uppercase()) {
        Cow::Owned(trimmed.to_ascii_lowercase())
    } else {
        Cow::Borrowed(trimmed)
    };
    let host = lowered.strip_prefix("www.").unwrap_or(&lowered);
    // `crate::sources::host_of` keeps whatever followed the authority, port
    // included, so the host can arrive as `thing.vercel.app:443`. Every check
    // below is a name comparison and none of them survive a trailing `:443`,
    // so an ephemeral host with an explicit port would otherwise walk straight
    // through the one list written to stop it. IPv6 literals are bracketed, so
    // cutting at the first colon is only ever the port.
    let host = match host.split_once(':') {
        Some((name, _port)) if !host.starts_with('[') => name,
        _ => host,
    };
    if skip_host(host) || bad_host(host) {
        return Err(Reject::BadHost);
    }
    if ephemeral_host(host) {
        return Err(Reject::EphemeralHost);
    }
    if !public_name(host) {
        return Err(Reject::NotAPublicName);
    }

    let description = description.trim();
    if description.chars().filter(|c| c.is_alphabetic()).count() < MIN_DESC_LETTERS {
        return Err(Reject::JunkDescription);
    }
    if junk_description(description) {
        return Err(Reject::JunkDescription);
    }
    if description.chars().count() < MIN_DESC_CHARS
        || description.split_whitespace().count() < MIN_DESC_WORDS
    {
        return Err(Reject::ThinDescription);
    }
    // Each on its own rather than joined: concatenating them lets a multi-word
    // marker match across the seam between a URL and a sentence that neither
    // of them contains.
    if looks_like_toy(resource) || looks_like_toy(description) {
        return Err(Reject::Toy);
    }

    match price_usd {
        Some(price) if price.is_finite() && price > 0.0 => {}
        _ => return Err(Reject::Unpriced),
    }
    match network.map(str::trim) {
        Some(net) if !net.is_empty() && !net.eq_ignore_ascii_case("unknown") => {}
        _ => return Err(Reject::NoNetwork),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The score
// ---------------------------------------------------------------------------
//
// [`quality_gate`] answers "could a stranger call this at all". That is a
// FLOOR, and a floor is not a standard: 45,162 listings across the two
// facilitators, 7,978 of them clear it, and most of those 7,978 are still one
// sentence and a price. An agent handed one of them has to guess the
// parameters, and a guess that costs money is not discovery.
//
// So the floor decides what is admissible and this decides what is GOOD, out
// of ten, from what the listing publishes about itself. Every point is a fact
// on the record — a field that is present or absent — never a judgement about
// the seller and never a model's opinion.
//
// ---------------------------------------------------------------------------
// Where the facts come from
// ---------------------------------------------------------------------------
//
// The two facilitators speak the same envelope and document themselves
// completely differently, so both dialects are read here rather than one being
// treated as the shape and the other as a defect. Measured 2026-09-04:
//
//                                   Bazaar (17,032)   PayAI (28,106)
//   description >= 80c / 12 words        78.5%            10.2%
//   input properties named               99.3%             0.0%
//   every property typed                 98.2%             0.0%
//   `required` declared                  93.8%             0.0%
//   output shape or example              77.8%            77.4%
//   names itself                         61.4%             0.1%
//   tags or category                     60.4%             0.0%
//
// PayAI's `inputSchema` is not a schema — it is a captured call, values and
// all (`{type, method, pathParams: {inbox_id: "henry@grailcart.com"}}`). It
// tells you the shape of one request somebody once made, which is worth a
// point as a call shape and cannot be worth the three that a typed, named,
// required-annotated contract is worth. That is why the table above is in this
// comment: the scoring is not tuned against one source, and the consequence
// (PayAI's ceiling is 6, so it publishes nothing that clears 8) is a fact
// about what PayAI publishes rather than a decision taken about PayAI.
//
// ---------------------------------------------------------------------------
// Measurement caps, never lifts
// ---------------------------------------------------------------------------
//
// The Bazaar publishes real usage per resource: calls and unique payers over
// thirty days, and when it was last called. It is the only evidence in the
// corpus that is not the seller describing itself, and it is brutal — the
// median listing was called ONCE in thirty days.
//
// It is applied exactly the way `crate::quality::rate` applies a measured
// success rate to a catalogue card, and for the same reason: a source that
// publishes no usage must not be punished for the silence, and a source that
// publishes usage must not be rewarded for a big number it wrote itself. So
// usage can only ever take points away. A listing nobody but its own author
// has paid for is capped below the bar however well it is written, and a
// listing with no usage data is scored on its documentation alone.

/// The bar. A listing scoring below this is not published to discovery.
///
/// Eight of ten means: it describes itself in a real sentence, names its
/// inputs, types them, says which are required, describes its output, names
/// itself, carries tags, is priced in a named asset on a named rail — and, if
/// anybody is counting, has been paid by more than one person this month.
pub const MIN_LISTING_SCORE: u8 = 8;

/// Highest score a listing can reach.
pub const MAX_LISTING_SCORE: u8 = 10;

/// What one listing publishes about itself, and what that was worth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListingScore {
    /// The score after any usage cap. This is what the bar is applied to.
    pub score: u8,
    /// The score its documentation alone earned, before usage was considered.
    /// Equal to `score` unless `capped` is set.
    pub documented: u8,
    /// Why measurement took points off, or `None` when it did not (which
    /// includes every listing whose source publishes no usage at all).
    pub capped: Option<&'static str>,
}

/// Read a listing's own record and score it out of ten.
///
/// `item` is the raw index entry and `accept` its first `accepts` element —
/// both dialects of the x402 discovery envelope are read. Nothing here fetches
/// anything or calls the endpoint: an unreachable endpoint that documents
/// itself perfectly still scores ten, and that is correct, because this
/// measures the LISTING. Whether the thing behind it answers is what
/// `POST /v1/x402/fetch` finds out at purchase time.
pub fn score_listing(item: &Value, accept: Option<&Value>) -> ListingScore {
    let bazaar = item
        .get("extensions")
        .or_else(|| accept.and_then(|a| a.get("extensions")))
        .and_then(|e| e.get("bazaar"));
    let info = bazaar.and_then(|b| b.get("info"));
    let schema = bazaar.and_then(|b| b.get("schema"));

    let mut points = 0u8;

    // --- What it does (3) --------------------------------------------------
    let description = item
        .get("description")
        .and_then(Value::as_str)
        .or_else(|| {
            accept
                .and_then(|a| a.get("description"))
                .and_then(Value::as_str)
        })
        .unwrap_or("")
        .trim();
    let chars = description.chars().count();
    let words = description.split_whitespace().count();
    if chars >= MIN_DESC_CHARS && words >= MIN_DESC_WORDS {
        points += 1;
    }
    if chars >= MIN_DESC_CHARS * 2 && words >= MIN_DESC_WORDS * 2 {
        points += 1;
    }
    // Four times the floor. At this length a listing is not describing itself
    // in a line, it is documenting itself — the best entries in the corpus
    // write their parameters and an example call into the description, which
    // no schema field on either dialect captures.
    if chars >= MIN_DESC_CHARS * 4 && words >= MIN_DESC_WORDS * 4 {
        points += 1;
    }

    // --- What to send (3) --------------------------------------------------
    let inputs = declared_inputs(item, info, schema);
    if inputs.named {
        points += 1;
    }
    if inputs.typed {
        points += 1;
    }
    if inputs.required {
        points += 1;
    }

    // --- What comes back (2) -----------------------------------------------
    let outputs = [
        info.and_then(|i| i.get("output")),
        schema.and_then(|s| s.get("output")),
        item.get("outputSchema"),
    ];
    if outputs.iter().flatten().any(|v| !v.is_null()) {
        points += 1;
    }
    // An output with FIELDS in it. `{"type": "json"}` says the reply is JSON,
    // which is not a description of anything; a `properties` map or a worked
    // example is what lets a caller know whether the answer contains the field
    // it came for.
    if outputs.iter().flatten().any(|v| output_has_shape(v)) {
        points += 1;
    }

    // --- What it costs, and how to call it (2) -----------------------------
    let priced = accept
        .and_then(|a| a.get("amount").or_else(|| a.get("maxAmountRequired")))
        .is_some_and(|v| !v.is_null());
    let asset_named = accept
        .and_then(|a| a.get("extra"))
        .and_then(|e| e.get("name"))
        .and_then(Value::as_str)
        .is_some_and(|n| !n.trim().is_empty());
    if priced && asset_named {
        points += 1;
    }
    let rail_named = accept
        .and_then(|a| a.get("network"))
        .and_then(Value::as_str)
        .is_some_and(|n| !n.trim().is_empty() && !n.eq_ignore_ascii_case("unknown"));
    if rail_named && inputs.method {
        points += 1;
    }

    let documented = points.min(MAX_LISTING_SCORE);
    let (score, capped) = match usage_cap(item) {
        Some((cap, reason)) if documented > cap => (cap, Some(reason)),
        _ => (documented, None),
    };
    ListingScore {
        score,
        documented,
        capped,
    }
}

/// What a listing says about its own inputs, in whichever dialect it says it.
#[derive(Default)]
struct DeclaredInputs {
    /// At least one input is named.
    named: bool,
    /// Every named input carries a type or an enumeration of its values.
    typed: bool,
    /// At least one input is knowably mandatory.
    required: bool,
    /// The verb is stated.
    method: bool,
}

/// Read the input declaration out of every dialect in the corpus.
///
/// This function is the whole reason the rubric is about information rather
/// than about the Bazaar. The first version read only
/// `extensions.bazaar.schema.properties`, so a listing that names its inputs
/// as `inputSchema.queryParams` — which is how one of the two facilitators
/// publishes ALL of them — scored zero for naming its inputs, zero for typing
/// them and zero for saying which are mandatory. That is not a measurement of
/// documentation quality, it is a measurement of which index a seller
/// happened to register with.
///
/// Three shapes are read:
///
/// * a JSON Schema at `extensions.bazaar.schema` — `properties` names the
///   fields, `required` lists the mandatory ones;
/// * a captured call at `inputSchema` or `extensions.bazaar.info.input` —
///   `{type, method, pathParams, queryParams, body, bodyFields}`. The values
///   are examples, but the KEYS are the parameter names, which is the fact
///   being scored. A path parameter is mandatory by construction: the URL
///   cannot be built without it;
/// * field definitions nested inside that captured call — several publishers
///   put `{"type": ..., "description": ..., "enum": [...]}` under `body` or
///   `bodyFields`, which is a real contract wearing the other dialect's
///   clothes.
fn declared_inputs(item: &Value, info: Option<&Value>, schema: Option<&Value>) -> DeclaredInputs {
    let mut out = DeclaredInputs::default();

    // 1. A real JSON Schema.
    if let Some(properties) = schema
        .and_then(|s| s.get("properties"))
        .and_then(Value::as_object)
        .filter(|p| !p.is_empty())
    {
        out.named = true;
        out.typed = properties.values().all(|v| {
            v.get("type")
                .and_then(Value::as_str)
                .is_some_and(|t| !t.is_empty())
        });
        if schema
            .and_then(|s| s.get("required"))
            .and_then(Value::as_array)
            .is_some_and(|r| !r.is_empty())
        {
            out.required = true;
        }
    }

    // 2. A captured call, from either dialect.
    for call in [item.get("inputSchema"), info.and_then(|i| i.get("input"))]
        .into_iter()
        .flatten()
    {
        if call.get("method").and_then(Value::as_str).is_some() {
            out.method = true;
        }
        for slot in ["pathParams", "queryParams", "body", "bodyFields", "headers"] {
            let Some(fields) = call.get(slot).and_then(Value::as_object) else {
                continue;
            };
            if fields.is_empty() {
                continue;
            }
            out.named = true;
            // The URL cannot be built without a path parameter.
            if slot == "pathParams" {
                out.required = true;
            }
            // 3. Field definitions hiding inside the example.
            let definitions: Vec<&Value> = fields.values().filter(|v| v.is_object()).collect();
            if definitions.len() == fields.len()
                && definitions
                    .iter()
                    .all(|d| d.get("type").is_some() || d.get("enum").is_some())
            {
                out.typed = true;
                if definitions
                    .iter()
                    .any(|d| d.get("required").and_then(Value::as_bool) == Some(true))
                {
                    out.required = true;
                }
            }
        }
    }

    if item.get("method").and_then(Value::as_str).is_some() {
        out.method = true;
    }
    out
}

/// True when an output declaration names fields rather than only a media type.
fn output_has_shape(value: &Value) -> bool {
    let Some(map) = value.as_object() else {
        return false;
    };
    if map
        .get("properties")
        .and_then(Value::as_object)
        .is_some_and(|p| !p.is_empty())
    {
        return true;
    }
    match map.get("example") {
        Some(Value::Object(o)) if !o.is_empty() => return true,
        Some(Value::Array(a)) if !a.is_empty() => return true,
        _ => {}
    }
    // Anything beyond a bare `{"type": "json"}`.
    map.keys().any(|k| k != "type")
}

/// Days after which a listing nobody has called reads as abandoned.
const STALE_AFTER_DAYS: i64 = 30;

/// More payers than the person who published it.
const MIN_UNIQUE_PAYERS: u64 = 2;

/// The ceiling published usage puts on a listing, if its source publishes any.
///
/// `None` means "nobody counted", which is never held against a listing — see
/// the module note and `crate::quality::rate`, which says the same thing about
/// catalogue cards.
fn usage_cap(item: &Value) -> Option<(u8, &'static str)> {
    let quality = item.get("quality")?;
    let payers = quality.get("l30DaysUniquePayers").and_then(Value::as_u64)?;
    let stale = quality
        .get("lastCalledAt")
        .and_then(Value::as_str)
        .map(|last| days_since(last).is_none_or(|days| days > STALE_AFTER_DAYS))
        .unwrap_or(true);

    if payers == 0 || stale {
        // Nobody has bought this in a month. Whatever it says about itself,
        // it is not something to hand an agent as an answer.
        Some((4, "nobody paid for it in the last 30 days"))
    } else if payers < MIN_UNIQUE_PAYERS {
        // One payer is consistent with the seller calling its own endpoint,
        // which is how most of this corpus got its single call.
        Some((7, "only one payer in the last 30 days"))
    } else {
        None
    }
}

/// Whole days between an RFC 3339 timestamp and now, or `None` if unparseable.
fn days_since(timestamp: &str) -> Option<i64> {
    let then = chrono::DateTime::parse_from_rfc3339(timestamp).ok()?;
    Some(
        (chrono::Utc::now() - then.with_timezone(&chrono::Utc))
            .num_days()
            .max(0),
    )
}

/// Offers one host contributes to the index before the rest are dropped.
///
/// A cap on the CORPUS, which is a different thing from the caps the sweep and
/// the ranking already apply to an ANSWER. Without it one seller's thousand
/// near-identical endpoints are a thousand rows the ranking has to look at on
/// every search, forever, to return at most a handful of them. Forty is more
/// than any real job needs from one seller and small enough that the largest
/// publisher cannot be a quarter of the corpus.
pub const MAX_PER_HOST: usize = 40;

/// Phrasings to search for one capability.
///
/// The Bazaar's `/search` caps at 20 hits per query (25 returns 400), so the
/// only way past that ceiling is to ask more than once. Measured against the
/// live corpus, the extra phrasings are not redundant: one query returned 10
/// suppliers for reading a web page, four returned 29.
///
/// Deliberately derived from what the capability already says rather than from
/// a hand-written synonym list per capability, so a new capability widens its
/// own search without anyone remembering to add keywords.
pub fn query_variants(
    name: &str,
    description: &str,
    category: Option<&str>,
    slug: &str,
) -> Vec<String> {
    let mut out = vec![name.trim().to_string()];

    // The slug's own words: `web.page-to-text` carries "page to text", which is
    // often phrased differently from the human-readable name.
    let from_slug = slug.replace(['.', '-', '_'], " ");
    if from_slug.to_lowercase() != name.trim().to_lowercase() {
        out.push(from_slug);
    }

    // The first sentence of the description: the domain words live there, and
    // the whole description is too long to match well.
    if let Some(first) = description.split('.').next() {
        let first = first.trim();
        if first.len() > 12 && !first.eq_ignore_ascii_case(name.trim()) {
            out.push(first.to_string());
        }
    }

    if let Some(cat) = category.map(str::trim).filter(|c| !c.is_empty()) {
        out.push(format!("{cat} {}", name.trim()));
    }

    out.retain(|q| !q.trim().is_empty());
    out.dedup();
    out.truncate(4);
    out
}

#[cfg(test)]
mod variant_tests {
    use super::*;

    #[test]
    fn discovery_distinguishes_configured_fetch_prices_from_balance_quotes() {
        let offer = ExternalOffer {
            resource: "https://intel.rallylive.ca/dns/ns-provider".into(),
            host: "intel.rallylive.ca".into(),
            price_usd: Some(0.01),
            ..Default::default()
        };
        // A legacy deployment can still configure 10% or a minimum. Neither
        // may be relabelled as the standard balance quote or silently changed.
        for fetch_price in [0.011, 0.02] {
            let settlement = ExternalSettlement {
                payable: true,
                buyer_price_usd: Some(fetch_price),
                reason: None,
            };
            let value = offer.to_json_settled(&settlement);
            assert_eq!(value["buyer_price_usd"], json!(fetch_price));
            assert_eq!(value["buyer_price_basis"], "indicative_external_fetch_config");
            assert_eq!(value["balance_buyer_price_usd"], json!(0.0115));
            assert_eq!(value["balance_buyer_markup_bps"], 1500);
            assert_eq!(value["price_is_estimate"], true);
            assert!(!value["note"].as_str().unwrap().contains("usual 15%"));
        }
        let dust = ExternalOffer { price_usd: Some(0.000003), ..offer.clone() };
        let value = dust.to_json_settled(&ExternalSettlement { payable: true, buyer_price_usd: Some(0.000004), reason: None });
        assert_eq!(value["balance_buyer_price_usd"], json!(0.000004), "ceil the fee at sub-cent prices");
        let refused = offer.to_json_settled(&ExternalSettlement { reason: Some("Host unavailable".into()), ..Default::default() });
        assert_eq!(refused["settlement"], "direct");
        assert!(refused["balance_buyer_price_usd"].is_null());
        assert_eq!(refused["settlement_reason"], "Host unavailable");
    }

    #[test]
    fn a_capability_generates_several_distinct_phrasings() {
        // One query hits the /search ceiling of 20; four phrasings measured
        // 29 suppliers where one found 10.
        let v = query_variants(
            "Read a web page as text",
            "Fetch one public URL and return its main content as clean text or markdown. For agents that need to read a page.",
            Some("data"),
            "web.page-to-text",
        );
        assert!(v.len() >= 3, "got {v:?}");
        assert!(v.contains(&"Read a web page as text".to_string()));
        // The slug carries phrasing the human name does not.
        assert!(v.iter().any(|q| q.contains("page to text")));
        // The first sentence, not the whole description.
        assert!(v.iter().any(|q| q.starts_with("Fetch one public URL")));
        assert!(!v.iter().any(|q| q.contains("For agents that need")));
    }

    #[test]
    fn variants_are_capped_and_never_empty() {
        let v = query_variants("Name", "A. B. C. D. E.", Some("cat"), "a.b");
        assert!(v.len() <= 4);
        assert!(!v.is_empty());
        assert!(v.iter().all(|q| !q.trim().is_empty()));
    }

    #[test]
    fn a_slug_that_repeats_the_name_is_not_searched_twice() {
        let v = query_variants("weather forecast", "", None, "weather-forecast");
        assert_eq!(
            v.iter()
                .filter(|q| q.to_lowercase() == "weather forecast")
                .count(),
            1
        );
    }

    #[test]
    fn a_capability_with_no_description_still_searches() {
        let v = query_variants("Weather forecast", "", None, "weather.forecast");
        assert!(!v.is_empty());
        assert_eq!(v[0], "Weather forecast");
    }

    #[test]
    fn market_indices_are_not_dice_rollers() {
        // "dice" was a substring test, so every market-data endpoint whose
        // description said "indices" was dropped from discovery as a toy.
        assert!(!looks_like_toy(
            "Real-time crypto prices and market indices"
        ));
        assert!(!looks_like_toy("Jurisdiction and indices lookup"));
        // The toys it is there for still go.
        assert!(looks_like_toy("Roll a dice"));
        assert!(looks_like_toy("DICE roller demo"));
        assert!(looks_like_toy("coinflip"));
        assert!(looks_like_toy("A hello world endpoint"));
    }

    /// A description long enough to clear the floor, so a test about hosts is
    /// about hosts.
    fn real_description() -> String {
        "Returns the official company registration record for one Dutch entity \
         by its eight-digit KVK number, including trade names and addresses."
            .to_string()
    }

    fn gate(resource: &str, description: &str) -> Result<(), Reject> {
        let host = resource
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or("")
            .to_string();
        quality_gate(
            resource,
            &host,
            description,
            Some(0.001),
            Some("eip155:8453"),
        )
    }

    #[test]
    fn a_real_listing_passes() {
        assert_eq!(
            gate("https://api.kvk.nl/v1/basisprofielen", &real_description()),
            Ok(())
        );
    }

    #[test]
    fn nothing_a_stranger_cannot_reach_is_supply() {
        let d = real_description();
        for host in [
            "https://localhost:3000/x",
            "https://127.0.0.1/x",
            "https://192.168.1.44/api",
            "https://my-api.local/x",
            "https://demo.example.com/x",
            "https://a1b2.ngrok-free.app/x",
            "https://tall-cats-sing.trycloudflare.com/x",
        ] {
            assert_eq!(gate(host, &d), Err(Reject::BadHost), "{host}");
        }
    }

    #[test]
    fn free_hosting_tiers_are_hobby_demos_on_this_corpus() {
        let d = real_description();
        for host in [
            "https://my-x402.onrender.com/x",
            "https://thing.vercel.app/x",
            "https://thing.fly.dev/x",
            "https://thing.workers.dev/x",
        ] {
            assert_eq!(gate(host, &d), Err(Reject::EphemeralHost), "{host}");
        }
    }

    #[test]
    fn http_is_not_supply_however_good_the_listing() {
        assert_eq!(
            gate("http://api.kvk.nl/v1/basisprofielen", &real_description()),
            Err(Reject::NotHttps)
        );
    }

    #[test]
    fn a_label_is_not_a_description() {
        // Long enough words, far too few of them.
        assert_eq!(
            gate("https://api.example-provider.io/x", "Weather API"),
            Err(Reject::ThinDescription)
        );
        // Clears the character floor on hyphens alone, and says nothing.
        assert_eq!(
            gate(
                "https://api.example-provider.io/x",
                "get-current-weather-forecast-by-latitude-and-longitude-for-any-city-worldwide"
            ),
            Err(Reject::ThinDescription)
        );
    }

    #[test]
    fn a_placeholder_nobody_replaced_is_not_a_description() {
        for junk in [
            "test",
            "TODO",
            "lorem ipsum dolor sit amet",
            "your description here",
        ] {
            assert!(
                matches!(
                    gate("https://api.example-provider.io/x", junk),
                    Err(Reject::JunkDescription) | Err(Reject::ThinDescription)
                ),
                "{junk}"
            );
        }
    }

    /// The rule that makes the junk list safe: one junk WORD inside a real
    /// sentence is a real sentence. Without this, every endpoint that
    /// mentions a test suite or a demo account is dropped.
    #[test]
    fn a_real_sentence_that_mentions_a_junk_word_survives() {
        assert_eq!(
            gate(
                "https://api.example-provider.io/x",
                "Run a deliverability test against one email address and return \
                 the mailbox provider, the MX records and a confidence score."
            ),
            Ok(())
        );
    }

    #[test]
    fn an_offer_nobody_can_quote_is_not_an_offer() {
        let d = real_description();
        let r = "https://api.example-provider.io/x";
        let h = "api.example-provider.io";
        assert_eq!(
            quality_gate(r, h, &d, None, Some("eip155:8453")),
            Err(Reject::Unpriced)
        );
        assert_eq!(
            quality_gate(r, h, &d, Some(0.0), Some("eip155:8453")),
            Err(Reject::Unpriced)
        );
        assert_eq!(
            quality_gate(r, h, &d, Some(0.001), None),
            Err(Reject::NoNetwork)
        );
        assert_eq!(
            quality_gate(r, h, &d, Some(0.001), Some("unknown")),
            Err(Reject::NoNetwork)
        );
    }

    /// The gate must not undo the one thing `looks_like_toy` was fixed for:
    /// "indices" is not a dice roller.
    #[test]
    fn a_market_data_endpoint_is_not_a_dice_roller() {
        assert_eq!(
            gate(
                "https://api.example-provider.io/indices",
                "Latest closing values for the major equity indices, including \
                 the AEX, the S&P 500 and the Nikkei, with daily percentage change."
            ),
            Ok(())
        );
    }

    /// The `indices` bug, in the host list this time. `.local` as a substring
    /// drops `api.locale.com`; `example.com` as a substring drops
    /// `counterexample.com`. Both are real sellers and both would have been
    /// invisible, which is the expensive direction to be wrong in.
    /// A Bazaar entry that documents itself completely, in the real dialect:
    /// the input JSON Schema sits at `extensions.bazaar.schema`, the call
    /// shape and output example at `extensions.bazaar.info`.
    fn well_documented() -> Value {
        json!({
            "resource": "https://api.example-provider.io/v1/resolve",
            "description": "Resolve an ENS name to its address, its avatar and every text \
                            record published against it, returning the resolver that answered, \
                            whether the answer came from the universal resolver or from the \
                            name's own contract, the coin type of each resolved address and \
                            the content hash with its protocol decoded where one is set, plus \
                            the block height the answer was read at.",
            "extensions": {
                "bazaar": {
                    "schema": {
                        "type": "object",
                        "properties": { "ens": { "type": "string" } },
                        "required": ["ens"]
                    },
                    "info": {
                        "input": { "method": "GET", "queryParams": { "ens": "vitalik.eth" } },
                        "output": { "type": "json", "example": { "address": "0x0", "fullName": "vitalik.eth" } }
                    }
                }
            }
        })
    }

    fn accept() -> Value {
        json!({
            "amount": "1000",
            "network": "eip155:8453",
            "extra": { "name": "USD Coin" }
        })
    }

    #[test]
    fn a_fully_documented_listing_scores_ten() {
        let scored = score_listing(&well_documented(), Some(&accept()));
        assert_eq!(scored.documented, 10, "{scored:?}");
        assert_eq!(scored.capped, None);
        assert!(scored.score >= MIN_LISTING_SCORE);
    }

    /// Each fact the rubric names is worth a point on its own, so removing one
    /// drops the score by exactly one. The three the request named by name —
    /// documentation, required values, the rest of the metadata — are all here.
    #[test]
    fn every_missing_fact_costs_exactly_one_point() {
        let full = score_listing(&well_documented(), Some(&accept())).documented;

        let mut no_required = well_documented();
        no_required["extensions"]["bazaar"]["schema"]
            .as_object_mut()
            .unwrap()
            .remove("required");
        // The query parameter is still named, and a query parameter is not
        // mandatory by construction the way a path parameter is.
        assert_eq!(
            score_listing(&no_required, Some(&accept())).documented,
            full - 1,
            "required values are worth a point"
        );

        let mut untyped = well_documented();
        untyped["extensions"]["bazaar"]["schema"]["properties"]["ens"] = json!({});
        assert_eq!(
            score_listing(&untyped, Some(&accept())).documented,
            full - 1,
            "typing every property is worth a point"
        );

        let mut no_output_shape = well_documented();
        no_output_shape["extensions"]["bazaar"]["info"]["output"] = json!({ "type": "json" });
        assert_eq!(
            score_listing(&no_output_shape, Some(&accept())).documented,
            full - 1,
            "a media type is not a description of the answer"
        );

        let mut no_method = well_documented();
        no_method["extensions"]["bazaar"]["info"]["input"]
            .as_object_mut()
            .unwrap()
            .remove("method");
        assert_eq!(
            score_listing(&no_method, Some(&accept())).documented,
            full - 1,
            "the verb is worth a point"
        );

        let mut terse = well_documented();
        // Long enough to be a real sentence, short of the length at which a
        // listing is documenting itself rather than describing itself: it
        // loses the third description point and keeps the first two.
        terse["description"] = json!(
            "Resolve an ENS name to its address and its avatar record, returning the \
             resolver that answered the query, whether it was the universal resolver \
             or the name's own contract, and the coin type of the address."
        );
        assert_eq!(
            score_listing(&terse, Some(&accept())).documented,
            full - 1,
            "a listing that documents itself at length is worth a point over one that does not"
        );
    }

    #[test]
    fn one_untyped_property_is_enough_to_lose_the_point() {
        let mut item = well_documented();
        item["extensions"]["bazaar"]["schema"]["properties"]["coinType"] = json!({});
        assert_eq!(
            score_listing(&item, Some(&accept())).documented,
            score_listing(&well_documented(), Some(&accept())).documented - 1
        );
    }

    /// The bug this rubric was rewritten for.
    ///
    /// One of the two facilitators publishes EVERY input as a captured call —
    /// `inputSchema.queryParams`, values and all — and never as a JSON Schema.
    /// Scoring only the schema shape meant those listings scored zero for
    /// naming their inputs, zero for typing them and zero for saying which
    /// were mandatory, which measured which index the seller registered with
    /// rather than how well the endpoint is documented.
    #[test]
    fn a_captured_call_names_its_inputs_too() {
        let item = json!({
            "resource": "https://api.example-provider.io/v1/fundamentals",
            "description": "Company fundamentals: comprehensive financial metrics for any stock \
                            ticker, including sector, market capitalisation, the trailing and \
                            forward price to earnings ratios, revenue and net income for the \
                            last four reported quarters, and the reporting currency each figure \
                            is denominated in. Pass the ticker as a query parameter; responses \
                            are cached for five minutes and say so.",
            "method": "POST",
            "inputSchema": {
                "type": "http",
                "method": "POST",
                "queryParams": { "ticker": "AAPL" }
            },
            "outputSchema": { "type": "json", "example": { "name": "Apple Inc.", "sector": "Technology" } }
        });
        let scored = score_listing(&item, Some(&accept()));
        assert!(
            scored.documented >= MIN_LISTING_SCORE,
            "a well-documented captured call must be able to clear the bar: {scored:?}"
        );
    }

    /// A path parameter is mandatory by construction — the URL cannot be built
    /// without it — so it counts as a required value even though the dialect
    /// has no `required` list to put it in.
    #[test]
    fn a_path_parameter_is_required_by_construction() {
        let mut item = json!({
            "resource": "https://api.example-provider.io/v1/gate/:symbol",
            "description": "Pre-trade gate built to be polled: is the tokenised fund tracking \
                            its reference right now, and by how much, with the last on-chain \
                            observation and its age in seconds.",
            "inputSchema": { "type": "http", "method": "GET", "pathParams": { "symbol": "USO" } }
        });
        let with_path = score_listing(&item, Some(&accept())).documented;
        item["inputSchema"]["queryParams"] = item["inputSchema"]["pathParams"].take();
        item["inputSchema"]
            .as_object_mut()
            .unwrap()
            .remove("pathParams");
        let with_query = score_listing(&item, Some(&accept())).documented;
        assert_eq!(
            with_path,
            with_query + 1,
            "a path parameter is required and a query parameter is not"
        );
    }

    /// Field definitions hiding inside the example: several publishers put a
    /// real contract under `bodyFields` in the captured-call dialect.
    #[test]
    fn a_contract_wearing_the_other_dialects_clothes_still_counts() {
        let item = json!({
            "resource": "https://api.example-provider.io/v1/trades",
            "description": "Recent decentralised exchange trades by wallets the provider \
                            classifies as smart money, filtered to one chain, with the token \
                            pair, the direction, the notional in dollars and the block \
                            timestamp of each fill.",
            "method": "POST",
            "inputSchema": {
                "method": "POST",
                "bodyFields": {
                    "chain": { "type": "string", "enum": ["ethereum", "base"], "required": true }
                }
            },
            "outputSchema": { "type": "json", "example": { "trades": [] } }
        });
        let scored = score_listing(&item, Some(&accept()));
        assert!(scored.documented >= MIN_LISTING_SCORE, "{scored:?}");
    }

    /// The house rule, the same one `crate::quality::rate` applies to a
    /// catalogue card: what was measured can only take points away.
    #[test]
    fn measurement_caps_and_never_lifts() {
        let mut dead = well_documented();
        dead["quality"] = json!({
            "l30DaysUniquePayers": 0,
            "l30DaysTotalCalls": 0,
            "lastCalledAt": "2026-01-01T00:00:00.000Z"
        });
        let scored = score_listing(&dead, Some(&accept()));
        assert_eq!(scored.documented, 10, "documentation is unchanged");
        assert_eq!(scored.score, 4, "and the usage caps it well under the bar");
        assert!(scored.capped.is_some());

        let mut alone = well_documented();
        alone["quality"] = json!({
            "l30DaysUniquePayers": 1,
            "l30DaysTotalCalls": 40,
            "lastCalledAt": chrono::Utc::now().to_rfc3339()
        });
        assert_eq!(score_listing(&alone, Some(&accept())).score, 7);

        let mut used = well_documented();
        used["quality"] = json!({
            "l30DaysUniquePayers": 9,
            "l30DaysTotalCalls": 400,
            "lastCalledAt": chrono::Utc::now().to_rfc3339()
        });
        let scored = score_listing(&used, Some(&accept()));
        assert_eq!(
            scored.score, 10,
            "and real use never lifts past the ceiling"
        );
        assert_eq!(scored.capped, None);
    }

    #[test]
    fn no_usage_data_is_never_held_against_a_listing() {
        let scored = score_listing(&well_documented(), Some(&accept()));
        assert_eq!(scored.capped, None);
        assert_eq!(scored.score, scored.documented);
    }

    #[test]
    fn a_thin_listing_scores_near_nothing() {
        let item =
            json!({ "resource": "https://api.example-provider.io/x", "description": "Weather" });
        assert!(score_listing(&item, Some(&accept())).documented <= 2);
    }

    #[test]
    fn a_suffix_is_not_a_substring() {
        let d = real_description();
        for resource in [
            "https://api.locale.com/v1/x",
            "https://counterexample.com/v1/x",
            "https://mylocalhost-data.io/v1/x",
            "https://contest.io/v1/x",
            "https://internal-revenue-data.com/v1/x",
        ] {
            assert_eq!(gate(resource, &d), Ok(()), "{resource}");
        }
    }

    /// `crate::sources::host_of` keeps the port. Without stripping it, every
    /// check here is a name comparison against a string ending in `:443`, so
    /// the ephemeral list — the one thing standing between the corpus and a
    /// few thousand hobby demos — matches nothing at all.
    #[test]
    fn a_port_does_not_smuggle_a_host_past_its_own_list() {
        let d = real_description();
        assert_eq!(
            quality_gate(
                "https://thing.vercel.app:443/x",
                "thing.vercel.app:443",
                &d,
                Some(0.001),
                Some("eip155:8453")
            ),
            Err(Reject::EphemeralHost)
        );
        assert_eq!(
            quality_gate(
                "https://localhost:3000/x",
                "localhost:3000",
                &d,
                Some(0.001),
                Some("eip155:8453")
            ),
            Err(Reject::BadHost)
        );
    }

    #[test]
    fn www_is_not_a_different_seller() {
        let d = real_description();
        assert_eq!(
            quality_gate(
                "https://www.api.example-provider.io/x",
                "www.api.example-provider.io",
                &d,
                Some(0.001),
                Some("eip155:8453")
            ),
            Ok(())
        );
    }
}
