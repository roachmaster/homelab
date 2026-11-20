"""Source selection requires both a durable receipt and the exact private ref."""

from dataclasses import replace
import unittest
from unittest.mock import Mock
from pathlib import Path
from test_work_executor import raw_config
from work_worker_config import parse_config, validated_copy
from work_publication_config import parse_config as parse_publication

from test_work_merge import merge_fixture, MergeStore
from work_executor import _git
from work_merge import MergeWorker
from work_source_revision import SelectedSourceRevisionProvider, SourceRevisionError, VerifiedSourceSyncProvider, VERIFIED_REF


class SourceRevisionTest(unittest.TestCase):
    def test_source_selection_is_explicit_frozen_and_can_coexist_with_legacy_projects(self):
        raw = raw_config("/tmp/forge-source-selector")
        raw["projects"]["docs"] = "/tmp/forge-source-selector/docs-repository"
        raw["autonomous_projects"] = ["homelab"]
        worker = parse_config(raw)
        raw["autonomous_projects"].clear()
        self.assertEqual(validated_copy(worker).autonomous_projects, frozenset({"homelab"}))
        settings = {"worker_config": "/tmp/forge-source-selector/worker.json", "merge_enabled": True,
            "repositories": {"homelab": "fixture/project", "docs": "fixture/docs"},
            "github_config_dir": str(worker.account.home / ".config/gh")}
        self.assertTrue(parse_publication(settings, worker).merge_enabled)
        with self.assertRaises(ValueError):
            parse_publication(settings, replace(worker, autonomous_projects=frozenset()))
        for selection in (["unknown"], ["homelab", "homelab"], "homelab", [True]):
            raw["autonomous_projects"] = selection
            with self.subTest(selection=selection), self.assertRaises(ValueError):
                parse_config(raw)

    def test_durable_merge_ref_supplies_new_base_without_mutating_checkout(self):
        with merge_fixture() as (config, job, provider, _client, _candidates, _hosted):
            worker = config.worker
            canonical = worker.projects["homelab"]
            old_head = _git(canonical, "rev-parse", "HEAD")
            old_contents = (canonical / "app.py").read_bytes()
            store = Mock()
            store.project_revisions.return_value = {}
            selected = SelectedSourceRevisionProvider(worker, store, _git)
            self.assertEqual(selected.projects()[0]["base_revision"], old_head.decode().strip())
            merge_store = MergeStore()
            MergeWorker(config, merge_store, provider, source_sync=VerifiedSourceSyncProvider(config, provider.git)).process(job)
            merged = merge_store.results[-1][1]["merge_commit"]
            with self.assertRaises(SourceRevisionError):
                selected.projects()  # Ref ahead of durable finish receipt.
            store.project_revisions.return_value = {"homelab": merged}
            self.assertEqual(selected.projects()[0]["base_revision"], merged)
            self.assertEqual(_git(canonical, "rev-parse", "HEAD"), old_head)
            self.assertEqual((canonical / "app.py").read_bytes(), old_contents)
            _git(canonical, "update-ref", "-d", VERIFIED_REF)
            with self.assertRaises(SourceRevisionError):
                selected.projects()
            # Legacy projects continue reading HEAD and do not read the DB port.
            store.project_revisions.reset_mock()
            legacy = SelectedSourceRevisionProvider(replace(worker, autonomous_projects=frozenset()), store, _git)
            self.assertEqual(legacy.projects()[0]["base_revision"], old_head.decode().strip())
            store.project_revisions.assert_not_called()

    def test_divergent_private_source_is_never_overwritten_or_reported_ready(self):
        with merge_fixture() as (config, job, provider, client, _candidates, _hosted):
            canonical = config.worker.projects["homelab"]
            base = _git(canonical, "rev-parse", "HEAD").decode().strip()
            tree = _git(canonical, "rev-parse", "HEAD^{tree}").decode().strip()
            divergent = client.git("-c", "user.name=Fixture", "-c", "user.email=fixture@localhost", "commit-tree", tree,
                                    "-p", base, data=b"unverified private source\n").stdout.decode().strip()
            provider.git._git(canonical, "fetch", "--quiet", "--no-write-fetch-head", "--", str(client.bare), divergent)
            _git(canonical, "update-ref", VERIFIED_REF, divergent)
            store = MergeStore()
            MergeWorker(config, store, provider, source_sync=VerifiedSourceSyncProvider(config, provider.git)).process(job)
            self.assertEqual(store.results[-1][0], "needs_reconcile")
            self.assertEqual(_git(canonical, "rev-parse", VERIFIED_REF).decode().strip(), divergent)
            self.assertEqual(_git(canonical, "rev-parse", "HEAD").decode().strip(), base)
