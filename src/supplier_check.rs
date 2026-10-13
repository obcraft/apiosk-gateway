//! "Can I safely pay this supplier?" — a deterministic workflow preset.
//!
//! The parser selects one preset goal. Code, not the model, then reads the
//! identifiers that appear literally in the question, expands the goal into
//! concrete registry, VAT, sanctions, domain and IBAN checks, and finally
//! evaluates the saved source documents into a HOLD/REVIEW/OK verdict. The
//! verdict never contains a model-written fact: every sentence is rendered from
//! a validated source field or from the invoice text itself.
use crate::{
    catalog::Catalog,
    parser::{Goal, Intent},
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

pub const PRESET: &str = "payment.supplier-check";
pub const DOMAIN_REGISTRATION: &str = "domain.registration";
pub const DOMAIN_MAIL_DNS: &str = "domain.mail-dns";
pub const DOMAIN_FACT: &str = "domain.name";
const KVK: &str = "kvk-dutch-business-register";
const CH: &str = "companies-house";
/// First-party IBAN check (structure, check digits, bank); Sirenic is the fallback.
const IBAN_CHECK: &str = "iban.validate";
const IBAN_FACT: &str = "bank.iban";
const SIRENIC_IBAN_CHECK: &str = "company.sirenic-iban-verifier-iban";
const SIRENIC_IBAN_INPUT: &str = "company.sirenic.input.iban_verifier_iban.iban";
/// TLDs whose registry publishes an RDAP registration event (IANA bootstrap,
/// checked 2026-09-15). .de/.eu/.be publish no registration date.
pub const RDAP_TLDS: &[&str] = &["nl", "com", "net", "org", "uk", "fr", "app", "dev"];
const FREE_MAIL: &[&str] = &["gmail.com", "googlemail.com", "outlook.com", "hotmail.com", "hotmail.nl", "live.com", "live.nl", "yahoo.com", "icloud.com", "me.com", "proton.me", "protonmail.com", "gmx.com", "gmx.de", "ziggo.nl", "kpnmail.nl", "planet.nl", "home.nl", "aol.com"];
const VAT_CODES: &[&str] = &["AT", "BE", "BG", "CY", "CZ", "DE", "DK", "EE", "EL", "ES", "FI", "FR", "HR", "HU", "IE", "IT", "LT", "LU", "LV", "MT", "NL", "PL", "PT", "RO", "SE", "SI", "SK", "XI"];

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Invoice {
    pub entity_ref: Uuid,
    pub name: Option<String>,
    pub kvk: Option<String>,
    pub uk_company: Option<String>,
    pub vat: Option<String>,
    pub iban: Option<String>,
    pub domain: Option<String>,
    #[serde(default)]
    pub free_mail_domain: Option<String>,
    pub postcode: Option<String>,
    pub house_number: Option<String>,
    pub invoice_number: Option<String>,
    pub language: String,
}
impl Invoice {
    pub fn dutch(&self) -> bool {
        self.language == "nl"
    }
    pub fn country(&self) -> Option<String> {
        if self.kvk.is_some() {
            return Some("NL".into());
        }
        if self.uk_company.is_some() {
            return Some("GB".into());
        }
        self.vat.as_ref().map(|v| v[..2].replace("EL", "GR"))
    }
    /// Stable identities used to find earlier invoices from this supplier.
    pub fn keys(&self) -> Vec<String> {
        let mut keys = vec![];
        if let Some(v) = &self.kvk { keys.push(format!("kvk:{v}")) }
        if let Some(v) = &self.uk_company { keys.push(format!("gb:{v}")) }
        if let Some(v) = &self.vat { keys.push(format!("vat:{v}")) }
        if let Some(v) = self.name.as_deref().map(normalize_name).filter(|v| v.len() >= 3) { keys.push(format!("name:{v}")) }
        keys
    }
}

pub struct Expansion {
    pub invoice: Invoice,
    pub facts: Vec<(String, Value)>,
}

// ---------------------------------------------------------------- parsing

fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '(' | ')' | '"' | '\'' | '<' | '>' | '[' | ']' | '|'))
        .map(|t| t.trim_matches(|c: char| matches!(c, '.' | ':' | '!' | '?')).to_owned())
        .filter(|t| !t.is_empty())
        .collect()
}
fn after_keyword<'a>(tokens: &'a [String], keywords: &[&str], window: usize) -> Vec<&'a str> {
    let lower: Vec<String> = tokens.iter().map(|t| t.to_lowercase()).collect();
    let mut hits = vec![];
    for (i, t) in lower.iter().enumerate() {
        let single = keywords.contains(&t.as_str());
        let pair = lower.get(i + 1).is_some_and(|n| keywords.contains(&format!("{t} {n}").as_str()));
        let first = if pair { i + 2 } else { i + 1 };
        if single || pair {
            hits.extend(first..(first + window).min(tokens.len()));
        }
    }
    hits.into_iter().map(|i| tokens[i].as_str()).collect()
}
fn iban_length(country: &str) -> Option<usize> {
    Some(match country {
        "NO" => 15, "BE" => 16, "DK" | "FI" | "FO" | "GL" | "NL" => 18, "MK" | "SI" => 19,
        "AT" | "BA" | "EE" | "KZ" | "LT" | "LU" | "XK" => 20, "CH" | "HR" | "LI" | "LV" => 21,
        "BG" | "BH" | "CR" | "DE" | "GB" | "GE" | "IE" | "ME" | "RS" => 22, "GI" | "IL" | "AE" => 23,
        "CZ" | "ES" | "MD" | "PK" | "RO" | "SA" | "SE" | "SK" | "TN" | "VG" => 24, "PT" => 25, "IS" | "TR" => 26,
        "FR" | "GR" | "IT" | "MC" | "MR" | "SM" => 27, "AL" | "AZ" | "CY" | "DO" | "GT" | "HU" | "LB" | "PL" => 28,
        "BR" | "PS" | "QA" => 29, "JO" | "KW" | "MU" => 30, "MT" => 31,
        _ => return None,
    })
}
pub fn iban_valid(iban: &str) -> bool {
    let iban = iban.to_ascii_uppercase();
    if iban.len() < 15 || !iban.bytes().all(|b| b.is_ascii_alphanumeric()) || iban_length(&iban[..2]) != Some(iban.len()) {
        return false;
    }
    let rearranged = format!("{}{}", &iban[4..], &iban[..4]);
    let mut remainder: u32 = 0;
    for c in rearranged.chars() {
        let value = if c.is_ascii_digit() { c.to_digit(10).unwrap() } else { c as u32 - 55 };
        for d in value.to_string().chars() {
            remainder = (remainder * 10 + d.to_digit(10).unwrap()) % 97;
        }
    }
    remainder == 1
}
/// IBANs are often printed in groups of four. Join adjacent groups only while
/// the result can still become a valid IBAN for its country.
fn find_iban(tokens: &[String]) -> Option<String> {
    let mut misprinted = None;
    for start in 0..tokens.len() {
        let first = tokens[start].to_ascii_uppercase();
        let b = first.as_bytes();
        if first.len() < 4 || !first.is_ascii() || !b[0].is_ascii_alphabetic() || !b[1].is_ascii_alphabetic() || !b[2].is_ascii_digit() || !b[3].is_ascii_digit() {
            continue;
        }
        let Some(len) = iban_length(&first[..2]) else { continue };
        let mut joined = String::new();
        for token in &tokens[start..tokens.len().min(start + 10)] {
            let part: String = token.to_ascii_uppercase().chars().filter(|c| *c != '-' && *c != '.').collect();
            if !part.bytes().all(|b| b.is_ascii_alphanumeric()) { break }
            joined.push_str(&part);
            if joined.len() >= len { break }
        }
        if joined.len() == len && iban_valid(&joined) {
            return Some(joined);
        }
        // A well-formed IBAN with wrong check digits is itself a red flag; keep
        // it so the verdict can report it instead of silently ignoring it.
        let groups = tokens[start..].iter().take_while(|t| t.len() <= 4 || t.len() == len).count();
        if misprinted.is_none() && joined.len() == len && joined[2..4].bytes().all(|b| b.is_ascii_digit()) && (tokens[start].len() == len || groups >= 4) {
            misprinted = Some(joined);
        }
    }
    misprinted
}
fn vat_shape(c: &str) -> bool {
    if c.len() < 4 || c.len() > 16 || !c.is_char_boundary(2) || !VAT_CODES.contains(&&c[..2]) || !c[2..].bytes().all(|b| b.is_ascii_alphanumeric()) {
        return false;
    }
    match &c[..2] {
        "NL" => c.len() == 14 && c[2..11].bytes().all(|b| b.is_ascii_digit()) && &c[11..12] == "B" && c[12..].bytes().all(|b| b.is_ascii_digit()),
        "BE" => c.len() == 12 && c[2..].bytes().all(|b| b.is_ascii_digit()),
        "DE" => c.len() == 11 && c[2..].bytes().all(|b| b.is_ascii_digit()),
        _ => c[2..].bytes().filter(u8::is_ascii_digit).count() >= 6,
    }
}
fn find_vat(tokens: &[String], iban: Option<&str>) -> Option<String> {
    let clean = |t: &str| t.to_ascii_uppercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>();
    let ok = |c: &str| vat_shape(c) && Some(c) != iban && !iban_valid(c);
    // A strictly shaped Dutch/Belgian/German number anywhere, e.g. NL123456789B01.
    for t in tokens {
        let c = clean(t);
        if ok(&c) && matches!(&c[..2], "NL" | "BE" | "DE") {
            return Some(c);
        }
    }
    // Other shapes, or numbers printed with separators, only after a VAT keyword.
    let window = after_keyword(tokens, &["btw", "btw-nummer", "btw-nr", "btw-id", "btw-identificatienummer", "btwnummer", "vat", "vat-nummer", "vat number", "vat-id", "vat no", "tva", "ust-idnr", "iva"], 4);
    for start in 0..window.len() {
        let mut joined = String::new();
        for part in &window[start..] {
            joined.push_str(&clean(part));
            if ok(&joined) {
                return Some(joined);
            }
            if joined.len() > 16 { break }
        }
    }
    None
}
fn find_kvk(tokens: &[String]) -> Option<String> {
    let window = after_keyword(tokens, &["kvk", "kvk-nummer", "kvknummer", "kvk-nr", "kvk nr", "coc", "coc-nummer", "chamber of commerce", "handelsregister"], 4);
    for (i, t) in window.iter().enumerate() {
        let digits: String = t.chars().filter(|c| *c != '.').collect();
        if digits.len() == 8 && digits.bytes().all(|b| b.is_ascii_digit()) {
            return Some(digits);
        }
        if let Some(next) = window.get(i + 1) {
            let joined = format!("{t}{next}");
            if t.len() == 4 && joined.len() == 8 && joined.bytes().all(|b| b.is_ascii_digit()) {
                return Some(joined);
            }
        }
    }
    None
}
fn find_uk_company(tokens: &[String]) -> Option<String> {
    after_keyword(tokens, &["company number", "company no", "companies house", "crn", "company registration", "registered number", "registration number"], 5)
        .into_iter()
        .map(|t| t.to_ascii_uppercase())
        .find(|t| t.len() == 8 && t.bytes().all(|b| b.is_ascii_alphanumeric()) && t.bytes().filter(u8::is_ascii_digit).count() >= 6)
}
fn domain_shape(value: &str) -> Option<String> {
    let mut v = value.trim().to_ascii_lowercase();
    for prefix in ["https://", "http://"] {
        if let Some(rest) = v.strip_prefix(prefix) { v = rest.to_owned() }
    }
    if let Some(i) = v.find(['/', '?', '#']) { v.truncate(i) }
    let v = v.strip_prefix("www.").unwrap_or(&v).trim_end_matches('.').to_owned();
    let labels: Vec<&str> = v.split('.').collect();
    let tld = *labels.last()?;
    let valid = labels.len() >= 2
        && v.len() <= 253
        && tld.len() >= 2
        && tld.bytes().all(|b| b.is_ascii_lowercase())
        && !["pdf", "png", "jpg", "jpeg", "doc", "docx", "xls", "xlsx", "csv", "txt", "html", "zip"].contains(&tld)
        && labels.iter().all(|l| !l.is_empty() && l.len() <= 63 && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') && !l.starts_with('-') && !l.ends_with('-'))
        && labels[labels.len() - 2].len() >= 2;
    valid.then_some(v)
}
fn find_domains(tokens: &[String]) -> (Option<String>, Option<String>) {
    let mut website = None;
    let mut mail = None;
    for t in tokens {
        if let Some((_, host)) = t.rsplit_once('@') {
            if let Some(d) = domain_shape(host) { mail.get_or_insert(d); }
        } else if t.contains('.') {
            if let Some(d) = domain_shape(t) { website.get_or_insert(d); }
        }
    }
    (website, mail)
}
fn find_nl_address(tokens: &[String]) -> (Option<String>, Option<String>) {
    let upper: Vec<String> = tokens.iter().map(|t| t.to_ascii_uppercase()).collect();
    for i in 0..upper.len() {
        let t = &upper[i];
        // Byte offsets below are only valid for ASCII tokens ("100€" is 6 bytes).
        if !t.is_ascii() {
            continue;
        }
        let postcode = if t.len() == 6 && t[..4].bytes().all(|b| b.is_ascii_digit()) && t[4..].bytes().all(|b| b.is_ascii_uppercase()) {
            t.clone()
        } else if t.len() == 4 && t.bytes().all(|b| b.is_ascii_digit()) && upper.get(i + 1).is_some_and(|n| n.len() == 2 && n.bytes().all(|b| b.is_ascii_uppercase())) {
            format!("{t}{}", upper[i + 1])
        } else {
            continue;
        };
        if postcode.starts_with('0') || ["SA", "SD", "SS"].contains(&&postcode[4..]) { continue }
        // House number: a short number directly after a street word, either just
        // before the postcode ("Keizersgracht 126, 1015 CW") or in the few tokens
        // after it ("1015 CW Amsterdam, Keizersgracht 126"). Never IBAN groups.
        let consumed = if t.len() == 6 { 1 } else { 2 };
        let street_number = |j: usize| -> Option<String> {
            let token = upper.get(j)?;
            let street = upper.get(j.checked_sub(1)?)?;
            let digits: String = token.chars().take_while(char::is_ascii_digit).collect();
            let rest = &token[digits.len()..];
            (!digits.is_empty() && digits.len() <= 5 && rest.len() <= 4 && rest.bytes().all(|b| b.is_ascii_alphabetic() || b == b'-')
                && street.len() >= 3 && street.bytes().all(|b| b.is_ascii_alphabetic() || b == b'-' || b == b'.' || b >= 0x80)).then_some(digits)
        };
        let number = (i.saturating_sub(2)..i).rev().find_map(&street_number)
            .or_else(|| (i + consumed + 1..(i + consumed + 5).min(upper.len())).find_map(street_number));
        return (Some(postcode), number);
    }
    (None, None)
}
fn find_uk_postcode(tokens: &[String]) -> Option<String> {
    let upper: Vec<String> = tokens.iter().map(|t| t.to_ascii_uppercase()).collect();
    upper.windows(2).find_map(|w| {
        let (out, inw) = (&w[0], &w[1]);
        if !out.is_ascii() || !inw.is_ascii() {
            return None;
        }
        let out_ok = (2..=4).contains(&out.len()) && out.as_bytes()[0].is_ascii_uppercase() && out.bytes().any(|b| b.is_ascii_digit()) && out.bytes().all(|b| b.is_ascii_alphanumeric());
        let in_ok = inw.len() == 3 && inw.as_bytes()[0].is_ascii_digit() && inw[1..].bytes().all(|b| b.is_ascii_uppercase());
        (out_ok && in_ok).then(|| format!("{out}{inw}"))
    })
}
fn find_invoice_number(tokens: &[String]) -> Option<String> {
    after_keyword(tokens, &["factuur", "factuurnummer", "factuurnr", "invoice", "invoice number", "invoice no", "rechnung", "facture"], 3)
        .into_iter()
        .find(|t| t.len() >= 3 && t.len() <= 40 && t.bytes().any(|b| b.is_ascii_digit()) && t.bytes().all(|b| b.is_ascii_alphanumeric() || b"-/_.".contains(&b)))
        .map(str::to_owned)
}
pub fn language(question: &str) -> &'static str {
    let q = question.to_lowercase();
    let dutch = ["betalen", "leverancier", "factuur", "kan ik", "btw", "adres", "veilig", "rekening", "bedrijf"];
    if dutch.iter().any(|w| q.contains(w)) { "nl" } else { "en" }
}
pub fn parse(question: &str) -> Invoice {
    let tokens = tokens(question);
    let iban = find_iban(&tokens);
    let vat = find_vat(&tokens, iban.as_deref());
    let (website, mail) = find_domains(&tokens);
    let free_mail = mail.clone().filter(|d| FREE_MAIL.contains(&d.as_str()));
    let domain = website.filter(|d| !FREE_MAIL.contains(&d.as_str())).or(mail.filter(|d| !FREE_MAIL.contains(&d.as_str())));
    let (mut postcode, house_number) = find_nl_address(&tokens);
    if postcode.is_none() { postcode = find_uk_postcode(&tokens) }
    Invoice {
        entity_ref: Uuid::nil(),
        name: None,
        kvk: find_kvk(&tokens),
        uk_company: find_uk_company(&tokens),
        vat,
        iban,
        domain,
        free_mail_domain: free_mail,
        postcode,
        house_number,
        invoice_number: find_invoice_number(&tokens),
        language: language(question).into(),
    }
}

// ---------------------------------------------------------------- routing

/// Wording that asks whether an invoice, supplier or bank account can be trusted.
const CHECK_PHRASES: &[&str] = &[
    "veilig betalen", "veilig te betalen", "kan ik betalen", "mag ik betalen", "kan ik deze", "klopt deze factuur", "klopt de factuur", "is deze factuur",
    "factuur controleren", "controleer deze factuur", "controleer de factuur", "factuur checken", "check deze factuur", "check de factuur", "factuur verifiëren",
    "nep factuur", "nepfactuur", "valse factuur", "factuurfraude", "spookfactuur", "phishing factuur", "leverancier controleren", "controleer deze leverancier",
    "leverancier checken", "leverancier verifiëren", "betrouwbare leverancier", "is deze leverancier", "iban gewijzigd", "rekeningnummer gewijzigd",
    "bankgegevens gewijzigd", "nieuw rekeningnummer", "nieuwe iban", "betaalverzoek",
    "safely pay", "safe to pay", "ok to pay", "okay to pay", "should i pay", "can i pay", "is this invoice", "check this invoice", "verify this invoice",
    "check the invoice", "verify the invoice", "invoice legit", "invoice real", "fake invoice", "invoice fraud", "invoice scam", "verify supplier",
    "verify this supplier", "check this supplier", "vendor verification", "verify vendor", "supplier check", "bank details changed", "new bank details",
    "changed bank account", "payee verification", "before paying", "before payment", "voordat ik betaal", "voor betaling",
];
const INVOICE_WORDS: &[&str] = &["factuur", "facturen", "invoice", "leverancier", "supplier", "vendor", "crediteur", "betalen", "betaling", "payment", " pay ", "iban", "rekeningnummer", "bankrekening"];

pub fn mentions_invoice_check(question: &str) -> bool {
    let q = format!(" {} ", question.to_lowercase());
    CHECK_PHRASES.iter().any(|p| q.contains(p))
}

/// The supplier name as written after "van"/"from"/"leverancier", up to punctuation.
fn written_supplier_name(question: &str) -> Option<String> {
    let lower = question.to_lowercase();
    for marker in [" van ", " from ", "leverancier ", "supplier ", "vendor "] {
        let Some(at) = lower.find(marker) else { continue };
        let start = at + marker.len();
        if !question.is_char_boundary(start) { continue }
        let rest = &question[start..];
        let end = rest.char_indices()
            .find(|(i, c)| matches!(c, ',' | '?' | ';' | '(' | '\n') || (*c == '.' && rest[i + 1..].starts_with(' ') && !rest[..*i].to_lowercase().ends_with(" b") && !rest[..*i].to_lowercase().ends_with(" n") && !rest[..*i].to_lowercase().ends_with(".v")))
            .map(|(i, _)| i).unwrap_or(rest.len());
        let name = rest[..end].trim();
        let words = name.split_whitespace().count();
        let digits = name.chars().filter(char::is_ascii_digit).count();
        if (2..=80).contains(&name.len()) && words <= 8 && digits * 3 < name.len() && !["deze", "this", "de", "the", "mijn", "my"].contains(&name.to_lowercase().as_str()) {
            return Some(name.to_owned());
        }
    }
    None
}

/// A deterministic preset intent for an invoice question the model did not
/// route: it must mention invoices/payment and contain a printed identifier.
pub fn fallback_intent(question: &str, catalog: &Catalog) -> Option<Intent> {
    if !available(catalog) { return None }
    let q = format!(" {} ", question.to_lowercase());
    if !(mentions_invoice_check(question) || INVOICE_WORDS.iter().any(|w| q.contains(w))) { return None }
    let invoice = parse(question);
    let identifier = invoice.kvk.clone().or(invoice.uk_company.clone()).or(invoice.vat.clone()).or(invoice.iban.clone())?;
    let label = written_supplier_name(question).unwrap_or(identifier);
    Some(Intent {
        subjects: vec![crate::parser::Subject { entity_ref: None, label, values: BTreeMap::new() }],
        goals: vec![Goal { capability: PRESET.into(), subject: 0, constraints: BTreeMap::new() }],
        answer_operations: vec!["Supplier payment safety verdict".into()],
        unresolved_references: vec![],
    })
}

/// An invoice check asked without any printed identifier: the App should ask
/// for the invoice itself instead of a model guessing at a supplier.
pub fn needs_invoice_upload(question: &str, catalog: &Catalog) -> bool {
    if !available(catalog) || !mentions_invoice_check(question) { return false }
    let invoice = parse(question);
    invoice.kvk.is_none() && invoice.uk_company.is_none() && invoice.vat.is_none() && invoice.iban.is_none()
}
pub fn upload_message(question: &str) -> String {
    if language(question) == "nl" {
        "Upload de factuur, dan lees ik de leveranciersgegevens en controleer ik die voor je.".into()
    } else {
        "Upload the invoice and I will read the supplier details and check them for you.".into()
    }
}

/// Other data requests that must still go through the model, even when the
/// question also mentions an invoice.
const OTHER_REQUESTS: &[&str] = &["jaarrekening", "annual account", "financial statement", "balans", "omzet", "revenue", "winst", "profit", "nieuws", "news", "statistiek", "statistic", "vergelijk", "compare", "werknemers", "employees", "pdf rapport", "report", "bestuurders", "directors", "aandeelhouders", "shareholders"];

/// Fast path: an explicit invoice-check question with a printed identifier and
/// no other data request is routed in code, without a model call (cost and
/// seconds saved on every check).
pub fn direct_intent(question: &str, catalog: &Catalog) -> Option<Intent> {
    let q = question.to_lowercase();
    if !mentions_invoice_check(question) || OTHER_REQUESTS.iter().any(|w| q.contains(w)) {
        return None;
    }
    fallback_intent(question, catalog)
}

/// Replace a partial registry/VAT/sanctions interpretation of an explicit
/// invoice-check question with the complete preset. Returns true when routed.
pub fn route(question: &str, intent: &mut Intent, catalog: &Catalog) -> bool {
    const PARTIAL: &[&str] = &["company.registry-profile", "company_registry.search", "company.uk-registry-search", "company.officers", "company.filing-history", "company.sanctions-screen", "vat.eu.validate", "vat.eu.check", DOMAIN_REGISTRATION, DOMAIN_MAIL_DNS, IBAN_CHECK, SIRENIC_IBAN_CHECK, "company.sirenic-facture-verifier"];
    if !available(catalog) || intent.subjects.is_empty() || intent.goals.iter().any(|g| g.capability == PRESET) || !mentions_invoice_check(question) {
        return false;
    }
    if !intent.goals.iter().all(|g| PARTIAL.contains(&g.capability.as_str()) && g.subject == intent.goals[0].subject) {
        return false;
    }
    let subject = intent.goals.first().map_or(0, |g| g.subject);
    intent.goals = vec![Goal { capability: PRESET.into(), subject, constraints: BTreeMap::new() }];
    intent.unresolved_references.clear();
    true
}

pub fn routing_message(question: &str) -> String {
    if language(question) == "nl" {
        "Ik controleer deze leverancier via het handelsregister, btw, sanctielijsten, domein en bankgegevens en laat je eerst de prijs zien.".into()
    } else {
        "I will check this supplier across the business register, VAT, sanctions, domain and bank details, and show you the price first.".into()
    }
}

// -------------------------------------------------------------- expansion

fn has_service(catalog: &Catalog, capability: &str, source: Option<&str>) -> bool {
    catalog.by_capability.get(capability).is_some_and(|ids| ids.iter().any(|i| source.is_none_or(|s| catalog.services[*i].api_slug == s)))
}
fn schema_ok(catalog: &Catalog, kind: &str, value: &Value) -> bool {
    catalog.vocabulary.get(kind).is_some_and(|ft| catalog_core::facts::schema::validate(&ft.value_schema, value).is_ok())
}
/// Offered to the parser only when at least the core registry or VAT check can run.
pub fn available(catalog: &Catalog) -> bool {
    has_service(catalog, "company.registry-profile", Some(KVK)) || has_service(catalog, "vat.eu.validate", Some("vies"))
}

/// Replace the preset goal with the concrete checks the catalog can run for
/// the identifiers printed on the invoice. Returns None when no preset was requested.
pub fn expand(question: &str, intent: &mut Intent, catalog: &Catalog) -> Option<Expansion> {
    let index = intent.goals.iter().position(|g| g.capability == PRESET)?;
    let subject_index = intent.goals[index].subject;
    let covered = ["company.registry-profile", "company_registry.search", "company.uk-registry-search", "company.officers", "company.filing-history", "company.sanctions-screen", "vat.eu.validate", "vat.eu.check", DOMAIN_REGISTRATION, DOMAIN_MAIL_DNS, IBAN_CHECK, SIRENIC_IBAN_CHECK];
    intent.goals.retain(|g| g.capability != PRESET && !(g.subject == subject_index && covered.contains(&g.capability.as_str())));
    let subject = intent.subjects.get(subject_index)?.clone();
    let entity = subject.entity_ref?;
    let mut invoice = parse(question);
    invoice.entity_ref = entity;
    let bound = |kind: &str| subject.values.get(kind).and_then(Value::as_str).map(str::to_owned);
    invoice.kvk = invoice.kvk.or_else(|| bound("company_registry.kvknummer"));
    invoice.uk_company = invoice.uk_company.or_else(|| bound("company.registration.gb.companies_house"));
    let label = subject.label.trim();
    let label_is_identifier = label.bytes().filter(u8::is_ascii_digit).count() * 2 >= label.len();
    invoice.name = bound("company.name").or_else(|| (!label_is_identifier && !label.is_empty()).then(|| label.to_owned()));

    let mut facts: Vec<(String, Value)> = vec![];
    let mut fact = |kind: &str, value: Value| {
        if schema_ok(catalog, kind, &value) && !subject.values.contains_key(kind) {
            facts.push((kind.to_owned(), value));
        }
    };
    if let Some(name) = &invoice.name { fact("company.name", json!(name)) }
    if let Some(kvk) = &invoice.kvk { fact("company_registry.kvknummer", json!(kvk)) }
    if let Some(number) = &invoice.uk_company { fact("company.registration.gb.companies_house", json!(number)) }
    let uk = invoice.uk_company.is_some() || invoice.vat.as_deref().is_some_and(|v| v.starts_with("GB"));
    let mut goals: Vec<(&str, Option<&str>)> = vec![];
    if invoice.uk_company.is_some() {
        goals.extend([("company.registry-profile", Some(CH)), ("company.officers", Some(CH)), ("company.filing-history", Some(CH))]);
    } else if !uk && (invoice.kvk.is_some() || invoice.name.is_some()) {
        goals.push(("company.registry-profile", Some(KVK)));
    }
    if let Some(vat) = &invoice.vat {
        fact("vat.eu.country_code", json!(&vat[..2]));
        fact("vat.eu.number", json!(&vat[2..]));
        goals.push(("vat.eu.validate", Some("vies")));
    }
    if invoice.name.is_some() { goals.push(("company.sanctions-screen", Some("opensanctions"))) }
    if let Some(domain) = &invoice.domain {
        fact(DOMAIN_FACT, json!(domain));
        if RDAP_TLDS.iter().any(|t| domain.ends_with(&format!(".{t}"))) { goals.push((DOMAIN_REGISTRATION, None)) }
        goals.push((DOMAIN_MAIL_DNS, None));
    }
    if let Some(iban) = &invoice.iban {
        if has_service(catalog, IBAN_CHECK, Some("iban-check")) {
            fact(IBAN_FACT, json!(iban));
            goals.push((IBAN_CHECK, Some("iban-check")));
        } else {
            fact(SIRENIC_IBAN_INPUT, json!(iban));
            goals.push((SIRENIC_IBAN_CHECK, Some("sirenic")));
        }
    }
    let mut added = 0;
    for (capability, source) in goals {
        if !has_service(catalog, capability, source) { continue }
        let mut constraints = BTreeMap::new();
        if let Some(source) = source { constraints.insert("source".to_owned(), json!(source)); }
        intent.goals.push(Goal { capability: capability.into(), subject: subject_index, constraints });
        added += 1;
    }
    if added == 0 {
        // Nothing executable: keep the preset so the resolver reports it as unsupported.
        intent.goals.push(Goal { capability: PRESET.into(), subject: subject_index, constraints: BTreeMap::new() });
        return None;
    }
    if intent.answer_operations.is_empty() {
        intent.answer_operations.push("Supplier payment safety verdict".into());
    }
    Some(Expansion { invoice, facts })
}

// ------------------------------------------------------------- evaluation

pub fn normalize_name(name: &str) -> String {
    const FORMS: &[&str] = &["bv", "nv", "vof", "cv", "ltd", "limited", "plc", "llp", "llc", "inc", "gmbh", "ag", "sa", "sas", "sarl", "srl", "spa", "bvba", "the"];
    let folded: String = name.to_lowercase().chars().filter_map(|c| match c {
        'á' | 'à' | 'ä' | 'â' => Some('a'), 'é' | 'è' | 'ë' | 'ê' => Some('e'), 'í' | 'ï' | 'î' => Some('i'), 'ó' | 'ö' | 'ô' => Some('o'), 'ú' | 'ü' | 'û' => Some('u'),
        '.' => None, c if c.is_alphanumeric() => Some(c), _ => Some(' '),
    }).collect();
    folded.split_whitespace().filter(|w| !FORMS.contains(w)).collect::<Vec<_>>().join(" ")
}
pub fn names_match(a: &str, b: &str) -> bool {
    let (a, b) = (normalize_name(a), normalize_name(b));
    if a.is_empty() || b.is_empty() { return false }
    if a == b || a.replace(' ', "") == b.replace(' ', "") { return true }
    let (short, long) = if a.len() <= b.len() { (&a, &b) } else { (&b, &a) };
    let words: Vec<&str> = long.split(' ').collect();
    short.len() >= 4 && words.windows(short.split(' ').count()).any(|w| w.join(" ") == *short)
}
fn mask_iban(iban: &str) -> String {
    if iban.len() < 8 { return iban.into() }
    format!("{} •••• {}", &iban[..4], &iban[iban.len() - 4..])
}
fn kvk_date(value: &Value) -> Option<NaiveDate> {
    let s = value.as_str()?;
    NaiveDate::parse_from_str(s, "%Y%m%d").ok().or_else(|| NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok())
}
fn display_date(date: NaiveDate, dutch: bool) -> String {
    if dutch { date.format("%d-%m-%Y").to_string() } else { date.format("%Y-%m-%d").to_string() }
}

struct Check {
    id: &'static str,
    status: &'static str,
    detail: String,
    source: Option<&'static str>,
    evidence: Vec<Value>,
    /// Separate reader-facing findings (with their own severity) inside one check.
    points: Vec<(&'static str, String)>,
}
fn ev(result: &Value, pointer: &str) -> Value {
    json!({"result_ref":result["result_ref"],"pointer":pointer,"value":result["document"].pointer(pointer).cloned().unwrap_or(Value::Null)})
}
fn find<'a>(sources: &'a [Value], provider: Option<&str>, path: &str, exact: bool) -> Option<&'a Value> {
    sources.iter().rev().find(|s| {
        provider.is_none_or(|p| s["document"]["source"]["provider"] == p)
            && s["document"]["source"]["path"].as_str().is_some_and(|p| if exact { p == path } else { p.starts_with(path) })
    })
}
fn label(id: &str, dutch: bool) -> &'static str {
    match (id, dutch) {
        ("company_exists", true) => "Bedrijf bestaat",
        ("company_exists", false) => "Company exists",
        ("registry_name_match", true) => "Naam komt overeen met register",
        ("registry_name_match", false) => "Name matches the register",
        ("name_vat_match", true) => "Btw-nummer hoort bij dit bedrijf",
        ("name_vat_match", false) => "VAT number belongs to this company",
        ("address_match", true) => "Factuuradres klopt met registratie",
        ("address_match", false) => "Invoice address matches registration",
        ("iban_valid", true) => "IBAN is geldig",
        ("iban_valid", false) => "IBAN is valid",
        ("account_holder", true) => "Rekeninghouder klopt",
        ("account_holder", false) => "Account holder matches",
        ("recent_changes", true) => "Niet recent opgericht of gewijzigd",
        ("recent_changes", false) => "Not recently registered or changed",
        ("domain_age", true) => "Domein is betrouwbaar",
        ("domain_age", false) => "Domain looks established",
        ("sanctions_pep", true) => "Geen sanctie- of PEP-hit",
        ("sanctions_pep", false) => "No sanctions or PEP hit",
        ("invoice_history", true) => "Gelijk aan eerdere facturen",
        _ => "Consistent with earlier invoices",
    }
}

/// `sources`: saved documents as `{result_ref, document}`. `previous`: the most
/// recent earlier `record` for this supplier, with `observed_at`.
pub fn evaluate(invoice: &Invoice, sources: &[Value], failures: &BTreeMap<String, Value>, previous: Option<&Value>, now: DateTime<Utc>) -> Value {
    let nl = invoice.dutch();
    let t = |nl_text: String, en_text: String| if nl { nl_text } else { en_text };
    let today = now.date_naive();
    let mut checks: Vec<Check> = vec![];
    let mut limitations: Vec<String> = vec![];
    let supplier = invoice.name.clone().unwrap_or_else(|| t("deze leverancier".into(), "this supplier".into()));
    // A refunded 404 is a definitive "no record", unlike an unavailable source.
    let not_found = |provider: &str| failures.iter().any(|(step, error)| step.starts_with(&format!("{provider} ")) && error["code"] == "source_not_found");

    // --- registry
    let kvk_profile = find(sources, Some(KVK), "/api/v1/basisprofielen", false);
    let kvk_search = find(sources, Some(KVK), "/api/v2/zoeken", false);
    let ch_profile = find(sources, Some(CH), "/company", true);
    let mut registry_names: Vec<String> = vec![];
    let mut registry_country = None;
    if let Some(r) = kvk_profile {
        let d = &r["document"]["data"];
        let number = d["kvkNummer"].as_str().unwrap_or_default();
        registry_country = Some("NL");
        for pointer in ["/statutaireNaam", "/naam"] {
            if let Some(n) = d.pointer(pointer).and_then(Value::as_str) { registry_names.push(n.into()) }
        }
        registry_names.extend(d["handelsnamen"].as_array().into_iter().flatten().filter_map(|h| h["naam"].as_str()).map(str::to_owned));
        checks.push(match d.pointer("/materieleRegistratie/datumEinde").and_then(kvk_date) {
            Some(end) => Check { id: "company_exists", status: "fail", detail: t(format!("KVK-inschrijving {number} is beëindigd op {}.", display_date(end, nl)), format!("KVK registration {number} ended on {}.", display_date(end, nl))), source: Some("KVK"), evidence: vec![ev(r, "/data/materieleRegistratie/datumEinde")], points: vec![] },
            None => Check { id: "company_exists", status: "pass", detail: t(format!("Ingeschreven bij KVK onder {number}."), format!("Registered with KVK under {number}.")), source: Some("KVK"), evidence: vec![ev(r, "/data/kvkNummer")], points: vec![] },
        });
        if invoice.kvk.as_deref().is_some_and(|kvk| !number.is_empty() && kvk != number) {
            checks.push(Check { id: "company_exists", status: "fail", detail: t("KVK-nummer op de factuur hoort niet bij het gevonden bedrijf.".into(), "The KVK number on the invoice does not belong to the company found.".into()), source: Some("KVK"), evidence: vec![ev(r, "/data/kvkNummer")], points: vec![] });
        }
    } else if let Some(r) = ch_profile {
        let d = &r["document"]["data"];
        registry_country = Some("GB");
        if let Some(n) = d["company_name"].as_str() { registry_names.push(n.into()) }
        let status = d["company_status"].as_str().unwrap_or("unknown");
        let number = d["company_number"].as_str().unwrap_or_default();
        checks.push(Check { id: "company_exists", status: if status == "active" { "pass" } else { "fail" },
            detail: if status == "active" { t(format!("Actief bij Companies House onder {number}."), format!("Active at Companies House under {number}.")) }
                else { t(format!("Companies House-status is '{status}'."), format!("Companies House status is '{status}'.")) },
            source: Some("Companies House"), evidence: vec![ev(r, "/data/company_status")], points: vec![] });
    } else if let Some(r) = kvk_search.filter(|r| r["document"]["data"]["resultaten"].as_array().is_some_and(Vec::is_empty) || r["document"]["coverage"] == "empty") {
        checks.push(Check { id: "company_exists", status: "fail", detail: t(format!("Geen KVK-inschrijving gevonden voor {supplier}."), format!("No KVK registration found for {supplier}.")), source: Some("KVK"), evidence: vec![ev(r, "/data/resultaten")], points: vec![] });
    } else if not_found(KVK) || not_found(CH) {
        let register = if not_found(KVK) { "KVK" } else { "Companies House" };
        checks.push(Check { id: "company_exists", status: "fail", detail: t(format!("Geen inschrijving gevonden bij {register} voor {supplier}."), format!("No {register} registration found for {supplier}.")), source: Some(register), evidence: vec![], points: vec![] });
    } else if invoice.kvk.is_some() || invoice.uk_company.is_some() || invoice.vat.is_none() {
        checks.push(Check { id: "company_exists", status: "unknown", detail: t("De inschrijving in het handelsregister kon niet worden bevestigd.".into(), "The business register entry could not be confirmed.".into()), source: None, evidence: vec![], points: vec![] });
    }
    if let (Some(name), Some(first), Some(r)) = (&invoice.name, registry_names.first(), kvk_profile.or(ch_profile)) {
        let matched = registry_names.iter().any(|n| names_match(name, n));
        let (src, pointer) = if kvk_profile.is_some() { ("KVK", "/data/statutaireNaam") } else { ("Companies House", "/data/company_name") };
        checks.push(Check { id: "registry_name_match", status: if matched { "pass" } else { "warn" },
            detail: if matched { t(format!("Naam op de factuur komt overeen met '{first}'."), format!("The invoice name matches '{first}'.")) }
                else { t(format!("Factuurnaam '{name}' wijkt af van de geregistreerde naam '{first}'."), format!("Invoice name '{name}' differs from the registered name '{first}'.")) },
            source: Some(src), evidence: vec![ev(r, pointer)], points: vec![] });
    }

    // --- VAT
    if let Some(vat) = &invoice.vat {
        match find(sources, Some("vies"), "/taxation_customs/vies", false) {
            Some(r) => {
                let d = &r["document"]["data"]["data"];
                let rsin = kvk_profile.and_then(|p| p["document"]["data"].pointer("/_embedded/eigenaar/rsin").and_then(Value::as_str));
                let vies_name = d["name"].as_str().map(str::trim).filter(|n| !n.is_empty() && *n != "---");
                checks.push(if d["valid"] == false {
                    Check { id: "name_vat_match", status: "fail", detail: t(format!("Btw-nummer {vat} is volgens VIES niet geldig."), format!("VAT number {vat} is not valid according to VIES.")), source: Some("VIES"), evidence: vec![ev(r, "/data/data/valid")], points: vec![] }
                } else if let (Some(rsin), Some(profile)) = (rsin.filter(|rsin| vat.starts_with("NL") && vat.get(2..11) == Some(*rsin)), kvk_profile) {
                    let _ = rsin;
                    Check { id: "name_vat_match", status: "pass", detail: t("Btw-nummer is geldig en hoort bij het RSIN van deze KVK-inschrijving.".into(), "VAT number is valid and belongs to the RSIN of this KVK registration.".into()), source: Some("VIES + KVK"), evidence: vec![ev(r, "/data/data/valid"), ev(profile, "/data/_embedded/eigenaar/rsin")], points: vec![] }
                } else if let (Some(vn), Some(name)) = (vies_name, invoice.name.as_deref()) {
                    let matched = names_match(name, vn) || registry_names.iter().any(|n| names_match(n, vn));
                    Check { id: "name_vat_match", status: if matched { "pass" } else { "warn" },
                        detail: if matched { t(format!("Btw-nummer is geldig en staat op naam van '{vn}'."), format!("VAT number is valid and registered to '{vn}'.")) }
                            else { t(format!("Btw-nummer is geldig maar staat op naam van '{vn}', niet {name}."), format!("VAT number is valid but registered to '{vn}', not {name}.")) },
                        source: Some("VIES"), evidence: vec![ev(r, "/data/data/name")], points: vec![] }
                } else {
                    limitations.push(t("VIES geeft voor dit btw-nummer geen naam terug; alleen de geldigheid is gecontroleerd.".into(), "VIES returned no name for this VAT number; only validity was checked.".into()));
                    Check { id: "name_vat_match", status: "unknown", detail: t("Btw-nummer is geldig, maar de naam kon niet worden vergeleken.".into(), "VAT number is valid, but the name could not be compared.".into()), source: Some("VIES"), evidence: vec![ev(r, "/data/data/valid")], points: vec![] }
                });
            }
            None => checks.push(Check { id: "name_vat_match", status: "unknown", detail: t("VIES was niet beschikbaar; btw-nummer niet gecontroleerd.".into(), "VIES was unavailable; VAT number not checked.".into()), source: Some("VIES"), evidence: vec![], points: vec![] }),
        }
    }

    // --- address
    if let Some(postcode) = &invoice.postcode {
        let norm = |s: &str| s.to_ascii_uppercase().chars().filter(char::is_ascii_alphanumeric).collect::<String>();
        let registered = if let Some(r) = kvk_profile {
            let rows = r["document"]["data"].pointer("/_embedded/hoofdvestiging/adressen").and_then(Value::as_array).cloned().unwrap_or_default();
            let visible: Vec<(String, Option<String>)> = rows.iter().filter(|a| a["indAfgeschermd"] != "Ja").filter_map(|a| Some((norm(a["postcode"].as_str()?), a["huisnummer"].as_u64().map(|n| n.to_string())))).collect();
            Some((visible, r, "/data/_embedded/hoofdvestiging/adressen", "KVK"))
        } else {
            ch_profile.map(|r| (r["document"]["data"]["registered_office_address"]["postal_code"].as_str().map(|p| vec![(norm(p), None)]).unwrap_or_default(), r, "/data/registered_office_address/postal_code", "Companies House"))
        };
        match registered {
            Some((rows, r, pointer, src)) if !rows.is_empty() => {
                let matched = rows.iter().any(|(pc, nr)| *pc == norm(postcode) && (invoice.house_number.is_none() || nr.is_none() || *nr == invoice.house_number));
                let official = rows.iter().map(|(pc, nr)| format!("{pc}{}", nr.as_ref().map(|n| format!(" {n}")).unwrap_or_default())).collect::<Vec<_>>().join(", ");
                checks.push(Check { id: "address_match", status: if matched { "pass" } else { "warn" },
                    detail: if matched { t("Factuuradres komt overeen met de officiële registratie.".into(), "Invoice address matches the official registration.".into()) }
                        else { t(format!("Factuuradres wijkt af van officiële registratie ({official})."), format!("Invoice address differs from the official registration ({official}).")) },
                    source: Some(src), evidence: vec![json!({"result_ref":r["result_ref"],"pointer":pointer,"value":official})], points: vec![] });
            }
            _ => checks.push(Check { id: "address_match", status: "unknown", detail: t("Geen openbaar geregistreerd adres om mee te vergelijken.".into(), "No publicly registered address to compare with.".into()), source: None, evidence: vec![], points: vec![] }),
        }
    }

    // --- IBAN
    let supplier_country = registry_country.map(str::to_owned).or_else(|| invoice.country());
    if let Some(iban) = &invoice.iban {
        let own = find(sources, Some("iban-check"), "/iban/", false);
        let bank = own.or_else(|| find(sources, Some("sirenic"), "/v1/iban/verifier", false));
        let (bank_pointer, source_name) = if own.is_some() { ("/data/data/bank/name", "IBAN Check") } else { ("/data/banque/nom", "Sirenic") };
        let bank_name = bank.and_then(|r| r["document"].pointer(bank_pointer)).and_then(Value::as_str);
        let evidence = bank.map(|r| vec![ev(r, bank_pointer)]).unwrap_or_default();
        let foreign = supplier_country.as_deref().filter(|c| *c != &iban[..2]);
        checks.push(if !iban_valid(iban) {
            Check { id: "iban_valid", status: "fail", detail: t(format!("IBAN {} is ongeldig (controlegetal klopt niet).", mask_iban(iban)), format!("IBAN {} is invalid (check digits fail).", mask_iban(iban))), source: None, evidence: vec![], points: vec![] }
        } else if let Some(country) = foreign {
            Check { id: "iban_valid", status: "warn", detail: t(format!("IBAN is van een bank in {} terwijl de leverancier in {country} is gevestigd.", &iban[..2]), format!("IBAN belongs to a bank in {} while the supplier is based in {country}.", &iban[..2])), source: bank.map(|_| source_name), evidence, points: vec![] }
        } else {
            Check { id: "iban_valid", status: "pass", detail: match bank_name { Some(b) => t(format!("IBAN is geldig ({b})."), format!("IBAN is valid ({b}).")), None => t("IBAN-structuur en controlegetal zijn geldig.".into(), "IBAN structure and check digits are valid.".into()) }, source: bank.map(|_| source_name), evidence, points: vec![] }
        });
        let payee = sources.iter().rev().find_map(|r| {
            let d = &r["document"]["data"];
            d["name_match"].as_str().or(d["nameMatchResult"].as_str()).or(d.pointer("/data/name_match").and_then(Value::as_str)).map(|m| (r, m.to_ascii_lowercase()))
        });
        checks.push(match payee {
            Some((r, m)) if m == "match" || m == "yes" => Check { id: "account_holder", status: "pass", detail: t("Naam van de rekeninghouder komt overeen.".into(), "Account holder name matches.".into()), source: Some("IBAN-Name Check"), evidence: vec![ev(r, "/data")], points: vec![] },
            Some((r, m)) if m == "no_match" || m == "no" => Check { id: "account_holder", status: "fail", detail: t("Rekeninghouder komt niet overeen met de leverancier.".into(), "Account holder does not match the supplier.".into()), source: Some("IBAN-Name Check"), evidence: vec![ev(r, "/data")], points: vec![] },
            Some((r, _)) => Check { id: "account_holder", status: "warn", detail: t("Naam van de rekeninghouder komt slechts gedeeltelijk overeen of kon niet worden gematcht.".into(), "Account holder name matches only partially or could not be matched.".into()), source: Some("IBAN-Name Check"), evidence: vec![ev(r, "/data")], points: vec![] },
            None => {
                limitations.push(t("De naam van de rekeninghouder is niet geverifieerd: daarvoor is een IBAN-Naam-Check (Verification of Payee) nodig.".into(), "The account holder name is not verified: that requires an IBAN-Name Check (Verification of Payee).".into()));
                Check { id: "account_holder", status: "unknown", detail: t("Niet gecontroleerd (geen IBAN-Naam-Check beschikbaar).".into(), "Not checked (no IBAN-Name Check available).".into()), source: None, evidence: vec![], points: vec![] }
            }
        });
    }

    // --- recent registration / changes
    if let Some(r) = kvk_profile {
        let d = &r["document"]["data"];
        let (start, pointer) = match d.pointer("/materieleRegistratie/datumAanvang").and_then(kvk_date) {
            Some(date) => (Some(date), "/data/materieleRegistratie/datumAanvang"),
            None => (d.pointer("/_embedded/hoofdvestiging/materieleRegistratie/datumAanvang").and_then(kvk_date), "/data/_embedded/hoofdvestiging/materieleRegistratie/datumAanvang"),
        };
        checks.push(match start {
            Some(date) if (today - date).num_days() < 365 => Check { id: "recent_changes", status: "warn", detail: t(format!("Bedrijf is pas ingeschreven op {} ({} dagen geleden).", display_date(date, nl), (today - date).num_days()), format!("Company was only registered on {} ({} days ago).", display_date(date, nl), (today - date).num_days())), source: Some("KVK"), evidence: vec![ev(r, pointer)], points: vec![] },
            Some(date) => Check { id: "recent_changes", status: "pass", detail: t(format!("Ingeschreven sinds {}.", display_date(date, nl)), format!("Registered since {}.", display_date(date, nl))), source: Some("KVK"), evidence: vec![ev(r, pointer)], points: vec![] },
            None => Check { id: "recent_changes", status: "unknown", detail: t("Inschrijvingsdatum niet beschikbaar.".into(), "Registration date not available.".into()), source: Some("KVK"), evidence: vec![], points: vec![] },
        });
        limitations.push(t("Recente KVK-wijzigingen (adres, naam, bestuurders) zijn niet opgevraagd: die historie vereist een KVK Mutatieservice-abonnement. Bestuurders zijn niet op sancties/PEP gescreend omdat de KVK-API ze niet levert.".into(), "Recent KVK changes (address, name, directors) were not retrieved: that history requires a KVK Mutatieservice subscription. Directors were not screened for sanctions/PEP because the KVK API does not provide them.".into()));
    } else if let Some(r) = ch_profile {
        let d = &r["document"]["data"];
        let mut findings: Vec<(String, Value)> = vec![];
        if let Some(created) = d["date_of_creation"].as_str().and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()) {
            if (today - created).num_days() < 365 { findings.push((t(format!("Bedrijf opgericht op {}.", display_date(created, nl)), format!("Company incorporated on {}.", display_date(created, nl))), ev(r, "/data/date_of_creation"))) }
        }
        if let Some(h) = find(sources, Some(CH), "/company/{company_number}/filing-history", true) {
            for (i, item) in h["document"]["data"]["items"].as_array().into_iter().flatten().enumerate().take(100) {
                let Some(date) = item["date"].as_str().and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()) else { continue };
                let kind = item["type"].as_str().unwrap_or("");
                if (today - date).num_days() <= 90 && ["AD01", "NM01", "NM04", "CONNOT", "AP01", "TM01", "CH01", "PSC01", "PSC07"].contains(&kind) {
                    findings.push((t(format!("Recente wijziging bij Companies House ({kind}) op {}.", display_date(date, nl)), format!("Recent Companies House change ({kind}) on {}.", display_date(date, nl))), ev(h, &format!("/data/items/{i}/type"))));
                }
            }
        }
        if let Some(o) = find(sources, Some(CH), "/company/{company_number}/officers", true) {
            for (i, item) in o["document"]["data"]["items"].as_array().into_iter().flatten().enumerate().take(100) {
                let Some(date) = item["appointed_on"].as_str().and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()) else { continue };
                if (today - date).num_days() <= 90 && item["resigned_on"].is_null() {
                    findings.push((t(format!("Nieuwe functionaris benoemd op {}.", display_date(date, nl)), format!("New officer appointed on {}.", display_date(date, nl))), ev(o, &format!("/data/items/{i}/appointed_on"))));
                }
            }
        }
        checks.push(if findings.is_empty() {
            Check { id: "recent_changes", status: "pass", detail: t("Geen recente oprichting of wijzigingen gevonden.".into(), "No recent incorporation or changes found.".into()), source: Some("Companies House"), evidence: vec![ev(r, "/data/date_of_creation")], points: vec![] }
        } else {
            Check { id: "recent_changes", status: "warn", detail: findings.iter().take(3).map(|f| f.0.clone()).collect::<Vec<_>>().join(" "), source: Some("Companies House"), evidence: findings.into_iter().take(4).map(|f| f.1).collect(), points: vec![] }
        });
        limitations.push(t("Functionarissen zijn niet afzonderlijk op sancties/PEP gescreend; alleen de bedrijfsnaam.".into(), "Officers were not individually screened for sanctions/PEP; only the company name was.".into()));
    }

    // --- domain
    if let Some(domain) = &invoice.domain {
        let rdap = find(sources, None, "/domain/{domain}", true);
        let dns = find(sources, None, "/dns/{domain}", true);
        let registered = rdap.and_then(|r| r["document"]["data"]["data"]["registered_at"].as_str()).and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|d| d.with_timezone(&Utc));
        let mut check = match (rdap, registered) {
            (Some(r), Some(at)) => {
                let days = (now - at).num_days().max(0);
                let date = display_date(at.date_naive(), nl);
                let evidence = vec![ev(r, "/data/data/registered_at")];
                if days < 30 { Check { id: "domain_age", status: "fail", detail: t(format!("Domein {domain} geregistreerd {days} dagen geleden."), format!("Domain {domain} registered {days} days ago.")), source: Some("RDAP"), evidence, points: vec![] } }
                else if days < 180 { Check { id: "domain_age", status: "warn", detail: t(format!("Domein {domain} is recent geregistreerd ({date})."), format!("Domain {domain} was registered recently ({date}).")), source: Some("RDAP"), evidence, points: vec![] } }
                else { Check { id: "domain_age", status: "pass", detail: t(format!("Domein {domain} bestaat sinds {date}."), format!("Domain {domain} exists since {date}.")), source: Some("RDAP"), evidence, points: vec![] } }
            }
            _ if not_found("rdap") => Check { id: "domain_age", status: "fail", detail: t(format!("Domein {domain} is niet geregistreerd."), format!("Domain {domain} is not registered.")), source: Some("RDAP"), evidence: vec![], points: vec![] },
            _ => {
                if !RDAP_TLDS.iter().any(|tld| domain.ends_with(&format!(".{tld}"))) {
                    limitations.push(t(format!("De registry van {domain} publiceert geen registratiedatum."), format!("The registry of {domain} publishes no registration date.")));
                }
                Check { id: "domain_age", status: "unknown", detail: t(format!("Registratiedatum van {domain} niet beschikbaar."), format!("Registration date of {domain} is not available.")), source: None, evidence: vec![], points: vec![] }
            }
        };
        if let Some(r) = dns.filter(|r| r["document"]["data"]["data"]["mx"].as_array().is_some_and(Vec::is_empty)) {
            if check.status != "fail" {
                check.status = "warn";
                check.detail = format!("{} {}", check.detail, t(format!("{domain} ontvangt geen e-mail (geen MX-record)."), format!("{domain} receives no e-mail (no MX record).")));
                check.evidence.push(ev(r, "/data/data/mx"));
            }
        }
        checks.push(check);
    }
    if let Some(free) = &invoice.free_mail_domain {
        checks.push(Check { id: "domain_age", status: "warn", detail: t(format!("Factuur gebruikt een gratis e-mailadres ({free}) in plaats van een bedrijfsdomein."), format!("Invoice uses a free e-mail address ({free}) instead of a company domain.")), source: None, evidence: vec![], points: vec![] });
    }

    // --- sanctions / PEP
    if invoice.name.is_some() {
        match find(sources, Some("opensanctions"), "/match", false) {
            Some(r) => {
                let results = r["document"]["data"].pointer("/responses/q1/results").and_then(Value::as_array).cloned().unwrap_or_default();
                let hit = results.iter().enumerate().find(|(_, x)| x["match"] == true);
                checks.push(match hit {
                    Some((i, x)) => {
                        let topics: Vec<&str> = x.pointer("/properties/topics").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
                        let sanctioned = topics.iter().any(|t| t.starts_with("sanction")) || x["datasets"].as_array().is_some_and(|d| d.iter().any(|v| v.as_str().is_some_and(|s| s.contains("sanction") || s.contains("ofac"))));
                        let caption = x["caption"].as_str().unwrap_or("?");
                        Check { id: "sanctions_pep", status: if sanctioned { "fail" } else { "warn" },
                            detail: if sanctioned { t(format!("Sanctie-hit: '{caption}' staat op een sanctielijst."), format!("Sanctions hit: '{caption}' is on a sanctions list.")) }
                                else if topics.contains(&"role.pep") { t(format!("PEP-hit: '{caption}'."), format!("PEP hit: '{caption}'.")) }
                                else { t(format!("Mogelijke watchlist-hit: '{caption}'."), format!("Possible watchlist hit: '{caption}'.")) },
                            source: Some("OpenSanctions"), evidence: vec![ev(r, &format!("/data/responses/q1/results/{i}/caption"))], points: vec![] }
                    }
                    None => Check { id: "sanctions_pep", status: "pass", detail: t("Geen sanctie- of PEP-match op de bedrijfsnaam.".into(), "No sanctions or PEP match on the company name.".into()), source: Some("OpenSanctions"), evidence: vec![ev(r, "/data/responses/q1/total")], points: vec![] },
                });
            }
            None => checks.push(Check { id: "sanctions_pep", status: "unknown", detail: t("Sanctiescreening kon niet worden uitgevoerd.".into(), "Sanctions screening could not be performed.".into()), source: None, evidence: vec![], points: vec![] }),
        }
    }

    // --- earlier invoices
    match previous {
        Some(prev) => {
            let mut diffs: Vec<(&'static str, String)> = vec![];
            let seen = prev["observed_at"].as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|d| display_date(d.date_naive(), nl)).unwrap_or_default();
            if let (Some(old), Some(new)) = (prev["iban"].as_str(), invoice.iban.as_deref()) {
                if old != new { diffs.push(("fail", t(format!("IBAN is gewijzigd ({} → {}).", mask_iban(old), mask_iban(new)), format!("IBAN has changed ({} → {}).", mask_iban(old), mask_iban(new))))) }
            }
            if let (Some(old), Some(new)) = (prev["domain"].as_str(), invoice.domain.as_deref()) {
                if old != new { diffs.push(("warn", t(format!("Domein is gewijzigd ({old} → {new})."), format!("Domain has changed ({old} → {new})."))) ) }
            }
            if let (Some(old), Some(new)) = (prev["postcode"].as_str(), invoice.postcode.as_deref()) {
                let number_changed = prev["house_number"].as_str().zip(invoice.house_number.as_deref()).is_some_and(|(a, b)| a != b);
                if old != new || number_changed { diffs.push(("warn", t("Factuuradres wijkt af van eerdere facturen.".into(), "Invoice address differs from earlier invoices.".into()))) }
            }
            if let (Some(old), Some(new)) = (prev["invoice_number"].as_str(), invoice.invoice_number.as_deref()) {
                if old == new { diffs.push(("warn", t(format!("Factuur {new} is al eerder gecontroleerd ({seen}); let op dubbele betaling."), format!("Invoice {new} was already checked before ({seen}); beware of a double payment."))) ) }
            }
            checks.push(if diffs.is_empty() {
                Check { id: "invoice_history", status: "pass", detail: t(format!("Gegevens gelijk aan de vorige gecontroleerde factuur ({seen})."), format!("Details match the previously checked invoice ({seen}).")), source: Some("Apiosk"), evidence: vec![], points: vec![] }
            } else {
                let status = if diffs.iter().any(|d| d.0 == "fail") { "fail" } else { "warn" };
                Check { id: "invoice_history", status, detail: diffs.iter().map(|d| d.1.clone()).collect::<Vec<_>>().join(" "), source: Some("Apiosk"), evidence: vec![], points: diffs }
            });
        }
        None => checks.push(Check { id: "invoice_history", status: "unknown", detail: t("Eerste gecontroleerde factuur van deze leverancier; volgende facturen worden hiermee vergeleken.".into(), "First checked invoice from this supplier; later invoices are compared with it.".into()), source: Some("Apiosk"), evidence: vec![], points: vec![] }),
    }

    if !failures.is_empty() {
        limitations.push(t("Niet alle bronnen gaven een resultaat; ontbrekende controles staan als 'onbekend'.".into(), "Not every source returned a result; missing checks are marked 'unknown'.".into()));
    }

    // --- verdict: most important signals first, in the order the reader acts on them.
    let order = ["invoice_history", "iban_valid", "account_holder", "company_exists", "sanctions_pep", "domain_age", "address_match", "name_vat_match", "registry_name_match", "recent_changes"];
    checks.sort_by_key(|c| order.iter().position(|o| *o == c.id).unwrap_or(order.len()));
    let decision = if checks.iter().any(|c| c.status == "fail") { "hold" }
        else if checks.iter().any(|c| c.status == "warn" || (c.id == "company_exists" && c.status == "unknown")) { "review" }
        else { "ok" };
    let (label_text, icon) = match (decision, nl) {
        ("hold", true) => ("BETALING TEGENHOUDEN", "⚠️"),
        ("hold", false) => ("HOLD PAYMENT", "⚠️"),
        ("review", true) => ("EERST CONTROLEREN", "🔎"),
        ("review", false) => ("REVIEW BEFORE PAYING", "🔎"),
        (_, true) => ("GEEN RODE VLAGGEN GEVONDEN", "✅"),
        (_, false) => ("NO RED FLAGS FOUND", "✅"),
    };
    let flagged: Vec<&Check> = checks.iter().filter(|c| c.status == "fail").chain(checks.iter().filter(|c| c.status == "warn")).collect();
    // (severity, is secondary history difference, text): failures first, then
    // source warnings, then softer differences from earlier invoices.
    let findings: Vec<(&str, bool, &str)> = checks.iter().flat_map(|c| {
        if c.points.is_empty() { vec![(c.status, false, c.detail.as_str())] } else { c.points.iter().map(|(s, d)| (*s, *s != "fail", d.as_str())).collect() }
    }).collect();
    let rank = |f: &&(&str, bool, &str)| (if f.0 == "fail" { 0 } else { 1 }, f.1);
    let mut ranked: Vec<&(&str, bool, &str)> = findings.iter().filter(|f| f.0 == "fail" || f.0 == "warn").collect();
    ranked.sort_by_key(rank);
    let mut reasons: Vec<String> = ranked.iter().map(|f| f.2.to_owned()).take(6).collect();
    if decision == "review" && reasons.is_empty() {
        reasons.extend(checks.iter().find(|c| c.id == "company_exists").map(|c| c.detail.clone()));
    }
    let rendered: Vec<Value> = checks.iter().map(|c| json!({"id":c.id,"label":label(c.id, nl),"status":c.status,"detail":c.detail,"source":c.source,"evidence":c.evidence})).collect();
    let mut observations = vec![json!({"text":format!("{icon} {label_text}"),"evidence":[]})];
    observations.extend(flagged.iter().take(5).map(|c| json!({"text":c.detail,"evidence":c.evidence})));
    let partial = checks.iter().any(|c| c.status == "unknown" && ["company_exists", "sanctions_pep", "name_vat_match"].contains(&c.id));
    json!({
        "status": if partial { "partial" } else { "completed" },
        "kind": "supplier_payment_check",
        "verdict": {
            "decision": decision, "label": label_text, "headline": format!("{icon} {label_text}"), "language": invoice.language,
            "reasons": reasons,
            "supplier": {"name": invoice.name, "kvk_number": invoice.kvk.as_ref().or(invoice.uk_company.as_ref()), "vat_number": invoice.vat, "iban_masked": invoice.iban.as_deref().map(mask_iban), "domain": invoice.domain},
            "checks": rendered,
        },
        "observations": observations,
        "limitations": limitations,
    })
}

/// The row stored for comparison with the next invoice from this supplier.
pub fn record(invoice: &Invoice) -> Value {
    json!({"name":invoice.name,"kvk":invoice.kvk,"uk_company":invoice.uk_company,"vat":invoice.vat,"iban":invoice.iban,"domain":invoice.domain,
        "postcode":invoice.postcode,"house_number":invoice.house_number,"invoice_number":invoice.invoice_number})
}

#[cfg(test)]
mod tests {
    use super::*;

    const Q: &str = "Kan ik deze leverancier veilig betalen? Factuur INV-2026-0042 van Mollie B.V., KVK 30204462, btw-nummer NL815839091B01, IBAN NL91 ABNA 0417 1643 00, website https://www.mollie.com/nl, e-mail billing@mollie.com, adres Keizersgracht 126, 1015 CW Amsterdam.";

    #[test]
    fn reads_literal_identifiers_from_an_invoice_question() {
        let i = parse(Q);
        assert_eq!(i.kvk.as_deref(), Some("30204462"));
        assert_eq!(i.vat.as_deref(), Some("NL815839091B01"));
        assert_eq!(i.iban.as_deref(), Some("NL91ABNA0417164300"));
        assert_eq!(i.domain.as_deref(), Some("mollie.com"));
        assert_eq!(i.postcode.as_deref(), Some("1015CW"));
        assert_eq!(i.house_number.as_deref(), Some("126"));
        assert_eq!(i.invoice_number.as_deref(), Some("INV-2026-0042"));
        assert_eq!(i.language, "nl");
    }
    #[test]
    fn rejects_invalid_ibans_and_invented_shapes() {
        assert!(iban_valid("NL91ABNA0417164300"));
        assert!(!iban_valid("NL92ABNA0417164300"));
        let i = parse("Is this invoice safe to pay? Supplier Acme Ltd, company number 01234567, IBAN GB82 WEST 1234 5698 7654 32, invoice.pdf, total 1.210,00, VAT GB123456789, SW1A 1AA");
        assert_eq!(i.uk_company.as_deref(), Some("01234567"));
        assert_eq!(i.iban.as_deref(), Some("GB82WEST12345698765432"));
        assert_eq!(i.domain, None);
        assert_eq!(i.vat, None);
        assert_eq!(i.postcode.as_deref(), Some("SW1A1AA"));
        assert_eq!(i.language, "en");
        assert_eq!(parse("Invoice from x, kvk 1234").kvk, None);
        assert_eq!(parse("Kan ik betalen? IBAN NL92 ABNA 0417 1643 00").iban.as_deref(), Some("NL92ABNA0417164300"));
        assert_eq!(parse("Kan ik betalen? Factuur NL12 van Acme").iban, None);
    }
    #[test]
    fn fast_path_only_for_plain_invoice_checks() {
        let catalog = Catalog::build(serde_json::from_str(include_str!("../tests/fixtures/supplier-check/catalog.json")).unwrap());
        assert!(available(&catalog));
        let direct = direct_intent("Kan ik deze leverancier veilig betalen? Factuur INV-1 van Mollie B.V., KVK 30204462", &catalog).unwrap();
        assert_eq!(direct.subjects[0].label, "Mollie B.V.");
        assert_eq!(direct.goals[0].capability, PRESET);
        assert!(direct_intent("Kan ik Mollie B.V. veilig betalen en wat is hun jaarrekening? KVK 30204462", &catalog).is_none());
        assert!(direct_intent("Kan ik deze leverancier veilig betalen?", &catalog).is_none(), "no identifier: ask for the invoice");
        assert!(needs_invoice_upload("Kan ik deze leverancier veilig betalen?", &catalog));
        assert!(needs_invoice_upload("Can I safely pay this supplier?", &catalog));
        assert!(!needs_invoice_upload("Kan ik Mollie veilig betalen? KVK 30204462", &catalog));
        assert!(!needs_invoice_upload("Wat is de jaarrekening van Mollie?", &catalog));
        assert!(direct_intent("Wat is het KVK-profiel van 30204462?", &catalog).is_none());
    }
    #[test]
    fn written_names_and_check_wording_are_recognised() {
        assert_eq!(written_supplier_name("Kan ik deze leverancier veilig betalen? Factuur INV-1 van Mollie B.V., KVK 30204462").as_deref(), Some("Mollie B.V."));
        assert_eq!(written_supplier_name("Is this invoice from Acme Ltd. legit? IBAN GB82WEST12345698765432").as_deref(), Some("Acme Ltd"));
        assert!(mentions_invoice_check("Klopt deze factuur wel?"));
        assert!(mentions_invoice_check("Our vendor sent new bank details, is this invoice real?"));
        assert!(!mentions_invoice_check("Wat is de jaarrekening van Mollie?"));
    }
    #[test]
    fn multibyte_tokens_never_panic() {
        for q in ["Kan ik 100€ betalen aan Acme BV?", "Factuur €1.210 van Café Müller GmbH, 1015 ÇW, IBAN ÑL91 ABNA", "₿₿₿₿₿₿ 1234 €€ ééé"] {
            let _ = parse(q);
        }
    }
    #[test]
    fn house_numbers_follow_street_names_not_iban_groups() {
        let i = parse("klopt deze factuur? IBAN NL91 ABNA 0417 1643 00, website mollie.com, 1015 CW Amsterdam, Keizersgracht 126");
        assert_eq!((i.postcode.as_deref(), i.house_number.as_deref()), (Some("1015CW"), Some("126")));
        let i = parse("adres Hoofdstraat 12A, 1234 AB Utrecht");
        assert_eq!(i.house_number.as_deref(), Some("12"));
        let i = parse("IBAN NL91 ABNA 0417 1643 00 1015 CW");
        assert_eq!(i.house_number, None);
    }
    #[test]
    fn free_mail_is_not_a_company_domain() {
        let i = parse("Kan ik betalen? Factuur van Bouw BV, mail bouwbv@gmail.com");
        assert_eq!(i.domain, None);
        assert_eq!(i.free_mail_domain.as_deref(), Some("gmail.com"));
    }
    #[test]
    fn names_ignore_legal_forms_and_punctuation() {
        assert!(names_match("Mollie B.V.", "MOLLIE BV"));
        assert!(names_match("Acme Ltd", "ACME LIMITED"));
        assert!(!names_match("Acme B.V.", "Globex B.V."));
    }

    fn doc(provider: &str, path: &str, data: Value) -> Value {
        json!({"result_ref":Uuid::new_v4(),"document":{"source":{"provider":provider,"path":path},"data":data}})
    }
    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-15T12:00:00Z").unwrap().with_timezone(&Utc)
    }

    #[test]
    fn holds_payment_for_changed_iban_new_domain_and_different_address() {
        let mut invoice = parse(Q);
        invoice.name = Some("Mollie B.V.".into());
        let sources = vec![
            doc(KVK, "/api/v1/basisprofielen", json!({"kvkNummer":"30204462","statutaireNaam":"Mollie B.V.","materieleRegistratie":{"datumAanvang":"20050421"},"_embedded":{"eigenaar":{"rsin":"815839091"},"hoofdvestiging":{"adressen":[{"type":"bezoekadres","postcode":"1017AB","huisnummer":10,"indAfgeschermd":"Nee"}]}}})),
            doc("vies", "/taxation_customs/vies/rest-api/ms/{cc}/vat/{nr}", json!({"data":{"valid":true,"name":"MOLLIE B.V."}})),
            doc("opensanctions", "/match", json!({"responses":{"q1":{"results":[],"total":{"value":0}}}})),
            doc("rdap", "/domain/{domain}", json!({"data":{"registered_at":"2026-09-03T09:00:00Z"}})),
            doc("sirenic", "/v1/iban/verifier/{iban}", json!({"valide":true,"banque":{"nom":"ABN AMRO Bank"}})),
        ];
        let previous = json!({"iban":"NL02ABNA0123456789","domain":"mollie.com","postcode":"1015CW","house_number":"126","observed_at":"2026-08-01T10:00:00Z"});
        let v = evaluate(&invoice, &sources, &BTreeMap::new(), Some(&previous), now());
        assert_eq!(v["verdict"]["decision"], "hold", "{v:#}");
        assert_eq!(v["verdict"]["headline"], "⚠️ BETALING TEGENHOUDEN");
        let reasons: Vec<&str> = v["verdict"]["reasons"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        assert!(reasons[0].starts_with("IBAN is gewijzigd"), "{reasons:?}");
        assert!(reasons[1].contains("geregistreerd 12 dagen geleden"), "{reasons:?}");
        assert!(reasons[2].starts_with("Factuuradres wijkt af van officiële registratie"), "{reasons:?}");
        let checks = v["verdict"]["checks"].as_array().unwrap();
        assert!(checks.iter().any(|c| c["id"] == "name_vat_match" && c["status"] == "pass"));
        assert!(checks.iter().any(|c| c["id"] == "account_holder" && c["status"] == "unknown"));
        assert!(!v["verdict"]["supplier"]["iban_masked"].as_str().unwrap().contains("0417"));
    }
    #[test]
    fn reports_no_red_flags_only_when_every_evaluated_check_passes() {
        let mut invoice = parse("Can I safely pay this supplier? Invoice from Mollie B.V., KVK 30204462, VAT NL815839091B01, address Keizersgracht 126, 1015CW Amsterdam, website mollie.com");
        invoice.name = Some("Mollie B.V.".into());
        assert_eq!(invoice.language, "en");
        let sources = vec![
            doc(KVK, "/api/v1/basisprofielen", json!({"kvkNummer":"30204462","statutaireNaam":"Mollie B.V.","materieleRegistratie":{"datumAanvang":"20050421"},"_embedded":{"eigenaar":{"rsin":"815839091"},"hoofdvestiging":{"adressen":[{"type":"bezoekadres","postcode":"1015CW","huisnummer":126,"indAfgeschermd":"Nee"}]}}})),
            doc("vies", "/taxation_customs/vies/rest-api/ms/{cc}/vat/{nr}", json!({"data":{"valid":true,"name":"---"}})),
            doc("opensanctions", "/match", json!({"responses":{"q1":{"results":[{"match":false,"score":0.4,"caption":"Other"}]}}})),
            doc("rdap", "/domain/{domain}", json!({"data":{"registered_at":"2004-06-01T00:00:00Z"}})),
            doc("rdap", "/dns/{domain}", json!({"data":{"mx":["10 mx.mollie.com."]}})),
        ];
        let v = evaluate(&invoice, &sources, &BTreeMap::new(), None, now());
        assert_eq!(v["verdict"]["decision"], "ok", "{v:#}");
        assert!(v["limitations"].to_string().contains("Mutatieservice"));
    }
    #[test]
    fn refunded_not_found_is_a_definitive_absence() {
        let mut invoice = parse("Kan ik betalen? Factuur van Nepbedrijf BV, KVK 12345678, website nepbedrijf-betalingen.nl");
        invoice.name = Some("Nepbedrijf BV".into());
        let failures = BTreeMap::from([
            (format!("{KVK} /api/v1/basisprofielen"), json!({"code":"source_not_found"})),
            ("rdap /domain/{domain}".to_owned(), json!({"code":"source_not_found"})),
        ]);
        let v = evaluate(&invoice, &[], &failures, None, now());
        assert_eq!(v["verdict"]["decision"], "hold");
        let checks = v["verdict"]["checks"].to_string();
        assert!(checks.contains("Geen inschrijving gevonden bij KVK"), "{checks}");
        assert!(checks.contains("is niet geregistreerd"), "{checks}");
    }
    #[test]
    fn sanctions_hit_and_missing_registry_hold_payment() {
        let mut invoice = parse("Kan ik deze leverancier veilig betalen? Factuur van Gazprom");
        invoice.name = Some("Gazprom".into());
        let sources = vec![
            doc(KVK, "/api/v2/zoeken", json!({"resultaten":[],"totaal":0})),
            doc("opensanctions", "/match", json!({"responses":{"q1":{"results":[{"match":true,"score":1,"caption":"GAZPROM","properties":{"topics":["sanction"]}}]}}})),
        ];
        let v = evaluate(&invoice, &sources, &BTreeMap::new(), None, now());
        assert_eq!(v["verdict"]["decision"], "hold");
        let reasons = v["verdict"]["reasons"].to_string();
        assert!(reasons.contains("Geen KVK-inschrijving"), "{reasons}");
        assert!(reasons.contains("Sanctie-hit"), "{reasons}");
    }
}
