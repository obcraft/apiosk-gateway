# API.market category catalog

Fetch public API listings grouped by category, with no account, provider calls, purchases, or production database writes:

```sh
node scripts/apimarket-catalog.mjs --out=/tmp/apiosk-apimarket-catalog
node scripts/apimarket-catalog.mjs --category=data-web-intelligence --category=finance --out=/tmp/apimarket-selected
node --test scripts/apimarket-catalog.test.mjs
```

With no category arguments the script fetches all canonical categories in API.market's public sitemap (22 when inspected on 2026-10-08). Repeat `--category` to select several. Unknown categories fail before category requests are sent. The output directory contains:

- `categories.json`: category names, URLs, subcategory filters, listing counts and product IDs.
- `categories.csv`: a compact category/count summary for spreadsheets.
- `products.json`: unique products keyed by workspace/product slug, titles, descriptions, all discovery categories, public product URLs, and published pricing-plan metadata.
- `report.json`: completion status, failures, duplicate memberships and differences from the sitemap's product URLs.

The script reads the structured `__NEXT_DATA__` payload delivered with each public category page. The current category UI filters and sorts that full array locally; it exposes no pagination or authoritative total. `complete: true` means every selected page was fetched and parsed, **not** that every marketplace product has been found. Products present only in the sitemap are reported separately. A new pagination field, missing payload, changed page shape or failed HTTP request makes the run incomplete. Incomplete runs preserve successfully fetched records and exit with status 1. Do not use an incomplete run to remove existing records.

Requests are sequential, use a 30-second timeout, stop at 5 MiB per response, reject redirects and wait 1.1 seconds between requests. This follows the public `robots.txt` crawl delay. The fetcher never visits `/api/`, auth, account, private or seller pages. It accepts only the fixed API.market sitemap and canonical category URLs. Public site markup can change; this is a website metadata adapter rather than a documented catalog API.

Pricing plans are snapshots, not executable per-call quotes. A free monthly plan does not establish free calls for an existing account, and plan metadata may omit currency or endpoint-specific unit costs. Provider websites are left null because the category payload does not provide them. The public product page remains the source link. External approval status is preserved as metadata and does not approve a product inside Apiosk.

Execution is a separate integration: API.market documents an `x-api-market-key` header and an active subscription for per-product MCP endpoints. Its newer gateway also offers OAuth and pay-as-you-go. No execution credentials are needed for this export, and the export does not make these APIs runnable in Apiosk.

Official references inspected on 2026-10-08:

- [Sitemap](https://api.market/sitemap.xml), [crawl policy](https://api.market/robots.txt)
- [Example category](https://api.market/category/data-web-intelligence)
- [MCP gateway](https://api.market/mcp), [per-product MCP guide](https://docs.api.market/api.market-mcp-server-guide)
- [Authenticated product and subscription API](https://docs.api.market/subscription-management-api-documentation)

No explicit catalog redistribution license was found in the public documentation reviewed. The fetcher preserves attribution and source URLs; it does not represent public listings as permission to execute or resell their underlying APIs.

## Business category onboarding

Migration `20261008110548_onboard_apimarket_business_sources.sql` registers the 37 reviewed `business-apis` listings from the October 8 catalog. Its source manifest is `tests/fixtures/apimarket/business-sources.json`. Each product retains its own source identity, category, subcategory tags, documentation URL and attributed pricing-plan snapshot under `listing_metadata.apimarket`.

The existing Sources query, Ask search and planner source catalog read these registrations. They are **discovery-only**: no API.market credential, executable endpoint, quoted price or execution contract is created. `is_verified` records the listing's metadata review, not proof of a successful API response. Execution requires a separately configured connection and validated operations. Reapplying the migration preserves existing hidden/suspended states and rejects identity collisions.

```sh
node scripts/test-apimarket-business-migration.mjs
cargo run --example audit-apimarket-business
```

The audit uses `DATABASE_URL` from the environment or `.env`, forces its database session read-only, and checks the currently curated Sources/planner entries, branding, endpoint page and the two validated Crunchbase operations. It makes no model or provider calls.

Production verification on October 8 admitted all 37 entries to Sources and the planner catalog. All three representative searches matched the relevant imported source. Marketing-name/tagline probes matched 36 of 37; Local Business Data's broad tagline did not pass the existing capability relevance filter. The audit reports such search misses separately without weakening relevance gates or treating catalog presence as an execution guarantee.

## Current curation

At the owner’s request, `20261008115557_show_only_crunchbase_apimarket.sql` hides 36 imported listings using `discoverable=false` and keeps only **Crunchbase** visible. The shorter title has no emoji; its logo is the official `https://www.crunchbase.com/favicon.ico` (visually verified). Original metadata and the original title are retained. Hidden listings are excluded from both Sources and the planner.

The user-supplied API.market key is stored in Vault as `APIMARKET_API_KEY`; no key value is kept in this repository. The abandoned admin-form RPCs have been removed. Storing the key does not enable execution.

## Crunchbase execution

`20261008122054_register_crunchbase_endpoints.sql` connects two operations to the existing Crunchbase source:

- **Find companies by name**: `POST /autocompletes`, with a company-name query.
- **Browse companies**: `POST /searches/organizations`, with a result limit and fixed unfiltered request parameters.

Both returned HTTP 200 with structured company results during the October 8 live verification. The existing authenticated subscription is **Free**, with a shared hard limit of **100 calls per month** and no overage. Apiosk lists these operations at zero cost. No subscription was purchased or changed. When the quota is exhausted, execution can fail until it resets.

The embedded executor loads the key from Vault and injects `x-api-market-key` only for the exact reviewed HTTPS origin and these two POST routes. Planner contracts supply the required browse defaults and reject provider errors or malformed responses. The other 36 imported listings stay hidden.

`POST /entities/organizations` is retained as inactive: it returned HTTP 500 for two valid company identifiers. Advanced search filters are not offered because the provider returned unfiltered results during verification. Neither feature is represented as working.

Run `cargo test --test crunchbase` for planner binding and output validation, and the embedded executor's `apimarket::tests` for credential route restrictions. `cargo run --example audit-apimarket-business` checks the live catalog without executing provider calls. [Provider documentation](https://docs.api.market/api-product-docs/magicapi/crunchbase-api).
