// Paid mode runs six reviewed calls through the same /v1/do treasury path used
// by use-selection. Maximum provider spend $0.18; buyer-equivalent total $0.207.
// This is an operator procurement test, not a debit of a customer's balance.
// Default is dry-run. A persisted attempt is never sent again by this script.
import fs from 'node:fs/promises';
import {execFileSync} from 'node:child_process';
import {randomUUID} from 'node:crypto';
import assert from 'node:assert/strict';
const root=new URL('../',import.meta.url), out=new URL('output/sirenic-20260912/',root);
const f=JSON.parse(await fs.readFile(new URL('tests/fixtures/sirenic/catalog.json',root),'utf8'));
const paid=process.argv.includes('--pay');
const run=process.env.SIRENIC_CHECK_RUN || 'initial';
assert.match(run,/^[a-z0-9-]{1,40}$/);
let key=process.env.APIOSK_APP_SERVICE_KEY;
try { key ||= execFileSync('fly',['ssh','console','-a','apiosk-gateway-v2','-q','-C',`sh -c 'printf "%s" "$APIOSK_APP_SERVICE_KEY"'`],{encoding:'utf8',stdio:['ignore','pipe','pipe']}).trim(); } catch { throw new Error('Treasury service credential unavailable'); }
assert.ok(key.length>20 && !key.includes('\n'),'service credential unavailable');
const start=Number(process.env.SIRENIC_CHECK_START || '1');
assert.ok(Number.isInteger(start) && start>=1 && start<=6);
let spent=0;
// Resume only after checking every previous capture. Already-paid calls are
// never sent again, even when their old wrapper reported incorrect pricing.
for (let j=0;j<start-1;j++) {
 const prior=JSON.parse(await fs.readFile(new URL(`${run}-paid-${String(j+1).padStart(2,'0')}.json`,out),'utf8'));
 assert.equal(prior.path,f.services[j].path);
 assert.equal(prior.body.did,true);
 assert.equal(prior.body.execution.paid,true);
 assert.match(prior.body.execution.tx_hash,/^0x[0-9a-f]{64}$/i);
 spent+=prior.provider_cap;
}
for(const [i,s] of f.services.entries()) {
 if(i+1<start) continue;
 const label=String(i+1).padStart(2,'0');
 const attempt=new URL(`${run}-${paid?'paid':'dry'}-${label}.json`,out);
 const exists=await fs.stat(attempt).then(()=>true,()=>false);
 assert.ok(!exists,`Attempt already exists for ${s.path}; inspect ledger before any new request`);
 const inputs=s.path.includes('/eu/') ? (s.path.endsWith('/recherche')?{q:'EQUINOR ASA',pays:'NO'}:{pays:'NO',id:'923609016'}) : (s.path.endsWith('/recherche')?{q:'DANONE'}:{siren:'552032534'});
 const total=s.list_atomic+Math.ceil(s.list_atomic*0.15);
 const idempotency=randomUUID();
 const record={path:s.path,idempotency,provider_cap:s.list_atomic,buyer_cap:total,started_at:new Date().toISOString(),status:'dispatching'};
 await fs.writeFile(attempt,JSON.stringify(record,null,2)+'\n',{flag:'wx'});
 const response=await fetch('https://apiosk-gateway-v2.fly.dev/v1/do',{method:'POST',headers:{'content-type':'application/json','X-Apiosk-Service-Key':key,'Idempotency-Key':idempotency},body:JSON.stringify({candidate_id:s.endpoint_id,job:'Sirenic integration verification',inputs,deterministic:true,parse:false,max_price:total/1e6,max_price_usdc:total/1e6,dry_run:!paid,timeout_ms:90000}),signal:AbortSignal.timeout(100000)});
 const body=await response.json();
 record.status='response';record.http_status=response.status;record.body=body;
 await fs.writeFile(attempt,JSON.stringify(record,null,2)+'\n');
 console.log(JSON.stringify({path:s.path,status:response.status,did:body.did,dry_run:body.dry_run,missing:body.request?.missing_required,error:body.error,execution_status:body.execution?.status,charged:body.execution?.charged_usdc,request_id:body.execution?.request_id}));
 assert.equal(response.status,200,`HTTP failure: ${JSON.stringify(body).slice(0,300)}`);
 if(paid) {
  assert.equal(body.did,true,`Payment not confirmed: ${JSON.stringify(body).slice(0,500)}`);
  assert.equal(body.execution?.paid,true);
  assert.equal(Math.round(Number(body.execution.charged_usdc)*1e6),total);
  assert.ok(body.execution.body && typeof body.execution.body==='object');
  spent+=s.list_atomic;assert.ok(spent<=180000);
 } else {
  assert.equal(body.dry_run,true);
  assert.equal(body.decision.selected.method,'GET');
  assert.equal(Math.round(body.decision.selected.price_usd*1e6),s.list_atomic);
  assert.deepEqual(body.request.ignored_inputs,[]);
  assert.equal(body.request.method,'GET');
  assert.equal(body.request.ready,true);
  assert.equal(body.request?.missing_required?.length??0,0);
 }
}
console.log(JSON.stringify({mode:paid?'treasury procurement':'dry-run',calls:7-start,provider_spent_micro_usd:spent,customer_balance_debited:false}));
