"""Session-selection through the real adapter with owned fake Python children."""

import json
from pathlib import Path
import subprocess
from tempfile import TemporaryDirectory
import threading
import unittest
from unittest.mock import Mock
from uuid import uuid4

from test_work_executor import FakeSupervisor, lease, raw_config
from work_executor import CommandRunnerProvider, WorkspaceLease, unit_name
from work_sessions import AntigravitySessionProvider
from work_worker_config import parse_config


class ConversationSupervisor(FakeSupervisor):
    def __init__(self):
        super().__init__("")
        self.invocations = []
        self.fail = False

    def argv(self, value, invocation):
        self.invocations.append(invocation)
        conversation = invocation.session.conversation_id or str(uuid4())
        result = {"event": "result", "result": {
            "conversation_id": conversation, "status": "SUCCESS",
            "response": "Fixture final result", "usage": {
                "input_tokens": 100, "output_tokens": 20, "cached_tokens": 80, "total_tokens": 120},
        }}
        text = "not-json" if self.fail else json.dumps(result)
        return ["/usr/bin/python3", "-c", "import sys;sys.stdin.read();print(" + repr(text) + ")"]


class SessionExecutionTest(unittest.TestCase):
    def setup_provider(self, root):
        config = parse_config(raw_config(root))
        value = lease(config)
        path = Path(root) / "task"
        path.mkdir()
        (path / "source.txt").write_text("Fixture workspace")
        workspace = WorkspaceLease(path, config.projects["homelab"], Path(root) / "git",
                                   value["task_id"], value["base_revision"])
        supervisor = ConversationSupervisor()
        def popen(*args, **kwargs):
            supervisor.process = subprocess.Popen(*args, **kwargs)
            return supervisor.process
        sessions = AntigravitySessionProvider(config)
        provider = CommandRunnerProvider(config, supervisor, sessions=sessions, popen=popen)
        provider.prepare()
        return value, workspace, supervisor, provider

    def continued(self, value, workspace):
        attempt = str(uuid4())
        return {**value, "attempt_id": attempt, "attempt_token": str(uuid4()),
                "resume_attempt_id": value["attempt_id"],
                "unit_name": unit_name(attempt), "resume_workspace_digest": workspace.digest()}

    def test_explicit_followup_pins_conversation_and_never_attributes_cumulative_usage(self):
        with TemporaryDirectory(prefix="forge-session-execution-") as root:
            value, workspace, supervisor, provider = self.setup_provider(root)
            first = provider.run(value, workspace, threading.Event())
            self.assertEqual(first.status, "awaiting_review")
            self.assertTrue(first.fresh_session)
            self.assertNotIn("--conversation", supervisor.invocations[0].argv)
            continued = self.continued(value, workspace)
            resumed = provider.run(continued, workspace, threading.Event())
            self.assertEqual(resumed.status, "awaiting_review")
            self.assertFalse(resumed.fresh_session)
            self.assertEqual(resumed.conversation_id, first.conversation_id)
            invocation = supervisor.invocations[-1]
            self.assertEqual(invocation.argv[-2:], ("--conversation", first.conversation_id))
            self.assertNotIn("--continue", invocation.argv)
            self.assertNotIn("--resume", invocation.argv)
            self.assertNotIn("-p", invocation.argv)
            for flag in ("--input-format", "--output-format"):
                self.assertEqual(invocation.argv[invocation.argv.index(flag) + 1], "stream-json")
            self.assertEqual(invocation.argv[invocation.argv.index("--print-timeout") + 1], "14400s")
            self.assertNotIn(value["prompt"], invocation.argv)
            self.assertIn(value["prompt"], json.loads(invocation.stdin)["message"]["content"])
            settings = json.loads((invocation.scratch / "settings.json").read_text())
            self.assertFalse(settings["useG1Credits"])

    def test_failed_or_unconfirmed_execution_cannot_reuse_an_older_hint(self):
        with TemporaryDirectory(prefix="forge-session-failed-") as root:
            value, workspace, supervisor, provider = self.setup_provider(root)
            first = provider.run(value, workspace, threading.Event())
            self.assertEqual(first.status, "awaiting_review")
            supervisor.fail = True
            failed = provider.run(self.continued(value, workspace), workspace, threading.Event())
            self.assertEqual(failed.status, "failed")
            self.assertFalse(failed.fresh_session)
            self.assertFalse((provider.sessions.directory / (value["task_id"] + ".json")).exists())
            fresh = provider.invocation(self.continued(value, workspace), workspace)
            self.assertNotIn("--conversation", fresh.argv)

    def test_optional_hint_write_failure_preserves_coherent_result(self):
        with TemporaryDirectory(prefix="forge-session-hint-failure-") as root:
            value, workspace, supervisor, provider = self.setup_provider(root)
            provider.sessions.remember = Mock(side_effect=OSError("private filename"))
            with self.assertLogs("forge-coding-worker", level="WARNING") as messages:
                result = provider.run(value, workspace, threading.Event())
            self.assertEqual(result.status, "awaiting_review")
            self.assertNotIn("private filename", " ".join(messages.output))


if __name__ == "__main__":
    unittest.main()
