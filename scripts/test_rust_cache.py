#!/usr/bin/env python3
"""Synthetic cache metadata only; never invokes Cargo or deletes build artifacts."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location('rust_workspace', Path(__file__).with_name('rust_workspace.py'))
workspace = importlib.util.module_from_spec(spec)
spec.loader.exec_module(workspace)


class CacheTag(unittest.TestCase):
    def test_tag_is_created_once_without_replacing_existing_metadata(self):
        with tempfile.TemporaryDirectory(dir='/private/tmp') as temp:
            root = Path(temp) / 'cache'
            with mock.patch.object(workspace, 'BUILD', root):
                workspace.prepare_build()
                tag = root / 'target/CACHEDIR.TAG'
                tag.write_bytes(workspace.CACHE_TAG + b'# retained marker\n')
                workspace.prepare_build()
                self.assertTrue(tag.read_bytes().endswith(b'# retained marker\n'))

    def test_invalid_tag_is_preserved(self):
        with tempfile.TemporaryDirectory(dir='/private/tmp') as temp:
            root = Path(temp)
            (root / 'target').mkdir()
            tag = root / 'target/CACHEDIR.TAG'
            tag.write_bytes(b'own synthetic evidence')
            with mock.patch.object(workspace, 'BUILD', root):
                with self.assertRaisesRegex(RuntimeError, 'invalid'):
                    workspace.prepare_build()
            self.assertEqual(tag.read_bytes(), b'own synthetic evidence')

    def test_target_and_tag_symlinks_are_rejected(self):
        for link_tag in [False, True]:
            with tempfile.TemporaryDirectory(dir='/private/tmp') as temp:
                root = Path(temp)
                outside = root / 'own-sentinel'
                outside.mkdir()
                sentinel = outside / 'CACHEDIR.TAG'
                sentinel.write_bytes(b'unchanged')
                if link_tag:
                    (root / 'target').mkdir()
                    (root / 'target/CACHEDIR.TAG').symlink_to(sentinel)
                else:
                    (root / 'target').symlink_to(outside, target_is_directory=True)
                with mock.patch.object(workspace, 'BUILD', root):
                    with self.assertRaises(RuntimeError):
                        workspace.prepare_build()
                self.assertEqual(sentinel.read_bytes(), b'unchanged')


if __name__ == '__main__':
    unittest.main()
