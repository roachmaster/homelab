"""Manual-only Muse allowance transport; never generation, login or polling.

The verified installed command is exactly ``usage --json``. Each explicit read
owns a private temporary HOME containing only a staged selected-account auth
inode. The pinned executable is held open across exec; user settings, MCP,
hooks, provider overrides and inherited environment are never imported.
"""

from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import selectors
import signal
import stat
import subprocess
from tempfile import mkdtemp
import shutil
import time
from types import SimpleNamespace

from usage_rpc import MetadataProcessConfig, ProviderFailure, _invalid_constant, _unique_object
from work_muse import BINARY_SHA256, stage_auth
from work_native_auth import AuthCleanupError, AuthLease


class MuseUsageProcess:
    """Acquire one bounded native metadata process through an injected port."""

    MAX_OUTPUT_BYTES = 64 * 1024
    MAX_BINARY_BYTES = 512 * 1024 * 1024

    def __init__(self, config, *, process_factory=subprocess.Popen, auth_stager=stage_auth,
                 signal_group=os.killpg, monotonic=time.monotonic, cleanup_delay=time.sleep):
        if not isinstance(config, MetadataProcessConfig) or config.code_home is not None:
            raise TypeError("Muse usage requires isolated typed process configuration")
        self._config = config
        self._spawn = process_factory
        self._stage_auth = auth_stager
        self._signal = signal_group
        self._clock = monotonic
        self._delay = cleanup_delay

    @contextmanager
    def _executable(self):
        descriptor = None
        try:
            descriptor = os.open(self._config.binary, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK)
            info = os.fstat(descriptor)
            if (not stat.S_ISREG(info.st_mode) or not 0 < info.st_size <= self.MAX_BINARY_BYTES
                    or info.st_mode & 0o022 or not info.st_mode & 0o111):
                raise ProviderFailure("unsupported_version")
            digest = hashlib.sha256()
            seen = 0
            while chunk := os.read(descriptor, 1024 * 1024):
                seen += len(chunk)
                if seen > self.MAX_BINARY_BYTES:
                    raise ProviderFailure("unsupported_version")
                digest.update(chunk)
            if digest.hexdigest() != BINARY_SHA256:
                raise ProviderFailure("unsupported_version")
            os.lseek(descriptor, 0, os.SEEK_SET)
            yield descriptor
        except OSError:
            raise ProviderFailure("unavailable") from None
        finally:
            if descriptor is not None:
                os.close(descriptor)

    def _read_output(self, process):
        output = bytearray()
        seen = 0
        deadline = self._clock() + self._config.timeout_seconds
        with selectors.DefaultSelector() as selected:
            selected.register(process.stdout, selectors.EVENT_READ, "stdout")
            selected.register(process.stderr, selectors.EVENT_READ, "stderr")
            while selected.get_map():
                remaining = deadline - self._clock()
                if remaining <= 0:
                    raise ProviderFailure("timeout")
                ready = selected.select(remaining)
                if not ready:
                    raise ProviderFailure("timeout")
                for key, _mask in ready:
                    data = os.read(key.fd, 65536)
                    if not data:
                        selected.unregister(key.fileobj)
                        continue
                    seen += len(data)
                    if seen > self.MAX_OUTPUT_BYTES:
                        raise ProviderFailure("response_limit")
                    if key.data == "stdout":
                        output.extend(data)
                    # Never retain or forward stderr, even on native failure.
        return bytes(output)

    def _stop(self, process):
        # The unreaped leader reserves its group identity until both signals.
        # Always kill descendants too, even when the leader has already exited.
        if type(process.pid) is not int or process.pid <= 1 or process.pid in (os.getpid(), os.getpgrp()):
            raise ProviderFailure("unavailable")
        try:
            for signum in (signal.SIGTERM, signal.SIGKILL):
                try:
                    self._signal(process.pid, signum)
                except ProcessLookupError:
                    pass
                if signum == signal.SIGTERM:
                    self._delay(.05)
            status = process.wait(timeout=2)
            try:
                self._signal(process.pid, 0)
            except ProcessLookupError:
                return status
            raise ProviderFailure("unavailable")
        except (OSError, subprocess.SubprocessError):
            raise ProviderFailure("unavailable") from None
        finally:
            process.stdout.close()
            process.stderr.close()

    def read(self):
        uid = self._config.run_uid if self._config.run_uid is not None else os.geteuid()
        gid = self._config.run_gid if self._config.run_gid is not None else os.getegid()
        if uid <= 0 or os.geteuid() not in (0, uid):
            raise ProviderFailure("unavailable")
        home = Path(mkdtemp(prefix="forge-muse-usage-"))
        credential = None
        process = None
        spawn_attempted = False
        stopped = False
        acquisition_uncertain = False
        try:
            settings = home / ".config/muse"
            settings.mkdir(mode=0o700, parents=True)
            auth = settings / "auth.json"
            try:
                credential = self._stage_auth(SimpleNamespace(home=self._config.home, uid=uid), auth)
            except AuthCleanupError:
                acquisition_uncertain = True
                raise ProviderFailure("unavailable") from None
            if not credential:
                raise ProviderFailure("auth_required")
            if not isinstance(credential, AuthLease):
                raise ProviderFailure("unavailable")
            # The account reads its staged auth even when the parent is root.
            for path in (home, home / ".config", settings):
                path.chmod(0o700)
                if os.geteuid() == 0:
                    os.chown(path, uid, gid)
            if os.geteuid() == 0:
                os.chown(auth, uid, gid)
            environment = {"HOME": str(home), "XDG_CONFIG_HOME": str(home / ".config"),
                           "MUSE_AUTH_PATH": str(auth), "PATH": "/usr/local/bin:/usr/bin:/bin",
                           "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8", "RUST_LOG": "error"}
            options = {"stdin": subprocess.DEVNULL, "stdout": subprocess.PIPE, "stderr": subprocess.PIPE,
                       "env": environment, "cwd": "/", "close_fds": True, "start_new_session": True}
            if self._config.run_uid is not None:
                options.update(user=uid, group=gid, extra_groups=[])
            with self._executable() as descriptor:
                # Execute the already verified inode, not a path replaceable
                # between hashing and exec. argv remains the pinned identity.
                options.update(executable=f"/proc/self/fd/{descriptor}", pass_fds=(descriptor,))
                spawn_attempted = True
                try:
                    process = self._spawn([str(self._config.binary), "usage", "--json"], **options)
                except OSError:
                    # Popen did not create an owned child and reaps exec errors.
                    spawn_attempted = False
                    raise
                try:
                    raw = self._read_output(process)
                finally:
                    status = self._stop(process)
                    stopped = True
            if status != 0:
                raise ProviderFailure("unavailable")
            try:
                return json.loads(raw.decode("utf-8"), object_pairs_hook=_unique_object,
                                  parse_constant=_invalid_constant)
            except (ValueError, UnicodeError, RecursionError):
                raise ProviderFailure("malformed_metadata") from None
        except ProviderFailure:
            raise
        except (OSError, ValueError, TypeError):
            raise ProviderFailure("unavailable") from None
        finally:
            # No pathname cleanup can stand in for proof of owned group stop.
            # An uncertain process or replaced auth inode retains its private
            # directory for explicit reconciliation; never publish native data.
            safe = not acquisition_uncertain and (not spawn_attempted or stopped)
            try:
                if isinstance(credential, AuthLease):
                    safe = safe and credential.cleanup()
                if safe:
                    shutil.rmtree(home)
            finally:
                if isinstance(credential, AuthLease):
                    credential.close()
            if not safe:
                raise ProviderFailure("unavailable") from None
