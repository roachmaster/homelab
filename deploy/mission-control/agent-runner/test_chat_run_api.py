"""Exercise the private async chat dispatch HTTP and projection contracts."""

from contextlib import nullcontext
import json
import threading
import unittest
from unittest.mock import Mock, patch
from uuid import UUID

import psycopg2

import chat_run_api as api
import runner
from chat_history_api import PATH
from queue_store import Rejected
from test_workspace_api import post, server


MODEL = "qwen2.5-coder-7b"
REQUEST_ID = "async-test"
CHAT = UUID("4fb285b2-5a53-4caa-90b6-d0aff012c0a4")
RUN = UUID("72c85704-8c75-4e7e-982f-19cd5587842f")
ACCEPTED = {"run_id": RUN, "chat_id": CHAT, "status": "queued"}
SUBMIT = {"request_id": REQUEST_ID, "kind": "chat_run_submit", "chat_id": str(CHAT),
          "idempotency_key": "tab-intent-1", "content": "What changed?"}


def dispatch_post(port, destination, body, **extra_headers):
    status, raw, cache = post(port, body, path=PATH, headers={
        "Forge-Destination": destination.value if isinstance(destination, api.Destination) else destination,
        "Forge-Envelope-Version": "1", "Forge-Contract-Version": "1", "Forge-Request-Id": REQUEST_ID,
        **extra_headers,
    })
    return status, json.loads(raw), cache


def reply(kind, **fields):
    return {"protocol_version": 1, "event": {"kind": kind, "request_id": REQUEST_ID, **fields}}


class ChatRunApiTest(unittest.TestCase):
    def test_acknowledges_durable_submission_without_an_inference_call(self):
        inference = Mock()
        with (server(distributed=True, model=MODEL, inference=inference) as port,
              patch.object(runner.store, "admit_chat_turn", return_value=ACCEPTED) as admit,
              patch.object(runner.Handler, "await_reply") as legacy_wait):
            self.assertEqual(dispatch_post(port, api.Destination.SUBMIT, SUBMIT), (
                202, reply("chat_run_accepted", run_id=str(RUN), chat_id=str(CHAT), status="queued"), "no-store",
            ))
            admit.assert_called_once_with(MODEL, CHAT, None, SUBMIT["content"], SUBMIT["idempotency_key"],
                                          model_id=None, configured_models=(MODEL,))
            self.assertEqual(inference.mock_calls, [])
            legacy_wait.assert_not_called()

    def test_lists_configured_models_and_passes_an_explicit_selection_to_admission(self):
        models = (runner.DEFAULT_MODEL, MODEL)
        options = [
            {"model_id": model, "label": runner.store.MODEL_LABELS[model], "ready": model == MODEL}
            for model in models
        ]
        with (server(distributed=True, models=models) as port,
              patch.object(runner.store, "model_catalog", return_value=options) as catalog,
              patch.object(runner.store, "admit_chat_turn", return_value=ACCEPTED) as admit):
            request = {"kind": "chat_run_models", "request_id": REQUEST_ID}
            self.assertEqual(dispatch_post(port, api.Destination.MODELS, request), (
                200, reply("chat_run_models_listed", default_model_id=runner.DEFAULT_MODEL, models=options),
                "no-store",
            ))
            catalog.assert_called_once_with(models)
            self.assertEqual(dispatch_post(port, api.Destination.SUBMIT, dict(SUBMIT, model_id=MODEL))[0], 202)
            admit.assert_called_once_with(runner.DEFAULT_MODEL, CHAT, None, SUBMIT["content"],
                                          SUBMIT["idempotency_key"], model_id=MODEL, configured_models=models)

    def test_model_catalog_configuration_requires_known_unique_models_and_default(self):
        self.assertEqual(runner.configured_models(runner.DEFAULT_MODEL, None), (runner.DEFAULT_MODEL,))
        self.assertEqual(runner.configured_models(runner.DEFAULT_MODEL, f"{MODEL}, {runner.DEFAULT_MODEL}"),
                         (MODEL, runner.DEFAULT_MODEL))
        for setting in ("", "unknown", MODEL, f"{runner.DEFAULT_MODEL},{runner.DEFAULT_MODEL}",
                        f"{runner.DEFAULT_MODEL},"):
            with self.subTest(setting=setting), self.assertRaises(ValueError):
                runner.configured_models(runner.DEFAULT_MODEL, setting)

    def test_reads_ordered_events_and_completed_reply_and_cancels(self):
        request = {"request_id": REQUEST_ID, "kind": "chat_run_events", "run_id": str(RUN),
                   "after_sequence": 1}
        page = dict(ACCEPTED, status="completed", worker_ready=True, answer="done", revision=2,
                    finish_reason="length", next_sequence=3, events=[
                        {"sequence": 2, "type": "model_started", "data": {"model": MODEL}},
                        {"sequence": 3, "type": "completed", "data": {}},
                    ])
        with (server(distributed=True) as port,
              patch.object(runner.store, "get_events", return_value=page) as get_events,
              patch.object(runner.store, "cancel", return_value=dict(ACCEPTED, status="cancelled")) as cancel):
            self.assertEqual(dispatch_post(port, api.Destination.EVENTS, request), (
                200, reply("chat_run_events", run_id=str(RUN), chat_id=str(CHAT), status="completed",
                           events=[{"sequence": 2, "kind": "model_started"}, {"sequence": 3, "kind": "completed"}],
                           answer="done\n\n[Answer cut off at the output limit.]", worker_ready=True,
                           next_sequence=3, revision=2), "no-store",
            ))
            get_events.assert_called_once_with(RUN, 1, include_completion=True)
            self.assertEqual(dispatch_post(port, api.Destination.CANCEL, {
                "request_id": REQUEST_ID, "kind": "chat_run_cancel", "run_id": str(RUN),
            }), (200, reply("chat_run_cancelled", run_id=str(RUN), chat_id=str(CHAT), status="cancelled"), "no-store"))
            cancel.assert_called_once_with(RUN)

    def test_rejects_invalid_routes_and_invalid_transfer_objects_before_admission(self):
        with (server(distributed=True) as port,
              patch.object(runner.store, "admit_chat_turn") as admit):
            for destination, body, headers, status, code in [
                (api.Destination.SUBMIT, SUBMIT, {"Authorization": "invalid"}, 401, None),
                ("ide.chat.run.unknown", SUBMIT, {}, 404, "not_found"),
                (api.Destination.EVENTS, SUBMIT, {}, 400, "invalid_request"),
                (api.Destination.SUBMIT, SUBMIT, {"Forge-Contract-Version": "2"}, 400, "invalid_request"),
                (api.Destination.SUBMIT, dict(SUBMIT, request_id="other"), {}, 400, "invalid_request"),
                (api.Destination.SUBMIT, dict(SUBMIT, owner="other"), {}, 400, "invalid_request"),
                (api.Destination.SUBMIT, dict(SUBMIT, content="☃" * 683), {}, 400, "invalid_request"),
                (api.Destination.SUBMIT, dict(SUBMIT, expected_revision=True), {}, 400, "invalid_request"),
                (api.Destination.SUBMIT, dict(SUBMIT, idempotency_key="bad key"), {}, 400, "invalid_request"),
                (api.Destination.SUBMIT, dict(SUBMIT, model_id="unknown-model"), {}, 400, "invalid_request"),
                (api.Destination.SUBMIT, dict(SUBMIT, model_id=None), {}, 400, "invalid_request"),
                (api.Destination.SUBMIT, dict(SUBMIT, model_id=[]), {}, 400, "invalid_request"),
                (api.Destination.SUBMIT, b"x" * 9000, {}, 413, "limit"),
            ]:
                with self.subTest(status=status, headers=headers, body=type(body)):
                    actual, message, _ = dispatch_post(port, destination, body, **headers)
                    self.assertEqual(actual, status)
                    expected = reply("chat_run_failed", code=code) if code else {"error": "unauthorized"}
                    self.assertEqual(message, expected)
            admit.assert_not_called()

    def test_errors_and_legacy_mode_admission_are_typed(self):
        with (server(distributed=True) as port,
              patch.object(runner.store, "admit_chat_turn") as admit):
            for error, status, code in [
                (Rejected(409, "request_id_conflict"), 409, "idempotency_conflict"),
                (Rejected(409, "chat_busy"), 409, "chat_busy"),
                (Rejected(409, "revision_conflict"), 409, "revision_conflict"),
                (Rejected(422, "invalid_messages"), 422, "limit"),
                (Rejected(503, "queue_full"), 503, "queue_full"),
                (psycopg2.OperationalError("private error"), 503, "unavailable"),
            ]:
                admit.side_effect = error
                self.assertEqual(dispatch_post(port, api.Destination.SUBMIT, SUBMIT),
                                 (status, reply("chat_run_failed", code=code), "no-store"))
        with server() as port, patch.object(runner.store, "admit_chat_turn") as admit:
            self.assertEqual(dispatch_post(port, api.Destination.SUBMIT, SUBMIT),
                             (503, reply("chat_run_failed", code="unavailable"), "no-store"))
            admit.assert_not_called()

    def test_compatibility_endpoints_use_database_dispatch_in_distributed_mode(self):
        inference = Mock()
        with (server(distributed=True, model=MODEL, inference=inference) as port,
              patch.object(runner.store, "admit", return_value=ACCEPTED) as admit,
              patch.object(runner.Handler, "await_reply", return_value=(200, {})) as legacy_wait):
            for path, status in [("/v1/runs", 202), ("/v1/chat/completions", 200)]:
                self.assertEqual(post(port, {"messages": [{"role": "user", "content": "hello"}]}, path=path)[0], status)
                self.assertEqual(admit.call_args.args[0].model, MODEL)
                self.assertEqual(admit.call_args.kwargs, {"distributed": True})
            self.assertEqual(inference.mock_calls, [])
            legacy_wait.assert_called_once_with(RUN, False)

    def test_completion_consumer_scans_before_waiting_and_rechecks_deadlines(self):
        signal = object()
        ready = threading.Event()
        database = Mock()
        database.listen.return_value = nullcontext(signal)
        database.finalize_ready.side_effect = [runner.RESULT_BATCH, 0]
        database.expire_runs.return_value = 0
        database.wait_for_signal.side_effect = KeyboardInterrupt
        with patch.object(runner, "store", database), self.assertRaises(KeyboardInterrupt):
            runner.complete_dispatched_runs(ready)
        self.assertTrue(ready.is_set())
        database.listen.assert_called_once_with("forge_agent_results")
        self.assertEqual(database.finalize_ready.call_count, 2)
        self.assertEqual(database.expire_runs.call_count, 2)
        database.wait_for_signal.assert_called_once_with(signal, runner.RESULT_SCAN_SECONDS)
