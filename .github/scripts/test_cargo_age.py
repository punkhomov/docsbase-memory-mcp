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
