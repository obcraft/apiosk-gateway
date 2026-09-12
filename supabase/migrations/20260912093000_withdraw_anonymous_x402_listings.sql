-- Withdraw the two anonymous x402 resale listings from the catalog.
--
-- WHY THESE TWO AND NOT THE OTHER RESELLERS
-- ---------------------------------------------------------------------------
-- "x402 Endpoints" and "x402 Seller" are not brands. Neither has a website_url;
-- both point at an anonymous onrender.com host for documentation, and both are
-- named in `readiness::assess` by title, in the same list as "unknown" and
-- "unnamed", as titles that cannot count as a documented identity. Between them
-- they publish 75 endpoints and hold zero execution contracts, so every one of
-- those endpoints is a row a person can read about and nobody can buy.
--
-- On a page whose claim is "ask this and get an answer" that is the worst
-- combination available: the largest endpoint counts in the catalog attached to
-- the least executable sources in it. AgentBodega and CrowdPull are also
-- discovery-only today, but they are named sellers with a documented identity
-- and a route to contracts; these two have neither.
--
-- WHAT THIS DOES NOT DO
-- ---------------------------------------------------------------------------
-- The listings and their endpoints stay in the database. One historical
-- `user_selected_apis` row from 2026-09-02 references x402-endpoints and is
-- untouched, so that purchase still reads back with its source name intact.
--
-- REVERSING THIS IS NOT A SINGLE UPDATE. `apiosk_apis_review_gate` rewrites any
-- transition to status='active' into 'pending' unless the transaction holds the
-- API-specific approval. Re-listing means the review path
-- (`admin_review_api(id,'approve')`), on purpose.
BEGIN;
SELECT pg_advisory_xact_lock(hashtext('apiosk-withdraw-x402-listings'));

DO $withdraw$ DECLARE affected int; BEGIN
  -- The premise is that nothing buyable is lost. If either listing has since
  -- gained a contract, that premise is gone and this must be reconsidered
  -- rather than applied.
  IF EXISTS (SELECT 1 FROM public.capability_execution_contracts c
      JOIN public.api_endpoints e ON e.id = c.endpoint_id
      JOIN public.apis a ON a.id = e.api_id
      WHERE a.slug IN ('x402-endpoints','x402-seller')) THEN
    RAISE EXCEPTION 'An x402 listing now carries an execution contract; withdrawing it would remove buyable capability';
  END IF;

  UPDATE public.apis SET status='inactive', active=false, discoverable=false,
    listing_metadata = coalesce(listing_metadata,'{}'::jsonb) || jsonb_build_object(
      'catalog_state','withdrawn',
      'withdrawn_on','2026-09-12',
      'withdrawn_reason','Anonymous resale listing: no documented identity, no website, and no runtime-accepted execution contract.')
  WHERE slug IN ('x402-endpoints','x402-seller');
  GET DIAGNOSTICS affected = ROW_COUNT;
  IF affected <> 2 THEN RAISE EXCEPTION 'Expected exactly 2 x402 listings, updated %', affected; END IF;
END $withdraw$;

-- The catalog snapshot in src/catalog.sql reads status, active, is_verified,
-- discoverable and suspended_at together. Three of the five are now off, so
-- neither listing can reach /v1/providers, /v2/sources or the planner.
DO $verify$ BEGIN
  IF EXISTS (SELECT 1 FROM public.apis WHERE slug IN ('x402-endpoints','x402-seller')
      AND status='active' AND coalesce(active,true) AND coalesce(discoverable,true)) THEN
    RAISE EXCEPTION 'An x402 listing is still published';
  END IF;
END $verify$;

COMMIT;
