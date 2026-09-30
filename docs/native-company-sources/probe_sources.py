#!/usr/bin/env python3
"""Read-only, bounded public registry probes. Never purchases or activates sources."""
import concurrent.futures
import hashlib
import json
from pathlib import Path
import time
import urllib.request

OUT = Path(__file__).parent / 'evidence'
URLS = {
    'no-brreg-profile': 'https://data.brreg.no/enhetsregisteret/api/enheter/923609016',
    'fi-prh-profile': 'https://avoindata.prh.fi/opendata-ytj-api/v3/companies?businessId=0112038-9',
    'cz-ares-profile': 'https://ares.gov.cz/ekonomicke-subjekty-v-be/rest/ekonomicke-subjekty/00177041',
    'pl-krs-profile': 'https://api-krs.ms.gov.pl/api/krs/OdpisAktualny/0000006865?rejestr=P&format=json',
    'fr-dinum-search': 'https://recherche-entreprises.api.gouv.fr/search?q=552032534&per_page=1',
    'sk-ruz-profile': 'https://www.registeruz.sk/cruz-public/api/uctovna-jednotka?id=10882',
    'sk-ruz-identity': 'https://www.registeruz.sk/cruz-public/api/uctovne-jednotky?zmenene-od=2000-01-01&ico=00603481&max-zaznamov=1',
}

def get(item):
    key, url = item
    start = time.monotonic()
    record = {'id': key, 'url': url, 'checked_at': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()), 'method': 'GET'}
    try:
        req = urllib.request.Request(url, headers={'User-Agent': 'Apiosk source integration research (https://apiosk.com)', 'Accept': 'application/json'})
        with urllib.request.urlopen(req, timeout=30) as response:
            body = response.read(1_500_001)
            record.update(status=response.status, content_type=response.headers.get('content-type'), bytes=len(body))
        if len(body) > 1_500_000:
            raise ValueError('Response exceeded 1.5 MB probe limit')
        data = json.loads(body)
        # KRS responses can contain officer records; the evidence intentionally
        # retains only company identity and schema keys, not personal records.
        if key == 'pl-krs-profile':
            data = {'odpis': {'naglowekA': data.get('odpis', {}).get('naglowekA'), 'dane': {'dzial1': {'danePodmiotu': data.get('odpis', {}).get('dane', {}).get('dzial1', {}).get('danePodmiotu')}}}, '_evidence_note': 'Company identity projection only; personal records omitted.'}
        (OUT / (key + '.json')).write_text(json.dumps(data, ensure_ascii=False, indent=2) + '\n')
        record.update(body_file=key + '.json', upstream_sha256=hashlib.sha256(body).hexdigest(), top_level_fields=list(data) if isinstance(data, dict) else ['array'])
    except Exception as error:
        record['error'] = str(error)
    record['elapsed_seconds'] = round(time.monotonic() - start, 3)
    return record

if __name__ == '__main__':
    OUT.mkdir(parents=True, exist_ok=True)
    with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
        records = list(pool.map(get, URLS.items()))
    (OUT / 'probe-manifest.json').write_text(json.dumps(records, indent=2) + '\n')
    print(json.dumps(records, indent=2))
