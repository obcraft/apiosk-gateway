// Promote staged source-document contracts that returned a usable live
// response in validate-source-operations.mjs. Writes one migration; never
// touches a contract without paid evidence.
//
//   node scripts/render-source-operation-promotion.mjs <out-dir> <migration.sql>
//
// The evidence is the provider's own response, trimmed to its shape (three
// items per array, 600 characters per string) so the runtime catalogue stays
// small. The full response stays in the attempt file, named in the note.
import fs from 'node:fs/promises';
import path from 'node:path';
import {createHash} from 'node:crypto';

const [outDir, migration] = process.argv.slice(2);
if (!outDir || !migration) throw Error('Usage: render-source-operation-promotion.mjs out-dir migration.sql');

export function trim(value, depth = 0) {
  if (typeof value === 'string') return value.length > 600 ? value.slice(0, 600) : value;
  if (Array.isArray(value)) return depth > 8 ? [] : value.slice(0, 3).map((v) => trim(v, depth + 1));
  if (value && typeof value === 'object') {
    if (depth > 8) return {};
    return Object.fromEntries(Object.entries(value).slice(0, 60).map(([k, v]) => [k, trim(v, depth + 1)]));
  }
  return value;
}

const q = (x) => `'${String(x).replaceAll("'", "''")}'`;
const paidDir = path.join(outDir, 'paid');
const rows = [];
for (const file of (await fs.readdir(paidDir)).sort()) {
  const r = JSON.parse(await fs.readFile(path.join(paidDir, file), 'utf8'));
  if (r.usable !== true || r.did !== true || r.body == null) continue;
  // A string body is kept as the document itself, like the adapter does.
  const sample = typeof r.body === 'string' ? r.body.slice(0, 4000) : trim(r.body);
  const digest = createHash('sha256').update(JSON.stringify(r.body)).digest('hex').slice(0, 16);
  const note = `Live treasury procurement ${r.started_at.slice(0, 10)}: request ${r.request_id ?? 'n/a'}, charged ${r.charged_usdc} USDC, inputs ${JSON.stringify(r.inputs)}; sample trimmed from response sha256:${digest}`;
  rows.push(`UPDATE catalog.capability_execution_contracts SET status='active', sample_payload=${q(JSON.stringify(sample))}::jsonb, validated_at=now(), validated_note=${q(note)}, updated_at=now()
 WHERE id=${q(r.id)}::uuid AND endpoint_id=${q(r.endpoint_id)}::uuid AND status='validated' AND adapter_version='source-document-v1'
   AND NOT EXISTS (SELECT 1 FROM catalog.capability_execution_contracts x WHERE x.endpoint_id=${q(r.endpoint_id)}::uuid AND x.status='active');`);
}
const sql = [
  `-- Promote ${rows.length} staged source-document contracts with a usable live response.`,
  '-- Evidence: scripts/validate-source-operations.mjs treasury procurement; see validated_note per row.',
  'BEGIN;',
  ...rows,
  'COMMIT;',
  '',
].join('\n\n');
await fs.writeFile(migration, sql);
console.log(JSON.stringify({promoted: rows.length, migration}));
