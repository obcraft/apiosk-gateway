// Render reviewed ECB-only source staging. No network or database writes.
import fs from 'node:fs/promises';
const root=new URL('../',import.meta.url);
const sample=JSON.parse(await fs.readFile(new URL('tests/fixtures/frankfurter/eur-usd.json',root),'utf8'));
const rates=JSON.parse(await fs.readFile(new URL('tests/fixtures/frankfurter/eur-currencies.json',root),'utf8'));
const currencies=[...new Set(rates.map(r=>r.quote))].sort();
if(sample.base!=='EUR'||sample.quote!=='USD'||!(sample.rate>0)||!currencies.includes('USD')||rates.some(r=>r.base!=='EUR'))throw Error('Unexpected ECB source response');
const endpointId='6ff3c2cf-19dc-4555-9051-e1c80c6c6e67',contractId='c9b32ac9-4c79-4c82-bb61-16619a7d7526';
const description='Latest daily ECB euro foreign-exchange reference rate for one target currency, via Frankfurter. EUR base; includes observation date. ECB data is free at ecb.europa.eu; Apiosk charges for retrieval. Not intraday, historical rates, Euribor or trading quotes.';
const currency={type:'string',enum:currencies,description:'Target currency from the verified ECB daily euro reference currency set.'};
const response={type:'object',required:['date','base','quote','rate'],properties:{date:{type:'string',pattern:'^[0-9]{4}-[0-9]{2}-[0-9]{2}$'},base:{type:'string',enum:['EUR']},quote:currency,rate:{type:'number',minimum:0.000000001}}};
const inputs={type:'object',required:['quote'],properties:{quote:{...currency,'x-parameter-location':'path'}}};
const fixture={known_sources:[{slug:'frankfurter',name:'Frankfurter',metadata:{coverage_notice:description}}],vocabulary:[{slug:'fx.ecb.quote',value_schema:currency},{slug:'fx.ecb.reference_rate',value_schema:response}],services:[{id:contractId,endpoint_id:endpointId,api_slug:'frankfurter',name:'Frankfurter',description,source_url:'https://www.ecb.europa.eu/stats/policy_and_exchange_rates/euro_reference_exchange_rates/html/index.en.html',method:'GET',path:'/v2/providers/ecb/rate/EUR/{quote}',listing_type:'api',list_atomic:50000,fixed_params:{},preconditions:{},pagination:{},freshness_seconds:3600,jurisdiction:null,sample_payload:sample,contract:{capability_slug:'fx.ecb-reference-rate',version:1,requires:[{fact_type:'fx.ecb.quote',subject_role:'subject',optional:false}],input_map:{'fx.ecb.quote':{in:'path',name:'quote'}},output_map:{base:'$',produces:{'fx.ecb.reference_rate':{path:'$'}}},cardinality:'one',identifies:null,max_fan_out:1}}]};
await fs.writeFile(new URL('tests/fixtures/frankfurter/catalog.json',root),JSON.stringify(fixture,null,2)+'\n');
const s=fixture.services[0],c=s.contract,q=x=>`'${String(x).replaceAll("'","''")}'`,j=x=>q(JSON.stringify(x))+'::jsonb';
const sql=`-- Reviewed local staging only. Does not unhide Frankfurter, activate the endpoint,
-- publish the capability contract, or remove any source block.
-- Release requires showing ECB attribution and free-source notice before payment
-- and on each saved-result access; terms: https://www.ecb.europa.eu/services/using-our-site/disclaimer/html/index.en.html
-- The v2 provider-specific route avoids Frankfurter's default blended rates.
-- Existing Frankfurter tariff retained: USD0.05 list, USD0.0575 standard buyer total.
BEGIN;
SET LOCAL lock_timeout='5s';
SET LOCAL statement_timeout='20s';
SELECT pg_advisory_xact_lock(hashtext('frankfurter-ecb-reference-v1'));
DO $guard$ BEGIN
 IF NOT EXISTS (SELECT 1 FROM providers.apis a JOIN providers.api_endpoints e ON e.api_id=a.id
   WHERE a.id='0e2ef2d1-1fc0-4a5b-a88d-eec3f629a067' AND a.slug='frankfurter'
   AND a.origin_url='https://api.frankfurter.dev/v1' AND a.status='active' AND a.active
   AND a.discoverable=false AND a.suspended_at IS NULL
   AND a.listing_metadata->>'planner_block_reason' IS NULL
   AND e.id='be7a01a6-a21b-4228-aa66-fa4a1e64d710' AND e.path='/latest' AND e.method='GET' AND e.price=0.05)
 THEN RAISE EXCEPTION 'Frankfurter listing or tariff drift; review before staging'; END IF;
END $guard$;
INSERT INTO catalog.fact_subject_kinds(slug,description) VALUES('dataset','Source query or result') ON CONFLICT DO NOTHING;
${fixture.vocabulary.map(v=>`INSERT INTO catalog.fact_types(slug,name,description,subject_kind,value_schema,status,freshness_seconds) VALUES(${q(v.slug)},${q(v.slug)},${q(description)},'dataset',${j(v.value_schema)},'active',3600) ON CONFLICT(slug) DO NOTHING;`).join('\n')}
INSERT INTO providers.api_endpoints(id,api_id,method,path,price,active,payment_required,cache_ttl_seconds,description,query_params,request_body,response_body,request_example,response_example,tags,search_terms,endpoint_category,action_type,external_docs_url,documentation_markdown,metadata_source,metadata_confidence,origin_override,auth_override,task_summary,contract_documented)
VALUES(${q(endpointId)}::uuid,'0e2ef2d1-1fc0-4a5b-a88d-eec3f629a067','GET',${q(s.path)},0.05,false,true,3600,${q(description)},${j(inputs)},'{}'::jsonb,${j(response)},'{"quote":"USD"}'::jsonb,${j(sample)},ARRAY['ECB','FX','exchange rate','wisselkoers','EUR'],ARRAY['ECB','Frankfurter','foreign exchange','currency','wisselkoers'],'finance','lookup','https://frankfurter.dev/',${q(description)},'official_documentation',1,'https://api.frankfurter.dev','{}'::jsonb,${q(description)},true)
ON CONFLICT(id) DO NOTHING;
INSERT INTO catalog.capabilities(slug,name,description,category,input_contract,output_contract,status)
VALUES('fx.ecb-reference-rate','ECB daily euro reference exchange rate',${q(description)},'finance',${j(inputs)},${j(response)},'active') ON CONFLICT(slug) DO NOTHING;
INSERT INTO catalog.capability_execution_contracts(id,capability_slug,endpoint_id,version,status,requires,produces,input_map,fixed_params,output_map,cardinality,identifies,max_fan_out,preconditions,pagination,effect,price_model,freshness_seconds,sample_payload,validated_at,validated_note,notes,adapter_version)
VALUES(${q(contractId)}::uuid,'fx.ecb-reference-rate',${q(endpointId)}::uuid,1,'validated',${j(c.requires)},${j([{fact_type:'fx.ecb.reference_rate',subject_role:'subject',cardinality:'one'}])},${j(c.input_map)},'{}'::jsonb,${j(c.output_map)},'one',NULL,1,'{}'::jsonb,'{}'::jsonb,'read','per_call',3600,${j(sample)},now(),'Public provider-specific ECB route HTTP200; observed date/base/quote/rate; local runtime admission, extraction and exact input/price tests. No Apiosk paid E2E run.', 'Staged only. Keep listing hidden and endpoint inactive until ECB attribution/free-source disclosure and buyer-flow validation are verified.','frankfurter-ecb-v1')
ON CONFLICT(id) DO NOTHING;
COMMIT;
`;
for (const path of ['supabase/migrations/', '../app/supabase/migrations/']) {
  await fs.writeFile(new URL(path+'20261002084852_frankfurter_ecb_reference_rates_staged_prod_only.sql',root),sql);
}
console.log(JSON.stringify({capability:c.capability_slug,endpoint:s.path,origin_override:'https://api.frankfurter.dev',currencies:currencies.length,list_atomic:s.list_atomic,standard_buyer_atomic:57500,production_published:false}));
