import fs from 'node:fs/promises';
const files=['facts.rs','fees.rs','facts/adapter.rs','facts/contract.rs','facts/path.rs','facts/schema.rs'];
for(const file of files) {
 const original=await fs.readFile(new URL(`../../gateway/src/${file}`,import.meta.url));
 const extracted=await fs.readFile(new URL(`../crates/catalog-core/src/${file}`,import.meta.url));
 if(!original.equals(extracted)) throw new Error(`Shared core drift: ${file}. Review the change before updating either implementation.`);
}
console.log(`${files.length} shared catalog/pricing source files match the existing gateway.`);
