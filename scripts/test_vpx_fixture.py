#!/usr/bin/env python3
"""Prepared encoder guards; synthetic files only, no subprocess or network."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import time
import unittest

spec = importlib.util.spec_from_file_location('fixture', Path(__file__).with_name('vpx_fixture.py'))
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


class PreparedGuards(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='tw-vpx-fixture-', dir='/private/tmp')
        self.stage = Path(self.temporary.name)
        (self.stage / 'source').mkdir()
        self.binary = self.stage / 'source/vpxenc'
        self.binary.write_bytes(b'never execute this fixture')
        (self.stage / 'source.tar.gz').write_bytes(b'not trusted source')
        (self.stage / 'receipt.json').write_text(json.dumps({'version': fixture.VERSION, 'flags': fixture.FLAGS, 'source': {'sha256': fixture.SHA}}))

    def tearDown(self):
        self.temporary.cleanup()

    def test_fifo_encoder_is_rejected_without_reading(self):
        self.binary.unlink()
        os.mkfifo(self.binary)
        started = time.monotonic()
        with self.assertRaisesRegex(RuntimeError, 'Invalid prepared encoder input'):
            fixture.checked(self.stage)
        self.assertLess(time.monotonic() - started, 1)

    def test_archive_symlink_is_rejected_and_target_preserved(self):
        archive = self.stage / 'source.tar.gz'
        archive.unlink()
        archive.symlink_to(self.binary)
        with self.assertRaisesRegex(RuntimeError, 'Symlink'):
            fixture.checked(self.stage)
        self.assertEqual(self.binary.read_bytes(), b'never execute this fixture')

    def test_changed_source_archive_never_reaches_encoder(self):
        with self.assertRaisesRegex(RuntimeError, 'provenance changed'):
            fixture.checked(self.stage)
        self.assertEqual(self.binary.read_bytes(), b'never execute this fixture')


if __name__ == '__main__':
    unittest.main()
