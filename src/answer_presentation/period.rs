//! Periods and dates as sources state them. A year label is never turned into
//! calendar dates (fiscal years exist), and an unrecognised form stays
//! displayable but is never used as an axis or comparison basis.
use crate::answer_blocks::{DatePrecision, EventTime, Granularity};
use chrono::{DateTime, NaiveDate, NaiveDateTime, SecondsFormat};
use serde_json::Value;

/// Server-derived fields of a period qualifier.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ParsedPeriod {
    pub(super) text: String,
    pub(super) key: Option<String>,
    pub(super) granularity: Granularity,
    pub(super) start: Option<NaiveDate>,
    pub(super) end: Option<NaiveDate>,
}

/// Source text of a scalar: strings as stated, numbers and booleans as JSON.
pub(super) fn scalar_text(value: &Value) -> String {
    value.as_str().map_or_else(|| value.to_string(), str::to_owned)
}

fn digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

fn year(text: &str) -> Option<&str> {
    (text.len() == 4 && digits(text)).then_some(text)
}

fn month(year: &str, month: &str) -> Option<String> {
    (month.len() == 2 && digits(month) && (1..=12).contains(&month.parse::<u8>().ok()?)).then(|| format!("{year}-{month}"))
}

/// An exact calendar date: `YYYY-MM-DD`, or the date an RFC 3339 instant states.
fn exact_date(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .ok()
        .or_else(|| DateTime::parse_from_rfc3339(text).ok().map(|instant| instant.date_naive()))
        .or_else(|| NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S").ok().map(|local| local.date()))
}

/// Key and granularity of one period label, or None when it is not a known form.
fn period_key(text: &str) -> Option<(String, Granularity)> {
    if let Some(year) = year(text) {
        return Some((year.to_owned(), Granularity::Year));
    }
    if let Some(date) = exact_date(text) {
        return Some((date.format("%Y-%m-%d").to_string(), Granularity::Day));
    }
    let (head, tail) = (text.get(..4)?, text.get(4..)?);
    let year = year(head)?;
    if let Some(value) = tail.strip_prefix('-') {
        if let Some(quarter) = value.strip_prefix('Q').filter(|q| matches!(*q, "1" | "2" | "3" | "4")) {
            return Some((format!("{year}-Q{quarter}"), Granularity::Quarter));
        }
        if let Some(half) = value.strip_prefix('H').filter(|h| matches!(*h, "1" | "2")) {
            return Some((format!("{year}-H{half}"), Granularity::HalfYear));
        }
        return month(year, value).map(|key| (key, Granularity::Month));
    }
    // CBS StatLine period codes: 2024JJ00 (year), 2024KW01 (quarter), 2024MM03 (month).
    match (tail.get(..2)?, tail.get(2..)?) {
        ("JJ", "00") => Some((year.to_owned(), Granularity::Year)),
        ("KW", quarter) if matches!(quarter, "01" | "02" | "03" | "04") => {
            Some((format!("{year}-Q{}", &quarter[1..]), Granularity::Quarter))
        }
        ("MM", value) => month(year, value).map(|key| (key, Granularity::Month)),
        _ => None,
    }
}

/// Parse a non-null period value, with an optional explicit end date field.
pub(super) fn parse_period(value: &Value, end: Option<&Value>) -> ParsedPeriod {
    let text = match value.as_u64() {
        Some(number) if (1000..=9999).contains(&number) => number.to_string(),
        _ => scalar_text(value),
    };
    if let Some(end) = end.filter(|end| !end.is_null()) {
        let range = exact_date(&text).zip(end.as_str().and_then(exact_date)).filter(|(start, end)| start <= end);
        return match range {
            Some((start, end)) => ParsedPeriod {
                key: Some(format!("{start}/{end}")),
                granularity: Granularity::Range,
                start: Some(start),
                end: Some(end),
                text,
            },
            None => ParsedPeriod { text, key: None, granularity: Granularity::Unparsed, start: None, end: None },
        };
    }
    match period_key(&text) {
        Some((key, Granularity::Day)) => {
            let date = exact_date(&text);
            ParsedPeriod { text, key: Some(key), granularity: Granularity::Day, start: date, end: date }
        }
        Some((key, granularity)) => ParsedPeriod { text, key: Some(key), granularity, start: None, end: None },
        None => ParsedPeriod { text, key: None, granularity: Granularity::Unparsed, start: None, end: None },
    }
}

/// A date or date-time exactly as precise as its source value. An offset is
/// kept only when the value states one; no default time zone is assumed.
pub(super) fn event_time(value: &Value) -> EventTime {
    let unknown = EventTime { precision: DatePrecision::Unknown, sort_key: None, utc_offset: None, time_zone: None };
    let text = match value.as_u64() {
        Some(number) if (1000..=9999).contains(&number) => number.to_string(),
        _ => match value.as_str() {
            Some(text) => text.trim().to_owned(),
            None => return unknown,
        },
    };
    let known = |precision, sort_key: String, utc_offset| EventTime { precision, sort_key: Some(sort_key), utc_offset, time_zone: None };
    if let Some(year) = year(&text) {
        return known(DatePrecision::Year, year.to_owned(), None);
    }
    if let Some(key) = text.get(..4).zip(text.get(4..)).and_then(|(y, rest)| month(year(y)?, rest.strip_prefix('-')?)) {
        return known(DatePrecision::Month, key, None);
    }
    if let Ok(date) = NaiveDate::parse_from_str(&text, "%Y-%m-%d") {
        return known(DatePrecision::Day, date.format("%Y-%m-%d").to_string(), None);
    }
    if let Ok(instant) = DateTime::parse_from_rfc3339(&text) {
        return known(
            DatePrecision::DateTime,
            instant.to_utc().to_rfc3339_opts(SecondsFormat::AutoSi, true),
            Some(instant.offset().to_string()),
        );
    }
    if let Ok(local) = NaiveDateTime::parse_from_str(&text, "%Y-%m-%dT%H:%M:%S") {
        return known(DatePrecision::DateTime, local.format("%Y-%m-%dT%H:%M:%S").to_string(), None);
    }
    unknown
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn key(value: Value) -> (Option<String>, Granularity) {
        let parsed = parse_period(&value, None);
        (parsed.key, parsed.granularity)
    }

    #[test]
    fn source_period_forms_get_one_comparable_key() {
        assert_eq!(key(json!(2024)), (Some("2024".into()), Granularity::Year));
        assert_eq!(key(json!("2024")), (Some("2024".into()), Granularity::Year));
        assert_eq!(key(json!("2024JJ00")), (Some("2024".into()), Granularity::Year));
        assert_eq!(key(json!("2024-03")), (Some("2024-03".into()), Granularity::Month));
        assert_eq!(key(json!("2024MM03")), (Some("2024-03".into()), Granularity::Month));
        assert_eq!(key(json!("2024-Q1")), (Some("2024-Q1".into()), Granularity::Quarter));
        assert_eq!(key(json!("2024KW02")), (Some("2024-Q2".into()), Granularity::Quarter));
        assert_eq!(key(json!("2024-H2")), (Some("2024-H2".into()), Granularity::HalfYear));
        assert_eq!(key(json!("2024-03-31")), (Some("2024-03-31".into()), Granularity::Day));
        assert_eq!(key(json!("2024-03-31T23:30:00+01:00")), (Some("2024-03-31".into()), Granularity::Day));
    }

    #[test]
    fn a_year_is_not_turned_into_calendar_dates_and_unknown_forms_stay_unparsed() {
        let year = parse_period(&json!(2024), None);
        assert_eq!((year.start, year.end), (None, None));
        assert_eq!(parse_period(&json!("2024-13"), None).granularity, Granularity::Unparsed);
        assert_eq!(parse_period(&json!("boekjaar"), None).key, None);
        assert_eq!(parse_period(&json!(24.5), None).granularity, Granularity::Unparsed);
        assert_eq!(parse_period(&json!("2024KW05"), None).granularity, Granularity::Unparsed);
    }

    #[test]
    fn a_range_needs_two_stated_dates_in_order() {
        let range = parse_period(&json!("2023-07-01"), Some(&json!("2024-06-30")));
        assert_eq!(range.key.as_deref(), Some("2023-07-01/2024-06-30"));
        assert_eq!(range.granularity, Granularity::Range);
        assert_eq!(range.start, NaiveDate::from_ymd_opt(2023, 7, 1));
        assert_eq!(parse_period(&json!("2024"), Some(&json!("2024-06-30"))).granularity, Granularity::Unparsed);
        assert_eq!(parse_period(&json!("2024-07-01"), Some(&json!("2024-06-30"))).granularity, Granularity::Unparsed);
    }

    #[test]
    fn event_times_keep_their_stated_precision_and_offset() {
        assert_eq!(event_time(&json!("2024")).precision, DatePrecision::Year);
        assert_eq!(event_time(&json!("2024-05")).sort_key.as_deref(), Some("2024-05"));
        assert_eq!(event_time(&json!("2024-05-02")).precision, DatePrecision::Day);
        let instant = event_time(&json!("2024-05-02T09:00:00+02:00"));
        assert_eq!(instant.precision, DatePrecision::DateTime);
        assert_eq!(instant.sort_key.as_deref(), Some("2024-05-02T07:00:00Z"));
        assert_eq!(instant.utc_offset.as_deref(), Some("+02:00"));
        let floating = event_time(&json!("2024-05-02T09:00:00"));
        assert_eq!((floating.precision, floating.utc_offset), (DatePrecision::DateTime, None));
        assert_eq!(event_time(&json!("binnenkort")).sort_key, None);
        assert_eq!(event_time(&Value::Null).precision, DatePrecision::Unknown);
    }
}
