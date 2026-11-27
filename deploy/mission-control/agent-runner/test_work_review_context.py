"""Feedback is current leased data, never a replacement plan or shell source."""

import hashlib
import json
from pathlib import Path
import stat
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import Mock
from uuid import uuid4

from test_work_executor import lease, raw_config
from test_work_plan_execution import context as plan_context
from work_executor import CHILD_STATE, CommandRunnerProvider, ExecutionError, SystemdSupervisor, WorkspaceLease
from work_review_context import ReviewContextError, ReviewContextProvider, validate_context
from work_worker_config import parse_config


def fixture(root):
    raw = raw_config(root)
    raw["allow_implement"] = True
    config = parse_config(raw)
    value = lease(config, mode="implement")
    path = Path(root) / "task"
    path.mkdir()
    (path / "source.txt").write_text("approved source\n")
    workspace = WorkspaceLease(path, config.projects["homelab"], Path(root) / "git",
                               value["task_id"], value["base_revision"])
    value["resume_attempt_id"] = str(uuid4())
    value["resume_workspace_digest"] = workspace.digest()
    feedback = {"version": 1, "feedback_id": str(uuid4()), "task_id": value["task_id"],
        "parent_attempt_id": value["resume_attempt_id"], "source_revision": 5,
        "parent_result_sha256": "b" * 64, "reconciliation_receipt_id": str(uuid4()),
        "workspace_digest": workspace.digest(), "candidate_commit": "c" * 40,
        "integration_base": value["base_revision"], "gate_manifest_sha256": "d" * 64,
        "round": 1, "disposition": "repair", "agent_profile": value["agent_profile"],
        "plan_decision_id": None, "summary": "Fix the verified fixture failure.",
        "findings": [{"finding_id": "fixture-assertion", "category": "source",
                      "summary": "Expected three; observed two.", "check_id": "fixture-test"}]}
    encoded = json.dumps(feedback, indent=2, ensure_ascii=False)
    value["feedback_id"] = feedback["feedback_id"]
    value["feedback_sha256"] = hashlib.sha256(encoded.encode()).hexdigest()
    envelope = {"feedback": feedback, "feedback_json": encoded, "legacy_resume_note": ""}
    return config, value, workspace, envelope


class ReviewContextTest(unittest.TestCase):
    def test_candidate_tree_feedback_survives_no_edit_native_reconciliation(self):
        with TemporaryDirectory(prefix="forge-candidate-recovery-") as root:
            _config, value, workspace, envelope = fixture(root)
            (workspace.path / ".git").write_text("gitdir: /approved/worktrees/child\n")
            value.update(parent_task_id=str(uuid4()), repair_source_review_id=str(uuid4()),
                         repair_candidate_commit=value["base_revision"], resume_attempt_id=str(uuid4()),
                         resume_workspace_digest=workspace.digest())
            envelope["feedback"].update(workspace_digest_kind="candidate_tree", candidate_commit=value["base_revision"],
                                        workspace_digest=workspace.source_digest())
            def bind_packet():
                envelope["feedback_json"] = json.dumps(envelope["feedback"])
                value["feedback_sha256"] = hashlib.sha256(envelope["feedback_json"].encode()).hexdigest()
            bind_packet()
            self.assertNotEqual(value["resume_attempt_id"], envelope["feedback"]["parent_attempt_id"])
            self.assertIsNotNone(validate_context(value, workspace, envelope))
            envelope["feedback"]["workspace_digest_kind"] = "native_receipt"
            bind_packet()
            with self.assertRaises(ReviewContextError):
                validate_context(value, workspace, envelope)
            envelope["feedback"]["workspace_digest_kind"] = "candidate_tree"
            bind_packet()
            (workspace.path / "source.txt").write_text("partial unrelated source edit\n")
            with self.assertRaises(ReviewContextError):
                validate_context(value, workspace, envelope)

    def test_linked_clone_input_then_native_child_receipt_use_distinct_comparisons(self):
        with TemporaryDirectory(prefix="forge-feedback-digest-") as root:
            _config, value, workspace, envelope = fixture(root)
            (workspace.path / ".git").write_text("gitdir: /approved/parent/worktrees/child\n")
            value.update(parent_task_id=str(uuid4()), repair_source_review_id=str(uuid4()),
                         repair_candidate_commit=value["base_revision"])
            for candidate, expected in ((value["base_revision"], workspace.source_digest()),
                                        ("d" * 40, workspace.digest())):
                feedback = envelope["feedback"]
                feedback.update(candidate_commit=candidate, workspace_digest=expected)
                value["resume_workspace_digest"] = expected
                envelope["feedback_json"] = json.dumps(feedback)
                value["feedback_sha256"] = hashlib.sha256(envelope["feedback_json"].encode()).hexdigest()
                self.assertIsNotNone(validate_context(value, workspace, envelope))
            self.assertNotEqual(workspace.digest(), workspace.source_digest())

    def test_exact_sql_json_bytes_are_hash_bound_and_mounted_readonly(self):
        with TemporaryDirectory(prefix="forge-feedback-") as root:
            config, value, workspace, envelope = fixture(root)
            store = Mock()
            store.read_feedback.return_value = envelope
            ready = ReviewContextProvider(store).acquire(value, workspace)
            runner = CommandRunnerProvider(config, SystemdSupervisor(config))
            runner.prepare()
            invocation = runner.invocation(value, workspace, feedback_context=ready)
            path = invocation.readonly_files[0]
            self.assertEqual(path.read_bytes(), envelope["feedback_json"].encode())
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o400)
            prompt = json.loads(invocation.stdin)["message"]["content"]
            self.assertIn(str(CHILD_STATE / "review-feedback.json"), prompt)
            self.assertIn(value["feedback_sha256"], prompt)
            self.assertIn("fixture-assertion", prompt)
            self.assertNotIn(envelope["feedback_json"], prompt)
            argv = runner.supervisor.argv(value, invocation)
            self.assertTrue(any(str(path) + ":" + str(CHILD_STATE / path.name) in arg
                                for arg in argv if arg.startswith("--property=BindReadOnlyPaths=")))
            store.read_feedback.assert_called_once_with(value)

    def test_stale_source_missing_context_wrong_identity_or_digest_prevent_execution(self):
        for changed in ("source", "digest", "task", "missing", "json-mismatch"):
            with self.subTest(changed=changed), TemporaryDirectory(prefix="forge-feedback-reject-") as root:
                config, value, workspace, envelope = fixture(root)
                if changed == "source":
                    (workspace.path / "source.txt").write_text("changed")
                elif changed == "digest":
                    value["feedback_sha256"] = "e" * 64
                elif changed == "task":
                    value["task_id"] = str(uuid4())
                elif changed == "missing":
                    envelope = None
                else:
                    envelope["feedback"]["summary"] = "Different parsed value"
                with self.assertRaises(ReviewContextError):
                    validate_context(value, workspace, envelope)
                runner = CommandRunnerProvider(config, SystemdSupervisor(config))
                runner.prepare()
                with self.assertRaises(ExecutionError):
                    runner.invocation(value, workspace)

    def test_latest_resume_note_reaches_prompt_without_replacing_original_plan(self):
        with TemporaryDirectory(prefix="forge-resume-feedback-") as root:
            config, value, workspace, _ = fixture(root)
            value.pop("feedback_id")
            value.pop("feedback_sha256")
            saved = plan_context()
            value["plan_decision_id"] = saved["decision_id"]
            note = "Fix the new failure; $(this-is-literal-data)."
            ready = validate_context(value, workspace, {
                "feedback": None, "feedback_json": None, "legacy_resume_note": note})
            runner = CommandRunnerProvider(config, SystemdSupervisor(config))
            runner.prepare()
            invocation = runner.invocation(value, workspace, plan_context=saved, feedback_context=ready)
            prompt = json.loads(invocation.stdin)["message"]["content"]
            self.assertIn(saved["note"], prompt)
            self.assertIn(note, prompt)
            self.assertEqual(invocation.plan_file.read_text(), saved["text"])
            self.assertNotIn(note, invocation.argv)

    def test_approved_plan_does_not_replay_its_prior_phase_resume_direction(self):
        with TemporaryDirectory(prefix="forge-prior-phase-feedback-") as root:
            config,value,workspace,_=fixture(root)
            value.pop('feedback_id');value.pop('feedback_sha256')
            saved=plan_context();value['plan_decision_id']=saved['decision_id']
            value['resume_attempt_id']=saved['plan_attempt_id']
            note='Produce a fresh Plan and wait for independent review. Do not implement yet.'
            ready=validate_context(value,workspace,{'feedback':None,'feedback_json':None,'legacy_resume_note':note})
            runner=CommandRunnerProvider(config,SystemdSupervisor(config));runner.prepare()
            invocation=runner.invocation(value,workspace,plan_context=saved,feedback_context=ready)
            prompt=json.loads(invocation.stdin)['message']['content']
            self.assertIn('Implement the approved plan',prompt)
            self.assertNotIn(note,prompt)
            self.assertNotIn('OWNER RESUME FEEDBACK',prompt)
            self.assertFalse(any(p.name=='review-feedback.json' for p in invocation.readonly_files))
            self.assertEqual(ready.legacy_resume_note,note)
            from work_review_context import current_phase_context,ReviewContext
            for changed,plan in (({**value,'mode':'plan'},saved),
                                 ({**value,'resume_attempt_id':str(uuid4())},saved),
                                 (value,{**saved,'decision':'revise'}),
                                 (value,{**saved,'decision_id':str(uuid4())})):
                self.assertIs(current_phase_context(changed,ready,plan),ready)
            repair=ReviewContext(b'bound repair','a'*64,str(uuid4()),('finding-1',),note)
            self.assertIs(current_phase_context(value,repair,saved),repair)

    def test_expired_note_must_still_pass_artifact_integrity_checks(self):
        from dataclasses import replace
        with TemporaryDirectory(prefix='forge-expired-note-integrity-') as root:
            config,value,workspace,_=fixture(root)
            value.pop('feedback_id');value.pop('feedback_sha256')
            saved=plan_context();value['plan_decision_id']=saved['decision_id']
            value['resume_attempt_id']=saved['plan_attempt_id']
            ready=validate_context(value,workspace,{'feedback':None,'feedback_json':None,'legacy_resume_note':'Return a fresh Plan.'})
            runner=CommandRunnerProvider(config,SystemdSupervisor(config));runner.prepare()
            with self.assertRaises(ExecutionError) as error:
                runner.invocation(value,workspace,plan_context=saved,feedback_context=replace(ready,sha256='0'*64))
            self.assertIn('feedback changed',error.exception.summary)

    def test_secret_feedback_and_planted_artifact_are_rejected(self):
        with TemporaryDirectory(prefix="forge-feedback-secret-") as root:
            config, value, workspace, envelope = fixture(root)
            envelope["legacy_resume_note"] = "ghp_" + "aB0123456789" * 3
            with self.assertRaises(ReviewContextError):
                validate_context(value, workspace, envelope)
            envelope["legacy_resume_note"] = ""
            ready = validate_context(value, workspace, envelope)
            runner = CommandRunnerProvider(config, SystemdSupervisor(config))
            runner.prepare()
            scratch = config.state_root / "attempts" / value["attempt_id"]
            scratch.mkdir()
            protected = Path(root) / "protected"
            protected.write_text("preserve")
            (scratch / "review-feedback.json").symlink_to(protected)
            with self.assertRaises(ExecutionError):
                runner.invocation(value, workspace, feedback_context=ready)
            self.assertEqual(protected.read_text(), "preserve")


if __name__ == "__main__":
    unittest.main()
