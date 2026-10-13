BEGIN;
SELECT pg_advisory_xact_lock(hashtext('apiosk-nl-benefits-20261013'));
DO $guard$ BEGIN
IF NOT EXISTS (SELECT 1 FROM providers.apis WHERE slug='koop' AND is_platform_owned AND owner_id='bf085fe8-41c4-4699-978f-e75e89518bf7'::uuid) THEN RAISE EXCEPTION 'Expected platform-owned KOOP source missing'; END IF;
END $guard$;
-- Apiosk source bundle v1. Schema inspected in production on 2026-10-11.
-- TEMPLATE: render with render.mjs and a reviewed manifest. Not an auto-applied migration.
-- One transaction registers ONE NEW source, all endpoints, vocabulary and contracts.
-- The source remains inactive. See README.txt for the separate publication contract.
-- Never embed real credentials in this file, its manifest, examples or AI input.
-- Optional backend credential input on the SAME connection, BEFORE this script:
--   SELECT set_config('apiosk.onboarding_secrets', $1, false);
-- Bind $1 to a JSON object {"VAULT_NAME":"secret"}; do not interpolate SQL.
-- Use a dedicated connection and clear the setting/discard connection in finally.
-- Vault storage does NOT implement a generic gateway credential resolver.

SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '30s';
SET LOCAL standard_conforming_strings = on;

DO $block_74138553edd806cd1a4053ffb5b287bda8716898230c085a5c0235a793f4fc3b$
DECLARE
  bundle jsonb := $bundle_74138553edd806cd1a4053ffb5b287bda8716898230c085a5c0235a793f4fc3b${
  "format": "apiosk.source-bundle.v1",
  "bundle_id": "888aed01-e330-4745-88b9-0d9e5c3eb4b8",
  "provenance": {
    "model": "codex",
    "generated_at": "2026-10-13T17:13:07.262800+00:00",
    "documentation_urls": [
      "https://data.overheid.nl/sites/default/files/dataset/d0cca537-44ea-48cf-9880-fa21e1a7058f/resources/HandleidingSRU2.0.pdf"
    ],
    "reviewer": null,
    "live_test": {
      "status": "not_run",
      "tested_at": null,
      "notes": "Apiosk execution not run. Upstream-only probe: HTTP 200, valid SRU XML, 736 results and 2 returned records for bijstand. Evidence: evidence/samenwerkende-catalogi.xml and probes.json"
    }
  },
  "source": {
    "owner_id": "bf085fe8-41c4-4699-978f-e75e89518bf7",
    "slug": "samenwerkende-catalogi",
    "name": "Samenwerkende Catalogi — overheidsproducten en diensten",
    "origin_url": "https://repository.overheid.nl",
    "description": "Landelijke index van overheidsproducten en diensten, met gemeentelijke aanvraaginformatie en bronlinks. Geen volledige gemeentelijke website-index en geen berekening van persoonlijk recht.",
    "long_description": "Landelijke index van overheidsproducten en diensten, met gemeentelijke aanvraaginformatie en bronlinks. Geen volledige gemeentelijke website-index en geen berekening van persoonlijk recht.",
    "category": "government",
    "docs_url": "https://data.overheid.nl/sites/default/files/dataset/d0cca537-44ea-48cf-9880-fa21e1a7058f/resources/HandleidingSRU2.0.pdf",
    "website_url": "https://data.overheid.nl/sites/default/files/dataset/d0cca537-44ea-48cf-9880-fa21e1a7058f/resources/HandleidingSRU2.0.pdf",
    "authentication_info": {
      "type": "none"
    },
    "static_headers": [
      {
        "name": "Accept",
        "value": "application/xml"
      }
    ],
    "listing_metadata": {
      "tags": [
        "gemeenten",
        "Samenwerkende Catalogi",
        "bijzondere bijstand",
        "inkomenstoeslag",
        "aanvragen",
        "lokale regelingen"
      ],
      "sectors": [
        "Government and public data"
      ],
      "jurisdiction": "NL",
      "language": "nl",
      "coverage_notice": "Landelijke index van overheidsproducten en diensten, met gemeentelijke aanvraaginformatie en bronlinks. Geen volledige gemeentelijke website-index en geen berekening van persoonlijk recht.",
      "readiness": {
        "state": "staged",
        "blockers": [
          "SRU XML normalization and bounded collection binding",
          "Apiosk execution acceptance",
          "Listing price review"
        ]
      },
      "pricing": {
        "state": "unpriced",
        "upstream_cost_usd": null,
        "listing_price_note": "Zero is a required database placeholder for inactive endpoints, not a free production offer."
      },
      "operator_notice": "Apiosk integration; not affiliated with the publisher.",
      "upstream": {
        "collection": "samenwerkendecatalogi",
        "protocol": "SRU 2.0",
        "fair_use": true,
        "maximum_records": 1000
      },
      "municipal_websites": {
        "state": "requires_connector",
        "discovery": "Official links from Samenwerkende Catalogi",
        "blockers": [
          "Verify official domain per municipality",
          "HTML/PDF extraction",
          "Per-domain robots and reuse policy",
          "Change detection"
        ],
        "subjects": [
          "bijzondere bijstand",
          "individuele inkomenstoeslag",
          "energiefondsen",
          "kwijtschelding",
          "kindregelingen",
          "sport en cultuur",
          "aanvraagvoorwaarden"
        ]
      },
      "upstream_probe": {
        "name": "samenwerkende-catalogi",
        "url": "https://repository.overheid.nl/sru?query=c.product-area%3D%3Dsamenwerkendecatalogi+AND+dt.title%3Dbijstand&maximumRecords=2",
        "final_url": "https://repository.overheid.nl/sru?query=c.product-area%3D%3Dsamenwerkendecatalogi+AND+dt.title%3Dbijstand&maximumRecords=2",
        "status": 200,
        "content_type": "application/xml",
        "bytes": 6713,
        "sha256": "4fbe64f58781372ba74aecebfea5c7c3a158b7bdd227718bf78d785cc0cdbf16"
      }
    }
  },
  "credential_requirements": [],
  "subject_kinds": [
    {
      "slug": "dataset",
      "description": "Dataset or source response"
    }
  ],
  "fact_types": [
    {
      "slug": "nl.municipal.services.search.input.query",
      "name": "query",
      "description": "Zoek gemeentelijke en landelijke producten en diensten: query",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": "string",
        "minLength": 1,
        "description": "CQL scoped to c.product-area==samenwerkendecatalogi; collection enforcement must be implemented before activation."
      },
      "freshness_seconds": null,
      "match_hints": {}
    },
    {
      "slug": "nl.municipal.services.search.input.startrecord",
      "name": "startRecord",
      "description": "Zoek gemeentelijke en landelijke producten en diensten: startRecord",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": "integer",
        "minimum": 1
      },
      "freshness_seconds": null,
      "match_hints": {}
    },
    {
      "slug": "nl.municipal.services.search.input.maximumrecords",
      "name": "maximumRecords",
      "description": "Zoek gemeentelijke en landelijke producten en diensten: maximumRecords",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": "integer",
        "minimum": 1,
        "maximum": 25
      },
      "freshness_seconds": null,
      "match_hints": {}
    },
    {
      "slug": "nl.municipal.services.search.response",
      "name": "Zoek gemeentelijke en landelijke producten en diensten",
      "description": "Search product descriptions and links. One bounded SRU page. No entitlement or complete municipal coverage claim; enforce collection before activation.",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": "string",
        "description": "Raw SRU XML. Executor parsing and normalized response contract are pending."
      },
      "freshness_seconds": 86400,
      "match_hints": {}
    }
  ],
  "capabilities": [
    {
      "slug": "nl.municipal.services.search",
      "name": "Zoek gemeentelijke en landelijke producten en diensten",
      "description": "Search product descriptions and links. One bounded SRU page. No entitlement or complete municipal coverage claim; enforce collection before activation.",
      "category": "government",
      "input_contract": {
        "type": "object",
        "properties": {
          "query": {
            "type": "string",
            "minLength": 1,
            "description": "CQL scoped to c.product-area==samenwerkendecatalogi; collection enforcement must be implemented before activation."
          },
          "startRecord": {
            "type": "integer",
            "minimum": 1
          },
          "maximumRecords": {
            "type": "integer",
            "minimum": 1,
            "maximum": 25
          }
        },
        "required": [
          "query"
        ]
      },
      "output_contract": {
        "type": "string",
        "description": "Raw SRU XML. Executor parsing and normalized response contract are pending."
      }
    }
  ],
  "endpoints": [
    {
      "key": "search",
      "method": "GET",
      "path": "/sru",
      "price": 0,
      "description": "Zoek gemeentelijke en landelijke producten en diensten",
      "query_params": {
        "type": "object",
        "properties": {
          "query": {
            "type": "string",
            "minLength": 1,
            "description": "CQL scoped to c.product-area==samenwerkendecatalogi; collection enforcement must be implemented before activation."
          },
          "startRecord": {
            "type": "integer",
            "minimum": 1
          },
          "maximumRecords": {
            "type": "integer",
            "minimum": 1,
            "maximum": 25
          }
        },
        "required": [
          "query"
        ]
      },
      "request_body": null,
      "response_body": {
        "type": "string",
        "description": "Raw SRU XML. Executor parsing and normalized response contract are pending."
      },
      "request_example": {
        "query": "c.product-area==samenwerkendecatalogi AND dt.title=bijstand",
        "maximumRecords": 2
      },
      "response_example": null,
      "headers": {
        "Accept": {
          "type": "string",
          "description": "text/html or application/xml"
        }
      },
      "documentation_markdown": "Zoek gemeentelijke en landelijke producten en diensten\n\nSearch product descriptions and links. One bounded SRU page. No entitlement or complete municipal coverage claim; enforce collection before activation.\n\nRegistered on 2026-10-13. Inactive; no successful Apiosk execution is claimed. Price awaits review.",
      "external_docs_url": "https://data.overheid.nl/sites/default/files/dataset/d0cca537-44ea-48cf-9880-fa21e1a7058f/resources/HandleidingSRU2.0.pdf",
      "authentication_notes": "Public read access.",
      "origin_override": null,
      "cache_ttl_seconds": 86400,
      "tags": [
        "gemeenten",
        "Samenwerkende Catalogi",
        "bijzondere bijstand",
        "inkomenstoeslag",
        "aanvragen",
        "lokale regelingen"
      ],
      "search_terms": [
        "gemeenten",
        "Samenwerkende Catalogi",
        "bijzondere bijstand",
        "inkomenstoeslag",
        "aanvragen",
        "lokale regelingen",
        "Zoek gemeentelijke en landelijke producten en diensten"
      ],
      "use_cases": [
        "Zoek gemeentelijke en landelijke producten en diensten"
      ],
      "sectors": [
        "Government and public data"
      ],
      "input_entities": [
        "query",
        "startRecord",
        "maximumRecords"
      ],
      "output_entities": [
        "nl.municipal.services.search.response"
      ],
      "endpoint_category": "government",
      "action_type": "lookup",
      "task_summary": "Zoek gemeentelijke en landelijke producten en diensten",
      "metadata_source": "provider_documentation",
      "metadata_model": "codex",
      "metadata_confidence": 0.9,
      "metadata_at": "2026-10-13T17:13:07.262800+00:00",
      "contract_documented": false
    }
  ],
  "contracts": [
    {
      "endpoint_key": "search",
      "capability_slug": "nl.municipal.services.search",
      "version": 1,
      "requires": [
        {
          "fact_type": "nl.municipal.services.search.input.query",
          "subject_role": "subject",
          "optional": false
        },
        {
          "fact_type": "nl.municipal.services.search.input.startrecord",
          "subject_role": "subject",
          "optional": true
        },
        {
          "fact_type": "nl.municipal.services.search.input.maximumrecords",
          "subject_role": "subject",
          "optional": true
        }
      ],
      "produces": [
        {
          "fact_type": "nl.municipal.services.search.response",
          "subject_role": "subject",
          "cardinality": "one"
        }
      ],
      "input_map": {
        "nl.municipal.services.search.input.query": {
          "in": "query",
          "name": "query"
        },
        "nl.municipal.services.search.input.startrecord": {
          "in": "query",
          "name": "startRecord"
        },
        "nl.municipal.services.search.input.maximumrecords": {
          "in": "query",
          "name": "maximumRecords"
        }
      },
      "fixed_params": {
        "version": "2.0",
        "operation": "searchRetrieve"
      },
      "output_map": {
        "base": "$",
        "produces": {
          "nl.municipal.services.search.response": {
            "path": "$"
          }
        }
      },
      "cardinality": "one",
      "max_fan_out": 1,
      "preconditions": {},
      "pagination": {},
      "effect": "read",
      "price_model": "per_call",
      "freshness_seconds": 86400,
      "adapter_version": "pending-nl-benefits-v1",
      "notes": "DRAFT: Search product descriptions and links. One bounded SRU page. No entitlement or complete municipal coverage claim; enforce collection before activation. Requires a verified executor response shape and live extraction before activation."
    }
  ],
  "offers": [
    {
      "endpoint_key": "search",
      "capability_slug": "nl.municipal.services.search",
      "source": "declared",
      "confidence": 0.9,
      "input_map": {
        "query": {
          "in": "query",
          "name": "query"
        },
        "startRecord": {
          "in": "query",
          "name": "startRecord"
        },
        "maximumRecords": {
          "in": "query",
          "name": "maximumRecords"
        }
      },
      "fixed_params": {
        "version": "2.0",
        "operation": "searchRetrieve"
      },
      "notes": "Documentation-backed discovery link; endpoint and execution contract remain inactive/draft."
    }
  ]
}$bundle_74138553edd806cd1a4053ffb5b287bda8716898230c085a5c0235a793f4fc3b$::jsonb;
  src jsonb;
  item jsonb;
  requirement jsonb;
  source_id uuid;
  endpoint_id uuid;
  endpoint_ids jsonb := '{}'::jsonb;
  endpoint_row providers.api_endpoints%ROWTYPE;
  contract_row catalog.capability_execution_contracts%ROWTYPE;
  credential_values jsonb := coalesce(nullif(current_setting('apiosk.onboarding_secrets', true), ''), '{}')::jsonb;
  secret_name text;
  fact_slug text;
  reused jsonb;
BEGIN
  IF bundle->>'format' IS DISTINCT FROM 'apiosk.source-bundle.v1' THEN
    RAISE EXCEPTION 'Unsupported source bundle format';
  END IF;
  src := bundle->'source';
  IF jsonb_typeof(src) IS DISTINCT FROM 'object'
     OR coalesce(src->>'slug','') !~ '^[a-z0-9]+(-[a-z0-9]+)*$'
     OR nullif(btrim(src->>'name'),'') IS NULL
     OR nullif(btrim(src->>'description'),'') IS NULL
     OR coalesce(src->>'origin_url','') !~ '^https://[^/@?#]+(:[0-9]+)?(/[^?#]*)?$'
     OR coalesce(src->>'docs_url','') !~ '^https://'
     OR nullif(src->>'category','') IS NULL
     OR src->>'owner_id' IS NULL THEN
    RAISE EXCEPTION 'Source identity, existing owner UUID, HTTPS origin/docs and category are required';
  END IF;
  IF NOT EXISTS (SELECT 1 FROM auth.users WHERE id=(src->>'owner_id')::uuid) THEN
    RAISE EXCEPTION 'Source owner must be an existing auth.users ID';
  END IF;
  IF jsonb_typeof(bundle->'endpoints') IS DISTINCT FROM 'array'
     OR jsonb_array_length(bundle->'endpoints')=0 THEN
    RAISE EXCEPTION 'At least one endpoint is required';
  END IF;
  IF jsonb_typeof(credential_values) IS DISTINCT FROM 'object' THEN
    RAISE EXCEPTION 'Credential input must be a server-bound JSON object';
  END IF;
  IF jsonb_typeof(src->'listing_metadata') IS DISTINCT FROM 'object'
     OR jsonb_typeof(src->'authentication_info') IS DISTINCT FROM 'object'
     OR jsonb_typeof(src->'static_headers') IS DISTINCT FROM 'array' THEN
    RAISE EXCEPTION 'Source metadata/authentication must be objects and static_headers an array';
  END IF;
  -- Serializes this importer for the same slug; the unique constraint also
  -- protects against other writers. Existing sources require a separate update.
  PERFORM pg_advisory_xact_lock(hashtextextended('source-bundle:'||(src->>'slug'), 0));
  IF EXISTS (SELECT 1 FROM providers.apis WHERE slug=src->>'slug') THEN
    RAISE EXCEPTION 'Source slug already exists; inspect prior import instead of overwriting';
  END IF;
  IF coalesce(src->'listing_metadata','{}') ?| ARRAY['auth_headers','auth_query','planner_block_reason','onboarding'] THEN
    RAISE EXCEPTION 'Use static_headers/credential_requirements; reserved metadata cannot be supplied';
  END IF;
  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(src->'static_headers','[]')) LOOP
    -- These are public protocol settings. Credentials have a separate channel.
    IF lower(coalesce(item->>'name','')) NOT IN ('accept','content-type','accept-language')
       OR nullif(item->>'value','') IS NULL OR (item->>'value') ~ '[\r\n]' THEN
      RAISE EXCEPTION 'Only public Accept, Content-Type and Accept-Language static headers are supported by this template';
    END IF;
  END LOOP;

  INSERT INTO providers.apis (
    owner_id,slug,name,origin_url,endpoint_url,description,long_description,
    category,docs_url,website_url,github_url,example_usage,authentication_info,
    status,active,discoverable,is_verified,listing_type,is_platform_owned,
    is_external,is_community,source,integration_mode,settlement_mode,
    requires_payout_wallet,listing_metadata
  ) VALUES (
    (src->>'owner_id')::uuid,src->>'slug',src->>'name',src->>'origin_url',src->>'origin_url',
    src->>'description',src->>'long_description',src->>'category',src->>'docs_url',
    src->>'website_url',src->>'github_url',src->>'example_usage',src->'authentication_info',
    'inactive',false,false,false,'api',true,false,false,'manual','gateway','apiosk_revenue',false,
    coalesce(src->'listing_metadata','{}') || jsonb_build_object(
      'auth_headers',coalesce(src->'static_headers','[]'),
      'planner_block_reason','Onboarding: credentials, runtime contract and live acceptance await review',
      'onboarding',jsonb_build_object('format',bundle->>'format','bundle_id',bundle->>'bundle_id',
        'provenance',bundle->'provenance','credential_requirements',coalesce(bundle->'credential_requirements','[]'),
        'state','staged'))
  ) RETURNING id INTO source_id;
  -- Production's owner trigger auto-verifies on INSERT. Explicitly reset this
  -- metadata so staging does not imply that an upstream test was successful.
  UPDATE providers.apis SET is_verified=false,verified_at=NULL WHERE id=source_id;

  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(bundle->'subject_kinds','[]')) LOOP
    INSERT INTO catalog.fact_subject_kinds(slug,description)
    VALUES(item->>'slug',item->>'description') ON CONFLICT(slug) DO NOTHING;
  END LOOP;

  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(bundle->'fact_types','[]')) LOOP
    INSERT INTO catalog.fact_types(slug,name,description,subject_kind,jurisdiction,
      value_schema,freshness_seconds,match_hints,status)
    VALUES(item->>'slug',item->>'name',item->>'description',item->>'subject_kind',item->>'jurisdiction',
      item->'value_schema',(item->>'freshness_seconds')::integer,coalesce(item->'match_hints','{}'),'draft')
    ON CONFLICT(slug) DO NOTHING;
    -- Global vocabulary is shared by existing sources. Never redefine it.
    SELECT jsonb_build_object('subject_kind',f.subject_kind,'jurisdiction',f.jurisdiction,
      'value_schema',f.value_schema,'freshness_seconds',f.freshness_seconds,'match_hints',f.match_hints)
      INTO reused FROM catalog.fact_types f WHERE f.slug=item->>'slug';
    IF reused IS DISTINCT FROM jsonb_build_object('subject_kind',item->>'subject_kind',
      'jurisdiction',item->>'jurisdiction','value_schema',item->'value_schema',
      'freshness_seconds',(item->>'freshness_seconds')::integer,'match_hints',coalesce(item->'match_hints','{}')) THEN
      RAISE EXCEPTION 'Shared fact definition differs: %',item->>'slug';
    END IF;
  END LOOP;

  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(bundle->'capabilities','[]')) LOOP
    INSERT INTO catalog.capabilities(slug,name,description,category,input_contract,output_contract,status)
    VALUES(item->>'slug',item->>'name',item->>'description',item->>'category',
      coalesce(item->'input_contract','{}'),coalesce(item->'output_contract','{}'),'draft')
    ON CONFLICT(slug) DO NOTHING;
    SELECT jsonb_build_object('name',c.name,'description',c.description,'category',c.category,
      'input_contract',c.input_contract,'output_contract',c.output_contract)
      INTO reused FROM catalog.capabilities c WHERE c.slug=item->>'slug';
    IF reused IS DISTINCT FROM jsonb_build_object('name',item->>'name','description',item->>'description',
      'category',item->>'category','input_contract',coalesce(item->'input_contract','{}'),
      'output_contract',coalesce(item->'output_contract','{}')) THEN
      RAISE EXCEPTION 'Shared capability definition differs: %',item->>'slug';
    END IF;
  END LOOP;

  FOR item IN SELECT value FROM jsonb_array_elements(bundle->'endpoints') LOOP
    IF nullif(item->>'key','') IS NULL OR endpoint_ids ? (item->>'key') THEN
      RAISE EXCEPTION 'Each endpoint needs a unique bundle-local key';
    END IF;
    endpoint_row := jsonb_populate_record(NULL::providers.api_endpoints,item-'key');
    IF endpoint_row.method NOT IN ('GET','POST') OR endpoint_row.method IS NULL
       OR coalesce(endpoint_row.path,'') !~ '^/[^?#]*$' OR endpoint_row.path LIKE '//%'
       OR endpoint_row.price IS NULL OR endpoint_row.price<0
       OR endpoint_row.price<>round(endpoint_row.price,6)
       OR nullif(btrim(endpoint_row.description),'') IS NULL
       OR endpoint_row.auth_override IS NOT NULL THEN
      RAISE EXCEPTION 'Invalid endpoint: require read GET/POST, relative path, micro-USD precision, description; secret auth_override is unsupported';
    END IF;
    IF endpoint_row.origin_override IS NOT NULL AND endpoint_row.origin_override !~ '^https://[^/@?#]+(:[0-9]+)?(/[^?#]*)?$' THEN
      RAISE EXCEPTION 'Origin override must be a reviewed HTTPS origin without credentials/query/fragment';
    END IF;
    IF jsonb_typeof(endpoint_row.query_params->'properties') IS DISTINCT FROM 'object'
       AND jsonb_typeof(endpoint_row.request_body->'properties') IS DISTINCT FROM 'object' THEN
      RAISE EXCEPTION 'Endpoint must document its inputs, including properties={} for no inputs';
    END IF;
    INSERT INTO providers.api_endpoints (
      api_id,method,path,payment_required,price,description,request_body,response_body,
      request_example,response_example,headers,query_params,active,documentation_markdown,
      external_docs_url,openapi_url,authentication_notes,origin_override,cache_ttl_seconds,
      tags,search_terms,use_cases,sectors,input_entities,output_entities,endpoint_category,
      action_type,task_summary,metadata_source,metadata_model,metadata_confidence,metadata_at,contract_documented
    ) VALUES (
      source_id,endpoint_row.method,endpoint_row.path,endpoint_row.price>0,endpoint_row.price,
      endpoint_row.description,endpoint_row.request_body,endpoint_row.response_body,
      endpoint_row.request_example,endpoint_row.response_example,endpoint_row.headers,
      endpoint_row.query_params,false,endpoint_row.documentation_markdown,
      endpoint_row.external_docs_url,endpoint_row.openapi_url,endpoint_row.authentication_notes,
      endpoint_row.origin_override,endpoint_row.cache_ttl_seconds,endpoint_row.tags,
      endpoint_row.search_terms,endpoint_row.use_cases,endpoint_row.sectors,
      endpoint_row.input_entities,endpoint_row.output_entities,endpoint_row.endpoint_category,
      endpoint_row.action_type,endpoint_row.task_summary,endpoint_row.metadata_source,
      endpoint_row.metadata_model,endpoint_row.metadata_confidence,endpoint_row.metadata_at,
      coalesce(endpoint_row.contract_documented,false)
    ) RETURNING id INTO endpoint_id;
    endpoint_ids := endpoint_ids || jsonb_build_object(item->>'key',endpoint_id);
  END LOOP;

  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(bundle->'contracts','[]')) LOOP
    endpoint_id := (endpoint_ids->>(item->>'endpoint_key'))::uuid;
    IF endpoint_id IS NULL THEN RAISE EXCEPTION 'Unknown contract endpoint_key'; END IF;
    contract_row := jsonb_populate_record(NULL::catalog.capability_execution_contracts,item-'endpoint_key');
    IF coalesce(item->>'effect','read')<>'read' OR coalesce(item->>'price_model','per_call')<>'per_call'
       OR coalesce(item->'preconditions','{}')<>'{}'::jsonb THEN
      RAISE EXCEPTION 'Current Ask runtime requires read/per_call and empty preconditions';
    END IF;
    IF jsonb_typeof(item->'requires') IS DISTINCT FROM 'array'
       OR jsonb_typeof(item->'produces') IS DISTINCT FROM 'array'
       OR jsonb_typeof(item->'input_map') IS DISTINCT FROM 'object'
       OR jsonb_typeof(item->'output_map'->'produces') IS DISTINCT FROM 'object'
       OR jsonb_array_length(item->'produces')=0 THEN
      RAISE EXCEPTION 'Contract requires typed requires/produces and input/output maps';
    END IF;
    FOR requirement IN SELECT value FROM jsonb_array_elements((item->'requires')||(item->'produces')) LOOP
      fact_slug := requirement->>'fact_type';
      IF requirement->>'subject_role' IS DISTINCT FROM 'subject'
         OR NOT EXISTS(SELECT 1 FROM catalog.fact_types WHERE slug=fact_slug) THEN
        RAISE EXCEPTION 'Unknown fact or unsupported subject role: %',fact_slug;
      END IF;
    END LOOP;
    IF EXISTS(SELECT 1 FROM jsonb_array_elements(item->'requires') r
      WHERE NOT (item->'input_map' ? (r->>'fact_type')))
      OR EXISTS(SELECT 1 FROM jsonb_array_elements(item->'produces') r
      WHERE NOT (item->'output_map'->'produces' ? (r->>'fact_type'))) THEN
      RAISE EXCEPTION 'Every declared input/output requires a mapping';
    END IF;
    INSERT INTO catalog.capability_execution_contracts (
      capability_slug,endpoint_id,version,status,requires,produces,input_map,fixed_params,
      output_map,cardinality,identifies,max_fan_out,preconditions,pagination,effect,price_model,
      freshness_seconds,adapter_version,sample_payload,notes
    ) VALUES (
      contract_row.capability_slug,endpoint_id,coalesce(contract_row.version,1),'draft',
      contract_row.requires,contract_row.produces,contract_row.input_map,
      coalesce(contract_row.fixed_params,'{}'),contract_row.output_map,
      coalesce(contract_row.cardinality,'one'),contract_row.identifies,
      coalesce(contract_row.max_fan_out,1),'{}',coalesce(contract_row.pagination,'{}'),
      'read','per_call',contract_row.freshness_seconds,coalesce(contract_row.adapter_version,'v1'),
      contract_row.sample_payload,contract_row.notes
    );
  END LOOP;

  -- Optional legacy discovery links. These do NOT authorize execution.
  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(bundle->'offers','[]')) LOOP
    endpoint_id := (endpoint_ids->>(item->>'endpoint_key'))::uuid;
    IF endpoint_id IS NULL THEN RAISE EXCEPTION 'Unknown offer endpoint_key'; END IF;
    INSERT INTO catalog.capability_offers(capability_slug,endpoint_id,api_id,source,confidence,
      input_map,fixed_params,output_map,notes)
    VALUES(item->>'capability_slug',endpoint_id,source_id,coalesce(item->>'source','inferred'),
      coalesce((item->>'confidence')::numeric,0),coalesce(item->'input_map','{}'),
      coalesce(item->'fixed_params','{}'),item->'output_map',item->>'notes');
  END LOOP;

  -- Optional atomic Vault provisioning: values arrive only through a bound
  -- server connection, not the AI manifest. No existing key is overwritten.
  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(bundle->'credential_requirements','[]')) LOOP
    secret_name := item->>'vault_name';
    IF coalesce(secret_name,'') !~ '^[A-Z][A-Z0-9_]+$' THEN RAISE EXCEPTION 'Invalid Vault name'; END IF;
    IF item->>'endpoint_key' IS NOT NULL AND NOT endpoint_ids ? (item->>'endpoint_key') THEN
      RAISE EXCEPTION 'Unknown credential endpoint_key';
    END IF;
    IF credential_values ? secret_name THEN
      IF jsonb_typeof(credential_values->secret_name)<>'string'
         OR nullif(btrim(credential_values->>secret_name),'') IS NULL THEN
        RAISE EXCEPTION 'Credential must be a nonempty string';
      END IF;
      IF EXISTS(SELECT 1 FROM vault.secrets WHERE name=secret_name) THEN
        RAISE EXCEPTION 'Vault name already exists; key rotation requires a separate explicit operation';
      END IF;
      PERFORM vault.create_secret(credential_values->>secret_name,secret_name,
        'Apiosk source onboarding: '||(src->>'slug')||'; runtime binding must be implemented and tested');
    END IF;
  END LOOP;
  IF EXISTS(SELECT 1 FROM jsonb_object_keys(credential_values) k WHERE NOT EXISTS(
    SELECT 1 FROM jsonb_array_elements(coalesce(bundle->'credential_requirements','[]')) r WHERE r->>'vault_name'=k)) THEN
    RAISE EXCEPTION 'Credential input contains an undeclared Vault name';
  END IF;
  PERFORM set_config('apiosk.onboarding_secrets','',true);
  RAISE NOTICE 'Source % registered inactive with % endpoints; contracts remain draft. No live execution has been certified.',src->>'slug',jsonb_array_length(bundle->'endpoints');
END;
$block_74138553edd806cd1a4053ffb5b287bda8716898230c085a5c0235a793f4fc3b$;


-- Apiosk source bundle v1. Schema inspected in production on 2026-10-11.
-- TEMPLATE: render with render.mjs and a reviewed manifest. Not an auto-applied migration.
-- One transaction registers ONE NEW source, all endpoints, vocabulary and contracts.
-- The source remains inactive. See README.txt for the separate publication contract.
-- Never embed real credentials in this file, its manifest, examples or AI input.
-- Optional backend credential input on the SAME connection, BEFORE this script:
--   SELECT set_config('apiosk.onboarding_secrets', $1, false);
-- Bind $1 to a JSON object {"VAULT_NAME":"secret"}; do not interpolate SQL.
-- Use a dedicated connection and clear the setting/discard connection in finally.
-- Vault storage does NOT implement a generic gateway credential resolver.

SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '30s';
SET LOCAL standard_conforming_strings = on;

DO $block_144025cc10b5398ca0b62c8a78ec83d01f9f2d4f0d81454d982b34d6e4a10070$
DECLARE
  bundle jsonb := $bundle_144025cc10b5398ca0b62c8a78ec83d01f9f2d4f0d81454d982b34d6e4a10070${
  "format": "apiosk.source-bundle.v1",
  "bundle_id": "91b47c46-c3ef-47f7-829e-683c3c37cb3d",
  "provenance": {
    "model": "codex",
    "generated_at": "2026-10-13T17:13:07.262800+00:00",
    "documentation_urls": [
      "https://www.belastingdienst.nl/wps/wcm/connect/nl/toeslagen/toeslagen"
    ],
    "reviewer": null,
    "live_test": {
      "status": "not_run",
      "notes": "Documentation verification is not an Apiosk execution test."
    }
  },
  "source": {
    "owner_id": "bf085fe8-41c4-4699-978f-e75e89518bf7",
    "slug": "dienst-toeslagen",
    "name": "Dienst Toeslagen — voorwaarden en tabellen",
    "origin_url": "https://www.belastingdienst.nl",
    "description": "Officiële publieke informatie over zorgtoeslag, huurtoeslag, kinderopvangtoeslag en kindgebonden budget. HTML-bronnen; geen bevestigde openbare berekenings-API en geen persoonlijke aanspraakberekening.",
    "long_description": "Officiële publieke informatie over zorgtoeslag, huurtoeslag, kinderopvangtoeslag en kindgebonden budget. HTML-bronnen; geen bevestigde openbare berekenings-API en geen persoonlijke aanspraakberekening.",
    "category": "government",
    "docs_url": "https://www.belastingdienst.nl/wps/wcm/connect/nl/toeslagen/toeslagen",
    "website_url": "https://www.belastingdienst.nl/wps/wcm/connect/nl/toeslagen/toeslagen",
    "authentication_info": {
      "type": "none"
    },
    "static_headers": [
      {
        "name": "Accept",
        "value": "text/html"
      }
    ],
    "listing_metadata": {
      "tags": [
        "toeslagen",
        "zorgtoeslag",
        "huurtoeslag",
        "kinderopvangtoeslag",
        "kindgebonden budget",
        "voorwaarden"
      ],
      "sectors": [
        "Government and public data"
      ],
      "jurisdiction": "NL",
      "language": "nl",
      "coverage_notice": "Officiële publieke informatie over zorgtoeslag, huurtoeslag, kinderopvangtoeslag en kindgebonden budget. HTML-bronnen; geen bevestigde openbare berekenings-API en geen persoonlijke aanspraakberekening.",
      "readiness": {
        "state": "staged",
        "blockers": [
          "HTML extraction and source provenance",
          "Year-specific content/version checks",
          "Apiosk execution acceptance",
          "Listing price review"
        ]
      },
      "pricing": {
        "state": "unpriced",
        "upstream_cost_usd": null,
        "listing_price_note": "Zero is a required database placeholder for inactive endpoints, not a free production offer."
      },
      "operator_notice": "Apiosk integration; not affiliated with the publisher.",
      "eligibility": {
        "state": "not_implemented",
        "public_calculation_api_confirmed": false,
        "requirements": [
          "Year-versioned parameters",
          "Validated calculation rules",
          "Comparison with official trial calculations"
        ]
      }
    }
  },
  "credential_requirements": [],
  "subject_kinds": [
    {
      "slug": "dataset",
      "description": "Dataset or source response"
    }
  ],
  "fact_types": [
    {
      "slug": "nl.benefits.zorgtoeslag.information.response",
      "name": "Lees officiële zorgtoeslag informatie",
      "description": "Public HTML information only. Preserve source URL, retrieval date and applicable year. No entitlement calculation, login, application submission or personal data.",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": "string",
        "description": "Official HTML page; not a JSON benefits calculator."
      },
      "freshness_seconds": 86400,
      "match_hints": {}
    },
    {
      "slug": "nl.benefits.huurtoeslag.information.response",
      "name": "Lees officiële huurtoeslag informatie",
      "description": "Public HTML information only. Preserve source URL, retrieval date and applicable year. No entitlement calculation, login, application submission or personal data.",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": "string",
        "description": "Official HTML page; not a JSON benefits calculator."
      },
      "freshness_seconds": 86400,
      "match_hints": {}
    },
    {
      "slug": "nl.benefits.kinderopvangtoeslag.information.response",
      "name": "Lees officiële kinderopvangtoeslag informatie",
      "description": "Public HTML information only. Preserve source URL, retrieval date and applicable year. No entitlement calculation, login, application submission or personal data.",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": "string",
        "description": "Official HTML page; not a JSON benefits calculator."
      },
      "freshness_seconds": 86400,
      "match_hints": {}
    },
    {
      "slug": "nl.benefits.kindgebonden-budget.information.response",
      "name": "Lees officiële kindgebonden budget informatie",
      "description": "Public HTML information only. Preserve source URL, retrieval date and applicable year. No entitlement calculation, login, application submission or personal data.",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": "string",
        "description": "Official HTML page; not a JSON benefits calculator."
      },
      "freshness_seconds": 86400,
      "match_hints": {}
    },
    {
      "slug": "nl.benefits.zorgtoeslag-tabellen.information.response",
      "name": "Lees officiële zorgtoeslagtabellen informatie",
      "description": "Public HTML information only. Preserve source URL, retrieval date and applicable year. No entitlement calculation, login, application submission or personal data.",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": "string",
        "description": "Official HTML page; not a JSON benefits calculator."
      },
      "freshness_seconds": 86400,
      "match_hints": {}
    }
  ],
  "capabilities": [
    {
      "slug": "nl.benefits.zorgtoeslag.information",
      "name": "Lees officiële zorgtoeslag informatie",
      "description": "Public HTML information only. Preserve source URL, retrieval date and applicable year. No entitlement calculation, login, application submission or personal data.",
      "category": "government",
      "input_contract": {
        "type": "object",
        "properties": {}
      },
      "output_contract": {
        "type": "string",
        "description": "Official HTML page; not a JSON benefits calculator."
      }
    },
    {
      "slug": "nl.benefits.huurtoeslag.information",
      "name": "Lees officiële huurtoeslag informatie",
      "description": "Public HTML information only. Preserve source URL, retrieval date and applicable year. No entitlement calculation, login, application submission or personal data.",
      "category": "government",
      "input_contract": {
        "type": "object",
        "properties": {}
      },
      "output_contract": {
        "type": "string",
        "description": "Official HTML page; not a JSON benefits calculator."
      }
    },
    {
      "slug": "nl.benefits.kinderopvangtoeslag.information",
      "name": "Lees officiële kinderopvangtoeslag informatie",
      "description": "Public HTML information only. Preserve source URL, retrieval date and applicable year. No entitlement calculation, login, application submission or personal data.",
      "category": "government",
      "input_contract": {
        "type": "object",
        "properties": {}
      },
      "output_contract": {
        "type": "string",
        "description": "Official HTML page; not a JSON benefits calculator."
      }
    },
    {
      "slug": "nl.benefits.kindgebonden-budget.information",
      "name": "Lees officiële kindgebonden budget informatie",
      "description": "Public HTML information only. Preserve source URL, retrieval date and applicable year. No entitlement calculation, login, application submission or personal data.",
      "category": "government",
      "input_contract": {
        "type": "object",
        "properties": {}
      },
      "output_contract": {
        "type": "string",
        "description": "Official HTML page; not a JSON benefits calculator."
      }
    },
    {
      "slug": "nl.benefits.zorgtoeslag-tabellen.information",
      "name": "Lees officiële zorgtoeslagtabellen informatie",
      "description": "Public HTML information only. Preserve source URL, retrieval date and applicable year. No entitlement calculation, login, application submission or personal data.",
      "category": "government",
      "input_contract": {
        "type": "object",
        "properties": {}
      },
      "output_contract": {
        "type": "string",
        "description": "Official HTML page; not a JSON benefits calculator."
      }
    }
  ],
  "endpoints": [
    {
      "key": "zorgtoeslag",
      "method": "GET",
      "path": "/wps/wcm/connect/nl/zorgtoeslag/zorgtoeslag",
      "price": 0,
      "description": "Lees officiële zorgtoeslag informatie",
      "query_params": {
        "type": "object",
        "properties": {}
      },
      "request_body": null,
      "response_body": {
        "type": "string",
        "description": "Official HTML page; not a JSON benefits calculator."
      },
      "request_example": null,
      "response_example": null,
      "headers": {
        "Accept": {
          "type": "string",
          "description": "text/html or application/xml"
        }
      },
      "documentation_markdown": "Lees officiële zorgtoeslag informatie\n\nPublic HTML information only. Preserve source URL, retrieval date and applicable year. No entitlement calculation, login, application submission or personal data.\n\nRegistered on 2026-10-13. Inactive; no successful Apiosk execution is claimed. Price awaits review.",
      "external_docs_url": "https://www.belastingdienst.nl/wps/wcm/connect/nl/zorgtoeslag/zorgtoeslag",
      "authentication_notes": "Public read access.",
      "origin_override": null,
      "cache_ttl_seconds": 86400,
      "tags": [
        "Dienst Toeslagen",
        "Zorgtoeslag",
        "voorwaarden",
        "bedragen",
        "inkomensgrenzen",
        "Nederland"
      ],
      "search_terms": [
        "Dienst Toeslagen",
        "Zorgtoeslag",
        "voorwaarden",
        "bedragen",
        "inkomensgrenzen",
        "Nederland",
        "Lees officiële zorgtoeslag informatie"
      ],
      "use_cases": [
        "Lees officiële zorgtoeslag informatie"
      ],
      "sectors": [
        "Government and public data"
      ],
      "input_entities": [],
      "output_entities": [
        "nl.benefits.zorgtoeslag.information.response"
      ],
      "endpoint_category": "government",
      "action_type": "lookup",
      "task_summary": "Lees officiële zorgtoeslag informatie",
      "metadata_source": "provider_documentation",
      "metadata_model": "codex",
      "metadata_confidence": 0.9,
      "metadata_at": "2026-10-13T17:13:07.262800+00:00",
      "contract_documented": false
    },
    {
      "key": "huurtoeslag",
      "method": "GET",
      "path": "/wps/wcm/connect/nl/huurtoeslag/huurtoeslag",
      "price": 0,
      "description": "Lees officiële huurtoeslag informatie",
      "query_params": {
        "type": "object",
        "properties": {}
      },
      "request_body": null,
      "response_body": {
        "type": "string",
        "description": "Official HTML page; not a JSON benefits calculator."
      },
      "request_example": null,
      "response_example": null,
      "headers": {
        "Accept": {
          "type": "string",
          "description": "text/html or application/xml"
        }
      },
      "documentation_markdown": "Lees officiële huurtoeslag informatie\n\nPublic HTML information only. Preserve source URL, retrieval date and applicable year. No entitlement calculation, login, application submission or personal data.\n\nRegistered on 2026-10-13. Inactive; no successful Apiosk execution is claimed. Price awaits review.",
      "external_docs_url": "https://www.belastingdienst.nl/wps/wcm/connect/nl/huurtoeslag/huurtoeslag",
      "authentication_notes": "Public read access.",
      "origin_override": null,
      "cache_ttl_seconds": 86400,
      "tags": [
        "Dienst Toeslagen",
        "Huurtoeslag",
        "voorwaarden",
        "bedragen",
        "inkomensgrenzen",
        "Nederland"
      ],
      "search_terms": [
        "Dienst Toeslagen",
        "Huurtoeslag",
        "voorwaarden",
        "bedragen",
        "inkomensgrenzen",
        "Nederland",
        "Lees officiële huurtoeslag informatie"
      ],
      "use_cases": [
        "Lees officiële huurtoeslag informatie"
      ],
      "sectors": [
        "Government and public data"
      ],
      "input_entities": [],
      "output_entities": [
        "nl.benefits.huurtoeslag.information.response"
      ],
      "endpoint_category": "government",
      "action_type": "lookup",
      "task_summary": "Lees officiële huurtoeslag informatie",
      "metadata_source": "provider_documentation",
      "metadata_model": "codex",
      "metadata_confidence": 0.9,
      "metadata_at": "2026-10-13T17:13:07.262800+00:00",
      "contract_documented": false
    },
    {
      "key": "kinderopvangtoeslag",
      "method": "GET",
      "path": "/wps/wcm/connect/nl/kinderopvangtoeslag/kinderopvangtoeslag",
      "price": 0,
      "description": "Lees officiële kinderopvangtoeslag informatie",
      "query_params": {
        "type": "object",
        "properties": {}
      },
      "request_body": null,
      "response_body": {
        "type": "string",
        "description": "Official HTML page; not a JSON benefits calculator."
      },
      "request_example": null,
      "response_example": null,
      "headers": {
        "Accept": {
          "type": "string",
          "description": "text/html or application/xml"
        }
      },
      "documentation_markdown": "Lees officiële kinderopvangtoeslag informatie\n\nPublic HTML information only. Preserve source URL, retrieval date and applicable year. No entitlement calculation, login, application submission or personal data.\n\nRegistered on 2026-10-13. Inactive; no successful Apiosk execution is claimed. Price awaits review.",
      "external_docs_url": "https://www.belastingdienst.nl/wps/wcm/connect/nl/kinderopvangtoeslag/kinderopvangtoeslag",
      "authentication_notes": "Public read access.",
      "origin_override": null,
      "cache_ttl_seconds": 86400,
      "tags": [
        "Dienst Toeslagen",
        "Kinderopvangtoeslag",
        "voorwaarden",
        "bedragen",
        "inkomensgrenzen",
        "Nederland"
      ],
      "search_terms": [
        "Dienst Toeslagen",
        "Kinderopvangtoeslag",
        "voorwaarden",
        "bedragen",
        "inkomensgrenzen",
        "Nederland",
        "Lees officiële kinderopvangtoeslag informatie"
      ],
      "use_cases": [
        "Lees officiële kinderopvangtoeslag informatie"
      ],
      "sectors": [
        "Government and public data"
      ],
      "input_entities": [],
      "output_entities": [
        "nl.benefits.kinderopvangtoeslag.information.response"
      ],
      "endpoint_category": "government",
      "action_type": "lookup",
      "task_summary": "Lees officiële kinderopvangtoeslag informatie",
      "metadata_source": "provider_documentation",
      "metadata_model": "codex",
      "metadata_confidence": 0.9,
      "metadata_at": "2026-10-13T17:13:07.262800+00:00",
      "contract_documented": false
    },
    {
      "key": "kindgebonden-budget",
      "method": "GET",
      "path": "/wps/wcm/connect/nl/kindgebonden-budget/kindgebonden-budget",
      "price": 0,
      "description": "Lees officiële kindgebonden budget informatie",
      "query_params": {
        "type": "object",
        "properties": {}
      },
      "request_body": null,
      "response_body": {
        "type": "string",
        "description": "Official HTML page; not a JSON benefits calculator."
      },
      "request_example": null,
      "response_example": null,
      "headers": {
        "Accept": {
          "type": "string",
          "description": "text/html or application/xml"
        }
      },
      "documentation_markdown": "Lees officiële kindgebonden budget informatie\n\nPublic HTML information only. Preserve source URL, retrieval date and applicable year. No entitlement calculation, login, application submission or personal data.\n\nRegistered on 2026-10-13. Inactive; no successful Apiosk execution is claimed. Price awaits review.",
      "external_docs_url": "https://www.belastingdienst.nl/wps/wcm/connect/nl/kindgebonden-budget/kindgebonden-budget",
      "authentication_notes": "Public read access.",
      "origin_override": null,
      "cache_ttl_seconds": 86400,
      "tags": [
        "Dienst Toeslagen",
        "Kindgebonden budget",
        "voorwaarden",
        "bedragen",
        "inkomensgrenzen",
        "Nederland"
      ],
      "search_terms": [
        "Dienst Toeslagen",
        "Kindgebonden budget",
        "voorwaarden",
        "bedragen",
        "inkomensgrenzen",
        "Nederland",
        "Lees officiële kindgebonden budget informatie"
      ],
      "use_cases": [
        "Lees officiële kindgebonden budget informatie"
      ],
      "sectors": [
        "Government and public data"
      ],
      "input_entities": [],
      "output_entities": [
        "nl.benefits.kindgebonden-budget.information.response"
      ],
      "endpoint_category": "government",
      "action_type": "lookup",
      "task_summary": "Lees officiële kindgebonden budget informatie",
      "metadata_source": "provider_documentation",
      "metadata_model": "codex",
      "metadata_confidence": 0.9,
      "metadata_at": "2026-10-13T17:13:07.262800+00:00",
      "contract_documented": false
    },
    {
      "key": "zorgtoeslag-tabellen",
      "method": "GET",
      "path": "/wps/wcm/connect/nl/zorgtoeslag/content/hoeveel-zorgtoeslag",
      "price": 0,
      "description": "Lees officiële zorgtoeslagtabellen informatie",
      "query_params": {
        "type": "object",
        "properties": {}
      },
      "request_body": null,
      "response_body": {
        "type": "string",
        "description": "Official HTML page; not a JSON benefits calculator."
      },
      "request_example": null,
      "response_example": null,
      "headers": {
        "Accept": {
          "type": "string",
          "description": "text/html or application/xml"
        }
      },
      "documentation_markdown": "Lees officiële zorgtoeslagtabellen informatie\n\nPublic HTML information only. Preserve source URL, retrieval date and applicable year. No entitlement calculation, login, application submission or personal data.\n\nRegistered on 2026-10-13. Inactive; no successful Apiosk execution is claimed. Price awaits review.",
      "external_docs_url": "https://www.belastingdienst.nl/wps/wcm/connect/nl/zorgtoeslag/content/hoeveel-zorgtoeslag",
      "authentication_notes": "Public read access.",
      "origin_override": null,
      "cache_ttl_seconds": 86400,
      "tags": [
        "Dienst Toeslagen",
        "Zorgtoeslagtabellen",
        "voorwaarden",
        "bedragen",
        "inkomensgrenzen",
        "Nederland"
      ],
      "search_terms": [
        "Dienst Toeslagen",
        "Zorgtoeslagtabellen",
        "voorwaarden",
        "bedragen",
        "inkomensgrenzen",
        "Nederland",
        "Lees officiële zorgtoeslagtabellen informatie"
      ],
      "use_cases": [
        "Lees officiële zorgtoeslagtabellen informatie"
      ],
      "sectors": [
        "Government and public data"
      ],
      "input_entities": [],
      "output_entities": [
        "nl.benefits.zorgtoeslag-tabellen.information.response"
      ],
      "endpoint_category": "government",
      "action_type": "lookup",
      "task_summary": "Lees officiële zorgtoeslagtabellen informatie",
      "metadata_source": "provider_documentation",
      "metadata_model": "codex",
      "metadata_confidence": 0.9,
      "metadata_at": "2026-10-13T17:13:07.262800+00:00",
      "contract_documented": false
    }
  ],
  "contracts": [
    {
      "endpoint_key": "zorgtoeslag",
      "capability_slug": "nl.benefits.zorgtoeslag.information",
      "version": 1,
      "requires": [],
      "produces": [
        {
          "fact_type": "nl.benefits.zorgtoeslag.information.response",
          "subject_role": "subject",
          "cardinality": "one"
        }
      ],
      "input_map": {},
      "fixed_params": {},
      "output_map": {
        "base": "$",
        "produces": {
          "nl.benefits.zorgtoeslag.information.response": {
            "path": "$"
          }
        }
      },
      "cardinality": "one",
      "max_fan_out": 1,
      "preconditions": {},
      "pagination": {},
      "effect": "read",
      "price_model": "per_call",
      "freshness_seconds": 86400,
      "adapter_version": "pending-nl-benefits-v1",
      "notes": "DRAFT: Public HTML information only. Preserve source URL, retrieval date and applicable year. No entitlement calculation, login, application submission or personal data. Requires a verified executor response shape and live extraction before activation."
    },
    {
      "endpoint_key": "huurtoeslag",
      "capability_slug": "nl.benefits.huurtoeslag.information",
      "version": 1,
      "requires": [],
      "produces": [
        {
          "fact_type": "nl.benefits.huurtoeslag.information.response",
          "subject_role": "subject",
          "cardinality": "one"
        }
      ],
      "input_map": {},
      "fixed_params": {},
      "output_map": {
        "base": "$",
        "produces": {
          "nl.benefits.huurtoeslag.information.response": {
            "path": "$"
          }
        }
      },
      "cardinality": "one",
      "max_fan_out": 1,
      "preconditions": {},
      "pagination": {},
      "effect": "read",
      "price_model": "per_call",
      "freshness_seconds": 86400,
      "adapter_version": "pending-nl-benefits-v1",
      "notes": "DRAFT: Public HTML information only. Preserve source URL, retrieval date and applicable year. No entitlement calculation, login, application submission or personal data. Requires a verified executor response shape and live extraction before activation."
    },
    {
      "endpoint_key": "kinderopvangtoeslag",
      "capability_slug": "nl.benefits.kinderopvangtoeslag.information",
      "version": 1,
      "requires": [],
      "produces": [
        {
          "fact_type": "nl.benefits.kinderopvangtoeslag.information.response",
          "subject_role": "subject",
          "cardinality": "one"
        }
      ],
      "input_map": {},
      "fixed_params": {},
      "output_map": {
        "base": "$",
        "produces": {
          "nl.benefits.kinderopvangtoeslag.information.response": {
            "path": "$"
          }
        }
      },
      "cardinality": "one",
      "max_fan_out": 1,
      "preconditions": {},
      "pagination": {},
      "effect": "read",
      "price_model": "per_call",
      "freshness_seconds": 86400,
      "adapter_version": "pending-nl-benefits-v1",
      "notes": "DRAFT: Public HTML information only. Preserve source URL, retrieval date and applicable year. No entitlement calculation, login, application submission or personal data. Requires a verified executor response shape and live extraction before activation."
    },
    {
      "endpoint_key": "kindgebonden-budget",
      "capability_slug": "nl.benefits.kindgebonden-budget.information",
      "version": 1,
      "requires": [],
      "produces": [
        {
          "fact_type": "nl.benefits.kindgebonden-budget.information.response",
          "subject_role": "subject",
          "cardinality": "one"
        }
      ],
      "input_map": {},
      "fixed_params": {},
      "output_map": {
        "base": "$",
        "produces": {
          "nl.benefits.kindgebonden-budget.information.response": {
            "path": "$"
          }
        }
      },
      "cardinality": "one",
      "max_fan_out": 1,
      "preconditions": {},
      "pagination": {},
      "effect": "read",
      "price_model": "per_call",
      "freshness_seconds": 86400,
      "adapter_version": "pending-nl-benefits-v1",
      "notes": "DRAFT: Public HTML information only. Preserve source URL, retrieval date and applicable year. No entitlement calculation, login, application submission or personal data. Requires a verified executor response shape and live extraction before activation."
    },
    {
      "endpoint_key": "zorgtoeslag-tabellen",
      "capability_slug": "nl.benefits.zorgtoeslag-tabellen.information",
      "version": 1,
      "requires": [],
      "produces": [
        {
          "fact_type": "nl.benefits.zorgtoeslag-tabellen.information.response",
          "subject_role": "subject",
          "cardinality": "one"
        }
      ],
      "input_map": {},
      "fixed_params": {},
      "output_map": {
        "base": "$",
        "produces": {
          "nl.benefits.zorgtoeslag-tabellen.information.response": {
            "path": "$"
          }
        }
      },
      "cardinality": "one",
      "max_fan_out": 1,
      "preconditions": {},
      "pagination": {},
      "effect": "read",
      "price_model": "per_call",
      "freshness_seconds": 86400,
      "adapter_version": "pending-nl-benefits-v1",
      "notes": "DRAFT: Public HTML information only. Preserve source URL, retrieval date and applicable year. No entitlement calculation, login, application submission or personal data. Requires a verified executor response shape and live extraction before activation."
    }
  ],
  "offers": [
    {
      "endpoint_key": "zorgtoeslag",
      "capability_slug": "nl.benefits.zorgtoeslag.information",
      "source": "declared",
      "confidence": 0.9,
      "input_map": {},
      "fixed_params": {},
      "notes": "Documentation-backed discovery link; endpoint and execution contract remain inactive/draft."
    },
    {
      "endpoint_key": "huurtoeslag",
      "capability_slug": "nl.benefits.huurtoeslag.information",
      "source": "declared",
      "confidence": 0.9,
      "input_map": {},
      "fixed_params": {},
      "notes": "Documentation-backed discovery link; endpoint and execution contract remain inactive/draft."
    },
    {
      "endpoint_key": "kinderopvangtoeslag",
      "capability_slug": "nl.benefits.kinderopvangtoeslag.information",
      "source": "declared",
      "confidence": 0.9,
      "input_map": {},
      "fixed_params": {},
      "notes": "Documentation-backed discovery link; endpoint and execution contract remain inactive/draft."
    },
    {
      "endpoint_key": "kindgebonden-budget",
      "capability_slug": "nl.benefits.kindgebonden-budget.information",
      "source": "declared",
      "confidence": 0.9,
      "input_map": {},
      "fixed_params": {},
      "notes": "Documentation-backed discovery link; endpoint and execution contract remain inactive/draft."
    },
    {
      "endpoint_key": "zorgtoeslag-tabellen",
      "capability_slug": "nl.benefits.zorgtoeslag-tabellen.information",
      "source": "declared",
      "confidence": 0.9,
      "input_map": {},
      "fixed_params": {},
      "notes": "Documentation-backed discovery link; endpoint and execution contract remain inactive/draft."
    }
  ]
}$bundle_144025cc10b5398ca0b62c8a78ec83d01f9f2d4f0d81454d982b34d6e4a10070$::jsonb;
  src jsonb;
  item jsonb;
  requirement jsonb;
  source_id uuid;
  endpoint_id uuid;
  endpoint_ids jsonb := '{}'::jsonb;
  endpoint_row providers.api_endpoints%ROWTYPE;
  contract_row catalog.capability_execution_contracts%ROWTYPE;
  credential_values jsonb := coalesce(nullif(current_setting('apiosk.onboarding_secrets', true), ''), '{}')::jsonb;
  secret_name text;
  fact_slug text;
  reused jsonb;
BEGIN
  IF bundle->>'format' IS DISTINCT FROM 'apiosk.source-bundle.v1' THEN
    RAISE EXCEPTION 'Unsupported source bundle format';
  END IF;
  src := bundle->'source';
  IF jsonb_typeof(src) IS DISTINCT FROM 'object'
     OR coalesce(src->>'slug','') !~ '^[a-z0-9]+(-[a-z0-9]+)*$'
     OR nullif(btrim(src->>'name'),'') IS NULL
     OR nullif(btrim(src->>'description'),'') IS NULL
     OR coalesce(src->>'origin_url','') !~ '^https://[^/@?#]+(:[0-9]+)?(/[^?#]*)?$'
     OR coalesce(src->>'docs_url','') !~ '^https://'
     OR nullif(src->>'category','') IS NULL
     OR src->>'owner_id' IS NULL THEN
    RAISE EXCEPTION 'Source identity, existing owner UUID, HTTPS origin/docs and category are required';
  END IF;
  IF NOT EXISTS (SELECT 1 FROM auth.users WHERE id=(src->>'owner_id')::uuid) THEN
    RAISE EXCEPTION 'Source owner must be an existing auth.users ID';
  END IF;
  IF jsonb_typeof(bundle->'endpoints') IS DISTINCT FROM 'array'
     OR jsonb_array_length(bundle->'endpoints')=0 THEN
    RAISE EXCEPTION 'At least one endpoint is required';
  END IF;
  IF jsonb_typeof(credential_values) IS DISTINCT FROM 'object' THEN
    RAISE EXCEPTION 'Credential input must be a server-bound JSON object';
  END IF;
  IF jsonb_typeof(src->'listing_metadata') IS DISTINCT FROM 'object'
     OR jsonb_typeof(src->'authentication_info') IS DISTINCT FROM 'object'
     OR jsonb_typeof(src->'static_headers') IS DISTINCT FROM 'array' THEN
    RAISE EXCEPTION 'Source metadata/authentication must be objects and static_headers an array';
  END IF;
  -- Serializes this importer for the same slug; the unique constraint also
  -- protects against other writers. Existing sources require a separate update.
  PERFORM pg_advisory_xact_lock(hashtextextended('source-bundle:'||(src->>'slug'), 0));
  IF EXISTS (SELECT 1 FROM providers.apis WHERE slug=src->>'slug') THEN
    RAISE EXCEPTION 'Source slug already exists; inspect prior import instead of overwriting';
  END IF;
  IF coalesce(src->'listing_metadata','{}') ?| ARRAY['auth_headers','auth_query','planner_block_reason','onboarding'] THEN
    RAISE EXCEPTION 'Use static_headers/credential_requirements; reserved metadata cannot be supplied';
  END IF;
  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(src->'static_headers','[]')) LOOP
    -- These are public protocol settings. Credentials have a separate channel.
    IF lower(coalesce(item->>'name','')) NOT IN ('accept','content-type','accept-language')
       OR nullif(item->>'value','') IS NULL OR (item->>'value') ~ '[\r\n]' THEN
      RAISE EXCEPTION 'Only public Accept, Content-Type and Accept-Language static headers are supported by this template';
    END IF;
  END LOOP;

  INSERT INTO providers.apis (
    owner_id,slug,name,origin_url,endpoint_url,description,long_description,
    category,docs_url,website_url,github_url,example_usage,authentication_info,
    status,active,discoverable,is_verified,listing_type,is_platform_owned,
    is_external,is_community,source,integration_mode,settlement_mode,
    requires_payout_wallet,listing_metadata
  ) VALUES (
    (src->>'owner_id')::uuid,src->>'slug',src->>'name',src->>'origin_url',src->>'origin_url',
    src->>'description',src->>'long_description',src->>'category',src->>'docs_url',
    src->>'website_url',src->>'github_url',src->>'example_usage',src->'authentication_info',
    'inactive',false,false,false,'api',true,false,false,'manual','gateway','apiosk_revenue',false,
    coalesce(src->'listing_metadata','{}') || jsonb_build_object(
      'auth_headers',coalesce(src->'static_headers','[]'),
      'planner_block_reason','Onboarding: credentials, runtime contract and live acceptance await review',
      'onboarding',jsonb_build_object('format',bundle->>'format','bundle_id',bundle->>'bundle_id',
        'provenance',bundle->'provenance','credential_requirements',coalesce(bundle->'credential_requirements','[]'),
        'state','staged'))
  ) RETURNING id INTO source_id;
  -- Production's owner trigger auto-verifies on INSERT. Explicitly reset this
  -- metadata so staging does not imply that an upstream test was successful.
  UPDATE providers.apis SET is_verified=false,verified_at=NULL WHERE id=source_id;

  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(bundle->'subject_kinds','[]')) LOOP
    INSERT INTO catalog.fact_subject_kinds(slug,description)
    VALUES(item->>'slug',item->>'description') ON CONFLICT(slug) DO NOTHING;
  END LOOP;

  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(bundle->'fact_types','[]')) LOOP
    INSERT INTO catalog.fact_types(slug,name,description,subject_kind,jurisdiction,
      value_schema,freshness_seconds,match_hints,status)
    VALUES(item->>'slug',item->>'name',item->>'description',item->>'subject_kind',item->>'jurisdiction',
      item->'value_schema',(item->>'freshness_seconds')::integer,coalesce(item->'match_hints','{}'),'draft')
    ON CONFLICT(slug) DO NOTHING;
    -- Global vocabulary is shared by existing sources. Never redefine it.
    SELECT jsonb_build_object('subject_kind',f.subject_kind,'jurisdiction',f.jurisdiction,
      'value_schema',f.value_schema,'freshness_seconds',f.freshness_seconds,'match_hints',f.match_hints)
      INTO reused FROM catalog.fact_types f WHERE f.slug=item->>'slug';
    IF reused IS DISTINCT FROM jsonb_build_object('subject_kind',item->>'subject_kind',
      'jurisdiction',item->>'jurisdiction','value_schema',item->'value_schema',
      'freshness_seconds',(item->>'freshness_seconds')::integer,'match_hints',coalesce(item->'match_hints','{}')) THEN
      RAISE EXCEPTION 'Shared fact definition differs: %',item->>'slug';
    END IF;
  END LOOP;

  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(bundle->'capabilities','[]')) LOOP
    INSERT INTO catalog.capabilities(slug,name,description,category,input_contract,output_contract,status)
    VALUES(item->>'slug',item->>'name',item->>'description',item->>'category',
      coalesce(item->'input_contract','{}'),coalesce(item->'output_contract','{}'),'draft')
    ON CONFLICT(slug) DO NOTHING;
    SELECT jsonb_build_object('name',c.name,'description',c.description,'category',c.category,
      'input_contract',c.input_contract,'output_contract',c.output_contract)
      INTO reused FROM catalog.capabilities c WHERE c.slug=item->>'slug';
    IF reused IS DISTINCT FROM jsonb_build_object('name',item->>'name','description',item->>'description',
      'category',item->>'category','input_contract',coalesce(item->'input_contract','{}'),
      'output_contract',coalesce(item->'output_contract','{}')) THEN
      RAISE EXCEPTION 'Shared capability definition differs: %',item->>'slug';
    END IF;
  END LOOP;

  FOR item IN SELECT value FROM jsonb_array_elements(bundle->'endpoints') LOOP
    IF nullif(item->>'key','') IS NULL OR endpoint_ids ? (item->>'key') THEN
      RAISE EXCEPTION 'Each endpoint needs a unique bundle-local key';
    END IF;
    endpoint_row := jsonb_populate_record(NULL::providers.api_endpoints,item-'key');
    IF endpoint_row.method NOT IN ('GET','POST') OR endpoint_row.method IS NULL
       OR coalesce(endpoint_row.path,'') !~ '^/[^?#]*$' OR endpoint_row.path LIKE '//%'
       OR endpoint_row.price IS NULL OR endpoint_row.price<0
       OR endpoint_row.price<>round(endpoint_row.price,6)
       OR nullif(btrim(endpoint_row.description),'') IS NULL
       OR endpoint_row.auth_override IS NOT NULL THEN
      RAISE EXCEPTION 'Invalid endpoint: require read GET/POST, relative path, micro-USD precision, description; secret auth_override is unsupported';
    END IF;
    IF endpoint_row.origin_override IS NOT NULL AND endpoint_row.origin_override !~ '^https://[^/@?#]+(:[0-9]+)?(/[^?#]*)?$' THEN
      RAISE EXCEPTION 'Origin override must be a reviewed HTTPS origin without credentials/query/fragment';
    END IF;
    IF jsonb_typeof(endpoint_row.query_params->'properties') IS DISTINCT FROM 'object'
       AND jsonb_typeof(endpoint_row.request_body->'properties') IS DISTINCT FROM 'object' THEN
      RAISE EXCEPTION 'Endpoint must document its inputs, including properties={} for no inputs';
    END IF;
    INSERT INTO providers.api_endpoints (
      api_id,method,path,payment_required,price,description,request_body,response_body,
      request_example,response_example,headers,query_params,active,documentation_markdown,
      external_docs_url,openapi_url,authentication_notes,origin_override,cache_ttl_seconds,
      tags,search_terms,use_cases,sectors,input_entities,output_entities,endpoint_category,
      action_type,task_summary,metadata_source,metadata_model,metadata_confidence,metadata_at,contract_documented
    ) VALUES (
      source_id,endpoint_row.method,endpoint_row.path,endpoint_row.price>0,endpoint_row.price,
      endpoint_row.description,endpoint_row.request_body,endpoint_row.response_body,
      endpoint_row.request_example,endpoint_row.response_example,endpoint_row.headers,
      endpoint_row.query_params,false,endpoint_row.documentation_markdown,
      endpoint_row.external_docs_url,endpoint_row.openapi_url,endpoint_row.authentication_notes,
      endpoint_row.origin_override,endpoint_row.cache_ttl_seconds,endpoint_row.tags,
      endpoint_row.search_terms,endpoint_row.use_cases,endpoint_row.sectors,
      endpoint_row.input_entities,endpoint_row.output_entities,endpoint_row.endpoint_category,
      endpoint_row.action_type,endpoint_row.task_summary,endpoint_row.metadata_source,
      endpoint_row.metadata_model,endpoint_row.metadata_confidence,endpoint_row.metadata_at,
      coalesce(endpoint_row.contract_documented,false)
    ) RETURNING id INTO endpoint_id;
    endpoint_ids := endpoint_ids || jsonb_build_object(item->>'key',endpoint_id);
  END LOOP;

  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(bundle->'contracts','[]')) LOOP
    endpoint_id := (endpoint_ids->>(item->>'endpoint_key'))::uuid;
    IF endpoint_id IS NULL THEN RAISE EXCEPTION 'Unknown contract endpoint_key'; END IF;
    contract_row := jsonb_populate_record(NULL::catalog.capability_execution_contracts,item-'endpoint_key');
    IF coalesce(item->>'effect','read')<>'read' OR coalesce(item->>'price_model','per_call')<>'per_call'
       OR coalesce(item->'preconditions','{}')<>'{}'::jsonb THEN
      RAISE EXCEPTION 'Current Ask runtime requires read/per_call and empty preconditions';
    END IF;
    IF jsonb_typeof(item->'requires') IS DISTINCT FROM 'array'
       OR jsonb_typeof(item->'produces') IS DISTINCT FROM 'array'
       OR jsonb_typeof(item->'input_map') IS DISTINCT FROM 'object'
       OR jsonb_typeof(item->'output_map'->'produces') IS DISTINCT FROM 'object'
       OR jsonb_array_length(item->'produces')=0 THEN
      RAISE EXCEPTION 'Contract requires typed requires/produces and input/output maps';
    END IF;
    FOR requirement IN SELECT value FROM jsonb_array_elements((item->'requires')||(item->'produces')) LOOP
      fact_slug := requirement->>'fact_type';
      IF requirement->>'subject_role' IS DISTINCT FROM 'subject'
         OR NOT EXISTS(SELECT 1 FROM catalog.fact_types WHERE slug=fact_slug) THEN
        RAISE EXCEPTION 'Unknown fact or unsupported subject role: %',fact_slug;
      END IF;
    END LOOP;
    IF EXISTS(SELECT 1 FROM jsonb_array_elements(item->'requires') r
      WHERE NOT (item->'input_map' ? (r->>'fact_type')))
      OR EXISTS(SELECT 1 FROM jsonb_array_elements(item->'produces') r
      WHERE NOT (item->'output_map'->'produces' ? (r->>'fact_type'))) THEN
      RAISE EXCEPTION 'Every declared input/output requires a mapping';
    END IF;
    INSERT INTO catalog.capability_execution_contracts (
      capability_slug,endpoint_id,version,status,requires,produces,input_map,fixed_params,
      output_map,cardinality,identifies,max_fan_out,preconditions,pagination,effect,price_model,
      freshness_seconds,adapter_version,sample_payload,notes
    ) VALUES (
      contract_row.capability_slug,endpoint_id,coalesce(contract_row.version,1),'draft',
      contract_row.requires,contract_row.produces,contract_row.input_map,
      coalesce(contract_row.fixed_params,'{}'),contract_row.output_map,
      coalesce(contract_row.cardinality,'one'),contract_row.identifies,
      coalesce(contract_row.max_fan_out,1),'{}',coalesce(contract_row.pagination,'{}'),
      'read','per_call',contract_row.freshness_seconds,coalesce(contract_row.adapter_version,'v1'),
      contract_row.sample_payload,contract_row.notes
    );
  END LOOP;

  -- Optional legacy discovery links. These do NOT authorize execution.
  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(bundle->'offers','[]')) LOOP
    endpoint_id := (endpoint_ids->>(item->>'endpoint_key'))::uuid;
    IF endpoint_id IS NULL THEN RAISE EXCEPTION 'Unknown offer endpoint_key'; END IF;
    INSERT INTO catalog.capability_offers(capability_slug,endpoint_id,api_id,source,confidence,
      input_map,fixed_params,output_map,notes)
    VALUES(item->>'capability_slug',endpoint_id,source_id,coalesce(item->>'source','inferred'),
      coalesce((item->>'confidence')::numeric,0),coalesce(item->'input_map','{}'),
      coalesce(item->'fixed_params','{}'),item->'output_map',item->>'notes');
  END LOOP;

  -- Optional atomic Vault provisioning: values arrive only through a bound
  -- server connection, not the AI manifest. No existing key is overwritten.
  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(bundle->'credential_requirements','[]')) LOOP
    secret_name := item->>'vault_name';
    IF coalesce(secret_name,'') !~ '^[A-Z][A-Z0-9_]+$' THEN RAISE EXCEPTION 'Invalid Vault name'; END IF;
    IF item->>'endpoint_key' IS NOT NULL AND NOT endpoint_ids ? (item->>'endpoint_key') THEN
      RAISE EXCEPTION 'Unknown credential endpoint_key';
    END IF;
    IF credential_values ? secret_name THEN
      IF jsonb_typeof(credential_values->secret_name)<>'string'
         OR nullif(btrim(credential_values->>secret_name),'') IS NULL THEN
        RAISE EXCEPTION 'Credential must be a nonempty string';
      END IF;
      IF EXISTS(SELECT 1 FROM vault.secrets WHERE name=secret_name) THEN
        RAISE EXCEPTION 'Vault name already exists; key rotation requires a separate explicit operation';
      END IF;
      PERFORM vault.create_secret(credential_values->>secret_name,secret_name,
        'Apiosk source onboarding: '||(src->>'slug')||'; runtime binding must be implemented and tested');
    END IF;
  END LOOP;
  IF EXISTS(SELECT 1 FROM jsonb_object_keys(credential_values) k WHERE NOT EXISTS(
    SELECT 1 FROM jsonb_array_elements(coalesce(bundle->'credential_requirements','[]')) r WHERE r->>'vault_name'=k)) THEN
    RAISE EXCEPTION 'Credential input contains an undeclared Vault name';
  END IF;
  PERFORM set_config('apiosk.onboarding_secrets','',true);
  RAISE NOTICE 'Source % registered inactive with % endpoints; contracts remain draft. No live execution has been certified.',src->>'slug',jsonb_array_length(bundle->'endpoints');
END;
$block_144025cc10b5398ca0b62c8a78ec83d01f9f2d4f0d81454d982b34d6e4a10070$;


-- Apiosk source bundle v1. Schema inspected in production on 2026-10-11.
-- TEMPLATE: render with render.mjs and a reviewed manifest. Not an auto-applied migration.
-- One transaction registers ONE NEW source, all endpoints, vocabulary and contracts.
-- The source remains inactive. See README.txt for the separate publication contract.
-- Never embed real credentials in this file, its manifest, examples or AI input.
-- Optional backend credential input on the SAME connection, BEFORE this script:
--   SELECT set_config('apiosk.onboarding_secrets', $1, false);
-- Bind $1 to a JSON object {"VAULT_NAME":"secret"}; do not interpolate SQL.
-- Use a dedicated connection and clear the setting/discard connection in finally.
-- Vault storage does NOT implement a generic gateway credential resolver.

SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '30s';
SET LOCAL standard_conforming_strings = on;

DO $block_697743d2ef082059a865806b93585cba29979e003feb0b44d11408941ae7cf62$
DECLARE
  bundle jsonb := $bundle_697743d2ef082059a865806b93585cba29979e003feb0b44d11408941ae7cf62${
  "format": "apiosk.source-bundle.v1",
  "bundle_id": "f6056ece-0902-452e-8ad5-cc09bb638ad9",
  "provenance": {
    "model": "codex",
    "generated_at": "2026-10-13T17:13:07.262800+00:00",
    "documentation_urls": [
      "https://developer.nibud.nl/products2",
      "https://developer.nibud.nl/technical-uitgaven",
      "https://developer.nibud.nl/technical-pensioen-uitgaven",
      "https://developer.nibud.nl/technical-afloscapaciteit"
    ],
    "reviewer": null,
    "live_test": {
      "status": "not_run",
      "notes": "Documentation verification is not an Apiosk execution test."
    },
    "openapi_sha256": "1a5e3c24d6a008472ff768a36ebac4838c42a93d677247a981153ee939664a69"
  },
  "source": {
    "owner_id": "bf085fe8-41c4-4699-978f-e75e89518bf7",
    "slug": "nibud",
    "name": "Nibud — huishoudelijke uitgaven en afloscapaciteit",
    "origin_url": "https://api.nibud.nl",
    "description": "Gelicentieerde referentie-uitgaven voor huishoudens, pensioenuitgaven en afloscapaciteit. Vereist een Nibud-abonnement en API-key; prijs en doorleveringsrechten zijn nog niet bevestigd. Geen toeslagrechten-database.",
    "long_description": "Gelicentieerde referentie-uitgaven voor huishoudens, pensioenuitgaven en afloscapaciteit. Vereist een Nibud-abonnement en API-key; prijs en doorleveringsrechten zijn nog niet bevestigd. Geen toeslagrechten-database.",
    "category": "finance",
    "docs_url": "https://developer.nibud.nl/products2",
    "website_url": "https://developer.nibud.nl/products2",
    "authentication_info": {
      "type": "api_key",
      "in": "header",
      "name": "Ocp-Apim-Subscription-Key",
      "alternative": {
        "in": "query",
        "name": "key"
      },
      "upstream": {
        "required": true
      }
    },
    "static_headers": [
      {
        "name": "Accept",
        "value": "application/json"
      },
      {
        "name": "Content-Type",
        "value": "application/json"
      }
    ],
    "listing_metadata": {
      "tags": [
        "Nibud",
        "budget",
        "huishouden",
        "referentiebedragen",
        "pensioen",
        "afloscapaciteit"
      ],
      "sectors": [
        "Personal finance",
        "Financial services"
      ],
      "jurisdiction": "NL",
      "language": "nl",
      "coverage_notice": "Gelicentieerde referentie-uitgaven voor huishoudens, pensioenuitgaven en afloscapaciteit. Vereist een Nibud-abonnement en API-key; prijs en doorleveringsrechten zijn nog niet bevestigd. Geen toeslagrechten-database.",
      "readiness": {
        "state": "staged",
        "blockers": [
          "Subscription/API key",
          "Commercial price and redistribution licence",
          "Private credential binding",
          "Resolve documented schema/example inconsistencies",
          "Live response schemas and Apiosk execution acceptance"
        ]
      },
      "pricing": {
        "state": "unpriced",
        "upstream_cost_usd": null,
        "listing_price_note": "Zero is a required database placeholder for inactive endpoints, not a free production offer."
      },
      "operator_notice": "Apiosk integration; not affiliated with the publisher.",
      "license": {
        "name": "Nibud commercial subscription",
        "status": "required",
        "redistribution_rights": "unconfirmed",
        "price_per_call_usd": null,
        "url": "https://developer.nibud.nl/getting-started"
      }
    }
  },
  "credential_requirements": [
    {
      "vault_name": "APIOSK_NIBUD_SUBSCRIPTION_KEY",
      "endpoint_key": null,
      "in": "header",
      "name": "Ocp-Apim-Subscription-Key",
      "prefix": "",
      "allowed_origin": "https://api.nibud.nl",
      "runtime_binding": "missing",
      "notes": "Product scope must be checked. No secret supplied or stored in catalog metadata."
    }
  ],
  "subject_kinds": [
    {
      "slug": "dataset",
      "description": "Dataset or source response"
    }
  ],
  "fact_types": [
    {
      "slug": "nl.nibud.expenditures.versie.response",
      "name": "Nibud Uitgaven: Versie",
      "description": "Official v6 OpenAPI input schema preserved. It disagrees with examples on casing and some numeric types, and requires woning.bouwenergielabel without defining it. Resolve with Nibud before activation. Output references Basis/Voorbeeld amounts; exact envelope unverified.",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": [
          "object",
          "array"
        ],
        "description": "Exact JSON response envelope is not published in OpenAPI and has not been verified with an authenticated call."
      },
      "freshness_seconds": 86400,
      "match_hints": {}
    },
    {
      "slug": "nl.nibud.expenditures.input.hoofdpersonen",
      "name": "hoofdpersonen",
      "description": "Nibud Uitgaven: Referentiebedragen: hoofdpersonen",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": "array",
        "items": {
          "required": [
            "geboortedatum"
          ],
          "type": "object",
          "properties": {
            "geboortedatum": {
              "type": "string"
            },
            "geslacht": {
              "type": "string"
            }
          }
        }
      },
      "freshness_seconds": null,
      "match_hints": {}
    },
    {
      "slug": "nl.nibud.expenditures.input.kinderen",
      "name": "kinderen",
      "description": "Nibud Uitgaven: Referentiebedragen: kinderen",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": "array",
        "items": {
          "required": [
            "geboortedatum"
          ],
          "type": "object",
          "properties": {
            "geboortedatum": {
              "type": "string"
            },
            "geslacht": {
              "type": "string"
            }
          }
        }
      },
      "freshness_seconds": null,
      "match_hints": {}
    },
    {
      "slug": "nl.nibud.expenditures.input.autos",
      "name": "autos",
      "description": "Nibud Uitgaven: Referentiebedragen: autos",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": "array",
        "items": {
          "required": [
            "nieuwwaarde",
            "kilometersPerJaar",
            "status"
          ],
          "type": "object",
          "properties": {
            "nieuwwaarde": {
              "type": "string"
            },
            "kilometersPerJaar": {
              "type": "string"
            },
            "status": {
              "type": "string"
            }
          }
        }
      },
      "freshness_seconds": null,
      "match_hints": {}
    },
    {
      "slug": "nl.nibud.expenditures.input.nettobesteedbaarinkomenpermaand",
      "name": "nettoBesteedbaarInkomenPerMaand",
      "description": "Nibud Uitgaven: Referentiebedragen: nettoBesteedbaarInkomenPerMaand",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": "integer"
      },
      "freshness_seconds": null,
      "match_hints": {}
    },
    {
      "slug": "nl.nibud.expenditures.input.woning",
      "name": "woning",
      "description": "Nibud Uitgaven: Referentiebedragen: woning",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "required": [
          "soort",
          "typeWoning",
          "bouwjaar",
          "bouwenergielabel"
        ],
        "type": "object",
        "properties": {
          "soort": {
            "type": "string"
          },
          "typeWoning": {
            "type": "string"
          },
          "bouwjaar": {
            "type": "string"
          },
          "energielabel": {
            "type": "string"
          },
          "wozWaarde": {
            "type": "integer"
          },
          "gemeenteCbsId": {
            "type": "string"
          }
        }
      },
      "freshness_seconds": null,
      "match_hints": {}
    },
    {
      "slug": "nl.nibud.expenditures.response",
      "name": "Nibud Uitgaven: Referentiebedragen",
      "description": "Official v6 OpenAPI input schema preserved. It disagrees with examples on casing and some numeric types, and requires woning.bouwenergielabel without defining it. Resolve with Nibud before activation. Output references Basis/Voorbeeld amounts; exact envelope unverified.",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": [
          "object",
          "array"
        ],
        "description": "Exact JSON response envelope is not published in OpenAPI and has not been verified with an authenticated call."
      },
      "freshness_seconds": 86400,
      "match_hints": {}
    },
    {
      "slug": "nl.nibud.expenditures.gemeentes.response",
      "name": "Nibud Uitgaven: Gemeentes",
      "description": "Official v6 OpenAPI input schema preserved. It disagrees with examples on casing and some numeric types, and requires woning.bouwenergielabel without defining it. Resolve with Nibud before activation. Output references Basis/Voorbeeld amounts; exact envelope unverified.",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": [
          "object",
          "array"
        ],
        "description": "Exact JSON response envelope is not published in OpenAPI and has not been verified with an authenticated call."
      },
      "freshness_seconds": 86400,
      "match_hints": {}
    },
    {
      "slug": "nl.nibud.pension-expenditures.input.hoofdpersonen",
      "name": "Hoofdpersonen",
      "description": "Nibud pensioenuitgaven berekenen: Hoofdpersonen",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": "array",
        "items": {
          "type": "object",
          "properties": {
            "Geslacht": {
              "type": "integer"
            }
          }
        }
      },
      "freshness_seconds": null,
      "match_hints": {}
    },
    {
      "slug": "nl.nibud.pension-expenditures.input.kinderen",
      "name": "Kinderen",
      "description": "Nibud pensioenuitgaven berekenen: Kinderen",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": "array",
        "items": {
          "type": "object",
          "properties": {
            "Leeftijd": {
              "type": "integer"
            },
            "Geslacht": {
              "type": "integer"
            }
          },
          "required": [
            "Leeftijd"
          ]
        }
      },
      "freshness_seconds": null,
      "match_hints": {}
    },
    {
      "slug": "nl.nibud.pension-expenditures.input.autos",
      "name": "Autos",
      "description": "Nibud pensioenuitgaven berekenen: Autos",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": "array",
        "items": {
          "type": "object",
          "properties": {
            "KilometersPerJaar": {
              "type": "integer"
            },
            "TypeAuto": {
              "type": "integer"
            },
            "StatusAuto": {
              "type": "integer"
            }
          },
          "required": [
            "KilometersPerJaar",
            "TypeAuto",
            "StatusAuto"
          ]
        }
      },
      "freshness_seconds": null,
      "match_hints": {}
    },
    {
      "slug": "nl.nibud.pension-expenditures.input.woning",
      "name": "Woning",
      "description": "Nibud pensioenuitgaven berekenen: Woning",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": "object",
        "properties": {
          "TypeWoning": {
            "type": "integer"
          },
          "SoortWoning": {
            "type": "integer"
          }
        },
        "required": [
          "TypeWoning",
          "SoortWoning"
        ]
      },
      "freshness_seconds": null,
      "match_hints": {}
    },
    {
      "slug": "nl.nibud.pension-expenditures.input.nettobesteedbaarinkomenpermaand",
      "name": "NettoBesteedbaarInkomenPerMaand",
      "description": "Nibud pensioenuitgaven berekenen: NettoBesteedbaarInkomenPerMaand",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": "integer"
      },
      "freshness_seconds": null,
      "match_hints": {}
    },
    {
      "slug": "nl.nibud.pension-expenditures.response",
      "name": "Nibud pensioenuitgaven berekenen",
      "description": "Input fields transcribed from the official operation reference. Enum string examples differ from integer schemas; verify before activation. Technical docs describe Bedrag/Uitgavepost outputs; exact envelope unverified.",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": [
          "object",
          "array"
        ],
        "description": "Exact JSON response envelope is not published in OpenAPI and has not been verified with an authenticated call."
      },
      "freshness_seconds": 86400,
      "match_hints": {}
    },
    {
      "slug": "nl.nibud.repayment-capacity.input.leeftijdhoofdpersoon",
      "name": "LeeftijdHoofdpersoon",
      "description": "Nibud afloscapaciteit berekenen: LeeftijdHoofdpersoon",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": "integer"
      },
      "freshness_seconds": null,
      "match_hints": {}
    },
    {
      "slug": "nl.nibud.repayment-capacity.input.heeftpartner",
      "name": "HeeftPartner",
      "description": "Nibud afloscapaciteit berekenen: HeeftPartner",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": "boolean"
      },
      "freshness_seconds": null,
      "match_hints": {}
    },
    {
      "slug": "nl.nibud.repayment-capacity.input.aantalthuiswonendekinderen",
      "name": "AantalThuiswonendeKinderen",
      "description": "Nibud afloscapaciteit berekenen: AantalThuiswonendeKinderen",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": "integer"
      },
      "freshness_seconds": null,
      "match_hints": {}
    },
    {
      "slug": "nl.nibud.repayment-capacity.input.aantalautos",
      "name": "AantalAutos",
      "description": "Nibud afloscapaciteit berekenen: AantalAutos",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": "integer"
      },
      "freshness_seconds": null,
      "match_hints": {}
    },
    {
      "slug": "nl.nibud.repayment-capacity.input.nettobesteedbaarinkomenpermaand",
      "name": "NettoBesteedbaarInkomenPerMaand",
      "description": "Nibud afloscapaciteit berekenen: NettoBesteedbaarInkomenPerMaand",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": "integer"
      },
      "freshness_seconds": null,
      "match_hints": {}
    },
    {
      "slug": "nl.nibud.repayment-capacity.input.bijzondereuitgaven",
      "name": "BijzondereUitgaven",
      "description": "Nibud afloscapaciteit berekenen: BijzondereUitgaven",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": "object",
        "properties": {
          "KostenZorg": {
            "type": "integer"
          },
          "KostenPartnerAlimentatie": {
            "type": "integer"
          },
          "KostenKinderAlimentatie": {
            "type": "integer"
          },
          "KostenKinderOpvang": {
            "type": "integer"
          },
          "KostenOverigeAflossingen": {
            "type": "integer"
          }
        }
      },
      "freshness_seconds": null,
      "match_hints": {}
    },
    {
      "slug": "nl.nibud.repayment-capacity.input.woning",
      "name": "Woning",
      "description": "Nibud afloscapaciteit berekenen: Woning",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": "object",
        "properties": {
          "WoningSoort": {
            "type": "integer"
          },
          "BrutoHypotheeklasten": {
            "type": "integer"
          },
          "HuurPerMaand": {
            "type": "integer"
          },
          "HuurServiceKosten": {
            "type": "integer"
          },
          "BijdrageVve": {
            "type": "integer"
          }
        },
        "required": [
          "WoningSoort"
        ]
      },
      "freshness_seconds": null,
      "match_hints": {}
    },
    {
      "slug": "nl.nibud.repayment-capacity.response",
      "name": "Nibud afloscapaciteit berekenen",
      "description": "Official raw request schema transcribed. Example uses string woning type/null costs where schema says integer: resolve before activation. Describes repayment and maximum repayment capacity, not benefit eligibility.",
      "subject_kind": "dataset",
      "jurisdiction": "NL",
      "value_schema": {
        "type": [
          "object",
          "array"
        ],
        "description": "Exact JSON response envelope is not published in OpenAPI and has not been verified with an authenticated call."
      },
      "freshness_seconds": 86400,
      "match_hints": {}
    }
  ],
  "capabilities": [
    {
      "slug": "nl.nibud.expenditures.versie",
      "name": "Nibud Uitgaven: Versie",
      "description": "Official v6 OpenAPI input schema preserved. It disagrees with examples on casing and some numeric types, and requires woning.bouwenergielabel without defining it. Resolve with Nibud before activation. Output references Basis/Voorbeeld amounts; exact envelope unverified.",
      "category": "finance",
      "input_contract": {
        "type": "object",
        "properties": {}
      },
      "output_contract": {
        "type": [
          "object",
          "array"
        ],
        "description": "Exact JSON response envelope is not published in OpenAPI and has not been verified with an authenticated call."
      }
    },
    {
      "slug": "nl.nibud.expenditures",
      "name": "Nibud Uitgaven: Referentiebedragen",
      "description": "Official v6 OpenAPI input schema preserved. It disagrees with examples on casing and some numeric types, and requires woning.bouwenergielabel without defining it. Resolve with Nibud before activation. Output references Basis/Voorbeeld amounts; exact envelope unverified.",
      "category": "finance",
      "input_contract": {
        "required": [
          "nettoBesteedbaarInkomenPerMaand",
          "woning",
          "hoofdpersonen",
          "kinderen"
        ],
        "type": "object",
        "properties": {
          "hoofdpersonen": {
            "type": "array",
            "items": {
              "required": [
                "geboortedatum"
              ],
              "type": "object",
              "properties": {
                "geboortedatum": {
                  "type": "string"
                },
                "geslacht": {
                  "type": "string"
                }
              }
            }
          },
          "kinderen": {
            "type": "array",
            "items": {
              "required": [
                "geboortedatum"
              ],
              "type": "object",
              "properties": {
                "geboortedatum": {
                  "type": "string"
                },
                "geslacht": {
                  "type": "string"
                }
              }
            }
          },
          "autos": {
            "type": "array",
            "items": {
              "required": [
                "nieuwwaarde",
                "kilometersPerJaar",
                "status"
              ],
              "type": "object",
              "properties": {
                "nieuwwaarde": {
                  "type": "string"
                },
                "kilometersPerJaar": {
                  "type": "string"
                },
                "status": {
                  "type": "string"
                }
              }
            }
          },
          "nettoBesteedbaarInkomenPerMaand": {
            "type": "integer"
          },
          "woning": {
            "required": [
              "soort",
              "typeWoning",
              "bouwjaar",
              "bouwenergielabel"
            ],
            "type": "object",
            "properties": {
              "soort": {
                "type": "string"
              },
              "typeWoning": {
                "type": "string"
              },
              "bouwjaar": {
                "type": "string"
              },
              "energielabel": {
                "type": "string"
              },
              "wozWaarde": {
                "type": "integer"
              },
              "gemeenteCbsId": {
                "type": "string"
              }
            }
          }
        }
      },
      "output_contract": {
        "type": [
          "object",
          "array"
        ],
        "description": "Exact JSON response envelope is not published in OpenAPI and has not been verified with an authenticated call."
      }
    },
    {
      "slug": "nl.nibud.expenditures.gemeentes",
      "name": "Nibud Uitgaven: Gemeentes",
      "description": "Official v6 OpenAPI input schema preserved. It disagrees with examples on casing and some numeric types, and requires woning.bouwenergielabel without defining it. Resolve with Nibud before activation. Output references Basis/Voorbeeld amounts; exact envelope unverified.",
      "category": "finance",
      "input_contract": {
        "type": "object",
        "properties": {}
      },
      "output_contract": {
        "type": [
          "object",
          "array"
        ],
        "description": "Exact JSON response envelope is not published in OpenAPI and has not been verified with an authenticated call."
      }
    },
    {
      "slug": "nl.nibud.pension-expenditures",
      "name": "Nibud pensioenuitgaven berekenen",
      "description": "Input fields transcribed from the official operation reference. Enum string examples differ from integer schemas; verify before activation. Technical docs describe Bedrag/Uitgavepost outputs; exact envelope unverified.",
      "category": "finance",
      "input_contract": {
        "type": "object",
        "properties": {
          "Hoofdpersonen": {
            "type": "array",
            "items": {
              "type": "object",
              "properties": {
                "Geslacht": {
                  "type": "integer"
                }
              }
            }
          },
          "Kinderen": {
            "type": "array",
            "items": {
              "type": "object",
              "properties": {
                "Leeftijd": {
                  "type": "integer"
                },
                "Geslacht": {
                  "type": "integer"
                }
              },
              "required": [
                "Leeftijd"
              ]
            }
          },
          "Autos": {
            "type": "array",
            "items": {
              "type": "object",
              "properties": {
                "KilometersPerJaar": {
                  "type": "integer"
                },
                "TypeAuto": {
                  "type": "integer"
                },
                "StatusAuto": {
                  "type": "integer"
                }
              },
              "required": [
                "KilometersPerJaar",
                "TypeAuto",
                "StatusAuto"
              ]
            }
          },
          "Woning": {
            "type": "object",
            "properties": {
              "TypeWoning": {
                "type": "integer"
              },
              "SoortWoning": {
                "type": "integer"
              }
            },
            "required": [
              "TypeWoning",
              "SoortWoning"
            ]
          },
          "NettoBesteedbaarInkomenPerMaand": {
            "type": "integer"
          }
        },
        "required": [
          "Hoofdpersonen",
          "Kinderen",
          "Autos",
          "Woning",
          "NettoBesteedbaarInkomenPerMaand"
        ]
      },
      "output_contract": {
        "type": [
          "object",
          "array"
        ],
        "description": "Exact JSON response envelope is not published in OpenAPI and has not been verified with an authenticated call."
      }
    },
    {
      "slug": "nl.nibud.repayment-capacity",
      "name": "Nibud afloscapaciteit berekenen",
      "description": "Official raw request schema transcribed. Example uses string woning type/null costs where schema says integer: resolve before activation. Describes repayment and maximum repayment capacity, not benefit eligibility.",
      "category": "finance",
      "input_contract": {
        "type": "object",
        "properties": {
          "LeeftijdHoofdpersoon": {
            "type": "integer"
          },
          "HeeftPartner": {
            "type": "boolean"
          },
          "AantalThuiswonendeKinderen": {
            "type": "integer"
          },
          "AantalAutos": {
            "type": "integer"
          },
          "NettoBesteedbaarInkomenPerMaand": {
            "type": "integer"
          },
          "BijzondereUitgaven": {
            "type": "object",
            "properties": {
              "KostenZorg": {
                "type": "integer"
              },
              "KostenPartnerAlimentatie": {
                "type": "integer"
              },
              "KostenKinderAlimentatie": {
                "type": "integer"
              },
              "KostenKinderOpvang": {
                "type": "integer"
              },
              "KostenOverigeAflossingen": {
                "type": "integer"
              }
            }
          },
          "Woning": {
            "type": "object",
            "properties": {
              "WoningSoort": {
                "type": "integer"
              },
              "BrutoHypotheeklasten": {
                "type": "integer"
              },
              "HuurPerMaand": {
                "type": "integer"
              },
              "HuurServiceKosten": {
                "type": "integer"
              },
              "BijdrageVve": {
                "type": "integer"
              }
            },
            "required": [
              "WoningSoort"
            ]
          }
        },
        "required": [
          "LeeftijdHoofdpersoon",
          "HeeftPartner",
          "AantalThuiswonendeKinderen",
          "AantalAutos",
          "NettoBesteedbaarInkomenPerMaand",
          "BijzondereUitgaven",
          "Woning"
        ]
      },
      "output_contract": {
        "type": [
          "object",
          "array"
        ],
        "description": "Exact JSON response envelope is not published in OpenAPI and has not been verified with an authenticated call."
      }
    }
  ],
  "endpoints": [
    {
      "key": "uitgaven-versie",
      "method": "GET",
      "path": "/Uitgaven/v6/Versie",
      "price": 0,
      "description": "Nibud Uitgaven: Versie",
      "query_params": {
        "type": "object",
        "properties": {}
      },
      "request_body": null,
      "response_body": {
        "type": [
          "object",
          "array"
        ],
        "description": "Exact JSON response envelope is not published in OpenAPI and has not been verified with an authenticated call."
      },
      "request_example": null,
      "response_example": null,
      "headers": {
        "Accept": {
          "type": "string",
          "description": "application/json"
        }
      },
      "documentation_markdown": "Nibud Uitgaven: Versie\n\nOfficial v6 OpenAPI input schema preserved. It disagrees with examples on casing and some numeric types, and requires woning.bouwenergielabel without defining it. Resolve with Nibud before activation. Output references Basis/Voorbeeld amounts; exact envelope unverified.\n\nRegistered on 2026-10-13. Inactive; no successful Apiosk execution is claimed. Price awaits review.",
      "external_docs_url": "https://developer.nibud.nl/reference#api=Uitgaven-v6&operation=get-getversie",
      "authentication_notes": "Nibud subscription key required; inject privately on the server. Never request the key from buyers.",
      "origin_override": null,
      "cache_ttl_seconds": 86400,
      "tags": [
        "Nibud",
        "Uitgaven",
        "Versie",
        "huishoudbudget"
      ],
      "search_terms": [
        "Nibud",
        "Uitgaven",
        "Versie",
        "huishoudbudget",
        "Nibud Uitgaven: Versie"
      ],
      "use_cases": [
        "Nibud Uitgaven: Versie"
      ],
      "sectors": [
        "Personal finance",
        "Financial services"
      ],
      "input_entities": [],
      "output_entities": [
        "nl.nibud.expenditures.versie.response"
      ],
      "endpoint_category": "finance",
      "action_type": "lookup",
      "task_summary": "Nibud Uitgaven: Versie",
      "metadata_source": "provider_documentation",
      "metadata_model": "codex",
      "metadata_confidence": 0.9,
      "metadata_at": "2026-10-13T17:13:07.262800+00:00",
      "contract_documented": false
    },
    {
      "key": "uitgaven-referentiebedragen",
      "method": "POST",
      "path": "/Uitgaven/v6/Referentiebedragen",
      "price": 0,
      "description": "Nibud Uitgaven: Referentiebedragen",
      "query_params": {
        "type": "object",
        "properties": {}
      },
      "request_body": {
        "required": [
          "nettoBesteedbaarInkomenPerMaand",
          "woning",
          "hoofdpersonen",
          "kinderen"
        ],
        "type": "object",
        "properties": {
          "hoofdpersonen": {
            "type": "array",
            "items": {
              "required": [
                "geboortedatum"
              ],
              "type": "object",
              "properties": {
                "geboortedatum": {
                  "type": "string"
                },
                "geslacht": {
                  "type": "string"
                }
              }
            }
          },
          "kinderen": {
            "type": "array",
            "items": {
              "required": [
                "geboortedatum"
              ],
              "type": "object",
              "properties": {
                "geboortedatum": {
                  "type": "string"
                },
                "geslacht": {
                  "type": "string"
                }
              }
            }
          },
          "autos": {
            "type": "array",
            "items": {
              "required": [
                "nieuwwaarde",
                "kilometersPerJaar",
                "status"
              ],
              "type": "object",
              "properties": {
                "nieuwwaarde": {
                  "type": "string"
                },
                "kilometersPerJaar": {
                  "type": "string"
                },
                "status": {
                  "type": "string"
                }
              }
            }
          },
          "nettoBesteedbaarInkomenPerMaand": {
            "type": "integer"
          },
          "woning": {
            "required": [
              "soort",
              "typeWoning",
              "bouwjaar",
              "bouwenergielabel"
            ],
            "type": "object",
            "properties": {
              "soort": {
                "type": "string"
              },
              "typeWoning": {
                "type": "string"
              },
              "bouwjaar": {
                "type": "string"
              },
              "energielabel": {
                "type": "string"
              },
              "wozWaarde": {
                "type": "integer"
              },
              "gemeenteCbsId": {
                "type": "string"
              }
            }
          }
        }
      },
      "response_body": {
        "type": [
          "object",
          "array"
        ],
        "description": "Exact JSON response envelope is not published in OpenAPI and has not been verified with an authenticated call."
      },
      "request_example": null,
      "response_example": null,
      "headers": {
        "Accept": {
          "type": "string",
          "description": "application/json"
        }
      },
      "documentation_markdown": "Nibud Uitgaven: Referentiebedragen\n\nOfficial v6 OpenAPI input schema preserved. It disagrees with examples on casing and some numeric types, and requires woning.bouwenergielabel without defining it. Resolve with Nibud before activation. Output references Basis/Voorbeeld amounts; exact envelope unverified.\n\nRegistered on 2026-10-13. Inactive; no successful Apiosk execution is claimed. Price awaits review.",
      "external_docs_url": "https://developer.nibud.nl/reference#api=Uitgaven-v6&operation=post-postreferentiebedragen",
      "authentication_notes": "Nibud subscription key required; inject privately on the server. Never request the key from buyers.",
      "origin_override": null,
      "cache_ttl_seconds": 0,
      "tags": [
        "Nibud",
        "Uitgaven",
        "Referentiebedragen",
        "huishoudbudget"
      ],
      "search_terms": [
        "Nibud",
        "Uitgaven",
        "Referentiebedragen",
        "huishoudbudget",
        "Nibud Uitgaven: Referentiebedragen"
      ],
      "use_cases": [
        "Nibud Uitgaven: Referentiebedragen"
      ],
      "sectors": [
        "Personal finance",
        "Financial services"
      ],
      "input_entities": [
        "hoofdpersonen",
        "kinderen",
        "autos",
        "nettoBesteedbaarInkomenPerMaand",
        "woning"
      ],
      "output_entities": [
        "nl.nibud.expenditures.response"
      ],
      "endpoint_category": "finance",
      "action_type": "compute",
      "task_summary": "Nibud Uitgaven: Referentiebedragen",
      "metadata_source": "provider_documentation",
      "metadata_model": "codex",
      "metadata_confidence": 0.9,
      "metadata_at": "2026-10-13T17:13:07.262800+00:00",
      "contract_documented": false
    },
    {
      "key": "uitgaven-gemeentes",
      "method": "GET",
      "path": "/Uitgaven/v6/Gemeentes",
      "price": 0,
      "description": "Nibud Uitgaven: Gemeentes",
      "query_params": {
        "type": "object",
        "properties": {}
      },
      "request_body": null,
      "response_body": {
        "type": [
          "object",
          "array"
        ],
        "description": "Exact JSON response envelope is not published in OpenAPI and has not been verified with an authenticated call."
      },
      "request_example": null,
      "response_example": null,
      "headers": {
        "Accept": {
          "type": "string",
          "description": "application/json"
        }
      },
      "documentation_markdown": "Nibud Uitgaven: Gemeentes\n\nOfficial v6 OpenAPI input schema preserved. It disagrees with examples on casing and some numeric types, and requires woning.bouwenergielabel without defining it. Resolve with Nibud before activation. Output references Basis/Voorbeeld amounts; exact envelope unverified.\n\nRegistered on 2026-10-13. Inactive; no successful Apiosk execution is claimed. Price awaits review.",
      "external_docs_url": "https://developer.nibud.nl/reference#api=Uitgaven-v6&operation=get-getgemeentes",
      "authentication_notes": "Nibud subscription key required; inject privately on the server. Never request the key from buyers.",
      "origin_override": null,
      "cache_ttl_seconds": 86400,
      "tags": [
        "Nibud",
        "Uitgaven",
        "Gemeentes",
        "huishoudbudget"
      ],
      "search_terms": [
        "Nibud",
        "Uitgaven",
        "Gemeentes",
        "huishoudbudget",
        "Nibud Uitgaven: Gemeentes"
      ],
      "use_cases": [
        "Nibud Uitgaven: Gemeentes"
      ],
      "sectors": [
        "Personal finance",
        "Financial services"
      ],
      "input_entities": [],
      "output_entities": [
        "nl.nibud.expenditures.gemeentes.response"
      ],
      "endpoint_category": "finance",
      "action_type": "lookup",
      "task_summary": "Nibud Uitgaven: Gemeentes",
      "metadata_source": "provider_documentation",
      "metadata_model": "codex",
      "metadata_confidence": 0.9,
      "metadata_at": "2026-10-13T17:13:07.262800+00:00",
      "contract_documented": false
    },
    {
      "key": "pensioen-resultaat",
      "method": "POST",
      "path": "/pensioenuitgaven/Resultaat",
      "price": 0,
      "description": "Nibud pensioenuitgaven berekenen",
      "query_params": {
        "type": "object",
        "properties": {}
      },
      "request_body": {
        "type": "object",
        "properties": {
          "Hoofdpersonen": {
            "type": "array",
            "items": {
              "type": "object",
              "properties": {
                "Geslacht": {
                  "type": "integer"
                }
              }
            }
          },
          "Kinderen": {
            "type": "array",
            "items": {
              "type": "object",
              "properties": {
                "Leeftijd": {
                  "type": "integer"
                },
                "Geslacht": {
                  "type": "integer"
                }
              },
              "required": [
                "Leeftijd"
              ]
            }
          },
          "Autos": {
            "type": "array",
            "items": {
              "type": "object",
              "properties": {
                "KilometersPerJaar": {
                  "type": "integer"
                },
                "TypeAuto": {
                  "type": "integer"
                },
                "StatusAuto": {
                  "type": "integer"
                }
              },
              "required": [
                "KilometersPerJaar",
                "TypeAuto",
                "StatusAuto"
              ]
            }
          },
          "Woning": {
            "type": "object",
            "properties": {
              "TypeWoning": {
                "type": "integer"
              },
              "SoortWoning": {
                "type": "integer"
              }
            },
            "required": [
              "TypeWoning",
              "SoortWoning"
            ]
          },
          "NettoBesteedbaarInkomenPerMaand": {
            "type": "integer"
          }
        },
        "required": [
          "Hoofdpersonen",
          "Kinderen",
          "Autos",
          "Woning",
          "NettoBesteedbaarInkomenPerMaand"
        ]
      },
      "response_body": {
        "type": [
          "object",
          "array"
        ],
        "description": "Exact JSON response envelope is not published in OpenAPI and has not been verified with an authenticated call."
      },
      "request_example": null,
      "response_example": null,
      "headers": {
        "Accept": {
          "type": "string",
          "description": "application/json"
        }
      },
      "documentation_markdown": "Nibud pensioenuitgaven berekenen\n\nInput fields transcribed from the official operation reference. Enum string examples differ from integer schemas; verify before activation. Technical docs describe Bedrag/Uitgavepost outputs; exact envelope unverified.\n\nRegistered on 2026-10-13. Inactive; no successful Apiosk execution is claimed. Price awaits review.",
      "external_docs_url": "https://developer.nibud.nl/reference#api=pensioenuitgaven&operation=berekenresultaat",
      "authentication_notes": "Nibud subscription key required; inject privately on the server. Never request the key from buyers.",
      "origin_override": null,
      "cache_ttl_seconds": 0,
      "tags": [
        "Nibud",
        "pensioen",
        "uitgaven",
        "referentiebedragen"
      ],
      "search_terms": [
        "Nibud",
        "pensioen",
        "uitgaven",
        "referentiebedragen",
        "Nibud pensioenuitgaven berekenen"
      ],
      "use_cases": [
        "Nibud pensioenuitgaven berekenen"
      ],
      "sectors": [
        "Personal finance",
        "Financial services"
      ],
      "input_entities": [
        "Hoofdpersonen",
        "Kinderen",
        "Autos",
        "Woning",
        "NettoBesteedbaarInkomenPerMaand"
      ],
      "output_entities": [
        "nl.nibud.pension-expenditures.response"
      ],
      "endpoint_category": "finance",
      "action_type": "compute",
      "task_summary": "Nibud pensioenuitgaven berekenen",
      "metadata_source": "provider_documentation",
      "metadata_model": "codex",
      "metadata_confidence": 0.9,
      "metadata_at": "2026-10-13T17:13:07.262800+00:00",
      "contract_documented": false
    },
    {
      "key": "afloscapaciteit-resultaat",
      "method": "POST",
      "path": "/Afloscapaciteit/Resultaat",
      "price": 0,
      "description": "Nibud afloscapaciteit berekenen",
      "query_params": {
        "type": "object",
        "properties": {}
      },
      "request_body": {
        "type": "object",
        "properties": {
          "LeeftijdHoofdpersoon": {
            "type": "integer"
          },
          "HeeftPartner": {
            "type": "boolean"
          },
          "AantalThuiswonendeKinderen": {
            "type": "integer"
          },
          "AantalAutos": {
            "type": "integer"
          },
          "NettoBesteedbaarInkomenPerMaand": {
            "type": "integer"
          },
          "BijzondereUitgaven": {
            "type": "object",
            "properties": {
              "KostenZorg": {
                "type": "integer"
              },
              "KostenPartnerAlimentatie": {
                "type": "integer"
              },
              "KostenKinderAlimentatie": {
                "type": "integer"
              },
              "KostenKinderOpvang": {
                "type": "integer"
              },
              "KostenOverigeAflossingen": {
                "type": "integer"
              }
            }
          },
          "Woning": {
            "type": "object",
            "properties": {
              "WoningSoort": {
                "type": "integer"
              },
              "BrutoHypotheeklasten": {
                "type": "integer"
              },
              "HuurPerMaand": {
                "type": "integer"
              },
              "HuurServiceKosten": {
                "type": "integer"
              },
              "BijdrageVve": {
                "type": "integer"
              }
            },
            "required": [
              "WoningSoort"
            ]
          }
        },
        "required": [
          "LeeftijdHoofdpersoon",
          "HeeftPartner",
          "AantalThuiswonendeKinderen",
          "AantalAutos",
          "NettoBesteedbaarInkomenPerMaand",
          "BijzondereUitgaven",
          "Woning"
        ]
      },
      "response_body": {
        "type": [
          "object",
          "array"
        ],
        "description": "Exact JSON response envelope is not published in OpenAPI and has not been verified with an authenticated call."
      },
      "request_example": null,
      "response_example": null,
      "headers": {
        "Accept": {
          "type": "string",
          "description": "application/json"
        }
      },
      "documentation_markdown": "Nibud afloscapaciteit berekenen\n\nOfficial raw request schema transcribed. Example uses string woning type/null costs where schema says integer: resolve before activation. Describes repayment and maximum repayment capacity, not benefit eligibility.\n\nRegistered on 2026-10-13. Inactive; no successful Apiosk execution is claimed. Price awaits review.",
      "external_docs_url": "https://developer.nibud.nl/reference#api=afloscapaciteit&operation=post-berekenresultaat",
      "authentication_notes": "Nibud subscription key required; inject privately on the server. Never request the key from buyers.",
      "origin_override": null,
      "cache_ttl_seconds": 0,
      "tags": [
        "Nibud",
        "afloscapaciteit",
        "schulden",
        "betalingsregeling",
        "huishoudbudget"
      ],
      "search_terms": [
        "Nibud",
        "afloscapaciteit",
        "schulden",
        "betalingsregeling",
        "huishoudbudget",
        "Nibud afloscapaciteit berekenen"
      ],
      "use_cases": [
        "Nibud afloscapaciteit berekenen"
      ],
      "sectors": [
        "Personal finance",
        "Financial services"
      ],
      "input_entities": [
        "LeeftijdHoofdpersoon",
        "HeeftPartner",
        "AantalThuiswonendeKinderen",
        "AantalAutos",
        "NettoBesteedbaarInkomenPerMaand",
        "BijzondereUitgaven",
        "Woning"
      ],
      "output_entities": [
        "nl.nibud.repayment-capacity.response"
      ],
      "endpoint_category": "finance",
      "action_type": "compute",
      "task_summary": "Nibud afloscapaciteit berekenen",
      "metadata_source": "provider_documentation",
      "metadata_model": "codex",
      "metadata_confidence": 0.9,
      "metadata_at": "2026-10-13T17:13:07.262800+00:00",
      "contract_documented": false
    }
  ],
  "contracts": [
    {
      "endpoint_key": "uitgaven-versie",
      "capability_slug": "nl.nibud.expenditures.versie",
      "version": 1,
      "requires": [],
      "produces": [
        {
          "fact_type": "nl.nibud.expenditures.versie.response",
          "subject_role": "subject",
          "cardinality": "one"
        }
      ],
      "input_map": {},
      "fixed_params": {},
      "output_map": {
        "base": "$",
        "produces": {
          "nl.nibud.expenditures.versie.response": {
            "path": "$"
          }
        }
      },
      "cardinality": "one",
      "max_fan_out": 1,
      "preconditions": {},
      "pagination": {},
      "effect": "read",
      "price_model": "per_call",
      "freshness_seconds": 86400,
      "adapter_version": "pending-nl-benefits-v1",
      "notes": "DRAFT: Official v6 OpenAPI input schema preserved. It disagrees with examples on casing and some numeric types, and requires woning.bouwenergielabel without defining it. Resolve with Nibud before activation. Output references Basis/Voorbeeld amounts; exact envelope unverified. Requires a verified executor response shape and live extraction before activation."
    },
    {
      "endpoint_key": "uitgaven-referentiebedragen",
      "capability_slug": "nl.nibud.expenditures",
      "version": 1,
      "requires": [
        {
          "fact_type": "nl.nibud.expenditures.input.hoofdpersonen",
          "subject_role": "subject",
          "optional": false
        },
        {
          "fact_type": "nl.nibud.expenditures.input.kinderen",
          "subject_role": "subject",
          "optional": false
        },
        {
          "fact_type": "nl.nibud.expenditures.input.autos",
          "subject_role": "subject",
          "optional": true
        },
        {
          "fact_type": "nl.nibud.expenditures.input.nettobesteedbaarinkomenpermaand",
          "subject_role": "subject",
          "optional": false
        },
        {
          "fact_type": "nl.nibud.expenditures.input.woning",
          "subject_role": "subject",
          "optional": false
        }
      ],
      "produces": [
        {
          "fact_type": "nl.nibud.expenditures.response",
          "subject_role": "subject",
          "cardinality": "one"
        }
      ],
      "input_map": {
        "nl.nibud.expenditures.input.hoofdpersonen": {
          "in": "body",
          "name": "hoofdpersonen"
        },
        "nl.nibud.expenditures.input.kinderen": {
          "in": "body",
          "name": "kinderen"
        },
        "nl.nibud.expenditures.input.autos": {
          "in": "body",
          "name": "autos"
        },
        "nl.nibud.expenditures.input.nettobesteedbaarinkomenpermaand": {
          "in": "body",
          "name": "nettoBesteedbaarInkomenPerMaand"
        },
        "nl.nibud.expenditures.input.woning": {
          "in": "body",
          "name": "woning"
        }
      },
      "fixed_params": {},
      "output_map": {
        "base": "$",
        "produces": {
          "nl.nibud.expenditures.response": {
            "path": "$"
          }
        }
      },
      "cardinality": "one",
      "max_fan_out": 1,
      "preconditions": {},
      "pagination": {},
      "effect": "read",
      "price_model": "per_call",
      "freshness_seconds": 300,
      "adapter_version": "pending-nl-benefits-v1",
      "notes": "DRAFT: Official v6 OpenAPI input schema preserved. It disagrees with examples on casing and some numeric types, and requires woning.bouwenergielabel without defining it. Resolve with Nibud before activation. Output references Basis/Voorbeeld amounts; exact envelope unverified. Requires a verified executor response shape and live extraction before activation."
    },
    {
      "endpoint_key": "uitgaven-gemeentes",
      "capability_slug": "nl.nibud.expenditures.gemeentes",
      "version": 1,
      "requires": [],
      "produces": [
        {
          "fact_type": "nl.nibud.expenditures.gemeentes.response",
          "subject_role": "subject",
          "cardinality": "one"
        }
      ],
      "input_map": {},
      "fixed_params": {},
      "output_map": {
        "base": "$",
        "produces": {
          "nl.nibud.expenditures.gemeentes.response": {
            "path": "$"
          }
        }
      },
      "cardinality": "one",
      "max_fan_out": 1,
      "preconditions": {},
      "pagination": {},
      "effect": "read",
      "price_model": "per_call",
      "freshness_seconds": 86400,
      "adapter_version": "pending-nl-benefits-v1",
      "notes": "DRAFT: Official v6 OpenAPI input schema preserved. It disagrees with examples on casing and some numeric types, and requires woning.bouwenergielabel without defining it. Resolve with Nibud before activation. Output references Basis/Voorbeeld amounts; exact envelope unverified. Requires a verified executor response shape and live extraction before activation."
    },
    {
      "endpoint_key": "pensioen-resultaat",
      "capability_slug": "nl.nibud.pension-expenditures",
      "version": 1,
      "requires": [
        {
          "fact_type": "nl.nibud.pension-expenditures.input.hoofdpersonen",
          "subject_role": "subject",
          "optional": false
        },
        {
          "fact_type": "nl.nibud.pension-expenditures.input.kinderen",
          "subject_role": "subject",
          "optional": false
        },
        {
          "fact_type": "nl.nibud.pension-expenditures.input.autos",
          "subject_role": "subject",
          "optional": false
        },
        {
          "fact_type": "nl.nibud.pension-expenditures.input.woning",
          "subject_role": "subject",
          "optional": false
        },
        {
          "fact_type": "nl.nibud.pension-expenditures.input.nettobesteedbaarinkomenpermaand",
          "subject_role": "subject",
          "optional": false
        }
      ],
      "produces": [
        {
          "fact_type": "nl.nibud.pension-expenditures.response",
          "subject_role": "subject",
          "cardinality": "one"
        }
      ],
      "input_map": {
        "nl.nibud.pension-expenditures.input.hoofdpersonen": {
          "in": "body",
          "name": "Hoofdpersonen"
        },
        "nl.nibud.pension-expenditures.input.kinderen": {
          "in": "body",
          "name": "Kinderen"
        },
        "nl.nibud.pension-expenditures.input.autos": {
          "in": "body",
          "name": "Autos"
        },
        "nl.nibud.pension-expenditures.input.woning": {
          "in": "body",
          "name": "Woning"
        },
        "nl.nibud.pension-expenditures.input.nettobesteedbaarinkomenpermaand": {
          "in": "body",
          "name": "NettoBesteedbaarInkomenPerMaand"
        }
      },
      "fixed_params": {},
      "output_map": {
        "base": "$",
        "produces": {
          "nl.nibud.pension-expenditures.response": {
            "path": "$"
          }
        }
      },
      "cardinality": "one",
      "max_fan_out": 1,
      "preconditions": {},
      "pagination": {},
      "effect": "read",
      "price_model": "per_call",
      "freshness_seconds": 300,
      "adapter_version": "pending-nl-benefits-v1",
      "notes": "DRAFT: Input fields transcribed from the official operation reference. Enum string examples differ from integer schemas; verify before activation. Technical docs describe Bedrag/Uitgavepost outputs; exact envelope unverified. Requires a verified executor response shape and live extraction before activation."
    },
    {
      "endpoint_key": "afloscapaciteit-resultaat",
      "capability_slug": "nl.nibud.repayment-capacity",
      "version": 1,
      "requires": [
        {
          "fact_type": "nl.nibud.repayment-capacity.input.leeftijdhoofdpersoon",
          "subject_role": "subject",
          "optional": false
        },
        {
          "fact_type": "nl.nibud.repayment-capacity.input.heeftpartner",
          "subject_role": "subject",
          "optional": false
        },
        {
          "fact_type": "nl.nibud.repayment-capacity.input.aantalthuiswonendekinderen",
          "subject_role": "subject",
          "optional": false
        },
        {
          "fact_type": "nl.nibud.repayment-capacity.input.aantalautos",
          "subject_role": "subject",
          "optional": false
        },
        {
          "fact_type": "nl.nibud.repayment-capacity.input.nettobesteedbaarinkomenpermaand",
          "subject_role": "subject",
          "optional": false
        },
        {
          "fact_type": "nl.nibud.repayment-capacity.input.bijzondereuitgaven",
          "subject_role": "subject",
          "optional": false
        },
        {
          "fact_type": "nl.nibud.repayment-capacity.input.woning",
          "subject_role": "subject",
          "optional": false
        }
      ],
      "produces": [
        {
          "fact_type": "nl.nibud.repayment-capacity.response",
          "subject_role": "subject",
          "cardinality": "one"
        }
      ],
      "input_map": {
        "nl.nibud.repayment-capacity.input.leeftijdhoofdpersoon": {
          "in": "body",
          "name": "LeeftijdHoofdpersoon"
        },
        "nl.nibud.repayment-capacity.input.heeftpartner": {
          "in": "body",
          "name": "HeeftPartner"
        },
        "nl.nibud.repayment-capacity.input.aantalthuiswonendekinderen": {
          "in": "body",
          "name": "AantalThuiswonendeKinderen"
        },
        "nl.nibud.repayment-capacity.input.aantalautos": {
          "in": "body",
          "name": "AantalAutos"
        },
        "nl.nibud.repayment-capacity.input.nettobesteedbaarinkomenpermaand": {
          "in": "body",
          "name": "NettoBesteedbaarInkomenPerMaand"
        },
        "nl.nibud.repayment-capacity.input.bijzondereuitgaven": {
          "in": "body",
          "name": "BijzondereUitgaven"
        },
        "nl.nibud.repayment-capacity.input.woning": {
          "in": "body",
          "name": "Woning"
        }
      },
      "fixed_params": {},
      "output_map": {
        "base": "$",
        "produces": {
          "nl.nibud.repayment-capacity.response": {
            "path": "$"
          }
        }
      },
      "cardinality": "one",
      "max_fan_out": 1,
      "preconditions": {},
      "pagination": {},
      "effect": "read",
      "price_model": "per_call",
      "freshness_seconds": 300,
      "adapter_version": "pending-nl-benefits-v1",
      "notes": "DRAFT: Official raw request schema transcribed. Example uses string woning type/null costs where schema says integer: resolve before activation. Describes repayment and maximum repayment capacity, not benefit eligibility. Requires a verified executor response shape and live extraction before activation."
    }
  ],
  "offers": [
    {
      "endpoint_key": "uitgaven-versie",
      "capability_slug": "nl.nibud.expenditures.versie",
      "source": "declared",
      "confidence": 0.9,
      "input_map": {},
      "fixed_params": {},
      "notes": "Documentation-backed discovery link; endpoint and execution contract remain inactive/draft."
    },
    {
      "endpoint_key": "uitgaven-referentiebedragen",
      "capability_slug": "nl.nibud.expenditures",
      "source": "declared",
      "confidence": 0.9,
      "input_map": {
        "hoofdpersonen": {
          "in": "body",
          "name": "hoofdpersonen"
        },
        "kinderen": {
          "in": "body",
          "name": "kinderen"
        },
        "autos": {
          "in": "body",
          "name": "autos"
        },
        "nettoBesteedbaarInkomenPerMaand": {
          "in": "body",
          "name": "nettoBesteedbaarInkomenPerMaand"
        },
        "woning": {
          "in": "body",
          "name": "woning"
        }
      },
      "fixed_params": {},
      "notes": "Documentation-backed discovery link; endpoint and execution contract remain inactive/draft."
    },
    {
      "endpoint_key": "uitgaven-gemeentes",
      "capability_slug": "nl.nibud.expenditures.gemeentes",
      "source": "declared",
      "confidence": 0.9,
      "input_map": {},
      "fixed_params": {},
      "notes": "Documentation-backed discovery link; endpoint and execution contract remain inactive/draft."
    },
    {
      "endpoint_key": "pensioen-resultaat",
      "capability_slug": "nl.nibud.pension-expenditures",
      "source": "declared",
      "confidence": 0.9,
      "input_map": {
        "Hoofdpersonen": {
          "in": "body",
          "name": "Hoofdpersonen"
        },
        "Kinderen": {
          "in": "body",
          "name": "Kinderen"
        },
        "Autos": {
          "in": "body",
          "name": "Autos"
        },
        "Woning": {
          "in": "body",
          "name": "Woning"
        },
        "NettoBesteedbaarInkomenPerMaand": {
          "in": "body",
          "name": "NettoBesteedbaarInkomenPerMaand"
        }
      },
      "fixed_params": {},
      "notes": "Documentation-backed discovery link; endpoint and execution contract remain inactive/draft."
    },
    {
      "endpoint_key": "afloscapaciteit-resultaat",
      "capability_slug": "nl.nibud.repayment-capacity",
      "source": "declared",
      "confidence": 0.9,
      "input_map": {
        "LeeftijdHoofdpersoon": {
          "in": "body",
          "name": "LeeftijdHoofdpersoon"
        },
        "HeeftPartner": {
          "in": "body",
          "name": "HeeftPartner"
        },
        "AantalThuiswonendeKinderen": {
          "in": "body",
          "name": "AantalThuiswonendeKinderen"
        },
        "AantalAutos": {
          "in": "body",
          "name": "AantalAutos"
        },
        "NettoBesteedbaarInkomenPerMaand": {
          "in": "body",
          "name": "NettoBesteedbaarInkomenPerMaand"
        },
        "BijzondereUitgaven": {
          "in": "body",
          "name": "BijzondereUitgaven"
        },
        "Woning": {
          "in": "body",
          "name": "Woning"
        }
      },
      "fixed_params": {},
      "notes": "Documentation-backed discovery link; endpoint and execution contract remain inactive/draft."
    }
  ]
}$bundle_697743d2ef082059a865806b93585cba29979e003feb0b44d11408941ae7cf62$::jsonb;
  src jsonb;
  item jsonb;
  requirement jsonb;
  source_id uuid;
  endpoint_id uuid;
  endpoint_ids jsonb := '{}'::jsonb;
  endpoint_row providers.api_endpoints%ROWTYPE;
  contract_row catalog.capability_execution_contracts%ROWTYPE;
  credential_values jsonb := coalesce(nullif(current_setting('apiosk.onboarding_secrets', true), ''), '{}')::jsonb;
  secret_name text;
  fact_slug text;
  reused jsonb;
BEGIN
  IF bundle->>'format' IS DISTINCT FROM 'apiosk.source-bundle.v1' THEN
    RAISE EXCEPTION 'Unsupported source bundle format';
  END IF;
  src := bundle->'source';
  IF jsonb_typeof(src) IS DISTINCT FROM 'object'
     OR coalesce(src->>'slug','') !~ '^[a-z0-9]+(-[a-z0-9]+)*$'
     OR nullif(btrim(src->>'name'),'') IS NULL
     OR nullif(btrim(src->>'description'),'') IS NULL
     OR coalesce(src->>'origin_url','') !~ '^https://[^/@?#]+(:[0-9]+)?(/[^?#]*)?$'
     OR coalesce(src->>'docs_url','') !~ '^https://'
     OR nullif(src->>'category','') IS NULL
     OR src->>'owner_id' IS NULL THEN
    RAISE EXCEPTION 'Source identity, existing owner UUID, HTTPS origin/docs and category are required';
  END IF;
  IF NOT EXISTS (SELECT 1 FROM auth.users WHERE id=(src->>'owner_id')::uuid) THEN
    RAISE EXCEPTION 'Source owner must be an existing auth.users ID';
  END IF;
  IF jsonb_typeof(bundle->'endpoints') IS DISTINCT FROM 'array'
     OR jsonb_array_length(bundle->'endpoints')=0 THEN
    RAISE EXCEPTION 'At least one endpoint is required';
  END IF;
  IF jsonb_typeof(credential_values) IS DISTINCT FROM 'object' THEN
    RAISE EXCEPTION 'Credential input must be a server-bound JSON object';
  END IF;
  IF jsonb_typeof(src->'listing_metadata') IS DISTINCT FROM 'object'
     OR jsonb_typeof(src->'authentication_info') IS DISTINCT FROM 'object'
     OR jsonb_typeof(src->'static_headers') IS DISTINCT FROM 'array' THEN
    RAISE EXCEPTION 'Source metadata/authentication must be objects and static_headers an array';
  END IF;
  -- Serializes this importer for the same slug; the unique constraint also
  -- protects against other writers. Existing sources require a separate update.
  PERFORM pg_advisory_xact_lock(hashtextextended('source-bundle:'||(src->>'slug'), 0));
  IF EXISTS (SELECT 1 FROM providers.apis WHERE slug=src->>'slug') THEN
    RAISE EXCEPTION 'Source slug already exists; inspect prior import instead of overwriting';
  END IF;
  IF coalesce(src->'listing_metadata','{}') ?| ARRAY['auth_headers','auth_query','planner_block_reason','onboarding'] THEN
    RAISE EXCEPTION 'Use static_headers/credential_requirements; reserved metadata cannot be supplied';
  END IF;
  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(src->'static_headers','[]')) LOOP
    -- These are public protocol settings. Credentials have a separate channel.
    IF lower(coalesce(item->>'name','')) NOT IN ('accept','content-type','accept-language')
       OR nullif(item->>'value','') IS NULL OR (item->>'value') ~ '[\r\n]' THEN
      RAISE EXCEPTION 'Only public Accept, Content-Type and Accept-Language static headers are supported by this template';
    END IF;
  END LOOP;

  INSERT INTO providers.apis (
    owner_id,slug,name,origin_url,endpoint_url,description,long_description,
    category,docs_url,website_url,github_url,example_usage,authentication_info,
    status,active,discoverable,is_verified,listing_type,is_platform_owned,
    is_external,is_community,source,integration_mode,settlement_mode,
    requires_payout_wallet,listing_metadata
  ) VALUES (
    (src->>'owner_id')::uuid,src->>'slug',src->>'name',src->>'origin_url',src->>'origin_url',
    src->>'description',src->>'long_description',src->>'category',src->>'docs_url',
    src->>'website_url',src->>'github_url',src->>'example_usage',src->'authentication_info',
    'inactive',false,false,false,'api',true,false,false,'manual','gateway','apiosk_revenue',false,
    coalesce(src->'listing_metadata','{}') || jsonb_build_object(
      'auth_headers',coalesce(src->'static_headers','[]'),
      'planner_block_reason','Onboarding: credentials, runtime contract and live acceptance await review',
      'onboarding',jsonb_build_object('format',bundle->>'format','bundle_id',bundle->>'bundle_id',
        'provenance',bundle->'provenance','credential_requirements',coalesce(bundle->'credential_requirements','[]'),
        'state','staged'))
  ) RETURNING id INTO source_id;
  -- Production's owner trigger auto-verifies on INSERT. Explicitly reset this
  -- metadata so staging does not imply that an upstream test was successful.
  UPDATE providers.apis SET is_verified=false,verified_at=NULL WHERE id=source_id;

  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(bundle->'subject_kinds','[]')) LOOP
    INSERT INTO catalog.fact_subject_kinds(slug,description)
    VALUES(item->>'slug',item->>'description') ON CONFLICT(slug) DO NOTHING;
  END LOOP;

  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(bundle->'fact_types','[]')) LOOP
    INSERT INTO catalog.fact_types(slug,name,description,subject_kind,jurisdiction,
      value_schema,freshness_seconds,match_hints,status)
    VALUES(item->>'slug',item->>'name',item->>'description',item->>'subject_kind',item->>'jurisdiction',
      item->'value_schema',(item->>'freshness_seconds')::integer,coalesce(item->'match_hints','{}'),'draft')
    ON CONFLICT(slug) DO NOTHING;
    -- Global vocabulary is shared by existing sources. Never redefine it.
    SELECT jsonb_build_object('subject_kind',f.subject_kind,'jurisdiction',f.jurisdiction,
      'value_schema',f.value_schema,'freshness_seconds',f.freshness_seconds,'match_hints',f.match_hints)
      INTO reused FROM catalog.fact_types f WHERE f.slug=item->>'slug';
    IF reused IS DISTINCT FROM jsonb_build_object('subject_kind',item->>'subject_kind',
      'jurisdiction',item->>'jurisdiction','value_schema',item->'value_schema',
      'freshness_seconds',(item->>'freshness_seconds')::integer,'match_hints',coalesce(item->'match_hints','{}')) THEN
      RAISE EXCEPTION 'Shared fact definition differs: %',item->>'slug';
    END IF;
  END LOOP;

  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(bundle->'capabilities','[]')) LOOP
    INSERT INTO catalog.capabilities(slug,name,description,category,input_contract,output_contract,status)
    VALUES(item->>'slug',item->>'name',item->>'description',item->>'category',
      coalesce(item->'input_contract','{}'),coalesce(item->'output_contract','{}'),'draft')
    ON CONFLICT(slug) DO NOTHING;
    SELECT jsonb_build_object('name',c.name,'description',c.description,'category',c.category,
      'input_contract',c.input_contract,'output_contract',c.output_contract)
      INTO reused FROM catalog.capabilities c WHERE c.slug=item->>'slug';
    IF reused IS DISTINCT FROM jsonb_build_object('name',item->>'name','description',item->>'description',
      'category',item->>'category','input_contract',coalesce(item->'input_contract','{}'),
      'output_contract',coalesce(item->'output_contract','{}')) THEN
      RAISE EXCEPTION 'Shared capability definition differs: %',item->>'slug';
    END IF;
  END LOOP;

  FOR item IN SELECT value FROM jsonb_array_elements(bundle->'endpoints') LOOP
    IF nullif(item->>'key','') IS NULL OR endpoint_ids ? (item->>'key') THEN
      RAISE EXCEPTION 'Each endpoint needs a unique bundle-local key';
    END IF;
    endpoint_row := jsonb_populate_record(NULL::providers.api_endpoints,item-'key');
    IF endpoint_row.method NOT IN ('GET','POST') OR endpoint_row.method IS NULL
       OR coalesce(endpoint_row.path,'') !~ '^/[^?#]*$' OR endpoint_row.path LIKE '//%'
       OR endpoint_row.price IS NULL OR endpoint_row.price<0
       OR endpoint_row.price<>round(endpoint_row.price,6)
       OR nullif(btrim(endpoint_row.description),'') IS NULL
       OR endpoint_row.auth_override IS NOT NULL THEN
      RAISE EXCEPTION 'Invalid endpoint: require read GET/POST, relative path, micro-USD precision, description; secret auth_override is unsupported';
    END IF;
    IF endpoint_row.origin_override IS NOT NULL AND endpoint_row.origin_override !~ '^https://[^/@?#]+(:[0-9]+)?(/[^?#]*)?$' THEN
      RAISE EXCEPTION 'Origin override must be a reviewed HTTPS origin without credentials/query/fragment';
    END IF;
    IF jsonb_typeof(endpoint_row.query_params->'properties') IS DISTINCT FROM 'object'
       AND jsonb_typeof(endpoint_row.request_body->'properties') IS DISTINCT FROM 'object' THEN
      RAISE EXCEPTION 'Endpoint must document its inputs, including properties={} for no inputs';
    END IF;
    INSERT INTO providers.api_endpoints (
      api_id,method,path,payment_required,price,description,request_body,response_body,
      request_example,response_example,headers,query_params,active,documentation_markdown,
      external_docs_url,openapi_url,authentication_notes,origin_override,cache_ttl_seconds,
      tags,search_terms,use_cases,sectors,input_entities,output_entities,endpoint_category,
      action_type,task_summary,metadata_source,metadata_model,metadata_confidence,metadata_at,contract_documented
    ) VALUES (
      source_id,endpoint_row.method,endpoint_row.path,endpoint_row.price>0,endpoint_row.price,
      endpoint_row.description,endpoint_row.request_body,endpoint_row.response_body,
      endpoint_row.request_example,endpoint_row.response_example,endpoint_row.headers,
      endpoint_row.query_params,false,endpoint_row.documentation_markdown,
      endpoint_row.external_docs_url,endpoint_row.openapi_url,endpoint_row.authentication_notes,
      endpoint_row.origin_override,endpoint_row.cache_ttl_seconds,endpoint_row.tags,
      endpoint_row.search_terms,endpoint_row.use_cases,endpoint_row.sectors,
      endpoint_row.input_entities,endpoint_row.output_entities,endpoint_row.endpoint_category,
      endpoint_row.action_type,endpoint_row.task_summary,endpoint_row.metadata_source,
      endpoint_row.metadata_model,endpoint_row.metadata_confidence,endpoint_row.metadata_at,
      coalesce(endpoint_row.contract_documented,false)
    ) RETURNING id INTO endpoint_id;
    endpoint_ids := endpoint_ids || jsonb_build_object(item->>'key',endpoint_id);
  END LOOP;

  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(bundle->'contracts','[]')) LOOP
    endpoint_id := (endpoint_ids->>(item->>'endpoint_key'))::uuid;
    IF endpoint_id IS NULL THEN RAISE EXCEPTION 'Unknown contract endpoint_key'; END IF;
    contract_row := jsonb_populate_record(NULL::catalog.capability_execution_contracts,item-'endpoint_key');
    IF coalesce(item->>'effect','read')<>'read' OR coalesce(item->>'price_model','per_call')<>'per_call'
       OR coalesce(item->'preconditions','{}')<>'{}'::jsonb THEN
      RAISE EXCEPTION 'Current Ask runtime requires read/per_call and empty preconditions';
    END IF;
    IF jsonb_typeof(item->'requires') IS DISTINCT FROM 'array'
       OR jsonb_typeof(item->'produces') IS DISTINCT FROM 'array'
       OR jsonb_typeof(item->'input_map') IS DISTINCT FROM 'object'
       OR jsonb_typeof(item->'output_map'->'produces') IS DISTINCT FROM 'object'
       OR jsonb_array_length(item->'produces')=0 THEN
      RAISE EXCEPTION 'Contract requires typed requires/produces and input/output maps';
    END IF;
    FOR requirement IN SELECT value FROM jsonb_array_elements((item->'requires')||(item->'produces')) LOOP
      fact_slug := requirement->>'fact_type';
      IF requirement->>'subject_role' IS DISTINCT FROM 'subject'
         OR NOT EXISTS(SELECT 1 FROM catalog.fact_types WHERE slug=fact_slug) THEN
        RAISE EXCEPTION 'Unknown fact or unsupported subject role: %',fact_slug;
      END IF;
    END LOOP;
    IF EXISTS(SELECT 1 FROM jsonb_array_elements(item->'requires') r
      WHERE NOT (item->'input_map' ? (r->>'fact_type')))
      OR EXISTS(SELECT 1 FROM jsonb_array_elements(item->'produces') r
      WHERE NOT (item->'output_map'->'produces' ? (r->>'fact_type'))) THEN
      RAISE EXCEPTION 'Every declared input/output requires a mapping';
    END IF;
    INSERT INTO catalog.capability_execution_contracts (
      capability_slug,endpoint_id,version,status,requires,produces,input_map,fixed_params,
      output_map,cardinality,identifies,max_fan_out,preconditions,pagination,effect,price_model,
      freshness_seconds,adapter_version,sample_payload,notes
    ) VALUES (
      contract_row.capability_slug,endpoint_id,coalesce(contract_row.version,1),'draft',
      contract_row.requires,contract_row.produces,contract_row.input_map,
      coalesce(contract_row.fixed_params,'{}'),contract_row.output_map,
      coalesce(contract_row.cardinality,'one'),contract_row.identifies,
      coalesce(contract_row.max_fan_out,1),'{}',coalesce(contract_row.pagination,'{}'),
      'read','per_call',contract_row.freshness_seconds,coalesce(contract_row.adapter_version,'v1'),
      contract_row.sample_payload,contract_row.notes
    );
  END LOOP;

  -- Optional legacy discovery links. These do NOT authorize execution.
  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(bundle->'offers','[]')) LOOP
    endpoint_id := (endpoint_ids->>(item->>'endpoint_key'))::uuid;
    IF endpoint_id IS NULL THEN RAISE EXCEPTION 'Unknown offer endpoint_key'; END IF;
    INSERT INTO catalog.capability_offers(capability_slug,endpoint_id,api_id,source,confidence,
      input_map,fixed_params,output_map,notes)
    VALUES(item->>'capability_slug',endpoint_id,source_id,coalesce(item->>'source','inferred'),
      coalesce((item->>'confidence')::numeric,0),coalesce(item->'input_map','{}'),
      coalesce(item->'fixed_params','{}'),item->'output_map',item->>'notes');
  END LOOP;

  -- Optional atomic Vault provisioning: values arrive only through a bound
  -- server connection, not the AI manifest. No existing key is overwritten.
  FOR item IN SELECT value FROM jsonb_array_elements(coalesce(bundle->'credential_requirements','[]')) LOOP
    secret_name := item->>'vault_name';
    IF coalesce(secret_name,'') !~ '^[A-Z][A-Z0-9_]+$' THEN RAISE EXCEPTION 'Invalid Vault name'; END IF;
    IF item->>'endpoint_key' IS NOT NULL AND NOT endpoint_ids ? (item->>'endpoint_key') THEN
      RAISE EXCEPTION 'Unknown credential endpoint_key';
    END IF;
    IF credential_values ? secret_name THEN
      IF jsonb_typeof(credential_values->secret_name)<>'string'
         OR nullif(btrim(credential_values->>secret_name),'') IS NULL THEN
        RAISE EXCEPTION 'Credential must be a nonempty string';
      END IF;
      IF EXISTS(SELECT 1 FROM vault.secrets WHERE name=secret_name) THEN
        RAISE EXCEPTION 'Vault name already exists; key rotation requires a separate explicit operation';
      END IF;
      PERFORM vault.create_secret(credential_values->>secret_name,secret_name,
        'Apiosk source onboarding: '||(src->>'slug')||'; runtime binding must be implemented and tested');
    END IF;
  END LOOP;
  IF EXISTS(SELECT 1 FROM jsonb_object_keys(credential_values) k WHERE NOT EXISTS(
    SELECT 1 FROM jsonb_array_elements(coalesce(bundle->'credential_requirements','[]')) r WHERE r->>'vault_name'=k)) THEN
    RAISE EXCEPTION 'Credential input contains an undeclared Vault name';
  END IF;
  PERFORM set_config('apiosk.onboarding_secrets','',true);
  RAISE NOTICE 'Source % registered inactive with % endpoints; contracts remain draft. No live execution has been certified.',src->>'slug',jsonb_array_length(bundle->'endpoints');
END;
$block_697743d2ef082059a865806b93585cba29979e003feb0b44d11408941ae7cf62$;


UPDATE providers.api_endpoints e SET tags=(SELECT array_agg(DISTINCT v ORDER BY v) FROM unnest(coalesce(e.tags,'{}')||ARRAY(SELECT jsonb_array_elements_text('["officiële bekendmakingen","gemeenteblad","Staatscourant","wijzigingen","besluiten","bijstand"]'::jsonb))) v), search_terms=(SELECT array_agg(DISTINCT v ORDER BY v) FROM unnest(coalesce(e.search_terms,'{}')||ARRAY(SELECT jsonb_array_elements_text('["officiële bekendmakingen","gemeenteblad","Staatscourant","wijzigingen","besluiten","bijstand"]'::jsonb))) v), sectors=(SELECT array_agg(DISTINCT v ORDER BY v) FROM unnest(coalesce(e.sectors,'{}')||ARRAY['Government and public data','Legal']) v), input_entities=(SELECT array_agg(DISTINCT v ORDER BY v) FROM unnest(coalesce(e.input_entities,'{}')||ARRAY(SELECT jsonb_array_elements_text('["trefwoord","publicatie-ID","publicerende overheid","publicatiedatum"]'::jsonb))) v), output_entities=(SELECT array_agg(DISTINCT v ORDER BY v) FROM unnest(coalesce(e.output_entities,'{}')||ARRAY(SELECT jsonb_array_elements_text('["publicatiemetadata","bronlinks","publicatiedatum","vervolgpagina"]'::jsonb))) v), use_cases=(SELECT array_agg(DISTINCT v ORDER BY v) FROM unnest(coalesce(e.use_cases,'{}')||ARRAY(SELECT jsonb_array_elements_text('["Zoek officiële publicaties en besluiten","Zoek aankondigingen over bijstand en lokale regelingen"]'::jsonb))) v), endpoint_category='government', action_type='search', task_summary='"Zoek officiële bekendmakingen op titel, overheid en publicatiedatum; één pagina."'::jsonb#>>'{}', metadata_source='provider_documentation_and_live_probe', metadata_at=now() FROM providers.apis a WHERE a.id=e.api_id AND a.slug='koop' AND e.method='GET' AND e.path='/sru';
UPDATE providers.api_endpoints e SET tags=(SELECT array_agg(DISTINCT v ORDER BY v) FROM unnest(coalesce(e.tags,'{}')||ARRAY(SELECT jsonb_array_elements_text('["BWB","CVDR","wetten.nl","lokale regelgeving","bijzondere bijstand","individuele inkomenstoeslag","Participatiewet","toeslagen"]'::jsonb))) v), search_terms=(SELECT array_agg(DISTINCT v ORDER BY v) FROM unnest(coalesce(e.search_terms,'{}')||ARRAY(SELECT jsonb_array_elements_text('["BWB","CVDR","wetten.nl","lokale regelgeving","bijzondere bijstand","individuele inkomenstoeslag","Participatiewet","toeslagen"]'::jsonb))) v), sectors=(SELECT array_agg(DISTINCT v ORDER BY v) FROM unnest(coalesce(e.sectors,'{}')||ARRAY['Government and public data','Legal']) v), input_entities=(SELECT array_agg(DISTINCT v ORDER BY v) FROM unnest(coalesce(e.input_entities,'{}')||ARRAY(SELECT jsonb_array_elements_text('["trefwoord","BWB of CVDR collectie","startrecord"]'::jsonb))) v), output_entities=(SELECT array_agg(DISTINCT v ORDER BY v) FROM unnest(coalesce(e.output_entities,'{}')||ARRAY(SELECT jsonb_array_elements_text('["wetgevingsmetadata","versiegegevens","bronlinks","vervolgpagina"]'::jsonb))) v), use_cases=(SELECT array_agg(DISTINCT v ORDER BY v) FROM unnest(coalesce(e.use_cases,'{}')||ARRAY(SELECT jsonb_array_elements_text('["Zoek landelijke wetgeving in BWB","Zoek gemeentelijke regelingen en beleidsregels in CVDR"]'::jsonb))) v), endpoint_category='government', action_type='search', task_summary='"Zoek BWB- en CVDR-regelingstitels; controleer versie en geldigheid in het brondocument."'::jsonb#>>'{}', metadata_source='provider_documentation_and_live_probe', metadata_at=now() FROM providers.apis a WHERE a.id=e.api_id AND a.slug='koop' AND e.method='GET' AND e.path='/sru/Search';
UPDATE providers.apis SET listing_metadata=listing_metadata || jsonb_build_object('nl_benefits_research','{"checked_at":"2026-10-13","documentation_url":"https://data.overheid.nl/sites/default/files/dataset/d0cca537-44ea-48cf-9880-fa21e1a7058f/resources/HandleidingSRU2.0.pdf","collections":["officielepublicaties","BWB","cvdr"],"capability_aliases":{"nl.legal.search":"government.bwb.search","nl.municipal.regulations":"government.cvdr.search"},"changes_feed":{"url":"https://repository.officiele-overheidspublicaties.nl/officielepublicaties/_events/","state":"documented_not_integrated"},"scope":"Existing title-search contracts retained. No municipal filter or full-text retrieval added."}'::jsonb),docs_url='https://data.overheid.nl/sites/default/files/dataset/d0cca537-44ea-48cf-9880-fa21e1a7058f/resources/HandleidingSRU2.0.pdf' WHERE slug='koop';
COMMIT;
