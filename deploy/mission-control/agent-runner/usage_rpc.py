"""Bounded Codex metadata RPC transport. It cannot start agent turns or login."""

from dataclasses import dataclass
import json
import os
from pathlib import Path
import selectors
import signal
import subprocess
import time

from usage_types import SAFE_ERRORS


class ProviderFailure(Exception):
    def __init__(self, code="unavailable"):
        self.code = code if code in SAFE_ERRORS else "unavailable"
        super().__init__(self.code)


@dataclass(frozen=True)
class MetadataProcessConfig:
    binary: Path
    home: Path
    code_home: Path | None = None
    run_uid: int | None = None
    run_gid: int | None = None
    timeout_seconds: float = 15

    def __post_init__(self):
        for path in (self.binary, self.home, self.code_home):
            if path is not None and (not isinstance(path, Path) or not path.is_absolute()
                                     or any(ord(char) < 32 for char in str(path))):
                raise ValueError("Metadata process paths must be absolute")
        if self.binary is None or self.home is None:
            raise ValueError("Metadata process requires binary and home")
        if ((self.run_uid is None) != (self.run_gid is None)
                or self.run_uid is not None and (type(self.run_uid) is not int or self.run_uid <= 0
                                                or type(self.run_gid) is not int or self.run_gid <= 0)):
            raise ValueError("Metadata UID/GID must select a non-root account together")
        if type(self.timeout_seconds) not in (int, float) or not 1 <= self.timeout_seconds <= 30:
            raise ValueError("Invalid metadata timeout")

    def environment(self):
        # Do not inherit API keys, DB credentials, bearer tokens, proxy settings or
        # provider/base-URL overrides from the parent worker.
        result = {"HOME": str(self.home), "PATH": "/usr/local/bin:/usr/bin:/bin",
                  "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8", "RUST_LOG": "error"}
        if self.code_home is not None:
            result["CODEX_HOME"] = str(self.code_home)
        return result


def _unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate metadata key")
        result[key] = value
    return result


def _invalid_constant(_value):
    raise ValueError("invalid metadata number")


class _Connection:
    MAX_FRAME = 256 * 1024
    MAX_STREAM = 1024 * 1024

    def __init__(self, process, *, monotonic=time.monotonic, timeout=15):
        self.process = process
        self._clock = monotonic
        self._deadline = monotonic() + timeout
        self._selector = selectors.DefaultSelector()
        try:
            self._selector.register(process.stdout, selectors.EVENT_READ, "stdout")
            self._selector.register(process.stderr, selectors.EVENT_READ, "stderr")
        except Exception:
            self._selector.close()
            raise ProviderFailure("unavailable") from None
        self._buffer = bytearray()
        self._seen = 0

    def close(self):
        self._selector.close()

    def line(self):
        while True:
            end = self._buffer.find(b"\n")
            if end >= 0:
                if end > self.MAX_FRAME:
                    raise ProviderFailure("response_limit")
                line = bytes(self._buffer[:end])
                del self._buffer[:end + 1]
                return line
            if len(self._buffer) > self.MAX_FRAME:
                raise ProviderFailure("response_limit")
            remaining = self._deadline - self._clock()
            if remaining <= 0:
                raise ProviderFailure("timeout")
            ready = self._selector.select(remaining)
            if not ready:
                raise ProviderFailure("timeout")
            for key, _mask in ready:
                data = os.read(key.fileobj.fileno(), 65536)
                if not data:
                    self._selector.unregister(key.fileobj)
                    if key.data == "stdout":
                        raise ProviderFailure("unavailable")
                    continue
                self._seen += len(data)
                if self._seen > self.MAX_STREAM:
                    raise ProviderFailure("response_limit")
                if key.data == "stdout":
                    self._buffer.extend(data)
                # stderr is deliberately drained but never retained or forwarded.

    def send(self, value):
        try:
            self.process.stdin.write(json.dumps(value, separators=(",", ":")).encode() + b"\n")
            self.process.stdin.flush()
        except (BrokenPipeError, OSError):
            raise ProviderFailure("unavailable") from None

    def request(self, request_id, method, params=None):
        request = {"id": request_id, "method": method}
        if params is not None:
            request["params"] = params
        self.send(request)
        for _ in range(128):
            try:
                item = json.loads(self.line().decode("utf-8"), object_pairs_hook=_unique_object,
                                  parse_constant=_invalid_constant)
            except (ValueError, RecursionError, UnicodeError):
                raise ProviderFailure("malformed_metadata") from None
            if not isinstance(item, dict):
                raise ProviderFailure("malformed_metadata")
            if "method" in item:
                if "id" in item:
                    # No authentication, approval, process or filesystem request
                    # from a provider can execute through this metadata channel.
                    self.send({"id": item["id"], "error": {"code": -32601,
                               "message": "Unsupported metadata client request"}})
                continue
            if item.get("id") != request_id:
                continue
            if "error" in item:
                raise ProviderFailure("unavailable")
            if "result" not in item or not isinstance(item["result"], dict):
                raise ProviderFailure("malformed_metadata")
            return item["result"]
        raise ProviderFailure("response_limit")


class CodexMetadataRPC:
    """Fixed metadata-only argv/RPC allowlist with injected process ownership."""

    def __init__(self, config, *, process_factory=subprocess.Popen, signal_group=os.killpg,
                 monotonic=time.monotonic, cleanup_delay=time.sleep):
        if not isinstance(config, MetadataProcessConfig):
            raise TypeError("Metadata RPC requires typed configuration")
        self._config = config
        self._spawn = process_factory
        self._signal = signal_group
        self._clock = monotonic
        self._delay = cleanup_delay

    def _start(self, arguments):
        options = {"stdin": subprocess.PIPE, "stdout": subprocess.PIPE, "stderr": subprocess.PIPE,
                   "env": self._config.environment(), "cwd": "/", "close_fds": True,
                   "start_new_session": True}
        if self._config.run_uid is not None:
            options.update(user=self._config.run_uid, group=self._config.run_gid, extra_groups=[])
        elif os.geteuid() == 0:
            raise ProviderFailure("unavailable")
        try:
            return self._spawn([str(self._config.binary), *arguments], **options)
        except (OSError, ValueError):
            raise ProviderFailure("unavailable") from None

    def _stop(self, process):
        # Do not poll/reap the leader before signaling its owned group: keeping
        # that PID reserved avoids signaling a newly reused process group.
        try:
            process.stdin.close()
        except OSError:
            pass
        try:
            self._signal(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        self._delay(0.05)
        try:
            self._signal(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            raise ProviderFailure("unavailable") from None
        finally:
            process.stdout.close()
            process.stderr.close()

    def version(self):
        process = self._start(["--version"])
        connection = None
        try:
            connection = _Connection(process, monotonic=self._clock, timeout=min(5, self._config.timeout_seconds))
            value = connection.line().decode("utf-8").strip()
            if not value.startswith("codex-cli ") or len(value) > 80:
                raise ProviderFailure("unsupported_version")
            return value.removeprefix("codex-cli ")
        except UnicodeError:
            raise ProviderFailure("unsupported_version") from None
        finally:
            if connection is not None:
                connection.close()
            self._stop(process)

    def read(self):
        process = self._start(["--no-daemon", "app-server", "--stdio", "-c", "analytics.enabled=false"])
        connection = None
        try:
            connection = _Connection(process, monotonic=self._clock, timeout=self._config.timeout_seconds)
            connection.request(1, "initialize", {"clientInfo": {"name": "forge_usage", "version": "1"},
                                                   "capabilities": None})
            connection.send({"method": "initialized"})
            account = connection.request(2, "account/read", {"refreshToken": False})
            current = account.get("account")
            if current is None:
                raise ProviderFailure("auth_required")
            if not isinstance(current, dict):
                raise ProviderFailure("malformed_metadata")
            if current.get("type") not in {"chatgpt", "chatgptAuthTokens", "personalAccessToken", "agentIdentity"}:
                return {"account": account, "quota": None, "activity": None, "quota_after": None}
            params = {"supportsLunaReserve": False, "excludeResetCreditDetails": True}
            quota = connection.request(3, "account/rateLimits/read", params)
            try:
                activity = connection.request(4, "account/usage/read")
            except ProviderFailure as error:
                if error.code != "unavailable":
                    raise
                activity = None
            # Bind the account-wide activity read to the same quota principal.
            quota_after = connection.request(5, "account/rateLimits/read", params)
            return {"account": account, "quota": quota, "activity": activity, "quota_after": quota_after}
        finally:
            if connection is not None:
                connection.close()
            self._stop(process)
