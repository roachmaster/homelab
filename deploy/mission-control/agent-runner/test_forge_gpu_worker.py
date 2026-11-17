"""Verify Forge-local inference scheduling without a socket or live database."""

from contextlib import nullcontext
import json
import unittest
from unittest.mock import Mock, call, patch

import psycopg2

import forge_gpu_worker as gpu
from model_adapter import MAIL_MODEL, MAIL_RESPONSE_SECONDS, MODEL_LOAD_SECONDS, ModelAdapter, ModelError


JOB = {
    "run_id": "72c85704-8c75-4e7e-982f-19cd5587842f",
    "attempt_token": "4fb285b2-5a53-4caa-90b6-d0aff012c0a4",
    "model": "gpt-oss-20b",
    "messages": [{"role": "user", "content": "hello"}],
    "max_tokens": 128,
    "temperature": 0.7,
    "allow_partial": True,
}
QWEN = "qwen2.5-coder-7b"


class ForgeWorkerTest(unittest.TestCase):
    def test_subscribes_then_checks_model_and_drains_durable_jobs(self):
        store, adapter = Mock(), Mock()
        adapter.ready_model.return_value = JOB["model"]
        store.listen.return_value = nullcontext("listener")
        store.next_model.side_effect = [JOB["model"], None]
        store.claim.return_value = JOB
        store.wait.side_effect = KeyboardInterrupt
        worker = gpu.ForgeWorker(store, adapter)
        with patch.object(worker, "process") as process, self.assertRaises(KeyboardInterrupt):
            worker.run_forever()
        self.assertEqual(store.mock_calls, [
            call.listen(),
            call.next_model(sorted(gpu.MODELS)),
            call.advertise(JOB["model"], True), call.claim(JOB["model"]),
            call.next_model(sorted(gpu.MODELS)), call.advertise(JOB["model"], True),
            call.wait("listener"),
        ])
        process.assert_called_once_with(JOB)
        adapter.load_model.assert_not_called()

    def test_loads_the_next_queued_model_before_advertising_and_claiming(self):
        store, adapter = Mock(), Mock()
        calls = Mock()
        calls.attach_mock(store, "store")
        calls.attach_mock(adapter, "adapter")
        adapter.ready_model.side_effect = [JOB["model"], QWEN]
        store.next_model.side_effect = [QWEN, None]
        qwen_job = {**JOB, "model": QWEN}
        store.claim.return_value = qwen_job
        store.wait.side_effect = KeyboardInterrupt
        worker = gpu.ForgeWorker(store, adapter)
        with patch.object(worker, "process") as process, self.assertRaises(KeyboardInterrupt):
            worker.run_listener("listener")
        self.assertEqual(calls.mock_calls, [
            call.adapter.ready_model(), call.store.next_model(sorted(gpu.MODELS)),
            call.store.advertise(JOB["model"], False), call.adapter.load_model(QWEN),
            call.store.advertise(QWEN, True), call.store.claim(QWEN),
            call.adapter.ready_model(), call.store.next_model(sorted(gpu.MODELS)),
            call.store.advertise(QWEN, True), call.store.wait("listener"),
        ])
        process.assert_called_once_with(qwen_job)

    def test_a_model_load_failure_backs_off_and_drains_other_work(self):
        store, adapter = Mock(), Mock()
        adapter.ready_model.return_value = JOB["model"]
        adapter.load_model.side_effect = ModelError(503, "forge_unavailable")
        store.next_model.side_effect = [QWEN, JOB["model"], None]
        store.claim.return_value = JOB
        store.wait.side_effect = KeyboardInterrupt
        worker = gpu.ForgeWorker(store, adapter)
        with (patch.object(worker, "process") as process, patch.object(gpu.time, "monotonic", return_value=10),
              self.assertLogs(gpu.LOG, level="WARNING"), self.assertRaises(KeyboardInterrupt)):
            worker.run_listener("listener")
        ready_models = sorted(gpu.MODELS - {QWEN})
        self.assertEqual(store.next_model.call_args_list, [call(sorted(gpu.MODELS)), call(ready_models),
                                                          call(ready_models)])
        self.assertEqual(worker.model_retry_at, {QWEN: 10 + gpu.MODEL_RETRY_SECONDS})
        adapter.load_model.assert_called_once_with(QWEN)
        store.claim.assert_called_once_with(JOB["model"])
        process.assert_called_once_with(JOB)

    def test_adapter_loads_only_a_matching_ready_model(self):
        adapter = ModelAdapter("test-token", 18080)
        with patch.object(adapter, "request") as request:
            request.side_effect = [{"state": "ready", "model": QWEN},
                                   {"state": "ready", "model": JOB["model"]}]
            adapter.load_model(QWEN)
            request.assert_called_once_with("PUT", "/v1/runtime", {"model": QWEN}, timeout=MODEL_LOAD_SECONDS)
            with self.assertRaises(ModelError):
                adapter.load_model(QWEN)

    def test_only_mail_uses_the_background_generation_timeout(self):
        adapter = ModelAdapter("test-token", 18080)
        for model, expected_options in ((JOB["model"], {}), (MAIL_MODEL, {"timeout": MAIL_RESPONSE_SECONDS})):
            with self.subTest(model=model), patch.object(adapter, "request") as request:
                request.side_effect = [
                    {"state": "ready", "model": model},
                    {"model": model, "choices": [{"finish_reason": "stop",
                     "message": {"role": "assistant", "content": "answer"}}]},
                ]
                payload = {"model": model, "messages": JOB["messages"]}
                self.assertEqual(adapter.generate(model, payload), ("answer", {}, "stop"))
                self.assertEqual(request.call_args_list, [
                    call("GET", "/v1/runtime", timeout=3),
                    call("POST", "/v1/chat/completions", payload, **expected_options),
                ])

    def test_retries_an_ambiguous_publish_with_the_same_attempt_and_outcome(self):
        store, adapter = Mock(), Mock()
        adapter.generate.return_value = (
            "saved answer", {"completion_tokens": 5, "total_tokens": -1, "other": "ignored"}, "stop",
        )
        store.publish.side_effect = [psycopg2.OperationalError("lost response"), True]
        gpu.ForgeWorker(store, adapter).process(JOB)
        result = {"answer": "saved answer", "usage": {"completion_tokens": 5}, "finish_reason": "stop"}
        self.assertEqual(store.publish.mock_calls, [
            call(JOB["run_id"], JOB["attempt_token"], result),
            call(JOB["run_id"], JOB["attempt_token"], result),
        ])
        adapter.generate.assert_called_once()
        model, request = adapter.generate.call_args.args
        self.assertEqual(model, JOB["model"])
        self.assertEqual(request, {
            "model": JOB["model"], "messages": JOB["messages"], "max_tokens": JOB["max_tokens"],
            "temperature": JOB["temperature"], "stream": False,
        })
        self.assertTrue(adapter.generate.call_args.kwargs["allow_partial"])
        store.renew.assert_not_called()

    def test_closes_a_lost_attempt_without_publishing_its_late_result(self):
        store, adapter = Mock(), Mock()
        store.renew.side_effect = [True, False]

        def local_generate(_model, _request, *, allow_partial, stop):
            self.assertTrue(allow_partial)
            self.assertTrue(stop.wait(2))
            return "this reply belongs to the lost attempt", {}, "stop"

        adapter.generate.side_effect = local_generate
        with patch.object(gpu, "RENEW_SECONDS", 0.005):
            gpu.ForgeWorker(store, adapter).process(JOB)
        self.assertEqual(store.renew.mock_calls, [call(JOB["run_id"], JOB["attempt_token"])] * 2)
        store.advertise.assert_called_once_with(JOB["model"], True)
        store.publish.assert_not_called()
        self.assertTrue(adapter.generate.call_args.kwargs["stop"].is_set())

    def test_model_failure_is_a_typed_result(self):
        store, adapter = Mock(), Mock()
        adapter.generate.side_effect = ModelError(504, "forge_timeout")
        gpu.ForgeWorker(store, adapter).process(JOB)
        store.publish.assert_called_once_with(JOB["run_id"], JOB["attempt_token"], {"error": "forge_timeout"})

    def test_mail_forwards_the_pinned_profile_and_maps_a_preflight_rejection(self):
        store = Mock()
        adapter = ModelAdapter("test-token", 18080)
        profile = {"profile_id": "gpt-oss-120b-harmony-v1", "prompt_tokens": 224}
        job = {**JOB, "model": MAIL_MODEL, "max_tokens": 1024, "mail_profile": profile}
        with patch("model_adapter.http.client.HTTPConnection") as connect:
            wire = connect.return_value
            wire.getresponse.side_effect = [
                Mock(status=200, read=Mock(return_value=b'{"state":"ready","model":"gpt-oss-120b"}')),
                Mock(status=422),
            ]
            gpu.ForgeWorker(store, adapter).process(job)
            self.assertEqual(connect.call_args_list, [call("127.0.0.1", 18080, timeout=3),
                                                     call("127.0.0.1", 18080, timeout=MAIL_RESPONSE_SECONDS)])
            post = wire.request.call_args.args
            self.assertEqual(post[:2], ("POST", "/v1/chat/completions"))
            self.assertEqual(json.loads(post[2])["mail_profile"], profile)
        store.publish.assert_called_once_with(JOB["run_id"], JOB["attempt_token"], {"error": "mail_profile_mismatch"})

    def test_mail_job_requires_profile_before_contacting_the_adapter(self):
        store, adapter = Mock(), Mock()
        gpu.ForgeWorker(store, adapter).process({**JOB, "model": MAIL_MODEL})
        adapter.generate.assert_not_called()
        store.publish.assert_called_once_with(JOB["run_id"], JOB["attempt_token"], {"error": "mail_profile_mismatch"})
