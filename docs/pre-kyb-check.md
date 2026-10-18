# Pre-KYB check: the owned wedge

Decision 2026-10-18: Apiosk owns one small problem and solves it best:
**"Who is this company, really?"** One input (company name plus country, or a
registration number), one output: a verified identity card with a preliminary
Pass / Review / Reject verdict, every line naming the register it came from.
Pitch: "Screen first. KYB second." One API or MCP call. Cents per company.

This document is the contract between the gateway, the app, the MCP server and
API callers. Change it here first.

## Principles

1. **Code decides, never the model.** The verdict is computed by
   `pre_kyb::evaluate` from the saved source documents, like the supplier
   check. A model may summarise afterwards; it never changes `decision`.
2. **Every line carries its register.** Identity lines and checks carry
   `source`, `result_ref` and evidence pointers into the saved documents.
3. **Say what was not checked.** `coverage_gaps` lists what the sources cannot
   prove (account holder, UBO, directors screening, credit), so Pass never
   overclaims.
4. **Same money path.** Quote, approval, worker, ledger and refunds are the
   ordinary gateway path. No special pricing.

## Inputs

The existing `pre-kyb-screening` recipe fields (see `workflow_catalog.json`):

| key | required | notes |
|---|---|---|
| `name` | yes | Company name as the caller knows it |
| `country` | yes | `NL`, `GB`, `FR` |
| `screening_level` | yes | `Standard` or `Enhanced` |
| `registration` | no | KVK number, Companies House number or SIREN. Avoids a paid name search |
| `domain` | no | Website domain |
| `vat_country`, `vat_number` | no | Both or neither |
| `iban` | no | Validated for structure and bank only |

## Output: the `pre_kyb_check` analysis

Saved as `task.analysis`, shown in `context_view.analysis` and in the public
API `answer`. Shape:

```json
{
  "status": "completed | partial",
  "kind": "pre_kyb_check",
  "verdict": {
    "decision": "pass | review | reject",
    "label": "PASS | REVIEW | REJECT",
    "headline": "✅ PASS: no red flags in the completed checks",
    "language": "en",
    "reasons": ["..."],
    "next_action": "Proceed to full KYB. This pre-screen complements it and does not replace it.",
    "subject": {
      "name": "Example B.V.", "country": "NL", "screening_level": "Standard",
      "registration": "12345678", "vat_number": null, "domain": "example.nl", "iban_masked": null
    },
    "identity": [
      {
        "id": "legal_name", "label": "Legal name", "value": "Example B.V.",
        "source": "KVK", "result_ref": "00000000-0000-0000-0000-000000000000", "retrieved_at": "2026-10-18T10:00:00Z",
        "evidence": [{"result_ref": "00000000-0000-0000-0000-000000000000", "pointer": "/data/statutaireNaam", "value": "Example B.V."}]
      }
    ],
    "checks": [
      {
        "id": "identity_resolved", "label": "Registered identity resolved",
        "status": "pass | warn | fail | unknown | not_selected",
        "detail": "Registered with KVK under 12345678.",
        "source": "KVK",
        "evidence": [{"result_ref": "00000000-0000-0000-0000-000000000000", "pointer": "/data/kvkNummer", "value": "12345678"}]
      }
    ],
    "coverage_gaps": ["Directors and owners were not individually screened.", "..."]
  },
  "observations": [{"text": "✅ PASS: ...", "evidence": []}],
  "limitations": ["..."]
}
```

### Identity lines (`identity[].id`)

In this order, omitted when no source supplied a value:
`legal_name`, `trade_names`, `registration_number`, `rsin` (NL only),
`legal_form`, `status`, `incorporated`, `registered_address`, `activities`,
`vat`, `lei`.

`source` is a short register name: `KVK`, `Companies House`, `Sirenic`,
`VIES`, `OpenSanctions`, `GLEIF`, `RDAP & DNS`, `IBAN check`.

### Checks (`checks[].id`) and statuses

| id | when | pass | warn | fail | unknown |
|---|---|---|---|---|---|
| `identity_resolved` | always | a registry profile document exists for the subject | name search returned several candidates and no profile | search returned no company, or the register returned not found | register unavailable |
| `name_match` | profile present | supplied name matches a registered or trade name | differs from every registered name | never | no names returned |
| `company_status` | profile present | active (KVK no `datumEinde`; CH `active`; Sirenic active flag) | status other than active or dissolved (e.g. liquidation, dormant) | dissolved or registration ended | status field missing |
| `sanctions` | always | screen returned no candidates | any candidate or PEP flag whose identity is not confirmed | a candidate whose registration number or LEI equals the registry record | screen unavailable |
| `vat` | vat supplied | VIES valid and name or RSIN consistent, or valid with no name available | valid but name differs | invalid | VIES unavailable |
| `domain` | domain supplied | registered 180 days or more ago | younger than 180 days, or no registration date published | never | RDAP unavailable |
| `mail_dns` | domain and Enhanced | MX and SPF present | MX or SPF missing | never | DNS unavailable |
| `iban` | iban supplied | structurally valid | never | invalid | checker unavailable |
| `officers` | GB Enhanced | officers listed (count in detail; not screened) | no active officers | never | unavailable |
| `controllers` | GB Enhanced | controllers listed | none listed | never | unavailable |
| `insolvency` | GB Enhanced | no records | never | records present | unavailable |
| `filings` | NL or FR Enhanced | accounts or figures available | none available | never | unavailable |
| `alerts` | FR Enhanced | no alerts | alerts present | never | unavailable |

A check whose precondition was not met is omitted, not emitted as
`not_selected`. `not_selected` is reserved for Enhanced-only checks when the
caller chose Standard, so the card can show what Enhanced would add.

### Decision rules

- `reject` only when `company_status` is `fail` (confirmed dissolved) or
  `sanctions` is `fail` (confirmed identity by identifier). Never on name
  similarity, PEP flag, missing data or domain age alone.
- `pass` only when `identity_resolved` is `pass`, `name_match` is `pass` or
  omitted because the caller supplied the registration number and the
  register returned a name, and every emitted check is `pass` or
  `not_selected`.
- otherwise `review`. `reasons` lists fail and warn details first, then
  unknown ones, at most six.
- `status` is `partial` when any emitted check is `unknown`; otherwise
  `completed`.

### Coverage gaps

Always included, in this order, dropping any that an emitted check covers:

1. "Directors and owners were not individually screened for sanctions, PEP
   status or disqualification."
2. "Ultimate beneficial ownership was not established."
3. "The bank account holder was not verified; IBAN checks cover structure and
   bank only." (only when an IBAN was supplied)
4. "No credit score or payment behaviour data was used."
5. "LEI and corporate parents were not retrieved." (dropped when an `lei`
   identity line exists)

## Surfaces

- **App**: `/check` page. One input box for the name, country and level,
  optional details collapsed. Starting opens the quote and approval on the same
  page and then shows the identity card with a JSON receipt download.
- **Public API**: `POST /v2/checks/pre-kyb` with the input fields plus
  `max_spend` (required for auto run) and optional `wait_seconds`. Without
  `max_spend` it returns the priced plan and the ask id; run it with
  `POST /v2/asks/{id}/run`. `GET /v2/checks/pre-kyb/{id}` reads it back. The
  response is the ordinary ask body with `verdict` copied to the top level.
- **MCP**: the existing discover `workflow` input with slug `pre-kyb-screening`;
  the result card renders the identity card and checks.

## Receipt (v1)

A JSON file assembled by the client from the saved task: `verdict`, `identity`,
`checks`, `coverage_gaps`, the `result_refs` with their source endpoints,
billing totals and `generated_at`. A signed PDF follows later.

## Enablement

The named Workflows feature stays paused except for this slug. The gateway
answers `workflows::enabled(slug)`; the app mirrors it with
`isWorkflowEnabled(slug)`.
