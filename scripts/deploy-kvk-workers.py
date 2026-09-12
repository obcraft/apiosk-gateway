#!/usr/bin/env python3
"""Provision one Fly worker app/IP and deploy one shared worker image.

Default is read-only. --provision creates billable apps/egress allocations and
imports only DATABASE_URL plus the worker's expected egress identity from .local/kvk-worker.env.
--image deploys an already-built image to the configured app (one Machine).
"""
import argparse
import ipaddress
import json
import os
from pathlib import Path
import subprocess
from urllib.parse import parse_qsl, urlencode, urlsplit, urlunsplit

ROOT = Path(__file__).resolve().parents[1]
APPS = [f'apiosk-kvk-worker-{i}' for i in range(1, 2)]
CONFIG = 'deploy/kvk-workers/fly.toml'

def fly(*args, secret_input=None):
    result = subprocess.run(['fly', *args], cwd=ROOT, input=secret_input, text=True, capture_output=True)
    if result.returncode:
        # Never include secret payloads in driver diagnostics.
        raise RuntimeError('Fly command failed: ' + ' '.join(args) + (' (secret input redacted)' if secret_input else '\n' + result.stderr[-3000:]))
    return result.stdout

def database_url():
    values = {}
    for line in (ROOT / '.local/kvk-worker.env').read_text().splitlines():
        if not line.startswith('#') and '=' in line:
            key, value = line.split('=',1)
            values[key] = value.strip().strip('"').strip("'")
    db = urlsplit(values['DATABASE_URL'])
    assert db.hostname == 'aws-1-eu-west-1.pooler.supabase.com'
    assert db.username == 'apiosk_kvk_worker.jgjoiyqdyypouskftzeq'
    query = dict(parse_qsl(db.query))
    query.update(sslmode='verify-full', sslrootcert='/etc/ssl/certs/supabase-prod-ca-2021.crt')
    return urlunsplit((db.scheme,db.netloc,db.path,urlencode(query),''))

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--provision',action='store_true')
    parser.add_argument('--image')
    parser.add_argument('--output',default='output/kvk-fly-workers.json')
    args = parser.parse_args()
    existing = {row['Name'] for row in json.loads(fly('apps','list','--org','apiosk','--json'))}
    manifest = []
    for app in APPS:
        if app not in existing:
            if not args.provision:
                manifest.append({'app':app,'state':'not provisioned'}); continue
            fly('apps','create',app,'--org','apiosk','--yes')
        addresses = json.loads(fly('ips','list','--app',app,'--json'))
        ipv4 = [a['Address'] for a in addresses if a['Type']=='egress_v4' and a['Region']=='ams']
        if not ipv4 and args.provision:
            fly('ips','allocate-egress','--app',app,'--region','ams','--yes')
            addresses = json.loads(fly('ips','list','--app',app,'--json'))
            ipv4 = [a['Address'] for a in addresses if a['Type']=='egress_v4' and a['Region']=='ams']
        if len(ipv4)!=1:
            raise RuntimeError(f'{app}: requires exactly one app-scoped egress IPv4 in ams')
        ipaddress.IPv4Address(ipv4[0])
        if args.provision:
            secret = 'DATABASE_URL='+database_url()+'\nKVK_EGRESS_ROUTES='+json.dumps([{'expected_ipv4':ipv4[0]}],separators=(',',':'))+'\n'
            fly('secrets','import','--app',app,'--stage',secret_input=secret)
        manifest.append({'app':app,'expected_ipv4':ipv4[0],'region':'ams'})
        print(json.dumps(manifest[-1]),flush=True)
    ips = [row['expected_ipv4'] for row in manifest if 'expected_ipv4' in row]
    if len(ips) != len(set(ips)):
        raise RuntimeError('Worker apps share an egress IPv4')
    if args.image:
        assert len(ips)==len(APPS), 'Provision all configured routes first'
        assert args.image.startswith('registry.fly.io/apiosk-kvk-worker-1:'), 'Use the reviewed worker image'
        for row in manifest:
            machines = json.loads(fly('machine','list','--app',row['app'],'--json'))
            assert len(machines)<=1, f"Unexpected spare machines in {row['app']}"
            fly('deploy','--app',row['app'],'--config',CONFIG,'--image',args.image,'--ha=false','--strategy','immediate','--yes')
            current = json.loads(fly('machine','list','--app',row['app'],'--json'))
            for machine in current:
                if machine['state'] in ('stopped','suspended'):
                    fly('machine','start',machine['id'],'--app',row['app'])
                fly('machine','wait',machine['id'],'--app',row['app'],'--state','started','--wait-timeout','45s')
            row['image']=args.image
            row['machines']=[{'id':m['id'],'state':m['state']} for m in json.loads(fly('machine','list','--app',row['app'],'--json'))]
            print(json.dumps(row),flush=True)
    target = ROOT / args.output
    target.parent.mkdir(parents=True,exist_ok=True)
    target.write_text(json.dumps(manifest,indent=2)+'\n')

if __name__ == '__main__':
    main()
