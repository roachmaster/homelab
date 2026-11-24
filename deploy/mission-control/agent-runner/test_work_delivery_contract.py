"""Fix 4 (plan section 6): interim delivery contract tests.

The contract is documented in docs/architecture/event-stream-delivery-contract.md.
These tests assert its three testable guarantees:

* every pg_notify payload in the migration tree is empty (a wake-up hint
  carries no data);
* the idle scan timeout is 5 seconds (the wait applied when the worker has
  nothing to do);
* a missed notification still converges to current state via the scan.

They assert NO latency bound for a busy worker. The busy-worker test only
asserts the structural fact the loop guarantees: while run_once() keeps
returning True (work is being processed), the wait is never entered, so the
next scan is deferred for the duration of the processing.
"""

from contextlib import contextmanager
import re
import threading
import unittest
from pathlib import Path

from work_publication import PublicationWorker

REPO_ROOT = Path(__file__).resolve().parents[3]
MIGRATIONS_DIR = REPO_ROOT / "deploy" / "mission-control" / "sql" / "migrations"
AGENT_RUNNER_DIR = REPO_ROOT / "deploy" / "mission-control" / "agent-runner"

# pg_notify('channel', 'payload') — the payload is the second argument.
NOTIFY_RE = re.compile(r"pg_notify\(\s*'([^']+)'\s*,\s*'([^']*)'\s*\)")


class NotifyPayloadTest(unittest.TestCase):
    def test_every_pg_notify_payload_is_empty(self):
        sites = []
        for directory in (MIGRATIONS_DIR, AGENT_RUNNER_DIR):
            for path in sorted(directory.glob("*.sql" if directory == MIGRATIONS_DIR else "*.py")):
                if path.name == Path(__file__).name:
                    continue  # this test's own docstring names the pattern
                for match in NOTIFY_RE.finditer(path.read_text(encoding="utf-8")):
                    sites.append((str(path.relative_to(REPO_ROOT)), match.group(1), match.group(2)))
        # The mechanism must exist for the contract to mean anything.
        self.assertGreater(len(sites), 0)
        channels = {channel for _path, channel, _payload in sites}
        self.assertIn("forge_coding_jobs", channels)
        for path, channel, payload in sites:
            self.assertEqual(payload, "", f"{path}: pg_notify('{channel}', ...) carries data")

    def test_notify_sites_cover_the_wake_up_channels(self):
        sites = []
        for path in sorted(MIGRATIONS_DIR.glob("*.sql")):
            for match in NOTIFY_RE.finditer(path.read_text(encoding="utf-8")):
                sites.append(match.group(1))
        self.assertIn("forge_coding_jobs", sites)


class _ListenerStore:
    """A fake store for the run_forever loop: listen yields a listener, wait
    records its timeout and reports no notification."""

    def __init__(self):
        self.wait_calls = []

    @contextmanager
    def listen(self):
        yield object()

    def wait(self, _listener, timeout):
        self.wait_calls.append(timeout)
        return False


def _loop_worker(store, run_once, stop):
    """Build the smallest worker run_forever can drive: only the attributes
    the loop touches (store, stop, native_ci, run_once)."""
    worker = PublicationWorker.__new__(PublicationWorker)
    worker.store = store
    worker.stop = stop
    worker.native_ci = None
    worker.run_once = run_once
    return worker


class IdleScanTimeoutTest(unittest.TestCase):
    def test_idle_worker_waits_five_seconds(self):
        store = _ListenerStore()
        stop = threading.Event()
        calls = []

        def run_once():
            calls.append("scan")
            stop.set()
            return False

        worker = _loop_worker(store, run_once, stop)
        worker.run_forever()
        self.assertEqual(calls, ["scan"])
        self.assertEqual(store.wait_calls, [5])

    def test_missed_notification_still_converges_via_scan(self):
        # The notification is lost: wait() returns False. The scan in
        # run_once() still finds the queued job and processes it.
        store = _ListenerStore()
        stop = threading.Event()
        processed = []

        def run_once():
            if not processed:
                processed.append("job")
                return True  # found by the scan, not by a notification
            stop.set()
            return False

        worker = _loop_worker(store, run_once, stop)
        worker.run_forever()
        self.assertEqual(processed, ["job"])
        self.assertEqual(store.wait_calls, [5])  # the wait reported nothing

    def test_busy_worker_defers_the_scan_without_a_latency_bound(self):
        # While run_once() keeps returning True the loop continues without
        # ever entering the wait: the next scan is deferred for the
        # processing duration. No timing is asserted here — the contract
        # claims no latency bound for a busy worker.
        store = _ListenerStore()
        stop = threading.Event()
        scans = []

        def run_once():
            scans.append("scan")
            if len(scans) >= 3:
                stop.set()
            return True

        worker = _loop_worker(store, run_once, stop)
        worker.run_forever()
        self.assertEqual(len(scans), 3)
        self.assertEqual(store.wait_calls, [])


if __name__ == "__main__":
    unittest.main()
