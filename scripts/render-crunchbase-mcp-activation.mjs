import fs from 'node:fs/promises';
import {source,tools} from './crunchbase-mcp-catalog.mjs';
const fixture=JSON.parse(await fs.readFile(new URL('../tests/fixtures/crunchbase/official-mcp.json',import.meta.url)));
const records=JSON.parse(await fs.readFile(new URL('../legacy-executor/tests/fixtures/crunchbase-mcp-live.json',import.meta.url)));
const q=v=>`'${String(v).replaceAll("'","''")}'`;
const j=v=>`${q(JSON.stringify(v))}::jsonb`;
const sql=[`-- Activate only after deploying the authenticated, public-scope MCP adapter.
-- Nine real public samples cover all four tools and company/person/funding variants.
-- Real adapter -> Vault -> cb_reference passed; these are provider acceptance,
-- not evidence of a paid buyer execution. Existing API.market tariffs stay intact.
BEGIN;
SELECT pg_advisory_xact_lock(hashtext('apiosk-crunchbase'));
DO $guard$ BEGIN
 IF NOT EXISTS(SELECT 1 FROM vault.secrets WHERE name='CRUNCHBASE_MCP_OAUTH') THEN RAISE EXCEPTION 'Crunchbase OAuth not connected'; END IF;
 IF (SELECT count(*) FROM providers.api_endpoints e JOIN providers.apis a ON a.id=e.api_id WHERE a.slug=${q(source)} AND e.path IN (${tools.map(t=>q('/mcp/'+t.tool)).join(',')}) AND e.method='POST' AND e.origin_override='https://mcp.crunchbase.com' AND e.price=0.047259 AND e.payment_required AND e.metadata_source='authenticated-official-mcp-discovery')<>4 THEN RAISE EXCEPTION 'Expected four schema-reviewed and priced MCP endpoints'; END IF;
END $guard$;`];
for(const s of fixture.services){
 const name=s.path.slice(5),c=s.contract;
 const record=records.find(r=>r.name===name && (name==='cb_reference'||
  name==='cb_entity_autocomplete'&&r.arguments.collection_ids===(c.fixed_params.collection_ids||'organizations')||
  name==='cb_entity_get'&&r.arguments.entity_def_id===c.fixed_params.entity_def_id||
  name==='cb_search_query'&&r.arguments.collection_id===c.fixed_params.collection_id));
 if(!record)throw Error('Missing real acceptance sample for '+c.capability_slug);
 const sample={source:'Crunchbase',transport:'official_mcp',tool:name,data:record.data,retrieved_at:record.at};
 sql.push(`UPDATE catalog.capability_execution_contracts c SET status='active',fixed_params=${j(c.fixed_params)},sample_payload=${j(sample)},validated_at=now(),validated_note=${q(`Authenticated public provider response ${record.data.request_id}; ${record.at}; input ${JSON.stringify(record.arguments)}. Buyer charge 0 (included provider allowance). Real Rust adapter and Vault tested separately with free reference. No paid buyer-flow claim.`)},updated_at=now()
 FROM providers.api_endpoints e JOIN providers.apis a ON a.id=e.api_id WHERE c.endpoint_id=e.id AND a.slug=${q(source)} AND c.capability_slug=${q(c.capability_slug)} AND c.adapter_version='crunchbase-mcp-v1' AND e.path=${q(s.path)};`);
}
sql.push(`UPDATE catalog.fact_types SET status='active' WHERE slug IN (${fixture.vocabulary.map(f=>q(f.slug)).join(',')});
UPDATE catalog.capabilities SET status='active' WHERE slug IN (${fixture.capabilities.map(c=>q(c.slug)).join(',')});
UPDATE providers.api_endpoints e SET active=true FROM providers.apis a WHERE e.api_id=a.id AND a.slug=${q(source)} AND e.method='POST' AND e.origin_override='https://mcp.crunchbase.com' AND e.path IN (${tools.map(t=>q('/mcp/'+t.tool)).join(',')});
UPDATE providers.apis SET listing_metadata=jsonb_set(jsonb_set(listing_metadata,'{official_mcp,status}','"active"'),' {coverage_notice}'::text[],${j('Official Crunchbase company, people and funding name search, public-field filters and profiles are available through four OAuth-backed MCP tools. Search returns at most 15 rows per approved call. Profile results include public company fields, founders, employees, board/advisors, funding rounds and investors where supplied; has_more_entities marks incomplete cards. Existing API.market name lookup and organization browsing remain available. Jina enriches the same Crunchbase result internally when the public page is readable. Private CRM, saved-list filters and recursive relationship queries are unsupported.')}) WHERE slug=${q(source)};
DO $verify$ BEGIN
 IF (SELECT count(*) FROM catalog.capability_execution_contracts c JOIN providers.api_endpoints e ON e.id=c.endpoint_id JOIN providers.apis a ON a.id=e.api_id WHERE a.slug=${q(source)} AND e.path LIKE '/mcp/%' AND c.status='active' AND e.active AND c.sample_payload IS NOT NULL)<>10 THEN RAISE EXCEPTION 'Expected ten active verified contracts'; END IF;
END $verify$;
COMMIT;`);
await fs.writeFile(new URL('../supabase/migrations/20261011152332_activate_crunchbase_official_mcp.sql',import.meta.url),sql.join('\n\n')+'\n');
console.log('Activation prepared from nine real public samples. Execute only after adapter deployment.');
