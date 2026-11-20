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
