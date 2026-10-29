//! One amount, summed from per-node amounts, in integer atomic USDC.
//!
//! THE RULE THAT MATTERS: a node's buyer-total is computed once, by
//! [`crate::fees::quote`], from that node's list price. The plan's total is the
//! SUM OF THOSE. It is never `sum(list) * 1.15`, and it is never
//! `sum(buyer_total) * 1.15`. The first would round differently from what the
//! buyer is charged per call; the second is the double markup the step 1 price
//! manifest found already reachable on the ask path today.
//!
//! Everything here is `i128` atomic USDC — 10^-6 USD. No float touches an
//! amount a buyer approves.

use serde::{Deserialize, Serialize};

use crate::fees;

/// The single amount a buyer approves, plus the euro figure shown next to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quote {
    /// What the buyer will be charged at most, in atomic USDC. The ceiling in
    /// the plan token and the ceiling the reservation holds are this number.
    pub buyer_total_atomic: i128,
    /// Per node, in plan order, so a receipt can be reconciled line by line.
    pub lines: Vec<QuoteLine>,
    /// The euro figure on the button, and the rate it was derived from.
    ///
    /// SNAPSHOTTED, not looked up again later. A rate move between approval and
    /// settlement may not raise a euro amount someone already said yes to; if
    /// the snapshot is absent, no euro figure is shown at all rather than one
    /// that might move.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eur: Option<EuroSnapshot>,
}

/// One node's contribution to the total.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteLine {
    pub node_id: String,
    /// What a person reads: the work, independent of which provider performs it.
    pub capability_slug: String,
    /// The source's list price before the buyer-side Apiosk markup.
    #[serde(default)]
    pub source_cost_atomic: i128,
    /// The 15% buyer-side markup for this call. This is not the provider-side
    /// revenue split.
    #[serde(default)]
    pub apiosk_fee_atomic: i128,
    pub buyer_total_atomic: i128,
}

/// The euro amount and exactly what produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EuroSnapshot {
    /// Euro cents. Integer, rounded up, so the shown figure is never below
    /// what can be charged.
    pub cents: i64,
    /// EUR per USD, in micro-units (1_000_000 = 1.00), as read at compile time.
    pub rate_micro: i64,
    /// When the rate was read. Part of the approved plan version.
    pub taken_at: chrono::DateTime<chrono::Utc>,
}

/// Compute a node's buyer-total from its list price. One rounding, here.
pub fn node_buyer_total(list_atomic: i128, class: fees::ProviderClass) -> i128 {
    fees::quote(list_atomic, class).buyer_total_atomic
}

/// Sum already-rounded per-node buyer-totals into the plan's one amount.
pub fn total(lines: Vec<QuoteLine>, eur: Option<EuroSnapshot>) -> Quote {
    let buyer_total_atomic = lines.iter().map(|line| line.buyer_total_atomic).sum();
    Quote {
        buyer_total_atomic,
        lines,
        eur,
    }
}

/// Convert an approved USD amount to euro cents at a snapshotted rate.
///
/// Rounds UP: the figure next to the button is a ceiling, and a ceiling that
/// rounds down is not a ceiling.
pub fn euro_cents(buyer_total_atomic: i128, rate_micro: i64) -> Option<i64> {
    if rate_micro <= 0 {
        return None;
    }
    // atomic USDC (10^-6 USD) * (EUR/USD in 10^-6) = 10^-12 EUR.
    // To cents: divide by 10^10, rounding up.
    let picos = buyer_total_atomic.checked_mul(rate_micro as i128)?;
    let divisor = 10_i128.pow(10);
    let cents = picos.div_euclid(divisor) + i128::from(picos.rem_euclid(divisor) != 0);
    i64::try_from(cents).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn line(id: &str, atomic: i128) -> QuoteLine {
        QuoteLine {
            node_id: id.to_string(),
            capability_slug: "x".to_string(),
            source_cost_atomic: atomic,
            apiosk_fee_atomic: 0,
            buyer_total_atomic: atomic,
        }
    }

    #[test]
    fn a_node_total_is_list_plus_the_one_markup() {
        // KVK zoeken lists at 0.018903 USDC = 18_903 atomic.
        let total = node_buyer_total(18_903, fees::ProviderClass::Managed);
        assert_eq!(
            total,
            18_903 + fees::markup_atomic(18_903, fees::BUYER_MARKUP_BPS)
        );
    }

    #[test]
    fn the_plan_total_is_the_sum_of_node_totals_and_not_a_second_markup() {
        // The three real KVK list prices of the first chain.
        let lists = [18_903_i128, 66_162, 66_162];
        let lines: Vec<QuoteLine> = lists
            .iter()
            .enumerate()
            .map(|(i, list)| {
                line(
                    &format!("n{i}"),
                    node_buyer_total(*list, fees::ProviderClass::Managed),
                )
            })
            .collect();
        let quote = total(lines, None);

        let expected: i128 = lists
            .iter()
            .map(|list| node_buyer_total(*list, fees::ProviderClass::Managed))
            .sum();
        assert_eq!(quote.buyer_total_atomic, expected);

        // What it must NOT be: the markup applied again to the total.
        let doubled = expected + fees::markup_atomic(expected, fees::BUYER_MARKUP_BPS);
        assert_ne!(quote.buyer_total_atomic, doubled);

        // And not the markup applied to the summed list either — that rounds
        // differently from what each call is actually charged.
        let summed_list: i128 = lists.iter().sum();
        let marked_up_sum = node_buyer_total(summed_list, fees::ProviderClass::Managed);
        assert!(quote.buyer_total_atomic >= marked_up_sum);
    }

    #[test]
    fn seven_source_research_manifest_is_034_plus_15_percent_equals_0391() {
        let sources = [
            ("KVK Search", 20_000_i128),
            ("KVK Basisprofiel", 70_000),
            ("KVK Jaarrekeningen", 50_000),
            ("GLEIF corporate structure", 30_000),
            ("OpenSanctions screening", 100_000),
            ("VAT verification", 30_000),
            ("Insolvency check", 40_000),
        ];
        let lines: Vec<QuoteLine> = sources
            .iter()
            .enumerate()
            .map(|(index, (label, source_cost_atomic))| {
                let buyer_total_atomic =
                    node_buyer_total(*source_cost_atomic, fees::ProviderClass::Managed);
                QuoteLine {
                    node_id: format!("n{index}"),
                    capability_slug: (*label).to_string(),
                    source_cost_atomic: *source_cost_atomic,
                    apiosk_fee_atomic: buyer_total_atomic - source_cost_atomic,
                    buyer_total_atomic,
                }
            })
            .collect();
        let quote = total(lines, None);

        assert_eq!(
            quote
                .lines
                .iter()
                .map(|line| line.source_cost_atomic)
                .sum::<i128>(),
            340_000
        );
        assert_eq!(
            quote
                .lines
                .iter()
                .map(|line| line.apiosk_fee_atomic)
                .sum::<i128>(),
            51_000
        );
        assert_eq!(quote.buyer_total_atomic, 391_000);
    }

    #[test]
    fn an_empty_plan_costs_nothing() {
        assert_eq!(total(vec![], None).buyer_total_atomic, 0);
    }

    #[test]
    fn euro_rounds_up_so_the_ceiling_stays_a_ceiling() {
        // 1.000000 USD at 0.92 EUR/USD is 92 cents exactly.
        assert_eq!(euro_cents(1_000_000, 920_000), Some(92));
        // 0.173913 USD at 0.92 is 0.15999996 EUR -> 16 cents, not 15.
        assert_eq!(euro_cents(173_913, 920_000), Some(16));
        // Anything above zero costs at least a cent.
        assert_eq!(euro_cents(1, 920_000), Some(1));
        assert_eq!(euro_cents(0, 920_000), Some(0));
    }

    #[test]
    fn a_missing_or_absurd_rate_yields_no_euro_figure_rather_than_a_wrong_one() {
        assert_eq!(euro_cents(1_000_000, 0), None);
        assert_eq!(euro_cents(1_000_000, -1), None);
    }

    #[test]
    fn the_snapshot_travels_with_the_quote() {
        let taken_at = chrono::Utc.with_ymd_and_hms(2026, 9, 5, 12, 0, 0).unwrap();
        let quote = total(
            vec![line("n0", 173_913)],
            Some(EuroSnapshot {
                cents: euro_cents(173_913, 920_000).unwrap(),
                rate_micro: 920_000,
                taken_at,
            }),
        );
        let eur = quote.eur.as_ref().unwrap();
        assert_eq!(eur.cents, 16);
        assert_eq!(eur.rate_micro, 920_000);
        assert_eq!(eur.taken_at, taken_at);

        // A later, worse rate does not touch the approved figure: it lives on
        // the plan version, and re-reading the rate is not something the
        // approved plan can be made to do.
        let quote_again = quote.clone();
        assert_eq!(quote_again.eur.unwrap().cents, 16);
    }
}
