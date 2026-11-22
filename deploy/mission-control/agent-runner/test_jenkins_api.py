"""Unit and authenticated HTTP front-door tests for ci.jenkins.* dispatch and lifecycle."""

from contextlib import closing, contextmanager
from http.client import HTTPConnection
import json
from pathlib import Path
from tempfile import TemporaryDirectory
import threading
import time
import unittest
from unittest.mock import Mock
from uuid import UUID

import chat_history_api
import jenkins_api
import jenkins_config
import runner
from queue_store import Rejected
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE

TOKEN = "c" * 32
REQUEST_ID = "jk-req-1"
ORIGIN = "https://jenkins.internal:8443"
OCCURRED_AT = "2026-11-21T05:00:00Z"
EVENT_ID = UUID("4fb285b2-5a53-4caa-90b6-d0aff012c0a4")
SHA1 = "f" * 40


def sample_raw_config(**overrides):
    raw = {
        "owner": "mission-control",
        "allowed_projects": ["homelab"],
        "allowed_instances": {"ci-primary": {"origin": ORIGIN}},
        "allowed_jobs": {"homelab": ["forge/ci-main"]},
        "consumers": [
            {"name": "observation", "handler": "observation", "max_attempts": 3},
        ],
        "max_pending_deliveries": 64,
        "lease_seconds": 30,
        "renew_seconds": 10,
        "poll_interval_seconds": 2,
        "backoff_seconds": 1,
        "max_backoff_seconds": 10,
    }
    raw.update(overrides)
    return raw


def emit_body(**overrides):
    body = {
        "kind": "jenkins_event_emit",
        "request_id": REQUEST_ID,
        "project_alias": "homelab",
        "jenkins_instance": "ci-primary",
        "job_name": "forge/ci-main",
        "delivery_id": "del-http-1",
        "event_kind": "build_completed",
        "build_number": 42,
        "build_result": "success",
        "commit_sha": SHA1,
        "branch": "main",
        "duration_ms": 1500,
        "build_url": f"{ORIGIN}/job/forge/job/ci-main/42/",
        "occurred_at": OCCURRED_AT,
        "summary": "Build #42 succeeded",
    }
    body.update(overrides)
    return body


@contextmanager
def runner_server(*, jenkins_service=None):
    ready = threading.Event()
    ready.set()
    with runner.Server(
        TOKEN, ready, None, jenkins_service=jenkins_service, port=0
    ) as server:
        thread = threading.Thread(
            target=lambda: server.serve_forever(poll_interval=0.01), daemon=True
        )
        thread.start()
        try:
            yield server.server_port
        finally:
            server.shutdown()
            thread.join()


def dispatch_post(port, destination, body, *, headers_override=None, raw_bytes=None):
    headers = {
        "Authorization": f"Bearer {TOKEN}",
        "Content-Type": "application/json",
        "Forge-Envelope-Version": "1",
        "Forge-Contract-Version": "1",
        "Forge-Destination": destination.value if isinstance(destination, jenkins_api.Destination) else destination,
        "Forge-Request-Id": REQUEST_ID,
    }
    if headers_override:
        for key, value in headers_override.items():
            if value is None:
                headers.pop(key, None)
            else:
                headers[key] = value
    payload = raw_bytes if raw_bytes is not None else json.dumps(body).encode("utf-8")
    with closing(HTTPConnection("127.0.0.1", port, timeout=5)) as conn:
        conn.request("POST", chat_history_api.PATH, body=payload, headers=headers)
        resp = conn.getresponse()
        return resp.status, json.loads(resp.read().decode("utf-8"))


class JenkinsApiContractTest(unittest.TestCase):
    def test_disabled_by_default_zero_effects_and_isolation(self):
        connect_factory = Mock()
        service = runner.build_jenkins_service({}, connect_factory)
        self.assertIsNone(service)
        connect_factory.assert_not_called()

        with runner_server(jenkins_service=None) as port:
            status, payload = dispatch_post(port, jenkins_api.Destination.EMIT, emit_body())
            self.assertEqual(status, 503)
            self.assertEqual(
                payload,
                {"protocol_version": 1, "event": {"kind": "jenkins_failed", "request_id": REQUEST_ID, "code": "unavailable"}},
            )

            status, payload = dispatch_post(port, "ci.jenkins.unknown.op", emit_body())
            self.assertEqual(status, 404)
            self.assertEqual(
                payload,
                {"protocol_version": 1, "event": {"kind": "jenkins_failed", "request_id": REQUEST_ID, "code": "not_found"}},
            )

    def test_header_admission_variant_agreement_and_zero_store_calls_on_rejection(self):
        cfg = jenkins_config.parse_jenkins_config(sample_raw_config())
        mock_store = Mock()
        service = jenkins_api.JenkinsService(cfg, mock_store)

        with runner_server(jenkins_service=service) as port:
            # Missing auth -> 401
            status, _ = dispatch_post(
                port, jenkins_api.Destination.EMIT, emit_body(), headers_override={"Authorization": None}
            )
            self.assertEqual(status, 401)

            # Bad envelope version -> 400
            status, payload = dispatch_post(
                port, jenkins_api.Destination.EMIT, emit_body(), headers_override={"Forge-Envelope-Version": "2"}
            )
            self.assertEqual(status, 400)
            self.assertEqual(payload["event"]["code"], "invalid_request")

            # Unknown Forge-* header -> 400
            status, payload = dispatch_post(
                port, jenkins_api.Destination.EMIT, emit_body(), headers_override={"Forge-Extra": "1"}
            )
            self.assertEqual(status, 400)
            self.assertEqual(payload["event"]["code"], "invalid_request")

            # Missing Forge-Request-Id -> 400 plain error
            status, payload = dispatch_post(
                port, jenkins_api.Destination.EMIT, emit_body(), headers_override={"Forge-Request-Id": None}
            )
            self.assertEqual(status, 400)
            self.assertEqual(payload, {"error": "invalid_request"})

            # request_id body/header mismatch -> 400
            status, payload = dispatch_post(
                port, jenkins_api.Destination.EMIT, emit_body(request_id="different-id")
            )
            self.assertEqual(status, 400)
            self.assertEqual(payload["event"]["code"], "invalid_request")

            # Destination vs kind mismatch -> 400
            status, payload = dispatch_post(
                port, jenkins_api.Destination.LIST, emit_body()
            )
            self.assertEqual(status, 400)
            self.assertEqual(payload["event"]["code"], "invalid_request")

            # Extra body field -> 400
            status, payload = dispatch_post(
                port, jenkins_api.Destination.EMIT, emit_body(unexpected_field=1)
            )
            self.assertEqual(status, 400)
            self.assertEqual(payload["event"]["code"], "invalid_request")

            # Oversized body (> 16 KiB) -> 413
            oversized = b'{"x":"' + (b"a" * (jenkins_api.MAX_REQUEST_BYTES + 10)) + b'"}'
            status, payload = dispatch_post(
                port, jenkins_api.Destination.EMIT, None, raw_bytes=oversized
            )
            self.assertEqual(status, 413)
            self.assertEqual(payload["event"]["code"], "invalid_request")

            # Unallowlisted project / instance / job -> 403 denied
            for override in (
                {"project_alias": "other-project"},
                {"jenkins_instance": "other-instance"},
                {"job_name": "forge/forbidden"},
            ):
                with self.subTest(override=override):
                    status, payload = dispatch_post(port, jenkins_api.Destination.EMIT, emit_body(**override))
                    self.assertEqual(status, 403)
                    self.assertEqual(payload["event"]["code"], "denied")

            # Origin mismatch on build_url -> 400 invalid_request
            status, payload = dispatch_post(
                port,
                jenkins_api.Destination.EMIT,
                emit_body(build_url="https://evil.example/job/forge/42/"),
            )
            self.assertEqual(status, 400)
            self.assertEqual(payload["event"]["code"], "invalid_request")

        mock_store.emit.assert_not_called()
        mock_store.list_events.assert_not_called()
        mock_store.get_event.assert_not_called()
        mock_store.list_deliveries.assert_not_called()

    def test_conflict_and_queue_full_error_mapping(self):
        cfg = jenkins_config.parse_jenkins_config(sample_raw_config())
        mock_store = Mock()
        service = jenkins_api.JenkinsService(cfg, mock_store)

        with runner_server(jenkins_service=service) as port:
            mock_store.emit.side_effect = Rejected(409, "idempotency_conflict")
            status, payload = dispatch_post(port, jenkins_api.Destination.EMIT, emit_body())
            self.assertEqual(status, 409)
            self.assertEqual(payload["event"]["code"], "idempotency_conflict")

            mock_store.emit.side_effect = Rejected(503, "queue_full")
            status, payload = dispatch_post(port, jenkins_api.Destination.EMIT, emit_body())
            self.assertEqual(status, 503)
            self.assertEqual(payload["event"]["code"], "queue_full")


@unittest.skipUnless(PG16_AVAILABLE, "Disposable PostgreSQL 16 binary not available")
class JenkinsApiEndToEndPostgresTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()

    @classmethod
    def tearDownClass(cls):
        cls.cluster.stop()

    def setUp(self):
        self.cluster.reset_jenkins_tables()

    def test_http_emit_consume_list_get_deliveries_and_replay_via_bootstrap_factory(self):
        with TemporaryDirectory(prefix="forge-jenkins-e2e-") as tmp:
            cfg_path = Path(tmp) / "jenkins.json"
            cfg_path.write_text(json.dumps(sample_raw_config()), encoding="utf-8")
            service = runner.build_jenkins_service(
                {jenkins_config.CONFIG_ENV_VAR: str(cfg_path)},
                self.cluster.connect,
            )
            self.assertIsNotNone(service)
            self.assertIsNotNone(service.consumer_worker)

            service.consumer_worker.start()
            try:
                with runner_server(jenkins_service=service) as port:
                    # Emit event
                    status, emitted = dispatch_post(port, jenkins_api.Destination.EMIT, emit_body())
                    self.assertEqual(status, 202)
                    ev = emitted["event"]
                    self.assertEqual(ev["kind"], "jenkins_event_emitted")
                    self.assertEqual(ev["sequence"], 1)
                    self.assertFalse(ev["deduplicated"])
                    self.assertEqual(ev["deliveries_queued"], 1)

                    # Exact replay -> 202 with same event_id and deduplicated=True
                    status, replay = dispatch_post(port, jenkins_api.Destination.EMIT, emit_body())
                    self.assertEqual(status, 202)
                    self.assertEqual(replay["event"]["event_id"], ev["event_id"])
                    self.assertTrue(replay["event"]["deduplicated"])

                    # Conflicting payload with same delivery_id -> 409 idempotency_conflict
                    status, conflict = dispatch_post(
                        port, jenkins_api.Destination.EMIT, emit_body(summary="Changed summary")
                    )
                    self.assertEqual(status, 409)
                    self.assertEqual(conflict["event"]["code"], "idempotency_conflict")

                    # Wait for background consumer to consume delivery via LISTEN wake-up.
                    deadline = time.monotonic() + 5.0
                    opened = None
                    while time.monotonic() < deadline:
                        status, resp = dispatch_post(
                            port,
                            jenkins_api.Destination.GET,
                            {
                                "kind": "jenkins_event_get",
                                "request_id": REQUEST_ID,
                                "project_alias": "homelab",
                                "event_id": ev["event_id"],
                            },
                        )
                        self.assertEqual(status, 200)
                        opened = resp["event"]
                        if opened["deliveries"] and opened["deliveries"][0]["status"] == "consumed":
                            break
                        time.sleep(0.05)

                    self.assertEqual(opened["kind"], "jenkins_event_opened")
                    self.assertEqual(opened["deliveries"][0]["status"], "consumed")
                    self.assertNotIn("lease_token", opened["deliveries"][0])

                    # List events
                    status, listed = dispatch_post(
                        port,
                        jenkins_api.Destination.LIST,
                        {
                            "kind": "jenkins_events_list",
                            "request_id": REQUEST_ID,
                            "project_alias": "homelab",
                            "job_name": "forge/ci-main",
                            "event_kind": "build_completed",
                            "commit_sha": SHA1,
                        },
                    )
                    self.assertEqual(status, 200)
                    self.assertEqual(listed["event"]["kind"], "jenkins_events_listed")
                    self.assertEqual(len(listed["event"]["events"]), 1)

                    # List deliveries
                    status, deliv = dispatch_post(
                        port,
                        jenkins_api.Destination.DELIVERIES_LIST,
                        {
                            "kind": "jenkins_deliveries_list",
                            "request_id": REQUEST_ID,
                            "project_alias": "homelab",
                            "consumer_name": "observation",
                            "status": "consumed",
                        },
                    )
                    self.assertEqual(status, 200)
                    self.assertEqual(deliv["event"]["kind"], "jenkins_deliveries_listed")
                    self.assertEqual(len(deliv["event"]["deliveries"]), 1)
            finally:
                service.consumer_worker.close(timeout=5.0)


if __name__ == "__main__":
    unittest.main()
