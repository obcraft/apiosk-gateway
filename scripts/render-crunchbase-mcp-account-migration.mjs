import fs from 'node:fs/promises';
import {source,tools,capabilities} from './crunchbase-mcp-catalog.mjs';
const file='20261011145243_refine_crunchbase_mcp_account_schema.sql';
const q=v=>`'${String(v).replaceAll("'","''")}'`;
const j=v=>`${q(JSON.stringify(v))}::jsonb`;
const fixture=JSON.parse(await fs.readFile(new URL('../tests/fixtures/crunchbase/official-mcp.json',import.meta.url)));
const sql=[`-- Authenticated tools/list and public cb_reference discovery, 2026-10-11.
-- Provider default search limit (20) exceeds this account's maximum (15).
-- Only public company, person, funding and identifier data is exposed.
-- Credentials, tariffs and activation state remain untouched.
BEGIN;
SELECT pg_advisory_xact_lock(hashtext('apiosk-crunchbase'));`];
for(const t of tools){
 sql.push(`UPDATE providers.api_endpoints e SET request_body=${j(t.schema)},request_example=${j(t.example)},description=${q(t.description)},metadata_source='authenticated-official-mcp-discovery',metadata_at=now()
 FROM providers.apis a WHERE e.api_id=a.id AND a.slug=${q(source)} AND e.path=${q('/mcp/'+t.tool)} AND e.method='POST' AND e.origin_override='https://mcp.crunchbase.com';`);
}
for(const f of fixture.vocabulary)sql.push(`UPDATE catalog.fact_types SET value_schema=${j(f.value_schema)} WHERE slug=${q(f.slug)};`);
for(const s of fixture.services){
 const c=s.contract,t=tools.find(t=>s.path==='/mcp/'+t.tool);
 sql.push(`UPDATE catalog.capabilities SET input_contract=${j(t.schema)},description=${q(s.description)} WHERE slug=${q(c.capability_slug)};`);
 sql.push(`UPDATE catalog.capability_execution_contracts c SET requires=${j(c.requires)},input_map=${j(c.input_map)},fixed_params=${j(c.fixed_params)},notes='Authenticated official MCP schema; public data only. Search limit 15. Simple predicates only, no private CRM or saved-list filters. Jina enrichment remains internal.'
 FROM providers.api_endpoints e JOIN providers.apis a ON a.id=e.api_id WHERE c.endpoint_id=e.id AND a.slug=${q(source)} AND c.capability_slug=${q(c.capability_slug)} AND c.adapter_version='crunchbase-mcp-v1';`);
}
sql.push(`UPDATE providers.apis SET listing_metadata=jsonb_set(listing_metadata,'{official_mcp,account_limits}',${j({search_max:15,public_data_only:true,schema_verified_on:'2026-10-11'})}) WHERE slug=${q(source)};
COMMIT;`);
await fs.writeFile(new URL('../supabase/migrations/'+file,import.meta.url),sql.join('\n\n')+'\n');
console.log(`${tools.length} endpoint schemas and ${capabilities.length} contracts aligned with authenticated discovery; no activation.`);
