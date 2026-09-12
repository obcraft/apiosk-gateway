// Fixed-price operator checks; default dry-run. Each paid attempt is immutable
// and sent at most once. No customer balance is debited; treasury cap $2.608.
import fs from 'node:fs/promises';
import {setTimeout as pause} from 'node:timers/promises';
import {execFileSync} from 'node:child_process';
import {randomUUID} from 'node:crypto';
import assert from 'node:assert/strict';
const root=new URL('../',import.meta.url);
const fixture=JSON.parse(await fs.readFile(new URL('tests/fixtures/sirenic/expanded-catalog.json',root),'utf8'));
const services=fixture.services.filter(s=>s.check_inputs);
const paid=process.argv.includes('--pay');
const out=new URL(`output/sirenic-20260913/${paid?'payments':'dry'}/`,root);
await fs.mkdir(out,{recursive:true});
assert.equal(services.reduce((n,s)=>n+s.list_atomic,0),2608000);
let key=process.env.APIOSK_APP_SERVICE_KEY;
try {key ||= execFileSync('fly',['ssh','console','-a','apiosk-gateway-v2','-q','-C',`sh -c 'printf "%s" "$APIOSK_APP_SERVICE_KEY"'`],{encoding:'utf8',stdio:['ignore','pipe','pipe']}).trim();}catch{throw Error('Service credential unavailable');}
assert.ok(key?.length>20 && !key.includes('\n'));
let spent=0,successful=0;
for(const [i,s] of services.entries()) {
 const file=new URL(`${s.id}.json`,out);
 let prior=await fs.readFile(file,'utf8').then(JSON.parse,()=>null);
 if(!paid && prior?.http_status===429){await fs.rename(file,new URL(`${s.id}-rate-limited.txt`,out));prior=null;}
 if(paid && process.argv.includes('--recheck-reviewed') && prior) {
  const reviewed=JSON.parse(await fs.readFile(new URL('output/sirenic-20260913/reviewed-rechecks.json',root),'utf8'));
  const request=prior.body?.execution?.request_id;
  if(reviewed[request]) {
   assert.equal(prior.body.execution.paid,false);
   if(prior.http_status===504) {
    const proof=JSON.parse(await fs.readFile(new URL(`output/sirenic-20260913/reconciliation/${request}.json`,root),'utf8'));
    assert.equal(proof.database_status,'failed');assert.equal(proof.authorization_used,false);assert.ok(proof.block_timestamp>proof.expiry_upper_bound);
   }else{assert.equal(prior.body.execution.status,502);assert.equal(reviewed[request].ledger_status,'absent');}
   const archive=new URL('output/sirenic-20260913/previous-attempts/',root);await fs.mkdir(archive,{recursive:true});
   const target=new URL(`${request}.json`,archive);assert.equal(await fs.stat(target).then(()=>true,()=>false),false);
   await fs.rename(file,target);prior=null;
  }
 }
 if(prior){
  assert.equal(prior.path,s.path);assert.equal(prior.provider_cap,s.list_atomic);
  assert.equal(prior.status,'response','Prior attempt needs reconciliation');
  if(prior.http_status===504 || prior.body.error==='payment_unresolved') {
   const request=prior.body.execution.request_id;
   const proof=JSON.parse(await fs.readFile(new URL(`output/sirenic-20260913/reconciliation/${request}.json`,root),'utf8'));
   assert.equal(proof.request_id,request);assert.equal(proof.database_status,'failed');assert.equal(proof.authorization_used,false);assert.ok(proof.block_timestamp>proof.expiry_upper_bound);
  }
  spent+=s.list_atomic;continue;
 }
 const total=s.list_atomic+Math.ceil(s.list_atomic*.15),idempotency=randomUUID();
 const record={reviewed_max_timeout_seconds:200,service_id:s.id,path:s.path,inputs:s.check_inputs,idempotency,provider_cap:s.list_atomic,buyer_cap:total,started_at:new Date().toISOString(),status:'dispatching'};
 await fs.writeFile(file,JSON.stringify(record,null,2)+'\n',{flag:'wx'});
 await pause(2200);
 const response=await fetch('https://apiosk-gateway-v2.fly.dev/v1/do',{method:'POST',headers:{'content-type':'application/json','X-Apiosk-Service-Key':key,'Idempotency-Key':idempotency},body:JSON.stringify({candidate_id:s.endpoint_id,job:'Sirenic expanded source verification',inputs:s.check_inputs,deterministic:true,parse:false,max_price:total/1e6,max_price_usdc:total/1e6,dry_run:!paid,timeout_ms:90000}),signal:AbortSignal.timeout(100000)});
 const body=await response.json();Object.assign(record,{status:'response',http_status:response.status,finished_at:new Date().toISOString(),body});await fs.writeFile(file,JSON.stringify(record,null,2)+'\n');
 console.log(JSON.stringify({i:i+1,path:s.path,status:response.status,did:body.did,paid:body.execution?.paid,execution_status:body.execution?.status,charged:body.execution?.charged_usdc,request_id:body.execution?.request_id,error:body.error}));
 assert.ok(response.status!==504 && body.error?.code!=='payment_unresolved','Unresolved payment: stop and reconcile');
 if(paid){
  spent+=s.list_atomic;assert.ok(spent<=2608000);
  if(body.execution?.paid===true){assert.equal(body.did,true);assert.equal(Math.round(Number(body.execution.charged_usdc)*1e6),total);assert.match(body.execution.tx_hash,/^0x[0-9a-f]{64}$/i);successful++;}
 }else{
  assert.equal(response.status,200);assert.equal(body.dry_run,true);assert.equal(body.request.ready,true);assert.equal(body.request.method,'GET');assert.deepEqual(body.request.ignored_inputs,[]);assert.equal(Math.round(body.decision.selected.price_usd*1e6),s.list_atomic);
 }
}
console.log(JSON.stringify({paid,successful,attempt_cap_micro_usd:spent,customer_balance_debited:false}));
