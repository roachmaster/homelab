"""Disposable PostgreSQL 16 integration tests for coding execution, reconciliation, ACLs, and usage."""

from contextlib import closing
from datetime import UTC, datetime, timedelta
import hashlib
import json
import threading
import unittest
from uuid import UUID, uuid4

import psycopg2

from queue_store import Rejected
from usage_store import UsageStore
import work_api
import work_config
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE, SQL_DIR
import work_policy
from work_worker_store import (
    Lease,
    LeaseExpiredError,
    OutcomeConflictError,
    ResultMismatchError,
    WorkerAccessError,
    WorkerDB,
)

BASE_REV = "2e1300a00da92e9f97bdb1a656c2b182d4219a6c"
WORKSPACE_DIGEST = "c" * 64
HMAC_A = "1" * 64
HMAC_B = "2" * 64


def make_v2_config(*, owner="mission-control", projects=("homelab", "docs"),
                   modes=("plan", "review", "implement"), max_pending=32):
    return work_config.parse_work_config({
        "owner": owner,
        "allowed_projects": list(projects),
        "allowed_modes": list(modes),
        "max_pending": max_pending,
        "project_labels": {projects[0]: f"{projects[0].title()} Project"},
        "profiles": {
            "codex-v2": {
                "backend": "codex",
                "model": "gpt-5.4",
                "resource_profile": work_policy.POLICY_ID,
                "label": "Codex GPT-5.4",
            },
            "agy-v2": {
                "backend": "antigravity",
                "model": "gemini-2.5-pro",
                "resource_profile": work_policy.POLICY_ID,
                "label": "Antigravity Gemini",
            },
            "legacy-v1": {
                "backend": "codex",
                "model": "gpt-5.4",
                "resource_profile": "legacy-plan-v1",
            },
        },
    })


def make_submit(*, project_alias="homelab", base_revision=BASE_REV, mode="plan",
                agent_profile="codex-v2", prompt="Implement durable execution\nLine 2 details",
                acceptance_criteria="Verify all invariants", idempotency_key="exec-1"):
    return work_api.WorkSubmit(
        project_alias=project_alias,
        base_revision=base_revision,
        mode=mode,
        agent_profile=agent_profile,
        prompt=prompt,
        acceptance_criteria=acceptance_criteria,
        idempotency_key=idempotency_key,
    )


def make_stop_proof(unit_name, *, stopped=True):
    return {
        "unit_name": unit_name,
        "scope_stopped": stopped,
        "source": "systemd",
        "checked_at": datetime.now(UTC).isoformat().replace("+00:00", "Z"),
    }


_UNSET = object()


def make_snapshot(*, state="fresh", observed_at=_UNSET, source="codex-rpc", version="0.160.0",
                  last_error=None, account_identity=HMAC_A, allowance_state="available",
                  used_percent=42.5, lifetime_tokens="999999999999999999999999"):
    if observed_at is _UNSET:
        observed_at = datetime.now(UTC).isoformat().replace("+00:00", "Z")
    return {
        "snapshot_state": state,
        "observed_at": observed_at,
        "source": source,
        "version": version,
        "last_error": last_error,
        "allowance": {
            "state": allowance_state,
            "reason": None,
            "ordinary_usage_allowed": True if allowance_state == "available" else None,
            "buckets": [
                {
                    "key": "primary",
                    "label": "5h Window",
                    "used_percent": used_percent,
                    "window_minutes": 300,
                    "resets_at": observed_at,
                }
            ] if allowance_state == "available" else [],
        },
        "account_activity": {
            "state": "available",
            "lifetime_tokens": lifetime_tokens,
            "daily": [{"day": "2026-11-19", "tokens": "12345"}],
        },
        "account_identity": account_identity,
    }


@unittest.skipUnless(
    PG16_AVAILABLE,
    "Disposable PostgreSQL 16 integration requires its binaries and a non-root user",
)
class WorkExecutionAndUsagePostgresTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def setUp(self):
        self.cluster.reset_coding_tables()
        self.opened = []

    def _connect(self, user):
        conn = self.cluster.connect(user)
        self.opened.append(conn)
        return conn

    def assert_all_connections_closed(self):
        self.assertTrue(
            all(conn.closed != 0 for conn in self.opened),
            "Every per-operation database connection must be closed",
        )

    def make_service(self, **cfg_kwargs):
        config = make_v2_config(**cfg_kwargs)
        return work_api.WorkServiceFactory(config, lambda: self._connect("forge_agent")).create(), config

    def make_worker_db(self, *, owner="mission-control", with_reconciler=True):
        return WorkerDB(
            lambda: self._connect("forge_coding_worker"),
            owner,
            reconcile_connect_factory=(
                (lambda: self._connect("forge_coding_reconciler")) if with_reconciler else None
            ),
        )

    def grant_default_worker(self, *, owner="mission-control", projects=("homelab",),
                             profiles=("codex-v2", "agy-v2"), modes=("plan", "review", "implement")):
        for project in projects:
            for profile in profiles:
                self.cluster.grant_worker(owner, project, profile, modes)

    def advertise_default_worker(self, db, *, worker_id="worker-1", incarnation=None,
                                 projects=None, profiles=None, policy_digest=work_policy.POLICY_DIGEST):
        inc = incarnation or str(uuid4())
        if projects is None:
            projects = [{"alias": "homelab", "base_revision": BASE_REV}]
        if profiles is None:
            profiles = [
                {"alias": "codex-v2", "backend": "codex", "model": "gpt-5.4", "modes": ["plan", "review", "implement"]},
                {"alias": "agy-v2", "backend": "antigravity", "model": "gemini-2.5-pro", "modes": ["plan", "review"]},
            ]
        db.advertise(worker_id, inc, projects, profiles, policy_digest)
        return inc

    def assert_sql_verify(self, script, *, user="postgres", error=None):
        result = self.cluster.run_sql_script(script, user=user)
        if error is None:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn(error, result.stderr)

    def test_worker_and_reconciler_exec_only_acls_direct_inherited_public_column(self):
        verify_script = SQL_DIR / "management/verify.sql"
        self.assert_sql_verify(verify_script)

        with closing(self.cluster.connect("postgres")) as conn, conn.cursor() as cur:
            cur.execute("SELECT name FROM forge_agent.schema_migrations WHERE version=54")
            self.assertEqual(cur.fetchall(), [("pg_proxy_preparation",)])
        # M54 permits bounded direct reads during preparation, never direct writes.
        for role, tables in (
            ("forge_coding_worker", ("coding_tasks", "coding_attempts", "coding_worker_grants")),
            ("forge_coding_reconciler", ("coding_tasks", "coding_attempts")),
            ("forge_agent", ("coding_worker_grants",)),
        ):
            with closing(self.cluster.connect(role)) as conn, conn.cursor() as cur:
                for table in tables:
                    with self.subTest(role=role, table=table):
                        cur.execute("SELECT count(*) FROM forge_agent." + table)
                        self.assertIsInstance(cur.fetchone()[0], int)
                        cur.execute("SELECT has_table_privilege(current_user,%s,'INSERT,UPDATE,DELETE,TRUNCATE')",
                                    ("forge_agent." + table,))
                        self.assertFalse(cur.fetchone()[0])

        # Coding worker cannot read private usage identities or invoke reconciler/internal helpers.
        with closing(self.cluster.connect("forge_coding_worker")) as conn:
            with conn.cursor() as cur:
                for sql in (
                    "SELECT count(*) FROM forge_agent.coding_provider_usage_identities",
                    "SELECT forge_coding_reconciler_api.expire_leases()",
                    "SELECT forge_coding_reconciler_api.pending_reconciliations(10)",
                    "SELECT forge_coding_worker_api._validate_usage_json('{}'::jsonb)",
                    "SELECT forge_agent_worker_api.claim('gpt-oss-20b')",
                ):
                    with self.assertRaises(psycopg2.errors.InsufficientPrivilege):
                        cur.execute(sql)
                    conn.rollback()

        # Reconciler still cannot call worker claim/publish.
        with closing(self.cluster.connect("forge_coding_reconciler")) as conn:
            with conn.cursor() as cur:
                for sql in (
                    "SELECT forge_coding_worker_api.next_refresh()",
                    "SELECT forge_agent_worker_api.claim('gpt-oss-20b')",
                ):
                    with self.assertRaises(psycopg2.errors.InsufficientPrivilege):
                        cur.execute(sql)
                    conn.rollback()

        # Runner still cannot read internal HMAC account identities or call worker helpers.
        with closing(self.cluster.connect("forge_agent")) as conn:
            with conn.cursor() as cur:
                for sql in (
                    "SELECT count(*) FROM forge_agent.coding_provider_usage_identities",
                    "SELECT forge_coding_worker_api.next_refresh()",
                    "SELECT forge_coding_reconciler_api.expire_leases()",
                ):
                    with self.assertRaises(psycopg2.errors.InsufficientPrivilege):
                        cur.execute(sql)
                    conn.rollback()

        # Verify catches direct, column-only, inherited, and PUBLIC drift for worker & reconciler.
        inherited = "forge_test_coding_inherited"
        self.cluster.admin_exec(f"CREATE ROLE {inherited} NOLOGIN", owner=False)
        try:
            for role, expected_err in (
                ("forge_coding_worker", "Unexpected Forge coding worker privileges"),
                ("forge_coding_reconciler", "Unexpected Forge coding reconciler privileges"),
            ):
                self.cluster.admin_exec(f"GRANT {inherited} TO {role} WITH INHERIT TRUE", owner=False)
                try:
                    for grantee, err in ((role, expected_err), (inherited, expected_err), ("PUBLIC", "Unexpected PUBLIC privileges")):
                        # A redundant column SELECT on coding_attempts is permitted
                        # by M54. Private identity reads and any column writes are not.
                        for privilege, table in (
                            ("SELECT (account_identity)", "coding_provider_usage_identities"),
                            ("UPDATE (attempt_token)", "coding_attempts"),
                        ):
                            with self.subTest(role=role, grantee=grantee, privilege=privilege):
                                try:
                                    self.cluster.admin_exec(
                                        f"GRANT {privilege} ON forge_agent.{table} TO {grantee}"
                                    )
                                    self.assert_sql_verify(verify_script, error=err)
                                finally:
                                    self.cluster.admin_exec(
                                        f"REVOKE {privilege} ON forge_agent.{table} FROM {grantee}"
                                    )
                                self.assert_sql_verify(verify_script)
                finally:
                    self.cluster.admin_exec(f"REVOKE {inherited} FROM {role}", owner=False)

            # Granting reconciler execute to worker must fail verification.
            try:
                self.cluster.admin_exec(
                    "GRANT EXECUTE ON FUNCTION forge_coding_reconciler_api.expire_leases() TO forge_coding_worker"
                )
                self.assert_sql_verify(verify_script, error="Unexpected Forge coding worker privileges")
            finally:
                self.cluster.admin_exec(
                    "REVOKE EXECUTE ON FUNCTION forge_coding_reconciler_api.expire_leases() FROM forge_coding_worker"
                )
            self.assert_sql_verify(verify_script)
        finally:
            self.cluster.admin_exec(f"DROP ROLE {inherited}", owner=False)

    def test_advertise_grant_intersection_cross_owner_deny_and_catalog_freshness(self):
        service_a, _ = self.make_service(owner="owner-a", projects=("homelab", "docs"), modes=("plan", "review"))
        service_b, _ = self.make_service(owner="owner-b", projects=("homelab",), modes=("plan",))
        db = self.make_worker_db(owner="owner-a")

        # Unconfigured worker with no grants is denied.
        with self.assertRaises(WorkerAccessError):
            self.advertise_default_worker(db)

        # Catalog before any advertisement reports worker_status=unknown and empty profiles.
        cat_unknown = service_a.catalog()
        self.assertEqual(cat_unknown["worker_status"], "unknown")
        self.assertIsNone(cat_unknown["observed_at"])
        self.assertEqual(cat_unknown["profiles"], [])
        self.assertEqual(cat_unknown["projects"][0]["base_revision"], None)

        # Grant worker ONLY owner-a / homelab / codex-v2 / plan.
        self.cluster.grant_worker("owner-a", "homelab", "codex-v2", ("plan",))
        self.advertise_default_worker(
            db,
            projects=[
                {"alias": "homelab", "base_revision": BASE_REV},
                {"alias": "docs", "base_revision": BASE_REV},
            ],
            profiles=[
                {"alias": "codex-v2", "backend": "codex", "model": "gpt-5.4", "modes": ["plan", "implement"]},
                {"alias": "agy-v2", "backend": "antigravity", "model": "gemini-2.5-pro", "modes": ["plan"]},
            ],
        )

        cat_a = service_a.catalog()
        self.assertEqual(cat_a["worker_status"], "ready")
        self.assertIsNotNone(cat_a["observed_at"])
        proj_map = {p["alias"]: p for p in cat_a["projects"]}
        self.assertEqual(proj_map["homelab"]["base_revision"], BASE_REV)
        self.assertEqual(proj_map["homelab"]["label"], "Homelab Project")
        self.assertIsNone(proj_map["docs"]["base_revision"])
        self.assertEqual(len(cat_a["profiles"]), 1)
        self.assertEqual(cat_a["profiles"][0]["alias"], "codex-v2")
        self.assertEqual(cat_a["profiles"][0]["modes"], ["plan"])

        # Cross-owner owner-b never sees owner-a's readiness or tasks.
        cat_b = service_b.catalog()
        self.assertEqual(cat_b["worker_status"], "unknown")
        self.assertEqual(cat_b["profiles"], [])

        # Age readiness beyond 90s -> worker_status becomes offline, profiles empty, base_revision null.
        self.cluster.admin_exec(
            "UPDATE forge_agent.coding_worker_readiness SET observed_at = clock_timestamp() - interval '120 seconds'",
            owner=False,
        )
        cat_offline = service_a.catalog()
        self.assertEqual(cat_offline["worker_status"], "offline")
        self.assertEqual(cat_offline["profiles"], [])
        self.assertIsNone({p["alias"]: p for p in cat_offline["projects"]}["homelab"]["base_revision"])
        self.assert_all_connections_closed()

    def test_concurrent_claim_skip_locked_policy_v2_and_grant_fences(self):
        service, _ = self.make_service(owner="mission-control", projects=("homelab", "docs"))
        db = self.make_worker_db()
        self.grant_default_worker(projects=("homelab",), profiles=("codex-v2",), modes=("plan",))
        inc = self.advertise_default_worker(db)

        # Submit: 1 legacy v1 task (not executable), 1 ungranted project task, 1 ungranted mode task, 3 executable v2 tasks.
        v1_task = service.submit(make_submit(agent_profile="legacy-v1", idempotency_key="t-v1"))
        ungranted_proj = service.submit(make_submit(project_alias="docs", idempotency_key="t-docs"))
        ungranted_mode = service.submit(make_submit(mode="implement", idempotency_key="t-impl"))
        v2_ids = {
            service.submit(make_submit(idempotency_key=f"t-v2-{i}"))["task_id"]
            for i in range(3)
        }

        # Concurrent claims across 6 worker threads claim exactly the 3 executable v2 tasks once each.
        barrier = threading.Barrier(6)
        claimed = []
        errors = []
        lock = threading.Lock()

        def worker_claim():
            try:
                barrier.wait(timeout=5)
                lease = db.claim("worker-1", inc, ["codex-v2", "legacy-v1"])
                with lock:
                    if lease is not None:
                        claimed.append(lease)
            except BaseException as exc:
                with lock:
                    errors.append(exc)

        threads = [threading.Thread(target=worker_claim) for _ in range(6)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=15)

        self.assertEqual(errors, [])
        self.assertEqual(len(claimed), 3)
        self.assertEqual({lease.task_id for lease in claimed}, v2_ids)
        self.assertEqual(len({lease.attempt_id for lease in claimed}), 3)
        self.assertEqual(len({lease.attempt_token for lease in claimed}), 3)
        for lease in claimed:
            self.assertIsInstance(lease, Lease)
            self.assertEqual(lease.unit_name, f"forge-coding-{UUID(lease.attempt_id).hex}")
            self.assertEqual(lease.recipe["version"], 2)
            self.assertEqual(lease.recipe["policy_digest"], work_policy.POLICY_DIGEST)
            self.assertIsNone(lease.resume_workspace_digest)

        # Legacy v1, ungranted project, and ungranted mode tasks remain unclaimed.
        self.assertIsNone(db.claim("worker-1", inc, ["codex-v2", "legacy-v1"]))
        self.assertEqual(service.get(work_api.WorkGet("homelab", UUID(v1_task["task_id"])))["status"], "queued")
        self.assertEqual(service.get(work_api.WorkGet("docs", UUID(ungranted_proj["task_id"])))["status"], "queued")
        self.assertEqual(service.get(work_api.WorkGet("homelab", UUID(ungranted_mode["task_id"])))["status"], "queued")
        self.assert_all_connections_closed()

    def test_wrong_token_expired_lease_deadline_and_post_expiry_record_stop(self):
        service, _ = self.make_service()
        db = self.make_worker_db()
        self.grant_default_worker()
        inc = self.advertise_default_worker(db)

        task_id = UUID(service.submit(make_submit(idempotency_key="fence-1"))["task_id"])
        lease = db.claim("worker-1", inc, ["codex-v2"])
        self.assertIsNotNone(lease)
        self.assertTrue(db.renew(lease))
        self.assertFalse(db.cancellation_requested(lease))

        # Wrong token fails renew and rejects checkpoint/append/publish/record_stop.
        bad_lease = {**lease.to_dict(), "attempt_token": str(uuid4())}
        self.assertFalse(db.renew(bad_lease))
        self.assertTrue(db.cancellation_requested(bad_lease))
        with self.assertRaises(WorkerAccessError):
            db.checkpoint(bad_lease, "spoofed")
        with self.assertRaises(WorkerAccessError):
            db.append_result(bad_lease, 0, "bad")
        with self.assertRaises(WorkerAccessError):
            db.publish(bad_lease, "failed", None, None, "spoofed", make_stop_proof(lease.unit_name))
        with self.assertRaises(WorkerAccessError):
            db.record_stop(bad_lease, make_stop_proof(lease.unit_name), "spoofed", "interrupted")

        # Expire lease -> renew fails, checkpoint/append/publish fail, record_stop STILL works without reopening.
        self.cluster.admin_exec(
            "UPDATE forge_agent.coding_attempts SET lease_expires_at = clock_timestamp() - interval '5 seconds'",
            owner=False,
        )
        self.assertFalse(db.renew(lease))
        self.assertTrue(db.cancellation_requested(lease))
        with self.assertRaises(LeaseExpiredError):
            db.checkpoint(lease, "too late")
        with self.assertRaises(LeaseExpiredError):
            db.append_result(lease, 0, "too late")
        with self.assertRaises(LeaseExpiredError):
            db.publish(lease, "failed", None, None, "too late", make_stop_proof(lease.unit_name))

        # Expire into needs_reconcile, then record_stop safely marks attempt stopped without reopening task.
        self.assertEqual(db.expire_leases(), 1)
        db.record_stop(lease, make_stop_proof(lease.unit_name, stopped=True), "stopped after expiry", "scope_stopped")
        detail = service.get(work_api.WorkGet("homelab", task_id))
        self.assertEqual(detail["status"], "needs_reconcile")
        self.assertEqual(detail["attempt"]["phase"], "stopped")
        self.assertEqual(detail["attempt"]["summary"], "stopped after expiry")

        # Exceeded deadline also rejects renew and publish.
        task2_id = UUID(service.submit(make_submit(idempotency_key="fence-deadline"))["task_id"])
        lease2 = db.claim("worker-1", inc, ["codex-v2"])
        self.cluster.admin_exec(
            f"UPDATE forge_agent.coding_attempts SET deadline_at = clock_timestamp() - interval '1 second' "
            f"WHERE attempt_id = '{lease2.attempt_id}'",
            owner=False,
        )
        self.assertFalse(db.renew(lease2))
        with self.assertRaises(LeaseExpiredError):
            db.publish(lease2, "failed", None, None, "deadline", make_stop_proof(lease2.unit_name))
        self.assertEqual(service.get(work_api.WorkGet("homelab", task2_id))["status"], "running")
        self.assert_all_connections_closed()

    def test_staging_invisible_utf8_escaped_over_64kib_reassemble_and_publish_replay_after_accept(self):
        service, _ = self.make_service()
        db = self.make_worker_db()
        self.grant_default_worker()
        inc = self.advertise_default_worker(db)

        task_id = UUID(service.submit(make_submit(idempotency_key="result-full",mode="review"))["task_id"])
        lease = db.claim("worker-1", inc, ["codex-v2"])
        attempt_uuid = UUID(lease.attempt_id)

        # Build > 64 KiB payload mixing multi-byte UTF-8 and heavily JSON-escaped characters.
        chunks = [
            ("😀é日本語-" * 1000),                  # ~15.4 KiB multi-byte UTF-8
            ("\x1b\"\\\n" * 4000),                  # 16 KiB raw -> ~48 KiB JSON-escaped!
            ("line-" + "αβγδε" * 1400 + "\n"),      # ~14 KiB multi-byte UTF-8
            ("quote\"slash\\end\n" * 900),          # ~16 KiB raw
            ("tail-😀-final-ok" * 600),             # ~11 KiB
        ]
        full_text = "".join(chunks)
        full_bytes = full_text.encode("utf-8")
        self.assertGreater(len(full_bytes), 65536)
        full_sha = hashlib.sha256(full_bytes).hexdigest()

        for idx, chunk in enumerate(chunks):
            self.assertLessEqual(len(chunk.encode("utf-8")), 16384)
            db.append_result(lease, idx, chunk)

        # Exact chunk retry is idempotent; changed bytes on same index conflicts.
        db.append_result(lease, 0, chunks[0])
        with self.assertRaises(OutcomeConflictError):
            db.append_result(lease, 0, "tampered-chunk-0")

        # Staging is completely invisible before publish.
        opened_before = service.get(work_api.WorkGet("homelab", task_id))
        self.assertIsNone(opened_before["result"])
        with self.assertRaises(Rejected) as ctx:
            service.result(work_api.WorkResult("homelab", task_id, attempt_uuid, None, 16384))
        self.assertEqual((ctx.exception.status, ctx.exception.code), (404, "not_found"))

        # Manifest mismatch fails publish without exposing staging.
        bad_manifest = {"sha256": "0" * 64, "byte_count": len(full_bytes), "chunk_count": len(chunks)}
        with self.assertRaises(ResultMismatchError):
            db.publish(lease, "awaiting_review", bad_manifest, None, "done", make_stop_proof(lease.unit_name))

        # Valid publish commits atomically.
        manifest = {"sha256": full_sha, "byte_count": len(full_bytes), "chunk_count": len(chunks)}
        usage = {
            "input_tokens": "100",
            "output_tokens": "250",
            "cached_input_tokens": "20",
            "total_tokens": "350",
            "source": "codex-exec-turn-v1",
            "coverage": "complete",
            "session_provenance": "fresh_session",
        }
        proof = make_stop_proof(lease.unit_name, stopped=True)
        receipt = db.publish(lease, "awaiting_review", manifest, usage, "completed work", proof)
        self.assertEqual(receipt["status"], "awaiting_review")
        self.assertEqual(receipt["result"]["sha256"], full_sha)

        # Page through full result with default 16384 and check JSON framing stays bounded and reassembles exact bytes.
        collected = []
        cursor = None
        pages = 0
        while True:
            page = service.result(work_api.WorkResult("homelab", task_id, attempt_uuid, cursor, 16384))
            pages += 1
            self.assertEqual(page["result_digest"], full_sha)
            self.assertEqual(page["total_bytes"], len(full_bytes))
            self.assertLessEqual(len(page["content"].encode("utf-8")), 16384)
            self.assertLess(len(json.dumps(page, ensure_ascii=False).encode("utf-8")), 65536)
            collected.append(page["content"])
            if page["complete"]:
                self.assertIsNone(page["next_cursor"])
                break
            self.assertIsNotNone(page["next_cursor"])
            cursor = page["next_cursor"]
        self.assertGreater(pages, 4)
        self.assertEqual("".join(collected), full_text)

        # Small odd limit_bytes=7 never splits 4-byte emoji codepoints!
        small_page = service.result(work_api.WorkResult("homelab", task_id, attempt_uuid, None, 7))
        self.assertEqual(small_page["content"], "😀é")
        self.assertEqual(small_page["next_cursor"], f"{full_sha}.6")

        # Tampered manifest digest -> 409 result_mismatch; mid-UTF8 byte offset -> 400 invalid_request.
        with self.assertRaises(Rejected) as ctx:
            service.result(work_api.WorkResult("homelab", task_id, attempt_uuid, f"{'f' * 64}.0", 16))
        self.assertEqual((ctx.exception.status, ctx.exception.code), (409, "result_mismatch"))
        with self.assertRaises(Rejected) as ctx:
            service.result(work_api.WorkResult("homelab", task_id, attempt_uuid, f"{full_sha}.1", 16))
        self.assertEqual((ctx.exception.status, ctx.exception.code), (400, "invalid_request"))

        # Explicit acceptance marks completed; cross-action idempotency key conflicts.
        accepted = service.accept(work_api.WorkAccept("homelab", task_id, receipt["revision"], "acc-key-1", "Ship it"))
        self.assertEqual(accepted["status"], "completed")
        self.assertEqual(
            service.accept(work_api.WorkAccept("homelab", task_id, receipt["revision"], "acc-key-1", "Ship it")),
            accepted,
        )
        with self.assertRaises(Rejected) as ctx:
            service.cancel(work_api.WorkCancel("homelab", task_id, accepted["revision"], "acc-key-1"))
        self.assertEqual((ctx.exception.status, ctx.exception.code), (409, "idempotency_conflict"))

        # Exact publish replay AFTER user acceptance AND expired deadline returns original receipt without mutation!
        self.cluster.admin_exec(
            f"UPDATE forge_agent.coding_attempts SET deadline_at = clock_timestamp() - interval '1 hour' "
            f"WHERE attempt_id = '{lease.attempt_id}'",
            owner=False,
        )
        ev_before = service.events(work_api.WorkEvents("homelab", task_id, 0, 100))
        replayed = db.publish(lease, "awaiting_review", manifest, usage, "completed work", proof)
        self.assertEqual(replayed, receipt)
        ev_after = service.events(work_api.WorkEvents("homelab", task_id, 0, 100))
        self.assertEqual(ev_after, ev_before)
        self.assertEqual(service.get(work_api.WorkGet("homelab", task_id))["status"], "completed")

        # Changed outcome on published attempt -> OutcomeConflictError.
        with self.assertRaises(OutcomeConflictError):
            db.publish(lease, "failed", None, None, "different", proof)
        self.assert_all_connections_closed()

    def test_cancel_running_requests_stop_until_confirmed_stopped_publish(self):
        service, _ = self.make_service()
        db = self.make_worker_db()
        self.grant_default_worker()
        inc = self.advertise_default_worker(db)

        task_id = UUID(service.submit(make_submit(idempotency_key="cancel-run"))["task_id"])
        lease = db.claim("worker-1", inc, ["codex-v2"])

        # Cancelling running task requests stop (not cancelled yet!).
        cancel_rec = service.cancel(work_api.WorkCancel("homelab", task_id, 2, "cancel-run-k1"))
        self.assertEqual(cancel_rec, {
            "project_alias": "homelab",
            "task_id": str(task_id),
            "base_revision": 2,
            "revision": 3,
            "status": "cancel_requested",
            "actor": {"kind": "operator", "id": "mission-control"},
        })
        self.assertTrue(db.cancellation_requested(lease))

        # New key on cancel_requested conflicts with not_cancellable.
        with self.assertRaises(Rejected) as ctx:
            service.cancel(work_api.WorkCancel("homelab", task_id, 3, "cancel-run-k2"))
        self.assertEqual((ctx.exception.status, ctx.exception.code), (409, "not_cancellable"))

        # Publishing cancelled requires confirmed scope_stopped=True.
        with self.assertRaises(ValueError):
            db.publish(lease, "cancelled", None, None, "stopping", make_stop_proof(lease.unit_name, stopped=False))

        pub = db.publish(lease, "cancelled", None, None, "stopped on request", make_stop_proof(lease.unit_name, stopped=True))
        self.assertEqual(pub["status"], "cancelled")
        self.assertEqual(service.get(work_api.WorkGet("homelab", task_id))["status"], "cancelled")

        # Original cancel key STILL replays original cancel_requested receipt BEFORE state checks.
        self.assertEqual(
            service.cancel(work_api.WorkCancel("homelab", task_id, 2, "cancel-run-k1")),
            cancel_rec,
        )
        self.assert_all_connections_closed()

    def test_expire_reconcile_explicit_resume_revision_digest_and_event_sequence_independence(self):
        service, _ = self.make_service()
        db = self.make_worker_db()
        self.grant_default_worker()
        inc = self.advertise_default_worker(db)

        task_id = UUID(service.submit(make_submit(idempotency_key="reconcile-flow"))["task_id"])
        lease1 = db.claim("worker-1", inc, ["codex-v2"])
        # Two checkpoints bump event sequence WITHOUT bumping task revision.
        db.checkpoint(lease1, "step 1")
        db.checkpoint(lease1, "step 2")
        ev = service.events(work_api.WorkEvents("homelab", task_id, 0, 50))
        self.assertEqual(ev["revision"], 2)
        self.assertEqual(ev["next_sequence"], 4)

        # Force expiry -> expire_leases marks needs_reconcile, retains reservation, never requeues.
        self.cluster.admin_exec(
            f"UPDATE forge_agent.coding_attempts SET lease_expires_at = clock_timestamp() - interval '10 seconds' "
            f"WHERE attempt_id = '{lease1.attempt_id}'",
            owner=False,
        )
        self.assertEqual(db.expire_leases(), 1)
        self.assertIsNone(db.claim("worker-1", inc, ["codex-v2"]))

        pending = db.pending_reconciliations()
        self.assertEqual(len(pending), 1)
        self.assertEqual(pending[0]["attempt_id"], lease1.attempt_id)
        self.assertNotIn("prompt", pending[0])
        self.assertNotIn("attempt_token", pending[0])

        # Resume before reconciliation -> 409 reconcile_required.
        with self.assertRaises(Rejected) as ctx:
            service.resume(work_api.WorkResume(
                "homelab", task_id, 3, "res-before", "agy-v2", uuid4(), "try early",
            ))
        self.assertEqual((ctx.exception.status, ctx.exception.code), (409, "reconcile_required"))

        # Reconcile requires confirmed stopped proof.
        with self.assertRaises(ValueError):
            db.reconcile(pending[0], WORKSPACE_DIGEST, make_stop_proof(lease1.unit_name, stopped=False))

        rec_id = UUID(db.reconcile(pending[0], WORKSPACE_DIGEST, make_stop_proof(lease1.unit_name, stopped=True)))
        opened = service.get(work_api.WorkGet("homelab", task_id))
        self.assertEqual(opened["reconciliation_receipt_id"], str(rec_id))
        self.assertEqual(opened["revision"], 3)

        # Wrong expected_revision -> 409 revision_conflict.
        with self.assertRaises(Rejected) as ctx:
            service.resume(work_api.WorkResume("homelab", task_id, 2, "res-bad-rev", "agy-v2", rec_id))
        self.assertEqual((ctx.exception.status, ctx.exception.code), (409, "revision_conflict"))

        # Valid explicit resume switches profile to agy-v2 and queues with locked workspace digest.
        resumed = service.resume(work_api.WorkResume("homelab", task_id, 3, "res-ok", "agy-v2", rec_id, "retry"))
        self.assertEqual(resumed["status"], "queued")
        self.assertEqual(resumed["base_revision"], 3)
        self.assertEqual(resumed["revision"], 4)

        # Next claim carries resume_workspace_digest and uses attempt_no=2.
        lease2 = db.claim("worker-1", inc, ["agy-v2"])
        self.assertIsNotNone(lease2)
        self.assertEqual(lease2.task_id, str(task_id))
        self.assertEqual(lease2.agent_profile, "agy-v2")
        self.assertEqual(lease2.resume_workspace_digest, WORKSPACE_DIGEST)

        # Replay of original resume key returns original receipt even after re-claim.
        self.assertEqual(
            service.resume(work_api.WorkResume("homelab", task_id, 3, "res-ok", "agy-v2", rec_id, "retry")),
            resumed,
        )

        # After lease2 fails and stops, reusing old rec_id from revision 3 -> 409 stale_reconciliation.
        db.publish(lease2, "failed", None, None, "err", make_stop_proof(lease2.unit_name, stopped=True))
        with self.assertRaises(Rejected) as ctx:
            service.resume(work_api.WorkResume("homelab", task_id, 6, "res-stale-rec", "codex-v2", rec_id))
        self.assertEqual((ctx.exception.status, ctx.exception.code), (409, "stale_reconciliation"))
        self.assert_all_connections_closed()

    def test_usage_poll_out_of_order_config_epoch_account_switch_and_fail_preserves_good_snapshot(self):
        _, config = self.make_service()
        usage_store = UsageStore(config, lambda: self._connect("forge_agent"))
        db = self.make_worker_db()
        self.grant_default_worker()

        p1 = db.begin_usage_poll("codex-v2")
        self.assertEqual(p1, {"generation": 1, "configuration_epoch": 1})
        snap_a = make_snapshot(account_identity=HMAC_A, used_percent=10.0)
        self.assertTrue(db.record_usage("codex-v2", p1["generation"], p1["configuration_epoch"], snap_a))

        # Two overlapping polls start under epoch 1.
        p2 = db.begin_usage_poll("codex-v2")
        p3 = db.begin_usage_poll("codex-v2")
        self.assertEqual((p2["generation"], p3["generation"]), (2, 3))

        # Latest poll 3 observes account switch (HMAC_B != HMAC_A) -> bumps epoch to 2.
        snap_b = make_snapshot(account_identity=HMAC_B, used_percent=25.0)
        self.assertTrue(db.record_usage("codex-v2", p3["generation"], p3["configuration_epoch"], snap_b))

        # Older poll 2 started under old epoch 1 and is rejected after the switch.
        self.assertFalse(db.record_usage("codex-v2", p2["generation"], p2["configuration_epoch"], snap_a))

        # New poll observes epoch 2; out-of-order lower generation under epoch 2 is rejected.
        p4 = db.begin_usage_poll("codex-v2")
        self.assertEqual(p4, {"generation": 4, "configuration_epoch": 2})
        self.assertFalse(db.record_usage("codex-v2", 2, 2, snap_a))
        snap_b2 = make_snapshot(account_identity=HMAC_B, used_percent=30.0)
        self.assertTrue(db.record_usage("codex-v2", p4["generation"], p4["configuration_epoch"], snap_b2))

        # Poll failure keeps prior good snapshot freshness/observed_at intact and records last_error separately.
        p5 = db.begin_usage_poll("codex-v2")
        db.fail_usage_poll("codex-v2", p5["generation"], p5["configuration_epoch"], "provider_timeout")

        overview = usage_store.overview("homelab", ("codex-v2",))
        self.assertEqual(len(overview["providers"]), 1)
        prov = overview["providers"][0]
        self.assertEqual(prov["snapshot_state"], "fresh")
        self.assertEqual(prov["last_error"], "provider_timeout")
        self.assertEqual(prov["allowance"]["buckets"][0]["used_percent"], 30.0)
        self.assertNotIn("account_identity", prov)
        self.assert_all_connections_closed()

    def test_usage_store_wide_decimal_dedup_unknown_attempts_and_coalesced_refresh(self):
        service, config = self.make_service(projects=("homelab", "docs"))
        usage_store = UsageStore(config, lambda: self._connect("forge_agent"))
        db = self.make_worker_db()
        self.grant_default_worker(projects=("homelab", "docs"))
        inc = self.advertise_default_worker(
            db,
            projects=[
                {"alias": "homelab", "base_revision": BASE_REV},
                {"alias": "docs", "base_revision": BASE_REV},
            ],
        )

        # Absent usage snapshot reports unknown (never zero-fabricated tokens!).
        initial = usage_store.overview("homelab", ("agy-v2",))
        agy = initial["providers"][0]
        self.assertEqual(agy["snapshot_state"], "unknown")
        self.assertEqual(agy["allowance"]["state"], "unknown")
        self.assertIsNone(agy["jobs"]["total_tokens"])
        self.assertEqual((agy["jobs"]["known_attempts"], agy["jobs"]["unknown_attempts"]), (0, 0))

        # Attempt 1: wide 30-digit decimal counters (exceeds IEEE-754 float precision) + replay publish dedup.
        t1 = service.submit(make_submit(project_alias="homelab", agent_profile="agy-v2", idempotency_key="u-1"))
        l1 = db.claim("worker-1", inc, ["agy-v2"])
        db.append_result(l1, 0, "ok")
        m1 = {"sha256": hashlib.sha256(b"ok").hexdigest(), "byte_count": 2, "chunk_count": 1}
        wide_in = "100000000000000000000000000001"
        wide_out = "200000000000000000000000000002"
        wide_total = "300000000000000000000000000009"  # AGY reported total (not sum of parts)
        u1 = {
            "input_tokens": wide_in,
            "output_tokens": wide_out,
            "cached_input_tokens": None,
            "total_tokens": wide_total,
            "source": "agy-result-v1",
            "coverage": "complete",
            "session_provenance": "fresh_session",
        }
        proof1 = make_stop_proof(l1.unit_name, stopped=True)
        r1 = db.publish(l1, "awaiting_review", m1, u1, "done", proof1)
        # Replaying publish must NOT double count usage!
        self.assertEqual(db.publish(l1, "awaiting_review", m1, u1, "done", proof1), r1)

        # Attempt 2: usage=None -> counted as unknown_attempts.
        service.submit(make_submit(project_alias="homelab", agent_profile="agy-v2", idempotency_key="u-2"))
        l2 = db.claim("worker-1", inc, ["agy-v2"])
        db.publish(l2, "failed", None, None, "err", make_stop_proof(l2.unit_name, stopped=True))

        # Attempt 3: unverified_baseline -> counted as unknown_attempts, counters NOT added!
        service.submit(make_submit(project_alias="homelab", agent_profile="agy-v2", idempotency_key="u-3"))
        l3 = db.claim("worker-1", inc, ["agy-v2"])
        u3 = {
            "input_tokens": "999",
            "output_tokens": "999",
            "cached_input_tokens": None,
            "total_tokens": "1998",
            "source": "agy-result-v1",
            "coverage": "complete",
            "session_provenance": "unverified_baseline",
        }
        db.publish(l3, "failed", None, u3, "baseline bad", make_stop_proof(l3.unit_name, stopped=True))

        # Attempt 4 on OTHER project 'docs' must NOT leak into 'homelab' rollup!
        service.submit(make_submit(project_alias="docs", agent_profile="agy-v2", idempotency_key="u-docs"))
        l4 = db.claim("worker-1", inc, ["agy-v2"])
        db.publish(l4, "failed", None, u1, "docs job", make_stop_proof(l4.unit_name, stopped=True))

        ov = usage_store.overview("homelab", ("agy-v2",))
        jobs = ov["providers"][0]["jobs"]
        self.assertEqual(jobs["scope"], "forge_project")
        self.assertEqual(jobs["known_attempts"], 1)
        self.assertEqual(jobs["unknown_attempts"], 2)
        self.assertEqual(jobs["input_tokens"], wide_in)
        self.assertEqual(jobs["output_tokens"], wide_out)
        self.assertIsNone(jobs["cached_input_tokens"])
        self.assertEqual(jobs["total_tokens"], wide_total)
        self.assertIsNotNone(jobs["elapsed_ms"])

        # Coalesced refresh outbox -> worker next_refresh -> record_usage clears refresh_pending.
        req1 = usage_store.request_refresh("homelab", "agy-v2")
        self.assertFalse(req1["coalesced"])
        self.assertEqual(req1["profiles"], ["agy-v2"])
        req2 = usage_store.request_refresh("homelab", "agy-v2")
        self.assertTrue(req2["coalesced"])
        self.assertEqual(req2["refresh_id"], req1["refresh_id"])

        self.assertTrue(usage_store.overview("homelab", ("agy-v2",))["providers"][0]["refresh_pending"])
        claimed_ref = db.next_refresh()
        self.assertIsNotNone(claimed_ref)
        self.assertEqual(claimed_ref["refresh_id"], req1["refresh_id"])
        self.assertEqual(claimed_ref["profile_alias"], "agy-v2")
        self.assertTrue(db.record_usage(
            "agy-v2",
            claimed_ref["generation"],
            claimed_ref["configuration_epoch"],
            make_snapshot(state="unknown", observed_at=None, source="agy-cli", version="1.5.12",
                          allowance_state="unsupported", account_identity=None),
        ))
        self.assertFalse(usage_store.overview("homelab", ("agy-v2",))["providers"][0]["refresh_pending"])
        self.assert_all_connections_closed()


if __name__ == "__main__":
    unittest.main()
