"""Proxy DAO client: runtime callers reach forge_agent through forge-pg-proxy.

The proxy is the only writer to the database (forge-pg-proxy-inventory.md).
Runtime callers that used to hold a direct psycopg2 connection switch their
connect factory to :func:`connect` / :func:`connect_from_config` and keep
calling the same store methods: :class:`ProxyConnection` is a psycopg2
-compatible adapter that translates each SQL statement into exactly one
proxy operation (see proxy_statements.py).

Atomicity model
---------------
Each proxy operation runs as one server-side transaction (mutation plus
actor audit, or one read snapshot). A store method that used to run several
statements inside one transaction now runs one operation per statement, so
multi-statement atomicity is NOT preserved by the adapter alone. Methods
that need several statements to land or fail together must call a composite
proxy operation instead (a single entry point that performs the whole
sequence server-side). The per-caller migration notes in proxy_statements
name the composites each multi-statement method uses.

Semantics the adapter deliberately preserves
----------------------------------------------
* ``cursor.fetchone()[0]`` for ``SELECT <function>(...)`` returns the
  function's jsonb result (or None when the function returns NULL), exactly
  as the direct driver did.
* ``cursor.rowcount`` for an entry-point INSERT/UPDATE/DELETE is 1 when the
  entry point returned the affected row and 0 when it returned NULL (no row
  matched). Direct ``UPDATE ... WHERE <guard>`` statements map onto entry
  points that carry the same guard, so a guarded update that matches nothing
  still reports rowcount 0.
* ``set_config('forge.coding_owner', ...)`` is a no-op: the proxy binds the
  owner scope from its authenticated caller config, and a client-supplied
  owner could never be trusted.
* ``SELECT pg_advisory_xact_lock(...)`` is a no-op that reports success:
  the protected sequence is a single proxy operation, so the advisory lock
  has nothing left to serialize.
* ``SELECT clock_timestamp()`` is answered from the local clock; the value
  is only ever compared against other clock readings or stored as a
  timestamp, and the proxy host and the caller share the same clock.
* ``LISTEN <channel>`` subscribes through the proxy's long-poll endpoint;
  :meth:`ProxyConnection.poll` and :meth:`ProxyConnection.fileno` make the
  connection usable with ``select.select`` exactly like a direct connection,
  and :attr:`ProxyConnection.notifies` holds the delivered notifications.
* Errors surface as :class:`ProxyError`, a ``psycopg2.Error`` subclass, so
  existing ``except psycopg2.Error`` handlers keep working.

Transport selection
-------------------
Callers pick the proxy by configuration, never by code: either the
``FORGE_PG_PROXY_URL`` / ``FORGE_PG_PROXY_TOKEN_FILE`` /
``FORGE_PG_PROXY_CALLER`` environment variables, or a ``proxy`` section in
the caller's config (``{"url": ..., "token_file": ...}``). When no proxy is
configured the callers keep their direct psycopg2 factories, so the switch
is reversible by removing the configuration.
"""

from __future__ import annotations

import json
import os
import select
import socket
import threading
from contextlib import closing
from datetime import datetime, timezone
from http.client import HTTPConnection, HTTPException
from pathlib import Path
from urllib.parse import urlparse

import psycopg2

import proxy_statements
from proxy_statements import Operation, StaticResult, Subscribe
DEFAULT_TIMEOUT_SECONDS = 10.0
DEFAULT_POLL_TIMEOUT_SECONDS = 25.0


class ProxyError(psycopg2.Error):
    """A proxy rejection or transport failure, as a psycopg2 error.

    ``status`` is the HTTP status (None when the proxy was unreachable);
    ``code`` is the proxy's error code (``"transport_error"`` when the
    request never reached the proxy).
    """

    def __init__(self, message, *, status=None, code=None):
        super().__init__(message)
        self.status = status
        self.code = code or "transport_error"

    def __str__(self):
        if self.status is not None:
            return f"{self.args[0]} (proxy {self.status} {self.code})"
        return self.args[0]


def _read_token(token=None, token_file=None):
    if token is not None:
        if not isinstance(token, str) or not token:
            raise ValueError("Proxy token must be a non-empty string")
        return token
    if token_file is None:
        raise ValueError("Proxy token file is required")
    path = Path(token_file)
    value = path.read_text(encoding="utf-8").strip()
    if not value:
        raise ValueError(f"Proxy token file {path} is empty")
    return value


class ProxyClient:
    """One caller identity against one proxy base URL."""

    def __init__(self, url, caller, *, token=None, token_file=None,
                 timeout=DEFAULT_TIMEOUT_SECONDS):
        parsed = urlparse(url)
        if parsed.scheme not in ("http", "https") or not parsed.hostname:
            raise ValueError(f"Invalid proxy URL {url!r}")
        self.url = url.rstrip("/")
        self.host = parsed.hostname
        self.port = parsed.port or (443 if parsed.scheme == "https" else 80)
        self.secure = parsed.scheme == "https"
        self.base_path = parsed.path.rstrip("/")
        self.caller = caller
        self._token = _read_token(token, token_file)
        self.timeout = timeout

    def _request(self, method, path, body=None):
        payload = None if body is None else json.dumps(body).encode("utf-8")
        headers = {"Authorization": f"Bearer {self._token}"}
        if payload is not None:
            headers["Content-Type"] = "application/json"
        try:
            with closing(HTTPConnection(self.host, self.port,
                                        timeout=self.timeout)) as conn:
                conn.request(method, self.base_path + path,
                             body=payload, headers=headers)
                response = conn.getresponse()
                raw = response.read()
        except (OSError, HTTPException, socket.timeout) as error:
            raise ProxyError(f"Proxy {method} {path} unreachable: {error}",
                             code="transport_error") from error
        try:
            data = json.loads(raw.decode("utf-8")) if raw else {}
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise ProxyError(
                f"Proxy {method} {path} returned unreadable body",
                status=response.status, code="transport_error") from error
        if response.status >= 400 or not isinstance(data, dict):
            raise ProxyError(
                f"Proxy {method} {path} failed",
                status=response.status,
                code=data.get("error") if isinstance(data, dict) else None)
        return data

    def operation(self, operation, args=None):
        """Run one proxy operation; returns the decoded ``result``."""
        data = self._request("POST", "/v1/operation",
                             {"operation": operation, "args": args or {}})
        if data.get("status") != "ok":
            raise ProxyError("Proxy operation returned no result",
                             code="transport_error")
        return data.get("result")

    def notifications(self, channel, timeout_seconds=DEFAULT_POLL_TIMEOUT_SECONDS):
        """Long-poll one channel; returns the list of notifications."""
        data = self._request(
            "POST", "/v1/notifications",
            {"channel": channel, "timeout_seconds": timeout_seconds})
        return data.get("notifications") or []

    def health(self):
        """Return the proxy's /health payload."""
        return self._request("GET", "/health")


def connect(url, caller, *, token=None, token_file=None,
            timeout=DEFAULT_TIMEOUT_SECONDS):
    """Return a proxy connection for ``caller`` (a psycopg2-like object)."""
    return ProxyConnection(ProxyClient(url, caller, token=token,
                                       token_file=token_file,
                                       timeout=timeout))


def connect_from_config(*, url=None, token_file=None, caller="coordinator",
                        token=None):
    """Return a proxy connection from environment + explicit config.

    ``url``/``token_file`` fall back to FORGE_PG_PROXY_URL /
    FORGE_PG_PROXY_TOKEN_FILE; ``caller`` falls back to
    FORGE_PG_PROXY_CALLER. Raises ValueError when no URL is configured.
    """
    url = url or os.environ.get("FORGE_PG_PROXY_URL")
    if not url:
        raise ValueError("No proxy URL configured (FORGE_PG_PROXY_URL)")
    token_file = token_file or os.environ.get("FORGE_PG_PROXY_TOKEN_FILE")
    caller = caller or os.environ.get("FORGE_PG_PROXY_CALLER", "coordinator")
    return connect(url, caller, token=token, token_file=token_file)


def proxy_configured(*, url=None, token_file=None):
    """True when a proxy URL (explicit or from the environment) is set."""
    return bool(url or os.environ.get("FORGE_PG_PROXY_URL"))


class ProxyCursor:
    """A psycopg2-compatible cursor backed by proxy operations."""

    def __init__(self, connection, *, dict_rows=False):
        self._connection = connection
        self._dict_rows = dict_rows
        self._rows = []
        self._index = 0
        self.rowcount = -1
        self.description = None
        self.closed = False

    # -- execution -----------------------------------------------------
    def execute(self, sql, params=None):
        if self.closed:
            raise psycopg2.InterfaceError("Cursor is closed")
        try:
            translation = proxy_statements.translate(
                sql, params, caller=self._connection.client.caller)
        except proxy_statements.UnsupportedStatement as error:
            raise ProxyError(str(error), code="unsupported_statement") from error
        if isinstance(translation, StaticResult):
            self._set_result(translation.columns, translation.rows,
                             rowcount=translation.rowcount)
            return self
        if isinstance(translation, Subscribe):
            self._connection.subscribe(translation.channel)
            self._set_result([], [], rowcount=-1)
            return self
        if isinstance(translation, Operation):
            result = self._connection.client.operation(
                translation.operation, translation.args)
            rows, columns, rowcount = translation.shape(result)
            self._set_result(columns, rows, rowcount=rowcount)
            return self
        raise ProxyError(f"Unsupported statement translation {translation!r}")

    def executemany(self, sql, params_list):
        total = 0
        for params in params_list:
            self.execute(sql, params)
            if self.rowcount > 0:
                total += self.rowcount
        self.rowcount = total
        return self

    def _set_result(self, columns, rows, *, rowcount):
        self.description = tuple(
            (name, None, None, None, None, None, None) for name in columns)
        if self._dict_rows:
            self._rows = [dict(zip(columns, row)) for row in rows]
        else:
            self._rows = [tuple(row) for row in rows]
        self._index = 0
        self.rowcount = rowcount

    # -- fetching ------------------------------------------------------
    def fetchone(self):
        if self._index >= len(self._rows):
            return None
        row = self._rows[self._index]
        self._index += 1
        return row

    def fetchmany(self, size=None):
        if size is None:
            size = self.arraysize
        rows = self._rows[self._index:self._index + size]
        self._index += len(rows)
        return rows

    def fetchall(self):
        rows = self._rows[self._index:]
        self._index = len(self._rows)
        return rows

    arraysize = 1

    def close(self):
        self.closed = True

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc, tb):
        self.close()
        return False


class ProxyConnection:
    """A psycopg2-compatible connection backed by forge-pg-proxy."""

    def __init__(self, client):
        self.client = client
        self.autocommit = True
        self.closed = False
        self.notifies = []
        self._channels = set()
        self._notify_reader = None
        self._notify_writer = None
        self._notify_thread = None
        self._notify_stop = threading.Event()

    # -- cursors -------------------------------------------------------
    def cursor(self, cursor_factory=None, **kwargs):
        if self.closed:
            raise psycopg2.InterfaceError("Connection is closed")
        dict_rows = False
        if cursor_factory is not None:
            # RealDictCursor (and its subclasses) ask for dict rows; the
            # default cursor factory asks for tuples.
            dict_rows = issubclass(cursor_factory, psycopg2.extras.RealDictCursor)
        return ProxyCursor(self, dict_rows=dict_rows)

    # -- transaction control (each operation is already committed) -----
    def commit(self):
        return None

    def rollback(self):
        return None

    def close(self):
        self._stop_notify_thread()
        self.closed = True

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc, tb):
        self.close()
        return False

    # -- notifications -------------------------------------------------
    def subscribe(self, channel):
        """Subscribe to a channel; starts the long-poll thread on first use."""
        if channel in self._channels:
            return
        self._channels.add(channel)
        if self._notify_thread is None:
            reader, writer = os.pipe()
            os.set_blocking(reader, False)
            self._notify_reader, self._notify_writer = reader, writer
            self._notify_stop.clear()
            thread = threading.Thread(target=self._poll_loop, daemon=True,
                                      name="proxy-notify-poll")
            thread.start()
            self._notify_thread = thread

    def _poll_loop(self):
        while not self._notify_stop.is_set():
            for channel in sorted(self._channels):
                if self._notify_stop.is_set():
                    break
                try:
                    notes = self.client.notifications(
                        channel, timeout_seconds=DEFAULT_POLL_TIMEOUT_SECONDS)
                except ProxyError:
                    # A failed long-poll is a transient transport hiccup,
                    # not a lost notification: the caller re-reads state
                    # on wake, and the next poll resumes the stream.
                    notes = []
                for note in notes:
                    self.notifies.append(note)
                    try:
                        os.write(self._notify_writer, b"\x01")
                    except OSError:
                        pass
            # A failed poll above already consumed its timeout; a
            # successful empty poll returns after the server timeout.

    def _stop_notify_thread(self):
        if self._notify_thread is not None:
            self._notify_stop.set()
            self._notify_thread.join(timeout=2.0)
            self._notify_thread = None
        for fd in (self._notify_reader, self._notify_writer):
            if fd is not None:
                try:
                    os.close(fd)
                except OSError:
                    pass
        self._notify_reader = self._notify_writer = None
        self._channels.clear()

    def fileno(self):
        """The read end of the notify pipe, so select.select works."""
        if self._notify_reader is None:
            raise psycopg2.InterfaceError(
                "Connection is not listening on any channel")
        return self._notify_reader

    def poll(self):
        """Drain the notify pipe; notifications are already in .notifies."""
        if self._notify_reader is None:
            return
        try:
            while os.read(self._notify_reader, 4096):
                pass
        except BlockingIOError:
            pass
        except OSError:
            pass

    # -- misc psycopg2 surface -----------------------------------------
    def set_isolation_level(self, level):
        return None

    def get_backend_pid(self):
        return 0


def now_utc():
    """Local clock as a timezone-aware UTC datetime (clock_timestamp)."""
    return datetime.now(timezone.utc)
