"""Tests for JenkinsEventEmitter implementations and the preview-by-default CLI bridge."""

from contextlib import redirect_stderr, redirect_stdout
import io
import json
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import Mock

import jenkins_api
import jenkins_codec
import jenkins_config
import jenkins_emit_tool
from jenkins_emitter import (
    HttpDispatchJenkinsEmitter,
    InProcessJenkinsEmitter,
    JenkinsEmitterError,
)
from jenkins_events import JenkinsBuildResult, JenkinsEventEmit, JenkinsEventKind
from queue_store import Rejected
from test_jenkins_api import ORIGIN, TOKEN, runner_server, sample_raw_config

SECRET = b"jenkins-cli-hmac-secret"
OCCURRED_AT = "2026-11-21T05:00:00Z"


def sample_webhook_bytes():
    return json.dumps({
        "name": "forge/ci-main",
        "build": {
            "phase": "COMPLETED",
            "number": 99,
            "status": "SUCCESS",
            "duration": 3200,
            "full_url": f"{ORIGIN}/job/forge/job/ci-main/99/",
            "timestamp": OCCURRED_AT,
        },
    }).encode("utf-8")


def sample_emit():
    return JenkinsEventEmit(
        project_alias="homelab",
        jenkins_instance="ci-primary",
        job_name="forge/ci-main",
        delivery_id="emit-test-99",
        event_kind=JenkinsEventKind.BUILD_COMPLETED,
        occurred_at=OCCURRED_AT,
        summary="Build #99 succeeded",
        build_number=99,
        build_result=JenkinsBuildResult.SUCCESS,
        build_url=f"{ORIGIN}/job/forge/job/ci-main/99/",
    )


class JenkinsEmitterTest(unittest.TestCase):
    def test_in_process_emitter_delegates_to_service(self):
        mock_service = Mock()
        mock_service.emit.return_value = {"event_id": "4fb285b2-5a53-4caa-90b6-d0aff012c0a4", "sequence": 1}
        emitter = InProcessJenkinsEmitter(mock_service)
        ev = sample_emit()
        receipt = emitter.emit(ev)
        self.assertEqual(receipt["sequence"], 1)
        mock_service.emit.assert_called_once_with(ev)

    def test_http_emitter_enforces_loopback_and_dispatches_to_runner(self):
        with self.assertRaises(ValueError):
            HttpDispatchJenkinsEmitter(token=TOKEN, host="10.0.0.1", port=18083)

        cfg = jenkins_config.parse_jenkins_config(sample_raw_config())
        mock_store = Mock()
        mock_store.emit.return_value = {
            "event_id": "4fb285b2-5a53-4caa-90b6-d0aff012c0a4",
            "project_alias": "homelab",
            "sequence": 1,
            "payload_sha256": sample_emit().payload_sha256(),
            "deduplicated": False,
            "deliveries_queued": 1,
        }
        service = jenkins_api.JenkinsService(cfg, mock_store)

        with runner_server(jenkins_service=service) as port:
            emitter = HttpDispatchJenkinsEmitter(token=TOKEN, port=port)
            receipt = emitter.emit(sample_emit(), request_id="emitter-req-1")
            self.assertEqual(receipt["sequence"], 1)
            self.assertFalse(receipt["deduplicated"])

            mock_store.emit.side_effect = Rejected(409, "idempotency_conflict")
            with self.assertRaises(JenkinsEmitterError) as ctx:
                emitter.emit(sample_emit(), request_id="emitter-req-2")
            self.assertEqual(ctx.exception.code, "idempotency_conflict")
            self.assertEqual(ctx.exception.status, 409)

    def test_cli_tool_defaults_to_preview_and_emits_with_hmac_verification(self):
        raw_webhook = sample_webhook_bytes()
        sig = jenkins_codec.compute_hmac_sha256(SECRET, raw_webhook)

        with TemporaryDirectory(prefix="forge-jenkins-cli-") as tmp:
            tmp_dir = Path(tmp)
            cfg_path = tmp_dir / "jenkins.json"
            cfg_path.write_text(json.dumps(sample_raw_config()), encoding="utf-8")
            payload_path = tmp_dir / "webhook.json"
            payload_path.write_bytes(raw_webhook)
            secret_path = tmp_dir / "hmac.secret"
            secret_path.write_bytes(SECRET)
            token_path = tmp_dir / "runner.token"
            token_path.write_text(TOKEN, encoding="utf-8")

            # Preview mode (default) must NOT invoke emitter_factory.
            emitter_factory = Mock()
            out, err = io.StringIO(), io.StringIO()
            with redirect_stdout(out), redirect_stderr(err):
                rc = jenkins_emit_tool.main(
                    [
                        "--config", str(cfg_path),
                        "--project", "homelab",
                        "--instance", "ci-primary",
                        "--payload-file", str(payload_path),
                        "--delivery-id", "cli-del-99",
                        "--signature", sig,
                        "--hmac-secret-file", str(secret_path),
                    ],
                    emitter_factory=emitter_factory,
                )
            self.assertEqual(rc, 0, err.getvalue())
            emitter_factory.assert_not_called()
            preview = json.loads(out.getvalue())
            self.assertEqual(preview["action"], "preview")
            self.assertEqual(preview["destination"], "ci.jenkins.event.emit")
            self.assertEqual(preview["event"]["build_number"], 99)

            # Bad signature fails closed with exit code 2 before emitting.
            out, err = io.StringIO(), io.StringIO()
            with redirect_stdout(out), redirect_stderr(err):
                rc = jenkins_emit_tool.main(
                    [
                        "--action", "emit",
                        "--config", str(cfg_path),
                        "--project", "homelab",
                        "--instance", "ci-primary",
                        "--payload-file", str(payload_path),
                        "--signature", "sha256=" + "0" * 64,
                        "--hmac-secret-file", str(secret_path),
                        "--token-file", str(token_path),
                    ],
                    emitter_factory=emitter_factory,
                )
            self.assertEqual(rc, 2)
            self.assertEqual(json.loads(err.getvalue())["code"], "invalid_signature")
            emitter_factory.assert_not_called()

            # Supplying --signature without any configured HMAC secret must fail closed.
            out, err = io.StringIO(), io.StringIO()
            with redirect_stdout(out), redirect_stderr(err):
                rc = jenkins_emit_tool.main(
                    [
                        "--config", str(cfg_path),
                        "--project", "homelab",
                        "--instance", "ci-primary",
                        "--payload-file", str(payload_path),
                        "--signature", sig,
                    ],
                    emitter_factory=emitter_factory,
                )
            self.assertEqual(rc, 2)
            self.assertEqual(json.loads(err.getvalue())["code"], "missing_hmac_secret")
            emitter_factory.assert_not_called()

            # Valid --action emit dispatches through live runner server.
            cfg = jenkins_config.parse_jenkins_config(sample_raw_config())
            mock_store = Mock()
            mock_store.emit.return_value = {
                "event_id": "4fb285b2-5a53-4caa-90b6-d0aff012c0a4",
                "project_alias": "homelab",
                "sequence": 1,
                "payload_sha256": preview["payload_sha256"],
                "deduplicated": False,
                "deliveries_queued": 1,
            }
            service = jenkins_api.JenkinsService(cfg, mock_store)
            with runner_server(jenkins_service=service) as port:
                out, err = io.StringIO(), io.StringIO()
                with redirect_stdout(out), redirect_stderr(err):
                    rc = jenkins_emit_tool.main([
                        "--action", "emit",
                        "--config", str(cfg_path),
                        "--project", "homelab",
                        "--instance", "ci-primary",
                        "--payload-file", str(payload_path),
                        "--delivery-id", "cli-del-99",
                        "--signature", sig,
                        "--hmac-secret-file", str(secret_path),
                        "--runner-port", str(port),
                        "--token-file", str(token_path),
                        "--request-id", "cli-req-99",
                    ])
                self.assertEqual(rc, 0, err.getvalue())
                emitted = json.loads(out.getvalue())
                self.assertEqual(emitted["action"], "emit")
                self.assertEqual(emitted["receipt"]["sequence"], 1)


if __name__ == "__main__":
    unittest.main()
