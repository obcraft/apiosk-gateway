-- Companies House documents this response as companyProfile. Its existing
-- reviewed fields and validation remain identical; the aliases name the whole
-- registration record, not unrelated employee/review/funding data.
-- https://developer-specs.company-information.service.gov.uk/companies-house-public-data-api/resources/companyprofile?v=latest
DO $migration$
DECLARE
  actual jsonb;
  expected jsonb := $schema${"properties":{"company_name":{"$comment":"{\"apiosk_output_aliases_v1\":[\"legal_name\"]}","type":"string"},"company_number":{"$comment":"{\"apiosk_output_aliases_v1\":[\"registration_number\"]}","type":"string"},"company_status":{"$comment":"{\"apiosk_output_aliases_v1\":[\"registration_status\"]}","type":"string"},"date_of_cessation":{"type":"string"},"date_of_creation":{"$comment":"{\"apiosk_output_aliases_v1\":[\"incorporation_date\"]}","type":"string"},"jurisdiction":{"type":"string"},"previous_company_names":{"items":{"properties":{"ceased_on":{"type":"string"},"effective_from":{"type":"string"},"name":{"type":"string"}},"type":"object"},"type":"array"},"registered_office_address":{"$comment":"{\"apiosk_output_aliases_v1\":[\"registered_address\"]}","properties":{"address_line_1":{"type":"string"},"address_line_2":{"type":"string"},"care_of":{"type":"string"},"country":{"type":"string"},"locality":{"type":"string"},"po_box":{"type":"string"},"postal_code":{"type":"string"},"premises":{"type":"string"},"region":{"type":"string"}},"type":"object"},"sic_codes":{"$comment":"{\"apiosk_output_aliases_v1\":[\"industry_codes\"]}","items":{"type":"string"},"type":"array"},"type":{"$comment":"{\"apiosk_output_aliases_v1\":[\"company_type\",\"legal_form\"]}","type":"string"}},"required":["company_name","company_number"],"type":"object"}$schema$::jsonb;
  reviewed jsonb;
  matching_contracts integer;
BEGIN
  SELECT count(*) INTO matching_contracts
  FROM catalog.capability_execution_contracts c
  JOIN providers.api_endpoints e ON e.id=c.endpoint_id
  JOIN providers.apis a ON a.id=e.api_id
  WHERE a.slug='companies-house' AND e.active AND e.method='GET'
    AND e.path='/company' AND c.status='active'
    AND c.capability_slug='company.registry-profile'
    AND c.input_map='{"company.registration.gb.companies_house":{"in":"path","name":"company_number"}}'::jsonb
    AND c.output_map->'produces'->'company.uk_registry_record'='{"path":"$"}'::jsonb;
  IF matching_contracts <> 1
  THEN RAISE EXCEPTION 'Reviewed Companies House profile contract identity drift'; END IF;
  SELECT value_schema INTO STRICT actual FROM catalog.fact_types
    WHERE slug='company.uk_registry_record' AND status='active' FOR UPDATE;
  reviewed := expected || jsonb_build_object('$comment',
    '{"apiosk_output_aliases_v1":["profile","company_profile","registry_profile","registered_details"]}');
  IF actual IS DISTINCT FROM expected AND actual IS DISTINCT FROM reviewed
  THEN RAISE EXCEPTION 'Reviewed Companies House profile output schema drift'; END IF;
  UPDATE catalog.fact_types SET value_schema=reviewed
    WHERE slug='company.uk_registry_record';
END;
$migration$;
