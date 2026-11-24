"""Peer review uses the exact admitted artifact and a fresh read-only scope."""

from copy import deepcopy
import hashlib
import json
from pathlib import Path
import stat
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import Mock

from test_work_executor import lease, raw_config
from test_work_peer_review import bind, target, verdict
from work_executor import CommandRunnerProvider, ExecutionError, SystemdSupervisor, WorkspaceLease
from work_peer_context import PeerReviewContextProvider, validate_peer_context, validate_workflow_feedback
from work_review_context import ReviewContext
from uuid import uuid4
from work_peer_review import PeerReviewError
from work_worker_config import parse_config
from work_worker import CodingWorkerFactory, _lease
from work_executor import WorkspaceProvider, _git
from test_work_merge import merge_fixture
from work_peer_context import PeerReviewCandidateSourceProvider


def fixture(root, kind="plan"):
    config = parse_config(raw_config(root))
    value = lease(config, mode="review")
    path = Path(root) / "source"
    path.mkdir()
    (path / "source.txt").write_text("review this source\n")
    workspace = WorkspaceLease(path, config.projects["homelab"], Path(root) / "git", value["task_id"], value["base_revision"])
    subject = target(kind)
    subject.update(reviewer_task_id=value["task_id"], reviewer_profile=value["agent_profile"],
                   author_profile="separate-author", base_revision=value["base_revision"])
    if kind == "code":
        subject.update(candidate_commit=value["base_revision"], workspace_digest=workspace.digest())
        for row in subject["verification"]["checks"]:
            row["head_sha"] = value["base_revision"]
    raw = json.dumps(subject, indent=2, ensure_ascii=False)
    digest = hashlib.sha256(raw.encode()).hexdigest()
    value.update(workflow_id=subject["workflow_id"], workflow_review_id=subject["workflow_review_id"], review_target_sha256=digest)
    return config, value, workspace, {"target_json": raw, "target_sha256": digest}


class PeerContextTest(unittest.TestCase):
    def test_native_digest_stays_byte_identical_while_source_omits_only_root_git_file(self):
        with TemporaryDirectory(prefix="forge-digest-compat-") as root:
            path = Path(root)
            (path / ".git").write_text("gitdir: /approved/worktrees/fixture\n")
            (path / "source.txt").write_text("source\n")
            workspace = WorkspaceLease(path, path, path / "metadata", str(uuid4()), "a" * 40)
            self.assertEqual(workspace.digest(), "4a6aa9942ac96b177c303f1fec5780d113957e01b42a2e3898467c48fec9c358")
            normalized = workspace.source_digest()
            (path / ".git").write_text("gitdir: /another/worktrees/path\n")
            self.assertEqual(workspace.source_digest(), normalized)
            self.assertNotEqual(workspace.digest(), "4a6aa9942ac96b177c303f1fec5780d113957e01b42a2e3898467c48fec9c358")
            (path / "generated.ignored").write_text("still source-accounted\n")
            self.assertNotEqual(workspace.source_digest(), normalized)
    def test_normal_code_reviewer_materializes_exact_candidate_without_changing_main(self):
        with merge_fixture() as (publication, job, _provider, _client, candidates, _hosted):
            config = publication.worker
            manifest = candidates.read(job["review_id"])
            subject = target("code")
            reviewer = lease(config, backend="codex", mode="review")
            reviewer["base_revision"] = job["candidate_commit"]
            subject.update(reviewer_task_id=reviewer["task_id"], author_profile="agy", reviewer_profile="codex",
                source_task_id=job["task_id"], source_attempt_id=manifest["attempt_id"],
                base_revision=job["candidate_commit"], candidate_commit=job["candidate_commit"],
                review_id=job["review_id"], gate_manifest_sha256=job["gate_manifest_sha256"],
                workspace_digest=manifest["candidate_workspace_digest"], integration_base=manifest["integration_base"])
            for row in subject["verification"]["checks"]:
                row["head_sha"] = job["candidate_commit"]
            raw = json.dumps(subject)
            digest = hashlib.sha256(raw.encode()).hexdigest()
            reviewer.update(workflow_id=subject["workflow_id"], workflow_review_id=subject["workflow_review_id"],
                            review_target_sha256=digest)
            canonical = config.projects["homelab"]
            original_main = _git(canonical, "rev-parse", "HEAD")
            store = Mock()
            store.read_review_target.return_value = {"target_json": raw, "target_sha256": digest}
            contexts = PeerReviewContextProvider(store, PeerReviewCandidateSourceProvider(config, _git))
            contexts.prepare_source(reviewer)
            workspaces = WorkspaceProvider(config)
            workspaces.prepare()
            with workspaces.acquire(reviewer) as workspace:
                ready = contexts.acquire(reviewer, workspace)
                self.assertEqual(ready.sha256, digest)
                self.assertEqual(workspace.source_digest(), manifest["candidate_workspace_digest"])
                self.assertNotEqual(workspace.digest(), workspace.source_digest())
            self.assertEqual(_git(canonical, "rev-parse", "HEAD"), original_main)
            self.assertEqual(_git(candidates.directory(job["review_id"]) / "repository", "rev-parse", "HEAD").decode().strip(),
                             job["candidate_commit"])

    def test_author_receives_full_verdict_separately_from_original_plan(self):
        with TemporaryDirectory(prefix="forge-author-feedback-") as root:
            config, value, workspace, _ = fixture(root)
            subject = target()
            subject.update(source_task_id=value["task_id"], author_profile=value["agent_profile"],
                           reviewer_profile="independent-critic", base_revision=value["base_revision"])
            parsed = bind(subject)
            response = verdict(parsed, "revise")
            response["findings"] = [{"finding_id": "full-finding-" + str(i), "category": "source",
                                      "summary": ("Complete finding detail. " * 60)} for i in range(6)]
            raw = json.dumps(response, indent=2)
            self.assertGreater(len(raw), 8192)
            for field in ("workflow_review_id", "review_target_sha256"):
                value.pop(field)
            value.update(mode="plan", workflow_id=subject["workflow_id"],
                workflow_feedback_review_id=subject["workflow_review_id"],
                workflow_feedback_sha256=hashlib.sha256(raw.encode()).hexdigest())
            value["recipe"]["mode"] = "plan"
            context = {"decision_id": str(uuid4()), "decision": "revise", "text": subject["source_text"],
                "plan_attempt_id": subject["source_attempt_id"], "plan_sha256": subject["source_result_sha256"], "note": "Short summary."}
            value["plan_decision_id"] = context["decision_id"]
            store = Mock()
            store.read_workflow_feedback.return_value = {"target_json": json.dumps(subject),
                "target_sha256": parsed["_context_sha256"], "verdict_json": raw,
                "verdict_sha256": value["workflow_feedback_sha256"]}
            # bind() uses ordinary json.dumps with ensure_ascii=False; this
            # fixture contains ASCII and therefore preserves those exact bytes.
            ready = PeerReviewContextProvider(store).acquire_feedback(value, plan_context=context)
            runner = CommandRunnerProvider(config, SystemdSupervisor(config))
            runner.prepare()
            invocation = runner.invocation(value, workspace, plan_context=context, peer_feedback_context=ready)
            self.assertEqual(invocation.plan_file.read_text(), subject["source_text"])
            artifact = invocation.readonly_files[0]
            self.assertEqual(artifact.name, "agent-review-feedback.json")
            self.assertEqual(artifact.read_text(), raw)
            self.assertEqual(stat.S_IMODE(artifact.stat().st_mode), 0o400)
            for changed in ("hash", "source", "missing"):
                envelope = deepcopy(store.read_workflow_feedback.return_value)
                bad_context = deepcopy(context)
                if changed == "hash":
                    envelope["verdict_json"] += " "
                elif changed == "source":
                    bad_context["plan_attempt_id"] = str(uuid4())
                else:
                    envelope = None
                with self.subTest(changed=changed), self.assertRaises(PeerReviewError):
                    validate_workflow_feedback(value, envelope, plan_context=bad_context)

    def test_approved_plan_survives_a_later_native_resume_receipt(self):
        with TemporaryDirectory(prefix="forge-plan-resume-") as root:
            _config, value, _workspace, _ = fixture(root)
            subject = target()
            subject.update(source_task_id=value["task_id"], author_profile=value["agent_profile"],
                           reviewer_profile="independent-critic", base_revision=value["base_revision"])
            parsed = bind(subject)
            raw = json.dumps(verdict(parsed, "approve"))
            value.update(mode="implement", workflow_id=subject["workflow_id"],
                workflow_feedback_review_id=subject["workflow_review_id"],
                workflow_feedback_sha256=hashlib.sha256(raw.encode()).hexdigest(), resume_attempt_id=str(uuid4()))
            value.pop("workflow_review_id")
            value.pop("review_target_sha256")
            context = {"plan_attempt_id": subject["source_attempt_id"], "plan_sha256": subject["source_result_sha256"]}
            envelope = {"target_json": json.dumps(subject), "target_sha256": parsed["_context_sha256"],
                        "verdict_json": raw, "verdict_sha256": value["workflow_feedback_sha256"]}
            self.assertEqual(validate_workflow_feedback(value, envelope, plan_context=context).content, raw.encode())
            # A linked candidate may have a new Git base while preserving the
            # exact originally approved plan and its independent verdict.
            linked = {**value, "task_id": str(uuid4()), "parent_task_id": subject["source_task_id"],
                "repair_source_review_id": str(uuid4()), "feedback_id": str(uuid4()), "feedback_sha256": "e" * 64,
                "repair_candidate_commit": "c" * 40, "base_revision": "c" * 40}
            self.assertEqual(validate_workflow_feedback(linked, envelope, plan_context=context).content, raw.encode())

    def test_linked_code_author_feedback_requires_original_plan_and_exact_parent(self):
        with TemporaryDirectory(prefix="forge-code-feedback-") as root:
            config, value, _workspace, _ = fixture(root)
            subject = target("code")
            subject.update(author_profile=value["agent_profile"], reviewer_profile="independent-critic")
            parsed = bind(subject)
            response = json.dumps(verdict(parsed, "revise"))
            value.update(mode="implement", parent_task_id=subject["source_task_id"],
                workflow_id=subject["workflow_id"], workflow_feedback_review_id=subject["workflow_review_id"],
                workflow_feedback_sha256=hashlib.sha256(response.encode()).hexdigest(), repair_source_review_id=subject["review_id"])
            value.pop("workflow_review_id")
            value.pop("review_target_sha256")
            previous = {"parent_attempt_id": subject["source_attempt_id"],
                "parent_result_sha256": subject["source_result_sha256"], "candidate_commit": subject["candidate_commit"],
                "gate_manifest_sha256": subject["gate_manifest_sha256"]}
            old_raw = json.dumps(previous).encode()
            feedback = ReviewContext(old_raw, hashlib.sha256(old_raw).hexdigest(), str(uuid4()), ())
            context = {"plan_attempt_id": subject["approved_plan"]["attempt_id"],
                       "plan_sha256": subject["approved_plan"]["sha256"]}
            envelope = {"target_json": json.dumps(subject), "target_sha256": parsed["_context_sha256"],
                        "verdict_json": response, "verdict_sha256": value["workflow_feedback_sha256"]}
            ready = validate_workflow_feedback(value, envelope, plan_context=context, feedback_context=feedback)
            self.assertEqual(ready.content, response.encode())
            for field in ("parent_task_id", "repair_source_review_id"):
                changed = {**value, field: str(uuid4())}
                with self.subTest(field=field), self.assertRaises(PeerReviewError):
                    validate_workflow_feedback(changed, envelope, plan_context=context, feedback_context=feedback)

    def test_linked_child_later_same_task_code_revision_uses_current_review(self):
        with TemporaryDirectory(prefix="forge-child-feedback-") as root:
            _config, value, _workspace, _ = fixture(root)
            subject = target("code")
            subject.update(source_task_id=value["task_id"], author_profile=value["agent_profile"],
                           reviewer_profile="independent-critic")
            parsed = bind(subject)
            response = json.dumps(verdict(parsed, "revise"))
            parent = str(uuid4())
            original_parent_review = str(uuid4())
            self.assertNotEqual(original_parent_review, subject["review_id"])
            value.update(mode="implement", parent_task_id=parent,
                workflow_id=subject["workflow_id"], workflow_feedback_review_id=subject["workflow_review_id"],
                workflow_feedback_sha256=hashlib.sha256(response.encode()).hexdigest(),
                repair_source_review_id=original_parent_review)
            value.pop("workflow_review_id")
            value.pop("review_target_sha256")
            previous = {"parent_attempt_id": subject["source_attempt_id"],
                "parent_result_sha256": subject["source_result_sha256"], "candidate_commit": subject["candidate_commit"],
                "gate_manifest_sha256": subject["gate_manifest_sha256"]}
            content = json.dumps(previous).encode()
            feedback = ReviewContext(content, hashlib.sha256(content).hexdigest(), str(uuid4()), ())
            plan = {"plan_attempt_id": subject["approved_plan"]["attempt_id"],
                    "plan_sha256": subject["approved_plan"]["sha256"]}
            envelope = {"target_json": json.dumps(subject), "target_sha256": parsed["_context_sha256"],
                        "verdict_json": response, "verdict_sha256": value["workflow_feedback_sha256"]}
            store = Mock()
            store.read_workflow_feedback.return_value = envelope
            provider = PeerReviewContextProvider(store)
            self.assertEqual(provider.acquire_feedback(value, plan_context=plan, feedback_context=feedback).content,
                             response.encode())
            # A child retains its original parent/repair-source identity while a
            # later review is bound to the child itself. Ordinary same-task
            # feedback with no parent also retains its existing semantics.
            ordinary = {**value, "parent_task_id": None, "repair_source_review_id": None}
            self.assertEqual(provider.acquire_feedback(ordinary, plan_context=plan, feedback_context=feedback).content,
                             response.encode())
            for field in ("parent_attempt_id", "parent_result_sha256", "candidate_commit", "gate_manifest_sha256"):
                changed = {**previous, field: str(uuid4()) if field == "parent_attempt_id" else "0" * len(previous[field])}
                damaged = json.dumps(changed).encode()
                altered = ReviewContext(damaged, hashlib.sha256(damaged).hexdigest(), feedback.feedback_id, ())
                with self.subTest(field=field), self.assertRaises(PeerReviewError):
                    provider.acquire_feedback(value, plan_context=plan, feedback_context=altered)

    def test_exact_context_is_bound_readonly_without_author_session_reuse(self):
        with TemporaryDirectory(prefix="forge-peer-context-") as root:
            config, value, workspace, envelope = fixture(root, "code")
            store, sessions = Mock(), Mock()
            store.read_review_target.return_value = envelope
            context = PeerReviewContextProvider(store).acquire(value, workspace)
            runner = CommandRunnerProvider(config, SystemdSupervisor(config), sessions=sessions)
            runner.prepare()
            invocation = runner.invocation(value, workspace, peer_context=context)
            sessions.select.assert_not_called()
            self.assertIsNone(invocation.session)
            self.assertFalse(invocation.implement)
            self.assertNotIn("--conversation", invocation.argv)
            artifact = invocation.readonly_files[0]
            self.assertEqual(artifact.name, "agent-review.json")
            self.assertEqual(artifact.read_bytes(), envelope["target_json"].encode())
            self.assertEqual(stat.S_IMODE(artifact.stat().st_mode), 0o400)
            prompt = json.loads(invocation.stdin)["message"]["content"]
            self.assertIn("Return ONLY a JSON object", prompt)
            self.assertIn(value["review_target_sha256"], prompt)
            properties = runner.supervisor.argv(value, invocation)
            self.assertTrue(any(str(artifact) + ":/run/forge-coding-attempt/agent-review.json" in arg
                                for arg in properties if "BindReadOnlyPaths=" in arg))

    def test_missing_stale_wrong_scope_and_wrong_source_fail_before_execution(self):
        for case in ("missing", "hash", "task", "profile", "workflow", "base", "source", "mode"):
            with self.subTest(case=case), TemporaryDirectory(prefix="forge-peer-reject-") as root:
                config, value, workspace, envelope = fixture(root, "code")
                changed = deepcopy(envelope)
                if case == "missing":
                    changed = None
                elif case == "hash":
                    changed["target_json"] += " "
                elif case == "task":
                    value["task_id"] = "00000000-0000-4000-8000-000000000001"
                elif case == "profile":
                    value["agent_profile"] = "codex"
                elif case == "workflow":
                    value["workflow_id"] = "00000000-0000-4000-8000-000000000001"
                elif case == "base":
                    value["base_revision"] = "f" * 40
                elif case == "source":
                    (workspace.path / "source.txt").write_text("changed\n")
                else:
                    value["mode"] = "implement"
                with self.assertRaises(PeerReviewError):
                    validate_peer_context(value, workspace, changed)
                runner = CommandRunnerProvider(config, SystemdSupervisor(config))
                runner.prepare()
                with self.assertRaises(ExecutionError):
                    runner.invocation(value, workspace)

    def test_normal_factory_injects_selected_provider_and_lease_requires_context(self):
        with TemporaryDirectory(prefix="forge-peer-factory-") as root:
            config, value, _workspace, _envelope = fixture(root)
            store, runner, workspaces, supervisor, selected = Mock(), Mock(), Mock(), Mock(), Mock()
            provider = Mock(return_value=selected)
            worker = CodingWorkerFactory(config, lambda _config: store, runner_factory=lambda *_: runner,
                workspace_factory=lambda _: workspaces, supervisor_factory=lambda _: supervisor,
                peer_context_factory=provider).create()
            self.assertIs(worker.peer_contexts, selected)
            provider.assert_called_once_with(store)
            self.assertEqual(_lease(value)["workflow_review_id"], value["workflow_review_id"])
            for key in ("workflow_id", "review_target_sha256"):
                invalid = dict(value)
                invalid[key] = None
                with self.assertRaises(ValueError):
                    _lease(invalid)
