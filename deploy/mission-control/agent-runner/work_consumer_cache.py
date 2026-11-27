"""Prepare fixed consumer-build mounts outside an exclusively owned workspace.

Bootstrap selects this provider; construction is effect-free. The caller holds
the workspace lease and must stop/unmount its child before hashing the workspace.
Only empty, Git-ignored and untracked mountpoints are admitted. Existing output
is a reconciliation problem, never permission to prune or move files.
"""

from contextlib import contextmanager
import os
from pathlib import Path
import stat
import subprocess


CONSUMER_TARGETS = tuple(Path("src/rust") / crate / "target" for crate in (
    "forge-remote-https", "forge-ide", "forge-template",
))


class ConsumerCacheError(ValueError):
    """The selected workspace/cache cannot be safely prepared for mounting."""


def _absolute(path):
    path = Path(path)
    if not path.is_absolute() or ".." in path.parts:
        raise ConsumerCacheError("Consumer cache paths must be absolute and contained.")
    return path


@contextmanager
def _directory(path):
    """Open every component without following links, including trusted roots."""
    descriptor = os.open("/", os.O_RDONLY | os.O_DIRECTORY)
    try:
        for component in path.parts[1:]:
            child = os.open(component, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                            dir_fd=descriptor)
            os.close(descriptor)
            descriptor = child
        yield descriptor
    finally:
        os.close(descriptor)


def _empty_directory(parent, name):
    try:
        entry = os.stat(name, dir_fd=parent, follow_symlinks=False)
    except FileNotFoundError:
        return
    if not stat.S_ISDIR(entry.st_mode):
        raise ConsumerCacheError("Consumer cache mountpoints must be directories without links.")
    descriptor = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)
    try:
        if os.listdir(descriptor):
            raise ConsumerCacheError("Existing consumer output requires reconciliation.")
    finally:
        os.close(descriptor)


def _git(root, *args):
    environment = {key: value for key, value in os.environ.items()
                   if key in {"PATH", "LANG", "LC_ALL", "SYSTEMROOT"}}
    environment.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="/dev/null",
                       GIT_OPTIONAL_LOCKS="0", GIT_TERMINAL_PROMPT="0")
    return subprocess.run(("/usr/bin/git", "-C", str(root), *args), stdin=subprocess.DEVNULL,
                          stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                          env=environment, timeout=15, check=False)


class ConsumerCacheProvider:
    """Attempt-scoped mounts for the three public-consumer Cargo target roots."""

    def __init__(self, child_workspace=Path("/run/forge-coding-workspace")):
        self.child_workspace = _absolute(child_workspace)

    def prepare(self, workspace, scratch):
        """Return ``(scratch source, child destination)`` pairs after validation.

        ``workspace.path`` is the leased native source; ``scratch`` is the owned,
        existing attempt directory. Source parents must already exist. No source
        file is excluded from receipts, and no existing artifact is removed.
        """
        root, scratch = _absolute(workspace.path), _absolute(scratch)
        if root.is_relative_to(scratch) or scratch.is_relative_to(root):
            raise ConsumerCacheError("Consumer caches must be outside the source workspace.")
        cache_names = tuple("consumer-target-" + relative.parent.name for relative in CONSUMER_TARGETS)
        try:
            with _directory(root), _directory(scratch) as scratch_fd:
                # Validate every destination before creating any empty directory.
                for relative, name in zip(CONSUMER_TARGETS, cache_names):
                    with _directory(root / relative.parent) as parent:
                        _empty_directory(parent, relative.name)
                    _empty_directory(scratch_fd, name)
                    tracked = _git(root, "ls-files", "-z", "--", relative.as_posix())
                    ignored = _git(root, "check-ignore", "--quiet", "--", relative.as_posix() + "/")
                    if tracked.returncode != 0 or tracked.stdout or ignored.returncode != 0:
                        raise ConsumerCacheError("Consumer mountpoints must be Git-ignored and untracked.")
                for relative, name in zip(CONSUMER_TARGETS, cache_names):
                    with _directory(root / relative.parent) as parent:
                        _empty_directory(parent, relative.name)
                        try:
                            os.mkdir(relative.name, mode=0o700, dir_fd=parent)
                        except FileExistsError:
                            pass
                    _empty_directory(scratch_fd, name)
                    try:
                        os.mkdir(name, mode=0o700, dir_fd=scratch_fd)
                    except FileExistsError:
                        pass
        except (OSError, subprocess.SubprocessError) as error:
            raise ConsumerCacheError("Consumer cache preparation could not be verified.") from error
        return tuple((scratch / name, self.child_workspace / relative)
                     for relative, name in zip(CONSUMER_TARGETS, cache_names))


class ScopedConsumerCacheProvider:
    """Bootstrap selects only configured Forge projects, never arbitrary paths."""
    def __init__(self, repositories, provider=None):
        self.repositories = frozenset(_absolute(path) for path in repositories)
        self.provider = provider if provider is not None else ConsumerCacheProvider()

    def prepare(self, workspace, scratch):
        if (getattr(workspace, 'source_repository', None) or workspace.repository) not in self.repositories:
            return ()
        return self.provider.prepare(workspace, scratch)


def configured_consumer_caches(config):
    # The fixed capability belongs to the explicit homelab project grant. Other
    # projects keep their ordinary scratch contract and gain no extra mounts.
    selected = (config.projects["homelab"],) if "homelab" in config.projects else ()
    return ScopedConsumerCacheProvider(selected)
