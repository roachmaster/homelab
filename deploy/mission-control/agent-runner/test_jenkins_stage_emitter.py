"""Stage-event emitter contract: the stdlib-only verifier's payload must decode
through the runner's own jenkins_api.decode, byte-identically canonical, with
a delivery id both sides construct the same way. No network, no DB."""
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
CI_SPEC = importlib.util.spec_from_file_location('forge_ci_supervision', HERE.parent / 'jenkins/verify-candidate.py')
CI = importlib.util.module_from_spec(CI_SPEC)
sys.modules[CI_SPEC.name] = CI
CI_SPEC.loader.exec_module(CI)
SETUP_SPEC = importlib.util.spec_from_file_location('forge_ci_provision', HERE.parent / 'jenkins/setup-ci.py')
SETUP = importlib.util.module_from_spec(SETUP_SPEC)
sys.modules[SETUP_SPEC.name] = SETUP
SETUP_SPEC.loader.exec_module(SETUP)

import jenkins_api
from jenkins_events import canonical_json_bytes
from work_jenkins_lifecycle import stage_delivery_id as lifecycle_stage_delivery_id

TOKEN = 'a' * 64
ACTION_ID = '11111111-1111-4111-8111-111111111111'
COMMIT = 'b' * 40


class FakeResponse:
    def __init__(self, status, body):
        self.status = status
        self._body = body

    def read(self, _limit=None):
        return self._body


class FakeConnection:
    requests = []

    def __init__(self, host, port, timeout=None):
        self.host, self.port, self.timeout = host, port, timeout

    def request(self, method, url, body=None, headers=None):
        FakeConnection.requests.append({'method': method, 'url': url, 'body': body,
                                        'headers': dict(headers or {})})

    def getresponse(self):
        request = FakeConnection.requests[-1]
        correlation = request['headers']['Forge-Request-Id']
        return FakeResponse(202, json.dumps({
            'protocol_version': 1,
            'event': {'kind': 'jenkins_event_emitted', 'request_id': correlation,
                      'event_id': 'e' * 36, 'deduplicated': False}}).encode('utf-8'))

    def close(self):
        pass


class FailingConnection(FakeConnection):
    def getresponse(self):
        raise OSError('connection refused')


def emitter(log=None, connection_factory=FakeConnection, token_paths=None, **overrides):
    params = dict(action_id=ACTION_ID, project_alias='homelab', commit_sha=COMMIT,
                  jenkins_instance='ci-primary', build_number=7,
                  job_name='forge-native-candidate',
                  logical_names=('source-preflight', 'check', 'in-memory-backend'),
                  token_paths=token_paths or [], connection_factory=connection_factory,
                  log=log if log is not None else (lambda _line: None))
    params.update(overrides)
    return CI.StageEmitter(**params)


def token_file(directory):
    path = Path(directory) / 'runner.token'
    path.write_text(TOKEN + '\n')
    return path


class EmitterContractTest(unittest.TestCase):
    def setUp(self):
        FakeConnection.requests = []

    def test_posted_payload_decodes_through_runner_api(self):
        with tempfile.TemporaryDirectory() as temporary:
            logs = []
            stage = emitter(log=logs.append, token_paths=[token_file(temporary)])
            self.assertTrue(stage.emit_started(0))
        self.assertEqual(len(FakeConnection.requests), 1)
        request = FakeConnection.requests[0]
        self.assertEqual(request['method'], 'POST')
        self.assertEqual(request['url'], '/v1/dispatch')
        headers = request['headers']
        self.assertEqual(headers['Forge-Destination'], 'ci.jenkins.event.emit')
        self.assertEqual(headers['Forge-Envelope-Version'], '1')
        self.assertEqual(headers['Forge-Contract-Version'], '1')
        self.assertEqual(headers['Authorization'], 'Bearer ' + TOKEN)
        body = request['body']
        self.assertIsInstance(body, bytes)
        parsed = json.loads(body)
        # Canonical JSON, byte-identical to the agent-runner construction.
        self.assertEqual(canonical_json_bytes(parsed), body)
        command = jenkins_api.decode(jenkins_api.Destination.EMIT,
                                     headers['Forge-Request-Id'], parsed)
        self.assertEqual(command.event_kind, 'stage_started')
        self.assertEqual(command.stage_name, 'source-preflight')
        self.assertIsNone(command.stage_status)
        self.assertEqual(command.build_number, 7)
        self.assertEqual(command.job_name, 'forge-native-candidate')
        self.assertEqual(command.commit_sha, COMMIT)
        self.assertEqual(command.jenkins_instance, 'ci-primary')
        # The delivery id is the same digest both sides construct.
        expected = lifecycle_stage_delivery_id(ACTION_ID, 'source-preflight', 'stage_started')
        self.assertEqual(command.delivery_id, expected)
        self.assertEqual(command.delivery_id, CI.stage_delivery_id(ACTION_ID, 'source-preflight', 'stage_started'))

    def test_completed_payload_carries_stage_status(self):
        with tempfile.TemporaryDirectory() as temporary:
            stage = emitter(token_paths=[token_file(temporary)])
            self.assertTrue(stage.emit_completed(1, False))
        parsed = json.loads(FakeConnection.requests[0]['body'])
        self.assertEqual(parsed['event_kind'], 'stage_completed')
        self.assertEqual(parsed['stage_name'], 'check')
        self.assertEqual(parsed['stage_status'], 'failure')
        command = jenkins_api.decode(jenkins_api.Destination.EMIT,
                                     FakeConnection.requests[0]['headers']['Forge-Request-Id'], parsed)
        self.assertEqual(command.stage_status, 'failure')

    def test_out_of_range_index_emits_nothing(self):
        stage = emitter()
        stage.emit_started(3)
        stage.emit_completed(-1, True)
        self.assertEqual(FakeConnection.requests, [])


class EmitterFailureTest(unittest.TestCase):
    def setUp(self):
        FakeConnection.requests = []

    def test_missing_token_logs_and_never_raises(self):
        logs = []
        stage = emitter(log=logs.append, token_paths=[])
        self.assertFalse(stage.emit_started(0))
        self.assertEqual(FakeConnection.requests, [])
        self.assertTrue(any('token_unavailable' in line for line in logs))
        self.assertTrue(all(TOKEN not in line for line in logs))

    def test_runner_unavailable_retries_then_gives_up(self):
        with tempfile.TemporaryDirectory() as temporary:
            logs = []
            stage = emitter(log=logs.append, token_paths=[token_file(temporary)],
                            connection_factory=FailingConnection)
            self.assertFalse(stage.emit_started(0))
        self.assertEqual(len(FakeConnection.requests), 2)
        self.assertTrue(any('runner_unavailable' in line for line in logs))
        self.assertTrue(all(TOKEN not in line for line in logs))

    def test_wrong_response_kind_is_rejected(self):
        class WrongKind(FakeConnection):
            def getresponse(self):
                return FakeResponse(202, json.dumps({
                    'protocol_version': 1,
                    'event': {'kind': 'something_else', 'request_id': 'x'}}).encode('utf-8'))
        with tempfile.TemporaryDirectory() as temporary:
            logs = []
            stage = emitter(log=logs.append, token_paths=[token_file(temporary)],
                            connection_factory=WrongKind)
            self.assertFalse(stage.emit_started(0))
        self.assertTrue(any('invalid_runner_response' in line for line in logs))

    def test_total_budget_stops_emission(self):
        with tempfile.TemporaryDirectory() as temporary:
            logs = []
            stage = emitter(log=logs.append, token_paths=[token_file(temporary)],
                            total_budget_seconds=0)
            self.assertFalse(stage.emit_started(0))
        self.assertEqual(FakeConnection.requests, [])


class GateIdentityTest(unittest.TestCase):
    def build(self, env, config=None):
        action = ACTION_ID
        request = {'branch_scope': 'branch', 'project_alias': 'homelab',
                   'candidate_commit': COMMIT}
        merged = {'verification_profile': 'forge-sdlc-jenkins-v2'}
        merged.update(config or {})
        with patch.dict(os.environ, env, clear=True):
            return CI.build_stage_emitter(action, request, merged)

    def test_valid_identity_builds_emitter(self):
        stage, reason = self.build({'BUILD_NUMBER': '7', 'JOB_NAME': 'forge-native-candidate'})
        self.assertIsNone(reason)
        self.assertIsNotNone(stage)
        self.assertEqual(stage._build_number, 7)
        self.assertEqual(stage._logical_names,
                         ('source-preflight', 'check', 'in-memory-backend'))

    def test_main_scope_selects_main_logical_names(self):
        with patch.dict(os.environ, {'BUILD_NUMBER': '7', 'JOB_NAME': 'forge-native-candidate'}, clear=True):
            stage, reason = CI.build_stage_emitter(
                ACTION_ID,
                {'branch_scope': 'main', 'project_alias': 'homelab', 'candidate_commit': COMMIT},
                {'verification_profile': 'forge-sdlc-jenkins-v2'})
        self.assertIsNone(reason)
        self.assertEqual(stage._logical_names, ('check', 'real-browser', 'backend-suites'))

    def test_invalid_build_numbers_disable_emission(self):
        for raw in (None, '', '0', '-1', 'abc', '7.5', '2147483648', '007'):
            env = {'JOB_NAME': 'forge-native-candidate'}
            if raw is not None:
                env['BUILD_NUMBER'] = raw
            with self.subTest(raw=raw):
                stage, reason = self.build(env)
                self.assertIsNone(stage)
                self.assertEqual(reason, 'invalid BUILD_NUMBER')

    def test_wrong_job_disables_emission(self):
        for job in (None, '', 'forge-exact-candidate', 'other/job'):
            env = {'BUILD_NUMBER': '7'}
            if job is not None:
                env['JOB_NAME'] = job
            with self.subTest(job=job):
                stage, reason = self.build(env)
                self.assertIsNone(stage)
                self.assertEqual(reason, 'unexpected JOB_NAME')

    def test_invalid_instance_disables_emission(self):
        stage, reason = self.build(
            {'BUILD_NUMBER': '7', 'JOB_NAME': 'forge-native-candidate'},
            {'jenkins_instance': 'bad instance!'})
        self.assertIsNone(stage)
        self.assertEqual(reason, 'invalid jenkins_instance')


class SetupCiStageTest(unittest.TestCase):
    def render_args(self):
        return dict(operator='fixture', spool=Path('/tmp/spool'), state_root=Path('/tmp/state'),
                    toolchain=Path('/opt/forge-ci/toolchain'), registry=Path('/opt/forge-ci/registry'),
                    modules=Path('/opt/forge-ci/modules'), verifier_sha256='a' * 64)

    def test_pipeline_config_carries_jenkins_instance(self):
        configured = SETUP.render_pipeline_config(**self.render_args())
        self.assertEqual(configured['jenkins_instance'], 'ci-primary')
        configured = SETUP.render_pipeline_config(**self.render_args(), jenkins_instance='ci-two')
        self.assertEqual(configured['jenkins_instance'], 'ci-two')
        for invalid in ('', 'bad instance', 'x' * 65, None, 7):
            with self.subTest(value=invalid), self.assertRaises(ValueError):
                SETUP.render_pipeline_config(**self.render_args(), jenkins_instance=invalid)

    def test_sudoers_rule_preserves_only_build_identity_for_the_wrapper(self):
        rule = SETUP.NATIVE_SUDOERS_RULE.decode('ascii')
        self.assertIn('Defaults!/usr/local/libexec/forge-native-ci-check '
                      'env_keep += "BUILD_NUMBER JOB_NAME"', rule)
        self.assertIn('jenkins ALL=(root) NOPASSWD: /usr/local/libexec/forge-native-ci-check *', rule)
        # No other variable survives: the env_keep line names exactly two.
        env_line = [line for line in rule.splitlines() if 'env_keep' in line]
        self.assertEqual(len(env_line), 1)
        self.assertNotIn('PATH', env_line[0])
        self.assertNotIn('HOME', env_line[0])

    def test_token_requalification_prefers_systemd_credential(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            systemd = root / 'systemd.token'
            copy = root / 'copy.token'
            systemd.write_text(TOKEN)
            copy.write_text('b' * 64)
            self.assertEqual(SETUP.qualify_stage_token(systemd, copy), 'systemd-credential')
            systemd.unlink()
            self.assertEqual(SETUP.qualify_stage_token(systemd, copy), 'provisioned-copy')
            copy.write_text('not-a-token')
            with self.assertRaises(ValueError):
                SETUP.qualify_stage_token(systemd, copy)
            copy.unlink()
            with self.assertRaises(ValueError):
                SETUP.qualify_stage_token(systemd, copy)


if __name__ == '__main__':
    unittest.main()
