// Authenticated metadata discovery only: initialize, initialized, tools/list.
// Never calls a data tool, stores provider credentials, or activates endpoints.
import fs from 'node:fs/promises';
import {tools as reviewed} from './crunchbase-mcp-catalog.mjs';
const token=JSON.parse(await fs.readFile(new URL('../.local/crunchbase/oauth.json',import.meta.url),'utf8'));
if(token.expires_at<=Date.now()/1000+60)throw Error('OAuth expired; reconnect before discovery.');
let session,protocol='2025-11-25',id=0;
async function rpc(method,params,notification=false){
 const requestId=notification?undefined:++id;
 const response=await fetch('https://mcp.crunchbase.com',{method:'POST',redirect:'error',
  headers:{Authorization:`Bearer ${token.access_token}`,'Content-Type':'application/json',Accept:'application/json, text/event-stream','MCP-Protocol-Version':protocol,...(session?{'Mcp-Session-Id':session}:{})},
  body:JSON.stringify({jsonrpc:'2.0',...(notification?{}:{id:requestId}),method,...(params?{params}:{})}),signal:AbortSignal.timeout(30000)});
 if(!response.ok)throw Error(`Crunchbase metadata HTTP ${response.status}`);
 session=response.headers.get('Mcp-Session-Id')||session;
 if(notification){await response.body?.cancel();return;}
 const streaming=response.headers.get('content-type')?.startsWith('text/event-stream');
 let text='',size=0;const decoder=new TextDecoder();
 for await(const bytes of response.body){
  size+=bytes.length;if(size>4*1024*1024)throw Error('Metadata too large');
  text+=decoder.decode(bytes,{stream:true});
  const events=streaming?text.replaceAll('\r\n','\n').split('\n\n').slice(0,-1).map(e=>e.split('\n').filter(l=>l.startsWith('data:')).map(l=>l.slice(5).trimStart()).join('\n')):[text];
  for(const event of events){let data;try{data=JSON.parse(event);}catch{continue;}
   if(data.jsonrpc!=='2.0'||data.id!==requestId)continue;
   if(data.error)throw Error('Crunchbase metadata RPC error');
   return data.result;
  }
 }
 throw Error('Incomplete metadata response');
}
try{
 const init=await rpc('initialize',{protocolVersion:protocol,capabilities:{},clientInfo:{name:'Apiosk',version:'1.0'}});
 if(!['2025-11-25','2025-06-18','2025-03-26'].includes(init.protocolVersion))throw Error('Unsupported MCP version');
 protocol=init.protocolVersion;await rpc('notifications/initialized',undefined,true);
 const tools=[];let cursor;const cursors=new Set();
 for(let page=0;page<10;page++){
  const result=await rpc('tools/list',cursor?{cursor}:{});if(!Array.isArray(result.tools))throw Error('Missing tools');
  tools.push(...result.tools);cursor=result.nextCursor;
  if(!cursor)break;if(cursors.has(cursor)||page===9)throw Error('Incomplete tools pagination');cursors.add(cursor);
 }
 const selected=reviewed.map(r=>tools.find(t=>t.name===r.tool));
 if(selected.some(t=>!t))throw Error('A reviewed read tool is missing from this account');
 const record={at:new Date().toISOString(),server:'https://mcp.crunchbase.com',protocol,tools:selected,
  other_tool_names:tools.filter(t=>!reviewed.some(r=>r.tool===t.name)).map(t=>t.name),activation:false};
 await fs.mkdir(new URL('../output/crunchbase-mcp/',import.meta.url),{recursive:true});
 await fs.writeFile(new URL('../output/crunchbase-mcp/tool-discovery.json',import.meta.url),JSON.stringify(record,null,2)+'\n',{mode:0o600});
 console.log(JSON.stringify({read_tools:selected.map(t=>t.name),metadata_saved:true,data_calls:0,activated:false}));
}catch(error){console.error(error.message);process.exitCode=1;}
