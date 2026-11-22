"""Unit tests for pure Jenkins event contracts and frozen startup configuration."""

import ast
from dataclasses import FrozenInstanceError
import json
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

import jenkins_config
import jenkins_events
from jenkins_events import (
    ConsumptionReceipt,
    JenkinsBuildResult,
    JenkinsEventEmit,
    JenkinsEventKind,
    JenkinsStageStatus,
    sanitize_error_message,
    validate_build_url,
    validate_origin,
    validate_job_name,
)

SHA1 = "a" * 40
SHA256 = "b" * 64
EVENT_UUID = "4fb285b2-5a53-4caa-90b6-d0aff012c0a4"
OCCURRED_AT = "2026-11-21T05:00:00Z"


def sample_raw_config(**overrides):
    raw = {
        "owner": "mission-control",
        "allowed_projects": ["homelab", "docs"],
        "allowed_instances": {
            "ci-primary": {
                "origin": "https://jenkins.internal:8443",
                "hmac_secret_env": "FORGE_JENKINS_HMAC_SECRET",
            }
        },
        "allowed_jobs": {
            "homelab": ["forge/ci-main", "forge/nightly"],
            "docs": ["docs/build"],
        },
        "consumers": [
            {
                "name": "observation",
                "handler": "observation",
                "projects": ["homelab", "docs"],
                "event_kinds": ["build_queued", "build_started", "build_completed", "stage_completed"],
                "max_attempts": 5,
                "enabled": True,
            }
        ],
        "max_pending_deliveries": 128,
        "lease_seconds": 30,
        "renew_seconds": 10,
        "poll_interval_seconds": 5,
        "backoff_seconds": 5,
        "max_backoff_seconds": 120,
    }
    raw.update(overrides)
    return raw


class JenkinsEventsContractTest(unittest.TestCase):
    def test_jenkins_events_module_imports_only_stdlib(self):
        source = Path(jenkins_events.__file__).read_text(encoding="utf-8")
        tree = ast.parse(source)
        imported = set()
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                imported.update(alias.name.split(".")[0] for alias in node.names)
            elif isinstance(node, ast.ImportFrom) and node.module:
                imported.add(node.module.split(".")[0])
        forbidden = {"psycopg2", "os", "http", "urllib3", "requests", "subprocess", "work_store", "queue_store"}
        self.assertTrue(imported.isdisjoint(forbidden), f"Forbidden imports: {imported & forbidden}")

    def test_valid_event_kinds_and_cross_field_invariants(self):
        queued = JenkinsEventEmit(
            project_alias="homelab",
            jenkins_instance="ci-primary",
            job_name="forge/ci-main",
            delivery_id="del-queued-1",
            event_kind=JenkinsEventKind.BUILD_QUEUED,
            occurred_at="2026-11-21T05:00:00+00:00",
            summary="Queued build",
            queue_id=99,
        )
        self.assertEqual(queued.occurred_at, OCCURRED_AT)
        self.assertEqual(queued.observation_key(), "queue:99")
        self.assertEqual(len(queued.payload_sha256()), 64)

        started = JenkinsEventEmit(
            project_alias="homelab",
            jenkins_instance="ci-primary",
            job_name="forge/ci-main",
            delivery_id="del-started-1",
            event_kind="build_started",
            occurred_at=OCCURRED_AT,
            summary="Build started",
            build_number=42,
            queue_id=99,
            commit_sha=SHA1,
            branch="main",
            build_url="https://jenkins.internal:8443/job/forge/job/ci-main/42/",
        )
        self.assertEqual(started.event_kind, JenkinsEventKind.BUILD_STARTED)
        self.assertEqual(started.observation_key(), "build:42")

        completed = JenkinsEventEmit(
            project_alias="homelab",
            jenkins_instance="ci-primary",
            job_name="forge/ci-main",
            delivery_id="del-completed-1",
            event_kind=JenkinsEventKind.BUILD_COMPLETED,
            occurred_at=OCCURRED_AT,
            summary="Build completed",
            build_number=42,
            build_result="success",
            duration_ms=12500,
        )
        self.assertEqual(completed.build_result, JenkinsBuildResult.SUCCESS)

        stage = JenkinsEventEmit(
            project_alias="homelab",
            jenkins_instance="ci-primary",
            job_name="forge/ci-main",
            delivery_id="del-stage-1",
            event_kind=JenkinsEventKind.STAGE_COMPLETED,
            occurred_at=OCCURRED_AT,
            summary="Stage unit-tests passed",
            build_number=42,
            stage_name="unit-tests",
            stage_status="skipped",
        )
        self.assertEqual(stage.stage_status, JenkinsStageStatus.SKIPPED)

    def test_cross_field_mismatches_rejected(self):
        base = dict(
            project_alias="homelab",
            jenkins_instance="ci-primary",
            job_name="forge/ci-main",
            delivery_id="del-1",
            occurred_at=OCCURRED_AT,
            summary="summary",
        )
        invalid_cases = [
            # build_queued missing queue_id or setting build_number/build_result
            dict(event_kind="build_queued"),
            dict(event_kind="build_queued", queue_id=1, build_number=1),
            dict(event_kind="build_queued", queue_id=1, build_result="success"),
            # build_started missing build_number or setting result/stage
            dict(event_kind="build_started"),
            dict(event_kind="build_started", build_number=1, build_result="failure"),
            dict(event_kind="build_started", build_number=1, stage_name="test"),
            # build_completed missing build_result or setting stage
            dict(event_kind="build_completed", build_number=1),
            dict(event_kind="build_completed", build_number=1, build_result="success", stage_name="s", stage_status="success"),
            # stage_completed missing stage fields or setting build_result
            dict(event_kind="stage_completed", build_number=1, stage_name="test"),
            dict(event_kind="stage_completed", build_number=1, stage_status="success"),
            dict(event_kind="stage_completed", build_number=1, stage_name="test", stage_status="success", build_result="success"),
            # invalid integer types (bool is subclass of int)
            dict(event_kind="build_started", build_number=True),
            dict(event_kind="build_queued", queue_id=False),
            # naive timestamp
            dict(event_kind="build_started", build_number=1, occurred_at="2026-11-21T05:00:00"),
            # control characters
            dict(event_kind="build_started", build_number=1, summary="line1\nline2"),
            dict(event_kind="build_started", build_number=1, branch="main\x00evil"),
            # invalid commit sha (uppercase)
            dict(event_kind="build_started", build_number=1, commit_sha="A" * 40),
        ]
        for extra in invalid_cases:
            kwargs = {**base, **extra}
            with self.subTest(extra=extra):
                with self.assertRaises(ValueError):
                    JenkinsEventEmit(**kwargs)

    def test_job_name_and_origin_validation(self):
        self.assertEqual(validate_job_name("folder/sub-job_1.0"), "folder/sub-job_1.0")
        for bad_job in ("", "/leading", "trailing/", "a//b", "../escape", "a/../b", "a/./b", "bad space", "x" * 129):
            with self.subTest(bad_job=bad_job):
                with self.assertRaises(ValueError):
                    validate_job_name(bad_job)

        self.assertEqual(validate_origin("https://Jenkins.Internal:443"), "https://jenkins.internal")
        self.assertEqual(validate_origin("https://Jenkins.Internal:8443"), "https://jenkins.internal:8443")
        for bad_origin in (
            "http://jenkins.internal",
            "https://jenkins.internal/path",
            "https://jenkins.internal/",
            "https://user:pass@jenkins.internal",
            "https://jenkins.internal?x=1",
        ):
            with self.subTest(bad_origin=bad_origin):
                with self.assertRaises(ValueError):
                    validate_origin(bad_origin)

        self.assertEqual(
            validate_build_url(
                "https://jenkins.internal:8443/job/forge/1/",
                expected_origin="https://jenkins.internal:8443",
            ),
            "https://jenkins.internal:8443/job/forge/1/",
        )
        with self.assertRaises(ValueError):
            validate_build_url(
                "https://evil.example/job/forge/1/",
                expected_origin="https://jenkins.internal:8443",
            )
        with self.assertRaises(ValueError):
            validate_build_url(
                "https://jenkins.internal/job/forge/1/",
                expected_origin="https://jenkins.internal:8443",
            )
        # Uppercase scheme would violate PostgreSQL CHECK (^https://...); reject at Python boundary.
        with self.assertRaises(ValueError):
            validate_build_url(
                "HTTPS://jenkins.internal:8443/job/forge/1/",
                expected_origin="https://jenkins.internal:8443",
            )

    def test_payload_sha256_is_deterministic_and_sensitive_to_changes(self):
        ev1 = JenkinsEventEmit(
            project_alias="homelab",
            jenkins_instance="ci-primary",
            job_name="forge/ci-main",
            delivery_id="del-1",
            event_kind="build_started",
            occurred_at="2026-11-21T05:00:00Z",
            summary="Started",
            build_number=7,
        )
        ev2 = JenkinsEventEmit(
            project_alias="homelab",
            jenkins_instance="ci-primary",
            job_name="forge/ci-main",
            delivery_id="del-1",
            event_kind=JenkinsEventKind.BUILD_STARTED,
            occurred_at="2026-11-21T06:00:00+01:00",
            summary="Started",
            build_number=7,
        )
        ev3 = JenkinsEventEmit(
            project_alias="homelab",
            jenkins_instance="ci-primary",
            job_name="forge/ci-main",
            delivery_id="del-1",
            event_kind="build_started",
            occurred_at="2026-11-21T05:00:00Z",
            summary="Different summary",
            build_number=7,
        )
        self.assertEqual(ev1.payload_sha256(), ev2.payload_sha256())
        self.assertNotEqual(ev1.payload_sha256(), ev3.payload_sha256())

    def test_sanitize_error_and_consumption_receipt(self):
        cleaned = sanitize_error_message("boom\n\x00" + "x" * 2000)
        self.assertLessEqual(len(cleaned.encode("utf-8")), 1024)
        self.assertNotIn("\n", cleaned)
        self.assertNotIn("\x00", cleaned)

        receipt = ConsumptionReceipt("observation", EVENT_UUID, SHA256, "build:1")
        self.assertEqual(receipt.receipt_digest, SHA256)
        with self.assertRaises(ValueError):
            ConsumptionReceipt("observation", "not-a-uuid", SHA256)


class JenkinsConfigTest(unittest.TestCase):
    def test_load_none_has_zero_effects(self):
        self.assertIsNone(jenkins_config.load_jenkins_config(None))

    def test_load_and_freeze_config_file(self):
        with TemporaryDirectory(prefix="forge-jenkins-cfg-") as tmp:
            path = Path(tmp) / "jenkins.json"
            path.write_text(json.dumps(sample_raw_config()), encoding="utf-8")
            cfg = jenkins_config.load_jenkins_config(str(path))
            self.assertEqual(cfg.owner, "mission-control")
            self.assertEqual(cfg.instance_origin("ci-primary"), "https://jenkins.internal:8443")
            self.assertTrue(cfg.is_job_allowed("homelab", "forge/ci-main"))
            self.assertFalse(cfg.is_job_allowed("homelab", "docs/build"))
            self.assertEqual(len(cfg.subscribed_consumers("homelab", JenkinsEventKind.BUILD_COMPLETED)), 1)
            with self.assertRaises(FrozenInstanceError):
                cfg.owner = "other"
            with self.assertRaises(TypeError):
                cfg.allowed_instances["evil"] = None
            with self.assertRaises(TypeError):
                cfg.allowed_jobs["homelab"] = frozenset()

    def test_invalid_config_rejected(self):
        with self.assertRaises(ValueError):
            jenkins_config.load_jenkins_config("relative/path.json")
        with self.assertRaises(ValueError):
            jenkins_config.load_jenkins_config("")
        with self.assertRaises(ValueError):
            jenkins_config.parse_jenkins_config(sample_raw_config(unknown_key=1))
        with self.assertRaises(ValueError):
            jenkins_config.parse_jenkins_config(sample_raw_config(lease_seconds=10, renew_seconds=10))
        with self.assertRaises(ValueError):
            jenkins_config.parse_jenkins_config(
                sample_raw_config(allowed_jobs={"homelab": ["forge/ci-main"]})
            )
        with self.assertRaises(ValueError):
            jenkins_config.parse_jenkins_config(
                sample_raw_config(
                    consumers=[
                        {"name": "dup", "handler": "observation"},
                        {"name": "dup", "handler": "observation"},
                    ]
                )
            )
        with self.assertRaises(ValueError):
            jenkins_config.parse_jenkins_config(
                sample_raw_config(
                    consumers=[{"name": "bad", "handler": "observation", "projects": ["unknown-project"]}]
                )
            )

    def test_duplicate_json_keys_and_nan_rejected(self):
        with TemporaryDirectory(prefix="forge-jenkins-cfg-") as tmp:
            path = Path(tmp) / "dup.json"
            path.write_text('{"owner":"a","owner":"b"}', encoding="utf-8")
            with self.assertRaises(ValueError):
                jenkins_config.load_jenkins_config(str(path))
            nan_path = Path(tmp) / "nan.json"
            nan_path.write_text(json.dumps(sample_raw_config()).replace("128", "NaN"), encoding="utf-8")
            with self.assertRaises(ValueError):
                jenkins_config.load_jenkins_config(str(nan_path))


if __name__ == "__main__":
    unittest.main()
