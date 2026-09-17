//! Readable company names in rendered reports. Saved facts and identifiers stay exact.
use serde_json::Value;
use std::collections::BTreeSet;

pub fn company_name(name: &str) -> String {
    if name != name.to_uppercase() { return name.to_owned(); }
    let mut output = String::new();
    let mut word = String::new();
    fn flush(word: &mut String, output: &mut String) {
        if word.is_empty() { return; }
        if matches!(word.as_str(), "BV" | "NV" | "VOF" | "CV" | "LLC" | "LLP" | "PLC" | "AG" | "SA" | "SAS" | "SE" | "ASA" | "IT" | "ICT" | "AI" | "IBM" | "ING" | "ABN" | "AMRO" | "ASML" | "KPN" | "TNO") {
            output.push_str(word);
        } else if word == "GMBH" {
            output.push_str("GmbH");
        } else {
            let mut chars = word.chars();
            if let Some(first) = chars.next() { output.push(first); }
            output.push_str(&chars.as_str().to_lowercase());
        }
        word.clear();
    }
    for c in name.chars() {
        if c.is_alphabetic() || (!word.is_empty() && (c.is_alphanumeric() || matches!(c, '\'' | '’'))) { word.push(c); }
        else { flush(&mut word, &mut output); output.push(c); }
    }
    flush(&mut word, &mut output);
    output
}

pub fn name_field(key: &str) -> bool {
    let compact: String = key.chars().filter(|c| c.is_ascii_alphabetic()).flat_map(char::to_lowercase).collect();
    ["companyname", "legalname", "legalnamename", "registeredname", "businessname", "tradename", "statutairenaam", "handelsnaam", "handelsnamennaam", "nameofreportingentityorothermeansofidentification", "entitycurrentlegalorregisteredname", "denomination", "raisonsociale"].iter().any(|suffix| compact.ends_with(suffix))
}

pub struct CompanyNames(Vec<String>);
impl CompanyNames {
    pub fn from_document(document: &Value) -> Self {
        fn walk(value: &Value, path: &str, depth: usize, names: &mut BTreeSet<String>) {
            if depth > 20 { return; }
            match value {
                Value::Array(items) => for item in items { walk(item, path, depth + 1, names); },
                Value::Object(map) => {
                    let field = map.get("type").or_else(|| map.get("factType")).or_else(|| map.get("key")).or_else(|| map.get("field")).or_else(|| map.get("pointer")).and_then(Value::as_str);
                    if field.is_some_and(name_field) {
                        if let Some(name) = map.get("value").and_then(Value::as_str) { names.insert(name.to_owned()); }
                    }
                    let company = ["kvkNummer", "kvk_number", "company_number", "companyNumber", "lei", "legalName", "statutaireNaam", "siren", "siret"].iter().any(|key| map.contains_key(*key)) || ["/supplier", "/report_subject", "/candidates"].iter().any(|suffix| path.ends_with(suffix));
                    for (key, child) in map {
                        let next = format!("{path}/{key}");
                        if name_field(&next) || (company && matches!(key.as_str(), "name" | "naam" | "label")) {
                            if let Some(name) = child.as_str() { names.insert(name.to_owned()); }
                        }
                        walk(child, &next, depth + 1, names);
                    }
                }
                _ => (),
            }
        }
        let mut names = BTreeSet::new();
        walk(document, "", 0, &mut names);
        let mut names: Vec<_> = names.into_iter().filter(|name| !name.is_empty() && company_name(name) != *name).collect();
        names.sort_by_key(|name| std::cmp::Reverse(name.len()));
        Self(names)
    }
    pub fn text(&self, text: &str) -> String {
        let mut output = String::new();
        let mut offset = 0;
        while offset < text.len() {
            let rest = &text[offset..];
            let boundary = |c: char| c.is_alphanumeric() || matches!(c, '_' | '/' | '@');
            let found = self.0.iter().find(|name| rest.starts_with(name.as_str())
                && !text[..offset].chars().next_back().is_some_and(|c| boundary(c) || c == '.')
                && !rest[name.len()..].chars().next().is_some_and(boundary)
                && !(rest[name.len()..].starts_with('.') && rest[name.len()+1..].chars().next().is_some_and(char::is_alphanumeric)));
            if let Some(name) = found { output.push_str(&company_name(name)); offset += name.len(); }
            else { let c = rest.chars().next().unwrap(); output.push(c); offset += c.len_utf8(); }
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn report_display_preserves_source_evidence_and_identifier_tokens() {
        let source = json!({"supplier":{"name":"ORION"},"data":{"statutaireNaam":"ORION BEHEER BV","kvkNummer":"01234567","legalName":{"name":"ASML HOLDING NV"}}});
        let original = source.clone();
        let names = CompanyNames::from_document(&source);
        assert_eq!(names.text("ORION BEHEER BV and ASML HOLDING NV. ORION is registered."), "Orion Beheer BV and ASML Holding NV. Orion is registered.");
        assert_eq!(names.text("https://ORION.EXAMPLE/ORION mail@ORION.EXAMPLE ORION_ID 01234567"), "https://ORION.EXAMPLE/ORION mail@ORION.EXAMPLE ORION_ID 01234567");
        assert_eq!(source, original);
        for name in ["iDEAL BV", "eBay", "ASML", "IBM", "3M"] { assert_eq!(company_name(name), name); }
    }
}
