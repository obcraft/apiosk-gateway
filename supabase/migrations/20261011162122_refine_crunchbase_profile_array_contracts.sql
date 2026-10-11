-- Default cb_entity_get responses always retain the two labelled arrays.
-- Individual values/cards can be missing; employee_count is the source's
-- headcount range, not a fabricated exact count. Preserve raw values.
BEGIN;
SELECT pg_advisory_xact_lock(hashtext('apiosk-crunchbase'));
UPDATE catalog.fact_types
SET value_schema=jsonb_set(value_schema,'{properties,data,required}','["properties","cards"]')
WHERE slug IN ('company.crunchbase_profile_result','person.crunchbase_profile_result','funding.crunchbase_profile_result');
UPDATE catalog.fact_types
SET value_schema=jsonb_set(value_schema,'{properties,data,properties,properties,$comment}',to_jsonb('{"apiosk_output_aliases_v1":["name","description","website","headquarters","headcount","employee_count","funding_total"]}'::text))
WHERE slug='company.crunchbase_profile_result';
UPDATE catalog.capabilities c SET output_contract=f.value_schema
FROM catalog.fact_types f
WHERE c.slug IN ('company.crunchbase-profile','person.crunchbase-profile','funding.crunchbase-profile')
AND f.slug=replace(c.slug,'-','_')||'_result';
COMMIT;
