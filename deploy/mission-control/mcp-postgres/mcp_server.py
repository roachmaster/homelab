#!/usr/bin/env python3
"""Homelab MCP server: read-only PostgreSQL tools over MCP Streamable HTTP.

Listens on loopback only. Cloudflare Tunnel routes mcp.thedevex.com here and
does no authentication; every MCP request must carry a static bearer token
created with mcp_tokens.py. See README.md.
"""

import argparse
import base64
from datetime import date, datetime, time as dt_time, timedelta
from decimal import Decimal
import hashlib
import hmac
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import ipaddress
import json
import logging
import secrets
from pathlib import Path
import sys
import threading
import time
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))

from mcp_auth import BearerTokenAuth, FailureLimiter  # noqa: E402
from sql_guard import WORD, GuardError, check_read_only  # noqa: E402

SERVER_NAME = "homelab-mcp-postgres"
SERVER_VERSION = "0.1.0"
PROTOCOL_VERSIONS = ("2025-06-18", "2025-03-26", "2024-11-05")
MAX_BODY_BYTES = 64 * 1024
MCP_PATH = "/mcp"
LOG = logging.getLogger("homelab-mcp")

DEFAULTS = {
    "listen": "127.0.0.1:8791",
    "allowed_hosts": ["mcp.thedevex.com"],
    "allowed_origins": [],
    "token_file": "/etc/homelab-forge/mcp/tokens.json",
    "dsn": "host=/var/run/postgresql dbname=forge_agent user=homelab_mcp",
    "max_rows": 500,
    "default_page_size": 100,
    "cursor_ttl_seconds": 3600,
    "max_offset": 1000000,
    "statement_timeout_ms": 5000,
    "max_cell_chars": 2000,
    "max_response_bytes": 1000000,
    "max_concurrent_queries": 4,
    "trust_cf_connecting_ip": True,
    "rate_limit": {"failures": 10, "window_seconds": 60, "lockout_seconds": 300,
                   "global_failures_per_minute": 120},
}

TOOLS = [
    {
        "name": "query",
        "title": "Run a read-only SQL query",
        "description": ("Run one read-only SQL statement (SELECT, WITH, VALUES, TABLE, EXPLAIN or SHOW) "
                        "against the Forge PostgreSQL database. Results are paginated: each call returns up "
                        "to page_size rows and, when more rows exist, a next_cursor. To get the next page, "
                        "call again with the same sql and cursor=next_cursor. Include an ORDER BY on a "
                        "unique key so pages are stable. EXPLAIN and SHOW return a single page. The "
                        "transaction is always rolled back."),
        "inputSchema": {
            "type": "object",
            "properties": {
                "sql": {"type": "string", "description": "A single read-only SQL statement."},
                "page_size": {"type": "integer", "minimum": 1,
                              "description": "Rows per page (default 100, capped at the server maximum)."},
                "cursor": {"type": "string",
                           "description": "next_cursor from the previous page of this same sql."},
            },
            "required": ["sql"],
            "additionalProperties": False,
        },
        "annotations": {"readOnlyHint": True, "destructiveHint": False, "idempotentHint": True,
                        "openWorldHint": False},
    },
    {
        "name": "list_tables",
        "title": "List readable tables",
        "description": ("List tables and views this server's database role can read. Paginated like "
                        "query: pass next_cursor back as cursor with the same schema filter."),
        "inputSchema": {
            "type": "object",
            "properties": {"schema": {"type": "string", "description": "Optional schema filter."},
                           "page_size": {"type": "integer", "minimum": 1},
                           "cursor": {"type": "string"}},
            "additionalProperties": False,
        },
        "annotations": {"readOnlyHint": True, "destructiveHint": False, "idempotentHint": True,
                        "openWorldHint": False},
    },
    {
        "name": "describe_table",
        "title": "Describe a table",
        "description": "List the columns, types and nullability of one readable table or view.",
        "inputSchema": {
            "type": "object",
            "properties": {"schema": {"type": "string"}, "table": {"type": "string"}},
            "required": ["schema", "table"],
            "additionalProperties": False,
        },
        "annotations": {"readOnlyHint": True, "destructiveHint": False, "idempotentHint": True,
                        "openWorldHint": False},
    },
]
TOOL_SCOPES = {"query": "read", "list_tables": "read", "describe_table": "read"}

LIST_TABLES_SQL = """
SELECT table_schema, table_name, table_type FROM information_schema.tables
WHERE table_schema NOT IN ('pg_catalog', 'information_schema')
  AND (%(schema)s::text IS NULL OR table_schema = %(schema)s::text)
ORDER BY table_schema, table_name
LIMIT %(limit)s OFFSET %(offset)s
"""
DESCRIBE_SQL = """
SELECT column_name, data_type, is_nullable, column_default FROM information_schema.columns
WHERE table_schema = %(schema)s AND table_name = %(table)s ORDER BY ordinal_position
"""


PAGEABLE_FIRST = frozenset({"select", "with", "values", "table"})


def strip_for_keyword(sql):
    from sql_guard import strip_literals
    return strip_literals(sql)


class ConfigError(Exception):
    pass


def load_config(path):
    try:
        raw = json.loads(Path(path).read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise ConfigError(f"cannot read config {path}: {error}") from None
    if not isinstance(raw, dict):
        raise ConfigError("config must be a JSON object")
    unknown = set(raw) - set(DEFAULTS)
    if unknown:
        raise ConfigError(f"unknown config keys: {', '.join(sorted(unknown))}")
    config = {**DEFAULTS, **raw, "rate_limit": {**DEFAULTS["rate_limit"], **raw.get("rate_limit", {})}}
    host, _, port = config["listen"].rpartition(":")
    try:
        if not ipaddress.ip_address(host.strip("[]")).is_loopback:
            raise ConfigError("listen address must be loopback; expose it through the tunnel")
    except ValueError:
        raise ConfigError(f"invalid listen address {config['listen']}") from None
    if not port.isdecimal() or not 0 <= int(port) <= 65535:
        raise ConfigError(f"invalid listen port {port}")
    if not config["allowed_hosts"] or not all(isinstance(h, str) and h for h in config["allowed_hosts"]):
        raise ConfigError("allowed_hosts must be a non-empty list of host names")
    for key in ("max_rows", "statement_timeout_ms", "max_cell_chars", "max_response_bytes",
                "max_concurrent_queries", "default_page_size", "cursor_ttl_seconds", "max_offset"):
        if not isinstance(config[key], int) or isinstance(config[key], bool) or config[key] < 1:
            raise ConfigError(f"{key} must be a positive integer")
    config["default_page_size"] = min(config["default_page_size"], config["max_rows"])
    config["allowed_hosts"] = {h.lower() for h in config["allowed_hosts"]}
    config["allowed_origins"] = {o.lower().rstrip("/") for o in config["allowed_origins"]}
    return config


def json_value(value, max_chars):
    if value is None or isinstance(value, (bool, int)):
        return value
    if isinstance(value, float):
        return value if value == value and value not in (float("inf"), float("-inf")) else str(value)
    if isinstance(value, (datetime, date, dt_time)):
        return value.isoformat()
    if isinstance(value, (Decimal, uuid.UUID, timedelta)):
        return str(value)
    if isinstance(value, (bytes, bytearray, memoryview)):
        data = bytes(value)
        text = "\\x" + data[: max_chars // 2].hex()
        return text + ("…" if len(data) > max_chars // 2 else "")
    if isinstance(value, (list, tuple)):
        return [json_value(item, max_chars) for item in value]
    if isinstance(value, dict):
        return {str(k): json_value(v, max_chars) for k, v in value.items()}
    text = str(value)
    return text if len(text) <= max_chars else text[:max_chars] + "…"


class ToolError(Exception):
    """A tool failure reported to the client as an isError result."""


class PostgresBackend:
    """Runs each request in its own READ ONLY transaction that is always rolled back."""

    def __init__(self, dsn, statement_timeout_ms):
        self.dsn = dsn
        self.timeout = statement_timeout_ms

    def run(self, sql, params, limit, offset=0, server_side=False):
        """Return (columns, rows, has_more) for up to ``limit`` rows starting at ``offset``.

        ``server_side`` pages through the unmodified statement with a named
        (DECLARE) cursor: MOVE skips ``offset`` rows, then ``limit + 1`` rows are
        fetched. No SQL text is constructed around the client's statement.
        """
        import psycopg2
        options = (f"-c statement_timeout={self.timeout} -c lock_timeout={min(self.timeout, 2000)} "
                   f"-c idle_in_transaction_session_timeout={self.timeout + 5000} "
                   "-c default_transaction_read_only=on")
        try:
            connection = psycopg2.connect(self.dsn, connect_timeout=5, options=options,
                                          application_name=SERVER_NAME)
        except psycopg2.Error as error:
            LOG.error("database connection failed: %s", str(error).strip().splitlines()[0:1])
            raise ToolError("database is unavailable") from None
        try:
            connection.set_session(readonly=True, autocommit=False)
            if server_side:
                cursor = connection.cursor(name="mcp_page", withhold=False)
                cursor.itersize = limit + 1
            else:
                cursor = connection.cursor()
            with cursor:
                cursor.execute(sql, params)
                if server_side and offset:
                    cursor.scroll(offset, mode="relative")
                elif offset:
                    raise ToolError("this statement does not support pagination")
                rows = cursor.fetchmany(limit + 1)
                if cursor.description is None:
                    return [], [], False
                columns = [column.name for column in cursor.description]
                return columns, rows[:limit], len(rows) > limit
        except psycopg2.Error as error:
            message = (error.pgerror or str(error)).strip().splitlines()
            first = message[0] if message else "query failed"
            raise ToolError(f"{error.pgcode or 'error'}: {first}") from None
        finally:
            try:
                connection.rollback()
            finally:
                connection.close()


class McpApp:
    def __init__(self, config, auth, backend, limiter):
        self.config = config
        self.auth = auth
        self.backend = backend
        self.limiter = limiter
        self.slots = threading.BoundedSemaphore(config["max_concurrent_queries"])
        # Per-process key: cursors are short-lived and intentionally die with a restart.
        self.cursor_key = secrets.token_bytes(32)

    # --- JSON-RPC -------------------------------------------------------
    def handle_rpc(self, message, principal):
        """Return a JSON-RPC response dict, or None for notifications."""
        if not isinstance(message, dict) or message.get("jsonrpc") != "2.0" or not isinstance(message.get("method"), str):
            return error_response(message.get("id") if isinstance(message, dict) else None, -32600, "Invalid Request")
        method, request_id = message["method"], message.get("id")
        is_notification = "id" not in message
        params = message.get("params") or {}
        if not isinstance(params, dict):
            return None if is_notification else error_response(request_id, -32602, "params must be an object")
        if is_notification:
            return None
        if method == "initialize":
            requested = params.get("protocolVersion")
            version = requested if requested in PROTOCOL_VERSIONS else PROTOCOL_VERSIONS[0]
            return result_response(request_id, {
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": False}},
                "serverInfo": {"name": SERVER_NAME, "title": "Homelab PostgreSQL (read-only)",
                               "version": SERVER_VERSION},
                "instructions": ("Read-only access to the Homelab Forge PostgreSQL database. Use list_tables "
                                 "and describe_table to explore, then query with a single SELECT."),
            })
        if method == "ping":
            return result_response(request_id, {})
        if method == "tools/list":
            return result_response(request_id, {"tools": TOOLS})
        if method == "tools/call":
            return result_response(request_id, self.call_tool(params, principal))
        return error_response(request_id, -32601, f"Method not found: {method}")

    def call_tool(self, params, principal):
        name, arguments = params.get("name"), params.get("arguments") or {}
        if name not in TOOL_SCOPES:
            return tool_error(f"unknown tool: {name}")
        if TOOL_SCOPES[name] not in principal.scopes:
            return tool_error("token lacks the required scope")
        if not isinstance(arguments, dict):
            return tool_error("arguments must be an object")
        started, status = time.monotonic(), "error"
        if not self.slots.acquire(timeout=2):
            return tool_error("server is busy; retry shortly")
        try:
            outcome = getattr(self, f"tool_{name}")(arguments, principal)
            status = "ok"
            return outcome
        except (ToolError, GuardError) as error:
            return tool_error(str(error))
        finally:
            self.slots.release()
            LOG.info("tool=%s token=%s status=%s ms=%d", name, principal.token_id, status,
                     (time.monotonic() - started) * 1000)

    # --- pagination -----------------------------------------------------
    def _page_args(self, arguments):
        size = arguments.get("page_size", self.config["default_page_size"])
        if not isinstance(size, int) or isinstance(size, bool) or size < 1:
            raise ToolError("page_size must be a positive integer")
        return min(size, self.config["max_rows"])

    def _binding(self, tool, principal, subject):
        material = json.dumps([tool, principal.token_id, subject], separators=(",", ":"), ensure_ascii=False)
        return hashlib.sha256(material.encode()).hexdigest()[:32]

    def make_cursor(self, binding, offset):
        payload = json.dumps({"v": 1, "b": binding, "o": offset,
                              "e": int(time.time()) + self.config["cursor_ttl_seconds"]},
                             separators=(",", ":")).encode()
        mac = hmac.new(self.cursor_key, payload, hashlib.sha256).digest()
        encode = lambda data: base64.urlsafe_b64encode(data).rstrip(b"=").decode()
        return f"{encode(payload)}.{encode(mac)}"

    def read_cursor(self, cursor, binding):
        """Return the offset carried by a valid cursor for this exact request, else raise."""
        if cursor is None:
            return 0
        if not isinstance(cursor, str) or len(cursor) > 512 or cursor.count(".") != 1:
            raise ToolError("invalid cursor")
        decode = lambda text: base64.urlsafe_b64decode(text + "=" * (-len(text) % 4))
        try:
            payload, mac = (decode(part) for part in cursor.split("."))
        except (ValueError, TypeError):
            raise ToolError("invalid cursor") from None
        if not hmac.compare_digest(mac, hmac.new(self.cursor_key, payload, hashlib.sha256).digest()):
            raise ToolError("invalid or expired cursor (cursors do not survive a server restart)")
        data = json.loads(payload)
        if data.get("b") != binding:
            raise ToolError("cursor does not match this request; resend the same arguments with it")
        if data.get("e", 0) < time.time():
            raise ToolError("cursor expired; start again without a cursor")
        offset = data.get("o")
        if not isinstance(offset, int) or offset < 0 or offset > self.config["max_offset"]:
            raise ToolError("invalid cursor")
        return offset

    def _rows_result(self, columns, rows, has_more, offset=0, page_size=None, binding=None):
        limit_chars = self.config["max_cell_chars"]
        data = [dict(zip(columns, (json_value(v, limit_chars) for v in row))) for row in rows]
        size_cut = False

        def build():
            more = has_more or size_cut
            following = offset + len(data)
            cursor = (self.make_cursor(binding, following)
                      if more and binding and following <= self.config["max_offset"] else None)
            return {"columns": columns, "rows": data, "row_count": len(data), "offset": offset,
                    "page_size": page_size if page_size is not None else len(data),
                    "has_more": more, "next_cursor": cursor, "truncated": more}

        payload = build()
        text = json.dumps(payload, ensure_ascii=False)
        while len(text.encode()) > self.config["max_response_bytes"] and len(data) > 1:
            data = data[: len(data) // 2]
            size_cut = True
            payload = build()
            text = json.dumps(payload, ensure_ascii=False)
        return {"content": [{"type": "text", "text": text}], "structuredContent": payload, "isError": False}

    def tool_query(self, arguments, principal):
        if set(arguments) - {"sql", "page_size", "cursor"}:
            raise ToolError("unexpected arguments (use sql, page_size, cursor)")
        sql = check_read_only(arguments.get("sql"))
        page_size = self._page_args(arguments)
        first = WORD.search(strip_for_keyword(sql)).group(0).lower()
        pageable = first in PAGEABLE_FIRST
        binding = self._binding("query", principal, [sql, page_size]) if pageable else None
        offset = self.read_cursor(arguments.get("cursor"), binding) if pageable else (
            self.read_cursor(arguments.get("cursor"), "unpageable") if arguments.get("cursor") else 0)
        columns, rows, has_more = self.backend.run(sql, None, page_size, offset, pageable)
        return self._rows_result(columns, rows, has_more, offset, page_size, binding)

    def tool_list_tables(self, arguments, principal):
        schema = arguments.get("schema")
        if set(arguments) - {"schema", "page_size", "cursor"} or (schema is not None and not isinstance(schema, str)):
            raise ToolError("arguments are schema (string), page_size and cursor")
        page_size = self._page_args(arguments)
        binding = self._binding("list_tables", principal, [schema, page_size])
        offset = self.read_cursor(arguments.get("cursor"), binding)
        params = {"schema": schema, "limit": page_size + 1, "offset": offset}
        columns, rows, _ = self.backend.run(LIST_TABLES_SQL, params, page_size + 1)
        return self._rows_result(columns, rows[:page_size], len(rows) > page_size, offset, page_size, binding)

    def tool_describe_table(self, arguments, principal):
        schema, table = arguments.get("schema"), arguments.get("table")
        if set(arguments) - {"schema", "table"} or not isinstance(schema, str) or not isinstance(table, str):
            raise ToolError("schema and table must be strings")
        result = self._rows_result(*self.backend.run(DESCRIBE_SQL, {"schema": schema, "table": table},
                                                     self.config["max_rows"]))
        if not result["structuredContent"]["rows"]:
            raise ToolError(f"no readable table {schema}.{table}")
        return result


def result_response(request_id, result):
    return {"jsonrpc": "2.0", "id": request_id, "result": result}


def error_response(request_id, code, message):
    return {"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": message}}


def tool_error(message):
    return {"content": [{"type": "text", "text": message}], "isError": True}


class Handler(BaseHTTPRequestHandler):
    server_version = "homelab-mcp"
    sys_version = ""
    protocol_version = "HTTP/1.1"
    timeout = 30

    @property
    def app(self):
        return self.server.app

    def log_message(self, fmt, *args):
        LOG.debug("%s %s", self.address_string(), fmt % args)

    def handle_one_request(self):
        # Handler instances are reused for keep-alive connections.
        self.__dict__.pop("_body_result", None)
        super().handle_one_request()

    def consume_request_body(self):
        """Drain Content-Length bytes so a reused keep-alive connection stays aligned.

        Returns (body, reason). body is bytes (possibly empty) when the request
        can stay open. reason is None on success, or 'too_large', 'missing', or
        'unreadable' when the body cannot be consumed; close_connection is set
        in those cases and the caller must send Connection: close.
        """
        if hasattr(self, "_body_result"):
            return self._body_result
        result = self._read_request_body()
        self._body_result = result
        if result[0] is None:
            self.close_connection = True
        return result

    def _read_request_body(self):
        transfer = (self.headers.get("Transfer-Encoding") or "").strip().lower()
        if transfer and transfer != "identity":
            return None, "missing"
        raw_length = self.headers.get("Content-Length")
        if raw_length is None:
            return b"", None
        try:
            length = int(raw_length)
        except ValueError:
            return None, "missing"
        if length < 0 or length > MAX_BODY_BYTES:
            return None, "too_large"
        try:
            data = self.rfile.read(length)
        except (OSError, ValueError):
            return None, "unreadable"
        if len(data) < length:
            return None, "unreadable"
        return data, None

    def send_json(self, status, body, extra=None):
        self.consume_request_body()
        data = b"" if body is None else json.dumps(body, ensure_ascii=False).encode()
        extra = dict(extra or {})
        if any(key.lower() == "connection" and str(value).lower() == "close" for key, value in extra.items()):
            self.close_connection = True
        if self.close_connection:
            extra.setdefault("Connection", "close")
        self.send_response(status)
        if body is not None:
            self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        for key, value in extra.items():
            self.send_header(key, value)
        self.end_headers()
        if data and self.command != "HEAD":
            self.wfile.write(data)

    def client_key(self):
        peer = self.client_address[0]
        forwarded = self.headers.get("CF-Connecting-IP")
        if self.app.config["trust_cf_connecting_ip"] and forwarded and ipaddress.ip_address(peer).is_loopback:
            try:
                return str(ipaddress.ip_address(forwarded.strip()))
            except ValueError:
                return peer
        return peer

    def admit(self):
        """Host/Origin checks shared by every route. Returns False after responding."""
        host = (self.headers.get("Host") or "").lower()
        if host not in self.app.config["allowed_hosts"]:
            self.send_json(421 if host else 400, {"error": "invalid_host"}, {"Connection": "close"})
            return False
        origin = self.headers.get("Origin")
        if origin is not None and origin.lower().rstrip("/") not in self.app.config["allowed_origins"]:
            self.send_json(403, {"error": "origin_not_allowed"}, {"Connection": "close"})
            return False
        return True

    def authenticate(self):
        key = self.client_key()
        wait = self.app.limiter.retry_after(key)
        if wait:
            self.send_json(429, {"error": "too_many_failed_attempts"},
                           {"Retry-After": str(wait), "Connection": "close"})
            return None
        principal = self.app.auth.authenticate(self.headers)
        if principal is None:
            self.app.limiter.record_failure(key)
            presented = bool(self.headers.get("Authorization"))
            challenge = 'Bearer realm="homelab-mcp"' + (', error="invalid_token"' if presented else "")
            LOG.warning("auth failure key=%s presented=%s", key, presented)
            self.send_json(401, {"error": "invalid_token" if presented else "missing_token"},
                           {"WWW-Authenticate": challenge})
            return None
        self.app.limiter.record_success(key)
        return principal

    def do_GET(self):
        if not self.admit():
            return
        if self.path == "/healthz":
            self.send_json(200, {"status": "ok"})
        elif self.path.split("?")[0] == MCP_PATH:
            # No server-initiated stream is offered; the spec permits 405 here.
            if self.authenticate() is not None:
                self.send_json(405, {"error": "method_not_allowed"}, {"Allow": "POST"})
        else:
            self.send_json(404, {"error": "not_found"})

    def do_DELETE(self):
        if self.admit() and self.authenticate() is not None:
            self.send_json(405, {"error": "method_not_allowed"}, {"Allow": "POST"})

    def do_POST(self):
        if not self.admit():
            return
        if self.path.split("?")[0] != MCP_PATH:
            self.send_json(404, {"error": "not_found"})
            return
        principal = self.authenticate()
        if principal is None:
            return
        version = self.headers.get("MCP-Protocol-Version")
        if version is not None and version not in PROTOCOL_VERSIONS:
            self.send_json(400, error_response(None, -32600, f"Unsupported MCP-Protocol-Version {version}"))
            return
        content_type = (self.headers.get("Content-Type") or "").split(";")[0].strip().lower()
        if content_type != "application/json":
            self.send_json(415, error_response(None, -32700, "Content-Type must be application/json"))
            return
        body, reason = self.consume_request_body()
        if body is None:
            if reason == "too_large":
                self.send_json(413, error_response(None, -32700, "request too large"), {"Connection": "close"})
            else:
                self.send_json(411, error_response(None, -32700, "Content-Length required"), {"Connection": "close"})
            return
        try:
            message = json.loads(body)
        except (json.JSONDecodeError, UnicodeDecodeError):
            self.send_json(400, error_response(None, -32700, "Parse error"))
            return
        if isinstance(message, list):
            self.send_json(400, error_response(None, -32600, "JSON-RPC batches are not supported"))
            return
        if isinstance(message, dict) and "method" not in message and ("result" in message or "error" in message):
            self.send_json(202, None)
            return
        response = self.app.handle_rpc(message, principal)
        if response is None:
            self.send_json(202, None)
        else:
            self.send_json(200, response)


class Server(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True
    request_queue_size = 32

    def __init__(self, address, app):
        self.app = app
        super().__init__(address, Handler)


def build_app(config):
    auth = BearerTokenAuth(config["token_file"])
    limiter = FailureLimiter(**config["rate_limit"])
    return McpApp(config, auth, PostgresBackend(config["dsn"], config["statement_timeout_ms"]), limiter)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--config", required=True)
    parser.add_argument("--check-config", action="store_true", help="validate config and token file, then exit")
    args = parser.parse_args(argv)
    logging.basicConfig(level=logging.INFO, format="%(levelname)s %(message)s", stream=sys.stderr)
    try:
        config = load_config(args.config)
    except ConfigError as error:
        LOG.error("%s", error)
        return 2
    app = build_app(config)
    if app.auth.error:
        LOG.error("token file problem: %s", app.auth.error)
        if args.check_config:
            return 2
    if args.check_config:
        print("config ok")
        return 0
    host, _, port = config["listen"].rpartition(":")
    server = Server((host.strip("[]"), int(port)), app)
    LOG.info("listening on %s:%d", *server.server_address[:2])
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
