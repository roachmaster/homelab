"""Real filesystem/process failure boundaries of immutable identity snapshots."""
import fcntl
import multiprocessing
import os
from pathlib import Path
import stat
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch

from work_identity_store import persist_identity_snapshot,read_identity_snapshot
from work_publication_clients import PublicationError


def crashed_writer(path,value,point):
    import work_identity_store as store
    original=store.os.replace
    def crash(*args,**kwargs):
        if point=='after_replace':original(*args,**kwargs)
        os._exit(91)
    with patch.object(store.os,'replace',side_effect=crash):store.persist_identity_snapshot(path,value)


def paused_writer(path,value,ready,release):
    import work_identity_store as store
    original=store.os.replace
    def pause(*args,**kwargs):
        ready.set()
        if not release.wait(5):raise RuntimeError('Fixture release timeout')
        return original(*args,**kwargs)
    with patch.object(store.os,'replace',side_effect=pause):store.persist_identity_snapshot(path,value)


class IdentityStoreTest(unittest.TestCase):
    def setUp(self):
        self.temp=TemporaryDirectory(prefix='forge-identity-store-');self.addCleanup(self.temp.cleanup)
        self.path=Path(self.temp.name)/'scope'/'identity.json';self.value={'version':2,'generation':'fixture','identity':{'sha256':'a'*64}}
    def test_replay_is_immutable_canonical_and_private(self):
        persist_identity_snapshot(self.path,self.value);raw=self.path.read_bytes()
        persist_identity_snapshot(self.path,dict(reversed(list(self.value.items()))))
        self.assertEqual(self.path.read_bytes(),raw);self.assertEqual(read_identity_snapshot(self.path),self.value)
        self.assertEqual(stat.S_IMODE(self.path.stat().st_mode),0o600)
        self.assertEqual(stat.S_IMODE(self.path.with_name(self.path.name+'.lock').stat().st_mode),0o600)
        for value in ({**self.value,'generation':'changed'},{**self.value,'version':True}):
            with self.subTest(value=value),self.assertRaises(ValueError):persist_identity_snapshot(self.path,value)
        self.assertEqual(self.path.read_bytes(),raw)
    def test_exception_before_publish_never_creates_final_partial_file_and_retry_succeeds(self):
        with patch('work_identity_store.os.replace',side_effect=OSError('Injected rename failure')):
            with self.assertRaises(OSError):persist_identity_snapshot(self.path,self.value)
        self.assertFalse(self.path.exists());self.assertEqual(list(self.path.parent.glob('*.new')),[])
        persist_identity_snapshot(self.path,self.value);self.assertEqual(read_identity_snapshot(self.path),self.value)
    def test_data_is_synced_before_publish_and_directory_before_success_or_resumed_read(self):
        original_sync=os.fsync;original_replace=os.replace;events=[]
        def sync(descriptor):
            events.append('directory'if stat.S_ISDIR(os.fstat(descriptor).st_mode)else'file')
            return original_sync(descriptor)
        def publish(*args,**kwargs):
            events.append('publish');self.assertFalse(self.path.exists())
            return original_replace(*args,**kwargs)
        with patch('work_identity_store.os.fsync',side_effect=sync),patch('work_identity_store.os.replace',side_effect=publish):
            persist_identity_snapshot(self.path,self.value)
            self.assertEqual(events,['file','publish','directory'])
            events.clear();self.assertEqual(read_identity_snapshot(self.path),self.value)
            self.assertEqual(events,['directory'])
    def test_crash_before_or_after_atomic_publish_never_exposes_partial_json(self):
        context=multiprocessing.get_context('fork')
        for point in ('before_replace','after_replace'):
            with self.subTest(point=point):
                path=self.path.with_name(point+'.json')
                process=context.Process(target=crashed_writer,args=(path,self.value,point));process.start();process.join(5)
                self.assertFalse(process.is_alive());self.assertEqual(process.exitcode,91)
                if point=='before_replace':
                    self.assertFalse(path.exists());self.assertEqual(len(list(path.parent.glob(path.name+'.*.new'))),1)
                else:self.assertEqual(read_identity_snapshot(path),self.value)
                persist_identity_snapshot(path,self.value);self.assertEqual(read_identity_snapshot(path),self.value)
                before=path.read_bytes()
                with self.assertRaises(ValueError):persist_identity_snapshot(path,{**self.value,'generation':'cannot replace'})
                self.assertEqual(path.read_bytes(),before)
    def test_concurrent_claims_observe_busy_before_effect_then_replay_one_generation(self):
        context=multiprocessing.get_context('fork');ready=context.Event();release=context.Event()
        process=context.Process(target=paused_writer,args=(self.path,self.value,ready,release));process.start()
        try:
            self.assertTrue(ready.wait(5));self.assertFalse(self.path.exists())
            with self.assertRaises(PublicationError)as error:persist_identity_snapshot(self.path,self.value)
            self.assertEqual(error.exception.code,'content_identity_registration_busy')
            with self.assertRaises(PublicationError):persist_identity_snapshot(self.path,{**self.value,'generation':'competitor'})
            self.assertFalse(self.path.exists())
        finally:
            release.set();process.join(5)
            if process.is_alive():process.kill();process.join(5)
        self.assertEqual(process.exitcode,0);persist_identity_snapshot(self.path,self.value)
        self.assertEqual(read_identity_snapshot(self.path),self.value)
        self.assertEqual(list(self.path.parent.glob('*.new')),[])
    def test_unsafe_lock_or_final_file_is_rejected_without_following_or_replacing(self):
        self.path.parent.mkdir(mode=0o700,parents=True)
        target=self.path.parent/'target';target.write_bytes(b'unchanged');target.chmod(0o600)
        lock=self.path.with_name(self.path.name+'.lock')
        for kind in ('symlink','hardlink','fifo','permissions'):
            with self.subTest(kind=kind):
                if kind=='symlink':lock.symlink_to(target)
                elif kind=='hardlink':os.link(target,lock)
                elif kind=='fifo':os.mkfifo(lock,0o600)
                else:lock.write_bytes(b'');lock.chmod(0o644)
                with self.assertRaises((OSError,ValueError)):persist_identity_snapshot(self.path,self.value)
                self.assertFalse(self.path.exists());self.assertEqual(target.read_bytes(),b'unchanged');lock.unlink()
        self.path.symlink_to(target)
        with self.assertRaises((OSError,ValueError)):persist_identity_snapshot(self.path,self.value)
        self.assertTrue(self.path.is_symlink());self.assertEqual(target.read_bytes(),b'unchanged')
    def test_strict_bounded_read_and_invalid_scope_cannot_overwrite_existing_json(self):
        for raw in (b'{"version":1,"version":2}',b'{"value":NaN}',b'{"truncated":'):
            with self.subTest(raw=raw):
                self.path.parent.mkdir(mode=0o700,parents=True,exist_ok=True);self.path.write_bytes(raw);self.path.chmod(0o600)
                with self.assertRaises(ValueError):read_identity_snapshot(self.path)
                with self.assertRaises(ValueError):persist_identity_snapshot(self.path,self.value)
                self.assertEqual(self.path.read_bytes(),raw);self.path.unlink()
        for path in (Path('relative.json'),self.path.parent/'..'/'escape.json'):
            with self.subTest(path=path),self.assertRaises(ValueError):persist_identity_snapshot(path,self.value)
        with self.assertRaises(ValueError):persist_identity_snapshot(self.path,self.value,maximum=8)
        self.assertFalse(self.path.exists())
        with self.assertRaises(ValueError):persist_identity_snapshot(self.path,{'value':float('inf')})
        self.assertFalse(self.path.exists())

if __name__=='__main__':unittest.main()
