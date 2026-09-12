#!/usr/bin/env python3
"""Read-only proof of expired, unused x402 authorization. Never signs or retries.
Sirenic's reviewed adapter accepts timeout <=200 seconds. The captured response
finishes after signing, so finish+205 is a conservative expiry upper bound.
Database status must still be reconciled separately with a compare-and-set.
"""
import json,subprocess,sys,uuid
from datetime import datetime
from pathlib import Path
root=Path(__file__).resolve().parents[1]
rpc='https://base-rpc.publicnode.com'
for file in (root/'output/sirenic-20260913/payments').glob('*.json'):
 d=json.loads(file.read_text())
 if d.get('http_status')!=504:continue
 request=d['body']['execution']['request_id']
 prior=root/'output/sirenic-20260913/reconciliation'/(request+'.json')
 if prior.exists() and json.loads(prior.read_text()).get('database_status')=='failed':continue
 nonce=subprocess.check_output(['cast','keccak','0x'+uuid.UUID(request).bytes.hex()],text=True).strip()
 block=json.loads(subprocess.check_output(['cast','block','latest','--json','--rpc-url',rpc],text=True))
 timestamp=int(block['timestamp'],16)
 maximum=d.get('reviewed_max_timeout_seconds',200);assert maximum in [120,200]
 expiry=int(datetime.fromisoformat(d['finished_at'].replace('Z','+00:00')).timestamp())+maximum+5
 if timestamp<=expiry: print(f'{request}: authorization may still be valid');continue
 state=subprocess.check_output(['cast','call','0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913','authorizationState(address,bytes32)(bool)','0x1f2ddf2baf0a56988507e08a4622920a89751665',nonce,'--rpc-url',rpc],text=True).strip()
 evidence={'request_id':request,'service_id':d['service_id'],'path':d['path'],'provider_cap':d['provider_cap'],'nonce':nonce,'rpc':rpc,'chain':'eip155:8453','block_number':int(block['number'],16),'block_timestamp':timestamp,'expiry_upper_bound':expiry,'reviewed_max_timeout_seconds':maximum,'authorization_used':state=='true','eligible_for_failed_reconciliation':state=='false' and timestamp>expiry}
 out=root/'output/sirenic-20260913/reconciliation';out.mkdir(exist_ok=True)
 (out/(request+'.json')).write_text(json.dumps(evidence,indent=2)+'\n')
 print(json.dumps(evidence))
