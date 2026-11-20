"""Fake-child/parser tests; root separately verifies real systemd isolation."""

from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import time
from tempfile import TemporaryDirectory
import threading
import unittest
from unittest.mock import Mock, patch
from uuid import uuid4

from work_executor import (
    CliParser, CommandRunnerProvider, ExecutionError, SystemdSupervisor,
    WorkspaceLease, WorkspaceProvider, unit_name, utf8_chunks,
)
from work_policy import POLICY, POLICY_DIGEST, POLICY_ID
from work_worker_config import parse_config, load_config, verify_recipe


def raw_config(root):
    root = Path(root)
    return {
        "worker_id": "fixture-worker", "owner": "fixture-owner",
        "database": {"host": str(root / "db-socket"), "port": 5432,
                     "name": "forge_agent", "user": "forge_coding_worker"},
        "account": {"user": "lrocha", "uid": 1000, "gid": 1000, "home": str(root / "home")},
        "workspace_root": str(root / "workspaces"), "state_root": str(root / "state"),
        "projects": {"homelab": str(root / "repository")},
        "profiles": {
            "agy": {"backend": "antigravity", "model": "fixture-model", "modes": ["plan", "review", "implement"]},
            "codex": {"backend": "codex", "model": "fixture-model", "modes": ["plan", "review", "implement"]},
        },
        "binaries": {"antigravity": "/usr/bin/python3", "codex": "/usr/bin/python3"},
    }


def lease(config, *, backend="antigravity", mode="plan"):
    attempt = str(uuid4())
    return {
        "task_id": str(uuid4()), "project_alias": "homelab", "base_revision": "a" * 40,
        "agent_profile": "agy" if backend == "antigravity" else "codex", "mode": mode,
        "prompt": "Read the fixture. $(do-not-run)", "acceptance_criteria": "Return the fixture report.",
        "recipe": {"version": 2, "backend": backend, "model": "fixture-model",
                   "mode": mode, "resource_profile": POLICY_ID, "generation_token_limit": None,
                   "policy_digest": POLICY_DIGEST},
        "attempt_id": attempt, "attempt_token": str(uuid4()), "worker_id": config.worker_id,
        "incarnation": str(uuid4()), "lease_expires_at": "2026-11-19T19:00:00Z",
        "deadline_at": "2026-11-19T19:30:00Z", "unit_name": unit_name(attempt),
        "resume_workspace_digest": None,
        "resume_attempt_id": None,
    }


def proof(name=None, stopped=True):
    return {"unit_name": name or "forge-coding-" + "a" * 32, "scope_stopped": stopped,
            "source": "systemd", "checked_at": "2026-11-19T19:00:00Z"}


def agy_result(text="Fixture result", *, conversation=None, **changes):
    result = {"conversation_id": conversation or str(uuid4()), "status": "SUCCESS", "response": text,
              "usage": {"input_tokens": 10, "output_tokens": 20, "total_tokens": 30}, "denied_actions": []}
    result.update(changes)
    return {"event": "result", "result": result}


class WorkerConfigTest(unittest.TestCase):
    def test_configuration_is_strict_frozen_and_effect_free(self):
        raw = raw_config("/tmp/forge-configuration-fixture")
        with patch("pathlib.Path.open", side_effect=AssertionError("configuration parsing must be pure")):
            config = parse_config(raw)
        raw["projects"]["homelab"] = "/changed"
        raw["profiles"]["agy"]["modes"].clear()
        self.assertNotEqual(config.projects["homelab"], Path("/changed"))
        self.assertEqual(config.profiles["agy"].modes, ("plan", "review", "implement"))
        self.assertEqual(config.advertised_profiles()[0]["modes"], ["plan", "review"])
        self.assertIsNone(config.database.passfile)
        self.assertEqual(config.database.connection_arguments(reconciler=True)["user"], "forge_coding_reconciler")
        with self.assertRaises(TypeError):
            config.projects["other"] = "/other"
        for mutate in (
            lambda raw: raw.update(command="unsafe"),
            lambda raw: raw["account"].update(uid=0),
            lambda raw: raw.update(workspace_root=raw["projects"]["homelab"] + "/build"),
            lambda raw: raw.update(state_root="/tmp/unsafe space"),
            lambda raw: raw.update(state_root="/tmp/unsafe%h"),
            lambda raw: raw["database"].update(password="secret"),
            lambda raw: raw["profiles"]["agy"].update(modes=["plan", "plan"]),
        ):
            raw = raw_config("/tmp/forge-configuration-fixture")
            mutate(raw)
            with self.assertRaises(ValueError):
                parse_config(raw)

    def test_secret_or_malformed_configuration_never_appears_in_errors(self):
        with TemporaryDirectory(prefix="forge-coding-config-") as root:
            path = Path(root) / "worker.json"
            path.write_text('{"secret":"DO_NOT_REPORT","secret":"duplicate"}')
            with self.assertRaises(ValueError) as failure:
                load_config(str(path))
            self.assertNotIn("DO_NOT_REPORT", str(failure.exception))
        with self.assertRaises(ValueError):
            load_config("relative.json")

    def test_recipe_policy_profile_and_mode_are_independent_grants(self):
        config = parse_config(raw_config("/tmp/forge-config-fixture"))
        value = lease(config)
        self.assertTrue(verify_recipe(config, value))
        value["recipe"]["version"] = 1
        self.assertFalse(verify_recipe(config, value))
        value = lease(config, mode="implement")
        self.assertFalse(verify_recipe(config, value))
        raw = raw_config("/tmp/forge-config-fixture")
        raw["allow_implement"] = True
        self.assertTrue(verify_recipe(parse_config(raw), value))
        value["recipe"]["policy_digest"] = "b" * 64
        self.assertFalse(verify_recipe(parse_config(raw), value))

    def test_elevated_policy_is_versioned_and_refuses_previous_recipes(self):
        previous_digest = "2465fea4c303e83825339fd15793927a80b147519586519283e7534b2ad3be44"
        self.assertEqual(POLICY_ID, "forge-coding-v6")
        self.assertEqual(POLICY["antigravity_tool_policy"], "workspace-elevated-v1")
        self.assertNotEqual(POLICY_DIGEST, previous_digest)
        config = parse_config(raw_config("/tmp/forge-config-fixture"))
        self.assertTrue(verify_recipe(config, lease(config)))
        for resource, digest in (
            ("forge-coding-v1", POLICY_DIGEST),
            (POLICY_ID, previous_digest),
            ("forge-coding-v1", previous_digest),
            ("forge-coding-v2", "67065fb04466901c8c0df1faad5d7a04503322c6e4c1b8a75ca08eba1375fa58"),
            ("forge-coding-v3", "e619dbab071b1d1b0e3fa0cf6e6212e879c953825d6c80da5f4de048fb787075"),
        ):
            with self.subTest(resource=resource, digest=digest):
                previous = lease(config)
                previous["recipe"].update(resource_profile=resource, policy_digest=digest)
                self.assertFalse(verify_recipe(config, previous))


class CliParserTest(unittest.TestCase):
    def test_agy_chunked_ndjson_accepts_only_coherent_nonempty_owned_terminal_response(self):
        text = "é🛠\\\n" * 20_000
        event = agy_result(text)
        encoded = json.dumps(event, ensure_ascii=False).encode() + b"\n"
        parser = CliParser("antigravity")
        for start in range(0, len(encoded), 37):
            parser.feed(encoded[start:start + 37])
        result = parser.finish(0, proof())
        self.assertEqual(result.status, "awaiting_review")
        self.assertEqual(result.text, text)
        self.assertEqual(result.usage["total_tokens"], 30)
        chunks = list(utf8_chunks(result.text))
        self.assertEqual("".join(chunks), text)
        self.assertTrue(all(len(part.encode()) <= 16_384 for part in chunks))
        self.assertGreater(len(text.encode()), 65_536)
        self.assertEqual(hashlib.sha256("".join(chunks).encode()).digest(), hashlib.sha256(text.encode()).digest())

    def test_denied_empty_failed_or_unstopped_agy_never_becomes_ready(self):
        for event, exitcode, stopped, expected in (
            (agy_result(""), 0, True, "failed"),
            (agy_result(denied_actions=[{"action": "command"}]), 0, True, "blocked_permission"),
            (agy_result(status="ERROR", error={"code": "quota_exhausted"}), 0, True, "quota_blocked"),
            (agy_result(), 1, True, "failed"),
            (agy_result(), 0, False, "needs_reconcile"),
        ):
            parser = CliParser("antigravity")
            parser.feed(json.dumps(event).encode())
            self.assertEqual(parser.finish(exitcode, proof(stopped=stopped)).status, expected)
        parser = CliParser("antigravity")
        parser.feed(json.dumps(agy_result()).encode() + b"\n")
        parser.feed(b"jetski: no output produced - a permission that headless mode cannot prompt for", stderr=True)
        self.assertEqual(parser.finish(0, proof()).status, "blocked_permission")

    def test_specific_quota_and_auth_failures_survive_empty_output_diagnostics(self):
        for code, expected in (("quota_exhausted", "quota_blocked"),
                               ("authentication_required", "blocked_auth")):
            with self.subTest(code=code, source="terminal"):
                parser = CliParser("antigravity")
                parser.feed(json.dumps(agy_result("", status="ERROR", error={"code": code})).encode())
                parser.feed(b"jetski: no output produced", stderr=True)
                self.assertEqual(parser.finish(1, proof()).status, expected)
            with self.subTest(code=code, source="stderr"):
                parser = CliParser("antigravity")
                parser.feed((code + "; jetski: no output produced").encode(), stderr=True)
                self.assertEqual(parser.finish(1, proof()).status, expected)

    def test_malformed_duplicate_and_cross_conversation_outputs_reject(self):
        for raw in (b'{"event":"result","event":"x"}\n', b'not json\n', b'{"x":NaN}\n'):
            with self.assertRaises(ExecutionError):
                CliParser("antigravity").feed(raw)
        parser = CliParser("antigravity")
        parser.feed(json.dumps({"event": "step_update", "step_update": {"conversation_id": str(uuid4())}}).encode() + b"\n")
        with self.assertRaises(ExecutionError):
            parser.feed(json.dumps(agy_result()).encode() + b"\n")
        parser = CliParser("antigravity")
        record = json.dumps(agy_result()).encode() + b"\n"
        parser.feed(record)
        with self.assertRaises(ExecutionError):
            parser.feed(record)

    def test_codex_accepts_final_assistant_and_per_turn_usage_not_tool_logs(self):
        conversation = str(uuid4())
        events = [
            {"type": "thread.started", "thread_id": conversation},
            {"type": "item.completed", "item": {"type": "command_execution", "status": "completed",
                                                   "aggregated_output": "not the final response"}},
            {"type": "item.completed", "item": {"type": "agent_message", "text": "Final fixture reply"}},
            {"type": "turn.completed", "usage": {"input_tokens": 7, "output_tokens": 9, "cached_input_tokens": 3}},
        ]
        parser = CliParser("codex")
        for event in events:
            parser.feed(json.dumps(event).encode() + b"\n")
        result = parser.finish(0, proof())
        self.assertEqual((result.text, result.conversation_id, result.status),
                         ("Final fixture reply", conversation, "awaiting_review"))
        self.assertEqual(result.usage["input_tokens"], 7)
        parser = CliParser("codex")
        parser.feed(json.dumps({"type": "turn.failed", "error": {"message": "HTTP 429 backend busy"}}).encode() + b"\n")
        self.assertEqual(parser.finish(1, proof()).status, "failed", "429 alone is not proven exhausted quota")

    def test_stream_and_result_resource_limits_fail_explicitly(self):
        with patch("work_executor.POLICY", {"max_stream_bytes": 64, "max_result_bytes": 16}):
            with self.assertRaises(ExecutionError):
                CliParser("codex").feed(b"x" * 65)
        parser = CliParser("antigravity")
        parser.feed(json.dumps(agy_result("x" * 100)).encode() + b"\n")
        with patch("work_executor.POLICY", {"max_result_bytes": 16}):
            self.assertEqual(parser.finish(0, proof()).status, "failed")


class SupervisorContractTest(unittest.TestCase):
    def test_fixed_root_launcher_drops_uid_caps_before_any_model_code(self):
        with TemporaryDirectory(prefix="forge-supervisor-") as root:
            raw = raw_config(root)
            raw["database"]["passfile"] = str(Path(root) / "parent-secret")
            config = parse_config(raw, source_path=str(Path(root) / "worker-config.json"))
            provider = CommandRunnerProvider(config, SystemdSupervisor(config))
            provider.prepare()
            value = lease(config)
            workspace = WorkspaceLease(Path(root) / "task", config.projects["homelab"],
                                       config.projects["homelab"] / ".git/worktrees/fixture", value["task_id"], "a" * 40)
            invocation = provider.invocation(value, workspace)
            command = provider.supervisor.argv(value, invocation)
            self.assertIn("--expand-environment=no", command)
            self.assertIn("--property=Type=exec", command)
            self.assertIn("/usr/bin/unshare", command)
            self.assertLess(command.index("/usr/bin/unshare"), command.index("/usr/bin/setpriv"))
            self.assertLess(command.index("/usr/bin/setpriv"), command.index("/usr/bin/env"))
            self.assertIn("--no-new-privs", command)
            self.assertIn("--clear-groups", command)
            self.assertIn("--bounding-set=-all", command)
            self.assertIn("--reuid=1000", command)
            self.assertIn("--property=KillMode=control-group", command)
            self.assertIn("--property=KillSignal=SIGTERM", command)
            self.assertIn("--property=FinalKillSignal=SIGKILL", command)
            self.assertIn("--property=SendSIGKILL=yes", command)
            masked = next(arg for arg in command if arg.startswith("--property=InaccessiblePaths="))
            self.assertIn(str(config.database.host), masked)
            self.assertIn("/run/user/1000/bus", masked)
            self.assertIn(str(config.database.passfile), masked)
            self.assertNotIn(value["prompt"], command)
            self.assertNotIn("--continue", command)
            self.assertIn(value["prompt"], invocation.stdin.decode())
            settings = json.loads((invocation.scratch / "settings.json").read_text())
            self.assertFalse(settings["useG1Credits"])
            self.assertIn("write_file(*)", settings["permissions"]["deny"])
            self.assertIn("command(*)", settings["permissions"]["allow"])
            self.assertEqual(settings["permissions"]["ask"], [])
            codex = provider.invocation(lease(config, backend="codex"), workspace)
            self.assertIn("--no-daemon", codex.argv)
            self.assertIn("--ignore-user-config", codex.argv)
            self.assertIn("read-only", codex.argv)
            self.assertEqual(codex.argv, (str(Path("/usr/bin/python3").resolve()), "--no-daemon", "--ask-for-approval", "never", "exec",
                                         "--ignore-user-config", "--ignore-rules", "--sandbox", "read-only",
                                         "--model", "fixture-model", "--json", "--color", "never", "--ephemeral", "-"))
            self.assertFalse((codex.scratch / "report_progress.py").exists())

    def test_elevated_agy_tools_keep_mode_scoped_writes_and_unavailable_tools_denied(self):
        with TemporaryDirectory(prefix="forge-elevated-tools-") as root:
            raw = raw_config(root)
            raw["allow_implement"] = True
            config = parse_config(raw)
            provider = CommandRunnerProvider(config, SystemdSupervisor(config))
            provider.prepare()
            for mode in ("plan", "review", "implement"):
                with self.subTest(mode=mode):
                    value = lease(config, mode=mode)
                    workspace = WorkspaceLease(Path(root) / "task", config.projects["homelab"],
                                               config.projects["homelab"] / ".git/worktrees/fixture",
                                               value["task_id"], "a" * 40)
                    invocation = provider.invocation(value, workspace)
                    settings = json.loads((invocation.scratch / "settings.json").read_text())
                    permissions = settings["permissions"]
                    self.assertFalse(settings["useG1Credits"])
                    self.assertEqual(permissions["ask"], [])
                    for rule in ("command(*)", "read_url(*)",
                                 "read_file(/run/forge-coding-workspace)",
                                 "read_file(/run/forge-coding-attempt)"):
                        self.assertIn(rule, permissions["allow"])
                    self.assertIn("mcp(*)", permissions["deny"])
                    self.assertIn("execute_url(*)", permissions["deny"])
                    self.assertNotIn("--dangerously-skip-permissions", invocation.argv)
                    self.assertIn("MCP and browser actuation are not provisioned", invocation.stdin.decode())
                    writes = sorted(rule for rule in permissions["allow"] if rule.startswith("write_file("))
                    if mode == "implement":
                        self.assertEqual(writes, ["write_file(/run/forge-coding-attempt)",
                                                  "write_file(/run/forge-coding-workspace)"])
                        self.assertNotIn("write_file(*)", permissions["deny"])
                        self.assertIn("accept-edits", invocation.argv)
                    else:
                        self.assertEqual(writes, [])
                        self.assertIn("write_file(*)", permissions["deny"])
                        self.assertIn("plan", invocation.argv)

    def test_stopped_proof_requires_inactive_unit_and_empty_descendant_cgroup(self):
        with TemporaryDirectory(prefix="forge-cgroup-") as root:
            config = parse_config(raw_config(root))
            unit = "forge-coding-" + "b" * 32
            group = Path(root) / "cgroups/system.slice" / (unit + ".service")
            group.mkdir(parents=True)
            events = group / "cgroup.events"
            events.write_text("populated 1\nfrozen 0\n")
            command = Mock(return_value=subprocess.CompletedProcess([], 0,
                b"LoadState=loaded\nActiveState=inactive\nMainPID=0\nControlGroup=/system.slice/" + unit.encode() + b".service\n"))
            supervisor = SystemdSupervisor(config, command=command, cgroup_root=Path(root) / "cgroups")
            self.assertFalse(supervisor.proof(unit)["scope_stopped"])
            events.write_text("populated 0\nfrozen 0\n")
            self.assertTrue(supervisor.proof(unit)["scope_stopped"])
            command.side_effect = subprocess.TimeoutExpired("systemctl", 10)
            self.assertFalse(supervisor.proof(unit)["scope_stopped"])
            with self.assertRaises(ValueError):
                supervisor.stop("postgresql.service")

    def test_absent_unit_stop_is_confirmed_only_with_no_remaining_cgroup(self):
        with TemporaryDirectory(prefix="forge-absent-scope-") as root:
            config = parse_config(raw_config(root))
            unit = "forge-coding-" + "b" * 32
            command = Mock(return_value=subprocess.CompletedProcess([], 4,
                b"LoadState=not-found\nActiveState=inactive\nMainPID=0\nControlGroup=\n"))
            supervisor = SystemdSupervisor(config, command=command, cgroup_root=Path(root) / "cgroups")
            self.assertTrue(supervisor.proof(unit)["scope_stopped"])
            group = Path(root) / "cgroups/system.slice" / (unit + ".service")
            group.mkdir(parents=True)
            (group / "cgroup.events").write_text("populated 1\n")
            self.assertFalse(supervisor.proof(unit)["scope_stopped"])


class WorkspaceProviderTest(unittest.TestCase):
    def test_isolated_worktree_preserves_dirty_state_and_requires_matching_trusted_resume_digest(self):
        with TemporaryDirectory(prefix="forge-workspace-") as root:
            config = parse_config(raw_config(root))
            repo = config.projects["homelab"]
            repo.mkdir()
            def git(*args):
                return subprocess.run(["/usr/bin/git", "-C", str(repo), *args], check=True,
                                      capture_output=True).stdout.decode().strip()
            git("init", "--quiet")
            git("config", "user.name", "Fixture")
            git("config", "user.email", "fixture@localhost")
            (repo / "file.txt").write_text("original")
            git("add", "file.txt")
            git("commit", "--quiet", "-m", "fixture")
            value = lease(config)
            value["base_revision"] = git("rev-parse", "HEAD")
            provider = WorkspaceProvider(config)
            provider.prepare()
            with provider.acquire(value) as workspace:
                self.assertNotEqual(workspace.path, repo)
                (workspace.path / "file.txt").write_text("reviewed pending changes")
                digest = workspace.digest()
                with self.assertRaises(ExecutionError):
                    with provider.inspect(value):
                        pass
            self.assertEqual((repo / "file.txt").read_text(), "original")
            with self.assertRaises(ExecutionError):
                with provider.acquire(value):
                    pass
            resumed = {**value, "attempt_id": str(uuid4()), "resume_workspace_digest": digest}
            with provider.acquire(resumed) as workspace:
                self.assertEqual((workspace.path / "file.txt").read_text(), "reviewed pending changes")
                (workspace.path / "extra.txt").write_text("new drift")
            with self.assertRaises(ExecutionError):
                with provider.acquire(resumed):
                    pass
            with provider.inspect(value) as workspace:
                self.assertNotEqual(workspace.digest(), digest)


class FakeSupervisor:
    """Fake process-group supervision, not evidence of systemd isolation."""
    def __init__(self, script):
        self.script, self.process = script, None

    def argv(self, _lease, _invocation):
        return ["/usr/bin/python3", "-c", self.script.replace("__SCRATCH__", repr(str(_invocation.scratch)))]

    def stop(self, unit):
        if self.process is not None and self.process.poll() is None:
            os.killpg(self.process.pid, signal.SIGTERM)
            self.process.wait(timeout=5)
        return proof(unit)


class FakeChildTest(unittest.TestCase):
    def _run(self, script, stop=None, report_progress=None):
        with TemporaryDirectory(prefix="forge-coding-child-") as root:
            config = parse_config(raw_config(root))
            value = lease(config)
            supervisor = FakeSupervisor(script)
            def popen(*args, **kwargs):
                supervisor.process = subprocess.Popen(*args, **kwargs)
                return supervisor.process
            provider = CommandRunnerProvider(config, supervisor, popen=popen)
            provider.prepare()
            workspace = WorkspaceLease(Path(root) / "task", config.projects["homelab"], Path(root) / "git",
                                       value["task_id"], value["base_revision"])
            return provider.run(value, workspace, stop or threading.Event(), report_progress=report_progress)

    def test_fake_child_public_plan_and_progress_persist_during_execution_and_final_flush(self):
        with TemporaryDirectory(prefix="forge-progress-live-ack-") as root:
            acknowledgement = Path(root) / "plan-persisted"
            reports = []
            script = ("import sys,json,uuid,subprocess,pathlib,time\nsys.stdin.read()\n"
                      "helper=str(pathlib.Path(__SCRATCH__)/'report_progress.py')\n"
                      "subprocess.run(['/usr/bin/python3',helper,'plan','Read and test'],check=True)\n"
                      "deadline=time.monotonic()+5\n"
                      f"while not pathlib.Path({str(acknowledgement)!r}).exists():\n"
                      " if time.monotonic()>deadline: raise RuntimeError('No live progress callback')\n"
                      " time.sleep(.01)\n"
                      "subprocess.run(['/usr/bin/python3',helper,'progress','Tests passed'],check=True)\n"
                      "print(json.dumps({'event':'result','result':{'status':'SUCCESS',"
                      "'conversation_id':str(uuid.uuid4()),'response':'Final public result'}}))")
            def persisted(*record):
                reports.append(record)
                acknowledgement.write_text("persisted")
            result = self._run(script, report_progress=persisted)
            self.assertEqual(result.status, "awaiting_review")
            self.assertEqual(reports, [(1, "plan", "Read and test"), (2, "progress", "Tests passed")])

    def test_fake_child_missing_plan_malformed_report_or_uncertain_sink_cannot_publish_success(self):
        terminal = ("print(json.dumps({'event':'result','result':{'status':'SUCCESS',"
                    "'conversation_id':str(uuid.uuid4()),'response':'Final public result'}}))")
        base = "import sys,json,uuid,pathlib,subprocess;sys.stdin.read();"
        result = self._run(base + terminal, report_progress=lambda *_: None)
        self.assertEqual(result.status, "failed")
        self.assertIn("public plan", result.summary)
        result = self._run(base + "(pathlib.Path(__SCRATCH__)/'progress/000001.json').symlink_to('/etc/passwd');" + terminal,
                           report_progress=lambda *_: None)
        self.assertEqual(result.status, "failed")
        def unavailable(*_):
            raise ConnectionError("private connection detail")
        before = time.monotonic()
        result = self._run(base + "subprocess.run(['/usr/bin/python3',str(pathlib.Path(__SCRATCH__)/'report_progress.py'),"
                           "'plan','Public plan'],check=True);import time;time.sleep(30);" + terminal,
                           report_progress=unavailable)
        self.assertEqual(result.status, "needs_reconcile")
        self.assertNotIn("private", result.summary)
        self.assertLess(time.monotonic() - before, 5, "Uncertain persistence must stop the still-running child")

    def test_fake_child_drains_both_pipes_and_preserves_large_final_reply(self):
        result = self._run("import sys,json,uuid; sys.stdin.read(); "
                           "sys.stderr.write('safe warning\\n'*1000); "
                           "print(json.dumps({'event':'result','result':{'status':'SUCCESS',"
                           "'conversation_id':str(uuid.uuid4()),'response':'x'*100000,'usage':{}}}))")
        self.assertEqual((result.status, len(result.text)), ("awaiting_review", 100000))

    def test_fake_child_malformed_and_cancellation_never_publish_success(self):
        result = self._run("import sys;sys.stdin.read();print('malformed')")
        self.assertEqual(result.status, "failed")
        stop = threading.Event()
        timer = threading.Timer(.15, stop.set)
        timer.start()
        try:
            result = self._run("import time;time.sleep(30)", stop)
            self.assertEqual(result.status, "cancelled")
        finally:
            timer.cancel()


if __name__ == "__main__":
    unittest.main()
