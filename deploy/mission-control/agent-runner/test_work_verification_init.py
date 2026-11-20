"""Real owned-process evidence for the verifier's PID1 launcher recipe."""

import os
from pathlib import Path
import signal
import subprocess
import sys
import unittest

from work_verification_init import VERIFICATION_INIT, verification_argv


class VerificationInitTest(unittest.TestCase):
    def run_child(self, source):
        return subprocess.run(verification_argv((sys.executable, "-B", "-c", source)),
                              stdin=subprocess.DEVNULL, capture_output=True, timeout=5)

    def test_exit_status_and_child_signal_are_preserved(self):
        for source, expected in (("pass", 0), ("raise SystemExit(3)", 3),
                                 ("import os,signal;os.kill(os.getpid(),signal.SIGTERM)", 143)):
            with self.subTest(expected=expected):
                result = self.run_child(source)
                self.assertEqual(result.returncode, expected, result.stderr)

    def test_termination_is_forwarded_and_never_becomes_success(self):
        command = verification_argv((sys.executable, "-B", "-c",
                                     "import signal;print('ready',flush=True);signal.pause()"))
        process = subprocess.Popen(command, stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            self.assertEqual(process.stdout.readline(), b"ready\n")
            process.send_signal(signal.SIGTERM)
            self.assertEqual(process.wait(timeout=5), 143)
        finally:
            if process.poll() is None:
                process.kill()
            process.wait(timeout=5)
            process.stdout.close()
            process.stderr.close()

    @unittest.skipUnless(sys.platform.startswith("linux"), "PID namespace services run on Linux")
    def test_adopted_orphan_is_reaped_before_verifier_finishes(self):
        # A subreaper emulates PID1 adoption without requiring mount privileges.
        # The exact cleanup regression formerly failed inside the verifier's
        # namespace because its Python PID1 left an adopted zombie unreaped.
        prefix = ("import ctypes\n"
                  "assert ctypes.CDLL(None).prctl(36,1,0,0,0)==0\n")
        command = (sys.executable, "-B", "-c", prefix + VERIFICATION_INIT,
                   sys.executable, "-B", "-m", "unittest",
                   "test_work_process.LauncherCleanupTest."
                   "test_exited_leader_does_not_leave_descendant_holding_output_pipes")
        result = subprocess.run(command, cwd=Path(__file__).parent,
                                stdin=subprocess.DEVNULL, capture_output=True, timeout=10,
                                env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"})
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(b"Ran 1 test", result.stderr)

    def test_fixed_argv_is_required(self):
        for command in ([], (), ("",), ("tool\0",), "shell command"):
            with self.subTest(command=command), self.assertRaises(ValueError):
                verification_argv(command)


if __name__ == "__main__":
    unittest.main()
