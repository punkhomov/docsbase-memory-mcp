import datetime as dt
import importlib.util
from pathlib import Path
import unittest
spec = importlib.util.spec_from_file_location('age', Path(__file__).with_name('check-cargo-age.py'))
age = importlib.util.module_from_spec(spec)
spec.loader.exec_module(age)
class CargoAgeTests(unittest.TestCase):
    def test_exact_boundary(self):
        age.check_record({'pubtime':'2026-10-30T12:00:00Z'}, dt.datetime(2026,11,13,12,tzinfo=dt.timezone.utc))
    def test_fresh_locked_version_is_rejected(self):
        with self.assertRaises(ValueError):
            age.check_record({'pubtime':'2026-10-30T12:00:01Z'}, dt.datetime(2026,11,13,12,tzinfo=dt.timezone.utc))
    def test_missing_invalid_and_future_dates_fail(self):
        now=dt.datetime(2026,11,13,tzinfo=dt.timezone.utc)
        for value in [None, '', 'invalid', '2026-10-01T00:00:00', '2027-01-01T00:00:00Z']:
            with self.subTest(value=value), self.assertRaises(ValueError):
                age.check_record({'pubtime':value}, now)

class AuditIntegrationTests(unittest.TestCase):
    def test_locked_record_and_checksum_validation(self):
        import io
        from unittest.mock import patch
        now=dt.datetime(2026,11,13,tzinfo=dt.timezone.utc)
        for record, version, checksum, succeeds in [
            ({'vers':'1.0.0','cksum':'ok','pubtime':'2026-10-01T00:00:00Z'},'1.0.0','ok',True),
            ({'vers':'1.0.0','cksum':'ok','pubtime':'2026-11-12T00:00:00Z'},'1.0.0','ok',False),
            ({'vers':'1.0.0','cksum':'bad','pubtime':'2026-10-01T00:00:00Z'},'1.0.0','ok',False),
            ({'vers':'1.0.0','cksum':'ok'},'1.0.0','ok',False),
            ({'vers':'1.0.0','cksum':'ok'},'2.0.0','ok',False),
        ]:
            with self.subTest(record=record, version=version), patch.object(age.urllib.request,'urlopen',return_value=io.BytesIO(age.json.dumps(record).encode())):
                if succeeds: age.audit('example',[(version,checksum)],now)
                else:
                    with self.assertRaises((ValueError, KeyError)): age.audit('example',[(version,checksum)],now)
        with patch.object(age.urllib.request,'urlopen',side_effect=OSError('offline')):
            with self.assertRaises(OSError): age.audit('example',[('1','x')],now)

    def test_main_covers_all_locked_versions_and_rejects_sources(self):
        import tempfile
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as directory:
            lock=Path(directory)/'Cargo.lock'
            lock.write_text('[[package]]\nname="workspace"\nversion="0.1"\n'+''.join(f'[[package]]\nname="dep"\nversion="{v}"\nsource="{age.SOURCE}"\nchecksum="c{v}"\n' for v in ['1','2']))
            with patch.object(age.sys,'argv',['audit',str(lock)]), patch.object(age,'audit') as audit:
                age.main()
                self.assertEqual(audit.call_count,1)
                self.assertEqual(audit.call_args.args[:2],('dep',[('1','c1'),('2','c2')]))
            lock.write_text('[[package]]\nname="dep"\nversion="1"\nsource="git+https://example.invalid/repo"\n')
            with patch.object(age.sys,'argv',['audit',str(lock)]):
                with self.assertRaises(ValueError): age.main()


class VendoredAgeTests(unittest.TestCase):
    def test_override_manifest_rejects_duplicate_wrong_package_and_wrong_path(self):
        import json
        import tempfile
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'Cargo.toml').write_text('[patch.crates-io]\nmio={path="vendor/mio"}\n')
            vendored = root / 'vendor' / 'mio'
            vendored.mkdir(parents=True)
            package = vendored / 'Cargo.toml'
            package.write_text('[package]\nname="mio"\nversion="1.2.4"\n')
            manifest = root / 'vendor' / 'UPSTREAM.json'
            record = {'name': 'mio', 'version': '1.2.4', 'checksum': 'archive'}
            for records in [[], [record, record], [dict(record, version='1.2.3')]]:
                manifest.write_text(json.dumps(records))
                with self.subTest(records=records), self.assertRaises(ValueError):
                    age._manifest.load_upstream(root)
            manifest.write_text(json.dumps([record]))
            package.write_text('[package]\nname="other"\nversion="1.2.4"\n')
            with self.assertRaisesRegex(ValueError, 'package mismatch'):
                age._manifest.load_upstream(root)
            (root / 'Cargo.toml').write_text('[patch.crates-io]\nmio={path="somewhere-else"}\n')
            with self.assertRaisesRegex(ValueError, 'path mismatch'):
                age._manifest.load_upstream(root)

    def test_missing_override_record_is_rejected_before_network_audit(self):
        import json
        import tempfile
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            lock = root / 'Cargo.lock'
            lock.write_text('[[package]]\nname="dep"\nversion="1"\nsource="' + age.SOURCE + '"\nchecksum="registry"\n[[package]]\nname="mio"\nversion="1.2.4"\n[[package]]\nname="tokio"\nversion="1.54.1"\n')
            (root / 'Cargo.toml').write_text('[patch.crates-io]\nmio={path="vendor/mio"}\ntokio={path="vendor/tokio"}\n')
            (root / 'vendor').mkdir()
            (root / 'vendor' / 'UPSTREAM.json').write_text(json.dumps([
                {'name': 'mio', 'version': '1.2.4', 'checksum': 'original-archive'}
            ]))
            with patch.object(age.sys, 'argv', ['audit', str(lock)]), patch.object(age, 'audit') as audit:
                with self.assertRaisesRegex(ValueError, 'override coverage'):
                    age.main()
                audit.assert_not_called()

    def test_path_override_keeps_original_publication_age_audit(self):
        import json
        import tempfile
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            lock = root / 'Cargo.lock'
            lock.write_text('[[package]]\nname="dep"\nversion="1"\nsource="' + age.SOURCE + '"\nchecksum="registry"\n[[package]]\nname="mio"\nversion="1.2.4"\n')
            (root / 'Cargo.toml').write_text('[patch.crates-io]\nmio={path="vendor/mio"}\n')
            (root / 'vendor').mkdir()
            (root / 'vendor' / 'mio').mkdir()
            (root / 'vendor' / 'mio' / 'Cargo.toml').write_text('[package]\nname="mio"\nversion="1.2.4"\n')
            (root / 'vendor' / 'UPSTREAM.json').write_text(json.dumps([
                {'name': 'mio', 'version': '1.2.4', 'checksum': 'original-archive'}
            ]))
            with patch.object(age.sys, 'argv', ['audit', str(lock)]), patch.object(age, 'audit') as audit:
                age.main()
                observed = {call.args[0]: call.args[1] for call in audit.call_args_list}
                self.assertEqual(observed['mio'], [('1.2.4', 'original-archive')])
                self.assertEqual(observed['dep'], [('1', 'registry')])
            lock.write_text('[[package]]\nname="dep"\nversion="1"\nsource="' + age.SOURCE + '"\nchecksum="registry"\n')
            with patch.object(age.sys, 'argv', ['audit', str(lock)]):
                with self.assertRaisesRegex(ValueError, 'vendored source absent from lock'):
                    age.main()
