#!/usr/bin/env python3
"""Offline checks of asset integrity and renderer isolation; no tools launched."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

HELPERS = Path(__file__).resolve().parent.parent / 'Resources/Helpers'
def module(name):
    spec = importlib.util.spec_from_file_location(name, HELPERS / (name + '.py'))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value
assets = module('tex_assets')
renderer = module('render_preview')

class Assets(unittest.TestCase):
    def test_integrity_and_pointer_are_bound_to_owned_cache(self):
        with tempfile.TemporaryDirectory(prefix='tw-assets-check-') as temporary:
            state = Path(temporary).resolve()
            root = state / 'tex-assets'
            root.mkdir(mode=0o700)
            directory = root / 'assets-test'
            directory.mkdir(mode=0o700)
            cache = directory / 'cache'
            cache.mkdir()
            (cache / 'format').write_bytes(b'synthetic format')
            binary = state / 'tool'
            binary.write_bytes(b'synthetic tool')
            native = [{'path': str(binary), 'sha256': assets.digest(binary)}]
            receipt = {'recipe_sha256': hashlib.sha256(assets.SOURCE.encode()).hexdigest(), 'bundle': assets.BUNDLE, 'tectonic': str(binary), 'converter': str(binary), 'runtime_files': native, 'cache_files': assets.cache_manifest(cache)}
            (directory / 'receipt.json').write_text(json.dumps(receipt))
            assets.pointer(root, directory.name)
            with patch.object(assets, 'tools', return_value=(binary, binary)), patch.object(assets, 'runtime_files', return_value=[binary]):
                self.assertEqual(assets.load(state)[0], cache)
                (cache / 'extra').write_bytes(b'unexpected')
                with self.assertRaisesRegex(ValueError, 'cambió'):
                    assets.load(state)
            assets.pointer(root, '../escape')
            with self.assertRaisesRegex(ValueError, 'inválido'):
                assets.load(state)

    def test_symlinks_and_nonprivate_state_fail_closed(self):
        with tempfile.TemporaryDirectory(prefix='tw-assets-check-') as temporary:
            state = Path(temporary).resolve()
            cache = state / 'cache'
            cache.mkdir()
            (cache / 'linked').symlink_to(state)
            with self.assertRaisesRegex(ValueError, 'Symlink'):
                assets.cache_manifest(cache)
            state.chmod(0o755)
            with self.assertRaisesRegex(ValueError, '0700'):
                assets.private(state)
            state.chmod(0o700)

    def test_document_profile_denies_network_and_writes_to_cache(self):
        output = Path('/private/tmp/output-with-"quote')
        cache = Path('/private/tmp/sealed-cache')
        profile = renderer.sandbox_profile(output, Path('/private/tmp/project'), [Path('/opt/tool')], cache)
        self.assertIn('(deny network*)', profile)
        self.assertIn('(deny file-write*)', profile)
        self.assertIn('(allow file-write* (subpath ' + json.dumps(str(output)) + '))', profile)
        self.assertNotIn('(allow file-write* (subpath ' + json.dumps(str(cache)), profile)
        self.assertIn('(literal "/opt/tool")', profile)
        self.assertIn('(allow file-read* (literal "/"))', profile)
        self.assertNotIn('(subpath "/")', profile)
        bootstrap = renderer.sandbox_profile(output, output, [Path('/opt/tool')], network=True)
        self.assertNotIn('(deny network*)', bootstrap)
        self.assertIn('(deny file-read*)', bootstrap)
        self.assertNotIn(str(Path.home()), bootstrap)
        browser = renderer.sandbox_profile(output, Path('/private/tmp/project'), browser=True)
        self.assertIn('(deny network*)', browser)
        self.assertIn('(remote unix-socket (subpath ' + json.dumps(str(output)) + '))', browser)
        self.assertNotIn('(allow network-outbound (remote ip', browser)
        self.assertNotIn('unix-socket', profile)

if __name__ == '__main__':
    unittest.main()
