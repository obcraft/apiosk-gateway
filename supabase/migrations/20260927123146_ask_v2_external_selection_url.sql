-- The server already creates approved selections. External purchases also
-- need the canonical URL resolved from the provider directory. No new read
-- privileges, role membership, or balance mutation privileges are granted.
DO $$
BEGIN
  IF to_regclass('actions.user_selected_apis') IS NOT NULL THEN
    GRANT INSERT(resource_url) ON actions.user_selected_apis TO gateway_v2_runtime;
  ELSIF to_regclass('public.user_selected_apis') IS NOT NULL THEN
    GRANT INSERT(resource_url) ON public.user_selected_apis TO gateway_v2_runtime;
  ELSE
    RAISE EXCEPTION 'Selection table unavailable';
  END IF;
END $$;

-- Ask v2 already discovers through this exact public Coinbase endpoint. Make
-- its existing directory entry available for server-side settlement quoting.
-- Keep every other index and all payment-host/price policies unchanged.
DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM catalog.discovery_sources
    WHERE id='coinbase-x402-bazaar' AND fetch_kind='queried' AND shape='x402-items'
      AND url='https://api.cdp.coinbase.com/platform/v2/x402/discovery/search?limit=20&query={query}'
  ) THEN
    RAISE EXCEPTION 'Coinbase discovery configuration changed; review before enabling';
  END IF;
  UPDATE catalog.discovery_sources SET enabled=true
    WHERE id='coinbase-x402-bazaar' AND enabled=false;
END $$;
