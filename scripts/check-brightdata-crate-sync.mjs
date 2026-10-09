import { readdir, readFile } from 'node:fs/promises';
const source = new URL('../crates/brightdata-linkedin/', import.meta.url);
const mirror = new URL('../legacy-executor/crates/brightdata-linkedin/', import.meta.url);
async function files(root, prefix = '') {
  const result = [];
  for (const entry of await readdir(new URL(prefix, root), { withFileTypes: true })) {
    const name = `${prefix}${entry.name}`;
    if (entry.isDirectory()) result.push(...await files(root, `${name}/`));
    else result.push(name);
  }
  return result.sort();
}
const expected = await files(source);
if (JSON.stringify(expected) !== JSON.stringify(await files(mirror))) throw new Error('Bright Data crate mirror file list differs');
for (const name of expected) {
  const [a, b] = await Promise.all([readFile(new URL(name, source)), readFile(new URL(name, mirror))]);
  if (!a.equals(b)) throw new Error(`Bright Data crate mirror differs: ${name}`);
}
console.log(`PASS Bright Data shared crate: ${expected.length} identical files in gateway and executor`);
