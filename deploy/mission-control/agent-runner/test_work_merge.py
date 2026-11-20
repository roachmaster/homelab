"""Real Git object/ancestry regressions with an owned GitHub API substitute."""

from contextlib import contextmanager
from dataclasses import replace
from datetime import datetime, timezone
import os
import subprocess
import unittest
from unittest.mock import Mock
from uuid import uuid4

from test_work_publication import fixture
from test_work_review import ReviewChecks, ReviewStore, review_job
from work_merge import GitHubExactTreeMergeProvider, MergeWorker
from work_publication import PublicationWorkerFactory
from work_publication_clients import GitHubPullRequestClient, PublicationError, WorkspaceSnapshotProvider, branch_name
from work_publication_config import parse_config
from work_review import ReviewedCandidateProvider, ReviewWorker


class GitHubGitFixture:
    def __init__(self, config, bare, job, head):
        self.config, self.bare, self.job, self.head = config, bare, job, head
        self.calls, self.race, self.protected, self.lose_ack = [], None, False, False
        self.validator = GitHubPullRequestClient(config)

    def git(self, *args, data=None, env=None, check=True):
        return subprocess.run(["/usr/bin/git", "--git-dir", str(self.bare), *args], input=data,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, check=check)

    def _validated(self, *args):
        return self.validator._validated(*args)

    def _api(self, method, path, payload=None):
        self.calls.append((method, path, payload))
        repo = self.config.repositories[self.job["project_alias"]]
        if method == "GET" and "/pulls/" in path:
            return {"number": 1, "html_url": self.job["pr_url"], "state": "open", "merged_at": None,
                "head": {"sha": self.head, "ref": branch_name(self.job), "repo": {"full_name": repo}},
                "base": {"ref": "main", "repo": {"full_name": repo}}}
        if method == "GET" and path.endswith("/git/ref/heads/main"):
            return {"ref": "refs/heads/main", "object": {"type": "commit", "sha": self.git("rev-parse", "main").stdout.decode().strip()}}
        if method == "GET" and "/git/commits/" in path:
            head = path.rsplit("/", 1)[-1]
            return {"sha": head, "tree": {"sha": self.git("rev-parse", head + "^{tree}").stdout.decode().strip()}}
        if method == "GET" and "/compare/" in path:
            base, head = path.rsplit("/", 1)[-1].split("...")
            ancestor = self.git("merge-base", base, head, check=False)
            return {"status": "identical" if base == head else "ahead" if ancestor.stdout.decode().strip() == base else "diverged",
                "base_commit": {"sha": base}, "merge_base_commit": {"sha": ancestor.stdout.decode().strip()}}
        if method == "POST" and path.endswith("/git/commits"):
            env = {"PATH": "/usr/bin:/bin", "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null"}
            for kind in ("author", "committer"):
                for key in ("name", "email", "date"):
                    env["GIT_" + kind.upper() + "_" + key.upper()] = payload[kind][key]
            head = self.git("-c", "commit.gpgSign=false", "commit-tree", payload["tree"],
                "-p", payload["parents"][0], "-p", payload["parents"][1],
                data=payload["message"].encode(), env=env).stdout.decode().strip()
            return {"sha": head, "tree": {"sha": payload["tree"]}, "parents": [{"sha": p} for p in payload["parents"]]}
        if method == "PATCH" and path.endswith("/git/refs/heads/main"):
            if self.protected:
                raise PublicationError("repository_protection_rejected")
            self.assert_nonforce = payload["force"] is False
            if self.race:
                self.git("update-ref", "refs/heads/main", self.race)
            current = self.git("rev-parse", "main").stdout.decode().strip()
            if payload["force"] is not False or self.git("merge-base", "--is-ancestor", current, payload["sha"], check=False).returncode:
                raise PublicationError("reference_not_fast_forward")
            self.git("update-ref", "refs/heads/main", payload["sha"], current)
            if self.lose_ack:
                raise OSError("Acknowledgement lost after the reference changed")
            return {"object": {"sha": payload["sha"]}}
        raise AssertionError((method, path))


class MergeStore:
    def __init__(self):
        self.live, self.intent, self.results = True, None, []

    def merge_renew(self, _job):
        return self.live

    def merge_intent(self, _job, proposed):
        if self.intent is None:
            self.intent = proposed
        return self.intent

    def merge_finish(self, _job, status, receipt):
        self.results.append((status, receipt))
        return True


@contextmanager
def merge_fixture():
    with fixture() as values:
        _, config, source, workspaces, _, bare, git, *_ = values
        config = replace(config, merge_enabled=True)
        local_job, local_store = review_job(source), ReviewStore()
        candidates = ReviewedCandidateProvider(config, git)
        ReviewWorker(config, local_store, WorkspaceSnapshotProvider(workspaces), git,
                     ReviewChecks(config, True), candidates).process(local_job)
        outcome = local_store.outcomes[-1][1]
        job = {"merge_id": str(uuid4()), "workflow_id": str(uuid4()), "task_id": source["task_id"],
               "review_id": local_job["review_id"], "project_alias": "homelab", "lease_token": str(uuid4()),
               "candidate_commit": outcome["candidate_commit"], "gate_manifest_sha256": outcome["gate_manifest_sha256"],
               "target_publication_id": str(uuid4()), "source_publication_id": str(uuid4()),
               "pr_url": "https://github.com/fixture/project/pull/1", "merge_method": "merge",
               "required_hosted_checks": [row[2] for row in config.required_hosted_checks], "policy_digest": "a" * 64}
        job["branch"] = branch_name(job)
        repository = candidates.directory(local_job["review_id"]) / "repository"
        git._git(repository, "push", "--quiet", "--", str(bare), outcome["candidate_commit"] + ":refs/heads/" + job["branch"])
        client = GitHubGitFixture(config, bare, job, outcome["candidate_commit"])
        hosted = Mock()
        hosted.observe.return_value.status = "passed"
        provider = GitHubExactTreeMergeProvider(config, client, git, candidates, hosted,
            now=lambda: datetime(2026, 11, 20, 14, 0, tzinfo=timezone.utc))
        yield config, job, provider, client, candidates, hosted


class ExactTreeMergeTest(unittest.TestCase):
    def test_exact_tree_parents_and_nonforce_update_after_persisted_intent(self):
        with merge_fixture() as (config, job, provider, client, _candidates, _hosted):
            store = MergeStore()
            original_api = client._api
            def checked_api(method, path, payload=None):
                if method != "GET":
                    self.assertIsNotNone(store.intent)
                return original_api(method, path, payload)
            client._api = checked_api
            MergeWorker(config, store, provider).process(job)
            self.assertEqual(store.results[-1][0], "merged")
            intent = store.intent
            self.assertTrue(client.assert_nonforce)
            self.assertEqual(client.git("rev-parse", "main^{tree}").stdout.decode().strip(), intent["candidate_tree"])
            self.assertEqual(client.git("show", "-s", "--format=%P", "main").stdout.decode().strip().split(),
                             [intent["expected_main"], job["candidate_commit"]])

    def test_divergent_main_race_never_incorporates_unreviewed_bytes(self):
        with merge_fixture() as (config, job, provider, client, _candidates, _hosted):
            base = client.git("rev-parse", "main").stdout.decode().strip()
            tree = client.git("rev-parse", "main^{tree}").stdout.decode().strip()
            client.race = client.git("-c", "user.name=Fixture", "-c", "user.email=fixture@localhost", "commit-tree", tree,
                                     "-p", base, data=b"unreviewed concurrent main\n").stdout.decode().strip()
            store = MergeStore()
            MergeWorker(config, store, provider).process(job)
            self.assertEqual(store.results[-1][0], "needs_reconcile")
            self.assertEqual(client.git("rev-parse", "main").stdout.decode().strip(), client.race)
            self.assertEqual(sum(method == "PATCH" for method, *_ in client.calls), 1)

    def test_lost_ack_reconciles_without_repeated_reference_write(self):
        with merge_fixture() as (config, job, provider, client, _candidates, _hosted):
            store = MergeStore()
            client.lose_ack = True
            worker = MergeWorker(config, store, provider)
            worker.process(job)
            self.assertEqual(store.results[-1][0], "merged")
            worker.process({**job, "intent": store.intent})
            self.assertEqual(sum(method == "PATCH" for method, *_ in client.calls), 1)

    def test_protections_pending_checks_wrong_head_and_lease_loss_fail_closed(self):
        for case in ("protection", "checks", "head", "lease"):
            with self.subTest(case=case), merge_fixture() as (config, job, provider, client, _candidates, hosted):
                store = MergeStore()
                old_main = client.git("rev-parse", "main").stdout
                if case == "protection":
                    client.protected = True
                elif case == "checks":
                    hosted.observe.return_value.status = "pending"
                elif case == "head":
                    client.head = "f" * 40
                else:
                    store.live = False
                MergeWorker(config, store, provider).process(job)
                self.assertEqual(client.git("rev-parse", "main").stdout, old_main)
                self.assertFalse(store.results and store.results[-1][0] == "merged")
                if case != "protection":
                    self.assertFalse(any(method == "PATCH" for method, *_ in client.calls))

    def test_selected_merge_provider_is_created_only_when_explicitly_enabled(self):
        with merge_fixture() as (config, _job, _provider, _client, _candidates, _hosted):
            merge_provider, merge_worker = Mock(), Mock()
            store, workspaces, supervisor = Mock(), Mock(), Mock()
            factories = dict(workspace_factory=Mock(return_value=workspaces), supervisor_factory=Mock(return_value=supervisor),
                snapshot_factory=Mock(), git_factory=Mock(), checks_factory=Mock(), pull_request_factory=Mock(),
                candidate_factory=Mock(), reviewer_factory=Mock(), hosted_factory=Mock(),
                merge_provider_factory=merge_provider, merge_worker_factory=merge_worker)
            factory = PublicationWorkerFactory(config, lambda _: store, **factories)
            merge_provider.assert_not_called()
            service = factory.create()
            self.assertIs(service.merger, merge_worker.return_value)
            merge_provider.assert_called_once()
            merge_provider.reset_mock()
            service = PublicationWorkerFactory(replace(config, merge_enabled=False), lambda _: store, **factories).create()
            self.assertIsNone(service.merger)
            merge_provider.assert_not_called()
