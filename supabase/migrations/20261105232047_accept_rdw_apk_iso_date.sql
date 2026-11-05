-- Preserve the source date verbatim for both RDW YYYYMMDD and Overheid.io ISO dates.
DO $apk$
DECLARE
  facts regclass := coalesce(to_regclass('catalog.fact_types'), to_regclass('public.fact_types'));
  changed integer;
BEGIN
  IF facts IS NULL THEN RAISE EXCEPTION 'Fact catalog is missing'; END IF;
  EXECUTE format('UPDATE %s SET value_schema = jsonb_set(value_schema, ''{pattern}'', $1) WHERE slug = $2', facts)
    USING '"^([0-9]{8}|[0-9]{4}-[0-9]{2}-[0-9]{2})$"'::jsonb, 'vehicle.apk_expiry';
  GET DIAGNOSTICS changed = ROW_COUNT;
  IF changed <> 1 THEN RAISE EXCEPTION 'APK fact definition is missing'; END IF;
END $apk$;
