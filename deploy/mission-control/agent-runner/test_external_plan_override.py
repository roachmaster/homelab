"""Authorized external plan approve-override unit, CLI, and PostgreSQL tests.

Verifies the exact narrow override contract: operator-only grant wiring,
explicit artifact/review/revision binding, immutable audit table (P0009 +
UNIQUE(workflow_id)), additive event history, untouched reviews/retry budgets,
muse-db implement handoff through full downstream delivery, and denial of
every forbidden transition and unauthorized database role.
"""
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
from dataclasses import replace
from threading import Barrier
from types import SimpleNamespace
from unittest.mock import Mock, patch
from uuid import UUID, uuid4
import contextlib
import hashlib
import io
import json
import runpy
import unittest

import psycopg2
from psycopg2.extras import Json, RealDictCursor

from queue_store import Rejected
from test_work_execution_and_usage import make_stop_proof, make_submit
from work_api import WorkAccept, WorkEvents, WorkGet, WorkServiceFactory
from work_external import ExternalWorkProviderFactory
from work_external_plan_override import (
    ExternalPlanApproveOverride,
    PlanOverrideGrantScope,
    verified_installed_runtime,
)
from work_external_recovery import NativeReviewerGrantScope
from work_external_review_context import canonical
from work_pg_fixture import PG16_AVAILABLE, SQL_DIR
from work_retry_continuation import ExternalRetryPolicyContinuation
from work_retry_policy import AGENT_RETRY_LIMIT
import test_external_flow as fixture
import work_external_plan_override as override_mod


class ExternalPlanApproveOverrideUnitTest(unittest.TestCase):
    def _valid_kwargs(self, **changes):
        base = dict(
            project_alias="homelab",
            workflow_id=uuid4(),
            root_task_id=uuid4(),
            source_review_id=uuid4(),
            plan_assignment_id=uuid4(),
            plan_sha256="a" * 64,
            expected_workflow_revision=42,
            expected_root_revision=3,
            idempotency_key=uuid4(),
            reason="User accepted the Postgres proxy plan as-is after 20 revise rounds.",
            acceptance_ref="user-decision:2026-11-24#8c300cff",
            confirm_accept_as_is=True,
        )
        base.update(changes)
        return base

    def test_dto_validation_and_required_confirm_attestation(self):
        key = uuid4()
        cmd_uuid = ExternalPlanApproveOverride(**self._valid_kwargs(idempotency_key=key))
        cmd_str = ExternalPlanApproveOverride(**self._valid_kwargs(
            workflow_id=cmd_uuid.workflow_id,
            root_task_id=cmd_uuid.root_task_id,
            source_review_id=cmd_uuid.source_review_id,
            plan_assignment_id=cmd_uuid.plan_assignment_id,
            idempotency_key=str(key),
        ))
        self.assertEqual(cmd_uuid.intent("test-owner"), cmd_str.intent("test-owner"))
        self.assertEqual(cmd_uuid.intent("test-owner")["action"], "plan_approve_override")

        missing_confirm = self._valid_kwargs()
        missing_confirm.pop("confirm_accept_as_is")
        with self.assertRaises(TypeError):
            ExternalPlanApproveOverride(**missing_confirm)

        invalid_cases = [
            {"confirm_accept_as_is": False},
            {"confirm_accept_as_is": 1},
            {"project_alias": ""},
            {"project_alias": "bad project"},
            {"workflow_id": str(uuid4())},
            {"root_task_id": str(uuid4())},
            {"source_review_id": str(uuid4())},
            {"plan_assignment_id": str(uuid4())},
            {"plan_sha256": "A" * 64},
            {"plan_sha256": "a" * 63},
            {"expected_workflow_revision": 0},
            {"expected_workflow_revision": True},
            {"expected_root_revision": 9007199254740992},
            {"idempotency_key": ""},
            {"idempotency_key": "bad key with spaces"},
            {"idempotency_key": "k" * 129},
            {"reason": "   "},
            {"reason": "line\nbreak"},
            {"reason": "r" * 2049},
            {"acceptance_ref": "   "},
            {"acceptance_ref": "ref\x00bad"},
            {"acceptance_ref": "x" * 257},
        ]
        for bad in invalid_cases:
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                ExternalPlanApproveOverride(**self._valid_kwargs(**bad))

    def test_direct_script_entry_delegates_to_canonical_module(self):
        with patch.object(override_mod, "main") as main:
            runpy.run_path(override_mod.__file__, run_name="__main__")
        main.assert_called_once_with()

    def test_cli_requires_confirm_and_execute_and_wires_plan_override_provider(self):
        import work_api
        import work_config

        base_args = [
            "operator",
            "--config", "/owned/config.json",
            "--project", "homelab",
            "--workflow", str(uuid4()),
            "--root-task", str(uuid4()),
            "--source-review", str(uuid4()),
            "--plan-assignment", str(uuid4()),
            "--plan-sha256", "c" * 64,
            "--expected-workflow-revision", "44",
            "--expected-root-revision", "3",
            "--idempotency-key", str(uuid4()),
            "--reason", "User explicitly accepted the plan as-is.",
            "--acceptance-ref", "chat:2026-11-24-accept",
        ]
        for incomplete in (base_args + ["--execute"], base_args + ["--confirm-accept-as-is"]):
            with patch("sys.argv", incomplete), contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit) as exc:
                    override_mod.main()
                self.assertNotEqual(exc.exception.code, 0)

        full_args = base_args + ["--confirm-accept-as-is", "--execute"]
        service = Mock()
        service.approve_external_plan_override.return_value = {"status": "implement_queued"}
        factory = Mock()
        factory.return_value.create.return_value = service
        with (
            patch.object(override_mod.os, "geteuid", return_value=0),
            patch.object(override_mod, "verified_installed_runtime") as verify,
            patch.object(work_config, "load_work_config", return_value=fixture.cfg()),
            patch.object(work_api, "WorkServiceFactory", factory),
            patch("sys.argv", full_args),
            contextlib.redirect_stdout(io.StringIO()),
        ):
            override_mod.main()
        command = service.approve_external_plan_override.call_args.args[0]
        self.assertIs(type(command), override_mod.ExternalPlanApproveOverride)
        self.assertTrue(command.confirm_accept_as_is)
        self.assertEqual(command.expected_workflow_revision, 44)
        self.assertEqual(command.plan_sha256, "c" * 64)
        verify.assert_called_once_with(override_mod.__file__)
        external_factory = factory.call_args.kwargs["external_factory"]
        self.assertTrue(callable(external_factory))
        provider = external_factory(fixture.cfg())
        self.assertIsInstance(provider.workflow._plan_override_grants, PlanOverrideGrantScope)
        self.assertIsNone(provider.workflow._recovery_grants)

    def test_nonroot_operator_and_uninstalled_runtime_are_rejected(self):
        with self.assertRaises(ValueError):
            verified_installed_runtime("/tmp/work_external_plan_override.py")
        args = [
            "operator",
            "--config", "/owned/config.json",
            "--project", "homelab",
            "--workflow", str(uuid4()),
            "--root-task", str(uuid4()),
            "--source-review", str(uuid4()),
            "--plan-assignment", str(uuid4()),
            "--plan-sha256", "d" * 64,
            "--expected-workflow-revision", "2",
            "--expected-root-revision", "2",
            "--idempotency-key", str(uuid4()),
            "--reason", "User accepted.",
            "--acceptance-ref", "ref-1",
            "--confirm-accept-as-is",
            "--execute",
        ]
        with patch.object(override_mod.os, "geteuid", return_value=1000), patch("sys.argv", args):
            with self.assertRaises(RuntimeError):
                override_mod.main()

    def test_factory_validates_plan_override_grant_provider_type(self):
        with self.assertRaises(TypeError):
            ExternalWorkProviderFactory(fixture.cfg(), plan_override_grant_provider=object())
        with self.assertRaises(TypeError):
            PlanOverrideGrantScope("not-callable")


@unittest.skipUnless(PG16_AVAILABLE, "Requires owned PostgreSQL16")
class ExternalPlanApproveOverridePGTest(unittest.TestCase):
    call = fixture.ExternalFlowPGTest.call
    rows = fixture.ExternalFlowPGTest.rows
    review = fixture.ExternalFlowPGTest.review
    author = fixture.ExternalFlowPGTest.author
    prepared = fixture.ExternalFlowPGTest.prepared

    @classmethod
    def setUpClass(cls):
        fixture.ExternalFlowPGTest.setUpClass.__func__(cls)

    @classmethod
    def connect(cls):
        return cls.pg.connect("forge_agent")

    def setUp(self):
        fixture.ExternalFlowPGTest.setUp(self)
        self.pg.reset_retry_events()

    def write(self, sql, args=()):
        with closing(self.pg.connect("postgres")) as connection:
            with connection, connection.cursor() as cursor:
                cursor.execute(sql, args)

    def public_service(self, config=None):
        return WorkServiceFactory(config or fixture.cfg(), self.connect).create()

    def recovery_only_service(self, config=None):
        grants = NativeReviewerGrantScope(lambda: self.pg.connect("postgres"))
        return WorkServiceFactory(
            config or fixture.cfg(),
            self.connect,
            external_factory=lambda cfg: ExternalWorkProviderFactory(
                cfg, recovery_grant_provider=grants
            ).create(),
        ).create()

    def override_service(self, config=None):
        grants = PlanOverrideGrantScope(lambda: self.pg.connect("postgres"))
        return WorkServiceFactory(
            config or fixture.cfg(),
            self.connect,
            external_factory=lambda cfg: ExternalWorkProviderFactory(
                cfg, plan_override_grant_provider=grants
            ).create(),
        ).create()

    def submit_m31_plan(self, content="# Plan v0\nInitial complete plan."):
        service = self.public_service()
        command = replace(
            make_submit(agent_profile=fixture.ALIAS, mode="plan", idempotency_key=str(uuid4())),
            workflow_policy_digest=fixture.cfg().external_workflows["homelab"].summary(
                fixture.cfg().catalog()
            )["policy_digest"],
        )
        root = service.submit(command)
        self.call("advertise", "homelab", fixture.ALIAS, "owned-muse")
        job = self.call("claim", "homelab", fixture.ALIAS)
        self.call("finish", job["assignment_id"], job["generation"], job["token"], content, "Plan ready")
        flow = service._external_workflow
        flow.run_once()
        action = self.delivery.claim("owned-delivery")
        self.assertEqual(action.kind, "source")
        self.delivery.checkpoint(
            action,
            {"status": "passed", "receipt": {"base_revision": "a" * 40}, "summary": "Exact source"},
        )
        flow.run_once()
        return service, root, flow, job

    def exhaust_m31_plan_revisions(self):
        service, root, flow, job = self.submit_m31_plan()
        last_job = job
        last_content = "# Plan v0\nInitial complete plan."
        for round_num in range(1, AGENT_RETRY_LIMIT + 1):
            self.review(flow, "revise", summary=f"Revise round {round_num}.")
            last_content = f"# Plan revision {round_num}\nPostgres proxy design accepted as-is."
            last_job = self.author(flow, last_content)
        # 21st revise verdict exhausts the 20-attempt planning retry event.
        self.review(flow, "revise", summary="Revise round 21 triggers exhaustion.")
        w = self.rows("SELECT * FROM forge_external_workflow.workflows")[0]
        event = self.rows(
            "SELECT * FROM forge_agent.coding_retry_events "
            "WHERE scope_kind='external_workflow' AND scope_id=%s AND kind='planning'",
            (w["workflow_id"],),
        )[0]
        self.assertEqual((w["phase"], w["enabled"], w["plan_revisions"], w["code_revisions"]), ("escalated", False, 20, 0))
        self.assertEqual((event["used"], event["attempt_limit"], event["status"]), (20, 20, "exhausted"))
        return service, root, flow, last_job, last_content, w, event

    def exhaust_legacy_plan_revisions(self):
        old_config = replace(
            fixture.cfg(),
            external_workflows={
                "homelab": {
                    "author_profile": fixture.ALIAS,
                    "reviewer_profile": "codex-v2",
                    "max_plan_revisions": 3,
                    "max_code_revisions": 3,
                }
            },
        )
        service = WorkServiceFactory(
            old_config,
            self.connect,
            retry_event_factory=lambda _: SimpleNamespace(create=lambda: None),
        ).create()
        command = replace(
            make_submit(agent_profile=fixture.ALIAS, mode="plan", idempotency_key=str(uuid4())),
            workflow_policy_digest=old_config.external_workflows["homelab"].summary(
                old_config.catalog()
            )["policy_digest"],
        )
        root = service.submit(command)
        self.call("advertise", "homelab", fixture.ALIAS, "owned-muse")
        job = self.call("claim", "homelab", fixture.ALIAS)
        self.call("finish", job["assignment_id"], job["generation"], job["token"], "Legacy plan 0", "Plan ready")
        flow = service._external_workflow
        flow.run_once()
        action = self.delivery.claim("owned-delivery")
        self.delivery.checkpoint(
            action,
            {"status": "passed", "receipt": {"base_revision": "a" * 40}, "summary": "Exact source"},
        )
        flow.run_once()
        last_job = job
        last_content = "Legacy plan 0"
        for idx in range(1, 4):
            self.review(flow, "revise", summary=f"Legacy revise {idx}")
            last_content = f"Legacy plan revision {idx}"
            last_job = self.author(flow, last_content)
        self.review(flow, "revise", summary="Legacy revise 4 exhausts budget")
        w = self.rows("SELECT * FROM forge_external_workflow.workflows")[0]
        self.assertEqual((w["phase"], w["enabled"], w["plan_revisions"], w["code_revisions"]), ("escalated", False, 3, 0))
        return root, last_job, last_content, w

    def override_command(self, root, w, plan_job, plan_content, **changes):
        root_row = self.rows("SELECT revision FROM forge_agent.coding_tasks WHERE id=%s", (root["task_id"],))[0]
        values = dict(
            project_alias="homelab",
            workflow_id=w["workflow_id"],
            root_task_id=UUID(root["task_id"]),
            source_review_id=UUID(w["state"]["active_review_id"]),
            plan_assignment_id=UUID(plan_job["assignment_id"]),
            plan_sha256=hashlib.sha256(plan_content.encode("utf-8")).hexdigest(),
            expected_workflow_revision=w["revision"],
            expected_root_revision=root_row["revision"],
            idempotency_key=uuid4(),
            reason="User accepted the Postgres proxy plan as-is after 20 revise rounds.",
            acceptance_ref="owner-decision:8c300cff",
            confirm_accept_as_is=True,
        )
        values.update(changes)
        return ExternalPlanApproveOverride(**values)

    def test_m31_twenty_revise_exhaustion_override_and_full_downstream_delivery_lifecycle(self):
        pub_service, root, flow, last_job, last_content, before_w, before_event = self.exhaust_m31_plan_revisions()
        plan_sha = hashlib.sha256(last_content.encode("utf-8")).hexdigest()

        # Snapshot pre-override state that MUST remain immutable / byte-identical.
        old_reviews = self.rows("SELECT * FROM forge_external_workflow.reviews ORDER BY review_id")
        old_retry_events = self.rows("SELECT * FROM forge_agent.coding_retry_events ORDER BY event_id")
        old_admissions = self.rows("SELECT * FROM forge_agent.coding_retry_event_admissions ORDER BY event_id, ordinal")
        old_observations = self.rows("SELECT * FROM forge_agent.coding_retry_event_observations ORDER BY created_at, source_id")
        old_artifacts = self.rows("SELECT * FROM forge_external_agent.artifacts ORDER BY assignment_id")
        old_events = self.rows("SELECT * FROM forge_agent.coding_events WHERE task_id=%s ORDER BY sequence", (root["task_id"],))
        old_children = self.rows(
            "SELECT t.id, t.status, t.revision FROM forge_agent.coding_tasks t "
            "JOIN forge_external_workflow.tasks x ON (x.owner_id,x.project_alias,x.task_id)=(t.owner_id,t.project_alias,t.id) "
            "WHERE t.id<>%s ORDER BY t.id",
            (root["task_id"],),
        )

        command = self.override_command(root, before_w, last_job, last_content)

        # Normal HTTP composition and recovery-only composition MUST refuse the override.
        with self.assertRaises(Rejected) as ctx:
            pub_service.approve_external_plan_override(command)
        self.assertEqual((ctx.exception.status, ctx.exception.code), (403, "denied"))
        with self.assertRaises(Rejected) as ctx:
            self.recovery_only_service().approve_external_plan_override(command)
        self.assertEqual((ctx.exception.status, ctx.exception.code), (403, "denied"))

        # Manual control paths remain forbidden on external workflows.
        root_rev = self.rows("SELECT revision FROM forge_agent.coding_tasks WHERE id=%s", (root["task_id"],))[0]["revision"]
        with self.assertRaises(Rejected) as ctx:
            pub_service.accept(WorkAccept("homelab", UUID(root["task_id"]), root_rev, "manual-accept-blocked", "no"))
        self.assertEqual((ctx.exception.status, ctx.exception.code), (409, "not_reviewable"))

        # Authorized private operator override succeeds.
        op_service = self.override_service()
        receipt = op_service.approve_external_plan_override(command)
        self.assertEqual(receipt["status"], "implement_queued")
        self.assertEqual(receipt["prior_phase"], "escalated")
        self.assertEqual(receipt["phase"], "implement")
        self.assertEqual(receipt["workflow_id"], str(before_w["workflow_id"]))
        self.assertEqual(receipt["root_task_id"], root["task_id"])
        self.assertEqual(receipt["plan_assignment_id"], last_job["assignment_id"])
        self.assertEqual(receipt["plan_sha256"], plan_sha)
        self.assertEqual(receipt["prior_review_id"], before_w["state"]["active_review_id"])
        self.assertEqual(receipt["prior_plan_revisions"], 20)
        self.assertEqual(receipt["retry_event_id"], str(before_event["event_id"]))
        self.assertEqual(receipt["actor"], {"kind": "operator", "id": "test-owner"})
        self.assertEqual(receipt["reason"], command.reason)
        self.assertEqual(receipt["acceptance_ref"], command.acceptance_ref)
        self.assertEqual(receipt["workflow_revision"], before_w["revision"] + 1)
        self.assertEqual(receipt["root_revision"], root_rev + 1)

        # Idempotent replay returns exact receipt; conflicting intent -> 409 idempotency_conflict; new key -> 409 not_reviewable.
        self.assertEqual(op_service.approve_external_plan_override(command), receipt)
        with self.assertRaises(Rejected) as ctx:
            op_service.approve_external_plan_override(replace(command, reason="Conflicting replay reason"))
        self.assertEqual((ctx.exception.status, ctx.exception.code), (409, "idempotency_conflict"))
        with self.assertRaises(Rejected) as ctx:
            op_service.approve_external_plan_override(replace(command, idempotency_key=uuid4()))
        self.assertEqual((ctx.exception.status, ctx.exception.code), (409, "not_reviewable"))

        # Verify immutable audit table row.
        audit_rows = self.rows("SELECT * FROM forge_external_workflow.plan_approve_overrides")
        self.assertEqual(len(audit_rows), 1)
        audit = audit_rows[0]
        self.assertEqual(str(audit["override_id"]), receipt["override_id"])
        self.assertEqual((audit["actor_kind"], audit["actor_id"]), ("operator", "test-owner"))
        self.assertEqual(audit["reason"], command.reason)
        self.assertEqual(audit["acceptance_ref"], command.acceptance_ref)
        self.assertEqual(str(audit["plan_assignment_id"]), last_job["assignment_id"])
        self.assertEqual(audit["plan_sha256"], plan_sha)
        self.assertEqual(audit["prior_phase"], "escalated")
        self.assertEqual(audit["prior_plan_revisions"], 20)
        self.assertEqual(str(audit["prior_review_id"]), before_w["state"]["active_review_id"])
        self.assertEqual(audit["retry_event_id"], before_event["event_id"])
        self.assertEqual(str(audit["implement_task_id"]), receipt["implement_task_id"])
        self.assertEqual(str(audit["implement_assignment_id"]), receipt["implement_assignment_id"])
        self.assertEqual(audit["receipt"], receipt)

        # Verify history preservation: reviews, retry events, admissions, observations, artifacts, old children untouched.
        self.assertEqual(old_reviews, self.rows("SELECT * FROM forge_external_workflow.reviews ORDER BY review_id"))
        self.assertEqual(old_retry_events, self.rows("SELECT * FROM forge_agent.coding_retry_events ORDER BY event_id"))
        self.assertEqual(old_admissions, self.rows("SELECT * FROM forge_agent.coding_retry_event_admissions ORDER BY event_id, ordinal"))
        self.assertEqual(old_observations, self.rows("SELECT * FROM forge_agent.coding_retry_event_observations ORDER BY created_at, source_id"))
        self.assertEqual(old_artifacts, self.rows("SELECT * FROM forge_external_agent.artifacts ORDER BY assignment_id"))
        self.assertEqual(
            old_children,
            self.rows(
                "SELECT id, status, revision FROM forge_agent.coding_tasks WHERE id = ANY(%s) ORDER BY id",
                ([r["id"] for r in old_children],),
            ),
        )

        # Verify workflow state diff.
        after_w = self.rows("SELECT * FROM forge_external_workflow.workflows")[0]
        self.assertEqual((after_w["phase"], after_w["enabled"]), ("implement", True))
        self.assertEqual((after_w["plan_revisions"], after_w["code_revisions"]), (20, 0))
        self.assertEqual((after_w["snapshot"], after_w["policy_digest"]), (before_w["snapshot"], before_w["policy_digest"]))
        self.assertEqual(str(after_w["active_task_id"]), receipt["implement_task_id"])
        self.assertNotIn("active_review_id", after_w["state"])
        self.assertNotIn("escalation_reason", after_w["state"])
        self.assertEqual(
            after_w["state"]["approved_plan"],
            {"assignment_id": last_job["assignment_id"], "sha256": plan_sha, "content": last_content},
        )
        self.assertEqual(after_w["state"]["plan_review"]["decision"], "user_override_approve")
        self.assertEqual(after_w["state"]["plan_review"]["override_id"], receipt["override_id"])
        self.assertEqual(after_w["state"]["plan_review"]["prior_decision"], "revise")
        self.assertEqual(after_w["state"]["plan_review"]["prior_review_id"], before_w["state"]["active_review_id"])

        # Verify additive Event N+1 (operator) and Event N+2 (coordinator) on root_task_id.
        new_events = self.rows(
            "SELECT * FROM forge_agent.coding_events WHERE task_id=%s ORDER BY sequence",
            (root["task_id"],),
        )
        self.assertEqual(new_events[: len(old_events)], old_events)
        self.assertEqual(len(new_events), len(old_events) + 2)
        ev_op, ev_coord = new_events[-2], new_events[-1]
        self.assertEqual(ev_op["correlation_id"], old_events[0]["correlation_id"])
        self.assertEqual(ev_coord["correlation_id"], old_events[0]["correlation_id"])
        self.assertEqual(ev_op["sequence"], old_events[-1]["sequence"] + 1)
        self.assertEqual(ev_coord["sequence"], old_events[-1]["sequence"] + 2)
        self.assertEqual(ev_op["event_kind"], "checkpoint")
        self.assertEqual(ev_op["payload"]["actor"], {"kind": "operator", "id": "test-owner"})
        self.assertEqual(ev_op["payload"]["stage"], "plan_approve_override")
        self.assertEqual(ev_op["payload"]["override_id"], receipt["override_id"])
        self.assertEqual(ev_op["payload"]["plan_sha256"], plan_sha)
        self.assertEqual(ev_op["payload"]["reason"], command.reason)
        self.assertEqual(ev_op["payload"]["acceptance_ref"], command.acceptance_ref)
        self.assertEqual(ev_op["payload"]["implement_task_id"], receipt["implement_task_id"])
        self.assertEqual(ev_coord["event_kind"], "checkpoint")
        self.assertEqual(ev_coord["payload"]["actor"], {"kind": "coordinator", "id": "forge-coordinator"})
        self.assertEqual(ev_coord["payload"]["phase"], "implement")
        self.assertEqual(ev_coord["payload"]["related_task_id"], receipt["implement_task_id"])
        self.assertIn(receipt["override_id"], ev_coord["payload"]["summary"])
        self.assertIn(plan_sha[:12], ev_coord["payload"]["summary"])

        # Public events projection stays strictly DTO-compatible and projects Event N+2 as workflow_progress.
        projected = pub_service.events(WorkEvents("homelab", UUID(root["task_id"]), old_events[-1]["sequence"], 50))
        self.assertEqual(len(projected["events"]), 2)
        self.assertNotIn("workflow_progress", projected["events"][0])
        self.assertEqual(projected["events"][1]["workflow_progress"]["phase"], "implement")
        self.assertEqual(projected["events"][1]["workflow_progress"]["related_task_id"], receipt["implement_task_id"])

        # Muse claims the queued implement assignment with exact approved_plan context, then full downstream pipeline completes!
        manifest = canonical({
            "version": 1,
            "candidate_commit": "b" * 40,
            "candidate_tree": "d" * 40,
            "base_revision": "a" * 40,
            "summary": "Implemented accepted Postgres proxy plan.",
        }).decode()
        impl_job = self.call("claim", "homelab", fixture.ALIAS)
        self.assertIsNotNone(impl_job)
        self.assertEqual(impl_job["assignment_id"], receipt["implement_assignment_id"])
        self.assertEqual(impl_job["task_id"], receipt["implement_task_id"])
        self.assertEqual(impl_job["external_context"]["stage"], "implement")
        self.assertEqual(impl_job["external_context"]["approved_plan"], after_w["state"]["approved_plan"])
        self.assertEqual(impl_job["external_context"]["base_revision"], "a" * 40)
        self.assertIsNone(impl_job["external_context"]["feedback"])
        self.assertEqual(
            hashlib.sha256(impl_job["external_context_json"].encode("utf-8")).hexdigest(),
            impl_job["external_context_sha256"],
        )
        self.call("finish", impl_job["assignment_id"], impl_job["generation"], impl_job["token"], manifest, "Candidate ready")

        # Run downstream prepare -> independent Codex code_review -> Jenkins verify -> merge.
        v2_flow = WorkServiceFactory(
            fixture.cfg(),
            self.connect,
            retry_event_factory=lambda _: SimpleNamespace(create=lambda: None),
        ).create()._external_workflow
        self.assertTrue(v2_flow.run_once())
        candidate = self.prepared(v2_flow)
        code_target = self.review(v2_flow, "approve", summary="Independent code review approved.")
        self.assertEqual(code_target["target_kind"], "code")
        self.assertEqual(code_target["approved_plan"], after_w["state"]["approved_plan"])
        self.assertTrue(v2_flow.run_once())

        verify_action = self.delivery.claim("owned-delivery")
        self.assertEqual(verify_action.kind, "verify")
        self.assertEqual(verify_action.request["plan_review"]["decision"], "user_override_approve")
        self.assertEqual(verify_action.request["plan_review"]["override_id"], receipt["override_id"])
        verified = {k: candidate[k] for k in ("candidate_commit", "candidate_tree", "gate_manifest_sha256", "branch_scope")}
        verified.update(result="SUCCESS", build_id=99)
        self.delivery.checkpoint(verify_action, {"status": "passed", "receipt": verified, "summary": "Checks passed"})
        self.assertTrue(v2_flow.run_once())
        self.assertTrue(v2_flow.run_once())

        merge_action = self.delivery.claim("owned-delivery")
        self.assertEqual(merge_action.kind, "merge")
        self.assertEqual(merge_action.request["plan_review"]["decision"], "user_override_approve")
        self.assertEqual(merge_action.request["plan_review"]["override_id"], receipt["override_id"])
        self.delivery.checkpoint(
            merge_action,
            {
                "status": "merged",
                "receipt": {"merge_commit": "c" * 40, "candidate_tree": "d" * 40, "expected_main": "a" * 40},
                "summary": "Merged on main",
            },
        )
        self.assertTrue(v2_flow.run_once())
        final_w = self.rows("SELECT * FROM forge_external_workflow.workflows")[0]
        self.assertEqual((final_w["phase"], final_w["enabled"]), ("completed", False))
        self.assertEqual(pub_service.get(WorkGet("homelab", UUID(root["task_id"])))["status"], "completed")
        # Planning retry event stays permanently exhausted with all 20 admissions intact.
        self.assertEqual(old_retry_events, self.rows("SELECT * FROM forge_agent.coding_retry_events ORDER BY event_id"))
        # Replay after completion still returns the exact stored receipt.
        self.assertEqual(op_service.approve_external_plan_override(command), receipt)

    def test_legacy_pre_m31_plan_budget_exhaustion_override_succeeds(self):
        root, last_job, last_content, w = self.exhaust_legacy_plan_revisions()
        self.assertEqual(self.rows("SELECT count(*) AS n FROM forge_agent.coding_retry_events")[0]["n"], 0)
        command = self.override_command(root, w, last_job, last_content)
        receipt = self.override_service().approve_external_plan_override(command)
        self.assertEqual(receipt["status"], "implement_queued")
        self.assertIsNone(receipt["retry_event_id"])
        self.assertEqual(receipt["prior_plan_revisions"], 3)
        audit = self.rows("SELECT * FROM forge_external_workflow.plan_approve_overrides")[0]
        self.assertIsNone(audit["retry_event_id"])
        impl_job = self.call("claim", "homelab", fixture.ALIAS)
        self.assertIsNotNone(impl_job)
        self.assertEqual(impl_job["external_context"]["approved_plan"]["content"], last_content)

    def test_adopted_legacy_to_m31_exhaustion_override_succeeds(self):
        root, _, _, w = self.exhaust_legacy_plan_revisions()
        cont_grants = NativeReviewerGrantScope(lambda: self.pg.connect("postgres"))
        cont_flow = WorkServiceFactory(
            fixture.cfg(),
            self.connect,
            external_factory=lambda cfg: ExternalWorkProviderFactory(
                cfg, recovery_grant_provider=cont_grants
            ).create(),
        ).create()._external_workflow
        root_rev = self.rows("SELECT revision FROM forge_agent.coding_tasks WHERE id=%s", (root["task_id"],))[0]["revision"]
        cont_flow.continue_after_retry_policy_update(
            ExternalRetryPolicyContinuation(
                project_alias="homelab",
                workflow_id=w["workflow_id"],
                root_task_id=UUID(root["task_id"]),
                source_review_id=UUID(w["state"]["active_review_id"]),
                expected_workflow_revision=w["revision"],
                expected_root_revision=root_rev,
                idempotency_key=uuid4(),
                reason="Continue legacy workflow under 20-attempt policy.",
            )
        )
        last_content = "Adopted plan revision 4"
        last_job = self.author(cont_flow, last_content)
        for num in range(5, AGENT_RETRY_LIMIT + 1):
            self.review(cont_flow, "revise", summary=f"Adopted revise {num}")
            last_content = f"Adopted plan revision {num}"
            last_job = self.author(cont_flow, last_content)
        self.review(cont_flow, "revise", summary="Adopted revise 21 exhausts")
        exhausted_w = self.rows("SELECT * FROM forge_external_workflow.workflows")[0]
        event = self.rows("SELECT * FROM forge_agent.coding_retry_events WHERE kind='planning'")[0]
        self.assertEqual((event["legacy_used"], event["used"], event["status"]), (3, 20, "exhausted"))
        receipt = self.override_service().approve_external_plan_override(
            self.override_command(root, exhausted_w, last_job, last_content)
        )
        self.assertEqual(receipt["retry_event_id"], str(event["event_id"]))
        self.assertEqual(receipt["phase"], "implement")

    def test_concurrent_raced_override_replays_produce_single_audit_and_assignment(self):
        _, root, _, last_job, last_content, w, _ = self.exhaust_m31_plan_revisions()
        service = self.override_service()
        command = self.override_command(root, w, last_job, last_content)
        barrier = Barrier(2)

        def run(_):
            barrier.wait()
            return service.approve_external_plan_override(command)

        with ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(run, range(2)))
        self.assertEqual(results[0], results[1])
        self.assertEqual(self.rows("SELECT count(*) AS n FROM forge_external_workflow.plan_approve_overrides")[0]["n"], 1)
        self.assertEqual(self.rows("SELECT count(*) AS n FROM forge_external_agent.assignments WHERE status='queued'")[0]["n"], 1)

    def test_forbidden_cases_rejected_without_state_change(self):
        # 1. Non-escalated phase (plan_review).
        _, root, _, job = self.submit_m31_plan("# In-progress plan")
        w = self.rows("SELECT * FROM forge_external_workflow.workflows")[0]
        cmd = self.override_command(root, w, job, "# In-progress plan")
        with self.assertRaises(Rejected) as ctx:
            self.override_service().approve_external_plan_override(cmd)
        self.assertEqual((ctx.exception.status, ctx.exception.code), (409, "not_reviewable"))
        self.assertEqual(self.rows("SELECT count(*) AS n FROM forge_external_workflow.plan_approve_overrides")[0]["n"], 0)

        # 2. Reviewer 'escalate' verdict.
        self.setUp()
        _, root, flow, job = self.submit_m31_plan("# Escalated plan")
        self.review(flow, "escalate", summary="Reviewer explicitly escalated this plan.")
        w = self.rows("SELECT * FROM forge_external_workflow.workflows")[0]
        with self.assertRaises(Rejected) as ctx:
            self.override_service().approve_external_plan_override(self.override_command(root, w, job, "# Escalated plan"))
        self.assertEqual((ctx.exception.status, ctx.exception.code), (409, "not_reviewable"))

        # 3. Reviewer-correction malformed-output exhaustion.
        self.setUp()
        _, root, flow, job = self.submit_m31_plan("# Reviewer correction exhausted plan")
        for _ in range(AGENT_RETRY_LIMIT + 1):
            lease = self.db.claim("review-worker", self.inc, ["codex-v2"])
            proof = make_stop_proof(lease.unit_name)
            self.db.publish(lease, "failed", None, None, "The coding agent returned malformed machine output.", proof)
            self.db.reconcile(lease, "b" * 64, proof)
            flow.run_once()
        w = self.rows("SELECT * FROM forge_external_workflow.workflows")[0]
        self.assertEqual(w["phase"], "escalated")
        with self.assertRaises(Rejected) as ctx:
            self.override_service().approve_external_plan_override(
                self.override_command(root, w, job, "# Reviewer correction exhausted plan")
            )
        self.assertEqual((ctx.exception.status, ctx.exception.code), (409, "not_reviewable"))

        # 4. Mutations of an otherwise valid M31 exhausted workflow.
        # Stage one genuine 20-round exhausted workflow and verify each
        # forbidden condition rejects without altering workflow/audit state.
        self.setUp()
        _, root, _, last_job, last_content, w, event = self.exhaust_m31_plan_revisions()
        base_command = self.override_command(root, w, last_job, last_content)
        review_source_id = "review:" + w["state"]["active_review_id"]
        orig_obs_digest = self.rows(
            "SELECT source_digest FROM forge_agent.coding_retry_event_observations "
            "WHERE event_id=%s AND source_id=%s AND observation_kind='failure'",
            (event["event_id"], review_source_id),
        )[0]["source_digest"]
        child_author = self.rows(
            "SELECT t.id, t.cancelled_at FROM forge_agent.coding_tasks t "
            "JOIN forge_external_workflow.tasks x ON (x.owner_id,x.project_alias,x.task_id)=(t.owner_id,t.project_alias,t.id) "
            "WHERE x.workflow_id=%s AND x.role='author' AND t.id<>%s LIMIT 1",
            (w["workflow_id"], w["active_task_id"]),
        )[0]
        some_attempt = self.rows(
            "SELECT a.attempt_id FROM forge_agent.coding_attempts a "
            "JOIN forge_external_workflow.tasks t ON (t.owner_id,t.project_alias,t.task_id)=(a.owner_id,a.project_alias,a.task_id) "
            "WHERE t.workflow_id=%s LIMIT 1",
            (w["workflow_id"],),
        )[0]["attempt_id"]

        cases = (
            ("mismatched_plan_sha256", 409),
            ("mismatched_plan_assignment_id", 409),
            ("mismatched_source_review_id", 409),
            ("stale_workflow_revision", 409),
            ("stale_root_revision", 409),
            ("disabled_reviewer_profile", 403),
            ("drifted_policy_model", 403),
            ("user_cancelled", 409),
            ("code_stage_escalation", 409),
            ("unexhausted_planning_event", 409),
            ("mismatched_observation_digest", 409),
            ("revoked_scope_grant", 403),
            ("missing_worker_grant", 403),
            ("retained_workflow_lease", 409),
            ("active_delivery_action", 409),
            ("active_child_task", 409),
            ("active_non_stopped_attempt", 409),
            ("missing_admission_row", 409),
        )
        for case, expected_status in cases:
            with self.subTest(case=case):
                service = self.override_service()
                command = base_command
                undo = []
                if case == "user_cancelled":
                    cancel_key = str(uuid4())
                    self.write(
                        "INSERT INTO forge_agent.coding_control_receipts"
                        "(owner_id,project_alias,task_id,action,idempotency_key,intent_digest,base_revision,result_revision,receipt_json) "
                        "VALUES('test-owner','homelab',%s,'cancel',%s,%s,1,2,%s)",
                        (root["task_id"], cancel_key, "0" * 64, Json({"status": "cancelled"})),
                    )
                    undo.append(("DELETE FROM forge_agent.coding_control_receipts WHERE idempotency_key=%s", (cancel_key,)))
                elif case == "code_stage_escalation":
                    self.write(
                        "UPDATE forge_external_workflow.workflows SET code_revisions=1 WHERE workflow_id=%s",
                        (w["workflow_id"],),
                    )
                    undo.append(("UPDATE forge_external_workflow.workflows SET code_revisions=0 WHERE workflow_id=%s", (w["workflow_id"],)))
                elif case == "unexhausted_planning_event":
                    self.write(
                        "UPDATE forge_agent.coding_retry_events SET used=19, status='open' WHERE event_id=%s",
                        (event["event_id"],),
                    )
                    undo.append(("UPDATE forge_agent.coding_retry_events SET used=20, status='exhausted' WHERE event_id=%s", (event["event_id"],)))
                elif case == "missing_admission_row":
                    self.write(
                        "DELETE FROM forge_agent.coding_retry_event_admissions WHERE event_id=%s AND ordinal=20",
                        (event["event_id"],),
                    )
                elif case == "mismatched_observation_digest":
                    self.write(
                        "UPDATE forge_agent.coding_retry_event_observations SET source_digest=%s "
                        "WHERE event_id=%s AND source_id=%s",
                        ("0" * 64, event["event_id"], review_source_id),
                    )
                    undo.append((
                        "UPDATE forge_agent.coding_retry_event_observations SET source_digest=%s "
                        "WHERE event_id=%s AND source_id=%s",
                        (orig_obs_digest, event["event_id"], review_source_id),
                    ))
                elif case == "mismatched_plan_sha256":
                    command = replace(command, plan_sha256="0" * 64)
                elif case == "mismatched_plan_assignment_id":
                    command = replace(command, plan_assignment_id=uuid4())
                elif case == "mismatched_source_review_id":
                    command = replace(command, source_review_id=uuid4())
                elif case == "stale_workflow_revision":
                    command = replace(command, expected_workflow_revision=command.expected_workflow_revision + 1)
                elif case == "stale_root_revision":
                    command = replace(command, expected_root_revision=command.expected_root_revision + 1)
                elif case == "revoked_scope_grant":
                    self.write("UPDATE forge_external_agent.scope_grants SET enabled=false")
                    undo.append(("UPDATE forge_external_agent.scope_grants SET enabled=true", ()))
                elif case == "missing_worker_grant":
                    self.write("DELETE FROM forge_agent.coding_worker_grants")
                    undo.append(("GRANT_WORKER", ()))
                elif case == "disabled_reviewer_profile":
                    base = fixture.cfg()
                    service._external_workflow.config = SimpleNamespace(
                        profiles={
                            **base.profiles,
                            "codex-v2": replace(base.profiles["codex-v2"], execution_enabled=False),
                        }
                    )
                elif case == "drifted_policy_model":
                    base = fixture.cfg()
                    drifted = replace(
                        base,
                        profiles={**base.profiles, "codex-v2": replace(base.profiles["codex-v2"], model="other-model")},
                    )
                    service = self.override_service(drifted)
                elif case == "retained_workflow_lease":
                    self.write(
                        "UPDATE forge_external_workflow.workflows "
                        "SET lease_token=gen_random_uuid(), lease_until=clock_timestamp()+interval '1 hour' "
                        "WHERE workflow_id=%s",
                        (w["workflow_id"],),
                    )
                    undo.append((
                        "UPDATE forge_external_workflow.workflows SET lease_token=NULL, lease_until=NULL WHERE workflow_id=%s",
                        (w["workflow_id"],),
                    ))
                elif case == "active_delivery_action":
                    self.write(
                        "INSERT INTO forge_external_workflow.actions"
                        "(workflow_id,owner_id,project_alias,workflow_revision,kind,source_key,intent,status) "
                        "VALUES(%s,'test-owner','homelab',%s,'source','stale-check',%s,'queued')",
                        (w["workflow_id"], w["revision"], Json({"kind": "source"})),
                    )
                    undo.append((
                        "UPDATE forge_external_workflow.actions SET status='failed' WHERE workflow_id=%s AND source_key='stale-check'",
                        (w["workflow_id"],),
                    ))
                elif case == "active_child_task":
                    self.write(
                        "UPDATE forge_agent.coding_tasks SET status='queued', cancelled_at=NULL WHERE id=%s",
                        (child_author["id"],),
                    )
                    undo.append((
                        "UPDATE forge_agent.coding_tasks SET status='cancelled', cancelled_at=%s WHERE id=%s",
                        (child_author["cancelled_at"], child_author["id"]),
                    ))
                elif case == "active_non_stopped_attempt":
                    self.write(
                        "UPDATE forge_agent.coding_attempts SET phase='running', reservation_active=true WHERE attempt_id=%s",
                        (some_attempt,),
                    )
                    undo.append((
                        "UPDATE forge_agent.coding_attempts SET phase='stopped', reservation_active=false WHERE attempt_id=%s",
                        (some_attempt,),
                    ))

                try:
                    before = self.rows("SELECT phase, enabled, revision, state FROM forge_external_workflow.workflows")[0]
                    with self.assertRaises(Rejected) as ctx:
                        service.approve_external_plan_override(command)
                    self.assertEqual(ctx.exception.status, expected_status)
                    self.assertEqual(
                        before,
                        self.rows("SELECT phase, enabled, revision, state FROM forge_external_workflow.workflows")[0],
                    )
                    self.assertEqual(self.rows("SELECT count(*) AS n FROM forge_external_workflow.plan_approve_overrides")[0]["n"], 0)
                    self.assertEqual(self.rows("SELECT count(*) AS n FROM forge_external_agent.assignments WHERE status='queued'")[0]["n"], 0)
                finally:
                    for sql, params in reversed(undo):
                        if sql == "GRANT_WORKER":
                            self.pg.grant_worker("test-owner", "homelab", "codex-v2")
                        else:
                            self.write(sql, params)

    def test_audit_immutability_unique_constraint_and_role_privileges(self):
        _, root, _, last_job, last_content, w, _ = self.exhaust_m31_plan_revisions()
        command = self.override_command(root, w, last_job, last_content)
        receipt = self.override_service().approve_external_plan_override(command)

        # UPDATE and DELETE raise P0009 even as superuser / table owner.
        for stmt in (
            "UPDATE forge_external_workflow.plan_approve_overrides SET reason='tampered'",
            "DELETE FROM forge_external_workflow.plan_approve_overrides",
        ):
            with closing(self.pg.connect("postgres")) as conn:
                with self.assertRaises(psycopg2.Error) as ctx:
                    with conn, conn.cursor() as cur:
                        cur.execute(stmt)
                self.assertEqual(ctx.exception.pgcode, "P0009")

        # forge_agent has SELECT/INSERT only (no UPDATE/DELETE); duplicate workflow_id hits UNIQUE(workflow_id).
        for stmt in (
            "UPDATE forge_external_workflow.plan_approve_overrides SET reason='tampered'",
            "DELETE FROM forge_external_workflow.plan_approve_overrides",
        ):
            with closing(self.pg.connect("forge_agent")) as conn:
                with self.assertRaises(psycopg2.errors.InsufficientPrivilege):
                    with conn, conn.cursor() as cur:
                        cur.execute(stmt)

        audit = self.rows("SELECT * FROM forge_external_workflow.plan_approve_overrides")[0]
        with closing(self.pg.connect("forge_agent")) as conn:
            with self.assertRaises(psycopg2.errors.UniqueViolation):
                with conn, conn.cursor() as cur:
                    cur.execute(
                        """INSERT INTO forge_external_workflow.plan_approve_overrides(
                               owner_id, project_alias, workflow_id, root_task_id,
                               actor_kind, actor_id, reason, acceptance_ref,
                               plan_task_id, plan_assignment_id, plan_sha256,
                               prior_workflow_revision, prior_root_revision, prior_phase,
                               prior_plan_revisions, prior_review_id, prior_escalation_reason,
                               retry_event_id, idempotency_key, intent_digest,
                               implement_task_id, implement_assignment_id, receipt
                           ) VALUES (
                               %s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s
                           )""",
                        (
                            audit["owner_id"], audit["project_alias"], audit["workflow_id"], audit["root_task_id"],
                            "operator", "test-owner", "second override", "ref-2",
                            audit["plan_task_id"], audit["plan_assignment_id"], audit["plan_sha256"],
                            audit["prior_workflow_revision"], audit["prior_root_revision"], "escalated",
                            audit["prior_plan_revisions"], audit["prior_review_id"], audit["prior_escalation_reason"],
                            audit["retry_event_id"], str(uuid4()), "f" * 64,
                            audit["implement_task_id"], audit["implement_assignment_id"], Json(receipt),
                        ),
                    )

        # muse_events, forge_coding_worker, and forge_coding_reconciler have ZERO privileges on plan_approve_overrides.
        for role in ("muse_events", "forge_coding_worker", "forge_coding_reconciler"):
            for priv in ("SELECT", "INSERT", "UPDATE", "DELETE"):
                allowed = self.rows(
                    "SELECT has_table_privilege(%s, 'forge_external_workflow.plan_approve_overrides', %s) AS ok",
                    (role, priv),
                )[0]["ok"]
                self.assertFalse(allowed, f"Role {role} unexpectedly has {priv} on plan_approve_overrides")
            with closing(self.pg.connect(role)) as conn:
                with self.assertRaises(psycopg2.errors.InsufficientPrivilege):
                    with conn, conn.cursor() as cur:
                        cur.execute("SELECT * FROM forge_external_workflow.plan_approve_overrides LIMIT 1")

        # All management verification scripts pass cleanly.
        for script in (
            "management/verify-external-workflows.sql",
            "management/verify-muse-read-access.sql",
            "management/verify.sql",
        ):
            res = self.pg.run_sql_script(SQL_DIR / script)
            self.assertEqual(res.returncode, 0, f"{script} failed: {res.stdout}\n{res.stderr}")
        res = self.pg.run_sql_script(SQL_DIR / "management/verify-runner-privileges.sql", user="forge_agent")
        self.assertEqual(res.returncode, 0, res.stdout + res.stderr)


if __name__ == "__main__":
    unittest.main()
