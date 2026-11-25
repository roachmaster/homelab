"""Explicitly selected, bounded read-only adapters for the pure artifact port."""
from dataclasses import dataclass
import hashlib
import os
from pathlib import Path
import re
import stat
import time

from work_artifact_identity import (ArtifactEntry, ArtifactManifest, MAX_FILES,
    MAX_FILE_BYTES, MAX_TOTAL_BYTES, MAX_PATH_BYTES, MODES, validate_path)

_OBJECT = re.compile(r'(?:[0-9a-f]{40}|[0-9a-f]{64})')


@dataclass(frozen=True)
class SourceLimits:
    max_files: int = MAX_FILES
    max_file_bytes: int = MAX_FILE_BYTES
    max_total_bytes: int = MAX_TOTAL_BYTES
    timeout_seconds: int = 120

    def __post_init__(self):
        for value, maximum in ((self.max_files, MAX_FILES), (self.max_file_bytes, MAX_FILE_BYTES),
                               (self.max_total_bytes, MAX_TOTAL_BYTES), (self.timeout_seconds, 600)):
            if type(value) is not int or not 1 <= value <= maximum:
                raise ValueError('Invalid artifact acquisition limit')


def _root(value):
    if not isinstance(value, Path) or not value.is_absolute() or '..' in value.parts:
        raise ValueError('Expected absolute artifact source root')


@dataclass(frozen=True)
class GitSourceConfig:
    repository: Path
    revision: str
    limits: SourceLimits = SourceLimits()

    def __post_init__(self):
        _root(self.repository)
        if (not isinstance(self.revision, str) or not 1 <= len(self.revision.encode('utf-8')) <= 1024
                or self.revision.startswith('-') or any(ord(c) < 32 or ord(c) == 127 for c in self.revision)
                or not isinstance(self.limits, SourceLimits)):
            raise ValueError('Invalid committed artifact scope')


@dataclass(frozen=True)
class DirectorySourceConfig:
    root: Path
    limits: SourceLimits = SourceLimits()

    def __post_init__(self):
        _root(self.root)
        if not isinstance(self.limits, SourceLimits):
            raise ValueError('Invalid directory artifact limits')


@dataclass(frozen=True)
class MemorySourceConfig:
    """A tuple of (relative path, canonical mode, exact bytes) source records."""
    files: tuple
    limits: SourceLimits = SourceLimits()

    def __post_init__(self):
        if not isinstance(self.files, tuple) or not isinstance(self.limits, SourceLimits):
            raise ValueError('Expected immutable in-memory artifact scope')
        if len(self.files) > self.limits.max_files:
            raise ValueError('Artifact exceeds file count limit')
        total = 0
        for item in self.files:
            if not isinstance(item, tuple) or len(item) != 3:
                raise ValueError('Invalid in-memory artifact record')
            path, mode, data = item
            validate_path(path)
            if not isinstance(mode, str) or mode not in MODES or not isinstance(data, bytes):
                raise ValueError('Invalid in-memory artifact file')
            total += len(data)
            if len(data) > self.limits.max_file_bytes or total > self.limits.max_total_bytes:
                raise ValueError('Artifact exceeds byte limit')
        # Reject duplicate and file/directory conflicts before acquisition.
        ArtifactManifest(tuple(ArtifactEntry(p, m, len(b), hashlib.sha256(b).hexdigest())
                               for p, m, b in self.files))


class _Budget:
    def __init__(self, limits):
        self.limits = limits
        self.deadline = time.monotonic() + limits.timeout_seconds
        self.files = self.total = 0

    def remaining(self):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise ValueError('Artifact acquisition deadline exceeded')
        return min(60, remaining)

    def add(self, size):
        self.remaining()
        if type(size) is not int or not 0 <= size <= self.limits.max_file_bytes:
            raise ValueError('Artifact exceeds file byte limit')
        self.files += 1
        self.total += size
        if self.files > self.limits.max_files or self.total > self.limits.max_total_bytes:
            raise ValueError('Artifact exceeds acquisition limits')


class GitArtifactSource:
    def __init__(self, config, git):
        self.config, self.git = config, git

    def acquire(self):
        config = self.config
        budget = _Budget(config.limits)
        def run(*args):
            result = self.git._git(config.repository, '--no-replace-objects', *args,
                                   timeout=budget.remaining())
            if not isinstance(result, bytes):
                raise ValueError('Expected raw Git artifact bytes')
            return result
        # Resolve once: a moving ref is provenance, not a mutable read scope.
        tree = run('rev-parse', '--verify', '--end-of-options', config.revision + '^{tree}').decode('ascii').strip()
        if not _OBJECT.fullmatch(tree):
            raise ValueError('Invalid source tree object')
        raw = run('ls-tree', '-r', '--full-tree', '-z', '--long', tree)
        if len(raw) > config.limits.max_files * (MAX_PATH_BYTES + 128) or raw and not raw.endswith(b'\0'):
            raise ValueError('Invalid Git source tree listing')
        records = []
        paths = set()
        for record in raw.split(b'\0')[:-1]:
            try:
                description, raw_path = record.split(b'\t', 1)
                mode, kind, blob, raw_size = description.decode('ascii').split()
                path = raw_path.decode('utf-8')
                size = int(raw_size)
            except (ValueError, UnicodeError) as error:
                raise ValueError('Malformed Git source entry') from error
            validate_path(path)
            if (mode not in MODES or kind != 'blob' or not _OBJECT.fullmatch(blob)
                    or not re.fullmatch(r'0|[1-9][0-9]*', raw_size) or path in paths):
                raise ValueError('Unsupported or duplicate Git source entry')
            budget.add(size)
            paths.add(path)
            records.append((path, mode, blob, size))
        # Structural conflicts fail before reading any object content.
        ArtifactManifest(tuple(ArtifactEntry(path, mode, size, '0' * 64)
                               for path, mode, _blob, size in records))
        entries = []
        for path, mode, blob, size in records:
            content = run('cat-file', 'blob', blob)
            if len(content) != size:
                raise ValueError('Source blob size changed')
            entries.append(ArtifactEntry(path, mode, size, hashlib.sha256(content).hexdigest()))
        return ArtifactManifest(tuple(entries))


class DirectoryArtifactSource:
    def __init__(self, config):
        self.config = config

    def acquire(self):
        budget = _Budget(self.config.limits)
        entries = []
        visited = 0
        flags = os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK
        def stable(info):
            return (info.st_dev, info.st_ino, info.st_mode, info.st_size, info.st_mtime_ns, info.st_ctime_ns)
        def walk(directory, prefix):
            nonlocal visited
            before_directory = stable(os.fstat(directory))
            with os.scandir(directory) as scan:
                names = []
                for entry in scan:
                    visited += 1
                    if visited > self.config.limits.max_files * 2:
                        raise ValueError('Artifact exceeds directory entry limit')
                    names.append(entry.name)
            for name in sorted(names):
                budget.remaining()
                path = prefix + name
                validate_path(path)
                info = os.stat(name, dir_fd=directory, follow_symlinks=False)
                if stat.S_ISLNK(info.st_mode):
                    raise ValueError('Artifact symlink is unsupported')
                if stat.S_ISDIR(info.st_mode):
                    child = os.open(name, flags | os.O_DIRECTORY, dir_fd=directory)
                    try:
                        if stable(os.fstat(child)) != stable(info):
                            raise ValueError('Artifact directory changed during acquisition')
                        walk(child, path + '/')
                    finally:
                        os.close(child)
                    continue
                if not stat.S_ISREG(info.st_mode) or info.st_mode & (stat.S_ISUID | stat.S_ISGID | stat.S_ISVTX):
                    raise ValueError('Artifact special file mode is unsupported')
                budget.add(info.st_size)
                descriptor = os.open(name, flags, dir_fd=directory)
                try:
                    opened = os.fstat(descriptor)
                    if not stat.S_ISREG(opened.st_mode) or stable(opened) != stable(info):
                        raise ValueError('Artifact file changed during acquisition')
                    digest = hashlib.sha256()
                    count = 0
                    while True:
                        budget.remaining()
                        data = os.read(descriptor, 256 * 1024)
                        if not data:
                            break
                        count += len(data)
                        if count > info.st_size or count > self.config.limits.max_file_bytes:
                            raise ValueError('Artifact file changed during acquisition')
                        digest.update(data)
                    if count != info.st_size or stable(os.fstat(descriptor)) != stable(info):
                        raise ValueError('Artifact file changed during acquisition')
                    mode = '100755' if info.st_mode & stat.S_IXUSR else '100644'
                    entries.append(ArtifactEntry(path, mode, count, digest.hexdigest()))
                finally:
                    os.close(descriptor)
            if stable(os.fstat(directory)) != before_directory:
                raise ValueError('Artifact directory changed during acquisition')
        root = os.open(self.config.root, flags | os.O_DIRECTORY)
        try:
            walk(root, '')
        finally:
            os.close(root)
        return ArtifactManifest(tuple(entries))


class MemoryArtifactSource:
    def __init__(self, config):
        self.config = config

    def acquire(self):
        return ArtifactManifest(tuple(ArtifactEntry(path, mode, len(content), hashlib.sha256(content).hexdigest())
                                      for path, mode, content in self.config.files))


class ArtifactSourceFactory:
    """Effect-free typed selection. Only the acquired ready port reads a source."""
    def __init__(self, config, *, git=None):
        if not isinstance(config, (GitSourceConfig, DirectorySourceConfig, MemorySourceConfig)):
            raise TypeError('Expected typed artifact source selection')
        if isinstance(config, GitSourceConfig) and not callable(getattr(git, '_git', None)):
            raise TypeError('Expected ready Git executor')
        self.config, self.git = config, git

    def create(self):
        if isinstance(self.config, GitSourceConfig):
            return GitArtifactSource(self.config, self.git)
        if isinstance(self.config, DirectorySourceConfig):
            return DirectoryArtifactSource(self.config)
        return MemoryArtifactSource(self.config)


class GitArtifactProvider:
    """Ready scoped provider injected once; callers supply only provenance scope."""
    def __init__(self, git, limits):
        self.git, self.limits = git, limits

    def acquire(self, repository, revision):
        return GitArtifactSource(GitSourceConfig(repository, revision, self.limits), self.git).acquire()


class GitArtifactProviderFactory:
    def __init__(self, git, *, limits=SourceLimits()):
        if not callable(getattr(git, '_git', None)) or not isinstance(limits, SourceLimits):
            raise TypeError('Expected ready Git executor and typed limits')
        self.git, self.limits = git, limits

    def create(self):
        return GitArtifactProvider(self.git, self.limits)
