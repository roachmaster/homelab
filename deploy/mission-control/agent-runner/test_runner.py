import unittest
from unittest.mock import patch

from queue_store import Rejected
from runner import Inference, submission


class CompletionTest(unittest.TestCase):
    def complete(self, finish_reason, content, *, allow_partial=False):
        model = "gpt-oss-20b"
        command = submission(
            {"model": model, "messages": [{"role": "user", "content": "Explain this."}]},
            None,
            allow_partial=allow_partial,
        )
        inference = Inference("unused")
        runtime = {"state": "ready", "model": model}
        completion = {"model": model, "choices": [{
            "finish_reason": finish_reason,
            "message": {"role": "assistant", "content": content},
        }], "usage": {"completion_tokens": 384}}
        with patch.object(inference, "request", side_effect=[runtime, completion]):
            return inference.complete(command)

    def test_visible_partial_answer_is_available_on_the_chat_route(self):
        self.assertEqual(
            self.complete("length", "An answer", allow_partial=True),
            ("An answer", {"completion_tokens": 384}, "length"),
        )

    def test_async_runs_and_reasoning_only_replies_still_fail_on_length(self):
        for content, allow_partial in [("An answer", False), (None, True), (" ", True)]:
            with self.subTest(content=content, allow_partial=allow_partial):
                with self.assertRaisesRegex(Rejected, "output_budget_exhausted"):
                    self.complete("length", content, allow_partial=allow_partial)

    def test_complete_answer_preserves_stop(self):
        self.assertEqual(self.complete("stop", "An answer")[2], "stop")
