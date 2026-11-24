"""Lifecycle tracker + intent drain contracts: intents are durable, delivery is
concurrent with the verification loop, and nothing here can fail a check."""
import json
import os
from pathlib import Path
import tempfile
import threading
import time
import unittest
from uuid import uuid4

from jenkins_events import JenkinsEventEmit, JenkinsEventKind
from work_jenkins_lifecycle import (
    IntentDrain, LifecycleTracker, lifecycle_delivery_id, stage_delivery_id)

ACTION_ID = '11111111-1111-4111-8111-111111111111'
COMMIT = 'c' * 40


class RecordingEmitter:
    def __init__(self, fail_kinds=()):
        self.emitted = []
        self.fail_kinds = set(fail_kinds)
        self._lock = threading.Lock()

    def emit(self, event):
        with self._lock:
            self.emitted.append(event)
        if event.event_kind.value in self.fail_kinds:
            raise ValueError('simulated emit failure')

    def kinds(self):
        with self._lock:
            return [event.event_kind.value for event in self.emitted]


def tracker(directory, **overrides):
    params = dict(action_id=ACTION_ID, project_alias='homelab', jenkins_instance='ci-primary',
                  job_name='forge-native-candidate', commit_sha=COMMIT)
    params.update(overrides)
    return LifecycleTracker(Path(directory), **params)


def result(status, **receipt):
    return {'status': status, 'summary': 'fixture', 'receipt': receipt}


class LifecycleTrackerTest(unittest.TestCase):
    def test_queue_then_build_then_completion_records_each_transition_once(self):
        with tempfile.TemporaryDirectory() as temporary:
            observed = tracker(temporary)
            self.assertEqual(observed.observe(result('pending', queue_id=7)), ('build_queued',))
            self.assertEqual(observed.observe(result('pending', queue_id=7, build_number=4)),
                             ('build_started',))
            self.assertEqual(observed.observe(result('passed', queue_id=7, build_number=4,
                                                     checks=[{'index': 0, 'returncode': 0}])),
                             ('build_completed',))
            # Re-observing the same state records nothing.
            self.assertEqual(observed.observe(result('passed', queue_id=7, build_number=4,
                                                     checks=[{'index': 0, 'returncode': 0}])), ())
            self.assertEqual(observed.observed_kinds,
                             {'build_queued', 'build_started', 'build_completed'})
            intents = sorted(p.name for p in Path(temporary).glob('pending-emit-*.json'))
            self.assertEqual(intents, ['pending-emit-build_completed.json',
                                       'pending-emit-build_queued.json',
                                       'pending-emit-build_started.json'])
            payload = json.loads((Path(temporary) / 'pending-emit-build_completed.json').read_text())
            self.assertEqual(payload['build_result'], 'success')
            self.assertEqual(payload['delivery_id'],
                             lifecycle_delivery_id(ACTION_ID, 'build_completed'))

    def test_build_observation_backfills_the_queue_transition(self):
        with tempfile.TemporaryDirectory() as temporary:
            observed = tracker(temporary)
            recorded = observed.observe(result('pending', queue_id=7, build_number=4))
            self.assertEqual(recorded, ('build_queued', 'build_started'))

    def test_completion_result_prefers_receipt_then_jenkins_record(self):
        with tempfile.TemporaryDirectory() as temporary:
            observed = tracker(temporary)
            observed.observe(result('pending', queue_id=7, build_number=4))
            # Receipt checks present: passed -> success even if Jenkins said FAILURE.
            observed.observe(result('passed', queue_id=7, build_number=4, result='FAILURE',
                                    checks=[{'index': 0, 'returncode': 0}]))
            payload = json.loads((Path(temporary) / 'pending-emit-build_completed.json').read_text())
            self.assertEqual(payload['build_result'], 'success')
        with tempfile.TemporaryDirectory() as temporary:
            observed = tracker(temporary)
            observed.observe(result('pending', queue_id=7, build_number=4))
            # No checks list: the Jenkins result is the fallback.
            observed.observe(result('failed', queue_id=7, build_number=4, result='FAILURE'))
            payload = json.loads((Path(temporary) / 'pending-emit-build_completed.json').read_text())
            self.assertEqual(payload['build_result'], 'failure')

    def test_completion_without_any_result_is_not_recorded(self):
        with tempfile.TemporaryDirectory() as temporary:
            observed = tracker(temporary)
            observed.observe(result('pending', queue_id=7, build_number=4))
            self.assertEqual(observed.observe(result('failed', queue_id=7, build_number=4)), ())
            self.assertFalse((Path(temporary) / 'pending-emit-build_completed.json').exists())

    def test_malformed_results_never_raise(self):
        with tempfile.TemporaryDirectory() as temporary:
            observed = tracker(temporary)
            for bad in (None, 'text', {}, {'receipt': None}, {'receipt': 'x'},
                        {'status': 'passed', 'receipt': {'build_number': 'not-an-int'}}):
                self.assertEqual(observed.observe(bad), ())

    def test_cancellation_records_aborted_only_with_a_build_number(self):
        with tempfile.TemporaryDirectory() as temporary:
            observed = tracker(temporary)
            self.assertEqual(observed.observe_cancelled(
                {'status': 'cancelled', 'receipt': {'queue_id': 7}}), ())
            self.assertEqual(observed.observe_cancelled(
                {'status': 'cancelled', 'receipt': {'queue_id': 7, 'build_number': 4}}),
                ('build_completed',))
            payload = json.loads((Path(temporary) / 'pending-emit-build_completed.json').read_text())
            self.assertEqual(payload['build_result'], 'aborted')
            # A second cancellation does not re-record.
            self.assertEqual(observed.observe_cancelled(
                {'status': 'cancelled', 'receipt': {'queue_id': 7, 'build_number': 4}}), ())

    def test_reconcile_discards_delivered_and_recreates_lost_intents(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            observed = tracker(temporary)
            observed.observe(result('pending', queue_id=7))
            observed.observe(result('pending', queue_id=7, build_number=4))
            # Simulate a crash after the drain delivered build_queued but
            # before it removed the intent, and after the build_started
            # intent file was lost.
            (root / 'pending-emit-build_started.json').unlink()
            delivered = [{'event_kind': 'build_queued',
                          'delivery_id': lifecycle_delivery_id(ACTION_ID, 'build_queued')}]
            restarted = tracker(temporary, event_lookup=lambda *args: delivered)
            self.assertEqual(restarted.reconcile(), {'build_queued'})
            self.assertFalse((root / 'pending-emit-build_queued.json').exists())
            self.assertTrue((root / 'pending-emit-build_started.json').exists())

    def test_delivery_ids_are_deterministic_and_distinct(self):
        first = lifecycle_delivery_id(ACTION_ID, 'build_started')
        self.assertEqual(first, lifecycle_delivery_id(ACTION_ID, JenkinsEventKind.BUILD_STARTED))
        self.assertNotEqual(first, lifecycle_delivery_id(ACTION_ID, 'build_completed'))
        self.assertNotEqual(first, lifecycle_delivery_id(str(uuid4()), 'build_started'))
        stage = stage_delivery_id(ACTION_ID, 'check', 'stage_started')
        self.assertEqual(stage, stage_delivery_id(ACTION_ID, 'check', 'stage_started'))
        self.assertNotEqual(stage, stage_delivery_id(ACTION_ID, 'check', 'stage_completed'))
        self.assertNotEqual(stage, stage_delivery_id(ACTION_ID, 'other', 'stage_started'))


class IntentDrainTest(unittest.TestCase):
    def test_sweep_delivers_and_removes_intents(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binding = root / 'actions' / ACTION_ID
            binding.mkdir(parents=True)
            observed = tracker(binding)
            observed.observe(result('pending', queue_id=7))
            observed.observe(result('pending', queue_id=7, build_number=4))
            emitter = RecordingEmitter()
            drain = IntentDrain(root / 'actions', emitter)
            counts = drain.sweep_once()
            self.assertEqual(counts, {'delivered': 2, 'pending': 0, 'failed': 0})
            self.assertEqual(emitter.kinds(), ['build_queued', 'build_started'])
            self.assertEqual(list(binding.glob('pending-emit-*.json')), [])
            self.assertEqual(list(binding.glob('delivered-emit-*.json')), [])

    def test_failed_emit_leaves_the_intent_pending(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binding = root / 'actions' / ACTION_ID
            binding.mkdir(parents=True)
            observed = tracker(binding)
            observed.observe(result('pending', queue_id=7))
            emitter = RecordingEmitter(fail_kinds={'build_queued'})
            drain = IntentDrain(root / 'actions', emitter)
            counts = drain.sweep_once()
            self.assertEqual(counts, {'delivered': 0, 'pending': 1, 'failed': 1})
            self.assertTrue((binding / 'pending-emit-build_queued.json').exists())
            # The next sweep retries the same intent.
            emitter.fail_kinds = set()
            counts = drain.sweep_once()
            self.assertEqual(counts['delivered'], 1)

    def test_invalid_intent_is_counted_failed_and_left_in_place(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binding = root / 'actions' / ACTION_ID
            binding.mkdir(parents=True)
            (binding / 'pending-emit-build_queued.json').write_text('{not json')
            (binding / 'pending-emit-build_queued.json').chmod(0o600)
            drain = IntentDrain(root / 'actions', RecordingEmitter())
            counts = drain.sweep_once()
            self.assertEqual(counts['failed'], 1)
            self.assertTrue((binding / 'pending-emit-build_queued.json').exists())

    def test_sweep_caps_at_sixteen_intents(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binding = root / 'actions' / ACTION_ID
            binding.mkdir(parents=True)
            for index in range(20):
                payload = JenkinsEventEmit(
                    project_alias='homelab', jenkins_instance='ci-primary',
                    job_name='forge-native-candidate', delivery_id=f'delivery-{index}',
                    event_kind='build_queued', occurred_at='2026-11-24T01:00:00Z',
                    summary='fixture', queue_id=index + 1).semantic_payload()
                path = binding / f'pending-emit-build_queued-{index:02d}.json'
                path.write_text(json.dumps(payload))
                path.chmod(0o600)
            drain = IntentDrain(root / 'actions', RecordingEmitter())
            counts = drain.sweep_once()
            self.assertEqual(counts['delivered'], 16)
            self.assertEqual(len(list(binding.glob('pending-emit-*.json'))), 4)

    def test_drain_thread_delivers_concurrently_and_joins_within_its_bound(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binding = root / 'actions' / ACTION_ID
            binding.mkdir(parents=True)
            emitter = RecordingEmitter()
            drain = IntentDrain(root / 'actions', emitter, interval_seconds=0.01)
            stop = threading.Event()
            drain.start(stop)
            try:
                observed = tracker(binding)
                observed.observe(result('pending', queue_id=7, build_number=4))
                deadline = time.monotonic() + 5
                while set(emitter.kinds()) != {'build_queued', 'build_started'}:
                    self.assertLess(time.monotonic(), deadline, 'drain did not deliver concurrently')
                    time.sleep(0.01)
            finally:
                stop.set()
                started = time.monotonic()
                self.assertTrue(drain.join(timeout=10))
                self.assertLess(time.monotonic() - started, 10)


if __name__ == '__main__':
    unittest.main()
