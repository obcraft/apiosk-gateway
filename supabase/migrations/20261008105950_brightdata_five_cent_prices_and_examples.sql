-- Five euro cents at the currently published USD/EUR display rate.
-- Ledger amounts remain USD; no fee rule, currency preference or FX rate changes.
-- 47,258 list micro-USD + ceil(15%) 7,089 = 54,347 buyer micro-USD.
-- At EUR 0.92/USD this displays as EUR 0.05; later FX updates may change that display.
BEGIN;
SELECT pg_advisory_xact_lock(hashtext('apiosk-brightdata-linkedin-v1'));
DO $price_and_examples$
DECLARE
  source_id uuid;
  example jsonb;
  examples constant jsonb := $examples$[
  {
    "path": "/linkedin/profile",
    "question": "What public profile details are available for Satya Nadella on LinkedIn?",
    "inputs": {
      "url": "https://www.linkedin.com/in/satyanadella"
    }
  },
  {
    "path": "/linkedin/company",
    "question": "What does Microsoft's public LinkedIn company page say about the company?",
    "inputs": {
      "url": "https://www.linkedin.com/company/microsoft"
    }
  },
  {
    "path": "/linkedin/job",
    "question": "What role, requirements and location are listed in this LinkedIn job posting?",
    "inputs": {
      "url": "https://www.linkedin.com/jobs/view/4476198009"
    }
  },
  {
    "path": "/linkedin/post",
    "question": "What does Microsoft's LinkedIn post about its AI transformation say?",
    "inputs": {
      "url": "https://www.linkedin.com/posts/microsoft_september-2026-lessons-from-our-ai-transformation-activity-7508888854716628992-JRUU"
    }
  }
]$examples$::jsonb;
  logo constant text := 'https://brightdata.com/wp-content/themes/brightdata/assets/images/favicon.png';
  price_text constant text := 'Standard standalone call: EUR 0.05 including the 15% Apiosk fee at the published rate of EUR 0.92 per USD (2026-08-21). The ledger total is USD 0.054347, from a USD 0.047258 list price. Euro display prices may change with the exchange rate. Existing named-workflow pricing remains separate.';
BEGIN
  SELECT id INTO STRICT source_id FROM providers.apis
    WHERE slug='brightdata'
      AND owner_id=(SELECT owner_id FROM providers.apis WHERE slug='koop')
      AND is_platform_owned AND NOT is_external AND NOT requires_payout_wallet
      AND integration_mode='gateway' AND settlement_mode='apiosk_revenue'
      AND listing_type='api' AND origin_url='https://api.brightdata.com'
      AND endpoint_url='https://api.brightdata.com' AND suspended_at IS NULL
      AND status='active' AND active AND is_verified AND discoverable
      AND listing_metadata->>'adapter'='brightdata-linkedin-v1'
      AND listing_metadata->>'capability_adapter'='brightdata-linkedin-v1'
      AND listing_metadata->>'catalog_state'='published'
      AND listing_metadata #>> '{credential,configured}'='true'
      AND listing_metadata #>> '{credential,env}'='BRIGHTDATA_API_KEY'
      AND listing_metadata #>> '{billing,buyer_markup_percent}'='15'
      AND listing_metadata #>> '{billing,list_micro_usd}' IN ('1500','47258')
      AND NOT (listing_metadata ?| ARRAY['auth_headers','auth_query'])
    FOR UPDATE;
  PERFORM 1 FROM providers.api_endpoints WHERE api_id=source_id FOR UPDATE;
  IF (SELECT count(*) FROM providers.api_endpoints WHERE api_id=source_id)<>4
     OR (SELECT count(*) FROM providers.api_endpoints WHERE api_id=source_id
       AND method='POST' AND path IN ('/linkedin/profile','/linkedin/company','/linkedin/job','/linkedin/post')
       AND active AND price IN (0.0015,0.047258) AND payment_required AND contract_documented
       AND request_body->'required'='["url"]'::jsonb
       AND request_body->'additionalProperties'='false'::jsonb
       AND (SELECT count(*) FROM jsonb_object_keys(request_body->'properties'))=1
       AND request_body #>> '{properties,url,type}'='string'
       AND request_body #>> '{properties,url,maxLength}'='2048')<>4 THEN
    RAISE EXCEPTION 'Bright Data endpoint tariff or request contract drift';
  END IF;

  FOR example IN SELECT value FROM jsonb_array_elements(examples) LOOP
    UPDATE providers.api_endpoints SET price=0.047258,
      request_example=example->'inputs',
      request_body=jsonb_set(request_body,'{properties,url,example}',example #> '{inputs,url}'),
      use_cases=CASE WHEN (example->>'question')=ANY(coalesce(use_cases,ARRAY[]::text[]))
        THEN use_cases ELSE array_append(coalesce(use_cases,ARRAY[]::text[]),example->>'question') END,
      documentation_markdown=replace(documentation_markdown,
        'One URL is priced at USD 0.0015 before the standard Apiosk buyer fee.',price_text)
        || CASE WHEN position('Example question: '||(example->>'question') IN documentation_markdown)>0
          THEN '' ELSE E'\n\nExample question: '||(example->>'question')||E'\nLinkedIn URL: '||(example #>> '{inputs,url}') END
      WHERE api_id=source_id AND method='POST' AND path=example->>'path';
  END LOOP;
  UPDATE providers.apis SET
    listing_metadata=jsonb_set(listing_metadata,'{billing}',
      coalesce(listing_metadata->'billing','{}'::jsonb)||jsonb_build_object(
        'list_micro_usd',47258,'buyer_total_micro_usd',54347,'buyer_markup_percent',15,
        'target_display_currency','EUR','target_total_eur',0.05,
        'conversion_rate',0.92,'conversion_date','2026-08-21',
        'conversion_basis','EUR per USD; display only, ledger remains USD',
        'scope','standard standalone call'))
      || jsonb_build_object('logo',logo,'example_questions',examples)
    WHERE id=source_id;

  IF (SELECT count(*) FROM providers.api_endpoints WHERE api_id=source_id
       AND active AND price=0.047258
       AND price*1000000+ceil(price*1000000*1500/10000)=54347
       AND position(price_text IN documentation_markdown)>0
       AND documentation_markdown NOT LIKE '%One URL is priced at USD 0.0015%')<>4
     OR NOT EXISTS (SELECT 1 FROM providers.apis WHERE id=source_id AND status='active'
       AND active AND is_verified AND discoverable
       AND listing_metadata->>'logo'=logo
       AND jsonb_array_length(listing_metadata->'example_questions')=4
       AND listing_metadata #>> '{billing,buyer_total_micro_usd}'='54347') THEN
    RAISE EXCEPTION 'Bright Data five-cent tariff or example publication invariant failed';
  END IF;
END $price_and_examples$;
COMMIT;
