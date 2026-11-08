#!/usr/bin/env python3
"""Check provider handlers in the actual CLI binary without network or models."""

import argparse
import os
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    failures = []
    with tempfile.TemporaryDirectory(prefix="docsbase-cli-smoke-") as temporary:
        root = Path(temporary)
        environment = os.environ.copy()
        for name in ("OPENROUTER_API_KEY", "ORT_DYLIB_PATH"):
            environment.pop(name, None)
        for name in ("CACHE", "CONFIG", "DATA"):
            path = root / name.lower()
            path.mkdir()
            environment[f"DOCSBASE_{name}_DIR"] = str(path)

        def run(*arguments):
            return subprocess.run(
                [str(binary), *arguments],
                cwd=root,
                env=environment,
                capture_output=True,
                text=True,
                timeout=30,
                check=False,
            )

        local = run("models", "fetch", "--help")
        if local.returncode != 0 or "fetch" not in local.stdout:
            failures.append(f"local model handler missing:\n{local.stdout}{local.stderr}")

        remote = run("connect", "openrouter", "--check")
        # An enabled provider reaches credential validation. The key is
        # deliberately absent, so no request or config write can occur.
        if remote.returncode == 0 or "OPENROUTER_API_KEY" not in remote.stderr:
            failures.append(
                f"remote provider did not reach credential validation:\n"
                f"{remote.stdout}{remote.stderr}"
            )

    if failures:
        raise SystemExit("\n".join(failures))
    print(f"full CLI capabilities OK: {binary}")


if __name__ == "__main__":
    main()
