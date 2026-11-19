"""Disposable PostgreSQL 16 integration tests for Forge coding-job durable admission."""

from contextlib import closing
from datetime import UTC, datetime
import select
import threading
import unittest
from unittest.mock import Mock, patch
from uuid import UUID, uuid4

import psycopg2

from queue_store import Rejected
import work_api
import work_config
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE, SQL_DIR
from work_store import NOTIFY_CHANNEL, WorkStore

BASE_REV = "2e1300a00da92e9f97bdb1a656c2b182d4219a6c"


def make_config(*, owner="mission-control", projects=("homelab", "docs"),
                modes=("plan", "review", "implement"), max_pending=32,
                agy_model="gemini-2.5-pro", codex_model="gpt-5.4"):
    return work_config.parse_work_config({
        "owner": owner,
        "allowed_projects": list(projects),
        "allowed_modes": list(modes),
        "max_pending": max_pending,
        "profiles": {
            "agy-plan": {
                "backend": "antigravity",
                "model": agy_model,
                "resource_profile": "plan-default",
            },
            "codex-review": {
                "backend": "codex",
                "model": codex_model,
                "resource_profile": "review-default",
            },
        },
    })


def make_submit(*, project_alias="homelab", base_revision=BASE_REV, mode="plan",
                agent_profile="agy-plan", prompt="Draft plan",
                acceptance_criteria="Bounded output", idempotency_key="job-1"):
    return work_api.WorkSubmit(
        project_alias=project_alias,
        base_revision=base_revision,
        mode=mode,
        agent_profile=agent_profile,
        prompt=prompt,
        acceptance_criteria=acceptance_criteria,
        idempotency_key=idempotency_key,
    )


def drain_notifications(listener, timeout=0.4):
    found = []
    if select.select([listener], [], [], timeout)[0]:
        listener.poll()
        while listener.notifies:
            found.append(listener.notifies.pop(0))
    return found


@unittest.skipUnless(
    PG16_AVAILABLE,
    "Disposable PostgreSQL 16 integration requires its binaries and a non-root user",
)
class WorkStorePostgresTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def setUp(self):
        self.cluster.reset_coding_tables()
        self.opened_connections = []

    def tracked_connect(self):
        conn = self.cluster.connect("forge_agent")
        self.opened_connections.append(conn)
        return conn

    def assert_all_connections_closed(self):
        self.assertTrue(
            all(conn.closed != 0 for conn in self.opened_connections),
            "Every per-operation database connection must be closed",
        )

    def make_service(self, **config_kwargs):
        config = make_config(**config_kwargs)
        return work_api.WorkServiceFactory(config, self.tracked_connect).create()

    def assert_sql_verification(self, script, *, user="postgres", error=None):
        result = self.cluster.run_sql_script(script, user=user)
        if error is None:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn(error, result.stderr)

    def test_apply_and_verify_twice_and_detect_migration_0009_drift(self):
        self.cluster.apply_and_verify()
        self.cluster.apply_and_verify()

        verify_script = SQL_DIR / "management/verify.sql"
        self.assert_sql_verification(verify_script)

        try:
            self.cluster.admin_exec("GRANT SELECT ON forge_agent.coding_tasks TO PUBLIC")
            self.assert_sql_verification(verify_script, error="Unexpected PUBLIC privileges")
        finally:
            self.cluster.admin_exec("REVOKE SELECT ON forge_agent.coding_tasks FROM PUBLIC")
        self.assert_sql_verification(verify_script)

        try:
            self.cluster.admin_exec("GRANT SELECT (payload) ON forge_agent.coding_events TO PUBLIC")
            self.assert_sql_verification(verify_script, error="Unexpected PUBLIC privileges")
        finally:
            self.cluster.admin_exec("REVOKE SELECT (payload) ON forge_agent.coding_events FROM PUBLIC")
        self.assert_sql_verification(verify_script)

        try:
            self.cluster.admin_exec("GRANT SELECT ON forge_agent.coding_control_receipts TO forge_gpu_worker")
            self.assert_sql_verification(verify_script, error="Unexpected Forge GPU worker privileges")
        finally:
            self.cluster.admin_exec("REVOKE SELECT ON forge_agent.coding_control_receipts FROM forge_gpu_worker")
        self.assert_sql_verification(verify_script)

        try:
            self.cluster.admin_exec(
                "ALTER TABLE forge_agent.coding_tasks DROP CONSTRAINT coding_tasks_recipe_mode_check"
            )
            self.assert_sql_verification(verify_script, error="coding_tasks_recipe_mode_check")
        finally:
            self.cluster.admin_exec(
                "ALTER TABLE forge_agent.coding_tasks ADD CONSTRAINT coding_tasks_recipe_mode_check "
                "CHECK ((pinned_recipe->>'mode') = mode)"
            )
        self.assert_sql_verification(verify_script)

    def test_exact_acl_matrix_covers_coding_and_existing_mail_gpu_tables(self):
        script = SQL_DIR / "management/verify-runner-privileges.sql"
        self.assert_sql_verification(script, user="forge_agent")
        self.assert_sql_verification(script, error="Expected current_user forge_agent")

        allowed = {
            "mail_threads": {"SELECT", "INSERT", "UPDATE"},
            "mail_messages": {"SELECT", "INSERT"},
            "inference_results": {"SELECT", "DELETE"},
            "gpu_worker_readiness": {"SELECT"},
            "coding_tasks": {"SELECT", "INSERT", "UPDATE"},
            "coding_events": {"SELECT", "INSERT"},
            "coding_control_receipts": {"SELECT", "INSERT"},
        }
        columns = {
            "mail_threads": "owner_id",
            "mail_messages": "content",
            "inference_results": "run_id",
            "gpu_worker_readiness": "worker_name",
            "coding_tasks": "prompt",
            "coding_events": "payload",
            "coding_control_receipts": "receipt_json",
        }
        for table, privileges in allowed.items():
            for privilege in ("SELECT", "INSERT", "UPDATE", "DELETE", "TRUNCATE", "REFERENCES", "TRIGGER"):
                required = privilege in privileges
                change, restore = ("REVOKE", "GRANT") if required else ("GRANT", "REVOKE")
                with self.subTest(drift=change, table=table, privilege=privilege):
                    try:
                        self.cluster.admin_exec(
                            f"{change} {privilege} ON forge_agent.{table} "
                            f"{'FROM' if required else 'TO'} forge_agent"
                        )
                        self.assert_sql_verification(
                            script, user="forge_agent", error="Unexpected Forge database privileges"
                        )
                    finally:
                        self.cluster.admin_exec(
                            f"{restore} {privilege} ON forge_agent.{table} "
                            f"{'TO' if required else 'FROM'} forge_agent"
                        )
                    self.assert_sql_verification(script, user="forge_agent")

            for privilege in ("SELECT", "INSERT", "UPDATE", "REFERENCES"):
                if privilege in privileges:
                    continue
                with self.subTest(drift="column_grant", table=table, privilege=privilege):
                    try:
                        self.cluster.admin_exec(
                            f"GRANT {privilege} ({columns[table]}) ON forge_agent.{table} TO forge_agent"
                        )
                        self.assert_sql_verification(
                            script, user="forge_agent", error="Unexpected Forge database privileges"
                        )
                    finally:
                        self.cluster.admin_exec(
                            f"REVOKE {privilege} ({columns[table]}) ON forge_agent.{table} FROM forge_agent"
                        )
                    self.assert_sql_verification(script, user="forge_agent")

        inherited = "forge_test_work_grants"
        self.cluster.admin_exec(
            f"CREATE ROLE {inherited} NOLOGIN",
            f"GRANT {inherited} TO forge_agent",
            owner=False,
        )
        try:
            for table, col in [
                ("coding_tasks", "prompt"),
                ("coding_events", "payload"),
                ("coding_control_receipts", "receipt_json"),
            ]:
                for grantee in ("PUBLIC", inherited):
                    with self.subTest(table=table, grantee=grantee):
                        try:
                            self.cluster.admin_exec(
                                f"REVOKE SELECT ON forge_agent.{table} FROM forge_agent",
                                f"GRANT SELECT ON forge_agent.{table} TO {grantee}",
                            )
                            self.assert_sql_verification(script, user="forge_agent")
                            self.cluster.admin_exec(f"GRANT DELETE ON forge_agent.{table} TO {grantee}")
                            self.assert_sql_verification(
                                script, user="forge_agent", error="Unexpected Forge database privileges"
                            )
                            self.cluster.admin_exec(f"REVOKE DELETE ON forge_agent.{table} FROM {grantee}")
                            if table != "coding_tasks":
                                self.cluster.admin_exec(
                                    f"GRANT UPDATE ({col}) ON forge_agent.{table} TO {grantee}"
                                )
                                self.assert_sql_verification(
                                    script, user="forge_agent", error="Unexpected Forge database privileges"
                                )
                        finally:
                            self.cluster.admin_exec(
                                f"REVOKE ALL ON forge_agent.{table} FROM {grantee}",
                                f"GRANT SELECT ON forge_agent.{table} TO forge_agent",
                            )
                        self.assert_sql_verification(script, user="forge_agent")
        finally:
            self.cluster.admin_exec(
                f"REVOKE {inherited} FROM forge_agent",
                f"DROP ROLE {inherited}",
                owner=False,
            )

    def test_gpu_worker_cannot_gain_column_privileges_directly_or_by_inheritance(self):
        script = SQL_DIR / "management/verify.sql"
        inherited = "forge_test_work_gpu_grants"
        self.cluster.admin_exec(
            f"CREATE ROLE {inherited} NOLOGIN",
            f"GRANT {inherited} TO forge_gpu_worker WITH INHERIT TRUE",
            owner=False,
        )
        try:
            for table, column in (
                ("coding_tasks", "prompt"),
                ("coding_events", "payload"),
                ("coding_control_receipts", "receipt_json"),
            ):
                for grantee in ("forge_gpu_worker", inherited, "PUBLIC"):
                    for privilege in ("SELECT", "INSERT", "UPDATE", "REFERENCES"):
                        with self.subTest(table=table, grantee=grantee, privilege=privilege):
                            try:
                                self.cluster.admin_exec(
                                    f"GRANT {privilege} ({column}) ON forge_agent.{table} TO {grantee}"
                                )
                                error = (
                                    "Unexpected PUBLIC privileges" if grantee == "PUBLIC"
                                    else "Unexpected Forge GPU worker privileges"
                                )
                                self.assert_sql_verification(script, error=error)
                            finally:
                                self.cluster.admin_exec(
                                    f"REVOKE {privilege} ({column}) ON forge_agent.{table} FROM {grantee}"
                                )
                            self.assert_sql_verification(script)
        finally:
            self.cluster.admin_exec(
                f"REVOKE {inherited} FROM forge_gpu_worker",
                f"DROP ROLE {inherited}",
                owner=False,
            )

    def test_verify_rejects_same_named_weakened_constraints(self):
        script = SQL_DIR / "management/verify.sql"
        drifts = (
            ("coding_tasks", "coding_tasks_idempotency_unique",
             "UNIQUE (owner_id, project_alias, idempotency_key, id)"),
            ("coding_events", "coding_events_task_fk",
             "FOREIGN KEY (task_id) REFERENCES forge_agent.coding_tasks(id)"),
            ("coding_control_receipts", "coding_control_receipts_task_fk",
             "FOREIGN KEY (task_id) REFERENCES forge_agent.coding_tasks(id)"),
            ("coding_tasks", "coding_tasks_revision_check", "CHECK (revision >= 1)"),
            ("coding_tasks", "coding_tasks_status_check",
             "CHECK (status IN ('queued', 'cancelled', 'running'))"),
            ("coding_tasks", "coding_tasks_status_revision_check", "CHECK (true)"),
            ("coding_tasks", "coding_tasks_pinned_recipe_check",
             "CHECK (jsonb_typeof(pinned_recipe) = 'object')"),
            ("coding_tasks", "coding_tasks_recipe_mode_check", "CHECK (true)"),
            ("coding_events", "coding_events_sequence_check", "CHECK (sequence >= 1)"),
            ("coding_control_receipts", "coding_control_receipts_revision_step_check",
             "CHECK (result_revision >= base_revision)"),
        )
        for table, constraint, drift in drifts:
            with self.subTest(constraint=constraint):
                with closing(self.cluster.connect("postgres")) as connection, connection.cursor() as cursor:
                    cursor.execute(
                        "SELECT pg_get_constraintdef(oid, false) FROM pg_constraint "
                        "WHERE conrelid = %s::regclass AND conname = %s",
                        (f"forge_agent.{table}", constraint),
                    )
                    original = cursor.fetchone()[0]
                try:
                    self.cluster.admin_exec(
                        f"ALTER TABLE forge_agent.{table} DROP CONSTRAINT {constraint}",
                        f"ALTER TABLE forge_agent.{table} ADD CONSTRAINT {constraint} {drift}",
                    )
                    self.assert_sql_verification(script, error=constraint)
                finally:
                    self.cluster.admin_exec(
                        f"ALTER TABLE forge_agent.{table} DROP CONSTRAINT {constraint}",
                        f"ALTER TABLE forge_agent.{table} ADD CONSTRAINT {constraint} {original}",
                    )
                self.assert_sql_verification(script)

    def test_verify_rejects_same_named_index_definition_drift(self):
        script = SQL_DIR / "management/verify.sql"
        name = "coding_tasks_by_project_created"
        drifts = (
            "ON forge_agent.coding_tasks (project_alias, owner_id, created_at, id)",
            "ON forge_agent.coding_tasks (owner_id, project_alias, created_at DESC, id)",
            "ON forge_agent.coding_tasks (owner_id, project_alias, created_at, id) WHERE status = 'queued'",
            "ON forge_agent.coding_tasks (owner_id, lower(project_alias), created_at, id)",
            "ON forge_agent.coding_tasks (owner_id, project_alias, created_at, id) INCLUDE (prompt)",
            "ON forge_agent.coding_events (owner_id, project_alias, created_at, task_id)",
        )
        with closing(self.cluster.connect("postgres")) as connection, connection.cursor() as cursor:
            cursor.execute("SELECT pg_get_indexdef(%s::regclass, 0, false)", (f"forge_agent.{name}",))
            original = cursor.fetchone()[0]
        for drift in drifts:
            with self.subTest(definition=drift):
                try:
                    self.cluster.admin_exec(
                        f"DROP INDEX forge_agent.{name}", f"CREATE INDEX {name} {drift}"
                    )
                    self.assert_sql_verification(script, error=name)
                finally:
                    self.cluster.admin_exec(f"DROP INDEX forge_agent.{name}", original)
                self.assert_sql_verification(script)

    def test_schema_constraints_and_runner_immutability_grants(self):
        service = self.make_service()
        receipt = service.submit(make_submit())
        task_id = UUID(receipt["task_id"])

        with closing(self.cluster.connect("forge_agent")) as conn:
            with conn.cursor() as cur:
                # Non-null generation_token_limit rejected by CHECK.
                with self.assertRaises(psycopg2.IntegrityError):
                    cur.execute("""
                        INSERT INTO forge_agent.coding_tasks (
                            owner_id, project_alias, id, idempotency_key, intent_digest,
                            base_revision, mode, agent_profile, prompt, acceptance_criteria,
                            pinned_recipe, recipe_digest, status, revision
                        )
                        VALUES (
                            'mission-control', 'homelab', %s, 'bad-token', %s,
                            %s, 'plan', 'agy-plan', 'p', 'a',
                            '{"version":1,"backend":"codex","model":"m","resource_profile":"r","mode":"plan","generation_token_limit":256}'::jsonb,
                            %s, 'queued', 1
                        )
                    """, (uuid4(), "a" * 64, BASE_REV, "b" * 64))
                conn.rollback()

                # Extra secret key in pinned_recipe rejected by CHECK.
                with self.assertRaises(psycopg2.IntegrityError):
                    cur.execute("""
                        INSERT INTO forge_agent.coding_tasks (
                            owner_id, project_alias, id, idempotency_key, intent_digest,
                            base_revision, mode, agent_profile, prompt, acceptance_criteria,
                            pinned_recipe, recipe_digest, status, revision
                        )
                        VALUES (
                            'mission-control', 'homelab', %s, 'bad-secret', %s,
                            %s, 'plan', 'agy-plan', 'p', 'a',
                            '{"version":1,"backend":"codex","model":"m","resource_profile":"r","mode":"plan","generation_token_limit":null,"env":"secret"}'::jsonb,
                            %s, 'queued', 1
                        )
                    """, (uuid4(), "a" * 64, BASE_REV, "b" * 64))
                conn.rollback()

                # Runner role cannot DELETE coding_tasks or UPDATE coding_events / coding_control_receipts.
                for sql, params in [
                    ("DELETE FROM forge_agent.coding_tasks WHERE id = %s", (task_id,)),
                    ("UPDATE forge_agent.coding_events SET event_kind = 'cancelled' WHERE task_id = %s", (task_id,)),
                    ("DELETE FROM forge_agent.coding_events WHERE task_id = %s", (task_id,)),
                    ("UPDATE forge_agent.coding_control_receipts SET action = 'cancel' WHERE task_id = %s", (task_id,)),
                ]:
                    with self.assertRaises(psycopg2.errors.InsufficientPrivilege):
                        cur.execute(sql, params)
                    conn.rollback()
        self.assert_all_connections_closed()

    def test_submit_commits_task_event_empty_notify_and_get_omits_recipe_body(self):
        service = self.make_service()
        with (closing(self.cluster.connect("forge_agent")) as listener,
              patch("subprocess.Popen", side_effect=AssertionError("no agent subprocess")),
              patch("subprocess.run", side_effect=AssertionError("no agent subprocess"))):
            listener.autocommit = True
            with listener.cursor() as cur:
                cur.execute(f"LISTEN {NOTIFY_CHANNEL}")

            receipt = service.submit(make_submit())
            notifies = drain_notifications(listener)
            self.assertEqual(len(notifies), 1)
            self.assertEqual(notifies[0].channel, NOTIFY_CHANNEL)
            self.assertEqual(notifies[0].payload, "")

            self.assertEqual(receipt["project_alias"], "homelab")
            self.assertEqual(receipt["revision"], 1)
            self.assertEqual(receipt["status"], "queued")
            self.assertRegex(receipt["recipe_digest"], r"^[0-9a-f]{64}$")
            task_id = UUID(receipt["task_id"])

            opened = service.get(work_api.WorkGet("homelab", task_id))
            self.assertEqual(opened["task_id"], str(task_id))
            self.assertEqual(opened["prompt"], "Draft plan")
            self.assertEqual(opened["acceptance_criteria"], "Bounded output")
            self.assertEqual(opened["recipe_digest"], receipt["recipe_digest"])
            self.assertNotIn("pinned_recipe", opened)
            self.assertNotIn("recipe", opened)
            self.assertTrue(opened["created_at"].endswith("Z"))
            self.assertLessEqual(len(opened["created_at"].encode("utf-8")), 80)
            datetime.fromisoformat(opened["created_at"].replace("Z", "+00:00"))

            events = service.events(work_api.WorkEvents("homelab", task_id, 0, 50))
            self.assertEqual(events["revision"], 1)
            self.assertEqual(events["status"], "queued")
            self.assertEqual(events["next_sequence"], 1)
            self.assertEqual(len(events["events"]), 1)
            self.assertEqual(events["events"][0]["sequence"], 1)
            self.assertEqual(events["events"][0]["kind"], "queued")

        self.assert_all_connections_closed()

    def test_concurrent_duplicate_submit_admits_only_one_task_and_event(self):
        service = self.make_service()
        workers = 6
        barrier = threading.Barrier(workers)
        results = []
        errors = []
        lock = threading.Lock()

        def run_one():
            try:
                barrier.wait(timeout=5)
                res = service.submit(make_submit(idempotency_key="concurrent-dup"))
                with lock:
                    results.append(res)
            except BaseException as exc:
                with lock:
                    errors.append(exc)

        threads = [threading.Thread(target=run_one) for _ in range(workers)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(timeout=15)

        self.assertEqual(errors, [])
        self.assertEqual(len(results), workers)
        self.assertEqual(len({item["task_id"] for item in results}), 1)
        self.assertEqual(len({item["recipe_digest"] for item in results}), 1)

        with closing(self.cluster.connect("forge_agent")) as conn, conn.cursor() as cur:
            cur.execute("SELECT count(*) FROM forge_agent.coding_tasks")
            self.assertEqual(cur.fetchone()[0], 1)
            cur.execute("SELECT count(*) FROM forge_agent.coding_events")
            self.assertEqual(cur.fetchone()[0], 1)
        self.assert_all_connections_closed()

    def test_capacity_concurrency_changed_intent_and_replay_before_full(self):
        service = self.make_service(max_pending=2)
        workers = 6
        barrier = threading.Barrier(workers)
        admitted = []
        rejected = []
        lock = threading.Lock()

        def run_one(idx):
            try:
                barrier.wait(timeout=5)
                res = service.submit(make_submit(idempotency_key=f"cap-{idx}"))
                with lock:
                    admitted.append((f"cap-{idx}", res))
            except Rejected as exc:
                with lock:
                    rejected.append((exc.status, exc.code))

        threads = [threading.Thread(target=run_one, args=(i,)) for i in range(workers)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(timeout=15)

        self.assertEqual(len(admitted), 2)
        self.assertEqual(rejected, [(503, "queue_full")] * 4)

        # Replay of an already admitted key succeeds even while queue is full.
        key_0, receipt_0 = admitted[0]
        self.assertEqual(service.submit(make_submit(idempotency_key=key_0)), receipt_0)

        # Same key with changed user intent fails 409 idempotency_conflict.
        with self.assertRaises(Rejected) as ctx:
            service.submit(make_submit(idempotency_key=key_0, prompt="Changed prompt"))
        self.assertEqual((ctx.exception.status, ctx.exception.code), (409, "idempotency_conflict"))

        # Cancelling one queued task frees capacity for a new admission.
        service.cancel(work_api.WorkCancel("homelab", UUID(receipt_0["task_id"]), 1, "free-slot"))
        third = service.submit(make_submit(idempotency_key="cap-after-cancel"))
        self.assertEqual(third["status"], "queued")
        self.assert_all_connections_closed()

    def test_submit_replay_immutable_after_cancel_and_catalog_drift_and_revoked_grants_denied(self):
        service_v1 = self.make_service(agy_model="gemini-model-v1")
        original = service_v1.submit(make_submit(idempotency_key="drift-key"))
        task_id = UUID(original["task_id"])

        service_v1.cancel(work_api.WorkCancel("homelab", task_id, 1, "cancel-drift"))
        opened = service_v1.get(work_api.WorkGet("homelab", task_id))
        self.assertEqual((opened["status"], opened["revision"]), ("cancelled", 2))

        # Drift catalog to model-v2 and verify replay rereads original receipt without resolving recipe.
        config_v2 = make_config(agy_model="gemini-model-v2")
        catalog_v2 = config_v2.catalog()
        store_v2 = WorkStore(
            config_v2.owner, config_v2.allowed_projects, self.tracked_connect,
            max_pending=config_v2.max_pending,
        )
        with patch.object(catalog_v2, "resolve", wraps=catalog_v2.resolve) as resolve_spy:
            service_v2 = work_api.WorkService(config_v2, catalog_v2, store_v2)
            replayed = service_v2.submit(make_submit(idempotency_key="drift-key"))
            self.assertEqual(replayed, original)
            self.assertEqual(replayed["status"], "queued")
            self.assertEqual(replayed["revision"], 1)
            resolve_spy.assert_not_called()

            fresh = service_v2.submit(make_submit(idempotency_key="fresh-v2"))
            self.assertNotEqual(fresh["recipe_digest"], original["recipe_digest"])
            resolve_spy.assert_called_once_with("agy-plan", "plan")

        # Revoked project, mode, or profile must deny retries before opening a DB connection.
        no_connect = Mock(side_effect=AssertionError("must not connect when grant revoked"))
        for bad_cfg in [
            make_config(projects=("docs",)),
            make_config(modes=("review",)),
            work_config.parse_work_config({
                "owner": "mission-control",
                "allowed_projects": ["homelab"],
                "allowed_modes": ["plan"],
                "profiles": {
                    "other-profile": {
                        "backend": "codex", "model": "gpt-5.4", "resource_profile": "r1",
                    }
                },
            }),
        ]:
            revoked_service = work_api.WorkServiceFactory(bad_cfg, no_connect).create()
            with self.assertRaises(Rejected) as ctx:
                revoked_service.submit(make_submit(idempotency_key="drift-key"))
            self.assertEqual((ctx.exception.status, ctx.exception.code), (403, "denied"))
        no_connect.assert_not_called()
        self.assert_all_connections_closed()

    def test_owner_and_project_isolation_across_reads_lists_events_cancel_and_cursors(self):
        owner_a = self.make_service(owner="owner-a", projects=("proj-a", "proj-b"))
        owner_b = self.make_service(owner="owner-b", projects=("proj-a", "proj-b"))

        task_a = UUID(owner_a.submit(make_submit(project_alias="proj-a", idempotency_key="a-1"))["task_id"])
        task_b = UUID(owner_a.submit(make_submit(project_alias="proj-b", idempotency_key="b-1"))["task_id"])

        # Cross-owner queries return 404; cross-owner cursor returns 400.
        for fn in [
            lambda: owner_b.get(work_api.WorkGet("proj-a", task_a)),
            lambda: owner_b.events(work_api.WorkEvents("proj-a", task_a, 0, 50)),
            lambda: owner_b.cancel(work_api.WorkCancel("proj-a", task_a, 1, "c-cross-owner")),
        ]:
            with self.assertRaises(Rejected) as ctx:
                fn()
            self.assertEqual((ctx.exception.status, ctx.exception.code), (404, "not_found"))

        self.assertEqual(
            owner_b.list_tasks(work_api.WorkList("proj-a", None, 20, None)),
            {"project_alias": "proj-a", "tasks": [], "next_cursor": None},
        )
        with self.assertRaises(Rejected) as ctx:
            owner_b.list_tasks(work_api.WorkList("proj-a", None, 20, task_a))
        self.assertEqual((ctx.exception.status, ctx.exception.code), (400, "invalid_request"))

        # Cross-project queries return 404; cross-project and unknown cursors return 400.
        for fn in [
            lambda: owner_a.get(work_api.WorkGet("proj-b", task_a)),
            lambda: owner_a.events(work_api.WorkEvents("proj-b", task_a, 0, 50)),
            lambda: owner_a.cancel(work_api.WorkCancel("proj-b", task_a, 1, "c-cross-proj")),
        ]:
            with self.assertRaises(Rejected) as ctx:
                fn()
            self.assertEqual((ctx.exception.status, ctx.exception.code), (404, "not_found"))

        for bad_cursor in (task_a, uuid4()):
            with self.assertRaises(Rejected) as ctx:
                owner_a.list_tasks(work_api.WorkList("proj-b", None, 20, bad_cursor))
            self.assertEqual((ctx.exception.status, ctx.exception.code), (400, "invalid_request"))

        self.assertEqual([t["task_id"] for t in owner_a.list_tasks(
            work_api.WorkList("proj-b", None, 20, None)
        )["tasks"]], [str(task_b)])

        # Direct WorkStore enforces project allowlist before opening a connection.
        no_connect = Mock(side_effect=AssertionError("must not connect"))
        direct_store = WorkStore("owner-a", frozenset({"proj-a"}), no_connect)
        with self.assertRaises(Rejected) as ctx:
            direct_store.list_tasks(project_alias="proj-b", limit=999)
        self.assertEqual((ctx.exception.status, ctx.exception.code), (403, "denied"))
        no_connect.assert_not_called()
        self.assert_all_connections_closed()

    def test_list_ordering_keyset_pagination_and_status_filter(self):
        service = self.make_service()
        task_ids = [
            UUID(service.submit(make_submit(idempotency_key=f"page-{i}"))["task_id"])
            for i in range(5)
        ]
        # Force identical created_at on two rows to exercise (created_at ASC, id ASC) tie-break.
        shared_ts = datetime(2026, 11, 19, 12, 0, 0, tzinfo=UTC)
        with closing(self.cluster.connect("postgres")) as conn:
            with conn, conn.cursor() as cur:
                cur.execute(
                    "UPDATE forge_agent.coding_tasks SET created_at = %s WHERE id IN (%s, %s)",
                    (shared_ts, task_ids[0], task_ids[1]),
                )

        with closing(self.cluster.connect("forge_agent")) as conn, conn.cursor() as cur:
            cur.execute("""
                SELECT id FROM forge_agent.coding_tasks
                WHERE owner_id = 'mission-control' AND project_alias = 'homelab'
                ORDER BY created_at ASC, id ASC
            """)
            expected_order = [str(row[0]) for row in cur.fetchall()]

        collected = []
        cursor = None
        pages = 0
        while True:
            page = service.list_tasks(work_api.WorkList("homelab", None, 2, cursor))
            pages += 1
            collected.extend(item["task_id"] for item in page["tasks"])
            if page["next_cursor"] is None:
                break
            self.assertEqual(page["next_cursor"], page["tasks"][-1]["task_id"])
            cursor = UUID(page["next_cursor"])

        self.assertEqual(pages, 3)
        self.assertEqual(collected, expected_order)

        # Status filtering and boundary task cancellation between pages.
        service.cancel(work_api.WorkCancel("homelab", UUID(expected_order[0]), 1, "c0"))
        service.cancel(work_api.WorkCancel("homelab", UUID(expected_order[1]), 1, "c1"))

        queued_page = service.list_tasks(work_api.WorkList("homelab", "queued", 50, None))
        self.assertEqual([t["task_id"] for t in queued_page["tasks"]], expected_order[2:])
        self.assertIsNone(queued_page["next_cursor"])

        cancelled_page = service.list_tasks(work_api.WorkList("homelab", "cancelled", 1, None))
        self.assertEqual([t["task_id"] for t in cancelled_page["tasks"]], [expected_order[0]])
        self.assertEqual(cancelled_page["next_cursor"], expected_order[0])

        # Anchor cursor still works cleanly after its task changed status.
        page_after_cancelled = service.list_tasks(
            work_api.WorkList("homelab", "queued", 2, UUID(expected_order[1]))
        )
        self.assertEqual([t["task_id"] for t in page_after_cancelled["tasks"]], expected_order[2:4])
        self.assert_all_connections_closed()

    def test_cancel_cas_replays_conflicts_events_and_concurrency(self):
        service = self.make_service()
        task_id = UUID(service.submit(make_submit(idempotency_key="cancel-target"))["task_id"])

        # Wrong expected_revision on queued task -> 409 revision_conflict.
        with self.assertRaises(Rejected) as ctx:
            service.cancel(work_api.WorkCancel("homelab", task_id, 2, "wrong-rev"))
        self.assertEqual((ctx.exception.status, ctx.exception.code), (409, "revision_conflict"))

        # Valid queued cancel -> 200 immutable receipt.
        receipt = service.cancel(work_api.WorkCancel("homelab", task_id, 1, "cancel-k1"))
        self.assertEqual(receipt, {
            "project_alias": "homelab",
            "task_id": str(task_id),
            "base_revision": 1,
            "revision": 2,
            "status": "cancelled",
        })

        # Same key + same intent replays original business receipt BEFORE CAS/state checks.
        self.assertEqual(
            service.cancel(work_api.WorkCancel("homelab", task_id, 1, "cancel-k1")),
            receipt,
        )

        # Same key + changed expected_revision -> 409 idempotency_conflict.
        with self.assertRaises(Rejected) as ctx:
            service.cancel(work_api.WorkCancel("homelab", task_id, 2, "cancel-k1"))
        self.assertEqual((ctx.exception.status, ctx.exception.code), (409, "idempotency_conflict"))

        # New key on already-cancelled task -> 409 not_cancellable (for either revision).
        for rev in (1, 2):
            with self.assertRaises(Rejected) as ctx:
                service.cancel(work_api.WorkCancel("homelab", task_id, rev, f"new-key-{rev}"))
            self.assertEqual((ctx.exception.status, ctx.exception.code), (409, "not_cancellable"))

        # Monotonic event pagination.
        ev1 = service.events(work_api.WorkEvents("homelab", task_id, 0, 1))
        self.assertEqual(ev1["next_sequence"], 1)
        self.assertEqual([(e["sequence"], e["kind"]) for e in ev1["events"]], [(1, "queued")])

        ev2 = service.events(work_api.WorkEvents("homelab", task_id, 1, 50))
        self.assertEqual(ev2["next_sequence"], 2)
        self.assertEqual((ev2["revision"], ev2["status"]), (2, "cancelled"))
        self.assertEqual([(e["sequence"], e["kind"]) for e in ev2["events"]], [(2, "cancelled")])

        ev_empty = service.events(work_api.WorkEvents("homelab", task_id, 2, 50))
        self.assertEqual(ev_empty["events"], [])
        self.assertEqual(ev_empty["next_sequence"], 2)

        # Concurrent cancel race with distinct keys allows only one winner.
        race_id = UUID(service.submit(make_submit(idempotency_key="cancel-race"))["task_id"])
        barrier = threading.Barrier(4)
        winners = []
        losers = []
        lock = threading.Lock()

        def race(idx):
            try:
                barrier.wait(timeout=5)
                res = service.cancel(work_api.WorkCancel("homelab", race_id, 1, f"race-key-{idx}"))
                with lock:
                    winners.append(res)
            except Rejected as exc:
                with lock:
                    losers.append((exc.status, exc.code))

        threads = [threading.Thread(target=race, args=(i,)) for i in range(4)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(timeout=15)

        self.assertEqual(len(winners), 1)
        self.assertEqual(losers, [(409, "not_cancellable")] * 3)
        self.assert_all_connections_closed()

    def test_rollback_commits_no_rows_and_emits_no_notify_on_failure(self):
        class FailingCommitConnection:
            def __init__(self, inner):
                self._inner = inner

            def __enter__(self):
                self._inner.__enter__()
                return self

            def __exit__(self, exc_type, exc, tb):
                if exc_type is None:
                    self._inner.rollback()
                    raise psycopg2.OperationalError("simulated commit failure")
                return self._inner.__exit__(exc_type, exc, tb)

            def cursor(self, *args, **kwargs):
                return self._inner.cursor(*args, **kwargs)

            def close(self):
                return self._inner.close()

            @property
            def closed(self):
                return self._inner.closed

        def failing_connect():
            conn = FailingCommitConnection(self.cluster.connect("forge_agent"))
            self.opened_connections.append(conn)
            return conn

        config = make_config()
        failing_service = work_api.WorkServiceFactory(config, failing_connect).create()

        with closing(self.cluster.connect("forge_agent")) as listener:
            listener.autocommit = True
            with listener.cursor() as cur:
                cur.execute(f"LISTEN {NOTIFY_CHANNEL}")

            with self.assertRaises(psycopg2.OperationalError):
                failing_service.submit(make_submit(idempotency_key="rollback-key"))

            self.assertEqual(drain_notifications(listener, timeout=0.3), [])

        with closing(self.cluster.connect("forge_agent")) as conn, conn.cursor() as cur:
            cur.execute("SELECT count(*) FROM forge_agent.coding_tasks")
            self.assertEqual(cur.fetchone()[0], 0)
            cur.execute("SELECT count(*) FROM forge_agent.coding_events")
            self.assertEqual(cur.fetchone()[0], 0)
        self.assert_all_connections_closed()


if __name__ == "__main__":
    unittest.main()
