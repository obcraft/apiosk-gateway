import fs from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
// These two modules still intentionally share an identical implementation.
// Contracts/adapters/schema have evolved separately; pricing is checked by behavior.
const files=['facts.rs','facts/path.rs'];
for(const file of files) {
 const original=await fs.readFile(new URL(`../crates/commerce-runtime/src/${file}`,import.meta.url));
 const extracted=await fs.readFile(new URL(`../crates/catalog-core/src/${file}`,import.meta.url));
 if(!original.equals(extracted)) throw new Error(`Shared core drift: ${file}. Review the change before updating either implementation.`);
}
console.log(`${files.length} shared fact modules match commerce-runtime.`);
const check=spawnSync('cargo',['test','--locked','--test','source_reuse'],{
 cwd:new URL('..',import.meta.url),stdio:'inherit'
});
if(check.error) throw check.error;
if(check.status!==0) process.exit(check.status??1);
