"""Real PostgreSQL regressions for ordered, replayable coding progress."""

from contextlib import closing
import hashlib
import select
import threading
import time
import unittest
from unittest.mock import Mock
from uuid import UUID, uuid4

import psycopg2

from test_work_execution_and_usage import BASE_REV, make_stop_proof, make_submit, make_v2_config
from work_api import WorkAccept, WorkCancel, WorkGet, WorkEvents, WorkServiceFactory
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE, SQL_DIR
from work_store import NOTIFY_CHANNEL
from work_worker_store import LeaseExpiredError, OutcomeConflictError, WorkerAccessError, WorkerDB
import work_policy


PROGRESS_FUNCTION = "forge_coding_worker_api.report_progress(uuid,uuid,integer,text,text)"


@unittest.skipUnless(PG16_AVAILABLE, "Requires non-root disposable PostgreSQL 16")
class WorkProgressTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def setUp(self):
        self.cluster.reset_coding_tables()
        self.serial = 0
        self.db = self.make_db()
        self.service = WorkServiceFactory(make_v2_config(), lambda: self.cluster.connect("forge_agent")).create()
        self.cluster.grant_worker("mission-control", "homelab", "codex-v2", ("plan", "review"))
        self.incarnation = str(uuid4())
        self.db.advertise(
            "progress-worker", self.incarnation, [{"alias": "homelab", "base_revision": BASE_REV}],
            [{"alias": "codex-v2", "backend": "codex", "model": "gpt-5.4", "modes": ["plan", "review"]}],
            work_policy.POLICY_DIGEST,
        )

    def make_db(self, owner="mission-control", connect_factory=None):
        return WorkerDB(connect_factory or (lambda: self.cluster.connect("forge_coding_worker")), owner)

    def claim(self, mode="plan"):
        self.serial += 1
        task = self.service.submit(make_submit(idempotency_key=f"progress-{self.serial}", mode=mode))
        lease = self.db.claim("progress-worker", self.incarnation, ["codex-v2"])
        self.assertEqual(lease.task_id, task["task_id"])
        return lease

    def rows(self, statement, parameters=()):
        with closing(self.cluster.connect("postgres")) as connection, connection.cursor() as cursor:
            cursor.execute(statement, parameters)
            return cursor.fetchall()

    def detail(self, lease):
        return self.service.get(WorkGet("homelab", UUID(lease.task_id)))

    def events(self, lease):
        return self.rows(
            "SELECT event_kind, payload, progress_attempt_id, progress_sequence "
            "FROM forge_agent.coding_events WHERE task_id=%s ORDER BY sequence", (UUID(lease.task_id),)
        )

    def snapshot(self, lease):
        return self.rows(
            "SELECT to_jsonb(t),to_jsonb(a) FROM forge_agent.coding_tasks t "
            "JOIN forge_agent.coding_attempts a ON a.task_id=t.id AND a.attempt_id=%s WHERE t.id=%s",
            (UUID(lease.attempt_id), UUID(lease.task_id)),
        )[0]

    def notices(self, listener, timeout=0.1):
        if select.select([listener], [], [], timeout)[0]:
            listener.poll()
        notices = list(listener.notifies)
        listener.notifies.clear()
        return notices

    def test_ordered_plan_progress_retries_and_concurrent_identical_commit_once(self):
        lease = self.claim()
        before_task, before_attempt = self.snapshot(lease)
        with closing(self.cluster.connect("postgres")) as listener:
            listener.autocommit = True
            with listener.cursor() as cursor:
                cursor.execute(f"LISTEN {NOTIFY_CHANNEL}")
            self.db.report_progress(lease, 1, "plan", "Inspect the existing implementation.")
            notice = self.notices(listener)
            self.assertEqual([(item.channel, item.payload) for item in notice], [(NOTIFY_CHANNEL, "")])
            self.db.report_progress(lease, 2, "progress", "The state transition has been implemented.")
            self.assertEqual(len(self.notices(listener)), 1)
            self.db.report_progress(lease, 1, "plan", "Inspect the existing implementation.")
            self.assertEqual(self.notices(listener), [])
        self.assertEqual(self.snapshot(lease)[1]["summary"], "The state transition has been implemented.")
        with self.assertRaises(OutcomeConflictError):
            self.db.report_progress(lease, 2, "progress", "Changed content for a committed ordinal.")
        with self.assertRaises((ValueError, OutcomeConflictError)):
            self.db.report_progress(lease, 4, "progress", "Sequence three is missing.")
        barrier = threading.Barrier(3)
        errors = []

        def report():
            try:
                barrier.wait(timeout=3)
                self.db.report_progress(lease, 3, "progress", "Focused verification passed.")
            except BaseException as error:
                errors.append(error)

        threads = [threading.Thread(target=report) for _ in range(2)]
        for thread in threads:
            thread.start()
        barrier.wait(timeout=3)
        for thread in threads:
            thread.join(5)
            self.assertFalse(thread.is_alive())
        self.assertEqual(errors, [])
        progress = [row for row in self.events(lease) if row[3] is not None]
        self.assertEqual([row[3] for row in progress], [1, 2, 3])
        for ordinal, row in enumerate(progress, 1):
            kind, payload, attempt_id, sequence = row
            self.assertEqual(kind, "checkpoint")
            self.assertEqual(attempt_id, UUID(lease.attempt_id))
            self.assertEqual(set(payload), {"attempt_id", "progress_sequence", "stage", "summary"})
            self.assertEqual(payload["attempt_id"], lease.attempt_id)
            self.assertEqual(payload["progress_sequence"], sequence)
            self.assertEqual(payload["stage"], "plan" if ordinal == 1 else "progress")
        after_task, after_attempt = self.snapshot(lease)
        self.assertEqual(after_task, before_task)
        for field in ("phase", "outcome", "stop_proof", "result_sha256", "lease_expires_at", "deadline_at", "reservation_active"):
            self.assertEqual(after_attempt[field], before_attempt[field], field)
        self.assertEqual(after_attempt["summary"], "Focused verification passed.")
        public = self.service.events(WorkEvents("homelab", UUID(lease.task_id), 0, 50))
        reports = [e["progress"] for e in public["events"] if "progress" in e]
        self.assertEqual([p["sequence"] for p in reports], [1, 2, 3])
        self.assertEqual(reports[0]["stage"], "plan")
        self.assertEqual(reports[-1]["summary"], "Focused verification passed.")
        self.assertEqual(set(reports[0]), {"attempt_id", "sequence", "stage", "summary"})
        self.assertEqual(public["revision"], before_task["revision"])
        self.assertEqual(public["status"], "running")


    def test_bounds_controls_plan_first_and_128_event_cap(self):
        lease = self.claim()
        unopened = Mock(side_effect=AssertionError("Invalid input must be rejected before opening a connection"))
        validating = self.make_db(connect_factory=unopened)
        invalid = (
            (True, "plan", "x"), (False, "plan", "x"), (0, "plan", "x"), (129, "progress", "x"),
            (1.0, "plan", "x"), ("1", "plan", "x"), (1, "other", "x"),
            (1, "plan", ""), (1, "plan", " \r\n\t"), (1, "plan", "x" * 4097),
            (1, "plan", "é" * 2049), (1, "plan", "x\0y"), (1, "plan", "x\x01y"),
            (1, "plan", "x\x85y"), (1, "plan", "x\ry"), (1, "plan", "\ud800"), (1, "plan", None),
        )
        for arguments in invalid:
            with self.subTest(arguments=repr(arguments)):
                with self.assertRaises(ValueError):
                    validating.report_progress(lease, *arguments)
        unopened.assert_not_called()
        for arguments in ((2, "progress", "No plan yet"), (1, "progress", "Wrong first stage")):
            with self.assertRaises((ValueError, OutcomeConflictError)):
                self.db.report_progress(lease, *arguments)
        # The SECURITY DEFINER boundary enforces its own bounds, independently of Python.
        for summary in ("\x01invalid", "x" * 4097, " \t\r\n"):
            with closing(self.cluster.connect("forge_coding_worker")) as connection:
                with self.assertRaises(psycopg2.Error) as caught:
                    with connection, connection.cursor() as cursor:
                        cursor.execute("SELECT set_config('forge.coding_owner','mission-control',true)")
                        cursor.execute("SELECT forge_coding_worker_api.report_progress(%s,%s,1,'plan',%s)",
                                       (UUID(lease.attempt_id), UUID(lease.attempt_token), summary))
                self.assertEqual(caught.exception.pgcode, "22023")
        self.db.report_progress(lease, 1, "plan", "é" * 2048)
        for ordinal in range(2, 129):
            self.db.report_progress(lease, ordinal, "progress", f"Step {ordinal}: allowed\n\ttext")
        self.assertEqual(len([row for row in self.events(lease) if row[3] is not None]), 128)
        self.assertEqual(self.snapshot(lease)[1]["summary"], "Step 128: allowed\n\ttext")
        self.db.report_progress(lease, 1, "plan", "é" * 2048)
        with self.assertRaises(ValueError):
            self.db.report_progress(lease, 129, "progress", "Beyond the cap")

    def test_exact_replay_after_expiry_completed_and_cancelled_cannot_regress_state(self):
        for state in ("expired", "completed", "cancelled"):
            with self.subTest(state=state):
                lease = self.claim(mode="review" if state == "completed" else "plan")
                self.db.report_progress(lease, 1, "plan", "The original plan")
                self.db.report_progress(lease, 2, "progress", "The newest progress")
                if state == "expired":
                    self.cluster.admin_exec(
                        "UPDATE forge_agent.coding_attempts SET lease_expires_at=clock_timestamp()-interval '1 second' "
                        f"WHERE attempt_id='{lease.attempt_id}'"
                    )
                elif state == "cancelled":
                    self.service.cancel(WorkCancel("homelab", UUID(lease.task_id), 2, "cancel-progress"))
                    self.db.publish(lease, "cancelled", None, None, "Stopped as requested", make_stop_proof(lease.unit_name))
                else:
                    text = "A completed response"
                    self.db.append_result(lease, 0, text)
                    manifest = {"sha256": hashlib.sha256(text.encode()).hexdigest(), "byte_count": len(text), "chunk_count": 1}
                    result = self.db.publish(lease, "awaiting_review", manifest, None, "Final reply summary", make_stop_proof(lease.unit_name))
                    self.service.accept(WorkAccept("homelab", UUID(lease.task_id), result["revision"], "accept-progress", ""))
                before = self.snapshot(lease), self.events(lease)
                self.db.report_progress(lease, 1, "plan", "The original plan")
                self.db.report_progress(lease, 2, "progress", "The newest progress")
                self.assertEqual((self.snapshot(lease), self.events(lease)), before)
                with self.assertRaises(OutcomeConflictError):
                    self.db.report_progress(lease, 2, "progress", "Changed after the attempt finished")
                with self.assertRaises(LeaseExpiredError):
                    self.db.report_progress(lease, 3, "progress", "A new write after the fence")

    def test_new_progress_rechecks_db_time_after_waiting_for_task_lock(self):
        for bound in ("lease_expires_at", "deadline_at"):
            with self.subTest(bound=bound):
                lease = self.claim()
                self.db.report_progress(lease, 1, "plan", "A committed plan")
                before = self.snapshot(lease), self.events(lease)
                entered = threading.Event()
                result = {}

                def connect():
                    connection = self.cluster.connect("forge_coding_worker")
                    result["pid"] = connection.get_backend_pid()
                    entered.set()
                    return connection

                db = self.make_db(connect_factory=connect)

                def report():
                    try:
                        db.report_progress(lease, 2, "progress", "Too late after the row-lock wait")
                    except BaseException as error:
                        result["error"] = error

                with (closing(self.cluster.connect("postgres")) as blocker,
                      closing(self.cluster.connect("postgres")) as monitor):
                    monitor.autocommit = True
                    with blocker.cursor() as cursor:
                        cursor.execute("SELECT id FROM forge_agent.coding_tasks WHERE id=%s FOR UPDATE", (UUID(lease.task_id),))
                    thread = threading.Thread(target=report, daemon=True)
                    thread.start()
                    try:
                        self.assertTrue(entered.wait(3))
                        timeout = time.monotonic() + 3
                        with monitor.cursor() as cursor:
                            while True:
                                cursor.execute("SELECT wait_event_type FROM pg_stat_activity WHERE pid=%s", (result["pid"],))
                                if cursor.fetchone() == ("Lock",):
                                    break
                                self.assertLess(time.monotonic(), timeout)
                                time.sleep(0.01)
                            cursor.execute(
                                f"UPDATE forge_agent.coding_attempts SET {bound}=clock_timestamp()+interval '0.05 seconds' "
                                "WHERE attempt_id=%s", (UUID(lease.attempt_id),),
                            )
                            cursor.execute(
                                f"SELECT pg_sleep(greatest(0,extract(epoch FROM {bound}-clock_timestamp()))+0.05) "
                                "FROM forge_agent.coding_attempts WHERE attempt_id=%s", (UUID(lease.attempt_id),),
                            )
                    finally:
                        blocker.commit()
                        thread.join(5)
                    self.assertFalse(thread.is_alive())
                self.assertIsInstance(result.get("error"), LeaseExpiredError)
                # The fixture changed only the chosen time bound; no progress write survived.
                self.assertEqual(self.events(lease), before[1])
                current = self.snapshot(lease)
                self.assertEqual(current[0], before[0][0])
                self.assertEqual(current[1]["summary"], before[0][1]["summary"])

    def test_wrong_owner_token_and_legacy_checkpoint_are_independent(self):
        lease = self.claim()
        before = self.events(lease)
        wrong = {"attempt_id": lease.attempt_id, "attempt_token": str(uuid4())}
        with self.assertRaises(WorkerAccessError):
            self.db.report_progress(wrong, 1, "plan", "An unauthorized token")
        with self.assertRaises(WorkerAccessError):
            self.make_db("different-owner").report_progress(lease, 1, "plan", "An unauthorized owner")
        self.assertEqual(self.events(lease), before)
        self.db.checkpoint(lease, "Existing observer checkpoint")
        legacy = self.events(lease)[-1]
        self.assertEqual(legacy[0], "checkpoint")
        self.assertNotIn("progress_sequence", legacy[1])
        self.assertEqual(legacy[2:], (None, None))
        self.db.report_progress(lease, 1, "plan", "Progress ordinal one remains available")
        self.assertEqual(self.events(lease)[-1][3], 1)
        self.assertTrue(all(row[2:] == (None, None) for row in self.events(lease)[:3]))

    def test_migration_12_apply_twice_schema_scope_and_narrow_acl_drift(self):
        self.cluster.apply_and_verify()
        self.cluster.apply_and_verify()
        self.assertEqual(self.rows("SELECT version FROM forge_agent.schema_migrations WHERE version=12")[0][0], 12)
        columns = self.rows(
            "SELECT attname,format_type(atttypid,atttypmod),attnotnull FROM pg_attribute "
            "WHERE attrelid='forge_agent.coding_events'::regclass "
            "AND attname IN ('progress_attempt_id','progress_sequence') ORDER BY attname"
        )
        self.assertEqual(columns, [("progress_attempt_id", "uuid", False), ("progress_sequence", "integer", False)])
        foreign = self.rows(
            "SELECT pg_get_constraintdef(c.oid,false) FROM pg_constraint c "
            "WHERE c.conrelid='forge_agent.coding_events'::regclass AND c.contype='f' "
            "AND EXISTS(SELECT 1 FROM pg_attribute a WHERE a.attrelid=c.conrelid "
            "AND a.attnum=ANY(c.conkey) AND a.attname='progress_attempt_id')"
        )
        self.assertEqual(len(foreign), 1)
        self.assertIn("(owner_id, project_alias, task_id, progress_attempt_id)", foreign[0][0])
        self.assertIn("coding_attempts(owner_id, project_alias, task_id, attempt_id)", foreign[0][0])
        for role in ("public", "forge_agent", "forge_gpu_worker", "forge_coding_reconciler", "forge_coding_worker"):
            self.assertEqual(self.rows("SELECT has_function_privilege(%s,%s,'EXECUTE')", (role, PROGRESS_FUNCTION))[0][0],
                             role == "forge_coding_worker")
        script = SQL_DIR / "management/verify.sql"

        def verify_fails():
            result = self.cluster.run_sql_script(script)
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)

        for grant in (True, False):
            try:
                self.cluster.admin_exec(
                    f"GRANT EXECUTE ON FUNCTION {PROGRESS_FUNCTION} TO PUBLIC" if grant
                    else f"REVOKE EXECUTE ON FUNCTION {PROGRESS_FUNCTION} FROM forge_coding_worker"
                )
                verify_fails()
            finally:
                self.cluster.admin_exec(
                    f"REVOKE EXECUTE ON FUNCTION {PROGRESS_FUNCTION} FROM PUBLIC" if grant
                    else f"GRANT EXECUTE ON FUNCTION {PROGRESS_FUNCTION} TO forge_coding_worker"
                )
        try:
            self.cluster.admin_exec(f"ALTER FUNCTION {PROGRESS_FUNCTION} SECURITY INVOKER")
            verify_fails()
        finally:
            self.cluster.admin_exec(f"ALTER FUNCTION {PROGRESS_FUNCTION} SECURITY DEFINER")
        try:
            self.cluster.admin_exec("ALTER TABLE forge_agent.coding_events RENAME progress_sequence TO hidden_progress_sequence")
            verify_fails()
        finally:
            self.cluster.admin_exec("ALTER TABLE forge_agent.coding_events RENAME hidden_progress_sequence TO progress_sequence")
        self.cluster.apply_and_verify()


if __name__ == "__main__":
    unittest.main()
