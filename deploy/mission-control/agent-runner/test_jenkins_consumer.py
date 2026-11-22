"""Disposable PostgreSQL 16 integration tests for JenkinsEventConsumer and handler substitution."""

import threading
import time
import unittest

from jenkins_config import parse_jenkins_config
from jenkins_consumer import (
    JenkinsConsumerFactory,
    ObservationJenkinsHandler,
    PermanentHandlerError,
    RetryableHandlerError,
)
from jenkins_events import (
    ConsumptionReceipt,
    JenkinsBuildResult,
    JenkinsEventEmit,
    JenkinsEventKind,
    sha256_canonical,
)
from jenkins_store import JenkinsStore
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE

ORIGIN = "https://jenkins.internal:8443"
OCCURRED_AT = "2026-11-21T05:00:00Z"


def make_config(**overrides):
    raw = {
        "owner": "mission-control",
        "allowed_projects": ["homelab"],
        "allowed_instances": {"ci-primary": {"origin": ORIGIN}},
        "allowed_jobs": {"homelab": ["forge/ci-main"]},
        "consumers": [
            {"name": "observation", "handler": "observation", "max_attempts": 2},
        ],
        "max_pending_deliveries": 64,
        "lease_seconds": 5,
        "renew_seconds": 1,
        "poll_interval_seconds": 2,
        "backoff_seconds": 1,
        "max_backoff_seconds": 4,
    }
    raw.update(overrides)
    return parse_jenkins_config(raw)


def wait_until(predicate, timeout=5.0, interval=0.05):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(interval)
    return predicate()


@unittest.skipUnless(PG16_AVAILABLE, "Disposable PostgreSQL 16 binary not available")
class JenkinsConsumerPostgresTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()

    @classmethod
    def tearDownClass(cls):
        cls.cluster.stop()

    def setUp(self):
        self.cluster.reset_jenkins_tables()

    def test_background_consumer_listen_wakeup_monotonic_observation_and_clean_shutdown(self):
        cfg = make_config()
        store = JenkinsStore(cfg, self.cluster.connect)
        consumer = JenkinsConsumerFactory(cfg, store, worker_id="consumer-live").create()

        before_threads = threading.active_count()
        consumer.start()
        try:
            started = store.emit(
                JenkinsEventEmit(
                    project_alias="homelab",
                    jenkins_instance="ci-primary",
                    job_name="forge/ci-main",
                    delivery_id="live-started-55",
                    event_kind=JenkinsEventKind.BUILD_STARTED,
                    occurred_at=OCCURRED_AT,
                    summary="Build #55 started",
                    build_number=55,
                    commit_sha="a" * 40,
                    branch="main",
                    build_url=f"{ORIGIN}/job/forge/job/ci-main/55/",
                )
            )
            completed = store.emit(
                JenkinsEventEmit(
                    project_alias="homelab",
                    jenkins_instance="ci-primary",
                    job_name="forge/ci-main",
                    delivery_id="live-completed-55",
                    event_kind=JenkinsEventKind.BUILD_COMPLETED,
                    occurred_at="2026-11-21T05:01:00Z",
                    summary="Build #55 succeeded",
                    build_number=55,
                    build_result=JenkinsBuildResult.SUCCESS,
                    duration_ms=60000,
                )
            )

            done = wait_until(
                lambda: len(
                    store.list_deliveries(project_alias="homelab", status="consumed")["deliveries"]
                ) == 2
            )
            self.assertTrue(done, "Background consumer did not consume both deliveries in time")

            obs = store.get_observation(
                project_alias="homelab",
                jenkins_instance="ci-primary",
                job_name="forge/ci-main",
                observation_key="build:55",
            )
            self.assertIsNotNone(obs)
            self.assertEqual(obs["last_event_id"], completed["event_id"])
            self.assertEqual(obs["last_event_sequence"], 2)
            self.assertEqual(obs["last_event_kind"], "build_completed")
            self.assertEqual(obs["build_result"], "success")
            # Commit SHA and build_url from started event are preserved by COALESCE.
            self.assertEqual(obs["commit_sha"], "a" * 40)
            self.assertEqual(obs["build_url"], f"{ORIGIN}/job/forge/job/ci-main/55/")
        finally:
            consumer.close(timeout=5.0)

        self.assertLessEqual(threading.active_count(), before_threads)

        # Verify monotonic guard directly: re-upserting older sequence 1 keeps sequence 2.
        from jenkins_store import _row_to_event
        with store._transaction() as cursor:
            cursor.execute("SELECT * FROM forge_agent.jenkins_events WHERE sequence = 1")
            ev1 = _row_to_event(cursor.fetchone())
        res = store.upsert_observation(ev1)
        self.assertFalse(res["applied"])
        obs_after = store.get_observation(
            project_alias="homelab",
            jenkins_instance="ci-primary",
            job_name="forge/ci-main",
            observation_key="build:55",
        )
        self.assertEqual(obs_after["last_event_sequence"], 2)
        self.assertEqual(obs_after["build_result"], "success")

    def test_provider_substitution_custom_handler_and_unregistered_rejection(self):
        cfg = make_config(
            consumers=[{"name": "custom-sink", "handler": "custom_recorder", "max_attempts": 2}]
        )
        store = JenkinsStore(cfg, self.cluster.connect)

        # Unregistered handler fails closed before worker construction.
        with self.assertRaises(ValueError):
            JenkinsConsumerFactory(cfg, store)

        recorded = []

        class RecordingHandler:
            def __init__(self, injected_store):
                self.store = injected_store

            def handle(self, event, delivery):
                recorded.append((event.event_id, delivery.consumer_name, event.job_name))
                return ConsumptionReceipt(
                    consumer_name=delivery.consumer_name,
                    event_id=event.event_id,
                    receipt_digest=sha256_canonical({"custom": event.event_id}),
                )

        consumer = JenkinsConsumerFactory(
            cfg,
            store,
            handler_factories={"custom_recorder": RecordingHandler},
            worker_id="custom-worker",
        ).create()

        emitted = store.emit(
            JenkinsEventEmit(
                project_alias="homelab",
                jenkins_instance="ci-primary",
                job_name="forge/ci-main",
                delivery_id="sub-1",
                event_kind=JenkinsEventKind.BUILD_STARTED,
                occurred_at=OCCURRED_AT,
                summary="Started",
                build_number=10,
            )
        )
        self.assertTrue(consumer.run_once())
        self.assertFalse(consumer.run_once())
        self.assertEqual(recorded, [(emitted["event_id"], "custom-sink", "forge/ci-main")])
        # Custom handler did not populate observations table.
        self.assertIsNone(
            store.get_observation(
                project_alias="homelab",
                jenkins_instance="ci-primary",
                job_name="forge/ci-main",
                observation_key="build:10",
            )
        )

    def test_retryable_exhaustion_permanent_error_and_unhandled_sanitization(self):
        cfg = make_config(
            consumers=[{"name": "flaky", "handler": "flaky", "max_attempts": 2}],
            backoff_seconds=1,
            max_backoff_seconds=2,
        )
        store = JenkinsStore(cfg, self.cluster.connect)
        mode = {"behavior": "retryable"}

        class FlakyHandler:
            def __init__(self, _store):
                pass

            def handle(self, _event, _delivery):
                if mode["behavior"] == "retryable":
                    raise RetryableHandlerError("transient_upstream_timeout")
                if mode["behavior"] == "permanent":
                    raise PermanentHandlerError("invalid_schema\nsecret=do_not_break_line")
                raise RuntimeError("sensitive_internal_trace_should_not_leak")

        consumer = JenkinsConsumerFactory(
            cfg, store, handler_factories={"flaky": FlakyHandler}, worker_id="flaky-w"
        ).create()

        # 1. Retryable -> failed (attempt 1) -> wait -> dead_lettered (attempt 2).
        ev1 = store.emit(
            JenkinsEventEmit(
                project_alias="homelab",
                jenkins_instance="ci-primary",
                job_name="forge/ci-main",
                delivery_id="retry-1",
                event_kind=JenkinsEventKind.BUILD_STARTED,
                occurred_at=OCCURRED_AT,
                summary="Retry test",
                build_number=1,
            )
        )
        self.assertTrue(consumer.run_once())
        d1 = store.get_event(project_alias="homelab", event_id=ev1["event_id"])["deliveries"][0]
        self.assertEqual(d1["status"], "failed")
        self.assertEqual(d1["attempt_count"], 1)
        self.assertEqual(d1["last_error"], "transient_upstream_timeout")

        # Force next_attempt_at to now so we don't sleep unnecessarily.
        self.cluster.admin_exec(
            f"UPDATE forge_agent.jenkins_event_deliveries SET next_attempt_at = clock_timestamp() "
            f"WHERE delivery_row_id = '{d1['delivery_row_id']}'::uuid"
        )
        self.assertTrue(consumer.run_once())
        d1_final = store.get_event(project_alias="homelab", event_id=ev1["event_id"])["deliveries"][0]
        self.assertEqual(d1_final["status"], "dead_lettered")
        self.assertEqual(d1_final["attempt_count"], 2)

        # 2. Permanent error dead-letters on attempt 1 with control chars stripped.
        mode["behavior"] = "permanent"
        ev2 = store.emit(
            JenkinsEventEmit(
                project_alias="homelab",
                jenkins_instance="ci-primary",
                job_name="forge/ci-main",
                delivery_id="perm-1",
                event_kind=JenkinsEventKind.BUILD_STARTED,
                occurred_at=OCCURRED_AT,
                summary="Permanent test",
                build_number=2,
            )
        )
        self.assertTrue(consumer.run_once())
        d2 = store.get_event(project_alias="homelab", event_id=ev2["event_id"])["deliveries"][0]
        self.assertEqual(d2["status"], "dead_lettered")
        self.assertEqual(d2["attempt_count"], 1)
        self.assertNotIn("\n", d2["last_error"])

        # 3. Unhandled exception dead-letters with only exception class name (no raw message/trace).
        mode["behavior"] = "unhandled"
        ev3 = store.emit(
            JenkinsEventEmit(
                project_alias="homelab",
                jenkins_instance="ci-primary",
                job_name="forge/ci-main",
                delivery_id="unhandled-1",
                event_kind=JenkinsEventKind.BUILD_STARTED,
                occurred_at=OCCURRED_AT,
                summary="Unhandled test",
                build_number=3,
            )
        )
        self.assertTrue(consumer.run_once())
        d3 = store.get_event(project_alias="homelab", event_id=ev3["event_id"])["deliveries"][0]
        self.assertEqual(d3["status"], "dead_lettered")
        self.assertEqual(d3["last_error"], "unhandled_error:RuntimeError")

    def test_heartbeat_renews_lease_during_slow_handler(self):
        cfg = make_config(lease_seconds=5, renew_seconds=1)
        store = JenkinsStore(cfg, self.cluster.connect)
        seen_initial_expiry = []

        class SlowHandler:
            def __init__(self, inner_store):
                self._inner = inner_store

            def handle(self, event, delivery):
                seen_initial_expiry.append(delivery.lease_expires_at)
                time.sleep(1.4)
                return ObservationJenkinsHandler(self._inner).handle(event, delivery)

        consumer = JenkinsConsumerFactory(
            cfg, store, handler_factories={"observation": SlowHandler}, worker_id="slow-w"
        ).create()
        ev = store.emit(
            JenkinsEventEmit(
                project_alias="homelab",
                jenkins_instance="ci-primary",
                job_name="forge/ci-main",
                delivery_id="hb-1",
                event_kind=JenkinsEventKind.BUILD_STARTED,
                occurred_at=OCCURRED_AT,
                summary="Slow build event",
                build_number=77,
            )
        )
        self.assertTrue(consumer.run_once())
        d = store.get_event(project_alias="homelab", event_id=ev["event_id"])["deliveries"][0]
        self.assertEqual(d["status"], "consumed")
        self.assertEqual(len(seen_initial_expiry), 1)


if __name__ == "__main__":
    unittest.main()
