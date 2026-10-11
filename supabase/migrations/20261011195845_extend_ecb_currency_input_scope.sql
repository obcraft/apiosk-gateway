-- Reviewed metadata only: the fixed ECB EUR route accepts a literal target
-- currency. Yelp business-search aliases keep company and place separate.
-- Preserve all validation, output aliases, prices and route contracts.
-- Exact schema guards stop this update if the catalog changed after review.
DO $migration$
DECLARE
  item jsonb;
  actual jsonb;
  endpoint_id text;
  annotation jsonb;
  schema_state text;
BEGIN
  IF (SELECT count(*) FROM providers.api_endpoints e JOIN providers.apis a ON a.id=e.api_id
      WHERE a.slug='frankfurter' AND e.method='GET' AND e.path='/v2/providers/ecb/rate/EUR/{quote}' AND e.active) <> 1
  THEN RAISE EXCEPTION 'Reviewed ECB endpoint identity drift'; END IF;
  SELECT e.id::text INTO STRICT endpoint_id FROM providers.api_endpoints e JOIN providers.apis a ON a.id=e.api_id
    WHERE a.slug='frankfurter' AND e.method='GET' AND e.path='/v2/providers/ecb/rate/EUR/{quote}' AND e.active;
  FOR item IN SELECT value FROM jsonb_array_elements($reviewed$
[
  {
    "slug": "fx.ecb.quote",
    "before": {
      "enum": [
        "AUD",
        "BRL",
        "CAD",
        "CHF",
        "CNY",
        "CZK",
        "DKK",
        "EUR",
        "GBP",
        "HKD",
        "HUF",
        "IDR",
        "ILS",
        "INR",
        "ISK",
        "JPY",
        "KRW",
        "MXN",
        "MYR",
        "NOK",
        "NZD",
        "PHP",
        "PLN",
        "RON",
        "SEK",
        "SGD",
        "THB",
        "TRY",
        "USD",
        "ZAR"
      ],
      "type": "string",
      "title": "Target currency",
      "description": "Target currency from the verified ECB daily euro reference currency set."
    },
    "after": {
      "enum": [
        "AUD",
        "BRL",
        "CAD",
        "CHF",
        "CNY",
        "CZK",
        "DKK",
        "EUR",
        "GBP",
        "HKD",
        "HUF",
        "IDR",
        "ILS",
        "INR",
        "ISK",
        "JPY",
        "KRW",
        "MXN",
        "MYR",
        "NOK",
        "NZD",
        "PHP",
        "PLN",
        "RON",
        "SEK",
        "SGD",
        "THB",
        "TRY",
        "USD",
        "ZAR"
      ],
      "type": "string",
      "title": "Target currency",
      "description": "Target currency from the verified ECB daily euro reference currency set.",
      "$comment": "{\"apiosk_input_aliases_v1\":[\"quote_currency\",\"target_currency\"]}"
    }
  },
  {
    "slug": "fx.ecb.reference_rate",
    "before": {
      "type": "object",
      "$comment": "{\"apiosk_fixed_scope_v1\":[{\"path\":\"/v2/providers/ecb/rate/EUR/{quote}\",\"inputs\":{\"rate_type\":[\"official ECB reference exchange rate\",\"ECB reference exchange rate\",\"official ECB reference rate\",\"ECB reference rate\"],\"observation_date\":[\"latest\",\"most recent\",\"laatste\",\"meest recente\"]},\"method\":\"GET\",\"input_facts\":[\"fx.ecb.quote\"],\"source_slug\":\"frankfurter\"}]}",
      "required": [
        "date",
        "base",
        "quote",
        "rate"
      ],
      "properties": {
        "base": {
          "enum": [
            "EUR"
          ],
          "type": "string"
        },
        "date": {
          "type": "string",
          "pattern": "^[0-9]{4}-[0-9]{2}-[0-9]{2}$",
          "$comment": "{\"apiosk_output_aliases_v1\":[\"source_date\",\"observation_date\"]}"
        },
        "rate": {
          "type": "number",
          "minimum": 1e-09,
          "$comment": "{\"apiosk_output_aliases_v1\":[\"exchange_rate\"]}"
        },
        "quote": {
          "enum": [
            "AUD",
            "BRL",
            "CAD",
            "CHF",
            "CNY",
            "CZK",
            "DKK",
            "EUR",
            "GBP",
            "HKD",
            "HUF",
            "IDR",
            "ILS",
            "INR",
            "ISK",
            "JPY",
            "KRW",
            "MXN",
            "MYR",
            "NOK",
            "NZD",
            "PHP",
            "PLN",
            "RON",
            "SEK",
            "SGD",
            "THB",
            "TRY",
            "USD",
            "ZAR"
          ],
          "type": "string",
          "description": "Target currency from the verified ECB daily euro reference currency set."
        }
      }
    },
    "after": {
      "type": "object",
      "$comment": "{\"apiosk_fixed_scope_v1\":[{\"path\":\"/v2/providers/ecb/rate/EUR/{quote}\",\"inputs\":{\"rate_type\":[\"official ECB reference exchange rate\",\"ECB reference exchange rate\",\"official ECB reference rate\",\"ECB reference rate\",\"official ECB reference\",\"ECB reference\"],\"observation_date\":[\"latest\",\"most recent\",\"laatste\",\"meest recente\"],\"base_currency\":[\"EUR\"],\"reference_source\":[\"ECB\"]},\"method\":\"GET\",\"input_facts\":[\"fx.ecb.quote\"],\"source_slug\":\"frankfurter\"}]}",
      "required": [
        "date",
        "base",
        "quote",
        "rate"
      ],
      "properties": {
        "base": {
          "enum": [
            "EUR"
          ],
          "type": "string"
        },
        "date": {
          "type": "string",
          "pattern": "^[0-9]{4}-[0-9]{2}-[0-9]{2}$",
          "$comment": "{\"apiosk_output_aliases_v1\":[\"source_date\",\"observation_date\"]}"
        },
        "rate": {
          "type": "number",
          "minimum": 1e-09,
          "$comment": "{\"apiosk_output_aliases_v1\":[\"exchange_rate\"]}"
        },
        "quote": {
          "enum": [
            "AUD",
            "BRL",
            "CAD",
            "CHF",
            "CNY",
            "CZK",
            "DKK",
            "EUR",
            "GBP",
            "HKD",
            "HUF",
            "IDR",
            "ILS",
            "INR",
            "ISK",
            "JPY",
            "KRW",
            "MXN",
            "MYR",
            "NOK",
            "NZD",
            "PHP",
            "PLN",
            "RON",
            "SEK",
            "SGD",
            "THB",
            "TRY",
            "USD",
            "ZAR"
          ],
          "type": "string",
          "description": "Target currency from the verified ECB daily euro reference currency set."
        }
      }
    }
  },
  {
    "slug": "source_input.eeddb418e721e589b9ea21083dee98afd.location",
    "before": {
      "type": "string",
      "description": "Search location."
    },
    "after": {
      "type": "string",
      "description": "Search location.",
      "$comment": "{\"apiosk_input_aliases_v1\":[\"city\",\"location\"]}"
    }
  },
  {
    "slug": "source_input.eeddb418e721e589b9ea21083dee98afd.query",
    "before": {
      "type": "string",
      "description": "Search query / keyword"
    },
    "after": {
      "type": "string",
      "description": "Search query / keyword",
      "$comment": "{\"apiosk_input_aliases_v1\":[\"company_name\",\"business_name\"]}"
    }
  }
]
$reviewed$::jsonb)
  LOOP
    IF item->>'slug' = 'fx.ecb.reference_rate' THEN
      FOREACH schema_state IN ARRAY ARRAY['before','after'] LOOP
        annotation := jsonb_set((item->schema_state->>'$comment')::jsonb,
          '{apiosk_fixed_scope_v1,0,endpoint_id}',to_jsonb(endpoint_id),true);
        item := jsonb_set(item,ARRAY[schema_state,'$comment'],to_jsonb(annotation::text));
      END LOOP;
    END IF;
    SELECT value_schema INTO STRICT actual FROM catalog.fact_types
      WHERE slug = item->>'slug' FOR UPDATE;
    IF actual = item->'after' THEN
      CONTINUE;
    END IF;
    IF actual IS DISTINCT FROM item->'before' THEN
      RAISE EXCEPTION 'Reviewed vocabulary changed since review: %', item->>'slug';
    END IF;
    UPDATE catalog.fact_types SET value_schema = item->'after'
      WHERE slug = item->>'slug';
  END LOOP;
END;
$migration$;

