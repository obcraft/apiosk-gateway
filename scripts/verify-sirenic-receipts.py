#!/usr/bin/env python3
"""Read-only: verify each captured provider receipt on Base against USDC logs."""
import json,urllib.request
from decimal import Decimal
from pathlib import Path
root=Path(__file__).resolve().parents[1]/'output/sirenic-20260912'
files=sorted(root.glob('canonical-paid-*.json'))
assert len(files)==6,'Six successful captured calls required'
payer='1f2ddf2baf0a56988507e08a4622920a89751665';payee='76a672eee56d29d475b0715cc03b8c99d70ec8a2'
evidence=[]
for p in files:
 d=json.loads(p.read_text());execution=d['body']['execution'];assert execution['paid'] is True
 tx=execution['tx_hash'];assert isinstance(tx,str) and len(tx)==66
 def fetch(suffix):
  request=urllib.request.Request('https://base.blockscout.com/api/v2/transactions/'+tx+suffix,headers={'User-Agent':'apiosk-integration-check/1.0'})
  return json.load(urllib.request.urlopen(request,timeout=20))
 receipt=fetch('');transfers=fetch('/token-transfers')
 assert receipt['hash']==tx and receipt['status']=='ok' and receipt['confirmations']>0,receipt
 assert transfers['next_page_params'] is None,'Incomplete transfer response'
 matches=[t for t in transfers['items'] if t['transaction_hash']==tx and t['token']['address_hash'].lower()=='0x833589fcd6edb6e08f4c7c32d4f71b54bda02913' and t['from']['hash'].lower()=='0x'+payer and t['to']['hash'].lower()=='0x'+payee and t['total']['decimals']=='6']
 amount=sum(int(t['total']['value']) for t in matches);assert amount==d['provider_cap'],(d['path'],amount,d['provider_cap'])
 (root/(p.stem+'-chain-receipt.txt')).write_text(json.dumps({'transaction':receipt,'transfers':transfers},indent=2)+'\n')
 reported=int(Decimal(execution['charged_usdc'])*1000000)
 pricing_verified=reported==d['buyer_cap']
 if not pricing_verified:
  assert execution['request_id']=='8b64f08d-fcd6-445a-a818-99e517d144c5' and reported==2645,'Unexpected pricing discrepancy'
 evidence.append({'buyer_pricing_verified':pricing_verified,'reported_buyer_total_micro_usd':reported,'path':d['path'],'transaction':tx,'block':receipt['block_number'],'provider_paid_micro_usd':amount,'buyer_total_micro_usd':d['buyer_cap']})
assert len({e['transaction'] for e in evidence})==6
assert sum(e['provider_paid_micro_usd'] for e in evidence)==180000
(root/'verified-receipts.json').write_text(json.dumps({'verification_source':'https://base.blockscout.com/api/v2/transactions/','network':'eip155:8453','asset':'USDC','treasury':'0x'+payer,'payee':'0x'+payee,'calls':evidence,'provider_total_micro_usd':180000,'customer_balance_debited':False,'pricing_note':'First search capture preceded the receipt header fix and reported 2645 instead of 2300 micro-USDC. The treasury transferred only 2000; no customer balance was debited. Remaining five calls verify corrected pricing.'},indent=2)+'\n')
print('Verified six successful Base receipts: USDC transfers from treasury to Sirenic total $0.18')
