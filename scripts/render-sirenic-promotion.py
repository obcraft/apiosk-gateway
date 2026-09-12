#!/usr/bin/env python3
"""Enable only completed paid checks with valid extraction and chain receipts."""
import json,hashlib
from pathlib import Path
R=Path(__file__).resolve().parents[1];out=R/'output/sirenic-20260913'
f=json.loads((R/'tests/fixtures/sirenic/expanded-catalog.json').read_text())
reports=json.loads((out/'paid-contract-evidence.json').read_text())
by_id={r['service_id']:r for r in reports}
chain=json.loads((out/'payments/verified-receipts.json').read_text())
txs={r['transaction'] for r in chain['calls']}
txs.update(r['transaction'] for r in json.loads((R/'output/sirenic-20260912/verified-receipts.json').read_text())['calls'])
adjustments=json.loads((R/'tests/fixtures/sirenic/verified-contract-adjustments.json').read_text())['services']
q=lambda s:"'"+str(s).replace("'","''")+"'"
api='804c77ce-bac1-4880-9e83-04f8b701b839'
approved=[];failed=[];sql=['-- Enable only routes with reviewed paid response and confirmed treasury receipt.','BEGIN;']
for adjustment in adjustments:
 c=adjustment['contract'];produces=[dict(fact_type=k,subject_role='subject',cardinality=c['cardinality']) for k in c['output_map']['produces']]
 sql.append(f"UPDATE catalog.capability_execution_contracts SET output_map={q(json.dumps(c['output_map']))}::jsonb,produces={q(json.dumps(produces))}::jsonb,identifies=NULL WHERE id={q(adjustment['id'])};")

for s in f['services']:
 if s.get('check_inputs') is not None:
  attempt=json.loads((out/'payments'/(s['id']+'.json')).read_text())
  assert attempt['status']=='response'
  if attempt.get('http_status')==504:
   request=attempt['body']['execution']['request_id'];proof=json.loads((out/'reconciliation'/(request+'.json')).read_text())
   assert proof['database_status']=='failed' and not proof['authorization_used'] and proof['block_timestamp']>proof['expiry_upper_bound']
 report=by_id.get(s['id'])
 if report and report['valid']:
  assert report['tx_hash'] in txs, 'Missing independent chain receipt'
  approved.append(s)
  note='Paid production result validated against the extraction contract; Base USDC treasury receipt '+report['tx_hash']+'. Evidence: output/sirenic-20260913/paid-contract-evidence.json.'
  sql.append(f"UPDATE catalog.capability_execution_contracts SET status='active',validated_at=now(),validated_note={q(note)} WHERE id={q(s['id'])} AND endpoint_id={q(s['endpoint_id'])};")
 else:
  failed.append(s)
  sql.append(f"UPDATE catalog.capability_execution_contracts SET status='draft',validated_at=NULL,validated_note='Production verification did not return a usable paid result; excluded from planner.' WHERE id={q(s['id'])};")
assert len(approved)>=6
# Only replace original EU contracts after their v2 extraction has passed real captures.
ids=','.join(q(s['id']) for s in approved)
assert all(any(s['path']==path for s in approved) for path in ['/v1/eu/recherche','/v1/eu/entreprise/{pays}/{id}'])
sql.insert(2,f"UPDATE catalog.capability_execution_contracts c SET status='retired' FROM providers.api_endpoints e WHERE e.id=c.endpoint_id AND e.api_id={q(api)} AND e.path IN ('/v1/eu/recherche','/v1/eu/entreprise/{{pays}}/{{id}}') AND c.version=1 AND c.id NOT IN ({ids});")
approved_endpoints={s['endpoint_id'] for s in approved}
for s in failed:
 if s['endpoint_id'] not in approved_endpoints:
  sql.append(f"UPDATE providers.api_endpoints SET active=false WHERE id={q(s['endpoint_id'])} AND api_id={q(api)};")
notice=f"{len(approved_endpoints)} fixed-price JSON routes ({len(approved)} planner operations) verified with paid results and treasury receipts. Availability varies by register, company and filing. Variable-price batches, binary documents, subscriptions and unverified operations are excluded. Source coverage notes and missing values are preserved."
metadata=json.dumps({'coverage_notice':notice,'planner_block_reason':None},ensure_ascii=False)
sql.append(f"UPDATE providers.apis SET description='French and European company data from official registers: company profiles, annual accounts, directors and ownership, legal alerts, procurement, establishments and business checks.',listing_metadata=listing_metadata || {q(metadata)}::jsonb WHERE id={q(api)};")
sql.append(f"DO $check$ BEGIN IF (SELECT count(*) FROM catalog.capability_execution_contracts c JOIN providers.api_endpoints e ON e.id=c.endpoint_id WHERE e.api_id={q(api)} AND c.status='active' AND e.active AND c.validated_at IS NOT NULL) <> {len(approved)} THEN RAISE EXCEPTION 'Unexpected Sirenic active contract count'; END IF; END $check$;")
sql+=['COMMIT;','']
p=next((R/'supabase/migrations').glob('*_sirenic_verified_company_routes.sql'));p.write_text('\n\n'.join(sql))
summary={'active_routes':len(approved_endpoints),'active_contracts':len(approved),'excluded':[{'path':s['path'],'contract':s['id'],'inputs':s.get('check_inputs')} for s in failed],'provider_verification_cost_micro_usd':chain['provider_total_micro_usd'],'customer_balance_debited':False}
(out/'promotion-summary.json').write_text(json.dumps(summary,indent=2)+'\n')
print(json.dumps(summary,indent=2))
