// Operator acceptance: one explicit read call, no retries or buyer charges.
// Uses the approved Vault connection; never prints or persists credentials.
import fs from 'node:fs/promises';
import {execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {tools as reviewed} from './crunchbase-mcp-catalog.mjs';
const [name,input='{}']=process.argv.slice(2);
if(![...reviewed.map(t=>t.tool),'cb_metering_get_state','--adapter-reference'].includes(name))throw Error('An explicit reviewed read tool is required');
const args=JSON.parse(input);
if(!args||Array.isArray(args)||typeof args!=='object')throw Error('Arguments must be an object');
let database;
try{database=execFileSync('fly',['ssh','console','-a','apiosk-gateway-v2','-q','-C',`sh -c 'printf "%s" "$EXECUTOR_DATABASE_URL"'`],{encoding:'utf8',stdio:['ignore','pipe','pipe']}).trim();}
catch{throw Error('Could not read the authorized database configuration');}
const url=new URL(database);
if(url.hostname!=='db.jgjoiyqdyypouskftzeq.supabase.co')throw Error('Unexpected project database');
if(name==='--adapter-reference'){
 url.hostname='aws-1-eu-west-1.pooler.supabase.com';url.port='5432';url.username='postgres.jgjoiyqdyypouskftzeq';url.pathname='/postgres';
 try{execFileSync('cargo',['test','--offline','--bin','apiosk-gateway','proxy::crunchbase::tests::live_reference_uses_vault_and_the_real_adapter','--','--ignored','--exact'],{
  cwd:fileURLToPath(new URL('../legacy-executor',import.meta.url)),env:{...process.env,APIOSK_CRUNCHBASE_ACCEPTANCE_DATABASE_URL:url.href},stdio:['ignore','pipe','pipe']});}
 catch(error){
  const log=String(error.stdout||'')+String(error.stderr||'');
  const stage=log.match(/operator database connection failed: (io|tls|database|pool_timeout|configuration)/)?.[0]||['Vault credentials available','official adapter reference succeeds','could not compile'].find(s=>log.includes(s))||'test process';
  throw Error(`Live adapter reference failed at ${stage}; credentials were not logged`);
 }
 console.log('PASS: real adapter -> Vault -> official Crunchbase reference; no Jina or buyer charge.');
 process.exit(0);
}
let token;
try{token=JSON.parse(execFileSync('psql',['-X','-t','-A','-v','ON_ERROR_STOP=1','-c',"SELECT decrypted_secret FROM vault.decrypted_secrets WHERE name='CRUNCHBASE_MCP_OAUTH'"],{
 env:{...process.env,PGHOST:'aws-1-eu-west-1.pooler.supabase.com',PGPORT:'5432',PGDATABASE:'postgres',PGUSER:'postgres.jgjoiyqdyypouskftzeq',PGPASSWORD:decodeURIComponent(url.password),PGSSLMODE:'verify-full',PGSSLROOTCERT:fileURLToPath(new URL('../deploy/certs/supabase-prod-ca-2021.crt',import.meta.url)),PGCONNECT_TIMEOUT:'15'},
 encoding:'utf8',stdio:['ignore','pipe','pipe']}));}
catch{throw Error('Could not read the approved Vault account');}
if(token.expires_at<Date.now()/1000+90)throw Error('Access token needs a server-side refresh before acceptance');
let session,protocol='2025-11-25',id=0;
async function rpc(method,params,notification=false){
 const requestId=notification?undefined:++id;
 const response=await fetch('https://mcp.crunchbase.com',{method:'POST',redirect:'error',
 headers:{Authorization:`Bearer ${token.access_token}`,'Content-Type':'application/json',Accept:'application/json, text/event-stream','MCP-Protocol-Version':protocol,...(session?{'Mcp-Session-Id':session}:{})},
 body:JSON.stringify({jsonrpc:'2.0',...(notification?{}:{id:requestId}),method,...(params?{params}:{})}),signal:AbortSignal.timeout(45000)});
 if(!response.ok)throw Error(`Crunchbase HTTP ${response.status}`);
 session=response.headers.get('Mcp-Session-Id')||session;
 if(notification){await response.body?.cancel();return;}
 const streaming=response.headers.get('content-type')?.startsWith('text/event-stream');
 let text='',size=0;const decoder=new TextDecoder();
 for await(const bytes of response.body){
  size+=bytes.length;if(size>4*1024*1024)throw Error('Reply too large');
  text+=decoder.decode(bytes,{stream:true});
  const events=streaming?text.replaceAll('\r\n','\n').split('\n\n').slice(0,-1).map(e=>e.split('\n').filter(l=>l.startsWith('data:')).map(l=>l.slice(5).trimStart()).join('\n')):[text];
  for(const event of events){let data;try{data=JSON.parse(event);}catch{continue;}
   if(data.jsonrpc!=='2.0'||data.id!==requestId)continue;
   if(data.error)throw Error('Crunchbase RPC error');
   return data.result;
  }
 }
 throw Error('Incomplete response; not retried');
}
try{
 const init=await rpc('initialize',{protocolVersion:protocol,capabilities:{},clientInfo:{name:'Apiosk acceptance',version:'1.0'}});
 if(!['2025-11-25','2025-06-18','2025-03-26'].includes(init.protocolVersion))throw Error('Unsupported protocol');
 protocol=init.protocolVersion;await rpc('notifications/initialized',undefined,true);
 const result=await rpc('tools/call',{name,arguments:args});
 const data=result.structuredContent??JSON.parse(result.content.filter(b=>b.type==='text').map(b=>b.text).join('\n'));
 const record={at:new Date().toISOString(),name,arguments:args,result,data,buyer_charged:0};
 const filename=`${Date.now()}-${name}.json`;
 await fs.writeFile(new URL('../output/crunchbase-mcp/'+filename,import.meta.url),JSON.stringify(record,null,2)+'\n',{mode:0o600,flag:'wx'});
 const failed=result.isError===true||data.error!=null||(data.response_status??200)>=400;
 console.log(JSON.stringify({name,file:'output/crunchbase-mcp/'+filename,success:!failed,buyer_charged:0,
  ...(failed?{error:data.error}:{}),...(name==='cb_metering_get_state'?{meters:data.meters}:{})}));
 if(failed)process.exitCode=1;
}catch(error){console.error(error.message);process.exitCode=1;}
