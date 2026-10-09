-- Stage managed Bright Data LinkedIn scraping. No credential or live-validation
-- claim belongs in this migration. Deployment and credential validation activate it.
-- Public-page jobs are internal, keyed by scraper kind + canonical URL. Snapshots
-- cannot be requested by callers and must never be included in public responses.
BEGIN;
SELECT pg_advisory_xact_lock(hashtext('apiosk-brightdata-linkedin-v1'));

CREATE SCHEMA IF NOT EXISTS gateway_private;
REVOKE ALL ON SCHEMA gateway_private FROM PUBLIC, anon, authenticated;
CREATE TABLE IF NOT EXISTS gateway_private.brightdata_linkedin_jobs (
  request_key text PRIMARY KEY CHECK (request_key ~ '^[0-9a-f]{64}$'),
  kind text NOT NULL CHECK (kind IN ('profile','company','job','post')),
  source_url text NOT NULL CHECK (length(source_url) BETWEEN 1 AND 2048),
  state text NOT NULL CHECK (state IN ('submitting','running','ready','failed','unknown')),
  snapshot_id text,
  result jsonb,
  error_code text,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  expires_at timestamptz NOT NULL DEFAULT now() + interval '1 hour'
);
CREATE INDEX IF NOT EXISTS brightdata_linkedin_jobs_expiry
  ON gateway_private.brightdata_linkedin_jobs(expires_at);
ALTER TABLE gateway_private.brightdata_linkedin_jobs ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON gateway_private.brightdata_linkedin_jobs FROM PUBLIC, anon, authenticated;
GRANT USAGE ON SCHEMA gateway_private TO apiosk_gateway, service_role;
GRANT SELECT,INSERT,UPDATE,DELETE ON gateway_private.brightdata_linkedin_jobs TO apiosk_gateway, service_role;
DROP POLICY IF EXISTS brightdata_executor_access ON gateway_private.brightdata_linkedin_jobs;
CREATE POLICY brightdata_executor_access ON gateway_private.brightdata_linkedin_jobs
  TO apiosk_gateway USING (true) WITH CHECK (true);
COMMENT ON TABLE gateway_private.brightdata_linkedin_jobs IS
  'Internal Bright Data LinkedIn collection state. Never expire, delete or automatically resubmit submitting/unknown rows: they may represent a paid upstream request. Only ready/failed jobs may be renewed. No caller-provided snapshot IDs.';

DO $onboard$
DECLARE
  platform_owner uuid;
  source_id uuid;
  metadata jsonb := $metadata${"adapter":"brightdata-linkedin-v1","capability_adapter":"brightdata-linkedin-v1","catalog_state":"staged","staging_reason":"missing_server_credential_and_live_validation","credential":{"configured":false,"location":"server_environment","env":"BRIGHTDATA_API_KEY","header":"Authorization","scheme":"Bearer"},"billing":{"mode":"apiosk_balance","list_micro_usd":1500,"basis":"one LinkedIn page URL per purchase","buyer_markup_percent":15},"tags":["bright data","brightdata","linkedin","web scraping","person profiles","company profiles","job postings","social media posts"],"sectors":["Company data","Recruitment","Sales","Marketing","Research"],"coverage_notice":"Public LinkedIn data returned by Bright Data. Profile position, experience and education may be absent or cached from November 13, 2025 or earlier; current_company.industry has been unsupported since August 7, 2025. Other fields may also be absent. This is not verified identity or an exhaustive search.","operator_notice":"Bright Data operates the scraper; LinkedIn is the source website.","activation_requirements":["Server-side Bright Data credential provisioned","Bright Data LinkedIn adapter deployed","Successful live provider and purchase checks"],"service_groups":[{"name":"LinkedIn scraping","description":"Retrieve one public LinkedIn person profile, company page, job posting or post from its exact URL.","endpoint_paths":["/linkedin/profile","/linkedin/company","/linkedin/job","/linkedin/post"]}],"provider":{"name":"Bright Data","website":"https://brightdata.com","docs":"https://docs.brightdata.com/products/scrapers/linkedin/send-first-request"},"coverage_notice_url":"https://docs.brightdata.com/products/scrapers/linkedin/data-policy-change","usage_notice":{"text":"Public LinkedIn data returned by Bright Data. Profile position, experience and education may be absent or cached from November 13, 2025 or earlier; current_company.industry has been unsupported since August 7, 2025. Other fields may also be absent. This is not verified identity or an exhaustive search.","url":"https://docs.brightdata.com/products/scrapers/linkedin/data-policy-change","label":"Bright Data LinkedIn field availability"}}$metadata$::jsonb;
  endpoint jsonb;
BEGIN
  SELECT owner_id INTO STRICT platform_owner FROM providers.apis
  WHERE slug='koop' AND is_platform_owned AND settlement_mode='apiosk_revenue';
  IF EXISTS (SELECT 1 FROM providers.apis WHERE slug='brightdata' AND
    (owner_id IS DISTINCT FROM platform_owner OR NOT coalesce(is_platform_owned,false)
      OR settlement_mode IS DISTINCT FROM 'apiosk_revenue'
      OR listing_metadata->>'adapter' IS DISTINCT FROM 'brightdata-linkedin-v1'
      OR status='active' OR coalesce(active,false) OR coalesce(discoverable,false))) THEN
    RAISE EXCEPTION 'Bright Data listing conflicts with staged managed source; do not overwrite an existing live listing';
  END IF;
  INSERT INTO providers.apis(
    id,owner_id,slug,name,status,description,long_description,category,
    docs_url,website_url,origin_url,endpoint_url,authentication_info,is_verified,
    active,discoverable,listing_type,listing_metadata,integration_mode,settlement_mode,
    is_external,is_platform_owned,is_community,source,requires_payout_wallet
  ) VALUES(
    gen_random_uuid(),platform_owner,'brightdata','Bright Data — LinkedIn Scraping','inactive',
    'Scrape public LinkedIn person profiles, company pages, job postings and posts from exact URLs.',
    'Managed Bright Data scraping of one public LinkedIn page per operation. Retrieve available profile, company, job or post fields from a known URL. Results may omit unavailable fields. This source is staged until its credential, adapter deployment and live requests are verified.',
    'data','https://docs.brightdata.com/products/scrapers/linkedin/send-first-request',
    'https://brightdata.com','https://api.brightdata.com','https://api.brightdata.com',
    '{"type":"apiKey","caller":"Pay per completed operation through the Apiosk balance; callers do not provide a Bright Data key.","upstream":{"header":"Authorization","scheme":"Bearer","note":"Dedicated server secret BRIGHTDATA_API_KEY; never stored in public catalog metadata."}}'::jsonb,
    false,false,false,'api',metadata,'gateway','apiosk_revenue',false,true,false,'manual',false
  ) ON CONFLICT(slug) DO NOTHING;
  SELECT id INTO STRICT source_id FROM providers.apis WHERE slug='brightdata' AND owner_id=platform_owner;
  -- Platform-owner identity verification runs on INSERT, even for a staged API.
  -- This source has no live execution evidence, so clear that insertion default.
  UPDATE providers.apis SET is_verified=false,verified_at=NULL WHERE id=source_id;
  FOR endpoint IN SELECT value FROM jsonb_array_elements($endpoints$[
  {
    "kind": "profile",
    "path": "/linkedin/profile",
    "description": "Scrape one public LinkedIn person profile by its exact URL. Returns available public profile fields. Position, experience and education may be absent or cached from November 13, 2025 or earlier; current_company.industry has been unsupported since August 7, 2025.",
    "request_body": {
      "type": "object",
      "required": [
        "url"
      ],
      "additionalProperties": false,
      "properties": {
        "url": {
          "type": "string",
          "minLength": 1,
          "maxLength": 2048,
          "pattern": "^https://(www\\.)?linkedin\\.com/in/[A-Za-z0-9_-]+/?$",
          "description": "Exact public LinkedIn profile HTTPS URL. No query string, fragment, credentials or additional inputs.",
          "example": "https://www.linkedin.com/in/satyanadella"
        }
      }
    },
    "response_body": {
      "type": "object",
      "required": [
        "data",
        "source_url",
        "notice",
        "coverage"
      ],
      "additionalProperties": false,
      "properties": {
        "data": {
          "type": "array",
          "minItems": 1,
          "maxItems": 1,
          "items": {
            "type": "object",
            "additionalProperties": true
          },
          "description": "The single public-page record returned by Bright Data; provider fields vary and are not guaranteed."
        },
        "source_url": {
          "type": "string",
          "description": "Canonical LinkedIn URL requested."
        },
        "notice": {
          "type": "string"
        },
        "coverage": {
          "type": "object",
          "required": [
            "complete",
            "scope"
          ],
          "additionalProperties": false,
          "properties": {
            "complete": {
              "type": "boolean",
              "const": false
            },
            "scope": {
              "type": "string"
            }
          }
        }
      }
    },
    "request_example": {
      "url": "https://www.linkedin.com/in/satyanadella"
    },
    "tags": [
      "bright data",
      "linkedin",
      "web scraping",
      "linkedin profile",
      "person profile",
      "public profile",
      "historical experience",
      "historical education"
    ],
    "search_terms": [
      "read public linkedin person profile",
      "scrape available public profile fields",
      "linkedin profiel ophalen"
    ],
    "use_cases": [
      "Read the public professional profile at a known LinkedIn URL"
    ],
    "sectors": [
      "Recruitment",
      "Sales",
      "Research"
    ],
    "input_entities": [
      "linkedin profile URL"
    ],
    "output_entities": [
      "public person profile",
      "available historical experience",
      "available historical education"
    ],
    "task_summary": "Scrape one public LinkedIn profile by URL",
    "documentation_markdown": "POST /linkedin/profile accepts one JSON object with one url. This Apiosk operation uses the maintained Bright Data LinkedIn scraper for this page type. Only the exact public page is retrieved; no discovery, batch URLs, login credentials or custom dataset IDs are accepted.\n\nOne URL is priced at USD 0.0015 before the standard Apiosk buyer fee. The adapter waits for the provider collection through a server-side durable job. A pending collection returns HTTP 503 with Retry-After and no successful buyer settlement; retry the same URL after the stated interval. Snapshot IDs and the Bright Data key are never caller inputs. A completed public-page result may be reused for up to one hour.\n\nPublic LinkedIn page data returned by Bright Data. Fields may be absent or stale; this is not verified identity, contact permission, or an exhaustive search.\n\nThis listing is staged: no live provider response or successful purchase is claimed. The request example is illustrative and may no longer be available. Activation requires the server-side credential, deployed adapter and a successful live check.\n\nProvider references: https://docs.brightdata.com/products/scrapers/linkedin/send-first-request ; https://docs.brightdata.com/api-reference/rest-api/scraper/asynchronous-requests ; https://docs.brightdata.com/api-reference/scrapers/management-apis/monitor-progress\n\nField availability: Public LinkedIn data returned by Bright Data. Profile position, experience and education may be absent or cached from November 13, 2025 or earlier; current_company.industry has been unsupported since August 7, 2025. Other fields may also be absent. This is not verified identity or an exhaustive search. See https://docs.brightdata.com/products/scrapers/linkedin/data-policy-change."
  },
  {
    "kind": "company",
    "path": "/linkedin/company",
    "description": "Scrape one public LinkedIn company page by its exact company URL. Returns available company description, industry, employee counts, locations and website.",
    "request_body": {
      "type": "object",
      "required": [
        "url"
      ],
      "additionalProperties": false,
      "properties": {
        "url": {
          "type": "string",
          "minLength": 1,
          "maxLength": 2048,
          "pattern": "^https://(www\\.)?linkedin\\.com/company/[A-Za-z0-9_-]+/?$",
          "description": "Exact public LinkedIn company HTTPS URL. No query string, fragment, credentials or additional inputs.",
          "example": "https://www.linkedin.com/company/microsoft"
        }
      }
    },
    "response_body": {
      "type": "object",
      "required": [
        "data",
        "source_url",
        "notice",
        "coverage"
      ],
      "additionalProperties": false,
      "properties": {
        "data": {
          "type": "array",
          "minItems": 1,
          "maxItems": 1,
          "items": {
            "type": "object",
            "additionalProperties": true
          },
          "description": "The single public-page record returned by Bright Data; provider fields vary and are not guaranteed."
        },
        "source_url": {
          "type": "string",
          "description": "Canonical LinkedIn URL requested."
        },
        "notice": {
          "type": "string"
        },
        "coverage": {
          "type": "object",
          "required": [
            "complete",
            "scope"
          ],
          "additionalProperties": false,
          "properties": {
            "complete": {
              "type": "boolean",
              "const": false
            },
            "scope": {
              "type": "string"
            }
          }
        }
      }
    },
    "request_example": {
      "url": "https://www.linkedin.com/company/microsoft"
    },
    "tags": [
      "bright data",
      "linkedin",
      "web scraping",
      "linkedin company",
      "company profile",
      "company data",
      "employee count",
      "industry"
    ],
    "search_terms": [
      "scrape linkedin company page",
      "linkedin business profile",
      "linkedin bedrijfsprofiel ophalen"
    ],
    "use_cases": [
      "Read a company profile from its known LinkedIn company URL"
    ],
    "sectors": [
      "Company data",
      "Sales",
      "Research"
    ],
    "input_entities": [
      "linkedin company URL"
    ],
    "output_entities": [
      "company profile",
      "industry",
      "employee count",
      "company website"
    ],
    "task_summary": "Scrape one public LinkedIn company by URL",
    "documentation_markdown": "POST /linkedin/company accepts one JSON object with one url. This Apiosk operation uses the maintained Bright Data LinkedIn scraper for this page type. Only the exact public page is retrieved; no discovery, batch URLs, login credentials or custom dataset IDs are accepted.\n\nOne URL is priced at USD 0.0015 before the standard Apiosk buyer fee. The adapter waits for the provider collection through a server-side durable job. A pending collection returns HTTP 503 with Retry-After and no successful buyer settlement; retry the same URL after the stated interval. Snapshot IDs and the Bright Data key are never caller inputs. A completed public-page result may be reused for up to one hour.\n\nPublic LinkedIn page data returned by Bright Data. Fields may be absent or stale; this is not verified identity, contact permission, or an exhaustive search.\n\nThis listing is staged: no live provider response or successful purchase is claimed. The request example is illustrative and may no longer be available. Activation requires the server-side credential, deployed adapter and a successful live check.\n\nProvider references: https://docs.brightdata.com/products/scrapers/linkedin/send-first-request ; https://docs.brightdata.com/api-reference/rest-api/scraper/asynchronous-requests ; https://docs.brightdata.com/api-reference/scrapers/management-apis/monitor-progress"
  },
  {
    "kind": "job",
    "path": "/linkedin/job",
    "description": "Scrape one public LinkedIn job posting by its exact numeric job URL. Returns available job title, employer, location, description and employment details.",
    "request_body": {
      "type": "object",
      "required": [
        "url"
      ],
      "additionalProperties": false,
      "properties": {
        "url": {
          "type": "string",
          "minLength": 1,
          "maxLength": 2048,
          "pattern": "^https://(www\\.)?linkedin\\.com/jobs/view/[0-9]+/?$",
          "description": "Exact public LinkedIn job HTTPS URL. No query string, fragment, credentials or additional inputs.",
          "example": "https://www.linkedin.com/jobs/view/1234567890"
        }
      }
    },
    "response_body": {
      "type": "object",
      "required": [
        "data",
        "source_url",
        "notice",
        "coverage"
      ],
      "additionalProperties": false,
      "properties": {
        "data": {
          "type": "array",
          "minItems": 1,
          "maxItems": 1,
          "items": {
            "type": "object",
            "additionalProperties": true
          },
          "description": "The single public-page record returned by Bright Data; provider fields vary and are not guaranteed."
        },
        "source_url": {
          "type": "string",
          "description": "Canonical LinkedIn URL requested."
        },
        "notice": {
          "type": "string"
        },
        "coverage": {
          "type": "object",
          "required": [
            "complete",
            "scope"
          ],
          "additionalProperties": false,
          "properties": {
            "complete": {
              "type": "boolean",
              "const": false
            },
            "scope": {
              "type": "string"
            }
          }
        }
      }
    },
    "request_example": {
      "url": "https://www.linkedin.com/jobs/view/1234567890"
    },
    "tags": [
      "bright data",
      "linkedin",
      "web scraping",
      "linkedin job",
      "job posting",
      "vacancy",
      "employer",
      "recruitment"
    ],
    "search_terms": [
      "scrape linkedin job posting",
      "read job description",
      "linkedin vacature ophalen"
    ],
    "use_cases": [
      "Read the details of a known LinkedIn job posting"
    ],
    "sectors": [
      "Recruitment",
      "Employment",
      "Research"
    ],
    "input_entities": [
      "linkedin job URL"
    ],
    "output_entities": [
      "job posting",
      "job description",
      "employer",
      "employment details"
    ],
    "task_summary": "Scrape one public LinkedIn job by URL",
    "documentation_markdown": "POST /linkedin/job accepts one JSON object with one url. This Apiosk operation uses the maintained Bright Data LinkedIn scraper for this page type. Only the exact public page is retrieved; no discovery, batch URLs, login credentials or custom dataset IDs are accepted.\n\nOne URL is priced at USD 0.0015 before the standard Apiosk buyer fee. The adapter waits for the provider collection through a server-side durable job. A pending collection returns HTTP 503 with Retry-After and no successful buyer settlement; retry the same URL after the stated interval. Snapshot IDs and the Bright Data key are never caller inputs. A completed public-page result may be reused for up to one hour.\n\nPublic LinkedIn page data returned by Bright Data. Fields may be absent or stale; this is not verified identity, contact permission, or an exhaustive search.\n\nThis listing is staged: no live provider response or successful purchase is claimed. The request example is illustrative and may no longer be available. Activation requires the server-side credential, deployed adapter and a successful live check.\n\nProvider references: https://docs.brightdata.com/products/scrapers/linkedin/send-first-request ; https://docs.brightdata.com/api-reference/rest-api/scraper/asynchronous-requests ; https://docs.brightdata.com/api-reference/scrapers/management-apis/monitor-progress"
  },
  {
    "kind": "post",
    "path": "/linkedin/post",
    "description": "Scrape one public LinkedIn post by its exact post URL. Returns available author, post content, publication time and engagement counts.",
    "request_body": {
      "type": "object",
      "required": [
        "url"
      ],
      "additionalProperties": false,
      "properties": {
        "url": {
          "type": "string",
          "minLength": 1,
          "maxLength": 2048,
          "pattern": "^https://(www\\.)?linkedin\\.com/posts/[A-Za-z0-9_-]+/?$",
          "description": "Exact public LinkedIn post HTTPS URL. No query string, fragment, credentials or additional inputs.",
          "example": "https://www.linkedin.com/posts/example_activity-1234567890-abcd"
        }
      }
    },
    "response_body": {
      "type": "object",
      "required": [
        "data",
        "source_url",
        "notice",
        "coverage"
      ],
      "additionalProperties": false,
      "properties": {
        "data": {
          "type": "array",
          "minItems": 1,
          "maxItems": 1,
          "items": {
            "type": "object",
            "additionalProperties": true
          },
          "description": "The single public-page record returned by Bright Data; provider fields vary and are not guaranteed."
        },
        "source_url": {
          "type": "string",
          "description": "Canonical LinkedIn URL requested."
        },
        "notice": {
          "type": "string"
        },
        "coverage": {
          "type": "object",
          "required": [
            "complete",
            "scope"
          ],
          "additionalProperties": false,
          "properties": {
            "complete": {
              "type": "boolean",
              "const": false
            },
            "scope": {
              "type": "string"
            }
          }
        }
      }
    },
    "request_example": {
      "url": "https://www.linkedin.com/posts/example_activity-1234567890-abcd"
    },
    "tags": [
      "bright data",
      "linkedin",
      "web scraping",
      "linkedin post",
      "social media post",
      "post content",
      "engagement",
      "publication time"
    ],
    "search_terms": [
      "scrape linkedin post",
      "read linkedin post content",
      "linkedin bericht ophalen"
    ],
    "use_cases": [
      "Read the content and available engagement of a known LinkedIn post"
    ],
    "sectors": [
      "Marketing",
      "Social media",
      "Research"
    ],
    "input_entities": [
      "linkedin post URL"
    ],
    "output_entities": [
      "post content",
      "author",
      "publication time",
      "engagement counts"
    ],
    "task_summary": "Scrape one public LinkedIn post by URL",
    "documentation_markdown": "POST /linkedin/post accepts one JSON object with one url. This Apiosk operation uses the maintained Bright Data LinkedIn scraper for this page type. Only the exact public page is retrieved; no discovery, batch URLs, login credentials or custom dataset IDs are accepted.\n\nOne URL is priced at USD 0.0015 before the standard Apiosk buyer fee. The adapter waits for the provider collection through a server-side durable job. A pending collection returns HTTP 503 with Retry-After and no successful buyer settlement; retry the same URL after the stated interval. Snapshot IDs and the Bright Data key are never caller inputs. A completed public-page result may be reused for up to one hour.\n\nPublic LinkedIn page data returned by Bright Data. Fields may be absent or stale; this is not verified identity, contact permission, or an exhaustive search.\n\nThis listing is staged: no live provider response or successful purchase is claimed. The request example is illustrative and may no longer be available. Activation requires the server-side credential, deployed adapter and a successful live check.\n\nProvider references: https://docs.brightdata.com/products/scrapers/linkedin/send-first-request ; https://docs.brightdata.com/api-reference/rest-api/scraper/asynchronous-requests ; https://docs.brightdata.com/api-reference/scrapers/management-apis/monitor-progress"
  }
]$endpoints$::jsonb) LOOP
    IF EXISTS (SELECT 1 FROM providers.api_endpoints WHERE api_id=source_id
      AND method='POST' AND path=endpoint->>'path' AND
      (active OR price IS DISTINCT FROM 0.0015 OR request_body IS DISTINCT FROM endpoint->'request_body')) THEN
      RAISE EXCEPTION 'Bright Data endpoint drift: %',endpoint->>'path';
    END IF;
    INSERT INTO providers.api_endpoints(
      id,api_id,method,path,price,active,payment_required,cache_ttl_seconds,
      description,query_params,request_body,response_body,request_example,response_example,
      tags,search_terms,sectors,use_cases,input_entities,output_entities,endpoint_category,
      action_type,external_docs_url,documentation_markdown,metadata_source,metadata_confidence,
      metadata_at,task_summary,contract_documented
    ) SELECT
      gen_random_uuid(),source_id,'POST',endpoint->>'path',0.0015,false,true,0,
      endpoint->>'description','{"type":"object","properties":{},"required":[],"additionalProperties":false}'::jsonb,
      endpoint->'request_body',endpoint->'response_body',endpoint->'request_example',NULL,
      ARRAY(SELECT jsonb_array_elements_text(endpoint->'tags')),
      ARRAY(SELECT jsonb_array_elements_text(endpoint->'search_terms')),
      ARRAY(SELECT jsonb_array_elements_text(endpoint->'sectors')),
      ARRAY(SELECT jsonb_array_elements_text(endpoint->'use_cases')),
      ARRAY(SELECT jsonb_array_elements_text(endpoint->'input_entities')),
      ARRAY(SELECT jsonb_array_elements_text(endpoint->'output_entities')),
      'data','lookup','https://docs.brightdata.com/products/scrapers/linkedin/send-first-request',
      endpoint->>'documentation_markdown','provider_documentation_adapter_contract',1,now(),
      endpoint->>'task_summary',true
    WHERE NOT EXISTS (SELECT 1 FROM providers.api_endpoints WHERE api_id=source_id
      AND method='POST' AND path=endpoint->>'path');
  END LOOP;
  IF (SELECT count(*) FROM providers.api_endpoints WHERE api_id=source_id) <> 4
     OR EXISTS (SELECT 1 FROM providers.api_endpoints WHERE api_id=source_id AND active)
     OR EXISTS (SELECT 1 FROM providers.apis WHERE id=source_id
       AND (status='active' OR coalesce(active,false) OR coalesce(discoverable,false)
         OR coalesce(is_verified,false) OR verified_at IS NOT NULL)) THEN
    RAISE EXCEPTION 'Bright Data staging invariant failed';
  END IF;
END $onboard$;
COMMIT;
