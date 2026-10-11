// Operator-only OAuth PKCE handoff. No provider data calls or subscriptions.
// The browser receives only the authorization URL. Tokens stay in a 0600 file
// under ignored .local until installed in Vault; never print them or commit them.
import http from 'node:http';
import crypto from 'node:crypto';
import fs from 'node:fs/promises';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
const directory=path.resolve(fileURLToPath(new URL('../.local/crunchbase/',import.meta.url)));
const redirect='http://127.0.0.1:18743/callback';
const clientId=process.env.CRUNCHBASE_CLIENT_ID || 'https://apiosk-gateway-v2.fly.dev/.well-known/crunchbase-client.json';
const clientSecret=process.env.CRUNCHBASE_CLIENT_SECRET;
const verifier=crypto.randomBytes(48).toString('base64url');
const state=crypto.randomBytes(32).toString('base64url');
const challenge=crypto.createHash('sha256').update(verifier).digest('base64url');
const url=new URL('https://www.crunchbase.com/oauth/authorize');
url.search=new URLSearchParams({response_type:'code',client_id:clientId,redirect_uri:redirect,
  code_challenge:challenge,code_challenge_method:'S256',state,scope:'offline_access',resource:'https://mcp.crunchbase.com'}).toString();
await fs.mkdir(directory,{recursive:true,mode:0o700});
await fs.chmod(directory,0o700);
const server=http.createServer(async (req,res)=>{
  res.setHeader('Cache-Control','no-store'); res.setHeader('Referrer-Policy','no-referrer');
  res.setHeader('Content-Type','text/plain; charset=utf-8');
  const request=new URL(req.url,redirect);
  if(req.method!=='GET'||req.headers.host!=='127.0.0.1:18743'||request.pathname!=='/callback') {res.writeHead(404).end('Not found');return;}
  if(request.searchParams.get('state')!==state) {res.writeHead(400).end('Invalid OAuth state');return;}
  if(request.searchParams.has('error')) {res.writeHead(400).end('Crunchbase connection was not authorized.');console.error('Crunchbase authorization declined.');server.close();return;}
  const code=request.searchParams.get('code'); if(!code){res.writeHead(400).end('Missing authorization code');return;}
  // Consume the callback once, before contacting the token server.
  server.removeAllListeners('request');
  server.on('request',(_request,response)=>response.writeHead(409).end('Connection is being completed.'));
  try {
    const body=new URLSearchParams({grant_type:'authorization_code',code,redirect_uri:redirect,client_id:clientId,code_verifier:verifier,resource:'https://mcp.crunchbase.com'});
    if(clientSecret)body.set('client_secret',clientSecret);
    const response=await fetch('https://oauth.crunchbase.com/token',{method:'POST',body,redirect:'error',signal:AbortSignal.timeout(20000)});
    if(!response.ok)throw Error('Token exchange failed; reconnect to retry.');
    const token=await response.json();
    if(!token.access_token||!token.refresh_token||String(token.token_type).toLowerCase()!=='bearer'||!(Number(token.expires_in)>90))throw Error('Incomplete OAuth token response.');
    const bundle={client_id:clientId,...(clientSecret?{client_secret:clientSecret}:{}),access_token:token.access_token,refresh_token:token.refresh_token,
      expires_at:Math.floor(Date.now()/1000)+Number(token.expires_in),scope:token.scope||'offline_access'};
    // Exclusive creation prevents accidentally replacing another account.
    await fs.writeFile(path.join(directory,'oauth.json'),JSON.stringify(bundle),{mode:0o600,flag:'wx'});
    res.end('Crunchbase is connected to the local Apiosk setup. You can return to Codex.');
    console.log('OAuth completed. Encrypted Vault installation and read-tool discovery are next; no company data was queried.');
  } catch {res.writeHead(502).end('Connection could not be completed. Return to Codex.');console.error('OAuth failed; no credentials were printed. Start a new connection to retry.');}
  server.close(); clearTimeout(expiry);
});
const expiry=setTimeout(()=>{console.error('OAuth handoff expired; start again.');server.close();},15*60*1000);
server.listen(18743,'127.0.0.1',async()=>{
  await fs.writeFile(path.join(directory,'authorize-url.txt'),url.href,{mode:0o600});
  console.log('OAuth handoff ready at .local/crunchbase/authorize-url.txt (15 minutes).');
});
