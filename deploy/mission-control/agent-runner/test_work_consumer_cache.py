"""Owned Git fixtures for fixed consumer cache mounting; no live agent calls."""

from contextlib import contextmanager
from pathlib import Path
import subprocess
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from work_consumer_cache import CONSUMER_TARGETS, ConsumerCacheError, ConsumerCacheProvider
from work_executor import WorkspaceLease


@contextmanager
def fixture(*, ignored=True):
    with TemporaryDirectory(prefix="forge-consumer-cache-") as directory:
        base = Path(directory)
        root, scratch = base / "workspace", base / "attempt"
        root.mkdir()
        scratch.mkdir()
        subprocess.run(("git", "init", "--quiet", str(root)), check=True)
        for target in CONSUMER_TARGETS:
            (root / target.parent).mkdir(parents=True)
            (root / target.parent / "Cargo.toml").write_text("# fixture source\n")
        if ignored:
            (root / ".gitignore").write_text("**/target/\n")
        subprocess.run(("git", "-C", str(root), "add", "."), check=True)
        workspace = WorkspaceLease(root, root, root / ".git", "fixture-task", "a" * 40)
        yield workspace, scratch


class ConsumerCacheProviderTest(unittest.TestCase):
    def test_construction_is_effect_free(self):
        with patch("os.open", side_effect=AssertionError("no construction IO")), \
                patch("subprocess.run", side_effect=AssertionError("no construction process")):
            provider = ConsumerCacheProvider()
        self.assertEqual(provider.child_workspace, Path("/run/forge-coding-workspace"))

    def test_fixed_ignored_empty_mounts_keep_all_cache_bytes_outside_digest(self):
        with fixture() as (workspace, scratch):
            provider = ConsumerCacheProvider()
            before = workspace.digest()
            mounts = provider.prepare(workspace, scratch)
            self.assertEqual(len(mounts), 3)
            self.assertEqual(provider.prepare(workspace, scratch), mounts)
            self.assertEqual(workspace.digest(), before)
            self.assertEqual(tuple(destination for _, destination in mounts), tuple(
                Path("/run/forge-coding-workspace") / relative for relative in CONSUMER_TARGETS))
            for (source, _), relative in zip(mounts, CONSUMER_TARGETS):
                self.assertTrue(source.is_relative_to(scratch))
                self.assertFalse(source.is_relative_to(workspace.path))
                (source / "public-consumer").mkdir()
                (source / "public-consumer/artifact").write_bytes(b"large generated output" * 1024)
                self.assertEqual(list((workspace.path / relative).iterdir()), [])
            self.assertEqual(workspace.digest(), before)
            # The ordinary source receipt still sees unexpected ignored output.
            (workspace.path / CONSUMER_TARGETS[0] / "unexpected").write_text("must be hashed")
            self.assertNotEqual(workspace.digest(), before)

    def test_nonempty_mountpoint_is_preserved_and_rejected_before_other_creation(self):
        with fixture() as (workspace, scratch):
            target = workspace.path / CONSUMER_TARGETS[-1]
            target.mkdir()
            artifact = target / "user-owned"
            artifact.write_text("preserve me")
            with self.assertRaises(ConsumerCacheError):
                ConsumerCacheProvider().prepare(workspace, scratch)
            self.assertEqual(artifact.read_text(), "preserve me")
            self.assertFalse((workspace.path / CONSUMER_TARGETS[0]).exists())
            self.assertEqual(list(scratch.iterdir()), [])

    def test_not_ignored_mountpoint_is_rejected_without_effects(self):
        with fixture(ignored=False) as (workspace, scratch):
            with self.assertRaises(ConsumerCacheError):
                ConsumerCacheProvider().prepare(workspace, scratch)
            self.assertFalse(any((workspace.path / target).exists() for target in CONSUMER_TARGETS))
            self.assertEqual(list(scratch.iterdir()), [])

    def test_tracked_target_is_rejected_even_after_worktree_file_removed(self):
        with fixture() as (workspace, scratch):
            target = workspace.path / CONSUMER_TARGETS[0]
            target.mkdir()
            tracked = target / "tracked"
            tracked.write_text("tracked source")
            subprocess.run(("git", "-C", str(workspace.path), "add", "--force", str(tracked)), check=True)
            tracked.unlink()
            with self.assertRaises(ConsumerCacheError):
                ConsumerCacheProvider().prepare(workspace, scratch)
            self.assertEqual(list(target.iterdir()), [])
            self.assertEqual(list(scratch.iterdir()), [])

    def test_symlink_mountpoint_cannot_expose_outside_files(self):
        with fixture() as (workspace, scratch):
            outside = scratch / "outside"
            outside.mkdir()
            secret = outside / "private"
            secret.write_text("preserve private fixture")
            (workspace.path / CONSUMER_TARGETS[0]).symlink_to(outside, target_is_directory=True)
            with self.assertRaises(ConsumerCacheError):
                ConsumerCacheProvider().prepare(workspace, scratch)
            self.assertEqual(secret.read_text(), "preserve private fixture")
            self.assertEqual(list(scratch.iterdir()), [outside])

    def test_symlink_workspace_parent_component_is_rejected(self):
        with fixture() as (workspace, scratch):
            parent = workspace.path / CONSUMER_TARGETS[0].parent
            moved = workspace.path / "preserved-crate"
            parent.rename(moved)
            parent.symlink_to(moved, target_is_directory=True)
            with self.assertRaises(ConsumerCacheError):
                ConsumerCacheProvider().prepare(workspace, scratch)
            self.assertFalse((moved / "target").exists())
            self.assertEqual(list(scratch.iterdir()), [])

    def test_scratch_symlinks_nonempty_entries_and_containment_are_rejected(self):
        with fixture() as (workspace, scratch):
            name = "consumer-target-" + CONSUMER_TARGETS[0].parent.name
            cache = scratch / name
            cache.symlink_to(workspace.path, target_is_directory=True)
            with self.assertRaises(ConsumerCacheError):
                ConsumerCacheProvider().prepare(workspace, scratch)
            cache.unlink()
            cache.mkdir()
            (cache / "keep").write_text("owned")
            with self.assertRaises(ConsumerCacheError):
                ConsumerCacheProvider().prepare(workspace, scratch)
            self.assertEqual((cache / "keep").read_text(), "owned")
            with self.assertRaises(ConsumerCacheError):
                ConsumerCacheProvider().prepare(workspace, workspace.path / "attempt")
            alias = scratch.parent / "alias"
            alias.symlink_to(workspace.path, target_is_directory=True)
            with self.assertRaises(ConsumerCacheError):
                ConsumerCacheProvider().prepare(SimpleNamespace(path=alias), scratch)

    def test_missing_source_parent_does_not_create_unrelated_directories(self):
        with TemporaryDirectory(prefix="forge-consumer-missing-") as directory:
            root, scratch = Path(directory) / "source", Path(directory) / "attempt"
            root.mkdir()
            scratch.mkdir()
            with self.assertRaises(ConsumerCacheError):
                ConsumerCacheProvider().prepare(SimpleNamespace(path=root), scratch)
            self.assertEqual(list(root.iterdir()), [])
            self.assertEqual(list(scratch.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
