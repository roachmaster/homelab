import json
from pathlib import Path
import subprocess
from tempfile import TemporaryDirectory
import unittest

import agent_tools
from queue_store import Rejected
from runner import tool_submission


class AgentToolTest(unittest.TestCase):
    def setUp(self):
        temporary = TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.git("init", "-q")
        (self.root / "README.md").write_text("pinned\nsecond line\n")
        (self.root / "link.txt").symlink_to("README.md")
        self.git("add", "README.md", "link.txt")
        self.git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "fixture")
        self.revision = self.git("rev-parse", "HEAD").strip()
        (self.root / "README.md").write_text("a later unstaged edit\n")

    def git(self, *arguments):
        return subprocess.check_output(["git", "-C", str(self.root), *arguments], text=True)

    def test_snapshot_uses_the_pinned_regular_files(self):
        snapshot = agent_tools.Snapshot(self.root, self.revision)
        listing = snapshot.run(agent_tools.ToolCall("list", "list_files", {}))
        self.assertEqual(listing, {"text": "README.md", "truncated": False})
        result = snapshot.run(agent_tools.ToolCall("read", "read_file", {"path": "README.md", "start_line": 2}))
        self.assertEqual(result, {"text": "second line", "start_line": 2, "truncated": False})

    def test_tool_result_continues_the_matching_model_call(self):
        tool_reply = '{"tool_call":{"id":"readme","name":"read_file","arguments":{"path":"README.md"}}}'
        events = []

        def generate(request):
            if len(request["messages"]) == 2:
                return tool_reply, {}, "stop"
            self.assertEqual(json.loads(request["messages"][-2]["content"]), json.loads(tool_reply))
            tool_result = json.loads(request["messages"][-1]["content"])["tool_result"]
            self.assertEqual((tool_result["id"], tool_result["text"]), ("readme", "pinned\nsecond line"))
            return "The pinned file has two lines.", {"completion_tokens": 3}, "stop"

        reply = agent_tools.complete(
            generate, {"messages": [{"role": "user", "content": "Describe the readme."}]},
            agent_tools.Snapshot(self.root, self.revision),
            lambda call: events.append(("started", call.id)),
            lambda call, _: events.append(("finished", call.id)),
        )
        self.assertEqual(reply[0], "The pinned file has two lines.")
        self.assertEqual(events, [("started", "readme"), ("finished", "readme")])

    def test_admission_and_tool_schema_reject_unapproved_input(self):
        body = {"repository": "fixture", "revision": self.revision,
                "messages": [{"role": "user", "content": "Describe the readme."}]}
        command = tool_submission(body, "request-1", {"fixture": self.root})
        self.assertEqual((command.tool_repository, command.tool_revision), ("fixture", self.revision))
        with self.assertRaisesRegex(Rejected, "repository_not_found"):
            tool_submission(body, "request-1", {})
        with self.assertRaisesRegex(Rejected, "invalid_revision"):
            tool_submission(dict(body, revision="HEAD"), "request-1", {"fixture": self.root})
        for path in ("../README.md", "/README.md", ".git/config", "src//x"):
            with self.subTest(path=path), self.assertRaisesRegex(agent_tools.ToolError, "invalid_tool_call"):
                agent_tools.model_call(json.dumps({"tool_call": {"id": "id", "name": "read_file",
                                                                "arguments": {"path": path}}}))
