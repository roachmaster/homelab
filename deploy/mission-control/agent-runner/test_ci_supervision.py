"""Privilege-boundary and profile-compatibility checks; no privileged effects."""
import importlib.util
import hashlib
import json
import stat
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import Mock, patch
from uuid import uuid4

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location('forge_ci_supervision', HERE.parent / 'jenkins/verify-candidate.py')
CI = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CI)
SETUP_SPEC = importlib.util.spec_from_file_location('forge_ci_provision', HERE.parent / 'jenkins/setup-ci.py')
SETUP = importlib.util.module_from_spec(SETUP_SPEC)
SETUP_SPEC.loader.exec_module(SETUP)


class SupervisionBoundaryTest(unittest.TestCase):
    def test_input_reader_rejects_link_fifo_writable_and_hardlinked_inputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            good = root / 'good'; good.write_bytes(b'approved'); good.chmod(0o600)
            self.assertEqual(CI.read_file(good, owner=os.getuid(), maximum=64), b'approved')
            link = root / 'link'; link.symlink_to(good)
            with self.assertRaises(OSError): CI.read_file(link, owner=os.getuid(), maximum=64)
            fifo = root / 'fifo'; os.mkfifo(fifo)
            with self.assertRaises(ValueError): CI.read_file(fifo, owner=os.getuid(), maximum=64)
            good.chmod(0o666)
            with self.assertRaises(ValueError): CI.read_file(good, owner=os.getuid(), maximum=64)
            good.chmod(0o600); os.link(good, root / 'hardlink')
            with self.assertRaises(ValueError): CI.read_file(good, owner=os.getuid(), maximum=64)

    def test_process_cleanup_only_accepts_an_actual_negative_observation(self):
        account = SimpleNamespace(pw_uid=43210)
        with patch.object(CI.subprocess, 'run', side_effect=[SimpleNamespace(returncode=1), SimpleNamespace(returncode=1)]):
            CI.kill_build_user(account)
        for results in [[2], [0, 2]]:
            with patch.object(CI.subprocess, 'run', side_effect=[SimpleNamespace(returncode=x) for x in results]):
                with self.assertRaises(RuntimeError): CI.kill_build_user(account)

    def test_bounded_observation_enforces_limit_and_exit_status(self):
        env = {'PATH': '/usr/bin:/bin'}
        self.assertEqual(CI.bounded_output(['/usr/bin/python3', '-c', 'print("ok")'], env=env), b'ok\n')
        with self.assertRaises(ValueError):
            CI.bounded_output(['/usr/bin/python3', '-c', 'print("x"*100000)'], env=env, maximum=128)
        with self.assertRaises(ValueError):
            CI.bounded_output(['/usr/bin/python3', '-c', 'raise SystemExit(3)'], env=env)

    def test_operator_and_privileged_gate_predicates_use_identical_source_inputs(self):
        from work_external_delivery import protected_delivery_gate, canonical
        candidates = ['Makefile', 'Cargo.lock', 'src/rust/scripts/ui_tools.py',
            'src/rust/forge-ide-ui/tests/history.rs', 'deploy/mission-control/agent-runner/test_trace.py',
            'deploy/mission-control/agent-runner/work_external_delivery.py',
            'deploy/mission-control/jenkins/setup-ci.py', 'src/rust/lib.rs',
            'docs/évent.md', 'tests/事件.rs', '.github/workflows/test.yml']
        for path in candidates:
            self.assertEqual(CI.protected_gate_path(path), protected_delivery_gate(path), path)
        value = {'tests/事件.rs': ['100644', 'a' * 40], 'checks': CI.CHECKS}
        self.assertEqual(CI.canonical(value), canonical(value))

    def test_non_head_delivery_bundle_imports_exact_objects_before_any_checkout(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); original = root / 'original'; source = root / 'source'
            env = {'PATH': '/usr/bin:/bin', 'HOME': str(root), 'GIT_CONFIG_NOSYSTEM': '1',
                'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_NO_REPLACE_OBJECTS': '1',
                'GIT_AUTHOR_NAME': 'Owned fixture', 'GIT_COMMITTER_NAME': 'Owned fixture',
                'GIT_AUTHOR_EMAIL': 'fixture@example.invalid', 'GIT_COMMITTER_EMAIL': 'fixture@example.invalid'}
            def git(*args):
                return subprocess.check_output(['/usr/bin/git', '-c', 'core.hooksPath=/dev/null', *args], env=env, stderr=subprocess.DEVNULL)
            git('init', '--quiet', str(original))
            (original / 'source.txt').write_text('Exact immutable input')
            git('-C', str(original), 'add', 'source.txt')
            git('-C', str(original), 'commit', '--quiet', '-m', 'Owned bundle fixture')
            commit = git('-C', str(original), 'rev-parse', 'HEAD').strip()
            tree = git('-C', str(original), 'rev-parse', 'HEAD^{tree}').strip()
            ref = 'refs/forge/external-candidates/' + str(uuid4())
            git('-C', str(original), 'update-ref', ref, commit.decode())
            bundle = root / 'candidate.bundle'
            git('-C', str(original), 'bundle', 'create', str(bundle), ref)
            self.assertNotIn(b'refs/heads/', git('bundle', 'list-heads', str(bundle)))
            CI.import_verified_bundle(bundle, source, env)
            self.assertEqual(git('-C', str(source), 'rev-parse', commit.decode() + '^{tree}').strip(), tree)
            self.assertEqual([p.name for p in source.iterdir()], ['.git'])
            self.assertFalse((source / '.git/FETCH_HEAD').exists())
            imported = git('-C', str(source), 'for-each-ref', '--format=%(refname)').strip()
            self.assertTrue(imported.startswith(b'refs/forge-ci-import/forge/external-candidates/'))

    def test_restrictive_service_umask_keeps_explicit_traversal_and_receipt_readability(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prior = os.umask(0o077)
            try:
                job = root / 'job'
                CI.create_job_directory(job)
                self.assertEqual(stat.S_IMODE(job.stat().st_mode), 0o711)
                with patch.object(CI.os, 'fchown'):
                    fd = CI.open_log(root / 'run.log', os.getgid())
                    try:
                        self.assertEqual(stat.S_IMODE(os.fstat(fd).st_mode), 0o640)
                    finally:
                        os.close(fd)
                    CI.write_result(root / 'result.json', {'status': 'passed'}, os.getgid())
                self.assertEqual(stat.S_IMODE((root / 'result.json').stat().st_mode), 0o640)
                self.assertEqual(json.loads((root / 'result.json').read_text()), {'status': 'passed'})
                self.assertFalse((root / 'result.new').exists())
            finally:
                os.umask(prior)

    def test_remoting_executes_only_after_dropping_groups_uid_and_privilege_gain(self):
        account = SimpleNamespace(pw_name='forge_ci', pw_uid=43210, pw_gid=43211)
        calls = []
        library = SimpleNamespace(prctl=Mock(return_value=0))
        with patch.object(SETUP.os, 'setgroups', side_effect=lambda groups: calls.append(('groups', groups))), \
             patch.object(SETUP.os, 'setgid', side_effect=lambda gid: calls.append(('gid', gid))), \
             patch.object(SETUP.os, 'setuid', side_effect=lambda uid: calls.append(('uid', uid))), \
             patch.object(SETUP.os, 'umask'), patch.object(SETUP.ctypes, 'CDLL', return_value=library):
            SETUP.remoting_identity(account)()
        self.assertEqual(calls, [('groups', []), ('gid', 43211), ('uid', 43210)])
        library.prctl.assert_called_once_with(38, 1, 0, 0, 0)
        with self.assertRaises(ValueError):
            SETUP.remoting_identity(SimpleNamespace(pw_name='forge_ci', pw_uid=0))
        with patch.object(SETUP.os, 'setgroups'), patch.object(SETUP.os, 'setgid'), \
             patch.object(SETUP.os, 'setuid'), \
             patch.object(SETUP.ctypes, 'CDLL', return_value=SimpleNamespace(prctl=Mock(return_value=-1))):
            with self.assertRaises(OSError):
                SETUP.remoting_identity(account)()

    def test_remoting_help_uses_verified_root_copy_nonroot_preexec_and_clean_environment(self):
        with tempfile.TemporaryDirectory() as temporary:
            jar = Path(temporary) / 'agent.jar'
            jar.write_bytes(b'owned fixture jar')
            jar.chmod(0o644)
            account = SimpleNamespace(pw_name='forge_ci', pw_uid=43210, pw_gid=43211)
            original_fstat = os.fstat
            def root_file(fd):
                info = original_fstat(fd)
                return SimpleNamespace(st_mode=info.st_mode, st_uid=0,
                    st_nlink=info.st_nlink, st_size=info.st_size)
            output = SimpleNamespace(stdout='-webSocket -secret -workDir', stderr='')
            with patch.object(SETUP.os, 'fstat', side_effect=root_file), \
                 patch.object(SETUP.subprocess, 'run', return_value=output) as run:
                SETUP.qualify_remoting(jar, hashlib.sha256(jar.read_bytes()).hexdigest(), account)
                argv = run.call_args.args[0]
                options = run.call_args.kwargs
                self.assertEqual(argv, ['/usr/bin/java', '-jar', str(jar), '-help'])
                self.assertTrue(callable(options['preexec_fn']))
                self.assertEqual(options['env'], {'PATH': '/usr/bin:/bin', 'HOME': '/var/lib/forge-ci', 'LANG': 'C.UTF-8'})
                self.assertEqual(options['cwd'], jar.parent)
                self.assertTrue(options['close_fds'])
                self.assertEqual(options['timeout'], 15)
                run.reset_mock()
                with self.assertRaises(ValueError):
                    SETUP.qualify_remoting(jar, '0' * 64, account)
                run.assert_not_called()
            with patch.object(SETUP.subprocess, 'run') as run:
                jar.chmod(0o666)
                with self.assertRaises(ValueError):
                    SETUP.qualify_remoting(jar, hashlib.sha256(jar.read_bytes()).hexdigest(), account)
                run.assert_not_called()

    def test_supervised_cancellation_writes_failed_receipt_after_confirmed_uid_cleanup(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            action = str(uuid4())
            spool, results, work = (root / name for name in ('spool', 'results', 'work'))
            for path in (spool, results, work): path.mkdir()
            directory = spool / action; directory.mkdir()
            data = b'owned bundle fixture'
            (directory / 'candidate.bundle').write_bytes(data)
            (directory / 'candidate.bundle').chmod(0o600)
            helper_digest = hashlib.sha256(Path(CI.__file__).read_bytes()).hexdigest()
            protected = {'Makefile': ['100644', 'a' * 40]}
            manifest = hashlib.sha256(CI.canonical({'version': 1, 'provider': 'jenkins',
                'profile': 'fixture', 'checks': CI.CHECKS, 'verifier_sha256': helper_digest,
                'source_gate_sha256': hashlib.sha256(CI.canonical(protected)).hexdigest()})).hexdigest()
            request = {'version': 1, 'action_id': action, 'workflow_id': str(uuid4()),
                'project_alias': 'homelab', 'candidate_commit': 'b' * 40,
                'candidate_tree': 'c' * 40, 'expected_main': 'd' * 40,
                'gate_manifest_sha256': manifest, 'bundle_sha256': hashlib.sha256(data).hexdigest(),
                'verification_profile': 'fixture', 'correlation_id': str(uuid4())}
            (directory / 'request.json').write_text(json.dumps(request))
            (directory / 'request.json').chmod(0o600)
            config = {'build_user': 'forge_ci', 'submitter_user': 'fixture',
                'spool_root': str(spool), 'results_root': str(results), 'work_root': str(work),
                'projects': ['homelab'], 'verification_profile': 'fixture', 'verifier_sha256': helper_digest,
                'toolchain': str(root / 'toolchain'), 'cargo_registry': str(root / 'registry'),
                'node_modules': str(root / 'modules'), 'timeout_seconds': 60}
            build = SimpleNamespace(pw_uid=43210, pw_gid=43211, pw_name='forge_ci')
            submitter = SimpleNamespace(pw_uid=os.getuid(), pw_gid=os.getgid())
            source = work / action / 'source'
            def git(command, **_options):
                if 'init' in command:
                    (source / 'src/rust/forge-ide-web/tests/browser').mkdir(parents=True)
                    (source / 'Makefile').write_text('fixture')
                return SimpleNamespace(returncode=0)
            order = []
            def receipt(_path, result, _group):
                order.append(('receipt', result))
            with patch.object(CI.pwd, 'getpwnam', side_effect=lambda name: build if name == 'forge_ci' else submitter), \
                 patch.object(CI.grp, 'getgrgid', return_value=SimpleNamespace(gr_name='forge_ci')), \
                 patch.object(CI, 'regular_directory'), patch.object(CI.os, 'chown'), \
                 patch.object(CI.os, 'fchown'), patch.object(CI.subprocess, 'run', side_effect=git), \
                 patch.object(CI, 'bounded_output', side_effect=[('c' * 40).encode(), b'100644 blob ' + b'a' * 40 + b' 7\tMakefile\0']), \
                 patch.object(CI, 'kill_build_user', side_effect=lambda _account: order.append(('cleanup', None))), \
                 patch.object(CI, 'run_child', side_effect=InterruptedError('owned cancellation fixture')), \
                 patch.object(CI, 'write_result', side_effect=receipt):
                result = CI.execute_locked(action, config)
            self.assertEqual([item[0] for item in order], ['cleanup', 'cleanup', 'receipt'])
            self.assertEqual(result['status'], 'failed')
            self.assertEqual(result['failure'], 'cancelled_or_timed_out')
            self.assertTrue(result['cleanup_confirmed'])
            self.assertEqual(result['checks'], [])
            self.assertEqual(order[-1][1], result)

    def test_provisioner_requires_explicit_execution_before_reading_configuration(self):
        result = subprocess.run([sys.executable, '-B', str(HERE.parent / 'jenkins/setup-ci.py'),
            '--client-config', '/nonexistent/never-opened'], capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 2)
        self.assertIn('Explicit --execute', result.stderr)


if __name__ == '__main__': unittest.main()
