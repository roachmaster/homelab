"""Source readiness delivers the same stopped work without resetting budgets."""

from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
import hashlib
from threading import Barrier
import time
import unittest
from uuid import UUID, uuid4

from queue_store import Rejected
from test_work_execution_and_usage import make_stop_proof
import test_work_preparation_retry as preparation_fixtures
from work_api import WorkCancel
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE


@unittest.skipUnless(PG16_AVAILABLE, "Requires owned PostgreSQL16")
class SourceReadinessTest(unittest.TestCase):
    # Share scenario construction, not TestCase inheritance/discovery.
    setUp = preparation_fixtures.PreparationRetryTest.setUp
    submit = preparation_fixtures.PreparationRetryTest.submit
    claim = preparation_fixtures.PreparationRetryTest.claim
    publish = preparation_fixtures.PreparationRetryTest.publish
    get = preparation_fixtures.PreparationRetryTest.get
    verdict = preparation_fixtures.PreparationRetryTest.verdict
    finish_plan_round = preparation_fixtures.PreparationRetryTest.finish_plan_round
    checked_implementation = preparation_fixtures.PreparationRetryTest.checked_implementation
    preparation_failure = preparation_fixtures.PreparationRetryTest.preparation_failure
    failed_outcome = staticmethod(preparation_fixtures.PreparationRetryTest.failed_outcome)
    counts = preparation_fixtures.PreparationRetryTest.counts

    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def rows(self, statement, arguments=()):
        with closing(self.cluster.connect("postgres")) as connection:
            with connection.cursor() as cursor:
                cursor.execute(statement, arguments)
                return cursor.fetchall()

    def source_failure(self, *, reconcile=True, workspace_digest="b" * 64, reason="review_unavailable"):
        author, child, plan, _source, first, raw, request0 = self.preparation_failure()
        self.coordinator.store.retry_preparation(**request0)
        job = self.db.review_claim("checks")
        self.assertIsNotNone(job)
        self.assertEqual(job["infrastructure_retries"], 1)
        self.db.review_finish(job, {**self.failed_outcome(job), "reason": reason})
        self.coordinator.run_once()
        self.assertEqual(self.get(child.task_id)["workflow"]["escalation_reason"], reason)
        rec = (self.db.reconcile(child, workspace_digest, make_stop_proof(child.unit_name))
               if reconcile else str(request0["reconciliation_receipt_id"]))
        request = dict(project_alias="homelab", task_id=UUID(child.task_id),
            expected_revision=self.get(child.task_id)["revision"], review_id=UUID(job["review_id"]),
            reconciliation_receipt_id=UUID(rec), idempotency_key="source-readiness-delivery")
        return author, child, plan, first, job, raw, request

    def assert_delivery(self, child, old, raw, request, before, original):
        job = self.db.review_claim("checks")
        self.assertIsNotNone(job)
        self.assertEqual(job["task_id"], child.task_id)
        self.assertEqual(job["attempt_id"], child.attempt_id)
        self.assertEqual(job["parent_result_sha256"], hashlib.sha256(raw.encode()).hexdigest())
        self.assertEqual(job["base_revision"], child.base_revision)
        self.assertEqual(job["workspace_digest"], old["workspace_digest"])
        self.assertEqual(job["scope_plan_decision_id"], child.scope_plan_decision_id)
        self.assertEqual(job["source_review_id"], old["review_id"])
        self.assertIsNone(job["candidate_commit"])
        retained = self.rows("SELECT infrastructure_retries,round,stagnant_rounds FROM forge_agent.coding_reviews WHERE review_id=%s",
                             (old["review_id"],))[0]
        for key, value in zip(("infrastructure_retries", "round", "stagnant_rounds"), retained):
            self.assertEqual(job[key], value)
        self.assertEqual(job["infrastructure_retries"], 1)
        unchanged_findings = self.rows("SELECT next.findings=prior.findings AND "
            "next.verified_finding_ids=prior.verified_finding_ids FROM forge_agent.coding_reviews next "
            "JOIN forge_agent.coding_reviews prior ON prior.review_id=next.previous_review_id "
            "WHERE next.review_id=%s", (job["review_id"],))[0][0]
        self.assertTrue(unchanged_findings)
        current = self.rows("SELECT reconciliation_receipt_id,source_revision FROM forge_agent.coding_reviews WHERE review_id=%s",
                            (job["review_id"],))[0]
        self.assertEqual(current, (request["reconciliation_receipt_id"], request["expected_revision"]))
        self.assertEqual(self.get(child.task_id)["revision"], request["expected_revision"])
        self.assertEqual(self.counts(child), before)
        self.assertEqual(before[2], 1)
        self.assertIsNone(self.claim("agy-v2"))
        self.assertEqual(self.rows("SELECT finish_receipt FROM forge_agent.coding_reviews WHERE review_id=%s",
                                  (old["review_id"],))[0][0], original)
        return job

    def test_scanner_delivers_newly_reconciled_source_without_resetting_budget_or_revision(self):
        author, child, plan, _first, old, raw, request = self.source_failure()
        before = self.counts(child)
        original = self.rows("SELECT finish_receipt FROM forge_agent.coding_reviews WHERE review_id=%s", (old["review_id"],))[0][0]
        self.assertTrue(self.rows("SELECT r.created_at>q.updated_at FROM forge_agent.coding_reconciliation_receipts r "
            "JOIN forge_agent.coding_reviews q ON q.review_id=%s WHERE r.receipt_id=%s",
            (old["review_id"], request["reconciliation_receipt_id"]))[0][0])
        self.coordinator.run_once()
        self.assert_delivery(child, old, raw, request, before, original)
        self.assertEqual(plan["decision_id"], child.scope_plan_decision_id)
        self.assertEqual(child.scope_plan_decision_id, author.plan_decision_id)

    def test_concurrent_delivery_returns_one_immutable_receipt_and_creates_one_review(self):
        _author, child, _plan, _first, old, raw, request = self.source_failure()
        before = self.counts(child)
        original = self.rows("SELECT finish_receipt FROM forge_agent.coding_reviews WHERE review_id=%s", (old["review_id"],))[0][0]
        barrier = Barrier(2)

        def deliver():
            barrier.wait(timeout=5)
            return self.coordinator.store.defer_source_readiness(**request)

        with ThreadPoolExecutor(max_workers=2) as pool:
            left, right = pool.submit(deliver), pool.submit(deliver)
            receipt = left.result(timeout=10)
            self.assertEqual(right.result(timeout=10), receipt)
        self.assertEqual(self.coordinator.store.defer_source_readiness(**request), receipt)
        self.assert_delivery(child, old, raw, request, before, original)
        self.assertEqual(self.coordinator.store.defer_source_readiness(**request), receipt)
        with self.assertRaises(Rejected):
            self.coordinator.store.defer_source_readiness(**{**request, "idempotency_key": "different-delivery-intent"})
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_reviews WHERE previous_review_id=%s", (old["review_id"],))[0][0], 1)
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_workflow_actions WHERE workflow_id="
            "(SELECT workflow_id FROM forge_agent.coding_tasks WHERE id=%s) AND source_key LIKE 'source-readiness-delivery:%%'",
            (child.task_id,))[0][0], 1)

    def test_missing_new_proof_changed_source_or_authority_never_delivers(self):
        for case in ("old_receipt", "changed_workspace", "wrong_receipt", "revision", "reason", "cancel", "grant"):
            with self.subTest(case=case):
                self.setUp()
                _author, child, _plan, _first, old, _raw, request = self.source_failure(
                    reconcile=case != "old_receipt", workspace_digest="c" * 64 if case == "changed_workspace" else "b" * 64,
                    reason="unknown_io_error" if case == "reason" else "review_unavailable")
                before = self.counts(child)
                if case == "wrong_receipt":
                    request["reconciliation_receipt_id"] = uuid4()
                elif case == "revision":
                    request["expected_revision"] -= 1
                elif case == "cancel":
                    self.service.cancel(WorkCancel("homelab", UUID(child.task_id), self.get(child.task_id)["revision"], "cancel-readiness-delivery"))
                elif case == "grant":
                    self.cluster.admin_exec("DELETE FROM forge_agent.coding_worker_grants WHERE agent_profile='agy-v2'", owner=False)
                with self.assertRaises(Rejected):
                    self.coordinator.store.defer_source_readiness(**request)
                self.assertEqual(self.counts(child), before)
                self.assertIsNone(self.claim("agy-v2"))
                self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_reviews WHERE previous_review_id=%s", (old["review_id"],))[0][0], 0)

    def test_explicit_and_implicit_linked_admission_serialize_on_native_task(self):
        _admitted, author, critic, _source = self.checked_implementation(linked=True)
        self.publish(critic, self.verdict(critic, "revise"))
        self.coordinator.run_once()
        child = self.claim("agy-v2")
        self.assertEqual(child.parent_task_id, author.task_id)
        rec = self.publish(child, "The exact linked repair is ready for independent checks.")
        blocker = self.cluster.connect("postgres")
        blocker.autocommit = False
        try:
            with blocker.cursor() as cursor:
                cursor.execute("SELECT id FROM forge_agent.coding_tasks WHERE id=%s FOR UPDATE", (child.task_id,))
            def explicit():
                return self.db.queue_review(child, rec)

            def implicit():
                return self.db.review_claim("admission-race")

            with ThreadPoolExecutor(max_workers=2) as pool:
                admitted = pool.submit(explicit)
                try:
                    deadline = time.monotonic() + 3
                    admission_held = False
                    while time.monotonic() < deadline:
                        admission_held = self.rows("SELECT EXISTS(SELECT 1 FROM pg_locks l "
                            "JOIN pg_stat_activity a USING(pid) WHERE l.locktype='advisory' AND l.granted "
                            "AND a.datname='forge_agent' AND a.wait_event_type='Lock' "
                            "AND a.query LIKE 'SELECT forge_coding_worker_api.queue_review(%%')")[0][0]
                        if admission_held:
                            break
                        time.sleep(.01)
                    self.assertTrue(admission_held)
                    claimed = pool.submit(implicit)
                    time.sleep(.1)
                    self.assertFalse(admitted.done())
                    self.assertFalse(claimed.done())
                finally:
                    blocker.rollback()
                review_id, job = admitted.result(timeout=10), claimed.result(timeout=10)
            self.assertIsNotNone(job)
            self.assertEqual(review_id, job["review_id"])
            self.assertEqual(job["task_id"], child.task_id)
            self.assertEqual(job["scope_plan_decision_id"], child.scope_plan_decision_id)
            self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_reviews WHERE task_id=%s AND attempt_id=%s",
                                      (child.task_id, child.attempt_id))[0][0], 1)
        finally:
            blocker.rollback()
            blocker.close()
