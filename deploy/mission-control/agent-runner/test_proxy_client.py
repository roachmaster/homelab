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

    def test_update_require_revision_guard(self):
        # The WHERE revision is the optimistic-concurrency guard, not
        # the new value: it must reach require_revision (regression
        # test — the first C2 translator bound it to "revision" and
        # the guard was silently dropped).
        result = self.op(
            "UPDATE forge_agent.coding_tasks SET status = %s "
            "WHERE (owner_id, project_alias, id) = (%s, %s, %s) "
            "AND revision = %s",
            ("completed", "o", "homelab",
             "00000000-0000-0000-0000-000000000001", 7))
        self.assertEqual(result.operation, "op.coding_task_update")
        self.assertEqual(result.args["require_revision"], 7)
        self.assertNotIn("revision", result.args)

    def test_update_require_enabled_and_phase_guards(self):
        result = self.op(
            "UPDATE forge_agent.coding_workflows SET enabled = %s "
            "WHERE workflow_id = %s AND enabled = %s",
            (True, "00000000-0000-0000-0000-000000000001", True))
        self.assertEqual(result.args["require_enabled"], True)
        self.assertEqual(result.args["enabled"], True)
        result = self.op(
            "UPDATE forge_agent.coding_workflows SET phase = %s "
            "WHERE workflow_id = %s AND phase NOT IN ('completed', "
            "'cancelled', 'escalated')",
            ("checks", "00000000-0000-0000-0000-000000000001"))
        self.assertEqual(result.args["require_phase_not_in"],
                         ["completed", "cancelled", "escalated"])

    def test_update_require_status_guard(self):
        result = self.op(
            "UPDATE forge_agent.coding_reviews SET status = %s "
            "WHERE review_id = %s AND status = %s",
            ("approved", "00000000-0000-0000-0000-000000000001",
             "awaiting_review"))
        self.assertEqual(result.operation, "op.coding_review_update")
        self.assertEqual(result.args["status"], "approved")
        self.assertEqual(result.args["require_status"], "awaiting_review")

    def test_update_tool_calls_by_run(self):
        # The bulk recover form (queue_store.py recover) has no
        # call_id: it routes to the by-run entry point and shapes
        # {"updated": n} as the rowcount.
        result = self.op(
            "UPDATE forge_agent.tool_calls SET status = %s "
            "WHERE run_id = %s AND status = 'running'",
            ("outcome_unknown", "00000000-0000-0000-0000-000000000001"))
        self.assertEqual(result.operation,
                         "op.ide_tool_call_update_by_run")
        self.assertEqual(result.args["status"], "outcome_unknown")
        self.assertEqual(result.args["require_status"], "running")
        self.assertEqual(result.shape({"updated": 3}), ([], [], 3))

    def test_insert_select_run_event(self):
        result = self.op(
            "INSERT INTO forge_agent.run_events (run_id, sequence, "
            "event_type, data) SELECT %s, coalesce(max(sequence), 0) + 1, "
            "%s, %s FROM forge_agent.run_events WHERE run_id = %s",
            ("00000000-0000-0000-0000-000000000001", "queued", {"x": 1},
             "00000000-0000-0000-0000-000000000001"))
        self.assertEqual(result.operation, "op.ide_run_event_insert")
        self.assertNotIn("sequence", result.args)

    def test_insert_select_message(self):
        result = self.op(
            "INSERT INTO forge_agent.messages (conversation_id, position, "
            "run_id, role, content) SELECT %s, coalesce(max(position), 0) "
            "+ 1, %s, %s, %s FROM forge_agent.messages "
            "WHERE conversation_id = %s",
            ("00000000-0000-0000-0000-000000000001",
             "00000000-0000-0000-0000-000000000001", "assistant", "hi",
             "00000000-0000-0000-0000-000000000001"))
        self.assertEqual(result.operation, "op.ide_message_insert")
        self.assertNotIn("position", result.args)

    def test_insert_select_mail_message(self):
        result = self.op(
            "INSERT INTO forge_agent.mail_messages (owner_id, thread_id, "
            "sequence, run_id, role, content) SELECT %s, %s, "
            "coalesce(max(sequence), 0) + 1, %s, %s, %s FROM "
            "forge_agent.mail_messages WHERE (owner_id, thread_id) = "
            "(%s, %s)",
            ("o", "00000000-0000-0000-0000-000000000001",
             "00000000-0000-0000-0000-000000000001", "assistant", "hi",
             "o", "00000000-0000-0000-0000-000000000001"))
        self.assertEqual(result.operation, "op.mail_message_insert")
        self.assertNotIn("sequence", result.args)

    def test_insert_select_review_retry_copy(self):
        result = self.op(
            "INSERT INTO forge_agent.coding_reviews(owner_id,project_alias,"
            "task_id,attempt_id,plan_decision_id,scope_plan_decision_id,"
            "reconciliation_receipt_id,base_revision,workspace_digest,"
            "parent_result_sha256,source_revision,review_no,round,"
            "stagnant_rounds,infrastructure_retries,previous_review_id,"
            "candidate_commit,summary,phase,owner_decision,verification,"
            "target_publication_id,source_publication_id,"
            "gate_manifest_sha256) SELECT owner_id,project_alias,task_id,"
            "attempt_id,plan_decision_id,scope_plan_decision_id,"
            "reconciliation_receipt_id,base_revision,workspace_digest,"
            "parent_result_sha256,%s,(SELECT coalesce(max(review_no),0)+1 "
            "FROM forge_agent.coding_reviews WHERE owner_id=%s AND "
            "project_alias=%s AND task_id=%s),round,stagnant_rounds,"
            "infrastructure_retries+1,review_id,candidate_commit,%s,phase,"
            "owner_decision,verification,target_publication_id,"
            "source_publication_id,gate_manifest_sha256 FROM "
            "forge_agent.coding_reviews WHERE review_id=%s "
            "RETURNING review_id",
            (8, "o", "homelab", "00000000-0000-0000-0000-000000000001",
             "note", "00000000-0000-0000-0000-000000000001"))
        self.assertEqual(result.operation, "op.coding_review_insert_copy")
        self.assertEqual(result.args["source_revision"], 8)
        self.assertEqual(result.args["summary"], "note")
        self.assertTrue(result.args["increment_infrastructure_retries"])
        self.assertEqual(result.columns, ["review_id"])

    def test_insert_select_review_delivery_copy(self):
        result = self.op(
            "INSERT INTO forge_agent.coding_reviews(owner_id,project_alias,"
            "task_id,attempt_id,plan_decision_id,scope_plan_decision_id,"
            "reconciliation_receipt_id,base_revision,workspace_digest,"
            "parent_result_sha256,source_revision,review_no,round,"
            "stagnant_rounds,infrastructure_retries,previous_review_id,"
            "findings,verified_finding_ids,verification,summary,phase,"
            "owner_decision) SELECT owner_id,project_alias,task_id,"
            "attempt_id,plan_decision_id,scope_plan_decision_id,%s,"
            "base_revision,workspace_digest,parent_result_sha256,"
            "source_revision,review_no+1,round,stagnant_rounds,"
            "infrastructure_retries,review_id,findings,"
            "verified_finding_ids,verification-'failure_stage'-"
            "'exception_type',%s,phase,owner_decision FROM "
            "forge_agent.coding_reviews WHERE review_id=%s "
            "RETURNING review_id",
            ("00000000-0000-0000-0000-000000000001", "delivered",
             "00000000-0000-0000-0000-000000000001"))
        self.assertEqual(result.operation, "op.coding_review_insert_copy")
        self.assertTrue(result.args["strip_verification_keys"])

    def test_insert_select_workflow_action_review_copy(self):
        result = self.op(
            "INSERT INTO forge_agent.coding_workflow_actions(workflow_id,"
            "owner_id,project_alias,workflow_review_id,kind,source_key) "
            "SELECT workflow_id,owner_id,project_alias,workflow_review_id,"
            "'decide',%s FROM forge_agent.coding_workflow_reviews WHERE "
            "workflow_review_id=%s AND status='reviewing' "
            "ON CONFLICT DO NOTHING",
            ("key1", "00000000-0000-0000-0000-000000000001"))
        self.assertEqual(result.operation,
                         "op.coding_workflow_action_insert")
        self.assertTrue(result.args["only_if_reviewing"])
        self.assertTrue(result.args["on_conflict_do_nothing"])


class CompositeCallTest(unittest.TestCase):
    """forge_pg_proxy_api composite calls translate to op.<name>."""

    def op(self, sql, params=()):
        result = translate(sql, params)
        self.assertIsInstance(result, Operation)
        return result

    def test_claim_next_binds_tools_only(self):
        result = self.op(
            "SELECT forge_pg_proxy_api.ide_run_claim_next(%s)", (True,))
        self.assertEqual(result.operation, "op.ide_run_claim_next")
        self.assertEqual(result.args, {"tools_only": True})
        self.assertEqual(result.columns, ["ide_run_claim_next"])

    def test_admit_binds_all_params_and_unwraps_json(self):
        from psycopg2.extras import Json
        from uuid import UUID
        conversation = UUID("00000000-0000-0000-0000-000000000001")
        result = self.op(
            "SELECT forge_pg_proxy_api.ide_run_admit(%s, %s, %s, %s, %s, %s, "
            "%s, %s, %s, %s, %s, %s, %s)",
            (conversation, "req-1", "model", "fp",
             Json([{"role": "user", "content": "hi"}]),
             Json({"model": "model", "max_tokens": 8}),
             True, None, None, None, None, False, False))
        self.assertEqual(result.operation, "op.ide_run_admit")
        self.assertEqual(result.args["conversation_id"],
                         "00000000-0000-0000-0000-000000000001")
        self.assertEqual(result.args["messages"],
                         [{"role": "user", "content": "hi"}])
        self.assertEqual(result.args["settings"],
                         {"model": "model", "max_tokens": 8})
        self.assertIsNone(result.args["chat_id"])
        self.assertFalse(result.args["explicit_conversation"])

    def test_tool_started_unwraps_arguments(self):
        from psycopg2.extras import Json
        result = self.op(
            "SELECT forge_pg_proxy_api.ide_tool_started(%s, %s, %s, %s)",
            ("00000000-0000-0000-0000-000000000001", "c1", "read",
             Json({"path": "README.md"})))
        self.assertEqual(result.operation, "op.ide_tool_started")
        self.assertEqual(result.args["arguments"], {"path": "README.md"})

    def test_interrupt_binds_tools_only(self):
        result = self.op(
            "SELECT forge_pg_proxy_api.ide_interrupt_conversation_runs(%s)",
            (False,))
        self.assertEqual(result.operation,
                         "op.ide_interrupt_conversation_runs")
        self.assertEqual(result.args, {"tools_only": False})

    def test_workflow_cancel_binds_all_params(self):
        result = self.op(
            "SELECT forge_pg_proxy_api.coding_workflow_cancel"
            "(%s, %s, %s, %s, %s, %s, %s)",
            ("homelab", "00000000-0000-0000-0000-000000000001", 1,
             "k1", "operator", "mission-control", "operator_request"))
        self.assertEqual(result.operation, "op.coding_workflow_cancel")
        self.assertEqual(result.args, {
            "project_alias": "homelab",
            "task_id": "00000000-0000-0000-0000-000000000001",
            "expected_revision": 1, "idempotency_key": "k1",
            "actor_kind": "operator", "actor_id": "mission-control",
            "cause": "operator_request"})
        self.assertEqual(result.columns, ["coding_workflow_cancel"])

    def test_optional_default_omitted(self):
        result = self.op(
            "SELECT forge_pg_proxy_api.ide_run_admit(%s, %s, %s, %s, %s, %s, "
            "%s, DEFAULT, DEFAULT, DEFAULT, DEFAULT, DEFAULT, DEFAULT)",
            ("00000000-0000-0000-0000-000000000001", "req-1", "model", "fp",
             [{"role": "user", "content": "hi"}], {"model": "model"}, True))
        self.assertEqual(result.operation, "op.ide_run_admit")
        self.assertNotIn("chat_id", result.args)
        self.assertNotIn("distributed", result.args)

    def test_unknown_composite_is_unsupported(self):
        from proxy_statements import UnsupportedStatement
        with self.assertRaises(UnsupportedStatement):
            translate("SELECT forge_pg_proxy_api.nope(%s)", (1,))

    def test_composite_result_shape(self):
        transport = FakeTransport()
        transport.caller = "coordinator"
        transport.results["op.ide_run_claim_next"] = {
            "run_id": "00000000-0000-0000-0000-000000000001",
            "conversation_id": "00000000-0000-0000-0000-000000000002",
            "model_id": "model", "settings": {"max_tokens": 8},
            "messages": [{"role": "user", "content": "hi"}]}
        connection = ProxyConnection(
            ProxyClient("http://127.0.0.1:18087", "coordinator", token="fake"))
        connection.client = transport
        cursor = connection.cursor()
        cursor.execute(
            "SELECT forge_pg_proxy_api.ide_run_claim_next(%s)", (False,))
        row = cursor.fetchone()
        self.assertEqual(row[0]["model_id"], "model")
        self.assertEqual(cursor.description[0][0], "ide_run_claim_next")
        self.assertEqual(cursor.rowcount, 1)


class ReadQueryTest(unittest.TestCase):
    """proxy_client.run_read_query on both transports."""

    def test_proxy_path_runs_readq_operation(self):
        transport = FakeTransport()
        transport.caller = "coordinator"
        transport.results["readq.run_fingerprint_by_mail"] = {
            "rows": [{"id": "00000000-0000-0000-0000-000000000001",
                      "fingerprint": "abc"}]}
        connection = ProxyConnection(
            ProxyClient("http://127.0.0.1:18087", "coordinator", token="fake"))
        connection.client = transport
        cursor = connection.cursor()
        rows = proxy_client.run_read_query(
            cursor, "run_fingerprint_by_mail",
            {"mail_owner_id": "mission-control",
             "mail_thread_id": "00000000-0000-0000-0000-000000000002",
             "request_id": "req-1"})
        self.assertEqual(rows, [{"id": "00000000-0000-0000-0000-000000000001",
                                 "fingerprint": "abc"}])
        self.assertEqual(transport.requests,
                         [("readq.run_fingerprint_by_mail",
                           {"mail_owner_id": "mission-control",
                            "mail_thread_id": "00000000-0000-0000-0000-000000000002",
                            "request_id": "req-1"})])

    def test_proxy_path_json_encodes_uuid_params(self):
        from uuid import UUID
        transport = FakeTransport()
        transport.caller = "coordinator"
        transport.results["readq.run_detail"] = {"rows": []}
        connection = ProxyConnection(
            ProxyClient("http://127.0.0.1:18087", "coordinator", token="fake"))
        connection.client = transport
        rows = proxy_client.run_read_query(
            connection.cursor(), "run_detail",
            {"run_id": UUID("00000000-0000-0000-0000-000000000001"),
             "owner_id": "mission-control", "distributed_chat": False})
        self.assertEqual(rows, [])
        self.assertEqual(
            transport.requests[0][1]["run_id"],
            "00000000-0000-0000-0000-000000000001")

    def test_direct_path_executes_fixed_sql(self):
        class StubCursor:
            def __init__(self):
                self.executed = []
                self.description = (("id",), ("fingerprint",))
            def execute(self, sql, params):
                self.executed.append((sql, params))
            def fetchall(self):
                return [("00000000-0000-0000-0000-000000000001", "abc")]
        cursor = StubCursor()
        rows = proxy_client.run_read_query(
            cursor, "run_fingerprint_by_mail",
            {"mail_owner_id": "mission-control",
             "mail_thread_id": "00000000-0000-0000-0000-000000000002",
             "request_id": "req-1"})
        self.assertEqual(rows, [{"id": "00000000-0000-0000-0000-000000000001",
                                 "fingerprint": "abc"}])
        self.assertIn("SELECT r.id", cursor.executed[0][0])
        self.assertEqual(cursor.executed[0][1]["request_id"], "req-1")

    def test_repeatable_read_only_is_a_noop(self):
        result = translate(
            "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY", ())
        self.assertIsInstance(result, StaticResult)

    def test_repeatable_read_bare_is_a_noop(self):
        # WorkStore.get/list_tasks/_list_task_scope/events request
        # REPEATABLE READ (without READ ONLY) when an external workflow
        # is present; all four are read-only paths, so the snapshot
        # request degrades to per-operation reads like the READ ONLY
        # form. A read-then-write flow still fails on its FOR UPDATE.
        result = translate(
            "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ", ())
        self.assertIsInstance(result, StaticResult)

    def test_proxy_path_runs_new_coordinator_readqs(self):
        transport = FakeTransport()
        transport.caller = "coordinator"
        transport.results["readq.deadline_retry_count"] = {
            "rows": [{"n": 2}]}
        transport.results["readq.task_root"] = {
            "rows": [{"root": "00000000-0000-0000-0000-000000000001"}]}
        transport.results["readq.worker_readiness_catalog"] = {
            "rows": [{"worker_id": "w1", "is_fresh": True}]}
        transport.results["readq.workflows_open"] = {"rows": []}
        connection = ProxyConnection(
            ProxyClient("http://127.0.0.1:18087", "coordinator", token="<redacted>"))
        connection.client = transport
        cursor = connection.cursor()
        rows = proxy_client.run_read_query(
            cursor, "deadline_retry_count",
            {"workflow_id": "00000000-0000-0000-0000-000000000001"})
        self.assertEqual(rows, [{"n": 2}])
        rows = proxy_client.run_read_query(
            cursor, "task_root",
            {"owner_id": "mission-control", "project_alias": "homelab",
             "task_id": "00000000-0000-0000-0000-000000000001"})
        self.assertEqual(
            rows, [{"root": "00000000-0000-0000-0000-000000000001"}])
        rows = proxy_client.run_read_query(
            cursor, "worker_readiness_catalog",
            {"max_age_seconds": 90, "owner_id": "mission-control"})
        self.assertEqual(rows, [{"worker_id": "w1", "is_fresh": True}])
        rows = proxy_client.run_read_query(
            cursor, "workflows_open", {"owner_id": "mission-control"})
        self.assertEqual(rows, [])
        self.assertEqual(
            [request[0] for request in transport.requests],
            ["readq.deadline_retry_count", "readq.task_root",
             "readq.worker_readiness_catalog", "readq.workflows_open"])

    def test_savepoint_still_unsupported(self):
        from proxy_statements import UnsupportedStatement
        with self.assertRaises(UnsupportedStatement):
            translate("SAVEPOINT sp1", ())


class RestoreActionJobTest(unittest.TestCase):
    """work_workflow._restore_action_job: the composite's to_jsonb
    renders UUIDs and timestamps as strings; the process() job
    contract carries native types."""

    def test_restores_uuids_and_timestamps(self):
        from work_workflow import _restore_action_job
        job = _restore_action_job({
            "action_id": "00000000-0000-0000-0000-000000000001",
            "workflow_id": "00000000-0000-0000-0000-000000000002",
            "workflow_review_id": None,
            "lease_token": "00000000-0000-0000-0000-000000000003",
            "created_at": "2026-11-25T10:00:00+00:00",
            "updated_at": "2026-11-25T10:00:00+00:00",
            "lease_expires_at": None,
            "status": "leased",
        })
        from uuid import UUID
        from datetime import datetime
        self.assertIsInstance(job["action_id"], UUID)
        self.assertIsInstance(job["workflow_id"], UUID)
        self.assertIsNone(job["workflow_review_id"])
        self.assertIsInstance(job["lease_token"], UUID)
        self.assertIsInstance(job["created_at"], datetime)
        self.assertIsNone(job["lease_expires_at"])
        self.assertEqual(job["status"], "leased")


class TransportSelectionTest(unittest.TestCase):
    """The proxy transport switch: env vars for the runner/GPU, the
    database.proxy section for the worker/publication configs."""

    def test_connect_factory_returns_fresh_proxy_connections(self):
        import tempfile
        with tempfile.NamedTemporaryFile("w", delete=False) as handle:
            handle.write("fake-token")
            token_file = handle.name
        factory = proxy_client.connect_factory(
            "http://127.0.0.1:18087", "worker", token_file=token_file)
        first = factory()
        second = factory()
        self.assertIsInstance(first, ProxyConnection)
        self.assertIsInstance(second, ProxyConnection)
        self.assertIsNot(first, second)
        self.assertEqual(first.client.caller, "worker")
        self.assertEqual(first.client.url, "http://127.0.0.1:18087")

    def test_caller_for_role_covers_every_proxy_role(self):
        self.assertEqual(proxy_client.caller_for_role("forge_agent"),
                         "coordinator")
        self.assertEqual(proxy_client.caller_for_role("forge_coding_worker"),
                         "worker")
        self.assertEqual(
            proxy_client.caller_for_role("forge_coding_reconciler"),
            "reconciler")
        self.assertEqual(proxy_client.caller_for_role("forge_gpu_worker"),
                         "gpu")
        self.assertEqual(proxy_client.caller_for_role("muse_proxy_writer"),
                         "muse")
        self.assertEqual(proxy_client.caller_for_role("forge_pg_proxy"),
                         "operator")

    def test_caller_for_role_rejects_an_unknown_role(self):
        with self.assertRaises(ValueError):
            proxy_client.caller_for_role("postgres")

    def test_queue_store_connect_uses_proxy_when_configured(self):
        import queue_store
        import tempfile
        with tempfile.NamedTemporaryFile("w", delete=False) as handle:
            handle.write("fake-token")
            token_file = handle.name
        env = {
            "FORGE_PG_PROXY_URL": "http://127.0.0.1:18087",
            "FORGE_PG_PROXY_TOKEN_FILE": token_file,
            "FORGE_PG_PROXY_CALLER": "coordinator",
        }
        with patch.dict(os.environ, env, clear=False):
            connection = queue_store.connect()
        self.assertIsInstance(connection, ProxyConnection)
        self.assertEqual(connection.client.caller, "coordinator")

    def test_queue_store_connect_stays_direct_without_proxy_env(self):
        import queue_store
        env = {key: "" for key in ("FORGE_PG_PROXY_URL",
                                   "FORGE_PG_PROXY_TOKEN_FILE",
                                   "FORGE_PG_PROXY_CALLER")}
        with patch.dict(os.environ, env, clear=False):
            with patch.object(queue_store.psycopg2, "connect",
                              return_value="direct") as direct:
                self.assertEqual(queue_store.connect(), "direct")
            direct.assert_called_once()

    def test_database_config_proxy_parses_and_selects_caller(self):
        from test_work_executor import raw_config
        from work_worker_config import parse_config
        import tempfile
        with tempfile.TemporaryDirectory() as root:
            token_path = os.path.join(root, "worker.token")
            with open(token_path, "w", encoding="utf-8") as handle:
                handle.write("fake-token")
            raw = raw_config(root)
            raw["database"]["proxy"] = {
                "url": "http://127.0.0.1:18087",
                "token_file": token_path,
            }
            config = parse_config(raw)
            self.assertEqual(config.database.proxy.url,
                             "http://127.0.0.1:18087")
            worker = config.database.connect_factory()()
            self.assertIsInstance(worker, ProxyConnection)
            self.assertEqual(worker.client.caller, "worker")
            reconciler = config.database.connect_factory(
                reconciler=True)()
            self.assertEqual(reconciler.client.caller, "reconciler")

    def test_database_config_without_proxy_stays_direct(self):
        from test_work_executor import raw_config
        from work_worker_config import parse_config
        import tempfile
        with tempfile.TemporaryDirectory() as root:
            config = parse_config(raw_config(root))
            self.assertIsNone(config.database.proxy)
            with patch("psycopg2.connect",
                      return_value="direct") as direct:
                self.assertEqual(config.database.connect_factory()(),
                                 "direct")
            self.assertEqual(direct.call_args.kwargs["user"],
                             "forge_coding_worker")
            with patch("psycopg2.connect",
                      return_value="direct") as direct:
                self.assertEqual(
                    config.database.connect_factory(reconciler=True)(),
                    "direct")
            self.assertEqual(direct.call_args.kwargs["user"],
                             "forge_coding_reconciler")

    def test_database_config_proxy_rejects_bad_url_and_extra_keys(self):
        from test_work_executor import raw_config
        from work_worker_config import parse_config
        import tempfile
        with tempfile.TemporaryDirectory() as root:
            raw = raw_config(root)
            raw["database"]["proxy"] = {"url": "not-a-url",
                                        "token_file": "/x.token"}
            with self.assertRaises(ValueError):
                parse_config(raw)
            raw = raw_config(root)
            raw["database"]["proxy"] = {"url": "http://127.0.0.1:18087",
                                        "token_file": "/x.token",
                                        "extra": True}
            with self.assertRaises(ValueError):
                parse_config(raw)


if __name__ == "__main__":
    unittest.main()
