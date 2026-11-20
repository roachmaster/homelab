"""Owned Python child regressions for the normal CLI provider lifecycle.

The supervisor returns a separate scope proof without controlling the launcher,
so a successful cleanup assertion must come from the provider's own launcher
handling. These fixtures invoke no agy, systemd, database or account service.
"""

from contextlib import contextmanager
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
from tempfile import TemporaryDirectory
import threading
import time
import unittest
from unittest.mock import patch
from uuid import uuid4

from test_work_executor import agy_result, lease, proof, raw_config
from work_executor import CliParser, CommandRunnerProvider, ExecutionError, WorkspaceLease
from work_process import terminate_and_reap
from work_worker_config import parse_config


class ScopeProofSupervisor:
    """Confirms only its separate scope; never stops the local fixture child."""

    def __init__(self, script):
        self.script = script
        self.argv_calls = 0
        self.stop_calls = 0

    def argv(self, _lease, _invocation):
        self.argv_calls += 1
        return [sys.executable, "-B", "-c", self.script]

    def stop(self, unit):
        self.stop_calls += 1
        return proof(unit)


@contextmanager
def execution(script, *, short_wait=False):
    with TemporaryDirectory(prefix="forge-agy-lifecycle-") as root:
        config = parse_config(raw_config(root))
        value = lease(config)
        supervisor = ScopeProofSupervisor(script)
        processes = []

        def popen(*args, **kwargs):
            process = subprocess.Popen(*args, **kwargs)
            processes.append(process)
            if short_wait:
                wait = process.wait

                def bounded_wait(timeout=None):
                    # Shorten only the provider's post-EOF launcher wait. The
                    # real cleanup helper still owns TERM/KILL and its waits.
                    return wait(timeout=.03 if timeout == 15 else timeout)

                process.wait = bounded_wait
            return process

        provider = CommandRunnerProvider(config, supervisor, popen=popen)
        provider.prepare()
        workspace = WorkspaceLease(Path(root) / "task", config.projects["homelab"],
                                   Path(root) / "git", value["task_id"], value["base_revision"])
        try:
            yield provider, value, workspace, supervisor, processes
        finally:
            # Independent fixture ownership cleanup protects the test host even
            # when a regression returns while its launcher is still alive.
            for process in processes:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait(timeout=5)
                for stream in (process.stdin, process.stdout, process.stderr):
                    if stream is not None:
                        stream.close()


class AntigravityLifecycleTest(unittest.TestCase):
    def test_selector_failure_stops_and_reaps_local_launcher_and_closes_pipes(self):
        with execution("import time;time.sleep(30)") as (provider, value, workspace, supervisor, processes):
            with patch("work_executor.selectors.DefaultSelector") as selected:
                selected.return_value.__enter__.return_value.select.side_effect = OSError("fixture selector failure")
                outcome = provider.run(value, workspace, threading.Event())
            self.assertEqual(outcome.status, "failed")
            self.assertEqual(len(processes), 1)
            self.assertIsNotNone(processes[0].poll(), "Provider must reap its own launcher on I/O failure")
            self.assertTrue(all(stream.closed for stream in (processes[0].stdin, processes[0].stdout, processes[0].stderr)))
            self.assertGreater(supervisor.stop_calls, 0)

    def test_read_failure_stops_and_reaps_local_launcher_and_closes_pipes(self):
        script = "import sys,time;sys.stdin.read();print('ready',flush=True);time.sleep(30)"
        with execution(script) as (provider, value, workspace, _supervisor, processes):
            original_read = os.read

            def read(descriptor, count):
                # Popen's exec-error pipe must work before the fixture process
                # exists; inject failure only on its admitted output pipe.
                if processes and not processes[0].stdout.closed and descriptor == processes[0].stdout.fileno():
                    raise OSError("fixture output failure")
                return original_read(descriptor, count)

            with patch("work_executor.os.read", side_effect=read):
                outcome = provider.run(value, workspace, threading.Event())
            self.assertEqual(outcome.status, "failed")
            self.assertIsNotNone(processes[0].poll())
            self.assertTrue(all(stream.closed for stream in (processes[0].stdin, processes[0].stdout, processes[0].stderr)))

    def test_unexpected_selector_exception_stops_scope_and_reaps_launcher(self):
        with execution("import time;time.sleep(30)") as (provider, value, workspace, supervisor, processes):
            with patch("work_executor.selectors.DefaultSelector") as selected:
                selected.return_value.__enter__.return_value.select.side_effect = RuntimeError("fixture selector defect")
                with self.assertRaises(RuntimeError):
                    provider.run(value, workspace, threading.Event())
            self.assertGreater(supervisor.stop_calls, 0, "Unexpected errors must stop the independently owned scope")
            self.assertIsNotNone(processes[0].poll())
            self.assertTrue(all(stream.closed for stream in (processes[0].stdin, processes[0].stdout, processes[0].stderr)))

    def test_unexpected_parser_exception_stops_scope_and_reaps_launcher(self):
        result = json.dumps(agy_result("A fixture result before the unexpected parser defect"))
        script = f"import sys,time;sys.stdin.read();print({result!r},flush=True);time.sleep(30)"
        with execution(script) as (provider, value, workspace, supervisor, processes):
            with patch("work_executor.CliParser.feed", side_effect=RuntimeError("fixture parser defect")):
                with self.assertRaises(RuntimeError):
                    provider.run(value, workspace, threading.Event())
            self.assertGreater(supervisor.stop_calls, 0)
            self.assertIsNotNone(processes[0].poll())
            self.assertTrue(all(stream.closed for stream in (processes[0].stdin, processes[0].stdout, processes[0].stderr)))

    def test_already_cancelled_task_spawns_no_cli_process(self):
        with execution("raise RuntimeError('must never start')") as (provider, value, workspace, supervisor, processes):
            stop = threading.Event()
            stop.set()
            outcome = provider.run(value, workspace, stop)
            self.assertEqual(outcome.status, "cancelled")
            self.assertEqual(processes, [])
            self.assertEqual(supervisor.argv_calls, 0)
            self.assertTrue(outcome.stop_proof["scope_stopped"])

    def test_output_eof_live_launcher_uses_term_then_kill_and_never_publishes_success(self):
        script = ("import os,signal,sys,time;sys.stdin.read();"
                  "signal.signal(signal.SIGTERM,signal.SIG_IGN);os.close(1);os.close(2);time.sleep(30)")
        with execution(script, short_wait=True) as (provider, value, workspace, _supervisor, processes):
            signals = []
            killpg = os.killpg

            def tracked_signal(group, signum):
                if signum:
                    signals.append(signum)
                return killpg(group, signum)

            def short_cleanup(process):
                return terminate_and_reap(process, term_seconds=.05, kill_seconds=.5)

            started = time.monotonic()
            with patch("work_executor.terminate_and_reap", side_effect=short_cleanup), \
                    patch("work_process.os.killpg", side_effect=tracked_signal):
                outcome = provider.run(value, workspace, threading.Event())
            self.assertEqual(outcome.status, "failed")
            self.assertEqual(processes[0].returncode, -signal.SIGKILL)
            self.assertEqual(signals, [signal.SIGTERM, signal.SIGKILL])
            self.assertLess(time.monotonic() - started, 2)

    def test_uncertain_launcher_cleanup_cannot_publish_coherent_success(self):
        result = json.dumps(agy_result("A coherent fixture result"))
        script = f"import sys;sys.stdin.read();print({result!r},flush=True)"
        with execution(script) as (provider, value, workspace, _supervisor, processes):
            with patch("work_executor.terminate_and_reap", return_value=False):
                outcome = provider.run(value, workspace, threading.Event())
            self.assertEqual(processes[0].returncode, 0)
            self.assertEqual(outcome.status, "needs_reconcile")
            self.assertEqual(outcome.text, "")
            self.assertTrue(outcome.stop_proof["scope_stopped"], "Local launcher certainty is independent of scope proof")

    def test_expected_conversation_rejects_different_init_identity(self):
        expected, other = str(uuid4()), str(uuid4())
        parser = CliParser("antigravity", expected_conversation_id=expected)
        with self.assertRaises(ExecutionError):
            parser.feed(json.dumps({"event": "init", "conversation_id": other}).encode() + b"\n")

    def test_expected_conversation_rejects_different_terminal_identity(self):
        expected, other = str(uuid4()), str(uuid4())
        parser = CliParser("antigravity", expected_conversation_id=expected)
        parser.feed(json.dumps({"event": "init", "conversation_id": expected}).encode() + b"\n")
        with self.assertRaises(ExecutionError):
            parser.feed(json.dumps(agy_result(conversation=other)).encode() + b"\n")

    def test_expected_conversation_accepts_matching_init_and_terminal(self):
        expected = str(uuid4())
        parser = CliParser("antigravity", expected_conversation_id=expected)
        parser.feed(json.dumps({"event": "init", "conversation_id": expected}).encode() + b"\n")
        parser.feed(json.dumps(agy_result(conversation=expected)).encode() + b"\n")
        outcome = parser.finish(0, proof())
        self.assertEqual(outcome.status, "awaiting_review")
        self.assertEqual(outcome.conversation_id, expected)


if __name__ == "__main__":
    unittest.main()
