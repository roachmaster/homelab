"""Header routing and typed Mail requests through the private runner."""

import unittest
from unittest.mock import Mock, patch
from uuid import UUID

import runner
from queue_store import Rejected, TokenizerUnavailable
from test_chat_run_api import REQUEST_ID, dispatch_post, reply
from test_workspace_api import server


THREAD = UUID("4fb285b2-5a53-4caa-90b6-d0aff012c0a4")
RUN = UUID("72c85704-8c75-4e7e-982f-19cd5587842f")
SEND = {"request_id": REQUEST_ID, "kind": "mail_send", "thread_id": str(THREAD),
        "subject": "Research question", "content": "Can you explain?", "idempotency_key": "draft-1"}
RECEIPT = {"thread_id": THREAD, "run_id": RUN, "status": "queued"}


class MailApiTest(unittest.TestCase):
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
