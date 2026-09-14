# Reuters and Bloomberg news via RapidAPI — 14 September 2026

Two platform-owned `apiosk_revenue` listings, resold from Apiosk's RapidAPI subscriptions. Upstream credentials live only in `listing_metadata.auth_headers` (`x-rapidapi-key`, `x-rapidapi-host`); they are not in migrations, fixtures or evidence, and public metadata strips them.

| Source | Slug | Capabilities (category `news`) | Upstream quota | Buyer price per call |
|---|---|---|---|---|
| Reuters News | `reuters` | `news.reuters-latest`, `news.reuters-search`, `news.reuters-sections`, 18× `news.reuters-section-<section>` | 10,000/month (operator-reported) plus a per-second limit | 47,259 list + 7,089 fee = 54,348 micro-USD |
| Bloomberg News | `bloomberg` | `news.bloomberg-latest` | 500/month, free plan | same |

## Operations

| Capability | Route | Input | Output fact |
|---|---|---|---|
| `news.reuters-latest` | `GET /latest` | optional `news.reuters_result_limit` (1–12) → `limit` | `news.reuters_latest_headlines`: `[{link,title,published_at}]` |
| `news.reuters-search` | `GET /search` | `news.reuters_search_query` (2–120 chars) → `q`; optional limit | `news.reuters_search_headlines`: `[{link,title}]` |
| `news.reuters-section-<s>` | `GET /category` | none; `url` fixed per section | `news.reuters_section_headlines`: `[{link,title,position}]` |
| `news.reuters-sections` | `GET /sections` | none | `news.reuters_sections`: `[{name,url}]` |
| `news.bloomberg-latest` | `GET /news/list` | none; `id=phx-latest` fixed | `news.bloomberg_latest_stories`: whole envelope, `status` must be `true` |

Sections: World, Africa, Americas, Asia Pacific, China, Europe, India, Middle East, UK, US, Business, Markets, Technology, Legal, Sustainability, Science, Sports, Lifestyle.

## Not offered (registered inactive, with reason)

- Reuters `/details`: returns text synthesized from the URL (title-cased slug, boilerplate body, URL date, default logo), not the article.
- Reuters Breakingviews section: HTTP 502 twice.
- Bloomberg `/news/list-by-region`, `/stories/list`, `/stories/detail`, `/stock/get-financials`, `/stock/get-keystats`: HTTP 406 Not Acceptable twice (Bloomberg CDN block passed through).

## Design notes

- The intent parser only accepts input values that appear literally in the question. A section URL never does, and the provider rejects short values (`url=africa` → HTTP 400). Each section is therefore its own capability with the URL in reviewed `fixed_params`; migration `20260914110000` retired the initial URL-input `news.reuters-section` contract and `news.reuters_section_url` fact.
- Provider errors never become data: Reuters outputs are arrays (RapidAPI error objects are rejected); Bloomberg requires `status: true` and a `data` object. Empty Reuters pages are accepted as empty results.
- Search results have no date field and loose keyword matching; coverage notices say so.

## Verification

- `cargo test --test rapidapi_news`: catalog build, resolution, binding, full-capture extraction, error rejection, fixed sections.
- `node scripts/prepare-rapidapi-news.mjs --check`: fixture and both migrations match the generator.
- `node scripts/check-rapidapi-news-live.mjs [--pay]`: production `/v1/do`; 5/5 dry runs ready and 5/5 paid executions returned upstream HTTP 200.
- `cargo run --example check-rapidapi-news`: production catalog + configured intent model; 10/10 English and Dutch questions chose and bound the expected capability.
- `/v1/source-readiness`: both `planner-ready`, `available_in_v2`.

Evidence: `tests/fixtures/rapidapi-news/measurements.json` and captures; operator records under `evidence/2026-09-14/rapidapi-news/`.

Both migrations were applied with `supabase db query --linked -f` and recorded in `supabase_migrations.schema_migrations` under their file versions, because `db push` would also have applied the unreleased organisation-workspace migrations and the history already has two remote-only versions (`20260913121701`, `20260913134913`).
