"""Regression checks for CI diagnostics and failure propagation."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest


OBSERVER = Path(__file__).with_name("observe-tests.py")


class ObserverTests(unittest.TestCase):
    def observe(self, root, phase, code, extra_env=None):
        environment = os.environ.copy()
        environment["GITHUB_STEP_SUMMARY"] = str(root / "summary.md")
        environment["OPENROUTER_API_KEY"] = "not-a-real-secret-do-not-record"
        environment.update(extra_env or {})
        return subprocess.run(
            [sys.executable, str(OBSERVER), "--suite", "fixture", "--phase", phase,
             "--output-dir", str(root), "--", sys.executable, "-c", code],
            env=environment, capture_output=True, encoding="utf-8", check=False,
        )

    def test_non_latin_output_survives_a_legacy_stdout_encoding(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            result = self.observe(
                root, "execute",
                "import sys; sys.stdout.buffer.write('Ошибка: 日本語\\n'.encode('utf-8'))",
                {"PYTHONIOENCODING": "ascii:strict"},
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("Ошибка: 日本語", result.stdout)
            self.assertIn("Ошибка: 日本語", (root / "fixture/execute.log").read_text(encoding="utf-8"))
            report = json.loads((root / "fixture/execute.json").read_text(encoding="utf-8"))
            self.assertEqual(report["status"], "completed")
            self.assertEqual(report["exit_code"], 0)

    def test_child_failure_keeps_output_status_and_target_results(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            result = self.observe(root, "execute", """
print('\\x1b[32m     Running\\x1b[0m tests/example.rs (target/example)')
print('test failing_contract ... FAILED')
print('test result: FAILED. 2 passed; 1 failed; 3 ignored; 0 measured; 0 filtered out; finished in 1.25s')
raise SystemExit(7)
""")
            self.assertEqual(result.returncode, 7, result.stderr)
            self.assertIn("failing_contract", result.stdout)
            report = json.loads((root / "fixture/execute.json").read_text())
            self.assertEqual(report["status"], "completed")
            self.assertEqual(report["exit_code"], 7)
            self.assertGreaterEqual(report["elapsed_seconds"], 0)
            target = report["targets"][0]
            self.assertIn("tests/example.rs", target["target"])
            self.assertEqual(target["failed"], 1)
            self.assertEqual(target["ignored"], 3)
            self.assertEqual(target["elapsed_seconds"], 1.25)
            self.assertIn("failing_contract", (root / "fixture/execute.log").read_text())
            self.assertIn("exit 7", (root / "summary.md").read_text())
            self.assertNotIn("not-a-real-secret", (root / "fixture/execute.json").read_text())

    def test_inventory_records_names_per_target_and_doctests(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            result = self.observe(root, "inventory", """
print('     Running unittests src/lib.rs (target/lib)')
print('module::first: test')
print('1 test, 0 benchmarks')
print('   Doc-tests docsbase_memory')
print('src/lib.rs - example (line 12): test')
""")
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads((root / "fixture/inventory.json").read_text())
            self.assertEqual(report["inventory"], [
                {"target": "Running unittests src/lib.rs (target/lib)",
                 "name": "module::first", "kind": "test"},
                {"target": "Doc-tests docsbase_memory",
                 "name": "src/lib.rs - example (line 12)", "kind": "test"},
            ])

    def test_helper_subprocess_summary_does_not_double_count_parent_target(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            result = self.observe(root, "execute", """
print('     Running unittests src/lib.rs (target/lib)')
print('test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s')
print('test result: ok. 4 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 0.25s')
""")
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads((root / "fixture/execute.json").read_text())
            self.assertEqual(len(report["targets"]), 1)
            self.assertEqual(report["targets"][0]["passed"], 4)
            self.assertEqual(report["targets"][0]["elapsed_seconds"], 0.25)

    def test_missing_command_fails_with_diagnostics(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            result = subprocess.run(
                [sys.executable, str(OBSERVER), "--suite", "fixture", "--phase", "prepare",
                 "--output-dir", str(root), "--", str(root / "missing-executable")],
                capture_output=True, text=True, check=False,
                env={key: value for key, value in os.environ.items()
                     if key != "GITHUB_STEP_SUMMARY"},
            )
            self.assertEqual(result.returncode, 127, result.stderr)
            report = json.loads((root / "fixture/prepare.json").read_text())
            self.assertEqual(report["exit_code"], 127)
            self.assertIn("missing-executable", (root / "fixture/prepare.log").read_text())

    def test_heartbeat_saves_running_state_and_flushed_output_before_completion(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            release = root / "release"
            code = """
from pathlib import Path
import sys
import time
print('entered controlled wait', flush=True)
while not Path(sys.argv[1]).exists():
    time.sleep(0.01)
"""
            child = subprocess.Popen(
                [sys.executable, str(OBSERVER), "--suite", "fixture", "--phase", "execute",
                 "--output-dir", str(root), "--heartbeat-seconds", "0.05", "--",
                 sys.executable, "-c", code, str(release)],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                env={key: value for key, value in os.environ.items()
                     if key != "GITHUB_STEP_SUMMARY"},
            )
            try:
                deadline = time.monotonic() + 10
                report_path = root / "fixture/execute.json"
                while time.monotonic() < deadline:
                    if report_path.exists():
                        report = json.loads(report_path.read_text())
                        if report["last_output"] == "entered controlled wait":
                            break
                    time.sleep(0.01)
                else:
                    self.fail("observer did not checkpoint the controlled wait")
                self.assertEqual(report["status"], "running")
                self.assertIsNone(report["exit_code"])
                self.assertIn("entered controlled wait", (root / "fixture/execute.log").read_text())
            finally:
                release.touch()
                stdout, stderr = child.communicate(timeout=10)
            self.assertEqual(child.returncode, 0, stderr)
            self.assertIn("elapsed; last output", stdout)
            self.assertEqual(json.loads(report_path.read_text())["status"], "completed")


if __name__ == "__main__":
    unittest.main()
