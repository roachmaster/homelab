"""Tests for the Homelab MCP server, token CLI, SQL guard and read-only database role."""

from datetime import UTC, datetime, timedelta
import base64
import http.client
import json
import os
from pathlib import Path
import subprocess
import sys
from tempfile import TemporaryDirectory
import re
import threading
import time
import unittest
from urllib.parse import parse_qsl, urlencode, urlsplit

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import mcp_auth  # noqa: E402
from mcp_auth import BearerTokenAuth, FailureLimiter, Principal  # noqa: E402
import mcp_oauth  # noqa: E402
import mcp_server  # noqa: E402
from sql_guard import GuardError, check_read_only  # noqa: E402

POSTGRES_BIN = Path("/usr/lib/postgresql/16/bin")
SQL = HERE.parent / "sql"
HOST = "mcp.test"


def cli(token_file, *args, check=True):
    env = {**os.environ, "HOMELAB_MCP_TOKENS_ALLOW_NONROOT": "1", "PYTHONDONTWRITEBYTECODE": "1"}
    return subprocess.run([sys.executable, str(HERE / "mcp_tokens.py"), "--file", str(token_file), *args],
                          capture_output=True, text=True, env=env, check=check, timeout=30)


class SqlGuardTest(unittest.TestCase):
    def test_accepts_read_only_statements(self):
        for sql in ("SELECT 1", "select * from t;", "WITH x AS (SELECT 1) SELECT * FROM x",
                    "VALUES (1)", "TABLE forge_agent.coding_tasks", "EXPLAIN SELECT 1", "SHOW server_version",
                    "SELECT 'insert; drop table t' AS s", "SELECT $$;delete$$", "SELECT $q$ update $q$",
                    "SELECT \"update\" FROM t", "-- comment; delete\nSELECT 1", "/* a /* nested; */ */ SELECT 1",
                    "SELECT E'it\\'s; insert'", "SELECT comment, status FROM forge_agent.coding_reviews"):
            with self.subTest(sql=sql):
                self.assertEqual(check_read_only(sql), sql.strip())

    def test_rejects_writes_and_unsafe_forms(self):
        for sql in ("", "   ", "INSERT INTO t VALUES (1)", "DELETE FROM t", "UPDATE t SET a=1",
                    "SELECT 1; SELECT 2", "SELECT 1; DROP TABLE t", "WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d",
                    "SELECT * INTO new_t FROM t", "SELECT * FROM t FOR UPDATE", "SELECT * FROM t FOR NO KEY UPDATE",
                    "SELECT pg_sleep(10)", "SELECT pg_catalog.pg_read_file('/etc/passwd')",
                    "SELECT set_config('x','y',false)", "EXPLAIN ANALYZE SELECT 1", "EXPLAIN (ANALYZE) SELECT 1",
                    "COPY t TO STDOUT", "SET search_path=x", "BEGIN", "CALL p()", "DO $$BEGIN END$$",
                    "SELECT 'unterminated", "SELECT 1 /* open", "SELECT $x$ open", "SELECT nextval('s')",
                    "SELECT 1\x00", "SELECT " + "1," * 10001 + "1"):
            with self.subTest(sql=sql[:60]):
                with self.assertRaises(GuardError):
                    check_read_only(sql)


class TokenCliAndAuthTest(unittest.TestCase):
    def setUp(self):
        directory = TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.file = Path(directory.name) / "tokens.json"

    def create(self, *extra):
        result = cli(self.file, "create", "--name", "test client", *extra)
        token = result.stdout.strip()
        self.assertRegex(token, mcp_auth.TOKEN_PATTERN.pattern)
        self.assertIn("cannot be shown again", result.stderr)
        return token

    def test_create_stores_only_hash_and_authenticates(self):
        token = self.create()
        raw = self.file.read_text()
        self.assertNotIn(token, raw)
        self.assertNotIn(token.split("_")[-1], raw)
        self.assertEqual(self.file.stat().st_mode & 0o777, 0o640)
        auth = BearerTokenAuth(self.file)
        principal = auth.authenticate({"Authorization": f"Bearer {token}"})
        self.assertEqual(principal.name, "test client")
        self.assertEqual(principal.scopes, frozenset({"read"}))
        self.assertEqual(json.loads(cli(self.file, "list", "--json").stdout)[0]["status"], "active")

    def test_rejects_missing_wrong_and_malformed_tokens(self):
        token = self.create()
        auth = BearerTokenAuth(self.file)
        wrong = token[:-4] + ("AAAA" if not token.endswith("AAAA") else "BBBB")
        for headers in ({}, {"Authorization": ""}, {"Authorization": token}, {"Authorization": f"Basic {token}"},
                        {"Authorization": f"Bearer {wrong}"}, {"Authorization": "Bearer hmcp_short"},
                        {"Authorization": "Bearer " + "hmcp_000000000000_" + "A" * 43}):
            with self.subTest(headers=headers):
                self.assertIsNone(auth.authenticate(headers))

    def test_revoke_takes_effect_without_restart(self):
        token = self.create()
        auth = BearerTokenAuth(self.file)
        self.assertIsNotNone(auth.authenticate({"Authorization": f"Bearer {token}"}))
        token_id = token.split("_")[1]
        self.assertIn("Revoked", cli(self.file, "revoke", token_id).stdout)
        self.assertIsNone(auth.authenticate({"Authorization": f"Bearer {token}"}))
        self.assertEqual(json.loads(cli(self.file, "list", "--json").stdout)[0]["status"], "revoked")
        self.assertIn("already revoked", cli(self.file, "revoke", token_id).stdout)
        self.assertNotEqual(cli(self.file, "revoke", "0" * 12, check=False).returncode, 0)

    def test_expiry(self):
        token = self.create("--ttl-days", "1")
        later = BearerTokenAuth(self.file, clock=lambda: datetime.now(UTC) + timedelta(days=2))
        self.assertIsNone(later.authenticate({"Authorization": f"Bearer {token}"}))
        for ttl in ("0", "366", "-1"):
            self.assertNotEqual(cli(self.file, "create", "--name", "x", "--ttl-days", ttl, check=False).returncode, 0)
        self.assertNotEqual(cli(self.file, "create", "--name", "x", "--scope", "write", check=False).returncode, 0)

    def test_unsafe_or_corrupt_file_fails_closed(self):
        token = self.create()
        auth = BearerTokenAuth(self.file)
        os.chmod(self.file, 0o644)
        self.assertIsNone(auth.authenticate({"Authorization": f"Bearer {token}"}))
        self.assertIn("world", auth.error)
        os.chmod(self.file, 0o640)
        self.assertIsNotNone(auth.authenticate({"Authorization": f"Bearer {token}"}))
        self.file.write_text("{not json")
        os.chmod(self.file, 0o640)
        self.assertIsNone(auth.authenticate({"Authorization": f"Bearer {token}"}))
        self.assertNotEqual(cli(self.file, "list", check=False).returncode, 0)

    def test_cli_requires_root(self):
        if os.geteuid() == 0:
            self.skipTest("running as root")
        env = {k: v for k, v in os.environ.items() if k != "HOMELAB_MCP_TOKENS_ALLOW_NONROOT"}
        env["PYTHONDONTWRITEBYTECODE"] = "1"
        result = subprocess.run([sys.executable, str(HERE / "mcp_tokens.py"), "--file", str(self.file), "list"],
                                capture_output=True, text=True, env=env, timeout=30)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("root", result.stderr)


class FailureLimiterTest(unittest.TestCase):
    def test_lockout_and_global_ceiling(self):
        clock = [0.0]
        limiter = FailureLimiter(failures=3, window_seconds=60, lockout_seconds=300,
                                 global_failures_per_minute=5, monotonic=lambda: clock[0])
        for _ in range(3):
            self.assertEqual(limiter.retry_after("a"), 0)
            limiter.record_failure("a")
        self.assertGreater(limiter.retry_after("a"), 0)
        self.assertEqual(limiter.retry_after("b"), 0)
        clock[0] = 301
        self.assertEqual(limiter.retry_after("a"), 0)
        for key in "cdefg":
            limiter.record_failure(key)
        self.assertGreater(limiter.retry_after("z"), 0)
        clock[0] = 400
        self.assertEqual(limiter.retry_after("z"), 0)


class FakeBackend:
    """Serves 250 rows, honouring limit/offset like the real backend."""

    def __init__(self, total=250):
        self.calls = []
        self.total = total

    def run(self, sql, params, limit, offset=0, server_side=False):
        self.calls.append((sql, params, limit, offset, server_side))
        if params and "offset" in params:
            offset = params["offset"]
        rows = [(i, f"row {i}", datetime(2026, 1, 1, tzinfo=UTC)) for i in range(offset, min(self.total, offset + limit + 1))]
        return ["id", "label", "at"], rows[:limit], len(rows) > limit


class HttpServerTest(unittest.TestCase):
    def setUp(self):
        directory = TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.file = Path(directory.name) / "tokens.json"
        self.token = cli(self.file, "create", "--name", "agent").stdout.strip()
        config_path = Path(directory.name) / "server.json"
        config_path.write_text(json.dumps({
            "listen": "127.0.0.1:0", "allowed_hosts": [HOST], "allowed_origins": ["https://allowed.example"],
            "token_file": str(self.file), "max_rows": 120,
            "rate_limit": {"failures": 4, "window_seconds": 60, "lockout_seconds": 300,
                           "global_failures_per_minute": 1000}}))
        config = mcp_server.load_config(config_path)
        self.backend = FakeBackend()
        app = mcp_server.McpApp(config, BearerTokenAuth(self.file), self.backend,
                                FailureLimiter(**config["rate_limit"]))
        self.server = mcp_server.Server(("127.0.0.1", 0), app)
        thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(lambda: (self.server.shutdown(), self.server.server_close()))

    def request_on(self, connection, method="POST", path="/mcp", body=None, token="default", headers=None, raw=None):
        sent = {"Host": HOST, "Content-Type": "application/json", "Accept": "application/json, text/event-stream"}
        if token == "default":
            token = self.token
        if token:
            sent["Authorization"] = f"Bearer {token}"
        sent.update(headers or {})
        payload = raw if raw is not None else (json.dumps(body).encode() if body is not None else None)
        connection.request(method, path, body=payload, headers={k: v for k, v in sent.items() if v is not None})
        response = connection.getresponse()
        data = response.read()
        return response.status, dict(response.getheaders()), (json.loads(data) if data else None)

    def request(self, method="POST", path="/mcp", body=None, token="default", headers=None, raw=None):
        connection = http.client.HTTPConnection("127.0.0.1", self.server.server_address[1], timeout=10)
        try:
            return self.request_on(connection, method, path, body, token, headers, raw)
        finally:
            connection.close()

    def rpc(self, method, params=None, request_id=1, **kwargs):
        message = {"jsonrpc": "2.0", "id": request_id, "method": method}
        if params is not None:
            message["params"] = params
        return self.request(body=message, **kwargs)

    def test_missing_and_invalid_token_get_401_challenge(self):
        status, headers, body = self.rpc("tools/list", token=None)
        self.assertEqual((status, body["error"]), (401, "missing_token"))
        self.assertEqual(headers["WWW-Authenticate"], 'Bearer realm="homelab-mcp"')
        status, headers, body = self.rpc("tools/list", token="hmcp_000000000000_" + "A" * 43)
        self.assertEqual(status, 401)
        self.assertIn('error="invalid_token"', headers["WWW-Authenticate"])
        self.assertEqual(self.backend.calls, [])

    def test_unauthenticated_post_does_not_poison_keepalive(self):
        connection = http.client.HTTPConnection("127.0.0.1", self.server.server_address[1], timeout=10)
        self.addCleanup(connection.close)
        status, headers, body = self.request_on(connection, body={"x": 1}, token=None)
        self.assertEqual((status, body["error"]), (401, "missing_token"))
        self.assertEqual(headers["WWW-Authenticate"], 'Bearer realm="homelab-mcp"')
        self.assertNotEqual((headers.get("Connection") or "").lower(), "close")
        status, _, body = self.request_on(
            connection, body={"jsonrpc": "2.0", "id": 1, "method": "tools/list"})
        self.assertEqual(status, 200)
        self.assertIn("tools", body["result"])

    def test_rejected_paths_do_not_poison_keepalive(self):
        connection = http.client.HTTPConnection("127.0.0.1", self.server.server_address[1], timeout=10)
        self.addCleanup(connection.close)
        status, headers, _ = self.request_on(
            connection, body={"jsonrpc": "2.0", "id": 1, "method": "tools/list"},
            token="hmcp_000000000000_" + "A" * 43)
        self.assertEqual(status, 401)
        self.assertIn('error="invalid_token"', headers["WWW-Authenticate"])
        status, _, body = self.request_on(connection, path="/nope", body={"x": 1})
        self.assertEqual(status, 404)
        status, _, body = self.request_on(
            connection, body={"jsonrpc": "2.0", "id": 2, "method": "initialize",
                              "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                                         "clientInfo": {"name": "t", "version": "1"}}})
        self.assertEqual(status, 200)
        self.assertEqual(body["result"]["serverInfo"]["name"], "homelab-mcp-postgres")

    def test_oversize_body_closes_connection(self):
        status, headers, _ = self.request(raw=b"x" * (mcp_server.MAX_BODY_BYTES + 1))
        self.assertEqual(status, 413)
        self.assertEqual(headers.get("Connection"), "close")

    def test_failed_attempts_are_rate_limited(self):
        for _ in range(4):
            self.assertEqual(self.rpc("ping", token="bad")[0], 401)
        status, headers, _ = self.rpc("ping")
        self.assertEqual(status, 429)
        self.assertIn("Retry-After", headers)
        # Separate Cloudflare client IPs are bucketed separately.
        self.assertEqual(self.rpc("ping", headers={"CF-Connecting-IP": "203.0.113.9"})[0], 200)

    def test_host_and_origin_checks(self):
        self.assertEqual(self.rpc("ping", headers={"Host": "evil.example"})[0], 421)
        self.assertEqual(self.rpc("ping", headers={"Origin": "https://evil.example"})[0], 403)
        self.assertEqual(self.rpc("ping", headers={"Origin": "https://allowed.example"})[0], 200)
        self.assertEqual(self.request("GET", "/healthz", token=None, headers={"Host": "other"})[0], 421)
        self.assertEqual(self.request("GET", "/healthz", token=None)[0], 200)

    def test_initialize_list_and_notifications(self):
        status, headers, body = self.rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                                        "clientInfo": {"name": "t", "version": "1"}})
        self.assertEqual(status, 200)
        self.assertEqual(body["result"]["protocolVersion"], "2025-06-18")
        self.assertIn("tools", body["result"]["capabilities"])
        self.assertEqual(headers["Cache-Control"], "no-store")
        status, _, body = self.rpc("initialize", {"protocolVersion": "1999-01-01"})
        self.assertEqual(body["result"]["protocolVersion"], mcp_server.PROTOCOL_VERSIONS[0])
        status, _, body = self.request(body={"jsonrpc": "2.0", "method": "notifications/initialized"})
        self.assertEqual((status, body), (202, None))
        status, _, body = self.rpc("tools/list", headers={"MCP-Protocol-Version": "2025-06-18"})
        self.assertEqual({tool["name"] for tool in body["result"]["tools"]}, {"query", "list_tables", "describe_table"})
        self.assertEqual(self.rpc("nope")[2]["error"]["code"], -32601)

    def query(self, **arguments):
        status, _, body = self.rpc("tools/call", {"name": "query", "arguments": arguments})
        self.assertEqual(status, 200)
        return body["result"]

    def test_query_tool(self):
        result = self.query(sql="SELECT 1")
        self.assertFalse(result["isError"])
        page = result["structuredContent"]
        self.assertEqual((page["row_count"], page["offset"], page["page_size"], page["has_more"]), (100, 0, 100, True))
        self.assertTrue(page["next_cursor"])
        self.assertEqual(page["rows"][0]["at"], "2026-01-01T00:00:00+00:00")
        self.assertEqual(json.loads(result["content"][0]["text"]), page)
        self.assertEqual(self.backend.calls[-1], ("SELECT 1", None, 100, 0, True))
        self.query(sql="SELECT 1", page_size=7)
        self.assertEqual(self.backend.calls[-1][2], 7)
        self.query(sql="SELECT 1", page_size=9999)
        self.assertEqual(self.backend.calls[-1][2], 120)
        self.assertTrue(self.query(sql="SELECT 1", max_rows=5)["isError"])

    def test_query_pagination_walks_all_rows(self):
        seen, cursor, pages = [], None, 0
        while True:
            arguments = {"sql": "SELECT * FROM t ORDER BY id", "page_size": 60}
            if cursor:
                arguments["cursor"] = cursor
            page = self.query(**arguments)["structuredContent"]
            seen += [row["id"] for row in page["rows"]]
            pages += 1
            cursor = page["next_cursor"]
            if not cursor:
                self.assertFalse(page["has_more"])
                break
        self.assertEqual((seen, pages), (list(range(250)), 5))

    def test_cursor_is_bound_and_tamper_proof(self):
        cursor = self.query(sql="SELECT 1", page_size=10)["structuredContent"]["next_cursor"]
        self.assertFalse(self.query(sql="SELECT 1", page_size=10, cursor=cursor)["isError"])
        self.assertEqual(self.backend.calls[-1][3], 10)
        for arguments in ({"sql": "SELECT 2", "page_size": 10}, {"sql": "SELECT 1", "page_size": 11}):
            with self.subTest(arguments=arguments):
                result = self.query(**arguments, cursor=cursor)
                self.assertTrue(result["isError"])
                self.assertIn("does not match", result["content"][0]["text"])
        payload, mac = cursor.split(".")
        forged = json.loads(base64.urlsafe_b64decode(payload + "=" * (-len(payload) % 4)))
        forged["o"] = 200
        forged_payload = base64.urlsafe_b64encode(json.dumps(forged, separators=(",", ":")).encode()).rstrip(b"=").decode()
        for bad in (f"{forged_payload}.{mac}", "garbage", "a.b", "x" * 600, 5):
            with self.subTest(bad=str(bad)[:20]):
                self.assertTrue(self.query(sql="SELECT 1", page_size=10, cursor=bad)["isError"])
        # Another token cannot replay this token's cursor.
        other = cli(self.file, "create", "--name", "other").stdout.strip()
        _, _, body = self.rpc("tools/call", {"name": "query", "arguments": {
            "sql": "SELECT 1", "page_size": 10, "cursor": cursor}}, token=other)
        self.assertTrue(body["result"]["isError"])

    def test_cursor_expires(self):
        app = self.server.app
        app.config["cursor_ttl_seconds"] = -1
        cursor = self.query(sql="SELECT 1", page_size=10)["structuredContent"]["next_cursor"]
        result = self.query(sql="SELECT 1", page_size=10, cursor=cursor)
        self.assertIn("expired", result["content"][0]["text"])

    def test_explain_and_show_are_single_page(self):
        page = self.query(sql="SHOW server_version", page_size=10)["structuredContent"]
        self.assertEqual(self.backend.calls[-1][4], False)
        self.assertTrue(page["has_more"])
        self.assertIsNone(page["next_cursor"])

    def test_response_size_cut_still_paginates(self):
        self.server.app.config["max_response_bytes"] = 2000
        page = self.query(sql="SELECT 1", page_size=100)["structuredContent"]
        self.assertLess(page["row_count"], 100)
        following = self.query(sql="SELECT 1", page_size=100, cursor=page["next_cursor"])["structuredContent"]
        self.assertEqual(following["offset"], page["row_count"])
        self.assertEqual(following["rows"][0]["id"], page["row_count"])

    def test_list_tables_pagination(self):
        def call(**arguments):
            return self.rpc("tools/call", {"name": "list_tables", "arguments": arguments})[2]["result"]
        first = call(page_size=100)["structuredContent"]
        self.assertEqual(self.backend.calls[-1][1]["limit"], 101)
        second = call(page_size=100, cursor=first["next_cursor"])["structuredContent"]
        self.assertEqual((second["offset"], second["rows"][0]["id"]), (100, 100))
        self.assertTrue(call(page_size=100, schema="x", cursor=first["next_cursor"])["isError"])

    def test_guard_rejection_is_tool_error_without_database_call(self):
        status, _, body = self.rpc("tools/call", {"name": "query", "arguments": {"sql": "DELETE FROM t"}})
        self.assertEqual(status, 200)
        self.assertTrue(body["result"]["isError"])
        self.assertEqual(self.backend.calls, [])
        self.assertTrue(self.rpc("tools/call", {"name": "missing", "arguments": {}})[2]["result"]["isError"])

    def test_scope_is_enforced(self):
        app = self.server.app
        result = app.call_tool({"name": "query", "arguments": {"sql": "SELECT 1"}},
                               Principal("x" * 12, "noscope", frozenset()))
        self.assertTrue(result["isError"])
        self.assertEqual(self.backend.calls, [])

    def test_transport_rules(self):
        self.assertEqual(self.request("GET", "/mcp")[0], 405)
        self.assertEqual(self.request("GET", "/mcp", token=None)[0], 401)
        self.assertEqual(self.request("DELETE", "/mcp")[0], 405)
        self.assertEqual(self.request(path="/other", body={})[0], 404)
        self.assertEqual(self.rpc("ping", headers={"MCP-Protocol-Version": "1999-01-01"})[0], 400)
        self.assertEqual(self.rpc("ping", headers={"Content-Type": "text/plain"})[0], 415)
        self.assertEqual(self.request(raw=b"{bad")[0], 400)
        self.assertEqual(self.request(body=[{"jsonrpc": "2.0", "id": 1, "method": "ping"}])[0], 400)
        self.assertEqual(self.request(raw=b"x" * (mcp_server.MAX_BODY_BYTES + 1))[0], 413)

    def test_revocation_applies_to_running_server(self):
        self.assertEqual(self.rpc("ping")[0], 200)
        cli(self.file, "revoke", self.token.split("_")[1])
        self.assertEqual(self.rpc("ping")[0], 401)



def oauth_cli(directory, *args, check=True, stdin=None):
    env = {**os.environ, "HOMELAB_MCP_TOKENS_ALLOW_NONROOT": "1", "PYTHONDONTWRITEBYTECODE": "1"}
    base = [sys.executable, str(HERE / "mcp_oauth.py"), "--clients-file", str(directory / "clients.json"),
            "--operator-file", str(directory / "operator.json"), "--state-db", str(directory / "oauth.sqlite3")]
    return subprocess.run(base + list(args), capture_output=True, text=True, env=env, check=check, timeout=60,
                          input=stdin)


REDIRECT = "https://oauth-redirect-test.googleusercontent.com/r/test-project"


class RedirectAllowlistTest(unittest.TestCase):
    def test_prefix_matching(self):
        ok = mcp_oauth.redirect_uri_allowed
        for uri in ("https://oauth-redirect.googleusercontent.com/r/p1", "https://oauth-redirect.googleusercontent.com/a/p",
                    "https://oauth-redirect-sandbox.googleusercontent.com/r/x?y=1", REDIRECT,
                    "https://oauth-redirect-test.googleusercontent.com/a/app-123"):
            self.assertTrue(ok(uri), uri)
        for uri in ("http://oauth-redirect.googleusercontent.com/r/p", "https://oauth-redirect.googleusercontent.com/r/",
                    "https://oauth-redirect.googleusercontent.com/x/p", "https://oauth-redirect.googleusercontent.com.evil.com/r/p",
                    "https://evil.com/r/p", "https://oauth-redirect.googleusercontent.com@evil.com/r/p",
                    "https://oauth-redirect.googleusercontent.com:444/r/p", "https://OAUTH-redirect.googleusercontent.com/r/p",
                    "https://oauth-redirect.googleusercontent.com/r/../../evil", "https://oauth-redirect.googleusercontent.com/r/%2e%2e/x",
                    "https://oauth-redirect.googleusercontent.com/r/p#frag", "https://oauth-redirect.googleusercontent.com/r/p q",
                    "https://oauth-redirect.googleusercontent.com/r\\@evil.com", "//evil.com/r/p", "", None,
                    "https://oauth-redirect.googleusercontent.com/rr/p"):
            self.assertFalse(ok(uri), uri)


class OAuthHttpTest(unittest.TestCase):
    def setUp(self):
        directory = TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.dir = Path(directory.name)
        self.file = self.dir / "tokens.json"
        self.static_token = cli(self.file, "create", "--name", "grok").stdout.strip()
        self.client_id = oauth_cli(self.dir, "client", "create", "--name", "gemini",
                                   "--secret-file", str(self.dir / "client.txt")).stdout.strip()
        lines = dict(line.split("=", 1) for line in (self.dir / "client.txt").read_text().split())
        self.assertEqual(lines["client_id"], self.client_id)
        self.secret = lines["client_secret"]
        self.assertEqual((self.dir / "client.txt").stat().st_mode & 0o777, 0o600)
        oauth_cli(self.dir, "operator", "set-password", "--generate", str(self.dir / "pw.txt"))
        self.password = (self.dir / "pw.txt").read_text().strip()
        self.assertNotIn(self.password, (self.dir / "operator.json").read_text())
        self.assertNotIn(self.secret, (self.dir / "clients.json").read_text())
        config_path = self.dir / "server.json"
        config_path.write_text(json.dumps({
            "listen": "127.0.0.1:0", "allowed_hosts": [HOST], "token_file": str(self.file), "max_rows": 120,
            "oauth": {"enabled": True, "issuer": "https://mcp.test", "clients_file": str(self.dir / "clients.json"),
                      "operator_file": str(self.dir / "operator.json"), "state_db": str(self.dir / "oauth.sqlite3"),
                      "login_rate_limit": {"failures": 3, "window_seconds": 60, "lockout_seconds": 300}}}))
        config = mcp_server.load_config(config_path)
        self.backend = FakeBackend()
        self.clock = [time.time()]
        oauth = mcp_oauth.OAuthServer(config["oauth"], clock=lambda: self.clock[0])
        app = mcp_server.McpApp(config, BearerTokenAuth(self.file), self.backend,
                                FailureLimiter(**config["rate_limit"]), oauth)
        self.server = mcp_server.Server(("127.0.0.1", 0), app)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(lambda: (self.server.shutdown(), self.server.server_close()))

    def http(self, method, path, body=None, headers=None, connection=None):
        own = connection is None
        connection = connection or http.client.HTTPConnection("127.0.0.1", self.server.server_address[1], timeout=20)
        sent = {"Host": HOST, **(headers or {})}
        if isinstance(body, dict):
            body = urlencode(body).encode()
            sent.setdefault("Content-Type", "application/x-www-form-urlencoded")
        connection.request(method, path, body=body, headers=sent)
        response = connection.getresponse()
        data = response.read()
        if own:
            connection.close()
        return response.status, {k.lower(): v for k, v in response.getheaders()}, data

    def token_call(self, form, basic=False, path="/oauth/token"):
        headers = {}
        form = dict(form)
        if basic:
            raw = f"{self.client_id}:{self.secret}".encode()
            headers["Authorization"] = "Basic " + base64.b64encode(raw).decode()
        else:
            form.update(client_id=self.client_id, client_secret=self.secret)
        status, headers, data = self.http("POST", path, form, headers)
        return status, headers, (json.loads(data) if data else None)

    def authorize(self, verifier="v" * 50, redirect=REDIRECT, password=None, extra=None):
        params = {"response_type": "code", "client_id": self.client_id, "redirect_uri": redirect,
                  "code_challenge": mcp_oauth.s256(verifier), "code_challenge_method": "S256",
                  "scope": "read", "state": "st-123", **(extra or {})}
        status, headers, page = self.http("GET", "/oauth/authorize?" + urlencode(params))
        self.assertEqual(status, 200, page[:300])
        self.assertIn("form-action", headers["content-security-policy"])
        cookie = headers["set-cookie"].split(";")[0]
        self.assertIn("Secure", headers["set-cookie"])
        csrf = re.search(r'name="csrf" value="([0-9a-f]+)"', page.decode()).group(1)
        form = {**params, "csrf": csrf, "decision": "allow",
                "password": self.password if password is None else password}
        return self.http("POST", "/oauth/authorize", form,
                         {"Cookie": cookie, "Origin": "https://mcp.test"})

    def code_for(self, verifier="v" * 50):
        status, headers, _ = self.authorize(verifier)
        self.assertEqual(status, 302)
        location = headers["location"]
        self.assertTrue(location.startswith(REDIRECT + "?"), location)
        query = dict(parse_qsl(urlsplit(location).query))
        self.assertEqual(query["state"], "st-123")
        return query["code"]

    def exchange(self, code, verifier="v" * 50, redirect=REDIRECT, basic=False):
        return self.token_call({"grant_type": "authorization_code", "code": code, "redirect_uri": redirect,
                                "code_verifier": verifier}, basic=basic)

    def mcp(self, token, method="tools/list", params=None):
        body = {"jsonrpc": "2.0", "id": 1, "method": method}
        if params is not None:
            body["params"] = params
        status, headers, data = self.http("POST", "/mcp", json.dumps(body).encode(), {
            "Content-Type": "application/json", "Authorization": f"Bearer {token}"})
        return status, headers, (json.loads(data) if data else None)

    def test_metadata_documents(self):
        for path in ("/.well-known/oauth-protected-resource", "/.well-known/oauth-protected-resource/mcp"):
            status, _, data = self.http("GET", path)
            body = json.loads(data)
            self.assertEqual(status, 200)
            self.assertEqual((body["resource"], body["authorization_servers"], body["scopes_supported"]),
                             ("https://mcp.test/mcp", ["https://mcp.test"], ["read"]))
        status, _, data = self.http("GET", "/.well-known/oauth-authorization-server")
        body = json.loads(data)
        self.assertEqual(status, 200)
        self.assertEqual(body["issuer"], "https://mcp.test")
        self.assertEqual(body["authorization_endpoint"], "https://mcp.test/oauth/authorize")
        self.assertEqual(body["token_endpoint"], "https://mcp.test/oauth/token")
        self.assertEqual(body["revocation_endpoint"], "https://mcp.test/oauth/revoke")
        self.assertEqual(body["code_challenge_methods_supported"], ["S256"])
        self.assertEqual(body["grant_types_supported"], ["authorization_code", "refresh_token"])
        self.assertEqual(body["token_endpoint_auth_methods_supported"], ["client_secret_post", "client_secret_basic"])
        self.assertNotIn("registration_endpoint", body)

    def test_unauthenticated_mcp_points_to_resource_metadata(self):
        status, headers, _ = self.http("POST", "/mcp", b'{"x":1}', {"Content-Type": "application/json"})
        self.assertEqual(status, 401)
        self.assertEqual(headers["www-authenticate"], 'Bearer realm="homelab-mcp", resource_metadata='
                         '"https://mcp.test/.well-known/oauth-protected-resource"')
        status, headers, _ = self.mcp(mcp_oauth.ACCESS_PREFIX + "A" * 43)
        self.assertEqual(status, 401)
        self.assertTrue(headers["www-authenticate"].endswith(', error="invalid_token"'))

    def test_browser_consent_preserves_origin_and_rejects_untrusted_origins(self):
        params = {"response_type": "code", "client_id": self.client_id, "redirect_uri": REDIRECT,
                  "code_challenge": mcp_oauth.s256("v" * 50), "code_challenge_method": "S256",
                  "scope": "read", "state": "browser-origin"}
        status, page_headers, page = self.http("GET", "/oauth/authorize?" + urlencode(params))
        self.assertEqual(status, 200)
        cookie = page_headers["set-cookie"].split(";")[0]
        csrf = re.search(r'name="csrf" value="([0-9a-f]+)"', page.decode()).group(1)
        # Fetch's append-a-request-origin-header algorithm makes no-referrer
        # navigation POSTs send Origin: null, including same-origin HTML forms.
        origin = "null" if page_headers["referrer-policy"] == "no-referrer" else "https://mcp.test"
        status, headers, body = self.http("POST", "/oauth/authorize",
                                         {**params, "csrf": csrf, "decision": "allow", "password": self.password},
                                         {"Cookie": cookie, "Origin": origin})
        self.assertEqual(status, 302, body[:300])
        self.assertIn("code=", headers["location"])
        self.assertEqual(page_headers["referrer-policy"], "same-origin")
        self.assertEqual(headers["referrer-policy"], "no-referrer")
        for rejected_origin in ("null", "https://evil.example"):
            with self.subTest(origin=rejected_origin):
                status, _, body = self.http("POST", "/oauth/authorize", {}, {"Origin": rejected_origin})
                self.assertEqual(status, 403)
                self.assertEqual(json.loads(body), {"error": "origin_not_allowed"})

    def test_gemini_2025_11_25_handshake_and_read_only_discovery(self):
        status, _, token = self.exchange(self.code_for())
        self.assertEqual(status, 200)
        headers = {"Authorization": "Bearer " + token["access_token"],
                   "Content-Type": "application/json", "Accept": "application/json, text/event-stream",
                   "MCP-Protocol-Version": "2025-11-25"}
        initialize = {"jsonrpc": "2.0", "id": 1, "method": "initialize",
                      "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                                 "clientInfo": {"name": "gemini-proxy", "version": "test"}}}
        status, _, body = self.http("POST", "/mcp", json.dumps(initialize).encode(), headers)
        self.assertEqual(status, 200, body[:300])
        result = json.loads(body)["result"]
        self.assertEqual(result["protocolVersion"], "2025-11-25")
        self.assertIn("tools", result["capabilities"])
        initialized = {"jsonrpc": "2.0", "method": "notifications/initialized"}
        status, _, body = self.http("POST", "/mcp", json.dumps(initialized).encode(), headers)
        self.assertEqual(status, 202)
        self.assertEqual(body, b"")
        listing = {"jsonrpc": "2.0", "id": 2, "method": "tools/list"}
        status, _, body = self.http("POST", "/mcp", json.dumps(listing).encode(), headers)
        self.assertEqual(status, 200)
        tools = json.loads(body)["result"]["tools"]
        self.assertEqual({tool["name"] for tool in tools}, {"query", "list_tables", "describe_table"})
        self.assertTrue(all(tool["annotations"]["readOnlyHint"] for tool in tools))
        status, _, _ = self.http("POST", "/mcp", json.dumps(listing).encode(),
                                 {**headers, "MCP-Protocol-Version": "2099-01-01"})
        self.assertEqual(status, 400)

    def test_pkce_happy_path_dual_auth_and_static_token(self):
        code = self.code_for()
        status, headers, body = self.exchange(code)
        self.assertEqual(status, 200, body)
        self.assertEqual(headers["cache-control"], "no-store")
        self.assertEqual((body["token_type"], body["expires_in"], body["scope"]), ("Bearer", 3600, "read"))
        self.assertTrue(body["access_token"].startswith("hmcpat_"))
        self.assertTrue(body["refresh_token"].startswith("hmcprt_"))
        status, _, result = self.mcp(body["access_token"])
        self.assertEqual(status, 200)
        self.assertEqual(len(result["result"]["tools"]), 3)
        status, _, result = self.mcp(body["access_token"], "tools/call",
                                     {"name": "query", "arguments": {"sql": "SELECT 1"}})
        self.assertFalse(result["result"]["isError"])
        self.assertEqual(self.mcp(self.static_token)[0], 200)
        db = (self.dir / "oauth.sqlite3").read_bytes()
        for secret in (code, body["access_token"], body["refresh_token"], self.secret):
            self.assertNotIn(secret.encode(), db)

    def test_basic_client_auth_and_wrong_secret(self):
        status, _, body = self.exchange(self.code_for(), basic=True)
        self.assertEqual(status, 200, body)
        status, headers, body = self.http("POST", "/oauth/token", {"grant_type": "authorization_code"},
                                          {"Authorization": "Basic " + base64.b64encode(
                                              f"{self.client_id}:wrong".encode()).decode()})
        self.assertEqual(status, 401)
        self.assertEqual(json.loads(body)["error"], "invalid_client")
        self.assertIn("Basic", headers["www-authenticate"])

    def test_bad_verifier_and_code_single_use(self):
        code = self.code_for()
        status, _, body = self.exchange(code, verifier="w" * 50)
        self.assertEqual((status, body["error"]), (400, "invalid_grant"))
        status, _, body = self.exchange(code)
        self.assertEqual((status, body["error"]), (400, "invalid_grant"))

    def test_code_reuse_revokes_issued_tokens(self):
        code = self.code_for()
        status, _, body = self.exchange(code)
        self.assertEqual(status, 200)
        status, _, again = self.exchange(code)
        self.assertEqual((status, again["error"]), (400, "invalid_grant"))
        self.assertEqual(self.mcp(body["access_token"])[0], 401)

    def test_code_expires_and_redirect_must_match(self):
        code = self.code_for()
        status, _, body = self.exchange(code, redirect="https://oauth-redirect-test.googleusercontent.com/r/other")
        self.assertEqual((status, body["error"]), (400, "invalid_grant"))
        code = self.code_for()
        self.clock[0] += 61
        self.assertEqual(self.exchange(code)[2]["error"], "invalid_grant")

    def test_bad_redirect_and_client_never_redirect(self):
        for redirect in ("https://evil.example/r/x", "https://oauth-redirect.googleusercontent.com.evil.com/r/x"):
            params = {"response_type": "code", "client_id": self.client_id, "redirect_uri": redirect,
                      "code_challenge": mcp_oauth.s256("v" * 50), "code_challenge_method": "S256"}
            status, headers, _ = self.http("GET", "/oauth/authorize?" + urlencode(params))
            self.assertEqual(status, 400)
            self.assertNotIn("location", headers)
        params = {"response_type": "code", "client_id": "hmcpc_0000000000000000", "redirect_uri": REDIRECT,
                  "code_challenge": mcp_oauth.s256("v" * 50), "code_challenge_method": "S256"}
        self.assertEqual(self.http("GET", "/oauth/authorize?" + urlencode(params))[0], 400)

    def test_pkce_required_and_s256_only(self):
        base = {"response_type": "code", "client_id": self.client_id, "redirect_uri": REDIRECT, "state": "s"}
        for extra in ({}, {"code_challenge": "v" * 50, "code_challenge_method": "plain"},
                      {"code_challenge": mcp_oauth.s256("v" * 50)},
                      {"code_challenge": mcp_oauth.s256("v" * 50), "code_challenge_method": "S256", "scope": "write"},
                      {"code_challenge": mcp_oauth.s256("v" * 50), "code_challenge_method": "S256",
                       "response_type": "token"}):
            status, headers, _ = self.http("GET", "/oauth/authorize?" + urlencode({**base, **extra}))
            self.assertEqual(status, 302, extra)
            query = dict(parse_qsl(urlsplit(headers["location"]).query))
            self.assertIn(query["error"], ("invalid_request", "invalid_scope", "unsupported_response_type"))
            self.assertEqual(query["state"], "s")
            self.assertNotIn("code", query)

    def test_login_csrf_deny_and_lockout(self):
        status, _, page = self.authorize(password="wrong-password-123")
        self.assertEqual(status, 401)
        self.assertIn(b"Incorrect password", page)
        params = {"response_type": "code", "client_id": self.client_id, "redirect_uri": REDIRECT,
                  "code_challenge": mcp_oauth.s256("v" * 50), "code_challenge_method": "S256", "state": "x"}
        status, _, _ = self.http("POST", "/oauth/authorize", {**params, "csrf": "0" * 64, "decision": "allow",
                                                              "password": self.password},
                                 {"Cookie": "__Host-hmcp_csrf=" + "a" * 43})
        self.assertEqual(status, 403)
        status, _, _ = self.http("POST", "/oauth/authorize", {**params, "password": self.password, "decision": "allow"})
        self.assertEqual(status, 403)
        status, _, _ = self.http("POST", "/oauth/authorize", {**params, "csrf": "x"},
                                 {"Origin": "https://evil.example"})
        self.assertEqual(status, 403)
        status, headers, _ = self.authorize(extra={"state": "x"}, password="")
        status, headers, _ = self.http("GET", "/oauth/authorize?" + urlencode(params))
        cookie = headers["set-cookie"].split(";")[0]
        csrf = re.search(r'name="csrf" value="([0-9a-f]+)"', _.decode()).group(1)
        status, headers, _ = self.http("POST", "/oauth/authorize", {**params, "csrf": csrf, "decision": "deny"},
                                       {"Cookie": cookie})
        self.assertEqual(status, 302)
        self.assertIn("error=access_denied", headers["location"])
        self.authorize(password="wrong-again-12345")
        status, _, _ = self.authorize()
        self.assertEqual(status, 429)

    def test_refresh_rotation_and_reuse_detection(self):
        _, _, first = self.exchange(self.code_for())
        status, _, second = self.token_call({"grant_type": "refresh_token", "refresh_token": first["refresh_token"]})
        self.assertEqual(status, 200, second)
        self.assertNotEqual(second["refresh_token"], first["refresh_token"])
        self.assertEqual(self.mcp(second["access_token"])[0], 200)
        status, _, reused = self.token_call({"grant_type": "refresh_token", "refresh_token": first["refresh_token"]})
        self.assertEqual((status, reused["error"]), (400, "invalid_grant"))
        self.assertEqual(self.mcp(second["access_token"])[0], 401)
        status, _, body = self.token_call({"grant_type": "refresh_token", "refresh_token": second["refresh_token"]})
        self.assertEqual((status, body["error"]), (400, "invalid_grant"))

    def test_access_token_expires(self):
        _, _, body = self.exchange(self.code_for())
        self.clock[0] += 3601
        self.assertEqual(self.mcp(body["access_token"])[0], 401)

    def test_revoke(self):
        _, _, body = self.exchange(self.code_for())
        status, headers, _ = self.token_call({"token": body["access_token"]}, path="/oauth/revoke")
        self.assertEqual(status, 200)
        self.assertEqual(headers["cache-control"], "no-store")
        self.assertEqual(self.mcp(body["access_token"])[0], 401)
        status, _, fresh = self.token_call({"grant_type": "refresh_token", "refresh_token": body["refresh_token"]})
        self.assertEqual(status, 200)
        self.assertEqual(self.token_call({"token": fresh["refresh_token"], "token_type_hint": "refresh_token"},
                                         path="/oauth/revoke")[0], 200)
        self.assertEqual(self.mcp(fresh["access_token"])[0], 401)
        self.assertEqual(self.token_call({"grant_type": "refresh_token",
                                          "refresh_token": fresh["refresh_token"]})[2]["error"], "invalid_grant")
        self.assertEqual(self.token_call({"token": "unknown"}, path="/oauth/revoke")[0], 200)
        status, _, _ = self.http("POST", "/oauth/revoke", {"token": "x", "client_id": self.client_id,
                                                           "client_secret": "nope"})
        self.assertEqual(status, 401)

    def test_cli_grants_and_client_revoke(self):
        _, _, body = self.exchange(self.code_for())
        listing = json.loads(oauth_cli(self.dir, "grant", "list", "--json").stdout)
        self.assertEqual(len(listing), 1)
        oauth_cli(self.dir, "grant", "revoke", listing[0]["grant_id"])
        self.assertEqual(self.mcp(body["access_token"])[0], 401)
        _, _, body = self.exchange(self.code_for())
        oauth_cli(self.dir, "client", "revoke", self.client_id)
        self.assertEqual(self.mcp(body["access_token"])[0], 401)
        self.assertEqual(self.mcp(self.static_token)[0], 200)
        clients = oauth_cli(self.dir, "client", "list").stdout
        self.assertIn("revoked", clients)
        self.assertNotIn(self.secret, clients)

    def test_secret_file_is_never_overwritten_and_not_printed(self):
        result = oauth_cli(self.dir, "client", "create", "--name", "x", "--secret-file",
                           str(self.dir / "client.txt"), check=False)
        self.assertNotEqual(result.returncode, 0)
        result = oauth_cli(self.dir, "client", "create", "--name", "x", "--secret-file", str(self.dir / "c2.txt"))
        secret = (self.dir / "c2.txt").read_text().split("client_secret=")[1].strip()
        self.assertNotIn(secret, result.stdout + result.stderr)

    def test_oauth_disabled_by_default(self):
        self.assertFalse(mcp_server.DEFAULTS["oauth"]["enabled"])


class ConfigTest(unittest.TestCase):
    def test_rejects_public_bind_and_unknown_keys(self):
        with TemporaryDirectory() as directory:
            path = Path(directory) / "c.json"
            for bad in ({"listen": "0.0.0.0:8791"}, {"listen": "192.168.1.5:8791"}, {"bogus": 1},
                        {"allowed_hosts": []}, {"max_rows": 0}):
                path.write_text(json.dumps(bad))
                with self.subTest(bad=bad), self.assertRaises(mcp_server.ConfigError):
                    mcp_server.load_config(path)
            path.write_text((HERE / "server.example.json").read_text())
            self.assertEqual(mcp_server.load_config(path)["listen"], "127.0.0.1:8791")
            tokens = Path(directory) / "tokens.json"
            config = json.loads((HERE / "server.example.json").read_text())
            path.write_text(json.dumps({**config, "token_file": str(tokens)}))
            check = [sys.executable, str(HERE / "mcp_server.py"), "--config", str(path), "--check-config"]
            env = {**os.environ, "PYTHONDONTWRITEBYTECODE": "1"}
            self.assertEqual(subprocess.run(check, capture_output=True, timeout=30, env=env).returncode, 2)
            cli(tokens, "create", "--name", "x")
            result = subprocess.run(check, capture_output=True, text=True, timeout=30, env=env)
            self.assertEqual((result.returncode, result.stdout.strip()), (0, "config ok"))


@unittest.skipUnless(os.geteuid() != 0 and (POSTGRES_BIN / "initdb").is_file(),
                     "Disposable PostgreSQL 16 integration requires its binaries and a non-root user")
class ReadOnlyRoleIntegrationTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        temporary = TemporaryDirectory(prefix="forge-mcp-pg-")
        cls.addClassCleanup(temporary.cleanup)
        cls.root = Path(temporary.name)
        cls.data = cls.root / "data"

        def command(name, *args):
            return subprocess.run([POSTGRES_BIN / name, *map(str, args)], capture_output=True, text=True,
                                  check=True, timeout=60)

        def stop_server():
            if (cls.data / "postmaster.pid").exists():
                command("pg_ctl", "-D", cls.data, "-m", "immediate", "-w", "stop")

        command("initdb", "-D", cls.data, "--username=postgres", "--auth=trust", "--no-locale", "--encoding=UTF8")
        cls.addClassCleanup(stop_server)
        command("pg_ctl", "-D", cls.data, "-l", cls.root / "postgres.log", "-o",
                f"-k {cls.root} -c listen_addresses=", "-w", "start")
        psql = ("-h", cls.root, "-U", "postgres", "-X", "-v", "ON_ERROR_STOP=1")
        command("psql", *psql, "-d", "postgres", "-f", SQL / "provision.sql")
        command("psql", *psql, "-d", "forge_agent", "-c", "SET ROLE forge_agent_owner", "-f", SQL / "apply.sql")
        command("psql", *psql, "-d", "postgres", "-c",
                "CREATE ROLE homelab_mcp LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION "
                "CONNECTION LIMIT 8; ALTER ROLE homelab_mcp SET default_transaction_read_only = on; "
                "ALTER ROLE homelab_mcp SET statement_timeout = '5s'")
        cls.grant_output = command("psql", *psql, "-d", "forge_agent", "-f",
                                   HERE / "sql/grant-mcp-readonly.sql").stdout
        cls.backend = mcp_server.PostgresBackend(f"host={cls.root} dbname=forge_agent user=homelab_mcp", 1500)

    def test_grant_script_verifies(self):
        self.assertIn("homelab_mcp reader verified", self.grant_output)

    def test_verify_detects_write_grant(self):
        psql = [POSTGRES_BIN / "psql", "-h", self.root, "-U", "postgres", "-X", "-v", "ON_ERROR_STOP=1",
                "-d", "forge_agent"]
        run = lambda *args: subprocess.run([*map(str, psql), *args], capture_output=True, text=True, timeout=60)
        self.assertEqual(run("-c", "GRANT INSERT ON forge_agent.coding_tasks TO homelab_mcp").returncode, 0)
        try:
            failed = run("-f", str(HERE / "sql/verify-mcp-readonly.sql"))
            self.assertNotEqual(failed.returncode, 0)
            self.assertIn("write privileges", failed.stderr)
        finally:
            run("-c", "REVOKE INSERT ON forge_agent.coding_tasks FROM homelab_mcp")
        self.assertEqual(run("-f", str(HERE / "sql/verify-mcp-readonly.sql")).returncode, 0)

    def test_reads_granted_tables(self):
        columns, rows, truncated = self.backend.run("SELECT count(*) AS n FROM forge_agent.coding_tasks", None, 10)
        self.assertEqual((columns, rows, truncated), (["n"], [(0,)], False))
        columns, rows, truncated = self.backend.run("SELECT generate_series(1, 20) AS i", None, 5)
        self.assertEqual((len(rows), truncated), (5, True))

    def test_database_blocks_writes_even_without_guard(self):
        with self.assertRaisesRegex(mcp_server.ToolError, "25006|42501"):
            self.backend.run("CREATE TABLE forge_agent.x (a int)", None, 1)
        with self.assertRaisesRegex(mcp_server.ToolError, "25006|42501"):
            self.backend.run("DELETE FROM forge_agent.coding_tasks", None, 1)
        with self.assertRaisesRegex(mcp_server.ToolError, "25006|42501"):
            self.backend.run("CREATE TEMP TABLE t (a int)", None, 1)

    def test_ungranted_tables_are_denied(self):
        with self.assertRaisesRegex(mcp_server.ToolError, "42501"):
            self.backend.run("SELECT * FROM forge_agent.conversations", None, 1)

    def test_statement_timeout(self):
        with self.assertRaisesRegex(mcp_server.ToolError, "57014"):
            self.backend.run("SELECT pg_sleep(5)", None, 1)

    def test_server_side_pagination(self):
        sql = "SELECT i, 'v' || i AS label FROM generate_series(1, 23) AS i ORDER BY i DESC"
        columns, rows, more = self.backend.run(sql, None, 10, 0, True)
        self.assertEqual((columns, [r[0] for r in rows], more), (["i", "label"], list(range(23, 13, -1)), True))
        _, rows, more = self.backend.run(sql, None, 10, 20, True)
        self.assertEqual(([r[0] for r in rows], more), ([3, 2, 1], False))
        _, rows, more = self.backend.run(sql, None, 10, 100, True)
        self.assertEqual((rows, more), ([], False))
        for variant in (sql + ";", sql + " -- trailing; comment", "WITH x AS (" + sql + ") SELECT * FROM x",
                        "VALUES (1), (2), (3)", "TABLE forge_agent.coding_tasks"):
            with self.subTest(variant=variant[:40]):
                self.backend.run(variant, None, 2, 1, True)
        with self.assertRaisesRegex(mcp_server.ToolError, "25006|42501"):
            self.backend.run("SELECT * FROM forge_agent.conversations", None, 2, 1, True)

    def test_paged_tools_end_to_end(self):
        app = mcp_server.McpApp({**mcp_server.DEFAULTS, "max_rows": 1000}, None, self.backend, None)
        principal = Principal("a" * 12, "t", frozenset({"read"}))
        arguments = {"sql": "SELECT g FROM generate_series(1, 7) g ORDER BY g", "page_size": 3}
        seen = []
        while True:
            page = app.call_tool({"name": "query", "arguments": arguments}, principal)["structuredContent"]
            seen += [row["g"] for row in page["rows"]]
            if not page["next_cursor"]:
                break
            arguments = {**arguments, "cursor": page["next_cursor"]}
        self.assertEqual(seen, list(range(1, 8)))
        explain = app.call_tool({"name": "query", "arguments": {"sql": "EXPLAIN SELECT 1"}}, principal)
        self.assertFalse(explain["isError"])
        tables, arguments = [], {"schema": "forge_agent", "page_size": 5}
        while True:
            page = app.call_tool({"name": "list_tables", "arguments": arguments}, principal)["structuredContent"]
            tables += [row["table_name"] for row in page["rows"]]
            if not page["next_cursor"]:
                break
            arguments = {**arguments, "cursor": page["next_cursor"]}
        self.assertEqual(tables, sorted(tables))
        self.assertIn("coding_tasks", tables)
        self.assertEqual(len(tables), len(set(tables)))

    def test_tools_end_to_end(self):
        app = mcp_server.McpApp({**mcp_server.DEFAULTS, "max_rows": 1000}, None, self.backend, None)
        principal = Principal("a" * 12, "t", frozenset({"read"}))
        tables = app.call_tool({"name": "list_tables", "arguments": {"schema": "forge_agent"}}, principal)
        names = {row["table_name"] for row in tables["structuredContent"]["rows"]}
        self.assertIn("coding_tasks", names)
        self.assertNotIn("conversations", names)
        described = app.call_tool({"name": "describe_table",
                                   "arguments": {"schema": "forge_agent", "table": "coding_tasks"}}, principal)
        self.assertFalse(described["isError"])
        self.assertTrue(app.call_tool({"name": "describe_table", "arguments": {
            "schema": "forge_agent", "table": "conversations"}}, principal)["isError"])


if __name__ == "__main__":
    unittest.main()
