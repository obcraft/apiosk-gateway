import fs from 'node:fs/promises';
for (const [name, destination] of [['tool-schemas.json','gateway-v2-contracts.json'], ['host-instructions.md','gateway-v2-instructions.md']]) {
const source=new URL(`../contracts/${name}`,import.meta.url);
const target=new URL(`../../mcp/src/${destination}`,import.meta.url);
const data=await fs.readFile(source,'utf8');
if(process.argv.includes('--check')) {
 if(await fs.readFile(target,'utf8')!==data) throw new Error('MCP v2 schemas drifted; run sync-contracts.mjs');
} else {await fs.writeFile(target,data);}
}

for (const [sourceName, destination] of [['tool-schemas.json','gateway-v2-contracts.json'], ['host-instructions.md','gateway-v2-instructions.md']]) {
 const data=await fs.readFile(new URL(`../contracts/${sourceName}`,import.meta.url),'utf8');
 const target=new URL(`../../app/src/lib/webmcp/${destination}`,import.meta.url);
 if(process.argv.includes('--check')) {if(await fs.readFile(target,'utf8')!==data) throw new Error('WebMCP v2 contract drift');}
 else await fs.writeFile(target,data);
}
