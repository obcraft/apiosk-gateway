# Ask intent field contract, schema version 2

The model extraction schema (`ask-intent.schema.json`) is an internal proposal
format. Its snake-case requested field names are not evidence of coverage.
`POST /v2/ask-v2/intent` grounds that proposal before returning schema version 2.
The UI displays `{schema_version, ...intent}`; the API retains its envelope with
`intent`, `model`, `timing`, `attempts` and `usage`.

## Canonical fields

`requests[].required_fields` and `optional_fields` contain only IDs in a closed,
runtime-derived field registry. It comes from admitted, verified execution
contracts and their produced `catalog.fact_types.value_schema` definitions.
The registry is scoped to the exact capability and, when present, explicitly
requested provider names/slugs. Taxonomy descriptions, inferred metadata,
sample-payload keys, local draft definitions and opaque objects do not add fields.

An ID is `<fact-type>#<schema-pointer>`, for example:

```json
{
  "required_fields": ["company.profile#/properties/legal_name"],
  "optional_fields": [],
  "field_resolution": "unresolved",
  "unresolved_fields": {
    "required": [{"field": "registration_details", "reason": "undocumented"}],
    "optional": []
  }
}
```

The pointer is a JSON Pointer into the fact's JSON Schema, not a provider JSON
response path. `company.name#` refers to the whole `company.name` fact;
`company.profile#/properties/activities/items/properties/code` refers to the
documented code inside each activity. Property names use JSON Pointer escaping.
IDs are stable across catalog ordering changes. Schema changes are pinned by
`intent.field_catalog_version`, the existing catalog content hash.

`intent.field_definitions` contains the definitions referenced by the requests:

```json
[
  {
    "id": "company.profile#/properties/legal_name",
    "fact_type": "company.profile",
    "schema_pointer": "/properties/legal_name"
  }
]
```

An exact property name, its deterministic snake-case spelling, or a reviewed
`apiosk_output_aliases_v1` annotation can resolve to one canonical ID. Fact names
and their final component can also identify the whole documented fact. Aliases
use the same annotation validator as existing plan coverage checks. No fuzzy
description matching, synonym invention or broad-record fallback is allowed.
Multiple possible IDs leave the proposed name unresolved with reason `ambiguous`.
An absent field uses reason `undocumented`. Both retain the original requirement.
Unsupported schema constructs are conservatively unresolved.

Required and optional qualifiers survive grounding. If two aliases resolve to
the same ID across both lists, the optional qualifier wins, matching the existing
extraction normalization. IDs are deduplicated and sorted deterministically.

## Consumer rules

- Do not interpret an empty `required_fields` list as complete coverage without
  also checking `unresolved_fields.required`. Unresolved required fields must
  block a claim of complete coverage. Optional gaps remain visible but do not
  become mandatory requirements.
- `field_resolution: resolved` means every requested field was mapped to the
  documented vocabulary. It does not mean an endpoint was selected, inputs are
  available, an individual row contains all fields, or all fields can be supplied
  by a single endpoint. No request for fields means there are no fields to map.
- Source selection must revalidate IDs against its catalog and the actual
  selected endpoint contracts, preserving request/subject bindings and sources.
  The client-provided JSON and catalog hash are not authorization or proof.
- The older source-search `ParsedRequest` format is unchanged. The standalone
  intent page still does not plan, purchase, or execute sources.
- Do not silently treat schema 1 field names as schema 2 IDs. Re-ground them
  explicitly before using them for source-coverage claims.

The grounding pass is deterministic, adds no model call and does not modify
database schemas or purchase data. A catalog read failure fails before model
extraction instead of returning ungrounded fields as a standardized response.
