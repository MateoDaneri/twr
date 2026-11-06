#!/usr/bin/env python3
"""Deterministic regression for the observed image2 atomic-rename race."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('video', Path(__file__).resolve().parents[1] / 'Resources/Helpers/prepare_video.py')
video = importlib.util.module_from_spec(spec)
spec.loader.exec_module(video)


class FrameCache(unittest.TestCase):
    def test_atomic_rename_between_listing_and_stat_does_not_abort_decode(self):
        with tempfile.TemporaryDirectory(prefix='tw-video-cache-race-', dir='/private/tmp') as name:
            frames = Path(name)
            first = frames / 'frame-000001.png'
            first.write_bytes(b'first')
            pending = frames / 'frame-000002.png.tmp'
            pending.write_bytes(b'second')
            snapshot = list(frames.iterdir())
            pending.rename(frames / 'frame-000002.png')
            self.assertEqual(video.cache_bytes(snapshot), 5)
            self.assertEqual(video.cache_bytes(list(frames.iterdir())), 11)

    def test_nonregular_output_is_rejected_instead_of_following_a_link(self):
        with tempfile.TemporaryDirectory(prefix='tw-video-cache-race-', dir='/private/tmp') as name:
            root = Path(name)
            source = root / 'frame.png'
            source.write_bytes(b'frame')
            link = root / 'frame.png.tmp'
            link.symlink_to(source)
            with self.assertRaises(RuntimeError):
                video.cache_bytes([link])

    def test_progress_excludes_incomplete_and_noncontiguous_frames(self):
        with tempfile.TemporaryDirectory(prefix='tw-video-progress-', dir='/private/tmp') as name:
            job = Path(name)
            frames = job / 'frames'
            frames.mkdir()
            (frames / 'frame-000001.png').write_bytes(b'one')
            (frames / 'frame-000002.png.tmp').write_bytes(b'partial')
            (frames / 'frame-000003.png').write_bytes(b'three')
            metadata = {'schema': 1, 'fps': 12, 'duration': 1.}
            self.assertEqual(video.publish_progress(frames, metadata, 0), 1)
            first = video.json.loads((job / 'progress.json').read_text())
            self.assertEqual((first['frames'], first['cache_bytes']), (1, 3))
            (frames / 'frame-000002.png.tmp').rename(frames / 'frame-000002.png')
            self.assertEqual(video.publish_progress(frames, metadata, 1), 3)
            final = video.json.loads((job / 'progress.json').read_text())
            self.assertEqual((final['frames'], final['cache_bytes']), (3, 15))
            self.assertFalse((job / 'manifest.json').exists())


if __name__ == '__main__':
    unittest.main()
