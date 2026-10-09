# Bright Data LinkedIn

The `brightdata` source provides four single-URL operations:

| Operation | Input URL | Bright Data dataset |
| --- | --- | --- |
| `POST /linkedin/profile` | `https://www.linkedin.com/in/{name}` | `gd_l1viktl72bvl7bjuj0` |
| `POST /linkedin/company` | `https://www.linkedin.com/company/{name}` | `gd_l1vikfnt1wgvvqz95w` |
| `POST /linkedin/job` | `https://www.linkedin.com/jobs/view/{id}` | `gd_lpfll7v5hcqtkxl6l` |
| `POST /linkedin/post` | `https://www.linkedin.com/posts/{slug}` | `gd_lyy3tktm25m4avu764` |

Each request accepts only `{"url":"https://www.linkedin.com/company/microsoft"}` with the URL appropriate to that operation. Dataset IDs, batch inputs, webhooks, and arbitrary snapshot downloads are not caller parameters.

Gateway v2 starts one asynchronous collection after approval, budget and catalog validation. Pending collections use the existing task waiting flow, before a buyer reservation. It checks authorization again when the record is ready, then executes the normal purchase against the cached record. Direct executor callers receive HTTP 503 with `Retry-After` while collection is pending and can retry the same operation and URL. Pending responses do not settle a buyer payment. A completed response contains `data`, `source_url`, `notice`, and `coverage`. Public records are cached by canonical URL; callers cannot retrieve snapshots by ID. An ambiguous submission is retained for operator reconciliation instead of automatically starting another paid scrape.

Collection uses the platform's Bright Data account before customer settlement. Cancellation, later budget refusal, or a failed collection may leave a provider cost that Apiosk absorbs. An approval is rechecked before the buyer reservation; it cannot be used to charge a canceled task. The shared URL claim prevents duplicate collection submissions for the same cache entry.

Each successful operation is priced at EUR 0.05 with the standard 15% fee included, using the configured billing rate of EUR 0.92 per USD dated 2026-08-21. Underlying catalog and settlement amounts remain in USD: the list price is USD 0.047258 (47,258 micro-USD), the fee rounds up to USD 0.007089 (7,089 micro-USD), and the buyer total is USD 0.054347. Conversion at that billing rate gives EUR 0.04999924, which rounds up to EUR 0.050000 at six decimal places. The EUR amount depends on the configured billing rate and must be reviewed if that rate changes. Apply `brightdata_five_cent_prices_and_examples` to set this price and matching examples for all four operations. Each operation is restricted to one URL and one returned record. Bright Data's separate published pay-as-you-go provider cost, checked 2026-10-08, is USD 0.0015 per delivered LinkedIn record; the account's actual provider plan determines Apiosk's cost. Provider errors and empty/error records are not successful data.

Profile position, experience, and education may be absent or cached because of LinkedIn's field restrictions. `current_company.industry` is unsupported. The source does not promise complete or current employment history.

## Provision and release

1. Apply the `onboard_brightdata_linkedin` and `brightdata_v2_admission_access` migrations. They stage the source and four documented operations, populate taxonomy/search metadata, and create a private cache accessible only to the executor and gateway runtime. The migrations contain no credentials and leave the source inactive.
2. Set `BRIGHTDATA_API_KEY` in the ignored `gateway/.env` or the server process environment. `node scripts/provision-brightdata.mjs --check` checks presence without printing it. `node scripts/provision-brightdata.mjs` stages it in Fly through stdin; it does not restart the app or activate the source. Never store the key in public listing metadata, migration files, frontend variables, or request examples.
3. Deploy the gateway image containing the shared adapter and updated `legacy-executor`, plus the App's updated `use-selection` and `agent-gateway` Edge Functions for explicit unpaid-outcome mapping. Both gateway processes inherit the Fly server secret. The local `./dev` launcher uses the configured remote execution functions; a local provider test must start the embedded executor with this key and a database role permitted to use the private cache. Run `node scripts/check-brightdata-crate-sync.mjs` before building to verify the gateway and executor share identical adapter code.
4. Perform one authorized live scrape for each operation to confirm credentials, dataset access, response shape, pending-to-ready retrieval, and pricing. Exercise the adapter, not only Bright Data directly. Keep the source staged if any operation is unverified. Verify no buyer payment settles for pending/error responses.
5. Activate only the verified operations and source in a follow-up migration; record the validation evidence without personal record contents or the key. Existing catalog refresh rebuilds the search index automatically. Verify both catalog discovery and a completed Apiosk execution.

No live scrape or activation is implied by a passing local fixture test. Do not reset `submitting` or `unknown` jobs until their upstream outcome is reconciled, because resetting them can cause a second provider charge.

The repository's Dockerfile builds the operator binary, which checks the same shared adapter using secrets already present on Fly. Its availability depends on the deployed image: the live image observed at `be0c668` does not contain it. Check for the executable before using these commands; do not replace a newer gateway image with an older probe image.

```sh
fly ssh console --app apiosk-gateway-v2 -C 'test -x /usr/local/bin/check-brightdata-linkedin'
```

When the executable is present:

```sh
fly ssh console --app apiosk-gateway-v2 -C 'check-brightdata-linkedin company https://www.linkedin.com/company/microsoft'
```

Exit code 2 means the collection is pending; repeat the same command after at least ten seconds. Exit code 0 means one completed record. Other exit codes require inspecting the reported error code before continuing. Output contains only status, record count and timing, never credentials, snapshot IDs or record contents. Each collection invocation is bounded to 28 seconds including database setup.

After activation, append `--purchase` to check one completed Apiosk operator purchase, capped at USD 0.054347 (EUR 0.05 at the configured billing rate above). That mode requires the same URL's unexpired ready record and an active endpoint with list price USD 0.047258, uses the internal service credential on loopback, and never starts a provider collection directly. It makes one HTTP request with a stable idempotency key; an unconfirmed outcome must be reconciled before another purchase attempt. Reusing an existing probe key can replay an earlier paid receipt at its original price; the cap is a spending limit, not proof that a new five-cent charge occurred. A purchase check may take up to 110 seconds.

Successful verification requires HTTP 200, `did=true`, `execution.paid=true`, a successful numeric execution status, a valid request ID, no execution error, and a positive reported charge within the cap. This platform-owned source settles as `apiosk_revenue` through `apiosk_balance`; the legacy receipt deliberately omits an on-chain transaction hash. A missing `tx_hash` therefore does not invalidate an otherwise successful purchase. The probe reports `tx_present` only as diagnostic information. Keep deployment and validation records free of API keys and scraped record contents.

## Production validation, 2026-10-08

All four public LinkedIn examples completed with one record through the shared adapter. The first company submission was explicitly rejected without a snapshot; a subsequent diagnostic submission was accepted, reconciled into the private job, and retrieved successfully. No ambiguous submission was replayed. The activation migration is `20261008104542_activate_brightdata_linkedin.sql`; branding, examples and current five-cent display pricing are in `20261008105950_brightdata_five_cent_prices_and_examples.sql`.

An operator purchase on gateway `be0c668` returned HTTP 200, `did=true`, `paid=true`, `payment_rail=apiosk_balance` and charged USD 0.054347 (request `4312dfe1-d659-45ac-94eb-c991b4193175`). This is EUR 0.05 at the configured display rate. Supabase `use-selection` v79 and `agent-gateway` v76 are active with their existing authentication modes. Netlify deployment `6ac77863f0cb2ccd369c8448` publishes the four composer examples. The live source page was visually checked for the official Bright Data mark, all four EUR 0.05 prices, and an expanded example question with its LinkedIn URL.

## Provider references

- [LinkedIn request guide](https://docs.brightdata.com/products/scrapers/linkedin/send-first-request)
- [Asynchronous collection](https://docs.brightdata.com/api-reference/rest-api/scraper/asynchronous-requests)
- [Progress](https://docs.brightdata.com/api-reference/scrapers/management-apis/monitor-progress)
- [Snapshot download](https://docs.brightdata.com/api-reference/scrapers/delivery-apis/download-snapshot)
- [LinkedIn field restrictions](https://docs.brightdata.com/products/scrapers/linkedin/data-policy-change)
- [LinkedIn pricing](https://brightdata.com/products/web-scraper/linkedin)
