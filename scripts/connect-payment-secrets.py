"""Connect the supplied server-only keys to the existing x402 executor.

Default is read-only validation. --apply imports only the two named secrets
into the existing Fly app over stdin; no secret appears in argv or output.
This does not change user limits, transfer funds, or create a payment.
"""
import argparse
import json
import re
import subprocess
import urllib.request
from pathlib import Path

root = Path(__file__).resolve().parent.parent
args = argparse.ArgumentParser(description=__doc__)
args.add_argument('--apply', action='store_true')
args = args.parse_args()
values = {}
for line in (root / '.env').read_text().splitlines():
    if '=' in line and not line.lstrip().startswith('#'):
        key, value = line.split('=', 1)
        values[key.strip()] = value.strip().strip('"').strip("'")
treasury = values['TREASURY_PRIVATE_KEY']
service = values['SUPABASE_SERVICE_ROLE_KEY']
assert re.fullmatch(r'(0x)?[0-9a-fA-F]{64}', treasury), 'Invalid treasury key format'
assert service and not any(c.isspace() for c in service), 'Invalid service key format'
assert values['SUPABASE_URL'] == 'https://api.apiosk.com', 'Unexpected project URL'
request = urllib.request.Request('https://api.apiosk.com/rest/v1/apis?select=id&limit=0',
    headers={'apikey': service, 'Authorization': 'Bearer ' + service})
with urllib.request.urlopen(request, timeout=10) as response:
    assert response.status == 200 and json.load(response) == [], 'Service key check failed'
print('Supabase server key accepted; treasury key format valid. Destination: Fly apioskgateway.')
if args.apply:
    result = subprocess.run(['fly', 'secrets', 'import', '--app', 'apioskgateway'],
        input='TREASURY_PRIVATE_KEY=' + treasury + '\nSUPABASE_SERVICE_ROLE_KEY=' + service + '\n',
        text=True, capture_output=True, cwd=root.parent / 'gateway')
    if result.returncode:
        raise SystemExit('Secret import did not complete; inspect deployment status. Secret output suppressed.')
    print('Treasury and Supabase server-role keys connected to the existing payment executor. No payment submitted.')
