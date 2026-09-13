// Four read-only requests through the configured provider subscription.
// No Apiosk buyer debit or x402 purchase; never print or persist credentials.
import fs from 'node:fs/promises';
import path from 'node:path';
import {read} from './audit-catalog-coverage.mjs';
const output=path.resolve(process.argv[2]??'output/cityfalcon-live');
await fs.mkdir(output,{recursive:true});
const [a]=await read('providers','apis',{select:'id,origin_url,listing_metadata',slug:'eq.cityfalcon-financial-api'});
if(a.origin_url!=='https://api.cityfalcon.com/v0.2')throw Error('CityFALCON configured origin changed');
const es=await read('providers','api_endpoints',{select:'id,path,method',api_id:`eq.${a.id}`,active:'eq.true'});
const secrets=(a.listing_metadata.auth_query??[]).map(q=>q.value);
for(const e of es) {
  if(e.method!=='GET'||!['/stories','/services/sentiment','/insider_transactions','/investor_relations'].includes(e.path))throw Error('Unexpected operation');
  const file=path.join(output,e.path.replaceAll('/','_')+'.json');
  const fh=await fs.open(file,'wx');
  try {
    const query={identifiers:'AAPL_US',...(e.path==='/stories'?{identifier_type:'full_tickers',languages:'en',time_filter:'d1'}:{}),...(e.path==='/services/sentiment'?{identifier_type:'full_tickers',period:'d1'}:{})};
    const url=new URL(a.origin_url.replace(/\/$/,'')+e.path);
    for(const [k,v] of Object.entries(query))url.searchParams.set(k,v);
    for(const q of a.listing_metadata.auth_query??[])url.searchParams.set(q.name,q.value);
    const response=await fetch(url,{headers:{Accept:'application/json'},redirect:'error',signal:AbortSignal.timeout(45000)});
    let body=await response.text();for(const secret of secrets)if(secret)body=body.replaceAll(secret,'[REDACTED]');
    let data;try{data=JSON.parse(body)}catch{data={non_json:body.slice(0,500)}}
    const record={at:new Date().toISOString(),endpoint_id:e.id,path:e.path,query,status:response.status,data};
    await fh.writeFile(JSON.stringify(record,null,2)+'\n');
    console.log(JSON.stringify({path:e.path,status:response.status,keys:Object.keys(data),error:response.ok?undefined:data}));
    if(!response.ok||data.non_json)throw Error(`CityFALCON ${e.path}: HTTP ${response.status} or invalid response`);
  } finally {await fh.close();}
}
