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
//! One exception, on the buyer side only: an Apiosk **workflow run** is priced
//! at list + 20% ([`WORKFLOW_BUYER_MARKUP_BPS`], via [`quote_with_markup`]),
//! and only when the App's authenticated service asks for it. The provider
//! legs above are unchanged; the extra 5% is Apiosk's take.
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

/// The buyer markup on an Apiosk **workflow run** (first: the supplier payment
/// check), in basis points. The App's buyer ledger reserves and settles these
/// runs at list + 20% and tells this executor so on the one authenticated
/// surface that may (`X-Apiosk-Buyer-Markup-Bps` beside a valid
/// `X-Apiosk-Service-Key`, see `crate::service_auth::buyer_markup_bps`).
///
/// The extra 5% is Apiosk's: provider legs are computed from the list price
/// alone and do not move with the markup.
pub const WORKFLOW_BUYER_MARKUP_BPS: i128 = 2_000; // 20%

/// Normalise a requested buyer markup to one this module prices at.
///
/// Exactly two markups exist — [`BUYER_MARKUP_BPS`] and
/// [`WORKFLOW_BUYER_MARKUP_BPS`]. Anything else (0, 1999, 10 000, a negative
/// number) is not a tariff anyone agreed to and falls back to the default
/// 15%, so a malformed or hostile value can never under- or over-charge.
pub fn allowed_buyer_markup_bps(requested_bps: i128) -> i128 {
    match requested_bps {
        WORKFLOW_BUYER_MARKUP_BPS => WORKFLOW_BUYER_MARKUP_BPS,
        _ => BUYER_MARKUP_BPS,
    }
}

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

/// Keep the existing buyer tariff; a reviewed open-data listing has no
/// provider payout. All collected revenue belongs to Apiosk.
///
/// The tariff is whatever `quote` was taken at: pass a
/// [`quote_with_markup`] at 2000 bps and the buyer total is list + 20%, all
/// of it Apiosk's take.
pub fn platform_revenue_quote(mut quote: FeeQuote) -> FeeQuote {
    quote.provider_fee_atomic = 0;
    quote.provider_payout_atomic = 0;
    quote.apiosk_take_atomic = quote.buyer_total_atomic;
    quote
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

/// [`quote`] at an explicit buyer markup, in basis points.
///
/// `buyer_markup_bps` is normalised through [`allowed_buyer_markup_bps`]: only
/// 1500 and 2000 are honoured, anything else prices at the default 15%. The
/// markup changes the buyer total and Apiosk's take only — the provider fee
/// and payout are functions of the list price and class, identical at every
/// markup — and the legs still re-sum to the buyer total exactly.
pub fn quote_with_markup(
    list_atomic: i128,
    class: ProviderClass,
    buyer_markup_bps: i128,
) -> FeeQuote {
    let list_atomic = list_atomic.max(0);
    let buyer_markup_bps = allowed_buyer_markup_bps(buyer_markup_bps);

    // The markup rounds up and the provider fee rounds down: the platform
    // never earns less than the rule says, and the provider is never paid a
    // fraction of a unit less than theirs.
    let buyer_markup_atomic = markup_atomic(list_atomic, buyer_markup_bps);
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

/// The settlement quote for a **resold** call: the bridge wallet already paid
/// the provider `upstream_atomic` inline, and the buyer was charged
/// `list_quote.buyer_total_atomic` (at whatever markup `list_quote` was taken).
///
/// The bridge is reimbursed exactly what it fronted, capped at the buyer total
/// so the platform never owes out more than it took in, and every remaining
/// unit — the standard markup and, on a 20% workflow run, the extra 5% — is
/// Apiosk's take. This is the arithmetic `proxy::settlement` books; it lives
/// here so it is priced (and tested) beside every other split.
pub fn resell_bridge_quote(list_quote: FeeQuote, upstream_atomic: i128) -> FeeQuote {
    let buyer_total = list_quote.buyer_total_atomic.max(0);
    let upstream = upstream_atomic.clamp(0, buyer_total);
    federated_bridge_quote(upstream, buyer_total - upstream)
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
    buyer_total_usd_with_markup(price_usd, hosted_externally, BUYER_MARKUP_BPS)
}

/// [`buyer_total_usd`] at an explicit (normalised) buyer markup.
pub fn buyer_total_usd_with_markup(
    price_usd: f64,
    hosted_externally: bool,
    buyer_markup_bps: i128,
) -> f64 {
    let list = bigdecimal::BigDecimal::try_from(price_usd).unwrap_or_default();
    let class = ProviderClass::from_listing_type(Some(if hosted_externally {
        "federated"
    } else {
        "managed"
    }));
    quote_usdc_with_markup(&list, class, buyer_markup_bps).buyer_total_atomic as f64
        / USDC_SCALE as f64
}

pub fn quote_usdc(list: &BigDecimal, class: ProviderClass) -> FeeQuote {
    quote(usdc_to_atomic(list), class)
}

/// [`quote_usdc`] at an explicit (normalised) buyer markup, in basis points.
pub fn quote_usdc_with_markup(
    list: &BigDecimal,
    class: ProviderClass,
    buyer_markup_bps: i128,
) -> FeeQuote {
    quote_with_markup(usdc_to_atomic(list), class, buyer_markup_bps)
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

    #[test]
    fn public_source_revenue_preserves_buyer_tariff_without_provider_payout() {
        for list in [0, 1, 17_391, 47_259, 100_000] {
            let original = quote(list, ProviderClass::Managed);
            let revenue = platform_revenue_quote(original);
            assert_eq!(revenue.buyer_total_atomic, original.buyer_total_atomic);
            assert_eq!(revenue.buyer_markup_atomic, original.buyer_markup_atomic);
            assert_eq!(revenue.list_atomic, original.list_atomic);
            assert_eq!(revenue.provider_payout_atomic, 0);
            assert_eq!(revenue.apiosk_take_atomic, original.buyer_total_atomic);
        }
    }

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
    // ── Per-request buyer markup (workflow runs at list + 20%) ─────────────

    #[test]
    fn only_the_two_tariffs_are_honoured() {
        assert_eq!(allowed_buyer_markup_bps(1_500), 1_500);
        assert_eq!(allowed_buyer_markup_bps(2_000), 2_000);
        for other in [0i128, -2_000, 1, 1_499, 1_501, 1_999, 2_001, 2_500, 10_000] {
            assert_eq!(
                allowed_buyer_markup_bps(other),
                BUYER_MARKUP_BPS,
                "{other} bps is not a tariff and must price at 15%"
            );
        }
        // An unrecognised markup prices exactly like the default quote.
        for list in [1i128, 99, 100_000, 1_234_567] {
            for class in [ProviderClass::Federated, ProviderClass::Managed] {
                assert_eq!(quote_with_markup(list, class, 1_750), quote(list, class));
                assert_eq!(quote_with_markup(list, class, 1_500), quote(list, class));
            }
        }
    }

    /// $0.10 federated at 20%: buyer pays $0.12, provider keeps $0.10, the
    /// extra 5% is Apiosk's.
    #[test]
    fn federated_ten_cent_example_at_twenty_percent() {
        let q = quote_with_markup(100_000, ProviderClass::Federated, WORKFLOW_BUYER_MARKUP_BPS);
        assert_eq!(q.buyer_markup_atomic, 20_000);
        assert_eq!(q.buyer_total_atomic, 120_000, "buyer pays $0.12");
        assert_eq!(q.provider_payout_atomic, 100_000, "provider keeps $0.10");
        assert_eq!(q.provider_fee_atomic, 0);
        assert_eq!(q.apiosk_take_atomic, 20_000, "Apiosk earns $0.02");
    }

    /// $0.10 managed at 20%: buyer pays $0.12, provider keeps $0.09 as at 15%,
    /// Apiosk earns 30% of list.
    #[test]
    fn managed_ten_cent_example_at_twenty_percent() {
        let q = quote_with_markup(100_000, ProviderClass::Managed, WORKFLOW_BUYER_MARKUP_BPS);
        assert_eq!(q.buyer_total_atomic, 120_000, "buyer pays $0.12");
        assert_eq!(q.provider_fee_atomic, 10_000);
        assert_eq!(q.provider_payout_atomic, 90_000, "provider keeps $0.09");
        assert_eq!(q.apiosk_take_atomic, 30_000, "Apiosk earns $0.03");
    }

    /// The markup moves the buyer total and Apiosk's take, never a provider
    /// leg; the difference between the tariffs is entirely Apiosk's; and the
    /// legs re-sum at every price, with the markup still rounding up.
    #[test]
    fn twenty_percent_leaves_provider_legs_unchanged_and_resums() {
        for list in [
            0i128,
            1,
            2,
            3,
            5,
            7,
            9,
            15,
            99,
            100,
            4_999,
            18_903,
            100_000,
            100_001,
            999_999,
            1_000_000,
            1_234_567,
            123_456_789,
        ] {
            for class in [ProviderClass::Federated, ProviderClass::Managed] {
                let std = quote(list, class);
                let wf = quote_with_markup(list, class, WORKFLOW_BUYER_MARKUP_BPS);
                assert_eq!(wf.list_atomic, std.list_atomic);
                assert_eq!(
                    wf.provider_fee_atomic, std.provider_fee_atomic,
                    "{list} {class:?}"
                );
                assert_eq!(
                    wf.provider_payout_atomic, std.provider_payout_atomic,
                    "{list} {class:?}"
                );
                assert_eq!(wf.class, std.class);
                assert_eq!(
                    wf.provider_payout_atomic + wf.apiosk_take_atomic,
                    wf.buyer_total_atomic,
                    "legs must re-sum for {list} {class:?}"
                );
                assert_eq!(
                    wf.buyer_markup_atomic,
                    markup_atomic(list, WORKFLOW_BUYER_MARKUP_BPS),
                    "20% markup rounds up for {list}"
                );
                assert!(wf.buyer_total_atomic >= std.buyer_total_atomic);
                assert_eq!(
                    wf.apiosk_take_atomic - std.apiosk_take_atomic,
                    wf.buyer_total_atomic - std.buyer_total_atomic,
                    "the extra markup is all Apiosk's for {list} {class:?}"
                );
            }
        }
        // Sub-cent: 20% of 3 units is 0.6, which rounds up to a full unit.
        let dust = quote_with_markup(3, ProviderClass::Managed, WORKFLOW_BUYER_MARKUP_BPS);
        assert_eq!(dust.buyer_markup_atomic, 1);
        assert_eq!(dust.provider_payout_atomic, 3);
        // 20% of 7 units is 1.4 → 2.
        assert_eq!(
            quote_with_markup(7, ProviderClass::Federated, WORKFLOW_BUYER_MARKUP_BPS)
                .buyer_markup_atomic,
            2
        );
    }

    #[test]
    fn platform_revenue_at_twenty_percent_books_everything_to_apiosk() {
        for list in [0i128, 1, 17_391, 47_259, 100_000] {
            let wf = quote_with_markup(list, ProviderClass::Managed, WORKFLOW_BUYER_MARKUP_BPS);
            let revenue = platform_revenue_quote(wf);
            assert_eq!(
                revenue.buyer_total_atomic,
                list + markup_atomic(list, WORKFLOW_BUYER_MARKUP_BPS),
                "buyer total is list + 20%"
            );
            assert_eq!(revenue.provider_payout_atomic, 0);
            assert_eq!(revenue.apiosk_take_atomic, revenue.buyer_total_atomic);
            assert_eq!(
                revenue.buyer_total_atomic
                    - platform_revenue_quote(quote(list, ProviderClass::Managed))
                        .buyer_total_atomic,
                markup_atomic(list, WORKFLOW_BUYER_MARKUP_BPS)
                    - markup_atomic(list, BUYER_MARKUP_BPS)
            );
        }
        let q = platform_revenue_quote(quote_with_markup(100_000, ProviderClass::Managed, 2_000));
        assert_eq!(q.buyer_total_atomic, 120_000);
        assert_eq!(q.apiosk_take_atomic, 120_000);
    }

    /// A resold call reimburses the bridge exactly what it fronted at either
    /// tariff; at 20% the extra 5% lands in Apiosk's take.
    #[test]
    fn resell_bridge_at_twenty_percent_reimburses_exactly_and_banks_the_extra() {
        let std = resell_bridge_quote(quote(100_000, ProviderClass::Federated), 100_000);
        assert_eq!(std.buyer_total_atomic, 115_000);
        assert_eq!(std.provider_payout_atomic, 100_000);
        assert_eq!(std.apiosk_take_atomic, 15_000);

        let wf = resell_bridge_quote(
            quote_with_markup(100_000, ProviderClass::Federated, WORKFLOW_BUYER_MARKUP_BPS),
            100_000,
        );
        assert_eq!(wf.buyer_total_atomic, 120_000, "buyer paid list + 20%");
        assert_eq!(
            wf.provider_payout_atomic, 100_000,
            "bridge made whole exactly"
        );
        assert_eq!(
            wf.apiosk_take_atomic, 20_000,
            "15% + the extra 5% to Apiosk"
        );

        for (list, upstream) in [
            (100_000i128, 95_000i128),
            (7, 7),
            (1_234_567, 1_000_000),
            (5, 0),
        ] {
            for bps in [BUYER_MARKUP_BPS, WORKFLOW_BUYER_MARKUP_BPS] {
                let q = resell_bridge_quote(
                    quote_with_markup(list, ProviderClass::Managed, bps),
                    upstream,
                );
                assert_eq!(q.provider_payout_atomic, upstream, "bridge leg is upstream");
                assert_eq!(
                    q.provider_payout_atomic + q.apiosk_take_atomic,
                    q.buyer_total_atomic
                );
            }
        }
        // An upstream above what the buyer paid is capped, never negative.
        let capped = resell_bridge_quote(quote(100, ProviderClass::Federated), 1_000);
        assert_eq!(capped.provider_payout_atomic, 115);
        assert_eq!(capped.apiosk_take_atomic, 0);
    }

    /// The resell helper is exactly the arithmetic settlement booked inline
    /// before it moved here.
    #[test]
    fn resell_bridge_quote_matches_the_previous_inline_booking() {
        for list in [10_000i128, 100_000, 5, 15, 1_234_567] {
            for upstream in [0i128, 1, list, list + 1] {
                let list_quote = quote(list, ProviderClass::Federated);
                let buyer_total = list_quote.buyer_total_atomic;
                let up = upstream.min(buyer_total);
                assert_eq!(
                    resell_bridge_quote(list_quote, upstream),
                    federated_bridge_quote(up, buyer_total - up)
                );
            }
        }
    }

    #[test]
    fn usdc_and_usd_helpers_honour_the_markup() {
        let list = BigDecimal::from_str("0.10").unwrap();
        let q = quote_usdc_with_markup(&list, ProviderClass::Managed, WORKFLOW_BUYER_MARKUP_BPS);
        assert_eq!(q.buyer_total_usdc(), BigDecimal::from_str("0.12").unwrap());
        assert_eq!(
            q.provider_payout_usdc(),
            BigDecimal::from_str("0.09").unwrap()
        );
        assert_eq!(
            quote_usdc_with_markup(&list, ProviderClass::Managed, 42),
            quote_usdc(&list, ProviderClass::Managed)
        );
        assert!((buyer_total_usd_with_markup(0.10, false, 2_000) - 0.12).abs() < 1e-9);
        assert!((buyer_total_usd_with_markup(0.10, true, 9_999) - 0.115).abs() < 1e-9);
        assert_eq!(
            buyer_total_usd(0.10, false),
            buyer_total_usd_with_markup(0.10, false, 1_500)
        );
    }
}
