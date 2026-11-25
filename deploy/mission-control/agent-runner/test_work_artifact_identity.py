"""Content identity is independent of source technology and checkout provenance."""
from dataclasses import replace
import hashlib
import json
import os
from pathlib import Path
import subprocess
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch
from uuid import uuid4

from work_artifact_identity import (ArtifactEntry, ArtifactManifest, MAX_FILES, MAX_FILE_BYTES,
    VerificationIdentity, VerificationPolicySnapshot, canonical_json)
from work_artifact_source import (ArtifactSourceFactory, DirectorySourceConfig,
    GitArtifactProviderFactory, GitSourceConfig, MemorySourceConfig, SourceLimits)


FILES = (('README.md', '100644', b'Hello\n'), ('src/run', '100755', b'#!/bin/sh\nprintf test\n'),
         ('images/\u00e9.bin', '100644', b'\x00\xff\r\n'))


def entry(path='a.txt', mode='100644', content=b'a'):
    return ArtifactEntry(path, mode, len(content), hashlib.sha256(content).hexdigest())


class LocalGit:
    def __init__(self):
        self.calls = []

    def _git(self, repository, *args, timeout=60):
        self.calls.append((repository, args))
        return subprocess.run(['/usr/bin/git', '-c', 'core.hooksPath=/dev/null',
            '-c', 'commit.gpgSign=false', '-C', str(repository), *args],
            env={'PATH': '/usr/bin:/bin', 'GIT_CONFIG_NOSYSTEM': '1',
                 'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_AUTHOR_NAME': 'Fixture',
                 'GIT_AUTHOR_EMAIL': 'fixture@invalid', 'GIT_COMMITTER_NAME': 'Fixture',
                 'GIT_COMMITTER_EMAIL': 'fixture@invalid'},
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True, timeout=timeout).stdout


class ArtifactIdentityTest(unittest.TestCase):
    def test_canonical_manifest_sorts_exact_paths_and_roundtrips(self):
        a, b = entry('z'), entry('a', '100755', b'\0\xff')
        manifest = ArtifactManifest((a, b))
        self.assertEqual(manifest.entries, (b, a))
        self.assertEqual(manifest.artifact_id, hashlib.sha256(manifest.canonical_bytes()).hexdigest())
        self.assertEqual(ArtifactManifest.from_bytes(manifest.canonical_bytes()), manifest)
        self.assertEqual(ArtifactManifest.from_dict(manifest.to_dict()), manifest)
        self.assertEqual(manifest, ArtifactManifest([b, a]))
        self.assertEqual(ArtifactManifest(()).to_dict()['files'], [])
        self.assertNotIn(b'commit', manifest.canonical_bytes())
        self.assertEqual(ArtifactManifest((entry(),)).canonical_bytes(),
            b'{"domain":"forge.source-artifact","files":[{"mode":"100644","path":"a.txt",'
            b'"sha256":"ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb",'
            b'"size":1}],"version":1}')

    def test_newline_content_path_and_executable_changes_each_change_identity(self):
        original = ArtifactManifest((entry('a', content=b'hello\n'),)).artifact_id
        for changed in (entry('a', content=b'hello\r\n'), entry('a', content=b'hello'),
                        entry('a', mode='100755', content=b'hello\n'), entry('b', content=b'hello\n')):
            with self.subTest(changed=changed):
                self.assertNotEqual(original, ArtifactManifest((changed,)).artifact_id)

    def test_paths_modes_duplicates_conflicts_and_bounds_are_strict(self):
        for path in ('', '/a', '../a', 'a/../b', './a', 'a//b', 'a/', 'a\\b', 'a\x00b', 'a\nb', '\udcff', 'x' * 1025):
            with self.subTest(path=repr(path)), self.assertRaises(ValueError):
                entry(path)
        for mode in ('120000', '160000', '100600', '755', 100644, []):
            with self.subTest(mode=mode), self.assertRaises(ValueError):
                entry(mode=mode)
        for entries in ((entry(), entry()), (entry('a'), entry('a/b'))):
            with self.assertRaises(ValueError):ArtifactManifest(entries)
        with self.assertRaises(ValueError):ArtifactManifest((entry(),) * (MAX_FILES + 1))
        for size in (-1, True, MAX_FILE_BYTES + 1):
            with self.assertRaises(ValueError):ArtifactEntry('a', '100644', size, '0' * 64)
        with self.assertRaises(ValueError):ArtifactManifest(tuple(ArtifactEntry(str(i), '100644', MAX_FILE_BYTES, '0' * 64) for i in range(9)))

    def test_manifest_json_rejects_malformed_duplicate_unknown_and_nonfinite_values(self):
        raw = ArtifactManifest((entry(),)).canonical_bytes()
        invalid = (b'{', raw.replace(b'"version":1', b'"version":1,"version":1'),
                   raw.replace(b'"size":1', b'"size":NaN'), raw.replace(b'"size":1', b'"size":true'),
                   raw.replace(b'"version":1', b'"version":true'), raw.replace(b'"files":', b'"extra":0,"files":'))
        for value in invalid:
            with self.subTest(value=value[:80]), self.assertRaises(ValueError):
                ArtifactManifest.from_bytes(value)

    def test_policy_snapshot_is_immutable_versioned_and_generation_is_explicit(self):
        source = {'checks': [['format', ['cargo', 'fmt']]], 'protected_source': ArtifactManifest(()).to_dict()}
        policy = VerificationPolicySnapshot.from_dict(source)
        original = policy.policy_sha256
        source['checks'].append(['build', ['cargo', 'build']])
        exposed = policy.to_dict();exposed['policy']['checks'].clear()
        self.assertEqual(policy.policy_sha256, original)
        self.assertEqual(VerificationPolicySnapshot.from_bytes(policy.canonical_bytes()), policy)
        self.assertEqual(VerificationPolicySnapshot.parse_dict(policy.to_dict()), policy)
        self.assertEqual(VerificationPolicySnapshot.from_policy({'a': 1}), VerificationPolicySnapshot.from_dict({'a': 1}))
        self.assertNotEqual(VerificationPolicySnapshot.from_dict(source).policy_sha256, original)
        identity = VerificationIdentity(ArtifactManifest(()).artifact_id, original, uuid4())
        self.assertEqual(VerificationIdentity.from_dict(identity.to_dict()), identity)
        self.assertEqual(VerificationIdentity.from_bytes(identity.canonical_bytes()), identity)
        self.assertNotEqual(replace(identity, generation=uuid4()).verification_id, identity.verification_id)
        self.assertNotEqual(replace(identity, policy_sha256='f' * 64).verification_id, identity.verification_id)
        with self.assertRaises(ValueError):VerificationPolicySnapshot.from_dict({'number': float('nan')})
        with self.assertRaises(ValueError):VerificationPolicySnapshot.from_bytes(b'{"domain":"forge.verification-policy","version":1,"policy":{"x":1,"x":2}}')
        with self.assertRaises(ValueError):VerificationIdentity.from_dict({**identity.to_dict(), 'commit': 'a' * 40})


class ArtifactSourceTest(unittest.TestCase):
    def setUp(self):
        self.temp = TemporaryDirectory(prefix='forge-artifact-identity-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.git = LocalGit()

    def write_files(self, root, files=FILES):
        root.mkdir()
        for path, mode, data in files:
            target = root / path;target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data);target.chmod(0o755 if mode == '100755' else 0o644)

    def repo(self, name='repo', object_format=None):
        root = self.root / name
        self.write_files(root)
        self.git._git(root, 'init', '-q', *(['--object-format=' + object_format] if object_format else []))
        self.git._git(root, 'add', '--all')
        self.git._git(root, 'commit', '-q', '-m', 'source')
        return root

    def test_real_git_directory_and_memory_sources_have_identical_artifact_id(self):
        repo = self.repo()
        export = self.root / 'export';self.write_files(export)
        sources = (ArtifactSourceFactory(GitSourceConfig(repo, 'HEAD'), git=self.git).create(),
                   ArtifactSourceFactory(DirectorySourceConfig(export)).create(),
                   ArtifactSourceFactory(MemorySourceConfig(tuple(reversed(FILES)))).create())
        manifests = [source.acquire() for source in sources]
        self.assertEqual(manifests[0], manifests[1]);self.assertEqual(manifests[1], manifests[2])
        # Commit metadata, object abbreviation policy, untracked and changed
        # working-tree bytes affect neither a committed-tree acquisition nor ID.
        (repo / 'README.md').write_bytes(b'uncommitted')
        (repo / 'untracked').write_bytes(b'not in source scope')
        self.git._git(repo, 'config', 'core.abbrev', '4')
        self.git._git(repo, 'commit', '--allow-empty', '-q', '-m', 'different provenance')
        provider = GitArtifactProviderFactory(self.git).create()
        self.assertEqual(provider.acquire(repo, 'HEAD').artifact_id, manifests[0].artifact_id)
        short = self.git._git(repo, 'rev-parse', '--short=8', 'HEAD').decode().strip()
        self.assertEqual(provider.acquire(repo, short).artifact_id, manifests[0].artifact_id)
        for path, _mode, _data in FILES:os.utime(export / path, (1, 1))
        self.assertEqual(sources[1].acquire().artifact_id, manifests[0].artifact_id)

    def test_git_sha256_object_names_and_different_repository_root_are_only_provenance(self):
        first, second = self.repo(), self.repo('sha256', object_format='sha256')
        provider = GitArtifactProviderFactory(self.git).create()
        self.assertEqual(provider.acquire(first, 'HEAD'), provider.acquire(second, 'HEAD'))

    def test_empty_committed_tree_directory_and_memory_are_the_same_artifact(self):
        repo = self.root / 'empty-repo';repo.mkdir()
        self.git._git(repo, 'init', '-q')
        self.git._git(repo, 'commit', '--allow-empty', '-q', '-m', 'empty source')
        directory = self.root / 'empty-directory';directory.mkdir()
        expected = ArtifactManifest(())
        self.assertEqual(GitArtifactProviderFactory(self.git).create().acquire(repo, 'HEAD'), expected)
        self.assertEqual(ArtifactSourceFactory(DirectorySourceConfig(directory)).create().acquire(), expected)
        self.assertEqual(ArtifactSourceFactory(MemorySourceConfig(())).create().acquire(), expected)

    def test_selection_is_effect_free_and_only_selected_source_is_acquired(self):
        class Unused:
            def _git(self, *_args, **_kwargs):raise AssertionError('Unused default had effects')
        source = ArtifactSourceFactory(MemorySourceConfig(FILES), git=Unused()).create()
        self.assertEqual(len(source.acquire().entries), 3)
        missing = ArtifactSourceFactory(GitSourceConfig(self.root / 'absent', 'HEAD'), git=Unused()).create()
        self.assertIsNotNone(missing)
        with self.assertRaises(TypeError):GitArtifactProviderFactory(None)
        with self.assertRaises(TypeError):ArtifactSourceFactory({'type': 'git'})
        with self.assertRaises(ValueError):GitSourceConfig(self.root, '--upload-pack=bad')

    def test_directory_rejects_symlinks_special_modes_and_byte_count_limits(self):
        import stat
        export = self.root / 'export';self.write_files(export)
        (export / 'link').symlink_to(export / 'README.md')
        with self.assertRaises(ValueError):ArtifactSourceFactory(DirectorySourceConfig(export)).create().acquire()
        (export / 'link').unlink();(export / 'README.md').chmod(0o1644)
        with self.assertRaises(ValueError):ArtifactSourceFactory(DirectorySourceConfig(export)).create().acquire()
        (export / 'README.md').chmod(0o644)
        try:
            (export / 'README.md').chmod(0o4644)
        except PermissionError:
            real_stat = os.stat
            for special_bit in (stat.S_ISUID, stat.S_ISGID):
                def with_special_bit(path, *args, _bit=special_bit, **kwargs):
                    info = real_stat(path, *args, **kwargs)
                    if path == 'README.md':
                        values = list(info)
                        values[0] = info.st_mode | _bit
                        return os.stat_result(values)
                    return info
                with self.subTest(special_bit=oct(special_bit)), patch('work_artifact_source.os.stat', side_effect=with_special_bit):
                    with self.assertRaises(ValueError):ArtifactSourceFactory(DirectorySourceConfig(export)).create().acquire()
        else:
            try:
                with self.assertRaises(ValueError):ArtifactSourceFactory(DirectorySourceConfig(export)).create().acquire()
            finally:
                (export / 'README.md').chmod(0o644)
        for limits in (SourceLimits(max_files=2), SourceLimits(max_file_bytes=2), SourceLimits(max_total_bytes=2)):
            with self.subTest(limits=limits), self.assertRaises(ValueError):
                ArtifactSourceFactory(DirectorySourceConfig(export, limits)).create().acquire()
        (export / 'README.md').unlink();os.mkfifo(export / 'README.md')
        with self.assertRaises(ValueError):ArtifactSourceFactory(DirectorySourceConfig(export)).create().acquire()

    def test_directory_rejects_a_real_file_change_during_acquisition(self):
        export = self.root / 'export';self.write_files(export)
        read = os.read
        changed = False
        def change_after_read(descriptor, size):
            nonlocal changed
            data = read(descriptor, size)
            if data and not changed:
                changed = True
                (export / 'README.md').write_bytes(b'changed while being read')
            return data
        with patch('work_artifact_source.os.read', side_effect=change_after_read):
            with self.assertRaises(ValueError):
                ArtifactSourceFactory(DirectorySourceConfig(export)).create().acquire()

    def test_git_rejects_symlink_submodule_duplicate_malformed_and_size_mismatch(self):
        class FixtureGit:
            def __init__(self, listing, content=b'x'):self.listing,self.content,self.calls=listing,content,[]
            def _git(self, repository, *args, **kwargs):
                self.calls.append(args)
                if args[1] == 'rev-parse':return b'a' * 40 + b'\n'
                if args[1] == 'ls-tree':return self.listing
                return self.content
        regular = b'100644 blob ' + b'b' * 40 + b' 1\ta\0'
        for listing in (regular.replace(b'100644', b'120000'),
                        regular.replace(b'100644 blob', b'160000 commit'), regular + regular,
                        regular[:-1], regular.replace(b'\ta', b'\t../a'),
                        regular.replace(b' 1\t', b' 999999999\t')):
            git = FixtureGit(listing)
            with self.subTest(listing=listing[:30]), self.assertRaises(ValueError):
                GitArtifactProviderFactory(git).create().acquire(self.root, 'HEAD')
            self.assertFalse(any(call[1] == 'cat-file' for call in git.calls))
        with self.assertRaises(ValueError):GitArtifactProviderFactory(FixtureGit(regular, b'xx')).create().acquire(self.root, 'HEAD')
        for limits in (SourceLimits(max_files=1), SourceLimits(max_file_bytes=1), SourceLimits(max_total_bytes=1)):
            with self.assertRaises(ValueError):
                ArtifactSourceFactory(MemorySourceConfig((('a', '100644', b'xx'), ('b', '100644', b'x')), limits)).create()


if __name__ == '__main__':
    unittest.main()
