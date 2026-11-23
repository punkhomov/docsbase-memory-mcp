"""Resolve every Cargo path override to its reviewed upstream source record."""
import json
from pathlib import Path
import tomllib


def load_upstream(root):
    root = Path(root).resolve()
    cargo = root / 'Cargo.toml'
    overrides = (tomllib.loads(cargo.read_text()).get('patch', {}).get('crates-io', {})
                 if cargo.exists() else {})
    paths = {name: value['path'] for name, value in overrides.items()
             if isinstance(value, dict) and 'path' in value}
    manifest = root / 'vendor' / 'UPSTREAM.json'
    records = json.loads(manifest.read_text()) if manifest.exists() else []
    names = [record['name'] for record in records]
    if len(names) != len(set(names)) or set(names) != set(paths):
        raise ValueError('vendored override coverage does not match Cargo path overrides')
    for record in records:
        name = record['name']
        actual = (root / paths[name]).resolve()
        expected = (root / 'vendor' / name).resolve()
        if actual != expected or not actual.is_relative_to(root / 'vendor'):
            raise ValueError(f'vendored override path mismatch: {name}')
        package = tomllib.loads((actual / 'Cargo.toml').read_text())['package']
        if package['name'] != name or package['version'] != record['version']:
            raise ValueError(f'vendored override package mismatch: {name}')
    return records
