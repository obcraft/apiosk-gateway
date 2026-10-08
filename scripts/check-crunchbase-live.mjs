// Operator verification. --execute uses one call per operation from the
// existing shared Free subscription. No subscription or purchase is created.
// Attempt files are exclusive: an uncertain attempt is never auto-retried.
import fs from 'node:fs/promises';
import {execFileSync} from 'node:child_process';
import {randomUUID} from 'node:crypto';
import assert from 'node:assert/strict';

const fixture=JSON.parse(await fs.readFile(new URL('../tests/fixtures/crunchbase/catalog.json',import.meta.url)));
const output=new URL('../output/crunchbase-gateway/',import.meta.url);
await fs.mkdir(output,{recursive:true});
const execute=process.argv.includes('--execute');
let key=process.env.APIOSK_APP_SERVICE_KEY;
key ||= execFileSync('fly',['ssh','console','-a','apiosk-gateway-v2','-q','-C',`sh -c 'printf "%s" "$APIOSK_APP_SERVICE_KEY"'`],{encoding:'utf8',stdio:['ignore','pipe','pipe']}).trim();
assert.ok(key.length>20&&!key.includes('\n'),'service credential unavailable');
for(const service of fixture.services){
 const endpoint=fixture.endpoints.find(e=>e.id===service.endpoint_id);
 const file=new URL(`${service.contract.capability_slug}-${execute?'executed':'dry'}.json`,output);
 const record={endpoint_id:service.endpoint_id,inputs:endpoint.request_example,idempotency:randomUUID(),status:'dispatching',at:new Date().toISOString()};
 await fs.writeFile(file,JSON.stringify(record),{flag:'wx'});
 const response=await fetch('https://apiosk-gateway-v2.fly.dev/v1/do',{
  method:'POST',headers:{'content-type':'application/json','X-Apiosk-Service-Key':key,'Idempotency-Key':record.idempotency},
  body:JSON.stringify({candidate_id:service.endpoint_id,job:'Verify Crunchbase company integration',inputs:record.inputs,deterministic:true,parse:false,max_price:0.10,max_price_usdc:0.10,dry_run:!execute,timeout_ms:60000}),signal:AbortSignal.timeout(90000)
 });
 const body=await response.json();
 Object.assign(record,{status:'response',http_status:response.status,body});
 await fs.writeFile(file,JSON.stringify(record,null,2)+'\n');
 console.log(JSON.stringify({capability:service.contract.capability_slug,http:response.status,did:body.did,error:body.error,ready:body.request?.ready,execution:body.execution}));
 assert.equal(response.status,200);
 if(execute)assert.equal(body.did,true);
 else {assert.equal(body.dry_run,true);assert.equal(body.request.ready,true);assert.deepEqual(body.request.ignored_inputs,[]);}
}
