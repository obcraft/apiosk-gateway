// The legacy module also exposes ledger helpers used by its own binary.
#[allow(dead_code)]
#[path = "../legacy-executor/src/fees.rs"]
mod legacy_fees;

#[test]
fn catalog_and_executor_agree_on_every_pricing_leg() {
    use catalog_core::fees;
    for listing in [None, Some("api"), Some("federated"), Some("external")] {
        let class = fees::ProviderClass::from_listing_type(listing);
        let legacy_class = legacy_fees::ProviderClass::from_listing_type(listing);
        assert_eq!(class.as_str(), legacy_class.as_str());
        for price in (-1..=10_001).chain([47_259, 1_000_000, 1_000_000_001, i64::MAX as i128]) {
            for markup in [-1, 0, 1_500, 1_999, 2_000, 10_000] {
                let actual = fees::quote_with_markup(price, class, markup);
                let expected = legacy_fees::quote_with_markup(price, legacy_class, markup);
                assert_eq!(
                    (actual.list_atomic, actual.buyer_markup_atomic, actual.buyer_total_atomic,
                     actual.provider_fee_atomic, actual.provider_payout_atomic, actual.apiosk_take_atomic),
                    (expected.list_atomic, expected.buyer_markup_atomic, expected.buyer_total_atomic,
                     expected.provider_fee_atomic, expected.provider_payout_atomic, expected.apiosk_take_atomic),
                    "price={price}, markup={markup}, listing={listing:?}",
                );
            }
        }
    }
}
