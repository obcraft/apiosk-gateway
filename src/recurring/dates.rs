//! Date rules bind to individual saved input fields. Fixed literals in the
//! original question are preserved, even when another field is relative.
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DateRule {
    pub pointer: String,
    pub original: String,
    pub offset_seconds: i64,
    pub date_only: bool,
}

fn explicit_date(question: &str, date: NaiveDate) -> bool {
    let q = question.to_lowercase();
    for format in [
        "%Y-%m-%d",
        "%-d/%-m/%Y",
        "%d/%m/%Y",
        "%-m/%-d/%Y",
        "%m/%d/%Y",
        "%-d-%-m-%Y",
        "%d-%m-%Y",
    ] {
        if q.contains(&date.format(format).to_string()) {
            return true;
        }
    }
    let dutch = [
        "januari",
        "februari",
        "maart",
        "april",
        "mei",
        "juni",
        "juli",
        "augustus",
        "september",
        "oktober",
        "november",
        "december",
    ];
    let months = [
        date.format("%B").to_string().to_lowercase(),
        date.format("%b").to_string().to_lowercase(),
        dutch[date.month0() as usize].to_owned(),
    ];
    months.iter().any(|month| {
        q.contains(&format!("{} {}", date.day(), month))
            || q.contains(&format!("{} {}", month, date.day()))
    })
}

fn offsets(question: &str) -> Vec<i64> {
    let q = question.to_lowercase();
    let words: Vec<&str> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|v| !v.is_empty())
        .collect();
    let mut out = vec![];
    if words
        .iter()
        .any(|s| matches!(*s, "today" | "vandaag" | "now" | "nu"))
    {
        out.push(0);
    }
    if words.iter().any(|s| matches!(*s, "yesterday" | "gisteren")) {
        out.push(-86400);
    }
    for w in words.windows(3) {
        if matches!(w[0], "last" | "past" | "afgelopen") {
            if let Ok(n) = w[1].parse::<i64>() {
                let unit = match w[2] {
                    "hours" | "hour" | "uur" => 3600,
                    "days" | "day" | "dagen" | "dag" => 86400,
                    "weeks" | "week" | "weken" => 604800,
                    _ => 0,
                };
                if (1..=366).contains(&n) && unit > 0 {
                    out.extend([0, -n * unit]);
                }
            }
        }
    }
    for phrase in ["last week", "past week", "afgelopen week"] {
        if q.contains(phrase) {
            out.extend([0, -604800]);
        }
    }
    out.sort();
    out.dedup();
    out
}
pub fn infer(
    inputs: &Value,
    question: &str,
    anchor: DateTime<Utc>,
    local_date: NaiveDate,
) -> Vec<DateRule> {
    fn visit(
        value: &Value,
        path: &str,
        question: &str,
        offsets: &[i64],
        anchor: DateTime<Utc>,
        local_date: NaiveDate,
        out: &mut Vec<DateRule>,
    ) {
        match value {
            Value::Object(m) => {
                for (k, v) in m {
                    visit(
                        v,
                        &format!("{}/{}", path, k.replace('~', "~0").replace('/', "~1")),
                        question,
                        offsets,
                        anchor,
                        local_date,
                        out,
                    )
                }
            }
            Value::Array(a) => {
                for (i, v) in a.iter().enumerate() {
                    visit(
                        v,
                        &format!("{path}/{i}"),
                        question,
                        offsets,
                        anchor,
                        local_date,
                        out,
                    )
                }
            }
            Value::String(s) => {
                if s.len() < 10
                    || question.contains(s.as_str())
                    || question
                        .contains(&s[..s.char_indices().nth(10).map(|(i, _)| i).unwrap_or(s.len())])
                {
                    return;
                }
                if let Ok(date) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
                    if explicit_date(question, date) {
                        return;
                    }
                    let delta = (date - local_date).num_days() * 86400;
                    if offsets.contains(&delta) {
                        out.push(DateRule {
                            pointer: path.into(),
                            original: s.clone(),
                            offset_seconds: delta,
                            date_only: true,
                        });
                    }
                } else if let Ok(date) = DateTime::parse_from_rfc3339(s) {
                    if explicit_date(question, date.date_naive()) {
                        return;
                    }
                    let delta = (date.with_timezone(&Utc) - anchor).num_seconds();
                    if let Some(offset) = offsets
                        .iter()
                        .min_by_key(|o| (**o - delta).abs())
                        .filter(|o| (**o - delta).abs() <= 1800)
                    {
                        out.push(DateRule {
                            pointer: path.into(),
                            original: s.clone(),
                            offset_seconds: *offset,
                            date_only: false,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    let mut rules = vec![];
    visit(
        inputs,
        "",
        question,
        &offsets(question),
        anchor,
        local_date,
        &mut rules,
    );
    rules
}
pub fn materialize(
    inputs: &Value,
    rules: &[DateRule],
    scheduled: DateTime<Utc>,
    local_date: NaiveDate,
) -> anyhow::Result<Value> {
    let mut out = inputs.clone();
    for rule in rules {
        let slot = out
            .pointer_mut(&rule.pointer)
            .ok_or_else(|| anyhow::anyhow!("Date rule no longer matches an input."))?;
        anyhow::ensure!(
            slot.as_str() == Some(rule.original.as_str()),
            "Date input changed."
        );
        let value = if rule.date_only {
            (local_date + chrono::Duration::days(rule.offset_seconds / 86400))
                .format("%Y-%m-%d")
                .to_string()
        } else {
            (scheduled + chrono::Duration::seconds(rule.offset_seconds))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        };
        *slot = Value::String(value);
    }
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn mixed_fixed_dates_and_today() {
        let anchor = "2026-09-30T07:00:00Z".parse().unwrap();
        let input = json!({"today":"2026-09-30","fixed":"2024-01-01","nested":{"from":"2026-09-29T07:00:00Z","to":"2026-09-30T07:00:00Z"}});
        let rules = infer(
            &input,
            "Compare today and the last 24 hours with 2024-01-01",
            anchor,
            "2026-09-30".parse().unwrap(),
        );
        let next = materialize(
            &input,
            &rules,
            "2026-10-26T08:00:00Z".parse().unwrap(),
            "2026-10-26".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(next["today"], "2026-10-26");
        assert_eq!(next["fixed"], "2024-01-01");
        assert_eq!(next["nested"]["from"], "2026-10-25T08:00:00Z");
        assert_eq!(next["nested"]["to"], "2026-10-26T08:00:00Z");
    }
    #[test]
    fn absolute_request_never_advances() {
        let rules = infer(
            &json!({"date":"2026-09-30"}),
            "Report for 2026-09-30",
            "2026-09-30T07:00:00Z".parse().unwrap(),
            "2026-09-30".parse().unwrap(),
        );
        assert!(rules.is_empty());
    }
    #[test]
    fn explicit_date_stays_fixed_in_relative_question() {
        let rules = infer(
            &json!({"date":"2026-09-30"}),
            "Compare today with 2026-09-30",
            "2026-09-30T07:00:00Z".parse().unwrap(),
            "2026-09-30".parse().unwrap(),
        );
        assert!(rules.is_empty());
    }
}

/// All date-valued fields are reviewable, including fields whose relative
/// meaning cannot be inferred safely from the original question.
pub fn candidates(inputs: &Value) -> Vec<DateRule> {
    fn walk(value: &Value, path: String, out: &mut Vec<DateRule>) {
        match value {
            Value::Object(values) => {
                for (k, v) in values {
                    walk(
                        v,
                        format!("{}/{}", path, k.replace('~', "~0").replace('/', "~1")),
                        out,
                    )
                }
            }
            Value::Array(values) => {
                for (i, v) in values.iter().enumerate() {
                    walk(v, format!("{path}/{i}"), out)
                }
            }
            Value::String(s) => {
                let date_only = NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok();
                if date_only || DateTime::parse_from_rfc3339(s).is_ok() {
                    out.push(DateRule {
                        pointer: path,
                        original: s.clone(),
                        offset_seconds: 0,
                        date_only,
                    });
                }
            }
            _ => {}
        }
    }
    let mut out = vec![];
    walk(inputs, String::new(), &mut out);
    out
}

#[cfg(test)]
mod date_review_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn named_fixed_date_stays_fixed() {
        let input = json!({"date":"2026-09-30"});
        assert!(infer(
            &input,
            "Compare today with 30 September 2026",
            "2026-09-30T07:00:00Z".parse().unwrap(),
            "2026-09-30".parse().unwrap()
        )
        .is_empty());
        assert_eq!(candidates(&input).len(), 1);
    }
    #[test]
    fn changing_one_date_field_does_not_change_another() {
        let input = json!({"from":"2026-09-29T07:00:00Z","fixed":"2024-01-01"});
        let mut rules = candidates(&input);
        rules.retain(|rule| rule.pointer == "/from");
        rules[0].offset_seconds = -86400;
        let out = materialize(
            &input,
            &rules,
            "2026-10-26T08:00:00Z".parse().unwrap(),
            "2026-10-26".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(out["fixed"], "2024-01-01");
        assert_eq!(out["from"], "2026-10-25T08:00:00Z");
    }
}
