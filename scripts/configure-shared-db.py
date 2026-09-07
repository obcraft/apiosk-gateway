"""Reuse v1's Supabase project through a dedicated v2 database login.
Default: read-only preflight. --apply: provision login and update ignored .env.
Secrets are read from files and sent via process environment/stdin, never printed.
Requires the reviewed runtime-role migration. No model or payment calls.
"""
import argparse, os, secrets, shutil, subprocess
from pathlib import Path
from urllib.parse import urlsplit, urlunsplit, quote, unquote, urlencode

root = Path(__file__).resolve().parent.parent
os.umask(0o077)
ca_cert = root / '.local/certs/supabase-prod-ca-2021.crt'

def read_env(path):
    result = {}
    for line in path.read_text().splitlines():
        if line.strip().startswith('#') or '=' not in line:
            continue
        key, value = line.split('=', 1)
        if not key.strip().replace('_', '').isalnum():
            continue
        result[key.strip()] = value.strip().strip('"').strip("'")
    return result

args = argparse.ArgumentParser(description=__doc__)
args.add_argument('--apply', action='store_true')
args = args.parse_args()
old = read_env(root.parent / 'gateway/.env')
local = read_env(root / '.env')
source = urlsplit(old['DATABASE_URL'])
assert source.hostname == 'aws-1-eu-west-1.pooler.supabase.com', 'Unexpected project host'
assert source.username == 'postgres.jgjoiyqdyypouskftzeq', 'Unexpected project identity'
psql = shutil.which('psql')
assert psql, 'psql is required'

def sql(statement, user=None, password=None):
    env = {**os.environ, 'PGHOST': source.hostname, 'PGPORT': str(source.port or 5432),
           'PGDATABASE': source.path.lstrip('/'), 'PGUSER': user or unquote(source.username),
           'PGPASSWORD': password or unquote(source.password), 'PGSSLMODE': 'verify-full',
           'PGSSLROOTCERT': str(ca_cert), 'PGCONNECT_TIMEOUT': '10', 'PGOPTIONS': '-c statement_timeout=10000'}
    result = subprocess.run([psql, '-X', '-A', '-t', '-v', 'ON_ERROR_STOP=1'], input=statement,
                            env=env, text=True, capture_output=True, timeout=25)
    if result.returncode:
        # Do not echo SQL or driver stderr: credential DDL can include the key.
        reason = 'TLS verification' if 'certificate' in result.stderr.lower() else 'connection or SQL permission'
        raise SystemExit(f'Database operation failed ({reason}); no credential output retained.')
    return result.stdout.strip()

ready = sql("SELECT to_regclass('gateway_v2.tasks') IS NOT NULL AND EXISTS(SELECT 1 FROM pg_roles WHERE rolname='gateway_v2_runtime');")
assert ready == 't', 'Apply the approved v2 state/runtime migrations first'
print('Verified v1 Supabase project and installed v2 schema/runtime role over verified TLS.')
if not args.apply:
    raise SystemExit()
login = 'gateway_v2_server'
project_login = login + '.jgjoiyqdyypouskftzeq'
exists = sql("SELECT EXISTS(SELECT 1 FROM pg_roles WHERE rolname='gateway_v2_server');") == 't'
if exists:
    configured = urlsplit(local['DATABASE_URL'])
    assert configured.hostname == source.hostname and configured.username == project_login, 'Existing login: supply its credential locally; password will not be reset'
    password = unquote(configured.password)
else:
    password = secrets.token_hex(32)
    sql("BEGIN; CREATE ROLE gateway_v2_server LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS PASSWORD '" + password + "'; GRANT gateway_v2_runtime TO gateway_v2_server; COMMIT;")
# Preserve the new credential before verification so a failure remains recoverable.
url = urlunsplit(('postgresql', f'{project_login}:{quote(password)}@{source.hostname}:{source.port}', source.path, urlencode({'sslmode':'verify-full','sslrootcert':str(ca_cert)}), ''))
updates = {'DATABASE_URL': url, 'SUPABASE_URL': old['SUPABASE_URL'], 'V2_EXECUTION_ENABLED': '0'}
path = root / '.env'
backup = root / '.env.local-db.backup'
if not backup.exists():
    backup.write_text(path.read_text()); backup.chmod(0o600)
lines = path.read_text().splitlines()
for key, value in updates.items():
    lines = [l for l in lines if not l.startswith(key + '=')]
    lines.append(key + '=' + value)
path.write_text('\n'.join(lines) + '\n'); path.chmod(0o600)
checks = sql("SELECT current_user, has_table_privilege(current_user,'gateway_v2.tasks','SELECT'), has_table_privilege(current_user,'public.user_balances','UPDATE'), has_table_privilege(current_user,'public.apis','SELECT');", project_login, password)
assert checks == 'gateway_v2_server|t|f|f', 'Runtime privilege verification failed'
print('Configured same Supabase database using dedicated runtime login. No balance-write or provider-secret access. Execution remains off.')
