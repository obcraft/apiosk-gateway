# Research report presentation

The research PDF follows the layout reference supplied as `asml-research-report.pdf` on 22 September 2026. It is a reusable projection of saved evidence, not a hardcoded ASML document.

- Opening assessment, original request and evidence coverage.
- Captured building and parcel maps immediately after the overview, with address/date captions below the images.
- Findings with validated source references and readable evidence-field tables.
- Legal entity and location records; eligible financial charts only when the saved evidence supports them.
- Evidence gaps, suggested next steps, and data-quality notes for duplicates and uncited records.
- One provider row in the source register, with brand, saved references, retrieval dates and links.
- Subtle Apiosk wordmark; source badges with horizontal and vertical breathing room; 1.25 line spacing and 2.3 mm table-cell padding; page numbers and a consistent footer.

The uploaded example is a design reference, not a new data source. Its conclusions, confidence labels and figures are not copied. Saved findings and limitations remain unchanged. Missing evidence is never converted into a negative result. Charts require explicit comparable figures, currency and period. A linked parcel is not proof of ownership.

The PDF and evidence ZIP are generated from the same signed snapshot. The ZIP retains complete original responses and attachments, including unused search results. Rendering uses bundled brand assets and the existing Rust PDF library: no extra source purchases, model calls or external rendering fees are introduced, and pricing/margin configuration is unchanged.

To preview an existing snapshot without paid calls:

```sh
cargo run --example render-research-report -- snapshot.json output/pdf/report.pdf
```

Inspect every rendered page with `pdftoppm` before accepting layout changes. Use `cargo test --all-targets` and the isolated `research_report_flow` suite to check evidence validation and the one-approval flow.
