"""Disposable PostgreSQL policy-version upgrade and immutable receipt regressions."""

from contextlib import closing
import hashlib
from pathlib import Path
import re
import shutil
from tempfile import TemporaryDirectory
import unittest
from uuid import UUID, uuid4

from test_work_execution_and_usage import BASE_REV, make_stop_proof, make_submit, make_v2_config
from work_api import WorkAccept, WorkGet, WorkResume, WorkServiceFactory
from work_config import PinnedRecipe, sha256_canonical
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE, SQL_DIR
from work_store import WorkStore
from work_worker_store import WorkerDB
import work_policy


# Historical digests must not be reconstructed from the current policy: adding
# a v3 session field must never retroactively change v1/v2 identity.
PREVIOUS_POLICY_ID = "forge-coding-v2"
PREVIOUS_POLICY = {
    "id": PREVIOUS_POLICY_ID,
    "adapter_versions": {"antigravity": "1.5.12", "codex": "0.160.0"},
    "antigravity_tool_policy": "workspace-elevated-v1",
    "max_wall_seconds": 1800,
    "max_stream_bytes": 67108864,
    "max_result_bytes": 16777216,
    "lease_seconds": 60,
    "renew_seconds": 10,
    "fresh_session": True,
    "scope": "systemd-root-v1",
    "workspace": "isolated-git-worktree-v1",
    "billing": "included-no-fallback",
}
PREVIOUS_POLICY_DIGEST = sha256_canonical(PREVIOUS_POLICY)
OLD_POLICY_ID = "forge-coding-v1"
OLD_POLICY = {key: value for key, value in PREVIOUS_POLICY.items() if key != "antigravity_tool_policy"}
OLD_POLICY["id"] = OLD_POLICY_ID
OLD_POLICY_DIGEST = sha256_canonical(OLD_POLICY)


class PolicyIdentityTest(unittest.TestCase):
    def test_session_policy_is_versioned_without_changing_historical_digest_identity(self):
        self.assertEqual(OLD_POLICY_DIGEST, "2465fea4c303e83825339fd15793927a80b147519586519283e7534b2ad3be44")
        self.assertEqual(PREVIOUS_POLICY_DIGEST, "67065fb04466901c8c0df1faad5d7a04503322c6e4c1b8a75ca08eba1375fa58")
        self.assertEqual(work_policy.POLICY_ID, "forge-coding-v3")
        self.assertEqual(work_policy.POLICY["antigravity_session_policy"], "fresh-unless-receipt-owned-task-resume-v1")
        self.assertEqual(work_policy.POLICY["codex_session_policy"], "ephemeral-v1")
        self.assertNotIn("fresh_session", work_policy.POLICY)
        self.assertNotIn(work_policy.POLICY_DIGEST, (OLD_POLICY_DIGEST, PREVIOUS_POLICY_DIGEST))
        comparable = {key: value for key, value in work_policy.POLICY.items()
                      if key not in {"id", "antigravity_session_policy", "codex_session_policy"}}
        self.assertEqual(comparable, {key: value for key, value in PREVIOUS_POLICY.items()
                                      if key not in {"id", "fresh_session"}})



class PreUpgradeCluster(DisposablePostgresCluster):
    """Apply an owned historical registry copy before the production upgrade."""

    def __init__(self, legacy_sql):
        super().__init__()
        self.legacy_sql = legacy_sql

    def apply_and_verify(self):
        self._run(
            "psql", "-h", self.root, "-U", "postgres", "-X", "-v", "ON_ERROR_STOP=1",
            "-d", "forge_agent", "-c", "SET ROLE forge_agent_owner",
            "-f", self.legacy_sql / "apply.sql",
            "-f", self.legacy_sql / "management/grant-runner.sql",
        )


@unittest.skipUnless(PG16_AVAILABLE, "Requires non-root disposable PostgreSQL 16")
class WorkPolicyUpgradeTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def setUp(self):
        self.cluster.reset_coding_tables()

    def rows(self, cluster, statement, parameters=()):
        with closing(cluster.connect("postgres")) as connection, connection.cursor() as cursor:
            cursor.execute(statement, parameters)
            return cursor.fetchall()

    def store(self, cluster):
        return WorkStore("mission-control", ("homelab",), lambda: cluster.connect("forge_agent"))

    def service(self, cluster):
        return WorkServiceFactory(make_v2_config(), lambda: cluster.connect("forge_agent")).create()

    def worker(self, cluster):
        return WorkerDB(
            lambda: cluster.connect("forge_coding_worker"), "mission-control",
            reconcile_connect_factory=lambda: cluster.connect("forge_coding_reconciler"),
        )

    def advertise(self, cluster, db, name, digest):
        cluster.grant_worker("mission-control", "homelab", "codex-v2", ("plan",))
        incarnation = str(uuid4())
        db.advertise(
            name, incarnation, [{"alias": "homelab", "base_revision": BASE_REV}],
            [{"alias": "codex-v2", "backend": "codex", "model": "gpt-5.4", "modes": ["plan"]}],
            digest,
        )
        return incarnation

    def old_recipe(self, policy=OLD_POLICY_ID):
        digest = {OLD_POLICY_ID: OLD_POLICY_DIGEST, PREVIOUS_POLICY_ID: PREVIOUS_POLICY_DIGEST}[policy]
        payload = {
            "version": 2, "backend": "codex", "model": "gpt-5.4",
            "resource_profile": policy, "mode": "plan", "generation_token_limit": None,
            "policy_digest": digest,
        }
        return PinnedRecipe(**payload, digest=sha256_canonical(payload))

    def admit_old(self, cluster, key, policy=OLD_POLICY_ID):
        command = make_submit(idempotency_key=key)
        return self.store(cluster).submit(**vars(command), resolve_recipe=lambda: self.old_recipe(policy))

    def stage(self, db, lease):
        text = "An immutable pre-upgrade reply. 😀"
        db.append_result(lease, 0, text)
        return text, {
            "sha256": hashlib.sha256(text.encode()).hexdigest(),
            "byte_count": len(text.encode()), "chunk_count": 1,
        }

    def test_published_old_policy_task_attempt_and_receipt_survive_real_migration_11(self):
        with TemporaryDirectory(prefix="forge-policy-upgrade-", dir="/tmp") as temporary:
            legacy_sql = Path(temporary) / "sql"
            shutil.copytree(SQL_DIR, legacy_sql)
            # This fixture is genuinely schema 10; the additive progress routine
            # does not exist until migration 12. Keep its grant out of the copy.
            grant = legacy_sql / "management/grant-runner.sql"
            grant.write_text(grant.read_text().replace(
                "    forge_coding_worker_api.report_progress(uuid, uuid, integer, text, text),\n", ""
            ))
            apply = (legacy_sql / "apply.sql").read_text()
            apply, removed = re.subn(
                r"SELECT NOT EXISTS \(\s*SELECT 1 FROM forge_agent\.schema_migrations "
                r"WHERE version\s*=\s*(?:1[1-9]|[2-9][0-9]+)\s*\).*?\\endif\s*", "", apply, flags=re.DOTALL,
            )
            self.assertGreaterEqual(removed, 1, "The legacy registry excludes migration 11 and all successors")
            (legacy_sql / "apply.sql").write_text(apply)
            cluster = PreUpgradeCluster(legacy_sql).start()
            try:
                self.assertEqual(self.rows(cluster, "SELECT max(version) FROM forge_agent.schema_migrations")[0][0], 10)
                db = self.worker(cluster)
                incarnation = self.advertise(cluster, db, "old-worker", OLD_POLICY_DIGEST)
                admitted = self.admit_old(cluster, "old-before-upgrade")
                lease = db.claim("old-worker", incarnation, ["codex-v2"])
                self.assertEqual(lease.task_id, admitted["task_id"])
                text, manifest = self.stage(db, lease)
                proof = make_stop_proof(lease.unit_name)
                receipt = db.publish(lease, "awaiting_review", manifest, None, "Old policy result", proof)
                snapshots = self.rows(
                    cluster, "SELECT to_jsonb(t), to_jsonb(a) FROM forge_agent.coding_tasks t "
                    "JOIN forge_agent.coding_attempts a ON a.task_id=t.id WHERE t.id=%s",
                    (UUID(lease.task_id),),
                )
                # Invoke the production migration registry/verifiers against the existing cluster.
                DisposablePostgresCluster.apply_and_verify(cluster)
                DisposablePostgresCluster.apply_and_verify(cluster)
                self.assertEqual(self.rows(cluster, "SELECT version FROM forge_agent.schema_migrations WHERE version = 11")[0][0], 11)
                self.assertEqual(self.rows(
                    cluster, "SELECT to_jsonb(t), to_jsonb(a) FROM forge_agent.coding_tasks t "
                    "JOIN forge_agent.coding_attempts a ON a.task_id=t.id WHERE t.id=%s",
                    (UUID(lease.task_id),),
                ), snapshots)
                self.assertEqual(db.publish(lease, "awaiting_review", manifest, None, "Old policy result", proof), receipt)
                self.assertEqual(self.service(cluster).get(WorkGet("homelab", UUID(lease.task_id)))["result"]["sha256"], manifest["sha256"])
                page = self.store(cluster).result(project_alias="homelab", task_id=UUID(lease.task_id),
                                                  attempt_id=UUID(lease.attempt_id))
                self.assertEqual(page["content"], text)
                self.assertTrue(page["complete"])
                self.assertEqual(self.admit_old(cluster, "old-before-upgrade"), admitted)
            finally:
                cluster.stop()

    def test_schema_12_to_13_preserves_v2_tasks_attempts_results_and_control_receipts(self):
        with TemporaryDirectory(prefix="forge-session-policy-upgrade-", dir="/tmp") as temporary:
            legacy_sql = Path(temporary) / "sql"
            shutil.copytree(SQL_DIR, legacy_sql)
            registry = legacy_sql / "apply.sql"
            script, removed = re.subn(
                r"SELECT NOT EXISTS \(\s*SELECT 1 FROM forge_agent\.schema_migrations "
                r"WHERE version\s*=\s*(?:1[3-9]|[2-9][0-9]+)\s*\).*?\\endif\s*", "", registry.read_text(), flags=re.DOTALL,
            )
            self.assertEqual(removed, 1, "The legacy registry is exactly schema12")
            registry.write_text(script)
            cluster = PreUpgradeCluster(legacy_sql).start()
            try:
                self.assertEqual(self.rows(cluster, "SELECT max(version) FROM forge_agent.schema_migrations")[0][0], 12)
                db = self.worker(cluster)
                incarnation = self.advertise(cluster, db, "v2-worker", PREVIOUS_POLICY_DIGEST)
                admitted = self.admit_old(cluster, "v2-before-session-policy", PREVIOUS_POLICY_ID)
                lease = db.claim("v2-worker", incarnation, ["codex-v2"])
                text, manifest = self.stage(db, lease)
                proof = make_stop_proof(lease.unit_name)
                publication = db.publish(lease, "awaiting_review", manifest, None, "V2 result", proof)
                command = WorkAccept("homelab", UUID(lease.task_id), publication["revision"],
                                     "v2-accept-before-session-policy", "Reviewed before upgrade")
                acceptance = self.service(cluster).accept(command)
                snapshot_query = (
                    "SELECT to_jsonb(t), to_jsonb(a), "
                    "(SELECT jsonb_agg(to_jsonb(r) ORDER BY r.idempotency_key) "
                    "FROM forge_agent.coding_control_receipts r WHERE r.task_id=t.id) "
                    "FROM forge_agent.coding_tasks t JOIN forge_agent.coding_attempts a ON a.task_id=t.id WHERE t.id=%s"
                )
                snapshots = self.rows(cluster, snapshot_query, (UUID(lease.task_id),))
                self.assertIsNotNone(snapshots[0][2], "An immutable explicit acceptance receipt exists before upgrade")
                DisposablePostgresCluster.apply_and_verify(cluster)
                DisposablePostgresCluster.apply_and_verify(cluster)
                self.assertEqual(self.rows(cluster, "SELECT max(version) FROM forge_agent.schema_migrations")[0][0], 13)
                self.assertEqual(self.rows(cluster, snapshot_query, (UUID(lease.task_id),)), snapshots)
                self.assertEqual(db.publish(lease, "awaiting_review", manifest, None, "V2 result", proof), publication)
                self.assertEqual(self.service(cluster).accept(command), acceptance)
                self.assertEqual(self.admit_old(cluster, "v2-before-session-policy", PREVIOUS_POLICY_ID), admitted)
                result = self.store(cluster).result(project_alias="homelab", task_id=UUID(lease.task_id),
                                                     attempt_id=UUID(lease.attempt_id))
                self.assertEqual(result["content"], text)
                self.assertTrue(result["complete"])
                # The upgraded registry still retains old digests, while new
                # submissions use v3 and are invisible to a v2-only claimant.
                current = self.service(cluster).submit(make_submit(idempotency_key="v3-after-upgrade"))
                self.assertIsNone(db.claim("v2-worker", incarnation, ["codex-v2"]))
                current_inc = self.advertise(cluster, db, "v3-worker", work_policy.POLICY_DIGEST)
                current_lease = db.claim("v3-worker", current_inc, ["codex-v2"])
                self.assertEqual(current_lease.task_id, current["task_id"])
                self.assertEqual(current_lease.recipe["resource_profile"], work_policy.POLICY_ID)
            finally:
                cluster.stop()

    def test_v1_v2_and_current_workers_claim_only_their_exact_policy_digest(self):
        old = self.admit_old(self.cluster, "old-queued-first")
        previous = self.admit_old(self.cluster, "previous-queued-second", PREVIOUS_POLICY_ID)
        current = self.service(self.cluster).submit(make_submit(idempotency_key="current-queued-third"))
        db = self.worker(self.cluster)
        old_inc = self.advertise(self.cluster, db, "old-worker", OLD_POLICY_DIGEST)
        previous_inc = self.advertise(self.cluster, db, "previous-worker", PREVIOUS_POLICY_DIGEST)
        current_inc = self.advertise(self.cluster, db, "current-worker", work_policy.POLICY_DIGEST)
        for worker_id, incarnation, admitted, policy, digest in (
            ("current-worker", current_inc, current, work_policy.POLICY_ID, work_policy.POLICY_DIGEST),
            ("previous-worker", previous_inc, previous, PREVIOUS_POLICY_ID, PREVIOUS_POLICY_DIGEST),
            ("old-worker", old_inc, old, OLD_POLICY_ID, OLD_POLICY_DIGEST),
        ):
            with self.subTest(policy=policy):
                lease = db.claim(worker_id, incarnation, ["codex-v2"])
                self.assertEqual(lease.task_id, admitted["task_id"])
                self.assertEqual(lease.recipe["resource_profile"], policy)
                self.assertEqual(lease.recipe["policy_digest"], digest)
                self.assertIsNone(db.claim(worker_id, incarnation, ["codex-v2"]))

    def test_current_policy_publish_reconcile_and_explicit_resume_preserve_recipe(self):
        service = self.service(self.cluster)
        admitted = service.submit(make_submit(idempotency_key="current-resume"))
        db = self.worker(self.cluster)
        incarnation = self.advertise(self.cluster, db, "current-worker", work_policy.POLICY_DIGEST)
        lease = db.claim("current-worker", incarnation, ["codex-v2"])
        self.assertEqual(lease.task_id, admitted["task_id"])
        self.assertEqual(lease.recipe["policy_digest"], work_policy.POLICY_DIGEST)
        self.assertIsNone(lease.resume_attempt_id)
        proof = make_stop_proof(lease.unit_name)
        publication = db.publish(lease, "quota_blocked", None, None, "Stopped at provider allowance", proof)
        receipt_id = db.reconcile(lease, "c" * 64, proof)
        resumed = service.resume(WorkResume("homelab", UUID(lease.task_id), publication["revision"],
                                            "resume-current", "codex-v2", UUID(receipt_id), "Reviewed continuation"))
        self.assertEqual(resumed["status"], "queued")
        replacement = db.claim("current-worker", incarnation, ["codex-v2"])
        self.assertNotEqual(replacement.attempt_id, lease.attempt_id)
        self.assertEqual(dict(replacement.recipe), dict(lease.recipe))
        self.assertEqual(replacement.resume_workspace_digest, "c" * 64)
        self.assertEqual(replacement.resume_attempt_id, lease.attempt_id)
        self.assertEqual(replacement.to_dict()["resume_attempt_id"], lease.attempt_id)
        self.assertEqual(db.publish(lease, "quota_blocked", None, None, "Stopped at provider allowance", proof), publication)

    def test_apply_twice_and_verify_policy_membership_and_digest_predicate_drift(self):
        self.cluster.apply_and_verify()
        self.cluster.apply_and_verify()
        self.assertEqual(self.rows(self.cluster, "SELECT version FROM forge_agent.schema_migrations WHERE version = 11")[0][0], 11)
        script = SQL_DIR / "management/verify.sql"
        self.assertEqual(self.rows(self.cluster,
            "SELECT count(*) FROM forge_agent.schema_migrations "
            "WHERE version=13 AND name='coding_task_session_policy'")[0][0], 1)
        try:
            self.cluster.admin_exec(
                "UPDATE forge_agent.schema_migrations SET name='unexpected-session-policy' WHERE version=13")
            result = self.cluster.run_sql_script(script)
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("session policy migration 0013 is missing", result.stderr)
        finally:
            self.cluster.admin_exec(
                "UPDATE forge_agent.schema_migrations SET name='coding_task_session_policy' WHERE version=13")
        original = self.rows(self.cluster,
            "SELECT pg_get_constraintdef(oid,false) FROM pg_constraint "
            "WHERE conrelid='forge_agent.coding_attempts'::regclass AND conname='coding_attempts_pinned_recipe_check'")[0][0]
        for policy in (OLD_POLICY_ID, PREVIOUS_POLICY_ID, work_policy.POLICY_ID):
            self.assertIn(f"'{policy}'::text", original)
            drift = original.replace(f"'{policy}'::text", "'removed-policy'::text")
            try:
                self.cluster.admin_exec(
                    "ALTER TABLE forge_agent.coding_attempts DROP CONSTRAINT coding_attempts_pinned_recipe_check",
                    "ALTER TABLE forge_agent.coding_attempts ADD CONSTRAINT coding_attempts_pinned_recipe_check " + drift,
                )
                result = self.cluster.run_sql_script(script)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            finally:
                self.cluster.admin_exec(
                    "ALTER TABLE forge_agent.coding_attempts DROP CONSTRAINT coding_attempts_pinned_recipe_check",
                    "ALTER TABLE forge_agent.coding_attempts ADD CONSTRAINT coding_attempts_pinned_recipe_check " + original,
                )
        function = self.rows(self.cluster,
            "SELECT pg_get_functiondef('forge_coding_worker_api.claim(text,uuid,text[])'::regprocedure)")[0][0]
        no_new_policy = function.replace("'forge-coding-v3'", "'removed-policy'")
        no_digest, replacements = re.subn(
            r"\(t\.pinned_recipe->>'policy_digest'\)\s*=\s*r\.policy_digest", "true", function,
        )
        self.assertEqual(replacements, 1)
        no_resume_identity = function.replace("'resume_attempt_id'", "'removed-resume-identity'")
        for drift in (no_new_policy, no_digest, no_resume_identity):
            self.assertNotEqual(drift, function)
            try:
                self.cluster.admin_exec(drift)
                result = self.cluster.run_sql_script(script)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("claim policy predicates", result.stderr)
            finally:
                self.cluster.admin_exec(function)
        self.cluster.apply_and_verify()


if __name__ == "__main__":
    unittest.main()
