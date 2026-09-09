// Read-only live source audit. No Apiosk connector, approval, billing or database writes.
// Run: node --env-file=.env scripts/test-direct-usecases.mjs
import fs from 'node:fs/promises';
import path from 'node:path';
const slugs=['kvk-dutch-business-register','gleif','opensanctions','companies-house','tenderned','cbs-statline','eurostat','data-overheid-nl','pdok','overheid-io'];
const sb=process.env.SUPABASE_URL, key=process.env.SUPABASE_SERVICE_ROLE_KEY;
if(!sb||!key) throw Error('Configured database credentials required');
async function read(table,params){const url=new URL('/rest/v1/'+table,sb);for(const [k,v] of Object.entries(params))url.searchParams.set(k,v);const r=await fetch(url,{headers:{apikey:key,Authorization:'Bearer '+key},signal:AbortSignal.timeout(30000),redirect:'error'});if(!r.ok)throw Error('Metadata read HTTP '+r.status);return r.json();}
const apis=await read('apis',{select:'id,slug,origin_url,endpoint_url,listing_metadata',slug:'in.('+slugs.join(',')+')'});
const endpoints=await read('api_endpoints',{select:'id,api_id,path,method,origin_override,auth_override',api_id:'in.('+apis.map(a=>a.id).join(',')+')',active:'eq.true'});
const out=await fs.mkdtemp(path.resolve('output/direct-usecases-'));
console.log('EVIDENCE_DIRECTORY '+out);
const secrets=[key,...apis.flatMap(a=>(a.listing_metadata?.auth_headers||[]).map(x=>x.value)),...endpoints.flatMap(e=>(e.auth_override?.auth_headers||[]).map(x=>x.value))].filter(x=>typeof x==='string'&&x.length>5);
function scrub(s){for(const x of secrets)s=s.replaceAll(x,'[REDACTED]');return s;}
let count=0;const results=[];let annualLast=0;
async function call(id,slug,route,{query={},params={},append='',body,actualPath}={}){
 const api=apis.find(a=>a.slug===slug), ep=endpoints.find(e=>e.api_id===api?.id&&e.path===route);
 if(!api||!ep)throw Error('Unknown configured endpoint '+slug+route);
 if(++count>40)throw Error('40-call audit cap');
 const meta=ep.auth_override??(ep.origin_override?{}:api.listing_metadata??{});
 let p=actualPath??ep.path;for(const [k,v] of Object.entries(params))p=p.replaceAll('{'+k+'}',encodeURIComponent(v));
 if(append)p+='/'+encodeURIComponent(append);
 if(p.includes('{'))throw Error('Unresolved route');
 const url=new URL((ep.origin_override||api.endpoint_url||api.origin_url).replace(/\/$/,'')+'/'+p.replace(/^\//,''));
 if(url.protocol!=='https:')throw Error('HTTPS required');
 for(const [k,v] of Object.entries(query))url.searchParams.set(k,String(v));
 const publicURL=url.toString();
 const headers={Accept:'application/json'};for(const h of meta.auth_headers||[])headers[h.name]=h.value;
 for(const q of meta.auth_query||[])url.searchParams.set(q.name,q.value);
 if(body)headers['Content-Type']='application/json';
 if(url.hostname==='opendata.kvk.nl'&&route.includes('jaarrekeningen')){
   const wait=61000-(Date.now()-annualLast);if(wait>0){console.log('KVK_COOLDOWN_SECONDS '+Math.ceil(wait/1000));await new Promise(r=>setTimeout(r,wait));}annualLast=Date.now();
 }
 const start=Date.now();let status=0,data,error;
 try{const r=await fetch(url,{method:ep.method,headers,body:body?JSON.stringify(body):undefined,signal:AbortSignal.timeout(45000),redirect:'manual'});status=r.status;const txt=scrub(await r.text());try{data=JSON.parse(txt);}catch{data={non_json_preview:txt.slice(0,1500)};}if(!r.ok)error='HTTP '+status;}
 catch(e){error=scrub(e.name+': '+e.message);}
 const record={id,source:slug,route,endpoint_id:ep.id,url:publicURL,method:ep.method,query,request_body:body,at:new Date().toISOString(),duration_ms:Date.now()-start,status,error,data};
 await fs.writeFile(path.join(out,id+'.json'),JSON.stringify(record,null,2));results.push(record);
 console.log(JSON.stringify({id,status,error,ms:record.duration_ms,keys:data&&typeof data==='object'?Object.keys(data).slice(0,10):[],count:data?.total??data?.totaal??data?.total_results??data?.result?.count??data?.['@odata.count']}));return record;
}
function normalized(s){return String(s).toLowerCase().replace(/[^a-z0-9]/g,'');}
function exact(rows,name,label,id){const matches=rows.filter(x=>normalized(label(x))===normalized(name));const ids=[...new Set(matches.map(id))];if(ids.length!==1)return null;return matches.find(x=>id(x)===ids[0]);}
async function kvk(){
 const found={};for(const [short,name] of [['hema','HEMA B.V.'],['afas','AFAS Software B.V.'],['coolblue','Coolblue B.V.']]){
   const r=await call(short+'-search','kvk-dutch-business-register','/api/v2/zoeken',{query:{naam:name}});
   const rows=r.data?.resultaten??[];found[short]=exact(rows,name,x=>x.naam,x=>x.kvkNummer);
   console.log(JSON.stringify({selection:short,selected:found[short]?.kvkNummer??null,candidates:rows.map(x=>({name:x.naam,kvk:x.kvkNummer,type:x.type})).slice(0,10)}));
 }
 if(found.coolblue)await call('coolblue-profile','kvk-dutch-business-register','/api/v1/basisprofielen',{append:found.coolblue.kvkNummer});
 for(const short of ['hema','afas'])if(found[short])await call(short+'-accounts','kvk-dutch-business-register','/api/v1/hvds/jaarrekeningen/kvknummer',{append:found[short].kvkNummer});
}
async function gleif(){const r=await call('asml-search','gleif','/lei-records',{query:{'filter[entity.legalName]':'ASML Holding N.V.','page[size]':10}});const s=exact(r.data?.data??[],'ASML Holding N.V.',x=>x.attributes?.entity?.legalName?.name,x=>x.id);console.log(JSON.stringify({selection:'asml',selected:s?.id??null,candidates:(r.data?.data??[]).map(x=>({id:x.id,name:x.attributes?.entity?.legalName?.name}))}));if(s)await call('asml-children','gleif','/lei-records/{lei}/direct-children',{params:{lei:s.id}});}
async function uk(){const r=await call('monzo-search','companies-house','/search/companies',{query:{q:'MONZO BANK LIMITED',items_per_page:10}});const s=exact(r.data?.items??[],'MONZO BANK LIMITED',x=>x.title,x=>x.company_number);console.log(JSON.stringify({selection:'monzo',selected:s?.company_number??null}));if(s){await call('monzo-profile','companies-house','/company',{append:s.company_number});for(const route of ['officers','persons-with-significant-control'])await call('monzo-'+route,'companies-house','/company/{company_number}/'+route,{params:{company_number:s.company_number},query:{items_per_page:100}});}}
const jobs=[kvk,gleif,uk,
 ()=>call('coolblue-sanctions','opensanctions','/match',{append:'default',query:{limit:10},body:{queries:{q1:{schema:'Company',properties:{name:['Coolblue B.V.']}}}}}),
 ()=>call('tenders','tenderned','/v2/publicaties',{query:{search:'laadpalen'}}),
 ()=>call('cbs-bankruptcies','cbs-statline','/odata/v1/CBS/82244NED/Observations',{query:{$top:100,$count:true,$orderby:'Perioden desc'}}),
 async()=>{for(const [name,code,select] of [['measures','MeasureCodes','Identifier,Title,Unit'],['sectors','BedrijfstakkenBranchesSBI2008Codes','Identifier,Title'],['entity-types','TypeGefailleerdeCodes','Identifier,Title']])await call('cbs-'+name,'cbs-statline','/odata/v1/CBS/{tableId}/'+code,{params:{tableId:'82244NED'},query:{$select:select}});},
 async()=>{for(const geo of ['NL','DE'])await call('gdp-'+geo,'eurostat','/statistics/1.0/data',{append:'nama_10_gdp',query:{geo,time:'2023',unit:'CP_MEUR',na_item:'B1GQ',format:'JSON',lang:'EN'}});},
 ()=>call('waterquality','data-overheid-nl','/data/api/3/action/package_search',{query:{q:'waterkwaliteit',rows:20}}),
 ()=>call('dam-address','pdok','/bzk/locatieserver/search/v3_1/free',{query:{q:'Dam 1 Amsterdam'}}),
 ()=>call('postcode-addresses','overheid-io','/bag',{query:{'filters[postcode]':'1012JS'}})
];
let next=0;await Promise.all(Array.from({length:3},async()=>{while(next<jobs.length){const job=jobs[next++];try{await job();}catch(e){console.log('JOB_ERROR '+scrub(e.message));}}}));
await fs.writeFile(path.join(out,'manifest.json'),JSON.stringify({started:new Date().toISOString(),scope:'Direct configured upstreams; no Apiosk billing or connector. Provider subscription usage may apply.',calls:results.map(({data,request_body,...r})=>r)},null,2));
console.log('AUDIT_FINISHED '+out+' calls='+count);
