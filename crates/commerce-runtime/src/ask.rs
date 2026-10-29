//! Vraag → antwoordklare endpoints, in één snelle beweging.
//!
//! ```text
//! vraag
//!   │  1. lezen        één modelcall, strikt schema → geparseerd object
//!   ▼
//! geparseerd object
//!   │  2. voorfilteren lexicaal over catalogus + indexen, IN HET GEHEUGEN
//!   ▼
//! shortlist (60)
//!   │  3. herordenen   één modelcall over alleen de shortlist; per pick een `fit`
//!   ▼
//! top 5 endpoints, elk met de capability die het bedient, waarom, en hoe goed
//!   │  4. de lat       haalt de beste `fit` [`MATCH_FLOOR`] niet, dan vraagt het
//!   ▼                  antwoord de steller om context in plaats van te gokken
//! antwoord, of één vervolgvraag
//! ```
//!
//! # Waarom voorfilteren en dan pas een model
//!
//! Duizenden endpoints aan een model voeren kost tokens en tijd, en het meeste
//! ervan is evident irrelevant. Een lexicale score gooit dat in microseconden
//! weg; het model ziet zestig kandidaten in plaats van duizenden en hoeft
//! alleen nog de moeilijke keuze te maken. Dat is waar een model goed in is en
//! een `LIKE` niet.
//!
//! # Wat er doorzocht wordt
//!
//! De catalogus én de open indexen ([`crate::source_index`]) in één pool; zie
//! [`as_offer`] voor hoe een gecrawlde listing dezelfde vorm krijgt.
//!
//! # Waarom alles in het geheugen staat
//!
//! Een paar duizend endpoints, een paar megabyte. Onder die omvang is één keer
//! laden en in het proces zoeken simpelweg sneller dan een SQL-woordmatch per
//! verzoek: geen round trip, geen scan, geen `statement_timeout` om je zorgen
//! over te maken. Boven enkele tienduizenden endpoints wordt dat anders — dan
//! is de eerste stap een echte index, niet een grotere machine.

use std::sync::Arc;

use serde::Serialize;
use serde_json::{json, Value};

use crate::bazaar::ExternalOffer;
use crate::db::CapabilityOffer;

/// Hoeveel kandidaten het model te zien krijgt. Stond op veertig toen de pool
/// alleen de catalogus was, en op zestig toen de open indexen erbij kwamen —
/// maar die indexen kwamen er toen GESNOEID bij: 400 listings per bron in
/// plaats van de 2.490 die de crawl houdt. Nu de hele pool wordt gescoord
/// (~3.600 endpoints) staat zestig voor een kleiner deel van het veld dan het
/// getal ooit bedoelde, dus gaat het mee omhoog.
///
/// Honderdtwintig kandidaten is ongeveer 18k tokens in de herordeningsprompt.
/// Op het model dat die stap draait is dat centen, en het is de enige plek in
/// de keten waar breedte iets kost: de lexicale pas ervoor doet de hele pool
/// in ~0,25 s.
pub const SHORTLIST: usize = 120;

/// Hoeveel tekens van een endpointbeschrijving het model per kandidaat ziet.
///
/// De beschrijving is het gewicht van de herordeningsprompt: honderdtwintig
/// kandidaten à 400 tekens was ~56 kB. De eerste driehonderd tekens dragen
/// wat het endpoint DOET; wat daarna komt is veldenlijst en voorbehoud, en dat
/// leest het model beter uit `requires`. Korter is hier sneller, en snelheid
/// is bij deze stap de hele wedstrijd — zie `config::DEFAULT_ASK_RERANK_TIMEOUT_MS`.
pub const DESCRIPTION_CHARS: usize = 300;

/// Hoeveel er uiteindelijk uit komen.
pub const TOP_N: usize = 5;

/// Onder deze relevantie is er niets plausibels om aan het model voor te leggen.
///
/// # Waarom dit een zeef is en geen oordeel
///
/// Dit was de beslissing zelf: onder de grens meldden we "geen bron". Met de
/// parser en de vertaling erbij houdt die grens geen stand meer. Gemeten op de
/// draaiende gateway, 2026-09-02:
///
/// | vraag                                    | top | juiste uitkomst   |
/// |------------------------------------------|-----|-------------------|
/// | jaarrekening van een nederlands bedrijf  |  91 | KVK               |
/// | wie is de eigenaar van Mollie            |  83 | FilingsPulse      |
/// | sanctions screening for a company        |  81 | CompliancePulse   |
/// | zoek afbeeldingen van een product        |  68 | StableEnrich      |
/// | recept voor appeltaart                   |  42 | GEEN BRON         |
/// | verrijk een persoon met zijn e-mailadres |  39 | StableEnrich      |
///
/// De laatste twee staan in de verkeerde volgorde: een vraag die we kunnen
/// beantwoorden scoort lager dan een vraag die we niet kunnen beantwoorden. Er
/// bestaat dus geen getal dat ze scheidt — niet omdat de grens verkeerd stond,
/// maar omdat een woordtelling het verschil niet kán zien. "verrijk een persoon
/// met zijn e-mailadres" wordt "enrich a person with their email address", en
/// `person`, `their` en `address` staan nergens in een pad terwijl ze wel in de
/// noemer meetellen. Langere vragen worden zo systematisch bestraft.
///
/// Daarom beslist nu het herordeningsmodel, dat de kandidaten echt LEEST: geeft
/// het een lege lijst terug, dan hebben we geen bron. Deze grens doet nog één
/// ding, en dat is goedkoop zijn: is er niets dat ook maar in de buurt komt, dan
/// hoeft er geen modelcall te gebeuren om dat te bevestigen. 25 ligt onder elke
/// gemeten goede vraag en boven een vraag die op nul woorden matcht.
///
/// Een fout antwoord met vijf keurige kaarten eromheen blijft erger dan een lege
/// uitkomst: het eerste kost geld en vertrouwen, het tweede alleen een vraag.
pub const RELEVANCE_FLOOR: u8 = 25;

/// Onder deze `fit` is een antwoord geen antwoord maar een gok, en dan vragen we
/// de steller om context.
///
/// # Waarom een tweede lat, en waarom op de `fit` en niet op de relevantie
///
/// [`RELEVANCE_FLOOR`] is lexicaal en zegt alleen of het de moeite is het model
/// te vragen. De tabel daarboven laat zien dat woordtelling geen oordeel kan
/// zijn: een goede vraag scoort daar 39 en een onbeantwoordbare 42. De `fit`
/// komt van het herordeningsmodel, dat de kandidaten echt LEEST en per pick
/// zegt hoe goed die de vraag beantwoordt ZOALS GESTELD — 90+ is precies de
/// gevraagde data voor de gevraagde entiteit, 50–74 is verwante data of het
/// juiste soort data voor een andere scope (ander land, ander register).
///
/// 75 is de grens tussen "dit beantwoordt de kern" en "dit komt in de buurt".
/// Daaronder is de eerlijke reactie niet vijf kaarten met een lage score,
/// maar één vraag terug: welk land, welk soort gegevens, welke identificatie.
/// De kaarten blijven wél in het antwoord staan — wie de zwakke keuze toch wil,
/// kan hem zien en pakken — maar `meta.confident` is dan `false` en
/// `needs_context` draagt de vraag.
///
/// ZONDER MODEL GELDT DEZE LAT NIET, en dat was hier lang anders. Dezelfde 75
/// werd dan tegen de lexicale relevantie gehouden — op een schaal waar een
/// goede vraag 39 haalt, dus in de praktijk onhaalbaar. Gemeten: het KVK-
/// register scoort op "Where is Mollie B.V. registered?" fit 90 en relevantie
/// 59. Eén time-out op de herordening maakte van dat antwoord een `weak_match`
/// met een contextvraag erbij, terwijl er aan de vraag niets mankeerde.
///
/// Draaide er geen model, dan zegt `match_scored_by` "lexical" en blijft
/// `meta.confident` `false` — we hebben niet geoordeeld, dus we beweren niet
/// dat het goed is. Maar er wordt geen context gevraagd: dat zou onze eigen
/// onzekerheid presenteren als een gebrek in de vraag van de steller. Een
/// aanroeper kan strenger of soepeler zijn met `min_match`.
pub const MATCH_FLOOR: u8 = 75;

/// Woorden die niets onderscheiden. Zonder deze lijst scoort elk endpoint op
/// "the" en wordt de shortlist willekeurig.
const STOPWORDS: &[&str] = &[
    "the", "a", "an", "of", "for", "and", "or", "to", "in", "on", "with", "by", "from", "is",
    "are", "what", "which", "who", "how", "me", "my", "get", "find", "show", "api", "de", "het",
    "een", "van", "voor", "met", "en", "of", "is", "zijn", "wat", "welke", "wie", "hoe", "mij",
    "geef", "zoek", "toon",
];

/// Eén kandidaat zoals het model hem te zien krijgt.
#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    /// Index in de shortlist. Het model verwijst hiernaar, niet naar een UUID —
    /// een kort getal is moeilijker te verhaspelen en scheelt tokens.
    pub id: usize,
    pub resource: String,
    pub description: String,
    pub price: String,
    pub provider: String,
    pub capability: String,
    /// De velden die dit endpoint verplicht stelt, uit zijn eigen invoerschema.
    ///
    /// Het model krijgt ze mee omdat "past bij de vraag" en "is uit te voeren"
    /// twee verschillende dingen zijn. Een endpoint dat een `domain` eist terwijl
    /// de vraag alleen een bedrijfsnaam noemt, is de juiste keuze én nog niet
    /// aanroepbaar; zonder deze lijst kan het model dat verschil niet zien en
    /// eindigt het verschil bij een betaalde 400.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    /// 0–100. Hoeveel van de vraag dit endpoint letterlijk terugkaatst.
    ///
    /// NIET naar het model. Gemeten op 2026-09-02: met dit getal in de
    /// kandidaat gaf Haiku bij "sanctions screening for a company" een `fit`
    /// van 74, 73 en 65 — exact de drie relevantiegetallen die het te zien
    /// kreeg. Een woordtelling als anker maakt het oordeel een echo van de
    /// woordtelling, en dan is de tweede maat geen tweede maat.
    #[serde(skip_serializing)]
    pub relevance: u8,
}

/// De verplichte velden van een endpoint, uit ALLEBEI zijn invoerschema's.
///
/// # Waarom dit meer is dan één veld lezen
///
/// De catalogus houdt invoer op twee plaatsen — `request_body` voor een POST,
/// `query_params` voor een GET — en 1029 van de 1122 endpoints zijn GET. Alleen
/// de body lezen leverde verplichte velden op bij 54 endpoints en bij KVK,
/// CityFalcon en PulseNetwork helemaal niets. Dat was een uitspraak over de
/// kolom, niet over de API's: `query_params` is gevuld voor 999 endpoints, en
/// maar 31 endpoints in de hele catalogus documenteren geen enkele invoer.
///
/// # De drie vormen
///
/// Ze zijn alle drie echt aanwezig, want ze komen van drie soorten bronnen —
/// een OpenAPI-import, een handgeschreven schema en een llms.txt-parse:
///
/// ```text
///   A  JSON Schema      {"type":"object","required":["chain_id"],"properties":{…}}
///   B  vlag per veld    {"properties":{"name":{"required":true,…}}}
///   C  platte map       {"kvkNummer":{"required":true,…},"geoData":{…}}   ← KVK
/// ```
///
/// C heeft geen omhulsel: de eigenschappen staan in de wortel, náást metavelden
/// als `type` en `$schema`. Vandaar de eis dat een kandidaat een OBJECT is met
/// `required: true` — `"type": "object"` is een string en valt daarmee vanzelf
/// af, zonder een lijst metaveldnamen die bij de volgende bron alweer incompleet
/// is.
pub fn required_inputs(offer: &CapabilityOffer) -> Vec<String> {
    let mut out = Vec::new();
    collect_required(&offer.request_body, &mut out);
    collect_required(&offer.query_params, &mut out);

    out.sort();
    out.dedup();
    out.truncate(12);
    out
}

/// Voegt de verplichte veldnamen uit één schema toe aan `out`.
fn collect_required(schema: &Value, out: &mut Vec<String>) {
    // Vorm A: de expliciete lijst.
    if let Some(names) = schema.get("required").and_then(Value::as_array) {
        out.extend(names.iter().filter_map(|v| v.as_str()).map(str::to_string));
    }

    // Vorm B en C: de vlag per veld. `properties` wanneer die er is, anders is
    // de wortel zelf de map.
    let map = schema
        .get("properties")
        .and_then(Value::as_object)
        .or_else(|| schema.as_object());

    let Some(map) = map else { return };
    for (name, field) in map {
        if field.get("required") == Some(&Value::Bool(true)) {
            out.push(name.clone());
        }
    }
}

/// Wat er uiteindelijk uit komt.
#[derive(Debug, Clone, Serialize)]
pub struct Selected {
    pub resource: String,
    pub description: String,
    pub price: String,
    pub provider: String,
    pub capability: String,
    /// Eén zin over waarom dit endpoint past. Van het model.
    pub reason: String,
    /// De verplichte velden van dit endpoint. Meegegeven zodat een aanroeper
    /// zelf kan zien of hij het kan uitvoeren, ook zonder de vervolgvraag.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    /// 0–100, dezelfde maat als [`RELEVANCE_FLOOR`]. Gaat mee naar buiten zodat
    /// een aanroeper zelf strenger kan zijn dan wij, en zodat een zwakke keuze
    /// zichtbaar zwak is in plaats van net zo zelfverzekerd te ogen als een sterke.
    pub relevance: u8,
    /// 0–100, het oordeel van het herordeningsmodel: hoe goed dit endpoint de
    /// vraag beantwoordt zoals gesteld. Dit is de maat waar [`MATCH_FLOOR`]
    /// tegen gehouden wordt. `None` wanneer geen model gerangschikt heeft —
    /// dan is `relevance` het enige getal, en zegt het antwoord dat erbij.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fit: Option<u8>,
    /// Voor de vervolgstap: `/v1/do`, `/v1/compare`, `/v1/decide`.
    pub candidate_id: Option<uuid::Uuid>,
    /// De uitvoerbare kandidaat in exact dezelfde vorm als `/v1/discover`.
    ///
    /// `/v1/ask` is de enige plek die beslist WELK endpoint past. De App en
    /// agent-gateway mogen daarna niet opnieuw discoveren en zelf een andere
    /// winnaar kiezen; zij hebben wel prijs, methode en invoerschema nodig om
    /// de gekozen call te tonen en uit te voeren. Daarom reist die bestaande
    /// candidate-vorm mee met het semantische oordeel.
    pub candidate: Value,
}

// ---------------------------------------------------------------------------
// De open indexen, in dezelfde vorm als de catalogus
// ---------------------------------------------------------------------------

/// Uit een open index in plaats van uit de catalogus: een catalogusrij heeft
/// altijd een `endpoint_id`, een gecrawlde listing staat niet in onze database.
pub fn is_external(offer: &CapabilityOffer) -> bool {
    offer.endpoint_id.is_none()
}

/// Een gecrawlde listing als catalogusrij, zodat één zeef en één modelcall over
/// allebei beslissen. Zonder invoerschema: de crawler bewaart het niet.
pub fn as_offer(external: &ExternalOffer) -> CapabilityOffer {
    let host = external.host.trim().to_ascii_lowercase();
    let provider = external
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or(&host)
        .to_string();

    CapabilityOffer {
        // Niemand heeft deze listing tegen een contract gelegd.
        capability_slug: String::new(),
        endpoint_id: None,
        api_slug: host.clone(),
        api_name: provider.clone(),
        endpoint_method: external
            .method
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .unwrap_or("GET")
            .to_ascii_uppercase(),
        endpoint_path: resource_path(&external.resource),
        endpoint_description: Some(listing_text(external)),
        price_usd: external.price_usd.unwrap_or(0.0),
        payment_required: external.price_usd.is_some_and(|p| p > 0.0),
        // Fee-klasse `federated`: lijst + 15%, zoals `/v1/x402/fetch` afrekent.
        hosted_externally: true,
        call_url: Some(external.resource.clone()),
        input_map: json!({}),
        fixed_params: json!({}),
        request_body: json!({}),
        query_params: json!({}),
        // The rail this listing settles on, carried on the one free text field
        // a catalogue row has; `candidate_value` publishes it as `network`.
        notes: external.network.clone(),
        source: external.source.clone(),
        confidence: 0.0,
        provider_name: Some(provider),
        provider_username: None,
        // None, not empty: a crawled listing's category and tags are already
        // folded into the description by `listing_text`, and counting the same
        // words twice would weigh an external row against a catalogue row on a
        // difference in bookkeeping rather than in content.
        search_text: None,
    }
}

/// Het pad van een resource-URL: dat is wat [`score`] dubbel weegt, en
/// `https://` is dat niet.
fn resource_path(resource: &str) -> String {
    url::Url::parse(resource)
        .ok()
        .map(|u| u.path().to_string())
        .filter(|p| !p.is_empty() && p != "/")
        .unwrap_or_else(|| resource.to_string())
}

/// De woorden waarop een externe listing gevonden mag worden. Categorie en tags
/// erbij, want de crawl indexeert daar ook op.
fn listing_text(external: &ExternalOffer) -> String {
    let mut text = external.description.trim().to_string();
    let extra: Vec<&str> = external
        .category
        .as_deref()
        .into_iter()
        .chain(external.tags.iter().map(String::as_str))
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();
    if !extra.is_empty() {
        if !text.is_empty() {
            text.push_str(" — ");
        }
        text.push_str(&extra.join(", "));
    }
    text
}

/// De kandidaatvorm die bij deze rij hoort: één plek die weet dat catalogus en
/// index niet hetzelfde afrekenen.
fn candidate_value(offer: &CapabilityOffer) -> Value {
    if !is_external(offer) {
        return crate::v1_routes::candidate_json(offer, false, published_schema(offer));
    }
    json!({
        "candidate_id": Value::Null,
        "kind": "external",
        "provider": offer.api_name,
        "api_slug": offer.api_slug,
        "host": offer.api_slug,
        "capability": Value::Null,
        "description": offer.endpoint_description,
        "method": offer.endpoint_method,
        "path": offer.endpoint_path,
        "resource": offer.call_url,
        "indicative_price_usd": crate::v1_routes::buyer_price_usd(offer),
        "provider_price_usd": offer.price_usd,
        "availability": "callable",
        // Apiosk fronts the provider's own 402; nobody reviewed this endpoint.
        "settlement": "apiosk-x402",
        "network": offer.notes,
        "source": offer.source,
        "measured": false,
        "reviewed": false,
    })
}

fn published_schema(offer: &CapabilityOffer) -> Option<Value> {
    for schema in [&offer.query_params, &offer.request_body] {
        if schema.as_object().is_some_and(|object| !object.is_empty()) {
            return Some(schema.clone());
        }
    }
    None
}

/// Splitst tekst in vergelijkbare termen.
pub fn terms(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '.')
        .filter(|t| t.len() > 2 && !STOPWORDS.contains(t))
        .map(str::to_string)
        .collect()
}

/// The two haystacks one endpoint is matched against, lowercased once.
///
/// The path was CHOSEN and the description was WRITTEN, which is why a path hit
/// counts double in [`weighted_score`]. Split out so the rarity pass and the
/// scoring pass read exactly the same text — a term counted as common in one
/// and matched in the other would weigh itself against the wrong denominator.
fn haystacks(offer: &CapabilityOffer) -> (String, String) {
    let path = format!(
        "{} {} {}",
        offer.api_slug, offer.endpoint_path, offer.capability_slug
    )
    .to_lowercase();
    // Description PLUS the endpoint's own search metadata, which is written to
    // be matched rather than read: `search_terms` carries the buyer's wording
    // next to the provider's — "company VAT number" against
    // "BTW-identificatie" — and no amount of substring matching on provider
    // prose bridges that on its own.
    //
    // It shares the prose weight rather than earning its own, because it is the
    // same KIND of signal: written about the endpoint, not chosen for it. The
    // path keeps its double weight for exactly that reason.
    //
    // NULL on every pre-rework query, so with ASK_ENGINE_V2 off this line adds
    // nothing and no score moves.
    let mut prose = offer
        .endpoint_description
        .as_deref()
        .unwrap_or_default()
        .to_lowercase();
    if let Some(extra) = offer.search_text.as_deref() {
        prose.push(' ');
        prose.push_str(&extra.to_lowercase());
    }
    (path, prose)
}

/// Een term die bijna nergens staat is bijna nooit ruis; onbegrensd laten
/// oplopen maakt hem wél een loterij, want één zeldzaam woord zou dan in zijn
/// eentje de hele shortlist bepalen.
const MAX_WEIGHT: f64 = 6.0;

/// Een term die overal staat onderscheidt niets, maar is ook geen stopwoord:
/// hij mag een gelijkspel nog breken.
const MIN_WEIGHT: f64 = 0.2;

/// Wat elke zoekterm waard is, gegeven hoe algemeen hij is in de pool waarin
/// gezocht wordt.
///
/// # Waarom lengte alleen niet volstaat
///
/// [`weighted_score`] woog een term op zijn LENGTE: "sanctions" zegt meer dan
/// "list". Dat klopt gemiddeld en faalt precies waar het pijn doet, want lengte
/// en zeldzaamheid zijn niet hetzelfde. Gemeten op de draaiende gateway,
/// 2026-09-04, `q=validate an IBAN`:
///
/// | # | endpoint                          | waarom daar             |
/// |---|-----------------------------------|-------------------------|
/// | 0 | `agents.ai-rook.com/api/email-validate` | `validate` = 16 punten |
/// | 1 | `webbersites/api/email/verify`    | idem                    |
/// | 2 | `stableenrich/.../validate-email` | idem                    |
/// | 3 | `mailcheck.hugen.tokyo/validate`  | idem                    |
/// | 4 | `api.kadec0.xyz/v1/email-validate`| idem                    |
/// | 5 | `chainverdict.xyz/v1/validate/iban/:iban` | `iban` = 8 punten |
///
/// Vijf e-mailvalidators boven de ENIGE IBAN-validator, omdat `validate` acht
/// letters heeft en `iban` er vier. Bij een korte vraag redt het
/// herordeningsmodel dat nog; bij een lange vraag ("check whether this IBAN
/// belongs to the company name on the invoice") halen `company` en `invoice`
/// genoeg generieke treffers op om `chainverdict` buiten de zestig te duwen, en
/// dan ziet het model hem nooit.
///
/// De ontbrekende maat is hoe ZELDZAAM een woord is. `validate` staat in
/// honderden endpoints en zegt daarom weinig; `iban` staat in een handvol en
/// zegt daarom veel. Dat is klassieke idf, hier `ln(n / df)`.
///
/// # Waarom de schaal er niet toe doet, en de verhouding wel
///
/// [`relevance_weighted`] deelt de score door [`weighted_max`], en beide dragen
/// dezelfde gewichten. Een gemeenschappelijke factor valt dus weg: alleen de
/// VERHOUDING tussen termen verandert de uitkomst, niet de eenheid waarin ze
/// staan. Daarom blijft [`RELEVANCE_FLOOR`] betekenen wat hij betekende.
///
/// # Een woord dat de pool nog nooit gezien heeft, weegt niets
///
/// `df == 0` krijgt gewicht nul in plaats van het maximum, en dat is de tweede
/// helft van de reparatie. De kop van [`RELEVANCE_FLOOR`] beschrijft het
/// probleem al: "verrijk een persoon met zijn e-mailadres" wordt "enrich a
/// person with their email address", en `person`, `their` en `address` staan
/// nergens in een pad terwijl ze wel in de noemer meetellen — zo worden lange
/// vragen systematisch bestraft. Een woord dat de hele catalogus niet kent, is
/// geen meetlat voor de kandidaten in die catalogus; het valt hier uit de
/// noemer en de vraag wordt gemeten op de woorden die er wél toe doen.
pub struct Weights {
    by_term: std::collections::HashMap<String, f64>,
}

impl Weights {
    /// Elke term even zwaar: precies het gedrag van vóór deze weging.
    ///
    /// Dit is wat [`score`] en [`max_score`] gebruiken, zodat de betekenis die
    /// hun tests vastleggen ("een padtreffer telt dubbel", "langere termen
    /// wegen meer") onveranderd blijft gelden.
    pub fn uniform() -> Self {
        Self {
            by_term: std::collections::HashMap::new(),
        }
    }

    /// De gewichten zoals deze pool ze dicteert.
    ///
    /// Eén doorloop over de pool, dezelfde haystacks als de scoring. Bij ~3.600
    /// endpoints en een handvol termen zijn dat enkele tienduizenden
    /// substring-checks: verwaarloosbaar naast de twee modelcalls eromheen.
    pub fn over(offers: &[CapabilityOffer], terms: &Terms) -> Self {
        let all = terms.all();
        if offers.is_empty() || all.is_empty() {
            return Self::uniform();
        }

        let mut hits: Vec<usize> = vec![0; all.len()];
        for offer in offers {
            let (path, prose) = haystacks(offer);
            for (i, term) in all.iter().enumerate() {
                if path.contains(term.as_str()) || prose.contains(term.as_str()) {
                    hits[i] += 1;
                }
            }
        }

        let n = offers.len() as f64;
        let by_term: std::collections::HashMap<String, f64> = all
            .into_iter()
            .zip(hits)
            .map(|(term, df)| {
                let weight = if df == 0 {
                    0.0
                } else {
                    (n / df as f64).ln().clamp(MIN_WEIGHT, MAX_WEIGHT)
                };
                (term, weight)
            })
            .collect();

        // Alles nul is geen weging maar een lege meetlat: dan valt elke score
        // en elke noemer weg en zou elk endpoint relevantie 0 krijgen. Dat kan
        // alleen als geen enkele term ergens voorkomt, en dan is gelijk wegen
        // net zo goed en een stuk voorspelbaarder.
        if by_term.values().all(|w| *w <= 0.0) {
            return Self::uniform();
        }
        Self { by_term }
    }

    /// Onbekende termen wegen één: een gewicht dat nooit is gemeten mag een
    /// term niet stilletjes laten verdwijnen.
    fn of(&self, term: &str) -> f64 {
        if self.by_term.is_empty() {
            return 1.0;
        }
        self.by_term.get(term).copied().unwrap_or(1.0)
    }
}

/// De hoogst haalbare score voor deze termen: elke term in het pad gevonden.
///
/// Bestaat zodat een score een BETEKENIS heeft in plaats van een optelsom te
/// zijn. Zonder deze noemer is 30 punten veel bij twee woorden en weinig bij
/// tien, en dan kun je er geen ondergrens op zetten.
pub fn max_score(terms: &[String]) -> usize {
    weighted_max(terms, &Weights::uniform()).round() as usize
}

/// [`max_score`] met de gewichten van de pool erin.
pub fn weighted_max(terms: &[String], weights: &Weights) -> f64 {
    terms
        .iter()
        .map(|t| t.len() as f64 * 2.0 * weights.of(t))
        .sum()
}

/// Relevantie van 0 tot 100, gemeten op de woorden van de VRAAG.
///
/// 100 betekent: elk woord uit de vraag komt terug in het pad van dit endpoint.
/// Dat haalt vrijwel niets, en dat hoort ook niet — het is de bovengrens, niet
/// de verwachting. Zie [`RELEVANCE_FLOOR`] voor waar de streep ligt.
pub fn relevance(offer: &CapabilityOffer, terms: &[String]) -> u8 {
    relevance_weighted(offer, terms, &Weights::uniform())
}

/// [`relevance`] met de gewichten van de pool erin.
pub fn relevance_weighted(offer: &CapabilityOffer, terms: &[String], weights: &Weights) -> u8 {
    let max = weighted_max(terms, weights);
    if max <= 0.0 {
        return 0;
    }
    let pct = weighted_score(offer, terms, weights) * 100.0 / max;
    pct.clamp(0.0, 100.0).round() as u8
}

/// Hoeveel een treffer op een parser-term er bovenop mag doen.
///
/// Begrensd, want een uitbreiding is een gok van het model over wat de vraag
/// BEDOELDE. Hij mag een goede kandidaat over de streep helpen, niet een
/// slechte erin praten.
const EXPANSION_BONUS: u8 = 20;

/// Relevantie met de parser-termen als bonus in plaats van als verdunning.
pub fn relevance_with(offer: &CapabilityOffer, terms: &Terms) -> u8 {
    relevance_with_weights(offer, terms, &Weights::uniform())
}

/// [`relevance_with`] met de gewichten van de pool erin.
///
/// De bonus telt TREFFERS en geen gewicht: een uitbreiding is een gok van het
/// model over wat de vraag bedoelde, en die gok hoort niet zwaarder te wegen
/// omdat hij toevallig een zeldzaam woord opleverde.
pub fn relevance_with_weights(offer: &CapabilityOffer, terms: &Terms, weights: &Weights) -> u8 {
    let base = relevance_weighted(offer, &terms.core, weights);
    if terms.expansion.is_empty() {
        return base;
    }
    let hits = terms
        .expansion
        .iter()
        .filter(|t| score(offer, std::slice::from_ref(*t)) > 0)
        .count();
    let bonus = (EXPANSION_BONUS as usize * hits / terms.expansion.len()) as u8;
    base.saturating_add(bonus).min(100)
}

/// Lexicale relevantie van één endpoint voor een set termen.
///
/// Langere termen wegen zwaarder: "sanctions" zegt meer dan "list". Een treffer
/// in het pad telt dubbel — een pad is gekozen, een omschrijving is geschreven,
/// en het eerste is daarmee het scherpere signaal.
pub fn score(offer: &CapabilityOffer, terms: &[String]) -> usize {
    weighted_score(offer, terms, &Weights::uniform()).round() as usize
}

/// [`score`] met de gewichten van de pool erin. Met [`Weights::uniform`] is dit
/// tot op het gehele getal nauwkeurig dezelfde som als vóór de weging.
pub fn weighted_score(offer: &CapabilityOffer, terms: &[String], weights: &Weights) -> f64 {
    let (path, prose) = haystacks(offer);

    let mut total = 0.0;
    for term in terms {
        let weight = weights.of(term);
        if path.contains(term.as_str()) {
            total += term.len() as f64 * 2.0 * weight;
        } else if prose.contains(term.as_str()) {
            total += term.len() as f64 * weight;
        }
    }
    total
}

/// De termen waarop gezocht wordt, uit het geparseerde object plus de vraag zelf.
///
/// De ruwe vraag gaat mee omdat het model soms netjes normaliseert ("Mollie
/// B.V.") terwijl de catalogus het woord uit de vraag bevat ("mollie").
pub struct Terms {
    /// De woorden van de vraag zelf. Hierop wordt de relevantie gemeten.
    pub core: Vec<String>,
    /// Wat de parser eraan toevoegde: capabilities, entiteitsnamen, gevraagde
    /// uitvoer. Deze mogen de score alleen VERHOGEN.
    pub expansion: Vec<String>,
}

impl Terms {
    /// Alles bij elkaar, voor het voorfilteren — daar wil je juist de breedte.
    pub fn all(&self) -> Vec<String> {
        let mut out = self.core.clone();
        out.extend(self.expansion.iter().cloned());
        out.sort();
        out.dedup();
        out
    }
}

/// De zoektermen, gesplitst in wat er gevraagd is en wat de parser erbij vond.
///
/// DE SPLITSING IS HET PUNT. Relevantie was "welk deel van alle termen komt
/// terug", en toen de parser er capabilities en entiteitsnamen bij zette, ZAKTE
/// de score van precies de vragen die beter begrepen werden: "dutch company
/// annual accounts" ging van 73 naar 36 en viel onder de drempel. Een rijkere
/// lezing werd zo bestraft, wat de omgekeerde wereld is.
///
/// Nu meet de kern (de vraag) de score, en kan de uitbreiding hem alleen
/// omhoog duwen.
///
/// # Waarom de kern drie dingen bevat en niet alleen de vraag
///
/// De catalogus is Engels; de vraag hoeft dat niet te zijn. "wie is de eigenaar
/// van Mollie" levert kernwoorden `eigenaar` en `mollie` op, en die staan in
/// geen enkel pad — de vraag scoorde 15 en werd afgewezen terwijl FilingsPulse
/// letterlijk `/api/filings/ownership` aanbiedt. Elke Nederlandse vraag liep zo
/// vast, en een gebonden bonus tilt een score van nul niet over de drempel.
///
/// De kern is daarom DEZELFDE VRAAG, drie keer gesteld:
///
///   * `question.text_en` — de vraag in het Engels, door de parser vertaald.
///     Dit is wat de catalogus kan lezen, en het VERVANGT de oorspronkelijke
///     woorden; zie de functie zelf voor waarom erbij zetten niet werkt.
///     Eigennamen overleven de vertaling, dus "Mollie" blijft een kernwoord.
///   * `expected_output` — welke gegevens de steller terug wil ("ownership"),
///     wat een vraag in één woord is.
///
/// Allebei zijn een herformulering van de vraag zelf. Wat de parser eróver
/// concludeert — capability-slugs, entiteitsnamen, de intent — blijft
/// uitbreiding: dat zijn gevolgtrekkingen, en die mogen de meetlat niet worden.
///
/// Het onderscheid houdt de ondergrens werken: "recept voor appeltaart" vertaalt
/// naar "recipe for apple pie" en wil `recipe`, en geen van die woorden komt in
/// de catalogus voor. De vraag zakt dus nog steeds door de drempel — wat klopt.
pub fn search_terms(question: &str, parsed: &Value) -> Terms {
    // De vertaling VERVANGT de vraag, ze komt er niet bij.
    //
    // Erbij zetten leek veiliger en was het niet: `eigenaar` en `wie` kunnen in
    // een Engelse catalogus nooit matchen, maar ze staan wél in de noemer. Met
    // beide talen erin haalde "wie is de eigenaar van Mollie" 33 in plaats van
    // 47 — dood gewicht dat precies de vragen afwijst die goed vertaald zijn.
    //
    // Voor een Engelse vraag is `text_en` het origineel, letterlijk overgenomen,
    // dus daar verandert er niets. Eigennamen overleven de vertaling omdat de
    // prompt daar expliciet om vraagt: "Mollie" blijft "Mollie".
    let mut core = parsed
        .pointer("/question/text_en")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(terms)
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| terms(question));
    let mut text = String::new();

    if let Some(v) = parsed.pointer("/question/intent").and_then(Value::as_str) {
        text.push(' ');
        text.push_str(v);
    }
    if let Some(steps) = parsed.get("execution_plan").and_then(Value::as_array) {
        for step in steps {
            for key in ["capability", "provider"] {
                if let Some(v) = step.get(key).and_then(Value::as_str) {
                    text.push(' ');
                    text.push_str(v);
                }
            }
        }
    }
    if let Some(entities) = parsed.get("resolved_entities").and_then(Value::as_array) {
        for e in entities {
            for key in ["type", "name", "domain"] {
                if let Some(v) = e.get(key).and_then(Value::as_str) {
                    text.push(' ');
                    text.push_str(v);
                }
            }
        }
    }
    // Wat de steller terug wil, in de taal van de catalogus. Naar de KERN, niet
    // naar de uitbreiding — zie de kop van deze functie.
    //
    // Een lijst sinds strict mode: "any keys" is daar niet uit te drukken.
    // Beide vormen worden gelezen, zodat een oud geparseerd object niet stil
    // zijn zoektermen verliest.
    let mut wanted = String::new();
    match parsed.get("expected_output") {
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(s) = item.as_str() {
                    wanted.push(' ');
                    wanted.push_str(s);
                }
            }
        }
        Some(Value::Object(map)) => {
            for key in map.keys() {
                wanted.push(' ');
                wanted.push_str(key);
            }
        }
        _ => {}
    }
    for t in terms(&wanted) {
        if !core.contains(&t) {
            core.push(t);
        }
    }

    let mut expansion = terms(&text);
    expansion.retain(|t| !core.contains(t));
    expansion.sort();
    expansion.dedup();

    Terms { core, expansion }
}

/// Prijs als leesbare tekst. Gratis is een echt antwoord, geen ontbrekend getal.
fn price_label(offer: &CapabilityOffer) -> String {
    if !offer.payment_required || offer.price_usd <= 0.0 {
        return "free".to_string();
    }
    // Vier decimalen, zonder nutteloze nullen: $0.01 en niet $0.0100.
    let s = format!("{:.4}", offer.price_usd);
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    format!("${trimmed}")
}

/// De hoogste relevantie in de shortlist.
///
/// MAXIMUM, niet de eerste. De shortlist staat gesorteerd op ALLE termen —
/// kern plus wat de parser erbij bedacht — terwijl de relevantie alleen de
/// kernwoorden van de vraag meet. Dat zijn twee verschillende ordeningen, dus de
/// bovenste kandidaat is niet noodzakelijk de meest relevante.
///
/// Dit getal beslist of we zeggen dat we een bron hebben (zie
/// [`RELEVANCE_FLOOR`]). Op de eerste kandidaat kijken wees daarom vragen af die
/// we gewoon konden beantwoorden: "insider trading for a us stock" gaf 20 en
/// werd afgewezen, terwijl CityFalcons `/insider_transactions` verderop in
/// dezelfde lijst stond. Het maximum kan de vraag onterecht toelaten waar de
/// eerste hem onterecht afwees, en van die twee is alleen de tweede stil: een
/// zwakke kandidaat is zichtbaar zwak, een afgewezen vraag ziet eruit alsof we
/// niets hebben.
pub fn best_relevance(pairs: &[(Candidate, &CapabilityOffer)]) -> u8 {
    pairs.iter().map(|(c, _)| c.relevance).max().unwrap_or(0)
}

/// De shortlist plus hoe groot het veld was waar hij uit komt.
pub struct Shortlist<'a> {
    pub pairs: Vec<(Candidate, &'a CapabilityOffer)>,
    /// Treffers vóór de afkapping. `pairs.len()` is bijna altijd `limit` en
    /// zegt daarom bij elke vraag hetzelfde; dit getal beweegt mee.
    pub matched: usize,
}

/// Bouwt de shortlist: score, sorteer, kap af.
///
/// Bij een gelijke score wint het goedkoopste endpoint, en daarna het pad —
/// zodat dezelfde vraag altijd dezelfde volgorde geeft en niet die van de
/// database.
pub fn shortlist_with_matches<'a>(
    offers: &'a [CapabilityOffer],
    terms: &Terms,
    weights: &Weights,
    limit: usize,
) -> Shortlist<'a> {
    // Voorfilteren op ALLE termen: daar wil je juist de breedte van de parser.
    // De score verderop meet alleen de kern.
    let all = terms.all();
    // Duizendsten, zodat een gewogen score een GEHEEL getal blijft en de
    // sortering deterministisch is: `f64` sorteren vraagt om `partial_cmp` en
    // geeft bij een gelijkspel niet elke keer dezelfde volgorde terug.
    let mut scored: Vec<(u64, &CapabilityOffer)> = offers
        .iter()
        .map(|o| {
            (
                (weighted_score(o, &all, weights) * 1_000.0).round() as u64,
                o,
            )
        })
        .filter(|(s, _)| *s > 0)
        .collect();

    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(
                a.1.price_usd
                    .partial_cmp(&b.1.price_usd)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(a.1.endpoint_path.cmp(&b.1.endpoint_path))
    });

    // Eén endpoint, één plek in de lijst.
    //
    // Sinds elk endpoint aan meerdere capabilities gekoppeld kan zijn, levert de
    // catalogus meerdere rijen voor dezelfde aanroep. Zonder deze stap vult
    // hetzelfde KVK-endpoint drie van de vijf plaatsen en lijkt het antwoord
    // rijker dan het is. De hoogst scorende rij wint, dus de capability die het
    // best bij de vraag past blijft staan.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    scored.retain(|(_, o)| {
        let key = o
            .endpoint_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| format!("{} {}", o.endpoint_method, o.endpoint_path));
        seen.insert(key)
    });

    let matched = scored.len();
    let pairs = scored
        .into_iter()
        .take(limit)
        .enumerate()
        .map(|(id, (_, o))| {
            (
                Candidate {
                    id,
                    resource: o
                        .call_url
                        .clone()
                        .unwrap_or_else(|| format!("{} {}", o.endpoint_method, o.endpoint_path)),
                    description: o
                        .endpoint_description
                        .clone()
                        .unwrap_or_default()
                        .chars()
                        .take(DESCRIPTION_CHARS)
                        .collect(),
                    price: price_label(o),
                    provider: o.api_name.clone(),
                    capability: o.capability_slug.clone(),
                    requires: required_inputs(o),
                    relevance: relevance_with_weights(o, terms, weights),
                },
                o,
            )
        })
        .collect();

    Shortlist { pairs, matched }
}

/// De instructie voor de herordening.
///
/// De `fit`-rubriek is het hart. Zonder cijfer zegt een pick alleen "dit past",
/// en dan is een endpoint dat de vraag half beantwoordt niet te onderscheiden
/// van een dat hem echt beantwoordt. Met de rubriek is 75 een betekenisvolle
/// streep — zie [`MATCH_FLOOR`].
///
/// HET MODEL KENT DIE STREEP NIET, en dat is met opzet. Het schreef eerder
/// alleen een `context_question` wanneer het zelf oordeelde dat niets
/// `match_floor` haalde, terwijl de gateway diezelfde vraag met haar eigen som
/// beantwoordde. Twee rechters, één beslissing: gaven ze een ander antwoord —
/// model geeft fit 70 en vindt zijn keuze prima, gateway ziet 70 < 75 — dan
/// stond er geen vraag klaar en viel het antwoord terug op een sjabloon dat de
/// vraag niet gelezen had. Nu levert het model altijd materiaal en beslist de
/// gateway alleen nog of ze het gebruikt.
pub const RERANK_SYSTEM_PROMPT: &str = "\
You select paid API endpoints that can answer a parsed research question, and \
you say how well each one fits.

Given the parsed question and a list of candidate endpoints, pick the most \
suitable ones, best first.

Rules:
- Use ONLY ids from the candidate list. Never invent an id.
- Judge the KIND of data or action an endpoint returns, not the topic words it \
shares with the question. A company-news endpoint does not answer who owns the \
company; a stock-quote endpoint does not answer what the news says; a registry \
for one country does not answer a question about a company in another.
- `capability` is the capability from the execution plan this endpoint serves.
- `reason` is ONE short sentence on why this endpoint fits THIS question. Be \
concrete: name the entity or the field it returns. Never restate the description.
- `fit` is 0-100: how well this endpoint answers the question AS ASKED.
    90-100  returns exactly the kind of data asked for, for the kind of entity asked about.
    75-89   answers the core of the question; a detail or one extra call may be needed.
    50-74   related data that answers only part of the question, or the right kind \
of data for a different scope (another country, register or entity type).
    under 50  shares words with the question but does not answer it. Leave these out.
  Score the KIND of data against the question, not whether the question already \
supplies every input: an endpoint that returns exactly what was asked scores 90+ \
even when the asker still has to name the company or give an identifier — that \
goes in `missing`, never into a lower `fit`. Judge for yourself; the input carries \
no score to copy.
- Prefer an endpoint that answers the question directly over one that would need \
another call first.
- Return fewer than the maximum when fewer candidates genuinely fit. Return an \
EMPTY list only when no candidate returns the kind of data asked for. A candidate \
that needs an identifier the question does not give (a registration number for a \
company named only by name) is NOT a reason for an empty list or a low fit: pick \
it, and name the identifier in `missing`. Padding the list with weak matches is \
worse than a short list.

Each candidate lists the fields it REQUIRES. Check the top pick against the \
parsed question:
- `missing` names every required field the question does not supply, for the \
top pick only, spelled exactly as in that pick's `requires` list (its own field \
name, never a generic word for it). Empty when the question already answers all \
of them. A question that names no specific company, person or place while the \
pick requires one (\"sanctions screening for a company\") is missing that field.
- `follow_up` is ONE short question to the asker for exactly those fields, in \
the asker's own language, naming the endpoint's purpose rather than its field \
names (\"Which company domain should I look up?\" — not \"Provide `domain`\"). \
Null when nothing is missing. Never guess a value and never ask for something \
the question already gave you.

ALWAYS write `context_question`, whatever `fit` you gave your top pick. You are \
not the one who decides whether the pick is good enough — that is settled after \
you answer, against a threshold you never see. When the pick holds up, your \
question is discarded and nothing is lost. When it does not, your question is \
the only thing standing between the asker and a generic form. Do not try to \
guess which it will be.
- `context_question` is ONE short question to the asker, in the asker's own \
language, asking for the single piece of context that would most improve THIS \
choice: the country or market, the kind of data wanted, an identifier they have \
(registration number, domain, ticker), or the time window. Say what the closest \
candidates DO cover when that helps the asker decide. NEVER ask for anything the \
question or the parsed entities already give you — a question that names \
\"Mollie B.V.\" is never asked for a company name. Null ONLY when no answer the \
asker could give would change which candidates you picked.
- `context_wanted` lists the pieces of context that question asks for, as short \
labels in the asker's language (\"country\", \"registration number\", \"kind of \
data\"). Empty when `context_question` is null.

Write `follow_up`, `context_question` and `context_wanted` in the asker's \
language: `asker_language` in the input when given (an ISO-639-1 code), \
otherwise the language the question is written in. A Dutch question gets Dutch \
questions back.

`summary` is one or two sentences, in the asker's language, saying what you \
understood the asker wants and what the endpoints you picked can tell them — \
addressed to the asker, no list, no ids. Null when `endpoints` is empty.

Three patterns, so the difference is clear:
- \"How many people work at Mollie and where is it located?\" with a business \
register profile endpoint that returns employee counts and addresses and requires \
a registration number: the endpoint answers exactly this kind of question, so \
pick it with fit 90+, missing [\"<the number field>\"], follow_up asking for that \
company's registration number. Not an empty list. Write `context_question` too — \
asking for that same registration number, never for the company name, which the \
question gives — and expect it to go unused.
- \"Sanctions screening for a company\" names the KIND of data but no company: \
the sanctions-screening endpoints answer this kind of question, so pick them with \
fit by kind (90+ for a direct screen), missing [\"<the entity field>\"] on the top \
pick, follow_up asking which company to screen. Not an empty list. \
`context_question` asks which company.
- \"Give me data about a company\" names no company AND no kind of data: nothing \
can be picked, so endpoints is empty and `context_question` asks which company \
and which data. Only this pattern gets an empty list.";

/// Het schema van het herordenings-antwoord.
pub fn rerank_schema(max: usize) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        // Strict mode kent geen optioneel veld: `follow_up`, `missing`,
        // `context_question` en `context_wanted` staan allemaal in `required`
        // en mogen leeg of null zijn. Zie `read_schema` in v1_routes/ask.rs
        // voor waarom dat geen keuze is.
        "required": ["endpoints", "missing", "follow_up", "context_question", "context_wanted", "summary"],
        "properties": {
            "missing": {
                "type": "array",
                "description": "Required fields of the top pick that the question does not supply. Empty when it is runnable as asked.",
                "items": { "type": "string" }
            },
            "follow_up": {
                "type": ["string", "null"],
                "description": "One short question asking the asker for exactly those fields. Null when nothing is missing."
            },
            "context_question": {
                "type": ["string", "null"],
                "description": "Always written: one short question, in the asker's language, asking for the single piece of context that would most improve the choice. Never asks for anything the question already gives. Null only when no answer could change the picks."
            },
            "context_wanted": {
                "type": "array",
                "description": "The pieces of context that question asks for, as short labels. Empty when context_question is null.",
                "items": { "type": "string" }
            },
            "summary": {
                "type": ["string", "null"],
                "description": "One or two sentences, in the asker's language, on what the asker wants and what the picked endpoints can tell them. Null when endpoints is empty."
            },
            "endpoints": {
                "type": "array",
                "description": format!("At most {max} endpoints, best first."),
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id", "capability", "reason", "fit"],
                    "properties": {
                        "id": { "type": "integer", "description": "Index from the candidate list." },
                        "capability": { "type": "string" },
                        "reason": { "type": "string" },
                        "fit": { "type": "integer", "description": "0-100: how well this endpoint answers the question as asked. See the rubric." }
                    }
                }
            }
        }
    })
}

/// De samenvatting uit het herordenings-antwoord, als het model er een schreef.
///
/// Eén of twee zinnen in de taal van de steller: wat er begrepen is en wat de
/// gekozen endpoints kunnen vertellen. De App zet dit boven de kaarten; zonder
/// dit veld moest zij daar zelf een zin voor verzinnen, en dat was een tweede
/// modellaag voor een zin die dit model toch al in zijn hoofd had.
pub fn read_summary(raw: &Value) -> Option<String> {
    raw.get("summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(500).collect())
}

/// Heeft het model gelezen en nee gezegd?
///
/// Een geldig antwoord met een LEGE lijst endpoints. Dat is een oordeel over de
/// kandidaten, geen storing: het model kreeg de shortlist te zien en vond er
/// niets bij dat de vraag beantwoordt. Een ontbrekend of onleesbaar veld is
/// géén nee — dan is er niets gezegd, en valt de aanroeper terug op de lexicale
/// volgorde.
pub fn said_no(raw: &Value) -> bool {
    raw.get("endpoints")
        .and_then(Value::as_array)
        .is_some_and(|items| items.is_empty())
}

/// Zet het modelantwoord om in resultaten.
///
/// Een id dat niet in de shortlist staat wordt overgeslagen in plaats van
/// gefabriceerd: een verzonnen verwijzing is erger dan een korter antwoord.
pub fn read_rerank(
    raw: &Value,
    candidates: &[Candidate],
    offers: &[&CapabilityOffer],
    max: usize,
) -> Vec<Selected> {
    let Some(items) = raw.get("endpoints").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();

    for item in items {
        if out.len() >= max {
            break;
        }
        let Some(id) = item.get("id").and_then(Value::as_u64).map(|v| v as usize) else {
            continue;
        };
        let (Some(c), Some(offer)) = (candidates.get(id), offers.get(id)) else {
            tracing::warn!("rerank named id {id}, which is not in the shortlist");
            continue;
        };
        if !seen.insert(id) {
            continue;
        }

        out.push(Selected {
            resource: c.resource.clone(),
            description: c.description.clone(),
            price: c.price.clone(),
            provider: c.provider.clone(),
            capability: item
                .get("capability")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(&c.capability)
                .to_string(),
            reason: item
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .chars()
                .take(240)
                .collect(),
            requires: c.requires.clone(),
            relevance: c.relevance,
            fit: item
                .get("fit")
                .and_then(Value::as_i64)
                .map(|v| v.clamp(0, 100) as u8),
            candidate_id: offer.endpoint_id,
            candidate: candidate_value(offer),
        });
    }
    // De `fit` is het cijfer, de volgorde is de bijvangst. Het model hoort
    // "beste eerst" te geven en doet dat vrijwel altijd, maar als de lijst en
    // de cijfers elkaar tegenspreken wint het cijfer: `needs_input` en de lat
    // van [`MATCH_FLOOR`] worden tegen de BOVENSTE gehouden, en die moet dan
    // ook de beste zijn. Stabiel, zodat gelijke cijfers de modelvolgorde houden
    // en een ontbrekend cijfer achteraan komt.
    out.sort_by_key(|s| std::cmp::Reverse(s.fit));
    out
}

/// De hoogste `fit` onder de gekozen endpoints. `None` zonder model.
pub fn best_fit(selected: &[Selected]) -> Option<u8> {
    selected.iter().filter_map(|s| s.fit).max()
}

/// Het getal waar de lat tegen gehouden wordt, en wie het gaf.
///
/// Het model als het gerangschikt heeft; anders de lexicale relevantie, met de
/// naam erbij zodat een aanroeper een 70 van een woordtelling niet aanziet voor
/// een 70 van een oordeel. Beide zitten op dezelfde 0–100-schaal, wat de
/// enige reden is dat één lat voor allebei kan dienen.
pub fn match_score(top_fit: Option<u8>, best_relevance: u8) -> (u8, &'static str) {
    match top_fit {
        Some(fit) => (fit, "model"),
        None => (best_relevance, "lexical"),
    }
}

/// Moet er om context gevraagd worden, gegeven wat er gemeten is?
///
/// Puur en apart, omdat dit de beslissing is die misging. Twee voorwaarden, en
/// de eerste werd niet gesteld: er moet een MODEL geoordeeld hebben (`top_fit`
/// is dan `Some`), en dat oordeel moet onder de lat liggen. Zonder model is
/// `score` de lexicale relevantie, en die tegen [`MATCH_FLOOR`] houden is een
/// vergelijking tussen twee schalen — zie de kop van die constante.
pub fn asks_for_context(top_fit: Option<u8>, score: u8, floor: u8) -> bool {
    top_fit.is_some() && score < floor
}

/// Wat er nog aan de vraag ontbreekt voordat de bovenste keuze uitvoerbaar is.
#[derive(Debug, Clone, Serialize)]
pub struct NeedsInput {
    /// De verplichte velden die de vraag niet invult. Altijd een deelverzameling
    /// van wat het endpoint zelf eist.
    pub missing: Vec<String>,
    /// Eén vraag aan de steller, van het model.
    pub question: String,
}

/// De vervolgvraag uit het herordenings-antwoord, tegen de kandidaat gehouden.
///
/// DE FILTER IS HET PUNT. Het model mag zeggen dát er iets ontbreekt, maar niet
/// wát: alleen velden die de gekozen kandidaat daadwerkelijk in zijn schema als
/// verplicht opgeeft, komen erdoor. Anders vraagt een model dat zich vergist om
/// gegevens die nergens heen gaan, en dat is precies de soort vraag die iemand
/// één keer beantwoordt en daarna wegklikt.
///
/// `None` zodra er na dat filter niets overblijft — inclusief het geval waarin
/// het model een vraag stelde zonder een veld te noemen. Een vervolgvraag zonder
/// ontbrekend veld is ruis boven een antwoord dat al klopt.
pub fn read_follow_up(raw: &Value, top: Option<&Candidate>) -> Option<NeedsInput> {
    let top = top?;
    if top.requires.is_empty() {
        return None;
    }

    let missing: Vec<String> = raw
        .get("missing")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|v| v.as_str())
        .filter(|name| top.requires.iter().any(|r| r == name))
        .map(str::to_string)
        .collect();

    if missing.is_empty() {
        return None;
    }

    let question = raw
        .get("follow_up")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        // Het model zet de vraag soms in het andere vak: `missing` gevuld,
        // `follow_up` null, en "Wat is het KVK-nummer van Mollie?" in
        // `context_question`. Gemeten met Haiku, 2026-09-02. De vraag is er,
        // en het ontbrekende veld is hierboven al tegen het schema gehouden —
        // dat is het deel dat niet van het model mag komen. De vraag wel.
        .or_else(|| {
            raw.get("context_question")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })?
        .chars()
        .take(300)
        .collect();

    Some(NeedsInput { missing, question })
}

/// Wat de steller nog moet vertellen voordat er een bron gekozen kan worden.
///
/// De tegenhanger van [`NeedsInput`]. Die vraagt om een VELD voor een endpoint
/// dat al gekozen is; dit vraagt om CONTEXT omdat er nog geen endpoint gekozen
/// kán worden — niets haalt [`MATCH_FLOOR`], of er is helemaal niets. Nooit
/// allebei tegelijk: één vraag per antwoord, en een veld opvragen voor een
/// keuze waar we niet achter staan is ruis.
#[derive(Debug, Clone, Serialize)]
pub struct NeedsContext {
    /// `weak_match`: er zijn bronnen, maar geen haalt de lat. `no_source`:
    /// niets in de buurt, of het model heeft gelezen en nee gezegd.
    pub reason: &'static str,
    /// De hoogste score die een kandidaat haalde, op dezelfde schaal als `floor`.
    pub best_match: u8,
    /// De lat die niet gehaald is. Standaard [`MATCH_FLOOR`].
    pub floor: u8,
    /// Eén vraag aan de steller, in de taal van de vraag.
    pub question: String,
    /// Waar die vraag om vraagt, als korte labels. Voor een formulier dat meer
    /// wil tonen dan één tekstveld.
    pub wanted: Vec<String>,
    /// Wie de vraag schreef: `rerank` (zag de kandidaten en kan zeggen wat ze
    /// wél dekken), `reader` (zag alleen de vraag), of `template` (geen model).
    pub written_by: &'static str,
    /// Of ANTWOORDEN OP DEZE VRAAG KAN HELPEN.
    ///
    /// Het verschil tussen "ik mis één gegeven van jou" en "hier staat niets
    /// dat dit dekt", en het is geen nuance: zonder dat onderscheid stelt de
    /// chat dezelfde vraag opnieuw zodra het antwoord de score niet verandert,
    /// en dat blijft hij doen. Een gebruiker vroeg twee keer om hetzelfde en
    /// had het oneindig vaak gekregen.
    ///
    /// Waar op wordt gescheiden: heeft het herordeningsmodel de kandidaten
    /// GEZIEN. Zo ja, dan is zijn vraag geworteld in wat de catalogus wél
    /// heeft ("ik heb Nederlandse registers, bedoel je een Nederlands
    /// bedrijf?") en kan een antwoord een kandidaat over de lat tillen. Zo
    /// nee — niets haalde zelfs de lexicale ondergrens, dus er is nooit een
    /// shortlist geweest — dan is de vraag een gok van de lezer of van een
    /// sjabloon, en geen enkel antwoord verandert daar iets aan. Dan hoort er
    /// te staan dat de catalogus dit niet dekt.
    pub answerable: bool,
}

/// Bouwt de contextvraag uit wat er beschikbaar is, beste bron eerst.
///
/// Drie bronnen, in aflopende kwaliteit. Het herordeningsmodel heeft de
/// kandidaten gezien en kan zeggen "ik heb Nederlandse registers, bedoel je
/// een Nederlands bedrijf?" — dat is de vraag die iemand in één keer goed
/// beantwoordt. De lezer heeft alleen de vraag gezien en kan zeggen dat die te
/// vaag is. Het sjabloon weet niets en vraagt om alles.
pub fn needs_context(
    reason: &'static str,
    best_match: u8,
    floor: u8,
    rerank: Option<&Value>,
    parsed: Option<&Value>,
    question: &str,
) -> NeedsContext {
    let dutch = is_dutch(parsed, question);
    let known = known_from(parsed);

    if let Some((question, wanted)) = rerank.and_then(|raw| read_context_question(raw, &known)) {
        return NeedsContext {
            reason,
            best_match,
            floor,
            question,
            wanted,
            written_by: "rerank",
            // Het model zag de shortlist; zijn vraag gaat over wat er wél is.
            answerable: true,
        };
    }
    // HIER HOORT HET NIET MEER TE KOMEN ALS ER EEN VERDICT IS.
    //
    // Het model zag de kandidaten en kreeg de opdracht onvoorwaardelijk een
    // `context_question` te schrijven. Doet het dat niet, dan klopt er iets
    // niet aan de prompt, het schema of het model — en de steller merkt daar
    // alleen iets van in de vorm van een vager antwoord dan nodig. Eén regel
    // in het log maakt het meetbaar in plaats van onzichtbaar.
    if rerank.is_some() {
        tracing::warn!(
            reason,
            best_match,
            floor,
            "ask: rerank gave no usable context_question; falling back to reader or template"
        );
    }

    // Ook de lezer krijgt de afgeslankte labels: hij schreef de vraag, maar het
    // formulier ernaast hoort niet om een bedrijfsnaam te vragen die de vraag
    // al noemt.
    let wanted = context_labels(dutch, &known);
    if let Some(clarification) = parsed.and_then(reader_clarification) {
        return NeedsContext {
            reason,
            best_match,
            floor,
            question: clarification,
            wanted,
            written_by: "reader",
            // De lezer zag alleen de VRAAG, nooit de kandidaten. Zijn
            // verduidelijking helpt dus alleen als er iets te herordenen viel:
            // draaide het model, dan is er een shortlist geweest en kan een
            // scherpere vraag daar doorheen komen.
            answerable: rerank.is_some(),
        };
    }
    NeedsContext {
        reason,
        best_match,
        floor,
        question: template_question(reason, dutch, question, &known, &wanted),
        wanted,
        written_by: "template",
        // Een sjabloonvraag zonder herordening betekent dat er nooit een
        // shortlist was: niets haalde de lexicale ondergrens. Wat de steller
        // ook antwoordt, dezelfde nul kandidaten komen eruit. Dit is precies
        // de lus die iemand twee keer dezelfde vraag opleverde.
        answerable: rerank.is_some(),
    }
}

/// De contextvraag uit het herordenings-antwoord, als het model er een gaf.
///
/// Leeg of alleen spaties is geen vraag. De labels zijn een lijst van korte
/// strings; alles wat dat niet is valt eruit, en meer dan acht is geen lijst
/// meer maar een opsomming die niemand invult.
///
/// EN DE FILTER, om dezelfde reden als in [`read_follow_up`]. Het model schrijft
/// deze vraag nu onvoorwaardelijk, dus hij wordt vaker getoond dan vroeger; hem
/// ongezien doorgeven zou de vraag die dit hele geval opleverde — vraag de
/// steller om de bedrijfsnaam die hij net typte — gewoon via een andere deur
/// terug laten komen. De prompt verbiedt het, en de prompt verbood het vorige
/// keer ook.
///
/// `None` zodra elk gevraagd stuk al in de vraag stond: dan is de vraag van het
/// model geen vraag meer, en is het sjabloon — dat de parse wél leest — beter.
fn read_context_question(raw: &Value, known: &Known) -> Option<(String, Vec<String>)> {
    let question: String = raw
        .get("context_question")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .chars()
        .take(300)
        .collect();
    let wanted = raw
        .get("context_wanted")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.chars().take(60).collect::<String>())
                .take(8)
                .collect()
        })
        .unwrap_or_default();

    // Zonder labels valt er niets te toetsen; dan telt de vraag zoals hij is.
    let wanted: Vec<String> = wanted;
    if wanted.is_empty() {
        return Some((question, wanted));
    }
    let kept: Vec<String> = wanted
        .into_iter()
        .filter(|label| !known.answers(label))
        .collect();
    if kept.is_empty() {
        return None;
    }
    Some((question, kept))
}

/// De verhelderende vraag die de lezer stelde, als hij de vraag te vaag vond.
fn reader_clarification(parsed: &Value) -> Option<String> {
    parsed
        .pointer("/question/clarifying_question")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(300).collect())
}

/// Is de vraag Nederlands? De lezer weet het; zonder lezer wordt het geraden.
fn is_dutch(parsed: Option<&Value>, question: &str) -> bool {
    match parsed
        .and_then(|p| p.pointer("/question/language"))
        .and_then(Value::as_str)
        .map(|s| s.trim().to_ascii_lowercase())
    {
        Some(code) if !code.is_empty() => code == "nl" || code.starts_with("nl-"),
        _ => looks_dutch(question),
    }
}

/// Functiewoorden die een Nederlandse zin vrijwel altijd bevat en een Engelse
/// vrijwel nooit. Twee treffers is genoeg: "de" alleen is ook Frans of Spaans,
/// "de" plus "van" is Nederlands.
const DUTCH_MARKERS: &[&str] = &[
    "de",
    "het",
    "een",
    "van",
    "voor",
    "met",
    "wie",
    "wat",
    "welke",
    "welk",
    "hoe",
    "waar",
    "wanneer",
    "ik",
    "mij",
    "mijn",
    "zijn",
    "niet",
    "geef",
    "zoek",
    "naar",
    "over",
    "bedrijf",
    "bedrijven",
    "jaarrekening",
    "eigenaar",
    "adres",
    "hoeveel",
    "werken",
    "bij",
];

/// Ruwe taalgok voor het sjabloon, alleen gebruikt als geen model iets zei.
///
/// Bewust simpel en bewust binair: de catalogus is Engels en de vragen die
/// hier binnenkomen zijn Engels of Nederlands. Een Duitse vraag krijgt het
/// Engelse sjabloon, wat niet mooi is en wel begrijpelijk.
pub fn looks_dutch(text: &str) -> bool {
    let hits = text
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && DUTCH_MARKERS.contains(w))
        .count();
    hits >= 2
}

/// Wat de lezer al uit de vraag heeft gehaald.
///
/// DE LEZER HEEFT DE VRAAG AL GESCAND. `resolved_entities`, `expected_output`
/// en `recency_days` komen uit een modelcall die toch al gedaan is. Vragen om
/// iets dat daar al in staat, laat de steller herhalen wat hij net getypt
/// heeft — en dat is precies wat hier gebeurde: "Where is Mollie B.V.
/// registered?" kreeg een vraag terug om een bedrijfsnaam, omdat het sjabloon
/// de vraag alleen tussen aanhalingstekens terugcitéérde en hem nooit las.
#[derive(Default)]
struct Known {
    /// De eerste entiteit die de lezer herkende, als naam.
    subject: Option<String>,
    /// Een ticker vragen is alleen zinvol voor een verhandelbaar ding.
    is_asset: bool,
    /// Een registratienummer of domein vragen is zinvol voor een bedrijf of
    /// een asset; voor een persoon of een plaats niet.
    is_addressable: bool,
    country: bool,
    domain: bool,
    registration: bool,
    ticker: bool,
    kind_of_data: bool,
    /// Of de lezer überhaupt iets opleverde. Zonder lezer weten we niets en is
    /// om alles vragen het eerlijke antwoord.
    read: bool,
}

impl Known {
    /// Geeft de vraag dit label al antwoord?
    ///
    /// Conservatief met opzet, en de asymmetrie is de reden: een label dat
    /// blijft staan kost de steller een overbodig invulveld, een label dat
    /// onterecht wegvalt kost hem het antwoord. Bij twijfel blijft het staan.
    fn answers(&self, label: &str) -> bool {
        (self.country && phrase_hit(label, &["country", "land", "market", "markt"]))
            || (self.subject.is_some()
                && phrase_hit(
                    label,
                    &[
                        "company name",
                        "bedrijfsnaam",
                        "name of the company",
                        "naam van het bedrijf",
                    ],
                ))
            || (self.domain && phrase_hit(label, &["domain", "domein", "website"]))
            || (self.kind_of_data
                && phrase_hit(
                    label,
                    &[
                        "kind of data",
                        "soort gegevens",
                        "type gegevens",
                        "what data",
                        "welke gegevens",
                    ],
                ))
            || (self.ticker && phrase_hit(label, &["ticker"]))
            || (self.registration
                && phrase_hit(
                    label,
                    &[
                        "registration number",
                        "registratienummer",
                        "kvk",
                        "company number",
                        "vat number",
                        "btw",
                    ],
                ))
    }
}

/// Zoekt een woord of woordgroep in een label, op woordgrenzen.
///
/// Op woordgrenzen en niet als losse substring, anders slaat "land" aan op
/// "Nederland" en gooit de filter precies het label weg dat wél gevraagd moest
/// worden.
fn phrase_hit(label: &str, needles: &[&str]) -> bool {
    let words: Vec<String> = label
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    let padded = format!(" {} ", words.join(" "));
    needles.iter().any(|n| padded.contains(&format!(" {n} ")))
}

/// Een tekstveld telt alleen als ingevuld wanneer er ook echt iets in staat.
fn filled(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty())
}

/// Leest het geparseerde object uit tot wat het over de vraag zegt.
fn known_from(parsed: Option<&Value>) -> Known {
    let Some(parsed) = parsed else {
        return Known::default();
    };
    let mut known = Known {
        read: true,
        ..Known::default()
    };

    for entity in parsed
        .get("resolved_entities")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let kind = entity.get("type").and_then(Value::as_str).unwrap_or("");
        if known.subject.is_none() {
            known.subject = entity
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.chars().take(60).collect());
        }
        known.is_asset |= kind == "asset";
        known.is_addressable |= matches!(kind, "company" | "asset" | "domain" | "other" | "");
        known.country |= filled(entity.get("country"));
        known.domain |= filled(entity.get("domain"));
        let ids = entity.get("identifiers");
        known.registration |= ["kvk_number", "company_number", "lei", "vat_number"]
            .iter()
            .any(|field| filled(ids.and_then(|i| i.get(field))));
        known.ticker |= filled(ids.and_then(|i| i.get("ticker")));
    }

    known.kind_of_data = parsed
        .get("expected_output")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items
                .iter()
                .any(|v| v.as_str().is_some_and(|s| !s.trim().is_empty()))
        });

    known
}

/// Alles vragen. Juist voor een vraag als "geef me data over een bedrijf":
/// die noemt geen onderwerp en geen soort gegevens, en dan is elk van deze
/// vier het antwoord dat de bronkeuze losmaakt.
fn vague_labels(dutch: bool) -> Vec<String> {
    let labels: &[&str] = if dutch {
        &[
            "land of markt",
            "soort gegevens dat je terug wilt",
            "identificatie (bedrijfsnaam, KVK-nummer, domein, ticker)",
            "periode",
        ]
    } else {
        &[
            "country or market",
            "kind of data you want back",
            "identifier (company name, registration number, domain, ticker)",
            "time window",
        ]
    };
    labels.iter().map(|s| s.to_string()).collect()
}

/// De identificatie die nog ontbreekt, of niets als de vraag hem al geeft.
///
/// Per stuk, niet als blok: een vraag die de naam noemt maar het nummer niet,
/// hoort om het nummer gevraagd te worden en niet opnieuw om de naam.
fn identifier_label(dutch: bool, known: &Known) -> Option<String> {
    if !known.is_addressable {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    if known.subject.is_none() {
        parts.push(if dutch {
            "bedrijfsnaam"
        } else {
            "company name"
        });
    }
    if !known.registration {
        parts.push(if dutch {
            "KVK-nummer"
        } else {
            "registration number"
        });
    }
    if !known.domain {
        parts.push(if dutch { "domein" } else { "domain" });
    }
    if known.is_asset && !known.ticker {
        parts.push("ticker");
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!(
        "{} ({})",
        if dutch { "identificatie" } else { "identifier" },
        parts.join(", ")
    ))
}

/// Wat een steller nog kan toevoegen, als labels voor een formulier.
///
/// Alleen wat de vraag NIET al geeft. De periode staat bewust niet in de
/// restlijst: een tijdvenster is een parameter van een aanroep, geen reden om
/// de ene bron boven de andere te kiezen, dus hem hier vragen kost de steller
/// een antwoord dat de keuze niet losmaakt.
fn context_labels(dutch: bool, known: &Known) -> Vec<String> {
    if !known.read || (known.subject.is_none() && !known.kind_of_data) {
        return vague_labels(dutch);
    }

    let mut labels = Vec::new();
    if !known.country {
        labels.push(
            if dutch {
                "land of markt"
            } else {
                "country or market"
            }
            .to_string(),
        );
    }
    if !known.kind_of_data {
        labels.push(
            if dutch {
                "soort gegevens dat je terug wilt"
            } else {
                "kind of data you want back"
            }
            .to_string(),
        );
    }
    labels.extend(identifier_label(dutch, known));
    labels
}

/// De punt achter een naam, tenzij de naam er zelf al een heeft.
fn stop(name: &str) -> &'static str {
    if name.ends_with('.') {
        ""
    } else {
        "."
    }
}

/// "a, b en c" — een opsomming die als zin leest.
fn join_labels(labels: &[String], conjunction: &str) -> String {
    match labels {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{}, {conjunction} {last}", rest.join(", ")),
    }
}

/// De vraag als geen model er een schreef.
///
/// Drie delen: wat er niet gevonden is, wat we van de vraag begrepen hebben,
/// en wat er nog nodig is. Het middelste deel is het punt — het laat zien dat
/// de vraag gelezen is, en maakt een verkeerde resolutie meteen corrigeerbaar.
fn template_question(
    reason: &str,
    dutch: bool,
    question: &str,
    known: &Known,
    wanted: &[String],
) -> String {
    let quoted: String = question.trim().chars().take(80).collect();

    let found = match (dutch, reason) {
        (true, "weak_match") => format!(
            "Ik heb bronnen gevonden die in de buurt komen, maar geen die \"{quoted}\" duidelijk beantwoordt."
        ),
        (true, _) => format!("Ik heb geen bron gevonden die \"{quoted}\" beantwoordt."),
        (false, "weak_match") => format!(
            "I found sources that come close, but none that clearly answers \"{quoted}\"."
        ),
        (false, _) => format!("I couldn't find a source that answers \"{quoted}\"."),
    };

    let subject = match (known.subject.as_deref(), dutch) {
        // "Mollie B.V." draagt zijn eigen punt al. Er nog een achter zetten
        // geeft "Mollie B.V..", en dat leest als een tikfout van iemand die
        // niet oplet — precies de indruk die deze zin moet wegnemen.
        (Some(name), true) => format!(" Ik lees dit als een vraag over {name}{}", stop(name)),
        (Some(name), false) => format!(" I read this as being about {name}{}", stop(name)),
        (None, _) => String::new(),
    };

    // NIETS MEER TE VRAGEN IS OOK EEN ANTWOORD. De vraag is dan volledig en
    // het gat zit bij ons, niet bij de steller; hem dan alsnog een formulier
    // voorhouden is de fout uit dit sjabloon in een andere vorm.
    let ask = match (wanted.is_empty(), dutch) {
        (true, true) => " Als je weet in welk register of welke dataset dit zou moeten staan, zeg het dan, dan kijk ik daar.".to_string(),
        (true, false) => " If you know which register or dataset should have this, tell me and I'll look there.".to_string(),
        (false, true) => format!(" Kun je meer context geven: {}?", join_labels(wanted, "en")),
        (false, false) => format!(" Can you tell me more: {}?", join_labels(wanted, "and")),
    };

    format!("{found}{subject}{ask}")
}

/// Zonder model: de lexicale volgorde is het antwoord.
///
/// Gebruikt wanneer de herordening niet beschikbaar is of faalt. Slechter dan
/// een gewogen keuze, en stukken beter dan niets — de shortlist stond al op
/// relevantie gesorteerd.
pub fn without_rerank(
    candidates: &[Candidate],
    offers: &[&CapabilityOffer],
    max: usize,
) -> Vec<Selected> {
    candidates
        .iter()
        .zip(offers.iter())
        .take(max)
        .map(|(c, offer)| Selected {
            resource: c.resource.clone(),
            description: c.description.clone(),
            price: c.price.clone(),
            provider: c.provider.clone(),
            capability: c.capability.clone(),
            reason: "Matched on the words of the request; not ranked by a model.".to_string(),
            requires: c.requires.clone(),
            relevance: c.relevance,
            fit: None,
            candidate_id: offer.endpoint_id,
            candidate: candidate_value(offer),
        })
        .collect()
}

/// De snapshot zoals de rest van de gateway hem gebruikt.
pub type Snapshot = Arc<Vec<CapabilityOffer>>;

#[cfg(test)]
mod tests {
    use super::*;

    /// De shortlist zonder het veld eromheen, wat is wat deze tests meten.
    fn shortlist<'a>(
        offers: &'a [CapabilityOffer],
        terms: &Terms,
        limit: usize,
    ) -> Vec<(Candidate, &'a CapabilityOffer)> {
        shortlist_with_matches(offers, terms, &Weights::uniform(), limit).pairs
    }

    /// De shortlist zoals de route hem bouwt: gewogen op de pool zelf.
    fn weighted_shortlist<'a>(
        offers: &'a [CapabilityOffer],
        terms: &Terms,
        limit: usize,
    ) -> Vec<(Candidate, &'a CapabilityOffer)> {
        shortlist_with_matches(offers, terms, &Weights::over(offers, terms), limit).pairs
    }

    /// Alleen kernwoorden, zoals een vraag zonder parser eruitziet.
    fn core(text: &str) -> Terms {
        Terms {
            core: terms(text),
            expansion: Vec::new(),
        }
    }

    fn offer(slug: &str, path: &str, desc: &str, price: f64, cap: &str) -> CapabilityOffer {
        offer_requiring(slug, path, desc, price, cap, &[])
    }

    /// Hetzelfde, met een gepubliceerd invoerschema dat velden verplicht stelt.
    fn offer_requiring(
        slug: &str,
        path: &str,
        desc: &str,
        price: f64,
        cap: &str,
        required: &[&str],
    ) -> CapabilityOffer {
        CapabilityOffer {
            capability_slug: cap.to_string(),
            endpoint_id: Some(uuid::Uuid::new_v4()),
            api_slug: slug.to_string(),
            api_name: slug.to_string(),
            endpoint_method: "GET".into(),
            endpoint_path: path.to_string(),
            endpoint_description: Some(desc.to_string()),
            price_usd: price,
            payment_required: price > 0.0,
            hosted_externally: false,
            call_url: Some(format!("https://gateway.apiosk.com/{slug}{path}")),
            input_map: json!({}),
            fixed_params: json!({}),
            request_body: json!({}),
            query_params: json!({ "type": "object", "required": required }),
            notes: None,
            source: "curated".into(),
            confidence: 0.9,
            provider_name: None,
            provider_username: None,
            search_text: None,
        }
    }

    #[test]
    fn the_buyers_wording_finds_the_provider_who_used_another_one() {
        // Why migration 109's `search_terms` exists, in one case.
        //
        // The provider wrote the Dutch term. The buyer types the English one.
        // Neither the path, the slug nor the description contains it, so
        // substring matching over provider wording scores exactly zero — the
        // endpoint is not ranked low, it is not a candidate at all.
        let terms = vec!["vat".to_string()];

        let mut provider_wording = offer(
            "kvk-dutch-business-register",
            "/v1/basisprofielen",
            "Basisprofiel van een onderneming, inclusief BTW-identificatie.",
            0.02,
            "company.profile",
        );
        assert_eq!(
            score(&provider_wording, &terms),
            0,
            "precondition: the buyer's word is genuinely absent from the provider's own text"
        );

        // 110 backfilled exactly this, for 1295 endpoints, and until now no
        // query in the gateway read the column.
        provider_wording.search_text = Some("vat number company vat registration btw".to_string());
        assert!(
            score(&provider_wording, &terms) > 0,
            "the endpoint must become findable on the word the buyer actually types"
        );
    }

    #[test]
    fn search_metadata_moves_no_score_while_the_flag_is_off() {
        // Every pre-rework query selects `NULL::text as search_text`, so the
        // default path cannot shift by a single point. That is what makes the
        // flag a real rollback rather than a second behaviour to reason about.
        let terms = vec!["reviews".to_string(), "aliexpress".to_string()];
        let plain = offer(
            "crowdpull",
            "/v1/aliexpress/reviews",
            "Collect product reviews.",
            0.01,
            "reviews.collect",
        );
        assert!(plain.search_text.is_none());
        let with_empty = CapabilityOffer {
            search_text: None,
            ..plain.clone()
        };
        assert_eq!(score(&plain, &terms), score(&with_empty, &terms));
    }

    /// Eén gecrawlde listing, zoals de open indexen ze publiceren.
    fn listing(host: &str, path: &str, description: &str, price: f64) -> ExternalOffer {
        ExternalOffer {
            resource: format!("https://{host}{path}"),
            host: host.to_string(),
            description: description.to_string(),
            price_usd: Some(price),
            source: "coinbase-x402-bazaar-index".into(),
            provider: Some("UK Intel".into()),
            ..Default::default()
        }
    }

    // -- de open indexen ----------------------------------------------------

    #[test]
    fn a_crawled_listing_is_scored_on_its_path_the_way_a_catalogue_row_is() {
        let external = as_offer(&listing(
            "api.ukintel.uk",
            "/v1/company/credit-score",
            "A UK company's credit score and payment behaviour.",
            0.02,
        ));

        assert_eq!(external.endpoint_path, "/v1/company/credit-score");
        assert!(
            score(&external, &terms("credit score")) > 0,
            "the path is what a question is matched against"
        );
    }

    #[test]
    fn a_listings_tags_stay_searchable_after_the_conversion() {
        // The crawl matched it on its tags; the shortlist has to be able to,
        // or the pipeline drops what its own index just handed it.
        let mut raw = listing("api.example.com", "/lookup", "One call, one answer.", 0.01);
        raw.category = Some("Finance".into());
        raw.tags = vec!["creditworthiness".into(), "kyb".into()];

        let external = as_offer(&raw);

        assert!(score(&external, &terms("creditworthiness")) > 0);
        assert!(score(&external, &terms("finance")) > 0);
    }

    #[test]
    fn a_crawled_listing_carries_no_candidate_id_and_says_how_it_settles() {
        let external = as_offer(&listing(
            "api.ukintel.uk",
            "/v1/fca",
            "FCA register status.",
            0.02,
        ));
        assert!(is_external(&external));

        let candidate = candidate_value(&external);
        assert_eq!(candidate["candidate_id"], Value::Null);
        assert_eq!(candidate["kind"], "external");
        assert_eq!(candidate["settlement"], "apiosk-x402");
        assert_eq!(candidate["resource"], "https://api.ukintel.uk/v1/fca");
        assert_eq!(candidate["provider_price_usd"], 0.02);
        assert!(
            candidate["indicative_price_usd"].as_f64().unwrap() > 0.02,
            "the buyer pays list plus the fee, on both halves of the answer"
        );
    }

    #[test]
    fn a_catalogue_row_keeps_the_shape_discovery_already_publishes() {
        let row = offer("acme", "/lookup", "company lookup", 0.01, "x.y");
        assert!(!is_external(&row));

        let candidate = candidate_value(&row);
        assert_eq!(candidate["settlement"], "apiosk-proxied");
        assert!(candidate["candidate_id"].is_string());
    }

    #[test]
    fn the_catalogue_and_the_indexes_compete_in_one_shortlist() {
        let weak = offer(
            "newspulse",
            "/api/news",
            "Company news headlines",
            0.01,
            "news.read",
        );
        let strong = as_offer(&listing(
            "api.ukintel.uk",
            "/v1/company/credit-score",
            "A UK company's credit score and payment behaviour signals.",
            0.02,
        ));

        let pool = vec![weak, strong];
        let list = shortlist(&pool, &core("credit score payment behaviour"), 5);

        assert_eq!(
            list.first().map(|(c, _)| c.resource.as_str()),
            Some("https://api.ukintel.uk/v1/company/credit-score"),
            "the better answer wins, whichever half of the pool it came from"
        );
    }

    /// Een shortlist van één, met de verplichte velden die de test nodig heeft.
    fn one_candidate(required: &[&str]) -> Vec<Candidate> {
        let o = offer_requiring("acme", "/lookup", "company lookup", 0.01, "x.y", required);
        let pairs = shortlist(std::slice::from_ref(&o), &core("company lookup"), 1);
        pairs.into_iter().map(|(c, _)| c).collect()
    }

    #[test]
    fn a_missing_required_field_becomes_a_follow_up_question() {
        let candidates = one_candidate(&["domain"]);
        let raw = json!({
            "endpoints": [],
            "missing": ["domain"],
            "follow_up": "Which company domain should I look up?"
        });

        let needs = read_follow_up(&raw, candidates.first()).expect("a follow-up");
        assert_eq!(needs.missing, vec!["domain".to_string()]);
        assert!(needs.question.contains("domain"));
    }

    #[test]
    fn a_field_the_endpoint_does_not_require_is_never_asked_for() {
        // Het model noemt twee velden; het endpoint eist er één. Vragen om het
        // andere levert gegevens op die nergens heen gaan — de kern van waarom
        // deze functie tegen de kandidaat filtert in plaats van het model te
        // geloven.
        let candidates = one_candidate(&["domain"]);
        let raw = json!({
            "endpoints": [],
            "missing": ["domain", "vat_number"],
            "follow_up": "Give me the domain and the VAT number."
        });

        let needs = read_follow_up(&raw, candidates.first()).expect("a follow-up");
        assert_eq!(needs.missing, vec!["domain".to_string()]);
    }

    #[test]
    fn nothing_is_asked_when_the_endpoint_requires_nothing() {
        // Ook niet als het model toch een vraag verzint: zonder verplicht veld
        // is er niets om op te halen.
        let candidates = one_candidate(&[]);
        let raw = json!({
            "endpoints": [],
            "missing": ["domain"],
            "follow_up": "Which domain?"
        });
        assert!(read_follow_up(&raw, candidates.first()).is_none());
    }

    #[test]
    fn a_question_in_the_other_slot_still_counts_as_the_follow_up() {
        // `missing` gevuld, `follow_up` null, en de vraag om het nummer in
        // `context_question`: dan is de vraag er, en het veld is tegen het
        // schema gehouden. Niets vragen zou de steller met een gekozen
        // endpoint en geen weg ernaartoe achterlaten.
        let candidates = one_candidate(&["kvkNummer"]);
        let raw = json!({
            "endpoints": [{ "id": 0, "capability": "c", "reason": "r", "fit": 92 }],
            "missing": ["kvkNummer"],
            "follow_up": null,
            "context_question": "Wat is het KVK-nummer van Mollie?",
            "context_wanted": ["KVK-nummer"]
        });
        let needs = read_follow_up(&raw, candidates.first()).expect("a follow-up");
        assert_eq!(needs.missing, vec!["kvkNummer".to_string()]);
        assert_eq!(needs.question, "Wat is het KVK-nummer van Mollie?");
    }

    #[test]
    fn a_runnable_question_gets_no_follow_up() {
        let candidates = one_candidate(&["domain"]);
        let raw = json!({ "endpoints": [], "missing": [], "follow_up": null });
        assert!(read_follow_up(&raw, candidates.first()).is_none());
    }

    #[test]
    fn a_missing_field_without_a_question_is_not_a_follow_up() {
        // Een lijst zonder vraag is geen vervolgvraag maar een half antwoord;
        // de aanroeper zou een leeg tekstveld tonen.
        let candidates = one_candidate(&["domain"]);
        let raw = json!({ "endpoints": [], "missing": ["domain"], "follow_up": "   " });
        assert!(read_follow_up(&raw, candidates.first()).is_none());
    }

    #[test]
    fn the_rerank_schema_asks_for_the_follow_up_in_strict_form() {
        // Strict mode kent geen optioneel veld: alles MOET in `required`
        // staan, anders weigert OpenRouter de hele call en valt /v1/ask stil
        // terug op de lexicale volgorde zonder dat iemand het merkt.
        let schema = rerank_schema(5);
        let required = schema["required"].as_array().expect("required");
        for field in [
            "endpoints",
            "missing",
            "follow_up",
            "context_question",
            "context_wanted",
        ] {
            assert!(
                required.iter().any(|v| v == field),
                "{field} must be required in strict mode"
            );
        }
        assert_eq!(
            schema["properties"]["follow_up"]["type"],
            json!(["string", "null"])
        );
        assert_eq!(
            schema["properties"]["context_question"]["type"],
            json!(["string", "null"])
        );
        // En per endpoint het cijfer waar de lat tegen gehouden wordt.
        let item_required = schema["properties"]["endpoints"]["items"]["required"]
            .as_array()
            .expect("item required");
        assert!(item_required.iter().any(|v| v == "fit"));
    }

    // --- de lat --------------------------------------------------------------

    #[test]
    fn the_fit_is_read_and_clamped() {
        let (offers, list) = fixture();
        let refs: Vec<&CapabilityOffer> = offers.iter().collect();
        let raw = json!({ "endpoints": [
            { "id": 0, "capability": "c", "reason": "r", "fit": 130 },
            { "id": 1, "capability": "c", "reason": "r" }
        ]});
        let out = read_rerank(&raw, &list, &refs, TOP_N);
        assert_eq!(out[0].fit, Some(100), "boven de schaal wordt afgekapt");
        assert_eq!(out[1].fit, None, "geen cijfer is geen cijfer, niet nul");
    }

    #[test]
    fn endpoints_are_ordered_by_fit_not_by_the_models_list_order() {
        // De lat en de vervolgvraag worden tegen de BOVENSTE gehouden, dus die
        // moet de beste zijn — ook als het model zijn eigen cijfers niet in
        // volgorde zette.
        let (offers, list) = fixture();
        let refs: Vec<&CapabilityOffer> = offers.iter().collect();
        let raw = json!({ "endpoints": [
            { "id": 1, "capability": "c", "reason": "second", "fit": 60 },
            { "id": 0, "capability": "c", "reason": "first", "fit": 92 }
        ]});
        let out = read_rerank(&raw, &list, &refs, TOP_N);
        assert_eq!(out[0].reason, "first");
        assert_eq!(best_fit(&out), Some(92));
    }

    #[test]
    fn without_a_model_there_is_no_fit_and_the_lexical_score_is_the_match() {
        let (offers, list) = fixture();
        let refs: Vec<&CapabilityOffer> = offers.iter().collect();
        let out = without_rerank(&list, &refs, TOP_N);
        assert!(out.iter().all(|s| s.fit.is_none()));
        assert_eq!(best_fit(&out), None);
        // En het antwoord zegt dat het een woordtelling was, geen oordeel.
        assert_eq!(match_score(None, 68), (68, "lexical"));
        assert_eq!(match_score(Some(81), 68), (81, "model"));
    }

    /// EEN LAT OP DE VERKEERDE SCHAAL, MET DE GEMETEN CIJFERS ERBIJ.
    ///
    /// Op "Where is Mollie B.V. registered?" geeft het model het KVK-register
    /// fit 90; dezelfde rij scoort lexicaal 59. Viel de modelcall weg, dan werd
    /// die 59 tegen 75 gehouden en heette een perfect antwoord ineens zwak.
    #[test]
    fn without_a_model_no_context_is_asked_however_low_the_lexical_score() {
        // Zoals het hoort te gaan: het model oordeelde, en het oordeel telt.
        assert!(!asks_for_context(Some(90), 90, MATCH_FLOOR));
        assert!(asks_for_context(Some(60), 60, MATCH_FLOOR));

        // De echte rij, zonder model. 59 < 75, en toch geen contextvraag:
        // wij hebben niet geoordeeld, dus wij vragen niets.
        assert!(!asks_for_context(None, 59, MATCH_FLOOR));
        // En dat geldt over de hele lexicale schaal, want die draagt geen
        // oordeel — ook 25 of 74 is geen bewijs dat de vraag tekortschiet.
        for lexical in [RELEVANCE_FLOOR, 39, 42, 59, 74] {
            assert!(
                !asks_for_context(None, lexical, MATCH_FLOOR),
                "lexicale {lexical} werd als zwakke match gelezen"
            );
        }
    }

    /// `min_match` van de aanroeper blijft werken — op de `fit`, waar hij hoort.
    #[test]
    fn a_caller_can_still_tighten_the_bar_on_the_models_judgement() {
        assert!(!asks_for_context(Some(80), 80, 75));
        assert!(asks_for_context(Some(80), 80, 90));
        // Maar strenger zijn maakt de lexicale schaal niet ineens bruikbaar.
        assert!(!asks_for_context(None, 80, 90));
    }

    /// Het model mag de vraag stellen, maar niet ongezien.
    ///
    /// De prompt verbiedt vragen om wat de vraag al geeft. De prompt verbood
    /// dat de vorige keer ook, en toch stond hier een sjabloon dat om een
    /// bedrijfsnaam vroeg. Eén verbod is geen garantie; de filter wel.
    #[test]
    fn a_model_question_that_asks_for_what_is_already_there_is_rejected() {
        let parsed = json!({
            "question": { "language": "en", "clarifying_question": null, "recency_days": null },
            "resolved_entities": [{
                "type": "company",
                "name": "Mollie B.V.",
                "country": "NL",
                "domain": "mollie.com",
                "identifiers": {
                    "kvk_number": null, "company_number": null,
                    "lei": null, "vat_number": null, "ticker": null
                }
            }],
            "expected_output": ["registration"]
        });
        // Het model vraagt om precies de drie dingen die de vraag al noemt.
        let raw = json!({
            "context_question": "Which company, in which country, and on which domain?",
            "context_wanted": ["company name", "country", "domain"]
        });

        let ctx = needs_context(
            "weak_match",
            60,
            MATCH_FLOOR,
            Some(&raw),
            Some(&parsed),
            "Where is Mollie B.V. registered?",
        );

        assert_ne!(ctx.written_by, "rerank", "{}", ctx.question);
        assert!(!ctx.question.contains("Which company"));
        // En de terugval doet het wél goed.
        assert_eq!(ctx.wanted, vec!["identifier (registration number)"]);
    }

    /// Eén bruikbaar label is genoeg: dan wint de vraag van het model, want
    /// die zag de kandidaten en het sjabloon niet.
    #[test]
    fn a_model_question_keeps_the_labels_the_question_does_not_answer() {
        let parsed = json!({
            "question": { "language": "en", "clarifying_question": null, "recency_days": null },
            "resolved_entities": [{
                "type": "company",
                "name": "Mollie B.V.",
                "country": "NL",
                "domain": "mollie.com",
                "identifiers": {
                    "kvk_number": null, "company_number": null,
                    "lei": null, "vat_number": null, "ticker": null
                }
            }],
            "expected_output": ["registration"]
        });
        let raw = json!({
            "context_question": "I have the Dutch business register; what is Mollie's KVK number?",
            "context_wanted": ["company name", "country", "KVK-nummer"]
        });

        let ctx = needs_context(
            "weak_match",
            60,
            MATCH_FLOOR,
            Some(&raw),
            Some(&parsed),
            "Where is Mollie B.V. registered?",
        );

        assert_eq!(ctx.written_by, "rerank");
        assert!(ctx.question.contains("KVK number"));
        // De twee labels die de vraag al beantwoordt, zijn eruit.
        assert_eq!(ctx.wanted, vec!["KVK-nummer"]);
    }

    /// Een vraag zonder labels valt niet te toetsen, en blijft dus staan.
    #[test]
    fn a_model_question_without_labels_is_taken_as_written() {
        let parsed = json!({
            "question": { "language": "en", "clarifying_question": null, "recency_days": null },
            "resolved_entities": [],
            "expected_output": []
        });
        let raw = json!({
            "context_question": "Which register should I search?",
            "context_wanted": []
        });

        let ctx = needs_context(
            "weak_match",
            60,
            MATCH_FLOOR,
            Some(&raw),
            Some(&parsed),
            "x",
        );
        assert_eq!(ctx.written_by, "rerank");
        assert_eq!(ctx.question, "Which register should I search?");
    }

    /// "land" mag niet aanslaan op "Nederland".
    ///
    /// Een substring-filter gooit dan precies het label weg dat gevraagd moest
    /// worden, en dat is de fout die deze filter moest repareren, omgekeerd.
    #[test]
    fn the_label_filter_matches_words_not_substrings() {
        assert!(phrase_hit("land of markt", &["land"]));
        assert!(phrase_hit("KVK-nummer", &["kvk"]));
        assert!(!phrase_hit("Nederland", &["land"]));
        assert!(!phrase_hit("company number", &["company name"]));
        assert!(!phrase_hit("landmark", &["land"]));
    }

    /// DE TEGENSTRIJDIGHEID DIE DE BUG MAAKTE, VASTGELEGD.
    ///
    /// De prompt liet het model zelf narekenen of iets `match_floor` haalde
    /// voordat het een `context_question` schreef, terwijl de gateway die
    /// beslissing met haar eigen som al nam. Bij het kleinste meningsverschil
    /// stond er geen vraag klaar. Eén rechter, en dat is de gateway.
    #[test]
    fn the_prompt_never_delegates_the_threshold_to_the_model() {
        let prompt = RERANK_SYSTEM_PROMPT;

        assert!(
            prompt.contains("ALWAYS write `context_question`"),
            "de contextvraag is niet meer voorwaardelijk"
        );
        // Geen enkele formulering die het model weer medebeslisser maakt.
        for delegated in [
            "Null when a candidate reaches",
            "When NO candidate reaches",
            "not a context question",
        ] {
            assert!(
                !prompt.contains(delegated),
                "prompt delegeert de drempel nog: {delegated}"
            );
        }
        // `match_floor` staat niet meer in de invoer, dus de prompt mag er ook
        // niet meer naar verwijzen als iets dat het model kan aflezen.
        assert!(
            !prompt.contains("`match_floor` (given in the input)"),
            "prompt verwijst naar een veld dat niet meer meegestuurd wordt"
        );
        // En de regel die dit hele geval had moeten voorkomen.
        assert!(prompt.contains("never asked for a company name"));
    }

    /// Het schema is óók prompt: het model leest de beschrijvingen.
    #[test]
    fn the_schema_describes_the_same_contract_as_the_prompt() {
        let schema = rerank_schema(5);
        let described = schema["properties"]["context_question"]["description"]
            .as_str()
            .expect("context_question heeft een beschrijving");

        assert!(described.starts_with("Always written"), "{described}");
        assert!(!described.contains("match_floor"), "{described}");
    }

    #[test]
    fn the_models_context_question_wins_when_it_wrote_one() {
        // Het herordeningsmodel zag de kandidaten en kan zeggen wat die wél
        // dekken; die vraag gaat vóór alles wat de lezer of het sjabloon zegt.
        let raw = json!({
            "endpoints": [],
            "missing": [], "follow_up": null,
            "context_question": "Ik heb Nederlandse registers; gaat het om een Nederlands bedrijf?",
            "context_wanted": ["land", "bedrijfsnaam", "", 7]
        });
        let parsed = json!({ "question": {
            "language": "nl",
            "clarifying_question": "Welk bedrijf bedoel je?"
        }});
        let ctx = needs_context(
            "weak_match",
            60,
            MATCH_FLOOR,
            Some(&raw),
            Some(&parsed),
            "x",
        );
        assert_eq!(ctx.written_by, "rerank");
        assert!(ctx.question.starts_with("Ik heb Nederlandse registers"));
        assert_eq!(
            ctx.wanted,
            vec!["land".to_string(), "bedrijfsnaam".to_string()]
        );
        assert_eq!(ctx.reason, "weak_match");
        assert_eq!((ctx.best_match, ctx.floor), (60, MATCH_FLOOR));
    }

    /// Doorvragen mag alleen als antwoorden iets kán veranderen.
    ///
    /// Het geval dat dit afdwingt is echt gebeurd. Iemand vroeg om een bron
    /// die in de catalogus staat, kreeg een wedervraag, antwoordde, en kreeg
    /// dezelfde wedervraag opnieuw — en dat zou oneindig zijn doorgegaan. De
    /// oorzaak zat niet in zijn formulering: er was nooit een shortlist,
    /// omdat het endpoint geen capability-koppeling had en dus buiten de pool
    /// viel. Geen antwoord ter wereld tilt een kandidaat over de lat als er
    /// geen kandidaat is.
    ///
    /// `answerable` is de scheiding, en hij hangt aan één ding: heeft het
    /// herordeningsmodel de kandidaten gezien.
    #[test]
    fn a_question_nobody_can_answer_is_marked_as_such() {
        let parsed = json!({"question": {"language": "en"}});

        // Geen herordening: er was niets om te herordenen. De vraag terug komt
        // van het sjabloon en is een gok.
        let dead_end = needs_context("no_source", 25, MATCH_FLOOR, None, Some(&parsed), "x");
        assert_eq!(dead_end.written_by, "template");
        assert!(
            !dead_end.answerable,
            "zonder shortlist kan geen antwoord de score veranderen; de chat hoort te \
             zeggen dat de catalogus dit niet dekt in plaats van opnieuw te vragen"
        );

        // Wel herordening, met een echte contextvraag: het model zag de
        // kandidaten en weet wat ze dekken, dus antwoorden helpt.
        let raw =
            json!({"context_question": "Do you mean a Dutch company?", "wanted": ["country"]});
        let worth_asking = needs_context(
            "weak_match",
            60,
            MATCH_FLOOR,
            Some(&raw),
            Some(&parsed),
            "x",
        );
        assert_eq!(worth_asking.written_by, "rerank");
        assert!(worth_asking.answerable);
    }

    #[test]
    fn a_blank_model_question_falls_through_to_the_reader() {
        let raw = json!({ "endpoints": [], "context_question": "   ", "context_wanted": [] });
        let parsed = json!({ "question": {
            "language": "en",
            "clarifying_question": "Which company do you mean?"
        }});
        let ctx = needs_context("no_source", 12, MATCH_FLOOR, Some(&raw), Some(&parsed), "x");
        assert_eq!(ctx.written_by, "reader");
        assert_eq!(ctx.question, "Which company do you mean?");
        // Zonder labels van het model komen de standaardlabels mee, in de
        // taal die de lezer noemde.
        assert!(ctx.wanted.iter().any(|w| w.starts_with("country")));
    }

    #[test]
    fn with_no_model_at_all_the_template_asks_in_the_askers_language() {
        // Nederlands, geraden aan de functiewoorden — geen lezer, dus geen
        // taalcode.
        let nl = needs_context(
            "no_source",
            0,
            MATCH_FLOOR,
            None,
            None,
            "wat is de omzet van Mollie",
        );
        assert_eq!(nl.written_by, "template");
        assert!(
            nl.question.starts_with("Ik heb geen bron gevonden"),
            "{}",
            nl.question
        );
        assert!(nl.question.contains("wat is de omzet van Mollie"));
        assert!(nl.wanted.iter().any(|w| w.starts_with("land")));

        // Engels, en de andere reden krijgt de andere zin.
        let en = needs_context(
            "weak_match",
            55,
            MATCH_FLOOR,
            None,
            None,
            "revenue of Mollie",
        );
        assert!(
            en.question.starts_with("I found sources that come close"),
            "{}",
            en.question
        );
        assert!(en.wanted.iter().any(|w| w.starts_with("country")));

        // De lezer's taalcode wint van de gok.
        let parsed = json!({ "question": { "language": "nl", "clarifying_question": null } });
        let told = needs_context(
            "no_source",
            0,
            MATCH_FLOOR,
            None,
            Some(&parsed),
            "revenue of Mollie",
        );
        assert!(told.question.starts_with("Ik heb geen bron gevonden"));
    }

    /// HET GEVAL DAT DIT SJABLOON KAPOT LIET ZIEN.
    ///
    /// "Where is Mollie B.V. registered?" kreeg een vraag terug om onder meer
    /// een bedrijfsnaam — terwijl diezelfde zin twee woorden eerder tussen
    /// aanhalingstekens werd teruggeciteerd. De lezer had de naam, het land én
    /// het domein al; het sjabloon las hem alleen niet.
    #[test]
    fn the_template_never_asks_for_what_the_question_already_gives() {
        let parsed = json!({
            "question": {
                "language": "en",
                "clarifying_question": null,
                "recency_days": null
            },
            "resolved_entities": [{
                "type": "company",
                "name": "Mollie B.V.",
                "country": "NL",
                "domain": "mollie.com",
                "identifiers": {
                    "kvk_number": null,
                    "company_number": null,
                    "lei": null,
                    "vat_number": null,
                    "ticker": null
                }
            }],
            "expected_output": ["registration"]
        });

        let ctx = needs_context(
            "weak_match",
            60,
            MATCH_FLOOR,
            None,
            Some(&parsed),
            "Where is Mollie B.V. registered?",
        );

        assert_eq!(ctx.written_by, "template");
        // Het onderwerp komt terug, zodat een verkeerde resolutie te
        // corrigeren is.
        assert!(
            ctx.question
                .contains("I read this as being about Mollie B.V."),
            "{}",
            ctx.question
        );
        // En niets waar de vraag zelf al antwoord op geeft.
        for already_known in [
            "company name",
            "country or market",
            "kind of data",
            "domain",
            "ticker",
            "time window",
        ] {
            assert!(
                !ctx.question.contains(already_known),
                "asked for {already_known}, which the question already gives: {}",
                ctx.question
            );
        }
        // Wat écht ontbreekt, blijft staan: zonder nummer is er geen lookup.
        assert_eq!(ctx.wanted, vec!["identifier (registration number)"]);
        assert_eq!(
            ctx.question,
            "I found sources that come close, but none that clearly answers \
             \"Where is Mollie B.V. registered?\". I read this as being about \
             Mollie B.V. Can you tell me more: identifier (registration number)?"
        );
    }

    /// Een volledige vraag zonder bron is ONS gat, niet dat van de steller.
    #[test]
    fn with_nothing_left_to_ask_the_template_stops_asking() {
        let parsed = json!({
            "question": { "language": "en", "clarifying_question": null, "recency_days": null },
            "resolved_entities": [{
                "type": "company",
                "name": "Mollie B.V.",
                "country": "NL",
                "domain": "mollie.com",
                "identifiers": {
                    "kvk_number": "30204462",
                    "company_number": null,
                    "lei": null,
                    "vat_number": null,
                    "ticker": null
                }
            }],
            "expected_output": ["registration"]
        });

        let ctx = needs_context(
            "weak_match",
            60,
            MATCH_FLOOR,
            None,
            Some(&parsed),
            "Where is Mollie B.V. registered?",
        );

        assert!(ctx.wanted.is_empty(), "{:?}", ctx.wanted);
        assert!(
            ctx.question.contains("which register or dataset"),
            "{}",
            ctx.question
        );
        assert!(!ctx.question.contains("Can you tell me more"));
    }

    /// De vage vraag verandert niet: die verdient alle vier de labels.
    #[test]
    fn a_question_that_names_nothing_still_gets_the_full_form() {
        let parsed = json!({
            "question": { "language": "en", "clarifying_question": null, "recency_days": null },
            "resolved_entities": [],
            "expected_output": []
        });

        let ctx = needs_context(
            "no_source",
            0,
            MATCH_FLOOR,
            None,
            Some(&parsed),
            "give me data about a company",
        );

        assert_eq!(ctx.wanted.len(), 4);
        assert!(ctx.question.contains("company name"));
        assert!(!ctx.question.contains("I read this as being about"));
    }

    /// Een ticker vragen over een persoon is dezelfde soort ruis.
    #[test]
    fn a_person_is_never_asked_for_a_registration_number() {
        let parsed = json!({
            "question": { "language": "nl", "clarifying_question": null, "recency_days": null },
            "resolved_entities": [{
                "type": "person",
                "name": "Adriaan Mol",
                "country": "NL",
                "domain": null,
                "identifiers": {
                    "kvk_number": null,
                    "company_number": null,
                    "lei": null,
                    "vat_number": null,
                    "ticker": null
                }
            }],
            "expected_output": ["leadership"]
        });

        let ctx = needs_context(
            "weak_match",
            60,
            MATCH_FLOOR,
            None,
            Some(&parsed),
            "waar woont Adriaan Mol",
        );

        assert!(ctx.wanted.is_empty(), "{:?}", ctx.wanted);
        assert!(!ctx.question.contains("KVK"));
        assert!(ctx.question.contains("Adriaan Mol"));
    }

    #[test]
    fn the_language_guess_needs_two_dutch_markers() {
        assert!(looks_dutch("wie is de eigenaar van Mollie"));
        assert!(looks_dutch("hoeveel mensen werken er bij Mollie"));
        assert!(!looks_dutch("who owns Mollie"));
        // Eén "de" is ook Frans of Spaans.
        assert!(!looks_dutch("tour de france results"));
    }

    #[test]
    fn a_long_question_is_quoted_short_in_the_template() {
        let long = "a ".repeat(200);
        let ctx = needs_context("no_source", 0, MATCH_FLOOR, None, None, &long);
        assert!(ctx.question.len() < 400);
    }

    /// De drie vormen die de catalogus echt bevat. Zie [`required_inputs`].
    #[test]
    fn every_shape_in_the_catalogue_yields_its_required_fields() {
        let mut o = offer("acme", "/x", "x", 0.01, "x.y");

        // A — JSON Schema met een expliciete lijst. BlockDB, PulseNetwork.
        o.query_params = json!({
            "type": "object",
            "required": ["chain_id"],
            "properties": { "chain_id": { "type": "number" } }
        });
        assert_eq!(required_inputs(&o), vec!["chain_id".to_string()]);

        // B — vlag per veld binnen `properties`. WebberSites.
        o.query_params = json!({
            "type": "object",
            "required": [],
            "properties": {
                "name": { "type": "string", "required": true },
                "limit": { "type": "number", "required": false }
            }
        });
        assert_eq!(required_inputs(&o), vec!["name".to_string()]);

        // C — platte map zonder omhulsel. KVK: de eigenschappen staan in de
        // wortel, naast metavelden als `type`.
        o.query_params = json!({
            "kvkNummer": { "type": "string", "required": true },
            "geoData": { "type": "boolean", "required": false }
        });
        assert_eq!(required_inputs(&o), vec!["kvkNummer".to_string()]);
    }

    #[test]
    fn both_columns_are_read_and_merged() {
        // Een endpoint mag beide hebben — WebberSites heeft padparameters in
        // `query_params` en een body daarnaast. Eén van de twee lezen laat de
        // helft van de verplichte velden vallen.
        let mut o = offer("acme", "/x", "x", 0.01, "x.y");
        o.request_body = json!({ "type": "object", "required": ["url"] });
        o.query_params = json!({ "type": "object", "required": ["collection"] });
        assert_eq!(
            required_inputs(&o),
            vec!["collection".to_string(), "url".to_string()]
        );
    }

    #[test]
    fn a_meta_field_never_becomes_a_required_input() {
        // De platte vorm heeft geen omhulsel, dus `type` en `$schema` staan
        // náást de echte velden. Zonder de object-eis zou "type" een veld worden
        // waar de vervolgvraag om vraagt.
        let mut o = offer("acme", "/x", "x", 0.01, "x.y");
        o.query_params = json!({
            "type": "object",
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "Retrieves insider transactions",
            "ticker": { "type": "string", "required": true }
        });
        assert_eq!(required_inputs(&o), vec!["ticker".to_string()]);
    }

    #[test]
    fn required_inputs_ignores_anything_that_is_not_a_name() {
        // Vrije JSON uit de catalogus. Een getal of een null in de lijst mag
        // geen veldnaam worden in de prompt.
        let mut o = offer("acme", "/x", "x", 0.01, "x.y");
        o.query_params = json!({ "required": ["domain", 7, null, "vat"] });
        assert_eq!(
            required_inputs(&o),
            vec!["domain".to_string(), "vat".to_string()]
        );
    }

    #[test]
    fn an_endpoint_with_only_optional_parameters_requires_nothing() {
        // Echt bestaand geval: /api/bulletin heeft alleen filters. Een lege
        // lijst is een antwoord, geen ontbrekende data — en dus geen
        // vervolgvraag.
        let mut o = offer("acme", "/bulletin", "x", 0.01, "x.y");
        o.query_params = json!({
            "type": "object",
            "required": [],
            "properties": { "lang": { "type": "string", "required": false } }
        });
        assert!(required_inputs(&o).is_empty());
    }

    #[test]
    fn the_best_relevance_is_the_highest_not_the_first() {
        // De shortlist sorteert op alle termen, de relevantie meet de kern —
        // dus de bovenste kandidaat is niet de meest relevante. Op de eerste
        // kijken wees vragen af die de catalogus wél kon beantwoorden.
        let offers = vec![
            // Scoort hoog op de parser-uitbreiding, laag op de vraag zelf.
            offer(
                "newsapi",
                "/markets/news",
                "market news feed",
                0.01,
                "news.search",
            ),
            // Scoort op het woord dat de vraag echt stelde.
            offer(
                "cityfalcon",
                "/insider_transactions",
                "insider transactions",
                0.02,
                "finance.lookup",
            ),
        ];
        let terms = Terms {
            core: terms("insider transactions"),
            expansion: terms("market news feed"),
        };
        let pairs = shortlist(&offers, &terms, 10);

        let first = pairs.first().map(|(c, _)| c.relevance).unwrap_or(0);
        let best = best_relevance(&pairs);
        assert!(best >= first);
        assert!(
            best >= RELEVANCE_FLOOR,
            "de catalogus heeft dit endpoint; {best} moet de grens halen"
        );
    }

    #[test]
    fn a_dutch_question_is_measured_on_what_it_wants_not_on_dutch_words() {
        // De catalogus is Engels. Zonder `expected_output` in de kern scoort
        // elke Nederlandse vraag op woorden die in geen enkel pad staan, en de
        // gebonden bonus tilt een nul niet over de drempel.
        let parsed = json!({
            "question": {
                "text": "wie is de eigenaar van Mollie",
                "text_en": "who owns Mollie",
                "intent": "company_research"
            },
            "execution_plan": [{ "step": 1, "capability": "company_intel.lookup", "subject": "Mollie" }],
            "resolved_entities": [{ "type": "company", "name": "Mollie" }],
            "expected_output": ["ownership"]
        });
        let t = search_terms("wie is de eigenaar van Mollie", &parsed);
        // De vertaling en het gevraagde gegeven staan allebei in de kern.
        assert!(t.core.contains(&"owns".to_string()), "core: {:?}", t.core);
        assert!(t.core.contains(&"ownership".to_string()));
        // De gevolgtrekkingen blijven uitbreiding: die mogen de meetlat niet zijn.
        assert!(t.expansion.iter().any(|x| x == "intel.lookup"));

        let o = offer(
            "filingspulse",
            "/api/filings/ownership",
            "ownership data",
            0.02,
            "c.lookup",
        );
        assert!(
            relevance_with(&o, &t) >= RELEVANCE_FLOOR,
            "de catalogus biedt dit letterlijk aan"
        );
    }

    #[test]
    fn a_question_the_catalogue_cannot_serve_still_falls_through() {
        // De keerzijde van de test hierboven: `expected_output` in de kern mag
        // de ondergrens niet uithollen. "recipe" komt in geen enkel pad voor.
        let parsed = json!({
            "question": {
                "text": "recept voor appeltaart",
                "text_en": "recipe for apple pie",
                "intent": "cooking"
            },
            "execution_plan": [],
            "resolved_entities": [],
            "expected_output": ["recipe"]
        });
        let t = search_terms("recept voor appeltaart", &parsed);
        let o = offer(
            "filingspulse",
            "/api/filings/ownership",
            "ownership data",
            0.02,
            "c.lookup",
        );
        assert!(relevance_with(&o, &t) < RELEVANCE_FLOOR);
    }

    #[test]
    fn stopwords_do_not_become_search_terms() {
        // Without this every endpoint scores on "the" and the shortlist is noise.
        let t = terms("wie is de eigenaar van Mollie");
        assert!(t.contains(&"eigenaar".to_string()));
        assert!(t.contains(&"mollie".to_string()));
        assert!(!t.contains(&"de".to_string()));
        assert!(!t.contains(&"van".to_string()));
    }

    #[test]
    fn a_path_hit_outweighs_a_description_hit() {
        // A path was chosen; a description was written. The first is the
        // sharper signal, so it counts double.
        let terms = vec!["sanctions".to_string()];
        let in_path = offer("x", "/api/sanctions", "generic screening", 0.01, "c");
        let in_prose = offer("x", "/api/screen", "checks sanctions lists", 0.01, "c");
        assert!(score(&in_path, &terms) > score(&in_prose, &terms));
    }

    #[test]
    fn longer_terms_weigh_more() {
        let specific = vec!["jaarrekening".to_string()];
        let generic = vec!["data".to_string()];
        let o = offer("kvk", "/api/v1/jaarrekening/data", "", 0.05, "c");
        assert!(score(&o, &specific) > score(&o, &generic));
    }

    #[test]
    fn irrelevant_endpoints_are_dropped_not_ranked_last() {
        // A zero score means "does not match", and a non-match has no business
        // in a shortlist a model then has to reject one by one.
        let offers = vec![
            offer("kvk", "/api/v2/zoeken", "search dutch companies", 0.02, "c"),
            offer("pulse", "/api/football/odds", "match odds", 0.10, "c"),
        ];
        let list = shortlist(&offers, &core("dutch company search"), 10);
        assert_eq!(list.len(), 1);
        assert!(list[0].0.resource.contains("kvk"));
    }

    #[test]
    fn a_tie_is_broken_by_price_then_path() {
        // Same score must give the same order every time, not the database's.
        let offers = vec![
            offer("a", "/api/search", "company search", 0.50, "c"),
            offer("b", "/api/search", "company search", 0.01, "c"),
        ];
        let list = shortlist(&offers, &core("company search"), 10);
        assert_eq!(list[0].0.price, "$0.01");
    }

    #[test]
    fn one_endpoint_takes_one_place() {
        // An endpoint mapped to several capabilities produces several rows. Left
        // alone, the same KVK call filled three of five slots and the answer
        // looked richer than it was.
        let mut a = offer(
            "kvk",
            "/api/v2/zoeken",
            "search dutch companies",
            0.02,
            "company_registry.search",
        );
        let mut b = a.clone();
        b.capability_slug = "company_intel.search".into();
        let mut c = a.clone();
        c.capability_slug = "people.search".into();
        let id = uuid::Uuid::new_v4();
        a.endpoint_id = Some(id);
        b.endpoint_id = Some(id);
        c.endpoint_id = Some(id);

        let rows = vec![a, b, c];
        let list = shortlist(&rows, &core("dutch company search"), 10);
        assert_eq!(
            list.len(),
            1,
            "hetzelfde endpoint mag maar één plek innemen"
        );
    }

    #[test]
    fn a_candidate_carries_its_own_row() {
        // The id the model names is the index in both lists; if they ever drift,
        // a pick resolves to the wrong endpoint and nothing would say so.
        let offers = vec![
            offer("kvk", "/api/v2/zoeken", "search dutch companies", 0.02, "c"),
            offer(
                "kvk",
                "/api/v1/jaarrekeningen",
                "annual accounts",
                0.07,
                "c",
            ),
        ];
        let list = shortlist(&offers, &core("dutch company annual accounts search"), 10);
        for (candidate, row) in &list {
            assert_eq!(candidate.resource, row.call_url.clone().unwrap());
        }
    }

    #[test]
    fn the_shortlist_is_capped() {
        let offers: Vec<_> = (0..SHORTLIST + 40)
            .map(|i| offer("x", &format!("/api/company/{i}"), "company data", 0.01, "c"))
            .collect();
        assert_eq!(
            shortlist(&offers, &core("company"), SHORTLIST).len(),
            SHORTLIST
        );
    }

    #[test]
    fn free_endpoints_say_free() {
        let o = offer("x", "/api/health", "liveness", 0.0, "c");
        assert_eq!(price_label(&o), "free");
        assert_eq!(price_label(&offer("x", "/p", "d", 0.01, "c")), "$0.01");
        assert_eq!(price_label(&offer("x", "/p", "d", 0.15, "c")), "$0.15");
        // Geen $0.0100.
        assert!(!price_label(&offer("x", "/p", "d", 0.1, "c")).ends_with('0'));
    }

    #[test]
    fn search_terms_keep_the_raw_question() {
        // The model may normalise "Mollie" to "Mollie B.V."; the catalogue has
        // the word from the question.
        let parsed = json!({
            "question": { "text": "Who owns Mollie B.V.?", "intent": "company_research" },
            "execution_plan": [{ "capability": "ubo_lookup", "provider": "kvk" }],
            "resolved_entities": [{ "type": "company", "name": "Mollie B.V.", "domain": "mollie.com" }],
            "expected_output": { "ownership": {} }
        });
        let t = search_terms("wie bezit Mollie", &parsed).all();
        assert!(t.contains(&"mollie".to_string()));
        assert!(t.contains(&"ubo_lookup".to_string()) || t.contains(&"ubo".to_string()));
        assert!(t.contains(&"ownership".to_string()));
        assert!(t.contains(&"mollie.com".to_string()));
    }

    // --- herordening --------------------------------------------------------

    fn fixture() -> (Vec<CapabilityOffer>, Vec<Candidate>) {
        let offers = vec![
            offer(
                "kvk",
                "/api/v2/zoeken",
                "search dutch companies",
                0.02,
                "company_registry.search",
            ),
            offer(
                "kvk",
                "/api/v1/jaarrekeningen",
                "annual accounts",
                0.07,
                "company_registry.lookup",
            ),
        ];
        let list = shortlist(&offers, &core("dutch company annual accounts search"), 10)
            .into_iter()
            .map(|(c, _)| c)
            .collect();
        (offers, list)
    }

    #[test]
    fn the_rerank_answer_is_mapped_back_to_real_endpoints() {
        let (offers, list) = fixture();
        let refs: Vec<&CapabilityOffer> = offers.iter().collect();
        let raw = json!({ "endpoints": [
            { "id": 0, "capability": "company_registry.search", "reason": "Resolves the KVK number first." }
        ]});
        let out = read_rerank(&raw, &list, &refs, TOP_N);
        assert_eq!(out.len(), 1);
        assert!(out[0].reason.contains("KVK"));
        assert!(out[0].candidate_id.is_some());
    }

    #[test]
    fn an_invented_id_is_skipped_not_fabricated() {
        // A model naming a candidate that does not exist must shorten the
        // answer, never produce a resource nobody offered.
        let (offers, list) = fixture();
        let refs: Vec<&CapabilityOffer> = offers.iter().collect();
        let raw = json!({ "endpoints": [
            { "id": 99, "capability": "x", "reason": "nonsense" },
            { "id": 0,  "capability": "y", "reason": "fine" }
        ]});
        let out = read_rerank(&raw, &list, &refs, TOP_N);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].reason, "fine");
    }

    #[test]
    fn a_repeated_id_is_returned_once() {
        let (offers, list) = fixture();
        let refs: Vec<&CapabilityOffer> = offers.iter().collect();
        let raw = json!({ "endpoints": [
            { "id": 0, "capability": "a", "reason": "first" },
            { "id": 0, "capability": "a", "reason": "again" }
        ]});
        assert_eq!(read_rerank(&raw, &list, &refs, TOP_N).len(), 1);
    }

    #[test]
    fn a_shapeless_answer_yields_nothing_rather_than_guesses() {
        let (offers, list) = fixture();
        let refs: Vec<&CapabilityOffer> = offers.iter().collect();
        assert!(read_rerank(&json!({}), &list, &refs, TOP_N).is_empty());
        assert!(read_rerank(&json!({ "endpoints": "nope" }), &list, &refs, TOP_N).is_empty());
    }

    #[test]
    fn without_a_model_the_lexical_order_still_answers() {
        // The fallback must return real endpoints and say plainly that no model
        // ranked them — a silent downgrade reads as a worse model, not a
        // missing one.
        let (offers, list) = fixture();
        let refs: Vec<&CapabilityOffer> = offers.iter().collect();
        let out = without_rerank(&list, &refs, TOP_N);
        assert_eq!(out.len(), list.len().min(TOP_N));
        assert!(out[0].reason.contains("not ranked by a model"));
        assert!(out[0].candidate_id.is_some());
    }

    #[test]
    fn relevance_is_a_percentage_of_what_was_asked() {
        // 100 means every word of the question came back in the path. It is the
        // ceiling, not the expectation — which is why the floor sits at 45.
        let terms = terms("sanctions screening");
        let perfect = offer("x", "/api/sanctions/screening", "", 0.01, "c");
        let partial = offer("x", "/api/sanctions", "", 0.01, "c");
        let none = offer("x", "/api/football/odds", "", 0.01, "c");

        assert_eq!(relevance(&perfect, &terms), 100);
        assert!(relevance(&partial, &terms) > 0 && relevance(&partial, &terms) < 100);
        assert_eq!(relevance(&none, &terms), 0);
    }

    #[test]
    fn relevance_does_not_depend_on_how_many_words_were_asked() {
        // Without normalising, a long question scores higher on everything and
        // one floor cannot serve both. Same match quality must give the same
        // number whether the question had two words or six.
        let short = offer("x", "/api/company", "", 0.01, "c");
        let long = offer(
            "x",
            "/api/company/annual/accounts/dutch/register",
            "",
            0.01,
            "c",
        );
        assert_eq!(relevance(&short, &terms("company")), 100);
        assert_eq!(
            relevance(&long, &terms("company annual accounts dutch register")),
            100
        );
    }

    #[test]
    fn an_empty_question_scores_zero_rather_than_dividing_by_zero() {
        assert_eq!(relevance(&offer("x", "/p", "d", 0.01, "c"), &[]), 0);
        assert_eq!(max_score(&[]), 0);
    }

    #[test]
    fn a_richer_parse_never_lowers_the_score() {
        // THE BUG THIS LOCKS DOWN: relevance was "what fraction of ALL terms
        // came back", so every term the parser added made every endpoint look
        // worse. "dutch company annual accounts" scored 73 on its own words and
        // 36 once parsed — and fell under the floor. Understanding a question
        // better must never cost it its answer.
        let o = offer(
            "kvk",
            "/api/v1/jaarrekeningen",
            "annual accounts",
            0.07,
            "c",
        );
        let bare = core("dutch company annual accounts");
        let parsed = Terms {
            core: bare.core.clone(),
            expansion: terms("company_registry lookup ownership mollie.com netherlands"),
        };
        assert!(
            relevance_with(&o, &parsed) >= relevance_with(&o, &bare),
            "parser-termen mogen de score niet verlagen"
        );
    }

    #[test]
    fn an_expansion_hit_lifts_the_score_but_only_so_far() {
        // The expansion is the model's guess at what the question MEANT. It may
        // carry a good candidate over the line; it may not talk a bad one in.
        let o = offer(
            "kvk",
            "/api/v1/jaarrekeningen",
            "annual accounts",
            0.07,
            "c",
        );
        let bare = core("dutch company");
        let helped = Terms {
            core: bare.core.clone(),
            expansion: terms("jaarrekeningen annual accounts"),
        };
        let base = relevance_with(&o, &bare);
        let lifted = relevance_with(&o, &helped);
        assert!(lifted > base, "een rake uitbreiding hoort te helpen");
        assert!(
            lifted <= base.saturating_add(EXPANSION_BONUS),
            "de bonus is begrensd"
        );
    }

    #[test]
    fn the_core_is_the_question_and_the_expansion_is_what_was_concluded() {
        let parsed = json!({
            "question": { "text": "who owns Mollie", "text_en": "who owns Mollie", "intent": "company_research" },
            "execution_plan": [{ "capability": "company_intel.lookup", "subject": "Mollie" }],
            "resolved_entities": [{ "type": "company", "name": "Mollie", "domain": "mollie.com" }],
            "expected_output": ["ownership"]
        });
        let t = search_terms("who owns Mollie", &parsed);

        // De vraag, haar vertaling en wat ze wil: alle drie herformuleringen.
        assert!(t.core.contains(&"owns".to_string()));
        assert!(t.core.contains(&"mollie".to_string()));
        assert!(t.core.contains(&"ownership".to_string()));

        // Wat de parser eróver concludeerde blijft uitbreiding.
        assert!(t.expansion.iter().any(|x| x == "research"));
        assert!(t.expansion.iter().any(|x| x == "mollie.com"));
        assert!(!t.core.iter().any(|x| x == "research"));
    }

    #[test]
    fn the_selected_endpoint_carries_its_score() {
        // A weak pick must look weak. Without this the fifth card reads as
        // confident as the first.
        let (offers, list) = fixture();
        let refs: Vec<&CapabilityOffer> = offers.iter().collect();
        let raw = json!({ "endpoints": [{ "id": 0, "capability": "c", "reason": "r" }] });
        let out = read_rerank(&raw, &list, &refs, TOP_N);
        assert_eq!(out[0].relevance, list[0].relevance);
    }

    #[test]
    fn the_rerank_schema_forbids_extra_fields() {
        let s = rerank_schema(TOP_N);
        assert_eq!(s["additionalProperties"], json!(false));
        assert_eq!(
            s["properties"]["endpoints"]["items"]["additionalProperties"],
            json!(false)
        );
    }

    /// De regressie waar deze weging voor bestaat.
    ///
    /// "validate an IBAN" op de draaiende gateway zette vijf e-mailvalidators
    /// bóven de enige IBAN-validator, omdat `validate` acht letters heeft en
    /// `iban` er vier. Lengte is niet hetzelfde als zeggingskracht.
    #[test]
    fn a_rare_word_outranks_a_long_common_one() {
        let mut offers: Vec<CapabilityOffer> = (0..40)
            .map(|i| {
                offer(
                    &format!("mail{i}"),
                    &format!("/api/{i}/email-validate"),
                    "validate an email address",
                    0.01,
                    "c",
                )
            })
            .collect();
        offers.push(offer(
            "chainverdict",
            "/v1/iban/:iban",
            "structural check of a bank account number",
            0.01,
            "c",
        ));

        let terms = core("validate an iban");

        // Ongewogen wint `validate` het van `iban`, en de IBAN-validator staat
        // achteraan in plaats van vooraan.
        let plain = shortlist(&offers, &terms, 5);
        assert!(!plain[0].0.resource.contains("chainverdict"));

        // Gewogen op de pool staat hij bovenaan: `iban` komt één keer voor,
        // `validate` een-en-veertig keer.
        let weighted = weighted_shortlist(&offers, &terms, 5);
        assert!(weighted[0].0.resource.contains("chainverdict"));
    }

    #[test]
    fn a_word_the_pool_has_never_seen_does_not_dilute_the_question() {
        // De kop van `RELEVANCE_FLOOR` beschrijft precies dit: woorden die in
        // geen enkel pad voorkomen telden wel mee in de noemer, en lange
        // vragen zakten daardoor door de ondergrens.
        let offers = vec![
            offer("kvk", "/api/v1/jaarrekening", "annual accounts", 0.02, "c"),
            offer("pulse", "/api/football/odds", "match odds", 0.10, "c"),
        ];
        let bare = core("jaarrekening");
        let padded = core("jaarrekening voor deze onderneming alsjeblieft");

        let weights_bare = Weights::over(&offers, &bare);
        let weights_padded = Weights::over(&offers, &padded);

        assert_eq!(
            relevance_weighted(&offers[0], &bare.core, &weights_bare),
            relevance_weighted(&offers[0], &padded.core, &weights_padded),
            "words nothing in the pool uses must not move the score"
        );
    }

    #[test]
    fn uniform_weights_reproduce_the_unweighted_sum_exactly() {
        // De brug tussen oud en nieuw: `score` is nu een gewogen som met
        // gewicht één, en moet tot op het punt hetzelfde getal geven.
        let o = offer(
            "kvk",
            "/api/v1/jaarrekening/data",
            "company data",
            0.05,
            "c",
        );
        let t = terms("jaarrekening data");
        assert_eq!(
            score(&o, &t),
            weighted_score(&o, &t, &Weights::uniform()).round() as usize
        );
        assert_eq!("jaarrekening".len() * 2 + "data".len() * 2, score(&o, &t));
    }

    #[test]
    fn a_term_nothing_matches_weighs_nothing_and_one_everything_matches_weighs_little() {
        let offers = vec![
            offer("a", "/api/company/search", "company search", 0.01, "c"),
            offer("b", "/api/company/filings", "company filings", 0.01, "c"),
        ];
        let t = core("company helicopter");
        let w = Weights::over(&offers, &t);

        assert_eq!(w.of("helicopter"), 0.0, "unseen word is not a yardstick");
        assert_eq!(
            w.of("company"),
            MIN_WEIGHT,
            "a word in everything barely counts"
        );
        // ...maar het blijft een meetlat: de noemer is niet nul, dus er komt
        // een echte relevantie uit.
        assert!(relevance_weighted(&offers[0], &t.core, &w) > 0);
    }
}
