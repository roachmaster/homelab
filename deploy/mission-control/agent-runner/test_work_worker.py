"""Owned collaborator/lease-loop proofs using fake DB/CLI ports only."""

from contextlib import contextmanager
import hashlib
from dataclasses import fields
from types import MappingProxyType
from pathlib import Path
from tempfile import TemporaryDirectory
import threading
import unittest
from unittest.mock import Mock, patch

from test_work_executor import lease, proof, raw_config
from work_executor import CliOutcome, ExecutionError
from work_worker import CodingWorkerFactory, CodingWorker, default_collector_factory
from work_worker_config import parse_config
from work_worker_store import Lease


class FakeWorkspace:
    def digest(self):
        return "e" * 64


class FakeWorkspaces:
    def __init__(self, events):
        self.events = events

    def prepare(self):
        self.events.append("workspace-prepare")

    def projects(self):
        return [{"alias": "homelab", "base_revision": "a" * 40}]

    @contextmanager
    def acquire(self, lease):
        self.events.append("workspace-acquire")
        yield FakeWorkspace()
        self.events.append("workspace-release")

    def inspect(self, lease):
        return self.acquire(lease)


class FakeRunner:
    def __init__(self, events, action=None):
        self.events, self.action = events, action
        self.callbacks = []
        self.plan_contexts = []
        self.feedback_contexts = []

    def prepare(self):
        self.events.append("runner-prepare")

    def run(self, lease, workspace, stop, *, report_progress=None, plan_context=None, feedback_context=None):
        self.events.append("runner")
        self.callbacks.append(report_progress)
        self.plan_contexts.append(plan_context)
        self.feedback_contexts.append(feedback_context)
        if report_progress is not None:
            report_progress(1, "plan", "Fixture public plan.")
        if self.action:
            return self.action(lease, workspace, stop)
        return CliOutcome("awaiting_review", "é🛠x" * 20_000, "c" * 36,
                          {"input_tokens": 5}, "Fixture ready", proof(lease["unit_name"]))


class FakeStore:
    def __init__(self, events):
        self.events = events
        self.chunks = []
        self.publications = []
        self.live = True
        self.cancelled = False
        self.pending = []
        self.claimed = None
        self.reports = []

    def advertise(self, *args):
        self.events.append("advertise")

    def renew(self, lease):
        self.events.append("renew")
        return self.live

    def cancellation_requested(self, lease):
        return self.cancelled

    def checkpoint(self, lease, summary):
        self.events.append("checkpoint")

    def report_progress(self, lease, sequence, kind, summary):
        self.events.append("progress")
        self.reports.append((lease, sequence, kind, summary))

    def append_result(self, lease, index, text):
        self.chunks.append((index, text))

    def publish(self, *args):
        self.events.append("publish")
        self.publications.append(args)
        return {"fixture": "receipt"}

    def record_stop(self, *args):
        self.events.append("record-stop")

    def expire_leases(self):
        self.events.append("expire")
        return 0

    def pending_reconciliations(self):
        return self.pending

    def reconcile(self, *args):
        self.events.append("reconcile")
        return "fixture-receipt"

    def claim(self, *args):
        self.events.append("claim")
        value, self.claimed = self.claimed, None
        return value


class WorkerLifecycleTest(unittest.TestCase):
    def test_codex_retains_terminal_reporting_without_scratch_write_callback(self):
        with TemporaryDirectory(prefix="forge-codex-no-progress-") as root:
            worker, _, store, events = self._worker(root)
            value = lease(worker.config, backend="codex")
            value["incarnation"] = worker.incarnation
            worker.process(value)
            self.assertEqual(worker.runner.callbacks, [None])
            self.assertEqual(store.reports, [])
            self.assertEqual(store.publications[0][1], "awaiting_review")
            self.assertEqual(events.count("runner"), 1)

    def test_public_progress_uses_owned_store_and_retries_only_identical_receipt(self):
        with TemporaryDirectory(prefix="forge-progress-retry-") as root:
            worker, value, store, events = self._worker(root)
            store.report_progress = Mock(side_effect=[ConnectionError("lost ack"), None])
            worker.process(value)
            self.assertEqual(store.report_progress.call_count, 2)
            self.assertEqual(store.report_progress.call_args_list[0], store.report_progress.call_args_list[1])
            self.assertEqual(store.report_progress.call_args.args, (value, 1, "plan", "Fixture public plan."))
            self.assertEqual(events.count("runner"), 1)
            self.assertEqual(store.publications[0][1], "awaiting_review")

    def test_unrecoverable_progress_failure_never_publishes_success_or_replays_cli(self):
        with TemporaryDirectory(prefix="forge-progress-failure-") as root:
            worker, value, store, events = self._worker(root)
            store.report_progress = Mock(side_effect=ValueError("invalid receipt"))
            with self.assertRaises(ValueError):
                worker.process(value)
            self.assertEqual(store.report_progress.call_count, 1)
            self.assertEqual(events.count("runner"), 1)
            self.assertEqual(store.publications, [])
            self.assertIn("record-stop", events)

    def test_real_immutable_database_lease_is_accepted_without_deepcopy(self):
        with TemporaryDirectory(prefix="forge-worker-lease-") as root:
            worker, value, store, events = self._worker(root)
            value["recipe"] = MappingProxyType(value["recipe"])
            actual = Lease(**{field.name: value.get(field.name) for field in fields(Lease)})
            worker.process(actual)
            self.assertEqual(events.count("runner"), 1)
            self.assertEqual(store.publications[0][1], "awaiting_review")

    def test_chunk_ack_retry_preserves_complete_local_reply_without_cli_replay(self):
        with TemporaryDirectory(prefix="forge-worker-spool-") as root:
            worker, value, store, events = self._worker(root)
            append = store.append_result
            calls = []
            def fail_once(lease_value, index, text):
                calls.append((index, text))
                if len(calls) == 1:
                    append(lease_value, index, text)
                    raise ConnectionError("lost chunk acknowledgement")
                if len(calls) == 2:
                    return  # replay of the immutable index
                append(lease_value, index, text)
            store.append_result = fail_once
            worker.process(value)
            reply = worker.config.state_root / "results" / value["attempt_id"] / "reply.txt"
            self.assertEqual(reply.read_text(), "é🛠x" * 20_000)
            self.assertEqual(calls[0], calls[1])
            self.assertEqual(events.count("runner"), 1)
            self.assertEqual(store.publications[0][1], "awaiting_review")

    def _worker(self, root, *, action=None, normalizer=None, collector=None):
        config = parse_config(raw_config(root))
        events = []
        store = FakeStore(events)
        workspaces = FakeWorkspaces(events)
        runner = FakeRunner(events, action)
        supervisor = Mock()
        supervisor.stop.side_effect = lambda unit: proof(unit)
        worker = CodingWorker(config, store, runner, workspaces, supervisor,
                              usage_normalizer=normalizer or (lambda *a, **k: {"total_tokens": "5"}),
                              collector=collector)
        value = lease(config)
        value["incarnation"] = worker.incarnation
        return worker, value, store, events

    def test_selected_factories_are_effect_free_until_create_and_instances_are_used(self):
        with TemporaryDirectory(prefix="forge-worker-factory-") as root:
            config = parse_config(raw_config(root))
            events = []
            store, workspace, runner, supervisor = FakeStore(events), FakeWorkspaces(events), FakeRunner(events), Mock()
            selected_store = Mock(return_value=store)
            selected_runner = Mock(return_value=runner)
            selected_workspace = Mock(return_value=workspace)
            selected_supervisor = Mock(return_value=supervisor)
            factory = CodingWorkerFactory(config, selected_store, runner_factory=selected_runner,
                                          workspace_factory=selected_workspace, supervisor_factory=selected_supervisor)
            selected_store.assert_not_called()
            selected_runner.assert_not_called()
            worker = factory.create()
            self.assertIs(worker.store, store)
            self.assertIs(worker.runner, runner)
            self.assertIs(worker.workspaces, workspace)
            self.assertIs(worker.supervisor, supervisor)
            self.assertEqual(events, ["workspace-prepare", "runner-prepare"])
            self.assertIsNone(default_collector_factory(config, store))

    def test_default_runner_uses_selected_session_provider_without_starting_a_cli(self):
        with TemporaryDirectory(prefix="forge-session-factory-") as root:
            config = parse_config(raw_config(root))
            selected = Mock()
            sessions = Mock()
            selected.return_value = sessions
            factory = CodingWorkerFactory(
                config, lambda config: FakeStore([]), session_factory=selected,
                workspace_factory=lambda config: FakeWorkspaces([]),
                supervisor_factory=lambda config: Mock(),
            )
            selected.assert_not_called()
            with patch("subprocess.Popen", side_effect=AssertionError("Factory must not start agent")):
                worker = factory.create()
            selected.assert_called_once_with(factory.config)
            self.assertIs(worker.runner.sessions, sessions)
            sessions.prepare.assert_called_once_with()

            # An explicit runner replacement must never construct the default's
            # unused session provider or acquire its resources.
            selected.reset_mock()
            replacement = FakeRunner([])
            replaced = CodingWorkerFactory(
                config, lambda config: FakeStore([]), session_factory=selected,
                runner_factory=lambda config, supervisor: replacement,
                workspace_factory=lambda config: FakeWorkspaces([]),
                supervisor_factory=lambda config: Mock(),
            ).create()
            self.assertIs(replaced.runner, replacement)
            selected.assert_not_called()

            raw = raw_config(root)
            del raw["profiles"]["agy"]
            del raw["binaries"]["antigravity"]
            codex_only = CodingWorkerFactory(
                parse_config(raw), lambda config: FakeStore([]), session_factory=selected,
                workspace_factory=lambda config: FakeWorkspaces([]),
                supervisor_factory=lambda config: Mock(),
            ).create()
            self.assertIsNone(codex_only.runner.sessions)
            selected.assert_not_called()

    def test_full_result_is_chunked_before_one_explicit_review_publication(self):
        with TemporaryDirectory(prefix="forge-worker-flow-") as root:
            worker, value, store, events = self._worker(root)
            worker.process(value)
            self.assertLess(events.index("renew"), events.index("workspace-acquire"))
            self.assertLess(events.index("workspace-acquire"), events.index("runner"))
            self.assertLess(events.index("publish"), events.index("reconcile"))
            self.assertLess(events.index("reconcile"), events.index("workspace-release"))
            self.assertEqual(len(store.publications), 1)
            accepted = store.publications[0]
            self.assertEqual(accepted[1], "awaiting_review")
            result = "".join(text for index, text in store.chunks).encode()
            self.assertGreater(len(result), 65_536)
            self.assertEqual(accepted[2], {"sha256": hashlib.sha256(result).hexdigest(),
                                           "byte_count": len(result), "chunk_count": len(store.chunks)})
            self.assertEqual(accepted[3], {"total_tokens": "5"})

    def test_policy_or_expired_lease_rejection_has_zero_cli_effects(self):
        for drift in ("policy", "lease"):
            with self.subTest(drift=drift), TemporaryDirectory(prefix="forge-worker-fence-") as root:
                worker, value, store, events = self._worker(root)
                if drift == "policy":
                    value["recipe"]["policy_digest"] = "0" * 64
                else:
                    store.live = False
                worker.process(value)
                self.assertNotIn("runner", events)
                self.assertNotIn("workspace-acquire", events)
                self.assertEqual(len(store.publications), 1 if drift == "policy" else 0)
                if drift == "lease":
                    self.assertIn("record-stop", events)

    def test_lost_publication_ack_retries_receipt_without_rerunning_agent(self):
        with TemporaryDirectory(prefix="forge-worker-replay-") as root:
            worker, value, store, events = self._worker(root)
            publish = Mock(side_effect=[ConnectionError("lost acknowledgement"), {"fixture": "receipt"}])
            store.publish = publish
            worker.process(value)
            self.assertEqual(events.count("runner"), 1)
            self.assertEqual(publish.call_count, 2)
            self.assertEqual(publish.call_args_list[0], publish.call_args_list[1])

    def test_usage_failure_never_discards_a_coherent_reply(self):
        with TemporaryDirectory(prefix="forge-worker-telemetry-") as root:
            worker, value, store, events = self._worker(root, normalizer=Mock(side_effect=ValueError("unknown")))
            worker.process(value)
            self.assertEqual(store.publications[0][1], "awaiting_review")
            self.assertIsNone(store.publications[0][3])

    def test_user_cancel_and_worker_shutdown_are_distinct(self):
        for shutdown in (False, True):
            with self.subTest(shutdown=shutdown), TemporaryDirectory(prefix="forge-worker-cancel-") as root:
                worker, value, store, events = self._worker(root)
                if shutdown:
                    worker.stop.set()
                else:
                    store.cancelled = True
                worker.process(value)
                self.assertNotIn("runner", events)
                self.assertEqual(store.publications[0][1], "needs_reconcile" if shutdown else "cancelled")

    def test_surviving_reconciler_stops_old_scope_before_any_new_claim(self):
        with TemporaryDirectory(prefix="forge-worker-recovery-") as root:
            worker, value, store, events = self._worker(root)
            store.pending = [{key: item for key, item in {**value, "owner_id": worker.config.owner}.items()
                              if key not in {"prompt", "acceptance_criteria", "recipe", "attempt_token", "lease_expires_at"}}]
            worker.supervisor.stop.side_effect = lambda unit: (events.append("scope-stop") or proof(unit))
            self.assertFalse(worker.run_once())
            self.assertLess(events.index("expire"), events.index("scope-stop"))
            self.assertLess(events.index("scope-stop"), events.index("reconcile"))
            self.assertLess(events.index("reconcile"), events.index("claim"))
            self.assertNotIn("runner", events)

    def test_unconfirmed_scope_or_workspace_drift_never_mints_takeover(self):
        with TemporaryDirectory(prefix="forge-worker-uncertain-") as root:
            worker, value, store, events = self._worker(root)
            store.pending = [{key: item for key, item in {**value, "owner_id": worker.config.owner}.items()
                              if key not in {"prompt", "acceptance_criteria", "recipe", "attempt_token", "lease_expires_at"}}]
            worker.supervisor.stop.side_effect = lambda unit: proof(unit, stopped=False)
            worker.reconcile_pending()
            self.assertNotIn("reconcile", events)
            self.assertNotIn("workspace-acquire", events)

    def test_idle_usage_collection_is_independent_from_claims_and_errors(self):
        with TemporaryDirectory(prefix="forge-worker-collector-") as root:
            collector = Mock()
            collector.run_once.side_effect = RuntimeError("metadata unavailable")
            worker, value, store, events = self._worker(root, collector=collector)
            self.assertFalse(worker.run_once())
            collector.run_once.assert_called_once()
            store.claimed = value
            self.assertTrue(worker.run_once())
            self.assertEqual(collector.run_once.call_count, 2)


if __name__ == "__main__":
    unittest.main()
