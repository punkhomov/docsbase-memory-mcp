"""Native probe supervisor: require a sensitive RED and six clean GREEN cases."""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time


BASE = Path(__file__).resolve().parent
OUT = BASE / "evidence"


def run_case(name, command, env, timeout):
    began = time.monotonic()
    try:
        result = subprocess.run(command, cwd=BASE, env=env, capture_output=True, timeout=timeout)
    except subprocess.TimeoutExpired as error:
        (OUT / f"{name}.timeout.json").write_text(
            json.dumps({"timeout_seconds": timeout, "forced_cleanup": True})
        )
        (OUT / f"{name}.stdout.log").write_bytes(error.stdout or b"")
        (OUT / f"{name}.stderr.log").write_bytes(error.stderr or b"")
        raise RuntimeError(f"{name} watchdog expired; this is not acceptance") from error
    (OUT / f"{name}.stdout.log").write_bytes(result.stdout)
    (OUT / f"{name}.stderr.log").write_bytes(result.stderr)
    (OUT / f"{name}.exit.json").write_text(
        json.dumps({"exit_code": result.returncode, "elapsed_seconds": time.monotonic() - began})
    )
    print(result.stdout.decode(errors="replace"), flush=True)
    print(result.stderr.decode(errors="replace"), flush=True)
    return result


def main():
    if sys.platform != "win32":
        raise RuntimeError("native Windows is required; compile-only checks do not qualify")
    OUT.mkdir(exist_ok=True)
    env = dict(os.environ, CARGO_TERM_COLOR="never", RUSTUP_TOOLCHAIN="1.100.0")
    env.pop("PROBE_LOGICAL_GUARD", None)
    provenance = {
        "head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=BASE).decode().strip(),
        "tree": subprocess.check_output(["git", "rev-parse", "HEAD^{tree}"], cwd=BASE).decode().strip(),
        "rustc": subprocess.check_output(["rustc", "-Vv"], env=env).decode(),
        "patches": {
            name: hashlib.sha256((BASE / name).read_bytes()).hexdigest()
            for name in ["mio.patch", "tokio.patch"]
        },
    }
    (OUT / "provenance.json").write_text(json.dumps(provenance, indent=2))
    built = run_case(
        "build",
        ["cargo", "test", "--locked", "--no-run", "--message-format=json"],
        env,
        300,
    )
    if built.returncode:
        raise RuntimeError("probe build failed")
    artifacts = [json.loads(line) for line in built.stdout.splitlines() if line.startswith(b"{")]
    executables = [
        item["executable"]
        for item in artifacts
        if item.get("reason") == "compiler-artifact"
        and item.get("executable")
        and item.get("profile", {}).get("test")
    ]
    if len(executables) != 1:
        raise RuntimeError(f"expected exactly one test executable, got {executables}")
    executable = executables[0]
    (OUT / "executable-sha256.txt").write_text(hashlib.sha256(Path(executable).read_bytes()).hexdigest())
    listed = run_case("inventory", [executable, "--list"], env, 30)
    if listed.returncode or sum(line.endswith(b": test") for line in listed.stdout.splitlines()) != 6:
        raise RuntimeError("expected exactly six nonempty native cases")
    red = run_case(
        "logical-attachment-red",
        [executable, "--exact", "tests::original_admission_survives_logical_drop_until_real_mio_retirement", "--nocapture"],
        dict(env, PROBE_LOGICAL_GUARD="1"),
        90,
    )
    if (
        red.returncode != 101
        or b"cleanup_live=0 cleanup_slots=1" not in red.stdout
        or b"original charge must follow physical ownership" not in red.stderr
        or b"0 passed; 1 failed" not in red.stdout
    ):
        raise RuntimeError("attachment sensitivity was not the intended cleaned-up RED")
    green = run_case("native-green", [executable, "--nocapture", "--test-threads=1"], env, 120)
    if green.returncode or b"6 passed; 0 failed; 0 ignored" not in green.stdout:
        raise RuntimeError("six-case native acceptance failed")
    print("Probe result: cleaned-up attachment RED and six native GREEN cases; not production acceptance.", flush=True)


if __name__ == "__main__":
    main()
