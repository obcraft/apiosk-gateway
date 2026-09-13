// Read-only production metadata audit. Never exports credentials or customer data.
import fs from 'node:fs/promises';
import path from 'node:path';
export async function read(schema, table, params) {
  const url = new URL('/rest/v1/' + table, process.env.SUPABASE_URL);
  for (const [k,v] of Object.entries(params)) url.searchParams.set(k,v);
  const key=process.env.SUPABASE_SERVICE_ROLE_KEY;
  const r=await fetch(url,{headers:{apikey:key,Authorization:`Bearer ${key}`,'Accept-Profile':schema},signal:AbortSignal.timeout(30000)});
  if(!r.ok) throw Error(`${schema}.${table}: HTTP ${r.status}`);
  return r.json();
}
if (process.argv[1]===new URL(import.meta.url).pathname) {
  const output=path.resolve(process.argv[2]??'output/catalog-coverage');
  await fs.mkdir(output,{recursive:true});
  const apis=await read('providers','apis',{select:'id,slug,name,origin_url,docs_url,website_url,is_external,external_discovery_url',status:'eq.active',is_verified:'eq.true',or:'(discoverable.is.null,discoverable.eq.true)',suspended_at:'is.null',order:'slug'});
  const endpoints=[];
  for (let offset=0;;offset+=500) {
    const page=await read('providers','api_endpoints',{select:'id,api_id,method,path,price,query_params,request_body,response_example,response_body,description,action_type,contract_documented,external_docs_url,openapi_url',active:'eq.true',order:'id',limit:500,offset});
    endpoints.push(...page.filter(e=>apis.some(a=>a.id===e.api_id)));
    if(page.length<500)break;
  }
  const contracts=[];
  for (let offset=0;;offset+=500) {
    const page=await read('catalog','capability_execution_contracts',{select:'id,endpoint_id,capability_slug,status,validated_at,effect,price_model',order:'id',limit:500,offset});
    contracts.push(...page);
    if(page.length<500)break;
  }
  const snapshot={at:new Date().toISOString(),apis,endpoints,contracts};
  await fs.writeFile(path.join(output,'catalog.json'),JSON.stringify(snapshot,null,2)+'\n');
  const rows=apis.map(a=>{const es=endpoints.filter(e=>e.api_id===a.id);return {source:a.slug,endpoints:es.length,mapped:es.filter(e=>contracts.some(c=>c.endpoint_id===e.id&&c.status==='active')).length,missing_mapping:es.filter(e=>!contracts.some(c=>c.endpoint_id===e.id&&c.status==='active')).length,missing_response_evidence:es.filter(e=>e.response_example==null).length};});
  await fs.writeFile(path.join(output,'coverage.json'),JSON.stringify(rows,null,2)+'\n');
  console.log(JSON.stringify({sources:rows.length,endpoints:endpoints.length,mapped:rows.reduce((n,r)=>n+r.mapped,0),missing_mapping:rows.reduce((n,r)=>n+r.missing_mapping,0),output}));
}
