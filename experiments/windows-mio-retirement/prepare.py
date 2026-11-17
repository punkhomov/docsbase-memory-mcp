"""Prepare checksum-verified locked source and apply the experimental patches."""

import hashlib
from pathlib import Path
import shutil
import subprocess
import tarfile
import tomllib
import urllib.request


def main():
    base = Path(__file__).resolve().parent
    root = base.parents[1]
    packages = tomllib.loads((root / "Cargo.lock").read_text())["package"]
    for name, version in [("mio", "1.2.4"), ("tokio", "1.54.1")]:
        locked = next(p for p in packages if p["name"] == name and p["version"] == version)
        archive = base / "archives" / f"{name}-{version}.crate"
        archive.parent.mkdir(exist_ok=True)
        if not archive.exists():
            with urllib.request.urlopen(
                f"https://static.crates.io/crates/{name}/{name}-{version}.crate"
            ) as response:
                archive.write_bytes(response.read())
        actual = hashlib.sha256(archive.read_bytes()).hexdigest()
        if actual != locked["checksum"]:
            raise RuntimeError(f"archive checksum mismatch: {name}")
        destination = base / "vendor" / name
        if destination.exists():
            raise RuntimeError(f"refusing to overwrite existing generated source: {destination}")
        staging = base / "vendor" / f"{name}-{version}"
        with tarfile.open(archive) as source:
            source.extractall(base / "vendor", filter="data")
        shutil.move(staging, destination)
        subprocess.run(
            ["git", "apply", "--no-index", "--unsafe-paths", str(base / f"{name}.patch")],
            cwd=destination,
            check=True,
        )
        print(f"prepared {name}@{version}, archive sha256={actual}", flush=True)


if __name__ == "__main__":
    main()
