-- These adapter routes consume one JSON array query value, not CSV or exploded fields.
DO $$
DECLARE changed integer;
BEGIN
  UPDATE providers.api_endpoints e
  SET query_params = jsonb_set(e.query_params,
    ARRAY['properties', CASE e.path WHEN '/apiosk/company-address' THEN 'addresses' ELSE 'designations' END, 'x-query-encoding'],
    '"json"'::jsonb, true)
  FROM providers.apis a
  WHERE a.id=e.api_id AND a.slug='pdok' AND e.method='GET'
    AND e.path IN ('/apiosk/company-address','/apiosk/linked-parcels');
  GET DIAGNOSTICS changed = ROW_COUNT;
  IF changed <> 2 THEN RAISE EXCEPTION 'Expected exactly two PDOK JSON array endpoints, got %', changed; END IF;
END $$;
