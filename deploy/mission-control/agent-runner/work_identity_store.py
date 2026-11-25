"""Atomic, immutable, bounded JSON snapshots for a selected identity scope.

Only cooperating writers holding the private per-path lock may publish a new
snapshot. Interrupted writes can leave private temporary files, never a partial
final receipt. The module knows no native or external workflow protocol.
"""
import fcntl
import json
import os
from pathlib import Path
import stat

from work_publication_clients import PublicationError

MAX_SNAPSHOT_BYTES=16*1024*1024


def _canonical(value):
    return json.dumps(value,sort_keys=True,ensure_ascii=False,allow_nan=False,separators=(',',':')).encode('utf-8')


def _strict_json(raw):
    def pairs(items):
        value={}
        for key,item in items:
            if key in value:raise ValueError('Duplicate identity snapshot JSON field')
            value[key]=item
        return value
    def invalid(_value):raise ValueError('Nonfinite identity snapshot JSON')
    return json.loads(raw,object_pairs_hook=pairs,parse_constant=invalid)


def _scope(path,maximum):
    if (not isinstance(path,Path)or not path.is_absolute()or '..'in path.parts or not path.name
            or type(maximum)is not int or not 1<=maximum<=MAX_SNAPSHOT_BYTES):
        raise ValueError('Invalid identity persistence scope or bound')
    if any(parent.is_symlink()for parent in (path.parent,*path.parent.parents)):
        raise ValueError('Identity scope cannot traverse a symlink')


def _private_regular(descriptor):
    info=os.fstat(descriptor)
    if (not stat.S_ISREG(info.st_mode)or info.st_uid!=os.geteuid()or info.st_nlink!=1
            or stat.S_IMODE(info.st_mode)!=0o600):raise ValueError('Unsafe identity persistence file')
    return info


def _open_directory(path):
    descriptor=os.open(path,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
    info=os.fstat(descriptor)
    if info.st_uid!=os.geteuid()or info.st_mode&0o022:
        os.close(descriptor);raise ValueError('Unsafe identity persistence directory')
    return descriptor


def _read(directory,name,maximum):
    descriptor=os.open(name,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK,dir_fd=directory)
    with os.fdopen(descriptor,'rb')as stream:
        info=_private_regular(stream.fileno())
        if info.st_size>maximum:raise ValueError('Content identity snapshot exceeds its bound')
        raw=stream.read(maximum+1)
        after=_private_regular(stream.fileno())
        if (len(raw)>maximum or (info.st_size,info.st_mtime_ns,info.st_ctime_ns)!=(after.st_size,after.st_mtime_ns,after.st_ctime_ns)):
            raise ValueError('Content identity snapshot changed during read')
    return _strict_json(raw)


def read_identity_snapshot(path,*,maximum=MAX_SNAPSHOT_BYTES):
    """Read an owned 0600 regular snapshot, rejecting duplicate JSON fields."""
    _scope(path,maximum);directory=_open_directory(path.parent)
    try:
        value=_read(directory,path.name,maximum)
        # A prior process may have died immediately after rename. Readers must
        # durably register that complete namespace entry before admitting CI.
        os.fsync(directory)
        return value
    finally:os.close(directory)


def persist_identity_snapshot(path,value,*,maximum=MAX_SNAPSHOT_BYTES):
    """Publish once after fsync; replay equal JSON and reject identity changes."""
    _scope(path,maximum);raw=_canonical(value)+b'\n'
    if len(raw)>maximum:raise ValueError('Content identity snapshot exceeds its bound')
    path.parent.mkdir(mode=0o700,parents=True,exist_ok=True)
    directory=_open_directory(path.parent);lock=None;temporary=None
    try:
        lock=os.open(path.name+'.lock',os.O_RDWR|os.O_CREAT|os.O_NOFOLLOW|os.O_NONBLOCK,0o600,dir_fd=directory)
        _private_regular(lock)
        try:fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
        except BlockingIOError:raise PublicationError('content_identity_registration_busy')from None
        try:os.stat(path.name,dir_fd=directory,follow_symlinks=False)
        except FileNotFoundError:pass
        else:
            if _canonical(_read(directory,path.name,maximum))!=raw[:-1]:
                raise ValueError('Existing verification generation cannot change identity')
            os.fsync(directory)
            return
        temporary=path.name+'.'+str(os.getpid())+'.'+os.urandom(8).hex()+'.new'
        descriptor=os.open(temporary,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600,dir_fd=directory)
        with os.fdopen(descriptor,'wb')as stream:
            _private_regular(stream.fileno());stream.write(raw);stream.flush();os.fsync(stream.fileno())
        # All writers for this scope hold the same private lock; the final path
        # was observed absent under that lock and can never expose partial bytes.
        os.replace(temporary,path.name,src_dir_fd=directory,dst_dir_fd=directory)
        temporary=None;os.fsync(directory)
    finally:
        if temporary is not None:
            try:os.unlink(temporary,dir_fd=directory)
            except FileNotFoundError:pass
        if lock is not None:os.close(lock)
        os.close(directory)
