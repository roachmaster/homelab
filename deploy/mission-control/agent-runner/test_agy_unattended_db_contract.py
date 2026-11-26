"""Owned PostgreSQL M23 admission, isolation and unchanged P10 regressions."""

from contextlib import closing
from dataclasses import replace
import hashlib
import json
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from uuid import UUID, uuid4

import psycopg2
from psycopg2.extras import Json

from test_work_execution_and_usage import BASE_REV, make_stop_proof, make_submit, make_v2_config
from test_work_policy_upgrade import PreUpgradeCluster, historical_registry
from work_api import WorkServiceFactory
from work_config import sha256_canonical
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE, SQL_DIR
from work_store import WorkStore
from work_worker_store import WorkerDB
import work_policy


RESOURCE = "forge-coding-agy-unattended-v1"
POLICY_DIGEST = "223cba6fa54a1d6c809ed8ab84d7b96b140962e5dbe026bc28cc3a42a29b3130"
P10_DIGEST = "58a3f1c876630627ee01220b0d45524557cf3f4f685993f027ee31d9fe52d523"


def recipe(**patch):
    value = dict(version=2, backend="antigravity", model="gemini-2.5-pro",
                resource_profile=RESOURCE, mode="plan", generation_token_limit=None,
                policy_digest=POLICY_DIGEST)
    value.update(patch)
    return value


@unittest.skipUnless(PG16_AVAILABLE, "Requires non-root disposable PostgreSQL 16")
class AgYUnattendedDatabaseContractTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def setUp(self):
        self.cluster.reset_coding_tables()

    def rows(self, statement, params=(), *, cluster=None):
        with closing((cluster or self.cluster).connect("postgres")) as c, c.cursor() as q:
            q.execute(statement, params)
            return q.fetchall()

    def admit(self, value, *, profile="agy-unattended", cluster=None, auto_repair=False):
        task_id = uuid4()
        with closing((cluster or self.cluster).connect("forge_agent")) as c, c, c.cursor() as q:
            q.execute("""INSERT INTO forge_agent.coding_tasks (
                owner_id,project_alias,id,idempotency_key,intent_digest,base_revision,
                mode,agent_profile,title,prompt,acceptance_criteria,pinned_recipe,recipe_digest,auto_repair)
                VALUES('mission-control','homelab',%s,%s,%s,%s,%s,%s,
                       'Owned contract fixture','Owned contract fixture','Verify ownership',%s,%s,%s)""",
                (task_id, str(uuid4()), "a" * 64, BASE_REV, value.get("mode", "plan"),
                 profile, Json(value), sha256_canonical(value), auto_repair))
            q.execute("""INSERT INTO forge_agent.coding_events(
                owner_id,project_alias,task_id,sequence,event_kind,payload)
                VALUES('mission-control','homelab',%s,1,'queued',%s)""",
                (task_id, Json(dict(task_id=str(task_id), project_alias="homelab",
                                   revision=1, status="queued", recipe_digest=sha256_canonical(value)))))
        return str(task_id)

    def worker(self, *, cluster=None):
        selected = cluster or self.cluster
        return WorkerDB(lambda: selected.connect("forge_coding_worker"), "mission-control",
                        reconcile_connect_factory=lambda: selected.connect("forge_coding_reconciler"))

    def advertise(self, worker_id, digest, *, backend="antigravity", model="gemini-2.5-pro",
                  modes=("plan",), profile="agy-unattended", cluster=None):
        selected = cluster or self.cluster
        selected.grant_worker("mission-control", "homelab", profile, modes)
        incarnation = str(uuid4())
        self.worker(cluster=selected).advertise(worker_id, incarnation,
            [dict(alias="homelab", base_revision=BASE_REV)],
            [dict(alias=profile, backend=backend, model=model, modes=list(modes))], digest)
        return incarnation

    def test_resource_is_exactly_v2_agy_and_rejects_other_backend_or_policy(self):
        valid = recipe()
        for mode in ("plan", "review", "implement"):
            with self.subTest(valid_mode=mode):
                self.admit({**valid, "mode": mode})
        mutants = [
            {**valid, "backend": backend} for backend in ("codex", "grok", "muse")
        ] + [
            {**valid, "policy_digest": P10_DIGEST},
            {**valid, "policy_digest": "bad"},
            {key: value for key, value in {**valid, "version": 1}.items() if key != "policy_digest"},
            {**valid, "generation_token_limit": 8192},
            {**valid, "extra": "unapproved"},
            {**valid, "model": " "},
            {**valid, "model": "model\nunsafe"},
        ]
        for value in mutants:
            with self.subTest(invalid=value), self.assertRaises(psycopg2.errors.CheckViolation):
                self.admit(value)

    def test_native_claim_requires_exact_digest_and_advertised_agy_model_mode(self):
        task = self.admit(recipe())
        db = self.worker()
        old = self.advertise("p10", P10_DIGEST)
        self.assertIsNone(db.claim("p10", old, ["agy-unattended"]))
        mismatches = [dict(backend="codex"), dict(model="wrong-model"), dict(modes=("review",))]
        for index, patch in enumerate(mismatches):
            name = "mismatch-" + str(index)
            incarnation = self.advertise(name, POLICY_DIGEST, **patch)
            with self.subTest(advertisement=patch):
                self.assertIsNone(db.claim(name, incarnation, ["agy-unattended"]))
        ready = self.advertise("unattended", POLICY_DIGEST)
        lease = db.claim("unattended", ready, ["agy-unattended"])
        self.assertEqual(lease.task_id, task)
        self.assertEqual(lease.recipe["policy_digest"], POLICY_DIGEST)
        self.assertEqual(lease.recipe["backend"], "antigravity")
        self.assertEqual(lease.recipe["resource_profile"], RESOURCE)

    def test_attempt_constraint_rejects_new_identifier_for_other_backends(self):
        task = self.admit(recipe())
        ready = self.advertise("unattended", POLICY_DIGEST)
        lease = self.worker().claim("unattended", ready, ["agy-unattended"])
        self.assertEqual(lease.task_id, task)
        for backend in ("codex", "grok", "muse"):
            with self.subTest(backend=backend), closing(self.cluster.connect("postgres")) as c:
                with self.assertRaises(psycopg2.errors.CheckViolation), c, c.cursor() as q:
                    attempt = uuid4()
                    q.execute("""INSERT INTO forge_agent.coding_attempts
                        SELECT (jsonb_populate_record(NULL::forge_agent.coding_attempts,
                          to_jsonb(a)||jsonb_build_object('attempt_id',%s::uuid,
                            'attempt_token',%s::uuid,'attempt_no',999,'unit_name',%s,
                            'pinned_recipe',%s::jsonb))).*
                        FROM forge_agent.coding_attempts a WHERE a.attempt_id=%s""",
                        (attempt, uuid4(), "forge-coding-" + attempt.hex,
                         Json({**dict(lease.recipe), "backend": backend}), UUID(lease.attempt_id)))

    def test_current_verifier_rejects_removed_guard_and_changed_claim(self):
        for statement in (
            "ALTER TABLE forge_agent.coding_tasks DROP CONSTRAINT coding_tasks_agy_unattended_recipe_check",
            "ALTER FUNCTION forge_coding_worker_api.claim(text,uuid,text[]) SET search_path=public",
        ):
            with self.subTest(drift=statement), closing(self.cluster.connect("postgres")) as c:
                with c.cursor() as q:
                    q.execute(statement)
                    with self.assertRaises(psycopg2.Error):
                        q.execute((SQL_DIR / "management/verify-agy-unattended.sql").read_text())
                c.rollback()

    def test_unattended_direct_implementation_retains_native_feedback_semantics(self):
        task = self.admit(recipe(mode="implement"), auto_repair=True)
        incarnation = self.advertise("unattended", POLICY_DIGEST, modes=("implement",))
        db = self.worker()
        lease = db.claim("unattended", incarnation, ["agy-unattended"])
        self.assertEqual(lease.task_id, task)
        text = "An owned unattended implementation fixture."
        db.append_result(lease, 0, text)
        proof = make_stop_proof(lease.unit_name)
        db.publish(lease, "awaiting_review", dict(sha256=hashlib.sha256(text.encode()).hexdigest(),
                   byte_count=len(text.encode()), chunk_count=1), None, "Owned fixture", proof)
        receipt = db.reconcile(lease, "c" * 64, proof)
        review = db.queue_review(lease, receipt)
        job = db.review_claim("reviewer")
        self.assertEqual(job["review_id"], review)
        self.assertTrue(job["direct_implementation"])
        finding = dict(finding_id="edge", category="source", summary="Repair the owned fixture.")
        outcome = dict(cleanup_confirmed=True, disposition="repair", candidate_commit="d" * 40,
                       gate_manifest_sha256="e" * 64, findings=[finding], verified_finding_ids=[],
                       fingerprint="1" * 64, summary="A repair is required.", reason="Owned fixture",
                       verification=dict(checks_passed=False, candidate_workspace_digest="f" * 64,
                                         repair_requires_new_workspace=False))
        decision = db.review_finish(job, outcome)
        self.assertEqual(decision["repair_task_id"], task)
        following = db.claim("unattended", incarnation, ["agy-unattended"])
        packet = db.read_feedback(following)
        feedback = json.loads(packet["feedback_json"])
        self.assertEqual(feedback["workspace_digest_kind"], "native_receipt")
        self.assertEqual(following.recipe["resource_profile"], RESOURCE)
        self.assertEqual(following.recipe["policy_digest"], POLICY_DIGEST)

    def test_schema22_upgrade_preserves_receipts_and_old_p10_worker_remains_usable(self):
        with TemporaryDirectory(prefix="forge-agy-policy-upgrade-") as temporary:
            legacy = Path(temporary) / "sql"
            historical_registry(legacy, 22)
            cluster = PreUpgradeCluster(legacy).start()
            try:
                # Reproduce the admission policy that existed on schema22.
                config = replace(make_v2_config(),repair_policy={'max_rounds':3,'max_stagnant_rounds':2,'max_infrastructure_retries':1})
                service = WorkServiceFactory(config, lambda: cluster.connect("forge_agent")).create()
                db = self.worker(cluster=cluster)
                ready = self.advertise("old-p10", P10_DIGEST, backend="codex", model="gpt-5.4",
                                       profile="codex-v2", cluster=cluster)
                submitted = service.submit(make_submit(idempotency_key="pre-m23"))
                lease = db.claim("old-p10", ready, ["codex-v2"])
                result = "An immutable original P10 result."
                db.append_result(lease, 0, result)
                manifest = dict(sha256=hashlib.sha256(result.encode()).hexdigest(),
                                byte_count=len(result.encode()), chunk_count=1)
                proof = make_stop_proof(lease.unit_name)
                receipt = db.publish(lease, "awaiting_review", manifest, None,
                                     "Owned policy upgrade fixture", proof)
                snapshot_query = """SELECT
                    (SELECT jsonb_agg(to_jsonb(t) ORDER BY t.id) FROM forge_agent.coding_tasks t),
                    (SELECT jsonb_agg(to_jsonb(a) ORDER BY a.attempt_id) FROM forge_agent.coding_attempts a),
                    (SELECT jsonb_agg(to_jsonb(e) ORDER BY e.task_id,e.sequence) FROM forge_agent.coding_events e),
                    (SELECT jsonb_agg(to_jsonb(r) ORDER BY r.attempt_id,r.chunk_index) FROM forge_agent.coding_result_chunks r)"""
                before = self.rows(snapshot_query, cluster=cluster)
                DisposablePostgresCluster.apply_and_verify(cluster)
                DisposablePostgresCluster.apply_and_verify(cluster)
                self.assertEqual(self.rows(snapshot_query, cluster=cluster), before)
                self.assertEqual(self.rows("SELECT max(version) FROM forge_agent.schema_migrations", cluster=cluster), [(45,)])
                self.assertEqual(db.publish(lease, "awaiting_review", manifest, None,
                                           "Owned policy upgrade fixture", proof), receipt)
                self.assertEqual(service.submit(make_submit(idempotency_key="pre-m23")), submitted)
                later = service.submit(make_submit(idempotency_key="after-m23"))
                claimed = db.claim("old-p10", ready, ["codex-v2"])
                self.assertEqual(claimed.task_id, later["task_id"])
                self.assertEqual(claimed.recipe["policy_digest"], P10_DIGEST)
                self.assertEqual(work_policy.POLICY_DIGEST, P10_DIGEST)
                self.assertEqual(work_policy.POLICY_ID, "forge-coding-v10")
            finally:
                cluster.stop()


if __name__ == "__main__":
    unittest.main()
