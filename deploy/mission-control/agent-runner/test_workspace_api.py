"""Exercise the authenticated workspace HTTP adapter without a live database."""

from contextlib import contextmanager
from http.client import HTTPConnection
from http.server import ThreadingHTTPServer
import json
import threading
import unittest
from unittest.mock import patch

import runner


TAB = "a" * 32
LEASE = "b" * 64
TOKEN = "local-test-token"
RESTORE = {"operation": "restore", "tab": TAB}


def save(snapshot):
    return {"operation": "save", "tab": TAB, "lease": LEASE, "revision": 1, "snapshot": snapshot}


def snapshot(*conversations):
    return {"chat": {"selected": 0, "conversations": list(conversations)},
            "workbench": None, "workbench_open": False}


@contextmanager
def server(*, distributed=False, model=runner.DEFAULT_MODEL, inference=None, models=None):
    with ThreadingHTTPServer(("127.0.0.1", 0), runner.Handler) as http:
        http.authorization = f"Bearer {TOKEN}".encode()
        http.distributed = distributed
        http.pinned_model = model
        http.configured_models = (model,) if models is None else models
        http.inference = inference
        worker = threading.Thread(target=lambda: http.serve_forever(poll_interval=0.01), daemon=True)
        worker.start()
        try:
            yield http.server_port
        finally:
            http.shutdown()
            worker.join()


def post(port, body, *, path=runner.WORKSPACE_PATH, headers=None):
    headers = {"Authorization": f"Bearer {TOKEN}", "Content-Type": "application/json", **(headers or {})}
    payload = body if isinstance(body, bytes) else runner.workspace_json(body)
    connection = HTTPConnection("127.0.0.1", port, timeout=2)
    try:
        connection.request("POST", path, payload, headers)
        response = connection.getresponse()
        return response.status, response.read(), response.getheader("Cache-Control")
    finally:
        connection.close()


class WorkspaceApiTest(unittest.TestCase):
    def test_authenticated_restore_and_save_preserve_unicode_and_exact_envelope(self):
        saved = snapshot({"draft": "café", "messages": []})
        restored = {"lease": LEASE, "snapshot": saved}
        with (server() as port,
              patch.object(runner.store, "restore_workspace", return_value=restored) as restore,
              patch.object(runner.store, "save_workspace", return_value={}) as store):
            status, body, cache = post(port, RESTORE)
            self.assertEqual((status, json.loads(body), cache), (200, restored, "no-store"))
            self.assertIn("café".encode("utf-8"), body)
            restore.assert_called_once_with(TAB)
            status, body, _ = post(port, save(saved))
            self.assertEqual((status, json.loads(body)), (200, {}))
            store.assert_called_once_with(TAB, LEASE, 1, saved)

    def test_invalid_request_does_not_reach_store(self):
        valid = save(snapshot())
        duplicate = f'{{"operation":"restore","tab":"{TAB}","tab":"{TAB}"}}'.encode()
        with (server() as port, patch.object(runner.store, "restore_workspace") as restore,
              patch.object(runner.store, "save_workspace") as store):
            for body, options, status, error in [
                (b"invalid", {"headers": {"Authorization": "Bearer other"}}, 401, "unauthorized"),
                (RESTORE, {"path": runner.WORKSPACE_PATH + "?"}, 404, "route_not_found"),
                (RESTORE, {"headers": {"Content-Type": "text/plain"}}, 415, "json_required"),
                (b"", {"headers": {"Content-Length": str(runner.MAX_WORKSPACE_BODY + 1)}},
                 413, "invalid_body_size"),
                (duplicate, {}, 400, "invalid_workspace_json"),
                (b'{"operation":"restore","tab":NaN}', {}, 400, "invalid_workspace_json"),
                (b'{"operation":"restore","tab":"\\ud800"}', {}, 400, "invalid_workspace_json"),
                (save(snapshot({"draft": "co\0py", "messages": []})), {}, 400, "invalid_workspace_json"),
                (dict(valid, revision=True), {}, 400, "invalid_workspace_request"),
                (dict(RESTORE, models=True), {}, 400, "invalid_workspace_request"),
                (save(snapshot({"draft": "", "messages": [{"role": "user", "content": "stray"}]})),
                 {}, 400, "invalid_workspace_snapshot"),
            ]:
                with self.subTest(expected=error):
                    actual, payload, _ = post(port, body, **options)
                    self.assertEqual((actual, json.loads(payload)), (status, {"error": error}))
            restore.assert_not_called()
            store.assert_not_called()

    def test_model_aware_restore_is_opt_in(self):
        with (server(distributed=True) as port,
              patch.object(runner.store, "restore_workspace",
                           return_value={"lease": LEASE, "snapshot": None}) as restore):
            self.assertEqual(post(port, dict(RESTORE, history=True))[0], 200)
            restore.assert_called_once_with(TAB, history_enabled=True)
            restore.reset_mock()
            self.assertEqual(post(port, dict(RESTORE, history=True, models=True))[0], 200)
            restore.assert_called_once_with(TAB, history_enabled=True, models=True)

    def test_inert_restore_and_save_require_explicit_true_and_route_without_hydration(self):
        saved = snapshot({"draft": "dormant", "messages": [], "chat_id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", "revision": 3})
        saved["chat"]["model_id"] = "gpt-oss-20b"
        restored = {"lease": LEASE, "snapshot": saved}
        with (server() as port,
              patch.object(runner.store, "restore_workspace", return_value=restored) as restore,
              patch.object(runner.store, "save_workspace", return_value={}) as store):
            status, body, cache = post(port, dict(RESTORE, preserve_chat=True))
            self.assertEqual((status, json.loads(body), cache), (200, restored, "no-store"))
            restore.assert_called_once_with(TAB, preserve_chat=True)
            self.assertEqual(post(port, dict(save(saved), preserve_chat=True))[0], 200)
            store.assert_called_once_with(TAB, LEASE, 1, saved, preserve_chat=True)
            restore.reset_mock()
            store.reset_mock()
            for request in (
                dict(RESTORE, preserve_chat=True, history=True),
                dict(RESTORE, preserve_chat=True, models=True),
                dict(RESTORE, preserve_chat=True, history=True, models=True),
                dict(RESTORE, preserve_chat=False),
                dict(RESTORE, preserve_chat=1),
                dict(save(saved), preserve_chat=False),
                dict(save(saved), preserve_chat=1),
                dict(save(saved), preserve_chat=True, history=True),
            ):
                with self.subTest(request=request):
                    self.assertEqual(post(port, request)[0], 400)
            restore.assert_not_called()
            store.assert_not_called()

    def test_stale_lease_and_oversized_stored_response_return_generic_errors(self):
        stale = runner.Rejected(401, "workspace_lease")
        with (server() as port, patch.object(runner.store, "save_workspace", side_effect=stale),
              patch.object(runner.store, "restore_workspace", return_value={"text": "x" * 250})):
            status, body, _ = post(port, save(snapshot()))
            self.assertEqual((status, json.loads(body)), (401, {"error": "workspace_lease"}))
            with patch.object(runner, "MAX_WORKSPACE_BODY", 128):
                status, body, _ = post(port, RESTORE)
            self.assertEqual((status, json.loads(body)), (503, {"error": "invalid_workspace_response"}))


if __name__ == "__main__":
    unittest.main()
