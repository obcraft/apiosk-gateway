-- The native TenderNed plan already quotes this ordering. Its endpoint schema
-- omitted sort, so the shared executor correctly rejected it as undeclared and
-- the source silently used relevance order. Declare the source-supported value.
-- No price, permission, saved result, approval or execution row changes.
BEGIN;
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '20s';
SELECT pg_advisory_xact_lock(hashtext('tenderned-publication-sort-v1'));
DO $migration$
DECLARE
  endpoint_id uuid;
  current_query jsonb;
  expected_query jsonb := $schema${
  "page": {
    "type": "integer",
    "required": false,
    "description": "Zero-based page number."
  },
  "size": {
    "type": "integer",
    "required": false,
    "description": "Results per page (the site itself uses 50)."
  },
  "search": {
    "type": "string",
    "required": false,
    "description": "Free-text search over the notice title, the awarding authority and the contract description, e.g. zorg or wegonderhoud."
  },
  "cpvCodes": {
    "type": "string",
    "required": false,
    "description": "EU Common Procurement Vocabulary code, INCLUDING the check digit, e.g. 85000000-9 (health and social work). An eight-digit code without the check digit is rejected."
  },
  "procedure": {
    "enum": [
      "OPE",
      "NOP",
      "CCD",
      "OMB",
      "OZB",
      "OHP",
      "OH",
      "INP",
      "CA",
      "APM",
      "APE",
      "VUB"
    ],
    "type": "string",
    "required": false,
    "description": "Procedure code."
  },
  "publicatieId": {
    "type": "string",
    "required": false,
    "description": "A known publication id to fetch that notice's full detail directly, e.g. 438729. Omit to search instead using the parameters below.",
    "x-parameter-location": "path"
  },
  "typeOpdracht": {
    "enum": [
      "W",
      "L",
      "D"
    ],
    "type": "string",
    "required": false,
    "description": "Contract type: W werken (works), L leveringen (supplies), D diensten (services)."
  },
  "sluitingsDatumTot": {
    "type": "string",
    "required": false,
    "description": "Only notices closing up to this date, ISO yyyy-mm-dd."
  },
  "publicatieDatumTot": {
    "type": "string",
    "required": false,
    "description": "Only notices published up to this date, ISO yyyy-mm-dd."
  },
  "sluitingsDatumVanaf": {
    "type": "string",
    "required": false,
    "description": "Only notices closing on or after this date, ISO yyyy-mm-dd — the filter for finding tenders still open to bid on. Note the capital D."
  },
  "publicatieDatumVanaf": {
    "type": "string",
    "required": false,
    "description": "Only notices published on or after this date, ISO yyyy-mm-dd."
  },
  "publicatieDatumPreset": {
    "enum": [
      "AF7",
      "AF30"
    ],
    "type": "string",
    "required": false,
    "description": "Shorthand window on the publication date: AF7 for the last 7 days, AF30 for the last 30. Any other value is rejected."
  }
}$schema$::jsonb;
  expected_sort jsonb := $sort${"type": "string", "required": false, "enum": ["publicatieDatum,desc"], "description": "Order publications by publication date, newest first. Verified against the official public TenderNed search endpoint."}$sort$::jsonb;
BEGIN
  SELECT e.id,e.query_params INTO STRICT endpoint_id,current_query
  FROM providers.api_endpoints e
  JOIN providers.apis a ON a.id=e.api_id
  WHERE a.slug='tenderned' AND e.method='GET' AND e.path='/v2/publicaties'
    AND e.active
    AND a.origin_url='https://www.tenderned.nl/papi/tenderned-rs-tns'
    AND coalesce(a.endpoint_url,a.origin_url)=a.origin_url
  FOR UPDATE OF e;
  IF current_query - 'sort' IS DISTINCT FROM expected_query
     OR (current_query ? 'sort' AND current_query->'sort' IS DISTINCT FROM expected_sort) THEN
    RAISE EXCEPTION 'TenderNed query schema drift; review before applying';
  END IF;
  UPDATE providers.api_endpoints
  SET query_params=jsonb_set(current_query,'{sort}',expected_sort,true)
  WHERE id=endpoint_id;
END $migration$;
COMMIT;
