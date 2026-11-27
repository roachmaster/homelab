"""Replay real sanitized provider errors without invoking an agent or its API."""
import json
import unittest
from uuid import uuid4

from work_executor import CliParser
from work_quota_diagnostics import quota_diagnostic


ACTUAL_ERROR = ('RESOURCE_EXHAUSTED (code 429): Individual quota reached. '
                'Please upgrade your subscription to increase your limits. Resets in 20m56s.')


class QuotaDiagnosticTest(unittest.TestCase):
    def result(self, message, *, success=False, stderr=False):
        parser = CliParser('antigravity')
        result = dict(status='SUCCESS' if success else 'ERROR', conversation_id=str(uuid4()),
                      response=message if success else '', usage={}, denied_actions=[])
        if not success:
            result['error'] = dict(code='RESOURCE_EXHAUSTED', message=message)
        parser.feed(json.dumps(dict(event='result', result=result)).encode()+b'\n')
        if stderr:
            parser.feed(message.encode(), stderr=True)
        return parser.finish(0 if success else 3, {'scope_stopped': True})

    def test_actual_account_quota_error_is_blocked_and_reset_is_typed(self):
        outcome = self.result(ACTUAL_ERROR)
        self.assertEqual(outcome.status, 'quota_blocked')
        self.assertEqual(outcome.quota_diagnostic['retry_after_seconds'], 1256)
        self.assertEqual(outcome.quota_diagnostic['code'], 'provider_quota_exhausted')
        self.assertNotIn('upgrade', outcome.summary.lower())
        self.assertNotIn(ACTUAL_ERROR, str(outcome.quota_diagnostic))

    def test_actual_third_party_quota_uses_existing_wait_or_handoff_path(self):
        message=('RESOURCE_EXHAUSTED (code 429): Third-party quota reached. '
                 'Please switch to Gemini models or upgrade your subscription. Resets in 4h52m54s.')
        outcome=self.result(message)
        self.assertEqual(outcome.status,'quota_blocked')
        self.assertEqual(outcome.quota_diagnostic['retry_after_seconds'],17574)
        self.assertNotIn('upgrade',outcome.summary.lower())
        self.assertEqual(self.result(message,success=True).status,'awaiting_review')

    def test_transient_429_resource_or_success_prose_never_becomes_quota(self):
        for message in ('HTTP 429 Too many requests; retry in 2s.',
                        'RESOURCE_EXHAUSTED (code 429): temporary concurrency limit.',
                        'RESOURCE_EXHAUSTED: memory allocation failed.'):
            with self.subTest(message=message):
                self.assertIsNone(quota_diagnostic(message))
                self.assertEqual(self.result(message).status, 'failed')
        self.assertEqual(self.result(ACTUAL_ERROR, success=True).status, 'awaiting_review')

    def test_stderr_chunking_retains_quota_and_reset_without_scanning_reply(self):
        parser = CliParser('antigravity')
        for offset in range(0, len(ACTUAL_ERROR), 7):
            parser.feed(ACTUAL_ERROR[offset:offset+7].encode(), stderr=True)
        outcome = parser.finish(3, {'scope_stopped': True})
        self.assertEqual(outcome.status, 'quota_blocked')
        self.assertEqual(outcome.quota_diagnostic['retry_after_seconds'], 1256)

    def test_reset_duration_is_bounded_and_errors_are_not_retained(self):
        for value, expected in [('1h2m3s',3723), ('0s',None), ('99999h',None),
                                ('1m2m',None), ('2s1h',None), ('24h',86400), ('2d',None)]:
            with self.subTest(value=value):
                diagnostic = quota_diagnostic('Individual quota reached. Resets in '+value+'.')
                self.assertEqual(diagnostic['retry_after_seconds'], expected)
        self.assertIsNone(quota_diagnostic('x'*4096+ACTUAL_ERROR))


if __name__ == '__main__':
    unittest.main()
