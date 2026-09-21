//! Bounded, source-derived bindings for UK supplier checks. No network or LLM.
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub const DIRECTOR_LIMIT: usize = 5;

/// Sirenic's UK roster contains active officers only. Never screen a truncated
/// roster, silently choose five of a larger board, or include a secretary.
pub fn directors(body: &Value, key: &str) -> Option<Value> {
    let rows = body.get("dirigeants")?.as_array()?;
    let complete =
        body["tronque"] == false && body["nombre_actifs"].as_u64() == Some(rows.len() as u64);
    let directors: Vec<_> = rows
        .iter()
        .filter(|r| {
            matches!(
                r["fonction"].as_str(),
                Some(
                    "director"
                        | "corporate-director"
                        | "nominee-director"
                        | "corporate-nominee-director"
                )
            )
        })
        .collect();
    let supported = rows.iter().all(|r| {
        matches!(
            r["fonction"].as_str(),
            Some(
                "director"
                    | "corporate-director"
                    | "nominee-director"
                    | "corporate-nominee-director"
                    | "secretary"
                    | "corporate-secretary"
                    | "nominee-secretary"
                    | "corporate-nominee-secretary"
            )
        )
    });
    let complete = complete
        && supported
        && directors.len() <= DIRECTOR_LIMIT
        && directors.iter().all(|r| {
            r["nom"]
                .as_str()
                .is_some_and(|n| !n.trim().is_empty() && n.len() <= 200)
        });
    if key == "coverage" {
        return Some(
            json!({"complete":complete,"active_directors":directors.len(),
            "screening_limit":DIRECTOR_LIMIT,"scope":"Active directors; OpenSanctions watchlist matching. Not a background check or declaration that a person is clean."}),
        );
    }
    if !complete {
        return None;
    }
    let (index, field) = key.split_once('.')?;
    let row = directors.get(index.parse::<usize>().ok()?)?;
    match field {
        "name" => Some(row["nom"].clone()),
        "query" => {
            let corporate = row["fonction"].as_str()?.starts_with("corporate-");
            let mut properties = json!({"name":[row["nom"]]});
            if !corporate {
                if let Some(year) = self::directors(body, &format!("{index}.birth_year")) {
                    properties["birthDate"] = json!([year]);
                }
            }
            Some(
                json!({"q1":{"schema":if corporate {"Company"} else {"Person"},"properties":properties}}),
            )
        }
        "birth_year" => {
            // The source publishes month/year, never infer a complete DOB.
            let birth = &row["naissance"];
            let year = birth
                .get("year")
                .or_else(|| birth.get("annee"))
                .and_then(Value::as_u64)
                .or_else(|| {
                    birth.as_str().and_then(|s| {
                        s.split(['-', '/'])
                            .find_map(|p| (p.len() == 4).then(|| p.parse::<u64>().ok()).flatten())
                    })
                })?;
            (1850..=2100)
                .contains(&year)
                .then(|| json!(year.to_string()))
        }
        _ => None,
    }
}

/// GB-prefixed candidates in retrieved search snippets. A candidate is NOT an
/// identity assertion or validation. Multiple candidates remain unresolved.
pub fn vat_candidates(body: &Value, key: &str) -> Option<Value> {
    let rows = body.get("results")?.as_array()?;
    if rows.len() > 10 {
        return None;
    }
    let mut evidence = Vec::new();
    let mut numbers = BTreeSet::new();
    for row in rows {
        let url = row.get("url")?.as_str()?;
        if !url.starts_with("https://") {
            continue;
        }
        let mut texts = Vec::new();
        for field in ["title", "text", "content"] {
            if let Some(s) = row[field].as_str() {
                texts.push(s);
            }
        }
        if let Some(highlights) = row["highlights"].as_array() {
            texts.extend(highlights.iter().filter_map(Value::as_str));
        }
        for text in texts {
            if text.len() > 100_000 {
                continue;
            }
            let upper = text.to_ascii_uppercase();
            for (start, _) in upper.match_indices("GB") {
                if start > 0 && upper.as_bytes()[start - 1].is_ascii_alphanumeric() {
                    continue;
                }
                let rest = &upper[start + 2..];
                let digits: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || matches!(c, ' ' | '-'))
                    .filter(char::is_ascii_digit)
                    .collect();
                if digits.len() != 9 {
                    continue;
                }
                let end = rest
                    .char_indices()
                    .filter(|(_, c)| c.is_ascii_digit())
                    .nth(8)?
                    .0
                    + 1;
                if rest
                    .as_bytes()
                    .get(end)
                    .is_some_and(u8::is_ascii_alphanumeric)
                {
                    continue;
                }
                let number = format!("GB{digits}");
                numbers.insert(number.clone());
                if !evidence
                    .iter()
                    .any(|e: &Value| e["number"] == number && e["url"] == url)
                {
                    evidence
                        .push(json!({"number":number,"url":url,"status":"unverified_candidate"}));
                }
            }
        }
    }
    match key {
        "number" if numbers.len() == 1 => numbers.into_iter().next().map(Value::String),
        "evidence" => Some(
            json!({"candidates":evidence,"distinct_candidates":numbers.len(),
            "notice":"Published search candidates only; not proof of entity ownership or VAT registration. No candidate is not proof of no registration."}),
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roster_is_complete_active_and_bounded() {
        let mut r = json!({"tronque":false,"nombre_actifs":3,"dirigeants":[
            {"nom":"Secretary Ltd","fonction":"corporate-secretary"},
            {"nom":"Example One","fonction":"director","naissance":{"year":1961,"month":5}},
            {"nom":"Example Two","fonction":"director","naissance":null}]});
        assert_eq!(directors(&r, "0.name"), Some(json!("Example One")));
        assert_eq!(directors(&r, "0.birth_year"), Some(json!("1961")));
        assert_eq!(
            directors(&r, "0.query").unwrap()["q1"]["properties"]["birthDate"],
            json!(["1961"])
        );
        assert_eq!(directors(&r, "0.query").unwrap()["q1"]["schema"], "Person");
        assert_eq!(directors(&r, "1.birth_year"), None);
        assert_eq!(directors(&r, "2.name"), None);
        assert_eq!(directors(&r, "coverage").unwrap()["active_directors"], 2);
        r["dirigeants"][2]["fonction"] = json!("corporate-director");
        assert_eq!(directors(&r, "1.query").unwrap()["q1"]["schema"], "Company");
        assert!(directors(&r, "1.query").unwrap()["q1"]["properties"]
            .get("birthDate")
            .is_none());
        r["tronque"] = json!(true);
        assert_eq!(directors(&r, "0.name"), None);
        r["tronque"] = json!(false);
        r["nombre_actifs"] = json!(4);
        assert_eq!(directors(&r, "0.name"), None);
        r["nombre_actifs"] = json!(6);
        r["dirigeants"] = Value::Array(vec![json!({"nom":"A","fonction":"director"}); 6]);
        assert_eq!(directors(&r, "0.name"), None);
    }
    #[test]
    fn candidate_identity_is_not_guessed_and_ambiguity_stops_validation() {
        let r =
            json!({"results":[{"url":"https://example.test/a","title":"UK VAT GB 471 1446 61"}]});
        assert_eq!(vat_candidates(&r, "number"), Some(json!("GB471144661")));
        let mut r = r;
        r["results"][0]["highlights"] = json!(["Other business GB220430231"]);
        assert_eq!(vat_candidates(&r, "number"), None);
        assert_eq!(
            vat_candidates(&r, "evidence").unwrap()["distinct_candidates"],
            2
        );
        for text in [
            "GB1234567890",
            "GB123456789ABC",
            "XGB471144661",
            "GB12345",
            "IE4276970QH",
        ] {
            assert_eq!(
                vat_candidates(
                    &json!({"results":[{"url":"https://example.test","title":text}]}),
                    "number"
                ),
                None
            );
        }
    }
}
