"""Real PostgreSQL regressions for coding-worker fencing and recovery boundaries."""

from contextlib import closing
import hashlib
import threading
import time
import unittest
from uuid import UUID, uuid4

from test_work_execution_and_usage import (
    BASE_REV, make_stop_proof, make_submit, make_v2_config,
)
from work_api import WorkAccept, WorkCancel, WorkGet, WorkServiceFactory
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE
from work_worker_store import LeaseExpiredError, WorkerAccessError, WorkerDB, WorkerStoreError
import work_policy


@unittest.skipUnless(PG16_AVAILABLE, "Requires non-root disposable PostgreSQL 16")
class WorkerDatabaseFencesTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def setUp(self):
        self.cluster.reset_coding_tables()
        self.serial = 0
        self.service = self.make_service()
        self.db = self.make_db()
        self.cluster.grant_worker("mission-control", "homelab", "codex-v2", ("plan",))
        self.incarnation = str(uuid4())
        self.advertise(self.db, self.incarnation)

    def make_service(self, owner="mission-control"):
        return WorkServiceFactory(
            make_v2_config(owner=owner), lambda: self.cluster.connect("forge_agent")
        ).create()

    def make_db(self, owner="mission-control", connect_factory=None):
        return WorkerDB(
            connect_factory or (lambda: self.cluster.connect("forge_coding_worker")), owner,
            reconcile_connect_factory=lambda: self.cluster.connect("forge_coding_reconciler"),
        )

    def advertise(self, db, incarnation, modes=("plan",)):
        db.advertise(
            "fence-worker", incarnation,
            [{"alias": "homelab", "base_revision": BASE_REV}],
            [{"alias": "codex-v2", "backend": "codex", "model": "gpt-5.4", "modes": list(modes)}],
            work_policy.POLICY_DIGEST,
        )

    def claim(self, mode="plan"):
        self.serial += 1
        receipt = self.service.submit(make_submit(idempotency_key=f"fence-{self.serial}",mode=mode))
        lease = self.db.claim("fence-worker", self.incarnation, ["codex-v2"])
        self.assertIsNotNone(lease)
        self.assertEqual(lease.task_id, receipt["task_id"])
        return lease

    def detail(self, lease):
        return self.service.get(WorkGet("homelab", UUID(lease.task_id)))

    def rows(self, sql, parameters=()):
        with closing(self.cluster.connect("postgres")) as connection, connection.cursor() as cursor:
            cursor.execute(sql, parameters)
            return cursor.fetchall()

    def stage(self, lease):
        text = "A complete, validated response. 😀"
        self.db.append_result(lease, 0, text)
        raw = text.encode("utf-8")
        return {"sha256": hashlib.sha256(raw).hexdigest(), "byte_count": len(raw), "chunk_count": 1}

    def event_count(self, lease):
        return self.rows("SELECT count(*) FROM forge_agent.coding_events WHERE task_id=%s",
                         (UUID(lease.task_id),))[0][0]

    def call_after_expiry_under_task_lock(self, lease, operation, bound):
        """Observe the worker waiting on a real row lock, then cross its DB-time fence."""
        started = threading.Event()
        result = {}

        def connect_worker():
            connection = self.cluster.connect("forge_coding_worker")
            result["pid"] = connection.get_backend_pid()
            started.set()
            return connection

        worker = self.make_db(connect_factory=connect_worker)

        def invoke():
            try:
                result["value"] = operation(worker)
            except BaseException as error:
                result["error"] = error

        with (closing(self.cluster.connect("postgres")) as blocker,
              closing(self.cluster.connect("postgres")) as monitor):
            monitor.autocommit = True
            with blocker.cursor() as cursor:
                cursor.execute(
                    "UPDATE forge_agent.coding_attempts "
                    "SET lease_expires_at=clock_timestamp()+interval '30 seconds', "
                    "deadline_at=clock_timestamp()+interval '30 seconds' WHERE attempt_id=%s",
                    (UUID(lease.attempt_id),),
                )
            blocker.commit()
            with blocker.cursor() as cursor:
                cursor.execute("SELECT id FROM forge_agent.coding_tasks WHERE id=%s FOR UPDATE",
                               (UUID(lease.task_id),))
            thread = threading.Thread(target=invoke, daemon=True)
            thread.start()
            try:
                self.assertTrue(started.wait(3), "Worker did not acquire its own database connection")
                timeout = time.monotonic() + 3
                with monitor.cursor() as cursor:
                    while True:
                        cursor.execute("SELECT wait_event_type FROM pg_stat_activity WHERE pid=%s",
                                       (result["pid"],))
                        if cursor.fetchone() == ("Lock",):
                            break
                        self.assertLess(time.monotonic(), timeout, "Worker never waited for the task lock")
                        time.sleep(0.01)
                    # Expire only after observing the blocked call. Its entry-time
                    # clock is therefore earlier than the fence regardless of
                    # how quickly this machine schedules the worker thread.
                    cursor.execute(
                        f"UPDATE forge_agent.coding_attempts SET {bound}=clock_timestamp()+interval '0.05 seconds' "
                        "WHERE attempt_id=%s", (UUID(lease.attempt_id),),
                    )
                    cursor.execute(
                        f"SELECT pg_sleep(greatest(0, extract(epoch FROM {bound}-clock_timestamp()))+0.05) "
                        "FROM forge_agent.coding_attempts WHERE attempt_id=%s",
                        (UUID(lease.attempt_id),),
                    )
            finally:
                blocker.commit()
                thread.join(5)
            self.assertFalse(thread.is_alive(), "Worker did not finish after releasing the row lock")
        return result

    def test_live_mutations_recheck_lease_and_deadline_after_waiting_for_task_lock(self):
        for bound in ("lease_expires_at", "deadline_at"):
            for name in ("renew", "checkpoint", "append_result", "publish"):
                with self.subTest(bound=bound, operation=name):
                    lease = self.claim()
                    manifest = self.stage(lease) if name == "publish" else None
                    before = self.event_count(lease)
                    proof = make_stop_proof(lease.unit_name)
                    operations = {
                        "renew": lambda db: db.renew(lease),
                        "checkpoint": lambda db: db.checkpoint(lease, "Must not persist after expiry"),
                        "append_result": lambda db: db.append_result(lease, 0, "Must not persist after expiry"),
                        "publish": lambda db: db.publish(
                            lease, "awaiting_review", manifest, None, "Must not publish after expiry", proof),
                    }
                    result = self.call_after_expiry_under_task_lock(lease, operations[name], bound)
                    if name == "renew":
                        self.assertNotIn("error", result)
                        self.assertIs(result.get("value"), False)
                    else:
                        self.assertIsInstance(result.get("error"), LeaseExpiredError, result)
                    self.assertEqual(self.detail(lease)["status"], "running")
                    self.assertEqual(self.event_count(lease), before)
                    if name == "append_result":
                        self.assertEqual(self.rows(
                            "SELECT count(*) FROM forge_agent.coding_result_chunks WHERE attempt_id=%s",
                            (UUID(lease.attempt_id),),
                        )[0][0], 0)

    def test_cancellation_winning_row_lock_fences_success_and_preserves_receipt_replay(self):
        lease = self.claim()
        manifest = self.stage(lease)
        command = WorkCancel("homelab", UUID(lease.task_id), 2, "cancel-winning")
        cancel_receipt = self.service.cancel(command)
        self.assertEqual(cancel_receipt["status"], "cancel_requested")
        before = self.event_count(lease)
        proof = make_stop_proof(lease.unit_name)
        with self.assertRaises(WorkerStoreError) as caught:
            self.db.publish(lease, "awaiting_review", manifest, None, "Must not replace cancellation", proof)
        self.assertEqual(caught.exception.status, 409)
        self.assertEqual(self.detail(lease)["status"], "cancel_requested")
        self.assertEqual(self.event_count(lease), before)
        publication = self.db.publish(lease, "cancelled", None, None, "Stopped as requested", proof)
        self.assertEqual(publication["status"], "cancelled")
        after = self.event_count(lease)
        self.assertEqual(self.db.publish(lease, "cancelled", None, None, "Stopped as requested", proof), publication)
        self.assertEqual(self.service.cancel(command), cancel_receipt)
        self.assertEqual(self.event_count(lease), after)

    def test_record_stop_transitions_unpublished_current_attempts_without_stranding_running_tasks(self):
        for cancelled in (False, True):
            with self.subTest(cancel_requested=cancelled):
                lease = self.claim()
                if cancelled:
                    self.service.cancel(WorkCancel("homelab", UUID(lease.task_id), 2, "stop-cancel"))
                before = self.event_count(lease)
                self.db.record_stop(lease, make_stop_proof(lease.unit_name), "Publication was interrupted", "interrupted")
                expected = "cancelled" if cancelled else "needs_reconcile"
                self.assertEqual(self.detail(lease)["status"], expected)
                self.assertEqual(self.event_count(lease), before + 1)
                self.assertEqual(self.db.expire_leases(), 0)
                if not cancelled:
                    self.assertIn(lease.task_id, [item["task_id"] for item in self.db.pending_reconciliations()])
                # Stop acknowledgements may repeat; they must not repeat state transitions.
                self.db.record_stop(lease, make_stop_proof(lease.unit_name), "Repeated stop observation", "interrupted")
                self.assertEqual(self.detail(lease)["status"], expected)
                self.assertEqual(self.event_count(lease), before + 1)

    def test_late_stop_never_changes_completed_publication_or_its_original_receipt(self):
        self.cluster.grant_worker("mission-control","homelab","codex-v2",("review",))
        self.advertise(self.db,self.incarnation,modes=("plan","review"))
        lease = self.claim(mode="review")
        manifest = self.stage(lease)
        proof = make_stop_proof(lease.unit_name)
        receipt = self.db.publish(lease, "awaiting_review", manifest, None, "Accepted agent result", proof)
        self.service.accept(WorkAccept("homelab", UUID(lease.task_id), receipt["revision"], "accept-final", ""))
        before = self.detail(lease)
        count = self.event_count(lease)
        self.db.record_stop(lease, proof, "Late cleanup must not overwrite the publication", "interrupted")
        self.assertEqual(self.detail(lease), before)
        self.assertEqual(self.event_count(lease), count)
        self.assertEqual(self.db.publish(lease, "awaiting_review", manifest, None, "Accepted agent result", proof), receipt)

    def test_stopped_normal_outcomes_missing_receipts_remain_discoverable_and_redacted(self):
        for outcome in ("quota_blocked", "failed", "awaiting_review"):
            with self.subTest(outcome=outcome):
                lease = self.claim()
                manifest = self.stage(lease) if outcome == "awaiting_review" else None
                proof = make_stop_proof(lease.unit_name)
                self.db.publish(lease, outcome, manifest, None, "Stopped publication before worker crash", proof)
                pending = {item["task_id"]: item for item in self.db.pending_reconciliations()}
                self.assertIn(lease.task_id, pending)
                row = pending[lease.task_id]
                for private in ("attempt_token", "prompt", "acceptance_criteria", "recipe"):
                    self.assertNotIn(private, row)
                receipt = self.db.reconcile(row, "d" * 64, proof)
                UUID(receipt)
                self.assertEqual(self.detail(lease)["reconciliation_receipt_id"], receipt)
                self.assertNotIn(lease.task_id, [item["task_id"] for item in self.db.pending_reconciliations()])

    def test_owner_without_matching_grants_cannot_advertise_or_claim_another_owners_task(self):
        wrong_owner = self.make_db("other-owner")
        incarnation = str(uuid4())
        with self.assertRaises(WorkerAccessError):
            self.advertise(wrong_owner, incarnation)
        self.assertEqual(self.rows(
            "SELECT count(*) FROM forge_agent.coding_worker_readiness WHERE incarnation=%s", (UUID(incarnation),)
        )[0][0], 0)
        receipt = self.service.submit(make_submit(idempotency_key="owner-bound"))
        try:
            claimed = wrong_owner.claim("fence-worker", self.incarnation, ["codex-v2"])
        except WorkerAccessError:
            claimed = None
        self.assertIsNone(claimed)
        self.assertEqual(self.rows("SELECT status FROM forge_agent.coding_tasks WHERE id=%s",
                                   (UUID(receipt["task_id"]),))[0][0], "queued")

    def test_shared_role_and_worker_identity_still_respect_each_ports_configured_owner(self):
        self.cluster.grant_worker("other-owner", "homelab", "codex-v2", ("plan",))
        other_service = self.make_service("other-owner")
        first = other_service.submit(make_submit(idempotency_key="other-first"))
        second = self.service.submit(make_submit(idempotency_key="own-second"))
        other_db = self.make_db("other-owner")
        self.advertise(other_db, self.incarnation)
        self.advertise(self.db, self.incarnation)
        own = self.db.claim("fence-worker", self.incarnation, ["codex-v2"])
        self.assertEqual(own.task_id, second["task_id"])
        other = other_db.claim("fence-worker", self.incarnation, ["codex-v2"])
        self.assertEqual(other.task_id, first["task_id"])


if __name__ == "__main__":
    unittest.main()
