# Saved-result paging — 5 October 2026

Starting Gateway commit: `c183e3b`. This is a local implementation and fixture
measurement; production request latency has not been measured for this change.

## Change

Large saved results are returned in pages of 8,000 Unicode scalar values. Before
this change, every page built a `Vec<char>` containing the entire serialized
document, including the first page projected into task status and SSE snapshots.

`engine::results::text_page` now finds UTF-8 byte boundaries with `char_indices`
and borrows the requested slice. It stops at the end of that page. It neither
allocates a full character vector nor scans the remaining document. Later pages
still scan their prefix to resolve the protocol's character offset.

The response still allocates its page string, and the complete public document
is still serialized before slicing. Source projection, the inline-size threshold,
metadata, offset units, signed links, result access checks, caching, pricing,
approval and billing are unchanged. No database migration is needed.

## Local measurement

The opt-in `text_page_local_benchmark` compares the previous vector-and-slice
implementation with the new helper, including copying the resulting page string
in both paths. These are medians of 25 samples in the local Cargo **debug test
profile**, for the first page only:

| Synthetic serialized document | Bytes | Previous median | New median | Previous temporary character buffer | New temporary character buffer |
| --- | ---: | ---: | ---: | ---: | ---: |
| ASCII | 1,048,599 | 10.142 ms | 0.136 ms | 4,194,416 bytes | 0 bytes |
| Mixed Unicode | 1,200,023 | 8.478 ms | 0.193 ms | 2,400,056 bytes | 0 bytes |

The buffer sizes use the old vector's actual capacity times `size_of::<char>()`.
They exclude the original document, serialized JSON, response string and other
allocations. Timings exclude database reads, JSON serialization, source branding,
signing, HTTP and browser rendering. They are not production or end-to-end speedup
claims, and the test imposes no timing threshold.

## Verification

- Boundary tests compare the exact previous paging semantics for empty input,
  ASCII, multibyte characters, emoji, combining characters, page boundaries,
  offsets at the end, out-of-range offsets and `usize::MAX`.
- Multi-page JSON reconstruction preserves every byte, including escaped text,
  leading-zero identifiers, false, zero and null.
- The existing result projection test still checks source data reconstruction,
  exact charges, currency, missing fields and invalid offset rejection.
- The focused checks passed (3 tests); the benchmark passed separately. The
  complete `cargo test --locked --all-targets -- --test-threads=1` run passed
  858 tests across 83 targets, with 0 failures and 91 ignored environment/live
  tests or opt-in measurements. This does not establish live provider behavior.

Run focused checks and the separate benchmark from the Gateway root:

```sh
RUST_MIN_STACK=16777216 cargo test --locked --lib page_metadata_tests -- --test-threads=1
RUST_MIN_STACK=16777216 cargo test --locked --lib text_page_local_benchmark -- --ignored --nocapture --test-threads=1
```

The benchmark performs no database, network, model, provider or payment calls.
