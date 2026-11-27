"""Fix 2, plan section 8: every emitted event carries its exact actor (and cause).

Attribution is asserted by EQUALITY, not existence: a test that only checks
an actor is present would accept the round-8 misattribution (service events
carrying the agent profile and vice versa). Each test below names the exact
``{'kind': ..., 'id': ...}`` dict the event must carry, and the reconcile
tests name the exact entry point, reason and subject too.

Coverage map (live emission sites -> covering test):
- reconcile ``resumed`` (expired lease) ............ test_reconcile_expired_lease_*
- reconcile ``failed`` (passed deadline) ........... test_reconcile_passed_deadline_*
- reconcile ``blocked_permission`` (disabled grant)  test_reconcile_disabled_grant_*
- reconcile ``cancelled`` (with / without receipt) . test_reconcile_cancelled_task_*
- claim-triggered ``resumed`` ...................... test_claim_after_lease_expiry_*
- claim returning NULL ............................. test_claim_after_cancellation_*
- fail-closed housekeeping ......................... test_housekeeping_as_postgres_*
- agent path claim/progress/finish/release ......... test_agent_path_*_carry_agent_actor
- cancel paths (single-task / workflow-required) ... test_cancel_single_task_* /
                                                     test_workflow_cancel_*
- worker ``record_stop`` lease_lost ................ test_record_stop_lease_lost_*

Migration 0036's fourteen sites, the 0020 ``_event`` funnel, M41's
reservation-bound branch publication and M47's worker deadline extension are
live. Older definitions remain pinned as history by the inventory test below.
M41 validates the active native-workflow branch reservation before emitting
its service actor. M47 attribution is covered by test_deadline_extension_events.
"""
from contextlib import closing
from dataclasses import replace
import hashlib
import json
import re
import unittest
from pathlib import Path
from types import SimpleNamespace
from uuid import UUID, uuid4

import psycopg2

from test_work_execution_and_usage import (
    BASE_REV, make_stop_proof, make_submit, make_v2_config,
)
from test_work_external import ALIAS
from work_api import WorkCancel, WorkEvents, WorkGet, WorkServiceFactory
from work_config import WorkProfile, WorkflowPolicy
from work_external_policy import MODEL, POLICY_DIGEST, POLICY_ID
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE, SQL_DIR
from work_policy import POLICY_DIGEST as NATIVE_POLICY_DIGEST
from work_policy import POLICY_ID as NATIVE_POLICY_ID
from work_worker_store import WorkerDB
import work_policy


OWNER = "test-owner"
OPERATOR = {"kind": "operator", "id": OWNER}
SERVICE = {"kind": "service", "id": "forge-external-reconciler"}
AGENT = {"kind": "agent", "id": ALIAS}
WORKER = {"kind": "worker", "id": "workflow-worker"}
REASON = "external_lease_reconciled"


def external_config(owner=OWNER):
    """The muse-db-primary profile plus the homelab native workflow policy."""
    value = make_v2_config(owner=owner)
    return replace(value, profiles={**value.profiles,
        ALIAS: WorkProfile(ALIAS, "muse", MODEL, POLICY_ID, execution_enabled=True),
        "muse-primary": WorkProfile("muse-primary", "muse", "native-model", NATIVE_POLICY_ID),
    }, project_workflows={"homelab": {"author_profile": "agy-v2", "reviewer_profile": "codex-v2"}})


@unittest.skipUnless(PG16_AVAILABLE, "Requires non-root disposable PostgreSQL 16")
class EventAttributionPostgresTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)
        cls.cluster.admin_exec("CREATE ROLE muse_events LOGIN NOINHERIT", owner=False)
        for script in ("grant-muse-read-access.sql", "grant-external-agent.sql"):
            result = cls.cluster.run_sql_script(SQL_DIR / "management" / script)
            if result.returncode:
                raise AssertionError(result.stdout + result.stderr)

    def setUp(self):
        self.cluster.reset_coding_tables()
        self.cluster.admin_exec("TRUNCATE forge_external_agent.scope_grants CASCADE", owner=False)
        self.cluster.admin_exec(
            "INSERT INTO forge_external_agent.scope_grants(external_role,service_role,owner_id,project_alias,profile_alias,enabled) "
            "VALUES('muse_events','forge_agent','test-owner','homelab','muse-db-primary',true)"
        )
        self.cfg = external_config()
        self.service = WorkServiceFactory(self.cfg, lambda: self.cluster.connect("forge_agent")).create()
        self.serial = 0

    def rows(self, statement, parameters=()):
        with closing(self.cluster.connect("postgres")) as connection, connection.cursor() as cursor:
            cursor.execute(statement, parameters)
            return cursor.fetchall()

    def external(self, function, parameters=()):
        # Names below are fixed fixture operations, never external task data.
        placeholders = ", ".join(["%s"] * len(parameters))
        with closing(self.cluster.connect("muse_events")) as connection:
            with connection, connection.cursor() as cursor:
                cursor.execute(
                    "SELECT forge_external_agent_api.%s(%s)" % (function, placeholders),
                    parameters)
                # The API functions return a single jsonb column (or NULL).
                return cursor.fetchone()[0]

    def reconcile(self):
        with closing(self.cluster.connect("forge_agent")) as connection:
            with connection, connection.cursor() as cursor:
                cursor.execute("SELECT forge_external_agent_api.reconcile('test-owner')")

    def submit(self):
        self.serial += 1
        return self.service.submit(make_submit(
            agent_profile=ALIAS, mode="plan", idempotency_key=f"attrib-{self.serial}"))

    def claim(self):
        # Readiness is time-bound; advertise immediately before claiming.
        self.external("advertise", ("homelab", ALIAS, "muse"))
        value = self.external("claim", ("homelab", ALIAS))
        self.assertIsNotNone(value)
        return value

    def events(self, task_id):
        return self.rows(
            "SELECT event_kind, payload FROM forge_agent.coding_events "
            "WHERE task_id=%s ORDER BY sequence", (UUID(task_id),))

    def event_count(self, task_id):
        return len(self.events(task_id))

    def assignment_id_for(self, task_id):
        row = self.rows(
            "SELECT assignment_id FROM forge_external_agent.assignments WHERE task_id=%s",
            (UUID(task_id),))
        self.assertEqual(len(row), 1)
        return str(row[0][0])

    def assignment(self, assignment_id):
        row = self.rows(
            "SELECT status, lease_until FROM forge_external_agent.assignments "
            "WHERE assignment_id=%s", (UUID(assignment_id),))
        self.assertEqual(len(row), 1)
        return row[0]

    def expire_lease(self, assignment_id):
        self.cluster.admin_exec(
            "UPDATE forge_external_agent.assignments SET lease_until = clock_timestamp() - interval '1 minute' "
            "WHERE assignment_id=%s" % ("'" + str(assignment_id) + "'"), owner=False)

    def pass_deadline(self, assignment_id):
        # deadline_at is part of the immutable assignment binding, so the
        # binding trigger must be briefly disabled to stage an expired
        # deadline. The housekeeping logic under test is unaffected.
        self.cluster.admin_exec(
            "ALTER TABLE forge_external_agent.assignments DISABLE TRIGGER external_assignment_binding; "
            "UPDATE forge_external_agent.assignments SET deadline_at = clock_timestamp() - interval '1 minute' "
            "WHERE assignment_id='%s'; "
            "ALTER TABLE forge_external_agent.assignments ENABLE TRIGGER external_assignment_binding"
            % assignment_id, owner=False)

    def cancel_via_service(self, task_id):
        detail = self.service.get(WorkGet("homelab", UUID(task_id)))
        self.serial += 1
        return self.service.cancel(WorkCancel(
            "homelab", UUID(task_id), detail["revision"], f"cancel-attrib-{self.serial}"))

    def assert_housekeeping_event(self, payload, *, kind, actor, entry_point, cause=None, originator=None):
        self.assertEqual(payload["actor"], actor)
        self.assertEqual(payload["entry_point"], entry_point)
        self.assertEqual(payload["reason"], REASON)
        self.assertEqual(payload["subject"]["profile"], ALIAS)
        self.assertEqual(payload["subject"]["external_role"], "muse_events")
        if cause is None:
            self.assertNotIn("cause", payload)
        else:
            self.assertEqual(payload["cause"], cause)
        if originator is None:
            self.assertNotIn("originator", payload)
        else:
            self.assertEqual(payload["originator"], originator)

    def test_reconcile_expired_lease_emits_resumed_with_service_actor(self):
        receipt = self.submit()
        leased = self.claim()
        self.assertEqual(leased["task_id"], receipt["task_id"])
        self.expire_lease(leased["assignment_id"])
        before = self.event_count(receipt["task_id"])
        self.reconcile()
        after = self.events(receipt["task_id"])
        self.assertEqual(len(after), before + 1)
        kind, payload = after[-1]
        self.assertEqual(kind, "resumed")
        self.assert_housekeeping_event(payload, kind=kind, actor=SERVICE, entry_point="reconcile")
        self.assertEqual(payload["subject"]["assignment_id"], leased["assignment_id"])
        self.assertEqual(self.assignment(leased["assignment_id"])[0], "queued")
        self.assertIsNone(self.assignment(leased["assignment_id"])[1])

    def test_reconcile_passed_deadline_emits_failed_with_service_actor(self):
        receipt = self.submit()
        leased = self.claim()
        self.pass_deadline(leased["assignment_id"])
        before = self.event_count(receipt["task_id"])
        self.reconcile()
        after = self.events(receipt["task_id"])
        self.assertEqual(len(after), before + 1)
        kind, payload = after[-1]
        self.assertEqual(kind, "failed")
        self.assert_housekeeping_event(payload, kind=kind, actor=SERVICE, entry_point="reconcile")
        self.assertEqual(payload["subject"]["assignment_id"], leased["assignment_id"])
        self.assertEqual(self.assignment(leased["assignment_id"])[0], "failed")

    def test_reconcile_disabled_grant_emits_blocked_permission_with_service_actor(self):
        receipt = self.submit()
        self.cluster.admin_exec(
            "UPDATE forge_external_agent.scope_grants SET enabled=false WHERE profile_alias='muse-db-primary'",
            owner=False)
        before = self.event_count(receipt["task_id"])
        self.reconcile()
        after = self.events(receipt["task_id"])
        self.assertEqual(len(after), before + 1)
        kind, payload = after[-1]
        self.assertEqual(kind, "blocked_permission")
        self.assert_housekeeping_event(payload, kind=kind, actor=SERVICE, entry_point="reconcile")
        self.assertEqual(self.assignment(self.assignment_id_for(receipt["task_id"]))[0], "failed")

    def test_reconcile_cancelled_task_with_receipt_carries_cause_and_originator(self):
        receipt = self.submit()
        cancel_receipt = self.cancel_via_service(receipt["task_id"])
        self.assertEqual(cancel_receipt["actor"], OPERATOR)
        before = self.event_count(receipt["task_id"])
        self.reconcile()
        after = self.events(receipt["task_id"])
        self.assertEqual(len(after), before + 1)
        kind, payload = after[-1]
        self.assertEqual(kind, "cancelled")
        self.assert_housekeeping_event(
            payload, kind=kind, actor=SERVICE, entry_point="reconcile",
            cause="operator_request", originator=OPERATOR)
        self.assertEqual(self.assignment(self.assignment_id_for(receipt["task_id"]))[0], "cancelled")

    def test_reconcile_cancelled_task_without_receipt_defaults_cause_without_originator(self):
        receipt = self.submit()
        self.cluster.admin_exec(
            "UPDATE forge_agent.coding_tasks SET status='cancelled', cancelled_at=clock_timestamp(), "
            "revision=revision+1 WHERE id='%s'" % receipt["task_id"], owner=False)
        before = self.event_count(receipt["task_id"])
        self.reconcile()
        after = self.events(receipt["task_id"])
        self.assertEqual(len(after), before + 1)
        kind, payload = after[-1]
        self.assertEqual(kind, "cancelled")
        # No control receipt exists, so the default cause applies and there
        # is no originator to record.
        self.assert_housekeeping_event(
            payload, kind=kind, actor=SERVICE, entry_point="reconcile",
            cause="operator_request", originator=None)
        self.assertEqual(self.assignment(self.assignment_id_for(receipt["task_id"]))[0], "cancelled")

    def test_claim_after_lease_expiry_emits_resumed_with_agent_actor(self):
        receipt = self.submit()
        first = self.claim()
        self.expire_lease(first["assignment_id"])
        before = self.event_count(receipt["task_id"])
        second = self.claim()
        self.assertEqual(second["assignment_id"], first["assignment_id"])
        after = self.events(receipt["task_id"])[before:]
        self.assertEqual([kind for kind, _ in after], ["resumed", "started"])
        resumed = after[0][1]
        self.assert_housekeeping_event(resumed, kind="resumed", actor=AGENT, entry_point="claim")
        self.assertEqual(resumed["subject"]["assignment_id"], first["assignment_id"])
        self.assertEqual(after[1][1]["actor"], AGENT)

    def test_claim_after_cancellation_returns_null_with_only_housekeeping_event(self):
        receipt = self.submit()
        self.claim()
        self.cancel_via_service(receipt["task_id"])
        before = self.event_count(receipt["task_id"])
        value = self.external("claim", ("homelab", ALIAS))
        self.assertIsNone(value)
        after = self.events(receipt["task_id"])[before:]
        self.assertEqual(len(after), 1)
        kind, payload = after[0]
        self.assertEqual(kind, "cancelled")
        self.assert_housekeeping_event(
            payload, kind=kind, actor=AGENT, entry_point="claim",
            cause="operator_request", originator=OPERATOR)

    def test_housekeeping_as_postgres_fails_closed_with_no_event(self):
        receipt = self.submit()
        leased = self.claim()
        self.expire_lease(leased["assignment_id"])
        before = self.event_count(receipt["task_id"])
        with closing(self.cluster.connect("postgres")) as connection:
            with connection.cursor() as cursor:
                with self.assertRaises(psycopg2.Error) as caught:
                    cursor.execute(
                        "SELECT forge_external_agent._housekeep(g, 64) "
                        "FROM forge_external_agent.scope_grants g "
                        "WHERE g.profile_alias='muse-db-primary'")
                self.assertIn("no attributable entry point", str(caught.exception))
            connection.rollback()
        self.assertEqual(self.event_count(receipt["task_id"]), before)
        # The row is untouched, so the real entry point still reconciles it.
        self.reconcile()
        kind, payload = self.events(receipt["task_id"])[-1]
        self.assertEqual(kind, "resumed")
        self.assertEqual(payload["actor"], SERVICE)
        self.assertEqual(payload["entry_point"], "reconcile")

    def test_agent_path_claim_progress_finish_release_carry_agent_actor(self):
        receipt = self.submit()
        leased = self.claim()
        before = self.event_count(receipt["task_id"])
        kind, payload = self.events(receipt["task_id"])[before - 1]
        self.assertEqual(kind, "started")
        self.assertEqual(payload["actor"], AGENT)

        self.external("progress", (leased["assignment_id"], leased["generation"], leased["token"],
                                   "Checking the plan."))
        kind, payload = self.events(receipt["task_id"])[-1]
        self.assertEqual(kind, "checkpoint")
        self.assertEqual(payload["actor"], AGENT)

        content = "# External plan\n\nReview this exact artifact."
        self.external("finish", (leased["assignment_id"], leased["generation"], leased["token"],
                                 content, "Plan delivered."))
        kind, payload = self.events(receipt["task_id"])[-1]
        self.assertEqual(kind, "awaiting_review")
        self.assertEqual(payload["actor"], AGENT)

        second = self.submit()
        leased2 = self.claim()
        self.assertEqual(leased2["task_id"], second["task_id"])
        self.external("release", (leased2["assignment_id"], leased2["generation"], leased2["token"]))
        kind, payload = self.events(second["task_id"])[-1]
        self.assertEqual(kind, "resumed")
        self.assertEqual(payload["actor"], AGENT)
        self.assertEqual(self.assignment(leased2["assignment_id"])[0], "queued")

    def test_cancel_single_task_event_carries_operator_actor_and_cause(self):
        receipt = self.submit()
        cancel_receipt = self.cancel_via_service(receipt["task_id"])
        self.assertEqual(cancel_receipt["actor"], OPERATOR)
        kind, payload = self.events(receipt["task_id"])[-1]
        self.assertEqual(kind, "cancelled")
        self.assertEqual(payload["actor"], OPERATOR)
        self.assertEqual(payload["cause"], "operator_request")


@unittest.skipUnless(PG16_AVAILABLE, "Requires non-root disposable PostgreSQL 16")
class NativeAttributionPostgresTest(unittest.TestCase):
    """Workflow-required cancel and worker record_stop attribution."""

    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def setUp(self):
        self.cluster.reset_retry_events()
        self.cluster.admin_exec(
            "TRUNCATE forge_agent.coding_tasks,forge_agent.coding_worker_grants,"
            "forge_agent.coding_worker_readiness CASCADE", owner=False)
        self.config = replace(
            make_v2_config(),
            project_workflows={"homelab": WorkflowPolicy("agy-v2", "codex-v2", max_plan_revisions=3)})
        self.config = replace(self.config, repair_policy={
            **dict(self.config.repair_policy),
            "max_rounds": 3, "max_stagnant_rounds": 2, "max_infrastructure_retries": 1})
        self.service = WorkServiceFactory(
            self.config, lambda: self.cluster.connect("forge_agent"),
            retry_event_factory=lambda _: SimpleNamespace(create=lambda: None)).create()
        self.coordinator = self.service.workflow_worker
        self.db = WorkerDB(
            lambda: self.cluster.connect("forge_coding_worker"), "mission-control",
            reconcile_connect_factory=lambda: self.cluster.connect("forge_coding_reconciler"))
        self.incarnation = str(uuid4())
        for profile in ("agy-v2", "codex-v2"):
            self.cluster.grant_worker("mission-control", "homelab", profile)
        self.db.advertise(
            "workflow-worker", self.incarnation,
            [{"alias": "homelab", "base_revision": BASE_REV}],
            [{"alias": p.alias, "backend": p.backend, "model": p.model,
              "modes": ["plan", "review", "implement"]}
             for p in self.config.profiles.values() if p.alias in {"agy-v2", "codex-v2"}],
            work_policy.POLICY_DIGEST)

    def submit(self, key=None):
        policy = self.config.project_workflows["homelab"].summary(
            self.config.catalog(), self.config.repair_policy)
        command = replace(
            make_submit(agent_profile="agy-v2", idempotency_key=key or str(uuid4())),
            workflow_policy_digest=policy["policy_digest"])
        return command, self.service.submit(command)

    def claim(self, profile):
        return self.db.claim("workflow-worker", self.incarnation, [profile])

    def publish(self, lease, text):
        self.db.append_result(lease, 0, text)
        proof = make_stop_proof(lease.unit_name)
        self.db.publish(lease, "awaiting_review", {
            "sha256": hashlib.sha256(text.encode()).hexdigest(),
            "byte_count": len(text.encode()), "chunk_count": 1}, None, "Done", proof)
        return self.db.reconcile(lease, "b" * 64, proof)

    def get(self, task):
        return self.service.get(WorkGet("homelab", UUID(task)))

    def events(self, task_id):
        with closing(self.cluster.connect("postgres")) as connection, connection.cursor() as cursor:
            cursor.execute(
                "SELECT event_kind, payload FROM forge_agent.coding_events "
                "WHERE task_id=%s ORDER BY sequence", (UUID(task_id),))
            return cursor.fetchall()

    def test_workflow_cancel_emits_event_per_affected_task_with_actor_and_cause(self):
        _, admitted = self.submit()
        author = self.claim("agy-v2")
        self.assertIsNotNone(author)
        self.publish(author, "Plan.")
        self.assertTrue(self.coordinator.run_once())
        reviewer_id = self.rows_workflow_tasks(admitted["task_id"], author.task_id)
        self.assertEqual(len(reviewer_id), 1)
        detail = self.get(author.task_id)
        receipt = self.service.cancel(WorkCancel(
            "homelab", UUID(author.task_id), detail["revision"], "cancel-workflow-attrib"))
        operator = {"kind": "operator", "id": "mission-control"}
        self.assertEqual(receipt["actor"], operator)
        kind, payload = self.events(author.task_id)[-1]
        self.assertEqual(kind, "cancelled")
        self.assertEqual(payload["actor"], operator)
        self.assertEqual(payload["cause"], "operator_request")
        kind, payload = self.events(reviewer_id[0])[-1]
        self.assertEqual(kind, "cancelled")
        self.assertEqual(payload["actor"], operator)
        # The sibling is cancelled because the workflow was, not by the caller.
        self.assertEqual(payload["cause"], "workflow_cancelled")

    def rows_workflow_tasks(self, root_task_id, author_task_id):
        with closing(self.cluster.connect("postgres")) as connection, connection.cursor() as cursor:
            cursor.execute(
                "SELECT id FROM forge_agent.coding_tasks "
                "WHERE workflow_id=(SELECT workflow_id FROM forge_agent.coding_tasks WHERE id=%s) "
                "AND id<>%s ORDER BY id", (UUID(root_task_id), UUID(author_task_id)))
            return [str(row[0]) for row in cursor.fetchall()]

    def test_record_stop_lease_lost_event_carries_worker_actor_and_cause(self):
        _, admitted = self.submit()
        lease = self.claim("agy-v2")
        self.assertIsNotNone(lease)
        self.cluster.admin_exec(
            "UPDATE forge_agent.coding_attempts SET lease_expires_at = clock_timestamp() - interval '1 minute' "
            "WHERE attempt_id='%s'" % lease.attempt_id, owner=False)
        before = len(self.events(lease.task_id))
        self.db.record_stop(
            lease, make_stop_proof(lease.unit_name), "Worker lost its lease", "lease_lost")
        self.assertEqual(self.get(lease.task_id)["status"], "needs_reconcile")
        after = self.events(lease.task_id)
        self.assertEqual(len(after), before + 1)
        kind, payload = after[-1]
        self.assertEqual(kind, "needs_reconcile")
        self.assertEqual(payload["actor"], WORKER)
        self.assertEqual(payload["cause"], "lease_lost")
        self.assertEqual(payload["reason"], "stopped_without_publication")
        # The public events API surfaces the kind; the payload is asserted
        # against the stored row above.
        public = self.service.events(WorkEvents("homelab", UUID(lease.task_id), 0, 50))
        self.assertIn("needs_reconcile", [item["kind"] for item in public["events"]])
        stop = next(item for item in public['events'] if item['kind'] == 'needs_reconcile')
        self.assertEqual(stop['attribution']['actor'], WORKER)
        self.assertEqual(stop['attribution']['cause'], 'lease_lost')
        self.assertNotIn('payload', stop)



# Checked-in inventory of every INSERT into forge_agent.coding_events in the
# migration history, keyed by (file, qualified function) -> count. Only
# 0036's fourteen sites, the 0020 _event funnel, and M41/M47 producers are
# live; the rest are retained historical definitions. Any new emission
# site must land here with its covering test, or this test fails.
EXPECTED_SQL_EVENT_INSERTS = {
    ("0010_coding_execution_and_usage.sql", "forge_coding_worker_api.claim"): 1,
    ("0010_coding_execution_and_usage.sql", "forge_coding_worker_api.checkpoint"): 1,
    ("0010_coding_execution_and_usage.sql", "forge_coding_worker_api.publish"): 1,
    ("0010_coding_execution_and_usage.sql", "forge_coding_worker_api.record_stop"): 1,
    ("0010_coding_execution_and_usage.sql", "forge_coding_reconciler_api.expire_leases"): 1,
    ("0011_coding_elevated_tool_policy.sql", "forge_coding_worker_api.claim"): 1,
    ("0012_coding_progress.sql", "forge_coding_worker_api.report_progress"): 1,
    ("0013_coding_task_session_policy.sql", "forge_coding_worker_api.claim"): 1,
    ("0014_coding_plan_workflow.sql", "forge_coding_worker_api.claim"): 1,
    ("0014_coding_plan_workflow.sql", "forge_coding_reconciler_api.publication_finish"): 1,
    ("0015_coding_feedback_loop.sql", "forge_coding_worker_api.claim"): 1,
    ("0015_coding_feedback_loop.sql", "forge_agent.coding_schedule_repair"): 2,
    ("0015_coding_feedback_loop.sql", "forge_coding_reconciler_api.review_finish"): 1,
    ("0015_coding_feedback_loop.sql", "forge_coding_reconciler_api.publication_finish"): 1,
    ("0016_coding_autonomous_workflows.sql", "forge_coding_worker_api.claim"): 1,
    ("0017_coding_codex_warm_sessions.sql", "forge_coding_worker_api.claim"): 1,
    ("0020_external_agent_transport.sql", "forge_external_agent._event"): 1,
    ("0025_native_delivery_repair.sql", "forge_coding_reconciler_api.native_merge_failure"): 1,
    ("0025_native_delivery_repair.sql", "forge_coding_reconciler_api.native_branch_first_replay"): 1,
    ("0025_native_delivery_repair.sql", "forge_coding_reconciler_api.native_branch_published"): 1,
    ("0030_agent_wide_retry_policy.sql", "forge_agent.amend_agent_retry_policy"): 1,
    ("0036_event_stream_actor_cause.sql", "forge_coding_worker_api.claim"): 1,
    ("0036_event_stream_actor_cause.sql", "forge_coding_worker_api.checkpoint"): 1,
    ("0036_event_stream_actor_cause.sql", "forge_coding_worker_api.publish"): 1,
    ("0036_event_stream_actor_cause.sql", "forge_coding_worker_api.record_stop"): 1,
    ("0036_event_stream_actor_cause.sql", "forge_coding_worker_api.report_progress"): 1,
    ("0036_event_stream_actor_cause.sql", "forge_coding_reconciler_api.expire_leases"): 1,
    ("0036_event_stream_actor_cause.sql", "forge_coding_reconciler_api.publication_finish"): 1,
    ("0036_event_stream_actor_cause.sql", "forge_coding_reconciler_api.review_finish"): 1,
    ("0036_event_stream_actor_cause.sql", "forge_coding_reconciler_api.native_branch_first_replay"): 1,
    ("0036_event_stream_actor_cause.sql", "forge_coding_reconciler_api.native_branch_published"): 1,
    ("0036_event_stream_actor_cause.sql", "forge_coding_reconciler_api.native_merge_failure"): 1,
    ("0036_event_stream_actor_cause.sql", "forge_agent.coding_schedule_repair"): 2,
    ("0036_event_stream_actor_cause.sql", "forge_agent.amend_agent_retry_policy"): 1,
    ("0041_agent_branch_and_cicd_stages.sql", "forge_coding_reconciler_api.native_agent_branch_published"): 1,
    ("0047_deadline_extension_events.sql", "forge_coding_worker_api.renew_deadline"): 1,
    ("0052_pg_proxy_preparation.sql", "forge_pg_proxy_api.coding_event_insert"): 1,
    ("0052_pg_proxy_preparation.sql", "forge_pg_proxy_api.coding_workflow_cancel"): 4,
    ("0052_pg_proxy_preparation.sql", "forge_pg_proxy_api.ext_workflow_cancel"): 3,
}


class EventAttributionCoverageTest(unittest.TestCase):
    """Source-scan coverage: no emission site outside the sanctioned ones."""

    def test_python_event_inserts_live_only_in_work_events(self):
        runner = Path(__file__).resolve().parent
        hits = {}
        for path in sorted(runner.glob("*.py")):
            if path.name.startswith("test_"):
                continue
            count = len(re.findall(
                r"INSERT INTO forge_agent\.coding_events\b", path.read_text()))
            if count:
                hits[path.name] = count
        # Both INSERTs are the two statements inside work_events.insert_event,
        # the single sanctioned Python inserter, plus the one checkpoint
        # INSERT in work_superseding_review.correct (main's 0033
        # superseding-review path, which records the queued correction
        # directly).
        self.assertEqual(hits, {"work_events.py": 2, "work_superseding_review.py": 1})

    def test_sql_event_insert_inventory_matches_checked_in_set(self):
        migrations = SQL_DIR / "migrations"
        actual = {}
        for path in sorted(migrations.glob("*.sql")):
            text = path.read_text()
            funcs = list(re.finditer(
                r"CREATE (?:OR REPLACE )?FUNCTION\s+([\w.]+)\(", text))
            for index, match in enumerate(funcs):
                end = funcs[index + 1].start() if index + 1 < len(funcs) else len(text)
                count = len(re.findall(
                    r"INSERT INTO forge_agent\.coding_events\b", text[match.start():end]))
                if count:
                    actual[(path.name, match.group(1))] = count
        self.assertEqual(actual, EXPECTED_SQL_EVENT_INSERTS)
        self.assertEqual(sum(actual.values()), 46)


if __name__ == "__main__":
    unittest.main()
