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

Bright Data's published pay-as-you-go price, checked 2026-10-08, is USD 0.0015 per delivered LinkedIn record. Each operation is restricted to one URL and one returned record. The catalog list price is USD 0.0015, with Apiosk's normal fee applied by the existing payment flow. Account-specific provider pricing must be checked during live activation. Provider errors and empty/error records are not successful data.

Profile position, experience, and education may be absent or cached because of LinkedIn's field restrictions. `current_company.industry` is unsupported. The source does not promise complete or current employment history.

## Provision and release

1. Apply the `onboard_brightdata_linkedin` and `brightdata_v2_admission_access` migrations. They stage the source and four documented operations, populate taxonomy/search metadata, and create a private cache accessible only to the executor and gateway runtime. The migrations contain no credentials and leave the source inactive.
2. Set `BRIGHTDATA_API_KEY` in the ignored `gateway/.env` or the server process environment. `node scripts/provision-brightdata.mjs --check` checks presence without printing it. `node scripts/provision-brightdata.mjs` stages it in Fly through stdin; it does not restart the app or activate the source. Never store the key in public listing metadata, migration files, frontend variables, or request examples.
3. Deploy the gateway image containing the shared adapter and updated `legacy-executor`, plus the App's updated `use-selection` and `agent-gateway` Edge Functions for explicit unpaid-outcome mapping. Both gateway processes inherit the Fly server secret. The local `./dev` launcher uses the configured remote execution functions; a local provider test must start the embedded executor with this key and a database role permitted to use the private cache. Run `node scripts/check-brightdata-crate-sync.mjs` before building to verify the gateway and executor share identical adapter code.
4. Perform one authorized live scrape for each operation to confirm credentials, dataset access, response shape, pending-to-ready retrieval, and pricing. Exercise the adapter, not only Bright Data directly. Keep the source staged if any operation is unverified. Verify no buyer payment settles for pending/error responses.
5. Activate only the verified operations and source in a follow-up migration; record the validation evidence without personal record contents or the key. Existing catalog refresh rebuilds the search index automatically. Verify both catalog discovery and a completed Apiosk execution.

No live scrape or activation is implied by a passing local fixture test. Do not reset `submitting` or `unknown` jobs until their upstream outcome is reconciled, because resetting them can cause a second provider charge.

## Provider references

- [LinkedIn request guide](https://docs.brightdata.com/products/scrapers/linkedin/send-first-request)
- [Asynchronous collection](https://docs.brightdata.com/api-reference/rest-api/scraper/asynchronous-requests)
- [Progress](https://docs.brightdata.com/api-reference/scrapers/management-apis/monitor-progress)
- [Snapshot download](https://docs.brightdata.com/api-reference/scrapers/delivery-apis/download-snapshot)
- [LinkedIn field restrictions](https://docs.brightdata.com/products/scrapers/linkedin/data-policy-change)
- [LinkedIn pricing](https://brightdata.com/products/web-scraper/linkedin)
