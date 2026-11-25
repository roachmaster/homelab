"""Verify forge-pg-proxy without a socket or live database.

Covers plan §8: auth, allowlists, identity (session_user), owner
scope binding and connection reuse, actor audit (including spoofing
and atomicity), grants/admission shape, reads, notifications, the
write lock, and health. The database is a fake connection that
records every statement and returns canned rows.
"""

import http.client
import json
import threading
import unittest
from unittest.mock import Mock

import forge_pg_proxy as proxy


class FakeCursor:
    def __init__(self, connection):
        self.connection = connection
        self.statements = connection.statements
        self._row = None

    def execute(self, sql, params=None):
        self.statements.append((sql, params))
        if "current_setting('forge.coding_owner'" in sql:
            self._row = (self.connection.scope_value,)
            return
        if sql.strip() == "SELECT 1":
            self._row = (1,)
            return
        if "forge_pg_proxy_api.audit" in sql:
            if self.connection.fail_audit:
                raise RuntimeError("audit unavailable")
            self._row = (True,)
            return
        if sql.strip().upper().startswith("SELECT"):
            self._row = {"ok": True}
            return
        self._row = None

    def fetchone(self):
        return self._row

    def fetchall(self):
        return []

    def close(self):
        pass


class FakeConnection:
    def __init__(self, user=None, scope_value="", fail_audit=False):
        self.user = user
        self.scope_value = scope_value
        self.fail_audit = fail_audit
        self.statements = []
        self.commits = 0
        self.rollbacks = 0
        self.closed = False

    def cursor(self, cursor_factory=None):
        cursor = FakeCursor(self)
        cursor._row = None
        return cursor

    def commit(self):
        self.commits += 1

    def rollback(self):
        self.rollbacks += 1

    def close(self):
        self.closed = True


def make_proxy(fail_audit=False, scope_value=""):
    created = []

    def connect(**kwargs):
        conn = FakeConnection(user=kwargs.get("user"),
                              scope_value=scope_value,
                              fail_audit=fail_audit)
        created.append(conn)
        return conn

    callers = {}
    for name in ("coordinator", "worker", "reconciler", "gpu", "muse",
                 "operator"):
        callers[name] = proxy.Caller(
            name, proxy.CALLER_ROLES[name], "mission-control",
            proxy.CALLER_ACTOR_KINDS[name], proxy.CALLER_ALLOWLISTS[name])
    instance = proxy.Proxy(callers, {"host": "127.0.0.1", "port": 5432,
                                     "dbname": "forge_agent"},
                           connect=connect)
    return instance, created


class ProxyAuthTest(unittest.TestCase):
    def test_rejects_unknown_operation(self):
        instance, _ = make_proxy()
        coordinator = instance.callers["coordinator"]
        # The coordinator may read any table, so an unregistered table
        # reaches resolution and is rejected as unknown.
        with self.assertRaises(proxy.Rejected) as caught:
            instance.execute(coordinator, "read.forge_agent.nope", {})
        self.assertEqual(caught.exception.code, "unknown_operation")
        # Resolution itself rejects unregistered functions.
        with self.assertRaises(proxy.Rejected) as caught:
            instance._resolve("fn.nope.nope")
        self.assertEqual(caught.exception.code, "unknown_operation")

    def test_allowlists_reject_forbidden_operations(self):
        instance, _ = make_proxy()
        # The GPU worker may not cancel workflows or read coordinator tables.
        gpu = instance.callers["gpu"]
        for operation in ("op.ext_workflow_cancel",
                          "fn.forge_coding_worker_api.claim",
                          "read.forge_agent.coding_tasks"):
            with self.assertRaises(proxy.Rejected) as caught:
                instance.execute(gpu, operation, {})
            self.assertEqual(caught.exception.code, "not_allowed")
        # The worker may not use the GPU API or the Muse transport.
        worker = instance.callers["worker"]
        for operation in ("fn.forge_agent_worker_api.claim",
                          "fn.forge_external_agent_api.claim"):
            with self.assertRaises(proxy.Rejected) as caught:
                instance.execute(worker, operation, {})
            self.assertEqual(caught.exception.code, "not_allowed")
        # Muse may not use service-role functions or coordinator entry points.
        muse = instance.callers["muse"]
        for operation in ("fn.forge_external_agent_api.enqueue",
                          "op.coding_task_update"):
            with self.assertRaises(proxy.Rejected) as caught:
                instance.execute(muse, operation, {})
            self.assertEqual(caught.exception.code, "not_allowed")

    def test_allowlists_permit_the_callers_own_operations(self):
        instance, created = make_proxy()
        gpu = instance.callers["gpu"]
        instance.execute(gpu, "fn.forge_agent_worker_api.advertise",
                         {"p_model": "gpt-oss-20b", "p_ready": True})
        self.assertEqual(created[0].user, "forge_gpu_worker")
        muse = instance.callers["muse"]
        instance.execute(muse, "fn.forge_external_agent_api.claim",
                         {"p_project": "homelab",
                          "p_profile": "muse-db-primary"})
        self.assertEqual(created[1].user, "muse_proxy_writer")


class ProxyIdentityTest(unittest.TestCase):
    def test_executes_with_the_callers_role_as_session_user(self):
        instance, created = make_proxy()
        worker = instance.callers["worker"]
        instance.execute(worker, "fn.forge_coding_worker_api.advertise",
                         {"p_worker_id": "w1",
                          "p_incarnation": "00000000-0000-0000-0000-000000000001",
                          "p_projects": ["homelab"],
                          "p_profiles": ["forge-coding-v10"],
                          "p_policy_digest": "a" * 64})
        self.assertEqual(created[0].user, "forge_coding_worker")

    def test_a_caller_cannot_act_as_another_caller(self):
        instance, created = make_proxy()
        # The worker names a coordinator-only entry point; the allowlist
        # rejects it before any connection is opened.
        worker = instance.callers["worker"]
        with self.assertRaises(proxy.Rejected):
            instance.execute(worker, "op.ext_workflow_cancel",
                             {"project_alias": "homelab",
                              "task_id": "00000000-0000-0000-0000-000000000001",
                              "expected_revision": 1,
                              "idempotency_key": "k",
                              "actor_kind": "operator",
                              "actor_id": "someone-else",
                              "cause": "x"})
        self.assertEqual(created, [])


class ProxyScopeTest(unittest.TestCase):
    def test_binds_owner_scope_from_trusted_config(self):
        instance, created = make_proxy()
        coordinator = instance.callers["coordinator"]
        instance.execute(coordinator, "op.coding_task_update",
                         {"owner_id": "mission-control",
                          "project_alias": "homelab",
                          "id": "00000000-0000-0000-0000-000000000001",
                          "status": "running"})
        scope_sets = [params for sql, params in created[0].statements
                      if sql.startswith("SET LOCAL")]
        self.assertEqual(scope_sets, [("mission-control",)])

    def test_a_request_cannot_select_a_scope(self):
        instance, created = make_proxy()
        coordinator = instance.callers["coordinator"]
        # A request that smuggles an owner field is rejected (unknown arg)
        # and the scope is still the trusted one.
        with self.assertRaises(proxy.Rejected):
            instance.execute(coordinator, "op.coding_task_update",
                             {"owner_id": "mission-control",
                              "project_alias": "homelab",
                              "id": "00000000-0000-0000-0000-000000000001",
                              "status": "running",
                              "forge.coding_owner": "attacker"})
        self.assertEqual(created, [])

    def test_a_reused_connection_with_a_leaked_scope_is_discarded(self):
        instance, created = make_proxy()
        coordinator = instance.callers["coordinator"]
        args = {"owner_id": "mission-control",
                "project_alias": "homelab",
                "id": "00000000-0000-0000-0000-000000000001",
                "status": "running"}
        instance.execute(coordinator, "op.coding_task_update", args)
        self.assertEqual(len(created), 1)
        # Simulate a leaked scope on the pooled connection.
        created[0].scope_value = "stale-owner"
        instance.execute(coordinator, "op.coding_task_update", args)
        # The pooled connection was discarded; a fresh one served it.
        self.assertEqual(len(created), 2)
        self.assertTrue(created[0].closed)
        self.assertFalse(created[1].closed)


class ProxyAuditTest(unittest.TestCase):
    def test_audit_uses_the_authenticated_caller_not_the_request(self):
        instance, created = make_proxy()
        coordinator = instance.callers["coordinator"]
        # A request that smuggles an actor field is rejected outright:
        # the actor never comes from the request.
        with self.assertRaises(proxy.Rejected) as caught:
            instance.execute(coordinator, "op.coding_task_update",
                             {"owner_id": "mission-control",
                              "project_alias": "homelab",
                              "id": "00000000-0000-0000-0000-000000000001",
                              "status": "running",
                              "actor": {"kind": "operator", "id": "mallory"}})
        self.assertEqual(caught.exception.code, "invalid_argument")
        self.assertEqual(created, [])
        # A clean request records the authenticated caller's actor.
        instance.execute(coordinator, "op.coding_task_update",
                         {"owner_id": "mission-control",
                          "project_alias": "homelab",
                          "id": "00000000-0000-0000-0000-000000000001",
                          "status": "running"})
        audits = [(sql, params) for sql, params in created[0].statements
                  if "forge_pg_proxy_api.audit" in sql]
        self.assertEqual(len(audits), 1)
        self.assertEqual(audits[0][1][:3],
                         ("coordinator", "coordinator", "entry.coding_task_update"))

    def test_a_failed_audit_rolls_the_mutation_back(self):
        instance, created = make_proxy(fail_audit=True)
        coordinator = instance.callers["coordinator"]
        with self.assertRaises(proxy.Rejected) as caught:
            instance.execute(coordinator, "op.coding_task_update",
                             {"owner_id": "mission-control",
                              "project_alias": "homelab",
                              "id": "00000000-0000-0000-0000-000000000001",
                              "status": "running"})
        self.assertEqual(caught.exception.code, "database_error")
        self.assertGreaterEqual(created[0].rollbacks, 1)
        self.assertEqual(created[0].commits, 0)


class ProxyReadTest(unittest.TestCase):
    def test_reads_use_fixed_sql_with_only_registered_filters(self):
        instance, created = make_proxy()
        coordinator = instance.callers["coordinator"]
        result = instance.execute(
            coordinator, "read.forge_agent.coding_tasks",
            {"filter": {"owner_id": "mission-control", "status": "queued"},
             "limit": 5})
        self.assertEqual(result, {"rows": []})
        selects = [sql for sql, _ in created[0].statements
                   if sql.startswith("SELECT * FROM")]
        self.assertEqual(len(selects), 1)
        self.assertIn("forge_agent.coding_tasks", selects[0])
        self.assertNotIn("queued", selects[0])

    def test_reads_reject_unknown_tables_and_columns(self):
        instance, _ = make_proxy()
        coordinator = instance.callers["coordinator"]
        with self.assertRaises(proxy.Rejected):
            instance.execute(coordinator, "read.forge_agent.nope", {})
        with self.assertRaises(proxy.Rejected):
            instance.execute(
                coordinator, "read.forge_agent.coding_tasks",
                {"filter": {"nonexistent": "x"}})
        with self.assertRaises(proxy.Rejected):
            instance.execute(
                coordinator, "read.forge_agent.coding_tasks",
                {"limit": 100000})


class ProxyLockTest(unittest.TestCase):
    def test_concurrent_writes_serialize(self):
        # The fake connection sleeps inside each statement; if the
        # write lock did not serialize, the statement intervals of
        # concurrent writers would overlap.
        import time

        class SlowCursor(FakeCursor):
            def execute(self, sql, params=None):
                start = time.monotonic()
                time.sleep(0.02)
                super().execute(sql, params)
                self.connection.intervals.append(
                    (start, time.monotonic()))

        class SlowConnection(FakeConnection):
            def __init__(self, *args, **kwargs):
                super().__init__(*args, **kwargs)
                self.intervals = []

            def cursor(self, cursor_factory=None):
                return SlowCursor(self)

        created = []

        def connect(**kwargs):
            conn = SlowConnection(user=kwargs.get("user"))
            created.append(conn)
            return conn

        callers = {}
        for name in ("coordinator",):
            callers[name] = proxy.Caller(
                name, proxy.CALLER_ROLES[name], "mission-control",
                proxy.CALLER_ACTOR_KINDS[name], proxy.CALLER_ALLOWLISTS[name])
        instance = proxy.Proxy(
            callers, {"host": "127.0.0.1", "port": 5432,
                      "dbname": "forge_agent"}, connect=connect)
        coordinator = instance.callers["coordinator"]

        def run():
            instance.execute(coordinator, "op.coding_task_update",
                             {"owner_id": "mission-control",
                              "project_alias": "homelab",
                              "id": "00000000-0000-0000-0000-000000000001",
                              "status": "running"})

        threads = [threading.Thread(target=run) for _ in range(4)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(timeout=10)
        self.assertFalse(any(thread.is_alive() for thread in threads))
        intervals = sorted(i for conn in created for i in conn.intervals)
        for first, second in zip(intervals, intervals[1:]):
            self.assertLessEqual(first[1], second[0] + 0.005)
        # Four writers ran; the pool reuses connections, so the count
        # is not the assertion — the non-overlap above is.


class ProxyHealthTest(unittest.TestCase):
    def test_health_ok_when_the_database_answers(self):
        instance, _ = make_proxy()
        self.assertTrue(instance.health())

    def test_health_fails_when_the_database_is_unreachable(self):
        def failing_connect(**kwargs):
            raise RuntimeError("no database")

        instance, _ = make_proxy()
        instance.connect = failing_connect
        self.assertFalse(instance.health())


class ProxyHttpTest(unittest.TestCase):
    def setUp(self):
        self.instance, _ = make_proxy()
        self.tokens = {name: f"Bearer test-token-{name}".encode()
                       for name in self.instance.callers}
        self.hub = Mock()
        self.hub.wait.return_value = {"notifications": []}
        self.server = proxy.Server(("127.0.0.1", 0), self.instance,
                                   self.hub, self.tokens)
        self.thread = threading.Thread(target=self.server.serve_forever,
                                       daemon=True)
        self.thread.start()
        self.port = self.server.server_address[1]

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)

    def _post(self, path, body, token=None):
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        headers = {"Content-Type": "application/json"}
        if token is not None:
            headers["Authorization"] = token
        conn.request("POST", path, json.dumps(body), headers)
        response = conn.getresponse()
        return response.status, json.loads(response.read())

    def test_unauthenticated_call_is_rejected(self):
        status, body = self._post("/v1/operation", {"operation": "read.forge_agent.coding_tasks", "args": {}})
        self.assertEqual(status, 401)
        self.assertEqual(body, {"error": "unauthorized"})

    def test_wrong_token_is_rejected(self):
        status, body = self._post(
            "/v1/operation", {"operation": "read.forge_agent.coding_tasks", "args": {}},
            token="Bearer wrong")
        self.assertEqual(status, 401)

    def test_authenticated_call_succeeds(self):
        status, body = self._post(
            "/v1/operation",
            {"operation": "read.forge_agent.coding_tasks",
             "args": {"filter": {"owner_id": "mission-control"}}},
            token="Bearer test-token-coordinator")
        self.assertEqual(status, 200)
        self.assertEqual(body["status"], "ok")

    def test_health_endpoint(self):
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        conn.request("GET", "/health")
        response = conn.getresponse()
        self.assertEqual(response.status, 200)
        self.assertEqual(json.loads(response.read())["proxy"], "ok")

    def test_notifications_require_a_known_channel(self):
        self.hub.wait.side_effect = proxy.Rejected(400, "unknown_operation")
        status, body = self._post(
            "/v1/notifications", {"channel": "nope", "timeout_seconds": 1},
            token="Bearer test-token-gpu")
        self.assertEqual(status, 400)
        self.assertEqual(body, {"error": "unknown_operation"})


class ProxyCatalogTest(unittest.TestCase):
    def test_every_registered_function_is_in_the_catalog(self):
        # The proxy must know every public API function the codebase can
        # call, so a caller can never name an unregistered function.
        self.assertIn("forge_coding_worker_api.claim", proxy.FUNC_CATALOG)
        self.assertIn("forge_coding_reconciler_api.reconcile",
                      proxy.FUNC_CATALOG)
        self.assertIn("forge_agent_worker_api.publish", proxy.FUNC_CATALOG)
        self.assertIn("forge_external_agent_api.finish", proxy.FUNC_CATALOG)
        self.assertIn("forge_external_agent_api.enqueue", proxy.FUNC_CATALOG)

    def test_every_entry_point_has_a_matching_allowlist_entry(self):
        allowed = set()
        for spec in proxy.CALLER_ALLOWLISTS.values():
            allowed |= spec["op"]
        missing = set(proxy.ENTRY_POINTS) - allowed
        self.assertEqual(missing, set())

    def test_every_allowlisted_function_exists_in_the_catalog(self):
        catalog = set(proxy.FUNC_CATALOG)
        for name, spec in proxy.CALLER_ALLOWLISTS.items():
            unknown = set(spec["fn"]) - catalog
            self.assertEqual(unknown, set(), name)

    def test_argument_validation_rejects_wrong_types(self):
        with self.assertRaises(proxy.Rejected):
            proxy.validate_args([("id", "uuid", True)],
                                {"id": "not-a-uuid"})
        with self.assertRaises(proxy.Rejected):
            proxy.validate_args([("n", "integer", True)], {"n": "3"})
        self.assertEqual(
            proxy.validate_args([("n", "integer", True)], {"n": 3}), [3])
        self.assertEqual(
            proxy.validate_args([("x", "text", False)], {}), [None])

    def test_reconciled_functions_are_in_the_catalog(self):
        # Direct-write inventory: the callers invoke these functions and
        # no entry point may duplicate them (plan §5).
        for name in (
                "forge_agent.coding_make_feedback",
                "forge_agent.amend_agent_retry_policy",
                "forge_agent.coding_schedule_repair",
                "forge_agent.coding_queue_approved_review",
                "forge_agent.coding_workflow_role_granted",
                "forge_agent.delivery_project_head",
                "forge_agent.delivery_repair_admit",
                "forge_agent.delivery_repair_bind",
                "forge_agent.owner_continuation_capability",
                "forge_agent.retry_event_adopt_legacy",
                "forge_agent.retry_event_bind_review",
                "forge_agent.retry_event_get",
                "forge_agent.retry_event_observe",
                "forge_agent.retry_event_project",
                "forge_agent.retry_event_reserve",
                "forge_agent.retry_event_resolve",
                "forge_external_workflow_api.enqueue_v2",
                "forge_external_workflow_api.service_scope",
                "forge_external_workflow_worker_api.read_context"):
            self.assertIn(name, proxy.FUNC_CATALOG)

    def test_reconciled_entry_points_validate(self):
        # The corrected registry: mail writes, the review copy insert,
        # and the state-machine/clear flags the update sites need.
        self.assertIn("mail_thread_insert", proxy.ENTRY_POINTS)
        self.assertIn("mail_thread_update", proxy.ENTRY_POINTS)
        self.assertIn("mail_message_insert", proxy.ENTRY_POINTS)
        self.assertIn("coding_review_insert_copy", proxy.ENTRY_POINTS)
        values = proxy.validate_args(
            proxy.ENTRY_POINTS["coding_task_update"][0],
            {"owner_id": "mission-control", "project_alias": "homelab",
             "id": "00000000-0000-0000-0000-000000000001",
             "cancel_transition": True, "clear_cancelled_at": False})
        self.assertTrue(values[proxy.ENTRY_POINTS["coding_task_update"][0]
                               .index(("cancel_transition", "boolean", False))])
        proxy.validate_args(
            proxy.ENTRY_POINTS["jenkins_event_delivery_update"][0],
            {"delivery_row_id": "00000000-0000-0000-0000-000000000001",
             "increment_attempt_count": True, "lease_expires_in_secs": 60})
        proxy.validate_args(
            proxy.ENTRY_POINTS["ide_run_delete"][0],
            {"ide_chat_owner_id": "mission-control",
             "ide_chat_id": "00000000-0000-0000-0000-000000000001"})
        proxy.validate_args(
            proxy.ENTRY_POINTS["ide_conversation_delete"][0],
            {"owner_id": "mission-control",
             "ids": ["00000000-0000-0000-0000-000000000001"]})
        proxy.validate_args(
            proxy.ENTRY_POINTS["mail_thread_update"][0],
            {"owner_id": "mission-control",
             "id": "00000000-0000-0000-0000-000000000001",
             "archive": True})


class TupleRowCursor(FakeCursor):
    """A cursor that returns rows the way psycopg2 does: fetchone()
    yields a 1-tuple, not a dict. A function returning jsonb yields
    a tuple holding a dict; a scalar function yields a tuple holding
    the scalar."""

    def execute(self, sql, params=None):
        self.statements.append((sql, params))
        if "current_setting('forge.coding_owner'" in sql:
            self._row = (self.connection.scope_value,)
            return
        if "forge_pg_proxy_api.audit" in sql:
            self._row = (True,)
            return
        if sql.strip().upper().startswith("SELECT"):
            self._row = ({"workflow_id": "00000000-0000-0000-0000-000000000001"},)
            return
        self._row = None


class TupleRowConnection(FakeConnection):
    def cursor(self, cursor_factory=None):
        return TupleRowCursor(self)


class ProxyResultDecodingTest(unittest.TestCase):
    """_execute_write must decode the tuple rows a real database
    returns; treating the tuple as a dict 503'd every call."""

    def _tuple_proxy(self):
        created = []

        def connect(**kwargs):
            conn = TupleRowConnection(user=kwargs.get("user"))
            created.append(conn)
            return conn

        callers = {}
        for name in ("coordinator", "worker", "reconciler", "gpu", "muse",
                     "operator"):
            callers[name] = proxy.Caller(
                name, proxy.CALLER_ROLES[name], "mission-control",
                proxy.CALLER_ACTOR_KINDS[name], proxy.CALLER_ALLOWLISTS[name])
        return proxy.Proxy(callers, {"host": "127.0.0.1", "port": 5432,
                                     "dbname": "forge_agent"},
                           connect=connect), created

    def test_function_result_tuple_decodes_to_the_jsonb_value(self):
        instance, _ = self._tuple_proxy()
        result = instance.execute(
            instance.callers["coordinator"],
            "fn.forge_agent.coding_make_feedback",
            {"p_review": "00000000-0000-0000-0000-000000000001",
             "p_task": "00000000-0000-0000-0000-000000000002",
             "p_disposition": "approve", "p_summary": "ok",
             "p_findings": []})
        self.assertEqual(
            result, {"workflow_id": "00000000-0000-0000-0000-000000000001"})

    def test_entry_point_result_tuple_decodes_to_the_jsonb_value(self):
        instance, _ = self._tuple_proxy()
        result = instance.execute(
            instance.callers["coordinator"],
            "op.coding_workflow_insert",
            {"owner_id": "mission-control", "project_alias": "homelab",
             "root_task_id": "00000000-0000-0000-0000-000000000001",
             "policy_digest": "a" * 64, "snapshot": {}})
        self.assertEqual(
            result, {"workflow_id": "00000000-0000-0000-0000-000000000001"})

    def test_decode_result_handles_dict_none_and_scalar(self):
        self.assertEqual(proxy._decode_result(None), {})
        self.assertEqual(proxy._decode_result({"a": 1}), {"a": 1})
        self.assertEqual(proxy._decode_result(({"a": 1},)), {"a": 1})
        self.assertEqual(proxy._decode_result((None,)), {})
        self.assertEqual(proxy._decode_result((7,)), {"value": 7})


if __name__ == "__main__":
    unittest.main()
