-- USDC leaves the treasury only for providers that sell through x402.
--
-- A normal API (called with Apiosk's own API key, credits or subscription) is
-- paid through that contract, never per call on-chain. Such listings were still
-- `provider_payout`, so every paid call sent the provider share in USDC to the
-- listing wallet (mostly Apiosk's own payout wallet 0x8797…3bb4), costing gas.
--
-- x402 listings are untouched: listing_type 'federated', resold listings
-- (listing_metadata.resell.enabled) and listings that declare an x402 payment
-- model (listing_metadata.payment_model). Everything else becomes Apiosk
-- revenue: the buyer's balance is charged as before, nothing is sent on-chain.
-- The executor treats a listing as Apiosk revenue when it is platform-owned,
-- `apiosk_revenue`, needs no payout wallet and is an 'api' listing.
BEGIN;

CREATE TEMP TABLE payout_target ON COMMIT DROP AS
SELECT id FROM providers.apis
WHERE listing_type::text = 'api'
  AND coalesce(listing_metadata->'resell'->>'enabled', 'false') <> 'true'
  AND NOT (coalesce(listing_metadata, '{}'::jsonb) ? 'payment_model')
  AND NOT (is_platform_owned AND settlement_mode = 'apiosk_revenue' AND NOT requires_payout_wallet);

UPDATE providers.apis a
SET is_platform_owned = true,
    settlement_mode = 'apiosk_revenue',
    requires_payout_wallet = false,
    listing_metadata = coalesce(a.listing_metadata, '{}'::jsonb) || jsonb_build_object('payout_stopped', jsonb_build_object('on', '2026-09-15', 'reason', 'not an x402 provider'))
FROM payout_target t
WHERE a.id = t.id;

DO $verify$ BEGIN
  IF EXISTS (
    SELECT 1 FROM providers.apis
    WHERE listing_type::text = 'api'
      AND coalesce(listing_metadata->'resell'->>'enabled', 'false') <> 'true'
      AND NOT (coalesce(listing_metadata, '{}'::jsonb) ? 'payment_model')
      AND NOT (is_platform_owned AND settlement_mode = 'apiosk_revenue' AND NOT requires_payout_wallet)
  ) THEN
    RAISE EXCEPTION 'a non-x402 listing would still pay out on-chain';
  END IF;
END $verify$;

COMMIT;
