"""Verify exact upstream crate archives plus the only reviewed local patches."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import urllib.request


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--archives', type=Path)
    args = parser.parse_args()
    base = Path(__file__).resolve().parent
    records = json.loads((base / 'UPSTREAM.json').read_text())
    with tempfile.TemporaryDirectory(prefix='docsbase-vendor-') as temporary:
        staging = Path(temporary)
        for record in records:
            name, version = record['name'], record['version']
            filename = f'{name}-{version}.crate'
            archive = staging / filename
            if args.archives:
                payload = (args.archives / filename).read_bytes()
            else:
                with urllib.request.urlopen(
                    f'https://static.crates.io/crates/{name}/{filename}', timeout=60
                ) as response:
                    payload = response.read()
            if hashlib.sha256(payload).hexdigest() != record['checksum']:
                raise ValueError(f'{name}: upstream archive checksum mismatch')
            archive.write_bytes(payload)
            with tarfile.open(archive) as source:
                source.extractall(staging, filter='data')
            expected = staging / f'{name}-{version}'
            patch = (base / 'patches' / f'{name}.patch').read_bytes()
            if hashlib.sha256(patch).hexdigest() != record['patch_sha256']:
                raise ValueError(f'{name}: reviewed patch checksum mismatch')
            subprocess.run(['git', 'apply', '--no-index', '-'], input=patch, cwd=expected, check=True)
            actual = base / name
            expected_files = {p.relative_to(expected): p.read_bytes() for p in expected.rglob('*') if p.is_file()}
            actual_files = {p.relative_to(actual): p.read_bytes() for p in actual.rglob('*') if p.is_file()}
            if expected_files != actual_files:
                changed = sorted(str(p) for p in expected_files.keys() | actual_files.keys()
                                 if expected_files.get(p) != actual_files.get(p))
                raise ValueError(f'{name}: unreviewed vendored source delta: {changed}')
            print(f'{name}@{version}: upstream checksum + exact reviewed patch + complete source tree OK')


if __name__ == '__main__':
    main()
