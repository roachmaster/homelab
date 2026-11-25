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
        # The GPU identity is exactly the five approved operations:
        # no lock_attempt, no inference_result_delete, no reads.
        for operation in ("fn.forge_agent_worker_api.lock_attempt",
                          "op.inference_result_delete",
                          "read.forge_agent.inference_results",
                          "read.forge_agent.runs",
                          "read.forge_agent.gpu_worker_readiness",
                          "notify.forge_agent_jobs"):
            with self.assertRaises(proxy.Rejected) as caught:
                instance.execute(gpu, operation, {})
            self.assertEqual(caught.exception.code, "not_allowed")
        # Subscription and publication are separate: the GPU worker
        # may subscribe to forge_agent_jobs but not publish on it,
        # and Muse may subscribe to its own channel but not publish.
        self.assertTrue(instance.callers["gpu"].allows(
            "subscribe.forge_agent_jobs"))
        self.assertFalse(instance.callers["gpu"].allows(
            "notify.forge_agent_jobs"))
        self.assertTrue(instance.callers["muse"].allows(
            "subscribe.forge_external_agent_jobs"))
        self.assertFalse(instance.callers["muse"].allows(
            "notify.forge_external_agent_jobs"))
        # The coordinator may both publish and subscribe.
        self.assertTrue(instance.callers["coordinator"].allows(
            "subscribe.forge_coding_jobs"))
        self.assertTrue(instance.callers["coordinator"].allows(
            "notify.forge_coding_jobs"))
        # The worker publishes nothing and subscribes to its two
        # channels; the reconciler and operator neither publish nor
        # subscribe.
        self.assertFalse(instance.callers["worker"].allows(
            "notify.forge_coding_jobs"))
        self.assertTrue(instance.callers["worker"].allows(
            "subscribe.forge_coding_usage_refresh"))
        self.assertFalse(instance.callers["reconciler"].allows(
            "subscribe.forge_coding_jobs"))
        self.assertFalse(instance.callers["operator"].allows(
            "subscribe.forge_coding_jobs"))
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

    def test_audit_preserves_identifiers_and_redacts_payload(self):
        args = {
            "id": "00000000-0000-0000-0000-000000000001",
            "attempt_no": 3,
            "status": "running",
            "owner_id": "mission-control",
            "digest": "a" * 64,
            "models": ["gpt-oss-20b", "llama-4"],
            "prompt": "do the thing with the database",
            "content": "a long free-text result body",
            "payload": {"key": "value"},
        }
        kept = proxy._audit_args(args)
        self.assertEqual(
            kept["id"], "00000000-0000-0000-0000-000000000001")
        self.assertEqual(kept["attempt_no"], 3)
        self.assertEqual(kept["status"], "running")
        self.assertEqual(kept["owner_id"], "mission-control")
        self.assertEqual(kept["digest"], "a" * 64)
        self.assertEqual(kept["models"], ["gpt-oss-20b", "llama-4"])
        self.assertEqual(kept["prompt"], "str")
        self.assertEqual(kept["content"], "str")
        self.assertEqual(kept["payload"], "dict")

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
                   if sql.startswith("SELECT t.* FROM")]
        self.assertEqual(len(selects), 1)
        self.assertIn("forge_agent.coding_tasks", selects[0])
        self.assertNotIn("queued", selects[0])

    def test_reads_always_carry_the_callers_owner_scope(self):
        instance, created = make_proxy()
        coordinator = instance.callers["coordinator"]
        # No filter at all: the read is still scoped to the caller.
        instance.execute(coordinator, "read.forge_agent.coding_tasks",
                         {})
        selects = [(sql, params) for sql, params in created[0].statements
                   if sql.startswith("SELECT t.* FROM")]
        self.assertEqual(len(selects), 1)
        self.assertIn("t.owner_id = %s", selects[0][0])
        self.assertEqual(selects[0][1][0], "mission-control")

    def test_reads_reject_a_foreign_owner_filter(self):
        instance, _ = make_proxy()
        coordinator = instance.callers["coordinator"]
        with self.assertRaises(proxy.Rejected) as caught:
            instance.execute(
                coordinator, "read.forge_agent.coding_tasks",
                {"filter": {"owner_id": "other-owner"}})
        self.assertEqual(caught.exception.code, "not_allowed")

    def test_reads_scope_joined_tables_to_the_owner(self):
        instance, created = make_proxy()
        coordinator = instance.callers["coordinator"]
        instance.execute(coordinator,
                         "read.forge_agent.coding_workflow_reviews",
                         {"filter": {"status": "reviewing"}})
        selects = [sql for sql, _ in created[0].statements
                   if sql.startswith("SELECT t.* FROM")]
        self.assertEqual(len(selects), 1)
        self.assertIn(
            "JOIN forge_agent.coding_workflows wf "
            "ON wf.workflow_id = t.workflow_id", selects[0])
        self.assertIn("wf.owner_id = %s", selects[0])

    def test_role_scoped_reads_bind_the_callers_role(self):
        instance, created = make_proxy()
        gpu = instance.callers["gpu"]
        # The GPU worker may not read at all (allowlist), so use the
        # coordinator's view of the table shape via a direct call:
        # gpu_worker_readiness is role-scoped to the reader's role.
        coordinator = instance.callers["coordinator"]
        instance.execute(coordinator,
                         "read.forge_agent.gpu_worker_readiness",
                         {"filter": {"worker_name": "forge_agent"}})
        selects = [(sql, params) for sql, params in created[0].statements
                   if sql.startswith("SELECT t.* FROM")]
        self.assertEqual(len(selects), 1)
        self.assertIn("t.worker_name = %s", selects[0][0])
        self.assertEqual(selects[0][1][0], "forge_agent")
        with self.assertRaises(proxy.Rejected) as caught:
            instance.execute(
                coordinator, "read.forge_agent.gpu_worker_readiness",
                {"filter": {"worker_name": "forge_gpu_worker"}})
        self.assertEqual(caught.exception.code, "not_allowed")
        # And the GPU caller itself may not read the table at all.
        with self.assertRaises(proxy.Rejected) as caught:
            instance.execute(gpu, "read.forge_agent.gpu_worker_readiness",
                             {})
        self.assertEqual(caught.exception.code, "not_allowed")

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


class ProxyHubTest(unittest.TestCase):
    """NotificationHub fan-out: one waiter's delivery never consumes
    another's, and already-queued notifications return at once."""

    def _hub_with_note(self, channel="forge_coding_jobs"):
        hub = proxy.NotificationHub(
            {"host": "127.0.0.1", "port": 5432, "dbname": "forge_agent"},
            channels=frozenset({channel}))
        # Seed one notification the way _listen_loop appends it.
        with hub._cond:
            hub._seq[channel] += 1
            hub._notes[channel].append((hub._seq[channel], {
                "channel": channel, "payload": "", "pid": 1}))
            hub._cond.notify_all()
        return hub

    def test_two_subscribers_both_receive_the_same_notification(self):
        hub = self._hub_with_note()
        first = hub.wait("worker", "forge_coding_jobs", 1)
        second = hub.wait("coordinator", "forge_coding_jobs", 1)
        self.assertEqual(len(first["notifications"]), 1)
        self.assertEqual(len(second["notifications"]), 1)
        self.assertEqual(first["notifications"],
                         second["notifications"])

    def test_a_second_wait_by_the_same_caller_gets_nothing_new(self):
        hub = self._hub_with_note()
        first = hub.wait("worker", "forge_coding_jobs", 1)
        self.assertEqual(len(first["notifications"]), 1)
        second = hub.wait("worker", "forge_coding_jobs", 0)
        self.assertEqual(second, {"notifications": []})

    def test_already_queued_notifications_return_immediately(self):
        import time
        hub = self._hub_with_note()
        start = time.monotonic()
        result = hub.wait("worker", "forge_coding_jobs", 10)
        elapsed = time.monotonic() - start
        self.assertEqual(len(result["notifications"]), 1)
        self.assertLess(elapsed, 5)

    def test_wait_times_out_empty_when_nothing_arrives(self):
        hub = proxy.NotificationHub(
            {"host": "127.0.0.1", "port": 5432, "dbname": "forge_agent"},
            channels=frozenset({"forge_coding_jobs"}))
        self.assertEqual(hub.wait("worker", "forge_coding_jobs", 0),
                         {"notifications": []})

    def test_wait_rejects_an_unknown_channel(self):
        hub = proxy.NotificationHub(
            {"host": "127.0.0.1", "port": 5432, "dbname": "forge_agent"},
            channels=frozenset({"forge_coding_jobs"}))
        with self.assertRaises(proxy.Rejected) as caught:
            hub.wait("worker", "nope", 1)
        self.assertEqual(caught.exception.code, "unknown_operation")


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

    def test_notifications_reject_a_channel_outside_the_callers_allowlist(self):
        # muse may only subscribe to forge_external_agent_jobs; polling the
        # coordinator's channel is a cross-caller subscription.
        status, body = self._post(
            "/v1/notifications",
            {"channel": "forge_coding_jobs", "timeout_seconds": 1},
            token="Bearer test-token-muse")
        self.assertEqual(status, 403)
        self.assertEqual(body, {"error": "not_allowed"})
        self.hub.wait.assert_not_called()

    def test_notifications_allow_the_callers_own_channel(self):
        status, body = self._post(
            "/v1/notifications",
            {"channel": "forge_external_agent_jobs", "timeout_seconds": 1},
            token="Bearer test-token-muse")
        self.assertEqual(status, 200)
        self.assertEqual(body, {"notifications": []})
        self.hub.wait.assert_called_once_with(
            "muse", "forge_external_agent_jobs", 1)


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

    def test_config_allowlist_narrows_only_the_kinds_it_names(self):
        import os
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            config = {"callers": {},
                      "db": {"dbname": "forge_agent",
                             "host": "127.0.0.1", "port": 5432}}
            for name in ("coordinator", "gpu"):
                path = os.path.join(tmp, f"{name}.token")
                with open(path, "w", encoding="utf-8") as handle:
                    handle.write(f"token-{name}")
                spec = {"owner": "mission-control",
                        "token_file": path}
                if name == "gpu":
                    # Narrow only fn: subscribe keeps its built-in
                    # set instead of being silently denied.
                    spec["allowlist"] = {
                        "fn": ["forge_agent_worker_api.advertise"]}
                config["callers"][name] = spec
            instance, _ = proxy.build_proxy(config)
            gpu = instance.callers["gpu"]
            self.assertTrue(gpu.allows(
                "fn.forge_agent_worker_api.advertise"))
            self.assertFalse(gpu.allows(
                "fn.forge_agent_worker_api.claim"))
            self.assertTrue(gpu.allows("subscribe.forge_agent_jobs"))
            self.assertFalse(gpu.allows("notify.forge_agent_jobs"))

    def test_config_allowlist_cannot_widen(self):
        import os
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "gpu.token")
            with open(path, "w", encoding="utf-8") as handle:
                handle.write("token-gpu")
            config = {"callers": {
                "gpu": {"owner": "mission-control",
                        "token_file": path,
                        "allowlist": {
                            "fn": ["forge_agent_worker_api.lock_attempt"]}}},
                      "db": {"dbname": "forge_agent",
                             "host": "127.0.0.1", "port": 5432}}
            with self.assertRaises(ValueError):
                proxy.build_proxy(config)

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
