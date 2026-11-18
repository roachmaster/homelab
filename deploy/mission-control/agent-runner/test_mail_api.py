"""Header routing and typed Mail requests through the private runner."""

from contextlib import nullcontext
import unittest
from unittest.mock import Mock, patch
from uuid import UUID

import runner
import mail_api
from queue_store import Rejected, TokenizerUnavailable
from test_chat_run_api import REQUEST_ID, dispatch_post, reply
from test_workspace_api import server


THREAD = UUID("4fb285b2-5a53-4caa-90b6-d0aff012c0a4")
RUN = UUID("72c85704-8c75-4e7e-982f-19cd5587842f")
SEND = {"request_id": REQUEST_ID, "kind": "mail_send", "thread_id": str(THREAD),
        "subject": "Research question", "content": "Can you explain?", "idempotency_key": "draft-1"}
RECEIPT = {"thread_id": THREAD, "run_id": RUN, "status": "queued"}


class MailApiTest(unittest.TestCase):
    def test_metadata_is_opt_in_without_changing_owner_or_paging(self):
        thread = {"thread_id": str(THREAD), "subject": "Mail fixture", "status": "ready",
                  "updated_at": "2026-11-18T12:00:00Z", "turn_count": 1, "revision": 2,
                  "latest_run_id": str(RUN), "archive_reason": None, "preview": "A stored reply"}
        message = {"sequence": 1, "role": "assistant", "content": "A stored reply",
                   "created_at": "2026-11-18T12:00:00Z"}
        listed = {"items": [thread], "next_cursor": "next-page"}
        opened = {"thread": {**thread, "context_remaining_tokens": 1024},
                  "messages": [message], "next_sequence": 1}
        legacy_thread = {key: value for key, value in thread.items() if key != "preview"}
        cursor = Mock()
        with (server(distributed=True) as port,
              patch.object(mail_api.queue, "transaction", return_value=nullcontext(cursor)),
              patch.object(mail_api.mail_store, "list_threads", return_value=listed) as list_threads,
              patch.object(mail_api.mail_store, "open_thread", return_value=opened) as open_thread):
            for metadata in [{}, {"include_metadata": False}, {"include_metadata": True}]:
                enabled = metadata.get("include_metadata", False)
                body = {"request_id": REQUEST_ID, "kind": "mail_list", "archived": False,
                        "limit": 2, "cursor": "page-before", **metadata}
                self.assertEqual(dispatch_post(port, "ide.mail.list", body), (
                    200, reply("mail_listed", items=[thread if enabled else legacy_thread], next_cursor="next-page"),
                    "no-store",
                ))
                list_threads.assert_called_with(cursor, mail_api.queue.OWNER, 2, "page-before", False)
                body = {"request_id": REQUEST_ID, "kind": "mail_open", "thread_id": str(THREAD),
                        "limit": 1, "after_sequence": 0, **metadata}
                expected = opened if enabled else {
                    "thread": {**legacy_thread, "context_remaining_tokens": 1024},
                    "messages": [{key: value for key, value in message.items() if key != "created_at"}],
                    "next_sequence": 1,
                }
                self.assertEqual(dispatch_post(port, "ide.mail.open", body),
                                 (200, reply("mail_opened", **expected), "no-store"))
                open_thread.assert_called_with(cursor, mail_api.queue.OWNER, THREAD, 1, 0)
            self.assertIn("preview", listed["items"][0], "legacy projection must not mutate the DAO result")
            self.assertIn("created_at", opened["messages"][0])
            calls = list_threads.call_count
            body = {"request_id": REQUEST_ID, "kind": "mail_list", "include_metadata": True}
            self.assertEqual(dispatch_post(port, "ide.mail.list", body, Authorization="Bearer other")[0], 401)
            self.assertEqual(list_threads.call_count, calls)
            self.assertEqual(dispatch_post(port, "ide.mail.list", {**body, "owner": "other"})[0], 400)
            self.assertEqual(list_threads.call_count, calls)

    def test_metadata_flag_rejects_non_boolean_values_before_store_access(self):
        with (server(distributed=True) as port,
              patch.object(mail_api.queue, "transaction") as transaction):
            for destination, base in [
                ("ide.mail.list", {"kind": "mail_list"}),
                ("ide.mail.open", {"kind": "mail_open", "thread_id": str(THREAD)}),
            ]:
                for value in [None, 0, 1, "true", [], {}]:
                    body = {"request_id": REQUEST_ID, **base, "include_metadata": value}
                    self.assertEqual(dispatch_post(port, destination, body),
                                     (400, reply("mail_failed", code="invalid_request"), "no-store"))
            transaction.assert_not_called()

    def test_mail_send_and_retry_acknowledge_without_inference(self):
        inference = Mock()
        with (server(distributed=True, inference=inference) as port,
              patch.object(runner.store, "admit_mail", return_value=RECEIPT) as admit,
              patch.object(runner.store, "retry_mail", return_value=RECEIPT) as retry,
              patch.object(runner.Handler, "await_reply") as await_reply):
            expected = (202, reply("mail_sent", thread_id=str(THREAD), run_id=str(RUN), status="queued"), "no-store")
            self.assertEqual(dispatch_post(port, "ide.mail.send", SEND), expected)
            admit.assert_called_once_with(THREAD, None, SEND["subject"], SEND["content"], SEND["idempotency_key"])
            self.assertEqual(dispatch_post(port, "ide.mail.retry", {
                "request_id": REQUEST_ID, "kind": "mail_retry", "run_id": str(RUN), "idempotency_key": "retry",
            }), expected)
            retry.assert_called_once_with(RUN, "retry")
            await_reply.assert_not_called()
            self.assertEqual(inference.mock_calls, [])

    def test_mail_rejects_invalid_inputs_and_maps_context_errors(self):
        with server(distributed=True) as port, patch.object(runner.store, "admit_mail") as admit:
            for body in [dict(SEND, owner="other"), dict(SEND, expected_revision=1), dict(SEND, subject=""),
                         dict(SEND, content="⚙" * 1366), dict(SEND, content="x\x01"),
                         dict(SEND, request_id="mismatch")]:
                self.assertEqual(dispatch_post(port, "ide.mail.send", body),
                                 (400, reply("mail_failed", code="invalid_request"), "no-store"))
            admit.assert_not_called()
            for error, status, code in [
                (Rejected(422, "context_limit"), 422, "context_limit"),
                (Rejected(409, "revision_conflict"), 409, "revision_conflict"),
                (TokenizerUnavailable("tokenizer test"), 503, "unavailable"),
            ]:
                admit.side_effect = error
                self.assertEqual(dispatch_post(port, "ide.mail.send", SEND),
                                 (status, reply("mail_failed", code=code), "no-store"))
