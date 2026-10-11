Apiosk: source onboarding export contract
Verified against apiosk-production (jgjoiyqdyypouskftzeq) on 11 October 2026.

DELIVERABLE

The separate onboarding tool should collect documentation + credentials, run
bounded live tests on its backend, ask AI for a normalized JSON bundle, validate
that bundle, and export ONE SQL transaction per new source. The AI should never
write executable SQL or receive secret values. render.mjs emits SQL deterministically.

This package defines and tests that export format. It does not build the tool UI,
call a model, test a real provider, install a runtime adapter or modify production.
The example source is fictional; its zero owner UUID must be replaced.

FILES

onboard-source.template.sql   Transactional registration/import template.
example-onboard-source.sql    Fully expanded fictional example, ready to inspect.
manifest.example.json        Complete source/endpoint/capability bundle example.
manifest.schema.json         Allowed envelope fields for the AI/backend export.
normalization.prompt.txt     Prompt contract for the normalization model.
render.mjs                   Offline JSON validator and safe SQL renderer.
test.mjs                     Local PostgreSQL/PGlite transaction tests.
schema-snapshot.json         Actual production columns, constraints and triggers.

TABLES THE IMPORT WRITES (all already exist)

1. providers.apis
   One source row: owner_id, slug, name, description/long_description, origin_url,
   endpoint_url, docs_url, website_url, github_url, category, example_usage,
   authentication_info, listing_type, integration_mode, commercial settlement,
   visibility/review state and listing_metadata.

2. providers.api_endpoints
   One row per method/path, linked by api_id. Actual input/output JSON schemas,
   examples, documentation, headers, origin_override, cache TTL, price, tags,
   search_terms, use_cases, sectors, input_entities/output_entities,
   endpoint_category, action_type, task_summary and metadata provenance.
   UNIQUE(api_id,method,path). The bundle uses a local key; SQL resolves real UUIDs.

3. catalog.fact_subject_kinds
   Subject vocabulary, e.g. company, person, address. Usually reuse existing rows.

4. catalog.fact_types
   Typed facts: slug, subject_kind, JSON value_schema, jurisdiction,
   freshness_seconds, match_hints and status. Reuse exact meanings/schemas.

5. catalog.capabilities
   What a user can request: slug, name, description, category, input_contract,
   output_contract and status. One capability can have several source offers.

6. catalog.capability_execution_contracts
   Actual capability -> endpoint binding. requires, produces, input_map,
   fixed_params, output_map, cardinality, identifies, max_fan_out, pagination,
   freshness, effect, price_model, adapter_version, sample_payload and evidence.
   UNIQUE(capability_slug,endpoint_id,version). These are separate from labels.

7. catalog.capability_offers (optional)
   Legacy discovery links with source=declared|curated|inferred and confidence.
   These do not replace execution contracts or validate an API. For these links,
   input_map uses capability input names; execution contracts use fact slugs.

Optional secret provisioning calls vault.create_secret, adding encrypted Vault
records. Vault is existing infrastructure, not a new credential table.

INSPECTED, BUT NOT REQUIRED / NOT WRITTEN BY THIS IMPORT

- catalog.param_resolvers DOES exist in the catalog schema. It describes how to
  resolve a missing parameter using another endpoint. Not required for a source
  with directly supplied inputs; it is not the core typed contract above.
- providers.provider_api_keys stores hashed Apiosk provider-access keys, not
  third-party API credentials to forward upstream.
- providers.source_providers is discovery/curation inventory, not the canonical
  providers.apis source registration.
- providers.endpoint_pricing_rules is parameter-dependent legacy pricing. This
  v1 exporter deliberately supports fixed per-call prices only; the current v2
  catalog takes e.price. Do not add dynamic rules without verifying quote/charge parity.
- provider_profiles is an existing owner/profile dependency. The importer checks
  auth.users for the supplied owner, and does not create an account or organisation.
- endpoint_contract_coverage and federated_execution_readiness are derived views;
  do not INSERT registrations into them. Federated/x402 registration is a different
  flow and is outside this gateway-API template.

There are no separate mandatory tags/categories tables in this onboarding path.
Source category is apis.category; endpoint category/tags are api_endpoints columns;
capability category is capabilities.category. Source-level tags/logo/licensing/
coverage may also live in listing_metadata. Gateway discovery aggregates endpoint
tags, so populating only source metadata is insufficient.

The snapshot is an inventory of existing tables, not a proposal to create all of
them. Its import_tables and inspected_but_not_written keys make that distinction.
Observed counts: 709 total source rows; 267 pass source visibility filters; 93
visible, unblocked sources have active contracts across 476 endpoints / 567
contracts. These counts do not certify runtime acceptance or successful live calls.

CREDENTIALS AND HEADERS: CURRENT RUNTIME, NOT ASSUMPTIONS

authentication_info and authentication_notes describe auth. They do not inject it.
api_endpoints.headers describes headers; this is not a general secret resolver.

The existing generic executor injects these literal runtime formats:

  listing_metadata.auth_headers = [{"name":"X-Api-Key","value":"ACTUAL_VALUE"}]
  listing_metadata.auth_query   = [{"name":"api_key","value":"ACTUAL_VALUE"}]

Bearer/basic values are complete literal header values in that older path.
api_endpoints.auth_override uses that same object shape and REPLACES source auth.
If origin_override is set without auth_override, inherited credentials are removed
to avoid forwarding a source credential to another host. A public static header
may also need explicit endpoint handling on an override route.

Do not place a key in those JSON fields from the new tool. Live column grants for
listing_metadata/authentication_info include anon/authenticated; grants alone do
not prove exposure because RLS/views also matter, but these are not an acceptable
new private secret storage design. This task did not fetch or expose stored keys.

Vault stores encrypted secrets and returns them through a privileged decrypted
view: https://supabase.com/docs/guides/database/vault
The current API.market/Crunchbase code has provider-specific Vault handling.
It does NOT resolve arbitrary {vault_name: ...} values for every source.

credential_requirements in this bundle are private-backend requirements recorded
as non-secret onboarding metadata. The fields identify Vault name, header/query
location, prefix, endpoint scope and allowed origin. They are NOT executable auth
bindings yet. No fake auth_ref/auth_secret_ref runtime support is claimed.

To provision a new key atomically with registration, use a dedicated trusted
server connection:

  1. Parameter-bind SELECT set_config('apiosk.onboarding_secrets', $1, false).
     $1 is JSON, e.g. {"APIOSK_EXAMPLE_COMPANY_KEY": "<actual secret>"}.
  2. Execute the rendered SQL on that SAME connection. It creates Vault secrets
     inside the source transaction, and rolls them back if another insert fails.
  3. In finally, ROLLBACK if necessary; clear the setting and close/discard the
     connection. A transaction-local reset does not clear the prior session value
     after COMMIT. Never return that connection to a pool with the setting present.

SQL Editor-only import: register the source using the generated file, then supply
credentials through a trusted server/Vault UI. A shareable SQL file cannot contain
the real API key and also keep it out of migration history. Parameter binding
requires a backend/driver; never interpolate the key into SQL or a shell command.
Redact bind parameters in backend/database logging; never pass them to the AI.
Existing Vault names are never overwritten; rotation is a separate operation.

To make authenticated new sources work generically, implement a one-time executor
change: load approved private credential bindings, fetch only the allowed Vault
secret with the worker role, validate the exact origin/method/path, inject it only
after routing, strip buyer overrides, and redact errors/traces. OAuth refresh,
HMAC signing and multi-step auth need corresponding execution support. Merely
importing the secret and its name is insufficient. This change is not in this package.

PRICE AND COMMERCIAL FIELDS

This template targets platform-operated third-party APIs: listing_type=api,
integration_mode=gateway, is_platform_owned=true, settlement_mode=apiosk_revenue,
requires_payout_wallet=false. It does not create provider payouts or wallets.
External independent providers / federated x402 need a different commercial setup.

api_endpoints.price is the pre-markup USD listing amount, not micro-USD and not EUR.
The gateway computes ceil(price * 1,000,000) for list_atomic. The template limits
precision to six decimals and sets payment_required=(price>0). Example 0.050000
becomes 50,000 list micro-USD; standard 15% markup gives 57,500 buyer micro-USD.
Workflow markup can differ (currently 20%). Use the actual gateway fee calculator
for quotes. commission_rate on apis is not the v2 buyer-markup control.
Store upstream cost, licence, quotas and attribution separately as reviewed
metadata; metadata does not enforce quotas, retries, timeouts or rate limits.
network_token_id is left null (existing gateway default); no made-up token UUID.

AI NORMALIZATION AND VALIDATION

Capture docs/OpenAPI + redacted real request/response evidence. Record model ID,
document hashes, generated_at, reviewer and live-test status. These provenance
fields are an export convention under listing_metadata.onboarding, not new DB columns.
metadata_confidence is confidence in an annotation, not proof an API works.

The backend must validate more than JSON shape:
- HTTPS origin and redirect policy, SSRF/private-address restrictions, host ownership.
- Method/path allowlists; no probe of write/payment endpoints without specific intent.
- Required path/query/body input locations and actual serialization.
- Bound live-test cost/time/bytes/page count, auth behavior, 4xx/5xx and empty results.
- Output scope/entity identity, pagination coverage, licensing and actual price.
- Runtime contract.check + extraction against executor-normalized real samples.
- Missing mappings, existing vocabulary conflicts and unsupported adapter names.

requires contains typed facts needed to call an endpoint. input_map maps each fact
to {"in":"path|query|body","name":"provider_parameter"}; optional inputs may have
a reviewed default. fixed_params holds provider constants, not secrets/AI guesses.
output_map.base selects a subject. Each produced fact is read relative to that
subject with {"path":"$.field"}, {"fields":{...}}, or a real registered adapter.
cardinality=one|many and identifies must match actual entity granularity. Arbitrary
JSONPath syntax is not supported; use catalog-core's dialect. A new adapter_version
string only labels a contract; it does not create or deploy implementation code.

New capabilities/facts/contracts are draft. Existing matching vocabulary is reused
without changing its state. Conflicting shared schemas or capability semantics
abort the transaction. A duplicate source slug also aborts without mutation; this
is retry-safe failure, not automatic upsert. After a lost connection, query by
slug/bundle_id before retrying. Updates need explicit, guarded version migrations.

GENERATE AND VERIFY (from gateway/)

The SQL test uses PGlite from the adjacent app checkout. Install its locked
dependencies first with `npm ci --prefix ../app` if they are not already present.
The renderer itself requires only Node.js; the runtime checker also requires
Rust and cached Cargo dependencies for the offline command below.

  node docs/source-onboarding/render.mjs your-source.json /absolute/path/onboard.sql
  node docs/source-onboarding/test.mjs
  cargo run --offline --manifest-path crates/catalog-core/Cargo.toml \
    --example check_source_bundle -- docs/source-onboarding/manifest.example.json

The Rust checker can take a second JSON file containing existing fact definitions
when the bundle references vocabulary without repeating its definitions.
It verifies the runtime contract/path/schema/extraction dialect, not HTTP input
binding, live evidence, licensing, approval or complete gateway acceptance.

When saving a reviewed export in Supabase migration history, first create its
filename with `supabase migration new onboard_<source>` in the owning checkout,
then put the generated SQL in that file. Templates/examples intentionally live
outside supabase/migrations so db push cannot publish a fictional source.

REGISTRATION VS PUBLICATION

The exported transaction completes ALL registration rows, plus optionally Vault
storage. It intentionally ends with source inactive/hidden/unverified, endpoints
inactive and contracts draft. There is no actual API specified in this request
whose successful tests/credentials/rights could justify a publishable export.

Actual production trigger behavior matters:
- apiosk_auto_verify_owner_apis can set is_verified on INSERT; the template resets
  it explicitly. Database owner verification is not an upstream live-test result.
- apiosk_apis_review_gate turns a fresh active transition into pending unless the
  existing admin approval path has authorized that API.
- apiosk_sync_apis_active_with_status derives active from status.
- pending/active transitions can enqueue review email / publication dispatch.
  The inactive registration avoids those transitions; no mail was sent here.

For the future tool's single final publish transaction AFTER backend tests/review:
1. Lock this exact source and assert bundle ID, origin, endpoint/contract hashes,
   reviewed tariff and credential binding against the tested export. Revalidate
   if anything drifted. Do not treat an AI boolean as evidence.
2. Persist real normalized samples + validated_at + validated_note on each tested
   contract. Activate only tested endpoints/contracts and their approved fact /
   capability definitions. A generic source must not use the special
   validated/source-document-v1 exception as an activation shortcut.
3. Set is_verified=true with the real reviewer and test time, discoverable=true,
   and remove only this onboarding planner_block_reason after its conditions pass.
   Keep unrelated suspensions/blocks. Keep status=pending until admin approval.
4. In the genuinely authenticated platform-admin context, call
     SELECT * FROM public.admin_review_api(<actual_source_uuid>, 'approve');
   This existing function checks is_active_platform_admin() and supplies its
   API-specific approval marker. Do not invent JWT claims in a generated migration.
   An ordinary SQL Editor postgres session is not automatically that auth context.
5. Assert status=active, active=true, verified/discoverable flags, no suspension /
   planner block; verify runtime Catalog::build has accepted the intended contracts.
   Then verify discover -> resolve/quote -> one approval -> execute -> saved result.

If review, runtime support and live evidence already exist before first import,
the tool can combine registration and those publication steps in ONE transaction
under the real admin session. The current template does not manufacture those
preconditions or change the existing admin approval mechanism.

READBACK WITHOUT SECRETS

  SELECT a.id,a.slug,a.status,a.active,a.discoverable,a.is_verified,
         a.listing_metadata->'onboarding'->>'bundle_id' AS bundle_id,
         e.id AS endpoint_id,e.method,e.path,e.active AS endpoint_active,e.price,
         c.capability_slug,c.version,c.status AS contract_status,c.validated_at
  FROM providers.apis a
  LEFT JOIN providers.api_endpoints e ON e.api_id=a.id
  LEFT JOIN catalog.capability_execution_contracts c ON c.endpoint_id=e.id
  WHERE a.slug='<your-source-slug>'
  ORDER BY e.method,e.path,c.capability_slug,c.version;

VALIDATION BOUNDARY

Local tests reconstruct the real columns/defaults/constraints and five relevant
source triggers from the read-only production snapshot. They verify inserts,
staging state, relations, vocabulary drift, duplicate protection, SQL quoting,
credential parameter binding, no secret in catalog metadata, no key overwrite
and atomic rollback. Vault encryption and notification/backoffice/search triggers
are not reproduced. No production SQL was executed except SELECT introspection.
Runtime extraction is additionally checked with the actual catalog-core crate.

SOURCE CODE AUTHORITY

gateway/src/catalog.sql                       runtime inclusion/price filters
gateway/src/catalog.rs                        Catalog::build acceptance checks
gateway/src/catalog/registered.sql            direct endpoint registration path
gateway/src/input_binding.rs                  request binding restrictions
gateway/crates/catalog-core/src/facts/contract.rs   typed mapping/extraction dialect
gateway/legacy-executor/src/proxy/browser.rs   literal header/query credential shape
gateway/legacy-executor/src/proxy/handle_flow.rs    auth precedence and Vault special cases
gateway/legacy-executor/src/proxy/forward.rs   target URL, query auth and normalization
schema-snapshot.json                          live DB columns, checks and triggers
