"""Preparation recovery reuses stopped linked results through scoped PG ports."""

from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
import hashlib
from threading import Barrier
import unittest
from uuid import UUID, uuid4

from queue_store import Rejected
from work_api import WorkCancel
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE
import test_work_workflow as workflow_fixtures


@unittest.skipUnless(PG16_AVAILABLE, "Requires owned PostgreSQL16")
class PreparationRetryTest(unittest.TestCase):
    setUp = workflow_fixtures.AutonomousWorkflowTest.setUp
    submit = workflow_fixtures.AutonomousWorkflowTest.submit
    claim = workflow_fixtures.AutonomousWorkflowTest.claim
    publish = workflow_fixtures.AutonomousWorkflowTest.publish
    get = workflow_fixtures.AutonomousWorkflowTest.get
    verdict = workflow_fixtures.AutonomousWorkflowTest.verdict
    finish_plan_round = workflow_fixtures.AutonomousWorkflowTest.finish_plan_round
    checked_implementation = workflow_fixtures.AutonomousWorkflowTest.checked_implementation

    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def rows(self, statement, arguments=()):
        with closing(self.cluster.connect("postgres")) as connection:
            with connection.cursor() as cursor:
                cursor.execute(statement, arguments)
                return cursor.fetchall()

    @staticmethod
    def failed_outcome(job):
        return {"disposition": "handoff", "reason": "publication_command_failed",
            "candidate_commit": None, "gate_manifest_sha256": "e" * 64,
            "findings": [], "verified_finding_ids": [], "fingerprint": None,
            "summary": "Independent review stopped before approval.", "verification": {
                "checks_passed": False, "candidate_workspace_digest": job["workspace_digest"],
                "repair_requires_new_workspace": False, "toolchain": "Not reported",
                "integration_base": job["base_revision"], "cleanup_confirmed": True, "checks": []}}

    def preparation_failure(self):
        _admitted, author, critic, source = self.checked_implementation(linked=True)
        self.publish(critic, self.verdict(critic, "revise"))
        self.assertTrue(self.coordinator.run_once())
        child = self.claim("agy-v2")
        self.assertIsNotNone(child)
        self.assertEqual(child.parent_task_id, author.task_id)
        plan = self.db.read_plan(child)
        raw = "Repair complete. Preserve provider ownership and the approved Plan."
        rec = self.publish(child, raw)
        self.db.queue_review(child, rec)
        self.coordinator.run_once()
        job = self.db.review_claim("checks")
        self.assertIsNotNone(job)
        self.db.review_finish(job, self.failed_outcome(job))
        # The original coordinator records the failure before the next scan.
        self.coordinator.run_once()
        self.assertEqual(self.get(child.task_id)["workflow"]["escalation_reason"], "publication_command_failed")
        request = dict(project_alias="homelab", task_id=UUID(child.task_id),
            expected_revision=self.get(child.task_id)["revision"], review_id=UUID(job["review_id"]),
            reconciliation_receipt_id=UUID(rec), idempotency_key="preparation-recovery")
        return author, child, plan, source, job, raw, request

    def counts(self, task):
        return self.rows("SELECT (SELECT count(*) FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t "
            "ON(t.owner_id,t.project_alias,t.id)=(a.owner_id,a.project_alias,a.task_id) WHERE t.workflow_id=w.workflow_id),"
            "w.plan_revisions,b.repair_rounds FROM forge_agent.coding_workflows w JOIN forge_agent.coding_tasks t "
            "ON t.workflow_id=w.workflow_id LEFT JOIN forge_agent.coding_review_budget b "
            "ON(b.owner_id,b.project_alias,b.root_task_id)=(w.owner_id,w.project_alias,w.root_task_id) "
            "WHERE t.id=%s", (task.task_id,))[0]

    def assert_retry_job(self, child, old, raw, before):
        job = self.db.review_claim("checks")
        self.assertIsNotNone(job)
        self.assertEqual(job["task_id"], child.task_id)
        self.assertEqual(job["attempt_id"], child.attempt_id)
        self.assertEqual(job["parent_result_sha256"], hashlib.sha256(raw.encode()).hexdigest())
        self.assertEqual(job["workspace_digest"], old["workspace_digest"])
        self.assertEqual(job["base_revision"], child.base_revision)
        self.assertEqual(job["source_review_id"], old["review_id"])
        self.assertIsNone(job["candidate_commit"])
        self.assertEqual(job["infrastructure_retries"], 1)
        self.assertEqual(job["scope_plan_decision_id"], child.scope_plan_decision_id)
        self.assertEqual(self.counts(child), before)
        self.assertEqual(before[2], 1)
        self.assertIsNone(self.claim("agy-v2"))
        return job

    def test_scanner_appends_verifier_retry_without_reinvoking_author_or_changing_scope(self):
        author, child, plan, _source, old, raw, _request = self.preparation_failure()
        before = self.counts(child)
        immutable = self.rows("SELECT finish_receipt FROM forge_agent.coding_reviews WHERE review_id=%s", (old["review_id"],))[0][0]
        self.coordinator.run_once()
        job = self.assert_retry_job(child, old, raw, before)
        self.assertEqual(self.rows("SELECT finish_receipt FROM forge_agent.coding_reviews WHERE review_id=%s", (old["review_id"],))[0][0], immutable)
        self.assertEqual(plan["decision_id"], child.scope_plan_decision_id)
        self.assertEqual(child.scope_plan_decision_id, author.plan_decision_id)
        self.assertEqual(self.rows("SELECT scope_plan_decision_id FROM forge_agent.coding_reviews WHERE review_id=%s", (job["review_id"],))[0][0], UUID(plan["decision_id"]))

    def test_concurrent_retry_is_idempotent_and_preserves_original_finish_receipt(self):
        _author, child, _plan, _source, old, raw, request = self.preparation_failure()
        before = self.counts(child)
        immutable = self.rows("SELECT finish_receipt FROM forge_agent.coding_reviews WHERE review_id=%s", (old["review_id"],))[0][0]
        barrier = Barrier(2)

        def retry():
            barrier.wait(timeout=5)
            return self.coordinator.store.retry_preparation(**request)

        with ThreadPoolExecutor(max_workers=2) as executor:
            left, right = executor.submit(retry), executor.submit(retry)
            receipt = left.result(timeout=10)
            self.assertEqual(right.result(timeout=10), receipt)
        self.assertEqual(self.coordinator.store.retry_preparation(**request), receipt)
        self.assert_retry_job(child, old, raw, before)
        self.assertEqual(self.rows("SELECT finish_receipt FROM forge_agent.coding_reviews WHERE review_id=%s", (old["review_id"],))[0][0], immutable)
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_reviews WHERE previous_review_id=%s", (old["review_id"],))[0][0], 1)

    def test_stale_unknown_and_post_check_failures_refuse_preparation_replay(self):
        for case in ("revision", "receipt", "reason", "candidate", "checks", "cleanup", "cancelled", "grant"):
            with self.subTest(case=case):
                self.setUp()
                _author, child, _plan, _source, old, _raw, request = self.preparation_failure()
                before = self.counts(child)
                if case == "revision":
                    request["expected_revision"] -= 1
                elif case == "receipt":
                    request["reconciliation_receipt_id"] = uuid4()
                elif case == "reason":
                    self.cluster.admin_exec("UPDATE forge_agent.coding_reviews SET reason='unknown_io_error' WHERE review_id='" + old["review_id"] + "'", owner=False)
                elif case == "candidate":
                    self.cluster.admin_exec("UPDATE forge_agent.coding_reviews SET candidate_commit=repeat('d',40) WHERE review_id='" + old["review_id"] + "'", owner=False)
                elif case == "checks":
                    self.cluster.admin_exec("UPDATE forge_agent.coding_reviews SET verification=jsonb_set(verification,'{checks}',"
                        "'[{\"check_id\":\"make-ci\",\"status\":\"failed\",\"head_sha\":\"dddddddddddddddddddddddddddddddddddddddd\"}]'::jsonb) "
                        "WHERE review_id='" + old["review_id"] + "'", owner=False)
                elif case == "cleanup":
                    self.cluster.admin_exec("UPDATE forge_agent.coding_reviews SET verification=jsonb_set(verification,'{cleanup_confirmed}','false') WHERE review_id='" + old["review_id"] + "'", owner=False)
                elif case == "cancelled":
                    self.service.cancel(WorkCancel("homelab", UUID(child.task_id), self.get(child.task_id)["revision"], "cancel-preparation-retry"))
                else:
                    self.cluster.admin_exec("DELETE FROM forge_agent.coding_worker_grants WHERE agent_profile='agy-v2'", owner=False)
                with self.assertRaises(Rejected):
                    self.coordinator.store.retry_preparation(**request)
                self.assertEqual(self.counts(child), before)
                self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_reviews WHERE previous_review_id=%s", (old["review_id"],))[0][0], 0)
                self.assertIsNone(self.claim("agy-v2"))

    def test_second_preparation_failure_exhausts_existing_infrastructure_allowance(self):
        _author, child, _plan, _source, old, raw, request = self.preparation_failure()
        before = self.counts(child)
        self.coordinator.store.retry_preparation(**request)
        retried = self.assert_retry_job(child, old, raw, before)
        self.db.review_finish(retried, self.failed_outcome(retried))
        self.coordinator.run_once()
        request2 = {**request, "expected_revision": self.get(child.task_id)["revision"],
            "review_id": UUID(retried["review_id"]), "idempotency_key": "second-preparation-recovery"}
        with self.assertRaises(Rejected):
            self.coordinator.store.retry_preparation(**request2)
        self.coordinator.run_once()
        self.assertIsNone(self.db.review_claim("checks"))
        self.assertIsNone(self.claim("agy-v2"))
        self.assertEqual(self.counts(child), before)
