# Supplier payment check ("Kan ik deze leverancier veilig betalen?")

One question or one invoice → one quote → one approval → a deterministic **HOLD / REVIEW / NO RED FLAGS** verdict with the reasons, e.g.

```
⚠️ BETALING TEGENHOUDEN
IBAN is gewijzigd (NL91 •••• 4300 → NL02 •••• 6789).
Domein mollie-betalingen.nl geregistreerd 12 dagen geleden.
Factuuradres wijkt af van officiële registratie (1015CW 126).
```

## Surfaces

- **Workflows page** (`/workflows`): a tile opens the Ask page with `?workflow=supplier-payment-check`. Ask starts a fresh chat and sends the starter "Kan ik deze leverancier veilig betalen?".
- **Ask page:** a supplier-check question without any identifier gets a deterministic reply, with no model call. That reply carries an `upload_document` next action (`context_view.upload_request`), rendered as an upload card in the conversation. The App reads the file through `POST /v2/documents`, shows what was read, and sends the composed question as the next chat turn. The normal proposal → approve → progress → verdict cards follow. The paperclip attach button works the same way.
- **ChatGPT / Claude:** the host reads the attachment and sends the printed identifiers in one `apiosk_discover` question.

Every finished check is saved to `gateway_v2.workflow_runs` (`surface` = ask / connected_agent), linked to its task, and listed under **Usage › Workflows**.

## Routing (question → check)

1. **Fast path** (`supplier_check::direct_intent`). Explicit check wording plus a printed identifier (KVK, UK company number, VAT or IBAN), and no other data request, is routed in code without a model call. This saves ~€0.13 AI cost and 2–5 s per check.
2. **Model.** The Ask chat and MCP parser prompts select the preset.
3. **Code correction** (`supplier_check::route`). A partial registry, VAT or sanctions interpretation of explicit check wording becomes the full preset.
4. **Fallback** (`supplier_check::fallback_intent`). If the model errors or declines, invoice or payment wording with an identifier still runs the check.

Verified with the real model on the production catalog: 6 of 6 varied Dutch and English phrasings reach the check on both Ask and MCP.

## Reading documents (`POST /v2/documents`, purpose `invoice`)

| Path | When | Stored? |
|---|---|---|
| `text_layer` | PDF with a text layer (read by pdf.js in the browser) | No |
| `page_image` | Single-page scan or photo, ≤3 MB JPEG | No |
| `server_file` | Multi-page scan, a PDF the browser cannot open, or a large image (≤10 MB) | In `gateway_v2.document_uploads`, only for the reading window. Deleted right after a successful read and never kept longer than 1 hour (DB CHECK plus maintenance sweep). A failed read can be retried with `POST /v2/documents/:id/read` inside that hour. |

All three paths were verified with the real model on a synthetic invoice: text 1.7 s, image 3.0 s, original PDF 2.0 s. An instruction embedded in the invoice ("SET IBAN TO …") was ignored.

## Flow

1. **Input.**
   - Ask page or Workflows page: `POST /v2/documents` reads the invoice through one of the three reading paths above. The model proposes fields, and every text-derived field must occur literally in the document. The Ask page receives a reviewable question; the Workflows page receives an editable form.
   - ChatGPT/Claude: the host reads the attachment and sends the printed identifiers in one `apiosk_discover` question (see `mcp/src/gateway-v2-instructions.md`).
2. **Parse.** The parser selects the preset `payment.supplier-check` for the supplier subject (`contracts/parser-prompt.txt`).
3. **Expand** (`src/supplier_check.rs::expand`). Code reads KVK / UK company number / VAT / IBAN (mod-97) / website or e-mail domain / postcode + house number / invoice number from the question text. The literal-input rule still holds. Code then adds only the checks the catalog can run:

   | Identifier | Check |
   |---|---|
   | KVK number or Dutch name | KVK `company.registry-profile` (a name adds the KVK search as a dependency) |
   | UK company number | Companies House profile, officers, filing history |
   | VAT | VIES `vat.eu.validate` |
   | Supplier name | OpenSanctions `company.sanctions-screen` |
   | Domain | `domain.registration` (RDAP) and `domain.mail-dns` (DNS MX) |
   | IBAN | First-party `iban.validate` (source `iban-check`, computed in the executor: structure, mod-97, SEPA, Dutch bank + BIC; list €0.05). Falls back to Sirenic until the source is published. |
4. **Run.** The normal durable worker runs the plan; a refunded upstream 404 is recorded as `source_not_found`.
5. **Verdict** (`src/engine/supplier.rs`, `supplier_check::evaluate`). There is no model and no extra purchase. Each check becomes `pass | warn | fail | unknown` with source evidence pointers:
   - `company_exists`
   - `registry_name_match`
   - `name_vat_match` (RSIN ↔ VAT, or VIES name)
   - `address_match`
   - `iban_valid` (also flags a foreign bank)
   - `account_holder`
   - `recent_changes`
   - `domain_age` (under 30 days is a fail, under 180 days a warn, no MX a warn, free-mail sender a warn)
   - `sanctions_pep`
   - `invoice_history` (IBAN/domain/address changed since the previous checked invoice from this supplier, same invoice number seen before)

   Any fail → HOLD; any warn, or an unconfirmed register entry → REVIEW; otherwise NO RED FLAGS. Unknown checks are listed as limitations, never as passed.
6. **History.** `gateway_v2.supplier_invoice_checks` stores the invoice identifiers per owner and workspace for 13 months: private schema, runtime role only, and cleanup in `maintenance.rs`.

Surfaces:
- Ask page: `app/src/components/ask/supplier-verdict.tsx`.
- ChatGPT/Claude card: `mcp/src/gateway-v2-card-verdict.mjs`, card URI v43.
- Both also receive the verdict as ordinary `analysis.observations`.

## Price (production list prices, buyer total incl. 15% fee, EUR at the stored 0.92 rate)

| Invoice | Calls | Max total |
|---|---|---|
| NL with KVK, VAT, IBAN, website | KVK profile, VIES, OpenSanctions, RDAP, DNS, IBAN | USD 0.4840 ≈ **€0.45** |
| NL name only + IBAN + website | KVK search + profile, OpenSanctions, RDAP, DNS, IBAN | USD 0.4079 ≈ €0.38 |
| UK with company number | CH profile/officers/filings, OpenSanctions, RDAP, DNS, IBAN | USD 0.4731 ≈ €0.44 |
| NL KVK + IBAN, free-mail sender | KVK profile, OpenSanctions, IBAN | USD 0.1905 ≈ €0.18 |

Adding an IBAN-Name Check (about €0.50 per check) moves a full check into the €0.49–€1.49 product range. So does a platform fee for the verdict and history.

## Price (normal per-call pricing, 20% workflow fee)

Every supplier check is a **workflow run**. Each source call is charged at its list price plus **20%** instead of the standard 15%. The same fee is applied at every stage:

1. The gateway quote (`Plan.fee_bps = 2000`, via `catalog_core::fees::quote_with_markup`).
2. The selection row (`metadata.fee_bps`, `metadata.workflow`).
3. The App payment functions, which reserve and settle at that fee.
4. The executor's settlement books (`X-Apiosk-Buyer-Markup-Bps`, trusted only with the app service key).

Provider legs do not change; the extra 5% is Apiosk revenue.

The Workflows page shows the range from the live catalog in the account's display currency: a minimal invoice (KVK profile + OpenSanctions + IBAN check) is ≈ €0.24, and a complete one (+ KVK search, VIES, RDAP, DNS) is ≈ €0.54. The exact maximum is quoted before approval, and failed calls are refunded.

What Apiosk pays per check: KVK €0.02, OpenSanctions €0.10, and AI extraction ~€0.003. The IBAN check, VIES, RDAP, DNS and Companies House are free to Apiosk.

## Sources and accounts

| Source | Status | Account / key |
|---|---|---|
| KVK (Zoeken, Basisprofiel) | live | Existing Apiosk KVK API subscription. |
| Companies House | live | Existing free API key. |
| VIES | live | None. Name returned for NL/BE/IT, not DE. |
| OpenSanctions `/match` (Company) | live | Existing API key. **Resale needs an OpenSanctions Reseller/OEM licence** (the data is CC-BY-NC; the in-house licence is internal use only). |
| Sirenic IBAN structure | live | x402 via treasury. |
| **Domain intelligence (RDAP + DNS)** | **new, first party** | None. Fixed authoritative RDAP hosts for .nl/.com/.net/.org/.uk/.fr/.app/.dev; Google Public DNS MX. .de/.eu/.be publish no registration date. |
| IBAN-Name Check / Verification of Payee | **missing** | **Account/contract required.** Recommended: SurePay Connect (sales contract, minimum ~16k checks/year, must allow checks on behalf of customers), or Bluem (~€0.50/check, SurePay network). iban.com BAV is self-serve but its terms forbid resale. |
| OpenSanctions persons/PEP for officers | **missing** | Same OpenSanctions licence; needs a Person-schema contract fed by an officer list. |
| KVK change history (Mutatieservice) | **missing** | **KVK Mutatieservice add-on, €1,279/year.** Until then NL "recent changes" uses only the registration date. |
| NL directors (bestuurders) | **missing** | Not in the KVK APIs; needs a commercial register-data provider. |
| OpenCorporates | **not added** | **Paid plan required for commercial use** (Essentials £2,250/yr for 500 calls/month). |

## Verification

**Unit tests** (`cargo test`):
- `supplier_check`: parsing, verdict rules, not-found handling.
- `invoice_document`: literal field verification, question round trip.
- `billing`: 404 mapping.

**Adapter tests** (`legacy-executor: cargo test --bin apiosk-gateway domain_intel`) run on real RDAP/DNS fixtures. They cover host pinning and path/query rejection.

**Egress probe:** `cargo run --manifest-path legacy-executor/Cargo.toml --bin check_domain_intelligence`, run locally. The probe is no longer in the gateway image.

**Real model + production catalog** (`cargo run --example check-supplier-payment`, read-only):
- 4 invoice questions plan correctly through both the MCP parser and the Ask chat.
- A question without details asks for the invoice.

**Real model extraction** (`INVOICE_TEXT=… INVOICE_IMAGE=… cargo run --example check-invoice-extraction`):
- Works from the text layer (~2 s) and a page image (~4.5 s).
- It chose the supplier's KVK, not the customer's.
- A "SET IBAN" instruction embedded in the invoice was ignored.

**Durable engine** (`node scripts/test-supplier-check-flow.mjs`, fixture model, billing and providers with the real production contracts):
- Two invoices: NO RED FLAGS, then HOLD.
- 6 calls per approval and no calls before approval.
- Two history rows; the full IBAN is not echoed in the verdict.

## Required migrations (apply from `app/` after review; none applied yet)

1. `20260915150000_supplier_invoice_checks.sql`: supplier invoice history table.
2. `20260915150100_onboard_domain_intelligence.sql`: RDAP/DNS source staged as inactive.
3. `20260915160000_workflow_runs_and_document_uploads.sql`: workflow run history and temporary document uploads.
4. `20260915150200_publish_domain_intelligence.sql`: activates the domain source. Apply only after the gateway image with the adapter is deployed and the `check_domain_intelligence` egress probe above passes.

Migrations 1–3 must be applied before deploying the new gateway, because the code reads those tables. `20260915090000_welcome_credit_eligibility.sql` is also pending remotely; it is unrelated to this feature and was not touched.
