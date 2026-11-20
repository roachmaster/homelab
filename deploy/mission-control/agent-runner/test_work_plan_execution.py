"""Approved-plan data and publication handoff stay with the trusted parent."""

import hashlib
import json
from pathlib import Path
import stat
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import Mock
from uuid import uuid4

import test_work_worker as worker_fixtures
from test_work_executor import lease, raw_config
from work_executor import (CHILD_STATE, CHILD_WORKSPACE, CommandRunnerProvider,
                           ExecutionError, Invocation, SystemdSupervisor, WorkspaceLease)
from work_worker_config import parse_config


def context(decision="resume"):
    text = "# Approved fixture plan\n\nImplement and verify the fixture."
    return {"decision_id": str(uuid4()), "decision": decision,
            "plan_attempt_id": str(uuid4()), "plan_sha256": hashlib.sha256(text.encode()).hexdigest(),
            "text": text, "note": "Use the reviewed scope. $(literal-feedback)"}


class PlanExecutionTest(unittest.TestCase):
    def configured(self, root, mode="implement"):
        raw = raw_config(root)
        raw["allow_implement"] = True
        config = parse_config(raw)
        value = lease(config, mode=mode)
        workspace = WorkspaceLease(Path(root) / "task", config.projects["homelab"],
                                   Path(root) / "git", value["task_id"], value["base_revision"])
        runner = CommandRunnerProvider(config, SystemdSupervisor(config))
        runner.prepare()
        return value, workspace, runner

    def test_exact_saved_artifact_is_readonly_and_prompt_contains_current_decision(self):
        for mode, decision, filename in (("implement", "resume", "approved-plan.md"),
                                         ("plan", "revise", "previous-plan.md")):
            with self.subTest(mode=mode), TemporaryDirectory(prefix="forge-plan-context-") as root:
                value, workspace, runner = self.configured(root, mode)
                saved = context(decision)
                value["plan_decision_id"] = saved["decision_id"]
                invocation = runner.invocation(value, workspace, plan_context=saved)
                self.assertEqual(invocation.plan_file.read_text(), saved["text"])
                self.assertEqual(stat.S_IMODE(invocation.plan_file.stat().st_mode), 0o400)
                body = json.loads(invocation.stdin)["message"]["content"]
                self.assertIn("EXPLICIT PLAN DECISION: " + decision.upper(), body)
                self.assertIn(str(CHILD_STATE / filename), body)
                self.assertIn(saved["note"], body)
                self.assertNotIn(saved["text"], body, "Large plans stay out of the prompt envelope")
                self.assertNotIn(saved["note"], invocation.argv, "Review feedback is data, never shell arguments")
                command = runner.supervisor.argv(value, invocation)
                readonly = next(arg for arg in command if arg.startswith("--property=BindReadOnlyPaths="))
                self.assertIn(str(invocation.plan_file) + ":" + str(CHILD_STATE / filename), readonly)

    def test_missing_or_wrong_plan_never_prepares_a_runnable_invocation(self):
        for field, changed in (("text", "Different plan"), ("decision_id", str(uuid4())),
                               ("decision", "revise"), ("plan_attempt_id", "invalid")):
            with self.subTest(field=field), TemporaryDirectory(prefix="forge-plan-reject-") as root:
                value, workspace, runner = self.configured(root)
                saved = context()
                value["plan_decision_id"] = saved["decision_id"]
                saved[field] = changed
                with self.assertRaises(ExecutionError):
                    runner.invocation(value, workspace, plan_context=saved)
        with TemporaryDirectory(prefix="forge-plan-missing-") as root:
            value, workspace, runner = self.configured(root)
            value["plan_decision_id"] = str(uuid4())
            with self.assertRaises(ExecutionError):
                runner.invocation(value, workspace)

    def test_plan_file_cannot_overwrite_a_preexisting_symlink(self):
        with TemporaryDirectory(prefix="forge-plan-file-") as root:
            value, workspace, runner = self.configured(root)
            saved = context()
            value["plan_decision_id"] = saved["decision_id"]
            scratch = runner.config.state_root / "attempts" / value["attempt_id"]
            scratch.mkdir()
            protected = Path(root) / "protected"
            protected.write_text("Preserve these bytes")
            (scratch / "approved-plan.md").symlink_to(protected)
            with self.assertRaises(ExecutionError):
                runner.invocation(value, workspace, plan_context=saved)
            self.assertEqual(protected.read_text(), "Preserve these bytes")

    def test_parent_loads_plan_before_cli_and_queues_publication_only_after_reconciliation(self):
        for mode, decision in (("implement", "resume"), ("plan", "revise")):
            with self.subTest(mode=mode), TemporaryDirectory(prefix="forge-plan-handoff-") as root:
                worker, value, store, events = worker_fixtures.WorkerLifecycleTest()._worker(root)
                raw = raw_config(root)
                raw["allow_implement"] = True
                worker.config = parse_config(raw)
                value = lease(worker.config, mode=mode)
                value["incarnation"] = worker.incarnation
                saved = context(decision)
                value["plan_decision_id"] = saved["decision_id"]
                store.read_plan = Mock(side_effect=lambda lease: (events.append("read-plan") or saved))
                store.queue_publication = Mock(side_effect=lambda *args: events.append("queue-publication"))
                worker.process(value)
                self.assertEqual(worker.runner.plan_contexts, [saved])
                self.assertLess(events.index("read-plan"), events.index("runner"))
                if mode == "implement":
                    self.assertLess(events.index("publish"), events.index("reconcile"))
                    self.assertLess(events.index("reconcile"), events.index("queue-publication"))
                    store.queue_publication.assert_called_once_with(value, "fixture-receipt")
                else:
                    store.queue_publication.assert_not_called()

    def test_unavailable_plan_has_zero_cli_or_publication_effects(self):
        with TemporaryDirectory(prefix="forge-plan-unavailable-") as root:
            worker, value, store, events = worker_fixtures.WorkerLifecycleTest()._worker(root)
            value["plan_decision_id"] = str(uuid4())
            store.read_plan = Mock(side_effect=RuntimeError("private DB detail"))
            store.queue_publication = Mock()
            worker.process(value)
            self.assertNotIn("runner", events)
            store.queue_publication.assert_not_called()
            self.assertEqual(store.publications[0][1], "needs_reconcile")
            self.assertNotIn("private DB detail", store.publications[0][4])

    def test_verification_scope_has_no_native_auth_and_git_metadata_is_readonly(self):
        with TemporaryDirectory(prefix="forge-verification-scope-") as root:
            value, workspace, runner = self.configured(root)
            scratch = runner.config.state_root / "attempts" / value["attempt_id"]
            scratch.mkdir()
            invocation = Invocation(("/usr/bin/python3", "-V"), b"", "verification",
                                    workspace.path, scratch, workspace, True)
            command = runner.supervisor.argv(value, invocation)
            self.assertIn("--property=PrivateNetwork=yes", command)
            binds = next(arg for arg in command if arg.startswith("--property=BindPaths="))
            self.assertNotIn(str(runner.config.account.home / ".gemini"), binds)
            self.assertNotIn(str(runner.config.account.home / ".codex"), binds)
            self.assertNotIn(str(workspace.git_directory), binds)
            readonly = next(arg for arg in command if arg.startswith("--property=BindReadOnlyPaths="))
            self.assertIn(str(workspace.git_directory) + ":" + str(CHILD_WORKSPACE / ".git"), readonly)
            self.assertIn("HOME=" + str(CHILD_STATE / "home"), command)
            self.assertFalse(any(arg.startswith(("GH_TOKEN=", "GITHUB_TOKEN=")) for arg in command))
            native = runner.supervisor.argv(value, Invocation(
                ("/usr/bin/python3", "-V"), b"", "antigravity",
                workspace.path, scratch, workspace, True,
            ))
            self.assertNotIn("--property=PrivateNetwork=yes", native)


if __name__ == "__main__":
    unittest.main()
