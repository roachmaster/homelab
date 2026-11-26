"""Owned collaborator/lease-loop proofs using fake DB/CLI ports only."""

from contextlib import contextmanager
import hashlib
import json
from dataclasses import fields
from types import MappingProxyType
from pathlib import Path
from tempfile import TemporaryDirectory
import threading
import unittest
from unittest.mock import Mock, patch

from test_work_executor import lease, proof, raw_config
from test_work_peer_review import parse_review_target, sha, target as peer_target, verdict as peer_verdict
from work_executor import CliOutcome, ExecutionError
from work_peer_context import PeerReviewContext
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

    def queue_review(self, *args):
        self.events.append("queue-review")
        return "fixture-review"

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
            selected_codex = Mock()
            codex_sessions = Mock()
            selected_codex.return_value = codex_sessions
            factory = CodingWorkerFactory(
                config, lambda config: FakeStore([]), session_factory=selected,
                codex_session_factory=selected_codex,
                workspace_factory=lambda config: FakeWorkspaces([]),
                supervisor_factory=lambda config: Mock(),
            )
            selected.assert_not_called()
            selected_codex.assert_not_called()
            with patch("subprocess.Popen", side_effect=AssertionError("Factory must not start agent")):
                worker = factory.create()
            selected.assert_called_once_with(factory.config)
            selected_codex.assert_called_once_with(factory.config)
            self.assertIs(worker.runner.sessions, sessions)
            self.assertIs(worker.runner.codex_sessions, codex_sessions)
            sessions.prepare.assert_called_once_with()
            codex_sessions.prepare.assert_called_once_with()

            # An explicit runner replacement must never construct the default's
            # unused session providers or acquire their resources.
            selected.reset_mock()
            selected_codex.reset_mock()
            replacement = FakeRunner([])
            replaced = CodingWorkerFactory(
                config, lambda config: FakeStore([]), session_factory=selected,
                codex_session_factory=selected_codex,
                runner_factory=lambda config, supervisor: replacement,
                workspace_factory=lambda config: FakeWorkspaces([]),
                supervisor_factory=lambda config: Mock(),
            ).create()
            self.assertIs(replaced.runner, replacement)
            selected.assert_not_called()
            selected_codex.assert_not_called()

            raw = raw_config(root)
            del raw["profiles"]["agy"]
            del raw["binaries"]["antigravity"]
            codex_only_factory = CodingWorkerFactory(
                parse_config(raw), lambda config: FakeStore([]), session_factory=selected,
                codex_session_factory=selected_codex,
                workspace_factory=lambda config: FakeWorkspaces([]),
                supervisor_factory=lambda config: Mock(),
            )
            selected_codex.assert_not_called()
            codex_only = codex_only_factory.create()
            self.assertIsNone(codex_only.runner.sessions)
            self.assertIs(codex_only.runner.codex_sessions, codex_sessions)
            selected.assert_not_called()
            selected_codex.assert_called_once_with(codex_only_factory.config)

            selected.reset_mock()
            selected_codex.reset_mock()
            agy_raw = raw_config(root)
            del agy_raw["profiles"]["codex"]
            del agy_raw["binaries"]["codex"]
            agy_only_factory = CodingWorkerFactory(
                parse_config(agy_raw), lambda config: FakeStore([]), session_factory=selected,
                codex_session_factory=selected_codex,
                workspace_factory=lambda config: FakeWorkspaces([]),
                supervisor_factory=lambda config: Mock(),
            )
            agy_only = agy_only_factory.create()
            self.assertIs(agy_only.runner.sessions, sessions)
            self.assertIsNone(agy_only.runner.codex_sessions)
            selected.assert_called_once_with(agy_only_factory.config)
            selected_codex.assert_not_called()

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

    def test_typed_renewal_never_labels_cancellation_or_deadline_as_lease_lost(self):
        import work_worker
        cases = (("task_cancelled", "scope_stopped", work_worker.CANCELLED_STOP_SUMMARY),
                 ("deadline_exceeded", "deadline_exceeded", work_worker.DEADLINE_STOP_SUMMARY),
                 ("lease_expired", "lease_lost", None))
        for status, cause, summary in cases:
            with self.subTest(status=status), TemporaryDirectory(prefix="forge-typed-renew-") as root:
                worker, value, store, events = self._worker(root)
                signals, stops = [], []
                store.renew_status = lambda lease_value, signal=None: (signals.append(signal), status)[1]
                store.record_stop = lambda *args: stops.append(args)
                worker.process(value)
                self.assertNotIn("runner", events)
                self.assertEqual(store.publications, [])
                self.assertEqual(stops[-1][3], cause)
                if summary is not None:
                    self.assertEqual(stops[-1][2], summary)
                self.assertNotIn("renew", events)  # typed renewal replaces the boolean call
                self.assertIsNone(signals[0])

    def test_typed_renewal_falls_back_to_boolean_renew_before_m45(self):
        with TemporaryDirectory(prefix="forge-typed-renew-fallback-") as root:
            worker, value, store, events = self._worker(root)
            store.renew_status = Mock(return_value=None)
            worker.process(value)
            self.assertIn("renew", events)
            self.assertEqual(store.publications[0][1], "awaiting_review")

    def test_agent_output_signals_renewal_and_extended_deadline_reaches_runner(self):
        from datetime import datetime, timedelta, timezone
        with TemporaryDirectory(prefix="forge-deadline-hint-") as root:
            worker, value, store, events = self._worker(root)
            calls, hints = [], []
            store.renew_deadline = lambda lease_value, signal=None: (calls.append(signal), {
                "status": "renewed", "deadline_at": datetime.now(timezone.utc) + timedelta(minutes=40), "extended": True})[1]
            base = worker.runner

            class HookedRunner:
                def prepare(self):
                    base.prepare()

                def run(self, lease, workspace, stop, *, report_progress=None, plan_context=None, feedback_context=None,
                        report_activity=None, deadline_hint=None):
                    report_activity()
                    worker_renewed = calls[:]
                    hints.append((deadline_hint(), worker_renewed))
                    return base.run(lease, workspace, stop, report_progress=report_progress,
                                    plan_context=plan_context, feedback_context=feedback_context)

            worker.runner = HookedRunner()
            worker.process(value)
            self.assertEqual(store.publications[0][1], "awaiting_review")
            self.assertGreater(hints[0][0], 30 * 60)
            self.assertNotIn("renew", events)  # M47 renewal replaces the boolean call

    def test_cancellation_without_proven_scope_stop_is_interrupted(self):
        import work_worker
        self.assertEqual(work_worker._stop_cause("scope_stopped", {"scope_stopped": False}), "interrupted")
        self.assertEqual(work_worker._stop_cause("scope_stopped", {"scope_stopped": True}), "scope_stopped")
        self.assertEqual(work_worker._stop_cause("lease_lost", None), "lease_lost")

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


class ScriptedReviewRunner:
    """Return canned review outcomes in order, recording each call's kwargs."""

    def __init__(self):
        self.outcomes = []
        self.calls = []

    def run(self, lease, workspace, stop, **kwargs):
        self.calls.append(dict(kwargs))
        return self.outcomes[len(self.calls) - 1]


class ReviewVerdictSelfCheckTest(unittest.TestCase):
    """Plan §3.2/§8: a malformed first verdict is retried inside the attempt."""

    def _worker(self, root):
        config = parse_config(raw_config(root))
        events = []
        store = FakeStore(events)
        worker = CodingWorker(config, store, FakeRunner(events), FakeWorkspaces(events), Mock(),
                              usage_normalizer=lambda *a, **k: {"total_tokens": "5"})
        scripted = ScriptedReviewRunner()
        worker.runner = scripted
        summaries = []
        store.checkpoint = lambda lease_value, summary: summaries.append(summary)
        value = lease(config)
        value["incarnation"] = worker.incarnation
        return worker, value, scripted, summaries

    def _peer_context(self):
        raw = peer_target("code")
        raw_json = json.dumps(raw)
        parsed = parse_review_target(raw_json, sha(raw_json))
        valid = peer_verdict(parsed, "revise")
        malformed = dict(valid)
        malformed["context_sha256"] = valid["context_sha256"][:49]
        context = PeerReviewContext(raw_json.encode("utf-8"), sha(raw_json),
                                    parsed["workflow_id"], parsed["workflow_review_id"],
                                    parsed["target_kind"])
        return context, json.dumps(valid), json.dumps(malformed)

    def _make_outcome(self, value, text):
        return CliOutcome("awaiting_review", text, "c" * 36, {"input_tokens": 5},
                          "Fixture ready", proof(value["unit_name"]))

    def test_malformed_first_verdict_is_retried_and_never_published(self):
        with TemporaryDirectory(prefix="forge-verdict-self-check-") as root:
            context, valid_text, malformed_text = self._peer_context()
            worker, value, scripted, summaries = self._worker(root)
            scripted.outcomes = [self._make_outcome(value, malformed_text),
                                 self._make_outcome(value, valid_text)]
            outcome = worker._run_review(value, FakeWorkspace(), threading.Event(),
                                         {"peer_context": context})
            self.assertEqual(outcome.text, valid_text)
            self.assertEqual(len(scripted.calls), 2)
            self.assertNotIn("verdict_retry_note", scripted.calls[0])
            self.assertIn("verdict.identity.context_sha256",
                          scripted.calls[1]["verdict_retry_note"])
            self.assertEqual(len(summaries), 1)
            self.assertTrue(summaries[0].startswith(
                "verdict_self_check_failed: check=verdict.identity.context_sha256"))

    def test_never_valid_verdict_returns_last_outcome_after_two_retries(self):
        """Documents the §3.2 divergence: once the retry budget is spent the
        last outcome is published, so the coordinator escalation plus the
        correction path remains the backstop for a verdict that never
        becomes valid (the plan's acceptance says 'not with a coordinator
        escalation'; the harness keeps the audit trail and the recovery)."""
        with TemporaryDirectory(prefix="forge-verdict-self-check-") as root:
            context, _valid_text, malformed_text = self._peer_context()
            worker, value, scripted, summaries = self._worker(root)
            scripted.outcomes = [self._make_outcome(value, malformed_text) for _ in range(3)]
            outcome = worker._run_review(value, FakeWorkspace(), threading.Event(),
                                         {"peer_context": context})
            self.assertEqual(outcome.text, malformed_text)
            self.assertEqual(len(scripted.calls), 3)
            self.assertEqual(len(summaries), 2)
            self.assertTrue(all(summary.startswith(
                "verdict_self_check_failed: check=verdict.identity.context_sha256")
                for summary in summaries))


if __name__ == "__main__":
    unittest.main()
