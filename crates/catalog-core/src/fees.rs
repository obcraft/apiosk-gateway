//! The hybrid fee model — the single source of truth for what a buyer pays,
//! what a provider receives, and what Apiosk keeps.
//!
//! Specified in `docs/FEES_AND_BUSINESS_MODEL.md` and the 2026-08-18 "Apiosk
//! Hybrid Fee Model" note. One buyer-facing rule, two provider classes:
//!
//! ```text
//!                     buyer pays        provider receives     Apiosk keeps
//!   Federated         list × 1.15       list × 1.00           15% of list
//!   Managed           list × 1.15       list × 0.90           25% of list
//! ```
//!
//! The buyer-side rule never varies: **buyer total = list + 15%**. Only the
//! provider leg depends on how much of the commercial infrastructure Apiosk
//! operates for that listing.
//!
//! # Why this module exists
//!
//! The previous model took a flat 2% *out of* the list price, so a single
//! `commission_rate` on the `apis` row described the whole split. The hybrid
//! model is two-sided — a markup added on top AND a provider fee taken off —
//! which no single rate can express. Everything that needs to know a fee now
//! calls [`quote`]; nothing re-derives a split from a bare percentage.
//!
//! # Units
//!
//! All arithmetic is in **atomic USDC** (6 decimals, `i128`) so the legs are
//! exact integers that re-sum to the buyer total with no floating point and no
//! lost dust. Rounding is fixed by construction and goes in Apiosk's favour on
//! both sides: the buyer markup rounds **up** to the next atomic unit
//! ([`markup_atomic`]), the managed provider fee rounds **down**, and whatever
//! is left between the two lands in [`FeeQuote::apiosk_take_atomic`]. A
//! provider is therefore never paid a fraction of a unit less than the rule
//! says, and the platform never earns less than 15% of list — at a $0.000003
//! list price the markup is a full unit, not zero. The app-side pricing module
//! (`app/supabase/functions/_shared/pricing.ts`) rounds the same way, so the
//! two never disagree by a unit.

use bigdecimal::{BigDecimal, ToPrimitive};

/// Atomic units per whole USDC (6 decimals).
pub const USDC_SCALE: i128 = 1_000_000;

/// Basis-point denominator.
pub const BPS_DENOMINATOR: i128 = 10_000;

/// What every buyer pays on top of the provider's list price, in basis points.
/// Invariant across both provider classes — this is the number a buyer can
/// reason about without knowing how the endpoint is sourced.
pub const BUYER_MARKUP_BPS: i128 = 1_500; // 15%

/// What a **managed** provider pays for onboarding, publishing, Bazaar seeding,
/// gateway execution, payment collection, payouts and distribution.
pub const MANAGED_PROVIDER_FEE_BPS: i128 = 1_000; // 10%

/// What a **federated** provider pays: nothing. They already run their own x402
/// endpoint and only receive additional demand through Apiosk, so they keep
/// their full asking price.
pub const FEDERATED_PROVIDER_FEE_BPS: i128 = 0;

/// How a listing is distributed through Apiosk. This — not the provider's
/// identity, and never the ranking — decides the provider-side fee.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderClass {
    /// Sells independently through x402 at its own price; Apiosk discovers,
    /// compares, selects and executes. Provider pays no fee.
    Federated,
    /// Uses Apiosk as its sales, distribution and payment infrastructure.
    /// Provider pays [`MANAGED_PROVIDER_FEE_BPS`].
    Managed,
}

impl ProviderClass {
    /// Classify from the `apis.listing_type` column. `'federated'` is the single
    /// authoritative marker for external x402 supply (the same predicate used by
    /// `db::discovery` and `proxy::external_fetch`); everything else — `api`,
    /// `dataset`, `service`, `connector`, `skill`, `product`, or NULL — is
    /// managed supply that Apiosk publishes and operates.
    pub fn from_listing_type(listing_type: Option<&str>) -> Self {
        match listing_type.map(str::trim) {
            Some("federated") | Some("external") => Self::Federated,
            _ => Self::Managed,
        }
    }

    /// The provider-side fee for this class, in basis points of the list price.
    pub fn provider_fee_bps(self) -> i128 {
        match self {
            Self::Federated => FEDERATED_PROVIDER_FEE_BPS,
            Self::Managed => MANAGED_PROVIDER_FEE_BPS,
        }
    }

    /// Stable wire/ledger name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Federated => "federated",
            Self::Managed => "managed",
        }
    }
}

/// The full economics of one paid call, in atomic USDC.
///
/// Invariant, guaranteed by [`quote`] for every input:
/// `provider_payout_atomic + apiosk_take_atomic == buyer_total_atomic`,
/// and every field is `>= 0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeQuote {
    /// The provider's asking price — what the buyer is shown as the API price.
    pub list_atomic: i128,
    /// The 15% added on top. Disclosed to the buyer, never hidden.
    pub buyer_markup_atomic: i128,
    /// What actually leaves the buyer's wallet. This is the number that
    /// `max_price` / per-tx caps must be checked against.
    pub buyer_total_atomic: i128,
    /// The provider-side fee (0 for federated, 10% of list for managed).
    pub provider_fee_atomic: i128,
    /// What the provider receives.
    pub provider_payout_atomic: i128,
    /// What is retained in the Apiosk platform wallet: buyer markup + provider
    /// fee + any rounding remainder.
    pub apiosk_take_atomic: i128,
    /// Which class produced this quote.
    pub class: ProviderClass,
}

impl FeeQuote {
    /// The Apiosk take expressed in basis points **of the buyer total**.
    ///
    /// This existed for the on-chain settlement contract, which split the gross
    /// that arrived rather than the list price and so needed a rate. It is
    /// deliberately *not* a constant: 15/115 (federated) and 25/115 (managed)
    /// are 1304.35 and 2173.91 bps, neither of which is a whole number of basis
    /// points — which is precisely why `settle_exact` passed the atomic legs
    /// on-chain instead of this figure.
    ///
    /// That contract is gone with the buyer's on-chain rail, so nothing in the
    /// binary calls this any more; the two tests below are its only remaining
    /// readers. Kept because it is the clearest statement of why the split
    /// cannot be expressed as a rate, which is the mistake it was written to
    /// prevent.
    pub fn apiosk_take_bps_of_gross(&self) -> i128 {
        if self.buyer_total_atomic == 0 {
            return 0;
        }
        self.apiosk_take_atomic * BPS_DENOMINATOR / self.buyer_total_atomic
    }

    pub fn list_usdc(&self) -> BigDecimal {
        atomic_to_usdc(self.list_atomic)
    }

    pub fn buyer_markup_usdc(&self) -> BigDecimal {
        atomic_to_usdc(self.buyer_markup_atomic)
    }

    pub fn buyer_total_usdc(&self) -> BigDecimal {
        atomic_to_usdc(self.buyer_total_atomic)
    }

    pub fn provider_payout_usdc(&self) -> BigDecimal {
        atomic_to_usdc(self.provider_payout_atomic)
    }

    pub fn apiosk_take_usdc(&self) -> BigDecimal {
        atomic_to_usdc(self.apiosk_take_atomic)
    }

    /// The buyer-facing disclosure block. Shows all three numbers — list price,
    /// platform fee, total charged — because a total that exceeds the displayed
    /// price is only defensible if the buyer is told the tariff.
    ///
    /// Carries nothing about the provider leg: the upstream destination and the
    /// amount paid to the provider are gateway-internal and must never reach a
    /// client response.
    pub fn buyer_disclosure(&self) -> serde_json::Value {
        serde_json::json!({
            "list_price_usdc": self.list_usdc().to_string(),
            "platform_fee_usdc": self.buyer_markup_usdc().to_string(),
            "total_charged_usdc": self.buyer_total_usdc().to_string(),
            "platform_fee_bps": BUYER_MARKUP_BPS,
        })
    }
}

/// The buyer markup on a list price, in atomic units, rounded **up**.
///
/// This is the one place the markup's rounding is decided. Every route that
/// adds a platform fee on top of a provider price — [`quote`], the external
/// x402 fetch proxy with its configurable `EXTERNAL_FETCH_FEE_BPS`, the demo
/// balance — computes it here, so a buyer is shown and charged the same unit
/// everywhere. Rounding up is what makes "list + 15%" hold at sub-cent prices:
/// floored, a $0.000003 call would carry no fee at all.
pub fn markup_atomic(list_atomic: i128, markup_bps: i128) -> i128 {
    let list_atomic = list_atomic.max(0);
    let markup_bps = markup_bps.max(0);
    div_ceil(list_atomic * markup_bps, BPS_DENOMINATOR)
}

/// Integer division rounded towards positive infinity, for non-negative
/// numerators and positive denominators (the only inputs this module produces;
/// `i128::div_ceil` is not on stable yet).
fn div_ceil(numerator: i128, denominator: i128) -> i128 {
    (numerator + denominator - 1) / denominator
}

/// Price one call under the hybrid model.
///
/// `list_atomic` is the provider's asking price in atomic USDC. A negative
/// input is clamped to zero; a zero price yields an all-zero quote (free
/// endpoints never reach settlement).
pub fn quote(list_atomic: i128, class: ProviderClass) -> FeeQuote {
    quote_with_markup(list_atomic, class, BUYER_MARKUP_BPS)
}

/// The buyer markup for runs of an Apiosk workflow (e.g. the supplier payment
/// check): list + 20%. The provider leg is unchanged; the extra goes to Apiosk.
pub const WORKFLOW_MARKUP_BPS: i128 = 2_000;

/// Only the standard and the workflow markup exist; anything else is standard.
pub fn allowed_markup(markup_bps: i128) -> i128 {
    if markup_bps == WORKFLOW_MARKUP_BPS { WORKFLOW_MARKUP_BPS } else { BUYER_MARKUP_BPS }
}

/// [`quote`] with an allowed buyer markup ([`allowed_markup`]).
pub fn quote_with_markup(list_atomic: i128, class: ProviderClass, markup_bps: i128) -> FeeQuote {
    let list_atomic = list_atomic.max(0);
    let markup_bps = allowed_markup(markup_bps);

    // The markup rounds up and the provider fee rounds down: the platform
    // never earns less than the rule says, and the provider is never paid a
    // fraction of a unit less than theirs.
    let buyer_markup_atomic = markup_atomic(list_atomic, markup_bps);
    let provider_fee_atomic = list_atomic * class.provider_fee_bps() / BPS_DENOMINATOR;

    let buyer_total_atomic = list_atomic + buyer_markup_atomic;
    let provider_payout_atomic = list_atomic - provider_fee_atomic;

    // Remainder to Apiosk, so the legs re-sum to the buyer total exactly.
    let apiosk_take_atomic = buyer_total_atomic - provider_payout_atomic;

    FeeQuote {
        list_atomic,
        buyer_markup_atomic,
        buyer_total_atomic,
        provider_fee_atomic,
        provider_payout_atomic,
        apiosk_take_atomic,
        class,
    }
}

/// Recover the full quote from a **buyer total** that was already charged.
///
/// Some rails quote and settle in two phases: the challenge stores what the
/// buyer must pay, and settlement later splits that stored amount. By then the
/// list price is gone, so the split has to be derived back out of the gross.
///
/// The markup inside the gross is `buyer_total × 1500 / 11500`, rounded **up**,
/// and the list is the rest. That is the exact inverse of [`quote`]'s
/// rounded-up forward markup: for every list price `x`,
/// `from_buyer_total(quote(x).buyer_total).list == x` (tested exhaustively
/// below). A gross that no list price produces — the 15% rule cannot reach
/// every integer — recovers the nearest list below it and leaves the odd unit
/// in the fee, so the platform can never owe out more than it took in.
pub fn from_buyer_total(buyer_total_atomic: i128, class: ProviderClass) -> FeeQuote {
    let buyer_total_atomic = buyer_total_atomic.max(0);
    let markup_in_gross = div_ceil(
        buyer_total_atomic * BUYER_MARKUP_BPS,
        BPS_DENOMINATOR + BUYER_MARKUP_BPS,
    )
    .min(buyer_total_atomic);
    let list_atomic = buyer_total_atomic - markup_in_gross;
    let provider_fee_atomic = list_atomic * class.provider_fee_bps() / BPS_DENOMINATOR;
    let provider_payout_atomic = list_atomic - provider_fee_atomic;
    FeeQuote {
        list_atomic,
        buyer_markup_atomic: buyer_total_atomic - list_atomic,
        buyer_total_atomic,
        provider_fee_atomic,
        provider_payout_atomic,
        // Remainder, so the legs re-sum to the gross that actually settled.
        apiosk_take_atomic: buyer_total_atomic - provider_payout_atomic,
        class,
    }
}

/// Build the settlement quote for a **federated bridge fetch**, where the
/// platform payer wallet has already fronted the provider's own x402 pull and
/// must be reimbursed exactly.
///
/// This differs from [`quote`] in one way: the fee is passed in rather than
/// derived, because the external-fetch route allows a deployment to override the
/// markup (`EXTERNAL_FETCH_FEE_BPS`) or put a floor under it
/// (`EXTERNAL_FETCH_FEE_MIN_USDC`). The provider leg, however, is never
/// negotiable — it is exactly what the bridge paid upstream, so the bridge is
/// made whole to the unit and the federated provider keeps their full asking
/// price.
///
/// `apiosk_take` is the markup, and it goes to the platform wallet rather than
/// being left in the bridge wallet: the bridge is a float, not a revenue
/// account, and a markup that accumulates there is invisible to the books.
pub fn federated_bridge_quote(quoted_atomic: i128, fee_atomic: i128) -> FeeQuote {
    let quoted_atomic = quoted_atomic.max(0);
    let fee_atomic = fee_atomic.max(0);
    FeeQuote {
        list_atomic: quoted_atomic,
        buyer_markup_atomic: fee_atomic,
        buyer_total_atomic: quoted_atomic + fee_atomic,
        provider_fee_atomic: 0,
        // Reimburse the bridge exactly what it fronted.
        provider_payout_atomic: quoted_atomic,
        apiosk_take_atomic: fee_atomic,
        class: ProviderClass::Federated,
    }
}

/// Price one call from a `BigDecimal` list price (the shape the `endpoints`
/// table and the x402 requirement builders use).
/// The buyer total for one call, as a plain USD float.
///
/// The same arithmetic as [`quote_usdc`], in the shape the read-only surfaces
/// and the confirm UI speak: list plus the 15% markup, rounded up to the
/// atomic unit. Floats are the wire shape of `price_usd` throughout discovery
/// — the money itself is always summed in atomic units.
pub fn buyer_total_usd(price_usd: f64, hosted_externally: bool) -> f64 {
    let list = bigdecimal::BigDecimal::try_from(price_usd).unwrap_or_default();
    let class = ProviderClass::from_listing_type(Some(if hosted_externally {
        "federated"
    } else {
        "managed"
    }));
    quote_usdc(&list, class).buyer_total_atomic as f64 / USDC_SCALE as f64
}

pub fn quote_usdc(list: &BigDecimal, class: ProviderClass) -> FeeQuote {
    quote(usdc_to_atomic(list), class)
}

/// The value `apis.commission_rate` should carry for a listing of this class,
/// expressed the way that column always was: Apiosk's share as a fraction of
/// what the buyer paid.
///
/// Federated is 15/115 and managed is 25/115 — recurring decimals, so these are
/// the 4-decimal roundings `NUMERIC(6,4)` can actually store, matching what
/// migration 088 backfills. Used only to detect stale rows; no money is moved
/// from this number.
pub fn legacy_commission_rate_for(class: ProviderClass) -> BigDecimal {
    use std::str::FromStr;
    match class {
        // 15/115 = 0.130434…
        ProviderClass::Federated => BigDecimal::from_str("0.1304").unwrap_or_default(),
        // 25/115 = 0.217391…
        ProviderClass::Managed => BigDecimal::from_str("0.2174").unwrap_or_default(),
    }
}

/// Convert a decimal USDC amount to atomic units, rounding to the nearest unit.
/// Values beyond `i128` range saturate rather than wrap.
pub fn usdc_to_atomic(value: &BigDecimal) -> i128 {
    (value * BigDecimal::from(USDC_SCALE as i64))
        .round(0)
        .to_i128()
        .unwrap_or(0)
}

/// Convert atomic units back to a 6-decimal USDC amount.
pub fn atomic_to_usdc(atomic: i128) -> BigDecimal {
    BigDecimal::from(atomic) / BigDecimal::from(USDC_SCALE as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    /// The worked example from the fee model note: a $0.10 federated API.
    #[test]
    fn federated_ten_cent_example() {
        let q = quote(100_000, ProviderClass::Federated);
        assert_eq!(q.buyer_total_atomic, 115_000, "buyer pays $0.115");
        assert_eq!(q.provider_payout_atomic, 100_000, "provider keeps $0.10");
        assert_eq!(q.apiosk_take_atomic, 15_000, "Apiosk earns $0.015");
        assert_eq!(q.provider_fee_atomic, 0, "federated providers pay nothing");
    }

    /// The worked example from the fee model note: a $0.10 managed API.
    #[test]
    fn managed_ten_cent_example() {
        let q = quote(100_000, ProviderClass::Managed);
        assert_eq!(q.buyer_total_atomic, 115_000, "buyer pays $0.115");
        assert_eq!(q.provider_payout_atomic, 90_000, "provider keeps $0.09");
        assert_eq!(q.apiosk_take_atomic, 25_000, "Apiosk earns $0.025");
    }

    /// The buyer-side rule is identical across classes — that is the whole
    /// point of the model, and the thing a regression would quietly break.
    #[test]
    fn buyer_price_does_not_depend_on_provider_class() {
        for list in [1i128, 7, 999, 100_000, 1_234_567, 50_000_000] {
            let f = quote(list, ProviderClass::Federated);
            let m = quote(list, ProviderClass::Managed);
            assert_eq!(
                f.buyer_total_atomic, m.buyer_total_atomic,
                "buyer total must be list + 15% regardless of class (list={list})"
            );
        }
    }

    /// The take rates quoted to investors: ~13.0% of buyer spend federated,
    /// ~21.7% managed.
    #[test]
    fn effective_take_rate_of_buyer_spend() {
        let f = quote(100_000, ProviderClass::Federated);
        assert_eq!(f.apiosk_take_bps_of_gross(), 1304); // 13.04%
        let m = quote(100_000, ProviderClass::Managed);
        assert_eq!(m.apiosk_take_bps_of_gross(), 2173); // 21.73%
    }

    /// Conservation: nothing is created or destroyed between the buyer's wallet
    /// and the two destinations, at any price, for either class.
    #[test]
    fn legs_always_resum_to_the_buyer_total() {
        for list in [
            0i128,
            1,
            2,
            3,
            7,
            9,
            99,
            100,
            4_999,
            100_000,
            999_999,
            1_000_000,
            123_456_789,
        ] {
            for class in [ProviderClass::Federated, ProviderClass::Managed] {
                let q = quote(list, class);
                assert_eq!(
                    q.provider_payout_atomic + q.apiosk_take_atomic,
                    q.buyer_total_atomic,
                    "legs must re-sum for list={list} class={:?}",
                    class
                );
                assert!(
                    q.provider_payout_atomic >= 0
                        && q.apiosk_take_atomic >= 0
                        && q.buyer_markup_atomic >= 0,
                    "no negative legs for list={list} class={:?}",
                    class
                );
            }
        }
    }

    /// A federated provider must receive their asking price to the unit — this
    /// is the promise that lets existing x402 supply be monetized without an
    /// integration, so rounding must never shave it.
    #[test]
    fn federated_provider_always_receives_exactly_list() {
        for list in [1i128, 3, 7, 99, 100_001, 7_777_777] {
            let q = quote(list, ProviderClass::Federated);
            assert_eq!(q.provider_payout_atomic, list);
        }
    }

    /// Sub-cent prices: the markup rounds up to a whole unit, the provider fee
    /// rounds down, and the difference is Apiosk's — never the provider's loss
    /// and never a free call.
    #[test]
    fn dust_prices_round_in_apiosks_direction() {
        // $0.000003 managed: 15% of 3 units is 0.45 — the buyer pays a full
        // unit of markup, the provider fee floors to 0.
        let q = quote(3, ProviderClass::Managed);
        assert_eq!(q.buyer_markup_atomic, 1, "markup rounds up to one unit");
        assert_eq!(q.buyer_total_atomic, 4);
        assert_eq!(q.provider_payout_atomic, 3, "provider fee floors to zero");
        assert_eq!(q.apiosk_take_atomic, 1, "the rounded-up unit is Apiosk's");
    }

    /// Odd amounts where 15% is not a whole number of atomic units: the markup
    /// is the next unit up, never the unit below.
    #[test]
    fn the_markup_rounds_up_on_odd_amounts() {
        for (list, markup) in [
            (5i128, 1i128),
            (15, 3),
            (1_234_567, 185_186),
            (99, 15),
            (100_001, 15_001),
        ] {
            let q = quote(list, ProviderClass::Federated);
            assert_eq!(q.buyer_markup_atomic, markup, "markup of {list}");
            assert_eq!(q.buyer_total_atomic, list + markup, "total of {list}");
            assert_eq!(
                q.provider_payout_atomic, list,
                "federated provider keeps list"
            );
            assert_eq!(q.apiosk_take_atomic, markup);
        }
        // Exact multiples of twenty are unchanged by the rounding direction.
        assert_eq!(
            quote(100_000, ProviderClass::Managed).buyer_total_atomic,
            115_000
        );
        assert_eq!(markup_atomic(20, BUYER_MARKUP_BPS), 3);
        assert_eq!(markup_atomic(21, BUYER_MARKUP_BPS), 4);
        assert_eq!(markup_atomic(0, BUYER_MARKUP_BPS), 0);
        assert_eq!(markup_atomic(-7, BUYER_MARKUP_BPS), 0);
        assert_eq!(markup_atomic(7, 0), 0);
    }

    /// The inverse must recover every list price exactly from its own forward
    /// quote, for both classes — a two-phase rail that stores the gross and
    /// splits it later must land on the legs the buyer was quoted.
    #[test]
    fn from_buyer_total_is_the_exact_inverse_of_quote_for_every_small_price() {
        for list in 0..=20_000i128 {
            for class in [ProviderClass::Federated, ProviderClass::Managed] {
                let fwd = quote(list, class);
                let back = from_buyer_total(fwd.buyer_total_atomic, class);
                assert_eq!(
                    back.list_atomic, list,
                    "list recovered for {list} {class:?}"
                );
                assert_eq!(back, fwd, "full quote recovered for {list} {class:?}");
            }
        }
    }

    #[test]
    fn zero_and_negative_prices_are_inert() {
        let q = quote(0, ProviderClass::Managed);
        assert_eq!(q.buyer_total_atomic, 0);
        assert_eq!(q.apiosk_take_atomic, 0);
        let neg = quote(-500, ProviderClass::Federated);
        assert_eq!(
            neg.list_atomic, 0,
            "negatives clamp rather than invert a split"
        );
        assert_eq!(neg.buyer_total_atomic, 0);
    }

    /// The bridge must be reimbursed to the unit and the markup must be the
    /// Apiosk leg — if the markup stayed in the bridge wallet it would never
    /// appear in the books.
    #[test]
    fn federated_bridge_quote_reimburses_exactly_and_banks_the_markup() {
        let q = federated_bridge_quote(100_000, 15_000);
        assert_eq!(
            q.provider_payout_atomic, 100_000,
            "bridge made whole exactly"
        );
        assert_eq!(q.apiosk_take_atomic, 15_000, "markup is the Apiosk leg");
        assert_eq!(q.buyer_total_atomic, 115_000, "buyer paid list + 15%");
        assert_eq!(
            q.provider_payout_atomic + q.apiosk_take_atomic,
            q.buyer_total_atomic
        );
        // A zero tariff is a subsidy, but must still balance rather than go
        // negative anywhere.
        let free = federated_bridge_quote(5_000, 0);
        assert_eq!(free.provider_payout_atomic, 5_000);
        assert_eq!(free.apiosk_take_atomic, 0);
        assert_eq!(free.buyer_total_atomic, 5_000);
    }

    /// A default-configured federated fetch must match the plain hybrid quote —
    /// the two paths price the same call and must not drift.
    #[test]
    fn federated_bridge_quote_matches_the_hybrid_quote_at_default_markup() {
        for list in [10_000i128, 100_000, 250_000, 1_000_000, 5, 15, 1_234_567] {
            let markup = markup_atomic(list, BUYER_MARKUP_BPS);
            let bridge = federated_bridge_quote(list, markup);
            let plain = quote(list, ProviderClass::Federated);
            assert_eq!(bridge.buyer_total_atomic, plain.buyer_total_atomic);
            assert_eq!(bridge.provider_payout_atomic, plain.provider_payout_atomic);
            assert_eq!(bridge.apiosk_take_atomic, plain.apiosk_take_atomic);
        }
    }

    /// Round-tripping a forward quote through the inverse must land on the same
    /// legs — a two-phase rail splits the stored gross, not the list price.
    #[test]
    fn from_buyer_total_inverts_quote() {
        for list in [10_000i128, 100_000, 250_000, 1_000_000, 33_330_000] {
            for class in [ProviderClass::Federated, ProviderClass::Managed] {
                let fwd = quote(list, class);
                let back = from_buyer_total(fwd.buyer_total_atomic, class);
                assert_eq!(
                    back.list_atomic, fwd.list_atomic,
                    "list for {list} {class:?}"
                );
                assert_eq!(
                    back.provider_payout_atomic, fwd.provider_payout_atomic,
                    "provider leg for {list} {class:?}"
                );
                assert_eq!(back.apiosk_take_atomic, fwd.apiosk_take_atomic);
            }
        }
    }

    /// The inverse must conserve too, including on gross amounts that never came
    /// from a clean list price.
    #[test]
    fn from_buyer_total_never_pays_out_more_than_came_in() {
        for gross in [0i128, 1, 2, 7, 11, 99, 100, 110_001, 999_999_999] {
            for class in [ProviderClass::Federated, ProviderClass::Managed] {
                let q = from_buyer_total(gross, class);
                assert_eq!(q.provider_payout_atomic + q.apiosk_take_atomic, gross);
                assert!(q.provider_payout_atomic >= 0 && q.apiosk_take_atomic >= 0);
                assert!(
                    q.provider_payout_atomic <= gross,
                    "provider leg must never exceed the gross for {gross} {class:?}"
                );
            }
        }
    }

    #[test]
    fn listing_type_classification() {
        assert_eq!(
            ProviderClass::from_listing_type(Some("federated")),
            ProviderClass::Federated
        );
        assert_eq!(
            ProviderClass::from_listing_type(Some("external")),
            ProviderClass::Federated
        );
        for managed in ["api", "dataset", "service", "connector", "skill", "product"] {
            assert_eq!(
                ProviderClass::from_listing_type(Some(managed)),
                ProviderClass::Managed,
                "{managed} is managed supply"
            );
        }
        assert_eq!(
            ProviderClass::from_listing_type(None),
            ProviderClass::Managed,
            "a NULL listing_type defaults to managed, matching db::discovery's COALESCE"
        );
    }

    #[test]
    fn bigdecimal_round_trip_matches_atomic_math() {
        let list = BigDecimal::from_str("0.10").unwrap();
        let q = quote_usdc(&list, ProviderClass::Managed);
        // Compared as values, not strings: BigDecimal normalises trailing zeros
        // and the rest of the gateway renders amounts with plain `to_string`.
        assert_eq!(q.buyer_total_usdc(), BigDecimal::from_str("0.115").unwrap());
        assert_eq!(
            q.provider_payout_usdc(),
            BigDecimal::from_str("0.09").unwrap()
        );
        assert_eq!(q.apiosk_take_usdc(), BigDecimal::from_str("0.025").unwrap());
    }

    /// The disclosure block is what makes the markup defensible; it must carry
    /// all three numbers and nothing about the provider leg.
    #[test]
    fn buyer_disclosure_shows_three_numbers_and_no_provider_leg() {
        let q = quote(100_000, ProviderClass::Managed);
        let d = q.buyer_disclosure();
        assert_eq!(d["list_price_usdc"], "0.1");
        assert_eq!(d["platform_fee_usdc"], "0.015");
        assert_eq!(d["total_charged_usdc"], "0.115");
        let serialized = d.to_string();
        assert!(
            !serialized.contains("provider") && !serialized.contains("payout"),
            "buyer disclosure must not leak the provider leg: {serialized}"
        );
    }
}
