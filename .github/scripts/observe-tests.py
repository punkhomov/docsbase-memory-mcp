#!/usr/bin/env python3
"""Observe an unchanged Cargo command using only stable output and stdlib."""

import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import threading
import time


ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
RESULT = re.compile(
    r"^test result: \S+\. (\d+) passed; (\d+) failed; (\d+) ignored; "
    r"(\d+) measured; (\d+) filtered out; finished in ([\d.]+)s$"
)
PUBLIC_SETTINGS = (
    "GITHUB_SHA", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT", "GITHUB_JOB",
    "RUNNER_OS", "RUNNER_ARCH", "RUSTUP_TOOLCHAIN", "CARGO_BUILD_JOBS",
    "CARGO_TARGET_DIR", "DOCSBASE_BENCH_LATENCY_SLACK", "CI_RUST_CACHE_HIT",
)


def now():
    return datetime.now(timezone.utc).isoformat()


def observe(args):
    directory = args.output_dir / args.suite
    directory.mkdir(parents=True, exist_ok=True)
    report_path = directory / f"{args.phase}.json"
    started = time.monotonic()
    report = {
        "suite": args.suite, "phase": args.phase, "command": args.command,
        "started_at": now(), "status": "running", "exit_code": None,
        "platform": platform.platform(), "cpu_count": os.cpu_count(),
        "settings": {key: os.environ.get(key) for key in PUBLIC_SETTINGS},
        "elapsed_seconds": 0, "last_output": None, "targets": [], "inventory": [],
    }
    lock = threading.Lock()
    stopped = threading.Event()

    def checkpoint():
        # Called under lock once the heartbeat thread starts. Atomic replacement
        # leaves a readable earlier snapshot if the runner kills us mid-write.
        report["elapsed_seconds"] = round(time.monotonic() - started, 3)
        temporary = report_path.with_suffix(".json.tmp")
        temporary.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        temporary.replace(report_path)

    def heartbeat():
        while not stopped.wait(args.heartbeat_seconds):
            with lock:
                checkpoint()
                print(
                    f"[observe {args.suite}/{args.phase}] "
                    f"{report['elapsed_seconds']:.1f}s elapsed; "
                    f"last output: {report['last_output'] or '(none yet)'}",
                    flush=True,
                )

    checkpoint()
    with (directory / f"{args.phase}.log").open("w", encoding="utf-8") as log:
        try:
            child = subprocess.Popen(
                args.command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                text=True, encoding="utf-8", errors="replace", bufsize=1,
            )
        except OSError as error:
            message = f"Cannot start {args.command[0]}: {error}\n"
            log.write(message)
            print(message, end="", flush=True)
            report["last_output"] = message.strip()
            exit_code = 127
        else:
            ticker = threading.Thread(target=heartbeat, daemon=True)
            ticker.start()
            target = "unattributed"
            result_index = None
            try:
                for line in child.stdout:
                    log.write(line)
                    log.flush()
                    plain = ANSI.sub("", line).strip()
                    entry = re.fullmatch(r"(.+): (test|benchmark)", plain)
                    # Keep inventory names in the artifact without printing the
                    # entire suite twice. Compiler errors/progress stay visible.
                    if args.phase != "inventory" or not entry:
                        print(line, end="", flush=True)
                    with lock:
                        report["last_output"] = plain[-500:]
                        if plain.startswith(("Running ", "Doc-tests ")):
                            target = plain
                            result_index = None
                        if args.phase == "inventory":
                            if entry:
                                report["inventory"].append({
                                    "target": target, "name": entry[1], "kind": entry[2],
                                })
                        result = RESULT.fullmatch(plain)
                        if result:
                            counts = dict(zip(
                                ("passed", "failed", "ignored", "measured", "filtered_out"),
                                map(int, result.groups()[:5]),
                            ))
                            observation = {
                                "target": target, **counts,
                                "elapsed_seconds": float(result[6]),
                            }
                            # Helper subprocesses can print a summary inside
                            # the parent target. Its final summary supersedes
                            # that preliminary observation; do not double count.
                            if result_index is None:
                                result_index = len(report["targets"])
                                report["targets"].append(observation)
                            else:
                                report["targets"][result_index] = observation
                exit_code = child.wait()
            except BaseException:
                # Do not leave the directly observed process running if the
                # observer is interrupted. Runner cleanup handles descendants.
                child.terminate()
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
                raise
            finally:
                stopped.set()
                ticker.join()
                child.stdout.close()

    report.update(status="completed", exit_code=exit_code, ended_at=now())
    checkpoint()
    print(
        f"[observe {args.suite}/{args.phase}] exit {exit_code}; "
        f"{report['elapsed_seconds']:.3f}s; "
        f"{len(report['inventory'])} inventory entries; reports: {directory}",
        flush=True,
    )
    if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with Path(summary).open("a", encoding="utf-8") as output:
            output.write(
                f"### {args.suite}: {args.phase}\n\n"
                f"Wall time: {report['elapsed_seconds']:.3f}s; exit {exit_code}. "
                f"Inventory entries: {len(report['inventory'])}.\n\n"
                f"```text\n{json.dumps(args.command)}\n```\n\n"
            )
            if report["targets"]:
                output.write("Slowest test targets (libtest-reported execution time):\n\n")
                output.write("| Target | Seconds | Passed | Failed | Ignored |\n")
                output.write("|---|---:|---:|---:|---:|\n")
                for item in sorted(report["targets"], key=lambda item: -item["elapsed_seconds"])[:10]:
                    name = item["target"].replace("|", "\\|").replace("`", "")
                    output.write(
                        f"| {name} | {item['elapsed_seconds']:.2f} | {item['passed']} "
                        f"| {item['failed']} | {item['ignored']} |\n"
                    )
                output.write("\n")
    return exit_code


def main():
    # GitHub captures redirected stdout; Windows may otherwise choose a legacy
    # codepage even though Rust emits UTF-8. Diagnostics must not stop Cargo.
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--suite", required=True)
    parser.add_argument("--phase", required=True, choices=("prepare", "inventory", "execute"))
    parser.add_argument("--output-dir", type=Path, default=Path("target/ci-observations"))
    parser.add_argument("--heartbeat-seconds", type=float, default=30)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]*", args.suite):
        parser.error("suite must contain lowercase letters, digits or hyphens")
    if args.command[:1] == ["--"]:
        args.command = args.command[1:]
    if not args.command or args.heartbeat_seconds <= 0:
        parser.error("a command and positive heartbeat interval are required")
    raise SystemExit(observe(args))


if __name__ == "__main__":
    main()
