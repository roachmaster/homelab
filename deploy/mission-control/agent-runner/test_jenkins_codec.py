"""Unit tests for the pure Jenkins webhook codec and HMAC-SHA256 verifier."""

import ast
import json
from pathlib import Path
import unittest

import jenkins_codec
from jenkins_codec import (
    JenkinsCodecError,
    MAX_WEBHOOK_BYTES,
    compute_hmac_sha256,
    translate_jenkins_webhook,
    verify_hmac_sha256,
)
from jenkins_events import JenkinsBuildResult, JenkinsEventKind, JenkinsStageStatus

ORIGIN = "https://jenkins.internal:8443"
SECRET = b"super-secret-jenkins-hmac-key"
SHA1_UPPER = "ABCDEF0123456789ABCDEF0123456789ABCDEF01"


class JenkinsCodecTest(unittest.TestCase):
    def test_codec_has_no_network_db_or_env_imports(self):
        source = Path(jenkins_codec.__file__).read_text(encoding="utf-8")
        tree = ast.parse(source)
        imported = set()
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                imported.update(alias.name.split(".")[0] for alias in node.names)
            elif isinstance(node, ast.ImportFrom) and node.module:
                imported.add(node.module.split(".")[0])
        forbidden = {"psycopg2", "os", "http", "urllib", "socket", "subprocess", "pathlib"}
        self.assertTrue(imported.isdisjoint(forbidden), f"Forbidden imports: {imported & forbidden}")

    def test_hmac_verification_valid_and_invalid(self):
        body = b'{"name":"forge/ci-main","build":{"phase":"STARTED","number":1}}'
        sig = compute_hmac_sha256(SECRET, body)
        verify_hmac_sha256(SECRET, body, sig)

        for bad_sig in (None, "", "sha256=short", "md5=" + "a" * 64, "sha256=" + "0" * 64):
            with self.subTest(bad_sig=bad_sig):
                with self.assertRaises(JenkinsCodecError) as ctx:
                    verify_hmac_sha256(SECRET, body, bad_sig)
                self.assertEqual(ctx.exception.code, "invalid_signature")

        with self.assertRaises(JenkinsCodecError) as ctx:
            verify_hmac_sha256(b"wrong-secret", body, sig)
        self.assertEqual(ctx.exception.code, "invalid_signature")

        with self.assertRaises(JenkinsCodecError) as ctx:
            verify_hmac_sha256(SECRET, body + b" ", sig)
        self.assertEqual(ctx.exception.code, "invalid_signature")

        oversized = b"x" * (MAX_WEBHOOK_BYTES + 1)
        with self.assertRaises(JenkinsCodecError) as ctx:
            verify_hmac_sha256(SECRET, oversized, compute_hmac_sha256(SECRET, oversized))
        self.assertEqual(ctx.exception.code, "payload_too_large")

    def test_stage_started_phase_is_rejected_by_webhook_codec(self):
        # stage_started is verifier-emitted only (loopback dispatch with the
        # dispatch bearer token). A webhook claiming that phase must not
        # decode: webhooks carry no stage-start identity.
        body = json.dumps({
            "name": "forge/ci-main",
            "build": {
                "phase": "STAGE_STARTED",
                "number": 108,
                "full_url": f"{ORIGIN}/job/forge/job/ci-main/108/",
                "timestamp": "2026-11-21T05:10:00Z",
                "scm": {"commit": SHA1_UPPER, "branch": "origin/main"},
            },
        }).encode("utf-8")
        sig = compute_hmac_sha256(SECRET, body)
        with self.assertRaises(JenkinsCodecError) as ctx:
            translate_jenkins_webhook(
                body,
                project_alias="homelab",
                jenkins_instance="ci-primary",
                expected_origin=ORIGIN,
                delivery_id="hdr-delivery-stage",
                signature_header=sig,
                hmac_secret=SECRET,
            )
        self.assertEqual(ctx.exception.code, "invalid_webhook_payload")

    def test_translate_completed_and_stage_and_queued_events(self):
        completed_body = json.dumps({
            "name": "forge/ci-main",
            # Untrusted attempt to override routing identity inside the webhook body:
            "project_alias": "attacker-project",
            "jenkins_instance": "attacker-instance",
            "owner_id": "attacker",
            "destination": "ide.work.submit",
            "build": {
                "phase": "FINALIZED",
                "number": 108,
                "status": "SUCCESS",
                "duration": 42000,
                "full_url": f"{ORIGIN}/job/forge/job/ci-main/108/",
                "timestamp": "2026-11-21T05:10:00Z",
                "scm": {
                    "commit": SHA1_UPPER,
                    "branch": "origin/main",
                },
                "correlation_id": "trace-108",
            },
        }).encode("utf-8")
        sig = compute_hmac_sha256(SECRET, completed_body)
        event = translate_jenkins_webhook(
            completed_body,
            project_alias="homelab",
            jenkins_instance="ci-primary",
            expected_origin=ORIGIN,
            delivery_id="hdr-delivery-108",
            signature_header=sig,
            hmac_secret=SECRET,
        )
        self.assertEqual(event.project_alias, "homelab")
        self.assertEqual(event.jenkins_instance, "ci-primary")
        self.assertEqual(event.job_name, "forge/ci-main")
        self.assertEqual(event.delivery_id, "hdr-delivery-108")
        self.assertEqual(event.event_kind, JenkinsEventKind.BUILD_COMPLETED)
        self.assertEqual(event.build_number, 108)
        self.assertEqual(event.build_result, JenkinsBuildResult.SUCCESS)
        self.assertEqual(event.commit_sha, SHA1_UPPER.lower())
        self.assertEqual(event.branch, "origin/main")
        self.assertEqual(event.duration_ms, 42000)
        self.assertEqual(event.correlation_id, "trace-108")

        stage_body = json.dumps({
            "name": "forge/ci-main",
            "build": {
                "phase": "STAGE_COMPLETED",
                "number": 108,
                "timestamp": 1795237800000,
                "stage": {"name": "cargo-test", "status": "FAILURE"},
            },
        }).encode("utf-8")
        stage_event = translate_jenkins_webhook(
            stage_body,
            project_alias="homelab",
            jenkins_instance="ci-primary",
            expected_origin=ORIGIN,
        )
        self.assertEqual(stage_event.event_kind, JenkinsEventKind.STAGE_COMPLETED)
        self.assertEqual(stage_event.stage_name, "cargo-test")
        self.assertEqual(stage_event.stage_status, JenkinsStageStatus.FAILURE)
        self.assertTrue(stage_event.delivery_id.startswith("wh-"))

        queued_body = json.dumps({
            "name": "forge/ci-main",
            "build": {
                "phase": "QUEUED",
                "queue_id": 5001,
                "timestamp": "2026-11-21T05:09:00Z",
            },
        }).encode("utf-8")
        queued_event = translate_jenkins_webhook(
            queued_body,
            project_alias="homelab",
            jenkins_instance="ci-primary",
            expected_origin=ORIGIN,
        )
        self.assertEqual(queued_event.event_kind, JenkinsEventKind.BUILD_QUEUED)
        self.assertEqual(queued_event.queue_id, 5001)
        self.assertIsNone(queued_event.build_number)

    def test_origin_mismatch_and_malformed_payloads_rejected(self):
        wrong_origin = json.dumps({
            "name": "forge/ci-main",
            "build": {
                "phase": "STARTED",
                "number": 1,
                "timestamp": "2026-11-21T05:09:00Z",
                "full_url": "https://evil.example/job/forge/1/",
            },
        }).encode("utf-8")
        with self.assertRaises(JenkinsCodecError) as ctx:
            translate_jenkins_webhook(
                wrong_origin,
                project_alias="homelab",
                jenkins_instance="ci-primary",
                expected_origin=ORIGIN,
            )
        self.assertEqual(ctx.exception.code, "invalid_build_url")

        dup_keys = b'{"name":"forge/ci-main","name":"other","build":{"phase":"STARTED","number":1,"timestamp":"2026-11-21T05:09:00Z"}}'
        with self.assertRaises(JenkinsCodecError) as ctx:
            translate_jenkins_webhook(
                dup_keys,
                project_alias="homelab",
                jenkins_instance="ci-primary",
                expected_origin=ORIGIN,
            )
        self.assertEqual(ctx.exception.code, "invalid_webhook_payload")


if __name__ == "__main__":
    unittest.main()
