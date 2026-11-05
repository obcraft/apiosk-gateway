-- Reviewed RDW aliases; works before and after the catalog schema move.
DO $rdw$
DECLARE
  facts regclass := coalesce(to_regclass('catalog.fact_types'), to_regclass('public.fact_types'));
  rdw_schema jsonb;
  fields jsonb;
  entry record;
  field jsonb;
  metadata jsonb;
  aliases jsonb;
BEGIN
  IF facts IS NULL THEN RAISE EXCEPTION 'Fact catalog is missing'; END IF;
  EXECUTE format('SELECT value_schema FROM %s WHERE slug=$1 FOR UPDATE',facts)
    INTO rdw_schema USING 'vehicle.rdw.technical_page';
  IF rdw_schema IS NULL THEN RAISE EXCEPTION 'RDW fact definition is missing'; END IF;
  fields := coalesce(rdw_schema #> '{properties,items,items,properties}','{}'::jsonb);
  FOR entry IN SELECT * FROM jsonb_each($fields${"handelsbenaming":{"type":"string","$comment":"{\"apiosk_output_aliases_v1\":[\"trade_name\",\"commercial_name\",\"model\"]}","description":"RDW commercial designation (handelsbenaming), such as POLO. This does not identify an exact trim or model variant."},"datum_eerste_toelating":{"type":"string","$comment":"{\"apiosk_output_aliases_v1\":[\"first_admission_date\",\"first_registration\",\"first_registration_date\"]}","description":"Date of first admission to road use, including admission abroad. Report separately from first Dutch registration."},"openstaande_terugroepactie_indicator":{"type":"string","$comment":"{\"apiosk_output_aliases_v1\":[\"outstanding_recall_indicator\",\"recall_indicator\",\"recall_indicators\"]}","description":"Outstanding recall indicator only; does not provide recall details or recall history."},"datum_eerste_tenaamstelling_in_nederland":{"type":"string","$comment":"{\"apiosk_output_aliases_v1\":[\"first_dutch_registration_date\",\"first_registration\",\"first_registration_date\"]}","description":"Date of first registration in the Netherlands. Report separately from first admission."}}$fields$::jsonb)
  LOOP
    field := coalesce(fields->entry.key,'{"type":"string"}'::jsonb);
    metadata := coalesce((field->>'$comment')::jsonb,'{}'::jsonb);
    SELECT jsonb_agg(DISTINCT value ORDER BY value) INTO aliases
      FROM jsonb_array_elements(coalesce(metadata->'apiosk_output_aliases_v1','[]'::jsonb)
        || ((entry.value->>'$comment')::jsonb->'apiosk_output_aliases_v1'));
    metadata := metadata || jsonb_build_object('apiosk_output_aliases_v1',aliases);
    fields := fields || jsonb_build_object(entry.key,field || jsonb_build_object(
      '$comment',metadata::text,'description',entry.value->>'description'));
  END LOOP;
  rdw_schema := jsonb_set(rdw_schema,'{properties,items,items}',
    coalesce(rdw_schema #> '{properties,items,items}','{"type":"object"}'::jsonb) || jsonb_build_object('properties',fields));
  EXECUTE format('UPDATE %s SET value_schema=$1 WHERE slug=$2',facts)
    USING rdw_schema,'vehicle.rdw.technical_page';
END $rdw$;
