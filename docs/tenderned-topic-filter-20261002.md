# Literal topics in public tender search

The published question `Find public tenders for IT services in the Netherlands.`
was parsed without either supplied input. Planning chose the optional-filter
TenderNed feed with no search input and then rejected invented required fields.

The repair recognizes the complete plain tender/topic/country question form,
copies its topic and country, and requests the publication record already
declared by the catalog. Explicit additional dates, sources, formats and output
requirements continue through the normal parser. It does not invent CPV codes.
Only an unchanged Dutch request can use the already-discovered TenderNed
national-publications contract for model-free planning. Ordinary contract,
binding, resolver, coverage, quote and approval checks still apply.

## Public source checks

Anonymous reads on 2026-10-02 of the official
`https://www.tenderned.nl/papi/tenderned-rs-tns/v2/publicaties` endpoint gave:

| Filter | Reported total | Meaning |
| --- | ---: | --- |
| No search | 146,213 | Mixed national publication feed |
| `search=IT services` | 96,673 | Broad token matching; unrelated construction-waste notice appeared first |
| `search="IT services"` | 8,004 | Phrase matching; first records concern identity/access management, a software broker, and ICT services |
| Nonsense literal control | 0 | Confirms the search parameter is applied |
| `cpvCodes=72000000` | HTTP 400 | An eight-digit CPV alone is rejected |
| `cpvCodes=72000000-5` | 12,650 | Classification query; not used by this repair |

The initial broad query is **not an ignored parameter**. Control queries show
that unquoted terms search more broadly than the literal phrase. The official
TenderNed search frontend sends `search` and `cpvCodes`; its source was inspected
from the scripts linked by the public announcement page. The public Swagger
document describes the separate authenticated XML interface, not this search
endpoint.

`tests/fixtures/tenderned-it-public-page.json` records the exact phrase URL and
the first 20 returned records. Totals are a snapshot, not a permanent assertion.
The page includes AAO, AGO, MAC, REC and VAK publication types. These are not all
open tenders. TenderNed explains its classification hierarchy at
<https://www.tenderned.nl/cms/nl/vraag/zoek-op-omschrijving-cpv-code>.

## Binding and presentation

On the reviewed TenderNed search endpoint only, plain multiword topics are
serialized as quoted phrases. Existing quotes, Boolean syntax, single words,
date filters and CPV filters are preserved. The quote and execution share the
same `Service::inputs` function.

The result document saves the actual bound request parameters. The direct
answer accepts only the plain question and matching saved request, endpoint,
subject and response metadata. Old documents without query provenance are not
relabeled as filtered results. The table retains every saved row with title,
buyer, publication type, publication date, optional closing date and verified
original TenderNed link. It states that this is one page, may mix announcements
and awards, and does not prove eligibility or current availability.

All public probes were anonymous direct source reads. They did not execute,
approve or settle an Apiosk purchase. Local fixtures and planning tests alone
are not paid end-to-end evidence.
