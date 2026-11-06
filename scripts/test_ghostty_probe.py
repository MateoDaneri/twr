#!/usr/bin/env python3
"""Own archive fixtures only; no network, compiler or terminal sessions."""
import hashlib
import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

spec=importlib.util.spec_from_file_location('ghostty_probe',Path(__file__).with_name('ghostty_probe.py'))
probe=importlib.util.module_from_spec(spec);spec.loader.exec_module(probe)

class ArchiveControls(unittest.TestCase):
    def setUp(self):
        disk=mock.patch.object(probe.shutil,'disk_usage',return_value=SimpleNamespace(free=probe.MIN_FREE+1024))
        disk.start();self.addCleanup(disk.stop)

    def archive(self,root,name,kind=tarfile.REGTYPE,data=b'fixture'):
        path=root/'fixture.tar';entry=tarfile.TarInfo(name);entry.type=kind;entry.mode=0o644
        if kind==tarfile.REGTYPE:entry.size=len(data)
        else:entry.linkname='../forbidden'
        with tarfile.open(path,'w') as tar:tar.addfile(entry,io.BytesIO(data) if kind==tarfile.REGTYPE else None)
        return path
    def test_verified_file_and_wrong_blob(self):
        with tempfile.TemporaryDirectory(dir='/private/tmp') as temp:
            root=Path(temp);archive=self.archive(root,'source/safe/fixture.txt')
            digest=hashlib.sha1(b'blob 7\0fixture').hexdigest()
            result=probe.extract(archive,root/'valid','source',1024,{'safe/fixture.txt':digest})
            self.assertEqual(result['files'],1);self.assertEqual((root/'valid/safe/fixture.txt').read_bytes(),b'fixture')
            with self.assertRaisesRegex(RuntimeError,'blob mismatch'):probe.extract(archive,root/'wrong','source',1024,{'safe/fixture.txt':'0'*40})
    def test_traversal_links_and_byte_budget(self):
        for name,kind in [('source/../escaped.txt',tarfile.REGTYPE),('source/link',tarfile.SYMTYPE),('source/link',tarfile.LNKTYPE)]:
            with tempfile.TemporaryDirectory(dir='/private/tmp') as temp:
                root=Path(temp);archive=self.archive(root,name,kind)
                with self.assertRaises(RuntimeError):probe.extract(archive,root/'output','source',1024)
                self.assertFalse((root/'escaped.txt').exists())
        with tempfile.TemporaryDirectory(dir='/private/tmp') as temp:
            root=Path(temp);archive=self.archive(root,'source/large')
            with self.assertRaisesRegex(RuntimeError,'budget'):probe.extract(archive,root/'output','source',1)
    def test_symlink_parent_is_rejected_before_read(self):
        with tempfile.TemporaryDirectory(dir='/private/tmp') as temp:
            root=Path(temp);outside=root/'own-forbidden';outside.mkdir();(outside/'marker').write_text('do not read')
            (root/'source').symlink_to(outside,target_is_directory=True)
            with self.assertRaisesRegex(RuntimeError,'Symlink'):probe.child(root,root/'source/marker')

    def test_low_disk_stops_before_writing_member(self):
        with tempfile.TemporaryDirectory(dir='/private/tmp') as temp:
            root=Path(temp);archive=self.archive(root,'source/marker')
            with mock.patch.object(probe.shutil,'disk_usage',return_value=SimpleNamespace(free=probe.MIN_FREE-1)):
                with self.assertRaisesRegex(RuntimeError,'2GiB free guard'):probe.extract(archive,root/'output','source',1024)
            self.assertFalse((root/'output/marker').exists())

class ProcessReceipt(unittest.TestCase):
    def test_cleanup_denial_retains_exit_status_and_pid(self):
        with tempfile.TemporaryDirectory(dir='/private/tmp') as temp:
            root=Path(temp);(root/'source').mkdir();log=root/'build.log'
            process=mock.Mock(pid=45123,returncode=7)
            process.poll.return_value=7
            with mock.patch.object(probe.subprocess,'Popen',return_value=process), mock.patch.object(probe.os,'killpg',side_effect=PermissionError('fixture denial')):
                result=probe.run_owned(root,['synthetic-build'],log)
            recorded=probe.json.loads(log.with_suffix('.json').read_text())
            self.assertEqual(result,recorded)
            self.assertEqual(recorded['pid'],45123)
            self.assertEqual(recorded['returncode'],7)
            self.assertEqual(recorded['state'],'cleanup_unverified')
            self.assertTrue(recorded['cleanup_errors'])
            process.send_signal.assert_not_called()

if __name__=='__main__':unittest.main()
