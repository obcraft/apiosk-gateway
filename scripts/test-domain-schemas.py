#!/usr/bin/env python3
"""Validate domain views on disposable PostgreSQL; never connects to production."""
import importlib.util
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
PG = Path(os.environ.get("PG_BIN", "/opt/homebrew/opt/postgresql@17/bin"))
MANIFEST = json.loads((ROOT / "supabase/domain-schemas.json").read_text())
spec = importlib.util.spec_from_file_location("domains", ROOT / "scripts/generate-domain-schemas.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
MIGRATION = module.generate(MANIFEST)

with tempfile.TemporaryDirectory(prefix="apiosk-domain-test-") as tmp:
    data = Path(tmp) / "data"
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    subprocess.run([str(PG / "initdb"), "-D", str(data), "-U", "postgres", "-A", "trust", "--no-locale"], check=True, stdout=subprocess.DEVNULL)
    subprocess.run([str(PG / "pg_ctl"), "-D", str(data), "-l", str(Path(tmp) / "postgres.log"), "-o", f"-h 127.0.0.1 -p {port} -k {tmp} -c wal_level=logical", "-w", "start"], check=True, stdout=subprocess.DEVNULL)

    def run(sql):
        return subprocess.run([str(PG / "psql"), "-X", "-qAt", "-v", "ON_ERROR_STOP=1", "-h", "127.0.0.1", "-p", str(port), "-U", "postgres", "-d", "postgres"], input=sql, text=True, capture_output=True, check=True).stdout.strip()

    try:
        setup = ["CREATE ROLE anon; CREATE ROLE authenticated; CREATE ROLE service_role BYPASSRLS; CREATE ROLE domain_reader; CREATE SCHEMA gateway_v2;"]
        for group in MANIFEST["domains"]:
            for rel in group["relations"]:
                src = module.ident(rel["source_schema"]) + "." + module.ident(rel["source_name"])
                setup.extend([
                    f"CREATE TABLE {src}(id integer PRIMARY KEY, owner_name text, payload jsonb);",
                    f"ALTER TABLE {src} ENABLE ROW LEVEL SECURITY;",
                    f"CREATE POLICY own_rows ON {src} USING(owner_name=current_user);",
                    f"INSERT INTO {src} VALUES(1,'domain_reader','{{\"amount\":100}}'),(2,'someone_else','{{\"amount\":200}}');",
                    f"GRANT SELECT ON {src} TO service_role, domain_reader;",
                ])
        setup.extend([
            "GRANT USAGE ON SCHEMA public,gateway_v2 TO service_role,domain_reader;",
            "CREATE PUBLICATION fixture_realtime FOR TABLE public.agent_connections;",
            # A nested source view exercises preserving existing view behavior.
            "ALTER TABLE public.admin_api_endpoints RENAME TO admin_api_endpoints_fixture;",
            "CREATE VIEW public.admin_api_endpoints WITH(security_invoker=true) AS SELECT * FROM public.admin_api_endpoints_fixture;",
            "GRANT SELECT ON public.admin_api_endpoints TO service_role,domain_reader;",
            # Even global default grants cannot accidentally expose the new aliases.
            "ALTER DEFAULT PRIVILEGES FOR ROLE postgres GRANT ALL ON TABLES TO anon,authenticated;",
        ])
        run("\n".join(setup))
        canonical = "SELECT md5(string_agg(oid::text||':'||relname||':'||relkind::text||':'||relrowsecurity::text,',' ORDER BY oid)) FROM pg_class WHERE relnamespace IN ('public'::regnamespace,'gateway_v2'::regnamespace);"
        before = run(canonical)
        run("BEGIN;\n" + MIGRATION + "\nCOMMIT;")
        assert run(canonical) == before, "Canonical objects changed"
        assert run("SELECT schemaname||'.'||tablename FROM pg_publication_tables WHERE pubname='fixture_realtime';") == "public.agent_connections"
        comparisons = []
        for group in MANIFEST["domains"]:
            for rel in group["relations"]:
                alias = group["name"] + "." + rel["name"]
                src = rel["source_schema"] + "." + rel["source_name"]
                comparisons.append(f"SELECT '{alias}' WHERE (SELECT jsonb_agg(to_jsonb(t) ORDER BY id) FROM {alias} t) IS DISTINCT FROM (SELECT jsonb_agg(to_jsonb(t) ORDER BY id) FROM {src} t);")
        assert not run("\n".join(comparisons)), "An alias changed source contents"
        domains = ",".join(module.literal(x["name"]) for x in MANIFEST["domains"])
        assert run(f"SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname IN ({domains}) AND (has_table_privilege('anon',c.oid,'SELECT') OR has_table_privilege('authenticated',c.oid,'SELECT') OR has_table_privilege('service_role',c.oid,'UPDATE') OR NOT ('security_invoker=true'=ANY(c.reloptions)));" ) == "0"
        assert run(f"SELECT count(*) FROM pg_namespace WHERE nspname IN ({domains}) AND (has_schema_privilege('anon',oid,'USAGE') OR has_schema_privilege('authenticated',oid,'USAGE'));" ) == "0"
        # Test RLS as an actual non-owner caller through a domain view.
        run("GRANT USAGE ON SCHEMA kyb TO domain_reader; GRANT SELECT ON kyb.profiles TO domain_reader;")
        assert run("SET ROLE domain_reader; SELECT count(*) FROM kyb.profiles; RESET ROLE;") == "1"
        # Existing upserts and the alias's live visibility still work.
        run("INSERT INTO public.kyb_profiles VALUES(1,'domain_reader','{\"amount\":300}') ON CONFLICT(id) DO UPDATE SET payload=excluded.payload;")
        assert run("SELECT payload->>'amount' FROM kyb.profiles WHERE id=1;") == "300"
        assert run("SET ROLE service_role; SELECT count(*) FROM kyb.profiles; RESET ROLE;") == "2"
        # Do not broaden existing source access via a granted wrapper.
        run("REVOKE SELECT ON public.kyb_profiles FROM domain_reader;")
        try:
            run("SET ROLE domain_reader; SELECT count(*) FROM kyb.profiles;")
            raise AssertionError("View bypassed underlying source privileges")
        except subprocess.CalledProcessError as exc:
            assert "permission denied" in exc.stderr
        print(f"PASS: {sum(len(x['relations']) for x in MANIFEST['domains'])} views in {len(MANIFEST['domains'])} domains; source identity/data, RLS, least privilege, canonical upsert, live alias reads and Realtime preserved.")
    except subprocess.CalledProcessError as exc:
        print(exc.stderr)
        raise
    finally:
        subprocess.run([str(PG / "pg_ctl"), "-D", str(data), "-m", "immediate", "-w", "stop"], check=True, stdout=subprocess.DEVNULL)
