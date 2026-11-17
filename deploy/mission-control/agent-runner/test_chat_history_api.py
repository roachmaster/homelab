"""Verify the private chat history dispatch and linked IDE completion contract."""

from contextlib import closing, nullcontext
from http.client import HTTPConnection
import json
import unittest
from unittest.mock import patch
from uuid import UUID

import psycopg2

import chat_history_api as history
import runner
from test_workspace_api import RESTORE, TAB, TOKEN, post, server


CHAT_ID = UUID("4fb285b2-5a53-4caa-90b6-d0aff012c0a4")
RUN_ID = UUID("72c85704-8c75-4e7e-982f-19cd5587842f")
REQUEST_ID = "history-test"
POLICY = {"kind": "long_term"}
SUMMARY = {
    "chat_id": str(CHAT_ID),
    "title": "History example",
    "updated_at": "2026-11-16T10:00:00Z",
    "revision": 4,
    "retention": POLICY,
}


def headers(route):
    return {
        "Forge-Envelope-Version": "1",
        "Forge-Contract-Version": "1",
        "Forge-Destination": route.value,
        "Forge-Request-Id": REQUEST_ID,
    }


def history_post(port, route, body, *, extra_headers=None, path=history.PATH):
    status, raw, cache = post(port, body, path=path, headers=dict(headers(route), **(extra_headers or {})))
    return status, json.loads(raw), cache


class ChatHistoryApiTest(unittest.TestCase):
    def test_registered_destinations_match_exact_requests_and_response_envelopes(self):
        opened = {"summary": SUMMARY, "messages": [{"position": 1, "role": "user", "content": "café?"}]}
        with server() as port, patch.object(history.history, "dao") as dao:
            dao.list_chats.return_value = {"items": [SUMMARY], "next_cursor": None}
            dao.open_chat.return_value = opened
            dao.set_retention.return_value = {"chat_id": str(CHAT_ID), "revision": 5, "retention": POLICY}
            dao.delete_chat.return_value = {"chat_id": str(CHAT_ID), "revision": 6}
            cases = [
                (history.Destination.LIST, "chat_history_list", {"limit": 20, "cursor": "opaque"},
                 "chat_history_listed", dao.list_chats.return_value),
                (history.Destination.OPEN, "chat_history_open", {"chat_id": str(CHAT_ID)},
                 "chat_history_opened", {"chat": opened}),
                (history.Destination.RETENTION, "chat_history_retention_set",
                 {"chat_id": str(CHAT_ID), "expected_revision": 4, "policy": POLICY},
                 "chat_history_retention_set", dao.set_retention.return_value),
                (history.Destination.DELETE, "chat_history_delete",
                 {"chat_id": str(CHAT_ID), "expected_revision": 5},
                 "chat_history_deleted", dao.delete_chat.return_value),
            ]
            for route, kind, fields, response, result in cases:
                with self.subTest(destination=route):
                    request = {"kind": kind, "request_id": REQUEST_ID, **fields}
                    self.assertEqual(history_post(port, route, request), (
                        200, history.envelope(response, REQUEST_ID, **result), "no-store",
                    ))
            dao.list_chats.assert_called_once_with(cursor_token="opaque", limit=20)
            dao.open_chat.assert_called_once_with(CHAT_ID)
            dao.set_retention.assert_called_once_with(CHAT_ID, 4, POLICY)
            dao.delete_chat.assert_called_once_with(CHAT_ID, 5)

    def test_authentication_and_exact_routing_reject_invalid_requests_before_data_access(self):
        valid = {"kind": "chat_history_delete", "request_id": REQUEST_ID,
                 "chat_id": str(CHAT_ID), "expected_revision": 4}
        delete = history.Destination.DELETE
        failure = history.envelope("chat_history_failed", REQUEST_ID, code="invalid_request")
        with server() as port, patch.object(history.history, "dao") as dao:
            for route, body, options, status, response in [
                (delete, valid, {"extra_headers": {"Authorization": "Bearer wrong"}}, 401, {"error": "unauthorized"}),
                (delete, valid, {"path": history.PATH + "?"}, 404, {"error": "route_not_found"}),
                (delete, valid, {"extra_headers": {"Forge-Contract-Version": "2"}}, 400, failure),
                (delete, valid, {"extra_headers": {"Forge-Envelope-Version": "01"}}, 400, failure),
                (delete, valid, {"extra_headers": {"Forge-Destination": "not.registered"}},
                 404, history.envelope("chat_history_failed", REQUEST_ID, code="not_found")),
                (delete, valid, {"extra_headers": {"Forge-User": "someone"}}, 400, failure),
                (delete, valid, {"extra_headers": {"Content-Type": "text/plain"}}, 415, failure),
                (delete, b"x" * (history.MAX_REQUEST_BYTES + 1), {}, 413, failure),
                (history.Destination.OPEN, valid, {}, 400, failure),
                (delete, dict(valid, request_id="other"), {}, 400, failure),
                (delete, dict(valid, owner="someone"), {}, 400, failure),
                (delete, dict(valid, expected_revision=True), {}, 400, failure),
                (delete, dict(valid, chat_id=CHAT_ID.hex), {}, 400, failure),
                (delete, json.dumps(valid).replace('"expected_revision": 4',
                 '"expected_revision": 4, "expected_revision": 5').encode(), {}, 400, failure),
            ]:
                with self.subTest(request=options, status=status):
                    self.assertEqual(history_post(port, route, body, **options), (status, response, "no-store"))
            self.assertEqual(dao.mock_calls, [])
            for key, value, expected in [
                ("Authorization", "Bearer other", 401),
                ("Forge-Destination", history.Destination.OPEN.value, 400),
            ]:
                payload = json.dumps(valid).encode()
                with closing(HTTPConnection("127.0.0.1", port, timeout=2)) as connection:
                    connection.putrequest("POST", history.PATH)
                    for name, field in {
                        **headers(delete), "Content-Type": "application/json",
                        "Content-Length": str(len(payload)), "Authorization": f"Bearer {TOKEN}",
                    }.items():
                        connection.putheader(name, field)
                    connection.putheader(key.lower(), value)
                    connection.endheaders(payload)
                    self.assertEqual(connection.getresponse().status, expected)
            self.assertEqual(dao.mock_calls, [])

    def test_conflicts_missing_chats_invalid_policies_and_database_outages_are_typed(self):
        request = {"kind": "chat_history_retention_set", "request_id": REQUEST_ID, "chat_id": str(CHAT_ID),
                   "expected_revision": 4, "policy": {"kind": "expires", "expires_on": "2026-11-30"}}
        with server() as port, patch.object(history.history, "dao") as dao:
            for error, status, code in [
                (runner.Rejected(409, "revision_conflict"), 409, "revision_conflict"),
                (runner.Rejected(404, "chat_not_found"), 404, "not_found"),
                (runner.Rejected(400, "invalid_policy"), 400, "invalid_policy"),
                (psycopg2.OperationalError("private DSN"), 503, "unavailable"),
            ]:
                with self.subTest(code=code):
                    dao.set_retention.side_effect = error
                    self.assertEqual(history_post(port, history.Destination.RETENTION, request), (
                        status, history.envelope("chat_history_failed", REQUEST_ID, code=code), "no-store",
                    ))

    def test_linked_workspace_restore_requires_explicit_opt_in(self):
        with server() as port, patch.object(runner.store, "restore_workspace", return_value={}) as restore:
            self.assertEqual(post(port, RESTORE)[0], 200)
            restore.assert_called_once_with(TAB)
            restore.reset_mock()
            self.assertEqual(post(port, dict(RESTORE, history=True))[0], 200)
            restore.assert_called_once_with(TAB, history_enabled=True)
            self.assertEqual(post(port, dict(RESTORE, history=1))[0], 400)

    def test_linked_ide_completion_returns_the_committed_revision(self):
        request = {"model": "gpt-oss-20b", "messages": [{"role": "user", "content": "hello"}],
                   "chat_id": str(CHAT_ID), "expected_revision": 4}
        run = {"run_id": RUN_ID, "chat_id": CHAT_ID, "revision": 5, "model": request["model"],
               "status": "completed", "answer": "hello back", "finish_reason": "stop"}
        with (server() as port, patch.object(runner.store, "admit", return_value=run) as admit,
              patch.object(runner.store, "listen", return_value=nullcontext()),
              patch.object(runner.store, "get_run", return_value=run) as get_run):
            status, body, _ = post(port, request, path="/v1/chat/completions")
            self.assertEqual(status, 200)
            self.assertEqual(json.loads(body)["chat_id"], str(CHAT_ID))
            self.assertEqual(json.loads(body)["revision"], 5)
            self.assertEqual(admit.call_args.args[0].chat_id, CHAT_ID)
            self.assertEqual(admit.call_args.args[0].expected_revision, 4)
            status, _, _ = post(port, request, path="/v1/runs")
            self.assertEqual(status, 400)
            get_run.side_effect = runner.Rejected(404, "run_not_found")
            status, body, _ = post(port, request, path="/v1/chat/completions")
            self.assertEqual((status, json.loads(body)), (404, {"error": "chat_not_found"}))

    def test_expiry_scheduler_retries_after_database_outage(self):
        with (patch.object(runner.history_store.dao, "expire_due",
                           side_effect=[psycopg2.OperationalError("private DSN"), 1]) as expire,
              patch.object(runner.time, "sleep", side_effect=[None, InterruptedError]) as sleep,
              self.assertLogs(runner.LOG, "WARNING") as logs,
              self.assertRaises(InterruptedError)):
            runner.expire_chat_history()
        self.assertEqual(expire.call_count, 2)
        self.assertEqual(sleep.call_count, 2)
        sleep.assert_called_with(runner.HISTORY_EXPIRY_INTERVAL_SECONDS)
        self.assertNotIn("private DSN", logs.output[0])


if __name__ == "__main__":
    unittest.main()
