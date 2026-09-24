"""Render the bounded TenderNed browse contract from captured public evidence.

No network, credentials or database writes. Pass an existing CLI-created migration.
"""
import copy
import json
import sys
from pathlib import Path

root = Path(__file__).resolve().parents[1]
sample_path, migration_path = map(Path, sys.argv[1:])
sample = json.loads(sample_path.read_text())
assert sample["number"] == 0 and sample["size"] == 20
assert len(sample["content"]) <= 20 and sample["totalElements"] >= len(sample["content"])
original = json.loads((root / "tests/fixtures/workflows/catalog.json").read_text())
service = copy.deepcopy(next(s for s in original["services"]
                            if s["api_slug"] == "tenderned"
                            and s["contract"]["capability_slug"] == "company_intel.search"))
capability = "procurement.nl-public-contracts"
description = (
    "Browse Dutch public government contracts and procurement notices in the Netherlands via TenderNed. "
    "No keyword or company identifier required; optional literal search/date/sector filters. "
    "One page of 20, sorted by publication date. Preserve notice types: announcements, awards, "
    "corrections and cancellations. Not all are open tenders or awarded contracts; no full contract PDFs."
)
service["id"] = "4a6fd23f-9bf4-41d9-96f1-6ca514a2f6c4"  # Local fixture identity only.
service["description"] = description
service["jurisdiction"] = None
service["fixed_params"] = {"page": 0, "size": 20, "sort": "publicatieDatum,desc"}
service["sample_payload"] = sample
contract = service["contract"]
contract["capability_slug"] = capability
contract["version"] = 1
contract["requires"] = [dict(r, optional=True) for r in contract["requires"]
                        if r["fact_type"] != "company_intel.publicatieid"]
contract["input_map"].pop("company_intel.publicatieid")
used = {r["fact_type"] for r in contract["requires"]} | set(contract["output_map"]["produces"])
fixture = {"description": "TenderNed public upstream page captured 2026-09-24; no Apiosk purchase.",
           "services": [service],
           "vocabulary": [w for w in original["vocabulary"] if w["slug"] in used]}
(root / "tests/fixtures/tenderned-browse.json").write_text(json.dumps(fixture, ensure_ascii=False, indent=2) + "\n")

def sql_literal(value):
    return "'" + str(value).replace("'", "''") + "'"

def sql_json(value):
    return sql_literal(json.dumps(value, ensure_ascii=False, separators=(",", ":"))) + "::jsonb"

sql = f"""-- Add country-wide browsing without changing existing searches, prices or approvals.
-- Source: anonymous TenderNed GET /v2/publicaties?page=0&size=20&sort=publicatieDatum,desc.
-- Retains original publication types and bounded pagination; not an award-only feed.
BEGIN;
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '20s';
SELECT pg_advisory_xact_lock(hashtext('tenderned-public-contracts-browse-v1'));

DO $migration$
DECLARE
  template catalog.capability_execution_contracts%ROWTYPE;
  expected_requires jsonb := {sql_json(contract['requires'])};
  expected_map jsonb := {sql_json(contract['input_map'])};
  expected_fixed jsonb := {sql_json(service['fixed_params'])};
BEGIN
  SELECT c.* INTO STRICT template
  FROM catalog.capability_execution_contracts c
  JOIN providers.api_endpoints e ON e.id = c.endpoint_id
  JOIN providers.apis a ON a.id = e.api_id
  WHERE a.slug = 'tenderned' AND e.path = '/v2/publicaties' AND e.method = 'GET'
    AND e.active AND c.capability_slug = 'company_intel.search'
    AND c.status = 'active' AND c.version = 3;

  IF template.output_map <> {sql_json(contract['output_map'])}
     OR template.cardinality <> 'one' OR template.effect <> 'read'
     OR template.price_model <> 'per_call' THEN
    RAISE EXCEPTION 'TenderNed source contract drift; review before applying';
  END IF;

  INSERT INTO catalog.capabilities(slug,name,description,category,input_contract,output_contract,status)
  VALUES ('{capability}', 'Browse Dutch government contracts', {sql_literal(description)},
          'government', '{{"type":"object"}}', '{{"type":"object"}}', 'active')
  ON CONFLICT (slug) DO NOTHING;
  IF NOT EXISTS (SELECT 1 FROM catalog.capabilities
                 WHERE slug = '{capability}' AND status = 'active'
                   AND description = {sql_literal(description)}) THEN
    RAISE EXCEPTION 'Conflicting Dutch procurement capability';
  END IF;

  IF EXISTS (SELECT 1 FROM catalog.capability_execution_contracts
             WHERE endpoint_id = template.endpoint_id AND capability_slug = '{capability}') THEN
    IF NOT EXISTS (SELECT 1 FROM catalog.capability_execution_contracts
                   WHERE endpoint_id = template.endpoint_id AND capability_slug = '{capability}'
                     AND version = 1 AND status = 'active' AND requires = expected_requires
                     AND input_map = expected_map AND fixed_params = expected_fixed
                     AND output_map = template.output_map AND effect = 'read') THEN
      RAISE EXCEPTION 'Conflicting Dutch procurement browse contract';
    END IF;
    RETURN;
  END IF;

  INSERT INTO catalog.capability_execution_contracts
    (capability_slug,endpoint_id,version,status,requires,produces,input_map,fixed_params,
     output_map,cardinality,identifies,max_fan_out,preconditions,pagination,effect,
     price_model,freshness_seconds,sample_payload,validated_at,validated_note,notes)
  VALUES
    ('{capability}',template.endpoint_id,1,'active',expected_requires,template.produces,
     expected_map,expected_fixed,template.output_map,'one',NULL,1,'{{}}','{{}}','read',
     'per_call',3600,{sql_json(sample)},now(),
     'Anonymous upstream HTTP 200 with no keyword; bounded page, source types and dates preserved; Rust admission, binding and planning regression tests.',
     'tenderned-public-contracts-browse-v1; new read capability on the existing endpoint and tariff');
END $migration$;
COMMIT;
"""
assert migration_path.is_file(), "Create the migration with the Supabase CLI first"
migration_path.write_text(sql)
print(f"Rendered {migration_path.name} and TenderNed browse fixture")
