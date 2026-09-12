#!/usr/bin/env python3
"""Generate the physical move, preserving relation identities and privileges."""
import json
from pathlib import Path
root=Path(__file__).resolve().parents[1]
manifest=json.loads((root/'supabase/domain-schemas.json').read_text())
rows=[(r['source_schema'],r['source_name'],d['name'],r['name']) for d in manifest['domains'] for r in d['relations']]
values=',\n'.join('('+','.join("'"+v+"'" for v in row)+')' for row in rows)
schemas=[d['name'] for d in manifest['domains']]
path='public, gateway_v2, '+', '.join(schemas)+', extensions'
exposed='public, graphql_public, '+', '.join(schemas)
sql="""-- Replace the administrative aliases with real tables and original views.
-- Names and OIDs are preserved. No table copies and no compatibility tables in public.
-- Deploy schema-aware API clients and SQL search paths BEFORE this migration.
SET LOCAL lock_timeout = '3s';
SET LOCAL statement_timeout = '120s';
CREATE TEMP TABLE domain_move_map(source_schema text, source_name text, target_schema text, old_alias text, source_oid oid, old_acl aclitem[], old_owner oid, old_rls boolean, old_filenode oid) ON COMMIT DROP;
INSERT INTO domain_move_map(source_schema,source_name,target_schema,old_alias) VALUES
"""+values+""";
UPDATE domain_move_map m SET source_oid=c.oid,old_acl=c.relacl,old_owner=c.relowner,old_rls=c.relrowsecurity,old_filenode=c.relfilenode
FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
WHERE n.nspname=m.source_schema AND c.relname=m.source_name AND c.relkind IN ('r','p','v','m');
DO $migration$
#variable_conflict use_column
DECLARE m record; r record; f record; ddl text; original_ddl text; setting text; reference_pattern text;
BEGIN
  IF EXISTS(SELECT 1 FROM domain_move_map WHERE source_oid IS NULL) THEN
    RAISE EXCEPTION 'Missing canonical source: schema drift; abort before changing anything';
  END IF;
  IF EXISTS(SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public' AND c.relkind IN ('r','p','v','m') AND NOT EXISTS(SELECT 1 FROM domain_move_map m WHERE m.source_oid=c.oid)) THEN
    RAISE EXCEPTION 'Unmapped public relation: update reviewed mapping first';
  END IF;
  -- All 189 aliases must still be invoker views directly dependent on the source.
  FOR m IN SELECT * FROM domain_move_map ORDER BY source_oid LOOP
    IF NOT EXISTS(SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
      WHERE n.nspname=m.target_schema AND c.relname=m.old_alias AND c.relkind='v'
      AND 'security_invoker=true'=ANY(c.reloptions)
      AND EXISTS(SELECT 1 FROM pg_rewrite rw JOIN pg_depend dp ON dp.classid='pg_rewrite'::regclass AND dp.objid=rw.oid
        WHERE rw.ev_class=c.oid AND dp.refclassid='pg_class'::regclass AND dp.refobjid=m.source_oid)) THEN
      RAISE EXCEPTION 'Directory alias changed: %.%',m.target_schema,m.old_alias;
    END IF;
  END LOOP;
  -- No CASCADE: any unexpected dependency aborts the transaction.
  FOR m IN SELECT * FROM domain_move_map ORDER BY source_oid LOOP
    EXECUTE format('DROP VIEW %I.%I',m.target_schema,m.old_alias);
  END LOOP;
  FOR m IN SELECT * FROM domain_move_map ORDER BY source_oid LOOP
    -- Preserve effective source access. No table grants are added or widened.
    FOR r IN SELECT rolname FROM pg_roles WHERE NOT rolsuper
      AND has_schema_privilege(oid,m.source_schema,'USAGE')
      AND has_table_privilege(oid,m.source_oid,'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER') LOOP
      EXECUTE format('GRANT USAGE ON SCHEMA %I TO %I',m.target_schema,r.rolname);
    END LOOP;
    SELECT relkind INTO setting FROM pg_class WHERE oid=m.source_oid;
    EXECUTE format('ALTER %s %I.%I SET SCHEMA %I',CASE WHEN setting='v' THEN 'VIEW' WHEN setting='m' THEN 'MATERIALIZED VIEW' ELSE 'TABLE' END,m.source_schema,m.source_name,m.target_schema);
  END LOOP;
  -- Parsed FK/view/trigger dependencies follow the unchanged relation OIDs.
  -- PL/pgSQL and SQL function text must be rewritten explicitly.
  FOR f IN SELECT p.oid,p.proconfig,n.nspname,pg_get_functiondef(p.oid) AS definition
    FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
    WHERE p.prokind IN ('f','p') AND n.nspname IN ('public','gateway_v2','kvk_cache')
    AND NOT EXISTS(SELECT 1 FROM pg_depend d WHERE d.classid='pg_proc'::regclass AND d.objid=p.oid AND d.deptype='e') LOOP
    ddl:=f.definition;
    FOR m IN SELECT * FROM domain_move_map ORDER BY length(source_name) DESC LOOP
      reference_pattern := '\\m' || m.source_schema || '[.]' || m.source_name || '\\M';
      ddl:=regexp_replace(ddl,reference_pattern,format('%I.%I',m.target_schema,m.source_name),'g');
      ddl:=replace(ddl,format('"%s"."%s"',m.source_schema,m.source_name),format('%I.%I',m.target_schema,m.source_name));
      -- Three RPC capability probes inspect information_schema by schema/name.
      ddl:=regexp_replace(ddl,'table_schema = '''||m.source_schema||'''(\\s+and table_name\\s*=\\s*'''||m.source_name||''')','table_schema = '''||m.target_schema||'''\\1','gi');
    END LOOP;
    IF ddl IS DISTINCT FROM f.definition THEN EXECUTE ddl; END IF;
    IF f.nspname IN ('public','gateway_v2') THEN
      SELECT substring(x FROM length('search_path=')+1) INTO setting FROM unnest(f.proconfig) x WHERE x LIKE 'search_path=%';
      -- Empty search paths remain empty: their references are fully qualified.
      IF setting IS NULL OR setting NOT IN ('','""') THEN
        setting := regexp_replace(coalesce(setting,'public, extensions'),'\\mpublic\\M','__SEARCH_PATH__','g');
        EXECUTE format('ALTER %s %s SET search_path TO %s',CASE WHEN (SELECT prokind FROM pg_proc WHERE oid=f.oid)='p' THEN 'PROCEDURE' ELSE 'FUNCTION' END,f.oid::regprocedure,setting);
      END IF;
    END IF;
  END LOOP;
  IF EXISTS(SELECT 1 FROM domain_move_map m JOIN pg_class c ON c.oid=m.source_oid JOIN pg_namespace n ON n.oid=c.relnamespace
    WHERE n.nspname<>m.target_schema OR c.relname<>m.source_name OR c.relacl IS DISTINCT FROM m.old_acl
      OR c.relowner<>m.old_owner OR c.relrowsecurity<>m.old_rls OR c.relfilenode<>m.old_filenode) THEN
    RAISE EXCEPTION 'Physical identity, grants, owner or RLS changed unexpectedly';
  END IF;
END
$migration$;
"""
sql=sql.replace("__SEARCH_PATH__",path)
for d in manifest['domains']:
 desc=d['description'].replace("'","''")
 sql+=f"COMMENT ON SCHEMA {d['name']} IS '{desc}. Canonical physical tables and domain views.';\n"
sql+=f"ALTER ROLE authenticator SET pgrst.db_schemas = '{exposed}';\n"
sql+="NOTIFY pgrst, 'reload config';\nNOTIFY pgrst, 'reload schema';\n"
p=root/'supabase/migrations/20260912130830_physically_organize_domain_tables.sql'
p.write_text(sql)
print(p)
