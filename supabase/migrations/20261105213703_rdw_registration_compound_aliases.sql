-- Combined make/model requests bind to both manufacturer and commercial designation.
DO $rdw$
DECLARE
  facts regclass := coalesce(to_regclass('catalog.fact_types'),to_regclass('public.fact_types'));
  rdw_schema jsonb;
  field_name text;
  field jsonb;
  metadata jsonb;
  aliases jsonb;
BEGIN
  IF facts IS NULL THEN RAISE EXCEPTION 'Fact catalog is missing'; END IF;
  EXECUTE format('SELECT value_schema FROM %s WHERE slug=$1 FOR UPDATE',facts)
    INTO rdw_schema USING 'vehicle.rdw.technical_page';
  IF rdw_schema IS NULL THEN RAISE EXCEPTION 'RDW fact definition is missing'; END IF;
  FOREACH field_name IN ARRAY ARRAY['merk','handelsbenaming'] LOOP
    field := coalesce(rdw_schema #> ARRAY['properties','items','items','properties',field_name],'{"type":"string"}'::jsonb);
    metadata := coalesce((field->>'$comment')::jsonb,'{}'::jsonb);
    SELECT jsonb_agg(DISTINCT value ORDER BY value) INTO aliases
      FROM jsonb_array_elements(coalesce(metadata->'apiosk_output_aliases_v1','[]'::jsonb) || '["make_model"]'::jsonb);
    metadata := metadata || jsonb_build_object('apiosk_output_aliases_v1',aliases);
    rdw_schema := jsonb_set(rdw_schema,ARRAY['properties','items','items','properties',field_name],field || jsonb_build_object('$comment',metadata::text));
  END LOOP;
  EXECUTE format('UPDATE %s SET value_schema=$1 WHERE slug=$2',facts)
    USING rdw_schema,'vehicle.rdw.technical_page';
END $rdw$;
