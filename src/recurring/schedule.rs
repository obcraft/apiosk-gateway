use chrono::{Datelike, NaiveDate, NaiveTime};
use serde::{Deserialize, Serialize};

fn weekly() -> String {
    "weekly".into()
}
fn one() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    pub start: NaiveDate,
    #[serde(default)]
    pub end: Option<NaiveDate>,
    pub time: NaiveTime,
    pub timezone: String,
    #[serde(default)]
    pub weekdays: Vec<i32>,
    #[serde(default = "weekly")]
    pub frequency: String,
    #[serde(default = "one")]
    pub interval: u32,
    #[serde(default)]
    pub count: Option<usize>,
}

impl Schedule {
    pub fn validate(&self, per_run: bool) -> anyhow::Result<()> {
        anyhow::ensure!(
            matches!(self.frequency.as_str(), "daily" | "weekly" | "monthly"),
            "Choose daily, weekly or monthly."
        );
        anyhow::ensure!(
            (1..=12).contains(&self.interval),
            "Invalid schedule interval."
        );
        anyhow::ensure!(
            !self.timezone.is_empty() && self.timezone.len() <= 100,
            "Choose a timezone."
        );
        anyhow::ensure!(
            self.weekdays.len() <= 7 && self.weekdays.iter().all(|d| (1..=7).contains(d)),
            "Choose valid weekdays."
        );
        anyhow::ensure!(
            self.frequency != "weekly" || !self.weekdays.is_empty(),
            "Choose weekdays."
        );
        if per_run {
            anyhow::ensure!(
                self.end.is_none() && self.count.is_none(),
                "Ongoing schedules cannot have an end date or run count."
            );
        } else {
            anyhow::ensure!(
                self.end.is_some() != self.count.is_some(),
                "Choose an end date or a number of runs."
            );
            anyhow::ensure!(
                self.end
                    .is_none_or(|e| e >= self.start && (e - self.start).num_days() <= 36_600),
                "Invalid schedule period."
            );
            anyhow::ensure!(
                self.count.is_none_or(|n| (1..=366).contains(&n)),
                "Choose between 1 and 366 runs."
            );
        }
        Ok(())
    }

    /// Calendar rules are anchored to the original start, never the previous
    /// clamped date. Conversion to UTC happens in PostgreSQL with the IANA zone.
    pub fn dates(&self, after: NaiveDate, limit: usize) -> anyhow::Result<Vec<NaiveDate>> {
        let mut cursor = self.start;
        let mut occurrences = 0usize;
        let mut out = Vec::new();
        let monday =
            self.start - chrono::Duration::days(self.start.weekday().num_days_from_monday().into());
        for _ in 0..146_400 {
            if self.end.is_some_and(|end| cursor > end)
                || self.count.is_some_and(|n| occurrences >= n)
            {
                break;
            }
            let elapsed = (cursor - self.start).num_days();
            let matches = match self.frequency.as_str() {
                "daily" => elapsed % i64::from(self.interval) == 0,
                "weekly" => {
                    ((cursor - monday).num_days() / 7) % i64::from(self.interval) == 0
                        && self
                            .weekdays
                            .contains(&(cursor.weekday().number_from_monday() as i32))
                }
                "monthly" => {
                    let months = (cursor.year() - self.start.year()) * 12 + cursor.month() as i32
                        - self.start.month() as i32;
                    let next = if cursor.month() == 12 {
                        NaiveDate::from_ymd_opt(cursor.year() + 1, 1, 1)
                    } else {
                        NaiveDate::from_ymd_opt(cursor.year(), cursor.month() + 1, 1)
                    };
                    let last = next
                        .and_then(|d| d.pred_opt())
                        .ok_or_else(|| anyhow::anyhow!("Schedule out of range."))?
                        .day();
                    months % self.interval as i32 == 0 && cursor.day() == self.start.day().min(last)
                }
                _ => anyhow::bail!("Invalid frequency."),
            };
            if matches {
                occurrences += 1;
                if cursor >= after {
                    out.push(cursor);
                }
                if out.len() >= limit {
                    return Ok(out);
                }
            }
            cursor = cursor
                .succ_opt()
                .ok_or_else(|| anyhow::anyhow!("Schedule out of range."))?;
        }
        anyhow::ensure!(!out.is_empty(), "No future runs in this period.");
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn schedule(value: serde_json::Value) -> Schedule {
        serde_json::from_value(value).unwrap()
    }
    #[test]
    fn monthly_keeps_the_original_day_after_short_months() {
        let s = schedule(
            serde_json::json!({"start":"2027-01-31","count":4,"time":"09:00:00","timezone":"Europe/Amsterdam","frequency":"monthly"}),
        );
        s.validate(false).unwrap();
        assert_eq!(
            s.dates(s.start, 366)
                .unwrap()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["2027-01-31", "2027-02-28", "2027-03-31", "2027-04-30"]
        );
    }
    #[test]
    fn count_and_interval_do_not_restart_when_queue_is_extended() {
        let s = schedule(
            serde_json::json!({"start":"2026-10-09","count":5,"time":"09:00:00","timezone":"UTC","frequency":"daily","interval":2}),
        );
        assert_eq!(
            s.dates("2026-10-13".parse().unwrap(), 366)
                .unwrap()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["2026-10-13", "2026-10-15", "2026-10-17"]
        );
    }
    #[test]
    fn rejects_ambiguous_or_unbounded_prepaid_schedule() {
        let mut s = schedule(
            serde_json::json!({"start":"2026-10-09","time":"09:00:00","timezone":"UTC","weekdays":[1]}),
        );
        assert!(s.validate(false).is_err());
        s.validate(true).unwrap();
        s.count = Some(5);
        s.end = Some("2026-11-01".parse().unwrap());
        assert!(s.validate(false).is_err());
        assert!(s.validate(true).is_err());
    }
}
