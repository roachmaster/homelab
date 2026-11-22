"""Owned PostgreSQL proofs for source-bound reviewer attestation corrections."""

from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
from dataclasses import replace
import hashlib
import json
import unittest
from unittest.mock import patch
from uuid import UUID, uuid4

from queue_store import Rejected
from work_api import WorkResume
from work_peer_review import PeerReviewError, parse_review_target, validate_live_review_verdict
from work_pg_fixture import PG16_AVAILABLE
from test_work_execution_and_usage import make_stop_proof
import test_work_workflow as workflows


@unittest.skipUnless(PG16_AVAILABLE, "Requires owned disposable PostgreSQL16")
class ReviewerCorrectionTest(unittest.TestCase):
    setUpClass = classmethod(workflows.AutonomousWorkflowTest.setUpClass.__func__)
    setUp = workflows.AutonomousWorkflowTest.setUp
    submit = workflows.AutonomousWorkflowTest.submit
    claim = workflows.AutonomousWorkflowTest.claim
    publish = workflows.AutonomousWorkflowTest.publish
    get = workflows.AutonomousWorkflowTest.get
    verdict = workflows.AutonomousWorkflowTest.verdict
    finish_plan_round = workflows.AutonomousWorkflowTest.finish_plan_round
    checked_implementation = workflows.AutonomousWorkflowTest.checked_implementation
    exhausted_code_review = workflows.AutonomousWorkflowTest.exhausted_code_review
    code_handoff_state = workflows.AutonomousWorkflowTest.code_handoff_state

    def rows(self, statement, parameters=()):
        with closing(self.cluster.connect("postgres")) as connection:
            with connection.cursor() as cursor:
                cursor.execute(statement, parameters)
                return cursor.fetchall()

    def target(self, critic):
        envelope = self.db.read_review_target(critic)
        return envelope, parse_review_target(envelope["target_json"], envelope["target_sha256"])

    def authority_state(self, source, review):
        source_state = self.code_handoff_state(source.task_id, review["review_id"])
        workflow = self.rows(
            "SELECT snapshot,policy_digest,plan_revisions FROM forge_agent.coding_workflows "
            "WHERE workflow_id=(SELECT workflow_id FROM forge_agent.coding_tasks WHERE id=%s)",
            (source.task_id,),
        )[0]
        return source_state, workflow

    def correction_count(self, source):
        return self.rows(
            "SELECT count(*) FROM forge_agent.coding_workflow_actions WHERE workflow_id="
            "(SELECT workflow_id FROM forge_agent.coding_tasks WHERE id=%s) AND status='completed' "
            "AND source_key LIKE 'peer-reviewer-correction:%%'", (source.task_id,),
        )[0][0]

    def assert_no_authority(self):
        self.assertIsNone(self.claim("agy-v2"))
        self.assertIsNone(self.db.publication_claim("correction-publisher"))
        self.assertIsNone(self.db.merge_claim("correction-merger"))

    def assert_artifact(self, critic, raw):
        observed = self.rows(
            "SELECT a.result_sha256,string_agg(c.content,'' ORDER BY c.chunk_index) "
            "FROM forge_agent.coding_attempts a JOIN forge_agent.coding_result_chunks c "
            "USING(owner_id,project_alias,task_id,attempt_id) WHERE a.attempt_id=%s "
            "GROUP BY a.result_sha256", (critic.attempt_id,),
        )[0]
        self.assertEqual(observed, (hashlib.sha256(raw.encode()).hexdigest(), raw))

    def incomplete(self, critic):
        raw = self.verdict(critic, attested_finding_ids=[])
        receipt = self.publish(critic, raw)
        return raw, receipt

    def exhausted_eight_finding_review(self):
        # Eight real independent findings survive normal linked repair/check
        # transitions. Reusing ci-2 in the failed check keeps the ledger at eight.
        admitted, author, critic, _checks = self.checked_implementation(linked=True)
        findings = [{"finding_id": "ci-2", "category": "source", "summary": "Fix the independent check."}]
        findings += [{"finding_id": "peer-" + str(index), "category": "source",
                      "summary": "Verify independent source issue " + str(index)} for index in range(1, 8)]
        self.publish(critic, self.verdict(critic, "revise", findings=findings))
        self.assertTrue(self.coordinator.run_once())
        repair = self.claim("agy-v2")
        self.assertEqual(repair.parent_task_id, author.task_id)
        receipt = self.publish(repair, "First repair preserves all eight findings and still fails a fixed gate.")
        self.db.queue_review(repair, receipt)
        job = self.db.review_claim("eight-finding-checks")
        outcome = {
            "disposition": "repair", "candidate_commit": "d" * 40, "gate_manifest_sha256": "e" * 64,
            "findings": [{**findings[0], "check_id": "make-ci"}], "verified_finding_ids": [],
            "fingerprint": "f" * 64, "summary": "The source still needs its independent check.",
            "cleanup_confirmed": True,
            "verification": {"checks_passed": False, "candidate_workspace_digest": "f" * 64,
                             "repair_requires_new_workspace": False, "cleanup_confirmed": True},
        }
        self.db.review_finish(job, outcome)
        repair = self.claim("agy-v2")
        receipt = self.publish(repair, "Fixed gates pass; all eight source findings still require reviewer attestation.")
        self.db.queue_review(repair, receipt)
        job = self.db.review_claim("eight-finding-checks")
        outcome.update(
            disposition="awaiting_review", findings=[], fingerprint=None,
            summary="Fixed gates pass but protected source findings require independent attestation.",
            verification={"checks_passed": True, "candidate_workspace_digest": "f" * 64,
                          "repair_requires_new_workspace": False, "cleanup_confirmed": True,
                          "integration_base": "a" * 40, "gate_changes_require_review": True,
                          "checks": [{"check_id": "make-ci", "status": "passed", "head_sha": "d" * 40}]},
        )
        self.db.review_finish(job, outcome)
        self.assertTrue(self.coordinator.run_once())
        critic = self.claim("codex-v2")
        self.assertIsNotNone(critic)
        return admitted, repair, critic, job

    def legacy_escalation(self):
        _admitted, source, critic, review = self.exhausted_code_review()
        envelope, target = self.target(critic)
        self.assertTrue([f for f in target["findings"] if f.get("status", "open") == "open"])
        raw, stopped_receipt = self.incomplete(critic)
        strict = validate_live_review_verdict

        def old_completeness_only(value, bound_target):
            # Reproduce only the released omission: every identity, finding and
            # other decoder guard remains validated by the selected strict parser.
            try:
                return strict(value, bound_target)
            except PeerReviewError as error:
                if hasattr(error, "verdict") and hasattr(error, "missing_finding_ids"):
                    return error.verdict
                raise

        with patch("work_peer_review.validate_live_review_verdict", side_effect=old_completeness_only):
            self.assertTrue(self.coordinator.run_once())
        self.assertEqual(self.get(source.task_id)["workflow"]["phase"], "escalated")
        fresh = self.db.reconcile(critic, "b" * 64, make_stop_proof(critic.unit_name))
        command = WorkResume(
            "homelab", UUID(critic.task_id), self.get(critic.task_id)["revision"],
            "reviewer-correction-" + str(uuid4()), "codex-v2", UUID(fresh),
            "Correct only the missing finding attestations against the exact unchanged target.",
        )
        return source, critic, review, envelope, raw, stopped_receipt, command

    def test_automatic_correction_uses_same_reviewer_and_target_then_genuine_approval(self):
        _admitted, source, critic, review = self.exhausted_eight_finding_review()
        before = self.authority_state(source, review)
        self.assertEqual((before[0][2]["repair_rounds"], before[0][2]["stagnant_rounds"]), (2, 2))
        envelope, target = self.target(critic)
        open_ids = [f["finding_id"] for f in target["findings"] if f.get("status", "open") == "open"]
        self.assertEqual(len(open_ids), 8)
        raw = self.verdict(critic, attested_finding_ids=open_ids[:3])
        self.publish(critic, raw)
        self.assertTrue(self.coordinator.run_once())
        self.assertEqual(self.authority_state(source, review), before)
        self.assertEqual(self.correction_count(source), 1)
        correction_intent = self.rows(
            "SELECT intent FROM forge_agent.coding_workflow_actions WHERE workflow_id="
            "(SELECT workflow_id FROM forge_agent.coding_tasks WHERE id=%s) "
            "AND source_key LIKE 'peer-reviewer-correction:%%'", (source.task_id,),
        )[0][0]
        self.assertEqual(set(correction_intent["missing_finding_ids"]), set(open_ids[3:]))
        note = self.rows('SELECT legacy_resume_note FROM forge_agent.coding_tasks WHERE id=%s', (critic.task_id,))[0][0]
        for identity in open_ids:
            self.assertIn(identity, note)
        self.assertIn('Required IDs:', note)
        self.assertIn('Missing IDs:', note)
        self.assertTrue(note.endswith('Return ONLY the same exact source-bound JSON verdict, no Markdown.'))
        self.assert_no_authority()
        correction = self.claim("codex-v2")
        self.assertIsNotNone(correction)
        self.assertEqual(correction.task_id, critic.task_id)
        self.assertNotEqual(correction.attempt_id, critic.attempt_id)
        self.assertEqual(correction.mode, "review")
        self.assertEqual(dict(correction.recipe), dict(critic.recipe))
        corrected_envelope, corrected_target = self.target(correction)
        self.assertEqual(corrected_envelope["target_json"], envelope["target_json"])
        self.assertEqual(corrected_envelope["target_sha256"], envelope["target_sha256"])
        self.assertEqual(corrected_target, target)
        self.assert_artifact(critic, raw)
        attested = [f["finding_id"] for f in target["findings"] if f.get("status", "open") == "open"]
        self.publish(correction, self.verdict(correction, attested_finding_ids=attested))
        self.assertTrue(self.coordinator.run_once())
        after = self.authority_state(source, review)
        self.assertEqual(after[0][2], before[0][2])
        self.assertEqual(after[0][3], before[0][3])
        self.assertEqual(after[1], before[1])
        self.assertEqual(self.get(source.task_id)["workflow"]["phase"], "publishing")
        self.assertIsNotNone(self.db.publication_claim("approved-correction-publisher"))
        self.assertIsNone(self.db.merge_claim("still-needs-hosted-checks"))
        self.assert_artifact(critic, raw)

    def test_three_corrections_are_bounded_without_resetting_exhausted_source_budget(self):
        _admitted, source, critic, review = self.exhausted_code_review()
        before = self.authority_state(source, review)
        envelope, _target = self.target(critic)
        for round_number in range(1, 5):
            with self.subTest(round=round_number):
                raw, _receipt = self.incomplete(critic)
                self.assertTrue(self.coordinator.run_once())
                self.assert_artifact(critic, raw)
                self.assertEqual(self.authority_state(source, review), before)
                self.assertEqual(self.correction_count(source), min(round_number, 3))
                self.assert_no_authority()
                following = self.claim("codex-v2")
                if round_number == 4:
                    self.assertIsNone(following)
                    self.assertEqual(self.get(source.task_id)["workflow"]["phase"], "escalated")
                else:
                    self.assertEqual(following.task_id, critic.task_id)
                    self.assertNotEqual(following.attempt_id, critic.attempt_id)
                    next_envelope, _ = self.target(following)
                    self.assertEqual(next_envelope["target_json"], envelope["target_json"])
                    critic = following

    def test_manual_legacy_correction_is_once_and_replay_preserves_saved_evidence(self):
        source, critic, review, envelope, raw, _stopped, command = self.legacy_escalation()
        before = self.authority_state(source, review)
        with ThreadPoolExecutor(max_workers=2) as executor:
            replies = list(executor.map(lambda _: self.service.resume(command), range(2)))
        self.assertEqual(replies[0], replies[1])
        self.assertEqual(self.service.resume(command), replies[0])
        self.assertEqual(self.authority_state(source, review), before)
        self.assertEqual(self.correction_count(source), 1)
        self.assert_no_authority()
        following = self.claim("codex-v2")
        self.assertEqual(following.task_id, critic.task_id)
        next_envelope, _ = self.target(following)
        self.assertEqual(next_envelope["target_sha256"], envelope["target_sha256"])
        self.assert_artifact(critic, raw)
        self.cluster.admin_exec(
            "DELETE FROM forge_agent.coding_worker_grants; DELETE FROM forge_agent.coding_worker_readiness",
            owner=False,
        )
        self.assertEqual(self.service.resume(command), replies[0])
        with self.assertRaises(Rejected):
            self.service.resume(replace(command, note="Changed immutable correction intent"))

    def test_manual_correction_rejects_missing_ci_source_drift_profile_and_stale_reconciliation(self):
        for fault in ("ci", "source", "profile", "stale"):
            with self.subTest(fault=fault):
                self.setUp()
                source, critic, review, _envelope, raw, stopped, command = self.legacy_escalation()
                if fault in {"ci", "source"}:
                    path = "checks_passed" if fault == "ci" else "candidate_workspace_digest"
                    value = "false" if fault == "ci" else '"' + "c" * 64 + '"'
                    self.cluster.admin_exec(
                        "UPDATE forge_agent.coding_reviews SET verification=jsonb_set(verification,'{" + path + "}',"
                        "'" + value + "'::jsonb) WHERE review_id='" + review["review_id"] + "'", owner=False,
                    )
                elif fault == "profile":
                    command = replace(command, agent_profile="agy-v2")
                else:
                    command = replace(command, reconciliation_receipt_id=UUID(stopped))
                before = self.authority_state(source, review)
                with self.assertRaises(Rejected):
                    self.service.resume(command)
                self.assertEqual(self.authority_state(source, review), before)
                self.assertEqual(self.correction_count(source), 0)
                self.assertIsNone(self.claim("codex-v2"))
                self.assert_no_authority()
                self.assert_artifact(critic, raw)

    def test_manual_correction_revalidates_attempt_plan_ci_projection_and_stopped_source(self):
        for fault in ("attempt_profile", "attempt_recipe", "ci_projection", "plan", "source_reserved"):
            with self.subTest(fault=fault):
                self.setUp()
                source, critic, review, _envelope, raw, _stopped, command = self.legacy_escalation()
                # Corrupt only the owned disposable database to prove the normal
                # service rejects drift. No production role/trigger is changed.
                with closing(self.cluster.connect("postgres")) as connection:
                    with connection, connection.cursor() as cursor:
                        if fault == "attempt_profile":
                            cursor.execute("UPDATE forge_agent.coding_attempts SET agent_profile='agy-v2' "
                                           "WHERE attempt_id=%s", (critic.attempt_id,))
                        elif fault == "attempt_recipe":
                            cursor.execute("UPDATE forge_agent.coding_attempts SET recipe_digest=%s "
                                           "WHERE attempt_id=%s", ("1" * 64, critic.attempt_id))
                        elif fault == "ci_projection":
                            cursor.execute("UPDATE forge_agent.coding_reviews SET verification="
                                           "jsonb_set(verification,'{checks,0,status}','\"failed\"'::jsonb) "
                                           "WHERE review_id=%s", (review["review_id"],))
                        elif fault == "plan":
                            cursor.execute("UPDATE forge_agent.coding_plan_decisions SET plan_sha256=%s "
                                           "WHERE decision_id=(SELECT coalesce(plan_decision_id,scope_plan_decision_id) "
                                           "FROM forge_agent.coding_tasks WHERE id=%s)", ("0" * 64, source.task_id))
                        else:
                            cursor.execute("UPDATE forge_agent.coding_attempts SET reservation_active=true "
                                           "WHERE attempt_id=%s", (source.attempt_id,))
                before = self.authority_state(source, review)
                with self.assertRaises(Rejected):
                    self.service.resume(command)
                self.assertEqual(self.authority_state(source, review), before)
                self.assertEqual(self.correction_count(source), 0)
                self.assertIsNone(self.claim("codex-v2"))
                self.assert_no_authority()
                self.assert_artifact(critic, raw)

    def test_reviewer_revision_after_correction_keeps_substantive_budget_and_handoff(self):
        _admitted, source, critic, review = self.exhausted_code_review()
        before = self.authority_state(source, review)
        self.incomplete(critic)
        self.assertTrue(self.coordinator.run_once())
        correction = self.claim("codex-v2")
        self.assertIsNotNone(correction)
        self.publish(correction, self.verdict(correction, "revise"))
        self.assertTrue(self.coordinator.run_once())
        self.assertEqual(self.authority_state(source, review), before)
        self.assertEqual(self.get(source.task_id)["workflow"]["phase"], "escalated")
        self.assert_no_authority()


if __name__ == "__main__":
    unittest.main()
