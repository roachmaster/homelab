"""Transactional settlement of terminal workflow projections, open retry events, and scratch-DB repair."""
from contextlib import closing
from dataclasses import replace
import hashlib
import re
import threading
import time
from types import SimpleNamespace
import unittest
from uuid import UUID, uuid4

import psycopg2
from psycopg2.extras import Json, RealDictCursor

import test_external_flow as ext_fixtures
import test_work_workflow as native_fixtures
from queue_store import Rejected
from test_work_execution_and_usage import make_stop_proof, make_submit
from work_api import WorkCancel, WorkList, WorkServiceFactory
from work_events import COORDINATOR_ACTOR, insert_event
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE, SQL_DIR
from work_retry_events import RetryEventProviderFactory, RetryScope
from work_worker_store import WorkerDB

REPAIR_SCRIPT = SQL_DIR / "management" / "repair-stale-terminal-projections.sql"
DIGEST = "a" * 64


def _digest(label):
    return hashlib.sha256(label.encode("utf-8")).hexdigest()


@unittest.skipUnless(PG16_AVAILABLE, "Requires non-root disposable PostgreSQL 16")
class TerminalProjectionSettlementTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)
        cls.cluster.admin_exec("CREATE ROLE muse_events LOGIN NOINHERIT", owner=False)
        for script in ("grant-muse-read-access.sql", "grant-external-agent.sql", "grant-external-workflows.sql"):
            result = cls.cluster.run_sql_script(SQL_DIR / "management" / script)
            if result.returncode:
                raise AssertionError(result.stdout + result.stderr)

    def setUp(self):
        self.cluster.reset_coding_tables()
        self.cluster.admin_exec("TRUNCATE forge_external_agent.scope_grants CASCADE", owner=False)
        self.cluster.admin_exec("TRUNCATE forge_external_workflow.delivery_grants CASCADE", owner=False)
        self.cluster.admin_exec(
            "INSERT INTO forge_external_agent.scope_grants(external_role,service_role,owner_id,project_alias,profile_alias,enabled) "
            "VALUES('muse_events','forge_agent','test-owner','homelab','muse-db-primary',true)"
        )
        self.cluster.admin_exec(
            "INSERT INTO forge_external_workflow.delivery_grants(worker_role,owner_id,project_alias,enabled) "
            "VALUES('forge_coding_reconciler','test-owner','homelab',true)"
        )
        # Native fixture (owner = 'mission-control')
        self.native = native_fixtures.AutonomousWorkflowTest()
        self.native.cluster = self.cluster
        self.native.setUp()
        self.native_provider = RetryEventProviderFactory(self.native.config.owner).create()
        self.native.service._store.retry_events = self.native_provider
        self.native.coordinator.store.retry_events = self.native_provider

        # External fixture (owner = 'test-owner')
        self.ext_config = ext_fixtures.cfg()
        self.ext_service = WorkServiceFactory(
            self.ext_config, lambda: self.cluster.connect("forge_agent")
        ).create()
        self.ext_flow = self.ext_service._external_workflow
        self.ext_provider = RetryEventProviderFactory(self.ext_config.owner).create()
        self.cluster.grant_worker("test-owner", "homelab", "codex-v2")

    def rows(self, sql, args=()):
        with closing(self.cluster.connect("postgres")) as c, c.cursor(cursor_factory=RealDictCursor) as q:
            q.execute(sql, args)
            return q.fetchall()

    def exec_sql(self, sql, args=()):
        with closing(self.cluster.connect("postgres")) as c, c, c.cursor() as q:
            q.execute(sql, args)

    def open_retry(self, *, owner, scope_kind, scope_id, kind, used=0, phase_id="", project="homelab"):
        provider = RetryEventProviderFactory(owner).create()
        scope = RetryScope(owner, project, scope_kind, UUID(str(scope_id)))
        with closing(self.cluster.connect("forge_agent")) as c, c, c.cursor(cursor_factory=RealDictCursor) as q:
            q.execute("SELECT set_config('forge.coding_owner', %s, true)", (owner,))
            event = provider.observe_failure(
                q, scope, kind, f"src-{kind}-{uuid4()}", _digest(f"{scope_id}:{kind}:{uuid4()}"),
                phase_id=phase_id or kind,
            )
            for idx in range(used):
                event = provider.reserve(q, event["event_id"], f"act-{idx}-{uuid4()}")
            return event

    def exhaust_retry(self, *, owner, scope_kind, scope_id, kind, phase_id="", project="homelab"):
        provider = RetryEventProviderFactory(owner).create()
        scope = RetryScope(owner, project, scope_kind, UUID(str(scope_id)))
        with closing(self.cluster.connect("forge_agent")) as c, c, c.cursor(cursor_factory=RealDictCursor) as q:
            q.execute("SELECT set_config('forge.coding_owner', %s, true)", (owner,))
            event = provider.observe_failure(
                q, scope, kind, f"src-{kind}-{uuid4()}", _digest(f"{scope_id}:{kind}:{uuid4()}"),
                phase_id=phase_id or kind,
            )
            for idx in range(21):
                event = provider.reserve(q, event["event_id"], f"exhaust-{idx}-{uuid4()}")
            self.assertEqual(event["status"], "exhausted")
            self.assertEqual(event["used"], 20)
            return event

    def list_by_scope(self, service, scope):
        result = service.list_tasks(WorkList(project_alias="homelab", status_filter=scope, limit=50, cursor=None))
        return {item["task_id"]: item for item in result["tasks"]}

    def submit_native_workflow(self):
        _, admitted = self.native.submit()
        row = self.rows(
            "SELECT workflow_id, root_task_id FROM forge_agent.coding_workflows WHERE root_task_id=%s",
            (UUID(admitted["task_id"]),),
        )[0]
        return row["workflow_id"], row["root_task_id"]

    def submit_external_workflow(self):
        policy_digest = self.ext_config.external_workflows["homelab"].summary(
            self.ext_config.catalog()
        )["policy_digest"]
        cmd = replace(
            make_submit(agent_profile=ext_fixtures.ALIAS, mode="plan", idempotency_key=str(uuid4())),
            workflow_policy_digest=policy_digest,
        )
        admitted = self.ext_service.submit(cmd)
        row = self.rows(
            "SELECT workflow_id, root_task_id FROM forge_external_workflow.workflows WHERE root_task_id=%s",
            (UUID(admitted["task_id"]),),
        )[0]
        return row["workflow_id"], row["root_task_id"]

    def add_external_child_task(self, workflow_id, *, role="author", status="queued"):
        child_id = uuid4()
        wf = self.rows(
            "SELECT * FROM forge_external_workflow.workflows WHERE workflow_id=%s",
            (workflow_id,),
        )[0]
        root = self.rows(
            "SELECT * FROM forge_agent.coding_tasks WHERE id=%s", (wf["root_task_id"],)
        )[0]
        impl_recipe = self.ext_config.catalog().resolve(wf["author_profile"], "implement")
        self.exec_sql(
            """INSERT INTO forge_agent.coding_tasks(
                   owner_id, project_alias, id, idempotency_key, intent_digest, base_revision,
                   mode, agent_profile, title, prompt, acceptance_criteria, pinned_recipe,
                   recipe_digest, status, revision, parent_task_id, workflow_required
               ) VALUES(%s,%s,%s,%s,%s,%s,'implement',%s,%s,%s,%s,%s,%s,%s,1,%s,false)""",
            (
                wf["owner_id"], wf["project_alias"], child_id, f"child-{child_id}",
                _digest(str(child_id)), root["base_revision"], wf["author_profile"],
                "External child", "Implement approved plan", "Pass all gates",
                Json(impl_recipe.to_dict()), impl_recipe.digest, status, wf["root_task_id"],
            ),
        )
        self.exec_sql(
            """INSERT INTO forge_agent.coding_events(owner_id, project_alias, task_id, sequence, event_kind, payload)
               VALUES(%s,%s,%s,1,'queued',%s)""",
            (wf["owner_id"], wf["project_alias"], child_id, Json({"actor": COORDINATOR_ACTOR})),
        )
        self.exec_sql(
            """INSERT INTO forge_external_workflow.tasks(owner_id, project_alias, task_id, workflow_id, role)
               VALUES(%s,%s,%s,%s,%s)""",
            (wf["owner_id"], wf["project_alias"], child_id, workflow_id, role),
        )
        self.exec_sql(
            """UPDATE forge_external_workflow.workflows
               SET active_task_id=%s, phase='repair', revision=revision+1
               WHERE workflow_id=%s""",
            (child_id, workflow_id),
        )
        return child_id

    # ------------------------------------------------------------------
    # AC 1: Escalation settles open retries atomically; preserves exhausted
    # ------------------------------------------------------------------

    def test_ac1_native_escalate_resolves_review_2_of_20_and_verification_0_of_20(self):
        # Case 3824445d: review 2/20 open
        wf_review, root_review = self.submit_native_workflow()
        ev_review = self.open_retry(
            owner=self.native.config.owner, scope_kind="native_workflow",
            scope_id=wf_review, kind="review", used=2,
        )
        # Case c87bf342: verification 0/20 open
        wf_verify, root_verify = self.submit_native_workflow()
        ev_verify = self.open_retry(
            owner=self.native.config.owner, scope_kind="native_workflow",
            scope_id=wf_verify, kind="verification", used=0,
        )

        # Live workflows still project open retry events (AC 3)
        active = self.list_by_scope(self.native.service, "active")
        self.assertEqual(active[str(root_review)]["retry_event"]["status"], "open")
        self.assertEqual(active[str(root_review)]["retry_event"]["kind"], "review")
        self.assertEqual(active[str(root_review)]["retry_event"]["used"], 2)
        self.assertEqual(active[str(root_verify)]["retry_event"]["status"], "open")
        self.assertEqual(active[str(root_verify)]["retry_event"]["kind"], "verification")
        self.assertEqual(active[str(root_verify)]["retry_event"]["used"], 0)

        # Escalate both via WorkflowStore._escalate in their transactions
        with self.native.coordinator.store._transaction() as cursor:
            for wf_id, reason in ((wf_review, "Code review escalated (3824445d)."),
                                  (wf_verify, "Verification escalated (c87bf342).")):
                cursor.execute("SELECT * FROM forge_agent.coding_workflows WHERE workflow_id=%s FOR UPDATE", (wf_id,))
                self.native.coordinator.store._escalate(cursor, cursor.fetchone(), reason)

        attention = self.list_by_scope(self.native.service, "attention")
        for root_id, ev_id, kind, used in (
            (root_review, ev_review["event_id"], "review", 2),
            (root_verify, ev_verify["event_id"], "verification", 0),
        ):
            item = attention[str(root_id)]
            self.assertEqual(item["workflow_phase"], "escalated")
            self.assertEqual(item["retry_event"]["status"], "resolved")
            self.assertEqual(item["retry_event"]["kind"], kind)
            self.assertEqual(item["retry_event"]["used"], used)
            db_row = self.rows(
                "SELECT status, resolved_at FROM forge_agent.coding_retry_events WHERE event_id=%s",
                (UUID(ev_id),),
            )[0]
            self.assertEqual(db_row["status"], "resolved")
            self.assertIsNotNone(db_row["resolved_at"])
            # No fabricated 'passed' observation
            obs = self.rows(
                "SELECT count(*) AS n FROM forge_agent.coding_retry_event_observations WHERE event_id=%s AND observation_kind='passed'",
                (UUID(ev_id),),
            )[0]["n"]
            self.assertEqual(obs, 0)

    def test_ac1_external_escalate_resolves_feedback_delivery_1_of_20(self):
        # Case 7615bf8b: feedback_delivery 1/20 open
        wf_id, root_id = self.submit_external_workflow()
        ev = self.open_retry(
            owner=self.ext_config.owner, scope_kind="external_workflow",
            scope_id=wf_id, kind="feedback_delivery", used=1,
        )
        # Live external workflow projects open retry event (AC 3)
        active = self.list_by_scope(self.ext_service, "active")
        self.assertEqual(active[str(root_id)]["retry_event"]["status"], "open")
        self.assertEqual(active[str(root_id)]["retry_event"]["kind"], "feedback_delivery")
        self.assertEqual(active[str(root_id)]["retry_event"]["used"], 1)

        with self.ext_flow._transaction() as cursor:
            cursor.execute("SELECT * FROM forge_external_workflow.workflows WHERE workflow_id=%s FOR UPDATE", (wf_id,))
            self.ext_flow._escalate(cursor, cursor.fetchone(), "Feedback delivery escalated (7615bf8b).")

        attention = self.list_by_scope(self.ext_service, "attention")
        item = attention[str(root_id)]
        self.assertEqual(item["workflow_phase"], "escalated")
        self.assertEqual(item["retry_event"]["status"], "resolved")
        self.assertEqual(item["retry_event"]["kind"], "feedback_delivery")
        self.assertEqual(item["retry_event"]["used"], 1)
        db_row = self.rows(
            "SELECT status, resolved_at FROM forge_agent.coding_retry_events WHERE event_id=%s",
            (UUID(ev["event_id"]),),
        )[0]
        self.assertEqual(db_row["status"], "resolved")
        self.assertIsNotNone(db_row["resolved_at"])

    def test_ac1_multiple_open_events_rollback_atomicity_and_exhausted_preservation(self):
        wf_id, root_id = self.submit_native_workflow()
        ev_open_1 = self.open_retry(
            owner=self.native.config.owner, scope_kind="native_workflow",
            scope_id=wf_id, kind="review", used=2,
        )
        ev_open_2 = self.open_retry(
            owner=self.native.config.owner, scope_kind="native_workflow",
            scope_id=wf_id, kind="verification", used=0,
        )
        ev_exhausted = self.exhaust_retry(
            owner=self.native.config.owner, scope_kind="native_workflow",
            scope_id=wf_id, kind="tests",
        )

        # Rollback atomicity: both escalation and retry settlement roll back together
        with self.assertRaises(RuntimeError):
            with self.native.coordinator.store._transaction() as cursor:
                cursor.execute("SELECT * FROM forge_agent.coding_workflows WHERE workflow_id=%s FOR UPDATE", (wf_id,))
                self.native.coordinator.store._escalate(cursor, cursor.fetchone(), "Simulated rollback.")
                raise RuntimeError("abort_tx")

        self.assertEqual(
            self.rows("SELECT phase FROM forge_agent.coding_workflows WHERE workflow_id=%s", (wf_id,))[0]["phase"],
            "plan",
        )
        for ev in (ev_open_1, ev_open_2):
            self.assertEqual(
                self.rows("SELECT status FROM forge_agent.coding_retry_events WHERE event_id=%s", (UUID(ev["event_id"]),))[0]["status"],
                "open",
            )

        # Now commit escalation: both open events resolve, exhausted stays exhausted
        with self.native.coordinator.store._transaction() as cursor:
            cursor.execute("SELECT * FROM forge_agent.coding_workflows WHERE workflow_id=%s FOR UPDATE", (wf_id,))
            self.native.coordinator.store._escalate(cursor, cursor.fetchone(), "Committed escalation.")

        statuses = {
            str(r["event_id"]): r["status"]
            for r in self.rows("SELECT event_id, status FROM forge_agent.coding_retry_events WHERE scope_id=%s", (wf_id,))
        }
        self.assertEqual(statuses[ev_open_1["event_id"]], "resolved")
        self.assertEqual(statuses[ev_open_2["event_id"]], "resolved")
        self.assertEqual(statuses[ev_exhausted["event_id"]], "exhausted")
        # List projection still surfaces the preserved exhausted event (20/20)
        attention = self.list_by_scope(self.native.service, "attention")
        self.assertEqual(attention[str(root_id)]["retry_event"]["status"], "exhausted")
        self.assertEqual(attention[str(root_id)]["retry_event"]["used"], 20)

    def test_ac1_native_merge_finish_and_owner_fenced_merge_failure_settle_open_retries(self):
        import json
        import test_work_owner_fence as fence_fixtures

        # 1. merge_finish handoff settles open retry in same transaction
        _, _, critic, _ = self.native.checked_implementation()
        self.native.publish(critic, self.native.verdict(critic))
        self.native.coordinator.run_once()
        pub = self.native.db.publication_claim("publisher")
        self.native.db.publication_finish(pub, "publishing", "Owned branch", branch="forge/test", commit_sha="d" * 40)
        self.native.db.publication_finish(pub, "published", "Owned branch", branch="forge/test", commit_sha="d" * 40, pr_url="https://github.com/fixture/project/pull/1")
        build = dict(provider="jenkins", job="forge-native-candidate", action_id=str(uuid4()), queue_id=1, build_number=10)
        self.native.db.record_hosted_checks(
            pub["publication_id"], "d" * 40,
            [dict(name=n, status="success", head_sha="d" * 40, **build) for n in ("check", "pr-fast-gate", "real-browser")],
        )
        self.native.coordinator.run_once()
        merge_job = self.native.db.merge_claim("merge-worker")
        self.assertIsNotNone(merge_job)
        wf_id = UUID(str(merge_job["workflow_id"]))
        ev = self.open_retry(
            owner=self.native.config.owner, scope_kind="native_workflow",
            scope_id=wf_id, kind="merge", used=1,
        )
        self.native.db.merge_finish(
            merge_job, "handoff",
            {"candidate_commit": "d" * 40, "merge_commit": None,
             "pr_url": "https://github.com/fixture/project/pull/1", "summary": "Merge handoff escalation."},
        )
        self.assertEqual(
            self.rows("SELECT phase FROM forge_agent.coding_workflows WHERE workflow_id=%s", (wf_id,))[0]["phase"],
            "escalated",
        )
        self.assertEqual(
            self.rows("SELECT status FROM forge_agent.coding_retry_events WHERE event_id=%s", (UUID(ev["event_id"]),))[0]["status"],
            "resolved",
        )

        # 2. native_merge_failure returning status=='handoff' (owner-fenced) resolves open retry
        fence = fence_fixtures.OwnerContinuationFenceTest()
        fence.cluster = self.cluster
        fence.setUp()
        _, scope, _, _ = fence.owner_marker()
        author2, checks2, outcome2 = fence.checked_owner_attempt(scope)
        fence.f.db.review_finish(checks2, outcome2)
        fence.f.coordinator.run_once()
        critic2 = fence.f.claim("codex-v2")
        envelope = fence.f.db.read_review_target(critic2)
        target = json.loads(envelope["target_json"])
        fence.f.publish(
            critic2,
            fence.f.verdict(
                critic2,
                attested_finding_ids=[f["finding_id"] for f in target["findings"] if f.get("status", "open") == "open"],
            ),
        )
        fence.f.coordinator.run_once()
        pub2 = fence.f.db.publication_claim("publisher2")
        fence.f.db.publication_finish(pub2, "publishing", "Owned branch", branch="forge/test2", commit_sha="d" * 40)
        fence.f.db.publication_finish(pub2, "published", "Owned branch", branch="forge/test2", commit_sha="d" * 40, pr_url="https://github.com/fixture/project/pull/2")
        build2 = dict(provider="jenkins", job="forge-native-candidate", action_id=str(uuid4()), queue_id=9, build_number=6)
        fence.f.db.record_hosted_checks(
            pub2["publication_id"], "d" * 40,
            [dict(name=n, status="success", head_sha="d" * 40, **build2) for n in ("check", "pr-fast-gate", "real-browser")],
        )
        fence.f.coordinator.run_once()
        merge_job2 = fence.f.db.merge_claim("merge-worker-2")
        self.assertIsNotNone(merge_job2)
        wf2 = UUID(str(merge_job2["workflow_id"]))
        ev2 = self.open_retry(
            owner=fence.f.config.owner, scope_kind="native_workflow",
            scope_id=wf2, kind="verification", used=0,
        )
        failure = dict(
            version=1, reason="merge_main_requires_reverification",
            summary="Actual main drift requires repair.", source_review_id=str(uuid4()),
            candidate_commit="c" * 40, candidate_workspace_digest="b" * 64,
            integration_base="a" * 40, gate_manifest_sha256="9" * 64, gate_source_sha256="8" * 64,
            tested_candidate_commit="d" * 40, evidence_digest="7" * 64,
            findings=[dict(finding_id="merge-source", category="source", summary="Resolve main drift.")],
            build=build2, no_merge_confirmed=True,
            delivery_failure=dict(
                version=1, tested_candidate="d" * 40, current_main="a" * 40,
                build=build2, conflict_paths=["src/owned.rs"],
            ),
        )
        res = fence.f.db.native_merge_failure(merge_job2, failure)
        self.assertEqual(res["status"], "handoff")
        self.assertEqual(
            self.rows("SELECT phase FROM forge_agent.coding_workflows WHERE workflow_id=%s", (wf2,))[0]["phase"],
            "escalated",
        )
        self.assertEqual(
            self.rows("SELECT status FROM forge_agent.coding_retry_events WHERE event_id=%s", (UUID(ev2["event_id"]),))[0]["status"],
            "resolved",
        )

    # ------------------------------------------------------------------
    # AC 2: Root cancellation moves workflow to cancelled & settles retries
    # ------------------------------------------------------------------

    def test_ac2_native_service_cancel_and_recover_settle_cancelled_root(self):
        # 1. WorkService.cancel on native workflow resolves open retry in same transaction
        wf1, root1 = self.submit_native_workflow()
        ev1 = self.open_retry(
            owner=self.native.config.owner, scope_kind="native_workflow",
            scope_id=wf1, kind="review", used=2,
        )
        self.native.service.cancel(WorkCancel(
            project_alias="homelab", task_id=root1, expected_revision=1, idempotency_key="native-cancel-1",
        ))
        history = self.list_by_scope(self.native.service, "history")
        self.assertEqual(history[str(root1)]["workflow_phase"], "cancelled")
        self.assertEqual(history[str(root1)]["retry_event"]["status"], "resolved")
        self.assertEqual(
            self.rows("SELECT status FROM forge_agent.coding_retry_events WHERE event_id=%s", (UUID(ev1["event_id"]),))[0]["status"],
            "resolved",
        )

        # 2. WorkflowStore.recover on cancelled root moves workflow to cancelled (not escalated)
        wf2, root2 = self.submit_native_workflow()
        ev2 = self.open_retry(
            owner=self.native.config.owner, scope_kind="native_workflow",
            scope_id=wf2, kind="verification", used=0,
        )
        self.exec_sql(
            "UPDATE forge_agent.coding_tasks SET status='cancelled', cancelled_at=clock_timestamp(), revision=revision+1 WHERE id=%s",
            (root2,),
        )
        self.native.coordinator.run_once()
        wf2_row = self.rows("SELECT phase, enabled FROM forge_agent.coding_workflows WHERE workflow_id=%s", (wf2,))[0]
        self.assertEqual((wf2_row["phase"], wf2_row["enabled"]), ("cancelled", False))
        self.assertEqual(
            self.rows("SELECT status FROM forge_agent.coding_retry_events WHERE event_id=%s", (UUID(ev2["event_id"]),))[0]["status"],
            "resolved",
        )
        history = self.list_by_scope(self.native.service, "history")
        self.assertEqual(history[str(root2)]["workflow_phase"], "cancelled")
        self.assertEqual(history[str(root2)]["retry_event"]["status"], "resolved")

    def test_ac2_native_recover_cancelled_and_cancel_requested_root_outranks_invalid_policy(self):
        for root_status, invalid_mode in (
            ("cancelled", "removed_delegation"),
            ("cancel_requested", "mismatched_policy_digest"),
        ):
            with self.subTest(root_status=root_status, invalid_mode=invalid_mode):
                self.cluster.reset_coding_tables()
                self.native.setUp()
                self.native.service._store.retry_events = self.native_provider
                self.native.coordinator.store.retry_events = self.native_provider
                _, admitted = self.native.submit()
                self.native.finish_plan_round("approve")
                wf_row = self.rows(
                    "SELECT workflow_id, root_task_id FROM forge_agent.coding_workflows WHERE root_task_id=%s",
                    (UUID(admitted["task_id"]),),
                )[0]
                wf_id, root_id = wf_row["workflow_id"], wf_row["root_task_id"]
                ev = self.open_retry(
                    owner=self.native.config.owner, scope_kind="native_workflow",
                    scope_id=wf_id, kind="review", used=2,
                )
                # Put a pending workflow action so we verify pending actions are cancelled too
                pending_key = f"pending-{uuid4()}"
                self.exec_sql(
                    """INSERT INTO forge_agent.coding_workflow_actions(
                           workflow_id, owner_id, project_alias, kind, source_key
                       ) VALUES(%s,%s,'homelab','decide',%s)""",
                    (wf_id, self.native.config.owner, pending_key),
                )
                # Mark root as cancelled or cancel_requested while leaving implement sibling queued
                self.exec_sql(
                    "UPDATE forge_agent.coding_tasks SET status=%s, cancelled_at=CASE WHEN %s='cancelled' THEN clock_timestamp() ELSE NULL END WHERE id=%s",
                    (root_status, root_status, root_id),
                )
                if invalid_mode == "removed_delegation":
                    bad_cfg = replace(self.native.config, project_workflows={})
                    self.native.coordinator.store.config = bad_cfg
                else:
                    # Swap author/reviewer roles so _current_policy(w) fails on mismatched policy digest
                    swapped = replace(
                        self.native.config,
                        project_workflows={"homelab": {"author_profile": "codex-v2", "reviewer_profile": "agy-v2"}},
                    )
                    self.native.coordinator.store.config = swapped

                self.native.coordinator.run_once()
                w = self.rows("SELECT phase, enabled, escalation_reason FROM forge_agent.coding_workflows WHERE workflow_id=%s", (wf_id,))[0]
                self.assertEqual((w["phase"], w["enabled"]), ("cancelled", False))
                # Open retry event settled in same transaction
                self.assertEqual(
                    self.rows("SELECT status FROM forge_agent.coding_retry_events WHERE event_id=%s", (UUID(ev["event_id"]),))[0]["status"],
                    "resolved",
                )
                # Sibling task and pending workflow action stopped
                siblings = self.rows(
                    "SELECT status FROM forge_agent.coding_tasks WHERE workflow_id=%s AND id<>%s",
                    (wf_id, root_id),
                )
                self.assertTrue(all(s["status"] in ("cancelled", "cancel_requested", "completed") for s in siblings))
                pending_row = self.rows(
                    "SELECT status FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND source_key=%s",
                    (wf_id, pending_key),
                )[0]
                self.assertEqual(pending_row["status"], "cancelled")
                # List projects Cancelled in history scope, never attention/escalated
                history = self.list_by_scope(self.native.service, "history")
                self.assertEqual(history[str(root_id)]["workflow_phase"], "cancelled")
                self.assertEqual(history[str(root_id)]["retry_event"]["status"], "resolved")

    def test_ac2_native_process_defense_in_depth_cancels_instead_of_escalating_cancelled_root(self):
        _, admitted = self.native.submit()
        lease = self.native.claim("agy-v2")
        receipt = self.native.publish(lease, "# Plan\nDetailed plan text.")
        self.native.db.queue_review(lease, receipt)
        # Claim creates the review action in recover() and leases it
        action = self.native.coordinator.store.claim("wf-worker")
        self.assertIsNotNone(action)
        wf_id = action["workflow_id"]
        root_id = UUID(admitted["task_id"])
        ev = self.open_retry(
            owner=self.native.config.owner, scope_kind="native_workflow",
            scope_id=wf_id, kind="review", used=2,
        )
        # Simulate root task already cancelled + invalid policy before process() executes
        self.exec_sql(
            "UPDATE forge_agent.coding_tasks SET status='cancelled', cancelled_at=clock_timestamp() WHERE id=%s",
            (root_id,),
        )
        self.native.coordinator.store.config = replace(self.native.config, project_workflows={})

        self.native.coordinator.store.process(action)
        w = self.rows("SELECT phase, enabled FROM forge_agent.coding_workflows WHERE workflow_id=%s", (wf_id,))[0]
        self.assertEqual((w["phase"], w["enabled"]), ("cancelled", False))
        self.assertEqual(
            self.rows("SELECT status FROM forge_agent.coding_retry_events WHERE event_id=%s", (UUID(ev["event_id"]),))[0]["status"],
            "resolved",
        )
        history = self.list_by_scope(self.native.service, "history")
        self.assertEqual(history[str(root_id)]["workflow_phase"], "cancelled")
        self.assertEqual(history[str(root_id)]["retry_event"]["status"], "resolved")

    def test_ac2_external_cancel_service_direct_store_step_and_revoked_grant_cases(self):
        # 1. External cancel via WorkService.cancel (case 020118a9 + feedback_delivery 1/20)
        wf1, root1 = self.submit_external_workflow()
        child1 = self.add_external_child_task(wf1)
        ev1 = self.open_retry(
            owner=self.ext_config.owner, scope_kind="external_workflow",
            scope_id=wf1, kind="feedback_delivery", used=1,
        )
        self.ext_service.cancel(WorkCancel(
            project_alias="homelab", task_id=root1, expected_revision=1, idempotency_key="ext-cancel-020118a9",
        ))
        self.assertEqual(
            self.rows("SELECT phase, enabled FROM forge_external_workflow.workflows WHERE workflow_id=%s", (wf1,))[0]["phase"],
            "cancelled",
        )
        self.assertEqual(
            self.rows("SELECT status FROM forge_agent.coding_tasks WHERE id=%s", (child1,))[0]["status"],
            "cancelled",
        )
        self.assertEqual(
            self.rows("SELECT status FROM forge_agent.coding_retry_events WHERE event_id=%s", (UUID(ev1["event_id"]),))[0]["status"],
            "resolved",
        )
        history = self.list_by_scope(self.ext_service, "history")
        self.assertEqual(history[str(root1)]["workflow_phase"], "cancelled")
        self.assertEqual(history[str(root1)]["retry_event"]["status"], "resolved")

        # 2. Direct WorkStore.cancel on external root (case b5ee2238)
        wf2, root2 = self.submit_external_workflow()
        child2 = self.add_external_child_task(wf2)
        ev2 = self.open_retry(
            owner=self.ext_config.owner, scope_kind="external_workflow",
            scope_id=wf2, kind="feedback_delivery", used=1,
        )
        # Non-root child cannot be cancelled via WorkStore.cancel either
        with self.assertRaises(Rejected) as ctx:
            self.ext_service._store.cancel(
                project_alias="homelab", task_id=child2, expected_revision=1, idempotency_key="child-cancel-denied",
            )
        self.assertEqual(ctx.exception.code, "not_cancellable")

        receipt2 = self.ext_service._store.cancel(
            project_alias="homelab", task_id=root2, expected_revision=1, idempotency_key="ext-direct-b5ee2238",
        )
        self.assertEqual(receipt2["status"], "cancelled")
        self.assertEqual(
            self.rows("SELECT phase, enabled FROM forge_external_workflow.workflows WHERE workflow_id=%s", (wf2,))[0]["phase"],
            "cancelled",
        )
        self.assertEqual(
            self.rows("SELECT status FROM forge_agent.coding_tasks WHERE id=%s", (child2,))[0]["status"],
            "cancelled",
        )
        self.assertEqual(
            self.rows("SELECT status FROM forge_agent.coding_retry_events WHERE event_id=%s", (UUID(ev2["event_id"]),))[0]["status"],
            "resolved",
        )
        history = self.list_by_scope(self.ext_service, "history")
        self.assertEqual(history[str(root2)]["workflow_phase"], "cancelled")
        self.assertEqual(history[str(root2)]["retry_event"]["status"], "resolved")

        # 3. External _step observing cancelled root while active_task_id != root_task_id
        wf3, root3 = self.submit_external_workflow()
        child3 = self.add_external_child_task(wf3, status="queued")
        ev3 = self.open_retry(
            owner=self.ext_config.owner, scope_kind="external_workflow",
            scope_id=wf3, kind="feedback_delivery", used=1,
        )
        self.exec_sql(
            "UPDATE forge_agent.coding_tasks SET status='cancelled', cancelled_at=clock_timestamp(), revision=revision+1 WHERE id=%s",
            (root3,),
        )
        self.assertTrue(self.ext_flow.run_once())
        self.assertEqual(
            self.rows("SELECT phase, enabled FROM forge_external_workflow.workflows WHERE workflow_id=%s", (wf3,))[0]["phase"],
            "cancelled",
        )
        self.assertEqual(
            self.rows("SELECT status FROM forge_agent.coding_tasks WHERE id=%s", (child3,))[0]["status"],
            "cancelled",
        )
        self.assertEqual(
            self.rows("SELECT status FROM forge_agent.coding_retry_events WHERE event_id=%s", (UUID(ev3["event_id"]),))[0]["status"],
            "resolved",
        )

        # 4. External run_once with revoked grant + already-cancelled root moves to cancelled (not escalated)
        wf4, root4 = self.submit_external_workflow()
        self.add_external_child_task(wf4, status="queued")
        ev4 = self.open_retry(
            owner=self.ext_config.owner, scope_kind="external_workflow",
            scope_id=wf4, kind="feedback_delivery", used=1,
        )
        self.exec_sql(
            "UPDATE forge_agent.coding_tasks SET status='cancelled', cancelled_at=clock_timestamp(), revision=revision+1 WHERE id=%s",
            (root4,),
        )
        self.cluster.admin_exec("UPDATE forge_external_agent.scope_grants SET enabled=false")
        self.assertTrue(self.ext_flow.run_once())
        self.assertEqual(
            self.rows("SELECT phase, enabled FROM forge_external_workflow.workflows WHERE workflow_id=%s", (wf4,))[0]["phase"],
            "cancelled",
        )
        self.assertEqual(
            self.rows("SELECT status FROM forge_agent.coding_retry_events WHERE event_id=%s", (UUID(ev4["event_id"]),))[0]["status"],
            "resolved",
        )
        history = self.list_by_scope(self.ext_service, "history")
        self.assertEqual(history[str(root4)]["workflow_phase"], "cancelled")
        self.assertEqual(history[str(root4)]["retry_event"]["status"], "resolved")

    def test_ac2_external_direct_cancel_and_coordinator_serialize_without_deadlock(self):
        def wait_for_blocked_query(fragment, timeout=5.0):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                blocked = self.rows(
                    """SELECT pid, wait_event_type, wait_event, query
                       FROM pg_stat_activity
                       WHERE datname = current_database()
                         AND wait_event_type = 'Lock'
                         AND cardinality(pg_blocking_pids(pid)) > 0
                         AND query LIKE %s""",
                    (f"%{fragment}%",),
                )
                if blocked:
                    return blocked
                time.sleep(0.02)
            return []

        # Direction 1: Coordinator (ExternalWorkflowStore.run_once) locks workflow first
        # and pauses BEFORE locking root/child tasks in _escalate/_cancel_children, while
        # direct WorkStore.cancel runs concurrently on the same external root task.
        wf1, root1 = self.submit_external_workflow()
        self.add_external_child_task(wf1, status="queued")
        ev1 = self.open_retry(
            owner=self.ext_config.owner, scope_kind="external_workflow",
            scope_id=wf1, kind="feedback_delivery", used=1,
        )
        self.cluster.admin_exec("UPDATE forge_external_agent.scope_grants SET enabled=false")

        coord_locked_workflow = threading.Event()
        release_coord = threading.Event()
        orig_escalate = self.ext_flow._escalate

        def paused_escalate(cursor, w, reason):
            coord_locked_workflow.set()
            if not release_coord.wait(timeout=5.0):
                raise AssertionError("Timed out waiting to release coordinator")
            return orig_escalate(cursor, w, reason)

        coord_result = {}
        cancel_result = {}

        def run_coordinator():
            try:
                coord_result["value"] = self.ext_flow.run_once()
            except BaseException as exc:
                coord_result["error"] = exc

        def run_direct_cancel():
            try:
                cancel_result["value"] = self.ext_service._store.cancel(
                    project_alias="homelab",
                    task_id=root1,
                    expected_revision=1,
                    idempotency_key="concurrent-direct-cancel-1",
                )
            except BaseException as exc:
                cancel_result["error"] = exc

        self.ext_flow._escalate = paused_escalate
        t_coord = threading.Thread(target=run_coordinator, daemon=True)
        t_cancel = threading.Thread(target=run_direct_cancel, daemon=True)
        try:
            t_coord.start()
            self.assertTrue(coord_locked_workflow.wait(timeout=5.0))
            t_cancel.start()
            # Direct WorkStore.cancel must wait on the external workflow lock BEFORE
            # acquiring any coding_tasks or advisory lock (no lock-order cycle).
            # The cancel runs as the coding_workflow_cancel composite, so the
            # blocked session's query text is the composite call; it waits on
            # the same workflow row lock inside the function.
            blocked = wait_for_blocked_query("coding_workflow_cancel")
            self.assertTrue(blocked, "Expected direct WorkStore.cancel to wait on external workflow lock")
            cancel_pid = blocked[0]["pid"]
            held_task_locks = self.rows(
                """SELECT mode FROM pg_locks
                   WHERE pid = %s AND granted
                     AND relation = 'forge_agent.coding_tasks'::regclass""",
                (cancel_pid,),
            )
            self.assertEqual(held_task_locks, [])
        finally:
            release_coord.set()
            t_coord.join(timeout=10.0)
            t_cancel.join(timeout=10.0)
            self.ext_flow._escalate = orig_escalate

        self.assertNotIn("error", coord_result, f"Coordinator failed: {coord_result.get('error')!r}")
        self.assertTrue(coord_result.get("value"))
        # Direct cancel wakes cleanly after coordinator commits and observes terminal/advanced state
        # without any PostgreSQL deadlock or lock timeout.
        self.assertIsInstance(cancel_result.get("error"), Rejected)
        self.assertIn(cancel_result["error"].code, {"not_cancellable", "revision_conflict"})
        self.assertEqual(
            self.rows("SELECT phase, enabled FROM forge_external_workflow.workflows WHERE workflow_id=%s", (wf1,))[0]["phase"],
            "escalated",
        )
        self.assertEqual(
            self.rows("SELECT status FROM forge_agent.coding_retry_events WHERE event_id=%s", (UUID(ev1["event_id"]),))[0]["status"],
            "resolved",
        )

        # Direction 2: Direct WorkStore.cancel locks external workflow -> tasks first and pauses
        # at its retry settlement, while ExternalWorkflowStore.run_once and
        # ExternalWorkflowStore.cancel run concurrently on the same workflow.
        self.cluster.admin_exec("UPDATE forge_external_agent.scope_grants SET enabled=true")
        wf2, root2 = self.submit_external_workflow()
        child2 = self.add_external_child_task(wf2, status="queued")
        ev2 = self.open_retry(
            owner=self.ext_config.owner, scope_kind="external_workflow",
            scope_id=wf2, kind="feedback_delivery", used=1,
        )

        # The direct cancel runs as the coding_workflow_cancel composite:
        # its settlement step updates the scope's open retry rows, so
        # holding ev2's row lock pauses it mid-cancel — after it has
        # locked the external workflow, before it settles — at exactly
        # the point the inline implementation paused in
        # _resolve_open_retries.
        hold = self.cluster.connect("postgres")
        hold_cursor = hold.cursor()
        hold_cursor.execute(
            "SELECT event_id FROM forge_agent.coding_retry_events"
            " WHERE event_id = %s FOR UPDATE",
            (UUID(ev2["event_id"]),),
        )

        direct_result = {}
        ext_cancel_result = {}

        def run_direct():
            try:
                direct_result["value"] = self.ext_service._store.cancel(
                    project_alias="homelab",
                    task_id=root2,
                    expected_revision=1,
                    idempotency_key="concurrent-direct-cancel-2",
                )
            except BaseException as exc:
                direct_result["error"] = exc

        def run_ext_cancel():
            try:
                ext_cancel_result["value"] = self.ext_flow.cancel(
                    project_alias="homelab",
                    task_id=root2,
                    expected_revision=1,
                    idempotency_key="concurrent-direct-cancel-2",
                )
            except BaseException as exc:
                ext_cancel_result["error"] = exc

        t_direct = threading.Thread(target=run_direct, daemon=True)
        t_ext_cancel = threading.Thread(target=run_ext_cancel, daemon=True)
        try:
            t_direct.start()
            # The direct cancel blocks settling ev2 (held above) while
            # holding the external workflow lock.
            self.assertTrue(wait_for_blocked_query("coding_workflow_cancel"))
            # ExternalWorkflowStore.run_once uses SKIP LOCKED and completes cleanly without blocking.
            self.assertFalse(self.ext_flow.run_once())
            # Concurrent ExternalWorkflowStore.cancel waits on the same workflow lock, then
            # observes the durable idempotency receipt after direct cancel commits.
            t_ext_cancel.start()
            self.assertTrue(wait_for_blocked_query("forge_external_workflow.workflows"))
        finally:
            hold.commit()
            hold.close()
            t_direct.join(timeout=10.0)
            t_ext_cancel.join(timeout=10.0)

        self.assertNotIn("error", direct_result, f"Direct cancel failed: {direct_result.get('error')!r}")
        self.assertNotIn("error", ext_cancel_result, f"External cancel failed: {ext_cancel_result.get('error')!r}")
        self.assertEqual(direct_result["value"]["status"], "cancelled")
        self.assertEqual(ext_cancel_result["value"], direct_result["value"])
        self.assertEqual(
            self.rows("SELECT phase, enabled FROM forge_external_workflow.workflows WHERE workflow_id=%s", (wf2,))[0]["phase"],
            "cancelled",
        )
        self.assertEqual(
            self.rows("SELECT status FROM forge_agent.coding_tasks WHERE id=%s", (child2,))[0]["status"],
            "cancelled",
        )
        self.assertEqual(
            self.rows("SELECT status FROM forge_agent.coding_retry_events WHERE event_id=%s", (UUID(ev2["event_id"]),))[0]["status"],
            "resolved",
        )
        history = self.list_by_scope(self.ext_service, "history")
        self.assertEqual(history[str(root2)]["workflow_phase"], "cancelled")
        self.assertEqual(history[str(root2)]["retry_event"]["status"], "resolved")

    def test_ac2_manual_task_cancel_resolves_open_retry_and_preserves_live_guard(self):
        manual_cfg = replace(self.native.config, project_workflows={})
        manual_service = WorkServiceFactory(manual_cfg, lambda: self.cluster.connect("forge_agent")).create()
        admitted = manual_service.submit(make_submit(idempotency_key="manual-retry-cancel"))
        task_id = UUID(admitted["task_id"])
        ev = self.open_retry(
            owner=manual_cfg.owner, scope_kind="manual_task",
            scope_id=task_id, kind="tests", used=3,
        )
        # AC 3 DB guard: live manual task rejects resolve_open_scope with P0009
        with closing(self.cluster.connect("forge_agent")) as c, c, c.cursor() as q:
            q.execute("SELECT set_config('forge.coding_owner', %s, true)", (manual_cfg.owner,))
            with self.assertRaises(psycopg2.Error) as err:
                q.execute("SELECT forge_agent.retry_event_resolve_open_scope('homelab', 'manual_task', %s)", (task_id,))
            self.assertEqual(err.exception.pgcode, "P0009")

        manual_service.cancel(WorkCancel(
            project_alias="homelab", task_id=task_id, expected_revision=1, idempotency_key="manual-cancel-1",
        ))
        self.assertEqual(
            self.rows("SELECT status FROM forge_agent.coding_retry_events WHERE event_id=%s", (UUID(ev["event_id"]),))[0]["status"],
            "resolved",
        )

    # ------------------------------------------------------------------
    # AC 3: Live scopes reject terminal settlement & port enforces contract
    # ------------------------------------------------------------------

    def test_ac3_live_scopes_reject_terminal_settlement_and_bind_checks_port(self):
        wf_native, _ = self.submit_native_workflow()
        wf_ext, _ = self.submit_external_workflow()
        self.open_retry(owner=self.native.config.owner, scope_kind="native_workflow", scope_id=wf_native, kind="review", used=2)
        self.open_retry(owner=self.ext_config.owner, scope_kind="external_workflow", scope_id=wf_ext, kind="feedback_delivery", used=1)

        for owner, kind, scope_id in (
            (self.native.config.owner, "native_workflow", wf_native),
            (self.ext_config.owner, "external_workflow", wf_ext),
        ):
            with closing(self.cluster.connect("forge_agent")) as c, c, c.cursor() as q:
                q.execute("SELECT set_config('forge.coding_owner', %s, true)", (owner,))
                with self.assertRaises(psycopg2.Error) as err:
                    q.execute("SELECT forge_agent.retry_event_resolve_open_scope('homelab', %s, %s)", (kind, scope_id))
                self.assertEqual(err.exception.pgcode, "P0009")

        # ExternalWorkflowStore.bind_retry_events requires resolve_open_scope
        from work_external_workflow import ExternalWorkflowStore
        fresh_flow = ExternalWorkflowStore(self.ext_config, self.ext_config.catalog())
        incomplete = SimpleNamespace(
            observe_failure=lambda *a, **k: None,
            reserve=lambda *a, **k: None,
            resolve_verified=lambda *a, **k: None,
            project=lambda *a, **k: None,
            get=lambda *a, **k: None,
            adopt_legacy=lambda *a, **k: None,
        )
        with self.assertRaises(TypeError):
            fresh_flow.bind_retry_events(incomplete)

    # ------------------------------------------------------------------
    # AC 5: Scratch-DB repair script verification with strict controls
    # ------------------------------------------------------------------

    def test_ac5_scratch_database_repair_settles_exact_stuck_rows_and_preserves_controls(self):
        # 1. Seed the 5 live-verified stuck cases:
        # (a) 3824445d: native escalated with open review 2/20
        wf_3824, root_3824 = self.submit_native_workflow()
        ev_3824 = self.open_retry(
            owner=self.native.config.owner, scope_kind="native_workflow",
            scope_id=wf_3824, kind="review", used=2,
        )
        self.exec_sql(
            "UPDATE forge_agent.coding_workflows SET phase='escalated', enabled=false, escalation_reason='Review failed (3824445d).' WHERE workflow_id=%s",
            (wf_3824,),
        )

        # (b) c87bf342: native escalated with open verification 0/20
        wf_c87b, root_c87b = self.submit_native_workflow()
        ev_c87b = self.open_retry(
            owner=self.native.config.owner, scope_kind="native_workflow",
            scope_id=wf_c87b, kind="verification", used=0,
        )
        self.exec_sql(
            "UPDATE forge_agent.coding_workflows SET phase='escalated', enabled=false, escalation_reason='Verification failed (c87bf342).' WHERE workflow_id=%s",
            (wf_c87b,),
        )

        # (c) 7615bf8b / root 020118a9: external escalated with independently cancelled root + open feedback_delivery 1/20
        wf_7615, root_0201 = self.submit_external_workflow()
        self.add_external_child_task(wf_7615)
        ev_7615 = self.open_retry(
            owner=self.ext_config.owner, scope_kind="external_workflow",
            scope_id=wf_7615, kind="feedback_delivery", used=1,
        )
        self.exec_sql(
            "UPDATE forge_agent.coding_tasks SET status='cancelled', cancelled_at=clock_timestamp(), revision=2 WHERE id=%s",
            (root_0201,),
        )
        self.exec_sql(
            """INSERT INTO forge_agent.coding_events(owner_id, project_alias, task_id, sequence, event_kind, payload)
               VALUES(%s,'homelab',%s,99,'cancelled',%s)""",
            (
                self.ext_config.owner, root_0201,
                Json({"base_revision": 1, "revision": 2,
                      "actor": {"kind": "operator", "id": self.ext_config.owner},
                      "cause": "operator_request"}),
            ),
        )
        self.exec_sql(
            "UPDATE forge_external_workflow.workflows SET phase='escalated', enabled=false, state=state || %s WHERE workflow_id=%s",
            (Json({"escalation_reason": "Stale escalation after root 020118a9 cancel."}), wf_7615),
        )

        # (d) b6d49933 / root b5ee2238: external escalated with independently cancelled root (pre-M36 unattributed cancel event)
        wf_b6d4, root_b5ee = self.submit_external_workflow()
        self.add_external_child_task(wf_b6d4)
        self.exec_sql(
            "UPDATE forge_agent.coding_tasks SET status='cancelled', cancelled_at=clock_timestamp(), revision=2 WHERE id=%s",
            (root_b5ee,),
        )
        self.exec_sql(
            """INSERT INTO forge_agent.coding_events(owner_id, project_alias, task_id, sequence, event_kind, payload)
               VALUES(%s,'homelab',%s,99,'cancelled',%s)""",
            (self.ext_config.owner, root_b5ee, Json({"base_revision": 1, "revision": 2})),
        )
        self.exec_sql(
            "UPDATE forge_external_workflow.workflows SET phase='escalated', enabled=false, state=state || %s WHERE workflow_id=%s",
            (Json({"escalation_reason": "Stale escalation after root b5ee2238 cancel."}), wf_b6d4),
        )

        # 2. Seed Positive Native Repair Case: escalated native workflow with cancel receipt on root_task_id itself
        wf_native_pos, root_native_pos = self.submit_native_workflow()
        self.exec_sql(
            "UPDATE forge_agent.coding_tasks SET status='cancelled', cancelled_at=clock_timestamp(), revision=2 WHERE id=%s",
            (root_native_pos,),
        )
        self.exec_sql(
            """INSERT INTO forge_agent.coding_control_receipts(
                   owner_id, project_alias, task_id, action, idempotency_key, intent_digest,
                   base_revision, result_revision, receipt_json
               ) VALUES(%s,'homelab',%s,'cancel','pos-native-cancel',%s,1,2,%s)""",
            (self.native.config.owner, root_native_pos, _digest("pos-native"), Json({"status": "cancelled"})),
        )
        self.exec_sql(
            "UPDATE forge_agent.coding_workflows SET phase='escalated', enabled=false, escalation_reason='Workflow task was cancelled.' WHERE workflow_id=%s",
            (wf_native_pos,),
        )

        # 3. Seed Controls that MUST remain 'escalated' (or untouched):
        # Control A: Sibling-triggered native escalation (queued root cancelled only by _escalate cleanup)
        wf_ctrl_sib_q, root_ctrl_sib_q = self.submit_native_workflow()
        sib_q = uuid4()
        root_row_q = self.rows("SELECT * FROM forge_agent.coding_tasks WHERE id=%s", (root_ctrl_sib_q,))[0]
        sib_recipe = self.native.config.catalog().resolve(root_row_q["agent_profile"], "implement")
        self.exec_sql(
            """INSERT INTO forge_agent.coding_tasks(
                   owner_id, project_alias, id, idempotency_key, intent_digest, base_revision,
                   mode, agent_profile, title, prompt, acceptance_criteria, pinned_recipe,
                   recipe_digest, status, revision, cancelled_at, parent_task_id, workflow_required, workflow_id
               ) VALUES(%s,'homelab',%s,%s,%s,%s,'implement',%s,'Sibling','p','a',%s,%s,'cancelled',2,clock_timestamp(),%s,true,%s)""",
            (
                self.native.config.owner, sib_q, f"sib-q-{sib_q}", _digest(str(sib_q)),
                root_row_q["base_revision"], root_row_q["agent_profile"],
                Json(sib_recipe.to_dict()), sib_recipe.digest,
                root_ctrl_sib_q, wf_ctrl_sib_q,
            ),
        )
        # Operator cancel receipt/event is on the SIBLING only, NOT the root
        self.exec_sql(
            """INSERT INTO forge_agent.coding_control_receipts(
                   owner_id, project_alias, task_id, action, idempotency_key, intent_digest,
                   base_revision, result_revision, receipt_json
               ) VALUES(%s,'homelab',%s,'cancel','sib-only-cancel',%s,1,2,%s)""",
            (self.native.config.owner, sib_q, _digest("sib-q"), Json({"status": "cancelled"})),
        )
        # Root was stopped only by coordinator cleanup during _escalate with exact escalation_reason
        self.exec_sql(
            "UPDATE forge_agent.coding_tasks SET status='cancelled', cancelled_at=clock_timestamp(), revision=2 WHERE id=%s",
            (root_ctrl_sib_q,),
        )
        self.exec_sql(
            """INSERT INTO forge_agent.coding_events(owner_id, project_alias, task_id, sequence, event_kind, payload)
               VALUES(%s,'homelab',%s,99,'cancelled',%s)""",
            (
                self.native.config.owner, root_ctrl_sib_q,
                Json({"base_revision": 1, "revision": 2, "workflow_id": str(wf_ctrl_sib_q),
                      "actor": COORDINATOR_ACTOR, "cause": "workflow_cancelled",
                      "cause_detail": "Workflow task was cancelled."}),
            ),
        )
        self.exec_sql(
            "UPDATE forge_agent.coding_workflows SET phase='escalated', enabled=false, escalation_reason='Workflow task was cancelled.' WHERE workflow_id=%s",
            (wf_ctrl_sib_q,),
        )
        # Also attach an open retry to Control A — repair must keep phase='escalated' AND resolve its open retry!
        ev_ctrl_sib_q = self.open_retry(
            owner=self.native.config.owner, scope_kind="native_workflow",
            scope_id=wf_ctrl_sib_q, kind="review", used=1,
        )

        # Control B: Sibling-triggered native escalation (running root -> cancel_requested -> worker cancelled)
        wf_ctrl_sib_r, root_ctrl_sib_r = self.submit_native_workflow()
        self.exec_sql(
            "UPDATE forge_agent.coding_tasks SET status='cancelled', cancelled_at=clock_timestamp(), revision=3 WHERE id=%s",
            (root_ctrl_sib_r,),
        )
        self.exec_sql(
            """INSERT INTO forge_agent.coding_events(owner_id, project_alias, task_id, sequence, event_kind, payload)
               VALUES(%s,'homelab',%s,98,'cancel_requested',%s),
                     (%s,'homelab',%s,99,'cancelled',%s)""",
            (
                self.native.config.owner, root_ctrl_sib_r,
                Json({"base_revision": 1, "revision": 2, "workflow_id": str(wf_ctrl_sib_r),
                      "actor": COORDINATOR_ACTOR, "cause": "workflow_cancelled"}),
                self.native.config.owner, root_ctrl_sib_r,
                Json({"attempt_id": str(uuid4()), "actor": {"kind": "worker", "id": "worker-1"},
                      "cause": "workflow_cancelled"}),
            ),
        )
        self.exec_sql(
            "UPDATE forge_agent.coding_workflows SET phase='escalated', enabled=false, escalation_reason='Workflow task was cancelled.' WHERE workflow_id=%s",
            (wf_ctrl_sib_r,),
        )

        # Control C: Genuinely escalated external workflow (cleanup-only root stop + reconciler finalized)
        wf_ctrl_ext_1, root_ctrl_ext_1 = self.submit_external_workflow()
        self.exec_sql(
            "UPDATE forge_agent.coding_tasks SET status='cancelled', cancelled_at=clock_timestamp(), revision=2 WHERE id=%s",
            (root_ctrl_ext_1,),
        )
        self.exec_sql(
            """INSERT INTO forge_agent.coding_events(owner_id, project_alias, task_id, sequence, event_kind, payload)
               VALUES(%s,'homelab',%s,98,'cancelled',%s),
                     (%s,'homelab',%s,99,'checkpoint',%s)""",
            (
                self.ext_config.owner, root_ctrl_ext_1,
                Json({"base_revision": 1, "revision": 2, "workflow_id": str(wf_ctrl_ext_1),
                      "actor": COORDINATOR_ACTOR, "cause": "workflow_cancelled"}),
                self.ext_config.owner, root_ctrl_ext_1,
                Json({"workflow_id": str(wf_ctrl_ext_1), "phase": "escalated", "summary": "Escalated.", "actor": COORDINATOR_ACTOR}),
            ),
        )
        self.exec_sql(
            "UPDATE forge_external_workflow.workflows SET phase='escalated', enabled=false WHERE workflow_id=%s",
            (wf_ctrl_ext_1,),
        )

        wf_ctrl_ext_2, root_ctrl_ext_2 = self.submit_external_workflow()
        self.exec_sql(
            "UPDATE forge_agent.coding_tasks SET status='cancelled', cancelled_at=clock_timestamp(), revision=3 WHERE id=%s",
            (root_ctrl_ext_2,),
        )
        self.exec_sql(
            """INSERT INTO forge_agent.coding_events(owner_id, project_alias, task_id, sequence, event_kind, payload)
               VALUES(%s,'homelab',%s,97,'cancel_requested',%s),
                     (%s,'homelab',%s,98,'checkpoint',%s),
                     (%s,'homelab',%s,99,'cancelled',%s)""",
            (
                self.ext_config.owner, root_ctrl_ext_2,
                Json({"base_revision": 1, "revision": 2, "workflow_id": str(wf_ctrl_ext_2),
                      "actor": COORDINATOR_ACTOR, "cause": "workflow_cancelled"}),
                self.ext_config.owner, root_ctrl_ext_2,
                Json({"workflow_id": str(wf_ctrl_ext_2), "phase": "escalated", "summary": "Escalated.", "actor": COORDINATOR_ACTOR}),
                self.ext_config.owner, root_ctrl_ext_2,
                Json({"assignment_id": str(uuid4()), "reason": "external_lease_reconciled",
                      "actor": {"kind": "service", "id": "forge-external-reconciler"}}),
            ),
        )
        self.exec_sql(
            "UPDATE forge_external_workflow.workflows SET phase='escalated', enabled=false WHERE workflow_id=%s",
            (wf_ctrl_ext_2,),
        )

        # Control D: Live workflow with open retry event (must remain open)
        wf_ctrl_live, root_ctrl_live = self.submit_native_workflow()
        ev_ctrl_live = self.open_retry(
            owner=self.native.config.owner, scope_kind="native_workflow",
            scope_id=wf_ctrl_live, kind="tests", used=3,
        )

        # Control E: Escalated workflow with exhausted 20/20 retry (must remain exhausted)
        wf_ctrl_exh, root_ctrl_exh = self.submit_native_workflow()
        ev_ctrl_exh = self.exhaust_retry(
            owner=self.native.config.owner, scope_kind="native_workflow",
            scope_id=wf_ctrl_exh, kind="compilation",
        )
        self.exec_sql(
            "UPDATE forge_agent.coding_workflows SET phase='escalated', enabled=false, escalation_reason='Exhausted 20/20.' WHERE workflow_id=%s",
            (wf_ctrl_exh,),
        )

        # Assert stale list_tasks projections BEFORE repair
        pre_native_attn = self.list_by_scope(self.native.service, "attention")
        self.assertEqual(pre_native_attn[str(root_3824)]["retry_event"]["status"], "open")
        self.assertEqual(pre_native_attn[str(root_c87b)]["retry_event"]["status"], "open")
        self.assertEqual(pre_native_attn[str(root_native_pos)]["workflow_phase"], "escalated")
        pre_ext_attn = self.list_by_scope(self.ext_service, "attention")
        self.assertEqual(pre_ext_attn[str(root_0201)]["workflow_phase"], "escalated")
        self.assertEqual(pre_ext_attn[str(root_0201)]["retry_event"]["status"], "open")
        self.assertEqual(pre_ext_attn[str(root_b5ee)]["workflow_phase"], "escalated")

        # Execute repair script via psql against the scratch database
        run1 = self.cluster.run_sql_script(REPAIR_SCRIPT)
        self.assertEqual(run1.returncode, 0, run1.stdout + run1.stderr)
        # Verify printed summary counts: 2 external workflows, 1 native workflow, 4 open retry events (3 live-verified + Control A)
        self.assertIn(str(wf_7615), run1.stdout)
        self.assertIn(str(wf_b6d4), run1.stdout)
        self.assertIn(str(wf_native_pos), run1.stdout)
        self.assertIn(ev_3824["event_id"], run1.stdout)
        self.assertIn(ev_c87b["event_id"], run1.stdout)
        self.assertIn(ev_7615["event_id"], run1.stdout)
        self.assertIn(ev_ctrl_sib_q["event_id"], run1.stdout)
        self.assertRegex(run1.stdout, r"\b2\s*\|\s*1\s*\|\s*4\b")

        # Assert post-repair projections for the 5 live-verified cases
        post_native_attn = self.list_by_scope(self.native.service, "attention")
        self.assertEqual(post_native_attn[str(root_3824)]["workflow_phase"], "escalated")
        self.assertEqual(post_native_attn[str(root_3824)]["retry_event"]["status"], "resolved")
        self.assertEqual(post_native_attn[str(root_c87b)]["workflow_phase"], "escalated")
        self.assertEqual(post_native_attn[str(root_c87b)]["retry_event"]["status"], "resolved")

        post_ext_hist = self.list_by_scope(self.ext_service, "history")
        self.assertEqual(post_ext_hist[str(root_0201)]["workflow_phase"], "cancelled")
        self.assertEqual(post_ext_hist[str(root_0201)]["retry_event"]["status"], "resolved")
        self.assertEqual(post_ext_hist[str(root_b5ee)]["workflow_phase"], "cancelled")

        # Assert positive native repair case
        post_native_hist = self.list_by_scope(self.native.service, "history")
        self.assertEqual(post_native_hist[str(root_native_pos)]["workflow_phase"], "cancelled")
        pos_row = self.rows("SELECT phase, enabled, escalation_reason FROM forge_agent.coding_workflows WHERE workflow_id=%s", (wf_native_pos,))[0]
        self.assertEqual((pos_row["phase"], pos_row["enabled"], pos_row["escalation_reason"]), ("cancelled", False, ""))
        pos_cp = self.rows(
            "SELECT payload FROM forge_agent.coding_events WHERE task_id=%s AND event_kind='checkpoint' ORDER BY sequence DESC LIMIT 1",
            (root_native_pos,),
        )[0]["payload"]
        self.assertEqual(pos_cp["phase"], "cancelled")
        self.assertEqual(pos_cp["actor"], {"kind": "operator", "id": "terminal-projection-repair"})

        # Assert ALL controls remained untouched in phase / status
        ctrl_a = self.rows("SELECT phase, escalation_reason FROM forge_agent.coding_workflows WHERE workflow_id=%s", (wf_ctrl_sib_q,))[0]
        self.assertEqual((ctrl_a["phase"], ctrl_a["escalation_reason"]), ("escalated", "Workflow task was cancelled."))
        self.assertEqual(
            self.rows("SELECT status FROM forge_agent.coding_retry_events WHERE event_id=%s", (UUID(ev_ctrl_sib_q["event_id"]),))[0]["status"],
            "resolved",
        )
        self.assertEqual(
            self.rows("SELECT phase FROM forge_agent.coding_workflows WHERE workflow_id=%s", (wf_ctrl_sib_r,))[0]["phase"],
            "escalated",
        )
        self.assertEqual(
            self.rows("SELECT phase FROM forge_external_workflow.workflows WHERE workflow_id=%s", (wf_ctrl_ext_1,))[0]["phase"],
            "escalated",
        )
        self.assertEqual(
            self.rows("SELECT phase FROM forge_external_workflow.workflows WHERE workflow_id=%s", (wf_ctrl_ext_2,))[0]["phase"],
            "escalated",
        )
        self.assertEqual(
            self.rows("SELECT status FROM forge_agent.coding_retry_events WHERE event_id=%s", (UUID(ev_ctrl_live["event_id"]),))[0]["status"],
            "open",
        )
        self.assertEqual(
            self.rows("SELECT status FROM forge_agent.coding_retry_events WHERE event_id=%s", (UUID(ev_ctrl_exh["event_id"]),))[0]["status"],
            "exhausted",
        )
        self.assertEqual(post_native_attn[str(root_ctrl_exh)]["retry_event"]["status"], "exhausted")
        post_native_active = self.list_by_scope(self.native.service, "active")
        self.assertEqual(post_native_active[str(root_ctrl_live)]["retry_event"]["status"], "open")

        # Re-run repair script: idempotent (0 workflows, 0 retries)
        run2 = self.cluster.run_sql_script(REPAIR_SCRIPT)
        self.assertEqual(run2.returncode, 0, run2.stdout + run2.stderr)
        self.assertRegex(run2.stdout, r"\b0\s*\|\s*0\s*\|\s*0\b")

        # Run all SQL verifiers against the repaired scratch DB
        self.cluster.apply_and_verify()
        for verifier in (
            "verify.sql",
            "verify-retry-events.sql",
            "verify-runner-privileges.sql",
            "verify-muse-read-access.sql",
            "verify-autonomy.sql",
            "verify-external-workflows.sql",
        ):
            user = "forge_agent" if verifier == "verify-runner-privileges.sql" else "postgres"
            if verifier == "verify-runner-privileges.sql":
                res = self.cluster._run(
                    "psql", "-h", self.cluster.root, "-U", user, "-X",
                    "-v", "ON_ERROR_STOP=1", "-d", "forge_agent",
                    "-f", SQL_DIR / "management" / verifier,
                )
            elif verifier == "verify.sql":
                res = self.cluster._run(
                    "psql", "-h", self.cluster.root, "-U", "postgres", "-X",
                    "-v", "ON_ERROR_STOP=1", "-d", "forge_agent",
                    "-c", "SET ROLE forge_agent_owner",
                    "-f", SQL_DIR / "management" / verifier,
                )
            else:
                res = self.cluster.run_sql_script(SQL_DIR / "management" / verifier)
            self.assertEqual(res.returncode, 0, f"{verifier} failed: {res.stdout}\n{res.stderr}")


if __name__ == "__main__":
    unittest.main()
