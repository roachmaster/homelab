"""Owned native CI source/status contracts; no real Jenkins or GitHub calls."""

from contextlib import closing
from dataclasses import replace
import unittest
from uuid import UUID, uuid4

import psycopg2

import test_work_workflow as workflow_fixture
import test_work_publication as publication_fixture
from work_api import WorkCancel, WorkServiceFactory
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE, SQL_DIR
from work_policy import AGY_POLICY_ID, AGY_POLICY_DIGEST
from work_publication_clients import WorkspaceSnapshotProvider
from work_source_revision import VERIFIED_REF
from work_worker_store import WorkerAccessError, WorkerDB


@unittest.skipUnless(PG16_AVAILABLE, "Requires owned PostgreSQL16")
class NativeJenkinsDatabaseContractTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def setUp(self):
        self.fixture = workflow_fixture.AutonomousWorkflowTest()
        self.fixture.cluster = self.cluster
        self.fixture.setUp()
        f = self.fixture
        f.config = replace(f.config, profiles={**f.config.profiles,
            "agy-v2": replace(f.config.profiles["agy-v2"], resource_profile=AGY_POLICY_ID)})
        f.service = WorkServiceFactory(f.config, lambda: self.cluster.connect("forge_agent")).create()
        f.coordinator = f.service.workflow_worker
        incarnation = str(uuid4())
        f.db.advertise("unattended-author", incarnation,
            [dict(alias="homelab", base_revision="a" * 40)],
            [dict(alias="agy-v2", backend="antigravity", model=f.config.profiles["agy-v2"].model,
                  modes=["plan", "review", "implement"])], AGY_POLICY_DIGEST)
        f.claim = lambda profile: f.db.claim(
            "unattended-author" if profile == "agy-v2" else "workflow-worker",
            incarnation if profile == "agy-v2" else f.incarnation, [profile])

    def rows(self, statement, params=()):
        with closing(self.cluster.connect("postgres")) as c, c.cursor() as q:
            q.execute(statement, params)
            return q.fetchall()

    def local_review(self, revisions=0):
        f = self.fixture
        _, admitted = f.submit()
        for _ in range(revisions):
            f.finish_plan_round("revise")
        f.finish_plan_round("approve")
        author = f.claim("agy-v2")
        receipt = f.publish(author, "An owned implementation fixture.")
        f.db.queue_review(author, receipt)
        f.coordinator.run_once()
        job = f.db.review_claim("checks")
        self.assertIsNotNone(job)
        self.assertEqual(job["phase"], "local")
        self.assertNotIn("workflow_id", job)
        self.assertNotIn("correlation_id", job)
        return admitted, author, job

    def finish_local(self, job):
        self.fixture.db.review_finish(job, dict(
            disposition="awaiting_review", candidate_commit="d" * 40,
            gate_manifest_sha256="e" * 64, findings=[], verified_finding_ids=[],
            fingerprint=None, summary="Owned SQL fixture; no Jenkins invocation.", cleanup_confirmed=True,
            verification=dict(checks_passed=True, candidate_workspace_digest="f" * 64,
                repair_requires_new_workspace=False, cleanup_confirmed=True,
                integration_base="a" * 40, toolchain="fixture",
                checks=[dict(check_id="make-ci", status="passed", head_sha="d" * 40)])))

    @staticmethod
    def key(job):
        return {"version": 1, **{name: job[name] for name in
            ("project_alias", "review_id", "task_id", "attempt_id")}}

    def test_acquisition_returns_real_ids_without_changing_frozen_policy_or_counters(self):
        admitted, author, job = self.local_review(revisions=3)
        before = self.rows("SELECT policy_digest,snapshot,plan_revisions FROM forge_agent.coding_workflows WHERE root_task_id=%s", (UUID(author.task_id),))
        context = self.fixture.db.native_ci_acquire_context({**job, "workflow_id": str(uuid4()),
            "correlation_id": str(uuid4()), "unrelated": "ignored"})
        actual = self.rows("SELECT workflow_id,correlation_id FROM forge_agent.coding_tasks WHERE id=%s", (UUID(author.task_id),))[0]
        self.assertEqual((context["workflow_id"], context["correlation_id"]), tuple(map(str, actual)))
        self.assertEqual(context["state"], "active")
        self.assertTrue(context["active"])
        self.assertNotIn("lease_token", context)
        self.assertNotIn("prompt", context)
        self.assertEqual(self.fixture.db.native_ci_binding(self.key(job)), context)
        self.assertEqual(self.rows("SELECT policy_digest,snapshot,plan_revisions FROM forge_agent.coding_workflows WHERE root_task_id=%s", (UUID(author.task_id),)), before)
        self.assertEqual(before[0][2], 3)
        self.assertEqual(self.fixture.db.project_revisions(), {})

    def test_new_context_requires_current_review_lease_but_status_survives_lease_release(self):
        _, _, job = self.local_review()
        with self.assertRaises(WorkerAccessError):
            self.fixture.db.native_ci_acquire_context({**job, "lease_token": str(uuid4())})
        context = self.fixture.db.native_ci_acquire_context(job)
        self.finish_local(job)
        fresh = WorkerDB(lambda: self.cluster.connect("forge_coding_worker"), "mission-control",
                        reconcile_connect_factory=lambda: self.cluster.connect("forge_coding_reconciler"))
        binding = fresh.native_ci_binding(self.key(job))
        self.assertTrue(binding["active"])
        self.assertEqual(binding["workflow_id"], context["workflow_id"])
        self.assertEqual(binding["candidate_commit"], "d" * 40)
        with self.assertRaises(WorkerAccessError):
            fresh.native_ci_acquire_context(job)

    def test_cancellation_is_visible_after_restart_without_claiming_or_renewing(self):
        admitted, author, job = self.local_review()
        self.fixture.db.native_ci_acquire_context(job)
        detail = self.fixture.get(author.task_id)
        self.fixture.service.cancel(WorkCancel("homelab", UUID(author.task_id), detail["revision"], "cancel-ci"))
        fresh = WorkerDB(lambda: self.cluster.connect("forge_coding_worker"), "mission-control",
                        reconcile_connect_factory=lambda: self.cluster.connect("forge_coding_reconciler"))
        binding = fresh.native_ci_binding(self.key(job))
        self.assertFalse(binding["active"])
        self.assertEqual(binding["state"], "cancelled")
        with self.assertRaises(WorkerAccessError):
            fresh.native_ci_acquire_context(job)
        self.assertEqual(self.fixture.get(admitted["task_id"])["workflow"]["phase"], "cancelled")

    def test_expired_review_lease_is_inactive_and_cannot_acquire_a_new_build(self):
        _, _, job = self.local_review()
        self.assertTrue(self.fixture.db.native_ci_acquire_context(job)["active"])
        self.cluster.admin_exec("UPDATE forge_agent.coding_reviews SET lease_expires_at=clock_timestamp()-interval '1 second' WHERE review_id='%s'" % job["review_id"])
        binding = self.fixture.db.native_ci_binding(self.key(job))
        self.assertFalse(binding["active"])
        self.assertEqual(binding["state"], "inactive")
        with self.assertRaises(WorkerAccessError):
            self.fixture.db.native_ci_acquire_context(job)

    def test_superseded_attempt_is_visible_after_normal_peer_repair(self):
        _, _, job = self.local_review()
        self.finish_local(job)
        f = self.fixture
        f.coordinator.run_once()
        critic = f.claim("codex-v2")
        f.publish(critic, f.verdict(critic, "revise"))
        f.coordinator.run_once()
        following = f.claim("agy-v2")
        self.assertIsNotNone(following)
        self.assertNotEqual(following.attempt_id, job["attempt_id"])
        binding = f.db.native_ci_binding(self.key(job))
        self.assertFalse(binding["active"])
        self.assertEqual(binding["state"], "superseded")

    def test_foreign_bindings_and_native_worker_direct_access_are_denied(self):
        _, _, job = self.local_review()
        for patch in (dict(project_alias="other"), dict(task_id=str(uuid4())),
                      dict(attempt_id=str(uuid4())), dict(review_id=str(uuid4()))):
            with self.subTest(patch=patch), self.assertRaises(WorkerAccessError):
                self.fixture.db.native_ci_binding({**self.key(job), **patch})
        foreign = WorkerDB(lambda: self.cluster.connect("forge_coding_worker"), "another-owner",
                           reconcile_connect_factory=lambda: self.cluster.connect("forge_coding_reconciler"))
        with self.assertRaises(WorkerAccessError):
            foreign.native_ci_binding(self.key(job))
        worker_only = WorkerDB(lambda: self.cluster.connect("forge_coding_worker"), "mission-control",
                               reconcile_connect_factory=lambda: self.cluster.connect("forge_coding_worker"))
        with self.assertRaises(WorkerAccessError):
            worker_only.native_ci_binding(self.key(job))
        for relation in ("coding_tasks", "coding_attempts", "coding_workflows", "coding_reviews"):
            self.assertFalse(self.rows("SELECT has_table_privilege('forge_coding_reconciler',%s,'SELECT')", ("forge_agent." + relation,))[0][0])

    def test_provider_neutral_gate_observations_preserve_frozen_workflow(self):
        _, author, job = self.local_review(revisions=1)
        frozen = self.rows("SELECT policy_digest,snapshot,plan_revisions FROM forge_agent.coding_workflows WHERE root_task_id=%s", (UUID(author.task_id),))
        self.finish_local(job)
        f = self.fixture
        f.coordinator.run_once()
        critic = f.claim("codex-v2")
        f.publish(critic, f.verdict(critic))
        f.coordinator.run_once()
        publication = f.db.publication_claim("publisher")
        self.assertIsNotNone(publication)
        f.db.publication_finish(publication, "publishing", "Owned fixture", branch="forge/fixture", commit_sha="d" * 40)
        f.db.publication_finish(publication, "published", "Owned fixture", branch="forge/fixture", commit_sha="d" * 40,
                               pr_url="https://github.com/fixture/project/pull/1")
        names = ("check", "pr-fast-gate", "real-browser")
        checks = [dict(name=name, status="success", head_sha="d" * 40, provider="jenkins",
                       evidence_digest="1" * 64) for name in names]
        incomplete = f.db.record_hosted_checks(publication["publication_id"], "d" * 40, checks[:-1])
        self.assertFalse(incomplete["required_green"])
        f.coordinator.run_once()
        self.assertIsNone(f.db.merge_claim("merger"))
        with self.assertRaises(ValueError):
            f.db.record_hosted_checks(publication["publication_id"], "d" * 40,
                                      [{**item, "head_sha": "c" * 40} for item in checks])
        complete = f.db.record_hosted_checks(publication["publication_id"], "d" * 40, checks)
        self.assertTrue(complete["required_green"])
        f.coordinator.run_once()
        self.assertIsNotNone(f.db.merge_claim("merger"))
        self.assertEqual(self.rows("SELECT policy_digest,snapshot,plan_revisions FROM forge_agent.coding_workflows WHERE root_task_id=%s", (UUID(author.task_id),)), frozen)
        self.assertEqual(f.db.project_revisions(), {})  # CI green alone is not a merge/source receipt.

    def test_strict_context_verifier_rejects_function_or_acl_drift(self):
        signature = "forge_coding_reconciler_api.native_ci_status(uuid,text,uuid,uuid)"
        for statement in ("ALTER FUNCTION " + signature + " SET search_path=public",
                          "GRANT EXECUTE ON FUNCTION " + signature + " TO PUBLIC"):
            with self.subTest(statement=statement), closing(self.cluster.connect("postgres")) as c:
                with c.cursor() as q:
                    q.execute(statement)
                    with self.assertRaises(psycopg2.Error):
                        q.execute((SQL_DIR / "management/verify-native-jenkins.sql").read_text())
                c.rollback()


class NativeCurrentMainIntegrationTest(unittest.TestCase):
    def test_current_main_is_merged_before_checks_and_verified_authority_does_not_move(self):
        with publication_fixture.fixture() as values:
            _, config, job, workspaces, _, bare, git, *_ = values
            canonical = config.worker.projects["homelab"]
            base = job["base_revision"]
            git._git(canonical, "update-ref", VERIFIED_REF, base)
            (canonical / "newer-main.txt").write_text("Keep all manually published main work.\n")
            git._git(canonical, "add", "newer-main.txt")
            git._git(canonical, "commit", "--quiet", "-m", "newer independently published main")
            current_main = git._git(canonical, "rev-parse", "HEAD").decode().strip()
            git._git(canonical, "push", "--quiet", "--", str(bare), "main")
            snapshot = WorkspaceSnapshotProvider(workspaces).create(job)
            repository, candidate = git.prepare(job, snapshot)
            self.assertEqual((repository / "newer-main.txt").read_text(), "Keep all manually published main work.\n")
            self.assertEqual((repository / "app.py").read_text(), "implemented\n")
            self.assertEqual(git._git(repository, "rev-parse", "FETCH_HEAD").decode().strip(), current_main)
            git._git(repository, "merge-base", "--is-ancestor", current_main, candidate)
            self.assertEqual(git._git(canonical, "rev-parse", VERIFIED_REF).decode().strip(), base)
            self.assertEqual(git._git(canonical, "rev-parse", "HEAD").decode().strip(), current_main)


if __name__ == "__main__":
    unittest.main()
