#!/usr/bin/env python3
"""Build the offline country catalog and deterministic country JSON exports."""
import html
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent
CAT = json.loads((ROOT / 'catalog.json').read_text())
SOURCES = {s['source_id']: s for s in CAT['sources']}
NATIVE_IDS = {sid for country in CAT['countries'] for sid in country['native_source_ids']}


def implementation_plan(s):
    existing = bool(s['current_apiosk'])
    sampled = 'live_upstream' in s['readiness']['evidence']
    reference = s['source_class'] == 'registry_directory'
    operations = [op for op in CAT['capability_contracts'] if op['source_id'] == s['source_id']]
    gates = [
        {'stage': 'Access', 'status': 'Sample verified' if sampled else 'Needs validation',
         'required': 'Confirm the selected product, authentication, quotas and error responses. One sample does not prove complete coverage.'},
        {'stage': 'Reuse rights', 'status': 'Open licence documented' if s['rights']['licence'] else 'Terms to confirm',
         'required': s['rights']['required_action']},
        {'stage': 'Inputs & outputs', 'status': 'Detailed draft available' if operations else 'Logical specification only',
         'required': 'Validate exact request/response schemas and keep country-qualified identifiers, dates and missing-field reasons.'},
        {'stage': 'Adapter', 'status': 'Reference ingestion required' if reference else 'Existing route to revalidate' if existing else 'Implementation required',
         'required': 'Ingest versioned authority metadata for discovery.' if reference else 'Implement or revalidate the declared route, normalization and identity checks before returning an Apiosk result.'},
        {'stage': 'Execution contract', 'status': 'Not applicable to company execution' if reference else 'Existing listing observed' if existing else 'Registration required',
         'required': 'Do not advertise a registry directory as a company-data operation.' if reference else 'Reuse existing accepted contracts where applicable; otherwise register exact schemas, facts and adapter mapping.'},
        {'stage': 'Pricing', 'status': 'Not applicable to company execution' if reference else 'Existing prices to revalidate' if existing else 'Not set',
         'required': 'No company-record purchase is inferred from this directory.' if reference else 'Confirm the product tariff and bounded operation cost; retain one total price and one approval.'},
        {'stage': 'Live execution', 'status': 'Not applicable to company execution' if reference else 'Not tested in this review',
         'required': 'Verify version, authority codes and links after ingestion.' if reference else 'Verify approved execution, source identity, actual charge and saved-result recovery before advertising support.'},
    ]
    return {'source_id': s['source_id'], 'name': s['name'], 'country_codes': s['country_codes'],
            'catalog_section': 'countries' if s['source_id'] in NATIVE_IDS else 'other', 'priority': s['priority'],
            'evidence_group': 'sample' if sampled else 'existing' if existing else 'pending',
            'reference_only': reference, 'next_action': s['required_work'][0], 'required_work': s['required_work'],
            'operation_spec_ids': [op['spec_id'] for op in operations], 'stages': gates,
            'evidence': {'assessment': s['readiness']['evidence'], 'references': s['references'],
                         'fixtures': sorted({op['verification']['fixture'] for op in operations if op['verification']['fixture']})},
            'new_activation_proven': False}


PLANS = {s['source_id']: implementation_plan(s) for s in CAT['sources']}
(ROOT / 'implementation.json').write_text(json.dumps({
    'schema_version': CAT['schema_version'], 'as_of': CAT['as_of'],
    'scope': 'Implementation specifications, not completed adapters or executable admission records.',
    'source_plans': list(PLANS.values())}, ensure_ascii=False, indent=2) + '\n')


def e(value):
    return html.escape(str(value), quote=True)


def human(value):
    return str(value).replace('_', ' ')


def bullets(values):
    return '<ul>' + ''.join('<li>' + e(v) + '</li>' for v in values) + '</ul>'


def data_table(values):
    return '<dl>' + ''.join('<dt>' + e(k) + '</dt><dd>' + e(v) + '</dd>' for k, v in values) + '</dl>'


def code(value):
    return '<pre>' + e(json.dumps(value, ensure_ascii=False, indent=2)) + '</pre>'


def operation(op):
    fixture = op['verification'].get('fixture')
    link = (' · <a href="' + e(fixture) + '">Sample response</a>') if fixture else ''
    return ('<details class="operation"><summary><span class="method">' + e(op['method']) + '</span> '
            + e(op['public_capability']) + '</summary><div class="inside"><p class="mono">'
            + e(op['origin_url'] + op['path']) + '</p><p>' + e(human(op['verification']['level'])) + link + '</p>'
            + '<h5>Inputs and upstream bindings</h5>' + code({'schema': op['input_schema'], 'bindings': op['input_map'],
                'fixed': op['fixed_params'], 'defaults': op.get('request_defaults', {})})
            + '<h5>Output mapping</h5><p>' + e(op['field_projection_scope']) + '</p>' + code(op['upstream_output_map'])
            + '<h5>Validation and coverage</h5>' + bullets(op['normalization_requirements'])
            + '<p class="muted">Draft contract. Maximum fan-out: ' + e(op['max_fan_out']) + '. Apiosk price unset.</p></div></details>')


def source(s):
    live = 'live_upstream' in s['readiness']['evidence']
    badge = 'Public sample verified' if live else human(s['readiness']['evidence'])
    current = s['current_apiosk']
    state = ('Existing Apiosk listing: ' + current['slug'] + ' · active endpoints: ' + str(current['active_endpoint_count'])
             + '. Execution not retested.') if current else 'Source specification only. No new activation.'
    ops = [op for op in CAT['capability_contracts'] if op['source_id'] == s['source_id']]
    refs = ''.join('<li><a href="' + e(ref['url']) + '" target="_blank" rel="noreferrer">'
                   + e(ref['url'].split('/')[2]) + ' — ' + e(human(ref['evidence'])) + '</a></li>' for ref in s['references'])
    io_rows = ''.join('<tr><td><b>' + e(p['public_name']) + '</b><p class="muted">' + e(p['coverage_rule'] or '')
                     + '</p></td><td>' + bullets(p['inputs']) + '</td><td>' + bullets(p['outputs']) + '</td></tr>'
                     for p in s['proposed_capabilities'])
    return ('<details class="source" id="' + e(s['source_id']) + '"><summary><span><b>' + e(s['name'])
        + '</b><small>' + e(s['publisher']) + '</small></span><span class="badge ' + ('verified' if live else '') + '">'
        + e(badge) + '</span></summary><div class="inside"><p>' + e(s['coverage']['description']) + '</p>'
        + '<p class="note">' + e(s['coverage']['limitations']) + '</p>'
        + data_table([('Source ID', s['source_id']), ('Source type', human(s['source_class'])),
            ('Integration route', human(s['access']['mode'])), ('Access', s['access']['authentication']),
            ('Upstream costs', s['access']['upstream_cost']), ('Reuse terms', s['rights']['licence'] or 'Product terms still to confirm'),
            ('Freshness', s['coverage']['freshness']), ('Apiosk status', state),
            ('Authority codes', ', '.join(s['registration_authority_codes']) or 'No source-level mapping asserted')])
        + '<h4>Identifiers</h4>' + bullets([i['scheme'] + ' — ' + i['description'] for i in s['identifiers']])
        + '<h4>Proposed capabilities</h4><div class="chips">' + ''.join('<code>' + e(p['public_name']) + '</code>' for p in s['proposed_capabilities'])
        + '</div><p class="muted">Candidate capabilities require accepted execution contracts.</p>'
        + '<details class="operation"><summary>Capability inputs and outputs</summary><div class="inside scroll">'
        + '<p class="muted">Logical source specifications. Exact wire schemas remain pending unless a detailed operation is supplied below.</p>'
        + '<table><thead><tr><th>Capability and limits</th><th>Inputs</th><th>Outputs</th></tr></thead><tbody>' + io_rows + '</tbody></table></div></details>'
        + '<h4>Work required to integrate</h4>' + bullets(s['required_work'])
        + '<button type="button" class="text-link" data-implementation-source="' + e(s['source_id']) + '">Open implementation plan ↗</button>'
        + ('<h4>Detailed operation specifications</h4>' + ''.join(operation(op) for op in ops) if ops else '')
        + '<h4>Official documentation and evidence</h4><ul class="refs">' + refs + '</ul></div></details>')


(ROOT / 'countries').mkdir(exist_ok=True)
country_cards = []
for country in sorted(CAT['countries'], key=lambda c: c['name']):
    selected = [SOURCES[sid] for sid in country['native_source_ids']]
    export = {'schema_version': CAT['schema_version'], 'as_of': CAT['as_of'], 'scope': CAT['scope']['deliverable'],
              'country': country, 'sources': selected,
              'implementation_plans': [PLANS[s['source_id']] for s in selected],
              'capability_contracts': [op for op in CAT['capability_contracts'] if op['country_code'] == country['country_code']],
              'shared_contracts_and_integration': '../catalog.json'}
    filename = country['country_code'].lower() + '.json'
    (ROOT / 'countries' / filename).write_text(json.dumps(export, ensure_ascii=False, indent=2) + '\n')
    haystack = country['name'] + ' ' + country['country_code'] + ' ' + json.dumps(selected, ensure_ascii=False)
    country_cards.append('<article class="country" data-region="' + e(country['region']) + '" data-search="' + e(haystack.casefold()) + '">'
        + '<div class="country-heading"><div><span class="country-code">' + e(country['country_code']) + '</span><h3>' + e(country['name'])
        + '</h3><span class="region">' + e({'EU27': 'EU', 'EEA_non_EU': 'EEA', 'European_neighbour_or_UK': 'Europe & neighbours'}[country['region']])
        + '</span></div><a href="countries/' + filename + '" download>Country JSON ↗</a></div>'
        + ''.join(source(s) for s in selected) + '</article>')

other = ''.join(source(s) for s in CAT['sources'] if s['source_id'] not in NATIVE_IDS)
implementation_cards = []
country_names = {country['country_code']: country['name'] for country in CAT['countries']}
for plan in sorted(PLANS.values(), key=lambda p: (p['priority'], p['name'])):
    s = SOURCES[plan['source_id']]
    places = ', '.join(country_names.get(cc, cc) for cc in s['country_codes']) or 'Cross-border'
    searchable = ' '.join([plan['source_id'], s['name'], places, s['publisher'], ' '.join(s['country_codes'])]).casefold()
    stages = ''.join('<tr><td><b>' + e(g['stage']) + '</b></td><td>' + e(g['status'])
                     + '</td><td>' + e(g['required']) + '</td></tr>' for g in plan['stages'])
    evidence_links = ''.join('<li><a href="' + e(f) + '">Saved upstream sample ↗</a></li>' for f in plan['evidence']['fixtures'])
    badge = {'sample': 'Upstream sample verified', 'existing': 'Existing catalog listing', 'pending': 'Access validation pending'}[plan['evidence_group']]
    implementation_cards.append('<details class="source implementation-source" id="implementation-' + e(s['source_id'])
        + '" data-search="' + e(searchable) + '" data-evidence="' + e(plan['evidence_group']) + '"><summary><span><b>'
        + e(s['name']) + '</b><small>' + e(places) + ' · Priority ' + str(plan['priority']) + '</small></span><span class="badge">'
        + e(badge) + '</span></summary><div class="inside"><h4>Next integration step</h4><p>' + e(plan['next_action'])
        + '</p>' + ('<p class="note">Reference directory: this supports source discovery, not company-record execution.</p>' if plan['reference_only'] else '')
        + '<div class="scroll"><table><thead><tr><th>Stage</th><th>Current evidence</th><th>Required work</th></tr></thead><tbody>'
        + stages + '</tbody></table></div><h4>Source-specific work</h4>' + bullets(plan['required_work'])
        + ('<h4>Saved evidence</h4><ul>' + evidence_links + '</ul>' if evidence_links else '')
        + '<p class="muted">These stages describe remaining implementation work. No new source activation is claimed.</p></div></details>')
integration = ''.join('<article class="work-item"><span>' + str(i + 1).zfill(2) + '</span><div><h3>' + e(human(fix['id']).replace('-', ' '))
                      + '</h3><p>' + e(fix['change']) + '</p><p class="muted">' + e(fix['reason']) + '</p></div></article>'
                      for i, fix in enumerate(CAT['apiosk_integration']['required_fixes']))
waves = ''.join('<article class="wave"><span>Wave ' + str(w['wave']) + '</span><h3>' + e(w['purpose']) + '</h3><p>' + e(w['acceptance'])
                + '</p><details><summary>' + str(len(w['sources'])) + ' priority sources</summary>' + bullets([SOURCES[sid]['name'] for sid in w['sources']])
                + '</details></article>' for w in CAT['priorities'])
manifest = json.loads((ROOT / 'evidence/probe-manifest.json').read_text())
probes = ''.join('<tr><td>' + e(p['id']) + '</td><td><span class="badge verified">HTTP ' + str(p['status']) + '</span></td><td>'
                 + e(p['checked_at']) + '</td><td><a href="evidence/' + e(p['body_file']) + '">View sample ↗</a></td></tr>' for p in manifest)
backlog = ''.join('<article class="wave"><h3>' + e(b['family']) + '</h3><p>' + e(', '.join(b['publishers'])) + '</p><p>'
                 + e(b['role']) + '</p><p class="muted">' + e(b['required']) + '</p></article>' for b in CAT['additional_enrichment_backlog'])
template = (ROOT / 'catalog.template.html').read_text()
for key, value in {'COUNTRIES': ''.join(country_cards), 'OTHER': other, 'INTEGRATION': integration, 'WAVES': waves,
                   'IMPLEMENTATION_PLANS': ''.join(implementation_cards),
                   'PROBES': probes, 'CORRECTIONS': bullets(CAT['corrections_to_pasted_list']), 'BACKLOG': backlog,
                   'IDENTITY': data_table([(human(k), ', '.join(v) if isinstance(v, list) else v) for k, v in CAT['identity_design'].items()]),
                   'DATABASE': data_table(CAT['apiosk_integration']['source_spec_to_database'].items()),
                   'PRICE_POLICY': e(CAT['apiosk_integration']['price_policy'])}.items():
    template = template.replace('@@' + key + '@@', value)
(ROOT / 'index.html').write_text(template)
print('Built index.html and 42 country exports from catalog.json')
