//! The browser's half of a trace: one beacon per question with durations
//! between fixed marks and a few counts, logged as one `client` line. The
//! allowed names come from `perf/phases.json`; there is no free text and no
//! money in a beacon, and nothing is stored.

use super::{emit, Origin, Surface, Trace};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::{collections::BTreeMap, sync::OnceLock};
use uuid::Uuid;

/// The largest beacon body the route accepts.
pub const MAX_BYTES: usize = 4096;

fn dictionary() -> &'static Value {
    static PHASES: OnceLock<Value> = OnceLock::new();
    PHASES.get_or_init(|| serde_json::from_str(include_str!("../../perf/phases.json")).expect("perf/phases.json is valid JSON"))
}
fn listed(section: &Value, name: &str) -> bool {
    match section {
        Value::Array(items) => items.iter().any(|item| item == name),
        // Browser-measured phases only: derived ones have no `from` mark.
        Value::Object(map) => map.get(name).is_some_and(|entry| entry.get("from").is_some()),
        _ => false,
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Beacon {
    v: u8,
    trace: Uuid,
    #[serde(default)]
    task: Option<Uuid>,
    #[serde(default)]
    quote: Option<Uuid>,
    device: String,
    viewport_bucket: u16,
    net: String,
    #[serde(default)]
    phases: BTreeMap<String, u64>,
    #[serde(default)]
    counts: BTreeMap<String, u64>,
    outcome: String,
}

impl Beacon {
    /// Every name must be in the dictionary and every value in range, or
    /// the whole beacon is refused: a partial line would read as complete.
    fn check(&self) -> Result<(), &'static str> {
        let phases = dictionary();
        let beacon = &phases["beacon"];
        let limit = |key: &str| beacon[key].as_u64().unwrap_or(0);
        if Some(u64::from(self.v)) != beacon["version"].as_u64() {
            return Err("unsupported beacon version");
        }
        if self.trace.get_version_num() != 4 {
            return Err("invalid trace");
        }
        if !listed(&beacon["devices"], &self.device) || !listed(&beacon["nets"], &self.net) || !listed(&beacon["outcomes"], &self.outcome) {
            return Err("unknown device, network or outcome");
        }
        if !beacon["viewport_buckets"].as_array().is_some_and(|buckets| buckets.iter().any(|bucket| bucket.as_u64() == Some(u64::from(self.viewport_bucket)))) {
            return Err("unknown viewport bucket");
        }
        for (name, ms) in &self.phases {
            if !(listed(&phases["journey_phases"], name) || listed(&phases["client_phases"], name)) || *ms > limit("max_phase_ms") {
                return Err("unknown or out-of-range phase");
            }
        }
        for (name, count) in &self.counts {
            if !listed(&phases["client_counts"], name) || *count > limit("max_count") {
                return Err("unknown or out-of-range count");
            }
        }
        Ok(())
    }

    /// Logs the beacon under its own trace id, as one `client` line.
    pub fn record(self) -> Result<(), &'static str> {
        self.check()?;
        let trace = Trace::new(self.trace, Origin::Client);
        trace.surface.set(Surface::ALL, Surface::App);
        let mut fields = Map::new();
        fields.insert("device".into(), json!(self.device));
        fields.insert("viewport_bucket".into(), json!(self.viewport_bucket));
        fields.insert("net".into(), json!(self.net));
        fields.insert("outcome".into(), json!(self.outcome));
        fields.insert("phases".into(), json!(self.phases));
        fields.insert("counts".into(), json!(self.counts));
        emit("client", Some(&trace), self.task, self.quote, fields);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::capture;

    fn beacon(extra: Value) -> Result<Beacon, serde_json::Error> {
        let mut body = json!({"v":1,"trace":Uuid::new_v4(),"device":"mobile","viewport_bucket":390,"net":"4g",
            "phases":{"find_to_price":2400,"ui.answer_render":16},"counts":{"sse_open":1},"outcome":"answered"});
        if let (Some(body), Value::Object(extra)) = (body.as_object_mut(), extra) {
            body.extend(extra);
        }
        serde_json::from_value(body)
    }

    #[test]
    fn a_valid_beacon_becomes_one_client_line() {
        let lines = capture::Lines::default();
        let _guard = lines.install();
        beacon(json!({"task":Uuid::new_v4()})).unwrap().record().unwrap();
        let telemetry = lines.telemetry();
        assert_eq!(telemetry.len(), 1);
        assert_eq!(telemetry[0]["kind"], "client");
        assert_eq!(telemetry[0]["origin"], "client");
        assert_eq!(telemetry[0]["phases"]["find_to_price"], 2400);
        assert!(telemetry[0]["task"].is_string());
    }

    #[test]
    fn free_text_unknown_names_and_money_are_refused() {
        assert!(beacon(json!({"question":"Quokka Canary 4f1c B.V."})).is_err(), "unknown fields are rejected");
        assert!(beacon(json!({"charged_micro_usd":54348})).is_err());
        for bad in [
            json!({"phases":{"Quokka Canary":1}}),
            json!({"phases":{"find_to_price":600_001}}),
            json!({"counts":{"reloads":1001}}),
            json!({"device":"Quokka"}),
            json!({"viewport_bucket":391}),
            json!({"outcome":"Quokka"}),
            json!({"v":2}),
            json!({"trace":"00000000-0000-1000-8000-000000000000"}),
            json!({"phases":{"provider_wait":5}}),
        ] {
            assert!(beacon(bad.clone()).unwrap().record().is_err(), "{bad}");
        }
    }
}
