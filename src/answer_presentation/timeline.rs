//! Timeline (#51): events on the dates sources state, at the precision they
//! state. An unknown date is never estimated; it goes to an "unknown date" group.
use super::period::event_time;
use super::resolve::{ensure, fail, valid_relative, Checked, Issue, Rows, Scope};
use crate::answer_blocks::{BlockIssue, Coverage, Fact, TimelineBlock, TimelineEvent, MAX_TIMELINE_EVENTS};

/// A link a person can open: https without embedded credentials.
fn safe_link(link: Option<Fact>, path: &str) -> Checked<Option<Fact>> {
    let Some(link) = link.filter(|link| !link.value.is_null()) else { return Ok(None) };
    let url = link.value.as_str().and_then(|text| url::Url::parse(text).ok());
    let safe = url.is_some_and(|url| url.scheme() == "https" && url.username().is_empty() && url.password().is_none());
    ensure(safe, BlockIssue::Schema, path)?;
    Ok(Some(link))
}

/// An IANA zone name stated by the source, e.g. "Europe/Amsterdam".
fn zone_name(fact: Option<&Fact>) -> Option<String> {
    let zone = fact?.value.as_str()?.trim();
    let plausible = zone.len() <= 64
        && (zone == "UTC" || zone.contains('/'))
        && zone.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '+' | '-'));
    plausible.then(|| zone.to_owned())
}

fn place(mut event: TimelineEvent) -> TimelineEvent {
    event.when = event.date.as_ref().filter(|date| !date.value.is_null()).map(|date| {
        let mut when = event_time(&date.value);
        when.time_zone = zone_name(event.time_zone.as_ref());
        when
    });
    event
}

pub(super) fn timeline(block: TimelineBlock, scope: &mut Scope) -> Checked<(TimelineBlock, Option<Coverage>)> {
    let mut events = Vec::new();
    let coverage = match &block.collection {
        Some(collection) => {
            let rows = scope.collection(collection, "collection")?;
            let date_kind = block.date_kind.ok_or_else(|| Issue { code: BlockIssue::Schema, path: "date_kind".into() })?;
            let date_pointer = block.date_pointer.as_deref().filter(|p| valid_relative(p));
            let date_pointer = date_pointer.ok_or_else(|| Issue { code: BlockIssue::InvalidPointer, path: "date_pointer".into() })?;
            for (pointer, name) in [(&block.label_pointer, "label_pointer"), (&block.link_pointer, "link_pointer")] {
                ensure(pointer.as_deref().is_none_or(valid_relative), BlockIssue::InvalidPointer, name)?;
            }
            for (position, (index, record)) in rows.records.iter().enumerate() {
                let path = format!("events/{position}");
                let cell = |pointer: Option<&str>, field: &str| pointer.map_or(Ok(None), |p| Rows::cell(record, p, &format!("{path}/{field}")));
                events.push(place(TimelineEvent {
                    id: format!("row-{index}"),
                    label: None,
                    label_fact: cell(block.label_pointer.as_deref(), "label_fact")?,
                    date_kind,
                    date: cell(Some(date_pointer), "date")?,
                    time_zone: None,
                    link: safe_link(cell(block.link_pointer.as_deref(), "link")?, &format!("{path}/link"))?,
                    facts: Vec::new(),
                    row_index: Some(*index),
                    when: None,
                }));
            }
            Some(rows.coverage)
        }
        None => {
            ensure(!block.events.is_empty(), BlockIssue::Schema, "events")?;
            ensure(block.events.len() <= MAX_TIMELINE_EVENTS, BlockIssue::TooManyItems, "events")?;
            for (index, event) in block.events.iter().enumerate() {
                let path = format!("events/{index}");
                scope.item_id(&event.id, &path)?;
                if let Some(label) = &event.label {
                    scope.label(label, &format!("{path}/label"))?;
                }
                let label_fact = scope.optional_fact(event.label_fact.as_ref(), &format!("{path}/label_fact"))?;
                if event.label.is_none() && label_fact.is_none() {
                    return fail(BlockIssue::Schema, format!("{path}/label"));
                }
                let link = scope.optional_fact(event.link.as_ref(), &format!("{path}/link"))?;
                events.push(place(TimelineEvent {
                    label_fact,
                    date: scope.optional_fact(event.date.as_ref(), &format!("{path}/date"))?,
                    time_zone: scope.optional_fact(event.time_zone.as_ref(), &format!("{path}/time_zone"))?,
                    link: safe_link(link, &format!("{path}/link"))?,
                    facts: event.facts.iter().enumerate().map(|(i, item)| scope.labeled(item, &format!("{path}/facts/{i}"))).collect::<Checked<Vec<_>>>()?,
                    row_index: None,
                    when: None,
                    ..event.clone()
                }));
            }
            None
        }
    };
    // Dated events in order; undated ones keep their order after them.
    events.sort_by(|a, b| {
        let key = |event: &TimelineEvent| event.when.as_ref().and_then(|when| when.sort_key.clone());
        match (key(a), key(b)) {
            (Some(a), Some(b)) => a.cmp(&b),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    });
    Ok((TimelineBlock { events, ..block }, coverage))
}
