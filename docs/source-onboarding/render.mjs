// Offline SQL exporter. No database, provider, model or network calls.
import fs from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

export function validate(value, schema, at = '$') {
  const type = value === null ? 'null' : Array.isArray(value) ? 'array' : typeof value;
  if (schema.type && ![schema.type].flat().includes(type)) throw Error(`${at}: expected ${schema.type}`);
  if ('const' in schema && value !== schema.const) throw Error(`${at}: invalid constant`);
  if (schema.enum && !schema.enum.includes(value)) throw Error(`${at}: invalid enum`);
  if (type === 'object') {
    for (const key of schema.required ?? []) if (!(key in value)) throw Error(`${at}.${key}: required`);
    for (const [key, child] of Object.entries(value)) {
      if (schema.additionalProperties === false && !(key in (schema.properties ?? {}))) throw Error(`${at}.${key}: unknown field`);
      if (schema.properties?.[key]) validate(child, schema.properties[key], `${at}.${key}`);
    }
  }
  if (type === 'array') {
    if (value.length < (schema.minItems ?? 0)) throw Error(`${at}: too few items`);
    if (schema.items) value.forEach((child, i) => validate(child, schema.items, `${at}[${i}]`));
  }
  if (type === 'string') {
    if (value.length < (schema.minLength ?? 0)) throw Error(`${at}: too short`);
    if (schema.pattern && !new RegExp(schema.pattern).test(value)) throw Error(`${at}: invalid format`);
  }
  if (type === 'number' && (!Number.isFinite(value) || value < (schema.minimum ?? -Infinity))) throw Error(`${at}: invalid number`);
}

export async function render(manifest) {
  const schema = JSON.parse(await fs.readFile(new URL('manifest.schema.json', import.meta.url), 'utf8'));
  validate(manifest, schema);
  const json = JSON.stringify(manifest, null, 2);
  // Never interpolate AI text into SQL syntax. Dollar quoting is safe only if
  // neither the inner JSON delimiter nor the outer DO delimiter occurs in it.
  const hash = createHash('sha256').update(json).digest('hex');
  const manifestTag = `$bundle_${hash}$`;
  const blockTag = `$block_${hash}$`;
  if (json.includes(manifestTag) || json.includes(blockTag)) throw Error('SQL delimiter collision');
  const template = await fs.readFile(new URL('onboard-source.template.sql', import.meta.url), 'utf8');
  return template.replaceAll('$onboard$', blockTag).replaceAll('$manifest$', manifestTag)
    .replace('__SOURCE_MANIFEST_JSON__', () => json);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [input, output] = process.argv.slice(2);
  if (!input || !output) throw Error('Usage: node render.mjs manifest.json output.sql');
  const sql = await render(JSON.parse(await fs.readFile(input, 'utf8')));
  await fs.writeFile(output, sql, { flag: 'wx' });
  console.log(`Wrote ${output}. Inactive registration only; no database changes were made.`);
}
