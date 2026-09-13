// Bounded operator checks of reviewed read-only operations. No customer debit.
// A persisted paid attempt is never resent; inspect its outcome before continuing.
import fs from 'node:fs/promises';
import {execFileSync} from 'node:child_process';
import {randomUUID} from 'node:crypto';
import assert from 'node:assert/strict';
const root=new URL('../../evidence/2026-09-13/catalog-repair/',import.meta.url);
const catalog=JSON.parse(await fs.readFile(new URL('catalog.json',root)));
const checks=process.argv.includes('--additional')?[
 ['agentbodega','/api/status/github',{includeComponents:true}],
 ['crowdpull','/api/actors/list',{query:'marketplace',limit:2}],
 ['browserbase','/fetch',{url:'https://example.com'}],
 ['browserbase','/search',{query:'Apiosk',numResults:2}],
]:[
 ['stableenrich','/api/exa/search',{query:'Apiosk data APIs',numResults:2}],
 ['linkup','/v1/search',{q:'What does Apiosk do?',depth:'standard',outputType:'sourcedAnswer'}],
 ['linkup','/v1/fetch',{url:'https://example.com',mode:'standard'}],
];
const pay=process.argv.includes('--pay');
const output=new URL('procurement/',root);await fs.mkdir(output,{recursive:true});
let key=process.env.APIOSK_APP_SERVICE_KEY;
key ||= execFileSync('fly',['ssh','console','-a','apiosk-gateway-v2','-q','-C',`sh -c 'printf "%s" "$APIOSK_APP_SERVICE_KEY"'`],{encoding:'utf8',stdio:['ignore','pipe','pipe']}).trim();
assert.ok(key.length>20&&!key.includes('\n'),'service credential unavailable');
let committed=0;
const start=Number(process.argv.find(a=>a.startsWith('--start='))?.split('=')[1]??1);
assert.ok(Number.isInteger(start)&&start>=1&&start<=checks.length);
for(const [index,[slug,path,inputs]] of checks.entries()){
 const api=catalog.apis.find(a=>a.slug===slug),e=catalog.endpoints.find(e=>e.api_id===api.id&&e.path===path);
 const cap=Math.ceil(Number(e.price)*1e6),total=cap+Math.ceil(cap*.15);committed+=cap;
 assert.ok(committed<=30000,'Maximum $0.03 upstream spend');
 const file=new URL(`${slug}-${path.replaceAll('/','_')}-${pay?'paid':'dry'}.json`,output);
 if(index+1<start){
  const prior=JSON.parse(await fs.readFile(file));
  assert.equal(prior.endpoint_id,e.id);assert.equal(prior.status,'response');
  // A failed or unresolved operation stays consumed, while independent sources
  // can still be checked. This never sends the prior authorization again.
  continue;
 }
 const handle=await fs.open(file,'wx');
 const record={source:slug,path,endpoint_id:e.id,inputs,provider_cap_atomic:cap,buyer_equivalent_atomic:total,idempotency:randomUUID(),status:'dispatching',at:new Date().toISOString()};
 await handle.writeFile(JSON.stringify(record));await handle.close();
 const r=await fetch('https://apiosk-gateway-v2.fly.dev/v1/do',{method:'POST',headers:{'content-type':'application/json','X-Apiosk-Service-Key':key,'Idempotency-Key':record.idempotency},body:JSON.stringify({candidate_id:e.id,job:'Read-only source integration verification',inputs,deterministic:true,parse:false,max_price:total/1e6,max_price_usdc:total/1e6,dry_run:!pay,timeout_ms:90000}),signal:AbortSignal.timeout(100000)});
 const body=await r.json();Object.assign(record,{status:'response',http_status:r.status,body});await fs.writeFile(file,JSON.stringify(record,null,2)+'\n');
 console.log(JSON.stringify({source:slug,path,http_status:r.status,did:body.did,error:body.error,message:body.message,ready:body.request?.ready,missing:body.request?.missing_required,execution:body.execution?{status:body.execution.status,paid:body.execution.paid,charged:body.execution.charged_usdc,request_id:body.execution.request_id}:null}));
 assert.equal(r.status,200);if(pay){assert.equal(body.did,true);assert.equal(body.execution.paid,true);assert.match(body.execution.tx_hash,/^0x[0-9a-f]{64}$/i);}else {assert.equal(body.dry_run,true);assert.equal(body.request.ready,true);assert.deepEqual(body.request.ignored_inputs,[]);}
}
