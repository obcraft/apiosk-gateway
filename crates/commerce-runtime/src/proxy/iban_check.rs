//! First-party IBAN check: structure, ISO 7064 mod-97 check digits, SEPA
//! membership and the bank behind a Dutch bank code. Computed here, without an
//! upstream call, and served through the same billed path as a cached answer.
//! It does NOT confirm that an account exists or who holds it.
use serde_json::{json, Value};

pub const SOURCE: &str = "iban-check";

/// IBAN lengths per country (SWIFT IBAN registry).
fn length(country: &str) -> Option<usize> {
    Some(match country {
        "NO" => 15, "BE" => 16, "DK" | "FI" | "FO" | "GL" | "NL" => 18, "MK" | "SI" => 19,
        "AT" | "BA" | "EE" | "KZ" | "LT" | "LU" | "XK" => 20, "CH" | "HR" | "LI" | "LV" => 21,
        "BG" | "BH" | "CR" | "DE" | "GB" | "GE" | "IE" | "ME" | "RS" | "VA" => 22, "GI" | "IL" | "AE" | "TL" => 23,
        "AD" | "CZ" | "ES" | "MD" | "PK" | "RO" | "SA" | "SE" | "SK" | "TN" | "VG" => 24, "PT" | "ST" => 25, "IS" | "TR" => 26,
        "FR" | "GR" | "IT" | "MC" | "MR" | "SM" => 27, "AL" | "AZ" | "CY" | "DO" | "GT" | "HU" | "LB" | "PL" => 28,
        "BR" | "EG" | "PS" | "QA" | "UA" => 29, "JO" | "KW" | "MU" => 30, "MT" | "SC" => 31, "LC" => 32, "RU" => 33,
        _ => return None,
    })
}

const SEPA: &[&str] = &["AD", "AT", "BE", "BG", "CH", "CY", "CZ", "DE", "DK", "EE", "ES", "FI", "FR", "GB", "GI", "GR", "HR", "HU", "IE", "IS", "IT", "LI", "LT", "LU", "LV", "MC", "MT", "NL", "NO", "PL", "PT", "RO", "SE", "SI", "SK", "SM", "VA"];

/// Dutch bank codes (IBAN positions 5–8) with their BIC.
const NL_BANKS: &[(&str, &str, &str)] = &[
    ("ABNA", "ABN AMRO Bank", "ABNANL2A"),
    ("ADYB", "Adyen", "ADYBNL2A"),
    ("ASNB", "ASN Bank", "ASNBNL21"),
    ("BNGH", "BNG Bank", "BNGHNL2G"),
    ("BUNQ", "bunq", "BUNQNL2A"),
    ("COBA", "Commerzbank", "COBANL2X"),
    ("DEUT", "Deutsche Bank Nederland", "DEUTNL2A"),
    ("FVLB", "Van Lanschot Kempen", "FVLBNL22"),
    ("HAND", "Svenska Handelsbanken", "HANDNL2A"),
    ("INGB", "ING Bank", "INGBNL2A"),
    ("ISBK", "Isbank", "ISBKNL2A"),
    ("KNAB", "Knab", "KNABNL2H"),
    ("NWAB", "Nederlandse Waterschapsbank", "NWABNL2G"),
    ("RABO", "Rabobank", "RABONL2U"),
    ("RBRB", "RegioBank", "RBRBNL21"),
    ("REVO", "Revolut Bank", "REVONL22"),
    ("SNSB", "SNS Bank", "SNSBNL2A"),
    ("TRIO", "Triodos Bank", "TRIONL2U"),
];

fn mod97(iban: &str) -> bool {
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

/// A decoded IBAN → the check result, or an input error.
pub fn check(raw: &str) -> Result<Value, String> {
    let iban: String = raw.chars().filter(|c| !c.is_whitespace() && *c != '-').collect::<String>().to_ascii_uppercase();
    if iban.len() < 5 || iban.len() > 34 || !iban.is_ascii() || !iban.bytes().all(|b| b.is_ascii_alphanumeric())
        || !iban.as_bytes()[..2].iter().all(u8::is_ascii_uppercase) || !iban.as_bytes()[2..4].iter().all(u8::is_ascii_digit)
    {
        return Err("Use an IBAN: two letters, two check digits and up to 30 letters or digits".into());
    }
    let country = &iban[..2];
    let expected = length(country);
    let length_ok = expected == Some(iban.len());
    let checksum_ok = length_ok && mod97(&iban);
    let bank_code = (country == "NL" && length_ok).then(|| iban[4..8].to_owned());
    let bank = bank_code.as_deref().and_then(|code| NL_BANKS.iter().find(|(c, _, _)| *c == code)).map(|(_, name, bic)| json!({"name":name,"bic":bic}));
    let nl_shape = country != "NL" || (length_ok && iban[4..8].bytes().all(|b| b.is_ascii_uppercase()) && iban[8..].bytes().all(|b| b.is_ascii_digit()));
    Ok(json!({
        "data": {
            "iban": iban, "valid": length_ok && checksum_ok && nl_shape, "country_code": country, "check_digits": &iban[2..4],
            "bank_code": bank_code, "bank": bank, "sepa": SEPA.contains(&country),
            "checks": {"country_known": expected.is_some(), "length": length_ok, "checksum": checksum_ok},
        },
        "notice": "Structure and check digits only. Does not confirm that the account exists or who holds it (that needs a Verification of Payee / IBAN-Name Check).",
        "coverage": {"complete": true, "scope": "IBAN structure, ISO 7064 mod-97 check digits, SEPA membership and Dutch bank-code directory"},
        "source_url": "https://www.swift.com/standards/data-standards/iban-international-bank-account-number",
    }))
}

/// The billed answer for `GET /iban/{iban}` on the iban-check source.
pub fn answer(method: &str, path: &str) -> Result<Value, String> {
    let segment = path.strip_prefix("/iban/").filter(|s| !s.is_empty() && !s.contains('/')).ok_or("Unsupported IBAN check route")?;
    if method != "GET" {
        return Err("Use GET for the IBAN check".into());
    }
    let decoded = url::form_urlencoded::parse(format!("x={segment}").as_bytes()).next().map(|(_, v)| v.into_owned()).unwrap_or_default();
    check(&decoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_structure_checksum_and_dutch_bank() {
        let ok = answer("GET", "/iban/NL91ABNA0417164300").unwrap();
        assert_eq!(ok["data"]["valid"], true);
        assert_eq!(ok["data"]["bank"]["name"], "ABN AMRO Bank");
        assert_eq!(ok["data"]["bank"]["bic"], "ABNANL2A");
        assert_eq!(ok["data"]["sepa"], true);
        let spaced = answer("GET", "/iban/NL91%20ABNA%200417%201643%2000").unwrap();
        assert_eq!(spaced["data"]["iban"], "NL91ABNA0417164300");
        let wrong = answer("GET", "/iban/NL92ABNA0417164300").unwrap();
        assert_eq!(wrong["data"]["valid"], false);
        assert_eq!(wrong["data"]["checks"]["checksum"], false);
        let uk = answer("GET", "/iban/GB82WEST12345698765432").unwrap();
        assert_eq!(uk["data"]["valid"], true);
        assert_eq!(uk["data"]["bank"], Value::Null);
        let short = answer("GET", "/iban/NL91ABNA04171643").unwrap();
        assert_eq!(short["data"]["checks"]["length"], false);
        assert!(answer("GET", "/iban/not-an-iban!").is_err());
        assert!(answer("POST", "/iban/NL91ABNA0417164300").is_err());
        assert!(answer("GET", "/other/NL91ABNA0417164300").is_err());
        assert!(answer("GET", "/iban/NL91ABNA0417164300/x").is_err());
        assert!(check("ÑL91ABNA0417164300").is_err());
    }
}
