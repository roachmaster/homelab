"""Source-bound AGY to Codex handoff on an owned, real PostgreSQL instance.

Only fixture methods are reused. Importing other TestCase classes into this
module would duplicate their cases in the trusted backend inventory.
"""

from concurrent.futures import ThreadPoolExecutor
from copy import deepcopy
from dataclasses import replace
from datetime import UTC, datetime, timedelta
import hashlib
import json
from threading import Barrier
from types import SimpleNamespace
import unittest
from unittest.mock import patch
from uuid import UUID, uuid4

from queue_store import Rejected
from work_agent_handoff import AuthorHandoff, AgentHandoffFactory, changed_snapshot
from work_api import WorkCancel, WorkServiceFactory
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE
from test_work_execution_and_usage import make_stop_proof
import test_provider_model_route as route_fixtures
import test_work_native_retry as native_fixtures
import test_work_workflow as workflow_fixtures
import work_policy


BACKUP = route_fixtures.BACKUP


@unittest.skipUnless(PG16_AVAILABLE, "Requires owned PostgreSQL16")
class AgentHandoffPostgresTest(unittest.TestCase):
    submit = native_fixtures.NativeTimeoutRetryTest.submit
    claim = native_fixtures.NativeTimeoutRetryTest.claim
    publish = native_fixtures.NativeTimeoutRetryTest.publish
    get = native_fixtures.NativeTimeoutRetryTest.get
    verdict = native_fixtures.NativeTimeoutRetryTest.verdict
    finish_plan_round = native_fixtures.NativeTimeoutRetryTest.finish_plan_round
    implementation = native_fixtures.NativeTimeoutRetryTest.implementation
    checked_implementation = workflow_fixtures.AutonomousWorkflowTest.checked_implementation
    rows = native_fixtures.NativeTimeoutRetryTest.rows
    frozen = route_fixtures.ProviderModelRouteTest.frozen
    stop_quota_attempt = route_fixtures.ProviderModelRouteTest.stop_quota_attempt
    route = route_fixtures.ProviderModelRouteTest.route

    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def setUp(self):
        native_fixtures.NativeTimeoutRetryTest.setUp(self)
        profiles = dict(self.config.profiles)
        profiles["agy-v2"] = replace(profiles["agy-v2"], quota_handoff_profile="codex-author")
        profiles["codex-author"] = replace(profiles["codex-v2"], alias="codex-author")
        self.install_config(replace(self.config, profiles=profiles))
        self.cluster.grant_worker("mission-control", "homelab", "codex-author", modes=("plan", "implement"))
        self.advertise()

    def install_config(self, config):
        self.config = config
        self.service = WorkServiceFactory(config, lambda: self.cluster.connect("forge_agent"),
            retry_event_factory=lambda _: SimpleNamespace(create=lambda: None)).create()
        self.coordinator = self.service.workflow_worker
        self.handoff_port = self.coordinator.store.execution_policy.author_handoff

    def advertise(self, *, author=True, reviewer=True, backup=True, source_modes=None):
        profiles = []
        for alias, modes in (("agy-v2", source_modes or ["plan", "implement"]),
                             ("codex-author", ["plan", "implement"]),
                             ("codex-v2", ["review"])):
            if alias == "codex-author" and not author or alias == "codex-v2" and not reviewer:
                continue
            profile = self.config.profiles[alias]
            profiles.extend(dict(alias=alias, backend=profile.backend, model=model, modes=modes)
                            for model in (profile.model, *profile.backup_models)
                            if backup or model != BACKUP)
        self.db.advertise("workflow-worker", self.incarnation,
            [dict(alias="homelab", base_revision="a" * 40)], profiles, work_policy.POLICY_DIGEST)

    def configure_model_backup(self):
        profiles = dict(self.config.profiles)
        profiles["agy-v2"] = replace(profiles["agy-v2"], backup_models=(BACKUP,))
        self.install_config(replace(self.config, profiles=profiles))
        self.advertise()

    def command(self, lease, *, quota_attempt=None):
        return AuthorHandoff("homelab", UUID(lease.task_id), self.get(lease.task_id)["revision"],
                             UUID(lease.attempt_id), UUID(quota_attempt or lease.attempt_id))

    def stop_failed_attempt(self, lease):
        # Prepare a historical model overlay without running the automatic
        # scanner for unrelated workflows in this fixture's shared project.
        from work_provider_model_route import ModelRoute
        proof = make_stop_proof(lease.unit_name)
        self.db.publish(lease, "failed", None, None, "Historical pre-quota-classifier invocation failure.", proof)
        rec = self.db.reconcile(lease, "c" * 64, proof)
        return ModelRoute("homelab", UUID(lease.task_id), UUID(lease.attempt_id),
            self.get(lease.task_id)["revision"], UUID(rec), BACKUP,
            "quota-route-" + lease.attempt_id, "Preserve the pre-existing explicit model recovery.")

    def queued_command(self, task_id, quota_attempt):
        return AuthorHandoff("homelab", UUID(task_id), self.get(task_id)["revision"], None, UUID(quota_attempt))

    def delegation_count(self):
        return self.rows("SELECT count(*) FROM forge_agent.coding_workflow_actions WHERE source_key LIKE 'provider-author-handoff:%%'")[0][0]

    def workflow_counts(self, task_id):
        workflow = self.get(task_id)["workflow"]
        return workflow["plan_revisions"], workflow["code_revisions"]

    def handed_off_implementation(self):
        _admitted, _plan, original = self.implementation()
        approved = self.db.read_plan(original)
        self.stop_quota_attempt(original)
        self.handoff_port.handoff(self.command(original))
        handed = self.claim("codex-author")
        self.assertIsNotNone(handed)
        return original, handed, approved

    def checked_handed_off_code(self, source, *, source_revision=False):
        receipt = self.publish(source, "Codex completed the same approved implementation scope.")
        self.db.queue_review(source, receipt)
        self.coordinator.run_once()
        check = self.db.review_claim("fixed-handoff-checks")
        self.assertIsNotNone(check)
        self.db.review_finish(check, dict(disposition="awaiting_review", candidate_commit=("5" if source_revision else "d") * 40,
            gate_manifest_sha256="e" * 64, findings=[], verified_finding_ids=[], fingerprint=None,
            summary="Fixed independent handoff fixture checks passed.", cleanup_confirmed=True,
            verification=dict(checks_passed=True, candidate_workspace_digest="f" * 64,
                repair_requires_new_workspace=False, cleanup_confirmed=True, integration_base="a" * 40,
                gate_changes_require_review=source_revision, toolchain="fixture",
                checks=[dict(check_id="make-ci", status="passed", head_sha=("5" if source_revision else "d") * 40)])))
        self.coordinator.run_once()
        critic = self.claim("codex-v2")
        self.assertIsNotNone(critic)
        return critic

    def test_codex_deadline_scan_keeps_same_author_plan_workspace_and_twenty_per_event(self):
        from work_retry_events import RetryEventProviderFactory, RetryScope
        original, current, approved = self.handed_off_implementation()
        frozen = self.frozen(original)
        counts = self.workflow_counts(original.task_id)
        flow = self.coordinator.store
        events = RetryEventProviderFactory(self.config.owner).create()
        flow.retry_events = events
        self.service._store.retry_events = events
        with flow._transaction() as cursor:
            other = events.observe_failure(cursor,
                RetryScope(self.config.owner, "homelab", "native_workflow", UUID(current.workflow_id)),
                "infrastructure", "separate-jenkins-event", "a" * 64, phase_id="jenkins.deadline")
            events.reserve(cursor, other["event_id"], "separate-jenkins-first-retry")
        for ordinal in range(1, 21):
            native_fixtures.NativeTimeoutRetryTest.stop_at_deadline(self, current)
            # Exercise the ordinary scan, not the private direct retry ingress.
            flow._recover_native_timeouts()
            flow._recover_native_timeouts()
            resumed = self.claim("codex-author")
            self.assertIsNotNone(resumed, ordinal)
            self.assertEqual(resumed.task_id, current.task_id)
            self.assertEqual(resumed.agent_profile, "codex-author")
            self.assertEqual(resumed.recipe, current.recipe)
            self.assertEqual(resumed.resume_attempt_id, current.attempt_id)
            self.assertEqual(resumed.resume_workspace_digest, "c" * 64)
            self.assertEqual(self.db.read_plan(resumed), approved)
            self.assertEqual(self.workflow_counts(original.task_id), counts)
            self.assertIsNone(self.claim("agy-v2"))
            current = resumed
        self.assertEqual(self.rows("SELECT used,status FROM forge_agent.coding_retry_events WHERE event_id=%s", (other["event_id"],)), [(1, "open")])
        native_fixtures.NativeTimeoutRetryTest.stop_at_deadline(self, current)
        flow._recover_native_timeouts()
        self.assertIsNone(self.claim("codex-author"))
        self.assertIsNone(self.claim("agy-v2"))
        self.assertEqual(self.rows("SELECT used,attempt_limit,status FROM forge_agent.coding_retry_events WHERE scope_id=%s AND phase_id=%s",
            (current.workflow_id, "native_execution.deadline." + current.task_id)), [(20, 20, "exhausted")])
        self.assertEqual(self.frozen(original), frozen)
        self.assertEqual(self.workflow_counts(original.task_id), counts)

    def test_codex_deadline_scan_recovers_a_genuine_preexisting_escalation(self):
        original, stopped, approved = self.handed_off_implementation()
        frozen = self.frozen(original)
        flow = self.coordinator.store
        native_fixtures.NativeTimeoutRetryTest.stop_at_deadline(self, stopped)
        # Reproduce an older controller's genuine parked state by withholding
        # only this new scanner for one normal event-processing turn.
        with patch.object(flow, "_recover_native_timeouts", return_value=None):
            self.coordinator.run_once()
        self.assertEqual(self.get(original.task_id)["workflow"]["phase"], "escalated")
        flow._recover_native_timeouts()
        resumed = self.claim("codex-author")
        self.assertIsNotNone(resumed)
        self.assertEqual(resumed.agent_profile, "codex-author")
        self.assertEqual(resumed.resume_attempt_id, stopped.attempt_id)
        self.assertEqual(resumed.resume_workspace_digest, "c" * 64)
        self.assertEqual(self.db.read_plan(resumed), approved)
        self.assertEqual(self.frozen(original), frozen)

    def test_codex_source_incomplete_approval_gets_reviewer_only_correction(self):
        from work_peer_review import parse_review_target
        original, source, _approved = self.handed_off_implementation()
        frozen = self.frozen(original)
        critic = self.checked_handed_off_code(source)
        self.publish(critic, self.verdict(critic, "revise"))
        self.coordinator.run_once()
        source = self.claim("codex-author")
        self.assertIsNotNone(source)
        critic = self.checked_handed_off_code(source, source_revision=True)
        target = self.db.read_review_target(critic)
        parsed = parse_review_target(target["target_json"], target["target_sha256"])
        required = [finding["finding_id"] for finding in parsed["findings"] if finding["status"] == "open"]
        self.assertTrue(required)
        raw = self.verdict(critic, attested_finding_ids=[])
        self.publish(critic, raw)
        counts = self.workflow_counts(original.task_id)
        self.coordinator.run_once()
        correction = self.claim("codex-v2")
        self.assertIsNotNone(correction)
        self.assertEqual(correction.task_id, critic.task_id)
        self.assertNotEqual(correction.attempt_id, critic.attempt_id)
        self.assertEqual(correction.mode, "review")
        self.assertEqual(self.db.read_review_target(correction), target)
        self.assertIsNone(self.claim("codex-author"))
        self.assertIsNone(self.db.publication_claim("unapproved-codex-source"))
        self.publish(correction, self.verdict(correction, attested_finding_ids=required))
        self.coordinator.run_once()
        self.assertIsNotNone(self.db.publication_claim("corrected-independent-codex-review"))
        self.assertEqual(self.frozen(original), frozen)
        self.assertEqual(self.workflow_counts(original.task_id), counts)
        self.assertEqual(self.rows("SELECT result_sha256 FROM forge_agent.coding_attempts WHERE attempt_id=%s", (critic.attempt_id,)), [(hashlib.sha256(raw.encode()).hexdigest(),)])

    def test_codex_plan_bad_context_hash_gets_fresh_review_without_changing_old_escalated_row(self):
        from work_retry_events import RetryEventProviderFactory
        events = RetryEventProviderFactory(self.config.owner).create()
        self.coordinator.store.retry_events = events
        self.service._store.retry_events = events
        self.submit()
        original = self.claim("agy-v2")
        self.stop_quota_attempt(original)
        self.handoff_port.handoff(self.command(original))
        source = self.claim("codex-author")
        self.publish(source, "Codex Plan with the same admitted source-bound scope.")
        self.coordinator.run_once()
        critic = self.claim("codex-v2")
        target = self.db.read_review_target(critic)
        malformed = json.loads(self.verdict(critic))
        malformed["context_sha256"] = malformed["context_sha256"][:49]
        raw = json.dumps(malformed, separators=(",", ":"))
        self.publish(critic, raw)
        for _ in range(3):
            self.coordinator.run_once()
        replacement = self.claim("codex-v2")
        self.assertIsNotNone(replacement)
        self.assertNotEqual(replacement.task_id, critic.task_id)
        self.assertNotEqual(replacement.workflow_review_id, critic.workflow_review_id)
        self.assertIsNone(self.claim("codex-author"))
        self.assertEqual(self.rows("SELECT status,target_json,target_sha256 FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id=%s", (critic.workflow_review_id,)), [("escalated", target["target_json"], target["target_sha256"])])
        self.assertEqual(self.rows("SELECT result_sha256 FROM forge_agent.coding_attempts WHERE attempt_id=%s", (critic.attempt_id,)), [(hashlib.sha256(raw.encode()).hexdigest(),)])
        self.publish(replacement, self.verdict(replacement, "approve"))
        self.coordinator.run_once()
        implementation = self.claim("codex-author")
        self.assertIsNotNone(implementation)
        self.assertEqual(implementation.mode, "implement")
        self.assertEqual(self.db.read_plan(implementation)["plan_attempt_id"], source.attempt_id)

    def test_approved_implementation_keeps_plan_workspace_original_authority_and_budgets(self):
        _, plan, original = self.implementation()
        approved = self.db.read_plan(original)
        frozen = self.frozen(original)
        counts = self.workflow_counts(original.task_id)
        self.stop_quota_attempt(original)
        receipt = self.handoff_port.handoff(self.command(original))
        self.assertEqual(receipt["status"], "queued")
        projected = self.get(original.task_id)["workflow"]
        self.assertEqual(projected["author_profile"], "codex-author")
        self.assertEqual(projected["reviewer_profile"], "codex-v2")
        resumed = self.claim("codex-author")
        self.assertIsNotNone(resumed)
        self.assertEqual(resumed.task_id, original.task_id)
        self.assertEqual(resumed.agent_profile, "codex-author")
        self.assertEqual(resumed.recipe["backend"], "codex")
        self.assertEqual(resumed.mode, "implement")
        self.assertEqual(resumed.base_revision, original.base_revision)
        self.assertEqual(resumed.resume_attempt_id, original.attempt_id)
        self.assertEqual(resumed.resume_workspace_digest, "c" * 64)
        self.assertEqual(resumed.plan_decision_id, original.plan_decision_id)
        self.assertEqual(self.db.read_plan(resumed), approved)
        self.assertEqual(approved["plan_attempt_id"], plan.attempt_id)
        self.assertEqual(self.frozen(original), frozen)
        self.assertEqual(self.workflow_counts(original.task_id), counts)
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_retry_event_admissions"), [(0,)])
        self.assertIsNone(self.claim("agy-v2"))

    def test_normal_state_scan_hands_off_quota_source_once_without_new_agy_invocation(self):
        self.submit()
        original = self.claim("agy-v2")
        self.stop_quota_attempt(original)
        self.coordinator.run_once()
        self.coordinator.run_once()
        self.assertEqual(self.delegation_count(), 1)
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_attempts WHERE task_id=%s", (original.task_id,)), [(1,)])
        self.assertIsNone(self.claim("agy-v2"))
        resumed = self.claim("codex-author")
        self.assertEqual(resumed.task_id, original.task_id)

    def test_unattended_agy_source_hands_off_to_separately_pinned_codex_worker(self):
        profiles = dict(self.config.profiles)
        profiles["agy-v2"] = replace(profiles["agy-v2"], resource_profile=work_policy.AGY_POLICY_ID)
        self.install_config(replace(self.config, profiles=profiles))
        projects = [dict(alias="homelab", base_revision="a" * 40)]
        self.db.advertise("workflow-worker", self.incarnation, projects,
            [dict(alias=alias, backend="codex", model=profiles[alias].model, modes=modes)
             for alias, modes in (("codex-author", ["plan", "implement"]), ("codex-v2", ["review"]))],
            work_policy.POLICY_DIGEST)
        agy_incarnation = str(uuid4())
        self.db.advertise("agy-unattended", agy_incarnation, projects,
            [dict(alias="agy-v2", backend="antigravity", model=profiles["agy-v2"].model, modes=["plan", "implement"])],
            work_policy.AGY_POLICY_DIGEST)
        self.submit()
        original = self.db.claim("agy-unattended", agy_incarnation, ["agy-v2"])
        self.assertIsNotNone(original)
        frozen = self.frozen(original)
        self.assertEqual(original.recipe["resource_profile"], work_policy.AGY_POLICY_ID)
        self.stop_quota_attempt(original)
        self.handoff_port.handoff(self.command(original))
        resumed = self.claim("codex-author")
        self.assertIsNotNone(resumed)
        self.assertEqual(resumed.recipe["resource_profile"], work_policy.POLICY_ID)
        self.assertEqual(resumed.recipe["policy_digest"], work_policy.POLICY_DIGEST)
        self.assertEqual(self.frozen(original), frozen)

    def test_never_started_queued_plan_uses_same_project_model_quota_evidence(self):
        self.submit("quota-source")
        quota = self.claim("agy-v2")
        self.stop_quota_attempt(quota)
        _, queued = self.submit("still-never-started")
        command = self.queued_command(queued["task_id"], quota.attempt_id)
        self.handoff_port.handoff(command)
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_attempts WHERE task_id=%s", (queued["task_id"],)), [(0,)])
        resumed = self.claim("codex-author")
        self.assertEqual(resumed.task_id, queued["task_id"])
        self.assertIsNone(resumed.resume_attempt_id)
        self.assertIsNone(resumed.resume_workspace_digest)
        self.assertEqual(self.get(queued["task_id"])["workflow"]["phase"], "plan")

    def test_queued_implementation_after_approved_agy_plan_keeps_the_plan_source(self):
        self.submit("approved-plan-queued-implementation")
        # Freeze the intended fixture state before running the source-bound
        # handoff explicitly; no background worker exists in this database.
        with patch.object(self.handoff_port, "run_once", return_value=None):
            plan, _critic = self.finish_plan_round()
        # The quota must follow this actual Plan success. An older quota is
        # correctly closed by the successful AGY turn and cannot authorize it.
        self.submit("global-quota-source")
        self.cluster.admin_exec("DELETE FROM forge_agent.coding_worker_grants WHERE agent_profile='agy-v2' AND mode='implement'", owner=False)
        quota = self.claim("agy-v2")
        self.assertEqual(quota.mode, "plan")
        self.assertNotEqual(quota.task_id, plan.task_id)
        self.stop_quota_attempt(quota)
        self.cluster.grant_worker("mission-control", "homelab", "agy-v2", modes=("implement",))
        detail = self.get(plan.task_id)
        self.assertEqual(detail["mode"], "implement")
        self.assertEqual(detail["status"], "queued")
        self.assertEqual(self.rows("SELECT latest_attempt_id FROM forge_agent.coding_tasks WHERE id=%s", (plan.task_id,)), [(UUID(plan.attempt_id),)])
        self.handoff_port.handoff(self.command(plan, quota_attempt=quota.attempt_id))
        resumed = self.claim("codex-author")
        self.assertEqual(resumed.task_id, plan.task_id)
        self.assertEqual(resumed.mode, "implement")
        self.assertEqual(resumed.resume_attempt_id, plan.attempt_id)
        self.assertEqual(resumed.resume_workspace_digest, "b" * 64)
        self.assertEqual(self.db.read_plan(resumed)["plan_attempt_id"], plan.attempt_id)
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_attempts WHERE task_id=%s AND agent_profile='agy-v2'", (plan.task_id,)), [(1,)])

    def test_queued_plan_revision_hands_off_without_rewriting_the_revise_decision(self):
        self.submit("plan-awaiting-independent-revision")
        original = self.claim("agy-v2")
        frozen = self.frozen(original)
        self.publish(original, "AGY Plan awaiting independent feedback.")
        self.coordinator.run_once()
        critic = self.claim("codex-v2")
        target = self.db.read_review_target(critic)
        # Establish the genuine current quota after the successful Plan turn,
        # while its reviewer is still active and no author revision is queued.
        self.submit("quota-after-plan-before-revision")
        quota = self.claim("agy-v2")
        self.stop_quota_attempt(quota)
        verdict = self.verdict(critic, "revise")
        self.publish(critic, verdict)
        with patch.object(self.handoff_port, "run_once", return_value=None):
            self.coordinator.run_once()
        detail = self.get(original.task_id)
        self.assertEqual(detail["status"], "queued")
        self.assertEqual(detail["workflow"]["phase"], "plan_revision")
        counts = self.workflow_counts(original.task_id)
        self.handoff_port.handoff(self.command(original, quota_attempt=quota.attempt_id))
        revised = self.claim("codex-author")
        self.assertEqual(revised.task_id, original.task_id)
        self.assertEqual(revised.mode, "plan")
        self.assertEqual(revised.resume_attempt_id, original.attempt_id)
        feedback = self.db.read_workflow_feedback(revised)
        self.assertEqual(feedback["target_json"], target["target_json"])
        self.assertEqual(feedback["target_sha256"], target["target_sha256"])
        self.assertEqual(feedback["verdict_json"], verdict)
        self.assertIn("author_handoff", feedback)
        self.assertEqual(self.workflow_counts(original.task_id), counts)
        self.assertEqual(counts, (1, 0))
        self.assertEqual(self.frozen(original), frozen)

    def test_linked_repair_keeps_partial_workspace_plan_and_exact_feedback(self):
        _admitted, parent, critic, _checks = self.checked_implementation(linked=True)
        self.publish(critic, self.verdict(critic, "revise"))
        self.coordinator.run_once()
        child = self.claim("agy-v2")
        self.assertEqual(child.parent_task_id, parent.task_id)
        feedback = self.db.read_workflow_feedback(child)
        source_feedback = self.db.read_feedback(child)
        approved = self.db.read_plan(child)
        counts = self.workflow_counts(parent.task_id)
        frozen = self.frozen(child)
        self.stop_quota_attempt(child)
        self.handoff_port.handoff(self.command(child))
        resumed = self.claim("codex-author")
        self.assertEqual(resumed.task_id, child.task_id)
        self.assertEqual(resumed.parent_task_id, parent.task_id)
        self.assertEqual(resumed.scope_plan_decision_id, child.scope_plan_decision_id)
        self.assertEqual(resumed.feedback_id, child.feedback_id)
        self.assertEqual(resumed.feedback_sha256, child.feedback_sha256)
        self.assertEqual(resumed.resume_attempt_id, child.attempt_id)
        self.assertEqual(resumed.resume_workspace_digest, "c" * 64)
        handed_feedback = self.db.read_workflow_feedback(resumed)
        handed_source = self.db.read_feedback(resumed)
        for key, value in feedback.items():
            self.assertEqual(handed_feedback[key], value)
        for key, value in source_feedback.items():
            self.assertEqual(handed_source[key], value)
        self.assertIn("author_handoff", handed_feedback)
        self.assertIn("author_handoff", handed_source)
        self.assertEqual(self.db.read_plan(resumed), approved)
        self.assertEqual(self.workflow_counts(parent.task_id), counts)
        self.assertEqual(self.frozen(child), frozen)

    def test_queued_work_without_any_actual_quota_source_is_unchanged(self):
        _, queued = self.submit()
        command = self.queued_command(queued["task_id"], str(uuid4()))
        with self.assertRaises(Rejected):
            self.handoff_port.handoff(command)
        self.assertEqual(self.delegation_count(), 0)
        self.assertEqual(self.get(queued["task_id"])["agent_profile"], "agy-v2")

    def test_existing_model_overlay_composes_with_handoff_after_real_backup_quota(self):
        self.configure_model_backup()
        _, _, original = self.implementation()
        frozen = self.frozen(original)
        approved = self.db.read_plan(original)
        route = self.stop_failed_attempt(original)
        self.route(original, route)
        backup = self.claim("agy-v2")
        self.assertEqual(backup.recipe["model"], BACKUP)
        self.stop_quota_attempt(backup)
        self.handoff_port.handoff(self.command(backup))
        resumed = self.claim("codex-author")
        self.assertEqual(resumed.resume_attempt_id, backup.attempt_id)
        self.assertEqual(self.db.read_plan(resumed), approved)
        self.assertEqual(self.frozen(original), frozen)
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_workflow_actions WHERE source_key LIKE 'provider-model-route:%%'"), [(1,)])
        self.assertEqual(self.delegation_count(), 1)

    def test_existing_queued_opus_resume_retains_older_source_and_workspace(self):
        self.configure_model_backup()
        self.submit("backup-quota-source")
        quota_primary = self.claim("agy-v2")
        route = self.stop_failed_attempt(quota_primary)
        self.route(quota_primary, route)
        quota_backup = self.claim("agy-v2")
        self.stop_quota_attempt(quota_backup)
        self.submit("queued-old-source")
        primary = self.claim("agy-v2")
        route = self.stop_failed_attempt(primary)
        self.route(primary, route)
        detail = self.get(primary.task_id)
        self.assertEqual(detail["status"], "queued")
        self.handoff_port.handoff(self.command(primary, quota_attempt=quota_backup.attempt_id))
        resumed = self.claim("codex-author")
        self.assertEqual(resumed.task_id, primary.task_id)
        self.assertEqual(resumed.resume_attempt_id, primary.attempt_id)
        self.assertEqual(resumed.resume_workspace_digest, "c" * 64)
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_attempts WHERE task_id=%s", (primary.task_id,)), [(2,)])

    def test_concurrent_identical_handoff_is_idempotent_and_changed_request_is_rejected(self):
        self.submit()
        original = self.claim("agy-v2")
        self.stop_quota_attempt(original)
        command = self.command(original)
        barrier = Barrier(2)
        def run():
            barrier.wait(timeout=5)
            return self.handoff_port.handoff(command)
        with ThreadPoolExecutor(max_workers=2) as executor:
            left, right = executor.submit(run), executor.submit(run)
            receipt = left.result(timeout=10)
            self.assertEqual(right.result(timeout=10), receipt)
        self.assertEqual(self.handoff_port.handoff(command), receipt)
        with self.assertRaises(Rejected):
            self.handoff_port.handoff(replace(command, quota_source_attempt_id=uuid4()))
        self.assertEqual(self.delegation_count(), 1)
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_events WHERE task_id=%s AND event_kind='resumed'", (original.task_id,)), [(1,)])

    def test_claim_race_never_steals_a_live_agy_writer(self):
        self.submit("quota-for-race")
        quota = self.claim("agy-v2")
        self.stop_quota_attempt(quota)
        _, queued = self.submit("queued-claim-race")
        command = self.queued_command(queued["task_id"], quota.attempt_id)
        barrier = Barrier(2)
        def handoff():
            barrier.wait(timeout=5)
            try:
                return self.handoff_port.handoff(command)
            except Rejected:
                return None
        def claim():
            barrier.wait(timeout=5)
            return self.claim("agy-v2")
        with ThreadPoolExecutor(max_workers=2) as executor:
            hand, author = executor.submit(handoff), executor.submit(claim)
            handed, claimed = hand.result(timeout=10), author.result(timeout=10)
        if claimed is not None:
            self.assertEqual(claimed.task_id, queued["task_id"])
            self.assertIsNone(handed)
            self.assertEqual(self.delegation_count(), 0)
            self.assertEqual(self.get(queued["task_id"])["agent_profile"], "agy-v2")
        else:
            self.assertIsNotNone(handed)
            self.assertEqual(self.delegation_count(), 1)
            self.assertEqual(self.claim("codex-author").task_id, queued["task_id"])

    def test_cancel_race_preserves_owner_cancellation_and_no_claimable_successor(self):
        self.submit()
        original = self.claim("agy-v2")
        self.stop_quota_attempt(original)
        command = self.command(original)
        barrier = Barrier(2)
        def handoff():
            barrier.wait(timeout=5)
            try:
                return self.handoff_port.handoff(command)
            except Rejected:
                return None
        def cancel():
            barrier.wait(timeout=5)
            try:
                self.service.cancel(WorkCancel("homelab", UUID(original.task_id), command.expected_revision, "owner-race"))
            except Rejected:
                # If handoff wins, cancellation must bind to the new revision.
                self.service.cancel(WorkCancel("homelab", UUID(original.task_id), self.get(original.task_id)["revision"], "owner-race-current"))
        with ThreadPoolExecutor(max_workers=2) as executor:
            hand, owner = executor.submit(handoff), executor.submit(cancel)
            hand.result(timeout=10)
            owner.result(timeout=10)
        self.assertEqual(self.get(original.task_id)["status"], "cancelled")
        self.assertIsNone(self.claim("agy-v2"))
        self.assertIsNone(self.claim("codex-author"))

    def test_missing_or_stale_readiness_and_revoked_grants_leave_source_unchanged(self):
        for case in ("author_readiness", "reviewer_readiness", "stale_readiness", "author_grant", "reviewer_grant"):
            with self.subTest(case=case):
                self.setUp()
                self.submit()
                original = self.claim("agy-v2")
                self.stop_quota_attempt(original)
                if case == "author_readiness":
                    self.advertise(author=False)
                elif case == "reviewer_readiness":
                    self.advertise(reviewer=False)
                elif case == "stale_readiness":
                    self.cluster.admin_exec("UPDATE forge_agent.coding_worker_readiness SET observed_at=clock_timestamp()-interval '5 minutes'", owner=False)
                else:
                    alias = "codex-author" if case == "author_grant" else "codex-v2"
                    self.cluster.admin_exec("DELETE FROM forge_agent.coding_worker_grants WHERE agent_profile='" + alias + "'", owner=False)
                with self.assertRaises(Rejected):
                    self.handoff_port.handoff(self.command(original))
                self.assertEqual(self.delegation_count(), 0)
                self.assertEqual(self.get(original.task_id)["status"], "quota_blocked")
                self.assertEqual(self.get(original.task_id)["agent_profile"], "agy-v2")

    def test_wrong_revision_attempt_project_and_quota_source_are_rejected(self):
        self.submit()
        original = self.claim("agy-v2")
        self.stop_quota_attempt(original)
        command = self.command(original)
        for changed in (replace(command, expected_revision=command.expected_revision + 1),
                        replace(command, source_attempt_id=uuid4()),
                        replace(command, quota_source_attempt_id=uuid4()),
                        replace(command, project_alias="docs")):
            with self.subTest(command=changed), self.assertRaises(Rejected):
                self.handoff_port.handoff(changed)
        self.assertEqual(self.delegation_count(), 0)

    def test_unrelated_owner_and_wrong_model_quota_cannot_authorize_a_handoff(self):
        self.submit("actual-quota")
        quota = self.claim("agy-v2")
        self.stop_quota_attempt(quota)
        other_config = replace(self.config, owner="unrelated-owner")
        other = WorkServiceFactory(other_config, lambda: self.cluster.connect("forge_agent"),
            retry_event_factory=lambda _: SimpleNamespace(create=lambda: None)).create()
        with self.assertRaises(Rejected):
            other.workflow_worker.store.execution_policy.author_handoff.handoff(self.command(quota))
        profiles = dict(self.config.profiles)
        profiles["agy-v2"] = replace(profiles["agy-v2"], model="different-gemini")
        self.install_config(replace(self.config, profiles=profiles))
        self.advertise()
        _, queued = self.submit("different-model")
        with self.assertRaises(Rejected):
            self.handoff_port.handoff(self.queued_command(queued["task_id"], quota.attempt_id))
        self.assertEqual(self.delegation_count(), 0)

    def test_unattested_quota_and_a_later_success_close_the_recovery_source(self):
        self.submit("malformed-quota")
        malformed = self.claim("agy-v2")
        proof = make_stop_proof(malformed.unit_name)
        self.db.publish(malformed, "quota_blocked", None, None, "Model prose about quota is not terminal telemetry.", proof)
        self.db.reconcile(malformed, "c" * 64, proof)
        with self.assertRaises(Rejected):
            self.handoff_port.handoff(self.command(malformed))
        self.submit("actual-quota")
        quota = self.claim("agy-v2")
        self.stop_quota_attempt(quota)
        self.submit("later-model-success")
        success = self.claim("agy-v2")
        self.publish(success, "A later genuine successful Plan closes the earlier quota episode.")
        with self.assertRaises(Rejected):
            self.handoff_port.handoff(self.command(quota))
        self.assertEqual(self.delegation_count(), 0)

    def test_missing_reconciliation_or_active_reservation_blocks_a_stopped_source(self):
        for case in ("missing_reconciliation", "active_reservation", "wrong_scope_proof"):
            with self.subTest(case=case):
                self.setUp()
                self.submit()
                original = self.claim("agy-v2")
                self.stop_quota_attempt(original)
                if case == "missing_reconciliation":
                    self.cluster.admin_exec("DELETE FROM forge_agent.coding_reconciliation_receipts WHERE attempt_id='" + original.attempt_id + "'", owner=False)
                elif case == "active_reservation":
                    self.cluster.admin_exec("UPDATE forge_agent.coding_attempts SET reservation_active=true WHERE attempt_id='" + original.attempt_id + "'", owner=False)
                else:
                    self.cluster.admin_exec("UPDATE forge_agent.coding_reconciliation_receipts SET stop_proof=jsonb_set(stop_proof,'{unit_name}','\"unrelated.scope\"') WHERE attempt_id='" + original.attempt_id + "'", owner=False)
                with self.assertRaises(Rejected):
                    self.handoff_port.handoff(self.command(original))
                self.assertEqual(self.delegation_count(), 0)

    def test_reviewer_tasks_and_current_reviewer_alias_cannot_become_the_author(self):
        self.submit()
        author = self.claim("agy-v2")
        self.publish(author, "Plan requiring independent review.")
        self.coordinator.run_once()
        reviewer = self.claim("codex-v2")
        self.stop_quota_attempt(reviewer)
        with self.assertRaises(Rejected):
            self.handoff_port.handoff(self.command(reviewer))
        snapshot = self.rows("SELECT snapshot FROM forge_agent.coding_workflows WHERE root_task_id=%s", (author.task_id,))[0][0]
        class SameReviewerCatalog:
            def resolve_handoff(_self, _source, mode):
                return "codex-v2", self.config.catalog().resolve("codex-v2", mode)
        with self.assertRaises(Rejected):
            changed_snapshot(snapshot, SameReviewerCatalog())
        self.assertEqual(self.delegation_count(), 0)

    def test_delegated_plan_revision_approval_and_implementation_keep_independent_review(self):
        self.submit()
        original = self.claim("agy-v2")
        frozen = self.frozen(original)
        self.stop_quota_attempt(original)
        self.handoff_port.handoff(self.command(original))
        first = self.claim("codex-author")
        self.publish(first, "First Codex Plan keeps the existing scope and provider boundary.")
        self.coordinator.run_once()
        critic = self.claim("codex-v2")
        self.assertEqual(critic.mode, "review")
        self.publish(critic, self.verdict(critic, "revise"))
        self.coordinator.run_once()
        revised = self.claim("codex-author")
        self.assertEqual(revised.mode, "plan")
        self.assertEqual(revised.task_id, original.task_id)
        self.assertIsNotNone(self.db.read_workflow_feedback(revised))
        self.publish(revised, "Revised Codex Plan addresses the source-bound finding.")
        self.coordinator.run_once()
        critic = self.claim("codex-v2")
        self.publish(critic, self.verdict(critic, "approve"))
        self.coordinator.run_once()
        implementation = self.claim("codex-author")
        self.assertEqual(implementation.mode, "implement")
        self.assertIsNotNone(implementation.plan_decision_id)
        self.assertEqual(self.workflow_counts(original.task_id), (1, 0))
        self.assertEqual(self.frozen(original), frozen)
        self.assertIsNone(self.claim("agy-v2"))

    def test_codex_quota_respects_reported_reset_without_bouncing_back_to_agy(self):
        self.submit()
        original = self.claim("agy-v2")
        self.stop_quota_attempt(original)
        self.handoff_port.handoff(self.command(original))
        codex = self.claim("codex-author")
        self.stop_quota_attempt(codex, reset=1256)
        self.coordinator.run_once()
        self.assertIsNone(self.claim("codex-author"))
        self.assertIsNone(self.claim("agy-v2"))
        wait = self.rows("SELECT intent FROM forge_agent.coding_workflow_actions WHERE source_key=%s",
                         ("provider-quota-wait:" + codex.attempt_id,))[0][0]
        self.assertEqual(wait["wait_source"], "provider_reported_reset")
        self.assertEqual(wait["diagnostic"]["retry_after_seconds"], 1256)
        clock = SimpleNamespace(now=lambda _tz: datetime.now(UTC) + timedelta(seconds=1257), fromisoformat=datetime.fromisoformat)
        with patch("work_provider_model_route.datetime", clock):
            self.coordinator.run_once()
        resumed = self.claim("codex-author")
        self.assertIsNotNone(resumed)
        self.assertEqual(resumed.agent_profile, "codex-author")
        self.assertEqual(resumed.resume_attempt_id, codex.attempt_id)
        self.assertEqual(resumed.resume_workspace_digest, "c" * 64)
        self.assertEqual(self.delegation_count(), 1)
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_attempts WHERE task_id=%s AND agent_profile='agy-v2'", (original.task_id,)), [(1,)])

    def test_codex_unknown_quota_reset_uses_local_backoff_and_twenty_episode_attempts(self):
        self.submit()
        original = self.claim("agy-v2")
        self.stop_quota_attempt(original)
        self.handoff_port.handoff(self.command(original))
        current = self.claim("codex-author")
        counts = self.workflow_counts(original.task_id)
        clock = SimpleNamespace(now=lambda _tz: datetime.now(UTC) + timedelta(seconds=901), fromisoformat=datetime.fromisoformat)
        for ordinal in range(21):
            proof = make_stop_proof(current.unit_name)
            self.db.publish(current, "quota_blocked", None, None,
                "Codex reported exhausted included usage without a reset timestamp.", proof)
            self.db.reconcile(current, "c" * 64, proof)
            self.coordinator.run_once()
            wait = self.rows("SELECT intent FROM forge_agent.coding_workflow_actions WHERE source_key=%s",
                             ("provider-quota-wait:" + current.attempt_id,))[0][0]
            self.assertEqual(wait["wait_source"], "local_quota_retry_backoff")
            self.assertIsNone(wait["diagnostic"]["retry_after_seconds"])
            self.assertEqual(wait["reset_resumes_used"], min(ordinal, 20))
            self.assertIsNone(self.claim("codex-author"))
            self.assertIsNone(self.claim("agy-v2"))
            with patch("work_provider_model_route.datetime", clock):
                self.coordinator.run_once()
            next_attempt = self.claim("codex-author")
            if ordinal < 20:
                self.assertIsNotNone(next_attempt, ordinal)
                self.assertEqual(next_attempt.resume_attempt_id, current.attempt_id)
                self.assertEqual(next_attempt.recipe, current.recipe)
                current = next_attempt
            else:
                self.assertIsNone(next_attempt)
        detail = self.get(original.task_id)
        self.assertEqual(detail["status"], "quota_blocked")
        self.assertNotEqual(detail["workflow"]["phase"], "escalated")
        self.assertEqual(self.workflow_counts(original.task_id), counts)
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_workflow_actions WHERE source_key LIKE 'provider-quota-resume:%%'"), [(20,)])
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_retry_event_admissions"), [(0,)])
        self.assertEqual(self.delegation_count(), 1)

    def test_delegated_candidate_requires_exact_checks_independent_review_and_merge_gates(self):
        _, _, original = self.implementation()
        frozen = self.frozen(original)
        self.stop_quota_attempt(original)
        self.handoff_port.handoff(self.command(original))
        resumed = self.claim("codex-author")
        rec = self.publish(resumed, "Implemented the original approved Plan under the Codex handoff.")
        self.db.queue_review(resumed, rec)
        check = self.db.review_claim("fixed-checks")
        self.assertIsNotNone(check)
        self.assertEqual(str(check["attempt_id"]), resumed.attempt_id)
        self.db.review_finish(check, dict(disposition="awaiting_review", candidate_commit="d" * 40,
            gate_manifest_sha256="e" * 64, findings=[], verified_finding_ids=[], fingerprint=None,
            summary="Fixed fixture checks passed.", cleanup_confirmed=True,
            verification=dict(checks_passed=True, candidate_workspace_digest="f" * 64,
                repair_requires_new_workspace=False, cleanup_confirmed=True, integration_base="a" * 40,
                toolchain="fixture", checks=[dict(check_id="make-ci", status="passed", head_sha="d" * 40)])))
        self.coordinator.run_once()
        critic = self.claim("codex-v2")
        self.assertIsNotNone(critic)
        self.assertEqual(critic.mode, "review")
        self.assertNotEqual(critic.agent_profile, resumed.agent_profile)
        self.assertIsNone(self.db.publication_claim("publisher"))
        self.publish(critic, self.verdict(critic, "approve"))
        self.coordinator.run_once()
        publication = self.db.publication_claim("publisher")
        self.assertIsNotNone(publication)
        self.db.publication_finish(publication, "publishing", "Reviewed branch", branch="forge/handoff-fixture", commit_sha="d" * 40)
        self.db.publication_finish(publication, "published", "Reviewed PR", branch="forge/handoff-fixture", commit_sha="d" * 40,
            pr_url="https://github.com/fixture/project/pull/1")
        self.coordinator.run_once()
        self.assertIsNone(self.db.merge_claim("merger"))
        self.db.record_hosted_checks(publication["publication_id"], "d" * 40,
            [dict(name=name, status="success", head_sha="d" * 40) for name in ("check", "pr-fast-gate", "real-browser")])
        self.coordinator.run_once()
        merge = self.db.merge_claim("merger")
        self.assertIsNotNone(merge)
        self.assertEqual(merge["policy_digest"], frozen[1])
        intent = dict(version=1, merge_id=merge["merge_id"], workflow_id=merge["workflow_id"], task_id=merge["task_id"],
            repository="fixture/project", pr_url=merge["pr_url"], candidate_commit="d" * 40, candidate_tree="1" * 40,
            expected_main="a" * 40, merge_commit="2" * 40, review_id=merge["review_id"], gate_manifest_sha256="e" * 64,
            commit_payload=dict(message="Merge independently reviewed handoff", tree="1" * 40, parents=["a" * 40, "d" * 40],
                author=dict(name="Forge", email="forge@example.test", date="2026-11-20T00:00:00Z"),
                committer=dict(name="Forge", email="forge@example.test", date="2026-11-20T00:00:00Z")))
        self.db.merge_intent(merge, intent)
        self.db.merge_finish(merge, "merged", dict(candidate_commit="d" * 40, merge_commit="2" * 40,
            pr_url=merge["pr_url"], summary="Exact independently reviewed source merged."))
        self.assertEqual(self.get(original.task_id)["workflow"]["phase"], "completed")
        self.assertEqual(self.frozen(original), frozen)


class AgentHandoffContractTest(unittest.TestCase):
    def test_command_rejects_invalid_identifiers_and_revisions(self):
        base = dict(project_alias="homelab", task_id=uuid4(), expected_revision=1,
                    source_attempt_id=None, quota_source_attempt_id=uuid4())
        for change in (dict(project_alias="../other"), dict(task_id="not-uuid"), dict(expected_revision=True),
                       dict(expected_revision=0), dict(source_attempt_id="not-uuid"), dict(quota_source_attempt_id="not-uuid")):
            with self.subTest(change=change), self.assertRaises(ValueError):
                AuthorHandoff(**{**base, **change})

    def test_factory_is_effect_free_and_requires_a_ready_source_policy(self):
        factory = AgentHandoffFactory(object(), lambda *_args, **_kwargs: None)
        port = factory.create()
        self.assertTrue(callable(port.run_once))
        self.assertTrue(callable(port.handoff))
        with self.assertRaises(TypeError):
            AgentHandoffFactory(object(), None).create()

    def test_codex_handoff_starts_fresh_and_review_never_reads_author_session(self):
        import test_codex_sessions as session_fixtures
        from work_sessions import AntigravitySessionProvider
        session_fixtures.CodexSessionTest.setUp(self)
        old = deepcopy(self.task)
        old["agent_profile"] = "agy-v2"
        old["recipe"]["backend"] = "antigravity"
        old["recipe"]["model"] = "fixture-gemini"
        agy = AntigravitySessionProvider(self.config)
        agy.prepare()
        agy.remember(agy.select(old, self.workspace, self.binary_sha), self.workspace, str(uuid4()))
        handed = session_fixtures.CodexSessionTest._resume(self, old)
        handed["agent_profile"] = "codex-author"
        handed["recipe"] = dict(self.task["recipe"])
        selected = self.provider.select(handed, self.workspace, self.binary_sha)
        self.assertIsNone(selected.conversation_id)
        self.provider.remember(selected, self.workspace, str(uuid4()))
        reviewer = session_fixtures.CodexSessionTest._resume(self, handed)
        reviewer["agent_profile"] = "codex-v2"
        reviewer["mode"] = "review"
        reviewer["recipe"]["mode"] = "review"
        reviewer["workflow_review_id"] = str(uuid4())
        with patch.object(self.provider, "_read", side_effect=AssertionError("Reviewer must start fresh")):
            self.assertIsNone(self.provider.select(reviewer, self.workspace, self.binary_sha).conversation_id)


if __name__ == "__main__":
    unittest.main()
