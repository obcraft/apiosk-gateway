// Operator check of the five Reuters/Bloomberg news operations through the
// production gateway /v1/do. Dry run by default; `--pay` performs one real
// execution per operation (uses RapidAPI quota, Apiosk-revenue settlement).
// Each attempt is recorded once (`wx`) and never resent.
import fs from 'node:fs/promises';
import {execFileSync} from 'node:child_process';
import {randomUUID} from 'node:crypto';
import assert from 'node:assert/strict';
const fixture=JSON.parse(await fs.readFile(new URL('../tests/fixtures/rapidapi-news/catalog.json',import.meta.url)));
const output=new URL('../../evidence/2026-09-14/rapidapi-news/gateway/',import.meta.url);
await fs.mkdir(output,{recursive:true});
const inputs={
 'news.reuters-latest':{limit:3},
 'news.reuters-search':{q:'bitcoin',limit:3},
 'news.reuters-section':{url:'https://www.reuters.com/world/africa/'},
 'news.reuters-sections':{},
 'news.bloomberg-latest':{id:'phx-latest'},
};
const pay=process.argv.includes('--pay');
let key=process.env.APIOSK_APP_SERVICE_KEY;
key ||= execFileSync('fly',['ssh','console','-a','apiosk-gateway-v2','-q','-C',`sh -c 'printf "%s" "$APIOSK_APP_SERVICE_KEY"'`],{encoding:'utf8',stdio:['ignore','pipe','pipe']}).trim();
assert.ok(key.length>20&&!key.includes('\n'),'service credential unavailable');
for(const s of fixture.services){
 const cap=s.contract.capability_slug,total=s.list_atomic+Math.ceil(s.list_atomic*.15);
 const file=new URL(`${cap}-${pay?'paid':'dry'}.json`,output);
 const handle=await fs.open(file,'wx');
 const record={capability:cap,endpoint_id:s.endpoint_id,inputs:inputs[cap],buyer_total_atomic:total,idempotency:randomUUID(),status:'dispatching',at:new Date().toISOString()};
 await handle.writeFile(JSON.stringify(record));await handle.close();
 const started=Date.now();
 const r=await fetch('https://apiosk-gateway-v2.fly.dev/v1/do',{method:'POST',headers:{'content-type':'application/json','X-Apiosk-Service-Key':key,'Idempotency-Key':record.idempotency},
  body:JSON.stringify({candidate_id:s.endpoint_id,job:'Read-only news source integration verification',inputs:inputs[cap],deterministic:true,parse:false,max_price:total/1e6,max_price_usdc:total/1e6,dry_run:!pay,timeout_ms:60000}),signal:AbortSignal.timeout(90000)});
 const body=await r.json();
 Object.assign(record,{status:'response',http_status:r.status,elapsed_ms:Date.now()-started,body});
 await fs.writeFile(file,JSON.stringify(record,null,2)+'\n');
 const data=body.data??body.result??body.response;
 console.log(JSON.stringify({cap,http:r.status,ms:record.elapsed_ms,did:body.did,error:body.error,message:body.message,ready:body.request?.ready,ignored:body.request?.ignored_inputs,
  execution:body.execution?{status:body.execution.status,paid:body.execution.paid,charged:body.execution.charged_usdc,upstream:body.execution.upstream_status}:null,
  items:Array.isArray(data)?data.length:data?.data?.modules?.[1]?.stories?.length}));
 assert.equal(r.status,200);
 if(pay)assert.equal(body.did,true);
 else {assert.equal(body.dry_run,true);assert.equal(body.request.ready,true);assert.deepEqual(body.request.ignored_inputs,[]);}
}
