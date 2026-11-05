# Reuse the Clearname watchlist import

The first implementation stage consumes the existing Clearname `sanctions.json`
snapshot from `obcraft/clearname-watchlist-check`. It does not run a second set of
provider downloads or create additional records for aliases. The importer uses
only Python's standard library and makes no model or network requests.

```sh
python3 scripts/import_clearname_watchlists.py /path/to/sanctions.json --output output/clearname-watchlists
python3 -m unittest discover -s scripts -p 'test_clearname_watchlists.py' -v
```

The snapshot must contain every source in `sources.json`, with a nonempty record
set per source, recognizable entity types and names, and unique source/record-ID
pairs. Its generation timestamp must be within 48 hours, with at most five minutes
of future clock skew. `--max-age-hours` explicitly changes that import-age limit;
it is not evidence that individual upstream lists are current. Clearname currently
allows some provider imports to fail independently, so missing lists are rejected
here instead of silently promoting a partial snapshot.

CSL records retain their underlying list codes. An explicit list allowlist
excludes SDN and accepts the separate Non-SDN lists. A newly introduced CSL list
requires review. Records from different lists remain distinct: cross-list entity
resolution is not inferred from a name or an ID.

Validated artifacts are stored under the input's full SHA-256 directory:

- `index.json`: names, aliases, source code, original record ID, normalized entity
  type, programme, notes and CSL underlying list where applicable.
- `manifest.json`: input and index checksums, snapshot generation time, actual
  record/name counts, per-source categories, CSL breakdown and coverage limits.
- `current.json` in the output directory: an atomically replaced pointer to a
  complete snapshot and its manifest checksum. Failed validation preserves the
  previous pointer. Identical input is idempotent; altered existing artifacts
  cause an error.

No input names are printed by the CLI. `output/` is ignored by Git; downloaded
datasets and generated indexes are not source files.

## Verified initial snapshot

The Clearname snapshot generated at `2026-11-05T13:54Z` contains **59,295 source
records** and **140,233 names/aliases**. These are historical verification counts,
not fixed expectations for future imports or counts of unique people.

Input SHA-256:
`6fb66ee942535b827525b51968e86554b808396c97b6686f7451a9251210d8ab`.

| Source | Records |
| --- | ---: |
| EU | 6,269 |
| OFAC SDN | 19,499 |
| UN | 1,007 |
| UK | 6,420 |
| Canada | 5,708 |
| Australia | 2,273 |
| Switzerland | 9,045 |
| Netherlands | 117 |
| CSL, excluding SDN | 6,699 |
| World Bank debarments | 1,538 |
| European Parliament current MEPs | 720 |

World Bank debarment and the limited MEP PEP list remain separate categories from
sanctions. The snapshot timestamp describes Clearname's generation, not each
list's publication date. Nonempty sources do not prove complete coverage.

## Remaining integration

These artifacts are not yet an Ask source or a screening endpoint. Runtime
loading, refresh scheduling from Clearname, candidate matching and explicit
coverage in results still need to be wired before publishing source-catalog
entries. Name matches must be presented as candidates for review, and a lack of
matches must not be presented as a comprehensive sanctions or PEP clearance.
