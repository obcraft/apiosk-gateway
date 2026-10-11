// Installs the locally approved account in server-side Vault. No tokens appear
// in command arguments, logs, browser state or catalog metadata.
import fs from 'node:fs/promises';
import {execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
const secretFile=new URL('../.local/crunchbase/oauth.json',import.meta.url);
if(process.argv.slice(2).some(arg=>arg!=='--check'))throw Error('Only --check is supported');
const check=process.argv.includes('--check');
const token=check?null:JSON.parse(await fs.readFile(secretFile,'utf8'));
if(!check&&(!token.access_token||!token.refresh_token||!token.client_id||token.expires_at<Date.now()/1000+90))throw Error('Valid local OAuth connection required');
let database=process.env.APIOSK_CRUNCHBASE_DATABASE_URL;
if(!database){
 try {database=execFileSync('fly',['ssh','console','-a','apiosk-gateway-v2','-q','-C',`sh -c 'printf "%s" "$EXECUTOR_DATABASE_URL"'`],{encoding:'utf8',stdio:['ignore','pipe','pipe']}).trim();}
 catch {throw Error('Could not read the authorized Gateway database configuration');}
}
const url=new URL(database);
if(!['postgres:','postgresql:'].includes(url.protocol)||!(url.hostname==='db.jgjoiyqdyypouskftzeq.supabase.co'||(url.hostname.endsWith('.pooler.supabase.com')&&url.username.endsWith('.jgjoiyqdyypouskftzeq'))))throw Error('Unexpected production database host');
// The local operator uses the project's verified IPv4 session pooler. The
// embedded executor keeps its existing direct database connection unchanged.
if(url.hostname==='db.jgjoiyqdyypouskftzeq.supabase.co'){
 url.hostname='aws-1-eu-west-1.pooler.supabase.com';url.port='5432';url.username+='.'+'jgjoiyqdyypouskftzeq';
}
const rootCert=fileURLToPath(new URL('../deploy/certs/supabase-prod-ca-2021.crt',import.meta.url));
const env={...process.env,PGHOST:url.hostname,PGPORT:url.port||'5432',PGDATABASE:url.pathname.slice(1)||'postgres',PGUSER:decodeURIComponent(url.username),PGPASSWORD:decodeURIComponent(url.password),PGSSLMODE:'verify-full',PGSSLROOTCERT:rootCert,PGCONNECT_TIMEOUT:'15'};
// psql 16+ extended-query binding keeps credentials out of SQL statement text.
const body=Buffer.from(check?'connection-check':JSON.stringify(token)).toString('base64');
const sql=check?`SELECT has_function_privilege(current_user,'vault.create_secret(text,text,text,uuid)','EXECUTE'), convert_from(decode($1,'base64'),'UTF8')
\\bind '${body}'
\\g
`:`BEGIN;
SET LOCAL statement_timeout='15s';
SET LOCAL standard_conforming_strings=on;
SELECT pg_advisory_xact_lock(hashtext('apiosk-crunchbase-oauth'));
DO $install$ BEGIN
 IF EXISTS(SELECT 1 FROM vault.secrets WHERE name='CRUNCHBASE_MCP_OAUTH') THEN RAISE EXCEPTION 'Existing Crunchbase account must be explicitly disconnected before replacement'; END IF;
END $install$;
SELECT vault.create_secret(convert_from(decode($1,'base64'),'UTF8'),'CRUNCHBASE_MCP_OAUTH','Official Crunchbase MCP OAuth account; server only')
\\bind '${body}'
\\g
COMMIT;`;
try{execFileSync('psql',['-X','-v','ON_ERROR_STOP=1','-q'],{env,input:sql,encoding:'utf8',stdio:['pipe','pipe','pipe']});}
catch{throw Error('Vault installation failed. No credentials were logged. Check connectivity and server-role Vault permissions.');}
if(check)console.log('Verified TLS database connection and parameter binding; no secret read or written.');
else{
 await fs.unlink(secretFile);
 console.log('Crunchbase OAuth stored in Vault; local credential file removed. Endpoints remain inactive pending acceptance.');
}
