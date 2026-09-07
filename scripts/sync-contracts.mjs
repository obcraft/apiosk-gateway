import fs from 'node:fs/promises';
const source=new URL('../contracts/tool-schemas.json',import.meta.url);
const target=new URL('../../mcp/src/gateway-v2-contracts.json',import.meta.url);
const data=await fs.readFile(source,'utf8');
if(process.argv.includes('--check')) {
 if(await fs.readFile(target,'utf8')!==data) throw new Error('MCP v2 schemas drifted; run sync-contracts.mjs');
} else {await fs.writeFile(target,data);}
