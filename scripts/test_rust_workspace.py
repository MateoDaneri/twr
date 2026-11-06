#!/usr/bin/env python3
"""Offline Cargo argument regression; no builds, dotfiles or GUI launches."""
import importlib.util
from pathlib import Path
import unittest
from unittest import mock
import tempfile
import shutil
import contextlib
import io
import json
import os
import subprocess

spec = importlib.util.spec_from_file_location('driver', Path(__file__).with_name('rust_workspace.py'))
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)


snapshot_spec = importlib.util.spec_from_file_location('source_snapshot', Path(__file__).with_name('source_snapshot.py'))
source_snapshot = importlib.util.module_from_spec(snapshot_spec)
snapshot_spec.loader.exec_module(source_snapshot)


app_spec = importlib.util.spec_from_file_location('rust_app', Path(__file__).with_name('rust_app.py'))
rust_app = importlib.util.module_from_spec(app_spec)
app_spec.loader.exec_module(rust_app)


class GhosttyArguments(unittest.TestCase):
    def test_old_snapshot_mtime_is_rejected_before_cargo(self):
        with tempfile.TemporaryDirectory(dir='/private/tmp') as directory:
            root = Path(directory)
            source = root / 'source'
            source.mkdir()
            file = source / 'main.rs'
            file.write_text('fixture')
            (source / 'source-manifest.json').write_text(json.dumps({'main.rs': 'fixture'}))
            fingerprint = root / 'target/debug/.fingerprint/workspace-app-fixture'
            fingerprint.mkdir(parents=True)
            artifact = fingerprint / 'invoked.timestamp'
            artifact.write_text('')
            os.utime(file, ns=(1_000_000_000, 1_000_000_000))
            os.utime(artifact, ns=(2_000_000_000, 2_000_000_000))
            with self.assertRaisesRegex(RuntimeError, 'timestamps are not newer'):
                driver.verify_fresh_source_times(source, root / 'target')
            os.utime(file, ns=(3_000_000_000, 3_000_000_000))
            driver.verify_fresh_source_times(source, root / 'target')

    def test_packaging_does_not_start_while_build_lock_is_held(self):
        with tempfile.TemporaryDirectory(dir='/private/tmp') as directory:
            build = Path(directory)
            with mock.patch.object(rust_app.driver, 'BUILD', build), \
                 mock.patch.object(rust_app.driver, 'prepare_build'), \
                 mock.patch.object(rust_app, 'package_locked', return_value='fixture') as inner:
                with driver.exclusive_build_lock(build):
                    with self.assertRaisesRegex(RuntimeError, 'Another build driver'):
                        rust_app.package(staged=True)
                    inner.assert_not_called()
                self.assertEqual(rust_app.package(staged=True), 'fixture')
                inner.assert_called_once_with(False, True, False, False, None)

    def test_driver_lock_rejects_a_second_owner(self):
        with tempfile.TemporaryDirectory(dir='/private/tmp') as directory:
            build = Path(directory)
            with driver.exclusive_build_lock(build):
                with self.assertRaisesRegex(RuntimeError, 'Another build driver'):
                    with driver.exclusive_build_lock(build):
                        self.fail('second driver acquired the lock')
            with driver.exclusive_build_lock(build):
                pass

    def test_fresh_snapshots_preserve_identity_and_never_reuse_paths(self):
        source = Path(__file__).resolve().parents[1]
        with tempfile.TemporaryDirectory(dir='/private/tmp') as directory:
            cache = Path(directory) / 'cache'
            with contextlib.redirect_stdout(io.StringIO()):
                source_snapshot.snapshot(source, cache, fresh=True)
                first = Path(json.loads((cache / 'latest.json').read_text())['path'])
                source_snapshot.snapshot(source, cache, fresh=True)
                second = Path(json.loads((cache / 'latest.json').read_text())['path'])
            self.assertNotEqual(first, second)
            self.assertEqual(first.name.split('-')[0], second.name.split('-')[0])
            self.assertEqual((first / 'source-manifest.json').read_bytes(), (second / 'source-manifest.json').read_bytes())
            self.assertTrue((first / 'crates/workspace-app/src/tmux.rs').is_file())
            self.assertTrue((second / 'crates/workspace-app/src/tmux.rs').is_file())

    def test_fresh_source_rebuilds_when_returning_to_earlier_content(self):
        cargo = driver.PINNED_BIN / 'cargo'
        if not cargo.is_file():
            self.skipTest('private Cargo toolchain unavailable')
        with tempfile.TemporaryDirectory(dir='/private/tmp') as directory:
            root = Path(directory)
            env = dict(os.environ, CARGO_HOME=str(driver.TOOLS / 'cargo'),
                       CARGO_TARGET_DIR=str(root / 'target'), RUSTC=str(driver.PINNED_BIN / 'rustc'),
                       CARGO_INCREMENTAL='0')
            def make(name, value):
                path = root / name
                (path / 'src').mkdir(parents=True)
                (path / 'Cargo.toml').write_text('[package]\nname="probe"\nversion="0.1.0"\nedition="2021"\n')
                (path / 'src/main.rs').write_text('fn main() { println!("' + value + '"); }\n')
                return path
            def run(path):
                return subprocess.run([str(cargo), 'run', '--offline', '--quiet'], cwd=path,
                                      env=env, check=True, capture_output=True, text=True, timeout=30).stdout.strip()
            first = make('a', 'A')
            self.assertEqual(run(first), 'A')
            self.assertEqual(run(make('b', 'B')), 'B')
            # Replicate the old source-content identity using newly written files.
            self.assertEqual(run(make('fresh-a', 'A')), 'A')

    def test_navigation_contract_change_invalidates_release_identity(self):
        # Real build inputs in a synthetic tree; no compiler, profiles or user data.
        source = Path(__file__).resolve().parents[1]
        before, files = driver.native_fingerprint(source)
        self.assertIn('Resources/Fixtures/native-tmux.conf', files)
        case_fixtures = ('tmux-control-output.json', 'nvim-case-line-fold.json', 'nvim-case-line-operator.json', 'nvim-case-line-repeat.json',
                         'nvim-case-line-after-failed.json', 'nvim-case-line-goto.json')
        for name in case_fixtures:
            self.assertIn('Resources/Fixtures/' + name, files)
        self.assertIn('Resources/Fixtures/nvim-wrap-short-matrix.json', files)
        self.assertIn('Resources/Fixtures/nvim-wrap-control-scroll.json', files)
        self.assertIn('Resources/Fixtures/nvim-wrap-mixed-matrix.json', files)
        self.assertIn('Resources/Fixtures/nvim-visual-insert.json', files)
        self.assertIn('Resources/Fixtures/nvim-picker-input.json', files)
        self.assertIn('Resources/Fixtures/nvim-window-focus.json', files)
        self.assertIn('Resources/Fixtures/nvim-window-count.json', files)
        self.assertIn('Resources/Fixtures/nvim-list-extremes.json', files)
        self.assertIn('Resources/Fixtures/nvim-editor-panes.rs', files)
        self.assertIn('Resources/Fixtures/nvim-editor-panes-independent.rs', files)
        self.assertIn('Resources/Fixtures/nvim-editor-panes-independent.json', files)
        self.assertIn('Resources/Fixtures/nvim-fold-indent-shift-reference.json', files)
        self.assertIn('Resources/Fixtures/nvim-editor-pane-focus.rs', files)
        self.assertIn('Resources/Fixtures/nvim-buffer-views.json', files)
        self.assertIn('Resources/Fixtures/nvim-snacks-unlisted.json', files)
        self.assertIn('Resources/Fixtures/nvim-unnamed-reopen-reference.json', files)
        self.assertIn('Resources/Fixtures/nvim-unnamed-saveas-reference.json', files)
        self.assertIn('Resources/Fixtures/nvim-bdelete-layout-reference.json', files)
        self.assertIn('Resources/Fixtures/nvim-bdelete-target-reference.json', files)
        self.assertIn('Resources/Fixtures/nvim-bdelete-terminal-reference.json', files)
        self.assertIn('Resources/Fixtures/nvim-terminal-q-reference.json', files)
        self.assertIn('Resources/Fixtures/nvim-buffer-columns.json', files)
        self.assertIn('Resources/Fixtures/snacks-focus-reference.json', files)
        self.assertIn('Resources/Fixtures/snacks-paging-reference.json', files)
        with tempfile.TemporaryDirectory(dir='/private/tmp') as directory:
            copy = Path(directory)
            for relative in files:
                target = copy / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(source / relative, target)
            self.assertEqual(driver.native_fingerprint(copy)[0], before)
            for name in case_fixtures:
                relative = 'Resources/Fixtures/' + name
                fixture = copy / relative
                fixture.write_text(fixture.read_text() + '\n')
                self.assertNotEqual(driver.native_fingerprint(copy)[0], before, relative)
                fixture.write_bytes((source / relative).read_bytes())
            wrap_mixed = copy / 'Resources/Fixtures/nvim-wrap-mixed-matrix.json'
            wrap_mixed.write_text(wrap_mixed.read_text() + '\n')
            self.assertNotEqual(driver.native_fingerprint(copy)[0], before)
            wrap_mixed.write_bytes((source / 'Resources/Fixtures/nvim-wrap-mixed-matrix.json').read_bytes())
            wrap_short = copy / 'Resources/Fixtures/nvim-wrap-short-matrix.json'
            wrap_short.write_text(wrap_short.read_text() + '\n')
            self.assertNotEqual(driver.native_fingerprint(copy)[0], before)
            wrap_short.write_bytes((source / 'Resources/Fixtures/nvim-wrap-short-matrix.json').read_bytes())
            conf = copy / 'Resources/Fixtures/native-tmux.conf'
            conf.write_text(conf.read_text() + '\n# synthetic contract mutation\n')
            self.assertNotEqual(driver.native_fingerprint(copy)[0], before)
            conf.write_bytes((source / 'Resources/Fixtures/native-tmux.conf').read_bytes())
            visual = copy / 'Resources/Fixtures/nvim-visual-insert.json'
            visual.write_text(visual.read_text() + '\n')
            self.assertNotEqual(driver.native_fingerprint(copy)[0], before)
            visual.write_bytes((source / 'Resources/Fixtures/nvim-visual-insert.json').read_bytes())
            picker = copy / 'Resources/Fixtures/nvim-picker-input.json'
            picker.write_text(picker.read_text() + '\n')
            self.assertNotEqual(driver.native_fingerprint(copy)[0], before)
            picker.write_bytes((source / 'Resources/Fixtures/nvim-picker-input.json').read_bytes())
            windows = copy / 'Resources/Fixtures/nvim-window-focus.json'
            windows.write_text(windows.read_text() + '\n')
            self.assertNotEqual(driver.native_fingerprint(copy)[0], before)
            windows.write_bytes((source / 'Resources/Fixtures/nvim-window-focus.json').read_bytes())
            counts = copy / 'Resources/Fixtures/nvim-window-count.json'
            counts.write_text(counts.read_text() + '\n')
            self.assertNotEqual(driver.native_fingerprint(copy)[0], before)
            counts.write_bytes((source / 'Resources/Fixtures/nvim-window-count.json').read_bytes())
            lists = copy / 'Resources/Fixtures/nvim-list-extremes.json'
            lists.write_text(lists.read_text() + '\n')
            self.assertNotEqual(driver.native_fingerprint(copy)[0], before)
            lists.write_bytes((source / 'Resources/Fixtures/nvim-list-extremes.json').read_bytes())
            panes = copy / 'Resources/Fixtures/nvim-editor-panes.rs'
            panes.write_text(panes.read_text() + '\n')
            self.assertNotEqual(driver.native_fingerprint(copy)[0], before)
            panes.write_bytes((source / 'Resources/Fixtures/nvim-editor-panes.rs').read_bytes())
            for name in ('nvim-editor-panes-independent.rs', 'nvim-editor-panes-independent.json',
                         'nvim-fold-indent-shift-reference.json'):
                fixture = copy / 'Resources/Fixtures' / name
                fixture.write_text(fixture.read_text() + '\n')
                self.assertNotEqual(driver.native_fingerprint(copy)[0], before)
                fixture.write_bytes((source / 'Resources/Fixtures' / name).read_bytes())
            focus = copy / 'Resources/Fixtures/nvim-editor-pane-focus.rs'
            focus.write_text(focus.read_text() + '\n')
            self.assertNotEqual(driver.native_fingerprint(copy)[0], before)
            focus.write_bytes((source / 'Resources/Fixtures/nvim-editor-pane-focus.rs').read_bytes())
            views = copy / 'Resources/Fixtures/nvim-buffer-views.json'
            views.write_text(views.read_text() + '\n')
            self.assertNotEqual(driver.native_fingerprint(copy)[0], before)
            views.write_bytes((source / 'Resources/Fixtures/nvim-buffer-views.json').read_bytes())
            columns = copy / 'Resources/Fixtures/nvim-buffer-columns.json'
            columns.write_text(columns.read_text() + '\n')
            self.assertNotEqual(driver.native_fingerprint(copy)[0], before)
            columns.write_bytes((source / 'Resources/Fixtures/nvim-buffer-columns.json').read_bytes())
            snacks = copy / 'Resources/Fixtures/snacks-focus-reference.json'
            snacks.write_text(snacks.read_text() + '\n')
            self.assertNotEqual(driver.native_fingerprint(copy)[0], before)
            snacks.write_bytes((source / 'Resources/Fixtures/snacks-focus-reference.json').read_bytes())
            paging = copy / 'Resources/Fixtures/snacks-paging-reference.json'
            paging.write_text(paging.read_text() + '\n')
            self.assertNotEqual(driver.native_fingerprint(copy)[0], before)
            paging.write_bytes((source / 'Resources/Fixtures/snacks-paging-reference.json').read_bytes())
            for name in ('nvim-wrap-rows.jsonl', 'nvim-wrap-positions.jsonl',
                         'nvim-wrap-operators.jsonl', 'nvim-wrap-follow-limits.jsonl',
                         'nvim-wrap-fold-operators.jsonl', 'nvim-wrap-fold-change.jsonl',
                         'nvim-wrap-change.jsonl', 'nvim-mouse-wheel.jsonl', 'nvim-mouse-wheel-fold.jsonl', 'nvim-line-operators.jsonl', 'nvim-line-fold-operators.jsonl', 'nvim-percent.jsonl', 'nvim-end-count.jsonl', 'nvim-unicode-sequences.jsonl', 'nvim-unicode-word.jsonl', 'nvim-word-classes-0.12.1.txt', 'nvim-word-class-sequences.jsonl', 'nvim-word-objects-unicode.jsonl', 'nvim-word-objects-multiline.jsonl', 'nvim-multiline-change.jsonl', 'nvim-word-objects-change.jsonl', 'nvim-visual-word-objects.jsonl', 'nvim-visual-word-yank.jsonl', 'nvim-find-repeat.jsonl', 'nvim-find-operator-repeat.jsonl', 'nvim-textobject-replay.jsonl', 'nvim-word-multiline.jsonl', 'nvim-change-word.jsonl',
                         'nvim-wrap-wheel-wide-unicode.json', 'nvim-wrap-wheel-extra-geometries.json', 'nvim-wrap-wheel-mixed-directions.json',
                         'nvim-wrap-wheel-visual.json', 'nvim-wrap-wheel-visual-line.json', 'nvim-wrap-wheel-block-vspan.json', 'nvim-wrap-block-paint.json', 'nvim-wrap-block-eol-lazy-postkeys.json', 'nvim-wrap-block-lazy-postkeys.json', 'nvim-wrap-block-eol-paint.json', 'nvim-wrap-block-eol-wheel-paint.json', 'nvim-wrap-block-eol-wheel-intermediate.json', 'nvim-wrap-block-wheel-paint.json', 'nvim-wrap-wheel-block-extra-vspan.json', 'nvim-wrap-wheel-block-mixed-vspan.json', 'nvim-wrap-wheel-insert.json', 'nvim-wrap-wheel-matrix.json',
                         'nvim-pattern-reference.json', 'nvim-prompt-caret.json', 'nvim-prompt-history.json', 'nvim-prompt-register.json'):
                relative = 'Resources/Fixtures/' + name
                self.assertIn(relative, files)
                fixture = copy / relative
                fixture.write_bytes(fixture.read_bytes() + b'\n')
                self.assertNotEqual(driver.native_fingerprint(copy)[0], before, name)
                fixture.write_bytes((source / relative).read_bytes())

    def test_clippy_flags_remain_after_cargo_features(self):
        flags = ['clippy', '--workspace', '--', '-D', 'warnings']
        driver.enable_ghostty(flags)
        self.assertEqual(flags, ['clippy', '--workspace', '--features', 'workspace-app/ghostty', '--', '-D', 'warnings'])

    def test_application_arguments_are_preserved(self):
        flags = ['run', '-p', 'workspace-cli', '--', 'demo']
        driver.enable_ghostty(flags)
        self.assertEqual(flags[-2:], ['--', 'demo'])
        self.assertLess(flags.index('--features'), flags.index('--'))

    def test_existing_native_and_test_commands(self):
        flags = ['clippy', '--features', 'native', '--', '-D', 'warnings']
        driver.enable_ghostty(flags)
        self.assertEqual(flags[2], 'native,ghostty')
        flags = ['test', '--workspace', '--locked']
        driver.enable_ghostty(flags)
        self.assertEqual(flags[-2:], ['--features', 'workspace-app/ghostty'])


if __name__ == '__main__':
    unittest.main()
