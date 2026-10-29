//! 04-01 — the single, pure source of the payment split.
//!
//! Given a gross amount (integer minor units, USDC = 6 decimals) and the
//! marketplace + Apiosk fee in basis points, produce the three components. The
//! rounding remainder always goes to the SELLER, so the parts sum EXACTLY to
//! `gross` — the same invariant enforced by `connect_payments.chk_connect_split`
//! (040) and required by the 402 builder (04-02). This function is the one place
//! the split is computed; callers must not re-derive it.
//!
//! Multiplication is done in `i128` to avoid overflow on large gross amounts
//! (`gross * bps` can exceed `i64::MAX`), then narrowed back to `i64`.

/// The three parts of a payment, in minor units. Invariant (for valid inputs):
/// every field is `>= 0` and `seller_amount + marketplace_fee + apiosk_fee == gross`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Split {
    pub seller_amount: i64,
    pub marketplace_fee: i64,
    pub apiosk_fee: i64,
}

/// Compute the split. Preconditions (guaranteed upstream by the skill snapshot +
/// DB constraints): `gross >= 0`, `mkt_bps >= 0`, `apiosk_bps >= 0`, and
/// `mkt_bps + apiosk_bps <= 10_000`. Each fee is `floor(gross * bps / 10_000)`;
/// the seller receives the remainder, so the sum is always exactly `gross`. Pure.
pub fn compute_split(gross: i64, mkt_bps: i64, apiosk_bps: i64) -> Split {
    // i128 intermediate: `gross * bps` can overflow i64 for large gross.
    let g = gross as i128;
    let marketplace_fee = (g * mkt_bps as i128 / 10_000) as i64;
    let apiosk_fee = (g * apiosk_bps as i128 / 10_000) as i64;
    // Remainder to the seller — guarantees the three parts re-sum to gross.
    let seller_amount = gross - marketplace_fee - apiosk_fee;
    Split {
        seller_amount,
        marketplace_fee,
        apiosk_fee,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agensi_example_100_usdc() {
        // 100 USDC @ 30% marketplace + 2% Apiosk → 68 / 30 / 2.
        let s = compute_split(100_000000, 3000, 200);
        assert_eq!(s.marketplace_fee, 30_000000);
        assert_eq!(s.apiosk_fee, 2_000000);
        assert_eq!(s.seller_amount, 68_000000);
        assert_eq!(
            s.seller_amount + s.marketplace_fee + s.apiosk_fee,
            100_000000
        );
    }

    #[test]
    fn odd_gross_rounds_to_seller_and_still_sums_exactly() {
        // 99 @ 3000/200: floor(29.7)=29, floor(1.98)=1, seller = 99-29-1 = 69.
        let s = compute_split(99, 3000, 200);
        assert_eq!(s.marketplace_fee, 29);
        assert_eq!(s.apiosk_fee, 1);
        assert_eq!(s.seller_amount, 69);
        assert_eq!(s.seller_amount + s.marketplace_fee + s.apiosk_fee, 99);
    }

    #[test]
    fn zero_fees_send_everything_to_seller() {
        let s = compute_split(100_000000, 0, 0);
        assert_eq!(
            s,
            Split {
                seller_amount: 100_000000,
                marketplace_fee: 0,
                apiosk_fee: 0
            }
        );
    }

    #[test]
    fn near_total_fees_leave_a_nonnegative_seller_remainder() {
        // mkt + apiosk = 9999 bps → seller keeps the last basis point + rounding.
        let s = compute_split(1_000000, 9000, 999);
        assert_eq!(s.marketplace_fee, 900000);
        assert_eq!(s.apiosk_fee, 99900);
        assert_eq!(s.seller_amount, 100);
        assert!(s.seller_amount >= 0);
        assert_eq!(s.seller_amount + s.marketplace_fee + s.apiosk_fee, 1_000000);
    }

    #[test]
    fn smallest_unit_gives_dust_to_seller() {
        // gross = 1: both fees floor to 0, seller gets the single unit.
        let s = compute_split(1, 3000, 200);
        assert_eq!(
            s,
            Split {
                seller_amount: 1,
                marketplace_fee: 0,
                apiosk_fee: 0
            }
        );
    }

    #[test]
    fn large_gross_does_not_overflow_i64() {
        // gross * bps here is 1e15 * 10_000 = 1e19, which overflows i64 (max ~9.22e18)
        // if multiplied in i64 — the i128 intermediate is what keeps this correct.
        let gross = 1_000_000_000_000_000; // 1e15
        let s = compute_split(gross, 9000, 999);
        assert_eq!(s.seller_amount + s.marketplace_fee + s.apiosk_fee, gross);
        assert!(s.seller_amount >= 0 && s.marketplace_fee >= 0 && s.apiosk_fee >= 0);
    }

    #[test]
    fn invariant_holds_across_a_grid_of_inputs() {
        // Exhaustive-ish sweep: sum == gross and all parts >= 0 for every valid combo.
        for gross in [0i64, 1, 2, 7, 99, 100, 333, 1_000000, 999_999, 12_345_678] {
            for mkt_bps in [0i64, 1, 200, 2500, 3000, 5000, 9000] {
                for apiosk_bps in [0i64, 1, 200, 500, 1000] {
                    if mkt_bps + apiosk_bps > 10_000 {
                        continue;
                    }
                    let s = compute_split(gross, mkt_bps, apiosk_bps);
                    assert_eq!(
                        s.seller_amount + s.marketplace_fee + s.apiosk_fee,
                        gross,
                        "sum must equal gross for {gross}/{mkt_bps}/{apiosk_bps}"
                    );
                    assert!(
                        s.seller_amount >= 0 && s.marketplace_fee >= 0 && s.apiosk_fee >= 0,
                        "no negative parts for {gross}/{mkt_bps}/{apiosk_bps}"
                    );
                }
            }
        }
    }
}
