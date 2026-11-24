"""Exact Codex0.160 flattened web-search IDs are transport metadata only."""
import hashlib
import json
from pathlib import Path
import subprocess
import threading
import unittest
from tempfile import TemporaryDirectory
from unittest.mock import Mock, patch
from uuid import uuid4

from work_cli_transport import CliDiagnosticReporter, CliTransportError, parse_record
from work_executor import CliParser, CommandRunnerProvider, ExecutionError, WorkspaceLease
from work_worker import CodingWorkerFactory
from work_worker_config import parse_config
from test_work_executor import FakeSupervisor, lease, raw_config


def web_search(*, event='item.completed', ids='"id":"item_2","id":"ws_provider"', extra=''):
    return ('{"type":' + json.dumps(event) + ',"item":{' + ids +
        ',"type":"web_search","query":"fixture reference","action":{"type":"search","query":"fixture reference"}' + extra + '}}').encode()


class CodexWireRecordTest(unittest.TestCase):
    def test_exact_flattened_search_ids_keep_only_outer_item_identity(self):
        for event in ('item.started', 'item.updated', 'item.completed'):
            with self.subTest(event=event):
                value = parse_record(web_search(event=event), 'codex')
                self.assertEqual(value['item']['id'], 'item_2')
                self.assertEqual(value['item']['type'], 'web_search')
                self.assertEqual(value['item']['action'], {'type': 'search', 'query': 'fixture reference'})

    def test_single_search_id_and_ordinary_unique_events_stay_unchanged(self):
        value = parse_record(web_search(ids='"id":"item_2"'), 'codex')
        self.assertEqual(value['item']['id'], 'item_2')
        raw = b'{"type":"item.completed","item":{"id":"item_3","type":"agent_message","text":"Final report"}}'
        self.assertEqual(parse_record(raw, 'codex'), json.loads(raw))

    def test_duplicate_search_alias_does_not_apply_to_other_backends(self):
        for backend in ('antigravity', 'grok', 'muse'):
            with self.subTest(backend=backend), self.assertRaises(CliTransportError):
                parse_record(web_search(), backend)

    def test_duplicate_control_response_usage_and_nested_payload_fields_reject(self):
        records = [
            b'{"type":"turn.completed","type":"item.completed","item":{"id":"item_2","id":"ws_x","type":"web_search"}}',
            b'{"type":"thread.started","thread_id":"a","thread_id":"b"}',
            b'{"type":"item.completed","item":{"id":"item_2","id":"ws_x","type":"agent_message","text":"Unsafe result"}}',
            b'{"type":"item.completed","item":{"type":"agent_message","text":"a","text":"b"}}',
            b'{"type":"turn.completed","usage":{"input_tokens":1,"input_tokens":2}}',
            web_search(extra=',"query":"duplicate"'),
            web_search(extra=',"results":[{"id":"item_2","id":"ws_other","type":"web_search"}]'),
            web_search(extra=',"type":"web_search"'),
            web_search(event='other.event'),
            web_search(ids='"id":"item_2","id":"ws_a","id":"ws_b"'),
            web_search(ids='"id":"item_2","id":123'),
            web_search(ids='"id":"ws_a","id":"item_2"'),
        ]
        for raw in records:
            with self.subTest(record_sha256=hashlib.sha256(raw).hexdigest()), self.assertRaises(CliTransportError):
                parse_record(raw, 'codex')

    def test_invalid_json_utf8_constants_and_top_level_shape_reject(self):
        for raw, reason in ((b'not json', 'invalid_json'), (b'\xff', 'invalid_utf8'),
                (b'{"count":NaN}', 'nonfinite_number'), (b'[]', 'nonobject_record')):
            with self.subTest(reason=reason), self.assertRaises(CliTransportError) as caught:
                parse_record(raw, 'codex')
            self.assertEqual(caught.exception.reason, reason)

    def test_diagnostic_paths_never_include_unrecognized_or_secret_fields(self):
        with self.assertRaises(CliTransportError) as caught:
            parse_record(b'{"private_fixture_secret":{"token-value":1,"token-value":2}}', 'codex')
        self.assertEqual(caught.exception.path, '$.*.*')
        self.assertNotIn('private_fixture_secret', str(caught.exception))
        self.assertNotIn('token-value', str(caught.exception))

    def test_json_integer_conversion_bound_remains_a_controlled_transport_failure(self):
        raw = b'{"type":"turn.completed","usage":{"input_tokens":' + b'1'*5000 + b'}}'
        with self.assertRaises(CliTransportError) as decoded:
            parse_record(raw, 'codex')
        self.assertEqual(decoded.exception.reason, 'invalid_json')
        with self.assertRaises(ExecutionError) as parsed:
            CliParser('codex').feed(raw + b'\n')
        self.assertEqual(parsed.exception.outcome, 'failed')
        self.assertEqual(parsed.exception.transport_diagnostic['reason'], 'invalid_json')


class CodexSearchStreamTest(unittest.TestCase):
    def test_full_chunked_search_stream_keeps_real_response_usage_and_session(self):
        session = str(uuid4())
        response = 'Final review response with Unicode é🛠.'
        rows = [json.dumps({'type': 'thread.started', 'thread_id': session}).encode(),
            web_search(event='item.started'), web_search(event='item.updated'), web_search(),
            json.dumps({'type': 'item.completed', 'item': {'id': 'item_3',
                'type': 'agent_message', 'text': response}}, ensure_ascii=False).encode(),
            json.dumps({'type': 'turn.completed', 'usage': {'input_tokens': 10,
                'output_tokens': 3, 'cached_input_tokens': 4}}).encode()]
        raw = b'\n'.join(rows) + b'\n'
        for width in (1, 37, len(raw)):
            with self.subTest(width=width):
                parser = CliParser('codex')
                for start in range(0, len(raw), width):
                    parser.feed(raw[start:start+width])
                outcome = parser.finish(0, {'scope_stopped': True})
                self.assertEqual(outcome.status, 'awaiting_review')
                self.assertEqual(outcome.text, response)
                self.assertEqual(outcome.conversation_id, session)
                self.assertEqual(outcome.usage, {'input_tokens': 10, 'output_tokens': 3, 'cached_input_tokens': 4})

    def test_final_verdict_content_is_never_reparsed_or_rewritten(self):
        session = str(uuid4())
        verdict = '{"decision":"revise","decision":"approve","attested_finding_ids":["not-authority"]}'
        parser = CliParser('codex')
        for row in ({'type': 'thread.started', 'thread_id': session},
                {'type': 'item.completed', 'item': {'type': 'agent_message', 'text': verdict}},
                {'type': 'turn.completed', 'usage': {}}):
            parser.feed(json.dumps(row).encode() + b'\n')
        self.assertEqual(parser.finish(0, {'scope_stopped': True}).text, verdict)


class TransportDiagnosticTest(unittest.TestCase):
    def test_bad_record_reports_bounded_structural_metadata_without_raw_values(self):
        parser = CliParser('codex')
        parser.feed(b'{"type":"turn.started"}\n')
        raw = b'{"private_fixture_secret":{"token-value":1,"token-value":2}}'
        with self.assertRaises(ExecutionError) as caught:
            parser.feed(raw + b'\n')
        error = caught.exception
        self.assertEqual(error.summary, 'The coding agent returned malformed machine output.')
        diagnostic = error.transport_diagnostic
        self.assertEqual(diagnostic, dict(version=1, backend='codex', reason='duplicate_field',
            path='$.*.*', record_ordinal=2, record_byte_count=len(raw),
            record_sha256=hashlib.sha256(raw).hexdigest()))
        logger = Mock()
        identity = dict(task_id=str(uuid4()), attempt_id=str(uuid4()))
        CliDiagnosticReporter(logger).report(identity, diagnostic)
        logger.warning.assert_called_once()
        template, payload = logger.warning.call_args.args
        self.assertEqual(template, 'cli_transport_failure %s')
        self.assertEqual(json.loads(payload), {**diagnostic, **identity})
        self.assertNotIn('private_fixture_secret', payload)
        self.assertNotIn('token-value', payload)

    def test_reporter_rejects_arbitrary_metadata_and_paths_before_logging(self):
        logger = Mock()
        identity = dict(task_id=str(uuid4()), attempt_id=str(uuid4()))
        good = dict(version=1, backend='codex', reason='duplicate_field', path='$.item.id',
            record_ordinal=2, record_byte_count=153, record_sha256='a'*64)
        for extra in ({'raw': 'secret'}, {'path': '$.private_secret'}, {'reason': 'private_secret'},
                {'record_byte_count': 67108865}, {'record_ordinal': True}):
            with self.subTest(fields=tuple(extra)), self.assertRaises(ValueError):
                CliDiagnosticReporter(logger).report(identity, {**good, **extra})
        logger.warning.assert_not_called()

    def test_selected_ready_reporter_is_used_and_failure_preserves_original_outcome(self):
        reporter = Mock()
        provider = CommandRunnerProvider(object(), object(), transport_diagnostics=reporter)
        error = ExecutionError('failed', 'Original failure')
        error.transport_diagnostic = {'fixture': True}
        identity = dict(task_id=str(uuid4()), attempt_id=str(uuid4()))
        provider._report_transport_failure(identity, error)
        reporter.report.assert_called_once_with(identity, error.transport_diagnostic)
        reporter.report.side_effect = OSError('unavailable private logger')
        with patch('work_executor.LOG') as fallback:
            provider._report_transport_failure(identity, error)
        fallback.warning.assert_called_once_with('CLI structural diagnostics unavailable')
        self.assertEqual((error.outcome, error.summary), ('failed', 'Original failure'))

    def test_unselected_or_invalid_reporters_have_no_effect(self):
        error = ExecutionError()
        error.transport_diagnostic = {'fixture': True}
        CommandRunnerProvider(object(), object())._report_transport_failure({}, error)
        with self.assertRaises(TypeError):
            CommandRunnerProvider(object(), object(), transport_diagnostics=object())

    def test_normal_worker_factory_injects_selected_ready_reporter_at_acquisition(self):
        with TemporaryDirectory(prefix='forge-cli-diagnostics-') as root:
            config = parse_config(raw_config(root))
            reporter = Mock()
            with patch('work_worker.CliDiagnosticReporter', return_value=reporter) as selected, \
                    patch('work_worker.CommandRunnerProvider') as runner:
                factory = CodingWorkerFactory(config, Mock())
                selected.assert_not_called()
                supervisor = object()
                factory.runner_factory(config, supervisor)
                selected.assert_called_once()
                self.assertIs(runner.call_args.kwargs['transport_diagnostics'], reporter)
                reporter.report.assert_not_called()
            with patch('work_worker.CliDiagnosticReporter', side_effect=AssertionError('Unused default constructed')):
                replacement = Mock()
                factory = CodingWorkerFactory(config, Mock(), runner_factory=replacement)
                factory.runner_factory(config, supervisor)
                replacement.assert_called_once_with(config, supervisor)


class TransportProcessTest(unittest.TestCase):
    """Real local fake process reaches the normal runner; not a sandbox proof."""
    def test_supervised_runner_search_stream_completes_and_invalid_control_record_reports_once(self):
        for valid in (True, False):
            with self.subTest(valid=valid), TemporaryDirectory(prefix='forge-cli-transport-child-') as root:
                config = parse_config(raw_config(root))
                value = lease(config, backend='codex', mode='review')
                session = str(uuid4())
                rows = [json.dumps({'type': 'thread.started', 'thread_id': session}).encode(),
                    web_search() if valid else b'{"type":"thread.started","thread_id":"a","thread_id":"b"}',
                    json.dumps({'type': 'item.completed', 'item': {'type': 'agent_message', 'text': 'Fixture report'}}).encode(),
                    json.dumps({'type': 'turn.completed', 'usage': {'input_tokens': 3, 'output_tokens': 2}}).encode()]
                output = b'\n'.join(rows) + b'\n'
                supervisor = FakeSupervisor('import sys;sys.stdin.read();sys.stdout.buffer.write(' + repr(output) + ');sys.stdout.flush()')
                def popen(*args, **kwargs):
                    supervisor.process = subprocess.Popen(*args, **kwargs)
                    return supervisor.process
                reporter = Mock()
                runner = CommandRunnerProvider(config, supervisor, popen=popen, transport_diagnostics=reporter)
                runner.prepare()
                workspace = WorkspaceLease(Path(root)/'task', config.projects['homelab'],
                    Path(root)/'git', value['task_id'], value['base_revision'])
                result = runner.run(value, workspace, threading.Event())
                if valid:
                    self.assertEqual((result.status, result.text, result.conversation_id),
                        ('awaiting_review', 'Fixture report', session))
                    self.assertEqual(result.usage, {'input_tokens': 3, 'output_tokens': 2})
                    reporter.report.assert_not_called()
                else:
                    self.assertEqual(result.status, 'failed')
                    self.assertEqual(result.text, '')
                    reporter.report.assert_called_once()
                    self.assertEqual(reporter.report.call_args.args[0], value)
                    self.assertEqual(reporter.report.call_args.args[1]['path'], '$.thread_id')
                self.assertTrue(result.stop_proof['scope_stopped'])


if __name__ == '__main__':
    unittest.main()
