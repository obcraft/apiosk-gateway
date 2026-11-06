//! How many stars a catalogue card wears, and what earns them.
//!
//! The catalogue asks the same things of every source before it is safe to
//! call blind, and those requirements are written down across the migrations
//! that onboard one: a named input schema per priced endpoint (100, 103), a
//! `required` list on it so `/v1/ask` knows what to ask for (099), a sentence
//! saying what the endpoint does, and a mark to wear on its card (105). A
//! reader browsing Explore cannot see any of that, so the cards all look
//! equally trustworthy — including the one that will 400 on its first paid
//! call because nobody wrote down which field is mandatory.
//!
//! Three stars is that checklist, met. Nothing here is an opinion about the
//! provider: every point is a fact about what they published, countable in
//! SQL, and the summary string names the ones they missed.
//!
//! ---------------------------------------------------------------------------
//! Measurement can only take stars away, never add them
//! ---------------------------------------------------------------------------
//!
//! A listing Apiosk has never proxied has no record, and `v1_routes/flow.rs`
//! is explicit about what that must mean: absence of evidence is never held
//! against a provider, because it says more about our coverage than about
//! them. So an unmeasured source is rated on its documentation alone.
//!
//! A source we HAVE measured, and measured failing, is a different matter: a
//! listing under [`crate::db::QUARANTINE_FLOOR`] is already held back
//! from selection, and a card that still wore three stars while its endpoints
//! answered 4 calls in 10 would be advertising a broken thing on the strength
//! of its paperwork. So a measured record caps the rating, and the cap is read
//! off the RECENT window for the reason `provider_stats` gives: ninety days
//! describes a provider, two weeks judges it.
//!
//! ---------------------------------------------------------------------------
//! No stars is a real answer
//! ---------------------------------------------------------------------------
//!
//! A card with no priced, active endpoints has nothing to be judged on. One
//! star would read as "we looked and it is poor"; the truth is that there is
//! nothing to look at. [`rate`] returns `None` and the card draws nothing —
//! the same shape `provider_stats` uses for an unmeasured latency: absent,
//! never zero.

use crate::db::QUARANTINE_FLOOR;

/// Measured recent success at or above this keeps a card at full stars.
const RELIABLE: f64 = 0.9;

/// What a card publishes, counted over its priced, active endpoints, plus
/// whatever the gateway has measured. Every field is a count of something
/// somebody wrote or something that happened; none of it is judgement.
#[derive(Debug, Clone, Default)]
pub struct CardEvidence {
    /// The denominator: priced, active endpoints behind this card.
    pub paid_endpoints: i64,
    /// How many of them name their inputs (a non-empty `properties`).
    pub with_inputs: i64,
    /// How many declare which of those inputs are mandatory.
    pub with_required: i64,
    /// How many say, in their own sentence, what they do.
    pub with_description: i64,
    /// Whether the card has a mark of its own to wear.
    pub has_mark: bool,
    /// Recent measured success rate, or `None` when nobody has called it
    /// lately. Never defaulted to zero — see the module note.
    pub recent_success_rate: Option<f64>,
}

/// A rating and the sentence that explains it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CardQuality {
    /// 1, 2 or 3.
    pub stars: u8,
    /// Why, in one line, for the title attribute on the card.
    pub summary: String,
}

/// Rate one card. `None` when there is nothing to judge.
pub fn rate(evidence: &CardEvidence) -> Option<CardQuality> {
    let total = evidence.paid_endpoints;
    if total <= 0 {
        return None;
    }

    let all = |count: i64| count >= total;
    let mut missing: Vec<&str> = Vec::new();
    let mut points = 0u8;

    if all(evidence.with_inputs) {
        points += 1;
    } else {
        missing.push("not every endpoint names its inputs");
    }
    if all(evidence.with_required) {
        points += 1;
    } else {
        missing.push("not every endpoint says which inputs are required");
    }
    if all(evidence.with_description) {
        points += 1;
    } else {
        missing.push("not every endpoint describes what it does");
    }
    if evidence.has_mark {
        points += 1;
    } else {
        missing.push("no mark on the card");
    }

    let documented = match points {
        4 => 3,
        2 | 3 => 2,
        _ => 1,
    };

    // Measurement caps, never lifts.
    let (stars, measured) = match evidence.recent_success_rate {
        Some(rate) if rate < QUARANTINE_FLOOR => (
            1,
            Some(format!("measured {} of calls answered lately", pct(rate))),
        ),
        Some(rate) if rate < RELIABLE => (
            documented.min(2),
            Some(format!("measured {} of calls answered lately", pct(rate))),
        ),
        Some(rate) => (
            documented,
            Some(format!("measured {} of calls answered lately", pct(rate))),
        ),
        None => (documented, None),
    };

    let mut summary = match stars {
        3 => "Documents every priced endpoint: inputs, which of them are required, what it does"
            .to_string(),
        _ if missing.is_empty() => "Documents every priced endpoint".to_string(),
        _ => format!("Documentation: {}", missing.join("; ")),
    };
    if let Some(measured) = measured {
        summary.push_str(". ");
        summary.push_str(&measured);
    } else {
        summary.push_str(". Not measured yet, which is never held against it");
    }

    Some(CardQuality { stars, summary })
}

fn pct(rate: f64) -> String {
    format!("{}%", (rate * 100.0).round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn perfect() -> CardEvidence {
        CardEvidence {
            paid_endpoints: 4,
            with_inputs: 4,
            with_required: 4,
            with_description: 4,
            has_mark: true,
            recent_success_rate: None,
        }
    }

    #[test]
    fn nothing_priced_is_not_rated() {
        let evidence = CardEvidence {
            paid_endpoints: 0,
            ..perfect()
        };
        assert!(rate(&evidence).is_none());
    }

    #[test]
    fn the_full_checklist_earns_three() {
        assert_eq!(rate(&perfect()).unwrap().stars, 3);
    }

    #[test]
    fn an_unmeasured_source_is_not_penalised() {
        // The whole point: a brand-new, well documented source is not held
        // below a worse-documented one that happens to have traffic.
        let fresh = perfect();
        let measured = CardEvidence {
            recent_success_rate: Some(0.99),
            ..perfect()
        };
        assert_eq!(rate(&fresh).unwrap().stars, rate(&measured).unwrap().stars);
        assert!(rate(&fresh).unwrap().summary.contains("Not measured"));
    }

    #[test]
    fn a_missing_required_list_costs_a_star() {
        let evidence = CardEvidence {
            with_required: 1,
            ..perfect()
        };
        let rated = rate(&evidence).unwrap();
        assert_eq!(rated.stars, 2);
        assert!(rated.summary.contains("required"));
    }

    #[test]
    fn thin_documentation_lands_on_one() {
        let evidence = CardEvidence {
            with_inputs: 0,
            with_required: 0,
            with_description: 0,
            has_mark: false,
            ..perfect()
        };
        assert_eq!(rate(&evidence).unwrap().stars, 1);
    }

    #[test]
    fn a_measured_failure_caps_a_well_documented_card() {
        let wobbly = CardEvidence {
            recent_success_rate: Some(0.7),
            ..perfect()
        };
        assert_eq!(rate(&wobbly).unwrap().stars, 2);

        let broken = CardEvidence {
            recent_success_rate: Some(0.2),
            ..perfect()
        };
        assert_eq!(rate(&broken).unwrap().stars, 1);
        assert!(rate(&broken).unwrap().summary.contains("20%"));
    }

    #[test]
    fn measurement_never_lifts_a_thin_card() {
        let evidence = CardEvidence {
            with_inputs: 0,
            with_required: 0,
            with_description: 0,
            has_mark: false,
            recent_success_rate: Some(1.0),
            ..perfect()
        };
        assert_eq!(rate(&evidence).unwrap().stars, 1);
    }
}
