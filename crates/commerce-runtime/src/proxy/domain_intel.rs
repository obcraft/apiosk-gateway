//! Domain intelligence for supplier checks: one bounded upstream call per
//! purchase. RDAP registration data from the authoritative registry (no
//! redirects, fixed hosts) and typed DNS records through Google Public DNS.
use super::Prepared;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use url::Url;

pub const SOURCE: &str = "domain-intelligence";
/// Authoritative RDAP bases from the IANA bootstrap (checked 2026-09-15).
/// Registries of .de/.eu/.be publish no registration date and are not offered.
const RDAP: &[(&str, &str)] = &[
    ("com", "https://rdap.verisign.com/com/v1/"),
    ("net", "https://rdap.verisign.com/net/v1/"),
    ("org", "https://rdap.publicinterestregistry.org/rdap/"),
    ("nl", "https://rdap.sidn.nl/"),
    ("uk", "https://rdap.nominet.uk/uk/"),
    ("fr", "https://rdap.nic.fr/"),
    ("app", "https://pubapi.registry.google/rdap/"),
    ("dev", "https://pubapi.registry.google/rdap/"),
];
const UK_SECOND_LEVEL: &[&str] = &["co", "org", "me", "ltd", "plc", "net", "sch", "ac", "gov"];
const DNS_TYPES: &[(&str, u64)] = &[
    ("A", 1), ("AAAA", 28), ("CNAME", 5), ("MX", 15),
    ("NS", 2), ("TXT", 16), ("CAA", 257), ("SOA", 6),
];

/// A DNS question names an exact owner, not the registrable root. Underscore
/// labels are necessary for records such as _dmarc and selector._domainkey.
fn dns_name(input: &str) -> Result<String, String> {
    let name = input.trim().trim_end_matches('.').to_ascii_lowercase();
    let labels: Vec<_> = name.split('.').collect();
    if name.len() > 253 || labels.len() < 2
        || labels.iter().any(|label| label.is_empty() || label.len() > 63
            || label.starts_with('-') || label.ends_with('-')
            || !label.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
        || !labels.last().is_some_and(|label| label.len() >= 2 && label.bytes().all(|byte| byte.is_ascii_lowercase()))
    {
        return Err("Use an exact DNS name such as www.example.nl or _dmarc.example.nl".into());
    }
    Ok(name)
}

pub fn handles(source: &str) -> bool {
    source == SOURCE
}

/// The registrable name (example.co.uk, not www.example.co.uk), lowercase.
pub fn registrable(input: &str) -> Result<String, String> {
    let d = input.trim().trim_end_matches('.').to_ascii_lowercase();
    let labels: Vec<&str> = d.split('.').collect();
    let valid = d.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|l| !l.is_empty() && l.len() <= 63 && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') && !l.starts_with('-') && !l.ends_with('-'))
        && labels.last().is_some_and(|t| t.len() >= 2 && t.bytes().all(|b| b.is_ascii_lowercase()));
    if !valid {
        return Err("Use a domain name such as example.nl".into());
    }
    let uk = labels.len() >= 3 && labels[labels.len() - 1] == "uk" && UK_SECOND_LEVEL.contains(&labels[labels.len() - 2]);
    let keep = if uk { 3 } else { 2 };
    Ok(labels[labels.len() - keep..].join("."))
}

pub fn prepare(method: &str, original: &str) -> Result<Option<Prepared>, String> {
    let url = Url::parse(original).map_err(|_| "Invalid domain source URL")?;
    if method != "GET"
        || url.scheme() != "https"
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query().is_some_and(|q| !q.is_empty())
    {
        return Err("Unsupported domain intelligence route".into());
    }
    let (route, raw) = match (url.host_str(), url.path().strip_prefix("/domain/"), url.path().strip_prefix("/dns/")) {
        (Some("rdap.org"), Some(rest), _) => ("rdap", rest),
        (Some("dns.google"), _, Some(rest)) => ("dns", rest),
        _ => return Err("Unsupported domain intelligence route".into()),
    };
    // Path segments are already percent-decoded by callers; any remaining
    // separator or escape is rejected by the strict label check.
    let (domain, record_type) = if route == "dns" && raw.contains('/') {
        let (name, kind) = raw.split_once('/').unwrap();
        let kind = kind.to_ascii_uppercase();
        if !DNS_TYPES.iter().any(|(supported, _)| *supported == kind) {
            return Err("Unsupported DNS record type; choose A, AAAA, CNAME, MX, NS, TXT, CAA or SOA".into());
        }
        (dns_name(name)?, Some(kind))
    } else {
        (registrable(raw)?, None)
    };
    let mut params = BTreeMap::new();
    params.insert("domain".to_owned(), domain.clone());
    if let Some(kind) = &record_type { params.insert("record_type".into(), kind.clone()); }
    let upstream = if route == "rdap" {
        let tld = domain.rsplit('.').next().unwrap_or_default();
        let base = RDAP
            .iter()
            .find(|(t, _)| *t == tld)
            .map(|(_, b)| *b)
            .ok_or_else(|| format!("No RDAP registration data is published for .{tld} domains"))?;
        format!("{base}domain/{domain}")
    } else {
        let mut u = Url::parse("https://dns.google/resolve").unwrap();
        u.query_pairs_mut().append_pair("name", &domain).append_pair("type", record_type.as_deref().unwrap_or("MX"));
        u.to_string()
    };
    let source = if record_type.is_some() { "domain:dns-records".into() } else { format!("domain:{route}") };
    Ok(Some(Prepared { url: upstream, json_body: None, source, params }))
}

fn dns_question(data: &Value, domain: &str, kind: u64) -> Result<u64, String> {
    let status = data["Status"].as_u64().ok_or("DNS response has no status")?;
    if status != 0 && status != 3 { return Err("DNS resolution failed".into()); }
    let questions = data["Question"].as_array().ok_or("DNS response has no question")?;
    if questions.len() != 1
        || questions[0]["name"].as_str().map(|name| name.trim_end_matches('.').to_ascii_lowercase()).as_deref() != Some(domain)
        || questions[0]["type"].as_u64() != Some(kind)
    {
        return Err("DNS question name or record type mismatch".into());
    }
    Ok(status)
}

fn event(data: &Value, action: &str) -> Option<String> {
    data["events"].as_array()?.iter().find(|e| e["eventAction"] == action).and_then(|e| e["eventDate"].as_str()).map(str::to_owned)
}

pub fn normalize(p: &Prepared, bytes: &[u8]) -> Result<Value, String> {
    let data: Value = serde_json::from_slice(bytes).map_err(|_| "Domain source returned invalid JSON".to_string())?;
    let domain = p.params.get("domain").ok_or("Missing domain")?;
    let mut result = match p.source.as_str() {
        "domain:rdap" => {
            let name = data["ldhName"].as_str().ok_or("RDAP response has no domain name")?.trim_end_matches('.').to_ascii_lowercase();
            if data["objectClassName"] != "domain" || name != *domain {
                return Err("RDAP domain identity mismatch".into());
            }
            let registered = event(&data, "registration");
            if let Some(date) = &registered {
                chrono::DateTime::parse_from_rfc3339(date).map_err(|_| "Invalid RDAP registration date")?;
            }
            let registrar = data["entities"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|e| e["roles"].as_array().is_some_and(|r| r.iter().any(|v| v == "registrar")))
                .and_then(|e| e["vcardArray"][1].as_array())
                .and_then(|card| card.iter().find(|f| f[0] == "fn"))
                .and_then(|f| f[3].as_str())
                .map(str::to_owned);
            let nameservers: Vec<String> = data["nameservers"].as_array().into_iter().flatten().filter_map(|n| n["ldhName"].as_str()).map(|n| n.to_ascii_lowercase()).collect();
            json!({"data":{"domain":domain,"registered_at":registered,"last_changed_at":event(&data,"last changed"),"expires_at":event(&data,"expiration"),
                    "registrar":registrar,"statuses":data["status"].as_array().cloned().unwrap_or_default(),"nameservers":nameservers,
                    "rdap_server":Url::parse(&p.url).ok().and_then(|u| u.host_str().map(str::to_owned))},
                "notice":"Registry RDAP data at request time. Registrant identity is usually redacted; a registration date shows when the current registration started, not who controls the domain.",
                "coverage":{"complete":registered.is_some(),"scope":"Authoritative registry RDAP record for one domain"}})
        }
        "domain:dns-records" => {
            let kind = p.params.get("record_type").ok_or("Missing DNS record type")?;
            let code = DNS_TYPES.iter().find(|(name, _)| *name == kind).map(|(_, code)| *code).ok_or("Unsupported DNS record type")?;
            let status = dns_question(&data, domain, code)?;
            if data.get("Answer").is_some_and(|answers| !answers.is_array()) {
                return Err("DNS response has invalid answer records".into());
            }
            let mut records = Vec::new();
            for answer in data["Answer"].as_array().into_iter().flatten() {
                let answer_code = answer["type"].as_u64().ok_or("DNS answer has no record type")?;
                // CNAMEs preserve the alias chain for an A/AAAA/MX/etc query.
                // Incidental DNSSEC records are not this purchased capability.
                if answer_code != code && answer_code != 5 { continue; }
                let name = dns_name(answer["name"].as_str().ok_or("DNS answer has no owner name")?)?;
                let value = answer["data"].as_str().ok_or("DNS answer has no record value")?;
                let ttl = match answer.get("TTL") {
                    None | Some(Value::Null) => None,
                    Some(ttl) => Some(ttl.as_u64().ok_or("DNS answer has invalid TTL")?),
                };
                let answer_type = DNS_TYPES.iter().find(|(_, number)| *number == answer_code).unwrap().0;
                records.push(json!({"name":name,"type":answer_type,"type_code":answer_code,"ttl":ttl,"data":value}));
            }
            if status == 3 && records.iter().any(|record| record["type_code"] == code) {
                return Err("DNS response contradicts its NXDOMAIN status".into());
            }
            // RFC 2308 section 2.1: NXDOMAIN can describe the final CNAME
            // target while the queried alias itself demonstrably exists.
            let exists = status == 0 || records.iter().any(|record|
                record["type_code"] == 5 && record["name"].as_str() == Some(domain.as_str()));
            json!({"data":{"domain":domain,"record_type":kind,"response_code":status,"exists":exists,"records":records,"dnssec_validated":data["AD"] == true},
                "notice":"Public DNS lookup at request time (Google Public DNS). Records describe this exact name and record type, not every record in a DNS zone, company identity, domain ownership, or service availability. Empty records do not establish that a service is unavailable.",
                "coverage":{"complete":data["TC"] == false,"scope":format!("{kind} records for the exact DNS name {domain}, including returned CNAME aliases")}})
        }
        "domain:dns" => {
            let status = dns_question(&data, domain, 15)?;
            // Keep every MX record, including RFC 7505 null MX ("0 ."), so
            // callers can distinguish an explicit no-mail declaration from
            // an absent MX record. The indicator only detects a mail server.
            let mx: Vec<String> = data["Answer"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|a| a["type"] == 15)
                .filter_map(|a| a["data"].as_str())
                .map(str::to_owned)
                .collect();
            let has_mail_server = mx.iter().any(|record| record.split_whitespace().nth(1).is_some_and(|host| host != "."));
            json!({"data":{"domain":domain,"exists":status == 0,"accepts_mail":has_mail_server,"mx":mx,"dnssec_validated":data["AD"] == true},
                "notice":"Public DNS MX lookup at request time (Google Public DNS). DNS shows mail routing, not ownership.",
                "coverage":{"complete":true,"scope":"MX records for one domain"}})
        }
        _ => return Err("Unknown domain route".into()),
    };
    result["source_url"] = json!(p.url);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    // include_bytes! resolves next to this file, so the tests also run when the
    // gateway crate includes this adapter through #[path].
    fn fixture(name: &str) -> Vec<u8> {
        match name {
            "rdap-nl.json" => include_bytes!("fixtures/domain-intel/rdap-nl.json").to_vec(),
            "rdap-com.json" => include_bytes!("fixtures/domain-intel/rdap-com.json").to_vec(),
            "dns-mx.json" => include_bytes!("fixtures/domain-intel/dns-mx.json").to_vec(),
            "dns-nxdomain.json" => include_bytes!("fixtures/domain-intel/dns-nxdomain.json").to_vec(),
            "dns-null-mx.json" => include_bytes!("fixtures/domain-intel/dns-null-mx.json").to_vec(),
            _ => unreachable!(),
        }
    }
    #[test]
    fn routes_rdap_to_the_authoritative_registry_without_redirects() {
        let p = prepare("GET", "https://rdap.org/domain/WWW.Mollie.NL").unwrap().unwrap();
        assert_eq!(p.url, "https://rdap.sidn.nl/domain/mollie.nl");
        let uk = prepare("GET", "https://rdap.org/domain/shop.tesco.co.uk").unwrap().unwrap();
        assert_eq!(uk.url, "https://rdap.nominet.uk/uk/domain/tesco.co.uk");
        assert!(prepare("GET", "https://rdap.org/domain/denic.de").unwrap_err().contains(".de"));
        assert!(prepare("GET", "https://rdap.org/domain/evil.nl%2F..%2Fx").is_err());
        assert!(prepare("GET", "https://rdap.org/domain/a/b.nl").is_err());
        assert!(prepare("POST", "https://rdap.org/domain/mollie.nl").is_err());
        assert!(prepare("GET", "https://attacker.example/domain/mollie.nl").is_err());
        assert!(prepare("GET", "https://rdap.org/domain/mollie.nl?x=1").is_err());
    }
    #[test]
    fn normalizes_real_registry_records() {
        let p = prepare("GET", "https://rdap.org/domain/mollie.nl").unwrap().unwrap();
        let v = normalize(&p, &fixture("rdap-nl.json")).unwrap();
        assert_eq!(v["data"]["registered_at"], "2000-05-23T14:06:31Z");
        assert_eq!(v["data"]["registrar"], "team.blue nl B.V.");
        let com = prepare("GET", "https://rdap.org/domain/mollie.com").unwrap().unwrap();
        let v = normalize(&com, &fixture("rdap-com.json")).unwrap();
        assert_eq!(v["data"]["registered_at"], "2003-08-26T18:10:03Z");
        assert_eq!(v["data"]["expires_at"], "2027-08-26T18:10:03Z");
        let wrong = prepare("GET", "https://rdap.org/domain/other.com").unwrap().unwrap();
        assert!(normalize(&wrong, &fixture("rdap-com.json")).is_err());
    }
    #[test]
    fn reads_mail_servers_and_null_mx() {
        let p = prepare("GET", "https://dns.google/dns/mollie.com").unwrap().unwrap();
        assert_eq!(p.url, "https://dns.google/resolve?name=mollie.com&type=MX");
        let mail = normalize(&p, &fixture("dns-mx.json")).unwrap();
        assert_eq!(mail["data"]["accepts_mail"], true);
        assert_eq!(mail["data"]["mx"].as_array().unwrap().len(), 5);
        assert_eq!(mail["data"]["mx"][0], "10 aspmx2.googlemail.com.");
        let nx = prepare("GET", "https://dns.google/dns/dit-bestaat-echt-niet-apiosk-9876.nl").unwrap().unwrap();
        let v = normalize(&nx, &fixture("dns-nxdomain.json")).unwrap();
        assert_eq!(v["data"]["exists"], false);
        assert_eq!(v["data"]["mx"], json!([]));
        let null = prepare("GET", "https://dns.google/dns/example.com").unwrap().unwrap();
        let no_mail = normalize(&null, &fixture("dns-null-mx.json")).unwrap();
        assert_eq!(no_mail["data"]["accepts_mail"], false);
        assert_eq!(no_mail["data"]["mx"], json!(["0 ."]));
        assert_eq!(no_mail["data"]["exists"], true);
    }
    #[test]
    fn empty_mx_and_mixed_records_preserve_the_public_source_evidence() {
        let prepared = prepare("GET", "https://dns.google/dns/example.com").unwrap().unwrap();
        let mut raw: Value = serde_json::from_slice(&fixture("dns-null-mx.json")).unwrap();
        raw["Answer"] = json!([]);
        let empty = normalize(&prepared, &serde_json::to_vec(&raw).unwrap()).unwrap();
        assert_eq!(empty["data"]["exists"], true);
        assert_eq!(empty["data"]["accepts_mail"], false);
        assert_eq!(empty["data"]["mx"], json!([]));

        raw["Answer"] = json!([
            {"type":15,"data":"0 ."},
            {"type":15,"data":"10 mail.example.com."},
            {"type":5,"data":"alias.example.com."}
        ]);
        let mixed = normalize(&prepared, &serde_json::to_vec(&raw).unwrap()).unwrap();
        assert_eq!(mixed["data"]["accepts_mail"], true);
        assert_eq!(mixed["data"]["mx"], json!(["0 .", "10 mail.example.com."]));
    }
}
