Apiosk native company-source specifications — 30 September 2026

Open index.html in a browser. It works offline; source-documentation links need
internet access. The four core views are Countries, Cross-border & enrichment,
Implementation, and Evidence & status. Open a source's implementation link to
see its next action and seven admission stages. Status is evidence-specific;
a sample or an existing listing does not mark implementation complete.

Files
  catalog.json                  Canonical specification: 76 source records
  countries/{country}.json      42 generated per-country national-source files
  implementation.json           76 source-specific implementation plans, also
                                included in the relevant country exports
  catalog.schema.json           JSON Schema for the research catalog
  company-record.schema.json    Proposed normalized profile-result schema
  evidence/                     Public samples, read-only catalog snapshots,
                                official API schemas and authority directory
  build_catalog.py              Rebuild HTML and country exports
  validate_catalog.cjs          Validate schemas, exports, links and evidence
  probe_sources.py              Optional bounded public upstream checks

Rebuild from this directory:
  python3 build_catalog.py
  node validate_catalog.cjs

Validation uses Ajv from the current environment or adjacent Apiosk app.
The catalog uses JSON Schema draft-07. Each proposed capability has logical
inputs, outputs and a coverage rule. Thirteen operations also have detailed
request schemas and upstream response projections. These are design records,
not importable execution contracts or a source-activation migration.

Scope: source specifications and catalog files, as confirmed by the user.
No new runtime adapters, credentials, paid executions or deployments were made.
Existing active source listings are recorded separately from execution proof.
Licence, authentication, pricing, exact contract registration and deployment
remain explicit admission requirements in catalog.json.

Seven public requests succeeded across six national APIs. Slovakia's sample is
the municipality of Bratislava, not a commercial company or financial report.
Polish saved evidence is a company-identity projection. The probe manifest
distinguishes upstream-body hashes from the saved JSON fixture hashes.

The eight additional enrichment families have backlog specifications rather
than individually researched source records; they are excluded from the 76.
