-- Preserve schema reachability for existing column-level grants as well.
-- No column or table privileges are added.
DO $migration$
DECLARE r record;
BEGIN
  FOR r IN
    SELECT DISTINCT n.nspname, p.rolname
    FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
    CROSS JOIN pg_roles p
    WHERE n.nspname IN ('kyb','providers','catalog','actions','billing','agents','wallets','research','integrations','observability','support','content','operations','identity')
      AND c.relkind IN ('r','p','v','m') AND NOT p.rolsuper
      AND has_schema_privilege(p.oid,CASE WHEN n.nspname='actions' AND c.relname IN ('tasks','requests','results','executions','authorizations') THEN 'gateway_v2' ELSE 'public' END,'USAGE')
      AND has_any_column_privilege(p.oid,c.oid,'SELECT,INSERT,UPDATE,REFERENCES')
      AND NOT has_schema_privilege(p.oid,n.oid,'USAGE')
  LOOP
    EXECUTE format('GRANT USAGE ON SCHEMA %I TO %I',r.nspname,r.rolname);
  END LOOP;
END
$migration$;
NOTIFY pgrst, 'reload schema';
