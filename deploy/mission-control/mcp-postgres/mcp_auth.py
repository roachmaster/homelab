"""Bearer-token authentication and failed-attempt rate limiting for the Homelab MCP server.

Tokens look like ``hmcp_<id>_<secret>``. Only the SHA-256 of the full token is
stored; tokens carry 256 bits of randomness, so a fast hash is sufficient and a
lookup never needs the plaintext. ``Authenticator`` is the seam for a later
OAuth 2.1 resource-server implementation: anything returning a ``Principal``
from request headers can replace ``BearerTokenAuth``.
"""

from dataclasses import dataclass
from datetime import UTC, datetime
import hashlib
import hmac
import json
import os
from pathlib import Path
import re
import secrets
import stat
import threading
import time

TOKEN_PREFIX = "hmcp_"
TOKEN_PATTERN = re.compile(r"hmcp_([0-9a-f]{12})_([A-Za-z0-9_-]{43})")
TOKEN_ID_PATTERN = re.compile(r"[0-9a-f]{12}")
KNOWN_SCOPES = frozenset({"read"})
FILE_VERSION = 1


class TokenFileError(Exception):
    """The token store is missing, malformed or has unsafe permissions."""


@dataclass(frozen=True)
class Principal:
    token_id: str
    name: str
    scopes: frozenset
    subject: str | None = None


def now_utc():
    return datetime.now(UTC)


def iso(moment):
    return moment.astimezone(UTC).strftime("%Y-%m-%dT%H:%M:%SZ")


def parse_iso(text):
    return datetime.strptime(text, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=UTC)


def token_hash(token):
    return hashlib.sha256(token.encode("ascii")).hexdigest()


def generate_token():
    token_id = secrets.token_hex(6)
    token = f"{TOKEN_PREFIX}{token_id}_{secrets.token_urlsafe(32)}"
    assert TOKEN_PATTERN.fullmatch(token)
    return token_id, token


def empty_store():
    return {"version": FILE_VERSION, "tokens": []}


def validate_store(data):
    if not isinstance(data, dict) or data.get("version") != FILE_VERSION or not isinstance(data.get("tokens"), list):
        raise TokenFileError("token file has an unsupported layout")
    seen = set()
    for record in data["tokens"]:
        if not isinstance(record, dict):
            raise TokenFileError("token record is not an object")
        token_id = record.get("id")
        if not isinstance(token_id, str) or not TOKEN_ID_PATTERN.fullmatch(token_id) or token_id in seen:
            raise TokenFileError("token record has an invalid or duplicate id")
        seen.add(token_id)
        digest = record.get("sha256")
        if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
            raise TokenFileError(f"token {token_id} has an invalid hash")
        scopes = record.get("scopes")
        if not isinstance(scopes, list) or not scopes or not set(scopes) <= KNOWN_SCOPES:
            raise TokenFileError(f"token {token_id} has invalid scopes")
        if not isinstance(record.get("name"), str):
            raise TokenFileError(f"token {token_id} has no name")
        for key in ("created_at", "expires_at"):
            try:
                parse_iso(record[key])
            except (KeyError, TypeError, ValueError):
                raise TokenFileError(f"token {token_id} has an invalid {key}") from None
        if record.get("revoked_at") is not None:
            try:
                parse_iso(record["revoked_at"])
            except (TypeError, ValueError):
                raise TokenFileError(f"token {token_id} has an invalid revoked_at") from None
    return data


def read_store(path, *, require_private=True):
    path = Path(path)
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    except FileNotFoundError:
        raise TokenFileError(f"token file {path} does not exist") from None
    except OSError as error:
        raise TokenFileError(f"token file {path} cannot be opened: {error.strerror}") from None
    with os.fdopen(descriptor, "rb") as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode):
            raise TokenFileError("token file is not a regular file")
        if require_private and info.st_mode & (stat.S_IWGRP | stat.S_IROTH | stat.S_IWOTH):
            raise TokenFileError("token file must not be group-writable or world-accessible")
        raw = source.read(4 * 1024 * 1024 + 1)
    if len(raw) > 4 * 1024 * 1024:
        raise TokenFileError("token file is too large")
    try:
        return validate_store(json.loads(raw)), (info.st_dev, info.st_ino, info.st_mtime_ns, info.st_size)
    except (UnicodeDecodeError, json.JSONDecodeError):
        raise TokenFileError("token file is not valid JSON") from None


class BearerTokenAuth:
    """Validates ``Authorization: Bearer`` against the hashed token file, reloading it on change.

    A token file that cannot be read fails closed: every request is rejected
    until the file is repaired.
    """

    scheme = "Bearer"

    def __init__(self, path, *, clock=now_utc, require_private=True):
        self.path = Path(path)
        self.clock = clock
        self.require_private = require_private
        self._lock = threading.Lock()
        self._signature = None
        self._records = {}
        self._error = None

    def _refresh(self):
        try:
            info = os.stat(self.path)
            signature = (info.st_dev, info.st_ino, info.st_mtime_ns, info.st_size)
        except OSError:
            signature = None
        with self._lock:
            if signature is not None and signature == self._signature and self._error is None:
                return
            try:
                data, signature = read_store(self.path, require_private=self.require_private)
                self._records = {record["id"]: record for record in data["tokens"]}
                self._error = None
            except TokenFileError as error:
                self._records = {}
                self._error = str(error)
            self._signature = signature

    @property
    def error(self):
        self._refresh()
        return self._error

    def authenticate(self, headers):
        """Return a Principal for a valid, unexpired, unrevoked token, else None."""
        header = headers.get("Authorization")
        if not header:
            return None
        scheme, _, credential = header.partition(" ")
        if scheme.lower() != "bearer":
            return None
        match = TOKEN_PATTERN.fullmatch(credential.strip())
        self._refresh()
        if not match:
            return None
        with self._lock:
            record = self._records.get(match.group(1))
        # Always compare so timing does not reveal whether the id exists.
        expected = record["sha256"] if record else "0" * 64
        valid = hmac.compare_digest(expected, token_hash(credential.strip()))
        if not record or not valid or record.get("revoked_at") is not None:
            return None
        if parse_iso(record["expires_at"]) <= self.clock():
            return None
        return Principal(record["id"], record["name"], frozenset(record["scopes"]),
                         subject="static:" + record["id"])


class FailureLimiter:
    """Locks out a client key after repeated authentication failures.

    The key is only used for bucketing failures; it is never an identity.
    A global ceiling limits total failed attempts regardless of key.
    """

    def __init__(self, failures=10, window_seconds=60, lockout_seconds=300,
                 global_failures_per_minute=120, monotonic=time.monotonic):
        self.failures = failures
        self.window = window_seconds
        self.lockout = lockout_seconds
        self.global_limit = global_failures_per_minute
        self.monotonic = monotonic
        self._lock = threading.Lock()
        self._attempts = {}
        self._locked_until = {}
        self._global = []

    def retry_after(self, key):
        """Seconds until ``key`` may try again, or 0 when not limited."""
        now = self.monotonic()
        with self._lock:
            self._global = [moment for moment in self._global if now - moment < 60]
            until = self._locked_until.get(key, 0)
            if until > now:
                return max(1, int(until - now + 0.999))
            self._locked_until.pop(key, None)
            if len(self._global) >= self.global_limit:
                return max(1, int(60 - (now - self._global[0]) + 0.999))
            return 0

    def record_failure(self, key):
        now = self.monotonic()
        with self._lock:
            self._global.append(now)
            attempts = [moment for moment in self._attempts.get(key, ()) if now - moment < self.window]
            attempts.append(now)
            if len(attempts) >= self.failures:
                self._locked_until[key] = now + self.lockout
                attempts = []
            self._attempts[key] = attempts
            if len(self._attempts) > 10000:
                self._attempts = {k: v for k, v in self._attempts.items() if v and now - v[-1] < self.window}
                self._locked_until = {k: v for k, v in self._locked_until.items() if v > now}

    def record_success(self, key):
        with self._lock:
            self._attempts.pop(key, None)
