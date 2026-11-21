"""Privilege-boundary, profile-compatibility, and Cargo cache supervision checks."""
import importlib.util
import hashlib
import json
import stat
import socket
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from types import SimpleNamespace
from unittest.mock import Mock, patch
from uuid import uuid4

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location('forge_ci_supervision', HERE.parent / 'jenkins/verify-candidate.py')
CI = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = CI
SPEC.loader.exec_module(CI)
SETUP_SPEC = importlib.util.spec_from_file_location('forge_ci_provision', HERE.parent / 'jenkins/setup-ci.py')
SETUP = importlib.util.module_from_spec(SETUP_SPEC)
sys.modules[SETUP_SPEC.name] = SETUP
SETUP_SPEC.loader.exec_module(SETUP)
CLEAR_SPEC = importlib.util.spec_from_file_location('forge_ci_clear', HERE.parent / 'jenkins/clear-ci-cache.py')
CLEAR = importlib.util.module_from_spec(CLEAR_SPEC)
sys.modules[CLEAR_SPEC.name] = CLEAR
CLEAR_SPEC.loader.exec_module(CLEAR)

TOOLCHAIN = Path('/opt/forge-ci/toolchain')
TOOLCHAIN_AVAILABLE = (TOOLCHAIN / 'bin/cargo').is_file() and (TOOLCHAIN / 'bin/rustc').is_file()


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

    def test_branch_helpers_match_across_supervisor_delivery_and_workflow(self):
        import work_external_delivery as delivery
        import work_external_workflow as workflow
        modules = (CI, delivery, workflow)
        for name in ('main', 'feat/cargo-cache', 'feat/a-b', 'feat-a/b', 'release_1.2.3'):
            slugs = [m.canonical_branch_slug(name) for m in modules]
            self.assertEqual(len(set(slugs)), 1, name)
        self.assertNotEqual(CI.canonical_branch_slug('feat/a-b'), CI.canonical_branch_slug('feat-a/b'))
        for bad in ('', '../main', '/abs', 'feat//x', 'feat/.dot', 'feat/a.lock', 'feat@{1}', 'bad space'):
            for m in modules:
                with self.assertRaises(ValueError):
                    m.validate_branch_name(bad)
        for m in modules:
            self.assertEqual(m.derive_branch_scope(candidate_commit='a' * 40, expected_main='a' * 40, source_branch='main'), 'main')
            self.assertEqual(m.derive_branch_scope(candidate_commit='b' * 40, expected_main='a' * 40, source_branch=None), 'detached')
            with self.assertRaises(ValueError):
                m.derive_branch_scope(candidate_commit='b' * 40, expected_main='a' * 40, source_branch='main')
            for bad_scope in ('candidates/main', 'workflow-123', '../main', 'branch/BadUpper', ''):
                with self.assertRaises(ValueError):
                    m.validate_branch_scope(bad_scope, candidate_commit='b' * 40, expected_main='a' * 40)

    def test_cargo_cache_config_and_factory_bootstrap_validation(self):
        disabled = CI.CargoCacheConfig.parse(None)
        self.assertFalse(disabled.enabled)
        self.assertIsInstance(CI.CargoCacheFactory(disabled).create(), CI.NullCargoCacheProvider)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            cache_dir = root / 'ci-cache'
            cfg = CI.CargoCacheConfig.parse(
                {'enabled': True, 'root': str(cache_dir), 'max_total_bytes': 1024, 'max_entries': 2, 'max_age_seconds': 60},
                trusted_prefix=root,
            )
            factory = CI.CargoCacheFactory(cfg, expected_owner=os.getuid())
            # Effect-free before create/use: directory not yet created.
            self.assertFalse(cache_dir.exists())
            provider = factory.create()
            self.assertIsInstance(provider, CI.LocalCargoCacheProvider)
            self.assertFalse(cache_dir.exists())
            # Outside trusted prefix or symlink rejected before any disk effect.
            with self.assertRaises(ValueError):
                CI.CargoCacheConfig.parse({'enabled': True, 'root': '/tmp/outside-cache'}, trusted_prefix=root)
            link = root / 'link-cache'; link.symlink_to(root)
            with self.assertRaises(ValueError):
                CI.CargoCacheConfig.parse({'enabled': True, 'root': str(link)}, trusted_prefix=root)
            with self.assertRaises(ValueError):
                CI.CargoCacheConfig.parse({'enabled': 1, 'root': str(cache_dir)}, trusted_prefix=root)
            with self.assertRaises(ValueError):
                CI.CargoCacheConfig.parse({'enabled': True, 'root': str(cache_dir), 'max_entries': 0}, trusted_prefix=root)
            with self.assertRaises(ValueError):
                CI.CargoCacheConfig.parse({'enabled': True, 'root': str(cache_dir), 'unexpected': True}, trusted_prefix=root)

    def test_pure_timestamp_ordering_validation_edges(self):
        inv = {'src/lib.rs': {'blob': 'a' * 40, 'mode': '100644', 'mtime_ns': 1000}}
        self.assertEqual(
            CI.validate_publish_timestamps(
                published_at_ns=2000, max_artifact_mtime_ns=1900, max_fingerprint_mtime_ns=1800, source_inventory=inv
            ),
            (True, None),
        )
        # Future artifact or source mtime rejected at publish time.
        self.assertEqual(
            CI.validate_publish_timestamps(
                published_at_ns=2000, max_artifact_mtime_ns=2001, max_fingerprint_mtime_ns=1800, source_inventory=inv
            ),
            (False, 'future_artifact_mtime'),
        )
        self.assertEqual(
            CI.validate_publish_timestamps(
                published_at_ns=2000, max_artifact_mtime_ns=1900, max_fingerprint_mtime_ns=1950, source_inventory=inv
            ),
            (False, 'invalid_fingerprint_mtime'),
        )
        self.assertEqual(
            CI.validate_publish_timestamps(
                published_at_ns=2000, max_artifact_mtime_ns=1900, max_fingerprint_mtime_ns=1800,
                source_inventory={'src/lib.rs': {'blob': 'a' * 40, 'mode': '100644', 'mtime_ns': 2500}},
            ),
            (False, 'future_source_mtime'),
        )
        self.assertEqual(
            CI.validate_publish_timestamps(
                published_at_ns=2000, max_artifact_mtime_ns=0, max_fingerprint_mtime_ns=0, source_inventory=inv
            ),
            (False, 'non_positive_artifact_mtime'),
        )

        # Valid strict ordering at restore time.
        self.assertEqual(
            CI.validate_restore_timestamp_ordering(
                published_at_ns=2000,
                observed_max_artifact_mtime_ns=1900,
                observed_max_fingerprint_mtime_ns=1800,
                source_inventory=inv,
                restore_now_ns=3000,
                min_fresh_checkout_mtime_ns=2500,
                min_changed_source_mtime_ns=2600,
            ),
            (True, None),
        )
        # Equal-tick tie rejected.
        self.assertEqual(
            CI.validate_restore_timestamp_ordering(
                published_at_ns=2000,
                observed_max_artifact_mtime_ns=1900,
                observed_max_fingerprint_mtime_ns=1800,
                source_inventory=inv,
                restore_now_ns=3000,
                min_fresh_checkout_mtime_ns=2000,
                min_changed_source_mtime_ns=2000,
            ),
            (False, 'publish_not_older_than_checkout'),
        )
        self.assertEqual(
            CI.validate_restore_timestamp_ordering(
                published_at_ns=2000,
                observed_max_artifact_mtime_ns=1900,
                observed_max_fingerprint_mtime_ns=1800,
                source_inventory=inv,
                restore_now_ns=3000,
                min_fresh_checkout_mtime_ns=1900,
            ),
            (False, 'artifact_not_older_than_checkout'),
        )
        # Clock rollback (published_at_ns > restore_now_ns) rejected.
        self.assertEqual(
            CI.validate_restore_timestamp_ordering(
                published_at_ns=4000,
                observed_max_artifact_mtime_ns=3900,
                observed_max_fingerprint_mtime_ns=3800,
                source_inventory=inv,
                restore_now_ns=3000,
                min_fresh_checkout_mtime_ns=2500,
            ),
            (False, 'future_published_at'),
        )
        # Future fingerprint > artifact rejected.
        self.assertEqual(
            CI.validate_restore_timestamp_ordering(
                published_at_ns=2000,
                observed_max_artifact_mtime_ns=1800,
                observed_max_fingerprint_mtime_ns=1900,
                source_inventory=inv,
                restore_now_ns=3000,
                min_fresh_checkout_mtime_ns=2500,
            ),
            (False, 'fingerprint_after_artifact'),
        )
        # Non-positive timestamp rejected.
        self.assertEqual(
            CI.validate_restore_timestamp_ordering(
                published_at_ns=2000,
                observed_max_artifact_mtime_ns=1800,
                observed_max_fingerprint_mtime_ns=1700,
                source_inventory={'a': {'mtime_ns': 0}},
                restore_now_ns=3000,
                min_fresh_checkout_mtime_ns=2500,
            ),
            (False, 'non_positive_cached_source_mtime'),
        )

    def test_cargo_telemetry_parser_handles_seconds_and_minutes(self):
        sample = (
            b'   Compiling lib-core v0.1.0 (/work/active/source/src/rust/lib-core)\n'
            b'   Compiling app-bin v0.1.0 (/work/active/source/src/rust/app-bin)\n'
            b'    Finished `dev` profile [unoptimized + debuginfo] target(s) in 20.80s\n'
            b'    Finished `release` profile [optimized] target(s) in 1m 13s\n'
            b'    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.14s\n'
        )
        parsed = CI.parse_cargo_telemetry(sample)
        self.assertEqual(parsed['cargo_compiling_units'], 2)
        self.assertEqual(parsed['cargo_finished_count'], 3)
        self.assertEqual(parsed['cargo_compile_ms'], 20800 + 73000 + 140)
        self.assertEqual(parsed['cargo_compile_ms_kind'], 'cargo_reported_cumulative_phase_ms')

    def test_setup_ci_renders_cache_root_timeout_and_systemd_readwrite_paths(self):
        cfg = SETUP.render_pipeline_config(
            operator='lrocha',
            spool=Path('/mnt/data/forge-homelab/external-delivery/submissions'),
            state_root=Path('/mnt/data/forge-homelab'),
            toolchain=Path('/opt/forge-ci/toolchain'),
            registry=Path('/opt/forge-ci/cargo-registry'),
            modules=Path('/opt/forge-ci/node_modules'),
            verifier_sha256='a' * 64,
        )
        self.assertEqual(cfg['timeout_seconds'], 2700)
        self.assertEqual(cfg['cargo_cache']['root'], '/mnt/data/forge-homelab/ci-cache')
        self.assertTrue(cfg['cargo_cache']['enabled'])
        self.assertTrue(cfg['cargo_cache']['seed_from_main'])
        self.assertEqual(cfg['cargo_cache']['max_entries'], 8)
        self.assertEqual(cfg['cargo_cache']['max_total_bytes'], 20 * 1024**3)
        self.assertEqual(cfg['cargo_cache']['max_age_seconds'], 14 * 86400)
        unit = SETUP.render_agent_unit()
        self.assertIn('/mnt/data/forge-homelab/ci-cache', unit)
        self.assertIn('ProtectSystem=strict', unit)
        # Ensure setup-ci.py source never grants forge-ci-cache-clear to jenkins in sudoers.
        setup_src = (HERE.parent / 'jenkins/setup-ci.py').read_text()
        self.assertIn('/usr/local/sbin/forge-ci-cache-clear', setup_src)
        self.assertIn(b'jenkins ALL=(root) NOPASSWD: /usr/local/libexec/forge-ci-check *\\n'.decode('unicode_escape').strip(), setup_src)
        self.assertNotIn('NOPASSWD: /usr/local/sbin/forge-ci-cache-clear', setup_src)

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

    def test_target_mapping_preserves_readonly_source_and_only_excludes_generated_outputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); source = root / 'source'; scratch = root / 'scratch'
            source.mkdir(); scratch.mkdir(); (scratch / 'target').mkdir()
            env = {'PATH': '/usr/bin:/bin', 'HOME': str(root), 'GIT_CONFIG_NOSYSTEM': '1',
                'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_AUTHOR_NAME': 'Owned fixture',
                'GIT_COMMITTER_NAME': 'Owned fixture', 'GIT_AUTHOR_EMAIL': 'fixture@example.invalid',
                'GIT_COMMITTER_EMAIL': 'fixture@example.invalid'}
            def git(*args):
                return subprocess.check_output(['/usr/bin/git', '-C', str(source), '-c', 'core.hooksPath=/dev/null', *args], env=env, stderr=subprocess.DEVNULL)
            git('init', '--quiet')
            parents = ['src/rust', 'src/rust/space [pkg]', 'src/rust/space [pkg]/tests/consumer']
            for relative in parents:
                parent = source / relative; parent.mkdir(parents=True, exist_ok=True)
                (parent / 'Cargo.toml').write_text('immutable fixture')
            git('add', '.'); git('commit', '--quiet', '-m', 'Readonly source fixture')
            workspace_target = source / 'src/rust/target'; workspace_target.symlink_to(scratch / 'target')
            generated = CI.map_cargo_targets(source, scratch, parents,
                SimpleNamespace(pw_uid=os.getuid(), pw_gid=os.getgid()))
            self.assertEqual(len(generated), 2)
            self.assertNotEqual(generated[0].resolve(), generated[1].resolve())
            CI.exclude_generated_outputs(source, [workspace_target, *generated])
            self.assertEqual(git('status', '--porcelain', '--untracked-files=all'), b'')
            for relative in parents:
                (source / relative).chmod(0o555)
            try:
                (generated[0] / 'consumer-artifact').write_text('only scratch changes')
                self.assertEqual((generated[0].resolve() / 'consumer-artifact').read_text(), 'only scratch changes')
                if os.geteuid() != 0:
                    with self.assertRaises(PermissionError):
                        (source / parents[1] / 'forbidden-source-output').write_text('no')
                self.assertEqual(git('status', '--porcelain', '--untracked-files=all'), b'')
                (source / parents[1] / 'Cargo.toml').write_text('tracked source changed')
                self.assertIn(b'Cargo.toml', git('status', '--porcelain', '--untracked-files=all'))
            finally:
                for relative in parents: (source / relative).chmod(0o755)

    def test_target_mapping_rejects_tracked_outputs_and_symlink_ancestors_without_overwrite(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); source = root / 'source'; scratch = root / 'scratch'
            source.mkdir(); scratch.mkdir(); (source / 'pkg').mkdir()
            target = source / 'pkg/target'; target.write_text('preserved tracked content')
            account = SimpleNamespace(pw_uid=os.getuid(), pw_gid=os.getgid())
            with self.assertRaises(ValueError):
                CI.map_cargo_targets(source, scratch, ['pkg'], account)
            self.assertEqual(target.read_text(), 'preserved tracked content')
            self.assertFalse((scratch / 'package-targets').exists())
            target.unlink(); (source / 'alias').symlink_to(source / 'pkg')
            for relative in ['alias', '../outside', '/absolute']:
                with self.assertRaises(ValueError):
                    CI.map_cargo_targets(source, scratch, [relative], account)

    def test_check_network_namespace_is_created_before_loopback_setup_and_uid_drop(self):
        account = SimpleNamespace(pw_uid=43210, pw_gid=43211)
        calls = []
        library = SimpleNamespace(prctl=Mock(return_value=0))
        def loopback(argv, **options):
            self.assertEqual(argv, ['/usr/sbin/ip', 'link', 'set', 'dev', 'lo', 'up'])
            self.assertTrue(options['check']); self.assertEqual(options['timeout'], 5)
            self.assertEqual(options['env'], {'PATH': '/usr/sbin:/usr/bin:/bin'})
            calls.append('loopback')
        with patch.object(CI.os, 'unshare', side_effect=lambda flags: calls.append(('network', flags))), \
             patch.object(CI.subprocess, 'run', side_effect=loopback), \
             patch.object(CI.os, 'setgroups', side_effect=lambda groups: calls.append(('groups', groups))), \
             patch.object(CI.os, 'setgid', side_effect=lambda gid: calls.append(('gid', gid))), \
             patch.object(CI.os, 'setuid', side_effect=lambda uid: calls.append(('uid', uid))), \
             patch.object(CI.os, 'umask'), patch.object(CI.ctypes, 'CDLL', return_value=library):
            CI.child_identity(account)()
        self.assertEqual(calls, [('network', os.CLONE_NEWNET), 'loopback', ('groups', []), ('gid', 43211), ('uid', 43210)])
        library.prctl.assert_called_once_with(38, 1, 0, 0, 0)
        with patch.object(CI.os, 'unshare', side_effect=PermissionError('owned failure fixture')), \
             patch.object(CI.os, 'setuid') as setuid:
            with self.assertRaises(PermissionError): CI.child_identity(account)()
            setuid.assert_not_called()

    def test_private_short_tmp_supports_real_unix_sockets_and_does_not_follow_cleanup_links(self):
        account = SimpleNamespace(pw_uid=os.getuid(), pw_gid=os.getgid())
        with tempfile.TemporaryDirectory() as outside:
            sentinel = Path(outside) / 'preserved'; sentinel.write_text('outside CI temporary scope')
            short = CI.create_short_tmp(account)
            try:
                self.assertEqual(short.parent, Path('/tmp'))
                self.assertLess(len(os.fsencode(short / 'owned.sock')), 108)
                self.assertEqual(stat.S_IMODE(short.stat().st_mode), 0o700)
                self.assertEqual(short.stat().st_uid, account.pw_uid)
                with socket.socket(socket.AF_UNIX) as owned:
                    owned.bind(str(short / 'owned.sock'))
                (short / 'outside-link').symlink_to(outside, target_is_directory=True)
                CI.cleanup_short_tmp(short)
                self.assertFalse(short.exists())
                self.assertEqual(sentinel.read_text(), 'outside CI temporary scope')
            finally:
                if short.exists(): CI.cleanup_short_tmp(short)
            replacement = CI.create_short_tmp(account)
            replacement.rmdir(); replacement.symlink_to(outside, target_is_directory=True)
            try:
                with self.assertRaises(ValueError): CI.cleanup_short_tmp(replacement)
                self.assertTrue(sentinel.exists())
            finally:
                replacement.unlink()

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
            scope = 'branch/' + CI.canonical_branch_slug('feat/cancel-test')
            request = {'version': 1, 'action_id': action, 'workflow_id': str(uuid4()),
                'project_alias': 'homelab', 'candidate_commit': 'b' * 40,
                'candidate_tree': 'c' * 40, 'expected_main': 'd' * 40, 'branch_scope': scope,
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
            source = work / 'active' / 'source'
            def git(command, **_options):
                if 'init' in command:
                    (source / 'src/rust/forge-ide-web/tests/browser').mkdir(parents=True)
                    (source / 'Makefile').write_text('fixture')
                    (source / '.git/info').mkdir(parents=True)
                    (source / '.git/info/exclude').write_text('')
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
            self.assertEqual(result['branch_scope'], scope)
            self.assertEqual(order[-1][1], result)
            self.assertFalse((work / 'active').exists())

    def test_real_node_import_resolves_sibling_packages_from_the_provisioned_cache_basename(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            modules = root / 'staged' / SETUP.NODE_MODULES.name
            package = modules / '@playwright/test'; package.mkdir(parents=True)
            sibling = modules / 'playwright'; sibling.mkdir()
            (package / 'package.json').write_text(json.dumps({'name': '@playwright/test', 'type': 'module', 'exports': './index.mjs'}))
            (package / 'index.mjs').write_text("export { ready } from 'playwright/test';\n")
            (sibling / 'package.json').write_text(json.dumps({'name': 'playwright', 'type': 'module', 'exports': {'./test': './test.mjs'}}))
            (sibling / 'test.mjs').write_text('export const ready = true;\n')
            project = root / 'project'; project.mkdir()
            (project / 'node_modules').symlink_to(modules, target_is_directory=True)
            result = subprocess.run(['/usr/bin/node', '--input-type=module', '-e',
                "const { ready } = await import('@playwright/test'); console.log(ready);"],
                cwd=project, env={'PATH': '/usr/bin:/bin', 'HOME': str(root)},
                capture_output=True, text=True, timeout=10, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, 'true\n')

    def test_provisioner_requires_explicit_execution_before_reading_configuration(self):
        result = subprocess.run([sys.executable, '-B', str(HERE.parent / 'jenkins/setup-ci.py'),
            '--client-config', '/nonexistent/never-opened'], capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 2)
        self.assertIn('Explicit --execute', result.stderr)


@unittest.skipUnless(TOOLCHAIN_AVAILABLE, 'Requires /opt/forge-ci/toolchain')
class CargoCacheSupervisorIntegrationTest(unittest.TestCase):
    """End-to-end tests using real git bundles and real /opt/forge-ci/toolchain/bin/cargo."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='forge-cargo-cache-test-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.spool = self.root / 'spool'
        self.results = self.root / 'results'
        self.work = self.root / 'work'
        self.cache_root = self.root / 'ci-cache'
        self.registry = self.root / 'registry'
        self.node_modules = self.root / 'node_modules'
        for p in (self.spool, self.results, self.work, self.registry, self.node_modules):
            p.mkdir(mode=0o700)
        self.repo = self.root / 'fixture-repo'
        self.git_env = {
            'PATH': '/usr/bin:/bin',
            'HOME': str(self.root),
            'GIT_CONFIG_NOSYSTEM': '1',
            'GIT_CONFIG_GLOBAL': '/dev/null',
            'GIT_NO_REPLACE_OBJECTS': '1',
            'GIT_AUTHOR_NAME': 'Fixture',
            'GIT_COMMITTER_NAME': 'Fixture',
            'GIT_AUTHOR_EMAIL': 'fixture@example.invalid',
            'GIT_COMMITTER_EMAIL': 'fixture@example.invalid',
        }
        self._init_cargo_repo()

    def _git(self, *args, cwd=None):
        return subprocess.check_output(
            ['/usr/bin/git', '-c', 'core.hooksPath=/dev/null', *args],
            cwd=cwd or self.repo,
            env=self.git_env,
            stderr=subprocess.DEVNULL,
        )

    def _init_cargo_repo(self):
        self.repo.mkdir()
        self._git('init', '--quiet', '--initial-branch=main')
        (self.repo / 'src/rust/forge-ide-web/tests/browser').mkdir(parents=True)
        (self.repo / 'src/rust/forge-ide-web/tests/browser/.keep').write_text('keep\n')
        (self.repo / 'src/rust/lib-core/src').mkdir(parents=True)
        (self.repo / 'src/rust/app-bin/src').mkdir(parents=True)
        (self.repo / 'Makefile').write_text('ci:\n\t@true\n')
        (self.repo / 'src/rust/Cargo.toml').write_text(
            '[workspace]\nmembers = ["lib-core", "app-bin"]\nresolver = "2"\n'
        )
        (self.repo / 'src/rust/lib-core/Cargo.toml').write_text(
            '[package]\nname = "lib-core"\nversion = "0.1.0"\nedition = "2021"\n'
        )
        (self.repo / 'src/rust/lib-core/src/lib.rs').write_text(
            'pub fn greeting() -> &\'static str { "core-v1" }\n'
        )
        (self.repo / 'src/rust/app-bin/Cargo.toml').write_text(
            '[package]\nname = "app-bin"\nversion = "0.1.0"\nedition = "2021"\n'
            '[dependencies]\nlib-core = { path = "../lib-core" }\n'
        )
        (self.repo / 'src/rust/app-bin/src/main.rs').write_text(
            'fn main() { println!("{}:v1", lib_core::greeting()); }\n'
            '#[cfg(test)]\nmod tests {\n'
            '    #[test]\n    fn prints_expected() {\n'
            '        assert_eq!(format!("{}:v1", lib_core::greeting()), "core-v1:v1");\n'
            '    }\n}\n'
        )
        self._git('add', '.')
        self._git('commit', '--quiet', '-m', 'Initial workspace v1')
        self.base_commit = self._git('rev-parse', 'HEAD').decode().strip()

    def _commit_change(self, rel_path, content, message='Edit'):
        path = self.repo / rel_path
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
        self._git('add', rel_path)
        self._git('commit', '--quiet', '-m', message)
        return self._git('rev-parse', 'HEAD').decode().strip()

    def _make_bundle(self, commit):
        ref = 'refs/forge/external-candidates/' + str(uuid4())
        self._git('update-ref', ref, commit)
        bundle = self.root / (str(uuid4()) + '.bundle')
        self._git('bundle', 'create', str(bundle), ref)
        return bundle.read_bytes()

    def _gate_manifest(self, commit, checks, profile='fixture-profile'):
        rows = self._git('ls-tree', '-r', '-z', '--full-tree', commit)
        protected = {}
        for record in rows.split(b'\0'):
            if not record:
                continue
            descriptor, raw_path = record.split(b'\t', 1)
            mode, _kind, blob = descriptor.decode().split()
            path = raw_path.decode('utf-8')
            if CI.protected_gate_path(path):
                protected[path] = [mode, blob]
        helper_digest = hashlib.sha256(Path(CI.__file__).read_bytes()).hexdigest()
        return helper_digest, hashlib.sha256(CI.canonical({
            'version': 1, 'provider': 'jenkins', 'profile': profile,
            'checks': checks, 'verifier_sha256': helper_digest,
            'source_gate_sha256': hashlib.sha256(CI.canonical(protected)).hexdigest(),
        })).hexdigest()

    def _run_supervisor(
        self,
        *,
        commit,
        expected_main,
        branch_scope,
        checks=None,
        profile='fixture-profile',
        cache_config_override=None,
        cache_port=None,
        extra_env=None,
        run_child_override=None,
        workflow_id=None,
    ):
        action = str(uuid4())
        if checks is None:
            checks = (
                (str(TOOLCHAIN / 'bin/cargo'), 'test', '--manifest-path', 'src/rust/Cargo.toml', '--offline'),
                ('/usr/bin/python3', '-c', 'import json, os, pathlib; '
                 'd = pathlib.Path(os.environ["QA_OUTPUT_ROOT"]) / "forge-ui-qa-1"; d.mkdir(parents=True); '
                 '(d / "run.json").write_text(json.dumps({"stages": ['
                 '{"id": "generated-assets", "status": "passed", "duration_ms": 11}, '
                 '{"id": "agent-and-fixture", "status": "passed", "duration_ms": 22}, '
                 '{"id": "browser-journeys", "status": "passed", "duration_ms": 33}]}))'),
                ('/usr/bin/python3', '-c', 'print("unit-ok")'),
            )
        bundle = self._make_bundle(commit)
        tree = self._git('rev-parse', commit + '^{tree}').decode().strip()
        helper_digest, manifest = self._gate_manifest(commit, checks, profile=profile)
        sub_dir = self.spool / action
        sub_dir.mkdir(mode=0o700)
        (sub_dir / 'candidate.bundle').write_bytes(bundle)
        (sub_dir / 'candidate.bundle').chmod(0o600)
        req = {
            'version': 1, 'action_id': action, 'workflow_id': workflow_id or str(uuid4()),
            'project_alias': 'homelab', 'candidate_commit': commit, 'candidate_tree': tree,
            'expected_main': expected_main, 'branch_scope': branch_scope,
            'gate_manifest_sha256': manifest, 'bundle_sha256': hashlib.sha256(bundle).hexdigest(),
            'verification_profile': profile, 'correlation_id': str(uuid4()),
        }
        (sub_dir / 'request.json').write_text(json.dumps(req, sort_keys=True))
        (sub_dir / 'request.json').chmod(0o600)
        cache_raw = {
            'enabled': True,
            'root': str(self.cache_root),
            'max_total_bytes': 2 * 1024**3,
            'max_entries': 8,
            'max_age_seconds': 14 * 86400,
            'seed_from_main': True,
        }
        if cache_config_override:
            cache_raw.update(cache_config_override)
        config = {
            'build_user': 'forge_ci', 'submitter_user': 'fixture',
            'spool_root': str(self.spool), 'results_root': str(self.results),
            'work_root': str(self.work), 'projects': ['homelab'],
            'verification_profile': profile, 'verifier_sha256': helper_digest,
            'toolchain': str(TOOLCHAIN), 'cargo_registry': str(self.registry),
            'node_modules': str(self.node_modules), 'timeout_seconds': 120,
            'cargo_cache': cache_raw,
        }
        # Use current non-root UID as build account and distinct fake submitter UID?
        # execute_locked checks account.pw_uid != submitter.pw_uid, so patch read_file/chown cleanly.
        uid_now = os.getuid()
        gid_now = os.getgid()
        build = SimpleNamespace(pw_uid=uid_now, pw_gid=gid_now, pw_name='forge_ci')
        submitter = SimpleNamespace(pw_uid=uid_now + 1, pw_gid=gid_now + 1)
        if cache_port is None:
            parsed = CI.CargoCacheConfig.parse(cache_raw, trusted_prefix=self.root)
            cache_port = CI.CargoCacheFactory(parsed, expected_owner=uid_now, expected_group=gid_now).create()

        def local_run_child(argv, *, cwd, env, account, log, timeout, cancelled):
            run_env = dict(env)
            if extra_env:
                run_env.update(extra_env)
            proc = subprocess.run(argv, cwd=cwd, env=run_env, stdin=subprocess.DEVNULL,
                                  stdout=log, stderr=subprocess.STDOUT, timeout=timeout, check=False)
            return proc.returncode

        orig_read_file = CI.read_file
        def test_read_file(path, *, owner, maximum):
            return orig_read_file(path, owner=uid_now, maximum=maximum)

        orig_write_result = CI.write_result
        def test_write_result(path, result, reader_group):
            with patch.object(CI.os, 'fchown'):
                return orig_write_result(path, result, reader_group)

        orig_open_log = CI.open_log
        def test_open_log(path, reader_group):
            with patch.object(CI.os, 'fchown'):
                return orig_open_log(path, reader_group)

        with patch.object(CI, 'CHECKS', checks), \
             patch.object(CI, 'TRUSTED_CACHE_ROOT_PREFIX', self.root), \
             patch.object(CI.pwd, 'getpwnam', side_effect=lambda name: build if name == 'forge_ci' else submitter), \
             patch.object(CI.grp, 'getgrgid', return_value=SimpleNamespace(gr_name='forge_ci')), \
             patch.object(CI, 'regular_directory'), \
             patch.object(CI, 'read_file', side_effect=test_read_file), \
             patch.object(CI, 'write_result', side_effect=test_write_result), \
             patch.object(CI, 'open_log', side_effect=test_open_log), \
             patch.object(CI, 'kill_build_user'), \
             patch.object(CI, 'run_child', side_effect=run_child_override or local_run_child):
            # If extra_env is supplied, also inject it into resolve_key's compiler env observation.
            if extra_env:
                orig_extract = CI.extract_compiler_and_feature_env
                def patched_extract(env):
                    return orig_extract({**env, **extra_env})
                with patch.object(CI, 'extract_compiler_and_feature_env', side_effect=patched_extract):
                    res = CI.execute_locked(action, config, cache_port=cache_port)
            else:
                res = CI.execute_locked(action, config, cache_port=cache_port)
        log_text = (self.results / (action + '.log')).read_text('utf-8', errors='replace')
        return res, log_text, config

    def _entry_dir(self, scope_dir, compat_key):
        return self.cache_root / 'scopes' / 'homelab' / scope_dir / compat_key

    def test_cross_workflow_reuse_incremental_recompile_and_revert(self):
        branch_scope = 'branch/' + CI.canonical_branch_slug('feat/cargo-cache')
        # Workflow 1 (cold): compiles both crates and publishes.
        r1, log1, _ = self._run_supervisor(
            commit=self.base_commit, expected_main='0' * 40, branch_scope=branch_scope, workflow_id=str(uuid4()),
        )
        self.assertEqual(r1['status'], 'passed', log1)
        self.assertEqual(r1['cache']['restore_status'], 'miss')
        self.assertEqual(r1['cache']['publish_status'], 'published')
        check0_1 = next(s for s in r1['stages'] if s['name'] == 'check_0')
        check1_1 = next(s for s in r1['stages'] if s['name'] == 'check_1')
        self.assertGreaterEqual(check0_1['cargo_compiling_units'], 2)
        self.assertEqual(check1_1['ui_qa_stages']['browser-journeys']['duration_ms'], 33)
        self.assertIn('FORGE_CACHE_RESTORE status=miss', log1)
        self.assertIn('FORGE_CACHE_PUBLISH status=published', log1)
        self.assertFalse((self.work / 'active').exists())

        # Advance past coarse kernel filesystem tick before Workflow 2.
        time.sleep(0.03)

        # Workflow 2 (distinct workflow_id, same branch_scope): warm hit, zero recompilations.
        r2, log2, _ = self._run_supervisor(
            commit=self.base_commit, expected_main='0' * 40, branch_scope=branch_scope, workflow_id=str(uuid4()),
        )
        self.assertEqual(r2['status'], 'passed', log2)
        self.assertEqual(r2['cache']['restore_status'], 'hit_branch')
        self.assertGreater(r2['cache']['restored_files'], 0)
        self.assertGreater(r2['cache']['restored_bytes'], 0)
        check0_2 = next(s for s in r2['stages'] if s['name'] == 'check_0')
        self.assertEqual(check0_2['cargo_compiling_units'], 0, log2)

        # Edit only app-bin (v1 -> v2): lib-core NOT recompiled, app-bin IS recompiled and passes v2 assertion.
        time.sleep(0.03)
        v2_commit = self._commit_change(
            'src/rust/app-bin/src/main.rs',
            'fn main() { println!("{}:v2", lib_core::greeting()); }\n'
            '#[cfg(test)]\nmod tests {\n'
            '    #[test]\n    fn prints_expected() {\n'
            '        assert_eq!(format!("{}:v2", lib_core::greeting()), "core-v1:v2");\n'
            '    }\n}\n',
            'Bump app-bin to v2',
        )
        r3, log3, _ = self._run_supervisor(
            commit=v2_commit, expected_main='0' * 40, branch_scope=branch_scope, workflow_id=str(uuid4()),
        )
        self.assertEqual(r3['status'], 'passed', log3)
        self.assertEqual(r3['cache']['restore_status'], 'hit_branch')
        check0_3 = next(s for s in r3['stages'] if s['name'] == 'check_0')
        self.assertGreaterEqual(check0_3['cargo_compiling_units'], 1)
        self.assertIn('Compiling app-bin', log3)
        self.assertNotIn('Compiling lib-core', log3)

        # Revert (v2 -> v1): app-bin recompiles cleanly and passes v1 assertion.
        time.sleep(0.03)
        v1_revert = self._commit_change(
            'src/rust/app-bin/src/main.rs',
            'fn main() { println!("{}:v1", lib_core::greeting()); }\n'
            '#[cfg(test)]\nmod tests {\n'
            '    #[test]\n    fn prints_expected() {\n'
            '        assert_eq!(format!("{}:v1", lib_core::greeting()), "core-v1:v1");\n'
            '    }\n}\n',
            'Revert app-bin to v1',
        )
        r4, log4, _ = self._run_supervisor(
            commit=v1_revert, expected_main='0' * 40, branch_scope=branch_scope, workflow_id=str(uuid4()),
        )
        self.assertEqual(r4['status'], 'passed', log4)
        self.assertEqual(r4['cache']['restore_status'], 'hit_branch')
        self.assertIn('Compiling app-bin', log4)
        self.assertNotIn('Compiling lib-core', log4)

        # Exact main reuse across two distinct workflows.
        rm1, _, _ = self._run_supervisor(commit=self.base_commit, expected_main=self.base_commit, branch_scope='main')
        self.assertEqual(rm1['cache']['restore_status'], 'miss')
        self.assertEqual(rm1['cache']['publish_status'], 'published')
        time.sleep(0.03)
        rm2, logm2, _ = self._run_supervisor(commit=self.base_commit, expected_main=self.base_commit, branch_scope='main')
        self.assertEqual(rm2['cache']['restore_status'], 'hit_main')
        check0_m2 = next(s for s in rm2['stages'] if s['name'] == 'check_0')
        self.assertEqual(check0_m2['cargo_compiling_units'], 0, logm2)

    def test_future_dated_snapshot_falls_back_cleanly_on_source_edit_and_self_heals(self):
        branch_scope = 'branch/' + CI.canonical_branch_slug('feat/future-guard')
        scope_dir = CI.scope_directory_name(branch_scope)
        # Build 1: valid v1 snapshot.
        r1, log1, _ = self._run_supervisor(commit=self.base_commit, expected_main='0' * 40, branch_scope=branch_scope)
        self.assertEqual(r1['status'], 'passed', log1)
        self.assertEqual(r1['cache']['publish_status'], 'published')
        compat_key = r1['cache']['compat_key']

        # Mutate cached snapshot: forge future timestamps (+3600s) on .fingerprint/*/invoked.timestamp
        # AND re-sign meta.json so digest checks pass (simulating clock rollback / future-dated snapshot).
        entry = self._entry_dir(scope_dir, compat_key)
        pointer = json.loads((entry / 'current.json').read_text())
        snap_dir = entry / pointer['snapshot_id']
        future_ns = time.time_ns() + 3600 * 1_000_000_000
        touched_fp = 0
        for p in sorted((snap_dir / 'target').rglob('invoked.timestamp')):
            os.utime(p, ns=(future_ns, future_ns))
            touched_fp += 1
        self.assertGreater(touched_fp, 0)
        # Recompute exact files_sha256 & manifest_sha256 to match mutated snapshot timestamps.
        t_entries, t_bytes, t_files, t_max_art, t_max_fp = CI._copy_tree_from_build(
            snap_dir / 'target', self.root / 'rehash-t', expected_uid=os.getuid(), prefix_rel='target'
        )
        p_entries, p_bytes, p_files, p_max_art, p_max_fp = CI._copy_tree_from_build(
            snap_dir / 'package-targets', self.root / 'rehash-p', expected_uid=os.getuid(), prefix_rel='package-targets'
        )
        meta = json.loads((snap_dir / 'meta.json').read_text())
        all_entries = sorted(t_entries + p_entries, key=lambda row: row[0])
        meta['published_at_ns'] = future_ns
        meta['max_artifact_mtime_ns'] = max(t_max_art, p_max_art)
        meta['max_fingerprint_mtime_ns'] = max(t_max_fp, p_max_fp)
        meta['files_sha256'] = hashlib.sha256(CI.canonical(all_entries)).hexdigest()
        meta['file_count'] = t_files + p_files
        meta['total_bytes'] = t_bytes + p_bytes
        meta['manifest_sha256'] = CI._snapshot_manifest_digest(meta)
        (snap_dir / 'meta.json').write_bytes(CI.canonical(meta) + b'\n')

        # Edit source v1 -> v2 (test specifically asserts compiled output is v2!).
        v2_commit = self._commit_change(
            'src/rust/app-bin/src/main.rs',
            'fn main() { println!("v2"); }\n'
            '#[cfg(test)]\nmod tests {\n'
            '    #[test]\n    fn prints_v2() {\n'
            '        let out = format!("{}:v2", lib_core::greeting());\n'
            '        assert_eq!(out, "core-v1:v2");\n'
            '    }\n}\n',
            'Source edit v1 -> v2',
        )

        # Build 2: MUST reject future snapshot, clean build v2, and self-heal snapshot.
        r2, log2, _ = self._run_supervisor(commit=v2_commit, expected_main='0' * 40, branch_scope=branch_scope)
        self.assertEqual(r2['status'], 'passed', log2)
        self.assertEqual(r2['cache']['restore_status'], 'timestamp_ordering_fallback')
        self.assertIn(r2['cache']['restore_fallback_reason'], ('future_published_at', 'artifact_not_older_than_checkout'))
        check0_2 = next(s for s in r2['stages'] if s['name'] == 'check_0')
        self.assertGreaterEqual(check0_2['cargo_compiling_units'], 2)
        self.assertIn('prints_v2', log2)
        self.assertEqual(r2['cache']['publish_status'], 'published')

        # Build 3: self-healed snapshot yields warm hit with zero recompilations.
        time.sleep(0.03)
        r3, log3, _ = self._run_supervisor(commit=v2_commit, expected_main='0' * 40, branch_scope=branch_scope)
        self.assertEqual(r3['status'], 'passed', log3)
        self.assertEqual(r3['cache']['restore_status'], 'hit_branch')
        check0_3 = next(s for s in r3['stages'] if s['name'] == 'check_0')
        self.assertEqual(check0_3['cargo_compiling_units'], 0, log3)

    def test_build_owned_future_timestamp_and_symlink_rejected_on_publish(self):
        branch_scope = 'branch/' + CI.canonical_branch_slug('feat/unsafe-publish')
        scope_dir = CI.scope_directory_name(branch_scope)
        # Seed known-good snapshot first.
        r1, _, _ = self._run_supervisor(commit=self.base_commit, expected_main='0' * 40, branch_scope=branch_scope)
        self.assertEqual(r1['cache']['publish_status'], 'published')
        entry = self._entry_dir(scope_dir, r1['cache']['compat_key'])
        known_good_pointer = (entry / 'current.json').read_bytes()

        # Case A: build touches a future-dated file in scratch/target while exiting 0.
        time.sleep(0.03)
        def forge_future_check(argv, *, cwd, env, account, log, timeout, cancelled):
            proc = subprocess.run(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                                  stdout=log, stderr=subprocess.STDOUT, timeout=timeout, check=False)
            target_file = Path(env['CARGO_TARGET_DIR']) / 'future-marker.tmp'
            target_file.write_text('forged future')
            future_ns = time.time_ns() + 3600 * 1_000_000_000
            os.utime(target_file, ns=(future_ns, future_ns))
            return proc.returncode

        r_future, _, _ = self._run_supervisor(
            commit=self.base_commit, expected_main='0' * 40, branch_scope=branch_scope,
            run_child_override=forge_future_check,
        )
        self.assertEqual(r_future['status'], 'passed')
        self.assertEqual(r_future['cache']['publish_status'], 'skipped_unsafe_timestamps')
        self.assertEqual((entry / 'current.json').read_bytes(), known_good_pointer)
        self.assertEqual([n for n in os.listdir(self.cache_root) if n.startswith('.staging-')], [])

        # Case B: build plants a symlink inside scratch/target pointing to an outside secret file.
        secret = self.root / 'outside-secret.txt'
        secret.write_text('TOP_SECRET_NEVER_CACHE')
        time.sleep(0.03)
        def plant_symlink_check(argv, *, cwd, env, account, log, timeout, cancelled):
            proc = subprocess.run(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                                  stdout=log, stderr=subprocess.STDOUT, timeout=timeout, check=False)
            link = Path(env['CARGO_TARGET_DIR']) / 'escape-link'
            if not link.is_symlink():
                link.symlink_to(secret)
            return proc.returncode

        r_sym, _, _ = self._run_supervisor(
            commit=self.base_commit, expected_main='0' * 40, branch_scope=branch_scope,
            run_child_override=plant_symlink_check,
        )
        self.assertEqual(r_sym['status'], 'passed')
        self.assertEqual(r_sym['cache']['publish_status'], 'skipped_unsafe_artifact')
        self.assertEqual((entry / 'current.json').read_bytes(), known_good_pointer)
        self.assertEqual([n for n in os.listdir(self.cache_root) if n.startswith('.staging-')], [])

    def test_compatibility_key_rejects_mismatched_flags_config_and_profile(self):
        branch_scope = 'branch/' + CI.canonical_branch_slug('feat/compat')
        r1, _, _ = self._run_supervisor(commit=self.base_commit, expected_main='0' * 40, branch_scope=branch_scope)
        self.assertEqual(r1['cache']['publish_status'], 'published')
        k1 = r1['cache']['compat_key']

        # Changing RUSTFLAGS produces distinct compat_key and forces clean rebuild.
        time.sleep(0.03)
        r_flags, log_flags, _ = self._run_supervisor(
            commit=self.base_commit, expected_main='0' * 40, branch_scope=branch_scope,
            extra_env={'RUSTFLAGS': '-C opt-level=1'},
        )
        self.assertEqual(r_flags['status'], 'passed', log_flags)
        self.assertNotEqual(r_flags['cache']['compat_key'], k1)
        self.assertEqual(r_flags['cache']['restore_status'], 'incompatible')
        self.assertGreaterEqual(next(s for s in r_flags['stages'] if s['name'] == 'check_0')['cargo_compiling_units'], 2)

        # Changing .cargo/config.toml produces distinct compat_key.
        time.sleep(0.03)
        cfg_commit = self._commit_change('.cargo/config.toml', '[build]\njobs = 2\n', 'Add cargo config')
        r_cfg, _, _ = self._run_supervisor(commit=cfg_commit, expected_main='0' * 40, branch_scope=branch_scope)
        self.assertNotEqual(r_cfg['cache']['compat_key'], k1)
        self.assertEqual(r_cfg['cache']['restore_status'], 'incompatible')

        # Changing verification_profile produces distinct compat_key.
        time.sleep(0.03)
        r_prof, _, _ = self._run_supervisor(
            commit=self.base_commit, expected_main='0' * 40, branch_scope=branch_scope, profile='other-profile',
        )
        self.assertNotEqual(r_prof['cache']['compat_key'], k1)
        self.assertEqual(r_prof['cache']['restore_status'], 'incompatible')

        # Changing observed toolchain fingerprint (rustc -vV / binary / rustlib target manifest)
        # produces distinct compat_key, refuses reuse, and forces clean rebuild.
        time.sleep(0.03)
        real_fp = CI.observe_toolchain_fingerprint(TOOLCHAIN)
        altered_fp = {
            **real_fp,
            'rustc_vv': real_fp['rustc_vv'] + '\nrelease: 1.98.0-alt',
            'rustlib_manifests': {**real_fp['rustlib_manifests'], 'manifest-rust-std-wasm32-unknown-unknown': '0' * 64},
        }
        with patch.object(CI, 'observe_toolchain_fingerprint', return_value=altered_fp):
            r_tc, log_tc, _ = self._run_supervisor(
                commit=self.base_commit, expected_main='0' * 40, branch_scope=branch_scope,
            )
        self.assertEqual(r_tc['status'], 'passed', log_tc)
        self.assertNotEqual(r_tc['cache']['compat_key'], k1)
        self.assertEqual(r_tc['cache']['restore_status'], 'incompatible')
        self.assertGreaterEqual(next(s for s in r_tc['stages'] if s['name'] == 'check_0')['cargo_compiling_units'], 2)

        # Pure compat_key sensitivity checks for host_platform, RUSTDOCFLAGS, CARGO_PROFILE_*, and CHECKS.
        base_args = dict(
            project_alias='homelab',
            verification_profile='forge-sdlc-jenkins-v1',
            checks=CI.CHECKS,
            toolchain_fingerprint=real_fp,
            host_platform={'platform': 'linux', 'machine': 'x86_64'},
            compiler_and_feature_env={},
            repo_build_config={},
            canonical_build_root='/mnt/data/forge-homelab/ci-work/active',
        )
        base_key, _ = CI.compute_compat_key(**base_args)
        for label, override in (
            ('host_platform', {'host_platform': {'platform': 'linux', 'machine': 'aarch64'}}),
            ('rustdocflags', {'compiler_and_feature_env': {'RUSTDOCFLAGS': '-D warnings'}}),
            ('cargo_profile', {'compiler_and_feature_env': {'CARGO_PROFILE_DEV_OPT_LEVEL': '1'}}),
            ('checks', {'checks': (('/usr/bin/make', 'ci'),)}),
            ('build_root', {'canonical_build_root': '/mnt/data/forge-homelab/ci-work/other'}),
        ):
            with self.subTest(compat_dim=label):
                alt_key, _ = CI.compute_compat_key(**{**base_args, **override})
                self.assertNotEqual(alt_key, base_key)

    def test_branch_isolation_readonly_main_seeding_detached_and_fail_closed_scopes(self):
        # 1. Populate main cache.
        rm, _, _ = self._run_supervisor(commit=self.base_commit, expected_main=self.base_commit, branch_scope='main')
        self.assertEqual(rm['cache']['publish_status'], 'published')
        main_entry = self._entry_dir('main', rm['cache']['compat_key'])
        main_pointer_before = (main_entry / 'current.json').read_bytes()
        main_snap = main_entry / json.loads(main_pointer_before)['snapshot_id']
        main_meta_before = (main_snap / 'meta.json').read_bytes()

        # 2. Branch A cold seeds read-only from main and publishes ONLY to Branch A.
        time.sleep(0.03)
        scope_a = 'branch/' + CI.canonical_branch_slug('feat/alpha')
        ra, log_a, _ = self._run_supervisor(commit=self.base_commit, expected_main=self.base_commit, branch_scope=scope_a)
        self.assertEqual(ra['status'], 'passed', log_a)
        self.assertEqual(ra['cache']['restore_status'], 'hit_main_seed')
        self.assertEqual(ra['cache']['publish_status'], 'published')
        self.assertEqual(next(s for s in ra['stages'] if s['name'] == 'check_0')['cargo_compiling_units'], 0)
        self.assertEqual((main_entry / 'current.json').read_bytes(), main_pointer_before)
        self.assertEqual((main_snap / 'meta.json').read_bytes(), main_meta_before)

        entry_a = self._entry_dir(CI.scope_directory_name(scope_a), ra['cache']['compat_key'])
        a_pointer_before = (entry_a / 'current.json').read_bytes()

        # 3. Branch B does not overwrite Branch A or main.
        time.sleep(0.03)
        scope_b = 'branch/' + CI.canonical_branch_slug('feat/beta')
        rb, _, _ = self._run_supervisor(commit=self.base_commit, expected_main=self.base_commit, branch_scope=scope_b)
        self.assertEqual(rb['cache']['restore_status'], 'hit_main_seed')
        self.assertEqual(rb['cache']['publish_status'], 'published')
        self.assertEqual((main_entry / 'current.json').read_bytes(), main_pointer_before)
        self.assertEqual((entry_a / 'current.json').read_bytes(), a_pointer_before)

        # 4. Detached scope seeds read-only from main and NEVER publishes.
        time.sleep(0.03)
        rd, _, _ = self._run_supervisor(commit=self.base_commit, expected_main='0' * 40, branch_scope='detached')
        self.assertEqual(rd['cache']['restore_status'], 'hit_main_seed')
        self.assertEqual(rd['cache']['publish_status'], 'skipped_unscoped')
        self.assertEqual((main_entry / 'current.json').read_bytes(), main_pointer_before)
        self.assertFalse((self.cache_root / 'scopes/homelab/detached').exists())

        # 5. Fail-closed supervisor guards reject invalid/spoofed scopes before touching cache or active slot.
        for bad_scope in ('main', 'candidates/main', 'workflow-abc', '../main', ''):
            with self.subTest(bad_scope=bad_scope):
                with self.assertRaises(ValueError):
                    self._run_supervisor(commit=self.base_commit, expected_main='0' * 40, branch_scope=bad_scope)

    def test_null_provider_recovery_retention_and_clear_helper(self):
        # 1. NullCargoCacheProvider substitution never touches cache_root.
        null_port = CI.NullCargoCacheProvider(CI.CargoCacheConfig(enabled=False))
        r_null, _, _ = self._run_supervisor(
            commit=self.base_commit, expected_main='0' * 40,
            branch_scope='branch/' + CI.canonical_branch_slug('feat/null'),
            cache_port=null_port,
        )
        self.assertEqual(r_null['status'], 'passed')
        self.assertEqual(r_null['cache']['provider'], 'null')
        self.assertEqual(r_null['cache']['restore_status'], 'disabled')
        self.assertEqual(r_null['cache']['publish_status'], 'disabled')
        self.assertFalse(self.cache_root.exists())

        # 2. Populate branch cache, then test failed check preserves known-good cache.
        scope = 'branch/' + CI.canonical_branch_slug('feat/recovery')
        scope_dir = CI.scope_directory_name(scope)
        r1, _, config = self._run_supervisor(commit=self.base_commit, expected_main='0' * 40, branch_scope=scope)
        self.assertEqual(r1['cache']['publish_status'], 'published')
        entry = self._entry_dir(scope_dir, r1['cache']['compat_key'])
        good_pointer = (entry / 'current.json').read_bytes()

        time.sleep(0.03)
        r_fail, _, _ = self._run_supervisor(
            commit=self.base_commit, expected_main='0' * 40, branch_scope=scope,
            run_child_override=lambda *a, **kw: 1,
        )
        self.assertEqual(r_fail['status'], 'failed')
        self.assertEqual(r_fail['cache']['publish_status'], 'skipped_failed')
        self.assertEqual((entry / 'current.json').read_bytes(), good_pointer)

        # 3. Corrupt meta.json -> corrupt_fallback clean build -> replaces corrupt snapshot.
        snap_dir = entry / json.loads(good_pointer)['snapshot_id']
        (snap_dir / 'meta.json').write_text('{corrupt-json')
        time.sleep(0.03)
        r_corrupt, _, _ = self._run_supervisor(commit=self.base_commit, expected_main='0' * 40, branch_scope=scope)
        self.assertEqual(r_corrupt['status'], 'passed')
        self.assertEqual(r_corrupt['cache']['restore_status'], 'corrupt_fallback')
        self.assertEqual(r_corrupt['cache']['publish_status'], 'published')

        # 4. Retention: main is preserved while older non-main entries are evicted under max_entries=2.
        time.sleep(0.03)
        rm, _, _ = self._run_supervisor(
            commit=self.base_commit, expected_main=self.base_commit, branch_scope='main',
            cache_config_override={'max_entries': 2},
        )
        self.assertEqual(rm['cache']['publish_status'], 'published')
        time.sleep(0.03)
        scope2 = 'branch/' + CI.canonical_branch_slug('feat/second')
        r_evict, _, _ = self._run_supervisor(
            commit=self.base_commit, expected_main='0' * 40, branch_scope=scope2,
            cache_config_override={'max_entries': 2},
        )
        self.assertEqual(r_evict['cache']['publish_status'], 'published')
        self.assertGreaterEqual(r_evict['cache']['evicted_entries'], 1)
        self.assertEqual(r_evict['cache']['cache_entry_count'], 2)
        # Main survives eviction of older non-main branch.
        self.assertTrue((self.cache_root / 'scopes/homelab/main').exists())
        self.assertFalse((self.cache_root / 'scopes/homelab' / scope_dir).exists())

        # TTL expiry via prune().
        provider = CI.LocalCargoCacheProvider(
            CI.CargoCacheConfig.parse({'enabled': True, 'root': str(self.cache_root), 'max_age_seconds': 60}, trusted_prefix=self.root),
            expected_owner=os.getuid(), expected_group=os.getgid(),
        )
        pruned = provider.prune(now_ns=time.time_ns() + 120 * 1_000_000_000)
        self.assertEqual(pruned.evicted_entries, 2)
        self.assertEqual(pruned.cache_entry_count, 0)

        # 5. Operator clear-ci-cache.py: scope clear then --all clear forces clean rebuild.
        time.sleep(0.03)
        self._run_supervisor(commit=self.base_commit, expected_main=self.base_commit, branch_scope='main')
        time.sleep(0.03)
        self._run_supervisor(commit=self.base_commit, expected_main='0' * 40, branch_scope=scope2)
        cleared_one = CLEAR.clear_cache(
            config, project='homelab', scope=scope2, trusted_prefix=self.root, expected_owner=os.getuid(),
        )
        self.assertEqual(cleared_one['removed_entries'], 1)
        self.assertEqual(cleared_one['cache_entry_count'], 1)
        self.assertTrue((self.cache_root / 'scopes/homelab/main').exists())

        cleared_all = CLEAR.clear_cache(
            config, clear_all=True, trusted_prefix=self.root, expected_owner=os.getuid(),
        )
        self.assertEqual(cleared_all['removed_entries'], 1)
        self.assertEqual(cleared_all['cache_entry_count'], 0)

        time.sleep(0.03)
        r_after_clear, log_after, _ = self._run_supervisor(
            commit=self.base_commit, expected_main=self.base_commit, branch_scope='main',
        )
        self.assertEqual(r_after_clear['status'], 'passed', log_after)
        self.assertEqual(r_after_clear['cache']['restore_status'], 'miss')
        self.assertGreaterEqual(next(s for s in r_after_clear['stages'] if s['name'] == 'check_0')['cargo_compiling_units'], 2)

    def test_end_to_end_delivery_prepare_submission_to_supervisor_reuse_and_extra_guards(self):
        import grp
        import work_external_delivery as delivery

        # 1. Real bare remote + TrustedDeliveryPort.prepare_candidate() -> _submission() -> CI.execute_locked()
        remote = self.root / 'e2e-remote.git'
        canonical = self.root / 'e2e-canonical'
        self._git('clone', '--quiet', '--bare', str(self.repo), str(remote), cwd=self.root)
        self._git('clone', '--quiet', str(remote), str(canonical), cwd=self.root)
        self._git('checkout', '--quiet', '-b', 'feat/e2e-cache')
        branch_commit = self._commit_change(
            'src/rust/lib-core/src/lib.rs',
            'pub fn greeting() -> &\'static str { "core-v1" }\n// branch e2e\n',
            'Feature branch commit',
        )
        branch_tree = self._git('rev-parse', branch_commit + '^{tree}').decode().strip()
        self._git('push', '--quiet', str(remote), 'HEAD:refs/heads/feat/e2e-cache')

        delivery_cfg = SimpleNamespace(
            state_root=self.root / 'delivery-state',
            spool_root=self.spool,
            results_root=self.results,
            projects={'homelab': {'repository': 'fixture/project', 'canonical_repo': str(canonical)}},
            github_config_dir=self.root / 'gh',
            verifier=Path(CI.__file__),
            jenkins_job='forge-exact-candidate',
        )
        base_commit = self.base_commit

        class LocalPort(delivery.TrustedDeliveryPort):
            def source_head(inner, _project):
                return base_commit
            def git(inner, repository, *args, **kwargs):
                args = tuple(str(remote) if x == 'https://github.com/fixture/project.git' else x for x in args)
                return super().git(repository, *args, **kwargs)

        port = LocalPort(delivery_cfg, Mock())
        checks = (
            (str(TOOLCHAIN / 'bin/cargo'), 'test', '--manifest-path', 'src/rust/Cargo.toml', '--offline'),
            ('/usr/bin/python3', '-c', 'print("ui-qa-stub")'),
            ('/usr/bin/python3', '-c', 'print("unit-stub")'),
        )
        uid_now, gid_now = os.getuid(), os.getgid()
        orig_del_read = delivery.read_private
        def owned_del_read(path, **kwargs):
            if kwargs.get('owner') == 0:
                kwargs['owner'] = uid_now
            return orig_del_read(path, **kwargs)

        def run_e2e_workflow():
            wf_id = str(uuid4())
            prep_action = str(uuid4())
            verify_action = str(uuid4())
            corr_id = str(uuid4())
            prep_req = {
                'action_id': prep_action, 'workflow_id': wf_id, 'project_alias': 'homelab',
                'source_task_id': str(uuid4()), 'source_assignment_id': str(uuid4()),
                'source_sha256': 'f' * 64, 'base_revision': base_commit,
                'candidate_commit': branch_commit, 'candidate_tree': branch_tree,
            }
            with patch.object(delivery, 'CHECKS', checks), \
                 patch.object(delivery, 'read_private', side_effect=owned_del_read):
                candidate = port.prepare_candidate(prep_req, Mock())
                verify_req = {
                    **candidate, 'action_id': verify_action, 'workflow_id': wf_id,
                    'project_alias': 'homelab', 'correlation_id': corr_id,
                }
                with patch.object(grp, 'getgrnam', return_value=SimpleNamespace(gr_gid=gid_now)):
                    _sub_dir, record = port._submission(verify_req)
            self.assertEqual(candidate['source_branch'], 'feat/e2e-cache')
            self.assertEqual(record['branch_scope'], 'branch/' + CI.canonical_branch_slug('feat/e2e-cache'))

            helper_digest = hashlib.sha256(Path(CI.__file__).read_bytes()).hexdigest()
            cache_raw = {
                'enabled': True, 'root': str(self.cache_root),
                'max_total_bytes': 2 * 1024**3, 'max_entries': 8, 'max_age_seconds': 86400,
                'seed_from_main': True,
            }
            cfg = {
                'build_user': 'forge_ci', 'submitter_user': 'fixture',
                'spool_root': str(self.spool), 'results_root': str(self.results),
                'work_root': str(self.work), 'projects': ['homelab'],
                'verification_profile': delivery.PROFILE, 'verifier_sha256': helper_digest,
                'toolchain': str(TOOLCHAIN), 'cargo_registry': str(self.registry),
                'node_modules': str(self.node_modules), 'timeout_seconds': 120,
                'cargo_cache': cache_raw,
            }
            cache_port = CI.CargoCacheFactory(
                CI.CargoCacheConfig.parse(cache_raw, trusted_prefix=self.root),
                expected_owner=uid_now, expected_group=gid_now,
            ).create()
            build = SimpleNamespace(pw_uid=uid_now, pw_gid=gid_now, pw_name='forge_ci')
            submitter = SimpleNamespace(pw_uid=uid_now + 1, pw_gid=gid_now + 1)
            orig_ci_read = CI.read_file
            orig_write = CI.write_result
            orig_log = CI.open_log

            def test_write(p, r, g):
                with patch.object(CI.os, 'fchown'):
                    return orig_write(p, r, g)

            def test_log(p, g):
                with patch.object(CI.os, 'fchown'):
                    return orig_log(p, g)

            with patch.object(CI, 'CHECKS', checks), \
                 patch.object(CI, 'TRUSTED_CACHE_ROOT_PREFIX', self.root), \
                 patch.object(CI.pwd, 'getpwnam', side_effect=lambda name: build if name == 'forge_ci' else submitter), \
                 patch.object(CI.grp, 'getgrgid', return_value=SimpleNamespace(gr_name='forge_ci')), \
                 patch.object(CI, 'regular_directory'), \
                 patch.object(CI, 'read_file', side_effect=lambda p, *, owner, maximum: orig_ci_read(p, owner=uid_now, maximum=maximum)), \
                 patch.object(CI, 'write_result', side_effect=test_write), \
                 patch.object(CI, 'open_log', side_effect=test_log), \
                 patch.object(CI, 'kill_build_user'), \
                 patch.object(CI, 'run_child', side_effect=lambda argv, *, cwd, env, account, log, timeout, cancelled: subprocess.run(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT, timeout=timeout, check=False).returncode):
                return CI.execute_locked(verify_action, cfg, cache_port=cache_port)

        e1 = run_e2e_workflow()
        self.assertEqual(e1['status'], 'passed')
        self.assertEqual(e1['cache']['restore_status'], 'miss')
        self.assertEqual(e1['cache']['publish_status'], 'published')
        self.assertGreaterEqual(next(s for s in e1['stages'] if s['name'] == 'check_0')['cargo_compiling_units'], 2)

        time.sleep(0.03)
        e2 = run_e2e_workflow()
        self.assertEqual(e2['status'], 'passed')
        self.assertEqual(e2['cache']['restore_status'], 'hit_branch')
        self.assertEqual(next(s for s in e2['stages'] if s['name'] == 'check_0')['cargo_compiling_units'], 0)

        # 2. Metadata-vs-filesystem mtime mismatch triggers corrupt_fallback cleanly.
        scope = 'branch/' + CI.canonical_branch_slug('feat/e2e-cache')
        entry = self._entry_dir(CI.scope_directory_name(scope), e1['cache']['compat_key'])
        snap_dir = entry / json.loads((entry / 'current.json').read_text())['snapshot_id']
        meta = json.loads((snap_dir / 'meta.json').read_text())
        # Alter one restored file's mtime on disk without updating meta.json.
        some_file = next((snap_dir / 'target').rglob('invoked.timestamp'))
        old_ns = some_file.stat().st_mtime_ns
        os.utime(some_file, ns=(old_ns - 5_000_000_000, old_ns - 5_000_000_000))
        time.sleep(0.03)
        e3 = run_e2e_workflow()
        self.assertEqual(e3['status'], 'passed')
        self.assertEqual(e3['cache']['restore_status'], 'corrupt_fallback')
        self.assertEqual(e3['cache']['publish_status'], 'published')

        # 3. Hard max_total_bytes eviction removes non-main first, then main if required.
        provider_tight = CI.LocalCargoCacheProvider(
            CI.CargoCacheConfig.parse({'enabled': True, 'root': str(self.cache_root), 'max_total_bytes': 1}, trusted_prefix=self.root),
            expected_owner=uid_now, expected_group=gid_now,
        )
        pruned_hard = provider_tight.prune()
        self.assertGreaterEqual(pruned_hard.evicted_entries, 1)
        self.assertEqual(pruned_hard.cache_total_bytes, 0)
        self.assertEqual(pruned_hard.cache_entry_count, 0)

        # 4. Dirty source during checks fails verification and records skipped_dirty without overwriting cache.
        time.sleep(0.03)
        def dirty_source_check(argv, *, cwd, env, account, log, timeout, cancelled):
            (Path(cwd) / 'Makefile').write_text('dirty!\n')
            return 0
        r_dirty, _, _ = self._run_supervisor(
            commit=self.base_commit, expected_main='0' * 40, branch_scope=scope,
            run_child_override=dirty_source_check,
        )
        self.assertEqual(r_dirty['status'], 'failed')
        self.assertEqual(r_dirty['failure'], 'source_changed_during_checks')
        self.assertEqual(r_dirty['cache']['publish_status'], 'skipped_dirty')

        # 5. _read_ui_qa_stages ignores symlinked run.json safely.
        qa_dir = self.root / 'qa-symlink-test'
        (qa_dir / 'forge-ui-qa-1').mkdir(parents=True)
        outside = self.root / 'outside-qa.json'
        outside.write_text('{"stages": [{"id": "leaked", "status": "passed", "duration_ms": 1}]}')
        (qa_dir / 'forge-ui-qa-1/run.json').symlink_to(outside)
        self.assertIsNone(CI._read_ui_qa_stages(qa_dir, expected_uid=uid_now))


if __name__ == '__main__':
    unittest.main()
