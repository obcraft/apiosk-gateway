-- Stage EODHD as a complete financial-data source from the supplied OpenAPI
-- 3.1 catalogue (version 2026-09-07).
--
-- SECURITY / COMMERCIAL GATE:
-- * No API key is stored in this migration. Provision `api_token` later via
--   listing_metadata.auth_query using the server-side secret workflow.
-- * The supplied catalogue marks all 60 operations x-apiosk-sellable=false
--   because EODHD's public terms prohibit resale/redistribution. Therefore
--   this migration keeps the source hidden, unverified and inactive, keeps
--   every endpoint inactive, and creates no v2 execution contracts.
-- * Activation requires a separate reviewed migration after a signed EODHD
--   agreement, credential provisioning and live endpoint validation.
--
-- Prices in api_endpoints are the pre-15%-markup base amounts. This makes the
-- Gateway's standard buyer markup reproduce the supplied target buyer prices
-- (subject to normal micro-unit rounding).

begin;

do $migration$
declare
  source_id constant uuid := 'b268fe55-41d2-5b7a-b147-31af1f1c1d0f';
  platform_owner constant uuid := 'bf085fe8-41c4-4699-978f-e75e89518bf7';
  public_metadata constant jsonb := $eodhd_metadata${"logo":"https://eodhd.com/img/eod_logo.svg","favicon":"https://eodhd.com/financial-apis/wp-content/uploads/2022/06/favicon.png","billing":{"mode":"apiosk_balance","buyer_markup_percent":15},"base_url":"https://eodhd.com","credential":{"location":"query","name":"api_token","configured":false},"provider":{"name":"EODHD","legal_name":"Unicorn Data Services","legal_registration":"835 149 998 R.C.S. Lyon, Greffe du tribunal de Commerce de Lyon","address":"Le Forum, 27 rue Maurice Flandin, 69003 Lyon, France","country":"FR","website":"https://eodhd.com","docs":"https://eodhd.com/financial-apis/","logo_svg":"https://eodhd.com/img/eod_logo.svg","logo_svg_mark":"https://eodhd.com/img/eod_logo.svg#logo","logo_svg_wordmark":"https://eodhd.com/img/eod_logo.svg#label","favicon_ico":"https://eodhd.com/favicon.ico","favicon_png":"https://eodhd.com/financial-apis/wp-content/uploads/2022/06/favicon.png","og_image_png":"https://eodhd.com/img/main.png","support_email":"support@eodhistoricaldata.com","sales_email":"sales@eodhistoricaldata.com","terms":"https://eodhd.com/financial-apis/terms-conditions","commercial_pricing":"https://eodhd.com/commercial-pricing"},"resale_status":{"status":"blocked","summary":"EODHD's standard Terms and Conditions prohibit reselling, retransmitting, redistributing and granting access to the Services, including in repackaged form, and separately prohibit sharing account access. A marketplace that proxies individual calls on one API key is not permitted under any publicly purchasable plan.","quote":"Selling, reselling, retransmitting, redistributing, displaying, or granting access to the Information or Services, whether in its original or repackaged form","quote_source":"https://eodhd.com/financial-apis/terms-conditions","internal_use_plan_quote":"the data is restricted to being used solely within your company. Displaying the data or sharing it with individuals outside your company is not permissible under this package.","internal_use_plan_source":"https://eodhd.com/commercial-pricing","exchange_reporting_quote":"we are required to report all commercial users of exchange data to the relevant exchanges","required_before_launch":"A signed Custom plan Data Services Agreement with an explicit sublicensing and per-call redistribution clause, negotiated with sales@eodhistoricaldata.com. Publicly listed Custom pricing starts at USD 399 per month."},"pricing_model":{"basis":"Free plan plus purchased extra API call packages","observed_package":"EUR 25 for 500,000 extra API calls, EODHD dashboard, September 2026","cost_per_call_unit_eur":5.0e-05,"price_floor_eur":0.02,"bundle_markup":25,"value_factor_by_uniqueness":{"1":0.5,"2":0.75,"3":1.0,"4":1.5,"5":2.5},"formula":"retail = max(price_floor * value_factor(uniqueness), call_weight * cost_per_call_unit * bundle_markup)","caveat":"Extra API calls raise the volume ceiling only. They unlock no data the plan does not already include, so the free plan reaches only the operations marked x-apiosk-free-plan: true, and then at 20 weighted calls per day."},"plan_labels":{"free":"Free (20 weighted calls per day, one year of history, personal use)","eod":"EOD Historical Data, USD 19.99 per month","eod_intraday":"EOD + Intraday, USD 29.99 per month","fundamentals":"Fundamentals Data Feed, USD 59.99 per month","all_in_one":"ALL-IN-ONE, USD 99.99 per month","marketplace":"Separate marketplace subscription on top of a plan","extended_fundamentals":"Extended Fundamentals, on request"},"tags":["eodhd","stocks","market data","end of day prices","intraday","fundamentals","options","bonds","news","sentiment","macroeconomics","esg"],"sectors":["Finance","Investment","Capital markets"],"service_groups":[{"name":"Market prices","description":"EODHD Market prices endpoints.","endpoint_paths":["/api/eod/{symbol}","/api/real-time/{symbol}","/api/intraday/{symbol}","/api/eod-bulk-last-day/{exchange}"]},{"name":"Reference data","description":"EODHD Reference data endpoints.","endpoint_paths":["/api/splits/{symbol}","/api/div/{symbol}","/api/historical-market-cap/{symbol}","/api/exchanges-list/","/api/exchange-symbol-list/{exchange}","/api/exchange-details/{exchange}","/api/search/{query}"]},{"name":"Fundamentals","description":"EODHD Fundamentals endpoints.","endpoint_paths":["/api/v1.1/fundamentals/{symbol}","/api/fundamentals/{symbol}","/api/bulk-fundamentals/{EXCHANGE_CODE}","/api/v2/bulk-fundamentals/{EXCHANGE_CODE}","/api/mp/investverte/esg/{symbol}"]},{"name":"Corporate Events","description":"EODHD Corporate Events endpoints.","endpoint_paths":["/api/sec-filings/{symbol}/form4","/api/insider-transactions"]},{"name":"Calendar","description":"EODHD Calendar endpoints.","endpoint_paths":["/api/calendar/earnings","/api/calendar/trends","/api/calendar/ipos","/api/calendar/splits","/api/calendar/dividends"]},{"name":"Options","description":"EODHD Options endpoints.","endpoint_paths":["/api/mp/unicornbay/options/contracts","/api/mp/unicornbay/options/eod","/api/mp/unicornbay/options/underlying-symbols"]},{"name":"Fixed Income","description":"EODHD Fixed Income endpoints.","endpoint_paths":["/api/bond-fundamentals/{identifier}"]},{"name":"Screening","description":"EODHD Screening endpoints.","endpoint_paths":["/api/screener"]},{"name":"Technicals","description":"EODHD Technicals endpoints.","endpoint_paths":["/api/technical/{symbol}"]},{"name":"News","description":"EODHD News endpoints.","endpoint_paths":["/api/news","/api/news-word-weights"]},{"name":"Sentiment","description":"EODHD Sentiment endpoints.","endpoint_paths":["/api/sentiments"]},{"name":"Macro","description":"EODHD Macro endpoints.","endpoint_paths":["/api/macro-indicator/{country}","/api/economic-events"]},{"name":"Marketplace ESG","description":"EODHD Marketplace ESG endpoints.","endpoint_paths":["/api/mp/investverte/companies","/api/mp/investverte/countries","/api/mp/investverte/country/{symbol}","/api/mp/investverte/sectors","/api/mp/investverte/sector/{symbol}"]},{"name":"Marketplace Alternative Data","description":"EODHD Marketplace Alternative Data endpoints.","endpoint_paths":["/api/mp/unicornbay/tickdata/ticks","/api/logo/{symbol}","/api/logo-svg/{symbol}"]},{"name":"Marketplace Index Data","description":"EODHD Marketplace Index Data endpoints.","endpoint_paths":["/api/mp/unicornbay/spglobal/list","/api/mp/unicornbay/spglobal/comp/{symbol}"]},{"name":"Marketplace Market Status","description":"EODHD Marketplace Market Status endpoints.","endpoint_paths":["/api/mp/tradinghours/markets","/api/mp/tradinghours/markets/lookup","/api/mp/tradinghours/markets/details","/api/mp/tradinghours/markets/status"]},{"name":"Marketplace Analytics","description":"EODHD Marketplace Analytics endpoints.","endpoint_paths":["/api/mp/praams/analyse/equity/ticker/{ticker}","/api/mp/praams/analyse/equity/isin/{isin}","/api/mp/praams/analyse/bond/{isin}","/api/mp/praams/bank/income_statement/ticker/{ticker}","/api/mp/praams/bank/income_statement/isin/{isin}","/api/mp/praams/bank/balance_sheet/ticker/{ticker}","/api/mp/praams/bank/balance_sheet/isin/{isin}","/api/mp/praams/explore/equity","/api/mp/praams/explore/bond","/api/mp/praams/reports/equity/ticker/{ticker}","/api/mp/praams/reports/equity/isin/{isin}","/api/mp/praams/reports/bond/{isin}"]}],"catalog_state":"staged","activation_requirements":["Signed EODHD Data Services Agreement explicitly allowing sublicensing and per-call redistribution","Server-side api_token provisioned outside SQL","Entitlement validation for every endpoint that will be activated","Live response and pricing validation before v2 execution contracts are created"]}$eodhd_metadata$::jsonb;
begin
  if not exists (select 1 from public.provider_profiles where id = platform_owner) then
    raise exception 'Apiosk platform owner % is missing', platform_owner;
  end if;

  if exists (select 1 from public.apis where slug = 'eodhd' and id <> source_id) then
    raise exception 'The slug eodhd already belongs to another source id';
  end if;

  insert into public.apis (
    id, owner_id, slug, name, status, description, long_description,
    category, docs_url, website_url, origin_url, endpoint_url,
    authentication_info, is_verified, active, discoverable, listing_type,
    listing_metadata, integration_mode, settlement_mode, is_external,
    is_platform_owned, is_community, source, wallet_address, payout_network,
    requires_payout_wallet
  ) values (
    source_id,
    platform_owner,
    'eodhd',
    'EODHD Financial APIs',
    'inactive',
    'Global market prices, fundamentals, corporate events, options, fixed income, news, sentiment and macro data.',
    'EODHD by Unicorn Data Services provides 60 documented financial-data operations. This listing is staged and unavailable for purchase until Apiosk has explicit redistribution rights, a server-side credential and validated endpoint entitlements.',
    'finance',
    'https://eodhd.com/financial-apis/',
    'https://eodhd.com',
    'https://eodhd.com',
    'https://eodhd.com',
    jsonb_build_object(
      'type', 'apiKey',
      'caller', 'Callers never provide an EODHD token. Apiosk injects it server-side after the source is licensed and activated.',
      'upstream', jsonb_build_object(
        'query_parameter', 'api_token',
        'note', 'The private EODHD token must be provisioned outside SQL and must never be returned to callers.'
      )
    ),
    false,
    false,
    false,
    'api',
    public_metadata,
    'gateway',
    'provider_payout',
    false,
    true,
    true,
    'manual',
    '0x0000000000000000000000000000000000000000',
    'base',
    false
  )
  on conflict (id) do update set
    owner_id = excluded.owner_id,
    slug = excluded.slug,
    name = excluded.name,
    status = 'inactive',
    description = excluded.description,
    long_description = excluded.long_description,
    category = excluded.category,
    docs_url = excluded.docs_url,
    website_url = excluded.website_url,
    origin_url = excluded.origin_url,
    endpoint_url = excluded.endpoint_url,
    authentication_info = excluded.authentication_info,
    is_verified = false,
    active = false,
    discoverable = false,
    listing_type = excluded.listing_type,
    listing_metadata = excluded.listing_metadata ||
      case
        when public.apis.listing_metadata ? 'auth_query'
          then jsonb_build_object('auth_query', public.apis.listing_metadata->'auth_query')
        else '{}'::jsonb
      end,
    integration_mode = excluded.integration_mode,
    settlement_mode = excluded.settlement_mode,
    is_external = excluded.is_external,
    is_platform_owned = excluded.is_platform_owned,
    is_community = excluded.is_community,
    source = excluded.source,
    wallet_address = excluded.wallet_address,
    payout_network = excluded.payout_network,
    requires_payout_wallet = excluded.requires_payout_wallet;
end;
$migration$;

with endpoint_rows as (
  select value as row
  from jsonb_array_elements($eodhd_endpoints$[
  {
    "id": "9d7715dc-bf36-57d0-b4dc-bca366012a11",
    "method": "GET",
    "path": "/api/eod/{symbol}",
    "price": 0.017391,
    "description": "Daily, weekly or monthly open, high, low, close, adjusted close and volume for one instrument.",
    "query_params": {
      "type": "object",
      "properties": {
        "symbol": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "Ticker with exchange suffix in TICKER.EXCHANGE form."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The documentation states the server-side default is csv, so always send fmt=json for machine parsing."
        },
        "period": {
          "type": "string",
          "enum": [
            "d",
            "w",
            "m"
          ],
          "default": "d",
          "required": false,
          "x-parameter-location": "query",
          "description": "Bar aggregation period: d for daily, w for weekly, m for monthly."
        },
        "order": {
          "type": "string",
          "enum": [
            "a",
            "d"
          ],
          "default": "a",
          "required": false,
          "x-parameter-location": "query",
          "description": "Sort order of the returned series by date: a for ascending, d for descending."
        },
        "from": {
          "type": "string",
          "format": "date",
          "required": false,
          "x-parameter-location": "query",
          "description": "Start date of the requested range, inclusive, in YYYY-MM-DD format."
        },
        "to": {
          "type": "string",
          "format": "date",
          "required": false,
          "x-parameter-location": "query",
          "description": "End date of the requested range, inclusive, in YYYY-MM-DD format."
        },
        "filter": {
          "type": "string",
          "enum": [
            "last_date",
            "last_open",
            "last_high",
            "last_low",
            "last_close",
            "last_volume"
          ],
          "required": false,
          "x-parameter-location": "query",
          "description": "Return a single bare value from the most recent bar instead of the full series."
        }
      },
      "required": [
        "symbol"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.eod_historical",
      "x-apiosk-call-weight": 1,
      "x-apiosk-provider-cost-eur": 5.0e-05,
      "x-apiosk-target-buyer-price-eur": 0.02,
      "x-apiosk-plans": [
        "eod",
        "eod_intraday",
        "all_in_one",
        "free"
      ],
      "x-apiosk-free-plan": true,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "type": "array",
      "description": "Chronologically ordered list of price bars.",
      "items": {
        "type": "object",
        "properties": {
          "date": {
            "type": "string",
            "format": "date",
            "description": "Trading date of the bar in YYYY-MM-DD format."
          },
          "open": {
            "type": "number",
            "description": "Opening price of the period, unadjusted."
          },
          "high": {
            "type": "number",
            "description": "Highest price of the period, unadjusted."
          },
          "low": {
            "type": "number",
            "description": "Lowest price of the period, unadjusted."
          },
          "close": {
            "type": "number",
            "description": "Closing price of the period, unadjusted."
          },
          "adjusted_close": {
            "type": "number",
            "description": "Closing price adjusted for splits and dividends."
          },
          "volume": {
            "type": "integer",
            "description": "Traded volume over the period, adjusted for splits."
          }
        },
        "required": [
          "date",
          "open",
          "high",
          "low",
          "close",
          "adjusted_close",
          "volume"
        ]
      }
    },
    "request_example": {
      "symbol": "AAPL.US",
      "fmt": "json",
      "period": "d",
      "order": "a",
      "from": "2024-01-01",
      "to": "2024-12-31",
      "filter": "last_close"
    },
    "response_example": "[{\"date\":\"2024-01-02\",\"open\":187.15,\"high\":188.44,\"low\":183.89,\"close\":185.64,\"adjusted_close\":183.404,\"volume\":82488700},{\"date\":\"2024-01-03\",\"open\":184.22,\"high\":185.88,\"low\":183.43,\"close\":184.25,\"adjusted_close\":182.0308,\"volume\":58414500}]",
    "tags": [
      "market prices",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Daily, weekly or monthly open, high, low, close, adjusted close and volume for one instrument."
    ],
    "input_entities": [
      "symbol",
      "fmt",
      "period",
      "order",
      "from",
      "to",
      "filter"
    ],
    "output_entities": [
      "market prices"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "Daily, weekly or monthly open, high, low, close, adjusted close and volume for one instrument.",
    "external_docs_url": "https://eodhd.com/financial-apis/api-for-historical-data-and-volumes",
    "documentation_markdown": "GET /api/eod/{symbol} — Daily, weekly or monthly open, high, low, close, adjusted close and volume for one instrument.\n\nReturns the full end-of-day price history for a single ticker in TICKER.EXCHANGE form, covering stocks, ETFs, funds, indices, forex and cryptocurrencies. Over 30 years of history is available for many instruments. The open, high, low and close values are raw and unadjusted; only adjusted_close carries both split and dividend adjustments, while volume is adjusted for splits. Weekly and monthly periods aggregate the volume over the bar.\n\nEODHD call weight: 1; required plans: eod, eod_intraday, all_in_one, free.\n\nOne API call per request no matter how long the price history is, so ask for the widest range you need in a single call. There is no pagination. The free plan can call it but only for the past year of history and with 20 API calls per day. Using filter changes the response body from an array to a single scalar value, which breaks the array schema above. The default fmt on the server is csv, so fmt=json must be sent explicitly.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "eod_historical"
  },
  {
    "id": "14b7fc1e-76df-5d9c-ac70-baefbbbc1ddc",
    "method": "GET",
    "path": "/api/real-time/{symbol}",
    "price": 0.017391,
    "description": "Latest delayed quote with session open, high, low, close, volume and change for one or more tickers.",
    "query_params": {
      "type": "object",
      "properties": {
        "symbol": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "Primary ticker with exchange suffix in TICKER.EXCHANGE form."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The documentation states the server-side default is csv, so always send fmt=json for machine parsing."
        },
        "s": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Comma separated list of additional tickers to return alongside the path ticker. The documentation supports roughly 15 to 20 tickers per request. Presence of this parameter switches the response body to an array."
        },
        "filter": {
          "type": "string",
          "enum": [
            "code",
            "timestamp",
            "gmtoffset",
            "open",
            "high",
            "low",
            "close",
            "volume",
            "previousClose",
            "change",
            "change_p"
          ],
          "required": false,
          "x-parameter-location": "query",
          "description": "Return only one field of the quote as a bare value instead of the full object."
        }
      },
      "required": [
        "symbol"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.live_delayed_prices",
      "x-apiosk-call-weight": 1,
      "x-apiosk-provider-cost-eur": 5.0e-05,
      "x-apiosk-target-buyer-price-eur": 0.02,
      "x-apiosk-plans": [
        "all_in_one",
        "eod",
        "eod_intraday",
        "free"
      ],
      "x-apiosk-free-plan": true,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "type": "object",
      "description": "Delayed quote snapshot. When the s parameter is used the body is instead an array of these objects.",
      "properties": {
        "code": {
          "type": "string",
          "description": "Ticker with exchange suffix."
        },
        "timestamp": {
          "type": "integer",
          "description": "Unix time of the quote in seconds since 1970-01-01 UTC."
        },
        "gmtoffset": {
          "type": "integer",
          "description": "Offset from UTC in seconds."
        },
        "open": {
          "type": "number",
          "description": "Session-to-date opening price."
        },
        "high": {
          "type": "number",
          "description": "Session-to-date highest price."
        },
        "low": {
          "type": "number",
          "description": "Session-to-date lowest price."
        },
        "close": {
          "type": "number",
          "description": "Latest delayed price."
        },
        "volume": {
          "type": "integer",
          "description": "Session total traded volume."
        },
        "previousClose": {
          "type": "number",
          "description": "Close of the previous trading session."
        },
        "change": {
          "type": "number",
          "description": "Absolute change against the previous close."
        },
        "change_p": {
          "type": "number",
          "description": "Percentage change against the previous close."
        }
      },
      "required": [
        "code",
        "timestamp",
        "gmtoffset",
        "open",
        "high",
        "low",
        "close",
        "volume",
        "previousClose",
        "change",
        "change_p"
      ]
    },
    "request_example": {
      "symbol": "AAPL.US",
      "fmt": "json",
      "s": "VTI.US,EUR.FOREX",
      "filter": "close"
    },
    "response_example": "{\"code\":\"AAPL.US\",\"timestamp\":1788566400,\"gmtoffset\":0,\"open\":328.305,\"high\":328.93,\"low\":317.86,\"close\":319.97,\"volume\":39606884,\"previousClose\":328.21,\"change\":-8.24,\"change_p\":-2.5106}",
    "tags": [
      "market prices",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Latest delayed quote with session open, high, low, close, volume and change for one or more tickers."
    ],
    "input_entities": [
      "symbol",
      "fmt",
      "s",
      "filter"
    ],
    "output_entities": [
      "market prices"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "Latest delayed quote with session open, high, low, close, volume and change for one or more tickers.",
    "external_docs_url": "https://eodhd.com/financial-apis/live-realtime-stocks-api",
    "documentation_markdown": "GET /api/real-time/{symbol} — Latest delayed quote with session open, high, low, close, volume and change for one or more tickers.\n\nReturns the most recent delayed price snapshot for a ticker, and optionally for a batch of extra tickers through the s parameter. Stocks are delayed roughly 15 to 20 minutes and currencies by about one minute. The open, high and low values are session-to-date figures rather than per-minute bars, and close is the latest delayed price. A single ticker returns a JSON object; adding s returns a JSON array with one object per ticker.\n\nEODHD call weight: 1; required plans: all_in_one, eod, eod_intraday, free.\n\nThe response kind is conditional: an object for a single ticker, an array when s is present. Billing is 1 API call per ticker, so a batch of 10 tickers costs 10 calls even though it is one HTTP request. This is delayed data, not true real-time; for true real-time use the separate WebSocket product. Fields may return the string NA when a market has not traded. The documented plan list includes the Free plan. The server default fmt is csv.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "live_delayed_prices"
  },
  {
    "id": "e14efa7a-afca-5022-b125-df6febab8d0e",
    "method": "GET",
    "path": "/api/intraday/{symbol}",
    "price": 0.043478,
    "description": "Historical intraday OHLCV bars at 1 minute, 5 minute or 1 hour resolution for one instrument.",
    "query_params": {
      "type": "object",
      "properties": {
        "symbol": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "Ticker with exchange suffix in TICKER.EXCHANGE form."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The documentation states the server-side default is csv, so always send fmt=json for machine parsing."
        },
        "interval": {
          "type": "string",
          "enum": [
            "1m",
            "5m",
            "1h"
          ],
          "default": "5m",
          "required": false,
          "x-parameter-location": "query",
          "description": "Bar interval. 1m is one minute, 5m is five minutes, 1h is one hour."
        },
        "from": {
          "type": "integer",
          "required": false,
          "x-parameter-location": "query",
          "description": "Start of the requested window as a Unix timestamp in seconds, UTC. Note this is a Unix timestamp, not a YYYY-MM-DD date as on the EOD endpoint."
        },
        "to": {
          "type": "integer",
          "required": false,
          "x-parameter-location": "query",
          "description": "End of the requested window as a Unix timestamp in seconds, UTC."
        },
        "split-dt": {
          "type": "integer",
          "enum": [
            0,
            1
          ],
          "default": 0,
          "required": false,
          "x-parameter-location": "query",
          "description": "Set to 1 to return separate date and time fields instead of a single combined datetime field."
        }
      },
      "required": [
        "symbol"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.intraday_historical",
      "x-apiosk-call-weight": 5,
      "x-apiosk-provider-cost-eur": 0.00025,
      "x-apiosk-target-buyer-price-eur": 0.05,
      "x-apiosk-plans": [
        "eod_intraday",
        "all_in_one"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "type": "array",
      "description": "Chronologically ordered list of intraday bars.",
      "items": {
        "type": "object",
        "properties": {
          "timestamp": {
            "type": "integer",
            "description": "Bar start time as a Unix timestamp in seconds, UTC."
          },
          "gmtoffset": {
            "type": "integer",
            "description": "Offset from UTC in seconds."
          },
          "datetime": {
            "type": "string",
            "format": "date-time",
            "description": "Bar start time as a UTC string in YYYY-MM-DD HH:MM:SS format."
          },
          "open": {
            "type": "number",
            "description": "Opening price of the bar."
          },
          "high": {
            "type": "number",
            "description": "Highest price of the bar."
          },
          "low": {
            "type": "number",
            "description": "Lowest price of the bar."
          },
          "close": {
            "type": "number",
            "description": "Closing price of the bar."
          },
          "volume": {
            "type": "integer",
            "description": "Traded volume during the bar."
          }
        },
        "required": [
          "timestamp",
          "gmtoffset",
          "datetime",
          "open",
          "high",
          "low",
          "close",
          "volume"
        ]
      }
    },
    "request_example": {
      "symbol": "AAPL.US",
      "fmt": "json",
      "interval": "5m",
      "from": 1700000000,
      "to": 1700600000,
      "split-dt": 1
    },
    "response_example": "[{\"timestamp\":1778506200,\"gmtoffset\":0,\"datetime\":\"2026-05-11 13:30:00\",\"open\":291.979003,\"high\":292.700012,\"low\":290.5,\"close\":291.230102,\"volume\":2481823}]",
    "tags": [
      "market prices",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Historical intraday OHLCV bars at 1 minute, 5 minute or 1 hour resolution for one instrument."
    ],
    "input_entities": [
      "symbol",
      "fmt",
      "interval",
      "from",
      "to",
      "split-dt"
    ],
    "output_entities": [
      "market prices"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "Historical intraday OHLCV bars at 1 minute, 5 minute or 1 hour resolution for one instrument.",
    "external_docs_url": "https://eodhd.com/financial-apis/intraday-historical-data-api",
    "documentation_markdown": "GET /api/intraday/{symbol} — Historical intraday OHLCV bars at 1 minute, 5 minute or 1 hour resolution for one instrument.\n\nReturns historical intraday price bars for a single ticker with UTC timestamps. One minute US equity bars go back to 2004, while five minute and one hour bars start in October 2020; forex and crypto one minute bars go back to 2009. Each request is capped by interval: 120 days for 1m, 600 days for 5m and 7200 days for 1h, so long histories must be paged by moving the from and to window. Intraday data is delayed and finalised roughly two to three hours after the market close.\n\nEODHD call weight: 5; required plans: eod_intraday, all_in_one.\n\nCosts 5 API calls per request, five times the EOD endpoint. from and to are Unix timestamps here, which differs from every other endpoint in this group. Respect the per-interval maximum window of 120 / 600 / 7200 days and page the window yourself; there is no cursor. Bars are not adjusted for splits or dividends and there is no adjusted_close field. With split-dt=1 the datetime field is replaced by separate date and time fields; the docs do not print their exact key names, so verify before relying on them. The server default fmt is csv.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "intraday_historical"
  },
  {
    "id": "d2d9fa0d-be6a-5ff7-b6d5-96246562451b",
    "method": "GET",
    "path": "/api/eod-bulk-last-day/{exchange}",
    "price": 0.108696,
    "description": "One day of end-of-day prices, splits or dividends for every ticker on an exchange in a single request.",
    "query_params": {
      "type": "object",
      "properties": {
        "exchange": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "Exchange code such as US, XETRA or WAR. For the United States the venue codes NYSE, NASDAQ, BATS and AMEX are also accepted."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The documentation states the server-side default is csv, so always send fmt=json for machine parsing."
        },
        "type": {
          "type": "string",
          "enum": [
            "eod",
            "splits",
            "dividends"
          ],
          "default": "eod",
          "required": false,
          "x-parameter-location": "query",
          "description": "Which dataset to return for the day."
        },
        "date": {
          "type": "string",
          "format": "date",
          "required": false,
          "x-parameter-location": "query",
          "description": "Trading day to return in YYYY-MM-DD format. Defaults to the last trading day; a non-trading date falls back to the prior trading day."
        },
        "symbols": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Comma separated ticker codes to restrict the response to. Using it changes billing to 100 plus 1 call per ticker."
        },
        "filter": {
          "type": "string",
          "enum": [
            "extended"
          ],
          "required": false,
          "x-parameter-location": "query",
          "description": "Set to extended to add company name, instrument type, market capitalisation, beta and moving average or volume technical fields to the eod dataset."
        }
      },
      "required": [
        "exchange"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.bulk_eod_last_day",
      "x-apiosk-call-weight": 100,
      "x-apiosk-provider-cost-eur": 0.005,
      "x-apiosk-target-buyer-price-eur": 0.125,
      "x-apiosk-plans": [
        "all_in_one",
        "eod",
        "eod_intraday"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "type": "array",
      "description": "One record per ticker for the requested day. The property set depends on the type parameter.",
      "items": {
        "type": "object",
        "additionalProperties": true,
        "properties": {
          "code": {
            "type": "string",
            "description": "Ticker code without exchange suffix. Present in all three datasets."
          },
          "exchange_short_name": {
            "type": "string",
            "description": "Exchange designation. Present in the eod dataset."
          },
          "exchange": {
            "type": "string",
            "description": "Exchange designation. Present in the splits and dividends datasets."
          },
          "date": {
            "type": "string",
            "format": "date",
            "description": "Trading day for eod, event date for splits, ex-dividend date for dividends."
          },
          "open": {
            "type": "number",
            "description": "Opening price. eod dataset only."
          },
          "high": {
            "type": "number",
            "description": "Highest price of the session. eod dataset only."
          },
          "low": {
            "type": "number",
            "description": "Lowest price of the session. eod dataset only."
          },
          "close": {
            "type": "number",
            "description": "Closing price. eod dataset only."
          },
          "adjusted_close": {
            "type": "number",
            "description": "Close adjusted for splits and dividends. eod dataset only."
          },
          "volume": {
            "type": "integer",
            "description": "Shares traded during the session. eod dataset only."
          },
          "prev_close": {
            "type": "number",
            "description": "Previous session close. eod dataset, JSON output only."
          },
          "change": {
            "type": "number",
            "description": "Absolute change against the previous close. eod dataset, JSON output only."
          },
          "change_p": {
            "type": "number",
            "description": "Percentage change against the previous close. eod dataset, JSON output only."
          },
          "split": {
            "type": "string",
            "description": "Split ratio expressed as new shares/old shares. splits dataset only."
          },
          "dividend": {
            "type": "string",
            "description": "Dividend amount per share. dividends dataset only."
          },
          "currency": {
            "type": "string",
            "description": "Currency the dividend is denominated in. dividends dataset only."
          },
          "declarationDate": {
            "type": "string",
            "format": "date",
            "description": "Date the dividend was declared. dividends dataset only."
          },
          "recordDate": {
            "type": "string",
            "format": "date",
            "description": "Record date of the dividend. dividends dataset only."
          },
          "paymentDate": {
            "type": "string",
            "format": "date",
            "description": "Payment date of the dividend. dividends dataset only."
          },
          "period": {
            "type": "string",
            "description": "Dividend frequency such as Quarterly or Semiannual. dividends dataset only."
          },
          "unadjustedValue": {
            "type": "string",
            "description": "Dividend amount before adjustment. dividends dataset only."
          },
          "name": {
            "type": "string",
            "description": "Company name. Only when filter=extended."
          },
          "type": {
            "type": "string",
            "description": "Instrument type. Only when filter=extended."
          },
          "MarketCapitalization": {
            "type": "number",
            "description": "Market capitalisation. Only when filter=extended."
          },
          "Beta": {
            "type": "number",
            "description": "Beta coefficient. Only when filter=extended."
          },
          "ema_50d": {
            "type": "number",
            "description": "50 day exponential moving average. Only when filter=extended and only for the last 30 days."
          },
          "ema_200d": {
            "type": "number",
            "description": "200 day exponential moving average. Only when filter=extended and only for the last 30 days."
          },
          "hi_250d": {
            "type": "number",
            "description": "250 day high. Only when filter=extended and only for the last 30 days."
          },
          "lo_250d": {
            "type": "number",
            "description": "250 day low. Only when filter=extended and only for the last 30 days."
          },
          "avgvol_14d": {
            "type": "number",
            "description": "14 day average volume. Only when filter=extended and only for the last 30 days."
          },
          "avgvol_50d": {
            "type": "number",
            "description": "50 day average volume. Only when filter=extended and only for the last 30 days."
          },
          "avgvol_200d": {
            "type": "number",
            "description": "200 day average volume. Only when filter=extended and only for the last 30 days."
          }
        },
        "required": [
          "code",
          "date"
        ]
      }
    },
    "request_example": {
      "exchange": "US",
      "fmt": "json",
      "type": "eod",
      "date": "2026-08-18",
      "symbols": "MSFT,AAPL",
      "filter": "extended"
    },
    "response_example": "[{\"code\":\"MSFT\",\"exchange_short_name\":\"US\",\"date\":\"2026-08-18\",\"open\":481.54,\"high\":484.27,\"low\":477.15,\"close\":481.63,\"adjusted_close\":481.63,\"volume\":24059700,\"prev_close\":480.35,\"change\":1.28,\"change_p\":0.2665}]",
    "tags": [
      "market prices",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "One day of end-of-day prices, splits or dividends for every ticker on an exchange in a single request."
    ],
    "input_entities": [
      "exchange",
      "fmt",
      "type",
      "date",
      "symbols",
      "filter"
    ],
    "output_entities": [
      "market prices"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "One day of end-of-day prices, splits or dividends for every ticker on an exchange in a single request.",
    "external_docs_url": "https://eodhd.com/financial-apis/bulk-api-eod-splits-dividends",
    "documentation_markdown": "GET /api/eod-bulk-last-day/{exchange} — One day of end-of-day prices, splits or dividends for every ticker on an exchange in a single request.\n\nReturns a whole exchange snapshot for one trading day, which is the efficient way to refresh a large universe instead of looping per ticker. The type parameter selects prices, splits or dividends, and each dataset requires the matching per-ticker entitlement. If the requested date is not a trading day the API returns the prior trading day. Setting filter=extended adds company name, market capitalisation and technical fields, but those technical fields are computed for the last 30 days only and older dates return zeros.\n\nEODHD call weight: 100; required plans: all_in_one, eod, eod_intraday.\n\nThe path name says eod-bulk-last-day but the same path serves splits and dividends through type; there is no separate bulk splits or bulk dividends path. A whole exchange request is a flat 100 API calls regardless of how many rows come back, and adding symbols makes it 100 plus 1 per ticker, so never use symbols for a small list when the per-ticker EOD endpoint costs 1. The prev_close, change and change_p fields are documented as JSON only and are absent from CSV output. It returns one day only; there is no date range. The server default fmt is csv.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "bulk_eod_last_day"
  },
  {
    "id": "151c8570-594d-54fb-a790-41ce31bf7120",
    "method": "GET",
    "path": "/api/splits/{symbol}",
    "price": 0.017391,
    "description": "Full history of stock splits for one instrument with the split ratio.",
    "query_params": {
      "type": "object",
      "properties": {
        "symbol": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "Ticker with exchange suffix in TICKER.EXCHANGE form."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The documentation states the server-side default is csv, so always send fmt=json for machine parsing."
        },
        "from": {
          "type": "string",
          "format": "date",
          "required": false,
          "x-parameter-location": "query",
          "description": "Start date of the requested range, inclusive, in YYYY-MM-DD format."
        },
        "to": {
          "type": "string",
          "format": "date",
          "required": false,
          "x-parameter-location": "query",
          "description": "End date of the requested range, inclusive, in YYYY-MM-DD format."
        }
      },
      "required": [
        "symbol"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.historical_splits",
      "x-apiosk-call-weight": 1,
      "x-apiosk-provider-cost-eur": 5.0e-05,
      "x-apiosk-target-buyer-price-eur": 0.02,
      "x-apiosk-plans": [
        "all_in_one",
        "eod",
        "eod_intraday",
        "free"
      ],
      "x-apiosk-free-plan": true,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "type": "array",
      "description": "Chronologically ordered list of split events.",
      "items": {
        "type": "object",
        "properties": {
          "date": {
            "type": "string",
            "format": "date",
            "description": "Effective date of the split in YYYY-MM-DD format."
          },
          "split": {
            "type": "string",
            "description": "Split ratio as a string in the form new shares/old shares, for example 4.000000/1.000000 for a four for one split."
          }
        },
        "required": [
          "date",
          "split"
        ]
      }
    },
    "request_example": {
      "symbol": "AAPL.US",
      "fmt": "json",
      "from": "2000-01-01",
      "to": "2025-12-31"
    },
    "response_example": "[{\"date\":\"2014-06-09\",\"split\":\"7.000000/1.000000\"},{\"date\":\"2020-08-31\",\"split\":\"4.000000/1.000000\"}]",
    "tags": [
      "reference data",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Full history of stock splits for one instrument with the split ratio."
    ],
    "input_entities": [
      "symbol",
      "fmt",
      "from",
      "to"
    ],
    "output_entities": [
      "reference data"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "Full history of stock splits for one instrument with the split ratio.",
    "external_docs_url": "https://eodhd.com/financial-apis/api-splits-dividends",
    "documentation_markdown": "GET /api/splits/{symbol} — Full history of stock splits for one instrument with the split ratio.\n\nReturns every recorded share split for a single ticker as a chronological array, with the effective date and the ratio expressed as new shares over old shares. It is used to reconstruct unadjusted price series or to reconcile share counts across corporate actions. The free plan is limited to about one year of history while paid plans cover more than 30 years.\n\nEODHD call weight: 1; required plans: all_in_one, eod, eod_intraday, free.\n\nThe split value is a string containing a slash, not a number, so it must be parsed into numerator and denominator. The documentation page itself does not print a JSON example; the field names above were confirmed against the documented DEMO token for AAPL.US and match the splits shape shown on the bulk API documentation page. Returned in ascending date order. No pagination.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "historical_splits"
  },
  {
    "id": "acbd1484-a0db-5d1b-b0bc-68eafaec6fa6",
    "method": "GET",
    "path": "/api/div/{symbol}",
    "price": 0.017391,
    "description": "Full history of dividend payments for one instrument with ex-date, key dates and per share amounts.",
    "query_params": {
      "type": "object",
      "properties": {
        "symbol": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "Ticker with exchange suffix in TICKER.EXCHANGE form."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The documentation states the server-side default is csv, so always send fmt=json for machine parsing."
        },
        "from": {
          "type": "string",
          "format": "date",
          "required": false,
          "x-parameter-location": "query",
          "description": "Start date of the requested range, inclusive, in YYYY-MM-DD format."
        },
        "to": {
          "type": "string",
          "format": "date",
          "required": false,
          "x-parameter-location": "query",
          "description": "End date of the requested range, inclusive, in YYYY-MM-DD format."
        }
      },
      "required": [
        "symbol"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.historical_dividends",
      "x-apiosk-call-weight": 1,
      "x-apiosk-provider-cost-eur": 5.0e-05,
      "x-apiosk-target-buyer-price-eur": 0.02,
      "x-apiosk-plans": [
        "all_in_one",
        "eod",
        "eod_intraday",
        "free"
      ],
      "x-apiosk-free-plan": true,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "type": "array",
      "description": "Chronologically ordered list of dividend events.",
      "items": {
        "type": "object",
        "properties": {
          "date": {
            "type": "string",
            "format": "date",
            "description": "Ex-dividend date in YYYY-MM-DD format."
          },
          "declarationDate": {
            "type": "string",
            "format": "date",
            "description": "Date the dividend was declared. Documented for major US tickers in JSON output only."
          },
          "recordDate": {
            "type": "string",
            "format": "date",
            "description": "Record date of the dividend. Documented for major US tickers in JSON output only."
          },
          "paymentDate": {
            "type": "string",
            "format": "date",
            "description": "Date the dividend is paid. Documented for major US tickers in JSON output only."
          },
          "period": {
            "type": "string",
            "description": "Payment frequency such as Quarterly, Semiannual, Annual or Monthly."
          },
          "value": {
            "type": "number",
            "description": "Dividend per share adjusted for subsequent splits."
          },
          "unadjustedValue": {
            "type": "number",
            "description": "Dividend per share as actually paid, before split adjustment."
          },
          "currency": {
            "type": "string",
            "description": "Currency the dividend is denominated in."
          }
        },
        "required": [
          "date",
          "value"
        ]
      }
    },
    "request_example": {
      "symbol": "AAPL.US",
      "fmt": "json",
      "from": "2000-01-01",
      "to": "2025-12-31"
    },
    "response_example": "[{\"date\":\"2020-02-07\",\"declarationDate\":\"2020-01-28\",\"recordDate\":\"2020-02-10\",\"paymentDate\":\"2020-02-13\",\"period\":\"Quarterly\",\"value\":0.1925,\"unadjustedValue\":0.77,\"currency\":\"USD\"}]",
    "tags": [
      "reference data",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Full history of dividend payments for one instrument with ex-date, key dates and per share amounts."
    ],
    "input_entities": [
      "symbol",
      "fmt",
      "from",
      "to"
    ],
    "output_entities": [
      "reference data"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "Full history of dividend payments for one instrument with ex-date, key dates and per share amounts.",
    "external_docs_url": "https://eodhd.com/financial-apis/api-splits-dividends",
    "documentation_markdown": "GET /api/div/{symbol} — Full history of dividend payments for one instrument with ex-date, key dates and per share amounts.\n\nReturns every recorded dividend for a single ticker as a chronological array keyed on the ex-dividend date. For major NYSE and NASDAQ tickers the JSON output also carries the declaration, record and payment dates, the payment frequency, the unadjusted amount and the currency. The value field is adjusted for later splits while unadjustedValue is the amount actually paid at the time. The free plan is limited to about one year of history while paid plans cover more than 30 years.\n\nEODHD call weight: 1; required plans: all_in_one, eod, eod_intraday, free.\n\nThe path is /api/div/, not /api/dividends/. The documentation page describes the fields in prose but does not print a JSON example; the exact key names above were confirmed against the documented DEMO token for AAPL.US. Only date and value are guaranteed: outside major NYSE and NASDAQ tickers the documentation states the extra date fields, unadjustedValue and currency may be absent, and they are documented as JSON only. Note that value is split adjusted, so do not sum it against historical unadjusted prices. No pagination.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "historical_dividends"
  },
  {
    "id": "19455361-6c1c-551a-b742-ba872308549b",
    "method": "GET",
    "path": "/api/historical-market-cap/{symbol}",
    "price": 0.043478,
    "description": "Time series of historical market capitalisation in US dollars for one instrument.",
    "query_params": {
      "type": "object",
      "properties": {
        "symbol": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "Ticker with exchange suffix in TICKER.EXCHANGE form. Also accepts .CC crypto tickers."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The documentation states the server-side default is csv, so always send fmt=json for machine parsing."
        },
        "from": {
          "type": "string",
          "format": "date",
          "required": false,
          "x-parameter-location": "query",
          "description": "Start date of the requested range, inclusive, in YYYY-MM-DD format."
        },
        "to": {
          "type": "string",
          "format": "date",
          "required": false,
          "x-parameter-location": "query",
          "description": "End date of the requested range, inclusive, in YYYY-MM-DD format."
        }
      },
      "required": [
        "symbol"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.historical_market_cap",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.05,
      "x-apiosk-plans": [
        "all_in_one",
        "fundamentals"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "type": "object",
      "description": "Object whose keys are sequential numeric indices as strings, each mapping to one observation.",
      "additionalProperties": {
        "type": "object",
        "description": "One market capitalisation observation.",
        "properties": {
          "date": {
            "type": "string",
            "format": "date",
            "description": "Observation date in YYYY-MM-DD format."
          },
          "value": {
            "type": "integer",
            "description": "Market capitalisation on that date, in US dollars."
          }
        },
        "required": [
          "date",
          "value"
        ]
      }
    },
    "request_example": {
      "symbol": "AAPL.US",
      "fmt": "json",
      "from": "2024-01-01",
      "to": "2024-12-31"
    },
    "response_example": "{\"0\":{\"date\":\"2026-07-07\",\"value\":4556663954100},\"1\":{\"date\":\"2026-07-09\",\"value\":4638216299400},\"2\":{\"date\":\"2026-07-10\",\"value\":4625015380200}}",
    "tags": [
      "reference data",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Time series of historical market capitalisation in US dollars for one instrument."
    ],
    "input_entities": [
      "symbol",
      "fmt",
      "from",
      "to"
    ],
    "output_entities": [
      "reference data"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "Time series of historical market capitalisation in US dollars for one instrument.",
    "external_docs_url": "https://eodhd.com/financial-apis/historical-market-capitalization-api",
    "documentation_markdown": "GET /api/historical-market-cap/{symbol} — Time series of historical market capitalisation in US dollars for one instrument.\n\nReturns a dated series of market capitalisation values in US dollars for a single ticker, which is useful for size factor work, index weighting and valuation ratios over time. Coverage is all NYSE and NASDAQ listed companies sampled weekly from 2021-07-09, plus .CC cryptocurrency tickers sampled daily from late 2022 for most assets. The response is a JSON object keyed by sequential index rather than an array, and each entry holds a date and a value.\n\nEODHD call weight: 10; required plans: all_in_one, fundamentals.\n\nThe response is an object keyed by index string, not an array, so iterate the values rather than indexing positionally. Each request costs 10 API calls, the most expensive endpoint in this group. Equity data is weekly, not daily, and starts only at 2021-07-09, so it cannot be used for long horizon backtests. Values are always in US dollars regardless of the listing currency. Sits on the Fundamentals entitlement, so an EOD only plan cannot call it.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "historical_market_cap"
  },
  {
    "id": "f245d52e-ccc7-59f0-b434-dee1755d199f",
    "method": "GET",
    "path": "/api/exchanges-list/",
    "price": 0.013043,
    "description": "List of all exchanges supported by EODHD with codes, MICs, country and currency.",
    "query_params": {
      "type": "object",
      "properties": {
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The documentation states the server-side default is csv, so always send fmt=json for machine parsing."
        }
      },
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.exchanges_list",
      "x-apiosk-call-weight": 1,
      "x-apiosk-provider-cost-eur": 5.0e-05,
      "x-apiosk-target-buyer-price-eur": 0.015,
      "x-apiosk-plans": [
        "all_in_one",
        "eod",
        "eod_intraday",
        "fundamentals",
        "free"
      ],
      "x-apiosk-free-plan": true,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "type": "array",
      "description": "List of supported exchanges.",
      "items": {
        "type": "object",
        "properties": {
          "Name": {
            "type": "string",
            "description": "Full name of the exchange."
          },
          "Code": {
            "type": "string",
            "description": "Exchange code used across the EODHD APIs, both as a ticker suffix and as a path parameter."
          },
          "OperatingMIC": {
            "type": [
              "string",
              "null"
            ],
            "description": "Comma separated ISO 10383 operating MIC codes, or null when not applicable."
          },
          "Country": {
            "type": "string",
            "description": "Country of operation, or the literal Unknown."
          },
          "Currency": {
            "type": "string",
            "description": "Default trading currency of the exchange."
          },
          "CountryISO2": {
            "type": "string",
            "description": "ISO 3166-1 alpha-2 country code."
          },
          "CountryISO3": {
            "type": "string",
            "description": "ISO 3166-1 alpha-3 country code."
          }
        },
        "required": [
          "Name",
          "Code",
          "Country",
          "Currency"
        ]
      }
    },
    "request_example": {
      "fmt": "json"
    },
    "response_example": "[{\"Name\":\"USA Stocks\",\"Code\":\"US\",\"OperatingMIC\":\"XNAS, XNYS, OTCM, XCBO\",\"Country\":\"USA\",\"Currency\":\"USD\",\"CountryISO2\":\"US\",\"CountryISO3\":\"USA\"}]",
    "tags": [
      "reference data",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "List of all exchanges supported by EODHD with codes, MICs, country and currency."
    ],
    "input_entities": [
      "fmt"
    ],
    "output_entities": [
      "reference data"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "List of all exchanges supported by EODHD with codes, MICs, country and currency.",
    "external_docs_url": "https://eodhd.com/financial-apis/exchanges-api-list-of-tickers-and-trading-hours",
    "documentation_markdown": "GET /api/exchanges-list/ — List of all exchanges supported by EODHD with codes, MICs, country and currency.\n\nReturns every exchange EODHD supports, including the Code value that must be used as the exchange suffix on tickers and as the path parameter on other exchange endpoints. Each entry carries the full name, ISO 10383 operating MIC codes, country and default trading currency. The list also contains five virtual exchanges, CC for crypto, FOREX, MONEY, GBOND and EUFUND. This is the discovery entry point for building a valid ticker.\n\nEODHD call weight: 1; required plans: all_in_one, eod, eod_intraday, fundamentals, free.\n\nThis path keeps a trailing slash, unlike the other exchange endpoints. Field names use PascalCase here while the price endpoints use lowercase, so do not assume one casing convention across the API. The documentation states 70 exchanges as of August 2026 plus five virtual exchanges. Note that Code US is a single aggregated feed covering NYSE, NASDAQ and OTC rather than one venue. The DEMO token cannot call this endpoint and returns Forbidden.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "exchanges_list"
  },
  {
    "id": "fdbc0a15-4204-545b-8d9d-322eebd63171",
    "method": "GET",
    "path": "/api/exchange-symbol-list/{exchange}",
    "price": 0.017391,
    "description": "All tradable tickers on one exchange with name, type, currency and ISIN.",
    "query_params": {
      "type": "object",
      "properties": {
        "exchange": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "Exchange code as returned in the Code field of the exchanges list, for example US, LSE or WAR."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The documentation states the server-side default is csv, so always send fmt=json for machine parsing."
        },
        "delisted": {
          "type": "integer",
          "enum": [
            0,
            1
          ],
          "default": 0,
          "required": false,
          "x-parameter-location": "query",
          "description": "Set to 1 to return delisted tickers instead of the active universe."
        },
        "type": {
          "type": "string",
          "enum": [
            "common_stock",
            "preferred_stock",
            "stock",
            "etf",
            "fund"
          ],
          "required": false,
          "x-parameter-location": "query",
          "description": "Restrict the list to a single instrument class."
        },
        "symbols": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Comma separated ticker codes to check the existence and metadata of, instead of returning the whole exchange."
        }
      },
      "required": [
        "exchange"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.exchange_symbol_list",
      "x-apiosk-call-weight": 1,
      "x-apiosk-provider-cost-eur": 5.0e-05,
      "x-apiosk-target-buyer-price-eur": 0.02,
      "x-apiosk-plans": [
        "all_in_one",
        "eod",
        "eod_intraday",
        "fundamentals",
        "free"
      ],
      "x-apiosk-free-plan": true,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "type": "array",
      "description": "List of instruments on the exchange.",
      "items": {
        "type": "object",
        "properties": {
          "Code": {
            "type": "string",
            "description": "Ticker symbol without the exchange suffix."
          },
          "Name": {
            "type": "string",
            "description": "Company or instrument name."
          },
          "Country": {
            "type": "string",
            "description": "Country of listing."
          },
          "Exchange": {
            "type": "string",
            "description": "Specific venue code, which may be narrower than the requested exchange code."
          },
          "Currency": {
            "type": "string",
            "description": "Trading currency."
          },
          "Type": {
            "type": "string",
            "description": "Instrument type such as Common Stock, ETF or FUND."
          },
          "Isin": {
            "type": [
              "string",
              "null"
            ],
            "description": "ISIN identifier where available, otherwise null."
          }
        },
        "required": [
          "Code",
          "Name",
          "Country",
          "Exchange",
          "Currency",
          "Type"
        ]
      }
    },
    "request_example": {
      "exchange": "WAR",
      "fmt": "json",
      "delisted": 1,
      "type": "etf",
      "symbols": "AAPL,MSFT"
    },
    "response_example": "[{\"Code\":\"CDR\",\"Name\":\"CD PROJEKT SA\",\"Country\":\"Poland\",\"Exchange\":\"WAR\",\"Currency\":\"PLN\",\"Type\":\"Common Stock\",\"Isin\":\"PLOPTTC00011\"}]",
    "tags": [
      "reference data",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "All tradable tickers on one exchange with name, type, currency and ISIN."
    ],
    "input_entities": [
      "exchange",
      "fmt",
      "delisted",
      "type",
      "symbols"
    ],
    "output_entities": [
      "reference data"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "All tradable tickers on one exchange with name, type, currency and ISIN.",
    "external_docs_url": "https://eodhd.com/financial-apis/exchanges-api-list-of-tickers-and-trading-hours",
    "documentation_markdown": "GET /api/exchange-symbol-list/{exchange} — All tradable tickers on one exchange with name, type, currency and ISIN.\n\nReturns the full instrument universe of a single exchange, which is the way to enumerate every symbol before requesting prices. Each row gives the bare ticker code, the instrument name, the specific venue, the trading currency, the instrument type and the ISIN where available. The delisted parameter switches the response to historical delisted tickers, and type narrows the list to a single instrument class. The Code field is the bare symbol without the exchange suffix, so build TICKER.EXCHANGE yourself.\n\nEODHD call weight: 1; required plans: all_in_one, eod, eod_intraday, fundamentals, free.\n\nThis path has no trailing slash, unlike exchanges-list. Requesting US returns tens of thousands of rows in one uncompressed response with no pagination, so stream or write to disk rather than buffering. The Exchange field in each row can be a narrower venue such as NYSE or NASDAQ even when the request was for US. Only 1 API call per request even for a full exchange. The server default fmt is csv.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "exchange_symbol_list"
  },
  {
    "id": "7cee4cfc-a4c7-56ba-abdb-383ec1487e97",
    "method": "GET",
    "path": "/api/exchange-details/{exchange}",
    "price": 0.043478,
    "description": "Trading hours, working days, holiday calendar and ticker counts for one exchange.",
    "query_params": {
      "type": "object",
      "properties": {
        "exchange": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "Exchange code as returned in the Code field of the exchanges list."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The documentation states the server-side default is csv, so always send fmt=json for machine parsing."
        },
        "from": {
          "type": "string",
          "format": "date",
          "required": false,
          "x-parameter-location": "query",
          "description": "Start of the holiday calendar window in YYYY-MM-DD format. Defaults to six months before today."
        },
        "to": {
          "type": "string",
          "format": "date",
          "required": false,
          "x-parameter-location": "query",
          "description": "End of the holiday calendar window in YYYY-MM-DD format. Defaults to six months after today."
        }
      },
      "required": [
        "exchange"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.exchange_details",
      "x-apiosk-call-weight": 5,
      "x-apiosk-provider-cost-eur": 0.00025,
      "x-apiosk-target-buyer-price-eur": 0.05,
      "x-apiosk-plans": [
        "all_in_one",
        "eod_intraday"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "type": "object",
      "description": "Operating profile of one exchange, v1 response shape.",
      "properties": {
        "Name": {
          "type": "string",
          "description": "Full name of the exchange."
        },
        "Code": {
          "type": "string",
          "description": "Exchange code used across the EODHD APIs."
        },
        "OperatingMIC": {
          "type": [
            "string",
            "null"
          ],
          "description": "Comma separated ISO 10383 operating MIC codes."
        },
        "Country": {
          "type": "string",
          "description": "Country of operation."
        },
        "Currency": {
          "type": "string",
          "description": "Default trading currency."
        },
        "Timezone": {
          "type": "string",
          "description": "IANA timezone name of the exchange, for example America/New_York."
        },
        "isOpen": {
          "type": "boolean",
          "description": "Whether the exchange is currently in its regular trading session."
        },
        "TradingHours": {
          "type": "object",
          "description": "Regular session times and working days.",
          "properties": {
            "Open": {
              "type": "string",
              "description": "Regular session open in exchange local time, HH:MM:SS."
            },
            "Close": {
              "type": "string",
              "description": "Regular session close in exchange local time, HH:MM:SS."
            },
            "OpenUTC": {
              "type": "string",
              "description": "Regular session open in UTC, HH:MM:SS."
            },
            "CloseUTC": {
              "type": "string",
              "description": "Regular session close in UTC, HH:MM:SS."
            },
            "WorkingDays": {
              "type": "string",
              "description": "Comma separated list of trading weekdays, for example Mon,Tue,Wed,Thu,Fri."
            }
          },
          "required": [
            "Open",
            "Close",
            "WorkingDays"
          ]
        },
        "ActiveTickers": {
          "type": "integer",
          "description": "Number of currently active tickers on the exchange."
        },
        "UpdatedTickers": {
          "type": "integer",
          "description": "Number of tickers updated in the latest data refresh."
        },
        "ExchangeHolidays": {
          "type": "object",
          "description": "Holiday calendar keyed by sequential numeric index string in the v1 response.",
          "additionalProperties": {
            "type": "object",
            "description": "One holiday entry.",
            "properties": {
              "Holiday": {
                "type": "string",
                "description": "Name of the holiday."
              },
              "Date": {
                "type": "string",
                "format": "date",
                "description": "Date of the holiday in YYYY-MM-DD format."
              },
              "Type": {
                "type": "string",
                "description": "Holiday type, for example official."
              }
            },
            "required": [
              "Holiday",
              "Date",
              "Type"
            ]
          }
        }
      },
      "required": [
        "Name",
        "Code",
        "Country",
        "Currency",
        "Timezone",
        "TradingHours",
        "ExchangeHolidays"
      ]
    },
    "request_example": {
      "exchange": "US",
      "fmt": "json",
      "from": "2026-01-01",
      "to": "2026-12-31"
    },
    "response_example": "{\"Name\":\"USA Stocks\",\"Code\":\"US\",\"OperatingMIC\":\"XNAS, XNYS, OTCM, XCBO\",\"Country\":\"USA\",\"Currency\":\"USD\",\"Timezone\":\"America/New_York\",\"isOpen\":true,\"TradingHours\":{\"Open\":\"09:30:00\",\"Close\":\"16:00:00\",\"OpenUTC\":\"13:30:00\",\"CloseUTC\":\"20:00:00\",\"WorkingDays\":\"Mon,Tue,Wed,Thu,Fri\"},\"ActiveTickers\":52334,\"UpdatedTickers\":0,\"ExchangeHolidays\":{\"0\":{\"Holiday\":\"New Year's Day\",\"Date\":\"2026-01-01\",\"Type\":\"official\"}}}",
    "tags": [
      "reference data",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Trading hours, working days, holiday calendar and ticker counts for one exchange."
    ],
    "input_entities": [
      "exchange",
      "fmt",
      "from",
      "to"
    ],
    "output_entities": [
      "reference data"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "Trading hours, working days, holiday calendar and ticker counts for one exchange.",
    "external_docs_url": "https://eodhd.com/financial-apis/exchanges-api-trading-hours-and-stock-market-holidays",
    "documentation_markdown": "GET /api/exchange-details/{exchange} — Trading hours, working days, holiday calendar and ticker counts for one exchange.\n\nReturns the operating profile of a single exchange: its timezone, regular session open and close in both local and UTC time, the working days of the week, whether the market is currently open, and a holiday calendar over the requested window. The from and to parameters default to six months back and six months ahead. Use it to decide whether a market is trading before requesting live prices, and to skip non-trading days when paging historical data.\n\nEODHD call weight: 5; required plans: all_in_one, eod_intraday.\n\nThis is the v1 legacy path and it remains supported, but a v2 exists at /api/v2/exchange-details/{exchange} which wraps the payload in a data object, keys ExchangeHolidays by date instead of by index, drops the Date field from each holiday entry, adds PreMarketOpen, PreMarketClose, AfterHoursOpen and AfterHoursClose to TradingHours, and adds an EarlyClose type with an EarlyClose time. Calling /api/v2/exchange-details without a code lists the available exchanges. Costs 5 API calls per request, and unlike the other two exchange endpoints it is not in the Free plan and not in the EOD All World plan. ExchangeHolidays is an object keyed by index string, not an array.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "exchange_details"
  },
  {
    "id": "2e1b1131-89a5-530e-a855-881e84f8312d",
    "method": "GET",
    "path": "/api/search/{query}",
    "price": 0.017391,
    "description": "Full text lookup of tickers, company names and ISINs returning matching instruments.",
    "query_params": {
      "type": "object",
      "properties": {
        "query": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "The search input. Can be a ticker symbol, a company name or an ISIN. Must be URL encoded."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The documentation states the server-side default is csv, so always send fmt=json for machine parsing."
        },
        "limit": {
          "type": "integer",
          "default": 15,
          "required": false,
          "x-parameter-location": "query",
          "description": "Maximum number of results to return. Maximum value is 500."
        },
        "exchange": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Restrict results to a single exchange code, for example US, NYSE or NASDAQ."
        },
        "type": {
          "type": "string",
          "enum": [
            "all",
            "stock",
            "etf",
            "fund",
            "bond",
            "index",
            "crypto"
          ],
          "required": false,
          "x-parameter-location": "query",
          "description": "Restrict results to a single instrument class."
        },
        "bonds_only": {
          "type": "integer",
          "enum": [
            0,
            1
          ],
          "default": 0,
          "required": false,
          "x-parameter-location": "query",
          "description": "Set to 1 to return only bonds in the results."
        }
      },
      "required": [
        "query"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.search",
      "x-apiosk-call-weight": 1,
      "x-apiosk-provider-cost-eur": 5.0e-05,
      "x-apiosk-target-buyer-price-eur": 0.02,
      "x-apiosk-plans": [
        "all_in_one",
        "eod",
        "eod_intraday",
        "fundamentals",
        "free"
      ],
      "x-apiosk-free-plan": true,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "type": "array",
      "description": "List of matching instruments, best match first.",
      "items": {
        "type": "object",
        "properties": {
          "Code": {
            "type": "string",
            "description": "Ticker symbol without the exchange suffix."
          },
          "Exchange": {
            "type": "string",
            "description": "Exchange code to append to Code to form the callable TICKER.EXCHANGE symbol."
          },
          "Name": {
            "type": "string",
            "description": "Company or instrument name."
          },
          "Type": {
            "type": "string",
            "description": "Instrument type such as Common Stock, ETF or FUND."
          },
          "Country": {
            "type": "string",
            "description": "Country of listing."
          },
          "Currency": {
            "type": "string",
            "description": "Trading currency."
          },
          "ISIN": {
            "type": [
              "string",
              "null"
            ],
            "description": "ISIN identifier where available, otherwise null."
          },
          "previousClose": {
            "type": "number",
            "description": "Closing price of the previous trading session."
          },
          "previousCloseDate": {
            "type": "string",
            "format": "date",
            "description": "Date of the previous close in YYYY-MM-DD format."
          },
          "isPrimary": {
            "type": "boolean",
            "description": "Whether this listing is the primary listing of the instrument."
          }
        },
        "required": [
          "Code",
          "Exchange",
          "Name",
          "Type",
          "Country",
          "Currency"
        ]
      }
    },
    "request_example": {
      "query": "apple",
      "fmt": "json",
      "limit": 50,
      "exchange": "US",
      "type": "stock",
      "bonds_only": 1
    },
    "response_example": "[{\"Code\":\"AAPL\",\"Exchange\":\"US\",\"Name\":\"Apple Inc\",\"Type\":\"Common Stock\",\"Country\":\"USA\",\"Currency\":\"USD\",\"ISIN\":\"US0378331005\",\"previousClose\":229.65,\"previousCloseDate\":\"2025-08-12\",\"isPrimary\":true}]",
    "tags": [
      "reference data",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Full text lookup of tickers, company names and ISINs returning matching instruments."
    ],
    "input_entities": [
      "query",
      "fmt",
      "limit",
      "exchange",
      "type",
      "bonds_only"
    ],
    "output_entities": [
      "reference data"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "Full text lookup of tickers, company names and ISINs returning matching instruments.",
    "external_docs_url": "https://eodhd.com/financial-apis/search-api-for-stocks-etfs-mutual-funds-and-indices",
    "documentation_markdown": "GET /api/search/{query} — Full text lookup of tickers, company names and ISINs returning matching instruments.\n\nResolves a free text query, a ticker fragment, a company name or an ISIN into a list of matching instruments across all supported exchanges. Each hit gives the bare ticker code, the exchange code, the instrument type, country, currency, ISIN and the previous close with its date. This is the practical way for an agent to turn a human name such as apple into a callable TICKER.EXCHANGE symbol. Results default to 15 and can be raised to 500.\n\nEODHD call weight: 1; required plans: all_in_one, eod, eod_intraday, fundamentals, free.\n\nThe query is a path segment, not a query string parameter, so it must be URL encoded; spaces and slashes in company names will otherwise break the route. The ISIN key is upper case here while the exchange symbol list uses Isin for the same concept. Use isPrimary to pick the primary listing when the same company matches on several exchanges. There is no offset or cursor, only limit, so a broad query cannot be paged beyond 500 rows. The DEMO token cannot call this endpoint and returns Forbidden.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "search"
  },
  {
    "id": "74ec4e64-7314-5d5f-be76-80c677ce8851",
    "method": "GET",
    "path": "/api/v1.1/fundamentals/{symbol}",
    "price": 0.043478,
    "description": "Full fundamentals object for a common stock or equity ticker.",
    "query_params": {
      "type": "object",
      "properties": {
        "symbol": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "Ticker in TICKER.EXCHANGE format, for example AAPL.US. Also accepts index tickers such as GSPC.INDX and mutual fund ISINs."
        },
        "filter": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Return only one section or a single field instead of the full object. Nest layers with :: and pass several comma-separated filters at once. Section and field names are the response keys, for example General, General::Code, Financials::Balance_Sheet::quarterly, or General::Code,General,Earnings."
        },
        "historical": {
          "type": "integer",
          "required": false,
          "x-parameter-location": "query",
          "description": "Set to 1 to include point-in-time index membership snapshots (HistoricalComponents). Indices only; combine with from and to."
        },
        "from": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Start date (YYYY-MM-DD) for historical index snapshots, used with historical=1."
        },
        "to": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "End date (YYYY-MM-DD) for historical index snapshots, used with historical=1."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The fundamentals feed is JSON only because of the complex data structure, so json is the only useful value."
        }
      },
      "required": [
        "symbol"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.fundamentals_stock",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.05,
      "x-apiosk-plans": [
        "all_in_one",
        "fundamentals_data_feed"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "Full fundamentals object for a common stock or equity ticker. Sections are omitted when the issuer does not report them.",
      "properties": {
        "General": {
          "type": "object",
          "description": "Company profile block.",
          "properties": {
            "Code": {
              "type": [
                "string",
                "null"
              ],
              "description": "The ticker code of the company."
            },
            "Type": {
              "type": [
                "string",
                "null"
              ],
              "description": "The instrument type, for example Common Stock."
            },
            "Name": {
              "type": [
                "string",
                "null"
              ],
              "description": "The company name."
            },
            "Exchange": {
              "type": [
                "string",
                "null"
              ],
              "description": "The exchange the ticker is listed on."
            },
            "CurrencyCode": {
              "type": [
                "string",
                "null"
              ],
              "description": "Currency code of the listing."
            },
            "CurrencyName": {
              "type": [
                "string",
                "null"
              ],
              "description": "Full name of the listing currency."
            },
            "CurrencySymbol": {
              "type": [
                "string",
                "null"
              ],
              "description": "Symbol of the listing currency."
            },
            "CountryName": {
              "type": [
                "string",
                "null"
              ],
              "description": "Country where the company is registered."
            },
            "CountryISO": {
              "type": [
                "string",
                "null"
              ],
              "description": "ISO country code."
            },
            "OpenFigi": {
              "type": [
                "string",
                "null"
              ],
              "description": "The OpenFIGI identifier."
            },
            "ISIN": {
              "type": [
                "string",
                "null"
              ],
              "description": "International Securities Identification Number."
            },
            "LEI": {
              "type": [
                "string",
                "null"
              ],
              "description": "Legal Entity Identifier."
            },
            "PrimaryTicker": {
              "type": [
                "string",
                "null"
              ],
              "description": "The primary ticker for the company."
            },
            "CUSIP": {
              "type": [
                "string",
                "null"
              ],
              "description": "CUSIP identifier, mainly for US companies."
            },
            "CIK": {
              "type": [
                "string",
                "null"
              ],
              "description": "Central Index Key assigned by the SEC, US companies."
            },
            "EmployerIdNumber": {
              "type": [
                "string",
                "null"
              ],
              "description": "Employer Identification Number (EIN), US companies."
            },
            "FiscalYearEnd": {
              "type": [
                "string",
                "null"
              ],
              "description": "Month the fiscal year ends."
            },
            "IPODate": {
              "type": [
                "string",
                "null"
              ],
              "format": "date",
              "description": "Date of the initial public offering."
            },
            "InternationalDomestic": {
              "type": [
                "string",
                "null"
              ],
              "description": "International or domestic classification."
            },
            "Sector": {
              "type": [
                "string",
                "null"
              ],
              "description": "The sector of the company."
            },
            "Industry": {
              "type": [
                "string",
                "null"
              ],
              "description": "The industry of the company."
            },
            "GicSector": {
              "type": [
                "string",
                "null"
              ],
              "description": "GICS sector classification."
            },
            "GicGroup": {
              "type": [
                "string",
                "null"
              ],
              "description": "GICS industry group classification."
            },
            "GicIndustry": {
              "type": [
                "string",
                "null"
              ],
              "description": "GICS industry classification."
            },
            "GicSubIndustry": {
              "type": [
                "string",
                "null"
              ],
              "description": "GICS sub-industry classification."
            },
            "HomeCategory": {
              "type": [
                "string",
                "null"
              ],
              "description": "Home category of the ticker, for example Domestic or ADR."
            },
            "IsDelisted": {
              "type": [
                "boolean",
                "null"
              ],
              "description": "Whether the ticker has been delisted."
            },
            "Description": {
              "type": [
                "string",
                "null"
              ],
              "description": "A textual description of the company."
            },
            "Address": {
              "type": [
                "string",
                "null"
              ],
              "description": "The company address as a single string."
            },
            "AddressData": {
              "type": "object",
              "additionalProperties": true,
              "description": "The address split into Street, City, State, Country and ZIP."
            },
            "Listings": {
              "type": "object",
              "additionalProperties": true,
              "description": "Other exchange listings for the ticker, keyed by index, each entry with Code, Exchange and Name."
            },
            "Officers": {
              "type": "object",
              "additionalProperties": true,
              "description": "Company executives, keyed by index, each entry with Name, Title and YearBorn."
            },
            "Phone": {
              "type": [
                "string",
                "null"
              ],
              "description": "Company phone number."
            },
            "WebURL": {
              "type": [
                "string",
                "null"
              ],
              "description": "Company website URL."
            },
            "LogoURL": {
              "type": [
                "string",
                "null"
              ],
              "description": "Path to the company logo image."
            },
            "FullTimeEmployees": {
              "type": [
                "integer",
                "null"
              ],
              "description": "Number of full-time employees."
            },
            "UpdatedAt": {
              "type": [
                "string",
                "null"
              ],
              "format": "date",
              "description": "Date the record was last updated."
            },
            "ExchangeMarket": {
              "type": [
                "string",
                "null"
              ],
              "description": "Extra market label that may appear on non-US equities only."
            }
          },
          "additionalProperties": true
        },
        "Highlights": {
          "type": "object",
          "description": "Key metrics and analyst estimates.",
          "properties": {
            "MarketCapitalization": {
              "type": [
                "number",
                "null"
              ],
              "description": "Market capitalization."
            },
            "MarketCapitalizationMln": {
              "type": [
                "number",
                "null"
              ],
              "description": "Market capitalization in millions."
            },
            "EBITDA": {
              "type": [
                "number",
                "null"
              ],
              "description": "Earnings before interest, taxes, depreciation and amortization."
            },
            "PERatio": {
              "type": [
                "number",
                "null"
              ],
              "description": "Price-to-earnings ratio."
            },
            "PEGRatio": {
              "type": [
                "number",
                "null"
              ],
              "description": "Price/earnings-to-growth ratio."
            },
            "WallStreetTargetPrice": {
              "type": [
                "number",
                "null"
              ],
              "description": "Average analyst target price."
            },
            "BookValue": {
              "type": [
                "number",
                "null"
              ],
              "description": "Book value per share."
            },
            "DividendShare": {
              "type": [
                "number",
                "null"
              ],
              "description": "Dividend per share."
            },
            "DividendYield": {
              "type": [
                "number",
                "null"
              ],
              "description": "Dividend yield."
            },
            "EarningsShare": {
              "type": [
                "number",
                "null"
              ],
              "description": "Earnings per share (EPS)."
            },
            "EPSEstimateCurrentYear": {
              "type": [
                "number",
                "null"
              ],
              "description": "Estimated EPS for the current year."
            },
            "EPSEstimateNextYear": {
              "type": [
                "number",
                "null"
              ],
              "description": "Estimated EPS for the next year."
            },
            "EPSEstimateNextQuarter": {
              "type": [
                "number",
                "null"
              ],
              "description": "Estimated EPS for the next quarter."
            },
            "EPSEstimateCurrentQuarter": {
              "type": [
                "number",
                "null"
              ],
              "description": "Estimated EPS for the current quarter."
            },
            "MostRecentQuarter": {
              "type": [
                "string",
                "null"
              ],
              "format": "date",
              "description": "Date of the most recent reported quarter."
            },
            "ProfitMargin": {
              "type": [
                "number",
                "null"
              ],
              "description": "Profit margin."
            },
            "OperatingMarginTTM": {
              "type": [
                "number",
                "null"
              ],
              "description": "Operating margin over the trailing twelve months."
            },
            "ReturnOnAssetsTTM": {
              "type": [
                "number",
                "null"
              ],
              "description": "Return on assets over the trailing twelve months."
            },
            "ReturnOnEquityTTM": {
              "type": [
                "number",
                "null"
              ],
              "description": "Return on equity over the trailing twelve months."
            },
            "RevenueTTM": {
              "type": [
                "number",
                "null"
              ],
              "description": "Revenue over the trailing twelve months."
            },
            "RevenuePerShareTTM": {
              "type": [
                "number",
                "null"
              ],
              "description": "Revenue per share over the trailing twelve months."
            },
            "QuarterlyRevenueGrowthYOY": {
              "type": [
                "number",
                "null"
              ],
              "description": "Quarterly revenue growth year over year."
            },
            "GrossProfitTTM": {
              "type": [
                "number",
                "null"
              ],
              "description": "Gross profit over the trailing twelve months."
            },
            "DilutedEpsTTM": {
              "type": [
                "number",
                "null"
              ],
              "description": "Diluted EPS over the trailing twelve months."
            },
            "QuarterlyEarningsGrowthYOY": {
              "type": [
                "number",
                "null"
              ],
              "description": "Quarterly earnings growth year over year."
            }
          },
          "additionalProperties": true
        },
        "Valuation": {
          "type": "object",
          "description": "Market valuation ratios.",
          "properties": {
            "TrailingPE": {
              "type": [
                "number",
                "null"
              ],
              "description": "Trailing price-to-earnings ratio."
            },
            "ForwardPE": {
              "type": [
                "number",
                "null"
              ],
              "description": "Forward price-to-earnings ratio."
            },
            "PriceSalesTTM": {
              "type": [
                "number",
                "null"
              ],
              "description": "Price-to-sales ratio over the trailing twelve months."
            },
            "PriceBookMRQ": {
              "type": [
                "number",
                "null"
              ],
              "description": "Price-to-book ratio for the most recent quarter."
            },
            "EnterpriseValue": {
              "type": [
                "number",
                "null"
              ],
              "description": "Enterprise value."
            },
            "EnterpriseValueRevenue": {
              "type": [
                "number",
                "null"
              ],
              "description": "Enterprise value to revenue ratio."
            },
            "EnterpriseValueEbitda": {
              "type": [
                "number",
                "null"
              ],
              "description": "Enterprise value to EBITDA ratio."
            }
          },
          "additionalProperties": true
        },
        "SharesStats": {
          "type": "object",
          "description": "Share statistics and ownership percentages.",
          "properties": {
            "SharesOutstanding": {
              "type": [
                "number",
                "null"
              ],
              "description": "Total shares outstanding."
            },
            "SharesFloat": {
              "type": [
                "number",
                "null"
              ],
              "description": "Shares available for public trading."
            },
            "PercentInsiders": {
              "type": [
                "number",
                "null"
              ],
              "description": "Percentage of shares held by insiders."
            },
            "PercentInstitutions": {
              "type": [
                "number",
                "null"
              ],
              "description": "Percentage of shares held by institutions."
            },
            "SharesShort": {
              "type": [
                "number",
                "null"
              ],
              "description": "Number of shares sold short."
            },
            "SharesShortPriorMonth": {
              "type": [
                "number",
                "null"
              ],
              "description": "Shares sold short in the prior month."
            },
            "ShortRatio": {
              "type": [
                "number",
                "null"
              ],
              "description": "Short interest ratio."
            },
            "ShortPercentOutstanding": {
              "type": [
                "number",
                "null"
              ],
              "description": "Short interest as a percentage of shares outstanding."
            },
            "ShortPercentFloat": {
              "type": [
                "number",
                "null"
              ],
              "description": "Short interest as a percentage of the float."
            }
          },
          "additionalProperties": true
        },
        "Technicals": {
          "type": "object",
          "description": "Technical performance metrics.",
          "properties": {
            "Beta": {
              "type": [
                "number",
                "null"
              ],
              "description": "Measure of stock volatility relative to the market."
            },
            "52WeekHigh": {
              "type": [
                "number",
                "null"
              ],
              "description": "Highest price over the last 52 weeks."
            },
            "52WeekLow": {
              "type": [
                "number",
                "null"
              ],
              "description": "Lowest price over the last 52 weeks."
            },
            "50DayMA": {
              "type": [
                "number",
                "null"
              ],
              "description": "50-day moving average."
            },
            "200DayMA": {
              "type": [
                "number",
                "null"
              ],
              "description": "200-day moving average."
            },
            "SharesShort": {
              "type": [
                "number",
                "null"
              ],
              "description": "Number of shares sold short."
            },
            "SharesShortPriorMonth": {
              "type": [
                "number",
                "null"
              ],
              "description": "Shares sold short in the prior month."
            },
            "ShortRatio": {
              "type": [
                "number",
                "null"
              ],
              "description": "Short interest ratio."
            },
            "ShortPercent": {
              "type": [
                "number",
                "null"
              ],
              "description": "Short interest as a percentage."
            }
          },
          "additionalProperties": true
        },
        "SplitsDividends": {
          "type": "object",
          "description": "Dividend and split summary.",
          "properties": {
            "ForwardAnnualDividendRate": {
              "type": [
                "number",
                "null"
              ],
              "description": "Forward annual dividend rate."
            },
            "ForwardAnnualDividendYield": {
              "type": [
                "number",
                "null"
              ],
              "description": "Forward annual dividend yield."
            },
            "PayoutRatio": {
              "type": [
                "number",
                "null"
              ],
              "description": "Dividend payout ratio."
            },
            "DividendDate": {
              "type": [
                "string",
                "null"
              ],
              "format": "date",
              "description": "The dividend payment date."
            },
            "ExDividendDate": {
              "type": [
                "string",
                "null"
              ],
              "format": "date",
              "description": "The ex-dividend date."
            },
            "LastSplitFactor": {
              "type": [
                "string",
                "null"
              ],
              "description": "The factor of the last stock split, for example 4:1."
            },
            "LastSplitDate": {
              "type": [
                "string",
                "null"
              ],
              "format": "date",
              "description": "The date of the last stock split."
            },
            "NumberDividendsByYear": {
              "type": "object",
              "additionalProperties": true,
              "description": "Count of dividends paid per year, keyed by index, each entry with Year and Count."
            }
          },
          "additionalProperties": true
        },
        "Holders": {
          "type": "object",
          "description": "Institutional and fund ownership.",
          "properties": {
            "Institutions": {
              "type": "object",
              "additionalProperties": true,
              "description": "Institutional holders keyed by index string (\"0\", \"1\", ...). Each entry has name, date (report date), totalShares (percent of total shares held), totalAssets (percent of the holder assets in this position), currentShares (share count), change and change_p."
            },
            "Funds": {
              "type": "object",
              "additionalProperties": true,
              "description": "Fund holders keyed by index string. Same entry shape as Institutions: name, date, totalShares, totalAssets, currentShares, change, change_p."
            }
          },
          "additionalProperties": true
        },
        "InsiderTransactions": {
          "type": "object",
          "additionalProperties": true,
          "description": "Insider trading records keyed by index string. Each entry has date (report date), ownerCik, ownerName, transactionDate, transactionCode, transactionAmount (share count), transactionPrice, transactionAcquiredDisposed, postTransactionAmount and secLink (URL of the SEC filing)."
        },
        "outstandingShares": {
          "type": "object",
          "description": "Historical shares outstanding.",
          "properties": {
            "annual": {
              "type": "object",
              "additionalProperties": true,
              "description": "Yearly history keyed by index string. Each entry has date (period label), dateFormatted (period end date, YYYY-MM-DD), sharesMln (shares in millions, delivered as a string) and shares (integer share count)."
            },
            "quarterly": {
              "type": "object",
              "additionalProperties": true,
              "description": "Quarterly history keyed by index string. Same entry shape as annual: date, dateFormatted, sharesMln, shares."
            }
          },
          "additionalProperties": true
        },
        "Earnings": {
          "type": "object",
          "description": "Reported earnings, analyst trend and annual earnings.",
          "properties": {
            "History": {
              "type": "object",
              "additionalProperties": true,
              "description": "Reported earnings keyed by fiscal period end date (YYYY-MM-DD). Each entry has reportDate, date, beforeAfterMarket, currency, epsActual, epsEstimate, epsDifference and surprisePercent."
            },
            "Trend": {
              "type": "object",
              "additionalProperties": true,
              "description": "Analyst estimates by period. In v1.1 this block is split into Quarterly and Annual sections instead of one flat date-keyed list, and each quarterly entry carries a human-readable quarter field (Q1, Q2, Q3, Q4). Entries include growth, earningsEstimateAvg, earningsEstimateLow, earningsEstimateHigh, revenueEstimateAvg, the EPS trend values and the EPS revision counts."
            },
            "Annual": {
              "type": "object",
              "additionalProperties": true,
              "description": "Annual earnings keyed by date. Each entry has date and epsActual."
            }
          },
          "additionalProperties": true
        },
        "Financials": {
          "type": "object",
          "description": "Full financial statements.",
          "properties": {
            "Balance_Sheet": {
              "type": "object",
              "description": "Balance sheet.",
              "properties": {
                "currency_symbol": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "Reporting currency of the statement."
                },
                "yearly": {
                  "type": "object",
                  "additionalProperties": true,
                  "description": "Yearly reports keyed by period end date (YYYY-MM-DD). Each report carries its own date and filing_date plus the line items, for example totalAssets, totalLiab, totalStockholderEquity, cash, netDebt, longTermDebt, inventory and retainedEarnings. Numeric line items are delivered as strings."
                },
                "quarterly": {
                  "type": "object",
                  "additionalProperties": true,
                  "description": "Quarterly reports keyed by period end date (YYYY-MM-DD). Same entry shape as yearly."
                }
              },
              "additionalProperties": true
            },
            "Income_Statement": {
              "type": "object",
              "description": "Income statement.",
              "properties": {
                "currency_symbol": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "Reporting currency of the statement."
                },
                "yearly": {
                  "type": "object",
                  "additionalProperties": true,
                  "description": "Yearly reports keyed by period end date (YYYY-MM-DD). Each report carries its own date and filing_date plus the line items, for example totalRevenue, costOfRevenue, grossProfit, operatingIncome, ebit, ebitda, netIncome and researchDevelopment. Numeric line items are delivered as strings."
                },
                "quarterly": {
                  "type": "object",
                  "additionalProperties": true,
                  "description": "Quarterly reports keyed by period end date (YYYY-MM-DD). Same entry shape as yearly."
                }
              },
              "additionalProperties": true
            },
            "Cash_Flow": {
              "type": "object",
              "description": "Cash flow statement.",
              "properties": {
                "currency_symbol": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "Reporting currency of the statement."
                },
                "yearly": {
                  "type": "object",
                  "additionalProperties": true,
                  "description": "Yearly reports keyed by period end date (YYYY-MM-DD). Each report carries its own date and filing_date plus the line items, for example totalCashFromOperatingActivities, capitalExpenditures, freeCashFlow, dividendsPaid, netIncome and changeInCash. Numeric line items are delivered as strings."
                },
                "quarterly": {
                  "type": "object",
                  "additionalProperties": true,
                  "description": "Quarterly reports keyed by period end date (YYYY-MM-DD). Same entry shape as yearly."
                }
              },
              "additionalProperties": true
            }
          },
          "additionalProperties": true
        }
      },
      "additionalProperties": true
    },
    "request_example": {
      "symbol": "AAPL.US",
      "filter": "Financials::Balance_Sheet::quarterly",
      "historical": 1,
      "from": "2010-01-01",
      "to": "2010-12-31",
      "fmt": "json"
    },
    "response_example": "{\n \"General\": {\n  \"Code\": \"AAPL\",\n  \"Type\": \"Common Stock\",\n  \"Name\": \"Apple Inc.\",\n  \"Exchange\": \"NASDAQ\",\n  \"CurrencyCode\": \"USD\",\n  \"CountryName\": \"USA\",\n  \"CountryISO\": \"US\",\n  \"ISIN\": \"US0378331005\",\n  \"CUSIP\": \"037833100\",\n  \"CIK\": \"0000320193\",\n  \"PrimaryTicker\": \"AAPL.US\",\n  \"FiscalYearEnd\": \"September\",\n  \"IPODate\": \"1980-12-12\",\n  \"Sector\": \"Technology\",\n  \"Industry\": \"Consumer Electronics\",\n  \"GicSector\": \"Information Technology\",\n  \"HomeCategory\": \"Domestic\",\n  \"IsDelisted\": false,\n  \"FullTimeEmployees\": 166000,\n  \"UpdatedAt\": \"2026-07-13\"\n },\n \"Highlights\": {\n  \"MarketCapitalization\": 4631217307648,\n  \"EBITDA\": 144427003904,\n  \"PERatio\": 33.15,\n  \"PEGRatio\": 2.09,\n  \"WallStreetTargetPrice\": 315.5667,\n  \"EarningsShare\": 6.6,\n  \"DividendYield\": 0.0034,\n  \"ProfitMargin\": 0.2731,\n  \"RevenueTTM\": 408625012736\n },\n \"Valuation\": {\n  \"TrailingPE\": 33.15,\n  \"ForwardPE\": 30.77,\n  \"PriceSalesTTM\": 8.84,\n  \"PriceBookMRQ\": 60.72,\n  \"EnterpriseValue\": 4672684000000\n },\n \"SharesStats\": {\n  \"SharesOutstanding\": 14687356000,\n  \"SharesFloat\": 14662387495,\n  \"PercentInsiders\": 1.631,\n  \"PercentInstitutions\": 65.732\n },\n \"outstandingShares\": {\n  \"annual\": {\n   \"0\": {\n    \"date\": \"2026\",\n    \"dateFormatted\": \"2026-12-31\",\n    \"sharesMln\": \"14768.1150\",\n    \"shares\": 14768115000\n   }\n  }\n },\n \"Financials\": {\n  \"Income_Statement\": {\n   \"currency_symbol\": \"USD\",\n   \"yearly\": {\n    \"2025-09-30\": {\n     \"date\": \"2025-09-30\",\n     \"totalRevenue\": \"416161000000.00\",\n     \"grossProfit\": \"195201000000.00\",\n     \"operatingIncome\": \"133050000000.00\",\n     \"netIncome\": \"112010000000.00\",\n     \"ebitda\": \"144427000000.00\"\n    }\n   }\n  }\n }\n}",
    "tags": [
      "fundamentals",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Full fundamentals object for a common stock or equity ticker."
    ],
    "input_entities": [
      "symbol",
      "filter",
      "historical",
      "from",
      "to",
      "fmt"
    ],
    "output_entities": [
      "fundamentals"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "Full fundamentals object for a common stock or equity ticker.",
    "external_docs_url": "https://eodhd.com/financial-apis/stock-etfs-fundamental-data-feeds",
    "documentation_markdown": "GET /api/v1.1/fundamentals/{symbol} — Full fundamentals object for a common stock or equity ticker.\n\nReturns one JSON object per equity ticker with company profile, headline metrics, valuation multiples, share statistics, technicals, dividends and splits, institutional and fund ownership, insider transactions, historical shares outstanding, earnings history and trend, and full annual and quarterly financial statements. It is the single richest company endpoint in the EODHD catalogue and is the basis for screening, valuation and financial statement analysis. The payload is very large, so use the filter parameter to request one section or one field. Major US companies go back to 1985 and non-US symbols to 2000; smaller companies carry the last 6 years and 20 quarters, and the feed is available in JSON only.\n\nEODHD call weight: 10; required plans: all_in_one, fundamentals_data_feed.\n\nv1.1 is the recommended version; the unversioned /api/fundamentals/{symbol} path still works and takes the same parameters. v1.1 fixes missing Q4 rows in Earnings::Trend and splits that block into Quarterly and Annual sections with an explicit quarter label. The current documentation does not list AnalystRatings or ESGScores as top-level sections of the single-ticker response, so they are deliberately absent from the schema even though AnalystRatings appears in the Bulk Fundamentals output. The demo token works for AAPL.US, TSLA.US, VTI.US, AMZN.US, BTC-USD.CC and EURUSD.FOREX only. Financial statement line items and several nested numeric values arrive as strings, so cast before computing. Not every issuer reports every field, so expect nulls and missing keys.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "fundamentals_stock"
  },
  {
    "id": "65113d93-6e6b-5385-98ca-038b0085202d",
    "method": "GET",
    "path": "/api/fundamentals/{symbol}",
    "price": 0.043478,
    "description": "Unversioned legacy path of the Fundamental Data API, kept for backward compatibility.",
    "query_params": {
      "type": "object",
      "properties": {
        "symbol": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "Ticker in TICKER.EXCHANGE format, for example AAPL.US. Also accepts index tickers such as GSPC.INDX and mutual fund ISINs."
        },
        "filter": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Return only one section or a single field instead of the full object. Nest layers with :: and pass several comma-separated filters at once. Section and field names are the response keys, for example General, General::Code, Financials::Balance_Sheet::quarterly, or General::Code,General,Earnings."
        },
        "historical": {
          "type": "integer",
          "required": false,
          "x-parameter-location": "query",
          "description": "Set to 1 to include point-in-time index membership snapshots (HistoricalComponents). Indices only; combine with from and to."
        },
        "from": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Start date (YYYY-MM-DD) for historical index snapshots, used with historical=1."
        },
        "to": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "End date (YYYY-MM-DD) for historical index snapshots, used with historical=1."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The fundamentals feed is JSON only because of the complex data structure, so json is the only useful value."
        }
      },
      "required": [
        "symbol"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.fundamentals_stock_legacy_path",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.05,
      "x-apiosk-plans": [
        "all_in_one",
        "fundamentals_data_feed"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "Full fundamentals object for a common stock or equity ticker. Sections are omitted when the issuer does not report them.",
      "properties": {
        "General": {
          "type": "object",
          "description": "Company profile block.",
          "properties": {
            "Code": {
              "type": [
                "string",
                "null"
              ],
              "description": "The ticker code of the company."
            },
            "Type": {
              "type": [
                "string",
                "null"
              ],
              "description": "The instrument type, for example Common Stock."
            },
            "Name": {
              "type": [
                "string",
                "null"
              ],
              "description": "The company name."
            },
            "Exchange": {
              "type": [
                "string",
                "null"
              ],
              "description": "The exchange the ticker is listed on."
            },
            "CurrencyCode": {
              "type": [
                "string",
                "null"
              ],
              "description": "Currency code of the listing."
            },
            "CurrencyName": {
              "type": [
                "string",
                "null"
              ],
              "description": "Full name of the listing currency."
            },
            "CurrencySymbol": {
              "type": [
                "string",
                "null"
              ],
              "description": "Symbol of the listing currency."
            },
            "CountryName": {
              "type": [
                "string",
                "null"
              ],
              "description": "Country where the company is registered."
            },
            "CountryISO": {
              "type": [
                "string",
                "null"
              ],
              "description": "ISO country code."
            },
            "OpenFigi": {
              "type": [
                "string",
                "null"
              ],
              "description": "The OpenFIGI identifier."
            },
            "ISIN": {
              "type": [
                "string",
                "null"
              ],
              "description": "International Securities Identification Number."
            },
            "LEI": {
              "type": [
                "string",
                "null"
              ],
              "description": "Legal Entity Identifier."
            },
            "PrimaryTicker": {
              "type": [
                "string",
                "null"
              ],
              "description": "The primary ticker for the company."
            },
            "CUSIP": {
              "type": [
                "string",
                "null"
              ],
              "description": "CUSIP identifier, mainly for US companies."
            },
            "CIK": {
              "type": [
                "string",
                "null"
              ],
              "description": "Central Index Key assigned by the SEC, US companies."
            },
            "EmployerIdNumber": {
              "type": [
                "string",
                "null"
              ],
              "description": "Employer Identification Number (EIN), US companies."
            },
            "FiscalYearEnd": {
              "type": [
                "string",
                "null"
              ],
              "description": "Month the fiscal year ends."
            },
            "IPODate": {
              "type": [
                "string",
                "null"
              ],
              "format": "date",
              "description": "Date of the initial public offering."
            },
            "InternationalDomestic": {
              "type": [
                "string",
                "null"
              ],
              "description": "International or domestic classification."
            },
            "Sector": {
              "type": [
                "string",
                "null"
              ],
              "description": "The sector of the company."
            },
            "Industry": {
              "type": [
                "string",
                "null"
              ],
              "description": "The industry of the company."
            },
            "GicSector": {
              "type": [
                "string",
                "null"
              ],
              "description": "GICS sector classification."
            },
            "GicGroup": {
              "type": [
                "string",
                "null"
              ],
              "description": "GICS industry group classification."
            },
            "GicIndustry": {
              "type": [
                "string",
                "null"
              ],
              "description": "GICS industry classification."
            },
            "GicSubIndustry": {
              "type": [
                "string",
                "null"
              ],
              "description": "GICS sub-industry classification."
            },
            "HomeCategory": {
              "type": [
                "string",
                "null"
              ],
              "description": "Home category of the ticker, for example Domestic or ADR."
            },
            "IsDelisted": {
              "type": [
                "boolean",
                "null"
              ],
              "description": "Whether the ticker has been delisted."
            },
            "Description": {
              "type": [
                "string",
                "null"
              ],
              "description": "A textual description of the company."
            },
            "Address": {
              "type": [
                "string",
                "null"
              ],
              "description": "The company address as a single string."
            },
            "AddressData": {
              "type": "object",
              "additionalProperties": true,
              "description": "The address split into Street, City, State, Country and ZIP."
            },
            "Listings": {
              "type": "object",
              "additionalProperties": true,
              "description": "Other exchange listings for the ticker, keyed by index, each entry with Code, Exchange and Name."
            },
            "Officers": {
              "type": "object",
              "additionalProperties": true,
              "description": "Company executives, keyed by index, each entry with Name, Title and YearBorn."
            },
            "Phone": {
              "type": [
                "string",
                "null"
              ],
              "description": "Company phone number."
            },
            "WebURL": {
              "type": [
                "string",
                "null"
              ],
              "description": "Company website URL."
            },
            "LogoURL": {
              "type": [
                "string",
                "null"
              ],
              "description": "Path to the company logo image."
            },
            "FullTimeEmployees": {
              "type": [
                "integer",
                "null"
              ],
              "description": "Number of full-time employees."
            },
            "UpdatedAt": {
              "type": [
                "string",
                "null"
              ],
              "format": "date",
              "description": "Date the record was last updated."
            },
            "ExchangeMarket": {
              "type": [
                "string",
                "null"
              ],
              "description": "Extra market label that may appear on non-US equities only."
            }
          },
          "additionalProperties": true
        },
        "Highlights": {
          "type": "object",
          "description": "Key metrics and analyst estimates.",
          "properties": {
            "MarketCapitalization": {
              "type": [
                "number",
                "null"
              ],
              "description": "Market capitalization."
            },
            "MarketCapitalizationMln": {
              "type": [
                "number",
                "null"
              ],
              "description": "Market capitalization in millions."
            },
            "EBITDA": {
              "type": [
                "number",
                "null"
              ],
              "description": "Earnings before interest, taxes, depreciation and amortization."
            },
            "PERatio": {
              "type": [
                "number",
                "null"
              ],
              "description": "Price-to-earnings ratio."
            },
            "PEGRatio": {
              "type": [
                "number",
                "null"
              ],
              "description": "Price/earnings-to-growth ratio."
            },
            "WallStreetTargetPrice": {
              "type": [
                "number",
                "null"
              ],
              "description": "Average analyst target price."
            },
            "BookValue": {
              "type": [
                "number",
                "null"
              ],
              "description": "Book value per share."
            },
            "DividendShare": {
              "type": [
                "number",
                "null"
              ],
              "description": "Dividend per share."
            },
            "DividendYield": {
              "type": [
                "number",
                "null"
              ],
              "description": "Dividend yield."
            },
            "EarningsShare": {
              "type": [
                "number",
                "null"
              ],
              "description": "Earnings per share (EPS)."
            },
            "EPSEstimateCurrentYear": {
              "type": [
                "number",
                "null"
              ],
              "description": "Estimated EPS for the current year."
            },
            "EPSEstimateNextYear": {
              "type": [
                "number",
                "null"
              ],
              "description": "Estimated EPS for the next year."
            },
            "EPSEstimateNextQuarter": {
              "type": [
                "number",
                "null"
              ],
              "description": "Estimated EPS for the next quarter."
            },
            "EPSEstimateCurrentQuarter": {
              "type": [
                "number",
                "null"
              ],
              "description": "Estimated EPS for the current quarter."
            },
            "MostRecentQuarter": {
              "type": [
                "string",
                "null"
              ],
              "format": "date",
              "description": "Date of the most recent reported quarter."
            },
            "ProfitMargin": {
              "type": [
                "number",
                "null"
              ],
              "description": "Profit margin."
            },
            "OperatingMarginTTM": {
              "type": [
                "number",
                "null"
              ],
              "description": "Operating margin over the trailing twelve months."
            },
            "ReturnOnAssetsTTM": {
              "type": [
                "number",
                "null"
              ],
              "description": "Return on assets over the trailing twelve months."
            },
            "ReturnOnEquityTTM": {
              "type": [
                "number",
                "null"
              ],
              "description": "Return on equity over the trailing twelve months."
            },
            "RevenueTTM": {
              "type": [
                "number",
                "null"
              ],
              "description": "Revenue over the trailing twelve months."
            },
            "RevenuePerShareTTM": {
              "type": [
                "number",
                "null"
              ],
              "description": "Revenue per share over the trailing twelve months."
            },
            "QuarterlyRevenueGrowthYOY": {
              "type": [
                "number",
                "null"
              ],
              "description": "Quarterly revenue growth year over year."
            },
            "GrossProfitTTM": {
              "type": [
                "number",
                "null"
              ],
              "description": "Gross profit over the trailing twelve months."
            },
            "DilutedEpsTTM": {
              "type": [
                "number",
                "null"
              ],
              "description": "Diluted EPS over the trailing twelve months."
            },
            "QuarterlyEarningsGrowthYOY": {
              "type": [
                "number",
                "null"
              ],
              "description": "Quarterly earnings growth year over year."
            }
          },
          "additionalProperties": true
        },
        "Valuation": {
          "type": "object",
          "description": "Market valuation ratios.",
          "properties": {
            "TrailingPE": {
              "type": [
                "number",
                "null"
              ],
              "description": "Trailing price-to-earnings ratio."
            },
            "ForwardPE": {
              "type": [
                "number",
                "null"
              ],
              "description": "Forward price-to-earnings ratio."
            },
            "PriceSalesTTM": {
              "type": [
                "number",
                "null"
              ],
              "description": "Price-to-sales ratio over the trailing twelve months."
            },
            "PriceBookMRQ": {
              "type": [
                "number",
                "null"
              ],
              "description": "Price-to-book ratio for the most recent quarter."
            },
            "EnterpriseValue": {
              "type": [
                "number",
                "null"
              ],
              "description": "Enterprise value."
            },
            "EnterpriseValueRevenue": {
              "type": [
                "number",
                "null"
              ],
              "description": "Enterprise value to revenue ratio."
            },
            "EnterpriseValueEbitda": {
              "type": [
                "number",
                "null"
              ],
              "description": "Enterprise value to EBITDA ratio."
            }
          },
          "additionalProperties": true
        },
        "SharesStats": {
          "type": "object",
          "description": "Share statistics and ownership percentages.",
          "properties": {
            "SharesOutstanding": {
              "type": [
                "number",
                "null"
              ],
              "description": "Total shares outstanding."
            },
            "SharesFloat": {
              "type": [
                "number",
                "null"
              ],
              "description": "Shares available for public trading."
            },
            "PercentInsiders": {
              "type": [
                "number",
                "null"
              ],
              "description": "Percentage of shares held by insiders."
            },
            "PercentInstitutions": {
              "type": [
                "number",
                "null"
              ],
              "description": "Percentage of shares held by institutions."
            },
            "SharesShort": {
              "type": [
                "number",
                "null"
              ],
              "description": "Number of shares sold short."
            },
            "SharesShortPriorMonth": {
              "type": [
                "number",
                "null"
              ],
              "description": "Shares sold short in the prior month."
            },
            "ShortRatio": {
              "type": [
                "number",
                "null"
              ],
              "description": "Short interest ratio."
            },
            "ShortPercentOutstanding": {
              "type": [
                "number",
                "null"
              ],
              "description": "Short interest as a percentage of shares outstanding."
            },
            "ShortPercentFloat": {
              "type": [
                "number",
                "null"
              ],
              "description": "Short interest as a percentage of the float."
            }
          },
          "additionalProperties": true
        },
        "Technicals": {
          "type": "object",
          "description": "Technical performance metrics.",
          "properties": {
            "Beta": {
              "type": [
                "number",
                "null"
              ],
              "description": "Measure of stock volatility relative to the market."
            },
            "52WeekHigh": {
              "type": [
                "number",
                "null"
              ],
              "description": "Highest price over the last 52 weeks."
            },
            "52WeekLow": {
              "type": [
                "number",
                "null"
              ],
              "description": "Lowest price over the last 52 weeks."
            },
            "50DayMA": {
              "type": [
                "number",
                "null"
              ],
              "description": "50-day moving average."
            },
            "200DayMA": {
              "type": [
                "number",
                "null"
              ],
              "description": "200-day moving average."
            },
            "SharesShort": {
              "type": [
                "number",
                "null"
              ],
              "description": "Number of shares sold short."
            },
            "SharesShortPriorMonth": {
              "type": [
                "number",
                "null"
              ],
              "description": "Shares sold short in the prior month."
            },
            "ShortRatio": {
              "type": [
                "number",
                "null"
              ],
              "description": "Short interest ratio."
            },
            "ShortPercent": {
              "type": [
                "number",
                "null"
              ],
              "description": "Short interest as a percentage."
            }
          },
          "additionalProperties": true
        },
        "SplitsDividends": {
          "type": "object",
          "description": "Dividend and split summary.",
          "properties": {
            "ForwardAnnualDividendRate": {
              "type": [
                "number",
                "null"
              ],
              "description": "Forward annual dividend rate."
            },
            "ForwardAnnualDividendYield": {
              "type": [
                "number",
                "null"
              ],
              "description": "Forward annual dividend yield."
            },
            "PayoutRatio": {
              "type": [
                "number",
                "null"
              ],
              "description": "Dividend payout ratio."
            },
            "DividendDate": {
              "type": [
                "string",
                "null"
              ],
              "format": "date",
              "description": "The dividend payment date."
            },
            "ExDividendDate": {
              "type": [
                "string",
                "null"
              ],
              "format": "date",
              "description": "The ex-dividend date."
            },
            "LastSplitFactor": {
              "type": [
                "string",
                "null"
              ],
              "description": "The factor of the last stock split, for example 4:1."
            },
            "LastSplitDate": {
              "type": [
                "string",
                "null"
              ],
              "format": "date",
              "description": "The date of the last stock split."
            },
            "NumberDividendsByYear": {
              "type": "object",
              "additionalProperties": true,
              "description": "Count of dividends paid per year, keyed by index, each entry with Year and Count."
            }
          },
          "additionalProperties": true
        },
        "Holders": {
          "type": "object",
          "description": "Institutional and fund ownership.",
          "properties": {
            "Institutions": {
              "type": "object",
              "additionalProperties": true,
              "description": "Institutional holders keyed by index string (\"0\", \"1\", ...). Each entry has name, date (report date), totalShares (percent of total shares held), totalAssets (percent of the holder assets in this position), currentShares (share count), change and change_p."
            },
            "Funds": {
              "type": "object",
              "additionalProperties": true,
              "description": "Fund holders keyed by index string. Same entry shape as Institutions: name, date, totalShares, totalAssets, currentShares, change, change_p."
            }
          },
          "additionalProperties": true
        },
        "InsiderTransactions": {
          "type": "object",
          "additionalProperties": true,
          "description": "Insider trading records keyed by index string. Each entry has date (report date), ownerCik, ownerName, transactionDate, transactionCode, transactionAmount (share count), transactionPrice, transactionAcquiredDisposed, postTransactionAmount and secLink (URL of the SEC filing)."
        },
        "outstandingShares": {
          "type": "object",
          "description": "Historical shares outstanding.",
          "properties": {
            "annual": {
              "type": "object",
              "additionalProperties": true,
              "description": "Yearly history keyed by index string. Each entry has date (period label), dateFormatted (period end date, YYYY-MM-DD), sharesMln (shares in millions, delivered as a string) and shares (integer share count)."
            },
            "quarterly": {
              "type": "object",
              "additionalProperties": true,
              "description": "Quarterly history keyed by index string. Same entry shape as annual: date, dateFormatted, sharesMln, shares."
            }
          },
          "additionalProperties": true
        },
        "Earnings": {
          "type": "object",
          "description": "Reported earnings, analyst trend and annual earnings.",
          "properties": {
            "History": {
              "type": "object",
              "additionalProperties": true,
              "description": "Reported earnings keyed by fiscal period end date (YYYY-MM-DD). Each entry has reportDate, date, beforeAfterMarket, currency, epsActual, epsEstimate, epsDifference and surprisePercent."
            },
            "Trend": {
              "type": "object",
              "additionalProperties": true,
              "description": "Analyst estimates by period. In v1.1 this block is split into Quarterly and Annual sections instead of one flat date-keyed list, and each quarterly entry carries a human-readable quarter field (Q1, Q2, Q3, Q4). Entries include growth, earningsEstimateAvg, earningsEstimateLow, earningsEstimateHigh, revenueEstimateAvg, the EPS trend values and the EPS revision counts."
            },
            "Annual": {
              "type": "object",
              "additionalProperties": true,
              "description": "Annual earnings keyed by date. Each entry has date and epsActual."
            }
          },
          "additionalProperties": true
        },
        "Financials": {
          "type": "object",
          "description": "Full financial statements.",
          "properties": {
            "Balance_Sheet": {
              "type": "object",
              "description": "Balance sheet.",
              "properties": {
                "currency_symbol": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "Reporting currency of the statement."
                },
                "yearly": {
                  "type": "object",
                  "additionalProperties": true,
                  "description": "Yearly reports keyed by period end date (YYYY-MM-DD). Each report carries its own date and filing_date plus the line items, for example totalAssets, totalLiab, totalStockholderEquity, cash, netDebt, longTermDebt, inventory and retainedEarnings. Numeric line items are delivered as strings."
                },
                "quarterly": {
                  "type": "object",
                  "additionalProperties": true,
                  "description": "Quarterly reports keyed by period end date (YYYY-MM-DD). Same entry shape as yearly."
                }
              },
              "additionalProperties": true
            },
            "Income_Statement": {
              "type": "object",
              "description": "Income statement.",
              "properties": {
                "currency_symbol": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "Reporting currency of the statement."
                },
                "yearly": {
                  "type": "object",
                  "additionalProperties": true,
                  "description": "Yearly reports keyed by period end date (YYYY-MM-DD). Each report carries its own date and filing_date plus the line items, for example totalRevenue, costOfRevenue, grossProfit, operatingIncome, ebit, ebitda, netIncome and researchDevelopment. Numeric line items are delivered as strings."
                },
                "quarterly": {
                  "type": "object",
                  "additionalProperties": true,
                  "description": "Quarterly reports keyed by period end date (YYYY-MM-DD). Same entry shape as yearly."
                }
              },
              "additionalProperties": true
            },
            "Cash_Flow": {
              "type": "object",
              "description": "Cash flow statement.",
              "properties": {
                "currency_symbol": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "Reporting currency of the statement."
                },
                "yearly": {
                  "type": "object",
                  "additionalProperties": true,
                  "description": "Yearly reports keyed by period end date (YYYY-MM-DD). Each report carries its own date and filing_date plus the line items, for example totalCashFromOperatingActivities, capitalExpenditures, freeCashFlow, dividendsPaid, netIncome and changeInCash. Numeric line items are delivered as strings."
                },
                "quarterly": {
                  "type": "object",
                  "additionalProperties": true,
                  "description": "Quarterly reports keyed by period end date (YYYY-MM-DD). Same entry shape as yearly."
                }
              },
              "additionalProperties": true
            }
          },
          "additionalProperties": true
        }
      },
      "additionalProperties": true
    },
    "request_example": {
      "symbol": "AAPL.US",
      "filter": "Financials::Balance_Sheet::quarterly",
      "historical": 1,
      "from": "2010-01-01",
      "to": "2010-12-31",
      "fmt": "json"
    },
    "response_example": null,
    "tags": [
      "fundamentals",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Unversioned legacy path of the Fundamental Data API, kept for backward compatibility."
    ],
    "input_entities": [
      "symbol",
      "filter",
      "historical",
      "from",
      "to",
      "fmt"
    ],
    "output_entities": [
      "fundamentals"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "Unversioned legacy path of the Fundamental Data API, kept for backward compatibility.",
    "external_docs_url": "https://eodhd.com/financial-apis/stock-etfs-fundamental-data-feeds",
    "documentation_markdown": "GET /api/fundamentals/{symbol} — Unversioned legacy path of the Fundamental Data API, kept for backward compatibility.\n\nSame endpoint and same parameters as the v1.1 fundamentals path, served without the version prefix. It is documented as staying available for backward compatibility with existing integrations. Use it only when an existing client hardcodes the old URL; new integrations should call the v1.1 path because it fixes the missing Q4 rows in the earnings trend. All of the documentation examples for index constituents still use this path.\n\nEODHD call weight: 10; required plans: all_in_one, fundamentals_data_feed.\n\nIdentical behaviour to /api/v1.1/fundamentals/{symbol} except that Earnings::Trend keeps the original flat date-keyed layout in which a quarterly and an annual estimate sharing the same date could collide and silently drop Q4. The documentation gives no separate example response for this path.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "fundamentals_stock_legacy_path"
  },
  {
    "id": "021a7185-2f97-596e-a835-e603a7fe7155",
    "method": "GET",
    "path": "/api/bulk-fundamentals/{EXCHANGE_CODE}",
    "price": 0.108696,
    "description": "Reduced fundamentals for a whole exchange, a paginated slice of it, or a chosen list of symbols.",
    "query_params": {
      "type": "object",
      "properties": {
        "EXCHANGE_CODE": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "Exchange code, for example US, NYSE, NASDAQ, AMEX, BATS, or any non-US code such as LSE, XETRA or TO."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "csv",
          "required": false,
          "x-parameter-location": "query",
          "description": "Output format. Set to json for JSON; the default is CSV. JSON is strongly recommended, and version 1.2 is JSON only."
        },
        "offset": {
          "type": "integer",
          "default": 0,
          "required": false,
          "x-parameter-location": "query",
          "description": "Starting position for pagination, the number of symbols to skip."
        },
        "limit": {
          "type": "integer",
          "default": 500,
          "required": false,
          "x-parameter-location": "query",
          "description": "Number of symbols to return. The maximum is 500 per request."
        },
        "symbols": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Comma-separated list of tickers to fetch. When set, the exchange code in the path is ignored."
        },
        "version": {
          "type": "string",
          "default": "1.0",
          "required": false,
          "x-parameter-location": "query",
          "description": "Output template. Use 1.2 for the full current Fundamentals template including Earnings Trends. When omitted the API falls back to 1.0, a leaner legacy format that omits some blocks."
        }
      },
      "required": [
        "EXCHANGE_CODE"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.bulk_fundamentals",
      "x-apiosk-call-weight": 100,
      "x-apiosk-provider-cost-eur": 0.005,
      "x-apiosk-target-buyer-price-eur": 0.125,
      "x-apiosk-plans": [
        "extended_fundamentals"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "array",
      "description": "One object per company, in the order the exchange or the symbols list produced them.",
      "items": {
        "type": "object",
        "description": "Reduced fundamentals record for one company. Mirrors the single-ticker Fundamentals API with fewer fields.",
        "properties": {
          "General": {
            "type": "object",
            "additionalProperties": true,
            "description": "Identification and classification: Code, Name, Exchange, currency, country, ISIN, CUSIP, CIK, sector and industry, GIC classification, IPO date, delisting flag and description."
          },
          "Highlights": {
            "type": "object",
            "additionalProperties": true,
            "description": "Headline metrics: market capitalization, EBITDA, PE and PEG ratios, book value, dividend share and yield, EPS and EPS estimates, margins and returns."
          },
          "Valuation": {
            "type": "object",
            "additionalProperties": true,
            "description": "Valuation multiples: trailing and forward PE, price-to-sales, price-to-book, enterprise value and its ratios."
          },
          "SharesStats": {
            "type": "object",
            "additionalProperties": true,
            "description": "Share statistics: shares outstanding and float, insider and institutional ownership, short interest."
          },
          "Technicals": {
            "type": "object",
            "additionalProperties": true,
            "description": "Technical figures: beta, 52-week high and low, 50- and 200-day moving averages, short-interest figures."
          },
          "SplitsDividends": {
            "type": "object",
            "additionalProperties": true,
            "description": "Forward dividend rate and yield, payout ratio, dividend and ex-dividend dates, last split factor and date."
          },
          "AnalystRatings": {
            "type": "object",
            "additionalProperties": true,
            "description": "Consensus rating, target price and the counts of strong buy, buy, hold, sell and strong sell ratings."
          },
          "Earnings": {
            "type": "object",
            "additionalProperties": true,
            "description": "Earnings trend plus the last four reported quarters under the keys Last_0 to Last_3."
          },
          "Financials": {
            "type": "object",
            "additionalProperties": true,
            "description": "Balance_Sheet, Cash_Flow and Income_Statement, limited to the last 4 quarters and last 4 years."
          }
        },
        "additionalProperties": true
      }
    },
    "request_example": {
      "EXCHANGE_CODE": "NASDAQ",
      "fmt": "json",
      "offset": 1000,
      "limit": 200,
      "symbols": "AAPL.US,MSFT.US",
      "version": "1.2"
    },
    "response_example": "[\n {\n  \"General\": {\n   \"Code\": \"AAPL\",\n   \"Type\": \"Common Stock\",\n   \"Name\": \"Apple Inc.\",\n   \"Exchange\": \"NASDAQ\",\n   \"ISIN\": \"US0378331005\",\n   \"Sector\": \"Technology\"\n  },\n  \"Highlights\": {\n   \"MarketCapitalization\": 3200000000000,\n   \"PERatio\": 32.1,\n   \"DividendYield\": 0.0045,\n   \"EarningsShare\": 6.61\n  },\n  \"Valuation\": {\n   \"TrailingPE\": 32.1,\n   \"ForwardPE\": 28.4,\n   \"PriceSalesTTM\": 8.2,\n   \"PriceBookMRQ\": 51.3\n  },\n  \"SharesStats\": {\n   \"SharesOutstanding\": 14900000000,\n   \"SharesFloat\": 14880000000\n  },\n  \"Technicals\": {\n   \"Beta\": 1.24,\n   \"52WeekHigh\": 260.1,\n   \"52WeekLow\": 164.1,\n   \"50DayMA\": 228.4\n  },\n  \"SplitsDividends\": {\n   \"ForwardAnnualDividendRate\": 1.04,\n   \"PayoutRatio\": 0.15,\n   \"ExDividendDate\": \"2026-05-12\"\n  },\n  \"AnalystRatings\": {\n   \"Rating\": 4.2,\n   \"TargetPrice\": 275.0,\n   \"Buy\": 21,\n   \"Hold\": 8\n  }\n }\n]",
    "tags": [
      "fundamentals",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Reduced fundamentals for a whole exchange, a paginated slice of it, or a chosen list of symbols."
    ],
    "input_entities": [
      "EXCHANGE_CODE",
      "fmt",
      "offset",
      "limit",
      "symbols",
      "version"
    ],
    "output_entities": [
      "fundamentals"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "Reduced fundamentals for a whole exchange, a paginated slice of it, or a chosen list of symbols.",
    "external_docs_url": "https://eodhd.com/financial-apis/bulk-fundamentals-api-via-extended-fundamentals-plan",
    "documentation_markdown": "GET /api/bulk-fundamentals/{EXCHANGE_CODE} — Reduced fundamentals for a whole exchange, a paginated slice of it, or a chosen list of symbols.\n\nReturns a JSON array with one reduced fundamentals record per company, reading live data. Use offset and limit to page through an exchange up to 500 symbols at a time, or pass symbols to fetch a specific list, in which case the exchange code in the path is ignored. Each record carries fewer fields than a single-ticker call and history is capped at the last 4 quarters and last 4 years. Common stocks only; ETFs and mutual funds are not supported.\n\nEODHD call weight: 100; required plans: extended_fundamentals.\n\nAccess requires the Extended Fundamentals subscription, which is quoted on request; the All-In-One package on its own does not include it, so this is not covered by the plans that unlock the single-ticker endpoint. Cost is 100 API calls per request without symbols and 100 plus the number of symbols with them, so 3 symbols cost 103 calls. The same feed is also reachable at /api/v1.1/bulk-fundamentals/{EXCHANGE_CODE}. The documentation shows the block names and their contents in prose but does not enumerate every field of every block, so the second level is modelled as free-form objects. Version 1.0 is the silent default and omits Earnings Trends, so always send version=1.2.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "bulk_fundamentals"
  },
  {
    "id": "25442988-3222-5e35-aadd-75788a729bdd",
    "method": "GET",
    "path": "/api/v2/bulk-fundamentals/{EXCHANGE_CODE}",
    "price": 5.434783,
    "description": "Full-exchange fundamentals snapshot returned in a single call, served from an hourly cache.",
    "query_params": {
      "type": "object",
      "properties": {
        "EXCHANGE_CODE": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "One of the six exchanges with a prebuilt snapshot: NASDAQ, NYSE, AMEX, BATS, TO or V. Case-insensitive. Any other code, including the general US code, returns 404."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. This endpoint answers in JSON only; offset, limit and symbols are ignored here."
        }
      },
      "required": [
        "EXCHANGE_CODE"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.bulk_fundamentals_snapshot",
      "x-apiosk-call-weight": 5000,
      "x-apiosk-provider-cost-eur": 0.25,
      "x-apiosk-target-buyer-price-eur": 6.25,
      "x-apiosk-plans": [
        "extended_fundamentals"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "array",
      "description": "One object per company, in the order the exchange or the symbols list produced them.",
      "items": {
        "type": "object",
        "description": "Reduced fundamentals record for one company. Mirrors the single-ticker Fundamentals API with fewer fields.",
        "properties": {
          "General": {
            "type": "object",
            "additionalProperties": true,
            "description": "Identification and classification: Code, Name, Exchange, currency, country, ISIN, CUSIP, CIK, sector and industry, GIC classification, IPO date, delisting flag and description."
          },
          "Highlights": {
            "type": "object",
            "additionalProperties": true,
            "description": "Headline metrics: market capitalization, EBITDA, PE and PEG ratios, book value, dividend share and yield, EPS and EPS estimates, margins and returns."
          },
          "Valuation": {
            "type": "object",
            "additionalProperties": true,
            "description": "Valuation multiples: trailing and forward PE, price-to-sales, price-to-book, enterprise value and its ratios."
          },
          "SharesStats": {
            "type": "object",
            "additionalProperties": true,
            "description": "Share statistics: shares outstanding and float, insider and institutional ownership, short interest."
          },
          "Technicals": {
            "type": "object",
            "additionalProperties": true,
            "description": "Technical figures: beta, 52-week high and low, 50- and 200-day moving averages, short-interest figures."
          },
          "SplitsDividends": {
            "type": "object",
            "additionalProperties": true,
            "description": "Forward dividend rate and yield, payout ratio, dividend and ex-dividend dates, last split factor and date."
          },
          "AnalystRatings": {
            "type": "object",
            "additionalProperties": true,
            "description": "Consensus rating, target price and the counts of strong buy, buy, hold, sell and strong sell ratings."
          },
          "Earnings": {
            "type": "object",
            "additionalProperties": true,
            "description": "Earnings trend plus the last four reported quarters under the keys Last_0 to Last_3."
          },
          "Financials": {
            "type": "object",
            "additionalProperties": true,
            "description": "Balance_Sheet, Cash_Flow and Income_Statement, limited to the last 4 quarters and last 4 years."
          }
        },
        "additionalProperties": true
      }
    },
    "request_example": {
      "EXCHANGE_CODE": "NASDAQ",
      "fmt": "json"
    },
    "response_example": null,
    "tags": [
      "fundamentals",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Full-exchange fundamentals snapshot returned in a single call, served from an hourly cache."
    ],
    "input_entities": [
      "EXCHANGE_CODE",
      "fmt"
    ],
    "output_entities": [
      "fundamentals"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "Full-exchange fundamentals snapshot returned in a single call, served from an hourly cache.",
    "external_docs_url": "https://eodhd.com/financial-apis/bulk-fundamentals-api-via-extended-fundamentals-plan",
    "documentation_markdown": "GET /api/v2/bulk-fundamentals/{EXCHANGE_CODE} — Full-exchange fundamentals snapshot returned in a single call, served from an hourly cache.\n\nReturns every symbol on the exchange in one JSON response, built from a snapshot that is rebuilt hourly in the background. It takes no offset, limit or symbols parameters, which are ignored, and it replaces the many paginated calls the bulk endpoint would need. Only six exchanges have a prebuilt snapshot, and the payload is very large: NASDAQ is roughly 234 MB for about 3,070 companies, NYSE roughly 168 MB and AMEX roughly 13 MB.\n\nEODHD call weight: 5000; required plans: extended_fundamentals.\n\nCosts 5,000 API calls per request, by far the heaviest endpoint in this group. Returns HTTP 503 with a Retry-After header while the hourly snapshot is being rebuilt, so a client must honour that header and retry. Data is at most one hour stale, unlike the live bulk endpoint. Output blocks are identical to the bulk endpoint at version 1.2. Same 4 quarters and 4 years history cap, and stocks only.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "bulk_fundamentals_snapshot"
  },
  {
    "id": "39fbb501-51e7-5488-aa1e-de69b7bd5237",
    "method": "GET",
    "path": "/api/sec-filings/{symbol}/form4",
    "price": 0.043478,
    "description": "SEC Form 4 insider filings for one US issuer, with non-derivative and derivative rows and footnotes.",
    "query_params": {
      "type": "object",
      "properties": {
        "symbol": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "US ticker, for example AAPL or AAPL.US. Case-insensitive and the .US suffix is optional; both forms return the same data. Form 4 is filed by US-listed issuers only."
        },
        "page[offset]": {
          "type": "integer",
          "default": 0,
          "required": false,
          "x-parameter-location": "query",
          "description": "Pagination offset, zero-based."
        },
        "page[limit]": {
          "type": "integer",
          "default": 20,
          "required": false,
          "x-parameter-location": "query",
          "description": "Page size, between 1 and 100."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The documented examples pass fmt=json; the payload is JSON."
        }
      },
      "required": [
        "symbol"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.insider_transactions_form4",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.05,
      "x-apiosk-plans": [
        "all_in_one",
        "fundamentals_data_feed"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "Paginated list of SEC Form 4 filings for one US issuer, newest filed_at first.",
      "properties": {
        "data": {
          "type": "array",
          "description": "One entry per Form 4 filing.",
          "items": {
            "type": "object",
            "description": "A single Form 4 filing with its transaction rows and footnotes.",
            "properties": {
              "accession_number": {
                "type": [
                  "string",
                  "null"
                ],
                "description": "SEC accession number, unique identifier of the filing."
              },
              "filed_at": {
                "type": [
                  "string",
                  "null"
                ],
                "format": "date",
                "description": "Date the filing was submitted to the SEC, YYYY-MM-DD."
              },
              "period_of_report": {
                "type": [
                  "string",
                  "null"
                ],
                "format": "date",
                "description": "Reporting period, YYYY-MM-DD."
              },
              "non_derivative": {
                "type": "array",
                "description": "Direct stock transactions (Common Stock, Class A/B/C and similar). May be empty when the filing reports only derivative activity.",
                "items": {
                  "type": "object",
                  "description": "One non-derivative transaction row.",
                  "properties": {
                    "reporting_owner_cik": {
                      "type": [
                        "string",
                        "null"
                      ],
                      "description": "SEC CIK of the insider."
                    },
                    "reporting_owner_name": {
                      "type": [
                        "string",
                        "null"
                      ],
                      "description": "Full name of the insider."
                    },
                    "is_director": {
                      "type": [
                        "boolean",
                        "null"
                      ],
                      "description": "True if the insider is a board director."
                    },
                    "is_officer": {
                      "type": [
                        "boolean",
                        "null"
                      ],
                      "description": "True if the insider is a company officer."
                    },
                    "is_ten_percent_owner": {
                      "type": [
                        "boolean",
                        "null"
                      ],
                      "description": "True if the insider is a 10 percent or greater owner."
                    },
                    "is_other": {
                      "type": [
                        "boolean",
                        "null"
                      ],
                      "description": "True if the insider has another relationship."
                    },
                    "officer_title": {
                      "type": [
                        "string",
                        "null"
                      ],
                      "description": "Officer title, for example Chief Executive Officer. Null when is_officer is false."
                    },
                    "other_text": {
                      "type": [
                        "string",
                        "null"
                      ],
                      "description": "Description of the other relationship, when is_other is true."
                    },
                    "security_title": {
                      "type": [
                        "string",
                        "null"
                      ],
                      "description": "Security name, for example Common Stock or Class A Common Stock."
                    },
                    "transaction_date": {
                      "type": [
                        "string",
                        "null"
                      ],
                      "format": "date-time",
                      "description": "ISO 8601 datetime of the transaction."
                    },
                    "transaction_code": {
                      "type": [
                        "string",
                        "null"
                      ],
                      "description": "SEC Section 16 transaction code, one of P, S, A, D, F, M, G, V, J, L, C, E, H, O, X, I, U, W, Z or K."
                    },
                    "acquired_or_disposed": {
                      "type": [
                        "string",
                        "null"
                      ],
                      "description": "A for acquired or D for disposed."
                    },
                    "shares_amount": {
                      "type": [
                        "number",
                        "null"
                      ],
                      "description": "Number of shares transacted."
                    },
                    "price_per_share": {
                      "type": [
                        "number",
                        "null"
                      ],
                      "description": "USD price per share. May be null when not applicable, for example gifts."
                    },
                    "shares_owned_after": {
                      "type": [
                        "number",
                        "null"
                      ],
                      "description": "Total shares owned by this insider after the transaction."
                    },
                    "total_value": {
                      "type": [
                        "number",
                        "null"
                      ],
                      "description": "Computed shares times price. Null when price is unavailable or implausible."
                    },
                    "footnote_ids": {
                      "type": "array",
                      "description": "Identifiers of all footnotes that apply to this row, for example [\"F3\"]. Empty when the row has none.",
                      "items": {
                        "type": "string",
                        "description": "Footnote identifier."
                      }
                    },
                    "footnote_refs": {
                      "type": "object",
                      "additionalProperties": true,
                      "description": "Maps a field name to the footnote ids that annotate that specific field, for example {\"security_title\": [\"F3\"]}."
                    }
                  },
                  "additionalProperties": true
                }
              },
              "derivative": {
                "type": "array",
                "description": "Derivative transactions such as stock options, RSUs and warrants.",
                "items": {
                  "type": "object",
                  "description": "One derivative transaction row.",
                  "properties": {
                    "reporting_owner_cik": {
                      "type": [
                        "string",
                        "null"
                      ],
                      "description": "SEC CIK of the insider."
                    },
                    "reporting_owner_name": {
                      "type": [
                        "string",
                        "null"
                      ],
                      "description": "Full name of the insider."
                    },
                    "security_title": {
                      "type": [
                        "string",
                        "null"
                      ],
                      "description": "Derivative security name, for example Non-Qualified Stock Option."
                    },
                    "conversion_or_exercise_price": {
                      "type": [
                        "number",
                        "null"
                      ],
                      "description": "Exercise or conversion price in USD."
                    },
                    "transaction_date": {
                      "type": [
                        "string",
                        "null"
                      ],
                      "format": "date-time",
                      "description": "ISO 8601 datetime of the transaction."
                    },
                    "transaction_code": {
                      "type": [
                        "string",
                        "null"
                      ],
                      "description": "SEC Section 16 transaction code."
                    },
                    "acquired_or_disposed": {
                      "type": [
                        "string",
                        "null"
                      ],
                      "description": "A for acquired or D for disposed."
                    },
                    "shares_amount": {
                      "type": [
                        "number",
                        "null"
                      ],
                      "description": "Number of derivative units transacted."
                    },
                    "price_per_share": {
                      "type": [
                        "number",
                        "null"
                      ],
                      "description": "USD price per derivative unit."
                    },
                    "shares_owned_after": {
                      "type": [
                        "number",
                        "null"
                      ],
                      "description": "Total derivative units held after the transaction."
                    },
                    "underlying_security_title": {
                      "type": [
                        "string",
                        "null"
                      ],
                      "description": "Underlying security, for example Common Stock."
                    },
                    "underlying_shares": {
                      "type": [
                        "number",
                        "null"
                      ],
                      "description": "Number of underlying shares the derivative converts to."
                    },
                    "exercise_date": {
                      "type": [
                        "string",
                        "null"
                      ],
                      "format": "date-time",
                      "description": "Date the derivative becomes exercisable."
                    },
                    "expiration_date": {
                      "type": [
                        "string",
                        "null"
                      ],
                      "format": "date-time",
                      "description": "Expiration date of the derivative."
                    },
                    "footnote_ids": {
                      "type": "array",
                      "description": "Identifiers of all footnotes that apply to this row. Empty when the row has none.",
                      "items": {
                        "type": "string",
                        "description": "Footnote identifier."
                      }
                    },
                    "footnote_refs": {
                      "type": "object",
                      "additionalProperties": true,
                      "description": "Maps a field name to the footnote ids that annotate that specific field."
                    }
                  },
                  "additionalProperties": true
                }
              },
              "footnotes": {
                "type": "array",
                "description": "Footnotes referenced from transaction rows.",
                "items": {
                  "type": "object",
                  "description": "One footnote.",
                  "properties": {
                    "footnote_id": {
                      "type": [
                        "string",
                        "null"
                      ],
                      "description": "Identifier referenced from transaction rows, for example F1 or F2."
                    },
                    "text": {
                      "type": [
                        "string",
                        "null"
                      ],
                      "description": "Footnote text."
                    }
                  },
                  "additionalProperties": true
                }
              }
            },
            "additionalProperties": true
          }
        },
        "meta": {
          "type": "object",
          "description": "Paging metadata.",
          "properties": {
            "total": {
              "type": [
                "integer",
                "null"
              ],
              "description": "Total number of matching filings across all pages."
            },
            "page": {
              "type": "object",
              "description": "The paging you asked for, echoed back.",
              "properties": {
                "offset": {
                  "type": [
                    "integer",
                    "null"
                  ],
                  "description": "Zero-based offset of this page."
                },
                "limit": {
                  "type": [
                    "integer",
                    "null"
                  ],
                  "description": "Page size of this page."
                }
              },
              "additionalProperties": true
            }
          },
          "additionalProperties": true
        },
        "links": {
          "type": "object",
          "description": "Navigation links.",
          "properties": {
            "next": {
              "type": [
                "string",
                "null"
              ],
              "description": "Relative URL of the next page. Forward-only; there is no previous link."
            }
          },
          "additionalProperties": true
        }
      },
      "additionalProperties": true
    },
    "request_example": {
      "symbol": "AAPL",
      "page[offset]": 20,
      "page[limit]": 50,
      "fmt": "json"
    },
    "response_example": "{\n \"data\": [\n  {\n   \"accession_number\": \"0001104659-26-062860\",\n   \"filed_at\": \"2026-05-15\",\n   \"period_of_report\": \"2026-05-13\",\n   \"non_derivative\": [\n    {\n     \"reporting_owner_cik\": \"0001771340\",\n     \"reporting_owner_name\": \"Taneja Vaibhav\",\n     \"is_director\": false,\n     \"is_officer\": true,\n     \"is_ten_percent_owner\": false,\n     \"is_other\": false,\n     \"officer_title\": \"Chief Financial Officer\",\n     \"other_text\": null,\n     \"security_title\": \"Common Stock\",\n     \"transaction_date\": \"2026-05-13T00:00:00+00:00\",\n     \"transaction_code\": \"S\",\n     \"acquired_or_disposed\": \"D\",\n     \"shares_amount\": 3000,\n     \"price_per_share\": 450,\n     \"shares_owned_after\": 18106.5,\n     \"total_value\": 1350000,\n     \"footnote_ids\": [\n      \"F1\"\n     ],\n     \"footnote_refs\": {\n      \"shares_owned_after\": [\n       \"F1\"\n      ]\n     }\n    }\n   ],\n   \"derivative\": [],\n   \"footnotes\": [\n    {\n     \"footnote_id\": \"F1\",\n     \"text\": \"Transaction pursuant to a Rule 10b5-1 trading plan.\"\n    }\n   ]\n  }\n ],\n \"meta\": {\n  \"total\": 594,\n  \"page\": {\n   \"offset\": 0,\n   \"limit\": 20\n  }\n },\n \"links\": {\n  \"next\": \"/api/sec-filings/AAPL.US/form4?page%5Boffset%5D=20&page%5Blimit%5D=20\"\n }\n}",
    "tags": [
      "corporate events",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "SEC Form 4 insider filings for one US issuer, with non-derivative and derivative rows and footnotes."
    ],
    "input_entities": [
      "symbol",
      "page[offset]",
      "page[limit]",
      "fmt"
    ],
    "output_entities": [
      "corporate events"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "SEC Form 4 insider filings for one US issuer, with non-derivative and derivative rows and footnotes.",
    "external_docs_url": "https://eodhd.com/financial-apis/insider-transactions-api",
    "documentation_markdown": "GET /api/sec-filings/{symbol}/form4 — SEC Form 4 insider filings for one US issuer, with non-derivative and derivative rows and footnotes.\n\nReturns Form 4 filings straight from SEC EDGAR, paginated newest first by filing date. Each filing carries the reporting owner with CIK and relationship flags, both non-derivative and derivative transaction rows with the full SEC Section 16 code set, post-transaction holdings, and resolvable footnotes. This is the recommended endpoint for tracking insider buying and selling and it supersedes the flat legacy feed. Coverage is more than 1.47 million filings across 4,800 plus US issuers from 2000 onward, refreshed daily.\n\nEODHD call weight: 10; required plans: all_in_one, fundamentals_data_feed.\n\nThe task brief listed this as /api/insider-transactions; the documentation now puts the recommended endpoint at /api/sec-filings/{symbol}/form4 and marks /api/insider-transactions obsolete. Paging parameters use bracket syntax and must be sent as arrays, so page[offset] and page[limit]; passing page as a scalar returns 422, and page[limit] above 100 or a negative offset also returns 422. There are no date filters at all, only forward-only pagination through links.next, so reaching a specific page means incrementing page[offset] yourself using meta.total. Non-US issuers return 404. Expect quiet periods of 4 to 6 weeks before earnings because of insider blackout windows. Costs 10 API calls per request even for a 20-row page.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "insider_transactions_form4"
  },
  {
    "id": "c69c538f-efb1-5dd8-a63e-6783e017477a",
    "method": "GET",
    "path": "/api/insider-transactions",
    "price": 0.017391,
    "description": "Obsolete flat-schema insider transactions feed, optionally filtered by ticker and date.",
    "query_params": {
      "type": "object",
      "properties": {
        "code": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Ticker, for example AAPL or AAPL.US. By default all symbols are returned."
        },
        "from": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Start date, YYYY-MM-DD. Default: one year ago."
        },
        "to": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "End date, YYYY-MM-DD. Default: current date."
        },
        "limit": {
          "type": "integer",
          "default": 100,
          "required": false,
          "x-parameter-location": "query",
          "description": "Number of entries per result, 1 to 1000."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The documentation states that json is supported."
        }
      },
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.insider_transactions_legacy",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.02,
      "x-apiosk-plans": [
        "all_in_one",
        "fundamentals_data_feed"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "array",
      "description": "Flat list of insider transactions. The documentation for this endpoint states only that it returns the same flat schema as before and shows no response example; the field list below is the InsiderTransactions schema documented in the Fundamental Data API, which is the same flat schema.",
      "items": {
        "type": "object",
        "description": "One insider transaction row.",
        "properties": {
          "date": {
            "type": [
              "string",
              "null"
            ],
            "format": "date",
            "description": "Report date of the transaction."
          },
          "ownerCik": {
            "type": [
              "string",
              "null"
            ],
            "description": "Central Index Key of the owner."
          },
          "ownerName": {
            "type": [
              "string",
              "null"
            ],
            "description": "Name of the owner."
          },
          "transactionDate": {
            "type": [
              "string",
              "null"
            ],
            "format": "date",
            "description": "Date of the transaction."
          },
          "transactionCode": {
            "type": [
              "string",
              "null"
            ],
            "description": "The transaction code. This endpoint exposes only P for a purchase and S for a sale."
          },
          "transactionAmount": {
            "type": [
              "number",
              "null"
            ],
            "description": "Number of shares in the transaction."
          },
          "transactionPrice": {
            "type": [
              "number",
              "null"
            ],
            "description": "Price per share in the transaction."
          },
          "transactionAcquiredDisposed": {
            "type": [
              "string",
              "null"
            ],
            "description": "Whether shares were acquired or disposed."
          },
          "postTransactionAmount": {
            "type": [
              "number",
              "null"
            ],
            "description": "Shares held after the transaction."
          },
          "secLink": {
            "type": [
              "string",
              "null"
            ],
            "description": "Link to the SEC filing."
          }
        },
        "additionalProperties": true
      }
    },
    "request_example": {
      "code": "AAPL.US",
      "from": "2025-01-01",
      "to": "2025-12-31",
      "limit": 100,
      "fmt": "json"
    },
    "response_example": null,
    "tags": [
      "corporate events",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Obsolete flat-schema insider transactions feed, optionally filtered by ticker and date."
    ],
    "input_entities": [
      "code",
      "from",
      "to",
      "limit",
      "fmt"
    ],
    "output_entities": [
      "corporate events"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "Obsolete flat-schema insider transactions feed, optionally filtered by ticker and date.",
    "external_docs_url": "https://eodhd.com/financial-apis/insider-transactions-api",
    "documentation_markdown": "GET /api/insider-transactions — Obsolete flat-schema insider transactions feed, optionally filtered by ticker and date.\n\nThe original insider transactions endpoint, kept only for backward compatibility with existing integrations. It returns a flat list of transactions and, unlike the Form 4 endpoint, it can be queried across all symbols at once and accepts from, to and limit. It exposes only two transaction codes, P for a purchase and S for a sale, so option exercises, grants and gifts are not distinguishable here. New integrations should use the SEC Form 4 endpoint instead.\n\nEODHD call weight: 10; required plans: all_in_one, fundamentals_data_feed.\n\nExplicitly marked obsolete in the documentation. The page shows no example response and no explicit response field table for this endpoint, so the schema is taken from the identically shaped InsiderTransactions section of the Fundamental Data API. A ticker identifier field on each row is not documented anywhere, so none is modelled even though the endpoint can return all symbols at once. Only P and S codes appear here, against the 20 codes the Form 4 endpoint exposes.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "insider_transactions_legacy"
  },
  {
    "id": "d714da0c-ffdb-5107-a7e0-b176025c5586",
    "method": "GET",
    "path": "/api/calendar/earnings",
    "price": 0.026087,
    "description": "Reported and upcoming earnings dates with actual EPS, consensus estimate and the surprise.",
    "query_params": {
      "type": "object",
      "properties": {
        "from": {
          "type": "string",
          "default": "today",
          "required": false,
          "x-parameter-location": "query",
          "description": "Start of the window, YYYY-MM-DD, matched against the report date. An unparseable value returns HTTP 422."
        },
        "to": {
          "type": "string",
          "default": "from + 7 days",
          "required": false,
          "x-parameter-location": "query",
          "description": "End of the window, YYYY-MM-DD. Pass from alone and to becomes from plus 7 days; pass to alone and from becomes today."
        },
        "symbols": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "One or more tickers in EODHD format, comma-separated, for example AAPL.US,MSFT.US,AI.PA. This filters the date window, it does not replace it, so combine it with from and to."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "csv",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. Omitting the parameter returns CSV, whose column names differ from the JSON field names: Code, Report_Date, BeforeAfterMarket."
        }
      },
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.calendar_earnings",
      "x-apiosk-call-weight": 1,
      "x-apiosk-provider-cost-eur": 5.0e-05,
      "x-apiosk-target-buyer-price-eur": 0.03,
      "x-apiosk-plans": [
        "all_in_one",
        "fundamentals_data_feed",
        "corporate_events_calendar_and_news_feed"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "Envelope around the earnings list.",
      "properties": {
        "type": {
          "type": [
            "string",
            "null"
          ],
          "description": "Constant label of the feed, Earnings."
        },
        "description": {
          "type": [
            "string",
            "null"
          ],
          "description": "Human-readable description of the feed."
        },
        "from": {
          "type": [
            "string",
            "null"
          ],
          "format": "date",
          "description": "Start of the window, echoed back. Absent when symbols was passed."
        },
        "to": {
          "type": [
            "string",
            "null"
          ],
          "format": "date",
          "description": "End of the window, echoed back. Absent when symbols was passed."
        },
        "symbols": {
          "type": [
            "string",
            "null"
          ],
          "description": "The symbols you asked for, echoed back instead of from and to when symbols was passed."
        },
        "earnings": {
          "type": "array",
          "description": "One record per company and fiscal period.",
          "items": {
            "type": "object",
            "description": "A single earnings record.",
            "properties": {
              "code": {
                "type": [
                  "string",
                  "null"
                ],
                "description": "Ticker in EODHD format."
              },
              "report_date": {
                "type": [
                  "string",
                  "null"
                ],
                "format": "date",
                "description": "Day the results are announced. This is the date the window filters on."
              },
              "date": {
                "type": [
                  "string",
                  "null"
                ],
                "format": "date",
                "description": "End of the fiscal period the figures belong to."
              },
              "before_after_market": {
                "type": [
                  "string",
                  "null"
                ],
                "description": "BeforeMarket or AfterMarket, null when the timing is unknown."
              },
              "currency": {
                "type": [
                  "string",
                  "null"
                ],
                "description": "Reporting currency of the EPS figures."
              },
              "actual": {
                "type": [
                  "number",
                  "null"
                ],
                "description": "Reported EPS. Null for a report that has not happened yet."
              },
              "estimate": {
                "type": [
                  "number",
                  "null"
                ],
                "description": "Consensus EPS estimate, where analysts cover the name."
              },
              "difference": {
                "type": [
                  "number",
                  "null"
                ],
                "description": "Actual minus estimate, but 0 rather than null whenever the estimate is missing."
              },
              "percent": {
                "type": [
                  "number",
                  "null"
                ],
                "description": "Surprise in percent against the estimate."
              }
            },
            "additionalProperties": true
          }
        }
      },
      "additionalProperties": true
    },
    "request_example": {
      "from": "2025-01-01",
      "to": "2025-12-31",
      "symbols": "AAPL.US",
      "fmt": "json"
    },
    "response_example": "{\n \"type\": \"Earnings\",\n \"description\": \"Historical and upcoming Earnings\",\n \"symbols\": \"AAPL.US\",\n \"earnings\": [\n  {\n   \"code\": \"AAPL.US\",\n   \"report_date\": \"2025-01-30\",\n   \"date\": \"2024-12-31\",\n   \"before_after_market\": \"AfterMarket\",\n   \"currency\": \"USD\",\n   \"actual\": 2.4,\n   \"estimate\": 2.34,\n   \"difference\": 0.06,\n   \"percent\": 2.5641\n  },\n  {\n   \"code\": \"AAPL.US\",\n   \"report_date\": \"2025-05-01\",\n   \"date\": \"2025-03-31\",\n   \"before_after_market\": \"AfterMarket\",\n   \"currency\": \"USD\",\n   \"actual\": 1.65,\n   \"estimate\": 1.62,\n   \"difference\": 0.03,\n   \"percent\": 1.8519\n  }\n ]\n}",
    "tags": [
      "calendar",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Reported and upcoming earnings dates with actual EPS, consensus estimate and the surprise."
    ],
    "input_entities": [
      "from",
      "to",
      "symbols",
      "fmt"
    ],
    "output_entities": [
      "calendar"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "Reported and upcoming earnings dates with actual EPS, consensus estimate and the surprise.",
    "external_docs_url": "https://eodhd.com/financial-apis/calendar-upcoming-earnings-ipos-and-splits",
    "documentation_markdown": "GET /api/calendar/earnings — Reported and upcoming earnings dates with actual EPS, consensus estimate and the surprise.\n\nReturns one record per company and fiscal period inside a date window matched on the report date, with the actual EPS, the consensus estimate and the surprise already computed. History goes back to the 1990s and the forward book already held 26,414 scheduled reports between today and the end of 2027 when the documentation was written. Pass symbols to narrow the window to specific tickers. Nothing is paginated and this is the largest of the five calendar datasets, so keep windows short.\n\nEODHD call weight: 1; required plans: all_in_one, fundamentals_data_feed, corporate_events_calendar_and_news_feed.\n\ndifference is 0 and not null when the estimate is missing, so test estimate rather than difference before trusting a surprise of zero: in one default seven-day window all 1,636 records without an estimate carried difference 0 and percent null. More than ten years between from and to returns HTTP 422 with 'Maximum date range is 10 years', but the practical ceiling is far lower: one year is about 120,000 records and 21 MB, a six-year window came back as 158 MB after 45 seconds, and beyond roughly five years the request often fails with HTTP 500 and a bare 'Error occurred'. A window whose end precedes its start returns HTTP 200 with an empty list, while an unknown symbol returns HTTP 404 with a misleading body about a missing type. Default fmt is csv, not json, and the CSV column names differ from the JSON keys.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "calendar_earnings"
  },
  {
    "id": "0150981d-d431-5b35-b8c1-ef9dd8bf4b88",
    "method": "GET",
    "path": "/api/calendar/trends",
    "price": 0.043478,
    "description": "Consensus EPS and revenue estimates per fiscal period, with the revision history behind them.",
    "query_params": {
      "type": "object",
      "properties": {
        "symbols": {
          "type": "string",
          "required": true,
          "x-parameter-location": "query",
          "description": "One or more tickers in EODHD format, comma-separated. Required here: omitting it returns HTTP 422."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "csv",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. CSV is impractical here because it writes one line per symbol with the whole history on it, an 81 KB row for a single large-cap name."
        }
      },
      "required": [
        "symbols"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.calendar_earnings_trends",
      "x-apiosk-call-weight": 1,
      "x-apiosk-provider-cost-eur": 5.0e-05,
      "x-apiosk-target-buyer-price-eur": 0.05,
      "x-apiosk-plans": [
        "all_in_one",
        "fundamentals_data_feed",
        "corporate_events_calendar_and_news_feed"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "Envelope around the trends data.",
      "properties": {
        "type": {
          "type": [
            "string",
            "null"
          ],
          "description": "Constant label of the feed, Trends."
        },
        "description": {
          "type": [
            "string",
            "null"
          ],
          "description": "Human-readable description of the feed."
        },
        "symbols": {
          "type": [
            "string",
            "null"
          ],
          "description": "The symbols you asked for, echoed back."
        },
        "trends": {
          "type": "array",
          "description": "Outer array, one inner array per requested ticker in the order you listed them. A symbol with no analyst coverage keeps its slot as an empty inner array.",
          "items": {
            "type": "array",
            "description": "History of estimate records for one ticker.",
            "items": {
              "type": "object",
              "description": "One estimate record for one fiscal period and horizon. Every numeric value arrives as a string padded to four decimals, or null.",
              "properties": {
                "code": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "Ticker in EODHD format."
                },
                "date": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "format": "date",
                  "description": "Fiscal period end the estimate refers to: quarter end for a quarterly record, year end for an annual one."
                },
                "period": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "Which horizon this record was: 0q current quarter, +1q next quarter, 0y current fiscal year, +1y next fiscal year."
                },
                "growth": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "Growth against the comparable prior period, as a ratio rather than a percentage."
                },
                "earningsEstimateAvg": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "Consensus EPS."
                },
                "earningsEstimateLow": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "Lowest individual EPS estimate."
                },
                "earningsEstimateHigh": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "Highest individual EPS estimate."
                },
                "earningsEstimateYearAgoEps": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "EPS actually delivered in the comparable prior period."
                },
                "earningsEstimateNumberOfAnalysts": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "How many analysts stand behind the EPS consensus."
                },
                "earningsEstimateGrowth": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "EPS growth against the comparable prior period, as a ratio."
                },
                "revenueEstimateAvg": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "Consensus revenue in the reporting currency."
                },
                "revenueEstimateLow": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "Lowest individual revenue estimate."
                },
                "revenueEstimateHigh": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "Highest individual revenue estimate."
                },
                "revenueEstimateYearAgoEps": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "Revenue in the comparable prior period. Frequently null."
                },
                "revenueEstimateNumberOfAnalysts": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "Analyst count behind the revenue consensus."
                },
                "revenueEstimateGrowth": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "Revenue growth against the comparable prior period, as a ratio."
                },
                "epsTrendCurrent": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "The EPS consensus as it stands now."
                },
                "epsTrend7daysAgo": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "The same EPS consensus as it stood 7 days ago."
                },
                "epsTrend30daysAgo": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "The same EPS consensus as it stood 30 days ago."
                },
                "epsTrend60daysAgo": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "The same EPS consensus as it stood 60 days ago."
                },
                "epsTrend90daysAgo": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "The same EPS consensus as it stood 90 days ago."
                },
                "epsRevisionsUpLast7days": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "Number of analysts who moved their EPS estimate up in the last 7 days."
                },
                "epsRevisionsUpLast30days": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "Number of analysts who moved their EPS estimate up in the last 30 days."
                },
                "epsRevisionsDownLast30days": {
                  "type": [
                    "string",
                    "null"
                  ],
                  "description": "Number of analysts who moved their EPS estimate down in the last 30 days."
                }
              },
              "additionalProperties": true
            }
          }
        }
      },
      "additionalProperties": true
    },
    "request_example": {
      "symbols": "AAPL.US",
      "fmt": "json"
    },
    "response_example": "{\n \"type\": \"Trends\",\n \"description\": \"Historical and upcoming earning trends\",\n \"symbols\": \"AAPL.US\",\n \"trends\": [\n  [\n   {\n    \"code\": \"AAPL.US\",\n    \"date\": \"2027-09-30\",\n    \"period\": \"+1y\",\n    \"growth\": \"0.0804\",\n    \"earningsEstimateAvg\": \"9.5347\",\n    \"earningsEstimateLow\": \"8.2400\",\n    \"earningsEstimateHigh\": \"10.6700\",\n    \"earningsEstimateYearAgoEps\": \"8.8027\",\n    \"earningsEstimateNumberOfAnalysts\": \"40.0000\",\n    \"earningsEstimateGrowth\": \"0.0832\",\n    \"revenueEstimateAvg\": \"523204888660.00\",\n    \"revenueEstimateYearAgoEps\": null,\n    \"revenueEstimateNumberOfAnalysts\": \"40.00\",\n    \"epsTrendCurrent\": \"9.5347\",\n    \"epsTrend30daysAgo\": \"9.6928\",\n    \"epsRevisionsUpLast30days\": \"8.0000\",\n    \"epsRevisionsDownLast30days\": \"19.0000\"\n   }\n  ]\n ]\n}",
    "tags": [
      "calendar",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Consensus EPS and revenue estimates per fiscal period, with the revision history behind them."
    ],
    "input_entities": [
      "symbols",
      "fmt"
    ],
    "output_entities": [
      "calendar"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "Consensus EPS and revenue estimates per fiscal period, with the revision history behind them.",
    "external_docs_url": "https://eodhd.com/financial-apis/calendar-upcoming-earnings-ipos-and-splits",
    "documentation_markdown": "GET /api/calendar/trends — Consensus EPS and revenue estimates per fiscal period, with the revision history behind them.\n\nReturns analyst estimates rather than results: EPS and revenue consensus with high, low and analyst count, the same EPS consensus as it stood 7, 30, 60 and 90 days ago, and the count of upward and downward revisions. This is the endpoint for detecting whether the street is walking a number up or down before a report lands. The payload nests one level deeper than the other calendar endpoints: trends is an array of arrays, the n-th inner array holding the records for the n-th ticker in the symbols list. Each inner array is a history rather than a snapshot, about 96 records for a large US name.\n\nEODHD call weight: 1; required plans: all_in_one, fundamentals_data_feed, corporate_events_calendar_and_news_feed.\n\nThe brief called this 'earnings trends'; the real path is /api/calendar/trends. symbols is required, unlike every other calendar endpoint. There are no date parameters: passing from and to is accepted and the response is byte-for-byte the same. Every numeric value is a quoted string padded to four decimals, and null appears where a figure is genuinely missing, so a parser must handle both a quoted number and null in the same field. Watch the size: Apple alone is 71 KB of JSON and two large caps 143 KB. Empty inner arrays keep positions aligned with the symbols list.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "calendar_earnings_trends"
  },
  {
    "id": "000b1155-8083-5506-a545-f8fd4863c512",
    "method": "GET",
    "path": "/api/calendar/ipos",
    "price": 0.026087,
    "description": "IPO calendar covering filings, expected listings, amendments and priced deals.",
    "query_params": {
      "type": "object",
      "properties": {
        "from": {
          "type": "string",
          "default": "today",
          "required": false,
          "x-parameter-location": "query",
          "description": "Start of the window, YYYY-MM-DD, matched against the expected or effective first trading date."
        },
        "to": {
          "type": "string",
          "default": "from + 7 days",
          "required": false,
          "x-parameter-location": "query",
          "description": "End of the window, YYYY-MM-DD."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "csv",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. JSON carries every field."
        }
      },
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.calendar_ipos",
      "x-apiosk-call-weight": 1,
      "x-apiosk-provider-cost-eur": 5.0e-05,
      "x-apiosk-target-buyer-price-eur": 0.03,
      "x-apiosk-plans": [
        "all_in_one",
        "fundamentals_data_feed",
        "corporate_events_calendar_and_news_feed"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "Envelope around the IPO list.",
      "properties": {
        "type": {
          "type": [
            "string",
            "null"
          ],
          "description": "Constant label of the feed, IPOs."
        },
        "description": {
          "type": [
            "string",
            "null"
          ],
          "description": "Human-readable description of the feed."
        },
        "from": {
          "type": [
            "string",
            "null"
          ],
          "format": "date",
          "description": "Start of the window, echoed back."
        },
        "to": {
          "type": [
            "string",
            "null"
          ],
          "format": "date",
          "description": "End of the window, echoed back."
        },
        "ipos": {
          "type": "array",
          "description": "One record per deal state.",
          "items": {
            "type": "object",
            "description": "A single IPO record.",
            "properties": {
              "code": {
                "type": [
                  "string",
                  "null"
                ],
                "description": "Ticker in EODHD format."
              },
              "name": {
                "type": [
                  "string",
                  "null"
                ],
                "description": "Company name as filed."
              },
              "exchange": {
                "type": [
                  "string",
                  "null"
                ],
                "description": "Listing venue, as a name rather than an EODHD exchange code."
              },
              "currency": {
                "type": [
                  "string",
                  "null"
                ],
                "description": "Offering currency. Can also be an empty string."
              },
              "start_date": {
                "type": [
                  "string",
                  "null"
                ],
                "format": "date",
                "description": "Expected or effective first trading date. This is the field the window filters on."
              },
              "filing_date": {
                "type": [
                  "string",
                  "null"
                ],
                "format": "date",
                "description": "Initial filing date, where the feed carries it."
              },
              "amended_date": {
                "type": [
                  "string",
                  "null"
                ],
                "format": "date",
                "description": "Most recent amended filing."
              },
              "price_from": {
                "type": [
                  "number",
                  "null"
                ],
                "description": "Lower bound of the indicated price range. 0, not null, before a range is set."
              },
              "price_to": {
                "type": [
                  "number",
                  "null"
                ],
                "description": "Upper bound of the indicated price range. 0, not null, before a range is set."
              },
              "offer_price": {
                "type": [
                  "number",
                  "null"
                ],
                "description": "Final price. 0 until the deal prices."
              },
              "shares": {
                "type": [
                  "number",
                  "null"
                ],
                "description": "Shares offered. 0 when unknown."
              },
              "deal_type": {
                "type": [
                  "string",
                  "null"
                ],
                "description": "Where the deal stands. Four values appear: Expected, Priced, Amended and Filed."
              }
            },
            "additionalProperties": true
          }
        }
      },
      "additionalProperties": true
    },
    "request_example": {
      "from": "2025-01-01",
      "to": "2025-01-31",
      "fmt": "json"
    },
    "response_example": "{\n \"type\": \"IPOs\",\n \"description\": \"Historical and upcoming IPOs\",\n \"from\": \"2025-01-01\",\n \"to\": \"2025-01-31\",\n \"ipos\": [\n  {\n   \"code\": \"MCW.US\",\n   \"name\": \"Mister Car Wash, Inc. Common Stock\",\n   \"exchange\": \"Nasdaq\",\n   \"currency\": \"USD\",\n   \"start_date\": \"2025-01-02\",\n   \"filing_date\": null,\n   \"amended_date\": null,\n   \"price_from\": 15,\n   \"price_to\": 17,\n   \"offer_price\": 15,\n   \"shares\": 0,\n   \"deal_type\": \"Expected\"\n  }\n ]\n}",
    "tags": [
      "calendar",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "IPO calendar covering filings, expected listings, amendments and priced deals."
    ],
    "input_entities": [
      "from",
      "to",
      "fmt"
    ],
    "output_entities": [
      "calendar"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "IPO calendar covering filings, expected listings, amendments and priced deals.",
    "external_docs_url": "https://eodhd.com/financial-apis/calendar-upcoming-earnings-ipos-and-splits",
    "documentation_markdown": "GET /api/calendar/ipos — IPO calendar covering filings, expected listings, amendments and priced deals.\n\nReturns new listings inside a date window matched on the expected or effective first trading date, from initial filing through pricing. The same listing reappears as it moves through the process, so a code can occur several times in one window with different deal_type values. Coverage is continuous from February 2013 plus an isolated batch of 84 deals in early 2010, and forward visibility is short at roughly three weeks. Records are not sorted by date.\n\nEODHD call weight: 1; required plans: all_in_one, fundamentals_data_feed, corporate_events_calendar_and_news_feed.\n\nThis endpoint takes no symbols parameter: passing one is accepted and silently ignored, so you get the date window back rather than the company you asked for. Filter by code on your side. Treat 0 in price_from, price_to, offer_price and shares as 'not known yet' rather than as a real value. Re-read windows you have already stored, because filings get amended and both date and price range can change before the first trade: over the first half of 2025, 93 codes turned up more than once. Records are not sorted, so sort on your side.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "calendar_ipos"
  },
  {
    "id": "463d28e5-554a-5762-8a49-8fe07e3755b1",
    "method": "GET",
    "path": "/api/calendar/splits",
    "price": 0.026087,
    "description": "Split and reverse-split calendar by effective date, expressed as an old-to-new share ratio.",
    "query_params": {
      "type": "object",
      "properties": {
        "from": {
          "type": "string",
          "default": "today",
          "required": false,
          "x-parameter-location": "query",
          "description": "Start of the window, YYYY-MM-DD, matched against the effective split date."
        },
        "to": {
          "type": "string",
          "default": "from + 7 days",
          "required": false,
          "x-parameter-location": "query",
          "description": "End of the window, YYYY-MM-DD."
        },
        "symbols": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "One or more tickers in EODHD format, comma-separated. Narrows the window rather than replacing it, so a symbols-only request returns the next seven days for that ticker, which is normally nothing."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "csv",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format."
        }
      },
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.calendar_splits",
      "x-apiosk-call-weight": 1,
      "x-apiosk-provider-cost-eur": 5.0e-05,
      "x-apiosk-target-buyer-price-eur": 0.03,
      "x-apiosk-plans": [
        "all_in_one",
        "fundamentals_data_feed",
        "corporate_events_calendar_and_news_feed"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "Envelope around the split list.",
      "properties": {
        "type": {
          "type": [
            "string",
            "null"
          ],
          "description": "Constant label of the feed, Splits."
        },
        "description": {
          "type": [
            "string",
            "null"
          ],
          "description": "Human-readable description of the feed."
        },
        "from": {
          "type": [
            "string",
            "null"
          ],
          "format": "date",
          "description": "Start of the window, echoed back."
        },
        "to": {
          "type": [
            "string",
            "null"
          ],
          "format": "date",
          "description": "End of the window, echoed back."
        },
        "symbols": {
          "type": [
            "string",
            "null"
          ],
          "description": "The symbols you asked for, echoed back when you passed any."
        },
        "splits": {
          "type": "array",
          "description": "One record per split event.",
          "items": {
            "type": "object",
            "description": "A single split record.",
            "properties": {
              "code": {
                "type": [
                  "string",
                  "null"
                ],
                "description": "Ticker in EODHD format."
              },
              "split_date": {
                "type": [
                  "string",
                  "null"
                ],
                "format": "date",
                "description": "Effective date of the split. This is the date the window filters on."
              },
              "optionable": {
                "type": [
                  "string",
                  "null"
                ],
                "description": "Y or N, whether options trade on the name."
              },
              "old_shares": {
                "type": [
                  "number",
                  "null"
                ],
                "description": "Shares held before the split."
              },
              "new_shares": {
                "type": [
                  "number",
                  "null"
                ],
                "description": "Shares held after it."
              }
            },
            "additionalProperties": true
          }
        }
      },
      "additionalProperties": true
    },
    "request_example": {
      "from": "2010-01-01",
      "to": "2030-01-01",
      "symbols": "TSLA.US",
      "fmt": "json"
    },
    "response_example": "{\n \"type\": \"Splits\",\n \"description\": \"Historical and upcoming splits\",\n \"from\": \"2010-01-01\",\n \"to\": \"2030-01-01\",\n \"symbols\": \"TSLA.US\",\n \"splits\": [\n  {\n   \"code\": \"TSLA.US\",\n   \"split_date\": \"2020-08-31\",\n   \"optionable\": \"N\",\n   \"old_shares\": 1,\n   \"new_shares\": 5\n  },\n  {\n   \"code\": \"TSLA.US\",\n   \"split_date\": \"2022-08-25\",\n   \"optionable\": \"N\",\n   \"old_shares\": 1,\n   \"new_shares\": 3\n  }\n ]\n}",
    "tags": [
      "calendar",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Split and reverse-split calendar by effective date, expressed as an old-to-new share ratio."
    ],
    "input_entities": [
      "from",
      "to",
      "symbols",
      "fmt"
    ],
    "output_entities": [
      "calendar"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "Split and reverse-split calendar by effective date, expressed as an old-to-new share ratio.",
    "external_docs_url": "https://eodhd.com/financial-apis/calendar-upcoming-earnings-ipos-and-splits",
    "documentation_markdown": "GET /api/calendar/splits — Split and reverse-split calendar by effective date, expressed as an old-to-new share ratio.\n\nReturns splits and reverse splits inside a date window matched on the effective split date, as a share count before and after. Announced splits run well ahead of today: in August 2026 the calendar already held effective dates into February 2027. History is continuous from February 2013 plus 134 records in the first quarter of 2010. Pass symbols to narrow the window to specific tickers.\n\nEODHD call weight: 1; required plans: all_in_one, fundamentals_data_feed, corporate_events_calendar_and_news_feed.\n\nRead old_shares and new_shares as a ratio in the direction old to new: Tesla in 2020 is 1 to 5, a five-for-one split, while a reverse split arrives the other way round, such as 10 to 1 on the Korean listing 090150.KQ. Reverse splits are common, 36 of 97 events in one August 2026 week, so branch on which number is larger instead of assuming a forward split. Ratios are not always tidy, 1000 to 729 turns up on Taiwanese names, and a record can carry new_shares 0, which will divide by zero if you let it. The ten-year range ceiling and the CSV default apply here too.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "calendar_splits"
  },
  {
    "id": "3bce9a14-0814-5a3d-a5ce-8aaf596314c4",
    "method": "GET",
    "path": "/api/calendar/dividends",
    "price": 0.017391,
    "description": "Dividend dates for one symbol or for a single calendar day, paginated and deliberately thin.",
    "query_params": {
      "type": "object",
      "properties": {
        "filter[symbol]": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "A single ticker in EODHD format. Either this or filter[date_eq] must be present, otherwise the request returns HTTP 422."
        },
        "filter[date_eq]": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "One exact dividend date, YYYY-MM-DD, returning every symbol going ex-dividend that day. Required when filter[symbol] is absent."
        },
        "filter[date_from]": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Lower bound, inclusive. Only meaningful alongside filter[symbol]."
        },
        "filter[date_to]": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Upper bound, inclusive. Only meaningful alongside filter[symbol]."
        },
        "page[limit]": {
          "type": "integer",
          "default": 1000,
          "required": false,
          "x-parameter-location": "query",
          "description": "Records per page. Must be at least 1; values above the default are accepted rather than clamped, so 1001 comes back as 1001."
        },
        "page[offset]": {
          "type": "integer",
          "default": 0,
          "required": false,
          "x-parameter-location": "query",
          "description": "How many records to skip. Follow links.next instead of computing it yourself."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Ignored on this endpoint: it always answers in JSON, including when you ask for CSV."
        }
      },
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.calendar_dividends",
      "x-apiosk-call-weight": 1,
      "x-apiosk-provider-cost-eur": 5.0e-05,
      "x-apiosk-target-buyer-price-eur": 0.02,
      "x-apiosk-plans": [
        "all_in_one",
        "fundamentals_data_feed",
        "corporate_events_calendar_and_news_feed"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "Paginated envelope around the dividend date list.",
      "properties": {
        "meta": {
          "type": "object",
          "description": "Paging and echoed filters. Only the filters you actually sent appear as keys.",
          "properties": {
            "total": {
              "type": [
                "integer",
                "null"
              ],
              "description": "Matching records across all pages, not just this one."
            },
            "offset": {
              "type": [
                "integer",
                "null"
              ],
              "description": "The offset you asked for, echoed back."
            },
            "limit": {
              "type": [
                "integer",
                "null"
              ],
              "description": "The page size you asked for, echoed back."
            },
            "symbol": {
              "type": [
                "string",
                "null"
              ],
              "description": "The filter[symbol] you passed, echoed back."
            },
            "date_from": {
              "type": [
                "string",
                "null"
              ],
              "format": "date",
              "description": "The filter[date_from] you passed, echoed back."
            },
            "date_to": {
              "type": [
                "string",
                "null"
              ],
              "format": "date",
              "description": "The filter[date_to] you passed, echoed back."
            }
          },
          "additionalProperties": true
        },
        "data": {
          "type": "array",
          "description": "One record per dividend date, newest first.",
          "items": {
            "type": "object",
            "description": "A single dividend date record.",
            "properties": {
              "date": {
                "type": [
                  "string",
                  "null"
                ],
                "format": "date",
                "description": "Dividend date."
              },
              "symbol": {
                "type": [
                  "string",
                  "null"
                ],
                "description": "Ticker in EODHD format."
              }
            },
            "additionalProperties": true
          }
        },
        "links": {
          "type": "object",
          "description": "Navigation links.",
          "properties": {
            "next": {
              "type": [
                "string",
                "null"
              ],
              "description": "Ready-made URL for the next page, null on the last one."
            }
          },
          "additionalProperties": true
        }
      },
      "additionalProperties": true
    },
    "request_example": {
      "filter[symbol]": "AAPL.US",
      "filter[date_eq]": "2026-09-15",
      "filter[date_from]": "2025-01-01",
      "filter[date_to]": "2025-12-31",
      "page[limit]": 500,
      "page[offset]": 1000,
      "fmt": "json"
    },
    "response_example": "{\n \"meta\": {\n  \"total\": 4,\n  \"offset\": 0,\n  \"limit\": 1000,\n  \"symbol\": \"AAPL.US\",\n  \"date_from\": \"2025-01-01\",\n  \"date_to\": \"2025-12-31\"\n },\n \"data\": [\n  {\n   \"date\": \"2025-11-10\",\n   \"symbol\": \"AAPL.US\"\n  },\n  {\n   \"date\": \"2025-08-11\",\n   \"symbol\": \"AAPL.US\"\n  },\n  {\n   \"date\": \"2025-05-12\",\n   \"symbol\": \"AAPL.US\"\n  },\n  {\n   \"date\": \"2025-02-10\",\n   \"symbol\": \"AAPL.US\"\n  }\n ],\n \"links\": {\n  \"next\": null\n }\n}",
    "tags": [
      "calendar",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Dividend dates for one symbol or for a single calendar day, paginated and deliberately thin."
    ],
    "input_entities": [
      "filter[symbol]",
      "filter[date_eq]",
      "filter[date_from]",
      "filter[date_to]",
      "page[limit]",
      "page[offset]",
      "fmt"
    ],
    "output_entities": [
      "calendar"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "Dividend dates for one symbol or for a single calendar day, paginated and deliberately thin.",
    "external_docs_url": "https://eodhd.com/financial-apis/calendar-upcoming-earnings-ipos-and-splits",
    "documentation_markdown": "GET /api/calendar/dividends — Dividend dates for one symbol or for a single calendar day, paginated and deliberately thin.\n\nAnswers when a dividend falls rather than how much it is: each record is a date and a symbol, nothing else. It is built differently from its four siblings, with filters nested under filter and paging under page. History is deep and forward dates are covered too: Apple returns 92 dividend dates starting on 1987-05-11, and 347 symbols already carried a date on 2026-09-15. For amounts, currencies, declaration and payment dates use the Corporate Actions splits and dividends API instead.\n\nEODHD call weight: 1; required plans: all_in_one, fundamentals_data_feed, corporate_events_calendar_and_news_feed.\n\nThe odd one out of the five calendar endpoints: bracketed filter and page parameters, always JSON, paginated, and newest dates first rather than oldest. Either filter[symbol] or filter[date_eq] is mandatory or you get HTTP 422. A ticker that does not exist is not an error here: you get HTTP 200 with total 0, whereas the other four endpoints answer an unknown symbol with a 404 whose body mentions a missing type. The payload carries only a date and a symbol, no amount or currency.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "calendar_dividends"
  },
  {
    "id": "b1042d54-fe90-5591-bb36-adc22e585754",
    "method": "GET",
    "path": "/api/mp/investverte/esg/{symbol}",
    "price": 0.026087,
    "description": "AI-derived environmental, social, governance and total ESG scores for one company, by year and reporting frequency.",
    "query_params": {
      "type": "object",
      "properties": {
        "symbol": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "Company symbol as listed by the /companies endpoint, for example AAPL."
        },
        "year": {
          "type": "integer",
          "required": false,
          "x-parameter-location": "query",
          "description": "Year of the data."
        },
        "frequency": {
          "type": "string",
          "enum": [
            "FY",
            "Q1",
            "Q2",
            "Q3",
            "Q4"
          ],
          "required": false,
          "x-parameter-location": "query",
          "description": "Frequency of the data reporting."
        },
        "model": {
          "type": "string",
          "enum": [
            "ai",
            "legacy"
          ],
          "default": "ai",
          "required": false,
          "x-parameter-location": "query",
          "description": "Which scoring model to use. Endpoints return the AI model by default; pass legacy for the previous dataset."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The published OpenAPI specification for this Marketplace product declares JSON responses only and does not document an fmt parameter."
        }
      },
      "required": [
        "symbol"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.esg_company_scores",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.03,
      "x-apiosk-plans": [
        "marketplace_investverte_esg_data"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "array",
      "description": "ESG score records for the requested company.",
      "items": {
        "type": "object",
        "description": "One ESG score record for a given year and frequency.",
        "properties": {
          "e": {
            "type": [
              "number",
              "null"
            ],
            "description": "Environmental score."
          },
          "s": {
            "type": [
              "number",
              "null"
            ],
            "description": "Social score."
          },
          "g": {
            "type": [
              "number",
              "null"
            ],
            "description": "Governance score."
          },
          "esg": {
            "type": [
              "number",
              "null"
            ],
            "description": "Aggregate ESG score."
          },
          "year": {
            "type": [
              "number",
              "null"
            ],
            "description": "Year the record refers to."
          },
          "frequency": {
            "type": [
              "string",
              "null"
            ],
            "description": "Reporting frequency of the record: FY, Q1, Q2, Q3 or Q4."
          }
        },
        "additionalProperties": true
      }
    },
    "request_example": {
      "symbol": "AAPL",
      "year": 2014,
      "frequency": "FY",
      "model": "ai",
      "fmt": "json"
    },
    "response_example": "[\n {\n  \"e\": 62.61,\n  \"s\": 68.68,\n  \"g\": 64.65,\n  \"esg\": 65.45,\n  \"year\": 2014,\n  \"frequency\": \"FY\"\n },\n {\n  \"e\": 63.01,\n  \"s\": 68.36,\n  \"g\": 64.59,\n  \"esg\": 65.47,\n  \"year\": 2014,\n  \"frequency\": \"Q1\"\n }\n]",
    "tags": [
      "fundamentals",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "AI-derived environmental, social, governance and total ESG scores for one company, by year and reporting frequency."
    ],
    "input_entities": [
      "symbol",
      "year",
      "frequency",
      "model",
      "fmt"
    ],
    "output_entities": [
      "fundamentals"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "AI-derived environmental, social, governance and total ESG scores for one company, by year and reporting frequency.",
    "external_docs_url": "https://eodhd.com/marketplace/investverte/esg_data/docs",
    "documentation_markdown": "GET /api/mp/investverte/esg/{symbol} — AI-derived environmental, social, governance and total ESG scores for one company, by year and reporting frequency.\n\nReturns an array of ESG score records for a company, each with separate E, S and G scores plus an aggregate esg score, tagged with the year and the reporting frequency FY, Q1, Q2, Q3 or Q4. Scores come from an AI and deep learning framework that completes missing datapoints from peer patterns at country, sector and sub-sector level; the previous dataset is still reachable with model=legacy. Sibling paths under the same base cover countries (/country/{symbol}), sectors (/sector/{symbol}) and the available lists (/companies, /countries, /sectors). This is a Marketplace product from Investverte, not part of the core Fundamental Data API.\n\nEODHD call weight: 10; required plans: marketplace_investverte_esg_data.\n\nThe current Fundamental Data API documentation no longer lists an ESGScores section in the single-ticker response, so ESG now lives here as a separately priced Marketplace subscription rather than inside fundamentals. Priced at 29.99 per month at the time of writing. Limits are specific to this product: 100,000 API calls per 24 hours, 1,000 requests per minute, and one request costs 10 API calls; the 24-hour period is counted differently from the main EODHD plans. The symbol here is a plain company symbol such as AAPL, not the TICKER.EXCHANGE format used everywhere else in EODHD. No fmt parameter is documented; it is included above only to satisfy the uniform parameter contract.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "esg_company_scores"
  },
  {
    "id": "96300f44-4e51-59f3-ba6a-a5c81ba16b19",
    "method": "GET",
    "path": "/api/mp/unicornbay/options/contracts",
    "price": 0.043478,
    "description": "List US stock option contracts with quotes, Greeks and open interest, filtered by underlying, expiration, strike and type.",
    "query_params": {
      "type": "object",
      "properties": {
        "filter[contract]": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Filter options contracts by specific contract name or identifier."
        },
        "filter[underlying_symbol]": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Filter options contracts based on the underlying asset's symbol (for example 'AAPL' for Apple)."
        },
        "filter[exp_date_eq]": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Filter options contracts that expire on the exact given date (format YYYY-MM-DD)."
        },
        "filter[exp_date_from]": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Filter options contracts with expiration dates from the specified date onwards (format YYYY-MM-DD)."
        },
        "filter[exp_date_to]": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Filter options contracts with expiration dates up to the specified date (format YYYY-MM-DD)."
        },
        "filter[tradetime_eq]": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Filter options contracts with a trade time on the exact given date (format YYYY-MM-DD). Added in the March 2025 update."
        },
        "filter[tradetime_from]": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Filter options contracts with a trade time from the specified date onwards (format YYYY-MM-DD)."
        },
        "filter[tradetime_to]": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Filter options contracts with a trade time up to the specified date (format YYYY-MM-DD)."
        },
        "filter[type]": {
          "type": "string",
          "enum": [
            "put",
            "call"
          ],
          "required": false,
          "x-parameter-location": "query",
          "description": "Filter options contracts by type."
        },
        "filter[strike_eq]": {
          "type": "number",
          "required": false,
          "x-parameter-location": "query",
          "description": "Filter options contracts by an exact strike price."
        },
        "filter[strike_from]": {
          "type": "number",
          "required": false,
          "x-parameter-location": "query",
          "description": "Filter options contracts with strike prices from the specified value onwards."
        },
        "filter[strike_to]": {
          "type": "number",
          "required": false,
          "x-parameter-location": "query",
          "description": "Filter options contracts with strike prices up to the specified value."
        },
        "sort": {
          "type": "string",
          "enum": [
            "exp_date",
            "strike",
            "-exp_date",
            "-strike"
          ],
          "required": false,
          "x-parameter-location": "query",
          "description": "Sorting order of the results. A leading minus sign reverses the direction."
        },
        "page[offset]": {
          "type": "integer",
          "default": 0,
          "required": false,
          "x-parameter-location": "query",
          "description": "Offset for pagination, specifying the starting point of the results. Maximum is 10000."
        },
        "page[limit]": {
          "type": "integer",
          "default": 1000,
          "required": false,
          "x-parameter-location": "query",
          "description": "Maximum number of results to return per page. Default and maximum value is 1000."
        },
        "fields[options-contracts]": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Comma separated list of attribute names to include in the response, which shrinks the payload."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Output format. The Marketplace options specification does not document this parameter; the endpoint returns JSON."
        }
      },
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.options_contracts",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.05,
      "x-apiosk-plans": [
        "marketplace_us_stock_options_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "JSON:API style envelope with meta, data and links.",
      "properties": {
        "meta": {
          "type": "object",
          "description": "Metadata about the current page of results.",
          "properties": {
            "offset": {
              "type": "integer",
              "description": "Offset of the first returned record."
            },
            "limit": {
              "type": "integer",
              "description": "Maximum number of records per page."
            },
            "total": {
              "type": "integer",
              "description": "Total number of records available for the query."
            },
            "fields": {
              "type": "array",
              "description": "List of attribute names present in the response.",
              "items": {
                "type": "string",
                "description": "Attribute name."
              }
            }
          }
        },
        "data": {
          "type": "array",
          "description": "List of returned resources.",
          "items": {
            "type": "object",
            "description": "One resource object.",
            "properties": {
              "id": {
                "type": "string",
                "description": "Unique identifier of the resource."
              },
              "type": {
                "type": "string",
                "description": "Resource type, always 'options-contracts' for this endpoint."
              },
              "attributes": {
                "type": "object",
                "description": "The option contract data fields.",
                "properties": {
                  "contract": {
                    "type": "string",
                    "description": "Contract identifier (OCC style option symbol)."
                  },
                  "underlying_symbol": {
                    "type": "string",
                    "description": "Symbol of the underlying asset."
                  },
                  "exp_date": {
                    "type": "string",
                    "description": "Expiration date of the contract.",
                    "format": "date"
                  },
                  "expiration_type": {
                    "type": "string",
                    "description": "Expiration type of the contract (for example monthly, weekly)."
                  },
                  "type": {
                    "type": "string",
                    "description": "Contract type (call or put)."
                  },
                  "strike": {
                    "type": "number",
                    "description": "Strike price of the contract."
                  },
                  "exchange": {
                    "type": "string",
                    "description": "Exchange where the contract is traded."
                  },
                  "currency": {
                    "type": "string",
                    "description": "Currency of the contract."
                  },
                  "open": {
                    "type": "number",
                    "description": "Opening price."
                  },
                  "high": {
                    "type": "number",
                    "description": "Highest price."
                  },
                  "low": {
                    "type": "number",
                    "description": "Lowest price."
                  },
                  "last": {
                    "type": "number",
                    "description": "Last traded price."
                  },
                  "last_size": {
                    "type": "number",
                    "description": "Size of the last trade."
                  },
                  "change": {
                    "type": "number",
                    "description": "Price change versus the previous close."
                  },
                  "pctchange": {
                    "type": "number",
                    "description": "Percentage change in price."
                  },
                  "previous": {
                    "type": "number",
                    "description": "Previous close price."
                  },
                  "previous_date": {
                    "type": "string",
                    "description": "Date of the previous close price.",
                    "format": "date"
                  },
                  "bid": {
                    "type": "number",
                    "description": "Current bid price."
                  },
                  "bid_date": {
                    "type": "string",
                    "description": "Timestamp of the bid price.",
                    "format": "date-time"
                  },
                  "bid_size": {
                    "type": "integer",
                    "description": "Size of the bid."
                  },
                  "ask": {
                    "type": "number",
                    "description": "Current ask price."
                  },
                  "ask_date": {
                    "type": "string",
                    "description": "Timestamp of the ask price.",
                    "format": "date-time"
                  },
                  "ask_size": {
                    "type": "number",
                    "description": "Size of the ask."
                  },
                  "moneyness": {
                    "type": "number",
                    "description": "Moneyness of the option, a measure of how far in or out of the money it is."
                  },
                  "volume": {
                    "type": "number",
                    "description": "Trading volume."
                  },
                  "volume_change": {
                    "type": "number",
                    "description": "Change in trading volume."
                  },
                  "volume_pctchange": {
                    "type": "number",
                    "description": "Percentage change in trading volume."
                  },
                  "open_interest": {
                    "type": "number",
                    "description": "Open interest in the contract."
                  },
                  "open_interest_change": {
                    "type": "number",
                    "description": "Change in open interest."
                  },
                  "open_interest_pctchange": {
                    "type": "number",
                    "description": "Percentage change in open interest."
                  },
                  "volatility": {
                    "type": "number",
                    "description": "Implied volatility of the contract."
                  },
                  "volatility_change": {
                    "type": "number",
                    "description": "Change in implied volatility."
                  },
                  "volatility_pctchange": {
                    "type": "number",
                    "description": "Percentage change in implied volatility."
                  },
                  "theoretical": {
                    "type": "number",
                    "description": "Theoretical price of the contract."
                  },
                  "delta": {
                    "type": "number",
                    "description": "Delta of the contract, the sensitivity of price to the underlying price."
                  },
                  "gamma": {
                    "type": "number",
                    "description": "Gamma of the contract, the rate of change of delta."
                  },
                  "theta": {
                    "type": "number",
                    "description": "Theta of the contract, the time decay per day."
                  },
                  "vega": {
                    "type": "number",
                    "description": "Vega of the contract, the sensitivity to implied volatility."
                  },
                  "rho": {
                    "type": "number",
                    "description": "Rho of the contract, the sensitivity to interest rates."
                  },
                  "tradetime": {
                    "type": "string",
                    "description": "Date of the last recorded market activity for this contract, which is not always an actual trade.",
                    "format": "date"
                  },
                  "vol_oi_ratio": {
                    "type": "number",
                    "description": "Ratio of volume to open interest."
                  },
                  "dte": {
                    "type": "integer",
                    "description": "Days to expiration."
                  },
                  "midpoint": {
                    "type": "number",
                    "description": "Midpoint between bid and ask."
                  }
                }
              }
            }
          }
        },
        "links": {
          "type": "object",
          "description": "Pagination links.",
          "properties": {
            "next": {
              "type": "string",
              "description": "URL for the next page of results, or null when there are no more pages.",
              "examples": [
                "https://eodhd.com/mp/unicornbay/options/contracts?filter[underlying_symbol]=AAPL&sort=-exp_date&page[limit]=1&page[offset]=1"
              ]
            }
          }
        }
      },
      "required": [
        "meta",
        "data",
        "links"
      ]
    },
    "request_example": {
      "filter[contract]": "AAPL270115P00450000",
      "filter[underlying_symbol]": "AAPL",
      "filter[exp_date_eq]": "2025-03-19",
      "filter[exp_date_from]": "2025-01-01",
      "filter[exp_date_to]": "2025-06-01",
      "filter[tradetime_eq]": "2025-03-19",
      "filter[tradetime_from]": "2025-01-01",
      "filter[tradetime_to]": "2025-06-01",
      "filter[type]": "call",
      "filter[strike_eq]": 125.0,
      "filter[strike_from]": 120.0,
      "filter[strike_to]": 130.0,
      "sort": "exp_date",
      "page[offset]": 10,
      "page[limit]": 50,
      "fields[options-contracts]": "contract,exp_date,strike",
      "fmt": "json"
    },
    "response_example": "{\"meta\": {\"offset\": 0, \"limit\": 1, \"total\": 10552, \"fields\": [\"contract\", \"underlying_symbol\", \"exp_date\", \"type\", \"strike\"]}, \"data\": [{\"id\": \"AAPL270115C00450000\", \"type\": \"options-contracts\", \"attributes\": {\"contract\": \"AAPL270115C00450000\", \"underlying_symbol\": \"AAPL\", \"exp_date\": \"2027-01-15\", \"expiration_type\": \"monthly\", \"type\": \"call\", \"strike\": 450, \"exchange\": \"NASDAQ\", \"currency\": \"USD\", \"open\": 1.17, \"high\": 1.2, \"low\": 1.04, \"last\": 1.06, \"bid\": 1.05, \"ask\": 1.15, \"volume\": 692, \"open_interest\": 11638, \"volatility\": 0.2343, \"delta\": 0.040051, \"gamma\": 0.001174, \"theta\": -0.005183, \"vega\": 0.269504, \"rho\": 0.155196, \"tradetime\": \"2025-01-24\", \"dte\": 720, \"midpoint\": 1.1}}], \"links\": {\"next\": \"https://eodhd.com/mp/unicornbay/options/contracts?filter[underlying_symbol]=AAPL&sort=-exp_date&page[limit]=1&page[offset]=1\"}}",
    "tags": [
      "options",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "List US stock option contracts with quotes, Greeks and open interest, filtered by underlying, expiration, strike and type."
    ],
    "input_entities": [
      "filter[contract]",
      "filter[underlying_symbol]",
      "filter[exp_date_eq]",
      "filter[exp_date_from]",
      "filter[exp_date_to]",
      "filter[tradetime_eq]",
      "filter[tradetime_from]",
      "filter[tradetime_to]",
      "filter[type]",
      "filter[strike_eq]",
      "filter[strike_from]",
      "filter[strike_to]",
      "sort",
      "page[offset]",
      "page[limit]",
      "fields[options-contracts]",
      "fmt"
    ],
    "output_entities": [
      "options"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "List US stock option contracts with quotes, Greeks and open interest, filtered by underlying, expiration, strike and type.",
    "external_docs_url": "https://eodhd.com/marketplace/unicornbay/options/docs",
    "documentation_markdown": "GET /api/mp/unicornbay/options/contracts — List US stock option contracts with quotes, Greeks and open interest, filtered by underlying, expiration, strike and type.\n\nReturns a paginated list of US listed option contracts together with their latest end-of-day snapshot: OHLC, bid and ask with sizes, volume, open interest, implied volatility, the theoretical price and the Greeks delta, gamma, theta, vega and rho. Use it to build an option chain for an underlying symbol, to locate a specific contract, or to scan strikes and expirations. Results are filtered with filter[...] parameters, sorted by expiration date or strike, and paged with page[offset] and page[limit] up to a maximum offset of 10000 and 1000 rows per page. This is a separate EODHD Marketplace subscription and is not part of the standard EODHD plans.\n\nEODHD call weight: 10; required plans: marketplace_us_stock_options_subscription.\n\nThere is no compact mode on this endpoint; compact is documented only for /options/eod. Sold as a separate EODHD Marketplace subscription from Unicorn Data Services, not included in any standard EODHD plan. Listed price is 29.99 USD per month for the first three months and 39.99 USD per month afterwards. Limits: 100000 API calls per 24 hours, 1000 API requests per minute, and 1 API request equals 10 API calls. The 24 hour window is counted differently for Marketplace products than for the main EODHD plans. The api_token value 'demo' works for AAPL only. Coverage is US listed options on over 6000 top traded underlying symbols with roughly two years of history, and null is returned where a value is unavailable. The predecessor endpoint GET /api/options/{symbol} is deprecated: it still resolves but responds with 'Forbidden. You have no access to Options Data Feed.', its documentation page has been removed, and https://eodhd.com/financial-apis/options-data-api now redirects to the Marketplace product page. The OpenAPI document declares server /api/mp/unicornbay, so the live base path is https://eodhd.com/api/mp/unicornbay/options/... . Note that the 'next' links inside responses are rendered without the /api prefix as https://eodhd.com/mp/unicornbay/options/... , so prefer rebuilding the URL from page[offset] and page[limit]. The spec does not document an fmt parameter for the Marketplace options endpoints; responses are JSON.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "options_contracts"
  },
  {
    "id": "b3156d8c-18ed-5d03-bd46-2bc00d729c46",
    "method": "GET",
    "path": "/api/mp/unicornbay/options/eod",
    "price": 0.043478,
    "description": "Return the end-of-day history of quotes, volume, open interest and Greeks for US stock option contracts.",
    "query_params": {
      "type": "object",
      "properties": {
        "filter[contract]": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Filter options contracts by specific contract name or identifier."
        },
        "filter[underlying_symbol]": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Filter options contracts based on the underlying asset's symbol (for example 'AAPL' for Apple)."
        },
        "filter[exp_date_eq]": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Filter options contracts that expire on the exact given date (format YYYY-MM-DD)."
        },
        "filter[exp_date_from]": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Filter options contracts with expiration dates from the specified date onwards (format YYYY-MM-DD)."
        },
        "filter[exp_date_to]": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Filter options contracts with expiration dates up to the specified date (format YYYY-MM-DD)."
        },
        "filter[tradetime_eq]": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Filter options contracts with a trade time on the exact given date (format YYYY-MM-DD). Added in the March 2025 update."
        },
        "filter[tradetime_from]": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Filter options contracts with a trade time from the specified date onwards (format YYYY-MM-DD)."
        },
        "filter[tradetime_to]": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Filter options contracts with a trade time up to the specified date (format YYYY-MM-DD)."
        },
        "filter[type]": {
          "type": "string",
          "enum": [
            "put",
            "call"
          ],
          "required": false,
          "x-parameter-location": "query",
          "description": "Filter options contracts by type."
        },
        "filter[strike_eq]": {
          "type": "number",
          "required": false,
          "x-parameter-location": "query",
          "description": "Filter options contracts by an exact strike price."
        },
        "filter[strike_from]": {
          "type": "number",
          "required": false,
          "x-parameter-location": "query",
          "description": "Filter options contracts with strike prices from the specified value onwards."
        },
        "filter[strike_to]": {
          "type": "number",
          "required": false,
          "x-parameter-location": "query",
          "description": "Filter options contracts with strike prices up to the specified value."
        },
        "sort": {
          "type": "string",
          "enum": [
            "exp_date",
            "strike",
            "-exp_date",
            "-strike"
          ],
          "required": false,
          "x-parameter-location": "query",
          "description": "Sorting order of the results. A leading minus sign reverses the direction."
        },
        "page[offset]": {
          "type": "integer",
          "default": 0,
          "required": false,
          "x-parameter-location": "query",
          "description": "Offset for pagination, specifying the starting point of the results. Maximum is 10000."
        },
        "page[limit]": {
          "type": "integer",
          "default": 1000,
          "required": false,
          "x-parameter-location": "query",
          "description": "Maximum number of results to return per page. Default and maximum value is 1000."
        },
        "fields[options-eod]": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Comma separated list of attribute names to include in the response, which shrinks the payload."
        },
        "compact": {
          "type": "boolean",
          "default": false,
          "required": false,
          "x-parameter-location": "query",
          "description": "Enables compact mode. With compact=1 each element of data is a positional array of values in the order given by meta.fields instead of an object with attributes."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Output format. The Marketplace options specification does not document this parameter; the endpoint returns JSON."
        }
      },
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.options_eod",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.05,
      "x-apiosk-plans": [
        "marketplace_us_stock_options_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "JSON:API style envelope with meta, data and links.",
      "properties": {
        "meta": {
          "type": "object",
          "description": "Metadata about the current page of results.",
          "properties": {
            "offset": {
              "type": "integer",
              "description": "Offset of the first returned record."
            },
            "limit": {
              "type": "integer",
              "description": "Maximum number of records per page."
            },
            "total": {
              "type": "integer",
              "description": "Total number of records available for the query."
            },
            "fields": {
              "type": "array",
              "description": "List of attribute names present in the response.",
              "items": {
                "type": "string",
                "description": "Attribute name."
              }
            }
          }
        },
        "data": {
          "type": "array",
          "description": "List of returned resources.",
          "items": {
            "type": "object",
            "description": "One resource object.",
            "properties": {
              "id": {
                "type": "string",
                "description": "Unique identifier of the resource."
              },
              "type": {
                "type": "string",
                "description": "Resource type, always 'options-eod' for this endpoint."
              },
              "attributes": {
                "type": "object",
                "description": "The option contract data fields.",
                "properties": {
                  "contract": {
                    "type": "string",
                    "description": "Contract identifier (OCC style option symbol)."
                  },
                  "underlying_symbol": {
                    "type": "string",
                    "description": "Symbol of the underlying asset."
                  },
                  "exp_date": {
                    "type": "string",
                    "description": "Expiration date of the contract.",
                    "format": "date"
                  },
                  "expiration_type": {
                    "type": "string",
                    "description": "Expiration type of the contract (for example monthly, weekly)."
                  },
                  "type": {
                    "type": "string",
                    "description": "Contract type (call or put)."
                  },
                  "strike": {
                    "type": "number",
                    "description": "Strike price of the contract."
                  },
                  "exchange": {
                    "type": "string",
                    "description": "Exchange where the contract is traded."
                  },
                  "currency": {
                    "type": "string",
                    "description": "Currency of the contract."
                  },
                  "open": {
                    "type": "number",
                    "description": "Opening price."
                  },
                  "high": {
                    "type": "number",
                    "description": "Highest price."
                  },
                  "low": {
                    "type": "number",
                    "description": "Lowest price."
                  },
                  "last": {
                    "type": "number",
                    "description": "Last traded price."
                  },
                  "last_size": {
                    "type": "number",
                    "description": "Size of the last trade."
                  },
                  "change": {
                    "type": "number",
                    "description": "Price change versus the previous close."
                  },
                  "pctchange": {
                    "type": "number",
                    "description": "Percentage change in price."
                  },
                  "previous": {
                    "type": "number",
                    "description": "Previous close price."
                  },
                  "previous_date": {
                    "type": "string",
                    "description": "Date of the previous close price.",
                    "format": "date"
                  },
                  "bid": {
                    "type": "number",
                    "description": "Current bid price."
                  },
                  "bid_date": {
                    "type": "string",
                    "description": "Timestamp of the bid price.",
                    "format": "date-time"
                  },
                  "bid_size": {
                    "type": "integer",
                    "description": "Size of the bid."
                  },
                  "ask": {
                    "type": "number",
                    "description": "Current ask price."
                  },
                  "ask_date": {
                    "type": "string",
                    "description": "Timestamp of the ask price.",
                    "format": "date-time"
                  },
                  "ask_size": {
                    "type": "number",
                    "description": "Size of the ask."
                  },
                  "moneyness": {
                    "type": "number",
                    "description": "Moneyness of the option, a measure of how far in or out of the money it is."
                  },
                  "volume": {
                    "type": "number",
                    "description": "Trading volume."
                  },
                  "volume_change": {
                    "type": "number",
                    "description": "Change in trading volume."
                  },
                  "volume_pctchange": {
                    "type": "number",
                    "description": "Percentage change in trading volume."
                  },
                  "open_interest": {
                    "type": "number",
                    "description": "Open interest in the contract."
                  },
                  "open_interest_change": {
                    "type": "number",
                    "description": "Change in open interest."
                  },
                  "open_interest_pctchange": {
                    "type": "number",
                    "description": "Percentage change in open interest."
                  },
                  "volatility": {
                    "type": "number",
                    "description": "Implied volatility of the contract."
                  },
                  "volatility_change": {
                    "type": "number",
                    "description": "Change in implied volatility."
                  },
                  "volatility_pctchange": {
                    "type": "number",
                    "description": "Percentage change in implied volatility."
                  },
                  "theoretical": {
                    "type": "number",
                    "description": "Theoretical price of the contract."
                  },
                  "delta": {
                    "type": "number",
                    "description": "Delta of the contract, the sensitivity of price to the underlying price."
                  },
                  "gamma": {
                    "type": "number",
                    "description": "Gamma of the contract, the rate of change of delta."
                  },
                  "theta": {
                    "type": "number",
                    "description": "Theta of the contract, the time decay per day."
                  },
                  "vega": {
                    "type": "number",
                    "description": "Vega of the contract, the sensitivity to implied volatility."
                  },
                  "rho": {
                    "type": "number",
                    "description": "Rho of the contract, the sensitivity to interest rates."
                  },
                  "tradetime": {
                    "type": "string",
                    "description": "Date of the last recorded market activity for this contract, which is not always an actual trade.",
                    "format": "date"
                  },
                  "vol_oi_ratio": {
                    "type": "number",
                    "description": "Ratio of volume to open interest."
                  },
                  "dte": {
                    "type": "integer",
                    "description": "Days to expiration."
                  },
                  "midpoint": {
                    "type": "number",
                    "description": "Midpoint between bid and ask."
                  }
                }
              }
            }
          }
        },
        "links": {
          "type": "object",
          "description": "Pagination links.",
          "properties": {
            "next": {
              "type": "string",
              "description": "URL for the next page of results, or null when there are no more pages.",
              "examples": [
                "https://eodhd.com/mp/unicornbay/options/eod?filter[underlying_symbol]=AAPL&sort=-exp_date&page[limit]=1&page[offset]=1&compact=0"
              ]
            }
          }
        }
      },
      "required": [
        "meta",
        "data",
        "links"
      ]
    },
    "request_example": {
      "filter[contract]": "AAPL270115P00450000",
      "filter[underlying_symbol]": "AAPL",
      "filter[exp_date_eq]": "2025-03-19",
      "filter[exp_date_from]": "2025-01-01",
      "filter[exp_date_to]": "2025-06-01",
      "filter[tradetime_eq]": "2025-03-19",
      "filter[tradetime_from]": "2025-01-01",
      "filter[tradetime_to]": "2025-06-01",
      "filter[type]": "call",
      "filter[strike_eq]": 125.0,
      "filter[strike_from]": 120.0,
      "filter[strike_to]": 130.0,
      "sort": "exp_date",
      "page[offset]": 10,
      "page[limit]": 50,
      "fields[options-eod]": "contract,exp_date,strike",
      "compact": 1,
      "fmt": "json"
    },
    "response_example": "{\"meta\": {\"offset\": 0, \"limit\": 1, \"total\": 724056, \"fields\": [\"contract\", \"underlying_symbol\", \"exp_date\"]}, \"data\": [{\"id\": \"AAPL270115P00450000-2025-01-24\", \"type\": \"options-eod\", \"attributes\": {\"contract\": \"AAPL270115P00450000\", \"underlying_symbol\": \"AAPL\", \"exp_date\": \"2027-01-15\", \"expiration_type\": \"monthly\", \"type\": \"put\", \"strike\": 450, \"exchange\": \"NASDAQ\", \"currency\": \"USD\", \"open\": 0, \"high\": 0, \"low\": 0, \"last\": 224, \"last_size\": 0, \"previous\": 0, \"previous_date\": null, \"bid\": 225.2, \"bid_date\": \"2025-01-24 20:59:59\", \"bid_size\": 15, \"ask\": 229.05, \"ask_date\": \"2025-01-24 20:59:59\", \"ask_size\": 7, \"moneyness\": 1.02, \"volume\": 0, \"open_interest\": 0, \"volatility\": 0.4711, \"theoretical\": 229.05, \"delta\": -0.891269, \"gamma\": 0.00123, \"theta\": -0.007702, \"vega\": 0.452044, \"rho\": -0.999999, \"tradetime\": \"2025-01-22\", \"vol_oi_ratio\": 0, \"dte\": 720, \"midpoint\": 227.13}}], \"links\": {\"next\": \"https://eodhd.com/mp/unicornbay/options/eod?filter[underlying_symbol]=AAPL&sort=-exp_date&page[limit]=1&page[offset]=1&compact=0\"}}",
    "tags": [
      "options",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Return the end-of-day history of quotes, volume, open interest and Greeks for US stock option contracts."
    ],
    "input_entities": [
      "filter[contract]",
      "filter[underlying_symbol]",
      "filter[exp_date_eq]",
      "filter[exp_date_from]",
      "filter[exp_date_to]",
      "filter[tradetime_eq]",
      "filter[tradetime_from]",
      "filter[tradetime_to]",
      "filter[type]",
      "filter[strike_eq]",
      "filter[strike_from]",
      "filter[strike_to]",
      "sort",
      "page[offset]",
      "page[limit]",
      "fields[options-eod]",
      "compact",
      "fmt"
    ],
    "output_entities": [
      "options"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "Return the end-of-day history of quotes, volume, open interest and Greeks for US stock option contracts.",
    "external_docs_url": "https://eodhd.com/marketplace/unicornbay/options/docs",
    "documentation_markdown": "GET /api/mp/unicornbay/options/eod — Return the end-of-day history of quotes, volume, open interest and Greeks for US stock option contracts.\n\nReturns all available end-of-day trade and bid/ask records for option contracts, one record per contract per day, with the same 43 attribute fields as the contracts endpoint. Use it for backtesting, implied volatility time series, open interest history and daily Greek evolution over the roughly two year history. It supports the same filter[...], sort, page[offset] and page[limit] parameters plus compact=1, which drops the field names and returns each row as a positional array to cut bandwidth. This is a separate EODHD Marketplace subscription and is not part of the standard EODHD plans.\n\nEODHD call weight: 10; required plans: marketplace_us_stock_options_subscription.\n\nIn compact mode the response_schema above does not apply to data: each element is an array of scalar values ordered exactly as meta.fields, for example [\"AAPL270115P00450000\",0,0,0,224,\"2025-01-24 20:59:59\"]. The tradetime field is the date of the last recorded market activity and is not always an executed trade: a real trade is indicated by volume greater than zero together with a non zero last price, otherwise tradetime reflects the last bid/ask or open interest update. Sold as a separate EODHD Marketplace subscription from Unicorn Data Services, not included in any standard EODHD plan. Listed price is 29.99 USD per month for the first three months and 39.99 USD per month afterwards. Limits: 100000 API calls per 24 hours, 1000 API requests per minute, and 1 API request equals 10 API calls. The 24 hour window is counted differently for Marketplace products than for the main EODHD plans. The api_token value 'demo' works for AAPL only. Coverage is US listed options on over 6000 top traded underlying symbols with roughly two years of history, and null is returned where a value is unavailable. The predecessor endpoint GET /api/options/{symbol} is deprecated: it still resolves but responds with 'Forbidden. You have no access to Options Data Feed.', its documentation page has been removed, and https://eodhd.com/financial-apis/options-data-api now redirects to the Marketplace product page. The OpenAPI document declares server /api/mp/unicornbay, so the live base path is https://eodhd.com/api/mp/unicornbay/options/... . Note that the 'next' links inside responses are rendered without the /api prefix as https://eodhd.com/mp/unicornbay/options/... , so prefer rebuilding the URL from page[offset] and page[limit]. The spec does not document an fmt parameter for the Marketplace options endpoints; responses are JSON.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "options_eod"
  },
  {
    "id": "85a82684-c573-54c2-a766-5308951e4ac8",
    "method": "GET",
    "path": "/api/mp/unicornbay/options/underlying-symbols",
    "price": 0.026087,
    "description": "List every US stock ticker for which option contracts are available in the options dataset.",
    "query_params": {
      "type": "object",
      "properties": {
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Output format. The Marketplace options specification does not document this parameter; the endpoint returns JSON."
        }
      },
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.options_underlying_symbols",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.03,
      "x-apiosk-plans": [
        "marketplace_us_stock_options_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "Envelope containing the list of supported underlying symbols.",
      "properties": {
        "meta": {
          "type": "object",
          "description": "Metadata about the response.",
          "properties": {
            "total": {
              "type": "integer",
              "description": "The total number of items available."
            },
            "fields": {
              "type": "array",
              "description": "A list of fields included in the response.",
              "items": {
                "type": "string",
                "description": "Field name, in practice 'underlying_symbol'."
              }
            },
            "compact": {
              "type": "boolean",
              "description": "Indicates whether the response is in compact format."
            }
          }
        },
        "data": {
          "type": "array",
          "description": "The list of supported underlying stock tickers.",
          "items": {
            "type": "string",
            "description": "An underlying stock ticker, for example AAPL."
          }
        },
        "links": {
          "type": "object",
          "description": "Navigation links for paginated responses.",
          "properties": {
            "next": {
              "type": "string",
              "description": "The URL for the next page of results, or null if there are no more pages."
            }
          }
        }
      },
      "required": [
        "meta",
        "data",
        "links"
      ]
    },
    "request_example": {
      "fmt": "json"
    },
    "response_example": "{\"meta\": {\"total\": 3, \"fields\": [\"underlying_symbol\"], \"compact\": true}, \"data\": [\"AAPL\", \"MSFT\", \"NVDA\"], \"links\": {\"next\": null}}",
    "tags": [
      "options",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "List every US stock ticker for which option contracts are available in the options dataset."
    ],
    "input_entities": [
      "fmt"
    ],
    "output_entities": [
      "options"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "List every US stock ticker for which option contracts are available in the options dataset.",
    "external_docs_url": "https://eodhd.com/marketplace/unicornbay/options/docs",
    "documentation_markdown": "GET /api/mp/unicornbay/options/underlying-symbols — List every US stock ticker for which option contracts are available in the options dataset.\n\nReturns the full list of underlying stock symbols covered by the US Stock Options Data API, as a flat array of ticker strings inside a meta/data/links envelope. Use it to discover the supported universe before querying contracts or end-of-day option data, or to validate a ticker. The live response currently reports roughly 6900 symbols and takes no filter or pagination parameters. This is a separate EODHD Marketplace subscription and is not part of the standard EODHD plans.\n\nEODHD call weight: 10; required plans: marketplace_us_stock_options_subscription.\n\nThe documented example shows meta.total of 3, but a live call with the demo token returned meta.total of 6947, matching the advertised coverage of over 6000 top traded symbols. The docs also link a CSV list of covered tickers from the product page. Sold as a separate EODHD Marketplace subscription from Unicorn Data Services, not included in any standard EODHD plan. Listed price is 29.99 USD per month for the first three months and 39.99 USD per month afterwards. Limits: 100000 API calls per 24 hours, 1000 API requests per minute, and 1 API request equals 10 API calls. The 24 hour window is counted differently for Marketplace products than for the main EODHD plans. The api_token value 'demo' works for AAPL only. Coverage is US listed options on over 6000 top traded underlying symbols with roughly two years of history, and null is returned where a value is unavailable. The predecessor endpoint GET /api/options/{symbol} is deprecated: it still resolves but responds with 'Forbidden. You have no access to Options Data Feed.', its documentation page has been removed, and https://eodhd.com/financial-apis/options-data-api now redirects to the Marketplace product page. The OpenAPI document declares server /api/mp/unicornbay, so the live base path is https://eodhd.com/api/mp/unicornbay/options/... . Note that the 'next' links inside responses are rendered without the /api prefix as https://eodhd.com/mp/unicornbay/options/... , so prefer rebuilding the URL from page[offset] and page[limit]. The spec does not document an fmt parameter for the Marketplace options endpoints; responses are JSON.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "options_underlying_symbols"
  },
  {
    "id": "63b4f7af-9833-51a2-8dfe-88d806387b79",
    "method": "GET",
    "path": "/api/bond-fundamentals/{identifier}",
    "price": 0.043478,
    "description": "Return reference and fundamental data for a US corporate bond identified by ISIN or CUSIP.",
    "query_params": {
      "type": "object",
      "properties": {
        "identifier": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "The bond identifier, either an ISIN or a CUSIP code. Other identifier types are not supported."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Output format. The documentation states that bond fundamentals are returned in JSON only, so use fmt=json."
        }
      },
      "required": [
        "identifier"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.bond_fundamentals",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.05,
      "x-apiosk-plans": [
        "all_in_one"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "A single bond fundamentals object.",
      "properties": {
        "ISIN": {
          "type": "string",
          "description": "International Securities Identification Number of the bond."
        },
        "Name": {
          "type": "string",
          "description": "Name of the bond issue."
        },
        "UpdateDate": {
          "type": "string",
          "format": "date",
          "description": "Date the record was last updated."
        },
        "WKN": {
          "type": "string",
          "description": "German Wertpapierkennnummer securities identifier."
        },
        "FIGI": {
          "type": "string",
          "description": "Financial Instrument Global Identifier of the bond."
        },
        "Currency": {
          "type": "string",
          "description": "Currency the bond is denominated in."
        },
        "Coupon": {
          "type": "number",
          "description": "Coupon rate of the bond."
        },
        "LastTradeDate": {
          "type": "string",
          "format": "date",
          "description": "Date of the last recorded trade."
        },
        "Maturity_Date": {
          "type": "string",
          "format": "date",
          "description": "Maturity date of the bond."
        },
        "YieldToMaturity": {
          "type": "number",
          "description": "Yield to maturity of the bond."
        },
        "Callable": {
          "type": "integer",
          "description": "Whether the bond is callable."
        },
        "NextCallDate": {
          "type": "string",
          "format": "date",
          "description": "Next date on which the bond can be called."
        },
        "MinimumSettlementAmount": {
          "type": "number",
          "description": "Minimum settlement amount for a transaction in the bond."
        },
        "ParIntegralMultiple": {
          "type": "number",
          "description": "Integral multiple of par in which the bond can be traded."
        },
        "ClassificationData": {
          "type": "object",
          "description": "Instrument classification of the bond.",
          "properties": {
            "BondType": {
              "type": "string",
              "description": "Type of the bond."
            },
            "DebtType": {
              "type": "string",
              "description": "Type of the debt instrument."
            },
            "IndustryGroup": {
              "type": "string",
              "description": "Industry group of the issuer."
            },
            "IndustrySubGroup": {
              "type": "string",
              "description": "Industry sub group of the issuer."
            },
            "SubProductAsset": {
              "type": "string",
              "description": "Sub product asset classification."
            },
            "SubProductAssetType": {
              "type": "string",
              "description": "Sub product asset type classification."
            }
          }
        },
        "IssueData": {
          "type": "object",
          "description": "Issuance details of the bond.",
          "properties": {
            "IssueDate": {
              "type": "string",
              "format": "date",
              "description": "Date the bond was issued."
            },
            "OfferingDate": {
              "type": "string",
              "format": "date",
              "description": "Date the bond was offered."
            },
            "FirstCouponDate": {
              "type": "string",
              "format": "date",
              "description": "Date of the first coupon payment."
            },
            "FirstTradingDay": {
              "type": "string",
              "format": "date",
              "description": "First day the bond traded."
            },
            "CouponPaymentFrequency": {
              "type": "string",
              "description": "Frequency of coupon payments."
            },
            "Issuer": {
              "type": "string",
              "description": "Name of the issuer."
            },
            "IssuerDescription": {
              "type": "string",
              "description": "Description of the issuer."
            },
            "IssuerCountry": {
              "type": "string",
              "description": "Country of the issuer."
            },
            "IssuerURL": {
              "type": "string",
              "description": "Website of the issuer."
            }
          }
        }
      }
    },
    "request_example": {
      "identifier": "910047AG4",
      "fmt": "json"
    },
    "response_example": null,
    "tags": [
      "fixed income",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Return reference and fundamental data for a US corporate bond identified by ISIN or CUSIP."
    ],
    "input_entities": [
      "identifier",
      "fmt"
    ],
    "output_entities": [
      "fixed income"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "Return reference and fundamental data for a US corporate bond identified by ISIN or CUSIP.",
    "external_docs_url": "https://eodhd.com/financial-apis/bonds-fundamentals-and-historical-api",
    "documentation_markdown": "GET /api/bond-fundamentals/{identifier} — Return reference and fundamental data for a US corporate bond identified by ISIN or CUSIP.\n\nReturns a single JSON object describing one bond: identifiers such as ISIN, WKN and FIGI, coupon, maturity date, yield to maturity, callability and next call date, settlement conventions, a classification block and an issuance block naming the issuer. Use it to enrich a bond position, to build a bond master file, or to compute cash flow schedules from the coupon and coupon payment frequency. Only US corporate bonds under the BOND virtual exchange currently have fundamentals, and only ISIN and CUSIP identifiers are accepted.\n\nEODHD call weight: 10; required plans: all_in_one.\n\nThe documentation page lists the output field names but shows no complete example JSON response and gives no per field types or descriptions, so the types and descriptions in response_schema are inferred from the field names and are not quoted from the docs. No other fields are documented; anything not listed here was not shown by the docs. The documented example identifier is the CUSIP 910047AG4; the matching ISIN form is US910047AG49. Each BOND fundamentals request consumes 10 API calls and the data is available with the All-In-One subscription only. Data is updated daily and covers only US corporate bonds; EODHD states new bonds can be added on request within 24 hours.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "bond_fundamentals"
  },
  {
    "id": "de08dfba-fe9b-5c61-a055-09eb369ecdc8",
    "method": "GET",
    "path": "/api/screener",
    "price": 0.026087,
    "description": "Screen the whole covered global stock universe by fundamentals, performance and volume, and return the matching tickers.",
    "query_params": {
      "type": "object",
      "properties": {
        "filters": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Filter conditions as a JSON array of triples [[field, operation, value], ...], combined with AND. Must be valid JSON with straight double quotes. String fields accept the operations =, match, in and not in; number fields accept =, !=, >, <, >=, <=, in and not in. The in and not in operations take a JSON array as the value. Filterable fields are code, name, exchange, sub_exchange, sector, industry, market_capitalization, earnings_share, dividend_yield, refund_1d_p, refund_5d_p, adjusted_close, avgvol_1d and avgvol_200d."
        },
        "signals": {
          "type": "string",
          "enum": [
            "200d_new_lo",
            "200d_new_hi",
            "bookvalue_neg",
            "bookvalue_pos",
            "wallstreet_lo",
            "wallstreet_hi"
          ],
          "required": false,
          "x-parameter-location": "query",
          "description": "Comma separated list of pre-calculated signals. A ticker must satisfy every signal listed, and signals combine with filters the same way."
        },
        "sort": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Sort as field_name.direction, for example market_capitalization.desc. Direction is asc or desc, and every filterable field is also sortable."
        },
        "limit": {
          "type": "integer",
          "default": 50,
          "required": false,
          "x-parameter-location": "query",
          "description": "Number of tickers to return. Range 1 to 500."
        },
        "offset": {
          "type": "integer",
          "default": 0,
          "required": false,
          "x-parameter-location": "query",
          "description": "Number of tickers to skip, for paging. Range 0 to 999, so no screen can be paged beyond its first 1000 matches."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Output format of the response."
        }
      },
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.stock_screener",
      "x-apiosk-call-weight": 5,
      "x-apiosk-provider-cost-eur": 0.00025,
      "x-apiosk-target-buyer-price-eur": 0.03,
      "x-apiosk-plans": [
        "all_in_one",
        "eod_intraday_all_world_extended"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "Envelope holding the matching tickers.",
      "properties": {
        "data": {
          "type": "array",
          "description": "The tickers matching the screen, in the requested sort order.",
          "items": {
            "type": "object",
            "description": "One matching ticker.",
            "properties": {
              "code": {
                "type": "string",
                "description": "Ticker code without the exchange suffix."
              },
              "name": {
                "type": "string",
                "description": "Company name."
              },
              "last_day_data_date": {
                "type": "string",
                "description": "Trading day the price and performance values come from.",
                "format": "date"
              },
              "adjusted_close": {
                "type": "number",
                "description": "Latest known end-of-day adjusted close."
              },
              "refund_1d": {
                "type": "number",
                "description": "Absolute price change over the last trading day, in the listing currency."
              },
              "refund_1d_p": {
                "type": "number",
                "description": "Price change over the last trading day, in percent."
              },
              "refund_5d": {
                "type": "number",
                "description": "Absolute price change over the last 5 trading days."
              },
              "refund_5d_p": {
                "type": "number",
                "description": "Price change over the last 5 trading days, in percent."
              },
              "exchange": {
                "type": "string",
                "description": "Exchange code the ticker belongs to."
              },
              "currency_symbol": {
                "type": "string",
                "description": "Currency the price and market capitalisation are expressed in."
              },
              "market_capitalization": {
                "type": "number",
                "description": "Latest market capitalisation, in the listing currency."
              },
              "earnings_share": {
                "type": "number",
                "description": "Latest earnings per share."
              },
              "dividend_yield": {
                "type": "number",
                "description": "Latest dividend yield as a fraction, so 0.0034 means 0.34 percent."
              },
              "sector": {
                "type": "string",
                "description": "Sector name."
              },
              "industry": {
                "type": "string",
                "description": "Industry name."
              },
              "avgvol_1d": {
                "type": "number",
                "description": "Last trading day volume."
              },
              "avgvol_200d": {
                "type": "number",
                "description": "Average volume over the last 200 trading days."
              }
            }
          }
        }
      },
      "required": [
        "data"
      ]
    },
    "request_example": {
      "filters": "[[\"exchange\",\"=\",\"us\"],[\"market_capitalization\",\">\",10000000000]]",
      "signals": "bookvalue_neg,200d_new_lo",
      "sort": "market_capitalization.desc",
      "limit": 10,
      "offset": 0,
      "fmt": "json"
    },
    "response_example": "{\"data\": [{\"code\": \"AAPL\", \"name\": \"Apple Inc.\", \"last_day_data_date\": \"2026-08-12\", \"adjusted_close\": 302.25, \"refund_1d\": -2.66, \"refund_1d_p\": -0.87, \"refund_5d\": -8.482, \"refund_5d_p\": -2.73, \"exchange\": \"US\", \"currency_symbol\": \"$\", \"market_capitalization\": 4411090796544, \"earnings_share\": 8.72, \"dividend_yield\": 0.0034, \"sector\": \"Technology\", \"industry\": \"Consumer Electronics\", \"avgvol_1d\": 40588500, \"avgvol_200d\": 49738504}]}",
    "tags": [
      "screening",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Screen the whole covered global stock universe by fundamentals, performance and volume, and return the matching tickers."
    ],
    "input_entities": [
      "filters",
      "signals",
      "sort",
      "limit",
      "offset",
      "fmt"
    ],
    "output_entities": [
      "screening"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "Screen the whole covered global stock universe by fundamentals, performance and volume, and return the matching tickers.",
    "external_docs_url": "https://eodhd.com/financial-apis/stock-market-screener-api/",
    "documentation_markdown": "GET /api/screener — Screen the whole covered global stock universe by fundamentals, performance and volume, and return the matching tickers.\n\nFilters every covered ticker in a single request on market capitalisation, exchange, sector, industry, EPS, dividend yield, one and five day performance and traded volume, and returns the matching rows with the values that were filtered on. Filters are a JSON array of [field, operation, value] triples combined with AND, signals add pre-calculated conditions such as 200 day highs or negative book value, sort takes a single field with a direction, and results page with limit and offset. Values come from the latest completed trading day, carried on every row in last_day_data_date. A single screen exposes at most its first 1000 matches because offset stops at 999.\n\nEODHD call weight: 5; required plans: all_in_one, eod_intraday_all_world_extended.\n\nFilter syntax traps: the filters value must be valid JSON with straight double quotes, and curly typographic quotes return HTTP 422 with 'The filters must be a valid JSON string'. There is no between operation, so a band is built from two conditions on the same field. The match operation matches from the start of a word and an asterisk extends the prefix, so 'Apple' and 'App*' both find Apple Inc. while a mid word fragment such as 'pple' matches nothing; multi word sector and industry values need match rather than an exact =. String comparisons are case insensitive. Applying a numeric operation to a string field or the reverse returns HTTP 422 with an invalid-operation error, a screen matching nothing returns HTTP 200 with an empty data array, and a missing or invalid token returns HTTP 401. Requesting more than 500 rows or an offset above 999 also returns 422. The exchange field accepts exchange codes such as us, lse, to and xetra and additionally accepts NYSE and NASDAQ, which narrow the US exchange to that venue and are equivalent to using sub_exchange. Data caveats: market capitalisation and prices are in each ticker's listing currency and are not converted to USD, so the same company appears once per listing and values are not comparable across rows without reading currency_symbol; filtering on a fundamentals derived field also excludes every ticker where the value is missing, which is common on secondary listings; and a minority of secondary foreign listings carry mis-scaled dividend_yield values in the thousands, so cap the upper bound. Each request consumes 5 API calls. Only the data key is documented in the response; no meta, count or pagination object is shown.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "stock_screener"
  },
  {
    "id": "d970f100-1b4d-5e4a-80ea-44acd0aa4e4e",
    "method": "GET",
    "path": "/api/technical/{symbol}",
    "price": 0.017391,
    "description": "Compute a technical indicator time series for any supported symbol, selected with the function parameter.",
    "query_params": {
      "type": "object",
      "properties": {
        "symbol": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "Ticker in the form {SYMBOL_NAME}.{EXCHANGE_ID}, for example AAPL.US for NASDAQ or AAPL.MX for the Mexican Stock Exchange. The exchange suffix may be omitted to default to US."
        },
        "function": {
          "type": "string",
          "enum": [
            "splitadjusted",
            "avgvol",
            "avgvolccy",
            "sma",
            "ema",
            "wma",
            "volatility",
            "stochastic",
            "rsi",
            "stddev",
            "stochrsi",
            "slope",
            "dmi",
            "adx",
            "macd",
            "atr",
            "cci",
            "sar",
            "beta",
            "bbands",
            "format_amibroker"
          ],
          "required": true,
          "x-parameter-location": "query",
          "description": "The indicator to compute. splitadjusted returns split adjusted OHLC rather than an indicator, and format_amibroker returns an AmiBroker import file rather than JSON."
        },
        "period": {
          "type": "integer",
          "default": 50,
          "required": false,
          "x-parameter-location": "query",
          "description": "Number of data points used for each value. Valid range 2 to 100000. Applies to sma, ema, wma, volatility, rsi, stddev, slope, dmi, adx, atr, cci, bbands, beta, avgvol and avgvolccy."
        },
        "from": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Start date of the returned series in YYYY-MM-DD format."
        },
        "to": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "End date of the returned series in YYYY-MM-DD format."
        },
        "order": {
          "type": "string",
          "enum": [
            "a",
            "d"
          ],
          "default": "a",
          "required": false,
          "x-parameter-location": "query",
          "description": "Date order: 'a' for ascending (old to new) or 'd' for descending (new to old)."
        },
        "filter": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Return only the last value as a single number instead of the full series, used together with fmt=json. The value is 'last_' plus the name of the response field, for example last_sma, last_ema or last_volume."
        },
        "splitadjusted_only": {
          "type": "integer",
          "enum": [
            0,
            1
          ],
          "default": 0,
          "required": false,
          "x-parameter-location": "query",
          "description": "Set to 1 to calculate from closes adjusted for splits only instead of splits and dividends. Works with sma, ema, wma, volatility, rsi, slope and macd."
        },
        "fast_kperiod": {
          "type": "integer",
          "default": 14,
          "required": false,
          "x-parameter-location": "query",
          "description": "Fast K period. Used by stochastic (default 14) and stochrsi (default 14). Valid range 2 to 100000."
        },
        "slow_kperiod": {
          "type": "integer",
          "default": 3,
          "required": false,
          "x-parameter-location": "query",
          "description": "Slow K period, used by stochastic. Valid range 2 to 100000."
        },
        "slow_dperiod": {
          "type": "integer",
          "default": 3,
          "required": false,
          "x-parameter-location": "query",
          "description": "Slow D period, used by stochastic. Valid range 2 to 100000."
        },
        "fast_dperiod": {
          "type": "integer",
          "default": 14,
          "required": false,
          "x-parameter-location": "query",
          "description": "Fast D period, used by stochrsi. Valid range 2 to 100000."
        },
        "fast_period": {
          "type": "integer",
          "default": 12,
          "required": false,
          "x-parameter-location": "query",
          "description": "Fast EMA period, used by macd. Valid range 2 to 100000."
        },
        "slow_period": {
          "type": "integer",
          "default": 26,
          "required": false,
          "x-parameter-location": "query",
          "description": "Slow EMA period, used by macd. Valid range 2 to 100000."
        },
        "signal_period": {
          "type": "integer",
          "default": 9,
          "required": false,
          "x-parameter-location": "query",
          "description": "Signal line period, used by macd. Valid range 2 to 100000."
        },
        "acceleration": {
          "type": "number",
          "default": 0.02,
          "required": false,
          "x-parameter-location": "query",
          "description": "Acceleration factor used up to the maximum value, used by sar."
        },
        "maximum": {
          "type": "number",
          "default": 0.2,
          "required": false,
          "x-parameter-location": "query",
          "description": "Maximum acceleration factor, used by sar."
        },
        "code2": {
          "type": "string",
          "default": "GSPC.INDX",
          "required": false,
          "x-parameter-location": "query",
          "description": "Ticker to calculate beta against, used by beta."
        },
        "agg_period": {
          "type": "string",
          "enum": [
            "d",
            "w",
            "m"
          ],
          "default": "d",
          "required": false,
          "x-parameter-location": "query",
          "description": "Aggregation period, used by splitadjusted: d for daily, w for weekly, m for monthly."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Output format of the response."
        }
      },
      "required": [
        "symbol",
        "function"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.technical_indicator",
      "x-apiosk-call-weight": 5,
      "x-apiosk-provider-cost-eur": 0.00025,
      "x-apiosk-target-buyer-price-eur": 0.02,
      "x-apiosk-plans": [
        "all_in_one",
        "eod_intraday_all_world_extended"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "array",
      "description": "Array of dated indicator values. Every element carries date plus the value fields specific to the requested function; only those fields are present in any single response.",
      "items": {
        "type": "object",
        "description": "One data point: the date plus the value or values produced by the requested indicator.",
        "properties": {
          "date": {
            "type": "string",
            "format": "date",
            "description": "Date of the data point in YYYY-MM-DD format."
          },
          "sma": {
            "type": "number",
            "description": "Simple moving average value, returned when function=sma."
          },
          "ema": {
            "type": "number",
            "description": "Exponential moving average value, returned when function=ema."
          },
          "wma": {
            "type": "number",
            "description": "Weighted moving average value, returned when function=wma."
          },
          "rsi": {
            "type": "number",
            "description": "Relative Strength Index value, returned when function=rsi."
          },
          "volatility": {
            "type": "number",
            "description": "Volatility value, returned when function=volatility."
          },
          "stddev": {
            "type": "number",
            "description": "Standard deviation value, returned when function=stddev."
          },
          "slope": {
            "type": "number",
            "description": "Linear regression slope value, returned when function=slope."
          },
          "atr": {
            "type": "number",
            "description": "Average True Range value, returned when function=atr."
          },
          "cci": {
            "type": "number",
            "description": "Commodity Channel Index value, returned when function=cci."
          },
          "adx": {
            "type": "number",
            "description": "Average Directional Movement Index value, returned when function=adx."
          },
          "dmi": {
            "type": "number",
            "description": "Directional Movement Index value, returned when function=dmi."
          },
          "sar": {
            "type": "number",
            "description": "Parabolic SAR value, returned when function=sar."
          },
          "beta": {
            "type": "number",
            "description": "Beta of the symbol against code2, returned when function=beta."
          },
          "avgvol": {
            "type": "number",
            "description": "Average trading volume, returned when function=avgvol."
          },
          "avgvolccy": {
            "type": "number",
            "description": "Average trading volume expressed in currency, returned when function=avgvolccy."
          },
          "macd": {
            "type": "number",
            "description": "MACD line value, returned when function=macd."
          },
          "signal": {
            "type": "number",
            "description": "MACD signal line value, returned when function=macd."
          },
          "divergence": {
            "type": "number",
            "description": "MACD divergence (histogram) value, returned when function=macd."
          },
          "uband": {
            "type": "number",
            "description": "Upper Bollinger band, returned when function=bbands."
          },
          "mband": {
            "type": "number",
            "description": "Middle Bollinger band, returned when function=bbands."
          },
          "lband": {
            "type": "number",
            "description": "Lower Bollinger band, returned when function=bbands."
          },
          "k_values": {
            "type": "number",
            "description": "Stochastic oscillator K value, returned when function=stochastic."
          },
          "d_values": {
            "type": "number",
            "description": "Stochastic oscillator D value, returned when function=stochastic."
          },
          "fastkline": {
            "type": "number",
            "description": "Stochastic RSI fast K line value, returned when function=stochrsi."
          },
          "fastdline": {
            "type": "number",
            "description": "Stochastic RSI fast D line value, returned when function=stochrsi."
          },
          "open": {
            "type": "number",
            "description": "Split adjusted open price, returned when function=splitadjusted."
          },
          "high": {
            "type": "number",
            "description": "Split adjusted high price, returned when function=splitadjusted."
          },
          "low": {
            "type": "number",
            "description": "Split adjusted low price, returned when function=splitadjusted."
          },
          "close": {
            "type": "number",
            "description": "Split adjusted close price, returned when function=splitadjusted."
          },
          "volume": {
            "type": "integer",
            "description": "Traded volume, returned when function=splitadjusted."
          }
        },
        "required": [
          "date"
        ]
      }
    },
    "request_example": {
      "symbol": "AAPL.US",
      "function": "sma",
      "period": 50,
      "from": "2017-08-01",
      "to": "2020-01-01",
      "order": "d",
      "filter": "last_ema",
      "splitadjusted_only": 1,
      "fast_kperiod": 14,
      "slow_kperiod": 3,
      "slow_dperiod": 3,
      "fast_dperiod": 14,
      "fast_period": 12,
      "slow_period": 26,
      "signal_period": 9,
      "acceleration": 0.02,
      "maximum": 0.2,
      "code2": "GSPC.INDX",
      "agg_period": "d",
      "fmt": "json"
    },
    "response_example": "[{\"date\": \"2019-12-10\", \"sma\": 60.155}, {\"date\": \"2019-12-09\", \"sma\": 59.9375}, {\"date\": \"2019-12-06\", \"sma\": 59.7029}]",
    "tags": [
      "technicals",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Compute a technical indicator time series for any supported symbol, selected with the function parameter."
    ],
    "input_entities": [
      "symbol",
      "function",
      "period",
      "from",
      "to",
      "order",
      "filter",
      "splitadjusted_only",
      "fast_kperiod",
      "slow_kperiod",
      "slow_dperiod",
      "fast_dperiod",
      "fast_period",
      "slow_period",
      "signal_period",
      "acceleration",
      "maximum",
      "code2",
      "agg_period",
      "fmt"
    ],
    "output_entities": [
      "technicals"
    ],
    "endpoint_category": "finance",
    "action_type": "compute",
    "task_summary": "Compute a technical indicator time series for any supported symbol, selected with the function parameter.",
    "external_docs_url": "https://eodhd.com/financial-apis/technical-indicators-api/",
    "documentation_markdown": "GET /api/technical/{symbol} — Compute a technical indicator time series for any supported symbol, selected with the function parameter.\n\nReturns a dated time series of a single technical indicator calculated server side on EODHD price history, covering moving averages, oscillators, volatility measures, trend and volume indicators plus split adjusted OHLC. The indicator is chosen with function and tuned with the parameters that function accepts, and the series can be restricted with from and to, reversed with order, or reduced to a single latest value with filter. It works for stocks, ETFs, indices, forex and crypto tickers in the same {SYMBOL}.{EXCHANGE_ID} format used elsewhere in the API. Each request consumes 5 API calls.\n\nEODHD call weight: 5; required plans: all_in_one, eod_intraday_all_world_extended.\n\nOne endpoint covers every indicator; the response shape varies per function and was verified against live demo calls to https://eodhd.com/api/technical/AAPL.US with api_token=demo, because the documentation page shows the example output as an image. Per function value fields: sma -> sma; ema -> ema; wma -> wma; rsi -> rsi; volatility -> volatility; stddev -> stddev; slope -> slope; atr -> atr; cci -> cci; adx -> adx; dmi -> dmi (a single value, not separate plus_di and minus_di); sar -> sar; beta -> beta; avgvol -> avgvol; avgvolccy -> avgvolccy; macd -> macd, signal, divergence; bbands -> uband, mband, lband; stochastic -> k_values, d_values; stochrsi -> fastkline, fastdline; splitadjusted -> open, high, low, close, volume. The function name is also the JSON key for all single value indicators, which makes filter=last_<function> the pattern for the last value shortcut. format_amibroker is not a JSON indicator: it returns an AmiBroker import file whose OHLC fields are split adjusted only and not adjusted for dividends, so the array schema above does not apply to it. A request whose from and to window is shorter than period returns an empty array rather than an error. Available in the All-In-One and EOD+Intraday All World Extended plans; each request consumes 5 API calls. EODHD notes further indicators are available on the Marketplace and that new indicators can be added on request.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "technical_indicator"
  },
  {
    "id": "59142e0a-cd5d-5c5d-b46d-b1ee4536450f",
    "method": "GET",
    "path": "/api/news",
    "price": 0.026087,
    "description": "Return financial news articles for a ticker or topic tag, each with in house sentiment scores.",
    "query_params": {
      "type": "object",
      "properties": {
        "s": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Ticker code to filter news, for example AAPL.US. Provide s or t; if both are omitted the latest general feed is returned."
        },
        "t": {
          "type": "string",
          "enum": [
            "balance sheet",
            "capital employed",
            "class action",
            "company announcement",
            "consensus eps estimate",
            "consensus estimate",
            "credit rating",
            "discounted cash flow",
            "dividend payments",
            "earnings estimate",
            "earnings growth",
            "earnings per share",
            "earnings release",
            "earnings report",
            "earnings results",
            "earnings surprise",
            "estimate revisions",
            "european regulatory news",
            "financial results",
            "fourth quarter",
            "free cash flow",
            "future cash flows",
            "growth rate",
            "initial public offering",
            "insider ownership",
            "insider transactions",
            "institutional investors",
            "institutional ownership",
            "intrinsic value",
            "market research reports",
            "net income",
            "operating income",
            "present value",
            "press releases",
            "price target",
            "quarterly earnings",
            "quarterly results",
            "ratings",
            "research analysis and reports",
            "return on equity",
            "revenue estimates",
            "revenue growth",
            "roce",
            "roe",
            "share price",
            "shareholder rights",
            "shareholder",
            "shares outstanding",
            "split",
            "strong buy",
            "total revenue",
            "zacks investment research",
            "zacks rank"
          ],
          "required": false,
          "x-parameter-location": "query",
          "description": "Topic tag to filter news, for example technology. Provide s or t. Besides the standard list of 50 tags the feed adds AI detected tags, so almost any topic can be queried."
        },
        "from": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Start date in YYYY-MM-DD format, inclusive."
        },
        "to": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "End date in YYYY-MM-DD format, inclusive."
        },
        "limit": {
          "type": "integer",
          "default": 50,
          "required": false,
          "x-parameter-location": "query",
          "description": "Number of articles to return. Range 1 to 1000."
        },
        "offset": {
          "type": "integer",
          "default": 0,
          "required": false,
          "x-parameter-location": "query",
          "description": "Number of articles to skip, for pagination."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format."
        }
      },
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.financial_news",
      "x-apiosk-call-weight": 5,
      "x-apiosk-provider-cost-eur": 0.00025,
      "x-apiosk-target-buyer-price-eur": 0.03,
      "x-apiosk-plans": [
        "standalone_package",
        "all_in_one",
        "eod_historical_data_all_world",
        "eod_intraday_all_world_extended",
        "fundamentals_data_feed",
        "free"
      ],
      "x-apiosk-free-plan": true,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "array",
      "description": "Array of news articles, newest first.",
      "items": {
        "type": "object",
        "description": "One news article with its sentiment scores.",
        "properties": {
          "date": {
            "type": "string",
            "format": "date-time",
            "description": "Publication date and time of the article, ISO 8601 with timezone offset."
          },
          "title": {
            "type": "string",
            "description": "Headline of the article."
          },
          "content": {
            "type": "string",
            "description": "Full article body as plain text."
          },
          "link": {
            "type": "string",
            "format": "uri",
            "description": "Direct URL to the original article on the publisher site."
          },
          "symbols": {
            "type": "array",
            "description": "Ticker symbols mentioned in the article, in EODHD CODE.EXCHANGE form.",
            "items": {
              "type": "string",
              "description": "One ticker symbol, for example AAPL.US."
            }
          },
          "tags": {
            "type": "array",
            "description": "Topic tags assigned to the article, may be empty. Returned upper cased and hyphenated in the documented example.",
            "items": {
              "type": "string",
              "description": "One topic tag, for example ARTIFICIAL-INTELLIGENCE."
            }
          },
          "sentiment": {
            "type": "object",
            "description": "In house sentiment scores for the article.",
            "properties": {
              "polarity": {
                "type": "number",
                "description": "Overall sentiment polarity of the article. The documentation does not state an explicit range; the documented example value is 0.987."
              },
              "neg": {
                "type": "number",
                "description": "Negative component of the sentiment. The documentation does not state an explicit range; documented example 0.007, and neg, neu and pos behave as proportions that sum to about 1."
              },
              "neu": {
                "type": "number",
                "description": "Neutral component of the sentiment. The documentation does not state an explicit range; documented example 0.89."
              },
              "pos": {
                "type": "number",
                "description": "Positive component of the sentiment. The documentation does not state an explicit range; documented example 0.103."
              }
            }
          }
        }
      }
    },
    "request_example": {
      "s": "AAPL.US",
      "t": "artificial intelligence",
      "from": "2026-06-01",
      "to": "2026-06-30",
      "limit": 10,
      "offset": 0,
      "fmt": "json"
    },
    "response_example": "[{\"date\": \"2026-07-15T10:37:18+00:00\", \"title\": \"Alibaba shares jump as China clears Apple's Qwen-powered AI system\", \"content\": \"Investing.com -- Shares of Alibaba Group in the U.S. rose around 5% in premarket trading ...\", \"link\": \"https://finance.yahoo.com/technology/ai/articles/alibaba-shares-jump-china-clears-103718143.html\", \"symbols\": [\"2RR.F\", \"9988.HK\", \"AAPL.US\"], \"tags\": [\"ARTIFICIAL-INTELLIGENCE\", \"CHINA-MARKET\", \"REGULATION\", \"TECH\"], \"sentiment\": {\"polarity\": 0.987, \"neg\": 0.007, \"neu\": 0.89, \"pos\": 0.103}}]",
    "tags": [
      "news",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Return financial news articles for a ticker or topic tag, each with in house sentiment scores."
    ],
    "input_entities": [
      "s",
      "t",
      "from",
      "to",
      "limit",
      "offset",
      "fmt"
    ],
    "output_entities": [
      "news"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "Return financial news articles for a ticker or topic tag, each with in house sentiment scores.",
    "external_docs_url": "https://eodhd.com/financial-apis/stock-market-financial-news-api",
    "documentation_markdown": "GET /api/news — Return financial news articles for a ticker or topic tag, each with in house sentiment scores.\n\nReturns an array of financial news articles aggregated from major financial news portals, with the full article body, the tickers mentioned and an in house sentiment object. Filter by ticker with s, by topic tag with t, and by publication date with from and to; when both s and t are omitted the endpoint returns the latest general news feed. Use limit and offset to page through results, up to 1000 articles per request. Good for building news feeds, event studies and sentiment driven signals.\n\nEODHD call weight: 5; required plans: standalone_package, all_in_one, eod_historical_data_all_world, eod_intraday_all_world_extended, fundamentals_data_feed, free.\n\nCall weight is 5 API calls per request plus 5 API calls per ticker, so a two ticker request costs 10. The documentation does not state how far back the news archive goes, nor the languages covered, the number of sources, or any deduplication rule, so do not assume a start year. Sentiment sub field ranges are not stated either; only the aggregated /api/sentiments normalized score is documented as running from -1 to 1. The demo token works for AAPL.US, TSLA.US, VTI.US, AMZN.US, BTC-USD.CC and EURUSD.FOREX only. Tags in the response example come back upper cased and hyphenated while the t parameter is documented with lower case spaced values, so normalise before matching. No total count field is returned, so page with offset until fewer than limit articles come back.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "financial_news"
  },
  {
    "id": "e1686282-5d80-5178-b521-0ff5701c3d29",
    "method": "GET",
    "path": "/api/sentiments",
    "price": 0.026087,
    "description": "Return daily aggregated news sentiment scores per ticker, normalized from -1 to 1.",
    "query_params": {
      "type": "object",
      "properties": {
        "s": {
          "type": "string",
          "required": true,
          "x-parameter-location": "query",
          "description": "One or more comma separated tickers, for example AAPL.US,BTC-USD.CC."
        },
        "from": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Start date in YYYY-MM-DD format, inclusive."
        },
        "to": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "End date in YYYY-MM-DD format, inclusive."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format."
        }
      },
      "required": [
        "s"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.news_sentiment",
      "x-apiosk-call-weight": 5,
      "x-apiosk-provider-cost-eur": 0.00025,
      "x-apiosk-target-buyer-price-eur": 0.03,
      "x-apiosk-plans": [
        "standalone_package",
        "all_in_one",
        "eod_historical_data_all_world",
        "eod_intraday_all_world_extended",
        "fundamentals_data_feed",
        "free"
      ],
      "x-apiosk-free-plan": true,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "Sentiment data grouped by ticker symbol. Each key is a ticker in CODE.EXCHANGE form.",
      "additionalProperties": {
        "type": "array",
        "description": "Daily sentiment entries for one ticker.",
        "items": {
          "type": "object",
          "description": "One day of aggregated sentiment for the ticker.",
          "properties": {
            "date": {
              "type": "string",
              "format": "date",
              "description": "Aggregation date in YYYY-MM-DD format."
            },
            "count": {
              "type": "integer",
              "description": "Number of articles used for sentiment on that day."
            },
            "normalized": {
              "type": "number",
              "minimum": -1,
              "maximum": 1,
              "description": "Sentiment score from -1 (very negative) to 1 (very positive)."
            }
          }
        }
      }
    },
    "request_example": {
      "s": "AAPL.US,BTC-USD.CC",
      "from": "2026-06-01",
      "to": "2026-06-30",
      "fmt": "json"
    },
    "response_example": "{\"AAPL.US\": [{\"date\": \"2026-07-15\", \"count\": 10, \"normalized\": 0.6829}, {\"date\": \"2026-07-14\", \"count\": 64, \"normalized\": 0.7071}, {\"date\": \"2026-07-13\", \"count\": 64, \"normalized\": 0.6146}]}",
    "tags": [
      "sentiment",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Return daily aggregated news sentiment scores per ticker, normalized from -1 to 1."
    ],
    "input_entities": [
      "s",
      "from",
      "to",
      "fmt"
    ],
    "output_entities": [
      "sentiment"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "Return daily aggregated news sentiment scores per ticker, normalized from -1 to 1.",
    "external_docs_url": "https://eodhd.com/financial-apis/stock-market-financial-news-api",
    "documentation_markdown": "GET /api/sentiments — Return daily aggregated news sentiment scores per ticker, normalized from -1 to 1.\n\nReturns daily aggregated sentiment for one or more instruments (stocks, ETFs, Forex and crypto), calculated from news and social media coverage. The response is an object keyed by ticker, each holding an array of daily entries with the article count and a normalized score running from -1 (very negative) to 1 (very positive). Good for building sentiment time series and sentiment based trading signals without parsing article text yourself.\n\nEODHD call weight: 5; required plans: standalone_package, all_in_one, eod_historical_data_all_world, eod_intraday_all_world_extended, fundamentals_data_feed, free.\n\nThe normalized score is the only sentiment value with a documented range: -1 to 1. Call weight is 5 API calls per request plus 5 per ticker, so batching many tickers in one call does not save credits. Ticker codes are accepted in lower case and are returned normalized in the response keys. Days with no coverage are simply absent from the array rather than returned with count 0. The docs do not state a maximum number of tickers per request or how far back the series goes.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "news_sentiment"
  },
  {
    "id": "f9134663-ca03-52c8-99fc-97ac9fc7df0b",
    "method": "GET",
    "path": "/api/news-word-weights",
    "price": 0.043478,
    "description": "Return the most relevant words in news about a ticker over a date range, each with a weight.",
    "query_params": {
      "type": "object",
      "properties": {
        "s": {
          "type": "string",
          "required": true,
          "x-parameter-location": "query",
          "description": "Ticker to analyze, for example AAPL.US."
        },
        "filter[date_from]": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Start date in YYYY-MM-DD format, inclusive."
        },
        "filter[date_to]": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "End date in YYYY-MM-DD format, inclusive."
        },
        "page[limit]": {
          "type": "integer",
          "required": false,
          "x-parameter-location": "query",
          "description": "Number of top weighted words to return."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format."
        }
      },
      "required": [
        "s"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.news_word_weights",
      "x-apiosk-call-weight": 5,
      "x-apiosk-provider-cost-eur": 0.00025,
      "x-apiosk-target-buyer-price-eur": 0.05,
      "x-apiosk-plans": [
        "standalone_package",
        "all_in_one",
        "eod_historical_data_all_world",
        "eod_intraday_all_world_extended",
        "fundamentals_data_feed",
        "free"
      ],
      "x-apiosk-free-plan": true,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "Weighted word list with processing metadata and pagination link.",
      "properties": {
        "data": {
          "type": "object",
          "description": "Keywords with their corresponding weights, as key value pairs.",
          "additionalProperties": {
            "type": "number",
            "description": "Weight of this keyword across the processed articles."
          }
        },
        "meta": {
          "type": "object",
          "description": "Processing metadata for the request.",
          "properties": {
            "news_found": {
              "type": "integer",
              "description": "Total number of articles matched."
            },
            "news_processed": {
              "type": "integer",
              "description": "Number of articles actually processed."
            }
          }
        },
        "links": {
          "type": "object",
          "description": "Pagination links.",
          "properties": {
            "next": {
              "type": [
                "string",
                "null"
              ],
              "description": "URL to the next page of results, or null if there is none."
            }
          }
        }
      }
    },
    "request_example": {
      "s": "AAPL.US",
      "filter[date_from]": "2026-06-01",
      "filter[date_to]": "2026-06-15",
      "page[limit]": 10,
      "fmt": "json"
    },
    "response_example": "{\"data\": {\"stock\": 0.02227, \"apple\": 0.01224, \"companies\": 0.01029, \"year\": 0.0097, \"market\": 0.00945}, \"meta\": {\"news_processed\": 300, \"news_found\": 4126}, \"links\": {\"next\": null}}",
    "tags": [
      "news",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Return the most relevant words in news about a ticker over a date range, each with a weight."
    ],
    "input_entities": [
      "s",
      "filter[date_from]",
      "filter[date_to]",
      "page[limit]",
      "fmt"
    ],
    "output_entities": [
      "news"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "Return the most relevant words in news about a ticker over a date range, each with a weight.",
    "external_docs_url": "https://eodhd.com/financial-apis/stock-market-financial-news-api",
    "documentation_markdown": "GET /api/news-word-weights — Return the most relevant words in news about a ticker over a date range, each with a weight.\n\nReturns a weighted list of the most relevant words found in news articles about a given ticker over a date range. Each word is scored by frequency and significance across the processed articles, which makes it useful for trend analysis, NLP feature input and thematic clustering. This endpoint uses AI over hundreds or thousands of articles, so responses can be slow; narrow the date range if a request times out.\n\nEODHD call weight: 5; required plans: standalone_package, all_in_one, eod_historical_data_all_world, eod_intraday_all_world_extended, fundamentals_data_feed, free.\n\nThis endpoint is documented on the same page as the Financial News API and shares its plan list and its 5 API calls per request plus 5 per ticker weight. It uses JSON:API style bracket parameters (filter[date_from], filter[date_to], page[limit]) rather than the plain from, to and limit used by /api/news, so URL encode the brackets where your client requires it. news_processed can be much smaller than news_found, meaning weights are computed on a sample. There is no page[offset] documented; paginate by following links.next.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "news_word_weights"
  },
  {
    "id": "a860e7c0-de37-5406-a60b-3acd96c4e569",
    "method": "GET",
    "path": "/api/macro-indicator/{country}",
    "price": 0.013043,
    "description": "Return an annual World Bank macroeconomic indicator time series for one country or aggregate.",
    "query_params": {
      "type": "object",
      "properties": {
        "country": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "ISO 3166-1 alpha-3 country code, case insensitive (usa and USA are equivalent). World Bank aggregate codes are also accepted: WLD (World), EUU (European Union), EMU (Euro area), OED (OECD members), LCN (Latin America and Caribbean), SSF (Sub-Saharan Africa), EAS (East Asia and Pacific), MEA (Middle East and North Africa)."
        },
        "indicator": {
          "type": "string",
          "enum": [
            "gdp_current_usd",
            "gdp_per_capita_usd",
            "gdp_growth_annual",
            "gni_usd",
            "gni_per_capita_usd",
            "gni_ppp_usd",
            "gni_per_capita_ppp_usd",
            "gross_capital_formation_percent_gdp",
            "agriculture_value_added_percent_gdp",
            "industry_value_added_percent_gdp",
            "services_value_added_percent_gdp",
            "inflation_consumer_prices_annual",
            "consumer_price_index",
            "inflation_gdp_deflator_annual",
            "real_interest_rate",
            "net_trades_goods_services",
            "exports_of_goods_services_percent_gdp",
            "imports_of_goods_services_percent_gdp",
            "merchandise_trade_percent_gdp",
            "high_technology_exports_percent_total",
            "debt_percent_gdp",
            "revenue_excluding_grants_percent_gdp",
            "cash_surplus_deficit_percent_gdp",
            "total_debt_service_percent_gni",
            "population_total",
            "population_growth_annual",
            "net_migration",
            "life_expectancy",
            "fertility_rate",
            "prevalence_hiv_total",
            "unemployment_total_percent",
            "income_share_lowest_twenty",
            "poverty_poverty_lines_percent_population",
            "market_cap_domestic_companies_percent_gdp",
            "mobile_subscriptions_per_hundred",
            "internet_users_per_hundred",
            "startup_procedures_register",
            "co2_emissions_tons_per_capita",
            "surface_area_km"
          ],
          "default": "gdp_current_usd",
          "required": false,
          "x-parameter-location": "query",
          "description": "Indicator key. The documentation lists 39 keys with these human readable meanings: gdp_current_usd (GDP, current US$), gdp_per_capita_usd (GDP per capita, current US$), gdp_growth_annual (GDP growth, annual %), gni_usd (GNI, Atlas method, current US$), gni_per_capita_usd (GNI per capita, Atlas method, current US$), gni_ppp_usd (GNI, PPP, current international $, from 1990), gni_per_capita_ppp_usd (GNI per capita, PPP, from 1990), gross_capital_formation_percent_gdp, agriculture_value_added_percent_gdp, industry_value_added_percent_gdp, services_value_added_percent_gdp, inflation_consumer_prices_annual, consumer_price_index (2010 = 100), inflation_gdp_deflator_annual, real_interest_rate (stale, last update 2021), net_trades_goods_services, exports_of_goods_services_percent_gdp, imports_of_goods_services_percent_gdp, merchandise_trade_percent_gdp, high_technology_exports_percent_total (from 2007), debt_percent_gdp (limited country coverage), revenue_excluding_grants_percent_gdp, cash_surplus_deficit_percent_gdp (historical, last update 2014), total_debt_service_percent_gni (IBRD and IDA borrowers only), population_total, population_growth_annual, net_migration (5 year totals), life_expectancy, fertility_rate, prevalence_hiv_total, unemployment_total_percent, income_share_lowest_twenty, poverty_poverty_lines_percent_population, market_cap_domestic_companies_percent_gdp, mobile_subscriptions_per_hundred, internet_users_per_hundred (historical, last update 2015), startup_procedures_register (discontinued, final data point 2019), co2_emissions_tons_per_capita (stale, last update 2020) and surface_area_km."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format, json (default) or csv. Note that csv truncates decimal values to integers, so use json when you need full precision."
        }
      },
      "required": [
        "country"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.macro_indicator",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.015,
      "x-apiosk-plans": [
        "all_in_one",
        "fundamentals_data_feed"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "array",
      "description": "Array of annual observations, one per year, ordered from newest to oldest. Empty when there is no data.",
      "items": {
        "type": "object",
        "description": "One annual observation of the requested indicator.",
        "properties": {
          "CountryCode": {
            "type": "string",
            "description": "ISO 3166-1 alpha-3 country code, always returned in upper case regardless of the case used in the request."
          },
          "CountryName": {
            "type": "string",
            "description": "Country name in English, as published by the World Bank."
          },
          "Indicator": {
            "type": "string",
            "description": "Human readable indicator name, matching the World Bank WDI indicator name verbatim."
          },
          "Date": {
            "type": "string",
            "format": "date",
            "description": "End of period date in YYYY-MM-DD format, always 12-31 for annual data."
          },
          "Period": {
            "type": "string",
            "description": "Reporting period. Currently always Annual; no quarterly or monthly series are exposed."
          },
          "Value": {
            "type": [
              "number",
              "null"
            ],
            "description": "Indicator value. Units depend on the indicator and are reflected in the Indicator name (US$, %, count and so on)."
          }
        }
      }
    },
    "request_example": {
      "country": "FRA",
      "indicator": "inflation_consumer_prices_annual",
      "fmt": "json"
    },
    "response_example": "[{\"CountryCode\": \"FRA\", \"CountryName\": \"France\", \"Indicator\": \"Inflation, consumer prices (annual %)\", \"Date\": \"2024-12-31\", \"Period\": \"Annual\", \"Value\": 1.999}, {\"CountryCode\": \"FRA\", \"CountryName\": \"France\", \"Indicator\": \"Inflation, consumer prices (annual %)\", \"Date\": \"2023-12-31\", \"Period\": \"Annual\", \"Value\": 4.9296}]",
    "tags": [
      "macro",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Return an annual World Bank macroeconomic indicator time series for one country or aggregate."
    ],
    "input_entities": [
      "country",
      "indicator",
      "fmt"
    ],
    "output_entities": [
      "macro"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "Return an annual World Bank macroeconomic indicator time series for one country or aggregate.",
    "external_docs_url": "https://eodhd.com/financial-apis/macroeconomics-data-and-macro-indicators-api",
    "documentation_markdown": "GET /api/macro-indicator/{country} — Return an annual World Bank macroeconomic indicator time series for one country or aggregate.\n\nReturns the full annual history of a single macroeconomic indicator for one country, ordered from newest to oldest. The data comes from the World Bank World Development Indicators, so all series are annual and most start in 1960. 39 indicator keys are exposed covering national accounts, inflation, trade, government debt, demographics, labour, financial markets and environment. Good for country level screening, macro overlays and long horizon research, but not for high frequency macro releases.\n\nEODHD call weight: 10; required plans: all_in_one, fundamentals_data_feed.\n\nEach request costs 10 API calls, twice the price of a news request, which is high for what is republished World Bank data. Annual frequency only; for quarterly or monthly releases use /api/economic-events instead. An unknown country code returns HTTP 200 with an empty array, not a 404, so validate country codes before calling; an unrecognised indicator does return 404 with the plain text message 'Indicator or Country are Not Found.'. Expect the latest year to be one to two years behind the current calendar year. The documentation flags co2_emissions_tons_per_capita, internet_users_per_hundred and cash_surplus_deficit_percent_gdp as returning values that look out of range, and startup_procedures_register, cash_surplus_deficit_percent_gdp and internet_users_per_hundred as historical only (last updates 2019, 2014 and 2015). There is no parameter to request several indicators or several countries at once.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "macro_indicator"
  },
  {
    "id": "bd4f436e-742d-50f3-a979-c71e6290b4fb",
    "method": "GET",
    "path": "/api/economic-events",
    "price": 0.017391,
    "description": "Return the macroeconomic events calendar with actual, previous and estimated values.",
    "query_params": {
      "type": "object",
      "properties": {
        "from": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Start date in YYYY-MM-DD format."
        },
        "to": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "End date in YYYY-MM-DD format."
        },
        "country": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Country code in ISO 3166 format, 2 characters, for example US, GB, JP."
        },
        "comparison": {
          "type": "string",
          "enum": [
            "mom",
            "qoq",
            "yoy"
          ],
          "required": false,
          "x-parameter-location": "query",
          "description": "Comparison type: mom (month over month), qoq (quarter over quarter) or yoy (year over year)."
        },
        "type": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Event type filter, URL encoded, for example type=House%20Price%20Index."
        },
        "limit": {
          "type": "integer",
          "default": 50,
          "required": false,
          "x-parameter-location": "query",
          "description": "Number of results. Values 0 to 1000."
        },
        "offset": {
          "type": "integer",
          "default": 0,
          "required": false,
          "x-parameter-location": "query",
          "description": "Pagination offset. Values 0 to 1000."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format."
        }
      },
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.economic_events",
      "x-apiosk-call-weight": 1,
      "x-apiosk-provider-cost-eur": 5.0e-05,
      "x-apiosk-target-buyer-price-eur": 0.02,
      "x-apiosk-plans": [
        "all_in_one",
        "fundamentals_data_feed"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "array",
      "description": "Array of economic event objects.",
      "items": {
        "type": "object",
        "description": "One economic event or data release.",
        "properties": {
          "type": {
            "type": "string",
            "description": "Type of economic event, for example GDP Growth Rate or Nonfarm Payrolls."
          },
          "comparison": {
            "type": [
              "string",
              "null"
            ],
            "description": "Comparison type: mom, qoq, yoy, or null if not applicable.",
            "enum": [
              "mom",
              "qoq",
              "yoy",
              null
            ]
          },
          "period": {
            "type": [
              "string",
              "null"
            ],
            "description": "Period associated with the event, for example Q4, Jan or May, or null if not applicable."
          },
          "country": {
            "type": "string",
            "description": "Country code in ISO 3166 format, for example US or JP."
          },
          "date": {
            "type": "string",
            "description": "Date and time of the event in YYYY-MM-DD HH:MM:SS format. Not ISO 8601 and carries no timezone offset."
          },
          "actual": {
            "type": [
              "number",
              "null"
            ],
            "description": "Actual reported value."
          },
          "previous": {
            "type": [
              "number",
              "null"
            ],
            "description": "Previous value for the same event."
          },
          "estimate": {
            "type": [
              "number",
              "null"
            ],
            "description": "Estimated (forecast) value."
          },
          "change": {
            "type": [
              "number",
              "null"
            ],
            "description": "Change from previous value."
          },
          "change_percentage": {
            "type": [
              "number",
              "null"
            ],
            "description": "Percentage change from previous value."
          }
        }
      }
    },
    "request_example": {
      "from": "2025-03-01",
      "to": "2025-03-07",
      "country": "US",
      "comparison": "yoy",
      "type": "House Price Index",
      "limit": 50,
      "offset": 0,
      "fmt": "json"
    },
    "response_example": "[{\"type\": \"Capital Expenditure\", \"comparison\": \"yoy\", \"period\": \"Q4\", \"country\": \"JP\", \"date\": \"2025-03-03 23:50:00\", \"actual\": -0.2, \"previous\": 8.1, \"estimate\": 4.9, \"change\": -8.3, \"change_percentage\": -102.469}, {\"type\": \"Jobs/applications ratio\", \"comparison\": null, \"period\": \"Jan\", \"country\": \"JP\", \"date\": \"2025-03-03 23:30:00\", \"actual\": 1.26, \"previous\": 1.25, \"estimate\": 1.25, \"change\": 0.01, \"change_percentage\": 0.8}]",
    "tags": [
      "macro",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Return the macroeconomic events calendar with actual, previous and estimated values."
    ],
    "input_entities": [
      "from",
      "to",
      "country",
      "comparison",
      "type",
      "limit",
      "offset",
      "fmt"
    ],
    "output_entities": [
      "macro"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "Return the macroeconomic events calendar with actual, previous and estimated values.",
    "external_docs_url": "https://eodhd.com/financial-apis/economic-events-data-api",
    "documentation_markdown": "GET /api/economic-events — Return the macroeconomic events calendar with actual, previous and estimated values.\n\nReturns a calendar of macroeconomic events and releases such as GDP Growth Rate, Nonfarm Payrolls or House Price Index, with the actual reported value, the previous value, the consensus estimate and the change against the previous value. Filter by date range, country, comparison basis and event type, and page with limit and offset. Good for building an economic calendar, for event driven backtests and for surprise (actual minus estimate) signals.\n\nEODHD call weight: 1; required plans: all_in_one, fundamentals_data_feed.\n\nCall weight is only 1 API call per request, the cheapest endpoint in this group and ten times cheaper than the Macro Indicators API on the same plans. Note that country here is a 2 letter ISO 3166 code (US), while /api/macro-indicator takes a 3 letter code (USA). Both limit and offset are capped at 1000, so a single date range cannot be paged beyond offset 1000; narrow the from and to window instead. The demo token works on this endpoint. The documentation does not state how far back the calendar goes, whether future scheduled events are included, the unit of each value, or the list of valid type values, and there is no importance or impact field.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "economic_events"
  },
  {
    "id": "523ebe3a-6147-5495-adcc-815ce023fed9",
    "method": "GET",
    "path": "/api/mp/investverte/companies",
    "price": 0.013043,
    "description": "List the companies covered by the InvestVerte ESG dataset.",
    "query_params": {
      "type": "object",
      "properties": {
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The marketplace specification documents JSON responses only, so csv is not confirmed for this endpoint."
        }
      },
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.mp_esg_companies",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.015,
      "x-apiosk-plans": [
        "marketplace_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "array",
      "description": "List of covered companies.",
      "items": {
        "type": "object",
        "description": "One covered company.",
        "properties": {
          "symbol": {
            "type": "string",
            "description": "Company ticker symbol, for example AAPL or 000001.SZ."
          },
          "name": {
            "type": "string",
            "description": "Company name in English."
          }
        }
      }
    },
    "request_example": {
      "fmt": "json"
    },
    "response_example": "[{\"symbol\": \"AAPL\", \"name\": \"Apple Inc.\"}, {\"symbol\": \"MSFT\", \"name\": \"Microsoft Corporation\"}]",
    "tags": [
      "marketplace esg",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "List the companies covered by the InvestVerte ESG dataset."
    ],
    "input_entities": [
      "fmt"
    ],
    "output_entities": [
      "marketplace esg"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "List the companies covered by the InvestVerte ESG dataset.",
    "external_docs_url": "https://eodhd.com/marketplace/investverte/esg_data/docs",
    "documentation_markdown": "GET /api/mp/investverte/companies — List the companies covered by the InvestVerte ESG dataset.\n\nReturns the full list of companies for which InvestVerte publishes ESG ratings, as symbol and name pairs. Use it to discover valid symbols before calling the company ESG endpoint. The response is a plain array with no pagination parameters documented.\n\nEODHD call weight: 10; required plans: marketplace_subscription.\n\nMarketplace product with its own subscription, separate from the main EODHD plans. Limits are 100 000 API calls per 24 hours and 1000 requests per minute, and 1 request equals 10 API calls. For marketplace products the 24 hour window starts at the first request rather than at a fixed daily reset. Symbols here are bare tickers without the .EXCHANGE suffix used elsewhere in EODHD.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "mp_esg_companies"
  },
  {
    "id": "b109ce7b-79e5-5935-9809-fd13e41205e2",
    "method": "GET",
    "path": "/api/mp/investverte/countries",
    "price": 0.013043,
    "description": "List the countries covered by the InvestVerte ESG dataset.",
    "query_params": {
      "type": "object",
      "properties": {
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The marketplace specification documents JSON responses only, so csv is not confirmed for this endpoint."
        }
      },
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.mp_esg_countries",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.015,
      "x-apiosk-plans": [
        "marketplace_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "array",
      "description": "List of covered countries.",
      "items": {
        "type": "object",
        "description": "One covered country.",
        "properties": {
          "country_code": {
            "type": "string",
            "description": "Two letter country code, for example US or HK."
          },
          "country_descr": {
            "type": "string",
            "description": "Country name in English."
          }
        }
      }
    },
    "request_example": {
      "fmt": "json"
    },
    "response_example": "[{\"country_code\": \"US\", \"country_descr\": \"United States of America\"}, {\"country_code\": \"HK\", \"country_descr\": \"Hong Kong\"}]",
    "tags": [
      "marketplace esg",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "List the countries covered by the InvestVerte ESG dataset."
    ],
    "input_entities": [
      "fmt"
    ],
    "output_entities": [
      "marketplace esg"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "List the countries covered by the InvestVerte ESG dataset.",
    "external_docs_url": "https://eodhd.com/marketplace/investverte/esg_data/docs",
    "documentation_markdown": "GET /api/mp/investverte/countries — List the countries covered by the InvestVerte ESG dataset.\n\nReturns the list of countries for which aggregated ESG ratings are available, as two letter country code and English description pairs. Use it to discover valid symbols for the country ESG endpoint. No parameters other than authentication are documented.\n\nEODHD call weight: 10; required plans: marketplace_subscription.\n\nMarketplace product with its own subscription, separate from the main EODHD plans. Limits are 100 000 API calls per 24 hours and 1000 requests per minute, and 1 request equals 10 API calls. For marketplace products the 24 hour window starts at the first request rather than at a fixed daily reset.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "mp_esg_countries"
  },
  {
    "id": "a4d2554f-30f8-5bdf-ba79-43247541aa34",
    "method": "GET",
    "path": "/api/mp/investverte/country/{symbol}",
    "price": 0.026087,
    "description": "Return aggregated ESG scores for one country, by year and reporting frequency.",
    "query_params": {
      "type": "object",
      "properties": {
        "symbol": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "Country symbol, the two letter country code from the countries endpoint."
        },
        "year": {
          "type": "number",
          "required": false,
          "x-parameter-location": "query",
          "description": "Year of the data."
        },
        "frequency": {
          "type": "string",
          "enum": [
            "FY",
            "Q1",
            "Q2",
            "Q3",
            "Q4"
          ],
          "required": false,
          "x-parameter-location": "query",
          "description": "Frequency of the data reporting."
        },
        "model": {
          "type": "string",
          "enum": [
            "ai",
            "legacy"
          ],
          "default": "ai",
          "required": false,
          "x-parameter-location": "query",
          "description": "Scoring model to use. ai returns the AI based scores (default), legacy returns the previous dataset."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The marketplace specification documents JSON responses only, so csv is not confirmed for this endpoint."
        }
      },
      "required": [
        "symbol"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.mp_esg_country",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.03,
      "x-apiosk-plans": [
        "marketplace_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "array",
      "description": "Aggregated country ESG observations.",
      "items": {
        "type": "object",
        "description": "One country ESG observation for a year and frequency.",
        "properties": {
          "symbol": {
            "type": "string",
            "description": "Country symbol, for example US."
          },
          "name": {
            "type": "string",
            "description": "Country name in English."
          },
          "mean": {
            "type": "number",
            "description": "Mean ESG score across the covered companies in the country."
          },
          "median": {
            "type": "number",
            "description": "Median ESG score across the covered companies in the country."
          },
          "year": {
            "type": "number",
            "description": "Year of the observation."
          },
          "frequency": {
            "type": "string",
            "description": "Reporting frequency of the observation: FY, Q1, Q2, Q3 or Q4."
          }
        }
      }
    },
    "request_example": {
      "symbol": "US",
      "year": 2014,
      "frequency": "FY",
      "model": "ai",
      "fmt": "json"
    },
    "response_example": "[{\"symbol\": \"US\", \"name\": \"United States of America\", \"mean\": 63.23235093094508, \"median\": 64.24, \"year\": 2014, \"frequency\": \"FY\"}, {\"symbol\": \"US\", \"name\": \"United States of America\", \"mean\": 64.18487623762375, \"median\": 64.2, \"year\": 2014, \"frequency\": \"Q1\"}]",
    "tags": [
      "marketplace esg",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Return aggregated ESG scores for one country, by year and reporting frequency."
    ],
    "input_entities": [
      "symbol",
      "year",
      "frequency",
      "model",
      "fmt"
    ],
    "output_entities": [
      "marketplace esg"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "Return aggregated ESG scores for one country, by year and reporting frequency.",
    "external_docs_url": "https://eodhd.com/marketplace/investverte/esg_data/docs",
    "documentation_markdown": "GET /api/mp/investverte/country/{symbol} — Return aggregated ESG scores for one country, by year and reporting frequency.\n\nReturns the mean and median ESG score for all covered companies in a country, split by year and reporting frequency (full year and quarters). Use it as a benchmark against which individual company ESG scores can be compared. Scores come from the AI model by default; pass model=legacy for the previous dataset.\n\nEODHD call weight: 10; required plans: marketplace_subscription.\n\nMarketplace product with its own subscription, separate from the main EODHD plans. Limits are 100 000 API calls per 24 hours and 1000 requests per minute, and 1 request equals 10 API calls. For marketplace products the 24 hour window starts at the first request rather than at a fixed daily reset. The documentation does not state the numeric range of the mean and median scores; the examples sit in a 0 to 100 style band. Omitting year and frequency returns the available history.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "mp_esg_country"
  },
  {
    "id": "bde61d09-5151-59cf-abb7-e9f663415964",
    "method": "GET",
    "path": "/api/mp/investverte/sectors",
    "price": 0.013043,
    "description": "List the sectors covered by the InvestVerte ESG dataset.",
    "query_params": {
      "type": "object",
      "properties": {
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The marketplace specification documents JSON responses only, so csv is not confirmed for this endpoint."
        }
      },
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.mp_esg_sectors",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.015,
      "x-apiosk-plans": [
        "marketplace_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "array",
      "description": "List of covered sectors.",
      "items": {
        "type": "object",
        "description": "One covered sector.",
        "properties": {
          "sector": {
            "type": "string",
            "description": "Sector name in English."
          }
        }
      }
    },
    "request_example": {
      "fmt": "json"
    },
    "response_example": "[{\"sector\": \"Aerospace & Defense\"}, {\"sector\": \"Airlines\"}]",
    "tags": [
      "marketplace esg",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "List the sectors covered by the InvestVerte ESG dataset."
    ],
    "input_entities": [
      "fmt"
    ],
    "output_entities": [
      "marketplace esg"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "List the sectors covered by the InvestVerte ESG dataset.",
    "external_docs_url": "https://eodhd.com/marketplace/investverte/esg_data/docs",
    "documentation_markdown": "GET /api/mp/investverte/sectors — List the sectors covered by the InvestVerte ESG dataset.\n\nReturns the list of sector names for which aggregated ESG data is available, for example Aerospace & Defense or Airlines. Use it to discover valid symbols for the sector ESG endpoint. No parameters other than authentication are documented.\n\nEODHD call weight: 10; required plans: marketplace_subscription.\n\nMarketplace product with its own subscription, separate from the main EODHD plans. Limits are 100 000 API calls per 24 hours and 1000 requests per minute, and 1 request equals 10 API calls. For marketplace products the 24 hour window starts at the first request rather than at a fixed daily reset.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "mp_esg_sectors"
  },
  {
    "id": "6a21f1d2-584c-56b2-921f-2be804c12a4f",
    "method": "GET",
    "path": "/api/mp/investverte/sector/{symbol}",
    "price": 0.026087,
    "description": "Return an ESG score time series for one sector and its parent industries.",
    "query_params": {
      "type": "object",
      "properties": {
        "symbol": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "Sector name from the sectors endpoint."
        },
        "model": {
          "type": "string",
          "enum": [
            "ai",
            "legacy"
          ],
          "default": "ai",
          "required": false,
          "x-parameter-location": "query",
          "description": "Scoring model to use. ai returns the AI based scores (default), legacy returns the previous dataset."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The marketplace specification documents JSON responses only, so csv is not confirmed for this endpoint."
        }
      },
      "required": [
        "symbol"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.mp_esg_sector",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.03,
      "x-apiosk-plans": [
        "marketplace_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "Sector ESG series with aligned period labels.",
      "properties": {
        "find": {
          "type": "boolean",
          "description": "Whether the requested sector was found."
        },
        "industry": {
          "type": "object",
          "description": "ESG score series keyed by industry name. Each array is positionally aligned with the years array.",
          "additionalProperties": {
            "type": "array",
            "description": "ESG scores for that industry over the periods in years.",
            "items": {
              "type": [
                "number",
                "null"
              ],
              "description": "ESG score for one period, or null when unavailable."
            }
          }
        },
        "years": {
          "type": "array",
          "description": "Period labels aligned with each industry series.",
          "items": {
            "type": "string",
            "description": "Period label combining year and frequency, for example 2014-FY."
          }
        }
      }
    },
    "request_example": {
      "symbol": "Airlines",
      "model": "ai",
      "fmt": "json"
    },
    "response_example": "{\"find\": true, \"industry\": {\"Airlines\": [77.46, 77.46, 77.46], \"Transportation\": [null, null, null]}, \"years\": [\"2014-FY\", \"2014-Q1\", \"2014-Q2\"]}",
    "tags": [
      "marketplace esg",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Return an ESG score time series for one sector and its parent industries."
    ],
    "input_entities": [
      "symbol",
      "model",
      "fmt"
    ],
    "output_entities": [
      "marketplace esg"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "Return an ESG score time series for one sector and its parent industries.",
    "external_docs_url": "https://eodhd.com/marketplace/investverte/esg_data/docs",
    "documentation_markdown": "GET /api/mp/investverte/sector/{symbol} — Return an ESG score time series for one sector and its parent industries.\n\nReturns ESG score series for a sector, keyed by industry name, together with a parallel years array that labels each position in those series. The years labels combine year and frequency, for example 2014-FY or 2014-Q1. Use it to benchmark company scores against their sector over time.\n\nEODHD call weight: 10; required plans: marketplace_subscription.\n\nMarketplace product with its own subscription, separate from the main EODHD plans. Limits are 100 000 API calls per 24 hours and 1000 requests per minute, and 1 request equals 10 API calls. For marketplace products the 24 hour window starts at the first request rather than at a fixed daily reset. This is the only ESG endpoint that returns a column oriented shape, so join industry arrays to years by index. Sector names contain spaces and ampersands and must be URL encoded.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "mp_esg_sector"
  },
  {
    "id": "ac4959c0-dbb2-58c9-be02-b5b55993e7fb",
    "method": "GET",
    "path": "/api/mp/unicornbay/tickdata/ticks",
    "price": 0.043478,
    "description": "Return tick by tick trade data for a US stock ticker over a Unix timestamp range.",
    "query_params": {
      "type": "object",
      "properties": {
        "s": {
          "type": "string",
          "required": true,
          "x-parameter-location": "query",
          "description": "Ticker symbol, maximum 30 characters, for example AAPL, MSFT or GOOGL."
        },
        "from": {
          "type": "integer",
          "required": false,
          "x-parameter-location": "query",
          "description": "Start timestamp as a Unix timestamp in seconds. Defaults to the start of yesterday if not provided."
        },
        "to": {
          "type": "integer",
          "required": false,
          "x-parameter-location": "query",
          "description": "End timestamp as a Unix timestamp in seconds. Defaults to the end of yesterday if not provided."
        },
        "limit": {
          "type": "integer",
          "required": false,
          "x-parameter-location": "query",
          "description": "Maximum number of ticks to return, 1 to 10000. If omitted or 0, all ticks in the time range are returned."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The marketplace specification documents JSON responses only, so csv is not confirmed for this endpoint."
        }
      },
      "required": [
        "s"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.mp_tickdata_ticks",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.05,
      "x-apiosk-plans": [
        "marketplace_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "Column oriented tick data. All arrays have the same length and are aligned by index.",
      "properties": {
        "ts": {
          "type": "array",
          "description": "Timestamps in milliseconds (Unix timestamp) for each tick.",
          "items": {
            "type": "integer",
            "description": "Tick timestamp in milliseconds since the Unix epoch."
          }
        },
        "price": {
          "type": "array",
          "description": "Trade execution prices.",
          "items": {
            "type": "number",
            "description": "Execution price of one tick."
          }
        },
        "shares": {
          "type": "array",
          "description": "Number of shares traded in each tick.",
          "items": {
            "type": "integer",
            "description": "Share count of one tick."
          }
        },
        "mkt": {
          "type": "array",
          "description": "Market identifiers for each tick.",
          "items": {
            "type": "string",
            "description": "Market identifier code, for example K or Q."
          }
        },
        "seq": {
          "type": "array",
          "description": "Sequence numbers for each tick.",
          "items": {
            "type": "integer",
            "description": "Sequence number of one tick."
          }
        },
        "sl": {
          "type": "array",
          "description": "Source or location identifiers.",
          "items": {
            "type": "string",
            "description": "Source or location identifier of one tick."
          }
        },
        "sub_mkt": {
          "type": "array",
          "description": "Sub market identifiers.",
          "items": {
            "type": "string",
            "description": "Sub market identifier of one tick, often an empty string."
          }
        }
      }
    },
    "request_example": {
      "s": "AAPL",
      "from": 1694077200,
      "to": 1694080800,
      "limit": 100,
      "fmt": "json"
    },
    "response_example": "{\"mkt\": [\"K\", \"Q\", \"Q\"], \"price\": [177.88, 177.95, 177.97], \"seq\": [1434370, 1436393, 1436394], \"shares\": [1, 50, 50], \"sl\": [\"@ TI\", \"@ TI\", \"@ TI\"], \"sub_mkt\": [\"\", \"\", \"\"], \"ts\": [1694077201147, 1694077206102, 1694077206102]}",
    "tags": [
      "marketplace alternative data",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Return tick by tick trade data for a US stock ticker over a Unix timestamp range."
    ],
    "input_entities": [
      "s",
      "from",
      "to",
      "limit",
      "fmt"
    ],
    "output_entities": [
      "marketplace alternative data"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "Return tick by tick trade data for a US stock ticker over a Unix timestamp range.",
    "external_docs_url": "https://eodhd.com/marketplace/unicornbay/tickdata/docs",
    "documentation_markdown": "GET /api/mp/unicornbay/tickdata/ticks — Return tick by tick trade data for a US stock ticker over a Unix timestamp range.\n\nReturns individual trade records for a US stock with millisecond timestamps, execution price, share count, market and sub market identifiers and a sequence number. The response is column oriented: each field is a parallel array and the arrays are aligned by index. Intended for market microstructure research, high frequency backtesting and volume profile work.\n\nEODHD call weight: 10; required plans: marketplace_subscription.\n\nMarketplace product with its own subscription, separate from the main EODHD plans. Limits are 100 000 API calls per 24 hours and 1000 requests per minute, and 1 request equals 10 API calls. For marketplace products the 24 hour window starts at the first request rather than at a fixed daily reset. Unusual for EODHD: from and to are Unix timestamps in seconds, while ts in the response is in milliseconds. The response is column oriented rather than a list of objects, so zip the arrays by index. The documentation does not explain the meaning of the mkt, sl and sub_mkt code values, nor how far back the tick history goes.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "mp_tickdata_ticks"
  },
  {
    "id": "4cdaecac-d972-5c7b-8f8f-90e28bfaf6f5",
    "method": "GET",
    "path": "/api/mp/unicornbay/spglobal/list",
    "price": 0.026087,
    "description": "List 100+ S&P and Dow Jones indices with end of day value, market cap and divisor.",
    "query_params": {
      "type": "object",
      "properties": {
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The marketplace specification documents JSON responses only, so csv is not confirmed for this endpoint."
        }
      },
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.mp_spglobal_list",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.03,
      "x-apiosk-plans": [
        "marketplace_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "array",
      "description": "List of indices with end of day details.",
      "items": {
        "type": "object",
        "description": "One index with its end of day details.",
        "properties": {
          "ID": {
            "type": "string",
            "description": "Unique EODHD index identifier, for example GSPC.INDX."
          },
          "Code": {
            "type": "string",
            "description": "Asset code without the exchange suffix, for example GSPC."
          },
          "Name": {
            "type": "string",
            "description": "Index name in English."
          },
          "Constituents": {
            "type": "number",
            "description": "Current number of constituents in the index."
          },
          "LastUpdate": {
            "type": "string",
            "format": "date",
            "description": "Last update date in ISO 8601 format."
          },
          "Value": {
            "type": "number",
            "description": "Index value."
          },
          "MarketCap": {
            "type": "number",
            "description": "Market capitalization of the index."
          },
          "Divisor": {
            "type": "number",
            "description": "Index divisor."
          },
          "DailyReturn": {
            "type": "string",
            "description": "Index daily return value. Typed as string in the specification even though it carries a number, for example -0.0043."
          },
          "Dividend": {
            "type": "number",
            "description": "Current index dividend."
          },
          "AdjustedMarketCap": {
            "type": "number",
            "description": "Market capitalization, adjusted."
          },
          "AdjustedDivisor": {
            "type": "number",
            "description": "Index divisor, adjusted."
          },
          "AdjustedConstituents": {
            "type": "number",
            "description": "Adjusted number of constituents in the index."
          }
        }
      }
    },
    "request_example": {
      "fmt": "json"
    },
    "response_example": "[{\"ID\": \"GSPC.INDX\", \"Code\": \"GSPC\", \"Name\": \"S&P 500\", \"Constituents\": 502, \"LastUpdate\": \"2024-01-01\", \"Value\": 21947.6275, \"MarketCap\": 9953830971.3261, \"Divisor\": 453526.5125, \"DailyReturn\": \"-0.0043\", \"Dividend\": 0.9391, \"AdjustedMarketCap\": 9953830971.3261, \"AdjustedDivisor\": 453526.5125, \"AdjustedConstituents\": 502}]",
    "tags": [
      "marketplace index data",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "List 100+ S&P and Dow Jones indices with end of day value, market cap and divisor."
    ],
    "input_entities": [
      "fmt"
    ],
    "output_entities": [
      "marketplace index data"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "List 100+ S&P and Dow Jones indices with end of day value, market cap and divisor.",
    "external_docs_url": "https://eodhd.com/marketplace/unicornbay/spglobal/docs",
    "documentation_markdown": "GET /api/mp/unicornbay/spglobal/list — List 100+ S&P and Dow Jones indices with end of day value, market cap and divisor.\n\nReturns end of day details for more than 100 global S&P and Dow Jones indices, including the S&P 500, 600, 100, 400 and about 21 key industry indices. Each row carries the index value, market capitalisation, divisor, daily return, dividend and the adjusted variants, plus the current constituent count. Use it to discover the index symbols accepted by the components endpoint.\n\nEODHD call weight: 10; required plans: marketplace_subscription.\n\nMarketplace product with its own subscription, separate from the main EODHD plans. Limits are 100 000 API calls per 24 hours and 1000 requests per minute, and 1 request equals 10 API calls. For marketplace products the 24 hour window starts at the first request rather than at a fixed daily reset. DailyReturn is declared as a string in the specification while every other numeric field is a number, so parse it defensively. No filter or pagination parameters are documented.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "mp_spglobal_list"
  },
  {
    "id": "31c7874e-26ff-58a6-a819-aaab1cbfea1b",
    "method": "GET",
    "path": "/api/mp/unicornbay/spglobal/comp/{symbol}",
    "price": 0.043478,
    "description": "Return current and historical constituents of one S&P or Dow Jones index.",
    "query_params": {
      "type": "object",
      "properties": {
        "symbol": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "Index symbol, as returned by the list endpoint."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The marketplace specification documents JSON responses only, so csv is not confirmed for this endpoint."
        }
      },
      "required": [
        "symbol"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.mp_spglobal_components",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.05,
      "x-apiosk-plans": [
        "marketplace_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "Index fundamentals with current components and historical membership records.",
      "properties": {
        "General": {
          "type": "object",
          "description": "Index ticker fundamental data.",
          "properties": {
            "Code": {
              "type": "string",
              "description": "Index code."
            },
            "Type": {
              "type": "string",
              "description": "Asset type, INDEX."
            },
            "Name": {
              "type": "string",
              "description": "Index name in English."
            },
            "Exchange": {
              "type": "string",
              "description": "Exchange code, INDX."
            },
            "MarketCap": {
              "type": "number",
              "description": "Market capitalization of the index."
            },
            "CurrencyCode": {
              "type": "string",
              "description": "Currency code, for example USD."
            },
            "CurrencyName": {
              "type": "string",
              "description": "Currency name in English."
            },
            "CurrencySymbol": {
              "type": "string",
              "description": "Currency symbol, for example $."
            },
            "CountryName": {
              "type": "string",
              "description": "Country name, or Unknown for global indices."
            },
            "CountryISO": {
              "type": "string",
              "description": "ISO country code, or NA when unknown."
            },
            "OpenFigi": {
              "type": [
                "string",
                "null"
              ],
              "description": "OpenFIGI identifier, or null."
            }
          }
        },
        "Components": {
          "type": "object",
          "description": "Current index components, keyed by a stringified running index such as \"0\", \"1\".",
          "additionalProperties": {
            "type": "object",
            "description": "One current component.",
            "properties": {
              "Code": {
                "type": "string",
                "description": "Component ticker code, for example NEM."
              },
              "Exchange": {
                "type": "string",
                "description": "Exchange code of the component, for example US."
              },
              "Name": {
                "type": "string",
                "description": "Company name in English."
              },
              "Sector": {
                "type": [
                  "string",
                  "null"
                ],
                "description": "Sector of the component, or null."
              },
              "Industry": {
                "type": [
                  "string",
                  "null"
                ],
                "description": "Industry of the component, or null."
              },
              "Weight": {
                "type": [
                  "number",
                  "null"
                ],
                "description": "Weight of the component in the index, or null."
              }
            }
          }
        },
        "HistoricalTickerComponents": {
          "type": "object",
          "description": "Historical membership records, keyed by a stringified running index such as \"0\", \"1\".",
          "additionalProperties": {
            "type": "object",
            "description": "One historical membership record.",
            "properties": {
              "Code": {
                "type": "string",
                "description": "Component ticker code, for example AA."
              },
              "Name": {
                "type": "string",
                "description": "Company name in English."
              },
              "StartDate": {
                "type": "string",
                "format": "date",
                "description": "Date of ticker addition to the index."
              },
              "EndDate": {
                "type": "string",
                "format": "date",
                "description": "Date of ticker removal from the index."
              },
              "IsActiveNow": {
                "type": "number",
                "enum": [
                  0,
                  1
                ],
                "description": "1 when the company is still part of the index, 0 otherwise."
              },
              "IsDelisted": {
                "type": "number",
                "enum": [
                  0,
                  1
                ],
                "description": "1 when the company is no longer traded at all, 0 otherwise."
              }
            }
          }
        }
      }
    },
    "request_example": {
      "symbol": "GSPC.INDX",
      "fmt": "json"
    },
    "response_example": "{\"General\": {\"Code\": \"SPSIMM\", \"Type\": \"INDEX\", \"Name\": \"S&P Metals and Mining Select Industry Index\", \"Exchange\": \"INDX\", \"MarketCap\": 9984236890.9408, \"CurrencyCode\": \"USD\", \"CurrencyName\": \"US Dollar\", \"CurrencySymbol\": \"$\", \"CountryName\": \"Unknown\", \"CountryISO\": \"NA\", \"OpenFigi\": null}, \"Components\": {\"0\": {\"Code\": \"NEM\", \"Exchange\": \"US\", \"Name\": \"Newmont Goldcorp Corp\", \"Sector\": null, \"Industry\": null, \"Weight\": 0.0561}}, \"HistoricalTickerComponents\": {\"0\": {\"Code\": \"AA\", \"Name\": \"Alcoa Corp\", \"StartDate\": \"2023-09-18\", \"EndDate\": \"2024-02-02\", \"IsActiveNow\": 1, \"IsDelisted\": 0}}}",
    "tags": [
      "marketplace index data",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Return current and historical constituents of one S&P or Dow Jones index."
    ],
    "input_entities": [
      "symbol",
      "fmt"
    ],
    "output_entities": [
      "marketplace index data"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "Return current and historical constituents of one S&P or Dow Jones index.",
    "external_docs_url": "https://eodhd.com/marketplace/unicornbay/spglobal/docs",
    "documentation_markdown": "GET /api/mp/unicornbay/spglobal/comp/{symbol} — Return current and historical constituents of one S&P or Dow Jones index.\n\nReturns the current component list of an index plus, for about 30 major S&P and Dow Jones indices, 2 to 12 years of historical membership changes with the start and end date of every addition and removal. Each historical record is flagged with IsActiveNow (still in the index) and IsDelisted (still traded at all). This is the dataset to use for survivorship bias free index backtests.\n\nEODHD call weight: 10; required plans: marketplace_subscription.\n\nMarketplace product with its own subscription, separate from the main EODHD plans. Limits are 100 000 API calls per 24 hours and 1000 requests per minute, and 1 request equals 10 API calls. For marketplace products the 24 hour window starts at the first request rather than at a fixed daily reset. Components and HistoricalTickerComponents are objects keyed by stringified integers, not arrays, so iterate over values rather than indexing. The constituent count for a date can differ by a few names from the index headline size because multiple share classes count as separate listings and corporate actions create transitional periods; the S&P 500 typically returns 500 to 505. For minor indices data completeness varies and historical changes may be absent.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "mp_spglobal_components"
  },
  {
    "id": "0ce00d5c-93e1-5657-b07b-c5f8970e0393",
    "method": "GET",
    "path": "/api/logo/{symbol}",
    "price": 0.017391,
    "description": "Return the company logo for a ticker as a 200x200 transparent PNG image.",
    "query_params": {
      "type": "object",
      "properties": {
        "symbol": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "Ticker in {ticker}.{exchange} form, for example AAPL.US. Available exchanges include AS, AT, AU, BA, BK, BR, BSE, CN, CO, CSE, DU, F, HE, HK, HM, IC, IR, IS, JK, JSE, KLSE, KO, KQ, LS, LSE, MC, MCX, MI, MU, MX, NEO, NSE, NZ, OL, PA, RG, SA, SG, SHE, SHG, SN, SR, ST, STU, SW, TA, TO, TSE, TW, TWO, US, V, VI, VS, VX and XETRA."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The marketplace specification documents JSON responses only, so csv is not confirmed for this endpoint."
        }
      },
      "required": [
        "symbol"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.mp_logo_png",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.02,
      "x-apiosk-plans": [
        "marketplace_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "string",
      "contentEncoding": "binary",
      "contentMediaType": "image/png",
      "description": "Binary PNG image data, a 200x200 pixel logo with transparency. Not a JSON body."
    },
    "request_example": {
      "symbol": "AAPL.US",
      "fmt": "json"
    },
    "response_example": null,
    "tags": [
      "marketplace alternative data",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Return the company logo for a ticker as a 200x200 transparent PNG image."
    ],
    "input_entities": [
      "symbol",
      "fmt"
    ],
    "output_entities": [
      "marketplace alternative data"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "Return the company logo for a ticker as a 200x200 transparent PNG image.",
    "external_docs_url": "https://eodhd.com/marketplace/unicornbay/logo/docs",
    "documentation_markdown": "GET /api/logo/{symbol} — Return the company logo for a ticker as a 200x200 transparent PNG image.\n\nReturns the company logo image for a ticker in CODE.EXCHANGE form. The collection holds around 40 000 logos across 60+ exchanges as 200x200 pixel PNG files with transparency. The response body is binary image data, not JSON, so fmt has no effect here. A CSV or XLSX list of all available tickers is published alongside the documentation.\n\nEODHD call weight: 10; required plans: marketplace_subscription.\n\nMarketplace product with its own subscription, separate from the main EODHD plans. Limits are 100 000 API calls per 24 hours and 1000 requests per minute, and 1 request equals 10 API calls. For marketplace products the 24 hour window starts at the first request rather than at a fixed daily reset. This endpoint lives at /api/logo/{symbol}, not under /api/mp/, unlike the other marketplace products. The response is an image, so fmt and JSON parsing do not apply and a 404 is returned when no logo exists for the ticker. The docs show a working example with api_token=demo.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "mp_logo_png"
  },
  {
    "id": "90011a63-1ad8-57b1-a1c5-1091bba1f9ba",
    "method": "GET",
    "path": "/api/logo-svg/{symbol}",
    "price": 0.017391,
    "description": "Return the company logo for a US or Toronto ticker as a scalable SVG image.",
    "query_params": {
      "type": "object",
      "properties": {
        "symbol": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "Ticker in {ticker}.{exchange} form, for example AAPL.US. SVG format is only available for the US and TO exchanges."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The marketplace specification documents JSON responses only, so csv is not confirmed for this endpoint."
        }
      },
      "required": [
        "symbol"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.mp_logo_svg",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.02,
      "x-apiosk-plans": [
        "marketplace_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "string",
      "contentEncoding": "binary",
      "contentMediaType": "image/svg+xml",
      "description": "Binary SVG image data. Not a JSON body."
    },
    "request_example": {
      "symbol": "AAPL.US",
      "fmt": "json"
    },
    "response_example": null,
    "tags": [
      "marketplace alternative data",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Return the company logo for a US or Toronto ticker as a scalable SVG image."
    ],
    "input_entities": [
      "symbol",
      "fmt"
    ],
    "output_entities": [
      "marketplace alternative data"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "Return the company logo for a US or Toronto ticker as a scalable SVG image.",
    "external_docs_url": "https://eodhd.com/marketplace/unicornbay/logo-svg/docs",
    "documentation_markdown": "GET /api/logo-svg/{symbol} — Return the company logo for a US or Toronto ticker as a scalable SVG image.\n\nReturns the company logo in SVG format for a ticker in CODE.EXCHANGE form. SVG is only available for the US and TO (Toronto) exchanges; every other exchange is PNG only through /api/logo/{symbol}. Use it for high resolution displays and print. The response body is binary image data, not JSON.\n\nEODHD call weight: 10; required plans: marketplace_subscription.\n\nMarketplace product with its own subscription, separate from the main EODHD plans. Limits are 100 000 API calls per 24 hours and 1000 requests per minute, and 1 request equals 10 API calls. For marketplace products the 24 hour window starts at the first request rather than at a fixed daily reset. Sold as an extension of the PNG logos product; the SVG extension spec documents both /api/logo/{symbol} and /api/logo-svg/{symbol}. Coverage is limited to US and TO, so fall back to the PNG endpoint for other exchanges. A separate XLSX list of SVG capable tickers is published with the documentation.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "mp_logo_svg"
  },
  {
    "id": "9f6b3654-77b0-595e-a6fc-249be3d2a361",
    "method": "GET",
    "path": "/api/mp/tradinghours/markets",
    "price": 0.026087,
    "description": "List all trading venues available to your tier with their FinID and MIC.",
    "query_params": {
      "type": "object",
      "properties": {
        "group": {
          "type": "string",
          "enum": [
            "core",
            "extended",
            "all",
            "allowed"
          ],
          "default": "all",
          "required": false,
          "x-parameter-location": "query",
          "description": "Which group of markets to show. core is the G20+ tier, extended is Global Equities, all is Global Equities and Derivatives, and allowed restricts the result to what your subscription covers."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The marketplace specification documents JSON responses only, so csv is not confirmed for this endpoint."
        }
      },
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.mp_tradinghours_markets",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.03,
      "x-apiosk-plans": [
        "marketplace_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "array",
      "description": "List of markets.",
      "items": {
        "type": "object",
        "description": "One market or trading schedule.",
        "properties": {
          "fin_id": {
            "type": "string",
            "description": "TradingHours FinID, the unique identifier of one distinct trading schedule, for example AE.ADX."
          },
          "exchange": {
            "type": "string",
            "description": "Exchange name in English."
          },
          "market": {
            "type": "string",
            "description": "Market or trading venue name in English."
          },
          "products": {
            "type": [
              "string",
              "null"
            ],
            "description": "Products or securities traded on this schedule, or null."
          },
          "mic": {
            "type": "string",
            "description": "ISO 10383 segment MIC of the market, for example XADS."
          },
          "asset_type": {
            "type": [
              "string",
              "null"
            ],
            "description": "Asset type traded, for example Securities, or null."
          },
          "group": {
            "type": "string",
            "description": "Access tier this market belongs to, for example Core or Extended."
          }
        }
      }
    },
    "request_example": {
      "group": "allowed",
      "fmt": "json"
    },
    "response_example": "[{\"fin_id\": \"AE.ADX\", \"exchange\": \"Abu Dhabi Securities Exchange\", \"market\": \"Abu Dhabi Securities Exchange (ADX)\", \"products\": null, \"mic\": \"XADS\", \"asset_type\": \"Securities\", \"group\": \"Extended\"}]",
    "tags": [
      "marketplace market status",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "List all trading venues available to your tier with their FinID and MIC."
    ],
    "input_entities": [
      "group",
      "fmt"
    ],
    "output_entities": [
      "marketplace market status"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "List all trading venues available to your tier with their FinID and MIC.",
    "external_docs_url": "https://eodhd.com/marketplace/tradinghours/markets-g20/docs",
    "documentation_markdown": "GET /api/mp/tradinghours/markets — List all trading venues available to your tier with their FinID and MIC.\n\nReturns every market covered by the requested access tier, each with the TradingHours FinID that uniquely identifies one distinct trading schedule, the exchange and market names, the segment MIC and the asset type. Start here to resolve a FinID before calling the details or status endpoints. A MIC can be used in place of a FinID and the system will pick the closest matching schedule.\n\nEODHD call weight: 10; required plans: marketplace_subscription.\n\nMarketplace product with its own subscription, separate from the main EODHD plans. Limits are 100 000 API calls per 24 hours and 1000 requests per minute, and 1 request equals 10 API calls. For marketplace products the 24 hour window starts at the first request rather than at a fixed daily reset. Sold in three tiers that share the same paths: G20+ Markets (core, 24 markets), Global Equities (extended, 148 equity markets) and Global Equities & Derivatives (all, 900+ markets). Your token is limited to the tier you bought, and group=allowed returns exactly what your subscription covers. The specification declares the response as a bare array while the example shows the same rows wrapped in a data object, so handle both shapes. MICs alone are usually not granular enough to identify a distinct trading schedule, which is why FinIDs exist.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "mp_tradinghours_markets"
  },
  {
    "id": "30e7f7a4-1b04-52ad-a459-53ff653d05f7",
    "method": "GET",
    "path": "/api/mp/tradinghours/markets/lookup",
    "price": 0.017391,
    "description": "Search markets by exchange name, market name, security description, MIC or country.",
    "query_params": {
      "type": "object",
      "properties": {
        "q": {
          "type": "string",
          "required": false,
          "x-parameter-location": "query",
          "description": "Free form search field, matched against exchange name, market name, security description, MIC and country."
        },
        "group": {
          "type": "string",
          "enum": [
            "core",
            "extended",
            "all",
            "allowed"
          ],
          "default": "all",
          "required": false,
          "x-parameter-location": "query",
          "description": "Which group of markets to show. core is the G20+ tier, extended is Global Equities, all is Global Equities and Derivatives, and allowed restricts the result to what your subscription covers."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The marketplace specification documents JSON responses only, so csv is not confirmed for this endpoint."
        }
      },
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.mp_tradinghours_lookup",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.02,
      "x-apiosk-plans": [
        "marketplace_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "array",
      "description": "List of matching markets.",
      "items": {
        "type": "object",
        "description": "One matching market or trading schedule.",
        "properties": {
          "fin_id": {
            "type": "string",
            "description": "TradingHours FinID, the unique identifier of one distinct trading schedule, for example AE.ADX."
          },
          "exchange": {
            "type": "string",
            "description": "Exchange name in English."
          },
          "market": {
            "type": "string",
            "description": "Market or trading venue name in English."
          },
          "products": {
            "type": [
              "string",
              "null"
            ],
            "description": "Products or securities traded on this schedule, or null."
          },
          "mic": {
            "type": "string",
            "description": "ISO 10383 segment MIC of the market, for example XADS."
          },
          "asset_type": {
            "type": [
              "string",
              "null"
            ],
            "description": "Asset type traded, for example Securities, or null."
          },
          "group": {
            "type": "string",
            "description": "Access tier this market belongs to, for example Core or Extended."
          }
        }
      }
    },
    "request_example": {
      "q": "hong kong",
      "group": "allowed",
      "fmt": "json"
    },
    "response_example": "[{\"fin_id\": \"AE.ADX\", \"exchange\": \"Abu Dhabi Securities Exchange\", \"market\": \"Abu Dhabi Securities Exchange (ADX)\", \"products\": null, \"mic\": \"XADS\", \"asset_type\": \"Securities\", \"group\": \"Extended\"}]",
    "tags": [
      "marketplace market status",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Search markets by exchange name, market name, security description, MIC or country."
    ],
    "input_entities": [
      "q",
      "group",
      "fmt"
    ],
    "output_entities": [
      "marketplace market status"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "Search markets by exchange name, market name, security description, MIC or country.",
    "external_docs_url": "https://eodhd.com/marketplace/tradinghours/markets-g20/docs",
    "documentation_markdown": "GET /api/mp/tradinghours/markets/lookup — Search markets by exchange name, market name, security description, MIC or country.\n\nFree text search across market attributes, returning the same market rows as the list endpoint. Use it when you know an exchange or country by name but not its FinID. More than 900 distinct trading schedules are tracked in total, because most exchanges run several schedules for equities, bonds and futures.\n\nEODHD call weight: 10; required plans: marketplace_subscription.\n\nMarketplace product with its own subscription, separate from the main EODHD plans. Limits are 100 000 API calls per 24 hours and 1000 requests per minute, and 1 request equals 10 API calls. For marketplace products the 24 hour window starts at the first request rather than at a fixed daily reset. Sold in three tiers that share the same paths: G20+ Markets (core, 24 markets), Global Equities (extended, 148 equity markets) and Global Equities & Derivatives (all, 900+ markets). Your token is limited to the tier you bought, and group=allowed returns exactly what your subscription covers. q is documented as optional, so an empty query behaves like the list endpoint. No pagination parameters are documented.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "mp_tradinghours_lookup"
  },
  {
    "id": "d1ad739b-12a6-5d70-9a85-f23d2e506abf",
    "method": "GET",
    "path": "/api/mp/tradinghours/markets/details",
    "price": 0.026087,
    "description": "Return static details for one or more markets identified by FinID.",
    "query_params": {
      "type": "object",
      "properties": {
        "fin_id": {
          "type": "string",
          "required": true,
          "x-parameter-location": "query",
          "description": "Which market or markets to show data for, by FinID. A MIC is also accepted and resolves to the closest matching FinID."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The marketplace specification documents JSON responses only, so csv is not confirmed for this endpoint."
        }
      },
      "required": [
        "fin_id"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.mp_tradinghours_details",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.03,
      "x-apiosk-plans": [
        "marketplace_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "array",
      "description": "Market detail records.",
      "items": {
        "type": "object",
        "description": "Static details of one market.",
        "properties": {
          "fin_id": {
            "type": "string",
            "description": "TradingHours FinID of the market, for example JP.JPX."
          },
          "country_code": {
            "type": "string",
            "description": "Two letter country code of the market, for example JP."
          },
          "exchange": {
            "type": "string",
            "description": "Exchange name in English."
          },
          "market": {
            "type": "string",
            "description": "Market or trading venue name in English."
          },
          "products": {
            "type": [
              "string",
              "null"
            ],
            "description": "Products or securities traded on this schedule, or null."
          },
          "mic": {
            "type": "string",
            "description": "ISO 10383 MIC of the market."
          },
          "mic_extended": {
            "type": "string",
            "description": "Extended MIC of the market."
          },
          "acronym": {
            "type": "string",
            "description": "Common acronym of the exchange, for example JPX."
          },
          "asset_type": {
            "type": [
              "string",
              "null"
            ],
            "description": "Asset type traded, or null."
          },
          "memo": {
            "type": "string",
            "description": "Free text note about the coverage of this schedule."
          },
          "permanently_closed": {
            "type": [
              "string",
              "null"
            ],
            "format": "date",
            "description": "Date on which the market permanently closed, or null if it is still operating."
          },
          "timezone": {
            "type": "string",
            "description": "IANA timezone of the market, for example Asia/Tokyo."
          },
          "weekend_definition": {
            "type": "string",
            "description": "Which days count as the weekend for this market, for example Sat-Sun."
          }
        }
      }
    },
    "request_example": {
      "fin_id": "us.nyse",
      "fmt": "json"
    },
    "response_example": "[{\"fin_id\": \"US.NYSE\", \"country_code\": \"US\", \"exchange\": \"New York Stock Exchange\", \"market\": \"Canonical\", \"products\": null, \"mic\": \"XNYS\", \"mic_extended\": \"XNYS\", \"acronym\": \"NYSE\", \"asset_type\": null, \"memo\": \"Canonical\", \"permanently_closed\": null, \"timezone\": \"America/New_York\", \"weekend_definition\": \"Sat-Sun\"}]",
    "tags": [
      "marketplace market status",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Return static details for one or more markets identified by FinID."
    ],
    "input_entities": [
      "fin_id",
      "fmt"
    ],
    "output_entities": [
      "marketplace market status"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "Return static details for one or more markets identified by FinID.",
    "external_docs_url": "https://eodhd.com/marketplace/tradinghours/markets-g20/docs",
    "documentation_markdown": "GET /api/mp/tradinghours/markets/details — Return static details for one or more markets identified by FinID.\n\nReturns descriptive metadata for a market: country code, exchange and market names, MIC and extended MIC, acronym, asset type, a free text memo, the IANA timezone, the weekend definition and a permanent closure date if any. Use it together with the status endpoint to render human readable market information.\n\nEODHD call weight: 10; required plans: marketplace_subscription.\n\nMarketplace product with its own subscription, separate from the main EODHD plans. Limits are 100 000 API calls per 24 hours and 1000 requests per minute, and 1 request equals 10 API calls. For marketplace products the 24 hour window starts at the first request rather than at a fixed daily reset. Sold in three tiers that share the same paths: G20+ Markets (core, 24 markets), Global Equities (extended, 148 equity markets) and Global Equities & Derivatives (all, 900+ markets). Your token is limited to the tier you bought, and group=allowed returns exactly what your subscription covers. fin_id is case insensitive in the documented example (us.nyse in the request, US.NYSE in the response). The parameter accepts several markets in one call, but the documentation does not state the separator or a maximum count.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "mp_tradinghours_details"
  },
  {
    "id": "62c9c1fb-8be5-5bc2-9234-c509c7cb953b",
    "method": "GET",
    "path": "/api/mp/tradinghours/markets/status",
    "price": 0.043478,
    "description": "Return the real time open or closed status of one or more markets with the next bell time.",
    "query_params": {
      "type": "object",
      "properties": {
        "fin_id": {
          "type": "string",
          "required": true,
          "x-parameter-location": "query",
          "description": "Which market or markets to show data for, by FinID. A MIC is also accepted and resolves to the closest matching FinID."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The marketplace specification documents JSON responses only, so csv is not confirmed for this endpoint."
        }
      },
      "required": [
        "fin_id"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.mp_tradinghours_status",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.05,
      "x-apiosk-plans": [
        "marketplace_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "Envelope holding market status keyed by FinID.",
      "properties": {
        "data": {
          "type": "object",
          "description": "Market status objects keyed by FinID, for example US.NYSE.",
          "additionalProperties": {
            "type": "object",
            "description": "Live status of one market.",
            "properties": {
              "fin_id": {
                "type": "string",
                "description": "TradingHours FinID of the market."
              },
              "exchange": {
                "type": "string",
                "description": "Exchange name in English."
              },
              "market": {
                "type": "string",
                "description": "Market or trading venue name in English."
              },
              "products": {
                "type": [
                  "string",
                  "null"
                ],
                "description": "Products or securities traded on this schedule, or null."
              },
              "timezone": {
                "type": "string",
                "description": "IANA timezone of the market."
              },
              "status": {
                "type": "string",
                "description": "Current status of the market, for example Open or Closed."
              },
              "reason": {
                "type": [
                  "string",
                  "null"
                ],
                "description": "Explanation of an irregular status, for example a market holiday or partial session, or null."
              },
              "until": {
                "type": "string",
                "format": "date-time",
                "description": "Timestamp until which the current status will not change. Safe to cache the result until this time."
              },
              "next_bell": {
                "type": "string",
                "format": "date-time",
                "description": "Timestamp of the next official market open or close bell."
              }
            }
          }
        }
      }
    },
    "request_example": {
      "fin_id": "us.nyse",
      "fmt": "json"
    },
    "response_example": "{\"data\": {\"US.NYSE\": {\"fin_id\": \"US.NYSE\", \"exchange\": \"New York Stock Exchange\", \"market\": \"Canonical\", \"products\": null, \"timezone\": \"America/New_York\", \"status\": \"Open\", \"reason\": \"Market Holiday - Primary Trading Session (Partial)\", \"until\": \"2020-11-27T12:45:00-05:00\", \"next_bell\": \"2020-11-27T13:00:00-05:00\"}}}",
    "tags": [
      "marketplace market status",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Return the real time open or closed status of one or more markets with the next bell time."
    ],
    "input_entities": [
      "fin_id",
      "fmt"
    ],
    "output_entities": [
      "marketplace market status"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "Return the real time open or closed status of one or more markets with the next bell time.",
    "external_docs_url": "https://eodhd.com/marketplace/tradinghours/markets-g20/docs",
    "documentation_markdown": "GET /api/mp/tradinghours/markets/status — Return the real time open or closed status of one or more markets with the next bell time.\n\nReturns the live status of a market: whether it is open or closed, the current trading phase such as a pre or post trading session, the reason when a holiday or irregular schedule applies, the time until which the status is guaranteed and the time of the next opening or closing bell. Good for dashboards, countdowns and for gating trading algorithms. Results are safe to cache until the until timestamp.\n\nEODHD call weight: 10; required plans: marketplace_subscription.\n\nMarketplace product with its own subscription, separate from the main EODHD plans. Limits are 100 000 API calls per 24 hours and 1000 requests per minute, and 1 request equals 10 API calls. For marketplace products the 24 hour window starts at the first request rather than at a fixed daily reset. Sold in three tiers that share the same paths: G20+ Markets (core, 24 markets), Global Equities (extended, 148 equity markets) and Global Equities & Derivatives (all, 900+ markets). Your token is limited to the tier you bought, and group=allowed returns exactly what your subscription covers. until is not always the same as next_bell: during a post trading session until marks the end of that session while next_bell is the following official open. The API accounts for scheduled holidays and half days but not for circuit breakers or halts. The time query parameter used by TradingHours directly is not available here; contact tradinghours.com for that. Cache until the until timestamp to stay within the rate limits.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "mp_tradinghours_status"
  },
  {
    "id": "80b8dd80-d189-535b-bbbf-0a0f3a484d5a",
    "method": "GET",
    "path": "/api/mp/praams/analyse/equity/ticker/{ticker}",
    "price": 0.043478,
    "description": "Return PRAAMS risk and return analytics for one equity, looked up by ticker.",
    "query_params": {
      "type": "object",
      "properties": {
        "ticker": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "The unique ticker symbol of the asset to retrieve. Demo tickers: AAPL, TSLA and AMZN."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The marketplace specification documents JSON responses only, so csv is not confirmed for this endpoint."
        }
      },
      "required": [
        "ticker"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.mp_praams_analyse_equity_ticker",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.05,
      "x-apiosk-plans": [
        "marketplace_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "PRAAMS response envelope.",
      "properties": {
        "success": {
          "type": "boolean",
          "description": "Whether the request succeeded."
        },
        "message": {
          "type": [
            "string",
            "null"
          ],
          "description": "Human readable status or error message."
        },
        "errors": {
          "type": [
            "array",
            "null"
          ],
          "description": "List of errors, or null when the request succeeded.",
          "items": {
            "type": "object",
            "description": "One error entry."
          }
        },
        "item": {
          "type": "object",
          "description": "The main equity analysis payload.",
          "properties": {
            "asset": {
              "type": "object",
              "description": "Basic asset information such as identifiers, name and market data."
            },
            "description": {
              "type": "object",
              "description": "Textual descriptions and executive summaries of the asset."
            },
            "profile": {
              "type": "object",
              "description": "Company profile information."
            },
            "scores": {
              "type": "object",
              "description": "Risk and return scores, including the summarising PRAAMS Ratio, the total return score and the total risk score."
            },
            "keyFactors": {
              "type": "object",
              "description": "Key factors driving the risk and return assessment."
            },
            "valuation": {
              "type": "object",
              "description": "Valuation multiples and related metrics."
            },
            "performance": {
              "type": "object",
              "description": "Historical performance metrics."
            },
            "analystView": {
              "type": "object",
              "description": "Analyst consensus view and price targets."
            },
            "profitability": {
              "type": "object",
              "description": "Profitability metrics, margins and return on invested capital versus WACC."
            },
            "growthMomentum": {
              "type": "object",
              "description": "Growth and momentum metrics."
            },
            "dividend": {
              "type": "object",
              "description": "Dividend metrics."
            }
          }
        }
      }
    },
    "request_example": {
      "ticker": "AAPL",
      "fmt": "json"
    },
    "response_example": null,
    "tags": [
      "marketplace analytics",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Return PRAAMS risk and return analytics for one equity, looked up by ticker."
    ],
    "input_entities": [
      "ticker",
      "fmt"
    ],
    "output_entities": [
      "marketplace analytics"
    ],
    "endpoint_category": "finance",
    "action_type": "compute",
    "task_summary": "Return PRAAMS risk and return analytics for one equity, looked up by ticker.",
    "external_docs_url": "https://eodhd.com/marketplace/praams/analyse/docs",
    "documentation_markdown": "GET /api/mp/praams/analyse/equity/ticker/{ticker} — Return PRAAMS risk and return analytics for one equity, looked up by ticker.\n\nReturns a full risk and return analysis for a stock, summarising more than 470 metrics into the PRAAMS Ratio plus separate total return and total risk scores. The payload also carries valuation multiples, profitability, growth, dividend, analyst view, default risk, volatility and stress testing and liquidity blocks, with descriptions and executive summaries. Coverage is 120 000+ global equities and bonds.\n\nEODHD call weight: 10; required plans: marketplace_subscription.\n\nMarketplace product with its own subscription, separate from the main EODHD plans. Limits are 100 000 API calls per 24 hours and 1000 requests per minute, and 1 request equals 10 API calls. For marketplace products the 24 hour window starts at the first request rather than at a fixed daily reset. PRAAMS response bodies are wrapped in a standard envelope with success, message, errors and item (or items for collections), and the detailed payload schemas live in the shared component file https://eodhd.com/vendor/marketplace/praams/components.yaml rather than in the endpoint specification. Error codes are non standard: 415 for a wrong token, 420 for a cancelled operation and 430 for data not found. Note that the ticker or ISIN is appended as a path segment after the literal ticker or isin segment. No example response is published in the specification, only a schema reference. The specification declares ticker as a path parameter without placing it in the path template; the cURL sample shows it appended after the ticker segment.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "mp_praams_analyse_equity_ticker"
  },
  {
    "id": "55234cca-2d98-5cba-b8f9-b460276ba746",
    "method": "GET",
    "path": "/api/mp/praams/analyse/equity/isin/{isin}",
    "price": 0.043478,
    "description": "Return PRAAMS risk and return analytics for one equity, looked up by ISIN.",
    "query_params": {
      "type": "object",
      "properties": {
        "isin": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "The unique ISIN of the asset to retrieve. Demo ISINs: US0378331005, US88160R1014 and US0231351067."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The marketplace specification documents JSON responses only, so csv is not confirmed for this endpoint."
        }
      },
      "required": [
        "isin"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.mp_praams_analyse_equity_isin",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.05,
      "x-apiosk-plans": [
        "marketplace_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "PRAAMS response envelope.",
      "properties": {
        "success": {
          "type": "boolean",
          "description": "Whether the request succeeded."
        },
        "message": {
          "type": [
            "string",
            "null"
          ],
          "description": "Human readable status or error message."
        },
        "errors": {
          "type": [
            "array",
            "null"
          ],
          "description": "List of errors, or null when the request succeeded.",
          "items": {
            "type": "object",
            "description": "One error entry."
          }
        },
        "item": {
          "type": "object",
          "description": "The main equity analysis payload.",
          "properties": {
            "asset": {
              "type": "object",
              "description": "Basic asset information such as identifiers, name and market data."
            },
            "description": {
              "type": "object",
              "description": "Textual descriptions and executive summaries of the asset."
            },
            "profile": {
              "type": "object",
              "description": "Company profile information."
            },
            "scores": {
              "type": "object",
              "description": "Risk and return scores, including the summarising PRAAMS Ratio, the total return score and the total risk score."
            },
            "keyFactors": {
              "type": "object",
              "description": "Key factors driving the risk and return assessment."
            },
            "valuation": {
              "type": "object",
              "description": "Valuation multiples and related metrics."
            },
            "performance": {
              "type": "object",
              "description": "Historical performance metrics."
            },
            "analystView": {
              "type": "object",
              "description": "Analyst consensus view and price targets."
            },
            "profitability": {
              "type": "object",
              "description": "Profitability metrics, margins and return on invested capital versus WACC."
            },
            "growthMomentum": {
              "type": "object",
              "description": "Growth and momentum metrics."
            },
            "dividend": {
              "type": "object",
              "description": "Dividend metrics."
            }
          }
        }
      }
    },
    "request_example": {
      "isin": "US0378331005",
      "fmt": "json"
    },
    "response_example": null,
    "tags": [
      "marketplace analytics",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Return PRAAMS risk and return analytics for one equity, looked up by ISIN."
    ],
    "input_entities": [
      "isin",
      "fmt"
    ],
    "output_entities": [
      "marketplace analytics"
    ],
    "endpoint_category": "finance",
    "action_type": "compute",
    "task_summary": "Return PRAAMS risk and return analytics for one equity, looked up by ISIN.",
    "external_docs_url": "https://eodhd.com/marketplace/praams/analyse/docs",
    "documentation_markdown": "GET /api/mp/praams/analyse/equity/isin/{isin} — Return PRAAMS risk and return analytics for one equity, looked up by ISIN.\n\nSame equity analysis payload as the ticker variant, addressed by ISIN instead of ticker. Useful when your security master is ISIN based or when a company is listed under several tickers. Returns the PRAAMS Ratio, return and risk scores and the supporting valuation, profitability, growth, dividend and analyst blocks.\n\nEODHD call weight: 10; required plans: marketplace_subscription.\n\nMarketplace product with its own subscription, separate from the main EODHD plans. Limits are 100 000 API calls per 24 hours and 1000 requests per minute, and 1 request equals 10 API calls. For marketplace products the 24 hour window starts at the first request rather than at a fixed daily reset. PRAAMS response bodies are wrapped in a standard envelope with success, message, errors and item (or items for collections), and the detailed payload schemas live in the shared component file https://eodhd.com/vendor/marketplace/praams/components.yaml rather than in the endpoint specification. Error codes are non standard: 415 for a wrong token, 420 for a cancelled operation and 430 for data not found. Note that the ticker or ISIN is appended as a path segment after the literal ticker or isin segment. No example response is published in the specification, only a schema reference.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "mp_praams_analyse_equity_isin"
  },
  {
    "id": "a7317763-8758-5469-a735-86ca43b6980c",
    "method": "GET",
    "path": "/api/mp/praams/analyse/bond/{isin}",
    "price": 0.043478,
    "description": "Return PRAAMS risk and return analytics for one bond, looked up by ISIN.",
    "query_params": {
      "type": "object",
      "properties": {
        "isin": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "The ISIN of the bond to retrieve. Demo ISINs: US7593518852 and US91282CJN20."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The marketplace specification documents JSON responses only, so csv is not confirmed for this endpoint."
        }
      },
      "required": [
        "isin"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.mp_praams_analyse_bond",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.05,
      "x-apiosk-plans": [
        "marketplace_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "PRAAMS response envelope.",
      "properties": {
        "success": {
          "type": "boolean",
          "description": "Whether the request succeeded."
        },
        "message": {
          "type": [
            "string",
            "null"
          ],
          "description": "Human readable status or error message."
        },
        "errors": {
          "type": [
            "array",
            "null"
          ],
          "description": "List of errors, or null when the request succeeded.",
          "items": {
            "type": "object",
            "description": "One error entry."
          }
        },
        "item": {
          "type": "object",
          "description": "The main bond analysis payload.",
          "properties": {
            "asset": {
              "type": "object",
              "description": "Basic asset information such as identifiers, name and market data."
            },
            "description": {
              "type": "object",
              "description": "Textual descriptions and executive summaries of the asset."
            },
            "profile": {
              "type": "object",
              "description": "Company profile information."
            },
            "scores": {
              "type": "object",
              "description": "Risk and return scores, including the summarising PRAAMS Ratio, the total return score and the total risk score."
            },
            "keyFactors": {
              "type": "object",
              "description": "Key factors driving the risk and return assessment."
            },
            "valuation": {
              "type": "object",
              "description": "Valuation multiples and related metrics."
            },
            "performance": {
              "type": "object",
              "description": "Historical performance metrics."
            },
            "profitability": {
              "type": "object",
              "description": "Profitability metrics, margins and return on invested capital versus WACC."
            },
            "growthMomentum": {
              "type": "object",
              "description": "Growth and momentum metrics."
            },
            "marketView": {
              "type": "object",
              "description": "Market view metrics for the bond."
            },
            "coupon": {
              "type": "object",
              "description": "Coupon information for the bond."
            }
          }
        }
      }
    },
    "request_example": {
      "isin": "US7593518852",
      "fmt": "json"
    },
    "response_example": null,
    "tags": [
      "marketplace analytics",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Return PRAAMS risk and return analytics for one bond, looked up by ISIN."
    ],
    "input_entities": [
      "isin",
      "fmt"
    ],
    "output_entities": [
      "marketplace analytics"
    ],
    "endpoint_category": "finance",
    "action_type": "compute",
    "task_summary": "Return PRAAMS risk and return analytics for one bond, looked up by ISIN.",
    "external_docs_url": "https://eodhd.com/marketplace/praams/analyse/docs",
    "documentation_markdown": "GET /api/mp/praams/analyse/bond/{isin} — Return PRAAMS risk and return analytics for one bond, looked up by ISIN.\n\nReturns the bond equivalent of the equity analysis: PRAAMS Ratio, return and risk scores, valuation, performance, market view, profitability, growth momentum and coupon blocks, plus descriptions and executive summaries. Bonds are always addressed by ISIN. Use it to screen credit risk and yield alongside equities in one framework.\n\nEODHD call weight: 10; required plans: marketplace_subscription.\n\nMarketplace product with its own subscription, separate from the main EODHD plans. Limits are 100 000 API calls per 24 hours and 1000 requests per minute, and 1 request equals 10 API calls. For marketplace products the 24 hour window starts at the first request rather than at a fixed daily reset. PRAAMS response bodies are wrapped in a standard envelope with success, message, errors and item (or items for collections), and the detailed payload schemas live in the shared component file https://eodhd.com/vendor/marketplace/praams/components.yaml rather than in the endpoint specification. Error codes are non standard: 415 for a wrong token, 420 for a cancelled operation and 430 for data not found. Note that the ticker or ISIN is appended as a path segment after the literal ticker or isin segment. The bond payload replaces the equity analystView and dividend blocks with marketView and coupon. No example response is published in the specification.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "mp_praams_analyse_bond"
  },
  {
    "id": "bb1b4101-15c6-52c6-9bd3-d94b1b98949e",
    "method": "GET",
    "path": "/api/mp/praams/bank/income_statement/ticker/{ticker}",
    "price": 0.043478,
    "description": "Return standardised bank income statement data for one bank, looked up by ticker.",
    "query_params": {
      "type": "object",
      "properties": {
        "ticker": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "The ticker symbol for the bank. Demo tickers: JPM (JPMorgan Chase & Co), BAC (Bank of America Corporation) and WFC (Wells Fargo & Company)."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The marketplace specification documents JSON responses only, so csv is not confirmed for this endpoint."
        }
      },
      "required": [
        "ticker"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.mp_praams_bank_income_ticker",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.05,
      "x-apiosk-plans": [
        "marketplace_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "PRAAMS response envelope.",
      "properties": {
        "success": {
          "type": "boolean",
          "description": "Whether the request succeeded."
        },
        "message": {
          "type": [
            "string",
            "null"
          ],
          "description": "Human readable status or error message."
        },
        "errors": {
          "type": [
            "array",
            "null"
          ],
          "description": "List of errors, or null when the request succeeded.",
          "items": {
            "type": "object",
            "description": "One error entry."
          }
        },
        "items": {
          "type": "array",
          "description": "Collection of returned records.",
          "items": {
            "type": "object",
            "description": "One reporting period of standardised bank income statement data.",
            "properties": {
              "date": {
                "type": "string",
                "format": "date-time",
                "description": "The date associated with the income statement data."
              },
              "period": {
                "type": "string",
                "description": "The period associated with the financial data reporting."
              },
              "isQuarter": {
                "type": "boolean",
                "description": "Indicates whether the financial data represents quarterly reporting."
              },
              "netFeeAndCommission": {
                "type": "number",
                "description": "The net fee and commission value derived from financial operations."
              },
              "creditLossesProvision": {
                "type": "number",
                "description": "The provision amount for credit losses in the income statement."
              },
              "specialIncomeCharges": {
                "type": "number",
                "description": "The amount associated with special income charges."
              },
              "nonInterestExpenses": {
                "type": "number",
                "description": "The total non-interest expenses for the financial period."
              },
              "interestIncome": {
                "type": "number",
                "description": "The interest income reported in the income statement."
              },
              "interestExpense": {
                "type": "number",
                "description": "The interest expense associated with the income statement data."
              },
              "incomeTaxExpense": {
                "type": "number",
                "description": "The income tax expense associated with the income statement data."
              },
              "netProfit": {
                "type": "number",
                "description": "The net profit value for the financial statement."
              },
              "paymentsOnBehalfEmployee": {
                "type": "number",
                "description": "The payments made on behalf of employees."
              },
              "otherCashPayments": {
                "type": "number",
                "description": "The value representing cash payments categorised as other."
              },
              "dps": {
                "type": "number",
                "description": "Dividend per share."
              },
              "provisioning": {
                "type": "number",
                "description": "The total provisioning amount in relation to credit loss provisions."
              },
              "operatingExpenses": {
                "type": "number",
                "description": "The operating expenses derived from the financial data."
              },
              "netInterestIncome": {
                "type": "number",
                "description": "The net interest income for the bank."
              },
              "coreRevenue": {
                "type": "number",
                "description": "The core revenue, the sum of net interest income and net fee and commission."
              },
              "ribpt": {
                "type": "number",
                "description": "Revenue after subtracting operating expenses from core revenue."
              },
              "preTaxProfit": {
                "type": "number",
                "description": "The pre-tax profit derived from income tax expense and net profit."
              },
              "ibpt": {
                "type": "number",
                "description": "Income before provision and tax."
              },
              "nonRecurringIncome": {
                "type": "number",
                "description": "The calculated value of non-recurring income."
              }
            }
          }
        }
      }
    },
    "request_example": {
      "ticker": "JPM",
      "fmt": "json"
    },
    "response_example": null,
    "tags": [
      "marketplace analytics",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Return standardised bank income statement data for one bank, looked up by ticker."
    ],
    "input_entities": [
      "ticker",
      "fmt"
    ],
    "output_entities": [
      "marketplace analytics"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "Return standardised bank income statement data for one bank, looked up by ticker.",
    "external_docs_url": "https://eodhd.com/marketplace/praams/bank/docs",
    "documentation_markdown": "GET /api/mp/praams/bank/income_statement/ticker/{ticker} — Return standardised bank income statement data for one bank, looked up by ticker.\n\nReturns income statement line items for a bank in a standardised, bank specific format that generic company fundamentals do not provide, with annual and quarterly periods in the same collection. Bank specific derived measures such as net interest income, core revenue, income before provision and tax and interest earning assets are computed for you. Good for cross border bank comparison and credit analysis.\n\nEODHD call weight: 10; required plans: marketplace_subscription.\n\nMarketplace product with its own subscription, separate from the main EODHD plans. Limits are 100 000 API calls per 24 hours and 1000 requests per minute, and 1 request equals 10 API calls. For marketplace products the 24 hour window starts at the first request rather than at a fixed daily reset. PRAAMS response bodies are wrapped in a standard envelope with success, message, errors and item (or items for collections), and the detailed payload schemas live in the shared component file https://eodhd.com/vendor/marketplace/praams/components.yaml rather than in the endpoint specification. Error codes are non standard: 415 for a wrong token, 420 for a cancelled operation and 430 for data not found. Note that the ticker or ISIN is appended as a path segment after the literal ticker or isin segment. This endpoint returns a collection envelope with items rather than a single item. Values are typed as double in the specification but no currency or scaling unit is documented, so confirm units before comparing banks. No example response is published.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "mp_praams_bank_income_ticker"
  },
  {
    "id": "1b4b6dd8-f93f-5543-976f-ab038d291602",
    "method": "GET",
    "path": "/api/mp/praams/bank/income_statement/isin/{isin}",
    "price": 0.043478,
    "description": "Return standardised bank income statement data for one bank, looked up by isin.",
    "query_params": {
      "type": "object",
      "properties": {
        "isin": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "The ISIN of the bank. Demo ISINs: US46625H1005 (JPMorgan Chase & Co), US0605051046 (Bank of America Corporation) and US9497461015 (Wells Fargo & Company)."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The marketplace specification documents JSON responses only, so csv is not confirmed for this endpoint."
        }
      },
      "required": [
        "isin"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.mp_praams_bank_income_isin",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.05,
      "x-apiosk-plans": [
        "marketplace_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "PRAAMS response envelope.",
      "properties": {
        "success": {
          "type": "boolean",
          "description": "Whether the request succeeded."
        },
        "message": {
          "type": [
            "string",
            "null"
          ],
          "description": "Human readable status or error message."
        },
        "errors": {
          "type": [
            "array",
            "null"
          ],
          "description": "List of errors, or null when the request succeeded.",
          "items": {
            "type": "object",
            "description": "One error entry."
          }
        },
        "items": {
          "type": "array",
          "description": "Collection of returned records.",
          "items": {
            "type": "object",
            "description": "One reporting period of standardised bank income statement data.",
            "properties": {
              "date": {
                "type": "string",
                "format": "date-time",
                "description": "The date associated with the income statement data."
              },
              "period": {
                "type": "string",
                "description": "The period associated with the financial data reporting."
              },
              "isQuarter": {
                "type": "boolean",
                "description": "Indicates whether the financial data represents quarterly reporting."
              },
              "netFeeAndCommission": {
                "type": "number",
                "description": "The net fee and commission value derived from financial operations."
              },
              "creditLossesProvision": {
                "type": "number",
                "description": "The provision amount for credit losses in the income statement."
              },
              "specialIncomeCharges": {
                "type": "number",
                "description": "The amount associated with special income charges."
              },
              "nonInterestExpenses": {
                "type": "number",
                "description": "The total non-interest expenses for the financial period."
              },
              "interestIncome": {
                "type": "number",
                "description": "The interest income reported in the income statement."
              },
              "interestExpense": {
                "type": "number",
                "description": "The interest expense associated with the income statement data."
              },
              "incomeTaxExpense": {
                "type": "number",
                "description": "The income tax expense associated with the income statement data."
              },
              "netProfit": {
                "type": "number",
                "description": "The net profit value for the financial statement."
              },
              "paymentsOnBehalfEmployee": {
                "type": "number",
                "description": "The payments made on behalf of employees."
              },
              "otherCashPayments": {
                "type": "number",
                "description": "The value representing cash payments categorised as other."
              },
              "dps": {
                "type": "number",
                "description": "Dividend per share."
              },
              "provisioning": {
                "type": "number",
                "description": "The total provisioning amount in relation to credit loss provisions."
              },
              "operatingExpenses": {
                "type": "number",
                "description": "The operating expenses derived from the financial data."
              },
              "netInterestIncome": {
                "type": "number",
                "description": "The net interest income for the bank."
              },
              "coreRevenue": {
                "type": "number",
                "description": "The core revenue, the sum of net interest income and net fee and commission."
              },
              "ribpt": {
                "type": "number",
                "description": "Revenue after subtracting operating expenses from core revenue."
              },
              "preTaxProfit": {
                "type": "number",
                "description": "The pre-tax profit derived from income tax expense and net profit."
              },
              "ibpt": {
                "type": "number",
                "description": "Income before provision and tax."
              },
              "nonRecurringIncome": {
                "type": "number",
                "description": "The calculated value of non-recurring income."
              }
            }
          }
        }
      }
    },
    "request_example": {
      "isin": "US46625H1005",
      "fmt": "json"
    },
    "response_example": null,
    "tags": [
      "marketplace analytics",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Return standardised bank income statement data for one bank, looked up by isin."
    ],
    "input_entities": [
      "isin",
      "fmt"
    ],
    "output_entities": [
      "marketplace analytics"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "Return standardised bank income statement data for one bank, looked up by isin.",
    "external_docs_url": "https://eodhd.com/marketplace/praams/bank/docs",
    "documentation_markdown": "GET /api/mp/praams/bank/income_statement/isin/{isin} — Return standardised bank income statement data for one bank, looked up by isin.\n\nReturns income statement line items for a bank in a standardised, bank specific format that generic company fundamentals do not provide, with annual and quarterly periods in the same collection. Bank specific derived measures such as net interest income, core revenue, income before provision and tax and interest earning assets are computed for you. Good for cross border bank comparison and credit analysis.\n\nEODHD call weight: 10; required plans: marketplace_subscription.\n\nMarketplace product with its own subscription, separate from the main EODHD plans. Limits are 100 000 API calls per 24 hours and 1000 requests per minute, and 1 request equals 10 API calls. For marketplace products the 24 hour window starts at the first request rather than at a fixed daily reset. PRAAMS response bodies are wrapped in a standard envelope with success, message, errors and item (or items for collections), and the detailed payload schemas live in the shared component file https://eodhd.com/vendor/marketplace/praams/components.yaml rather than in the endpoint specification. Error codes are non standard: 415 for a wrong token, 420 for a cancelled operation and 430 for data not found. Note that the ticker or ISIN is appended as a path segment after the literal ticker or isin segment. This endpoint returns a collection envelope with items rather than a single item. Values are typed as double in the specification but no currency or scaling unit is documented, so confirm units before comparing banks. No example response is published.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "mp_praams_bank_income_isin"
  },
  {
    "id": "a044f907-e32d-52be-9a65-1af8fb45f59f",
    "method": "GET",
    "path": "/api/mp/praams/bank/balance_sheet/ticker/{ticker}",
    "price": 0.043478,
    "description": "Return standardised bank balance sheet data for one bank, looked up by ticker.",
    "query_params": {
      "type": "object",
      "properties": {
        "ticker": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "The ticker symbol for the bank. Demo tickers: JPM, BAC and WFC."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The marketplace specification documents JSON responses only, so csv is not confirmed for this endpoint."
        }
      },
      "required": [
        "ticker"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.mp_praams_bank_balance_ticker",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.05,
      "x-apiosk-plans": [
        "marketplace_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "PRAAMS response envelope.",
      "properties": {
        "success": {
          "type": "boolean",
          "description": "Whether the request succeeded."
        },
        "message": {
          "type": [
            "string",
            "null"
          ],
          "description": "Human readable status or error message."
        },
        "errors": {
          "type": [
            "array",
            "null"
          ],
          "description": "List of errors, or null when the request succeeded.",
          "items": {
            "type": "object",
            "description": "One error entry."
          }
        },
        "items": {
          "type": "array",
          "description": "Collection of returned records.",
          "items": {
            "type": "object",
            "description": "One reporting period of standardised bank balance sheet data.",
            "properties": {
              "date": {
                "type": "string",
                "format": "date-time",
                "description": "The date corresponding to the financial data record."
              },
              "period": {
                "type": "string",
                "description": "The financial reporting period represented in the balance sheet."
              },
              "isQuarter": {
                "type": "boolean",
                "description": "Indicates whether the financial data corresponds to a quarter."
              },
              "derivativeAssets": {
                "type": "number",
                "description": "The value of derivative assets held by the bank."
              },
              "loansGross": {
                "type": "number",
                "description": "The gross value of loans reported in the balance sheet."
              },
              "loanProvisions": {
                "type": "number",
                "description": "The loan provisions, the allowances for potential loan losses."
              },
              "netLoan": {
                "type": "number",
                "description": "The net loan amount on the balance sheet."
              },
              "cashAndEquivalents": {
                "type": "number",
                "description": "The value of cash and cash equivalents held by the bank."
              },
              "depositsWithBanksNet": {
                "type": "number",
                "description": "The net deposits held with other banking institutions."
              },
              "securitiesRepoAssets": {
                "type": "number",
                "description": "The value of securities held under repurchase agreements."
              },
              "longTermInvestments": {
                "type": "number",
                "description": "The total value of long-term investments held by the bank."
              },
              "totalAssets": {
                "type": "number",
                "description": "The total value of all assets reported on the balance sheet."
              },
              "receivables": {
                "type": "number",
                "description": "The receivables recorded on the balance sheet."
              },
              "investmentPortfolio": {
                "type": "number",
                "description": "The aggregated value of the investment portfolio."
              },
              "otherAssets": {
                "type": "number",
                "description": "The calculated value of other assets on the balance sheet."
              },
              "tradingLiabilities": {
                "type": "number",
                "description": "The value of liabilities arising from trading activities."
              },
              "derivativeProductLiabilities": {
                "type": "number",
                "description": "The liabilities associated with derivative financial products."
              },
              "deposits": {
                "type": "number",
                "description": "The total amount of deposits held by the bank."
              },
              "securitiesRepoEquity": {
                "type": "number",
                "description": "The value of securities sold under repurchase agreements."
              },
              "payables": {
                "type": "number",
                "description": "The amount of outstanding payables for the financial entity."
              },
              "shortTermDebt": {
                "type": "number",
                "description": "The short-term debt amount reported on the balance sheet."
              },
              "longTermDebt": {
                "type": "number",
                "description": "The long-term debt value reported on the balance sheet."
              },
              "totalEquity": {
                "type": "number",
                "description": "The total equity of the bank as reported in the balance sheet."
              },
              "securityLiabilities": {
                "type": "number",
                "description": "The total value of liabilities related to securities."
              },
              "totalEquityAndLiabilities": {
                "type": "number",
                "description": "The total amount of equity and liabilities."
              },
              "otherLiabilities": {
                "type": "number",
                "description": "The calculated value of other liabilities."
              },
              "interestEarningAssets": {
                "type": "number",
                "description": "The total value of assets that generate interest income."
              },
              "interestBearingLiabilities": {
                "type": "number",
                "description": "The total amount of liabilities that incur interest obligations."
              }
            }
          }
        }
      }
    },
    "request_example": {
      "ticker": "JPM",
      "fmt": "json"
    },
    "response_example": null,
    "tags": [
      "marketplace analytics",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Return standardised bank balance sheet data for one bank, looked up by ticker."
    ],
    "input_entities": [
      "ticker",
      "fmt"
    ],
    "output_entities": [
      "marketplace analytics"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "Return standardised bank balance sheet data for one bank, looked up by ticker.",
    "external_docs_url": "https://eodhd.com/marketplace/praams/bank/docs",
    "documentation_markdown": "GET /api/mp/praams/bank/balance_sheet/ticker/{ticker} — Return standardised bank balance sheet data for one bank, looked up by ticker.\n\nReturns balance sheet line items for a bank in a standardised, bank specific format that generic company fundamentals do not provide, with annual and quarterly periods in the same collection. Bank specific derived measures such as net interest income, core revenue, income before provision and tax and interest earning assets are computed for you. Good for cross border bank comparison and credit analysis.\n\nEODHD call weight: 10; required plans: marketplace_subscription.\n\nMarketplace product with its own subscription, separate from the main EODHD plans. Limits are 100 000 API calls per 24 hours and 1000 requests per minute, and 1 request equals 10 API calls. For marketplace products the 24 hour window starts at the first request rather than at a fixed daily reset. PRAAMS response bodies are wrapped in a standard envelope with success, message, errors and item (or items for collections), and the detailed payload schemas live in the shared component file https://eodhd.com/vendor/marketplace/praams/components.yaml rather than in the endpoint specification. Error codes are non standard: 415 for a wrong token, 420 for a cancelled operation and 430 for data not found. Note that the ticker or ISIN is appended as a path segment after the literal ticker or isin segment. This endpoint returns a collection envelope with items rather than a single item. Values are typed as double in the specification but no currency or scaling unit is documented, so confirm units before comparing banks. No example response is published.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "mp_praams_bank_balance_ticker"
  },
  {
    "id": "ef4c07e4-568d-5d4b-b4f8-f1718905f736",
    "method": "GET",
    "path": "/api/mp/praams/bank/balance_sheet/isin/{isin}",
    "price": 0.043478,
    "description": "Return standardised bank balance sheet data for one bank, looked up by isin.",
    "query_params": {
      "type": "object",
      "properties": {
        "isin": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "The ISIN of the bank. Demo ISINs: US46625H1005, US0605051046 and US9497461015."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The marketplace specification documents JSON responses only, so csv is not confirmed for this endpoint."
        }
      },
      "required": [
        "isin"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.mp_praams_bank_balance_isin",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.05,
      "x-apiosk-plans": [
        "marketplace_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "PRAAMS response envelope.",
      "properties": {
        "success": {
          "type": "boolean",
          "description": "Whether the request succeeded."
        },
        "message": {
          "type": [
            "string",
            "null"
          ],
          "description": "Human readable status or error message."
        },
        "errors": {
          "type": [
            "array",
            "null"
          ],
          "description": "List of errors, or null when the request succeeded.",
          "items": {
            "type": "object",
            "description": "One error entry."
          }
        },
        "items": {
          "type": "array",
          "description": "Collection of returned records.",
          "items": {
            "type": "object",
            "description": "One reporting period of standardised bank balance sheet data.",
            "properties": {
              "date": {
                "type": "string",
                "format": "date-time",
                "description": "The date corresponding to the financial data record."
              },
              "period": {
                "type": "string",
                "description": "The financial reporting period represented in the balance sheet."
              },
              "isQuarter": {
                "type": "boolean",
                "description": "Indicates whether the financial data corresponds to a quarter."
              },
              "derivativeAssets": {
                "type": "number",
                "description": "The value of derivative assets held by the bank."
              },
              "loansGross": {
                "type": "number",
                "description": "The gross value of loans reported in the balance sheet."
              },
              "loanProvisions": {
                "type": "number",
                "description": "The loan provisions, the allowances for potential loan losses."
              },
              "netLoan": {
                "type": "number",
                "description": "The net loan amount on the balance sheet."
              },
              "cashAndEquivalents": {
                "type": "number",
                "description": "The value of cash and cash equivalents held by the bank."
              },
              "depositsWithBanksNet": {
                "type": "number",
                "description": "The net deposits held with other banking institutions."
              },
              "securitiesRepoAssets": {
                "type": "number",
                "description": "The value of securities held under repurchase agreements."
              },
              "longTermInvestments": {
                "type": "number",
                "description": "The total value of long-term investments held by the bank."
              },
              "totalAssets": {
                "type": "number",
                "description": "The total value of all assets reported on the balance sheet."
              },
              "receivables": {
                "type": "number",
                "description": "The receivables recorded on the balance sheet."
              },
              "investmentPortfolio": {
                "type": "number",
                "description": "The aggregated value of the investment portfolio."
              },
              "otherAssets": {
                "type": "number",
                "description": "The calculated value of other assets on the balance sheet."
              },
              "tradingLiabilities": {
                "type": "number",
                "description": "The value of liabilities arising from trading activities."
              },
              "derivativeProductLiabilities": {
                "type": "number",
                "description": "The liabilities associated with derivative financial products."
              },
              "deposits": {
                "type": "number",
                "description": "The total amount of deposits held by the bank."
              },
              "securitiesRepoEquity": {
                "type": "number",
                "description": "The value of securities sold under repurchase agreements."
              },
              "payables": {
                "type": "number",
                "description": "The amount of outstanding payables for the financial entity."
              },
              "shortTermDebt": {
                "type": "number",
                "description": "The short-term debt amount reported on the balance sheet."
              },
              "longTermDebt": {
                "type": "number",
                "description": "The long-term debt value reported on the balance sheet."
              },
              "totalEquity": {
                "type": "number",
                "description": "The total equity of the bank as reported in the balance sheet."
              },
              "securityLiabilities": {
                "type": "number",
                "description": "The total value of liabilities related to securities."
              },
              "totalEquityAndLiabilities": {
                "type": "number",
                "description": "The total amount of equity and liabilities."
              },
              "otherLiabilities": {
                "type": "number",
                "description": "The calculated value of other liabilities."
              },
              "interestEarningAssets": {
                "type": "number",
                "description": "The total value of assets that generate interest income."
              },
              "interestBearingLiabilities": {
                "type": "number",
                "description": "The total amount of liabilities that incur interest obligations."
              }
            }
          }
        }
      }
    },
    "request_example": {
      "isin": "US46625H1005",
      "fmt": "json"
    },
    "response_example": null,
    "tags": [
      "marketplace analytics",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Return standardised bank balance sheet data for one bank, looked up by isin."
    ],
    "input_entities": [
      "isin",
      "fmt"
    ],
    "output_entities": [
      "marketplace analytics"
    ],
    "endpoint_category": "finance",
    "action_type": "lookup",
    "task_summary": "Return standardised bank balance sheet data for one bank, looked up by isin.",
    "external_docs_url": "https://eodhd.com/marketplace/praams/bank/docs",
    "documentation_markdown": "GET /api/mp/praams/bank/balance_sheet/isin/{isin} — Return standardised bank balance sheet data for one bank, looked up by isin.\n\nReturns balance sheet line items for a bank in a standardised, bank specific format that generic company fundamentals do not provide, with annual and quarterly periods in the same collection. Bank specific derived measures such as net interest income, core revenue, income before provision and tax and interest earning assets are computed for you. Good for cross border bank comparison and credit analysis.\n\nEODHD call weight: 10; required plans: marketplace_subscription.\n\nMarketplace product with its own subscription, separate from the main EODHD plans. Limits are 100 000 API calls per 24 hours and 1000 requests per minute, and 1 request equals 10 API calls. For marketplace products the 24 hour window starts at the first request rather than at a fixed daily reset. PRAAMS response bodies are wrapped in a standard envelope with success, message, errors and item (or items for collections), and the detailed payload schemas live in the shared component file https://eodhd.com/vendor/marketplace/praams/components.yaml rather than in the endpoint specification. Error codes are non standard: 415 for a wrong token, 420 for a cancelled operation and 430 for data not found. Note that the ticker or ISIN is appended as a path segment after the literal ticker or isin segment. This endpoint returns a collection envelope with items rather than a single item. Values are typed as double in the specification but no currency or scaling unit is documented, so confirm units before comparing banks. No example response is published.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "mp_praams_bank_balance_isin"
  },
  {
    "id": "d0588265-3d4d-5952-bcaa-2475b2b7e598",
    "method": "POST",
    "path": "/api/mp/praams/explore/equity",
    "price": 0.043478,
    "description": "Screen equities by region, country, sector and risk and return score ranges.",
    "query_params": {
      "type": "object",
      "properties": {
        "skip": {
          "type": "integer",
          "required": false,
          "x-parameter-location": "query",
          "description": "The number of records to skip."
        },
        "take": {
          "type": "integer",
          "required": false,
          "x-parameter-location": "query",
          "description": "The number of records to retrieve."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The marketplace specification documents JSON responses only, so csv is not confirmed for this endpoint."
        }
      },
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.mp_praams_explore_equity",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.05,
      "x-apiosk-plans": [
        "marketplace_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "PRAAMS screener response envelope.",
      "properties": {
        "item": {
          "type": "object",
          "description": "The screener result set.",
          "properties": {
            "peers": {
              "type": "array",
              "description": "Collection of matching peer records, each carrying valuation, performance, profitability, growth momentum, country risk, liquidity, stress test, volatility, solvency, analyst view and dividend blocks plus riskWatch and returnWatch labels.",
              "items": {
                "type": "object",
                "description": "One matching instrument."
              }
            },
            "totalCount": {
              "type": "integer",
              "description": "Total number of items matching the filter, used for pagination."
            }
          }
        }
      }
    },
    "request_example": {
      "skip": 1,
      "take": 1,
      "fmt": "json"
    },
    "response_example": null,
    "tags": [
      "marketplace analytics",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Screen equities by region, country, sector and risk and return score ranges."
    ],
    "input_entities": [
      "skip",
      "take",
      "fmt"
    ],
    "output_entities": [
      "marketplace analytics"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "Screen equities by region, country, sector and risk and return score ranges.",
    "external_docs_url": "https://eodhd.com/marketplace/praams/explore/docs",
    "documentation_markdown": "POST /api/mp/praams/explore/equity — Screen equities by region, country, sector and risk and return score ranges.\n\nScreens the PRAAMS universe of 120 000+ instruments and returns an ordered list of peers matching the filter object sent in the request body. Filters are expressed as score ranges (minimum and maximum) on dimensions such as solvency, liquidity, performance, growth momentum, dividends or yield, combined with region, country and sector identifiers. Paging is done with the skip and take query parameters while the filter itself travels in the body.\n\nEODHD call weight: 10; required plans: marketplace_subscription.\n\nMarketplace product with its own subscription, separate from the main EODHD plans. Limits are 100 000 API calls per 24 hours and 1000 requests per minute, and 1 request equals 10 API calls. For marketplace products the 24 hour window starts at the first request rather than at a fixed daily reset. PRAAMS response bodies are wrapped in a standard envelope with success, message, errors and item (or items for collections), and the detailed payload schemas live in the shared component file https://eodhd.com/vendor/marketplace/praams/components.yaml rather than in the endpoint specification. Error codes are non standard: 415 for a wrong token, 420 for a cancelled operation and 430 for data not found. Note that the ticker or ISIN is appended as a path segment after the literal ticker or isin segment. This is the only POST endpoint in this group: the filter object (schema StocksPeersRequest) is sent as a JSON request body with Content-Type application/json, and only skip and take are query parameters. The specification does not enumerate the valid country, region and sector identifier numbers, and no example response is published. fmt is not meaningful here since the response is always JSON.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "mp_praams_explore_equity"
  },
  {
    "id": "95b8348b-8b10-5162-b318-ca31135f2f1c",
    "method": "POST",
    "path": "/api/mp/praams/explore/bond",
    "price": 0.043478,
    "description": "Screen bonds by region, country, sector and risk and return score ranges.",
    "query_params": {
      "type": "object",
      "properties": {
        "skip": {
          "type": "integer",
          "required": false,
          "x-parameter-location": "query",
          "description": "The number of records to skip."
        },
        "take": {
          "type": "integer",
          "required": false,
          "x-parameter-location": "query",
          "description": "The number of records to retrieve."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The marketplace specification documents JSON responses only, so csv is not confirmed for this endpoint."
        }
      },
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.mp_praams_explore_bond",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.05,
      "x-apiosk-plans": [
        "marketplace_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "description": "PRAAMS screener response envelope.",
      "properties": {
        "item": {
          "type": "object",
          "description": "The screener result set.",
          "properties": {
            "peers": {
              "type": "array",
              "description": "Collection of matching peer records, each carrying valuation, performance, profitability, growth momentum, country risk, liquidity, stress test, volatility, solvency, analyst view and dividend blocks plus riskWatch and returnWatch labels.",
              "items": {
                "type": "object",
                "description": "One matching instrument."
              }
            },
            "totalCount": {
              "type": "integer",
              "description": "Total number of items matching the filter, used for pagination."
            }
          }
        }
      }
    },
    "request_example": {
      "skip": 1,
      "take": 1,
      "fmt": "json"
    },
    "response_example": null,
    "tags": [
      "marketplace analytics",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Screen bonds by region, country, sector and risk and return score ranges."
    ],
    "input_entities": [
      "skip",
      "take",
      "fmt"
    ],
    "output_entities": [
      "marketplace analytics"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "Screen bonds by region, country, sector and risk and return score ranges.",
    "external_docs_url": "https://eodhd.com/marketplace/praams/explore/docs",
    "documentation_markdown": "POST /api/mp/praams/explore/bond — Screen bonds by region, country, sector and risk and return score ranges.\n\nScreens the PRAAMS universe of 120 000+ instruments and returns an ordered list of peers matching the filter object sent in the request body. Filters are expressed as score ranges (minimum and maximum) on dimensions such as solvency, liquidity, performance, growth momentum, dividends or yield, combined with region, country and sector identifiers. Paging is done with the skip and take query parameters while the filter itself travels in the body.\n\nEODHD call weight: 10; required plans: marketplace_subscription.\n\nMarketplace product with its own subscription, separate from the main EODHD plans. Limits are 100 000 API calls per 24 hours and 1000 requests per minute, and 1 request equals 10 API calls. For marketplace products the 24 hour window starts at the first request rather than at a fixed daily reset. PRAAMS response bodies are wrapped in a standard envelope with success, message, errors and item (or items for collections), and the detailed payload schemas live in the shared component file https://eodhd.com/vendor/marketplace/praams/components.yaml rather than in the endpoint specification. Error codes are non standard: 415 for a wrong token, 420 for a cancelled operation and 430 for data not found. Note that the ticker or ISIN is appended as a path segment after the literal ticker or isin segment. This is the only POST endpoint in this group: the filter object (schema BondsPeersRequest) is sent as a JSON request body with Content-Type application/json, and only skip and take are query parameters. The specification does not enumerate the valid country, region and sector identifier numbers, and no example response is published. fmt is not meaningful here since the response is always JSON.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "mp_praams_explore_bond"
  },
  {
    "id": "7a784ca7-e4a8-53f6-ae37-f641a3b8f252",
    "method": "GET",
    "path": "/api/mp/praams/reports/equity/ticker/{ticker}",
    "price": 0.026087,
    "description": "Generate and download a multi page PDF equity research report.",
    "query_params": {
      "type": "object",
      "properties": {
        "ticker": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "The unique ticker symbol of the asset for which the Equity report is requested. Demo tickers: AAPL, TSLA and AMZN."
        },
        "email": {
          "type": "string",
          "required": true,
          "x-parameter-location": "query",
          "description": "The email address to which notifications or confirmations may be sent."
        },
        "isFull": {
          "type": "boolean",
          "required": false,
          "x-parameter-location": "query",
          "description": "Whether the full report or a partial report is required."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The marketplace specification documents JSON responses only, so csv is not confirmed for this endpoint."
        }
      },
      "required": [
        "ticker",
        "email"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.mp_praams_reports_equity_ticker",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.03,
      "x-apiosk-plans": [
        "marketplace_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "string",
      "contentEncoding": "binary",
      "contentMediaType": "application/pdf",
      "description": "Binary PDF document containing the generated research report. Not a JSON body."
    },
    "request_example": {
      "ticker": "AAPL",
      "email": "user@example.com",
      "fmt": "json"
    },
    "response_example": null,
    "tags": [
      "marketplace analytics",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Generate and download a multi page PDF equity research report."
    ],
    "input_entities": [
      "ticker",
      "email",
      "isFull",
      "fmt"
    ],
    "output_entities": [
      "marketplace analytics"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "Generate and download a multi page PDF equity research report.",
    "external_docs_url": "https://eodhd.com/marketplace/praams/reports/docs",
    "documentation_markdown": "GET /api/mp/praams/reports/equity/ticker/{ticker} — Generate and download a multi page PDF equity research report.\n\nGenerates a multi page PDF research report for a single equity, built on six return factors and six risk factors, and returns it as a file download. Set isFull to choose between the full and the partial report. The response body is a PDF document rather than JSON, so this endpoint is for delivering research to people, not for machine parsing.\n\nEODHD call weight: 10; required plans: marketplace_subscription.\n\nMarketplace product with its own subscription, separate from the main EODHD plans. Limits are 100 000 API calls per 24 hours and 1000 requests per minute, and 1 request equals 10 API calls. For marketplace products the 24 hour window starts at the first request rather than at a fixed daily reset. PRAAMS response bodies are wrapped in a standard envelope with success, message, errors and item (or items for collections), and the detailed payload schemas live in the shared component file https://eodhd.com/vendor/marketplace/praams/components.yaml rather than in the endpoint specification. Error codes are non standard: 415 for a wrong token, 420 for a cancelled operation and 430 for data not found. Note that the ticker or ISIN is appended as a path segment after the literal ticker or isin segment. The email query parameter is required, which is unusual for a read only data API; the report generation flow sends notifications to that address. The response is a PDF, so fmt does not apply. Report generation takes time, so treat this as a slow endpoint.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "mp_praams_reports_equity_ticker"
  },
  {
    "id": "52df6379-db04-5b59-adf1-07791221b33c",
    "method": "GET",
    "path": "/api/mp/praams/reports/equity/isin/{isin}",
    "price": 0.026087,
    "description": "Generate and download a multi page PDF equity research report.",
    "query_params": {
      "type": "object",
      "properties": {
        "isin": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "The ISIN of the asset for which the Equity report is requested. Demo ISINs: US0378331005, US88160R1014 and US0231351067."
        },
        "email": {
          "type": "string",
          "required": true,
          "x-parameter-location": "query",
          "description": "The email address to which notifications or confirmations may be sent."
        },
        "isFull": {
          "type": "boolean",
          "required": false,
          "x-parameter-location": "query",
          "description": "Whether the full report or a partial report is required."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The marketplace specification documents JSON responses only, so csv is not confirmed for this endpoint."
        }
      },
      "required": [
        "isin",
        "email"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.mp_praams_reports_equity_isin",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.03,
      "x-apiosk-plans": [
        "marketplace_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "string",
      "contentEncoding": "binary",
      "contentMediaType": "application/pdf",
      "description": "Binary PDF document containing the generated research report. Not a JSON body."
    },
    "request_example": {
      "isin": "US0378331005",
      "email": "user@example.com",
      "fmt": "json"
    },
    "response_example": null,
    "tags": [
      "marketplace analytics",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Generate and download a multi page PDF equity research report."
    ],
    "input_entities": [
      "isin",
      "email",
      "isFull",
      "fmt"
    ],
    "output_entities": [
      "marketplace analytics"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "Generate and download a multi page PDF equity research report.",
    "external_docs_url": "https://eodhd.com/marketplace/praams/reports/docs",
    "documentation_markdown": "GET /api/mp/praams/reports/equity/isin/{isin} — Generate and download a multi page PDF equity research report.\n\nGenerates a multi page PDF research report for a single equity, built on six return factors and six risk factors, and returns it as a file download. Set isFull to choose between the full and the partial report. The response body is a PDF document rather than JSON, so this endpoint is for delivering research to people, not for machine parsing.\n\nEODHD call weight: 10; required plans: marketplace_subscription.\n\nMarketplace product with its own subscription, separate from the main EODHD plans. Limits are 100 000 API calls per 24 hours and 1000 requests per minute, and 1 request equals 10 API calls. For marketplace products the 24 hour window starts at the first request rather than at a fixed daily reset. PRAAMS response bodies are wrapped in a standard envelope with success, message, errors and item (or items for collections), and the detailed payload schemas live in the shared component file https://eodhd.com/vendor/marketplace/praams/components.yaml rather than in the endpoint specification. Error codes are non standard: 415 for a wrong token, 420 for a cancelled operation and 430 for data not found. Note that the ticker or ISIN is appended as a path segment after the literal ticker or isin segment. The email query parameter is required, which is unusual for a read only data API; the report generation flow sends notifications to that address. The response is a PDF, so fmt does not apply. Report generation takes time, so treat this as a slow endpoint.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "mp_praams_reports_equity_isin"
  },
  {
    "id": "5b6e0ea4-82cb-5eef-9dea-0eeae365b34f",
    "method": "GET",
    "path": "/api/mp/praams/reports/bond/{isin}",
    "price": 0.026087,
    "description": "Generate and download a multi page PDF bond research report.",
    "query_params": {
      "type": "object",
      "properties": {
        "isin": {
          "type": "string",
          "required": true,
          "x-parameter-location": "path",
          "description": "The ISIN of the bond for which the Bond report is requested."
        },
        "email": {
          "type": "string",
          "required": true,
          "x-parameter-location": "query",
          "description": "The email address to which notifications or confirmations may be sent."
        },
        "isFull": {
          "type": "boolean",
          "required": false,
          "x-parameter-location": "query",
          "description": "Whether the full report or a partial report is required."
        },
        "fmt": {
          "type": "string",
          "enum": [
            "json",
            "csv"
          ],
          "default": "json",
          "required": false,
          "x-parameter-location": "query",
          "description": "Response format. The marketplace specification documents JSON responses only, so csv is not confirmed for this endpoint."
        }
      },
      "required": [
        "isin",
        "email"
      ],
      "additionalProperties": false,
      "x-apiosk-capability-id": "eodhd.mp_praams_reports_bond",
      "x-apiosk-call-weight": 10,
      "x-apiosk-provider-cost-eur": 0.0005,
      "x-apiosk-target-buyer-price-eur": 0.03,
      "x-apiosk-plans": [
        "marketplace_subscription"
      ],
      "x-apiosk-free-plan": false,
      "x-apiosk-sellable": false,
      "x-apiosk-sellable-reason": "Blocked by EODHD's resale prohibition. See info.x-apiosk-resale-status."
    },
    "response_body": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "string",
      "contentEncoding": "binary",
      "contentMediaType": "application/pdf",
      "description": "Binary PDF document containing the generated research report. Not a JSON body."
    },
    "request_example": {
      "isin": "US59018YTM39",
      "email": "user@example.com",
      "fmt": "json"
    },
    "response_example": null,
    "tags": [
      "marketplace analytics",
      "eodhd",
      "financial data"
    ],
    "sectors": [
      "finance",
      "investment",
      "capital markets"
    ],
    "use_cases": [
      "Generate and download a multi page PDF bond research report."
    ],
    "input_entities": [
      "isin",
      "email",
      "isFull",
      "fmt"
    ],
    "output_entities": [
      "marketplace analytics"
    ],
    "endpoint_category": "finance",
    "action_type": "search",
    "task_summary": "Generate and download a multi page PDF bond research report.",
    "external_docs_url": "https://eodhd.com/marketplace/praams/reports/docs",
    "documentation_markdown": "GET /api/mp/praams/reports/bond/{isin} — Generate and download a multi page PDF bond research report.\n\nGenerates a multi page PDF research report for a single bond, built on six return factors and six risk factors, and returns it as a file download. Set isFull to choose between the full and the partial report. The response body is a PDF document rather than JSON, so this endpoint is for delivering research to people, not for machine parsing.\n\nEODHD call weight: 10; required plans: marketplace_subscription.\n\nMarketplace product with its own subscription, separate from the main EODHD plans. Limits are 100 000 API calls per 24 hours and 1000 requests per minute, and 1 request equals 10 API calls. For marketplace products the 24 hour window starts at the first request rather than at a fixed daily reset. PRAAMS response bodies are wrapped in a standard envelope with success, message, errors and item (or items for collections), and the detailed payload schemas live in the shared component file https://eodhd.com/vendor/marketplace/praams/components.yaml rather than in the endpoint specification. Error codes are non standard: 415 for a wrong token, 420 for a cancelled operation and 430 for data not found. Note that the ticker or ISIN is appended as a path segment after the literal ticker or isin segment. The email query parameter is required, which is unusual for a read only data API; the report generation flow sends notifications to that address. The response is a PDF, so fmt does not apply. Report generation takes time, so treat this as a slow endpoint.\n\nStaged only: resale and redistribution require a signed EODHD agreement before Apiosk may activate this endpoint.",
    "operation_id": "mp_praams_reports_bond"
  }
]$eodhd_endpoints$::jsonb)
)
insert into public.api_endpoints (
  id, api_id, method, path, price, active, description, query_params,
  response_body, request_example, response_example, tags, search_terms,
  use_cases, sectors, input_entities, output_entities, endpoint_category,
  action_type, task_summary, payment_required, external_docs_url,
  documentation_markdown, metadata_source, metadata_confidence, metadata_at
)
select
  (row->>'id')::uuid,
  'b268fe55-41d2-5b7a-b147-31af1f1c1d0f'::uuid,
  row->>'method',
  row->>'path',
  (row->>'price')::numeric,
  false,
  row->>'description',
  row->'query_params',
  row->'response_body',
  row->'request_example',
  row->'response_example',
  array(select jsonb_array_elements_text(row->'tags')),
  array[
    row->>'operation_id',
    row->>'path',
    row->>'description'
  ]::text[],
  array(select jsonb_array_elements_text(row->'use_cases')),
  array(select jsonb_array_elements_text(row->'sectors')),
  array(select jsonb_array_elements_text(row->'input_entities')),
  array(select jsonb_array_elements_text(row->'output_entities')),
  row->>'endpoint_category',
  row->>'action_type',
  row->>'task_summary',
  true,
  row->>'external_docs_url',
  row->>'documentation_markdown',
  'provided_openapi_2026-09-07',
  0.95,
  now()
from endpoint_rows
on conflict (id) do update set
  api_id = excluded.api_id,
  method = excluded.method,
  path = excluded.path,
  price = excluded.price,
  active = false,
  description = excluded.description,
  query_params = excluded.query_params,
  response_body = excluded.response_body,
  request_example = excluded.request_example,
  response_example = excluded.response_example,
  tags = excluded.tags,
  search_terms = excluded.search_terms,
  use_cases = excluded.use_cases,
  sectors = excluded.sectors,
  input_entities = excluded.input_entities,
  output_entities = excluded.output_entities,
  endpoint_category = excluded.endpoint_category,
  action_type = excluded.action_type,
  task_summary = excluded.task_summary,
  payment_required = excluded.payment_required,
  external_docs_url = excluded.external_docs_url,
  documentation_markdown = excluded.documentation_markdown,
  metadata_source = excluded.metadata_source,
  metadata_confidence = excluded.metadata_confidence,
  metadata_at = excluded.metadata_at;

do $postconditions$
declare
  endpoint_total integer;
  endpoint_active integer;
  source_visible boolean;
  credential_leaks integer;
begin
  select count(*), count(*) filter (where active)
    into endpoint_total, endpoint_active
  from public.api_endpoints
  where api_id = 'b268fe55-41d2-5b7a-b147-31af1f1c1d0f'::uuid;

  select status = 'active' or active or discoverable or is_verified
    into source_visible
  from public.apis
  where id = 'b268fe55-41d2-5b7a-b147-31af1f1c1d0f'::uuid;

  select count(*) into credential_leaks
  from public.api_endpoints
  where api_id = 'b268fe55-41d2-5b7a-b147-31af1f1c1d0f'::uuid
    and query_params->'properties' ? 'api_token';

  if endpoint_total <> 60 or endpoint_active <> 0 or coalesce(source_visible, true) or credential_leaks <> 0 then
    raise exception 'EODHD staging invariant failed: endpoints=%, active_endpoints=%, visible=%, credential_inputs=%',
      endpoint_total, endpoint_active, source_visible, credential_leaks;
  end if;
end;
$postconditions$;

commit;
