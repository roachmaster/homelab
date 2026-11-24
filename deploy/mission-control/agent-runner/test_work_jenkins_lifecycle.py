"""Producer tests for the Jenkins lifecycle/stage producer (fix 3, plan 5.4).

The producer turns delivery observations (verify() results, the journaled
submission, and the verifier's progress file) into jenkins_events rows.
These tests run the producer against a fake store that mirrors the real
store's idempotency contract (same delivery id + same digest -> deduplicated;
same delivery id + different digest -> 409 idempotency_conflict), plus a
real-store integration class on a disposable cluster for the codec+store
round trip and the section 5.5 contention bound.

Every attribution is asserted by equality on the emitted event's fields, not
by the mere existence of an event.
"""

import json
import os
from datetime import datetime, timedelta, timezone
import tempfile
import threading
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from uuid import uuid4

import psycopg2

from jenkins_config import parse_jenkins_config
from jenkins_events import JenkinsEventEmit
from jenkins_store import ADVISORY_LOCK_SALT, JenkinsStore
from queue_store import Rejected
from work_external_delivery import durable_json
from work_jenkins_lifecycle import (
    ACTOR,
    LifecycleProducer,
    create_lifecycle_producer,
    lifecycle_delivery_id,
    map_build_result,
    map_stage_status,
    stage_delivery_id,
)
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE

ORIGIN = "https://jenkins.internal:8443"
ACTION_ID = "11111111-2222-3333-4444-555555555555"
MANIFEST = "a" * 64
COMMIT = "b" * 40
TREE = "c" * 40
T0 = datetime(2026, 11, 24, 5, 0, 0, tzinfo=timezone.utc)


def make_jenkins_config(**overrides):
    raw = {
        "owner": "test-owner",
        "allowed_projects": ["homelab"],
        "allowed_instances": {"ci-primary": {"origin": ORIGIN}},
        "allowed_jobs": {"homelab": ["forge-native-candidate", "forge-exact-candidate"]},
        "consumers": [{"name": "observation", "handler": "observation"}],
    }
    raw.update(overrides)
    return parse_jenkins_config(raw)


class FixedClock:
    """A clock the test advances by hand; now() returns a UTC datetime."""

    def __init__(self, start=T0):
        self.current = start

    def __call__(self):
        return self.current

    def advance(self, seconds):
        self.current = self.current + timedelta(seconds=seconds)
        return self.current


class FakeStore:
    """An in-memory store mirroring the real store's idempotency contract."""

    def __init__(self, fail=None, conflict_ids=()):
        # fail: an exception to raise on every emit (transient failure).
        self.fail = fail
        self.conflict_ids = set(conflict_ids)
        self.events = {}  # delivery_id -> JenkinsEventEmit (first emission)
        self.calls = []  # every emit call, in order

    def emit(self, event):
        if not isinstance(event, JenkinsEventEmit):
            raise TypeError("FakeStore.emit requires a JenkinsEventEmit")
        self.calls.append(event)
        if self.fail is not None:
            raise self.fail
        if event.delivery_id in self.conflict_ids:
            raise Rejected(409, "idempotency_conflict")
        digest = event.payload_sha256()
        existing = self.events.get(event.delivery_id)
        if existing is not None:
            if existing.payload_sha256() != digest:
                raise Rejected(409, "idempotency_conflict")
            return {"event_id": f"fake-{event.delivery_id}", "deduplicated": True,
                    "payload_sha256": digest}
        self.events[event.delivery_id] = event
        return {"event_id": f"fake-{event.delivery_id}", "deduplicated": False,
                "payload_sha256": digest}


def make_binding():
    return {
        "version": "3",
        "request": {
            "action_id": ACTION_ID,
            "project_alias": "homelab",
            "candidate_commit": COMMIT,
            "candidate_tree": TREE,
            "verification_profile": "forge-sdlc-jenkins-v3",
            "branch_scope": "detached",
        },
        "native_gate_manifest_sha256": MANIFEST,
        "scope": {
            "owner_id": "test-owner",
            "project_alias": "homelab",
            "task_id": str(uuid4()),
            "attempt_id": str(uuid4()),
            "review_id": str(uuid4()),
            "workflow_id": str(uuid4()),
            "correlation_id": str(uuid4()),
        },
    }


def queued_observation(queue_id=42):
    return {"status": "pending",
            "receipt": {"provider": "jenkins", "job": "forge-native-candidate",
                        "queue_id": queue_id, "job_number": None}}


def building_observation(build_number=7, queue_id=42):
    return {"status": "pending",
            "receipt": {"provider": "jenkins", "job": "forge-native-candidate",
                        "queue_id": queue_id, "build_number": build_number}}


def terminal_observation(status="passed", result="SUCCESS", stages=None, build_number=7, queue_id=42):
    return {"status": status,
            "receipt": {"provider": "jenkins", "job": "forge-native-candidate",
                        "queue_id": queue_id, "build_number": build_number,
                        "jenkins_result": result,
                        "stages": stages if stages is not None else [
                            {"name": "check_0", "status": "passed", "returncode": 0, "elapsed_ms": 1200},
                            {"name": "check_1", "status": "passed", "returncode": 0, "elapsed_ms": 900},
                        ]}}


class ProducerTestCase(unittest.TestCase):
    """Shared fixture: one action directory, journal, binding, producer."""

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="forge-lifecycle-")
        self.root = Path(self.temporary.name)
        self.state_root = self.root / "state"
        self.results_root = self.root / "results"
        self.state_root.mkdir()
        self.results_root.mkdir()
        self.directory = self.state_root / "actions" / ACTION_ID
        self.directory.mkdir(parents=True)
        self.clock = FixedClock()
        self.store = FakeStore()
        self.config = make_jenkins_config()
        self.delivery_config = SimpleNamespace(
            state_root=self.state_root,
            results_root=self.results_root,
            jenkins_job="forge-native-candidate",
            jenkins_instance="ci-primary",
            jenkins_config=self.root / "jenkins-cli.json",
        )
        (self.root / "jenkins-cli.json").write_text(json.dumps({
            "endpoint": ORIGIN, "username": "bot",
            "token_file": str(self.root / "token.txt")}))
        self.binding = make_binding()
        self.producer = LifecycleProducer(
            store=self.store, delivery_config=self.delivery_config,
            jenkins_config=self.config, clock=self.clock,
            progress_owner=os.geteuid())
        # The binding lives on disk so drain_all (which reads it) works.
        self.write_binding()

    def tearDown(self):
        self.temporary.cleanup()

    # -- fixture helpers ----------------------------------------------------

    def write_journal(self, queue_id=42, build_number=None, instance="ci-primary"):
        journal = {"parameters": {"job": "forge-native-candidate"}, "post_started": True,
                   "jenkins_instance": instance}
        if queue_id is not None:
            journal["queue_id"] = queue_id
        if build_number is not None:
            journal["build_number"] = build_number
        durable_json(self.directory / "jenkins.json", journal)
        # The submitted timestamp is the journal file's mtime; pin it to T0.
        stamp = T0.timestamp()
        os.utime(self.directory / "jenkins.json", (stamp, stamp))
        return journal

    def write_binding(self):
        durable_json(self.directory / "native-binding.json", self.binding)

    def write_progress(self, records, header=True, owner_check=True):
        path = self.results_root / f"{ACTION_ID}.progress.jsonl"
        lines = []
        if header:
            lines.append({"action_id": ACTION_ID, "gate_manifest_sha256": MANIFEST})
        lines.extend(records)
        with open(path, "w", encoding="utf-8") as handle:
            for line in lines:
                handle.write(json.dumps(line) + "\n")
        os.chmod(path, 0o600)
        return path

    def start_record(self, index, monotonic_ms=1000):
        return {"index": index, "kind": "stage_started", "rc": None,
                "elapsed_ms": None, "monotonic_ms": monotonic_ms}

    def completion_record(self, index, rc=0, elapsed_ms=1200, monotonic_ms=2000):
        return {"index": index, "kind": "stage_completed", "rc": rc,
                "elapsed_ms": elapsed_ms, "monotonic_ms": monotonic_ms}

    def read_json(self, name):
        path = self.directory / name
        if not path.exists():
            return None
        return json.loads(path.read_text(encoding="utf-8"))

    def read_history(self):
        path = self.directory / "lifecycle.jsonl"
        if not path.exists():
            return []
        return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]

    def events_by_kind(self):
        return {event.event_kind.value: event for event in self.store.events.values()}

    def process(self, observation=None):
        return self.producer.process_action(ACTION_ID, binding=self.binding, observation=observation)


class LifecycleFlowTest(ProducerTestCase):
    """Lifecycle transitions: queued -> building -> finished, in order."""

    def test_queued_then_building_then_finished_emit_in_order(self):
        self.write_journal(queue_id=42)
        self.clock.advance(10)
        self.assertTrue(self.process(queued_observation()))
        self.clock.advance(20)
        self.assertTrue(self.process(building_observation()))
        self.clock.advance(30)
        self.assertTrue(self.process(terminal_observation(stages=[])))
        events = self.events_by_kind()
        self.assertEqual(set(events), {"build_queued", "build_started", "build_completed"})
        queued = events["build_queued"]
        self.assertEqual(queued.queue_id, 42)
        self.assertIsNone(queued.build_number)
        self.assertEqual(queued.occurred_at, "2026-11-24T05:00:00Z")  # submitted timestamp
        self.assertFalse(queued.backfilled)
        self.assertEqual(queued.actor, ACTOR)
        self.assertEqual(queued.delivery_id, lifecycle_delivery_id(ACTION_ID, "build_queued"))
        started = events["build_started"]
        self.assertEqual(started.build_number, 7)
        self.assertEqual(started.occurred_at, "2026-11-24T05:00:30Z")
        self.assertFalse(started.backfilled)  # observed live
        completed = events["build_completed"]
        self.assertEqual(completed.build_number, 7)
        self.assertEqual(completed.build_result.value, "success")
        self.assertEqual(completed.occurred_at, "2026-11-24T05:01:00Z")
        self.assertFalse(completed.backfilled)
        history = self.read_history()
        self.assertEqual([record["state"] for record in history],
                         ["submitted", "queued", "building", "finished"])
        self.assertIsNone(history[0]["payload"])

    def test_fast_build_backfills_queued_started_completed_in_order(self):
        # First observation is already terminal: queued and started are
        # backfilled from the submitted record, in lifecycle order.
        self.write_journal(queue_id=42, build_number=7)
        self.clock.advance(60)
        self.assertTrue(self.process(terminal_observation(stages=[])))
        events = self.events_by_kind()
        self.assertEqual(set(events), {"build_queued", "build_started", "build_completed"})
        self.assertEqual(events["build_queued"].occurred_at, "2026-11-24T05:00:00Z")
        self.assertFalse(events["build_queued"].backfilled)
        self.assertEqual(events["build_started"].occurred_at, "2026-11-24T05:01:00Z")
        self.assertTrue(events["build_started"].backfilled)
        self.assertEqual(events["build_completed"].occurred_at, "2026-11-24T05:01:00Z")
        self.assertTrue(events["build_completed"].backfilled)
        history = self.read_history()
        self.assertEqual([record["state"] for record in history],
                         ["submitted", "queued", "building", "finished"])

    def test_fast_build_without_queue_id_records_queued_without_an_event(self):
        # A build that went straight to a build number never observed a
        # queue id; build_queued requires one, so the transition is recorded
        # with no payload and no event is emitted for it.
        self.write_journal(queue_id=None, build_number=7)
        self.assertTrue(self.process(terminal_observation(queue_id=None, stages=[])))
        events = self.events_by_kind()
        self.assertEqual(set(events), {"build_started", "build_completed"})
        history = self.read_history()
        self.assertEqual([record["state"] for record in history],
                         ["submitted", "queued", "building", "finished"])
        self.assertIsNone(history[1]["payload"])

    def test_uncertain_submission_emits_nothing_until_find_reconciles(self):
        # No queue id and no build number anywhere: the submitted record is
        # the whole history and no lifecycle event is emitted.
        self.write_journal(queue_id=None)
        self.assertTrue(self.process({"status": "pending", "receipt": {}}))
        self.assertEqual(self.store.events, {})
        self.assertEqual([record["state"] for record in self.read_history()], ["submitted"])
        # A later poll reconciles the queue id; the queued event follows.
        self.clock.advance(15)
        self.assertTrue(self.process(queued_observation()))
        self.assertEqual(set(self.events_by_kind()), {"build_queued"})

    def test_mismatched_observation_is_ignored(self):
        self.write_journal(queue_id=42, build_number=7)
        self.assertTrue(self.process(building_observation(build_number=8)))
        self.assertEqual(self.store.events, {})
        # The submission itself is still anchored; only the observation is
        # ignored, so no transition beyond submitted is recorded.
        self.assertEqual([record["state"] for record in self.read_history()], ["submitted"])

    def test_no_journal_means_no_emission(self):
        self.assertFalse(self.process(queued_observation()))
        self.assertEqual(self.store.events, {})

    def test_no_instance_means_no_emission(self):
        self.delivery_config.jenkins_instance = None
        self.write_journal(queue_id=42, instance=None)
        self.producer = LifecycleProducer(store=self.store, delivery_config=self.delivery_config,
                                          jenkins_config=self.config, clock=self.clock,
                                          progress_owner=os.geteuid())
        self.assertFalse(self.process(queued_observation()))
        self.assertEqual(self.store.events, {})

    def test_failed_build_maps_jenkins_result(self):
        self.write_journal(queue_id=42, build_number=7)
        self.assertTrue(self.process(terminal_observation(status="failed", result="FAILURE",
                                                          stages=[{"name": "check_0", "status": "failed",
                                                                   "returncode": 1, "elapsed_ms": 500}])))
        completed = self.events_by_kind()["build_completed"]
        self.assertEqual(completed.build_result.value, "failure")

    def test_unmapped_jenkins_result_emits_no_completed_payload(self):
        self.write_journal(queue_id=42, build_number=7)
        self.assertTrue(self.process(terminal_observation(result="SOMETHING_NEW", stages=[])))
        events = self.events_by_kind()
        self.assertEqual(set(events), {"build_queued", "build_started"})
        history = self.read_history()
        self.assertEqual(history[-1]["state"], "finished")
        self.assertIsNone(history[-1]["payload"])


class StageProgressTest(ProducerTestCase):
    """Stage events from the verifier's progress file."""

    def test_live_stage_start_emits_within_the_poll(self):
        self.write_journal(queue_id=42, build_number=7)
        self.assertTrue(self.process(building_observation()))
        self.clock.advance(5)
        self.write_progress([self.start_record(0)])
        self.assertTrue(self.process(building_observation()))
        events = self.store.events
        self.assertEqual(len(events), 3)  # queued, started, stage_started
        started = events[stage_delivery_id(ACTION_ID, "stage_started", 0)]
        self.assertEqual(started.event_kind.value, "stage_started")
        self.assertEqual(started.stage_name, "check_0")
        self.assertEqual(started.build_number, 7)
        self.assertEqual(started.occurred_at, "2026-11-24T05:00:05Z")
        self.assertEqual(started.actor, ACTOR)
        self.assertIsNone(started.stage_status)
        self.assertFalse(started.backfilled)

    def test_forged_log_markers_produce_no_stage_event(self):
        # Fake FORGE_FIXED_CHECK_START/END lines in the LOG file are not the
        # progress channel; the producer never reads the log.
        self.write_journal(queue_id=42, build_number=7)
        self.assertTrue(self.process(building_observation()))
        log_path = self.results_root / f"{ACTION_ID}.log"
        log_path.write_text("FORGE_FIXED_CHECK_START 0\nFORGE_FIXED_CHECK_END 0 0\n",
                            encoding="utf-8")
        self.assertTrue(self.process(building_observation()))
        self.assertEqual(set(self.events_by_kind()), {"build_queued", "build_started"})

    def test_progress_record_for_another_action_is_ignored(self):
        self.write_journal(queue_id=42, build_number=7)
        self.assertTrue(self.process(building_observation()))
        other = str(uuid4())
        # A progress file whose header binds it to a different action.
        path = self.results_root / f"{ACTION_ID}.progress.jsonl"
        path.write_text(json.dumps({"action_id": other, "gate_manifest_sha256": MANIFEST}) + "\n"
                        + json.dumps(self.start_record(0)) + "\n", encoding="utf-8")
        self.assertTrue(self.process(building_observation()))
        self.assertNotIn(stage_delivery_id(ACTION_ID, "stage_started", 0), self.store.events)

    def test_progress_header_manifest_mismatch_ignores_the_file(self):
        self.write_journal(queue_id=42, build_number=7)
        self.assertTrue(self.process(building_observation()))
        path = self.results_root / f"{ACTION_ID}.progress.jsonl"
        path.write_text(json.dumps({"action_id": ACTION_ID, "gate_manifest_sha256": "f" * 64}) + "\n"
                        + json.dumps(self.start_record(0)) + "\n", encoding="utf-8")
        self.assertTrue(self.process(building_observation()))
        self.assertNotIn(stage_delivery_id(ACTION_ID, "stage_started", 0), self.store.events)

    def test_progress_file_with_wrong_owner_is_ignored(self):
        # The producer runs with progress_owner=0 (root) in production; a
        # file owned by the test user fails the private-file check.
        self.write_journal(queue_id=42, build_number=7)
        self.assertTrue(self.process(building_observation()))
        self.producer = LifecycleProducer(store=self.store, delivery_config=self.delivery_config,
                                          jenkins_config=self.config, clock=self.clock,
                                          progress_owner=0)
        self.write_progress([self.start_record(0)])
        self.assertTrue(self.process(building_observation()))
        self.assertNotIn(stage_delivery_id(ACTION_ID, "stage_started", 0), self.store.events)

    def test_group_writable_progress_file_is_ignored(self):
        self.write_journal(queue_id=42, build_number=7)
        self.assertTrue(self.process(building_observation()))
        path = self.write_progress([self.start_record(0)])
        os.chmod(path, 0o660)
        self.assertTrue(self.process(building_observation()))
        self.assertNotIn(stage_delivery_id(ACTION_ID, "stage_started", 0), self.store.events)

    def test_completion_is_not_emitted_live(self):
        self.write_journal(queue_id=42, build_number=7)
        self.assertTrue(self.process(building_observation()))
        self.write_progress([self.start_record(0), self.completion_record(0, rc=0)])
        self.assertTrue(self.process(building_observation()))
        self.assertNotIn(stage_delivery_id(ACTION_ID, "stage_completed", 0), self.store.events)
        state = self.read_json("progress-state.json")
        self.assertEqual(state["completions"]["0"]["rc"], 0)


class TerminalReconciliationTest(ProducerTestCase):
    """Terminal stage decisions: agree, conflict, gap, start-only."""

    def _terminal(self, stages, progress_records):
        self.write_journal(queue_id=42, build_number=7)
        self.assertTrue(self.process(building_observation()))
        self.write_progress(progress_records)
        self.assertTrue(self.process(building_observation()))
        self.clock.advance(30)
        self.assertTrue(self.process(terminal_observation(stages=stages)))
        return self.read_json("reconciliation.json")

    def test_agree_uses_progress_observed_at(self):
        decisions = self._terminal(
            [{"name": "check_0", "status": "passed", "returncode": 0, "elapsed_ms": 1200}],
            [self.start_record(0), self.completion_record(0, rc=0)])
        self.assertEqual(decisions["0"]["outcome"], "eligible")
        event = self.store.events[stage_delivery_id(ACTION_ID, "stage_completed", 0)]
        self.assertEqual(event.stage_status.value, "success")
        self.assertEqual(event.occurred_at, "2026-11-24T05:00:00Z")  # progress first-sight time
        self.assertFalse(event.backfilled)
        self.assertFalse(event.reconciled)
        self.assertFalse(event.completion_time_estimated)

    def test_conflict_suppresses_and_the_receipt_wins(self):
        decisions = self._terminal(
            [{"name": "check_0", "status": "passed", "returncode": 0, "elapsed_ms": 1200}],
            [self.start_record(0), self.completion_record(0, rc=1)])
        self.assertEqual(decisions["0"]["outcome"], "suppressed_conflict")
        self.assertIsNone(decisions["0"]["payload"])
        self.assertNotIn(stage_delivery_id(ACTION_ID, "stage_completed", 0), self.store.events)
        # Suppression survives a drain and a restart: no event ever appears.
        self.producer = LifecycleProducer(store=self.store, delivery_config=self.delivery_config,
                                          jenkins_config=self.config, clock=self.clock,
                                          progress_owner=os.geteuid())
        self.producer.drain_all()
        self.assertNotIn(stage_delivery_id(ACTION_ID, "stage_completed", 0), self.store.events)

    def test_conflict_completion_without_receipt_entry_is_suppressed(self):
        decisions = self._terminal(
            [{"name": "check_1", "status": "passed", "returncode": 0, "elapsed_ms": 900}],
            [self.start_record(0), self.completion_record(0, rc=0)])
        self.assertEqual(decisions["0"]["outcome"], "suppressed_conflict")
        self.assertNotIn(stage_delivery_id(ACTION_ID, "stage_completed", 0), self.store.events)

    def test_gap_is_reconciled_at_terminal_time(self):
        # The receipt has the stage and NO progress record exists at all
        # (neither start nor completion): a gap, reconciled at terminal time
        # with no estimated completion.
        decisions = self._terminal(
            [{"name": "check_0", "status": "passed", "returncode": 0, "elapsed_ms": 1200}],
            [])
        self.assertEqual(decisions["0"]["outcome"], "eligible")
        event = self.store.events[stage_delivery_id(ACTION_ID, "stage_completed", 0)]
        self.assertEqual(event.stage_status.value, "success")
        self.assertEqual(event.occurred_at, "2026-11-24T05:00:30Z")  # reconciliation time
        self.assertTrue(event.backfilled)
        self.assertTrue(event.reconciled)
        self.assertFalse(event.completion_time_estimated)

    def test_start_only_estimates_the_completion(self):
        # The start record exists and the receipt has the stage, but no
        # progress completion was ever observed: the completion is estimated.
        decisions = self._terminal(
            [{"name": "check_0", "status": "failed", "returncode": 124, "elapsed_ms": 5000}],
            [self.start_record(0)])
        self.assertEqual(decisions["0"]["outcome"], "eligible")
        event = self.store.events[stage_delivery_id(ACTION_ID, "stage_completed", 0)]
        self.assertEqual(event.stage_status.value, "aborted")  # rc 124
        self.assertTrue(event.backfilled)
        self.assertTrue(event.reconciled)
        self.assertTrue(event.completion_time_estimated)
        self.assertEqual(event.started_at, "2026-11-24T05:00:00Z")

    def test_start_only_without_receipt_entry_has_no_decision(self):
        decisions = self._terminal(
            [{"name": "check_1", "status": "passed", "returncode": 0, "elapsed_ms": 900}],
            [self.start_record(0)])
        self.assertNotIn("0", decisions)
        self.assertNotIn(stage_delivery_id(ACTION_ID, "stage_completed", 0), self.store.events)

    def test_terminal_observation_from_disk_reconciles_after_restart(self):
        # The terminal observation was written to observation.json but the
        # producer never saw it live; a later drain reads it from disk.
        self.write_journal(queue_id=42, build_number=7)
        self.assertTrue(self.process(building_observation()))
        self.write_progress([self.start_record(0), self.completion_record(0, rc=0)])
        self.assertTrue(self.process(building_observation()))
        durable_json(self.directory / "observation.json",
                     terminal_observation(stages=[{"name": "check_0", "status": "passed",
                                                   "returncode": 0, "elapsed_ms": 1200}]))
        self.producer = LifecycleProducer(store=self.store, delivery_config=self.delivery_config,
                                          jenkins_config=self.config, clock=self.clock,
                                          progress_owner=os.geteuid())
        self.producer.drain_all()
        event = self.store.events[stage_delivery_id(ACTION_ID, "stage_completed", 0)]
        self.assertEqual(event.stage_status.value, "success")


class DrainAndCrashTest(ProducerTestCase):
    """Deterministic reconstruction across drains and restarts."""

    def test_drain_reconstructs_a_lost_intent_from_history(self):
        self.write_journal(queue_id=42)
        self.assertTrue(self.process(queued_observation()))
        self.assertIn(lifecycle_delivery_id(ACTION_ID, "build_queued"), self.store.events)
        # Simulate a crash between the history append and the intent: the
        # intent file is gone, the history payload survives.
        (self.directory / "pending-emissions.json").unlink()
        (self.directory / "emitted.json").unlink()
        self.store.events.clear()
        self.producer.drain_all()
        event = self.store.events[lifecycle_delivery_id(ACTION_ID, "build_queued")]
        self.assertEqual(event.occurred_at, "2026-11-24T05:00:00Z")  # history bytes, not now

    def test_decision_then_crash_recreates_intents_from_decisions(self):
        self.write_journal(queue_id=42, build_number=7)
        self.assertTrue(self.process(building_observation()))
        self.write_progress([self.start_record(0), self.completion_record(0, rc=0)])
        self.assertTrue(self.process(building_observation()))
        self.assertTrue(self.process(terminal_observation(
            stages=[{"name": "check_0", "status": "passed", "returncode": 0, "elapsed_ms": 1200}])))
        self.assertIn(stage_delivery_id(ACTION_ID, "stage_completed", 0), self.store.events)
        (self.directory / "pending-emissions.json").unlink()
        (self.directory / "emitted.json").unlink()
        self.store.events.clear()
        self.producer.drain_all()
        event = self.store.events[stage_delivery_id(ACTION_ID, "stage_completed", 0)]
        self.assertEqual(event.occurred_at, "2026-11-24T05:00:00Z")

    def test_emit_then_crash_before_confirmation_deduplicates(self):
        self.write_journal(queue_id=42)
        self.assertTrue(self.process(queued_observation()))
        delivery_id = lifecycle_delivery_id(ACTION_ID, "build_queued")
        self.assertIn(delivery_id, self.store.events)
        first_digest = self.store.events[delivery_id].payload_sha256()
        # The emit landed in the store but the process crashed before the
        # confirmation (emitted.json) was written: the intent still exists
        # with the original payload bytes, and emitted.json is gone.
        history = self.read_history()
        queued_payload = [record["payload"] for record in history
                          if record["state"] == "queued"][0]
        durable_json(self.directory / "pending-emissions.json", {
            delivery_id: {"payload": queued_payload, "created_at": "2026-11-24T05:00:00Z",
                          "attempt_count": 0, "last_attempt_at": None,
                          "progress_record_offset": None}})
        (self.directory / "emitted.json").unlink()
        calls_before = len(self.store.calls)
        self.producer.drain_all()
        self.assertEqual(len(self.store.events), 1)  # no second row
        self.assertEqual(self.store.events[delivery_id].payload_sha256(), first_digest)
        self.assertEqual(self.store.calls[-1].payload_sha256(), first_digest)
        self.assertGreater(len(self.store.calls), calls_before)

    def test_start_intent_is_reused_verbatim_on_reread(self):
        self.write_journal(queue_id=42, build_number=7)
        self.assertTrue(self.process(building_observation()))
        # The first emit fails, so the start intent stays pending. Then the
        # progress-state save is lost (the crash window between the intent
        # write and the state save): the start record is read again, and
        # the existing intent is reused verbatim, not rebuilt with a new
        # timestamp.
        self.store.fail = Rejected(503, "queue_full")
        self.write_progress([self.start_record(0)])
        self.assertTrue(self.process(building_observation()))
        delivery_id = stage_delivery_id(ACTION_ID, "stage_started", 0)
        intent = self.read_json("pending-emissions.json")[delivery_id]
        self.assertEqual(intent["payload"]["occurred_at"], "2026-11-24T05:00:00Z")
        (self.directory / "progress-state.json").unlink()
        self.store.fail = None
        self.clock.advance(120)
        self.assertTrue(self.process(building_observation()))
        event = self.store.events[delivery_id]
        self.assertEqual(event.occurred_at, "2026-11-24T05:00:00Z")
        self.assertEqual(event.payload_sha256(),
                         JenkinsEventEmit(**intent["payload"]).payload_sha256())

    def test_restart_before_first_observation_emits_nothing_twice(self):
        self.write_journal(queue_id=42)
        self.assertTrue(self.process(queued_observation()))
        self.producer = LifecycleProducer(store=self.store, delivery_config=self.delivery_config,
                                          jenkins_config=self.config, clock=self.clock,
                                          progress_owner=os.geteuid())
        self.clock.advance(300)
        self.producer.drain_all()
        self.assertEqual(len(self.store.events), 1)
        self.assertEqual(len(self.read_history()), 2)  # submitted + queued, unchanged

    def test_transient_emit_failure_backs_off_then_retries(self):
        self.store.fail = Rejected(503, "queue_full")
        self.write_journal(queue_id=42)
        self.assertTrue(self.process(queued_observation()))
        self.assertEqual(self.store.events, {})
        intent = self.read_json("pending-emissions.json")[lifecycle_delivery_id(ACTION_ID, "build_queued")]
        self.assertEqual(intent["attempt_count"], 1)
        # Inside the 5s backoff: no retry.
        self.clock.advance(4)
        self.producer.drain_all()
        self.assertEqual(self.read_json("pending-emissions.json")
                         [lifecycle_delivery_id(ACTION_ID, "build_queued")]["attempt_count"], 1)
        # Past the backoff: a retry happens (and fails again, 15s next).
        self.clock.advance(2)
        self.producer.drain_all()
        intent = self.read_json("pending-emissions.json")[lifecycle_delivery_id(ACTION_ID, "build_queued")]
        self.assertEqual(intent["attempt_count"], 2)
        # The store recovers; the next due retry delivers.
        self.store.fail = None
        self.clock.advance(16)
        self.producer.drain_all()
        self.assertIn(lifecycle_delivery_id(ACTION_ID, "build_queued"), self.store.events)

    def test_intent_older_than_24h_is_escalated_not_retried(self):
        self.store.fail = Rejected(503, "queue_full")
        self.write_journal(queue_id=42)
        self.assertTrue(self.process(queued_observation()))
        self.clock.advance(25 * 3600)
        self.producer.drain_all()
        intent = self.read_json("pending-emissions.json")[lifecycle_delivery_id(ACTION_ID, "build_queued")]
        self.assertTrue(intent["undeliverable"])
        self.assertEqual(intent["undeliverable_reason"], "max_age_exceeded")
        self.assertEqual(self.store.events, {})

    def test_idempotency_conflict_escalates_the_intent(self):
        self.store = FakeStore(conflict_ids={lifecycle_delivery_id(ACTION_ID, "build_queued")})
        self.producer = LifecycleProducer(store=self.store, delivery_config=self.delivery_config,
                                          jenkins_config=self.config, clock=self.clock,
                                          progress_owner=os.geteuid())
        self.write_journal(queue_id=42)
        self.assertTrue(self.process(queued_observation()))
        intent = self.read_json("pending-emissions.json")[lifecycle_delivery_id(ACTION_ID, "build_queued")]
        self.assertTrue(intent["undeliverable"])
        self.assertEqual(intent["undeliverable_reason"], "idempotency_conflict")
        # A later drain does not retry it.
        self.clock.advance(600)
        self.producer.drain_all()
        self.assertEqual(len(self.store.calls), 1)


class MappingTest(unittest.TestCase):
    def test_build_result_mapping(self):
        self.assertEqual(map_build_result("SUCCESS"), "success")
        self.assertEqual(map_build_result("failure"), "failure")
        self.assertEqual(map_build_result("UNSTABLE"), "unstable")
        self.assertEqual(map_build_result("ABORTED"), "aborted")
        self.assertEqual(map_build_result("NOT_BUILT"), "not_built")
        self.assertIsNone(map_build_result("SOMETHING_NEW"))
        self.assertIsNone(map_build_result(None))

    def test_stage_status_mapping(self):
        self.assertEqual(map_stage_status(0), "success")
        self.assertEqual(map_stage_status(124), "aborted")
        self.assertEqual(map_stage_status(1), "failure")
        self.assertEqual(map_stage_status(137), "failure")


class FactoryTest(ProducerTestCase):
    def test_factory_returns_none_without_instance(self):
        self.delivery_config.jenkins_instance = None
        self.assertIsNone(create_lifecycle_producer(
            self.delivery_config, jenkins_config=self.config,
            connect_factory=lambda: None))

    def test_factory_validates_origin_match(self):
        self.delivery_config.jenkins_config = self.root / "jenkins-cli.json"
        producer = create_lifecycle_producer(
            self.delivery_config, jenkins_config=self.config,
            connect_factory=lambda: None)
        self.assertIsInstance(producer, LifecycleProducer)

    def test_factory_rejects_a_different_delivery_origin(self):
        (self.root / "jenkins-cli.json").write_text(json.dumps({
            "endpoint": "https://jenkins.other:8443", "username": "bot",
            "token_file": str(self.root / "token.txt")}))
        with self.assertRaises(ValueError):
            create_lifecycle_producer(
                self.delivery_config, jenkins_config=self.config,
                connect_factory=lambda: None)

    def test_factory_rejects_an_unadmitted_instance(self):
        self.delivery_config.jenkins_instance = "ci-other"
        with self.assertRaises(ValueError):
            create_lifecycle_producer(
                self.delivery_config, jenkins_config=self.config,
                connect_factory=lambda: None)

    def test_factory_requires_a_connect_factory(self):
        with self.assertRaises(ValueError):
            create_lifecycle_producer(self.delivery_config, jenkins_config=self.config)


@unittest.skipUnless(PG16_AVAILABLE, "Disposable PostgreSQL 16 binary not available")
class LifecycleStoreIntegrationTest(unittest.TestCase):
    """Real codec + real store: round trip, dedup, and the 5.5 lock bound."""

    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()

    @classmethod
    def tearDownClass(cls):
        cls.cluster.stop()

    def setUp(self):
        self.cluster.reset_jenkins_tables()
        self.config = make_jenkins_config()
        self.store = JenkinsStore(self.config, self.cluster.connect)

    def _producer_payload(self, **overrides):
        payload = {
            "event_kind": "stage_completed",
            "delivery_id": "11111111-2222-3333-4444-555555555555:stage_completed:0",
            "project_alias": "homelab",
            "jenkins_instance": "ci-primary",
            "job_name": "forge-native-candidate",
            "commit_sha": COMMIT,
            "correlation_id": ACTION_ID,
            "occurred_at": "2026-11-24T05:01:20Z",
            "summary": "Check 0 completed: success",
            "actor": ACTOR,
            "stage_name": "check_0",
            "stage_status": "success",
            "build_number": 7,
            "queue_id": 42,
            "build_url": f"{ORIGIN}/job/forge-native-candidate/7/",
            "backfilled": False,
            "reconciled": False,
            "completion_time_estimated": False,
        }
        payload.update(overrides)
        return payload

    def test_estimated_completion_round_trips_through_codec_and_store(self):
        payload = self._producer_payload(
            delivery_id="11111111-2222-3333-4444-555555555555:stage_completed:1",
            summary="Check 1 completed: failure",
            stage_name="check_1", stage_status="failure",
            occurred_at="2026-11-24T05:01:25Z",
            backfilled=True, reconciled=True, completion_time_estimated=True,
            started_at="2026-11-24T05:01:15Z")
        event = JenkinsEventEmit(**payload)
        result = self.store.emit(event)
        self.assertFalse(result["deduplicated"])
        stored = self.store.get_event(project_alias="homelab", event_id=result["event_id"])["event"]
        self.assertTrue(stored["completion_time_estimated"])
        self.assertEqual(stored["started_at"], "2026-11-24T05:01:15Z")
        self.assertTrue(stored["backfilled"])
        self.assertTrue(stored["reconciled"])
        # A retried emission of the same persisted payload produces an
        # identical semantic digest and is deduplicated, not stored twice.
        again = self.store.emit(JenkinsEventEmit(**payload))
        self.assertTrue(again["deduplicated"])
        self.assertEqual(again["payload_sha256"], result["payload_sha256"])
        self.assertEqual(again["event_id"], result["event_id"])

    def test_emit_blocks_on_the_project_lock_within_the_lock_bound(self):
        # Hold the project advisory lock from a second connection, then emit
        # through the real store: the 4s lock_timeout must fail the emit
        # rather than waiting indefinitely.
        holder = self.cluster.connect()
        holder.autocommit = False
        try:
            with holder.cursor() as cursor:
                cursor.execute(
                    "SELECT pg_advisory_xact_lock(hashtextextended(%s::text, %s::bigint))",
                    (f"{self.config.owner}\nhomelab", ADVISORY_LOCK_SALT))
                event = JenkinsEventEmit(**self._producer_payload())
                started = time.monotonic()
                with self.assertRaises(Exception):
                    self.store.emit(event)
                elapsed = time.monotonic() - started
                self.assertLess(elapsed, 10.0)
                self.assertGreater(elapsed, 2.0)  # it waited on the lock, then timed out
        finally:
            holder.rollback()
            holder.close()


if __name__ == "__main__":
    unittest.main()
