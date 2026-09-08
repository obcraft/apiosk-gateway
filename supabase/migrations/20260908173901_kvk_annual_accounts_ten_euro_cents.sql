-- EUR 0.10 displayed buyer total at the currently published EUR/USD rate
-- 0.92. List 94,518 + ceil(15%) 14,178 = 108,696 micro-USD.
-- 0.108696 * 0.92 = EUR 0.10000032, displayed as EUR 0.10.
-- Deliberately fixes only annual accounts; search/profile prices stay intact.
-- USD settlement is retained. Future FX changes can change the EUR display.
BEGIN;
DO $$
DECLARE affected integer;
BEGIN
  UPDATE public.api_endpoints e SET price = 0.094518
  FROM public.apis a
  WHERE a.id=e.api_id AND a.slug='kvk-dutch-business-register'
    AND e.path='/api/v1/hvds/jaarrekeningen/kvknummer';
  GET DIAGNOSTICS affected = ROW_COUNT;
  IF affected <> 1 THEN RAISE EXCEPTION 'Expected one KVK annual-account endpoint, got %', affected; END IF;
END $$;
COMMIT;
