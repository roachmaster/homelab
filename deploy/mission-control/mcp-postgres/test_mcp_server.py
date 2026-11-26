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
import threading
import unittest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import mcp_auth  # noqa: E402
from mcp_auth import BearerTokenAuth, FailureLimiter, Principal  # noqa: E402
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
