"""Fake (no-database) tests for the proxy DAO client and statement translator.

Covers proxy_client.ProxyClient/ProxyConnection/ProxyCursor against a
stubbed HTTP transport, and proxy_statements.translate over the
statement shapes the runtime callers actually execute. Real-Postgres
end-to-end coverage lives in test_forge_pg_proxy_db.py.
"""

import json
import os
import sys
import unittest
from datetime import datetime, timedelta, timezone
from unittest.mock import patch

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import proxy_client
import proxy_statements as statements
from proxy_client import ProxyClient, ProxyConnection, ProxyError
from proxy_statements import Operation, StaticResult, Subscribe, translate


class FakeTransport:
    """A stub HTTP transport: records requests, returns canned results."""

    def __init__(self):
        self.requests = []
        self.results = {}
        self.notifications = {}

    def operation(self, operation, args=None):
        self.requests.append((operation, args or {}))
        if operation in self.results:
            return self.results[operation]
        raise ProxyError(f"unexpected operation {operation}",
                         status=400, code="unknown_operation")

    def notifications(self, channel, timeout_seconds=25):
        return list(self.notifications.get(channel, []))

    def health(self):
        return {"status": "ok"}


class ClientTest(unittest.TestCase):
    def setUp(self):
        self.transport = FakeTransport()
        self.client = ProxyClient("http://127.0.0.1:18087", "coordinator",
                                  token="test-token")
        self.client._request = lambda method, path, body=None: \
            self._fake_request(method, path, body)

    def _fake_request(self, method, path, body=None):
        if method == "GET" and path == "/health":
            return {"status": "ok"}
        if method == "POST" and path == "/v1/operation":
            result = self.transport.operation(body["operation"], body["args"])
            return {"status": "ok", "result": result}
        if method == "POST" and path == "/v1/notifications":
            return {"notifications": self.transport.notifications(
                body["channel"], body["timeout_seconds"])}
        raise AssertionError(f"unexpected request {method} {path}")

    def test_operation_round_trip(self):
        self.transport.results["fn.forge_agent_worker_api.claim"] = \
            {"task_id": "00000000-0000-0000-0000-000000000001"}
        result = self.client.operation(
            "fn.forge_agent_worker_api.claim", {"p_worker": "w"})
        self.assertEqual(result["task_id"],
                         "00000000-0000-0000-0000-000000000001")

    def test_operation_error_surfaces_as_proxy_error(self):
        with self.assertRaises(ProxyError) as caught:
            self.client.operation("fn.nope.nope", {})
        self.assertEqual(caught.exception.code, "unknown_operation")

    def test_requires_token(self):
        with self.assertRaises(ValueError):
            ProxyClient("http://127.0.0.1:18087", "coordinator")


class CursorTest(unittest.TestCase):
    def setUp(self):
        self.transport = FakeTransport()
        self.connection = ProxyConnection(ProxyClient(
            "http://127.0.0.1:18087", "coordinator", token="test-token"))
        self.connection.client._request = lambda method, path, body=None: \
            self._fake_request(method, path, body)

    def _fake_request(self, method, path, body=None):
        if method == "POST" and path == "/v1/operation":
            result = self.transport.operation(body["operation"], body["args"])
            return {"status": "ok", "result": result}
        if method == "POST" and path == "/v1/notifications":
            return {"notifications": []}
        raise AssertionError(f"unexpected request {method} {path}")

    def cursor(self):
        return self.connection.cursor()

    def test_function_call_returns_dict_row(self):
        self.transport.results["fn.forge_coding_worker_api.claim"] = {
            "task_id": "t-1", "mode": "plan"}
        cursor = self.cursor()
        cursor.execute(
            "SELECT forge_coding_worker_api.claim(%s, %s, %s)",
            ("worker", "00000000-0000-0000-0000-000000000001", ["plan"]))
        row = cursor.fetchone()
        self.assertEqual(row[0]["task_id"], "t-1")
        self.assertEqual(cursor.rowcount, 1)
        self.assertEqual(cursor.description[0][0], "claim")

    def test_function_call_null_returns_none_row(self):
        self.transport.results["fn.forge_coding_worker_api.claim"] = {}
        cursor = self.cursor()
        cursor.execute(
            "SELECT forge_coding_worker_api.claim(%s, %s, %s)",
            ("worker", "00000000-0000-0000-0000-000000000001", ["plan"]))
        self.assertIsNone(cursor.fetchone()[0])

    def test_entry_point_update_rowcount(self):
        self.transport.results["op.coding_task_update"] = {
            "id": "00000000-0000-0000-0000-000000000001", "status": "running"}
        cursor = self.cursor()
        cursor.execute(
            "UPDATE forge_agent.coding_tasks SET status=%s "
            "WHERE owner_id=%s AND project_alias=%s AND id=%s",
            ("running", "owner", "homelab",
             "00000000-0000-0000-0000-000000000001"))
        self.assertEqual(cursor.rowcount, 1)
        self.assertIsNone(cursor.fetchone())

    def test_entry_point_update_no_match_rowcount_zero(self):
        self.transport.results["op.coding_task_update"] = {}
        cursor = self.cursor()
        cursor.execute(
            "UPDATE forge_agent.coding_tasks SET status=%s "
            "WHERE owner_id=%s AND project_alias=%s AND id=%s",
            ("running", "owner", "homelab",
             "00000000-0000-0000-0000-000000000001"))
        self.assertEqual(cursor.rowcount, 0)

    def test_read_projects_columns(self):
        self.transport.results["read.forge_agent.coding_tasks"] = {
            "rows": [{"id": "a", "status": "queued", "revision": 3,
                      "owner_id": "o", "project_alias": "homelab"}]}
        cursor = self.cursor()
        cursor.execute(
            "SELECT id, revision FROM forge_agent.coding_tasks "
            "WHERE owner_id=%s AND project_alias=%s AND id=%s",
            ("o", "homelab", "a"))
        self.assertEqual(cursor.fetchone(), ("a", 3))
        self.assertEqual(cursor.description[0][0], "id")
        self.assertEqual(cursor.description[1][0], "revision")

    def test_read_count(self):
        self.transport.results["read.forge_agent.coding_tasks.count"] = {
            "count": 7}
        cursor = self.cursor()
        cursor.execute(
            "SELECT count(*) AS n FROM forge_agent.coding_tasks "
            "WHERE owner_id=%s AND status=%s", ("o", "queued"))
        self.assertEqual(cursor.fetchone(), (7,))

    def test_set_config_is_local_noop(self):
        cursor = self.cursor()
        cursor.execute(
            "SELECT pg_catalog.set_config('forge.coding_owner', %s, true)",
            ("owner",))
        self.assertEqual(cursor.fetchone(), ("owner",))
        self.assertEqual(self.transport.requests, [])

    def test_advisory_lock_is_local_success(self):
        cursor = self.cursor()
        cursor.execute("SELECT pg_advisory_xact_lock(%s)", (12345,))
        self.assertEqual(cursor.fetchone(), (True,))

    def test_clock_timestamp_is_local(self):
        before = datetime.now(timezone.utc)
        cursor = self.cursor()
        cursor.execute("SELECT clock_timestamp()")
        value = cursor.fetchone()[0]
        self.assertGreaterEqual(value, before)

    def test_notify_goes_to_proxy(self):
        self.transport.results["notify.forge_agent_jobs"] = {}
        cursor = self.cursor()
        cursor.execute("SELECT pg_notify('forge_agent_jobs', '')")
        self.assertEqual(self.transport.requests[-1][0],
                         "notify.forge_agent_jobs")

    def test_listen_subscribes(self):
        cursor = self.cursor()
        cursor.execute("LISTEN forge_agent_jobs")
        self.assertIn("forge_agent_jobs", self.connection._channels)

    def test_unsupported_statement_raises(self):
        cursor = self.cursor()
        with self.assertRaises(ProxyError) as caught:
            cursor.execute(
                "SELECT * FROM forge_agent.coding_tasks "
                "WHERE owner_id=%s FOR UPDATE", ("o",))
        self.assertEqual(caught.exception.code, "unsupported_statement")

    def test_transaction_calls_are_noops(self):
        self.connection.commit()
        self.connection.rollback()
        with self.connection:
            pass


class TranslateTest(unittest.TestCase):
    def op(self, sql, params=()):
        result = translate(sql, params)
        self.assertIsInstance(result, Operation)
        return result

    def test_fn_positional_binding(self):
        result = self.op(
            "SELECT forge_coding_worker_api.advertise(%s, %s, %s, %s, %s)",
            ("w", "00000000-0000-0000-0000-000000000001", [{"alias": "h"}],
             [{"alias": "p"}], "a" * 64))
        self.assertEqual(result.operation,
                         "fn.forge_coding_worker_api.advertise")
        self.assertEqual(result.args["p_worker_id"], "w")

    def test_fn_default_omitted(self):
        result = self.op(
            "SELECT forge_coding_reconciler_api.pending_reconciliations()")
        self.assertNotIn("p_limit", result.args)

    def test_insert_maps_columns(self):
        result = self.op(
            "INSERT INTO forge_agent.tool_calls (run_id, call_id, tool_name, "
            "arguments, status) VALUES (%s, %s, %s, %s, 'running') "
            "ON CONFLICT (run_id, call_id) DO NOTHING",
            ("00000000-0000-0000-0000-000000000001", "c1", "tool",
             {"a": 1}))
        self.assertEqual(result.operation, "op.ide_tool_call_insert")
        self.assertEqual(result.args["status"], "running")

    def test_insert_computed_sequence_skipped(self):
        result = self.op(
            "INSERT INTO forge_agent.run_events (run_id, sequence, event_type, "
            "data) VALUES (%s, (SELECT coalesce(max(sequence), 0) + 1 FROM "
            "forge_agent.run_events WHERE run_id = %s), %s, %s)",
            ("00000000-0000-0000-0000-000000000001",
             "00000000-0000-0000-0000-000000000001", "queued", {"x": 1}))
        self.assertEqual(result.operation, "op.ide_run_event_insert")
        self.assertNotIn("sequence", result.args)

    def test_insert_task_defaults_skipped(self):
        result = self.op(
            "INSERT INTO forge_agent.coding_tasks (owner_id, project_alias, "
            "id, idempotency_key, intent_digest, base_revision, mode, "
            "agent_profile, title, prompt, acceptance_criteria, "
            "pinned_recipe, recipe_digest, status, revision, auto_repair, "
            "review_required) VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s, "
            "%s, %s, %s, %s, 'queued', 1, %s, %s)",
            ("o", "homelab", "00000000-0000-0000-0000-000000000001",
             "k", "d", "a" * 40, "plan", "p", "t", "prompt", "ac",
             {"r": 1}, "d2", False, False))
        self.assertEqual(result.operation, "op.coding_task_insert")
        self.assertNotIn("status", result.args)
        self.assertNotIn("revision", result.args)

    def test_update_clear_flag(self):
        result = self.op(
            "UPDATE forge_agent.coding_tasks SET cancelled_at=NULL "
            "WHERE owner_id=%s AND project_alias=%s AND id=%s",
            ("o", "homelab", "00000000-0000-0000-0000-000000000001"))
        self.assertTrue(result.args["clear_cancelled_at"])

    def test_update_cancel_transition(self):
        result = self.op(
            "UPDATE forge_agent.coding_tasks SET cancelled_at=CASE WHEN "
            "status='queued' THEN clock_timestamp() ELSE NULL END, "
            "status=CASE WHEN status='queued' THEN 'cancelled' ELSE "
            "'cancel_requested' END, revision=revision+1, "
            "updated_at=clock_timestamp() WHERE owner_id=%s AND "
            "project_alias=%s AND id=%s",
            ("o", "homelab", "00000000-0000-0000-0000-000000000001"))
        self.assertTrue(result.args["cancel_transition"])
        self.assertTrue(result.args["bump_revision"])

    def test_update_baked_guard(self):
        result = self.op(
            "UPDATE forge_agent.tool_calls SET status = 'finished' "
            "WHERE run_id = %s AND call_id = %s AND status = 'running'",
            ("00000000-0000-0000-0000-000000000001", "c1"))
        self.assertEqual(result.operation, "op.ide_tool_call_update")
        self.assertEqual(result.args["status"], "finished")

    def test_update_jenkins_lease_intervals(self):
        result = self.op(
            "UPDATE forge_agent.jenkins_event_deliveries SET status = 'leased', "
            "attempt_count = attempt_count + 1, worker_id = %s, "
            "lease_token = %s, lease_expires_at = clock_timestamp() + "
            "make_interval(secs => %s), updated_at = clock_timestamp() "
            "WHERE delivery_row_id = %s RETURNING *",
            ("w", "00000000-0000-0000-0000-000000000001", 60,
             "00000000-0000-0000-0000-000000000002"))
        self.assertEqual(result.operation,
                         "op.jenkins_event_delivery_update")
        self.assertTrue(result.args["increment_attempt_count"])
        self.assertEqual(result.args["lease_expires_in_secs"], 60)

    def test_delete_any(self):
        result = self.op(
            "DELETE FROM forge_agent.conversations WHERE owner_id = %s "
            "AND id = ANY(%s)",
            ("o", ["00000000-0000-0000-0000-000000000001"]))
        self.assertEqual(result.operation, "op.ide_conversation_delete")
        self.assertEqual(result.args["ids"],
                         ["00000000-0000-0000-0000-000000000001"])

    def test_read_in_filter(self):
        result = self.op(
            "SELECT id FROM forge_agent.coding_tasks WHERE owner_id=%s "
            "AND status IN ('queued', 'running')",
            ("o",))
        self.assertEqual(result.operation, "read.forge_agent.coding_tasks")
        self.assertEqual(result.args["filter"]["status"],
                         ["queued", "running"])

    def test_read_is_null(self):
        result = self.op(
            "SELECT id FROM forge_agent.ide_chats WHERE owner_id=%s "
            "AND removed_at IS NULL",
            ("o",))
        self.assertEqual(result.args["filter"]["removed_at"],
                         {"is_null": True})

    def test_read_bare_boolean(self):
        result = self.op(
            "SELECT id FROM forge_agent.runs WHERE ide_chat_owner_id=%s "
            "AND distributed",
            ("o",))
        self.assertEqual(result.args["filter"]["distributed"], True)

    def test_read_order_and_limit(self):
        result = self.op(
            "SELECT id FROM forge_agent.coding_events WHERE owner_id=%s "
            "ORDER BY sequence ASC LIMIT %s",
            ("o", 50))
        self.assertEqual(result.args["order"], "sequence")
        self.assertEqual(result.args["limit"], 50)

    def test_special_forms(self):
        self.assertIsInstance(
            translate("SELECT pg_catalog.set_config('forge.coding_owner', "
                      "%s, true)", ("o",)), StaticResult)
        self.assertIsInstance(
            translate("SELECT pg_advisory_xact_lock(%s)", (1,)), StaticResult)
        self.assertIsInstance(
            translate("LISTEN forge_agent_jobs"), Subscribe)
        self.assertIsInstance(
            translate("SELECT to_regprocedure('forge_agent.retry_event_project"
                      "(text,text,uuid)') IS NOT NULL AS available"),
            StaticResult)

    def test_for_update_rejected(self):
        from proxy_statements import UnsupportedStatement
        with self.assertRaises(UnsupportedStatement):
            translate("SELECT * FROM forge_agent.coding_tasks WHERE "
                      "owner_id=%s FOR UPDATE", ("o",))


if __name__ == "__main__":
    unittest.main()
