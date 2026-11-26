"""GB-3 item 5: no backend CLI carries a fixed 30-minute wall flag; the executor's
progress-aware deadline (M45/M47) is the only wall-time authority."""
import re
import unittest
from pathlib import Path

import work_executor
from work_executor import EXTENDED_WALL_BOUND_SECONDS, POLICY, _agy_argv
from work_grok import argv as grok_argv

HERE = Path(__file__).resolve().parent


class WallBoundTest(unittest.TestCase):
    def test_grok_argv_has_no_cli_wall_or_timeout_flag(self):
        args = grok_argv("/usr/bin/grok", "m", "/w", "/p", "00000000-0000-4000-8000-000000000001", "/s", implement=True)
        self.assertFalse([a for a in args if "timeout" in a.lower() or "max-wall" in a.lower()])

    def test_antigravity_cli_timeout_is_the_outer_bound_not_the_base_wall(self):
        args = _agy_argv("/usr/bin/agy", "m", implement=False, unattended=False)
        value = args[args.index("--print-timeout") + 1]
        self.assertEqual(value, f"{EXTENDED_WALL_BOUND_SECONDS}s")
        self.assertGreater(EXTENDED_WALL_BOUND_SECONDS, POLICY["max_wall_seconds"])

    def test_no_executor_cli_flag_hardcodes_the_base_wall_seconds(self):
        source = (HERE / "work_executor.py").read_text()
        self.assertNotRegex(source, r'"--print-timeout",\s*str\(POLICY\["max_wall_seconds"\]\)')

    def test_activity_signal_is_not_backend_gated(self):
        # Any stdout/stderr bytes (Grok's streaming-json included) signal activity.
        source = (HERE / "work_executor.py").read_text()
        block = source[source.index("if report_activity is not None:"):][:200]
        self.assertNotIn("backend", block)
        self.assertIn("report_activity()", block)


if __name__ == "__main__":
    unittest.main()
