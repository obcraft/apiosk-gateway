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
  const apis=await read('providers','apis',{select:'id,owner_id,slug,name,origin_url,docs_url,website_url,is_external,external_discovery_url,listing_metadata,active',status:'eq.active',is_verified:'eq.true',or:'(discoverable.is.null,discoverable.eq.true)',suspended_at:'is.null',order:'slug'});
  const profiles=await read('providers','provider_profiles',{select:'id,username',order:'username'});
  const owners=new Map(profiles.map(p=>[p.id,p.username]));
  const visible=apis.filter(a=>a.active!==false).map(({listing_metadata,...a})=>({
    ...a,planner_block_reason:listing_metadata?.planner_block_reason??null,
  }));
  const ids=new Set(visible.map(a=>a.id));
  const endpoints=[];
  for (let offset=0;;offset+=500) {
    const page=await read('providers','api_endpoints',{select:'id,api_id,active,method,path,price,query_params,request_body,response_example,response_body,description,action_type,contract_documented,external_docs_url,openapi_url',order:'id',limit:500,offset});
    endpoints.push(...page.filter(e=>ids.has(e.api_id)));
    if(page.length<500)break;
  }
  const contracts=[];
  for (let offset=0;;offset+=500) {
    const page=await read('catalog','capability_execution_contracts',{select:'id,endpoint_id,capability_slug,status,validated_at,effect,price_model',order:'id',limit:500,offset});
    contracts.push(...page);
    if(page.length<500)break;
  }
  const offers=[];
  for (let offset=0;;offset+=500) {
    const page=await read('catalog','capability_offers',{select:'endpoint_id,capability_slug,source',order:'endpoint_id',limit:500,offset});
    offers.push(...page);
    if(page.length<500)break;
  }
  const snapshot={at:new Date().toISOString(),apis:visible,endpoints,contracts,offers,profiles};
  await fs.writeFile(path.join(output,'catalog.json'),JSON.stringify(snapshot,null,2)+'\n');
  const contractsByEndpoint=Map.groupBy(contracts,c=>c.endpoint_id);
  const offersByEndpoint=Map.groupBy(offers,o=>o.endpoint_id);
  // Grouped providers: their listings are services of one source (src/sources.rs GROUPED_PROVIDERS).
  const grouped=new Set(['pulsenetwork','coinbase-bazaar']);
  const sourceFor=a=>grouped.has(owners.get(a.owner_id))?owners.get(a.owner_id):a.slug;
  const rows=Map.groupBy(visible,sourceFor);
  const coverage=[...rows].map(([source,services])=>{
    const all=endpoints.filter(e=>services.some(a=>a.id===e.api_id));
    const es=all.filter(e=>e.active);
    const state=e=>{
      const cs=contractsByEndpoint.get(e.id)??[];
      return cs.some(c=>c.status==='active'&&c.validated_at&&c.effect==='read'&&c.price_model==='per_call')?'active'
        :cs.some(c=>c.status==='validated')?'staged':'uncontracted';
    };
    return {source,services:services.length,endpoints:es.length,inactive_endpoints:all.length-es.length,
      active:es.filter(e=>state(e)==='active').length,
      staged:es.filter(e=>state(e)==='staged').length,
      uncontracted:es.filter(e=>state(e)==='uncontracted').length,
      without_response_example:es.filter(e=>e.response_example==null).length,
      without_capability_link:es.filter(e=>!(contractsByEndpoint.get(e.id)?.length||offersByEndpoint.get(e.id)?.length)).length,
      blocked_services:services.filter(a=>a.planner_block_reason).map(a=>a.slug),
      service_slugs:services.map(a=>a.slug).sort()};
  }).sort((a,b)=>a.source.localeCompare(b.source));
  await fs.writeFile(path.join(output,'coverage.json'),JSON.stringify(coverage,null,2)+'\n');
  console.log(JSON.stringify({sources:coverage.length,services:visible.length,endpoints:coverage.reduce((n,r)=>n+r.endpoints,0),inactive_endpoints:coverage.reduce((n,r)=>n+r.inactive_endpoints,0),active:coverage.reduce((n,r)=>n+r.active,0),staged:coverage.reduce((n,r)=>n+r.staged,0),uncontracted:coverage.reduce((n,r)=>n+r.uncontracted,0),output}));
}
