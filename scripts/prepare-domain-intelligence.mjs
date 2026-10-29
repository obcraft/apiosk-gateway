// Domain intelligence (RDAP + DNS MX) for the supplier payment check.
// Samples are the production adapter's normalized output for real registry
// responses in crates/commerce-runtime/src/proxy/fixtures/domain-intel/.
// Usage: node scripts/prepare-domain-intelligence.mjs supabase/migrations/<version>_onboard_domain_intelligence.sql
import fs from 'node:fs/promises';
import crypto from 'node:crypto';
const root = new URL('../', import.meta.url);
const fixture = async name => JSON.parse(await fs.readFile(new URL(`crates/commerce-runtime/src/proxy/fixtures/domain-intel/${name}`, root), 'utf8'));
const id = s => { const h = crypto.createHash('sha256').update('domain-intelligence-v1:' + s).digest('hex'); return `${h.slice(0,8)}-${h.slice(8,12)}-4${h.slice(13,16)}-8${h.slice(17,20)}-${h.slice(20,32)}`; };

const rdapRaw = await fixture('rdap-nl.json'), dnsRaw = await fixture('dns-mx.json');
const event = a => rdapRaw.events.find(e => e.eventAction === a)?.eventDate ?? null;
const registrar = rdapRaw.entities.find(e => e.roles?.includes('registrar'))?.vcardArray[1].find(f => f[0] === 'fn')?.[3] ?? null;
const rdapSample = {data:{domain:'mollie.nl',registered_at:event('registration'),last_changed_at:event('last changed'),expires_at:event('expiration'),registrar,statuses:rdapRaw.status??[],nameservers:(rdapRaw.nameservers??[]).map(n=>n.ldhName.toLowerCase()),rdap_server:'rdap.sidn.nl'},
  notice:'Registry RDAP data at request time. Registrant identity is usually redacted; a registration date shows when the current registration started, not who controls the domain.',
  coverage:{complete:true,scope:'Authoritative registry RDAP record for one domain'},source_url:'https://rdap.sidn.nl/domain/mollie.nl'};
const mx = dnsRaw.Answer.filter(a => a.type === 15 && a.data.split(/\s+/)[1] !== '.').map(a => a.data);
const dnsSample = {data:{domain:'mollie.com',exists:true,accepts_mail:mx.length>0,mx,dnssec_validated:dnsRaw.AD===true},
  notice:'Public DNS MX lookup at request time (Google Public DNS). DNS shows mail routing, not ownership.',
  coverage:{complete:true,scope:'MX records for one domain'},source_url:'https://dns.google/resolve?name=mollie.com&type=MX'};

const slug = 'domain-intelligence', name = 'Domain Intelligence (RDAP & DNS)';
const description = 'Registration date, registrar and status of a domain from the authoritative registry (RDAP), and whether the domain receives e-mail (DNS MX). Supports .nl, .com, .net, .org, .uk, .fr, .app and .dev; registries of .de, .eu and .be publish no registration date.';
const docs = 'https://about.rdap.org/';
const tags = ['RDAP','WHOIS','domain','domein','domain age','registratiedatum','DNS','MX','fraud','supplier check','leveranciercontrole'];
const sectors = ['Security','Company data','Finance'];
const price = 0.085066, listAtomic = 85066;
const domainFact = {slug:'domain.name',name:'domain name',description:'Registrable domain name in lowercase, e.g. example.nl',value_schema:{type:'string',minLength:4,maxLength:253,pattern:'^([a-z0-9-]{1,63}\\.)+[a-z]{2,63}$',description:'Registrable domain name in lowercase, e.g. example.nl'}};
const str = (d, extra={}) => ({type:'string',description:d,...extra});
const envelope = data => ({type:'object',required:['data','notice','coverage','source_url'],properties:{data,notice:str('Source limitations'),coverage:{type:'object',required:['complete','scope'],properties:{complete:{type:'boolean'},scope:str('Scope')}},source_url:str('Exact upstream request URL')}});
const endpoints = [
  {path:'/domain/{domain}',origin:null,capability:'domain.registration',title:'Domain registration date (RDAP)',sample:rdapSample,result:'domain.registration.result',
   response:envelope({type:'object',required:['domain','statuses','nameservers'],properties:{domain:str('Domain'),registered_at:{type:['string','null'],description:'RFC 3339 registration event'},last_changed_at:{type:['string','null']},expires_at:{type:['string','null']},registrar:{type:['string','null']},statuses:{type:'array'},nameservers:{type:'array'},rdap_server:{type:['string','null']}}})},
  {path:'/dns/{domain}',origin:'https://dns.google',capability:'domain.mail-dns',title:'Domain mail servers (DNS MX)',sample:dnsSample,result:'domain.mail-dns.result',
   response:envelope({type:'object',required:['domain','exists','accepts_mail','mx','dnssec_validated'],properties:{domain:str('Domain'),exists:{type:'boolean'},accepts_mail:{type:'boolean'},mx:{type:'array',items:{type:'string'}},dnssec_validated:{type:'boolean'}}})},
];
const q = v => v == null ? 'NULL' : `'${String(v).replaceAll("'", "''")}'`, j = v => `${q(JSON.stringify(v))}::jsonb`, a = v => `ARRAY[${v.map(q).join(',')}]::text[]`;
const input = {type:'object',required:['domain'],properties:{domain:{...domainFact.value_schema,'x-parameter-location':'path'}}};
const metadata = {tags,sectors,adapter:'domain-intelligence-v1',capability_adapter:'domain-intelligence-v1',catalog_state:'staged',coverage_notice:description,
  billing:{mode:'apiosk_balance',target_total_eur:0.09,list_micro_usd:listAtomic,buyer_total_micro_usd:97826,conversion_rate:0.92,conversion_date:'2026-08-21'},
  operator_notice:'Apiosk integration of public registry and DNS data; not affiliated with the registries or Google.',upstream_cost_eur:0};
const sql = [
  '-- Domain intelligence v1 (RDAP + DNS MX) for the supplier payment check. Staged until the adapter passes production-egress probes.',
  `-- List USD ${price} + fee = USD 0.097826 buyer total per call (EUR 0.09 at the stored 0.92 rate).`,
  'BEGIN;', "SELECT pg_advisory_xact_lock(hashtext('apiosk-domain-intelligence-v1'));",
  'DO $onboard$ DECLARE owner uuid; source_id uuid; BEGIN',
  "SELECT owner_id INTO STRICT owner FROM providers.apis WHERE slug='koop' AND is_platform_owned AND settlement_mode='apiosk_revenue';",
  `INSERT INTO providers.apis(id,owner_id,slug,name,status,description,long_description,category,docs_url,website_url,origin_url,endpoint_url,authentication_info,is_verified,active,discoverable,listing_type,listing_metadata,integration_mode,settlement_mode,is_external,is_platform_owned,is_community,source,requires_payout_wallet) VALUES(gen_random_uuid(),owner,${q(slug)},${q(name)},'inactive',${q(description)},${q(description)},'security',${q(docs)},${q(docs)},'https://rdap.org','https://rdap.org','{"type":"none","caller":"Apiosk balance approval required"}',false,false,false,'api',${j(metadata)},'gateway','apiosk_revenue',false,true,false,'manual',false) ON CONFLICT(slug) DO NOTHING;`,
  `SELECT id INTO STRICT source_id FROM providers.apis WHERE slug=${q(slug)} AND owner_id=owner AND origin_url='https://rdap.org';`,
];
for (const e of endpoints) sql.push(`INSERT INTO providers.api_endpoints(id,api_id,method,path,price,active,payment_required,cache_ttl_seconds,description,query_params,request_body,response_body,request_example,response_example,tags,search_terms,sectors,use_cases,input_entities,output_entities,endpoint_category,action_type,external_docs_url,documentation_markdown,metadata_source,metadata_confidence,origin_override,task_summary,contract_documented)
SELECT gen_random_uuid(),source_id,'GET',${q(e.path)},${price},false,true,3600,${q(e.title)},${j(input)},${j({type:'object',properties:{},required:[]})},${j(e.response)},${j({domain:e.sample.data.domain})},${j(e.sample)},${a(tags)},${a(tags)},${a(sectors)},${a([e.title,'Supplier payment check'])},ARRAY['domain']::text[],ARRAY['data','notice','coverage','source_url']::text[],'security','lookup',${q(docs)},${q(e.title+'\n\n'+description)},'provider_documentation_and_live_probe',1,${q(e.origin)},${q(e.title)},true WHERE NOT EXISTS(SELECT 1 FROM providers.api_endpoints WHERE api_id=source_id AND method='GET' AND path=${q(e.path)});`);
sql.push('END $onboard$;');
sql.push(`INSERT INTO catalog.fact_subject_kinds(slug,description) VALUES('dataset','Source query or result') ON CONFLICT DO NOTHING;`);
sql.push(`INSERT INTO catalog.fact_types(slug,name,description,subject_kind,jurisdiction,value_schema,status,freshness_seconds) VALUES(${q(domainFact.slug)},${q(domainFact.name)},${q(domainFact.description)},'dataset',NULL,${j(domainFact.value_schema)},'active',3600) ON CONFLICT(slug) DO NOTHING;`);
const services = [];
for (const e of endpoints) {
  const contract = {capability_slug:e.capability,version:1,requires:[{fact_type:'domain.name',subject_role:'subject',optional:false}],input_map:{'domain.name':{in:'path',name:'domain'}},output_map:{base:'$',produces:{[e.result]:{path:'$'}}},cardinality:'one',identifies:null,max_fan_out:1};
  sql.push(`INSERT INTO catalog.fact_types(slug,name,description,subject_kind,jurisdiction,value_schema,status,freshness_seconds) VALUES(${q(e.result)},${q(e.result.replaceAll('.',' '))},${q(e.title)},'dataset',NULL,${j({...e.response,description:e.title})},'active',3600) ON CONFLICT(slug) DO NOTHING;`);
  sql.push(`INSERT INTO catalog.capabilities(slug,name,description,category,input_contract,output_contract,status) VALUES(${q(e.capability)},${q(e.title)},${q(e.title+'; '+description)},'security',${j(input)},${j(e.response)},'active') ON CONFLICT(slug) DO NOTHING;`);
  sql.push(`INSERT INTO catalog.capability_execution_contracts(id,capability_slug,endpoint_id,version,status,requires,produces,input_map,fixed_params,output_map,cardinality,max_fan_out,preconditions,pagination,effect,price_model,freshness_seconds,sample_payload,validated_at,validated_note,notes,adapter_version)
SELECT gen_random_uuid(),${q(e.capability)},e.id,1,'validated',${j(contract.requires)},${j([{fact_type:e.result,subject_role:'subject',cardinality:'one'}])},${j(contract.input_map)},'{}'::jsonb,${j(contract.output_map)},'one',1,'{}','{}','read','per_call',3600,${j(e.sample)},now(),'Real registry/DNS fixtures and Rust adapter tests; activate after production verification','domain-intelligence-v1','domain-intelligence-v1' FROM providers.api_endpoints e JOIN providers.apis s ON s.id=e.api_id WHERE s.slug=${q(slug)} AND e.method='GET' AND e.path=${q(e.path)} AND NOT EXISTS(SELECT 1 FROM catalog.capability_execution_contracts c WHERE c.endpoint_id=e.id AND c.capability_slug=${q(e.capability)} AND c.version=1);`);
  services.push({id:id(e.capability),endpoint_id:id(e.path),api_slug:slug,name,method:'GET',path:e.path,listing_type:'api',list_atomic:listAtomic,source_url:docs,contract,fixed_params:{},preconditions:{},pagination:{},freshness_seconds:3600,sample_payload:e.sample,jurisdiction:null});
}
sql.push('COMMIT;', '');
const out = process.argv[2];
if (!out) throw Error('Pass the migration path');
await fs.writeFile(new URL(out, root), sql.join('\n\n'));
await fs.mkdir(new URL('tests/fixtures/domain-intelligence/', root), {recursive:true});
await fs.writeFile(new URL('tests/fixtures/domain-intelligence/services.json', root), JSON.stringify({services,vocabulary:[{slug:'domain.name',value_schema:domainFact.value_schema},...endpoints.map(e=>({slug:e.result,value_schema:{...e.response,description:e.title}}))]}, null, 2) + '\n');
console.log(`${endpoints.length} endpoints, ${services.length} contracts staged; ${listAtomic} list micro-USD per call.`);
