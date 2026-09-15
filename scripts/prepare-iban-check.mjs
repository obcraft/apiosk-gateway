// First-party IBAN check (structure, mod-97, SEPA, Dutch bank directory),
// computed by the executor without an upstream call. List EUR 0.05.
// Usage: node scripts/prepare-iban-check.mjs supabase/migrations/<version>_onboard_iban_check.sql
import fs from 'node:fs/promises';
const root = new URL('../', import.meta.url);
const slug = 'iban-check', name = 'IBAN Check';
const description = 'Validate an IBAN: country and length, ISO 7064 mod-97 check digits, SEPA membership and the bank and BIC behind Dutch bank codes. Does not confirm that the account exists or who holds it.';
const docs = 'https://www.swift.com/standards/data-standards/iban-international-bank-account-number';
const price = 0.054348, listAtomic = 54348; // EUR 0.05 at the stored 0.92 rate
const tags = ['IBAN', 'bankrekening', 'BIC', 'SWIFT', 'SEPA', 'IBAN check', 'IBAN validatie', 'supplier check', 'leveranciercontrole'];
const sectors = ['Finance', 'Company data'];
const sample = {data:{iban:'NL91ABNA0417164300',valid:true,country_code:'NL',check_digits:'91',bank_code:'ABNA',bank:{name:'ABN AMRO Bank',bic:'ABNANL2A'},sepa:true,checks:{country_known:true,length:true,checksum:true}},
  notice:'Structure and check digits only. Does not confirm that the account exists or who holds it (that needs a Verification of Payee / IBAN-Name Check).',
  coverage:{complete:true,scope:'IBAN structure, ISO 7064 mod-97 check digits, SEPA membership and Dutch bank-code directory'},
  source_url:'https://www.swift.com/standards/data-standards/iban-international-bank-account-number'};
const ibanFact = {slug:'bank.iban',name:'bank iban',description:'IBAN without spaces, upper case',value_schema:{type:'string',minLength:15,maxLength:34,pattern:'^[A-Z]{2}[0-9]{2}[A-Z0-9]{11,30}$',description:'IBAN without spaces, upper case'}};
const response = {type:'object',required:['data','notice','coverage','source_url'],properties:{data:{type:'object',required:['iban','valid','country_code','check_digits','sepa','checks'],properties:{iban:{type:'string'},valid:{type:'boolean'},country_code:{type:'string'},check_digits:{type:'string'},bank_code:{type:['string','null']},bank:{type:['object','null']},sepa:{type:'boolean'},checks:{type:'object'}}},notice:{type:'string'},coverage:{type:'object'},source_url:{type:'string'}}};
const input = {type:'object',required:['iban'],properties:{iban:{...ibanFact.value_schema,'x-parameter-location':'path'}}};
const q = v => v == null ? 'NULL' : `'${String(v).replaceAll("'", "''")}'`, j = v => `${q(JSON.stringify(v))}::jsonb`, a = v => `ARRAY[${v.map(q).join(',')}]::text[]`;
const metadata = {tags,sectors,adapter:'iban-check-v1',capability_adapter:'iban-check-v1',catalog_state:'staged',coverage_notice:description,
  billing:{mode:'apiosk_balance',target_list_eur:0.05,list_micro_usd:listAtomic,conversion_rate:0.92,conversion_date:'2026-08-21'},
  operator_notice:'Computed by Apiosk; no upstream provider.',upstream_cost_eur:0};
const contract = {requires:[{fact_type:'bank.iban',subject_role:'subject',optional:false}],input_map:{'bank.iban':{in:'path',name:'iban'}},output_map:{base:'$',produces:{'iban.validate.result':{path:'$'}}}};
const sql = [
  '-- First-party IBAN check v1, computed in the executor (no upstream). Staged until the executor with the adapter is deployed.',
  `-- List USD ${price} (EUR 0.05 at 0.92); buyer total list + 15%, or + 20% inside workflows.`,
  'BEGIN;', "SELECT pg_advisory_xact_lock(hashtext('apiosk-iban-check-v1'));",
  'DO $onboard$ DECLARE owner uuid; source_id uuid; BEGIN',
  "SELECT owner_id INTO STRICT owner FROM providers.apis WHERE slug='koop' AND is_platform_owned AND settlement_mode='apiosk_revenue';",
  `INSERT INTO providers.apis(id,owner_id,slug,name,status,description,long_description,category,docs_url,website_url,origin_url,endpoint_url,authentication_info,is_verified,active,discoverable,listing_type,listing_metadata,integration_mode,settlement_mode,is_external,is_platform_owned,is_community,source,requires_payout_wallet) VALUES(gen_random_uuid(),owner,${q(slug)},${q(name)},'inactive',${q(description)},${q(description)},'finance',${q(docs)},${q(docs)},'https://iban-check.apiosk.com','https://iban-check.apiosk.com','{"type":"none","caller":"Apiosk balance approval required"}',false,false,false,'api',${j(metadata)},'gateway','apiosk_revenue',false,true,false,'manual',false) ON CONFLICT(slug) DO NOTHING;`,
  `SELECT id INTO STRICT source_id FROM providers.apis WHERE slug=${q(slug)} AND owner_id=owner;`,
  `INSERT INTO providers.api_endpoints(id,api_id,method,path,price,active,payment_required,cache_ttl_seconds,description,query_params,request_body,response_body,request_example,response_example,tags,search_terms,sectors,use_cases,input_entities,output_entities,endpoint_category,action_type,external_docs_url,documentation_markdown,metadata_source,metadata_confidence,task_summary,contract_documented)
SELECT gen_random_uuid(),source_id,'GET','/iban/{iban}',${price},false,true,0,'Validate an IBAN and identify the bank',${j(input)},${j({type:'object',properties:{},required:[]})},${j(response)},${j({iban:'NL91ABNA0417164300'})},${j(sample)},${a(tags)},${a(tags)},${a(sectors)},${a(['Validate an IBAN','Supplier payment check'])},ARRAY['iban']::text[],ARRAY['data','notice','coverage','source_url']::text[],'finance','lookup',${q(docs)},${q(description)},'first_party_computation',1,'Validate an IBAN and identify the bank',true WHERE NOT EXISTS(SELECT 1 FROM providers.api_endpoints WHERE api_id=source_id AND method='GET' AND path='/iban/{iban}');`,
  'END $onboard$;',
  `INSERT INTO catalog.fact_subject_kinds(slug,description) VALUES('dataset','Source query or result') ON CONFLICT DO NOTHING;`,
  `INSERT INTO catalog.fact_types(slug,name,description,subject_kind,jurisdiction,value_schema,status,freshness_seconds) VALUES(${q(ibanFact.slug)},${q(ibanFact.name)},${q(ibanFact.description)},'dataset',NULL,${j(ibanFact.value_schema)},'active',3600) ON CONFLICT(slug) DO NOTHING;`,
  `INSERT INTO catalog.fact_types(slug,name,description,subject_kind,jurisdiction,value_schema,status,freshness_seconds) VALUES('iban.validate.result','iban validate result','IBAN check result','dataset',NULL,${j(response)},'active',3600) ON CONFLICT(slug) DO NOTHING;`,
  `INSERT INTO catalog.capabilities(slug,name,description,category,input_contract,output_contract,status) VALUES('iban.validate','Validate an IBAN and identify the bank',${q(description)},'finance',${j(input)},${j(response)},'active') ON CONFLICT(slug) DO NOTHING;`,
  `INSERT INTO catalog.capability_execution_contracts(id,capability_slug,endpoint_id,version,status,requires,produces,input_map,fixed_params,output_map,cardinality,max_fan_out,preconditions,pagination,effect,price_model,freshness_seconds,sample_payload,validated_at,validated_note,notes,adapter_version)
SELECT gen_random_uuid(),'iban.validate',e.id,1,'validated',${j(contract.requires)},${j([{fact_type:'iban.validate.result',subject_role:'subject',cardinality:'one'}])},${j(contract.input_map)},'{}'::jsonb,${j(contract.output_map)},'one',1,'{}','{}','read','per_call',3600,${j(sample)},now(),'Executor-computed; Rust adapter tests','iban-check-v1','iban-check-v1' FROM providers.api_endpoints e JOIN providers.apis s ON s.id=e.api_id WHERE s.slug=${q(slug)} AND e.method='GET' AND e.path='/iban/{iban}' AND NOT EXISTS(SELECT 1 FROM catalog.capability_execution_contracts c WHERE c.endpoint_id=e.id AND c.capability_slug='iban.validate' AND c.version=1);`,
  'COMMIT;', '',
];
const publish = [
  '-- Publish the IBAN check after the executor with iban_check.rs is deployed and a live check passes.',
  'BEGIN;', "SELECT pg_advisory_xact_lock(hashtext('apiosk-iban-check-v1'));",
  'DO $publish$ DECLARE source_id uuid; BEGIN',
  "SELECT id INTO STRICT source_id FROM providers.apis WHERE slug='iban-check' AND is_platform_owned AND settlement_mode='apiosk_revenue' AND listing_metadata->>'capability_adapter'='iban-check-v1' AND suspended_at IS NULL FOR UPDATE;",
  "IF (SELECT count(*) FROM catalog.capability_execution_contracts c JOIN providers.api_endpoints e ON e.id=c.endpoint_id WHERE e.api_id=source_id AND e.price=0.054348 AND c.adapter_version='iban-check-v1' AND c.status IN ('validated','active') AND c.sample_payload IS NOT NULL)<>1 THEN RAISE EXCEPTION 'iban-check publication invariant failed'; END IF;",
  "UPDATE providers.api_endpoints SET active=true WHERE api_id=source_id AND path='/iban/{iban}';",
  "UPDATE catalog.capability_execution_contracts SET status='active',validated_note='Executor-computed; Rust adapter tests and production check' WHERE adapter_version='iban-check-v1' AND status='validated';",
  "PERFORM set_config('apiosk.api_review_approved_id',source_id::text,true);",
  `UPDATE providers.apis SET status='active',active=true,is_verified=true,discoverable=true,verified_at=coalesce(verified_at,now()),approved_at=coalesce(approved_at,now()),listing_metadata=coalesce(listing_metadata,'{}'::jsonb)||'{"catalog_state":"published"}'::jsonb WHERE id=source_id;`,
  "PERFORM set_config('apiosk.api_review_approved_id','',true);",
  "IF NOT EXISTS(SELECT 1 FROM providers.apis WHERE id=source_id AND status='active' AND active AND is_verified AND discoverable) THEN RAISE EXCEPTION 'iban-check did not become publicly active'; END IF;",
  'END $publish$;', 'COMMIT;', '',
];
const [onboardPath, publishPath] = process.argv.slice(2);
if (!onboardPath || !publishPath) throw Error('Pass onboarding and publish migration paths');
await fs.writeFile(new URL(onboardPath, root), sql.join('\n\n'));
await fs.writeFile(new URL(publishPath, root), publish.join('\n\n'));
await fs.mkdir(new URL('tests/fixtures/iban-check/', root), {recursive:true});
await fs.writeFile(new URL('tests/fixtures/iban-check/services.json', root), JSON.stringify({services:[{id:'5e6b1d0e-1c7a-4d2b-8f3e-9a1b2c3d4e5f',endpoint_id:'6f7c2e1f-2d8b-4e3c-9a4f-0b1c2d3e4f50',api_slug:slug,name,method:'GET',path:'/iban/{iban}',listing_type:'api',list_atomic:listAtomic,source_url:docs,contract:{capability_slug:'iban.validate',version:1,...contract,cardinality:'one',identifies:null,max_fan_out:1},fixed_params:{},preconditions:{},pagination:{},freshness_seconds:3600,sample_payload:sample,jurisdiction:null}],vocabulary:[{slug:'bank.iban',value_schema:ibanFact.value_schema},{slug:'iban.validate.result',value_schema:response}]}, null, 2) + '\n');
console.log('iban-check staged: 1 endpoint, 1 contract, list 54348 micro-USD');
