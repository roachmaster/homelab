"""Authenticated presence uses real calls and local SQLite, never SQL grants."""
import json
from pathlib import Path
import sqlite3
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import Mock

from mcp_auth import Principal
import mcp_oauth
import mcp_presence as presence
import mcp_server
import test_mcp_server as fixtures

SUBJECT = "static:0123456789ab"
OAUTH = "oauth:hmcpc_0123456789abcdef"
READER = "reader_" + "R" * 43


def configured(root, subjects=None, enabled=True):
    return {"enabled": enabled, "store_path": str(root / "presence.sqlite3"),
            "reader_token_file": str(root / "reader.token"),
            "subjects": subjects if subjects is not None else {
                SUBJECT: {"agent_id": "dot", "label": "Dot"},
                OAUTH: {"agent_id": "gemini", "label": "Gemini"}}}


def reader_file(root):
    path = root / "reader.token"
    path.write_text(READER + "\n")
    path.chmod(0o600)
    return path


class PresenceStoreTest(unittest.TestCase):
    def setUp(self):
        temporary = TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        reader_file(self.root)
        self.clock = [1770000000.0]
        self.config = configured(self.root)
        self.factory = presence.PresenceProviderFactory(self.config, clock=lambda: self.clock[0])

    def principal(self, subject=SUBJECT):
        return Principal("grant-or-token-id", "caller label is not identity", frozenset({"read"}), subject)

    def test_real_calls_derive_roster_identity_and_restart_does_not_refresh_time(self):
        provider = self.factory.create()
        self.assertEqual(provider.snapshot(), {"version": 1, "agents": [
            {"agent_id": "dot", "label": "Dot", "state": "never", "last_seen_at": None},
            {"agent_id": "gemini", "label": "Gemini", "state": "never", "last_seen_at": None}]})
        result = provider.heartbeat(self.principal())
        self.assertEqual(result["agent_id"], "dot")
        self.assertEqual(result["label"], "Dot")
        self.assertEqual((result["heartbeat_interval_seconds"], result["freshness_seconds"]), (60, 180))
        self.assertEqual(provider.heartbeat(self.principal())["last_seen_at"], result["last_seen_at"])
        self.clock[0] += 180
        self.assertEqual(provider.snapshot()["agents"][0]["state"], "online")
        self.clock[0] += 1
        restored = self.factory.create()
        row = restored.snapshot()["agents"][0]
        self.assertEqual((row["state"], row["last_seen_at"]), ("stale", result["last_seen_at"]))
        self.assertEqual(restored.snapshot()["agents"][1]["state"], "never")
        with sqlite3.connect(self.config["store_path"]) as db:
            self.assertEqual(db.execute("SELECT count(*) FROM agent_presence").fetchone()[0], 1)
            self.assertEqual([r[1] for r in db.execute("PRAGMA table_info(agent_presence)")], ["agent_id", "last_seen"])
        self.assertEqual(Path(self.config["store_path"]).stat().st_mode & 0o777, 0o600)

    def test_different_credentials_for_one_agent_coalesce_and_rotation_prunes_old_agent_rows(self):
        other = "static:aaaaaaaaaaaa"
        config = configured(self.root, {SUBJECT: {"agent_id": "grok", "label": "Grok"},
                                       other: {"agent_id": "grok", "label": "Grok"}})
        provider = presence.PresenceProviderFactory(config, clock=lambda: self.clock[0]).create()
        provider.heartbeat(self.principal())
        provider.heartbeat(self.principal(other))
        self.assertEqual(len(provider.snapshot()["agents"]), 1)
        replacement = presence.PresenceProviderFactory(self.config, clock=lambda: self.clock[0]).create()
        self.assertTrue(all(row["state"] == "never" for row in replacement.snapshot()["agents"]))
        self.assertEqual(replacement.store.last_seen(), {})

    def test_unmapped_unscoped_and_legacy_principals_cannot_write_presence(self):
        provider = self.factory.create()
        for principal in (None, self.principal("static:aaaaaaaaaaaa"),
                          Principal("0123456789ab", "dot", frozenset({"read"})),
                          Principal("id", "Dot", frozenset(), SUBJECT)):
            with self.subTest(principal=principal), self.assertRaises(presence.PresenceError):
                provider.heartbeat(principal)
        self.assertEqual(provider.store.last_seen(), {})

    def test_configuration_is_effect_free_bounded_and_disabled_by_default(self):
        store, reader = Mock(), Mock()
        factory = presence.PresenceProviderFactory({}, store_factory=store, reader_factory=reader)
        self.assertIsNone(factory.create())
        store.assert_not_called()
        reader.assert_not_called()
        invalid = [
            {"subjects": {"oauth:grant-derived-id": {"agent_id": "dot", "label": "Dot"}}},
            {"subjects": {SUBJECT: {"agent_id": "dot", "label": "Dot", "role": "worker"}}},
            {"subjects": {SUBJECT: {"agent_id": "dot", "label": "line\nbreak"}}},
            {"subjects": {SUBJECT: {"agent_id": "../dot", "label": "Dot"}}},
            {"subjects": {f"static:{i:012x}": {"agent_id": f"a{i}", "label": "A"} for i in range(17)}},
            {"subjects": {SUBJECT: {"agent_id": "dot", "label": "Dot"}, OAUTH: {"agent_id": "dot", "label": "Other"}}},
            {"enabled": "true"}, {"store_path": "relative.db"}, {"unknown": True}]
        for config in invalid:
            with self.subTest(config=config), self.assertRaises(presence.PresenceError):
                presence.PresenceProviderFactory(config, store_factory=store, reader_factory=reader)
        store.assert_not_called()
        reader.assert_not_called()

    def test_reader_secret_and_store_paths_fail_closed_without_touching_other_data(self):
        original = self.root / "original.json"
        original.write_text('{"protected":true}')
        link = self.root / "link"
        link.symlink_to(original)
        for constructor in (lambda: presence.ReaderTokenAuth(link),
                            lambda: presence.SqlitePresenceStore(link, {"dot"})):
            with self.assertRaises(presence.PresenceError):
                constructor()
        self.assertEqual(original.read_text(), '{"protected":true}')
        reader_file(self.root).chmod(0o644)
        with self.assertRaises(presence.PresenceError):
            self.factory.create()
        self.assertFalse(Path(self.config["store_path"]).exists())

    def test_full_roster_stays_bounded_and_missing_store_fails_closed(self):
        subjects = {f"static:{i:012x}": {"agent_id": f"agent_{i}", "label": f"Agent {i}"}
                    for i in range(16)}
        provider = presence.PresenceProviderFactory(configured(self.root, subjects)).create()
        for subject in subjects:
            provider.heartbeat(self.principal(subject))
        self.assertEqual(len(provider.snapshot()["agents"]), 16)
        self.assertEqual(len(provider.store.last_seen()), 16)
        with self.assertRaises(presence.PresenceError):
            provider.store.heartbeat("spoofed", 1770000000)
        self.assertEqual(len(provider.store.last_seen()), 16)
        Path(self.config["store_path"]).unlink()
        with self.assertRaises(presence.PresenceError):
            provider.snapshot()
        self.assertFalse(Path(self.config["store_path"]).exists())

    def test_composition_injects_selected_provider_and_rejects_auth_file_collision(self):
        config_file = self.root / "server.json"
        config_file.write_text(json.dumps({"presence": self.config}))
        config = mcp_server.load_config(config_file)
        selected = Mock()
        factory = Mock(return_value=Mock(create=Mock(return_value=selected)))
        app = mcp_server.build_app(config, presence_factory=factory)
        self.assertIs(app.presence, selected)
        factory.assert_called_once_with(config["presence"])
        bad = {**self.config, "store_path": mcp_oauth.DEFAULT_STATE_DB}
        config_file.write_text(json.dumps({"presence": bad}))
        with self.assertRaises(mcp_server.ConfigError):
            mcp_server.load_config(config_file)


class PresenceHttpTest(unittest.TestCase):
    def setUp(self):
        fixture = fixtures.HttpServerTest("test_missing_and_invalid_token_get_401_challenge")
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        self.http = fixture
        self.root = fixture.file.parent
        reader_file(self.root)
        self.subject = "static:" + fixture.token.split("_")[1]
        self.config = configured(self.root, {self.subject: {"agent_id": "dot", "label": "Dot"},
                                            OAUTH: {"agent_id": "gemini", "label": "Gemini"}})
        self.clock = [1770000000.0]
        self.provider = presence.PresenceProviderFactory(self.config, clock=lambda: self.clock[0]).create()
        fixture.server.app.presence = self.provider

    def beat(self, arguments=None, **kwargs):
        return self.http.rpc("tools/call", {"name": "heartbeat", "arguments": {} if arguments is None else arguments}, **kwargs)

    def read(self, token=READER, headers=None, path=mcp_server.PRESENCE_PATH):
        local = {"Host": "127.0.0.1:" + str(self.http.server.server_address[1]), **(headers or {})}
        return self.http.request("GET", path, token=token, headers=local)

    def test_authenticated_heartbeat_is_the_only_write_and_does_not_advertise_execution_readiness(self):
        status, _, body = self.beat()
        self.assertEqual(status, 200)
        result = body["result"]
        self.assertFalse(result["isError"])
        self.assertEqual(result["structuredContent"]["agent_id"], "dot")
        self.assertEqual(self.http.backend.calls, [])
        status, headers, body = self.read()
        self.assertEqual((status, body["version"]), (200, 1))
        self.assertEqual(body["agents"][0]["state"], "online")
        self.assertEqual(body["agents"][1]["state"], "never")
        self.assertIn("no-store", headers["Cache-Control"])
        self.assertEqual(set(body), {"version", "agents"})
        self.assertEqual(set(body["agents"][0]), {"agent_id", "label", "state", "last_seen_at"})
        encoded = json.dumps(body)
        for secret in (READER, self.http.token, self.subject, OAUTH, "execution_enabled", "modes"):
            self.assertNotIn(secret, encoded)
        self.clock[0] += 181
        self.assertEqual(self.read()[2]["agents"][0]["state"], "stale")

    def test_missing_invalid_unmapped_or_spoofed_identity_never_records_a_heartbeat(self):
        self.assertEqual(self.beat(token=None)[0], 401)
        unmapped = fixtures.cli(self.http.file, "create", "--name", "Dot").stdout.strip()
        self.assertTrue(self.beat(token=unmapped)[2]["result"]["isError"])
        for arguments in ({"agent_id": "gemini"}, {"label": "Gemini"}, {"last_seen_at": "2099-01-01T00:00:00Z"}, {"subject": OAUTH}, []):
            with self.subTest(arguments=arguments):
                self.assertTrue(self.beat(arguments)[2]["result"]["isError"])
        outer = self.http.rpc("tools/call", {"name": "heartbeat", "arguments": {}, "agent_id": "gemini"})
        self.assertTrue(outer[2]["result"]["isError"])
        self.assertEqual(self.provider.store.last_seen(), {})

    def test_annotations_keep_old_sql_tools_read_only_and_query_writes_stay_blocked(self):
        tools = self.http.rpc("tools/list")[2]["result"]["tools"]
        self.assertEqual({tool["name"] for tool in tools}, {"query", "list_tables", "describe_table", "heartbeat"})
        for tool in tools:
            self.assertEqual(tool["annotations"]["readOnlyHint"], tool["name"] != "heartbeat")
            self.assertFalse(tool["annotations"]["destructiveHint"])
        self.assertTrue(self.http.rpc("tools/call", {"name": "query", "arguments": {"sql": "UPDATE t SET x=1"}})[2]["result"]["isError"])
        self.assertEqual(self.http.backend.calls, [])
        self.assertEqual(self.provider.store.last_seen(), {})

    def test_reader_requires_dedicated_token_local_host_and_no_tunnel_or_browser_headers(self):
        self.assertEqual(self.read(token=None)[0], 401)
        self.assertEqual(self.read(token=self.http.token)[0], 401)
        for headers in ({"Host": fixtures.HOST}, {"CF-Connecting-IP": "203.0.113.2"},
                        {"CF-Connecting-IP": ""}, {"Origin": "https://portal.example"},
                        {"Host": "localhost.evil.example"}):
            with self.subTest(headers=headers):
                self.assertEqual(self.read(headers=headers)[0], 403)
        self.assertEqual(self.read()[0], 200)
        self.assertEqual(self.read(path=mcp_server.PRESENCE_PATH + "?agent_id=dot")[0], 400)
        # The reader credential is not an MCP bearer and cannot record beats.
        self.assertEqual(self.beat(token=READER)[0], 401)
        self.assertEqual(self.provider.store.last_seen(), {})

    def test_disabled_presence_stays_unavailable_and_does_not_change_existing_three_tools(self):
        self.http.server.app.presence = None
        self.assertEqual(self.read()[0], 503)
        self.assertTrue(self.beat()[2]["result"]["isError"])
        self.assertEqual({t["name"] for t in self.http.rpc("tools/list")[2]["result"]["tools"]},
                         {"query", "list_tables", "describe_table"})

    def test_reader_failures_obey_existing_failure_limiter(self):
        for _ in range(4):
            self.assertEqual(self.read(token="wrong")[0], 401)
        self.assertEqual(self.read()[0], 429)
        self.assertEqual(self.provider.store.last_seen(), {})


class OAuthPresenceTest(unittest.TestCase):
    def test_separate_oauth_grants_share_authenticated_client_subject_not_grant_identity(self):
        fixture = fixtures.OAuthHttpTest("test_metadata_documents")
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        reader_file(fixture.dir)
        config = configured(fixture.dir, {"oauth:" + fixture.client_id: {"agent_id": "gemini", "label": "Gemini"}})
        provider = presence.PresenceProviderFactory(config).create()
        fixture.server.app.presence = provider
        tokens = []
        for _ in range(2):
            code = fixture.code_for()
            status, _, body = fixture.exchange(code)
            self.assertEqual(status, 200)
            tokens.append(body["access_token"])
        auth = fixture.server.app.oauth
        principals = [auth.authenticate_bearer(token) for token in tokens]
        self.assertNotEqual(principals[0].token_id, principals[1].token_id)
        self.assertEqual({p.subject for p in principals}, {"oauth:" + fixture.client_id})
        for token in tokens:
            status, _, response = fixture.mcp(token, "tools/call", {"name": "heartbeat", "arguments": {}})
            self.assertEqual(status, 200)
            self.assertFalse(response["result"]["isError"])
            self.assertEqual(response["result"]["structuredContent"]["agent_id"], "gemini")
        self.assertEqual(len(provider.store.last_seen()), 1)


if __name__ == "__main__":
    unittest.main()
