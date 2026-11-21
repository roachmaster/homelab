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
from work_api import WorkAccept, WorkGet, WorkResume, WorkPlanDecide, WorkServiceFactory
from work_config import PinnedRecipe, sha256_canonical
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE, SQL_DIR
from work_store import WorkStore, _accept_intent_digest, _submit_intent_digest
from work_config import prepare_title
from psycopg2.extras import Json
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
SESSION_POLICY_ID = "forge-coding-v3"
SESSION_POLICY = {key: value for key, value in PREVIOUS_POLICY.items() if key != "fresh_session"}
SESSION_POLICY.update(id=SESSION_POLICY_ID,
    antigravity_session_policy="fresh-unless-receipt-owned-task-resume-v1",codex_session_policy="ephemeral-v1")
SESSION_POLICY_DIGEST = sha256_canonical(SESSION_POLICY)
PLAN_POLICY_ID='forge-coding-v4'
PLAN_POLICY={**SESSION_POLICY,'id':PLAN_POLICY_ID,'plan_workflow':'artifact-bound-plan-decisions-publication-v1'}
PLAN_POLICY_DIGEST=sha256_canonical(PLAN_POLICY)
FEEDBACK_POLICY_ID='forge-coding-v5'
FEEDBACK_POLICY={**PLAN_POLICY,'id':FEEDBACK_POLICY_ID,'feedback_workflow':'lease-bound-immutable-feedback-review-v1'}
FEEDBACK_POLICY_DIGEST=sha256_canonical(FEEDBACK_POLICY)
PEER_POLICY_ID="forge-coding-v6"
PEER_POLICY={**FEEDBACK_POLICY,"id":PEER_POLICY_ID,"peer_review_workflow":"source-bound-readonly-peer-review-v1"}
PEER_POLICY_DIGEST=sha256_canonical(PEER_POLICY)
EXTERNAL_SANDBOX_POLICY_ID="forge-coding-v7"
EXTERNAL_SANDBOX_POLICY={
    **PEER_POLICY,
    "id":EXTERNAL_SANDBOX_POLICY_ID,
    "codex_sandbox":"supervised-external-v1",
    "codex_external_io":"host-network-staged-native-state-v1",
}
EXTERNAL_SANDBOX_POLICY_DIGEST=sha256_canonical(EXTERNAL_SANDBOX_POLICY)
WARM_POLICY_ID="forge-coding-v8"
WARM_POLICY={**EXTERNAL_SANDBOX_POLICY,"id":WARM_POLICY_ID,
 "codex_session_policy":"fresh-unless-receipt-owned-author-resume-ttl-v1",
 "codex_feature_controls":"no-user-config-no-multi-agent-no-fast-mode-v1"}
WARM_POLICY_DIGEST=sha256_canonical(WARM_POLICY)
GROK_POLICY_ID="forge-coding-v9"
GROK_POLICY={**WARM_POLICY,"id":GROK_POLICY_ID,
 "adapter_versions":{"antigravity":"1.5.12","codex":"0.160.0","grok":"1.0.92"},
 'grok_session_policy':'fresh-attempt-session-id-v1',
 'grok_execution_policy':'dontask-trusted-instructions-managed-only-supervised-v1',
 'grok_project_policy':'plain-instructions-reject-project-executable-config-v1',
 'grok_config_sha256':'478dc64ed679f9d5d91ec0a0b051f3dbb4406a84b280594e3e00871d09b5da34',
 'grok_managed_policy_sha256':'bdd376cc07a776d50d04e906e25a15b0057e1f6d421aa9c0fdf7336592a8beaf',
 'grok_state_policy':'isolated-home-no-shared-leader-auth-only-v1',
 'grok_auth_cleanup_policy':'retained-dirfd-inode-after-proven-stop-v1',
 'grok_binary_sha256':'add4d201dbf2c0bd6077a36feb55a76939de30bae48dbb06f16b2307d38ceacb',
}
GROK_POLICY_DIGEST=sha256_canonical(GROK_POLICY)




def historical_registry(root, maximum):
    """Copy a historical registry/grant surface without modifying migrations."""
    shutil.copytree(SQL_DIR, root)
    registry=root/"apply.sql"
    script=registry.read_text()
    script=re.sub(r"SELECT NOT EXISTS \(\s*SELECT 1 FROM forge_agent\.schema_migrations WHERE version\s*=\s*(\d+)\s*\).*?\\endif\s*",
                  lambda match: "" if int(match.group(1))>maximum else match.group(0),script,flags=re.DOTALL)
    registry.write_text(script)
    grant=root/"management/grant-runner.sql"
    value=grant.read_text()
    if maximum<16:value=value.split("-- M16:")[0]
    if maximum<15:value=value.split("-- M15:")[0]
    value=value.replace(",\n    forge_agent.coding_plan_decisions TO forge_agent;"," TO forge_agent;")
    value="\n".join(line for line in value.splitlines() if not any(token in line for token in (
        "GRANT SELECT ON forge_agent.coding_publications", "forge_coding_worker_api.read_plan(",
        "forge_coding_worker_api.queue_publication(", "forge_coding_reconciler_api.publication_")))+"\n"
    value=value.replace("forge_coding_reconciler_api.reconcile(uuid, uuid, text, jsonb),\nTO", "forge_coding_reconciler_api.reconcile(uuid, uuid, text, jsonb)\nTO")
    if maximum<12:value=value.replace("    forge_coding_worker_api.report_progress(uuid, uuid, integer, text, text),\n","")
    grant.write_text(value)


class PolicyIdentityTest(unittest.TestCase):
    def test_session_policy_is_versioned_without_changing_historical_digest_identity(self):
        self.assertEqual(WARM_POLICY_DIGEST,"03af7fd6e897a67254bd780a44536366b8dc11fdce307a44a29ddfb5a958cb33")
        self.assertEqual(OLD_POLICY_DIGEST, "2465fea4c303e83825339fd15793927a80b147519586519283e7534b2ad3be44")
        self.assertEqual(PREVIOUS_POLICY_DIGEST, "67065fb04466901c8c0df1faad5d7a04503322c6e4c1b8a75ca08eba1375fa58")
        self.assertEqual(SESSION_POLICY_DIGEST,"e619dbab071b1d1b0e3fa0cf6e6212e879c953825d6c80da5f4de048fb787075")
        self.assertEqual(work_policy.POLICY_ID, "forge-coding-v10")
        self.assertEqual(GROK_POLICY_DIGEST,"8191ffeda267b598c9d550a81e3f272d706c00e37731c7612c8ca3cb7d4ecf0e")
        self.assertEqual(work_policy.POLICY_DIGEST,"58a3f1c876630627ee01220b0d45524557cf3f4f685993f027ee31d9fe52d523")
        self.assertEqual(PEER_POLICY_DIGEST,"78d8f96280b9b75597e2b10276e1854f64026ff8f359f0f2abb65113774a8e5e")
        self.assertEqual(EXTERNAL_SANDBOX_POLICY_DIGEST,"40b4e50c9acf64a835fc477a8748fb1cf266188c46f6ac943dd075d506df6124")
        self.assertEqual(work_policy.POLICY["codex_sandbox"],"supervised-external-v1")
        self.assertEqual(work_policy.POLICY["codex_external_io"],"host-network-staged-native-state-v1")
        self.assertEqual(PLAN_POLICY_DIGEST,"b541291edbadf6390f1a46f1f8f82d5b090f9942aaf2b57ced50ef3f779ab5e4")
        self.assertEqual(work_policy.POLICY["feedback_workflow"],"lease-bound-immutable-feedback-review-v1")
        self.assertEqual(work_policy.POLICY["plan_workflow"], "artifact-bound-plan-decisions-publication-v1")
        self.assertEqual(work_policy.POLICY["antigravity_session_policy"], "fresh-unless-receipt-owned-task-resume-v1")
        self.assertEqual(work_policy.POLICY["codex_session_policy"], "fresh-unless-receipt-owned-author-resume-ttl-v1")
        self.assertEqual(work_policy.POLICY["codex_feature_controls"], "no-user-config-no-multi-agent-no-fast-mode-v1")
        self.assertNotIn("fresh_session", work_policy.POLICY)
        self.assertNotIn(work_policy.POLICY_DIGEST, (OLD_POLICY_DIGEST, PREVIOUS_POLICY_DIGEST, EXTERNAL_SANDBOX_POLICY_DIGEST))
        comparable = {key: value for key, value in work_policy.POLICY.items()
                      if not key.startswith(("grok_", "muse_")) and key not in {"id", "antigravity_session_policy", "codex_session_policy", "codex_feature_controls", "plan_workflow", "feedback_workflow", "peer_review_workflow", "codex_sandbox", "codex_external_io"}}
        comparable["adapter_versions"] = {k:v for k,v in comparable["adapter_versions"].items() if k not in {"grok","muse"}}
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
        digest = {
            OLD_POLICY_ID: OLD_POLICY_DIGEST,
            PREVIOUS_POLICY_ID: PREVIOUS_POLICY_DIGEST,
            SESSION_POLICY_ID: SESSION_POLICY_DIGEST,
            PLAN_POLICY_ID: PLAN_POLICY_DIGEST,
            FEEDBACK_POLICY_ID: FEEDBACK_POLICY_DIGEST,
            PEER_POLICY_ID: PEER_POLICY_DIGEST,
            EXTERNAL_SANDBOX_POLICY_ID: EXTERNAL_SANDBOX_POLICY_DIGEST,
            WARM_POLICY_ID: WARM_POLICY_DIGEST,
            GROK_POLICY_ID: GROK_POLICY_DIGEST,
        }[policy]
        payload = {
            "version": 2, "backend": "codex", "model": "gpt-5.4",
            "resource_profile": policy, "mode": "plan", "generation_token_limit": None,
            "policy_digest": digest,
        }
        return PinnedRecipe(**payload, digest=sha256_canonical(payload))

    def admit_old(self, cluster, key, policy=OLD_POLICY_ID):
        command=make_submit(idempotency_key=key)
        version=self.rows(cluster,'SELECT max(version) FROM forge_agent.schema_migrations')[0][0]
        with closing(cluster.connect('forge_agent')) as connection,connection,connection.cursor() as cursor:
            # The old application used the pre-M15 columns. This owned fixture
            # seeds that historic admission surface; the migrated replay below
            # deliberately exercises the current production store.
            if version>=15:
                return self.store(cluster).submit(**vars(command),resolve_recipe=lambda:self.old_recipe(policy))
            recipe=self.old_recipe(policy);task_id=uuid4()
            receipt={'task_id':str(task_id),'project_alias':'homelab','revision':1,'status':'queued','recipe_digest':recipe.digest}
            intent=_submit_intent_digest(project_alias=command.project_alias,base_revision=command.base_revision,mode=command.mode,
                agent_profile=command.agent_profile,prompt=command.prompt,acceptance_criteria=command.acceptance_criteria)
            cursor.execute("""INSERT INTO forge_agent.coding_tasks(owner_id,project_alias,id,idempotency_key,intent_digest,base_revision,
                mode,agent_profile,title,prompt,acceptance_criteria,pinned_recipe,recipe_digest,status,revision)
                VALUES('mission-control',%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,'queued',1)""",
                (command.project_alias,task_id,key,intent,command.base_revision,command.mode,command.agent_profile,prepare_title(command.prompt),
                 command.prompt,command.acceptance_criteria,Json(recipe.to_dict()),recipe.digest))
            cursor.execute("""INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload)
                VALUES('mission-control',%s,%s,1,'queued',%s)""",(command.project_alias,task_id,Json(receipt)))
            return receipt

    def legacy_accept(self, cluster, command):
        receipt={"project_alias":command.project_alias,"task_id":str(command.task_id),
                 "base_revision":command.expected_revision,"revision":command.expected_revision+1,"status":"completed"}
        intent=_accept_intent_digest(project_alias=command.project_alias,task_id=command.task_id,
                                    expected_revision=command.expected_revision,review_note=command.review_note)
        with closing(cluster.connect("postgres")) as c,c.cursor() as cursor:
            with c:
                cursor.execute("UPDATE forge_agent.coding_tasks SET status='completed',accepted_at=clock_timestamp(),revision=revision+1 "
                               "WHERE id=%s AND revision=%s",(command.task_id,command.expected_revision))
                cursor.execute("INSERT INTO forge_agent.coding_control_receipts(owner_id,project_alias,task_id,action,idempotency_key,intent_digest,base_revision,result_revision,receipt_json) "
                               "VALUES('mission-control',%s,%s,'accept',%s,%s,%s,%s,%s)",
                               (command.project_alias,command.task_id,command.idempotency_key,intent,command.expected_revision,command.expected_revision+1,Json(receipt)))
        return receipt

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
            historical_registry(legacy_sql,10)
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
                    cluster, "SELECT to_jsonb(t)-ARRAY['plan_decision_id','feedback_id','feedback_sha256','legacy_resume_note','review_required','parent_task_id','repair_source_review_id','repair_candidate_commit','auto_repair','repair_root_task_id','repair_target_publication_id','repair_expected_remote_head','scope_plan_decision_id','workflow_required','workflow_id','workflow_review_id','review_target_sha256','workflow_feedback_review_id','workflow_feedback_sha256'], to_jsonb(a)-ARRAY['plan_decision_id','feedback_id','feedback_sha256','legacy_resume_note','repair_source_review_id','repair_candidate_commit','scope_plan_decision_id','workflow_required','workflow_id','workflow_review_id','review_target_sha256','workflow_feedback_review_id','workflow_feedback_sha256'] FROM forge_agent.coding_tasks t "
                    "JOIN forge_agent.coding_attempts a ON a.task_id=t.id WHERE t.id=%s",
                    (UUID(lease.task_id),),
                )
                # Invoke the production migration registry/verifiers against the existing cluster.
                DisposablePostgresCluster.apply_and_verify(cluster)
                DisposablePostgresCluster.apply_and_verify(cluster)
                self.assertEqual(self.rows(cluster, "SELECT version FROM forge_agent.schema_migrations WHERE version = 11")[0][0], 11)
                self.assertEqual(self.rows(
                    cluster, "SELECT to_jsonb(t)-ARRAY['plan_decision_id','feedback_id','feedback_sha256','legacy_resume_note','review_required','parent_task_id','repair_source_review_id','repair_candidate_commit','auto_repair','repair_root_task_id','repair_target_publication_id','repair_expected_remote_head','scope_plan_decision_id','workflow_required','workflow_id','workflow_review_id','review_target_sha256','workflow_feedback_review_id','workflow_feedback_sha256'], to_jsonb(a)-ARRAY['plan_decision_id','feedback_id','feedback_sha256','legacy_resume_note','repair_source_review_id','repair_candidate_commit','scope_plan_decision_id','workflow_required','workflow_id','workflow_review_id','review_target_sha256','workflow_feedback_review_id','workflow_feedback_sha256'] FROM forge_agent.coding_tasks t "
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
            historical_registry(legacy_sql,12)
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
                acceptance = self.legacy_accept(cluster,command)
                snapshot_query = (
                    "SELECT to_jsonb(t)-ARRAY['plan_decision_id','feedback_id','feedback_sha256','legacy_resume_note','review_required','parent_task_id','repair_source_review_id','repair_candidate_commit','auto_repair','repair_root_task_id','repair_target_publication_id','repair_expected_remote_head','scope_plan_decision_id','workflow_required','workflow_id','workflow_review_id','review_target_sha256','workflow_feedback_review_id','workflow_feedback_sha256'], to_jsonb(a)-ARRAY['plan_decision_id','feedback_id','feedback_sha256','legacy_resume_note','repair_source_review_id','repair_candidate_commit','scope_plan_decision_id','workflow_required','workflow_id','workflow_review_id','review_target_sha256','workflow_feedback_review_id','workflow_feedback_sha256'], "
                    "(SELECT jsonb_agg(to_jsonb(r) ORDER BY r.idempotency_key) "
                    "FROM forge_agent.coding_control_receipts r WHERE r.task_id=t.id) "
                    "FROM forge_agent.coding_tasks t JOIN forge_agent.coding_attempts a ON a.task_id=t.id WHERE t.id=%s"
                )
                snapshots = self.rows(cluster, snapshot_query, (UUID(lease.task_id),))
                self.assertIsNotNone(snapshots[0][2], "An immutable explicit acceptance receipt exists before upgrade")
                DisposablePostgresCluster.apply_and_verify(cluster)
                DisposablePostgresCluster.apply_and_verify(cluster)
                self.assertEqual(self.rows(cluster, "SELECT max(version) FROM forge_agent.schema_migrations")[0][0], 19)
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

    def test_schema_13_to_14_preserves_v3_history_and_allows_legacy_plan_decision(self):
        with TemporaryDirectory(prefix="forge-plan-upgrade-",dir="/tmp") as temporary:
            legacy_sql=Path(temporary)/"sql"
            historical_registry(legacy_sql,13)
            cluster=PreUpgradeCluster(legacy_sql).start()
            try:
                self.assertEqual(self.rows(cluster,"SELECT max(version) FROM forge_agent.schema_migrations")[0][0],13)
                db=self.worker(cluster)
                incarnation=self.advertise(cluster,db,"v3-worker",SESSION_POLICY_DIGEST)
                admitted=self.admit_old(cluster,"v3-legacy-plan",SESSION_POLICY_ID)
                lease=db.claim("v3-worker",incarnation,["codex-v2"])
                text,manifest=self.stage(db,lease)
                proof=make_stop_proof(lease.unit_name)
                publication=db.publish(lease,"awaiting_review",manifest,None,"V3 Plan",proof)
                db.reconcile(lease,"c"*64,proof)
                command=WorkAccept("homelab",UUID(lease.task_id),publication["revision"],"old-v3-accept","Historical approval")
                acceptance=self.legacy_accept(cluster,command)
                snapshot="SELECT to_jsonb(t)-ARRAY['plan_decision_id','feedback_id','feedback_sha256','legacy_resume_note','review_required','parent_task_id','repair_source_review_id','repair_candidate_commit','auto_repair','repair_root_task_id','repair_target_publication_id','repair_expected_remote_head','scope_plan_decision_id','workflow_required','workflow_id','workflow_review_id','review_target_sha256','workflow_feedback_review_id','workflow_feedback_sha256'],to_jsonb(a)-ARRAY['plan_decision_id','feedback_id','feedback_sha256','legacy_resume_note','repair_source_review_id','repair_candidate_commit','scope_plan_decision_id','workflow_required','workflow_id','workflow_review_id','review_target_sha256','workflow_feedback_review_id','workflow_feedback_sha256'] FROM forge_agent.coding_tasks t JOIN forge_agent.coding_attempts a ON a.task_id=t.id WHERE t.id=%s"
                before=self.rows(cluster,snapshot,(UUID(lease.task_id),))
                DisposablePostgresCluster.apply_and_verify(cluster)
                DisposablePostgresCluster.apply_and_verify(cluster)
                self.assertEqual(self.rows(cluster,snapshot,(UUID(lease.task_id),)),before)
                self.assertEqual(self.service(cluster).accept(command),acceptance)
                self.assertEqual(self.admit_old(cluster,"v3-legacy-plan",SESSION_POLICY_ID),admitted)
                decision=self.service(cluster).plan_decide(WorkPlanDecide("homelab",UUID(lease.task_id),acceptance["revision"],
                    UUID(lease.attempt_id),manifest["sha256"],"resume","codex-v2","approve-v3-plan"))
                self.assertEqual((decision["status"],decision["mode"]),("queued","implement"))
            finally:cluster.stop()

    def test_schema14_v4_history_is_not_upgraded_or_opted_into_repairs(self):
        with TemporaryDirectory(prefix='forge-v4-feedback-upgrade-',dir='/tmp') as temporary:
            legacy_sql=Path(temporary)/'sql';historical_registry(legacy_sql,14)
            cluster=PreUpgradeCluster(legacy_sql).start()
            try:
                db=self.worker(cluster);inc=self.advertise(cluster,db,'v4-worker',PLAN_POLICY_DIGEST)
                admitted=self.admit_old(cluster,'v4-before-feedback',PLAN_POLICY_ID)
                lease=db.claim('v4-worker',inc,['codex-v2']);text,manifest=self.stage(db,lease);proof=make_stop_proof(lease.unit_name)
                publication=db.publish(lease,'awaiting_review',manifest,None,'V4 Plan',proof)
                db.reconcile(lease,'c'*64,proof)
                command=WorkAccept('homelab',UUID(lease.task_id),publication['revision'],'v4-history-accept','Historical reviewed Plan')
                accepted=self.legacy_accept(cluster,command)
                before=self.rows(cluster,'SELECT pinned_recipe,recipe_digest,review_note FROM forge_agent.coding_tasks WHERE id=%s',(UUID(lease.task_id),))
                DisposablePostgresCluster.apply_and_verify(cluster);DisposablePostgresCluster.apply_and_verify(cluster)
                self.assertEqual(self.rows(cluster,'SELECT pinned_recipe,recipe_digest,review_note FROM forge_agent.coding_tasks WHERE id=%s',(UUID(lease.task_id),)),before)
                self.assertEqual(self.rows(cluster,'SELECT auto_repair,review_required,feedback_id FROM forge_agent.coding_tasks WHERE id=%s',(UUID(lease.task_id),)),[(False,False,None)])
                self.assertEqual(self.service(cluster).accept(command),accepted)
                self.assertFalse(self.service(cluster).get(WorkGet('homelab',UUID(lease.task_id)))['review_required'])
                self.assertEqual(self.admit_old(cluster,'v4-before-feedback',PLAN_POLICY_ID),admitted)
                self.assertEqual(db.publish(lease,'awaiting_review',manifest,None,'V4 Plan',proof),publication)
                self.assertEqual(self.store(cluster).result(project_alias='homelab',task_id=UUID(lease.task_id),attempt_id=UUID(lease.attempt_id))['content'],text)
            finally:cluster.stop()

    def test_schema15_v5_receipts_and_feedback_history_are_not_rewritten_by_policy6(self):
        with TemporaryDirectory(prefix='forge-v5-autonomy-upgrade-',dir='/tmp') as temporary:
            legacy_sql=Path(temporary)/'sql';historical_registry(legacy_sql,15)
            cluster=PreUpgradeCluster(legacy_sql).start()
            try:
                db=self.worker(cluster);inc=self.advertise(cluster,db,'v5-worker',FEEDBACK_POLICY_DIGEST)
                admitted=self.admit_old(cluster,'v5-before-autonomy',FEEDBACK_POLICY_ID)
                lease=db.claim('v5-worker',inc,['codex-v2']);text,manifest=self.stage(db,lease);proof=make_stop_proof(lease.unit_name)
                published=db.publish(lease,'awaiting_review',manifest,None,'V5 Plan',proof);db.reconcile(lease,'c'*64,proof)
                before=self.rows(cluster,'SELECT pinned_recipe,recipe_digest,review_note FROM forge_agent.coding_tasks WHERE id=%s',(UUID(lease.task_id),))
                DisposablePostgresCluster.apply_and_verify(cluster);DisposablePostgresCluster.apply_and_verify(cluster)
                self.assertEqual(self.rows(cluster,'SELECT pinned_recipe,recipe_digest,review_note FROM forge_agent.coding_tasks WHERE id=%s',(UUID(lease.task_id),)),before)
                self.assertEqual(self.rows(cluster,'SELECT workflow_required,workflow_id FROM forge_agent.coding_tasks WHERE id=%s',(UUID(lease.task_id),)),[(False,None)])
                self.assertEqual(self.admit_old(cluster,'v5-before-autonomy',FEEDBACK_POLICY_ID),admitted)
                self.assertEqual(db.publish(lease,'awaiting_review',manifest,None,'V5 Plan',proof),published)
                queued=self.admit_old(cluster,'v5-queued-after-upgrade',FEEDBACK_POLICY_ID)
                current=self.advertise(cluster,db,'v6-worker',work_policy.POLICY_DIGEST)
                self.assertIsNone(db.claim('v6-worker',current,['codex-v2']))
                old=self.advertise(cluster,db,'v5-worker',FEEDBACK_POLICY_DIGEST)
                self.assertEqual(db.claim('v5-worker',old,['codex-v2']).task_id,queued['task_id'])
            finally:cluster.stop()

    def test_v6_published_receipt_and_recipe_survive_external_sandbox_policy7(self):
        db=self.worker(self.cluster);inc=self.advertise(self.cluster,db,'v6-peer-worker',PEER_POLICY_DIGEST)
        admitted=self.admit_old(self.cluster,'v6-published-before-external',PEER_POLICY_ID)
        lease=db.claim('v6-peer-worker',inc,['codex-v2']);text,manifest=self.stage(db,lease);proof=make_stop_proof(lease.unit_name)
        published=db.publish(lease,'awaiting_review',manifest,None,'V6 exact artifact',proof);receipt=db.reconcile(lease,'c'*64,proof)
        reconciliation=self.rows(self.cluster,'SELECT to_jsonb(r) FROM forge_agent.coding_reconciliation_receipts r WHERE receipt_id=%s',(UUID(receipt),))
        snapshot=self.rows(self.cluster,'SELECT pinned_recipe,recipe_digest FROM forge_agent.coding_tasks WHERE id=%s',(UUID(lease.task_id),))
        self.assertEqual(snapshot[0][0]['resource_profile'],'forge-coding-v6');self.assertEqual(snapshot[0][0]['policy_digest'],PEER_POLICY_DIGEST)
        self.assertEqual(self.admit_old(self.cluster,'v6-published-before-external',PEER_POLICY_ID),admitted)
        self.assertEqual(db.publish(lease,'awaiting_review',manifest,None,'V6 exact artifact',proof),published)
        self.assertEqual(self.rows(self.cluster,'SELECT to_jsonb(r) FROM forge_agent.coding_reconciliation_receipts r WHERE receipt_id=%s',(UUID(receipt),)),reconciliation)
        self.assertEqual(self.rows(self.cluster,'SELECT pinned_recipe,recipe_digest FROM forge_agent.coding_tasks WHERE id=%s',(UUID(lease.task_id),)),snapshot)
        queued=self.admit_old(self.cluster,'v6-queued-before-external',PEER_POLICY_ID)
        current=self.advertise(self.cluster,db,'v7-external-worker',work_policy.POLICY_DIGEST)
        self.assertIsNone(db.claim('v7-external-worker',current,['codex-v2']))
        old=self.advertise(self.cluster,db,'v6-peer-worker',PEER_POLICY_DIGEST)
        self.assertEqual(db.claim('v6-peer-worker',old,['codex-v2']).task_id,queued['task_id'])

    def test_schema16_v7_tasks_attempts_and_receipts_survive_migration_17_and_isolate_v8(self):
        with TemporaryDirectory(prefix="forge-v7-codex-warm-upgrade-", dir="/tmp") as temporary:
            legacy_sql = Path(temporary) / "sql"
            historical_registry(legacy_sql, 16)
            cluster = PreUpgradeCluster(legacy_sql).start()
            try:
                self.assertEqual(self.rows(cluster, "SELECT max(version) FROM forge_agent.schema_migrations")[0][0], 16)
                db = self.worker(cluster)
                v7_inc = self.advertise(cluster, db, "v7-worker", EXTERNAL_SANDBOX_POLICY_DIGEST)
                admitted = self.admit_old(cluster, "v7-before-codex-warm", EXTERNAL_SANDBOX_POLICY_ID)
                lease = db.claim("v7-worker", v7_inc, ["codex-v2"])
                self.assertEqual(lease.task_id, admitted["task_id"])
                text, manifest = self.stage(db, lease)
                proof = make_stop_proof(lease.unit_name)
                published = db.publish(lease, "awaiting_review", manifest, None, "V7 result", proof)
                receipt = db.reconcile(lease, "d" * 64, proof)
                reconciliation = self.rows(
                    cluster, "SELECT to_jsonb(r) FROM forge_agent.coding_reconciliation_receipts r WHERE receipt_id=%s",
                    (UUID(receipt),),
                )
                before = self.rows(
                    cluster, "SELECT to_jsonb(t), to_jsonb(a) FROM forge_agent.coding_tasks t "
                    "JOIN forge_agent.coding_attempts a ON a.task_id=t.id WHERE t.id=%s",
                    (UUID(lease.task_id),),
                )
                DisposablePostgresCluster.apply_and_verify(cluster)
                DisposablePostgresCluster.apply_and_verify(cluster)
                self.assertEqual(self.rows(cluster, "SELECT max(version) FROM forge_agent.schema_migrations")[0][0], 19)
                self.assertEqual(
                    self.rows(cluster, "SELECT to_jsonb(t), to_jsonb(a) FROM forge_agent.coding_tasks t "
                              "JOIN forge_agent.coding_attempts a ON a.task_id=t.id WHERE t.id=%s",
                              (UUID(lease.task_id),)),
                    before,
                )
                self.assertEqual(
                    self.rows(cluster, "SELECT to_jsonb(r) FROM forge_agent.coding_reconciliation_receipts r WHERE receipt_id=%s",
                              (UUID(receipt),)),
                    reconciliation,
                )
                self.assertEqual(self.admit_old(cluster, "v7-before-codex-warm", EXTERNAL_SANDBOX_POLICY_ID), admitted)
                self.assertEqual(db.publish(lease, "awaiting_review", manifest, None, "V7 result", proof), published)
                self.assertEqual(
                    self.store(cluster).result(project_alias="homelab", task_id=UUID(lease.task_id),
                                               attempt_id=UUID(lease.attempt_id))["content"],
                    text,
                )
                v7_queued = self.admit_old(cluster, "v7-queued-after-m17", EXTERNAL_SANDBOX_POLICY_ID)
                v8_queued = self.service(cluster).submit(make_submit(idempotency_key="v8-queued-after-m17"))
                v8_inc = self.advertise(cluster, db, "v8-worker", work_policy.POLICY_DIGEST)
                v8_lease = db.claim("v8-worker", v8_inc, ["codex-v2"])
                self.assertEqual(v8_lease.task_id, v8_queued["task_id"])
                self.assertEqual(v8_lease.recipe["resource_profile"], work_policy.POLICY_ID)
                self.assertIsNone(db.claim("v8-worker", v8_inc, ["codex-v2"]))
                v7_inc2 = self.advertise(cluster, db, "v7-worker", EXTERNAL_SANDBOX_POLICY_DIGEST)
                v7_lease = db.claim("v7-worker", v7_inc2, ["codex-v2"])
                self.assertEqual(v7_lease.task_id, v7_queued["task_id"])
                self.assertEqual(v7_lease.recipe["resource_profile"], "forge-coding-v7")
            finally:
                cluster.stop()

    def test_schema17_v8_receipts_survive_m18_and_exact_policy_claims(self):
        with TemporaryDirectory(prefix="forge-v8-grok-upgrade-", dir="/tmp") as temporary:
            legacy_sql = Path(temporary) / "sql"
            historical_registry(legacy_sql, 17)
            cluster = PreUpgradeCluster(legacy_sql).start()
            try:
                self.assertEqual(self.rows(cluster, "SELECT max(version) FROM forge_agent.schema_migrations")[0][0], 17)
                db = self.worker(cluster)
                v7_inc = self.advertise(cluster, db, "v7-worker", WARM_POLICY_DIGEST)
                admitted = self.admit_old(cluster, "v7-before-codex-warm", WARM_POLICY_ID)
                lease = db.claim("v7-worker", v7_inc, ["codex-v2"])
                self.assertEqual(lease.task_id, admitted["task_id"])
                text, manifest = self.stage(db, lease)
                proof = make_stop_proof(lease.unit_name)
                published = db.publish(lease, "awaiting_review", manifest, None, "V7 result", proof)
                receipt = db.reconcile(lease, "d" * 64, proof)
                reconciliation = self.rows(
                    cluster, "SELECT to_jsonb(r) FROM forge_agent.coding_reconciliation_receipts r WHERE receipt_id=%s",
                    (UUID(receipt),),
                )
                before = self.rows(
                    cluster, "SELECT to_jsonb(t), to_jsonb(a) FROM forge_agent.coding_tasks t "
                    "JOIN forge_agent.coding_attempts a ON a.task_id=t.id WHERE t.id=%s",
                    (UUID(lease.task_id),),
                )
                DisposablePostgresCluster.apply_and_verify(cluster)
                DisposablePostgresCluster.apply_and_verify(cluster)
                self.assertEqual(self.rows(cluster, "SELECT max(version) FROM forge_agent.schema_migrations")[0][0], 19)
                self.assertEqual(
                    self.rows(cluster, "SELECT to_jsonb(t), to_jsonb(a) FROM forge_agent.coding_tasks t "
                              "JOIN forge_agent.coding_attempts a ON a.task_id=t.id WHERE t.id=%s",
                              (UUID(lease.task_id),)),
                    before,
                )
                self.assertEqual(
                    self.rows(cluster, "SELECT to_jsonb(r) FROM forge_agent.coding_reconciliation_receipts r WHERE receipt_id=%s",
                              (UUID(receipt),)),
                    reconciliation,
                )
                self.assertEqual(self.admit_old(cluster, "v7-before-codex-warm", WARM_POLICY_ID), admitted)
                self.assertEqual(db.publish(lease, "awaiting_review", manifest, None, "V7 result", proof), published)
                self.assertEqual(
                    self.store(cluster).result(project_alias="homelab", task_id=UUID(lease.task_id),
                                               attempt_id=UUID(lease.attempt_id))["content"],
                    text,
                )
                v7_queued = self.admit_old(cluster, "v7-queued-after-m17", WARM_POLICY_ID)
                v8_queued = self.service(cluster).submit(make_submit(idempotency_key="v8-queued-after-m17"))
                v8_inc = self.advertise(cluster, db, "v8-worker", work_policy.POLICY_DIGEST)
                v8_lease = db.claim("v8-worker", v8_inc, ["codex-v2"])
                self.assertEqual(v8_lease.task_id, v8_queued["task_id"])
                self.assertEqual(v8_lease.recipe["resource_profile"], work_policy.POLICY_ID)
                self.assertIsNone(db.claim("v8-worker", v8_inc, ["codex-v2"]))
                v7_inc2 = self.advertise(cluster, db, "v7-worker", WARM_POLICY_DIGEST)
                v7_lease = db.claim("v7-worker", v7_inc2, ["codex-v2"])
                self.assertEqual(v7_lease.task_id, v7_queued["task_id"])
                self.assertEqual(v7_lease.recipe["resource_profile"], "forge-coding-v8")
            finally:
                cluster.stop()

    def test_schema18_v9_receipts_survive_m19_and_exact_policy_claims(self):
        with TemporaryDirectory(prefix="forge-v9-muse-upgrade-", dir="/tmp") as temporary:
            legacy_sql = Path(temporary) / "sql"
            historical_registry(legacy_sql, 18)
            cluster = PreUpgradeCluster(legacy_sql).start()
            try:
                self.assertEqual(self.rows(cluster, "SELECT max(version) FROM forge_agent.schema_migrations")[0][0], 18)
                db = self.worker(cluster)
                v9_inc = self.advertise(cluster, db, "v9-worker", GROK_POLICY_DIGEST)
                admitted = self.admit_old(cluster, "v9-before-codex-warm", GROK_POLICY_ID)
                lease = db.claim("v9-worker", v9_inc, ["codex-v2"])
                self.assertEqual(lease.task_id, admitted["task_id"])
                text, manifest = self.stage(db, lease)
                proof = make_stop_proof(lease.unit_name)
                published = db.publish(lease, "awaiting_review", manifest, None, "V7 result", proof)
                receipt = db.reconcile(lease, "d" * 64, proof)
                reconciliation = self.rows(
                    cluster, "SELECT to_jsonb(r) FROM forge_agent.coding_reconciliation_receipts r WHERE receipt_id=%s",
                    (UUID(receipt),),
                )
                before = self.rows(
                    cluster, "SELECT to_jsonb(t), to_jsonb(a) FROM forge_agent.coding_tasks t "
                    "JOIN forge_agent.coding_attempts a ON a.task_id=t.id WHERE t.id=%s",
                    (UUID(lease.task_id),),
                )
                DisposablePostgresCluster.apply_and_verify(cluster)
                DisposablePostgresCluster.apply_and_verify(cluster)
                self.assertEqual(self.rows(cluster, "SELECT max(version) FROM forge_agent.schema_migrations")[0][0], 19)
                self.assertEqual(
                    self.rows(cluster, "SELECT to_jsonb(t), to_jsonb(a) FROM forge_agent.coding_tasks t "
                              "JOIN forge_agent.coding_attempts a ON a.task_id=t.id WHERE t.id=%s",
                              (UUID(lease.task_id),)),
                    before,
                )
                self.assertEqual(
                    self.rows(cluster, "SELECT to_jsonb(r) FROM forge_agent.coding_reconciliation_receipts r WHERE receipt_id=%s",
                              (UUID(receipt),)),
                    reconciliation,
                )
                self.assertEqual(self.admit_old(cluster, "v9-before-codex-warm", GROK_POLICY_ID), admitted)
                self.assertEqual(db.publish(lease, "awaiting_review", manifest, None, "V7 result", proof), published)
                self.assertEqual(
                    self.store(cluster).result(project_alias="homelab", task_id=UUID(lease.task_id),
                                               attempt_id=UUID(lease.attempt_id))["content"],
                    text,
                )
                v9_queued = self.admit_old(cluster, "v9-queued-after-m19", GROK_POLICY_ID)
                v10_queued = self.service(cluster).submit(make_submit(idempotency_key="v10-queued-after-m19"))
                v10_inc = self.advertise(cluster, db, "v10-worker", work_policy.POLICY_DIGEST)
                v10_lease = db.claim("v10-worker", v10_inc, ["codex-v2"])
                self.assertEqual(v10_lease.task_id, v10_queued["task_id"])
                self.assertEqual(v10_lease.recipe["resource_profile"], work_policy.POLICY_ID)
                self.assertIsNone(db.claim("v10-worker", v10_inc, ["codex-v2"]))
                v9_inc2 = self.advertise(cluster, db, "v9-worker", GROK_POLICY_DIGEST)
                v9_lease = db.claim("v9-worker", v9_inc2, ["codex-v2"])
                self.assertEqual(v9_lease.task_id, v9_queued["task_id"])
                self.assertEqual(v9_lease.recipe["resource_profile"], "forge-coding-v9")
            finally:
                cluster.stop()

    def test_v1_v2_and_current_workers_claim_only_their_exact_policy_digest(self):
        old = self.admit_old(self.cluster, "old-queued-first")
        previous = self.admit_old(self.cluster, "previous-queued-second", PREVIOUS_POLICY_ID)
        session = self.admit_old(self.cluster, "session-queued-third", SESSION_POLICY_ID)
        current = self.service(self.cluster).submit(make_submit(idempotency_key="current-queued-fourth"))
        db = self.worker(self.cluster)
        old_inc = self.advertise(self.cluster, db, "old-worker", OLD_POLICY_DIGEST)
        previous_inc = self.advertise(self.cluster, db, "previous-worker", PREVIOUS_POLICY_DIGEST)
        session_inc = self.advertise(self.cluster, db, "session-worker", SESSION_POLICY_DIGEST)
        current_inc = self.advertise(self.cluster, db, "current-worker", work_policy.POLICY_DIGEST)
        for worker_id, incarnation, admitted, policy, digest in (
            ("current-worker", current_inc, current, work_policy.POLICY_ID, work_policy.POLICY_DIGEST),
            ("session-worker", session_inc, session, SESSION_POLICY_ID, SESSION_POLICY_DIGEST),
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

    def test_grok_normal_admission_worker_tokens_and_exact_backend_policy_fences(self):
        from dataclasses import replace
        from work_config import WorkProfile
        from usage_types import normalize_attempt_usage
        config = make_v2_config()
        config = replace(config, profiles={**config.profiles,
            "grok": WorkProfile("grok", "grok", "fixture-grok-model", work_policy.POLICY_ID, execution_enabled=True)})
        service = WorkServiceFactory(config, lambda: self.cluster.connect("forge_agent")).create()
        db = self.worker(self.cluster)
        self.cluster.grant_worker("mission-control", "homelab", "grok", ("plan",))
        inc = str(uuid4())
        task = service.submit(make_submit(agent_profile="grok", idempotency_key="real-grok-m18"))
        # A latent metadata grant cannot claim Grok work while no matching
        # executable profile is advertised. This models usage-only registration.
        for profiles in (
            [{"alias":"codex-v2","backend":"codex","model":"gpt-5.4","modes":["plan"]}],
            [{"alias":"grok","backend":"grok","model":"wrong-model","modes":["plan"]}],
            [{"alias":"grok","backend":"grok","model":"fixture-grok-model","modes":["review"]}],
        ):
            db.advertise("grok-worker",inc,[{"alias":"homelab","base_revision":BASE_REV}],profiles,work_policy.POLICY_DIGEST)
            self.assertIsNone(db.claim("grok-worker",inc,["grok"]))
        db.advertise("grok-worker", inc, [{"alias":"homelab","base_revision":BASE_REV}],
            [{"alias":"grok","backend":"grok","model":"fixture-grok-model","modes":["plan"]}], work_policy.POLICY_DIGEST)
        lease = db.claim("grok-worker",inc,["grok"])
        self.assertEqual(lease.task_id,task["task_id"])
        self.assertEqual(lease.recipe["backend"],"grok")
        self.assertEqual(lease.recipe["resource_profile"],"forge-coding-v10")
        text, manifest = self.stage(db,lease)
        usage = normalize_attempt_usage("grok", {"input_tokens":20,"output_tokens":25,"cache_read_input_tokens":80,"cache_creation_input_tokens":0,"total_tokens":125})
        db.publish(lease,"awaiting_review",manifest,usage,"Grok fixture result",make_stop_proof(lease.unit_name))
        row = self.rows(self.cluster,"SELECT usage_json FROM forge_agent.coding_attempts WHERE attempt_id=%s",(UUID(lease.attempt_id),))[0][0]
        self.assertEqual(row,usage)
        self.assertEqual(row["total_tokens"],"125")
        self.assertEqual(row["cached_input_tokens"],"80")
        self.assertEqual(self.rows(self.cluster,"SELECT forge_coding_worker_api._validate_usage_json(%s)",(Json({**usage,"source":"unverified-grok-source"}),))[0][0],False)
        # Existing worker roles cannot mutate migration catalogs or grant their
        # own authority just because another provider has been selected.
        self.assertEqual(self.rows(self.cluster,"SELECT has_table_privilege('forge_coding_worker','forge_agent.schema_migrations','UPDATE')")[0][0],False)

    def test_disabled_grok_is_usage_visible_but_never_admitted_from_global_readiness(self):
        from dataclasses import replace
        from work_config import WorkProfile
        from queue_store import Rejected
        from usage_store import UsageStore
        config = make_v2_config()
        config = replace(config, profiles={**config.profiles,
            "grok-off":WorkProfile("grok-off","grok","fixture-grok",work_policy.POLICY_ID)})
        service=WorkServiceFactory(config,lambda:self.cluster.connect("forge_agent")).create()
        db=self.worker(self.cluster);inc=str(uuid4())
        self.cluster.grant_worker("mission-control","homelab","grok-off",("review",))
        db.advertise("ready-other-worker",inc,[{"alias":"homelab","base_revision":BASE_REV}],
            [{"alias":"grok-off","backend":"grok","model":"fixture-grok","modes":["plan"]}],work_policy.POLICY_DIGEST)
        self.assertNotIn("grok-off",[entry["alias"]for entry in service.catalog()["profiles"]])
        with self.assertRaises(Rejected):service.submit(make_submit(agent_profile="grok-off",idempotency_key="must-not-admit"))
        self.assertEqual(self.rows(self.cluster,"SELECT count(*) FROM forge_agent.coding_tasks WHERE agent_profile='grok-off'")[0][0],0)
        usage=UsageStore(config,lambda:self.cluster.connect("forge_agent")).overview("homelab")
        self.assertIn("grok-off",[entry["alias"]for entry in usage["providers"]])

    def test_enabled_then_disabled_submit_and_repair_replay_preserve_exact_receipts(self):
        from dataclasses import replace
        from work_config import WorkProfile
        from work_api import WorkRepair
        from queue_store import Rejected
        cfg=make_v2_config()
        profile=WorkProfile("grok-replay","grok","fixture-grok",work_policy.POLICY_ID,execution_enabled=True)
        cfg=replace(cfg,profiles={**cfg.profiles,profile.alias:profile})
        enabled=WorkServiceFactory(cfg,lambda:self.cluster.connect("forge_agent")).create()
        off=WorkServiceFactory(replace(cfg,profiles={**cfg.profiles,profile.alias:replace(profile,execution_enabled=False)}),lambda:self.cluster.connect("forge_agent")).create()
        command=make_submit(agent_profile=profile.alias,mode="implement",idempotency_key="grok-replay-submit")
        admitted=enabled.submit(command)
        self.assertEqual(off.submit(command),admitted)
        with self.assertRaises(Rejected)as error:off.submit(replace(command,idempotency_key="fresh-disabled-submit"))
        self.assertEqual(error.exception.status,403)
        db=self.worker(self.cluster);inc=str(uuid4())
        self.cluster.grant_worker("mission-control","homelab",profile.alias,("implement",))
        db.advertise("grok-replay-worker",inc,[{"alias":"homelab","base_revision":BASE_REV}],
            [{"alias":profile.alias,"backend":"grok","model":profile.model,"modes":["implement"]}],work_policy.POLICY_DIGEST)
        lease=db.claim("grok-replay-worker",inc,[profile.alias]);_,manifest=self.stage(db,lease)
        proof=make_stop_proof(lease.unit_name);db.publish(lease,"awaiting_review",manifest,None,"Fixture implementation",proof);db.reconcile(lease,"c"*64,proof)
        # Owned historical fixture: a completed implementation before source review.
        self.cluster.admin_exec("UPDATE forge_agent.coding_tasks SET status='completed',accepted_at=clock_timestamp(),revision=revision+1")
        revision=self.rows(self.cluster,"SELECT revision FROM forge_agent.coding_tasks WHERE id=%s",(UUID(lease.task_id),))[0][0]
        repair=WorkRepair("homelab",UUID(lease.task_id),revision,"d"*40,profile.alias,"Inspect this correction.",(),"grok-replay-repair")
        repaired=enabled.repair(repair)
        state=self.rows(self.cluster,"SELECT (SELECT count(*) FROM forge_agent.coding_tasks),(SELECT count(*) FROM forge_agent.coding_reviews),(SELECT count(*) FROM forge_agent.coding_control_receipts),(SELECT count(*) FROM forge_agent.coding_events)")
        self.assertEqual(off.repair(repair),repaired)
        with self.assertRaises(Rejected)as error:off.repair(replace(repair,idempotency_key="fresh-disabled-repair",expected_revision=repaired["revision"]))
        self.assertEqual(error.exception.status,403)
        self.assertEqual(self.rows(self.cluster,"SELECT (SELECT count(*) FROM forge_agent.coding_tasks),(SELECT count(*) FROM forge_agent.coding_reviews),(SELECT count(*) FROM forge_agent.coding_control_receipts),(SELECT count(*) FROM forge_agent.coding_events)"),state)

    def test_enabled_then_disabled_resume_replays_before_current_recipe_resolution(self):
        from dataclasses import replace
        from work_config import WorkProfile
        profile=WorkProfile("grok-resume","grok","fixture-grok",work_policy.POLICY_ID,execution_enabled=True)
        cfg=make_v2_config();cfg=replace(cfg,profiles={**cfg.profiles,profile.alias:profile})
        enabled=WorkServiceFactory(cfg,lambda:self.cluster.connect("forge_agent")).create()
        off=WorkServiceFactory(replace(cfg,profiles={**cfg.profiles,profile.alias:replace(profile,execution_enabled=False)}),lambda:self.cluster.connect("forge_agent")).create()
        command=make_submit(agent_profile=profile.alias,mode="review",idempotency_key="grok-resume-submit")
        admitted=enabled.submit(command);db=self.worker(self.cluster);inc=str(uuid4())
        self.cluster.grant_worker("mission-control","homelab",profile.alias,("review",))
        db.advertise("grok-resume-worker",inc,[{"alias":"homelab","base_revision":BASE_REV}],
            [{"alias":profile.alias,"backend":"grok","model":profile.model,"modes":["review"]}],work_policy.POLICY_DIGEST)
        lease=db.claim("grok-resume-worker",inc,[profile.alias]);proof=make_stop_proof(lease.unit_name)
        published=db.publish(lease,"quota_blocked",None,None,"Fixture quota boundary",proof);receipt=db.reconcile(lease,"c"*64,proof)
        resume=WorkResume("homelab",UUID(lease.task_id),published["revision"],"grok-resume-replay",profile.alias,UUID(receipt),"Operator qualified continuation.")
        admitted=enabled.resume(resume)
        before=self.rows(self.cluster,"SELECT count(*),max(revision) FROM forge_agent.coding_tasks")
        self.assertEqual(off.resume(resume),admitted)
        self.assertEqual(self.rows(self.cluster,"SELECT count(*),max(revision) FROM forge_agent.coding_tasks"),before)

    def test_muse_normal_admission_worker_tokens_and_exact_backend_policy_fences(self):
        from dataclasses import replace
        from work_config import WorkProfile
        from usage_types import normalize_attempt_usage
        config = make_v2_config()
        config = replace(config, profiles={**config.profiles,
            "muse": WorkProfile("muse", "muse", "fixture-meta-model", work_policy.POLICY_ID, execution_enabled=True)})
        service = WorkServiceFactory(config, lambda: self.cluster.connect("forge_agent")).create()
        db = self.worker(self.cluster)
        self.cluster.grant_worker("mission-control", "homelab", "muse", ("plan",))
        inc = str(uuid4())
        task = service.submit(make_submit(agent_profile="muse", idempotency_key="real-muse-m19"))
        # A latent metadata grant cannot claim Muse work while no matching
        # executable profile is advertised. This models usage-only registration.
        for profiles in (
            [{"alias":"codex-v2","backend":"codex","model":"gpt-5.4","modes":["plan"]}],
            [{"alias":"muse","backend":"muse","model":"wrong-model","modes":["plan"]}],
            [{"alias":"muse","backend":"muse","model":"fixture-meta-model","modes":["review"]}],
        ):
            db.advertise("muse-worker",inc,[{"alias":"homelab","base_revision":BASE_REV}],profiles,work_policy.POLICY_DIGEST)
            self.assertIsNone(db.claim("muse-worker",inc,["muse"]))
        db.advertise("muse-worker", inc, [{"alias":"homelab","base_revision":BASE_REV}],
            [{"alias":"muse","backend":"muse","model":"fixture-meta-model","modes":["plan"]}], work_policy.POLICY_DIGEST)
        lease = db.claim("muse-worker",inc,["muse"])
        self.assertEqual(lease.task_id,task["task_id"])
        self.assertEqual(lease.recipe["backend"],"muse")
        self.assertEqual(lease.recipe["resource_profile"],"forge-coding-v10")
        text, manifest = self.stage(db,lease)
        usage = normalize_attempt_usage("muse", {"input_tokens":100,"output_tokens":25,"cached_tokens":80})
        db.publish(lease,"awaiting_review",manifest,usage,"Muse fixture result",make_stop_proof(lease.unit_name))
        row = self.rows(self.cluster,"SELECT usage_json FROM forge_agent.coding_attempts WHERE attempt_id=%s",(UUID(lease.attempt_id),))[0][0]
        self.assertEqual(row,usage)
        self.assertEqual(row["total_tokens"],"125")
        self.assertEqual(row["cached_input_tokens"],"80")
        self.assertEqual(self.rows(self.cluster,"SELECT forge_coding_worker_api._validate_usage_json(%s)",(Json({**usage,"source":"unverified-muse-source"}),))[0][0],False)
        # Existing worker roles cannot mutate migration catalogs or grant their
        # own authority just because another provider has been selected.
        self.assertEqual(self.rows(self.cluster,"SELECT has_table_privilege('forge_coding_worker','forge_agent.schema_migrations','UPDATE')")[0][0],False)

    def test_muse_enabled_then_disabled_submit_and_repair_replay_preserve_exact_receipts(self):
        from dataclasses import replace
        from work_config import WorkProfile
        from work_api import WorkRepair
        from queue_store import Rejected
        cfg=make_v2_config()
        profile=WorkProfile("muse-replay","muse","fixture-muse",work_policy.POLICY_ID,execution_enabled=True)
        cfg=replace(cfg,profiles={**cfg.profiles,profile.alias:profile})
        enabled=WorkServiceFactory(cfg,lambda:self.cluster.connect("forge_agent")).create()
        off=WorkServiceFactory(replace(cfg,profiles={**cfg.profiles,profile.alias:replace(profile,execution_enabled=False)}),lambda:self.cluster.connect("forge_agent")).create()
        command=make_submit(agent_profile=profile.alias,mode="implement",idempotency_key="muse-replay-submit")
        admitted=enabled.submit(command)
        self.assertEqual(off.submit(command),admitted)
        with self.assertRaises(Rejected)as error:off.submit(replace(command,idempotency_key="fresh-disabled-submit"))
        self.assertEqual(error.exception.status,403)
        db=self.worker(self.cluster);inc=str(uuid4())
        self.cluster.grant_worker("mission-control","homelab",profile.alias,("implement",))
        db.advertise("muse-replay-worker",inc,[{"alias":"homelab","base_revision":BASE_REV}],
            [{"alias":profile.alias,"backend":"muse","model":profile.model,"modes":["implement"]}],work_policy.POLICY_DIGEST)
        lease=db.claim("muse-replay-worker",inc,[profile.alias]);_,manifest=self.stage(db,lease)
        proof=make_stop_proof(lease.unit_name);db.publish(lease,"awaiting_review",manifest,None,"Fixture implementation",proof);db.reconcile(lease,"c"*64,proof)
        # Owned historical fixture: a completed implementation before source review.
        self.cluster.admin_exec("UPDATE forge_agent.coding_tasks SET status='completed',accepted_at=clock_timestamp(),revision=revision+1")
        revision=self.rows(self.cluster,"SELECT revision FROM forge_agent.coding_tasks WHERE id=%s",(UUID(lease.task_id),))[0][0]
        repair=WorkRepair("homelab",UUID(lease.task_id),revision,"d"*40,profile.alias,"Inspect this correction.",(),"muse-replay-repair")
        repaired=enabled.repair(repair)
        state=self.rows(self.cluster,"SELECT (SELECT count(*) FROM forge_agent.coding_tasks),(SELECT count(*) FROM forge_agent.coding_reviews),(SELECT count(*) FROM forge_agent.coding_control_receipts),(SELECT count(*) FROM forge_agent.coding_events)")
        self.assertEqual(off.repair(repair),repaired)
        with self.assertRaises(Rejected)as error:off.repair(replace(repair,idempotency_key="fresh-disabled-repair",expected_revision=repaired["revision"]))
        self.assertEqual(error.exception.status,403)
        self.assertEqual(self.rows(self.cluster,"SELECT (SELECT count(*) FROM forge_agent.coding_tasks),(SELECT count(*) FROM forge_agent.coding_reviews),(SELECT count(*) FROM forge_agent.coding_control_receipts),(SELECT count(*) FROM forge_agent.coding_events)"),state)

    def test_muse_enabled_then_disabled_resume_replays_before_current_recipe_resolution(self):
        from dataclasses import replace
        from work_config import WorkProfile
        profile=WorkProfile("muse-resume","muse","fixture-muse",work_policy.POLICY_ID,execution_enabled=True)
        cfg=make_v2_config();cfg=replace(cfg,profiles={**cfg.profiles,profile.alias:profile})
        enabled=WorkServiceFactory(cfg,lambda:self.cluster.connect("forge_agent")).create()
        off=WorkServiceFactory(replace(cfg,profiles={**cfg.profiles,profile.alias:replace(profile,execution_enabled=False)}),lambda:self.cluster.connect("forge_agent")).create()
        command=make_submit(agent_profile=profile.alias,mode="review",idempotency_key="muse-resume-submit")
        admitted=enabled.submit(command);db=self.worker(self.cluster);inc=str(uuid4())
        self.cluster.grant_worker("mission-control","homelab",profile.alias,("review",))
        db.advertise("muse-resume-worker",inc,[{"alias":"homelab","base_revision":BASE_REV}],
            [{"alias":profile.alias,"backend":"muse","model":profile.model,"modes":["review"]}],work_policy.POLICY_DIGEST)
        lease=db.claim("muse-resume-worker",inc,[profile.alias]);proof=make_stop_proof(lease.unit_name)
        published=db.publish(lease,"quota_blocked",None,None,"Fixture quota boundary",proof);receipt=db.reconcile(lease,"c"*64,proof)
        resume=WorkResume("homelab",UUID(lease.task_id),published["revision"],"muse-resume-replay",profile.alias,UUID(receipt),"Operator qualified continuation.")
        admitted=enabled.resume(resume)
        before=self.rows(self.cluster,"SELECT count(*),max(revision) FROM forge_agent.coding_tasks")
        self.assertEqual(off.resume(resume),admitted)
        self.assertEqual(self.rows(self.cluster,"SELECT count(*),max(revision) FROM forge_agent.coding_tasks"),before)

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
        self.assertEqual(self.rows(self.cluster,
            "SELECT count(*) FROM forge_agent.schema_migrations "
            "WHERE version=17 AND name='coding_codex_warm_sessions'")[0][0], 1)
        try:
            self.cluster.admin_exec(
                "UPDATE forge_agent.schema_migrations SET name='unexpected-codex-warm' WHERE version=17")
            result = self.cluster.run_sql_script(script)
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("migration 0017 is missing", result.stderr)
        finally:
            self.cluster.admin_exec(
                "UPDATE forge_agent.schema_migrations SET name='coding_codex_warm_sessions' WHERE version=17")
        original = self.rows(self.cluster,
            "SELECT pg_get_constraintdef(oid,false) FROM pg_constraint "
            "WHERE conrelid='forge_agent.coding_attempts'::regclass AND conname='coding_attempts_pinned_recipe_check'")[0][0]
        for policy in (OLD_POLICY_ID, PREVIOUS_POLICY_ID, SESSION_POLICY_ID, EXTERNAL_SANDBOX_POLICY_ID, work_policy.POLICY_ID):
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
        no_new_policy = function.replace("'forge-coding-v8'", "'removed-policy'")
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
