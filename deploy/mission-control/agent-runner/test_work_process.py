"""Real local launcher cleanup fixtures; no agent, systemd or account calls."""

from contextlib import contextmanager
import os
from pathlib import Path
import signal
import subprocess
import sys
from tempfile import TemporaryDirectory
import time
import unittest
from unittest.mock import Mock, patch

from work_process import terminate_and_reap


@contextmanager
def child(script, *, new_session=True):
    process = subprocess.Popen([sys.executable, "-B", "-c", script],
                               stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, start_new_session=new_session)
    try:
        yield process
    finally:
        # Test ownership cleanup remains independent of the function under test.
        if new_session:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        elif process.poll() is None:
            process.kill()
        process.wait(timeout=5)
        process.stdout.close()
        process.stderr.close()


class LauncherCleanupTest(unittest.TestCase):
    def test_ordinary_launcher_is_terminated_and_reaped(self):
        with child("import time;print('ready',flush=True);time.sleep(30)") as process:
            self.assertEqual(process.stdout.readline(), b"ready\n")
            self.assertTrue(terminate_and_reap(process, term_seconds=.2, kill_seconds=.5))
            self.assertEqual(process.returncode, -signal.SIGTERM)

    def test_sigterm_resistant_launcher_escalates_to_sigkill_within_bound(self):
        script = ("import signal,time;signal.signal(signal.SIGTERM,signal.SIG_IGN);"
                  "print('ready',flush=True);time.sleep(30)")
        with child(script) as process:
            self.assertEqual(process.stdout.readline(), b"ready\n")
            started = time.monotonic()
            self.assertTrue(terminate_and_reap(process, term_seconds=.1, kill_seconds=.5))
            self.assertEqual(process.returncode, -signal.SIGKILL)
            self.assertLess(time.monotonic() - started, 2)

    def test_exited_leader_does_not_leave_descendant_holding_output_pipes(self):
        with TemporaryDirectory(prefix="forge-launcher-descendant-") as root:
            ready = Path(root) / "ready"
            script = ("import os,signal,time,pathlib\n"
                      "pid=os.fork()\n"
                      "if pid:\n"
                      f" while not pathlib.Path({str(ready)!r}).exists(): time.sleep(.005)\n"
                      " os._exit(0)\n"
                      "signal.signal(signal.SIGTERM,signal.SIG_IGN)\n"
                      f"pathlib.Path({str(ready)!r}).write_text(str(os.getpid()))\n"
                      "time.sleep(30)\n")
            with child(script) as process:
                self.assertEqual(process.wait(timeout=5), 0)
                descendant = int(ready.read_text())
                self.assertEqual(os.getpgid(descendant), process.pid)
                self.assertTrue(terminate_and_reap(process, term_seconds=.1, kill_seconds=1))
                self.assertEqual(process.stdout.read(), b"")
                self.assertEqual(process.stderr.read(), b"")
                with self.assertRaises(ProcessLookupError):
                    os.killpg(process.pid, 0)

    def test_cleanup_is_idempotent_after_normal_exit(self):
        with child("pass") as process:
            self.assertEqual(process.wait(timeout=5), 0)
            self.assertTrue(terminate_and_reap(process))
            self.assertTrue(terminate_and_reap(process))
        self.assertTrue(terminate_and_reap(None))

    def test_current_process_and_caller_group_are_never_signalled(self):
        for pid in (-1, 0, 1, os.getpid(), os.getpgrp(), True):
            with self.subTest(pid=pid), patch("work_process.os.killpg") as kill:
                self.assertFalse(terminate_and_reap(Mock(pid=pid)))
                kill.assert_not_called()

    def test_live_process_without_new_session_is_refused(self):
        with child("import time;time.sleep(30)", new_session=False) as process:
            self.assertFalse(terminate_and_reap(process))
            self.assertIsNone(process.poll())

    def test_pid_reuse_after_reaping_is_refused(self):
        process = Mock(pid=999_999, returncode=0)
        process.poll.return_value = 0
        with patch("work_process.os.getpgid", return_value=process.pid), \
                patch("work_process.os.getsid", return_value=process.pid), \
                patch("work_process.os.killpg") as kill:
            self.assertFalse(terminate_and_reap(process))
            kill.assert_not_called()

    def test_failed_group_observation_returns_unconfirmed(self):
        with child("import time;time.sleep(30)") as process:
            with patch("work_process.os.killpg", side_effect=PermissionError):
                self.assertFalse(terminate_and_reap(process))
            self.assertIsNone(process.poll())

    def test_unstopped_group_is_never_reported_confirmed_after_deadline(self):
        with child("import time;time.sleep(30)") as process:
            started = time.monotonic()
            with patch("work_process._signal_group") as signal_group:
                self.assertFalse(terminate_and_reap(process, term_seconds=.02, kill_seconds=.02))
            self.assertEqual([call.args[1] for call in signal_group.call_args_list],
                             [signal.SIGTERM, signal.SIGKILL])
            self.assertLess(time.monotonic() - started, 2)
            self.assertIsNone(process.poll())

    def test_invalid_waits_fail_before_any_process_effect(self):
        for value in (-1, 11, True, float("nan"), float("inf"), "2"):
            with self.subTest(value=value), patch("work_process.os.killpg") as kill:
                with self.assertRaises(ValueError):
                    terminate_and_reap(Mock(pid=999_999), term_seconds=value)
                with self.assertRaises(ValueError):
                    terminate_and_reap(Mock(pid=999_999), kill_seconds=value)
                kill.assert_not_called()


if __name__ == "__main__":
    unittest.main()
