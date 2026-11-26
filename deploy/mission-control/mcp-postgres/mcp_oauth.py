#!/usr/bin/env python3
"""Minimal OAuth 2.1 authorization server for the Homelab MCP server.

Authorization Code + PKCE (S256 only) with pre-registered confidential
clients (no dynamic client registration), rotating refresh tokens with reuse
detection, RFC 7009 revocation and a single-operator password login. The
existing static ``hmcp_`` tokens keep working; ``DualAuth`` accepts either.

Secrets at rest are hashed: client secrets, codes and tokens with SHA-256
(they carry 256 random bits), the operator password with scrypt.

Files (defaults):
  /etc/homelab-forge/mcp/oauth-clients.json  root:homelab_mcp 0640, written by this CLI
  /etc/homelab-forge/mcp/operator.json       root:homelab_mcp 0640, scrypt hash only
  /var/lib/homelab-forge/mcp/oauth.sqlite3   homelab_mcp 0600, codes/grants/tokens

CLI (run as root):
  mcp_oauth.py client create --name gemini --secret-file /root/x.txt   # prints client_id only
  mcp_oauth.py client list | client revoke <client_id>
  mcp_oauth.py operator set-password (--password-file PATH | --stdin | --generate PATH)
  mcp_oauth.py grant list | grant revoke <grant_id> | grant revoke-client <client_id>
"""

import argparse
import base64
from contextlib import contextmanager
from dataclasses import dataclass
import fcntl
import hashlib
import hmac
import html
import json
import os
from pathlib import Path
import re
import secrets
import sqlite3
import stat
import sys
import threading
import time
from urllib.parse import urlencode, urlsplit

sys.path.insert(0, str(Path(__file__).resolve().parent))

from mcp_auth import Principal  # noqa: E402

DEFAULT_CLIENTS_FILE = "/etc/homelab-forge/mcp/oauth-clients.json"
DEFAULT_OPERATOR_FILE = "/etc/homelab-forge/mcp/operator.json"
DEFAULT_STATE_DB = "/var/lib/homelab-forge/mcp/oauth.sqlite3"
SUPPORTED_SCOPES = ("read",)
GOOGLE_REDIRECT_PREFIXES = tuple(
    f"https://{host}/{kind}/"
    for host in ("oauth-redirect.googleusercontent.com", "oauth-redirect-sandbox.googleusercontent.com",
                 "oauth-redirect-test.googleusercontent.com")
    for kind in ("r", "a"))
ACCESS_PREFIX = "hmcpat_"
REFRESH_PREFIX = "hmcprt_"
CLIENT_ID_PATTERN = re.compile(r"hmcpc_[0-9a-f]{16}")
SECRET_PATTERN = re.compile(r"[A-Za-z0-9_-]{43}")
OPAQUE_PATTERN = re.compile(r"(hmcpat_|hmcprt_)?[A-Za-z0-9_-]{43}")
VERIFIER_PATTERN = re.compile(r"[A-Za-z0-9._~-]{43,128}")
CHALLENGE_PATTERN = re.compile(r"[A-Za-z0-9_-]{43}")
SCRYPT = {"n": 2 ** 15, "r": 8, "p": 1}
SCRYPT_MAXMEM = 64 * 1024 * 1024
FILE_VERSION = 1


class OAuthFileError(Exception):
    """A client or operator file is missing, malformed or unsafe."""


class OAuthError(Exception):
    """An RFC 6749 error response."""

    def __init__(self, error, description="", status=400):
        super().__init__(description or error)
        self.error, self.description, self.status = error, description, status

    def body(self):
        body = {"error": self.error}
        if self.description:
            body["error_description"] = self.description
        return body


def sha256_hex(text):
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def s256(verifier):
    return base64.urlsafe_b64encode(hashlib.sha256(verifier.encode("ascii")).digest()).rstrip(b"=").decode()


def redirect_uri_allowed(uri, prefixes=GOOGLE_REDIRECT_PREFIXES):
    """Exact, case-sensitive prefix match on a parsed absolute https URL. No open redirect."""
    if not isinstance(uri, str) or len(uri) > 2048:
        return False
    if any(ord(c) < 0x21 or ord(c) > 0x7e for c in uri) or "\\" in uri or "#" in uri:
        return False
    try:
        parts = urlsplit(uri)
        port = parts.port
    except ValueError:
        return False
    if parts.scheme != "https" or port is not None or "@" in parts.netloc:
        return False
    segments = parts.path.split("/")
    if any(s in (".", "..") for s in segments) or "%2e" in parts.path.lower() or "%2f" in parts.path.lower():
        return False
    for prefix in prefixes:
        expected = urlsplit(prefix)
        if (uri.startswith(prefix) and len(parts.path) > len(expected.path)
                and parts.netloc == expected.netloc and parts.path.startswith(expected.path)):
            return True
    return False


def _read_private_json(path, label):
    path = Path(path)
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    except FileNotFoundError:
        raise OAuthFileError(f"{label} {path} does not exist") from None
    except OSError as error:
        raise OAuthFileError(f"{label} {path} cannot be opened: {error.strerror}") from None
    with os.fdopen(descriptor, "rb") as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode):
            raise OAuthFileError(f"{label} is not a regular file")
        if info.st_mode & (stat.S_IWGRP | stat.S_IROTH | stat.S_IWOTH):
            raise OAuthFileError(f"{label} must not be group-writable or world-accessible")
        raw = source.read(1024 * 1024 + 1)
    if len(raw) > 1024 * 1024:
        raise OAuthFileError(f"{label} is too large")
    try:
        data = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError):
        raise OAuthFileError(f"{label} is not valid JSON") from None
    if not isinstance(data, dict) or data.get("version") != FILE_VERSION:
        raise OAuthFileError(f"{label} has an unsupported layout")
    return data, (info.st_dev, info.st_ino, info.st_mtime_ns, info.st_size)


def validate_clients(data):
    clients = data.get("clients")
    if not isinstance(clients, list):
        raise OAuthFileError("clients file has no clients list")
    seen = set()
    for client in clients:
        if not isinstance(client, dict) or not CLIENT_ID_PATTERN.fullmatch(str(client.get("client_id"))):
            raise OAuthFileError("client record has an invalid client_id")
        if client["client_id"] in seen:
            raise OAuthFileError("duplicate client_id")
        seen.add(client["client_id"])
        if not re.fullmatch(r"[0-9a-f]{64}", str(client.get("secret_sha256"))):
            raise OAuthFileError(f"client {client['client_id']} has an invalid secret hash")
        prefixes = client.get("redirect_prefixes")
        if (not isinstance(prefixes, list) or not prefixes
                or not all(isinstance(p, str) and p.startswith("https://") and p.endswith("/") for p in prefixes)):
            raise OAuthFileError(f"client {client['client_id']} has invalid redirect_prefixes")
        scopes = client.get("scopes")
        if not isinstance(scopes, list) or not scopes or not set(scopes) <= set(SUPPORTED_SCOPES):
            raise OAuthFileError(f"client {client['client_id']} has invalid scopes")
    return data


def validate_operator(data):
    record = data.get("scrypt")
    if (not isinstance(record, dict) or not isinstance(data.get("username"), str)
            or set(record) != {"n", "r", "p", "salt", "hash"}):
        raise OAuthFileError("operator file has an unsupported layout")
    try:
        base64.b64decode(record["salt"], validate=True)
        base64.b64decode(record["hash"], validate=True)
    except (ValueError, TypeError):
        raise OAuthFileError("operator file has invalid encoding") from None
    return data


def hash_password(password):
    salt = secrets.token_bytes(16)
    digest = hashlib.scrypt(password.encode("utf-8"), salt=salt, maxmem=SCRYPT_MAXMEM, dklen=32, **SCRYPT)
    return {**SCRYPT, "salt": base64.b64encode(salt).decode(), "hash": base64.b64encode(digest).decode()}


def verify_password(record, password):
    try:
        digest = hashlib.scrypt(password.encode("utf-8"), salt=base64.b64decode(record["salt"]),
                                n=int(record["n"]), r=int(record["r"]), p=int(record["p"]),
                                maxmem=SCRYPT_MAXMEM, dklen=32)
    except (ValueError, TypeError, KeyError, MemoryError):
        return False
    return hmac.compare_digest(digest, base64.b64decode(record["hash"]))


class _ReloadingFile:
    def __init__(self, path, label, validator):
        self.path, self.label, self.validator = Path(path), label, validator
        self._lock = threading.Lock()
        self._signature = None
        self._data = None
        self._error = None

    def get(self):
        try:
            info = os.stat(self.path)
            signature = (info.st_dev, info.st_ino, info.st_mtime_ns, info.st_size)
        except OSError:
            signature = None
        with self._lock:
            if signature is None or signature != self._signature or self._error is not None:
                try:
                    data, signature = _read_private_json(self.path, self.label)
                    self._data, self._error = self.validator(data), None
                except OAuthFileError as error:
                    self._data, self._error = None, str(error)
                self._signature = signature
            return self._data, self._error


SCHEMA = """
CREATE TABLE IF NOT EXISTS codes (
  code_hash TEXT PRIMARY KEY, client_id TEXT NOT NULL, redirect_uri TEXT NOT NULL,
  challenge TEXT NOT NULL, scope TEXT NOT NULL, expires REAL NOT NULL,
  used_at REAL, grant_id TEXT);
CREATE TABLE IF NOT EXISTS grants (
  grant_id TEXT PRIMARY KEY, client_id TEXT NOT NULL, scope TEXT NOT NULL,
  created REAL NOT NULL, revoked_at REAL, revoke_reason TEXT);
CREATE TABLE IF NOT EXISTS tokens (
  token_hash TEXT PRIMARY KEY, grant_id TEXT NOT NULL REFERENCES grants(grant_id),
  kind TEXT NOT NULL CHECK (kind IN ('access', 'refresh')), expires REAL NOT NULL,
  used_at REAL, revoked_at REAL);
CREATE INDEX IF NOT EXISTS tokens_grant ON tokens(grant_id);
"""


class OAuthStore:
    """SQLite-backed codes, grants and tokens. Every value stored is a hash."""

    def __init__(self, path, clock=time.time):
        self.path = Path(path)
        self.clock = clock
        self._lock = threading.Lock()
        with self.connect() as db:
            db.executescript(SCHEMA)

    @contextmanager
    def connect(self):
        old = os.umask(0o077)
        try:
            db = sqlite3.connect(self.path, timeout=5, isolation_level=None)
        finally:
            os.umask(old)
        try:
            db.execute("PRAGMA journal_mode=DELETE")
            db.execute("PRAGMA foreign_keys=ON")
            yield db
        finally:
            db.close()

    @contextmanager
    def transaction(self):
        with self._lock, self.connect() as db:
            db.execute("BEGIN IMMEDIATE")
            try:
                yield db
                db.execute("COMMIT")
            except BaseException:
                db.execute("ROLLBACK")
                raise

    def create_code(self, client_id, redirect_uri, challenge, scope, ttl):
        code = secrets.token_urlsafe(32)
        now = self.clock()
        with self.transaction() as db:
            db.execute("DELETE FROM codes WHERE expires < ?", (now - 86400,))
            db.execute("INSERT INTO codes VALUES (?,?,?,?,?,?,NULL,NULL)",
                       (sha256_hex(code), client_id, redirect_uri, challenge, scope, now + ttl))
        return code

    def _issue(self, db, grant_id, access_ttl, refresh_ttl):
        now = self.clock()
        access = ACCESS_PREFIX + secrets.token_urlsafe(32)
        refresh = REFRESH_PREFIX + secrets.token_urlsafe(32)
        db.execute("INSERT INTO tokens VALUES (?,?,?,?,NULL,NULL)", (sha256_hex(access), grant_id, "access", now + access_ttl))
        db.execute("INSERT INTO tokens VALUES (?,?,?,?,NULL,NULL)", (sha256_hex(refresh), grant_id, "refresh", now + refresh_ttl))
        db.execute("DELETE FROM tokens WHERE expires < ?", (now - 86400,))
        return access, refresh

    @staticmethod
    def _revoke_grant(db, grant_id, reason, now):
        db.execute("UPDATE grants SET revoked_at=?, revoke_reason=? WHERE grant_id=? AND revoked_at IS NULL",
                   (now, reason, grant_id))
        db.execute("UPDATE tokens SET revoked_at=? WHERE grant_id=? AND revoked_at IS NULL", (now, grant_id))

    def exchange_code(self, client_id, code, redirect_uri, verifier, access_ttl, refresh_ttl):
        if not isinstance(code, str) or not OPAQUE_PATTERN.fullmatch(code):
            raise OAuthError("invalid_grant", "unknown authorization code")
        if not isinstance(verifier, str) or not VERIFIER_PATTERN.fullmatch(verifier):
            raise OAuthError("invalid_grant", "code_verifier is missing or malformed")
        now = self.clock()
        failure = None
        with self.transaction() as db:
            row = db.execute("SELECT client_id, redirect_uri, challenge, scope, expires, used_at, grant_id "
                             "FROM codes WHERE code_hash=?", (sha256_hex(code),)).fetchone()
            if row is None:
                raise OAuthError("invalid_grant", "unknown authorization code")
            owner, bound_uri, challenge, scope, expires, used_at, prior_grant = row
            if used_at is not None:
                if prior_grant:
                    self._revoke_grant(db, prior_grant, "authorization code reused", now)
                failure = OAuthError("invalid_grant", "authorization code was already used")
            else:
                db.execute("UPDATE codes SET used_at=? WHERE code_hash=?", (now, sha256_hex(code)))
                if owner != client_id:
                    failure = OAuthError("invalid_grant", "code was issued to another client")
                elif expires <= now:
                    failure = OAuthError("invalid_grant", "authorization code expired")
                elif not isinstance(redirect_uri, str) or not hmac.compare_digest(bound_uri, redirect_uri):
                    failure = OAuthError("invalid_grant", "redirect_uri does not match")
                elif not hmac.compare_digest(s256(verifier), challenge):
                    failure = OAuthError("invalid_grant", "PKCE verification failed")
            if failure is None:
                grant_id = secrets.token_hex(8)
                db.execute("INSERT INTO grants VALUES (?,?,?,?,NULL,NULL)", (grant_id, client_id, scope, now))
                db.execute("UPDATE codes SET grant_id=? WHERE code_hash=?", (grant_id, sha256_hex(code)))
                access, refresh = self._issue(db, grant_id, access_ttl, refresh_ttl)
        if failure is not None:
            raise failure
        return access, refresh, scope

    def refresh(self, client_id, refresh_token, requested_scope, access_ttl, refresh_ttl):
        if not isinstance(refresh_token, str) or not refresh_token.startswith(REFRESH_PREFIX):
            raise OAuthError("invalid_grant", "unknown refresh token")
        now = self.clock()
        failure = None
        with self.transaction() as db:
            row = db.execute(
                "SELECT t.grant_id, t.expires, t.used_at, t.revoked_at, g.client_id, g.scope, g.revoked_at "
                "FROM tokens t JOIN grants g USING (grant_id) WHERE t.token_hash=? AND t.kind='refresh'",
                (sha256_hex(refresh_token),)).fetchone()
            if row is None:
                raise OAuthError("invalid_grant", "unknown refresh token")
            grant_id, expires, used_at, revoked_at, owner, scope, grant_revoked = row
            if owner != client_id:
                failure = OAuthError("invalid_grant", "refresh token was issued to another client")
            elif grant_revoked is not None or revoked_at is not None:
                failure = OAuthError("invalid_grant", "refresh token was revoked")
            elif used_at is not None:
                self._revoke_grant(db, grant_id, "refresh token reused", now)
                failure = OAuthError("invalid_grant", "refresh token was already used; grant revoked")
            elif expires <= now:
                failure = OAuthError("invalid_grant", "refresh token expired")
            elif requested_scope and not set(requested_scope.split()) <= set(scope.split()):
                failure = OAuthError("invalid_scope", "requested scope exceeds the original grant")
            else:
                db.execute("UPDATE tokens SET used_at=? WHERE token_hash=?", (now, sha256_hex(refresh_token)))
                access, refresh = self._issue(db, grant_id, access_ttl, refresh_ttl)
        if failure is not None:
            raise failure
        return access, refresh, scope

    def revoke_token(self, client_id, token):
        """RFC 7009: revoke a token issued to ``client_id``. Unknown tokens are ignored."""
        if not isinstance(token, str) or not OPAQUE_PATTERN.fullmatch(token):
            return False
        now = self.clock()
        with self.transaction() as db:
            row = db.execute("SELECT t.kind, t.grant_id, g.client_id FROM tokens t JOIN grants g USING (grant_id) "
                             "WHERE t.token_hash=?", (sha256_hex(token),)).fetchone()
            if row is None or row[2] != client_id:
                return False
            kind, grant_id, _ = row
            if kind == "refresh":
                self._revoke_grant(db, grant_id, "revoked by client", now)
            else:
                db.execute("UPDATE tokens SET revoked_at=? WHERE token_hash=? AND revoked_at IS NULL",
                           (now, sha256_hex(token)))
            return True

    def lookup_access(self, token):
        """Return (grant_id, client_id, scope) for a live access token, else None."""
        now = self.clock()
        with self.connect() as db:
            row = db.execute(
                "SELECT t.grant_id, g.client_id, g.scope FROM tokens t JOIN grants g USING (grant_id) "
                "WHERE t.token_hash=? AND t.kind='access' AND t.revoked_at IS NULL AND g.revoked_at IS NULL "
                "AND t.expires > ?", (sha256_hex(token), now)).fetchone()
        return row

    def list_grants(self):
        now = self.clock()
        with self.connect() as db:
            return db.execute(
                "SELECT g.grant_id, g.client_id, g.scope, g.created, g.revoked_at, g.revoke_reason, "
                "(SELECT COUNT(*) FROM tokens t WHERE t.grant_id=g.grant_id AND t.kind='refresh' "
                " AND t.used_at IS NULL AND t.revoked_at IS NULL AND t.expires > ?) "
                "FROM grants g ORDER BY g.created", (now,)).fetchall()

    def revoke_grants(self, grant_id=None, client_id=None, reason="revoked by operator"):
        now = self.clock()
        with self.transaction() as db:
            if grant_id:
                ids = [r[0] for r in db.execute("SELECT grant_id FROM grants WHERE grant_id=?", (grant_id,))]
            else:
                ids = [r[0] for r in db.execute("SELECT grant_id FROM grants WHERE client_id=?", (client_id,))]
            for gid in ids:
                self._revoke_grant(db, gid, reason, now)
            return len(ids)


DEFAULT_OAUTH = {
    "enabled": False,
    "issuer": "https://mcp.thedevex.com",
    "clients_file": DEFAULT_CLIENTS_FILE,
    "operator_file": DEFAULT_OPERATOR_FILE,
    "state_db": DEFAULT_STATE_DB,
    "code_ttl_seconds": 60,
    "access_ttl_seconds": 3600,
    "refresh_ttl_days": 90,
    "login_rate_limit": {"failures": 5, "window_seconds": 300, "lockout_seconds": 900,
                         "global_failures_per_minute": 30},
}


@dataclass
class AuthorizeRequest:
    client: dict
    redirect_uri: str
    state: str
    challenge: str
    scope: str


class OAuthServer:
    def __init__(self, config, clock=time.time):
        self.config = config
        self.issuer = config["issuer"].rstrip("/")
        self.resource = self.issuer + "/mcp"
        self.clients = _ReloadingFile(config["clients_file"], "OAuth clients file", validate_clients)
        self.operator = _ReloadingFile(config["operator_file"], "operator file", validate_operator)
        self.store = OAuthStore(config["state_db"], clock=clock)
        self.csrf_key = secrets.token_bytes(32)
        self.refresh_ttl = int(config["refresh_ttl_days"]) * 86400

    # --- metadata --------------------------------------------------------
    def protected_resource_metadata(self):
        return {"resource": self.resource, "authorization_servers": [self.issuer],
                "scopes_supported": list(SUPPORTED_SCOPES), "bearer_methods_supported": ["header"],
                "resource_name": "Homelab PostgreSQL (read-only)"}

    def authorization_server_metadata(self):
        return {
            "issuer": self.issuer,
            "authorization_endpoint": self.issuer + "/oauth/authorize",
            "token_endpoint": self.issuer + "/oauth/token",
            "revocation_endpoint": self.issuer + "/oauth/revoke",
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "token_endpoint_auth_methods_supported": ["client_secret_post", "client_secret_basic"],
            "revocation_endpoint_auth_methods_supported": ["client_secret_post", "client_secret_basic"],
            "code_challenge_methods_supported": ["S256"],
            "scopes_supported": list(SUPPORTED_SCOPES),
        }

    def resource_metadata_url(self):
        return self.issuer + "/.well-known/oauth-protected-resource"

    def problems(self):
        problems = []
        for name, source in (("clients", self.clients), ("operator", self.operator)):
            _, error = source.get()
            if error:
                problems.append(error)
        return problems

    # --- clients ---------------------------------------------------------
    def find_client(self, client_id):
        data, _ = self.clients.get()
        if not data or not isinstance(client_id, str):
            return None
        for client in data["clients"]:
            if client["client_id"] == client_id and client.get("revoked_at") is None:
                return client
        return None

    def authenticate_client(self, headers, form):
        """Return the client record or raise invalid_client (401)."""
        basic = headers.get("Authorization") or ""
        client_id = secret = None
        used_basic = False
        if basic[:6].lower() == "basic ":
            used_basic = True
            if "client_secret" in form:
                raise OAuthError("invalid_request", "use exactly one client authentication method")
            try:
                decoded = base64.b64decode(basic[6:].strip(), validate=True).decode("utf-8")
            except (ValueError, UnicodeDecodeError):
                raise OAuthError("invalid_client", "malformed Basic credentials", 401) from None
            raw_id, sep, raw_secret = decoded.partition(":")
            if not sep:
                raise OAuthError("invalid_client", "malformed Basic credentials", 401)
            from urllib.parse import unquote_plus
            client_id, secret = unquote_plus(raw_id), unquote_plus(raw_secret)
            if form.get("client_id") not in (None, client_id):
                raise OAuthError("invalid_request", "client_id does not match the credentials")
        else:
            client_id, secret = form.get("client_id"), form.get("client_secret")
        client = self.find_client(client_id)
        expected = client["secret_sha256"] if client else "0" * 64
        valid = hmac.compare_digest(expected, sha256_hex(secret or ""))
        if not client or not secret or not valid:
            error = OAuthError("invalid_client", "client authentication failed", 401)
            error.basic = used_basic
            raise error
        return client

    # --- authorization endpoint ---------------------------------------------
    def validate_authorize(self, params):
        """Raise ('page', message) for errors that must not redirect, or return
        (AuthorizeRequest, None) / (partial, OAuthError) for redirectable errors."""
        client = self.find_client(params.get("client_id"))
        if client is None:
            raise ValueError("Unknown or revoked client_id.")
        redirect_uri = params.get("redirect_uri")
        if not redirect_uri or not redirect_uri_allowed(redirect_uri, tuple(client["redirect_prefixes"])):
            raise ValueError("redirect_uri is not registered for this client.")
        state = params.get("state") or ""
        if len(state) > 1024:
            raise ValueError("state is too long.")
        request = AuthorizeRequest(client, redirect_uri, state, params.get("code_challenge") or "", "")
        if params.get("response_type") != "code":
            return request, OAuthError("unsupported_response_type", "response_type must be code")
        if params.get("code_challenge_method") != "S256":
            return request, OAuthError("invalid_request", "PKCE with code_challenge_method=S256 is required")
        if not CHALLENGE_PATTERN.fullmatch(request.challenge):
            return request, OAuthError("invalid_request", "code_challenge must be a base64url S256 value")
        scope = (params.get("scope") or "read").split()
        if not scope or not set(scope) <= set(client["scopes"]):
            return request, OAuthError("invalid_scope", "only the read scope is available")
        resource = params.get("resource")
        if resource is not None and resource.rstrip("/") not in (self.resource, self.issuer):
            return request, OAuthError("invalid_target", "unknown resource")
        request.scope = " ".join(sorted(set(scope)))
        return request, None

    def error_redirect(self, request, error):
        query = {"error": error.error, "error_description": error.description}
        if request.state:
            query["state"] = request.state
        return self._append(request.redirect_uri, query)

    def code_redirect(self, request):
        code = self.store.create_code(request.client["client_id"], request.redirect_uri, request.challenge,
                                      request.scope, int(self.config["code_ttl_seconds"]))
        query = {"code": code}
        if request.state:
            query["state"] = request.state
        return self._append(request.redirect_uri, query)

    @staticmethod
    def _append(uri, query):
        return uri + ("&" if "?" in uri else "?") + urlencode(query)

    def check_password(self, password):
        data, _ = self.operator.get()
        if not data or not isinstance(password, str) or not password or len(password) > 1024:
            # Spend comparable time so a missing file is not a fast oracle.
            hash_password("x")
            return False
        return verify_password(data["scrypt"], password)

    def csrf_value(self, cookie, params):
        binding = json.dumps([params.get(k) for k in ("client_id", "redirect_uri", "state", "code_challenge",
                                                       "scope")], separators=(",", ":"))
        return hmac.new(self.csrf_key, (cookie + "|" + binding).encode(), hashlib.sha256).hexdigest()

    def consent_page(self, request, params, csrf, message=""):
        fields = "".join(
            f'<input type="hidden" name="{html.escape(k)}" value="{html.escape(params.get(k) or "")}">'
            for k in ("response_type", "client_id", "redirect_uri", "state", "code_challenge",
                      "code_challenge_method", "scope", "resource") if params.get(k) is not None)
        name = html.escape(request.client.get("name") or request.client["client_id"])
        host = html.escape(urlsplit(request.redirect_uri).netloc)
        note = f'<p class="err">{html.escape(message)}</p>' if message else ""
        return f"""<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><title>Homelab MCP: authorize</title>
<style>body{{font:16px system-ui,sans-serif;max-width:28rem;margin:3rem auto;padding:0 1rem;color:#222}}
.box{{border:1px solid #ccc;border-radius:8px;padding:1.25rem}}input[type=password]{{width:100%;padding:.5rem;
font-size:1rem;box-sizing:border-box}}button{{padding:.55rem 1rem;font-size:1rem;margin-top:1rem;margin-right:.5rem}}
.err{{color:#b00020}}code{{background:#f3f3f3;padding:0 .25rem}}</style></head><body><div class="box">
<h1 style="font-size:1.25rem">Allow read-only access?</h1>
<p><b>{name}</b> wants <b>read-only</b> access to the Homelab PostgreSQL MCP server
(<code>{html.escape(self.resource)}</code>). It can run SELECT queries and list tables. It cannot change data.</p>
<p>You will be returned to <code>{host}</code>.</p>{note}
<form method="post" action="/oauth/authorize" autocomplete="off">{fields}
<input type="hidden" name="csrf" value="{csrf}">
<label for="pw">Operator password</label><input id="pw" type="password" name="password" autofocus>
<button type="submit" name="decision" value="allow">Allow read-only access</button>
<button type="submit" name="decision" value="deny">Deny</button></form></div></body></html>"""

    def form_action_sources(self):
        data, _ = self.clients.get()
        origins = {"'self'"}
        for client in (data or {}).get("clients", []):
            for prefix in client["redirect_prefixes"]:
                parts = urlsplit(prefix)
                origins.add(f"{parts.scheme}://{parts.netloc}")
        return " ".join(sorted(origins))

    # --- token endpoint ------------------------------------------------------
    def token(self, headers, form):
        client = self.authenticate_client(headers, form)
        grant_type = form.get("grant_type")
        access_ttl = int(self.config["access_ttl_seconds"])
        if grant_type == "authorization_code":
            access, refresh, scope = self.store.exchange_code(
                client["client_id"], form.get("code"), form.get("redirect_uri"), form.get("code_verifier"),
                access_ttl, self.refresh_ttl)
        elif grant_type == "refresh_token":
            access, refresh, scope = self.store.refresh(client["client_id"], form.get("refresh_token"),
                                                        form.get("scope"), access_ttl, self.refresh_ttl)
        else:
            raise OAuthError("unsupported_grant_type", "grant_type must be authorization_code or refresh_token")
        return {"access_token": access, "token_type": "Bearer", "expires_in": access_ttl,
                "refresh_token": refresh, "scope": scope}

    def revoke(self, headers, form):
        client = self.authenticate_client(headers, form)
        self.store.revoke_token(client["client_id"], form.get("token"))

    # --- resource server -------------------------------------------------------
    def authenticate_bearer(self, credential):
        if not credential.startswith(ACCESS_PREFIX):
            return None
        row = self.store.lookup_access(credential)
        if row is None:
            return None
        grant_id, client_id, scope = row
        client = self.find_client(client_id)
        if client is None:
            return None
        return Principal(f"oauth-{grant_id[:12]}", f"oauth:{client.get('name') or client_id}",
                         frozenset(scope.split()))


class DualAuth:
    """Accept static ``hmcp_`` tokens (unchanged) or OAuth access tokens."""

    scheme = "Bearer"

    def __init__(self, static, oauth):
        self.static, self.oauth = static, oauth

    @property
    def error(self):
        return self.static.error

    def authenticate(self, headers):
        header = headers.get("Authorization") or ""
        scheme, _, credential = header.partition(" ")
        credential = credential.strip()
        if scheme.lower() == "bearer" and self.oauth is not None and credential.startswith(ACCESS_PREFIX):
            try:
                return self.oauth.authenticate_bearer(credential)
            except sqlite3.Error:
                return None
        return self.static.authenticate(headers)


# --- CLI ---------------------------------------------------------------------
def _require_root():
    if os.geteuid() != 0 and os.environ.get("HOMELAB_MCP_TOKENS_ALLOW_NONROOT") != "1":
        sys.exit("mcp_oauth.py must run as root")


def _service_gid():
    if os.geteuid() != 0:
        return None
    import grp
    try:
        return grp.getgrnam("homelab_mcp").gr_gid
    except KeyError:
        return None


def _write_json_atomic(path, data, mode=0o640):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    try:
        with os.fdopen(fd, "w") as out:
            json.dump(data, out, indent=2, sort_keys=True)
            out.write("\n")
            out.flush()
            os.fsync(out.fileno())
        gid = _service_gid()
        if gid is not None:
            os.chown(tmp, 0, gid)
        os.chmod(tmp, mode)
        os.replace(tmp, path)
    except BaseException:
        tmp.unlink(missing_ok=True)
        raise


def _write_secret_file(path, text):
    path = Path(path)
    if path.exists() or path.is_symlink():
        sys.exit(f"refusing to overwrite existing {path}")
    path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "w") as out:
        out.write(text)
        out.flush()
        os.fsync(out.fileno())


@contextmanager
def _locked(path):
    lock_path = Path(str(path) + ".lock")
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(lock_path, os.O_RDWR | os.O_CREAT, 0o600)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        yield
    finally:
        os.close(fd)


def _load_clients(path):
    if not Path(path).exists():
        return {"version": FILE_VERSION, "clients": []}
    data, _ = _read_private_json(path, "OAuth clients file")
    return validate_clients(data)


def _now_iso():
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def _drop_to_db_owner(db_path):
    """Run state-db commands as the file owner so root never leaves root-owned journals."""
    if os.geteuid() != 0:
        return
    try:
        info = os.stat(db_path)
    except FileNotFoundError:
        info = os.stat(Path(db_path).parent)
    if info.st_uid != 0:
        os.setgid(info.st_gid)
        os.setuid(info.st_uid)


def main(argv=None):
    parser = argparse.ArgumentParser(description="Manage Homelab MCP OAuth clients, grants and operator login.")
    parser.add_argument("--clients-file", default=DEFAULT_CLIENTS_FILE)
    parser.add_argument("--operator-file", default=DEFAULT_OPERATOR_FILE)
    parser.add_argument("--state-db", default=DEFAULT_STATE_DB)
    sub = parser.add_subparsers(dest="area", required=True)
    client = sub.add_parser("client").add_subparsers(dest="action", required=True)
    create = client.add_parser("create")
    create.add_argument("--name", required=True)
    create.add_argument("--secret-file", required=True, help="new 0600 file receiving client_id and secret")
    client.add_parser("list").add_argument("--json", action="store_true")
    client.add_parser("revoke").add_argument("client_id")
    operator = sub.add_parser("operator").add_subparsers(dest="action", required=True)
    setpw = operator.add_parser("set-password")
    source = setpw.add_mutually_exclusive_group(required=True)
    source.add_argument("--password-file")
    source.add_argument("--stdin", action="store_true")
    source.add_argument("--generate", metavar="PATH", help="generate a password into a new 0600 file")
    setpw.add_argument("--username", default="operator")
    grant = sub.add_parser("grant").add_subparsers(dest="action", required=True)
    grant.add_parser("list").add_argument("--json", action="store_true")
    grant.add_parser("revoke").add_argument("grant_id")
    grant.add_parser("revoke-client").add_argument("client_id")
    args = parser.parse_args(argv)
    _require_root()

    if args.area == "client":
        with _locked(args.clients_file):
            data = _load_clients(args.clients_file)
            if args.action == "create":
                if not args.name.strip() or len(args.name) > 64:
                    sys.exit("--name must be 1-64 characters")
                client_id = "hmcpc_" + secrets.token_hex(8)
                secret = secrets.token_urlsafe(32)
                _write_secret_file(args.secret_file, f"client_id={client_id}\nclient_secret={secret}\n")
                data["clients"].append({
                    "client_id": client_id, "name": args.name.strip(), "secret_sha256": sha256_hex(secret),
                    "redirect_prefixes": list(GOOGLE_REDIRECT_PREFIXES), "scopes": ["read"],
                    "created_at": _now_iso(), "revoked_at": None})
                _write_json_atomic(args.clients_file, validate_clients(data))
                print(client_id)
                print(f"secret written to {args.secret_file}", file=sys.stderr)
            elif args.action == "list":
                rows = [{k: c.get(k) for k in ("client_id", "name", "scopes", "created_at", "revoked_at")}
                        for c in data["clients"]]
                if args.json:
                    print(json.dumps(rows, indent=2))
                else:
                    for row in rows:
                        status = "revoked" if row["revoked_at"] else "active"
                        print(f"{row['client_id']}  {status:8}  {row['created_at']}  {row['name']}")
            elif args.action == "revoke":
                for record in data["clients"]:
                    if record["client_id"] == args.client_id:
                        record["revoked_at"] = record.get("revoked_at") or _now_iso()
                        _write_json_atomic(args.clients_file, data)
                        break
                else:
                    sys.exit(f"no client {args.client_id}")
                print(f"revoked client {args.client_id}; run 'grant revoke-client' to drop its grants too",
                      file=sys.stderr)
        return 0

    if args.area == "operator":
        if args.generate:
            password = secrets.token_urlsafe(24)
        elif args.stdin:
            password = sys.stdin.readline().rstrip("\r\n")
        else:
            password = Path(args.password_file).read_text().strip()
        if len(password) < 16:
            sys.exit("operator password must be at least 16 characters")
        with _locked(args.operator_file):
            if args.generate:
                _write_secret_file(args.generate, password + "\n")
            _write_json_atomic(args.operator_file, {"version": FILE_VERSION, "username": args.username,
                                                    "scrypt": hash_password(password), "updated_at": _now_iso()})
        print(f"operator password hash written to {args.operator_file}", file=sys.stderr)
        return 0

    _drop_to_db_owner(args.state_db)
    store = OAuthStore(args.state_db)
    if args.action == "list":
        rows = [dict(zip(("grant_id", "client_id", "scope", "created", "revoked_at", "revoke_reason",
                          "live_refresh_tokens"), row)) for row in store.list_grants()]
        if args.json:
            print(json.dumps(rows, indent=2))
        else:
            for row in rows:
                created = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(row["created"]))
                status = "revoked" if row["revoked_at"] else "active"
                print(f"{row['grant_id']}  {status:8}  {created}  {row['client_id']}  {row['scope']}"
                      f"  {row['revoke_reason'] or ''}")
    elif args.action == "revoke":
        print(f"revoked {store.revoke_grants(grant_id=args.grant_id)} grant(s)")
    else:
        print(f"revoked {store.revoke_grants(client_id=args.client_id)} grant(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
