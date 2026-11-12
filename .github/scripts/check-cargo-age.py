#!/usr/bin/env python3
"""Audit the submitted crates.io lock, including entries Cargo grandfathers."""
import concurrent.futures
import datetime as dt
import json
from pathlib import Path
import sys
import tomllib
import urllib.request

MIN_AGE = dt.timedelta(days=14)
SOURCE = 'registry+https://github.com/rust-lang/crates.io-index'

def check_record(record, now):
    stamp = record.get('pubtime')
    if not stamp:
        raise ValueError('missing pubtime')
    published = dt.datetime.fromisoformat(stamp.replace('Z', '+00:00'))
    if published.tzinfo is None:
        raise ValueError('pubtime has no timezone')
    if now - published < MIN_AGE:
        raise ValueError(f'publication younger than 14 days: {stamp}')

def audit(name, versions, now):
    key = ('1/' + name if len(name) == 1 else '2/' + name if len(name) == 2
           else '3/' + name[0] + '/' + name if len(name) == 3
           else name[:2] + '/' + name[2:4] + '/' + name)
    req = urllib.request.Request('https://index.crates.io/' + key,
                                 headers={'User-Agent': 'docsbase-cargo-age-audit'})
    with urllib.request.urlopen(req, timeout=60) as response:
        records = {r['vers']: r for line in response for r in [json.loads(line)]}
    for version, checksum in versions:
        record = records[version]
        if record['cksum'] != checksum:
            raise ValueError(f'{name}@{version}: checksum mismatch')
        try:
            check_record(record, now)
        except ValueError as error:
            raise ValueError(f'{name}@{version}: {error}') from error

def main():
    lock = Path(sys.argv[1] if len(sys.argv) > 1 else 'Cargo.lock')
    packages = tomllib.loads(lock.read_text())['package']
    groups = {}
    for package in packages:
        source = package.get('source')
        if source is None:  # workspace/path packages have no registry publication
            continue
        if source != SOURCE:
            raise ValueError(f'unsupported source: {package["name"]}: {source}')
        groups.setdefault(package['name'], []).append((package['version'], package['checksum']))
    if not groups:
        raise ValueError('no registry packages found')
    now = dt.datetime.now(dt.timezone.utc)
    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
        futures = [pool.submit(audit, name, versions, now) for name, versions in groups.items()]
        for future in futures:
            future.result()
    print(f'Cargo age audit OK: {sum(map(len, groups.values()))} registry entries >=14 days')

if __name__ == '__main__':
    main()
