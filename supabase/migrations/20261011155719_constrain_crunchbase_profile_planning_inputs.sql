-- A profile plan needs only the known public entity ID. The adapter chooses
-- the reviewed public field/card defaults; direct endpoint schemas retain selectors.
BEGIN;
UPDATE catalog.capability_execution_contracts SET
 requires=(SELECT jsonb_agg(r) FROM jsonb_array_elements(requires) r WHERE r->>'fact_type' IN ('company.crunchbase_entity_id','person.crunchbase_entity_id','funding.crunchbase_entity_id')),
 input_map=(SELECT jsonb_object_agg(key,value) FROM jsonb_each(input_map) WHERE key IN ('company.crunchbase_entity_id','person.crunchbase_entity_id','funding.crunchbase_entity_id')),
 updated_at=now()
 WHERE adapter_version='crunchbase-mcp-v1' AND capability_slug IN ('company.crunchbase-profile','person.crunchbase-profile','funding.crunchbase-profile');
COMMIT;
