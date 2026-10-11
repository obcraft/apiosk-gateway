-- Match the authenticated cb_entity_get output schema: labels and card
-- entities may be null. No added scope, fields, execution or price changes.
BEGIN;
UPDATE catalog.fact_types SET value_schema=jsonb_set(jsonb_set(jsonb_set(jsonb_set(jsonb_set(jsonb_set(value_schema,
 '{properties,data,properties,entity_def_id}',jsonb_build_object('type',jsonb_build_array('string','null'),'enum',jsonb_build_array(CASE split_part(slug,'.',1) WHEN 'company' THEN 'organization' WHEN 'person' THEN 'person' ELSE 'funding_round' END,NULL))),
 '{properties,data,properties,properties,items,properties,label}','{"type":["string","null"]}'),
 '{properties,data,properties,cards,items,properties,label}','{"type":["string","null"]}'),
 '{properties,data,properties,cards,items,properties,entity_def_id}','{"type":["string","null"]}'),
 '{properties,data,properties,cards,items,properties,entities,type}','["array","null"]'),
 '{properties,data,properties,cards,items,properties,entities,items,properties,properties,items,properties,label}','{"type":["string","null"]}')
WHERE slug IN ('company.crunchbase_profile_result','person.crunchbase_profile_result','funding.crunchbase_profile_result');
UPDATE catalog.capabilities c SET output_contract=f.value_schema FROM catalog.fact_types f
WHERE c.slug IN ('company.crunchbase-profile','person.crunchbase-profile','funding.crunchbase-profile') AND f.slug=replace(c.slug,'-','_')||'_result';
COMMIT;
