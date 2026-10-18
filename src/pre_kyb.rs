//! Pre-KYB check: "Who is this company, really?" (see `docs/pre-kyb-check.md`).
//!
//! Two code-owned pieces live here. `screening()` makes the sanctions step use
//! the resolved registry name instead of a search term. `evaluate()` turns the
//! saved source documents of a `pre-kyb-screening` run into the
//! `pre_kyb_check` analysis: an identity card, a checklist and a preliminary
//! Pass / Review / Reject decision. Code decides; a model never changes it.
use crate::{
    resolver::{Plan, Step},
    state::{Fact, Task},
    supplier_check::{iban_valid, names_match},
};
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const SLUG: &str = "pre-kyb-screening";
pub const KIND: &str = "pre_kyb_check";
const KVK: &str = "kvk-dutch-business-register";
const CH: &str = "companies-house";
const SIRENIC: &str = "sirenic";
const DOMAIN_MIN_DAYS: i64 = 180;

pub(crate) enum Screening {
    NotApplicable,
    Waiting,
    Unavailable,
    Ready(Fact),
}

pub(crate) fn screening(task: &Task, step: &Step) -> Screening {
    if step.service.contract.capability_slug != "company.sanctions-screen"
        || !(task
            .workflow
            .as_ref()
            .is_some_and(|workflow| workflow.slug == SLUG)
            || task
                .proposal
                .as_ref()
                .is_some_and(|plan| plan["workflow_slug"] == SLUG))
    {
        return Screening::NotApplicable;
    }
    let Some(plan) = task
        .proposal
        .as_ref()
        .and_then(|value| serde_json::from_value::<Plan>(value.clone()).ok())
    else {
        return Screening::Unavailable;
    };
    let profiles: Vec<_> = plan
        .steps
        .iter()
        .filter(|profile| {
            profile.entity_ref == step.entity_ref
                && profile.service.contract.capability_slug == "company.registry-profile"
        })
        .collect();
    if profiles.is_empty() {
        return Screening::Unavailable;
    }
    if profiles
        .iter()
        .any(|profile| !task.step_finished(&profile.key()))
    {
        return Screening::Waiting;
    }
    if profiles.iter().any(|profile| {
        !task.completed.contains(&profile.key())
            || task.failed_steps.contains_key(&profile.key())
            || task.skipped_steps.contains(&profile.key())
    }) {
        return Screening::Unavailable;
    }
    let names: Vec<_> = task
        .facts
        .iter()
        .filter(|fact| {
            fact.entity_ref == step.entity_ref
                && fact.semantic_type == "company.name"
                && fact.verification == "source_observed"
                && fact.expires_at > chrono::Utc::now()
                && fact
                    .value
                    .as_str()
                    .is_some_and(|name| !name.trim().is_empty())
                && fact.provenance["kind"] == "provider_response"
                && profiles.iter().any(|profile| {
                    fact.provenance["contract_id"] == serde_json::json!(profile.service.id)
                })
                && fact.provenance["result_ref"]
                    .as_str()
                    .and_then(|id| id.parse().ok())
                    .is_some_and(|id| task.active_results.contains(&id))
        })
        .collect();
    match names.first() {
        Some(name) if names.iter().all(|other| other.value == name.value) => {
            Screening::Ready((*name).clone())
        }
        _ => Screening::Unavailable,
    }
}

// ------------------------------------------------------------------- input

/// The recipe fields of a `pre-kyb-screening` run, normalised the way
/// `workflow_catalog::prepare` normalises them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Input {
    pub name: String,
    pub country: String,
    pub screening_level: String,
    pub registration: Option<String>,
    pub domain: Option<String>,
    pub vat_country: Option<String>,
    pub vat_number: Option<String>,
    pub iban: Option<String>,
}

impl Input {
    pub fn from_workflow(input: &BTreeMap<String, Value>) -> Input {
        let text = |key: &str| -> Option<String> {
            let value = input.get(key)?;
            let text = match value {
                Value::String(s) => s.trim().to_owned(),
                Value::Number(n) => n.to_string(),
                _ => return None,
            };
            (!text.is_empty()).then_some(text)
        };
        let level = text("screening_level").unwrap_or_default();
        Input {
            name: text("name").unwrap_or_default(),
            country: text("country").unwrap_or_default().to_ascii_uppercase(),
            screening_level: if level.eq_ignore_ascii_case("enhanced") { "Enhanced".into() } else { "Standard".into() },
            registration: text("registration").map(|r| r.replace([' ', '.', '-'], "").to_ascii_uppercase()),
            domain: text("domain").map(|d| d.trim_start_matches("https://").trim_start_matches("http://").trim_end_matches('/').to_ascii_lowercase()),
            vat_country: text("vat_country").map(|c| c.to_ascii_uppercase()),
            vat_number: text("vat_number").map(|n| n.replace([' ', '.', '-'], "").to_ascii_uppercase()),
            iban: text("iban").map(|i| i.replace(' ', "").to_ascii_uppercase()),
        }
    }
    pub fn enhanced(&self) -> bool {
        self.screening_level == "Enhanced"
    }
    fn vat(&self) -> Option<String> {
        Some(format!("{}{}", self.vat_country.as_deref()?, self.vat_number.as_deref()?))
    }
}

// -------------------------------------------------------------- evaluation

struct Check {
    id: &'static str,
    status: &'static str,
    detail: String,
    source: Option<&'static str>,
    evidence: Vec<Value>,
}
struct Identity {
    id: &'static str,
    value: String,
    source: &'static str,
    result: Value,
    evidence: Vec<Value>,
}

fn ev(result: &Value, pointer: &str) -> Value {
    json!({"result_ref":result["result_ref"],"pointer":pointer,"value":result["document"].pointer(pointer).cloned().unwrap_or(Value::Null)})
}
fn ev_value(result: &Value, pointer: &str, value: Value) -> Value {
    json!({"result_ref":result["result_ref"],"pointer":pointer,"value":value})
}
fn find<'a>(sources: &'a [Value], provider: Option<&str>, path: &str, exact: bool) -> Option<&'a Value> {
    sources.iter().rev().find(|s| {
        provider.is_none_or(|p| s["document"]["source"]["provider"] == p)
            && s["document"]["source"]["path"].as_str().is_some_and(|p| if exact { p == path } else { p.starts_with(path) })
    })
}
fn retrieved_at(result: &Value) -> Value {
    let d = &result["document"];
    [&d["source"]["retrieved_at"], &d["retrieved_at"], &d["observed_at"]].into_iter().find(|v| v.is_string()).cloned().unwrap_or(Value::Null)
}
/// Several proxies wrap their payload as `{"data": {...}}`; read through it.
fn payload(result: &Value) -> (&Value, &'static str) {
    let data = &result["document"]["data"];
    if data["data"].is_object() { (&data["data"], "/data/data") } else { (data, "/data") }
}
fn kvk_date(value: &Value) -> Option<NaiveDate> {
    let s = value.as_str()?;
    NaiveDate::parse_from_str(s, "%Y%m%d").ok().or_else(|| NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok())
}
fn mask_iban(iban: &str) -> String {
    if iban.len() < 8 { return iban.into() }
    format!("{} •••• {}", &iban[..4], &iban[iban.len() - 4..])
}
fn identifier(value: &str) -> String {
    let cleaned: String = value.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_uppercase()).collect();
    cleaned.trim_start_matches('0').to_owned()
}
fn strings(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) => vec![s.clone()],
        Value::Array(items) => items.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
        _ => vec![],
    }
}
fn label(id: &str) -> &'static str {
    match id {
        "identity_resolved" => "Registered identity resolved",
        "name_match" => "Name matches the register",
        "company_status" => "Company is active",
        "sanctions" => "No sanctions or watchlist match",
        "vat" => "VAT number valid",
        "domain" => "Domain established",
        "mail_dns" => "Mail DNS configured",
        "iban" => "IBAN valid",
        "officers" => "Officers listed",
        "controllers" => "Significant controllers listed",
        "insolvency" => "No insolvency records",
        "filings" => "Filings available",
        "alerts" => "No registry alerts",
        _ => "Check",
    }
}
fn identity_label(id: &str) -> &'static str {
    match id {
        "legal_name" => "Legal name",
        "trade_names" => "Trade names",
        "registration_number" => "Registration number",
        "rsin" => "RSIN",
        "legal_form" => "Legal form",
        "status" => "Status",
        "incorporated" => "Incorporated",
        "registered_address" => "Registered address",
        "activities" => "Activities",
        "vat" => "VAT",
        _ => "LEI",
    }
}
fn register(country: &str) -> &'static str {
    match country {
        "NL" => "KVK",
        "GB" => "Companies House",
        "FR" => "Sirenic",
        _ => "the register",
    }
}

/// The registry profile a run resolved, read once for identity and checks.
struct Profile<'a> {
    result: &'a Value,
    source: &'static str,
    number: String,
    names: Vec<String>,
    rsin: Option<String>,
    /// `pass`, `warn`, `fail` or `unknown` with the sentence that explains it,
    /// plus the pointer of the status evidence.
    status: (&'static str, String, &'static str),
}

fn profile<'a>(sources: &'a [Value], country: &str) -> Option<Profile<'a>> {
    if let Some(r) = find(sources, Some(KVK), "/api/v1/basisprofielen", false).filter(|_| country == "NL") {
        let d = &r["document"]["data"];
        let number = d["kvkNummer"].as_str().unwrap_or_default().to_owned();
        let mut names: Vec<String> = ["/statutaireNaam", "/naam"].iter().filter_map(|p| d.pointer(p).and_then(Value::as_str)).map(str::to_owned).collect();
        names.extend(d["handelsnamen"].as_array().into_iter().flatten().filter_map(|h| h["naam"].as_str()).map(str::to_owned));
        let status = match d.get("materieleRegistratie") {
            Some(Value::Object(_)) => match d.pointer("/materieleRegistratie/datumEinde").and_then(kvk_date) {
                Some(end) => ("fail", format!("KVK registration {number} ended on {end}."), "/data/materieleRegistratie/datumEinde"),
                None => ("pass", format!("KVK registration {number} is active (no end date recorded)."), "/data/materieleRegistratie"),
            },
            _ => ("unknown", "KVK returned no registration status.".into(), "/data/materieleRegistratie"),
        };
        return Some(Profile { result: r, source: "KVK", number, names, rsin: d.pointer("/_embedded/eigenaar/rsin").and_then(Value::as_str).map(str::to_owned), status });
    }
    if let Some(r) = find(sources, Some(CH), "/company", true).filter(|_| country == "GB") {
        let d = &r["document"]["data"];
        let number = d["company_number"].as_str().unwrap_or_default().to_owned();
        let names: Vec<String> = d["company_name"].as_str().map(str::to_owned).into_iter().collect();
        let status = match d["company_status"].as_str() {
            Some("active") => ("pass", format!("Companies House lists {number} as active."), "/data/company_status"),
            Some(s @ ("dissolved" | "converted-closed")) => ("fail", format!("Companies House status is '{s}'."), "/data/company_status"),
            Some(s) => ("warn", format!("Companies House status is '{s}', not active."), "/data/company_status"),
            None => ("unknown", "Companies House returned no company status.".into(), "/data/company_status"),
        };
        return Some(Profile { result: r, source: "Companies House", number, names, rsin: None, status });
    }
    if let Some(r) = find(sources, Some(SIRENIC), "/v1/entreprise/{siren}", true).filter(|_| country == "FR") {
        let d = &r["document"]["data"];
        let number = d["siren"].as_str().unwrap_or_default().to_owned();
        let names: Vec<String> = ["denomination", "nom_commercial", "sigle"].iter().filter_map(|k| d[*k].as_str()).map(str::to_owned).collect();
        let (state, pointer) = match d["etat_administratif"].as_str() {
            Some(s) => (Some(s), "/data/etat_administratif"),
            None => (d["statut"].as_str(), "/data/statut"),
        };
        let status = match state.map(str::to_ascii_lowercase).as_deref() {
            Some("actif" | "a" | "active") => ("pass", format!("The French register lists SIREN {number} as active."), pointer),
            Some("cesse" | "cessé" | "c" | "ferme" | "fermé" | "radie" | "radié") => ("fail", format!("The French register lists SIREN {number} as ceased."), pointer),
            Some(s) => ("warn", format!("The French register status is '{s}', not active."), pointer),
            None => ("unknown", "The French register returned no administrative status.".into(), pointer),
        };
        return Some(Profile { result: r, source: "Sirenic", number, names, rsin: None, status });
    }
    None
}

fn identity_lines(input: &Input, sources: &[Value], profile: Option<&Profile<'_>>) -> Vec<Identity> {
    let mut lines: Vec<Identity> = vec![];
    let mut line = |id: &'static str, value: Option<String>, source: &'static str, result: &Value, pointers: &[&str]| {
        if let Some(value) = value.filter(|v| !v.trim().is_empty()) {
            let evidence = pointers.iter().map(|p| ev(result, p)).collect();
            lines.push(Identity { id, value, source, result: result.clone(), evidence });
        }
    };
    if let Some(p) = profile {
        let r = p.result;
        let d = &r["document"]["data"];
        match p.source {
            "KVK" => {
                line("legal_name", d["statutaireNaam"].as_str().or(d["naam"].as_str()).map(str::to_owned), "KVK", r, &["/data/statutaireNaam"]);
                let trade: Vec<&str> = d["handelsnamen"].as_array().into_iter().flatten().filter_map(|h| h["naam"].as_str()).collect();
                line("trade_names", (!trade.is_empty()).then(|| trade.join(", ")), "KVK", r, &["/data/handelsnamen"]);
                line("registration_number", d["kvkNummer"].as_str().map(str::to_owned), "KVK", r, &["/data/kvkNummer"]);
                line("rsin", p.rsin.clone(), "KVK", r, &["/data/_embedded/eigenaar/rsin"]);
                let form = d.pointer("/_embedded/eigenaar/uitgebreideRechtsvorm").or_else(|| d.pointer("/_embedded/eigenaar/rechtsvorm")).and_then(Value::as_str);
                line("legal_form", form.map(str::to_owned), "KVK", r, &["/data/_embedded/eigenaar/rechtsvorm"]);
                if p.status.0 != "unknown" {
                    let text = match d.pointer("/materieleRegistratie/datumEinde").and_then(kvk_date) { Some(end) => format!("Registration ended {end}"), None => "Active registration".to_owned() };
                    line("status", Some(text), "KVK", r, &[p.status.2]);
                }
                let start = d.pointer("/materieleRegistratie/datumAanvang").or_else(|| d.pointer("/formeleRegistratiedatum")).and_then(kvk_date);
                line("incorporated", start.map(|d| d.to_string()), "KVK", r, &["/data/materieleRegistratie/datumAanvang"]);
                let addresses = d.pointer("/_embedded/hoofdvestiging/adressen").and_then(Value::as_array).cloned().unwrap_or_default();
                let address = addresses.iter().enumerate().filter(|(_, a)| a["indAfgeschermd"] != "Ja").find_map(|(i, a)| {
                    let text = a["volledigAdres"].as_str().map(str::to_owned).or_else(|| {
                        let parts: Vec<String> = [a["straatnaam"].as_str().map(str::to_owned), a["huisnummer"].as_u64().map(|n| n.to_string()), a["postcode"].as_str().map(str::to_owned), a["plaats"].as_str().map(str::to_owned)].into_iter().flatten().collect();
                        (!parts.is_empty()).then(|| parts.join(" "))
                    })?;
                    Some((i, text))
                });
                if let Some((i, text)) = address {
                    line("registered_address", Some(text), "KVK", r, &[&format!("/data/_embedded/hoofdvestiging/adressen/{i}")]);
                }
                let activities: Vec<String> = d["sbiActiviteiten"].as_array().into_iter().flatten().filter_map(|a| {
                    let code = a["sbiCode"].as_str()?;
                    Some(match a["sbiOmschrijving"].as_str() { Some(text) => format!("{code} {text}"), None => code.to_owned() })
                }).collect();
                line("activities", (!activities.is_empty()).then(|| activities.join("; ")), "KVK", r, &["/data/sbiActiviteiten"]);
            }
            "Companies House" => {
                line("legal_name", d["company_name"].as_str().map(str::to_owned), "Companies House", r, &["/data/company_name"]);
                line("registration_number", d["company_number"].as_str().map(str::to_owned), "Companies House", r, &["/data/company_number"]);
                line("legal_form", d["type"].as_str().map(|t| t.replace('-', " ")), "Companies House", r, &["/data/type"]);
                let status = d["company_status"].as_str().map(|s| match d["company_status_detail"].as_str() { Some(detail) => format!("{s} ({})", detail.replace('-', " ")), None => s.to_owned() });
                line("status", status, "Companies House", r, &["/data/company_status"]);
                line("incorporated", d["date_of_creation"].as_str().map(str::to_owned), "Companies House", r, &["/data/date_of_creation"]);
                let office = &d["registered_office_address"];
                let parts: Vec<&str> = ["premises", "address_line_1", "address_line_2", "locality", "region", "postal_code", "country"].iter().filter_map(|k| office[*k].as_str()).collect();
                line("registered_address", (!parts.is_empty()).then(|| parts.join(", ")), "Companies House", r, &["/data/registered_office_address"]);
                let sic: Vec<&str> = d["sic_codes"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                line("activities", (!sic.is_empty()).then(|| format!("SIC {}", sic.join(", "))), "Companies House", r, &["/data/sic_codes"]);
            }
            _ => {
                line("legal_name", d["denomination"].as_str().map(str::to_owned), "Sirenic", r, &["/data/denomination"]);
                let trade: Vec<&str> = ["nom_commercial", "sigle"].iter().filter_map(|k| d[*k].as_str()).collect();
                line("trade_names", (!trade.is_empty()).then(|| trade.join(", ")), "Sirenic", r, &["/data/nom_commercial"]);
                line("registration_number", d["siren"].as_str().map(str::to_owned), "Sirenic", r, &["/data/siren"]);
                let form = d["forme_juridique"].as_str().map(str::to_owned).or_else(|| d.pointer("/forme_juridique/libelle").and_then(Value::as_str).map(str::to_owned)).or_else(|| d["nature_juridique"].as_str().map(|c| format!("Legal category {c}")));
                line("legal_form", form, "Sirenic", r, &["/data/nature_juridique"]);
                line("status", d["etat_administratif"].as_str().or(d["statut"].as_str()).map(str::to_owned), "Sirenic", r, &[p.status.2]);
                line("incorporated", d["date_creation"].as_str().map(str::to_owned), "Sirenic", r, &["/data/date_creation"]);
                let siege = &d["siege"];
                let address = siege["adresse"].as_str().map(str::to_owned).or_else(|| {
                    let parts: Vec<&str> = ["code_postal", "commune"].iter().filter_map(|k| siege[*k].as_str()).collect();
                    (!parts.is_empty()).then(|| parts.join(" "))
                });
                line("registered_address", address, "Sirenic", r, &["/data/siege/adresse"]);
                line("activities", d["activite_principale"].as_str().map(|c| format!("NAF {c}")), "Sirenic", r, &["/data/activite_principale"]);
            }
        }
    }
    if let (Some(vat), Some(r)) = (input.vat(), find(sources, Some("vies"), "/taxation_customs/vies", false)) {
        let (d, base) = payload(r);
        if d["valid"] == true {
            let name = d["name"].as_str().map(str::trim).filter(|n| !n.is_empty() && *n != "---");
            let text = match name { Some(n) => format!("{vat} (valid, registered to {n})"), None => format!("{vat} (valid)") };
            line("vat", Some(text), "VIES", r, &[&format!("{base}/valid")]);
        }
    }
    if let Some(r) = find(sources, Some("gleif"), "/", false) {
        let d = &r["document"]["data"];
        let lei = [d.pointer("/data/0/attributes/lei"), d.pointer("/data/attributes/lei"), d.pointer("/attributes/lei"), d.get("lei")].into_iter().flatten().find_map(Value::as_str);
        line("lei", lei.map(str::to_owned), "GLEIF", r, &["/data"]);
    }
    let order = ["legal_name", "trade_names", "registration_number", "rsin", "legal_form", "status", "incorporated", "registered_address", "activities", "vat", "lei"];
    lines.sort_by_key(|l| order.iter().position(|o| *o == l.id).unwrap_or(order.len()));
    lines
}

/// `sources`: saved documents as `{result_ref, document}` for every step of
/// the approved plan. `failures`: `"{api_slug} {path}"` → error for the steps
/// that failed. Produces the `pre_kyb_check` analysis from the contract.
pub fn evaluate(input: &Input, sources: &[Value], failures: &BTreeMap<String, Value>, now: DateTime<Utc>) -> Value {
    let enhanced = input.enhanced();
    let country = input.country.as_str();
    let reg = register(country);
    let mut checks: Vec<Check> = vec![];
    let mut limitations: Vec<String> = vec![];
    // A refunded 404 is a definitive "no record", unlike an unavailable source.
    let not_found = |provider: &str, path: &str| failures.iter().any(|(step, error)| step.starts_with(&format!("{provider} ")) && step.contains(path) && error["code"] == "source_not_found");

    // --- identity
    let profile = profile(sources, country);
    let identity = identity_lines(input, sources, profile.as_ref());
    let registry_number = profile.as_ref().map(|p| p.number.clone()).filter(|n| !n.is_empty()).or_else(|| input.registration.clone());
    match &profile {
        Some(p) => {
            let pointer = match p.source { "KVK" => "/data/kvkNummer", "Companies House" => "/data/company_number", _ => "/data/siren" };
            checks.push(Check { id: "identity_resolved", status: "pass", detail: format!("Registered with {} under {}.", p.source, p.number), source: Some(p.source), evidence: vec![ev(p.result, pointer)] });
            if p.names.is_empty() {
                checks.push(Check { id: "name_match", status: "unknown", detail: format!("{} returned no company name to compare with '{}'.", p.source, input.name), source: Some(p.source), evidence: vec![] });
            } else {
                let matched = p.names.iter().any(|n| names_match(&input.name, n));
                let first = &p.names[0];
                let pointer = match p.source { "KVK" => "/data/statutaireNaam", "Companies House" => "/data/company_name", _ => "/data/denomination" };
                checks.push(Check { id: "name_match", status: if matched { "pass" } else { "warn" },
                    detail: if matched { format!("Supplied name matches the registered name '{first}'.") } else { format!("Supplied name '{}' differs from the registered name '{first}'.", input.name) },
                    source: Some(p.source), evidence: vec![ev(p.result, pointer)] });
            }
            let (status, detail, pointer) = p.status.clone();
            checks.push(Check { id: "company_status", status, detail, source: Some(p.source), evidence: if status == "unknown" { vec![] } else { vec![ev(p.result, pointer)] } });
        }
        None => {
            let search = match country {
                "NL" => find(sources, Some(KVK), "/api/v2/zoeken", false).map(|r| (r, "/data/resultaten", r["document"]["data"]["resultaten"].as_array().map_or(0, Vec::len))),
                "GB" => find(sources, Some(CH), "/search/companies", true).map(|r| (r, "/data/items", r["document"]["data"]["items"].as_array().map_or(0, Vec::len))),
                _ => find(sources, Some(SIRENIC), "/v1/recherche", true).map(|r| (r, "/data/resultats", r["document"]["data"]["resultats"].as_array().map_or(0, Vec::len))),
            };
            let provider = match country { "NL" => KVK, "GB" => CH, _ => SIRENIC };
            checks.push(match search {
                Some((r, pointer, 0)) => Check { id: "identity_resolved", status: "fail", detail: format!("{reg} returned no company for '{}'.", input.name), source: Some(reg), evidence: vec![ev(r, pointer)] },
                Some((r, pointer, n)) => Check { id: "identity_resolved", status: "warn", detail: format!("{reg} name search returned {n} candidate{}; the registered identity was not resolved to one profile.", if n == 1 { "" } else { "s" }), source: Some(reg), evidence: vec![ev_value(r, pointer, json!(n))] },
                None if not_found(provider, "") => Check { id: "identity_resolved", status: "fail", detail: match &input.registration { Some(n) => format!("{reg} has no record under {n}."), None => format!("{reg} has no record for '{}'.", input.name) }, source: Some(reg), evidence: vec![] },
                None => Check { id: "identity_resolved", status: "unknown", detail: format!("{reg} was unavailable; the registered identity could not be resolved."), source: Some(reg), evidence: vec![] },
            });
        }
    }
    let registry_names: Vec<String> = profile.as_ref().map(|p| p.names.clone()).unwrap_or_default();

    // --- sanctions
    match find(sources, Some("opensanctions"), "/match", false) {
        Some(r) => {
            let results = r["document"]["data"].pointer("/responses/q1/results").and_then(Value::as_array).cloned().unwrap_or_default();
            let ids: Vec<String> = registry_number.iter().chain(input.registration.iter()).map(|n| identifier(n)).filter(|n| !n.is_empty()).collect();
            let lei = identity.iter().find(|l| l.id == "lei").map(|l| identifier(&l.value));
            let confirmed = results.iter().enumerate().find(|(_, x)| {
                let numbers = strings(&x["properties"]["registrationNumber"]).into_iter().chain(strings(&x["properties"]["idNumber"])).chain(strings(&x["properties"]["ogrnCode"]));
                let mut numbers: Vec<String> = numbers.map(|n| identifier(&n)).collect();
                numbers.extend(strings(&x["properties"]["leiCode"]).iter().map(|l| identifier(l)));
                numbers.iter().any(|n| !n.is_empty() && (ids.contains(n) || lei.as_deref() == Some(n.as_str())))
            });
            checks.push(match confirmed {
                Some((i, x)) => Check { id: "sanctions", status: "fail", detail: format!("OpenSanctions entity '{}' carries the same registration identifier as the registry record.", x["caption"].as_str().unwrap_or("?")), source: Some("OpenSanctions"), evidence: vec![ev(r, &format!("/data/responses/q1/results/{i}/caption")), ev(r, &format!("/data/responses/q1/results/{i}/properties/registrationNumber"))] },
                None if results.is_empty() => Check { id: "sanctions", status: "pass", detail: format!("OpenSanctions returned no candidates for '{}'.", input.name), source: Some("OpenSanctions"), evidence: vec![ev(r, "/data/responses/q1/total")] },
                None => {
                    // `results` is non-empty here (the empty case matched above); the fallback never runs.
                    let (i, x) = results.iter().enumerate().max_by(|a, b| a.1["score"].as_f64().unwrap_or(0.0).total_cmp(&b.1["score"].as_f64().unwrap_or(0.0))).unwrap_or((0, &results[0]));
                    let caption = x["caption"].as_str().unwrap_or("?");
                    let topics: Vec<&str> = x.pointer("/properties/topics").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
                    let kind = if topics.iter().any(|t| t.starts_with("sanction")) { "sanctions-list candidate" } else if topics.contains(&"role.pep") { "PEP-linked candidate" } else { "watchlist candidate" };
                    let n = results.len();
                    Check { id: "sanctions", status: "warn", detail: format!("OpenSanctions returned {n} {}; best {kind} '{caption}' (score {:.2}). The identity is not confirmed by a registration number.", if n == 1 { "candidate" } else { "candidates" }, x["score"].as_f64().unwrap_or(0.0)), source: Some("OpenSanctions"), evidence: vec![ev(r, &format!("/data/responses/q1/results/{i}/caption")), ev(r, &format!("/data/responses/q1/results/{i}/score"))] }
                }
            });
        }
        None => checks.push(Check { id: "sanctions", status: "unknown", detail: "Sanctions screening was unavailable.".into(), source: Some("OpenSanctions"), evidence: vec![] }),
    }

    // --- VAT
    if let Some(vat) = input.vat() {
        checks.push(match find(sources, Some("vies"), "/taxation_customs/vies", false) {
            Some(r) => {
                let (d, base) = payload(r);
                let vies_name = d["name"].as_str().map(str::trim).filter(|n| !n.is_empty() && *n != "---");
                match d["valid"].as_bool() {
                    Some(false) => Check { id: "vat", status: "fail", detail: format!("VAT number {vat} is not valid according to VIES."), source: Some("VIES"), evidence: vec![ev(r, &format!("{base}/valid"))] },
                    Some(true) => {
                        // The RSIN comes from the registry profile, so the profile is in hand whenever the RSIN is.
                        let rsin_profile = profile.as_ref().and_then(|p| p.rsin.as_deref().map(|rsin| (rsin, p)));
                        if let Some((_, registry)) = rsin_profile.filter(|(rsin, _)| vat.starts_with("NL") && vat.get(2..11) == Some(*rsin)) {
                            Check { id: "vat", status: "pass", detail: format!("VAT number {vat} is valid and belongs to the RSIN of this KVK registration."), source: Some("VIES"), evidence: vec![ev(r, &format!("{base}/valid")), ev(registry.result, "/data/_embedded/eigenaar/rsin")] }
                        } else if let Some(vn) = vies_name {
                            let matched = names_match(&input.name, vn) || registry_names.iter().any(|n| names_match(n, vn));
                            Check { id: "vat", status: if matched { "pass" } else { "warn" },
                                detail: if matched { format!("VAT number {vat} is valid and registered to '{vn}'.") } else { format!("VAT number {vat} is valid but registered to '{vn}', which differs from the supplied and registered names.") },
                                source: Some("VIES"), evidence: vec![ev(r, &format!("{base}/name"))] }
                        } else {
                            limitations.push("VIES returned no name for this VAT number; only validity was checked.".into());
                            Check { id: "vat", status: "pass", detail: format!("VAT number {vat} is valid; VIES returned no name to compare."), source: Some("VIES"), evidence: vec![ev(r, &format!("{base}/valid"))] }
                        }
                    }
                    None => Check { id: "vat", status: "unknown", detail: "VIES returned no validity flag.".into(), source: Some("VIES"), evidence: vec![] },
                }
            }
            None => Check { id: "vat", status: "unknown", detail: "VIES was unavailable; the VAT number was not checked.".into(), source: Some("VIES"), evidence: vec![] },
        });
    }

    // --- domain
    if let Some(domain) = &input.domain {
        let rdap = find(sources, None, "/domain/{domain}", true);
        checks.push(match rdap {
            Some(r) => {
                let (d, base) = payload(r);
                match d["registered_at"].as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|d| d.with_timezone(&Utc)) {
                    Some(at) => {
                        let days = (now - at).num_days().max(0);
                        let date = at.date_naive();
                        Check { id: "domain", status: if days >= DOMAIN_MIN_DAYS { "pass" } else { "warn" },
                            detail: if days >= DOMAIN_MIN_DAYS { format!("Domain {domain} was registered on {date} ({days} days ago).") } else { format!("Domain {domain} was registered recently, on {date} ({days} days ago).") },
                            source: Some("RDAP & DNS"), evidence: vec![ev(r, &format!("{base}/registered_at"))] }
                    }
                    None => Check { id: "domain", status: "warn", detail: format!("The registry of {domain} publishes no registration date."), source: Some("RDAP & DNS"), evidence: vec![ev(r, &format!("{base}/registered_at"))] },
                }
            }
            None if not_found("domain-intelligence", "/domain/") || not_found("rdap", "/domain/") => Check { id: "domain", status: "warn", detail: format!("RDAP has no registration record for {domain}."), source: Some("RDAP & DNS"), evidence: vec![] },
            None => Check { id: "domain", status: "unknown", detail: format!("RDAP was unavailable; the registration of {domain} was not checked."), source: Some("RDAP & DNS"), evidence: vec![] },
        });
        if enhanced {
            checks.push(match find(sources, None, "/dns/{domain}", true) {
                Some(r) => {
                    let (d, base) = payload(r);
                    let mx: Vec<&str> = d["mx"].as_array().into_iter().flatten().filter_map(Value::as_str).filter(|m| m.split_whitespace().nth(1).is_some_and(|h| h != ".")).collect();
                    let has_mail = !mx.is_empty() || d["accepts_mail"] == true;
                    let txt: Vec<String> = strings(&d["txt"]).into_iter().chain(d["records"].as_array().into_iter().flatten().filter_map(|x| x["data"].as_str()).map(str::to_owned)).chain(strings(&d["spf"])).collect();
                    let spf = match (&d["spf"], txt.is_empty()) {
                        (Value::Bool(b), _) => Some(*b),
                        (_, false) => Some(txt.iter().any(|t| t.to_ascii_lowercase().contains("v=spf1"))),
                        _ => None,
                    };
                    match (has_mail, spf) {
                        (true, Some(true)) => Check { id: "mail_dns", status: "pass", detail: format!("{domain} publishes MX ({} record{}) and SPF.", mx.len(), if mx.len() == 1 { "" } else { "s" }), source: Some("RDAP & DNS"), evidence: vec![ev(r, &format!("{base}/mx"))] },
                        (true, Some(false)) => Check { id: "mail_dns", status: "warn", detail: format!("{domain} publishes MX but no SPF record."), source: Some("RDAP & DNS"), evidence: vec![ev(r, &format!("{base}/mx"))] },
                        (true, None) => {
                            limitations.push("The mail DNS source reports MX records only; SPF was not retrieved.".into());
                            Check { id: "mail_dns", status: "pass", detail: format!("{domain} publishes MX ({} record{}); SPF was not reported by this source.", mx.len(), if mx.len() == 1 { "" } else { "s" }), source: Some("RDAP & DNS"), evidence: vec![ev(r, &format!("{base}/mx"))] }
                        }
                        (false, _) => Check { id: "mail_dns", status: "warn", detail: format!("{domain} publishes no MX record (receives no e-mail)."), source: Some("RDAP & DNS"), evidence: vec![ev(r, &format!("{base}/mx"))] },
                    }
                }
                None => Check { id: "mail_dns", status: "unknown", detail: format!("DNS was unavailable; the mail setup of {domain} was not checked."), source: Some("RDAP & DNS"), evidence: vec![] },
            });
        } else {
            checks.push(Check { id: "mail_dns", status: "not_selected", detail: "Enhanced screening checks MX and SPF records.".into(), source: Some("RDAP & DNS"), evidence: vec![] });
        }
    }

    // --- IBAN
    if let Some(iban) = &input.iban {
        let own = find(sources, Some("iban-check"), "/iban/", false);
        let doc = own.or_else(|| find(sources, Some(SIRENIC), "/v1/iban/verifier", false));
        let masked = mask_iban(iban);
        checks.push(match doc {
            Some(r) => {
                let (d, base) = payload(r);
                let valid = d["valid"].as_bool().or(d["valide"].as_bool());
                let bank = d.pointer("/bank/name").or_else(|| d.pointer("/banque/nom")).and_then(Value::as_str);
                let (pointer, bank_pointer) = if d["valid"].is_boolean() { (format!("{base}/valid"), format!("{base}/bank/name")) } else { (format!("{base}/valide"), format!("{base}/banque/nom")) };
                match valid {
                    Some(true) => Check { id: "iban", status: "pass", detail: match bank { Some(b) => format!("IBAN {masked} is structurally valid ({b}). The account holder was not verified."), None => format!("IBAN {masked} is structurally valid. The account holder was not verified.") }, source: Some("IBAN check"), evidence: vec![ev(r, &pointer), ev(r, &bank_pointer)] },
                    Some(false) => Check { id: "iban", status: "fail", detail: format!("IBAN {masked} is invalid (structure or check digits fail)."), source: Some("IBAN check"), evidence: vec![ev(r, &pointer)] },
                    None if iban_valid(iban) => Check { id: "iban", status: "pass", detail: format!("IBAN {masked} passes the structure and check-digit test; the checker returned no validity flag."), source: Some("IBAN check"), evidence: vec![ev(r, base)] },
                    None => Check { id: "iban", status: "fail", detail: format!("IBAN {masked} fails the structure or check-digit test."), source: None, evidence: vec![] },
                }
            }
            None if !iban_valid(iban) => Check { id: "iban", status: "fail", detail: format!("IBAN {masked} fails the structure or check-digit test."), source: None, evidence: vec![] },
            None => Check { id: "iban", status: "unknown", detail: format!("The IBAN checker was unavailable; {masked} passes the local check-digit test only."), source: Some("IBAN check"), evidence: vec![] },
        });
    }

    // --- country-specific Enhanced records
    let mut enhanced_only = |id: &'static str, source: &'static str, adds: &str, check: &mut dyn FnMut() -> Check| {
        if enhanced { checks.push(check()) } else { checks.push(Check { id, status: "not_selected", detail: format!("Enhanced screening adds {adds}."), source: Some(source), evidence: vec![] }) }
    };
    match country {
        "GB" => {
            enhanced_only("officers", "Companies House", "the current officer roster", &mut || match find(sources, Some(CH), "/company/{company_number}/officers", true) {
                Some(r) => {
                    let d = &r["document"]["data"];
                    let active = d["active_count"].as_u64().unwrap_or_else(|| d["items"].as_array().into_iter().flatten().filter(|i| i["resigned_on"].is_null()).count() as u64);
                    if active > 0 { Check { id: "officers", status: "pass", detail: format!("{active} active officer{} listed; officers are not individually screened.", if active == 1 { "" } else { "s" }), source: Some("Companies House"), evidence: vec![ev_value(r, "/data/active_count", json!(active))] } }
                    else { Check { id: "officers", status: "warn", detail: "No active officers are listed.".into(), source: Some("Companies House"), evidence: vec![ev(r, "/data/items")] } }
                }
                None => Check { id: "officers", status: "unknown", detail: "The officer list was unavailable.".into(), source: Some("Companies House"), evidence: vec![] },
            });
            enhanced_only("controllers", "Companies House", "persons with significant control", &mut || match find(sources, Some(CH), "/company/{company_number}/persons-with-significant-control", true) {
                Some(r) => {
                    let d = &r["document"]["data"];
                    let active = d["active_count"].as_u64().unwrap_or_else(|| d["items"].as_array().into_iter().flatten().filter(|i| i["ceased_on"].is_null() && i["ceased"] != true).count() as u64);
                    if active > 0 { Check { id: "controllers", status: "pass", detail: format!("{active} person{} with significant control listed; not screened or verified as ultimate owners.", if active == 1 { "" } else { "s" }), source: Some("Companies House"), evidence: vec![ev_value(r, "/data/active_count", json!(active))] } }
                    else { Check { id: "controllers", status: "warn", detail: "No persons with significant control are listed.".into(), source: Some("Companies House"), evidence: vec![ev(r, "/data/items")] } }
                }
                None => Check { id: "controllers", status: "unknown", detail: "The significant-control register was unavailable.".into(), source: Some("Companies House"), evidence: vec![] },
            });
            enhanced_only("insolvency", "Sirenic", "UK insolvency records", &mut || match find(sources, None, "/v1/eu/entreprise/GB/{company_number}/insolvabilite", true) {
                Some(r) => {
                    let d = &r["document"]["data"];
                    let cases = d["cas"].as_array().map_or(0, Vec::len);
                    let none = d["aucune_procedure"] == true || (d["aucune_procedure"].is_null() && cases == 0);
                    if none { Check { id: "insolvency", status: "pass", detail: "No insolvency records.".into(), source: Some("Sirenic"), evidence: vec![ev(r, "/data/aucune_procedure")] } }
                    else {
                        let statuses: Vec<&str> = d["statuts"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                        Check { id: "insolvency", status: "fail", detail: format!("Insolvency records present{}.", if statuses.is_empty() { String::new() } else { format!(" ({})", statuses.join(", ")) }), source: Some("Sirenic"), evidence: vec![ev(r, "/data/statuts"), ev_value(r, "/data/cas", json!(cases))] }
                    }
                }
                None => Check { id: "insolvency", status: "unknown", detail: "Insolvency records were unavailable.".into(), source: Some("Sirenic"), evidence: vec![] },
            });
        }
        "NL" => {
            enhanced_only("filings", "KVK", "available KVK annual accounts", &mut || match find(sources, Some(KVK), "/api/v1/hvds/jaarrekeningen", false) {
                Some(r) => {
                    let d = &r["document"]["data"];
                    let present = d.as_object().is_some_and(|o| !o.is_empty()) || d.as_array().is_some_and(|a| !a.is_empty());
                    if present { Check { id: "filings", status: "pass", detail: "KVK annual accounts are available.".into(), source: Some("KVK"), evidence: vec![ev_value(r, "/data", json!("available"))] } }
                    else { Check { id: "filings", status: "warn", detail: "No KVK annual accounts are available.".into(), source: Some("KVK"), evidence: vec![ev_value(r, "/data", json!("empty"))] } }
                }
                None if not_found(KVK, "jaarrekeningen") => Check { id: "filings", status: "warn", detail: "No KVK annual accounts are available.".into(), source: Some("KVK"), evidence: vec![] },
                None => Check { id: "filings", status: "unknown", detail: "KVK annual accounts were unavailable.".into(), source: Some("KVK"), evidence: vec![] },
            });
        }
        "FR" => {
            enhanced_only("filings", "Sirenic", "available French financial figures", &mut || match find(sources, Some(SIRENIC), "/v1/entreprise/{siren}/finances", true) {
                Some(r) => {
                    let d = &r["document"]["data"];
                    let years = d["nombre_exercices"].as_u64().unwrap_or_else(|| d["exercices"].as_array().map_or(0, Vec::len) as u64);
                    if years > 0 { Check { id: "filings", status: "pass", detail: format!("Financial figures available for {years} financial year{}.", if years == 1 { "" } else { "s" }), source: Some("Sirenic"), evidence: vec![ev_value(r, "/data/nombre_exercices", json!(years))] } }
                    else { Check { id: "filings", status: "warn", detail: "No financial figures are available.".into(), source: Some("Sirenic"), evidence: vec![ev(r, "/data/nombre_exercices")] } }
                }
                None if not_found(SIRENIC, "/finances") => Check { id: "filings", status: "warn", detail: "No financial figures are available.".into(), source: Some("Sirenic"), evidence: vec![] },
                None => Check { id: "filings", status: "unknown", detail: "French financial figures were unavailable.".into(), source: Some("Sirenic"), evidence: vec![] },
            });
            enhanced_only("alerts", "Sirenic", "BODACC alerts (collective proceedings, deregistrations)", &mut || match find(sources, Some(SIRENIC), "/v1/entreprise/{siren}/alertes", true) {
                Some(r) => {
                    let d = &r["document"]["data"];
                    let count = |k: &str| d[k].as_array().map_or(0, Vec::len);
                    let (proc_, rad, sales) = (count("procedures_collectives"), count("radiations"), count("ventes_cessions"));
                    if proc_ + rad + sales == 0 { Check { id: "alerts", status: "pass", detail: "No collective proceedings, deregistrations or business sales announced.".into(), source: Some("Sirenic"), evidence: vec![ev_value(r, "/data/procedures_collectives", json!(0))] } }
                    else { Check { id: "alerts", status: "warn", detail: format!("Alerts present: {proc_} collective proceeding(s), {rad} deregistration(s), {sales} business sale(s)."), source: Some("Sirenic"), evidence: vec![ev_value(r, "/data/procedures_collectives", json!(proc_)), ev_value(r, "/data/radiations", json!(rad))] } }
                }
                None => Check { id: "alerts", status: "unknown", detail: "French registry alerts were unavailable.".into(), source: Some("Sirenic"), evidence: vec![] },
            });
        }
        _ => {}
    }

    if !failures.is_empty() {
        limitations.push("Not every source returned a result; missing checks are marked 'unknown'.".into());
    }

    // --- decision
    let order = ["identity_resolved", "name_match", "company_status", "sanctions", "vat", "domain", "mail_dns", "iban", "officers", "controllers", "insolvency", "filings", "alerts"];
    checks.sort_by_key(|c| order.iter().position(|o| *o == c.id).unwrap_or(order.len()));
    let status_of = |id: &str| checks.iter().find(|c| c.id == id).map(|c| c.status);
    let reject = status_of("company_status") == Some("fail") || status_of("sanctions") == Some("fail");
    let name_ok = match status_of("name_match") {
        Some(s) => s == "pass",
        None => input.registration.is_some() && !registry_names.is_empty(),
    };
    let all_clear = checks.iter().all(|c| c.status == "pass" || c.status == "not_selected");
    let decision = if reject { "reject" } else if status_of("identity_resolved") == Some("pass") && name_ok && all_clear { "pass" } else { "review" };
    let flagged: Vec<&Check> = checks.iter().filter(|c| c.status == "fail").chain(checks.iter().filter(|c| c.status == "warn")).collect();
    let mut reasons: Vec<String> = flagged.iter().map(|c| c.detail.clone()).collect();
    reasons.extend(checks.iter().filter(|c| c.status == "unknown").map(|c| c.detail.clone()));
    reasons.truncate(6);
    let (label_text, headline, next_action) = match decision {
        "reject" => ("REJECT", format!("⛔ REJECT: {}", reasons.first().cloned().unwrap_or_default()), "Do not onboard on this evidence. Escalate to compliance with the cited register records before any full KYB."),
        "pass" => ("PASS", "✅ PASS: no red flags in the completed checks".to_owned(), "Proceed to full KYB. This pre-screen complements it and does not replace it."),
        _ => {
            let n = checks.iter().filter(|c| matches!(c.status, "fail" | "warn" | "unknown")).count();
            ("REVIEW", format!("🔎 REVIEW: {n} check{} need{} attention", if n == 1 { "" } else { "s" }, if n == 1 { "s" } else { "" }), "Review the flagged checks before onboarding, then proceed to full KYB. This pre-screen complements it and does not replace it.")
        }
    };
    let partial = checks.iter().any(|c| c.status == "unknown");

    let mut coverage_gaps = vec!["Directors and owners were not individually screened for sanctions, PEP status or disqualification.".to_owned(), "Ultimate beneficial ownership was not established.".to_owned()];
    if input.iban.is_some() { coverage_gaps.push("The bank account holder was not verified; IBAN checks cover structure and bank only.".into()) }
    coverage_gaps.push("No credit score or payment behaviour data was used.".into());
    if !identity.iter().any(|l| l.id == "lei") { coverage_gaps.push("LEI and corporate parents were not retrieved.".into()) }

    let rendered_identity: Vec<Value> = identity.iter().map(|l| json!({"id":l.id,"label":identity_label(l.id),"value":l.value,"source":l.source,"result_ref":l.result["result_ref"],"retrieved_at":retrieved_at(&l.result),"evidence":l.evidence})).collect();
    let rendered_checks: Vec<Value> = checks.iter().map(|c| json!({"id":c.id,"label":label(c.id),"status":c.status,"detail":c.detail,"source":c.source,"evidence":c.evidence})).collect();
    let mut observations = vec![json!({"text":headline,"evidence":[]})];
    observations.extend(flagged.iter().take(5).map(|c| json!({"text":c.detail,"evidence":c.evidence})));
    json!({
        "status": if partial { "partial" } else { "completed" },
        "kind": KIND,
        "verdict": {
            "decision": decision, "label": label_text, "headline": headline, "language": "en",
            "reasons": reasons,
            "next_action": next_action,
            "subject": {"name": input.name, "country": input.country, "screening_level": input.screening_level, "registration": input.registration,
                "vat_number": input.vat(), "domain": input.domain, "iban_masked": input.iban.as_deref().map(mask_iban)},
            "identity": rendered_identity,
            "checks": rendered_checks,
            "coverage_gaps": coverage_gaps,
        },
        "observations": observations,
        "limitations": limitations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn doc(provider: &str, path: &str, data: Value) -> Value {
        json!({"result_ref":Uuid::new_v4(),"document":{"source":{"provider":provider,"path":path,"retrieved_at":"2026-10-18T10:00:00Z"},"data":data}})
    }
    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-18T12:00:00Z").unwrap().with_timezone(&Utc)
    }
    fn given(values: Value) -> Input {
        Input::from_workflow(&serde_json::from_value(values).unwrap())
    }
    fn kvk_profile() -> Value {
        json!({"kvkNummer":"30204462","statutaireNaam":"Mollie B.V.","handelsnamen":[{"naam":"Mollie"}],"materieleRegistratie":{"datumAanvang":"20050421"},
            "sbiActiviteiten":[{"sbiCode":"6499","sbiOmschrijving":"Overige financiële intermediatie"}],
            "_embedded":{"eigenaar":{"rsin":"815839091","rechtsvorm":"Besloten Vennootschap"},"hoofdvestiging":{"adressen":[{"type":"bezoekadres","volledigAdres":"Keizersgracht 126 1015CW Amsterdam","indAfgeschermd":"Nee"}]}}})
    }
    fn no_candidates() -> Value {
        doc("opensanctions", "/match", json!({"responses":{"q1":{"results":[],"total":{"value":0}}}}))
    }
    fn status(v: &Value, id: &str) -> String {
        v["verdict"]["checks"].as_array().unwrap().iter().find(|c| c["id"] == id).map(|c| c["status"].as_str().unwrap().to_owned()).unwrap_or_else(|| panic!("check {id} missing in {v:#}"))
    }
    fn has_check(v: &Value, id: &str) -> bool {
        v["verdict"]["checks"].as_array().unwrap().iter().any(|c| c["id"] == id)
    }

    #[test]
    fn nl_standard_with_registration_passes_and_builds_the_identity_card() {
        let input = given(json!({"name":"Mollie B.V.","country":"NL","screening_level":"Standard","registration":"30204462"}));
        let sources = vec![doc(KVK, "/api/v1/basisprofielen", kvk_profile()), no_candidates()];
        let v = evaluate(&input, &sources, &BTreeMap::new(), now());
        assert_eq!(v["kind"], "pre_kyb_check");
        assert_eq!(v["status"], "completed", "{v:#}");
        assert_eq!(v["verdict"]["decision"], "pass", "{v:#}");
        assert_eq!(v["verdict"]["label"], "PASS");
        assert_eq!(v["verdict"]["language"], "en");
        assert!(v["verdict"]["next_action"].as_str().unwrap().starts_with("Proceed to full KYB"));
        let ids: Vec<&str> = v["verdict"]["identity"].as_array().unwrap().iter().map(|l| l["id"].as_str().unwrap()).collect();
        for want in ["legal_name", "registration_number", "rsin", "registered_address"] {
            assert!(ids.contains(&want), "{ids:?}");
        }
        let legal = &v["verdict"]["identity"][0];
        assert_eq!(legal["id"], "legal_name");
        assert_eq!(legal["value"], "Mollie B.V.");
        assert_eq!(legal["source"], "KVK");
        assert_eq!(legal["retrieved_at"], "2026-10-18T10:00:00Z");
        assert_eq!(legal["evidence"][0]["pointer"], "/data/statutaireNaam");
        assert_eq!(status(&v, "identity_resolved"), "pass");
        assert_eq!(status(&v, "name_match"), "pass");
        assert_eq!(status(&v, "company_status"), "pass");
        assert_eq!(status(&v, "sanctions"), "pass");
        assert!(!has_check(&v, "vat") && !has_check(&v, "domain") && !has_check(&v, "iban"), "{v:#}");
        assert_eq!(v["observations"][0]["text"], "✅ PASS: no red flags in the completed checks");
        let gaps = v["verdict"]["coverage_gaps"].as_array().unwrap();
        assert_eq!(gaps.len(), 4, "{gaps:?}");
        assert!(!gaps.iter().any(|g| g.as_str().unwrap().contains("bank account holder")));
    }
    #[test]
    fn a_name_that_matches_no_registered_name_is_a_review() {
        let input = given(json!({"name":"Globex Corporation","country":"NL","screening_level":"Standard","registration":"30204462"}));
        let sources = vec![doc(KVK, "/api/v1/basisprofielen", kvk_profile()), no_candidates()];
        let v = evaluate(&input, &sources, &BTreeMap::new(), now());
        assert_eq!(v["verdict"]["decision"], "review", "{v:#}");
        assert_eq!(status(&v, "name_match"), "warn");
        assert!(v["verdict"]["reasons"][0].as_str().unwrap().contains("differs from the registered name 'Mollie B.V.'"), "{v:#}");
        assert!(v["verdict"]["headline"].as_str().unwrap().starts_with("🔎 REVIEW"));
    }
    #[test]
    fn an_ended_kvk_registration_is_a_reject() {
        let input = given(json!({"name":"Mollie B.V.","country":"NL","screening_level":"Standard","registration":"30204462"}));
        let mut profile = kvk_profile();
        profile["materieleRegistratie"]["datumEinde"] = json!("20240101");
        let sources = vec![doc(KVK, "/api/v1/basisprofielen", profile), no_candidates()];
        let v = evaluate(&input, &sources, &BTreeMap::new(), now());
        assert_eq!(v["verdict"]["decision"], "reject", "{v:#}");
        assert_eq!(status(&v, "company_status"), "fail");
        assert!(v["verdict"]["headline"].as_str().unwrap().contains("ended on 2024-01-01"));
        assert_eq!(v["verdict"]["identity"].as_array().unwrap().iter().find(|l| l["id"] == "status").unwrap()["value"], "Registration ended 2024-01-01");
    }
    #[test]
    fn a_sanctions_candidate_with_the_registry_number_is_a_reject() {
        let input = given(json!({"name":"Mollie B.V.","country":"NL","screening_level":"Standard","registration":"30204462"}));
        let hit = json!({"responses":{"q1":{"results":[{"id":"x","match":true,"score":0.95,"caption":"MOLLIE B.V.","properties":{"topics":["sanction"],"registrationNumber":["30204462"]}}],"total":{"value":1}}}});
        let sources = vec![doc(KVK, "/api/v1/basisprofielen", kvk_profile()), doc("opensanctions", "/match", hit)];
        let v = evaluate(&input, &sources, &BTreeMap::new(), now());
        assert_eq!(v["verdict"]["decision"], "reject", "{v:#}");
        assert_eq!(status(&v, "sanctions"), "fail");
        assert!(v["verdict"]["reasons"][0].as_str().unwrap().contains("same registration identifier"));
    }
    #[test]
    fn a_name_only_sanctions_candidate_is_a_review() {
        let input = given(json!({"name":"Mollie B.V.","country":"NL","screening_level":"Standard","registration":"30204462"}));
        let hit = json!({"responses":{"q1":{"results":[{"id":"x","match":false,"score":0.7,"caption":"Mollie Holdings","properties":{"topics":["role.pep"]}}],"total":{"value":1}}}});
        let sources = vec![doc(KVK, "/api/v1/basisprofielen", kvk_profile()), doc("opensanctions", "/match", hit)];
        let v = evaluate(&input, &sources, &BTreeMap::new(), now());
        assert_eq!(v["verdict"]["decision"], "review", "{v:#}");
        assert_eq!(status(&v, "sanctions"), "warn");
        assert_eq!(v["status"], "completed");
        assert!(v["verdict"]["reasons"][0].as_str().unwrap().contains("PEP-linked candidate 'Mollie Holdings'"), "{v:#}");
    }
    #[test]
    fn a_failed_sanctions_step_leaves_the_check_unknown_and_the_run_partial() {
        let input = given(json!({"name":"Mollie B.V.","country":"NL","screening_level":"Standard","registration":"30204462"}));
        let sources = vec![doc(KVK, "/api/v1/basisprofielen", kvk_profile())];
        let failures = BTreeMap::from([("opensanctions /match".to_owned(), json!({"code":"source_unavailable"}))]);
        let v = evaluate(&input, &sources, &failures, now());
        assert_eq!(status(&v, "sanctions"), "unknown");
        assert_eq!(v["status"], "partial");
        assert_eq!(v["verdict"]["decision"], "review", "{v:#}");
        assert!(v["verdict"]["reasons"].as_array().unwrap().iter().any(|r| r.as_str().unwrap().contains("Sanctions screening was unavailable")));
        assert!(v["limitations"].as_array().unwrap().iter().any(|l| l.as_str().unwrap().contains("marked 'unknown'")));
    }
    #[test]
    fn gb_enhanced_counts_officers_and_controllers() {
        let input = given(json!({"name":"Monzo Bank Limited","country":"GB","screening_level":"Enhanced","registration":"09446231"}));
        let sources = vec![
            doc(CH, "/company", json!({"company_name":"MONZO BANK LIMITED","company_number":"09446231","company_status":"active","type":"ltd","date_of_creation":"2015-02-18",
                "registered_office_address":{"address_line_1":"Broadwalk House","address_line_2":"5 Appold Street","locality":"London","postal_code":"EC2A 2AG"},"sic_codes":["64191"]})),
            doc(CH, "/company/{company_number}/officers", json!({"active_count":12,"resigned_count":20,"items":[{"name":"A","officer_role":"director"}]})),
            doc(CH, "/company/{company_number}/persons-with-significant-control", json!({"active_count":1,"ceased_count":0,"items":[{"name":"Monzo Group Holdings Ltd"}]})),
            doc(SIRENIC, "/v1/eu/entreprise/GB/{company_number}/insolvabilite", json!({"pays":"GB","company_number":"09446231","aucune_procedure":true,"statuts":[],"cas":[]})),
            no_candidates(),
        ];
        let v = evaluate(&input, &sources, &BTreeMap::new(), now());
        assert_eq!(v["verdict"]["decision"], "pass", "{v:#}");
        let checks = v["verdict"]["checks"].as_array().unwrap();
        let officers = checks.iter().find(|c| c["id"] == "officers").unwrap();
        assert_eq!(officers["status"], "pass");
        assert!(officers["detail"].as_str().unwrap().starts_with("12 active officers"), "{officers}");
        let controllers = checks.iter().find(|c| c["id"] == "controllers").unwrap();
        assert_eq!(controllers["status"], "pass");
        assert!(controllers["detail"].as_str().unwrap().starts_with("1 person with significant control"), "{controllers}");
        assert_eq!(status(&v, "insolvency"), "pass");
        assert!(!checks.iter().any(|c| c["status"] == "not_selected"), "{checks:?}");
        let address = v["verdict"]["identity"].as_array().unwrap().iter().find(|l| l["id"] == "registered_address").unwrap();
        assert_eq!(address["value"], "Broadwalk House, 5 Appold Street, London, EC2A 2AG");
        assert_eq!(address["source"], "Companies House");
    }
    #[test]
    fn standard_level_shows_the_enhanced_checks_as_not_selected() {
        let input = given(json!({"name":"Monzo Bank Limited","country":"GB","screening_level":"Standard","registration":"09446231","domain":"monzo.com"}));
        let sources = vec![
            doc(CH, "/company", json!({"company_name":"MONZO BANK LIMITED","company_number":"09446231","company_status":"active"})),
            doc("domain-intelligence", "/domain/{domain}", json!({"data":{"domain":"monzo.com","registered_at":"2015-01-01T00:00:00Z"}})),
            no_candidates(),
        ];
        let v = evaluate(&input, &sources, &BTreeMap::new(), now());
        assert_eq!(v["verdict"]["decision"], "pass", "{v:#}");
        for id in ["officers", "controllers", "insolvency", "mail_dns"] {
            assert_eq!(status(&v, id), "not_selected", "{id}");
        }
        assert_eq!(status(&v, "domain"), "pass");
        assert!(!has_check(&v, "filings") && !has_check(&v, "alerts"));
        assert_eq!(v["status"], "completed");
    }
    #[test]
    fn a_valid_iban_passes_and_keeps_the_account_holder_gap() {
        let input = given(json!({"name":"Mollie B.V.","country":"NL","screening_level":"Standard","registration":"30204462","iban":"NL91 ABNA 0417 1643 00"}));
        let sources = vec![
            doc(KVK, "/api/v1/basisprofielen", kvk_profile()),
            doc("iban-check", "/iban/{iban}", json!({"data":{"iban":"NL91ABNA0417164300","valid":true,"bank":{"name":"ABN AMRO Bank","bic":"ABNANL2A"}}})),
            no_candidates(),
        ];
        let v = evaluate(&input, &sources, &BTreeMap::new(), now());
        assert_eq!(v["verdict"]["decision"], "pass", "{v:#}");
        let iban = v["verdict"]["checks"].as_array().unwrap().iter().find(|c| c["id"] == "iban").unwrap();
        assert_eq!(iban["status"], "pass");
        assert!(iban["detail"].as_str().unwrap().contains("ABN AMRO Bank"));
        assert_eq!(v["verdict"]["subject"]["iban_masked"], "NL91 •••• 4300");
        let gaps: Vec<&str> = v["verdict"]["coverage_gaps"].as_array().unwrap().iter().map(|g| g.as_str().unwrap()).collect();
        assert_eq!(gaps[2], "The bank account holder was not verified; IBAN checks cover structure and bank only.");
        assert_eq!(gaps.len(), 5);
    }
    #[test]
    fn a_name_search_without_a_profile_is_a_review_and_fr_enhanced_reads_sirenic() {
        let input = given(json!({"name":"Danone","country":"FR","screening_level":"Enhanced"}));
        let sources = vec![doc(SIRENIC, "/v1/recherche", json!({"total_results":3,"resultats":[{"siren":"1"},{"siren":"2"},{"siren":"3"}]})), no_candidates()];
        let v = evaluate(&input, &sources, &BTreeMap::new(), now());
        assert_eq!(v["verdict"]["decision"], "review", "{v:#}");
        assert_eq!(status(&v, "identity_resolved"), "warn");
        assert!(!has_check(&v, "name_match") && !has_check(&v, "company_status"));
        assert_eq!(status(&v, "filings"), "unknown");
        assert_eq!(status(&v, "alerts"), "unknown");
        let input = given(json!({"name":"Danone","country":"FR","screening_level":"Enhanced","registration":"552032534"}));
        let sources = vec![
            doc(SIRENIC, "/v1/entreprise/{siren}", json!({"siren":"552032534","denomination":"DANONE","nature_juridique":"5599","activite_principale":"70.10Z","date_creation":"1955-01-01","etat_administratif":"actif","siege":{"adresse":"59 RUE LA FAYETTE 75009 PARIS"}})),
            doc(SIRENIC, "/v1/entreprise/{siren}/finances", json!({"siren":"552032534","nombre_exercices":8,"exercices":[{}]})),
            doc(SIRENIC, "/v1/entreprise/{siren}/alertes", json!({"siren":"552032534","procedures_collectives":[],"radiations":[],"ventes_cessions":[]})),
            no_candidates(),
        ];
        let v = evaluate(&input, &sources, &BTreeMap::new(), now());
        assert_eq!(v["verdict"]["decision"], "pass", "{v:#}");
        assert_eq!(status(&v, "filings"), "pass");
        assert_eq!(status(&v, "alerts"), "pass");
        assert_eq!(v["verdict"]["identity"][0]["source"], "Sirenic");
    }
    #[test]
    fn workflow_input_is_normalised() {
        let input = given(json!({"name":" Mollie B.V. ","country":"nl","screening_level":"enhanced","registration":"3020 4462","domain":"https://Mollie.com/","vat_country":"NL","vat_number":"815839091b01","iban":"nl91 abna 0417 1643 00"}));
        assert_eq!(input.country, "NL");
        assert!(input.enhanced());
        assert_eq!(input.registration.as_deref(), Some("30204462"));
        assert_eq!(input.domain.as_deref(), Some("mollie.com"));
        assert_eq!(input.vat().as_deref(), Some("NL815839091B01"));
        assert_eq!(input.iban.as_deref(), Some("NL91ABNA0417164300"));
        assert_eq!(Input::from_workflow(&BTreeMap::new()).screening_level, "Standard");
    }
}
