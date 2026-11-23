#!/usr/bin/env python3
"""Best-effort bounded .ips evidence for installer children, never a test retry."""
import itertools
import json
from pathlib import Path
import re
import time

LIMIT = 8 * 1024 * 1024


def report_summary(text, pids):
    # Apple's .ips format can have a metadata object followed by a report object.
    decoder = json.JSONDecoder()
    first, end = decoder.raw_decode(text.lstrip())
    remaining = text.lstrip()[end:].strip()
    report = decoder.raw_decode(remaining)[0] if remaining else first
    if not isinstance(report, dict) or report.get("pid") not in pids:
        return None
    if report.get("procName") != "docsbase":
        return None
    return {key: report.get(key) for key in (
        "pid", "procName", "procPath", "captureTime", "parentProc", "parentPid",
        "exception", "termination", "osVersion",
    )}


def collect(output, roots):
    pids = set()
    notes = []
    for log in itertools.islice(output.glob("**/execute.log"), 16):
        with log.open("rb") as stream:
            data = stream.read(LIMIT + 1)
        if len(data) > LIMIT:
            notes.append(f"log exceeds byte limit: {log.name}")
            continue
        pids.update(int(pid) for pid in re.findall(rb"installer child: test=.*? pid=(\d+) program=", data))
    reports = []
    if pids:
        for root in roots:
            try:
                for path in itertools.islice(root.glob("docsbase*.ips"), 256):
                    try:
                        if path.is_symlink() or path.stat().st_mtime < time.time() - 7200:
                            continue
                        with path.open("rb") as stream:
                            data = stream.read(LIMIT + 1)
                        if len(data) > LIMIT:
                            notes.append(f"report exceeds byte limit: {path.name}")
                            continue
                        report = report_summary(data.decode("utf-8"), pids)
                        if report is not None:
                            reports.append({"file": path.name, **report})
                        if len(reports) >= 16:
                            break
                    except (OSError, ValueError) as error:
                        notes.append(f"unreadable report: {path.name}: {type(error).__name__}")
            except OSError as error:
                notes.append(f"report directory unavailable: {type(error).__name__}")
            if len(reports) >= 16:
                break
    output.mkdir(parents=True, exist_ok=True)
    result = {"installer_pids": sorted(pids), "reports": reports, "notes": notes,
              "limit": "16 reports; 256 candidates per directory; 8 MiB per input; last 2 hours",
              "interpretation": "PID/time/name candidates only; confirm path/time before attributing cause. No report is not evidence of no crash."}
    (output / "macos-installer-crashes.json").write_text(json.dumps(result, indent=2) + "\n")
    print(f"Installer diagnostics: {len(pids)} child PIDs, {len(reports)} candidate reports")


if __name__ == "__main__":
    collect(Path("target/ci-observations"), [
        Path.home() / "Library/Logs/DiagnosticReports",
        Path("/Library/Logs/DiagnosticReports"),
    ])
