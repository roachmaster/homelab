"""Owned build evidence, cross-task denial, and the real read-only inspection CLI."""
from contextlib import contextmanager
from dataclasses import replace
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import Mock
from uuid import uuid4

from test_work_review_context import fixture as review_fixture
from work_build_context import (BUILD_INFO_SCRIPT, BuildContext, BuildContextProvider,
                                canonical, read_regular)
from work_executor import CommandRunnerProvider, ExecutionError, SystemdSupervisor
from work_review_context import ReviewContext, validate_context
from work_worker_config import parse_config, validated_copy
from test_work_executor import raw_config


@contextmanager
def fixture():
    with TemporaryDirectory(prefix='forge-build-context-fixture-') as directory:
        root = Path(directory)
        state, results = root / 'state', root / 'results'
        action, workflow, review, task, attempt, fid = [str(uuid4()) for _ in range(6)]
        source = {'parent_attempt_id': attempt, 'candidate_commit': 'c' * 40,
                  'gate_manifest_sha256': 'd' * 64}
        content = canonical(source)
        feedback = ReviewContext(content, hashlib.sha256(content).hexdigest(), fid, ())
        build = {'provider': 'jenkins', 'job': 'forge-native-candidate',
                 'action_id': action, 'queue_id': 31, 'build_number': 7}
        failure = {'version': 1, 'tested_candidate': 'a' * 40, 'current_main': 'b' * 40,
                   'build': build, 'conflict_paths': ['app.py']}
        envelope = {'feedback_id': fid, 'feedback_sha256': feedback.sha256,
            'review_id': review, 'source_task_id': task, 'source_attempt_id': attempt,
            'candidate_commit': source['candidate_commit'],
            'gate_manifest_sha256': source['gate_manifest_sha256'], 'delivery_failure': failure}
        manifest = {'review_id': review, 'task_id': task, 'attempt_id': attempt,
            'candidate_commit': source['candidate_commit'],
            'gate_manifest_sha256': source['gate_manifest_sha256'], 'delivery_failure': failure}
        request = {'action_id': action, 'workflow_id': workflow, 'candidate_commit': 'a' * 40,
            'candidate_tree': 'e' * 40, 'integration_base': 'f' * 40,
            'gate_manifest_sha256': '1' * 64, 'correlation_id': str(uuid4())}
        secret = 'ghp_' + 'A' * 36
        log = ('first line\n' + secret + '\n' + 'last line\n' * 2000).encode()
        result = {k: request[k] for k in ('action_id', 'workflow_id', 'candidate_commit',
                  'candidate_tree', 'gate_manifest_sha256', 'correlation_id')}
        result.update(expected_main=request['integration_base'], status='failed',
                      log_sha256=hashlib.sha256(log).hexdigest())
        raw = canonical(result)
        receipt = {**build, 'check_receipt_sha256': hashlib.sha256(raw).hexdigest(),
                   'log_sha256': result['log_sha256']}
        action_root = state / 'publication/native-ci/actions' / action
        def write(path, data):
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
            path.chmod(0o600)
        write(state / 'publication' / review / 'review.json', canonical(manifest))
        write(action_root / 'native-binding.json', canonical({'request': request}))
        write(action_root / 'observation.json', canonical({'status': 'failed', 'receipt': receipt}))
        write(results / (action + '.json'), raw)
        write(results / (action + '.log'), log)
        store = Mock()
        store.read_native_delivery_context.return_value = envelope
        reads = []
        def owned_fixture_reader(path, *, owner, maximum=65536):
            # Substitute only the root-owned artifact authority for owned fixtures.
            # The production provider still requests owner0 for both CI artifacts.
            reads.append((path, owner))
            return read_regular(path, owner=os.geteuid(), maximum=maximum)
        provider = BuildContextProvider(store, state, results, reader=owned_fixture_reader)
        lease = {'workflow_id': workflow, 'task_id': str(uuid4())}
        yield root, provider, store, lease, feedback, envelope, results, action, reads


class BuildContextTest(unittest.TestCase):
    def test_source_bound_full_log_is_redacted_and_cli_pages_without_credentials(self):
        with fixture() as (root, provider, _store, lease, feedback, _envelope, results, action, reads):
            context = provider.acquire(lease, feedback)
            self.assertNotIn(b'ghp_', context.log)
            self.assertIn(b'[redacted]', context.log)
            metadata = json.loads(context.metadata)
            self.assertEqual(metadata['current_main'], 'b' * 40)
            self.assertEqual(metadata['expected_main_at_build'], 'f' * 40)
            self.assertEqual([(path.name, owner) for path, owner in reads if path.parent == results],
                             [(action + '.json', 0), (action + '.log', 0)])
            for name, data in [('build_info.py', BUILD_INFO_SCRIPT),
                               ('build-info.json', context.metadata), ('build.log', context.log)]:
                (root / name).write_bytes(data)
            def cli(*args):
                return subprocess.run([sys.executable, '-I', '-B', str(root / 'build_info.py'), *args],
                    capture_output=True, check=True, timeout=5)
            self.assertEqual(json.loads(cli('status').stdout)['build']['build_number'], 7)
            first = json.loads(cli('logs', '--limit', '23').stdout)
            self.assertEqual(first['next_offset'], 23)
            second = json.loads(cli('logs', '--offset', '23', '--limit', '65536').stdout)
            self.assertTrue(second['eof'])
            self.assertEqual((first['text'] + second['text']).encode(), context.log)
            (root / 'build.log').write_bytes(b'changed')
            with self.assertRaises(subprocess.CalledProcessError): cli('logs')

    def test_other_feedback_workflow_build_and_tampered_root_log_are_denied(self):
        for changed in ('feedback', 'workflow', 'build', 'root-log', 'manifest'):
            with self.subTest(changed=changed), fixture() as (_root, provider, store, lease, feedback, envelope, results, action, _reads):
                if changed == 'feedback': envelope['feedback_id'] = str(uuid4())
                elif changed == 'workflow': lease['workflow_id'] = str(uuid4())
                elif changed == 'build': envelope['delivery_failure']['build']['build_number'] += 1
                elif changed == 'root-log': (results / (action + '.log')).write_bytes(b'forged CI log')
                else: envelope['candidate_commit'] = '9' * 40
                with self.assertRaises(ValueError): provider.acquire(lease, feedback)

    def test_non_delivery_feedback_needs_no_artifact_access(self):
        provider = BuildContextProvider(Mock(), '/not-opened/state', '/not-opened/results',
                                        reader=Mock(side_effect=AssertionError('must not open')))
        provider.store.read_native_delivery_context.return_value = None
        self.assertIsNone(provider.acquire({}, None))

    def test_reader_rejects_links_and_wrong_ownership(self):
        with TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / 'artifact'
            path.write_bytes(b'owned')
            path.chmod(0o600)
            self.assertEqual(read_regular(path, owner=os.geteuid()), b'owned')
            with self.assertRaises(ValueError): read_regular(path, owner=os.geteuid() + 1)
            link = root / 'link'; link.symlink_to(path)
            with self.assertRaises(OSError): read_regular(link, owner=os.geteuid())
            os.link(path, root / 'hardlink')
            with self.assertRaises(ValueError): read_regular(path, owner=os.geteuid())

    def test_invocation_stages_all_cli_files_readonly_and_rejects_foreign_context(self):
        for foreign in (False, True):
            with self.subTest(foreign=foreign), TemporaryDirectory() as directory:
                config, lease, workspace, envelope = review_fixture(directory)
                ready = validate_context(lease, workspace, envelope)
                log = b'Jenkins fixture diagnostic\n'
                metadata = {'feedback_id': ready.feedback_id, 'feedback_sha256': ready.sha256,
                            'sanitized_log_sha256': hashlib.sha256(log).hexdigest()}
                build = BuildContext(canonical(metadata), log,
                    str(uuid4()) if foreign else ready.feedback_id, ready.sha256)
                ready = replace(ready, build=build)
                runner = CommandRunnerProvider(config, SystemdSupervisor(config)); runner.prepare()
                if foreign:
                    with self.assertRaises(ExecutionError): runner.invocation(lease, workspace, feedback_context=ready)
                    continue
                invocation = runner.invocation(lease, workspace, feedback_context=ready)
                files = {p.name: p for p in invocation.readonly_files}
                self.assertTrue({'build_info.py', 'build-info.json', 'build.log'} <= files.keys())
                self.assertEqual(files['build.log'].read_bytes(), log)
                argv = runner.supervisor.argv(lease, invocation)
                readonly = next(arg for arg in argv if arg.startswith('--property=BindReadOnlyPaths='))
                self.assertIn(str(files['build_info.py']), readonly)
                self.assertIn(b'build_info.py', invocation.stdin)
                self.assertNotIn(log, invocation.stdin)

    def test_worker_build_root_is_explicit_and_survives_validation(self):
        with TemporaryDirectory() as directory:
            raw = raw_config(directory)
            self.assertIsNone(parse_config(raw).build_results_root)
            raw['build_results_root'] = str(Path(directory) / 'results')
            self.assertEqual(validated_copy(parse_config(raw)).build_results_root, Path(raw['build_results_root']))
            raw['build_results_root'] = '../escape'
            with self.assertRaises(ValueError): parse_config(raw)


if __name__ == '__main__':
    unittest.main()
