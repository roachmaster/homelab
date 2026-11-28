"""Real PostgreSQL contracts for bounded native delivery failures; no remote CI."""
from contextlib import closing
import unittest
from uuid import UUID, uuid4

import psycopg2

import test_native_jenkins_sql_contract as native_fixture
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE, SQL_DIR
from work_worker_store import WorkerAccessError, WorkerDB, WorkerStoreError


@unittest.skipUnless(PG16_AVAILABLE, "Requires owned PostgreSQL16")
class NativeDeliveryDatabaseContractTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def setUp(self):
        self.base = native_fixture.NativeJenkinsDatabaseContractTest()
        self.base.cluster = self.cluster
        self.base.setUp()
        self.f = self.base.fixture

    def rows(self, statement, params=()):
        with closing(self.cluster.connect("postgres")) as c, c.cursor() as q:
            q.execute(statement, params)
            return q.fetchall()

    def merge_job(self):
        f = self.f
        admitted, author, critic, checks = f.checked_implementation()
        f.publish(critic, f.verdict(critic))
        f.coordinator.run_once()
        publication = f.db.publication_claim("publisher")
        f.db.publication_finish(publication, "publishing", "Owned fixture", branch="forge/test", commit_sha="d" * 40)
        f.db.publication_finish(publication, "published", "Owned fixture", branch="forge/test", commit_sha="d" * 40,
                                pr_url="https://github.com/fixture/project/pull/1")
        self.build = dict(provider="jenkins", job="forge-native-candidate", action_id=str(uuid4()), queue_id=7, build_number=4)
        f.db.record_hosted_checks(publication["publication_id"], "d" * 40,
            [dict(name=name, status="success", head_sha="d" * 40, **self.build)
             for name in ("check", "pr-fast-gate", "real-browser")])
        f.coordinator.run_once()
        job = f.db.merge_claim("merger")
        self.assertIsNotNone(job)
        delivery = dict(version=1, tested_candidate="d" * 40, current_main="a" * 40,
                        build=self.build, conflict_paths=["src/owned.rs"])
        self.failure = dict(version=1, reason="merge_main_requires_reverification", summary="Actual fixture main requires source repair.",
            source_review_id=str(uuid4()), candidate_commit="c" * 40, candidate_workspace_digest="b" * 64,
            integration_base="a" * 40, gate_manifest_sha256="9" * 64, gate_source_sha256="8" * 64,
            tested_candidate_commit="d" * 40, evidence_digest="7" * 64,
            findings=[dict(finding_id="merge-source", category="source", summary="Resolve the actual main conflict.")],
            build=self.build, no_merge_confirmed=True, delivery_failure=delivery)
        return admitted, author, checks, job

    def test_reconciled_failure_preserves_approval_and_queues_one_same_budget_author(self):
        _, author, checks, job = self.merge_job()
        original = self.rows("SELECT status,owner_decision,finish_receipt,candidate_commit,gate_manifest_sha256 FROM forge_agent.coding_reviews WHERE review_id=%s", (UUID(checks["review_id"]),))
        frozen = self.rows("SELECT snapshot,policy_digest,plan_revisions FROM forge_agent.coding_workflows WHERE workflow_id=%s", (UUID(job["workflow_id"]),))
        receipt = self.f.db.native_merge_failure(job, self.failure)
        self.assertEqual(self.f.db.native_merge_failure(job, self.failure), receipt)
        self.assertEqual(receipt["status"], "repair")
        self.assertEqual(self.rows("SELECT status,owner_decision,finish_receipt,candidate_commit,gate_manifest_sha256 FROM forge_agent.coding_reviews WHERE review_id=%s", (UUID(checks["review_id"]),)), original)
        self.assertEqual(self.rows("SELECT snapshot,policy_digest,plan_revisions FROM forge_agent.coding_workflows WHERE workflow_id=%s", (UUID(job["workflow_id"]),)), frozen)
        seed = self.rows("SELECT status,phase,verification->'checks_passed',verification->'checks',candidate_commit FROM forge_agent.coding_reviews WHERE review_id=%s", (UUID(self.failure["source_review_id"]),))[0]
        self.assertEqual(seed, ("repair", "source", False, [], "c" * 40))
        self.f.coordinator.run_once()
        self.assertEqual(self.f.get(author.task_id)["workflow"]["phase"], "repair")
        child = self.f.claim("agy-v2")
        self.assertEqual(child.task_id, receipt["repair_task_id"])
        self.assertEqual(child.parent_task_id, author.task_id)
        self.assertEqual(child.base_revision, "c" * 40)
        context = self.f.db.read_native_delivery_context(child)
        self.assertEqual(context["delivery_failure"], self.failure["delivery_failure"])
        self.assertEqual(context["candidate_commit"], "c" * 40)
        self.assertEqual(context["source_task_id"], author.task_id)
        self.assertNotIn("prompt", context)
        self.assertEqual(self.rows("SELECT repair_rounds FROM forge_agent.coding_review_budget WHERE root_task_id=%s", (UUID(author.task_id),)), [(1,)])
        self.assertEqual(self.f.db.project_revisions(), {})
        self.assertIsNone(self.f.db.merge_claim("another-merger"))
        with self.assertRaises(WorkerStoreError):
            self.f.db.read_native_delivery_context({**child.to_dict(), "attempt_token": str(uuid4())})

    def test_unknown_merge_ack_wrong_build_and_foreign_or_expired_lease_never_start_writer(self):
        _, _, _, job = self.merge_job()
        invalid = [dict(no_merge_confirmed=False), dict(tested_candidate_commit="f" * 40),
                   dict(build={**self.build, "build_number": 5}), dict(build={**self.build, "job": "untrusted-job"}),
                   dict(delivery_failure={**self.failure["delivery_failure"], "current_main": "f" * 40}),
                   dict(delivery_failure={**self.failure["delivery_failure"], "conflict_paths": ["../credentials"]}),
                   dict(reason="merge_reference_unconfirmed"), dict(version=True), dict(evidence_digest=None)]
        for changed in invalid:
            with self.subTest(changed=changed), self.assertRaises((WorkerAccessError, ValueError)):
                self.f.db.native_merge_failure(job, {**self.failure, **changed})
        for patch in (dict(merge_id=str(uuid4())), dict(lease_token=str(uuid4()))):
            with self.subTest(patch=patch), self.assertRaises(WorkerStoreError):
                self.f.db.native_merge_failure({**job, **patch}, self.failure)
        self.cluster.admin_exec("UPDATE forge_agent.coding_workflow_actions SET lease_expires_at=clock_timestamp()-interval '1 second' WHERE action_id='%s'" % job["merge_id"])
        with self.assertRaises(WorkerStoreError):
            self.f.db.native_merge_failure(job, self.failure)
        self.assertIsNone(self.f.claim("agy-v2"))
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_reviews WHERE phase='source'"), [(0,)])

    def test_exhausted_repair_budget_rolls_back_source_and_failure_receipt(self):
        _, author, _, job = self.merge_job()
        self.cluster.admin_exec("UPDATE forge_agent.coding_review_budget SET repair_rounds=3 WHERE root_task_id='%s'" % author.task_id)
        with self.assertRaises(WorkerStoreError):
            self.f.db.native_merge_failure(job, self.failure)
        self.assertEqual(self.rows("SELECT status,finish_receipt FROM forge_agent.coding_workflow_actions WHERE action_id=%s", (UUID(job["merge_id"]),)), [("leased", None)])
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_reviews WHERE phase='source'"), [(0,)])
        self.assertIsNone(self.f.claim("agy-v2"))

    def test_build_identity_must_match_the_persisted_exact_head_observation(self):
        _, _, _, job = self.merge_job()
        other = {**self.build, "build_number": 5}
        failure = {**self.failure, "build": other,
                   "delivery_failure": {**self.failure["delivery_failure"], "build": other}}
        with self.assertRaises(psycopg2.Error) as caught:
            self.f.db.native_merge_failure(job, failure)
        self.assertEqual(caught.exception.pgcode, "P0006")
        self.assertIsNone(self.f.claim("agy-v2"))
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_reviews WHERE phase='source'"), [(0,)])

    def conflict_handoff(self, revisions=0):
        admitted, author, job = self.base.local_review(revisions=revisions)
        outcome = dict(disposition="handoff", reason="main_merge_conflict", candidate_commit=None,
            gate_manifest_sha256="e" * 64, findings=[], verified_finding_ids=[], fingerprint=None,
            summary="Actual fixture preparation merge conflict.", verification=dict(checks_passed=False,
                cleanup_confirmed=True, checks=[], failure_stage="prepare", exception_type="PublicationError",
                candidate_workspace_digest=job["workspace_digest"], repair_requires_new_workspace=False))
        self.f.db.review_finish(job, outcome)
        self.f.coordinator.run_once()
        revision = self.f.get(author.task_id)["revision"]
        return admitted, author, job, revision

    def test_branch_first_replay_is_once_only_and_preserves_plan_history_and_budget(self):
        _, author, job, revision = self.conflict_handoff(revisions=3)
        before = self.rows("SELECT snapshot,policy_digest,plan_revisions FROM forge_agent.coding_workflows WHERE root_task_id=%s", (UUID(author.task_id),))
        old = self.rows("SELECT status,finish_receipt,infrastructure_retries FROM forge_agent.coding_reviews WHERE review_id=%s", (UUID(job["review_id"]),))
        receipt = self.f.db.native_branch_first_replay(job["review_id"], revision)
        self.assertEqual(self.f.db.native_branch_first_replay(job["review_id"], revision), receipt)
        self.assertEqual(receipt["infrastructure_retries"], 1)
        self.assertEqual(receipt["revision"], revision + 1)
        self.assertEqual(self.rows("SELECT snapshot,policy_digest,plan_revisions FROM forge_agent.coding_workflows WHERE root_task_id=%s", (UUID(author.task_id),)), before)
        self.assertEqual(self.rows("SELECT status,finish_receipt,infrastructure_retries FROM forge_agent.coding_reviews WHERE review_id=%s", (UUID(job["review_id"]),)), old)
        following = self.f.db.review_claim("branch-first-checks")
        self.assertEqual(following["review_id"], receipt["review_id"])
        self.assertEqual(following["attempt_id"], job["attempt_id"])
        self.assertEqual(following["infrastructure_retries"], 1)
        self.assertEqual(following["workspace_digest"], job["workspace_digest"])
        self.assertEqual(self.f.db.project_revisions(), {})

    def test_replay_rejects_wrong_revision_cancelled_scope_and_exhausted_budget(self):
        _, _, job, revision = self.conflict_handoff()
        with self.assertRaises(WorkerStoreError):
            self.f.db.native_branch_first_replay(job["review_id"], revision + 1)
        self.cluster.admin_exec("UPDATE forge_agent.coding_reviews SET infrastructure_retries=1 WHERE review_id='%s'" % job["review_id"])
        with self.assertRaises(WorkerStoreError):
            self.f.db.native_branch_first_replay(job["review_id"], revision)
        self.assertIsNone(self.f.db.review_claim("no-replay"))

    def test_regular_plan_context_is_null_and_preparation_access_is_read_only(self):
        self.f.submit()
        plan = self.f.claim("agy-v2")
        self.assertIsNone(self.f.db.read_native_delivery_context(plan))
        # Additive M54 preparation grants this exact read to the reconciler;
        # mutation authority remains behind the native delivery functions.
        self.assertEqual(self.rows("SELECT version,name FROM forge_agent.schema_migrations WHERE version>=54 ORDER BY version"),
                         [(54,"pg_proxy_preparation")])
        with closing(self.cluster.connect("forge_coding_reconciler")) as c, c.cursor() as q:
            q.execute("SELECT * FROM forge_agent.coding_reviews LIMIT 0")
            self.assertEqual(q.fetchall(), [])
        denied = (("forge_coding_worker", "SELECT forge_coding_reconciler_api.native_branch_first_replay(NULL,1)"),
                  ("forge_coding_reconciler", "INSERT INTO forge_agent.coding_reviews DEFAULT VALUES"),
                  ("forge_coding_reconciler", "UPDATE forge_agent.coding_reviews SET summary=summary WHERE false"),
                  ("forge_coding_reconciler", "DELETE FROM forge_agent.coding_reviews WHERE false"),
                  ("forge_coding_reconciler", "TRUNCATE forge_agent.coding_reviews"))
        for role, query in denied:
            with self.subTest(role=role,query=query), closing(self.cluster.connect(role)) as c, c.cursor() as q:
                with self.assertRaises(psycopg2.errors.InsufficientPrivilege): q.execute(query)

    def test_branch_push_event_is_durable_idempotent_and_bound_to_actual_local_review(self):
        _, _, job = self.base.local_review()
        context = self.f.db.native_ci_acquire_context(job)
        scope = {k: context[k] for k in ("owner_id", "project_alias", "task_id", "attempt_id", "review_id", "workflow_id", "correlation_id")}
        receipt = dict(version="forge-native-branch-v1", scope=scope, candidate_commit="d" * 40,
                       branch_publication_id=job["review_id"], branch="forge/task-" + job["review_id"])
        event = self.f.db.native_branch_published(job, receipt)
        self.assertEqual(self.f.db.native_branch_published(job, receipt), event)
        self.assertEqual(event["correlation_id"], context["correlation_id"])
        stored = self.rows("SELECT event_id,correlation_id,event_kind,payload FROM forge_agent.coding_events WHERE event_id=%s", (UUID(event["event_id"]),))
        self.assertEqual(len(stored), 1)
        self.assertEqual(tuple(map(str, stored[0][:2])), (event["event_id"], event["correlation_id"]))
        self.assertEqual(stored[0][2], "checkpoint")
        self.assertEqual(stored[0][3], {**receipt, "stage": "branch_pushed", "review_id": job["review_id"],
                                        "actor": {"kind": "service", "id": "forge-coding-reconciler"}})
        for changed in (dict(candidate_commit="f" * 40), dict(branch="main"),
                        dict(scope={**scope, "workflow_id": str(uuid4())})):
            with self.subTest(changed=changed), self.assertRaises((WorkerStoreError, ValueError, psycopg2.Error)):
                self.f.db.native_branch_published(job, {**receipt, **changed})
        with self.assertRaises(WorkerStoreError):
            self.f.db.native_branch_published({**job, "lease_token": str(uuid4())}, receipt)
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_events WHERE payload->>'stage'='branch_pushed'"), [(1,)])

    def test_strict_new_function_source_owner_and_acl_pins_reject_drift(self):
        for signature in ("forge_coding_reconciler_api.native_merge_failure(uuid,uuid,jsonb)",
                          "forge_coding_reconciler_api.native_branch_first_replay(uuid,bigint)",
                          "forge_coding_reconciler_api.native_branch_published(uuid,uuid,jsonb)",
                          "forge_coding_worker_api.read_native_delivery_context(uuid,uuid)"):
            for statement in ("ALTER FUNCTION " + signature + " SET search_path=public",
                              "GRANT EXECUTE ON FUNCTION " + signature + " TO PUBLIC"):
                with self.subTest(statement=statement), closing(self.cluster.connect("postgres")) as c:
                    with c.cursor() as q:
                        q.execute(statement)
                        with self.assertRaises(psycopg2.Error):
                            q.execute((SQL_DIR / "management/verify-native-delivery.sql").read_text())
                    c.rollback()


if __name__ == "__main__":
    unittest.main()
