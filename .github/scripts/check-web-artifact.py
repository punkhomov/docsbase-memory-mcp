#!/usr/bin/env python3
"""Smoke embedded Web assets and local session without Node or source files."""
import argparse
import json
import os
from pathlib import Path
import queue
import re
import subprocess
import tempfile
import threading
import urllib.request
import http.cookiejar


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix='docsbase-web-artifact-') as tmp:
        env = os.environ.copy()
        for name in ('CACHE', 'CONFIG', 'DATA'):
            env[f'DOCSBASE_{name}_DIR'] = str(Path(tmp) / name.lower())
        # The panel must not need Node; preserve Windows system tools only.
        tools = Path(tmp) / 'empty-path'
        tools.mkdir()
        env['PATH'] = os.path.join(env.get('SystemRoot', ''), 'System32') if os.name == 'nt' else str(tools)
        child = subprocess.Popen([str(binary), 'web'], cwd=tmp, env=env,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        lines = queue.Queue()
        threading.Thread(target=lambda: lines.put(child.stdout.readline()), daemon=True).start()
        errors = []
        threading.Thread(target=lambda: errors.extend(child.stderr.readlines()), daemon=True).start()
        try:
            line = lines.get(timeout=30)
            match = re.search(r'(http://127\.0\.0\.1:\d+)/#bootstrap=([a-f0-9]+)', line)
            if not match:
                raise RuntimeError('Web did not publish a launch URL: ' + ''.join(errors))
            base, token = match.groups()
            opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
            page = opener.open(base, timeout=10).read().decode()
            resources = re.findall(r'(?:src|href)="(/assets/[^"]+)"', page)
            assert resources, 'embedded JS/CSS missing'
            asset_bytes = sum(len(opener.open(base + route, timeout=10).read()) for route in resources)
            request = urllib.request.Request(base + '/api/v1/session', data=json.dumps({'token':token}).encode(), headers={'Content-Type':'application/json','Origin':base})
            opener.open(request, timeout=10).read()
            session = json.load(opener.open(base + '/api/v1/session', timeout=10))
            assert 'panel-read-v1' in session['daemon']['capabilities']
            assert json.load(opener.open(base + '/api/v1/projects', timeout=10)) == []
            print(f'Web artifact OK: executable={binary.stat().st_size} embedded-transferred={asset_bytes} bytes')
        finally:
            child.terminate()
            child.wait(timeout=10)
            subprocess.run([str(binary),'daemon','stop'],env=env,cwd=tmp,capture_output=True,timeout=10,check=False)


if __name__ == '__main__':
    main()
