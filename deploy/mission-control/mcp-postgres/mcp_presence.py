"""Authenticated connection presence, independent of PostgreSQL/execution readiness.

Configuration validation and factory selection are effect-free. Only create()
reads the dedicated reader secret and opens the service-owned SQLite store.
No subject, credential, prompt or execution role is stored in the database.
"""
from contextlib import closing
from datetime import UTC, datetime
import hashlib
import hmac
import math
import os
from pathlib import Path
import re
import sqlite3
import stat
import threading
import time

MAX_AGENTS = 16
FRESH_SECONDS = 180
HEARTBEAT_SECONDS = 60
DEFAULT_PRESENCE = {
    "enabled": False,
    "store_path": "/var/lib/homelab-forge/mcp/presence.sqlite3",
    "reader_token_file": "/etc/homelab-forge/mcp/presence-reader.token",
    "subjects": {},
}
SUBJECT = re.compile(r"(?:static:[0-9a-f]{12}|oauth:hmcpc_[0-9a-f]{16})")
AGENT_ID = re.compile(r"[a-z][a-z0-9_-]{0,63}")


class PresenceError(Exception):
    """A bounded presence configuration, authentication or store failure."""


def validate_config(raw):
    if not isinstance(raw, dict) or set(raw) - DEFAULT_PRESENCE.keys():
        raise PresenceError("presence must be an object with known keys only")
    config = {**DEFAULT_PRESENCE, **raw}
    if type(config["enabled"]) is not bool:
        raise PresenceError("presence.enabled must be a boolean")
    for field in ("store_path", "reader_token_file"):
        value = config[field]
        if not isinstance(value, str) or not value or not Path(value).is_absolute() or ".." in Path(value).parts:
            raise PresenceError(f"presence.{field} must be an absolute path without parent traversal")
    if config["store_path"] == config["reader_token_file"]:
        raise PresenceError("presence store and reader token must be separate files")
    subjects = config["subjects"]
    if not isinstance(subjects, dict) or len(subjects) > MAX_AGENTS:
        raise PresenceError("presence.subjects must contain at most 16 configured subjects")
    agents, selected = {}, {}
    for subject, identity in subjects.items():
        if not isinstance(subject, str) or not SUBJECT.fullmatch(subject):
            raise PresenceError("presence subject must name a static token id or OAuth client id")
        if not isinstance(identity, dict) or set(identity) != {"agent_id", "label"}:
            raise PresenceError("presence identities contain only agent_id and label")
        agent_id, label = identity["agent_id"], identity["label"]
        if not isinstance(agent_id, str) or not AGENT_ID.fullmatch(agent_id):
            raise PresenceError("presence agent_id must be a bounded lowercase identifier")
        if (not isinstance(label, str) or not label.strip() or label != label.strip()
                or len(label) > 80 or not label.isprintable() or len(label.encode("utf-8")) > 160):
            raise PresenceError("presence label must be bounded printable text")
        if agent_id in agents and agents[agent_id] != label:
            raise PresenceError("presence labels for the same agent_id must agree")
        agents[agent_id] = label
        selected[subject] = {"agent_id": agent_id, "label": label}
    config["subjects"] = selected
    return config


def timestamp(value):
    return datetime.fromtimestamp(value, UTC).strftime("%Y-%m-%dT%H:%M:%SZ")


class ReaderTokenAuth:
    """Dedicated local reader credential; never accepted as an MCP principal."""
    def __init__(self, path):
        try:
            descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
            with os.fdopen(descriptor, "rb") as stream:
                info = os.fstat(stream.fileno())
                if (not stat.S_ISREG(info.st_mode) or info.st_uid not in {0, os.geteuid()}
                        or info.st_mode & 0o027):
                    raise PresenceError("presence reader token must be a private regular file")
                raw = stream.read(258)
        except OSError:
            raise PresenceError("presence reader token cannot be read") from None
        token = raw.removesuffix(b"\n")
        if not re.fullmatch(rb"[A-Za-z0-9_-]{32,256}", token):
            raise PresenceError("presence reader token must contain 32 to 256 URL-safe characters")
        self._digest = hashlib.sha256(token).digest()

    def authenticate(self, headers):
        values = headers.get_all("Authorization") if hasattr(headers, "get_all") else None
        header = (values[0] if values and len(values) == 1 else "") if values is not None else headers.get("Authorization", "")
        scheme, _, credential = header.partition(" ")
        # Fixed-size digest comparison also covers missing/wrong-length values.
        actual = hashlib.sha256(credential.encode("utf-8")).digest()
        return hmac.compare_digest(self._digest, actual) and scheme.lower() == "bearer"


class SqlitePresenceStore:
    """At most 16 last-seen timestamps; each operation owns/closes its connection."""
    def __init__(self, path, agent_ids):
        self.path, self.agent_ids = Path(path), frozenset(agent_ids)
        self._lock = threading.Lock()
        try:
            parent = self.path.parent.stat()
            if (not stat.S_ISDIR(parent.st_mode) or parent.st_uid != os.geteuid()
                    or parent.st_mode & 0o022 or self.path.parent.is_symlink()):
                raise PresenceError("presence store parent must be service-owned and private")
            fd = os.open(self.path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
            with os.fdopen(fd, "rb") as stream:
                info = os.fstat(stream.fileno())
                if not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o077 or info.st_size > 1024 * 1024:
                    raise PresenceError("presence store must be a bounded private service-owned file")
            with closing(self._connect()) as db, db:
                tables = {r[0] for r in db.execute("SELECT name FROM sqlite_master WHERE type='table'")}
                if tables - {"agent_presence"} or db.execute("PRAGMA user_version").fetchone()[0] not in (0, 1):
                    raise PresenceError("presence store has an unsupported schema")
                db.execute("PRAGMA max_page_count=256")
                db.execute("CREATE TABLE IF NOT EXISTS agent_presence(agent_id TEXT PRIMARY KEY CHECK(length(agent_id) BETWEEN 1 AND 64), last_seen INTEGER NOT NULL CHECK(typeof(last_seen)='integer' AND last_seen>=0)) WITHOUT ROWID")
                columns = [r[1] for r in db.execute("PRAGMA table_info(agent_presence)")]
                if columns != ["agent_id", "last_seen"]:
                    raise PresenceError("presence store has unexpected columns")
                placeholders = ",".join("?" for _ in self.agent_ids)
                if self.agent_ids:
                    db.execute("DELETE FROM agent_presence WHERE agent_id NOT IN (" + placeholders + ")", tuple(self.agent_ids))
                else:
                    db.execute("DELETE FROM agent_presence")
                db.execute("PRAGMA user_version=1")
        except (OSError, sqlite3.Error):
            raise PresenceError("presence store is unavailable") from None

    def _connect(self):
        return sqlite3.connect(self.path.as_uri() + "?mode=rw", uri=True, timeout=1,
                               autocommit=sqlite3.LEGACY_TRANSACTION_CONTROL)

    def heartbeat(self, agent_id, observed_at):
        if agent_id not in self.agent_ids:
            raise PresenceError("heartbeat identity is not configured")
        try:
            with self._lock, closing(self._connect()) as db, db:
                db.execute("INSERT INTO agent_presence(agent_id,last_seen) VALUES(?,?) ON CONFLICT(agent_id) DO UPDATE SET last_seen=excluded.last_seen WHERE agent_presence.last_seen<>excluded.last_seen", (agent_id, observed_at))
        except sqlite3.Error:
            raise PresenceError("presence store is unavailable") from None

    def last_seen(self):
        try:
            with self._lock, closing(self._connect()) as db:
                rows = dict(db.execute("SELECT agent_id,last_seen FROM agent_presence LIMIT 17"))
                if (len(rows) > MAX_AGENTS or set(rows) - self.agent_ids
                        or any(type(value) is not int or not 0 <= value <= 253402300799 for value in rows.values())):
                    raise PresenceError("presence store contains invalid timestamps or identities")
                return rows
        except sqlite3.Error:
            raise PresenceError("presence store is unavailable") from None


class PresenceProvider:
    def __init__(self, subjects, store, reader, *, clock=time.time):
        self.subjects = {key: dict(value) for key, value in subjects.items()}
        self.agents = {value["agent_id"]: value["label"] for value in self.subjects.values()}
        self.store, self.reader, self.clock = store, reader, clock

    def is_mapped(self, principal):
        return (principal is not None and "read" in principal.scopes
                and getattr(principal, "subject", None) in self.subjects)

    def heartbeat(self, principal):
        if not self.is_mapped(principal):
            raise PresenceError("heartbeat identity is not configured for this authenticated client")
        now = self._now()
        agent = self.subjects[principal.subject]
        self.store.heartbeat(agent["agent_id"], now)
        return {"agent_id": agent["agent_id"], "label": agent["label"], "state": "online",
                "last_seen_at": timestamp(now), "heartbeat_interval_seconds": HEARTBEAT_SECONDS,
                "freshness_seconds": FRESH_SECONDS}

    def _now(self):
        now = self.clock()
        if isinstance(now, bool) or not isinstance(now, (int, float)) or not math.isfinite(now) or not 0 <= now <= 253402300799:
            raise PresenceError("presence server time is unavailable")
        return int(now)

    def snapshot(self):
        now, observed = self._now(), self.store.last_seen()
        agents = []
        for agent_id, label in sorted(self.agents.items()):
            seen = observed.get(agent_id)
            state = "never" if seen is None else ("online" if 0 <= now - seen <= FRESH_SECONDS else "stale")
            agents.append({"agent_id": agent_id, "label": label, "state": state,
                           "last_seen_at": None if seen is None else timestamp(seen)})
        return {"version": 1, "agents": agents}


class PresenceProviderFactory:
    def __init__(self, config, *, store_factory=SqlitePresenceStore, reader_factory=ReaderTokenAuth, clock=time.time):
        self.config = validate_config(config)
        self.store_factory, self.reader_factory, self.clock = store_factory, reader_factory, clock

    def create(self):
        if not self.config["enabled"]:
            return None
        reader = self.reader_factory(self.config["reader_token_file"])
        store = self.store_factory(self.config["store_path"], {row["agent_id"] for row in self.config["subjects"].values()})
        return PresenceProvider(self.config["subjects"], store, reader, clock=self.clock)
