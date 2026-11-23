import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("installer", Path(__file__).with_name("collect-macos-installer.py"))
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)


class InstallerReports(unittest.TestCase):
    def test_two_objects_match_child_without_exporting_unrelated_fields(self):
        text = '{}\n' + json.dumps({"pid": 42, "procName": "docsbase", "termination": {"namespace": "CODESIGNING"}, "private": "sentinel"})
        self.assertEqual(installer.report_summary(text, {42})["termination"]["namespace"], "CODESIGNING")
        self.assertNotIn("private", installer.report_summary(text, {42}))
        self.assertIsNone(installer.report_summary(text, {43}))
        self.assertIsNone(installer.report_summary('{"pid":42,"procName":"other"}', {42}))

    def test_collection_retains_missing_and_malformed_evidence_honestly(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            out = root / 'observations'
            (out / 'cli-full').mkdir(parents=True)
            (out / 'cli-full/execute.log').write_text('installer child: test=Some("install") pid=42 program="/tmp/docsbase"\n')
            reports = root / 'reports'
            reports.mkdir()
            (reports / 'docsbase-invalid.ips').write_text('invalid')
            (reports / 'docsbase-valid.ips').write_text('{"pid":42,"procName":"docsbase"}')
            installer.collect(out, [reports, root / 'missing'])
            data = json.loads((out / 'macos-installer-crashes.json').read_text())
            self.assertEqual(data['installer_pids'], [42])
            self.assertEqual(len(data['reports']), 1)
            self.assertTrue(data['notes'])
