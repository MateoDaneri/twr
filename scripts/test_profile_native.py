"""Synthetic process/filesystem tests; no Instruments, GUI or real providers."""
import importlib.util
import json
import os
from pathlib import Path
import stat
import sys
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('profile_native', Path(__file__).with_name('profile_native.py'))
p = importlib.util.module_from_spec(spec)
spec.loader.exec_module(p)


class ProfileSafety(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='tw-profiler-test-', dir='/private/tmp')
        self.root = Path(self.tmp.name)
    def tearDown(self):
        self.tmp.cleanup()
    def run_child(self, source, seconds=2, budget=4096):
        p.supervised([sys.executable, '-I', '-S', '-c', source], self.root, seconds,
                     budget=budget, minimum_free=0)
    def test_success_has_private_output_and_no_inherited_secret(self):
        os.environ['TW_SYNTHETIC_SECRET'] = 'must-not-inherit'
        try:
            self.run_child("import os;print('TW_SYNTHETIC_SECRET' in os.environ)")
        finally:
            os.environ.pop('TW_SYNTHETIC_SECRET')
        log = self.root / 'tool.log'
        self.assertEqual(log.read_text().strip(), 'False')
        self.assertEqual(stat.S_IMODE(log.stat().st_mode), 0o600)
    def test_timeout_terminates_only_owned_worker(self):
        with self.assertRaisesRegex(RuntimeError, 'deadline'):
            self.run_child("import time;time.sleep(10)", seconds=.2)
        self.assertTrue((self.root / 'tool.log').exists())
    def test_output_size_limit_preserves_partial_evidence(self):
        with self.assertRaises(RuntimeError):
            self.run_child("import os;os.write(1,b'x'*16384)", budget=1024)
        self.assertLessEqual((self.root / 'tool.log').stat().st_size, 1024)
    def test_aggregate_file_budget(self):
        with self.assertRaisesRegex(RuntimeError, 'budget'):
            self.run_child("from pathlib import Path;Path('a').write_bytes(b'x'*800);Path('b').write_bytes(b'x'*800)", budget=1024)
        self.assertTrue((self.root / 'a').exists())
    def test_existing_outputs_are_never_replaced(self):
        (self.root / 'tool.log').write_text('evidence')
        with self.assertRaises(FileExistsError):
            self.run_child("print('new')")
        self.assertEqual((self.root / 'tool.log').read_text(), 'evidence')
        with self.assertRaises(FileExistsError):
            p.exclusive(self.root / 'tool.log', 'replacement')
    def test_symlink_destinations_refused(self):
        (self.root / 'alias').symlink_to(self.root, target_is_directory=True)
        with self.assertRaises(ValueError):
            p.private_dir(self.root / 'alias')
        (self.root / 'out').symlink_to(self.root / 'absent')
        with self.assertRaises(ValueError):
            p.exclusive(self.root / 'out', 'no')
    def test_unsafe_output_entry_and_permissions_refused(self):
        (self.root / 'link').symlink_to('/private/tmp')
        with self.assertRaises(ValueError):
            p.size_under(self.root)
        self.root.chmod(0o755)
        with self.assertRaises(ValueError):
            p.private_dir(self.root)
    def test_limits_cannot_be_disabled(self):
        for seconds, budget in [(136, 4096), (1, p.MAX_BYTES + 1), (0, 4096)]:
            with self.assertRaises(ValueError):
                self.run_child("pass", seconds, budget)


if __name__ == '__main__':
    unittest.main()
