"""A verified author handoff delivers original review bytes to a new author."""

from copy import deepcopy
import hashlib
import json
import unittest
from tempfile import TemporaryDirectory
from unittest.mock import Mock
from uuid import uuid4

from test_work_peer_review import bind, target, verdict
from test_work_review_context import fixture as build_context_fixture
from work_peer_context import (AUTHOR_HANDOFF_POLICY, AuthorHandoffContext,
    PeerReviewContextProvider, validate_workflow_feedback)
from work_peer_review import PeerReviewError
from work_author_handoff_context import BuildAuthorHandoffContext
from work_review_context import ReviewContext, ReviewContextError, ReviewContextProvider, validate_context


def fixture(kind="plan", decision="approve", *, author="agy-primary"):
    subject = target(kind, source="Preserve the approved Plan, source and review history.")
    subject["author_profile"] = author
    parsed = bind(subject)
    response = json.dumps(verdict(parsed, decision), ensure_ascii=False, indent=2)
    lease = dict(task_id=subject["source_task_id"], attempt_id=str(uuid4()),
        workflow_id=subject["workflow_id"], mode="plan" if decision == "revise" and kind == "plan" else "implement",
        agent_profile="codex-author", base_revision=subject["base_revision"], workflow_review_id=None,
        workflow_feedback_review_id=subject["workflow_review_id"],
        workflow_feedback_sha256=hashlib.sha256(response.encode()).hexdigest())
    envelope = dict(target_json=json.dumps(subject, ensure_ascii=False),
        target_sha256=parsed["_context_sha256"], verdict_json=response,
        verdict_sha256=lease["workflow_feedback_sha256"])
    context = dict(plan_attempt_id=subject["source_attempt_id"], plan_sha256=subject["source_result_sha256"])
    feedback = None
    if kind == "code":
        context = dict(plan_attempt_id=subject["approved_plan"]["attempt_id"],
                       plan_sha256=subject["approved_plan"]["sha256"])
        previous = dict(parent_attempt_id=subject["source_attempt_id"],
            parent_result_sha256=subject["source_result_sha256"], candidate_commit=subject["candidate_commit"],
            gate_manifest_sha256=subject["gate_manifest_sha256"])
        raw = json.dumps(previous).encode()
        feedback = ReviewContext(raw, hashlib.sha256(raw).hexdigest(), str(uuid4()), ())
    proof = dict(version=1, policy=AUTHOR_HANDOFF_POLICY, workflow_id=lease["workflow_id"],
        task_id=lease["task_id"], attempt_id=lease["attempt_id"], source_author_profile=subject["author_profile"],
        target_author_profile=lease["agent_profile"], workflow_feedback_review_id=lease["workflow_feedback_review_id"],
        workflow_feedback_sha256=lease["workflow_feedback_sha256"], action_id=str(uuid4()),
        intent_sha256="a" * 64, original_snapshot_sha256="b" * 64)
    envelope["author_handoff"] = proof
    return lease, envelope, context, feedback


def build_fixture(root, *, kind="native_receipt", partial=True):
    _config, lease, workspace, envelope = build_context_fixture(root)
    lease["workflow_id"] = str(uuid4())
    lease["plan_decision_id"] = str(uuid4())
    packet = envelope["feedback"]
    packet["workspace_digest_kind"] = kind
    packet["plan_decision_id"] = lease["plan_decision_id"]
    if kind == "candidate_tree":
        (workspace.path / ".git").write_text("gitdir: /approved/worktrees/repair\n")
        lease.update(parent_task_id=str(uuid4()), repair_source_review_id=str(uuid4()),
                     repair_candidate_commit=lease["base_revision"])
        packet.update(candidate_commit=lease["base_revision"], workspace_digest=workspace.source_digest())
    envelope["feedback_json"] = json.dumps(packet, indent=2, ensure_ascii=False)
    lease["feedback_sha256"] = hashlib.sha256(envelope["feedback_json"].encode()).hexdigest()
    lease["agent_profile"] = "codex-author"
    lease["resume_attempt_id"] = str(uuid4())
    if partial:
        (workspace.path / "source.txt").write_text("partial approved repair before the AGY quota stop\n")
    lease["resume_workspace_digest"] = workspace.digest()
    envelope["author_handoff"] = dict(version=1, policy=AUTHOR_HANDOFF_POLICY,
        workflow_id=lease["workflow_id"], task_id=lease["task_id"], attempt_id=lease["attempt_id"],
        source_author_profile=packet["agent_profile"], target_author_profile=lease["agent_profile"],
        action_id=str(uuid4()), intent_sha256="a" * 64, original_snapshot_sha256="b" * 64,
        feedback_id=lease["feedback_id"], feedback_sha256=lease["feedback_sha256"],
        parent_attempt_id=packet["parent_attempt_id"], resume_attempt_id=lease["resume_attempt_id"],
        resume_workspace_digest=lease["resume_workspace_digest"], feedback_workspace_digest=packet["workspace_digest"])
    return lease, workspace, envelope


class AuthorHandoffContextTest(unittest.TestCase):
    def test_original_plan_approval_and_revision_bytes_survive_handoff(self):
        for decision in ("approve", "revise"):
            with self.subTest(decision=decision):
                lease, envelope, plan, _ = fixture(decision=decision)
                originals = deepcopy((lease, envelope, plan))
                store = Mock()
                store.read_workflow_feedback.return_value = envelope
                ready = PeerReviewContextProvider(store).acquire_feedback(lease, plan_context=plan)
                self.assertIsInstance(ready.author_handoff, AuthorHandoffContext)
                self.assertEqual(ready.content, envelope["verdict_json"].encode())
                self.assertEqual(ready.sha256, lease["workflow_feedback_sha256"])
                self.assertEqual(json.loads(envelope["target_json"])["author_profile"], "agy-primary")
                self.assertEqual((lease, envelope, plan), originals)
                store.read_workflow_feedback.assert_called_once_with(lease)

    def test_original_code_revision_and_approved_plan_survive_handoff(self):
        lease, envelope, plan, feedback = fixture("code", "revise")
        originals = deepcopy((lease, envelope, plan, feedback))
        ready = validate_workflow_feedback(lease, envelope, plan_context=plan, feedback_context=feedback)
        self.assertEqual(ready.content, envelope["verdict_json"].encode())
        self.assertEqual(ready.author_handoff.source_author_profile, "agy-primary")
        self.assertEqual(ready.author_handoff.target_author_profile, "codex-author")
        self.assertEqual((lease, envelope, plan, feedback), originals)

    def test_new_author_does_not_receive_old_author_feedback_without_proof(self):
        lease, envelope, plan, _ = fixture()
        envelope.pop("author_handoff")
        with self.assertRaises(PeerReviewError):
            validate_workflow_feedback(lease, envelope, plan_context=plan)

    def test_proof_is_bound_to_current_attempt_and_exact_feedback(self):
        for field in ("workflow_id", "task_id", "attempt_id", "workflow_feedback_review_id",
                      "workflow_feedback_sha256", "source_author_profile", "target_author_profile"):
            with self.subTest(field=field):
                lease, envelope, plan, _ = fixture()
                changed = envelope["author_handoff"]
                changed[field] = ("0" * 64 if field.endswith("sha256") else
                    "unrelated-author" if field.endswith("profile") else str(uuid4()))
                with self.assertRaises(PeerReviewError):
                    validate_workflow_feedback(lease, envelope, plan_context=plan)

    def test_proof_has_a_closed_versioned_schema_and_canonical_identities(self):
        cases = (("version", True), ("version", 2), ("policy", "unverified-override"),
            ("action_id", "not-a-uuid"), ("intent_sha256", "a" * 63),
            ("original_snapshot_sha256", "B" * 64), ("source_author_profile", "untrusted alias"),
            ("target_author_profile", ""), ("attempt_id", None))
        for field, value in cases:
            with self.subTest(field=field, value=value):
                lease, envelope, plan, _ = fixture()
                envelope["author_handoff"][field] = value
                with self.assertRaises(PeerReviewError):
                    validate_workflow_feedback(lease, envelope, plan_context=plan)
        for defect in ("missing", "extra", "null"):
            with self.subTest(defect=defect):
                lease, envelope, plan, _ = fixture()
                if defect == "missing":
                    envelope["author_handoff"].pop("original_snapshot_sha256")
                elif defect == "extra":
                    envelope["author_handoff"]["approval_granted"] = True
                else:
                    envelope["author_handoff"] = None
                with self.assertRaises(PeerReviewError):
                    validate_workflow_feedback(lease, envelope, plan_context=plan)

    def test_handoff_does_not_relax_plan_or_source_bindings(self):
        for field in ("plan_attempt_id", "plan_sha256"):
            with self.subTest(field=field):
                lease, envelope, plan, _ = fixture()
                plan[field] = str(uuid4()) if field == "plan_attempt_id" else "0" * 64
                with self.assertRaises(PeerReviewError):
                    validate_workflow_feedback(lease, envelope, plan_context=plan)
        lease, envelope, plan, _ = fixture()
        lease["task_id"] = str(uuid4())
        envelope["author_handoff"]["task_id"] = lease["task_id"]
        with self.assertRaises(PeerReviewError):
            validate_workflow_feedback(lease, envelope, plan_context=plan)

    def test_later_codex_authored_feedback_uses_unchanged_default_path(self):
        lease, envelope, plan, _ = fixture(author="codex-author")
        envelope.pop("author_handoff")
        ready = validate_workflow_feedback(lease, envelope, plan_context=plan)
        self.assertIsNone(ready.author_handoff)
        self.assertEqual(ready.content, envelope["verdict_json"].encode())

    def test_linked_child_keeps_original_parent_and_approved_plan(self):
        lease, envelope, plan, _ = fixture()
        lease["parent_task_id"] = lease["task_id"]
        lease["task_id"] = str(uuid4())
        envelope["author_handoff"]["task_id"] = lease["task_id"]
        ready = validate_workflow_feedback(lease, envelope, plan_context=plan)
        self.assertEqual(ready.content, envelope["verdict_json"].encode())
        lease["parent_task_id"] = str(uuid4())
        with self.assertRaises(PeerReviewError):
            validate_workflow_feedback(lease, envelope, plan_context=plan)

    def test_redundant_handoff_and_author_reviewer_collision_fail_closed(self):
        for defect in ("redundant", "reviewer"):
            with self.subTest(defect=defect):
                lease, envelope, plan, _ = fixture()
                if defect == "redundant":
                    lease["agent_profile"] = "agy-primary"
                    envelope["author_handoff"]["target_author_profile"] = "agy-primary"
                else:
                    lease["agent_profile"] = "codex-primary"
                    envelope["author_handoff"]["target_author_profile"] = "codex-primary"
                with self.assertRaises(PeerReviewError):
                    validate_workflow_feedback(lease, envelope, plan_context=plan)




class BuildAuthorHandoffContextTest(unittest.TestCase):
    def test_partial_native_repair_keeps_current_source_and_original_feedback(self):
        with TemporaryDirectory(prefix="forge-handoff-native-context-") as root:
            lease, workspace, envelope = build_fixture(root)
            original_packet = deepcopy(envelope)
            self.assertNotEqual(workspace.digest(), envelope["feedback"]["workspace_digest"])
            self.assertNotEqual(lease["resume_attempt_id"], envelope["feedback"]["parent_attempt_id"])
            store = Mock()
            store.read_feedback.return_value = envelope
            ready = ReviewContextProvider(store).acquire(lease, workspace)
            self.assertIsInstance(ready.author_handoff, BuildAuthorHandoffContext)
            self.assertEqual(ready.content, envelope["feedback_json"].encode())
            self.assertEqual(ready.sha256, lease["feedback_sha256"])
            self.assertEqual(ready.author_handoff.resume_workspace_digest, workspace.digest())
            self.assertEqual(envelope, original_packet)
            store.read_feedback.assert_called_once_with(lease)

    def test_partial_linked_candidate_uses_native_resume_digest_without_changing_old_source_digest(self):
        with TemporaryDirectory(prefix="forge-handoff-candidate-context-") as root:
            lease, workspace, envelope = build_fixture(root, kind="candidate_tree")
            self.assertNotEqual(workspace.digest(), workspace.source_digest())
            self.assertNotEqual(workspace.source_digest(), envelope["feedback"]["workspace_digest"])
            ready = validate_context(lease, workspace, envelope)
            self.assertEqual(ready.content, envelope["feedback_json"].encode())
            self.assertEqual(ready.author_handoff.feedback_workspace_digest, envelope["feedback"]["workspace_digest"])
            lease["repair_candidate_commit"] = "0" * 40
            with self.assertRaises(ReviewContextError):
                validate_context(lease, workspace, envelope)

    def test_no_change_repair_still_requires_handoff_for_original_author_and_parent(self):
        with TemporaryDirectory(prefix="forge-handoff-unchanged-context-") as root:
            lease, workspace, envelope = build_fixture(root, partial=False)
            self.assertEqual(workspace.digest(), envelope["feedback"]["workspace_digest"])
            self.assertIsNotNone(validate_context(lease, workspace, envelope))
            envelope.pop("author_handoff")
            with self.assertRaises(ReviewContextError):
                validate_context(lease, workspace, envelope)

    def test_source_changes_after_stopped_handoff_receipt_are_rejected(self):
        with TemporaryDirectory(prefix="forge-handoff-stale-source-") as root:
            lease, workspace, envelope = build_fixture(root)
            (workspace.path / "source.txt").write_text("unreconciled source\n")
            with self.assertRaises(ReviewContextError):
                validate_context(lease, workspace, envelope)

    def test_packet_plan_and_original_hash_remain_strict(self):
        for defect in ("plan", "packet-bytes", "packet-value"):
            with self.subTest(defect=defect), TemporaryDirectory(prefix="forge-handoff-packet-") as root:
                lease, workspace, envelope = build_fixture(root)
                if defect == "plan":
                    lease["plan_decision_id"] = str(uuid4())
                elif defect == "packet-bytes":
                    envelope["feedback_json"] += " "
                else:
                    envelope["feedback"]["summary"] = "A replacement instruction."
                with self.assertRaises(ReviewContextError):
                    validate_context(lease, workspace, envelope)

    def test_wrong_packet_source_attempt_workspace_or_actor_proof_is_rejected(self):
        for field in ("workflow_id", "task_id", "attempt_id", "action_id", "source_author_profile",
            "target_author_profile", "feedback_id", "feedback_sha256", "parent_attempt_id",
            "resume_attempt_id", "resume_workspace_digest", "feedback_workspace_digest"):
            with self.subTest(field=field), TemporaryDirectory(prefix="forge-handoff-proof-") as root:
                lease, workspace, envelope = build_fixture(root)
                envelope["author_handoff"][field] = ("not-an-action" if field == "action_id" else
                    "unrelated-profile" if field.endswith("profile") else
                    "0" * 64 if field.endswith(("sha256", "digest")) else str(uuid4()))
                with self.assertRaises(ReviewContextError):
                    validate_context(lease, workspace, envelope)

    def test_untyped_null_redundant_or_unexpected_proof_is_rejected(self):
        for defect in ("null", "version", "extra", "redundant", "no-packet"):
            with self.subTest(defect=defect), TemporaryDirectory(prefix="forge-handoff-proof-schema-") as root:
                lease, workspace, envelope = build_fixture(root)
                if defect == "null":
                    envelope["author_handoff"] = None
                elif defect == "version":
                    envelope["author_handoff"]["version"] = True
                elif defect == "extra":
                    envelope["author_handoff"]["approval_granted"] = True
                elif defect == "redundant":
                    lease["agent_profile"] = envelope["feedback"]["agent_profile"]
                    envelope["author_handoff"]["target_author_profile"] = lease["agent_profile"]
                else:
                    envelope.update(feedback=None, feedback_json=None)
                    lease.update(feedback_id=None, feedback_sha256=None)
                with self.assertRaises(ReviewContextError):
                    validate_context(lease, workspace, envelope)


if __name__ == "__main__":
    unittest.main()
