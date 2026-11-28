"""Verify forge-pg-proxy without a socket or live database.

Covers plan §8: auth, allowlists, identity (session_user), owner
scope binding and connection reuse, actor audit (including spoofing
and atomicity), grants/admission shape, reads, notifications, the
write lock, and health. The database is a fake connection that
records every statement and returns canned rows.
"""

import http.client
import json
import re
import threading
import unittest
from unittest.mock import Mock

import forge_pg_proxy as proxy


class FakeCursor:
    def __init__(self, connection):
        self.connection = connection
        self.statements = connection.statements
        self._row = None
        self.description = None

    def execute(self, sql, params=None):
        self.statements.append((sql, params))
        if "current_setting('forge.coding_owner'" in sql:
            self._row = (self.connection.scope_value,)
            return
        if sql.strip() == "SELECT 1":
            self._row = (1,)
            return
        if sql.strip().startswith("SELECT count(*)"):
            self._row = {"count": 3}
            return
        if "SELECT EXISTS(" in sql:
            self._row = {"exists": True}
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
        # A token-like prompt (no spaces, under the safe length) must
        # still be redacted: redaction is by key, not by value shape.
        kept = proxy._audit_args({"prompt": "short-token-prompt"})
        self.assertEqual(kept["prompt"], "str")

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
        # The coordinator monitors GPU workers, so it is exempt from
        # the role scope: it may filter worker_name freely (including
        # the GPU role) or read all rows with no worker predicate.
        instance, created = make_proxy()
        coordinator = instance.callers["coordinator"]
        instance.execute(coordinator,
                         "read.forge_agent.gpu_worker_readiness",
                         {"filter": {"worker_name": "forge_gpu_worker"}})
        selects = [(sql, params) for sql, params in created[0].statements
                   if sql.startswith("SELECT t.* FROM")]
        self.assertEqual(len(selects), 1)
        self.assertIn("t.worker_name = %s", selects[0][0])
        self.assertEqual(selects[0][1][0], "forge_gpu_worker")
        instance, created = make_proxy()
        coordinator = instance.callers["coordinator"]
        instance.execute(coordinator,
                         "read.forge_agent.gpu_worker_readiness", {})
        selects = [(sql, params) for sql, params in created[0].statements
                   if sql.startswith("SELECT t.* FROM")]
        self.assertEqual(len(selects), 1)
        self.assertNotIn("worker_name", selects[0][0])
        # A non-exempt caller is still bound to its own role.
        instance, created = make_proxy()
        restricted = proxy.Caller(
            "custom", "forge_coding_worker", "mission-control", "worker",
            {"fn": set(), "op": set(),
             "read": {"forge_agent.gpu_worker_readiness"},
             "notify": set(), "subscribe": set()})
        instance.callers["custom"] = restricted
        with self.assertRaises(proxy.Rejected) as caught:
            instance.execute(
                restricted, "read.forge_agent.gpu_worker_readiness",
                {"filter": {"worker_name": "forge_gpu_worker"}})
        self.assertEqual(caught.exception.code, "not_allowed")
        instance.execute(restricted,
                         "read.forge_agent.gpu_worker_readiness", {})
        selects = [(sql, params) for sql, params in created[0].statements
                   if sql.startswith("SELECT t.* FROM")]
        self.assertEqual(len(selects), 1)
        self.assertIn("t.worker_name = %s", selects[0][0])
        self.assertEqual(selects[0][1][0], "forge_coding_worker")
        # And the GPU caller itself may not read the table at all.
        instance, _ = make_proxy()
        gpu = instance.callers["gpu"]
        with self.assertRaises(proxy.Rejected) as caught:
            instance.execute(gpu, "read.forge_agent.gpu_worker_readiness",
                             {})
        self.assertEqual(caught.exception.code, "not_allowed")

    def test_reads_count_and_exists_share_filters_and_scope(self):
        instance, created = make_proxy()
        coordinator = instance.callers["coordinator"]
        result = instance.execute(
            coordinator, "read.forge_agent.coding_tasks.count",
            {"filter": {"status": "queued"}})
        self.assertEqual(result, {"count": 3})
        counts = [sql for sql, _ in created[0].statements
                  if sql.startswith("SELECT count(*)")]
        self.assertEqual(len(counts), 1)
        self.assertIn("t.owner_id = %s", counts[0])
        self.assertIn("t.status = %s", counts[0])
        result = instance.execute(
            coordinator, "read.forge_agent.coding_tasks.exists",
            {"filter": {"status": "queued"}})
        self.assertEqual(result, {"exists": True})
        exists = [sql for sql, _ in created[0].statements
                  if "SELECT EXISTS(" in sql]
        self.assertEqual(len(exists), 1)
        self.assertIn("t.owner_id = %s", exists[0])
        # The same scope rules apply: a foreign owner filter is
        # refused for the aggregate forms too.
        with self.assertRaises(proxy.Rejected) as caught:
            instance.execute(
                coordinator, "read.forge_agent.coding_tasks.count",
                {"filter": {"owner_id": "other-owner"}})
        self.assertEqual(caught.exception.code, "not_allowed")
        # Aggregates share the table's allowlist entry.
        worker = instance.callers["worker"]
        with self.assertRaises(proxy.Rejected) as caught:
            instance.execute(
                worker, "read.forge_agent.coding_publications.count", {})
        self.assertEqual(caught.exception.code, "not_allowed")
        with self.assertRaises(proxy.Rejected) as caught:
            instance.execute(coordinator, "read.forge_agent.nope.count",
                             {})
        self.assertEqual(caught.exception.code, "unknown_operation")

    def test_reads_paginate_and_order(self):
        instance, created = make_proxy()
        coordinator = instance.callers["coordinator"]
        instance.execute(coordinator, "read.forge_agent.coding_tasks",
                         {"limit": 5, "offset": 10, "order_desc": True})
        selects = [(sql, params) for sql, params in created[0].statements
                   if sql.startswith("SELECT t.* FROM")]
        self.assertEqual(len(selects), 1)
        self.assertIn("ORDER BY t.created_at DESC LIMIT %s OFFSET %s",
                      selects[0][0])
        self.assertEqual(selects[0][1][-2:], [5, 10])
        # Defaults: ascending order from offset 0.
        instance.execute(coordinator, "read.forge_agent.coding_tasks", {})
        selects = [(sql, params) for sql, params in created[0].statements
                   if sql.startswith("SELECT t.* FROM")]
        self.assertEqual(len(selects), 2)
        self.assertIn("ORDER BY t.created_at ASC LIMIT %s OFFSET %s",
                      selects[1][0])
        self.assertEqual(selects[1][1][-2:], [100, 0])
        for bad in ({"offset": -1}, {"offset": 100001},
                    {"offset": "10"}, {"order_desc": "yes"}):
            with self.assertRaises(proxy.Rejected) as caught:
                instance.execute(coordinator,
                                 "read.forge_agent.coding_tasks", bad)
            self.assertEqual(caught.exception.code, "invalid_argument")

    def test_readq_rejects_a_foreign_exact_owner_id(self):
        # Regression (round-6 finding 4): the owner-scope rule matched
        # only parameter names ending in "_owner_id", so a parameter
        # named exactly "owner_id" passed through unbound and any
        # caller could aim a named read at another owner's rows.
        instance, created = make_proxy()
        coordinator = instance.callers["coordinator"]
        with self.assertRaises(proxy.Rejected) as caught:
            instance.execute(
                coordinator, "readq.worker_readiness_catalog",
                {"max_age_seconds": 90, "owner_id": "other-owner"})
        self.assertEqual(caught.exception.code, "not_allowed")
        # Refused before any database connection was opened.
        self.assertEqual(created, [])

    def test_readq_binds_the_exact_owner_id_to_the_caller(self):
        instance, created = make_proxy()
        coordinator = instance.callers["coordinator"]
        result = instance.execute(
            coordinator, "readq.worker_readiness_catalog",
            {"max_age_seconds": 90, "owner_id": "mission-control"})
        self.assertEqual(result, {"rows": []})
        selects = [(sql, params) for sql, params in created[0].statements
                   if "coding_worker_readiness" in sql]
        self.assertEqual(len(selects), 1)
        self.assertEqual(selects[0][1]["owner_id"], "mission-control")

    def test_readq_object_id_queries_reject_a_foreign_owner(self):
        # The workflow-keyed named reads gained a required owner_id
        # in the same fix: a foreign value is refused, and omitting
        # it is an argument error (validate_args), never a silent
        # unscoped read.
        instance, created = make_proxy()
        coordinator = instance.callers["coordinator"]
        with self.assertRaises(proxy.Rejected) as caught:
            instance.execute(
                coordinator, "readq.deadline_retry_count",
                {"workflow_id": "00000000-0000-0000-0000-000000000001",
                 "owner_id": "other-owner"})
        self.assertEqual(caught.exception.code, "not_allowed")
        with self.assertRaises(proxy.Rejected):
            instance.execute(
                coordinator, "readq.deadline_retry_count",
                {"workflow_id": "00000000-0000-0000-0000-000000000001"})
        self.assertEqual(created, [])

    def test_defect_and_retained_source_reads_reject_foreign_owner_and_extra_scope(self):
        for name in ('filed_defects', 'external_filed_defects',
                     'retained_incident_sources', 'retained_baseline_sources'):
            instance, created = make_proxy()
            coordinator = instance.callers['coordinator']
            args = {'owner_id':'mission-control', 'project_alias':'homelab',
                    'workflow_id':'00000000-0000-0000-0000-000000000001'}
            if 'defects' in name:
                args['task_id'] = '00000000-0000-0000-0000-000000000002'
            for bad in ({**args,'owner_id':'other-owner'},
                        {key:value for key,value in args.items() if key != 'owner_id'},
                        {**args,'source_pattern':'%'}):
                with self.subTest(name=name,args=bad), self.assertRaises(proxy.Rejected):
                    instance.execute(coordinator, 'readq.'+name, bad)
            self.assertEqual(created, [])
            for caller in instance.callers.values():
                if caller is coordinator: continue
                with self.subTest(name=name,caller=caller), self.assertRaises(proxy.Rejected):
                    instance.execute(caller, 'readq.'+name, args)

    def test_every_readq_is_owner_scoped(self):
        # Structural guard: every named read declares exactly the
        # parameters its SQL uses, and (except the two GPU readiness
        # queries — gpu_worker_readiness has no owner column, it is
        # global worker state — and the author-handoff capability
        # probe, which reads only the schema catalog and returns no
        # table data) one of them is owner-scoped.
        exceptions = {"gpu_ready_models", "gpu_worker_ready",
                      "author_handoff_available"}
        for name, (sql, params) in proxy.READ_QUERIES.items():
            declared = {param[0] for param in params}
            used = set(re.findall(r"%\(([a-z0-9_]+)\)s", sql))
            self.assertEqual(used, declared, name)
            if name in exceptions:
                continue
            owner_params = {
                param for param in declared
                if param == "owner_id" or param.endswith("_owner_id")}
            self.assertTrue(owner_params, name)

    def test_proxy_connections_identify_themselves(self):
        # Round-6 finding 6: the cutover quiescence check excludes
        # the proxy's own sessions by application_name, so every
        # connection the proxy opens must carry it — otherwise the
        # drain counts the proxy's pooled caller-role sessions and
        # can never settle.
        seen = []

        def connect(**kwargs):
            seen.append(kwargs)
            return FakeConnection(user=kwargs.get("user"))

        callers = {}
        for name in ("coordinator",):
            callers[name] = proxy.Caller(
                name, proxy.CALLER_ROLES[name], "mission-control",
                proxy.CALLER_ACTOR_KINDS[name],
                proxy.CALLER_ALLOWLISTS[name])
        instance = proxy.Proxy(
            callers, {"host": "127.0.0.1", "port": 5432,
                      "dbname": "forge_agent"}, connect=connect)
        instance.execute(callers["coordinator"],
                         "read.forge_agent.coding_tasks", {})
        self.assertTrue(seen)
        self.assertTrue(all(
            kwargs.get("application_name") == "forge-pg-proxy"
            for kwargs in seen))

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

    def test_argument_validation_accepts_nulls(self):
        # Finding review-function-nullability-contract: validation
        # must accept the SQL NULLs valid DAO calls contain (a failed
        # publication has no branch, commit, or PR URL); the function
        # and column constraints decide whether a NULL is valid.
        values = proxy.validate_args(
            proxy.func_params(
                "forge_coding_reconciler_api.publication_finish",
                proxy.FUNC_CATALOG[
                    "forge_coding_reconciler_api.publication_finish"]),
            {"p_publication_id": "00000000-0000-0000-0000-000000000001",
             "p_lease_token": "00000000-0000-0000-0000-000000000002",
             "p_status": "failed", "p_branch": None,
             "p_commit_sha": None, "p_pr_url": None,
             "p_summary": "failed"})
        self.assertEqual(values[3:], [None, None, None, "failed"])
        for kind in ("text", "uuid", "integer", "bigint", "boolean",
                     "jsonb", "text[]"):
            self.assertIsNone(proxy.validate_value(kind, None))
        # Wrong types are still rejected.
        with self.assertRaises(proxy.Rejected):
            proxy.validate_args([("n", "integer", True)], {"n": "3"})
        with self.assertRaises(proxy.Rejected):
            proxy.validate_args([("id", "uuid", True)],
                                {"id": "not-a-uuid"})

    def test_pending_reconciliations_limit_is_integer(self):
        # The registry said text although the SQL parameter is integer.
        params = proxy.func_params(
            "forge_coding_reconciler_api.pending_reconciliations",
            proxy.FUNC_CATALOG[
                "forge_coding_reconciler_api.pending_reconciliations"])
        self.assertEqual(params, [("p_limit", "integer", False, True)])

    def test_omitted_defaulted_param_renders_sql_default(self):
        # A parameter with a SQL DEFAULT is optional; omitting it must
        # render the DEFAULT keyword, not NULL (pending_reconciliations
        # rejects NULL; retry_event_observe defaults to empty arrays).
        instance, created = make_proxy()
        reconciler = instance.callers["reconciler"]
        instance.execute(
            reconciler,
            "fn.forge_coding_reconciler_api.pending_reconciliations", {})
        calls = [(sql, params) for sql, params in created[0].statements
                 if "pending_reconciliations" in sql]
        self.assertEqual(len(calls), 1)
        self.assertEqual(
            calls[0][0],
            "SELECT forge_coding_reconciler_api.pending_reconciliations(DEFAULT)")
        self.assertEqual(calls[0][1], [])
        instance.execute(
            reconciler,
            "fn.forge_coding_reconciler_api.pending_reconciliations",
            {"p_limit": 10})
        calls = [(sql, params) for sql, params in created[0].statements
                 if "pending_reconciliations" in sql]
        self.assertEqual(calls[-1][0],
                         "SELECT forge_coding_reconciler_api.pending_reconciliations(%s)")
        self.assertEqual(calls[-1][1], [10])
        coordinator = instance.callers["coordinator"]
        instance.execute(
            coordinator, "fn.forge_agent.retry_event_observe",
            {"p_project": "homelab", "p_scope_kind": "task",
             "p_scope_id": "00000000-0000-0000-0000-000000000001",
             "p_kind": "build", "p_source_id": "src",
             "p_digest": "a" * 64})
        calls = [(sql, params) for sql, params in created[1].statements
                 if "retry_event_observe" in sql]
        self.assertEqual(len(calls), 1)
        self.assertEqual(
            calls[0][0],
            "SELECT forge_agent.retry_event_observe(%s, %s, %s, %s, %s, %s, DEFAULT, DEFAULT, DEFAULT)")

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


class ProxyCompositeTest(unittest.TestCase):
    def test_composite_entry_points_validate(self):
        # C2: the IDE composites, the savepoint-flow composite, and
        # the bulk tool-call update are registered entry points with
        # the parameters the coordinator passes; required parameters
        # are enforced and every one is in the coordinator allowlist.
        run_id = "00000000-0000-0000-0000-000000000001"
        for name, args in (
                ("ide_run_admit", {
                    "conversation_id": run_id, "request_id": "r1",
                    "model_id": "m", "fingerprint": "f",
                    "messages": [{"role": "user", "content": "hi"}],
                    "settings": {"model": "m"}, "model_allowed": True}),
                ("ide_run_claim_next", {}),
                ("ide_tool_started", {
                    "run_id": run_id, "call_id": "c1", "tool_name": "t",
                    "arguments": {}}),
                ("ide_tool_finished", {
                    "run_id": run_id, "call_id": "c1", "tool_name": "t",
                    "result": {"text": "ok"}}),
                ("ide_interrupt_conversation_runs", {}),
                ("ide_tool_calls_mark_outcome_unknown", {"run_id": run_id}),
                ("workflow_retry_allowance_amend", {
                    "project": "homelab", "root": run_id, "policy": {}}),
                ("ide_tool_call_update_by_run", {
                    "run_id": run_id, "status": "outcome_unknown"})):
            self.assertIn(name, proxy.ENTRY_POINTS)
            proxy.validate_args(proxy.ENTRY_POINTS[name][0], args)
            self.assertIn(name,
                          proxy.CALLER_ALLOWLISTS["coordinator"]["op"])
        with self.assertRaises(proxy.Rejected):
            proxy.validate_args(proxy.ENTRY_POINTS["ide_run_admit"][0],
                                {"request_id": "r1"})

    def test_require_revision_reaches_the_server(self):
        # The optimistic-concurrency guard must travel as the
        # require_revision parameter, not as the new revision value.
        instance, created = make_proxy()
        coordinator = instance.callers["coordinator"]
        instance.execute(coordinator, "op.coding_task_update", {
            "owner_id": "mission-control", "project_alias": "homelab",
            "id": "00000000-0000-0000-0000-000000000001",
            "status": "completed", "require_revision": 7})
        calls = [s for s in created[0].statements
                 if "forge_pg_proxy_api.coding_task_update" in s[0]]
        self.assertEqual(len(calls), 1)
        params = proxy.ENTRY_POINTS["coding_task_update"][0]
        index = [i for i, p in enumerate(params)
                 if p[0] == "require_revision"][0]
        self.assertEqual(calls[0][1][index], 7)

    def test_composite_call_is_audited(self):
        instance, created = make_proxy()
        coordinator = instance.callers["coordinator"]
        instance.execute(
            coordinator, "op.ide_tool_calls_mark_outcome_unknown",
            {"run_id": "00000000-0000-0000-0000-000000000001"})
        statements = created[0].statements
        self.assertTrue(any(
            "forge_pg_proxy_api.ide_tool_calls_mark_outcome_unknown" in s[0]
            for s in statements))
        self.assertTrue(any("forge_pg_proxy_api.audit" in s[0]
                            for s in statements))


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

    def test_encode_row_keeps_nested_lists_as_lists(self):
        # Composite results carry nested lists (claim's messages,
        # interrupt's run ids); encode_row used to str() them, so the
        # client received a Python-repr string instead of a list.
        import uuid
        run_id = uuid.UUID("00000000-0000-0000-0000-000000000001")
        self.assertEqual(
            proxy.encode_row({
                "run_id": run_id,
                "messages": [{"role": "user", "content": "hello"}],
                "interrupted": [run_id],
                "settings": {"max_tokens": 8}}),
            {"run_id": "00000000-0000-0000-0000-000000000001",
             "messages": [{"role": "user", "content": "hello"}],
             "interrupted": ["00000000-0000-0000-0000-000000000001"],
             "settings": {"max_tokens": 8}})



class ProxyTokenLayoutTest(unittest.TestCase):
    """Round-6 finding 3: caller processes must be able to traverse
    the proxy config directory and read their own token. Structural
    assertions on setup-runner.sh, the script that creates the
    layout (the live traversal is an install-time property)."""

    @classmethod
    def setUpClass(cls):
        import os
        path = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                            "setup-runner.sh")
        with open(path, encoding="utf-8") as handle:
            cls.script = handle.read()

    def test_config_directories_are_group_traversable(self):
        # 0710 = traverse without listing for group members (the
        # runner and workers join the proxy group for exactly this).
        # The old 0700 config directory blocked every caller.
        self.assertIn(
            'install -d -o "$proxy_user" -g "$proxy_user" -m 0710 '
            '"$proxy_config_directory"', self.script)
        self.assertIn(
            'install -d -o "$proxy_user" -g "$proxy_user" -m 0710 '
            '"$proxy_config_directory/tokens"', self.script)
        self.assertNotIn(
            '-m 0700 "$proxy_config_directory"', self.script)

    def test_token_files_are_group_scoped_per_caller(self):
        self.assertIn("coordinator) echo forge_agent", self.script)
        self.assertIn("worker|reconciler) echo lrocha", self.script)
        # Group and mode are re-applied outside the creation branch,
        # so pre-existing installs heal on the next setup run.
        creation = self.script.index('if [[ ! -s "$token_file" ]]')
        fixup = self.script.index('chown "$proxy_user:$(token_group_for')
        self.assertGreater(fixup, creation)
        self.assertIn("chmod 0640 \"$token_file\"", self.script)


class ProxyMigrationGuardTest(unittest.TestCase):
    """Round-6 finding 1: the deployed runner must not be switched
    to the proxy while coordinator paths still execute statements
    the proxy cannot carry, and the unmigrated set is measured,
    not asserted from memory.

    The measurement: parse each coordinator module, feed every
    literal SQL statement (constants, f-strings, and simple
    name-bound assignments) passed to execute()/executemany() to
    proxy_statements.translate, and collect the methods whose
    statements raise UnsupportedStatement. The result must equal
    UNMIGRATED exactly — a method that migrates drops out and
    fails the test until the method inventory is updated. A module stays in
    proxy-cutover.sh's UNMIGRATED_CALLERS while any of its methods remain;
    a regression that adds an
    untranslatable statement anywhere else fails it too."""

    UNMIGRATED = {
        "work_store.py": {
            "_control_task", "_record_task_identity", "_register_identity",
            "_workflow_projection", "accept",
            "handoff_author", "plan_decide",
            "publication_retry", "repair", "resume", "review_decide",
            "submit"},
        "work_workflow.py": {
            "_admit_reviewer", "_apply_verdict",
            "_artifact", "_cancel_workflow",
            "_code_review_source", "_escalate", "_native_deadline_event",
            "_revive",
            "_verify_peer_recovery_source",
            "defer_source_readiness",
            "process", "retry_native_timeout", "retry_peer_verdict",
            "retry_preparation", "revive_workflow", "validate"},
        "work_owner_control.py": {"resume", "review_decide"},
        "work_owner_reverification.py": {"recover"},
        "queue_store.py": {
            "_lock_owned_run", "_seed_workspace_history",
            "_workspace_history", "_workspace_history_marker",
            "_workspace_tab", "listen", "restore_workspace"},
        "jenkins_store.py": {
            "claim_next", "complete_delivery", "fail_delivery",
            "list_deliveries", "list_events", "listen",
            "renew_lease"},
        "usage_store.py": set(),
        "chat_history_store.py": {
            "_remove", "expire_due", "lock_chat", "open_chat"},
        "mail_store.py": {"archive_manual", "lock_thread",
                          "open_thread"},
    }

    @staticmethod
    def _literal(node, env):
        import ast
        if isinstance(node, ast.Constant) \
                and isinstance(node.value, str):
            return node.value
        if isinstance(node, ast.JoinedStr):
            parts = []
            for value in node.values:
                if isinstance(value, ast.Constant):
                    parts.append(str(value.value))
                else:
                    parts.append("%s")
            return "".join(parts)
        if isinstance(node, ast.Name):
            return env.get(node.id)
        return None

    @classmethod
    def _untranslatable_methods(cls, path):
        import ast
        from proxy_statements import UnsupportedStatement, translate
        with open(path, encoding="utf-8") as handle:
            tree = ast.parse(handle.read())
        found = set()
        for node in ast.walk(tree):
            if not isinstance(node, (ast.FunctionDef,
                                     ast.AsyncFunctionDef)):
                continue
            env = {}
            for child in ast.walk(node):
                if isinstance(child, ast.Assign) \
                        and len(child.targets) == 1 \
                        and isinstance(child.targets[0], ast.Name):
                    value = cls._literal(child.value, env)
                    if value is not None:
                        env[child.targets[0].id] = value
            for child in ast.walk(node):
                if isinstance(child, ast.Call) \
                        and isinstance(child.func, ast.Attribute) \
                        and child.func.attr in ("execute", "executemany") \
                        and child.args:
                    sql = cls._literal(child.args[0], env)
                    if not sql or not sql.strip():
                        continue
                    try:
                        translate(sql, ())
                    except UnsupportedStatement:
                        found.add(node.name)
                    except Exception:
                        # A parameter-shape error means the shape
                        # is translatable; only UnsupportedStatement
                        # marks a statement the proxy cannot carry.
                        pass
        return found

    def test_untranslatable_statements_match_the_unmigrated_list(self):
        import os
        here = os.path.dirname(os.path.abspath(__file__))
        for module, expected in sorted(self.UNMIGRATED.items()):
            with self.subTest(module=module):
                self.assertEqual(
                    self._untranslatable_methods(
                        os.path.join(here, module)),
                    expected)

    def test_cutover_script_lists_the_same_modules(self):
        import os
        here = os.path.dirname(os.path.abspath(__file__))
        with open(os.path.join(here, os.pardir, "proxy-cutover.sh"),
                  encoding="utf-8") as handle:
            script = handle.read()
        start = script.index("UNMIGRATED_CALLERS=(")
        block = script[start:script.index("\n)", start)]
        for module, methods in sorted(self.UNMIGRATED.items()):
            if methods:
                self.assertIn(module, block)

    def test_deployed_runner_unit_does_not_select_the_proxy(self):
        # The runner stays on direct Postgres until the unmigrated
        # paths above are gone: queue_store.connect() would select
        # the proxy for every coordinator store if the unit set
        # FORGE_PG_PROXY_URL, and their FOR UPDATE flows would fail
        # at runtime.
        import os
        here = os.path.dirname(os.path.abspath(__file__))
        with open(os.path.join(here, "forge-agent-runner.service"),
                  encoding="utf-8") as handle:
            unit = handle.read()
        for line in unit.splitlines():
            self.assertFalse(
                line.startswith("Environment=FORGE_PG_PROXY"),
                line)


if __name__ == "__main__":
    unittest.main()
